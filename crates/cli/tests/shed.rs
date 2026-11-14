mod common;
use common::*;
use drover::events::Event;
use drover::hosts::On;
use drover::runner::Runner;
use drover::shed::proto::Response;
use drover::shed::{self, Wire};
use drover::term;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

type Terms = Arc<Mutex<common::fake_shed::State>>;

fn lane_labels() -> Value {
    json!({"fleet": "sendmeter", "issue": "12", "role": "impl", "harness": "claude"})
}

#[test]
fn every_adapter_call_is_a_shed_request() {
    let b = FakeShed::new();
    b.add("sendmeter-impl-12", lane_labels());
    b.add("scratch", json!({}));
    let f = Fake::default();
    let r = shed::With(&f, b.shed());
    let lane = "sendmeter-impl-12";

    assert_eq!(term::read(&r, lane).unwrap(), "screen of sendmeter-impl-12");
    assert_eq!(b.last("read"), json!({"name": lane, "history": false}));
    term::recent(&r, lane, "200").unwrap();
    assert_eq!(b.last("read"), json!({"name": lane, "lines": 200, "history": true}));
    term::keys(&r, lane, "Escape").unwrap();
    assert_eq!(b.last("keys"), json!({"name": lane, "keys": ["esc"]}));
    term::reply(&r, false, lane, lane, "go on").unwrap();
    assert_eq!(b.last("send"), json!({"name": lane, "text": "go on"}));
    assert_eq!(b.last("keys"), json!({"name": lane, "keys": ["Enter"]}));
    term::reply(&r, true, lane, lane, "typed").unwrap();
    assert_eq!(b.last("send"), json!({"name": lane, "text": "typed"}));
    assert!(term::landed(&r, lane, lane, "x"));
    assert_eq!(
        b.last("wait"),
        json!({"name": lane, "until": ["working", "blocked"], "timeout": 20.0})
    );
    let pane = term::harness_in(&r, lane).unwrap();
    assert_eq!(
        (pane["agent"].as_str(), pane["pane_id"].as_str()),
        (Some("claude"), Some(lane))
    );
    let o = term::exec(&r, &term::wait_output_argv(lane, "❯", "60000")).unwrap();
    assert_eq!(o.code, 0);
    assert_eq!(
        b.last("wait"),
        json!({"name": lane, "until": [], "output": "❯", "timeout": 60.0})
    );
    term::ok(&r, &term::run_argv(lane, &["exec", "'dsh-tui'"])).unwrap();
    assert_eq!(b.last("send"), json!({"name": lane, "text": "exec 'dsh-tui'\r"}));
    let a = term::agents(&r).unwrap();
    assert_eq!(a.len(), 1, "{a:?}");
    assert_eq!(
        (a[0]["name"].as_str(), a[0]["agent_status"].as_str()),
        (Some(lane), Some("working"))
    );
    assert_eq!(term::list(&r).unwrap()["result"]["agents"], json!(a));
    term::close(&r, lane).unwrap();
    assert_eq!(b.methods().iter().rev().take(2).collect::<Vec<_>>(), ["rm", "kill"]);
    assert!(term::agents(&r).unwrap().is_empty());
    term::close(&r, lane).unwrap();
    let o = term::exec(&r, &term::read_argv(lane)).unwrap();
    assert!(term::failed_with(&o, "pane_not_found"), "{o:?}");
    assert!(term::read(&r, "w1:p1").is_err());
    assert!(f.calls.borrow().is_empty(), "{:?}", f.calls.borrow());
}

#[test]
fn a_wait_that_times_out_answers_as_herdr() {
    let b = FakeShed::new();
    b.add("slow", lane_labels());
    b.wait_ends("slow", "timeout");
    let f = Fake::default();
    let r = shed::With(&f, b.shed());
    let o = term::exec(&r, &term::wait_working_argv("slow", "20000")).unwrap();
    assert!(o.code != 0 && term::failed_with(&o, "timeout"), "{o:?}");
}

#[test]
fn another_build_of_the_same_protocol_is_accepted() {
    let b = FakeShed::start("9.9.9", vec![]);
    b.state.lock().unwrap().hello.as_mut().unwrap().build = "0000000".into();
    shed::call(&Fake::default(), &b.shed().wire, shed::proto::Method::Ls, json!({})).unwrap();
    assert_eq!(b.methods(), ["ls"]);
}

#[test]
fn another_shed_protocol_is_refused_naming_both_sides() {
    use shed::proto::{BUILD, PROTOCOL, VERSION};
    for protocol in [PROTOCOL - 1, PROTOCOL + 1] {
        let b = FakeShed::start("9.9.9", vec![]);
        b.state.lock().unwrap().hello.as_mut().unwrap().build = "0000000".into();
        b.state.lock().unwrap().hello.as_mut().unwrap().protocol = protocol;
        let e = shed::call(&Fake::default(), &b.shed().wire, shed::proto::Method::Ls, json!({})).unwrap_err();
        assert_eq!(
            format!("{e:#}"),
            format!(
                "SHED_VERSION: drover speaks shed {VERSION} ({BUILD}) protocol {PROTOCOL}, \
                 the server is 9.9.9 (0000000) protocol {protocol}"
            )
        );
    }
}

#[test]
fn another_host_is_reached_through_shed_connect_over_ssh() {
    let b = FakeShed::new();
    let hello = json!(Response::ok(json!(0), json!(shed::proto::Hello::ours())));
    let read = json!(Response::ok(json!(1), json!({"text": "remote screen"})));
    let f = Fake::default().reply("ssh", &format!("{hello}\n{read}\n"));
    let base = shed::With(&f, b.shed());
    let on = On::new(&base, Some("box"));
    assert_eq!(term::read(&on, "sendmeter-impl-12").unwrap(), "remote screen");
    let calls = f.calls.borrow();
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("ssh ") && c.contains("box -- 'shed' 'connect' '--autostart'")),
        "{calls:?}"
    );
    assert!(b.methods().is_empty(), "nothing went to this host's server");
}

fn shed_config(sock: &std::path::Path, extra: &str) -> String {
    kind_config("shed", sock, extra)
}

fn kind_config(kind: &str, sock: &std::path::Path, extra: &str) -> String {
    let cfg = CONFIG.replace("harness  =  \"hermes\"", "harness = \"claude\"");
    format!(
        "{cfg}\n[runtime]\nkind = \"{kind}\"\nsocket = \"{}\"\n{extra}",
        sock.display()
    )
}

fn spawn_fake() -> Fake {
    Fake::default()
        .once("git -C /tmp/does-not-matter/sendmeter rev-parse --verify", 1, "")
        .reply("gh issue view 12", r#"{"title":"Fix the meter","body":"AC: it works"}"#)
        .reply("git -C", "")
        .reply("which shed", "/opt/shed/bin/shed\n")
}

#[test]
fn a_lane_spawns_on_shed() {
    let b = FakeShed::new();
    let d = tmpdir("shed-spawn");
    let c = ctx(&d, &shed_config(&b.sock, ""));
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    let settings = std::fs::read_to_string(c.config.parent().unwrap().join("claude/fleet-impl/settings.json")).unwrap();
    let s: Value = serde_json::from_str(&settings).unwrap();
    assert!(
        s["hooks"]["UserPromptSubmit"].is_array() && s["hooks"]["Notification"].is_array(),
        "{s}"
    );

    let (code, out) = run(
        &c,
        &Fake::default().reply("which shed", "/opt/shed/bin/shed\n"),
        &["spawn", "sendmeter", "12", "impl", "--dry-run", "--json"],
    );
    assert_eq!(code, 0, "{out}");
    let plan: Value = serde_json::from_str(&out).unwrap();
    let cmds = plan["commands"].as_array().unwrap();
    assert_eq!(
        cmds[1].as_array().unwrap()[3..6],
        [json!("worktree"), json!("add"), json!("-b")]
    );
    assert_eq!((cmds[2][0].as_str(), cmds[2][1].as_str()), (Some("shed"), Some("run")));
    assert!(b.methods().is_empty(), "a dry run calls nothing");

    let f = spawn_fake();
    let (code, out) = run(&c, &f, &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 0, "{out} {:?}", f.calls.borrow());
    let wt = c.worktrees.join("sendmeter/issue-12");
    assert!(f.called(&format!(
        "git -C /tmp/does-not-matter/sendmeter worktree add -b issue-12 {} origin/staging",
        wt.display()
    )));
    assert!(
        !f.called("herdr agent start") && !f.called("herdr worktree"),
        "{:?}",
        f.calls.borrow()
    );
    let run_p = b.last("run");
    assert_eq!(run_p["name"], "sendmeter-impl-12");
    assert_eq!(run_p["labels"], lane_labels());
    assert_eq!(
        (run_p["cwd"].as_str(), run_p["keep"].as_bool()),
        (wt.to_str(), Some(true))
    );
    assert_eq!(
        run_p["cmd"],
        json!(["sh", "-c", "exec \"${SHELL:-sh}\" -l"]),
        "no secret in the terminal's argv"
    );
    let sends: Vec<String> = b
        .lock()
        .seen
        .iter()
        .filter(|s| s.0 == "send")
        .map(|s| s.1["text"].as_str().unwrap().into())
        .collect();
    assert!(
        sends[0].starts_with("export DROVER_LANE='sendmeter:12:impl'") && sends[0].ends_with('\r'),
        "{sends:?}"
    );
    assert!(
        sends[1].starts_with("exec 'claude' '--dangerously-skip-permissions'"),
        "{sends:?}"
    );
    assert_eq!(b.last("wait")["until"], json!(["working", "blocked"]));
    let ev = drover::events::read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .rfind(|e| e.kind == "spawn")
        .unwrap();
    assert_eq!(
        (ev.str("runtime"), ev.str("pane"), ev.str("workspace")),
        (Some("shed"), Some("sendmeter-impl-12"), None)
    );
    assert_eq!(ev.val("confirmed"), true);
    assert!(std::fs::read_to_string(wt.join(".brief.md"))
        .unwrap()
        .contains("AC: it works"));
    assert_eq!(ev.str("qos"), Some("background"));
    let (_, st) = run(&c, &spawn_fake(), &["status", "sendmeter"]);
    assert!(st.contains(" · qos background"), "{st}");

    let (code, out) = run(&c, &spawn_fake(), &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("LANE_LIVE"), "{out}");
}

fn acp_config(sock: &std::path::Path, harness: &str, account: bool) -> String {
    let acct = if account { "\naccount = \"C\"" } else { "" };
    let cfg = CONFIG.replace(
        "harness  =  \"hermes\"    # toggled in tests",
        &format!("harness = \"{harness}\"{acct}"),
    );
    format!(
        "{cfg}\n[fleet.sendmeter.runtime]\nkind = \"shed-acp\"\nsocket = \"{}\"\n\n[accounts]\nC = \"C\"\n",
        sock.display()
    )
}

fn acct_c() -> String {
    drover::config::home()
        .join(".config/drover/accounts/C")
        .display()
        .to_string()
}

const TOKEN_C: &str = concat!("sk", "-ant-oat01-CCCC-secret");

fn acp_fake(adapter: &[&str]) -> Fake {
    let agents = json!([{"id": "claude-acp", "version": "0.101.0", "distribution": "npx", "cmd": adapter}]);
    spawn_fake()
        .reply("shed agents --json", &agents.to_string())
        .reply("shed run", "")
        .reply("shed send", "")
        .reply("shed cancel", "")
}

fn acp_ctx(name: &str, cfg: &str) -> drover::Ctx {
    let mut c = ctx(&tmpdir(name), cfg);
    c.exe = env!("CARGO_BIN_EXE_drover").into();
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    c
}

#[test]
fn a_shed_acp_fleet_starts_its_lane_as_an_acp_session_with_the_brief_as_its_first_prompt() {
    let b = FakeShed::new();
    b.add("sendmeter-impl-12", lane_labels());
    b.lock().terms.get_mut("sendmeter-impl-12").unwrap()["state"] = json!("exited");
    let c = acp_ctx("acp-spawn", &acp_config(&b.sock, "claude", true));
    let f = acp_fake(&["echo", "claude-agent-acp"]);
    let cli = b.cli(&f);
    let (code, out) = run(&c, &cli, &["spawn", "sendmeter", "12", "impl", "--supersede"]);
    assert_eq!(code, 0, "{out} {:?}", f.calls.borrow());
    let wt = c.worktrees.join("sendmeter/issue-12");
    let calls = f.calls.borrow();
    let start = calls.iter().find(|c| c.starts_with("shed run")).unwrap();
    let head = format!(
        "shed run --acp claude-acp --policy allow --name sendmeter-impl-12 --cwd {} --label fleet=sendmeter \
         --label issue=12 --label role=impl --label harness=claude --label source=drover --label group=sendmeter \
         --label item=#12 --label round=0 --label url=https://github.com/jirathip-dev/sendmeter/issues/12 \
         --label drover.fleet=sendmeter --label drover.issue=12 --label drover.role=impl --label drover.round=0 -- sh -c ",
        wt.display()
    );
    assert!(start.starts_with(&head), "{start}");
    assert!(start.ends_with(" sh echo claude-agent-acp"), "{start}");
    assert!(
        !start.contains(TOKEN_C) && !out.contains(TOKEN_C),
        "no token in argv: {start}"
    );
    let brief = std::fs::read_to_string(wt.join(".brief.md")).unwrap();
    assert!(brief.contains("AC: it works"));
    let sends: Vec<_> = calls.iter().filter(|c| c.starts_with("shed send")).collect();
    assert_eq!(sends, [&format!("shed send sendmeter-impl-12 {brief}")]);
    let m = b.methods();
    assert_eq!(m.iter().filter(|m| *m == "rm").count(), 1, "{m:?}");
    assert!(!m.iter().any(|m| m == "send" || m == "keys" || m == "run"), "{m:?}");
    assert_eq!(b.last("wait")["until"], json!(["working", "blocked", "done"]));
    let ev = drover::events::read(&c.state).unwrap();
    let sp = ev.iter().rfind(|e| e.kind == "spawn").unwrap();
    assert_eq!(
        (sp.str("pane"), sp.val("confirmed").as_bool(), sp.str("runtime")),
        (Some("sendmeter-impl-12"), Some(true), Some("shed-acp"))
    );
    assert!(!drover::events::jsonl(&c.state).unwrap().contains(TOKEN_C));

    drop(calls);
    let (none, s) = (Fake::default(), b.shed());
    let none = shed::With(&none, s);
    assert!(
        term::agents(&On::new(&none, Some("bazzite"))).is_err(),
        "neither answers: unreachable"
    );
    let cfg = drover::config::Config::load(&c.config).unwrap();
    let d = json!({"id": "q1", "agent": "sendmeter-impl-12"});
    let ask = Event::new(c.now, "sendmeter", Some(12), "needs-decision", d);
    drover::events::append(&c.state, &ask).unwrap();
    let r = shed::With(&f, cfg.shed().unwrap());
    let to = drover::deliver(&c, &cfg, &r, &ask, "DECISION q1: go on", false).unwrap();
    assert_eq!(to.as_deref(), Some("sendmeter-impl-12"));
    assert!(f.called("shed send sendmeter-impl-12 DECISION q1: go on"));
    assert!(
        !b.methods().iter().any(|m| m == "send" || m == "keys"),
        "{:?}",
        b.methods()
    );
}

fn bash_call(cmd: &str, cwd: &std::path::Path) -> String {
    json!({"hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": {"command": cmd}, "cwd": cwd})
        .to_string()
}

#[test]
fn a_guarded_command_is_refused_inside_an_acp_lane() {
    let b = FakeShed::new();
    let c = acp_ctx("acp-guard", &acp_config(&b.sock, "claude", true));
    let ef = acp_fake(&["env"]);
    let cli = b.cli(&ef);
    let (code, out) = run(&c, &cli, &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 0, "{out}");
    let dump = cli.out.borrow().concat();
    let env: BTreeMap<&str, &str> = dump.lines().filter_map(|l| l.split_once('=')).collect();
    assert_eq!(env.get("CLAUDE_CONFIG_DIR"), Some(&acct_c().as_str()));
    assert!(!env.contains_key("CLAUDE_CODE_OAUTH_TOKEN"), "{env:?}");
    assert!(!dump.contains(TOKEN_C));
    assert_eq!(env.get("DROVER_LANE"), Some(&"sendmeter:12:impl"));
    assert_eq!(env.get("ANTHROPIC_MODEL"), Some(&"opus"));
    assert!(!env.contains_key("SHED"), "the session reports its state, not the hook");
    let wt = c.worktrees.join("sendmeter/issue-12");
    let local = std::fs::read_to_string(wt.join(".claude/settings.local.json")).unwrap();
    let s: Value = serde_json::from_str(&local).unwrap();
    assert_eq!(s["permissions"]["defaultMode"], "bypassPermissions");
    assert!(std::fs::read_to_string(wt.join("CLAUDE.local.md"))
        .unwrap()
        .contains("drover"));
    let hook = s["hooks"]["PreToolUse"][0]["hooks"][0]["command"].as_str().unwrap();
    let tool = |cmd: &str| {
        let mut ch = std::process::Command::new("sh")
            .args(["-c", hook])
            .env_clear()
            .envs(env.iter().map(|(k, v)| (*k, *v)))
            .env("FLEET_CONFIG", &c.config)
            .env("FLEET_STATE", &c.state)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        ch.stdin
            .take()
            .unwrap()
            .write_all(bash_call(cmd, &wt).as_bytes())
            .unwrap();
        let o = ch.wait_with_output().unwrap();
        assert!(o.status.success());
        String::from_utf8(o.stdout).unwrap()
    };
    let kill = ["p", "kill x"].concat();
    for (cmd, why) in [
        ("git push origin staging", "staging"),
        (kill.as_str(), "NO_BROAD_KILL"),
        ("gh pr merge 3", "merge"),
    ] {
        let o = tool(cmd);
        let v: Value = serde_json::from_str(&o).unwrap_or_else(|_| panic!("{cmd}: {o}"));
        let d = &v["hookSpecificOutput"];
        assert_eq!(d["permissionDecision"], "deny", "{cmd}: {o}");
        assert!(
            d["permissionDecisionReason"].as_str().unwrap().contains(why),
            "{cmd}: {o}"
        );
    }
    assert_eq!(tool("cargo test"), "", "an ordinary command runs");
}

#[test]
fn an_unguardable_acp_lane_refuses_to_start() {
    let b = FakeShed::new();
    let unsynced = ctx(&tmpdir("acp-nohook"), &acp_config(&b.sock, "claude", true));
    let cases = [
        (
            acp_ctx("acp-hermes", &acp_config(&b.sock, "hermes", true)),
            "runs no hermes hook",
        ),
        (unsynced, "no drover PreToolUse hook"),
    ];
    for (c, why) in cases {
        let f = acp_fake(&["env"]);
        let cli = b.cli(&f);
        let (code, out) = run(&c, &cli, &["spawn", "sendmeter", "12", "impl"]);
        assert_eq!(code, 3, "{out}");
        assert!(out.contains("REFUSED ACP_UNGUARDED") && out.contains(why), "{out}");
        assert!(
            !f.called("shed run") && !f.called("git"),
            "nothing started: {:?}",
            f.calls.borrow()
        );
        let (code, out) = run(&c, &f, &["spawn", "sendmeter", "12", "impl", "--dry-run"]);
        assert!(code == 3 && out.contains("ACP_UNGUARDED"), "the dry run too: {out}");
    }
    assert!(b.methods().is_empty(), "{:?}", b.methods());
}

#[test]
fn an_acp_lane_with_no_account_starts_guarded_on_the_hosts_own_login() {
    let b = FakeShed::new();
    let c = acp_ctx("acp-noacct", &acp_config(&b.sock, "claude", false));
    let f = acp_fake(&["echo", "claude-agent-acp"]);
    let cli = b.cli(&f);
    let (code, out) = run(&c, &cli, &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 0, "{out} {:?}", f.calls.borrow());
    let calls = f.calls.borrow();
    let start = calls
        .iter()
        .find(|c| c.starts_with("shed run --acp claude-acp"))
        .unwrap();
    assert!(
        start.contains("settings.local.json"),
        "its profile, so the hook guards it: {start}"
    );
    assert!(
        !start.contains("CLAUDE_CONFIG_DIR=") && !start.contains("HERDSMAN_ACCOUNT="),
        "no account env: {start}"
    );
    assert!(
        start.contains("unset CLAUDE_CODE_OAUTH_TOKEN"),
        "an inherited token never signs it in: {start}"
    );
}

#[test]
fn a_missing_shed_is_named_by_spawn_and_doctor_and_timer_install_adds_it() {
    let b = FakeShed::new();
    let c = acp_ctx("shed-missing", &acp_config(&b.sock, "claude", true));
    let f = acp_fake(&["no-such-adapter-600"]).once("which shed", 1, "");
    let (code, out) = run(&c, &f, &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("REFUSED SHED_MISSING: shed is not on PATH ("), "{out}");
    assert!(!out.contains("os error 2"), "{out}");
    assert!(!c.worktrees.join("sendmeter/issue-12").exists(), "no worktree");
    assert!(!f.called("shed ") && !f.called("git"), "{:?}", f.calls.borrow());

    let agents = json!([{"id": "claude-acp", "cmd": ["no-such-adapter-600"]}]);
    let none = Fake::default().reply("shed agents --json", &agents.to_string());
    assert_eq!(run(&c, &none, &["timer", "install", "--no-load"]).0, 0);
    let (_, out) = run(&c, &none, &["doctor"]);
    assert!(out.contains("✗ shed — not on the timer's PATH → "), "{out}");
    assert!(
        out.contains("✗ shed agent claude-acp — no-such-adapter-600 not on the timer's PATH → "),
        "{out}"
    );

    let bin = c.home.join("cargo-bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("shed"), "").unwrap();
    let r = none.reply("which shed", &format!("{}\n", bin.join("shed").display()));
    assert_eq!(run(&c, &r, &["timer", "install", "--no-load"]).0, 0);
    let job = [
        "Library/LaunchAgents/dev.drover.tick.plist",
        ".config/systemd/user/dev.drover.tick.service",
    ];
    let text = job
        .iter()
        .find_map(|j| std::fs::read_to_string(c.home.join(j)).ok())
        .unwrap();
    let path = text.split_once("PATH").unwrap().1;
    assert!(path.contains(&format!("{}:", bin.display())), "{text}");
    let (_, out) = run(&c, &r, &["doctor"]);
    assert!(
        out.contains(&format!("✓ shed — {} on the timer's PATH", bin.join("shed").display())),
        "{out}"
    );
}

#[test]
fn an_acp_lanes_state_follows_its_turn_events() {
    let ev = |seq: u64, kind: Value| {
        let mut e = json!({"seq": seq, "name": "sendmeter-impl-12", "labels": lane_labels()});
        e.as_object_mut().unwrap().extend(kind.as_object().unwrap().clone());
        e
    };
    let events = vec![
        ev(1, json!({"event": "turn", "prompt": "# Brief"})),
        ev(2, json!({"event": "state", "state": "working"})),
        ev(
            3,
            json!({"event": "tool_call", "id": "t1", "title": "git push", "status": "pending"}),
        ),
        ev(
            4,
            json!({"event": "approval_request", "tool": "git push", "options": []}),
        ),
        ev(5, json!({"event": "state", "state": "blocked", "note": "git push"})),
        ev(6, json!({"event": "reply", "text": "done", "stop_reason": "end_turn"})),
        ev(7, json!({"event": "state", "state": "done", "note": "end_turn"})),
        ev(8, json!({"event": "exited", "code": 0})),
    ];
    let b = FakeShed::start(shed::proto::VERSION, events);
    let c = ctx(&tmpdir("acp-state"), &acp_config(&b.sock, "claude", true));
    std::fs::create_dir_all(c.state.parent().unwrap()).unwrap();
    let cfg = drover::config::Config::load(&c.config).unwrap();
    assert!(cfg.runtime.is_none(), "no global runtime: the fleet's own");
    let sh = cfg.shed_for("sendmeter").unwrap();
    assert_eq!((sh.acp.as_deref(), sh.policy.as_str()), (Some("claude-acp"), "allow"));
    assert_eq!(shed::intake(&c, &cfg, &cfg.shed().unwrap().wire).unwrap(), 4);
    let states: Vec<_> = drover::events::read(&c.state)
        .unwrap()
        .iter()
        .filter(|e| e.kind == "lane-state")
        .map(|e| (e.str("state").map(String::from), e.str("note").map(String::from)))
        .collect();
    let s = |a: &str, n: Option<&str>| (Some(a.to_string()), n.map(String::from));
    assert_eq!(
        states,
        [
            s("working", None),
            s("blocked", Some("git push")),
            s("done", Some("end_turn")),
            s("gone", None)
        ]
    );
}

fn hook(sock: &std::path::Path, event: &str) -> String {
    hook_in("shed", "SHED", sock, event)
}

fn hook_in(kind: &str, var: &str, sock: &std::path::Path, event: &str) -> String {
    let d = tmpdir("shed-hook");
    std::fs::write(d.join("fleet.toml"), kind_config(kind, sock, "")).unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_drover"))
        .args(["hook", "--role", "impl"])
        .env("DROVER_LANE", "sendmeter:-:impl")
        .env_remove("SHED")
        .env_remove("BERTH")
        .env(var, "sendmeter-impl-12")
        .env("FLEET_CONFIG", d.join("fleet.toml"))
        .env("FLEET_STATE", d.join("state/events.jsonl"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let input = json!({"hook_event_name": event, "session_id": "s"}).to_string();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success());
    String::from_utf8(o.stderr).unwrap()
}

#[test]
fn the_lane_hook_reports_its_state_to_shed() {
    let b = FakeShed::new();
    b.add("sendmeter-impl-12", lane_labels());
    for (event, state) in [
        ("UserPromptSubmit", "working"),
        ("Notification", "blocked"),
        ("Stop", "done"),
    ] {
        hook(&b.sock, event);
        assert_eq!(
            b.last("state"),
            json!({"name": "sendmeter-impl-12", "state": state}),
            "{event}"
        );
    }
}

#[test]
fn drive_logs_sheds_state_changes_since_its_last_pass() {
    let ev = |seq: u64, name: &str, labels: Value, kind: Value| {
        let mut e = json!({"seq": seq, "name": name, "labels": labels});
        e.as_object_mut().unwrap().extend(kind.as_object().unwrap().clone());
        e
    };
    let events = vec![
        ev(
            4,
            "sendmeter-impl-12",
            lane_labels(),
            json!({"event": "state", "state": "done", "note": "end_turn"}),
        ),
        ev(5, "scratch", json!({}), json!({"event": "state", "state": "working"})),
        ev(
            6,
            "sendmeter-impl-12",
            lane_labels(),
            json!({"event": "resources", "rss_mb": 900, "cpu_pct": 50}),
        ),
        ev(
            7,
            "sendmeter-impl-12",
            lane_labels(),
            json!({"event": "exited", "code": 0}),
        ),
    ];
    let b = FakeShed::start(shed::proto::VERSION, events);
    let d = tmpdir("shed-drive");
    let c = ctx(&d, &shed_config(&b.sock, ""));
    std::fs::create_dir_all(c.state.parent().unwrap()).unwrap();
    let cfg = drover::config::Config::load(&c.config).unwrap();
    assert_eq!(shed::intake(&c, &cfg, &b.shed().wire).unwrap(), 2);
    assert_eq!(b.last("events"), json!({"labels": {}, "since": 0}));
    let logged: Vec<_> = drover::events::read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .filter(|e| e.kind == "lane-state")
        .collect();
    let states: Vec<_> = logged
        .iter()
        .map(|e| (e.issue, e.str("state"), e.str("agent")))
        .collect();
    assert_eq!(
        states,
        [
            (Some(12), Some("done"), Some("sendmeter-impl-12")),
            (Some(12), Some("gone"), Some("sendmeter-impl-12"))
        ]
    );
    assert_eq!(
        std::fs::read_to_string(c.state.with_file_name("shed.seq")).unwrap(),
        "7"
    );
    shed::intake(&c, &cfg, &b.shed().wire).unwrap();
    assert_eq!(
        b.last("events")["since"],
        7,
        "the next pass asks only for what it has not seen"
    );
    let none = Wire::Socket(d.join("none.sock"));
    assert!(shed::intake(&c, &cfg, &none).is_err());
}

#[test]
fn no_runtime_is_no_shed_and_herdr_fails_to_load() {
    let cfg = drover::config::Config::parse(CONFIG).unwrap();
    assert!(cfg.shed().is_none());
    assert!(Fake::default().shed().is_none());
    let bad = format!("{CONFIG}\n[runtime]\nkind = \"tmux\"\n");
    assert!(drover::config::Config::parse(&bad).is_err());
    for herdr in [
        format!("{CONFIG}\n[runtime]\nkind = \"herdr\"\n"),
        format!("{CONFIG}\n[fleet.sendmeter.runtime]\nkind = \"herdr\"\n"),
        format!("{CONFIG}\n[hosts.air]\nmachine = \"air\"\nruntime = \"herdr\"\n"),
    ] {
        let e = format!("{:#}", drover::config::Config::parse(&herdr).unwrap_err());
        assert!(e.contains(drover::config::RETIRED_RUNTIME), "{e}");
    }
}

#[test]
fn another_product_at_the_same_version_is_refused() {
    let hello = json!(Response::ok(json!(0), json!({"version": shed::proto::VERSION})));
    let f = Fake::default().reply("ssh", &format!("{hello}\n"));
    let on = On::new(&f, Some("box"));
    let e = shed::call(&on, &Wire::Connect, shed::proto::Method::Ls, json!({})).unwrap_err();
    assert!(
        format!("{e:#}").starts_with("SHED_VERSION") && format!("{e:#}").contains("unnamed"),
        "{e:#}"
    );
}

#[test]
fn the_old_kind_and_env_still_work_with_a_warning() {
    let b = FakeShed::new();
    b.add("sendmeter-impl-12", lane_labels());
    let err = hook_in("berth", "BERTH", &b.sock, "Stop");
    assert_eq!(b.last("state"), json!({"name": "sendmeter-impl-12", "state": "done"}));
    assert!(
        err.contains("kind = \"berth\" is deprecated, berth is now shed: set kind = \"shed\""),
        "{err}"
    );
    assert!(!hook(&b.sock, "Stop").contains("deprecated"));
    let d = tmpdir("shed-doctor");
    let c = ctx(&d, &kind_config("berth", &b.sock, ""));
    let (_, out) = run(&c, &Fake::default(), &["doctor"]);
    assert!(
        out.contains("! runtime — kind = \"berth\" is deprecated, berth is now shed → set kind = \"shed\" in "),
        "{out}"
    );
    let c = ctx(&tmpdir("shed-doctor-new"), &shed_config(&b.sock, ""));
    assert!(!run(&c, &Fake::default(), &["doctor"]).1.contains("! runtime"));
}

#[test]
fn drive_reads_berths_seq_file_once_when_sheds_is_missing() {
    let ev = json!({"seq": 9, "name": "sendmeter-impl-12", "labels": lane_labels(), "event": "killed"});
    let b = FakeShed::start(shed::proto::VERSION, vec![ev]);
    let d = tmpdir("shed-old-seq");
    let c = ctx(&d, &shed_config(&b.sock, ""));
    std::fs::create_dir_all(c.state.parent().unwrap()).unwrap();
    std::fs::write(c.state.with_file_name("berth.seq"), "8").unwrap();
    let cfg = drover::config::Config::load(&c.config).unwrap();
    assert_eq!(shed::intake(&c, &cfg, &b.shed().wire).unwrap(), 1);
    assert_eq!(b.last("events")["since"], 8);
    assert_eq!(
        std::fs::read_to_string(c.state.with_file_name("shed.seq")).unwrap(),
        "9"
    );
    shed::intake(&c, &cfg, &b.shed().wire).unwrap();
    assert_eq!(b.last("events")["since"], 9, "shed's file wins once it exists");
}

#[test]
fn the_service_follows_sheds_events_and_says_when_the_stream_drops() {
    let ev = |seq: u64, kind: Value| {
        let mut e = json!({"seq": seq, "name": "sendmeter-impl-12", "labels": lane_labels()});
        e.as_object_mut().unwrap().extend(kind.as_object().unwrap().clone());
        e
    };
    let events = vec![
        ev(3, json!({"event": "state", "state": "working"})),
        ev(4, json!({"event": "exited", "code": 137})),
    ];
    let b = FakeShed::start(shed::proto::VERSION, events);
    let d = tmpdir("shed-service");
    let c = ctx(&d, &shed_config(&b.sock, ""));
    std::fs::create_dir_all(c.state.parent().unwrap()).unwrap();
    let api = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let r: &'static Fake = Box::leak(Box::new(Fake::default().reply("herdr", "{}")));
    let sc = c.clone();
    std::thread::spawn(move || drover_service::run(sc, r, || 1_000_000, api));
    let logged = |kind: &str| -> Vec<drover::events::Event> {
        let all = drover::events::read(&c.state).unwrap_or_default();
        all.iter().filter(|e| e.kind == kind).cloned().collect()
    };
    let t0 = std::time::Instant::now();
    while logged("lane-state").len() < 2 {
        assert!(t0.elapsed().as_secs() < 2, "{:?}", logged("lane-state"));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let states: Vec<_> = logged("lane-state")
        .iter()
        .map(|e| (e.issue, e.str("state").map(String::from)))
        .collect();
    assert_eq!(
        states,
        [(Some(12), Some("working".into())), (Some(12), Some("gone".into()))]
    );
    assert_eq!(logged("lane-state")[1].str("why"), Some("exited"));
    assert_eq!(
        std::fs::read_to_string(c.state.with_file_name("shed.seq")).unwrap(),
        "4"
    );
    while !logged("lane-stream").iter().any(|e| e.str("state") == Some("down")) {
        assert!(t0.elapsed().as_secs() < 5, "{:?}", logged("lane-stream"));
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let s: Vec<_> = logged("lane-stream")
        .iter()
        .map(|e| (e.str("runtime").map(String::from), e.str("state").map(String::from)))
        .collect();
    assert_eq!(
        s[..2],
        [
            (Some("shed".into()), Some("live".into())),
            (Some("shed".into()), Some("down".into()))
        ]
    );
}

#[test]
fn the_lanes_tab_lists_a_running_shed_acp_lane_live() {
    let b = FakeShed::new();
    b.add("sendmeter-impl-12", lane_labels());
    let c = ctx(&tmpdir("acp-lanes-tab"), &acp_config(&b.sock, "claude", true));
    let d = json!({"role": "impl", "agent": "sendmeter-impl-12", "harness": "claude", "runtime": "shed",
        "pane": "sendmeter-impl-12", "worktree": "/w/issue-12"});
    drover::events::append(&c.state, &Event::new(999_000, "sendmeter", Some(12), "spawn", d)).unwrap();
    let r: &'static Fake = Box::leak(Box::new(Fake::default().reply("herdr agent list", NO_AGENTS)));
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let sc = c.clone();
    std::thread::spawn(move || drover::serve::serve_on(&sc, r, l, false));
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    write!(s, "GET /api/v1/lanes HTTP/1.1\r\nHost: {addr}\r\n\r\n").unwrap();
    let mut body = String::new();
    std::io::Read::read_to_string(&mut s, &mut body).unwrap();
    let v: Value = serde_json::from_str(body.split_once("\r\n\r\n").expect(&body).1).expect(&body);
    let row = v["sections"]
        .as_array()
        .unwrap_or_else(|| panic!("{v}"))
        .iter()
        .flat_map(|s| {
            s["repos"]
                .as_array()
                .unwrap()
                .iter()
                .map(move |r| (s["name"].clone(), r))
        })
        .flat_map(|(sec, r)| {
            r["lanes"]
                .as_array()
                .unwrap()
                .iter()
                .map(move |l| (sec.clone(), l.clone()))
        })
        .find(|(_, l)| l["name"] == "sendmeter-impl-12")
        .unwrap_or_else(|| panic!("{v}"));
    assert_eq!(
        (row.0.as_str(), row.1["state"].as_str(), row.1["gone"].as_bool()),
        (Some("working"), Some("working"), Some(false)),
        "{v}"
    );
}

struct Lands<'a>(&'a Fake, Terms);

impl Runner for Lands<'_> {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<drover::runner::Out> {
        if prog == "shed" && args[0] == "send" {
            let mut st = self.1.lock().unwrap();
            st.terms.get_mut(&args[1]).unwrap()["text"] = json!(format!("> {}", args[2]));
        }
        self.0.run(prog, args)
    }
    fn sleep(&self, _: u64) {}
}

#[test]
fn an_acp_lane_whose_first_prompt_was_dropped_gets_its_brief_again_once() {
    let b = FakeShed::new();
    let lane = "sendmeter-rev-12-r1";
    b.add(
        lane,
        json!({"fleet": "sendmeter", "issue": "12", "role": "rev", "harness": "claude"}),
    );
    {
        let mut st = b.lock();
        let terms = &mut st.terms;
        let t = terms.get_mut(lane).unwrap();
        (t["acp"], t["reported"], t["text"]) = (json!("claude-acp"), Value::Null, json!(""));
    }
    let c = ctx(&tmpdir("acp-dropped"), &acp_config(&b.sock, "claude", true));
    let head = "a".repeat(40);
    let wt = c.worktrees.join("sendmeter/rev-12-r1");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join(".brief.md"), "# Brief — review #12\n").unwrap();
    let ev = |ts: u64, kind: &str, d: Value| {
        drover::events::append(&c.state, &Event::new(ts, "sendmeter", Some(12), kind, d)).unwrap();
    };
    ev(
        10,
        "spawn",
        json!({"role": "impl", "agent": "sendmeter-impl-12", "branch": "issue-12"}),
    );
    ev(20, "pushed", json!({"sha": head}));
    let t0 = c.now - 60;
    ev(
        t0,
        "spawn",
        json!({"role": "rev", "check": true, "agent": lane, "pane": lane, "worktree": wt, "branch": "rev-12-r1",
            "sha": head, "runtime": "shed", "confirmed": true}),
    );
    let pr = json!({"number": 100, "state": "OPEN", "baseRefName": "staging", "headRefOid": head,
        "mergeable": "MERGEABLE", "statusCheckRollup": []});
    let f = Fake::default()
        .reply("gh pr view issue-12", &pr.to_string())
        .reply("git -C", "")
        .reply("shed send", "");
    let r = Lands(&f, b.state.clone());
    let at = |s: u64| drover::Ctx { now: s, ..c.clone() };
    let sends = || f.count(&format!("shed send {lane}"));
    let drive = |c: &drover::Ctx| {
        let (code, out) = run(c, &r, &["drive"]);
        assert_eq!(code, 0, "{out} {:?}", f.calls.borrow());
        out
    };
    drive(&c);
    assert_eq!(sends(), 0, "inside the start window");
    let out = drive(&at(t0 + 601));
    assert_eq!(sends(), 1, "{out} {:?}", f.calls.borrow());
    assert!(
        f.called(&format!("shed send {lane} # Brief — review #12")),
        "the brief itself"
    );
    let ev = drover::events::read(&c.state).unwrap();
    assert!(
        ev.iter()
            .any(|e| e.kind == "start-resent" && e.str("agent") == Some(lane)),
        "{out}"
    );
    drive(&at(t0 + 1202));
    assert_eq!(sends(), 1);
    let ev = drover::events::read(&c.state).unwrap().to_vec();
    assert!(
        !ev.iter().any(|e| matches!(e.kind.as_str(), "respawn" | "needs-human")),
        "{ev:?}"
    );
}

#[test]
fn the_build_trim_keeps_a_running_shed_lanes_scratch() {
    let b = FakeShed::new();
    b.add("sendmeter-impl-12", lane_labels());
    b.lock().terms.get_mut("sendmeter-impl-12").unwrap()["acp"] = json!("claude-acp");
    let mut gone = lane_labels();
    gone["issue"] = json!("13");
    b.add("sendmeter-impl-13", gone);
    b.lock().terms.get_mut("sendmeter-impl-13").unwrap()["state"] = json!("exited");
    let d = tmpdir("shed-trim");
    let cfg = CONFIG.replace(
        "build_root = \"\"",
        &format!("build_budget_gb = {}", 2000.0 / 1_073_741_824.0),
    );
    let c = ctx(&d, &cfg);
    let root = c.worktrees.join(".build");
    let scratch = |agent: &str, n: u64, at: u64| {
        let w = c.worktrees.join("sendmeter").join(agent);
        std::fs::create_dir_all(&w).unwrap();
        let ev = json!({"role": "impl", "agent": agent, "worktree": w, "pane": agent, "workspace": agent});
        drover::events::append(&c.state, &Event::new(10, "sendmeter", Some(n), "spawn", ev)).unwrap();
        let p = root.join(".scratch").join(agent);
        std::fs::create_dir_all(p.join("target")).unwrap();
        std::fs::write(p.join("target/blob"), vec![0u8; 4096]).unwrap();
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(at);
        for f in [p.join("target/blob"), p.join("target"), p.clone()] {
            std::fs::File::open(&f).unwrap().set_modified(at).unwrap();
        }
        p
    };
    let (live, exited) = (
        scratch("sendmeter-impl-12", 12, 1_000),
        scratch("sendmeter-impl-13", 13, 2_000),
    );
    let profile = root.join("sendmeter-impl-12/release");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join(".cargo-lock"), "").unwrap();
    std::fs::write(profile.join("blob"), vec![0u8; 4096]).unwrap();
    std::fs::File::open(&profile)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(500))
        .unwrap();
    let f = Fake::default().reply("herdr agent list", NO_AGENTS);
    let r = shed::With(&f, b.shed());
    let mut out = vec![];
    let cfg = drover::config::Config::load(&c.config).unwrap();
    drover::check::budget(&c, &cfg, &r, &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(profile.exists(), "a running lane's own build is never trimmed: {out}");
    assert!(
        live.exists(),
        "a running ACP lane's scratch is never trimmed, oldest or not: {out}"
    );
    assert!(!exited.exists(), "an exited lane's goes over budget: {out}");
    let line = format!("builds: trimmed {} ", exited.display());
    assert!(out.contains(&line) && out.contains("no live agent"), "{out}");
}

fn git(dir: &std::path::Path, a: &[&str]) -> String {
    let o = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "init.defaultBranch=main",
            "-C",
        ])
        .arg(dir)
        .args(a)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {a:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

struct Push<'a>(fake_shed::Cli<'a>, PathBuf);

impl Runner for Push<'_> {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<drover::runner::Out> {
        if prog == "git" && args.len() > 1 && args[0] == "-C" && std::path::Path::new(&args[1]).starts_with(&self.1) {
            self.0.f.calls.borrow_mut().push(format!("git {}", args.join(" ")));
            let o = std::process::Command::new("git").args(args).output()?;
            let s = |b: Vec<u8>| String::from_utf8_lossy(&b).into_owned();
            return Ok(drover::runner::Out {
                code: o.status.code().unwrap_or(1),
                stdout: s(o.stdout),
                stderr: s(o.stderr),
            });
        }
        self.0.run(prog, args)
    }
    fn sleep(&self, _: u64) {}
}

fn acp_lane(name: &str) -> (drover::Ctx, FakeShed, Fake, PathBuf, PathBuf) {
    std::env::set_var("DROVER_LANE", "sendmeter:12:impl");
    let b = FakeShed::new();
    let cfg =
        acp_config(&b.sock, "claude", true).replace("account = \"C\"", "accounts = [\"C\", \"D\"]") + "D = \"D\"\n";
    let c = acp_ctx(name, &cfg);
    let (wt, origin) = (c.worktrees.join("sendmeter/issue-12"), c.worktrees.join("origin.git"));
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::create_dir_all(&origin).unwrap();
    git(&origin, &["init", "-q", "--bare"]);
    git(&wt, &["init", "-q", "-b", "issue-12"]);
    git(&wt, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(&wt, &["commit", "-q", "--allow-empty", "-m", "base"]);
    git(&wt, &["push", "-q", "origin", "issue-12"]);
    let f = acp_fake(&["true"])
        .reply("sysctl -n kern.memorystatus_vm_pressure_level", "1\n")
        .reply("sysctl -n vm.loadavg", "{ 1.50 1.20 1.00 }\n")
        .reply("sysctl -n hw.ncpu", "10\n")
        .reply("vm_stat", "Pages free: 1.\nSwapouts:                     100.\n");
    let (code, out) = run(&c, &Push(b.cli(&f), wt.clone()), &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 0, "{out}");
    (c, b, f, wt, origin)
}

fn exits(b: &FakeShed, kinds: &[Value]) {
    b.lock().terms.get_mut("sendmeter-impl-12").unwrap()["state"] = json!("exited");
    let mut st = b.lock();
    let ev = &mut st.events;
    let start = ev.len() as u64 * 10;
    let all = [&[json!({"event": "turn", "prompt": "# Brief"})][..], kinds].concat();
    for (i, k) in all.iter().enumerate() {
        let mut e = json!({"seq": start + i as u64 + 1, "name": "sendmeter-impl-12", "labels": lane_labels()});
        e.as_object_mut().unwrap().extend(k.as_object().unwrap().clone());
        ev.push(e);
    }
}

fn later(c: &drover::Ctx, secs: u64) -> drover::Ctx {
    drover::Ctx {
        now: c.now + secs,
        ..c.clone()
    }
}

fn logged(c: &drover::Ctx, kind: &str) -> Vec<Event> {
    let all = drover::events::read(&c.state).unwrap().to_vec();
    all.into_iter().filter(|e| e.kind == kind).collect()
}

#[test]
fn an_acp_lane_that_exits_on_an_account_limit_is_pushed_and_resumed_in_its_worktree_on_another_account() {
    let (c, b, f, wt, origin) = acp_lane("acp-resume");
    assert_eq!(logged(&c, "spawn")[0].str("account"), Some("C"));
    for m in ["test", "fix"] {
        git(&wt, &["commit", "-q", "--allow-empty", "-m", m]);
    }
    std::fs::write(wt.join("more.rs"), "staged").unwrap();
    git(&wt, &["add", "more.rs"]);
    let head = git(&wt, &["rev-parse", "HEAD"]);
    let limit = "Claude AI usage limit reached ∙ resets 5pm";
    exits(
        &b,
        &[
            json!({"event": "text", "text": limit}),
            json!({"event": "exited", "code": 0}),
        ],
    );
    let cli = Push(b.cli(&f), wt.clone());
    let (code, out) = run(&later(&c, 300), &cli, &["drive"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(git(&origin, &["rev-parse", "issue-12"]), head, "{out}");
    assert!(git(&origin, &["ls-tree", "--name-only", "issue-12"]).is_empty());
    assert_eq!(
        git(&wt, &["diff", "--cached", "--name-only"]),
        "more.rs",
        "the staged change is kept"
    );
    let sp = logged(&c, "spawn");
    assert_eq!(sp.len(), 2, "{out}");
    assert_eq!(sp[1].str("account"), Some("D"), "{out}");
    assert_eq!(sp[1].str("worktree"), wt.to_str());
    let brief = std::fs::read_to_string(wt.join(".brief.md")).unwrap();
    assert!(brief.contains("local commits and staged changes are kept"), "{brief}");
    for i in 1..=3 {
        exits(&b, &[json!({"event": "exited", "code": 1})]);
        let (code, out) = run(&later(&c, 300 + i * 1900), &cli, &["drive"]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(logged(&c, "spawn").len(), 2 + i as usize, "{out}");
    }
    assert!(logged(&c, "needs-human").is_empty());
    let (_, st) = run(&later(&c, 6000), &cli, &["status", "sendmeter"]);
    assert!(!st.contains("needs-human"), "{st}");
}

#[test]
fn lane_dead_and_waiting_carry_an_acp_lanes_exit_cause() {
    let (c, b, f, wt, _) = acp_lane("acp-cause");
    exits(&b, &[json!({"event": "exited", "code": 139})]);
    let cfg = drover::config::Config::load(&c.config).unwrap();
    let c = later(&c, 300);
    shed::intake(&c, &cfg, &cfg.shed().unwrap().wire).unwrap();
    let cli = Push(b.cli(&f), wt.clone());
    let (code, out) = run(&c, &cli, &["check"]);
    assert_eq!(code, 0, "{out}");
    let dead = logged(&c, "lane-dead");
    assert_eq!(dead.len(), 1, "{out}");
    assert_eq!(dead[0].str("cause"), Some("adapter crash"));
    assert!(dead[0].str("reason").unwrap().contains("adapter crash"), "{dead:?}");
    let (_, w) = run(&c, &cli, &["waiting", "sendmeter"]);
    assert!(w.contains("lane gone (adapter crash), no push: respawning impl"), "{w}");
}

fn thread_events(c: &drover::Ctx, kind: &str) -> Vec<Event> {
    let all = drover::events::read(&c.state).unwrap().to_vec();
    all.into_iter().filter(|e| e.kind == kind).collect()
}

fn at(c: &drover::Ctx, now: u64) -> drover::Ctx {
    drover::Ctx { now, ..c.clone() }
}

#[test]
fn a_shed_acp_fleets_thread_runs_its_orchestrator_as_an_acp_session_that_sleeps_and_restarts() {
    let b = FakeShed::new();
    let c = acp_ctx("acp-orchestrator", &acp_config(&b.sock, "claude", true));
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Grill #12"]).1,
        "t1\n"
    );
    let f = acp_fake(&["echo", "claude-agent-acp"]);
    let cli = b.cli(&f);
    let (code, out) = run(&c, &cli, &["thread", "say", "t1", "what should #12 cover?"]);
    assert_eq!(
        (code, out.as_str()),
        (0, "t1: -> orchestrator-t1\n"),
        "{:?}",
        f.calls.borrow()
    );
    let dir = c.state.with_file_name("orchestrator").join("t1");
    let calls = f.calls.borrow().clone();
    let start = calls.iter().find(|c| c.starts_with("shed run")).unwrap();
    let head = format!(
        "shed run --acp claude-acp --policy allow --name orchestrator-t1 --cwd {} --label fleet=sendmeter --label issue=- \
         --label role=orchestrator --label harness=claude --label source=drover --label group=sendmeter --label thread=t1 \
         -- sh -c ",
        dir.display()
    );
    assert!(start.starts_with(&head), "{start}");
    let herdr = |c: &&String| c.starts_with("herdr") && *c != "herdr agent list";
    assert!(!calls.iter().any(|c| herdr(&c)), "no herdr pane: {calls:?}");
    let brief = std::fs::read_to_string(dir.join(".brief.md")).unwrap();
    let thread = dir.join(".thread.md");
    let first = format!(
        "shed send orchestrator-t1 {}\n\nThe thread so far is in {}, the last message new.\n\n[t1] owner: what should #12 cover?",
        brief.trim_end(),
        thread.display()
    );
    let sends: Vec<_> = calls.iter().filter(|c| c.starts_with("shed send")).collect();
    assert_eq!(sends, [&first]);
    assert!(brief.contains("Thread: t1, Grill #12"));
    let st = &thread_events(&c, "orchestrator-start")[0];
    assert_eq!(
        (st.str("runtime"), st.str("pane")),
        (Some("shed-acp"), Some("orchestrator-t1"))
    );
    assert_eq!(*st.val("confirmed"), json!(true));

    let f = acp_fake(&["echo"]);
    let cli = b.cli(&f);
    assert_eq!(
        run(&at(&c, c.now + 60), &cli, &["thread", "say", "t1", "scope\nthen tests"]).0,
        0
    );
    assert_eq!(
        f.calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("shed"))
            .collect::<Vec<_>>(),
        [&"shed send orchestrator-t1 [t1] owner: scope\nthen tests".to_string()]
    );
    assert!(
        !b.methods().iter().any(|m| m == "send" || m == "keys"),
        "{:?}",
        b.methods()
    );

    let quiet = at(&c, c.now + 60 + 31 * 60);
    assert_eq!(run(&quiet, &cli, &["check"]).0, 0);
    assert_eq!(
        b.methods().iter().filter(|m| *m == "kill").count(),
        1,
        "{:?}",
        b.methods()
    );
    assert!(!b.lock().terms.contains_key("orchestrator-t1"));
    let sl = thread_events(&c, "orchestrator-sleep");
    assert_eq!(sl.iter().map(|e| e.str("why")).collect::<Vec<_>>(), [Some("idle")]);
    let (_, ls) = run(&quiet, &Fake::default(), &["thread", "ls", "--json"]);
    assert!(ls.contains("\"state\": \"asleep\""), "{ls}");

    let f = acp_fake(&["echo"]);
    let cli = b.cli(&f);
    assert_eq!(
        run(&at(&c, quiet.now + 60), &cli, &["thread", "say", "t1", "third"]).0,
        0
    );
    assert_eq!(f.count("shed run --acp claude-acp"), 1);
    let sends: Vec<_> = f
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("shed send"))
        .cloned()
        .collect();
    assert!(sends.len() == 1 && sends[0].ends_with("[t1] owner: third"), "{sends:?}");
    let history = std::fs::read_to_string(&thread).unwrap();
    assert!(
        history.contains("owner: scope\nthen tests") && history.ends_with("owner: third\n"),
        "{history}"
    );
    let st = thread_events(&c, "orchestrator-start");
    assert_eq!((st.len(), st[1].val("resumed").clone()), (2, json!(false)));

    b.lock().terms.remove("orchestrator-t1");
    assert_eq!(run(&at(&c, quiet.now + 120), &cli, &["check"]).0, 0);
    let sl = thread_events(&c, "orchestrator-sleep");
    assert_eq!(sl.last().and_then(|e| e.str("why")), Some("gone"));
}

#[test]
fn an_acp_orchestrator_runs_under_the_orchestrator_guard() {
    let b = FakeShed::new();
    let c = acp_ctx("acp-orchestrator-guard", &acp_config(&b.sock, "claude", true));
    run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Grill"]);
    let ef = acp_fake(&["env"]);
    let cli = b.cli(&ef);
    assert_eq!(run(&c, &cli, &["thread", "say", "t1", "hi"]).0, 0);
    let dump = cli.out.borrow().concat();
    let env: BTreeMap<&str, &str> = dump.lines().filter_map(|l| l.split_once('=')).collect();
    assert_eq!(env.get("DROVER_LANE"), Some(&"sendmeter:-:orchestrator"));
    assert_eq!(env.get("DROVER_THREAD"), Some(&"t1"));
    assert!(!env.contains_key("CLAUDE_CODE_OAUTH_TOKEN"), "{env:?}");
    assert_eq!(env.get("CLAUDE_CONFIG_DIR"), Some(&acct_c().as_str()));
    let cwd = c.state.with_file_name("orchestrator").join("t1");
    let local = std::fs::read_to_string(cwd.join(".claude/settings.local.json")).unwrap();
    let s: Value = serde_json::from_str(&local).unwrap();
    let hook = s["hooks"]["PreToolUse"][0]["hooks"][0]["command"].as_str().unwrap();
    let tool = |cmd: &str| {
        let mut ch = std::process::Command::new("sh")
            .args(["-c", hook])
            .env_clear()
            .envs(env.iter().map(|(k, v)| (*k, *v)))
            .env("FLEET_CONFIG", &c.config)
            .env("FLEET_STATE", &c.state)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        ch.stdin
            .take()
            .unwrap()
            .write_all(bash_call(cmd, &cwd).as_bytes())
            .unwrap();
        let o = ch.wait_with_output().unwrap();
        assert!(o.status.success());
        String::from_utf8(o.stdout).unwrap()
    };
    let o = tool("drover card approve c1 --sha abc");
    let v: Value = serde_json::from_str(&o).unwrap_or_else(|_| panic!("{o}"));
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny", "{o}");
    assert!(o.contains("REFUSED OWNER_ONLY"), "{o}");
    assert_eq!(
        tool("drover amend sendmeter 12 'x' --basis q1"),
        "",
        "an orchestrator amends"
    );
}

#[test]
fn an_unguardable_acp_orchestrator_refuses_with_a_system_line_and_never_falls_back_to_herdr() {
    let b = FakeShed::new();
    let c = ctx(
        &tmpdir("acp-orchestrator-nohook"),
        &acp_config(&b.sock, "claude", false),
    );
    run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Grill"]);
    let f = acp_fake(&["env"]);
    let cli = b.cli(&f);
    let (code, out) = run(&c, &cli, &["thread", "say", "t1", "hi"]);
    assert_eq!(code, 3, "{out}");
    assert!(
        out.contains("REFUSED ACP_UNGUARDED") && out.contains("no drover PreToolUse hook"),
        "{out}"
    );
    let calls = f.calls.borrow();
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("shed run") || c.starts_with("herdr w") || c.starts_with("herdr agent s")),
        "{calls:?}"
    );
    let msgs = thread_events(&c, "thread-msg");
    let sys: Vec<_> = msgs.iter().filter(|e| e.str("from") == Some("system")).collect();
    assert_eq!(sys.len(), 1, "{msgs:?}");
    assert!(sys[0].str("text").unwrap().contains("ACP_UNGUARDED"), "{:?}", sys[0]);
    assert!(thread_events(&c, "orchestrator-start").is_empty());
}

fn board_get(c: &drover::Ctx, r: Fake, path: &str) -> Value {
    let r: &'static Fake = Box::leak(Box::new(r));
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let sc = c.clone();
    std::thread::spawn(move || drover::serve::serve_on(&sc, r, l, false));
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n").unwrap();
    let mut body = String::new();
    std::io::Read::read_to_string(&mut s, &mut body).unwrap();
    serde_json::from_str(body.split_once("\r\n\r\n").expect(&body).1).expect(&body)
}

fn wheres(c: &drover::Ctx, r: impl Fn() -> Fake) -> BTreeMap<String, (String, String, String)> {
    let s = |v: &Value| v.as_str().unwrap_or("?").to_string();
    let lanes = board_get(c, r(), "/api/v1/lanes");
    let rows = lanes["sections"].as_array().unwrap_or_else(|| panic!("{lanes}"));
    let rows = rows.iter().flat_map(|s| {
        s["repos"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|r| r["lanes"].as_array().unwrap())
    });
    let got: BTreeMap<_, _> = rows
        .map(|l| (s(&l["name"]), (s(&l["runtime"]), s(&l["host"]), s(&l["where"]))))
        .collect();
    let state = board_get(c, r(), "/api/v1/state");
    for l in state["lanes"].as_array().unwrap() {
        assert_eq!(
            got[&s(&l["name"])],
            (s(&l["runtime"]), s(&l["host"]), s(&l["where"])),
            "{state}"
        );
    }
    let herd = board_get(c, r(), "/herd.json");
    for l in herd["fleets"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|f| f["lanes"].as_array().unwrap())
    {
        assert_eq!(
            (s(&l["runtime"]), s(&l["where"])),
            (got[&s(&l["name"])].0.clone(), got[&s(&l["name"])].2.clone())
        );
    }
    got
}

#[test]
fn every_lane_says_where_it_runs() {
    let b = FakeShed::new();
    b.add("sendmeter-impl-12", lane_labels());
    let hosts = |air: bool| format!("\n[hosts.mini]\n\n[hosts.air]\nmachine = \"air\"\nenabled = {air}\n");
    let c = ctx(
        &tmpdir("where-lanes"),
        &(acp_config(&b.sock, "claude", true) + &hosts(true)),
    );
    let ev =
        |n, d: Value| drover::events::append(&c.state, &Event::new(999_000, "sendmeter", Some(n), "spawn", d)).unwrap();
    ev(
        12,
        json!({"role": "impl", "agent": "sendmeter-impl-12", "harness": "claude", "runtime": "shed-acp",
        "pane": "sendmeter-impl-12", "worktree": "/w/issue-12", "host": "mini"}),
    );
    ev(
        9,
        json!({"role": "rev", "agent": "sendmeter-rev-9", "harness": "claude", "pane": "w9:p1",
        "worktree": "/w/issue-9", "host": "air", "machine": "air"}),
    );
    let ssh = "ssh -oBatchMode=yes -oConnectTimeout=10 -oControlMaster=auto -oControlPath=/tmp/drover-ssh-%C \
               -oControlPersist=10m air -- 'shed' 'connect'";
    let ls = |terms: Value| {
        let hello = json!({"jsonrpc": "2.0", "id": 0, "result": {"product": "shed", "version": shed::proto::VERSION}});
        format!("{hello}\n{}\n", json!({"jsonrpc": "2.0", "id": 1, "result": terms}))
    };
    let rev9 = json!([{"name": "sendmeter-rev-9", "labels": {"fleet": "sendmeter", "harness": "claude"},
        "cwd": "/w/issue-9", "cmd": ["sh"], "keep": true, "state": "running", "exit": null, "pid": 8,
        "reported": {"state": "working"}}]);
    let both = || Fake::default().reply(ssh, &ls(rev9.clone()));
    let shed_only = || Fake::default().reply(ssh, &ls(json!([])));
    let t = |rt: &str, h: &str, w: &str| (rt.to_string(), h.to_string(), w.to_string());
    let (rev, imp) = ("sendmeter-rev-9", "sendmeter-impl-12");
    let mut shapes = vec![];

    let got = wheres(&c, both);
    assert_eq!(
        (&got[rev], &got[imp]),
        (&t("shed", "air", "air"), &t("shed-acp", "mini", "mini"))
    );
    shapes.push(json!(got));
    let q = board_get(&c, both(), "/api/v1/queue");
    let rows: Vec<_> = q["fleets"][0]["rows"]
        .as_array()
        .unwrap_or_else(|| panic!("{q}"))
        .iter()
        .map(|r| {
            (
                r["issue"].as_u64().unwrap(),
                t(
                    r["runtime"].as_str().unwrap(),
                    r["host"].as_str().unwrap(),
                    r["where"].as_str().unwrap(),
                ),
            )
        })
        .collect();
    assert!(rows.contains(&(9, t("shed", "air", "air"))), "{rows:?}");
    assert!(rows.contains(&(12, t("shed-acp", "mini", "mini"))), "{rows:?}");

    std::fs::write(&c.config, acp_config(&b.sock, "claude", true) + &hosts(false)).unwrap();
    let got = wheres(&c, both);
    assert_eq!((&got[rev].2, &got[imp].2), (&String::new(), &String::new()));
    shapes.push(json!(got));

    std::fs::write(&c.config, acp_config(&b.sock, "claude", true) + &hosts(true)).unwrap();
    let got = wheres(&c, shed_only);
    assert_eq!(got[imp], t("shed-acp", "mini", "mini"));
    shapes.push(json!(got));

    std::fs::write(&c.config, acp_config(&b.sock, "claude", true) + &hosts(false)).unwrap();
    let got = wheres(&c, shed_only);
    assert_eq!(got[imp], t("shed-acp", "mini", ""));
    shapes.push(json!(got));

    std::fs::write(&c.config, acp_config(&b.sock, "claude", true) + &hosts(true)).unwrap();
    for cmd in ["status", "board", "waiting"] {
        let (code, out) = run(&c, &both(), &[cmd, "sendmeter"]);
        assert!(code == 0 && out.contains("mini") && out.contains("air"), "{cmd}: {out}");
    }

    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/api_v1/where.json");
    let fresh = serde_json::to_string_pretty(
        &json!({"route": "GET /api/v1/lanes, state, queue, herd: each lane's [runtime, host, where]",
        "shapes": shapes}),
    )
    .unwrap()
        + "\n";
    if std::env::var_os("BLESS").is_some() {
        std::fs::write(&file, &fresh).unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(&file).unwrap_or_default(),
        fresh,
        "`BLESS=1` rewrites it"
    );
}

#[test]
fn a_lane_keeps_its_spawn_runtime_after_the_config_changes() {
    let b = FakeShed::new();
    b.add("sendmeter-impl-12", lane_labels());
    let c = ctx(&tmpdir("acp-keeps-runtime"), &acp_config(&b.sock, "claude", true));
    let d = json!({"role": "impl", "agent": "sendmeter-impl-12", "harness": "claude", "runtime": "shed-acp",
        "pane": "sendmeter-impl-12", "worktree": "/w/issue-12"});
    drover::events::append(&c.state, &Event::new(999_000, "sendmeter", Some(12), "spawn", d)).unwrap();
    let r = Fake::default;
    assert_eq!(wheres(&c, r)["sendmeter-impl-12"].0, "shed-acp");
    std::fs::write(
        &c.config,
        b.config(&CONFIG.replace("harness  =  \"hermes\"", "harness = \"claude\"")),
    )
    .unwrap();
    let cfg = drover::config::Config::load(&c.config).unwrap();
    assert!(
        cfg.shed_for("sendmeter").unwrap().acp.is_none(),
        "the fleet's new lanes start in a shed terminal"
    );
    let got = wheres(&c, r);
    assert_eq!(got["sendmeter-impl-12"].0, "shed-acp", "{got:?}");
}

fn herdr_era_lane(
    name: &str,
    b: &FakeShed,
    fix: bool,
    live: &[&str],
) -> (drover::Ctx, Fake, PathBuf, PathBuf, PathBuf) {
    let d = std::fs::canonicalize(tmpdir(name)).unwrap();
    let (local, old, origin) = (
        d.join("sendmeter"),
        d.join("herdr/sendmeter/issue-12"),
        d.join("origin.git"),
    );
    for p in [&local, &origin] {
        std::fs::create_dir_all(p).unwrap();
    }
    git(&origin, &["init", "-q", "--bare"]);
    git(&local, &["init", "-q", "-b", "staging"]);
    git(&local, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(&local, &["commit", "-q", "--allow-empty", "-m", "base"]);
    git(&local, &["push", "-q", "origin", "staging"]);
    git(
        &local,
        &["worktree", "add", "-q", "-b", "issue-12", old.to_str().unwrap()],
    );
    git(&old, &["commit", "-q", "--allow-empty", "-m", "impl"]);
    git(&old, &["push", "-q", "origin", "issue-12"]);
    let cfg = shed_config(&b.sock, "").replace("/tmp/does-not-matter/sendmeter", local.to_str().unwrap());
    let c = ctx(&d, &cfg);
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    let sp = json!({"role": "impl", "agent": "sendmeter-impl-12", "harness": "claude", "pane": "w1:p1",
        "workspace": "w1", "worktree": old, "branch": "issue-12"});
    let rev = json!({"role": "rev", "agent": "sendmeter-rev-12", "harness": "claude", "pane": "w2:p1",
        "workspace": "w2", "worktree": d.join("herdr/sendmeter/rev-12-r1"), "branch": "rev-12-r1"});
    for (i, (kind, data)) in [
        ("spawn", sp),
        ("spawn", rev),
        ("verdict", json!({"verdict": "FAIL", "sha": "1".repeat(40)})),
    ]
    .into_iter()
    .enumerate()
    .filter(|_| fix)
    {
        let e = Event::new(c.now - 900 + i as u64 * 60, "sendmeter", Some(12), kind, data);
        drover::events::append(&c.state, &e).unwrap();
    }
    for n in live {
        b.lane(n, "working", old.to_str().unwrap());
    }
    let f = Fake::default()
        .reply("gh issue view 12", r#"{"title":"Fix the meter","body":"AC: it works"}"#)
        .reply("which shed", "/opt/shed/bin/shed\n");
    (c, f, local, old, origin)
}

fn worktrees_of(local: &std::path::Path) -> String {
    git(local, &["worktree", "list", "--porcelain"])
}

#[test]
fn a_fix_round_moves_a_clean_herdr_era_worktree_to_the_new_root() {
    let b = FakeShed::new();
    let (c, f, local, old, _) = herdr_era_lane("moved-clean", &b, true, &[]);
    let cli = Push(b.cli(&f), local.parent().unwrap().into());
    let (code, out) = run(&c, &cli, &["spawn", "sendmeter", "12", "impl", "--supersede"]);
    assert_eq!(code, 0, "{out} {:?}", f.calls.borrow());
    let wt = c.worktrees.join("sendmeter/issue-12");
    assert_eq!(git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]), "issue-12");
    assert!(!old.exists(), "the old worktree is gone");
    assert!(
        !worktrees_of(&local).contains(old.to_str().unwrap()),
        "{}",
        worktrees_of(&local)
    );
    let moved = logged(&c, "worktree-moved");
    assert_eq!(moved.len(), 1, "{out}");
    assert_eq!(moved[0].str("old"), old.to_str());
    assert_eq!(*moved[0].val("wip"), Value::Null);
}

#[test]
fn a_herdr_era_worktrees_unpushed_commit_is_saved_to_wip_before_it_moves() {
    let b = FakeShed::new();
    let (c, f, local, old, origin) = herdr_era_lane("moved-unpushed", &b, false, &[]);
    git(&old, &["commit", "-q", "--allow-empty", "-m", "unpushed"]);
    let commit = git(&old, &["rev-parse", "HEAD"]);
    let cli = Push(b.cli(&f), local.parent().unwrap().into());
    let (code, out) = run(&c, &cli, &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 0, "{out} {:?}", f.calls.borrow());
    let moved = logged(&c, "worktree-moved");
    assert_eq!(moved.len(), 1, "{out}");
    let w = moved[0].str("wip").expect("saved to wip").to_string();
    assert!(w.starts_with("wip/sendmeter-impl-12-"), "{w}");
    git(&origin, &["merge-base", "--is-ancestor", &commit, &w]);
    assert!(!old.exists(), "the old worktree is gone");
    let brief = std::fs::read_to_string(c.worktrees.join("sendmeter/issue-12/.brief.md")).unwrap();
    assert!(brief.contains(&format!("origin/{w}")), "{brief}");
}

#[test]
fn a_live_agent_in_the_herdr_era_worktree_refuses_lane_live() {
    let b = FakeShed::new();
    let (c, f, local, old, _) = herdr_era_lane("moved-live", &b, false, &["shell"]);
    let cli = Push(b.cli(&f), local.parent().unwrap().into());
    let (code, out) = run(&c, &cli, &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 3, "{out}");
    assert!(
        out.contains("LANE_LIVE") && out.contains(old.to_str().unwrap()),
        "{out}"
    );
    assert!(old.exists() && !c.worktrees.join("sendmeter/issue-12").exists());
    assert!(logged(&c, "worktree-moved").is_empty());
}

#[test]
fn a_hook_synced_through_a_symlink_guards_a_spawn_through_the_real_path() {
    let b = FakeShed::new();
    let real = std::fs::canonicalize(env!("CARGO_BIN_EXE_drover")).unwrap();
    for (name, sync_via_link) in [("acp-link-sync", true), ("acp-real-sync", false)] {
        let dir = tmpdir(name);
        let link = dir.join("bin/drover");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let (sync, spawn) = if sync_via_link { (&link, &real) } else { (&real, &link) };
        let mut c = ctx(&dir, &acp_config(&b.sock, "claude", true));
        c.exe = sync.display().to_string();
        assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
        c.exe = spawn.display().to_string();
        let f = acp_fake(&["env"]);
        let cli = b.cli(&f);
        let (code, out) = run(&c, &cli, &["spawn", "sendmeter", "12", "impl", "--dry-run"]);
        assert!(code == 0 && !out.contains("ACP_UNGUARDED"), "{name}: {code} {out}");
    }
}

#[test]
fn a_hook_naming_another_binary_refuses_acp_unguarded() {
    let b = FakeShed::new();
    let dir = tmpdir("acp-other-bin");
    let other = dir.join("other/drover");
    std::fs::create_dir_all(other.parent().unwrap()).unwrap();
    std::fs::write(&other, "").unwrap();
    let mut c = ctx(&dir, &acp_config(&b.sock, "claude", true));
    c.exe = other.display().to_string();
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    c.exe = env!("CARGO_BIN_EXE_drover").into();
    let f = acp_fake(&["env"]);
    let cli = b.cli(&f);
    let (code, out) = run(&c, &cli, &["spawn", "sendmeter", "12", "impl", "--dry-run"]);
    assert!(code == 3 && out.contains("REFUSED ACP_UNGUARDED"), "{out}");
}

#[test]
fn a_codex_rev_and_a_claude_impl_spawn_with_their_own_agent_and_login() {
    let b = FakeShed::new();
    let cfg = acp_config(&b.sock, "claude", true)
        .replace(
            "[fleet.sendmeter.role.rev]\nharness = \"hermes\"",
            "[fleet.sendmeter.role.rev]\nagent = \"codex-acp\"\naccount = \"X\"\nmodel = \"gpt-5.5\"\neffort = \"high\"",
        )
        .replace("C = \"C\"\n", "C = \"C\"\nX = \"X\"\n");
    let c = acp_ctx("acp-codex", &cfg);
    let agents = json!([{"id": "claude-acp", "cmd": ["env"]}, {"id": "codex-acp", "cmd": ["env"]}]);
    let head = "a".repeat(40);
    let pr = json!({"number": 100, "state": "OPEN", "baseRefName": "staging", "headRefOid": head,
        "mergeable": "MERGEABLE", "statusCheckRollup": []});
    let f = spawn_fake()
        .reply("shed agents --json", &agents.to_string())
        .reply("shed run", "")
        .reply("shed send", "")
        .reply("gh pr view", &pr.to_string());
    let cli = b.cli(&f);
    let (code, out) = run(&c, &cli, &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 0, "{out}");
    let ev = |kind: &str, d: Value| {
        drover::events::append(&c.state, &Event::new(c.now, "sendmeter", Some(12), kind, d)).unwrap();
    };
    ev("pushed", json!({"sha": head}));
    ev("turn-end", json!({"state": "done", "agent": "sendmeter-impl-12"}));
    let (code, out) = run(&c, &cli, &["spawn", "sendmeter", "12", "rev"]);
    assert_eq!(code, 0, "{out} {:?}", f.calls.borrow());
    let calls = f.calls.borrow();
    let runs: Vec<_> = calls.iter().filter(|c| c.starts_with("shed run")).collect();
    assert!(
        runs[0].contains("--acp claude-acp ") && runs[0].contains("--label role=impl "),
        "{runs:?}"
    );
    assert!(
        runs[1].contains("--acp codex-acp ") && runs[1].contains("--label role=rev "),
        "{runs:?}"
    );
    assert!(runs[1].contains("--label harness=codex "), "{runs:?}");
    let dumps = cli.out.borrow();
    let env = |i: usize| -> BTreeMap<String, String> {
        let kv = dumps[i].lines().filter_map(|l| l.split_once('='));
        kv.map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    let (imp, rev) = (env(0), env(1));
    let acct = |n: &str| acct_c().replace("accounts/C", &format!("accounts/{n}"));
    assert_eq!(imp.get("CLAUDE_CONFIG_DIR"), Some(&acct("C")));
    assert!(!imp.contains_key("CODEX_HOME"), "{imp:?}");
    assert_eq!(rev.get("CODEX_HOME"), Some(&acct("X")), "{rev:?}");
    assert!(!rev.contains_key("CLAUDE_CONFIG_DIR"), "{rev:?}");
    assert_eq!(rev.get("DROVER_LANE").map(String::as_str), Some("sendmeter:12:rev"));
    let codex: Value = serde_json::from_str(&rev["CODEX_CONFIG"]).unwrap();
    assert_eq!(
        (&codex["model"], &codex["model_reasoning_effort"]),
        (&json!("gpt-5.5"), &json!("high"))
    );
    let wt = PathBuf::from(&rev["PWD"]);
    assert!(std::fs::read_to_string(wt.join("AGENTS.override.md"))
        .unwrap()
        .contains("drover"));
    let hook = codex["hooks"]["PreToolUse"][0]["hooks"][0]["command"].as_str().unwrap();
    assert!(hook.ends_with("hook --role rev"), "{hook}");
    let tool = |call: Value| {
        let mut ch = std::process::Command::new("sh")
            .args(["-c", hook])
            .env_clear()
            .envs(rev.iter())
            .env("FLEET_CONFIG", &c.config)
            .env("FLEET_STATE", &c.state)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        ch.stdin.take().unwrap().write_all(call.to_string().as_bytes()).unwrap();
        String::from_utf8(ch.wait_with_output().unwrap().stdout).unwrap()
    };
    let call = |tool: &str, cmd: &str| {
        json!({"hook_event_name": "PreToolUse", "turn_id": "t1", "model": "gpt-5.5", "cwd": wt,
            "tool_name": tool, "tool_input": {"command": cmd}})
    };
    let patch = "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-a\n+b\n*** End Patch";
    for (t, cmd, why) in [
        ("Bash", "gh pr merge 3", "merge"),
        ("Bash", "git push origin staging", "staging"),
        ("apply_patch", patch, "reviewers are read-only"),
    ] {
        let o = tool(call(t, cmd));
        let v: Value = serde_json::from_str(&o).unwrap_or_else(|_| panic!("{cmd}: {o}"));
        let d = &v["hookSpecificOutput"];
        assert_eq!(d["permissionDecision"], "deny", "{cmd}: {o}");
        assert!(
            d["permissionDecisionReason"].as_str().unwrap().contains(why),
            "{cmd}: {o}"
        );
    }
    assert_eq!(tool(call("Bash", "cargo test")), "", "an ordinary command runs");
}

#[test]
fn a_rev_on_the_impls_provider_refuses_to_load() {
    let sock = std::path::Path::new("/nonexistent/shed.sock");
    let base = acp_config(sock, "claude", false);
    let rev = |agent: &str| {
        base.replace(
            "[fleet.sendmeter.role.rev]\nharness = \"hermes\"",
            &format!("[fleet.sendmeter.role.rev]\nagent = \"{agent}\""),
        )
    };
    let load = |t: &str| drover::config::Config::parse(t).map_err(|e| format!("{e:#}"));
    let err = load(&rev("claude-acp")).unwrap_err();
    assert!(
        err.contains("REVIEW_SAME_PROVIDER") && err.contains("anthropic"),
        "{err}"
    );
    let imp = base.replace(
        "harness = \"claude\"\nprofile = \"fleet-impl\"",
        "agent = \"claude-acp\"\nprofile = \"fleet-impl\"",
    );
    let imp = imp.replace(
        "[fleet.sendmeter.role.rev]\nharness = \"hermes\"",
        "[fleet.sendmeter.role.rev]\nharness = \"claude\"",
    );
    assert!(load(&imp).unwrap_err().contains("REVIEW_SAME_PROVIDER"));
    let opt = |line: &str| {
        rev("claude-acp").replace(
            "integration = \"staging\"   # never main",
            &format!("integration = \"staging\"\n{line}"),
        )
    };
    assert!(
        load(&opt("rev_same_provider = true"))
            .unwrap_err()
            .contains("REVIEW_SAME_PROVIDER"),
        "no reason given"
    );
    assert!(load(&opt("rev_same_provider = true # one provider for now (q911)")).is_ok());
    let ok = load(&rev("codex-acp")).unwrap();
    assert_eq!(ok.fleet["sendmeter"].role["rev"].harness, "codex");
    assert!(load(&rev("gemini-acp"))
        .unwrap_err()
        .contains("no harness adapter guards it"));
    assert!(load(&base.replace("harness = \"hermes\"", "harness = \"claude\"")).is_ok());
}

fn lane_json(b: &FakeShed, agent: &str, query: &str) -> Value {
    let c = ctx(
        &tmpdir(&format!("acp-blocks-{agent}")),
        &acp_config(&b.sock, "claude", false),
    );
    let d = json!({"role": "impl", "agent": agent, "harness": "claude", "runtime": "shed", "pane": agent});
    drover::events::append(&c.state, &Event::new(999_000, "sendmeter", Some(12), "spawn", d)).unwrap();
    let fake = Fake::default();
    let r = shed::With(&fake, b.shed());
    let s = drover::serve::Server::new(&c, &r, Some("127.0.0.1:7777".into()));
    let cookie = pair(&s, &c, "Mozilla/5.0 (iPhone) Safari/604.1", 1_000_000);
    let q = format!("GET /api/v1/lane/{agent}{query} HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n{cookie}\r\n");
    let res = String::from_utf8_lossy(&s.respond(&q, 1_000_000)).into_owned();
    assert!(res.starts_with("HTTP/1.1 200 OK"), "{res}");
    serde_json::from_str(res.split_once("\r\n\r\n").unwrap().1).unwrap()
}

fn recorded(b: &FakeShed, agent: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/golden/acp/{agent}.events.json"));
    let events: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    b.add("sendmeter-impl-12", lane_labels());
    b.set("sendmeter-impl-12", |t| {
        (t["acp"], t["events"]) = (json!(format!("{agent}-acp")), events)
    });
    lane_json(b, "sendmeter-impl-12", "")
}

fn shape(b: &Value) -> String {
    let keys: Vec<&str> = b.as_object().unwrap().keys().map(String::as_str).collect();
    let tool = if b["k"] == "tool" {
        format!(" {} {}", b["status"], b["kind"])
    } else {
        String::new()
    };
    format!("{} {keys:?}{tool}", b["k"])
}

#[test]
fn acp_transcripts_from_different_agents_give_the_same_blocks() {
    let got: Vec<Value> = ["claude", "codex", "gemini"]
        .iter()
        .map(|a| recorded(&FakeShed::new(), a))
        .collect();
    let shapes: Vec<Vec<String>> = got
        .iter()
        .map(|v| v["blocks"].as_array().unwrap().iter().map(shape).collect())
        .collect();
    assert_eq!(
        shapes[0],
        [
            r#""prompt" ["k", "text"]"#,
            r#""plan" ["entries", "k"]"#,
            r#""tool" ["id", "k", "kind", "status", "title", "title_line"] "completed" "read""#,
            r#""tool" ["id", "k", "kind", "status", "title", "title_line"] "completed" "execute""#,
            r#""approval" ["answered", "k", "options", "tool"]"#,
            r#""tool" ["id", "k", "kind", "status", "title", "title_line"] "failed" "search""#,
            r#""reply" ["k", "markdown"]"#,
        ],
        "{}",
        got[0]
    );
    assert_eq!(shapes[1], shapes[0], "codex: {}", got[1]);
    assert_eq!(shapes[2], shapes[0], "gemini: {}", got[2]);
    let claude = got[0]["blocks"].as_array().unwrap();
    assert_eq!(claude[0]["text"], "replay");
    assert_eq!(claude[4]["answered"], "allow");
    assert_eq!(
        claude[4]["options"][0],
        json!({"id": "allow_always", "name": "Always Allow", "kind": "allow_always"})
    );
    assert!(claude[6]["markdown"]
        .as_str()
        .unwrap()
        .starts_with("## Fixed\n\nThe parser now:\n- skips"));
    assert!(claude[6]["markdown"]
        .as_str()
        .unwrap()
        .ends_with("```\nAll 12 tests pass."));
    for v in &got {
        let cmd = &v["blocks"][3];
        assert!(cmd["title"].as_str().unwrap().contains('\n'), "{cmd}");
        assert!(!cmd["title_line"].as_str().unwrap().contains('\n'), "{cmd}");
        assert!(cmd["title"]
            .as_str()
            .unwrap()
            .starts_with(cmd["title_line"].as_str().unwrap().trim_end_matches('…')));
        assert_eq!(v["lines"], json!([]), "an ACP lane sends no screen lines");
        assert_eq!((v["before"].as_u64(), &v["more"]), (Some(1), &json!(false)), "{v}");
    }
    assert_eq!(got[0]["blocks"][3]["title_line"], "`cargo test -p parser \\…");
}

#[test]
fn an_acp_transcript_pages_by_event_seq() {
    let b = FakeShed::new();
    recorded(&b, "claude");
    let v = lane_json(&b, "sendmeter-impl-12", "?before=13&n=4");
    let kinds: Vec<String> = v["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| format!("{} {}", b["k"], b["status"]))
        .collect();
    assert_eq!(kinds, [r#""tool" "completed""#, r#""tool" "failed""#], "{v}");
    assert_eq!((v["before"].as_u64(), &v["more"]), (Some(9), &json!(true)), "{v}");
    assert_eq!(b.last("read")["before"], 13);
}

#[test]
fn a_pty_lane_still_returns_its_screen() {
    let b = FakeShed::new();
    b.add("sendmeter-impl-12", lane_labels());
    let v = lane_json(&b, "sendmeter-impl-12", "");
    assert_eq!(v["lines"], json!(["screen of sendmeter-impl-12"]), "{v}");
    assert_eq!(v["blocks"], json!([{"k": "screen"}]), "{v}");
}

fn session(b: &FakeShed, kinds: &[Value]) {
    session_of(b, "sendmeter-impl-12", kinds);
}

fn session_of(b: &FakeShed, name: &str, kinds: &[Value]) {
    let mut st = b.lock();
    let ev = &mut st.events;
    for k in kinds {
        let seq = ev.iter().filter_map(|e| e["seq"].as_u64()).max().unwrap_or(0) + 1;
        let mut e = json!({"seq": seq, "name": name, "labels": lane_labels()});
        e.as_object_mut().unwrap().extend(k.as_object().unwrap().clone());
        ev.push(e);
    }
}

fn mid_turn(b: &FakeShed) {
    b.lock().terms.get_mut("sendmeter-impl-12").unwrap()["reported"] = json!({"state": "working"});
}

fn turn() -> Value {
    json!({"event": "turn", "prompt": "# Brief"})
}

fn tool(status: &str) -> Value {
    json!({"event": "tool_call", "id": "t1", "title": "cargo test", "status": status, "kind": "execute"})
}

fn cpu(pct: u64) -> Value {
    json!({"event": "resources", "rss_mb": 900, "cpu_pct": pct})
}

const SILENT: u64 = 600;

fn drive_at(c: &drover::Ctx, r: &dyn Runner, secs: u64) -> String {
    let (code, out) = run(&later(c, secs), r, &["drive"]);
    assert_eq!(code, 0, "{out}");
    out
}

fn host(f: &mut Fake, prog: &str, stdout: &str) {
    let o = drover::runner::Out {
        code: 0,
        stdout: stdout.into(),
        stderr: String::new(),
    };
    f.replies.insert(0, (prog.into(), o));
}

#[test]
fn a_silent_acp_lane_is_nudged_then_respawned_once_then_escalated() {
    let (c, b, f, wt, _) = acp_lane("acp-silent");
    mid_turn(&b);
    session(&b, &[turn(), tool("completed")]);
    let cli = Push(b.cli(&f), wt.clone());
    drive_at(&c, &cli, 60);
    drive_at(&c, &cli, 60 + SILENT - 1);
    assert!(!f.called("shed cancel"), "not silent yet");
    let out = drive_at(&c, &cli, 60 + SILENT);
    assert!(f.called("shed cancel sendmeter-impl-12"), "{out}");
    assert_eq!(f.count("shed send sendmeter-impl-12 continue: "), 1, "{out}");
    assert_eq!(logged(&c, "nudged").len(), 1);
    drive_at(&c, &cli, 60 + SILENT + 60);
    assert_eq!(f.count("shed cancel"), 1, "one nudge");
    let (_, w) = run(&later(&c, 60 + SILENT + 60), &cli, &["waiting", "sendmeter"]);
    assert!(w.contains("nudged"), "waiting shows the nudge: {w}");
    let out = drive_at(&c, &cli, 60 + 2 * SILENT + 1);
    assert_eq!(logged(&c, "respawned-silent").len(), 1, "{out}");
    assert_eq!(logged(&c, "spawn").len(), 2, "{out}");
    assert!(logged(&c, "needs-human").is_empty());
    mid_turn(&b);
    session(&b, &[turn()]);
    drive_at(&c, &cli, 60 + 2 * SILENT + 30);
    let out = drive_at(&c, &cli, 60 + 3 * SILENT + 31);
    let nh = logged(&c, "needs-human");
    assert_eq!(nh.len(), 1, "{out}");
    assert_eq!(nh[0].str("code"), Some("LANE_SILENT"));
    drive_at(&c, &cli, 60 + 5 * SILENT);
    assert_eq!(logged(&c, "needs-human").len(), 1);
    assert_eq!(logged(&c, "spawn").len(), 2);
    assert_eq!(logged(&c, "respawned-silent").len(), 1);
}

#[test]
fn a_silent_design_lane_is_nudged() {
    let b = FakeShed::new();
    let cfg = acp_config(&b.sock, "claude", true)
        + "\n[fleet.sendmeter.role.design]\nharness = \"claude\"\naccount = \"C\"\n";
    let c = acp_ctx("acp-silent-design", &cfg);
    let f = acp_fake(&["true"])
        .reply("sysctl -n kern.memorystatus_vm_pressure_level", "1\n")
        .reply("sysctl -n vm.loadavg", "{ 1.50 1.20 1.00 }\n")
        .reply("sysctl -n hw.ncpu", "10\n")
        .reply("vm_stat", "Pages free: 1.\nSwapouts:                     100.\n");
    let cli = b.cli(&f);
    assert_eq!(run(&c, &cli, &["queue", "sendmeter", "add", "12", "--design"]).0, 0);
    let (code, out) = run(&c, &cli, &["spawn", "sendmeter", "12", "design"]);
    assert_eq!(code, 0, "{out}");
    let lane = "sendmeter-design-12";
    b.lock().terms.get_mut(lane).unwrap()["reported"] = json!({"state": "working"});
    session_of(&b, lane, &[turn(), tool("failed")]);
    drive_at(&c, &cli, 60);
    let out = drive_at(&c, &cli, 60 + SILENT);
    assert!(f.called("shed cancel sendmeter-design-12"), "{out}");
    assert_eq!(logged(&c, "nudged").len(), 1, "{out}");
}

#[test]
fn a_pending_tool_call_in_a_live_session_is_not_silent() {
    let (c, b, f, wt, _) = acp_lane("acp-tool");
    mid_turn(&b);
    session(&b, &[turn(), tool("in_progress"), cpu(80)]);
    let cli = Push(b.cli(&f), wt.clone());
    drive_at(&c, &cli, 60 + 3 * SILENT);
    assert!(!f.called("shed cancel"));
    assert!(logged(&c, "nudged").is_empty());
    session(&b, &[tool("completed")]);
    drive_at(&c, &cli, 60 + 3 * SILENT + 1);
    let out = drive_at(&c, &cli, 60 + 4 * SILENT + 1);
    assert!(f.called("shed cancel sendmeter-impl-12"), "{out}");
}

#[test]
fn a_pending_tool_call_whose_process_died_is_silent() {
    let (c, b, f, wt, _) = acp_lane("acp-tool-dead");
    mid_turn(&b);
    session(&b, &[turn(), tool("in_progress"), cpu(80), cpu(0)]);
    let cli = Push(b.cli(&f), wt.clone());
    drive_at(&c, &cli, 60);
    let out = drive_at(&c, &cli, 60 + SILENT);
    assert!(f.called("shed cancel sendmeter-impl-12"), "{out}");
    assert_eq!(logged(&c, "nudged").len(), 1, "{out}");
}

#[test]
fn a_silent_lanes_respawn_waits_under_host_busy_and_mem_low() {
    for (name, prog, sample, spawns) in [
        ("acp-busy", "sysctl -n vm.loadavg", "{ 90.00 80.00 70.00 }\n", 1),
        (
            "acp-memlow",
            "vm_stat",
            "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free: 100.\nPages inactive: 100.\n",
            1,
        ),
    ] {
        let (c, b, mut f, wt, _) = acp_lane(name);
        mid_turn(&b);
        session(&b, &[turn()]);
        host(&mut f, prog, sample);
        let cli = Push(b.cli(&f), wt.clone());
        drive_at(&c, &cli, 60);
        drive_at(&c, &cli, 60 + SILENT);
        assert_eq!(logged(&c, "nudged").len(), 1, "{name}");
        let out = drive_at(&c, &cli, 60 + 2 * SILENT + 1);
        assert_eq!(logged(&c, "spawn").len(), spawns, "{name}: {out}");
        assert_eq!(logged(&c, "respawned-silent").len(), spawns - 1, "{name}: {out}");
        assert!(logged(&c, "override").is_empty(), "{name}: {out}");
        let cmds = logged(&c, "driven");
        assert!(
            cmds.iter()
                .all(|e| !e.str("cmd").unwrap_or_default().contains("--force")),
            "{name}: {cmds:?}"
        );
    }
}

#[test]
fn an_auth_failure_marks_the_account_signed_out_alerts_once_and_respawns_on_the_next_account() {
    let (c, b, f, wt, _) = acp_lane("acp-auth");
    mid_turn(&b);
    let err = "API Error: 401 {\"type\":\"error\",\"error\":{\"type\":\"authentication_error\"}}";
    b.lock().terms.get_mut("sendmeter-impl-12").unwrap()["text"] = json!(err);
    let cli = Push(b.cli(&f), wt.clone());
    for s in [60, 120] {
        let (code, out) = run(&later(&c, s), &cli, &["check"]);
        assert_eq!(code, 0, "{out}");
    }
    let out = logged(&c, "account-signed-out");
    assert_eq!(out.len(), 1, "alerted once: {out:?}");
    assert_eq!(out[0].str("account"), Some("C"));
    let why = out[0].str("reason").unwrap();
    assert!(why.contains("claude /login") && why.contains(&acct_c()), "{why}");
    drive_at(&c, &cli, 180);
    let sp = logged(&c, "spawn");
    assert_eq!(sp.len(), 2);
    assert_eq!(sp[1].str("account"), Some("D"));
}

#[test]
fn a_skipped_fix_round_spawn_shows_its_refusal_in_waiting() {
    let (c, b, mut f, wt, _) = acp_lane("acp-held");
    let ev = |ts: u64, kind: &str, d: Value| {
        drover::events::append(&c.state, &Event::new(ts, "sendmeter", Some(12), kind, d)).unwrap();
    };
    let head = "b".repeat(40);
    ev(c.now + 10, "pushed", json!({"sha": head}));
    ev(
        c.now + 20,
        "spawn",
        json!({"role": "rev", "check": true, "agent": "sendmeter-rev-12-r1", "sha": head}),
    );
    let d = json!({"verdict": "FAIL", "sha": head, "stage": "rev", "loop": "rev", "body": "FAIL"});
    ev(c.now + 30, "verdict", d);
    b.lock().terms.get_mut("sendmeter-impl-12").unwrap()["state"] = json!("exited");
    let pr = json!({"number": 100, "state": "OPEN", "baseRefName": "staging", "headRefOid": head,
        "mergeable": "MERGEABLE", "statusCheckRollup": []});
    f = f.reply("gh pr view issue-12", &pr.to_string());
    host(&mut f, "sysctl -n vm.loadavg", "{ 90.00 80.00 70.00 }\n");
    let cli = Push(b.cli(&f), wt.clone());
    drive_at(&c, &cli, 60);
    assert_eq!(logged(&c, "spawn").len(), 2, "the fix round (a new lane) waits");
    let (_, w) = run(&later(&c, 3 * 3600), &cli, &["waiting", "sendmeter"]);
    assert!(w.contains("held: HOST_BUSY load/cpu 9.00 > "), "{w}");
    assert!(!w.contains("nothing drives this"), "{w}");
}

#[test]
fn an_acp_lane_silent_after_its_first_turn_is_a_failed_start_naming_its_account() {
    let (c, b, f, wt, _) = acp_lane("acp-silent");
    let seq = b.lock().events.len() as u64 * 10 + 1;
    let turn =
        json!({"seq": seq, "name": "sendmeter-impl-12", "labels": lane_labels(), "event": "turn", "prompt": "# Brief"});
    b.lock().events.push(turn);
    let cfg = drover::config::Config::load(&c.config).unwrap();
    shed::intake(&c, &cfg, &cfg.shed().unwrap().wire).unwrap();
    let cli = Push(b.cli(&f), wt.clone());
    let check = |secs| {
        let (code, out) = run(&later(&c, secs), &cli, &["check"]);
        assert_eq!(code, 0, "{out}");
        out
    };
    check(120);
    assert!(logged(&c, "lane-failed-start").is_empty(), "2 min: not yet");
    let out = check(200);
    let failed = logged(&c, "lane-failed-start");
    assert_eq!(failed.len(), 1, "{out}");
    assert_eq!(failed[0].str("account"), Some("C"));
    assert!(failed[0].str("reason").unwrap().contains("account C"), "{failed:?}");
    check(400);
    assert_eq!(logged(&c, "lane-failed-start").len(), 1, "once per start");
    let (_, w) = run(&later(&c, 400), &cli, &["waiting", "sendmeter"]);
    assert!(w.contains("failed start") && !w.contains("idle without pushing"), "{w}");
}

fn verify_round_one(name: &str) -> (drover::Ctx, FakeShed) {
    let b = FakeShed::new();
    let verify = "\n[fleet.sendmeter.role.verify]\nharness = \"claude\"\nprofile = \"fleet-verify\"\n";
    let c = ctx(&tmpdir(name), &format!("{}{verify}", shed_config(&b.sock, "")));
    let ev = |kind: &str, d: Value| {
        drover::events::append(&c.state, &Event::new(c.now - 100, "sendmeter", Some(12), kind, d)).unwrap()
    };
    let impl_ = json!({"role": "impl", "agent": "sendmeter-impl-12", "branch": "issue-12"});
    ev("spawn", impl_.clone());
    ev("pushed", json!({"sha": "a".repeat(40)}));
    let r1 = "sendmeter-verify-12-r1";
    let wt = c.worktrees.join("sendmeter/verify-12-r1");
    ev(
        "spawn",
        json!({"role": "verify", "stage": "verify", "check": true, "agent": r1, "pane": r1, "runtime": "shed",
        "worktree": wt, "branch": "verify-12-r1"}),
    );
    let fail = json!({"verdict": "FAIL", "sha": "a".repeat(40), "stage": "verify", "body": "FAIL"});
    ev("verdict", fail);
    ev("spawn", impl_);
    ev("pushed", json!({"sha": "b".repeat(40)}));
    b.add(
        r1,
        json!({"fleet": "sendmeter", "issue": "12", "role": "verify", "harness": "claude"}),
    );
    (c, b)
}

fn carrying(b: &FakeShed, role: &str) -> Vec<String> {
    let st = b.lock();
    let live = st
        .terms
        .values()
        .filter(|t| t["state"] == "running" && t["labels"]["role"] == role);
    live.map(|t| t["name"].as_str().unwrap().to_string()).collect()
}

#[test]
fn spawning_verify_round_two_stops_round_ones_live_terminal_first() {
    let (c, b) = verify_round_one("dup-spawn");
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    let f = spawn_fake().reply("gh pr view", "{}");
    let (code, out) = run(&c, &f, &["spawn", "sendmeter", "12", "verify"]);
    assert_eq!(code, 0, "{out} {:?}", f.calls.borrow());
    let order: Vec<(String, String)> = (b.lock().seen.iter())
        .filter(|s| matches!(s.0.as_str(), "kill" | "run"))
        .map(|s| (s.0.clone(), s.1["name"].as_str().unwrap().to_string()))
        .collect();
    let want = [("kill", "sendmeter-verify-12-r1"), ("run", "sendmeter-verify-12-r2")];
    assert_eq!(
        order,
        want.map(|(m, n)| (m.to_string(), n.to_string())),
        "round 1 stops before round 2 starts"
    );
    assert_eq!(carrying(&b, "verify"), ["sendmeter-verify-12-r2"]);
    let closed = logged(&c, "lane-closed");
    assert_eq!(closed.len(), 1);
    assert_eq!(
        (closed[0].str("agent"), closed[0].str("why")),
        (Some("sendmeter-verify-12-r1"), Some("lane-dup"))
    );
}

#[test]
fn two_live_processes_of_one_lane_alert_once_and_the_next_drive_pass_keeps_the_newest() {
    let (c, b) = verify_round_one("dup-check");
    let r2 = "sendmeter-verify-12-r2";
    let d = json!({"role": "verify", "stage": "verify", "check": true, "agent": r2, "pane": r2, "runtime": "shed"});
    drover::events::append(&c.state, &Event::new(c.now - 50, "sendmeter", Some(12), "spawn", d)).unwrap();
    b.add(
        r2,
        json!({"fleet": "sendmeter", "issue": "12", "role": "verify", "harness": "claude"}),
    );
    let f = Fake::default();
    for _ in 0..2 {
        assert_eq!(run(&c, &f, &["check"]).0, 0);
    }
    let dup = logged(&c, "lane-dup");
    assert_eq!(dup.len(), 1, "once per state");
    assert!(
        dup[0]
            .str("reason")
            .unwrap()
            .contains("sendmeter-verify-12-r1, sendmeter-verify-12-r2"),
        "{:?}",
        dup[0]
    );
    let (code, out) = run(&later(&c, 600), &f, &["drive"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(carrying(&b, "verify"), [r2], "{out}");
    assert!(!b.has("sendmeter-verify-12-r1"));
}

#[test]
fn clean_removes_exited_shed_terminals_of_merged_lanes_only() {
    let (c, b) = verify_round_one("dup-clean");
    drover::events::append(
        &c.state,
        &Event::new(c.now, "sendmeter", Some(12), "merged", json!({"sha": "b".repeat(40)})),
    )
    .unwrap();
    b.exit("sendmeter-verify-12-r1");
    b.add(
        "sendmeter-impl-13",
        json!({"fleet": "sendmeter", "issue": "13", "role": "impl", "harness": "claude"}),
    );
    b.exit("sendmeter-impl-13");
    let f = Fake::default().reply("gh issue list", "[]");
    let (_, out) = run(&c, &f, &["clean"]);
    assert!(
        out.contains("would remove exited shed terminal sendmeter-verify-12-r1 (sendmeter#12 merged)"),
        "{out}"
    );
    assert_eq!(b.lock().terms.len(), 2, "a dry run removes nothing");
    let (_, out) = run(&c, &f, &["clean", "--yes"]);
    assert!(
        out.contains("removed exited shed terminal sendmeter-verify-12-r1"),
        "{out}"
    );
    let left: Vec<String> = b.lock().terms.keys().cloned().collect();
    assert_eq!(left, ["sendmeter-impl-13"], "an open issue's exited terminal stays");
}

fn hung_orchestrator(name: &str) -> (drover::Ctx, FakeShed) {
    let b = FakeShed::new();
    let c = acp_ctx(name, &acp_config(&b.sock, "claude", true));
    run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Grill"]);
    assert_eq!(
        run(&c, &b.cli(&acp_fake(&["echo"])), &["thread", "say", "t1", "hi"]).0,
        0
    );
    b.set("orchestrator-t1", |t| {
        (t["reported"], t["resources"]) = (json!({"state": "working"}), json!({"rss_mb": 90, "cpu_pct": 0}));
    });
    (c, b)
}

fn hang_fake() -> Fake {
    acp_fake(&["echo"]).reply("shed kill", "")
}

fn shed_calls(f: &Fake) -> Vec<String> {
    let calls = f.calls.borrow();
    let words = |c: &&String| c.split(' ').take(2).collect::<Vec<_>>().join(" ");
    calls
        .iter()
        .filter(|c| c.starts_with("shed "))
        .map(|c| words(&c))
        .collect()
}

#[test]
fn check_alerts_stall_once_for_a_hung_acp_orchestrator() {
    let (c, b) = hung_orchestrator("acp-orchestrator-stall");
    let f = hang_fake();
    assert_eq!(run(&at(&c, c.now + 29 * 60), &b.cli(&f), &["check"]).0, 0);
    assert!(thread_events(&c, "stall").is_empty(), "within stall_minutes");
    let late = at(&c, c.now + 31 * 60);
    let (code, out) = run(&late, &b.cli(&f), &["check"]);
    assert_eq!(code, 0, "{out}");
    let st = thread_events(&c, "stall");
    assert_eq!(st.len(), 1, "{out}");
    assert_eq!(
        (st[0].str("thread"), st[0].str("agent")),
        (Some("t1"), Some("orchestrator-t1"))
    );
    assert!(
        st[0].str("reason").unwrap().contains("no CPU or output for 31m"),
        "{st:?}"
    );
    run(&at(&c, c.now + 40 * 60), &b.cli(&f), &["check"]);
    assert_eq!(thread_events(&c, "stall").len(), 1, "once per session");
    assert!(
        thread_events(&c, "orchestrator-sleep").is_empty(),
        "a working session is never put to sleep"
    );
}

#[test]
fn thread_say_restarts_a_hung_acp_orchestrator_and_delivers_to_the_fresh_one() {
    let (c, b) = hung_orchestrator("acp-orchestrator-hung-say");
    let labels = json!({"fleet": "sendmeter", "issue": "-", "role": "orchestrator", "thread": "t1"});
    let text = json!({"seq": 1, "name": "orchestrator-t1", "labels": labels, "event": "text", "text": "thinking"});
    b.lock().events.push(text);
    let cfg = drover::config::Config::load(&c.config).unwrap();
    shed::intake(&at(&c, c.now + 20 * 60), &cfg, &cfg.shed().unwrap().wire).unwrap();
    let f = hang_fake();
    assert_eq!(
        run(
            &at(&c, c.now + 31 * 60),
            &b.cli(&f),
            &["thread", "say", "t1", "still there?"]
        )
        .0,
        0
    );
    assert_eq!(shed_calls(&f), ["shed send"]);
    let f = hang_fake();
    let (code, out) = run(&at(&c, c.now + 51 * 60), &b.cli(&f), &["thread", "say", "t1", "hello?"]);
    assert_eq!((code, out.as_str()), (0, "t1: -> orchestrator-t1\n"));
    assert_eq!(
        shed_calls(&f),
        ["shed cancel", "shed kill", "shed agents", "shed run", "shed send"]
    );
    let sends = f
        .calls
        .borrow()
        .iter()
        .filter(|c| c.starts_with("shed send"))
        .cloned()
        .collect::<Vec<_>>();
    assert!(sends[0].ends_with("[t1] owner: hello?"), "{sends:?}");
    let sl = thread_events(&c, "orchestrator-sleep");
    assert_eq!(sl.len(), 1);
    assert_eq!(
        (sl[0].str("why"), sl[0].val("quiet_secs").as_u64()),
        (Some("hung"), Some(31 * 60))
    );
    assert_eq!(thread_events(&c, "orchestrator-start").len(), 2);
}

#[test]
fn stop_ends_a_hung_acp_orchestrator_by_its_thread() {
    let (c, b) = hung_orchestrator("acp-orchestrator-stop");
    let f = hang_fake();
    let (code, out) = run(&at(&c, c.now + 60), &b.cli(&f), &["stop", "t1"]);
    assert_eq!((code, out.as_str()), (0, "stopped orchestrator-t1\n"));
    assert_eq!(shed_calls(&f), ["shed cancel", "shed kill"]);
    let st = thread_events(&c, "stop");
    assert_eq!(
        (st[0].str("thread"), st[0].str("agent")),
        (Some("t1"), Some("orchestrator-t1"))
    );
    let sl = thread_events(&c, "orchestrator-sleep");
    assert_eq!(sl.iter().map(|e| e.str("why")).collect::<Vec<_>>(), [Some("stopped")]);
    let (code, out) = run(&at(&c, c.now + 120), &b.cli(&hang_fake()), &["stop", "orchestrator-t1"]);
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("NO_SUCH_AGENT") && out.contains("asleep"), "{out}");
}
