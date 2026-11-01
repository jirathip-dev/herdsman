//! #22: the dsh harness — pinned dsh-tui launch, settle on herdr's `dsh-tui` label,
//! and the git pre-push hook that stands in for a pre-tool hook.
mod common;
use common::*;
use herdsman::events::read;
use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn dsh_config() -> String {
    CONFIG.replace(
        "[fleet.sendmeter.role.rev]\nharness = \"hermes\"",
        "[fleet.sendmeter.role.rev]\nharness = \"dsh\"",
    )
}

fn pane(agent: &str, status: &str) -> String {
    let agent = if agent.is_empty() { Value::Null } else { agent.into() };
    serde_json::json!({"result": {"pane": {"pane_id": "w9:p1", "agent": agent, "agent_status": status}}}).to_string()
}

/// A dsh rev spawn up to the kick; `states` are the successive `herdr pane get` answers.
fn spawn_rev(name: &str, states: &[(&str, &str)]) -> (herdsman::Ctx, Fake, i32, String) {
    spawn_rev_with(name, states, "  ⌸ ❯\n")
}

/// As `spawn_rev`, with `screen` as the lane's visible screen once idle.
fn spawn_rev_with(name: &str, states: &[(&str, &str)], screen: &str) -> (herdsman::Ctx, Fake, i32, String) {
    let d = tmpdir(name);
    let c = ctx(&d, &dsh_config());
    let wt = c.worktrees.join("sendmeter/rev-12-r1");
    let gd = d.join("gitdir");
    std::fs::create_dir_all(&wt).unwrap();
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    let mut r = Fake::default()
        .reply(
            "dsh-tui version",
            "@deepseek-harness-tui/dsh-tui 0.17.0 (launcher)\nprofile: 0.17.0  /x\n",
        )
        .reply("herdr agent list", NO_AGENTS)
        .reply(&format!("git -C {} rev-list", wt.display()), "0\n")
        .reply(
            &format!("git -C {} rev-parse --absolute-git-dir", wt.display()),
            &format!("{}\n", gd.display()),
        )
        .reply("gh issue view 12", r#"{"title":"t","body":"b"}"#)
        .reply("git -C", "")
        .reply(
            "herdr worktree open",
            r#"{"result":{"workspace":{"workspace_id":"w9"}}}"#,
        )
        .reply(
            "herdr pane list",
            r#"{"result":{"panes":[{"pane_id":"w9:p1","agent":null}]}}"#,
        )
        .reply("herdr pane run", "{}")
        .reply("herdr pane read w9:p1 --source visible", screen)
        .reply("herdr pane send-keys w9:p1 esc", "{}")
        .reply("herdr agent rename w9:p1 sendmeter-rev-12-r1", "{}")
        .reply("herdr pane send-text w9:p1 Read ", "{}")
        .reply("herdr pane send-keys w9:p1 enter", "{}");
    for (a, s) in states {
        r = r.once("herdr pane get w9:p1", 0, &pane(a, s));
    }
    let (code, out) = run(&c, &r, &["spawn", "sendmeter", "12", "rev"]);
    (c, r, code, out)
}

#[test]
fn dry_run_launches_pinned_dsh_tui_with_role_patch() {
    let d = tmpdir("dsh-plan");
    let c = ctx(&d, &dsh_config());
    let (code, out) = run(
        &c,
        &Fake::default(),
        &["spawn", "sendmeter", "12", "rev", "--dry-run", "--json"],
    );
    assert_eq!(code, 0, "{out}");
    let p: Value = serde_json::from_str(&out).unwrap();
    let cmd = |i: usize| -> Vec<String> {
        p["commands"][i]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_str().unwrap().to_string())
            .collect()
    };
    let patch = c.config.parent().unwrap().join("dsh/fleet-rev.patch.yml");
    assert_eq!(
        cmd(3),
        [
            "herdr",
            "pane",
            "run",
            "<pane>",
            "dsh-tui",
            "--patch",
            &format!("'{}'", patch.display())
        ]
    );
    assert_eq!(cmd(4), ["herdr", "pane", "get", "<pane>"]);
    assert_eq!(cmd(5), ["herdr", "pane", "read", "<pane>", "--source", "visible"]);
    assert_eq!(cmd(6), ["herdr", "pane", "send-keys", "<pane>", "esc"]);
    assert_eq!(cmd(7), ["herdr", "agent", "rename", "<pane>", "sendmeter-rev-12-r1"]);
    assert_eq!(cmd(8)[..4], ["herdr", "pane", "send-text", "<pane>"].map(String::from));
}

#[test]
fn sync_writes_per_role_dsh_patch() {
    let d = tmpdir("dsh-sync");
    let c = ctx(&d, &dsh_config());
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    let dir = c.config.parent().unwrap().join("dsh");
    let rev = std::fs::read_to_string(dir.join("fleet-rev.patch.yml")).unwrap();
    assert!(
        rev.contains("- id: sandbox-policy\n  config:\n    mode: workspace-write\n"),
        "{rev}"
    );
    assert!(rev.contains("- id: approval\n  config:\n    policy: never\n"), "{rev}");
    let imp = std::fs::read_to_string(dir.join("fleet-impl.patch.yml")).unwrap();
    assert!(imp.contains("mode: danger-full-access"), "{imp}");
}

/// AC3 (Amendment A): the `dsh-tui` label going idle -> working drives settle and confirmation.
#[test]
fn spawn_waits_for_dsh_tui_idle_then_kicks_and_confirms_working() {
    let (c, r, code, out) = spawn_rev(
        "dsh-working",
        &[("", "unknown"), ("dsh-tui", "idle"), ("dsh-tui", "working")],
    );
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    assert_eq!(r.count("herdr pane get w9:p1"), 3, "shell, idle, then working");
    let calls = r.calls.borrow();
    let start = r.index("herdr pane run w9:p1 dsh-tui --patch").unwrap();
    let idle = calls
        .iter()
        .enumerate()
        .filter(|(_, c)| c.starts_with("herdr pane get"))
        .nth(1)
        .unwrap()
        .0;
    let (rename, prompt) = (
        r.index("herdr agent rename").unwrap(),
        r.index("herdr pane send-text").unwrap(),
    );
    assert!(start < idle && idle < rename && rename < prompt, "{calls:?}");
    assert!(calls[start - 1].starts_with("herdr pane run w9:p1 export HERDSMAN_LANE='sendmeter:12:rev'"));
    // The pre-push hook is installed in this worktree only, before the harness starts.
    let wt_cfg = calls
        .iter()
        .position(|c| c.contains("config --worktree core.hooksPath"))
        .unwrap();
    assert!(wt_cfg < start);
    let root = c.state.parent().unwrap().parent().unwrap();
    let hook = std::fs::read_to_string(root.join("gitdir/fleet-hooks/pre-push")).unwrap();
    assert_eq!(
        hook,
        "#!/bin/sh\nexec '/opt/herdsman/bin/herdsman' hook --role rev --git-pre-push\n"
    );
    let ev = read(&c.state).unwrap();
    let last = ev.last().unwrap();
    assert_eq!(
        (last.kind.as_str(), &last.data["harness"], &last.data["confirmed"]),
        ("spawn", &"dsh".into(), &true.into())
    );
}

#[test]
fn quick_turn_that_lands_on_done_confirms() {
    let (_, r, code, out) = spawn_rev(
        "dsh-done",
        &[("dsh-tui", "idle"), ("dsh-tui", "idle"), ("dsh-tui", "done")],
    );
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    // Still idle right after the prompt: polled again (1 s apart) until done.
    assert_eq!(r.count("sleep 1"), 1);
}

#[test]
fn never_idle_sends_no_prompt_and_exits_4() {
    let (c, r, code, _) = spawn_rev("dsh-never", &[]);
    assert_eq!(code, 4);
    assert!(!r.called("herdr pane send-text"));
    assert_eq!(r.count("herdr pane get w9:p1"), 90);
    assert_eq!(read(&c.state).unwrap().last().unwrap().data["confirmed"], false);
}

/// Live pilot finding: on a machine that never finished it, dsh-TUI opens a first-run
/// wizard that reports idle but swallows input; Esc skips it before the prompt.
#[test]
fn first_run_wizard_is_skipped_before_the_prompt() {
    let wizard =
        "  Welcome to dsh-TUI  Step 1 of 4\n    ←/→ step · Enter act on this step / go next · Esc skip the guide\n";
    let (_, r, code, out) = spawn_rev_with(
        "dsh-wizard",
        &[("dsh-tui", "idle"), ("dsh-tui", "idle"), ("dsh-tui", "working")],
        wizard,
    );
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    let esc = r.index("herdr pane send-keys w9:p1 esc").expect("esc sent");
    assert!(esc < r.index("herdr pane send-text").unwrap());
    let (_, r, code, _) = spawn_rev("dsh-no-wizard", &[("dsh-tui", "idle"), ("dsh-tui", "working")]);
    assert_eq!(code, 0);
    assert!(!r.called("herdr pane send-keys w9:p1 esc"), "no wizard, no Esc");
}

/// Live pilot finding: a prompt sent while dsh-TUI redraws is lost. Retry 5 s later;
/// a kick left typed in the input box only gets Enter, never a second copy.
#[test]
fn unconfirmed_prompt_is_retried_and_a_typed_kick_gets_enter() {
    let mut states = vec![("dsh-tui", "idle")];
    states.extend(std::iter::repeat_n(("dsh-tui", "idle"), 10));
    states.push(("dsh-tui", "working"));
    let typed = format!("  ⌸ ❯ Read {}/fleet-test-dsh-retry\n", std::env::temp_dir().display());
    let (_, r, code, out) = spawn_rev_with("dsh-retry", &states, &typed);
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    assert_eq!(r.count("herdr pane send-text w9:p1"), 1, "typed kick is not sent twice");
    assert_eq!(
        r.count("herdr pane send-keys w9:p1 enter"),
        2,
        "Enter after the kick, then once more"
    );
    assert_eq!(r.count("sleep 5"), 1);
}

/// dsh-TUI's own state reports drop the herdr agent name: `herdsman check` still finds
/// the lane by its spawn pane instead of calling it dead.
#[test]
fn check_finds_unnamed_dsh_lane_by_its_pane() {
    let (c, _, code, _) = spawn_rev("dsh-check", &[("dsh-tui", "idle"), ("dsh-tui", "working")]);
    assert_eq!(code, 0);
    let unnamed = r#"{"result":{"agents":[{"agent":"dsh-tui","agent_status":"working","pane_id":"w9:p1"}]}}"#;
    let (code, out) = run(&c, &Fake::default().reply("herdr agent list", unnamed), &["check"]);
    assert_eq!(code, 0);
    assert!(!out.contains("lane-dead"), "{out}");
    let (_, out) = run(&c, &Fake::default().reply("herdr agent list", NO_AGENTS), &["check"]);
    assert!(out.contains("lane-dead"), "a vanished pane is still dead: {out}");
}

#[test]
fn spawn_refuses_unpinned_dsh_tui() {
    let d = tmpdir("dsh-pin");
    let c = ctx(&d, &dsh_config());
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    let r = Fake::default().reply("dsh-tui version", "profile: 0.18.0  /x\n");
    let (code, _) = run(&c, &r, &["spawn", "sendmeter", "12", "rev"]);
    assert_eq!(code, 1);
    assert!(!r.called("herdr"), "{:?}", r.calls.borrow());
}

fn pre_push(c: &herdsman::Ctx, role: &str, lines: &str) -> (i32, String) {
    let mut out = Vec::new();
    let args = ["herdsman", "hook", "--role", role, "--git-pre-push"]
        .map(String::from)
        .to_vec();
    let code = herdsman::main_with(args, c, &Fake::default(), &mut out, &mut lines.as_bytes());
    (code, String::from_utf8(out).unwrap())
}

#[test]
fn pre_push_refuses_protected_branches_and_reviewer_pushes() {
    let d = tmpdir("dsh-prepush");
    let c = ctx(&d, &dsh_config());
    let line = |dst: &str| format!("refs/heads/issue-12 {} {dst} {}\n", "a".repeat(40), "0".repeat(40));
    for b in ["staging", "main", "master"] {
        let (code, out) = pre_push(&c, "impl", &line(&format!("refs/heads/{b}")));
        assert_eq!(code, 1, "{b}");
        assert!(out.contains(&format!("pushing to `{b}` is blocked")), "{out}");
    }
    assert_eq!(pre_push(&c, "impl", &line("refs/heads/issue-12")).0, 0);
    assert_eq!(pre_push(&c, "impl", "").0, 0, "nothing to push");
    let (code, out) = pre_push(&c, "rev", &line("refs/heads/issue-12"));
    assert_eq!(code, 1);
    assert!(out.contains("reviewers are read-only"), "{out}");
}

fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("FLEET_CONFIG", dir.join("none.toml"))
        .env("FLEET_STATE", dir.join("state/events.jsonl"))
        .output()
        .unwrap()
}

/// AC5: a real `git push origin HEAD:staging` from a lane worktree is refused by the
/// installed hook; the repo's other checkouts are untouched.
#[test]
fn installed_hook_refuses_real_push_to_staging_from_lane_worktree_only() {
    let d = tmpdir("dsh-gitpush");
    let (remote, main, lane) = (d.join("remote.git"), d.join("main"), d.join("lane"));
    assert!(Command::new("git")
        .args(["init", "-q", "--bare"])
        .arg(&remote)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["clone", "-q"])
        .arg(&remote)
        .arg(&main)
        .status()
        .unwrap()
        .success());
    let commit = [
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "x",
    ];
    assert!(git(&main, &commit).status.success());
    assert!(git(
        &main,
        &["worktree", "add", "-q", "-b", "issue-12", lane.to_str().unwrap()]
    )
    .status
    .success());
    herdsman::harness::install_pre_push(&herdsman::runner::Real, env!("CARGO_BIN_EXE_herdsman"), &lane, "impl")
        .unwrap();

    let o = git(&lane, &["push", "origin", "HEAD:staging"]);
    assert!(!o.status.success());
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(said.contains("pushing to `staging` is blocked"), "{o:?}");
    assert!(
        git(&remote, &["rev-parse", "--verify", "-q", "refs/heads/staging"])
            .stdout
            .is_empty(),
        "nothing pushed"
    );
    assert!(
        git(&lane, &["push", "-q", "origin", "HEAD:issue-12"]).status.success(),
        "issue branch pushes"
    );
    assert!(
        git(&main, &["push", "-q", "origin", "HEAD:staging"]).status.success(),
        "main checkout has no herdsman hook"
    );
}
