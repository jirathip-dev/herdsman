//! fleet.toml: load, and in-place value edits that preserve every other byte.
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const ROLES: [&str; 4] = ["orch", "impl", "rev", "design"];

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
}

/// `[providers]`: API providers whose balance the Host tab shows (docs/providers.md).
#[derive(Debug, Default, Deserialize)]
pub struct Providers {
    pub deepseek: Option<Provider>,
}

/// The key is read from `env_file` (else `[accounts].accounts_env`, else the environment), never from fleet.toml.
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
}

/// `[accounts]`: account name -> env var holding its token, plus the settings below.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Accounts {
    /// Env file (`VAR=value` lines) the tokens are read from.
    pub accounts_env: Option<String>,
    /// Usage-limit banner, `|`-separated substrings (default `access::CAP_PATTERN`).
    pub cap_pattern: Option<String>,
    pub cap_reset_minutes: u64,
    /// `[accounts.usage]`: account name -> the `CLAUDE_CONFIG_DIR` of its own `claude` login, read only for the usage meter.
    pub usage: BTreeMap<String, String>,
    #[serde(flatten)]
    pub names: BTreeMap<String, String>,
}

impl Default for Accounts {
    fn default() -> Self {
        Accounts { accounts_env: None, cap_pattern: None, cap_reset_minutes: 300, usage: BTreeMap::new(), names: BTreeMap::new() }
    }
}

#[derive(Debug, Deserialize)]
pub struct Fleet {
    pub repo: String,
    pub local: String,
    pub integration: String,
    /// Globs only one lane at a time may touch: `--serial` issues (SERIAL_BUSY, SERIAL_UNMARKED).
    #[serde(default)]
    pub serial_paths: Vec<String>,
    /// The only checks merge, `check` and `wait` look at (latest run of each); empty = all.
    #[serde(default)]
    pub required_checks: Vec<String>,
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
    /// MCP servers (by `.mcp.json` name) the lane gets; none by default.
    #[serde(default)]
    pub mcp: Vec<String>,
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
    pub stall_minutes: u64,
    /// Host-load gate for `herdsman spawn` (HOST_BUSY).
    pub load_per_cpu: f64,
    /// Pages swapped out per 2 s sample.
    pub swapout_rate: f64,
    /// Default reasoning effort for roles that set none.
    pub effort: Option<String>,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { review_rounds: 3, stall_minutes: 30, load_per_cpu: 2.5, swapout_rate: 2000.0, effort: None }
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

/// A webpush notifier's events when `on` is unset: the Telegram set plus the asks (#40) and a stuck orch (#46).
pub const WEBPUSH_ON: [&str; 8] = ["blocked", "stall", "lane-dead", "ready-unmerged", "merged", "needs-decision", "orch-stuck", "ask-waiting"];

impl Notify {
    pub fn webpush(&self) -> bool {
        self.kind.as_deref() == Some("webpush")
    }
    pub fn wants(&self, kind: &str) -> bool {
        self.on.iter().any(|k| k == kind) || (self.on.is_empty() && self.webpush() && WEBPUSH_ON.contains(&kind))
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).with_context(|| format!("cannot read config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("invalid config {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Config> {
        let c: Config = toml::from_str(text)?;
        let raw: toml::Table = toml::from_str(text)?;
        if let Some((k, _)) = strings("", &toml::Value::Table(raw.clone())).into_iter().find(|(_, v)| secret_like(v)) {
            return crate::gate::refused("SECRET_IN_CONFIG", format!("{k} looks like a secret; keep keys in an env file (docs/providers.md)"));
        }
        if raw.get("limits").and_then(|l| l.get("max_agents")).is_some() {
            eprintln!("herdsman: [limits].max_agents is no longer used; ignoring it");
        }
        let levels = crate::harness::EFFORTS;
        if let Some(e) = c.limits.effort.as_deref().filter(|e| !levels.contains(e)) {
            bail!("limits.effort: \"{e}\" must be one of {levels:?}");
        }
        if c.notify.iter().chain(c.fleet.values().flat_map(|f| &f.notify)).any(|n| n.webpush() && n.sub.as_deref().is_none_or(str::is_empty)) {
            bail!("notify: a type = \"webpush\" notifier needs sub = \"mailto:you@example.com\"");
        }
        for (name, f) in &c.fleet {
            if f.notify.iter().any(|n| !n.webpush() && n.cmd.trim().is_empty()) {
                bail!("fleet.{name}.notify: cmd must not be empty");
            }
            for (role, r) in &f.role {
                if !ROLES.contains(&role.as_str()) {
                    bail!("fleet.{name}.role.{role}: role must be one of {ROLES:?}");
                }
                if !crate::harness::NAMES.contains(&r.harness.as_str()) {
                    bail!("fleet.{name}.role.{role}: harness must be one of {:?}", crate::harness::NAMES);
                }
                if let Some(a) = r.account_list().iter().find(|a| !c.accounts.names.contains_key(*a)) {
                    bail!("fleet.{name}.role.{role}: account '{a}' is not in [accounts]");
                }
                if let Some(e) = r.effort_or(&c.limits) {
                    let ok = crate::harness::efforts(&r.harness);
                    if !levels.contains(&e.as_str()) {
                        bail!("fleet.{name}.role.{role}: effort \"{e}\" must be one of {levels:?}");
                    }
                    if !ok.contains(&e.as_str()) {
                        bail!("fleet.{name}.role.{role}: harness {} does not support effort \"{e}\" (supported: {ok:?})", r.harness);
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

    pub fn fleet(&self, name: &str) -> Result<&Fleet> {
        self.fleet.get(name).ok_or_else(|| anyhow!("unknown fleet '{name}'"))
    }
}

impl Fleet {
    pub fn role(&self, role: &str) -> Result<&Role> {
        self.role.get(role).ok_or_else(|| anyhow!("role '{role}' not configured"))
    }
    pub fn local_path(&self) -> PathBuf {
        expand(&self.local)
    }
    pub fn repo_name(&self) -> &str {
        self.repo.rsplit('/').next().unwrap_or(&self.repo)
    }
    /// `path` matches a `serial_paths` glob.
    pub fn serial_path(&self, path: &str) -> bool {
        self.serial_paths.iter().any(|g| glob(g.as_bytes(), path.as_bytes()))
    }
}

/// A value shaped like a secret: a known token prefix, a JWT, or a 32+ char letter-and-digit run between `.`/`/`/`-`/`_`
/// (so `UPPER_SNAKE` env-var names, paths and model ids pass).
pub fn secret_like(v: &str) -> bool {
    let words = v.split(|c: char| !(c.is_ascii_alphanumeric() || "-_+/=.~".contains(c))).filter(|w| !w.is_empty());
    let b64 = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_=".contains(c));
    let long = |p: &str| p.len() >= 32 && p.chars().any(|c| c.is_ascii_digit()) && p.chars().any(|c| c.is_ascii_alphabetic());
    words.into_iter().any(|w| {
        let snake = w.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        crate::lanes::PREFIXES.iter().any(|p| w.starts_with(p) && w.len() >= p.len() + 10)
            || (w.starts_with("eyJ") && w.split('.').count() == 3 && w.split('.').all(b64))
            || (!snake && w.split(['.', '/', '-', '_']).any(long))
    })
}

/// Every string value under `v`, with its dotted key.
fn strings(key: &str, v: &toml::Value) -> Vec<(String, String)> {
    let sub = |k: String, v| strings(&k, v);
    match v {
        toml::Value::String(s) => vec![(key.into(), s.clone())],
        toml::Value::Table(t) => t.iter().flat_map(|(k, v)| sub(format!("{key}{}{k}", if key.is_empty() { "" } else { "." }), v)).collect(),
        toml::Value::Array(a) => a.iter().enumerate().flat_map(|(i, v)| sub(format!("{key}[{i}]"), v)).collect(),
        _ => vec![],
    }
}

/// `**` matches anything, `*` anything but `/`, `?` one char other than `/`.
pub fn glob(g: &[u8], s: &[u8]) -> bool {
    match g {
        [] => s.is_empty(),
        [b'*', b'*', rest @ ..] => (0..=s.len()).any(|i| glob(rest, &s[i..])),
        [b'*', rest @ ..] => (0..=s.len()).take_while(|&i| i == 0 || s[i - 1] != b'/').any(|i| glob(rest, &s[i..])),
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
    set_in(text, &format!("[fleet.{fleet}.role.{role}]"), key, &format!("\"{val}\""))
}

/// Set `key = <lit>` (a TOML literal) in the table `header`, as `set_value` does.
pub fn set_in(text: &str, header: &str, key: &str, lit: &str) -> Result<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let start = lines.iter().position(|l| l.split_whitespace().collect::<String>() == header);
    let start = start.ok_or_else(|| anyhow!("no {header} table in fleet.toml; add it first"))?;
    let end = lines[start + 1..].iter().position(|l| l.trim_start().starts_with('[')).map_or(lines.len(), |i| start + 1 + i);
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let hit = (start + 1..end).find(|&i| {
        let t = lines[i].trim_start();
        t.strip_prefix(key).is_some_and(|r| r.trim_start().starts_with('='))
    });
    match hit {
        Some(i) => {
            let l = lines[i];
            let eq = l.find('=').unwrap() + 1;
            let v = eq + l[eq..].len() - l[eq..].trim_start().len();
            let end = if l[v..].starts_with('"') {
                l[v + 1..].find('"').map(|c| v + c + 2)
            } else if lit.starts_with('"') {
                None
            } else {
                Some(v + l[v..].find(|c: char| c.is_whitespace() || c == '#').unwrap_or(l.len() - v))
            };
            let Some(end) = end else {
                bail!("{header} {key}: value must be a \"string\"");
            };
            out[i] = format!("{}{lit}{}", &l[..v], &l[end..]);
        }
        None => {
            let last = (start..end).rev().find(|&i| !lines[i].trim().is_empty()).unwrap();
            if !out[last].ends_with('\n') {
                out[last].push('\n');
            }
            out.insert(last + 1, format!("{key} = {lit}\n"));
        }
    }
    Ok(out.concat())
}

pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
