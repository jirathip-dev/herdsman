//! #14: per-fleet notifiers replace the global ones; `notify_global` sends to both.
mod common;
use common::*;
use herdsman::config::Config;
use herdsman::events::{emit, Event};
use std::path::Path;

fn sink(cmd: &Path) -> String {
    format!("cmd = \"cat >> '{}'\"\non = [\"merged\"]\n", cmd.display())
}

/// Fleet `a` has its own notifier (optionally `notify_global`), fleet `b` none; plus a global one.
fn setup(name: &str, global_too: bool) -> (std::path::PathBuf, Config) {
    let d = tmpdir(name);
    let fleet = |f: &str| format!("[fleet.{f}]\nrepo = \"o/{f}\"\nlocal = \"/tmp/{f}\"\nintegration = \"staging\"\n");
    let text = format!(
        "{}{}\n[[fleet.a.notify]]\n{}\n[[notify]]\n{}",
        fleet("a") + if global_too { "notify_global = true\n" } else { "" },
        fleet("b"),
        sink(&d.join("a.txt")),
        sink(&d.join("global.txt")),
    );
    (d, Config::parse(&text).unwrap())
}

fn merged(d: &Path, cfg: &Config, fleet: &str) {
    let ev = Event::new(1, fleet, Some(7), "merged", serde_json::json!({"into": "staging", "merge_sha": "abc", "pr": 9}));
    emit(&d.join("events.jsonl"), cfg, ev).unwrap();
}

fn got(d: &Path, f: &str) -> String {
    std::fs::read_to_string(d.join(f)).unwrap_or_default()
}

#[test]
fn fleet_notifier_replaces_global() {
    let (d, cfg) = setup("fn-a", false);
    merged(&d, &cfg, "a");
    assert!(got(&d, "a.txt").contains("a #7 merged"), "{}", got(&d, "a.txt"));
    assert_eq!(got(&d, "global.txt"), "");
}

#[test]
fn fleet_without_notifier_uses_global() {
    let (d, cfg) = setup("fn-b", false);
    merged(&d, &cfg, "b");
    assert_eq!(got(&d, "a.txt"), "");
    assert!(got(&d, "global.txt").contains("b #7 merged"));
}

#[test]
fn notify_global_sends_to_both() {
    let (d, cfg) = setup("fn-both", true);
    merged(&d, &cfg, "a");
    assert_eq!(got(&d, "a.txt").lines().count(), 1);
    assert_eq!(got(&d, "global.txt").lines().count(), 1);
}

#[test]
fn fleetless_event_goes_to_global_only() {
    let (d, cfg) = setup("fn-none", false);
    merged(&d, &cfg, "");
    assert_eq!(got(&d, "a.txt"), "");
    assert_eq!(got(&d, "global.txt").lines().count(), 1);
}

#[test]
fn empty_fleet_notify_cmd_is_rejected() {
    let text = format!("{CONFIG}\n[[fleet.sendmeter.notify]]\ncmd = \" \"\non = [\"merged\"]\n");
    let e = Config::parse(&text).unwrap_err();
    assert!(format!("{e:#}").contains("fleet.sendmeter.notify: cmd must not be empty"), "{e:#}");
}
