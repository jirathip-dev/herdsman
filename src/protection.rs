//! `herdsman protection`: read-only check that GitHub branch protection is on for
//! main, staging and the integration branch of every fleet repo. It only reads
//! `gh api .../protection`; enabling protection is the owner's job (docs/dsh.md).
use crate::config::Config;
use crate::runner::{argv, Runner};
use anyhow::Result;
use serde_json::Value;
use std::io::Write;

/// One branch's verdict: `Some(true)` protected, `Some(false)` not (or unknown),
/// `None` the branch does not exist.
fn branch(r: &dyn Runner, repo: &str, b: &str) -> (Option<bool>, String) {
    let o = match r.run("gh", &argv(&["api", &format!("repos/{repo}/branches/{b}/protection")])) {
        Ok(o) => o,
        Err(e) => return (Some(false), format!("✗ unavailable: no HTTP status ({e:#})")),
    };
    let body: Value = serde_json::from_str(&o.stdout).unwrap_or(Value::Null);
    if o.code == 0 {
        let yes = |k: &str| if body[k].is_object() { "yes" } else { "no" };
        let detail = format!("required status checks: {}, required PR reviews: {}", yes("required_status_checks"), yes("required_pull_request_reviews"));
        return (Some(true), format!("✓ protected ({detail})"));
    }
    let msg = body["message"].as_str().unwrap_or("").to_string();
    // gh prints the body on stdout and `gh: <message> (HTTP <code>)` on stderr.
    let status = body["status"].as_str().map(String::from).or_else(|| {
        let s = o.stderr.rsplit_once("(HTTP ")?.1;
        Some(s.split(')').next()?.to_string())
    });
    match (status.as_deref(), msg.as_str()) {
        (Some("404"), "Branch not found") => (None, "- no such branch (HTTP 404)".into()),
        (Some("404"), "Branch not protected") => (Some(false), "✗ not protected (HTTP 404)".into()),
        (Some(s), _) => {
            let err = o.stderr.split(" (HTTP ").next().unwrap_or("").trim();
            let why = if msg.is_empty() { err.trim_start_matches("gh: ") } else { &msg };
            (Some(false), format!("✗ unavailable: {why} (HTTP {s})"))
        }
        (None, _) => (Some(false), format!("✗ unavailable: no HTTP status (gh exit {}: {})", o.code, o.stderr.trim())),
    }
}

/// Prints one line per fleet repo/branch; exit 0 only when every existing branch is protected.
pub fn protection(cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<i32> {
    let mut code = 0;
    for f in cfg.fleet.values() {
        let mut branches = vec!["main", "staging"];
        if !branches.contains(&f.integration.as_str()) {
            branches.push(&f.integration);
        }
        for b in branches {
            let (ok, line) = branch(r, &f.repo, b);
            if ok == Some(false) {
                code = 1;
            }
            writeln!(out, "{} {b}: {line}", f.repo)?;
        }
    }
    Ok(code)
}
