//! #129: auto-update after a merge, the pure-update gate, the `git merge-tree` preview, merge order, heads-up, cap-check.
//! Real temp git repos (origin, a seed clone that pushes, the fleet's `local` clone); everything else is the fake.
mod common;
use common::*;
use herdsman::config::Config;
use herdsman::events::{append, fold, read, Event};
use herdsman::runner::{Out, Real, Runner};
use serde_json::json;
use std::path::{Path, PathBuf};

/// git runs for real, the rest (gh, herdr) is scripted; every call is recorded.
struct Mixed(Fake);

impl Runner for Mixed {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        if prog == "git" {
            self.0.calls.borrow_mut().push(format!("git {}", args.join(" ")));
            return Real.run(prog, args);
        }
        self.0.run(prog, args)
    }
    fn sleep(&self, _: u64) {}
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn commit(seed: &Path, file: &str, text: &str, push: &str) -> String {
    std::fs::write(seed.join(file), text).unwrap();
    git(seed, &["add", "."]);
    git(seed, &["commit", "-qm", file]);
    git(seed, &["push", "-q", "origin", &format!("HEAD:{push}")]);
    git(seed, &["rev-parse", "HEAD"])
}

const A: &str = "1\n2\n3\n4\n5\n";

/// origin with `staging` and `issue-1` (a.txt line 1), `issue-2` (a.txt line 1 too: conflicts with 1),
/// `issue-3` (b.txt: clean); returns (dir, seed, ctx, fleet config).
fn repos(name: &str) -> (PathBuf, PathBuf, herdsman::Ctx, Config) {
    let d = tmpdir(name);
    let (origin, seed, local) = (d.join("origin.git"), d.join("seed"), d.join("local"));
    git(&d, &["init", "-q", "--bare", "-b", "staging", origin.to_str().unwrap()]);
    git(&d, &["clone", "-q", origin.to_str().unwrap(), seed.to_str().unwrap()]);
    git(&d, &["clone", "-q", origin.to_str().unwrap(), local.to_str().unwrap()]);
    for dir in [&seed, &local] {
        git(dir, &["config", "user.email", "t@example.com"]);
        git(dir, &["config", "user.name", "t"]);
    }
    git(&seed, &["checkout", "-qb", "staging"]);
    commit(&seed, "a.txt", A, "staging");
    for (n, file, text) in [
        (1, "a.txt", "one\n2\n3\n4\n5\n"),
        (2, "a.txt", "uno\n2\n3\n4\n5\n"),
        (3, "b.txt", "b\n"),
    ] {
        git(&seed, &["checkout", "-q", "-B", &format!("issue-{n}"), "staging"]);
        commit(&seed, file, text, &format!("issue-{n}"));
    }
    git(&seed, &["checkout", "-q", "staging"]);
    let config = CONFIG.replace("/tmp/does-not-matter/sendmeter", local.to_str().unwrap());
    let config = config.replace(
        "integration = \"staging\"",
        "integration = \"staging\"\ndriver = \"herdsman\"",
    );
    let c = ctx(&d, &config);
    (d, seed, c, Config::parse(&config).unwrap())
}

fn head(seed: &Path, branch: &str) -> String {
    git(seed, &["fetch", "-q", "origin"]);
    git(seed, &["rev-parse", &format!("origin/{branch}")])
}

/// An impl lane for `n`, a PASS at its pushed head when `pass`, and its open PR #(10 + n) in the PR cache.
fn lane(c: &herdsman::Ctx, seed: &Path, n: u64, pass: bool) {
    let spawn = json!({"role": "impl", "agent": format!("sendmeter-impl-{n}"), "branch": format!("issue-{n}"), "pane": format!("p{n}")});
    append(&c.state, &Event::new(10, "sendmeter", Some(n), "spawn", spawn)).unwrap();
    if pass {
        let v = json!({"verdict": "PASS", "sha": head(seed, &format!("issue-{n}"))});
        append(&c.state, &Event::new(20, "sendmeter", Some(n), "verdict", v)).unwrap();
    }
    let cache = c.state.with_file_name("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let pr = json!({"ts": 0, "branch": format!("issue-{n}"), "pr": {"number": 10 + n, "state": "OPEN"}});
    std::fs::write(cache.join(format!("pr-sendmeter-{n}.json")), pr.to_string()).unwrap();
}

fn green_pr(n: u64, head: &str) -> String {
    let ok = json!([{"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"}]);
    json!({"number": 10 + n, "state": "OPEN", "baseRefName": "staging", "headRefOid": head, "statusCheckRollup": ok})
        .to_string()
}

fn merger(n: u64, head: &str) -> Mixed {
    Mixed(
        Fake::default()
            .reply(&format!("gh pr view issue-{n} "), &green_pr(n, head))
            .reply("gh pr merge", "")
            .reply("gh pr view 1", &json!({"mergeCommit": {"oid": "m3rg3"}}).to_string())
            .reply("gh issue", "")
            .reply("herdr", NO_AGENTS),
    )
}

#[test]
fn clean_auto_update_keeps_the_pass_and_spawns_no_lane() {
    let (_d, seed, c, cfg) = repos("au-clean");
    lane(&c, &seed, 3, true);
    let from = head(&seed, "issue-3");
    commit(&seed, "c.txt", "c\n", "staging");
    let r = Mixed(Fake::default().reply("herdr", NO_AGENTS));
    let mut out = vec![];
    herdsman::drive::auto_update(&c, &cfg, &r, &mut out, "sendmeter").unwrap();
    let to = head(&seed, "issue-3");
    let ev = read(&c.state).unwrap();
    let up = ev.iter().find(|e| e.kind == "auto-updated").expect("auto-updated");
    assert_eq!((up.str("from"), up.str("to")), (Some(from.as_str()), Some(to.as_str())));
    assert!(ev.iter().all(|e| e.kind != "spawn" || e.ts == 10), "no lane: {ev:?}");
    assert!(
        r.0.calls
            .borrow()
            .iter()
            .all(|l| l.starts_with("git ") || l.starts_with("herdr agent list")),
        "{:?}",
        r.0.calls.borrow()
    );
    // The gate takes the PASS of the head it updated: merged, no review round.
    let r = merger(3, &to);
    let (code, out) = run(&c, &r, &["merge", "sendmeter", "3"]);
    assert_eq!(code, 0, "{out}");
    assert!(r.0.called(&format!(
        "gh pr merge 13 -R jirathip-dev/sendmeter --merge --match-head-commit {to}"
    )));
    let st = &fold(&read(&c.state).unwrap())[&("sendmeter".to_string(), 3)];
    assert_eq!((st.state.as_str(), st.rounds), ("merged", 1));
}

#[test]
fn gate_refuses_a_merge_head_that_adds_its_own_change() {
    let (d, seed, c, _) = repos("au-impure");
    lane(&c, &seed, 3, true);
    commit(&seed, "c.txt", "c\n", "staging");
    // A merge of integration with an extra edit folded in: not a pure update.
    let w = d.join("w");
    git(
        &d,
        &[
            "clone",
            "-q",
            "-b",
            "issue-3",
            d.join("origin.git").to_str().unwrap(),
            w.to_str().unwrap(),
        ],
    );
    git(
        &w,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "merge",
            "-q",
            "--no-commit",
            "origin/staging",
        ],
    );
    std::fs::write(w.join("b.txt"), "sneaky\n").unwrap();
    git(&w, &["add", "."]);
    git(
        &w,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-qm",
            "merge",
        ],
    );
    git(&w, &["push", "-q", "origin", "HEAD:issue-3"]);
    let h = head(&seed, "issue-3");
    let r = merger(3, &h);
    let (code, out) = run(&c, &r, &["merge", "sendmeter", "3"]);
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("REFUSED STALE_VERDICT"), "{out}");
    assert!(!r.0.called("gh pr merge"));
}

#[test]
fn conflicting_auto_update_spawns_an_update_lane() {
    let (_d, seed, c, cfg) = repos("au-conflict");
    lane(&c, &seed, 1, true);
    let before = head(&seed, "issue-1");
    // #2's change lands on staging: #1 now conflicts on a.txt.
    git(&seed, &["merge", "-q", "--ff-only", "origin/issue-2"]);
    git(&seed, &["push", "-q", "origin", "HEAD:staging"]);
    let r = Mixed(Fake::default());
    herdsman::drive::auto_update(&c, &cfg, &r, &mut vec![], "sendmeter").unwrap();
    assert_eq!(head(&seed, "issue-1"), before, "nothing pushed");
    let ev = read(&c.state).unwrap();
    assert!(ev.iter().all(|e| e.kind != "auto-updated"));
    let d = ev.iter().find(|e| e.kind == "driven").expect("driven");
    assert_eq!(
        d.str("cmd"),
        Some("herdsman spawn sendmeter 1 impl --supersede --update")
    );
}

#[test]
fn merge_tree_finds_the_pair_and_its_files_once() {
    let (_d, seed, c, cfg) = repos("mt-pair");
    for n in 1..=3 {
        lane(&c, &seed, n, false);
    }
    let r = Mixed(Fake::default());
    let got = herdsman::overlap::preview(&c, &cfg, &r, "sendmeter", &read(&c.state).unwrap()).unwrap();
    assert_eq!(got[&1], vec![(2, vec!["a.txt".to_string()])]);
    assert_eq!(got[&2], vec![(1, vec!["a.txt".to_string()])]);
    assert!(!got.contains_key(&3), "{got:?}");
    let ev = read(&c.state).unwrap();
    let p: Vec<_> = ev.iter().filter(|e| e.kind == "conflict-predicted").collect();
    assert_eq!((p.len(), p[0].issue, &p[0].data["other"]), (1, Some(2), &json!(1)));
    // Same heads: cached, no merge-tree run and no second event.
    let trees = r.0.count("git -C");
    let r2 = Mixed(Fake::default());
    let again = herdsman::overlap::preview(&c, &cfg, &r2, "sendmeter", &read(&c.state).unwrap()).unwrap();
    assert_eq!(again, got);
    assert!(trees > 0 && !r2.0.calls.borrow().iter().any(|l| l.contains("merge-tree")));
    assert_eq!(
        read(&c.state)
            .unwrap()
            .iter()
            .filter(|e| e.kind == "conflict-predicted")
            .count(),
        1
    );
    assert_eq!(herdsman::overlap::cached(&c, "sendmeter"), got);
    let (_, st) = run(&c, &r2, &["status", "sendmeter", "--json"]);
    let rows: serde_json::Value = serde_json::from_str(&st).unwrap();
    assert_eq!(rows[0]["conflicts"], json!([[2, ["a.txt"]]]));
    let (_, board) = run(&c, &r2, &["board", "sendmeter"]);
    assert!(board.contains("⚠ conflicts with #2 (a.txt)"), "{board}");
}

#[test]
fn drive_merges_the_ready_pr_that_conflicts_with_nothing_first() {
    let (_d, seed, c, _) = repos("order");
    for n in 1..=3 {
        lane(&c, &seed, n, true);
    }
    let mut f = Fake::default()
        .reply("gh pr merge", "")
        .reply("gh pr view 1", &json!({"mergeCommit": {"oid": "m3rg3"}}).to_string())
        .reply("gh issue", "")
        .reply("herdr", NO_AGENTS);
    for n in 1..=3 {
        f = f.reply(
            &format!("gh pr view issue-{n} "),
            &green_pr(n, &head(&seed, &format!("issue-{n}"))),
        );
    }
    let r = Mixed(f);
    let (code, out) = run(&c, &r, &["drive", "sendmeter"]);
    assert_eq!(code, 0, "{out}");
    let at = |n: u64| r.0.index(&format!("gh pr merge {}", 10 + n)).unwrap_or(usize::MAX);
    assert!(at(3) < at(1), "#3 first: {:?}", r.0.calls.borrow());
    assert!(at(1) < usize::MAX, "{out}");
}

#[test]
fn heads_up_reaches_a_working_lane_once() {
    let (_d, seed, c, cfg) = repos("heads-up");
    lane(&c, &seed, 1, false);
    lane(&c, &seed, 2, true);
    let r = Mixed(Fake::default().reply("herdr", ""));
    for _ in 0..2 {
        herdsman::overlap::preview(&c, &cfg, &r, "sendmeter", &read(&c.state).unwrap()).unwrap();
    }
    let want =
        "herdr agent prompt sendmeter-impl-1 HEADS-UP: your branch will conflict with #2 on a.txt (ready to merge). \
                Merge origin/staging after #2 lands.";
    assert_eq!(r.0.count(want), 1, "{:?}", r.0.calls.borrow());
    assert_eq!(
        r.0.count("herdr agent prompt sendmeter-impl-2"),
        0,
        "a ready PR's lane is done"
    );
    assert_eq!(
        read(&c.state).unwrap().iter().filter(|e| e.kind == "heads-up").count(),
        1
    );
}

#[test]
fn cap_check_reads_the_limit_from_config() {
    let d = tmpdir("cap");
    let (low, high) = (d.join("low.toml"), d.join("high.toml"));
    std::fs::write(&low, "[fleet.x]\nline_cap = 10\n").unwrap();
    std::fs::write(&high, "[fleet.y]\nline_cap = 100000\n").unwrap();
    let c = ctx(&d, CONFIG);
    let r = Fake::default();
    assert_eq!(run(&c, &r, &["cap-check", "--config", low.to_str().unwrap()]).0, 1);
    let (code, out) = run(&c, &r, &["cap-check", "--config", high.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("/ cap 100000"), "{out}");
    let ci = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml")).unwrap();
    let step = ci
        .lines()
        .skip_while(|l| !l.contains("line cap"))
        .take(2)
        .collect::<String>();
    assert!(step.contains("herdsman cap-check --config ci/cap.toml"), "{step}");
    assert!(
        !step.chars().any(|c| c.is_ascii_digit()),
        "ci.yml holds no cap number: {step}"
    );
}
