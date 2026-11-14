mod common;
use common::*;
use drover::events::{append, fold, read, Event};
use serde_json::json;
use std::path::PathBuf;

fn sha(p: &str) -> String {
    format!("{p}{}", "0".repeat(40 - p.len()))
}

fn ev(c: &drover::Ctx, ts: u64, kind: &str, d: serde_json::Value) {
    append(&c.state, &Event::new(ts, "sendmeter", Some(12), kind, d)).unwrap();
}

fn pr(head: &str) -> String {
    let check = json!({"__typename": "CheckRun", "name": "ci", "status": "IN_PROGRESS"});
    json!({"number": 100, "state": "OPEN", "baseRefName": "staging", "headRefOid": head, "statusCheckRollup": [check]})
        .to_string()
}

fn shed_ctx(name: &str, extra: &str) -> (drover::Ctx, FakeShed) {
    let sh = FakeShed::new();
    let c = ctx(&tmpdir(name), &sh.config(&format!("{CONFIG}{extra}")));
    (c, sh)
}

fn at(sh: &FakeShed, method: &str, name: &str) -> Option<usize> {
    let st = sh.lock();
    st.seen.iter().position(|(m, p)| m == method && p["name"] == name)
}

fn fake() -> Fake {
    Fake::default()
        .reply("sysctl -n kern.memorystatus_vm_pressure_level", "1\n")
        .reply("sysctl -n vm.loadavg", "{ 1.50 1.20 1.00 }\n")
        .reply("sysctl -n hw.ncpu", "10\n")
        .reply("vm_stat", "Pages free: 1.\nSwapouts:                     100.\n")
        .reply("gh issue view 12", r#"{"title":"t","body":"b"}"#)
}

fn fast(c: &drover::Ctx, r: &dyn drover::runner::Runner) -> String {
    let (code, out) = run(c, r, &["drive", "--fast"]);
    assert_eq!(code, 0, "{out}");
    out
}

fn kinds(c: &drover::Ctx, kind: &str) -> Vec<Event> {
    read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

fn review(c: &drover::Ctx, verdict: &str) -> PathBuf {
    let wt = c.worktrees.join("sendmeter/issue-12");
    std::fs::create_dir_all(&wt).unwrap();
    let d = json!({"role": "impl", "agent": "sendmeter-impl-12", "pane": "sendmeter-impl-12", "worktree": wt,
        "branch": "issue-12"});
    ev(c, 10, "spawn", d);
    let rev = c.worktrees.join("sendmeter/rev-12-r1");
    std::fs::create_dir_all(&rev).unwrap();
    std::fs::write(rev.join("verdict.md"), verdict).unwrap();
    rev
}

#[test]
fn spawn_logs_spawn_started_before_the_harness_starts() {
    let sh = FakeShed::new();
    let cfg = CONFIG.replace(
        "harness  =  \"hermes\"    # toggled in tests",
        "harness = \"claude\"\naccount = \"C\"",
    );
    let cfg = format!(
        "{cfg}\n[fleet.sendmeter.runtime]\nkind = \"shed-acp\"\nsocket = \"{}\"\n\n[accounts]\nC = \"C\"\n",
        sh.sock.display()
    );
    let mut c = ctx(&tmpdir("orphan-started"), &cfg);
    c.exe = env!("CARGO_BIN_EXE_drover").into();
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    let f = fake()
        .reply("which shed", "/opt/shed/bin/shed\n")
        .reply("git -C", "")
        .reply(
            "shed agents --json",
            r#"[{"id": "claude-acp", "cmd": ["claude-agent-acp"]}]"#,
        )
        .once_err("shed run", 1, "claude-agent-acp: command not found");
    let r = sh.cli(&f);
    let (code, out) = run(&c, &r, &["spawn", "sendmeter", "12", "impl"]);
    assert_eq!(code, 1, "{out} {:?}", f.calls.borrow());
    assert!(f.called("shed run --acp"), "the harness start was tried");
    let st = &kinds(&c, "spawn-started")[0];
    assert_eq!(
        (st.str("agent"), st.str("role"), st.str("pane")),
        (Some("sendmeter-impl-12"), Some("impl"), Some("sendmeter-impl-12"))
    );
    assert_eq!(
        PathBuf::from(st.str("worktree").unwrap()),
        c.worktrees.join("sendmeter/issue-12")
    );
    assert!(kinds(&c, "spawn").is_empty(), "the start failed: no spawn");
    assert_eq!(
        fold(&read(&c.state).unwrap())[&("sendmeter".into(), 12)].state,
        "queued"
    );
}

#[test]
fn a_crash_after_spawn_started_is_adopted_by_the_next_step() {
    let (c, sh) = shed_ctx("orphan-crash", "");
    let wt = c.worktrees.join("sendmeter/issue-12");
    std::fs::create_dir_all(&wt).unwrap();
    ev(
        &c,
        50,
        "spawn-started",
        json!({"agent": "sendmeter-impl-12", "role": "impl", "worktree": wt, "pane": "sendmeter-impl-12"}),
    );
    sh.lane("sendmeter-impl-12", "working", &wt.to_string_lossy());
    let r = fake();
    let out = fast(&c, &r);
    assert!(out.contains("adopted sendmeter-impl-12 as sendmeter-impl-12"), "{out}");
    let sp = &kinds(&c, "spawn")[0];
    assert_eq!(
        (sp.str("pane"), sp.str("why"), sp.val("since").as_u64()),
        (Some("sendmeter-impl-12"), Some("orphan"), Some(50))
    );
    assert_eq!(kinds(&c, "spawn-adopted")[0].str("why"), Some("orphan"));
    assert_eq!(fold(&read(&c.state).unwrap())[&("sendmeter".into(), 12)].state, "impl");
    fast(&c, &r);
    assert_eq!(kinds(&c, "spawn").len(), 1);
}

#[test]
fn a_finished_orphan_review_at_the_head_is_recorded_then_closed() {
    let (c, sh) = shed_ctx("orphan-review", "");
    let head = sha("abc");
    let rev = review(&c, &format!("PASS\nReviewed head: {head}\nall good\n"));
    let name = "sendmeter-rev-12-r1";
    sh.lane(name, "done", &rev.to_string_lossy());
    let r = fake()
        .reply("gh pr view issue-12", &pr(&head))
        .reply(
            "gh api repos/jirathip-dev/sendmeter/branches/issue-12",
            &format!("{head}\n"),
        )
        .reply(&format!("git -C {} rev-list", rev.display()), "0\n")
        .reply("git -C", "");
    let out = fast(&c, &r);
    let adopted = &kinds(&c, "spawn")[1];
    assert_eq!((adopted.str("role"), adopted.str("why")), (Some("rev"), Some("orphan")));
    let v = &kinds(&c, "verdict")[0];
    assert_eq!(
        (v.str("verdict"), v.str("sha")),
        (Some("PASS"), Some(head.as_str())),
        "{out}"
    );
    let closed = kinds(&c, "lane-closed");
    assert_eq!(
        closed.first().and_then(|e| e.str("reason")),
        Some("verdict recorded"),
        "{out} {:?}",
        read(&c.state).unwrap().to_vec()
    );
    let (get, kill) = (at(&sh, "get", name).unwrap(), at(&sh, "kill", name).unwrap());
    assert!(kill > get, "{:?}", sh.methods());
    assert!(!sh.has(name), "removed");
}

#[test]
fn a_stale_head_orphan_review_is_closed_not_recorded() {
    let (c, sh) = shed_ctx("orphan-stale", "");
    let rev = review(&c, &format!("PASS\nReviewed head: {}\n", sha("0d")));
    let name = "sendmeter-rev-12-r1";
    sh.lane(name, "done", &rev.to_string_lossy());
    let r = fake()
        .reply("gh pr view issue-12", &pr(&sha("bee")))
        .reply(&format!("git -C {} rev-list", rev.display()), "1\n")
        .reply("git -C", "");
    let out = fast(&c, &r);
    assert!(
        out.contains("closed stale orphan sendmeter-rev-12-r1 (verdict for 0d00000, head is bee0000)"),
        "{out}"
    );
    let closed = &kinds(&c, "lane-closed")[0];
    assert_eq!(
        (closed.str("why"), closed.str("pane")),
        (Some("stale-orphan"), Some(name))
    );
    let push = format!(
        "git -C {} push origin HEAD:refs/heads/wip/sendmeter-rev-12-",
        rev.display()
    );
    assert!(r.called(&push), "{:?}", r.calls.borrow());
    assert!(sh.called("kill", name) && !sh.has(name), "{:?}", sh.methods());
    assert!(
        kinds(&c, "verdict").is_empty() && kinds(&c, "spawn").len() == 1,
        "not recorded, not adopted"
    );
    assert!(!sh.called("get", name), "never adopted: {:?}", sh.methods());
}

#[test]
fn a_merged_issues_orphan_is_closed() {
    let (c, sh) = shed_ctx("orphan-merged", "");
    let wt = c.worktrees.join("sendmeter/issue-12");
    std::fs::create_dir_all(&wt).unwrap();
    ev(&c, 10, "merged", json!({}));
    sh.add("t7", json!({"fleet": "sendmeter"}));
    sh.set("t7", |t| t["cwd"] = json!(wt));
    let r = fake().reply("git -C", "");
    let out = fast(&c, &r);
    assert!(out.contains("closed stale orphan t7 (sendmeter#12 is merged)"), "{out}");
    assert_eq!(kinds(&c, "lane-closed")[0].str("why"), Some("stale-orphan"));
    assert!(sh.called("kill", "t7") && !sh.has("t7"), "{:?}", sh.methods());
}

#[test]
fn an_unmatched_pane_is_only_alerted() {
    let (c, sh) = shed_ctx("orphan-unmatched", "");
    let wt = c.worktrees.join("sendmeter/issue-12");
    ev(
        &c,
        50,
        "spawn-started",
        json!({"agent": "sendmeter-impl-12", "role": "impl", "pane": "sendmeter-impl-12"}),
    );
    let labels = json!({"fleet": "sendmeter", "issue": "99", "role": "impl"});
    sh.add("sendmeter-impl-99", labels);
    sh.set("sendmeter-impl-99", |t| t["cwd"] = json!("/tmp/elsewhere"));
    sh.lane("sendmeter-impl-12", "working", &wt.to_string_lossy());
    let r = fake().reply("git -C", "");
    assert_eq!(run(&c, &r, &["check"]).0, 0);
    let alerts = kinds(&c, "orphan-lane");
    assert_eq!(alerts.len(), 1);
    assert_eq!(
        (alerts[0].issue, alerts[0].str("key")),
        (Some(99), Some("sendmeter-impl-99"))
    );
    fast(&c, &r);
    let other = "sendmeter-impl-99";
    assert!(
        !sh.called("kill", other) && !sh.called("rm", other) && sh.has(other),
        "{:?}",
        sh.methods()
    );
    assert!(kinds(&c, "spawn").iter().all(|e| e.issue == Some(12)));
}

const HOSTS: &str = "\n[hosts.mini]\n\n[hosts.bazzite]\nmachine = \"bazzite\"\n";

#[test]
fn a_remote_orphan_is_adopted_on_its_host() {
    let (c, sh) = shed_ctx("orphan-remote", HOSTS);
    ev(&c, 10, "queued", json!({}));
    let wt = "/home/me/.herdr/worktrees/sendmeter/issue-12";
    let baz = FakeShed::new();
    baz.lane("sendmeter-impl-12", "working", wt);
    let f = fake();
    let r = OverSsh {
        f: &f,
        hosts: vec![("bazzite", &baz)],
    };
    let out = fast(&c, &r);
    assert!(
        out.contains("adopted sendmeter-impl-12"),
        "{out} {:?}",
        f.calls.borrow()
    );
    let sp = &kinds(&c, "spawn")[0];
    assert_eq!(
        (sp.str("host"), sp.str("machine"), sp.str("worktree")),
        (Some("bazzite"), Some("bazzite"), Some(wt))
    );
    assert!(baz.called("get", "sendmeter-impl-12"), "{:?}", baz.methods());
    assert!(!sh.called("get", "sendmeter-impl-12"), "never adopted on this host");
}

struct Racing<'a>(&'a OverSsh<'a>, &'a drover::Ctx, &'a str);

impl Racing<'_> {
    fn race(&self, line: &str, input: &str) {
        let listing = line.contains("bazzite") && line.contains("connect") && input.contains("\"ls\"");
        if listing && kinds(self.1, "spawn-started").is_empty() {
            let d = json!({"agent": "sendmeter-impl-12", "role": "impl", "worktree": self.2,
                "pane": "sendmeter-impl-12", "host": "bazzite", "machine": "bazzite"});
            ev(self.1, self.1.now, "spawn-started", d);
        }
    }
}

impl drover::runner::Runner for Racing<'_> {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<drover::runner::Out> {
        self.0.run(prog, args)
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> anyhow::Result<drover::runner::Out> {
        let line = format!("{prog} {}", args.join(" "));
        self.race(&line, &format!("{line}{input}"));
        self.0.run_in(prog, args, input)
    }
    fn sleep(&self, secs: u64) {
        self.0.sleep(secs);
    }
}

#[test]
fn a_lane_spawned_during_the_remote_listing_is_not_adopted() {
    let (c, _sh) = shed_ctx("orphan-race", HOSTS);
    ev(&c, 10, "queued", json!({}));
    append(&c.state, &Event::new(10, "sendmeter", Some(13), "queued", json!({}))).unwrap();
    let (wt, wt13) = (
        "/home/me/.herdr/worktrees/sendmeter/issue-12",
        "/home/me/.herdr/worktrees/sendmeter/issue-13",
    );
    let baz = FakeShed::new();
    baz.lane("sendmeter-impl-12", "working", wt);
    baz.lane("sendmeter-impl-13", "working", wt13);
    let f = fake();
    let over = OverSsh {
        f: &f,
        hosts: vec![("bazzite", &baz)],
    };
    let (code, out) = run(&c, &Racing(&over, &c, wt), &["drive", "--fast"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(kinds(&c, "spawn-started").len(), 1, "logged during the listing");
    assert!(!baz.called("get", "sendmeter-impl-12"), "{:?}", baz.methods());
    let spawns = kinds(&c, "spawn");
    assert_eq!(
        spawns.iter().map(|e| (e.issue, e.str("why"))).collect::<Vec<_>>(),
        [(Some(13), Some("orphan"))],
        "{out}"
    );
    let adopted = kinds(&c, "spawn-adopted");
    assert_eq!(adopted.iter().map(|e| e.issue).collect::<Vec<_>>(), [Some(13)]);
}

#[test]
fn an_orphan_rev_lane_of_the_wrong_round_is_closed() {
    let (c, sh) = shed_ctx("orphan-812-round", "");
    review(&c, "");
    let rev = c.worktrees.join("sendmeter/rev-12-r3");
    std::fs::create_dir_all(&rev).unwrap();
    let name = "sendmeter-rev-12-r3";
    sh.lane(name, "idle", &rev.to_string_lossy());
    let r = fake().reply("git -C", "");
    let out = fast(&c, &r);
    assert!(
        out.contains("closed stale orphan sendmeter-rev-12-r3 (round r3, r1 is due)"),
        "{out}"
    );
    assert_eq!(kinds(&c, "lane-closed")[0].str("why"), Some("stale-orphan"));
    assert_eq!(kinds(&c, "spawn").len(), 1, "never adopted");
    assert!(sh.called("kill", name) && !sh.has(name), "{:?}", sh.methods());
}
