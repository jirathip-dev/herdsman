//! M2 spawn gates: host load (HOST_BUSY), duplicate guard (LANE_LIVE,
//! WORKTREE_DIRTY, --supersede), the Hermes kick retry (#9) and the orch lane.
mod common;
use common::*;
use herdsman::events::{append, read, Event};
use herdsman::runner::{Out, Runner};
use serde_json::json;
use std::path::PathBuf;

const WS: &str = r#"{"result":{"workspace":{"workspace_id":"w9"}}}"#;
const PANES: &str = r#"{"result":{"panes":[{"pane_id":"w9:p1","agent":null}]}}"#;

fn wt(c: &herdsman::Ctx) -> PathBuf {
    c.worktrees.join("sendmeter/issue-12")
}

/// A Fake on which `herdsman spawn sendmeter 12 impl` (hermes) succeeds end to end.
/// Prepend `once` replies or pass `agent_list` to steer it.
fn happy(c: &herdsman::Ctx, agent_list: &str) -> Fake {
    let w = wt(c);
    std::fs::create_dir_all(&w).unwrap();
    Fake::default()
        .reply("herdr agent list", agent_list)
        .reply("sysctl -n kern.memorystatus_vm_pressure_level", "1\n")
        .reply("sysctl -n vm.loadavg", "{ 1.50 1.20 1.00 }\n")
        .reply("sysctl -n hw.ncpu", "10\n")
        .reply("vm_stat", "Pages free: 1.\nSwapouts:                     100.\n")
        .reply(&format!("git -C {} rev-list", w.display()), "0\n")
        .reply("git -C", "")
        .reply("gh issue view 12", r#"{"title":"t","body":"b"}"#)
        .reply("herdr worktree", WS)
        .reply("herdr workspace create", WS)
        .reply("herdr pane list", PANES)
        .reply("herdr pane run", "{}")
        .reply("herdr agent start", "{}")
        .reply("herdr pane", "{}")
        .reply("herdr agent prompt", "{}")
        .reply("herdr agent wait", "{}")
}

fn spawn(c: &herdsman::Ctx, r: &dyn Runner, extra: &[&str]) -> (i32, String) {
    let mut a = vec!["spawn", "sendmeter", "12", "impl"];
    a.extend(extra);
    run(c, r, &a)
}

fn assert_refused(c: &herdsman::Ctx, r: &Fake, extra: &[&str], code: &str) -> String {
    let before = read(&c.state).unwrap().len();
    let (exit, out) = spawn(c, r, extra);
    assert_eq!(exit, 3, "{out} {:?}", r.calls.borrow());
    assert!(out.contains(&format!("REFUSED {code}")), "{out}");
    assert!(!r.called("herdr worktree"), "nothing created: {:?}", r.calls.borrow());
    assert!(!r.called("herdr agent start"));
    assert_eq!(read(&c.state).unwrap().len(), before, "nothing logged");
    out
}

// --- host-load gate ---

#[test]
fn host_busy_on_memory_pressure() {
    for level in ["2", "4"] {
        let d = tmpdir("host-pressure");
        let c = ctx(&d, CONFIG);
        let r = happy(&c, NO_AGENTS).once("sysctl -n kern.memorystatus_vm_pressure_level", 0, level);
        let out = assert_refused(&c, &r, &[], "HOST_BUSY");
        assert!(out.contains("memory pressure"), "{out}");
    }
}

#[test]
fn host_busy_on_swapout_rate() {
    let d = tmpdir("host-swap");
    let c = ctx(&d, CONFIG);
    let r = happy(&c, NO_AGENTS)
        .once("vm_stat", 0, "Swapouts: 100.\n")
        .once("vm_stat", 0, "Swapouts: 5100.\n");
    let out = assert_refused(&c, &r, &[], "HOST_BUSY");
    assert!(out.contains("swap-out 5000"), "{out}");
    assert!(r.called("sleep 2"), "two samples 2 s apart");
}

/// #103: a fake `df` under `[limits] min_free_gb` (default 10) refuses DISK_LOW; at or over it admits.
#[test]
fn disk_low_under_min_free_gb() {
    let d = tmpdir("disk-low");
    let c = ctx(&d, CONFIG);
    let df = |kb: u64| {
        format!("Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/disk3s1 239362496 9 {kb} 50% /\n")
    };
    let r = happy(&c, NO_AGENTS).once("df -k /", 0, &df(9 * 1_048_576));
    let out = assert_refused(&c, &r, &[], "DISK_LOW");
    assert!(
        out.contains("9.0 GB free < 10;") && out.contains("herdsman clean --yes"),
        "{out}"
    );
    let r = happy(&c, NO_AGENTS).once("df -k /", 0, &df(19 * 1_048_576));
    let (exit, out) = spawn(&c, &r, &[]);
    assert_eq!(exit, 0, "{out}");
}

#[test]
fn host_busy_on_load_per_cpu() {
    let d = tmpdir("host-load");
    let c = ctx(&d, CONFIG);
    let r = happy(&c, NO_AGENTS).once("sysctl -n vm.loadavg", 0, "{ 26.00 3.00 2.00 }\n");
    let out = assert_refused(&c, &r, &[], "HOST_BUSY");
    assert!(out.contains("load/cpu 2.60"), "{out}");
}

#[test]
fn many_live_agents_with_normal_signals_admit() {
    let d = tmpdir("host-many");
    let c = ctx(&d, &format!("{CONFIG}max_agents = 12\n"));
    let rows: Vec<String> = (0..50).map(|i| format!("other-{i}")).collect();
    let list: Vec<_> = rows
        .iter()
        .map(|n| (n.as_str(), "idle", "/elsewhere", "w1:p1"))
        .collect();
    let (exit, out) = spawn(&c, &happy(&c, &agents(&list)), &[]);
    assert_eq!(
        exit, 0,
        "a leftover max_agents key is ignored and there is no count cap: {out}"
    );
}

#[test]
fn host_under_every_threshold_admits() {
    let d = tmpdir("host-ok");
    let c = ctx(&d, CONFIG);
    let r = happy(&c, NO_AGENTS);
    let (exit, out) = spawn(&c, &r, &[]);
    assert_eq!(exit, 0, "{out} {:?}", r.calls.borrow());
    assert!(
        r.called("sysctl -n kern.memorystatus_vm_pressure_level"),
        "the host was probed"
    );
    let ev = read(&c.state).unwrap();
    assert!(ev.iter().any(|e| e.kind == "spawn"));
    assert!(ev.iter().all(|e| e.kind != "override"));
}

#[test]
fn force_admits_busy_host_and_logs_override() {
    let d = tmpdir("host-force");
    let c = ctx(&d, CONFIG);
    let r = happy(&c, NO_AGENTS).once("sysctl -n kern.memorystatus_vm_pressure_level", 0, "4");
    let (exit, out) = spawn(&c, &r, &["--force"]);
    assert_eq!(exit, 0, "{out}");
    let ev = read(&c.state).unwrap();
    let o = ev.iter().find(|e| e.kind == "override").expect("override event");
    assert_eq!(o.str("code"), Some("HOST_BUSY"));
    assert!(
        o.str("reason").unwrap().contains("memory pressure critical"),
        "{:?}",
        o.data
    );
    assert!(ev.iter().any(|e| e.kind == "spawn"));
}

// --- duplicate guard ---

#[test]
fn live_agent_in_worktree_is_lane_live() {
    let d = tmpdir("dup-live");
    let c = ctx(&d, CONFIG);
    let w = wt(&c).join("src");
    let list = agents(&[("someone", "working", w.to_str().unwrap(), "w9:p2")]);
    let out = assert_refused(&c, &happy(&c, &list), &[], "LANE_LIVE");
    assert!(out.contains("someone"), "{out}");
}

#[test]
fn unfinished_lane_in_log_is_lane_live() {
    let d = tmpdir("dup-log");
    let c = ctx(&d, CONFIG);
    let w = wt(&c).to_string_lossy().into_owned();
    let d0 = json!({"role": "impl", "agent": "sendmeter-impl-12", "branch": "issue-12", "worktree": w});
    append(&c.state, &Event::new(10, "sendmeter", Some(12), "spawn", d0)).unwrap();
    let out = assert_refused(&c, &happy(&c, NO_AGENTS), &[], "LANE_LIVE");
    assert!(out.contains("unfinished impl lane"), "{out}");
}

#[test]
fn dirty_unknown_worktree_is_worktree_dirty() {
    let d = tmpdir("dup-dirty");
    let c = ctx(&d, CONFIG);
    let w = wt(&c);
    let r = happy(&c, NO_AGENTS).once(&format!("git -C {} status", w.display()), 0, " M src/x.rs\n");
    let out = assert_refused(&c, &r, &[], "WORKTREE_DIRTY");
    assert!(out.contains("uncommitted"), "{out}");
    let r = happy(&c, NO_AGENTS).once(&format!("git -C {} rev-list", w.display()), 0, "3\n");
    let out = assert_refused(&c, &r, &[], "WORKTREE_DIRTY");
    assert!(out.contains("3 unpushed"), "{out}");
    // fleet's own brief does not make a worktree dirty.
    let r = happy(&c, NO_AGENTS).once(&format!("git -C {} status", w.display()), 0, "?? .brief.md\n");
    assert_eq!(spawn(&c, &r, &[]).0, 0);
}

#[test]
fn supersede_kills_old_agent_then_spawns() {
    let d = tmpdir("dup-supersede");
    let c = ctx(&d, CONFIG);
    let w = wt(&c).to_string_lossy().into_owned();
    let d0 = json!({"role": "impl", "agent": "sendmeter-impl-12", "branch": "issue-12", "worktree": w});
    append(&c.state, &Event::new(10, "sendmeter", Some(12), "spawn", d0)).unwrap();
    let list = agents(&[("sendmeter-impl-12", "idle", &w, "w9:p2")]);
    let r = happy(&c, &list);
    let (exit, out) = spawn(&c, &r, &["--supersede"]);
    assert_eq!(exit, 0, "{out} {:?}", r.calls.borrow());
    let kill = r.index("herdr pane close w9:p2").expect("old agent killed");
    assert!(kill < r.index("herdr agent start").unwrap(), "kill before start");
    let ev = read(&c.state).unwrap();
    let s = ev.iter().find(|e| e.kind == "supersede").expect("supersede logged");
    assert_eq!(s.data["killed"], json!(["sendmeter-impl-12"]));
    assert_eq!(ev.last().unwrap().kind, "spawn");
}

// --- #9: Hermes kick ---

/// herdr + Hermes model: a prompt only lands once the input is ready
/// (`pane wait-output` saw the prompt), and the first prompt after that is dropped.
struct Hermes {
    fake: Fake,
    ready: Shared<bool>,
    dropped: Shared<bool>,
    landed: Shared<bool>,
}

impl Runner for Hermes {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        let o = self.fake.run(prog, args)?;
        let line = format!("{prog} {}", args.join(" "));
        let out = |code: i32, s: &str| {
            Ok(Out {
                code,
                stdout: s.into(),
                stderr: String::new(),
            })
        };
        if line.starts_with("herdr pane wait-output") {
            self.ready.set(true);
        }
        if line.starts_with("herdr agent prompt") {
            if self.ready.get() && self.dropped.get() {
                self.landed.set(true);
            }
            self.dropped.set(self.ready.get());
            if line.contains("--wait") {
                return out(if self.landed.get() { 0 } else { 1 }, "");
            }
        }
        if line.starts_with("herdr agent wait") {
            return out(if self.landed.get() { 0 } else { 1 }, "");
        }
        if line.starts_with("herdr pane read") {
            return out(
                0,
                if self.landed.get() {
                    "fleet-impl ❯  ctx 52K"
                } else {
                    "fleet-impl ❯  ctx --"
                },
            );
        }
        Ok(o)
    }
    fn sleep(&self, secs: u64) {
        self.fake.sleep(secs)
    }
}

#[test]
fn hermes_kick_retries_after_dropped_first_prompt() {
    let d = tmpdir("kick-retry");
    let c = ctx(&d, CONFIG);
    let h = Hermes {
        fake: happy(&c, NO_AGENTS),
        ready: Shared::new(false),
        dropped: Shared::new(false),
        landed: Shared::new(false),
    };
    let (exit, out) = spawn(&c, &h, &[]);
    assert_eq!(exit, 0, "{out} {:?}", h.fake.calls.borrow());
    let f = &h.fake;
    assert!(
        f.index("herdr pane wait-output w9:p1").unwrap() < f.index("herdr agent prompt").unwrap(),
        "waits for input"
    );
    assert_eq!(
        f.count("herdr agent prompt sendmeter-impl-12"),
        2,
        "landed on attempt 2"
    );
    assert_eq!(
        f.count("herdr pane send-keys w9:p1 Enter"),
        2,
        "explicit Enter each attempt"
    );
    assert_eq!(f.count("sleep 10"), 1);
    assert_eq!(read(&c.state).unwrap().last().unwrap().data["confirmed"], true);
}

// --- orch lane ---

#[test]
fn orch_lane_spawns_without_issue() {
    let d = tmpdir("orch");
    let c = ctx(&d, CONFIG);
    let r = happy(&c, NO_AGENTS);
    let (exit, out) = run(&c, &r, &["spawn", "sendmeter", "-", "orch"]);
    assert_eq!(exit, 0, "{out} {:?}", r.calls.borrow());
    assert!(r.called("herdr workspace create --cwd /tmp/does-not-matter/sendmeter --label sendmeter-orch"));
    assert!(r.called("herdr agent start sendmeter-orch --kind hermes --pane w9:p1 --timeout 90000 -- -p fleet-orch"));
    assert!(!r.called("gh issue view") && !r.called("herdr worktree"));
    let brief = std::fs::read_to_string(c.state.parent().unwrap().join("orch-sendmeter.md")).unwrap();
    assert!(
        brief.contains("# ROLE: ORCHESTRATOR") && brief.contains("integration branch: `staging`"),
        "{brief}"
    );
    let ev = read(&c.state).unwrap();
    let s = ev.iter().find(|e| e.kind == "spawn").unwrap();
    assert_eq!((s.issue, s.str("role")), (None, Some("orch")));
    // A second orch while the first is live: LANE_LIVE.
    let r = happy(&c, &agents(&[("sendmeter-orch", "idle", "/x", "w9:p1")]));
    let (exit, out) = run(&c, &r, &["spawn", "sendmeter", "-", "orch"]);
    assert_eq!(exit, 3, "{out}");
    assert!(out.contains("REFUSED LANE_LIVE"), "{out}");
    // orch takes no issue; impl needs one.
    assert_eq!(
        run(&c, &happy(&c, NO_AGENTS), &["spawn", "sendmeter", "12", "orch"]).0,
        1
    );
    assert_eq!(
        run(&c, &happy(&c, NO_AGENTS), &["spawn", "sendmeter", "-", "impl"]).0,
        1
    );
}

#[test]
fn fix_round_brief_carries_fail_findings() {
    let d = tmpdir("fix-brief");
    let c = ctx(&d, CONFIG);
    let w = wt(&c).to_string_lossy().into_owned();
    let d0 = json!({"role": "impl", "agent": "sendmeter-impl-12", "branch": "issue-12", "worktree": w});
    append(&c.state, &Event::new(10, "sendmeter", Some(12), "spawn", d0)).unwrap();
    let v = json!({"verdict": "FAIL", "sha": "1".repeat(40), "body": "FAIL\n1. blocking: src/x.rs:3 off by one"});
    append(&c.state, &Event::new(20, "sendmeter", Some(12), "verdict", v)).unwrap();
    let (exit, out) = spawn(&c, &happy(&c, NO_AGENTS), &["--supersede"]);
    assert_eq!(exit, 0, "{out}");
    let brief = std::fs::read_to_string(wt(&c).join(".brief.md")).unwrap();
    assert!(
        brief.contains("Review findings to fix") && brief.contains("src/x.rs:3 off by one"),
        "{brief}"
    );
}

#[test]
fn a_new_lane_brief_carries_the_issue_decisions() {
    let d = tmpdir("decision-brief");
    let c = ctx(&d, CONFIG);
    let dec = json!({"id": "q3", "choice": "per API key", "by": "owner"});
    append(&c.state, &Event::new(10, "sendmeter", Some(12), "decision", dec)).unwrap();
    let (exit, out) = spawn(&c, &happy(&c, NO_AGENTS), &[]);
    assert_eq!(exit, 0, "{out}");
    let brief = std::fs::read_to_string(wt(&c).join(".brief.md")).unwrap();
    assert!(
        brief.contains("sendmeter #12 decision q3 by owner: per API key"),
        "{brief}"
    );
}

// --- #13: herdr start timeout under load ---

const TIMEOUT: &str = r#"{"error":{"code":"timeout","message":"timed out waiting for agent startup"}}"#;
const PANE_HERMES: &str = r#"{"result":{"pane":{"pane_id":"w9:p1","workspace_id":"w9","agent":"hermes"}}}"#;
const PANE_SHELL: &str = r#"{"result":{"pane":{"pane_id":"w9:p1","workspace_id":"w9","agent":null}}}"#;

#[test]
fn start_timeout_with_running_harness_registers_lane() {
    let d = tmpdir("timeout-adopt");
    let c = ctx(&d, CONFIG);
    let r = happy(&c, NO_AGENTS)
        .once_err("herdr agent start", 1, TIMEOUT)
        .once("herdr pane get w9:p1", 0, PANE_HERMES)
        .once("herdr agent rename w9:p1 sendmeter-impl-12", 0, "{}");
    let (exit, out) = spawn(&c, &r, &[]);
    assert_eq!(exit, 0, "{out} {:?}", r.calls.borrow());
    assert!(
        r.called("herdr agent rename w9:p1 sendmeter-impl-12"),
        "{:?}",
        r.calls.borrow()
    );
    let ev = read(&c.state).unwrap();
    let s = ev.iter().find(|e| e.kind == "spawn").expect("spawn logged");
    assert_eq!(
        (s.str("agent"), s.str("pane")),
        (Some("sendmeter-impl-12"), Some("w9:p1"))
    );
    assert_eq!(s.data["confirmed"], true);
    let (_, st) = run(&c, &Fake::default(), &["status"]);
    assert!(st.contains("sendmeter-impl-12"), "{st}");
}

#[test]
fn start_timeout_without_harness_closes_pane_and_logs_nothing() {
    let d = tmpdir("timeout-none");
    let c = ctx(&d, CONFIG);
    let r = happy(&c, NO_AGENTS)
        .once_err("herdr agent start", 1, TIMEOUT)
        .once("herdr pane get w9:p1", 0, PANE_SHELL);
    let (exit, out) = spawn(&c, &r, &[]);
    assert_eq!(exit, 1, "{out}");
    assert!(r.called("herdr pane close w9:p1"), "{:?}", r.calls.borrow());
    assert!(!r.called("herdr agent rename"));
    assert!(read(&c.state).unwrap().is_empty(), "nothing logged");
}

#[test]
fn lane_live_names_pane_of_unnamed_agent() {
    let d = tmpdir("dup-unnamed");
    let c = ctx(&d, CONFIG);
    let w = wt(&c).to_string_lossy().into_owned();
    let list = json!({"result": {"agents": [{"agent_status": "working", "cwd": w, "pane_id": "w63T:p1"}]}}).to_string();
    let out = assert_refused(&c, &happy(&c, &list), &[], "LANE_LIVE");
    assert!(out.contains("w63T:p1"), "{out}");
}

#[test]
fn adopt_records_running_lane() {
    let d = tmpdir("adopt");
    let c = ctx(&d, CONFIG);
    let r = Fake::default()
        .reply("herdr pane get w9:p1", PANE_HERMES)
        .reply("herdr agent rename w9:p1 sendmeter-impl-12", "{}");
    let (exit, out) = run(&c, &r, &["adopt", "sendmeter", "12", "impl", "--pane", "w9:p1"]);
    assert_eq!(exit, 0, "{out} {:?}", r.calls.borrow());
    let ev = read(&c.state).unwrap();
    let s = ev.iter().find(|e| e.kind == "spawn").expect("spawn logged");
    assert_eq!(
        (s.str("agent"), s.str("pane"), s.str("workspace")),
        (Some("sendmeter-impl-12"), Some("w9:p1"), Some("w9"))
    );
    assert_eq!(s.data["adopted"], true);
    let (_, st) = run(&c, &Fake::default(), &["status"]);
    assert!(st.contains("sendmeter-impl-12"), "{st}");
    // A pane with no harness is not a lane.
    let r = Fake::default().reply("herdr pane get w9:p1", PANE_SHELL);
    assert_eq!(
        run(&c, &r, &["adopt", "sendmeter", "13", "impl", "--pane", "w9:p1"]).0,
        1
    );
    assert!(!r.called("herdr agent rename"));
}

// --- #107: no orphan lanes ---

#[test]
fn host_busy_creates_no_worktree_dir() {
    let d = tmpdir("host-busy-nothing");
    let c = ctx(&d, CONFIG);
    let r = happy(&c, NO_AGENTS).once("sysctl -n vm.loadavg", 0, "{ 26.90 3.00 2.00 }\n");
    std::fs::remove_dir_all(wt(&c)).unwrap();
    assert_refused(&c, &r, &[], "HOST_BUSY");
    assert!(!wt(&c).exists(), "no worktree dir");
}

/// `happy`, but `herdr agent prompt` fails as a runner error (the kick cannot be delivered).
struct KickFails(Fake);
impl Runner for KickFails {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        anyhow::ensure!(
            !(prog == "herdr" && args[..2] == ["agent", "prompt"]),
            "agent_not_ready"
        );
        self.0.run(prog, args)
    }
    fn sleep(&self, secs: u64) {
        self.0.sleep(secs);
    }
}

#[test]
fn kick_failure_after_start_still_logs_unconfirmed_spawn() {
    let d = tmpdir("kick-fails");
    let c = ctx(&d, CONFIG);
    let (exit, out) = spawn(&c, &KickFails(happy(&c, NO_AGENTS)), &[]);
    assert_eq!(exit, 1, "{out}");
    let ev = read(&c.state).unwrap();
    let s = ev.iter().find(|e| e.kind == "spawn").expect("spawn logged");
    assert_eq!(
        (s.str("agent"), s.data["confirmed"].clone()),
        (Some("sendmeter-impl-12"), json!(false))
    );
    assert!(s.str("error").unwrap().contains("agent_not_ready"), "{:?}", s.data);
}

const TAKEN: &str = r#"{"error":{"code":"agent_name_taken","message":"agent name already in use"}}"#;
const PANE_P7: &str = r#"{"result":{"pane":{"pane_id":"w9:p7","workspace_id":"w9","agent":"hermes"}}}"#;

/// The guard's `agent list` sees nothing; after `agent_name_taken` the next one shows the orphan in `cwd`.
fn name_taken(c: &herdsman::Ctx, cwd: &str) -> Fake {
    let list = agents(&[("sendmeter-impl-12", "working", cwd, "w9:p7")]);
    happy(c, &list)
        .once("herdr agent list", 0, NO_AGENTS)
        .once_err("herdr agent start", 1, TAKEN)
        .once("herdr pane get w9:p7", 0, PANE_P7)
        .reply("herdr agent rename w9:p7 sendmeter-impl-12", "{}")
}

#[test]
fn agent_name_taken_in_lane_worktree_adopts_the_orphan() {
    let d = tmpdir("taken-adopt");
    let c = ctx(&d, CONFIG);
    let r = name_taken(&c, wt(&c).to_str().unwrap());
    let (exit, out) = spawn(&c, &r, &[]);
    assert_eq!(exit, 0, "{out} {:?}", r.calls.borrow());
    let ev = read(&c.state).unwrap();
    let s = ev.iter().find(|e| e.kind == "spawn").expect("spawn logged");
    assert_eq!(
        (s.str("agent"), s.str("pane")),
        (Some("sendmeter-impl-12"), Some("w9:p7"))
    );
    assert_eq!(s.data["adopted"], true);
}

#[test]
fn agent_name_taken_elsewhere_is_refused_as_before() {
    let d = tmpdir("taken-elsewhere");
    let c = ctx(&d, CONFIG);
    let r = name_taken(&c, "/elsewhere");
    let (exit, out) = spawn(&c, &r, &[]);
    assert_eq!(exit, 1, "{out}");
    assert!(!r.called("herdr agent rename"));
    assert!(read(&c.state).unwrap().is_empty(), "nothing logged");
}

#[test]
fn named_orphan_in_worktree_is_adopted_not_lane_live() {
    let d = tmpdir("dup-orphan");
    let c = ctx(&d, CONFIG);
    let list = agents(&[("sendmeter-impl-12", "working", wt(&c).to_str().unwrap(), "w9:p7")]);
    let r = happy(&c, &list)
        .once("herdr pane get w9:p7", 0, PANE_P7)
        .reply("herdr agent rename w9:p7 sendmeter-impl-12", "{}");
    let (exit, out) = spawn(&c, &r, &[]);
    assert_eq!(exit, 0, "{out}");
    assert!(!r.called("herdr agent start"), "nothing started");
    let ev = read(&c.state).unwrap();
    assert_eq!(ev.iter().filter(|e| e.kind == "spawn").count(), 1);
    assert_eq!(ev[0].data["adopted"], true);
}
