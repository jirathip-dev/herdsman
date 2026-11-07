//! The append-only event log (#344: in `state.db`, `db`), the state fold over it, and notifiers.
use crate::config::Config;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(optional_fields = nullable))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// #259: the log's monotonic number, set on append (an old row's is its line number); 0 before.
    #[serde(default, skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "ts", ts(as = "Option<u64>", optional))]
    pub seq: u64,
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
        Event {
            seq: 0,
            ts,
            fleet: fleet.into(),
            issue,
            kind: kind.into(),
            data,
        }
    }
    pub fn str(&self, k: &str) -> Option<&str> {
        self.data.get(k).and_then(Value::as_str)
    }
    /// A `kind` event about `fleet` #`issue`.
    pub fn is(&self, kind: &str, fleet: &str, issue: u64) -> bool {
        self.kind == kind && self.fleet == fleet && self.issue == Some(issue)
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if hands a reference
fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// #344: one event, numbered next, into the database beside `path` (`db::append`): every writer goes through here.
pub fn append(path: &Path, ev: &Event) -> Result<()> {
    crate::db::append(path, ev)
}

/// The live log (#343: without the events a rotation archived), by seq.
pub fn read(path: &Path) -> Result<Vec<Event>> {
    crate::db::events(path, "archived = ?1", 0)
}

/// #560: `read`, kept between calls (serve's requests and watcher): each call reads only the events appended since;
/// a rotation or another database file reads it all again. The last few databases read are kept.
pub fn shared(path: &Path) -> Result<std::sync::Arc<Vec<Event>>> {
    type Kept = (PathBuf, (Option<u64>, Option<u64>), std::sync::Arc<Vec<Event>>);
    static KEPT: std::sync::Mutex<Vec<Kept>> = std::sync::Mutex::new(Vec::new());
    let (file, key) = (crate::db::file(path), crate::db::version(path));
    let mut kept = KEPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut all = match kept.iter().position(|k| k.0 == file && k.1 == key) {
        Some(i) => kept.remove(i).2,
        None => std::sync::Arc::new(vec![]),
    };
    let last = all.last().map_or(0, |e| e.seq);
    let new = crate::db::events(path, "archived = 0 AND seq > ?1", last)?;
    if !new.is_empty() {
        std::sync::Arc::make_mut(&mut all).extend(new);
    }
    kept.truncate(7);
    kept.insert(0, (file, key, all.clone()));
    Ok(all)
}

/// #344: the log in the JSONL format before #344, one event a line with its seq (`herdsman events --jsonl`).
pub fn jsonl(path: &Path) -> Result<String> {
    history(path)?
        .iter()
        .map(|e| Ok(serde_json::to_string(e)? + "\n"))
        .collect()
}

/// A JSONL log from before #344 (`db::import`): every complete record; a torn last line is skipped silently, any other
/// bad line with a warning.
pub(crate) fn read_file(path: &Path) -> Result<Vec<Event>> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(parse(&t, path)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e.into()),
    }
}

fn parse(text: &str, path: &Path) -> Vec<Event> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::with_capacity(lines.len());
    for (i, l) in lines.iter().enumerate() {
        match serde_json::from_str::<Event>(l) {
            Ok(ev) if ev.seq == 0 => out.push(Event {
                seq: i as u64 + 1,
                ..ev
            }),
            Ok(ev) => out.push(ev),
            Err(_) if l.trim().is_empty() || i + 1 == lines.len() => {}
            Err(e) => eprintln!(
                "herdsman: {}: line {}: skipping corrupt record: {e}",
                path.display(),
                i + 1
            ),
        }
    }
    out
}

/// #343: the fold up to `seq` (the `states` snapshot row): a cache, the log rebuilds it.
#[derive(Serialize, Deserialize, Default)]
struct Checkpoint {
    seq: u64,
    states: Vec<IssueState>,
}

/// #343: the rotated archives (`events-YYYYMM.jsonl.gz`) of a log from before #344 (`db::import`).
pub(crate) fn archives(path: &Path) -> Result<Vec<Event>> {
    let dir = std::fs::read_dir(path.parent().unwrap_or(Path::new(".")));
    let mut gz: Vec<PathBuf> = dir.into_iter().flatten().filter_map(|e| Some(e.ok()?.path())).collect();
    gz.retain(|p| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with(".jsonl.gz"))
    });
    gz.sort();
    let mut all = vec![];
    for g in gz {
        let o = Command::new("gzip")
            .arg("-dc")
            .arg(&g)
            .stderr(Stdio::inherit())
            .output()?;
        all.extend(parse(&String::from_utf8_lossy(&o.stdout), &g));
    }
    Ok(all)
}

/// #343: every event, the archived ones too, by seq.
pub fn history(path: &Path) -> Result<Vec<Event>> {
    crate::db::events(path, "seq > ?1", 0)
}

/// #343: the checkpoint's states and seq, and the events after it (no checkpoint: every event), read by seq.
fn load(path: &Path) -> Result<(States, u64, Vec<Event>)> {
    let cp = crate::db::get::<Checkpoint>(path, "states")
        .map(|c| c.1)
        .unwrap_or_default();
    let m = cp.states.into_iter().map(|s| ((s.fleet.clone(), s.issue), s)).collect();
    Ok((m, cp.seq, crate::db::events(path, "seq > ?1", cp.seq)?))
}

/// #343: the current per-issue state, from the checkpoint and the events after it: `fold` of every event.
pub fn states(path: &Path) -> Result<States> {
    let (m, _, ev) = load(path)?;
    Ok(fold_into(m, &ev))
}

/// #343: save the checkpoint (each slow pass); returns its seq.
pub fn checkpoint(path: &Path) -> Result<u64> {
    let (m, seq, ev) = load(path)?;
    let seq = ev.last().map_or(seq, |e| e.seq);
    let states = fold_into(m, &ev).into_values().collect();
    crate::db::put(path, "states", seq, &Checkpoint { seq, states })?;
    Ok(seq)
}

/// #343: over `max` bytes of live log, the events up to the checkpoint and older than 30 days are archived (#344: kept
/// in the database for `history`, out of `read`). The live log keeps what its readers need: queue changes, every event
/// of an issue still open or active in the 30 days, and a done issue's merge or close.
pub fn rotate(path: &Path, max: u64, now: u64) -> Result<()> {
    if crate::db::live_bytes(path)? <= max {
        return Ok(());
    }
    let (cut, st, all) = (checkpoint(path)?, states(path)?, read(path)?);
    let old = |e: &Event| e.seq <= cut && e.ts + 30 * 86_400 < now && e.kind != "queue";
    let mut live_issue: BTreeMap<(&str, u64), bool> = BTreeMap::new();
    for e in &all {
        if let Some(n) = e.issue {
            let done = st.get(&(e.fleet.clone(), n)).is_some_and(IssueState::done);
            *live_issue.entry((&e.fleet, n)).or_default() |= !done || !old(e);
        }
    }
    // A merged issue keeps its merge; a closed one (it never merged) what closed it.
    let ends = |e: &Event, s: &IssueState| match s.state.as_str() {
        "merged" => e.kind == "merged",
        _ => e.kind == "closed" || e.kind == "clean" && e.str("why") != Some("merged"),
    };
    let moved = |e: &&Event| match e.issue {
        Some(n) => !live_issue[&(e.fleet.as_str(), n)] && !ends(e, &st[&(e.fleet.clone(), n)]),
        None => old(e),
    };
    let seqs: Vec<u64> = all.iter().filter(moved).map(|e| e.seq).collect();
    crate::db::archive(path, &seqs)
}

/// Append, then run notifiers. Notifier failures never fail the caller.
#[allow(clippy::needless_pass_by_value)] // public API: callers and tests hand the event over
pub fn emit(state: &Path, cfg: &Config, ev: Event) -> Result<()> {
    append(state, &ev)?;
    // #99: phones hear of owner asks and of each default taken, never of a routine ask merely waiting.
    // #407: a thread open on a board (its stream stamped `watch-<thread>.at` in the last 5 s) pushes nothing.
    let at = |t: &str| std::fs::read_to_string(state.with_file_name(format!("watch-{t}.at"))).ok();
    let watched = ev
        .str("thread")
        .and_then(at)
        .and_then(|a| a.trim().parse::<u64>().ok())
        .is_some_and(|a| a + 5 >= ev.ts);
    let push = match ev.kind.as_str() {
        _ if watched => "",
        "needs-decision" if ev.str("kind") == Some("routine") || ev.str("routed").is_some() => "",
        "decision" if ev.str("by") == Some("default") => "needs-decision",
        // #541: a lane ask put to the owner pushes as an ask; a conductor's answer to one never pushes.
        "ask-owner" => "needs-decision",
        "decision" if ev.str("by").is_some_and(|b| b.starts_with("conductor-")) => "",
        // #386: a conductor's reply pushes; the owner's own words and system lines never do.
        "thread-msg" if ev.str("from") != Some("conductor") => "",
        k => k,
    };
    let wanted = |n: &&crate::config::Notify| n.wants(if n.webpush() { push } else { &ev.kind });
    for n in cfg.notifiers(&ev.fleet).into_iter().filter(wanted) {
        let payload = if n.format.as_deref() == Some("json") {
            serde_json::to_string(&ev)?
        } else {
            plain(&ev) + "\n"
        };
        let res = if n.webpush() {
            crate::webpush::send(state, n.sub.as_deref().unwrap_or_default(), &ev, None).map(drop)
        } else {
            run_notifier(&n.cmd, &payload)
        };
        if let Err(e) = res {
            let cmd = if n.webpush() { "webpush" } else { &n.cmd };
            eprintln!("herdsman: notifier `{cmd}` failed: {e:#}");
            let d = serde_json::json!({"cmd": cmd, "event": ev.kind, "error": format!("{e:#}")});
            let _ = append(state, &Event::new(ev.ts, &ev.fleet, ev.issue, "notify-failed", d));
        }
    }
    Ok(())
}

/// One human-readable line, e.g. `api #1037 merged → staging 8ed4a06 (PR #1041)`.
pub fn plain(ev: &Event) -> String {
    let who = ev.issue.map_or(ev.fleet.clone(), |n| format!("{} #{n}", ev.fleet));
    let s = |k: &str| ev.str(k).unwrap_or("?");
    match ev.kind.as_str() {
        "merged" => {
            let sha: String = s("merge_sha").chars().take(7).collect();
            format!("{who} merged → {} {sha} (PR #{})", s("into"), s("pr"))
        }
        "blocked" => format!("{who} blocked {}: {}", s("code"), s("reason")),
        "needs-decision" | "ask-owner" => {
            let rec = ev.data["recommend"].as_u64();
            let opts: String = ev.data["options"]
                .as_array()
                .into_iter()
                .flatten()
                .zip(1..)
                .map(|(o, n)| {
                    format!(
                        "\n  {n}) {}{}",
                        o.as_str().unwrap_or("?"),
                        if rec == Some(n) { " (recommended)" } else { "" }
                    )
                })
                .collect();
            format!(
                "{who} needs you ({} asks {}): {}{opts}\n  herdsman answer {} {} <n|\"text\">",
                s("agent"),
                s("id"),
                s("question"),
                ev.fleet,
                s("id")
            )
        }
        "host-busy" => format!("host-busy {}: {}", s("host"), s("reason")),
        "decision" => format!("{who} decision {} by {}: {}", s("id"), s("by"), s("choice")),
        "queue" => {
            let n: Vec<String> = ev.data["issues"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|n| format!("#{n}"))
                .collect();
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

#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(optional_fields = nullable))]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct IssueState {
    pub fleet: String,
    pub issue: u64,
    pub state: String,
    pub lane: Option<String>,
    pub branch: Option<String>,
    /// The latest lane's reasoning effort, as spawned.
    pub effort: Option<String>,
    pub rounds: u32,
    /// Extra rounds the owner allowed (`round-extended`, #130); #309: the latest grants the round after any stage's.
    pub extra: u32,
    pub granted: u32,
    /// #123: passed `--update` rounds, apart from `rounds`; `updating` while one is open (state `fix(n)+uk`).
    pub updates: u32,
    pub updating: bool,
    pub last_ts: u64,
    pub last_event: String,
    /// Latest agent per role (impl, rev).
    pub lanes: BTreeMap<String, String>,
    /// (sha, PASS|FAIL, the check stage whose rounds it counts) in log order; logs before #173: `rev`.
    pub verdicts: Vec<(String, String, String)>,
    /// #173: the pipeline stage of the latest spawn, the latest check and build roles, review rounds per check stage.
    pub stage: String,
    pub check: Option<String>,
    pub build: Option<String>,
    pub stage_rounds: BTreeMap<String, u32>,
    /// #228: `FAIL (form-only)` verdicts that did not count toward the rounds.
    pub form: u32,
}

/// Per-issue state, keyed by (fleet, issue).
pub type States = BTreeMap<(String, u64), IssueState>;

/// The newest `spawn` of `agent`.
pub fn last_spawn<'a>(events: &'a [Event], agent: &str) -> Option<&'a Event> {
    events
        .iter()
        .rev()
        .find(|e| e.kind == "spawn" && e.str("agent") == Some(agent))
}

/// #425: the issue `s`'s lane is held on (`held`, no `released` or `spawn` since).
/// #505: the issues it waits on (`on`: one number before #505, a list since); empty when not held.
pub fn held_on(events: &[Event], s: &IssueState) -> Vec<u64> {
    let kinds = ["held", "released", "spawn"];
    let e = events
        .iter()
        .rev()
        .find(|e| e.fleet == s.fleet && e.issue == Some(s.issue) && kinds.contains(&&*e.kind) && !design(e));
    e.filter(|e| e.kind == "held")
        .map_or(vec![], |e| crate::queue::nums(&e.data["on"]))
}

/// #500: a design lane's spawn: never the issue's build or check lane.
pub fn design(e: &Event) -> bool {
    e.kind == "spawn" && e.str("role") == Some("design")
}

/// #424: lane end `e` ends spawn `sp` of its agent: only that pane's when `e` names one; a supersede logged
/// without a pane (before #424, after the new spawn) ends none.
pub fn ends(e: &Event, sp: &Event) -> bool {
    let pane = e.str("pane").map(|p| sp.str("pane") == Some(p));
    e.str("agent") == sp.str("agent") && pane.unwrap_or(e.str("reason") != Some("superseded"))
}

/// #173: a check stage's spawn (logs before #173: a `rev` one).
pub fn checks(e: &Event) -> bool {
    e.data
        .get("check")
        .and_then(Value::as_bool)
        .unwrap_or(e.str("role") == Some("rev"))
}

/// Events `herdsman check` and notifiers write about an issue; they are not activity.
pub const PASSIVE: &[&str] = &[
    "lane-state",
    "lesson-prompted",
    // #511: a lane's typed turn end and drive's prompt on it; neither moves the issue.
    "turn-end",
    "turn-prompted",
    "budget-freed",
    "wip-saved",
    "wip-blocked",
    "stall",
    "orphan-lane",
    "orphan-issue",
    "outbox-stale",
    "lane-dead",
    "ready-unmerged",
    "ci-missing",
    "notify-failed",
    "push-sent",
    "account-capped",
    "agent-unavailable",
    "spawn-failed",
    "spawn-started",
    // #500: an adoption is logged with its `spawn`; a design lane's alone must not end the issue's verdict wait.
    "spawn-adopted",
    "fallback",
    "ask-waiting",
    "driven",
    "respawn",
    "drive-pass",
    "kick",
    "amended",
    "closed-other",
    "conflict-predicted",
    "heads-up",
    "lane-closed",
    "lane-kept",
    "lane-kept-dirty",
    "lane-reaped",
    "lane-trimmed",
    "stale-pane-killed",
    "decision-redelivered",
    "labelled",
    "build-cloned",
    "unlabelled",
    "form-only-rejected",
    "form-rounds-exhausted",
    "stale-marker",
    "spawn-held",
    "host-busy",
    "outbox",
    // #584: an ask's other endings, and the one-time sweep.
    "ask-withdrawn",
    "ask-delivered",
    "ask-failed",
    "ask-expired",
    "asks-swept",
    // #509: a side effect's log around it; the step's own event (spawn, merged) is the activity.
    "intent",
    "done",
    "failed",
];

/// Why ask `a` closed (#584: every ending): answered, withdrawn, delivered or failed (a handoff card), expired, its
/// issue merged, or its lane killed, cleaned or respawned; `None` while open. The one place that decides "open".
pub fn closed(events: &[Event], a: &Event) -> Option<&'static str> {
    ending(events, a).map(|e| e.0)
}

/// `closed`, and the event that closed it.
fn ending<'a>(events: &'a [Event], a: &Event) -> Option<(&'static str, &'a Event)> {
    let before = || events.iter().take_while(|e| !std::ptr::eq(*e, a));
    let why: Vec<_> = (events.iter())
        .skip_while(|e| !std::ptr::eq(*e, a))
        .filter_map(|e| closes(a, e, || before().any(|x| started(x, e))).map(|w| (w, e)))
        .collect();
    let order = [
        "answered",
        "withdrawn",
        "delivered",
        "failed",
        "expired",
        "closed: merged",
        "closed: lane gone",
    ];
    order.into_iter().find_map(|w| why.iter().find(|x| x.0 == w).copied())
}

/// #584: `closed` as `herdsman asks` shows it: `withdrawn → q<new>`, `delivered to t<n>`.
pub fn closed_row(events: &[Event], a: &Event) -> Option<String> {
    let (why, e) = ending(events, a)?;
    Some(match (why, e.str("for"), e.str("to")) {
        ("withdrawn", Some(q), _) => format!("withdrawn → {q}"),
        ("delivered", _, Some(t)) => format!("delivered to {t}"),
        _ => why.into(),
    })
}

/// #446: `e` is the `spawn-started` of spawn `sp` (same agent and pane).
fn started(e: &Event, sp: &Event) -> bool {
    e.kind == "spawn-started" && e.str("agent") == sp.str("agent") && e.str("pane") == sp.str("pane")
}

/// #447: what later event `e` does to ask `a` (`closed`'s reasons, one event at a time). `own`: `e`'s
/// `spawn-started` came before the ask, so a `spawn` `e` is the asking lane itself (#446).
fn closes(a: &Event, e: &Event, own: impl FnOnce() -> bool) -> Option<&'static str> {
    let on = |k: &str| e.kind == k && a.issue.is_some() && e.issue == a.issue;
    let mine = matches!(e.kind.as_str(), "spawn" | "lane-killed") && e.str("agent") == a.str("agent");
    let id = e.str("id") == a.str("id");
    if e.fleet != a.fleet {
        None
    } else if e.kind == "decision" && id {
        Some(if a.data.get("handoff").is_some() {
            "delivered"
        } else {
            "answered"
        })
    } else if let (true, Some(w)) = (id, e.kind.strip_prefix("ask-")) {
        ["withdrawn", "delivered", "failed", "expired"]
            .into_iter()
            .find(|k| *k == w)
    } else if on("merged") {
        Some("closed: merged")
    } else if mine && !(e.kind == "spawn" && own()) || on("clean") {
        Some("closed: lane gone")
    } else {
        None
    }
}

/// The first finding of `fleet` #`issue`'s last verdict (the line after `PASS`/`FAIL`).
pub fn finding<'a>(events: &'a [Event], fleet: &str, issue: u64) -> Option<&'a str> {
    let v = events.iter().rev().find(|e| e.is("verdict", fleet, issue))?;
    v.str("body")?.lines().skip(1).map(str::trim).find(|l| !l.is_empty())
}

/// `needs-decision` events not yet closed, oldest first.
/// #447: one pass, each closing event checked against the asks still open.
pub fn open_asks(events: &[Event]) -> Vec<&Event> {
    let (mut open, mut first): (Vec<(usize, &Event)>, std::collections::HashMap<_, usize>) =
        (vec![], std::collections::HashMap::default());
    for (i, e) in events.iter().enumerate() {
        let at = (e.str("agent"), e.str("pane"));
        match e.kind.as_str() {
            "spawn-started" => {
                first.entry(at).or_insert(i);
            }
            "needs-decision" => open.push((i, e)),
            "decision" | "merged" | "clean" | "spawn" | "lane-killed" | "ask-withdrawn" | "ask-delivered"
            | "ask-failed" | "ask-expired" => {
                open.retain(|(n, a)| closes(a, e, || first.get(&at).is_some_and(|f| f < n)).is_none());
            }
            _ => {}
        }
    }
    open.into_iter().map(|x| x.1).collect()
}

/// Current per-issue state (the states in `roles/usage-tail.md`).
pub fn fold(events: &[Event]) -> States {
    fold_into(States::new(), events)
}

/// #343: `fold` on from the states `m` (a checkpoint's).
fn fold_into(mut m: States, events: &[Event]) -> States {
    for e in events {
        let Some(n) = e.issue else { continue };
        let new = || IssueState {
            fleet: e.fleet.clone(),
            issue: n,
            state: "queued".into(),
            ..Default::default()
        };
        let s = m.entry((e.fleet.clone(), n)).or_insert_with(new);
        // A merged issue never leaves `merged`, whatever is logged after it (#350: but for its landing reverted or
        // fixed forward); a closed one only for a new spawn.
        let back = matches!(e.kind.as_str(), "reverted" | "land-fix");
        if PASSIVE.contains(&e.kind.as_str())
            || s.state == "merged" && !back
            || s.state == "closed" && e.kind != "spawn"
        {
            continue;
        }
        // Design lanes are the owner's: they never move the issue through review or merge; #412: one on the issue
        // branch (a `--design` issue's, before impl) is the issue's design stage.
        if design(e) {
            if let Some(a) = e.str("agent") {
                s.lanes.insert("design".into(), a.into());
            }
            if !DESIGN.contains(&s.state.as_str()) || e.str("branch").is_none_or(|b| b.starts_with("design-")) {
                continue;
            }
            (s.state, s.lane) = ("design".into(), e.str("agent").map(String::from));
        }
        s.last_ts = e.ts;
        s.last_event.clone_from(&e.kind);
        match e.kind.as_str() {
            "spawn" if e.str("role") == Some("design") => {}
            "spawn" => {
                s.lane = e.str("agent").map(String::from);
                s.effort = e.str("effort").map(String::from);
                if let (Some(r), Some(a)) = (e.str("role"), e.str("agent")) {
                    s.lanes.insert(r.into(), a.into());
                }
                // #173: a check stage's spawn says so; before, only `rev` reviewed.
                let role = e.str("role").unwrap_or_default();
                let rev = checks(e);
                s.stage = e.str("stage").unwrap_or(role).into();
                *if rev { &mut s.check } else { &mut s.build } = Some(role.into());
                if !rev {
                    s.branch = e.str("branch").map(String::from).or(s.branch.take());
                    s.updating = e.data.get("update") == Some(&Value::Bool(true));
                }
                let u = if s.updating {
                    format!("+u{}", s.updates + 1)
                } else {
                    String::new()
                };
                let n = s.stage_rounds.get(&s.stage).copied().unwrap_or_default();
                s.state = match (rev, s.rounds) {
                    (true, _) => format!("review({}){u}", n + u32::from(!s.updating)),
                    (false, 0) => "impl".into(),
                    (false, n) => format!("fix({n}{}){u}", s.form_mark()),
                };
            }
            "verdict" => {
                let sha = e.str("sha").unwrap_or_default();
                let form = e.data.get("form_only") == Some(&Value::Bool(true));
                let v = e.str("verdict").unwrap_or_default().to_string() + if form { FORM } else { "" };
                // A round is a distinct reviewed sha; re-recording one does not burn another.
                // #123: a PASSed update round is an update; a FAILed merge-only check is a normal round.
                let new = u32::from(!s.reviewed(sha));
                // #173: a check stage counts its own reviews; a gate's (CI red) those of the check its fix goes to.
                let lp = e.str("loop").or(e.str("stage")).unwrap_or("rev").to_string();
                let mine = |x: &(String, String, String)| x.0.eq_ignore_ascii_case(sha) && x.2 == lp;
                let new_here = u32::from(!s.verdicts.iter().any(mine));
                // #228: a form-only FAIL starts a fix round but does not count toward the stage's rounds.
                if !(form || s.updating && v == "PASS") {
                    *s.stage_rounds.entry(lp.clone()).or_default() += new_here;
                }
                s.form += new_here * u32::from(form);
                *if s.updating && v == "PASS" {
                    &mut s.updates
                } else {
                    &mut s.rounds
                } += new;
                s.updating = false;
                s.verdicts.push((sha.into(), v.clone(), lp));
                s.state = if v == "PASS" {
                    "ready".into()
                } else {
                    format!("fix({}{})", s.rounds, s.form_mark())
                };
            }
            // #417: a verify stage that doesn't run at a head passes it there, in no round.
            "verify-skipped" | "verify-no-recipe" => {
                let (sha, stage) = (e.str("sha").unwrap_or_default(), e.str("stage").unwrap_or("verify"));
                s.verdicts.push((sha.into(), "PASS".into(), stage.into()));
            }
            "merged" | "closed" | "blocked" | "needs-human" | "parked" => s.state.clone_from(&e.kind),
            // #412: the owner's decision on the design: impl (or the design again) starts from the queue.
            "design-review" => s.state = "design-review".into(),
            "design-approved" | "design-revise" => (s.state, s.lane) = ("queued".into(), None),
            "design-rejected" => (s.state, s.lane) = ("needs-human".into(), None),
            // #500: a false `LANE_ENDS` park undone: the state before it.
            "repaired" => s.state = e.str("state").map_or(s.state.clone(), String::from),
            // #350: the FAIL verdict logged right after it starts the fix round.
            "reverted" | "land-fix" => s.state = format!("fix({})", s.rounds),
            "round-extended" => {
                (s.extra, s.granted) = (s.extra + 1, s.stage_rounds.values().max().unwrap_or(&0) + 1);
                // #151: `extend --no-spawn` spawns nothing, so the extra round itself leaves needs-human.
                if s.state == "needs-human" {
                    s.state = format!("fix({})", s.rounds);
                }
            }
            // #128: `clean` removes only merged or GitHub-closed lanes (logs before #128 carry no `why`).
            "clean" if e.str("why") != Some("merged") => s.state = "closed".into(),
            // Back to the state before the killed lane: queued / fix(n) for an impl, impl / fix(n) for a review.
            "lane-killed" if e.str("role").and_then(|r| s.lanes.get(r)).map(String::as_str) == e.str("agent") => {
                let role = e.str("role").unwrap_or_default();
                let chk = s.check.as_deref() == Some(role);
                s.lanes.remove(role);
                s.lane = None;
                if s.build.as_deref() == Some(role) && s.state == "impl" || chk && s.state.starts_with("review") {
                    s.state = if s.rounds > 0 {
                        format!("fix({})", s.rounds)
                    } else if chk {
                        "impl".into()
                    } else {
                        "queued".into()
                    };
                }
            }
            _ => {}
        }
    }
    m
}

/// #412: the states a design spawn moves into `design` (before impl).
pub const DESIGN: &[&str] = &["queued", "design", "design-review"];

/// #228: a form-only FAIL's suffix in `IssueState::verdicts` (`FAIL (form-only)`).
pub const FORM: &str = " (form-only)";

impl IssueState {
    /// #228: `·f` while the latest verdict is a form-only FAIL (`fix(2·f)`).
    fn form_mark(&self) -> &'static str {
        if self.verdicts.last().is_some_and(|v| v.1.ends_with(FORM)) {
            "·f"
        } else {
            ""
        }
    }

    /// A verdict was already recorded for this sha.
    pub fn reviewed(&self, sha: &str) -> bool {
        self.verdicts.iter().any(|(s, ..)| s.eq_ignore_ascii_case(sha))
    }

    /// Merged, or closed on GitHub: no lane, alert or queue slot any more.
    pub fn done(&self) -> bool {
        self.state == "merged" || self.state == "closed"
    }

    /// The lane that should be working now: impl in impl/fix, rev in review; none once ready, blocked or merged.
    pub fn active_lane(&self) -> Option<(&str, &str)> {
        let role = if self.state.starts_with("review") {
            self.check.as_deref()?
        } else if self.state == "impl" || self.state.starts_with("fix") {
            self.build.as_deref()?
        } else {
            return None;
        };
        self.lanes.get_key_value(role).map(|(r, a)| (r.as_str(), a.as_str()))
    }

    /// The event log still has an open lane for this role (`check`: a check stage's).
    pub fn unfinished(&self, role: &str, check: bool) -> bool {
        if check {
            self.state.starts_with("review")
        } else {
            self.lanes.contains_key(role) && !self.done()
        }
    }

    /// #173: the check lane (the latest check role's agent).
    pub fn check_lane(&self) -> Option<&String> {
        self.lanes.get(self.check.as_deref()?)
    }

    /// #173: the shown state: a check stage other than `rev` shows `<stage>(n)` for `review(n)`.
    pub fn shown(&self) -> String {
        match self.state.strip_prefix("review") {
            Some(n) if self.check.is_some() && self.stage != "rev" => format!("{}{n}", self.stage),
            _ => self.state.clone(),
        }
    }
}

pub fn age(secs: u64) -> String {
    match secs {
        s if s < 120 => format!("{s}s"),
        s if s < 7200 => format!("{}m", s / 60),
        s if s < 172_800 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86400),
    }
}

pub fn status_line(s: &IssueState, now: u64) -> String {
    let lane = s.lane.as_deref().unwrap_or("-");
    format!(
        "{:<12} #{:<5} {:<12} {lane:<24} {}",
        s.fleet,
        s.issue,
        s.shown(),
        age(now.saturating_sub(s.last_ts))
    )
}
