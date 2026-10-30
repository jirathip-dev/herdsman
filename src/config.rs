//! fleet.toml: load, and in-place value edits that preserve every other byte.
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const ROLES: [&str; 4] = ["orch", "impl", "rev", "design"];

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub fleet: BTreeMap<String, Fleet>,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub notify: Vec<Notify>,
    #[serde(default)]
    pub accounts: Accounts,
    /// MCP server names no lane is given (`*` wildcards, case-insensitive).
    #[serde(default = "mcp_deny")]
    pub mcp_deny: Vec<String>,
    /// The models `herdsman serve` offers per role (its model dropdown).
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub serve: Serve,
}

/// `[serve]`: extra `Host` names (e.g. the `*.ts.net` name of `tailscale serve`) a POST may come from.
#[derive(Debug, Default, Deserialize)]
pub struct Serve {
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

/// `[accounts]`: account name -> env var holding its token, plus the settings below.
#[derive(Debug, Deserialize)]
pub struct Accounts {
    /// Env file (`VAR=value` lines) the tokens are read from.
    pub accounts_env: Option<String>,
    /// Usage-limit banner, `|`-separated substrings (default `access::CAP_PATTERN`).
    pub cap_pattern: Option<String>,
    #[serde(default = "cap_reset_minutes")]
    pub cap_reset_minutes: u64,
    #[serde(flatten)]
    pub names: BTreeMap<String, String>,
}

impl Default for Accounts {
    fn default() -> Self {
        Accounts { accounts_env: None, cap_pattern: None, cap_reset_minutes: cap_reset_minutes(), names: BTreeMap::new() }
    }
}

fn cap_reset_minutes() -> u64 {
    300
}
fn mcp_deny() -> Vec<String> {
    vec!["*prod*".into()]
}

#[derive(Debug, Deserialize)]
pub struct Fleet {
    pub repo: String,
    pub local: String,
    pub integration: String,
    /// Globs only one lane at a time may touch: `--serial` issues (SERIAL_BUSY, SERIAL_UNMARKED).
    #[serde(default)]
    pub serial_paths: Vec<String>,
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
pub struct Limits {
    #[serde(default = "three")]
    pub review_rounds: u32,
    #[serde(default = "thirty")]
    pub stall_minutes: u64,
    /// Host-load gate for `herdsman spawn` (HOST_BUSY).
    #[serde(default = "load_per_cpu")]
    pub load_per_cpu: f64,
    /// Pages swapped out per 2 s sample.
    #[serde(default = "swapout_rate")]
    pub swapout_rate: f64,
    /// Default reasoning effort for roles that set none.
    #[serde(default)]
    pub effort: Option<String>,
}

fn three() -> u32 {
    3
}
fn thirty() -> u64 {
    30
}
fn load_per_cpu() -> f64 {
    2.5
}
fn swapout_rate() -> f64 {
    2000.0
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            review_rounds: three(),
            stall_minutes: thirty(),
            load_per_cpu: load_per_cpu(),
            swapout_rate: swapout_rate(),
            effort: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Notify {
    pub cmd: String,
    #[serde(default)]
    pub on: Vec<String>,
    /// "json" sends the raw event; anything else one plain line.
    #[serde(default)]
    pub format: Option<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("invalid config {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Config> {
        let c: Config = toml::from_str(text)?;
        let raw: toml::Table = toml::from_str(text)?;
        if raw.get("limits").and_then(|l| l.get("max_agents")).is_some() {
            eprintln!("herdsman: [limits].max_agents is no longer used; ignoring it");
        }
        let levels = crate::harness::EFFORTS;
        if let Some(e) = c.limits.effort.as_deref().filter(|e| !levels.contains(e)) {
            bail!("limits.effort: \"{e}\" must be one of {levels:?}");
        }
        for (name, f) in &c.fleet {
            if f.notify.iter().any(|n| n.cmd.trim().is_empty()) {
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

    /// Notifiers for an event of `fleet`: its own when it has any (plus the global ones
    /// with `notify_global`), otherwise the global ones.
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

/// Set `key = "val"` in `[fleet.<fleet>.role.<role>]`, touching only the value
/// bytes (or inserting one line when the key is absent). Returns the new text.
pub fn set_value(text: &str, fleet: &str, role: &str, key: &str, val: &str) -> Result<String> {
    set_in(text, &format!("[fleet.{fleet}.role.{role}]"), key, &format!("\"{val}\""))
}

/// Set `key = <lit>` (a TOML literal) in the table `header`, as `set_value` does.
pub fn set_in(text: &str, header: &str, key: &str, lit: &str) -> Result<String> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let start = lines
        .iter()
        .position(|l| l.split_whitespace().collect::<String>() == header)
        .ok_or_else(|| anyhow!("no {header} table in fleet.toml; add it first"))?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .map_or(lines.len(), |i| start + 1 + i);
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
