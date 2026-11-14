use crate::config::{Config, Fleet};
use crate::events::{emit, fold, Event, IssueState};
use crate::runner::{argv, ok, Runner};
use crate::{gh, Ctx};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Per issue: (the other issue, 0 for integration, the files both change).
pub type Conflicts = BTreeMap<u64, Vec<(u64, Vec<String>)>>;

pub fn git(r: &dyn Runner, dir: &Path, args: &[&str]) -> Result<String> {
    let d = dir.to_string_lossy();
    ok(r, "git", &[&["-C", &*d], args].concat()).map(|o| o.trim().to_string())
}

/// The last preview's conflicts; never runs git.
pub fn cached(ctx: &Ctx, fleet: &str) -> Conflicts {
    let v = crate::usage::load::<Value>(&gh::cache_path(ctx, "conflicts", fleet, 0)).unwrap_or_default();
    serde_json::from_value(v["conflicts"].clone()).unwrap_or_default()
}

fn collide(r: &dyn Runner, dir: &Path, a: &str, b: &str) -> Vec<String> {
    let d = dir.to_string_lossy();
    let o = r.run(
        "git",
        &argv(&["-C", &d, "merge-tree", "--write-tree", "--name-only", a, b]),
    );
    let o = o.ok().filter(|o| o.code == 1).unwrap_or_default();
    o.stdout
        .lines()
        .skip(1)
        .take_while(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

fn keep_going(o: u64) -> String {
    format!("may conflict with #{o}; keep going, drover updates you after it merges")
}

/// Every pair of pushed, unmerged branches and each against integration, cached by head pair; logs one `conflict-predicted` per new pair and prompts an impl/fix lane once per head about a ready PR or integration.
pub fn preview(ctx: &Ctx, cfg: &Config, r: &dyn Runner, fleet: &str, events: &[Event]) -> Result<Conflicts> {
    let (f, p) = (cfg.fleet(fleet)?, gh::cache_path(ctx, "conflicts", fleet, 0));
    let dir = f.local_path();
    let rev = |b: &str| git(r, &dir, &["rev-parse", &format!("origin/{b}")]);
    let ev = |n, kind, d| emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(n), kind, d));
    git(r, &dir, &["fetch", "-q", "origin"])?;
    let st: Vec<IssueState> = fold(events)
        .into_values()
        .filter(|s| s.fleet == fleet && !s.done())
        .collect();
    let pushed = st
        .iter()
        .filter_map(|s| Some((s.issue, rev(&crate::gate::branch_of(Some(s), s.issue)).ok()?)));
    let heads: Vec<(u64, String)> = std::iter::once((0, rev(&f.integration)?)).chain(pushed).collect();
    let old = crate::usage::load::<Value>(&p).unwrap_or_default();
    let (mut cache, mut out) = (BTreeMap::new(), Conflicts::new());
    for (i, (a, ha)) in heads.iter().enumerate() {
        for (b, hb) in &heads[i + 1..] {
            let key = format!("{ha} {hb}");
            let files: Vec<String> =
                serde_json::from_value(old["cache"][&key].clone()).unwrap_or_else(|_| collide(r, &dir, ha, hb));
            if !files.is_empty() && old["cache"][&key].is_null() {
                ev(
                    *b,
                    "conflict-predicted",
                    json!({"other": a, "files": files, "heads": key, "reason": (*a > 0).then(|| keep_going(*a))}),
                )?;
            }
            for (x, y) in [(b, a), (a, b)]
                .into_iter()
                .filter(|(x, _)| **x > 0 && !files.is_empty())
            {
                out.entry(*x).or_default().push((*y, files.clone()));
            }
            cache.insert(key, files);
        }
    }
    gh::store(&p, &json!({"cache": cache, "conflicts": out}))?;
    for s in st.iter().filter(|s| s.state == "impl" || s.state.starts_with("fix")) {
        let ready = |o: &u64| *o == 0 || st.iter().any(|x| x.issue == *o && x.state == "ready");
        let (Some(lane), Some((o, files))) = (&s.lane, out.get(&s.issue).and_then(|c| c.iter().find(|c| ready(&c.0))))
        else {
            continue;
        };
        let key = format!("{} {o}", heads.iter().find(|h| h.0 == s.issue).map_or("", |h| &h.1));
        if events
            .iter()
            .any(|e| e.is("heads-up", fleet, s.issue) && e.str("key") == Some(&key))
        {
            continue;
        }
        let (into, files) = (format!("origin/{}", f.integration), files.join(", "));
        let text = if *o == 0 {
            format!("HEADS-UP: your branch conflicts with {into} on {files}. Merge {into} now.")
        } else {
            format!(
                "HEADS-UP: #{o} is ready to merge and touches {files}: your branch {}.",
                keep_going(*o)
            )
        };
        let sp = crate::events::last_spawn(events, lane);
        let pane = sp.and_then(|e| e.str("pane")).unwrap_or_default();
        let on = crate::hosts::On::of(r, sp);
        if crate::term::tell(&on, sp.and_then(|e| e.str("harness")) == Some("dsh"), lane, pane, &text).is_ok() {
            ev(s.issue, "heads-up", json!({"key": key, "reason": text}))?;
        }
    }
    Ok(out)
}

/// #129: `h` merges a passed head (or an earlier pure update of one) with integration and adds nothing: its first parent carries the PASS, its second is on integration, and its tree is the one `git merge-tree` makes of the two.
pub fn pure(r: &dyn Runner, f: &Fleet, s: &IssueState, h: &str) -> bool {
    let dir = f.local_path();
    git(r, &dir, &["fetch", "-q", "origin"]).is_ok() && pure_at(r, &dir, &format!("origin/{}", f.integration), s, h)
}

/// #207: `h` is a pure update of `x`, so a verdict at `x` (whatever it says) carries to `h`.
pub fn carries(r: &dyn Runner, f: &Fleet, s: &IssueState, x: &str, h: &str) -> bool {
    let mut s = s.clone();
    s.verdicts.push((x.into(), "PASS".into(), String::new()));
    pure(r, f, &s, h)
}

fn pure_at(r: &dyn Runner, dir: &Path, into: &str, s: &IssueState, h: &str) -> bool {
    let Ok(p) = git(r, dir, &["rev-list", "--parents", "-n1", h]) else {
        return false;
    };
    let [_, a, b] = p.split(' ').collect::<Vec<_>>()[..] else {
        return false;
    };
    let pass = s
        .verdicts
        .iter()
        .rfind(|v| v.0.eq_ignore_ascii_case(a))
        .is_some_and(|v| v.1 == "PASS");
    (pass || pure_at(r, dir, into, s, a))
        && git(r, dir, &["merge-base", "--is-ancestor", b, into]).is_ok()
        && git(r, dir, &["merge-tree", "--write-tree", a, b]).ok()
            == git(r, dir, &["rev-parse", &format!("{h}^{{tree}}")]).ok()
}

/// `drover cap-check`: formatted lines of `src/**/*.rs` and `crates/*/src/**/*.rs` (#264) against the first `[fleet.<n>] line_cap` in `config`; #429: under `warn_within` lines of headroom it warns and still passes. #270: `web/src/**/*.ts` lines are reported, never capped.
pub fn cap_check(config: &Path, rules: &Path, out: &mut dyn std::io::Write) -> Result<i32> {
    let (cap, warn) = limits(&std::fs::read_to_string(config)?)
        .ok_or_else(|| anyhow::anyhow!("no [fleet.<name>] line_cap in {}", config.display()))?;
    let crates = std::fs::read_dir("crates").into_iter().flatten().flatten();
    let n = lines(Path::new("src"), "rs") + crates.map(|c| lines(&c.path().join("src"), "rs")).sum::<usize>();
    writeln!(out, "src lines (formatted): {n} / cap {cap}")?;
    writeln!(
        out,
        "web lines (TypeScript, soft budget, not capped): {}",
        lines(Path::new("web/src"), "ts")
    )?;
    let left = cap - i64::try_from(n)?;
    if (0..warn).contains(&left) {
        writeln!(out, "WARN: {left} lines of headroom (< {warn})")?;
    }
    let over = std::fs::read_to_string(rules).map_or(0, |t| t.lines().count());
    if over > 30 {
        writeln!(
            out,
            "{}: {over} lines, over 30: delete the rules a check now enforces",
            rules.display()
        )?;
    }
    Ok(i32::from(left < 0 || over > 30))
}

fn limits(text: &str) -> Option<(i64, i64)> {
    let v: toml::Value = toml::from_str(text).ok()?;
    let t = v
        .get("fleet")?
        .as_table()?
        .values()
        .find(|t| t.get("line_cap").is_some())?;
    let warn = t.get("warn_within").and_then(toml::Value::as_integer).unwrap_or(500);
    Some((t["line_cap"].as_integer()?, warn))
}

/// #429: `(sha, lines, cap, warn_within)` at the fleet's fetched integration head with a `ci/cap.toml`, by git alone; cached per sha (`serve`'s banner reads the cache).
pub fn cap_at(ctx: &Ctx, cfg: &Config, r: &dyn Runner, fleet: &str) -> Option<(String, i64, i64, i64)> {
    let (f, p) = (cfg.fleet(fleet).ok()?, gh::cache_path(ctx, "cap", fleet, 0));
    let dir = f.local_path();
    let sha = git(r, &dir, &["rev-parse", &format!("origin/{}", f.integration)]).ok()?;
    let old = crate::usage::load::<Value>(&p).unwrap_or_default();
    let n = |k: &str| old[k].as_i64();
    if old["sha"] == sha.as_str() {
        return Some((sha, n("lines")?, n("cap")?, n("warn")?));
    }
    let (cap, warn) = limits(&git(r, &dir, &["show", &format!("{sha}:ci/cap.toml")]).ok()?)?;
    let globs = [":(glob)src/**/*.rs", ":(glob)crates/*/src/**/*.rs"];
    let counts = git(r, &dir, &[&["grep", "-c", "", &sha, "--"][..], &globs].concat()).unwrap_or_default();
    let lines = counts
        .lines()
        .filter_map(|l| l.rsplit(':').next()?.parse::<i64>().ok())
        .sum();
    let _ = gh::store(&p, &json!({"sha": sha, "lines": lines, "cap": cap, "warn": warn}));
    Some((sha, lines, cap, warn))
}

fn lines(p: &Path, ext: &str) -> usize {
    let each = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path());
    let n = |p: PathBuf| match p.extension() {
        _ if p.is_dir() => lines(&p, ext),
        Some(x) if x == ext => std::fs::read_to_string(&p).map_or(0, |t| t.lines().count()),
        _ => 0,
    };
    each.map(n).sum()
}

fn stated_net(body: &str) -> Option<i64> {
    let rest = body.split("src/ net ").nth(1)?.trim_start();
    let n: String = rest.chars().take_while(|c| "+-,0123456789".contains(*c)).collect();
    n.replace([',', '+'], "").parse().ok()
}

struct Count {
    label: String,
    lines: i64,
    limit: i64,
}

fn cap_script(r: &dyn Runner, f: &Fleet, sha: &str) -> Option<String> {
    let run = |p: &crate::config::Premerge| match p {
        crate::config::Premerge::Cmd(c) => c.clone(),
        crate::config::Premerge::Named(c) => c.run.clone(),
    };
    let own = || {
        git(
            r,
            &f.local_path(),
            &["cat-file", "-e", &format!("{sha}:scripts/line-cap.sh")],
        )
        .ok()
    };
    (f.cap_script.clone())
        .or_else(|| f.premerge.iter().flatten().map(run).find(|c| c.contains("cap")))
        .or_else(|| own().map(|_| "sh scripts/line-cap.sh".to_string()))
}

fn script_caps(
    ctx: &Ctx,
    r: &dyn Runner,
    f: &Fleet,
    script: &str,
    base: &str,
    branch: Option<&str>,
) -> Result<Vec<Count>, String> {
    let (dir, wt) = (
        f.local_path(),
        ctx.state.with_file_name(format!("cap-{}", f.repo.replace('/', "-"))),
    );
    let wts = wt.to_string_lossy();
    let tree = |a: &[&str]| git(r, &dir, &[&["worktree"][..], a].concat());
    let _ = tree(&["remove", "--force", &wts]);
    tree(&["add", "-f", "--detach", &wts, base]).map_err(|e| e.to_string())?;
    let merged = branch.is_none_or(|b| git(r, &wt, &["merge", "--no-ff", "--no-edit", b]).is_ok());
    let sh = argv(&["-c", &format!("cd \"$0\" && {script}"), &wts]);
    let o = merged.then(|| r.run("sh", &sh));
    let _ = tree(&["remove", "--force", &wts]);
    let o = o.ok_or("conflicts")?.map_err(|e| e.to_string())?;
    let num = |w: &str| w.replace(',', "").parse::<i64>().ok();
    let cap = |l: &str| {
        let (left, rest) = l.split_once(" / ")?;
        let (label, n) = left
            .trim()
            .rsplit_once(char::is_whitespace)
            .unwrap_or(("", left.trim()));
        Some(Count {
            label: label.trim().trim_end_matches(':').trim().to_string(),
            lines: num(n)?,
            limit: num(rest.trim_start().trim_start_matches("cap ").split_whitespace().next()?)?,
        })
    };
    let text = format!("{}{}", o.stdout, o.stderr);
    let caps: Vec<Count> = text.lines().filter_map(cap).collect();
    let last = text.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("nothing");
    (!caps.is_empty())
        .then_some(caps)
        .ok_or_else(|| format!("printed `{}`", last.trim()))
}

/// #668: `drover cap <fleet>`: the queue-wide line-cap view, so one conductor-filed issue raises the cap for the whole queue: integration's total against the cap, every open PR's `src/ net` (its body's, else its measured diff), the queued issues with no PR, and the projected total with the ask a crossing lane files. #754: a fleet with its own cap script counts by it, at integration and on each open PR's merge, every cap it prints.
pub fn cap_view(ctx: &Ctx, cfg: &Config, r: &dyn Runner, fleet: &str, out: &mut dyn std::io::Write) -> Result<i32> {
    let f = cfg.fleet(fleet)?;
    let dir = f.local_path();
    let _ = git(r, &dir, &["fetch", "-q", "origin"]);
    let head = git(r, &dir, &["rev-parse", &format!("origin/{}", f.integration)]);
    let script = head.as_ref().ok().and_then(|h| cap_script(r, f, h));
    let (sha, caps, warn) = if let Some(s) = &script {
        let sha = head?;
        let toml = git(r, &dir, &["show", &format!("{sha}:ci/cap.toml")]).unwrap_or_default();
        let warn = limits(&toml).map_or(500, |l| l.1);
        let caps = script_caps(ctx, r, f, s, &sha, None).map_err(|got| {
            let at = &f.integration;
            anyhow::anyhow!("`{s}` at origin/{at} printed no `N / cap M` line drover can parse: {got}")
        })?;
        (sha, caps, warn)
    } else {
        let (sha, lines, cap, warn) = cap_at(ctx, cfg, r, fleet).ok_or_else(|| {
            anyhow::anyhow!(
                "no ci/cap.toml line_cap at origin/{} in {}",
                f.integration,
                dir.display()
            )
        })?;
        let label = String::new();
        (
            sha,
            vec![Count {
                label,
                lines,
                limit: cap,
            }],
            warn,
        )
    };
    let lab = |c: &Count| format!("{} ", c.label).trim_start().to_string();
    for c in &caps {
        writeln!(
            out,
            "{fleet}: {}{} / cap {} at {} {sha:.7}, headroom {}",
            lab(c),
            c.lines,
            c.limit,
            f.integration,
            c.limit - c.lines
        )?;
    }
    let by = script.as_deref().map_or_else(
        || "the built-in count of src/**/*.rs and crates/*/src/**/*.rs".to_string(),
        |s| format!("`{s}`"),
    );
    writeln!(out, "counted by {by}")?;
    let prs = gh::snapshot(ctx, r, f, fleet, ctx.now)?;
    let mut prs: Vec<&Value> = (prs.as_object().into_iter().flat_map(|m| m.values()))
        .filter(|p| p["baseRefName"] == f.integration.as_str())
        .collect();
    prs.sort_by_key(|p| p["number"].as_u64());
    let globs = [":(glob)src/**/*.rs", ":(glob)crates/*/src/**/*.rs"];
    let measured = |b: &str| -> Option<i64> {
        let range = format!("{sha}...origin/{b}");
        let o = git(r, &dir, &[&["diff", "--numstat", &range, "--"][..], &globs].concat()).ok()?;
        let n = |w: Option<&str>| w.and_then(|w| w.parse::<i64>().ok()).unwrap_or(0);
        Some(
            o.lines()
                .map(|l| l.split('\t'))
                .map(|mut w| n(w.next()) - n(w.next()))
                .sum(),
        )
    };
    let scripted = |s: &str, b: &str| -> Option<Vec<i64>> {
        let m = script_caps(ctx, r, f, s, &sha, Some(&format!("origin/{b}"))).ok()?;
        let net = |c: &Count| Some(m.iter().find(|x| x.label == c.label)?.lines - c.lines);
        caps.iter().map(net).collect()
    };
    let (mut sums, mut has) = (vec![0; caps.len()], std::collections::BTreeSet::new());
    writeln!(out, "open PRs (src/ net):")?;
    for p in prs {
        let b = p["headRefName"].as_str().unwrap_or_default();
        let issue = b.strip_prefix("issue-").and_then(|n| n.parse::<u64>().ok());
        has.extend(issue);
        let who = issue.map_or_else(|| b.to_string(), |n| format!("#{n}"));
        let net = match &script {
            Some(s) => scripted(s, b).map(|n| (n, "script")),
            None => stated_net(p["body"].as_str().unwrap_or_default())
                .map(|n| (vec![n], "body"))
                .or_else(|| measured(b).map(|n| (vec![n], "measured"))),
        };
        let num = p["number"].as_u64().unwrap_or(0);
        match net {
            Some((n, how)) => {
                let each = n.iter().zip(&caps).map(|(n, c)| match caps.len() {
                    1 => format!("{n:+}"),
                    _ => format!("{n:+} {}", c.label),
                });
                sums.iter_mut().zip(&n).for_each(|(s, n)| *s += n);
                writeln!(
                    out,
                    "  {who} (PR #{num}): {} ({how})",
                    each.collect::<Vec<_>>().join(", ")
                )?;
            }
            None if script.is_some() => writeln!(
                out,
                "  {who} (PR #{num}): unknown (conflicts with {} or the script printed no cap)",
                f.integration
            )?,
            None => writeln!(
                out,
                "  {who} (PR #{num}): unknown (no `src/ net` in its body, branch not fetched)"
            )?,
        }
    }
    let (events, st) = (crate::events::working(&ctx.state)?, crate::events::states(&ctx.state)?);
    let queued: Vec<u64> = (crate::queue::view(&events, &st, fleet).into_iter().map(|i| i.issue))
        .filter(|n| !has.contains(n))
        .collect();
    let none = queued.is_empty().then_some("none");
    writeln!(
        out,
        "queued, no PR: {}",
        none.map_or_else(|| crate::queue::hashes(&queued), String::from)
    )?;
    let mut raise = Vec::new();
    for (c, sum) in caps.iter().zip(&sums) {
        let projected = c.lines + sum;
        let of = if caps.len() > 1 {
            format!("{}: ", c.label)
        } else {
            String::new()
        };
        writeln!(
            out,
            "{of}sum of open PRs: {sum:+}; projected total {projected} / cap {}, headroom {}",
            c.limit,
            c.limit - projected
        )?;
        raise.push((projected > c.limit, format!("{}to {}", lab(c), projected + warn)));
    }
    if raise.iter().any(|r| r.0) {
        raise.retain(|r| r.0);
    }
    let raise: Vec<String> = raise.into_iter().map(|r| r.1).collect();
    writeln!(
        out,
        "ask options: 1. raise {} in one orchestrator-filed issue (recommended) 2. trim",
        raise.join(", ")
    )?;
    Ok(0)
}
