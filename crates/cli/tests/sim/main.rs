mod hunt;
mod invariants;
mod metrics;
mod model;
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
    m.sabotage = sabotage;
    scenario::setup(&root, &cfg);
    let sim = Sim::new(seed, scenario::T0, m, root);
    let end = scenario::T0 + HOURS * 3600;
    let mut failure = sim.run(end, steps.min(STEPS));
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
    let random: Vec<u64> = (1000..1000 + f.round as u64).collect();
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
        to_merge: vec![1012, 1337],
        spawns: 5,
        wasted: 1,
        supersedes: 1,
        lanes: 4,
        idle: 992,
        ci_wait: 600,
        lock_wait: 12,
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
            ("idle_lane_min", 16.53),
            ("lock_wait_min", 0.2),
            ("queue_to_merge_p50_min", 16.87),
            ("queue_to_merge_p90_min", 22.28),
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
