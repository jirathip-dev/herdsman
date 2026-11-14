mod common;
use common::*;
use drover::events::read;
use drover::runner::{Out, Runner};
use drover::shed::{Shed, Wire};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

fn dsh_config() -> String {
    CONFIG.replace(
        "[fleet.sendmeter.role.rev]\nharness = \"hermes\"",
        "[fleet.sendmeter.role.rev]\nharness = \"dsh\"",
    )
}

const LANE: &str = "sendmeter-rev-12-r1";

struct Lane<'a> {
    f: &'a Fake,
    b: &'a FakeShed,
    states: Mutex<VecDeque<String>>,
    screen: String,
    seen: Mutex<Vec<(String, String)>>,
}

impl Lane<'_> {
    fn polls(&self) -> usize {
        self.seen.lock().unwrap().iter().filter(|s| s.0 == "poll").count()
    }
    fn at(&self, method: &str, text: &str) -> Option<usize> {
        let seen = self.seen.lock().unwrap();
        seen.iter().position(|s| s.0 == method && s.1.starts_with(text))
    }
    fn count(&self, method: &str, text: &str) -> usize {
        let seen = self.seen.lock().unwrap();
        seen.iter().filter(|s| s.0 == method && s.1.starts_with(text)).count()
    }
}

impl Runner for Lane<'_> {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        self.f.run(prog, args)
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> anyhow::Result<Out> {
        if prog != "shed" {
            return self.f.run_in(prog, args, input);
        }
        let q: Value = serde_json::from_str(input.lines().last().unwrap_or_default()).unwrap();
        let (m, name) = (q["method"].as_str().unwrap_or_default(), q["params"]["name"].as_str());
        let mut lines: Vec<Value> = (self.b.connect(input).lines())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let res = &mut lines.last_mut().unwrap()["result"];
        let mut seen = self.seen.lock().unwrap();
        let acp_check = seen.last().is_some_and(|l| l.0 == "poll") && matches!(m, "send" | "keys");
        if acp_check {
            let (_, state) = seen.pop().unwrap();
            let mut states = self.states.lock().unwrap();
            if !state.is_empty() || !states.is_empty() {
                states.push_front(state);
            }
            drop(states);
        }
        let started = seen.iter().any(|l| l.0 == "run");
        match (m, name) {
            ("run", Some(LANE)) => seen.push(("run".into(), String::new())),
            ("get", Some(LANE)) if started => {
                let state = self.states.lock().unwrap().pop_front().unwrap_or_default();
                res["reported"] = if state.is_empty() {
                    Value::Null
                } else {
                    json!({"state": state})
                };
                seen.push(("poll".into(), state));
            }
            ("read", Some(LANE)) => {
                res["text"] = json!(self.screen);
                seen.push(("read".into(), String::new()));
            }
            ("send", Some(LANE)) => seen.push(("send".into(), q["params"]["text"].as_str().unwrap().into())),
            ("keys", Some(LANE)) => seen.push(("keys".into(), q["params"]["keys"][0].as_str().unwrap().into())),
            _ => {}
        }
        let stdout = lines.iter().map(|l| format!("{l}\n")).collect();
        Ok(Out {
            stdout,
            ..Out::default()
        })
    }
    fn sleep(&self, secs: u64) {
        self.f.sleep(secs);
    }
    fn shed(&self) -> Option<Shed> {
        let wire = Wire::Connect;
        Some(Shed { wire, ..self.b.shed() })
    }
}

fn spawn_rev(name: &str, states: &[&str], f: impl FnOnce(&drover::Ctx, &Lane)) -> i32 {
    spawn_rev_with(name, states, "  ⌸ ❯\n", f)
}

fn spawn_rev_with(name: &str, states: &[&str], screen: &str, f: impl FnOnce(&drover::Ctx, &Lane)) -> i32 {
    let d = tmpdir(name);
    let c = ctx(&d, &dsh_config());
    let b = FakeShed::attached(&c);
    let wt = c.worktrees.join("sendmeter/rev-12-r1");
    let gd = d.join("gitdir");
    std::fs::create_dir_all(&wt).unwrap();
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    let r = Fake::default()
        .reply(
            "dsh-tui version",
            "@deepseek-harness-tui/dsh-tui 0.17.0 (launcher)\nprofile: 0.17.0  /x\n",
        )
        .reply("which shed", "/opt/shed/bin/shed\n")
        .reply(&format!("git -C {} rev-list", wt.display()), "0\n")
        .reply(
            &format!("git -C {} rev-parse --absolute-git-dir", wt.display()),
            &format!("{}\n", gd.display()),
        )
        .reply("gh issue view 12", r#"{"title":"t","body":"b"}"#)
        .reply("git -C", "");
    let lane = Lane {
        f: &r,
        b: &b,
        states: Mutex::new(states.iter().map(|s| s.to_string()).collect()),
        screen: screen.into(),
        seen: Mutex::default(),
    };
    let (code, out) = run(&c, &lane, &["spawn", "sendmeter", "12", "rev"]);
    assert!(code == 0 || code == 4, "{out}");
    f(&c, &lane);
    code
}

#[test]
fn dry_run_launches_pinned_dsh_tui_with_role_patch() {
    let d = tmpdir("dsh-plan");
    let c = ctx(&d, &dsh_config());
    let sh = FakeShed::new();
    sh.attach(&c);
    let (code, out) = run(
        &c,
        &Fake::default().reply("which shed", "/opt/shed/bin/shed\n"),
        &["spawn", "sendmeter", "12", "rev", "--dry-run", "--json"],
    );
    assert_eq!(code, 0, "{out}");
    let p: Value = serde_json::from_str(&out).unwrap();
    let cmd = |i: usize| -> (String, Value) {
        let w = p["commands"][i].as_array().unwrap();
        assert_eq!(w[0], "shed", "{w:?}");
        let params = serde_json::from_str(w[2].as_str().unwrap()).unwrap();
        (w[1].as_str().unwrap().to_string(), params)
    };
    let (lane, patch) = (
        "sendmeter-rev-12-r1",
        c.config.parent().unwrap().join("dsh/fleet-rev.patch.yml"),
    );
    let at = |m: &str, v: Value| (m.to_string(), v);
    assert_eq!(cmd(2).0, "run");
    assert_eq!(cmd(2).1["labels"]["harness"], "dsh");
    let launch = format!("exec dsh-tui --patch '{}'\r", patch.display());
    assert_eq!(cmd(4), at("send", json!({"name": lane, "text": launch})));
    assert_eq!(cmd(5), at("get", json!({"name": lane})));
    assert_eq!(cmd(6), at("read", json!({"name": lane, "history": false})));
    assert_eq!(cmd(7), at("keys", json!({"name": lane, "keys": ["esc"]})));
    let (m, kick) = cmd(8);
    assert!(
        m == "send" && kick["text"].as_str().unwrap().starts_with("Read "),
        "{kick}"
    );
    assert_eq!(cmd(9), at("get", json!({"name": lane})));
    assert_eq!(cmd(10), at("keys", json!({"name": lane, "keys": ["enter"]})));
    assert!(sh.methods().is_empty(), "a dry run calls nothing");
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

#[test]
fn spawn_waits_for_dsh_tui_idle_then_kicks_and_confirms_working() {
    let code = spawn_rev("dsh-working", &["", "idle", "working"], |c, l| {
        assert_eq!(l.polls(), 3, "shell, idle, then working: {:?}", l.seen.lock().unwrap());
        let start = l.at("send", "exec dsh-tui --patch").unwrap();
        let prompt = l.at("send", "Read ").unwrap();
        let seen = l.seen.lock().unwrap();
        let idle = seen.iter().position(|s| s.0 == "poll" && s.1 == "idle").unwrap();
        assert!(start < idle && idle < prompt, "{seen:?}");
        assert!(
            seen[start - 1].1.starts_with("export DROVER_LANE='sendmeter:12:rev'"),
            "{seen:?}"
        );
        assert_eq!(seen[start - 2].0, "run", "{seen:?}");
        let calls = l.f.calls.borrow();
        assert!(calls.iter().any(|c| c.contains("config --worktree core.hooksPath")));
        let root = c.state.parent().unwrap().parent().unwrap();
        let hook = std::fs::read_to_string(root.join("gitdir/fleet-hooks/pre-push")).unwrap();
        assert_eq!(
            hook,
            "#!/bin/sh\nexec '/opt/drover/bin/drover' hook --role rev --git-pre-push\n"
        );
        let ev = read(&c.state).unwrap();
        let last = ev.last().unwrap();
        assert_eq!(
            (last.kind.as_str(), last.val("harness"), last.val("confirmed")),
            ("spawn", &"dsh".into(), &true.into())
        );
    });
    assert_eq!(code, 0);
}

#[test]
fn quick_turn_that_lands_on_done_confirms() {
    let code = spawn_rev("dsh-done", &["idle", "idle", "done"], |_, l| {
        assert_eq!(l.f.count("sleep 1"), 1, "{:?}", l.seen.lock().unwrap());
    });
    assert_eq!(code, 0);
}

#[test]
fn never_idle_sends_no_prompt_and_exits_4() {
    let code = spawn_rev("dsh-never", &[], |c, l| {
        assert_eq!(l.count("send", "Read "), 0);
        assert_eq!(l.polls(), 90);
        assert_eq!(*read(&c.state).unwrap().last().unwrap().val("confirmed"), false);
    });
    assert_eq!(code, 4);
}

#[test]
fn first_run_wizard_is_skipped_before_the_prompt() {
    let wizard =
        "  Welcome to dsh-TUI  Step 1 of 4\n    ←/→ step · Enter act on this step / go next · Esc skip the guide\n";
    let code = spawn_rev_with("dsh-wizard", &["idle", "idle", "working"], wizard, |_, l| {
        let esc = l.at("keys", "esc").expect("esc sent");
        assert!(esc < l.at("send", "Read ").unwrap(), "{:?}", l.seen.lock().unwrap());
    });
    assert_eq!(code, 0);
    let code = spawn_rev("dsh-no-wizard", &["idle", "working"], |_, l| {
        assert_eq!(l.at("keys", "esc"), None, "no wizard, no Esc");
    });
    assert_eq!(code, 0);
}

#[test]
fn unconfirmed_prompt_is_retried_and_a_typed_kick_gets_enter() {
    let mut states = vec!["idle"; 11];
    states.push("working");
    let typed = format!("  ⌸ ❯ Read {}/fleet-test-dsh-retry\n", std::env::temp_dir().display());
    let code = spawn_rev_with("dsh-retry", &states, &typed, |_, l| {
        assert_eq!(l.count("send", "Read "), 1, "typed kick is not sent twice");
        assert_eq!(l.count("keys", "enter"), 2, "Enter after the kick, then once more");
        assert_eq!(l.f.count("sleep 5"), 1);
    });
    assert_eq!(code, 0);
}

#[test]
fn spawn_refuses_unpinned_dsh_tui() {
    let d = tmpdir("dsh-pin");
    let c = ctx(&d, &dsh_config());
    let sh = FakeShed::new();
    sh.attach(&c);
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    let r = Fake::default()
        .reply("which shed", "/opt/shed/bin/shed\n")
        .reply("dsh-tui version", "profile: 0.18.0  /x\n");
    let (code, out) = run(&c, &r, &["spawn", "sendmeter", "12", "rev"]);
    assert_eq!(code, 1, "{out}");
    let started = |m: &String| ["run", "send", "keys"].contains(&m.as_str());
    assert!(!sh.methods().iter().any(started), "no lane started: {:?}", sh.methods());
    assert!(
        !r.called("git -C /tmp/does-not-matter/sendmeter worktree add"),
        "{:?}",
        r.calls.borrow()
    );
}

fn pre_push(c: &drover::Ctx, role: &str, lines: &str) -> (i32, String) {
    let mut out = Vec::new();
    let args = ["drover", "hook", "--role", role, "--git-pre-push"]
        .map(String::from)
        .to_vec();
    let code = drover::main_with(args, c, &Fake::default(), &mut out, &mut lines.as_bytes());
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

#[test]
fn pre_push_refuses_scripts_that_start_containers() {
    let d = tmpdir("dsh-prepush-deny");
    let c = ctx(&d, &dsh_config());
    let line = format!(
        "refs/heads/issue-12 {} refs/heads/issue-12 {}\n",
        "a".repeat(40),
        "0".repeat(40)
    );
    let push = |diff: &str| {
        let r = Fake::default().reply("git log -p", diff);
        let mut out = Vec::new();
        let args = ["drover", "hook", "--role", "impl", "--git-pre-push"]
            .map(String::from)
            .to_vec();
        let code = drover::main_with(args, &c, &r, &mut out, &mut line.as_bytes());
        let scanned = r
            .calls
            .borrow()
            .iter()
            .find(|l| l.starts_with("git log -p"))
            .cloned()
            .unwrap();
        (code, String::from_utf8(out).unwrap(), scanned)
    };
    let (code, out, scanned) = push("+++ b/scripts/test.sh\n+set -e\n+docker compose up -d db\n");
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("REFUSED NO_CONTAINERS"), "{out}");
    assert!(
        scanned.contains("*.sh") && scanned.contains("*.zsh") && scanned.contains(":!.github/workflows"),
        "{scanned}"
    );
    assert_eq!(
        push("+++ b/scripts/test.sh\n+cargo test\n-docker ps\n").0,
        0,
        "removed lines pass"
    );
    assert_eq!(push("+++ b/justfile\n+\tsupabase migration new x\n").0, 0);
    assert_eq!(
        push("+++ b/Makefile\n+db:\n+\tdocker run -d pg\n").0,
        1,
        "a recipe line"
    );
    assert!(
        !scanned.contains("package.json"),
        "JSON scripts are not command lines: {scanned}"
    );
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
    drover::providers::install_pre_push(&drover::runner::Real, env!("CARGO_BIN_EXE_drover"), &lane, "impl").unwrap();

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
        "main checkout has no drover hook"
    );
}
