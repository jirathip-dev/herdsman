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
    /// The models `drover serve` offers per role (its model dropdown).
    pub models: Vec<String>,
    pub serve: Serve,
    pub providers: Providers,
    /// #157: `[hosts.<name>]` lanes may run on (docs/hosts.md); none = this host only, as before.
    pub hosts: BTreeMap<String, HostCfg>,
    /// #303: `[[setup]]` logins a host card's [sign in] runs (`crate::setup`).
    pub setup: Vec<SetupCfg>,
    pub runtime: Option<Runtime>,
    /// #355: `[conductor]`: the conductor profile, plain markdown in a repo the owner picks (`crate::conductor`).
    #[serde(alias = "conductor")]
    pub orchestrator: OrchestratorCfg,
    /// #780: fleet.toml still names `[conductor]` or a `role.conductor` (doctor's deprecation line).
    #[serde(skip)]
    pub old_names: bool,
    /// #557: the Telegram bridge (#387) is removed; a leftover `[telegram]` is a warning (`Config::warnings`).
    pub telegram: Option<toml::Value>,
    /// #345: `[service] url`, where lanes on other hosts reach `drover service`'s API (docs/service.md).
    pub service: ServiceCfg,
    /// #404: `[chat]`, new threads' orchestrator defaults over the fleet's `role.orchestrator` (`orchestrator::defaults`).
    pub chat: Chat,
    /// #537: `[ci_runners]`, the owner's CI runner VMs (`crate::runners`, docs/hosts.md).
    pub ci_runners: Option<CiRunners>,
    /// #637: `[proxy]`, the token proxy remote ACP lanes' model calls go through (`crate::proxy`, docs/hosts.md).
    pub proxy: ProxyCfg,
    /// #894: `[self_update]`, the release branch the live install follows and the optional staging serve.
    pub self_update: SelfUpdate,
}

/// #894: `branch` (default `main`) `self-update` installs; `staging_serve` adds a read-only staging build on `staging_addr`.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct SelfUpdate {
    pub branch: String,
    pub staging_serve: bool,
    pub staging_addr: String,
}

impl Default for SelfUpdate {
    fn default() -> Self {
        SelfUpdate {
            branch: "main".into(),
            staging_serve: false,
            staging_addr: "127.0.0.1:7787".into(),
        }
    }
}

/// #637: the proxy's port (on this host, and on each `shed-acp` host through an ssh tunnel), where it forwards, and how long a lane key lives.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ProxyCfg {
    pub port: u16,
    pub upstream: String,
    pub key_hours: u64,
}

impl Default for ProxyCfg {
    fn default() -> Self {
        ProxyCfg {
            port: 41990,
            upstream: crate::providers::proxied().map_or("", |p| p.upstream).into(),
            key_hours: 12,
        }
    }
}

/// #537: the repo's runner VMs: their recipe (`provider`), spec, slots per VM, label, and the `max_vms` / `usd_day` cap; `vms` are the ones made by hand (`runners add|remove` change the set in the log); `local_*` the mini's runners.
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

/// #404: the harness, model, effort and account(s) a new thread's orchestrator starts on; each unset one is the role's.
#[derive(Debug, Default, Deserialize)]
pub struct Chat {
    pub harness: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub account: Option<String>,
    #[serde(default)]
    pub accounts: Vec<String>,
}

/// #355: the conductor's instructions file, skills folder and memory notes folder (`~/` expands); #386: `dir`, the root of the per-thread working folders (default `<state dir>/conductor`); #538: `allow`, its capability groups.
#[derive(Debug, Default, Deserialize)]
pub struct OrchestratorCfg {
    pub dir: Option<String>,
    pub instructions: Option<String>,
    pub skills: Option<String>,
    pub memory: Option<String>,
    pub allow: Option<Vec<String>>,
    /// #780: the default orchestrator's name: free text the owner calls it, shown on its labels, never read by code.
    pub name: Option<String>,
    /// #780: `[orchestrator.<id>]`, more orchestrators: each its own `name` and `fleets` (overlap allowed).
    #[serde(flatten)]
    pub more: BTreeMap<String, Orchestrator>,
}

/// #780: one `[orchestrator.<id>]`: what the owner calls it and the fleets its threads may be on.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Orchestrator {
    pub name: Option<String>,
    pub fleets: Vec<String>,
}

/// #538: each capability group an orchestrator may be allowed and its drover verbs (the hook's `group`).
pub const GROUPS: &[(&str, &str)] = &[
    ("design", "`spawn … design` or `… architect`"),
    ("clean", "`clean --yes`, `--orphans`, `--lane`"),
    ("hosts", "`hosts on|off|refresh`"),
    ("reclaim", "`reclaim`"),
    ("accounts", "`accounts reset`"),
    ("runners", "`runners …`"),
    ("pause", "`pause`, `resume`"),
    ("set", "`set` a role's harness, model or effort"),
];

impl OrchestratorCfg {
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
#[serde(try_from = "String")]
pub enum RuntimeKind {
    #[default]
    Shed,
    ShedAcp,
    Berth,
}

/// #721: the load error for the retired runtime, with its migration.
pub const NO_RUNTIME: &str = "no shed runtime: set [runtime] kind = \"shed-acp\" or \"shed\" (docs/runtimes.md)";

pub const RETIRED_RUNTIME: &str = "runtime \"herdr\" is retired (#721): set kind = \"shed-acp\" (each lane an ACP \
     session) or \"shed\" (each lane a shed terminal); docs/runtimes.md has the migration";

impl TryFrom<String> for RuntimeKind {
    type Error = String;
    fn try_from(k: String) -> std::result::Result<Self, String> {
        match k.as_str() {
            "shed" => Ok(Self::Shed),
            "shed-acp" => Ok(Self::ShedAcp),
            "berth" => Ok(Self::Berth),
            "herdr" => Err(RETIRED_RUNTIME.into()),
            _ => Err(format!("unknown runtime \"{k}\": \"shed-acp\" or \"shed\"")),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Runtime {
    pub kind: RuntimeKind,
    pub socket: Option<String>,
    pub acp: Option<String>,
    pub acp_policy: Option<String>,
}

impl Runtime {
    /// The shed runtime it names.
    pub fn shed(&self) -> crate::shed::Shed {
        let acp = self.kind == RuntimeKind::ShedAcp;
        crate::shed::Shed {
            wire: crate::shed::Wire::Socket(self.socket.as_deref().map_or_else(crate::shed::default_socket, expand)),
            acp: self
                .acp
                .clone()
                .or_else(|| acp.then(crate::providers::default_acp).flatten().map(Into::into)),
            policy: self
                .acp_policy
                .clone()
                .unwrap_or_else(|| if acp { "allow" } else { "ask" }.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SetupCfg {
    pub name: String,
    pub cmd: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct HostCfg {
    pub machine: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub root: Option<String>,
    #[serde(default)]
    pub roots: Vec<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// #254: a cloud host's recipe (`boat`, or `<config>/recipes/<name>.toml`), size, limits and idle stop (20 min).
    pub cloud: Option<String>,
    pub size: Option<String>,
    pub budget_usd_month: Option<f64>,
    pub max_hours_day: Option<f64>,
    pub idle_stop_minutes: Option<u64>,
    /// #256: its lanes' command rules: `deny_commands` replaces the fleet's list, `deny_commands_add` extends it, and `allow_commands` drops entries (unset: a cloud host allows `CLOUD_ALLOW`, a Mac nothing).
    pub deny_commands: Option<Vec<Deny>>,
    #[serde(default)]
    pub deny_commands_add: Vec<Deny>,
    pub allow_commands: Option<Vec<String>>,
    /// #637: `shed-acp`: the runtime its lanes start in (unset: the fleet's, as before).
    pub runtime: Option<RuntimeKind>,
    /// #984: lanes running a turn at once on this host, all fleets (unset: no cap).
    pub max_working: Option<usize>,
    /// #984: live lanes that need `sim` on this host at once (unset: no cap).
    pub max_simulators: Option<usize>,
    /// #909: heavy checks its lanes run at once, of every fleet (unset: 3).
    pub heavy_max: Option<usize>,
    /// #909: a smaller limit per heavy kind, e.g. `{ swiftlint = 1 }`.
    #[serde(default)]
    pub heavy_kind_max: BTreeMap<String, usize>,
    /// #909: minutes a heavy check waits for a slot before it runs anyway, logged `heavy-timeout` (unset: 30).
    pub heavy_wait_minutes: Option<u64>,
}

/// #256: a cloud host is a disposable machine: its lanes may start containers.
pub const CLOUD_ALLOW: [&str; 3] = ["docker", "docker compose", "supabase start"];

impl HostCfg {
    /// #630: its workspace roots in order, as written (none set: empty).
    pub fn roots(&self) -> Vec<String> {
        let mut v = self.roots.clone();
        v.extend(self.root.clone().filter(|_| v.is_empty()));
        v
    }
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

/// The key is read from `env_file` (#707: 0600, refused if looser; else the environment), never from fleet.toml.
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

/// `[serve]`: extra `Host` names (e.g. the `*.ts.net` name of `tailscale serve`) a POST may come from, and `approve_hint`, appended to the pairing panel's Mac line (`{code}` substituted).
#[derive(Debug, Default, Deserialize)]
pub struct Serve {
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    pub approve_hint: Option<String>,
    /// #98 `drover setup`: the board job's label (default `dev.drover.serve`), address, `tailscale serve` port.
    pub launchd_label: Option<String>,
    pub addr: Option<String>,
    pub tailscale_https_port: Option<u16>,
    /// #626: the Funnel listener, `POST /hooks/github` only (default `127.0.0.1:7778`; `""`: off).
    pub hooks_addr: Option<String>,
    /// #172: the Herd's theme, `<config dir>/themes/<name>/` or built-in (default `farm`).
    pub theme: Option<String>,
    /// #707: the 0600 file holding `GITHUB_WEBHOOK_SECRET` (unset: webhooks off).
    pub webhook_secret_file: Option<String>,
}

impl Serve {
    pub fn addr(&self) -> String {
        self.addr.clone().unwrap_or_else(|| "127.0.0.1:7777".into())
    }
    pub fn hooks_addr(&self) -> String {
        self.hooks_addr.clone().unwrap_or_else(|| "127.0.0.1:7778".into())
    }
}

/// `[accounts]`: the account names (#683: each value is unused, once its token's name in the store; a lane signs in with `account_env`'s env, else its folder, `Accounts::folder`), plus the settings below.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Accounts {
    /// #539: retired by #441, a warning (`Config::warnings`), never a load error; its tokens still back the store's missing ones, read-only, until the import and the line's removal.
    pub accounts_env: Option<String>,
    /// #683 (q846): the command (words; `{provider}` and `{name}` filled in) that prints an account's lane env as a JSON object, e.g. `cooee accounts env {provider} {name}`; unset: `Accounts::folder` as the provider's config folder.
    pub account_env: Option<String>,
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

impl Accounts {
    /// #683: account `name`'s own CLI login folder with no `account_env`: its `[accounts.usage]` one, else `~/.config/drover/accounts/<name>`.
    pub fn folder(&self, name: &str) -> PathBuf {
        let d = self.usage.get(name).cloned();
        expand(&d.unwrap_or_else(|| format!("~/.config/drover/accounts/{name}")))
    }
}

impl Default for Accounts {
    fn default() -> Self {
        Accounts {
            accounts_env: None,
            account_env: None,
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
    /// #150: a label the PR needs for its full CI (e.g. `merge-ready`), kept on it exactly while a PASS stands at its head; unset = none.
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
    /// `drover pause`: `next` offers nothing and impl/rev spawns refuse PAUSED.
    #[serde(default)]
    pub paused: bool,
    /// #638: this fleet's share of the spawn slots against the others' (default 1).
    pub weight: Option<usize>,
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
    /// #187: lanes' `DROVER_PORT` range, `lo-hi`; unset = `41000-41999`.
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
    /// #908: this fleet's `respawn_per_pass`; `Config::parse` fills in `[limits]`'s when absent.
    pub respawn_per_pass: Option<usize>,
    /// #216: replaces `[limits] deny_commands` (empty = off); `deny_commands_add` adds to whichever applies.
    pub deny_commands: Option<Vec<Deny>>,
    #[serde(default)]
    pub deny_commands_add: Vec<Deny>,
    /// #272: fast checks `merge` runs on the PR merged into current integration; unset = the cap check if `ci/cap.toml`.
    pub premerge: Option<Vec<Premerge>>,
    /// #754: the repo's own line-cap script `drover cap` reads `N / cap M` lines from; unset = the first `premerge` check naming a cap, else the built-in Rust count.
    pub cap_script: Option<String>,
    /// #350: `[fleet.<n>.land]`: how a passed PR lands; unset = full-first (today's rule).
    pub land: Option<Land>,
    /// #419: `weekly` (unset) has `tick` run `drover lesson` every 7 days; `off` opts out.
    pub lessons: Option<String>,
    /// #889: `weekly` (unset) has `tick` run `drover hotspots` every 7 days; `off` opts out.
    pub hotspots: Option<String>,
    /// #909: `[fleet.<n>.heavy]`: kind → command patterns (`*` any text) its lanes run in a host's heavy slots.
    #[serde(default)]
    pub heavy: BTreeMap<String, Vec<String>>,
    /// #417: globs of UI files: a PR touching one goes through the verify stage.
    #[serde(default)]
    pub ui_paths: Vec<String>,
    /// #541: the orchestrator thread (`t6`) lane asks go to first; unset = the repo's most recently active thread.
    pub ask_thread: Option<String>,
    /// #565: `[fleet.<n>.runtime]`, this fleet's new lanes' runtime over the global `[runtime]` (a pilot on one fleet).
    pub runtime: Option<Runtime>,
    /// #732: lets `rev` share `impl`'s provider; its line needs a `#` reason comment.
    pub rev_same_provider: Option<bool>,
}

/// #350: `mode = "fast-first"` lands a PASS once its `fast` checks are green (one touching `full_first_paths` waits for the full set); integration's own CI then runs the full suite, and `land::watch` reverts a red landing.
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
    /// #732: unset with `agent` set, the harness whose adapter that agent is.
    #[serde(default)]
    pub harness: String,
    /// #732: the ACP agent id its shed-acp lanes run (one `shed agents` lists) over the runtime's `acp`.
    pub agent: Option<String>,
    pub profile: Option<String>,
    pub model: Option<String>,
    /// Reasoning effort, mapped per harness (`harness::effort_ok`); `[limits].effort` when unset.
    pub effort: Option<String>,
    /// Lanes on `[accounts]`: the account (`[accounts]` name) to run on, or an ordered fallback list.
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
    /// #401: the default orchestrator `Config::parse` adds for a fleet without one: its impl role on `lane-orchestrator`.
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

/// `[limits]`: each key's doc line is its `LIMIT_DOCS` entry.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(default)]
pub struct Limits {
    pub review_rounds: u32,
    pub update_rounds: u32,
    pub stall_minutes: u64,
    pub silent_minutes: u64,
    pub ci_start_minutes: u64,
    pub load_per_cpu: f64,
    pub swapout_rate: f64,
    pub min_free_gb: f64,
    pub worktree_budget_gb: f64,
    pub build_budget_gb: f64,
    pub sweep_remote: bool,
    pub stray_roots: Vec<String>,
    pub min_free_mem_gb: f64,
    pub effort: Option<String>,
    pub ask_default_minutes: u64,
    pub ask_expire_hours: u64,
    pub idle_reap_minutes: u64,
    pub done_close_minutes: u64,
    pub idle_merged_minutes: u64,
    pub transcript_days: u64,
    pub deny_commands: Vec<Deny>,
    pub fast_secs: u64,
    pub slow_secs: u64,
    pub form_categories: Vec<String>,
    pub form_rounds: u32,
    pub overflow_after_minutes: u64,
    pub import_max_age_hours: u64,
    pub max_pushes_per_pass: usize,
    pub max_parallel_spawns: usize,
    pub spawn_pending_max_secs: u64,
    pub fallback_rest_minutes: u64,
    pub acp_start_minutes: u64,
    pub rotate_mb: u64,
    pub webhook_poll_minutes: u64,
    pub keychain_readers: usize,
    pub reap_keychain_readers: bool,
    pub respawn_max: u32,
    pub respawn_window_minutes: u64,
    pub respawn_per_pass: usize,
    pub runtime_down_failures: u64,
    pub runtime_down_minutes: u64,
    pub pass_max_secs: u64,
    pub ssh_connect_secs: u64,
    pub ssh_quick_secs: u64,
    pub ssh_call_secs: u64,
    pub access_probe_secs: u64,
    pub db_busy_secs: u64,
    pub db_retry_millis: u64,
    pub cloud_idle_stop_minutes: u64,
    pub notify_secs: u64,
    pub gate_check_minutes: u64,
    pub shed_read_millis: u64,
    pub proxy_connect_secs: u64,
    pub service_call_secs: u64,
    pub disk_probe_secs: u64,
    pub disk_sick_secs: u64,
    pub disk_trust_secs: u64,
    pub lane_read_secs: u64,
    pub poll_millis: u64,
    pub tick_beat_secs: u64,
    pub orchestrator_idle_minutes: u64,
    pub orchestrator_change_minutes: u64,
    pub token_refresh_minutes: u64,
    pub ask_waiting_minutes: u64,
    pub outbox_stale_minutes: u64,
    pub host_offline_minutes: u64,
    pub promise_passes: std::collections::BTreeMap<crate::promise::Kind, u64>,
    pub serve_shed_secs: u64,
    pub serve_remote_secs: u64,
    pub serve_host_secs: u64,
    pub serve_heartbeat_secs: u64,
    pub serve_handover_secs: u64,
    pub slo_api_p95_ms: u64,
    pub slo_api_minutes: u64,
    pub slo_idle_minutes: u64,
    pub slo_tick_minutes: u64,
}

/// #216: a `deny_commands` entry: a command word or command + subcommand prefix, bare or `{ cmd, why }`.
#[derive(Debug, Deserialize, serde::Serialize, Clone, PartialEq)]
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

/// `orchestrator_change_minutes`' default; the frozen board's `CHANGE` window follows it.
pub const ORCHESTRATOR_CHANGE_MINUTES: u64 = 30;

impl Default for Limits {
    fn default() -> Self {
        Limits {
            review_rounds: 3,
            update_rounds: 3,
            stall_minutes: 30,
            silent_minutes: 10,
            ci_start_minutes: 10,
            load_per_cpu: 2.5,
            swapout_rate: 2000.0,
            min_free_gb: 10.0,
            worktree_budget_gb: 25.0,
            build_budget_gb: 20.0,
            sweep_remote: true,
            stray_roots: vec!["/tmp".into(), "$TMPDIR".into()],
            min_free_mem_gb: 2.0,
            effort: None,
            ask_default_minutes: 30,
            ask_expire_hours: 24,
            idle_reap_minutes: 10,
            done_close_minutes: 5,
            idle_merged_minutes: 15,
            transcript_days: 30,
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
            acp_start_minutes: 3,
            rotate_mb: 20,
            webhook_poll_minutes: 10,
            keychain_readers: 3,
            reap_keychain_readers: false,
            respawn_max: 3,
            respawn_window_minutes: 60,
            respawn_per_pass: 2,
            runtime_down_failures: 5,
            runtime_down_minutes: 30,
            pass_max_secs: 120,
            ssh_connect_secs: 10,
            ssh_quick_secs: 5,
            ssh_call_secs: 20,
            access_probe_secs: 30,
            db_busy_secs: 60,
            db_retry_millis: 10,
            cloud_idle_stop_minutes: 20,
            notify_secs: 15,
            gate_check_minutes: 30,
            shed_read_millis: 300,
            proxy_connect_secs: 1,
            service_call_secs: 2,
            disk_probe_secs: 2,
            disk_sick_secs: 300,
            disk_trust_secs: 60,
            lane_read_secs: 5,
            poll_millis: 20,
            tick_beat_secs: 60,
            orchestrator_idle_minutes: 30,
            orchestrator_change_minutes: ORCHESTRATOR_CHANGE_MINUTES,
            token_refresh_minutes: 30,
            ask_waiting_minutes: 30,
            outbox_stale_minutes: 5,
            host_offline_minutes: 15,
            promise_passes: std::collections::BTreeMap::new(),
            serve_shed_secs: 5,
            serve_remote_secs: 10,
            serve_host_secs: 10,
            serve_heartbeat_secs: 15,
            serve_handover_secs: 300,
            slo_api_p95_ms: 1000,
            slo_api_minutes: 10,
            slo_idle_minutes: 20,
            slo_tick_minutes: 10,
        }
    }
}

/// #844: every `[limits]` key and what it sets, in docs/config.md's order; `drover config show` prints them.
pub const LIMIT_DOCS: &[(&str, &str)] = &[
    (
        "review_rounds",
        "review rounds before an issue goes needs-human (ROUND_LIMIT)",
    ),
    (
        "update_rounds",
        "PASSed update rounds before `spawn --update` refuses UPDATE_LIMIT",
    ),
    ("stall_minutes", "a working lane with no event this long is stalled"),
    (
        "silent_minutes",
        "an ACP lane's open turn with no session event this long is cancelled and nudged",
    ),
    (
        "ci_start_minutes",
        "a PR head with no checks this long after its push alerts `ci-missing`",
    ),
    ("load_per_cpu", "spawn refuses HOST_BUSY over this load per CPU"),
    (
        "swapout_rate",
        "pages swapped out per 2 s sample that count as swapping",
    ),
    ("min_free_gb", "spawn refuses DISK_LOW under this many GB free on `/`"),
    (
        "worktree_budget_gb",
        "over this many GB of worktrees drive frees build folders, then spawn refuses DISK_BUDGET",
    ),
    (
        "build_budget_gb",
        "over this many GB of lane build folders drive trims their unlocked profiles",
    ),
    (
        "sweep_remote",
        "the scratch sweep runs on every enabled remote host too",
    ),
    (
        "stray_roots",
        "where `drover clean` looks for stray build folders and temp dirs",
    ),
    (
        "min_free_mem_gb",
        "spawn refuses MEM_LOW under this many GB of free + inactive memory",
    ),
    ("effort", "default reasoning effort for roles that set none"),
    (
        "ask_default_minutes",
        "a routine ask unanswered this long takes its recommended option",
    ),
    (
        "ask_expire_hours",
        "an ask no live lane waits on expires after this many hours",
    ),
    (
        "idle_reap_minutes",
        "a lane done or idle this long loses the processes its agent started",
    ),
    (
        "done_close_minutes",
        "a done lane handed off this long closes its session",
    ),
    (
        "idle_merged_minutes",
        "a merged or closed issue's idle lane has its pane closed after this long",
    ),
    ("transcript_days", "a lane's agent keeps its transcripts this many days"),
    ("deny_commands", "commands the lane hook refuses (default: containers)"),
    (
        "fast_secs",
        "the fast step (`drive --fast`) runs every this many seconds",
    ),
    ("slow_secs", "the slow tick runs every this many seconds"),
    ("form_categories", "the finding tags a `FAIL (form-only)` may carry"),
    (
        "form_rounds",
        "form-only FAILs per issue that don't count as a review round",
    ),
    (
        "overflow_after_minutes",
        "every local host refused this long lets placement start a stopped cloud host",
    ),
    (
        "import_max_age_hours",
        "a remote host's asks older than this are never imported",
    ),
    (
        "max_pushes_per_pass",
        "push alerts `check` sends a pass before a summary",
    ),
    (
        "max_parallel_spawns",
        "concurrent detached spawn kicks (0: in the pass)",
    ),
    (
        "spawn_pending_max_secs",
        "a spawn marker or kick lock older than this is stale",
    ),
    (
        "fallback_rest_minutes",
        "how long an unavailable agent rests when its message names no reset",
    ),
    (
        "acp_start_minutes",
        "an ACP lane with no event back this long after its first turn is a failed start",
    ),
    (
        "rotate_mb",
        "the event log over this many MB moves old events to monthly archives",
    ),
    (
        "webhook_poll_minutes",
        "with GitHub webhooks on, the fast step re-reads PRs only this often",
    ),
    (
        "keychain_readers",
        "more Keychain readers up over 60 s alert `keychain-jammed`",
    ),
    ("reap_keychain_readers", "kill those stuck Keychain readers too"),
    (
        "respawn_max",
        "failed starts of an issue's role in the window before spawn refuses RESPAWN_LOOP",
    ),
    (
        "respawn_window_minutes",
        "the window `respawn_max` counts failed starts in",
    ),
    (
        "respawn_per_pass",
        "respawns a drive pass starts, each through admission (a fleet may set its own)",
    ),
    (
        "runtime_down_failures",
        "lanes on one host failing to start or ending without a push in the window before drive trips `runtime-down` there",
    ),
    (
        "runtime_down_minutes",
        "the window `runtime_down_failures` counts in",
    ),
    (
        "pass_max_secs",
        "a drive pass longer than this logs `pass-slow`; past 3 × it logs `pass-stuck` and frees its lock",
    ),
    ("ssh_connect_secs", "every host ssh's `-oConnectTimeout`"),
    (
        "ssh_quick_secs",
        "a maintenance ssh's connect timeout and each pass's `ssh <host> true` probe",
    ),
    ("ssh_call_secs", "a remote call in a pass is killed after this long"),
    (
        "access_probe_secs",
        "an account probe (`account_env`) is killed after this long",
    ),
    ("db_busy_secs", "the SQLite busy timeout"),
    (
        "db_retry_millis",
        "the pause between a new database's WAL switch retries",
    ),
    (
        "cloud_idle_stop_minutes",
        "a cloud host's `idle_stop_minutes` when it sets none",
    ),
    (
        "notify_secs",
        "a notifier command or web push send is killed after this long",
    ),
    (
        "gate_check_minutes",
        "a premerge or gate check (a build or test run) is killed after this long",
    ),
    (
        "shed_read_millis",
        "how long drive's intake waits for shed's state changes",
    ),
    ("proxy_connect_secs", "the token proxy's liveness connect timeout"),
    (
        "service_call_secs",
        "a call to the running service's socket times out after this long",
    ),
    (
        "disk_probe_secs",
        "a folder listing slower than this marks its disk as not answering",
    ),
    (
        "disk_sick_secs",
        "a disk that let a call run out of time stays hung this long",
    ),
    (
        "disk_trust_secs",
        "a folder that answered is trusted this long before it is probed again",
    ),
    (
        "lane_read_secs",
        "a lane worktree read slower than this skips the lane for the pass (`root-hung`)",
    ),
    (
        "poll_millis",
        "the pause between polls of a child process or disk probe",
    ),
    ("tick_beat_secs", "a running tick writes `tick.beat` this often"),
    (
        "orchestrator_idle_minutes",
        "an orchestrator whose thread is quiet this long closes on the next `check`",
    ),
    (
        "orchestrator_change_minutes",
        "an orchestrator's answer may not change one this long after it",
    ),
    (
        "token_refresh_minutes",
        "`check` pushes a fresh runner registration token to every VM this often",
    ),
    (
        "ask_waiting_minutes",
        "an owner ask open this long alerts `ask-waiting`",
    ),
    (
        "outbox_stale_minutes",
        "a remote lane's outbox line undelivered this long alerts `outbox-stale`",
    ),
    (
        "host_offline_minutes",
        "a remote host unreachable this long has its open lanes restarted on the main host",
    ),
    (
        "promise_passes",
        "drive passes a promise of each kind may stay open before `promise-missed` (default 2)",
    ),
    (
        "serve_shed_secs",
        "serve re-reads shed's lanes in the background this often",
    ),
    (
        "serve_remote_secs",
        "serve re-reads the remote hosts (ssh) in the background this often",
    ),
    (
        "serve_host_secs",
        "serve re-samples this host's load in the background this often",
    ),
    (
        "serve_heartbeat_secs",
        "`/live` pings this often; a stream whose last 2 pings are still unsent is dropped",
    ),
    (
        "serve_handover_secs",
        "`self-update` waits this long for the running serve to hand over before restarting it",
    ),
    (
        "slo_api_p95_ms",
        "`/api/v1` p95 over this many ms for `slo_api_minutes` alerts `slo-api` once",
    ),
    (
        "slo_api_minutes",
        "how long `/api/v1` p95 must stay over `slo_api_p95_ms` before `slo-api`",
    ),
    (
        "slo_idle_minutes",
        "no lane working this long while the queue has spawnable work breaches `slo-idle`",
    ),
    (
        "slo_tick_minutes",
        "a tick running longer than this breaches `slo-tick`; the next fast step stops it",
    ),
];

static ACTIVE: std::sync::RwLock<Option<std::sync::Arc<Limits>>> = std::sync::RwLock::new(None);

/// #844: the last loaded fleet.toml's `[limits]` (defaults before one loads), for code that holds no `Config`.
pub fn limits() -> std::sync::Arc<Limits> {
    static DEFAULT: std::sync::OnceLock<std::sync::Arc<Limits>> = std::sync::OnceLock::new();
    let active = ACTIVE.read().ok().and_then(|a| a.clone());
    active.unwrap_or_else(|| DEFAULT.get_or_init(Default::default).clone())
}

/// #844: `drover config show`: each `[limits]` key, its value and its doc line.
pub fn show(cfg: &Config, out: &mut dyn std::io::Write) -> Result<()> {
    let vals = toml::Table::try_from(&cfg.limits)?;
    for (k, doc) in LIMIT_DOCS {
        let v = vals.get(*k).map_or("unset".into(), toml::Value::to_string);
        writeln!(out, "{k} = {v}  # {doc}")?;
    }
    Ok(())
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

/// A webpush notifier's events when `on` is unset: the alert set plus the asks (#40) and needs-human (#130).
pub const WEBPUSH_ON: &[&str] = &[
    "slo-api",
    "stuck",
    "config-invalid",
    "thread-msg",
    "blocked",
    "stall",
    "lane-dead",
    "account-signed-out",
    "lane-failed-start",
    "rev-stale-replaced",
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
    "spawn-failing",
    "host-busy",
    "cap-near",
    "orphan-issue",
    "config-warning",
    "keychain-locked",
    "keychain-jammed",
    "self-update-failed",
    "host-sync-failed",
    "clean-failed",
    "runners-low",
    "runners-ok",
    "runtime-down",
    "heavy-timeout",
];

impl Notify {
    pub fn webpush(&self) -> bool {
        self.kind.as_deref() == Some("webpush")
    }
    /// #350: every landing pause and revert (or failed revert), and #345's stalled service, goes to each notifier, whatever its `on` says; #363: so does a board report, to each notifier but the phones'.
    pub fn wants(&self, kind: &str) -> bool {
        self.on.iter().any(|k| k == kind)
            || (self.on.is_empty() && self.webpush() && WEBPUSH_ON.contains(&kind))
            || matches!(kind, "land-paused" | "reverted" | "revert-failed" | "service-stalled")
            || kind == "report" && !self.webpush()
    }
}

impl Config {
    /// #539: each retired key still set, as `(key, what to do)`: doctor's `!` rows, the board's line, `check`'s `config-warning`; never a load error.
    pub fn warnings(&self) -> Vec<(&'static str, String)> {
        let run = |_| "retired (#441): drop the line (#683: accounts sign in with their own login)".to_string();
        let f = self.accounts.accounts_env.as_deref();
        let tg = "removed (#557): drover has no Telegram bridge now; drop the table".to_string();
        let tg = self.telegram.as_ref().map(|_| ("[telegram]", tg));
        let old =
            "renamed (#780): call it [orchestrator] and role.orchestrator; the old names are read for one release";
        let old = self.old_names.then(|| ("[conductor]", old.to_string()));
        f.map(|f| ("[accounts].accounts_env", run(f)))
            .into_iter()
            .chain(tg)
            .chain(old)
            .collect()
    }

    /// #539: secret `name` from the retired `accounts_env` file, for one the store lacks; None without it.
    pub fn legacy_secret(&self, name: &str) -> Option<String> {
        let text = std::fs::read_to_string(expand(self.accounts.accounts_env.as_deref()?)).ok()?;
        crate::usage::env_value(&text, name).filter(|v| !v.is_empty())
    }

    pub fn load(path: &Path) -> Result<Config> {
        Self::load_text(path).map(|c| c.0)
    }

    fn load_text(path: &Path) -> Result<(Config, String)> {
        let text = std::fs::read_to_string(path).with_context(|| format!("cannot read config {}", path.display()))?;
        let e = match Self::parse(&text) {
            Ok(c) => {
                if let Ok(mut a) = ACTIVE.write() {
                    *a = Some(std::sync::Arc::new(c.limits.clone()));
                }
                return Ok((c, text));
            }
            Err(e) => e,
        };
        let rf = e.downcast_ref::<crate::gate::Refusal>();
        let (msg, at) = match (e.downcast_ref::<toml::de::Error>(), rf) {
            (Some(t), _) => (t.message().replace('\n', "; "), t.span().map(|s| s.start)),
            (_, Some(r)) => (r.msg.clone(), None),
            _ => (format!("{e:#}"), None),
        };
        let quoted = msg.split(['\'', '"']).nth(1).filter(|q| !q.is_empty());
        let at = at.or_else(|| text.find(&format!("\"{}\"", quoted?)));
        let file = path.file_name().map_or("fleet.toml".into(), |f| f.to_string_lossy());
        let line = at.map_or(String::new(), |i| format!(":{}", text[..i].matches('\n').count() + 1));
        let line = format!("config invalid: {msg} ({file}{line})");
        match rf {
            Some(r) => crate::gate::refused(r.code, line),
            None => bail!(line),
        }
    }

    /// #709: the config, else (it fails to load) the error and the last good one, which each load keeps beside the state log (`fleet.good.toml`): serve reads from it and `check` alerts through its notifiers.
    pub fn loaded(ctx: &crate::Ctx) -> std::result::Result<Config, (anyhow::Error, Option<Box<Config>>)> {
        let good = ctx.state.with_file_name("fleet.good.toml");
        match Self::load_text(&ctx.config) {
            Ok((c, text)) => {
                if std::fs::read_to_string(&good).ok().as_deref() != Some(&text) {
                    let _ = good.parent().map(std::fs::create_dir_all);
                    let _ = std::fs::write(&good, text);
                }
                Ok(c)
            }
            Err(e) => Err((e, Self::load(&good).ok().map(Box::new))),
        }
    }

    /// #709: serve's reads: the config, else the last good one; Err only with none kept.
    pub fn kept(ctx: &crate::Ctx) -> Result<Config> {
        Self::loaded(ctx).or_else(|(e, good)| good.map(|g| *g).ok_or(e))
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
            eprintln!("drover: [limits].max_agents is no longer used; ignoring it");
        }
        if c.runtime.as_ref().is_some_and(|r| r.kind == RuntimeKind::Berth) {
            eprintln!("drover: [runtime] kind = \"berth\" is deprecated, berth is now shed: set kind = \"shed\"");
        }
        let levels = crate::providers::EFFORTS;
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
        if let Some((id, f)) = (c.orchestrator.more.iter())
            .find_map(|(id, o)| Some((id, o.fleets.iter().find(|f| *f != "all" && !c.fleet.contains_key(*f))?)))
        {
            bail!("orchestrator.{id}.fleets: \"{f}\" is no fleet");
        }
        if let Some(g) = c
            .orchestrator
            .groups()
            .iter()
            .find(|g| !GROUPS.iter().any(|k| k.0 == **g))
        {
            bail!(
                "orchestrator.allow: \"{g}\" is no group; one of {:?}",
                GROUPS.iter().map(|g| g.0).collect::<Vec<_>>()
            );
        }
        let ch = &c.chat;
        if let Some(h) = ch
            .harness
            .as_deref()
            .filter(|h| !crate::providers::lane_names().contains(h))
        {
            bail!(
                "chat.harness: \"{h}\" must be one of {:?}",
                crate::providers::lane_names()
            );
        }
        if let (Some(h), Some(e)) = (&ch.harness, &ch.effort) {
            if !crate::providers::get(h).efforts().contains(&e.as_str()) {
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
        c.old_names = raw.contains_key("conductor");
        for (name, f) in &mut c.fleet {
            if let Some(r) = f.role.remove("conductor") {
                f.role.entry("orchestrator".into()).or_insert(r);
                c.old_names = true;
            }
            let mut p = f
                .pipeline
                .take()
                .unwrap_or_else(|| crate::pipeline::Pipeline::builtin(&c.limits, f.role.contains_key("verify")));
            let head = format!("[[fleet.{name}.pipeline.stage]]");
            let lines = text
                .lines()
                .enumerate()
                .filter(|l| l.1.split_whitespace().collect::<String>() == head);
            for (s, (i, _)) in p.stage.iter_mut().zip(lines) {
                s.line = i + 1;
            }
            for g in p.stage.iter_mut().filter(|s| s.kind == "gate" && s.rounds.is_none()) {
                g.rounds = Some(c.limits.update_rounds);
            }
            f.form_categories
                .get_or_insert_with(|| c.limits.form_categories.clone());
            f.form_rounds.get_or_insert(c.limits.form_rounds);
            f.respawn_per_pass.get_or_insert(c.limits.respawn_per_pass);
            f.bad = p.validate(name, &f.role);
            f.pipeline = Some(p);
        }
        for (name, f) in &mut c.fleet {
            if let Some(r) = f.role.get("impl").filter(|_| !f.role.contains_key("orchestrator")) {
                let r = Role {
                    profile: Some("lane-orchestrator".into()),
                    default: true,
                    ..r.clone()
                };
                f.role.insert("orchestrator".into(), r);
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
                eprintln!("drover: fleet.{name}.deny_commands is empty: the lane hook denies no commands");
            }
            if let Some(k) = f.color.as_deref().filter(|k| !COLORS.contains(k)) {
                bail!("fleet.{name}.color: \"{k}\" must be one of {COLORS:?}");
            }
            if f.notify.iter().any(|n| !n.webpush() && n.cmd.trim().is_empty()) {
                bail!("fleet.{name}.notify: cmd must not be empty");
            }
            for (role, r) in &mut f.role {
                let id = r.agent.as_deref();
                match (id.map(|a| (a, crate::providers::of_agent(a))), r.harness.is_empty()) {
                    (Some((a, None)), _) => bail!("fleet.{name}.role.{role}: agent {a}: no harness adapter guards it"),
                    (Some((_, Some(h))), true) => r.harness = h.into(),
                    (Some((a, Some(h))), false) if h != r.harness => {
                        bail!(
                            "fleet.{name}.role.{role}: agent {a} runs harness {h}, not {}",
                            r.harness
                        )
                    }
                    _ => {}
                }
            }
            for (role, r) in &f.role {
                if role == "orch" || role.is_empty() || !role.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                    bail!("fleet.{name}.role.{role}: a role name is letters, digits and `-`, not orch");
                }
                if !crate::providers::known().any(|h| h == r.harness) {
                    bail!(
                        "fleet.{name}.role.{role}: harness must be one of {:?}",
                        crate::providers::lane_names()
                    );
                }
                if let Some(a) = r.account_list().iter().find(|a| !c.accounts.names.contains_key(*a)) {
                    bail!("fleet.{name}.role.{role}: account '{a}' is not in [accounts]");
                }
                let bad = |e: &&String| {
                    let (h, a) = e.split_once(':').unwrap_or((e, ""));
                    !crate::providers::lane_names().contains(&h) || !a.is_empty() && !c.accounts.names.contains_key(a)
                };
                if let Some(e) = r.fallback.iter().find(bad) {
                    bail!("fleet.{name}.role.{role}: fallback '{e}' is not harness[:account] ([accounts])");
                }
                if let Some(e) = r.effort_or(&c.limits) {
                    let ok = crate::providers::get(&r.harness).efforts();
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
        for name in c.fleet.keys() {
            c.same_provider(name, text)?;
        }
        Ok(c)
    }

    fn same_provider(&self, name: &str, text: &str) -> Result<()> {
        let f = &self.fleet[name];
        let (Some(i), Some(v)) = (f.role.get("impl"), f.role.get("rev")) else {
            return Ok(());
        };
        let run = self.shed_for(name).and_then(|b| b.acp);
        let of = |r: &Role| r.agent.clone().or_else(|| run.clone());
        let p = (of(i), of(v));
        let p = (
            p.0.as_deref().and_then(crate::providers::vendor_of_agent),
            p.1.as_deref().and_then(crate::providers::vendor_of_agent),
        );
        if i.agent.is_none() && v.agent.is_none() || p.0.is_none() || p.0 != p.1 {
            return Ok(());
        }
        let mut head = "";
        let ok = text.lines().any(|l| {
            let t = l.trim();
            if t.starts_with('[') {
                head = t;
            }
            head == format!("[fleet.{name}]") && t.starts_with("rev_same_provider") && t.contains('#')
        });
        if f.rev_same_provider == Some(true) && ok {
            return Ok(());
        }
        let why = format!(
            "fleet.{name}: rev runs on impl's provider {}; give rev another agent, or set rev_same_provider = true # <why>",
            p.0.unwrap_or_default()
        );
        crate::gate::refused("REVIEW_SAME_PROVIDER", why)
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
        own.or(self.runtime.as_ref()).map(Runtime::shed)
    }

    pub fn shed(&self) -> Option<crate::shed::Shed> {
        let first = || self.fleet.values().find_map(|f| f.runtime.as_ref());
        let host = || self.hosts.keys().find_map(|h| self.host_acp(h));
        self.runtime.as_ref().or_else(first).map(Runtime::shed).or_else(host)
    }

    /// #637: host `name`'s own `runtime = "shed-acp"`: the shed-acp runtime (shed's defaults), else None.
    pub fn host_acp(&self, name: &str) -> Option<crate::shed::Shed> {
        self.hosts
            .get(name)
            .filter(|h| h.runtime == Some(RuntimeKind::ShedAcp))?;
        Some(
            Runtime {
                kind: RuntimeKind::ShedAcp,
                ..Runtime::default()
            }
            .shed(),
        )
    }

    /// #637: the runtime fleet `fleet`'s new lanes start in on host `host`: the host's own, else the fleet's.
    pub fn shed_on(&self, fleet: &str, host: Option<&str>) -> Option<crate::shed::Shed> {
        host.and_then(|h| self.host_acp(h)).or_else(|| self.shed_for(fleet))
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
            "architect" => "architect",
            r if crate::orchestrator::is_role(r) => "orchestrator",
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

/// A value shaped like a secret: a known token prefix, a JWT, or a 32+ char letter-and-digit run between `.`/`/`/`-`/`_` (so `UPPER_SNAKE` env-var names, paths and model ids pass).
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
        let mut q = false;
        let end = l[v..]
            .find(|c| (q ^= c == '"', c == '#' && !q).1)
            .map_or(l.trim_end().len().max(v), |c| v + c);
        out[i] = format!("{}{lit}{}", &l[..v], &l[v + l[v..end].trim_end().len()..]);
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
