//! The dsh harness's kick, moved out of `src/harness/dsh.rs`.
use anyhow::Result;
use herdsman::harness;
use herdsman::runner::{Out, Runner};
use herdsman::spawn::Plan;
use std::path::PathBuf;

/// Answers `pane get` idle, then working; records every herdr call.
struct Fake(std::sync::Mutex<Vec<String>>);

impl Runner for Fake {
    fn run(&self, _: &str, args: &[String]) -> Result<Out> {
        let mut calls = self.0.lock().unwrap();
        calls.push(args.join(" "));
        let gets = calls.iter().filter(|c| c.starts_with("pane get")).count();
        let status = if gets == 1 { "idle" } else { "working" };
        let stdout = if args[1] == "get" {
            format!(r#"{{"result":{{"pane":{{"agent":"dsh-tui","agent_status":"{status}"}}}}}}"#)
        } else {
            String::new()
        };
        Ok(Out { code: 0, stdout, stderr: String::new() })
    }
    fn sleep(&self, _: u64) {}
}

/// Byte 20 of this kick falls inside `語`: the typed-kick check must not panic.
#[test]
fn kick_with_non_ascii_path_does_not_panic() {
    let wt = PathBuf::from("/Users/日本語/wt");
    let s = String::new;
    let p = Plan {
        agent: "a".into(), role: "rev".into(), harness: "dsh".into(), profile: s(), model: None, effort: None,
        branch: s(), base: s(), worktree: wt.clone(), brief: wt.join(".brief.md"),
        kick: "Read /Users/日本語/wt/.brief.md and execute it fully.".into(), lane: s(),
        account: None, mcp: Default::default(), mcp_file: wt.join(".mcp.lane.json"), head: s(), title: s(),
    };
    assert!(!p.kick.is_char_boundary(20));
    let r = Fake(Default::default());
    assert!(harness::get("dsh").kick(&r, &p, "w9:p1").unwrap());
    assert!(r.0.lock().unwrap().iter().any(|c| c.starts_with("pane send-text w9:p1 Read /Users/日本語")));
}
