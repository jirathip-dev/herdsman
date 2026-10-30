//! fleet.toml: load, and in-place value edits that preserve every other byte.
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const ROLES: [&str; 3] = ["orch", "impl", "rev"];

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub fleet: BTreeMap<String, Fleet>,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub notify: Vec<Notify>,
}

#[derive(Debug, Deserialize)]
pub struct Fleet {
    pub repo: String,
    pub local: String,
    pub integration: String,
    #[serde(default)]
    pub role: BTreeMap<String, Role>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Role {
    pub harness: String,
    pub profile: Option<String>,
    pub model: Option<String>,
}

impl Role {
    pub fn profile_or(&self, role: &str) -> String {
        self.profile.clone().unwrap_or_else(|| format!("fleet-{role}"))
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
        for (name, f) in &c.fleet {
            for (role, r) in &f.role {
                if !ROLES.contains(&role.as_str()) {
                    bail!("fleet.{name}.role.{role}: role must be one of {ROLES:?}");
                }
                if !crate::harness::NAMES.contains(&r.harness.as_str()) {
                    bail!("fleet.{name}.role.{role}: harness must be one of {:?}", crate::harness::NAMES);
                }
            }
        }
        Ok(c)
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
    let header = format!("[fleet.{fleet}.role.{role}]");
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
            let eq = l.find('=').unwrap();
            let open = l[eq..].find('"').map(|o| eq + o + 1);
            let close = open.and_then(|o| l[o..].find('"').map(|c| o + c));
            let (Some(o), Some(c)) = (open, close) else {
                bail!("{header} {key}: value must be a \"string\"");
            };
            out[i] = format!("{}{}{}", &l[..o], val, &l[c..]);
        }
        None => {
            let last = (start..end).rev().find(|&i| !lines[i].trim().is_empty()).unwrap();
            if !out[last].ends_with('\n') {
                out[last].push('\n');
            }
            out.insert(last + 1, format!("{key} = \"{val}\"\n"));
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
