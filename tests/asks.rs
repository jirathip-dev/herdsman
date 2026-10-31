//! #40: `herdsman ask` / `answer` / `asks`, the needs-you card, the `orch-holding` and `ask-waiting` alerts.
mod common;
use common::*;
use herdsman::events::{append, read, Event};
use herdsman::serve::Server;
use serde_json::json;

const NOW: u64 = 1_000_000;
const ORCH: &str = "sendmeter-orch";

fn kinds(c: &herdsman::Ctx, kind: &str) -> Vec<Event> {
    read(&c.state).unwrap().into_iter().filter(|e| e.kind == kind).collect()
}

/// The orch lane's spawn, so an answer can find its pane.
fn orch_spawn(c: &herdsman::Ctx, ts: u64) {
    append(&c.state, &Event::new(ts, "sendmeter", None, "spawn", json!({"role": "orch", "agent": ORCH, "pane": "w1:p1"}))).unwrap();
}

fn ask(c: &herdsman::Ctx) -> String {
    let args = ["ask", "sendmeter", "36", "Raise the src budget?", "--option", "yes, to 5,200", "--option", "no, trim", "--recommend", "2"];
    let (code, out) = run(c, &Fake::default(), &args);
    assert_eq!(code, 0, "{out}");
    out.trim().to_string()
}

#[test]
fn ask_answer_round_trip_prompts_the_asker_once() {
    let d = tmpdir("ask-answer");
    let c = ctx(&d, CONFIG);
    orch_spawn(&c, NOW - 60);
    let id = ask(&c);
    assert_eq!(id, "q1");
    let a = &kinds(&c, "needs-decision")[0];
    assert_eq!((a.issue, a.str("agent")), (Some(36), Some(ORCH)));
    assert_eq!(a.data["options"], json!(["yes, to 5,200", "no, trim"]));
    assert_eq!(a.data["recommend"], 2);
    let (_, open) = run(&c, &Fake::default(), &["asks", "sendmeter"]);
    assert!(open.contains("q1 #36") && open.contains("Raise the src budget?"), "{open}");

    let r = Fake::default().reply("herdr agent prompt", "").reply("herdr pane send-keys", "");
    let (code, out) = run(&c, &r, &["answer", "sendmeter", "q1", "2", "--by", "conductor"]);
    assert_eq!(code, 0, "{out}");
    assert!(r.called("herdr agent prompt sendmeter-orch DECISION q1: no, trim"), "{:?}", r.calls.borrow());
    assert!(r.index("herdr pane send-keys w1:p1 Enter") > r.index("herdr agent prompt"));
    let dec = &kinds(&c, "decision")[0];
    assert_eq!((dec.str("id"), dec.str("choice"), dec.str("by")), (Some("q1"), Some("no, trim"), Some("conductor")));
    assert_eq!(run(&c, &Fake::default(), &["asks"]).1, "", "answered asks are closed");

    let (code, out) = run(&c, &r, &["answer", "sendmeter", "q1", "free text"]);
    assert_eq!(code, 3);
    assert!(out.contains("REFUSED ALREADY_ANSWERED"), "{out}");
    assert_eq!(r.count("herdr agent prompt"), 1);
    assert!(run(&c, &r, &["answer", "sendmeter", "q9", "1"]).1.contains("REFUSED NO_SUCH_ASK"));
}

#[test]
fn ask_refuses_an_empty_question_or_five_options() {
    let d = tmpdir("ask-bad");
    let c = ctx(&d, CONFIG);
    assert!(run(&c, &Fake::default(), &["ask", "sendmeter", "-", " "]).1.contains("REFUSED BAD_VALUE"));
    let five = ["ask", "sendmeter", "-", "q?", "--option", "a", "--option", "b", "--option", "c", "--option", "d", "--option", "e"];
    assert_eq!(run(&c, &Fake::default(), &five).0, 3);
    assert!(kinds(&c, "needs-decision").is_empty());
}

#[test]
fn notifier_gets_the_question_numbered_options_and_answer_command() {
    let d = tmpdir("ask-notify");
    let sink = d.join("sink.txt");
    let cfg = format!("[[notify]]\ncmd = \"cat >> {}\"\non = [\"needs-decision\"]\n\n{CONFIG}", sink.display());
    let c = ctx(&d, &cfg);
    ask(&c);
    let got = std::fs::read_to_string(&sink).unwrap();
    for w in ["sendmeter #36 needs you", "Raise the src budget?", "1) yes, to 5,200", "2) no, trim (recommended)", "herdsman answer sendmeter q1 <n|\"text\">"] {
        assert!(got.contains(w), "missing {w:?} in {got}");
    }
}

fn check(c: &herdsman::Ctx, r: &Fake) -> String {
    let (code, out) = run(c, r, &["check"]);
    assert_eq!(code, 0, "{out}");
    out
}

#[test]
fn orch_holding_fires_once_and_not_while_an_ask_is_open() {
    let d = tmpdir("orch-holding");
    let mut c = ctx(&d, CONFIG);
    c.now = NOW - 22 * 60;
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "36"]).0, 0);
    c.now = NOW;
    let r = Fake::default().reply("herdr agent list", &agents(&[(ORCH, "idle", "/x", "w1:p1")]));
    let out = check(&c, &r);
    assert!(out.contains("sendmeter orch-holding: #36 startable, orch idle 22m, no ask"), "{out}");
    assert_eq!(check(&c, &r), "", "once per episode");
    assert_eq!(kinds(&c, "orch-holding").len(), 1);

    // An open ask is not a silent hold; past 30 min it re-alerts once as `ask-waiting`.
    let d2 = tmpdir("orch-asked");
    let mut c2 = ctx(&d2, CONFIG);
    c2.now = NOW - 40 * 60;
    assert_eq!(run(&c2, &Fake::default(), &["queue", "sendmeter", "add", "36"]).0, 0);
    ask(&c2);
    c2.now = NOW;
    let out = check(&c2, &r);
    assert!(!out.contains("orch-holding"), "{out}");
    assert!(out.contains("ask-waiting: q1 unanswered for 40m: Raise the src budget?"), "{out}");
    assert_eq!(check(&c2, &r), "");
    assert_eq!(kinds(&c2, "ask-waiting").len(), 1);
}

#[test]
fn needs_you_card_tops_every_tab_and_answer_needs_a_paired_device() {
    let d = tmpdir("ask-card");
    let c = ctx(&d, CONFIG);
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "36"]).0, 0);
    ask(&c);
    let r = Fake::default();
    let s = Server::new(&c, &r, Some("127.0.0.1:7777".into()));
    let page = String::from_utf8_lossy(&s.respond("GET / HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\r\n", NOW)).into_owned();
    let (card, main) = (page.find("<div class=ask>").expect("no needs-you card"), page.find("<main>").unwrap());
    assert!(card < main, "the card sits above every tab section");
    assert!(page[card..].find("value=2>2. no, trim") < page[card..].find("value=1>1. yes"), "recommended first");
    assert!(page.contains("⏸ waiting: decision"));
    let body = "op=answer&fleet=sendmeter&id=q1&choice=2";
    let req = format!("POST / HTTP/1.1\r\nHost: 127.0.0.1:7777\r\nContent-Length: {}\r\n\r\n{body}", body.len());
    let res = String::from_utf8_lossy(&s.respond(&req, NOW)).into_owned();
    assert!(res.starts_with("HTTP/1.1 401"), "{res}");
    assert!(kinds(&c, "decision").is_empty());
}

#[test]
fn answer_without_a_pane_refuses_and_keeps_the_ask_open() {
    let d = tmpdir("ask-nopane");
    let c = ctx(&d, CONFIG);
    ask(&c);
    let r = Fake::default().reply("herdr agent prompt", "").reply("herdr pane send-keys", "");
    let (code, out) = run(&c, &r, &["answer", "sendmeter", "q1", "1"]);
    assert_ne!(code, 0, "{out}");
    assert!(kinds(&c, "decision").is_empty() && !r.called("herdr agent prompt"));
    orch_spawn(&c, NOW);
    assert_eq!(run(&c, &r, &["answer", "sendmeter", "q1", "1"]).0, 0, "the ask is still answerable");
    assert!(r.called("herdr agent prompt sendmeter-orch DECISION q1: yes, to 5,200"));
}

/// POST `body` to `/` (plus extra header lines); the raw response.
fn post(s: &Server, body: &str, extra: &str) -> String {
    let req = format!("POST / HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len());
    String::from_utf8_lossy(&s.respond(&req, NOW)).into_owned()
}

#[test]
fn a_paired_device_answers_from_the_card_and_bad_text_is_refused() {
    let d = tmpdir("ask-web");
    let mut c = ctx(&d, CONFIG);
    c.now = NOW;
    orch_spawn(&c, NOW - 60);
    ask(&c);
    let r = Fake::default().reply("herdr agent prompt", "").reply("herdr pane send-keys", "");
    let s = Server::new(&c, &r, Some("127.0.0.1:7777".into()));
    let cookie = pair(&s, &c, "", NOW);
    let page = String::from_utf8_lossy(&s.respond("GET / HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\r\n", NOW)).into_owned();
    let i = page.find("name=mtime value=").unwrap() + 17;
    let mtime: String = page[i..].chars().take_while(char::is_ascii_digit).collect();
    let long = "x".repeat(2001);
    for bad in ["%20", "a%0Ab", long.as_str()] {
        let res = post(&s, &format!("op=answer&fleet=sendmeter&id=q1&text={bad}&mtime={mtime}"), &cookie);
        assert!(res.starts_with("HTTP/1.1 303") && res.contains("REFUSED%20BAD%5FVALUE"), "{res}");
    }
    assert!(kinds(&c, "decision").is_empty() && !r.called("herdr agent prompt"));
    let res = post(&s, &format!("op=answer&fleet=sendmeter&id=q1&choice=2&mtime={mtime}"), &cookie);
    assert!(res.starts_with("HTTP/1.1 303"), "{res}");
    let dec = &kinds(&c, "decision")[0];
    assert_eq!((dec.str("choice"), dec.str("by"), dec.str("via")), (Some("no, trim"), Some("owner"), Some("web")));
    assert!(r.called("herdr agent prompt sendmeter-orch DECISION q1: no, trim"), "{:?}", r.calls.borrow());
    assert_eq!(run(&c, &Fake::default(), &["asks"]).1, "", "the ask is closed");
}
