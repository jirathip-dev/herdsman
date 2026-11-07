//! #626: GitHub webhooks, `POST /hooks/github` on serve's Funnel listener (docs/serve.md). The `X-Hub-Signature-256`
//! HMAC-SHA256 of the raw body, checked in constant time against `GITHUB_WEBHOOK_SECRET`; a PR or CI event on a fleet
//! repo re-reads that issue's PR into the cache `herdsman pr` reads and kicks drive for it. The fast step's slow
//! catch-up read (`[limits] webhook_poll_minutes`) covers what a sleeping Mac or a down Funnel missed.
use crate::config::Config;
use crate::events::{fold, read};
use crate::runner::Runner;
use crate::{gate, gh, Ctx};
use hkdf::hmac::{Hmac, KeyInit, Mac};
use serde_json::Value;
use std::path::PathBuf;

pub const SECRET: &str = "GITHUB_WEBHOOK_SECRET";
/// GitHub's payload cap: a longer body is refused before it is read.
pub const MAX_BODY: usize = 25 << 20;
const EVENTS: [&str; 5] = ["pull_request", "check_suite", "check_run", "status", "push"];

/// Each fleet's webhook secret (its own, else `all`'s); empty: webhooks are off.
pub fn secrets(cfg: &Config, r: &dyn Runner) -> Vec<(String, String)> {
    let key = |f: &String| Some((f.clone(), crate::secret::lookup(r, f, SECRET).ok()??));
    cfg.fleet.keys().filter_map(key).collect()
}

/// `sig` (`sha256=<hex>`) is `body`'s HMAC-SHA256 under `secret`, compared in constant time.
pub fn signed(secret: &str, body: &[u8], sig: &str) -> bool {
    let hex = sig
        .strip_prefix("sha256=")
        .filter(|h| h.len() == 64)
        .unwrap_or_default();
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    let want: Option<Vec<u8>> = (0..hex.len()).step_by(2).map(byte).collect();
    let Ok(mut mac) = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    want.is_some_and(|w| !w.is_empty() && mac.verify_slice(&w).is_ok())
}

/// The count of refused deliveries (a bad or missing signature): all that is kept of them.
fn counter(ctx: &Ctx) -> PathBuf {
    ctx.state.with_file_name("webhook.rejected")
}

pub fn rejected(ctx: &Ctx) -> u64 {
    let n = std::fs::read_to_string(counter(ctx)).unwrap_or_default();
    n.trim().parse().unwrap_or(0)
}

/// The branches and PR numbers an event names (a push to integration: the merge commit's trailing `(#N)`).
fn names(v: &Value) -> (Vec<&str>, Vec<u64>) {
    let s = |p: &str| v.pointer(p).and_then(Value::as_str);
    let all = |p: &str| v.pointer(p).and_then(Value::as_array).into_iter().flatten();
    let mut branches: Vec<&str> = [
        "/pull_request/head/ref",
        "/check_suite/head_branch",
        "/check_run/check_suite/head_branch",
    ]
    .into_iter()
    .filter_map(s)
    .collect();
    branches.extend(s("/ref").and_then(|r| r.strip_prefix("refs/heads/")));
    branches.extend(all("/branches").filter_map(|b| b["name"].as_str()));
    let mut nums: Vec<u64> = v
        .pointer("/pull_request/number")
        .and_then(Value::as_u64)
        .into_iter()
        .collect();
    nums.extend(
        ["/check_suite/pull_requests", "/check_run/pull_requests"]
            .iter()
            .flat_map(|p| all(p))
            .filter_map(|p| p["number"].as_u64()),
    );
    let merged =
        s("/head_commit/message").and_then(|m| m.lines().next()?.trim_end().strip_suffix(')')?.rsplit_once("(#"));
    nums.extend(merged.and_then(|(_, n)| n.parse::<u64>().ok()));
    (branches, nums)
}

/// One delivery as `(status, text)`: 503 with no secret; 401 on a bad or missing signature (counted, nothing else
/// read); 200 for `ping`; 202 for an event no open fleet issue matches; else 200 once each matched issue's PR is re-read
/// and drive kicked for it.
pub fn deliver(ctx: &Ctx, r: &dyn Runner, event: &str, sig: Option<&str>, body: &[u8]) -> (u16, String) {
    let Ok(cfg) = Config::load(&ctx.config) else {
        return (503, "no fleet.toml".into());
    };
    let keys = secrets(&cfg, r);
    if keys.is_empty() {
        return (503, format!("webhook off: no {SECRET}"));
    }
    let ok: Vec<&str> = keys
        .iter()
        .filter(|k| sig.is_some_and(|s| signed(&k.1, body, s)))
        .map(|k| k.0.as_str())
        .collect();
    if ok.is_empty() {
        let _ = std::fs::write(counter(ctx), (rejected(ctx) + 1).to_string());
        return (401, "bad signature".into());
    }
    if event == "ping" {
        return (200, "pong".into());
    }
    let v: Value = serde_json::from_slice(body).unwrap_or_default();
    let repo = v
        .pointer("/repository/full_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (branches, nums) = names(&v);
    let states = fold(&read(&ctx.state).unwrap_or_default());
    let mut hit = vec![];
    for s in states
        .values()
        .filter(|s| EVENTS.contains(&event) && !matches!(s.state.as_str(), "merged" | "closed"))
    {
        let Some(f) = cfg
            .fleet
            .get(&s.fleet)
            .filter(|f| ok.contains(&s.fleet.as_str()) && f.repo.eq_ignore_ascii_case(repo))
        else {
            continue;
        };
        let branch = gate::branch_of(Some(s), s.issue);
        let num = gh::cached_pr(ctx, &s.fleet, s.issue)["number"].as_u64();
        if branches.contains(&branch.as_str()) || num.is_some_and(|n| nums.contains(&n)) {
            hit.push((f, s, branch));
        }
    }
    if hit.is_empty() {
        return (202, "ignored: no open fleet issue".into());
    }
    for (f, s, branch) in &hit {
        gh::forget_pr(ctx, &s.fleet, s.issue);
        gh::stale(ctx, &s.fleet);
        let _ = gh::pr(ctx, r, f, (&s.fleet, s.issue), branch, ctx.now);
        crate::drive::kick(ctx, r, &s.fleet, s.issue, "webhook");
    }
    // The board's snapshot gets a new version: serve re-renders now, not at its 15 s refresh.
    if let Some((ts, snap)) = crate::db::get::<crate::snapshot::Snapshot>(&ctx.state, "board") {
        let _ = crate::db::put(&ctx.state, "board", ts, &snap);
    }
    let done: Vec<String> = hit
        .iter()
        .map(|(_, s, _)| format!("{} #{}", s.fleet, s.issue))
        .collect();
    (200, format!("refreshed {}", done.join(", ")))
}
