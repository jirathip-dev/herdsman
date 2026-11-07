//! `herdsman check` (alerts, each once per state) and `herdsman clean` (reclaim merged or closed lanes).

use crate::config::{Config, Fleet};
use crate::events::{age, emit, fold, held_on, last_spawn, open_asks, plain, read, Event, IssueState};
use crate::gate::gate;
use crate::host;
use crate::hosts::On;
use crate::runner::{ok, Runner};
use crate::spawn::{changes, dirty, OWN_FILES, SHOT_DIRS};
use crate::Ctx;
use crate::{gh, herdr};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;

/// A ready issue's open PR.
fn ready<'a>(ctx: &Ctx, r: &dyn Runner, cfg: &'a Config, s: &IssueState) -> Option<(&'a Fleet, Value)> {
    let f = cfg.fleet(&s.fleet).ok().filter(|_| s.state == "ready")?;
    let branch = crate::gate::branch_of(Some(s), s.issue);
    let pr = gh::pr(ctx, r, f, (&s.fleet, s.issue), &branch, ctx.now).ok()?;
    (pr["state"] == "OPEN").then_some((f, pr))
}

/// `(url, prompt)` when the issue's latest amendment is missing from the lane's `.brief.md` (#101).
fn amendment(ctx: &Ctx, on: &On, cfg: &Config, s: &IssueState, wt: &str) -> Option<(String, String)> {
    let (f, r) = (cfg.fleet(&s.fleet).ok()?, on.r);
    let brief = on.read(&Path::new(wt).join(".brief.md"))?;
    let spec = gh::spec(ctx, r, f, &s.fleet, s.issue, true).ok()?;
    let url = spec["amended"].as_str().filter(|u| !brief.contains(*u))?;
    let n = s.issue;
    let fleet = &s.fleet;
    let line = format!("AMENDMENT on #{n} ({url}): run `herdsman brief {fleet} {n}` and apply it");
    Some((url.to_string(), line))
}

/// Each alert's dedup key is built from the issue's last activity: once per state, again after a change.
pub fn check(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let events = read(&ctx.state)?;
    PUSHES.set((0, 0));
    let _ = std::fs::write(ctx.state.with_file_name("check.at"), ctx.now.to_string());
    let agents = host::agents(r)
        .map_err(|e| eprintln!("herdsman check: herdr agent list failed ({e:#}); skipping lane checks"))
        .ok();
    if let Some(a) = &agents {
        crate::conductor::sleep_idle(ctx, cfg, r, &events, a)?; // #355
    }
    let stall = cfg.limits.stall_minutes * 60;
    // #157: each host's agents; a lane on an unreachable host is skipped (it never pages).
    let remote = crate::hosts::survey(ctx, cfg, r, false);
    crate::cloud::tick(ctx, cfg, r, &remote, out)?;
    // #478: a remote host's drop and its return alert once each; its lanes show `host offline` meanwhile.
    for (name, a) in &remote {
        let stopped = || crate::cloud::state(&events, name) == "stopped";
        if cfg
            .hosts
            .get(name)
            .is_none_or(|h| h.machine.is_none() || h.cloud.is_some() && stopped())
        {
            continue;
        }
        let was = |e: &&Event| e.str("host") == Some(name) && matches!(e.kind.as_str(), "host-offline" | "host-online");
        let last = events.iter().rev().find(was);
        if a.is_none() != last.is_some_and(|e| e.kind == "host-offline") {
            let kind = ["host-online", "host-offline"][usize::from(a.is_none())];
            let lanes = crate::hosts::lanes_on(cfg, &events, name);
            let why = format!("{name} {}; lanes: {}", &kind[5..], lanes.join(", "));
            let d = json!({"key": last.map_or(0, |e| e.ts), "host": name, "reason": why});
            fire(ctx, cfg, out, &events, ("herdsman", None), kind, d)?;
        }
    }
    crate::roots::watch(ctx, cfg, r, out)?; // #630
    crate::setup::reap(ctx, r, &events)?;
    // #537: CI runners below `slots × VMs` alert once per drop, `runners-ok` once on the way back.
    match crate::runners::tick(ctx, cfg, r, &events) {
        Some(Ok((on, want))) => {
            let was = |e: &&Event| matches!(e.kind.as_str(), "runners-low" | "runners-ok");
            let last = events.iter().rev().find(was);
            if (on < want) != last.is_some_and(|e| e.kind == "runners-low") {
                let kind = ["runners-ok", "runners-low"][usize::from(on < want)];
                let d = json!({"key": last.map_or(0, |e| e.ts), "reason": format!("CI runners {on}/{want} online")});
                fire(ctx, cfg, out, &events, ("herdsman", None), kind, d)?;
            }
        }
        Some(Err(e)) => eprintln!("herdsman check: CI runners: {e:#}"),
        None => {}
    }
    // #539: a retired config key still set alerts once per key.
    for (k, w) in cfg.warnings() {
        let d = json!({"key": k, "reason": format!("{k} is {w}")});
        fire(ctx, cfg, out, &events, ("herdsman", None), "config-warning", d)?;
    }
    for (name, f) in &cfg.fleet {
        let _ = crate::overlap::preview(ctx, cfg, r, name, &events);
        // #429: integration within `warn_within` of its line cap: once per integration sha.
        if let Some((sha, n, cap, warn)) = crate::overlap::cap_at(ctx, cfg, r, name).filter(|c| c.2 - c.1 < c.3) {
            let why = format!(
                "{} lines of headroom (< {warn}) at {sha:.7}, {n} / cap {cap}: propose the next cap step",
                cap - n
            );
            fire(
                ctx,
                cfg,
                out,
                &events,
                (name, None),
                "cap-near",
                json!({"key": sha, "reason": why}),
            )?;
        }
        // #331: a queued issue never spawned had no cached spec, so no Queue row title: fetch it once (cached after).
        for i in crate::queue::replay(&events, name) {
            let _ = gh::spec(ctx, r, f, name, i.issue, false);
        }
    }
    // #333: a crashed kick's marker or lock goes on the next check; an issue SPAWN_PENDING has held back in every
    // pass for 15 min alerts once per episode (its fleet's earliest hold is the key).
    crate::drive::markers(ctx, cfg, r, true);
    let mut held = BTreeMap::<(&str, u64), (u64, u64)>::new();
    for e in events.iter().filter(|e| e.kind == "drive-pass") {
        let acts = e.data.get("actions").and_then(serde_json::Value::as_array);
        for a in acts.into_iter().flatten() {
            let Some(n) = a["issue"].as_u64() else { continue };
            if a["code"] == "SPAWN_PENDING" {
                held.entry((&e.fleet, n)).or_insert((e.ts, e.ts)).1 = e.ts;
            } else {
                held.remove(&(e.fleet.as_str(), n));
            }
        }
    }
    held.retain(|_, (since, last)| ctx.now >= *since + 900 && ctx.now < *last + 900);
    for name in cfg.fleet.keys() {
        let mine: Vec<_> = held.iter().filter(|h| h.0 .0 == name).collect();
        let Some(since) = mine.iter().map(|h| h.1 .0).min() else {
            continue;
        };
        let ns: Vec<String> = mine.iter().map(|h| format!("#{}", h.0 .1)).collect();
        let why = format!(
            "{} held back by SPAWN_PENDING for {}",
            ns.join(", "),
            age(ctx.now - since)
        );
        let d = json!({"key": since, "reason": why});
        fire(ctx, cfg, out, &events, (name, None), "spawn-held", d)?;
    }
    // #604: every start a pass logged refused by one host gate (code and host) for 30 min alerts once per episode (its
    // first such pass is the key); a start that ran, or another refusal, ends it. A kick still pending says nothing.
    let mut busy: Option<(u64, u64, Gate)> = None;
    for e in events.iter().filter(|e| e.kind == "drive-pass") {
        let acts = e.data.get("actions").and_then(serde_json::Value::as_array);
        let mut starts = acts
            .into_iter()
            .flatten()
            .filter(|a| a["step"] == "start" && a["outcome"] != "pending");
        let Some(first) = starts.next() else { continue };
        let same = |a: Gate, b: Gate| a.0 == b.0 && a.1 == b.1;
        busy = match (host_gate(first), busy) {
            (Some(g), b) if starts.all(|a| host_gate(a).is_some_and(|h| same(g, h))) => match b {
                Some((since, _, h)) if same(g, h) => Some((since, e.ts, g)),
                _ => Some((e.ts, e.ts, g)),
            },
            _ => None,
        };
    }
    if let Some((since, _, (_, host, why))) = busy.filter(|b| ctx.now >= b.0 + 1800 && ctx.now < b.1 + 900) {
        let d = json!({"key": since, "host": host, "reason": why});
        fire(ctx, cfg, out, &events, ("herdsman", None), "host-busy", d)?;
    }
    // #424: an answer that never reached its lane alerts once per ask (drive keeps retrying it); a day on, never.
    let undelivered = cfg.fleet.keys().flat_map(|f| crate::undelivered(&events, f));
    for u in undelivered.filter(|u| u.ts + 86_400 > ctx.now) {
        let mut d = u.data.clone();
        d.insert("key".into(), json!(u.str("id")));
        let to = (u.fleet.as_str(), u.issue);
        fire(ctx, cfg, out, &events, to, "answer-undelivered", d.into())?;
    }
    for s in fold(&events).values().filter(|s| !s.done()) {
        let idle = ctx.now.saturating_sub(s.last_ts);
        let mut hits = vec![];
        let mut capped = None;
        if let (Some(agents), Some((role, lane))) = (&agents, s.active_lane()) {
            // Agents that report their own state (dsh-TUI) can lose the name: match the lane's pane.
            let spawned = last_spawn(&events, lane);
            let r = &On::of(r, spawned);
            let there = spawned.filter(|_| r.machine.is_some()).and_then(|e| e.str("host"));
            let theirs = there.map(|h| remote.get(h).cloned().flatten());
            let Some(agents) = theirs.as_ref().map_or(Some(agents), Option::as_ref) else {
                continue;
            };
            let pane = spawned.and_then(|e| e.str("pane"));
            let unnamed =
                |a: &&serde_json::Value| a["name"].as_str().is_none_or(str::is_empty) && a["pane_id"].as_str() == pane;
            let agent = agents
                .iter()
                .find(|a| a["name"] == lane)
                .or_else(|| agents.iter().find(unnamed));
            let status = agent.map(|a| a["agent_status"].as_str().unwrap_or("unknown"));
            // #83: a gone lane with a clean, pushed worktree is parked: one `lane-dead`, never a stall.
            let wt = spawned.and_then(|e| e.str("worktree"));
            let parked = status.is_none() && wt.is_some_and(|wt| dirty(r, Path::new(wt)).is_none());
            let key = if parked {
                format!("parked:{lane}")
            } else {
                format!("{}:{lane}", s.last_ts)
            };
            // #248: a lane herdsman closed once its handoff was logged is not gone.
            let mine = |e: &Event, sp| e.kind == "lane-closed" && crate::events::ends(e, sp) && e.ts >= sp.ts;
            let closed = spawned.is_some_and(|sp| events.iter().any(|e| mine(e, sp)));
            if status.is_none() && !closed {
                hits.push((
                    "lane-dead",
                    key.clone(),
                    format!("{role} lane {lane} is gone ({})", s.state),
                ));
            }
            // A lane on a named account that shows its harness's usage-limit banner caps that account.
            // #302: with a `fallback` chain, any lane's limit or sign-out rests its account (else its harness).
            let account = spawned.and_then(|e| e.str("account"));
            let chain = cfg
                .fleet(&s.fleet)
                .is_ok_and(|f| f.role.get(role).is_some_and(|r| !r.fallback.is_empty()));
            if let (Some(p), Some(sp), true) = (pane, spawned, status.is_some() && (account.is_some() || chain)) {
                let screen = herdr::read(r, p).unwrap_or_default();
                let name = account.or(sp.str("harness")).unwrap_or_default();
                let (harness, host) = (sp.str("harness"), sp.str("host"));
                let rest = |until| json!({"agent": lane, "harness": harness, "account": account, "host": host, "until": until});
                let h = crate::harness::get(harness.unwrap_or_default());
                if let Some(until) = h.usage_limited(cfg, r, &screen, ctx.now) {
                    let why = format!("{name}: {role} lane {lane} hit the usage limit; spawns skip {name} for now");
                    let kind = ["agent-unavailable", "account-capped"][usize::from(account.is_some())];
                    hits.push((kind, format!("{lane}:{name}"), why));
                    capped = Some(rest(until));
                } else if chain && h.signed_out(&screen) {
                    let why = format!("{name}: {role} lane {lane} is signed out; spawns skip {name} for now");
                    hits.push(("agent-unavailable", format!("{lane}:{name}:auth"), why));
                    capped = Some(rest(ctx.now + cfg.limits.fallback_rest_minutes * 60));
                }
            }
            if let Some(p) = pane.filter(|_| status.is_some()) {
                if let Some(f) = crate::lanes::log(&ctx.state, lane) {
                    crate::lanes::roll(&f, &crate::lanes::body(&herdr::recent(r, p, "200").unwrap_or_default()));
                }
                if let Some((url, line)) = wt.and_then(|wt| amendment(ctx, r, cfg, s, wt)) {
                    let key = format!("{lane}:{url}");
                    let seen = events.iter().any(|e| e.str("key") == Some(&key));
                    let dsh = spawned.and_then(|e| e.str("harness")) == Some("dsh");
                    if !seen && herdr::reply(r, dsh, lane, p, &line).is_ok() {
                        hits.push(("amended", key, line));
                    }
                }
            }
            // #223: a lane done/idle past `idle_reap_minutes` keeps its agent but not what the agent started.
            let trim =
                wt.filter(|_| matches!(status, Some("done" | "idle")) && idle > cfg.limits.idle_reap_minutes * 60);
            let trimmed = trim.map(|wt| host::reap(r, Path::new(wt), true));
            if let Some((killed, mb)) = trimmed.filter(|t| !t.0.is_empty()) {
                let d = json!({"agent": lane, "killed": killed, "freed_mb": mb});
                let e = Event::new(ctx.now, &s.fleet, Some(s.issue), "lane-trimmed", d);
                emit(&ctx.state, cfg, e)?;
            }
            // #425: a held lane waits by design; an unheld one alerts again every 2 h it stays idle.
            if idle > stall && status != Some("working") && !parked && held_on(&events, s).is_empty() {
                let st = status.unwrap_or("gone");
                // #319: a settled fix lane at the FAILed head says what it is missing.
                let pr = gh::cached_pr(ctx, &s.fleet, s.issue);
                let same = s.state.starts_with("fix") && pr["headRefOid"].as_str().is_some_and(|h| s.reviewed(h));
                let ci = cfg
                    .fleet(&s.fleet)
                    .map(|f| gh::ci(&gh::required(&pr["statusCheckRollup"], &f.required_checks)));
                let done = crate::waiting::fix_done(&events, &s.fleet, s.issue).is_some();
                let why = match ci {
                    Ok(_) if same && done => "fix-done recorded, waiting for drive to spawn review".into(),
                    Ok(ci) if same => format!(
                        "fix lane idle, head unchanged, CI {ci}: run herdsman done {} {} --no-change \"<why>\" or push",
                        s.fleet, s.issue
                    ),
                    _ => format!("{role} lane {lane} is {st}"),
                };
                let key = format!("{key}:{}", (idle - stall) / 7200);
                hits.push((
                    "stall",
                    key,
                    format!("no event for {} in {}; {why}", age(idle), s.state),
                ));
            }
        }
        if let Some((f, pr)) = ready(ctx, r, cfg, s) {
            // #388: a PASS whose head never got a check, once per head.
            if let Some(why) = gh::missing(ctx, Some(r), cfg, (&s.fleet, s.issue), &events, &pr) {
                hits.push(("ci-missing", pr["headRefOid"].as_str().unwrap_or_default().into(), why));
            }
            if let Some(head) = gate(f, &pr, Some(s), false).ok().filter(|_| idle > stall) {
                let (short, ago) = (&head[..7.min(head.len())], age(idle));
                let why = format!("PASS @ {short} with green CI, unmerged for {ago}; run `herdsman merge`");
                hits.push(("ready-unmerged", format!("{}:{head}", s.last_ts), why));
            }
        }
        for (kind, key, reason) in hits {
            let mut d = json!({"key": key, "reason": reason});
            if let (Some(serde_json::Value::Object(c)), "account-capped" | "agent-unavailable") = (&capped, kind) {
                c.iter().for_each(|(k, v)| d[k] = v.clone());
            }
            fire(ctx, cfg, out, &events, (&s.fleet, Some(s.issue)), kind, d)?;
        }
    }
    // #493: an open issue nothing drives for 30 min alerts once per episode; it only reports, never acts.
    if let Some(a) = &agents {
        let all: Vec<Value> = a.iter().chain(remote.values().flatten().flatten()).cloned().collect();
        let log = crate::waiting::Log::new(&events);
        for s in crate::waiting::guarded(&events, cfg) {
            let (f, pr) = (&cfg.fleet[&s.fleet], gh::cached_pr(ctx, &s.fleet, s.issue));
            let Some(since) = crate::waiting::undriven(&log, &s, f, &pr, &all, ctx.now) else {
                continue;
            };
            let lane = s.lane.as_deref().unwrap_or("none");
            let st = all
                .iter()
                .find(|a| a["name"] == lane)
                .map_or(["none", "gone"][usize::from(s.lane.is_some())], |a| {
                    a["agent_status"].as_str().unwrap_or("?")
                });
            let why = format!(
                "#{} {}: nothing drives it for {}; last event {} {} ago; last lane {lane} ({st})",
                s.issue,
                s.shown(),
                age(ctx.now - since),
                s.last_event,
                age(ctx.now.saturating_sub(s.last_ts))
            );
            let d = json!({"key": since, "reason": why});
            fire(ctx, cfg, out, &events, (&s.fleet, Some(s.issue)), "orphan-issue", d)?;
        }
    }
    // #107: a herdr agent named like a lane (`<fleet>-<role>-<n>…`) that no `spawn` event logged; once per agent.
    // #376: only one of no tracked issue: drive adopts (or closes) the rest.
    let tracked = fold(&events);
    for name in agents.iter().flatten().filter_map(|a| a["name"].as_str()) {
        let lane = crate::spawn::shaped(cfg, &json!({"name": name}));
        let lane = lane.filter(|l| l.2.is_some() && !tracked.contains_key(&(l.0.to_string(), l.1)));
        if let Some(l) = lane.filter(|_| last_spawn(&events, name).is_none()) {
            let why = format!("herdr agent {name} has no spawn event and matches no tracked issue");
            let d = json!({"key": name, "reason": why});
            fire(ctx, cfg, out, &events, (l.0, Some(l.1)), "orphan-lane", d)?;
        }
    }
    // #548: a keychain read that timed out on a dialog (a thread's row, a lane's spawn refusal) alerts once per
    // episode; a start or spawn on an account since ends it.
    let mut locked: Option<&Event> = None;
    for e in &events {
        if matches!(e.kind.as_str(), "conductor-start" | "spawn") && e.data.get("account").is_some_and(|a| !a.is_null())
        {
            locked = None;
        } else if locked.is_none()
            && e.kind != "keychain-locked"
            && e.data
                .values()
                .any(|v| v.as_str().is_some_and(|t| t.contains("KEYCHAIN_LOCKED")))
        {
            locked = Some(e);
        }
    }
    if let Some(e) = locked {
        let why = "the Keychain is waiting on a dialog: accounts and secrets can't be read → run `herdsman doctor` for the fix";
        let d = json!({"key": e.ts, "reason": why});
        fire(ctx, cfg, out, &events, ("herdsman", None), "keychain-locked", d)?;
    }
    // #389: a remote lane's outbox line drive has not delivered in 5 min; once per lane and line.
    for (e, lane, ts) in crate::outbox::stale(r, &events, ctx.now) {
        let why = format!(
            "{lane}'s outbox holds a line from {} ago that drive has not delivered",
            age(ctx.now.saturating_sub(ts / 1000))
        );
        let d = json!({"key": format!("{lane}:{ts}"), "agent": e.str("agent"), "reason": why});
        fire(ctx, cfg, out, &events, (&e.fleet, e.issue), "outbox-stale", d)?;
    }
    let events = expire(ctx, cfg, out, events, agents.as_deref(), &remote)?;
    let open = open_asks(&events);
    // #248: a done lane whose handoff is logged closes its session after `done_close_minutes`; its worktree stays.
    for a in agents.iter().flatten().filter(|a| a["agent_status"] == "done") {
        let e = a["name"].as_str().and_then(|n| last_spawn(&events, n));
        let (Some(name), Some(pane), Some(e)) = (a["name"].as_str(), a["pane_id"].as_str(), e) else {
            continue;
        };
        let (Some(n), Some(wt), None) = (e.issue, e.str("worktree"), On::of(r, Some(e)).machine) else {
            continue;
        };
        let asking = open.iter().any(|q| q.str("agent") == Some(name));
        let due = handoff(r, &events, e, (n, wt)).filter(|t| ctx.now >= t + cfg.limits.done_close_minutes * 60);
        // #444: a spawn whose prompt never landed is no finished lane: drive retries it.
        let landed = e.data.get("confirmed") != Some(&json!(false));
        let Some(at) = due.filter(|_| !asking && landed) else {
            continue;
        };
        if let Some(why) = dirty(r, Path::new(wt)) {
            let d = json!({"key": format!("{name}:{at}"), "agent": name, "why": why, "reason": format!("{name} stays open: {why}")});
            fire(ctx, cfg, out, &events, (&e.fleet, Some(n)), "lane-kept-dirty", d)?;
            continue;
        }
        let mb = host::held(r, &[a]).first().copied().unwrap_or_default();
        if herdr::close(r, pane).is_ok() {
            let d = json!({"key": format!("{name}:{at}"), "agent": name, "freed_mb": mb, "reason": "done"});
            fire(ctx, cfg, out, &events, (&e.fleet, Some(n)), "lane-closed", d)?;
        }
    }
    let routine = |a: &&&Event| a.str("kind") == Some("routine");
    let due = |a: &&&Event| ctx.now.saturating_sub(a.ts) >= cfg.limits.ask_default_minutes * 60;
    for a in open.iter().filter(routine).filter(due) {
        let id = a.str("id").unwrap_or_default();
        let rec = a.data["recommend"].to_string();
        crate::answer(ctx, cfg, r, out, (&a.fleet, id), &rec, ("default", None))?;
    }
    let waiting = |a: &&&Event| !routine(a) && a.str("from").is_none() && ctx.now.saturating_sub(a.ts) > 30 * 60;
    for a in open.iter().filter(waiting) {
        let why = format!(
            "{} unanswered for {}: {}",
            a.str("id").unwrap_or("?"),
            age(ctx.now.saturating_sub(a.ts)),
            a.str("question").unwrap_or("?")
        );
        // #584: again once a day while it waits (a live lane's ask never expires).
        let day = ctx.now.saturating_sub(a.ts) / 86_400;
        let key = a
            .str("id")
            .map(|id| [id.to_string(), format!("{id}@{day}d")][usize::from(day > 0)].clone());
        let d = json!({"key": key, "reason": why});
        fire(ctx, cfg, out, &events, (&a.fleet, a.issue), "ask-waiting", d)?;
    }
    if let (sent, held @ 1..) = PUSHES.replace((0, 0)) {
        let d = json!({"key": ctx.now, "reason": format!("{} questions waiting — open the board", sent + held)});
        fire(ctx, cfg, out, &events, ("herdsman", None), "ask-waiting", d)?;
    }
    // #87: devices subscribed but no webpush notifier: the page's alerts never send; once per device count.
    let subs = crate::webpush::load(&ctx.state).len();
    if subs > 0 && cfg.webpush_sub().is_none() {
        let why = format!("{subs} device(s) subscribed, but no type = \"webpush\" notifier: alerts are off");
        fire(
            ctx,
            cfg,
            out,
            &events,
            ("herdsman", None),
            "alerts-off",
            json!({"key": subs, "reason": why}),
        )?;
    }
    // DeepSeek under `warn_below`: once per crossing (a recovery logs `provider-ok`).
    if let Some(Ok((b, low))) = crate::usage::deepseek(ctx, cfg, r) {
        let last = events
            .iter()
            .rev()
            .find(|e| e.fleet == "deepseek" && matches!(e.kind.as_str(), "provider-low" | "provider-ok"));
        if low != last.is_some_and(|e| e.kind == "provider-low") {
            let warn = cfg.providers.deepseek.as_ref().map_or(0.0, |p| p.warn_below);
            let why = format!("balance {} {:.2} (warn below {warn})", b.currency, b.total);
            let ev = Event::new(
                ctx.now,
                "deepseek",
                None,
                if low { "provider-low" } else { "provider-ok" },
                json!({"reason": why}),
            );
            if low {
                writeln!(out, "{}", plain(&ev))?;
            }
            emit(&ctx.state, cfg, ev)?;
        }
    }
    // #186: processes left in closed lanes' worktrees.
    if let Some(a) = &agents {
        leftovers(ctx, cfg, r, a, true)?;
    }
    // #160: the worktree total (bytes, when) for the Services disk row; `serve` never walks or runs `du`.
    let _ = std::fs::write(
        ctx.state.with_file_name("worktrees.du"),
        format!("{} {}", du(&ctx.worktrees), ctx.now),
    );
    Ok(())
}

/// #248: when lane `e` handed off: the latest `pushed`/`verdict` since it spawned (design: its latest commit since).
fn handoff(r: &dyn Runner, events: &[Event], e: &Event, (n, wt): (u64, &str)) -> Option<u64> {
    if e.str("role") == Some("design") {
        let t = ok(r, "git", &["-C", wt, "log", "-1", "--format=%ct"]).ok()?;
        let t = t.trim().parse().ok();
        return t.filter(|t| *t > e.ts);
    }
    let by = |x: &&Event| x.ts >= e.ts && (x.is("pushed", &e.fleet, n) || x.is("verdict", &e.fleet, n));
    events.iter().rev().find(by).map(|x| x.ts)
}

thread_local!(static PUSHES: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) });

/// Log and print one alert unless one of its kind already carries `d`'s `key`; #283: past `max_pushes_per_pass`, no push.
fn fire(
    ctx: &Ctx,
    cfg: &Config,
    out: &mut dyn Write,
    events: &[Event],
    (fleet, issue): (&str, Option<u64>),
    kind: &str,
    d: serde_json::Value,
) -> Result<()> {
    if events
        .iter()
        .any(|e| e.kind == kind && e.fleet == fleet && e.issue == issue && e.data.get("key") == d.get("key"))
    {
        return Ok(());
    }
    let ev = Event::new(ctx.now, fleet, issue, kind, d);
    writeln!(out, "{}", plain(&ev))?;
    let ((sent, held), push) = (PUSHES.get(), crate::config::WEBPUSH_ON.contains(&kind));
    if push && sent >= cfg.limits.max_pushes_per_pass {
        PUSHES.set((sent, held + 1));
        return crate::events::append(&ctx.state, &ev);
    }
    PUSHES.set((sent + usize::from(push), held));
    emit(&ctx.state, cfg, ev)
}

/// #604: a host gate's refusal (code, host, why) from a `drive-pass` action; a reason without a `<host>: ` is local.
type Gate<'a> = (&'a str, &'a str, &'a str);
fn host_gate(a: &serde_json::Value) -> Option<Gate<'_>> {
    let code = a["code"]
        .as_str()
        .filter(|c| ["HOST_BUSY", "MEM_LOW", "DISK_LOW", "DISK_BUDGET"].contains(c))?;
    let why = a["why"].as_str().unwrap_or_default();
    let (host, why) = why
        .split_once(": ")
        .filter(|h| !h.0.contains(' '))
        .unwrap_or(("local", why));
    Some((code, host, why.split("; retry").next().unwrap_or(why)))
}

/// #584: an ask open over `ask_expire_hours` that nobody waits on (a thread ask with no lane, a handoff card, an ask
/// whose lane is no live agent) closes as `ask-expired`. The first pass over a log with asks sweeps it once: a handoff
/// card whose handoff went out (no decision was logged before #584) closes as `ask-delivered`; one `asks-swept` counts
/// the pass's closures. The log, read again if anything closed.
fn expire(
    ctx: &Ctx,
    cfg: &Config,
    out: &mut dyn Write,
    events: Vec<Event>,
    agents: Option<&[Value]>,
    remote: &BTreeMap<String, Option<Vec<Value>>>,
) -> Result<Vec<Event>> {
    let sweep = !events.iter().any(|e| e.kind == "asks-swept") && events.iter().any(|e| e.kind == "needs-decision");
    let lists = || agents.into_iter().flatten().chain(remote.values().flatten().flatten());
    // Unknown (herdr not answering, its host offline) counts as live.
    let live = |q: &Event, a: &str| {
        let down = q
            .str("host")
            .is_some_and(|h| remote.get(h).is_some_and(Option::is_none));
        agents.is_none() || down || lists().any(|x| x["name"].as_str() == Some(a))
    };
    let mut n = 0;
    for q in open_asks(&events) {
        let id = q.str("id").unwrap_or_default();
        let h = q.data.get("handoff").unwrap_or(&Value::Null);
        let went = |e: &&Event| {
            e.kind == "thread-handoff"
                && e.seq > q.seq
                && e.str("from") == q.str("thread")
                && e.data.get("title") == h.get("title")
        };
        let old = ctx.now.saturating_sub(q.ts) > cfg.limits.ask_expire_hours * 3600;
        let nobody = q.str("thread").is_some() || q.str("agent").is_some();
        let (kind, mut d) = if let Some(e) = events.iter().find(went).filter(|_| sweep && !h.is_null()) {
            ("ask-delivered", json!({"to": e.str("to")}))
        } else if old && nobody && !q.str("agent").is_some_and(|a| live(q, a)) {
            let (t, ask) = (age(ctx.now.saturating_sub(q.ts)), q.str("question").unwrap_or("?"));
            (
                "ask-expired",
                json!({"reason": format!("{id} expired after {t}: {ask}")}),
            )
        } else {
            continue;
        };
        (d["key"], d["id"]) = (json!(id), json!(id));
        fire(ctx, cfg, out, &events, (&q.fleet, q.issue), kind, d)?;
        n += 1;
    }
    if sweep {
        let ev = Event::new(ctx.now, "herdsman", None, "asks-swept", json!({"count": n}));
        if n > 0 {
            writeln!(out, "{}", plain(&ev))?;
        }
        crate::events::append(&ctx.state, &ev)?;
    }
    Ok(if n > 0 { read(&ctx.state)? } else { events })
}

/// After `herdsman merge`: close the issue's clean lane panes, then `herdsman clean --yes` them.
pub fn teardown(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, fleet: &str, issue: u64) -> Result<()> {
    let events = read(&ctx.state)?;
    let mut seen = std::collections::BTreeSet::new();
    for e in events.iter().rev().filter(|e| e.is("spawn", fleet, issue)) {
        let (Some(wt), Some(pane), on) = (e.str("worktree"), e.str("pane"), On::of(r, Some(e))) else {
            continue;
        };
        if e.str("role") == Some("design") || !seen.insert(wt) || !on.exists(Path::new(wt)) {
            continue;
        }
        match dirty(&on, Path::new(wt)) {
            Some(why) => writeln!(out, "keep pane {pane} ({wt}): {why}")?,
            None => drop(herdr::close(&on, pane)),
        }
    }
    if seen.is_empty() {
        return Ok(());
    }
    clean(ctx, cfg, r, out, Some(fleet), (Some(issue), None, false), true)
}

/// Remove worktree + pane of lanes whose issue is merged (or closed on GitHub). Dry run unless `yes`.
/// `--lane` (one agent's worktree) refuses `UNSAFE_CLEAN` with the reason when it cannot go.
pub fn clean(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    fleet: Option<&str>,
    (issue, lane, sched): (Option<u64>, Option<&str>, bool),
    yes: bool,
) -> Result<()> {
    let events = read(&ctx.state)?;
    let states = fold(&events);
    let agents = crate::hosts::all_agents(cfg, r).context("cannot list herdr agents; refusing to clean blind")?;
    let mut seen = std::collections::BTreeSet::new();
    let mut failed = 0;
    let (mut freed, mut removed) = (0, 0);
    let mut closed = BTreeMap::new();
    // #518: a `rev-<n>-r<k>` folder no spawn names, as its fleet's spawn (visited last, so a logged one wins).
    let revs: Vec<Event> = (cfg.fleet.iter().filter(|(n, _)| fleet.is_none_or(|f| f == n.as_str())))
        .flat_map(|(name, f)| {
            let dirs = std::fs::read_dir(ctx.worktrees.join(f.repo_name()))
                .into_iter()
                .flatten()
                .flatten();
            dirs.filter_map(move |d| {
                let dir = d.file_name().into_string().ok()?;
                let (n, k) = dir.strip_prefix("rev-")?.split_once("-r")?;
                let (n, _) = (n.parse::<u64>().ok()?, k.parse::<u64>().ok()?);
                let d = json!({"agent": format!("{name}-{dir}"), "worktree": d.path()});
                Some(Event::new(ctx.now, name, Some(n), "spawn", d))
            })
        })
        .collect();
    // Newest spawn per worktree wins (it has the current pane); #416: a `spawn-started` alone (the spawn failed) too.
    for e in (events.iter().rev())
        .filter(|e| matches!(e.kind.as_str(), "spawn" | "spawn-started") && fleet.is_none_or(|f| f == e.fleet))
        .chain(&revs)
    {
        let (Some(n), Some(wt)) = (e.issue, e.str("worktree")) else {
            continue;
        };
        if issue.is_some_and(|i| i != n) || lane.is_some_and(|a| e.str("agent") != Some(a)) {
            continue;
        }
        let on = On::of(r, Some(e));
        // #561: a disabled or unreachable host's worktrees are never probed; a merged or closed one there is listed.
        let away = e.str("host").filter(|_| on.machine.is_some());
        let held = match away.and_then(|h| cfg.hosts.get(h)) {
            Some(h) if !h.enabled => Some("on a disabled host (not cleaned)"),
            _ => crate::hosts::offline(ctx, Some(e)).map(|_| "host offline (not cleaned)"),
        };
        let gone = |c: &Event| c.kind == "clean" && c.str("worktree") == Some(wt) && c.ts >= e.ts;
        let gone = || held.map_or_else(|| !on.exists(Path::new(wt)), |_| events.iter().any(gone));
        if seen.contains(wt) || gone() {
            continue;
        }
        let unsafe_lane =
            |why: &str| crate::gate::refused("UNSAFE_CLEAN", format!("{}: {why}", lane.unwrap_or_default()));
        // Merged by `herdsman merge`, or closed on GitHub by any other route.
        let why = if states.get(&(e.fleet.clone(), n)).is_some_and(|s| s.state == "merged") {
            "merged"
        } else if closed_issues(ctx, cfg, r, out, &mut closed, &e.fleet).contains(&n) {
            "issue closed"
        } else if lane.is_some() {
            let p = Some(gh::cached_pr(ctx, &e.fleet, n))
                .filter(|p| p["state"] == "OPEN")
                .map(|p| p["number"].clone());
            return unsafe_lane(&p.map_or("only after merge".into(), |n| {
                format!("only after merge (PR #{n} open)")
            }));
        } else {
            continue;
        };
        // #557: the daily job takes lanes idle 3 days or more since their issue's last event, 10 per run.
        let idle = states
            .get(&(e.fleet.clone(), n))
            .is_some_and(|s| ctx.now >= s.last_ts + 3 * 86_400);
        if sched && !idle {
            continue;
        }
        if sched && removed == 10 {
            writeln!(out, "stop: 10 removed this run; the rest go next run")?;
            break;
        }
        seen.insert(wt);
        let pane = e.str("pane").unwrap_or_default();
        let shown = away.map_or(wt.into(), |h| format!("{h}:{wt}")); // #416
        if let Some(why) = held {
            if lane.is_some() {
                return unsafe_lane(why);
            }
            writeln!(out, "keep {shown}: {why}")?;
            continue;
        }
        let live = !host::live_in(&agents, e.str("agent").unwrap_or_default(), Path::new(wt)).is_empty();
        let keep = if live {
            Some("live agent".to_string())
        } else {
            if yes {
                reap(ctx, cfg, &on, (&e.fleet, Some(n)), wt)?;
                if on.machine.is_none() {
                    freed += purge(r, cfg, &e.fleet, out, Path::new(wt), false)?;
                }
            }
            dirty(&on, Path::new(wt))
        };
        let keep = match keep {
            Some(why) if !yes && !live && lane.is_none() => {
                writeln!(out, "would save {shown} to wip/ ({why}), then remove it")?;
                continue;
            }
            Some(_) if yes && !live && lane.is_none() => save(ctx, cfg, &on, out, &events, e, wt)?,
            k => k,
        };
        if let Some(reason) = keep {
            if lane.is_some() {
                return unsafe_lane(&reason);
            }
            writeln!(out, "keep {shown}: {reason}")?;
            continue;
        }
        if !yes {
            writeln!(out, "would remove {shown} (pane {pane}, {}#{n} {why})", e.fleet)?;
            continue;
        }
        if let Err(err) = remove(
            &on,
            cfg,
            &e.fleet,
            wt,
            e.str("workspace")
                .or_else(|| pane.split(':').next())
                .unwrap_or_default(),
        ) {
            writeln!(out, "failed {shown}: {err:#}")?;
            failed += 1;
            continue;
        }
        if !pane.is_empty() {
            let _ = herdr::close(&on, pane);
        }
        if on.machine.is_none() {
            crate::harness::remove_tmp(&stray_roots(cfg), wt);
        }
        drop_log(ctx, e.str("agent"));
        removed += 1;
        writeln!(out, "removed {shown} ({}#{n} {why})", e.fleet)?;
        emit(
            &ctx.state,
            cfg,
            Event::new(
                ctx.now,
                &e.fleet,
                Some(n),
                "clean",
                json!({"worktree": wt, "pane": pane, "why": why}),
            ),
        )?;
    }
    if lane.is_some() && seen.is_empty() {
        return crate::gate::refused(
            "UNSAFE_CLEAN",
            format!("{}: no worktree in the log", lane.unwrap_or_default()),
        );
    }
    if !yes {
        writeln!(out, "dry run: nothing changed; pass --yes to remove")?;
    } else if freed > 0 {
        writeln!(out, "freed {freed} bytes of build folders")?;
    }
    if failed > 0 {
        anyhow::bail!("{failed} worktree(s) could not be removed; see above");
    }
    Ok(())
}

/// #557 `clean --scheduled`, the daily `dev.herdsman.clean` job: `clean --yes` held to lanes idle 3 days (10 a run),
/// then each herdr workspace whose checkout folder is gone is closed. A refusal or failure alerts `clean-failed`
/// once (until a run passes); a clean no-op is silent.
pub fn scheduled(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let res = clean(ctx, cfg, r, out, None, (None, None, true), true).and_then(|()| {
        let list = herdr::list(r, "workspace")?;
        let all = list["result"]["workspaces"].as_array().cloned().unwrap_or_default();
        for w in all {
            let (id, path) = (w["workspace_id"].as_str(), w["worktree"]["checkout_path"].as_str());
            let (Some(id), Some(path)) = (id, path.filter(|p| !Path::new(p).exists())) else {
                continue;
            };
            let o = herdr::workspace_close(r, id)?;
            anyhow::ensure!(o.code == 0, "close workspace {id}: {}", herdr::message(&o));
            writeln!(out, "closed workspace {id}: {path} is gone")?;
        }
        Ok(())
    });
    let mark = ctx.state.with_file_name("clean.failed");
    let Err(e) = res else {
        let _ = std::fs::remove_file(&mark);
        return Ok(());
    };
    let why = format!("scheduled clean: {e:#}");
    if std::fs::read_to_string(&mark).ok().as_deref() != Some(why.as_str()) {
        std::fs::write(&mark, &why)?;
        let d = json!({"key": "clean", "reason": why});
        emit(
            &ctx.state,
            cfg,
            Event::new(ctx.now, "herdsman", None, "clean-failed", d),
        )?;
    }
    Err(e)
}

/// #186: TERM, then KILL, the processes inside `wt`; logs `lane-reaped` when any were signalled.
pub fn reap(ctx: &Ctx, cfg: &Config, r: &dyn Runner, (fleet, issue): (&str, Option<u64>), wt: &str) -> Result<usize> {
    let names = host::reap(r, Path::new(wt), false).0;
    if !names.is_empty() {
        let d = json!({"worktree": wt, "killed": names.len(), "names": names});
        emit(&ctx.state, cfg, Event::new(ctx.now, fleet, issue, "lane-reaped", d))?;
    }
    Ok(names.len())
}

/// #186: the processes in worktrees of closed lanes (newest spawn not its issue's active lane, no live herdr agent
/// there) as `(pid, command, cwd)`; with `yes` each such worktree is reaped.
pub fn leftovers(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    agents: &[serde_json::Value],
    yes: bool,
) -> Result<Vec<(String, String, String)>> {
    let (events, ps, mut seen, mut found) = (
        read(&ctx.state)?,
        host::cwds(r),
        std::collections::BTreeSet::new(),
        vec![],
    );
    let st = fold(&events);
    for e in events.iter().rev().filter(|e| e.kind == "spawn") {
        let (Some(wt), Some(n), agent, None) = (e.str("worktree"), e.issue, e.str("agent"), e.str("machine")) else {
            continue;
        };
        let active = st
            .get(&(e.fleet.clone(), n))
            .and_then(IssueState::active_lane)
            .is_some_and(|l| Some(l.1) == agent);
        let here = host::inside(&ps, Path::new(wt));
        if !seen.insert(wt)
            || active
            || here.is_empty()
            || !host::live_in(agents, agent.unwrap_or("\0"), Path::new(wt)).is_empty()
        {
            continue;
        }
        if yes {
            reap(ctx, cfg, r, (&e.fleet, Some(n)), wt)?;
        }
        found.extend(here);
    }
    Ok(found)
}

const BUILD_DIRS: &[&str] = &["target", "node_modules", ".next", "dist", ".cache"];
/// #103: delete a lane's build folders (#164: plus the fleet's `build_dirs`), even when the worktree stays; the bytes
/// freed (#361: with `dry`, listed and counted, none removed). One git does not ignore stays.
pub fn purge(r: &dyn Runner, cfg: &Config, fleet: &str, out: &mut dyn Write, wt: &Path, dry: bool) -> Result<u64> {
    let mut freed = 0;
    let more = cfg.fleet.get(fleet).map_or(&[][..], |f| &f.build_dirs);
    for d in BUILD_DIRS.iter().copied().chain(more.iter().map(String::as_str)) {
        let dir = wt.join(d);
        if !dir.is_dir() {
            continue;
        }
        if ok(r, "git", &["-C", &wt.to_string_lossy(), "check-ignore", "-q", d]).is_err() {
            writeln!(out, "keep {}: not git-ignored", dir.display())?;
            continue;
        }
        let size = du(&dir);
        let _ = dry.then(|| writeln!(out, "  would remove {} ({:.2} GB)", dir.display(), gb(size)));
        if dry || std::fs::remove_dir_all(&dir).is_ok() {
            freed += size;
        }
    }
    Ok(freed)
}

/// #164: a merged or closed lane's unsaved work goes to `wip/<agent>-<date>`, the remote sha read back, so the
/// worktree can go (`None`); a secret in it keeps the worktree, `wip-blocked` once (to the phone): the reason.
fn save(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    events: &[Event],
    e: &Event,
    wt: &str,
) -> Result<Option<String>> {
    let agent = e.str("agent").unwrap_or_default();
    let w = wip_branch(ctx, r, agent);
    let git = |a: &[&str]| ok(r, "git", &[&["-C", wt][..], a].concat());
    let sha = match snapshot(r, wt, &w, "wip: auto-saved by herdsman") {
        Err(Snap::Secret(hit)) => {
            let why = format!("unsaved work has a secret ({hit})");
            let d = json!({"key": wt, "agent": agent, "reason": format!("{wt}: {why}; kept")});
            fire(ctx, cfg, out, events, (&e.fleet, e.issue), "wip-blocked", d)?;
            return Ok(Some(why));
        }
        Err(Snap::Failed(err) | Snap::Push(err)) => return Ok(Some(format!("wip save failed: {err:#}"))),
        Ok(()) => git(&["rev-parse", "HEAD"])?.trim().to_string(),
    };
    let remote = git(&["ls-remote", "origin", &format!("refs/heads/{w}")])?;
    if remote.split_whitespace().next() != Some(sha.as_str()) {
        return Ok(Some(format!("{w} reads back `{}`, not {sha}", remote.trim())));
    }
    writeln!(out, "saved {wt} to {w} @ {sha:.7}")?;
    let d = json!({"agent": agent, "branch": w, "sha": sha});
    emit(&ctx.state, cfg, Event::new(ctx.now, &e.fleet, e.issue, "wip-saved", d))?;
    Ok(None)
}

/// #164: the worktree total by a full walk (drive's pass, never a request), stamped for spawn's `DISK_BUDGET` and the
/// Host tab. Over `worktree_budget_gb`, the idlest lanes' build folders go first (`budget-freed`), never a working one's.
pub fn budget(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let mut total = du(&ctx.worktrees).saturating_sub(du(&ctx.worktrees.join(crate::roots::BUILD)));
    let over = |t: u64| gb(t) > cfg.limits.worktree_budget_gb;
    if over(total) {
        let (events, agents) = (read(&ctx.state)?, host::agents(r)?);
        let states = fold(&events);
        let mut seen = std::collections::BTreeSet::new();
        let mut lanes: Vec<(u64, &Event, &str)> = (events.iter().rev())
            .filter(|e| e.kind == "spawn" && e.str("role") != Some("orch"))
            .filter_map(|e| Some((e, e.str("worktree")?)))
            .filter(|(_, w)| Path::new(w).starts_with(&ctx.worktrees) && Path::new(w).exists() && seen.insert(*w))
            .filter(|(e, _)| {
                !agents
                    .iter()
                    .any(|a| a["name"].as_str() == e.str("agent") && a["agent_status"] == "working")
            })
            .map(|(e, w)| {
                (
                    e.issue
                        .and_then(|n| states.get(&(e.fleet.clone(), n)))
                        .map_or(e.ts, |s| s.last_ts),
                    e,
                    w,
                )
            })
            .collect();
        lanes.sort_by_key(|l| l.0);
        for (_, e, wt) in lanes {
            if !over(total) {
                break;
            }
            let bytes = purge(r, cfg, &e.fleet, out, Path::new(wt), false)?;
            if bytes > 0 {
                total = total.saturating_sub(bytes);
                writeln!(out, "budget: freed {bytes} bytes of build folders in {wt}")?;
                let d = json!({"agent": e.str("agent"), "bytes": bytes});
                emit(
                    &ctx.state,
                    cfg,
                    Event::new(ctx.now, &e.fleet, e.issue, "budget-freed", d),
                )?;
            }
        }
    }
    sweep_scratch(ctx, cfg, r, out)?;
    trim_builds(ctx, cfg, cfg.limits.build_budget_gb, out, false)?;
    Ok(std::fs::write(
        ctx.state.with_file_name("worktrees.du"),
        format!("{total} {}", ctx.now),
    )?)
}

/// #438: each enabled remote host's shared build folder trimmed to `build_budget_gb`; #485: after the tick's drive,
/// one quick ssh per host (a disabled or offline one never holds the pass up).
pub fn trim_hosts(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let mut tried = std::collections::BTreeSet::new();
    let hosts = cfg.hosts.iter().filter(|h| h.1.enabled);
    for (name, m) in hosts.filter_map(|(n, h)| Some((n, h.machine.as_deref()?))) {
        if !tried.insert(m) {
            continue;
        }
        if let Err(e) = trim_on(ctx, &On::quick(r, Some(m)), cfg, cfg.limits.build_budget_gb, out) {
            writeln!(out, "builds: {name}: {e:#}")?;
        }
    }
    Ok(())
}

/// #536: an ended lane's scratch (`<root>/.build/.scratch/<agent>`: no herdr agent of that name, not an open issue's
/// active lane) goes, unless cargo is building there.
fn sweep_scratch(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let roots: Vec<_> = (builds(ctx, cfg).into_iter())
        .map(|b| b.join(crate::spawn::SCRATCH))
        .filter(|d| d.is_dir())
        .collect();
    if roots.is_empty() {
        return Ok(());
    }
    let (agents, states) = (host::agents(r)?, fold(&read(&ctx.state)?));
    let active: std::collections::BTreeSet<&str> = (states.values().filter(|s| !s.done()))
        .filter_map(|s| Some(s.active_lane()?.1))
        .collect();
    for d in roots
        .iter()
        .flat_map(std::fs::read_dir)
        .flatten()
        .flatten()
        .map(|e| e.path())
    {
        let name = d.file_name().unwrap_or_default().to_string_lossy();
        if active.contains(&*name) || agents.iter().any(|a| a["name"] == *name) || building(&d, 3) {
            continue;
        }
        if std::fs::remove_dir_all(&d).is_ok() {
            writeln!(out, "builds: removed {} (lane ended)", d.display())?;
        }
    }
    Ok(())
}

/// #536: cargo holds (flocks) a `.cargo-lock` in `d` or up to `depth` folders below it.
fn building(d: &Path, depth: u8) -> bool {
    let held = std::fs::File::open(d.join(".cargo-lock")).is_ok_and(|f| f.try_lock().is_err());
    held || depth > 0
        && (std::fs::read_dir(d).into_iter().flatten().flatten())
            .any(|e| e.file_type().is_ok_and(|t| t.is_dir()) && building(&e.path(), depth - 1))
}

/// #536: the newest mtime of `d` and its entries.
fn mtime(d: &Path) -> u64 {
    let m = |p: &Path| {
        p.metadata()
            .map_or(0, |m| std::os::unix::fs::MetadataExt::mtime(&m).unsigned_abs())
    };
    let ls = std::fs::read_dir(d).into_iter().flatten().flatten();
    ls.map(|e| m(&e.path())).fold(m(d), u64::max)
}

/// #630: this host's build folders, `<root>/.build` of each of its roots.
fn builds(ctx: &Ctx, cfg: &Config) -> Vec<std::path::PathBuf> {
    let name = cfg
        .hosts
        .iter()
        .find(|h| h.1.machine.is_none())
        .map_or("", |h| h.0.as_str());
    let roots = crate::roots::listed(cfg, name, None, &ctx.worktrees);
    roots.into_iter().map(|r| r.join(crate::roots::BUILD)).collect()
}

/// `trim_root` on each of this host's build folders. Bytes freed.
pub fn trim_builds(ctx: &Ctx, cfg: &Config, budget: f64, out: &mut dyn Write, dry: bool) -> Result<u64> {
    builds(ctx, cfg).iter().map(|b| trim_root(b, budget, out, dry)).sum()
}

/// #438: over `budget` GB, the shared build folder's profiles (`<root>/.build/<repo>/<profile>`) whose
/// `.cargo-lock` no cargo holds go, until it is under; a building one stays. #536: lane scratch dirs count and go
/// too, oldest first. Bytes freed.
fn trim_root(root: &Path, budget: f64, out: &mut dyn Write, dry: bool) -> Result<u64> {
    if !root.is_dir() {
        return Ok(0);
    }
    let (mut total, mut freed) = (du(root), 0);
    let ls = |p: &Path| std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path());
    let profiles = ls(root)
        .flat_map(|r| ls(&r))
        .filter(|p| p.join(".cargo-lock").is_file())
        .map(|p| (p, 0));
    let scratch = ls(&root.join(crate::spawn::SCRATCH))
        .filter(|p| p.is_dir())
        .map(|p| (p, 3));
    let mut dirs: Vec<_> = profiles.chain(scratch).map(|(p, n)| (mtime(&p), p, n)).collect();
    dirs.sort();
    for (_, dir, depth) in dirs {
        if gb(total) <= budget {
            break;
        }
        // Cargo flocks the profile's `.cargo-lock` while it builds there.
        if building(&dir, depth) {
            writeln!(out, "keep {}: cargo is building", dir.display())?;
            continue;
        }
        let size = du(&dir);
        let verb = if dry { "would trim" } else { "trimmed" };
        if dry || std::fs::remove_dir_all(&dir).is_ok() {
            writeln!(out, "builds: {verb} {} ({:.2} GB)", dir.display(), gb(size))?;
            (total, freed) = (total.saturating_sub(size), freed + size);
        }
    }
    Ok(freed)
}

/// #536: `[limits] stray_roots`, `$TMPDIR` resolved, each once.
fn stray_roots(cfg: &Config) -> Vec<std::path::PathBuf> {
    let mut v: Vec<_> = (cfg.limits.stray_roots.iter())
        .map(|r| match r.as_str() {
            "$TMPDIR" => std::env::temp_dir(),
            r => crate::config::expand(r),
        })
        .filter_map(|p| p.canonicalize().ok())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// #536: Cargo build folders (holding `CACHEDIR.TAG` or `.rustc_info.json`) lanes left in `/tmp` and `$TMPDIR`,
/// untouched a day and not building; listed, removed with `yes`.
pub fn strays(ctx: &Ctx, cfg: &Config, out: &mut dyn Write, yes: bool) -> Result<()> {
    for root in stray_roots(cfg) {
        for d in std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
        {
            let cargo = ["CACHEDIR.TAG", ".rustc_info.json"].iter().any(|f| d.join(f).is_file());
            if !cargo || ctx.now.saturating_sub(mtime(&d)) < 86_400 || building(&d, 1) {
                continue;
            }
            let size = gb(du(&d));
            if !yes {
                writeln!(out, "would remove stray build folder {} ({size:.2} GB)", d.display())?;
            } else if std::fs::remove_dir_all(&d).is_ok() {
                writeln!(out, "removed stray build folder {} ({size:.2} GB)", d.display())?;
            }
        }
    }
    Ok(())
}

/// #438: `trim_builds` on a remote host, in its shell: `lsof` stands in for cargo's flock (no `lsof`: nothing goes).
const TRIM: &str = r#"r=$1; case $r in "~/"*) r=$HOME/${r#"~/"};; esac
cd "$r" 2>/dev/null && command -v lsof >/dev/null || { echo "freed 0"; exit 0; }
t=$(du -sk . | cut -f1); f=0
for d in */*/; do
  [ "$t" -le "$2" ] && break
  [ -f "$d.cargo-lock" ] || continue
  if lsof "$d.cargo-lock" >/dev/null 2>&1; then echo "keep $r/$d: cargo is building"; continue; fi
  s=$(du -sk "$d" | cut -f1)
  rm -rf "./${d:?}" && t=$((t - s)) && f=$((f + s)) && echo "builds: trimmed $r/$d ($s KB)"
done
echo "freed $f""#;

/// The shared build folders on `on`'s host trimmed to `budget` GB each; bytes freed.
pub fn trim_on(ctx: &Ctx, on: &On, cfg: &Config, budget: f64, out: &mut dyn Write) -> Result<u64> {
    let Some(h) = cfg
        .hosts
        .values()
        .find(|h| h.machine.as_deref() == on.machine && on.machine.is_some())
    else {
        return trim_builds(ctx, cfg, budget, out, false);
    };
    let roots = Some(h.roots()).filter(|v| !v.is_empty());
    let mut freed = 0;
    for root in roots.unwrap_or_else(|| vec![crate::roots::DEFAULT.into()]) {
        freed += trim_remote(on, &format!("{root}/{}", crate::roots::BUILD), budget, out)?;
    }
    Ok(freed)
}

fn trim_remote(on: &On, root: &str, budget: f64, out: &mut dyn Write) -> Result<u64> {
    let kb = format!("{:.0}", budget * 1_048_576.0);
    let o = on.run("sh", &crate::runner::argv(&["-c", TRIM, "sh", root, &kb]))?;
    anyhow::ensure!(o.code == 0, "trim failed ({}): {}", o.code, o.stderr.trim());
    let s = o.stdout;
    let (log, last) = s.trim_end().rsplit_once('\n').unwrap_or(("", s.trim()));
    if !log.is_empty() {
        writeln!(out, "{log}")?;
    }
    Ok(last
        .strip_prefix("freed ")
        .and_then(|k| k.parse::<u64>().ok())
        .unwrap_or(0)
        * 1024)
}

const NOTE: &str = "(logical sizes; APFS clones may free less)";
/// #361: build folders of worktrees idle 7 days, previewed; removed with `yes`, or on a `y` (rechecked, then removed).
pub fn reclaim(ctx: &Ctx, cfg: &Config, r: &dyn Runner, o: &mut dyn Write, i: &mut dyn Read, yes: bool) -> Result<()> {
    let (events, agents, ps) = (read(&ctx.state)?, host::agents(r)?, host::cwds(r));
    let (mut seen, mut total) = (std::collections::BTreeSet::new(), 0);
    for e in events.iter().rev().filter(|e| e.kind == "spawn") {
        let wt = Path::new(e.str("worktree").unwrap_or_default());
        if e.str("machine").is_some() || !wt.starts_with(&ctx.worktrees) || !wt.is_dir() || !seen.insert(wt) {
            continue;
        }
        let why = match touched(r, wt).map(|t| ctx.now.saturating_sub(t)) {
            _ if !host::live_in(&agents, e.str("agent").unwrap_or("\0"), wt).is_empty() => "a live lane".into(),
            _ if !host::inside(&ps, wt).is_empty() => "a process runs there".into(),
            Some(a) if a >= 7 * 86_400 => String::new(),
            Some(a) => format!("touched {} ago", age(a)),
            None => "HEAD or status unreadable".into(),
        };
        if why.is_empty() {
            writeln!(o, "{}:", wt.display())?;
            total += purge(r, cfg, &e.fleet, o, wt, !yes)?;
        } else {
            writeln!(o, "skip {}: {why}", wt.display())?;
        }
    }
    total += trim_builds(ctx, cfg, cfg.limits.build_budget_gb, o, !yes)?;
    let done = if yes { "freed" } else { "reclaimable" };
    writeln!(o, "{:.2} GB {done} {NOTE}", gb(total))?;
    if yes {
        return Ok(());
    }
    let mut b = [0];
    if total > 0 && write!(o, "apply? [y/N] ").is_ok() && i.read(&mut b).is_ok() && b == *b"y" {
        return reclaim(ctx, cfg, r, o, i, true);
    }
    Ok(writeln!(o, "preview: nothing changed; pass --apply to remove")?)
}

/// #361: the latest of HEAD's last move and the mtimes of changed, deleted (nearest folder) or untracked files.
fn touched(r: &dyn Runner, wt: &Path) -> Option<u64> {
    let git = |a: &[&str]| ok(r, "git", &[&["-C", &*wt.to_string_lossy()][..], a].concat()).ok();
    let head = git(&["log", "-g", "-1", "--date=unix", "--format=%gd", "HEAD"])?;
    let mut t: u64 = head.trim().strip_prefix("HEAD@{")?.strip_suffix('}')?.parse().ok()?;
    for f in git(&["status", "--porcelain", "-z", "--untracked-files=all"])?.split_terminator('\0') {
        let f = f.get(3..).filter(|_| f.as_bytes().get(2) == Some(&b' ')).unwrap_or(f);
        let m = wt.join(f).ancestors().find_map(|a| a.metadata().ok());
        t = t.max(m.map_or(0, |m| std::os::unix::fs::MetadataExt::mtime(&m).unsigned_abs()));
    }
    Some(t)
}

#[allow(clippy::cast_precision_loss)] // GB to one decimal
fn gb(bytes: u64) -> f64 {
    bytes as f64 / 1_073_741_824.0
}

/// The last walk's worktree total in GB (`check`'s or `budget`'s, both stamp `worktrees.du`) and when.
pub fn worktree_du(ctx: &Ctx) -> Option<(f64, Option<u64>)> {
    let s = std::fs::read_to_string(ctx.state.with_file_name("worktrees.du")).ok()?;
    let mut n = s.split_whitespace();
    Some((gb(n.next()?.parse().ok()?), n.next().and_then(|t| t.parse().ok())))
}

pub fn worktree_gb(ctx: &Ctx) -> Option<f64> {
    worktree_du(ctx).map(|d| d.0)
}

/// #164: folders under the worktree root that no configured fleet's repo owns.
pub fn orphan_dirs(ctx: &Ctx, cfg: &Config) -> Vec<std::path::PathBuf> {
    let ours: Vec<&str> = cfg.fleet.values().map(crate::config::Fleet::repo_name).collect();
    let all = std::fs::read_dir(&ctx.worktrees).into_iter().flatten().flatten();
    all.map(|d| d.path())
        .filter(|p| {
            p.is_dir()
                && !p
                    .file_name()
                    .is_some_and(|n| n.as_encoded_bytes()[0] == b'.' || ours.iter().any(|o| n == *o))
        })
        .collect()
}

/// `orphan <path> <size>` per orphan folder; removed only with `remove`. The count.
pub fn orphans(ctx: &Ctx, cfg: &Config, out: &mut dyn Write, remove: bool) -> Result<usize> {
    let found = orphan_dirs(ctx, cfg);
    for p in &found {
        if remove {
            std::fs::remove_dir_all(p).with_context(|| format!("removing {}", p.display()))?;
            writeln!(out, "removed orphan {}", p.display())?;
        } else {
            writeln!(out, "orphan {} {:.1} GB", p.display(), gb(du(p)))?;
        }
    }
    Ok(found.len())
}

/// #416: `orphan <host>:<path> <size>` per orphan folder under each enabled remote host's root; gone with `remove`.
pub fn remote_orphans(cfg: &Config, r: &dyn Runner, out: &mut dyn Write, remove: bool) -> Result<usize> {
    let (ours, mut found): (Vec<&str>, _) = (cfg.fleet.values().map(crate::config::Fleet::repo_name).collect(), 0);
    for (name, h) in cfg.hosts.iter().filter(|h| h.1.enabled && h.1.machine.is_some()) {
        let (on, root) = (
            On::new(r, h.machine.as_deref()),
            h.root.as_deref().unwrap_or(crate::roots::DEFAULT),
        );
        let list = ok(&on, "sh", &["-c", DIRS, "sh", root.trim_start_matches("~/")]);
        let list = list.or_else(|e| writeln!(out, "cannot list orphans on {name}: {e:#}").map(|()| String::new()))?;
        for (kb, p) in list.lines().filter_map(|l| l.split_once('\t')) {
            if !ours.iter().any(|o| p.rsplit('/').next() == Some(o)) {
                found += 1;
                if remove {
                    ok(&on, "rm", &["-rf", "--", p])?;
                    writeln!(out, "removed orphan {name}:{p}")?;
                } else {
                    writeln!(
                        out,
                        "orphan {name}:{p} {:.1} GB",
                        gb(kb.parse::<u64>().unwrap_or(0) * 1024)
                    )?;
                }
            }
        }
    }
    Ok(found)
}

/// `<KB>\t<path>` per folder (not a symlink) under the root `$1`, from `$HOME` unless absolute.
const DIRS: &str = "cd && cd \"$1\" 2>/dev/null || exit 0; \
for d in */; do [ -d \"$d\" ] && [ ! -L \"${d%/}\" ] && du -sk \"$PWD/${d%/}\"; done; true";

/// #138: the lane's rolling transcript goes with it.
fn drop_log(ctx: &Ctx, agent: Option<&str>) {
    let _ = crate::lanes::log(&ctx.state, agent.unwrap_or_default()).map(std::fs::remove_file);
}

/// Bytes of blocks used under `p` (symlinks not followed), like `du -k`, not apparent size; the disk row's total.
pub fn du(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::read_dir(p).map_or(0, |d| d.flatten().map(|e| du(&e.path())).sum()),
        m => m.map_or(0, |m| m.blocks() * 512),
    }
}

/// The fleet's closed issue numbers, read once per fleet per run; a failed read is reported and counts as none.
fn closed_issues<'a>(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    memo: &'a mut BTreeMap<String, Vec<u64>>,
    fleet: &str,
) -> &'a Vec<u64> {
    if !memo.contains_key(fleet) {
        let got = cfg.fleet(fleet).and_then(|f| gh::closed(ctx, r, f, fleet, ctx.now));
        let list = got.unwrap_or_else(|e| {
            let _ = writeln!(
                out,
                "cannot list closed issues of {fleet} ({e:#}); only merged lanes are candidates"
            );
            vec![]
        });
        memo.insert(fleet.to_string(), list);
    }
    &memo[fleet]
}

/// herdr first; else `git worktree remove` (never `--force`) of the re-checked tree minus `OWN_FILES` and `SHOT_DIRS`.
pub fn remove(r: &On, cfg: &Config, fleet: &str, wt: &str, ws: &str) -> Result<()> {
    if !ws.is_empty() && herdr::worktree_remove(r, ws)? {
        return Ok(());
    }
    if let Some(why) = dirty(r, Path::new(wt)) {
        anyhow::bail!("{why}");
    }
    for f in OWN_FILES {
        r.remove(&Path::new(wt).join(f)).context(format!("removing {f}"))?;
    }
    for d in SHOT_DIRS.iter().map(|d| Path::new(wt).join(d)) {
        match r.machine {
            None if d.is_dir() => std::fs::remove_dir_all(&d).context(format!("removing {}", d.display()))?,
            None => {}
            Some(_) => drop(ok(r, "rm", &["-rf", &d.to_string_lossy()])?),
        }
    }
    // #157: on another host the repo checkout's path is its own; git finds it from the worktree.
    let local = match r.machine {
        None => cfg.fleet(fleet)?.local_path().to_string_lossy().into_owned(),
        Some(_) => wt.into(),
    };
    ok(r, "git", &["-C", &local, "worktree", "remove", wt])?;
    Ok(())
}

/// The agent's newest spawn (#478: on `host`), unless a kill of its pane followed it; else `NO_SUCH_AGENT`.
fn spawned<'a>(events: &'a [Event], agent: &str, host: Option<&str>) -> Result<&'a Event> {
    let i = events.iter().rposition(|e| {
        e.kind == "spawn" && e.str("agent") == Some(agent) && host.is_none_or(|h| e.str("host") == Some(h))
    });
    let killed = |e: &Event, sp: &Event| {
        matches!(e.kind.as_str(), "lane-killed" | "stale-pane-killed")
            && e.str("agent") == Some(agent)
            && (e.kind == "lane-killed" || e.str("pane") == sp.str("pane"))
    };
    match i.filter(|i| !events[*i..].iter().any(|e| killed(e, &events[*i]))) {
        Some(i) => Ok(&events[i]),
        None => crate::gate::refused("NO_SUCH_AGENT", format!("no live lane `{agent}` in the log")),
    }
}

/// `herdsman stop`: Esc to the agent's pane (it pauses); logs `stop`.
pub fn stop(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, agent: &str) -> Result<()> {
    let events = read(&ctx.state)?;
    let e = spawned(&events, agent, None)?;
    herdr::keys(&On::of(r, Some(e)), e.str("pane").unwrap_or_default(), "Escape")?;
    emit(
        &ctx.state,
        cfg,
        Event::new(ctx.now, &e.fleet, e.issue, "stop", json!({"agent": agent})),
    )?;
    Ok(writeln!(out, "sent Esc to {agent}")?)
}

/// `herdsman close`: close a non-lane herdr agent's pane (by name or pane id); files stay. Logs `closed-other`.
/// #478: `host` picks that machine's agent when names collide (a lane is only the one its newest spawn there).
pub fn close(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    agent: &str,
    host: Option<&str>,
) -> Result<()> {
    if let Some(h) = host.filter(|h| !cfg.hosts.contains_key(*h)) {
        return crate::gate::refused("BAD_VALUE", format!("--host {h}: no [hosts.{h}]"));
    }
    let on = On::new(r, host.and_then(|h| cfg.hosts[h].machine.as_deref()));
    let (events, agents) = (read(&ctx.state)?, herdr::agents(&on)?);
    let a = agents.iter().find(|a| a["name"] == agent || a["pane_id"] == agent);
    let name = a.and_then(|a| a["name"].as_str()).unwrap_or(agent);
    let here = |e: &Event| host.is_none_or(|h| e.str("host") == Some(h));
    if last_spawn(&events, agent)
        .or(last_spawn(&events, name))
        .is_some_and(here)
    {
        return crate::gate::refused("IS_LANE", format!("`{name}` is a lane: use stop, kill or clean"));
    }
    let pane = a.and_then(|a| a["pane_id"].as_str()).context("no such herdr agent")?;
    herdr::close(&on, pane)?;
    let ev = Event::new(ctx.now, "", None, "closed-other", json!({"agent": name}));
    emit(&ctx.state, cfg, ev)?;
    Ok(writeln!(out, "closed {name}'s pane; its worktree and files stay")?)
}

/// `herdsman kill`: the dry-run line (what is lost); with `yes`, unpushed or dirty work goes to
/// `wip/<agent>-<stamp>` first (refused `SECRET_IN_WIP`, naming `file:line`, on a real secret), then the pane closes.
/// The worktree is kept.
/// #478: `host` kills the agent's newest lane there; one an other host's lane superseded logs `stale-pane-killed`.
pub fn kill(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (agent, host): (&str, Option<&str>),
    yes: bool,
) -> Result<()> {
    let events = read(&ctx.state)?;
    let e = spawned(&events, agent, host)?;
    let stale = !last_spawn(&events, agent).is_some_and(|l| std::ptr::eq(l, e));
    let role = e.str("role").unwrap_or_default();
    let st = e.issue.and_then(|n| fold(&events).remove(&(e.fleet.clone(), n)));
    let on = On::of(r, Some(e));
    let wt = e.str("worktree").filter(|w| on.exists(Path::new(w)));
    let git = |a: &[&str]| ok(&on, "git", &[&["-C", wt.unwrap_or_default()][..], a].concat());
    let (dirty, ahead) = match wt {
        Some(_) => {
            let st = git(&["status", "--porcelain"])?;
            (
                changes(&st),
                git(&["rev-list", "--count", "HEAD", "--not", "--remotes"])?
                    .trim()
                    .parse()?,
            )
        }
        None => (0, 0),
    };
    let wip = (dirty + ahead > 0).then(|| wip_branch(ctx, r, agent));
    let pr = e.issue.zip(cfg.fleet.get(&e.fleet)).and_then(|(n, f)| {
        let b = crate::gate::branch_of(st.as_ref(), n);
        gh::pr(ctx, r, f, (&e.fleet, n), &b, ctx.now).ok()
    });
    let pr = pr
        .filter(|p| p["state"] == "OPEN")
        .map_or(String::new(), |p| format!(" · PR #{} open", p["number"]));
    let lost = match &wip {
        Some(w) => format!("{ahead} unpushed commit(s), {dirty} dirty file(s) → {w}"),
        None => "nothing unpushed".into(),
    };
    writeln!(
        out,
        "{agent} · {} · {lost}{pr} · pane closes, worktree kept",
        st.map_or(role.to_string(), |s| s.state)
    )?;
    if !yes {
        return Ok(writeln!(out, "dry run: nothing changed; pass --yes to kill")?);
    }
    if let Some(w) = &wip {
        match snapshot(r, wt.unwrap_or_default(), w, "wip: kill snapshot") {
            Err(Snap::Failed(e)) => {
                return crate::gate::refused(
                    "SNAPSHOT_FAILED",
                    format!("{agent}: wip snapshot failed ({e:#}); index restored, nothing pushed, pane kept"),
                )
            }
            Err(Snap::Secret(hit)) => {
                return crate::gate::refused(
                    "SECRET_IN_WIP",
                    format!("{agent}'s unpushed diff has a secret ({hit}); nothing pushed, pane kept"),
                )
            }
            Err(Snap::Push(e)) => return Err(e),
            Ok(()) => {}
        }
    }
    herdr::close(&on, e.str("pane").unwrap_or_default())?;
    drop_log(ctx, Some(agent));
    let d = json!({"agent": agent, "role": role, "pane": e.str("pane"), "host": e.str("host"), "wip": wip});
    let kind = ["lane-killed", "stale-pane-killed"][usize::from(stale)];
    emit(&ctx.state, cfg, Event::new(ctx.now, &e.fleet, e.issue, kind, d))?;
    Ok(writeln!(
        out,
        "killed {agent}{}",
        wip.map_or(String::new(), |w| format!(": work pushed to {w}"))
    )?)
}

/// #478: each pane a supersede left on a host it could not reach (`stale_host`), killed as `kill --host` does once
/// that host answers; a pane already gone there is only logged.
pub fn stale(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let events = read(&ctx.state)?;
    for s in events.iter().filter(|e| e.kind == "supersede") {
        let (Some(h), Some(agent), pane) = (s.str("stale_host"), s.str("agent"), s.str("stale_pane")) else {
            continue;
        };
        let done = |e: &Event| e.kind == "stale-pane-killed" && e.ts >= s.ts && e.str("agent") == Some(agent);
        let machine = cfg.hosts.get(h).and_then(|c| c.machine.as_deref());
        if events.iter().any(|e| done(e) && e.str("pane") == pane) || machine.is_none() {
            continue;
        }
        let Ok(there) = host::agents(&On::new(r, machine)) else {
            continue;
        }; // still offline
        if there
            .iter()
            .any(|a| a["name"] == agent && a["pane_id"].as_str() == pane)
        {
            if let Err(e) = kill(ctx, cfg, r, out, (agent, Some(h)), true) {
                writeln!(out, "stale pane {h}:{agent}: {e:#}")?;
            }
        } else {
            let d = json!({"agent": agent, "host": h, "pane": pane, "gone": true});
            emit(
                &ctx.state,
                cfg,
                Event::new(ctx.now, &s.fleet, s.issue, "stale-pane-killed", d),
            )?;
        }
    }
    Ok(())
}

/// `wip/<agent>-<YYYYmmdd-HHMM>` (the epoch when `date` is unreadable).
pub(crate) fn wip_branch(ctx: &Ctx, r: &dyn Runner, agent: &str) -> String {
    let stamp = crate::usage::date(r, ctx.now, "+%Y%m%d-%H%M");
    let ok = stamp.chars().all(|c| c.is_ascii_digit() || c == '-');
    format!("wip/{agent}-{}", if ok { stamp } else { ctx.now.to_string() })
}

/// #254: push `wt`'s unpushed or dirty work to `w` (`snapshot`, its secret scan first).
pub fn save_wip(r: &dyn Runner, wt: &str, w: &str, msg: &str) -> Result<()> {
    snapshot(r, wt, w, msg).map_err(|e| match e {
        Snap::Secret(hit) => anyhow::anyhow!("a secret in the diff ({hit})"),
        Snap::Failed(e) | Snap::Push(e) => e,
    })
}

/// Why a wip snapshot stopped: git failed (index restored), a secret in the diff (index restored), or the push.
enum Snap {
    Failed(anyhow::Error),
    Secret(String),
    Push(anyhow::Error),
}

/// Stage all but `OWN_FILES` and `SHOT_DIRS`, scan the staged and unpushed diff for a secret (#96, the first as `file:line: masked`),
/// commit any uncommitted change with `msg` and push HEAD to `w`.
fn snapshot(r: &dyn Runner, wt: &str, w: &str, msg: &str) -> std::result::Result<(), Snap> {
    let git = |a: &[&str]| ok(r, "git", &[&["-C", wt][..], a].concat());
    // Stage all, then unstage the scratch: an `:(exclude)` of an ignored file fails `git add`.
    let undo = |e: anyhow::Error| {
        let _ = git(&["reset", "-q"]);
        Snap::Failed(e)
    };
    let mut commit = false;
    let mut snap = || -> Result<(String, String)> {
        commit = changes(&git(&["status", "--porcelain"])?) > 0;
        git(&["add", "-A"])?;
        git(&[&["reset", "-q", "--"][..], OWN_FILES, SHOT_DIRS].concat())?;
        Ok((
            git(&["diff", "--cached"])?,
            git(&["log", "-p", "--format=", "HEAD", "--not", "--remotes"])?,
        ))
    };
    let (staged, log) = snap().map_err(undo)?;
    // The first added hunk line (a file header runs `diff --git` → `@@`) with a secret, as `file:line: masked`.
    let mut head = false;
    let mut file = "";
    let mut n = 0;
    let mut hit = None;
    for l in staged.lines().chain(log.lines()) {
        head = (head || l.starts_with("diff --git ")) && !l.starts_with("@@");
        if let Some((_, h)) = l.strip_prefix("@@ ").and_then(|h| h.split_once('+')) {
            n = h.split([',', ' ']).next().and_then(|s| s.parse().ok()).unwrap_or(1);
        } else if head {
            file = l.strip_prefix("+++ b/").unwrap_or(file);
        } else if let Some(a) = l.strip_prefix('+').filter(|_| hit.is_none()) {
            // Mask every secret on the line, not only the first.
            let m = crate::lanes::redact(a.trim());
            if m != a.trim() {
                hit = Some(format!("{file}:{n}: {m}"));
            }
        }
        if !head && l.starts_with(['+', ' ']) {
            n += 1;
        }
    }
    if let Some(hit) = hit {
        let _ = git(&["reset", "-q"]);
        return Err(Snap::Secret(hit));
    }
    if commit {
        // A host with no git identity still saves: herdsman signs only when none is configured.
        let unset = ["user.name", "user.email"].iter().any(|k| git(&["config", k]).is_err());
        let id: &[&str] = if unset {
            &["-c", "user.name=herdsman", "-c", "user.email=herdsman@localhost"]
        } else {
            &[]
        };
        git(&[id, &["commit", "-q", "--no-verify", "-m", msg]].concat()).map_err(undo)?;
    }
    git(&["push", "origin", &format!("HEAD:refs/heads/{w}")]).map_err(Snap::Push)?;
    Ok(())
}
