mod common;
use common::*;
use drover::events::{append, read, Event};
use drover::serve::Server;
use serde_json::json;

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

fn lane_spawn(c: &drover::Ctx, ts: u64) {
    append(
        &c.state,
        &Event::new(
            ts,
            "sendmeter",
            Some(36),
            "spawn",
            json!({"role": "impl", "agent": LANE, "pane": LANE}),
        ),
    )
    .unwrap();
}

fn shed(c: &drover::Ctx) -> FakeShed {
    let sh = FakeShed::new();
    sh.attach(c);
    sh.lane(LANE, "idle", "/x");
    sh
}

fn typed(sh: &FakeShed, text: &str) -> bool {
    let seen = sh.lock().seen.clone();
    let send = (seen.iter()).position(|(m, p)| m == "send" && p["name"] == LANE && p["text"] == text);
    let enter = (seen.iter()).rposition(|(m, p)| m == "keys" && p["name"] == LANE && p["keys"] == json!(["Enter"]));
    matches!((send, enter), (Some(s), Some(e)) if s < e)
}

fn calls(sh: &FakeShed) -> (usize, usize, usize) {
    let n = |m: &str| sh.all(m).iter().filter(|p| p["name"] == LANE).count();
    (n("send"), n("keys"), n("wait"))
}

fn from_lane(c: &drover::Ctx) {
    let Ok(log) = drover::events::jsonl(&c.state) else {
        return;
    };
    rewrite_log(
        &c.state,
        &log.replace("\"agent\":null", &format!("\"agent\":\"{LANE}\"")),
    );
}

fn ask(c: &drover::Ctx) -> String {
    let args = [
        "ask",
        "sendmeter",
        "36",
        "Raise the src budget?",
        "--option",
        "yes, to 5,200",
        "--option",
        "no, trim",
        "--recommend",
        "2",
    ];
    let (code, out) = run(c, &Fake::default(), &args);
    assert_eq!(code, 0, "{out}");
    from_lane(c);
    out.trim().to_string()
}

#[test]
fn ask_answer_round_trip_prompts_the_asker_once() {
    let d = tmpdir("ask-answer");
    let c = ctx(&d, CONFIG);
    let sh = shed(&c);
    lane_spawn(&c, NOW - 60);
    let id = ask(&c);
    assert_eq!(id, "q1");
    let a = &kinds(&c, "needs-decision")[0];
    assert_eq!((a.issue, a.str("agent")), (Some(36), Some(LANE)));
    assert_eq!(*a.val("options"), json!(["yes, to 5,200", "no, trim"]));
    assert_eq!(a.val("recommend"), 2);
    let (_, open) = run(&c, &Fake::default(), &["asks", "sendmeter"]);
    assert!(
        open.contains("q1 #36") && open.contains("Raise the src budget?"),
        "{open}"
    );

    let r = Fake::default();
    let (code, out) = run(&c, &r, &["answer", "sendmeter", "q1", "2", "--by", "orchestrator"]);
    assert_eq!(code, 0, "{out}");
    assert!(typed(&sh, "DECISION q1: no, trim"), "{:?}", sh.methods());
    let dec = &kinds(&c, "decision")[0];
    assert_eq!(
        (dec.str("id"), dec.str("choice"), dec.str("by")),
        (Some("q1"), Some("no, trim"), Some("orchestrator"))
    );
    assert_eq!(run(&c, &Fake::default(), &["asks"]).1, "", "answered asks are closed");
    assert!(run(&c, &Fake::default(), &["asks", "--all"])
        .1
        .contains("Raise the src budget? [answered]"));

    let (code, out) = run(&c, &r, &["answer", "sendmeter", "q1", "free text", "--by", "alice"]);
    assert_eq!(code, 3);
    assert!(out.contains("REFUSED ALREADY_ANSWERED"), "{out}");
    assert_eq!(sh.sent(LANE), ["DECISION q1: no, trim"]);
    assert!(run(&c, &r, &["answer", "sendmeter", "q9", "1"])
        .1
        .contains("REFUSED NO_SUCH_ASK"));
}

#[test]
fn an_ask_from_outside_a_lane_names_no_agent() {
    let c = ctx(&tmpdir("ask-nolane"), CONFIG);
    let (code, out) = run(
        &c,
        &Fake::default(),
        &["ask", "sendmeter", "-", "Pause it?", "--option", "yes"],
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(kinds(&c, "needs-decision")[0].str("agent"), None);
    let (code, out) = run(&c, &Fake::default(), &["answer", "sendmeter", "q1", "1"]);
    assert_eq!(
        (code, out.trim()),
        (0, "sendmeter q1: yes recorded; asking agent gone, not prompted")
    );
}

#[test]
fn ask_refuses_an_empty_question_or_five_options() {
    let d = tmpdir("ask-bad");
    let c = ctx(&d, CONFIG);
    assert!(run(&c, &Fake::default(), &["ask", "sendmeter", "-", " "])
        .1
        .contains("REFUSED BAD_VALUE"));
    let five = [
        "ask",
        "sendmeter",
        "-",
        "q?",
        "--option",
        "a",
        "--option",
        "b",
        "--option",
        "c",
        "--option",
        "d",
        "--option",
        "e",
    ];
    assert_eq!(run(&c, &Fake::default(), &five).0, 3);
    assert!(kinds(&c, "needs-decision").is_empty());
}

#[test]
fn notifier_gets_the_question_numbered_options_and_answer_command() {
    let d = tmpdir("ask-notify");
    let sink = d.join("sink.txt");
    let cfg = format!(
        "[[notify]]\ncmd = \"cat >> {}\"\non = [\"needs-decision\"]\n\n{CONFIG}",
        sink.display()
    );
    let c = ctx(&d, &cfg);
    ask(&c);
    let got = std::fs::read_to_string(&sink).unwrap();
    for w in [
        "sendmeter #36 needs you",
        "Raise the src budget?",
        "1) yes, to 5,200",
        "2) no, trim (recommended)",
        "drover answer sendmeter q1 <n|\"text\">",
    ] {
        assert!(got.contains(w), "missing {w:?} in {got}");
    }
}

fn check(c: &drover::Ctx, r: &Fake) -> String {
    let (code, out) = run(c, r, &["check"]);
    assert_eq!(code, 0, "{out}");
    out
}

#[test]
fn an_open_ask_re_alerts_once_as_ask_waiting_after_30_min() {
    let r = Fake::default();
    let d = tmpdir("ask-waiting");
    let mut c = ctx(&d, CONFIG);
    let _sh = shed(&c);
    c.now = NOW - 40 * 60;
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "36"]).0, 0);
    ask(&c);
    c.now = NOW;
    let out = check(&c, &r);
    assert!(
        out.contains("ask-waiting: q1 unanswered for 40m: Raise the src budget?"),
        "{out}"
    );
    assert_eq!(check(&c, &r), "");
    assert_eq!(kinds(&c, "ask-waiting").len(), 1);
}

#[test]
fn needs_you_card_folds_above_the_tabs_and_answer_needs_a_paired_device() {
    let d = tmpdir("ask-card");
    let c = ctx(&d, CONFIG);
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "36"]).0, 0);
    ask(&c);
    let r = Fake::default();
    let s = Server::new(&c, &r, Some("127.0.0.1:7777".into()));
    let page = String::from_utf8_lossy(&s.respond("GET / HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\r\n", NOW)).into_owned();
    let page = with_queue(&page, &s, "127.0.0.1:7777", NOW);
    let (card, main) = (
        page.find("<details class=ask>").expect("no needs-you card"),
        page.find("<main>").unwrap(),
    );
    assert!(card < main, "the card sits above every tab section");
    assert!(
        page[card..].find("value=2>2. no, trim") < page[card..].find("value=1>1. yes"),
        "recommended first"
    );
    assert!(page.contains("⏸ waiting: decision"));
    let body = "op=answer&fleet=sendmeter&id=q1&choice=2";
    let req = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1:7777\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let res = String::from_utf8_lossy(&s.respond(&req, NOW)).into_owned();
    assert!(res.starts_with("HTTP/1.1 401"), "{res}");
    assert!(kinds(&c, "decision").is_empty());
}

#[test]
fn answer_to_a_gone_agent_records_the_decision_and_exits_0() {
    let d = tmpdir("ask-nopane");
    let c = ctx(&d, CONFIG);
    ask(&c);
    let (code, out) = run(&c, &Fake::default(), &["answer", "sendmeter", "q1", "1"]);
    assert_eq!(code, 0, "{out}");
    let want = "sendmeter q1: yes, to 5,200 recorded; asking agent gone, not prompted; the next lane on #36 gets it in its brief";
    assert_eq!(out.trim(), want);
    assert_eq!(kinds(&c, "decision")[0].str("choice"), Some("yes, to 5,200"));

    let d = tmpdir("ask-gone");
    let c = ctx(&d, CONFIG);
    let sh = FakeShed::new();
    sh.attach(&c);
    lane_spawn(&c, NOW - 60);
    ask(&c);
    let (code, out) = run(&c, &Fake::default(), &["answer", "sendmeter", "q1", "2"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("recorded; asking agent gone, not prompted"), "{out}");
    assert_eq!(kinds(&c, "decision").len(), 1);
    assert!(sh.called("get", LANE) && !sh.has(LANE), "{:?}", sh.methods());
    assert!(kinds(&c, "answer-delivered").is_empty());
}

fn closed_by(then: Event) -> (usize, String) {
    let d = tmpdir(&format!("ask-closed-{}", then.kind));
    let c = ctx(&d, CONFIG);
    let impl36 = json!({"role": "impl", "agent": "sendmeter-impl-36", "pane": "w1:p2"});
    append(&c.state, &Event::new(NOW - 90, "sendmeter", Some(36), "spawn", impl36)).unwrap();
    let q = json!({"id": "q1", "question": "Split the PR?", "options": ["yes", "no"], "agent": "sendmeter-impl-36"});
    append(
        &c.state,
        &Event::new(NOW - 60, "sendmeter", Some(36), "needs-decision", q),
    )
    .unwrap();
    append(&c.state, &then).unwrap();
    let events = read(&c.state).unwrap();
    (
        drover::events::open_asks(&events).len(),
        run(&c, &Fake::default(), &["asks", "--all"]).1,
    )
}

#[test]
fn an_ask_closes_when_its_issue_merges_or_its_lane_goes() {
    let ev = |kind: &str, issue: u64, d| Event::new(NOW, "sendmeter", Some(issue), kind, d);
    let respawn = json!({"role": "impl", "agent": "sendmeter-impl-36", "pane": "w1:p3"});
    let killed = json!({"role": "impl", "agent": "sendmeter-impl-36"});
    for (then, why) in [
        (ev("merged", 36, json!({"pr": "52"})), "[closed: merged]"),
        (
            ev("clean", 36, json!({"worktree": "/wt/36", "pane": "w1:p2"})),
            "[closed: lane gone]",
        ),
        (ev("lane-killed", 36, killed), "[closed: lane gone]"),
        (ev("spawn", 36, respawn), "[closed: lane gone]"),
    ] {
        let kind = then.kind.clone();
        let (open, all) = closed_by(then);
        assert_eq!(open, 0, "{kind} closes the ask");
        assert!(all.contains("q1 #36") && all.trim_end().ends_with(why), "{kind}: {all}");
    }
    assert_eq!(closed_by(ev("merged", 37, json!({}))).0, 1);
    assert_eq!(closed_by(ev("clean", 37, json!({}))).0, 1);
}

fn post(s: &Server, body: &str, extra: &str) -> String {
    let req = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n{extra}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    String::from_utf8_lossy(&s.respond(&req, NOW)).into_owned()
}

#[test]
fn a_paired_device_answers_from_the_card_and_bad_text_is_refused() {
    let d = tmpdir("ask-web");
    let mut c = ctx(&d, CONFIG);
    c.now = NOW;
    let sh = shed(&c);
    lane_spawn(&c, NOW - 60);
    ask(&c);
    let f = Fake::default();
    let r = sh.on(&f);
    let s = Server::new(&c, &r, Some("127.0.0.1:7777".into()));
    let cookie = pair(&s, &c, "", NOW);
    let page = String::from_utf8_lossy(&s.respond("GET / HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\r\n", NOW)).into_owned();
    let page = with_queue(&page, &s, "127.0.0.1:7777", NOW);
    let i = page.find("name=mtime value=").unwrap() + 17;
    let mtime: String = page[i..].chars().take_while(char::is_ascii_digit).collect();
    let long = "x".repeat(2001);
    for bad in ["%20", "a%0Ab", long.as_str()] {
        let res = post(
            &s,
            &format!("op=answer&fleet=sendmeter&id=q1&text={bad}&mtime={mtime}"),
            &cookie,
        );
        assert!(
            res.starts_with("HTTP/1.1 303") && res.contains("REFUSED%20BAD%5FVALUE"),
            "{res}"
        );
    }
    assert!(kinds(&c, "decision").is_empty() && sh.sent(LANE).is_empty());
    let res = post(
        &s,
        &format!("op=answer&fleet=sendmeter&id=q1&choice=2&mtime={mtime}"),
        &cookie,
    );
    assert!(res.starts_with("HTTP/1.1 303"), "{res}");
    let dec = &kinds(&c, "decision")[0];
    assert_eq!(
        (dec.str("choice"), dec.str("by"), dec.str("via")),
        (Some("no, trim"), Some("owner"), Some("web"))
    );
    assert!(typed(&sh, "DECISION q1: no, trim"), "{:?}", sh.methods());
    assert!(f.called("sh -c nohup '/opt/drover/bin/drover' kick sendmeter 36 --why board --now"));
    assert_eq!(run(&c, &Fake::default(), &["asks"]).1, "", "the ask is closed");
}

fn routine(c: &drover::Ctx, q: &str) -> (i32, String) {
    let args = [
        "ask",
        "sendmeter",
        "36",
        q,
        "--option",
        "split",
        "--option",
        "keep",
        "--recommend",
        "2",
        "--kind",
        "routine",
    ];
    let res = run(c, &Fake::default(), &args);
    from_lane(c);
    res
}

#[test]
fn routine_needs_a_recommendation_and_no_owner_topic() {
    let d = tmpdir("ask-routine-bad");
    let c = ctx(&d, CONFIG);
    let (code, out) = run(
        &c,
        &Fake::default(),
        &["ask", "sendmeter", "36", "Split it?", "--kind", "routine"],
    );
    assert_eq!(code, 3);
    assert!(out.contains("REFUSED NO_RECOMMENDATION"), "{out}");
    for q in [
        "Rotate the token first?",
        "Push to main?",
        "Worth $40 a month?",
        "Add a dependency?",
        "Store the secrets in the repo?",
        "Drop the deleted rows?",
        "Pull in two dependencies?",
    ] {
        let (code, out) = routine(&c, q);
        assert_eq!(code, 3, "{q}");
        assert!(out.contains("REFUSED OWNER_TOPIC"), "{q}: {out}");
    }
    assert!(kinds(&c, "needs-decision").is_empty());
    assert_eq!(
        routine(&c, "Split the snapshot tests per pane?").0,
        0,
        "maintain/mainly are not `main`"
    );
    assert_eq!(kinds(&c, "needs-decision")[0].str("kind"), Some("routine"));
    ask(&c);
    assert_eq!(
        kinds(&c, "needs-decision")[1].str("kind"),
        Some("owner"),
        "unmarked asks are owner asks"
    );
}

#[test]
fn a_routine_ask_defaults_at_30_min_and_an_owner_ask_never_does() {
    let d = tmpdir("ask-default");
    let mut c = ctx(&d, CONFIG);
    let sh = shed(&c);
    lane_spawn(&c, NOW - 3600);
    c.now = NOW - 40 * 60;
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "36"]).0, 0);
    ask(&c);
    c.now = NOW - 30 * 60;
    assert_eq!(routine(&c, "Split the snapshot tests per pane?").0, 0);
    let r = Fake::default();
    c.now = NOW - 60;
    let page = |c: &drover::Ctx| {
        let r = sh.on(&r);
        let s = Server::new(c, &r, Some("127.0.0.1:7777".into()));
        let page =
            String::from_utf8_lossy(&s.respond("GET / HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\r\n", c.now)).into_owned();
        with_queue(&page, &s, "127.0.0.1:7777", c.now)
    };
    let p = page(&c);
    assert!(
        p.contains("routine · defaults to 2 in 60s") && p.contains("owner · waits for you"),
        "{p}"
    );
    check(&c, &r);
    assert!(kinds(&c, "decision").is_empty(), "29 min: no default yet");
    c.now = NOW;
    check(&c, &r);
    let dec = kinds(&c, "decision");
    assert_eq!(dec.len(), 1, "only the routine ask defaults");
    assert_eq!(
        (dec[0].str("id"), dec[0].str("choice"), dec[0].str("by")),
        (Some("q2"), Some("keep"), Some("default"))
    );
    assert!(
        typed(&sh, "DECISION q2: keep (default after 30 min, nobody replied)"),
        "{:?}",
        sh.sent(LANE)
    );
    c.now = NOW + 3 * 3600;
    check(&c, &r);
    assert_eq!(kinds(&c, "decision").len(), 1, "the owner ask still waits");
    assert!(page(&c).contains("decided by default: keep"));

    let (code, out) = run(&c, &r, &["answer", "sendmeter", "q2", "1"]);
    assert_eq!(code, 0, "{out}");
    let dec = kinds(&c, "decision");
    assert_eq!(
        (dec.len(), dec[1].str("choice"), dec[1].str("by")),
        (2, Some("split"), Some("owner"))
    );
    assert!(typed(&sh, "DECISION q2: split"), "{:?}", sh.sent(LANE));
    assert!(!page(&c).contains("decided by default"));
    assert!(run(&c, &r, &["answer", "sendmeter", "q2", "2"])
        .1
        .contains("REFUSED ALREADY_ANSWERED"));
}

#[test]
fn a_manual_answer_before_30_min_prevents_the_default() {
    let d = tmpdir("ask-default-manual");
    let mut c = ctx(&d, CONFIG);
    let sh = shed(&c);
    lane_spawn(&c, NOW - 3600);
    c.now = NOW - 40 * 60;
    assert_eq!(routine(&c, "Split the snapshot tests per pane?").0, 0);
    let r = Fake::default();
    c.now = NOW - 20 * 60;
    assert_eq!(run(&c, &r, &["answer", "sendmeter", "q1", "1"]).0, 0);
    c.now = NOW;
    check(&c, &r);
    let dec = kinds(&c, "decision");
    assert_eq!((dec.len(), dec[0].str("by")), (1, Some("owner")));
    assert_eq!(sh.sent(LANE).len(), 1, "{:?}", sh.sent(LANE));
}

#[test]
fn an_answer_the_agent_never_picks_up_is_retried_once_then_pushed() {
    let d = tmpdir("ask-landed");
    let c = ctx(&d, CONFIG);
    let sh = shed(&c);
    lane_spawn(&c, NOW - 60);
    ask(&c);
    assert_eq!(run(&c, &Fake::default(), &["answer", "sendmeter", "q1", "2"]).0, 0);
    assert_eq!(calls(&sh), (1, 1, 1), "working at once: nothing more");
    assert_eq!(sh.last("wait")["until"], json!(["working", "blocked"]));
    assert!(kinds(&c, "answer-undelivered").is_empty());
    assert_eq!(kinds(&c, "answer-delivered")[0].str("agent"), Some(LANE));

    let d = tmpdir("ask-undelivered");
    let c = ctx(&d, CONFIG);
    let sh = shed(&c);
    sh.wait_ends(LANE, "timeout");
    lane_spawn(&c, NOW - 60);
    ask(&c);
    assert_eq!(run(&c, &Fake::default(), &["answer", "sendmeter", "q1", "2"]).0, 0);
    assert_eq!(calls(&sh), (2, 2, 2), "one retry");
    let u = kinds(&c, "answer-undelivered");
    assert_eq!(
        (u.len(), u[0].str("id"), u[0].str("agent"), u[0].str("text")),
        (1, Some("q1"), Some(LANE), Some("DECISION q1: no, trim"))
    );
    assert!(drover::config::WEBPUSH_ON.contains(&"answer-undelivered"), "pushed");
    assert!(kinds(&c, "answer-delivered").is_empty());
}

#[test]
fn ask_waiting_pushes_are_capped_per_pass() {
    let r = Fake::default();
    let d = tmpdir("ask-cap");
    let sink = d.join("pushed.txt");
    let config = format!(
        "{CONFIG}\n[[notify]]\ncmd = \"cat >> '{}'\"\non = [\"ask-waiting\"]\n",
        sink.display()
    );
    let mut c = ctx(&d, &config);
    let _sh = shed(&c);
    c.now = NOW;
    for n in 1..=11 {
        let mut a = json!({"id": format!("q{n}"), "question": format!("Q{n}?"), "options": ["a"]});
        if n == 11 {
            a["from"] = json!("air:q99");
        }
        append(
            &c.state,
            &Event::new(NOW - 40 * 60, "sendmeter", Some(n), "needs-decision", a),
        )
        .unwrap();
    }
    check(&c, &r);
    let pushed = std::fs::read_to_string(&sink).unwrap();
    assert_eq!(pushed.lines().count(), 4, "{pushed}");
    assert!(
        pushed
            .lines()
            .last()
            .unwrap()
            .contains("10 questions waiting — open the board"),
        "{pushed}"
    );
    assert!(!pushed.contains("q11"), "{pushed}");
    assert_eq!(kinds(&c, "ask-waiting").len(), 11, "10 alerts logged + the summary");
    check(&c, &r);
    assert_eq!(
        std::fs::read_to_string(&sink).unwrap().lines().count(),
        4,
        "nothing again"
    );
}

#[test]
fn an_ask_made_during_its_lanes_startup_stays_open_answerable_and_alerts_once() {
    let d = tmpdir("ask-startup");
    let mut c = ctx(&d, CONFIG);
    let sh = shed(&c);
    c.now = NOW - 40 * 60;
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "36"]).0, 0);
    let ev = |kind: &str, ts, d| Event::new(ts, "sendmeter", Some(36), kind, d);
    let lane = |pane: &str| json!({"role": "impl", "agent": LANE, "pane": pane});
    let start = c.now - 75;
    for e in [
        ev("spawn", start - 3600, lane("old-36")),
        ev(
            "supersede",
            start,
            json!({"agent": LANE, "killed": [LANE], "role": "impl"}),
        ),
        ev("spawn-started", start, lane(LANE)),
    ] {
        append(&c.state, &e).unwrap();
    }
    ask(&c);
    append(&c.state, &ev("spawn", start, lane(LANE))).unwrap();
    c.now = NOW;
    let (_, open) = run(&c, &Fake::default(), &["asks", "sendmeter"]);
    assert!(open.contains("q1 #36"), "{open}");

    let r = Fake::default();
    let out = check(&c, &r);
    assert!(out.contains("ask-waiting: q1 unanswered for 40m"), "{out}");
    assert_eq!(check(&c, &r), "");
    assert_eq!(kinds(&c, "ask-waiting").len(), 1);

    let (code, out) = run(&c, &r, &["answer", "sendmeter", "q1", "1"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(&format!("-> {LANE}")), "{out}");
    assert!(typed(&sh, "DECISION q1: yes, to 5,200"), "{:?}", sh.methods());
    assert!(!sh.called("keys", "old-36"), "{:?}", sh.methods());
}

#[test]
fn asks_json_says_why_an_ask_closed() {
    let killed = Event::new(NOW, "sendmeter", Some(36), "lane-killed", json!({"agent": LANE}));
    let d = tmpdir("ask-closed-json");
    let c = ctx(&d, CONFIG);
    lane_spawn(&c, NOW - 90);
    ask(&c);
    let (_, open) = run(&c, &Fake::default(), &["asks", "--json"]);
    let open: serde_json::Value = serde_json::from_str(&open).unwrap();
    assert_eq!((&open[0]["id"], &open[0]["closed"]), (&json!("q1"), &json!(null)));
    append(&c.state, &killed).unwrap();
    let (_, all) = run(&c, &Fake::default(), &["asks", "--all", "--json"]);
    let all: serde_json::Value = serde_json::from_str(&all).unwrap();
    assert_eq!(all[0]["closed"], "closed: lane gone", "{all}");
}
