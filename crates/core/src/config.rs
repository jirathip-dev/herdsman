//! fleet.toml: load, and in-place value edits that preserve every other byte.
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// #162: the accent tokens a fleet card's bar takes (`--l` blue … `--a` amber); red means "needs you", so it is left out.
pub const COLORS: [&str; 5] = ["l", "p", "c", "g", "a"];

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub fleet: BTreeMap<String, Fleet>,
    pub limits: Limits,
    pub notify: Vec<Notify>,
    pub accounts: Accounts,
    /// MCP server names no lane is given (`*` wildcards, case-insensitive).
    #[serde(default = "mcp_deny")]
    pub mcp_deny: Vec<String>,
    /// The models `herdsman serve` offers per role (its model dropdown).
    pub models: Vec<String>,
    pub serve: Serve,
    pub providers: Providers,
    /// #157: `[hosts.<name>]` lanes may run on (docs/hosts.md); none = this host only, as before.
    pub hosts: BTreeMap<String, HostCfg>,
    /// #303: `[[setup]]` logins a host card's [sign in] runs (`crate::setup`).
    pub setup: Vec<SetupCfg>,
    /// #298: `[runtime]`, the terminal runtime new lanes start in (docs/runtimes.md); herdr when unset.
    pub runtime: Runtime,
    /// #355: `[conductor]`: the conductor profile, plain markdown in a repo the owner picks (`crate::conductor`).
    pub conductor: ConductorCfg,
    /// #387: `[telegram]`, the optional bridge (`crate::telegram`); off without `chat`.
    pub telegram: Telegram,
    /// #345: `[service] url`, where lanes on other hosts reach `herdsman service`'s API (docs/service.md).
    pub service: ServiceCfg,
    /// #404: `[chat]`, new threads' conductor defaults over the fleet's `role.conductor` (`conductor::defaults`).
    pub chat: Chat,
    /// #537: `[ci_runners]`, the owner's CI runner VMs (`crate::runners`, docs/hosts.md).
    pub ci_runners: Option<CiRunners>,
}

/// #537: the repo's runner VMs: their recipe (`provider`), spec, slots per VM, label, and the `max_vms` / `usd_day` cap;
/// `vms` are the ones made by hand (`runners add|remove` change the set in the log); `local_*` the mini's runners.
#[derive(Debug, Deserialize)]
pub struct CiRunners {
    pub provider: String,
    pub repo: String,
    pub label: String,
    pub size: String,
    #[serde(default)]
    pub ttl_hours: u64,
    #[serde(default)]
    pub snapshots: bool,
    pub slots: u64,
    pub max_vms: u64,
    pub usd_day: f64,
    #[serde(default)]
    pub vms: Vec<String>,
    pub local_label: Option<String>,
    pub local_stop: Option<String>,
    /// The VMs' runner service (the recipe's `{service}`).
    #[serde(default = "ci_service")]
    pub service: String,
}

fn ci_service() -> String {
    "ci-runner".into()
}

#[derive(Debug, Default, Deserialize)]
pub struct ServiceCfg {
    pub url: Option<String>,
}

/// #404: the harness, model, effort and account(s) a new thread's conductor starts on; each unset one is the role's.
#[derive(Debug, Default, Deserialize)]
pub struct Chat {
    pub harness: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub account: Option<String>,
    #[serde(default)]
    pub accounts: Vec<String>,
}

/// #387: the forum supergroup (`chat`), the one user whose messages and taps count (`owner`), and the Bot API root.
#[derive(Debug, Default, Deserialize)]
pub struct Telegram {
    pub chat: Option<i64>,
    pub owner: Option<i64>,
    pub api: Option<String>,
}

/// #355: the conductor's instructions file, skills folder and memory notes folder (`~/` expands); #386: `dir`, the
/// root of the per-thread working folders (default `<state dir>/conductor`); #538: `allow`, its capability groups.
#[derive(Debug, Default, Deserialize)]
pub struct ConductorCfg {
    pub dir: Option<String>,
    pub instructions: Option<String>,
    pub skills: Option<String>,
    pub memory: Option<String>,
    pub allow: Option<Vec<String>>,
}

/// #538: each capability group a conductor may be allowed and its herdsman verbs (the hook's `group`).
pub const GROUPS: &[(&str, &str)] = &[
    ("design", "`spawn … design`"),
    ("clean", "`clean --yes`, `--orphans`, `--lane`"),
    ("hosts", "`hosts on|off|refresh`"),
    ("reclaim", "`reclaim`"),
    ("accounts", "`accounts reset`"),
    ("runners", "`runners …`"),
    ("pause", "`pause`, `resume`"),
    ("set", "`set` a role's harness, model or effort"),
];

impl ConductorCfg {
    /// #538: the groups on: `allow`, else every group but `pause` and `set`.
    pub fn groups(&self) -> Vec<&str> {
        let default = || {
            GROUPS
                .iter()
                .map(|g| g.0)
                .filter(|g| !["pause", "set"].contains(g))
                .collect()
        };
        self.allow
            .as_ref()
            .map_or_else(default, |a| a.iter().map(String::as_str).collect())
    }
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    #[default]
    Herdr,
    Shed,
    /// #565: shed running each lane as an ACP session (`acp` defaults to `claude-acp`, `acp_policy` to `allow`).
    #[serde(rename = "shed-acp")]
    ShedAcp,
    /// #455: shed's old name, accepted for one release with a warning (and a doctor row).
    Berth,
}

/// #298: `kind`, herdr or shed; shed's `socket` (default its per-user one), and an ACP agent id its lanes run
/// instead of the harness's terminal UI (`shed run --acp`), with shed's permission `acp_policy` (default `ask`).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Runtime {
    pub kind: RuntimeKind,
    pub socket: Option<String>,
    pub acp: Option<String>,
    pub acp_policy: Option<String>,
}

impl Runtime {
    /// The shed runtime, when it is the kind.
    pub fn shed(&self) -> Option<crate::shed::Shed> {
        let acp = self.kind == RuntimeKind::ShedAcp;
        (acp || matches!(self.kind, RuntimeKind::Shed | RuntimeKind::Berth)).then(|| crate::shed::Shed {
            wire: crate::shed::Wire::Socket(self.socket.as_deref().map_or_else(crate::shed::default_socket, expand)),
            acp: self
                .acp
                .clone()
                .or_else(|| acp.then(crate::harness::default_acp).flatten().map(Into::into)),
            policy: self
                .acp_policy
                .clone()
                .unwrap_or_else(|| if acp { "allow" } else { "ask" }.into()),
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct SetupCfg {
    pub name: String,
    pub cmd: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// #157: one host: its herdr machine label (unset = this host), hand tags, worktree root, and whether new lanes go there.
#[derive(Debug, Deserialize)]
pub struct HostCfg {
    pub machine: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub root: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// #254: a cloud host's recipe (`boat`, or `<config>/recipes/<name>.toml`), size, limits and idle stop (20 min).
    pub cloud: Option<String>,
    pub size: Option<String>,
    pub budget_usd_month: Option<f64>,
    pub max_hours_day: Option<f64>,
    pub idle_stop_minutes: Option<u64>,
    /// #256: its lanes' command rules: `deny_commands` replaces the fleet's list, `deny_commands_add` extends it, and
    /// `allow_commands` drops entries (unset: a cloud host allows `CLOUD_ALLOW`, a Mac nothing).
    pub deny_commands: Option<Vec<Deny>>,
    #[serde(default)]
    pub deny_commands_add: Vec<Deny>,
    pub allow_commands: Option<Vec<String>>,
}

/// #256: a cloud host is a disposable machine: its lanes may start containers.
pub const CLOUD_ALLOW: [&str; 3] = ["docker", "docker compose", "supabase start"];

impl HostCfg {
    pub fn allow(&self) -> Vec<String> {
        let def = || {
            CLOUD_ALLOW
                .iter()
                .filter(|_| self.cloud.is_some())
                .map(ToString::to_string)
                .collect()
        };
        self.allow_commands.clone().unwrap_or_else(def)
    }
    /// #256: the lane hook's deny list on this host, from the fleet's (`fleet`).
    pub fn deny(&self, fleet: Vec<Deny>) -> Vec<Deny> {
        let (allow, mut d) = (self.allow(), self.deny_commands.clone().unwrap_or(fleet));
        d.extend(self.deny_commands_add.iter().cloned());
        d.retain(|x| !allow.iter().any(|a| a == x.cmd()));
        d
    }
}

fn yes() -> bool {
    true
}

/// `[providers]`: API providers whose balance the Host tab shows (docs/providers.md).
#[derive(Debug, Default, Deserialize)]
pub struct Providers {
    pub deepseek: Option<Provider>,
}

/// The key is read from `env_file` (else the store's `all` secret `key_env`, else the environment), never from fleet.toml.
#[derive(Debug, Deserialize)]
pub struct Provider {
    pub key_env: String,
    pub env_file: Option<String>,
    #[serde(default = "warn_below")]
    pub warn_below: f64,
}

fn warn_below() -> f64 {
    5.0
}

fn mcp_deny() -> Vec<String> {
    vec!["*prod*".into()]
}

/// `[serve]`: extra `Host` names (e.g. the `*.ts.net` name of `tailscale serve`) a POST may come from,
/// and `approve_hint`, appended to the pairing panel's Mac line (`{code}` substituted).
#[derive(Debug, Default, Deserialize)]
pub struct Serve {
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    pub approve_hint: Option<String>,
    /// #98 `herdsman setup`: the board job's label (default `dev.herdsman.serve`), address, `tailscale serve` port.
    pub launchd_label: Option<String>,
    pub addr: Option<String>,
    pub tailscale_https_port: Option<u16>,
    /// #626: the Funnel listener, `POST /hooks/github` only (default `127.0.0.1:7778`; `""`: off).
    pub hooks_addr: Option<String>,
    /// #172: the Herd's theme, `<config dir>/themes/<name>/` or built-in (default `farm`).
    pub theme: Option<String>,
}

impl Serve {
    pub fn addr(&self) -> String {
        self.addr.clone().unwrap_or_else(|| "127.0.0.1:7777".into())
    }
    pub fn hooks_addr(&self) -> String {
        self.hooks_addr.clone().unwrap_or_else(|| "127.0.0.1:7778".into())
    }
}

/// `[accounts]`: account name -> its token's secret name (#441: in the store, `herdsman accounts import`), plus the
/// settings below.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Accounts {
    /// #539: retired by #441, a warning (`Config::warnings`), never a load error; its tokens still back the store's
    /// missing ones, read-only, until the import and the line's removal.
    pub accounts_env: Option<String>,
    /// Usage-limit banner, `|`-separated substrings (default: the harness's own, `Harness::usage_limited`).
    pub cap_pattern: Option<String>,
    pub cap_reset_minutes: u64,
    /// `[accounts.usage]`: account name -> the config dir of its own harness login (`Harness::usage_meter`), read only for the usage meter.
    pub usage: BTreeMap<String, String>,
    /// `headroom`: spawns take the uncapped account with the most 5h/7d headroom; `order` (default): as before.
    pub pick: Option<String>,
    #[serde(flatten)]
    pub names: BTreeMap<String, String>,
}

impl Default for Accounts {
    fn default() -> Self {
        Accounts {
            accounts_env: None,
            cap_pattern: None,
            cap_reset_minutes: 300,
            usage: BTreeMap::new(),
            pick: None,
            names: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Fleet {
    pub repo: String,
    pub local: String,
    pub integration: String,
    /// #294: `[[fleet.<n>.serial]]` groups: globs only `max` issues at a time may touch (`SERIAL_BUSY`, `SERIAL_UNMARKED`).
    #[serde(default)]
    pub serial: Vec<Serial>,
    /// The old form of one group named `serial_label` (unset = `db`); `Config::parse` reads it into `serial`.
    #[serde(default)]
    pub serial_paths: Vec<String>,
    pub serial_max: Option<usize>,
    pub serial_label: Option<String>,
    /// The only checks merge, `check` and `wait` look at (latest run of each); empty = all.
    #[serde(default)]
    pub required_checks: Vec<String>,
    /// #150: a label the PR needs for its full CI (e.g. `merge-ready`), kept on it exactly while a PASS stands at its
    /// head; unset = none.
    pub merge_label: Option<String>,
    /// #285: `always` catches every behind ready PR up with integration; unset/`needed`: only on a conflict or a strict rule.
    pub update_ready: Option<String>,
    /// Optional caps on concurrent impl/fix and review lanes (CAP); unset = no cap.
    pub max_impl: Option<usize>,
    pub max_rev: Option<usize>,
    #[serde(default)]
    pub role: BTreeMap<String, Role>,
    /// Replace the global `[[notify]]` for this fleet's events; `notify_global` sends to both.
    #[serde(default)]
    pub notify: Vec<Notify>,
    #[serde(default)]
    pub notify_global: bool,
    /// `herdsman pause`: `next` offers nothing and impl/rev spawns refuse PAUSED.
    #[serde(default)]
    pub paused: bool,
    /// #103: exported into every lane (`~/` expands).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// #361: build caches shared by the fleet's lanes, exported like `env`; opt-in.
    #[serde(default)]
    pub shared_cache: BTreeMap<String, String>,
    /// #185: build folders a new lane APFS-clones from `local` on a lockfile match; `clone_node_modules` is its alias for one release.
    #[serde(default)]
    pub clone_build: Vec<String>,
    #[serde(default)]
    pub clone_node_modules: bool,
    /// #187: lanes' `HERDSMAN_PORT` range, `lo-hi`; unset = `41000-41999`.
    pub port_range: Option<String>,
    /// #164: build folders freed besides `target`, `node_modules`, `.next` and `dist` (paths in the worktree).
    #[serde(default)]
    pub build_dirs: Vec<String>,
    /// #173: `[fleet.<n>.pipeline]`; `Config::parse` fills in the built-in one when absent.
    pub pipeline: Option<crate::pipeline::Pipeline>,
    /// #173: the pipeline's broken rules: every command but `serve`/`doctor`/`status` refuses `BAD_PIPELINE`.
    #[serde(skip)]
    pub bad: Vec<crate::pipeline::Problem>,
    /// #162: the Queue card's colour, one of `COLORS`; unset = a stable hash of the fleet name.
    pub color: Option<String>,
    /// #157: host tags every lane of this fleet needs (e.g. `xcode`).
    #[serde(default)]
    pub needs: Vec<String>,
    /// #228: this fleet's form categories and form rounds; `Config::parse` fills in `[limits]`'s when absent.
    pub form_categories: Option<Vec<String>>,
    pub form_rounds: Option<u32>,
    /// #216: replaces `[limits] deny_commands` (empty = off); `deny_commands_add` adds to whichever applies.
    pub deny_commands: Option<Vec<Deny>>,
    #[serde(default)]
    pub deny_commands_add: Vec<Deny>,
    /// #272: fast checks `merge` runs on the PR merged into current integration; unset = the cap check if `ci/cap.toml`.
    pub premerge: Option<Vec<Premerge>>,
    /// #350: `[fleet.<n>.land]`: how a passed PR lands; unset = full-first (today's rule).
    pub land: Option<Land>,
    /// #419: `weekly` (unset) has `tick` run `herdsman lesson` every 7 days; `off` opts out.
    pub lessons: Option<String>,
    /// #417: globs of UI files: a PR touching one goes through the verify stage.
    #[serde(default)]
    pub ui_paths: Vec<String>,
    /// #541: the conductor thread (`t6`) lane asks go to first; unset = the repo's most recently active thread.
    pub ask_thread: Option<String>,
    /// #565: `[fleet.<n>.runtime]`, this fleet's new lanes' runtime over the global `[runtime]` (a pilot on one fleet).
    pub runtime: Option<Runtime>,
}

/// #350: `mode = "fast-first"` lands a PASS once its `fast` checks are green (one touching `full_first_paths` waits
/// for the full set); integration's own CI then runs the full suite, and `land::watch` reverts a red landing.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Land {
    pub mode: String,
    pub fast: Vec<String>,
    pub full_first_paths: Vec<String>,
}

/// #373: a plain command, or a named check: runs if the PR changes `when` paths, refuses with `code`, briefs `fix`.
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum Premerge {
    Cmd(String),
    Named(Check),
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct Check {
    pub name: String,
    pub run: String,
    #[serde(default)]
    pub when: Vec<String>,
    pub code: Option<String>,
    pub fix: Option<String>,
}

/// #294: a serial group; `name` (≤ 8 chars) is also its Queue mark.
#[derive(Debug, Deserialize, Clone)]
pub struct Serial {
    pub name: String,
    pub paths: Vec<String>,
    #[serde(default = "one")]
    pub max: usize,
}

fn one() -> usize {
    1
}

impl Serial {
    /// The first of `files` one of the group's globs matches.
    pub fn hit<'a>(&self, files: &'a [String]) -> Option<&'a String> {
        let hit = |p: &&String| self.paths.iter().any(|g| glob(g.as_bytes(), p.as_bytes()));
        files.iter().find(hit)
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct Role {
    pub harness: String,
    pub profile: Option<String>,
    pub model: Option<String>,
    /// Reasoning effort, mapped per harness (`harness::effort_ok`); `[limits].effort` when unset.
    pub effort: Option<String>,
    /// Claude lanes: the account (`[accounts]` name) to run on, or an ordered fallback list.
    pub account: Option<String>,
    #[serde(default)]
    pub accounts: Vec<String>,
    /// #302: ordered `harness[:account]` entries a lane carries on with when its agent is unavailable; empty = off.
    #[serde(default)]
    pub fallback: Vec<String>,
    /// MCP servers (by `.mcp.json` name) the lane gets; none by default.
    #[serde(default)]
    pub mcp: Vec<String>,
    /// #173: a file appended to the role's kind prompt (`roles/impl.md` for a build, `roles/rev.md` for a check).
    pub prompt: Option<String>,
    /// #157: host tags this role's lanes need besides the fleet's.
    #[serde(default)]
    pub needs: Vec<String>,
    /// #236: the host its lanes always run on (`main`: the one without `machine`); `design` defaults to `main`.
    pub host: Option<String>,
    /// #401: the default conductor `Config::parse` adds for a fleet without one: its impl role on `lane-conductor`.
    #[serde(skip)]
    pub default: bool,
}

impl Role {
    pub fn account_list(&self) -> Vec<String> {
        if self.accounts.is_empty() {
            self.account.iter().cloned().collect()
        } else {
            self.accounts.clone()
        }
    }
    pub fn profile_or(&self, role: &str) -> String {
        self.profile.clone().unwrap_or_else(|| format!("fleet-{role}"))
    }
    pub fn effort_or(&self, limits: &Limits) -> Option<String> {
        self.effort.clone().or_else(|| limits.effort.clone())
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub review_rounds: u32,
    pub update_rounds: u32,
    pub stall_minutes: u64,
    pub ci_start_minutes: u64,
    /// Host-load gate for `herdsman spawn` (`HOST_BUSY`).
    pub load_per_cpu: f64,
    /// Pages swapped out per 2 s sample.
    pub swapout_rate: f64,
    /// `herdsman spawn` refuses `DISK_LOW` under this many GB free on `/`.
    pub min_free_gb: f64,
    /// #164: over this many GB of worktrees drive frees the idlest lanes' build folders, then spawn refuses `DISK_BUDGET`.
    pub worktree_budget_gb: f64,
    /// #438: lanes share `CARGO_TARGET_DIR=<build_root>/<repo>` on their host (empty: off; a fleet's own wins); over
    /// `build_budget_gb` drive trims its unlocked profiles.
    pub build_root: String,
    pub build_budget_gb: f64,
    /// #536: where `herdsman clean` looks for stray Cargo build folders and Claude Code's per-worktree temp dirs.
    pub stray_roots: Vec<String>,
    /// #168: `herdsman spawn` refuses `MEM_LOW` under this many GB of free + inactive memory.
    pub min_free_mem_gb: f64,
    /// Default reasoning effort for roles that set none.
    pub effort: Option<String>,
    /// #99: a routine ask unanswered this long takes its recommended option.
    pub ask_default_minutes: u64,
    /// #584: an ask nobody waits on (no live asking lane) closes as expired after this many hours.
    pub ask_expire_hours: u64,
    /// #223: a lane `done`/idle this long loses the processes its agent started (tsserver, vite, ...).
    pub idle_reap_minutes: u64,
    /// #248: a `done` lane handed off this long closes its session; lane settings' `cleanupPeriodDays`.
    pub done_close_minutes: u64,
    pub transcript_days: u64,
    /// #216: commands the lane hook denies; `[fleet.<n>] deny_commands` replaces it, `deny_commands_add` extends it.
    pub deny_commands: Vec<Deny>,
    /// #212: the fast step (`drive --fast`) and the slow tick run every this many seconds.
    pub fast_secs: u64,
    pub slow_secs: u64,
    /// #228: the finding tags a `FAIL (form-only)` may carry, and how many per issue don't count as a review round.
    pub form_categories: Vec<String>,
    pub form_rounds: u32,
    /// #254: every local host refused this long lets placement start a stopped cloud host.
    pub overflow_after_minutes: u64,
    /// #283: a remote host's asks older than this are never imported; `check` pushes this many a pass, then a summary.
    pub import_max_age_hours: u64,
    pub max_pushes_per_pass: usize,
    pub max_parallel_spawns: usize, // #318: concurrent detached spawn kicks (0: in the pass)
    pub spawn_pending_max_secs: u64, // #333: a spawn marker or kick lock older than this is stale
    /// #302: how long an unavailable agent rests when its message names no reset.
    pub fallback_rest_minutes: u64,
    /// #343: the event log over this many MB moves its old events to monthly archives (each slow pass).
    pub rotate_mb: u64,
    /// #626: with GitHub webhooks on (a `GITHUB_WEBHOOK_SECRET`), the fast step re-reads PRs only this often.
    pub webhook_poll_minutes: u64,
}

/// #216: a `deny_commands` entry: a command word or command + subcommand prefix, bare or `{ cmd, why }`.
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum Deny {
    Cmd(String),
    Why { cmd: String, why: String },
}

impl Deny {
    pub fn cmd(&self) -> &str {
        match self {
            Deny::Cmd(c) | Deny::Why { cmd: c, .. } => c,
        }
    }
    /// The hook's message: `why` verbatim, else `REFUSED NO_COMMAND`.
    pub fn reason(&self) -> String {
        match self {
            Deny::Cmd(c) => format!("REFUSED NO_COMMAND: lanes never run `{c}` (deny_commands); ask the owner"),
            Deny::Why { why, .. } => why.clone(),
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            review_rounds: 3,
            update_rounds: 3,
            stall_minutes: 30,
            ci_start_minutes: 10,
            load_per_cpu: 2.5,
            swapout_rate: 2000.0,
            min_free_gb: 10.0,
            worktree_budget_gb: 25.0,
            build_root: "~/.cache/herdsman/target".into(),
            build_budget_gb: 20.0,
            stray_roots: vec!["/tmp".into(), "$TMPDIR".into()],
            min_free_mem_gb: 2.0,
            effort: None,
            ask_default_minutes: 30,
            ask_expire_hours: 24,
            idle_reap_minutes: 10,
            done_close_minutes: 5,
            transcript_days: 30,
            // #216: lanes never start containers (owner rule); the list is assets/deny_commands.toml.
            deny_commands: toml::from_str::<toml::Table>(include_str!("../assets/deny_commands.toml"))
                .ok()
                .and_then(|mut t| t.remove("deny_commands")?.try_into().ok())
                .unwrap_or_default(),
            fast_secs: 60,
            slow_secs: 300,
            overflow_after_minutes: 10,
            form_categories: [
                "trailer",
                "pr-body",
                "stale-baseline",
                "format",
                "docs-fresh",
                "migration-redate",
                "infra",
            ]
            .map(String::from)
            .into(),
            form_rounds: 3,
            import_max_age_hours: 24,
            max_pushes_per_pass: 3,
            max_parallel_spawns: 6,
            spawn_pending_max_secs: 300,
            fallback_rest_minutes: 60,
            rotate_mb: 20,
            webhook_poll_minutes: 10,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Notify {
    #[serde(default)]
    pub cmd: String,
    #[serde(default)]
    pub on: Vec<String>,
    /// "json" sends the raw event; anything else one plain line.
    #[serde(default)]
    pub format: Option<String>,
    /// "webpush": push to the page's subscribed devices instead of running `cmd`.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// webpush: the VAPID `sub` claim (`mailto:` or `https:`).
    #[serde(default)]
    pub sub: Option<String>,
}

/// A webpush notifier's events when `on` is unset: the Telegram set plus the asks (#40) and needs-human (#130).
pub const WEBPUSH_ON: &[&str] = &[
    "thread-msg",
    "blocked",
    "stall",
    "lane-dead",
    "ready-unmerged",
    "ci-missing",
    "outbox-stale",
    "merged",
    "needs-decision",
    "needs-human",
    "ask-waiting",
    "answer-undelivered",
    "state-write-failed",
    "wip-blocked",
    "spawn-held",
    "host-busy",
    "cap-near",
    "orphan-issue",
    "config-warning",
    "keychain-locked",
    "runners-low",
    "runners-ok",
];

impl Notify {
    pub fn webpush(&self) -> bool {
        self.kind.as_deref() == Some("webpush")
    }
    /// #350: every landing pause and revert (or failed revert), and #345's stalled service, goes to each notifier,
    /// whatever its `on` says;
    /// #363: so does a board report, to each notifier but the phones'.
    pub fn wants(&self, kind: &str) -> bool {
        self.on.iter().any(|k| k == kind)
            || (self.on.is_empty() && self.webpush() && WEBPUSH_ON.contains(&kind))
            || matches!(kind, "land-paused" | "reverted" | "revert-failed" | "service-stalled")
            || kind == "report" && !self.webpush()
    }
}

impl Config {
    /// #539: each retired key still set, as `(key, what to do)`: doctor's `!` rows, the board's line, `check`'s
    /// `config-warning`; never a load error.
    pub fn warnings(&self) -> Vec<(&'static str, String)> {
        let run = |f| format!("retired (#441): run `herdsman accounts import {f}`, then drop the line");
        let f = self.accounts.accounts_env.as_deref();
        f.map(|f| ("[accounts].accounts_env", run(f))).into_iter().collect()
    }

    /// #539: secret `name` from the retired `accounts_env` file, for one the store lacks; None without it.
    pub fn legacy_secret(&self, name: &str) -> Option<String> {
        let text = std::fs::read_to_string(expand(self.accounts.accounts_env.as_deref()?)).ok()?;
        crate::usage::env_value(&text, name).filter(|v| !v.is_empty())
    }

    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).with_context(|| format!("cannot read config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("invalid config {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Config> {
        let c: Config = toml::from_str(text)?;
        let raw: toml::Table = toml::from_str(text)?;
        if let Some((k, _)) = strings("", &toml::Value::Table(raw.clone()))
            .into_iter()
            .find(|(_, v)| secret_like(v))
        {
            return crate::gate::refused(
                "SECRET_IN_CONFIG",
                format!("{k} looks like a secret; keep keys in an env file (docs/providers.md)"),
            );
        }
        let env = c.fleet.iter().flat_map(|(n, f)| f.env.values().map(move |v| (n, v)));
        if let Some((n, v)) = env
            .filter_map(|(n, v)| Some((n, v.strip_prefix("secret:")?)))
            .find(|v| !crate::secret::name_ok(v.1))
        {
            bail!("fleet.{n}.env: secret:{v} is not a secret name (A-Z, 0-9, _)");
        }
        if raw.get("limits").and_then(|l| l.get("max_agents")).is_some() {
            eprintln!("herdsman: [limits].max_agents is no longer used; ignoring it");
        }
        if c.runtime.kind == RuntimeKind::Berth {
            eprintln!("herdsman: [runtime] kind = \"berth\" is deprecated, berth is now shed: set kind = \"shed\"");
        }
        let levels = crate::harness::EFFORTS;
        if let Some(e) = c.limits.effort.as_deref().filter(|e| !levels.contains(e)) {
            bail!("limits.effort: \"{e}\" must be one of {levels:?}");
        }
        if c.notify
            .iter()
            .chain(c.fleet.values().flat_map(|f| &f.notify))
            .any(|n| n.webpush() && n.sub.as_deref().is_none_or(str::is_empty))
        {
            bail!("notify: a type = \"webpush\" notifier needs sub = \"mailto:you@example.com\"");
        }
        if let Some(g) = c.conductor.groups().iter().find(|g| !GROUPS.iter().any(|k| k.0 == **g)) {
            bail!(
                "conductor.allow: \"{g}\" is no group; one of {:?}",
                GROUPS.iter().map(|g| g.0).collect::<Vec<_>>()
            );
        }
        let ch = &c.chat;
        if let Some(h) = ch.harness.as_deref().filter(|h| !crate::harness::NAMES.contains(h)) {
            bail!("chat.harness: \"{h}\" must be one of {:?}", crate::harness::NAMES);
        }
        if let (Some(h), Some(e)) = (&ch.harness, &ch.effort) {
            if !crate::harness::efforts(h).contains(&e.as_str()) {
                bail!("chat.effort: harness {h} does not support effort \"{e}\"");
            }
        }
        if let Some(a) = ch
            .account
            .iter()
            .chain(&ch.accounts)
            .find(|a| !c.accounts.names.contains_key(*a))
        {
            bail!("chat: account '{a}' is not in [accounts]");
        }
        let mut c = c;
        for (name, f) in &mut c.fleet {
            let mut p = f
                .pipeline
                .take()
                .unwrap_or_else(|| crate::pipeline::Pipeline::builtin(&c.limits, f.role.contains_key("verify")));
            // Each stage's line is its `[[fleet.<n>.pipeline.stage]]` header's, in order.
            let head = format!("[[fleet.{name}.pipeline.stage]]");
            let lines = text
                .lines()
                .enumerate()
                .filter(|l| l.1.split_whitespace().collect::<String>() == head);
            for (s, (i, _)) in p.stage.iter_mut().zip(lines) {
                s.line = i + 1;
            }
            // A gate without its own rounds takes `[limits] update_rounds`.
            for g in p.stage.iter_mut().filter(|s| s.kind == "gate" && s.rounds.is_none()) {
                g.rounds = Some(c.limits.update_rounds);
            }
            f.form_categories
                .get_or_insert_with(|| c.limits.form_categories.clone());
            f.form_rounds.get_or_insert(c.limits.form_rounds);
            f.bad = p.validate(name, &f.role);
            f.pipeline = Some(p);
        }
        for (name, f) in &mut c.fleet {
            if let Some(r) = f.role.get("impl").filter(|_| !f.role.contains_key("conductor")) {
                let r = Role {
                    profile: Some("lane-conductor".into()),
                    default: true,
                    ..r.clone()
                };
                f.role.insert("conductor".into(), r);
            }
            if !f.serial_paths.is_empty() {
                if !f.serial.is_empty() {
                    bail!("fleet.{name}: serial_paths and [[fleet.{name}.serial]] together; keep one");
                }
                let name = f.serial_label.clone().unwrap_or_else(|| "db".into());
                let (paths, max) = (f.serial_paths.clone(), f.serial_max.unwrap_or(1));
                f.serial.push(Serial { name, paths, max });
            }
            if f.serial.iter().any(|g| !(1..=8).contains(&g.name.chars().count())) {
                bail!("fleet.{name}.serial: a name (serial_label) must be 1 to 8 characters");
            }
            if f.deny_commands.as_ref().is_some_and(Vec::is_empty) {
                eprintln!("herdsman: fleet.{name}.deny_commands is empty: the lane hook denies no commands");
            }
            if let Some(k) = f.color.as_deref().filter(|k| !COLORS.contains(k)) {
                bail!("fleet.{name}.color: \"{k}\" must be one of {COLORS:?}");
            }
            if f.notify.iter().any(|n| !n.webpush() && n.cmd.trim().is_empty()) {
                bail!("fleet.{name}.notify: cmd must not be empty");
            }
            for (role, r) in &f.role {
                if role == "orch" || role.is_empty() || !role.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                    bail!("fleet.{name}.role.{role}: a role name is letters, digits and `-`, not orch");
                }
                if !crate::harness::NAMES.contains(&r.harness.as_str()) {
                    bail!(
                        "fleet.{name}.role.{role}: harness must be one of {:?}",
                        crate::harness::NAMES
                    );
                }
                if let Some(a) = r.account_list().iter().find(|a| !c.accounts.names.contains_key(*a)) {
                    bail!("fleet.{name}.role.{role}: account '{a}' is not in [accounts]");
                }
                let bad = |e: &&String| {
                    let (h, a) = e.split_once(':').unwrap_or((e, ""));
                    !crate::harness::NAMES.contains(&h) || !a.is_empty() && !c.accounts.names.contains_key(a)
                };
                if let Some(e) = r.fallback.iter().find(bad) {
                    bail!("fleet.{name}.role.{role}: fallback '{e}' is not harness[:account] ([accounts])");
                }
                if let Some(e) = r.effort_or(&c.limits) {
                    let ok = crate::harness::efforts(&r.harness);
                    if !levels.contains(&e.as_str()) {
                        bail!("fleet.{name}.role.{role}: effort \"{e}\" must be one of {levels:?}");
                    }
                    if !ok.contains(&e.as_str()) {
                        bail!(
                            "fleet.{name}.role.{role}: harness {} does not support effort \"{e}\" (supported: {ok:?})",
                            r.harness
                        );
                    }
                }
            }
        }
        Ok(c)
    }

    /// Notifiers for `fleet`'s events: its own if any (plus the global ones with `notify_global`), else the global.
    pub fn notifiers(&self, fleet: &str) -> Vec<&Notify> {
        match self.fleet.get(fleet).filter(|f| !f.notify.is_empty()) {
            Some(f) if f.notify_global => f.notify.iter().chain(&self.notify).collect(),
            Some(f) => f.notify.iter().collect(),
            None => self.notify.iter().collect(),
        }
    }

    /// The first webpush notifier's VAPID `sub`: `None` means the page's alerts are saved but never sent (#87).
    pub fn webpush_sub(&self) -> Option<&str> {
        let all = self.notify.iter().chain(self.fleet.values().flat_map(|f| &f.notify));
        all.filter(|n| n.webpush()).find_map(|n| n.sub.as_deref())
    }

    /// #565: the shed runtime fleet `name`'s new lanes start on: its own `[fleet.<n>.runtime]`, else `[runtime]`'s.
    pub fn shed_for(&self, name: &str) -> Option<crate::shed::Shed> {
        let own = self.fleet.get(name).and_then(|f| f.runtime.as_ref());
        own.map_or_else(|| self.runtime.shed(), Runtime::shed)
    }

    /// #565: the shed server lanes are reached on: `[runtime]`'s, else the first fleet runtime's.
    pub fn shed(&self) -> Option<crate::shed::Shed> {
        let first = || self.fleet.values().find_map(|f| f.runtime.as_ref()?.shed());
        self.runtime.shed().or_else(first)
    }

    /// The fleet, refused `BAD_PIPELINE` while its pipeline breaks a rule (#173: that fleet stops, the others run).
    pub fn fleet(&self, name: &str) -> Result<&Fleet> {
        let f = self.fleet.get(name).ok_or_else(|| anyhow!("unknown fleet '{name}'"))?;
        match f.bad.as_slice() {
            [] => Ok(f),
            bad => {
                let rules: Vec<String> = bad.iter().map(|p| format!("\n  {:<12} {}", p.rule, p.msg)).collect();
                let n = bad.len();
                let why = format!(
                    "fleet {name}: {n} error(s) in [fleet.{name}.pipeline]{}",
                    rules.concat()
                );
                crate::gate::refused("BAD_PIPELINE", why)
            }
        }
    }
}

impl Fleet {
    /// #173: the fleet's pipeline (the built-in one unless fleet.toml has its own).
    pub fn flow(&self) -> &crate::pipeline::Pipeline {
        self.pipeline
            .as_ref()
            .expect("Config::parse resolves every fleet's pipeline")
    }
    /// The prompt family a role's lane gets: `rev` for a check role, `design`, else `impl` (a build).
    pub fn family(&self, role: &str) -> &'static str {
        match role {
            "design" => "design",
            "conductor" => "conductor",
            r if self.flow().is_check(r) => "rev",
            _ => "impl",
        }
    }
    /// #162: the card colour: `color`, else `sha256(name)[0] % 5` over `COLORS` (the horse coats' hash).
    pub fn color(&self, name: &str) -> &str {
        self.color.as_deref().unwrap_or_else(|| {
            let h = crate::devices::sha256(name.as_bytes());
            COLORS[usize::from_str_radix(&h[..2], 16).unwrap_or(0) % COLORS.len()]
        })
    }
    /// #216: the lane hook's deny list: `deny_commands`, else `[limits]`'s, plus `deny_commands_add`.
    pub fn deny(&self, limits: &Limits) -> Vec<Deny> {
        let mut d = self
            .deny_commands
            .clone()
            .unwrap_or_else(|| limits.deny_commands.clone());
        d.extend(self.deny_commands_add.iter().cloned());
        d
    }
    /// #350: the `[land]` table when this fleet lands fast-first.
    pub fn fast_first(&self) -> Option<&Land> {
        self.land.as_ref().filter(|l| l.mode == "fast-first")
    }
    /// #350: the checks `pr` lands on: the `fast` ones once `land::mark` marked it fast-first, else `required_checks`.
    pub fn checks(&self, pr: &serde_json::Value) -> &[String] {
        match self.fast_first() {
            Some(l) if pr["landing"] == "fast-first" => &l.fast,
            _ => &self.required_checks,
        }
    }
    pub fn serial_label(&self) -> &str {
        self.serial.first().map_or("serial", |g| &g.name)
    }
    pub fn role(&self, role: &str) -> Result<&Role> {
        self.role
            .get(self.role_key(role))
            .ok_or_else(|| anyhow!("role '{role}' not configured"))
    }
    /// #419: the configured role a lane runs as: a lesson lane is impl's unless `[fleet.<n>.role.lesson]` is set.
    pub fn role_key<'a>(&self, role: &'a str) -> &'a str {
        if role == "lesson" && !self.role.contains_key(role) {
            "impl"
        } else {
            role
        }
    }
    pub fn local_path(&self) -> PathBuf {
        expand(&self.local)
    }
    pub fn repo_name(&self) -> &str {
        self.repo.rsplit('/').next().unwrap_or(&self.repo)
    }
}

/// A value shaped like a secret: a known token prefix, a JWT, or a 32+ char letter-and-digit run between `.`/`/`/`-`/`_`
/// (so `UPPER_SNAKE` env-var names, paths and model ids pass).
pub fn secret_like(v: &str) -> bool {
    let words = v
        .split(|c: char| !(c.is_ascii_alphanumeric() || "-_+/=.~".contains(c)))
        .filter(|w| !w.is_empty());
    let b64 = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_=".contains(c));
    let long =
        |p: &str| p.len() >= 32 && p.chars().any(|c| c.is_ascii_digit()) && p.chars().any(|c| c.is_ascii_alphabetic());
    words.into_iter().any(|w| {
        let snake = w
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        crate::lanes::PREFIXES
            .iter()
            .any(|p| w.starts_with(p) && w.len() >= p.len() + 10)
            || (w.starts_with("eyJ") && w.split('.').count() == 3 && w.split('.').all(b64))
            || (!snake && w.split(['.', '/', '-', '_']).any(long))
    })
}

/// Every string value under `v`, with its dotted key.
fn strings(key: &str, v: &toml::Value) -> Vec<(String, String)> {
    let sub = |k: String, v| strings(&k, v);
    match v {
        toml::Value::String(s) => vec![(key.into(), s.clone())],
        toml::Value::Table(t) => t
            .iter()
            .flat_map(|(k, v)| sub(format!("{key}{}{k}", if key.is_empty() { "" } else { "." }), v))
            .collect(),
        toml::Value::Array(a) => a
            .iter()
            .enumerate()
            .flat_map(|(i, v)| sub(format!("{key}[{i}]"), v))
            .collect(),
        _ => vec![],
    }
}

/// `**` matches anything, `*` anything but `/`, `?` one char other than `/`.
pub fn glob(g: &[u8], s: &[u8]) -> bool {
    match g {
        [] => s.is_empty(),
        [b'*', b'*', rest @ ..] => (0..=s.len()).any(|i| glob(rest, &s[i..])),
        [b'*', rest @ ..] => (0..=s.len())
            .take_while(|&i| i == 0 || s[i - 1] != b'/')
            .any(|i| glob(rest, &s[i..])),
        [b'?', rest @ ..] => s.first().is_some_and(|&c| c != b'/') && glob(rest, &s[1..]),
        [c, rest @ ..] => s.first() == Some(c) && glob(rest, &s[1..]),
    }
}

pub fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(p),
    }
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

/// Set `key = "val"` in `[fleet.<fleet>.role.<role>]`, touching only the value bytes (or adding one line).
pub fn set_value(text: &str, fleet: &str, role: &str, key: &str, val: &str) -> Result<String> {
    let h = format!("[fleet.{fleet}.role.{role}]");
    set_in(text, &h, key, &format!("\"{val}\""))
}

/// Set `key = <lit>` (a TOML literal) in the table `header`, as `set_value` does.
pub fn set_in(text: &str, header: &str, key: &str, lit: &str) -> Result<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let start = lines.iter().position(|l| {
        let (mut q, n) = (false, l.len());
        let n = l.find(|c| (q ^= c == '"', c == '#' && !q).1).unwrap_or(n);
        l[..n].split_whitespace().collect::<String>() == header
    });
    let start = start.ok_or_else(|| anyhow!("no {header} table in fleet.toml; add it first"))?;
    let end = (start + 1..lines.len())
        .find(|&i| lines[i].trim_start().starts_with('['))
        .unwrap_or(lines.len());
    let mut out: Vec<String> = lines.iter().map(std::string::ToString::to_string).collect();
    let hit = (start + 1..end).find(|&i| {
        let t = lines[i].trim_start();
        t.strip_prefix(key).is_some_and(|r| r.trim_start().starts_with('='))
    });
    if let Some(i) = hit {
        let l = lines[i];
        let eq = l.find('=').unwrap() + 1;
        let v = eq + l[eq..].len() - l[eq..].trim_start().len();
        let end = if l[v..].starts_with('"') {
            l[v + 1..].find('"').map(|c| v + c + 2)
        } else if lit.starts_with('"') {
            None
        } else {
            Some(
                v + l[v..]
                    .find(|c: char| c.is_whitespace() || c == '#')
                    .unwrap_or(l.len() - v),
            )
        };
        let end = end.ok_or_else(|| anyhow!("{header} {key}: value must be a \"string\""))?;
        out[i] = format!("{}{lit}{}", &l[..v], &l[end..]);
    } else {
        let last = (start..end).rev().find(|&i| !lines[i].trim().is_empty()).unwrap();
        if !out[last].ends_with('\n') {
            out[last].push('\n');
        }
        out.insert(last + 1, format!("{key} = {lit}\n"));
    }
    Ok(out.concat())
}

pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let tmp = crate::devices::tmp_name(path);
    std::fs::write(&tmp, text)?;
    Ok(std::fs::rename(&tmp, path)?)
}
