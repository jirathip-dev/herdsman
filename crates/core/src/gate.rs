use crate::config::{Check, Config, Fleet, Premerge};
use crate::events::{emit, fold, working, Event, IssueState};
use crate::overlap::git;
use crate::runner::{argv, json, ok, Runner};
use crate::Ctx;
use crate::{check, gh, term};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;

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
    Ok(fold(&working(&ctx.state)?).remove(&(fleet.to_string(), issue)))
}

pub fn branch_of(st: Option<&IssueState>, issue: u64) -> String {
    st.and_then(|s| s.branch.clone())
        .unwrap_or_else(|| format!("issue-{issue}"))
}

/// #418: each blocker of a FAIL carries a well-formed Proof block; whether it holds is the next round's call. The `NO_PROOF` text naming each one that doesn't.
pub fn no_proof(body: &str) -> Option<String> {
    let proofless = |(b, ls): (bool, Vec<&str>)| Some(format!("`{}` ({})", ls[0].trim(), unproven(&ls).filter(|_| b)?));
    let bad: Vec<String> = findings(body).into_iter().filter_map(proofless).collect();
    (!bad.is_empty()).then(|| bad.join("; "))
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
    let evs = working(&ctx.state)?;
    let reviewer = |e: &&Event| e.is("spawn", fleet, issue) && crate::events::checks(e);
    let at = evs
        .iter()
        .rfind(reviewer)
        .filter(|e| e.str("worktree").is_some_and(|w| file.starts_with(w)));
    let body = (crate::hosts::On::of(r, at).read(Path::new(file)))
        .with_context(|| format!("cannot read verdict file {file}"))?;
    anyhow::ensure!(!body.trim().is_empty(), "verdict file {file} is empty");
    if let Some(bad) = no_proof(&body).filter(|_| !pass) {
        return refused("NO_PROOF", bad);
    }
    let st = issue_state(ctx, fleet, issue)?;
    let branch = branch_of(st.as_ref(), issue);
    let api = format!("repos/{}/branches/{}", f.repo, branch);
    let head = ok(r, "gh", &["api", &api, "--jq", ".commit.sha"])?.trim().to_string();
    let carried = || {
        st.as_ref()
            .is_some_and(|s| crate::overlap::carries(r, f, s, sha, &head))
    };
    if !head.eq_ignore_ascii_case(sha) && !carried() {
        return refused("STALE_SHA", format!("{branch} head is {head}, not {sha}"));
    }
    let head = sha.to_ascii_lowercase();
    let p = f.flow();
    let stage = match st.as_ref() {
        Some(s) if s.state.starts_with("review") && s.check.is_some() => s.stage.as_str(),
        _ => p.gate(),
    };
    let lp = p.loop_of(stage);
    let prose = pass && stage != "verify" && crate::lesson::is_lesson(ctx, st.as_ref(), (fleet, issue)) && {
        crate::lesson::prose_only(&ok(r, "gh", &["pr", "diff", &branch, "-R", &f.repo])?)
    };
    let (pass, body) = if prose {
        writeln!(out, "prose-only lesson PR: recorded as a FAIL")?;
        (false, format!("FAIL\n\n{}\n\n{body}", crate::lesson::PROSE_ONLY))
    } else {
        (pass, body)
    };
    let v = if pass { "PASS" } else { "FAIL" };
    let repro = if pass && stage != "verify" && crate::queue::repro(&evs, fleet, issue) {
        let view = ["pr", "view", &branch, "-R", &f.repo, "--json", "body", "--jq", ".body"];
        let section = || ok(r, "gh", &view).ok().and_then(|b| repro_section(&b));
        Some(reproduced(&body, section).or_else(|why| refused("NO_REPRO", why))?)
    } else {
        None
    };
    match crate::queue::playbook(&evs, fleet, issue)
        .filter(|_| pass && stage != "verify")
        .as_deref()
    {
        Some("perf") => {
            let q = ".commits[0] | .messageHeadline + \"\\n\" + .messageBody";
            let first = ok(
                r,
                "gh",
                &["pr", "view", &branch, "-R", &f.repo, "--json", "commits", "--jq", q],
            );
            if !first.unwrap_or_default().to_ascii_lowercase().contains("baseline") {
                return refused(
                    "NO_BASELINE",
                    "the PR's first commit records no baseline (`baseline:` command and numbers)",
                );
            }
            if !body.lines().any(|l| l.trim().to_ascii_lowercase().starts_with("perf:")) {
                return refused("NO_BASELINE", "no `Perf:` block: before/after by the same command, the improvement stated, the benchmark checklist");
            }
        }
        Some("refactor") => {
            let diff = ok(r, "gh", &["pr", "diff", &branch, "-R", &f.repo])?;
            if let Some(files) = tests_changed(&diff) {
                return refused(
                    "TESTS_CHANGED",
                    format!("a `--refactor` PR edits tests beyond moves and renames: {files}"),
                );
            }
        }
        _ => {}
    }
    let cnt = p.counts(lp);
    let (used, of) = (st.as_ref().map_or(0, |s| s.form), f.form_rounds.unwrap_or_default());
    let tagged = !pass && body.lines().next().is_some_and(|l| l.trim() == "FAIL (form-only)");
    let demoted = if !tagged {
        None
    } else if !form_only(&body, f.form_categories.as_deref().unwrap_or_default()) {
        Some("form-only-rejected")
    } else {
        (used >= of).then_some("form-rounds-exhausted")
    };
    if let Some(k) = demoted {
        writeln!(out, "{k}: recorded as a plain FAIL")?;
        emit(
            &ctx.state,
            cfg,
            Event::new(ctx.now, fleet, Some(issue), k, json!({"sha": head})),
        )?;
    }
    let form = tagged && demoted.is_none();
    let round = st.as_ref().map_or(1, |s| {
        let new = !s.verdicts.iter().any(|x| x.0.eq_ignore_ascii_case(&head) && x.2 == cnt);
        s.stage_rounds.get(cnt).copied().unwrap_or_default() + u32::from(new && !form)
    });
    let lane = at.filter(|_| stage == "verify").and_then(|e| e.str("worktree"));
    let shots = crate::verify::shots(&body, Path::new(file), lane);
    let data = json!({"verdict": v, "sha": head, "branch": branch, "file": file, "body": body, "round": round,
        "stage": stage, "loop": lp, "form_only": form, "spawn_sha": at.and_then(|e| e.str("sha")), "files": shots, "repro": repro});
    emit(
        &ctx.state,
        cfg,
        Event::new(ctx.now, fleet, Some(issue), "verdict", data),
    )?;
    let (limit, of_limit) = limit(f, cnt, st.as_ref());
    let (v, note) = if form {
        ("FAIL (form-only)", format!(" · not counted · form {}/{of}", used + 1))
    } else {
        (v, String::new())
    };
    writeln!(
        out,
        "recorded {v} for {fleet}#{issue} @ {head} (round {round}{of_limit}{note})"
    )?;
    if !pass && !form && round >= limit {
        let of = match used {
            0 => String::new(),
            n => format!(", {n} form-only not counted"),
        };
        let d = json!({"reason": format!("review rounds ran out ({round}{of_limit}{of})"), "findings": body});
        emit(
            &ctx.state,
            cfg,
            Event::new(ctx.now, fleet, Some(issue), "needs-human", d),
        )?;
        writeln!(out, "round limit reached → needs-human")?;
    }
    if pass && f.merge_label.is_some() {
        gh::forget_pr(ctx, fleet, issue);
        let pr = gh::pr(ctx, r, f, (fleet, issue), &branch, ctx.now)?;
        if let Some(s) = issue_state(ctx, fleet, issue)? {
            if let Err(e) = sync_label(ctx, cfg, r, out, &s, &pr) {
                writeln!(out, "label: {e:#}")?;
            }
        }
    }
    let events = working(&ctx.state)?;
    let is_impl = |e: &&Event| pass && e.is("spawn", fleet, issue) && !crate::events::checks(e);
    if let Some(b) = events.iter().rfind(is_impl) {
        let wt = Path::new(b.str("worktree").unwrap_or_default());
        let working = |a: &serde_json::Value| a["name"].as_str() == b.str("agent") && a["agent_status"] == "working";
        if wt.is_dir() && term::agents(r).is_ok_and(|a| !a.iter().any(working)) {
            check::purge(r, cfg, fleet, out, wt, false)?;
        }
    }
    let is_rev = |e: &&Event| e.is("spawn", fleet, issue) && crate::events::checks(e);
    let rev = events.iter().rfind(is_rev);
    let on = rev.map(|e| crate::hosts::On::of(r, Some(e)));
    let exists = |w: &&str| on.as_ref().is_some_and(|on| on.exists(Path::new(w)));
    let (Some((e, wt)), Some(r)) = (rev.and_then(|e| Some((e, e.str("worktree").filter(exists)?))), &on) else {
        return Ok(());
    };
    let (agent, pane) = (e.str("agent"), e.str("pane").unwrap_or_default());
    let busy = term::agents(r).map_or(true, |a| {
        a.iter()
            .any(|a| a["name"].as_str() == agent && a["agent_status"] == "working")
    });
    if busy {
        return Ok(());
    }
    check::purge(r, cfg, fleet, out, Path::new(wt), false)?;
    let gone = match crate::spawn::dirty(r, Path::new(wt)) {
        Some(why) => Err(anyhow::anyhow!(why)),
        None => check::reap(ctx, cfg, r, (fleet, Some(issue)), wt)
            .and_then(|_| check::remove(r, cfg, fleet, wt, false))
            .and_then(|()| term::close(r, pane).map(drop)),
    };
    let kind = if gone.is_ok() { "lane-closed" } else { "lane-kept" };
    let reason = gone.map_or_else(|why| format!("{wt}: {why:#}"), |()| "verdict recorded".into());
    writeln!(out, "{kind} {}: {reason}", agent.unwrap_or_default())?;
    let d = json!({"agent": agent, "reason": reason});
    emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(issue), kind, d))
}

fn passed(events: &[Event], s: &IssueState, head: &str) -> bool {
    let mut h = head;
    loop {
        if let Some((_, v, _)) = s.verdicts.iter().rev().find(|(x, ..)| x.eq_ignore_ascii_case(h)) {
            return v == "PASS";
        }
        let up = events
            .iter()
            .find(|e| e.is("auto-updated", &s.fleet, s.issue) && e.str("to") == Some(h));
        match up.and_then(|e| e.str("from")) {
            Some(from) if from != h => h = from,
            _ => return false,
        }
    }
}

/// #150: keep the fleet's `merge_label` on the open PR exactly while a PASS stands at its head: add it once (`labelled`), take it off a head that moved past the PASS (`unlabelled`), so a stale PASS never starts the full run.
pub fn sync_label(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    s: &IssueState,
    pr: &Value,
) -> Result<()> {
    let (fleet, f) = (s.fleet.as_str(), cfg.fleet(&s.fleet)?);
    let head = pr["headRefOid"].as_str().unwrap_or_default();
    let (Some(label), Some(num)) = (&f.merge_label, pr["number"].as_u64()) else {
        return Ok(());
    };
    if s.done() || pr["state"] != "OPEN" || head.is_empty() || pr["landing"] == "fast-first" {
        return Ok(());
    }
    let carried = || s.verdicts.iter().any(|v| v.1 == "PASS") && crate::overlap::pure(r, f, s, head);
    let (flag, kind) = match (
        passed(&working(&ctx.state)?, s, head) || carried(),
        gh::has_label(pr, label),
    ) {
        (true, false) => ("--add-label", "labelled"),
        (false, true) => ("--remove-label", "unlabelled"),
        _ => return Ok(()),
    };
    let n = num.to_string();
    ok(r, "gh", &["pr", "edit", &n, "-R", &f.repo, flag, label])?;
    gh::forget_pr(ctx, fleet, s.issue);
    writeln!(out, "{fleet} #{} {kind} PR #{n} `{label}` @ {head:.7}", s.issue)?;
    let d = json!({"label": label, "pr": num, "sha": head});
    emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(s.issue), kind, d))
}

/// #839: a draft PR with a PASS at its head is marked ready for review (its full CI starts); true while it is a draft.
pub fn ready_draft(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    s: &IssueState,
    pr: &Value,
) -> Result<bool> {
    let (fleet, f) = (s.fleet.as_str(), cfg.fleet(&s.fleet)?);
    let head = pr["headRefOid"].as_str().unwrap_or_default();
    let Some(num) = pr["number"].as_u64() else {
        return Ok(false);
    };
    if pr["isDraft"] != true || pr["state"] != "OPEN" || s.done() || !passed(&working(&ctx.state)?, s, head) {
        return Ok(false);
    }
    let n = num.to_string();
    if let Err(e) = ok(r, "gh", &["pr", "ready", &n, "-R", &f.repo]) {
        writeln!(out, "{fleet} #{} PR #{n} is a draft: {e:#}", s.issue)?;
        return Ok(true);
    }
    gh::forget_pr(ctx, fleet, s.issue);
    writeln!(out, "{fleet} #{} marked-ready PR #{n} @ {head:.7}", s.issue)?;
    let d = json!({"pr": num, "sha": head});
    emit(
        &ctx.state,
        cfg,
        Event::new(ctx.now, fleet, Some(s.issue), "marked-ready", d),
    )?;
    Ok(true)
}

#[doc(hidden)]
pub fn ci_green(rollup: &Value, required: &[String]) -> Result<(), String> {
    rollup
        .as_array()
        .filter(|a| !a.is_empty() || !required.is_empty())
        .ok_or("no checks reported")?;
    let prefix = if required.is_empty() { "" } else { "required " };
    gh::not_green(&gh::required(rollup, required)).map_or(Ok(()), |m| Err(format!("{prefix}{m}")))
}

/// #228: every blocking finding (a `blocking` line, not `non-blocking`) is tagged `blocking [<category>]` with one of `cats`; at least one blocker.
#[doc(hidden)]
pub fn form_only(body: &str, cats: &[String]) -> bool {
    let blockers: Vec<String> = findings(body)
        .iter()
        .filter(|f| f.0)
        .map(|f| f.1[0].to_lowercase())
        .collect();
    let tag = |l: &str| {
        let (_, rest) = l.split_once("blocking")?;
        let rest = rest.trim_start_matches(['*', '_', ':', ' ']).strip_prefix('[')?;
        rest.split_once(']').map(|t| t.0.trim().to_string())
    };
    !blockers.is_empty() && blockers.iter().all(|l| tag(l).is_some_and(|t| cats.contains(&t)))
}

pub(crate) fn findings(body: &str) -> Vec<(bool, Vec<&str>)> {
    let (mut out, mut fence) = (Vec::<(bool, Vec<&str>)>::new(), false);
    for l in body.lines().skip(1) {
        let (t, lc) = (l.trim_start(), l.to_lowercase());
        let item = t.starts_with(['-', '*']) || t.starts_with(|c: char| c.is_ascii_digit());
        let finding = !fence && item && lc.contains("blocking");
        let prose = !l.starts_with(char::is_whitespace) && !t.is_empty() && !item && !t.starts_with(['>', '`', '~']);
        match out.last_mut() {
            _ if finding || (!fence && prose && !lc.starts_with("proof:")) => {
                out.push((finding && !lc.contains("non-blocking"), vec![l]));
            }
            Some(f) => f.1.push(l),
            None => {}
        }
        fence ^= t.starts_with("```") || t.starts_with("~~~");
    }
    out
}

fn unproven(ls: &[&str]) -> Option<&'static str> {
    let lc: Vec<String> = ls.iter().map(|l| l.to_lowercase()).collect();
    let Some(at) = lc
        .iter()
        .position(|l| l.trim_start_matches(['-', '*', '_', ' ']).starts_with("proof:"))
    else {
        return Some("no Proof block");
    };
    let kind = lc[at]
        .split_once("proof:")
        .map_or("", |k| k.1.trim_start_matches(['*', '_', '`', ' ']));
    let text = ls[at + 1..].join("\n");
    let p: Vec<&str> = text.split("```").map(str::trim).collect();
    let quote = |l: &&str| l.trim_start().starts_with('>');
    let loc = |w: &str| {
        let w = w.trim_matches(|c: char| "`*()[],;.:".contains(c));
        w.rsplit_once(':')
            .is_some_and(|(f, n)| !f.is_empty() && n.starts_with(|c: char| c.is_ascii_digit()))
    };
    match kind {
        k if k.starts_with("run") && [".png", ".jpg", ".webp"].iter().any(|x| text.contains(x)) => None,
        k if k.starts_with("run") && p.get(1).is_none_or(|c| c.is_empty()) => Some("run proof has no fenced command"),
        k if k.starts_with("run") => p[2..].iter().all(|o| o.is_empty()).then_some("run proof has no output"),
        k if k.starts_with("spec") && !text.lines().any(|l| quote(&l) && l.trim().len() > 1) => {
            Some("spec proof has no `>` quoted line")
        }
        k if k.starts_with("spec") => {
            let mut words = ls.iter().filter(|l| !quote(l)).flat_map(|l| l.split_whitespace());
            (!words.any(loc)).then_some("spec proof has no file:line")
        }
        _ => Some("Proof is neither `run` nor `spec`"),
    }
}

fn reproduced(body: &str, section: impl FnOnce() -> Option<String>) -> Result<&'static str, String> {
    let (kind, block) = repro_block(body).ok_or("no `Repro: run` or `Repro: verify` block")?;
    let flat = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    match kind.trim_start_matches(['*', '_', '`', ' ']) {
        k if k.starts_with("run") => {
            let text = block.join("\n");
            let fenced = text.split("```").skip(1).step_by(2);
            let blocks: Vec<&str> = fenced
                .map(|p| p.split_once('\n').map_or("", |x| x.1).trim())
                .filter(|b| !b.is_empty())
                .collect();
            let outs: Vec<&str> = blocks
                .iter()
                .skip(1)
                .filter(|b| Some(*b) != blocks.first())
                .copied()
                .collect();
            let failed = |o: &str| {
                let o = o.to_lowercase().replace("; 0 failed", "");
                ["fail", "panic", "error"].iter().any(|w| o.contains(w))
            };
            match outs.first() {
                Some(base) if outs.len() >= 2 && failed(base) => Ok("run"),
                Some(_) if outs.len() >= 2 => Err("`Repro: run`'s base plus commit 1 output shows no failure".into()),
                _ => Err(format!(
                    "`Repro: run` needs the test command fenced, then its failing output on the base plus commit 1 \
                     and its passing output at head, each fenced; it has {} fenced blocks",
                    blocks.len()
                )),
            }
        }
        k if k.starts_with("verify") => {
            let quote = block.iter().filter_map(|l| l.trim_start().strip_prefix('>'));
            let quote = flat(&quote.collect::<Vec<_>>().join(" "));
            if quote.is_empty() {
                return Err("`Repro: verify` has no `>` quoted reason".into());
            }
            let sec = section().ok_or("`Repro: verify` but the PR body has no `## Repro` section")?;
            (flat(&sec).contains(&quote))
                .then_some("verify")
                .ok_or("the `Repro: verify` quote is not in the PR body's `## Repro` section".into())
        }
        _ => Err("`Repro:` is neither `run` nor `verify`".into()),
    }
}

fn tests_changed(diff: &str) -> Option<String> {
    let (mut files, mut test, mut hunk, mut minus, mut plus) = (vec![], false, false, vec![], vec![]);
    for l in diff.lines() {
        if let Some(p) = l.strip_prefix("diff --git a/") {
            let p = p.rsplit(" b/").next().unwrap_or(p);
            let name = p.rsplit('/').next().unwrap_or(p);
            test = p.split('/').any(|c| matches!(c, "test" | "tests" | "__tests__"))
                || [".test.", ".spec.", "_test.", "_tests."]
                    .iter()
                    .any(|s| name.contains(s));
            hunk = false;
            if test {
                files.push(p.to_string());
            }
        } else if l.starts_with("@@") {
            hunk = true;
        } else if let Some((side, x)) = l.split_at_checked(1).filter(|_| test && hunk) {
            match side {
                "+" => plus.push(x.trim()),
                "-" => minus.push(x.trim()),
                _ => {}
            }
        }
    }
    for v in [&mut minus, &mut plus] {
        v.retain(|x| !x.is_empty());
        v.sort_unstable();
    }
    (minus != plus).then(|| files.join(", "))
}

/// #420: a verdict's `Repro:` block: the rest of its head line (lowercase, after `repro:`), then its lines to the next heading outside a fence.
pub fn repro_block(body: &str) -> Option<(String, Vec<&str>)> {
    let key = |l: &str| l.trim_start_matches(['-', '*', '_', '#', ' ']).to_lowercase();
    let ls: Vec<&str> = body.lines().collect();
    let at = ls.iter().position(|l| key(l).starts_with("repro:"))?;
    let mut fence = false;
    let block = (ls[at + 1..].iter().copied())
        .take_while(|l| {
            let t = l.trim_start();
            let end = !fence && t.starts_with('#');
            fence ^= t.starts_with("```");
            !end
        })
        .collect();
    Some((key(ls[at]).split_off(6), block))
}

fn repro_section(pr_body: &str) -> Option<String> {
    let mut ls = pr_body
        .lines()
        .skip_while(|l| !l.trim().eq_ignore_ascii_case("## Repro"));
    ls.next()?;
    Some(ls.take_while(|l| !l.starts_with('#')).collect::<Vec<_>>().join("\n"))
}

fn unverified(events: &[Event], (fleet, n): (&str, u64), head: &str) -> bool {
    let at = |e: &&Event| e.is("verdict", fleet, n) && e.str("sha").is_some_and(|s| s.eq_ignore_ascii_case(head));
    let mut here = events.iter().filter(at);
    here.clone().any(|e| e.str("repro") == Some("verify"))
        && !here.any(|e| e.str("stage") == Some("verify") && e.str("verdict") == Some("PASS"))
}

/// #309: a check stage's rounds (unset: no limit) plus the owner's extends, worded `/3` or ` of 9+1 (owner extended)`.
pub fn limit(f: &Fleet, stage: &str, st: Option<&IssueState>) -> (u32, String) {
    let base = f.flow().get(stage).and_then(|s| s.rounds).unwrap_or(u32::MAX);
    match st.map_or((0, base), |s| (s.extra, base.saturating_add(s.extra).max(s.granted))) {
        (0, l) => (l, format!("/{l}")),
        (_, l) => (l, format!(" of {base}+{} (owner extended)", l - base)),
    }
}

/// #650: the one rule for "verified enough to merge", shared by `gate`, drive and `waiting`: every check stage's last outcome at `head` is a PASS (a `verify-skipped`/`verify-no-recipe` there is verify's), within its rounds.
pub fn verified(f: &Fleet, st: Option<&IssueState>, head: &str, over: bool) -> Result<(), Refusal> {
    let verdicts = st.map(|s| s.verdicts.as_slice()).unwrap_or_default();
    let at = |x: &&(String, String, String)| x.0.eq_ignore_ascii_case(head);
    for c in f.flow().checks() {
        let last = verdicts.iter().rev().filter(at).find(|x| x.2 == c.name);
        match last.map(|x| x.1.as_str()) {
            Some("PASS") => {}
            Some(v) => return Err(refuse("VERDICT_FAIL", format!("{} verdict for {head} is {v}", c.name))),
            None => return Err(refuse("STALE_VERDICT", format!("no {} PASS for head {head}", c.name))),
        }
        let cnt = f.flow().counts(&c.name);
        let n = st.map_or(0, |s| s.stage_rounds.get(cnt).copied().unwrap_or_default());
        let (limit, of) = limit(f, cnt, st);
        if n > limit && !over {
            return Err(refuse("ROUND_LIMIT", format!("review rounds ran out ({n}{of})")));
        }
    }
    Ok(())
}

/// Pure merge gate: every check stage's PASS at the head, within its rounds (`over`: past them). Returns the head sha that may be merged.
pub fn gate(f: &Fleet, pr: &Value, st: Option<&IssueState>, over: bool) -> Result<String, Refusal> {
    let integration = f.integration.as_str();
    let base = pr["baseRefName"].as_str().unwrap_or_default();
    if is_main(integration) || is_main(base) {
        return Err(refuse(
            "MAIN_TARGET",
            format!("target `{integration}`/base `{base}`: main is promoted by a human only"),
        ));
    }
    if base != integration {
        return Err(refuse(
            "BASE_MISMATCH",
            format!("PR base `{base}` ≠ integration `{integration}`"),
        ));
    }
    let head = pr["headRefOid"].as_str().unwrap_or_default().to_string();
    let verdicts = st.map(|s| s.verdicts.as_slice()).unwrap_or_default();
    if verdicts.is_empty() {
        return Err(refuse(
            "NO_VERDICT",
            "no review verdict recorded; run a reviewer and `drover verdict`",
        ));
    }
    let at = |x: &&(String, String, String)| x.0.eq_ignore_ascii_case(&head);
    let Some((_, v, _)) = verdicts.iter().rev().find(at) else {
        return Err(refuse(
            "STALE_VERDICT",
            format!("no verdict for head {head}; re-review the current head"),
        ));
    };
    if v != "PASS" {
        return Err(refuse("VERDICT_FAIL", format!("verdict for {head} is {v}")));
    }
    verified(f, st, &head, over)?;
    ci_green(&pr["statusCheckRollup"], f.checks(pr)).map_err(|m| refuse("CI_NOT_GREEN", format!("{m} at {head}")))?;
    Ok(head)
}

/// #198: the merge commit reads like a changelog: `<title> (#PR)`, then the PR body's `## For humans` lines and a facts line. Returns (subject, body, whether the summary was there).
pub fn merge_message(pr: &Value, issue: u64, rounds: u32, sha: &str) -> (String, String, bool) {
    let title = pr["title"].as_str().unwrap_or_default().trim();
    let strip = |open: &str, close: char| {
        let t = title.strip_prefix(open)?;
        let rest = t.trim_start_matches(|c: char| c.is_ascii_digit());
        (rest.len() < t.len()).then_some(rest)?.strip_prefix(close)
    };
    let title = strip("[#", ']').or_else(|| strip("#", ':')).unwrap_or(title).trim();
    let num = pr["number"].as_u64().unwrap_or_default();
    let human: Vec<&str> = (pr["body"].as_str().unwrap_or_default().lines())
        .skip_while(|l| !l.trim().eq_ignore_ascii_case("## For humans"))
        .skip(1)
        .take_while(|l| !l.starts_with('#'))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let files = pr["files"].as_array().into_iter().flatten();
    let src: i64 = (files.filter(|x| x["path"].as_str().is_some_and(|p| p.starts_with("src/"))))
        .map(|x| x["additions"].as_i64().unwrap_or(0) - x["deletions"].as_i64().unwrap_or(0))
        .sum();
    let s = if rounds == 1 { "" } else { "s" };
    let facts = format!("Closes #{issue} · {rounds} review round{s} · src {src:+} · PASS {sha:.7}");
    let body = if human.is_empty() {
        facts
    } else {
        format!("{}\n\n{facts}", human.join("\n"))
    };
    (format!("{title} (#{num})"), body, !human.is_empty())
}

fn premerge_file(ctx: &Ctx, fleet: &str, issue: u64) -> std::path::PathBuf {
    ctx.state.with_file_name(format!("premerge-{fleet}-{issue}.md"))
}

fn premerge(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (fleet, issue): (&str, u64),
    head: &str,
) -> Result<Result<Option<u64>, Refusal>> {
    let f = cfg.fleet(fleet)?;
    let checks = premerge_checks(ctx, f);
    let rules = std::fs::read_to_string(crate::repo_path(&f.local_path().join(".drover/rules.md"))).unwrap_or_default();
    if checks.is_empty() && crate::lesson::checks(&rules).iter().all(|c| c.active) {
        return Ok(Ok(None));
    }
    let (wt, into) = (
        ctx.state.with_file_name(format!("premerge-{fleet}")),
        format!("origin/{}", f.integration),
    );
    let (dir, wts) = (f.local_path(), wt.to_string_lossy());
    let tip = || git(r, &dir, &["fetch", "-q", "origin"]).and_then(|_| git(r, &dir, &["rev-parse", &into]));
    let tree = |a: &[&str]| git(r, &dir, &[&["worktree"][..], a].concat());
    for _ in 0..2 {
        let base = tip()?;
        let _ = tree(&["remove", "--force", &wts]);
        tree(&["add", "-f", "--detach", &wts, &base])?;
        let clean = git(r, &wt, &["merge", "--no-ff", "--no-edit", head]).is_ok();
        let t = std::time::Instant::now();
        let fail = clean.then(|| run_checks(r, &wt, &base, &checks)).flatten();
        if clean && fail.is_none() {
            crate::lesson::record(ctx, r, &wt, (fleet, issue))?;
        }
        let secs = t.elapsed().as_secs();
        let _ = tree(&["remove", "--force", &wts]);
        if !clean {
            return Ok(Err(refuse("NOT_MERGEABLE", format!("{head:.7} conflicts with {into}"))));
        }
        if let Some((c, o)) = fail {
            let at = format!("{head:.7} merged with {into} @ {base:.7}, {secs}s");
            return Err(premerge_failed(ctx, cfg, r, out, (fleet, issue), (head, &at), (c, &o))?.into());
        }
        if tip()? == base {
            return Ok(Ok(Some(secs)));
        }
    }
    Ok(Err(refuse(
        "STAGING_MOVED",
        format!("{into} moved during the premerge checks twice"),
    )))
}

pub(crate) fn premerge_checks(ctx: &Ctx, f: &Fleet) -> Vec<Check> {
    let has = f.local_path().join("ci/cap.toml").exists();
    let cap = has.then(|| Premerge::Cmd(format!("{} cap-check --config ci/cap.toml", ctx.exe)));
    let check = |p| match p {
        Premerge::Cmd(run) => Check {
            name: run.clone(),
            run,
            ..Check::default()
        },
        Premerge::Named(c) => c,
    };
    let all = f.premerge.clone().unwrap_or(cap.into_iter().collect());
    all.into_iter().map(check).collect()
}

pub(crate) fn run_checks<'a>(r: &dyn Runner, wt: &Path, base: &str, cs: &'a [Check]) -> Option<(&'a Check, String)> {
    let diff = || git(r, wt, &["diff", "--name-only", base, "HEAD"]).unwrap_or_default();
    let files = cs.iter().any(|c| !c.when.is_empty()).then(diff).unwrap_or_default();
    let glob = |g: &String| files.lines().any(|p| crate::config::glob(g.as_bytes(), p.as_bytes()));
    let hit = |c: &&Check| c.when.is_empty() || c.when.iter().any(&glob);
    cs.iter().filter(hit).find_map(|c| {
        let sh = argv(&["-c", &format!("cd \"$0\" && {}", c.run), &wt.to_string_lossy()]);
        let limit = std::time::Duration::from_secs(crate::config::limits().gate_check_minutes * 60);
        let o = r.run_for("sh", &sh, "", limit).unwrap_or_default();
        (o.code != 0).then(|| (c, format!("{}{}", o.stdout, o.stderr)))
    })
}

pub(crate) fn premerge_failed(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (fleet, issue): (&str, u64),
    (head, at): (&str, &str),
    (c, o): (&Check, &str),
) -> Result<Refusal> {
    let o = crate::lanes::mask(o);
    let lines: Vec<&str> = o.lines().filter(|l| !l.trim().is_empty()).collect();
    let err = |l: &&&str| c.code.is_none() && l.contains("error");
    let first = lines.iter().find(err).or(lines.first()).map_or("failed", |l| l.trim());
    let msg = c.code.as_ref().map_or(format!("{}: {first}", c.name), |_| first.into());
    let block = format!("\n```\n{}\n```\n", lines[lines.len().saturating_sub(20)..].join("\n"));
    let min = o.lines().find_map(|l| l.split_once("min_version="));
    let min = min.map_or("", |m| m.1.trim());
    let tpl = |t: &String| t.replace("{output}", &block).replace("{min}", min);
    let file = premerge_file(ctx, fleet, issue);
    let fix = c.fix.as_ref().map_or(format!("```\n{o}\n```"), tpl);
    std::fs::write(&file, format!("FAIL\n\nPremerge on {at}: {msg}\n\n{fix}\n"))?;
    let what = c.fix.as_deref().and_then(|t| t.split_whitespace().next());
    let what = what.map(|w| format!(": {} needed", w.to_lowercase()));
    let code: &'static str = c.code.clone().map_or("STAGING_WOULD_BREAK", |k| &*k.leak());
    let note = crate::lanes::mask(&format!("{} failed{}", c.name, what.unwrap_or_default()));
    let d = json!({"name": crate::lanes::mask(&c.name), "code": code, "note": note});
    let ev = Event::new(ctx.now, fleet, Some(issue), "premerge-failed", d);
    emit(&ctx.state, cfg, ev)?;
    verdict(ctx, cfg, r, out, (fleet, issue), false, head, &file.to_string_lossy())?;
    Ok(refuse(code, msg))
}

pub(crate) fn unjoined(
    ctx: &Ctx,
    r: &dyn Runner,
    f: &Fleet,
    (fleet, n): (&str, u64),
    (num, head): (&str, &str),
) -> Result<Vec<(crate::config::Serial, String)>> {
    let ev = working(&ctx.state)?;
    let mine = crate::queue::groups(&crate::queue::replay(&ev, fleet), &ev, f, fleet, n);
    let mut new: Vec<_> = crate::queue::serials(&ev, f, fleet)
        .into_iter()
        .filter(|g| !mine.iter().any(|m| m.name == g.name))
        .collect();
    new.sort_by(|a, b| a.name.cmp(&b.name));
    let files = if new.is_empty() {
        vec![]
    } else {
        gh::diff_names(ctx, r, f, (fleet, n), num, head)?
    };
    Ok(new
        .into_iter()
        .filter_map(|g| g.hit(&files).map(|p| (g, p.clone())))
        .collect())
}

/// `over` (`--owner-override`, #130) lifts only `ROUND_LIMIT`.
pub fn merge(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    fleet: &str,
    issue: u64,
    over: bool,
) -> Result<()> {
    let f = cfg.fleet(fleet)?;
    let events = working(&ctx.state)?;
    let st = fold(&events).remove(&(fleet.to_string(), issue));
    if st.as_ref().is_some_and(|s| s.state == "merged") {
        let m = events.iter().find(|e| e.is("merged", fleet, issue));
        let s = |k| m.and_then(|e| e.str(k)).unwrap_or("?");
        writeln!(out, "already merged → {} (PR #{})", s("merge_sha"), s("pr"))?;
        return Ok(());
    }
    let landed = |num: &str| -> Result<bool> {
        Ok(json(r, "gh", &["pr", "view", num, "-R", &f.repo, "--json", "state"])?["state"] == "MERGED")
    };
    if let Some(a) = unlogged_merge(&events, fleet, issue) {
        let (num, head) = (a.str("pr").unwrap_or_default(), a.str("head").unwrap_or_default());
        if a.ts + MERGE_SECS > ctx.now && !crate::action::dead(r, a) {
            return refused("IN_FLIGHT", format!("skipped: merge of PR #{num} in flight"));
        }
        let d = json!({"pr": num, "head": head, "recovered": true});
        let done = a.kind == "done" || landed(num)?;
        if a.kind == "intent" {
            crate::action::finish(&ctx.state, ctx.now, a, ["failed", "done"][usize::from(done)], d)?;
        }
        if done {
            let over = a.data.get("over") == Some(&json!(true));
            let fast = a.data.get("fast") == Some(&json!(true));
            return merged(
                ctx,
                cfg,
                r,
                out,
                (fleet, issue),
                &json!({}),
                (num, head, over, None, fast),
            );
        }
    }
    let branch = branch_of(st.as_ref(), issue);
    let mut pr = if is_main(&f.integration) {
        json!({"baseRefName": f.integration})
    } else {
        match gh::pr(ctx, r, f, (fleet, issue), &branch, ctx.now)? {
            v if v["state"] == "OPEN" => v,
            _ => json!({"error": "no-pr"}),
        }
    };
    crate::land::mark(ctx, Some(r), f, (fleet, issue), &events, &mut pr);
    let fast = pr["landing"] == "fast-first";
    let h = pr["headRefOid"].as_str().unwrap_or_default();
    let st = st.map(|mut s| {
        if !h.is_empty() && !s.reviewed(h) && crate::overlap::pure(r, f, &s, h) {
            s.verdicts
                .extend(f.flow().checks().map(|c| (h.into(), "PASS".into(), c.name.clone())));
        }
        s
    });
    let num = pr["number"].as_u64().unwrap_or_default().to_string();
    let res = if pr.get("error").is_some() {
        Err(refuse("NO_PR", format!("no open PR for branch {branch} in {}", f.repo)))
    } else {
        gate(f, &pr, st.as_ref(), over)
    };
    let res = match res {
        Ok(head) => match unjoined(ctx, r, f, (fleet, issue), (&num, &head))?
            .iter()
            .find_map(|(g, p)| {
                if f.serial.iter().any(|c| c.name == g.name) {
                    return Some((
                        "SERIAL_UNMARKED",
                        format!("{}: PR touches {p}; queue #{issue} with `--serial {0}` first", g.name),
                    ));
                }
                let st = fold(&events);
                let h = crate::queue::holders(&events, &st, f, fleet, &g.name);
                (h.len() >= g.max).then(|| {
                    (
                        "SERIAL_BUSY",
                        format!("{}; retry after they merge", crate::queue::holds(g, &h)),
                    )
                })
            }) {
            Some((code, why)) => Err(refuse(code, why)),
            None if unverified(&events, (fleet, issue), &head) => Err(refuse(
                "NO_REPRO",
                format!("rev's `Repro: verify` PASS has no verify PASS at {head:.7}"),
            )),
            None => Ok(head),
        },
        res => res,
    };
    let res = match res {
        Ok(h) if fast && !crate::land::based(r, f, &h) => Err(refuse(
            "BEHIND",
            format!(
                "{} moved past {h:.7}; drive catches it up, its fast checks run again",
                f.integration
            ),
        )),
        res => res,
    };
    let res = match res {
        Ok(h) => premerge(ctx, cfg, r, out, (fleet, issue), &h)?.map(|s| (h, s)),
        Err(rf) => Err(rf),
    };
    let (head, secs) = match res {
        Ok(h) => h,
        Err(rf)
            if rf.code == "STAGING_MOVED"
                || rf.code == "BEHIND"
                || rf.code == "NO_PR" && st.as_ref().is_none_or(|s| s.lane.is_none())
                || rf.code == "CI_NOT_GREEN"
                    && gh::ci(&gh::required(&pr["statusCheckRollup"], f.checks(&pr))) == "pending" =>
        {
            return Err(rf.into());
        }
        Err(rf) => {
            let kind = if rf.code == "ROUND_LIMIT" {
                "needs-human"
            } else {
                "blocked"
            };
            let d = json!({"code": rf.code, "reason": rf.msg});
            emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(issue), kind, d))?;
            return Err(rf.into());
        }
    };
    gh::forget_pr(ctx, fleet, issue);
    let view = ["pr", "view", &num, "-R", &f.repo, "--json", "number,title,body,files"];
    let info = json(r, "gh", &view).unwrap_or_else(|_| pr.clone());
    let (subject, body, human) = merge_message(&info, issue, st.as_ref().map_or(0, |s| s.rounds), &head);
    if !human {
        let d = json!({"pr": num});
        emit(
            &ctx.state,
            cfg,
            Event::new(ctx.now, fleet, Some(issue), "no-human-summary", d),
        )?;
    }
    let m = [
        "--merge",
        "--match-head-commit",
        &head,
        "--subject",
        &subject,
        "--body",
        &body,
    ];
    let inputs = json!({"pr": num, "head": head, "over": over, "fast": fast});
    let probe = |_: &Event| Ok(landed(&num)?.then(|| json!({"pr": num, "head": head})));
    let at = (fleet, issue, "merge", head.as_str());
    let intent = crate::action::begin(r, &ctx.state, ctx.now, at, inputs, MERGE_SECS, &probe)?.act()?;
    ok(r, "gh", &[&["pr", "merge", &num, "-R", &f.repo][..], &m].concat())?;
    intent.done(json!({"pr": num, "head": head}))?;
    merged(ctx, cfg, r, out, (fleet, issue), &pr, (&num, &head, over, secs, fast))
}

/// #509: this issue's merge action not yet `failed` and no `merged` logged after it; `merge` logs it, with its head.
pub fn unlogged_merge<'a>(events: &'a [Event], fleet: &str, issue: u64) -> Option<&'a Event> {
    let logged = |a: &Event| events.iter().any(|e| e.seq > a.seq && e.is("merged", fleet, issue));
    crate::action::last(events, fleet, issue, "merge").filter(|a| a.kind != "failed" && !logged(a))
}

const MERGE_SECS: u64 = 600;

fn merged(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (fleet, issue): (&str, u64),
    pr: &Value,
    (num, head, over, secs, fast): (&str, &str, bool, Option<u64>, bool),
) -> Result<()> {
    let f = cfg.fleet(fleet)?;
    let mc = json(r, "gh", &["pr", "view", num, "-R", &f.repo, "--json", "mergeCommit"])?;
    let merge_sha = mc["mergeCommit"]["oid"].as_str().unwrap_or("?").to_string();
    let mut d = json!({"pr": num, "merge_sha": merge_sha, "verdict_sha": head, "into": f.integration});
    if let Some(s) = secs {
        d["premerge_secs"] = json!(s);
    }
    if over {
        (d["by"], d["override"]) = (json!("owner"), json!("ROUND_LIMIT"));
    }
    emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(issue), "merged", d))?;
    if f.land.is_some() {
        let checks = gh::required(&pr["statusCheckRollup"], f.checks(pr));
        let at = |k: &'static str| {
            checks
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(move |c| crate::usage::epoch(c[k].as_str()?))
        };
        let secs = at("completedAt")
            .max()
            .zip(at("startedAt").min())
            .map(|(b, a)| b.saturating_sub(a));
        let mode = if fast { "fast-first" } else { "full-first" };
        let d = json!({"mode": mode, "sha": merge_sha, "pr": num, "checks_secs": secs});
        emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(issue), "landed", d))?;
    }
    let log = working(&ctx.state)?;
    let why = log.iter().rfind(|e| e.is("why", fleet, issue));
    let why = why.and_then(|e| Some(format!("\nWhy: {}", e.str("owner")?)));
    let evidence = format!(
        "Merged via `drover merge`: PR #{num} → `{}` @ `{merge_sha}`\nPinned PASS verdict sha: `{head}`{}{}",
        f.integration,
        if over { "\nOwner override: ROUND_LIMIT" } else { "" },
        why.unwrap_or_default()
    );
    let n = issue.to_string();
    writeln!(out, "{evidence}")?;
    let comment = ["issue", "comment", &n, "-R", &f.repo, "--body", &evidence];
    if let Err(e) = ok(r, "gh", &comment).or_else(|_| ok(r, "gh", &comment)) {
        let reason = format!("PR #{num} merged @ {merge_sha}, but the evidence comment failed twice: {e:#}");
        let e = Event::new(
            ctx.now,
            fleet,
            Some(issue),
            "evidence-missing",
            json!({"reason": reason}),
        );
        emit(&ctx.state, cfg, e)?;
        bail!("{reason}; issue #{issue} left open");
    }
    let _ = r.run(
        "gh",
        &argv(&["issue", "close", &n, "-R", &f.repo, "--reason", "completed"]),
    );
    if let Err(e) = crate::check::teardown(ctx, cfg, r, out, fleet, issue) {
        writeln!(out, "teardown: {e:#}")?;
    }
    if crate::update::ours(f) {
        let exe = std::path::Path::new(&ctx.exe);
        let root = exe.parent().and_then(|b| b.parent()).unwrap_or(exe);
        let src = f.local_path();
        writeln!(
            out,
            "cargo build --release --locked -p drover --manifest-path {s}/Cargo.toml && sh {s}/scripts/install-gate.sh {s}/target/release/drover {r}/bin/drover && ln -sf drover {r}/bin/herdsman",
            s = src.display(),
            r = root.display()
        )?;
    }
    Ok(())
}

/// #130: a needs-human card's `override` / `extend` / `park`; #151: `drover extend|park` (`spawn: false` = `--no-spawn`). #712: `why` is `park --why`, stored on the `parked` event.
pub fn needs_human(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (fleet, n): (&str, u64),
    op: &str,
    (spawn, by, why): (bool, &str, Option<&str>),
) -> Result<()> {
    let state = held(ctx, fleet, n)?;
    let state = state.as_str();
    match op {
        "override" => merge(ctx, cfg, r, out, fleet, n, true)?,
        "extend" => {
            extended(ctx, (fleet, n), (spawn, by), state)?;
            if spawn {
                let o = crate::spawn::Opts {
                    supersede: true,
                    ..Default::default()
                };
                if let Err(e) = crate::spawn::spawn(ctx, cfg, r, out, (fleet, n, "impl"), &o) {
                    let rf = e.downcast_ref::<Refusal>();
                    let (code, why) = rf.map_or(("ERROR", format!("{e:#}")), |r| (r.code, r.msg.clone()));
                    if crate::pending::refused(ctx, (fleet, n), code, &why)? {
                        writeln!(out, "#{n}: starting, waiting for host ({code}); drive starts the round")?;
                        return Ok(());
                    }
                    let d = json!({"by": by, "reason": format!("{e:#}")});
                    crate::events::append(&ctx.state, &Event::new(ctx.now, fleet, Some(n), "extend-failed", d))?;
                    return Err(e);
                }
            }
        }
        _ => return park(ctx, cfg, r, out, (fleet, n), by, why),
    }
    writeln!(out, "#{n}: {op}")?;
    Ok(())
}

/// #975: the owner's extend decided, once: `round-extended` off needs-human, then its pending start unless one waits.
pub fn extended(ctx: &Ctx, (fleet, n): (&str, u64), (spawn, by): (bool, &str), state: &str) -> Result<()> {
    if state == "needs-human" {
        let d = json!({"by": by, "spawn": spawn});
        crate::events::append(&ctx.state, &Event::new(ctx.now, fleet, Some(n), "round-extended", d))?;
    }
    let waits =
        crate::pending::of(&working(&ctx.state)?, fleet, n).is_some_and(|p| p.action == "extend" && !p.back(ctx.now));
    if spawn && !waits {
        crate::pending::record(ctx, (fleet, n), "extend", "impl", None)?;
    }
    Ok(())
}

/// Issue `n`'s state if it waits on the owner's needs-human card (or #975 its extend's start), else `NOT_NEEDS_HUMAN`.
pub fn held(ctx: &Ctx, fleet: &str, n: u64) -> Result<String> {
    let st = issue_state(ctx, fleet, n)?.map(|s| (s.state, s.last_event));
    let (state, last) = st.as_ref().map_or(("unknown", ""), |(s, l)| (s.as_str(), l.as_str()));
    let events = working(&ctx.state)?;
    let back = crate::pending::of(&events, fleet, n).is_some_and(|p| p.back(ctx.now));
    let starting = !matches!(state, "merged" | "closed" | "parked") && crate::pending::extending(&events, fleet, n);
    if state != "needs-human" && last != "extend-failed" && !back && !starting {
        return refused("NOT_NEEDS_HUMAN", format!("#{n} is {state}, not needs-human"));
    }
    Ok(state.into())
}

/// `park`: log `parked` (`why`, if any) and reap what the issue's closed lanes left.
pub fn park(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (fleet, n): (&str, u64),
    by: &str,
    why: Option<&str>,
) -> Result<()> {
    let mut e = Event::new(ctx.now, fleet, Some(n), "parked", json!({"by": by}));
    if let Some(w) = why.filter(|w| !w.trim().is_empty()) {
        e.data.insert("why".into(), json!(w));
    }
    crate::events::append(&ctx.state, &e)?;
    if let Ok(a) = term::agents(r) {
        check::leftovers(ctx, cfg, r, &a, true)?;
    }
    Ok(writeln!(out, "#{n}: park")?)
}

/// #712: `drover unpark` / a row's [unpark]: a parked issue goes back to the state it had before the needs-human that the park closed (or, parked from a failed extend's card, the state it was parked in); `unparked` stores it as `repaired` does. Its queue place is kept; out of the queue, it goes to the end.
pub fn unpark(ctx: &Ctx, cfg: &Config, out: &mut dyn Write, (fleet, n): (&str, u64), by: &str) -> Result<()> {
    let events = working(&ctx.state)?;
    let mine = |e: &&Event| e.fleet == fleet && e.issue == Some(n);
    let state = fold(&events).remove(&(fleet.to_string(), n)).map(|s| s.state);
    let Some(at) = events
        .iter()
        .rposition(|e| mine(&e) && e.kind == "parked")
        .filter(|_| state.as_deref() == Some("parked"))
    else {
        return refused(
            "NOT_PARKED",
            format!("#{n} is {}, not parked", state.as_deref().unwrap_or("unknown")),
        );
    };
    let was = |i: usize| fold(&events[..i]).remove(&(fleet.to_string(), n)).map(|s| s.state);
    let mut to = was(at).unwrap_or_else(|| "queued".into());
    if to == "needs-human" {
        let nh = events[..at]
            .iter()
            .rposition(|e| mine(&e) && e.kind == "needs-human")
            .unwrap_or(at);
        to = was(nh)
            .filter(|s| s != "needs-human")
            .unwrap_or_else(|| "queued".into());
    }
    emit(
        &ctx.state,
        cfg,
        Event::new(ctx.now, fleet, Some(n), "unparked", json!({"by": by, "state": to})),
    )?;
    if !crate::queue::replay(&events, fleet).iter().any(|i| i.issue == n) {
        crate::queue_change(
            ctx,
            cfg,
            fleet,
            crate::queue::op("add", &[n], false, None, None, None),
            None,
        )?;
    }
    writeln!(out, "#{n}: unparked, back to {to}")?;
    Ok(())
}
