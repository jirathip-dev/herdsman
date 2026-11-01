//! #17: cached spec + PR/CI reads, `herdsman wait`, the queue, `herdsman next`, board, serve.
mod common;
use common::*;
use herdsman::events::{append, read, Event};
use serde_json::json;
use std::io::{Read, Write};

const HEAD: &str = "1111111111111111111111111111111111111111";

fn ev(c: &herdsman::Ctx, ts: u64, issue: u64, kind: &str, d: serde_json::Value) {
    append(&c.state, &Event::new(ts, "sendmeter", Some(issue), kind, d)).unwrap();
}

fn pr_json(state: &str, checks: serde_json::Value) -> String {
    json!({"number": 34, "state": state, "baseRefName": "staging", "headRefOid": HEAD, "mergeable": "MERGEABLE",
        "statusCheckRollup": checks})
    .to_string()
}

fn run_ci(status: &str, conclusion: &str) -> serde_json::Value {
    json!([{"__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "SUCCESS"},
           {"__typename": "CheckRun", "name": "test", "status": status, "conclusion": conclusion}])
}

// --- spec read once ---

#[test]
fn spec_is_refetched_only_when_updated_at_moves() {
    let d = tmpdir("spec");
    let c = ctx(&d, CONFIG);
    let cfg = herdsman::config::Config::load(&c.config).unwrap();
    let f = cfg.fleet("sendmeter").unwrap();
    let full = r#"{"title":"Fix","body":"AC: works","updatedAt":"2026-10-30T01:00:00Z"}"#;
    let r = Fake::default()
        .reply(
            "gh issue view 12 -R jirathip-dev/sendmeter --json updatedAt",
            r#"{"updatedAt":"2026-10-30T01:00:00Z"}"#,
        )
        .reply(
            "gh issue view 12 -R jirathip-dev/sendmeter --json title,body,updatedAt",
            full,
        );
    herdsman::gh::spec(&c, &r, f, "sendmeter", 12, true).unwrap();
    assert_eq!(r.count("gh issue view 12 -R jirathip-dev/sendmeter --json title"), 1);
    let v = herdsman::gh::spec(&c, &r, f, "sendmeter", 12, true).unwrap();
    assert_eq!(v["body"], "AC: works");
    assert_eq!(
        r.count("gh issue view 12 -R jirathip-dev/sendmeter --json title"),
        1,
        "unchanged: no body fetch"
    );
    assert_eq!(
        r.count("gh issue view 12 -R jirathip-dev/sendmeter --json updatedAt"),
        1
    );
    // `herdsman brief` prints the cache without any call.
    let quiet = Fake::default();
    let (code, out) = run(&c, &quiet, &["brief", "sendmeter", "12"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out, "# Issue #12: Fix\n\nAC: works\n");
    assert!(quiet.calls.borrow().is_empty());
    // Edited issue → fetched again.
    let r2 = Fake::default()
        .reply(
            "gh issue view 12 -R jirathip-dev/sendmeter --json updatedAt",
            r#"{"updatedAt":"2026-10-30T02:00:00Z"}"#,
        )
        .reply(
            "gh issue view 12 -R jirathip-dev/sendmeter --json title",
            r#"{"title":"Fix","body":"AC v2","updatedAt":"2026-10-30T02:00:00Z"}"#,
        );
    assert_eq!(
        herdsman::gh::spec(&c, &r2, f, "sendmeter", 12, true).unwrap()["body"],
        "AC v2"
    );
    assert_eq!(
        run(&c, &quiet, &["brief", "sendmeter", "12"]).1,
        "# Issue #12: Fix\n\nAC v2\n"
    );
}

// --- one cached PR/CI read ---

#[test]
fn pr_makes_one_gh_call_per_60s() {
    let d = tmpdir("pr-cache");
    let mut c = ctx(&d, CONFIG);
    let r = Fake::default().reply("gh pr view issue-12", &pr_json("OPEN", run_ci("COMPLETED", "SUCCESS")));
    for _ in 0..3 {
        let (code, out) = run(&c, &r, &["pr", "sendmeter", "12"]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(
            out,
            "PR #34 OPEN → staging head 1111111 mergeable MERGEABLE ci green (2 checks)\n"
        );
    }
    c.now += 59;
    let (_, js) = run(&c, &r, &["pr", "sendmeter", "12", "--json"]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&js).unwrap()["headRefOid"],
        HEAD
    );
    assert_eq!(r.count("gh"), 1, "{:?}", r.calls.borrow());
    c.now += 1;
    run(&c, &r, &["pr", "sendmeter", "12"]);
    assert_eq!(r.count("gh"), 2, "expired after 60 s");
    // merge and check read the same cache.
    run(&c, &r, &["merge", "sendmeter", "12"]);
    assert_eq!(r.count("gh pr view issue-12"), 2);
    // No PR: exit 1.
    let d = tmpdir("pr-none");
    let c = ctx(&d, CONFIG);
    let (code, out) = run(&c, &Fake::default(), &["pr", "sendmeter", "12"]);
    assert_eq!((code, out.as_str()), (1, "no PR for branch issue-12\n"));
}

fn wait(r: &Fake, until: &str, timeout: &str) -> (i32, String) {
    let d = tmpdir("wait");
    let c = ctx(&d, CONFIG);
    run(
        &c,
        r,
        &["wait", "sendmeter", "12", "--until", until, "--timeout", timeout],
    )
}

#[test]
fn wait_exit_codes() {
    let pending = pr_json("OPEN", run_ci("IN_PROGRESS", ""));
    // Pending, then green: polls through the cache with backoff, exits 0.
    let r = Fake::default()
        .once("gh pr view", 0, &pending)
        .reply("gh pr view issue-12", &pr_json("OPEN", run_ci("COMPLETED", "SUCCESS")));
    let (code, out) = wait(&r, "ci-green", "3600");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("ci green"), "{out}");
    assert_eq!(r.calls.borrow()[1..], ["sleep 30", "sleep 60", "gh pr view issue-12 -R jirathip-dev/sendmeter --json number,title,state,baseRefName,headRefOid,mergeable,mergeStateStatus,statusCheckRollup"]);
    // A failed check: 1, for ci-green and ci-done.
    let red = pr_json("OPEN", run_ci("COMPLETED", "FAILURE"));
    assert_eq!(
        wait(&Fake::default().reply("gh pr view", &red), "ci-green", "3600").0,
        1
    );
    assert_eq!(wait(&Fake::default().reply("gh pr view", &red), "ci-done", "3600").0, 1);
    // Never done: 124, backoff 30 → 5 min, never past the timeout.
    let r = Fake::default().reply("gh pr view", &pending);
    let (code, out) = wait(&r, "ci-done", "1000");
    assert_eq!(code, 124, "{out}");
    let sleeps: Vec<_> = r
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("sleep"))
        .cloned()
        .collect();
    assert_eq!(
        sleeps,
        [
            "sleep 30",
            "sleep 60",
            "sleep 120",
            "sleep 240",
            "sleep 300",
            "sleep 250"
        ]
    );
    assert!(r.count("gh pr view") <= 7);
    // merged / closed.
    assert_eq!(
        wait(
            &Fake::default().reply("gh pr view", &pr_json("MERGED", json!([]))),
            "merged",
            "60"
        )
        .0,
        0
    );
    assert_eq!(
        wait(
            &Fake::default().reply("gh pr view", &pr_json("CLOSED", json!([]))),
            "merged",
            "60"
        )
        .0,
        1
    );
}

// --- queue ---

fn q(c: &herdsman::Ctx, args: &[&str]) -> (i32, String) {
    let mut a = vec!["queue", "sendmeter"];
    a.extend(args);
    run(c, &Fake::default(), &a)
}

fn order(c: &herdsman::Ctx) -> Vec<u64> {
    let (_, out) = q(c, &["ls", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    v.as_array()
        .unwrap()
        .iter()
        .map(|r| r["issue"].as_u64().unwrap())
        .collect()
}

#[test]
fn queue_add_mv_rm_block_ordering() {
    let d = tmpdir("queue");
    let c = ctx(&d, CONFIG);
    assert_eq!(
        q(&c, &["add", "12", "13", "14"]),
        (0, "sendmeter queue: #12 #13 #14\n".into())
    );
    q(&c, &["add", "15", "--top"]);
    q(&c, &["add", "16", "--after", "13"]);
    assert_eq!(order(&c), [15, 12, 13, 16, 14]);
    q(&c, &["mv", "14", "--top"]);
    q(&c, &["mv", "15", "--before", "16"]);
    q(&c, &["mv", "12", "--after", "16"]);
    assert_eq!(order(&c), [14, 13, 15, 16, 12]);
    q(&c, &["rm", "13"]);
    q(&c, &["add", "14"]); // re-adding moves it to the bottom
    assert_eq!(order(&c), [15, 16, 12, 14]);
    assert_eq!(q(&c, &["block", "16", "--on", "15"]).0, 0);
    q(&c, &["mv", "16", "--top"]);
    let (_, ls) = q(&c, &["ls"]);
    assert_eq!(
        ls,
        " 1 #16    queued  blocked by #15\n 2 #15    queued\n 3 #12    queued\n 4 #14    queued\n"
    );
    // Invalid changes fail and log nothing.
    let n = read(&c.state).unwrap().len();
    assert_eq!(q(&c, &["mv", "99", "--top"]).0, 1);
    assert_eq!(q(&c, &["rm", "99"]).0, 1);
    assert_eq!(q(&c, &["add", "17", "--after", "99"]).0, 1);
    assert_eq!(q(&c, &["block", "12", "--on", "12"]).0, 1);
    assert_eq!(q(&c, &["mv", "12"]).0, 2, "mv needs a target");
    assert_eq!(read(&c.state).unwrap().len(), n);
    // Queue events are fleet-level: no issue state is created by queueing.
    assert!(read(&c.state)
        .unwrap()
        .iter()
        .all(|e| e.kind == "queue" && e.issue.is_none()));
}

#[test]
fn queue_survives_torn_last_line() {
    let d = tmpdir("queue-torn");
    let c = ctx(&d, CONFIG);
    q(&c, &["add", "12", "13"]);
    let line = serde_json::to_string(&Event::new(
        5,
        "sendmeter",
        None,
        "queue",
        json!({"op": "rm", "issues": [12]}),
    ))
    .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&c.state)
        .unwrap()
        .write_all(&line.as_bytes()[..30])
        .unwrap();
    assert_eq!(order(&c), [12, 13]);
    q(&c, &["add", "14", "--top"]);
    assert_eq!(order(&c), [14, 12, 13]);
}

#[test]
fn next_skips_blocked_and_live_lane_items() {
    let d = tmpdir("next");
    let c = ctx(&d, CONFIG);
    let next = || run(&c, &Fake::default(), &["next", "sendmeter"]);
    assert_eq!(next(), (1, "sendmeter: queue empty\n".into()));
    q(&c, &["add", "12", "13", "14"]);
    q(&c, &["block", "13", "--on", "12"]);
    assert_eq!(next(), (0, "12\n".into()));
    // #12 gets a lane: skipped; #13 is blocked on it; #14 is next.
    ev(
        &c,
        100,
        12,
        "spawn",
        json!({"role": "impl", "agent": "sendmeter-impl-12", "branch": "issue-12", "effort": "high"}),
    );
    assert_eq!(next(), (0, "14\n".into()));
    ev(
        &c,
        110,
        14,
        "spawn",
        json!({"role": "impl", "agent": "sendmeter-impl-14", "branch": "issue-14"}),
    );
    assert_eq!(next(), (2, "#13 is blocked by #12\n".into()));
    // The blocker merges: it leaves the queue and #13 is free.
    ev(
        &c,
        120,
        12,
        "merged",
        json!({"pr": "40", "merge_sha": "abc", "into": "staging"}),
    );
    assert_eq!(next(), (0, "13\n".into()));
    assert_eq!(order(&c), [13, 14]);
}

// --- board + serve ---

fn board_log(c: &herdsman::Ctx) {
    q(c, &["add", "17", "18", "12"]);
    q(c, &["block", "18", "--on", "17"]);
    ev(
        c,
        1_000_000 - 300,
        12,
        "spawn",
        json!({"role": "impl", "agent": "sendmeter-impl-12", "branch": "issue-12", "effort": "high"}),
    );
    ev(
        c,
        1_000_000 - 7200,
        9,
        "spawn",
        json!({"role": "rev", "agent": "sendmeter-rev-9-r1-with-a-long-name"}),
    );
    ev(
        c,
        1_000_000 - 3600,
        13,
        "merged",
        json!({"pr": "15", "merge_sha": "08c9338", "into": "staging"}),
    );
    ev(
        c,
        1_000_000 - 200_000,
        10,
        "merged",
        json!({"pr": "11", "merge_sha": "3514adb", "into": "staging"}),
    );
}

const BOARD: &str = "\
sendmeter: 3 queued, 2 active
 Q ISSUE  STATE       LANE                 EFF   AGE  LAST
 1 #17    queued      -                    -     -    -
 2 #18    queued      -                    -     -    on #17
 3 #12    impl        sendmeter-impl-12    high  5m   spawn
 - #9     review(1)   sendmeter-rev-9-r1-w -     2h   spawn
 - #13    merged      -                    -     60m  merged
";

#[test]
fn board_matches_fixture_and_fits_phone_width() {
    let d = tmpdir("board");
    let c = ctx(&d, CONFIG);
    board_log(&c);
    let (code, out) = run(&c, &Fake::default(), &["board", "sendmeter"]);
    assert_eq!(code, 0);
    assert_eq!(out, BOARD);
    assert!(out.lines().all(|l| l.chars().count() <= 60), "{out}");
    assert_eq!(run(&c, &Fake::default(), &["board"]).1, BOARD, "all fleets");
}

#[test]
fn serve_is_read_only_html() {
    let d = tmpdir("serve");
    let c = ctx(&d, CONFIG);
    board_log(&c);
    let r = Fake::default();
    let srv = herdsman::serve::Server::new(&c, &r, None);
    let res = String::from_utf8(srv.respond("GET / HTTP/1.1\r\nHost: x\r\n\r\n", 1_000_000)).unwrap();
    assert!(
        res.starts_with("HTTP/1.1 200 OK\r\n") && res.contains("Content-Type: text/html"),
        "{res}"
    );
    for n in ["#17", "#18", "#12"] {
        assert!(res.contains(n), "queued {n} missing");
    }
    assert!(
        res.contains("https://github.com/jirathip-dev/sendmeter/pull/15"),
        "PR link"
    );
    // #60: the Events tab body loads on tab open.
    let res = String::from_utf8(srv.respond("GET /frag?id=events HTTP/1.1\r\nHost: x\r\n\r\n", 1_000_000)).unwrap();
    assert!(
        res.contains("https://github.com/jirathip-dev/sendmeter/issues/12"),
        "issue link"
    );
    assert!(res.contains("actions?query=branch%3Aissue-12"), "Actions runs link");
    for m in ["POST", "PUT", "DELETE", "PATCH", "HEAD"] {
        let res = String::from_utf8(srv.respond(&format!("{m} / HTTP/1.1\r\n\r\n"), 1_000_000)).unwrap();
        assert!(res.starts_with("HTTP/1.1 405 "), "{m}: {res}");
    }
    // Over a real socket.
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let c2 = ctx(&d, CONFIG);
    std::thread::spawn(move || herdsman::serve::serve_on(&c2, &Fake::default(), l, true));
    let get = |req: &str| {
        let mut s = std::net::TcpStream::connect(addr).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut b = String::new();
        s.read_to_string(&mut b).unwrap();
        b
    };
    let page = get("GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(
        page.starts_with("HTTP/1.1 200 OK") && page.contains("#17") && page.contains("#18"),
        "{page}"
    );
    let post = get("POST /queue HTTP/1.1\r\nContent-Length: 2\r\n\r\nhi");
    assert!(post.starts_with("HTTP/1.1 405 "), "{post}");
    assert_eq!(order(&c), [17, 18, 12], "nothing changed");
}

// --- docs ---

#[test]
fn agent_doc_and_roles_cover_m3() {
    let d = tmpdir("help-m3");
    let c = ctx(&d, CONFIG);
    let (_, out) = run(&c, &Fake::default(), &["help", "--agent"]);
    for w in [
        "herdsman brief",
        "herdsman pr",
        "herdsman wait",
        "--until",
        "herdsman queue <FLEET> add",
        "herdsman queue <FLEET> mv",
        "herdsman queue <FLEET> rm",
        "herdsman queue <FLEET> block",
        "herdsman queue <FLEET> ls",
        "herdsman next",
        "herdsman board",
        "herdsman serve",
        "MERGED",
        "124",
    ] {
        assert!(out.contains(w), "missing {w}");
    }
    for (name, role) in [
        ("orch", include_str!("../roles/orch.md")),
        ("impl", include_str!("../roles/impl.md")),
    ] {
        for l in role.lines().filter(|l| l.to_lowercase().contains("comment")) {
            assert!(
                l.contains("Never comment") || l.contains("never comment") || l.contains("one closing comment"),
                "{name}: {l}"
            );
        }
        assert!(
            !role.contains("gh issue view") || role.contains("not `gh issue view`") || role.contains("Don't fetch"),
            "{name}"
        );
    }
    assert!(include_str!("../roles/orch.md").contains("herdsman next"));
}
