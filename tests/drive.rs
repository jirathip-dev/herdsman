//! #92 `herdsman drive`: each step once per pass, a second pass a no-op; paused, HOST_BUSY and the lock.
mod common;
use common::*;
use herdsman::events::{append, fold, read, Event};
use serde_json::json;
use std::path::PathBuf;

const WS: &str = r#"{"result":{"workspace":{"workspace_id":"w9"}}}"#;
const PANES: &str = r#"{"result":{"panes":[{"pane_id":"w9:p1","agent":null}]}}"#;

fn sha(p: &str) -> String {
    format!("{p}{}", "0".repeat(40 - p.len()))
}

fn setup(name: &str) -> herdsman::Ctx {
    ctx(&tmpdir(name), &CONFIG.replace("integration = \"staging\"", "integration = \"staging\"\ndriver = \"herdsman\""))
}

fn ev(c: &herdsman::Ctx, ts: u64, kind: &str, d: serde_json::Value) {
    append(&c.state, &Event::new(ts, "sendmeter", Some(12), kind, d)).unwrap();
}

/// A lane of #12 logged as spawned, in a real worktree dir.
fn lane(c: &herdsman::Ctx, ts: u64, role: &str, agent: &str, dir: &str) -> PathBuf {
    let wt = c.worktrees.join("sendmeter").join(dir);
    std::fs::create_dir_all(&wt).unwrap();
    let d = json!({"role": role, "agent": agent, "pane": format!("w9:{agent}"), "workspace": "w9", "worktree": wt,
        "branch": "issue-12"});
    ev(c, ts, "spawn", d);
    wt
}

fn pr(head: &str, ci: &str) -> String {
    let check = match ci {
        "green" => json!({"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"}),
        "red" => json!({"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "FAILURE",
            "detailsUrl": "https://github.com/jirathip-dev/sendmeter/actions/runs/777/job/1"}),
        _ => json!({"__typename": "CheckRun", "name": "ci", "status": "IN_PROGRESS"}),
    };
    json!({"number": 100, "state": "OPEN", "baseRefName": "staging", "headRefOid": head, "statusCheckRollup": [check]})
        .to_string()
}

/// A Fake on which spawns succeed and the host is calm; `agents` = the herdr agent list.
fn fake(agents: &str) -> Fake {
    Fake::default()
        .reply("herdr agent list", agents)
        .reply("sysctl -n kern.memorystatus_vm_pressure_level", "1\n")
        .reply("sysctl -n vm.loadavg", "{ 1.50 1.20 1.00 }\n")
        .reply("sysctl -n hw.ncpu", "10\n")
        .reply("vm_stat", "Pages free: 1.\nSwapouts:                     100.\n")
        .reply("gh issue view 12", r#"{"title":"t","body":"b"}"#)
        .reply("herdr worktree", WS)
        .reply("herdr pane list", PANES)
        .reply("herdr pane run", "{}")
        .reply("herdr agent start", "{}")
        .reply("herdr pane", "{}")
        .reply("herdr agent prompt", "{}")
        .reply("herdr agent wait", "{}")
}

/// `herdsman drive`: (output, `driven` steps logged by this pass).
fn drive(c: &herdsman::Ctx, r: &Fake) -> (String, Vec<String>) {
    let before = read(&c.state).unwrap().len();
    let (code, out) = run(c, r, &["drive"]);
    assert_eq!(code, 0, "{out} {:?}", r.calls.borrow());
    let new = read(&c.state).unwrap().split_off(before);
    (out, new.iter().filter(|e| e.kind == "driven").map(|e| e.str("step").unwrap().to_string()).collect())
}

/// The second pass changes nothing: no output, no event.
fn idle(c: &herdsman::Ctx, r: &Fake) {
    let before = read(&c.state).unwrap().len();
    assert_eq!(drive(c, r).0, "");
    assert_eq!(read(&c.state).unwrap().len(), before);
}

fn state(c: &herdsman::Ctx) -> String {
    fold(&read(&c.state).unwrap())[&("sendmeter".to_string(), 12)].state.clone()
}

#[test]
fn start_spawns_the_next_queued_issue() {
    let c = setup("drive-start");
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "12"]).0, 0);
    let r = fake(NO_AGENTS).reply("git -C", "");
    let (out, steps) = drive(&c, &r);
    assert_eq!(steps, ["start"], "{out}");
    assert!(out.starts_with("sendmeter #12 start: "), "{out}");
    assert!(r.called("herdr agent start sendmeter-impl-12"));
    assert_eq!(state(&c), "impl");
    let wt = c.worktrees.join("sendmeter/issue-12");
    idle(&c, &fake(&agents(&[("sendmeter-impl-12", "working", &wt.to_string_lossy(), "w9:p1")])));
}

#[test]
fn review_spawns_rev_at_the_pushed_head() {
    let c = setup("drive-review");
    let wt = lane(&c, 10, "impl", "sendmeter-impl-12", "issue-12");
    let head = sha("abc");
    let r = fake(&agents(&[("sendmeter-impl-12", "idle", &wt.to_string_lossy(), "w9:p1")]))
        .reply("gh pr view issue-12", &pr(&head, "pending"))
        .reply(&format!("git -C {} log", wt.display()), &format!("{head} 20"))
        .reply("git -C", "");
    let (out, steps) = drive(&c, &r);
    assert_eq!(steps, ["review"], "{out}");
    assert!(r.called("herdr worktree create --cwd /tmp/does-not-matter/sendmeter --branch rev-12-r1 --base origin/issue-12"));
    assert_eq!(state(&c), "review(1)");
    idle(&c, &r);
}

#[test]
fn a_written_verdict_is_recorded_at_the_head_then_pending_ci_waits() {
    let c = setup("drive-verdict");
    lane(&c, 10, "impl", "sendmeter-impl-12", "issue-12");
    let rev = lane(&c, 20, "rev", "sendmeter-rev-12-r1", "rev-12-r1");
    std::fs::write(rev.join("verdict.md"), "PASS\nall good\n").unwrap();
    let head = sha("abc");
    let r = fake(NO_AGENTS)
        .reply("gh pr view issue-12", &pr(&head, "pending"))
        .reply("gh api repos/jirathip-dev/sendmeter/branches/issue-12", &format!("{head}\n"))
        .reply("git -C", "");
    let (out, steps) = drive(&c, &r);
    assert_eq!(steps, ["verdict"], "{out}");
    let all = read(&c.state).unwrap();
    let v = all.iter().find(|e| e.kind == "verdict").unwrap();
    assert_eq!((v.str("verdict"), v.str("sha")), (Some("PASS"), Some(head.as_str())));
    // PASS with CI pending: drive waits.
    idle(&c, &r);
    assert!(!r.called("gh pr merge"));
}

#[test]
fn pass_and_green_ci_merges() {
    let c = setup("drive-merge");
    lane(&c, 10, "impl", "sendmeter-impl-12", "gone-12");
    std::fs::remove_dir_all(c.worktrees.join("sendmeter/gone-12")).unwrap();
    let head = sha("abc");
    ev(&c, 30, "verdict", json!({"verdict": "PASS", "sha": head, "body": "PASS"}));
    let r = fake(NO_AGENTS)
        .reply("gh pr view issue-12", &pr(&head, "green"))
        .reply("gh pr merge 100", "")
        .reply("gh pr view 100", r#"{"mergeCommit":{"oid":"m3rg3"}}"#)
        .reply("gh issue", "");
    let (out, steps) = drive(&c, &r);
    assert_eq!(steps, ["merge"], "{out}");
    assert!(r.called(&format!("gh pr merge 100 -R jirathip-dev/sendmeter --merge --match-head-commit {head}")));
    assert_eq!(state(&c), "merged");
    idle(&c, &r);
}

#[test]
fn a_fail_starts_a_fix_round_with_the_findings_verbatim() {
    let c = setup("drive-fix");
    let wt = lane(&c, 10, "impl", "sendmeter-impl-12", "issue-12");
    let body = "FAIL\n1. blocking: src/x.rs:3 off by one";
    ev(&c, 30, "verdict", json!({"verdict": "FAIL", "sha": sha("abc"), "body": body}));
    let r = fake(NO_AGENTS).reply("gh pr view issue-12", &pr(&sha("abc"), "green")).reply("git -C", "");
    let (out, steps) = drive(&c, &r);
    assert_eq!(steps, ["fix"], "{out}");
    let brief = std::fs::read_to_string(wt.join(".brief.md")).unwrap();
    assert!(brief.contains(body), "{brief}");
    assert!(brief.contains("If a finding is wrong or out of scope, say so in your report with file:line proof"));
    assert_eq!(state(&c), "fix(1)");
    idle(&c, &fake(&agents(&[("sendmeter-impl-12", "working", &wt.to_string_lossy(), "w9:p1")])));
}

#[test]
fn the_third_fail_stops_at_needs_human_with_the_findings() {
    let c = setup("drive-limit");
    lane(&c, 10, "impl", "sendmeter-impl-12", "issue-12");
    ev(&c, 11, "verdict", json!({"verdict": "FAIL", "sha": sha("1"), "body": "FAIL"}));
    ev(&c, 12, "verdict", json!({"verdict": "FAIL", "sha": sha("2"), "body": "FAIL"}));
    let rev = lane(&c, 20, "rev", "sendmeter-rev-12-r3", "rev-12-r3");
    std::fs::write(rev.join("verdict.md"), "FAIL\nsrc/y.rs:9 still wrong\n").unwrap();
    let head = sha("3");
    let r = fake(NO_AGENTS)
        .reply("gh pr view issue-12", &pr(&head, "green"))
        .reply("gh api repos/jirathip-dev/sendmeter/branches/issue-12", &format!("{head}\n"))
        .reply("git -C", "");
    let (out, steps) = drive(&c, &r);
    assert_eq!(steps, ["verdict"], "{out}");
    assert_eq!(state(&c), "needs-human");
    let all = read(&c.state).unwrap();
    let nh = all.iter().find(|e| e.kind == "needs-human").unwrap();
    assert!(nh.str("findings").unwrap().contains("src/y.rs:9 still wrong"));
    idle(&c, &r);
    assert!(!r.called("herdr agent start"), "no fix lane after the limit");
}

#[test]
fn red_ci_after_pass_is_a_fail_with_the_log_tail() {
    let c = setup("drive-red");
    lane(&c, 10, "impl", "sendmeter-impl-12", "issue-12");
    let head = sha("abc");
    ev(&c, 30, "verdict", json!({"verdict": "PASS", "sha": head, "body": "PASS"}));
    let r = fake(NO_AGENTS)
        .reply("gh pr view issue-12", &pr(&head, "red"))
        .reply("gh run view 777 -R jirathip-dev/sendmeter --log-failed", "test x ... FAILED\nerror: 1 test failed\n")
        .reply("gh api repos/jirathip-dev/sendmeter/branches/issue-12", &format!("{head}\n"))
        .reply("git -C", "");
    let (out, steps) = drive(&c, &r);
    assert_eq!(steps, ["ci-red"], "{out}");
    let all = read(&c.state).unwrap();
    let v = all.iter().rev().find(|e| e.kind == "verdict").unwrap();
    assert_eq!(v.str("verdict"), Some("FAIL"));
    assert!(v.str("body").unwrap().contains("error: 1 test failed"), "{:?}", v.str("body"));
    let st = &fold(&all)[&("sendmeter".to_string(), 12)];
    assert_eq!((st.state.as_str(), st.rounds), ("fix(1)", 1), "same head: no new round");
}

#[test]
fn clean_reclaims_merged_lanes() {
    let c = setup("drive-clean");
    let wt = lane(&c, 10, "impl", "sendmeter-impl-12", "issue-12");
    ev(&c, 30, "merged", json!({"pr": "100"}));
    let r = fake(NO_AGENTS).reply(&format!("git -C {} rev-list", wt.display()), "0\n").reply("git -C", "");
    let (out, steps) = drive(&c, &r);
    assert_eq!(steps, ["clean"], "{out}");
    assert!(r.called("herdr worktree remove --workspace w9"));
    std::fs::remove_dir_all(&wt).unwrap(); // herdr removed it
    idle(&c, &r);
}

#[test]
fn paused_fleets_orch_driven_fleets_and_host_busy_are_no_ops() {
    let c = setup("drive-paused");
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "12"]).0, 0);
    assert_eq!(run(&c, &Fake::default(), &["pause", "sendmeter"]).0, 0);
    let r = fake(NO_AGENTS).reply("git -C", "");
    idle(&c, &r);
    assert!(!r.called("herdr"), "{:?}", r.calls.borrow());
    let orch = ctx(&tmpdir("drive-orch"), CONFIG);
    assert_eq!(run(&orch, &Fake::default(), &["queue", "sendmeter", "add", "12"]).0, 0);
    idle(&orch, &r);
    assert_eq!(run(&orch, &r, &["drive", "sendmeter"]), (0, String::new()), "an orch fleet is not driven");

    let c = setup("drive-busy");
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "12"]).0, 0);
    let busy = Fake::default().once("sysctl -n kern.memorystatus_vm_pressure_level", 0, "4\n");
    let busy = Fake { replies: fake(NO_AGENTS).reply("git -C", "").replies, ..busy };
    let (out, steps) = drive(&c, &busy);
    assert_eq!(steps, ["start"]);
    assert!(out.contains("REFUSED HOST_BUSY"), "{out}");
    assert!(!busy.called("herdr worktree"));
    // Retried next pass, logged once per hour.
    let busy = Fake { replies: fake(NO_AGENTS).reply("git -C", "").replies, ..Default::default() }
        .once("sysctl -n kern.memorystatus_vm_pressure_level", 0, "4\n");
    let (out, steps) = drive(&c, &busy);
    assert!(out.contains("REFUSED HOST_BUSY") && steps.is_empty(), "{out}");
    assert_eq!(state(&c), "queued");
}

#[test]
fn the_lock_keeps_passes_apart_and_a_stale_one_is_taken_over() {
    let c = setup("drive-lock");
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "12"]).0, 0);
    let lock = c.state.parent().unwrap().join("drive.lock");
    std::fs::write(&lock, "").unwrap();
    let r = fake(NO_AGENTS).reply("git -C", "");
    let (out, steps) = drive(&c, &r);
    assert!(out.contains("another pass holds") && steps.is_empty(), "{out}");
    assert!(!r.called("herdr"));
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    std::fs::File::options().write(true).open(&lock).unwrap().set_modified(old).unwrap();
    assert_eq!(drive(&c, &r).1, ["start"]);
    assert!(!lock.exists(), "released after the pass");
    assert_eq!(herdsman::drive::last_pass(&c, "sendmeter"), Some(c.now));
}

#[test]
fn drive_now_on_the_board_is_paired_only_and_toasts_its_line() {
    let c = setup("drive-web");
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "12"]).0, 0);
    let r = fake(NO_AGENTS).reply("git -C", "");
    let s = herdsman::serve::Server::new(&c, &r, Some("127.0.0.1:7777".into()));
    let get = |s: &herdsman::serve::Server| {
        let res = s.respond("GET / HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n\r\n", c.now);
        String::from_utf8_lossy(&res).into_owned()
    };
    let page = get(&s);
    assert!(page.contains("<button>Drive now</button>") && page.contains("driver: herdsman · no pass yet"));
    let i = page.find("name=mtime value=").unwrap() + 17;
    let mtime: String = page[i..].chars().take_while(char::is_ascii_digit).collect();
    let body = format!("op=drive&fleet=sendmeter&mtime={mtime}");
    let post = |host: &str, extra: &str| {
        let req = format!("POST / HTTP/1.1\r\nHost: {host}\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len());
        String::from_utf8_lossy(&s.respond(&req, c.now)).into_owned()
    };
    assert!(post("127.0.0.1:7777", "").starts_with("HTTP/1.1 401"));
    let cookie = pair(&s, &c, "", c.now);
    assert!(post("evil.example", &cookie).starts_with("HTTP/1.1 403"));
    assert!(!r.called("herdr agent start"));
    let res = post("127.0.0.1:7777", &cookie);
    assert!(res.starts_with("HTTP/1.1 303") && res.contains("sendmeter%20%2312%20start"), "{res}");
    assert!(r.called("herdr agent start sendmeter-impl-12"));
    assert!(get(&s).contains("driver: herdsman · last pass 0s ago"));
}
