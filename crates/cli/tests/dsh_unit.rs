use anyhow::Result;
use drover::providers;
use drover::runner::{Out, Runner};
use drover::spawn::Plan;
use std::path::PathBuf;

struct Fake(std::sync::Mutex<Vec<String>>);

impl Runner for Fake {
    fn run(&self, prog: &str, args: &[String]) -> Result<Out> {
        self.run_in(prog, args, "")
    }
    fn run_in(&self, _: &str, _: &[String], input: &str) -> Result<Out> {
        use drover::shed::proto::{Hello, Request, Response};
        let mut calls = self.0.lock().unwrap();
        let mut stdout = String::new();
        for q in input.lines().filter_map(|l| serde_json::from_str::<Request>(l).ok()) {
            let id = q.id.clone().unwrap_or_default();
            if q.method != "hello" {
                calls.push(format!("{} {}", q.method, q.params));
            }
            let gets = calls.iter().filter(|c| c.starts_with("get")).count();
            let status = if gets == 1 { "idle" } else { "working" };
            let name = q.params["name"].clone();
            let v = match q.method.as_str() {
                "hello" => serde_json::json!(Hello::ours()),
                "get" => serde_json::json!({"name": name, "labels": {"harness": "dsh-tui"}, "cwd": "/w", "cmd": ["sh"],
                    "keep": true, "state": "running", "exit": null, "pid": 7, "reported": {"state": status}}),
                "read" => serde_json::json!({"text": ""}),
                _ => serde_json::json!({}),
            };
            stdout += &(serde_json::to_string(&Response::ok(id, v))? + "\n");
        }
        Ok(Out {
            code: 0,
            stdout,
            stderr: String::new(),
        })
    }
    fn sleep(&self, _: u64) {}
    fn shed(&self) -> Option<drover::shed::Shed> {
        let wire = drover::shed::Wire::Connect;
        Some(drover::shed::Shed {
            wire,
            acp: None,
            policy: "ask".into(),
        })
    }
}

#[test]
fn kick_with_non_ascii_path_does_not_panic() {
    let wt = PathBuf::from("/Users/日本語/wt");
    let s = String::new;
    let p = Plan {
        agent: "a".into(),
        role: "rev".into(),
        stage: "rev".into(),
        family: "rev",
        harness: "dsh".into(),
        profile: s(),
        model: None,
        effort: None,
        branch: s(),
        base: s(),
        worktree: wt.clone(),
        brief: wt.join(".brief.md"),
        kick: "Read /Users/日本語/wt/.brief.md and execute it fully.".into(),
        lane: s(),
        account: None,
        env: Default::default(),
        port: 41000,
        mcp: Default::default(),
        mcp_file: wt.join(".mcp.lane.json"),
        head: s(),
        title: s(),
        resting: None,
        session: None,
        lane_key: None,
        token: None,
    };
    assert!(!p.kick.is_char_boundary(20));
    let r = Fake(Default::default());
    assert!(providers::get("dsh").kick(&r, &p, "a").unwrap());
    let sent =
        r.0.lock()
            .unwrap()
            .iter()
            .any(|c| c.starts_with("send") && c.contains("Read /Users/日本語"));
    assert!(sent, "{:?}", r.0.lock().unwrap());
}
