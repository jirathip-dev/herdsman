//! AC5: spawn launches the right harness with the synced profile.
mod common;
use common::*;
use herdsman::events::{fold, read};
use serde_json::Value;

fn plan(c: &herdsman::Ctx, r: &Fake, role: &str) -> Value {
    let (code, out) = run(c, r, &["spawn", "sendmeter", "12", role, "--dry-run", "--json"]);
    assert_eq!(code, 0, "{out}");
    serde_json::from_str(&out).unwrap()
}

fn words(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|w| w.as_str().unwrap().to_string()).collect()
}

#[test]
fn claude_role_dry_run_uses_synced_profile() {
    let d = tmpdir("spawn-claude");
    let c = ctx(&d, &CONFIG.replace("harness  =  \"hermes\"", "harness = \"claude\""));
    let r = Fake::default();
    let p = plan(&c, &r, "impl");
    assert_eq!(words(&p["commands"][2])[1..], ["pane", "run", "<pane>", "export", "HERDSMAN_LANE='sendmeter:12:impl'"]);
    let start = words(&p["commands"][3]);
    let prof = c.config.parent().unwrap().join("claude/fleet-impl");
    let (settings, role) = (prof.join("settings.json"), prof.join("role.md"));
    let expect = [
        "herdr", "agent", "start", "sendmeter-impl-12", "--kind", "claude", "--pane", "<pane>", "--timeout", "90000", "--",
        "--dangerously-skip-permissions",
        "--settings", settings.to_str().unwrap(),
        "--append-system-prompt-file", role.to_str().unwrap(),
        "--model", "opus",
    ];
    assert_eq!(&start[..expect.len()], &expect);
    let kick = start.last().unwrap();
    assert!(kick.starts_with("Read ") && kick.ends_with("issue-12/.brief.md and execute it fully."), "{kick}");
    assert_eq!(p["branch"], "issue-12");
    assert_eq!(p["base"], "origin/staging");
    assert!(words(&p["commands"][4]).starts_with(&["herdr".into(), "agent".into(), "wait".into()]));
    assert!(r.calls.borrow().is_empty(), "dry-run calls nothing");
    assert!(read(&c.state).unwrap().is_empty(), "dry-run logs nothing");
}

#[test]
fn hermes_role_dry_run_kicks_prompt_after_boot() {
    let d = tmpdir("spawn-hermes");
    let c = ctx(&d, CONFIG);
    let p = plan(&c, &Fake::default(), "rev");
    assert_eq!(words(&p["commands"][2])[4..], ["export", "HERDSMAN_LANE='sendmeter:12:rev'"]);
    let start = words(&p["commands"][3]);
    assert_eq!(start[start.len() - 4..], ["--".to_string(), "-p".into(), "fleet-rev".into(), "--accept-hooks".into()]);
    assert!(start.contains(&"hermes".to_string()));
    assert!(words(&p["commands"][4]).starts_with(&["herdr".into(), "pane".into(), "wait-output".into()]));
    let kick = words(&p["commands"][5]);
    assert_eq!(kick[..4], ["herdr", "agent", "prompt", "sendmeter-rev-12-r1"].map(String::from));
    assert_eq!(kick[4], p["kick"].as_str().unwrap());
    assert_eq!(p["base"], "origin/issue-12");
}

#[test]
fn real_claude_spawn_writes_brief_and_logs_event() {
    let d = tmpdir("spawn-real");
    let c = ctx(&d, &CONFIG.replace("harness  =  \"hermes\"", "harness = \"claude\""));
    let wt = c.worktrees.join("sendmeter/issue-12");
    std::fs::create_dir_all(&wt).unwrap();
    // Without a synced profile a claude spawn is refused.
    let (code, _) = run(&c, &Fake::default(), &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 1);
    let (code, _) = run(&c, &Fake::default(), &["profiles", "sync"]);
    assert_eq!(code, 0);
    let settings = std::fs::read_to_string(c.config.parent().unwrap().join("claude/fleet-impl/settings.json")).unwrap();
    let s: Value = serde_json::from_str(&settings).unwrap();
    assert_eq!(s["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "'/opt/herdsman/bin/herdsman' hook --role impl");
    let r = Fake::default()
        .reply("herdr agent list", NO_AGENTS)
        .reply(&format!("git -C {} rev-list", wt.display()), "0\n")
        .reply("gh issue view 12", r#"{"title":"Fix the meter","body":"AC: it works"}"#)
        .reply("git -C", "")
        .reply("herdr worktree open", r#"{"result":{"workspace":{"workspace_id":"w9"}}}"#)
        .reply("herdr pane list --workspace w9", r#"{"result":{"panes":[{"pane_id":"w9:p1","agent":null}]}}"#)
        .reply("herdr pane run w9:p1 export HERDSMAN_LANE='sendmeter:12:impl'", "{}")
        .reply("herdr agent start sendmeter-impl-12 --kind claude --pane w9:p1", "{}")
        .reply("herdr agent wait sendmeter-impl-12", "{}");
    let (code, out) = run(&c, &r, &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    // #24: the lane's shell exports HERDSMAN_LANE before the harness starts, arming `herdsman hook`.
    assert!(r.index("herdr pane run w9:p1 export").unwrap() < r.index("herdr agent start").unwrap());
    let brief = std::fs::read_to_string(wt.join(".brief.md")).unwrap();
    assert!(brief.contains("# ROLE: IMPLEMENTER") && brief.contains("AC: it works") && brief.contains("Closes #12"));
    let st = &fold(&read(&c.state).unwrap())[&("sendmeter".to_string(), 12)];
    assert_eq!((st.state.as_str(), st.lane.as_deref()), ("impl", Some("sendmeter-impl-12")));
}

#[test]
fn unconfirmed_hermes_kick_presses_enter_then_exits_4() {
    let d = tmpdir("spawn-unconfirmed");
    let c = ctx(&d, CONFIG);
    let wt = c.worktrees.join("sendmeter/issue-12");
    std::fs::create_dir_all(&wt).unwrap();
    let r = Fake::default()
        .reply("herdr agent list", NO_AGENTS)
        .reply(&format!("git -C {} rev-list", wt.display()), "0\n")
        .reply("gh issue view 12", r#"{"title":"t","body":"b"}"#)
        .reply("git -C", "")
        .reply("herdr worktree open", r#"{"result":{"workspace":{"workspace_id":"w9"}}}"#)
        .reply("herdr pane list", r#"{"result":{"panes":[{"pane_id":"w9:p1","agent":null}]}}"#)
        .reply("herdr pane run", "{}")
        .reply("herdr agent start", "{}")
        .reply("herdr pane send-keys w9:p1 Enter", "{}");
    let (code, _) = run(&c, &r, &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 4);
    assert_eq!(r.count("herdr agent prompt sendmeter-impl-12 Read "), 3, "3 attempts");
    assert_eq!(r.count("herdr pane send-keys w9:p1 Enter"), 3);
    assert_eq!(r.count("sleep 10"), 2, "attempts spaced ≥10 s apart");
    let ev = read(&c.state).unwrap();
    assert_eq!(ev.last().unwrap().data["confirmed"], false);
}

#[test]
fn help_agent_doc_is_short_and_complete() {
    let d = tmpdir("help");
    let c = ctx(&d, CONFIG);
    let (code, out) = run(&c, &Fake::default(), &["help", "--agent"]);
    assert_eq!(code, 0);
    assert!(out.lines().count() <= 150, "{} lines", out.lines().count());
    for w in [
        "herdsman spawn", "herdsman merge", "herdsman verdict", "herdsman set", "herdsman status", "herdsman profiles sync",
        "herdsman check", "herdsman clean", "herdsman adopt", "--force", "--supersede", "--yes",
        "MAIN_TARGET", "HOST_BUSY", "LANE_LIVE", "WORKTREE_DIRTY", "review(n)",
        "--serial", "SERIAL_BUSY", "SERIAL_UNMARKED", "CAP", "design", "conductor-only",
    ] {
        assert!(out.contains(w), "missing {w}");
    }
    assert!(!out.contains("herdsman hook"));
}
