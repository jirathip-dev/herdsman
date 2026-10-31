//! The one herdr adapter (herdr ≥ 0.9.0, docs/setup.md): every `herdr` argv is built here. A `*_argv`
//! builder is for a command shown in a dry-run plan as well as run; the other functions run one operation.
use crate::runner::{self, argv, Out, Runner};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::Path;

/// Run a herdr argv built here (`*_argv`); the caller reads the exit code.
pub fn exec(r: &dyn Runner, a: &[String]) -> Result<Out> {
    r.run("herdr", a)
}

/// The full command line of a herdr argv, as a dry-run plan shows it.
pub fn line(a: Vec<String>) -> Vec<String> {
    std::iter::once("herdr".to_string()).chain(a).collect()
}

/// `exec`, failing on a non-zero exit; returns stdout.
pub fn ok(r: &dyn Runner, a: &[String]) -> Result<String> {
    runner::ok(r, "herdr", a)
}

/// `exec` as JSON, failing on a non-zero exit.
pub fn json(r: &dyn Runner, a: &[String]) -> Result<Value> {
    runner::json(r, "herdr", a)
}

/// `herdr agent list` or `workspace list` (`what`), as JSON.
pub fn list(r: &dyn Runner, what: &str) -> Result<Value> {
    json(r, &argv(&[what, "list"]))
}

/// Live herdr agents (`herdr agent list`).
pub fn agents(r: &dyn Runner) -> Result<Vec<Value>> {
    Ok(list(r, "agent")?["result"]["agents"].as_array().cloned().unwrap_or_default())
}

/// `herdr agent list`'s stdout when it ran OK (serve's watcher hashes it).
pub fn agent_list(r: &dyn Runner) -> Option<String> {
    exec(r, &argv(&["agent", "list"])).ok().filter(|o| o.code == 0).map(|o| o.stdout)
}

pub fn read_argv(pane: &str) -> Vec<String> {
    argv(&["pane", "read", pane, "--source", "visible"])
}

/// The pane's visible screen.
pub fn read(r: &dyn Runner, pane: &str) -> Result<String> {
    ok(r, &read_argv(pane))
}

/// The pane's last `lines` lines (herdr's whole `recent` window when shorter).
pub fn recent(r: &dyn Runner, pane: &str, lines: &str) -> Result<String> {
    ok(r, &argv(&["pane", "read", pane, "--source", "recent", "--lines", lines]))
}

pub fn keys_argv(pane: &str, key: &str) -> Vec<String> {
    argv(&["pane", "send-keys", pane, key])
}

/// Send one key (`Enter`, `Escape`, a menu digit) to the pane.
pub fn keys(r: &dyn Runner, pane: &str, key: &str) -> Result<String> {
    ok(r, &keys_argv(pane, key))
}

pub fn prompt_argv(agent: &str, text: &str) -> Vec<String> {
    argv(&["agent", "prompt", agent, text])
}

/// Type `text` into the agent's prompt (no Enter).
pub fn prompt(r: &dyn Runner, agent: &str, text: &str) -> Result<String> {
    ok(r, &prompt_argv(agent, text))
}

pub fn send_text_argv(pane: &str, text: &str) -> Vec<String> {
    argv(&["pane", "send-text", pane, text])
}

/// The page's reply: the text (dsh: `pane send-text`, else `agent prompt`), then Enter.
pub fn reply(r: &dyn Runner, dsh: bool, agent: &str, pane: &str, text: &str) -> Result<()> {
    // No `--`: herdr 0.9 reads the 2nd positional as literal text (even `-x`), and a `--` there would be sent as text.
    let first = if dsh { send_text_argv(pane, text) } else { prompt_argv(agent, text) };
    for a in [first, keys_argv(pane, "Enter")] {
        let o = exec(r, &a)?;
        if o.code != 0 {
            bail!("herdr {} {}: {}", a[0], a[1], o.stderr.trim());
        }
    }
    Ok(())
}

pub fn close(r: &dyn Runner, pane: &str) -> Result<String> {
    ok(r, &argv(&["pane", "close", pane]))
}

/// herdr's `{"error": {"code": "timeout"}}` answer (on stderr or stdout).
pub fn timed_out(o: &Out) -> bool {
    [&o.stderr, &o.stdout].iter().any(|s| serde_json::from_str::<Value>(s.trim()).is_ok_and(|v| v["error"]["code"] == "timeout"))
}

pub fn rename_argv(pane: &str, agent: &str) -> Vec<String> {
    argv(&["agent", "rename", pane, agent])
}

pub fn pane_get_argv(pane: &str) -> Vec<String> {
    argv(&["pane", "get", pane])
}

/// `herdr pane get`'s pane, when a harness (any detected agent) runs in it.
pub fn harness_in(r: &dyn Runner, pane: &str) -> Option<Value> {
    let v = json(r, &pane_get_argv(pane)).ok()?;
    Some(v["result"]["pane"].clone()).filter(|p| !p["agent"].is_null())
}

/// `herdr pane run <pane> <words…>`: type a line into the pane's shell.
pub fn run_argv(pane: &str, words: &[&str]) -> Vec<String> {
    argv(&[&["pane", "run", pane][..], words].concat())
}

/// `herdr agent start` for the harnesses herdr knows (`--kind`), `args` going to the harness.
pub fn start_argv(agent: &str, kind: &str, pane: &str, args: Vec<String>) -> Vec<String> {
    let mut c = argv(&["agent", "start", agent, "--kind", kind, "--pane", pane]);
    c.extend(argv(&["--timeout", "90000", "--"]));
    c.extend(args);
    c
}

/// `herdr agent wait` until the kick shows as working or blocked.
pub fn wait_working_argv(agent: &str, ms: &str) -> Vec<String> {
    argv(&["agent", "wait", agent, "--until", "working", "--until", "blocked", "--timeout", ms])
}

pub fn wait_output_argv(pane: &str, pattern: &str, ms: &str) -> Vec<String> {
    argv(&["pane", "wait-output", pane, "--match", pattern, "--source", "visible", "--timeout", ms])
}

pub fn workspace_create_argv(local: &str, label: &str) -> Vec<String> {
    argv(&["workspace", "create", "--cwd", local, "--label", label, "--no-focus"])
}

pub fn worktree_create_argv(local: &str, branch: &str, base: &str, path: &str, label: &str) -> Vec<String> {
    argv(&["worktree", "create", "--cwd", local, "--branch", branch, "--base", base, "--path", path, "--label", label, "--no-focus"])
}

pub fn worktree_open_argv(local: &str, path: &str) -> Vec<String> {
    argv(&["worktree", "open", "--cwd", local, "--path", path])
}

/// Remove the lane's worktree through herdr (by its workspace); false when herdr would not.
pub fn worktree_remove(r: &dyn Runner, ws: &str) -> Result<bool> {
    Ok(exec(r, &argv(&["worktree", "remove", "--workspace", ws]))?.code == 0)
}

/// Pick an idle shell pane in the workspace, splitting one off if all are busy.
pub fn free_pane(r: &dyn Runner, ws: &str, wt: &Path) -> Result<String> {
    let panes = json(r, &argv(&["pane", "list", "--workspace", ws]))?;
    let list = panes["result"]["panes"].as_array().cloned().unwrap_or_default();
    if let Some(p) = list.iter().find(|p| p["agent"].is_null()) {
        return Ok(p["pane_id"].as_str().unwrap_or_default().into());
    }
    let first = list.first().and_then(|p| p["pane_id"].as_str()).context("workspace has no panes")?;
    let wt = wt.to_string_lossy();
    let s = json(r, &argv(&["pane", "split", first, "--direction", "right", "--cwd", &wt, "--no-focus"]))?;
    Ok(s["result"]["pane"]["pane_id"].as_str().context("pane split: no pane_id")?.into())
}
