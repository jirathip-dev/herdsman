//! What a lane may use: its Claude account (first uncapped of an ordered list; tokens never read out) and MCP servers.
use crate::config::{expand, Config, Fleet, Role};
use crate::events::{emit, fold, last_spawn, read, Event};
use crate::gate::refused;
use crate::runner::Runner;
use crate::usage;
use crate::Ctx;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::Write;

/// The env var Claude Code reads its OAuth token from.
pub const TOKEN_VAR: &str = "CLAUDE_CODE_OAUTH_TOKEN";
/// `|`-separated, case-insensitive substrings of Claude Code's usage-limit banner.
pub const CAP_PATTERN: &str = "usage limit reached|hit your limit|limit reached ∙ resets|limit reached · resets";

/// When the account's latest cap (since any reset) ends: its logged `until`, else `cap_reset_minutes` after it.
pub fn capped_until(cfg: &Config, events: &[Event], name: &str, now: u64) -> Option<u64> {
    let mine =
        |e: &&Event| matches!(e.kind.as_str(), "account-capped" | "account-reset") && e.str("account") == Some(name);
    let last = events.iter().rev().find(mine)?;
    let until = last.data.get("until").and_then(Value::as_u64);
    let until = until.unwrap_or(last.ts + cfg.accounts.cap_reset_minutes * 60);
    (last.kind == "account-capped" && until > now).then_some(until)
}

/// The role's first uncapped account (the lowest weekly usage when every one was read in the last 30 min) and the
/// pane-shell assignment that exports its token straight from the env file.
pub fn pick(ctx: &Ctx, cfg: &Config, events: &[Event], role: &Role) -> Result<Option<(String, String)>> {
    let now = ctx.now;
    let mut names = role.account_list();
    if names.is_empty() {
        return Ok(None);
    }
    let week = |n: &String| {
        crate::usage::cached(ctx, n)
            .filter(|m| m.ts + 1800 > now)
            .and_then(|m| m.week)
            .map(|w| w.0)
    };
    let free: Vec<&String> = names
        .iter()
        .filter(|n| capped_until(cfg, events, n, now).is_none())
        .collect();
    if let Some(mut w) = free
        .iter()
        .map(|n| week(n).map(|w| (w, (*n).clone())))
        .collect::<Option<Vec<_>>>()
    {
        w.sort_by(|a, b| a.0.total_cmp(&b.0));
        names = w.into_iter().map(|w| w.1).collect();
    }
    let Some(name) = names.iter().find(|n| capped_until(cfg, events, n, now).is_none()) else {
        let all = role.account_list();
        let first = all.iter().filter_map(|n| capped_until(cfg, events, n, now)).min();
        let at = crate::events::age(first.unwrap_or(now).saturating_sub(now));
        let why = format!("every account {all:?} is capped; the first resets in {at}");
        return refused("ALL_CAPPED", why);
    };
    let var = &cfg.accounts.names[name];
    let file = expand(
        cfg.accounts
            .accounts_env
            .as_deref()
            .context("[accounts].accounts_env is not set")?,
    );
    let text = std::fs::read_to_string(&file).with_context(|| format!("cannot read {}", file.display()))?;
    let set = text
        .lines()
        .filter_map(|l| l.trim().trim_start_matches("export ").split_once('='))
        .any(|(k, v)| k.trim() == var && !v.trim().trim_matches(['"', '\'']).is_empty());
    if !set {
        bail!("account {name}: {var} is not set in {}", file.display());
    }
    let src = file.to_string_lossy().replace('\'', r"'\''");
    Ok(Some((
        name.clone(),
        format!("{TOKEN_VAR}=\"$(. '{src}' && printf %s \"${var}\")\""),
    )))
}

/// When the banner's `resets 9am` / `resets Nov 4, 8am` (host local time) falls; else `cap_reset_minutes` on, a
/// day for a weekly limit.
pub fn cap_until(cfg: &Config, r: &dyn Runner, screen: &str, now: u64) -> u64 {
    const MONTHS: &str = "janfebmaraprmayjunjulaugsepoctnovdec";
    let s = screen.to_lowercase().replace([',', '(', '.'], " ");
    let fallback = now + [cfg.accounts.cap_reset_minutes * 60, 86400][usize::from(s.contains("week"))];
    let z = usage::date(r, now, "+%z");
    let off = z.parse::<i64>().map_or(0, |z| z / 100 * 3600 + z % 100 * 60);
    let at = s.rfind("reset").unwrap_or(s.len());
    let w: Vec<&str> = s[at..].split_whitespace().skip(1).collect();
    let clock = w.iter().find_map(|t| {
        let (hm, pm) = (
            t.strip_suffix("am").or(t.strip_suffix("pm"))?,
            u64::from(t.ends_with("pm")),
        );
        let (h, m) = hm.split_once(':').unwrap_or((hm, "0"));
        Some((h.parse::<u64>().ok()? % 12 + pm * 12) * 3600 + m.parse::<u64>().ok()? * 60)
    });
    let m3 = w.first().and_then(|m| MONTHS.find(m.get(..3)?));
    let day = w.get(1).and_then(|d| d.parse::<u64>().ok());
    let date = m3.filter(|i| i % 3 == 0).zip(day);
    // (3 × 0-based month, day) of a day number since 1970-01-01 (Hinnant's civil_from_days).
    let md = |d: u64| {
        let doe = (d + 719_468) % 146_097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        (
            usize::try_from((mp + 2) % 12).unwrap_or(0) * 3,
            doy - (153 * mp + 2) / 5 + 1,
        )
    };
    let today = now.saturating_add_signed(off) / 86400;
    (today..today + 400)
        .filter(|&d| date.is_none_or(|x| x == md(d)))
        .filter_map(|d| (d * 86400 + clock?).checked_add_signed(-off))
        .find(|&t| t > now)
        .unwrap_or(fallback)
}

/// The pane shows Claude Code's usage-limit banner.
pub fn capped_screen(cfg: &Config, screen: &str) -> bool {
    let screen = screen.to_lowercase();
    let pat = cfg
        .accounts
        .cap_pattern
        .as_deref()
        .unwrap_or(CAP_PATTERN)
        .to_lowercase();
    pat.split('|').any(|p| !p.is_empty() && screen.contains(p))
}

/// `herdsman accounts`: each account's state and the open lanes on it. Names only, never tokens.
pub fn list(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let events = read(&ctx.state)?;
    let states = fold(&events);
    // Latest spawn per agent, kept while its issue (if any) is not merged.
    let mut on: BTreeMap<&str, &str> = BTreeMap::new();
    for e in events.iter().filter(|e| e.kind == "spawn") {
        if let Some(a) = e.str("agent") {
            on.insert(a, e.str("account").unwrap_or_default());
        }
    }
    let merged = |e: &Event| {
        e.issue
            .is_some_and(|n| states.get(&(e.fleet.clone(), n)).is_some_and(|s| s.state == "merged"))
    };
    let open = |a: &str| last_spawn(&events, a).is_some_and(|e| !merged(e));
    if cfg.accounts.names.is_empty() {
        writeln!(out, "no [accounts] configured")?;
    }
    for name in cfg.accounts.names.keys() {
        let lanes: Vec<&str> = on
            .iter()
            .filter(|(a, n)| *n == name && open(a))
            .map(|(a, _)| *a)
            .collect();
        writeln!(
            out,
            "{name:<8} {:<28} {} lanes {}",
            cap_line(cfg, r, &events, name, ctx.now),
            lanes.len(),
            lanes.join(" ")
        )?;
        if let Some(m) = usage::meter(ctx, cfg, r, name) {
            writeln!(
                out,
                "{:<8} {}",
                "",
                usage::meter_line(r, &m, &cfg.accounts.usage[name], ctx.now)
            )?;
        }
        writeln!(out, "{:<8} {}", "", usage::lane_hours(&events, name, ctx.now))?;
    }
    match usage::deepseek(ctx, cfg, r) {
        Some(Ok((b, low))) => writeln!(
            out,
            "deepseek · {} {:.2} · {}",
            b.currency,
            b.total,
            if low { "low" } else { "ok" }
        )?,
        Some(Err(hint)) => writeln!(out, "deepseek · {hint}")?,
        None => {}
    }
    Ok(())
}

/// `herdsman accounts reset <name>`: spawns may use a capped account again.
pub fn reset(ctx: &Ctx, cfg: &Config, out: &mut dyn Write, name: &str) -> Result<()> {
    anyhow::ensure!(cfg.accounts.names.contains_key(name), "unknown account '{name}'");
    let events = read(&ctx.state)?;
    if capped_until(cfg, &events, name, ctx.now).is_none() {
        writeln!(out, "{name} is not capped")?;
        return Ok(());
    }
    let cap = events
        .iter()
        .rev()
        .find(|e| e.kind == "account-capped" && e.str("account") == Some(name))
        .unwrap();
    emit(
        &ctx.state,
        cfg,
        Event::new(ctx.now, &cap.fleet, None, "account-reset", json!({"account": name})),
    )?;
    writeln!(out, "{name}: reset; spawns use it again")?;
    Ok(())
}

/// `capped until Nov 4 08:00` while the account's latest cap holds, else `ok`.
pub fn cap_line(cfg: &Config, r: &dyn Runner, events: &[Event], name: &str, now: u64) -> String {
    let until = |t| usage::date(r, t, "+%b %-d %H:%M");
    capped_until(cfg, events, name, now).map_or("ok".into(), |t| format!("capped until {}", until(t)))
}

/// `*`-wildcard match on lowercased bytes (callers lowercase both sides).
fn glob(g: &[u8], s: &[u8]) -> bool {
    match g {
        [] => s.is_empty(),
        [b'*', rest @ ..] => (0..=s.len()).any(|i| glob(rest, &s[i..])),
        [c, rest @ ..] => s.first() == Some(c) && glob(rest, &s[1..]),
    }
}

/// The role's MCP servers, copied by name from the repo's `.mcp.json` (`MCP_FORBIDDEN` if one matches `mcp_deny`).
pub fn mcp_servers(cfg: &Config, f: &Fleet, role: &Role) -> Result<Map<String, Value>> {
    for n in &role.mcp {
        if let Some(d) = cfg
            .mcp_deny
            .iter()
            .find(|d| glob(d.to_lowercase().as_bytes(), n.to_lowercase().as_bytes()))
        {
            return refused(
                "MCP_FORBIDDEN",
                format!("MCP server `{n}` matches mcp_deny `{d}`; lanes never get it"),
            );
        }
    }
    if role.mcp.is_empty() {
        return Ok(Map::new());
    }
    let src = f.local_path().join(".mcp.json");
    let text = std::fs::read_to_string(&src).with_context(|| format!("cannot read {}", src.display()))?;
    let all: Value = serde_json::from_str(&text).with_context(|| format!("{}: not JSON", src.display()))?;
    let get = |n: &String| all["mcpServers"].get(n).cloned().map(|v| (n.clone(), v));
    role.mcp
        .iter()
        .map(|n| get(n).ok_or_else(|| anyhow!("MCP server `{n}` is not in {}", src.display())))
        .collect()
}
