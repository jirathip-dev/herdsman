use crate::config::{Config, Fleet};
use crate::events::{age, checks, emit, fold, held_on, last_spawn, open_asks, plain, working, Event, IssueState, Log};
use crate::gate::gate;
use crate::host;
use crate::hosts::On;
use crate::runner::{ok, Runner};
use crate::spawn::{changes, dirty, own_files, SHOT_DIRS};
use crate::Ctx;
use crate::{gh, term};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

fn ready<'a>(ctx: &Ctx, r: &dyn Runner, cfg: &'a Config, s: &IssueState) -> Option<(&'a Fleet, Value)> {
    let f = cfg.fleet(&s.fleet).ok().filter(|_| s.state == "ready")?;
    let branch = crate::gate::branch_of(Some(s), s.issue);
    let pr = gh::pr(ctx, r, f, (&s.fleet, s.issue), &branch, ctx.now).ok()?;
    (pr["state"] == "OPEN").then_some((f, pr))
}

fn amendment(ctx: &Ctx, on: &On, cfg: &Config, s: &IssueState, wt: &str) -> Option<(String, String)> {
    let (f, r) = (cfg.fleet(&s.fleet).ok()?, on.r);
    let brief = on.read(&Path::new(wt).join(".brief.md"))?;
    let spec = gh::spec(ctx, r, f, &s.fleet, s.issue, true).ok()?;
    let url = spec["amended"].as_str().filter(|u| !brief.contains(*u))?;
    let n = s.issue;
    let fleet = &s.fleet;
    let line = format!("AMENDMENT on #{n} ({url}): run `drover brief {fleet} {n}` and apply it");
    Some((url.to_string(), line))
}

/// #709: the config for `check` and `tick`; failing to load, `config-invalid` alerts once per distinct error (to the last good config's notifiers) and the error stands; loading again after one logs `config-ok`.
pub fn config(ctx: &Ctx) -> Result<Config> {
    let (loaded, events) = (Config::loaded(ctx), working(&ctx.state)?);
    let kinds = ["config-invalid", "config-ok"];
    let last = events.iter().rev().find(|e| kinds.contains(&e.kind.as_str()));
    let was = last.filter(|e| e.kind == kinds[0]).and_then(|e| e.str("key"));
    let say = |cfg: &Config, kind, d| emit(&ctx.state, cfg, Event::new(ctx.now, "drover", None, kind, d));
    match loaded {
        Ok(cfg) if was.is_some() => say(&cfg, kinds[1], json!({})).map(|()| cfg),
        Ok(cfg) => Ok(cfg),
        Err((e, good)) => {
            let why = format!("{e:#}");
            if was != Some(why.as_str()) {
                say(
                    &good.map(|g| *g).unwrap_or_default(),
                    kinds[0],
                    json!({"key": why, "reason": why}),
                )?;
            }
            Err(e)
        }
    }
}

/// Each alert's dedup key is built from the issue's last activity: once per state, again after a change.
pub fn check(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let events = working(&ctx.state)?;
    PUSHES.set((0, 0));
    let _ = std::fs::write(ctx.state.with_file_name("check.at"), ctx.now.to_string());
    let agents = host::agents(r)
        .map_err(|e| eprintln!("drover check: the agent list failed ({e:#}); skipping lane checks"))
        .ok();
    if let Some(a) = &agents {
        crate::orchestrator::sleep_idle(ctx, cfg, r, &events, a)?;
        for t in events.iter().filter(|e| e.kind == "thread-new") {
            let id = t.str("thread").unwrap_or_default();
            if let Some((s, quiet)) = crate::orchestrator::hung(ctx, cfg, &events, a, id) {
                let agent = s.str("agent").unwrap_or_default();
                let why = format!("orchestrator {agent} working with no CPU or output for {}", age(quiet));
                let d = json!({"key": format!("{}:{agent}", s.ts), "agent": agent, "thread": id, "reason": why});
                fire(ctx, cfg, out, &events, (&t.fleet, None), "stall", d)?;
            }
        }
    }
    let stall = cfg.limits.stall_minutes * 60;
    let remote = crate::hosts::survey(ctx, cfg, r, false);
    crate::cloud::tick(ctx, cfg, r, &remote, out)?;
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
            fire(ctx, cfg, out, &events, ("drover", None), kind, d)?;
        }
    }
    let synced = working(&ctx.state)?;
    for name in cfg.hosts.keys() {
        let mut tries = (synced.iter().rev()).filter(|e| e.kind == "host-sync" && e.str("host") == Some(name));
        let Some(e) = tries.next().filter(|e| e.val("ok") == false) else {
            continue;
        };
        let since = tries.find(|e| e.val("ok") == true).map_or(0, |e| e.ts);
        let (from, to) = (e.str("from").unwrap_or("?"), e.str("to").unwrap_or("?"));
        let why =
            format!("{name}: drover install {from} → {to} failed (~/.drover-sync.log there); the next tick retries");
        let d = json!({"key": format!("{name}@{since}"), "host": name, "reason": why});
        fire(ctx, cfg, out, &synced, ("drover", None), "host-sync-failed", d)?;
    }
    for root in crate::roots::local(ctx, cfg) {
        let s = root.to_string_lossy();
        let was = |e: &&Event| e.str("root") == Some(&s) && matches!(e.kind.as_str(), "root-hung" | "root-answers");
        let last = events.iter().rev().find(was);
        let hung = !crate::runner::answers(&root);
        if hung != last.is_some_and(|e| e.kind == "root-hung") {
            let (kind, how) = [("root-answers", "answers again"), ("root-hung", "not answering")][usize::from(hung)];
            let d = json!({"key": last.map_or(0, |e| e.ts), "root": root, "reason": format!("root {s} {how}")});
            fire(ctx, cfg, out, &events, ("drover", None), kind, d)?;
        }
    }
    crate::setup::reap(ctx, r, &events)?;
    match crate::runners::tick(ctx, cfg, r, &events) {
        Some(Ok((on, want))) => {
            let was = |e: &&Event| matches!(e.kind.as_str(), "runners-low" | "runners-ok");
            let last = events.iter().rev().find(was);
            if (on < want) != last.is_some_and(|e| e.kind == "runners-low") {
                let kind = ["runners-ok", "runners-low"][usize::from(on < want)];
                let d = json!({"key": last.map_or(0, |e| e.ts), "reason": format!("CI runners {on}/{want} online")});
                fire(ctx, cfg, out, &events, ("drover", None), kind, d)?;
            }
        }
        Some(Err(e)) => eprintln!("drover check: CI runners: {e:#}"),
        None => {}
    }
    for (k, w) in cfg.warnings() {
        let d = json!({"key": k, "reason": format!("{k} is {w}")});
        fire(ctx, cfg, out, &events, ("drover", None), "config-warning", d)?;
    }
    for (name, f) in &cfg.fleet {
        let _ = crate::overlap::preview(ctx, cfg, r, name, &events);
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
        let ran = |e: &&Event| {
            e.fleet == *name && (e.kind == "lesson-skipped" || e.kind == "spawn" && e.str("role") == Some("lesson"))
        };
        let last = (events.iter().rfind(ran))
            .or_else(|| events.iter().find(|e| e.fleet == *name))
            .map(|e| e.ts)
            .filter(|t| !f.paused && f.lessons.as_deref() != Some("off") && ctx.now > t + 8 * 86400);
        if let Some(t) = last {
            let why = format!("no lessons issue for {} days", (ctx.now - t) / 86400);
            let d = json!({"key": t, "reason": why});
            fire(ctx, cfg, out, &events, (name, None), "lessons-missed", d)?;
        }
        for i in crate::queue::replay(&events, name) {
            let _ = gh::spec(ctx, r, f, name, i.issue, false);
        }
    }
    let events = closed_on_github(ctx, cfg, r, out, events)?;
    crate::drive::markers(ctx, cfg, r, true);
    let passes = crate::events::since(&ctx.state, &["drive-pass"], ctx.now.saturating_sub(2 * 86_400))?;
    let mut held = BTreeMap::<(&str, u64), (u64, u64)>::new();
    for e in &passes {
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
        let by = crate::drive::held_by(ctx, cfg, r);
        let by = if by.is_empty() {
            "no spawn starting now".into()
        } else {
            format!("slots held by {by}")
        };
        let why = format!(
            "{} held back by SPAWN_PENDING for {}: {by}",
            ns.join(", "),
            age(ctx.now - since)
        );
        let d = json!({"key": since, "reason": why});
        fire(ctx, cfg, out, &events, (name, None), "spawn-held", d)?;
    }
    let ids: std::collections::BTreeSet<_> = events
        .iter()
        .filter(|e| e.kind == "spawn-error")
        .filter_map(|e| Some((e.fleet.clone(), e.issue?)))
        .collect();
    for (fleet, n) in ids {
        let f = crate::drive::failures(&events, &fleet, n);
        if let Some((_, error)) = crate::drive::same_run(&f).filter(|r| r.0 >= 3) {
            let d = json!({"key": error, "error": error});
            fire(ctx, cfg, out, &events, (&fleet, Some(n)), "spawn-failing", d)?;
        }
    }
    let mut busy: Option<(u64, u64, Gate)> = None;
    for e in &passes {
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
        fire(ctx, cfg, out, &events, ("drover", None), "host-busy", d)?;
    }
    let undelivered = cfg.fleet.keys().flat_map(|f| crate::undelivered(&events, f));
    for u in undelivered.filter(|u| u.ts + 86_400 > ctx.now) {
        let mut d = u.data.clone();
        d.insert("key".into(), json!(u.str("id")));
        let to = (u.fleet.as_str(), u.issue);
        fire(ctx, cfg, out, &events, to, "answer-undelivered", d.into())?;
    }
    let silent = crate::shed::silent(ctx, cfg.limits.acp_start_minutes).into_iter();
    for (sp, agent, at) in silent.filter_map(|(a, at)| Some((last_spawn(&events, &a)?, a, at))) {
        let on = sp.str("account").map_or("its login".into(), |a| format!("account {a}"));
        let why = format!(
            "{agent}: failed start on {on}, no event {} after its first turn",
            age(ctx.now - at)
        );
        let d = json!({"key": format!("{agent}:{at}"), "agent": agent, "account": sp.str("account"), "reason": why});
        fire(ctx, cfg, out, &events, (&sp.fleet, sp.issue), "lane-failed-start", d)?;
    }
    for s in fold(&events).values().filter(|s| !s.done()) {
        let idle = ctx.now.saturating_sub(s.last_ts);
        let mut hits = vec![];
        let (mut capped, mut cause) = (None, None);
        if let (Some(agents), Some((role, lane))) = (&agents, s.active_lane()) {
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
            let wt = (spawned.filter(|e| crate::roots::skip(ctx, cfg, e).is_none())).and_then(|e| e.str("worktree"));
            let parked = status.is_none() && wt.is_some_and(|wt| dirty(r, Path::new(wt)).is_none());
            let key = if parked {
                format!("parked:{lane}")
            } else {
                format!("{}:{lane}", s.last_ts)
            };
            let mine = |e: &Event, sp| e.kind == "lane-closed" && crate::events::ends(e, sp) && e.ts >= sp.ts;
            let closed = spawned.is_some_and(|sp| events.iter().any(|e| mine(e, sp)));
            if status.is_none() && !closed {
                cause = crate::shed::exit_cause(&events, lane);
                let why = cause.map_or(String::new(), |c| format!(": {c}"));
                hits.push((
                    "lane-dead",
                    key.clone(),
                    format!("{role} lane {lane} is gone ({}){why}", s.state),
                ));
            }
            let account = spawned.and_then(|e| e.str("account"));
            let chain = cfg
                .fleet(&s.fleet)
                .is_ok_and(|f| f.role.get(role).is_some_and(|r| !r.fallback.is_empty()));
            if let (Some(p), Some(sp), true) = (pane, spawned, status.is_some() && (account.is_some() || chain)) {
                let screen = term::read(r, p).unwrap_or_default();
                let name = account.or(sp.str("harness")).unwrap_or_default();
                let (harness, host) = (sp.str("harness"), sp.str("host"));
                let h = crate::providers::get(harness.unwrap_or_default());
                let seen = match h.usage_limited(cfg, r, &screen, ctx.now) {
                    Some(u) => Some((crate::access::confirm(ctx, account, u), "")),
                    None if h.signed_out(&screen) => {
                        let (rest, none) = (ctx.now + cfg.limits.fallback_rest_minutes * 60, account.is_none());
                        let kind = ["account-signed-out", "agent-unavailable"][usize::from(none)];
                        Some(((kind, none.then_some(rest), "sign-in prompt".into()), ":auth"))
                    }
                    None => None,
                };
                if let Some(((kind, until, meter), auth)) = seen {
                    let why = match kind {
                        "cap-banner-ignored" => format!("{name}: {role} lane {lane} shows a limit banner but usage reads {meter}; {name} stays in rotation"),
                        "account-capped" | "agent-unavailable" if auth.is_empty() => format!("{name}: {role} lane {lane} hit the usage limit; spawns skip {name} for now"),
                        _ => {
                            let fix = account.map(|a| crate::access::sign_in(cfg, r, a).1);
                            let cmd = fix.and_then(|f| Some(f.split_once(" → ")?.1.rsplit(", or ").next()?.to_string()));
                            let fix = cmd.map_or("sign in".into(), |c| format!("sign in (`{c}`)"));
                            let fix = format!("{fix} or replace its token");
                            format!("{name}: {role} lane {lane} is signed out; spawns skip {name} until you {fix} (an account: then `drover accounts reset {name}`)")
                        }
                    };
                    hits.push((kind, format!("{lane}:{name}{auth}"), why));
                    let d = json!({"agent": lane, "harness": harness, "account": account, "host": host, "until": until, "meter": meter});
                    capped = Some((kind, d));
                }
            }
            if let Some(p) = pane.filter(|_| status.is_some()) {
                if let Some(f) = crate::lanes::log(&ctx.state, lane) {
                    crate::lanes::roll(&f, &crate::lanes::body(&term::recent(r, p, "200").unwrap_or_default()));
                }
                if let Some((url, line)) = wt.and_then(|wt| amendment(ctx, r, cfg, s, wt)) {
                    let key = format!("{lane}:{url}");
                    let seen = events.iter().any(|e| e.str("key") == Some(&key));
                    let dsh = spawned.and_then(|e| e.str("harness")) == Some("dsh");
                    if !seen && term::tell(r, dsh, lane, p, &line).is_ok() {
                        hits.push(("amended", key, line));
                    }
                }
            }
            let shed = agent.is_some_and(|a| a["runtime"] == "shed");
            let spare = agent.and_then(|a| a["pid"].as_u64()).map(|p| p.to_string());
            let working = crate::waiting::turn(&events, s).is_some_and(|t| t.str("state") == Some("working"));
            let trim = wt.filter(|_| {
                matches!(status, Some("done" | "idle"))
                    && idle > cfg.limits.idle_reap_minutes * 60
                    && !working
                    && (!shed || spare.is_some())
            });
            let trimmed = trim.map(|wt| host::reap(r, Path::new(wt), true, spare.as_deref()));
            if let Some((killed, mb)) = trimmed.filter(|t| !t.0.is_empty()) {
                let d = json!({"agent": lane, "killed": killed, "freed_mb": mb});
                let e = Event::new(ctx.now, &s.fleet, Some(s.issue), "lane-trimmed", d);
                emit(&ctx.state, cfg, e)?;
            }
            let between = shed && working;
            if idle > stall && status != Some("working") && !between && !parked && held_on(&events, s).is_empty() {
                let st = status.unwrap_or("gone");
                let pr = gh::cached_pr(ctx, &s.fleet, s.issue);
                let same = s.state.starts_with("fix") && pr["headRefOid"].as_str().is_some_and(|h| s.reviewed(h));
                let ci = cfg
                    .fleet(&s.fleet)
                    .map(|f| gh::ci(&gh::required(&pr["statusCheckRollup"], &f.required_checks)));
                let done = crate::waiting::fix_done(&events, &s.fleet, s.issue).is_some();
                let why = match ci {
                    Ok(_) if same && done => crate::promise::Say::Drive {
                        reason: "fix-done recorded".into(),
                        step: "spawn review".into(),
                    }
                    .line(),
                    Ok(ci) if same => format!(
                        "fix lane idle, head unchanged, CI {ci}: run drover done {} {} --no-change \"<why>\" or push",
                        s.fleet, s.issue
                    ),
                    _ => format!("{role} lane {lane} is {st}"),
                };
                hits.push((
                    "stall",
                    key,
                    format!("no event for {} in {}; {why}", age(idle), s.state),
                ));
            }
        }
        if let Some((f, pr)) = ready(ctx, r, cfg, s) {
            if let Some(why) = gh::missing(ctx, Some(r), cfg, (&s.fleet, s.issue), &events, &pr) {
                hits.push(("ci-missing", pr["headRefOid"].as_str().unwrap_or_default().into(), why));
            }
            if let Some(head) = gate(f, &pr, Some(s), false).ok().filter(|_| idle > stall) {
                let (short, ago) = (&head[..7.min(head.len())], age(idle));
                let why = format!("PASS @ {short} with green CI, unmerged for {ago}; run `drover merge`");
                hits.push(("ready-unmerged", format!("{}:{head}", s.last_ts), why));
            }
        }
        for (kind, key, reason) in hits {
            let mut d = json!({"key": key, "reason": reason});
            if let Some((_, serde_json::Value::Object(c))) = capped.as_ref().filter(|c| c.0 == kind) {
                c.iter().for_each(|(k, v)| d[k] = v.clone());
            }
            if let Some(c) = cause.filter(|_| kind == "lane-dead") {
                d["cause"] = json!(c);
            }
            fire(ctx, cfg, out, &events, (&s.fleet, Some(s.issue)), kind, d)?;
        }
    }
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
    let tracked = fold(&events);
    for name in agents.iter().flatten().filter_map(|a| a["name"].as_str()) {
        let lane = crate::spawn::shaped(cfg, &json!({"name": name}));
        let lane = lane.filter(|l| l.2.is_some() && !tracked.contains_key(&(l.0.to_string(), l.1)));
        if let Some(l) = lane.filter(|_| last_spawn(&events, name).is_none()) {
            let why = format!("agent {name} has no spawn event and matches no tracked issue");
            let d = json!({"key": name, "reason": why});
            fire(ctx, cfg, out, &events, (l.0, Some(l.1)), "orphan-lane", d)?;
        }
    }
    let all: Vec<Value> = (agents.iter().flatten())
        .chain(remote.values().flatten().flatten())
        .cloned()
        .collect();
    for ((fl, n, role), names) in dups(&events, &all) {
        let why = format!(
            "lane {fl}:{n}:{role} runs {} processes ({}); the next drive pass stops all but the newest",
            names.len(),
            names.join(", ")
        );
        let d = json!({"key": names.join(","), "reason": why});
        fire(ctx, cfg, out, &events, (&fl, Some(n)), "lane-dup", d)?;
    }
    let jam = host::keychain_jam(r);
    let last = events
        .iter()
        .rev()
        .find(|e| matches!(e.kind.as_str(), "keychain-jammed" | "keychain-clear"));
    if jam.0.len() > cfg.limits.keychain_readers {
        let key = events.iter().rev().find(|e| e.kind == "keychain-clear");
        let why = format!("Keychain readers {}", host::jam_line(&jam));
        let d = json!({"key": key.map_or(0, |e| e.seq), "reason": why});
        fire(ctx, cfg, out, &events, ("drover", None), "keychain-jammed", d)?;
        if cfg.limits.reap_keychain_readers {
            let pids: Vec<String> = jam.0.iter().map(|p| p.0.clone()).collect();
            let _ = r.run("kill", &pids);
            let agent = jam.1.map(|a| a.0);
            if let Some(a) = &agent {
                let _ = r.run("kill", std::slice::from_ref(a));
            }
            let e = Event::new(
                ctx.now,
                "drover",
                None,
                "keychain-reap",
                json!({"pids": pids, "agent": agent}),
            );
            emit(&ctx.state, cfg, e)?;
        }
    } else if last.is_some_and(|e| e.kind == "keychain-jammed") {
        let e = Event::new(ctx.now, "drover", None, "keychain-clear", json!({}));
        crate::events::append(&ctx.state, &e)?;
    }
    for (e, lane, ts) in crate::outbox::stale(r, &events, (ctx.now, cfg.limits.outbox_stale_minutes * 60)) {
        let why = format!(
            "{lane}'s outbox holds a line from {} ago that drive has not delivered",
            age(ctx.now.saturating_sub(ts / 1000))
        );
        let d = json!({"key": format!("{lane}:{ts}"), "agent": e.str("agent"), "reason": why});
        fire(ctx, cfg, out, &events, (&e.fleet, e.issue), "outbox-stale", d)?;
    }
    let events = expire(ctx, cfg, out, events, agents.as_deref(), &remote)?;
    let open = open_asks(&events);
    let st = fold(&events);
    let ended = |a: &&Value| matches!(a["agent_status"].as_str(), Some("done" | "idle"));
    for a in agents.iter().flatten().filter(ended) {
        let e = a["name"].as_str().and_then(|n| last_spawn(&events, n));
        let (Some(name), Some(pane), Some(e)) = (a["name"].as_str(), a["pane_id"].as_str(), e) else {
            continue;
        };
        let (Some(n), Some(wt), None) = (e.issue, e.str("worktree"), On::of(r, Some(e)).machine) else {
            continue;
        };
        let next = ["spawn", "verdict"][usize::from(checks(e))];
        let live = (st.get(&(e.fleet.clone(), n)).and_then(IssueState::active_lane)).is_some_and(|l| l.1 == name);
        let new = |x: &Event| x.ts >= e.ts && x.is(next, &e.fleet, n) && (next == "verdict" || checks(x));
        let acted = !live && events.iter().any(new);
        if a["agent_status"] == "idle" && !acted {
            continue;
        }
        let wait = [cfg.limits.done_close_minutes * 60, 0][usize::from(acted)];
        let asking = open.iter().any(|q| q.str("agent") == Some(name));
        let due = handoff(r, &events, e, (n, wt)).filter(|t| ctx.now >= t + wait);
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
        if term::close(r, pane).is_ok() {
            let d = json!({"key": format!("{name}:{at}"), "agent": name, "freed_mb": mb, "reason": "done"});
            fire(ctx, cfg, out, &events, (&e.fleet, Some(n)), "lane-closed", d)?;
        }
    }
    let routine = |a: &&&Event| a.str("kind") == Some("routine");
    let due = |a: &&&Event| ctx.now.saturating_sub(a.ts) >= cfg.limits.ask_default_minutes * 60;
    for a in open.iter().filter(routine).filter(due) {
        let rec = a.val("recommend").to_string();
        crate::answer_with(ctx, cfg, r, out, a, &rec, ("default", None), (None, None))?;
    }
    let waiting = |a: &&&Event| {
        !routine(a) && a.str("from").is_none() && ctx.now.saturating_sub(a.ts) > cfg.limits.ask_waiting_minutes * 60
    };
    for a in open.iter().filter(waiting) {
        let why = format!(
            "{} unanswered for {}: {}",
            a.str("id").unwrap_or("?"),
            age(ctx.now.saturating_sub(a.ts)),
            a.str("question").unwrap_or("?")
        );
        let day = ctx.now.saturating_sub(a.ts) / 86_400;
        let key = a
            .str("id")
            .map(|id| [id.to_string(), format!("{id}@{day}d")][usize::from(day > 0)].clone());
        let d = json!({"key": key, "reason": why});
        fire(ctx, cfg, out, &events, (&a.fleet, a.issue), "ask-waiting", d)?;
    }
    if let (sent, held @ 1..) = PUSHES.replace((0, 0)) {
        let d = json!({"key": ctx.now, "reason": format!("{} questions waiting — open the board", sent + held)});
        fire(ctx, cfg, out, &events, ("drover", None), "ask-waiting", d)?;
    }
    let subs = crate::webpush::load(&ctx.state).len();
    if subs > 0 && cfg.webpush_sub().is_none() {
        let why = format!("{subs} device(s) subscribed, but no type = \"webpush\" notifier: alerts are off");
        fire(
            ctx,
            cfg,
            out,
            &events,
            ("drover", None),
            "alerts-off",
            json!({"key": subs, "reason": why}),
        )?;
    }
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
    if let Some(a) = &agents {
        leftovers(ctx, cfg, r, a, true)?;
    }
    let _ = stamp(ctx, &measure(ctx, cfg));
    Ok(())
}

fn handoff(r: &dyn Runner, events: &[Event], e: &Event, (n, wt): (u64, &str)) -> Option<u64> {
    if crate::events::design(e) {
        let t = ok(r, "git", &["-C", wt, "log", "-1", "--format=%ct"]).ok()?;
        let t = t.trim().parse().ok();
        return t.filter(|t| *t > e.ts);
    }
    let by = |x: &&Event| x.ts >= e.ts && (x.is("pushed", &e.fleet, n) || x.is("verdict", &e.fleet, n));
    events.iter().rev().find(by).map(|x| x.ts)
}

thread_local!(static PUSHES: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) });

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

fn expire(
    ctx: &Ctx,
    cfg: &Config,
    out: &mut dyn Write,
    events: Arc<Log>,
    agents: Option<&[Value]>,
    remote: &BTreeMap<String, Option<Vec<Value>>>,
) -> Result<Arc<Log>> {
    let sweep = !events.iter().any(|e| e.kind == "asks-swept") && events.iter().any(|e| e.kind == "needs-decision");
    let lists = || agents.into_iter().flatten().chain(remote.values().flatten().flatten());
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
        (d["key"], d["id"], d["ask_seq"]) = (json!(id), json!(id), json!(q.seq));
        fire(ctx, cfg, out, &events, (&q.fleet, q.issue), kind, d)?;
        n += 1;
    }
    if sweep {
        let ev = Event::new(ctx.now, "drover", None, "asks-swept", json!({"count": n}));
        if n > 0 {
            writeln!(out, "{}", plain(&ev))?;
        }
        crate::events::append(&ctx.state, &ev)?;
    }
    Ok(if n > 0 { working(&ctx.state)? } else { events })
}

/// After `drover merge`: close the issue's clean lane panes, then `drover clean --yes` them.
pub fn teardown(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, fleet: &str, issue: u64) -> Result<()> {
    let events = working(&ctx.state)?;
    let mut seen = std::collections::BTreeSet::new();
    for e in events.iter().rev().filter(|e| e.is("spawn", fleet, issue)) {
        let (Some(wt), Some(pane), on) = (e.str("worktree"), e.str("pane"), On::of(r, Some(e))) else {
            continue;
        };
        if crate::events::design(e) || !seen.insert(wt) || crate::roots::skip(ctx, cfg, e).is_some() {
            continue;
        }
        if !on.exists(Path::new(wt)) {
            continue;
        }
        match dirty(&on, Path::new(wt)) {
            Some(why) => writeln!(out, "keep pane {pane} ({wt}): {why}")?,
            None => drop(term::close(&on, pane)),
        }
    }
    if seen.is_empty() {
        return Ok(());
    }
    clean(ctx, cfg, r, out, Some(fleet), (Some(issue), None, false), true)
}

/// Remove worktree + pane of lanes whose issue is merged (or closed on GitHub). Dry run unless `yes`. `--lane` (one agent's worktree) refuses `UNSAFE_CLEAN` with the reason when it cannot go.
pub fn clean(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    fleet: Option<&str>,
    (issue, lane, sched): (Option<u64>, Option<&str>, bool),
    yes: bool,
) -> Result<()> {
    clean_all(ctx, cfg, r, out, fleet, (issue, lane, sched, false), yes)
}

/// `clean`; #678: `all` (an explicit `drover clean`) also sweeps each disabled host that answers its one bounded probe: its worktrees and dead lanes' scratch.
pub fn clean_all(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    fleet: Option<&str>,
    (issue, lane, sched, all): (Option<u64>, Option<&str>, bool, bool),
    yes: bool,
) -> Result<()> {
    let events = working(&ctx.state)?;
    let states = fold(&events);
    let agents = crate::hosts::all_agents(cfg, r).context("cannot list the live agents; refusing to clean blind")?;
    let off = if all { disabled(cfg, r) } else { vec![] };
    let mut seen = std::collections::BTreeSet::new();
    let mut failed = 0;
    let (mut freed, mut removed) = (0, 0);
    let mut closed = BTreeMap::new();
    let mut idle: BTreeMap<String, u64> = crate::db::get(&ctx.state, IDLE).map(|v| v.1).unwrap_or_default();
    let mut fresh = std::collections::BTreeSet::new();
    let revs: Vec<Event> = (cfg.fleet.iter().filter(|(n, _)| fleet.is_none_or(|f| f == n.as_str())))
        .flat_map(|(name, f)| {
            let dirs = (crate::runner::answers(&ctx.worktrees))
                .then(|| std::fs::read_dir(ctx.worktrees.join(f.repo_name())))
                .into_iter()
                .flatten()
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
    let spawns: Vec<&Event> = (events.iter().rev())
        .filter(|e| matches!(e.kind.as_str(), "spawn" | "spawn-started") && fleet.is_none_or(|f| f == e.fleet))
        .chain(&revs)
        .filter(|e| e.issue.is_some_and(|n| issue.is_none_or(|i| i == n)))
        .filter(|e| lane.is_none_or(|a| e.str("agent") == Some(a)))
        .collect();
    let held = |e: &Event| {
        let away = e.str("host").filter(|_| e.str("machine").is_some());
        let held = match away.filter(|h| cfg.hosts.get(*h).is_some_and(|c| !c.enabled)) {
            Some(h) => !off.iter().any(|o| o.0 == h && o.2.is_some()),
            None => crate::hosts::offline(ctx, Some(e)).is_some(),
        };
        let root = crate::roots::skip(ctx, cfg, e).map(|w| match w {
            "off-root" => "off-root (not cleaned)",
            "root-hung" => "root-hung (not cleaned)",
            _ => "root unreachable (not cleaned)",
        });
        held.then_some("host unreachable (not cleaned)").or(root)
    };
    let there = crate::hosts::present(r, spawns.iter().copied().filter(|e| held(e).is_none()));
    for e in spawns {
        let (Some(n), Some(wt)) = (e.issue, e.str("worktree")) else {
            continue;
        };
        let (on, held) = (On::of(r, Some(e)), held(e));
        let away = e.str("host").filter(|_| on.machine.is_some());
        let gone = |c: &Event| c.kind == "clean" && c.str("worktree") == Some(wt) && c.ts >= e.ts;
        let gone = || held.map_or_else(|| !there.contains(&(on.machine, wt)), |_| events.iter().any(gone));
        if seen.contains(wt) || gone() {
            continue;
        }
        let unsafe_lane =
            |why: &str| crate::gate::refused("UNSAFE_CLEAN", format!("{}: {why}", lane.unwrap_or_default()));
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
        let stale = states
            .get(&(e.fleet.clone(), n))
            .is_some_and(|s| ctx.now >= s.last_ts + 3 * 86_400);
        if sched && !stale {
            continue;
        }
        if sched && removed == 10 {
            writeln!(out, "stop: 10 removed this run; the rest go next run")?;
            break;
        }
        seen.insert(wt);
        let pane = e.str("pane").unwrap_or_default();
        let shown = away.map_or(wt.into(), |h| format!("{h}:{wt}"));
        if let Some(why) = held {
            if lane.is_some() {
                return unsafe_lane(why);
            }
            writeln!(out, "keep {shown}: {why}")?;
            continue;
        }
        let gitdir = on.read(&Path::new(wt).join(".git"));
        let gitdir = gitdir
            .as_deref()
            .and_then(|g| g.strip_prefix("gitdir: "))
            .map(str::trim);
        if let Some(gd) = gitdir.filter(|g| g.starts_with('/') && !on.exists(Path::new(g))) {
            let there = away.unwrap_or("this host");
            let why = format!("its gitfile points at {gd}, not on {there}; fix: on {there}, check it holds nothing you need, then `rm -rf '{wt}'`");
            if lane.is_some() {
                return unsafe_lane(&format!("orphan (wrong host): {why}"));
            }
            writeln!(out, "orphan (wrong host) {shown}: {why}")?;
            continue;
        }
        let agent = e.str("agent").unwrap_or_default();
        let there: Vec<Value> = (agents.iter())
            .filter(|a| a["host"].as_str() == away)
            .cloned()
            .collect();
        let here = host::live_in(&there, agent, Path::new(wt));
        let mine = |a: &Value| a["name"] == agent || a["name"].is_null();
        let own = |a: &&Value| mine(a) && a["agent_status"] != "working";
        let key = format!("merged-idle:{agent}:{}", e.ts);
        let refused = events
            .iter()
            .any(|x| x.kind == "lane-kept-dirty" && x.str("key") == Some(&key));
        let since = (!here.is_empty() && here.iter().all(own)).then(|| *idle.entry(agent.into()).or_insert(ctx.now));
        if since.is_some() {
            fresh.insert(agent.to_string());
        }
        let left = since.map(|t| (t + cfg.limits.idle_merged_minutes * 60).saturating_sub(ctx.now));
        let mut live = !here.is_empty();
        let mut why_live = match left {
            Some(m @ 1..) if !refused => format!("live agent (idle; its pane closes in {} min)", m.div_ceil(60)),
            _ => "live agent".to_string(),
        };
        if left == Some(0) && !refused && !yes {
            writeln!(out, "would close {agent}'s idle pane {pane} (work to wip/ first)")?;
            live = false;
        } else if left == Some(0) && !refused {
            let shut = match spawned(&events, agent, e.str("host")) {
                Ok(_) => kill_as(ctx, cfg, r, out, (agent, e.str("host")), true, Some("merged-idle")),
                Err(_) => close_gone(&on, agent, wt, pane),
            };
            match shut {
                Ok(()) => live = false,
                Err(err) => {
                    why_live = format!("pane kept: {err:#}");
                    if err.downcast_ref::<crate::gate::Refusal>().is_some() {
                        let d = json!({"key": key, "agent": agent, "why": why_live, "reason": format!("{agent} stays open: {why_live}")});
                        fire(ctx, cfg, out, &events, (&e.fleet, Some(n)), "lane-kept-dirty", d)?;
                    }
                }
            }
        }
        let keep = if live {
            Some(why_live)
        } else {
            if yes {
                reap(ctx, cfg, &on, (&e.fleet, Some(n)), wt)?;
                if on.machine.is_none() {
                    freed += purge(r, cfg, &e.fleet, out, Path::new(wt), false)?;
                }
            }
            dirty(&on, Path::new(wt))
        };
        let saved = keep.is_some() && yes && !live && lane.is_none();
        let keep = match keep {
            Some(why) if !yes && !live && lane.is_none() => {
                writeln!(out, "would save {shown} to wip/ ({why}), then remove it")?;
                continue;
            }
            Some(_) if saved => save(ctx, cfg, &on, out, &events, e, wt)?,
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
        if let Err(err) = remove(&on, cfg, &e.fleet, wt, saved) {
            writeln!(out, "failed {shown}: {err:#}")?;
            failed += 1;
            continue;
        }
        if !pane.is_empty() {
            let _ = term::close(&on, pane);
        }
        if on.machine.is_none() {
            crate::providers::remove_tmp(&stray_roots(cfg), wt);
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
    let sheds = r.shed().filter(|_| lane.is_none()).map(term::Shed);
    for (b, t) in sheds
        .iter()
        .flat_map(|b| b.exited(r).unwrap_or_default().into_iter().map(move |t| (b, t)))
    {
        let (fl, n) = (&t.labels["fleet"], t.labels.get("issue").and_then(|i| i.parse().ok()));
        let Some(n) = n.filter(|n| issue.is_none_or(|i| i == *n) && fleet.is_none_or(|f| f == fl)) else {
            continue;
        };
        let why = if states.get(&(fl.clone(), n)).is_some_and(|s| s.state == "merged") {
            "merged"
        } else if cfg.fleet.contains_key(fl) && closed_issues(ctx, cfg, r, out, &mut closed, fl).contains(&n) {
            "issue closed"
        } else {
            continue;
        };
        if yes {
            b.rm(r, &t.name)?;
        }
        let did = if yes { "removed" } else { "would remove" };
        writeln!(out, "{did} exited shed terminal {} ({fl}#{n} {why})", t.name)?;
    }
    if lane.is_none() && issue.is_none() {
        sweep_off(ctx, cfg, r, out, &off, !yes)?;
    }
    if yes {
        idle.retain(|n, _| {
            fresh.contains(n) || (agents.iter()).any(|a| a["name"] == n.as_str() && a["agent_status"] != "working")
        });
        crate::db::put(&ctx.state, IDLE, ctx.now, &idle)?;
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

const IDLE: &str = "idle-merged";

pub fn scheduled(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let res = clean(ctx, cfg, r, out, None, (None, None, true), true);
    let mark = ctx.state.with_file_name("clean.failed");
    let Err(e) = res else {
        let _ = std::fs::remove_file(&mark);
        return Ok(());
    };
    let why = format!("scheduled clean: {e:#}");
    if std::fs::read_to_string(&mark).ok().as_deref() != Some(why.as_str()) {
        std::fs::write(&mark, &why)?;
        let d = json!({"key": "clean", "reason": why});
        emit(&ctx.state, cfg, Event::new(ctx.now, "drover", None, "clean-failed", d))?;
    }
    Err(e)
}

/// #186: TERM, then KILL, the processes inside `wt`; logs `lane-reaped` when any were signalled.
pub fn reap(ctx: &Ctx, cfg: &Config, r: &dyn Runner, (fleet, issue): (&str, Option<u64>), wt: &str) -> Result<usize> {
    let names = host::reap(r, Path::new(wt), false, None).0;
    if !names.is_empty() {
        let d = json!({"worktree": wt, "killed": names.len(), "names": names});
        emit(&ctx.state, cfg, Event::new(ctx.now, fleet, issue, "lane-reaped", d))?;
    }
    Ok(names.len())
}

pub fn leftovers(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    agents: &[serde_json::Value],
    yes: bool,
) -> Result<Vec<(String, String, String)>> {
    let (events, ps, mut seen, mut found) = (
        crate::events::working(&ctx.state)?,
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
const BUILDERS: &[&str] = &["cargo", "rustc", "node", "npm", "npx", "pnpm", "yarn", "bun"];
/// #103: delete a lane's build folders (#164: plus the fleet's `build_dirs`), even when the worktree stays; the bytes freed (#361: with `dry`, listed and counted, none removed). One git does not ignore stays.
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

fn save(
    ctx: &Ctx,
    cfg: &Config,
    r: &On,
    out: &mut dyn Write,
    events: &[Event],
    e: &Event,
    wt: &str,
) -> Result<Option<String>> {
    let agent = e.str("agent").unwrap_or_default();
    let git = |a: &[&str]| ok(r, "git", &[&["-C", wt][..], a].concat());
    let (w, sha) = match snapshot(r, wt, &wip_branch(ctx, r, agent), "wip: auto-saved by drover") {
        Err(Snap::Secret(hit)) => {
            let why = format!("unsaved work has a secret ({hit})");
            let d = json!({"key": wt, "agent": agent, "reason": format!("{wt}: {why}; kept")});
            fire(ctx, cfg, out, events, (&e.fleet, e.issue), "wip-blocked", d)?;
            return Ok(Some(why));
        }
        Err(Snap::Failed(err) | Snap::Push(err)) => return Ok(Some(format!("wip save failed: {err:#}"))),
        Ok((w, _)) => (w, git(&["rev-parse", "HEAD"])?.trim().to_string()),
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

/// #164: the worktree total by a full walk (drive's pass, never a request), stamped for spawn's `DISK_BUDGET` and the Host tab. Over `worktree_budget_gb`, the idlest lanes' build folders go first (`budget-freed`), never a working one's.
pub fn budget(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let usage = measure(ctx, cfg);
    let mut total = usage.total;
    let over = |t: u64| gb(t) > cfg.limits.worktree_budget_gb;
    let builds = builds(ctx, cfg);
    let live = if over(total) || builds.iter().any(|b| b.is_dir()) {
        live_lanes(ctx, cfg, r)?
    } else {
        Live::new()
    };
    if over(total) {
        let (events, agents, ps) = (working(&ctx.state)?, host::agents(r)?, host::cwds(r));
        let states = fold(&events);
        let since = ctx.now.saturating_sub(cfg.limits.stall_minutes * 60);
        let working = |a: &str| (agents.iter()).any(|x| x["name"] == a && x["agent_status"] == "working");
        let mut seen = std::collections::BTreeSet::new();
        let mut lanes: Vec<(u64, &Event, &str)> = (events.iter().rev())
            .filter(|e| e.kind == "spawn" && e.str("role") != Some("orch"))
            .filter_map(|e| Some((e, e.str("worktree")?)))
            .filter(|(e, _)| crate::roots::skip(ctx, cfg, e).is_none())
            .filter(|(_, w)| Path::new(w).starts_with(&ctx.worktrees) && Path::new(w).exists() && seen.insert(*w))
            .filter(|(e, _)| e.ts < since && !e.str("agent").is_some_and(working))
            .map(|(e, w)| {
                let s = e.issue.and_then(|n| states.get(&(e.fleet.clone(), n)));
                (s.map_or(e.ts, |s| s.last_ts), e, w)
            })
            .collect();
        lanes.sort_by_key(|l| l.0);
        let (mut bytes, mut names) = (0, vec![]);
        for (_, e, wt) in lanes {
            if !over(total) {
                break;
            }
            let agent = e.str("agent").unwrap_or_default();
            let caches: Vec<_> = (builds.iter().map(|b| b.join(agent)))
                .filter(|c| !agent.is_empty() && c.is_dir())
                .collect();
            let node = host::inside(&ps, Path::new(wt))
                .into_iter()
                .any(|p| BUILDERS.contains(&p.1.as_str()));
            if node || caches.iter().any(|c| building(c, 1)) {
                writeln!(out, "budget: keep {wt}: a build runs")?;
                continue;
            }
            let mut freed = purge(r, cfg, &e.fleet, out, Path::new(wt), false)?;
            total = total.saturating_sub(freed);
            freed += (caches.iter().map(|c| (du(c), std::fs::remove_dir_all(c))))
                .filter_map(|(n, res)| res.ok().map(|()| n))
                .sum::<u64>();
            if freed > 0 {
                writeln!(out, "budget: freed {freed} bytes of build folders of {wt} (waiting)")?;
                bytes += freed;
                names.push(agent);
            }
        }
        if !names.is_empty() {
            let why = format!("freed {:.1} GB from {}", gb(bytes), names.join(", "));
            let d = json!({"lanes": names, "bytes": bytes, "reason": why});
            emit(&ctx.state, cfg, Event::new(ctx.now, "all", None, "budget-freed", d))?;
        }
    }
    sweep(ctx, cfg, &On::new(r, None), "local", out)?;
    trim_builds(ctx, cfg, cfg.limits.build_budget_gb, &live, out, false)?;
    stamp(ctx, &if total == usage.total { usage } else { measure(ctx, cfg) })
}

/// #842: this host's worktrees (build folders aside) by one walk: a hard link or clone counts once across all roots.
pub struct Usage {
    pub total: u64,
    pub roots: Vec<(std::path::PathBuf, u64)>,
    pub top: Vec<(std::path::PathBuf, u64)>,
}

/// Each root's bytes (a missing one 0) and the 3 largest `<root>/<repo>/<lane>` folders.
pub fn measure(ctx: &Ctx, cfg: &Config) -> Usage {
    let ls = |p: &Path| std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path());
    let (mut seen, mut top, mut roots) = (Seen::new(), vec![], vec![]);
    for root in crate::roots::answering(ctx, cfg) {
        let mut sum = 0;
        for d in ls(&root).filter(|d| !d.ends_with(crate::roots::BUILD)) {
            let dir = std::fs::symlink_metadata(&d).is_ok_and(|m| m.is_dir());
            for p in if dir { ls(&d).collect() } else { vec![d] } {
                let b = walk(&p, &mut seen);
                sum += b;
                top.push((p, b));
            }
        }
        roots.push((root, sum));
    }
    top.sort_by_key(|a| std::cmp::Reverse(a.1));
    top.truncate(3);
    let total = roots.iter().map(|r| r.1).sum();
    Usage { total, roots, top }
}

fn stamp(ctx: &Ctx, u: &Usage) -> Result<()> {
    let mut s = format!("{} {}\n", u.total, ctx.now);
    for (kind, v) in [("root", &u.roots), ("top", &u.top)] {
        for (p, b) in v {
            s += &format!("{kind}\t{b}\t{}\n", p.display());
        }
    }
    Ok(std::fs::write(ctx.state.with_file_name("worktrees.du"), s)?)
}

/// The last walk's roots and largest folders, as a refusal names them.
pub fn worktree_why(ctx: &Ctx) -> String {
    let s = std::fs::read_to_string(ctx.state.with_file_name("worktrees.du")).unwrap_or_default();
    let of = |kind: &str| {
        let rows = s
            .lines()
            .filter_map(|l| l.strip_prefix(kind)?.strip_prefix('\t')?.split_once('\t'));
        let rows = rows.map(|(b, p)| format!("{p} {:.1} GB", gb(b.parse().unwrap_or(0))));
        rows.collect::<Vec<_>>().join(", ")
    };
    format!("measured {} (build folders aside); largest: {}", of("root"), of("top"))
}

/// #438: each enabled remote host's shared build folder trimmed to `build_budget_gb`; #485: after the tick's drive, one quick ssh per host (a disabled or offline one never holds the pass up). #651: then its scratch swept there.
pub fn trim_hosts(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let mut tried = std::collections::BTreeSet::new();
    let hosts = cfg.hosts.iter().filter(|h| h.1.enabled);
    for (name, m) in hosts.filter_map(|(n, h)| Some((n, h.machine.as_deref()?))) {
        if !tried.insert(m) {
            continue;
        }
        let on = On::quick(r, Some(m));
        if let Err(e) = trim_on(ctx, &on, cfg, cfg.limits.build_budget_gb, &Live::new(), out) {
            writeln!(out, "builds: {name}: {e:#}")?;
            continue;
        }
        if !cfg.limits.sweep_remote {
            continue;
        }
        if let Err(e) = sweep(ctx, cfg, &on, name, out) {
            writeln!(out, "scratch: {name}: {e:#}")?;
        }
    }
    Ok(())
}

pub type Live = std::collections::BTreeSet<String>;

type Off<'c> = Vec<(&'c str, &'c str, Option<Vec<Value>>)>;

fn disabled<'c>(cfg: &'c Config, r: &dyn Runner) -> Off<'c> {
    let hosts = cfg.hosts.iter().filter(|h| !h.1.enabled);
    let hosts = hosts.filter_map(|(n, h)| Some((n.as_str(), h.machine.as_deref()?)));
    hosts
        .map(|(n, m)| (n, m, crate::host::agents(&On::new(r, Some(m))).ok()))
        .collect()
}

fn sweep_off(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, off: &Off, dry: bool) -> Result<u64> {
    let mut freed = 0;
    for (name, m, agents) in off {
        let Some(agents) = agents else {
            writeln!(out, "scratch: {name}: host offline (not cleaned)")?;
            continue;
        };
        let on = On::new(r, Some(m));
        for b in build_dirs(ctx, cfg, &on) {
            let b = format!("{b}/{}", crate::spawn::SCRATCH);
            let res = scratch(&on, &b, &[]).and_then(|v| {
                let held = |n: &String| agents.iter().any(|a| a["name"] == n.as_str());
                let gone: Vec<String> = v.into_iter().map(|n| n.0).filter(|n| !held(n)).collect();
                if dry || gone.is_empty() {
                    Ok(gone.into_iter().map(|n| (n, 0)).collect())
                } else {
                    scratch(&on, &b, &gone)
                }
            });
            let gone = match res {
                Ok(g) => g,
                Err(e) => {
                    writeln!(out, "scratch: {name}: {e:#}")?;
                    continue;
                }
            };
            for (n, bytes) in gone {
                let p = format!("{b}/{n}");
                let line = if dry {
                    format!("would remove scratch {name}:{p}")
                } else {
                    format!("scratch: {name}: removed {p} {:.2} GB (no live lane there)", gb(bytes))
                };
                freed += bytes;
                writeln!(out, "{line}")?;
            }
        }
    }
    Ok(freed)
}

pub fn live_lanes(ctx: &Ctx, cfg: &Config, r: &dyn Runner) -> Result<Live> {
    let since = ctx.now.saturating_sub(cfg.limits.stall_minutes * 60);
    let events = working(&ctx.state)?;
    let spawned = (events.iter().filter(|e| e.kind == "spawn" && e.ts >= since)).filter_map(|e| e.str("agent"));
    let agents = host::agents(r)?;
    let names = agents.iter().filter_map(|a| a["name"].as_str());
    Ok(names.chain(spawned).map(String::from).collect())
}

fn sweep(ctx: &Ctx, cfg: &Config, on: &On, host: &str, out: &mut dyn Write) -> Result<()> {
    for b in build_dirs(ctx, cfg, on) {
        sweep_in(ctx, cfg, on, host, &format!("{b}/{}", crate::spawn::SCRATCH), out)?;
        sweep_in(ctx, cfg, on, host, &b, out)?;
    }
    Ok(())
}

fn finished(events: &[Event], states: &crate::events::States, i: usize) -> Option<&'static str> {
    let sp = &events[i];
    let (agent, role) = (sp.str("agent")?, sp.str("role").unwrap_or_default());
    let s = states.get(&(sp.fleet.clone(), sp.issue?))?;
    if s.done() {
        return Some("lane finished");
    }
    if s.lanes.get(role).map(String::as_str) != Some(agent) {
        return Some("superseded");
    }
    let ends = |e: &Event| match e.kind.as_str() {
        "verdict" | "design-review" => true,
        "turn-end" => e.str("state") == Some("done") && e.str("role") == Some(role),
        _ => false,
    };
    let mine = events[i + 1..]
        .iter()
        .filter(|e| e.fleet == sp.fleet && e.issue == sp.issue);
    mine.into_iter().any(ends).then_some("lane finished")
}

fn sweep_in(ctx: &Ctx, cfg: &Config, on: &On, host: &str, b: &str, out: &mut dyn Write) -> Result<()> {
    let names = scratch(on, b, &[])?;
    if names.is_empty() {
        return Ok(());
    }
    let scr = b.ends_with(crate::spawn::SCRATCH);
    let (agents, events) = (host::agents(on)?, working(&ctx.state)?);
    let states = fold(&events);
    let agent = |n: &str| agents.iter().find(|a| a["name"] == n);
    let spawn = |n: &str| (events.iter()).rposition(|e| e.kind == "spawn" && e.str("agent") == Some(n));
    let spawned = |n: &str| (events.iter()).any(|e| e.kind.starts_with("spawn") && e.str("agent") == Some(n));
    let low = host::free_gb(on).is_some_and(|g| g < cfg.limits.min_free_gb);
    let (mut why, total) = (BTreeMap::new(), names.len());
    for (n, _) in names {
        if agent(&n).is_some_and(|a| a["agent_status"] == "working") {
            continue;
        }
        let ended = spawn(&n).and_then(|i| finished(&events, &states, i));
        let unknown = !spawned(&n);
        let w = match ended {
            Some(w) => w,
            None if unknown && scr && agent(&n).is_none() => "lane ended",
            None if low && scr => "disk low",
            None => continue,
        };
        why.insert(n, w);
    }
    let gone: Vec<String> = why.keys().cloned().collect();
    let removed = if gone.is_empty() {
        vec![]
    } else {
        scratch(on, b, &gone)?
    };
    for (n, bytes) in &removed {
        let what = if scr { "scratch" } else { "builds" };
        writeln!(out, "{what}: {host}: removed {b}/{n} {:.2} GB ({})", gb(*bytes), why[n])?;
        if let Some(sp) = spawn(n).map(|i| &events[i]) {
            let d = json!({"agent": n, "bytes": bytes, "why": why[n], "host": host, "path": format!("{b}/{n}")});
            emit(
                &ctx.state,
                cfg,
                Event::new(ctx.now, &sp.fleet, sp.issue, "build-freed", d),
            )?;
        }
    }
    if on.machine.is_some() && scr {
        writeln!(
            out,
            "scratch: {host}: {} kept, {} removed",
            total - removed.len(),
            removed.len()
        )?;
    }
    Ok(())
}

fn scratch(on: &On, b: &str, rm: &[String]) -> Result<Vec<(String, u64)>> {
    if on.machine.is_some() {
        let args = crate::runner::argv(&["-c", SCRATCH, "sh", b]);
        let args = [args, rm.to_vec()].concat();
        let o = on.run("sh", &args)?;
        anyhow::ensure!(o.code == 0, "sweep failed ({}): {}", o.code, o.stderr.trim());
        let row = |l: &str| match l.split_once(' ') {
            Some((kb, n)) if !rm.is_empty() => Some((n.into(), kb.parse::<u64>().ok()? * 1024)),
            _ => (rm.is_empty() && !l.is_empty()).then(|| (l.into(), 0)),
        };
        return Ok(o.stdout.lines().filter_map(row).collect());
    }
    let root = crate::config::expand(b);
    let ls = std::fs::read_dir(&root).into_iter().flatten().flatten();
    let all = ls
        .filter(|e| e.path().is_dir() && !e.file_name().as_encoded_bytes().starts_with(b"."))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), 0));
    if rm.is_empty() {
        return Ok(all.collect());
    }
    let mut v: Vec<_> = rm.iter().map(|n| (du(&root.join(n)), n)).collect();
    v.sort_by(|a, b| b.cmp(a));
    let gone = |(s, n): (u64, &String)| {
        let d = root.join(n);
        (!building(&d, 3) && std::fs::remove_dir_all(&d).is_ok()).then(|| (n.clone(), s))
    };
    Ok(v.into_iter().filter_map(gone).collect())
}

const SCRATCH: &str = r#"b=$1; shift; case $b in "~/"*) b=$HOME/${b#"~/"};; esac
cd "$b" 2>/dev/null || exit 0
[ $# -eq 0 ] && { for d in */; do [ -d "$d" ] && echo "${d%/}"; done; exit 0; }
du -sk -- "$@" 2>/dev/null | sort -rn | while read -r s d; do
  l=$(find "$d" -maxdepth 4 -name .cargo-lock 2>/dev/null)
  [ -n "$l" ] && command -v lsof >/dev/null && [ -n "$(lsof $l 2>/dev/null)" ] && continue
  rm -rf "./${d:?}" && echo "$s $d"
done
exit 0"#;

fn building(d: &Path, depth: u8) -> bool {
    let held = std::fs::File::open(d.join(".cargo-lock")).is_ok_and(|f| f.try_lock().is_err());
    held || depth > 0
        && (std::fs::read_dir(d).into_iter().flatten().flatten())
            .any(|e| e.file_type().is_ok_and(|t| t.is_dir()) && building(&e.path(), depth - 1))
}

/// #536: the newest mtime of `d` and its entries.
pub fn mtime(d: &Path) -> u64 {
    let m = |p: &Path| {
        p.metadata()
            .map_or(0, |m| std::os::unix::fs::MetadataExt::mtime(&m).unsigned_abs())
    };
    let ls = std::fs::read_dir(d).into_iter().flatten().flatten();
    ls.map(|e| m(&e.path())).fold(m(d), u64::max)
}

fn builds(ctx: &Ctx, cfg: &Config) -> Vec<std::path::PathBuf> {
    let roots = crate::roots::answering(ctx, cfg);
    roots.into_iter().map(|r| r.join(crate::roots::BUILD)).collect()
}

/// `trim_root` on each of this host's build folders. Bytes freed.
pub fn trim_builds(ctx: &Ctx, cfg: &Config, budget: f64, live: &Live, out: &mut dyn Write, dry: bool) -> Result<u64> {
    builds(ctx, cfg)
        .iter()
        .map(|b| trim_root(b, budget, live, out, dry))
        .sum()
}

fn trim_root(root: &Path, budget: f64, live: &Live, out: &mut dyn Write, dry: bool) -> Result<u64> {
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
    let lane = |p: &Path, d: u8| {
        let p = if d > 0 { Some(p) } else { p.parent() };
        p.and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned())
    };
    let mut dirs: Vec<_> = profiles.chain(scratch).map(|(p, n)| (mtime(&p), p, n)).collect();
    dirs.sort();
    for (_, dir, depth) in dirs {
        if gb(total) <= budget {
            break;
        }
        if building(&dir, depth) {
            writeln!(out, "keep {}: cargo is building", dir.display())?;
            continue;
        }
        if lane(&dir, depth).is_some_and(|n| live.contains(&n)) {
            writeln!(out, "keep {}: a live lane", dir.display())?;
            continue;
        }
        let (size, why) = (du(&dir), if depth > 0 { "no live agent" } else { "not building" });
        let verb = if dry { "would trim" } else { "trimmed" };
        if dry || std::fs::remove_dir_all(&dir).is_ok() {
            writeln!(out, "builds: {verb} {} ({:.2} GB, {why})", dir.display(), gb(size))?;
            (total, freed) = (total.saturating_sub(size), freed + size);
        }
    }
    Ok(freed)
}

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

/// #536: Cargo build folders (holding `CACHEDIR.TAG` or `.rustc_info.json`) lanes left in `/tmp` and `$TMPDIR`, untouched a day and not building; listed, removed with `yes`.
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

fn build_dirs(ctx: &Ctx, cfg: &Config, on: &On) -> Vec<String> {
    let Some(h) = (cfg.hosts.values()).find(|h| h.machine.as_deref() == on.machine && on.machine.is_some()) else {
        return builds(ctx, cfg).iter().map(|b| b.display().to_string()).collect();
    };
    let roots = Some(h.roots()).filter(|v| !v.is_empty());
    let roots = roots.unwrap_or_else(|| vec![crate::roots::DEFAULT.into()]);
    roots.iter().map(|r| format!("{r}/{}", crate::roots::BUILD)).collect()
}

/// The shared build folders on `on`'s host trimmed to `budget` GB each; bytes freed.
pub fn trim_on(ctx: &Ctx, on: &On, cfg: &Config, budget: f64, live: &Live, out: &mut dyn Write) -> Result<u64> {
    if on.machine.is_none() {
        return trim_builds(ctx, cfg, budget, live, out, false);
    }
    let dirs = build_dirs(ctx, cfg, on);
    dirs.iter().map(|b| trim_remote(on, b, budget, out)).sum()
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
    let (events, agents, ps) = (working(&ctx.state)?, host::agents(r)?, host::cwds(r));
    let live = live_lanes(ctx, cfg, r)?;
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
    total += trim_builds(ctx, cfg, cfg.limits.build_budget_gb, &live, o, !yes)?;
    total += sweep_off(ctx, cfg, r, o, &disabled(cfg, r), !yes)?;
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

#[allow(clippy::cast_precision_loss)]
fn gb(bytes: u64) -> f64 {
    bytes as f64 / 1_073_741_824.0
}

/// The last walk's worktree total in GB (`check`'s or `budget`'s, both stamp `worktrees.du`) and when.
pub fn worktree_du(ctx: &Ctx) -> Option<(f64, Option<u64>)> {
    let s = std::fs::read_to_string(ctx.state.with_file_name("worktrees.du")).ok()?;
    let mut n = s.lines().next()?.split_whitespace();
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

const DIRS: &str = "cd && cd \"$1\" 2>/dev/null || exit 0; \
for d in */; do [ -d \"$d\" ] && [ ! -L \"${d%/}\" ] && du -sk \"$PWD/${d%/}\"; done; true";

fn drop_log(ctx: &Ctx, agent: Option<&str>) {
    let _ = crate::lanes::log(&ctx.state, agent.unwrap_or_default()).map(std::fs::remove_file);
}

/// Bytes of blocks used under `p` (symlinks not followed), like `du -k`, not apparent size; the disk row's total.
pub fn du(p: &Path) -> u64 {
    walk(p, &mut Seen::new())
}

type Seen = std::collections::HashSet<(u64, u64)>;

fn walk(p: &Path, seen: &mut Seen) -> u64 {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::read_dir(p).map_or(0, |d| d.flatten().map(|e| walk(&e.path(), seen)).sum()),
        Ok(m) => {
            let id = m.is_file().then(|| clone_id(p)).flatten();
            let id = id.or_else(|| (m.nlink() > 1).then(|| m.ino()));
            u64::from(id.is_none_or(|i| seen.insert((m.dev(), i)))) * m.blocks() * 512
        }
        Err(_) => 0,
    }
}

#[cfg(target_os = "macos")]
fn clone_id(p: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    #[repr(C)]
    struct AttrList([u16; 2], [u32; 5]);
    #[repr(C, packed)]
    struct Buf(u32, [u32; 5], u64, u64);
    extern "C" {
        fn getattrlist(
            path: *const std::ffi::c_char,
            list: *mut AttrList,
            buf: *mut Buf,
            size: usize,
            opts: u32,
        ) -> i32;
    }
    let path = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
    let mut list = AttrList([5, 0], [0x8000_0000, 0, 0, 0, 0x300]);
    let mut buf = Buf(0, [0; 5], 0, 0);
    // SAFETY: a NUL-terminated path, an attrlist and a buffer of the size passed, all live for the call.
    let rc = unsafe { getattrlist(path.as_ptr(), &raw mut list, &raw mut buf, size_of::<Buf>(), 0x21) };
    let (returned, id, flags) = (buf.1[4], buf.2, buf.3);
    (rc == 0 && returned & 0x300 == 0x300 && flags & 1 == 1).then_some(id)
}

#[cfg(not(target_os = "macos"))]
fn clone_id(_: &Path) -> Option<u64> {
    None
}

fn closed_on_github(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, ev: Arc<Log>) -> Result<Arc<Log>> {
    let st = fold(&ev);
    let mut any = false;
    for (name, f) in &cfg.fleet {
        let state = |n: u64| st.get(&(name.clone(), n));
        let mut open: Vec<u64> = (st.values().filter(|s| s.fleet == *name && !s.done()))
            .map(|s| s.issue)
            .collect();
        open.extend((crate::queue::replay(&ev, name).iter().map(|i| i.issue)).filter(|n| state(*n).is_none()));
        open.retain(|n| gh::cached_pr(ctx, name, *n)["state"] != "MERGED");
        let Some(closed) = (!open.is_empty())
            .then(|| gh::closed(ctx, r, f, name, ctx.now).ok())
            .flatten()
        else {
            continue;
        };
        for n in open.into_iter().filter(|n| closed.contains(n)) {
            writeln!(out, "{name} #{n} closed on GitHub")?;
            let d = json!({"by": "external", "why": "issue closed on GitHub"});
            emit(&ctx.state, cfg, Event::new(ctx.now, name, Some(n), "closed-other", d))?;
            any = true;
        }
    }
    if any {
        return working(&ctx.state);
    }
    Ok(ev)
}

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

pub fn remove(r: &On, cfg: &Config, fleet: &str, wt: &str, saved: bool) -> Result<()> {
    if let Some(why) = dirty(r, Path::new(wt)).filter(|_| !saved) {
        anyhow::bail!("{why}");
    }
    for f in own_files() {
        r.remove(&Path::new(wt).join(f)).context(format!("removing {f}"))?;
    }
    for d in SHOT_DIRS.iter().map(|d| Path::new(wt).join(d)) {
        match r.machine {
            None if d.is_dir() => std::fs::remove_dir_all(&d).context(format!("removing {}", d.display()))?,
            None => {}
            Some(_) => drop(ok(r, "rm", &["-rf", &d.to_string_lossy()])?),
        }
    }
    let local = match r.machine {
        None => cfg.fleet(fleet)?.local_path().to_string_lossy().into_owned(),
        Some(_) => wt.into(),
    };
    let mut args = vec!["-C", &local, "worktree", "remove", wt];
    if saved {
        args.insert(4, "--force");
    }
    ok(r, "git", &args)?;
    Ok(())
}

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

/// `drover stop`: Esc to the agent's pane (it pauses), or #946: a thread's orchestrator ended; logs `stop`.
pub fn stop(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, agent: &str) -> Result<()> {
    let events = working(&ctx.state)?;
    let thread = agent.strip_prefix("orchestrator-").unwrap_or(agent);
    let e = match spawned(&events, agent, None) {
        Err(_) if crate::orchestrator::find(&events, thread).is_ok() => {
            let a = crate::orchestrator::stop(ctx, cfg, r, thread)?;
            return Ok(writeln!(out, "stopped {a}")?);
        }
        e => e?,
    };
    term::keys(&On::of(r, Some(e)), e.str("pane").unwrap_or_default(), "Escape")?;
    emit(
        &ctx.state,
        cfg,
        Event::new(ctx.now, &e.fleet, e.issue, "stop", json!({"agent": agent})),
    )?;
    Ok(writeln!(out, "sent Esc to {agent}")?)
}

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
    let (events, agents) = (working(&ctx.state)?, term::agents(&on)?);
    let a = agents.iter().find(|a| a["name"] == agent || a["pane_id"] == agent);
    let name = a.and_then(|a| a["name"].as_str()).unwrap_or(agent);
    let here = |e: &Event| host.is_none_or(|h| e.str("host") == Some(h));
    if last_spawn(&events, agent)
        .or(last_spawn(&events, name))
        .is_some_and(here)
    {
        return crate::gate::refused("IS_LANE", format!("`{name}` is a lane: use stop, kill or clean"));
    }
    let pane = a.and_then(|a| a["pane_id"].as_str()).context("no such agent")?;
    term::close(&on, pane)?;
    let ev = Event::new(ctx.now, "", None, "closed-other", json!({"agent": name}));
    emit(&ctx.state, cfg, ev)?;
    Ok(writeln!(out, "closed {name}'s pane; its worktree and files stay")?)
}

/// `drover kill`: the dry-run line (what is lost); with `yes`, unpushed or dirty work goes to `wip/<agent>-<stamp>` first (refused `SECRET_IN_WIP`, naming `file:line`, on a real secret), then the pane closes. The worktree is kept. #478: `host` kills the agent's newest lane there; one an other host's lane superseded logs `stale-pane-killed`.
pub fn kill(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    lane: (&str, Option<&str>),
    yes: bool,
) -> Result<()> {
    kill_as(ctx, cfg, r, out, lane, yes, None)
}

fn kill_as(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (agent, host): (&str, Option<&str>),
    yes: bool,
    why: Option<&str>,
) -> Result<()> {
    let events = working(&ctx.state)?;
    let e = spawned(&events, agent, host)?;
    let stale = !last_spawn(&events, agent).is_some_and(|l| std::ptr::eq(l, e));
    let role = e.str("role").unwrap_or_default();
    let st = e.issue.and_then(|n| fold(&events).remove(&(e.fleet.clone(), n)));
    let on = On::of(r, Some(e));
    let wt = e.str("worktree").filter(|w| on.exists(Path::new(w)));
    let (dirty, ahead) = wt.map_or(Ok((0, 0)), |w| pending(&on, w))?;
    let wip = (dirty + ahead > 0).then(|| wip_branch(ctx, &on, agent));
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
    let wip = match wip {
        Some(w) => {
            let (w, skip) = save_on(&on, agent, wt.unwrap_or_default(), &w, "wip: kill snapshot")?;
            let _ = skip.is_empty()
                || writeln!(out, "skipped (build output or over the size caps): {}", skip.join(", ")).is_ok();
            Some(w)
        }
        None => None,
    };
    close_gone(
        &on,
        agent,
        e.str("worktree").unwrap_or("\0"),
        e.str("pane").unwrap_or_default(),
    )?;
    drop_log(ctx, Some(agent));
    let d = json!({"agent": agent, "role": role, "pane": e.str("pane"), "host": e.str("host"), "wip": wip, "why": why});
    let kind = why.map_or(["lane-killed", "stale-pane-killed"][usize::from(stale)], |_| {
        "lane-closed"
    });
    emit(&ctx.state, cfg, Event::new(ctx.now, &e.fleet, e.issue, kind, d))?;
    Ok(writeln!(
        out,
        "killed {agent}{}",
        wip.map_or(String::new(), |w| format!(": work pushed to {w}"))
    )?)
}

/// #478: each pane a supersede left on a host it could not reach (`stale_host`), killed as `kill --host` does once that host answers; a pane already gone there is only logged.
pub fn stale(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let events = working(&ctx.state)?;
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
        };
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

/// #899: each lane key (fleet, issue, role) its newest spawns give two or more live agents, oldest first.
pub fn dups(events: &[Event], agents: &[Value]) -> Vec<((String, u64, String), Vec<String>)> {
    let mut by: BTreeMap<(String, u64, String), BTreeMap<u64, String>> = BTreeMap::new();
    for name in agents.iter().filter_map(|a| a["name"].as_str()) {
        let Some(sp) = last_spawn(events, name) else {
            continue;
        };
        if let (Some(n), Some(role)) = (sp.issue, sp.str("role")) {
            let key = (sp.fleet.clone(), n, role.to_string());
            by.entry(key).or_default().insert(sp.seq, name.into());
        }
    }
    let many = by.into_iter().filter(|(_, v)| v.len() > 1);
    many.map(|(k, v)| (k, v.into_values().collect())).collect()
}

/// #899: stop each live agent of lane `(fleet, issue, role)` but `keep` as `kill --yes` does; the agents stopped.
pub fn stop_rounds(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    (fleet, issue, role): (&str, u64, &str),
    keep: &str,
) -> Result<Vec<String>> {
    let events = working(&ctx.state)?;
    let (mut lists, mut seen, mut stopped) = (BTreeMap::new(), vec![keep], vec![]);
    let olds = (events.iter().rev()).filter(|e| e.is("spawn", fleet, issue) && e.str("role") == Some(role));
    for sp in olds {
        let agent = sp.str("agent").unwrap_or(keep);
        if seen.contains(&agent) || crate::hosts::held(ctx, cfg, sp) {
            continue;
        }
        seen.push(agent);
        let on = On::of(r, Some(sp));
        let live = lists
            .entry(on.machine)
            .or_insert_with(|| host::agents(&on).unwrap_or_default());
        if live.iter().any(|a| a["name"] == agent) {
            let lane = (agent, sp.str("host"));
            kill_as(ctx, cfg, r, &mut std::io::sink(), lane, true, Some("lane-dup"))?;
            stopped.push(agent.to_string());
        }
    }
    Ok(stopped)
}

/// #899: drive's pass: each lane key two or more live agents carry keeps only its newest.
pub fn dedup(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let (events, mut by) = (working(&ctx.state)?, BTreeMap::new());
    let on = |f: &str| cfg.fleet.get(f).is_some_and(|f| !f.paused);
    let spawns = (events.iter()).filter(|e| e.kind == "spawn" && on(&e.fleet));
    let keys = spawns.filter_map(|e| Some(((e.fleet.as_str(), e.issue?, e.str("role")?), e.str("agent")?)));
    if !keys.fold(false, |two, (k, a)| by.insert(k, a).is_some_and(|b| b != a) || two) {
        return Ok(());
    }
    let all = crate::hosts::all_agents(cfg, r)?;
    for ((fl, n, role), names) in dups(&events, &all).into_iter().filter(|d| on(&d.0 .0)) {
        let keep = names.last().map_or("", String::as_str);
        match stop_rounds(ctx, cfg, r, (&fl, n, &role), keep) {
            Ok(gone) => writeln!(out, "lane {fl}:{n}:{role}: stopped {}, kept {keep}", gone.join(", "))?,
            Err(e) => writeln!(out, "lane {fl}:{n}:{role}: {e:#}")?,
        }
    }
    Ok(())
}

fn pending(on: &On, wt: &str) -> Result<(usize, usize)> {
    let git = |a: &[&str]| ok(on, "git", &[&["-C", wt][..], a].concat());
    let dirty = changes(&git(&["status", "--porcelain"])?);
    let ahead = git(&["rev-list", "--count", "HEAD", "--not", "--remotes"])?;
    Ok((dirty, ahead.trim().parse().unwrap_or(0)))
}

/// #676: does worktree `wt` on its host hold dirty files or unpushed commits (unreadable: no)?
pub fn unsaved(on: &On, wt: &Path) -> bool {
    on.exists(wt) && pending(on, &wt.to_string_lossy()).is_ok_and(|p| p != (0, 0))
}

fn close_gone(on: &On, agent: &str, wt: &str, pane: &str) -> Result<()> {
    term::close(on, pane)?;
    for again in [true, false] {
        let left: Vec<String> = (host::live_in(&term::agents(on)?, agent, Path::new(wt)).iter())
            .filter(|a| a["name"] == agent || a["name"].is_null())
            .filter_map(|a| a["pane_id"].as_str().map(String::from))
            .collect();
        if left.is_empty() {
            return Ok(());
        }
        anyhow::ensure!(
            again,
            "{agent}'s pane {} is still open on its host after close",
            left.join(", ")
        );
        for p in &left {
            term::close(on, p)?;
        }
    }
    Ok(())
}

fn save_on(on: &On, agent: &str, wt: &str, w: &str, msg: &str) -> Result<(String, Vec<String>)> {
    match snapshot(on, wt, w, msg) {
        Err(Snap::Failed(e)) => crate::gate::refused(
            "SNAPSHOT_FAILED",
            format!("{agent}: wip snapshot failed ({e:#}); index restored, nothing pushed, pane kept"),
        ),
        Err(Snap::Secret(hit)) => crate::gate::refused(
            "SECRET_IN_WIP",
            format!("{agent}'s unpushed diff has a secret ({hit}); nothing pushed, pane kept"),
        ),
        Err(Snap::Push(e)) => crate::gate::refused(
            "SNAPSHOT_FAILED",
            format!("{agent}: wip push failed ({e:#}); committed in its worktree, nothing pushed, pane kept"),
        ),
        Ok(ws) => Ok(ws),
    }
}

/// #649: a superseded lane's dirty or unpushed work in `wt`, saved to `wip/` on its host first; that branch, if any.
pub fn save_lane(ctx: &Ctx, on: &On, agent: &str, wt: &str) -> Result<Option<String>> {
    if !on.exists(Path::new(wt)) || pending(on, wt)? == (0, 0) {
        return Ok(None);
    }
    let w = wip_branch(ctx, on, agent);
    save_on(on, agent, wt, &w, "wip: supersede snapshot").map(|(w, _)| Some(w))
}

pub(crate) fn wip_branch(ctx: &Ctx, on: &On, agent: &str) -> String {
    let stamp = crate::usage::date(on.r, ctx.now, "+%Y%m%d-%H%M%S");
    let ok = stamp.chars().all(|c| c.is_ascii_digit() || c == '-');
    let mut w = format!("wip/{agent}-{}", if ok { stamp } else { ctx.now.to_string() });
    if let Some(m) = on.machine {
        w += "-";
        w.extend(m.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }));
    }
    format!("{w}-{}", tag(&w))
}

fn tag(seed: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut h);
    format!("{:04x}", h.finish() & 0xffff)
}

/// #254: push `wt`'s unpushed or dirty work to `w` (`snapshot`, its secret scan first); the branch it went to.
pub fn save_wip(r: &dyn Runner, wt: &str, w: &str, msg: &str) -> Result<String> {
    snapshot(r, wt, w, msg).map(|(w, _)| w).map_err(|e| match e {
        Snap::Secret(hit) => anyhow::anyhow!("a secret in the diff ({hit})"),
        Snap::Failed(e) | Snap::Push(e) => e,
    })
}

enum Snap {
    Failed(anyhow::Error),
    Secret(String),
    Push(anyhow::Error),
}

const NO_WIP: [&str; 3] = ["target*", "node_modules", ".lane-logs"];
const CAPS: (u64, u64) = (5 << 20, 50 << 20);
fn build(p: &str) -> bool {
    (p.split('/').rev().skip(1)).any(|c| c.starts_with("target") || NO_WIP[1..].contains(&c))
}

/// #646 `wip prune`: origin's `wip/*` branches of `fleet`'s repo, each with the size it adds over the integration branch, its issue's state and whether it carries build output, after the counts per issue. Unless `dry`, deleted on origin (a branch delete: never a history rewrite or force-push): #779 those of a merged or closed issue but its newest per role, and that one too if it carries build output. An open issue's are kept.
pub fn prune(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, fleet: &str, dry: bool) -> Result<()> {
    let (f, states) = (cfg.fleet(fleet)?, fold(&working(&ctx.state)?));
    let dir = f.local_path().to_string_lossy().into_owned();
    let git = |a: &[&str]| ok(r, "git", &[&["-C", &dir][..], a].concat());
    git(&["fetch", "-q", "--prune", "origin"])?;
    let integ = format!("origin/{}", f.integration);
    let refs = git(&[
        "for-each-ref",
        "--format=%(refname:lstrip=3)",
        "refs/remotes/origin/wip/",
    ])?;
    let of = |b: &str| {
        let mut w = b.split_once(&format!("{fleet}-"))?.1.split('-');
        let role = w.next()?.to_string();
        Some((w.next()?.parse::<u64>().ok()?, role))
    };
    let stamp = |b: &str| {
        let w: Vec<&str> = b.split('-').collect();
        let digits = |s: &str, n| s.len() == n && s.bytes().all(|c| c.is_ascii_digit());
        w.windows(2)
            .find(|p| digits(p[0], 8) && digits(p[1], 6))
            .map(|p| p.join("-"))
    };
    let mut newest: BTreeMap<(u64, String), (Option<String>, &str)> = BTreeMap::new();
    for b in refs.lines() {
        if let Some(k) = of(b) {
            let e = newest.entry(k).or_insert((None, b));
            if (stamp(b), b) >= (e.0.clone(), e.1) {
                *e = (stamp(b), b);
            }
        }
    }
    let mut rows = vec![];
    let mut counts: BTreeMap<Option<u64>, (usize, usize)> = BTreeMap::new();
    for b in refs.lines() {
        let tip = format!("origin/{b}");
        let size: u64 = git(&["rev-list", "--objects", "--disk-usage", &tip, "--not", &integ])?
            .trim()
            .parse()?;
        let has = git(&["log", "--name-only", "--format=", &tip, "--not", &integ])?
            .lines()
            .any(build);
        let k = of(b);
        let n = k.as_ref().map(|k| k.0);
        let st = n.and_then(|n| states.get(&(fleet.to_string(), n)));
        let last = k.and_then(|k| newest.get(&k)).is_some_and(|e| e.1 == b);
        let go = (has || !last) && st.is_some_and(crate::events::IssueState::done);
        if go && !dry {
            git(&["push", "origin", "--delete", b])?;
        }
        let c = counts.entry(n).or_default();
        (c.0, c.1) = (c.0 + 1, c.1 + usize::from(go));
        rows.push((b, size, n, st, has, go));
    }
    let gone = ["would delete", "deleted"][usize::from(!dry)];
    for (n, (all, del)) in &counts {
        let st = n
            .and_then(|n| states.get(&(fleet.to_string(), n)))
            .map_or("?", |s| &s.state);
        let issue = n.map_or("no issue".into(), |n| format!("#{n} {st}"));
        writeln!(out, "{issue}: {all} wip, {del} {gone}, {} kept", all - del)?;
    }
    for (b, size, n, st, has, go) in rows {
        let what = if go { gone } else { "kept" };
        let issue = n.map_or("no issue".into(), |n| format!("#{n} {}", st.map_or("?", |s| &s.state)));
        let has = ["", " · build output"][usize::from(has)];
        writeln!(out, "{b} · {} MB · {issue}{has} · {what}", size >> 20)?;
    }
    Ok(())
}

fn snapshot(r: &dyn Runner, wt: &str, w: &str, msg: &str) -> std::result::Result<(String, Vec<String>), Snap> {
    let git = |a: &[&str]| ok(r, "git", &[&["-C", wt][..], a].concat());
    let undo = |e: anyhow::Error| {
        let _ = git(&["reset", "-q"]);
        Snap::Failed(e)
    };
    let mut skip = vec![];
    let mut snap = || -> Result<(String, String)> {
        let st = git(&["status", "--porcelain"])?;
        skip = (st
            .lines()
            .filter_map(|l| l.strip_prefix("?? "))
            .filter(|p| build(p))
            .map(String::from))
        .collect();
        let no = NO_WIP.map(|d| format!(":(exclude,glob)**/{d}/**"));
        git(&[&["add", "-A", "--", "."][..], &no.each_ref().map(String::as_str)].concat())?;
        git(&[&["reset", "-q", "--"][..], &own_files().collect::<Vec<_>>(), SHOT_DIRS].concat())?;
        let changed = git(&["diff", "--cached", "--name-only", "--no-renames", "-z"])?;
        let tree = git(&["ls-tree", "-r", "-l", "-z", git(&["write-tree"])?.trim()])?;
        let mut files: Vec<(u64, &str)> = (tree.split('\0').filter_map(|l| l.split_once('\t')))
            .filter(|(_, p)| changed.split('\0').any(|c| c == *p))
            .filter_map(|(m, p)| Some((m.split_whitespace().nth(3)?.parse().ok()?, p)))
            .collect();
        files.sort_unstable();
        let mut total = 0;
        files.retain(|&(n, _)| {
            total += n;
            n > CAPS.0 || total > CAPS.1
        });
        for (n, p) in files {
            git(&["reset", "-q", "--", &format!(":(literal){p}")])?;
            skip.push(format!("{p} ({} MB)", n >> 20));
        }
        Ok((
            git(&["diff", "--cached"])?,
            git(&["log", "-p", "--format=", "HEAD", "--not", "--remotes"])?,
        ))
    };
    let (staged, log) = snap().map_err(undo)?;
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
    if !staged.is_empty() {
        let unset = ["user.name", "user.email"].iter().any(|k| git(&["config", k]).is_err());
        let id: &[&str] = if unset {
            &["-c", "user.name=drover", "-c", "user.email=drover@localhost"]
        } else {
            &[]
        };
        let msg = [msg, &format!("skipped: {}", skip.join(", "))][..=usize::from(!skip.is_empty())].join("\n\n");
        git(&[id, &["commit", "-q", "--no-verify", "-m", &msg]].concat()).map_err(undo)?;
    }
    let push = |w: String| git(&["push", "origin", &format!("HEAD:refs/heads/{w}")]).map(|_| w);
    match push(w.to_string()) {
        Err(e) if format!("{e:#}").contains("[rejected]") => push(format!("{w}-{}", tag(w))),
        p => p,
    }
    .map(|w| (w, skip))
    .map_err(Snap::Push)
}
