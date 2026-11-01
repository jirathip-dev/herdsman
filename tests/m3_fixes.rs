//! #17 6(a): a merged issue never leaves `merged`; spawn/adopt on it is refused.
//! #17 6(b): notifiers run without the lane's HERMES_* environment.
mod common;
use common::*;
use herdsman::events::{append, fold, read, Event};
use serde_json::json;
use std::process::Command;

fn merged_13(c: &herdsman::Ctx) {
    let e = |ts, kind: &str, d| append(&c.state, &Event::new(ts, "sendmeter", Some(13), kind, d)).unwrap();
    e(
        10,
        "spawn",
        json!({"role": "impl", "agent": "sendmeter-impl-13", "branch": "issue-13"}),
    );
    e(20, "verdict", json!({"verdict": "FAIL", "sha": "1".repeat(40)}));
    e(30, "verdict", json!({"verdict": "PASS", "sha": "2".repeat(40)}));
    e(
        40,
        "merged",
        json!({"pr": "15", "merge_sha": "08c9338", "into": "staging"}),
    );
}

#[test]
fn merged_issue_never_leaves_merged() {
    let d = tmpdir("sticky-merged");
    let c = ctx(&d, CONFIG);
    merged_13(&c);
    // What the #13 self-test logged: an adopt `spawn` after the merge.
    let late = json!({"role": "impl", "agent": "sendmeter-impl-13", "branch": "issue-13", "adopted": true});
    append(&c.state, &Event::new(50, "sendmeter", Some(13), "spawn", late)).unwrap();
    append(
        &c.state,
        &Event::new(60, "sendmeter", Some(13), "blocked", json!({"code": "NO_PR"})),
    )
    .unwrap();
    let st = &fold(&read(&c.state).unwrap())[&("sendmeter".to_string(), 13)];
    assert_eq!(st.state, "merged");
    let (_, out) = run(&c, &Fake::default(), &["status", "sendmeter"]);
    assert!(out.contains("merged") && !out.contains("fix(1)"), "{out}");
}

#[test]
fn spawn_and_adopt_on_merged_issue_are_refused() {
    let d = tmpdir("merged-refused");
    let c = ctx(&d, CONFIG);
    merged_13(&c);
    let before = read(&c.state).unwrap().len();
    let r = Fake::default()
        .reply("herdr", "{}")
        .reply("gh", "{}")
        .reply("git", "")
        .reply("sysctl", "1");
    for args in [
        vec!["spawn", "sendmeter", "13", "impl"],
        vec!["spawn", "sendmeter", "13", "rev", "--dry-run"],
        vec!["adopt", "sendmeter", "13", "impl", "--pane", "w9:p1"],
    ] {
        let (code, out) = run(&c, &r, &args);
        assert_eq!(code, 3, "{args:?}: {out}");
        assert!(out.contains("REFUSED MERGED"), "{args:?}: {out}");
    }
    assert!(
        !r.called("herdr agent") && !r.called("herdr worktree"),
        "{:?}",
        r.calls.borrow()
    );
    assert_eq!(read(&c.state).unwrap().len(), before, "nothing logged");
}

#[test]
fn notifier_runs_without_lane_hermes_env() {
    let d = tmpdir("notify-env");
    let sink = d.join("env.txt");
    let cfg = format!(
        "{CONFIG}\n[[notify]]\ncmd = \"env > '{}'\"\non = [\"set\"]\n",
        sink.display()
    );
    std::fs::write(d.join("fleet.toml"), cfg).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_herdsman"))
        .args(["set", "sendmeter", "impl", "--harness", "claude"])
        .env("FLEET_CONFIG", d.join("fleet.toml"))
        .env("FLEET_STATE", d.join("state/events.jsonl"))
        .env("HERMES_HOME", "/lane/.hermes/profiles/fleet-impl")
        .env("HERMES_PROFILE", "fleet-impl")
        .env("_HERMES_GATEWAY", "1")
        .env("FLEET_TEST_MARK", "kept")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let env = std::fs::read_to_string(&sink).expect("notifier ran");
    assert!(
        !env.contains("HERMES_"),
        "lane Hermes env leaked into the notifier:\n{env}"
    );
    assert!(
        env.contains("FLEET_TEST_MARK=kept") && env.contains("PATH="),
        "the rest of the env stays:\n{env}"
    );
}

/// #16: `clean` reaches a worktree whose only record is an adopted lane, and the
/// pane of an adopt logged after the merge (pre-#17 log) without a state flip.
#[test]
fn clean_reaches_adopted_lanes_of_merged_issues() {
    let d = tmpdir("clean-adopted");
    let c = ctx(&d, CONFIG);
    let wt = |n: &str| {
        let p = c.worktrees.join("sendmeter").join(n);
        std::fs::create_dir_all(&p).unwrap();
        p.to_string_lossy().into_owned()
    };
    let (w13, w14) = (wt("issue-13"), wt("issue-14"));
    let lane = |n: u64, ws: &str, w: &str| {
        json!({"role": "impl", "agent": format!("sendmeter-impl-{n}"), "branch": format!("issue-{n}"),
            "worktree": w, "pane": format!("{ws}:p1"), "workspace": ws, "adopted": true})
    };
    let e = |ts, n, kind: &str, d| append(&c.state, &Event::new(ts, "sendmeter", Some(n), kind, d)).unwrap();
    // #13: spawn, review PASS, merged, then a late adopt in a new pane.
    let mut first = lane(13, "wOld", &w13);
    first["adopted"] = json!(false);
    e(10, 13, "spawn", first);
    e(20, 13, "verdict", json!({"verdict": "PASS", "sha": "2".repeat(40)}));
    e(
        30,
        13,
        "merged",
        json!({"pr": "15", "merge_sha": "08c9338", "into": "staging"}),
    );
    e(40, 13, "spawn", lane(13, "w644", &w13));
    // #14: the adopt is the lane's only record.
    e(10, 14, "spawn", lane(14, "w7", &w14));
    e(20, 14, "verdict", json!({"verdict": "PASS", "sha": "3".repeat(40)}));
    e(
        30,
        14,
        "merged",
        json!({"pr": "16", "merge_sha": "abc", "into": "staging"}),
    );
    let states = fold(&read(&c.state).unwrap());
    for n in [13, 14] {
        assert_eq!(states[&("sendmeter".to_string(), n)].state, "merged", "#{n}");
    }
    let r = Fake::default()
        .reply("herdr agent list", NO_AGENTS)
        .reply(&format!("git -C {w13} rev-list"), "0\n")
        .reply(&format!("git -C {w14} rev-list"), "0\n")
        .reply("git -C", "")
        .reply("herdr", "{}");
    let (code, out) = run(&c, &r, &["clean", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(&format!("removed {w13}")) && out.contains(&format!("removed {w14}")),
        "{out}"
    );
    assert!(
        r.called("herdr worktree remove --workspace w644") && r.called("herdr pane close w644:p1"),
        "newest (adopted) pane wins"
    );
    assert!(r.called("herdr worktree remove --workspace w7") && r.called("herdr pane close w7:p1"));
    assert!(!r.called("--workspace wOld"));
}
