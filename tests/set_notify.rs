//! AC2 (set round-trip, byte-identical) and AC7 (notifier).
mod common;
use common::*;
use herdsman::events::read;

#[test]
fn set_round_trip_is_byte_identical() {
    let d = tmpdir("set");
    let c = ctx(&d, CONFIG);
    let r = Fake::default();
    let (code, out) = run(&c, &r, &["set", "sendmeter", "impl", "--harness", "claude"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("sendmeter.impl harness: hermes -> claude"), "{out}");
    let mid = std::fs::read_to_string(&c.config).unwrap();
    assert!(mid.contains("harness  =  \"claude\"    # toggled in tests"), "{mid}");
    assert_eq!(herdsman::config::Config::parse(&mid).unwrap().fleet["sendmeter"].role["impl"].harness, "claude");

    let (code, out) = run(&c, &r, &["set", "sendmeter", "impl", "--harness", "hermes"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("claude -> hermes"), "{out}");
    assert_eq!(std::fs::read(&c.config).unwrap(), CONFIG.as_bytes());
    let ev = read(&c.state).unwrap();
    assert_eq!(ev.iter().filter(|e| e.kind == "set").count(), 2);
    assert!(r.calls.borrow().is_empty(), "set must not call external tools");
}

#[test]
fn set_unknown_role_table_fails_without_writing() {
    let d = tmpdir("set-missing");
    let text = CONFIG.replace("[fleet.sendmeter.role.rev]\nharness = \"hermes\"\nprofile = \"fleet-rev\"\n", "");
    let c = ctx(&d, &text);
    let (code, _) = run(&c, &Fake::default(), &["set", "sendmeter", "rev", "--harness", "claude"]);
    assert_eq!(code, 1);
    assert_eq!(std::fs::read_to_string(&c.config).unwrap(), text);
}

#[test]
fn set_inserts_missing_key_in_place() {
    let d = tmpdir("set-insert");
    let c = ctx(&d, &format!("models = [\"glm\"]\n{CONFIG}"));
    let (code, out) = run(&c, &Fake::default(), &["set", "sendmeter", "rev", "--model", "glm"]);
    assert_eq!(code, 0, "{out}");
    let t = std::fs::read_to_string(&c.config).unwrap();
    assert!(t.contains("profile = \"fleet-rev\"\nmodel = \"glm\"\n\n[limits]"), "{t}");
}

fn notify_cfg(cmd: &str, on: &str) -> String {
    format!("{CONFIG}\n[[notify]]\ncmd = {cmd:?}\non = [{on:?}]\n")
}

#[test]
fn notifier_gets_event_json_on_stdin() {
    let d = tmpdir("notify");
    let sink = d.join("sink.json");
    let cfg = notify_cfg(&format!("cat > '{}'", sink.display()), "set") + "format = \"json\"\n";
    let c = ctx(&d, &cfg);
    let (code, _) = run(&c, &Fake::default(), &["set", "sendmeter", "impl", "--harness", "claude"]);
    assert_eq!(code, 0);
    let got: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&sink).unwrap()).unwrap();
    assert_eq!(got["type"], "set");
    assert_eq!(got["fleet"], "sendmeter");
    assert_eq!(got["new"], "claude");
}

#[test]
fn notifier_skips_unlisted_types() {
    let d = tmpdir("notify-skip");
    let sink = d.join("sink.json");
    let c = ctx(&d, &notify_cfg(&format!("cat > '{}'", sink.display()), "merged"));
    let (code, _) = run(&c, &Fake::default(), &["set", "sendmeter", "impl", "--harness", "claude"]);
    assert_eq!(code, 0);
    assert!(!sink.exists());
}

#[test]
fn failing_notifier_does_not_change_exit_code() {
    let d = tmpdir("notify-fail");
    let c = ctx(&d, &notify_cfg("exit 7", "set"));
    let (code, _) = run(&c, &Fake::default(), &["set", "sendmeter", "impl", "--harness", "claude"]);
    assert_eq!(code, 0);
    let ev = read(&c.state).unwrap();
    assert!(ev.iter().any(|e| e.kind == "notify-failed"), "failure is logged");
}
