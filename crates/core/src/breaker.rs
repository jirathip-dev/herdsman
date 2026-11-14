use crate::config::Config;
use crate::events::{append, emit, last_spawn, working, Event};
use crate::hosts::On;
use crate::runner::Runner;
use crate::Ctx;
use anyhow::Result;
use serde_json::json;
use std::io::Write;

/// The line a canary types; its output, not the echoed line, holds `CANARY_OUT`.
pub const CANARY: &str = "echo drover-canary-$((6*7))";
pub const CANARY_OUT: &str = "drover-canary-42";

/// The host spawn `sp`'s lane runs on (`local` without `[hosts]`).
pub fn host(sp: &Event) -> &str {
    sp.str("host").unwrap_or("local")
}

/// Host `h`'s `runtime-down` while its breaker is open.
pub fn down<'a>(ev: &'a [Event], h: &str) -> Option<&'a Event> {
    let mine = |e: &&Event| matches!(e.kind.as_str(), "runtime-down" | "runtime-up") && e.str("host") == Some(h);
    ev.iter().rfind(mine).filter(|e| e.kind == "runtime-down")
}

/// `runtime down on <host> since <HH:MM> UTC` while host `h`'s breaker is open.
pub fn says(ev: &[Event], h: &str) -> Option<String> {
    let t = down(ev, h)?.ts;
    Some(format!(
        "runtime down on {h} since {:02}:{:02} UTC",
        t / 3600 % 24,
        t / 60 % 60
    ))
}

/// The host lane `lane`'s last spawn in `ev` ran on, stamped `failed_on` on the `respawn` or `needs-human` it ends in.
pub fn blame<'a>(ev: &'a [Event], lane: &str) -> Option<&'a str> {
    last_spawn(ev, lane).map(host)
}

fn failure_on(e: &Event, h: &str) -> bool {
    let ends =
        e.kind == "respawn" || e.kind == "needs-human" && matches!(e.str("code"), Some("LANE_ENDS" | "START_FAILED"));
    ends && e.str("failed_on") == Some(h)
}

/// Lane `lane` failed to start or ended with no push: true while its host's breaker is open (trips it at the limit).
pub fn failed(ctx: &Ctx, cfg: &Config, out: &mut dyn Write, fleet: &str, lane: &str, why: &str) -> Result<bool> {
    let ev = working(&ctx.state)?;
    let Some(sp) = last_spawn(&ev, lane) else {
        return Ok(false);
    };
    let h = host(sp);
    if down(&ev, h).is_some() {
        return Ok(true);
    }
    let l = &cfg.limits;
    let up = ev.iter().rfind(|e| e.kind == "runtime-up" && e.str("host") == Some(h));
    if up.is_some_and(|u| sp.ts < u.ts) {
        return Ok(false);
    }
    let since = up
        .map_or(0, |e| e.ts)
        .max(ctx.now.saturating_sub(l.runtime_down_minutes * 60));
    let count = 1 + ev.iter().filter(|e| e.ts >= since && failure_on(e, h)).count();
    if (count as u64) < l.runtime_down_failures {
        return Ok(false);
    }
    let reason = format!(
        "runtime down on {h}: {count} lanes failed to start or ended without a push in {} min (last: {lane}: {why}); spawns there held until a canary passes",
        l.runtime_down_minutes
    );
    writeln!(out, "{reason}")?;
    let d = json!({"host": h, "failures": count, "sample": why, "reason": reason});
    emit(&ctx.state, cfg, Event::new(ctx.now, fleet, None, "runtime-down", d))?;
    Ok(true)
}

/// Each host whose breaker is open runs one canary; a pass closes it (`runtime-up`).
pub fn probe(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let ev = working(&ctx.state)?;
    let open = |e: &&Event| {
        e.kind == "runtime-down"
            && e.str("host")
                .is_some_and(|h| down(&ev, h).is_some_and(|d| std::ptr::eq(d, *e)))
    };
    for e in ev.iter().filter(open) {
        let h = e.str("host").unwrap_or_default();
        let on = On::new(r, cfg.hosts.get(h).and_then(|c| c.machine.as_deref()));
        let ok = crate::runner::Runner::shed(&on).is_some_and(|b| crate::term::Shed(b).canary(&on));
        if !ok {
            writeln!(out, "runtime down on {h}: canary failed")?;
            continue;
        }
        writeln!(out, "runtime up on {h}: canary passed")?;
        let since = e.ts.saturating_sub(cfg.limits.runtime_down_minutes * 60);
        let mut held: Vec<(&str, u64)> = (ev.iter())
            .filter(|f| f.ts >= since && f.kind == "respawn" && f.str("failed_on") == Some(h))
            .filter_map(|f| Some((f.fleet.as_str(), f.issue?)))
            .collect();
        held.sort_unstable();
        held.dedup();
        for (fleet, n) in held {
            append(
                &ctx.state,
                &Event::new(ctx.now, fleet, Some(n), "runtime-cleared", json!({"host": h})),
            )?;
        }
        let d = json!({"host": h, "down_since": e.ts});
        append(&ctx.state, &Event::new(ctx.now, &e.fleet, None, "runtime-up", d))?;
    }
    Ok(())
}
