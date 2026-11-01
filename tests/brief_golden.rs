//! #34: the lane briefs (impl, impl fix round, #102 impl update round, rev, design, orch) are byte-identical to staging's.
//! Goldens in tests/golden/briefs/ were written by staging's build; `BLESS=1` rewrites them.
//! #82: the fixture fleet is renamed `api` (`acme/api`), so the goldens carry neutral names.
mod common;
use common::*;
use herdsman::events::{append, Event};
use serde_json::json;
use std::path::Path;

const DESIGN: &str = "\n[fleet.api.role.design]\nharness = \"hermes\"\nprofile = \"fleet-design\"\n";

fn fake() -> Fake {
    Fake::default()
        .reply("herdr agent list", NO_AGENTS)
        .reply(
            "gh issue view 12",
            r#"{"title":"Fix the meter","body":"AC: it {works} \\ `ok`"}"#,
        )
        .reply("git -C", "0\n")
        .reply("herdr worktree", r#"{"result":{"workspace":{"workspace_id":"w9"}}}"#)
        .reply(
            "herdr workspace create",
            r#"{"result":{"workspace":{"workspace_id":"w9"}}}"#,
        )
        .reply(
            "herdr pane list",
            r#"{"result":{"panes":[{"pane_id":"w9:p1","agent":null}]}}"#,
        )
        .reply("herdr pane run", "{}")
        .reply("herdr agent start", "{}")
        .reply("herdr pane wait-output", "{}")
        .reply("herdr agent prompt", "{}")
        .reply("herdr pane read", "Read the brief")
}

/// Spawn one lane and return its brief, with the temp dir replaced by `<TMP>`.
fn brief(name: &str, issue: &str, role: &str, fail: bool, update: bool) -> String {
    let d = tmpdir(&format!("brief-{name}"));
    let c = ctx(
        &d,
        &(CONFIG
            .replace("jirathip-dev/sendmeter", "acme/api")
            .replace("sendmeter", "api")
            + DESIGN),
    );
    if fail {
        let body = "FAIL\n\n1. **blocking**: src/x.rs:3 off by {one}.";
        let data = json!({"verdict": "FAIL", "sha": "a".repeat(40), "body": body});
        append(&c.state, &Event::new(10, "api", Some(12), "verdict", data)).unwrap();
    }
    let args = ["spawn", "api", issue, role, "--update"];
    let (code, out) = run(&c, &fake(), &args[..4 + usize::from(update)]);
    assert!(code == 0 || code == 4, "{name}: {code} {out}");
    let path = match role {
        "orch" => d.join("state/orch-api.md"),
        "rev" => c.worktrees.join("api/rev-12-r1/.brief.md"),
        "design" => c.worktrees.join("api/design-12/.brief.md"),
        _ => c.worktrees.join("api/issue-12/.brief.md"),
    };
    std::fs::read_to_string(path)
        .unwrap()
        .replace(d.to_str().unwrap(), "<TMP>")
}

#[test]
fn lane_briefs_match_staging() {
    let cases = [
        ("impl", "12", "impl", false, false),
        ("impl-fix", "12", "impl", true, false),
        ("impl-update", "12", "impl", false, true),
        ("rev", "12", "rev", false, false),
        ("design", "12", "design", false, false),
        ("orch", "-", "orch", false, false),
    ];
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/briefs");
    for (name, issue, role, fail, update) in cases {
        let got = brief(name, issue, role, fail, update);
        let file = dir.join(format!("{name}.md"));
        if std::env::var_os("BLESS").is_some() {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&file, &got).unwrap();
        }
        assert_eq!(
            got,
            std::fs::read_to_string(&file).unwrap(),
            "{name} brief differs from staging's"
        );
    }
}
