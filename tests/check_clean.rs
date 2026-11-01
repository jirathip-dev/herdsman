//! M2: `herdsman check` alerts (once per state), `herdsman clean`, plain-text notifications.
mod common;
use common::*;
use herdsman::config::Config;
use herdsman::events::{append, emit, plain, read, Event};
use serde_json::json;

const HEAD: &str = "1111111111111111111111111111111111111111";
const NOW: u64 = 1_000_000;
const LANE: &str = "sendmeter-impl-12";

fn ev(c: &herdsman::Ctx, ts: u64, issue: u64, kind: &str, d: serde_json::Value) {
    append(&c.state, &Event::new(ts, "sendmeter", Some(issue), kind, d)).unwrap();
}

fn spawn_ev(c: &herdsman::Ctx, ts: u64, issue: u64, agent: &str) {
    ev(
        c,
        ts,
        issue,
        "spawn",
        json!({"role": "impl", "agent": agent, "branch": format!("issue-{issue}")}),
    );
}

fn kinds(c: &herdsman::Ctx, kind: &str) -> usize {
    read(&c.state).unwrap().iter().filter(|e| e.kind == kind).count()
}

fn check(c: &herdsman::Ctx, r: &Fake) -> String {
    let (code, out) = run(c, r, &["check"]);
    assert_eq!(code, 0, "{out}");
    out
}

#[test]
fn stall_fires_once_and_again_after_state_change() {
    let d = tmpdir("stall");
    let mut c = ctx(&d, CONFIG);
    spawn_ev(&c, NOW - 3600, 12, LANE);
    let r = Fake::default().reply("herdr agent list", &agents(&[(LANE, "idle", "/x", "w9:p1")]));
    let out = check(&c, &r);
    assert!(out.contains("sendmeter #12 stall: no event for 60m in impl"), "{out}");
    assert_eq!(kinds(&c, "stall"), 1);
    assert_eq!(check(&c, &r), "", "no re-fire on the next run");
    assert_eq!(kinds(&c, "stall"), 1);
    // A new event resets the clock; once it is stale again, it fires again.
    ev(&c, NOW + 10, 12, "spawn", json!({"role": "impl", "agent": LANE}));
    assert_eq!(check(&c, &r), "", "fresh activity");
    c.now = NOW + 10 + 3600;
    assert!(check(&c, &r).contains("stall"));
    assert_eq!(kinds(&c, "stall"), 2);
    assert_eq!(kinds(&c, "lane-dead"), 0);
}

#[test]
fn working_lane_is_not_a_stall() {
    let d = tmpdir("stall-working");
    let c = ctx(&d, CONFIG);
    spawn_ev(&c, NOW - 3600, 12, LANE);
    let r = Fake::default().reply("herdr agent list", &agents(&[(LANE, "working", "/x", "w9:p1")]));
    assert_eq!(check(&c, &r), "");
    assert_eq!(read(&c.state).unwrap().len(), 1);
}

#[test]
fn lane_dead_fires_once_and_again_after_state_change() {
    let d = tmpdir("lane-dead");
    let c = ctx(&d, CONFIG);
    spawn_ev(&c, NOW - 60, 12, LANE);
    let r = Fake::default().reply("herdr agent list", NO_AGENTS);
    let out = check(&c, &r);
    assert!(
        out.contains("sendmeter #12 lane-dead: impl lane sendmeter-impl-12 is gone"),
        "{out}"
    );
    assert_eq!(check(&c, &r), "");
    assert_eq!(kinds(&c, "lane-dead"), 1);
    assert_eq!(kinds(&c, "stall"), 0, "not stale yet");
    spawn_ev(&c, NOW - 30, 12, "sendmeter-impl-12b");
    assert!(check(&c, &r).contains("sendmeter-impl-12b is gone"));
    assert_eq!(kinds(&c, "lane-dead"), 2);
}

fn ready_fake(head: &str) -> Fake {
    let checks = json!([{"__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "SUCCESS"}]);
    let pr = json!({"number": 34, "state": "OPEN", "baseRefName": "staging", "headRefOid": head, "statusCheckRollup": checks});
    Fake::default()
        .reply("herdr agent list", NO_AGENTS)
        .reply("gh pr view issue-12", &pr.to_string())
}

#[test]
fn ready_unmerged_fires_once_and_again_after_state_change() {
    let d = tmpdir("ready");
    let mut c = ctx(&d, CONFIG);
    spawn_ev(&c, NOW - 7200, 12, LANE);
    ev(&c, NOW - 3600, 12, "verdict", json!({"verdict": "PASS", "sha": HEAD}));
    let r = ready_fake(HEAD);
    let out = check(&c, &r);
    assert!(
        out.contains("sendmeter #12 ready-unmerged: PASS @ 1111111 with green CI"),
        "{out}"
    );
    assert_eq!(check(&c, &r), "");
    assert_eq!(kinds(&c, "ready-unmerged"), 1);
    assert_eq!(
        kinds(&c, "lane-dead") + kinds(&c, "stall"),
        0,
        "a ready issue has no active lane"
    );
    // PASS at an older head: not ready-unmerged.
    let d2 = tmpdir("ready-stale");
    let c2 = ctx(&d2, CONFIG);
    spawn_ev(&c2, NOW - 7200, 12, LANE);
    ev(
        &c2,
        NOW - 3600,
        12,
        "verdict",
        json!({"verdict": "PASS", "sha": "2".repeat(40)}),
    );
    assert_eq!(check(&c2, &r), "");
    // New PASS (state change) → fires again once stale.
    let head2 = "3".repeat(40);
    ev(&c, NOW + 5, 12, "verdict", json!({"verdict": "PASS", "sha": head2}));
    c.now = NOW + 5 + 3600;
    assert!(check(&c, &ready_fake(&head2)).contains("ready-unmerged"));
    assert_eq!(kinds(&c, "ready-unmerged"), 2);
}

#[test]
fn merged_issues_are_never_flagged() {
    let d = tmpdir("check-merged");
    let c = ctx(&d, CONFIG);
    spawn_ev(&c, NOW - 90_000, 12, LANE);
    ev(&c, NOW - 80_000, 12, "verdict", json!({"verdict": "PASS", "sha": HEAD}));
    ev(
        &c,
        NOW - 70_000,
        12,
        "merged",
        json!({"pr": "34", "merge_sha": "abc", "into": "staging"}),
    );
    let r = ready_fake(HEAD);
    assert_eq!(check(&c, &r), "");
    assert_eq!(read(&c.state).unwrap().len(), 3);
}

// --- notifications ---

#[test]
fn plain_lines_for_merged_stall_needs_human() {
    let e = |kind: &str, d| Event::new(1, "sendmeter", Some(1037), kind, d);
    let merged = e(
        "merged",
        json!({"pr": "1041", "merge_sha": "8ed4a064abcdef", "into": "staging"}),
    );
    assert_eq!(plain(&merged), "sendmeter #1037 merged → staging 8ed4a06 (PR #1041)");
    let stall = e(
        "stall",
        json!({"key": "k", "reason": "no event for 45m in impl; impl lane x is idle"}),
    );
    assert_eq!(
        plain(&stall),
        "sendmeter #1037 stall: no event for 45m in impl; impl lane x is idle"
    );
    let nh = e("needs-human", json!({"reason": "FAIL in round 3/3"}));
    assert_eq!(plain(&nh), "sendmeter #1037 needs-human: FAIL in round 3/3");
}

#[test]
fn notifier_gets_plain_line_unless_format_json() {
    let d = tmpdir("notify-plain");
    let (a, b) = (d.join("plain.txt"), d.join("raw.json"));
    let cfg = format!(
        "{CONFIG}\n[[notify]]\ncmd = \"cat > '{}'\"\non = [\"merged\"]\n\n[[notify]]\ncmd = \"cat > '{}'\"\non = [\"merged\"]\nformat = \"json\"\n",
        a.display(),
        b.display()
    );
    let cfg = Config::parse(&cfg).unwrap();
    let merged = Event::new(
        1,
        "sendmeter",
        Some(1037),
        "merged",
        json!({"pr": "1041", "merge_sha": "8ed4a064", "into": "staging"}),
    );
    emit(&d.join("events.jsonl"), &cfg, merged).unwrap();
    assert_eq!(
        std::fs::read_to_string(&a).unwrap(),
        "sendmeter #1037 merged → staging 8ed4a06 (PR #1041)\n"
    );
    let raw: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&b).unwrap()).unwrap();
    assert_eq!(
        (raw["type"].as_str(), raw["merge_sha"].as_str()),
        (Some("merged"), Some("8ed4a064"))
    );
}

// --- clean ---

#[test]
fn clean_removes_only_merged_clean_idle_lanes() {
    let d = tmpdir("clean");
    let c = ctx(&d, CONFIG);
    let wt = |n: &str| {
        let p = c.worktrees.join("sendmeter").join(n);
        std::fs::create_dir_all(&p).unwrap();
        p.to_string_lossy().into_owned()
    };
    let (a, b, open, unpushed, live) = (
        wt("issue-12"),
        wt("rev-12-r1"),
        wt("issue-13"),
        wt("issue-14"),
        wt("issue-15"),
    );
    let lane = |ts, issue, role: &str, w: &str, ws: &str| {
        let d = json!({"role": role, "agent": format!("{role}-{issue}"), "worktree": w, "pane": format!("{ws}:p1"), "workspace": ws});
        ev(&c, ts, issue, "spawn", d);
    };
    let merged = |issue| {
        ev(
            &c,
            50,
            issue,
            "merged",
            json!({"pr": "1", "merge_sha": "m", "into": "staging"}),
        )
    };
    lane(10, 12, "impl", &a, "wA");
    lane(20, 12, "rev", &b, "wB");
    merged(12);
    lane(10, 13, "impl", &open, "wC");
    lane(10, 14, "impl", &unpushed, "wD");
    merged(14);
    lane(10, 15, "impl", &live, "wE");
    merged(15);
    let git = |w: &str, cmd: &str| format!("git -C {w} {cmd}");
    let r = || {
        Fake::default()
            .reply("herdr agent list", &agents(&[("someone", "idle", &live, "wE:p1")]))
            .reply(&git(&b, "status"), "?? verdict.md\n")
            .reply(&git(&unpushed, "rev-list"), "2\n")
            .reply(&git(&a, "rev-list"), "0\n")
            .reply(&git(&b, "rev-list"), "0\n")
            .once(&git(&unpushed, "check-ignore -q dist"), 1, "")
            .reply("git -C", "")
            .reply("herdr", "{}")
    };
    let r0 = r();
    let (code, dry) = run(&c, &r0, &["clean"]);
    assert_eq!(code, 0, "{dry}");
    assert!(
        dry.contains(&format!("would remove {a}")) && dry.contains(&format!("would remove {b}")),
        "{dry}"
    );
    assert!(dry.contains(&format!("keep {unpushed}: 2 unpushed commit(s)")), "{dry}");
    assert!(dry.contains(&format!("keep {live}: live agent")), "{dry}");
    assert!(!dry.contains(&open), "unmerged lanes are not candidates: {dry}");
    assert!(
        !r0.called("herdr worktree remove") && !r0.called("herdr pane close"),
        "dry run changes nothing"
    );
    assert_eq!(kinds(&c, "clean"), 0);

    // #103: build folders go for merged lanes, even kept (unpushed) ones; a live lane keeps its own.
    let build = |w: &str, d: &str| {
        let p = std::path::Path::new(w).join(d).join("deep");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("blob"), [0u8; 1000]).unwrap();
        p.parent().unwrap().to_path_buf()
    };
    let (gone, kept_wt, live_dir, open_dir) = (
        build(&a, "target"),
        build(&unpushed, "node_modules"),
        build(&live, "target"),
        build(&open, "dist"),
    );
    let (code, dry) = run(&c, &r(), &["clean"]);
    assert_eq!(code, 0, "{dry}");
    let tracked = build(&unpushed, "dist");
    let all = [&gone, &kept_wt, &live_dir, &open_dir, &tracked];
    assert!(
        all.iter().all(|p| p.exists()) && !dry.contains("freed"),
        "a dry run frees nothing: {dry}"
    );
    let r1 = r();
    let (code, done) = run(&c, &r1, &["clean", "--yes"]);
    assert_eq!(code, 0, "{done}");
    assert!(!gone.exists() && !kept_wt.exists(), "merged lanes' build folders go");
    assert!(
        live_dir.exists() && open_dir.exists(),
        "live and unmerged lanes keep theirs: {done}"
    );
    assert!(tracked.exists(), "a dist/ git does not ignore (tracked) stays");
    assert!(
        done.contains(&format!("keep {unpushed}/dist: not git-ignored")),
        "{done}"
    );
    assert!(done.contains("freed 2000 bytes of build folders"), "{done}");
    assert!(r1.called("herdr worktree remove --workspace wA") && r1.called("herdr worktree remove --workspace wB"));
    assert!(r1.called("herdr pane close wA:p1"));
    for ws in ["wC", "wD", "wE"] {
        assert!(
            !r1.called(&format!("herdr worktree remove --workspace {ws}")),
            "{ws} must stay"
        );
    }
    let cleaned: Vec<_> = read(&c.state)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == "clean")
        .map(|e| e.str("worktree").unwrap().to_string())
        .collect();
    assert_eq!(cleaned.len(), 2);
    assert!(cleaned.contains(&a) && cleaned.contains(&b));
}

/// Real git; herdr is scripted (no agents, workspace already gone).
struct GitHerdr(Fake);

impl herdsman::runner::Runner for GitHerdr {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<herdsman::runner::Out> {
        if prog == "git" {
            return herdsman::runner::Real.run(prog, args);
        }
        self.0.run(prog, args)
    }
}

#[test]
fn clean_falls_back_to_git_dropping_only_fleets_own_files_and_keeps_going() {
    let d = tmpdir("clean-git");
    let git = |dir: &std::path::Path, args: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    };
    let (origin, local) = (d.join("origin.git"), d.join("local"));
    git(&d, &["init", "-q", "--bare", origin.to_str().unwrap()]);
    git(&d, &["clone", "-q", origin.to_str().unwrap(), local.to_str().unwrap()]);
    git(&local, &["commit", "-q", "--allow-empty", "-m", "init"]);
    git(&local, &["push", "-q", "origin", "HEAD"]);
    let c = ctx(
        &d,
        &CONFIG.replace("/tmp/does-not-matter/sendmeter", local.to_str().unwrap()),
    );
    let wt = |n: &str, files: &[&str]| {
        let p = c.worktrees.join("sendmeter").join(n);
        git(&local, &["worktree", "add", "-q", "-b", n, p.to_str().unwrap()]);
        for f in files {
            std::fs::write(p.join(f), "x").unwrap();
        }
        p
    };
    let ours = wt("issue-12", &[".brief.md", "verdict.md"]);
    let user = wt("issue-13", &[".brief.md", "notes.txt"]);
    let locked = wt("issue-14", &[".brief.md"]);
    git(&local, &["worktree", "lock", locked.to_str().unwrap()]);
    // Newest spawn is visited first: the locked one fails before the others are tried.
    for (ts, issue, p) in [(10, 12, &ours), (11, 13, &user), (12, 14, &locked)] {
        let w = p.to_str().unwrap();
        ev(
            &c,
            ts,
            issue,
            "spawn",
            json!({"role": "impl", "agent": format!("impl-{issue}"), "worktree": w, "pane": format!("w{issue}:p1"), "workspace": format!("w{issue}")}),
        );
        ev(
            &c,
            50,
            issue,
            "merged",
            json!({"pr": "1", "merge_sha": "m", "into": "staging"}),
        );
    }
    let r = || GitHerdr(Fake::default().reply("herdr agent list", NO_AGENTS));
    let (code, dry) = run(&c, &r(), &["clean"]);
    assert_eq!(code, 0, "{dry}");
    let (ours_s, user_s, locked_s) = (
        ours.display().to_string(),
        user.display().to_string(),
        locked.display().to_string(),
    );
    assert!(
        dry.contains(&format!("would remove {ours_s}")) && dry.contains(&format!("would remove {locked_s}")),
        "{dry}"
    );
    assert!(
        dry.contains(&format!("keep {user_s}: 1 uncommitted change(s)")),
        "{dry}"
    );

    let (code, done) = run(&c, &r(), &["clean", "--yes"]);
    assert_ne!(code, 0, "a failed candidate makes the run fail: {done}");
    assert!(done.contains(&format!("failed {locked_s}: ")), "{done}");
    assert!(
        done.contains(&format!("removed {ours_s}")),
        "loop continues past the failure: {done}"
    );
    assert!(!ours.exists(), "fleet's own files no longer block removal");
    assert!(
        user.join("notes.txt").exists() && user.join(".brief.md").exists(),
        "unknown dirt is never touched"
    );
    assert!(locked.exists(), "the failed candidate is left in place");
    assert_eq!(kinds(&c, "clean"), 1);
}

#[test]
fn clean_reclaims_lanes_whose_issue_closed_outside_merge() {
    let d = tmpdir("clean-closed");
    let c = ctx(&d, CONFIG);
    let wt = |n: &str| {
        let p = c.worktrees.join("sendmeter").join(n);
        std::fs::create_dir_all(&p).unwrap();
        p.to_string_lossy().into_owned()
    };
    let (done, unpushed, open) = (wt("issue-21"), wt("issue-22"), wt("issue-23"));
    for (issue, w) in [(21, &done), (22, &unpushed), (23, &open)] {
        let d = json!({"role": "impl", "agent": format!("impl-{issue}"), "worktree": w, "pane": format!("w{issue}:p1"), "workspace": format!("w{issue}")});
        ev(&c, 10, issue, "spawn", d);
    }
    let r = || {
        Fake::default()
            .reply("herdr agent list", NO_AGENTS)
            .reply(
                "gh issue list -R jirathip-dev/sendmeter --state closed",
                r#"[{"number": 21}, {"number": 22}, {"number": 99}]"#,
            )
            .reply(&format!("git -C {unpushed} rev-list"), "1\n")
            .reply(&format!("git -C {done} rev-list"), "0\n")
            .reply("git -C", "")
            .reply("herdr", "{}")
    };
    let r0 = r();
    let (code, dry) = run(&c, &r0, &["clean"]);
    assert_eq!(code, 0, "{dry}");
    assert!(
        dry.contains(&format!("would remove {done} (pane w21:p1, sendmeter#21 issue closed)")),
        "{dry}"
    );
    assert!(dry.contains(&format!("keep {unpushed}: 1 unpushed commit(s)")), "{dry}");
    assert!(!dry.contains(&open), "open issues are not candidates: {dry}");
    assert_eq!(r0.count("gh "), 1, "one closed-issue read per fleet, not per lane");
    assert!(
        !r0.calls
            .borrow()
            .iter()
            .any(|l| l.starts_with("gh ") && !l.starts_with("gh issue list")),
        "no GitHub writes"
    );

    let r1 = r();
    let (code, out) = run(&c, &r1, &["clean", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(&format!("removed {done} (sendmeter#21 issue closed)")),
        "{out}"
    );
    assert!(r1.called("herdr worktree remove --workspace w21") && r1.called("herdr pane close w21:p1"));
    assert!(!r1.called("herdr worktree remove --workspace w22") && !r1.called("herdr worktree remove --workspace w23"));
    assert_eq!(r1.count("gh "), 0, "the closed-issue read is cached");
    assert_eq!(kinds(&c, "clean"), 1);
}

#[test]
fn orphan_lane_fires_once_per_agent() {
    let d = tmpdir("orphan-lane");
    let c = ctx(&d, CONFIG);
    let rows = [
        ("sendmeter-rev-93-r1", "working", "/w", "w6:p1"),
        ("sendmeter-orch", "idle", "/w", "w1:p1"),
        ("other-impl-5", "idle", "/w", "w2:p1"),
        (LANE, "working", "/w", "w9:p1"),
    ];
    spawn_ev(&c, NOW - 60, 12, LANE);
    let r = Fake::default().reply("herdr agent list", &agents(&rows));
    let out = check(&c, &r);
    assert!(
        out.contains("sendmeter #93 orphan-lane: herdr agent sendmeter-rev-93-r1 has no spawn event"),
        "{out}"
    );
    assert_eq!(
        kinds(&c, "orphan-lane"),
        1,
        "the logged lane, orch and other fleets are no orphans"
    );
    assert_eq!(check(&c, &r), "", "once");
}

/// #128: a verdict closes the reviewer's pane and removes its worktree; a dirty one stays, with the reason logged;
/// one herdr still shows working is untouched.
#[test]
fn verdict_closes_the_reviewer_lane_unless_dirty_or_working() {
    let working = agents(&[("sendmeter-rev-12-r1", "working", "/x", "wR:p1")]);
    for (status, list, kind) in [
        ("?? verdict.md\n", NO_AGENTS, "lane-closed"),
        (" M src/x.rs\n", NO_AGENTS, "lane-kept"),
        ("?? verdict.md\n", working.as_str(), "verdict"),
    ] {
        let d = tmpdir("verdict-close");
        let c = ctx(&d, CONFIG);
        let w = c.worktrees.join("sendmeter/rev-12-r1");
        std::fs::create_dir_all(&w).unwrap();
        let w = w.to_string_lossy().into_owned();
        spawn_ev(&c, 10, 12, LANE);
        let rev =
            json!({"role": "rev", "agent": "sendmeter-rev-12-r1", "worktree": w, "pane": "wR:p1", "workspace": "wR"});
        ev(&c, 20, 12, "spawn", rev);
        let f = d.join("verdict.md");
        std::fs::write(&f, "FAIL\n1. blocking\n").unwrap();
        let r = Fake::default()
            .reply("gh api repos/jirathip-dev/sendmeter/branches/issue-12", HEAD)
            .reply(&format!("git -C {w} status"), status)
            .reply(&format!("git -C {w} rev-list"), "0\n")
            .reply("herdr agent list", list)
            .reply("herdr", "{}");
        let args = [
            "verdict",
            "sendmeter",
            "12",
            "FAIL",
            "--sha",
            HEAD,
            "--file",
            f.to_str().unwrap(),
        ];
        let (code, out) = run(&c, &r, &args);
        assert_eq!(code, 0, "{out}");
        let e = read(&c.state).unwrap().pop().unwrap();
        assert_eq!(e.kind, kind, "{out}");
        let gone = kind == "lane-closed";
        assert_eq!(r.called("herdr pane close wR:p1"), gone, "{out}");
        assert_eq!(r.called("herdr worktree remove --workspace wR"), gone, "{out}");
        if kind == "verdict" {
            continue;
        }
        assert_eq!(e.str("agent"), Some("sendmeter-rev-12-r1"));
        assert_eq!(
            e.str("reason").unwrap(),
            if gone {
                "verdict recorded".to_string()
            } else {
                format!("{w}: 1 uncommitted change(s)")
            }
        );
        let st = &herdsman::events::fold(&read(&c.state).unwrap())[&("sendmeter".to_string(), 12)];
        assert_eq!(
            (st.state.as_str(), st.last_event.as_str()),
            ("fix(1)", "verdict"),
            "history kept"
        );
    }
}
