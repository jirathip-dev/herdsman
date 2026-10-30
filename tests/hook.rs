//! AC4: pipe PreToolUse (Claude) and pre_tool_call (Hermes) JSON into `herdsman hook`.
mod common;
use common::{tmpdir, CONFIG};
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};

/// `herdsman hook` as `role`, inside a fleet lane (`HERDSMAN_LANE` set) or outside one.
fn hook_in(lane: Option<&str>, role: &str, input: Value) -> String {
    hook_env(&[("HERDSMAN_LANE", lane)], role, input)
}

/// `herdsman hook` as `role` with each lane var set (`Some`) or unset (`None`).
fn hook_env(vars: &[(&str, Option<&str>)], role: &str, input: Value) -> String {
    let d = tmpdir("hook");
    std::fs::write(d.join("fleet.toml"), CONFIG).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_herdsman"));
    cmd.env_remove("HERDSMAN_LANE").env_remove("FLEET_LANE");
    for (k, v) in vars {
        if let Some(v) = v {
            cmd.env(k, v);
        }
    }
    let mut child = cmd
        .args(["hook", "--role", role])
        .env("FLEET_CONFIG", d.join("fleet.toml"))
        .env("FLEET_STATE", d.join("state/events.jsonl"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

fn hook_as(role: &str, input: Value) -> String {
    hook_in(Some(&format!("sendmeter:24:{role}")), role, input)
}

/// The same tool call in Claude's shape and in both Hermes shapes (stdin wire
/// `tool_input`, raw `args`). Claude tool names map to Hermes ones.
fn shapes(tool: &str, ti: Value) -> [Value; 3] {
    let htool = match tool {
        "Bash" => "terminal",
        "Edit" => "patch",
        _ => "write_file",
    };
    let mut hti = ti.clone();
    if let Some(f) = ti.get("file_path") {
        hti = json!({"path": f});
    }
    let cwd = std::env::temp_dir();
    [
        json!({"hook_event_name": "PreToolUse", "tool_name": tool, "cwd": cwd, "tool_input": ti}),
        json!({"hook_event_name": "pre_tool_call", "tool_name": htool, "cwd": cwd, "tool_input": hti,
            "profile": "fleet-rev", "session_id": "s", "extra": {}}),
        json!({"tool_name": htool, "args": hti}),
    ]
}

/// Runs the call in every shape as `role`. Some(message) when all deny (each in its
/// own dialect), None when all allow (empty stdout); panics when the shapes disagree.
fn call(role: &str, tool: &str, ti: Value) -> Option<String> {
    let outs: Vec<String> = shapes(tool, ti).into_iter().map(|i| hook_as(role, i)).collect();
    if outs.iter().all(String::is_empty) {
        return None;
    }
    let v: Vec<Value> = outs.iter().map(|o| serde_json::from_str(o).unwrap_or_default()).collect();
    let claude = &v[0]["hookSpecificOutput"];
    assert_eq!(claude["permissionDecision"], "deny", "claude: {outs:?}");
    for h in &v[1..] {
        assert_eq!(h["action"], "block", "hermes: {outs:?}");
        assert_eq!(h["message"], claude["permissionDecisionReason"], "{outs:?}");
    }
    Some(claude["permissionDecisionReason"].as_str().unwrap().to_string())
}

fn bash_as(role: &str, cmd: &str) -> Option<String> {
    call(role, "Bash", json!({"command": cmd}))
}

fn bash(cmd: &str) -> Option<String> {
    bash_as("impl", cmd)
}

/// #24 AC4: the hook is inert outside a fleet lane (shared Hermes profiles serve
/// non-fleet lanes too) and enforces its rules inside one.
#[test]
fn hook_is_inert_unless_fleet_lane_is_set() {
    for cmd in ["gh pr merge 5", "git push origin HEAD:staging"] {
        for input in shapes("Bash", json!({"command": cmd})) {
            assert_eq!(hook_in(None, "impl", input.clone()), "", "{cmd} allowed without HERDSMAN_LANE");
            assert_eq!(hook_in(Some(""), "impl", input.clone()), "", "{cmd} allowed with empty HERDSMAN_LANE");
            assert_ne!(hook_in(Some("fleet:24:impl"), "impl", input), "", "{cmd} blocked in a fleet lane");
        }
    }
    for input in shapes("Bash", json!({"command": "git status"})) {
        assert_eq!(hook_in(Some("fleet:24:impl"), "impl", input), "", "git status allowed in a fleet lane");
    }
}

/// #27: `FLEET_LANE` still arms the hook for one release; `HERDSMAN_LANE` is
/// authoritative when both are set, so an empty `HERDSMAN_LANE` keeps it inert.
#[test]
fn legacy_fleet_lane_arms_the_hook_and_herdsman_lane_wins() {
    for input in shapes("Bash", json!({"command": "gh pr merge 5"})) {
        let arm = |vars: &[(&str, Option<&str>)]| hook_env(vars, "impl", input.clone());
        assert_ne!(arm(&[("FLEET_LANE", Some("fleet:24:impl"))]), "", "FLEET_LANE alone arms");
        assert_ne!(arm(&[("HERDSMAN_LANE", Some("a:1:impl")), ("FLEET_LANE", Some("b:2:rev"))]), "", "both set arms");
        assert_eq!(arm(&[("HERDSMAN_LANE", Some("")), ("FLEET_LANE", Some("fleet:24:impl"))]), "", "empty HERDSMAN_LANE wins");
        assert_eq!(arm(&[("FLEET_LANE", Some(""))]), "", "empty FLEET_LANE is inert");
    }
}

#[test]
fn gh_pr_merge_is_denied() {
    let out = bash("gh pr merge 12").expect("denied");
    assert!(out.contains("herdsman merge"), "deny message points to herdsman merge: {out}");
    assert!(bash("cd /x && gh pr merge 3 --squash --admin").is_some());
    assert!(bash("gh api -X PUT repos/o/r/pulls/3/merge").is_some());
}

#[test]
fn github_comments_are_denied() {
    for cmd in [
        "gh issue comment 5 -b x",
        "gh pr comment 5 -b x",
        "gh api -X POST repos/o/r/issues/5/comments",
        "gh api repos/o/r/issues/5/comments -f body=x",
        "gh api --method=PATCH repos/o/r/issues/comments/9 -f body=y",
        "cd /x && gh pr review 5 --comment -b x",
    ] {
        let out = bash(cmd).unwrap_or_else(|| panic!("{cmd} allowed"));
        assert!(out.contains("never comment on GitHub"), "{cmd}: {out}");
    }
    for cmd in ["gh issue view 5", "gh pr view 5 --json state", "gh issue view 5 --comments", "gh api repos/o/r/issues/5/comments"] {
        assert_eq!(bash(cmd), None, "read-only {cmd} is allowed");
    }
}

#[test]
fn push_to_staging_is_denied() {
    for role in ["impl", "orch", "rev"] {
        assert!(bash_as(role, "git push origin staging").unwrap().contains("herdsman merge"));
        assert!(bash_as(role, "git push origin HEAD:staging").is_some());
    }
    assert!(bash("git push origin HEAD:main").is_some());
    assert!(bash("git push -f origin +feature:refs/heads/staging").is_some());
    assert!(bash("git -C /some/repo push origin main").is_some());
    assert!(bash("git push --all origin").is_some());
}

#[test]
fn hermes_terminal_workdir_resolves_bare_push() {
    // A bare `git push` pushes HEAD of the terminal's workdir.
    let d = tmpdir("hook-wd");
    let git = |a: &[&str]| assert!(Command::new("git").arg("-C").arg(&d).args(a).output().unwrap().status.success());
    git(&["init", "-q", "-b", "staging"]);
    git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"]);
    let input = json!({"hook_event_name": "pre_tool_call", "tool_name": "terminal", "cwd": "/",
        "tool_input": {"command": "git push", "workdir": d}});
    let v: Value = serde_json::from_str(&hook_as("impl", input)).unwrap();
    assert_eq!(v["action"], "block");
}

#[test]
fn delete_refspec_on_protected_branch_is_denied() {
    assert!(bash("git push origin :staging").is_some());
    assert!(bash("git push origin --delete staging").is_some());
    assert!(bash("git push origin :refs/heads/main").is_some());
    assert!(bash("git push -d origin master").is_some());
    assert_eq!(bash("git push origin :issue-12-thing"), None);
    assert_eq!(bash("git push origin --delete rev-12-r1"), None);
}

#[test]
fn push_to_feature_branch_is_allowed() {
    assert_eq!(bash("git push origin issue-12-thing"), None);
    assert_eq!(bash("git push -u origin feature-x && git status"), None);
    assert_eq!(bash("gh pr view 12 --json state"), None);
}

#[test]
fn reviewers_are_read_only() {
    for cmd in ["git commit -am x", "git push origin rev-12-r1", "git -C /x merge main", "git rebase origin/staging", "git reset --hard"] {
        let out = bash_as("rev", cmd).unwrap_or_else(|| panic!("rev {cmd} allowed"));
        assert!(out.contains("read-only"), "{cmd}: {out}");
    }
    for cmd in ["git commit -am x", "git push origin issue-12", "git rebase origin/staging"] {
        assert_eq!(bash_as("impl", cmd), None, "impl may run {cmd}");
    }
    for cmd in ["git status", "git diff origin/staging...HEAD", "cargo test", "gh pr view 5"] {
        assert_eq!(bash_as("rev", cmd), None, "rev may run {cmd}");
    }
    let src = json!({"file_path": "/wt/src/hook.rs"});
    assert!(call("rev", "Edit", src.clone()).unwrap().contains("read-only"));
    assert!(call("rev", "Write", src.clone()).is_some());
    assert_eq!(call("impl", "Edit", src), None);
    assert_eq!(call("rev", "Write", json!({"file_path": "/wt/rev-12-r1/verdict.md"})), None);
    assert_eq!(call("rev", "Write", json!({"file_path": "verdict.md"})), None);
    // A V4A patch carries no `path`.
    let v4a = json!({"tool_name": "patch", "args": {"mode": "patch", "patch": "*** Update File: src/a.rs\n"}});
    assert_eq!(serde_json::from_str::<Value>(&hook_as("rev", v4a)).unwrap()["action"], "block");
}

#[test]
fn protected_config_edits_are_denied() {
    let d = tmpdir("hook-cfg");
    let cfg = d.join("fleet.toml");
    std::fs::write(&cfg, CONFIG).unwrap();
    let run = |input: Value| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_herdsman"))
            .arg("hook")
            .env("FLEET_CONFIG", &cfg)
            .env("FLEET_STATE", d.join("state/events.jsonl"))
            .env("HERDSMAN_LANE", "sendmeter:24:impl")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
        String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap()
    };
    let deny = |tool: &str, ti: Value| {
        let outs: Vec<String> = shapes(tool, ti).into_iter().map(&run).collect();
        assert!(outs[0].contains("\"deny\""), "{outs:?}");
        assert!(outs[1..].iter().all(|o| o.contains("\"block\"")), "{outs:?}");
    };
    deny("Edit", json!({"file_path": cfg}));
    deny("Bash", json!({"command": format!("sed -i '' s/hermes/claude/ {}", cfg.display())}));
    deny("Write", json!({"file_path": "~/.hermes/profiles/fleet-rev/config.yaml"}));
    for i in shapes("Bash", json!({"command": format!("cat {}", cfg.display())})) {
        assert_eq!(run(i), "");
    }
}
