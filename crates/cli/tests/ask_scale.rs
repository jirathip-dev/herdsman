//! #632: on a 60,000-event log with 800 asks, the needs and chat views visit each event a bounded number of times
//! (`events::VISITS`: events indexed and (ask, event) pairs checked), never once per ask; one new event updates the
//! cached index alone, and a rotation since the last read reads the log again.
mod common;
use common::*;
use herdsman::events::{append, log, Event, VISITS};
use herdsman::serve::Server;
use serde_json::{json, Value};
use std::sync::atomic::Ordering::Relaxed;

const ADDR: &str = "127.0.0.1:7777";
const NOW: u64 = 1_000_000;
const EVENTS: usize = 60_000;
const ASKS: u64 = 800;
const AGENTS: u64 = 40;

fn needs(s: &Server) -> Vec<String> {
    let v: Value = serde_json::from_str(&api(s, ADDR, "", NOW + 500, "needs")).unwrap();
    v["asks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["ask"]["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn the_needs_and_chat_views_are_linear_in_the_log_and_one_event_updates_the_index() {
    let c = ctx(&tmpdir("ask-scale"), CONFIG);
    std::fs::create_dir_all(c.state.parent().unwrap()).unwrap();
    let ev = |issue: u64, kind: &str, d: Value| {
        let mut e = json!({"ts": NOW, "fleet": "sendmeter", "issue": issue, "type": kind});
        e.as_object_mut().unwrap().extend(d.as_object().unwrap().clone());
        e
    };
    let mut log_rows =
        vec![json!({"ts": NOW, "fleet": "sendmeter", "type": "thread-new", "thread": "t1", "title": "t1"})];
    // Ask j comes from agent j % 40 on its issue, each from a fresh spawn of its agent (closing that agent's earlier
    // ask: lane gone); every even ask is answered. Every 40th rides thread t1 (a chat ask row).
    for j in 0..ASKS {
        let (k, pane) = (j % AGENTS, format!("w1:p{j}"));
        let agent = format!("sendmeter-impl-{k}");
        let lane = json!({"role": "impl", "agent": agent, "pane": pane});
        log_rows.push(ev(k + 1, "spawn-started", lane.clone()));
        log_rows.push(ev(k + 1, "spawn", lane));
        let mut q =
            json!({"id": format!("q{j}"), "question": "go?", "options": ["yes", "no"], "recommend": 1, "agent": agent});
        if j % AGENTS == 0 {
            q["thread"] = json!("t1");
        }
        log_rows.push(ev(k + 1, "needs-decision", q));
        if j % 2 == 0 {
            log_rows.push(ev(
                k + 1,
                "decision",
                json!({"id": format!("q{j}"), "choice": "yes", "by": "owner"}),
            ));
        }
    }
    while log_rows.len() < EVENTS {
        log_rows.push(ev(1, "note", json!({"text": "filler"})));
    }
    let jsonl: String = log_rows.iter().map(|e| format!("{e}\n")).collect();
    std::fs::write(&c.state, jsonl).unwrap();

    let r = Fake::default();
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let before = VISITS.load(Relaxed);
    let open = needs(&s);
    let chat: Value = serde_json::from_str(&api(&s, ADDR, "", NOW + 500, "chat")).unwrap();
    let visits = VISITS.load(Relaxed) - before;
    // The last ask of each agent is open unless answered: the odd ones of the last 40.
    let want: Vec<String> = (ASKS - AGENTS..ASKS)
        .filter(|j| j % 2 == 1)
        .map(|j| format!("q{j}"))
        .collect();
    assert_eq!(open, want);
    assert!(
        chat.to_string().contains("\"open\":false"),
        "the chat tab shows t1's ask rows: {chat}"
    );
    assert!(
        visits <= 2 * EVENTS,
        "{visits} event visits for a {EVENTS}-event log with {ASKS} asks"
    );

    // One new event (q799 answered): only it is read and indexed, and the views see it.
    let d = json!({"id": "q799", "choice": "yes", "by": "owner"});
    append(&c.state, &Event::new(NOW, "sendmeter", Some(AGENTS), "decision", d)).unwrap();
    let before = VISITS.load(Relaxed);
    let l = log(&c.state).unwrap();
    assert!(
        VISITS.load(Relaxed) - before <= 2,
        "the earlier events were indexed again"
    );
    assert_eq!(l.len(), EVENTS + 1);
    assert_eq!(needs(&s), want[..want.len() - 1]);

    // A rotation archived events since the last read: the next new event reads the live log again.
    let db = rusqlite::Connection::open(herdsman::db::file(&c.state)).unwrap();
    db.execute("UPDATE events SET archived = 1", []).unwrap();
    append(&c.state, &Event::new(NOW, "sendmeter", Some(1), "note", json!({}))).unwrap();
    assert_eq!(log(&c.state).unwrap().len(), 1, "the archived events dropped out");
    assert_eq!(needs(&s), Vec::<String>::new());
}
