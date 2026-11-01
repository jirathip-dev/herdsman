//! `herdsman verdict` and `herdsman merge`: the review → merge gate.
use crate::config::{Config, Fleet};
use crate::events::{emit, fold, read, Event, IssueState};
use crate::gh;
use crate::runner::{argv, json, ok, Runner};
use crate::Ctx;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::Write;

#[derive(Debug)]
pub struct Refusal {
    pub code: &'static str,
    pub msg: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "REFUSED {}: {}", self.code, self.msg)
    }
}
impl std::error::Error for Refusal {}

pub fn refuse(code: &'static str, msg: impl Into<String>) -> Refusal {
    Refusal { code, msg: msg.into() }
}

/// `refuse` as an `anyhow` error.
pub fn refused<T>(code: &'static str, msg: impl Into<String>) -> Result<T> {
    Err(refuse(code, msg).into())
}

fn is_main(b: &str) -> bool {
    b == "main" || b == "master"
}

pub fn issue_state(ctx: &Ctx, fleet: &str, issue: u64) -> Result<Option<IssueState>> {
    Ok(fold(&read(&ctx.state)?).remove(&(fleet.to_string(), issue)))
}

pub fn branch_of(st: Option<&IssueState>, issue: u64) -> String {
    st.and_then(|s| s.branch.clone()).unwrap_or_else(|| format!("issue-{issue}"))
}

#[allow(clippy::too_many_arguments)]
pub fn verdict(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (fleet, issue): (&str, u64),
    pass: bool,
    sha: &str,
    file: &str,
) -> Result<()> {
    let f = cfg.fleet(fleet)?;
    if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return refused("BAD_SHA", format!("--sha {sha} must be a full 40-hex commit sha"));
    }
    let body = std::fs::read_to_string(file).with_context(|| format!("cannot read verdict file {file}"))?;
    anyhow::ensure!(!body.trim().is_empty(), "verdict file {file} is empty");
    let st = issue_state(ctx, fleet, issue)?;
    let branch = branch_of(st.as_ref(), issue);
    let api = format!("repos/{}/branches/{}", f.repo, branch);
    let head = ok(r, "gh", &["api", &api, "--jq", ".commit.sha"])?.trim().to_string();
    if !head.eq_ignore_ascii_case(sha) {
        return refused("STALE_SHA", format!("{branch} head is {head}, not {sha}"));
    }
    let v = if pass { "PASS" } else { "FAIL" };
    let round = st.as_ref().map_or(1, |s| s.rounds + u32::from(!s.reviewed(&head)));
    let data = json!({"verdict": v, "sha": head, "branch": branch, "file": file, "body": body, "round": round});
    emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(issue), "verdict", data))?;
    let limit = cfg.limits.review_rounds;
    writeln!(out, "recorded {v} for {fleet}#{issue} @ {head} (round {round}/{limit})")?;
    if !pass && round >= limit {
        let d = json!({"reason": format!("FAIL in round {round}/{limit}"), "findings": body});
        emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(issue), "needs-human", d))?;
        writeln!(out, "round limit reached → needs-human")?;
    }
    Ok(())
}

#[doc(hidden)]
pub fn ci_green(rollup: &Value, required: &[String]) -> Result<(), String> {
    rollup.as_array().filter(|a| !a.is_empty() || !required.is_empty()).ok_or("no checks reported")?;
    let prefix = if required.is_empty() { "" } else { "required " };
    gh::not_green(&gh::required(rollup, required)).map_or(Ok(()), |m| Err(format!("{prefix}{m}")))
}

/// Pure merge gate. Returns the head sha that may be merged.
pub fn gate(f: &Fleet, pr: &Value, st: Option<&IssueState>, limit: u32) -> Result<String, Refusal> {
    let integration = f.integration.as_str();
    let base = pr["baseRefName"].as_str().unwrap_or_default();
    if is_main(integration) || is_main(base) {
        return Err(refuse("MAIN_TARGET", format!("target `{integration}`/base `{base}`: main is promoted by a human only")));
    }
    if base != integration {
        return Err(refuse("BASE_MISMATCH", format!("PR base `{base}` ≠ integration `{integration}`")));
    }
    let head = pr["headRefOid"].as_str().unwrap_or_default().to_string();
    let verdicts = st.map(|s| s.verdicts.as_slice()).unwrap_or_default();
    if verdicts.is_empty() {
        return Err(refuse("NO_VERDICT", "no review verdict recorded; run a reviewer and `herdsman verdict`"));
    }
    let Some((_, v)) = verdicts.iter().rev().find(|(s, _)| s.eq_ignore_ascii_case(&head)) else {
        return Err(refuse("STALE_VERDICT", format!("no verdict for head {head}; re-review the current head")));
    };
    if v != "PASS" {
        return Err(refuse("VERDICT_FAIL", format!("verdict for {head} is {v}")));
    }
    let rounds = st.map_or(0, |s| s.rounds);
    if rounds > limit {
        return Err(refuse("ROUND_LIMIT", format!("{rounds} review rounds > limit {limit}; needs a human")));
    }
    ci_green(&pr["statusCheckRollup"], &f.required_checks).map_err(|m| refuse("CI_NOT_GREEN", format!("{m} at {head}")))?;
    Ok(head)
}

pub fn merge(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, fleet: &str, issue: u64) -> Result<()> {
    let f = cfg.fleet(fleet)?;
    let events = read(&ctx.state)?;
    let st = fold(&events).remove(&(fleet.to_string(), issue));
    // A retry after a merge is a no-op: no PR lookup, no event, no alert.
    if st.as_ref().is_some_and(|s| s.state == "merged") {
        let m = events.iter().find(|e| e.is("merged", fleet, issue));
        let s = |k| m.and_then(|e| e.str(k)).unwrap_or("?");
        writeln!(out, "already merged → {} (PR #{})", s("merge_sha"), s("pr"))?;
        return Ok(());
    }
    let branch = branch_of(st.as_ref(), issue);
    let pr = if is_main(&f.integration) {
        json!({"baseRefName": f.integration})
    } else {
        match gh::pr(ctx, r, f, (fleet, issue), &branch, ctx.now)? {
            v if v["state"] == "OPEN" => v,
            _ => json!({"error": "no-pr"}),
        }
    };
    let num = pr["number"].as_u64().unwrap_or_default().to_string();
    let res = if pr.get("error").is_some() {
        Err(refuse("NO_PR", format!("no open PR for branch {branch} in {}", f.repo)))
    } else {
        gate(f, &pr, st.as_ref(), cfg.limits.review_rounds)
    };
    let serial = || crate::queue::serial(&read(&ctx.state).unwrap_or_default(), fleet).contains(&issue);
    let res = match res {
        Ok(head) if !f.serial_paths.is_empty() && !serial() => {
            let files = gh::diff_names(ctx, r, f, (fleet, issue), &num, &head)?;
            match files.iter().find(|p| f.serial_path(p)) {
                Some(p) => Err(refuse("SERIAL_UNMARKED", format!("PR touches serial path {p}; queue #{issue} with `--serial` first"))),
                None => Ok(head),
            }
        }
        res => res,
    };
    let head = match res {
        Ok(h) => h,
        Err(rf) => {
            let kind = if rf.code == "ROUND_LIMIT" { "needs-human" } else { "blocked" };
            let d = json!({"code": rf.code, "reason": rf.msg});
            emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(issue), kind, d))?;
            return Err(rf.into());
        }
    };
    gh::forget_pr(ctx, fleet, issue);
    ok(r, "gh", &["pr", "merge", &num, "-R", &f.repo, "--merge", "--match-head-commit", &head])?;
    let mc = json(r, "gh", &["pr", "view", &num, "-R", &f.repo, "--json", "mergeCommit"])?;
    let merge_sha = mc["mergeCommit"]["oid"].as_str().unwrap_or("?").to_string();
    let d = json!({"pr": num, "merge_sha": merge_sha, "verdict_sha": head, "into": f.integration});
    emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(issue), "merged", d))?;
    // The one GitHub comment fleet writes: ≤ 5 lines, merge sha + verdict sha.
    let evidence = format!("Merged via `herdsman merge`: PR #{num} → `{}` @ `{merge_sha}`\nPinned PASS verdict sha: `{head}`", f.integration);
    let n = issue.to_string();
    writeln!(out, "{evidence}")?;
    let comment = ["issue", "comment", &n, "-R", &f.repo, "--body", &evidence];
    if let Err(e) = ok(r, "gh", &comment).or_else(|_| ok(r, "gh", &comment)) {
        let reason = format!("PR #{num} merged @ {merge_sha}, but the evidence comment failed twice: {e:#}");
        emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(issue), "evidence-missing", json!({"reason": reason})))?;
        bail!("{reason}; issue #{issue} left open");
    }
    // Already closed by `Closes #N` is fine.
    let _ = r.run("gh", &argv(&["issue", "close", &n, "-R", &f.repo, "--reason", "completed"]));
    // The merge stands either way; a teardown problem is reported, not fatal.
    if let Err(e) = crate::check::teardown(ctx, cfg, r, out, fleet, issue) {
        writeln!(out, "teardown: {e:#}")?;
    }
    if f.repo.ends_with("/herdsman") {
        // Dogfood fleet: say how to pick up the merged code; never run it.
        let exe = std::path::Path::new(&ctx.exe);
        let root = exe.parent().and_then(|b| b.parent()).unwrap_or(exe);
        writeln!(out, "cargo install --path {} --locked --root {} --force", f.local_path().display(), root.display())?;
    }
    Ok(())
}
