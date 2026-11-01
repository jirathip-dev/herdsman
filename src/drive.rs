//! #92 `herdsman drive`: one pass of the orch's loop, no AI; each action logs a passive `driven` event.
use crate::config::Config;
use crate::events::{emit, fold, read, Event};
use crate::runner::{ok, Runner};
use crate::{gh, host, queue, waiting, Ctx};
use anyhow::Result;
use serde_json::json;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// A lock older than this belongs to a pass that died; the next pass takes it over.
const STALE_LOCK_SECS: u64 = 1800;

/// `<state dir>/drive-<fleet>.last`: the time of the fleet's last pass (the board's `last pass`).
pub fn last_pass(ctx: &Ctx, fleet: &str) -> Option<u64> {
    std::fs::read_to_string(state_dir(ctx).join(format!("drive-{fleet}.last")))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn state_dir(ctx: &Ctx) -> &Path {
    ctx.state.parent().unwrap_or(&ctx.state)
}

/// #140: a pass's lines as one toast, per action (`merged #1, #2 · started #3`), then `2 skipped (<codes>)`.
pub fn summary(fleet: &str, out: &str) -> String {
    if out.starts_with("drive: ") {
        return out.trim().into(); // another pass holds the lock
    }
    let (mut by, mut skip) = (BTreeMap::<&str, Vec<&str>>::new(), vec![]);
    for (head, res) in out.lines().filter_map(|l| l.split_once(": ")) {
        let (step, mut w) = (head.rsplit(' ').next().unwrap_or(head), res.split([' ', ':']));
        let past = PAST.iter().find(|d| d.starts_with(step)).unwrap_or(&step);
        let issue = head.split(' ').find(|n| n.starts_with('#'));
        match w.next().filter(|c| ["REFUSED", "exit", "error"].contains(c)) {
            Some(c) => skip.extend(if c == "REFUSED" { w.next() } else { Some(c) }),
            None => by.entry(past).or_default().extend(issue),
        }
    }
    let n = skip.len();
    skip.sort_unstable();
    skip.dedup();
    let mut parts: Vec<String> = by.iter().map(|(v, ns)| format!("{v} {}", ns.join(", "))).collect();
    parts.extend((n > 0).then(|| format!("{n} skipped ({})", skip.join(", "))));
    parts.extend(parts.is_empty().then(|| "nothing due".into()));
    // An action without an issue (clean) leaves a double space.
    format!("{fleet}: {}", parts.join(" · ").replace("  ", " ").trim_end())
}
const PAST: [&str; 3] = ["started", "merged", "reviewed"];

/// One pass over every `driver = "herdsman"` fleet (or `only`), under `<state dir>/drive.lock`.
pub fn drive(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, only: Option<&str>) -> Result<()> {
    only.map(|f| cfg.fleet(f)).transpose()?;
    std::fs::create_dir_all(state_dir(ctx))?;
    let _ = std::fs::write(ctx.state.with_file_name("drive.at"), ctx.now.to_string());
    let lock = state_dir(ctx).join("drive.lock");
    let age = |p: &Path| {
        p.metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
    };
    if age(&lock).is_some_and(|a| a.as_secs() > STALE_LOCK_SECS) {
        let _ = std::fs::remove_file(&lock);
    }
    if std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .is_err()
    {
        return Ok(writeln!(
            out,
            "drive: another pass holds {}; nothing done",
            lock.display()
        )?);
    }
    let mine = |(n, f): &(&String, &crate::config::Fleet)| f.driven() && only.is_none_or(|o| o == n.as_str());
    // A paused fleet gets no pass (and no `last pass`); one fleet's error doesn't skip the others.
    let (mut res, mut start) = (Ok(()), true);
    for (name, _) in cfg.fleet.iter().filter(mine).filter(|(_, f)| !f.paused) {
        res = res.and(pass(ctx, cfg, r, out, name, start).map(|more| start = more));
        let _ = std::fs::write(state_dir(ctx).join(format!("drive-{name}.last")), ctx.now.to_string());
    }
    let _ = std::fs::remove_file(&lock);
    res
}

/// One fleet: each item's due step, then start every startable queued issue in order, then clean merged lanes.
/// Each spawn samples the host afresh; a refused item is skipped, `CAP` ends starts, `HOST_BUSY`/`ALL_CAPPED` all fleets'.
fn pass(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, fleet: &str, start: bool) -> Result<bool> {
    let events = read(&ctx.state)?;
    let agents = host::agents(r)?;
    let f = cfg.fleet(fleet)?;
    let waits = waiting::scan(ctx, cfg, r, &events, &agents, true);
    for (s, w) in waits
        .iter()
        .filter(|(s, w)| s.fleet == fleet && s.lane.as_ref() == Some(&w.lane))
    {
        let pr = gh::cached_pr(ctx, fleet, s.issue);
        let red = gh::ci(&gh::required(&pr["statusCheckRollup"], &f.required_checks)) == "failed";
        let n = s.issue.to_string();
        if w.who == "orch" {
            // `waiting` names the orch's exact next command; drive runs it instead.
            let Some(cmd) = &w.next_cmd else { continue };
            let args: Vec<&str> = cmd.split_whitespace().skip(1).collect();
            let step = match (args[0], args.get(3).copied()) {
                ("spawn", Some("rev")) => "review",
                ("spawn", _) if cmd.ends_with("--update") => "conflict",
                ("spawn", _) if cmd.ends_with("--supersede") => "fix",
                ("spawn", _) => "respawn",
                (v, _) => v,
            };
            act(ctx, cfg, r, out, &events, (fleet, Some(s.issue), step), &args)?;
        } else if s.state == "ready" && red {
            // Red CI after a PASS is a FAIL at the same head (no new round), the failed checks as the finding.
            let head = pr["headRefOid"].as_str().unwrap_or_default();
            let file = state_dir(ctx).join(format!("ci-{fleet}-{n}.md"));
            std::fs::write(&file, ci_findings(r, f, &pr, head))?;
            let file = file.to_string_lossy();
            let args = ["verdict", fleet, &n, "FAIL", "--sha", head, "--file", &file];
            act(ctx, cfg, r, out, &events, (fleet, Some(s.issue), "ci-red"), &args)?;
        }
    }
    let mut go = start;
    for n in queue::startable(&events, fleet).into_iter().take_while(|_| start) {
        let at = (fleet, n.parse().ok(), "start");
        let code = act(ctx, cfg, r, out, &events, at, &["spawn", fleet, &n, "impl"])?;
        go &= !matches!(code, Some("HOST_BUSY" | "DISK_LOW" | "ALL_CAPPED"));
        if matches!(code, Some("HOST_BUSY" | "DISK_LOW" | "CAP" | "ALL_CAPPED")) {
            break;
        }
    }
    let st = fold(&events);
    let merged = |e: &&Event| {
        e.issue
            .is_some_and(|n| st.get(&(fleet.into(), n)).is_some_and(|s| s.state == "merged"))
    };
    let left = |e: &&Event| e.str("worktree").is_some_and(|w| Path::new(w).exists());
    if events
        .iter()
        .filter(|e| e.kind == "spawn" && e.fleet == fleet)
        .filter(merged)
        .any(|e| left(&e))
    {
        act(
            ctx,
            cfg,
            r,
            out,
            &events,
            (fleet, None, "clean"),
            &["clean", fleet, "--yes"],
        )?;
    }
    Ok(go)
}

/// The finding for red CI: the first failing check, then the tail of its run's failed-step log.
fn ci_findings(r: &dyn Runner, f: &crate::config::Fleet, pr: &serde_json::Value, head: &str) -> String {
    let checks = gh::required(&pr["statusCheckRollup"], &f.required_checks);
    let failed = checks
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| gh::check_state(c) == "failed");
    let url = failed.and_then(|c| c["detailsUrl"].as_str()).unwrap_or_default();
    let run = url
        .split("/runs/")
        .nth(1)
        .and_then(|t| t.split('/').next())
        .unwrap_or_default();
    let log = (!run.is_empty()).then(|| ok(r, "gh", &["run", "view", run, "-R", &f.repo, "--log-failed"]).ok());
    let log = log.flatten().unwrap_or_default();
    let tail: Vec<&str> = log
        .lines()
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let what = gh::not_green(&checks).unwrap_or_else(|| "a check failed".into());
    // The tail lands in events and the fix brief: mask secret-looking runs first.
    format!(
        "FAIL\n\nCI is red at {head}: {what}.\n\n```\n{}\n```\n",
        crate::lanes::mask(&tail.join("\n"))
    )
}

/// (fleet, issue, step) of one action.
type At<'a> = (&'a str, Option<u64>, &'a str);
/// A refused or failed action's code (`REFUSED <code>`, `EXIT`, `ERROR`); `None` when it ran.
type Code = Option<&'static str>;

/// Run one CLI command in process (its own gates and refusals apply); print one line, log `driven`.
/// A refusal or error is logged once per item, step and outcome per hour, and retried next pass; returns its code.
fn act(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, ev: &[Event], at: At, c: &[&str]) -> Result<Code> {
    let (fleet, issue, step) = at;
    let mut buf = vec![];
    let res = crate::cli::run(c, ctx, r, &mut buf);
    let text = String::from_utf8_lossy(&buf);
    let (result, code) = match res {
        Ok(0) => (text.lines().next().unwrap_or("ok").to_string(), None),
        Ok(c) => (
            format!("exit {c}: {}", text.lines().next().unwrap_or_default()),
            Some("EXIT"),
        ),
        Err(e) => match e.downcast_ref::<crate::gate::Refusal>() {
            Some(rf) => (rf.to_string(), Some(rf.code)),
            None => (format!("error: {e:#}"), Some("ERROR")),
        },
    };
    // `clean` with nothing removable (a dirty merged lane) is not an action.
    if step == "clean" && code.is_none() && !text.contains("removed ") {
        return Ok(None);
    }
    let recent = |e: &Event| e.kind == "driven" && e.fleet == fleet && e.issue == issue && ctx.now < e.ts + 3600;
    let same = |e: &Event| e.str("step") == Some(step) && e.str("code") == code;
    // A repeated refusal is silent too (no cron mail every pass).
    if code.is_some() && ev.iter().any(|e| recent(e) && same(e)) {
        return Ok(code);
    }
    let who = issue.map_or(fleet.to_string(), |n| format!("{fleet} #{n}"));
    writeln!(out, "{who} {step}: {result}")?;
    let d = json!({"step": step, "cmd": format!("herdsman {}", c.join(" ")), "result": result, "code": code});
    emit(&ctx.state, cfg, Event::new(ctx.now, fleet, issue, "driven", d)).map(|()| code)
}
