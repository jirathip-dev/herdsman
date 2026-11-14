use crate::config::Config;
use crate::events::{append, emit, fold, last_spawn, Event};
use crate::hosts::On;
use crate::runner::Runner;
use crate::waiting::Waiting;
use crate::{term, Ctx};
use anyhow::Result;
use serde_json::{json, Value};
use std::io::Write;

/// A lane that shows no output this long after its spawn (or its first prompt sent again) never started.
pub const START_SECS: u64 = 600;

/// What drive does about a stalled lane: respawn it (`why`), or a needs-human card (code, reason).
pub enum Act {
    Respawn(String, &'static str),
    Escalate(&'static str, String),
}

/// #666: each of `fleet`'s active lanes the hook refused twice since its spawn: its pane closed, one needs-human card carrying the refusal (a respawn would only be refused again).
pub fn refused(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, ev: &[Event], fleet: &str) -> Result<()> {
    for s in fold(ev).values().filter(|s| s.fleet == fleet) {
        let Some((role, lane)) = s.active_lane() else { continue };
        let Some(sp) = last_spawn(ev, lane) else { continue };
        let hits = refusals(ev, sp, role);
        let Some(last) = hits.last().filter(|_| hits.len() >= 2) else {
            continue;
        };
        if let Some(p) = sp.str("pane") {
            let _ = term::close(&On::of(r, Some(sp)), p);
            let d = json!({"agent": lane, "pane": p, "reason": "hook-refused"});
            append(&ctx.state, &Event::new(ctx.now, fleet, Some(s.issue), "lane-closed", d))?;
        }
        let why = format!(
            "hook refused {lane} {} times, lane ended: REFUSED HOOK_FAILED_CLOSED: {}",
            hits.len(),
            last.str("cause").unwrap_or("?")
        );
        writeln!(out, "{fleet} #{} needs-human: {why}", s.issue)?;
        let d = json!({"code": "HOOK_REFUSED", "reason": why, "agent": lane});
        emit(
            &ctx.state,
            cfg,
            Event::new(ctx.now, fleet, Some(s.issue), "needs-human", d),
        )?;
    }
    Ok(())
}

fn refusals<'a>(ev: &'a [Event], sp: &Event, role: &str) -> Vec<&'a Event> {
    let (f, n) = (&sp.fleet, sp.issue.unwrap_or_default());
    let lane = format!("{f}:{n}:{role}");
    let mine =
        |e: &&Event| e.is("hook-failed-closed", f, n) && e.ts >= sp.ts && e.str("lane").is_none_or(|l| l == lane);
    ev.iter().filter(mine).collect()
}

/// #666: what to do about row `w`'s live lane, idle with no push or verdict (and no turn reported), if anything yet.
pub fn stalled(ctx: &Ctx, cfg: &Config, r: &dyn Runner, ev: &[Event], agents: &[Value], w: &Waiting) -> Option<Act> {
    let idle = ["idle without pushing", "rev idle without"];
    if w.who != "none" || w.next_cmd.is_some() || !idle.iter().any(|p| w.what.starts_with(p)) {
        return None;
    }
    let (lane, sp) = (&w.lane, last_spawn(ev, &w.lane)?);
    agents.iter().find(|a| a["name"] == lane.as_str())?;
    let (role, pane, on) = (sp.str("role")?, sp.str("pane")?, On::of(r, Some(sp)));
    let resent = |e: &&Event| e.kind == "start-resent" && e.str("agent") == Some(lane) && e.ts >= sp.ts;
    let resent = ev.iter().rfind(resent);
    let since = resent.map_or(sp.ts, |e| e.ts);
    let (start, stall) = (since + START_SECS, w.since + cfg.limits.stall_minutes * 60);
    if ctx.now < start.min(stall) {
        return None;
    }
    let screen = term::read(&on, pane).ok();
    let (n, _) = crate::waiting::respawns(ev, &w.fleet, w.issue);
    let ago = |t: u64| crate::events::age(ctx.now.saturating_sub(t));
    if screen.as_deref().is_some_and(|t| t.trim().is_empty()) {
        if ctx.now < start {
            return None;
        }
        let acp = sp.str("runtime") == Some("shed") && cfg.shed_for(&w.fleet).is_some_and(|b| b.acp.is_some());
        let brief = sp
            .str("worktree")
            .and_then(|wt| on.read(&std::path::Path::new(wt).join(".brief.md")));
        if let Some(text) = brief.filter(|_| acp && resent.is_none()) {
            let sent = term::prompt(&on, lane, &text).is_ok();
            let d = json!({"agent": lane, "sent": sent});
            let _ = append(
                &ctx.state,
                &Event::new(ctx.now, &w.fleet, Some(w.issue), "start-resent", d),
            );
            return None;
        }
        return Some(if n == 0 {
            Act::Respawn(role.into(), "failed start")
        } else {
            Act::Escalate(
                "START_FAILED",
                format!(
                    "lane {lane} never started: no output {} after its spawn, retried once",
                    ago(since)
                ),
            )
        });
    }
    if ctx.now < stall {
        return None;
    }
    if n == 0 {
        return Some(Act::Respawn(role.into(), "lane stalled"));
    }
    let tail = crate::lanes::tail(screen.as_deref().unwrap_or_default(), 5).join(" ⏎ ");
    let hook = refusals(ev, sp, role)
        .last()
        .and_then(|e| e.str("cause"))
        .map(|c| format!("; hook: {c}"));
    let why = format!(
        "lane {lane} stalled again after its respawn: no push, verdict or turn for {}; last screen: {tail}{}",
        ago(w.since),
        hook.unwrap_or_default()
    );
    Some(Act::Escalate("LANE_STALLED", why))
}

/// #819: a check lane with no verdict since its spawn and no new pane output this long is replaced.
pub const STALE_CHECK_SECS: u64 = 2 * 3600;

/// #819: the minutes check lane `lane` of `fleet` #`n` has been quiet, once that is [`STALE_CHECK_SECS`].
pub fn stale_check(
    ctx: &Ctx,
    r: &dyn Runner,
    ev: &[Event],
    agents: &[Value],
    (fleet, n): (&str, u64),
    lane: &str,
) -> Option<u64> {
    let i = ev
        .iter()
        .rposition(|e| e.kind == "spawn" && e.str("agent") == Some(lane))?;
    let sp = &ev[i];
    if !agents.iter().any(|a| a["name"] == lane) || ev[i + 1..].iter().any(|e| e.is("verdict", fleet, n)) {
        return None;
    }
    let screen = term::read(&On::of(r, Some(sp)), sp.str("pane")?).ok()?;
    let tools = crate::shed::session(ctx, lane)["tools"]
        .as_object()
        .is_some_and(|m| !m.is_empty());
    let quiet = ctx.now.saturating_sub(changed(ctx, lane, &screen, tools).max(sp.ts));
    (quiet >= STALE_CHECK_SECS).then_some(quiet / 60)
}

fn changed(ctx: &Ctx, lane: &str, screen: &str, busy: bool) -> u64 {
    use std::hash::{Hash, Hasher};
    let path = ctx.state.with_file_name("pane-seen.json");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut seen: serde_json::Map<String, Value> = serde_json::from_str(&text).unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    screen.hash(&mut h);
    let hash = format!("{:016x}", h.finish());
    let at = seen.get(lane).filter(|s| !busy && s["hash"] == hash.as_str());
    if let Some(t) = at.and_then(|s| s["at"].as_u64()) {
        return t;
    }
    seen.insert(lane.into(), json!({"hash": hash, "at": ctx.now}));
    let _ = std::fs::write(&path, Value::Object(seen).to_string());
    ctx.now
}

/// #792: the prompt a silent ACP lane gets once its turn is cancelled.
pub const NUDGE: &str = "continue: your last step finished or failed; carry on, or end your turn with `drover turn`";

/// #792: a lane's process tree at or over this much of one core runs its open tool call.
pub const BUSY_CPU_PCT: u64 = 10;

/// #792: what drive does about a silent ACP lane (`fleet` #`issue`'s `lane`).
pub enum Silent {
    Nudge(String),
    Respawn(String, String),
    Escalate(String),
}

pub fn silent(ctx: &Ctx, cfg: &Config, ev: &[Event], agents: &[Value], fleet: &str) -> Vec<(u64, String, Silent)> {
    let mut out = vec![];
    for s in fold(ev).values().filter(|s| s.fleet == fleet) {
        let staged = s.lanes.iter().find_map(|(r, a)| {
            let on = s.state == "design" && crate::events::STAGES.contains(&r.as_str()) && s.lane.as_ref() == Some(a);
            on.then_some((r.as_str(), a.as_str()))
        });
        let Some((role, lane)) = s.active_lane().or(staged) else {
            continue;
        };
        let (Some(sp), o) = (last_spawn(ev, lane), crate::shed::session(ctx, lane)) else {
            continue;
        };
        let open = o["tools"].as_object().is_some_and(|m| !m.is_empty()) && o["cpu"].as_u64() >= Some(BUSY_CPU_PCT);
        if o["turn"] != true || open || !agents.iter().any(|a| a["name"] == lane) {
            continue;
        }
        let mine = |k: &'static str| move |e: &&Event| e.kind == k && e.str("agent") == Some(lane);
        let nudged = ev.iter().rfind(mine("nudged")).filter(|e| e.ts >= sp.ts);
        let since = [o["at"].as_u64().unwrap_or(0), sp.ts, nudged.map_or(0, |e| e.ts)];
        let since = since.into_iter().max().unwrap_or_default();
        if ctx.now < since + cfg.limits.silent_minutes * 60 {
            continue;
        }
        let hour = |e: &&Event| ctx.now < e.ts + 3600;
        let respawned = ev.iter().rfind(mine("respawned-silent")).filter(hour);
        let recent = respawned.or(ev.iter().rfind(mine("nudged")).filter(hour));
        let quiet = crate::events::age(ctx.now - since);
        let unanswered = nudged.filter(|n| o["said"].as_u64().is_none_or(|t| t <= n.ts));
        let act = match (unanswered, respawned, recent) {
            (Some(_), None, _) => Silent::Respawn(role.into(), format!("silent {quiet} after its nudge")),
            (_, _, Some(e)) => Silent::Escalate(format!(
                "lane {lane} went silent mid-turn again ({quiet}) within the hour after its {}",
                ["nudge", "respawn"][usize::from(e.kind == "respawned-silent")]
            )),
            _ => Silent::Nudge(format!("silent {quiet} mid-turn")),
        };
        out.push((s.issue, lane.to_string(), act));
    }
    out
}

/// #792: cancel silent lane `lane`'s turn and prompt it once; logged `nudged`.
pub fn nudge(
    ctx: &Ctx,
    r: &dyn Runner,
    out: &mut dyn Write,
    ev: &[Event],
    (fleet, n): (&str, u64),
    lane: &str,
    why: &str,
) -> Result<()> {
    let on = On::of(r, last_spawn(ev, lane));
    if let Err(e) = term::awake(&on, lane) {
        return Ok(writeln!(out, "{fleet} #{n} nudge of {lane} waits: {e:#}")?);
    }
    let cancelled = crate::shed::cli(&on, &["cancel", lane]).is_ok();
    let sent = term::prompt(&on, lane, NUDGE).is_ok();
    writeln!(out, "{fleet} #{n} nudged {lane}: {why}")?;
    let d = json!({"agent": lane, "why": why, "cancelled": cancelled, "sent": sent});
    append(&ctx.state, &Event::new(ctx.now, fleet, Some(n), "nudged", d))
}
