//! #6 synergy cutover: serial_paths (SERIAL_BUSY, SERIAL_UNMARKED), optional caps (CAP)
//! and the conductor-only design role.
mod common;
use common::*;
use herdsman::events::{append, fold, read, Event};
use serde_json::json;

const HEAD: &str = "1111111111111111111111111111111111111111";
const WS: &str = r#"{"result":{"workspace":{"workspace_id":"w9"}}}"#;
const PANES: &str = r#"{"result":{"panes":[{"pane_id":"w9:p1","agent":null}]}}"#;

/// CONFIG plus `extra` lines in `[fleet.sendmeter]` and a design role.
fn config(extra: &str) -> String {
    CONFIG
        .replace(
            "integration = \"staging\"   # never main",
            &format!("integration = \"staging\"\n{extra}"),
        )
        .replace(
            "[limits]",
            "[fleet.sendmeter.role.design]\nharness = \"hermes\"\nprofile = \"fleet-design\"\n\n[limits]",
        )
}

const SERIAL: &str = "serial_paths = [\"supabase/migrations/**\"]";

/// A Fake on which any hermes spawn succeeds end to end, given the `herdr agent list`.
fn happy(agent_list: &str) -> Fake {
    Fake::default()
        .reply("herdr agent list", agent_list)
        .reply("sysctl -n kern.memorystatus_vm_pressure_level", "1\n")
        .reply("sysctl -n vm.loadavg", "{ 1.50 1.20 1.00 }\n")
        .reply("sysctl -n hw.ncpu", "10\n")
        .reply("vm_stat", "Swapouts: 100.\n")
        .reply("git -C", "")
        .reply("gh issue view", r#"{"title":"t","body":"b"}"#)
        .reply("herdr worktree", WS)
        .reply("herdr pane list", PANES)
        .reply("herdr pane run", "{}")
        .reply("herdr agent start", "{}")
        .reply("herdr pane", "{}")
        .reply("herdr agent prompt", "{}")
        .reply("herdr agent wait", "{}")
}

fn spawned(c: &herdsman::Ctx, n: u64, role: &str) {
    let d = json!({"role": role, "agent": format!("sendmeter-{role}-{n}"), "branch": format!("issue-{n}")});
    append(&c.state, &Event::new(10, "sendmeter", Some(n), "spawn", d)).unwrap();
}

fn log(c: &herdsman::Ctx, n: u64, kind: &str, d: serde_json::Value) {
    append(&c.state, &Event::new(20, "sendmeter", Some(n), kind, d)).unwrap();
}

fn spawn(c: &herdsman::Ctx, n: u64, role: &str) -> (i32, String) {
    run(c, &happy(NO_AGENTS), &["spawn", "sendmeter", &n.to_string(), role])
}

fn refused(c: &herdsman::Ctx, n: u64, role: &str, code: &str) {
    let before = read(&c.state).unwrap().len();
    let (exit, out) = spawn(c, n, role);
    assert_eq!(exit, 3, "{out}");
    assert!(out.contains(&format!("REFUSED {code}")), "{out}");
    assert_eq!(read(&c.state).unwrap().len(), before, "nothing logged");
}

// --- SERIAL_BUSY ---

#[test]
fn second_serial_impl_waits_until_first_merges() {
    let d = tmpdir("serial-busy");
    let c = ctx(&d, &config(SERIAL));
    assert_eq!(
        run(
            &c,
            &Fake::default(),
            &["queue", "sendmeter", "add", "1", "2", "--serial"]
        )
        .0,
        0
    );
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "3"]).0, 0);
    let (exit, out) = spawn(&c, 1, "impl");
    assert_eq!(exit, 0, "{out}");
    refused(&c, 2, "impl", "SERIAL_BUSY");
    // Non-serial issues are unaffected.
    assert_eq!(spawn(&c, 3, "impl").0, 0);
    // Still busy while #1 sits at review and at ready.
    spawned(&c, 1, "rev");
    refused(&c, 2, "impl", "SERIAL_BUSY");
    log(&c, 1, "verdict", json!({"verdict": "PASS", "sha": HEAD}));
    refused(&c, 2, "impl", "SERIAL_BUSY");
    log(&c, 1, "merged", json!({}));
    let (exit, out) = spawn(&c, 2, "impl");
    assert_eq!(exit, 0, "allowed once #1 merged: {out}");
    let (_, ls) = run(&c, &Fake::default(), &["queue", "sendmeter", "ls"]);
    assert!(ls.contains("#2     impl  serial"), "{ls}");
}

#[test]
fn serial_max_two_admits_a_second_serial_and_refuses_a_third() {
    let d = tmpdir("serial-max");
    let c = ctx(&d, &config(&format!("{SERIAL}\nserial_max = 2")));
    assert_eq!(
        run(
            &c,
            &Fake::default(),
            &["queue", "sendmeter", "add", "1", "2", "3", "--serial"]
        )
        .0,
        0
    );
    assert_eq!(spawn(&c, 1, "impl").0, 0);
    let (exit, out) = spawn(&c, 2, "impl");
    assert_eq!(exit, 0, "second serial admitted: {out}");
    let (_, board) = run(&c, &Fake::default(), &["board", "sendmeter"]);
    assert!(
        board.starts_with("sendmeter: 3 queued, 2 active, serial 2/2\n"),
        "{board}"
    );
    let (_, status) = run(&c, &Fake::default(), &["status", "sendmeter"]);
    assert!(status.contains("sendmeter: serial 2/2\n"), "{status}");
    let (exit, out) = spawn(&c, 3, "impl");
    assert_eq!(exit, 3, "{out}");
    assert!(
        out.contains("REFUSED SERIAL_BUSY")
            && out.contains("#1 is unmerged (impl)")
            && out.contains("#2 is unmerged (impl)"),
        "names both: {out}"
    );
    // A merged one frees a slot.
    log(&c, 1, "merged", json!({}));
    let (exit, out) = spawn(&c, 3, "impl");
    assert_eq!(exit, 0, "allowed once #1 merged: {out}");
}

// --- SERIAL_UNMARKED ---

fn merge_fake(files: &str) -> Fake {
    let checks = json!([{"__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "SUCCESS"}]);
    let pr = json!({"number": 34, "state": "OPEN", "baseRefName": "staging", "headRefOid": HEAD, "statusCheckRollup": checks});
    Fake::default()
        .reply("gh pr view issue-12", &pr.to_string())
        .reply("gh pr diff 34 -R jirathip-dev/sendmeter --name-only", files)
        .reply("gh pr merge 34", "")
        .reply("gh pr view 34", &json!({"mergeCommit": {"oid": "m3rg3"}}).to_string())
        .reply("gh issue", "")
        .reply("herdr agent list", NO_AGENTS)
}

fn merge(serial: bool, files: &str) -> (i32, String, Fake) {
    let d = tmpdir("serial-merge");
    let c = ctx(&d, &config(SERIAL));
    let mut q = vec!["queue", "sendmeter", "add", "12"];
    if serial {
        q.push("--serial");
    }
    run(&c, &Fake::default(), &q);
    spawned(&c, 12, "impl");
    log(&c, 12, "verdict", json!({"verdict": "PASS", "sha": HEAD}));
    let r = merge_fake(files);
    let (exit, out) = run(&c, &r, &["merge", "sendmeter", "12"]);
    (exit, out, r)
}

#[test]
fn unmarked_pr_touching_serial_path_is_refused() {
    let (exit, out, r) = merge(false, "src/a.ts\nsupabase/migrations/x.sql\n");
    assert_eq!(exit, 3, "{out}");
    assert!(
        out.contains("REFUSED SERIAL_UNMARKED") && out.contains("supabase/migrations/x.sql"),
        "{out}"
    );
    assert!(!r.called("gh pr merge"));
}

#[test]
fn marked_pr_touching_serial_path_merges() {
    let (exit, out, r) = merge(true, "supabase/migrations/x.sql\n");
    assert_eq!(exit, 0, "{out}");
    assert!(r.called("gh pr merge 34"));
    assert!(!r.called("gh pr diff"), "a marked issue needs no diff");
}

#[test]
fn pr_outside_serial_globs_merges_unmarked() {
    let (exit, out, r) = merge(false, "supabase/seed.sql\nsrc/migrations/x.sql\n");
    assert_eq!(exit, 0, "{out}");
    assert!(r.called("gh pr merge 34"));
}

#[test]
fn serial_globs() {
    use herdsman::config::glob;
    let g = |p: &str, s: &str| glob(p.as_bytes(), s.as_bytes());
    assert!(g("supabase/migrations/**", "supabase/migrations/x.sql"));
    assert!(g("supabase/migrations/**", "supabase/migrations/a/b.sql"));
    assert!(!g("supabase/migrations/**", "supabase/seed.sql"));
    assert!(g("db/*.sql", "db/x.sql") && !g("db/*.sql", "db/a/x.sql"));
    assert!(g("**/schema.prisma", "apps/web/schema.prisma"));
}

// --- CAP ---

#[test]
fn no_cap_by_default_even_with_50_lanes() {
    let d = tmpdir("cap-unset");
    let c = ctx(&d, &config(""));
    for n in 100..150 {
        spawned(&c, n, "impl");
    }
    for n in 200..250 {
        spawned(&c, n, "impl");
        log(&c, n, "spawn", json!({"role": "rev", "agent": format!("rev-{n}")}));
    }
    let (exit, out) = spawn(&c, 1, "impl");
    assert_eq!(exit, 0, "{out}");
    spawned(&c, 2, "impl");
    let (exit, out) = spawn(&c, 2, "rev");
    assert_eq!(exit, 0, "{out}");
}

#[test]
fn max_impl_refuses_the_third_impl() {
    let d = tmpdir("cap-impl");
    let c = ctx(&d, &config("max_impl = 2\nmax_rev = 1"));
    assert_eq!(spawn(&c, 1, "impl").0, 0);
    assert_eq!(spawn(&c, 2, "impl").0, 0);
    refused(&c, 3, "impl", "CAP");
    // A fix round of an open issue is not a new lane.
    assert_eq!(
        run(
            &c,
            &happy(NO_AGENTS),
            &["spawn", "sendmeter", "2", "impl", "--supersede"]
        )
        .0,
        0
    );
    // max_rev = 1: #1 under review fills it; #2's reviewer waits.
    spawned(&c, 1, "rev");
    refused(&c, 2, "rev", "CAP");
}

// --- design ---

#[test]
fn design_lane_spawns_but_never_moves_the_issue() {
    let d = tmpdir("design");
    let c = ctx(&d, &config(SERIAL));
    let r = happy(NO_AGENTS);
    let (exit, out) = run(&c, &r, &["spawn", "sendmeter", "12", "design"]);
    assert_eq!(exit, 0, "{out}");
    assert!(
        r.called("herdr agent start sendmeter-design-12"),
        "{:?}",
        r.calls.borrow()
    );
    assert!(
        r.called("herdr worktree create --cwd /tmp/does-not-matter/sendmeter --branch design-12 --base origin/staging")
    );
    let brief = std::fs::read_to_string(c.worktrees.join("sendmeter/design-12/.brief.md")).unwrap();
    assert!(
        brief.contains("# ROLE: DESIGNER") && brief.contains("owner's call"),
        "{brief}"
    );
    let st = &fold(&read(&c.state).unwrap())[&("sendmeter".to_string(), 12)];
    assert_eq!(
        (st.state.as_str(), st.branch.as_deref(), st.lane.as_deref()),
        ("queued", None, None)
    );
    // It shows in herdr like any lane: a live design agent is LANE_LIVE for a second design spawn.
    let live = agents(&[("sendmeter-design-12", "working", "/elsewhere", "w9:p1")]);
    let (exit, out) = run(&c, &happy(&live), &["spawn", "sendmeter", "12", "design"]);
    assert_eq!(exit, 3, "{out}");
    assert!(
        out.contains("REFUSED LANE_LIVE") && out.contains("sendmeter-design-12"),
        "{out}"
    );
    // The issue's impl lane is unaffected by the design lane.
    assert_eq!(spawn(&c, 12, "impl").0, 0);
}
