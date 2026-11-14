use crate::model::{Ev, Model, Script, FLEET};
use crate::scenario::{args, config, timers, T0};
use crate::sched::Rng;
use drover::events::Event;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// What a replay drives, times in seconds from the window's start (sim time `T0`).
#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct Scenario {
    pub fleet: String,
    /// The real window, unix seconds.
    pub from: u64,
    pub to: u64,
    /// The owner's queue verbs: when, and the words after `queue <fleet>`.
    pub queue: Vec<(u64, Vec<String>)>,
    /// Each `<role>#<issue>`'s lanes in spawn order: what its agent did (the last one again for any later lane).
    pub lanes: BTreeMap<String, Vec<Script>>,
    /// Windows the host refused spawns HOST_BUSY, and a host was offline (the sim has one host: both are busy).
    pub busy: Vec<(u64, u64)>,
    pub offline: Vec<(u64, u64)>,
    /// What really happened that week.
    pub real: Run,
}

/// A run's outcome: the merge order, lane deaths, verdicts logged and #685's metrics.
#[derive(Serialize, Deserialize, Debug, Default, Clone, PartialEq)]
pub struct Run {
    pub merged: Vec<u64>,
    pub deaths: usize,
    pub verdicts: usize,
    pub metrics: BTreeMap<String, f64>,
}

/// `fleet`'s events of the last `days` before its newest one, read-only.
pub fn read(db: &Path, fleet: &str, days: u64) -> anyhow::Result<(Vec<Event>, u64, u64)> {
    let c = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let to: u64 = c.query_row("SELECT max(ts) FROM events WHERE fleet = ?1", [fleet], |r| r.get(0))?;
    let from = to.saturating_sub(days * 86_400);
    let mut q = c.prepare("SELECT json FROM events WHERE fleet = ?1 AND ts >= ?2 ORDER BY ts, seq")?;
    let rows = q.query_map(rusqlite::params![fleet, from], |r| r.get::<_, String>(0))?;
    let evs = rows
        .map(|j| Ok(serde_json::from_str(&j?)?))
        .collect::<anyhow::Result<Vec<Event>>>()?;
    let from = evs.first().map_or(from, |e| e.ts);
    Ok((evs, from, to))
}

fn issues(e: &Event) -> Vec<u64> {
    let v = e.data.get("issues").and_then(|v| v.as_array());
    v.into_iter().flatten().filter_map(serde_json::Value::as_u64).collect()
}

fn role(e: &Event) -> String {
    e.str("role")
        .map(String::from)
        .or_else(|| e.str("agent").and_then(crate::invariants::lane_key).map(|k| k.1))
        .unwrap_or_default()
}

fn merge(mut w: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    w.sort_unstable();
    let mut out: Vec<(u64, u64)> = vec![];
    for (a, b) in w {
        match out.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// The scenario `evs` (one fleet's, ts-ordered, from `from` to `to`) describe.
pub fn build(evs: &[Event], fleet: &str, from: u64, to: u64) -> Scenario {
    let at = |e: &Event| e.ts.saturating_sub(from);
    let mut s = Scenario {
        fleet: fleet.into(),
        from,
        to,
        ..Scenario::default()
    };
    let mut queued = BTreeSet::new();
    for e in evs.iter().filter(|e| e.kind == "queue") {
        let ns = issues(e);
        let words: Vec<Vec<String>> = match e.str("op") {
            Some("add") => {
                queued.extend(&ns);
                let top = e.data.get("top").and_then(serde_json::Value::as_bool) == Some(true);
                let mut w = vec!["add".to_string()];
                w.extend(top.then(|| "--top".to_string()));
                w.extend(ns.iter().map(ToString::to_string));
                vec![w]
            }
            Some("block") => match e.data.get("on").and_then(serde_json::Value::as_u64) {
                Some(on) => (ns.iter())
                    .map(|n| args(&["block", "--on", &on.to_string(), &n.to_string()]))
                    .collect(),
                None => vec![],
            },
            Some(op @ ("unblock" | "rm")) => ns.iter().map(|n| args(&[op, &n.to_string()])).collect(),
            _ => vec![],
        };
        s.queue.extend(words.into_iter().map(|w| (at(e), w)));
    }
    let mut late = vec![];
    for e in evs
        .iter()
        .filter(|e| ["spawn", "pushed", "verdict", "merged"].contains(&e.kind.as_str()))
    {
        if let Some(n) = e.issue.filter(|n| queued.insert(*n)) {
            late.push((0, args(&["add", &n.to_string()])));
        }
    }
    s.queue.splice(0..0, late);
    let merged: BTreeSet<u64> = evs
        .iter()
        .filter(|e| e.kind == "merged")
        .filter_map(|e| e.issue)
        .collect();
    for (i, sp) in evs.iter().enumerate().filter(|(_, e)| e.kind == "spawn") {
        let (Some(n), r, agent) = (sp.issue, role(sp), sp.str("agent").unwrap_or_default()) else {
            continue;
        };
        if r != "impl" && r != "rev" {
            continue;
        }
        let secs = |e: &Event| e.ts.saturating_sub(sp.ts).max(1);
        let mine = |e: &Event| e.str("agent") == Some(agent) || e.str("key").is_some_and(|k| k.ends_with(agent));
        let end = evs[i + 1..].iter().enumerate().find_map(|(j, e)| {
            let same = e.issue == Some(n);
            let step = e.str("step").unwrap_or_default();
            match e.kind.as_str() {
                "pushed" if same && r == "impl" => Some(push(&evs[i + 1 + j..], n, secs(e))),
                "turn-end" if same && r == "impl" && e.str("state") == Some("done") => {
                    Some(push(&evs[i + 1 + j..], n, secs(e)))
                }
                "driven" if same && r == "impl" && ["review", "ci-red", "merge"].contains(&step) => {
                    Some(Script::Push {
                        secs: secs(e),
                        green: step != "ci-red",
                        ci: Some(60),
                    })
                }
                "verdict" if same && r == "rev" => Some(Script::Review {
                    secs: secs(e),
                    pass: e.str("body").is_some_and(|b| b.trim_start().starts_with("PASS")),
                    proof: true,
                    heeds: true,
                }),
                "lane-dead" | "account-capped" if mine(e) => Some(Script::Die { secs: secs(e) }),
                "spawn" | "supersede" if same && mine(e) => Some(Script::Hang),
                _ => None,
            }
        });
        let lanes = s.lanes.entry(format!("{r}#{n}")).or_default();
        lanes.push(end.unwrap_or(Script::Hang));
    }
    for &n in &queued {
        let imp = s.lanes.entry(format!("impl#{n}")).or_default();
        if imp.is_empty() {
            imp.push(Script::Push {
                secs: 600,
                green: true,
                ci: None,
            });
        }
        let rev = s.lanes.entry(format!("rev#{n}")).or_default();
        if rev.is_empty() {
            rev.push(match merged.contains(&n) {
                true => Script::Review {
                    secs: 600,
                    pass: true,
                    proof: true,
                    heeds: true,
                },
                false => Script::Hang,
            });
        }
    }
    let busy: Vec<(u64, u64)> = (evs.iter())
        .filter(|e| e.kind == "driven" && e.str("code") == Some("HOST_BUSY"))
        .map(|e| (at(e), at(e) + 300))
        .collect();
    s.busy = merge(busy);
    let mut offline = vec![];
    for (i, e) in evs.iter().enumerate().filter(|(_, e)| e.kind == "host-offline") {
        let on = (evs[i..].iter()).find(|x| x.kind == "host-online" && x.str("host") == e.str("host"));
        offline.push((at(e), on.map_or(to - from, at)));
    }
    s.offline = merge(offline);
    s.real = real(evs, from);
    s
}

fn push(evs: &[Event], n: u64, secs: u64) -> Script {
    let ci = (evs[1..].iter())
        .take_while(|e| !(e.kind == "pushed" && e.issue == Some(n)))
        .find(|e| {
            e.kind == "driven"
                && e.issue == Some(n)
                && ["review", "ci-red", "merge"].contains(&e.str("step").unwrap_or(""))
        });
    Script::Push {
        secs,
        green: ci.is_none_or(|e| e.str("step") != Some("ci-red")),
        ci: ci.map(|e| (e.ts - evs[0].ts).clamp(60, 3600)),
    }
}

/// What the log `evs` (from `from` on) says happened.
pub fn real(evs: &[Event], from: u64) -> Run {
    let mut produced = BTreeSet::new();
    let mut last: BTreeMap<(u64, String), *const Event> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut adds = vec![];
    for e in evs {
        let n = e.issue.unwrap_or(0);
        let r = role(e);
        let done = match e.kind.as_str() {
            "queue" if e.str("op") == Some("add") => {
                seen.extend(issues(e));
                None
            }
            "spawn" if r == "rev" => Some("impl"),
            "pushed" => Some("impl"),
            "turn-end" if e.str("state") == Some("done") => Some("impl"),
            "verdict" => Some("rev"),
            _ => None,
        };
        if let Some(p) = done.and_then(|d| last.get(&(n, d.to_string()))) {
            produced.insert(*p);
        }
        if e.kind == "spawn" {
            last.insert((n, r), e as *const Event);
        }
        if ["spawn", "pushed", "verdict", "merged"].contains(&e.kind.as_str()) && seen.insert(n) {
            adds.push(Event::new(
                from,
                &e.fleet,
                None,
                "queue",
                json!({"op": "add", "issues": [n]}),
            ));
        }
    }
    let log = adds.iter().map(|e| (from, e)).chain(evs.iter().map(|e| (e.ts, e)));
    let mut m = crate::metrics::summary(&[crate::metrics::log(log, |e| produced.contains(&(e as *const Event)))]);
    m.retain(|k, _| !["idle_lane_min", "ci_wait_min", "lock_wait_min"].contains(k));
    Run {
        merged: merged(evs),
        deaths: evs.iter().filter(|e| e.kind == "lane-dead").count(),
        verdicts: evs.iter().filter(|e| e.kind == "verdict").count(),
        metrics: m.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    }
}

fn merged(evs: &[Event]) -> Vec<u64> {
    let mut out = vec![];
    for n in evs.iter().filter(|e| e.kind == "merged").filter_map(|e| e.issue) {
        if !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// The sim's world for `s`: its timers, the queue verbs at their times, each lane's script, the busy windows.
pub fn model(s: &Scenario) -> (Model, String) {
    let mut rng = Rng::new(s.from);
    let mut m = Model::default();
    let sha = m.sha();
    m.origin.insert("staging".into(), sha);
    timers(&mut m, &mut rng, T0 + 1);
    let mut last = 0;
    for (t, w) in &s.queue {
        let mut a = args(&["queue", FLEET]);
        a.extend(w.iter().cloned());
        last = (*t).max(last + 5);
        m.at(T0 + last, Ev::Run(a));
    }
    for (a, b) in merge([&s.busy[..], &s.offline].concat()) {
        m.at(T0 + a, Ev::Load(40));
        m.at(T0 + b, Ev::Load(1));
    }
    m.scripts = s.lanes.clone();
    (m, config(4, 6))
}

/// Replay `s` to its end; the run and what the sim did.
pub fn run(s: &Scenario) -> (crate::Outcome, Run) {
    let (m, cfg) = model(s);
    let o = crate::play(s.from, m, &cfg, T0 + (s.to - s.from), u64::MAX / 2, u64::MAX / 2, true);
    let metrics = crate::metrics::summary(std::slice::from_ref(&o.metrics));
    let sim = Run {
        merged: merged(&o.events),
        deaths: o.events.iter().filter(|e| e.kind == "lane-dead").count(),
        verdicts: o.events.iter().filter(|e| e.kind == "verdict").count(),
        metrics: metrics.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
    };
    (o, sim)
}

/// The real-vs-sim table: the merges, then each #685 metric (`-` where the real log has none).
pub fn table(real: &Run, sim: &Run) -> String {
    let same = (real.merged.iter().zip(&sim.merged))
        .take_while(|(a, b)| a == b)
        .count();
    let mut t = String::from("| metric | real | sim |\n|---|---:|---:|\n");
    t += &format!("| merges | {} | {} |\n", real.merged.len(), sim.merged.len());
    t += &format!("| same merge order (leading issues) | {same} | |\n");
    t += &format!("| lane deaths | {} | {} |\n", real.deaths, sim.deaths);
    t += &format!("| verdicts logged | {} | {} |\n", real.verdicts, sim.verdicts);
    for (k, v) in &sim.metrics {
        let r = real.metrics.get(k).map_or("-".into(), ToString::to_string);
        t += &format!("| {k} | {r} | {v} |\n");
    }
    t
}
