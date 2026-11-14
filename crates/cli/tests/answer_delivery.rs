mod common;
use common::*;
use drover::events::{append, read, Event};
use drover::serve::Server;
use serde_json::{json, Value};

const NOW: u64 = 1_000_000;
const LANE: &str = "sendmeter-impl-36";

fn kinds(c: &drover::Ctx, kind: &str) -> Vec<Event> {
    read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

fn spawn(c: &drover::Ctx, extra: Value) {
    let mut d = json!({"role": "impl", "agent": LANE, "pane": LANE, "host": "mini"});
    d.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    append(&c.state, &Event::new(NOW - 60, "sendmeter", Some(36), "spawn", d)).unwrap();
}

fn lane_ask(c: &drover::Ctx) {
    std::env::set_var("DROVER_LANE", "sendmeter:36:impl");
    let args = [
        "ask",
        "sendmeter",
        "36",
        "Split it?",
        "--option",
        "yes",
        "--option",
        "no",
    ];
    let (code, out) = run(c, &Fake::default(), &args);
    assert_eq!((code, out.as_str()), (0, "q1\n"));
}

fn shed(c: &drover::Ctx) -> FakeShed {
    let sh = FakeShed::new();
    sh.attach(c);
    sh.lane(LANE, "idle", "/x");
    sh
}

fn typed(sh: &FakeShed, pane: &str, text: &str) -> bool {
    let seen = sh.lock().seen.clone();
    let send = (seen.iter()).position(|(m, p)| m == "send" && p["name"] == LANE && p["text"] == text);
    let enter = (seen.iter()).rposition(|(m, p)| m == "keys" && p["name"] == pane && p["keys"] == json!(["Enter"]));
    matches!((send, enter), (Some(s), Some(e)) if s < e)
}

#[test]
fn a_lane_ask_stores_its_agent_and_the_answer_is_typed_into_its_pane() {
    let c = ctx(&tmpdir("deliver-lane"), CONFIG);
    let sh = shed(&c);
    spawn(&c, json!({}));
    lane_ask(&c);
    let a = &kinds(&c, "needs-decision")[0];
    assert_eq!((a.str("agent"), a.str("host")), (Some(LANE), Some("mini")));
    let (code, out) = run(
        &c,
        &Fake::default(),
        &["answer", "sendmeter", "q1", "2", "--by", "orchestrator"],
    );
    assert_eq!((code, out.as_str()), (0, "sendmeter q1: no -> sendmeter-impl-36\n"));
    assert_eq!(sh.sent(LANE), ["DECISION q1: no"], "{:?}", sh.methods());
    assert!(typed(&sh, LANE, "DECISION q1: no"), "{:?}", sh.methods());
    assert!(sh.called("wait", LANE), "the delivery is confirmed: {:?}", sh.methods());
    let d = kinds(&c, "answer-delivered");
    assert_eq!(
        (d.len(), d[0].str("id"), d[0].str("agent")),
        (1, Some("q1"), Some(LANE))
    );
}

#[test]
fn an_ask_without_an_agent_is_answered_into_the_issues_current_lane() {
    let c = ctx(&tmpdir("deliver-null"), CONFIG);
    lane_ask(&c);
    assert_eq!(kinds(&c, "needs-decision")[0].str("agent"), None);
    let sh = shed(&c);
    spawn(&c, json!({}));
    assert_eq!(run(&c, &Fake::default(), &["answer", "sendmeter", "q1", "1"]).0, 0);
    assert!(typed(&sh, LANE, "DECISION q1: yes"), "{:?}", sh.methods());
    assert_eq!(kinds(&c, "answer-delivered")[0].str("agent"), Some(LANE));
}

#[test]
fn a_remote_lane_is_answered_on_its_machines_shed() {
    let c = ctx(&tmpdir("deliver-remote"), CONFIG);
    let (here, air) = (FakeShed::new(), FakeShed::new());
    here.attach(&c);
    air.lane(LANE, "idle", "/x");
    spawn(&c, json!({"host": "air", "machine": "air"}));
    lane_ask(&c);
    let f = Fake::default();
    let r = OverSsh {
        f: &f,
        hosts: vec![("air", &air)],
    };
    assert_eq!(run(&c, &r, &["answer", "sendmeter", "q1", "1"]).0, 0);
    assert!(typed(&air, LANE, "DECISION q1: yes"), "{:?}", air.methods());
    assert!(!here.called("send", LANE), "nothing typed on this host's shed");
    assert!(f.called("ssh "), "{:?}", f.calls.borrow());
    assert_eq!(kinds(&c, "answer-delivered").len(), 1);
}

#[test]
fn drive_retries_an_undelivered_answer_until_it_lands_or_the_lane_is_gone() {
    let c = ctx(&tmpdir("deliver-retry"), CONFIG);
    let sh = FakeShed::new();
    sh.attach(&c);
    spawn(&c, json!({}));
    lane_ask(&c);
    assert_eq!(run(&c, &Fake::default(), &["answer", "sendmeter", "q1", "1"]).0, 0);
    let u = kinds(&c, "answer-undelivered");
    assert_eq!(
        (u.len(), u[0].str("agent"), u[0].str("text")),
        (1, Some(LANE), Some("DECISION q1: yes"))
    );
    let page = |r: &Fake| {
        let r = sh.on(r);
        let s = Server::new(&c, &r, Some("127.0.0.1:7777".into()));
        let page =
            String::from_utf8_lossy(&s.respond("GET / HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\r\n", NOW)).into_owned();
        with_queue(&page, &s, "127.0.0.1:7777", NOW)
    };
    assert!(page(&Fake::default()).contains("<span class=r>answer q1 not delivered: "));

    let asked = || sh.all("get").iter().filter(|p| p["name"] == LANE).count();
    let before = asked();
    run(&c, &Fake::default(), &["drive", "--fast"]);
    assert!(asked() > before, "{:?}", sh.methods());
    assert_eq!(kinds(&c, "answer-undelivered").len(), 1);

    sh.lane(LANE, "idle", "/x");
    run(&c, &Fake::default(), &["drive", "--fast"]);
    assert!(typed(&sh, LANE, "DECISION q1: yes"), "{:?}", sh.methods());
    assert_eq!(kinds(&c, "answer-delivered")[0].str("agent"), Some(LANE));
    assert!(!page(&Fake::default()).contains("not delivered"));
    let tries = sh.sent(LANE).len();
    run(&c, &Fake::default(), &["drive", "--fast"]);
    assert_eq!(sh.sent(LANE).len(), tries, "typed once: {:?}", sh.sent(LANE));

    let c = ctx(&tmpdir("deliver-gone"), CONFIG);
    let sh = FakeShed::new();
    sh.attach(&c);
    spawn(&c, json!({}));
    lane_ask(&c);
    assert_eq!(run(&c, &Fake::default(), &["answer", "sendmeter", "q1", "1"]).0, 0);
    let closed = Event::new(NOW, "sendmeter", Some(36), "lane-closed", json!({"agent": LANE}));
    append(&c.state, &closed).unwrap();
    sh.lane(LANE, "idle", "/x");
    let tries = sh.sent(LANE).len();
    run(&c, &Fake::default(), &["drive", "--fast"]);
    assert_eq!(sh.sent(LANE).len(), tries, "{:?}", sh.methods());
}

#[test]
fn a_sleeping_lanes_answer_waits_for_room_then_lands() {
    let c = ctx(&tmpdir("deliver-asleep"), CONFIG);
    let sh = FakeShed::new();
    sh.attach(&c);
    sh.lane(LANE, "hibernated", "/x");
    spawn(&c, json!({}));
    lane_ask(&c);
    let host = |load: &str| {
        Fake::default()
            .reply("sysctl -n kern.memorystatus_vm_pressure_level", "1\n")
            .reply("sysctl -n vm.loadavg", &format!("{{ {load} 1.20 1.00 }}\n"))
            .reply("sysctl -n hw.ncpu", "10\n")
            .reply("vm_stat", "Pages free: 1.\nSwapouts:                     100.\n")
    };
    assert_eq!(run(&c, &host("50.00"), &["answer", "sendmeter", "q1", "1"]).0, 0);
    assert!(sh.sent(LANE).is_empty(), "{:?}", sh.methods());
    let u = kinds(&c, "answer-undelivered");
    assert_eq!(u.len(), 1);
    assert!(u[0].str("reason").unwrap().contains("HOST_BUSY"), "{u:?}");
    run(&c, &host("50.00"), &["drive", "--fast"]);
    assert!(sh.sent(LANE).is_empty(), "held while busy: {:?}", sh.methods());
    run(&c, &host("1.50"), &["drive", "--fast"]);
    assert!(typed(&sh, LANE, "DECISION q1: yes"), "{:?}", sh.methods());
    assert_eq!(kinds(&c, "answer-delivered").len(), 1);
}

#[test]
fn a_superseded_lanes_successor_gets_its_answer_typed_into_its_pane() {
    let c = ctx(&tmpdir("deliver-supersede"), CONFIG);
    let sh = FakeShed::new();
    sh.attach(&c);
    let wt = c.worktrees.join("sendmeter/issue-36");
    std::fs::create_dir_all(&wt).unwrap();
    spawn(&c, json!({"worktree": wt, "branch": "issue-36", "pane": "old-36"}));
    let r = Fake::default()
        .reply("which shed", "/opt/shed/bin/shed\n")
        .reply("sysctl -n kern.memorystatus_vm_pressure_level", "1\n")
        .reply("sysctl -n vm.loadavg", "{ 1.50 1.20 1.00 }\n")
        .reply("sysctl -n hw.ncpu", "10\n")
        .reply("vm_stat", "Pages free: 1.\nSwapouts:                     100.\n")
        .reply(&format!("git -C {} rev-list", wt.display()), "0\n")
        .reply("git -C", "")
        .reply("gh issue view 36", r#"{"title":"t","body":"b"}"#);
    let (code, out) = run(&c, &r, &["spawn", "sendmeter", "36", "impl", "--supersede"]);
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    let ev = read(&c.state).unwrap().to_vec();
    let closed = ev
        .iter()
        .position(|e| e.kind == "lane-closed")
        .expect("old lane closed");
    let spawned = ev.iter().rposition(|e| e.kind == "spawn").unwrap();
    assert!(closed < spawned, "{ev:?}");
    assert_eq!(
        (ev[closed].str("pane"), ev[spawned].str("pane")),
        (Some("old-36"), Some(LANE))
    );
    assert!(sh.called("kill", "old-36"), "{:?}", sh.methods());
    assert_eq!(sh.last("run")["name"], LANE);
    let ran = sh.lock().seen.len();
    lane_ask(&c);
    let (code, out) = run(&c, &Fake::default(), &["answer", "sendmeter", "q1", "1"]);
    assert_eq!((code, out.as_str()), (0, "sendmeter q1: yes -> sendmeter-impl-36\n"));
    let after: Vec<_> = sh.lock().seen[ran..].to_vec();
    let sent = (after.iter()).any(|(m, p)| m == "send" && p["name"] == LANE && p["text"] == "DECISION q1: yes");
    let enter = (after.iter()).any(|(m, p)| m == "keys" && p["name"] == LANE && p["keys"] == json!(["Enter"]));
    assert!(sent && enter, "{after:?}");
    assert_eq!(kinds(&c, "answer-delivered").len(), 1);
}

#[test]
fn a_supersede_close_logged_after_the_new_spawn_does_not_close_it() {
    let c = ctx(&tmpdir("deliver-410"), CONFIG);
    let sh = shed(&c);
    sh.lane("old-36", "idle", "/x");
    spawn(&c, json!({"pane": "old-36"}));
    spawn(&c, json!({}));
    let d = json!({"agent": LANE, "reason": "superseded"});
    append(&c.state, &Event::new(NOW - 50, "sendmeter", Some(36), "lane-closed", d)).unwrap();
    lane_ask(&c);
    let (code, out) = run(&c, &Fake::default(), &["answer", "sendmeter", "q1", "1"]);
    assert_eq!((code, out.as_str()), (0, "sendmeter q1: yes -> sendmeter-impl-36\n"));
    assert!(typed(&sh, LANE, "DECISION q1: yes"), "{:?}", sh.methods());
    assert!(!sh.called("keys", "old-36"), "{:?}", sh.methods());
}

#[test]
fn a_closed_lane_whose_agent_still_runs_in_its_worktree_gets_the_answer() {
    let c = ctx(&tmpdir("deliver-live"), CONFIG);
    let sh = FakeShed::new();
    sh.attach(&c);
    spawn(&c, json!({"worktree": "/wt/issue-36", "pane": "gone-36"}));
    lane_ask(&c);
    append(
        &c.state,
        &Event::new(NOW, "sendmeter", Some(36), "lane-closed", json!({"agent": LANE})),
    )
    .unwrap();
    sh.lane(LANE, "idle", "/wt/issue-36/src");
    let (code, out) = run(&c, &Fake::default(), &["answer", "sendmeter", "q1", "1"]);
    assert_eq!((code, out.as_str()), (0, "sendmeter q1: yes -> sendmeter-impl-36\n"));
    assert!(typed(&sh, LANE, "DECISION q1: yes"), "{:?}", sh.methods());
    assert!(!sh.called("keys", "gone-36"), "{:?}", sh.methods());
    let d = kinds(&c, "answer-delivered-live");
    assert_eq!((d.len(), d[0].str("agent")), (1, Some(LANE)));
    assert!(kinds(&c, "answer-undelivered").is_empty());
}

#[test]
fn an_answer_with_no_lane_alerts_once_from_check() {
    let c = ctx(&tmpdir("deliver-alert"), CONFIG);
    let sh = FakeShed::new();
    sh.attach(&c);
    spawn(&c, json!({"worktree": "/wt/issue-36"}));
    lane_ask(&c);
    append(
        &c.state,
        &Event::new(NOW, "sendmeter", Some(36), "lane-closed", json!({"agent": LANE})),
    )
    .unwrap();
    let r = Fake::default();
    assert_eq!(run(&c, &r, &["answer", "sendmeter", "q1", "1"]).0, 0);
    let u = kinds(&c, "answer-undelivered");
    assert_eq!((u.len(), u[0].str("agent")), (1, None));
    let (_, out) = run(&c, &r, &["check"]);
    assert!(out.contains("answer-undelivered"), "{out}");
    let (_, out) = run(&c, &r, &["check"]);
    assert!(!out.contains("answer-undelivered"), "{out}");
    let u = kinds(&c, "answer-undelivered");
    assert_eq!((u.len(), u[1].str("key")), (2, Some("q1")));
    assert!(sh.sent(LANE).is_empty(), "{:?}", sh.methods());
}
