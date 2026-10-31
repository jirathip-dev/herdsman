//! `herdsman check` (alerts, each once per state) and `herdsman clean` (reclaim merged or closed lanes).
use crate::access;
use crate::config::Config;
use crate::events::{age, emit, fold, last_spawn, plain, read, Event, IssueState};
use crate::gate::gate;
use crate::gh;
use crate::host;
use crate::runner::{argv, ok, Runner};
use crate::spawn::{dirty, OWN_FILES};
use crate::Ctx;
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// Head sha of an open PR that passes every merge gate, if any.
fn mergeable(ctx: &Ctx, r: &dyn Runner, cfg: &Config, s: &IssueState) -> Option<String> {
    let f = cfg.fleet(&s.fleet).ok()?;
    let branch = s.branch.clone().unwrap_or_else(|| format!("issue-{}", s.issue));
    let pr = gh::pr(ctx, r, f, (&s.fleet, s.issue), &branch, ctx.now).ok()?;
    (pr["state"] == "OPEN").then_some(())?;
    gate(f, &pr, Some(s), cfg.limits.review_rounds).ok()
}

/// Each alert's dedup key is built from the issue's last activity: once per state, again after a change.
pub fn check(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let events = read(&ctx.state)?;
    let agents = host::agents(r).map_err(|e| eprintln!("herdsman check: herdr agent list failed ({e:#}); skipping lane checks")).ok();
    let stall = cfg.limits.stall_minutes * 60;
    for s in fold(&events).values().filter(|s| s.state != "merged") {
        let idle = ctx.now.saturating_sub(s.last_ts);
        let mut hits = vec![];
        let mut capped = None;
        if let (Some(agents), Some((role, lane))) = (&agents, s.active_lane()) {
            // Agents that report their own state (dsh-TUI) can lose the name: match the lane's pane.
            let spawned = last_spawn(&events, lane);
            let pane = spawned.and_then(|e| e.str("pane"));
            let unnamed = |a: &&serde_json::Value| a["name"].as_str().is_none_or(str::is_empty) && a["pane_id"].as_str() == pane;
            let agent = agents.iter().find(|a| a["name"] == lane).or_else(|| agents.iter().find(unnamed));
            let status = agent.map(|a| a["agent_status"].as_str().unwrap_or("unknown"));
            let key = format!("{}:{lane}", s.last_ts);
            if status.is_none() {
                hits.push(("lane-dead", key.clone(), format!("{role} lane {lane} is gone ({})", s.state)));
            }
            // A Claude lane on a named account that shows the usage-limit banner caps that account.
            let account = spawned.and_then(|e| e.str("account"));
            if let (Some(name), Some(p), true) = (account, pane, status.is_some()) {
                let screen = ok(r, "herdr", &["pane", "read", p, "--source", "visible"]).unwrap_or_default();
                if access::capped_screen(cfg, &screen) {
                    let why = format!("{name}: {role} lane {lane} hit the usage limit; spawns skip {name} for now");
                    hits.push(("account-capped", format!("{lane}:{name}"), why));
                    capped = Some(name);
                }
            }
            if idle > stall && status != Some("working") {
                let st = status.unwrap_or("gone");
                hits.push(("stall", key, format!("no event for {} in {}; {role} lane {lane} is {st}", age(idle), s.state)));
            }
        }
        if s.state == "ready" && idle > stall {
            if let Some(head) = mergeable(ctx, r, cfg, s) {
                let why = format!("PASS @ {} with green CI, unmerged for {}; run `herdsman merge`", &head[..7.min(head.len())], age(idle));
                hits.push(("ready-unmerged", format!("{}:{head}", s.last_ts), why));
            }
        }
        for (kind, key, reason) in hits {
            if events.iter().any(|e| e.is(kind, &s.fleet, s.issue) && e.str("key") == Some(&key)) {
                continue;
            }
            let mut d = json!({"key": key, "reason": reason});
            if kind == "account-capped" {
                d["account"] = json!(capped);
            }
            let ev = Event::new(ctx.now, &s.fleet, Some(s.issue), kind, d);
            writeln!(out, "{}", plain(&ev))?;
            emit(&ctx.state, cfg, ev)?;
        }
    }
    Ok(())
}

/// After `herdsman merge`: close the issue's clean lane panes, then `herdsman clean --yes` them.
pub fn teardown(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, fleet: &str, issue: u64) -> Result<()> {
    let events = read(&ctx.state)?;
    let mut seen = std::collections::BTreeSet::new();
    for e in events.iter().rev().filter(|e| e.is("spawn", fleet, issue)) {
        let (Some(wt), Some(pane)) = (e.str("worktree"), e.str("pane")) else { continue };
        if e.str("role") == Some("design") || !seen.insert(wt) || !Path::new(wt).exists() {
            continue;
        }
        match dirty(r, Path::new(wt)) {
            Some(why) => writeln!(out, "keep pane {pane} ({wt}): {why}")?,
            None => drop(r.run("herdr", &argv(&["pane", "close", pane]))),
        }
    }
    if seen.is_empty() {
        return Ok(());
    }
    clean(ctx, cfg, r, out, Some(fleet), Some(issue), true)
}

/// Remove worktree + pane of lanes whose issue is merged (or closed on GitHub). Dry run unless `yes`.
pub fn clean(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, fleet: Option<&str>, issue: Option<u64>, yes: bool) -> Result<()> {
    let events = read(&ctx.state)?;
    let states = fold(&events);
    let agents = host::agents(r).context("cannot list herdr agents; refusing to clean blind")?;
    let mut seen = std::collections::BTreeSet::new();
    let mut failed = 0;
    let mut closed = BTreeMap::new();
    // Newest spawn per worktree wins (it has the current pane).
    for e in events.iter().rev().filter(|e| e.kind == "spawn" && fleet.is_none_or(|f| f == e.fleet)) {
        let (Some(n), Some(wt)) = (e.issue, e.str("worktree")) else { continue };
        if issue.is_some_and(|i| i != n) {
            continue;
        }
        if !Path::new(wt).exists() || seen.contains(wt) {
            continue;
        }
        // Merged by `herdsman merge`, or closed on GitHub by any other route.
        let why = if states.get(&(e.fleet.clone(), n)).is_some_and(|s| s.state == "merged") {
            "merged"
        } else if closed_issues(ctx, cfg, r, out, &mut closed, &e.fleet).contains(&n) {
            "issue closed"
        } else {
            continue;
        };
        seen.insert(wt);
        let pane = e.str("pane").unwrap_or_default();
        let keep = if !host::live_in(&agents, e.str("agent").unwrap_or_default(), Path::new(wt)).is_empty() {
            Some("live agent".to_string())
        } else {
            dirty(r, Path::new(wt))
        };
        if let Some(reason) = keep {
            writeln!(out, "keep {wt}: {reason}")?;
            continue;
        }
        if !yes {
            writeln!(out, "would remove {wt} (pane {pane}, {}#{n} {why})", e.fleet)?;
            continue;
        }
        if let Err(err) = remove(r, cfg, &e.fleet, wt, e.str("workspace").or_else(|| pane.split(':').next()).unwrap_or_default()) {
            writeln!(out, "failed {wt}: {err:#}")?;
            failed += 1;
            continue;
        }
        if !pane.is_empty() {
            let _ = r.run("herdr", &argv(&["pane", "close", pane]));
        }
        writeln!(out, "removed {wt} ({}#{n} {why})", e.fleet)?;
        emit(&ctx.state, cfg, Event::new(ctx.now, &e.fleet, Some(n), "clean", json!({"worktree": wt, "pane": pane})))?;
    }
    if !yes {
        writeln!(out, "dry run: nothing changed; pass --yes to remove")?;
    }
    if failed > 0 {
        anyhow::bail!("{failed} worktree(s) could not be removed; see above");
    }
    Ok(())
}

/// The fleet's closed issue numbers, read once per fleet per run; a failed read is reported and counts as none.
fn closed_issues<'a>(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, memo: &'a mut BTreeMap<String, Vec<u64>>, fleet: &str) -> &'a Vec<u64> {
    if !memo.contains_key(fleet) {
        let got = cfg.fleet(fleet).and_then(|f| gh::closed(ctx, r, f, fleet, ctx.now));
        let list = got.unwrap_or_else(|e| {
            let _ = writeln!(out, "cannot list closed issues of {fleet} ({e:#}); only merged lanes are candidates");
            vec![]
        });
        memo.insert(fleet.to_string(), list);
    }
    &memo[fleet]
}

/// herdr first; else `git worktree remove` (never `--force`) of the re-checked tree minus `OWN_FILES`.
fn remove(r: &dyn Runner, cfg: &Config, fleet: &str, wt: &str, ws: &str) -> Result<()> {
    if !ws.is_empty() && r.run("herdr", &argv(&["worktree", "remove", "--workspace", ws]))?.code == 0 {
        return Ok(());
    }
    if let Some(why) = dirty(r, Path::new(wt)) {
        anyhow::bail!("{why}");
    }
    for f in OWN_FILES {
        match std::fs::remove_file(Path::new(wt).join(f)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).context(format!("removing {f}")),
            _ => {}
        }
    }
    let local = cfg.fleet(fleet)?.local_path().to_string_lossy().into_owned();
    ok(r, "git", &["-C", &local, "worktree", "remove", wt])?;
    Ok(())
}
