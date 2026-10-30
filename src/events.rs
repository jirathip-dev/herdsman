//! Append-only JSONL event log, the state fold over it, and notifiers.
use crate::config::Config;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub ts: u64,
    pub fleet: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<u64>,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(flatten)]
    pub data: Map<String, Value>,
}

impl Event {
    pub fn new(ts: u64, fleet: &str, issue: Option<u64>, kind: &str, data: Value) -> Event {
        let data = match data {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        Event { ts, fleet: fleet.into(), issue, kind: kind.into(), data }
    }
    pub fn str(&self, k: &str) -> Option<&str> {
        self.data.get(k).and_then(Value::as_str)
    }
}

/// Append one line and fsync. If a previous writer died mid-line, the torn
/// tail is terminated first so it can never merge with this record.
pub fn append(path: &Path, ev: &Event) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)
        .with_context(|| format!("cannot open event log {}", path.display()))?;
    let mut line = serde_json::to_string(ev)?;
    line.push('\n');
    let len = f.metadata()?.len();
    if len > 0 {
        use std::io::{Seek, SeekFrom};
        let mut last = [0u8; 1];
        f.seek(SeekFrom::Start(len - 1))?;
        f.read_exact(&mut last)?;
        if last[0] != b'\n' {
            line.insert(0, '\n');
        }
    }
    f.write_all(line.as_bytes())?;
    f.sync_all()?;
    Ok(())
}

/// Read every complete record. A torn last line (writer killed mid-append) is
/// skipped silently; any other unparsable non-empty line is skipped with a warning.
pub fn read(path: &Path) -> Result<Vec<Event>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::with_capacity(lines.len());
    for (i, l) in lines.iter().enumerate() {
        match serde_json::from_str(l) {
            Ok(ev) => out.push(ev),
            Err(_) if l.trim().is_empty() || i + 1 == lines.len() => {}
            Err(e) => eprintln!("herdsman: {}: line {}: skipping corrupt record: {e}", path.display(), i + 1),
        }
    }
    Ok(out)
}

/// Append, then run notifiers. Notifier failures never fail the caller.
pub fn emit(state: &Path, cfg: &Config, ev: Event) -> Result<()> {
    append(state, &ev)?;
    for n in cfg.notify.iter().filter(|n| n.on.contains(&ev.kind)) {
        let payload = if n.format.as_deref() == Some("json") { serde_json::to_string(&ev)? } else { plain(&ev) + "\n" };
        if let Err(e) = run_notifier(&n.cmd, &payload) {
            eprintln!("herdsman: notifier `{}` failed: {e:#}", n.cmd);
            let fail = Event::new(
                ev.ts,
                &ev.fleet,
                ev.issue,
                "notify-failed",
                serde_json::json!({"cmd": n.cmd, "event": ev.kind, "error": format!("{e:#}")}),
            );
            let _ = append(state, &fail);
        }
    }
    Ok(())
}

/// One human-readable line, e.g. `sendmeter #1037 merged → staging 8ed4a06 (PR #1041)`.
pub fn plain(ev: &Event) -> String {
    let who = match ev.issue {
        Some(n) => format!("{} #{n}", ev.fleet),
        None => ev.fleet.clone(),
    };
    let s = |k: &str| ev.str(k).unwrap_or("?");
    match ev.kind.as_str() {
        "merged" => {
            let sha: String = s("merge_sha").chars().take(7).collect();
            format!("{who} merged → {} {sha} (PR #{})", s("into"), s("pr"))
        }
        "blocked" => format!("{who} blocked {}: {}", s("code"), s("reason")),
        "queue" => {
            let n: Vec<String> = ev.data["issues"].as_array().into_iter().flatten().map(|n| format!("#{n}")).collect();
            format!("{who} queue {} {}", s("op"), n.join(" "))
        }
        k => match ev.str("reason") {
            Some(r) => format!("{who} {k}: {r}"),
            None => format!("{who} {k}"),
        },
    }
}

/// Runs with the caller's environment minus Hermes-owned vars (`HERMES_*`, `_HERMES_*`):
/// a lane's Hermes profile must not leak into the notifier's own `hermes send`.
fn run_notifier(cmd: &str, payload: &str) -> Result<()> {
    let mut c = Command::new("sh");
    let hermes = |k: &std::ffi::OsString| k.to_string_lossy().trim_start_matches('_').starts_with("HERMES_");
    for (k, _) in std::env::vars_os().filter(|(k, _)| hermes(k)) {
        c.env_remove(k);
    }
    let mut child = c
        .args(["-c", cmd])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // A sink that exits without reading stdin must not fail the write.
    let _ = child.stdin.take().unwrap().write_all(payload.as_bytes());
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(st) = child.try_wait()? {
            anyhow::ensure!(st.success(), "exit {}", st.code().unwrap_or(-1));
            return Ok(());
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            anyhow::bail!("timed out after 15s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct IssueState {
    pub fleet: String,
    pub issue: u64,
    pub state: String,
    pub lane: Option<String>,
    pub branch: Option<String>,
    pub rounds: u32,
    pub last_ts: u64,
    pub last_event: String,
    /// Latest agent per role (impl, rev).
    pub lanes: BTreeMap<String, String>,
    /// (sha, PASS|FAIL) in log order.
    pub verdicts: Vec<(String, String)>,
}

/// Events `herdsman check` and notifiers write about an issue; they are not activity.
pub const PASSIVE: [&str; 5] = ["stall", "lane-dead", "ready-unmerged", "notify-failed", "account-capped"];

/// Current per-issue state: queued → impl → review(n) → fix(n) → ready → merged,
/// plus blocked / needs-human.
pub fn fold(events: &[Event]) -> BTreeMap<(String, u64), IssueState> {
    let mut m: BTreeMap<(String, u64), IssueState> = BTreeMap::new();
    for e in events {
        let Some(n) = e.issue else { continue };
        let s = m.entry((e.fleet.clone(), n)).or_insert_with(|| IssueState {
            fleet: e.fleet.clone(),
            issue: n,
            state: "queued".into(),
            ..Default::default()
        });
        // A merged issue never leaves `merged`, whatever is logged after it.
        if PASSIVE.contains(&e.kind.as_str()) || s.state == "merged" {
            continue;
        }
        s.last_ts = e.ts;
        s.last_event = e.kind.clone();
        match e.kind.as_str() {
            "spawn" => {
                s.lane = e.str("agent").map(String::from);
                if let (Some(r), Some(a)) = (e.str("role"), e.str("agent")) {
                    s.lanes.insert(r.into(), a.into());
                }
                if e.str("role") == Some("rev") {
                    s.state = format!("review({})", s.rounds + 1);
                } else {
                    if let Some(b) = e.str("branch") {
                        s.branch = Some(b.into());
                    }
                    s.state = if s.rounds == 0 { "impl".into() } else { format!("fix({})", s.rounds) };
                }
            }
            "verdict" => {
                let sha = e.str("sha").unwrap_or_default();
                // A round is a distinct reviewed sha; re-recording one does not burn another.
                if !s.reviewed(sha) {
                    s.rounds += 1;
                }
                let v = e.str("verdict").unwrap_or_default().to_string();
                s.verdicts.push((sha.into(), v.clone()));
                s.state = if v == "PASS" { "ready".into() } else { format!("fix({})", s.rounds) };
            }
            "merged" | "blocked" | "needs-human" => s.state = e.kind.clone(),
            _ => {}
        }
    }
    m
}

impl IssueState {
    /// A verdict was already recorded for this sha.
    pub fn reviewed(&self, sha: &str) -> bool {
        self.verdicts.iter().any(|(s, _)| s.eq_ignore_ascii_case(sha))
    }

    /// The lane that should be doing work now: impl while implementing or fixing,
    /// rev while a review round is open; none once ready, blocked or merged.
    pub fn active_lane(&self) -> Option<(&str, &str)> {
        let role = if self.state.starts_with("review") {
            "rev"
        } else if self.state == "impl" || self.state.starts_with("fix") {
            "impl"
        } else {
            return None;
        };
        self.lanes.get(role).map(|a| (role, a.as_str()))
    }

    /// The event log still has an open lane for this role.
    pub fn unfinished(&self, role: &str) -> bool {
        match role {
            "rev" => self.state.starts_with("review"),
            _ => self.lanes.contains_key(role) && self.state != "merged",
        }
    }
}

pub fn age(secs: u64) -> String {
    match secs {
        s if s < 120 => format!("{s}s"),
        s if s < 7200 => format!("{}m", s / 60),
        s if s < 172800 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86400),
    }
}

pub fn status_line(s: &IssueState, now: u64) -> String {
    format!(
        "{:<12} #{:<5} {:<12} {:<24} {}",
        s.fleet,
        s.issue,
        s.state,
        s.lane.as_deref().unwrap_or("-"),
        age(now.saturating_sub(s.last_ts))
    )
}
