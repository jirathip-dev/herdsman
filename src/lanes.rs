//! The page's Lanes tab: every herdr agent joined with the herdsman log, grouped by
//! fleet (agents in unregistered dirs go to Others), plus a masked activity line
//! taken from the agent's visible pane text.
use crate::config::Config;
use crate::events::{Event, IssueState};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct Lane {
    /// The registered fleet the agent works for; None = Others.
    pub fleet: Option<String>,
    /// Agent name, or the workspace label for an unnamed agent outside any fleet.
    pub name: String,
    pub issue: Option<u64>,
    /// orch | impl | rev | design, or `other` when herdsman did not spawn it.
    pub role: String,
    pub harness: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// herdr's live state (idle | working | done | blocked | unknown), or `dead`.
    pub state: String,
    /// When herdsman spawned it (0: unknown).
    pub since: u64,
    pub pane: Option<String>,
    /// The log has an open lane that herdr shows no pane for.
    pub mismatch: bool,
}

/// Join `herdr agent list` rows with the log. `ws` maps workspace id -> label.
pub fn join(
    cfg: &Config,
    worktrees: &Path,
    events: &[Event],
    st: &BTreeMap<(String, u64), IssueState>,
    agents: &[Value],
    ws: &BTreeMap<String, String>,
) -> Vec<Lane> {
    let spawns: Vec<&Event> = events.iter().rev().filter(|e| e.kind == "spawn").collect();
    let (mut out, mut seen) = (vec![], BTreeSet::new());
    for a in agents {
        let s = |k: &str| a[k].as_str().filter(|v| !v.is_empty());
        let (name, pane) = (s("name"), s("pane_id"));
        // Agents that report their own state (dsh-TUI) can lose the name: match the lane's pane.
        let sp = spawns.iter().find(|e| name.is_some() && e.str("agent") == name)
            .or_else(|| spawns.iter().find(|e| name.is_none() && pane.is_some() && e.str("pane") == pane));
        let mut l = Lane { state: s("agent_status").unwrap_or("unknown").into(), pane: pane.map(String::from), ..Lane::default() };
        match sp {
            Some(e) => {
                seen.insert(e.str("agent").unwrap_or_default().to_string());
                l = Lane { name: e.str("agent").unwrap_or_default().into(), ..from_spawn(e, l) };
            }
            None => {
                let inside = |d: &Path| ["cwd", "foreground_cwd"].iter().any(|k| s(k).is_some_and(|c| Path::new(c).starts_with(d)));
                let fleet = cfg.fleet.iter().find(|(_, f)| inside(&f.local_path()) || inside(&worktrees.join(f.repo_name())));
                l.fleet = fleet.map(|f| f.0.clone());
                l.role = "other".into();
                l.harness = s("agent").unwrap_or("shell").trim_end_matches("-tui").into();
                let label = s("workspace_id").map(|w| ws.get(w).cloned().unwrap_or_else(|| w.into()));
                l.name = name.map(String::from).or(label).unwrap_or_else(|| "?".into());
            }
        }
        out.push(l);
    }
    for s in st.values().filter(|s| cfg.fleet.contains_key(&s.fleet)) {
        let Some((_, agent)) = s.active_lane().filter(|(_, a)| !seen.contains(*a)) else { continue };
        if let Some(e) = spawns.iter().find(|e| e.str("agent") == Some(agent)) {
            let dead = Lane { state: "dead".into(), mismatch: true, ..Lane::default() };
            out.push(Lane { name: agent.into(), ..from_spawn(e, dead) });
        }
    }
    out
}

fn from_spawn(e: &Event, l: Lane) -> Lane {
    let s = |k: &str| e.str(k).map(String::from);
    Lane {
        fleet: Some(e.fleet.clone()),
        issue: e.issue,
        role: s("role").unwrap_or_default(),
        harness: s("harness").unwrap_or_default(),
        model: s("model"),
        effort: s("effort"),
        since: e.ts,
        ..l
    }
}

const HIDE: &str = "•••";
const PREFIXES: [&str; 8] = ["sk-", "ghp_", "gho_", "ghs_", "ghu_", "github_pat_", "xox", "xapp-"];

/// Mask anything that looks like a secret: `sk-…`, `ghp_…`, `gho_…`, `xox…` tokens,
/// JWTs, hex/base64 runs of 32+ chars, and the values of `*KEY=` / `*TOKEN=` / `*SECRET=`.
pub fn mask(s: &str) -> String {
    let tok = |c: char| c.is_ascii_alphanumeric() || "-_+/=.".contains(c);
    let (mut out, mut rest, mut hide_next) = (String::new(), s, false);
    while let Some(i) = rest.find(tok) {
        out += &rest[..i];
        let n = rest[i..].find(|c| !tok(c)).unwrap_or(rest.len() - i);
        let w = &rest[i..i + n];
        let (word, after_eq) = if std::mem::take(&mut hide_next) { (HIDE.to_string(), false) } else { word(w) };
        out += &word;
        // `TOKEN="value"`: the value is the next run.
        hide_next = after_eq && rest[i + n..].starts_with(['"', '\'']);
        rest = &rest[i + n..];
    }
    out + rest
}

/// One run of token chars, masked; true when it ends in a secret-named `KEY=`.
fn word(w: &str) -> (String, bool) {
    if let Some((k, v)) = w.split_once('=') {
        let up = k.to_ascii_uppercase();
        if ["KEY", "TOKEN", "SECRET", "PASSWORD"].iter().any(|s| up.contains(s)) {
            return if v.is_empty() { (w.into(), true) } else { (format!("{k}={HIDE}"), false) };
        }
    }
    if let Some(p) = PREFIXES.iter().find(|p| w.starts_with(**p) && w.len() >= p.len() + 10) {
        return (format!("{p}{HIDE}"), false);
    }
    let jwt = w.starts_with("eyJ") && w.matches('.').count() >= 2;
    let long = |seg: &str| {
        seg.len() >= 32 && seg.chars().any(|c| c.is_ascii_digit()) && seg.chars().any(|c| c.is_ascii_alphabetic())
    };
    if jwt || w.split(['.', '/', '-']).any(long) {
        return (HIDE.into(), false);
    }
    (w.into(), false)
}

/// Box-drawing rules, prompts and status bars: no activity there.
fn chrome(l: &str) -> bool {
    l.chars().all(|c| "─━│┃╭╮╰╯┌┐└┘═╌┄▔▁ ".contains(c)) || l.starts_with(['❯', '⏵', '>', '›'])
}

/// The agent's latest action from its visible pane, masked, cut to 60 chars: Claude
/// Code's last `⏺` line, Hermes's last tool line (`💻` / `┊`), else the last non-chrome line.
pub fn activity(harness: &str, screen: &str) -> String {
    let lines: Vec<&str> = screen.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let line = match harness {
        "claude" => lines.iter().rev().find(|l| l.starts_with('⏺')),
        "hermes" => lines.iter().rev().find(|l| l.contains('💻') || l.starts_with('┊')),
        _ => lines.iter().rev().find(|l| !chrome(l)),
    };
    let m = mask(line.copied().unwrap_or_default());
    match m.chars().count() {
        n if n > 60 => m.chars().take(59).collect::<String>() + "…",
        _ => m,
    }
}

/// The pane's last `n` non-empty lines, masked.
pub fn tail(screen: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].iter().map(|l| mask(l.trim_end())).collect()
}
