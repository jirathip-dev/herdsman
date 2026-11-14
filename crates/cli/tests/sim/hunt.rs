use crate::invariants::Sabotage;
use crate::model::{Ev, Model, Script};
use crate::scenario::{default_script, T0};
use crate::sched::{Rng, World};
use crate::{run_plan, seeds, Outcome};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// A run's input: `Plan::of(seed)` is the seed's own scenario; `steps` bounds the run (the settle's included).
#[derive(Clone, Debug)]
pub struct Plan {
    pub seed: u64,
    pub cfg: (u64, u64),
    pub steps: u64,
    pub items: Vec<(u64, Ev)>,
    pub scripts: Vec<(String, Script)>,
}

impl Plan {
    pub fn of(seed: u64) -> Plan {
        let (m, cfg) = if seed == seeds::INCIDENT {
            seeds::incident()
        } else {
            crate::scenario::random(seed)
        };
        let scripts = (m.scripts.into_iter())
            .flat_map(|(r, v)| v.into_iter().map(move |s| (r.clone(), s)))
            .collect();
        let items = m.agenda.into_iter().map(|((t, _), e)| (t, e)).collect();
        Plan {
            seed,
            cfg,
            steps: crate::STEPS + 1000,
            items,
            scripts,
        }
    }

    pub fn model(&self) -> Model {
        let mut m = Model::default();
        let s = m.sha();
        m.origin.insert("staging".into(), s);
        for (t, e) in &self.items {
            m.at(*t, e.clone());
        }
        for (r, s) in &self.scripts {
            m.scripts.entry(r.clone()).or_default().push(s.clone());
        }
        m
    }

    /// Faults, owner verbs and scripts: what a minimised plan drops (the timers are the fleet itself).
    pub fn size(&self) -> usize {
        self.items
            .iter()
            .filter(|(_, e)| !matches!(e, Ev::Timer { .. }))
            .count()
            + self.scripts.len()
    }

    /// One line: `seed=N cfg=S,R steps=K | +t T 300 drive | +t R queue … | +t L 40 | +t C kick | impl P 200 1 | …`.
    pub fn text(&self) -> String {
        let mut out = vec![format!(
            "seed={} cfg={},{} steps={}",
            self.seed, self.cfg.0, self.cfg.1, self.steps
        )];
        for (t, e) in &self.items {
            let at = t - T0;
            out.push(match e {
                Ev::Timer { args, every } => format!("+{at} T {every} {}", args.join(" ")),
                Ev::Run(args) => format!("+{at} R {}", args.join(" ")),
                Ev::Load(l) => format!("+{at} L {l}"),
                Ev::Crash(k) => format!("+{at} C {k}"),
                Ev::Lane { .. } | Ev::Ci { .. } => unreachable!("a plan holds no run-time event"),
            });
        }
        for (r, s) in &self.scripts {
            let b = |x: bool| u8::from(x);
            out.push(match s {
                Script::Push { secs, green, ci } => {
                    let ci = ci.map(|c| format!(" {c}")).unwrap_or_default();
                    format!("{r} P {secs} {}{ci}", b(*green))
                }
                Script::Review {
                    secs,
                    pass,
                    proof,
                    heeds,
                } => format!("{r} V {secs} {} {} {}", b(*pass), b(*proof), b(*heeds)),
                Script::Die { secs } => format!("{r} D {secs}"),
                Script::Hang => format!("{r} H"),
            });
        }
        out.join(" | ")
    }

    pub fn parse(s: &str) -> Option<Plan> {
        let mut parts = s.split('|').map(str::trim);
        let head: BTreeMap<&str, &str> = parts
            .next()?
            .split_whitespace()
            .filter_map(|kv| kv.split_once('='))
            .collect();
        let (a, b) = head.get("cfg")?.split_once(',')?;
        let mut p = Plan {
            seed: head.get("seed")?.parse().ok()?,
            cfg: (a.parse().ok()?, b.parse().ok()?),
            steps: head.get("steps")?.parse().ok()?,
            items: vec![],
            scripts: vec![],
        };
        for part in parts {
            let w: Vec<&str> = part.split_whitespace().collect();
            let n = |i: usize| w.get(i).and_then(|x| x.parse::<u64>().ok());
            let rest = || {
                w.get(2..)
                    .unwrap_or_default()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            };
            if let Some(at) = w.first().and_then(|x| x.strip_prefix('+')) {
                let t = T0 + at.parse::<u64>().ok()?;
                let e = match *w.get(1)? {
                    "T" => Ev::Timer {
                        every: n(2)?,
                        args: rest().split_off(1),
                    },
                    "R" => Ev::Run(rest()),
                    "L" => Ev::Load(n(2)?),
                    "C" => Ev::Crash(w.get(2)?.to_string()),
                    _ => return None,
                };
                p.items.push((t, e));
            } else {
                let f = |i: usize| n(i).map(|x| x == 1);
                let s = match *w.get(1)? {
                    "P" => Script::Push {
                        secs: n(2)?,
                        green: f(3)?,
                        ci: n(4),
                    },
                    "V" => Script::Review {
                        secs: n(2)?,
                        pass: f(3)?,
                        proof: f(4)?,
                        heeds: f(5)?,
                    },
                    "D" => Script::Die { secs: n(2)? },
                    "H" => Script::Hang,
                    _ => return None,
                };
                p.scripts.push((w.first()?.to_string(), s));
            }
        }
        Some(p)
    }

    /// One to three changes: a fault's time, a new or dropped fault, a lane's script, or the scheduler's ordering.
    pub fn mutate(&self, rng: &mut Rng) -> Plan {
        let mut p = self.clone();
        for _ in 0..rng.range(1, 3) {
            let faults: Vec<usize> = (0..p.items.len())
                .filter(|&i| !matches!(p.items[i].1, Ev::Timer { .. }))
                .collect();
            let pick = |rng: &mut Rng, v: &[usize]| v.get(rng.below(v.len() as u64) as usize).copied();
            match rng.below(6) {
                0 => {
                    if let Some(i) = pick(rng, &faults) {
                        let t = p.items[i].0 + rng.below(3600);
                        p.items[i].0 = t.saturating_sub(1800).max(T0);
                    }
                }
                1 => {
                    let t = T0 + rng.below(6 * 3600);
                    if rng.chance(50) {
                        p.items.push((t, Ev::Load(40)));
                        p.items.push((t + rng.range(120, 1200), Ev::Load(1)));
                    } else {
                        let kind = ["drive", "kick", "check"][rng.below(3) as usize];
                        p.items.push((t, Ev::Crash(kind.into())));
                    }
                }
                2 => {
                    if let Some(i) = pick(rng, &faults) {
                        p.items.remove(i);
                    }
                }
                3 | 4 => {
                    let role = ["impl", "rev"][rng.below(2) as usize];
                    let s = (role.to_string(), default_script(rng, role));
                    let at = rng.below(p.scripts.len() as u64 + 1) as usize;
                    if rng.chance(50) && at < p.scripts.len() {
                        p.scripts[at] = s;
                    } else {
                        p.scripts.insert(at, s);
                    }
                }
                _ => p.seed = rng.next() >> 16,
            }
        }
        p.items.sort_by_key(|x| x.0);
        p
    }
}

/// What a run reached: one `pass code kind step` per event (and per drive-pass action), and its peaks (supersedes, most spawns of one lane, the worst clock lag), each worse the higher.
pub fn coverage(w: &World) -> (BTreeSet<String>, Vec<u64>) {
    let s = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or("-").to_string();
    let (mut cov, mut spawns) = (BTreeSet::new(), BTreeMap::new());
    let (mut sup, mut lag) = (0, 0);
    let book = ["promise", "slo-idle", "slo-tick", "self-heal", "stuck", "slo-ok"];
    for x in w.wrote.iter().filter(|x| !book.contains(&x.ev.kind.as_str())) {
        let a = &w.procs[x.proc].args;
        let pass = a[..a.len().min(2)].join("-");
        let pass = if pass.ends_with("--fast") { pass } else { a[0].clone() };
        let (d, k) = (&x.ev.data, &x.ev.kind);
        cov.insert(format!("{pass} {} {k} {}", s(d.get("code")), s(d.get("step"))));
        for act in d.get("actions").and_then(Value::as_array).into_iter().flatten() {
            cov.insert(format!("{pass} {} action {}", s(act.get("code")), s(act.get("step"))));
        }
        sup += u64::from(k == "supersede");
        if k == "spawn" {
            *spawns.entry(s(d.get("agent"))).or_insert(0) += 1;
        }
        lag = lag.max(x.at.saturating_sub(x.ev.ts));
    }
    (cov, vec![sup, spawns.values().copied().max().unwrap_or(0), lag])
}

/// A failure the hunt found: the plan that failed, at which round, and its minimised form.
pub struct Found {
    pub round: usize,
    pub plan: Plan,
    pub small: Plan,
    pub failure: String,
}

impl Found {
    /// The seeds.rs line and the replay command.
    pub fn report(&self) -> String {
        let t = self.small.text();
        format!(
            "FAIL hunt round {}: {}\nminimised from {} to {} faults and scripts, {} to {} steps; for seeds.rs PLANS:\n    (\"{t}\", \"hunt: {}\"),\nreplay: DROVER_SIM_PLAN='{t}' cargo test -p drover --test sim\n",
            self.round,
            self.failure,
            self.plan.size(),
            self.small.size(),
            self.plan.steps,
            self.small.steps,
            self.failure.chars().take(80).collect::<String>()
        )
    }
}

fn code(f: &str) -> &str {
    f.split_whitespace().next().unwrap_or_default()
}

fn batch(plans: &[Plan], sabotage: Option<Sabotage>) -> Vec<Outcome> {
    let hs: Vec<_> = (plans.iter().cloned())
        .map(|p| std::thread::spawn(move || run_plan(&p, sabotage)))
        .collect();
    hs.into_iter().map(|h| h.join().expect("a run")).collect()
}

/// Drop faults and scripts one at a time while the plan still fails the same invariant, then bound its steps to the failing step.
pub fn minimise(plan: &Plan, failure: &str, steps: u64, sabotage: Option<Sabotage>) -> Plan {
    let mut p = Plan { steps, ..plan.clone() };
    let fails = |c: &Plan| {
        let o = run_plan(c, sabotage);
        (o.failure.as_deref().map(code) == Some(code(failure))).then_some(o.steps)
    };
    let mut i = p.items.len() + p.scripts.len();
    while i > 0 {
        i -= 1;
        let mut c = p.clone();
        match i.checked_sub(c.items.len()) {
            Some(j) => drop(c.scripts.remove(j)),
            None if matches!(c.items[i].1, Ev::Timer { .. }) => continue,
            None => drop(c.items.remove(i)),
        }
        if let Some(s) = fails(&c) {
            p = Plan { steps: s, ..c };
        }
    }
    p
}

pub struct Hunt {
    pub corpus: Vec<Plan>,
    pub found: Vec<Found>,
    pub rounds: usize,
}

/// Hunt from the seeds `start` (each counts as a round) with mutations drawn from `rng`, 8 rounds at a time, until `more(rounds, found)` says stop. Each invariant's first failure is minimised; a failing plan never joins the corpus.
pub fn hunt(start: &[u64], rng: u64, sabotage: Option<Sabotage>, more: impl Fn(usize, &[Found]) -> bool) -> Hunt {
    let mut rng = Rng::new(rng);
    let (mut seen, mut worst) = (BTreeSet::new(), vec![0; 3]);
    let mut h = Hunt {
        corpus: vec![],
        found: vec![],
        rounds: 0,
    };
    let mut next: Vec<Plan> = start.iter().map(|&s| Plan::of(s)).collect();
    while more(h.rounds, &h.found) {
        for (p, o) in next.iter().zip(batch(&next, sabotage)) {
            h.rounds += 1;
            if let Some(f) = o.failure {
                if !h.found.iter().any(|x| code(&x.failure) == code(&f)) {
                    let small = minimise(p, &f, o.steps, sabotage);
                    let found = Found {
                        round: h.rounds,
                        plan: p.clone(),
                        small,
                        failure: f,
                    };
                    eprintln!("{}", found.report());
                    h.found.push(found);
                }
                continue;
            }
            let new = o.cov.difference(&seen).count();
            let worse = o.peaks.iter().zip(&worst).any(|(m, w)| m > w);
            if new > 0 || worse {
                seen.extend(o.cov);
                worst = worst.iter().zip(&o.peaks).map(|(a, b)| *a.max(b)).collect();
                h.corpus.push(p.clone());
            }
        }
        if h.corpus.is_empty() {
            h.corpus.push(Plan::of(start.first().copied().unwrap_or(2)));
        }
        next = (0..8)
            .map(|_| {
                let i = rng.below(h.corpus.len() as u64) as usize;
                h.corpus[i].mutate(&mut rng)
            })
            .collect();
    }
    h
}
