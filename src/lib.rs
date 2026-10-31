//! fleet: agents act only through this CLI; it owns state and refuses rule breaks.
pub mod access;
pub mod check;
mod cli;
pub mod config;
pub mod devices;
pub mod events;
pub mod gate;
pub mod gh;
pub mod harness;
pub mod hook;
pub mod host;
pub mod lanes;
pub mod protection;
pub mod queue;
pub mod runner;
pub mod serve;
pub mod spawn;

pub use cli::{main_with, usage_doc};
use anyhow::{bail, Result};
use config::{home, set_in, set_value, write_atomic, Config};
use events::{emit, read, Event};
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;

#[derive(Clone)]
pub struct Ctx {
    pub config: PathBuf,
    pub state: PathBuf,
    pub worktrees: PathBuf,
    /// Hermes profiles root (`<root>/<profile>/config.yaml`).
    pub hermes: PathBuf,
    pub exe: String,
    pub now: u64,
    /// Tree `herdsman migrate` works on: `HERDSMAN_HOME`, else `$HOME`.
    pub home: PathBuf,
    /// Pre-rename fallbacks in use this run; data commands print them on stderr.
    pub hints: Vec<String>,
}

impl Ctx {
    pub fn from_env() -> Ctx {
        let env = |k: &str, d: PathBuf| var(k).map(PathBuf::from).unwrap_or(d);
        let root = var("HERDSMAN_HOME").map_or_else(home, PathBuf::from);
        let mut hints = vec![];
        // Read the pre-rename dir, never move it, while the new one is missing.
        let mut path = |k: &str, what: &str, d: &str, file: &str| {
            if let Some(p) = var(k) {
                return PathBuf::from(p);
            }
            let (old, new) = (root.join(d).join("fleet"), root.join(d).join("herdsman"));
            if old.is_dir() && !new.exists() {
                hints.push(format!("herdsman: found pre-rename {what} at {}; run 'herdsman migrate' to move it", old.display()));
                return old.join(file);
            }
            new.join(file)
        };
        let config = path("FLEET_CONFIG", "config", ".config", "fleet.toml");
        let state = path("FLEET_STATE", "state", ".local/state", "events.jsonl");
        Ctx {
            config,
            state,
            worktrees: env("FLEET_WORKTREES", home().join(".herdr/worktrees")),
            hermes: env("FLEET_HERMES_PROFILES", home().join(".hermes/profiles")),
            exe: std::env::current_exe().map_or("herdsman".into(), |p| p.to_string_lossy().into()),
            now: now(),
            home: root,
            hints,
        }
    }
    pub fn claude_dir(&self) -> PathBuf {
        self.config.parent().unwrap_or(&self.config).join("claude")
    }
}

/// A path variable, with set-but-empty treated as unset (an empty path would mean the cwd).
fn var(k: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(k).filter(|v| !v.is_empty())
}

/// `herdsman migrate` (docs/architecture.md): both moves or neither.
pub fn migrate(home: &std::path::Path, out: &mut dyn Write) -> Result<()> {
    let mut moved: Vec<(PathBuf, PathBuf)> = vec![];
    for d in [".config", ".local/state"] {
        let (old, new) = (home.join(d).join("fleet"), home.join(d).join("herdsman"));
        if new.exists() {
            writeln!(out, "nothing to move: {} already exists", new.display())?;
            continue;
        }
        if !old.is_dir() {
            writeln!(out, "nothing to move: no {}", old.display())?;
            continue;
        }
        if let Err(e) = std::fs::rename(&old, &new) {
            let mut msg = format!("moving {} -> {} failed: {e}", old.display(), new.display());
            for (o, n) in moved.iter().rev() {
                msg += &match std::fs::rename(n, o) {
                    Ok(()) => format!("; rolled back {} -> {}", n.display(), o.display()),
                    Err(e) => format!("; ROLLBACK FAILED {} -> {}: {e}", n.display(), o.display()),
                };
            }
            bail!(msg);
        }
        writeln!(out, "moved {} -> {}", old.display(), new.display())?;
        moved.push((old, new));
    }
    Ok(())
}

/// Seconds since the epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub const STATES: &str = "queued → impl → review(n) → fix(n) → ready → merged; plus blocked, needs-human";

/// Apply one queue change and log it (`herdsman queue`, and the page's ▲/▼ with `via`).
pub fn queue_change(ctx: &Ctx, cfg: &Config, fleet: &str, d: Value, via: Option<&str>) -> Result<Vec<queue::Item>> {
    cfg.fleet(fleet)?;
    let mut q = queue::replay(&read(&ctx.state)?, fleet);
    queue::apply(&mut q, &d)?;
    emit(&ctx.state, cfg, Event::new(ctx.now, fleet, None, "queue", with_via(d, via)))?;
    Ok(q)
}

/// `herdsman set` (and the page's model/effort dropdowns, with `via`): `wanted` is (key, new value).
pub fn set(ctx: &Ctx, out: &mut dyn Write, fleet: &str, role: &str, wanted: Vec<(&str, String)>, via: Option<&str>) -> Result<()> {
    let text = std::fs::read_to_string(&ctx.config)?;
    let cfg = Config::parse(&text)?;
    let cur = cfg.fleet(fleet)?.role(role)?.clone();
    if wanted.is_empty() {
        bail!("nothing to set: pass --harness, --model, --profile or --effort");
    }
    // Values become TOML strings: a quote, backslash or control char could inject lines (e.g. `[[notify]]`).
    if let Some((k, v)) = wanted.iter().find(|(_, v)| v.is_empty() || v.chars().any(|c| c == '"' || c == '\\' || c.is_control())) {
        return gate::refused("BAD_VALUE", format!("{k} {v:?}: empty, or has a quote, backslash or control char"));
    }
    if let Some((_, m)) = wanted.iter().find(|(k, m)| *k == "model" && !cfg.models.contains(m)) {
        return gate::refused("BAD_VALUE", format!("model {m:?} is not in `models` {:?}", cfg.models));
    }
    let mut new = text.clone();
    let mut changes = vec![];
    for (key, val) in wanted {
        let old = match key {
            "harness" => Some(cur.harness.clone()),
            "model" => cur.model.clone(),
            "profile" => cur.profile.clone(),
            _ => cur.effort.clone(),
        };
        let old = old.unwrap_or_else(|| "-".into());
        writeln!(out, "{fleet}.{role} {key}: {old} -> {val}")?;
        if old != val {
            new = set_value(&new, fleet, role, key, &val)?;
            changes.push(with_via(json!({"role": role, "key": key, "old": old, "new": val}), via));
        }
    }
    let new_cfg = Config::parse(&new)?;
    if new != text {
        write_atomic(&ctx.config, &new)?;
    }
    for c in changes {
        emit(&ctx.state, &new_cfg, Event::new(ctx.now, fleet, None, "set", c))?;
    }
    Ok(())
}

/// Event data `d`, plus `"via"` for the page's writes.
fn with_via(mut d: Value, via: Option<&str>) -> Value {
    if let Some(v) = via {
        d["via"] = json!(v);
    }
    d
}

/// `herdsman pause|resume <fleet>` (and the page's button, with `via`): `paused = true|false`.
pub fn pause(ctx: &Ctx, out: &mut dyn Write, fleet: &str, paused: bool, via: Option<&str>) -> Result<()> {
    let text = std::fs::read_to_string(&ctx.config)?;
    let cfg = Config::parse(&text)?;
    let word = if paused { "paused" } else { "resumed" };
    if cfg.fleet(fleet)?.paused == paused {
        writeln!(out, "{fleet}: already {word}")?;
        return Ok(());
    }
    let new = set_in(&text, &format!("[fleet.{fleet}]"), "paused", &paused.to_string())?;
    let new_cfg = Config::parse(&new)?;
    write_atomic(&ctx.config, &new)?;
    emit(&ctx.state, &new_cfg, Event::new(ctx.now, fleet, None, if paused { "pause" } else { "resume" }, with_via(json!({}), via)))?;
    writeln!(out, "{fleet}: {word}")?;
    Ok(())
}
