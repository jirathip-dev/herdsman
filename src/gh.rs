//! Cached GitHub reads: the issue spec (re-fetched only when `updatedAt` moved) and
//! one PR/CI view per issue, fresh for 60 s, shared by `pr`, `wait`, `merge`, `check`.
use crate::config::Fleet;
use crate::runner::{argv, json, Runner};
use crate::Ctx;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const PR_FIELDS: &str = "number,state,baseRefName,headRefOid,mergeable,statusCheckRollup";
pub const PR_TTL: u64 = 60;

fn cache_path(ctx: &Ctx, kind: &str, fleet: &str, issue: u64) -> PathBuf {
    ctx.state.parent().unwrap_or(&ctx.state).join("cache").join(format!("{kind}-{fleet}-{issue}.json"))
}

fn load(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

fn store(p: &Path, v: &Value) -> Result<()> {
    std::fs::create_dir_all(p.parent().unwrap_or(p))?;
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, v.to_string())?;
    std::fs::rename(&tmp, p)?;
    Ok(())
}

/// The cached spec, if any; never calls GitHub (dry runs).
pub fn cached_spec(ctx: &Ctx, fleet: &str, issue: u64) -> Option<Value> {
    load(&cache_path(ctx, "issue", fleet, issue))
}

/// The issue's `{title, body, updatedAt}`. With `refresh`, one small `updatedAt` read
/// decides whether the body is fetched again; without it, the cache is trusted.
pub fn spec(ctx: &Ctx, r: &dyn Runner, f: &Fleet, fleet: &str, issue: u64, refresh: bool) -> Result<Value> {
    let p = cache_path(ctx, "issue", fleet, issue);
    let n = issue.to_string();
    if let Some(c) = load(&p) {
        if !refresh {
            return Ok(c);
        }
        let u = json(r, "gh", &["issue", "view", &n, "-R", &f.repo, "--json", "updatedAt"])?;
        if !u["updatedAt"].is_null() && u["updatedAt"] == c["updatedAt"] {
            return Ok(c);
        }
    }
    let v = json(r, "gh", &["issue", "view", &n, "-R", &f.repo, "--json", "title,body,updatedAt"])?;
    store(&p, &v)?;
    Ok(v)
}

/// The PR for `branch` as of `now`: one `gh pr view` per 60 s. Null when there is none.
pub fn pr(ctx: &Ctx, r: &dyn Runner, f: &Fleet, (fleet, issue): (&str, u64), branch: &str, now: u64) -> Result<Value> {
    let p = cache_path(ctx, "pr", fleet, issue);
    if let Some(c) = load(&p) {
        let ts = c["ts"].as_u64().unwrap_or(0);
        if c["branch"] == branch && now >= ts && now - ts < PR_TTL {
            return Ok(c["pr"].clone());
        }
    }
    let o = r.run("gh", &argv(&["pr", "view", branch, "-R", &f.repo, "--json", PR_FIELDS]))?;
    let pr = if o.code == 0 { serde_json::from_str(&o.stdout).unwrap_or(Value::Null) } else { Value::Null };
    store(&p, &json!({"ts": now, "branch": branch, "pr": pr}))?;
    Ok(pr)
}

/// The last cached PR read, however old; never calls `gh` (Null when none).
pub fn cached_pr(ctx: &Ctx, fleet: &str, issue: u64) -> Value {
    load(&cache_path(ctx, "pr", fleet, issue)).map_or(Value::Null, |c| c["pr"].clone())
}

/// Numbers of the fleet's closed issues: one `gh issue list` per fleet, fresh for 60 s.
pub fn closed(ctx: &Ctx, r: &dyn Runner, f: &Fleet, fleet: &str, now: u64) -> Result<Vec<u64>> {
    let p = cache_path(ctx, "closed", fleet, 0);
    if let Some(c) = load(&p) {
        let ts = c["ts"].as_u64().unwrap_or(0);
        if now >= ts && now - ts < PR_TTL {
            return Ok(c["closed"].as_array().into_iter().flatten().filter_map(Value::as_u64).collect());
        }
    }
    let v = json(r, "gh", &["issue", "list", "-R", &f.repo, "--state", "closed", "--limit", "1000", "--json", "number"])?;
    let closed: Vec<u64> = v.as_array().into_iter().flatten().filter_map(|i| i["number"].as_u64()).collect();
    store(&p, &json!({"ts": now, "closed": closed}))?;
    Ok(closed)
}

/// Files PR `num` changes at `head` (`gh pr diff --name-only`), cached per head sha.
pub fn diff_names(ctx: &Ctx, r: &dyn Runner, f: &Fleet, (fleet, issue): (&str, u64), num: &str, head: &str) -> Result<Vec<String>> {
    let p = cache_path(ctx, "diff", fleet, issue);
    let files = match load(&p).filter(|c| c["head"] == head) {
        Some(c) => c["files"].clone(),
        None => {
            let out = crate::runner::ok(r, "gh", &["pr", "diff", num, "-R", &f.repo, "--name-only"])?;
            let v = json!(out.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>());
            store(&p, &json!({"head": head, "files": v}))?;
            v
        }
    };
    Ok(files.as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect())
}

/// Drop the cached PR read (after fleet changed the PR itself).
pub fn forget_pr(ctx: &Ctx, fleet: &str, issue: u64) {
    let _ = std::fs::remove_file(cache_path(ctx, "pr", fleet, issue));
}

/// One check of a statusCheckRollup: green | pending | failed.
pub fn check_state(c: &Value) -> &'static str {
    match c["__typename"].as_str() {
        Some("StatusContext") => match c["state"].as_str() {
            Some("SUCCESS") => "green",
            Some("PENDING" | "EXPECTED") => "pending",
            _ => "failed",
        },
        _ if c["status"] != "COMPLETED" => "pending",
        _ if matches!(c["conclusion"].as_str(), Some("SUCCESS" | "NEUTRAL" | "SKIPPED")) => "green",
        _ => "failed",
    }
}

/// The rollup: failed if any check failed, pending while any runs (or none reported yet).
pub fn ci(rollup: &Value) -> &'static str {
    let st: Vec<_> = rollup.as_array().into_iter().flatten().map(check_state).collect();
    if st.contains(&"failed") {
        "failed"
    } else if st.is_empty() || st.contains(&"pending") {
        "pending"
    } else {
        "green"
    }
}

pub fn summary(pr: &Value, branch: &str) -> String {
    if pr.is_null() {
        return format!("no PR for branch {branch}");
    }
    let head: String = pr["headRefOid"].as_str().unwrap_or("?").chars().take(7).collect();
    let n = pr["statusCheckRollup"].as_array().map_or(0, Vec::len);
    format!(
        "PR #{} {} → {} head {head} mergeable {} ci {} ({n} checks)",
        pr["number"],
        pr["state"].as_str().unwrap_or("?"),
        pr["baseRefName"].as_str().unwrap_or("?"),
        pr["mergeable"].as_str().unwrap_or("?"),
        ci(&pr["statusCheckRollup"])
    )
}

#[derive(Clone, Copy)]
pub enum Until {
    CiDone,
    CiGreen,
    Merged,
}

/// Exit code once `until` is decided: 0 reached, 1 can no longer be reached.
fn decided(pr: &Value, until: Until) -> Option<i32> {
    if pr.is_null() {
        return None;
    }
    let ci = ci(&pr["statusCheckRollup"]);
    match until {
        Until::Merged => match pr["state"].as_str() {
            Some("MERGED") => Some(0),
            Some("CLOSED") => Some(1),
            _ => None,
        },
        Until::CiGreen | Until::CiDone if ci == "green" => Some(0),
        Until::CiGreen if ci == "failed" => Some(1),
        Until::CiDone if ci == "failed" => {
            let pending = pr["statusCheckRollup"].as_array().into_iter().flatten().any(|c| check_state(c) == "pending");
            (!pending).then_some(1)
        }
        _ => None,
    }
}

/// `herdsman wait`: poll the cached read with backoff (30 s doubling to 5 min).
/// Returns 0 reached, 1 failed/closed, 124 timed out.
#[allow(clippy::too_many_arguments)]
pub fn wait(
    ctx: &Ctx,
    r: &dyn Runner,
    out: &mut dyn Write,
    f: &Fleet,
    lane: (&str, u64),
    branch: &str,
    until: Until,
    timeout: u64,
) -> Result<i32> {
    let (mut t, mut delay) = (0, 30);
    loop {
        let pr = pr(ctx, r, f, lane, branch, ctx.now + t)?;
        if let Some(code) = decided(&pr, until) {
            writeln!(out, "{}", summary(&pr, branch))?;
            return Ok(code);
        }
        if t >= timeout {
            writeln!(out, "timeout after {t}s: {}", summary(&pr, branch))?;
            return Ok(124);
        }
        let s = delay.min(timeout - t);
        r.sleep(s);
        t += s;
        delay = (delay * 2).min(300);
    }
}
