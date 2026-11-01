//! #129: fewer merge conflicts, with git alone: `git merge-tree` previews which open branches collide, a merge of
//! integration that adds nothing keeps a PASS, and `cap-check` reads the line cap from config.
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

/// The files `a` and `b` conflict on (`git merge-tree` exits 1, no worktree); empty when they merge cleanly.
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

/// Every pair of pushed, unmerged branches and each against integration, cached by head pair; logs one
/// `conflict-predicted` per new pair and prompts an impl/fix lane once per head about a ready PR or integration.
pub fn preview(ctx: &Ctx, cfg: &Config, r: &dyn Runner, fleet: &str, events: &[Event]) -> Result<Conflicts> {
    let (f, p) = (cfg.fleet(fleet)?, gh::cache_path(ctx, "conflicts", fleet, 0));
    let dir = f.local_path();
    let rev = |b: &str| git(r, &dir, &["rev-parse", &format!("origin/{b}")]);
    let ev = |n, kind, d| emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(n), kind, d));
    git(r, &dir, &["fetch", "-q", "origin"])?;
    let st: Vec<IssueState> = fold(events)
        .into_values()
        .filter(|s| s.fleet == fleet && s.state != "merged")
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
                    json!({"other": a, "files": files, "heads": key}),
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
            format!("HEADS-UP: your branch will conflict with #{o} on {files} (ready to merge). Merge {into} after #{o} lands.")
        };
        let sp = crate::events::last_spawn(events, lane);
        let pane = sp.and_then(|e| e.str("pane")).unwrap_or_default();
        if crate::herdr::reply(r, sp.and_then(|e| e.str("harness")) == Some("dsh"), lane, pane, &text).is_ok() {
            ev(s.issue, "heads-up", json!({"key": key, "reason": text}))?;
        }
    }
    Ok(out)
}

/// #129: `h` merges a passed head (or an earlier pure update of one) with integration and adds nothing: its first parent
/// carries the PASS, its second is on integration, and its tree is the one `git merge-tree` makes of the two.
pub fn pure(r: &dyn Runner, f: &Fleet, s: &IssueState, h: &str) -> bool {
    let dir = f.local_path();
    git(r, &dir, &["fetch", "-q", "origin"]).is_ok() && pure_at(r, &dir, &format!("origin/{}", f.integration), s, h)
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

/// `herdsman cap-check`: formatted lines of `src/**/*.rs` against the first `[fleet.<n>] line_cap` in `config`.
pub fn cap_check(config: &Path, out: &mut dyn std::io::Write) -> Result<i32> {
    let v: toml::Value = toml::from_str(&std::fs::read_to_string(config)?)?;
    let fleets = v.get("fleet").and_then(toml::Value::as_table);
    let cap = fleets.and_then(|t| t.values().find_map(|t| t.get("line_cap")?.as_integer()));
    let cap = cap.ok_or_else(|| anyhow::anyhow!("no [fleet.<name>] line_cap in {}", config.display()))?;
    let n = lines(Path::new("src"));
    writeln!(out, "src lines (formatted): {n} / cap {cap}")?;
    Ok(i32::from(i64::try_from(n)? > cap))
}

fn lines(p: &Path) -> usize {
    let each = std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path());
    let n = |p: PathBuf| match p.extension() {
        _ if p.is_dir() => lines(&p),
        Some(x) if x == "rs" => std::fs::read_to_string(&p).map_or(0, |t| t.lines().count()),
        _ => 0,
    };
    each.map(n).sum()
}
