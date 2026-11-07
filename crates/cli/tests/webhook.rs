//! #626: `POST /hooks/github` on serve's Funnel listener: signed GitHub events refresh the PR cache and kick drive.
mod common;
use common::*;
use herdsman::events::{append, Event};
use herdsman::hooks::{listen, reply};
use hkdf::hmac::{Hmac, KeyInit, Mac};
use serde_json::json;
use std::io::{Read, Write};

const SECRET: &str = "s3cret-hook";
const KEY: &str = "herdsman/all/GITHUB_WEBHOOK_SECRET";
const KICK: &str = "sh -c nohup '/opt/herdsman/bin/herdsman' kick sendmeter 7 --why webhook --now";

/// A sendmeter fleet with an impl lane on `issue-7`.
fn setup(name: &str) -> herdsman::Ctx {
    let c = ctx(&tmpdir(name), CONFIG);
    let d = json!({"role": "impl", "agent": "sendmeter-impl-7", "branch": "issue-7"});
    append(&c.state, &Event::new(10, "sendmeter", Some(7), "spawn", d)).unwrap();
    c
}

fn sign(key: &str, body: &str) -> String {
    let mut m = Hmac::<sha2::Sha256>::new_from_slice(key.as_bytes()).unwrap();
    m.update(body.as_bytes());
    let hex: String = m.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256={hex}")
}

fn request(event: &str, sig: Option<&str>, body: &str) -> Vec<u8> {
    let sig = sig.map_or(String::new(), |s| format!("X-Hub-Signature-256: {s}\r\n"));
    let n = body.len();
    format!("POST /hooks/github HTTP/1.1\r\nHost: h.ts.net\r\nX-GitHub-Event: {event}\r\n{sig}Content-Length: {n}\r\n\r\n{body}")
        .into_bytes()
}

fn send(c: &herdsman::Ctx, r: &Fake, req: Vec<u8>) -> String {
    String::from_utf8(reply(c, r, &mut std::io::Cursor::new(req))).unwrap()
}

fn suite(repo: &str, branch: &str) -> String {
    json!({"action": "completed", "repository": {"full_name": repo},
        "check_suite": {"head_branch": branch, "status": "completed", "conclusion": "success", "pull_requests": []}})
    .to_string()
}

fn pr(status: &str, conclusion: Option<&str>) -> String {
    let check = json!({"__typename": "CheckRun", "name": "build", "status": status, "conclusion": conclusion});
    json!({"number": 107, "state": "OPEN", "baseRefName": "staging", "headRefOid": "abc1234", "mergeable": "MERGEABLE",
        "statusCheckRollup": [check]})
    .to_string()
}

#[test]
fn a_signed_check_suite_completed_refreshes_the_pr_cache_and_kicks_drive() {
    let c = setup("hook-suite");
    // The cache holds the PR read while CI ran.
    let before = Fake::default()
        .reply("gh api graphql", "")
        .reply("gh pr view issue-7", &pr("IN_PROGRESS", None));
    let (code, out) = run(&c, &before, &["pr", "sendmeter", "7"]);
    assert!(code == 0 && out.contains("ci pending"), "{out}");

    herdsman::db::put(&c.state, "board", 1, &herdsman::snapshot::Snapshot::default()).unwrap();
    let board = herdsman::db::stamp(&c.state, "board");
    let r = Fake::default()
        .stored(KEY, SECRET)
        .reply("gh pr view issue-7", &pr("COMPLETED", Some("SUCCESS")));
    let body = suite("jirathip-dev/sendmeter", "issue-7");
    let res = send(&c, &r, request("check_suite", Some(&sign(SECRET, &body)), &body));
    assert!(res.starts_with("HTTP/1.1 200 OK"), "{res}");
    assert!(res.contains("refreshed sendmeter #7"), "{res}");
    assert_eq!(r.count("gh pr view issue-7"), 1, "{:?}", r.calls.borrow());
    assert_eq!(r.count(KICK), 1, "drive woken for #7: {:?}", r.calls.borrow());
    let cached = herdsman::gh::cached_pr(&c, "sendmeter", 7);
    assert_eq!(cached["statusCheckRollup"][0]["conclusion"], "SUCCESS", "{cached}");
    assert_ne!(
        herdsman::db::stamp(&c.state, "board"),
        board,
        "the board re-renders now"
    );

    // `herdsman pr` right after reads the refreshed cache: green, and no poll of its own.
    let now = herdsman::Ctx {
        now: herdsman::now(),
        ..c.clone()
    };
    let (code, out) = run(&now, &r, &["pr", "sendmeter", "7"]);
    assert!(code == 0 && out.contains("ci green"), "{out}");
    assert_eq!(r.count("gh "), 1, "{:?}", r.calls.borrow());
}

#[test]
fn a_bad_missing_or_wrong_secret_signature_is_401_and_changes_nothing() {
    let c = setup("hook-401");
    let r = Fake::default()
        .stored(KEY, SECRET)
        .reply("gh pr view", &pr("COMPLETED", Some("SUCCESS")));
    let body = suite("jirathip-dev/sendmeter", "issue-7");
    let other = suite("jirathip-dev/sendmeter", "issue-8");
    let sigs = [
        None,
        Some(sign("wrong secret", &body)),
        Some(sign(SECRET, &other)), // a valid signature of another body
        Some("sha256=".to_string() + &"0".repeat(64)),
        Some("sha1=abc".into()),
        Some(String::new()),
    ];
    for sig in &sigs {
        let res = send(&c, &r, request("check_suite", sig.as_deref(), &body));
        assert!(res.starts_with("HTTP/1.1 401 Unauthorized"), "{sig:?}: {res}");
    }
    assert!(r.calls.borrow().is_empty(), "{:?}", r.calls.borrow());
    assert!(herdsman::gh::cached_pr(&c, "sendmeter", 7).is_null());
    assert_eq!(herdsman::webhook::rejected(&c), sigs.len() as u64);
    let (_, out) = run(&c, &r, &["doctor"]);
    assert!(
        out.contains("✓ webhook — secret set · 6 refused (bad signature)"),
        "{out}"
    );
}

#[test]
fn an_unknown_repo_or_branch_is_202_and_ignored_ping_is_200() {
    let c = setup("hook-202");
    let r = Fake::default().stored(KEY, SECRET);
    for body in [
        suite("someone/else", "issue-7"),
        suite("jirathip-dev/sendmeter", "feature-x"),
    ] {
        let res = send(&c, &r, request("check_suite", Some(&sign(SECRET, &body)), &body));
        assert!(res.starts_with("HTTP/1.1 202 Accepted"), "{res}");
    }
    let body = json!({"zen": "Keep it logically awesome.", "hook_id": 1}).to_string();
    let res = send(&c, &r, request("ping", Some(&sign(SECRET, &body)), &body));
    assert!(res.starts_with("HTTP/1.1 200 OK"), "{res}");
    assert!(r.calls.borrow().is_empty(), "{:?}", r.calls.borrow());
    assert!(herdsman::gh::cached_pr(&c, "sendmeter", 7).is_null());
}

#[test]
fn with_no_secret_the_endpoint_is_503_and_doctor_warns() {
    let c = setup("hook-503");
    let r = Fake::default();
    let body = suite("jirathip-dev/sendmeter", "issue-7");
    let res = send(&c, &r, request("check_suite", Some(&sign(SECRET, &body)), &body));
    assert!(res.starts_with("HTTP/1.1 503 Service Unavailable"), "{res}");
    assert!(r.calls.borrow().is_empty(), "{:?}", r.calls.borrow());
    let (_, out) = run(&c, &r, &["doctor"]);
    assert!(
        out.contains("\n! webhook — no secret → herdsman secret request"),
        "{out}"
    );
}

#[test]
fn the_signature_check_matches_githubs_documented_example() {
    // docs.github.com "Validating webhook deliveries": secret, payload and the header GitHub sends.
    let sig = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
    assert!(herdsman::webhook::signed(
        "It's a Secret to Everybody",
        b"Hello, World!",
        sig
    ));
    assert!(!herdsman::webhook::signed(
        "It's a Secret to Everybody",
        b"Hello, World?",
        sig
    ));
    assert!(!herdsman::webhook::signed(
        "It's a Secret to Everybody",
        b"Hello, World!",
        &sig.to_uppercase()[..]
    ));
}

/// One request over TCP to the listener at `addr`: the whole response.
fn http(addr: std::net::SocketAddr, req: &[u8]) -> String {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.write_all(req).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

#[test]
fn the_funnel_listener_answers_no_path_but_hooks_github() {
    let c = setup("hook-funnel");
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let c2 = c.clone();
    std::thread::spawn(move || listen(&c2, &Fake::default().stored(KEY, SECRET), &l));
    for req in [
        "GET / HTTP/1.1\r\nHost: x\r\n\r\n",
        "GET /live HTTP/1.1\r\nHost: x\r\n\r\n",
        "GET /api/v1/board HTTP/1.1\r\nHost: x\r\n\r\n",
        "POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n",
        "POST /send HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n",
        "POST /pair/request HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n",
        "POST /hooks/github/../../ HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n",
        "POST /hooks/github?x=1 HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n",
        "GET /a/app.js HTTP/1.1\r\nHost: x\r\n\r\n",
    ] {
        let res = http(addr, req.as_bytes());
        assert!(
            res.starts_with("HTTP/1.1 404 Not Found") && res.ends_with("\r\n\r\nnot found\n"),
            "{req}: {res}"
        );
    }
    let res = http(addr, b"GET /hooks/github HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(res.starts_with("HTTP/1.1 405"), "{res}");
    // Over GitHub's 25 MB: refused from the header alone, nothing of the body sent or read.
    let res = http(
        addr,
        b"POST /hooks/github HTTP/1.1\r\nHost: x\r\nContent-Length: 26214401\r\n\r\n",
    );
    assert!(res.starts_with("HTTP/1.1 413"), "{res}");
    let body = json!({"zen": "z"}).to_string();
    let res = http(addr, &request("ping", Some(&sign(SECRET, &body)), &body));
    assert!(res.starts_with("HTTP/1.1 200 OK"), "{res}");

    // The documented Funnel line maps only that path, to the listener's default address.
    let docs = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/serve.md")).unwrap();
    let funnel: Vec<&str> = docs.lines().filter(|l| l.starts_with("tailscale funnel")).collect();
    assert_eq!(
        funnel,
        ["tailscale funnel --bg --https=10000 --set-path /hooks/github http://127.0.0.1:7778/hooks/github"]
    );
    let cfg = herdsman::config::Config::load(&c.config).unwrap();
    assert_eq!(cfg.serve.hooks_addr(), "127.0.0.1:7778");
}

/// `gh api graphql … --jq` output: #7's open PR, settled.
fn open_pr() -> String {
    let check = json!({"__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "SUCCESS"});
    let pr = json!({"number": 107, "state": "OPEN", "baseRefName": "staging", "headRefName": "issue-7",
        "headRefOid": "abc1234", "mergeable": "MERGEABLE", "labels": [], "statusCheckRollup": [check]});
    format!("{pr}\n")
}

#[test]
fn with_webhooks_on_the_fast_step_reads_prs_only_every_webhook_poll_minutes_or_after_a_delivery() {
    let c = setup("hook-catchup");
    let r = Fake::default()
        .stored(KEY, SECRET)
        .reply("herdr agent list", r#"{"result":{"agents":[]}}"#)
        .reply("sysctl -n kern.memorystatus_vm_pressure_level", "1\n")
        .reply("sysctl -n vm.loadavg", "{ 1.50 1.20 1.00 }\n")
        .reply("sysctl -n hw.ncpu", "10\n")
        .reply("vm_stat", "Pages free: 1.\nSwapouts:                     100.\n")
        .reply("gh api graphql", &open_pr())
        .reply("gh pr view issue-7", &pr("COMPLETED", Some("SUCCESS")));
    let at = |t: u64| herdsman::Ctx {
        now: c.now + t,
        ..c.clone()
    };
    let step = |t: u64| assert_eq!(run(&at(t), &r, &["drive", "--fast"]).0, 0);
    step(0);
    let reads = r.count("gh ");
    assert!(reads > 0 && r.count("gh api graphql") == 1);
    // Minutes later, with no delivery: no read at all (not even the ETag probes).
    for t in [60, 120, 540] {
        step(t);
    }
    assert_eq!(r.count("gh "), reads, "{:?}", r.calls.borrow());
    // The catch-up read once `webhook_poll_minutes` (10) passed.
    step(600);
    assert_eq!(r.count("gh api graphql"), 2, "{:?}", r.calls.borrow());
    // A delivery makes the next step read afresh.
    let body = suite("jirathip-dev/sendmeter", "issue-7");
    let res = send(&at(660), &r, request("check_suite", Some(&sign(SECRET, &body)), &body));
    assert!(res.starts_with("HTTP/1.1 200 OK"), "{res}");
    step(661);
    assert_eq!(r.count("gh api graphql"), 3, "{:?}", r.calls.borrow());

    // Without the secret, the fast step probes each minute as before.
    r.store.borrow_mut().clear();
    step(720);
    assert!(r.count("gh api -i") > 0, "{:?}", r.calls.borrow());
}
