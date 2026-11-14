use crate::sched::World;
use drover::events::Event;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// A metric worse than the baseline by more than this (every metric is lower-is-better) is a warning.
pub const WORSE: f64 = 0.20;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Metrics {
    /// Seconds from an issue's first queue add to its merged event, per merged issue.
    pub to_merge: Vec<u64>,
    pub spawns: u64,
    /// Spawns whose pane pushed no head and wrote no verdict drover takes.
    pub wasted: u64,
    pub supersedes: u64,
    /// Distinct lane names spawned.
    pub lanes: u64,
    /// Pane-seconds a live lane spent not working.
    pub idle: u64,
    /// Seconds heads spent with CI pending.
    pub ci_wait: u64,
    /// Process-seconds in 1 s polls on drive.lock or a kick.
    pub lock_wait: u64,
    /// Drive actions refused SERIAL_BUSY, by a busy host (HOST_BUSY, MEM_LOW, DISK_LOW), by a cap (CAP, ALL_CAPPED, SPAWN_PENDING: the parallel-spawn slots).
    pub serial: u64,
    pub host_busy: u64,
    pub caps: u64,
    pub merges: u64,
    /// Heads reviewed (a verdict recorded) on the merged issues.
    pub rounds: u64,
}

pub fn of(w: &World) -> Metrics {
    let produced = |e: &Event| w.model.produced.iter().any(|p| Some(p.as_str()) == e.str("pane"));
    let mut m = log(w.wrote.iter().map(|x| (x.at, &x.ev)), produced);
    (m.idle, m.ci_wait, m.lock_wait) = (w.model.idle, w.model.ci_wait, w.model.polled);
    m
}

/// The metrics a log alone gives, for the sim's run and a real one alike.
pub fn log<'a>(evs: impl Iterator<Item = (u64, &'a Event)> + Clone, produced: impl Fn(&Event) -> bool) -> Metrics {
    let mut m = Metrics::default();
    let mut queued: BTreeMap<u64, u64> = BTreeMap::new();
    let mut lanes = BTreeSet::new();
    let mut merged = BTreeSet::new();
    for (at, e) in evs.clone() {
        let n = e.issue.unwrap_or(0);
        match e.kind.as_str() {
            "queue" if e.str("op") == Some("add") => {
                for i in e.data.get("issues").and_then(Value::as_array).into_iter().flatten() {
                    queued.entry(i.as_u64().unwrap_or(0)).or_insert(at);
                }
            }
            "spawn" => {
                m.spawns += 1;
                lanes.insert(e.str("agent").unwrap_or_default().to_string());
                if !produced(e) {
                    m.wasted += 1;
                }
            }
            "supersede" => m.supersedes += 1,
            "merged" if merged.insert(n) => {
                m.merges += 1;
                if let Some(q) = queued.get(&n) {
                    m.to_merge.push(at - q);
                }
            }
            "drive-pass" if !e.data.contains_key("updated") => {
                for a in e.data.get("actions").and_then(Value::as_array).into_iter().flatten() {
                    match a["code"].as_str().unwrap_or("") {
                        "SERIAL_BUSY" => m.serial += 1,
                        "HOST_BUSY" | "MEM_LOW" | "DISK_LOW" => m.host_busy += 1,
                        "CAP" | "ALL_CAPPED" | "SPAWN_PENDING" => m.caps += 1,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let rounds: BTreeSet<_> = evs
        .filter(|(_, e)| e.kind == "verdict" && merged.contains(&e.issue.unwrap_or(0)))
        .map(|(_, e)| (e.issue, e.str("sha")))
        .collect();
    m.rounds = rounds.len() as u64;
    m.lanes = lanes.len() as u64;
    m
}

fn quantile(v: &mut [u64], q: f64) -> u64 {
    v.sort_unstable();
    let k = (q * v.len() as f64).ceil() as usize;
    v.get(k.saturating_sub(1)).copied().unwrap_or(0)
}

/// The metrics over `runs`: queue→merge quantiles over every merged issue, the rest a mean per run (minutes where the name says so), review rounds per merge.
pub fn summary(runs: &[Metrics]) -> BTreeMap<&'static str, f64> {
    let k = runs.len().max(1) as f64;
    let sum = |f: fn(&Metrics) -> u64| runs.iter().map(f).sum::<u64>() as f64;
    let mut all: Vec<u64> = runs.iter().flat_map(|m| m.to_merge.clone()).collect();
    let round = |x: f64| (x * 100.0).round() / 100.0;
    BTreeMap::from([
        ("queue_to_merge_p50_min", quantile(&mut all, 0.5) as f64 / 60.0),
        ("queue_to_merge_p90_min", quantile(&mut all, 0.9) as f64 / 60.0),
        ("wasted_spawns", sum(|m| m.wasted) / k),
        ("supersedes_per_lane", sum(|m| m.supersedes) / sum(|m| m.lanes).max(1.0)),
        ("idle_lane_min", sum(|m| m.idle) / 60.0 / k),
        ("ci_wait_min", sum(|m| m.ci_wait) / 60.0 / k),
        ("lock_wait_min", sum(|m| m.lock_wait) / 60.0 / k),
        ("serial_busy", sum(|m| m.serial) / k),
        ("host_busy", sum(|m| m.host_busy) / k),
        ("caps", sum(|m| m.caps) / k),
        (
            "review_rounds_per_merge",
            sum(|m| m.rounds) / sum(|m| m.merges).max(1.0),
        ),
    ])
    .into_iter()
    .map(|(n, v)| (n, round(v)))
    .collect()
}

pub fn to_json(seeds: usize, s: &BTreeMap<&str, f64>) -> Value {
    json!({"seeds": seeds, "metrics": s})
}

/// The metrics more than `WORSE` worse than `base`'s (a metrics JSON), one line each.
pub fn regressions(s: &BTreeMap<&str, f64>, base: &Value) -> Vec<String> {
    let mut out = vec![];
    for (n, &v) in s {
        let Some(b) = base["metrics"][*n].as_f64() else {
            continue;
        };
        if v > b * (1.0 + WORSE) && v > b {
            let pct = if b > 0.0 {
                format!("+{:.0}%", (v - b) / b * 100.0)
            } else {
                "from 0".into()
            };
            out.push(format!(
                "WARNING {n}: {b} -> {v} ({pct}) against the last staging baseline"
            ));
        }
    }
    out
}

/// A markdown table (metric, value, baseline), then each regression.
pub fn table(seeds: usize, s: &BTreeMap<&str, f64>, base: Option<&Value>) -> String {
    let mut t = format!("sim metrics over {seeds} seeds\n\n| metric | value | baseline |\n|---|---:|---:|\n");
    for (n, v) in s {
        let b = base
            .and_then(|b| b["metrics"][*n].as_f64())
            .map_or("-".into(), |b| b.to_string());
        t += &format!("| {n} | {v} | {b} |\n");
    }
    for r in base.map(|b| regressions(s, b)).unwrap_or_default() {
        t += &format!("\n{r}");
    }
    t
}
