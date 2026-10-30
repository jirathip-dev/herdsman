//! What a lane may use: the Anthropic account a Claude lane runs on (an ordered
//! fallback list that skips capped accounts) and the MCP servers it is given.
//! Tokens stay in the env file: herdsman checks they exist, never reads them out.
use crate::config::{expand, Config, Fleet, Role};
use crate::events::{emit, fold, read, Event};
use crate::gate::refuse;
use crate::runner::{argv, Runner};
use crate::Ctx;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::Write;

/// The env var Claude Code reads its OAuth token from.
pub const TOKEN_VAR: &str = "CLAUDE_CODE_OAUTH_TOKEN";
/// `|`-separated, case-insensitive substrings of Claude Code's usage-limit banner.
pub const CAP_PATTERN: &str = "usage limit reached|hit your limit|limit reached ∙ resets|limit reached · resets";

/// When the account's cap ends, if its latest cap (since any reset) is within `cap_reset_minutes`.
pub fn capped_until(cfg: &Config, events: &[Event], name: &str, now: u64) -> Option<u64> {
    let mine = |e: &&Event| matches!(e.kind.as_str(), "account-capped" | "account-reset") && e.str("account") == Some(name);
    let last = events.iter().rev().find(mine)?;
    let until = last.ts + cfg.accounts.cap_reset_minutes * 60;
    (last.kind == "account-capped" && until > now).then_some(until)
}

/// The role's first uncapped account and the pane-shell assignment that exports its
/// token (`CLAUDE_CODE_OAUTH_TOKEN="$(. '<env file>' && printf %s "$VAR")"`), so the
/// value goes from the env file straight into the lane's env. None: the role names no account.
pub fn pick(cfg: &Config, events: &[Event], role: &Role, now: u64) -> Result<Option<(String, String)>> {
    let names = role.account_list();
    if names.is_empty() {
        return Ok(None);
    }
    let name = names.iter().find(|n| capped_until(cfg, events, n, now).is_none()).ok_or_else(|| {
        anyhow!("every account of this role is capped ({}); wait, or `herdsman accounts reset <name>`", names.join(", "))
    })?;
    let var = &cfg.accounts.names[name];
    let file = expand(cfg.accounts.accounts_env.as_deref().context("[accounts].accounts_env is not set")?);
    let text = std::fs::read_to_string(&file).with_context(|| format!("cannot read {}", file.display()))?;
    let set = text.lines().filter_map(|l| l.trim().trim_start_matches("export ").split_once('=')).any(|(k, v)| {
        k.trim() == var && !v.trim().trim_matches(['"', '\'']).is_empty()
    });
    if !set {
        bail!("account {name}: {var} is not set in {}", file.display());
    }
    let src = file.to_string_lossy().replace('\'', r"'\''");
    Ok(Some((name.clone(), format!("{TOKEN_VAR}=\"$(. '{src}' && printf %s \"${var}\")\""))))
}

/// The pane shows Claude Code's usage-limit banner.
pub fn capped_screen(cfg: &Config, screen: &str) -> bool {
    let screen = screen.to_lowercase();
    let pat = cfg.accounts.cap_pattern.as_deref().unwrap_or(CAP_PATTERN).to_lowercase();
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
    let open = |a: &str| {
        let e = events.iter().rev().find(|e| e.kind == "spawn" && e.str("agent") == Some(a));
        e.is_some_and(|e| e.issue.is_none_or(|n| states.get(&(e.fleet.clone(), n)).is_none_or(|s| s.state != "merged")))
    };
    if cfg.accounts.names.is_empty() {
        writeln!(out, "no [accounts] configured")?;
    }
    for name in cfg.accounts.names.keys() {
        let state = capped_until(cfg, &events, name, ctx.now).map_or("ok".into(), |t| format!("capped until {}", hhmm(r, t)));
        let lanes: Vec<&str> = on.iter().filter(|(a, n)| *n == name && open(a)).map(|(a, _)| *a).collect();
        writeln!(out, "{name:<8} {state:<20} {}", if lanes.is_empty() { "-".into() } else { lanes.join(" ") })?;
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
    let cap = events.iter().rev().find(|e| e.kind == "account-capped" && e.str("account") == Some(name)).unwrap();
    emit(&ctx.state, cfg, Event::new(ctx.now, &cap.fleet, None, "account-reset", json!({"account": name})))?;
    writeln!(out, "{name}: reset; spawns use it again")?;
    Ok(())
}

/// Local hh:mm of `ts` (UTC when `date` cannot tell).
pub fn hhmm(r: &dyn Runner, ts: u64) -> String {
    match r.run("date", &argv(&["-r", &ts.to_string(), "+%H:%M"])) {
        Ok(o) if o.code == 0 => o.stdout.trim().to_string(),
        _ => format!("{:02}:{:02} UTC", ts % 86400 / 3600, ts % 3600 / 60),
    }
}

/// `*`-wildcard match, case-insensitive.
fn glob(pat: &str, s: &str) -> bool {
    let (pat, s) = (pat.to_lowercase(), s.to_lowercase());
    let parts: Vec<&str> = pat.split('*').collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if parts.len() == 1 {
        return pat == s;
    }
    if !s.starts_with(first) || !s[first.len()..].ends_with(last) || s.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &s[first.len()..s.len() - last.len()];
    for p in &parts[1..parts.len() - 1] {
        match rest.find(p) {
            Some(i) => rest = &rest[i + p.len()..],
            None => return false,
        }
    }
    true
}

/// The role's MCP servers, copied by name from the repo's `.mcp.json`. Refuses
/// `MCP_FORBIDDEN` when a name matches `mcp_deny`.
pub fn mcp_servers(cfg: &Config, f: &Fleet, role: &Role) -> Result<Map<String, Value>> {
    for n in &role.mcp {
        if let Some(d) = cfg.mcp_deny.iter().find(|d| glob(d, n)) {
            return Err(refuse("MCP_FORBIDDEN", format!("MCP server `{n}` matches mcp_deny `{d}`; lanes never get it")).into());
        }
    }
    if role.mcp.is_empty() {
        return Ok(Map::new());
    }
    let src = f.local_path().join(".mcp.json");
    let text = std::fs::read_to_string(&src).with_context(|| format!("cannot read {}", src.display()))?;
    let all: Value = serde_json::from_str(&text).with_context(|| format!("{}: not JSON", src.display()))?;
    let get = |n: &String| all["mcpServers"].get(n).cloned().map(|v| (n.clone(), v));
    role.mcp.iter().map(|n| get(n).ok_or_else(|| anyhow!("MCP server `{n}` is not in {}", src.display()))).collect()
}
