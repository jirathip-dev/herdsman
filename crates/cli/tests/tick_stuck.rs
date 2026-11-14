mod common;
use common::*;
use drover::events::{append, read, Event};
use drover::runner::{Out, Runner};
use serde_json::json;
use std::time::Duration;

const NOW: u64 = 1_000_000;
const WT: &str = "/Users/me/.local/share/drover/work/sendmeter/issue-9";
const SSH: &str = "ssh -oBatchMode=yes -oConnectTimeout=10 -oControlMaster=auto -oControlPath=/tmp/drover-ssh-%C -oControlPersist=10m air -- ";

fn setup(name: &str, extra: &str) -> (drover::Ctx, FakeShed) {
    let (c, sh) = setup_on(name, extra);
    sh.attach(&c);
    (c, sh)
}

fn setup_on(name: &str, extra: &str) -> (drover::Ctx, FakeShed) {
    let base = CONFIG.replace("harness  =  \"hermes\"", "harness = \"claude\"");
    let toml = format!("{base}\n[hosts.mini]\n\n[hosts.air]\nmachine = \"air\"\n{extra}");
    let c = ctx(&tmpdir(name), &toml);
    let sh = FakeShed::new();
    let st = c.state.parent().unwrap();
    std::fs::create_dir_all(st).unwrap();
    let cache = json!({
        "mini": {"tags": ["os:macos"], "home": "/Users/me", "probed": NOW, "seen": NOW, "up": true},
        "air": {"tags": ["os:macos"], "home": "/Users/me", "probed": NOW, "seen": NOW, "up": true},
    });
    std::fs::write(st.join("hosts.json"), cache.to_string()).unwrap();
    let d = json!({"role": "impl", "agent": "sendmeter-impl-9", "pane": "sendmeter-impl-9", "workspace": "",
                   "runtime": "shed", "worktree": WT, "branch": "issue-9", "host": "air", "machine": "air"});
    append(&c.state, &Event::new(NOW - 900, "sendmeter", Some(9), "spawn", d)).unwrap();
    append(
        &c.state,
        &Event::new(NOW - 800, "sendmeter", Some(9), "merged", json!({})),
    )
    .unwrap();
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    (c, sh)
}

fn local() -> Fake {
    Fake::default()
        .reply("sysctl -n vm.loadavg", "{ 0.4 1 1 }")
        .reply("sysctl -n hw.ncpu", "4")
        .reply("sysctl -n kern.memorystatus_vm_pressure_level", "1\n")
        .reply("git ", "")
        .reply("gh issue view", r#"{"title":"Fix the meter","body":"AC: it works"}"#)
        .reply("which shed", "/opt/shed/bin/shed\n")
}

struct Hang {
    f: Fake,
    mark: std::path::PathBuf,
    spent: Shared<u64>,
    seen: Shared<Vec<String>>,
    beats: Shared<Vec<String>>,
}

fn air(prog: &str, args: &[String]) -> bool {
    prog == "ssh" && args.iter().any(|a| a == "air")
}

impl Runner for Hang {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        assert!(!air(prog, args), "unbounded call to the Air: {prog} {args:?}");
        self.f.run(prog, args)
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> anyhow::Result<Out> {
        assert!(!air(prog, args), "unbounded call to the Air: {prog} {args:?}");
        self.f.run_in(prog, args, input)
    }
    fn run_for(&self, prog: &str, args: &[String], input: &str, limit: Duration) -> anyhow::Result<Out> {
        if !air(prog, args) {
            return self.f.run_in(prog, args, input);
        }
        self.f.calls.borrow_mut().push(format!("{prog} {}", args.join(" ")));
        self.seen
            .borrow_mut()
            .push(std::fs::read_to_string(&self.mark).unwrap_or_default());
        let beat = self.mark.with_file_name("tick.beat");
        self.beats
            .borrow_mut()
            .push(std::fs::read_to_string(beat).unwrap_or_default());
        self.spent.set(self.spent.get() + limit.as_secs());
        Ok(Out {
            code: 124,
            ..Out::default()
        })
    }
    fn detach(&self, exe: &str, args: &[String], log: &std::path::Path) -> anyhow::Result<u32> {
        self.f.detach(exe, args, log)
    }
}

#[test]
fn a_host_that_never_answers_costs_one_probe_and_drive_still_starts() {
    let (c, sh) = setup("stuck-air", "");
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "12"]).0, 0);
    let d = json!({"role": "impl", "agent": "sendmeter-impl-11", "pane": "w8:p1", "workspace": "w8",
                   "worktree": "/Users/me/.local/share/drover/work/sendmeter/issue-11", "branch": "issue-11",
                   "host": "air", "machine": "air"});
    append(&c.state, &Event::new(NOW - 600, "sendmeter", Some(11), "spawn", d)).unwrap();
    let r = Hang {
        f: local(),
        mark: c.state.with_file_name("tick.step"),
        spent: Shared::new(0),
        seen: Shared::default(),
        beats: Shared::default(),
    };
    let (code, out) = run(&c, &r, &["tick"]);
    assert_eq!(code, 0, "{out}");
    assert!(sh.called("run", "sendmeter-impl-12"), "drive still starts: {out}");
    let calls = r.f.calls.borrow();
    let to_air: Vec<_> = calls.iter().filter(|l| l.contains(" air ")).collect();
    assert_eq!(
        to_air,
        ["ssh -oBatchMode=yes -oConnectTimeout=5 -oControlPath=none air -- true"],
        "one probe, nothing after it"
    );
    assert!(r.spent.get() <= 60, "{}s on the Air", r.spent.get());
    let mark = r.seen.borrow()[0].clone();
    assert!(mark.contains(r#""step":"check""#) && mark.contains("air"), "{mark}");
    assert!(!r.mark.exists(), "the tick's step marker goes when it ends");
    assert_eq!(r.beats.borrow()[0], format!("{NOW} {NOW}"));
    assert!(
        !r.mark.with_file_name("tick.beat").exists(),
        "the heartbeat goes when the tick ends"
    );
    let cached = drover::hosts::cached(&c);
    assert!(!cached["air"].up, "the Air shows offline");
    assert!(
        out.contains(&format!("keep air:{WT}: host unreachable (not cleaned)")),
        "{out}"
    );
    let (_, status) = run(&c, &Fake::default(), &["status"]);
    assert!(status.contains("host offline: air"), "{status}");
}

#[test]
fn the_ticks_clean_never_probes_a_disabled_hosts_worktrees() {
    let (c, _sh) = setup("stuck-disabled", "enabled = false");
    let r = local().reply("herdr status", "server:\n  status: running\n");
    let air = FakeShed::new();
    let on = OverSsh {
        f: &r,
        hosts: vec![("air", &air)],
    };
    let (code, out) = run(&c, &on, &["tick"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(&format!("keep air:{WT}: host unreachable (not cleaned)")),
        "{out}"
    );
    let calls = r.calls.borrow();
    let removes = calls
        .iter()
        .filter(|l| l.contains(" air ") && l.contains("worktree remove"));
    assert_eq!(removes.count(), 0, "{calls:?}");
    drop(calls);
    assert!(!out.contains("scratch: air:"), "no scratch sweep there: {out}");
    let (c2, _sh2) = setup("stuck-enabled", "");
    let r = local().reply(&format!("{SSH}'sh' '-c' 'test -e"), "");
    let on = OverSsh {
        f: &r,
        hosts: vec![("air", &air)],
    };
    let (_, out) = run(&c2, &on, &["clean"]);
    assert!(r.called(&format!("{SSH}'sh' '-c' 'test -e")), "{out}");
    assert!(!out.contains("not cleaned"), "{out}");
}

#[test]
fn clean_reports_a_silent_disabled_host_offline_after_one_probe() {
    let (c, _sh) = setup("stuck-disabled-silent", "enabled = false");
    let r = Hang {
        f: local(),
        mark: c.state.with_file_name("tick.step"),
        spent: Shared::new(0),
        seen: Shared::default(),
        beats: Shared::default(),
    };
    let (code, out) = run(&c, &r, &["clean", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(&format!("keep air:{WT}: host unreachable (not cleaned)")),
        "{out}"
    );
    assert!(out.contains("scratch: air: host offline (not cleaned)"), "{out}");
    let calls = r.f.calls.borrow();
    let to_air: Vec<_> = calls.iter().filter(|l| l.contains(" air ")).collect();
    assert_eq!(
        to_air,
        ["ssh -oBatchMode=yes -oConnectTimeout=5 -oControlPath=none air -- true"],
        "one probe, nothing after it"
    );
    assert!(r.spent.get() <= 5, "{}s on the Air", r.spent.get());
}

#[test]
fn a_dead_shared_connection_drops_its_socket_and_runs_once_more_without_it() {
    let (c, _sh) = setup("stuck-socket", "");
    let air = FakeShed::new();
    let sock = c.state.with_file_name("drover-ssh-abc");
    std::fs::write(&sock, "").unwrap();
    let probe = format!("{SSH}'sh' '-c' 'test -e");
    let r = local()
        .reply(
            "ssh -G -oControlPath=/tmp/drover-ssh-%C air",
            &format!("user me\ncontrolpath {}\n", sock.display()),
        )
        .once_err(
            &probe,
            255,
            "mux_client_request_session: read from master failed: Broken pipe",
        )
        .reply(
            "ssh -oControlPath=none -oBatchMode=yes -oConnectTimeout=10 air -- 'sh' '-c' 'test -e",
            "",
        );
    let (code, out) = run(
        &c,
        &OverSsh {
            f: &r,
            hosts: vec![("air", &air)],
        },
        &["clean"],
    );
    assert_eq!(code, 0, "{out}");
    assert!(!sock.exists(), "the stale socket is removed");
    let calls = r.calls.borrow();
    let (dead, again) = (
        calls.iter().position(|l| l.starts_with(&probe)),
        calls
            .iter()
            .position(|l| l.starts_with("ssh -oControlPath=none -oBatchMode=yes -oConnectTimeout=10 air --")),
    );
    assert!(dead.is_some() && dead < again, "{calls:?}");
    assert!(
        out.contains(&format!("would save air:{WT}")),
        "the retry's answer counts: {out}"
    );
}

#[test]
fn a_tick_ten_minutes_old_it_cannot_name_raises_slo_tick_and_stuck_once() {
    let (c, _sh) = setup("stuck-alert", "");
    let mark = c.state.with_file_name("tick.step");
    let child = "ssh -oBatchMode=yes air -- 'sh' '-c' 'test -e \"$1\"'";
    let kinds = |c: &drover::Ctx, k: &str| -> Vec<Event> {
        read(&c.state)
            .unwrap()
            .to_vec()
            .into_iter()
            .filter(|e| e.kind == k)
            .collect()
    };
    let drive = |c: &drover::Ctx| assert_eq!(run(c, &local(), &["drive", "--fast"]).0, 0);
    std::fs::write(
        &mark,
        json!({"since": NOW - 300, "step": "clean", "child": child}).to_string(),
    )
    .unwrap();
    drive(&c);
    assert!(kinds(&c, "slo-tick").is_empty(), "5 min is not stuck");
    std::fs::write(
        &mark,
        json!({"since": NOW - 700, "step": "clean", "child": child}).to_string(),
    )
    .unwrap();
    let later = drover::Ctx {
        now: NOW + 60,
        ..c.clone()
    };
    for c in [&c, &c, &later] {
        drive(c);
        let e = kinds(c, "slo-tick");
        assert_eq!(e.len(), 1, "once: {e:?}");
        assert!(
            e[0].str("reason")
                .unwrap()
                .contains(&format!("tick running 11 min, in clean; waiting on: {child}")),
            "{e:?}"
        );
        let s = kinds(c, "stuck");
        assert_eq!(
            s.len(),
            1,
            "no heal for a tick with no pid: alerted at once, once: {s:?}"
        );
        assert!(
            s[0].str("reason").unwrap().ends_with("tried: nothing (no known heal)"),
            "{s:?}"
        );
    }
    assert!(kinds(&c, "self-heal").is_empty());
}

#[test]
fn a_drover_tick_over_its_limit_is_stopped_by_the_next_fast_step_and_logged_self_heal() {
    let (c, _sh) = setup("stuck-kill", "");
    let mark = c.state.with_file_name("tick.step");
    std::fs::write(
        &mark,
        json!({"since": NOW - 700, "step": "drive", "child": "", "pid": 4242}).to_string(),
    )
    .unwrap();
    let r = local()
        .once("kill -0 4242", 0, "")
        .reply("ps -o command= -p 4242", "/Users/me/.local/bin/drover tick\n")
        .reply("kill -TERM 4242", "");
    assert_eq!(run(&c, &r, &["drive", "--fast"]).0, 0);
    assert_eq!(r.count("kill -TERM 4242"), 1);
    assert!(!r.called("kill -KILL"), "it ended on SIGTERM");
    let ev = read(&c.state).unwrap();
    let heal: Vec<&Event> = ev.iter().filter(|e| e.kind == "self-heal").collect();
    assert_eq!(heal.len(), 1, "{heal:?}");
    assert_eq!(heal[0].str("did"), Some("stopped tick pid 4242 (SIGTERM)"));
    assert_eq!(ev.iter().filter(|e| e.kind == "slo-tick").count(), 1);
    let later = drover::Ctx {
        now: NOW + 600,
        ..c.clone()
    };
    let r = local().reply("ps -o command= -p 4242", "/Users/me/.local/bin/drover tick\n");
    assert_eq!(run(&later, &r, &["drive", "--fast"]).0, 0);
    assert!(!r.called("kill -TERM"), "a stopped tick is not stopped again");
    let kinds: Vec<String> = read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .map(|e| e.kind)
        .filter(|k| k.starts_with("s"))
        .collect();
    assert!(
        kinds.ends_with(&["slo-tick".into(), "self-heal".into(), "slo-ok".into()]),
        "{kinds:?}"
    );
    assert!(!kinds.contains(&"stuck".to_string()), "healed: no alert");
}

#[test]
fn a_process_that_is_no_drover_tick_is_never_killed() {
    let (c, _sh) = setup("stuck-foreign", "");
    let mark = c.state.with_file_name("tick.step");
    std::fs::write(
        &mark,
        json!({"since": NOW - 700, "step": "drive", "child": "", "pid": 4243}).to_string(),
    )
    .unwrap();
    let r = local()
        .reply("kill -0 4243", "")
        .reply("ps -o command= -p 4243", "/usr/bin/vim notes.txt\n");
    assert_eq!(run(&c, &r, &["drive", "--fast"]).0, 0);
    assert!(
        !r.called("kill -TERM") && !r.called("kill -KILL"),
        "{:?}",
        r.calls.borrow()
    );
    let ev = read(&c.state).unwrap();
    assert!(
        ev.iter().any(|e| e.kind == "stuck"),
        "a tick it cannot name is the owner's at once"
    );
}

#[test]
fn a_tick_that_ignores_sigterm_is_killed_after_10_s() {
    let (c, _sh) = setup("stuck-sigkill", "");
    let mark = c.state.with_file_name("tick.step");
    std::fs::write(
        &mark,
        json!({"since": NOW - 700, "step": "drive", "child": "", "pid": 4244}).to_string(),
    )
    .unwrap();
    let r = local()
        .reply("kill -0 4244", "")
        .reply("ps -o command= -p 4244", "drover drive\n")
        .reply("kill -TERM 4244", "")
        .reply("kill -KILL 4244", "");
    assert_eq!(run(&c, &r, &["drive", "--fast"]).0, 0);
    assert_eq!(
        (
            r.count("kill -TERM 4244"),
            r.count("sleep 1"),
            r.count("kill -KILL 4244")
        ),
        (1, 10, 1)
    );
    let ev = read(&c.state).unwrap();
    let heal = ev.iter().find(|e| e.kind == "self-heal").unwrap();
    assert_eq!(heal.str("did"), Some("stopped tick pid 4244 (SIGKILL after 10 s)"));
}
