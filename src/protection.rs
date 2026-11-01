//! `herdsman protection`: read-only `gh api …/protection` check; enabling it is the owner's job (docs/dsh.md).
use crate::config::Config;
use crate::runner::{argv, Runner};
use anyhow::Result;
use serde_json::Value;
use std::io::Write;

/// Some(true) fully protected, Some(false) not (or unknown), None no such branch; plus report lines.
fn branch(r: &dyn Runner, repo: &str, b: &str) -> (Option<bool>, Vec<String>) {
    let o = match r.run("gh", &argv(&["api", &format!("repos/{repo}/branches/{b}/protection")])) {
        Ok(o) => o,
        Err(e) => return (Some(false), vec![format!("✗ unavailable: no HTTP status ({e:#})")]),
    };
    let body: Value = serde_json::from_str(&o.stdout).unwrap_or(Value::Null);
    if o.code == 0 {
        let parts = [
            ("required status checks", "required_status_checks"),
            ("required PR reviews", "required_pull_request_reviews"),
        ];
        let missing: Vec<_> = parts.iter().filter(|p| !body[p.1].is_object()).map(|p| p.0).collect();
        let mut lines = vec![if missing.is_empty() {
            "✓ protected (required status checks, required PR reviews)".to_string()
        } else {
            format!("△ protected but incomplete: missing {}", missing.join(", "))
        }];
        if body["enforce_admins"]["enabled"] != true {
            lines.push("⚠ enforce_admins off: admins can bypass protection".into());
        }
        return (Some(missing.is_empty()), lines);
    }
    let msg = body["message"].as_str().unwrap_or("").to_string();
    // gh prints the body on stdout and `gh: <message> (HTTP <code>)` on stderr.
    let status = body["status"].as_str().map(String::from).or_else(|| {
        let s = o.stderr.rsplit_once("(HTTP ")?.1;
        Some(s.split(')').next()?.to_string())
    });
    match (status.as_deref(), msg.as_str()) {
        (Some("404"), "Branch not found") => (None, vec!["- no such branch (HTTP 404)".into()]),
        (Some("404"), "Branch not protected") => (Some(false), vec!["✗ not protected (HTTP 404)".into()]),
        (Some(s), _) => {
            let err = o.stderr.split(" (HTTP ").next().unwrap_or("").trim();
            let why = if msg.is_empty() {
                err.trim_start_matches("gh: ")
            } else {
                &msg
            };
            (Some(false), vec![format!("✗ unavailable: {why} (HTTP {s})")])
        }
        (None, _) => (
            Some(false),
            vec![format!(
                "✗ unavailable: no HTTP status (gh exit {}: {})",
                o.code,
                o.stderr.trim()
            )],
        ),
    }
}

/// One line per repo/branch; exit 0 only if every existing branch is protected and each repo has one.
pub fn protection(cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<i32> {
    let mut code = 0;
    for f in cfg.fleet.values() {
        let mut branches = vec!["main", "staging"];
        if !branches.contains(&f.integration.as_str()) {
            branches.push(&f.integration);
        }
        let mut seen = false;
        for b in branches {
            let (ok, lines) = branch(r, &f.repo, b);
            seen |= ok.is_some();
            if ok == Some(false) {
                code = 1;
            }
            for l in lines {
                writeln!(out, "{} {b}: {l}", f.repo)?;
            }
        }
        if !seen {
            code = 1;
            writeln!(
                out,
                "{}: ✗ no branch verified: none of the checked branches exists",
                f.repo
            )?;
        }
    }
    Ok(code)
}
