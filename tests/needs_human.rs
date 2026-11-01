//! #130: `needs-human` is never silent: it pushes by default, and every such issue gets a needs-you
//! card (Queue + Herd strip) with [merge anyway] only when the PR could merge but for the round limit.
mod common;
use common::*;
use herdsman::config::Config;
use herdsman::events::{append, fold, read, Event};
use herdsman::serve::Server;
use serde_json::json;

const NOW: u64 = 1_000_000;
const ADDR: &str = "127.0.0.1:7777";
const HEAD: &str = "1111111111111111111111111111111111111111";

/// #12: four review rounds (the last `last` at HEAD), then `needs-human`; the PR cache says open, green, mergeable.
fn fixture(name: &str, last: &str) -> herdsman::Ctx {
    let mut c = ctx(&tmpdir(name), CONFIG);
    c.now = NOW;
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "12"]).0, 0);
    let ev = |ts, kind, d| append(&c.state, &Event::new(ts, "sendmeter", Some(12), kind, d)).unwrap();
    ev(
        10,
        "spawn",
        json!({"role": "impl", "agent": "sendmeter-impl-12", "branch": "issue-12"}),
    );
    for (i, sha) in ["2", "3", "4"].iter().enumerate() {
        ev(
            20 + i as u64,
            "verdict",
            json!({"verdict": "FAIL", "sha": sha.repeat(40)}),
        );
    }
    let body = format!("{last}\n\nblocking: src/x.rs:9 still wrong\n");
    ev(30, "verdict", json!({"verdict": last, "sha": HEAD, "body": body}));
    ev(
        31,
        "needs-human",
        json!({"code": "ROUND_LIMIT", "reason": "review rounds ran out (4/3)"}),
    );
    let checks = json!([{"__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "SUCCESS"}]);
    let pr = json!({"number": 34, "state": "OPEN", "baseRefName": "staging", "headRefOid": HEAD, "mergeable": "MERGEABLE", "statusCheckRollup": checks});
    let cache = c.state.parent().unwrap().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let file = json!({"ts": NOW, "branch": "issue-12", "pr": pr});
    std::fs::write(cache.join("pr-sendmeter-12.json"), file.to_string()).unwrap();
    c
}

fn runner() -> Fake {
    Fake::default()
        .reply("herdr agent list", NO_AGENTS)
        .reply("herdr workspace list", r#"{"result":{"workspaces":[]}}"#)
}

fn get(s: &Server, path: &str, cookie: &str) -> String {
    let res = s.respond(&format!("GET {path} HTTP/1.1\r\nHost: {ADDR}\r\n{cookie}\r\n"), NOW);
    String::from_utf8_lossy(&res).into_owned()
}

/// The `#needs` strip's card for #12.
fn card(page: &str) -> &str {
    let a = page.find("<div class=ask data-n=12>").expect("no needs-human card");
    &page[a..a + page[a..].find("</div>").unwrap()]
}

#[test]
fn needs_human_pushes_with_the_default_on() {
    let cfg = Config::parse("[[notify]]\ntype = \"webpush\"\nsub = \"mailto:owner@example.com\"\n").unwrap();
    assert!(cfg.notify[0].wants("needs-human"));
}

#[test]
fn card_with_a_pass_at_head_offers_merge_anyway_to_a_paired_phone_only() {
    let c = fixture("nh-pass", "PASS");
    let r = runner();
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let cookie = pair(&s, &c, "", NOW);
    let page = get(&s, "/", &cookie);
    let k = card(&page);
    assert!(
        k.contains("sendmeter · #12") && k.contains("review rounds ran out (4/3)"),
        "{k}"
    );
    assert!(k.contains("PASS at 1111111") && k.contains("mergeable"), "{k}");
    assert!(k.contains("value=override") && k.contains("data-confirm="), "{k}");
    assert!(
        k.contains("value=extend>one more round") && k.contains("value=park>park"),
        "{k}"
    );
    assert!(
        page.find("<div class=ask data-n=12>") < page.find("<main>"),
        "above every tab"
    );

    let unpaired = get(&s, "/", "");
    let k = card(&unpaired);
    assert!(!k.contains("<form"), "an unpaired phone sees no buttons: {k}");
    let ro = Server::new(&c, &r, None);
    let page = get(&ro, "/", "");
    let k = card(&page);
    assert!(!k.contains("<form"), "a read-only server shows no buttons: {k}");
}

#[test]
fn card_with_a_fail_has_no_merge_anyway() {
    let c = fixture("nh-fail", "FAIL");
    let r = runner();
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let cookie = pair(&s, &c, "", NOW);
    let page = get(&s, "/", &cookie);
    let k = card(&page);
    assert!(k.contains("FAIL: blocking: src/x.rs:9 still wrong"), "{k}");
    assert!(!k.contains("value=override"), "{k}");
    assert!(k.contains("value=extend") && k.contains("value=park"), "{k}");
}

#[test]
fn herd_marks_the_issue_and_park_clears_the_card() {
    let c = fixture("nh-herd", "FAIL");
    let r = runner();
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let res = get(&s, "/herd.json", "");
    let j: serde_json::Value = serde_json::from_str(&res[res.find("\r\n\r\n").unwrap() + 4..]).unwrap();
    assert_eq!(j["fleets"][0]["needs"], json!([12]));
    assert_eq!(j["fleets"][0]["queue"][0]["issue"], 12, "no lane: a gate horse");

    let page = get(&s, "/", "");
    let i = page.find("name=mtime value=").unwrap() + 17;
    let mt: String = page[i..].chars().take_while(char::is_ascii_digit).collect();
    let body = format!("op=park&fleet=sendmeter&mtime={mt}&issue=12");
    let post = |extra: &str| {
        let req = format!(
            "POST / HTTP/1.1\r\nHost: {ADDR}\r\n{extra}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        String::from_utf8_lossy(&s.respond(&req, NOW)).into_owned()
    };
    assert!(post("").starts_with("HTTP/1.1 401"), "unpaired cannot park");
    let cookie = pair(&s, &c, "", NOW);
    assert!(post(&cookie).starts_with("HTTP/1.1 303"));
    let st = &fold(&read(&c.state).unwrap())[&("sendmeter".to_string(), 12)];
    assert_eq!(st.state, "parked");
    assert!(!get(&s, "/", &cookie).contains("data-n=12"), "parked: no card");
}
