use crate::model::{Ev, Model, Pr, Script, FLEET, REPO};
use crate::sched::{Rng, Sim, World};
use drover::Ctx;
use std::path::Path;
use std::sync::Arc;

/// 2026-11-04 22:11:25, the incident's frozen `ts`.
pub const T0: u64 = 1_793_805_085;

pub fn config(max_spawns: u64, rounds: u64) -> String {
    format!(
        r#"[fleet.{FLEET}]
repo = "{REPO}"
local = "LOCAL"
integration = "staging"

[fleet.{FLEET}.role.impl]
harness = "claude"
profile = "fleet-impl"

[fleet.{FLEET}.role.rev]
harness = "claude"
profile = "fleet-rev"

[limits]
review_rounds = {rounds}
stall_minutes = 30
max_parallel_spawns = {max_spawns}
build_root = ""
stray_roots = []
lessons = "off"
"#
    )
}

/// The process's view of the world's files (one per process: its `Ctx.now` is its start).
pub fn ctx(root: &Path, now: u64) -> Ctx {
    Ctx {
        config: root.join("config/fleet.toml"),
        state: root.join("state/events.jsonl"),
        worktrees: root.join("wt"),
        homes: [("hermes", root.join("hermes"))].into(),
        exe: "/opt/drover/bin/drover".into(),
        now,
        home: root.join("home"),
        hints: vec![],
    }
}

/// Write `cfg` and the files a spawn needs (Claude profiles, the fleet's local checkout).
pub fn setup(root: &Path, cfg: &str) {
    let local = root.join("repo").join(FLEET);
    std::fs::create_dir_all(local.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("config")).unwrap();
    let cfg = cfg.replace("LOCAL", &local.display().to_string());
    std::fs::write(root.join("config/fleet.toml"), cfg).unwrap();
    for p in ["fleet-impl", "fleet-rev"] {
        let d = root.join("config/claude").join(p);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("settings.json"), "{}").unwrap();
        std::fs::write(d.join("role.md"), "role").unwrap();
    }
}

/// The three timers launchd runs, each first at a seeded offset.
pub fn timers(m: &mut Model, rng: &mut Rng, start: u64) {
    for (args, every) in [(&["drive"][..], 300), (&["drive", "--fast"], 60), (&["check"], 300)] {
        let args = args.iter().map(ToString::to_string).collect();
        m.at(start + rng.below(every), Ev::Timer { args, every });
    }
}

pub fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(ToString::to_string).collect()
}

/// The first agenda item's time.
pub fn next(w: &World) -> Option<u64> {
    w.model.agenda.keys().next().map(|k| k.0)
}

/// Apply every agenda item due by now.
pub fn due(sim: &Arc<Sim>, w: &mut World) {
    while let Some((&(t, s), _)) = w.model.agenda.iter().next() {
        if t > w.now {
            break;
        }
        let e = w.model.agenda.remove(&(t, s)).unwrap();
        w.log(format!("event {e:?}").chars().take(200).collect());
        match e {
            Ev::Timer { args, every } => {
                let a: Vec<&str> = args.iter().map(String::as_str).collect();
                sim.spawn(w, &args.join("-"), &a);
                let next = w.now + every - 10 + w.rng.below(21);
                w.model.at(next, Ev::Timer { args, every });
            }
            Ev::Run(args) => {
                let a: Vec<&str> = args.iter().map(String::as_str).collect();
                sim.spawn(w, a.first().copied().unwrap_or("run"), &a);
            }
            Ev::Lane { pane, step } => lane(sim, w, &pane, step),
            Ev::Ci { sha, green } => {
                let since = w.model.ci_since.remove(&sha).unwrap_or(w.now);
                w.model.ci_wait += w.now - since;
                w.model.ci.insert(sha, Some(green));
                w.model.version += 1;
            }
            Ev::Load(l) => w.model.load = l,
            Ev::Crash(kind) => {
                let live: Vec<usize> = (0..w.procs.len())
                    .filter(|&i| w.procs[i].live() && w.procs[i].started.is_some() && w.procs[i].args[0] == kind)
                    .collect();
                if !live.is_empty() {
                    let i = live[w.rng.below(live.len() as u64) as usize];
                    w.procs[i].killed = true;
                    let name = w.procs[i].name.clone();
                    w.log(format!("crash {name}"));
                }
            }
        }
    }
}

fn issue_of(agent: &str) -> u64 {
    agent.split('-').nth(2).and_then(|n| n.parse().ok()).unwrap_or(0)
}

fn lane(sim: &Arc<Sim>, w: &mut World, id: &str, step: u8) {
    let now = w.now;
    let Some(i) = w
        .model
        .panes
        .iter()
        .position(|p| p.id == id && !p.closed && p.agent.is_some())
    else {
        return;
    };
    let (cwd, script) = (w.model.panes[i].cwd.clone(), w.model.panes[i].script.clone());
    let n = issue_of(w.model.panes[i].agent.as_deref().unwrap_or_default());
    match (script, step) {
        (Some(Script::Push { secs, .. } | Script::Review { secs, .. }), 0) => {
            w.model.at(
                now + secs,
                Ev::Lane {
                    pane: id.into(),
                    step: 1,
                },
            );
        }
        (Some(Script::Die { secs }), 0) => w.model.at(
            now + secs,
            Ev::Lane {
                pane: id.into(),
                step: 9,
            },
        ),
        (Some(Script::Die { .. }), 9) => {
            w.model.panes[i].closed = true;
            let a = w.model.panes[i].agent.clone().unwrap_or_default();
            w.model.died.push(a);
        }
        (Some(Script::Push { green, ci, .. }), 1) => {
            let sha = w.model.sha();
            let branch = cwd.rsplit('/').next().unwrap_or_default().to_string();
            w.model.heads.insert(cwd.clone(), sha.clone());
            let on = w.model.origin.get("staging").cloned().unwrap_or_default();
            w.model.base.insert(sha.clone(), on);
            w.model.origin.insert(branch.clone(), sha.clone());
            let pr = w.model.prs.entry(branch).or_insert(Pr {
                number: n,
                head: String::new(),
                base: "staging".into(),
                state: "OPEN".into(),
                merged: None,
            });
            pr.head = sha.clone();
            w.model.version += 1;
            let ci = w.model.ci_start(&sha, now, ci, &mut w.rng);
            w.model.produced.push(id.into());
            w.model.at(ci, Ev::Ci { sha, green });
            stop(sim, w, i, n);
        }
        (_, 3) => stop(sim, w, i, n),
        (Some(Script::Review { pass, proof, .. }), 1 | 2) => {
            let head = w.model.heads.get(&cwd).cloned().unwrap_or_default();
            let fixed = step == 2 || proof;
            let body = match (pass, fixed) {
                (true, _) => format!("PASS\n\nReviewed head: {head}\n\nAll criteria met.\n"),
                (false, false) => format!("FAIL\n\nReviewed head: {head}\n\n- blocking [correctness] src/lib.rs:10: wrong\n"),
                (false, true) => format!(
                    "FAIL\n\nReviewed head: {head}\n\n- blocking [correctness] src/lib.rs:10: wrong\n  Proof: spec\n  > AC 1: it works\n  src/lib.rs:10\n"
                ),
            };
            if pass || fixed {
                w.model.produced.push(id.into());
            }
            let f = Path::new(&cwd).join("verdict.md");
            std::fs::write(&f, body).unwrap();
            let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(now);
            let _ = std::fs::File::options()
                .write(true)
                .open(&f)
                .and_then(|f| f.set_modified(t));
            stop(sim, w, i, n);
        }
        _ => {}
    }
}

fn stop(sim: &Arc<Sim>, w: &mut World, i: usize, n: u64) {
    w.model.panes[i].status = "done".into();
    let ns = n.to_string();
    sim.spawn(w, "hook", &["kick", FLEET, &ns, "--why", "stop"]);
}

/// Pane `id` was sent a prompt: it works on it. A reviewer told its FAIL lacks Proof adds it within 30 s if it `heeds`, else writes the same verdict again; any other prompt is answered and the agent stops.
pub fn told(m: &mut Model, id: &str, now: u64) {
    let Some(p) = m
        .panes
        .iter_mut()
        .find(|p| p.id == id && !p.closed && p.agent.is_some())
    else {
        return;
    };
    p.status = "working".into();
    let proof = p.told.last().is_some_and(|t| t.contains("NO_PROOF"));
    let (at, step) = match p.script {
        Some(Script::Review { heeds: true, .. }) if proof => (30, 2),
        Some(Script::Review { heeds: false, .. }) if proof => (5, 1),
        _ => (30, 3),
    };
    m.at(now + at, Ev::Lane { pane: id.into(), step });
}

/// A lane's script when the scenario queued none for its role.
pub fn default_script(rng: &mut Rng, role: &str) -> Script {
    match (role, rng.below(10)) {
        (_, 0) => Script::Die {
            secs: rng.range(60, 1800),
        },
        (_, 1) => Script::Hang,
        ("rev", k) => Script::Review {
            secs: rng.range(60, 1200),
            pass: k >= 5,
            proof: k % 2 == 0,
            heeds: k != 3,
        },
        _ => Script::Push {
            secs: rng.range(120, 2400),
            green: rng.chance(80),
            ci: None,
        },
    }
}

/// A seed's own fleet: 2–6 issues queued over the first hour (some blocked on another for a while), lanes drawn from `default_script`, CI green or red, load spikes, crashed passes, and the owner's `drive` and `merge` by hand. `Model` and `config`'s (max_spawns, rounds).
pub fn random(seed: u64) -> (Model, (u64, u64)) {
    let mut rng = Rng::new(seed ^ 0x5eed);
    let mut m = Model::default();
    let s = m.sha();
    m.origin.insert("staging".into(), s);
    timers(&mut m, &mut rng, T0 + 1);
    let issues: Vec<u64> = (0..rng.range(2, 6)).map(|i| 101 + i).collect();
    for &n in &issues {
        let t = T0 + rng.below(3600);
        m.at(t, Ev::Run(args(&["queue", FLEET, "add", &n.to_string()])));
    }
    if issues.len() > 2 && rng.chance(40) {
        let (a, b) = (issues[0].to_string(), issues[1].to_string());
        m.at(T0 + 3601, Ev::Run(args(&["queue", FLEET, "block", "--on", &a, &b])));
        m.at(
            T0 + 3601 + rng.range(600, 7200),
            Ev::Run(args(&["queue", FLEET, "unblock", &b])),
        );
    }
    for _ in 0..rng.below(3) {
        let t = T0 + rng.below(5 * 3600);
        m.at(t, Ev::Load(40));
        m.at(t + rng.range(120, 1200), Ev::Load(1));
    }
    for _ in 0..rng.below(4) {
        let t = T0 + rng.below(6 * 3600);
        m.at(t, Ev::Run(args(&["drive"])));
        let n = issues[rng.below(issues.len() as u64) as usize].to_string();
        m.at(T0 + 3600 + rng.below(5 * 3600), Ev::Run(args(&["merge", FLEET, &n])));
    }
    for _ in 0..rng.below(3) {
        let kind = ["drive", "kick"][rng.below(2) as usize];
        m.at(T0 + rng.below(5 * 3600), Ev::Crash(kind.into()));
    }
    (m, (rng.range(2, 5), rng.range(2, 4)))
}
