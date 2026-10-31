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
    /// A `kind` event about `fleet` #`issue`.
    pub fn is(&self, kind: &str, fleet: &str, issue: u64) -> bool {
        self.kind == kind && self.fleet == fleet && self.issue == Some(issue)
    }
}

/// Append one line and fsync, terminating a torn tail (a writer died mid-line) first.
pub fn append(path: &Path, ev: &Event) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let f = OpenOptions::new().create(true).append(true).read(true).open(path);
    let mut f = f.with_context(|| format!("cannot open event log {}", path.display()))?;
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

/// Every complete record: a torn last line is skipped silently, any other bad line with a warning.
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
    for n in cfg.notifiers(&ev.fleet).into_iter().filter(|n| n.on.contains(&ev.kind)) {
        let payload = if n.format.as_deref() == Some("json") { serde_json::to_string(&ev)? } else { plain(&ev) + "\n" };
        if let Err(e) = run_notifier(&n.cmd, &payload) {
            eprintln!("herdsman: notifier `{}` failed: {e:#}", n.cmd);
            let d = serde_json::json!({"cmd": n.cmd, "event": ev.kind, "error": format!("{e:#}")});
            let _ = append(state, &Event::new(ev.ts, &ev.fleet, ev.issue, "notify-failed", d));
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

/// Runs without `HERMES_*` / `_HERMES_*`: a lane's Hermes profile must not leak into the notifier's `hermes send`.
fn run_notifier(cmd: &str, payload: &str) -> Result<()> {
    let mut c = Command::new("sh");
    let hermes = |k: &std::ffi::OsString| k.to_string_lossy().trim_start_matches('_').starts_with("HERMES_");
    for (k, _) in std::env::vars_os().filter(|(k, _)| hermes(k)) {
        c.env_remove(k);
    }
    let mut child = c.args(["-c", cmd]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
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
    /// The latest lane's reasoning effort, as spawned.
    pub effort: Option<String>,
    pub rounds: u32,
    pub last_ts: u64,
    pub last_event: String,
    /// Latest agent per role (impl, rev).
    pub lanes: BTreeMap<String, String>,
    /// (sha, PASS|FAIL) in log order.
    pub verdicts: Vec<(String, String)>,
}

/// The newest `spawn` of `agent`.
pub fn last_spawn<'a>(events: &'a [Event], agent: &str) -> Option<&'a Event> {
    events.iter().rev().find(|e| e.kind == "spawn" && e.str("agent") == Some(agent))
}

/// Events `herdsman check` and notifiers write about an issue; they are not activity.
pub const PASSIVE: [&str; 5] = ["stall", "lane-dead", "ready-unmerged", "notify-failed", "account-capped"];

/// Current per-issue state (the states in `roles/usage-tail.md`).
pub fn fold(events: &[Event]) -> BTreeMap<(String, u64), IssueState> {
    let mut m: BTreeMap<(String, u64), IssueState> = BTreeMap::new();
    for e in events {
        let Some(n) = e.issue else { continue };
        let new = || IssueState { fleet: e.fleet.clone(), issue: n, state: "queued".into(), ..Default::default() };
        let s = m.entry((e.fleet.clone(), n)).or_insert_with(new);
        // A merged issue never leaves `merged`, whatever is logged after it.
        if PASSIVE.contains(&e.kind.as_str()) || s.state == "merged" {
            continue;
        }
        // Design lanes are the owner's: they never move the issue through review or merge.
        if e.kind == "spawn" && e.str("role") == Some("design") {
            if let Some(a) = e.str("agent") {
                s.lanes.insert("design".into(), a.into());
            }
            continue;
        }
        s.last_ts = e.ts;
        s.last_event = e.kind.clone();
        match e.kind.as_str() {
            "spawn" => {
                s.lane = e.str("agent").map(String::from);
                s.effort = e.str("effort").map(String::from);
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

    /// The lane that should be working now: impl in impl/fix, rev in review; none once ready, blocked or merged.
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
    let lane = s.lane.as_deref().unwrap_or("-");
    format!("{:<12} #{:<5} {:<12} {lane:<24} {}", s.fleet, s.issue, s.state, age(now.saturating_sub(s.last_ts)))
}
