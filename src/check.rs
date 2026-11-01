//! `herdsman check` (alerts, each once per state) and `herdsman clean` (reclaim merged or closed lanes).
use crate::access;
use crate::config::Config;
use crate::events::{age, emit, fold, last_spawn, open_asks, plain, read, Event, IssueState, PASSIVE};
use crate::gate::gate;
use crate::host;
use crate::runner::{ok, Runner};
use crate::spawn::{changes, dirty, OWN_FILES};
use crate::Ctx;
use crate::{gh, herdr};
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// Head sha of an open PR that passes every merge gate, if any.
fn mergeable(ctx: &Ctx, r: &dyn Runner, cfg: &Config, s: &IssueState) -> Option<String> {
    let f = cfg.fleet(&s.fleet).ok()?;
    let branch = crate::gate::branch_of(Some(s), s.issue);
    let pr = gh::pr(ctx, r, f, (&s.fleet, s.issue), &branch, ctx.now).ok()?;
    (pr["state"] == "OPEN").then_some(())?;
    gate(f, &pr, Some(s), cfg.limits.review_rounds).ok()
}

/// Each alert's dedup key is built from the issue's last activity: once per state, again after a change.
pub fn check(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let events = read(&ctx.state)?;
    let agents = host::agents(r)
        .map_err(|e| eprintln!("herdsman check: herdr agent list failed ({e:#}); skipping lane checks"))
        .ok();
    let stall = cfg.limits.stall_minutes * 60;
    for s in fold(&events).values().filter(|s| s.state != "merged") {
        let idle = ctx.now.saturating_sub(s.last_ts);
        let mut hits = vec![];
        let mut capped = None;
        if let (Some(agents), Some((role, lane))) = (&agents, s.active_lane()) {
            // Agents that report their own state (dsh-TUI) can lose the name: match the lane's pane.
            let spawned = last_spawn(&events, lane);
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
            if status.is_none() {
                hits.push((
                    "lane-dead",
                    key.clone(),
                    format!("{role} lane {lane} is gone ({})", s.state),
                ));
            }
            // A Claude lane on a named account that shows the usage-limit banner caps that account.
            let account = spawned.and_then(|e| e.str("account"));
            if let (Some(name), Some(p), true) = (account, pane, status.is_some()) {
                let screen = herdr::read(r, p).unwrap_or_default();
                if access::capped_screen(cfg, &screen) {
                    let why = format!("{name}: {role} lane {lane} hit the usage limit; spawns skip {name} for now");
                    hits.push(("account-capped", format!("{lane}:{name}"), why));
                    capped = Some(name);
                }
            }
            if idle > stall && status != Some("working") && !parked {
                let st = status.unwrap_or("gone");
                hits.push((
                    "stall",
                    key,
                    format!("no event for {} in {}; {role} lane {lane} is {st}", age(idle), s.state),
                ));
            }
        }
        if s.state == "ready" && idle > stall {
            if let Some(head) = mergeable(ctx, r, cfg, s) {
                let why = format!(
                    "PASS @ {} with green CI, unmerged for {}; run `herdsman merge`",
                    &head[..7.min(head.len())],
                    age(idle)
                );
                hits.push(("ready-unmerged", format!("{}:{head}", s.last_ts), why));
            }
        }
        for (kind, key, reason) in hits {
            let mut d = json!({"key": key, "reason": reason});
            if kind == "account-capped" {
                d["account"] = json!(capped);
            }
            fire(ctx, cfg, out, &events, (&s.fleet, Some(s.issue)), kind, d)?;
        }
    }
    let open = open_asks(&events);
    for a in open.iter().filter(|a| ctx.now.saturating_sub(a.ts) > 30 * 60) {
        let why = format!(
            "{} unanswered for {}: {}",
            a.str("id").unwrap_or("?"),
            age(ctx.now.saturating_sub(a.ts)),
            a.str("question").unwrap_or("?")
        );
        fire(
            ctx,
            cfg,
            out,
            &events,
            (&a.fleet, a.issue),
            "ask-waiting",
            json!({"key": a.str("id"), "reason": why}),
        )?;
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
    // #46: an item waiting > 5 min on an idle orch prompts it once per (issue, head, state); 10 min on, `orch-stuck`.
    let waits = crate::waiting::scan(ctx, cfg, r, &events, agents.as_deref().unwrap_or_default(), true);
    for (s, w) in waits.iter().filter(|(s, w)| {
        w.who == "orch" && s.lane.as_ref() == Some(&w.lane) && ctx.now.saturating_sub(w.since) > 300 && agents.is_some()
    }) {
        let Some(next) = &w.next_cmd else { continue };
        let (orch, key) = (format!("{}-orch", s.fleet), format!("{}:{}", s.last_ts, w.what));
        let line = format!("#{} {} {} ago: run {next}", s.issue, w.what, age(ctx.now - w.since));
        let at = (s.fleet.as_str(), Some(s.issue));
        match events
            .iter()
            .find(|e| e.is("orch-nudged", &s.fleet, s.issue) && e.str("key") == Some(&key))
        {
            Some(e) if ctx.now.saturating_sub(e.ts) > 600 => fire(
                ctx,
                cfg,
                out,
                &events,
                at,
                "orch-stuck",
                json!({"key": key, "reason": format!("still waiting: {line}")}),
            )?,
            Some(_) => {}
            None => {
                // The pane as herdr shows it now; logged before the Enter, so a failed Enter never types the line twice.
                let a = agents.iter().flatten().find(|a| {
                    a["name"] == orch.as_str() && matches!(a["agent_status"].as_str(), Some("idle" | "done"))
                });
                let Some(pane) = a.and_then(|a| a["pane_id"].as_str()).or_else(|| {
                    last_spawn(&events, &orch)
                        .and_then(|e| e.str("pane"))
                        .filter(|_| a.is_some())
                }) else {
                    continue;
                };
                if herdr::prompt(r, &orch, &line).is_ok() {
                    fire(
                        ctx,
                        cfg,
                        out,
                        &events,
                        at,
                        "orch-nudged",
                        json!({"key": key, "reason": line}),
                    )?;
                    let _ = herdr::keys(r, pane, "Enter"); // unsent: `orch-stuck` follows
                }
            }
        }
    }
    // An idle orch with startable work and no open ask is holding silently.
    for fleet in cfg
        .fleet
        .iter()
        .filter(|(n, f)| !f.paused && !open.iter().any(|a| a.fleet == **n))
        .map(|f| f.0)
    {
        let orch = agents.iter().flatten().find(|a| a["name"] == format!("{fleet}-orch"));
        let last = events
            .iter()
            .filter(|e| e.fleet == *fleet && !PASSIVE.contains(&e.kind.as_str()))
            .map(|e| e.ts)
            .max()
            .unwrap_or(0);
        let idle = ctx.now.saturating_sub(last);
        if idle <= 15 * 60 || !orch.is_some_and(|a| matches!(a["agent_status"].as_str(), Some("idle" | "done"))) {
            continue;
        }
        let (code, n) = crate::queue::next(&events, fleet);
        let ready = || {
            fold(&events)
                .into_values()
                .find(|s| s.fleet == *fleet && s.state == "ready" && mergeable(ctx, r, cfg, s).is_some())
        };
        let Some(what) = (code == 0)
            .then(|| format!("#{n} startable"))
            .or_else(|| ready().map(|s| format!("#{} ready to merge", s.issue)))
        else {
            continue;
        };
        let why = format!("{what}, orch idle {}, no ask", age(idle));
        fire(
            ctx,
            cfg,
            out,
            &events,
            (fleet, None),
            "orch-holding",
            json!({"key": last.to_string(), "reason": why}),
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
    Ok(())
}

/// Log and print one alert unless one of its kind already carries `d`'s `key`.
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
    emit(&ctx.state, cfg, ev)
}

/// After `herdsman merge`: close the issue's clean lane panes, then `herdsman clean --yes` them.
pub fn teardown(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, fleet: &str, issue: u64) -> Result<()> {
    let events = read(&ctx.state)?;
    let mut seen = std::collections::BTreeSet::new();
    for e in events.iter().rev().filter(|e| e.is("spawn", fleet, issue)) {
        let (Some(wt), Some(pane)) = (e.str("worktree"), e.str("pane")) else {
            continue;
        };
        if e.str("role") == Some("design") || !seen.insert(wt) || !Path::new(wt).exists() {
            continue;
        }
        match dirty(r, Path::new(wt)) {
            Some(why) => writeln!(out, "keep pane {pane} ({wt}): {why}")?,
            None => drop(herdr::close(r, pane)),
        }
    }
    if seen.is_empty() {
        return Ok(());
    }
    clean(ctx, cfg, r, out, Some(fleet), (Some(issue), None), true)
}

/// Remove worktree + pane of lanes whose issue is merged (or closed on GitHub). Dry run unless `yes`.
/// `--lane` (one agent's worktree) refuses `UNSAFE_CLEAN` with the reason when it cannot go.
pub fn clean(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    fleet: Option<&str>,
    (issue, lane): (Option<u64>, Option<&str>),
    yes: bool,
) -> Result<()> {
    let events = read(&ctx.state)?;
    let states = fold(&events);
    let agents = host::agents(r).context("cannot list herdr agents; refusing to clean blind")?;
    let mut seen = std::collections::BTreeSet::new();
    let mut failed = 0;
    let mut closed = BTreeMap::new();
    // Newest spawn per worktree wins (it has the current pane).
    for e in events
        .iter()
        .rev()
        .filter(|e| e.kind == "spawn" && fleet.is_none_or(|f| f == e.fleet))
    {
        let (Some(n), Some(wt)) = (e.issue, e.str("worktree")) else {
            continue;
        };
        if issue.is_some_and(|i| i != n) || lane.is_some_and(|a| e.str("agent") != Some(a)) {
            continue;
        }
        if !Path::new(wt).exists() || seen.contains(wt) {
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
        seen.insert(wt);
        let pane = e.str("pane").unwrap_or_default();
        let keep = if host::live_in(&agents, e.str("agent").unwrap_or_default(), Path::new(wt)).is_empty() {
            dirty(r, Path::new(wt))
        } else {
            Some("live agent".to_string())
        };
        if let Some(reason) = keep {
            if lane.is_some() {
                return unsafe_lane(&reason);
            }
            writeln!(out, "keep {wt}: {reason}")?;
            continue;
        }
        if !yes {
            writeln!(out, "would remove {wt} (pane {pane}, {}#{n} {why})", e.fleet)?;
            continue;
        }
        if let Err(err) = remove(
            r,
            cfg,
            &e.fleet,
            wt,
            e.str("workspace")
                .or_else(|| pane.split(':').next())
                .unwrap_or_default(),
        ) {
            writeln!(out, "failed {wt}: {err:#}")?;
            failed += 1;
            continue;
        }
        if !pane.is_empty() {
            let _ = herdr::close(r, pane);
        }
        writeln!(out, "removed {wt} ({}#{n} {why})", e.fleet)?;
        emit(
            &ctx.state,
            cfg,
            Event::new(
                ctx.now,
                &e.fleet,
                Some(n),
                "clean",
                json!({"worktree": wt, "pane": pane}),
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
    }
    if failed > 0 {
        anyhow::bail!("{failed} worktree(s) could not be removed; see above");
    }
    Ok(())
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

/// herdr first; else `git worktree remove` (never `--force`) of the re-checked tree minus `OWN_FILES`.
fn remove(r: &dyn Runner, cfg: &Config, fleet: &str, wt: &str, ws: &str) -> Result<()> {
    if !ws.is_empty() && herdr::worktree_remove(r, ws)? {
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

/// The agent's newest spawn, unless a `lane-killed` followed it; else `NO_SUCH_AGENT`.
fn spawned<'a>(events: &'a [Event], agent: &str) -> Result<&'a Event> {
    let i = events
        .iter()
        .rposition(|e| e.kind == "spawn" && e.str("agent") == Some(agent));
    match i.filter(|i| {
        !events[*i..]
            .iter()
            .any(|e| e.kind == "lane-killed" && e.str("agent") == Some(agent))
    }) {
        Some(i) => Ok(&events[i]),
        None => crate::gate::refused("NO_SUCH_AGENT", format!("no live lane `{agent}` in the log")),
    }
}

/// `herdsman stop`: Esc to the agent's pane (it pauses); logs `stop`.
pub fn stop(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, agent: &str) -> Result<()> {
    let events = read(&ctx.state)?;
    let e = spawned(&events, agent)?;
    herdr::keys(r, e.str("pane").unwrap_or_default(), "Escape")?;
    emit(
        &ctx.state,
        cfg,
        Event::new(ctx.now, &e.fleet, e.issue, "stop", json!({"agent": agent})),
    )?;
    Ok(writeln!(out, "sent Esc to {agent}")?)
}

/// `herdsman close`: close a non-lane herdr agent's pane (by name or pane id); files stay. Logs `closed-other`.
pub fn close(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, agent: &str) -> Result<()> {
    let (events, agents) = (read(&ctx.state)?, herdr::agents(r)?);
    let a = agents.iter().find(|a| a["name"] == agent || a["pane_id"] == agent);
    let name = a.and_then(|a| a["name"].as_str()).unwrap_or(agent);
    if last_spawn(&events, agent).or(last_spawn(&events, name)).is_some() {
        return crate::gate::refused("IS_LANE", format!("`{name}` is a lane: use stop, kill or clean"));
    }
    let pane = a.and_then(|a| a["pane_id"].as_str()).context("no such herdr agent")?;
    herdr::close(r, pane)?;
    let ev = Event::new(ctx.now, "", None, "closed-other", json!({"agent": name}));
    emit(&ctx.state, cfg, ev)?;
    Ok(writeln!(out, "closed {name}'s pane; its worktree and files stay")?)
}

/// `herdsman kill`: the dry-run line (what is lost); with `yes`, unpushed or dirty work goes to
/// `wip/<agent>-<stamp>` first (refused `SECRET_IN_WIP` when the diff masks), then the pane closes.
/// The worktree is kept; an orch's checkout is the owner's and never snapshotted.
pub fn kill(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, agent: &str, yes: bool) -> Result<()> {
    let events = read(&ctx.state)?;
    let e = spawned(&events, agent)?;
    let role = e.str("role").unwrap_or_default();
    let st = e.issue.and_then(|n| fold(&events).remove(&(e.fleet.clone(), n)));
    let wt = e.str("worktree").filter(|w| role != "orch" && Path::new(w).exists());
    let git = |a: &[&str]| ok(r, "git", &[&["-C", wt.unwrap_or_default()][..], a].concat());
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
    let stamp = crate::usage::date(r, ctx.now, "+%Y%m%d-%H%M");
    let wip = (dirty + ahead > 0).then(|| {
        format!(
            "wip/{agent}-{}",
            if stamp.chars().all(|c| c.is_ascii_digit() || c == '-') {
                stamp
            } else {
                ctx.now.to_string()
            }
        )
    });
    let pr = e.issue.zip(cfg.fleet.get(&e.fleet)).and_then(|(n, f)| {
        gh::pr(
            ctx,
            r,
            f,
            (&e.fleet, n),
            &crate::gate::branch_of(st.as_ref(), n),
            ctx.now,
        )
        .ok()
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
        // Stage all, then unstage the scratch: an `:(exclude)` of an ignored file fails `git add`.
        let undo = |e: anyhow::Error| {
            let _ = git(&["reset", "-q"]);
            crate::gate::refused(
                "SNAPSHOT_FAILED",
                format!("{agent}: wip snapshot failed ({e:#}); index restored, nothing pushed, pane kept"),
            )
        };
        let snap = || -> Result<String> {
            git(&["add", "-A"])?;
            git(&[&["reset", "-q", "--"][..], &OWN_FILES].concat())?;
            Ok(git(&["diff", "--cached"])? + &git(&["log", "-p", "--format=", "HEAD", "--not", "--remotes"])?)
        };
        let diff = match snap() {
            Ok(d) => d,
            Err(e) => return undo(e),
        };
        // Added hunk lines only (a file header runs `diff --git` → `@@`), minus hash-shaped lockfile checksums.
        let hash = |l: &str| {
            let l = l.trim().trim_end_matches(',').strip_suffix('"').unwrap_or_default();
            l.strip_prefix("checksum = \"")
                .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
                || l.strip_prefix("\"integrity\": \"sha")
                    .and_then(|v| v.split_once('-'))
                    .is_some_and(|(a, b)| {
                        [("256", 44), ("384", 64), ("512", 88)].contains(&(a, b.len()))
                            && b.bytes().all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(&c))
                    })
        };
        let mut head = false;
        let diff: String = diff
            .lines()
            .filter(|l| {
                head = (head || l.starts_with("diff --git ")) && !l.starts_with("@@");
                !head
            })
            .filter_map(|l| l.strip_prefix('+'))
            .filter(|l| !hash(l))
            .collect::<Vec<_>>()
            .join("\n");
        if crate::lanes::mask(&diff) != diff {
            let _ = git(&["reset", "-q"]);
            return crate::gate::refused(
                "SECRET_IN_WIP",
                format!("{agent}'s unpushed diff has a secret-looking value; nothing pushed, pane kept"),
            );
        }
        if dirty > 0 {
            if let Err(e) = git(&["commit", "-q", "--no-verify", "-m", "wip: kill snapshot"]) {
                return undo(e);
            }
        }
        git(&["push", "origin", &format!("HEAD:refs/heads/{w}")])?;
    }
    herdr::close(r, e.str("pane").unwrap_or_default())?;
    let d = json!({"agent": agent, "role": role, "pane": e.str("pane"), "wip": wip});
    emit(
        &ctx.state,
        cfg,
        Event::new(ctx.now, &e.fleet, e.issue, "lane-killed", d),
    )?;
    Ok(writeln!(
        out,
        "killed {agent}{}",
        wip.map_or(String::new(), |w| format!(": work pushed to {w}"))
    )?)
}
