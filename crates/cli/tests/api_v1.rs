mod common;
use common::*;
use drover::events::{append, Event};
use drover::serve::Server;
use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:7777";
const T: u64 = 1_000_000;

fn req(s: &Server, method: &str, path: &str, body: &str, extra: &str) -> (String, Value) {
    let r = format!(
        "{method} {path} HTTP/1.1\r\nHost: {ADDR}\r\n{extra}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let res = String::from_utf8_lossy(&s.respond(&r, T)).into_owned();
    let (head, body) = res.split_once("\r\n\r\n").unwrap();
    assert!(head.contains("Content-Type: application/json"), "{res}");
    (
        head.lines().next().unwrap().to_string(),
        serde_json::from_str(body).unwrap(),
    )
}

fn golden(name: &str, mut v: Value) {
    if v.get("mtime").is_some() {
        v["mtime"] = json!("<mtime>");
    }
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/golden/api/{name}.json"));
    let got = serde_json::to_string_pretty(&v).unwrap() + "\n";
    if std::env::var_os("BLESS").is_some() {
        std::fs::write(&file, &got).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(&file).unwrap_or_default(),
        got,
        "{name}: `BLESS=1` rewrites it"
    );
}

fn logged(name: &str) -> drover::Ctx {
    let c = ctx(&tmpdir(name), CONFIG);
    assert_eq!(
        run(&c, &Fake::default(), &["queue", "sendmeter", "add", "12", "17"]).0,
        0
    );
    assert_eq!(
        run(
            &c,
            &Fake::default(),
            &["queue", "sendmeter", "block", "--on", "12", "17"]
        )
        .0,
        0
    );
    let ev = |ts, issue, kind, d| append(&c.state, &Event::new(ts, "sendmeter", Some(issue), kind, d)).unwrap();
    ev(
        900_100,
        12,
        "spawn",
        json!({"role": "impl", "agent": "sendmeter-impl-12", "harness": "claude"}),
    );
    ev(
        900_200,
        9,
        "needs-decision",
        json!({"id": "q7", "question": "Which DB?", "options": ["sqlite", "postgres"], "recommend": 1}),
    );
    ev(900_300, 12, "merged", json!({"pr": 40, "into": "staging"}));
    c
}

const SCREEN: &str = "⏺ Bash(cargo test)\n  ⎿ ok\n";

fn fixture(name: &str) -> (drover::Ctx, OnShed) {
    let r = OnShed::new(Fake::default());
    r.1.lane("sendmeter-impl-12", "working", "/x");
    r.1.screen("sendmeter-impl-12", SCREEN);
    (logged(name), r)
}

#[test]
fn state_events_and_lane_json_match_the_goldens() {
    let (c, r) = fixture("api-get");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let (status, state) = req(&s, "GET", "/api/v1/state", "", "");
    assert_eq!(status, "HTTP/1.1 200 OK");
    golden("state", state);
    let (_, all) = req(&s, "GET", "/api/v1/events", "", "");
    golden("events", all);
    let (_, after) = req(&s, "GET", "/api/v1/events?since=4", "", "");
    assert_eq!(after["list"].as_array().unwrap().len(), 1, "{after}");
    let cookie = pair(&s, &c, "Mozilla/5.0 (iPhone) Safari/604.1", T);
    let (status, lane) = req(&s, "GET", "/api/v1/lane/sendmeter-impl-12", "", &cookie);
    assert_eq!(status, "HTTP/1.1 200 OK");
    golden("lane", lane);
    let (status, none) = req(&s, "GET", "/api/v1/lane/nobody", "", &cookie);
    assert_eq!(
        (status.as_str(), &none["code"]),
        ("HTTP/1.1 404 Not Found", &json!("NOT_FOUND"))
    );
}

#[test]
fn over_500_new_events_ask_for_a_refresh() {
    let (c, r) = fixture("api-refresh");
    for i in 0..501 {
        append(
            &c.state,
            &Event::new(950_000 + i, "sendmeter", None, "driven", json!({})),
        )
        .unwrap();
    }
    let (_, v) = req(
        &Server::new(&c, &r, Some(ADDR.into())),
        "GET",
        "/api/v1/events?since=1",
        "",
        "",
    );
    assert_eq!((&v["refresh"], &v["list"]), (&json!(true), &json!([])), "{v}");
}

#[test]
fn refusals_are_json_with_a_code() {
    let (c, r) = fixture("api-refuse");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let code = |method, path, body, extra| {
        let (status, v) = req(&s, method, path, body, extra);
        assert_eq!(v["ok"], json!(false), "{v}");
        (status, v["code"].as_str().unwrap().to_string())
    };
    let want = |s: &str, c: &str| (format!("HTTP/1.1 {s}"), c.to_string());
    assert_eq!(
        code("GET", "/api/v1/lane/sendmeter-impl-12", "", ""),
        want("401 Unauthorized", "UNPAIRED")
    );
    assert_eq!(
        code("POST", "/api/v1/drive", "fleet=sendmeter", ""),
        want("401 Unauthorized", "UNPAIRED")
    );
    let evil = "Origin: https://evil.example\r\n";
    assert_eq!(
        code("GET", "/api/v1/state", "", evil),
        want("403 Forbidden", "FORBIDDEN")
    );
    assert_eq!(
        code("POST", "/api/v1/pause", "fleet=sendmeter", evil),
        want("403 Forbidden", "FORBIDDEN")
    );
    assert!(!r.1.called("read", "sendmeter-impl-12"));
    assert_eq!(
        code("POST", "/api/v1/pause", "fleet=sendmeter&mtime=1", ""),
        want("409 Conflict", "STALE")
    );
    assert_eq!(code("GET", "/api/v2/state", "", ""), want("404 Not Found", "NOT_FOUND"));
    let ro = Server::new(&c, &r, None);
    let (status, v) = req(&ro, "POST", "/api/v1/pause", "fleet=sendmeter", "");
    assert_eq!(
        (status.as_str(), &v["code"]),
        ("HTTP/1.1 405 Method Not Allowed", &json!("READ_ONLY"))
    );
}

#[test]
fn a_post_runs_the_boards_op_and_answers_json() {
    let (c, r) = fixture("api-post");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let mtime = req(&s, "GET", "/api/v1/state", "", "").1["mtime"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, v) = req(
        &s,
        "POST",
        "/api/v1/pause",
        &format!("fleet=sendmeter&mtime={mtime}"),
        "",
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    golden("pause", v);
    assert_eq!(
        req(&s, "GET", "/api/v1/state", "", "").1["fleets"][0]["paused"],
        json!(true)
    );
    let cookie = pair(&s, &c, "Mozilla/5.0 (iPhone) Safari/604.1", T);
    let mtime = req(&s, "GET", "/api/v1/state", "", "").1["mtime"].clone();
    let body = format!("fleet=sendmeter&id=q7&choice=1&mtime={}", mtime.as_str().unwrap());
    let (status, v) = req(&s, "POST", "/api/v1/answer", &body, &cookie);
    assert_eq!((status.as_str(), &v["ok"]), ("HTTP/1.1 200 OK", &json!(true)), "{v}");
    assert_eq!(req(&s, "GET", "/api/v1/state", "", "").1["asks"], json!([]));
}

#[test]
fn queue_rows_carry_their_marks_and_qset_sets_one() {
    let (c, r) = fixture("api-marks");
    let add = ["queue", "sendmeter", "add", "21", "--verify", "--repro"];
    assert_eq!(run(&c, &Fake::default(), &add).0, 0);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let (status, q) = req(&s, "GET", "/api/v1/queue", "", "");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(
        q["fleets"][0]["marks"]["21"],
        json!({"design": false, "verify": "on", "repro": true, "serial": [], "needs": [], "note": null}),
        "{q}"
    );
    assert_eq!(q["fleets"][0]["marks"]["17"]["verify"], json!(null));
    let body = "fleet=sendmeter&issue=21&key=design&value=on";
    let (status, v) = req(&s, "POST", "/api/v1/qset", body, "");
    assert_eq!(
        (status.as_str(), &v["code"]),
        ("HTTP/1.1 401 Unauthorized", &json!("UNPAIRED"))
    );
    let ls = || {
        let out = run(&c, &Fake::default(), &["queue", "sendmeter", "ls", "--json"]).1;
        let rows: Value = serde_json::from_str(&out).unwrap();
        rows.as_array()
            .unwrap()
            .iter()
            .find(|r| r["issue"] == 21)
            .unwrap()
            .clone()
    };
    assert_eq!(ls()["design"], json!(false));
    let cookie = pair(&s, &c, "Mozilla/5.0 (iPhone) Safari/604.1", T);
    let (status, v) = req(&s, "POST", "/api/v1/qset", body, &cookie);
    assert_eq!((status.as_str(), &v["ok"]), ("HTTP/1.1 200 OK", &json!(true)), "{v}");
    assert_eq!((&ls()["design"], &ls()["verify"]), (&json!(true), &json!(true)));
    let bad = "fleet=sendmeter&issue=21&key=verify&value=maybe";
    let (status, v) = req(&s, "POST", "/api/v1/qset", bad, &cookie);
    assert_eq!(
        (status.as_str(), &v["ok"]),
        ("HTTP/1.1 400 Bad Request", &json!(false)),
        "{v}"
    );
}

#[test]
fn an_issue_carries_its_latest_why_owner_line() {
    let (c, none) = (ctx(&tmpdir("api-why"), CONFIG), Fake::default());
    let s = Server::new(&c, &none, Some(ADDR.into()));
    let (_, v) = req(&s, "GET", "/api/v1/issue/sendmeter/12", "", "");
    assert_eq!(v["why"], json!(null), "{v}");
    for (round, sha, owner) in [(1, "a", "first"), (2, "b", "retries no longer double the meter")] {
        let d = json!({"sha": sha.repeat(40), "round": round, "owner": owner, "files": ["a.rs"]});
        append(&c.state, &Event::new(T - 10 + round, "sendmeter", Some(12), "why", d)).unwrap();
    }
    let s = Server::new(&c, &none, Some(ADDR.into()));
    let (status, v) = req(&s, "GET", "/api/v1/issue/sendmeter/12", "", "");
    assert_eq!(status, "HTTP/1.1 200 OK");
    let want = json!({"sha": "b".repeat(40), "round": 2, "owner": "retries no longer double the meter"});
    assert_eq!(v["why"], want, "{v}");
}

#[test]
fn an_issues_verdicts_come_newest_first_with_repro_and_shots() {
    let d = tmpdir("api-issue");
    let verify = "\n[fleet.sendmeter.role.verify]\nharness = \"hermes\"\nprofile = \"fleet-verify\"\n";
    let c = ctx(&d, &(CONFIG.to_string() + verify));
    assert_eq!(
        run(&c, &Fake::default(), &["queue", "sendmeter", "add", "12", "--repro"]).0,
        0
    );
    let lane = d.join("lane");
    std::fs::create_dir_all(lane.join("shots")).unwrap();
    std::fs::write(lane.join("shots/home-390.png"), b"\x89PNG narrow").unwrap();
    std::fs::write(lane.join("shots/home-1440.png"), b"\x89PNG wide").unwrap();
    std::fs::write(lane.join("shots/home-console.log"), "console error: boom\n").unwrap();
    let file = lane.join("verdict.md").to_string_lossy().to_string();
    let spawn = |ts, role: &str| {
        let (w, check) = (lane.to_string_lossy(), role != "impl");
        let d = json!({"role": role, "stage": role, "check": check, "agent": format!("{role}-{ts}"), "worktree": w});
        append(&c.state, &Event::new(ts, "sendmeter", Some(12), "spawn", d)).unwrap();
    };
    let verdict = |v: &str, s: &str, body: &str| {
        std::fs::write(&file, body).unwrap();
        let args = ["verdict", "sendmeter", "12", v, "--sha", s, "--file", &file];
        let (code, out) = run(&c, &Fake::default().reply("gh api", s), &args);
        assert_eq!(code, 0, "{out}");
    };
    let (a, b) = ("a".repeat(40), "b".repeat(40));
    let repro =
        "Repro: run\n\n```sh\ncargo test repro\n```\n```text\nFAILED on base\n```\n```sh\ncargo test repro\n```\n\
                 ```text\nok at head\n```\n";
    spawn(1, "impl");
    spawn(2, "rev");
    verdict("FAIL", &a, "FAIL\n\n- non-blocking: the test is flaky\n");
    spawn(3, "impl");
    spawn(4, "rev");
    verdict("PASS", &b, &format!("PASS\n\n{repro}"));
    spawn(5, "verify");
    verdict(
        "PASS",
        &b,
        "PASS\n\nshots/home-390.png and shots/home-1440.png\n\n- `shots/home-console.log`\n",
    );
    let none = Fake::default();
    let s = Server::new(&c, &none, Some(ADDR.into()));
    let (status, v) = req(&s, "GET", "/api/v1/issue/sendmeter/12", "", "");
    assert_eq!(status, "HTTP/1.1 200 OK");
    let vs = v["verdicts"].as_array().unwrap();
    let row = |i: usize| (vs[i]["role"].clone(), vs[i]["verdict"].clone(), vs[i]["sha"].clone());
    assert_eq!(vs.len(), 3, "{v}");
    assert_eq!(row(0), (json!("verify"), json!("PASS"), json!(b)));
    let want = json!(["shots/home-390.png", "shots/home-1440.png", "shots/home-console.log"]);
    assert_eq!(vs[0]["shots"], want);
    assert_eq!(vs[0]["repro"], json!(null));
    assert_eq!(row(1), (json!("rev"), json!("PASS"), json!(b)));
    assert_eq!(vs[1]["repro"]["kind"], json!("run"));
    let text = vs[1]["repro"]["text"].as_str().unwrap();
    assert!(text.contains("cargo test repro") && text.contains("FAILED on base") && text.contains("ok at head"));
    assert_eq!(row(2), (json!("rev"), json!("FAIL"), json!(a)));
    assert_eq!(vs[2]["findings"], json!("- non-blocking: the test is flaky"));
    assert_eq!((&vs[1]["shots"], &vs[2]["repro"]), (&json!([]), &json!(null)));
    let r =
        String::from_utf8_lossy(&s.respond(&format!("GET /shot/sendmeter/12/1 HTTP/1.1\r\nHost: {ADDR}\r\n\r\n"), T))
            .into_owned();
    assert!(r.starts_with("HTTP/1.1 200") && r.ends_with("PNG wide"), "{r}");
    let r =
        String::from_utf8_lossy(&s.respond(&format!("GET /shot/sendmeter/12/2 HTTP/1.1\r\nHost: {ADDR}\r\n\r\n"), T))
            .into_owned();
    assert!(
        r.starts_with("HTTP/1.1 200") && r.ends_with("console error: boom\n"),
        "{r}"
    );
}

#[test]
fn lessons_show_the_last_run_and_the_next_due_date() {
    let (c, r) = fixture("api-lessons");
    let cfg = drover::config::Config::load(&c.config).unwrap();
    drover::lesson::weekly(&c, &cfg, &Fake::default(), &mut vec![]).unwrap();
    let d = json!({"role": "lesson", "stage": "impl", "agent": "sendmeter-lesson-30", "harness": "claude"});
    append(&c.state, &Event::new(900_400, "sendmeter", Some(30), "spawn", d)).unwrap();
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let (status, v) = req(&s, "GET", "/api/v1/lessons", "", "");
    assert_eq!(status, "HTTP/1.1 200 OK");
    let want = json!({"fleets": [{"fleet": "sendmeter", "lessons": "weekly",
        "last": {"issue": 30, "ts": 900_400, "state": "impl"}, "next": c.now + 7 * 86400}]});
    assert_eq!(v, want);
    let (status, v) = req(&s, "GET", "/api/v1/lessons/sendmeter/digest?days=3", "", "");
    assert_eq!((status.as_str(), &v["days"]), ("HTTP/1.1 200 OK", &json!(3)));
    assert!(
        v["text"]
            .as_str()
            .unwrap()
            .starts_with("# Lessons digest: sendmeter, last 3 days"),
        "{v}"
    );
    let (status, _) = req(&s, "GET", "/api/v1/lessons/digest", "", "");
    assert_eq!(status, "HTTP/1.1 404 Not Found");
    let (status, v) = req(&s, "POST", "/api/v1/lesson", "fleet=sendmeter", "");
    assert_eq!(
        (status.as_str(), &v["code"]),
        ("HTTP/1.1 401 Unauthorized", &json!("UNPAIRED"))
    );
    assert!(!r.called("gh issue create"));
}
