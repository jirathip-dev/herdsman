mod hunt;
mod invariants;
mod metrics;
mod model;
mod replay;
mod scenario;
mod sched;
mod seeds;

#[path = "../common/mod.rs"]
#[allow(unused_imports)]
mod common;

use invariants::Sabotage;
use sched::Sim;

const HOURS: u64 = 6;
const STEPS: u64 = 4000;

pub struct Outcome {
    pub seed: u64,
    pub failure: Option<String>,
    pub trace: Vec<String>,
    /// #686: every event the run appended, in order, and (a replay's `keep_going`) each violation with its step.
    pub events: Vec<drover::events::Event>,
    pub violations: Vec<String>,
    /// Steps taken (the failing step, if it failed), and what the run reached and its peaks (`hunt::coverage`).
    pub steps: u64,
    pub cov: std::collections::BTreeSet<String>,
    pub peaks: Vec<u64>,
    pub metrics: metrics::Metrics,
}

impl Outcome {
    /// The failure with its seed, the replay line and the trace's last 200 lines plus every scenario event (all of it with `DROVER_SIM_TRACE=full`).
    pub fn report(&self) -> String {
        let full = std::env::var("DROVER_SIM_TRACE").is_ok_and(|v| v == "full");
        let tail = self.trace.len().saturating_sub(200);
        let lines: Vec<&str> = (self.trace.iter().enumerate())
            .filter(|(i, l)| full || *i >= tail || l.contains(" event "))
            .map(|(_, l)| l.as_str())
            .collect();
        format!(
            "{}\nFAIL seed={} {}\nreplay: DROVER_SIM_SEED={} cargo test -p drover --test sim\n",
            lines.join("\n"),
            self.seed,
            self.failure.as_deref().unwrap_or("(passed)"),
            self.seed
        )
    }
}

/// Run `seed` (seed 1 is the #370 incident, any other a random fleet), `sabotage` breaking one invariant on purpose.
pub fn run(seed: u64, sabotage: Option<Sabotage>) -> Outcome {
    run_plan(&hunt::Plan::of(seed), sabotage)
}

/// Run `plan` (#688: a seed's scenario, or a hunt's mutant of one).
pub fn run_plan(plan: &hunt::Plan, sabotage: Option<Sabotage>) -> Outcome {
    let cfg = scenario::config(plan.cfg.0, plan.cfg.1);
    run_world(plan.seed, (plan.model(), cfg), plan.steps, sabotage)
}

/// Run fleet `world` (its model and fleet.toml) under `seed`'s scheduler for at most `steps` (the settle's included).
pub fn run_world(seed: u64, (mut m, cfg): (model::Model, String), steps: u64, sabotage: Option<Sabotage>) -> Outcome {
    m.sabotage = sabotage;
    play(
        seed,
        m,
        &cfg,
        scenario::T0 + HOURS * 3600,
        steps.min(STEPS),
        steps,
        false,
    )
}

pub fn play(seed: u64, m: model::Model, cfg: &str, end: u64, run_steps: u64, steps: u64, keep_going: bool) -> Outcome {
    for k in [
        "DROVER_LANE",
        "DROVER_OUTBOX",
        "DROVER_SERVICE",
        "DROVER_TOKEN",
        "DROVER_HOST",
    ] {
        std::env::remove_var(k);
    }
    let root = common::tmpdir(&format!("sim-{seed}"));
    scenario::setup(&root, cfg);
    let sim = Sim::new(seed, scenario::T0, m, root);
    sim.lock().keep_going = keep_going;
    let mut failure = sim.run(end, run_steps);
    if failure.is_none() {
        let mut w = sim.lock();
        let t = w.now;
        w.model
            .agenda
            .retain(|_, e| !matches!(e, model::Ev::Load(_) | model::Ev::Crash(_)));
        w.model.load = 1;
        drop(w);
        failure = sim.run(t + 1800, steps);
    }
    let metrics = metrics::of(&sim.lock());
    sim.stop();
    let mut w = sim.lock();
    let (cov, peaks) = hunt::coverage(&w);
    Outcome {
        seed,
        failure,
        trace: std::mem::take(&mut w.trace),
        events: w.wrote.drain(..).map(|x| x.ev).collect(),
        violations: std::mem::take(&mut w.violations),
        steps: w.step,
        cov,
        peaks,
        metrics,
    }
}

fn seeds() -> Vec<u64> {
    match std::env::var("DROVER_SIM_SEED").ok().and_then(|s| s.parse().ok()) {
        Some(s) => vec![s],
        None => seeds::FIXED.iter().map(|s| s.0).collect(),
    }
}

fn batch(seeds: &[u64]) -> Vec<Outcome> {
    let runs: Vec<_> = seeds
        .iter()
        .map(|&s| std::thread::spawn(move || run(s, None)))
        .collect();
    runs.into_iter().map(|h| h.join().expect("a run")).collect()
}

fn check(seeds: &[u64]) -> Vec<String> {
    let mut bad = vec![];
    for o in batch(seeds) {
        if let Some(f) = &o.failure {
            eprintln!("{}", o.report());
            bad.push(format!("seed {}: {f}", o.seed));
        }
    }
    bad
}

#[test]
fn fixed_seeds_hold_every_invariant() {
    let mut bad = vec![];
    let plan = std::env::var("DROVER_SIM_PLAN").ok();
    if plan.is_none() {
        bad = check(&seeds());
    }
    let texts: Vec<String> = match plan {
        Some(p) => vec![p],
        None => seeds::PLANS.iter().map(|p| p.0.to_string()).collect(),
    };
    for t in texts {
        let o = run_plan(&hunt::Plan::parse(&t).expect("a plan"), None);
        if let Some(f) = &o.failure {
            eprintln!("{}\nFAIL plan {t}\n{f}", o.trace.join("\n"));
            bad.push(format!("plan {t}: {f}"));
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

#[test]
#[ignore = "parked: each seed's cause is its own issue"]
fn parked_seeds() {
    let parked: Vec<u64> = seeds::PARKED.iter().map(|s| s.0).collect();
    let bad = check(&parked);
    assert!(bad.is_empty(), "{bad:#?}");
}

#[test]
fn the_incident_seed_records_the_verdict_with_one_supersede() {
    let o = run(seeds::INCIDENT, None);
    assert!(o.failure.is_none(), "{}", o.report());
    let sup = o.trace.iter().filter(|l| l.contains(" append supersede #370 ")).count();
    let told = o
        .trace
        .iter()
        .filter(|l| l.contains(" append proof-refused #370 "))
        .count();
    assert!(
        sup <= 3 && told >= 2,
        "supersedes {sup}, refusals {told}\n{}",
        o.report()
    );
    assert!(
        o.trace.iter().any(|l| l.contains(" append verdict #370 ")),
        "{}",
        o.report()
    );
}

#[test]
fn a_seed_replays_to_the_same_trace() {
    let (a, b) = (run(7, None), run(7, None));
    assert!(a.trace.len() > 500, "{}", a.trace.len());
    let diff = a.trace.iter().zip(&b.trace).position(|(x, y)| x != y);
    assert!(
        diff.is_none() && a.trace.len() == b.trace.len(),
        "first difference at {diff:?}: {:?} vs {:?}",
        diff.map(|i| &a.trace[i]),
        diff.map(|i| &b.trace[i])
    );
}

#[test]
fn a_broken_invariant_fails_with_seed_and_trace() {
    for (s, code) in [(Sabotage::DoubleLane, "I1 "), (Sabotage::RedMerge, "I4 ")] {
        let o = (2..40)
            .map(|seed| run(seed, Some(s)))
            .find(|o| o.failure.is_some())
            .expect("a seed that trips it");
        let f = o.failure.clone().unwrap_or_default();
        assert!(f.starts_with(code), "{s:?}: {f}");
        let r = o.report();
        assert!(
            r.contains(&format!("FAIL seed={} {code}", o.seed)) && r.contains(&format!("DROVER_SIM_SEED={}", o.seed)),
            "{r}"
        );
        assert!(r.lines().count() > 50, "{r}");
    }
}

fn env(k: &str, default: u64) -> u64 {
    std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
}

#[test]
#[ignore = "long: `just sim --metrics`"]
fn metrics_batch() {
    let (n, from) = (env("DROVER_SIM_N", 50), env("DROVER_SIM_FROM", 1000));
    let all: Vec<u64> = (from..from + n).collect();
    let runs: Vec<metrics::Metrics> = all.chunks(16).flat_map(batch).map(|o| o.metrics).collect();
    let s = metrics::summary(&runs);
    let base = std::env::var("DROVER_SIM_BASELINE")
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok());
    let base: Option<serde_json::Value> = base.and_then(|b| serde_json::from_str(&b).ok());
    let table = metrics::table(runs.len(), &s, base.as_ref());
    let json = metrics::to_json(runs.len(), &s).to_string();
    match std::env::var("DROVER_SIM_OUT") {
        Ok(d) => {
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(format!("{d}/metrics.json"), json + "\n").unwrap();
            std::fs::write(format!("{d}/metrics.md"), table + "\n").unwrap();
        }
        Err(_) => println!("{table}\n{json}"),
    }
}

#[test]
#[ignore = "long: `just sim`"]
fn random_seeds() {
    let n: u64 = std::env::var("DROVER_SIM_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);
    let from: u64 = std::env::var("DROVER_SIM_FROM")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    let mut bad = vec![];
    let all: Vec<u64> = (from..from + n).collect();
    for chunk in all.chunks(16) {
        bad.extend(check(chunk));
    }
    assert!(bad.is_empty(), "{} of {n} seeds failed:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn the_hunt_finds_a_planted_bug_sooner_than_random_seeds_and_minimises_it() {
    let s = Some(Sabotage::Planted);
    let tripped = |f: &str| f.starts_with("I1 ");
    let h = hunt::hunt(&[2, 3, 4, 5], 11, s, |r, f| {
        r < 40 && !f.iter().any(|x| tripped(&x.failure))
    });
    let f = h
        .found
        .iter()
        .find(|x| tripped(&x.failure))
        .expect("the hunt finds it within 40 rounds");
    let random: Vec<u64> = (2..2 + f.round as u64).collect();
    let hs: Vec<_> = random.iter().map(|&n| std::thread::spawn(move || run(n, s))).collect();
    for o in hs.into_iter().map(|h| h.join().expect("a run")) {
        let why = o.failure.unwrap_or_default();
        assert!(
            !tripped(&why),
            "random seed {} found it within the hunt's {} rounds: {why}",
            o.seed,
            f.round
        );
    }
    assert!(
        f.small.size() < f.plan.size() && f.small.steps < f.plan.steps,
        "{}",
        f.report()
    );
    let r = f.report();
    let text = f.small.text();
    assert!(
        r.contains(&format!("(\"{text}\", \"hunt: I1 ")) && r.contains("DROVER_SIM_PLAN="),
        "{r}"
    );
    let again = run_plan(&hunt::Plan::parse(&text).expect("its plan parses"), s);
    let why = again.failure.unwrap_or_default();
    assert!(
        tripped(&why) && again.steps == f.small.steps,
        "{why} at {} of {}",
        again.steps,
        f.small.steps
    );
}

#[test]
#[ignore = "long: `just sim --hunt`"]
fn hunt() {
    let env = |k: &str| std::env::var(k).ok().and_then(|s| s.parse::<u64>().ok());
    let minutes = env("DROVER_SIM_HUNT_MINUTES").unwrap_or(10);
    let clock = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    let rng = env("DROVER_SIM_HUNT_RNG").unwrap_or_else(|| clock.map_or(0, |d| d.as_secs()));
    let until = std::time::Instant::now() + std::time::Duration::from_secs(minutes * 60);
    let fixed: Vec<u64> = seeds::FIXED.iter().map(|s| s.0).collect();
    let h = hunt::hunt(&fixed, rng, None, |_, _| std::time::Instant::now() < until);
    let dir =
        std::path::PathBuf::from(std::env::var("DROVER_SIM_CORPUS").unwrap_or_else(|_| "target/sim-corpus".into()));
    std::fs::create_dir_all(&dir).expect("the corpus folder");
    let corpus: Vec<String> = h.corpus.iter().map(hunt::Plan::text).collect();
    std::fs::write(dir.join("corpus.txt"), corpus.join("\n") + "\n").expect("corpus.txt");
    let found: Vec<String> = h.found.iter().map(hunt::Found::report).collect();
    std::fs::write(dir.join("failing.txt"), found.join("\n")).expect("failing.txt");
    eprintln!(
        "hunt rng {rng}: {} rounds, corpus {}, {} failures, in {}",
        h.rounds,
        corpus.len(),
        found.len(),
        dir.display()
    );
    assert!(
        found.is_empty(),
        "replay this hunt with DROVER_SIM_HUNT_RNG={rng}:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_known_fleet_gives_exact_metrics() {
    let o = run_world(seeds::KNOWN, seeds::known(), STEPS + 1000, None);
    assert!(o.failure.is_none(), "{}", o.report());
    let want = metrics::Metrics {
        to_merge: vec![1027, 1336],
        spawns: 5,
        wasted: 1,
        supersedes: 1,
        lanes: 4,
        idle: 1004,
        ci_wait: 600,
        lock_wait: 13,
        serial: 0,
        host_busy: 0,
        caps: 0,
        merges: 2,
        rounds: 2,
    };
    assert_eq!(o.metrics, want, "{}", o.report());
    let s = metrics::summary(&[o.metrics]);
    let got: Vec<(&str, f64)> = s.iter().map(|(k, v)| (*k, *v)).collect();
    assert_eq!(
        got,
        [
            ("caps", 0.0),
            ("ci_wait_min", 10.0),
            ("host_busy", 0.0),
            ("idle_lane_min", 16.73),
            ("lock_wait_min", 0.22),
            ("queue_to_merge_p50_min", 17.12),
            ("queue_to_merge_p90_min", 22.27),
            ("review_rounds_per_merge", 1.0),
            ("serial_busy", 0.0),
            ("supersedes_per_lane", 0.25),
            ("wasted_spawns", 1.0),
        ]
    );
}

#[test]
fn a_metric_over_20_percent_worse_is_a_warning() {
    let o = run_world(seeds::KNOWN, seeds::known(), STEPS + 1000, None);
    let s = metrics::summary(&[o.metrics]);
    let base = serde_json::json!({"seeds": 1, "metrics": {"ci_wait_min": 8.0, "supersedes_per_lane": 0.24,
        "wasted_spawns": 0.0, "idle_lane_min": 200.0}});
    let t = metrics::table(1, &s, Some(&base));
    let warns: Vec<&str> = t.lines().filter(|l| l.starts_with("WARNING")).collect();
    assert_eq!(
        warns,
        [
            "WARNING ci_wait_min: 8 -> 10 (+25%) against the last staging baseline",
            "WARNING wasted_spawns: 0 -> 1 (from 0) against the last staging baseline",
        ],
        "{t}"
    );
    assert!(
        t.contains("| ci_wait_min | 10 | 8 |\n") && t.contains("| caps | 0 | - |\n"),
        "{t}"
    );
    let back: serde_json::Value = serde_json::from_str(&metrics::to_json(1, &s).to_string()).unwrap();
    assert!(metrics::regressions(&s, &back).is_empty());
}

#[test]
fn a_fixture_db_replays_to_its_merge_order_and_counts() {
    let dir = common::tmpdir("replay-fixture");
    let log = dir.join("events.jsonl");
    let t = 1_790_000_000;
    let ev = |s: u64, n: u64, kind: &str, data: serde_json::Value| {
        drover::events::Event::new(t + s, "fx", Some(n), kind, data)
    };
    let spawn = |s, n, role: &str, agent: &str| {
        ev(
            s,
            n,
            "spawn",
            serde_json::json!({"role": role, "agent": agent, "sha": format!("{n}-{s}")}),
        )
    };
    let verdict = |s, n, body: &str| {
        ev(
            s,
            n,
            "verdict",
            serde_json::json!({"body": body, "sha": format!("{n}-{s}")}),
        )
    };
    let (pass, fail) = ("PASS\n\nall good", "FAIL\n\n- blocking [correctness] x");
    let driven = |s, n, step: &str| ev(s, n, "driven", serde_json::json!({"step": step, "code": null}));
    let pushed = |s, n| ev(s, n, "pushed", serde_json::json!({"sha": "a".repeat(40)}));
    let queue = |s, n| {
        drover::events::Event::new(
            t + s,
            "fx",
            None,
            "queue",
            serde_json::json!({"op": "add", "issues": [n]}),
        )
    };
    let evs = [
        queue(0, 11),
        queue(30, 12),
        spawn(60, 11, "impl", "fx-impl-11"),
        spawn(90, 12, "impl", "fx-impl-12"),
        pushed(1260, 11),
        driven(1500, 11, "review"),
        spawn(1500, 11, "rev", "fx-rev-11-r1"),
        ev(
            1800,
            12,
            "turn-end",
            serde_json::json!({"role": "impl", "state": "done"}),
        ),
        driven(2100, 12, "review"),
        spawn(2100, 12, "rev", "fx-rev-12-r1"),
        verdict(2400, 11, pass),
        driven(2400, 11, "merge"),
        ev(2400, 11, "merged", serde_json::json!({})),
        verdict(3000, 12, fail),
        spawn(3100, 12, "impl", "fx-impl-12"),
        queue(3600, 13),
        drover::events::Event::new(
            t + 3601,
            "fx",
            None,
            "queue",
            serde_json::json!({"op": "block", "issues": [13], "on": 12}),
        ),
        queue(3700, 14),
        spawn(3800, 14, "impl", "fx-impl-14"),
        ev(4400, 14, "lane-dead", serde_json::json!({"key": "1:fx-impl-14"})),
        pushed(5000, 12),
        driven(5300, 12, "review"),
        spawn(5300, 12, "rev", "fx-rev-12-r2"),
        verdict(5900, 12, pass),
        ev(5900, 12, "merged", serde_json::json!({})),
        spawn(6000, 13, "impl", "fx-impl-13"),
        pushed(6600, 13),
        driven(6900, 13, "review"),
        spawn(6900, 13, "rev", "fx-rev-13-r1"),
        verdict(7500, 13, pass),
        ev(7500, 13, "merged", serde_json::json!({})),
        ev(4 * 3600, 0, "drive-pass", serde_json::json!({})),
    ];
    for e in &evs {
        drover::events::append(&log, e).unwrap();
    }
    let (evs, from, to) = replay::read(&dir.join("state.db"), "fx", 1).unwrap();
    let s = replay::build(&evs, "fx", from, to);
    assert_eq!(s.real.merged, [11, 12, 13]);
    let (o, sim) = replay::run(&s);
    eprintln!("{}", replay::table(&s.real, &sim));
    assert!(
        o.failure.is_none() && o.violations.is_empty(),
        "{:?}\n{}",
        o.violations,
        o.report()
    );
    assert_eq!(sim.merged, s.real.merged, "{}", o.report());
    let rounds = |r: &replay::Run| r.metrics.get("review_rounds_per_merge").copied();
    assert_eq!(
        (sim.verdicts, sim.deaths, rounds(&sim)),
        (s.real.verdicts, s.real.deaths, rounds(&s.real)),
        "{}",
        o.report()
    );
    assert_eq!(rounds(&s.real), Some(1.33));
    let saved = serde_json::to_string(&s).unwrap();
    assert!(!saved.contains("all good") && !saved.contains("blocking"), "{saved}");
}

#[test]
#[ignore = "long: `just sim --replay`"]
fn replay_db() {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let src = var("DROVER_SIM_REPLAY").expect("DROVER_SIM_REPLAY");
    let s: replay::Scenario = if src.ends_with(".json") {
        serde_json::from_str(&std::fs::read_to_string(&src).unwrap()).unwrap()
    } else {
        let fleet = var("DROVER_SIM_FLEET").unwrap_or_else(|| "herdsman".into());
        let days = var("DROVER_SIM_DAYS").and_then(|d| d.parse().ok()).unwrap_or(7);
        let (evs, from, to) = replay::read(std::path::Path::new(&src), &fleet, days).unwrap();
        replay::build(&evs, &fleet, from, to)
    };
    if let Some(p) = var("DROVER_SIM_SAVE") {
        std::fs::write(&p, serde_json::to_string_pretty(&s).unwrap()).unwrap();
        println!("scenario saved to {p}");
    }
    let started = std::time::Instant::now();
    let (o, sim) = replay::run(&s);
    let lanes: usize = s.lanes.values().map(Vec::len).sum();
    println!(
        "replay {} {}..{} ({} queue verbs, {} lanes, {} busy and {} offline windows): {} sim steps in {:.0?}",
        s.fleet,
        s.from,
        s.to,
        s.queue.len(),
        lanes,
        s.busy.len(),
        s.offline.len(),
        o.trace.len(),
        started.elapsed()
    );
    let end = o
        .trace
        .last()
        .map(|l| l.split_whitespace().nth(1).unwrap_or_default().to_string());
    println!(
        "ran to {} (window {} s + 1800 s settle)",
        end.unwrap_or_default(),
        s.to - s.from
    );
    match &o.violations[..] {
        [] => println!("invariants: all held (I1–I10)"),
        v => println!(
            "invariants: {} violation(s), each at its step:\n{}",
            v.len(),
            v.join("\n")
        ),
    }
    println!("{}", replay::table(&s.real, &sim));
    assert!(o.failure.is_none(), "{:?}", o.failure);
}
