//! Harness adapters (profile + hook install, launch argv, kick): a new harness is one file plus its `NAMES` entry.
mod claude;
mod dsh;
mod hermes;

use crate::runner::{argv, Runner};
use crate::spawn::Plan;
use crate::Ctx;
use anyhow::Result;
use std::io::Write;

pub use dsh::{install_pre_push, DSH_TUI_VERSION};
pub use hermes::set_mcp;

pub const NAMES: [&str; 3] = ["claude", "hermes", "dsh"];

/// Effort levels a role may set (`effort = "…"`).
pub const EFFORTS: [&str; 5] = ["none", "low", "medium", "high", "xhigh"];

/// The levels `harness` takes: Claude `--effort` (`none` omits it), Hermes `--reasoning`; dsh none yet.
pub fn efforts(harness: &str) -> &'static [&'static str] {
    match harness {
        "dsh" => &["none"],
        _ => &EFFORTS,
    }
}

pub trait Harness {
    /// Write this harness's files for role `role`'s `profile` (prompts, hooks).
    fn sync(&self, ctx: &Ctx, role: &str, profile: &str, usage: &str, out: &mut dyn Write) -> Result<()>;
    /// Fail unless the profile and install are ready to spawn plan `p`.
    fn ready(&self, _ctx: &Ctx, _r: &dyn Runner, _p: &Plan) -> Result<()> {
        Ok(())
    }
    /// Lane setup in the worktree before the harness starts.
    fn prepare(&self, _ctx: &Ctx, _r: &dyn Runner, _p: &Plan) -> Result<()> {
        Ok(())
    }
    /// herdr argv that launches the harness in `pane`.
    fn start_cmd(&self, ctx: &Ctx, p: &Plan, pane: &str) -> Vec<String>;
    /// herdr commands that settle the harness, deliver the kick and confirm it landed.
    fn kick_cmds(&self, p: &Plan, pane: &str) -> Vec<Vec<String>>;
    /// Run `kick_cmds`; true once the kick is confirmed landed.
    fn kick(&self, r: &dyn Runner, p: &Plan, pane: &str) -> Result<bool>;
}

pub fn get(name: &str) -> &'static dyn Harness {
    match name {
        "hermes" => &hermes::Hermes,
        "dsh" => &dsh::Dsh,
        _ => &claude::Claude,
    }
}

/// The herdsman hook's command line, as Claude settings and Hermes config/allowlist name it.
pub fn hook_command(ctx: &Ctx, role: &str) -> String {
    format!("'{}' hook --role {role}", ctx.exe)
}

/// `herdr agent start` for the harnesses herdr knows (`--kind`).
fn agent_start(p: &Plan, kind: &str, pane: &str, args: Vec<String>) -> Vec<String> {
    let mut c = argv(&["agent", "start", &p.agent, "--kind", kind, "--pane", pane]);
    c.extend(argv(&["--timeout", "90000", "--"]));
    c.extend(args);
    c
}

/// `herdr agent wait` until the kick shows as working or blocked.
fn wait_working(p: &Plan, ms: &str) -> Vec<String> {
    argv(&["agent", "wait", &p.agent, "--until", "working", "--until", "blocked", "--timeout", ms])
}
