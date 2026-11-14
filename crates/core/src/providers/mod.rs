use crate::runner::Runner;
use crate::spawn::Plan;
use crate::term;
use crate::Ctx;
use anyhow::Result;
use serde_json::Value;
use std::io::Write;
use std::ops::Deref;
use std::path::{Path, PathBuf};

pub use claude::{remove_tmp, root as profiles_root, LOCAL_DIR, LOCAL_FILES};
pub use dsh::{install_pre_push, DSH_TUI_VERSION};
pub use hermes::set_mcp;

macro_rules! registry { ($($(#[$m:meta])* $n:ident),* $(,)?) => {
    $($(#[$m])* mod $n;)*
    static ALL: &[Entry] = &[$($(#[$m])* Entry { name: stringify!($n), provider: &$n::PROVIDER }),*];
} }
registry! { claude, hermes, dsh, codex, gemini, #[cfg(test)] fake }

/// A registry entry: its name (the registry's ident) and the provider it derefs to.
#[derive(Clone, Copy)]
pub struct Entry {
    pub name: &'static str,
    pub provider: &'static dyn Provider,
}

impl Deref for Entry {
    type Target = dyn Provider;
    fn deref(&self) -> &Self::Target {
        self.provider
    }
}

/// How a provider is reached.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Surface {
    Terminal,
    Acp,
    Listed,
}

/// Every provider, in registry order.
pub fn all() -> impl Iterator<Item = Entry> {
    ALL.iter().copied()
}

fn on(s: Surface) -> impl Iterator<Item = Entry> {
    all().filter(move |p| p.surface() == s)
}

/// The lane providers a role's `harness` may name.
pub fn lanes() -> impl Iterator<Item = Entry> {
    on(Surface::Terminal)
}

/// #732: providers a role reaches only through its ACP `agent` (no terminal lane, not offered as a `harness`).
pub fn acp_only() -> impl Iterator<Item = Entry> {
    on(Surface::Acp)
}

/// The names a role's `harness` may take.
pub fn lane_names() -> Vec<&'static str> {
    lanes().map(|p| p.name).collect()
}

/// #732: every provider name `get` knows, the lanes then the ACP-only ones.
pub fn known() -> impl Iterator<Item = &'static str> {
    lanes().chain(acp_only()).map(|p| p.name)
}

/// #732: ACP agent `id`'s model vendor, if known; a fleet's `rev` runs on another vendor than its `impl`.
pub fn vendor_of_agent(id: &str) -> Option<&'static str> {
    all().find(|p| p.acp() == Some(id)).and_then(|p| p.vendor())
}

/// #732: the provider whose adapter ACP agent `id` is (the one that guards it).
pub fn of_agent(id: &str) -> Option<&'static str> {
    known().find(|h| get(h).acp() == Some(id))
}

/// #198: the names an AI co-author trailer carries.
pub fn brands() -> impl Iterator<Item = &'static str> {
    all().flat_map(|p| p.brands().iter().copied())
}

/// #637: the token proxy's provider: the first that has one.
pub fn proxied() -> Option<&'static Proxy> {
    all().find_map(|p| p.proxy())
}

/// Effort levels a role may set (`effort = "…"`).
pub const EFFORTS: &[&str] = &["none", "low", "medium", "high", "xhigh"];

/// How a harness's lanes sign in: unknown, an `[accounts]` account of this provider (`access::pick`; #683: its env from `access::account_env`), or the harness's own login on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Credential {
    None,
    Account(Accounts),
    Own,
}

/// What an `[accounts]` account is to its provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Accounts {
    pub provider: &'static str,
    pub config_env: &'static str,
    pub token_env: Option<&'static str>,
}

/// #637: what the token proxy needs from the provider it fronts.
pub struct Proxy {
    pub upstream: &'static str,
    pub base_url_env: &'static str,
    pub auth_token_env: &'static str,
    pub headers: fn(&[Header]) -> Vec<Header>,
}

/// A forwarded HTTP header: its lowercase name and its value.
pub type Header = (String, String);

/// A provider's own folder under `~`, its env var overriding it.
pub struct Home {
    pub env: &'static str,
    pub default: &'static str,
}

impl Home {
    /// The folder: its env var when set and not empty, else the default under `home`.
    pub fn resolve(&self, home: &Path) -> PathBuf {
        std::env::var_os(self.env)
            .filter(|v| !v.is_empty())
            .map_or_else(|| home.join(self.default), PathBuf::from)
    }
}

/// A `drover doctor` row: passed, its mark when not, its text.
pub type Row = (bool, char, String);

/// A harness's hook protocol, as `drover hook` reads and answers it: whether it `sent` a call, the keys of a call's tool `input` (the first object wins), its `shell` and file-writing tools, its `hook_event_name` after a tool call and at a turn's end, and its `deny` answer.
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
    /// #511: its `hook_event_name` when a prompt is submitted, and its answer that ends no turn but goes on with `reason` (none: "", `None`).
    pub prompt: &'static str,
    pub again: Option<fn(&str) -> Value>,
    /// #909: its answer that runs the tool call with this tool input instead (none: it can't).
    pub rewrite: Option<fn(Value) -> Value>,
}

/// The hook dialects of every harness that has one, in `known` order.
pub fn dialects() -> impl Iterator<Item = &'static Dialect> {
    known().filter_map(|h| get(h).hook_dialect())
}

/// A usage window: used %, reset epoch, either unread.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Window {
    pub used: Option<f64>,
    pub resets: Option<u64>,
}

/// An account's limits: its session and its weekly window.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Limits {
    pub session: Window,
    pub week: Window,
}

/// A provider's usage meter for an `[accounts]` login (#258; #707: its status line only, no poll).
pub trait Usage: Sync {
    fn reading(&self, status_line: &Value) -> Limits;
    fn cap_banner(&self) -> &'static str;
}

/// The provider `[accounts]` are for: the first lane provider that runs on one.
pub fn accounts() -> Option<Entry> {
    lanes().find(|h| matches!(h.credential(), Credential::Account(_)))
}

pub trait Provider: Sync {
    fn surface(&self) -> Surface {
        Surface::Terminal
    }
    fn vendor(&self) -> Option<&'static str> {
        None
    }
    fn brands(&self) -> &'static [&'static str] {
        &[]
    }
    fn proxy(&self) -> Option<&'static Proxy> {
        None
    }
    fn efforts(&self) -> &'static [&'static str] {
        EFFORTS
    }
    fn model_default(&self) -> Option<&'static str> {
        None
    }
    fn home(&self) -> Option<Home> {
        None
    }
    fn profile_files(&self, _ctx: &Ctx, _profile: &str) -> Vec<PathBuf> {
        vec![]
    }
    fn hooks_allowed(
        &self,
        _ctx: &Ctx,
        _cfg: &crate::config::Config,
        _r: &dyn Runner,
        _out: &mut dyn Write,
    ) -> Result<bool> {
        Ok(true)
    }
    fn doctor(&self, _ctx: &Ctx, _cfg: &crate::config::Config, _r: &dyn Runner) -> Vec<Row> {
        vec![]
    }
    fn scrubs_env(&self, _var: &str) -> bool {
        false
    }
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
    fn ready(&self, _ctx: &Ctx, _r: &dyn Runner, _p: &Plan) -> Result<()> {
        Ok(())
    }
    fn prepare(&self, _ctx: &Ctx, _on: &crate::hosts::On, _p: &Plan) -> Result<()> {
        Ok(())
    }
    fn start_cmd(&self, _ctx: &Ctx, p: &Plan, pane: &str) -> Vec<String> {
        term::run_argv(pane, &[&p.harness])
    }
    fn resumes(&self) -> bool {
        false
    }
    fn takes_model(&self) -> bool {
        false
    }
    fn kick_cmds(&self, p: &Plan, pane: &str) -> Vec<Vec<String>> {
        vec![term::send_text_argv(pane, &p.kick), term::keys_argv(pane, "enter")]
    }
    fn kick(&self, r: &dyn Runner, p: &Plan, pane: &str) -> Result<bool> {
        Ok(self
            .kick_cmds(p, pane)
            .iter()
            .all(|c| term::exec(r, c).is_ok_and(|o| o.code == 0)))
    }
    fn env(&self) -> &'static [&'static str] {
        &[]
    }
    fn acp(&self) -> Option<&'static str> {
        None
    }
    fn acp_env(&self, _model: Option<&str>, _effort: Option<&str>) -> Vec<(&'static str, String)> {
        Vec::new()
    }
    fn signed_in(&self, _r: &dyn Runner, _dir: &Path) -> Option<bool> {
        None
    }
    fn acp_profile(&self, _settings: &str, _role: &str) -> String {
        String::new()
    }
    fn credential(&self) -> Credential {
        Credential::None
    }
    fn login(&self) -> Option<&'static str> {
        None
    }
    fn install(&self) -> Option<&'static str> {
        None
    }
    fn hook_dialect(&self) -> Option<&'static Dialect> {
        None
    }
    fn usage(&self) -> Option<&'static dyn Usage> {
        None
    }
    fn usage_limited(&self, _cfg: &crate::config::Config, _r: &dyn Runner, _screen: &str, _now: u64) -> Option<u64> {
        None
    }
    fn signed_out(&self, _screen: &str) -> bool {
        false
    }
    fn status_word(&self, _screen: &str) -> Option<(String, Option<String>)> {
        None
    }
    fn now_line(&self, line: &str) -> bool {
        !crate::lanes::chrome(line)
    }
    fn tool_line(&self, line: &str) -> Option<(String, String)> {
        Some(("$".into(), line.strip_prefix("$ ")?.trim_start().into()))
    }
    fn pending_menu(&self, _screen: &str) -> Option<(String, Vec<String>)> {
        None
    }
    fn answer_keys(&self, n: usize) -> String {
        n.to_string()
    }
    fn transcript(&self, _home: &Path, _wt: &str) -> Option<PathBuf> {
        None
    }
    fn health_check(&self) -> Option<(&'static str, &'static str, &'static str)> {
        None
    }
    fn process_names(&self) -> &'static [&'static str] {
        &[]
    }
}

struct Generic;
impl Provider for Generic {}

/// The provider a role's `harness` names; an unknown or listed-only one gets the generic fallbacks.
pub fn get(name: &str) -> &'static dyn Provider {
    all()
        .find(|p| p.name == name && p.surface() != Surface::Listed)
        .map_or(&Generic, |p| p.provider)
}

/// #565: a `shed-acp` runtime's default adapter: the first lane provider's that runs its hooks.
pub fn default_acp() -> Option<&'static str> {
    lanes().find_map(|h| h.acp())
}

/// The drover hook's command line, as Claude settings and Hermes config/allowlist name it.
pub fn hook_command(ctx: &Ctx, role: &str) -> String {
    format!("'{}' hook --role {role}", real(&ctx.exe))
}

fn real(path: &str) -> String {
    std::fs::canonicalize(path).map_or(path.into(), |p| p.to_string_lossy().into())
}

/// #737: the program of a drover hook command (`'<exe>' hook --role <r>`), any role; None for another hook.
pub fn hook_program(cmd: &str) -> Option<&str> {
    cmd.strip_prefix('\'')?.split_once("' hook --role ").map(|(exe, _)| exe)
}

/// #737: drover hook command `cmd` runs the running binary, through a symlink or not.
pub fn runs_this(ctx: &Ctx, cmd: &str) -> bool {
    hook_program(cmd).is_some_and(|exe| real(exe) == real(&ctx.exe))
}

/// #737: hook command `cmd` is this binary's hook for `role`.
pub fn guards(ctx: &Ctx, cmd: &str, role: &str) -> bool {
    runs_this(ctx, cmd) && cmd.ends_with(&format!("' hook --role {role}"))
}

fn attempts(r: &dyn Runner, n: u32, secs: u64) -> impl Iterator<Item = u32> + '_ {
    (0..n).inspect(move |&i| {
        if i > 0 {
            r.sleep(secs);
        }
    })
}
