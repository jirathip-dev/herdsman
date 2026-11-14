mod common;
use common::*;
use drover::events::{append, read, Event};
use drover::runner::{Out, Runner};
use serde_json::json;
use std::path::PathBuf;

fn wt(c: &drover::Ctx) -> PathBuf {
    c.worktrees.join("sendmeter/issue-12")
}

fn shed_happy(c: &drover::Ctx) -> (FakeShed, Fake) {
    let w = wt(c);
    std::fs::create_dir_all(&w).unwrap();
    let sh = FakeShed::new();
    let base = std::fs::read_to_string(&c.config).unwrap();
    let base = base.split("\n[runtime]\n").next().unwrap();
    std::fs::write(&c.config, sh.config(base)).unwrap();
    let f = Fake::default()
        .reply("which shed", "/opt/shed/bin/shed\n")
        .reply("sysctl -n kern.memorystatus_vm_pressure_level", "1\n")
        .reply("sysctl -n vm.loadavg", "{ 1.50 1.20 1.00 }\n")
        .reply("sysctl -n hw.ncpu", "10\n")
        .reply("vm_stat", "Pages free: 1.\nSwapouts:                     100.\n")
        .reply(&format!("git -C {} rev-list", w.display()), "0\n")
        .reply("git -C", "")
        .reply("gh issue view 12", r#"{"title":"t","body":"b"}"#);
    (sh, f)
}

fn spawn(c: &drover::Ctx, r: &dyn Runner, extra: &[&str]) -> (i32, String) {
    let mut a = vec!["spawn", "sendmeter", "12", "impl"];
    a.extend(extra);
    run(c, r, &a)
}

fn assert_refused(c: &drover::Ctx, sh: &FakeShed, r: &Fake, extra: &[&str], code: &str) -> String {
    let before = read(&c.state).unwrap().len();
    let (exit, out) = spawn(c, r, extra);
    assert_eq!(exit, 3, "{out} {:?}", r.calls.borrow());
    assert!(out.contains(&format!("REFUSED {code}")), "{out}");
    assert!(
        !r.called("git -C /tmp/does-not-matter/sendmeter worktree add"),
        "nothing created: {:?}",
        r.calls.borrow()
    );
    assert!(sh.all("run").is_empty(), "{:?}", sh.methods());
    assert_eq!(read(&c.state).unwrap().len(), before, "nothing logged");
    out
}

#[test]
fn host_busy_on_memory_pressure() {
    for level in ["2", "4"] {
        let d = tmpdir("host-pressure");
        let c = ctx(&d, CONFIG);
        let (sh, r) = shed_happy(&c);
        let r = r.once("sysctl -n kern.memorystatus_vm_pressure_level", 0, level);
        let out = assert_refused(&c, &sh, &r, &[], "HOST_BUSY");
        assert!(out.contains("memory pressure"), "{out}");
    }
}

#[test]
fn swap_refuses_on_sustained_median_with_pressure_not_one_spike() {
    let swap = |prev: &str, level: &str| {
        let d = tmpdir("host-swap");
        let c = ctx(&d, CONFIG);
        std::fs::create_dir_all(c.state.parent().unwrap()).unwrap();
        std::fs::write(c.state.with_file_name("swapouts"), prev).unwrap();
        let (sh, r) = shed_happy(&c);
        let r = r
            .once("sysctl -n kern.memorystatus_vm_pressure_level", 0, level)
            .once("vm_stat", 0, "Swapouts: 100.\n")
            .once("vm_stat", 0, "Swapouts: 5100.\n");
        (c, sh, r)
    };
    let (c, sh, r) = swap("0 0 0 0", "1");
    assert_eq!(spawn(&c, &r, &[]).0, 0, "a spike with normal pressure admits");
    assert!(r.called("sleep 2"), "two samples 2 s apart");
    assert_eq!(sh.all("run").len(), 1, "the lane started");
    let (c, sh, r) = swap("0 0 0 0", "2");
    let out = assert_refused(&c, &sh, &r, &[], "HOST_BUSY");
    assert!(!out.contains("swap-out"), "a spike's median is low: {out}");
    let (c, sh, r) = swap("4000 5000 6000 3000", "2");
    let out = assert_refused(&c, &sh, &r, &[], "HOST_BUSY");
    assert!(out.contains("swap-out median 5000 pages/2s > 2000"), "{out}");
    let w = std::fs::read_to_string(c.state.with_file_name("swapouts")).unwrap();
    assert_eq!(w, "4000 5000 6000 3000 5000", "the window keeps the last 5");
}

#[test]
fn mem_low_under_the_floor() {
    let d = tmpdir("mem-low");
    let c = ctx(&d, CONFIG);
    let vm = |pages: u64| {
        format!("Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free: {pages}.\nPages inactive: {pages}.\nSwapouts: 100.\n")
    };
    let (sh, r) = shed_happy(&c);
    let r = r.once("vm_stat", 0, &vm(32768)).once("vm_stat", 0, &vm(32768));
    let out = assert_refused(&c, &sh, &r, &[], "MEM_LOW");
    assert!(out.contains("only 1.0 GB of memory free (floor 2 GB)"), "{out}");
    let (sh, r) = shed_happy(&c);
    let r = r.once("vm_stat", 0, &vm(65536)).once("vm_stat", 0, &vm(98304));
    assert_eq!(spawn(&c, &r, &[]).0, 0, "3 GB free admits");
    assert_eq!(sh.all("run").len(), 1);
}

#[test]
fn disk_low_under_min_free_gb() {
    let d = tmpdir("disk-low");
    let c = ctx(&d, CONFIG);
    let df = |kb: u64| {
        format!("Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/disk3s1 239362496 9 {kb} 50% /\n")
    };
    let (sh, r) = shed_happy(&c);
    let r = r.once("df -k /", 0, &df(9 * 1_048_576));
    let out = assert_refused(&c, &sh, &r, &[], "DISK_LOW");
    assert!(
        out.contains("9.0 GB free < 10;") && out.contains("drover clean --yes"),
        "{out}"
    );
    let (sh, r) = shed_happy(&c);
    let r = r.once("df -k /", 0, &df(19 * 1_048_576));
    let (exit, out) = spawn(&c, &r, &[]);
    assert_eq!(exit, 0, "{out}");
    assert_eq!(sh.all("run").len(), 1);
}

#[test]
fn host_busy_on_load_per_cpu() {
    let d = tmpdir("host-load");
    let c = ctx(&d, CONFIG);
    let (sh, r) = shed_happy(&c);
    let r = r.once("sysctl -n vm.loadavg", 0, "{ 26.00 3.00 2.00 }\n");
    let out = assert_refused(&c, &sh, &r, &[], "HOST_BUSY");
    assert!(out.contains("load/cpu 2.60"), "{out}");
}

#[test]
fn load_from_drovers_own_processes_admits_and_an_outside_one_is_named() {
    let c = ctx(&tmpdir("host-own-load"), CONFIG);
    let ps = "  101     1 600.0 /opt/drover/bin/drover\n  102   101 350.0 /bin/cp\n  103     1   5.0 /usr/sbin/mds\n";
    let (sh, own) = shed_happy(&c);
    let own = own
        .once("sysctl -n vm.loadavg", 0, "{ 26.00 3.00 2.00 }\n")
        .reply("ps -Ao", ps);
    let (exit, out) = spawn(&c, &own, &[]);
    assert_eq!(exit, 0, "drover's own drive and clone: {out}");
    assert_eq!(sh.all("run").len(), 1);
    let busy = format!("{ps}  104     1 900.0 /usr/bin/rustc\n  105     1 120.0 /usr/libexec/fseventsd\n");
    let c = ctx(&tmpdir("host-other-load"), CONFIG);
    let (sh, other) = shed_happy(&c);
    let other = other
        .once("sysctl -n vm.loadavg", 0, "{ 36.00 3.00 2.00 }\n")
        .reply("ps -Ao", &busy);
    let out = assert_refused(&c, &sh, &other, &[], "HOST_BUSY");
    assert!(
        out.contains("load/cpu 2.65 > 2.5 (top: rustc 900%, fseventsd 120%, mds 5%)"),
        "{out}"
    );
}

#[test]
fn many_live_agents_with_normal_signals_admit() {
    let d = tmpdir("host-many");
    let c = ctx(&d, &format!("{CONFIG}max_agents = 12\n"));
    let (sh, r) = shed_happy(&c);
    for i in 0..50 {
        sh.lane(&format!("other-{i}"), "idle", "/elsewhere");
    }
    let (exit, out) = spawn(&c, &r, &[]);
    assert_eq!(
        exit, 0,
        "a leftover max_agents key is ignored and there is no count cap: {out}"
    );
    assert_eq!(sh.last("run")["name"], "sendmeter-impl-12");
}

#[test]
fn host_under_every_threshold_admits() {
    let d = tmpdir("host-ok");
    let c = ctx(&d, CONFIG);
    let (sh, r) = shed_happy(&c);
    let (exit, out) = spawn(&c, &r, &[]);
    assert_eq!(exit, 0, "{out} {:?}", r.calls.borrow());
    assert!(
        r.called("sysctl -n kern.memorystatus_vm_pressure_level"),
        "the host was probed"
    );
    assert_eq!(sh.last("run")["name"], "sendmeter-impl-12");
    let ev = read(&c.state).unwrap();
    assert!(ev.iter().any(|e| e.kind == "spawn"));
    assert!(ev.iter().all(|e| e.kind != "override"));
}

#[test]
fn force_admits_busy_host_and_logs_override() {
    let d = tmpdir("host-force");
    let c = ctx(&d, CONFIG);
    let (sh, r) = shed_happy(&c);
    let r = r.once("sysctl -n kern.memorystatus_vm_pressure_level", 0, "4");
    let (exit, out) = spawn(&c, &r, &["--force"]);
    assert_eq!(exit, 0, "{out}");
    assert_eq!(sh.all("run").len(), 1);
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

#[test]
fn live_agent_in_worktree_is_lane_live() {
    let d = tmpdir("dup-live");
    let c = ctx(&d, CONFIG);
    let w = wt(&c).join("src");
    let (sh, r) = shed_happy(&c);
    sh.lane("someone", "working", w.to_str().unwrap());
    let out = assert_refused(&c, &sh, &r, &[], "LANE_LIVE");
    assert!(out.contains("someone"), "{out}");
}

#[test]
fn unfinished_lane_in_log_is_lane_live() {
    let d = tmpdir("dup-log");
    let c = ctx(&d, CONFIG);
    let w = wt(&c).to_string_lossy().into_owned();
    let d0 = json!({"role": "impl", "agent": "sendmeter-impl-12", "branch": "issue-12", "worktree": w});
    append(&c.state, &Event::new(10, "sendmeter", Some(12), "spawn", d0)).unwrap();
    let (sh, r) = shed_happy(&c);
    let out = assert_refused(&c, &sh, &r, &[], "LANE_LIVE");
    assert!(out.contains("unfinished impl lane"), "{out}");
}

#[test]
fn dirty_unknown_worktree_is_worktree_dirty() {
    let d = tmpdir("dup-dirty");
    let c = ctx(&d, CONFIG);
    let w = wt(&c);
    let (sh, r) = shed_happy(&c);
    let r = r.once(&format!("git -C {} status", w.display()), 0, " M src/x.rs\n");
    let out = assert_refused(&c, &sh, &r, &[], "WORKTREE_DIRTY");
    assert!(out.contains("uncommitted"), "{out}");
    let (sh, r) = shed_happy(&c);
    let r = r.once(&format!("git -C {} rev-list", w.display()), 0, "3\n");
    let out = assert_refused(&c, &sh, &r, &[], "WORKTREE_DIRTY");
    assert!(out.contains("3 unpushed"), "{out}");
    let (sh, r) = shed_happy(&c);
    let r = r.once(&format!("git -C {} status", w.display()), 0, "?? .brief.md\n");
    assert_eq!(spawn(&c, &r, &[]).0, 0);
    assert_eq!(sh.all("run").len(), 1);
}

#[test]
fn supersede_kills_old_agent_then_spawns() {
    let d = tmpdir("dup-supersede");
    let c = ctx(&d, CONFIG);
    let w = wt(&c).to_string_lossy().into_owned();
    let d0 = json!({"role": "impl", "agent": "sendmeter-impl-12", "branch": "issue-12", "worktree": w});
    append(&c.state, &Event::new(10, "sendmeter", Some(12), "spawn", d0)).unwrap();
    let (sh, r) = shed_happy(&c);
    sh.lane("sendmeter-impl-12", "idle", &w);
    let (exit, out) = spawn(&c, &r, &["--supersede"]);
    assert_eq!(exit, 0, "{out} {:?}", r.calls.borrow());
    let m = sh.methods();
    let kill = m.iter().position(|m| m == "kill").expect("old agent killed");
    assert_eq!(sh.all("kill")[0]["name"], "sendmeter-impl-12");
    assert!(
        kill < m.iter().position(|m| m == "run").unwrap(),
        "kill before start: {m:?}"
    );
    let ev = read(&c.state).unwrap();
    let s = ev.iter().find(|e| e.kind == "supersede").expect("supersede logged");
    assert_eq!(*s.val("killed"), json!(["sendmeter-impl-12"]));
    assert_eq!(ev.last().unwrap().kind, "spawn");
}

struct Hermes<'a> {
    fake: Fake,
    sh: &'a FakeShed,
}

impl Runner for Hermes<'_> {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        self.fake.run(prog, args)
    }
    fn sleep(&self, secs: u64) {
        if secs == 10 {
            self.sh.lock().waits.clear();
            self.sh.screen(IMPL, "fleet-impl ❯  ctx 52K");
        }
        self.fake.sleep(secs)
    }
}

#[test]
fn hermes_kick_retries_after_dropped_first_prompt() {
    let d = tmpdir("kick-retry");
    let c = ctx(&d, CONFIG);
    let (sh, fake) = shed_happy(&c);
    sh.wait_ends(IMPL, "timeout");
    let h = Hermes { fake, sh: &sh };
    let (exit, out) = spawn(&c, &h, &[]);
    assert_eq!(exit, 0, "{out} {:?}", h.fake.calls.borrow());
    let f = &h.fake;
    let seen = sh.lock().seen.clone();
    let at = |m: &str, p: &dyn Fn(&serde_json::Value) -> bool| seen.iter().position(|s| s.0 == m && p(&s.1));
    let waited = at("wait", &|p| p["output"] == "❯").expect("waits for the prompt");
    let prompt = |p: &serde_json::Value| p["name"] == IMPL && !p["text"].as_str().unwrap().ends_with('\r');
    assert!(waited < at("send", &prompt).unwrap(), "waits for input");
    let prompts = seen.iter().filter(|s| s.0 == "send" && prompt(&s.1)).count();
    assert_eq!(prompts, 2, "landed on attempt 2: {seen:?}");
    let enters = sh
        .all("keys")
        .into_iter()
        .filter(|p| p["name"] == IMPL && p["keys"] == json!(["Enter"]));
    assert_eq!(enters.count(), 2, "explicit Enter each attempt");
    assert_eq!(f.count("sleep 10"), 1);
    assert_eq!(read(&c.state).unwrap().last().unwrap().val("confirmed"), true);
}

#[test]
fn an_orch_spawn_is_refused_and_every_lane_takes_an_issue() {
    let c = ctx(&tmpdir("no-orch"), CONFIG);
    let (sh, r) = shed_happy(&c);
    for issue in ["-", "12"] {
        let (exit, out) = run(&c, &r, &["spawn", "sendmeter", issue, "orch"]);
        assert_eq!(exit, 3, "{out}");
        assert_eq!(
            out.trim(),
            "REFUSED NO_ORCH: drover drives fleets itself; see drover timer"
        );
        assert!(sh.methods().is_empty() && !r.called("herdr"), "{:?}", sh.methods());
    }
    assert!(read(&c.state).unwrap_or_default().is_empty());
    assert_eq!(run(&c, &r, &["spawn", "sendmeter", "-", "impl"]).0, 1);
    assert_eq!(run(&c, &r, &["spawn", "sendmeter", "12", "boss"]).0, 1);
    assert!(sh.all("run").is_empty());
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
    let (_sh, r) = shed_happy(&c);
    let (exit, out) = spawn(&c, &r, &["--supersede"]);
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
    let (_sh, r) = shed_happy(&c);
    let (exit, out) = spawn(&c, &r, &[]);
    assert_eq!(exit, 0, "{out}");
    let brief = std::fs::read_to_string(wt(&c).join(".brief.md")).unwrap();
    assert!(
        brief.contains("sendmeter #12 decision q3 by owner: per API key"),
        "{brief}"
    );
}

#[test]
fn adopt_records_running_lane() {
    let d = tmpdir("adopt");
    let c = ctx(&d, CONFIG);
    let sh = FakeShed::new();
    sh.attach(&c);
    sh.lane("stray", "idle", "/elsewhere");
    sh.set("stray", |t| t["labels"]["harness"] = json!("hermes"));
    let r = Fake::default();
    let (exit, out) = run(&c, &r, &["adopt", "sendmeter", "12", "impl", "--pane", "stray"]);
    assert_eq!(exit, 0, "{out} {:?}", r.calls.borrow());
    assert!(sh.called("get", "stray"), "{:?}", sh.methods());
    let ev = read(&c.state).unwrap();
    let s = ev.iter().find(|e| e.kind == "spawn").expect("spawn logged");
    assert_eq!(
        (s.str("agent"), s.str("pane")),
        (Some("sendmeter-impl-12"), Some("stray"))
    );
    assert_eq!(s.val("adopted"), true);
    let (_, st) = run(&c, &Fake::default(), &["status"]);
    assert!(st.contains("sendmeter-impl-12"), "{st}");
    sh.exit("stray");
    assert_eq!(
        run(&c, &r, &["adopt", "sendmeter", "13", "impl", "--pane", "stray"]).0,
        1
    );
    assert!(read(&c.state).unwrap().iter().all(|e| e.issue != Some(13)));
}

#[test]
fn host_busy_creates_no_worktree_dir() {
    let d = tmpdir("host-busy-nothing");
    let c = ctx(&d, CONFIG);
    let (sh, r) = shed_happy(&c);
    let r = r.once("sysctl -n vm.loadavg", 0, "{ 26.90 3.00 2.00 }\n");
    std::fs::remove_dir_all(wt(&c)).unwrap();
    assert_refused(&c, &sh, &r, &[], "HOST_BUSY");
    assert!(!wt(&c).exists(), "no worktree dir");
}

#[test]
fn kick_failure_after_start_still_logs_unconfirmed_spawn() {
    let d = tmpdir("kick-fails");
    let c = ctx(&d, CONFIG);
    let (sh, r) = shed_happy(&c);
    sh.wait_ends(IMPL, "timeout");
    let (exit, out) = spawn(&c, &r, &[]);
    assert_eq!(exit, 4, "{out}");
    assert!(sh.has(IMPL), "the started lane is kept");
    let ev = read(&c.state).unwrap();
    let s = ev.iter().find(|e| e.kind == "spawn").expect("spawn logged");
    assert_eq!(
        (s.str("agent"), s.val("confirmed").clone()),
        (Some("sendmeter-impl-12"), json!(false))
    );
    assert_eq!(r.count("sleep 10"), 2, "three attempts");
}

#[test]
fn named_orphan_in_worktree_is_adopted_not_lane_live() {
    let d = tmpdir("dup-orphan");
    let c = ctx(&d, CONFIG);
    let (sh, r) = shed_happy(&c);
    sh.lane(IMPL, "working", wt(&c).to_str().unwrap());
    let (exit, out) = spawn(&c, &r, &[]);
    assert_eq!(exit, 0, "{out}");
    assert!(sh.all("run").is_empty(), "nothing started: {:?}", sh.methods());
    let ev = read(&c.state).unwrap();
    assert_eq!(ev.iter().filter(|e| e.kind == "spawn").count(), 1);
    assert_eq!((ev[0].str("pane"), ev[0].val("adopted")), (Some(IMPL), &json!(true)));
}

#[test]
fn supersede_closes_old_pane_after_confirm_unless_working() {
    for working in [false, true] {
        let d = tmpdir("supersede-old-pane");
        let c = ctx(&d, CONFIG);
        let w = wt(&c).to_string_lossy().into_owned();
        let d0 = json!({"role": "impl", "agent": "sendmeter-impl-12", "branch": "issue-12", "worktree": w, "pane": "old-pane"});
        append(&c.state, &Event::new(10, "sendmeter", Some(12), "spawn", d0)).unwrap();
        let (sh, r) = shed_happy(&c);
        if working {
            sh.lane("old-pane", "working", "/elsewhere");
        } else {
            sh.add("old-pane", json!({}));
        }
        let (exit, out) = spawn(&c, &r, &["--supersede"]);
        assert_eq!(exit, 0, "{out} {:?}", r.calls.borrow());
        let closed = read(&c.state).unwrap().iter().any(|e| e.kind == "lane-closed");
        let seen = sh.lock().seen.clone();
        let close = seen.iter().position(|s| s.0 == "kill" && s.1["name"] == "old-pane");
        if working {
            assert!(close.is_none() && !closed && sh.has("old-pane"), "{seen:?}");
        } else {
            let confirmed = seen.iter().rposition(|s| s.0 == "wait" && s.1["name"] == IMPL).unwrap();
            assert!(
                close.expect("old pane closed") > confirmed,
                "after the new lane is confirmed"
            );
            assert!(closed && !sh.has("old-pane"));
        }
    }
}
