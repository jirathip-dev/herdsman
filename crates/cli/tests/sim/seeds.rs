use crate::model::{Ev, Model, Script, FLEET};
use crate::scenario::{args, config, timers, T0};
use crate::sched::Rng;

/// Seed 1 (#370, 2026-11-04 22:11:25): a reviewer's FAIL without Proof blocks, refused NO_PROOF while one kick runs pass after pass on its start's `Ctx.now`. The reviewer ignores the prompt; every reviewer after it writes the same FAIL within seconds (and adds the Proof once told). Before #680's fix the kick superseded each new reviewer at once: 25 supersedes stamped with one second.
pub const INCIDENT: u64 = 1;

/// Seeds that found a cause of their own, parked `#[ignore]` (`parked_seeds`) until its issue's fix unparks them.
pub const PARKED: &[(u64, &str)] = &[
    (
        2335,
        "lock wait: a kick waits 120 s in drive::update on a crashed pass's drive.lock (held() ignores a dead holder)",
    ),
    (
        1069,
        "NO_PROOF re-review loop: a reviewer that never adds its Proof is told once, then superseded, without end",
    ),
];

/// #688: plans a hunt found (`Plan::text`, as its failure printed it), each joining with its fix like a seed.
pub const PLANS: &[(&str, &str)] = &[];

/// Seeds 2–24: random fleets (`scenario::random`).
pub const FIXED: &[(u64, &str)] = &[
    (INCIDENT, "#370: repeated supersede under a kick's frozen clock"),
    (2, "random"),
    (3, "random"),
    (4, "random"),
    (5, "random"),
    (6, "random"),
    (7, "random (also the determinism seed)"),
    (8, "random"),
    (9, "random"),
    (10, "random"),
    (11, "random"),
    (12, "random"),
    (13, "random"),
    (14, "random"),
    (15, "random"),
    (16, "#854: the fast step no longer logs a merge in flight as external"),
    (17, "random"),
    (18, "random"),
    (19, "random"),
    (20, "random"),
    (21, "random"),
    (22, "random"),
    (23, "random"),
    (24, "random"),
    (
        1382,
        "#854: an owner `merge` refused NO_PR before any lane no longer strands the queued issue",
    ),
];

/// Seed 1's fleet: #370 queued, its impl pushes, its first reviewer writes a FAIL without Proof and ignores the refusal; each reviewer after it writes the same FAIL 5 s in.
pub fn incident() -> (Model, (u64, u64)) {
    let mut rng = Rng::new(INCIDENT);
    let mut m = Model::default();
    let s = m.sha();
    m.origin.insert("staging".into(), s);
    timers(&mut m, &mut rng, T0 + 5);
    m.at(T0, Ev::Run(args(&["queue", FLEET, "add", "370"])));
    let first = Script::Review {
        secs: 60,
        pass: false,
        proof: false,
        heeds: false,
    };
    let again = Script::Review {
        secs: 5,
        pass: false,
        proof: false,
        heeds: true,
    };
    let mut rev = vec![first];
    rev.extend(std::iter::repeat_n(again, 40));
    m.scripts.insert("rev".into(), rev);
    let push = Script::Push {
        secs: 200,
        green: true,
        ci: None,
    };
    m.scripts.insert("impl".into(), vec![push; 6]);
    (m, (4, 3))
}

/// #685's known-answer fleet: #201 and #202 queued together at t+0; every impl works 600 s and pushes a green head, every head's CI takes 300 s, the host never busy. The first reviewer writes a FAIL without Proof 300 s in and ignores the refusal (a wasted spawn, superseded once); every reviewer after it writes PASS 300 s in.
pub fn known() -> (Model, String) {
    let mut rng = Rng::new(KNOWN);
    let mut m = Model::default();
    let s = m.sha();
    m.origin.insert("staging".into(), s);
    m.load = 1;
    m.ci_secs = Some(300);
    timers(&mut m, &mut rng, T0 + 1);
    m.at(T0, Ev::Run(args(&["queue", FLEET, "add", "201", "202"])));
    let push = Script::Push {
        secs: 600,
        green: true,
        ci: None,
    };
    m.scripts.insert("impl".into(), vec![push; 6]);
    let bad = Script::Review {
        secs: 300,
        pass: false,
        proof: false,
        heeds: false,
    };
    let pass = Script::Review {
        secs: 300,
        pass: true,
        proof: true,
        heeds: true,
    };
    let mut rev = vec![bad];
    rev.extend(std::iter::repeat_n(pass, 6));
    m.scripts.insert("rev".into(), rev);
    (m, config(4, 3))
}

/// The known-answer fleet's seed (its scheduler's choices and timer offsets).
pub const KNOWN: u64 = 685;
