use crate::sched::World;
use drover::events::Event;
use std::collections::BTreeMap;

/// Test-only break switches (AC 3): each makes the model break one invariant on purpose.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sabotage {
    DoubleLane,
    RedMerge,
    Planted,
}

/// I7: an event may be this far behind the true time it is appended at (owner, q836).
pub const LAG: u64 = 60;
/// I6: a queued, unblocked issue with room gets a lane within this many full drive passes (owner, q838).
pub const PASSES: usize = 3;
/// I9: spawns of one lane name allowed within a sim hour without a new head, verdict, owner event or injected death.
pub const CHURN: usize = 3;

fn agent(e: &Event) -> Option<&str> {
    e.str("agent")
}

/// (issue, role) of a lane name `<fleet>-<role>-<n>[-r<k>]`: a rev lane counts once, whatever its round.
pub fn lane_key(name: &str) -> Option<(u64, String)> {
    let mut w = name.split('-').skip(1);
    let role = w.next()?.to_string();
    Some((w.next()?.parse().ok()?, role))
}

/// Each append, as it happens: I2, I3, I7, I9, I10.
pub fn on_append(w: &mut World, me: usize) {
    let i = w.wrote.len() - 1;
    let (at, ev) = (w.wrote[i].at, w.wrote[i].ev.clone());
    let who = w.procs[me].name.clone();
    if ev.ts > at || at - ev.ts > LAG {
        let lag = at as i64 - ev.ts as i64;
        return w.fail(format!(
            "I7 clock: {who} wrote {} #{} ts=t+{} at true t+{} (lag {lag} s > {LAG} s)",
            ev.kind,
            ev.issue.unwrap_or(0),
            ev.ts - crate::scenario::T0,
            at - crate::scenario::T0
        ));
    }
    let n = ev.issue.unwrap_or(0);
    let before = &w.wrote[..i];
    match ev.kind.as_str() {
        "supersede" => {
            let a = agent(&ev).unwrap_or_default();
            let again = before
                .iter()
                .any(|x| x.proc == me && x.ev.kind == "supersede" && agent(&x.ev) == Some(a) && x.ev.ts == ev.ts);
            let spawned = before
                .iter()
                .rfind(|x| x.ev.kind == "spawn" && agent(&x.ev) == Some(a))
                .map(|x| x.at);
            if again {
                return w.fail(format!(
                    "I2 supersede-once: {a} superseded twice by {who}'s pass at ts=t+{}",
                    ev.ts - crate::scenario::T0
                ));
            }
            if let Some(s) = spawned.filter(|s| ev.ts < *s) {
                let t0 = crate::scenario::T0;
                return w.fail(format!(
                    "I2 supersede-after-spawn: {who} (pass at t+{}) superseded {a}, spawned at true t+{}",
                    ev.ts - t0,
                    s - t0
                ));
            }
        }
        "done" => {
            let id = ev.str("action_id").unwrap_or_default();
            if before
                .iter()
                .any(|x| x.ev.kind == "done" && x.ev.str("action_id") == Some(id))
            {
                return w.fail(format!("I3 action-once: {id} done twice (again by {who})"));
            }
        }
        "spawn" => {
            let a = agent(&ev).unwrap_or_default();
            let reason = |x: &Event| {
                x.issue == ev.issue
                    && (matches!(
                        x.kind.as_str(),
                        "pushed" | "verdict" | "queue" | "decision" | "round-extended"
                    ) || x.kind == "lane-dead" && agent(x) == Some(a))
            };
            let from = before.iter().rposition(|x| reason(&x.ev)).map_or(0, |p| p + 1);
            let k = (before[from..].iter())
                .filter(|x| x.ev.kind == "spawn" && agent(&x.ev) == Some(a) && x.at + 3600 > at)
                .count();
            if k + 1 > CHURN && !w.model.died.contains(&a.to_string()) {
                return w.fail(format!(
                    "I9 churn: {a} spawned {} times within an hour, nothing new in between",
                    k + 1
                ));
            }
        }
        _ => {}
    }
    let acts = ["spawn", "spawn-started", "supersede", "respawn"];
    if n != 0
        && acts.contains(&ev.kind.as_str())
        && before.iter().any(|x| x.ev.kind == "merged" && x.ev.issue == Some(n))
    {
        w.fail(format!("I10 merged: {who} logged {} on merged #{n}", ev.kind));
    }
}

/// I4, as the model's `gh pr merge` is accepted: the base is integration, the latest verdict at the exact head is PASS, and the head's CI is green.
pub fn on_merge(w: &mut World, issue: u64, head: &str, base: &str) {
    if w.model.sabotage == Some(Sabotage::RedMerge) {
        w.model.ci.insert(head.into(), Some(false));
    }
    let v = (w.wrote.iter().rev())
        .find(|x| x.ev.kind == "verdict" && x.ev.issue == Some(issue) && x.ev.str("sha") == Some(head))
        .and_then(|x| x.ev.str("verdict").map(String::from));
    let green = w.model.ci.get(head).copied().flatten() == Some(true);
    if base != "staging" || v.as_deref() != Some("PASS") || !green {
        w.fail(format!(
            "I4 merge: #{issue} merged into {base} at {head:.7}: verdict {v:?}, CI green {green}"
        ));
    }
}

/// After every step: I1, I5 (at quiet points), I6, and the merged event's verdict sha (I4).
pub fn after_step(w: &mut World) {
    if w.failure.is_some() {
        return;
    }
    let mut by: BTreeMap<(u64, String), Vec<String>> = BTreeMap::new();
    for p in w.model.live().filter(|p| !matches!(p.status.as_str(), "dead" | "done")) {
        if let Some(k) = p.agent.as_deref().and_then(lane_key) {
            by.entry(k)
                .or_default()
                .push(format!("{} on {}", p.agent.clone().unwrap_or_default(), p.id));
        }
    }
    if let Some((k, v)) = by.iter().find(|(_, v)| v.len() > 1) {
        w.fail(format!(
            "I1 one-lane: #{} {} has {} live lanes: {v:?}",
            k.0,
            k.1,
            v.len()
        ));
    }
    let now = (w.wrote.len(), w.model.merges.len());
    let fresh = std::mem::replace(&mut w.checked, now) != now;
    let bad: Vec<String> = (w.wrote.iter())
        .filter(|x| fresh && x.ev.kind == "merged")
        .filter_map(|x| {
            let n = x.ev.issue.unwrap_or(0);
            let merged = w.model.merges.iter().rfind(|m| m.issue == n).map(|m| m.head.clone());
            let named = x.ev.str("verdict_sha");
            (merged.as_deref() != named)
                .then(|| format!("I4 merged-sha: #{n} merged event names {named:?}, the model merged {merged:?}"))
        })
        .collect();
    for why in bad {
        w.fail(why);
    }
    if w.procs.iter().all(|p| !p.live() || p.starting.is_none()) {
        let orphan =
            (w.model.live()).find(|p| !w.spawned.contains(&(p.agent.clone().unwrap_or_default(), p.id.clone())));
        if let Some(p) = orphan {
            let why = format!("I5 orphan: pane {} runs {:?}, no spawn logged it", p.id, p.agent);
            w.fail(why);
        }
    }
    if !w.procs.iter().any(|p| p.killed) {
        if let Some(x) = w.wrote.iter().find(|x| x.ev.kind == "spawn-adopted") {
            let why = format!("I5 adopt: {:?} adopted with no process crashed", agent(&x.ev));
            w.fail(why);
        }
    }
    if fresh {
        liveness(w);
    }
}

fn liveness(w: &mut World) {
    let mut queued: BTreeMap<u64, usize> = BTreeMap::new();
    for (i, x) in w.wrote.iter().enumerate() {
        let e = &x.ev;
        match e.kind.as_str() {
            "queue" => {
                for n in e.data.get("issues").and_then(|v| v.as_array()).into_iter().flatten() {
                    let n = n.as_u64().unwrap_or(0);
                    match e.str("op") {
                        Some("add") => drop(queued.entry(n).or_insert(i)),
                        _ => drop(queued.remove(&n)),
                    }
                }
            }
            "spawn" | "spawn-started" => {
                queued.remove(&e.issue.unwrap_or(0));
            }
            _ => {}
        }
    }
    for (n, from) in queued {
        let full: Vec<String> = (w.wrote[from..].iter())
            .filter(|x| x.ev.kind == "drive-pass" && x.ev.data.get("by").and_then(|b| b.as_str()) == Some("auto"))
            .filter(|x| x.ev.data.get("kick").is_none_or(serde_json::Value::is_null))
            .filter(|x| {
                let acts = x.ev.data.get("actions").and_then(|a| a.as_array());
                let room = |a: &serde_json::Value| {
                    ["HOST_BUSY", "DISK_LOW", "MEM_LOW"].contains(&a["code"].as_str().unwrap_or(""))
                };
                let start = |a: &serde_json::Value| a["step"] == "start" && a["code"].is_null();
                !x.busy
                    && !acts
                        .into_iter()
                        .flatten()
                        .any(|a| a["issue"] == n && !a["code"].is_null() || room(a) || start(a))
            })
            .map(|x| {
                format!(
                    "t+{} {}",
                    x.ev.ts - crate::scenario::T0,
                    x.ev.data.get("actions").map(ToString::to_string).unwrap_or_default()
                )
            })
            .collect();
        if full.len() >= PASSES {
            return w.fail(format!(
                "I6 liveness: #{n} queued, no lane after {} full drive passes: {full:?}",
                full.len()
            ));
        }
    }
}

/// I8, as process `me` exits: it holds no lease (each guard released it; markers it handed on carry another pid). A `.again` flag is a message to the busy kick, not a hold.
pub fn on_exit(w: &mut World, me: usize, pid: u32) {
    let state = crate::scenario::ctx(&w.root, 0).state;
    let held: Vec<String> = (drover::db::leases(&state, "").into_iter())
        .filter(|l| l.pid == pid && l.expires_at > w.now && !l.key.ends_with(".again"))
        .map(|l| l.key)
        .collect();
    if !held.is_empty() {
        let who = w.procs[me].name.clone();
        w.fail(format!("I8 lease: {who} exited holding {held:?}"));
    }
}

/// `Sabotage::DoubleLane`: a copy of the pane whose agent just started comes up beside it.
pub fn double(w: &mut World, pane: &str) {
    if let Some(p) = w.model.panes.iter().find(|p| p.id == pane).cloned() {
        w.model.panes.push(crate::model::Pane {
            id: format!("{}x", p.id),
            ..p
        });
    }
}
