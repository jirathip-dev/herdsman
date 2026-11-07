//! Harness adapters (profile + hook install, launch argv, kick, screen readers, transcript, logins): a new harness is
//! one file plus its `NAMES` entry; core never names one, and any other name gets the generic fallbacks (#316).
mod claude;
mod claude_meter;
mod dsh;
mod hermes;

use crate::herdr;
use crate::runner::Runner;
use crate::spawn::Plan;
use crate::Ctx;
use anyhow::Result;
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};

pub use claude::{remove_tmp, root as profiles_root};
pub use claude_meter::{keychain_service, parse_usage};
pub use dsh::{install_pre_push, DSH_TUI_VERSION};
pub use hermes::set_mcp;

pub const NAMES: [&str; 3] = ["claude", "hermes", "dsh"];

/// Effort levels a role may set (`effort = "…"`).
pub const EFFORTS: &[&str] = &["none", "low", "medium", "high", "xhigh"];

/// The levels `harness` takes: Claude `--effort` (`none` omits it), Hermes `--reasoning`; dsh none yet.
pub fn efforts(harness: &str) -> &'static [&'static str] {
    match harness {
        "dsh" => &["none"],
        _ => EFFORTS,
    }
}

/// How a harness's lanes sign in: unknown, an `[accounts]` token (`access::pick`) exported as this env var, or the
/// harness's own login on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Credential {
    None,
    Token(&'static str),
    Own,
}

/// A harness's hook protocol, as `herdsman hook` reads and answers it: whether it `sent` a call, the keys of a call's
/// tool `input` (the first object wins), its `shell` and file-writing tools, its `hook_event_name` after a tool call
/// and at a turn's end, and its `deny` answer.
pub struct Dialect {
    pub sent: fn(&Value) -> bool,
    pub input: &'static [&'static str],
    pub shell: &'static str,
    pub edits: &'static [&'static str],
    /// #508: its `hook_event_name` before a tool call.
    pub pre: &'static str,
    pub post: &'static str,
    pub stop: &'static str,
    /// #298: its `hook_event_name` while it waits on a person (none: "").
    pub blocked: &'static str,
    pub deny: fn(&str) -> Value,
    /// #511: its `hook_event_name` when a prompt is submitted, and its answer that ends no turn but goes on with
    /// `reason` (none: "", `None`).
    pub prompt: &'static str,
    pub again: Option<fn(&str) -> Value>,
}

/// Every harness by name, in `NAMES` order.
pub fn all() -> impl Iterator<Item = (&'static str, &'static dyn Harness)> {
    NAMES.into_iter().map(|n| (n, get(n)))
}

/// The hook dialects of every harness that has one, in `NAMES` order.
pub fn dialects() -> impl Iterator<Item = &'static Dialect> {
    all().filter_map(|h| h.1.hook_dialect())
}

/// (used %, reset epoch) of a usage window, either part unread.
pub type Part = (Option<f64>, Option<u64>);

/// A harness's usage meter for an `[accounts]` login (#258).
pub trait UsageMeter: Sync {
    /// Read the 5-hour and 7-day windows of the login in `dir` (`[accounts.usage]`).
    fn poll(&self, r: &dyn Runner, dir: &Path, now: u64) -> crate::usage::Poll;
    /// The (5-hour, 7-day) parts of its status-line JSON (`herdsman statusline`).
    fn status(&self, v: &Value) -> (Part, Part);
}

/// The harness `[accounts]` tokens are for: the first that runs on one.
pub fn accounts() -> Option<&'static dyn Harness> {
    NAMES
        .iter()
        .map(|n| get(n))
        .find(|h| matches!(h.credential(), Credential::Token(_)))
}

pub trait Harness: Sync {
    /// Write this harness's files for a `role`-family `profile` (hooks) with `prompt`'s role prompt.
    fn sync(
        &self,
        _ctx: &Ctx,
        _role: &str,
        _prompt: &str,
        _profile: &str,
        _usage: &str,
        _out: &mut dyn Write,
    ) -> Result<()> {
        Ok(())
    }
    /// Fail unless the profile and install are ready to spawn plan `p`.
    fn ready(&self, _ctx: &Ctx, _r: &dyn Runner, _p: &Plan) -> Result<()> {
        Ok(())
    }
    /// Lane setup in the worktree before the harness starts.
    fn prepare(&self, _ctx: &Ctx, _on: &crate::hosts::On, _p: &Plan) -> Result<()> {
        Ok(())
    }
    /// herdr argv that launches the harness in `pane` (generic: its name typed into the lane's shell).
    fn start_cmd(&self, _ctx: &Ctx, p: &Plan, pane: &str) -> Vec<String> {
        herdr::run_argv(pane, &[&p.harness])
    }
    /// #355: `start_cmd` honours `Plan::session` (start it, or resume it with its conversation).
    fn resumes(&self) -> bool {
        false
    }
    /// #404: `start_cmd` passes `Plan::model` (else its profile owns the model).
    fn takes_model(&self) -> bool {
        false
    }
    /// herdr commands that settle the harness, deliver the kick and confirm it landed.
    fn kick_cmds(&self, p: &Plan, pane: &str) -> Vec<Vec<String>> {
        vec![herdr::send_text_argv(pane, &p.kick), herdr::keys_argv(pane, "enter")]
    }
    /// Run `kick_cmds`; true once the kick is confirmed landed.
    fn kick(&self, r: &dyn Runner, p: &Plan, pane: &str) -> Result<bool> {
        Ok(self
            .kick_cmds(p, pane)
            .iter()
            .all(|c| herdr::exec(r, c).is_ok_and(|o| o.code == 0)))
    }
    /// `VAR=value` words the lane's shell exports before the launch.
    fn env(&self) -> &'static [&'static str] {
        &[]
    }
    /// #565: the ACP adapter (its `shed agents` id) known to run this harness's hooks (generic: none, unguarded).
    fn acp(&self) -> Option<&'static str> {
        None
    }
    /// #565: the env its ACP adapter reads its config folder (`profile`) and model from.
    fn acp_env(&self, _profile: &Path, _model: Option<&str>) -> Vec<(&'static str, String)> {
        Vec::new()
    }
    /// How its lanes sign in (generic: unknown).
    fn credential(&self) -> Credential {
        Credential::None
    }
    /// The interactive login a person runs for it (setup panes, login hints).
    fn login(&self) -> Option<&'static str> {
        None
    }
    /// The shell line that installs it on a fresh host (`hosts bootstrap`; generic: none).
    fn install(&self) -> Option<&'static str> {
        None
    }
    /// Its `herdsman hook` protocol (generic: none, no tool guard but git's pre-push).
    fn hook_dialect(&self) -> Option<&'static Dialect> {
        None
    }
    /// Its accounts' usage meter (generic: none).
    fn usage_meter(&self) -> Option<&'static dyn UsageMeter> {
        None
    }
    /// The pane shows a usage-limit banner: until when.
    fn usage_limited(&self, _cfg: &crate::config::Config, _r: &dyn Runner, _screen: &str, _now: u64) -> Option<u64> {
        None
    }
    /// The pane shows a signed-out error (#302).
    fn signed_out(&self, _screen: &str) -> bool {
        false
    }
    /// The live spinner's word and, past a minute, its elapsed time; None when idle or without one.
    fn status_word(&self, _screen: &str) -> Option<(String, Option<String>)> {
        None
    }
    /// A transcript line is an action, the latest one the now-line (generic: any non-chrome line).
    fn now_line(&self, line: &str) -> bool {
        !crate::lanes::chrome(line)
    }
    /// A pane line's tool call as (tool, argument) for the `now:` line (generic: a `$ ` shell line).
    fn tool_line(&self, line: &str) -> Option<(String, String)> {
        Some(("$".into(), line.strip_prefix("$ ")?.trim_start().into()))
    }
    /// A blocked pane's question and numbered options (answered by `answer_keys`); None: a screen excerpt and reply box.
    fn pending_menu(&self, _screen: &str) -> Option<(String, Vec<String>)> {
        None
    }
    /// #463: a first-start prompt on a pane that would not start: its name and the keys that answer it, in order.
    fn startup_prompt(&self, _screen: &str) -> Option<(&'static str, Vec<String>)> {
        None
    }
    /// The keys that pick menu option `n` (1-based).
    fn answer_keys(&self, n: usize) -> String {
        n.to_string()
    }
    /// The newest local session transcript (`.jsonl`) of worktree `wt` under `home`; None: the chat reads the pane.
    fn transcript(&self, _home: &Path, _wt: &str) -> Option<PathBuf> {
        None
    }
    /// The login a host's herdr pane checks: (tool, command, fix).
    fn health_check(&self) -> Option<(&'static str, &'static str, &'static str)> {
        None
    }
    /// #359: what its startup terminal title (OSC 0) contains, confirming a lane came up (generic: none).
    fn startup_title(&self) -> Option<&'static str> {
        None
    }
    /// The herdr agent labels of its panes, for the Host tab's memory row (generic: an agent's own label is its row).
    fn process_names(&self) -> &'static [&'static str] {
        &[]
    }
}

struct Generic;
impl Harness for Generic {}

pub fn get(name: &str) -> &'static dyn Harness {
    match name {
        "claude" => &claude::Claude,
        "hermes" => &hermes::Hermes,
        "dsh" => &dsh::Dsh,
        _ => &Generic,
    }
}

/// #565: a `shed-acp` runtime's default adapter: the first harness's that runs its hooks.
pub fn default_acp() -> Option<&'static str> {
    all().find_map(|h| h.1.acp())
}

/// The herdsman hook's command line, as Claude settings and Hermes config/allowlist name it.
pub fn hook_command(ctx: &Ctx, role: &str) -> String {
    format!("'{}' hook --role {role}", ctx.exe)
}

/// Try numbers `0..n`, sleeping `secs` before every try but the first.
fn attempts(r: &dyn Runner, n: u32, secs: u64) -> impl Iterator<Item = u32> + '_ {
    (0..n).inspect(move |&i| {
        if i > 0 {
            r.sleep(secs);
        }
    })
}
