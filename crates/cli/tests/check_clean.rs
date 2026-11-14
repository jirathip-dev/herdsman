mod common;
use common::*;
use drover::config::Config;
use drover::events::{append, emit, plain, read, Event};
use serde_json::json;

const HEAD: &str = "1111111111111111111111111111111111111111";
const NOW: u64 = 1_000_000;
const LANE: &str = "sendmeter-impl-12";

fn ev(c: &drover::Ctx, ts: u64, issue: u64, kind: &str, d: serde_json::Value) {
    append(&c.state, &Event::new(ts, "sendmeter", Some(issue), kind, d)).unwrap();
}

fn spawn_ev(c: &drover::Ctx, ts: u64, issue: u64, agent: &str) {
    ev(
        c,
        ts,
        issue,
        "spawn",
        json!({"role": "impl", "agent": agent, "branch": format!("issue-{issue}")}),
    );
}

fn shed(c: &drover::Ctx, lanes: &[(&str, &str)]) -> FakeShed {
    let sh = FakeShed::new();
    sh.attach(c);
    for (name, status) in lanes {
        sh.lane(name, status, "/x");
    }
    sh
}

fn kinds(c: &drover::Ctx, kind: &str) -> usize {
    read(&c.state).unwrap().iter().filter(|e| e.kind == kind).count()
}

fn check(c: &drover::Ctx, r: &Fake) -> String {
    let (code, out) = run(c, r, &["check"]);
    assert_eq!(code, 0, "{out}");
    out
}

#[test]
fn stall_fires_once_and_again_after_state_change() {
    let d = tmpdir("stall");
    let mut c = ctx(&d, CONFIG);
    spawn_ev(&c, NOW - 3600, 12, LANE);
    let _sh = shed(&c, &[(LANE, "idle")]);
    let r = Fake::default();
    let out = check(&c, &r);
    assert!(out.contains("sendmeter #12 stall: no event for 60m in impl"), "{out}");
    assert_eq!(kinds(&c, "stall"), 1);
    assert_eq!(check(&c, &r), "", "no re-fire on the next run");
    assert_eq!(kinds(&c, "stall"), 1);
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
    let _sh = shed(&c, &[(LANE, "working")]);
    let r = Fake::default();
    assert_eq!(check(&c, &r), "");
    assert_eq!(read(&c.state).unwrap().len(), 1);
}

#[test]
fn lane_dead_fires_once_and_again_after_state_change() {
    let d = tmpdir("lane-dead");
    let c = ctx(&d, CONFIG);
    spawn_ev(&c, NOW - 60, 12, LANE);
    let _sh = shed(&c, &[]);
    let r = Fake::default();
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
    Fake::default().reply("gh pr view issue-12", &pr.to_string())
}

#[test]
fn ready_unmerged_fires_once_and_again_after_state_change() {
    let d = tmpdir("ready");
    let mut c = ctx(&d, CONFIG);
    let _sh = shed(&c, &[]);
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
    let d2 = tmpdir("ready-stale");
    let c2 = ctx(&d2, CONFIG);
    let _sh2 = shed(&c2, &[]);
    spawn_ev(&c2, NOW - 7200, 12, LANE);
    ev(
        &c2,
        NOW - 3600,
        12,
        "verdict",
        json!({"verdict": "PASS", "sha": "2".repeat(40)}),
    );
    assert_eq!(check(&c2, &r), "");
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
    let _sh = shed(&c, &[]);
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

#[test]
fn clean_removes_only_merged_clean_idle_lanes() {
    let d = tmpdir("clean");
    let c = ctx(&d, CONFIG);
    let sh = FakeShed::new();
    sh.attach(&c);
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
    let lane = |ts, issue, role: &str, w: &str| {
        let agent = format!("{role}-{issue}");
        let d = json!({"role": role, "agent": agent, "worktree": w, "pane": agent, "runtime": "shed"});
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
    lane(10, 12, "impl", &a);
    lane(20, 12, "rev", &b);
    merged(12);
    lane(10, 13, "impl", &open);
    lane(10, 14, "impl", &unpushed);
    merged(14);
    lane(10, 15, "impl", &live);
    merged(15);
    sh.lane("someone", "idle", &live);
    let git = |w: &str, cmd: &str| format!("git -C {w} {cmd}");
    let removed = |r: &Fake, w: &str| {
        r.calls
            .borrow()
            .iter()
            .any(|l| l.ends_with(&format!("worktree remove {w}")))
    };
    let r = || {
        Fake::default()
            .reply(&git(&b, "status"), "?? verdict.md\n")
            .reply(&git(&unpushed, "rev-list"), "2\n")
            .reply(&git(&a, "rev-list"), "0\n")
            .reply(&git(&b, "rev-list"), "0\n")
            .once(&git(&unpushed, "check-ignore -q dist"), 1, "")
            .reply("git -C", "")
    };
    let r0 = r();
    let (code, dry) = run(&c, &r0, &["clean"]);
    assert_eq!(code, 0, "{dry}");
    assert!(
        dry.contains(&format!("would remove {a}")) && dry.contains(&format!("would remove {b}")),
        "{dry}"
    );
    assert!(
        dry.contains(&format!(
            "would save {unpushed} to wip/ (2 unpushed commit(s)), then remove it"
        )),
        "{dry}"
    );
    assert!(dry.contains(&format!("keep {live}: live agent")), "{dry}");
    assert!(!dry.contains(&open), "unmerged lanes are not candidates: {dry}");
    assert!(
        [&a, &b, &unpushed, &live].iter().all(|w| !removed(&r0, w)) && !sh.methods().iter().any(|m| m == "kill"),
        "dry run changes nothing"
    );
    assert_eq!(kinds(&c, "clean"), 0);

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
    let freed = drover::check::du(&gone) + drover::check::du(&kept_wt);
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
    assert!(
        done.contains(&format!("freed {freed} bytes of build folders")),
        "{done}"
    );
    assert!(removed(&r1, &a) && removed(&r1, &b), "{:?}", r1.calls.borrow());
    assert!(sh.called("kill", "impl-12"), "{:?}", sh.methods());
    for w in [&open, &unpushed, &live] {
        assert!(!removed(&r1, w), "{w} must stay");
    }
    assert!(sh.has("someone"), "the live agent stays");
    let cleaned: Vec<_> = read(&c.state)
        .unwrap()
        .iter()
        .filter(|e| e.kind == "clean")
        .map(|e| e.str("worktree").unwrap().to_string())
        .collect();
    assert_eq!(cleaned.len(), 2);
    assert!(cleaned.contains(&a) && cleaned.contains(&b));
}

struct RealGit(Fake);

impl drover::runner::Runner for RealGit {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<drover::runner::Out> {
        if prog == "git" {
            return drover::runner::Real.run(prog, args);
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
    for (ts, issue, p) in [(10, 12, &ours), (11, 13, &user), (12, 14, &locked)] {
        let w = p.to_str().unwrap();
        ev(
            &c,
            ts,
            issue,
            "spawn",
            json!({"role": "impl", "agent": format!("impl-{issue}"), "worktree": w, "pane": format!("impl-{issue}"), "runtime": "shed"}),
        );
        ev(
            &c,
            50,
            issue,
            "merged",
            json!({"pr": "1", "merge_sha": "m", "into": "staging"}),
        );
    }
    let _sh = shed(&c, &[]);
    let r = || RealGit(Fake::default());
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
        dry.contains(&format!(
            "would save {user_s} to wip/ (1 uncommitted change(s)), then remove it"
        )),
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
    assert!(!user.exists(), "{done}");
    let wip = std::process::Command::new("git")
        .args([
            "-C",
            origin.to_str().unwrap(),
            "show",
            &format!("{}:notes.txt", saved_as(&done, "wip/impl-13-1000000-")),
        ])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&wip.stdout), "x");
    assert!(locked.exists(), "the failed candidate is left in place");
    assert_eq!(kinds(&c, "clean"), 2);
}

#[test]
fn clean_removes_merged_worktrees_holding_only_lane_artifacts_and_unlogged_rev_trees() {
    let d = tmpdir("clean-shots");
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
    std::fs::write(local.join(".gitignore"), "site/\n").unwrap();
    git(&local, &["add", ".gitignore"]);
    git(&local, &["commit", "-q", "-m", "init"]);
    git(&local, &["push", "-q", "origin", "HEAD"]);
    let c = ctx(
        &d,
        &CONFIG.replace("/tmp/does-not-matter/sendmeter", local.to_str().unwrap()),
    );
    let wt = |n: &str, files: &[&str]| {
        let p = c.worktrees.join("sendmeter").join(n);
        git(&local, &["worktree", "add", "-q", "-b", n, p.to_str().unwrap()]);
        for f in files {
            std::fs::create_dir_all(p.join(f).parent().unwrap()).unwrap();
            std::fs::write(p.join(f), "x").unwrap();
        }
        p
    };
    let shots = wt(
        "issue-15",
        &["ui-390/board.png", "ui-1440/board.png", "verdict.md", "site/index.html"],
    );
    let dirt = wt("issue-17", &["ui-390/board.png", "notes.txt"]);
    let rev = wt("rev-15-r1", &["verdict.md", ".mcp.lane.json"]);
    let open = wt("rev-16-r1", &["verdict.md"]);
    for (issue, p) in [(15, &shots), (17, &dirt)] {
        let d = json!({"role": "impl", "agent": format!("impl-{issue}"), "worktree": p.to_str().unwrap()});
        ev(&c, 10, issue, "spawn", d);
        ev(
            &c,
            50,
            issue,
            "merged",
            json!({"pr": "1", "merge_sha": "m", "into": "staging"}),
        );
    }
    let _sh = shed(&c, &[]);
    let r = || RealGit(Fake::default());
    let (code, dry) = run(&c, &r(), &["clean"]);
    assert_eq!(code, 0, "{dry}");
    assert!(dry.contains(&format!("would remove {} ", shots.display())), "{dry}");
    assert!(dry.contains(&format!("would remove {} ", rev.display())), "{dry}");
    assert!(
        dry.contains(&format!(
            "would save {} to wip/ (1 uncommitted change(s))",
            dirt.display()
        )),
        "{dry}"
    );
    assert!(
        !dry.contains(&open.display().to_string()),
        "an open issue's rev tree stays: {dry}"
    );

    let (code, done) = run(&c, &r(), &["clean", "--yes"]);
    assert_eq!(code, 0, "{done}");
    assert!(!shots.exists() && !rev.exists() && !dirt.exists(), "{done}");
    assert!(open.exists(), "{done}");
    let wip = std::process::Command::new("git")
        .args([
            "-C",
            origin.to_str().unwrap(),
            "show",
            &format!("{}:notes.txt", saved_as(&done, "wip/impl-17-1000000-")),
        ])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&wip.stdout),
        "x",
        "other dirt is saved first: {done}"
    );
    assert_eq!(kinds(&c, "clean"), 3);
}

#[test]
fn clean_reclaims_lanes_whose_issue_closed_outside_merge() {
    let d = tmpdir("clean-closed");
    let c = ctx(&d, CONFIG);
    let sh = shed(&c, &[]);
    let wt = |n: &str| {
        let p = c.worktrees.join("sendmeter").join(n);
        std::fs::create_dir_all(&p).unwrap();
        p.to_string_lossy().into_owned()
    };
    let (done, unpushed, open) = (wt("issue-21"), wt("issue-22"), wt("issue-23"));
    for (issue, w) in [(21, &done), (22, &unpushed), (23, &open)] {
        let agent = format!("impl-{issue}");
        let d = json!({"role": "impl", "agent": agent, "worktree": w, "pane": agent, "runtime": "shed"});
        ev(&c, 10, issue, "spawn", d);
    }
    let r = || {
        Fake::default()
            .reply(
                "gh issue list -R jirathip-dev/sendmeter --state closed",
                r#"[{"number": 21}, {"number": 22}, {"number": 99}]"#,
            )
            .reply(&format!("git -C {unpushed} rev-list"), "1\n")
            .reply(&format!("git -C {done} rev-list"), "0\n")
            .reply("git -C", "")
    };
    let removed = |r: &Fake, w: &str| {
        r.calls
            .borrow()
            .iter()
            .any(|l| l.ends_with(&format!("worktree remove {w}")))
    };
    let r0 = r();
    let (code, dry) = run(&c, &r0, &["clean"]);
    assert_eq!(code, 0, "{dry}");
    assert!(
        dry.contains(&format!(
            "would remove {done} (pane impl-21, sendmeter#21 issue closed)"
        )),
        "{dry}"
    );
    assert!(
        dry.contains(&format!(
            "would save {unpushed} to wip/ (1 unpushed commit(s)), then remove it"
        )),
        "{dry}"
    );
    assert!(!dry.contains(&open), "open issues are not candidates: {dry}");
    assert_eq!(r0.count("gh "), 1, "one closed-issue read per fleet, not per lane");
    assert!(
        !r0.calls
            .borrow()
            .iter()
            .any(|l| l.starts_with("gh ") && !l.starts_with("gh issue list")),
        "no GitHub writes"
    );

    let log = |a: &str| drover::lanes::log(&c.state, a).unwrap();
    std::fs::create_dir_all(log("impl-21").parent().unwrap()).unwrap();
    for a in ["impl-21", "impl-22"] {
        std::fs::write(log(a), "⏺ hi\n").unwrap();
    }
    let r1 = r();
    let (code, out) = run(&c, &r1, &["clean", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(&format!("removed {done} (sendmeter#21 issue closed)")),
        "{out}"
    );
    assert!(!log("impl-21").exists() && log("impl-22").exists());
    assert!(
        removed(&r1, &done) && sh.called("kill", "impl-21"),
        "{:?}",
        r1.calls.borrow()
    );
    assert!(!removed(&r1, &unpushed) && !removed(&r1, &open));
    assert!(!sh.called("kill", "impl-22") && !sh.called("kill", "impl-23"));
    assert_eq!(r1.count("gh "), 0, "the closed-issue read is cached");
    assert_eq!(kinds(&c, "clean"), 1);
}

#[test]
fn orphan_lane_fires_once_per_agent() {
    let d = tmpdir("orphan-lane");
    let c = ctx(&d, CONFIG);
    let _sh = shed(
        &c,
        &[
            ("sendmeter-rev-93-r1", "working"),
            ("other-impl-5", "idle"),
            (LANE, "working"),
        ],
    );
    spawn_ev(&c, NOW - 60, 12, LANE);
    let r = Fake::default();
    let out = check(&c, &r);
    assert!(
        out.contains("sendmeter #93 orphan-lane: agent sendmeter-rev-93-r1 has no spawn event"),
        "{out}"
    );
    assert_eq!(
        kinds(&c, "orphan-lane"),
        1,
        "the logged lane and other fleets are no orphans"
    );
    assert_eq!(check(&c, &r), "", "once");
}

#[test]
fn verdict_closes_the_reviewer_lane_unless_dirty_or_working() {
    const REV: &str = "sendmeter-rev-12-r1";
    for (status, working, kind) in [
        ("?? verdict.md\n", false, "lane-closed"),
        (" M src/x.rs\n", false, "lane-kept"),
        ("?? verdict.md\n", true, "verdict"),
    ] {
        let d = tmpdir("verdict-close");
        let c = ctx(&d, CONFIG);
        let lanes: &[(&str, &str)] = if working { &[(REV, "working")] } else { &[] };
        let sh = shed(&c, lanes);
        let w = c.worktrees.join("sendmeter/rev-12-r1");
        std::fs::create_dir_all(&w).unwrap();
        let w = w.to_string_lossy().into_owned();
        spawn_ev(&c, 10, 12, LANE);
        let rev = json!({"role": "rev", "agent": REV, "worktree": w, "pane": REV, "runtime": "shed"});
        ev(&c, 20, 12, "spawn", rev);
        let f = d.join("verdict.md");
        std::fs::write(&f, "FAIL\n1. blocking a.rs:1\nProof: spec\n> AC 1\n").unwrap();
        let r = Fake::default()
            .reply("gh api repos/jirathip-dev/sendmeter/branches/issue-12", HEAD)
            .reply(&format!("git -C {w} status"), status)
            .reply(&format!("git -C {w} rev-list"), "0\n")
            .reply("git -C", "");
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
        let e = read(&c.state).unwrap().to_vec().pop().unwrap();
        assert_eq!(e.kind, kind, "{out}");
        let gone = kind == "lane-closed";
        assert_eq!(sh.called("kill", REV), gone, "{out}");
        let removed = r
            .calls
            .borrow()
            .iter()
            .any(|l| l.ends_with(&format!("worktree remove {w}")));
        assert_eq!(removed, gone, "{out}");
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
        let st = &drover::events::fold(&read(&c.state).unwrap())[&("sendmeter".to_string(), 12)];
        assert_eq!(
            (st.state.as_str(), st.last_event.as_str()),
            ("fix(1)", "verdict"),
            "history kept"
        );
    }
}

#[test]
fn du_counts_blocks_used_not_apparent_size() {
    let d = tmpdir("du-blocks");
    std::fs::File::create(d.join("sparse"))
        .unwrap()
        .set_len(1 << 30)
        .unwrap();
    std::fs::write(d.join("data"), vec![1u8; 64 << 10]).unwrap();
    let n = drover::check::du(&d);
    assert!((64 << 10..1 << 20).contains(&n), "{n}");
}

#[test]
fn ci_missing_fires_once_per_head_after_the_grace() {
    let d = tmpdir("ci-missing");
    let mut c = ctx(&d, CONFIG);
    let _sh = shed(&c, &[]);
    spawn_ev(&c, NOW - 7200, 12, LANE);
    ev(&c, NOW - 300, 12, "verdict", json!({"verdict": "PASS", "sha": HEAD}));
    let pr =
        json!({"number": 34, "state": "OPEN", "baseRefName": "staging", "headRefOid": HEAD, "statusCheckRollup": []});
    let r = Fake::default().reply("gh pr view issue-12", &pr.to_string());
    assert_eq!(check(&c, &r), "", "inside the 10 min grace");
    c.now = NOW + 600;
    let out = check(&c, &r);
    assert!(
        out.contains("sendmeter #12 ci-missing: CI missing (no checks 10m after push)"),
        "{out}"
    );
    assert_eq!(check(&c, &r), "", "once per head");
    assert_eq!(kinds(&c, "ci-missing"), 1);
}

#[test]
fn an_unheld_idle_lane_stalls_once_until_it_changes() {
    let mut c = ctx(&tmpdir("stall-repeat"), CONFIG);
    spawn_ev(&c, NOW - 3600, 12, LANE);
    let _sh = shed(&c, &[(LANE, "idle")]);
    let r = Fake::default();
    assert!(check(&c, &r).contains("stall: no event for 60m"));
    c.now = NOW + 7200;
    assert_eq!(check(&c, &r), "", "2 h later: still the same stall");
    c.now = NOW + 24 * 3600;
    assert_eq!(check(&c, &r), "", "a day later too");
    assert_eq!(kinds(&c, "stall"), 1);
}

#[test]
fn a_blocked_live_lane_is_held_shown_held_and_never_stalls() {
    let mut c = ctx(&tmpdir("held"), CONFIG);
    spawn_ev(&c, NOW - 3600, 12, LANE);
    append(
        &c.state,
        &Event::new(
            NOW - 3600,
            "sendmeter",
            None,
            "queue",
            json!({"op": "add", "issues": [12]}),
        ),
    )
    .unwrap();
    let _sh = shed(&c, &[(LANE, "idle")]);
    let r = Fake::default();
    let (code, out) = run(&c, &r, &["queue", "sendmeter", "block", "--on", "11", "12"]);
    assert_eq!(code, 0, "{out}");
    let held: Vec<_> = read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .filter(|e| e.kind == "held")
        .collect();
    assert_eq!(held.len(), 1);
    assert_eq!(
        (held[0].issue, held[0].str("agent"), held[0].val("on")),
        (Some(12), Some(LANE), &json!([11]))
    );
    c.now = NOW + 6 * 3600;
    let (code, out) = run(&c, &r, &["status"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("held until #11 merges"), "{out}");
    let (_, out) = run(&c, &r, &["waiting"]);
    assert!(out.contains("held until #11 merges"), "{out}");
    assert_eq!(check(&c, &r), "", "a held lane is no stall");
    assert_eq!(kinds(&c, "stall"), 0);
}

fn saved_as(out: &str, prefix: &str) -> String {
    let at = out.find(prefix).unwrap_or_else(|| panic!("no {prefix} in {out}"));
    out[at..].split_whitespace().next().unwrap().to_string()
}

#[test]
fn clean_and_the_wip_snapshot_treat_spawns_claude_files_as_the_lanes_own() {
    let d = tmpdir("clean-claude-local");
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
        String::from_utf8_lossy(&o.stdout).into_owned()
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
        for f in ["CLAUDE.local.md", ".claude/settings.local.json"].iter().chain(files) {
            std::fs::create_dir_all(p.join(f).parent().unwrap()).unwrap();
            std::fs::write(p.join(f), "x").unwrap();
        }
        p
    };
    let (ours, user) = (wt("issue-12", &[]), wt("issue-13", &["notes.txt"]));
    for (ts, issue, p) in [(10, 12, &ours), (11, 13, &user)] {
        let w = p.to_str().unwrap();
        ev(
            &c,
            ts,
            issue,
            "spawn",
            json!({"role": "impl", "agent": format!("impl-{issue}"), "worktree": w, "pane": format!("impl-{issue}"), "runtime": "shed"}),
        );
        ev(
            &c,
            50,
            issue,
            "merged",
            json!({"pr": "1", "merge_sha": "m", "into": "staging"}),
        );
    }
    let _sh = shed(&c, &[]);
    let r = || RealGit(Fake::default());
    let (code, out) = run(&c, &r(), &["clean", "--lane", "impl-12", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(!ours.exists(), "{out}");
    let (_, dry) = run(&c, &r(), &["clean"]);
    let user_s = user.display().to_string();
    assert!(
        dry.contains(&format!(
            "would save {user_s} to wip/ (1 uncommitted change(s)), then remove it"
        )),
        "{dry}"
    );
    let (_, done) = run(&c, &r(), &["clean", "--yes"]);
    let wip = saved_as(&done, "wip/impl-13-1000000-");
    let files = git(&origin, &["ls-tree", "-r", "--name-only", &wip]);
    assert_eq!(files, "notes.txt\n", "{done}");
}
