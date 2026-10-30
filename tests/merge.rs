//! AC3: every `herdsman merge` refusal. Each test also asserts `gh pr merge` was
//! never called, so an always-merge gate turns every one of them RED.
mod common;
use common::*;
use herdsman::events::{append, fold, read, Event};
use serde_json::json;

const HEAD: &str = "1111111111111111111111111111111111111111";
const OLD: &str = "2222222222222222222222222222222222222222";

fn setup(name: &str, config: &str, verdicts: &[(&str, &str)]) -> (std::path::PathBuf, herdsman::Ctx) {
    let d = tmpdir(name);
    let c = ctx(&d, config);
    let spawn = json!({"role": "impl", "agent": "sendmeter-impl-12", "branch": "issue-12"});
    append(&c.state, &Event::new(10, "sendmeter", Some(12), "spawn", spawn)).unwrap();
    for (i, (v, sha)) in verdicts.iter().enumerate() {
        let e = Event::new(20 + i as u64, "sendmeter", Some(12), "verdict", json!({"verdict": v, "sha": sha}));
        append(&c.state, &e).unwrap();
    }
    (d, c)
}

fn green() -> serde_json::Value {
    json!([{"__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "SUCCESS"},
           {"__typename": "StatusContext", "context": "ci/legacy", "state": "SUCCESS"}])
}

fn pr(base: &str, checks: serde_json::Value) -> Fake {
    let pr = json!({"number": 34, "state": "OPEN", "baseRefName": base, "headRefOid": HEAD, "statusCheckRollup": checks});
    Fake::default()
        .reply("gh pr view issue-12", &pr.to_string())
        .reply("gh pr merge 34", "")
        .reply("gh pr view 34", &json!({"mergeCommit": {"oid": "m3rg3"}}).to_string())
        .reply("gh issue", "")
}

fn assert_refused(c: &herdsman::Ctx, r: &Fake, code: &str) {
    let (exit, out) = run(c, r, &["merge", "sendmeter", "12"]);
    assert_eq!(exit, 3, "{out}");
    assert!(out.contains(&format!("REFUSED {code}")), "{out}");
    assert!(!r.called("gh pr merge"), "must not merge: {:?}", r.calls.borrow());
    let st = &fold(&read(&c.state).unwrap())[&("sendmeter".to_string(), 12)];
    assert!(st.state == "blocked" || st.state == "needs-human", "{}", st.state);
    assert!(read(&c.state).unwrap().iter().all(|e| e.kind != "merged"));
}

#[test]
fn refuses_without_verdict() {
    let (_d, c) = setup("no-verdict", CONFIG, &[]);
    assert_refused(&c, &pr("staging", green()), "NO_VERDICT");
}

#[test]
fn refuses_verdict_for_older_sha() {
    let (_d, c) = setup("old-sha", CONFIG, &[("PASS", OLD)]);
    assert_refused(&c, &pr("staging", green()), "STALE_VERDICT");
}

#[test]
fn refuses_fail_verdict() {
    let (_d, c) = setup("fail", CONFIG, &[("PASS", OLD), ("FAIL", HEAD)]);
    assert_refused(&c, &pr("staging", green()), "VERDICT_FAIL");
}

#[test]
fn refuses_when_ci_not_green() {
    let red = json!([{"__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "FAILURE"}]);
    let pending = json!([{"__typename": "CheckRun", "name": "test", "status": "IN_PROGRESS", "conclusion": ""}]);
    // A fresh log each time: the PR read is cached for 60 s per issue.
    for checks in [red, pending, json!([])] {
        let (_d, c) = setup("ci-red", CONFIG, &[("PASS", HEAD)]);
        assert_refused(&c, &pr("staging", checks), "CI_NOT_GREEN");
    }
}

#[test]
fn required_checks_gate_only_the_latest_named_run() {
    let cfg = CONFIG.replace("integration = \"staging\"   # never main", "integration = \"staging\"\nrequired_checks = [\"quality\"]");
    let run = |n: &str, c: &str, t: &str| json!({"__typename": "CheckRun", "name": n, "status": "COMPLETED", "conclusion": c, "startedAt": t});
    let red = json!([run("quality", "SUCCESS", "T1"), run("quality", "FAILURE", "T2"), run("quality-narrowed", "SUCCESS", "T1")]);
    let (_d, c) = setup("req-red", &cfg, &[("PASS", HEAD)]);
    let (exit, out) = run_cli(&c, &pr("staging", red));
    assert_eq!(exit, 3, "{out}");
    assert!(out.contains("REFUSED CI_NOT_GREEN: required check `quality` is failed"), "{out}");
    let green = json!([run("quality", "CANCELLED", "T1"), run("quality", "SUCCESS", "T2"), run("quality-narrowed", "FAILURE", "T2")]);
    let (_d, c) = setup("req-green", &cfg, &[("PASS", HEAD)]);
    let (exit, out) = run_cli(&c, &pr("staging", green));
    assert_eq!(exit, 0, "{out}");
}

fn run_cli(c: &herdsman::Ctx, r: &Fake) -> (i32, String) {
    run(c, r, &["merge", "sendmeter", "12"])
}

#[test]
fn refuses_when_round_limit_exceeded() {
    let (a, b) = ("3333333333333333333333333333333333333333", "4444444444444444444444444444444444444444");
    let v = [("FAIL", OLD), ("FAIL", a), ("FAIL", b), ("PASS", HEAD)];
    let (_d, c) = setup("rounds", CONFIG, &v);
    assert_refused(&c, &pr("staging", green()), "ROUND_LIMIT");
    let st = &fold(&read(&c.state).unwrap())[&("sendmeter".to_string(), 12)];
    assert_eq!(st.state, "needs-human");
}

#[test]
fn reverdict_on_same_sha_is_one_round() {
    let v = [("FAIL", OLD), ("FAIL", OLD), ("FAIL", OLD), ("PASS", HEAD), ("PASS", HEAD)];
    let (_d, c) = setup("same-sha", CONFIG, &v);
    let st = &fold(&read(&c.state).unwrap())[&("sendmeter".to_string(), 12)];
    assert_eq!((st.state.as_str(), st.rounds), ("ready", 2));
    let (exit, out) = run(&c, &pr("staging", green()), &["merge", "sendmeter", "12"]);
    assert_eq!(exit, 0, "{out}");
}

#[test]
fn refuses_main_as_integration_target() {
    let cfg = CONFIG.replace("integration = \"staging\"", "integration = \"main\"");
    let (_d, c) = setup("main-cfg", &cfg, &[("PASS", HEAD)]);
    assert_refused(&c, &pr("main", green()), "MAIN_TARGET");
}

#[test]
fn refuses_pr_based_on_main() {
    let (_d, c) = setup("main-base", CONFIG, &[("PASS", HEAD)]);
    assert_refused(&c, &pr("main", green()), "MAIN_TARGET");
}

#[test]
fn refuses_without_open_pr() {
    let (_d, c) = setup("no-pr", CONFIG, &[("PASS", HEAD)]);
    assert_refused(&c, &Fake::default(), "NO_PR");
}

#[test]
fn merges_with_exact_head_pass_and_green_ci() {
    let (_d, c) = setup("happy", CONFIG, &[("FAIL", OLD), ("PASS", HEAD)]);
    let r = pr("staging", green());
    let (exit, out) = run(&c, &r, &["merge", "sendmeter", "12"]);
    assert_eq!(exit, 0, "{out}");
    assert!(r.called(&format!("gh pr merge 34 -R jirathip-dev/sendmeter --merge --match-head-commit {HEAD}")));
    assert!(r.called("gh issue comment 12 -R jirathip-dev/sendmeter --body Merged via `herdsman merge`"));
    assert!(out.contains("m3rg3") && out.contains(HEAD) && out.contains("Merged"), "{out}");
    let body = r.calls.borrow().iter().find(|c| c.starts_with("gh issue comment")).unwrap().clone();
    assert!(body.lines().count() <= 5, "closing comment is ≤ 5 lines: {body}");
    let st = &fold(&read(&c.state).unwrap())[&("sendmeter".to_string(), 12)];
    assert_eq!(st.state, "merged");
}

// --- herdsman verdict ---

#[test]
fn verdict_pins_exact_branch_head() {
    let (d, c) = setup("verdict", CONFIG, &[]);
    let f = d.join("verdict.md");
    std::fs::write(&f, "PASS\nlooks right\n").unwrap();
    let r = Fake::default().reply("gh api repos/jirathip-dev/sendmeter/branches/issue-12", &format!("{HEAD}\n"));
    let fp = f.to_str().unwrap();
    let (exit, out) = run(&c, &r, &["verdict", "sendmeter", "12", "PASS", "--sha", OLD, "--file", fp]);
    assert_eq!(exit, 3, "{out}");
    assert!(out.contains("REFUSED STALE_SHA"), "{out}");
    let (exit, out) = run(&c, &r, &["verdict", "sendmeter", "12", "PASS", "--sha", "abc", "--file", fp]);
    assert_eq!(exit, 3);
    assert!(out.contains("REFUSED BAD_SHA"), "{out}");
    assert!(read(&c.state).unwrap().iter().all(|e| e.kind != "verdict"));
    let (exit, out) = run(&c, &r, &["verdict", "sendmeter", "12", "PASS", "--sha", HEAD, "--file", fp]);
    assert_eq!(exit, 0, "{out}");
    let st = &fold(&read(&c.state).unwrap())[&("sendmeter".to_string(), 12)];
    assert_eq!((st.state.as_str(), st.rounds), ("ready", 1));
    assert_eq!(st.verdicts, vec![(HEAD.to_string(), "PASS".to_string())]);
}

// --- #6: the evidence comment must land ---

fn merge_fake() -> Fake {
    let pr = json!({"number": 34, "state": "OPEN", "baseRefName": "staging", "headRefOid": HEAD, "statusCheckRollup": green()});
    Fake::default()
        .reply("gh pr view issue-12", &pr.to_string())
        .reply("gh pr merge 34", "")
        .reply("gh pr view 34", &json!({"mergeCommit": {"oid": "m3rg3"}}).to_string())
        .reply("gh issue close", "")
}

#[test]
fn evidence_comment_failing_twice_leaves_issue_open_and_exits_nonzero() {
    let d = tmpdir("evidence-sink");
    let sink = d.join("sink.txt");
    let cfg = format!("{CONFIG}\n[[notify]]\ncmd = \"cat > '{}'\"\non = [\"evidence-missing\"]\n", sink.display());
    let (_d, c) = setup("evidence-missing", &cfg, &[("PASS", HEAD)]);
    let r = merge_fake(); // `gh issue comment` is unscripted → fails
    let (exit, _) = run(&c, &r, &["merge", "sendmeter", "12"]);
    assert_ne!(exit, 0);
    assert!(r.called("gh pr merge 34"), "the merge itself happened");
    assert_eq!(r.count("gh issue comment 12"), 2, "retried once");
    assert!(!r.called("gh issue close"), "issue must stay open");
    let ev = read(&c.state).unwrap();
    assert!(ev.iter().any(|e| e.kind == "merged"));
    let miss = ev.iter().find(|e| e.kind == "evidence-missing").expect("evidence-missing logged");
    assert!(miss.str("reason").unwrap().contains("m3rg3"), "{:?}", miss.data);
    let line = std::fs::read_to_string(&sink).expect("sent to notifiers");
    assert!(line.starts_with("sendmeter #12 evidence-missing: PR #34 merged @ m3rg3"), "{line}");
}

#[test]
fn evidence_comment_retry_that_lands_closes_issue() {
    let (_d, c) = setup("evidence-retry", CONFIG, &[("PASS", HEAD)]);
    let r = merge_fake().once("gh issue comment", 1, "").reply("gh issue comment", "");
    let (exit, out) = run(&c, &r, &["merge", "sendmeter", "12"]);
    assert_eq!(exit, 0, "{out}");
    assert_eq!(r.count("gh issue comment 12"), 2);
    assert!(r.called("gh issue close 12"));
    assert!(read(&c.state).unwrap().iter().all(|e| e.kind != "evidence-missing"));
}

// --- #19: lane teardown + dogfood reinstall line ---

#[test]
fn merge_tears_down_clean_lanes_and_keeps_unpushed_ones() {
    let (_d, c) = setup("teardown", CONFIG, &[("PASS", HEAD)]);
    let wt = |n: &str| {
        let p = c.worktrees.join("sendmeter").join(n);
        std::fs::create_dir_all(&p).unwrap();
        p.to_string_lossy().into_owned()
    };
    let (imp, rev) = (wt("issue-12"), wt("rev-12-r1"));
    for (ts, role, w, ws) in [(11, "impl", &imp, "wA"), (12, "rev", &rev, "wB")] {
        let d = json!({"role": role, "agent": format!("{role}-12"), "worktree": w, "pane": format!("{ws}:p1"), "workspace": ws});
        append(&c.state, &Event::new(ts, "sendmeter", Some(12), "spawn", d)).unwrap();
    }
    // After the clean impl pane is closed only the rev agent (unpushed work) is left.
    let r = pr("staging", green())
        .reply("herdr agent list", &agents(&[("rev-12", "idle", &rev, "wB:p1")]))
        .reply(&format!("git -C {rev} rev-list"), "1\n")
        .reply(&format!("git -C {imp} rev-list"), "0\n")
        .reply("git -C", "")
        .reply("herdr", "{}");
    let (exit, out) = run(&c, &r, &["merge", "sendmeter", "12"]);
    assert_eq!(exit, 0, "{out}");
    assert!(r.called("herdr pane close wA:p1"), "{:?}", r.calls.borrow());
    assert!(r.called("herdr worktree remove --workspace wA"));
    assert!(out.contains(&format!("removed {imp}")), "{out}");
    assert!(!r.called("herdr pane close wB:p1"), "a lane with unpushed commits keeps its pane");
    assert!(!r.called("herdr worktree remove --workspace wB"));
    assert!(out.contains(&format!("keep pane wB:p1 ({rev}): 1 unpushed commit(s)")), "{out}");
    assert!(!out.contains("cargo install"), "only the dogfood fleet prints the reinstall line");
}

#[test]
fn merge_into_fleet_repo_prints_reinstall_command_last() {
    let cfg = CONFIG.replace("jirathip-dev/sendmeter", "jirathip-dev/herdsman");
    let (_d, c) = setup("dogfood", &cfg, &[("PASS", HEAD)]);
    let r = merge_fake().reply("gh issue comment", "");
    let r = Fake { replies: r.replies.into_iter().map(|(p, o)| (p.replace("sendmeter", "herdsman"), o)).collect(), ..Fake::default() };
    let (exit, out) = run(&c, &r, &["merge", "sendmeter", "12"]);
    assert_eq!(exit, 0, "{out}");
    assert_eq!(
        out.lines().last(),
        Some("cargo install --path /tmp/does-not-matter/sendmeter --locked --root /opt/herdsman --force"),
        "{out}"
    );
    assert!(!r.called("cargo"), "printed, never run");
}
