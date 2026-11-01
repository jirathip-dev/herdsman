//! The per-fleet work queue (`queue` events): `herdsman queue`, `next` and `board`.
use crate::events::{age, fold, Event, IssueState, States};
use crate::overlap::Conflicts;
use anyhow::{anyhow, ensure, Result};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub issue: u64,
    pub blocked_by: Option<u64>,
    /// `queue add --serial`: touches the fleet's `serial_paths`.
    pub serial: bool,
}

/// A queue change, as logged: `op` is add | mv | rm | block.
pub fn op(op: &str, issues: &[u64], top: bool, before: Option<u64>, after: Option<u64>, on: Option<u64>) -> Value {
    let mut d = json!({"op": op, "issues": issues});
    for (k, v) in [("before", before), ("after", after), ("on", on)] {
        if let Some(v) = v {
            d[k] = json!(v);
        }
    }
    if top {
        d["top"] = json!(true);
    }
    d
}

/// Apply one change; Err leaves `q` untouched.
pub fn apply(q: &mut Vec<Item>, d: &Value) -> Result<()> {
    let issues: Vec<u64> = d["issues"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .collect();
    ensure!(!issues.is_empty(), "no issue given");
    let pos = |q: &[Item], n: u64| {
        q.iter()
            .position(|i| i.issue == n)
            .ok_or_else(|| anyhow!("#{n} is not queued"))
    };
    match d["op"].as_str().unwrap_or_default() {
        op @ ("add" | "mv") => {
            if op == "mv" {
                pos(q, issues[0])?;
            }
            let anchor = d["before"].as_u64().or(d["after"].as_u64());
            if let Some(a) = anchor {
                ensure!(!issues.contains(&a), "#{a} cannot be placed relative to itself");
                pos(q, a)?;
            }
            let serial = d["serial"].as_bool() == Some(true);
            let mut moved: Vec<Item> = issues
                .iter()
                .map(|&n| Item {
                    issue: n,
                    blocked_by: None,
                    serial,
                })
                .collect();
            for m in &mut moved {
                if let Some(old) = q.iter().find(|i| i.issue == m.issue) {
                    m.blocked_by = old.blocked_by;
                    m.serial |= old.serial;
                }
            }
            q.retain(|i| !issues.contains(&i.issue));
            let at = match (d["top"].as_bool(), d["before"].as_u64(), d["after"].as_u64()) {
                (Some(true), ..) => 0,
                (_, Some(b), _) => pos(q, b)?,
                (_, _, Some(a)) => pos(q, a)? + 1,
                _ => q.len(),
            };
            q.splice(at..at, moved);
        }
        "rm" => {
            q.remove(pos(q, issues[0])?);
        }
        "block" => {
            let on = d["on"].as_u64().ok_or_else(|| anyhow!("block needs --on"))?;
            ensure!(on != issues[0], "#{on} cannot block itself");
            let i = pos(q, issues[0])?;
            q[i].blocked_by = Some(on);
        }
        o => anyhow::bail!("unknown queue op `{o}`"),
    }
    Ok(())
}

/// Replay the fleet's `queue` events; invalid ones (never logged by fleet) are skipped.
pub fn replay(events: &[Event], fleet: &str) -> Vec<Item> {
    let mut q = vec![];
    for e in events.iter().filter(|e| e.kind == "queue" && e.fleet == fleet) {
        let mut next = q.clone();
        if apply(&mut next, &Value::Object(e.data.clone())).is_ok() {
            q = next;
        }
    }
    q
}

fn state<'a>(st: &'a States, fleet: &str, n: u64) -> &'a str {
    st.get(&(fleet.to_string(), n)).map_or("queued", |s| s.state.as_str())
}

/// The fleet's issues marked `--serial`, merged ones included.
pub fn serial(events: &[Event], fleet: &str) -> Vec<u64> {
    replay(events, fleet)
        .into_iter()
        .filter(|i| i.serial)
        .map(|i| i.issue)
        .collect()
}

/// The fleet's `--serial` issues in flight: a live impl/fix lane, or at review/ready (`SERIAL_BUSY`).
pub fn serial_in_flight<'a>(events: &[Event], st: &'a States, fleet: &str) -> Vec<&'a IssueState> {
    let serial = serial(events, fleet);
    let busy = |s: &str| s == "impl" || s.starts_with("fix") || s.starts_with("review") || s == "ready";
    st.values()
        .filter(|s| s.fleet == fleet && serial.contains(&s.issue) && busy(&s.state))
        .collect()
}

/// `serial n/max` for a fleet with `serial_paths`: board and status.
pub fn serial_use(events: &[Event], st: &States, fleet: &str, cfg: Option<&crate::config::Config>) -> Option<String> {
    let f = cfg
        .and_then(|c| c.fleet.get(fleet))
        .filter(|f| !f.serial_paths.is_empty())?;
    Some(format!(
        "serial {}/{}",
        serial_in_flight(events, st, fleet).len(),
        f.serial_max.unwrap_or(1)
    ))
}

/// The queue as shown: merged issues drop out; a blocker counts until it is merged.
pub fn view(events: &[Event], st: &States, fleet: &str) -> Vec<Item> {
    let open = replay(events, fleet)
        .into_iter()
        .filter(|i| state(st, fleet, i.issue) != "merged");
    open.map(|i| Item {
        blocked_by: i.blocked_by.filter(|b| state(st, fleet, *b) != "merged"),
        ..i
    })
    .collect()
}

/// `herdsman next`: (exit code, line) for the first unblocked queued issue with no lane.
pub fn next(events: &[Event], fleet: &str) -> (i32, String) {
    let st = fold(events);
    let free: Vec<Item> = view(events, &st, fleet)
        .into_iter()
        .filter(|i| state(&st, fleet, i.issue) == "queued")
        .collect();
    if let Some(n) = startable(events, fleet).into_iter().next() {
        return (0, n);
    }
    match free.first() {
        Some(Item {
            issue,
            blocked_by: Some(b),
            ..
        }) => (2, format!("#{issue} is blocked by #{b}")),
        _ => (1, format!("{fleet}: queue empty")),
    }
}

/// Every unblocked queued issue with no lane, in queue order: drive tries each, `next` is the first.
pub fn startable(events: &[Event], fleet: &str) -> Vec<String> {
    let st = fold(events);
    let free = |i: &Item| state(&st, fleet, i.issue) == "queued" && i.blocked_by.is_none();
    let q = view(events, &st, fleet);
    q.into_iter().filter(free).map(|i| i.issue.to_string()).collect()
}

/// (code, why) of drive's refused start of queued issue `n` in the last hour: `skipped this pass` (status, board).
pub fn skipped<'a>(events: &'a [Event], st: &States, fleet: &str, n: u64, now: u64) -> Option<(&'a str, &'a str)> {
    let s = |e: &&Event| e.kind == "driven" && e.fleet == fleet && e.issue == Some(n) && e.str("step") == Some("start");
    (state(st, fleet, n) == "queued").then_some(())?;
    let e = events.iter().rev().find(s).filter(|e| now < e.ts + 3600)?;
    Some((e.str("code")?, e.str("result")?.split_once(": ").map_or("", |w| w.1)))
}

pub fn ls_json(events: &[Event], fleet: &str) -> Value {
    let st = fold(events);
    let row = |(k, i): (usize, &Item)| json!({"pos": k + 1, "issue": i.issue, "state": state(&st, fleet, i.issue), "blocked_by": i.blocked_by, "serial": i.serial});
    Value::Array(view(events, &st, fleet).iter().enumerate().map(row).collect())
}

pub const WIDTH: usize = 60;

/// One fleet's board, lines ≤ 60 cols: the queue, other open issues, issues merged in the last 24 h.
pub fn board(events: &[Event], fleet: &str, now: u64, cfg: Option<&crate::config::Config>, c: &Conflicts) -> String {
    let st = fold(events);
    let q = view(events, &st, fleet);
    let mine: Vec<&IssueState> = st.values().filter(|s| s.fleet == fleet).collect();
    let active = mine.iter().filter(|s| s.active_lane().is_some()).count();
    let head = format!(
        "{:>2} {:<6} {:<11} {:<20} {:<5} {:<4} LAST",
        "Q", "ISSUE", "STATE", "LANE", "EFF", "AGE"
    );
    let serial = serial_use(events, &st, fleet, cfg).map_or(String::new(), |s| format!(", {s}"));
    let mut lines = vec![format!("{fleet}: {} queued, {active} active{serial}", q.len()), head];
    let row = |pos: String, n: u64, last: Option<String>| {
        let s = st.get(&(fleet.to_string(), n));
        let skip = skipped(events, &st, fleet, n, now).map(|(c, _)| format!("skip {c}"));
        let lane: String = s
            .and_then(|s| s.lane.clone())
            .or(skip)
            .unwrap_or_else(|| "-".into())
            .chars()
            .take(20)
            .collect();
        let eff = s.and_then(|s| s.effort.as_deref()).unwrap_or("-");
        let (ts, ev) = s.map_or((0, ""), |s| (s.last_ts, s.last_event.as_str()));
        let a = if ts == 0 {
            "-".into()
        } else {
            age(now.saturating_sub(ts))
        };
        // #129: its own line, the row is full: `⚠ conflicts with #98 (serve.rs)`.
        let warn = c.get(&n).and_then(|c| c.first()).map_or(String::new(), |(o, files)| {
            let o = if *o == 0 { "integration".into() } else { format!("#{o}") };
            let names: Vec<_> = files.iter().map(|p| p.rsplit('/').next().unwrap_or_default()).collect();
            format!("\n         ⚠ conflicts with {o} ({})", names.join(", "))
        });
        let last = last.unwrap_or_else(|| if ev.is_empty() { "-".into() } else { ev.into() });
        format!(
            "{pos:>2} #{n:<5} {:<11} {lane:<20} {eff:<5} {a:<4} {last}{warn}",
            state(&st, fleet, n)
        )
    };
    for (k, i) in q.iter().enumerate() {
        lines.push(row(
            (k + 1).to_string(),
            i.issue,
            i.blocked_by.map(|b| format!("on #{b}")),
        ));
    }
    let queued = |n: u64| q.iter().any(|i| i.issue == n);
    let rest = mine.iter().filter(|s| !queued(s.issue) && s.state != "merged");
    let merged = mine
        .iter()
        .filter(|s| s.state == "merged" && now.saturating_sub(s.last_ts) < 86_400);
    for s in rest.chain(merged) {
        lines.push(row("-".into(), s.issue, None));
    }
    lines
        .iter()
        .flat_map(|l| l.lines())
        .map(|l| l.chars().take(WIDTH).collect::<String>().trim_end().to_string() + "\n")
        .collect()
}

/// Fleets with anything in the log, in name order.
pub fn fleets(events: &[Event]) -> Vec<String> {
    let set: std::collections::BTreeSet<&str> = events.iter().map(|e| e.fleet.as_str()).collect();
    set.into_iter().map(String::from).collect()
}
