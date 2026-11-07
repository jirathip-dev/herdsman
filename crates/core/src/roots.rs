//! #630: a host's workspace roots. A lane's worktree is `<root>/<repo>/<lane>`, its builds go under `<root>/.build`.
//! A new lane takes the first root that is mounted, writable and has `min_free_gb`; `hosts eject` empties one, and a
//! pulled one's lanes restart on the next. Nothing moves back on its own.
use crate::config::{Config, HostCfg};
use crate::events::{emit, fold, read, Event};
use crate::hosts::{under, On};
use crate::runner::{ok, Runner};
use crate::Ctx;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DEFAULT: &str = "~/.local/share/herdsman/work";
/// The folder under a root holding its lanes' build caches.
pub const BUILD: &str = ".build";
const KINDS: [&str; 3] = ["root-eject", "root-gone", "root-back"];

/// Host `name`'s roots in order, expanded under its `home`; none set: `def`.
pub fn listed(cfg: &Config, name: &str, home: Option<&str>, def: &Path) -> Vec<PathBuf> {
    let set = cfg.hosts.get(name).map(HostCfg::roots).unwrap_or_default();
    let v: Vec<PathBuf> = set.iter().map(|p| under(p, home)).collect();
    Some(v)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| vec![def.to_path_buf()])
}

/// `active`, `ejecting` (`hosts eject` ran, still mounted: it takes no new lane) or `gone` (not mounted or writable).
pub fn state(events: &[Event], on: &On, name: &str, root: &Path) -> &'static str {
    let r = root.to_string_lossy();
    let mark =
        |e: &&Event| KINDS.contains(&e.kind.as_str()) && e.str("host") == Some(name) && e.str("root") == Some(&r);
    let mounted = match on.machine {
        None => root.is_dir() && std::fs::metadata(root).is_ok_and(|m| !m.permissions().readonly()),
        Some(_) => on
            .sh("test -d \"$1\" && test -w \"$1\"", root, "")
            .is_ok_and(|o| o.code == 0),
    };
    match events.iter().rev().find(mark) {
        _ if !mounted => "gone",
        Some(e) if e.kind == "root-eject" => "ejecting",
        _ => "active",
    }
}

/// Free GB at `root` (`df -k`), when readable.
pub fn free(on: &On, root: &Path) -> Option<f64> {
    let s = ok(on, "df", &["-k", &root.to_string_lossy()]).ok()?;
    Some(s.lines().nth(1)?.split_whitespace().nth(3)?.parse::<f64>().ok()? / 1_048_576.0)
}

/// The root a new lane on host `name` takes: `prefer` (its issue's last) while it is active with room, else the first
/// that is; none: the last.
pub fn pick(cfg: &Config, events: &[Event], on: &On, name: &str, roots: &[PathBuf], prefer: Option<&str>) -> PathBuf {
    let fits = |p: &&PathBuf| {
        state(events, on, name, p) == "active" && free(on, p).is_none_or(|g| g >= cfg.limits.min_free_gb)
    };
    let pref = roots
        .iter()
        .filter(|p| Some(&*p.to_string_lossy()) == prefer)
        .find(fits);
    pref.or_else(|| roots.iter().find(fits))
        .or(roots.last())
        .cloned()
        .unwrap_or_default()
}

/// A root as the last look saw it (`hosts.json`, the host card, `/api/v1`).
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(optional_fields = nullable, rename = "HostRoot"))]
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Root {
    pub path: String,
    pub state: String,
    pub free_gb: Option<f64>,
}

/// Each of host `name`'s set roots, looked at.
pub fn rows(cfg: &Config, events: &[Event], on: &On, name: &str, home: &str) -> Vec<Root> {
    let home = on.machine.is_some().then_some(home);
    let set = cfg.hosts.get(name).is_some_and(|h| !h.roots().is_empty());
    let roots = listed(cfg, name, home, Path::new("")).into_iter().filter(|_| set);
    let row = |p: PathBuf| Root {
        state: state(events, on, name, &p).into(),
        free_gb: free(on, &p),
        path: p.display().to_string(),
    };
    roots.map(row).collect()
}

/// Is spawn `e`'s lane under `root` (its logged root, else its worktree's path)?
fn inside(e: &Event, root: &Path) -> bool {
    let wt = e.str("worktree").map(Path::new);
    e.str("root")
        .map_or(wt.is_some_and(|w| w.starts_with(root)), |r| Path::new(r) == root)
}

/// `herdsman hosts eject <host> [<root>]` (default: its first root): no new lane goes there; each lane there is saved
/// and restarted on the next root; then `safe to unplug <root>` once no herdr agent works there (exit 0), else 1.
pub fn eject(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    name: &str,
    root: Option<&str>,
) -> Result<i32> {
    let Some(h) = cfg.hosts.get(name) else {
        return crate::gate::refused("BAD_VALUE", format!("no [hosts.{name}]"));
    };
    let home = crate::hosts::cached(ctx)
        .remove(name)
        .map(|s| s.home)
        .filter(|_| h.machine.is_some());
    let on = On::new(r, h.machine.as_deref());
    let def = home
        .as_deref()
        .map_or(ctx.worktrees.clone(), |h| under(DEFAULT, Some(h)));
    let all = listed(cfg, name, home.as_deref(), &def);
    let root = root.map_or(all[0].clone(), |p| under(p, home.as_deref()));
    if !all.contains(&root) || all.len() < 2 {
        return crate::gate::refused(
            "BAD_VALUE",
            format!("{}: not one of {name}'s roots, or its last", root.display()),
        );
    }
    let d = json!({"host": name, "root": root});
    emit(&ctx.state, cfg, Event::new(ctx.now, "herdsman", None, "root-eject", d))?;
    restart(ctx, cfg, r, out, (name, &root), false)?;
    let open: Vec<String> = (crate::host::agents(&on)?.iter())
        .filter(|a| a["cwd"].as_str().is_some_and(|c| Path::new(c).starts_with(&root)))
        .map(|a| a["name"].as_str().unwrap_or("?").to_string())
        .collect();
    if open.is_empty() {
        writeln!(out, "safe to unplug {}", root.display())?;
        return Ok(0);
    }
    writeln!(out, "not safe yet, still open there: {}", open.join(", "))?;
    Ok(1)
}

/// #630: `check`'s look at each host's roots: one that vanished under a lane is noted once (`! root <path> gone`) and
/// its lanes restart on the next root; one mounted again is logged back (new lanes use it).
pub fn watch(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let events = read(&ctx.state)?;
    let cache = crate::hosts::cached(ctx);
    for (name, h) in cfg.hosts.iter().filter(|h| h.1.enabled && !h.1.roots().is_empty()) {
        let home = cache.get(name).map(|s| s.home.as_str()).filter(|_| h.machine.is_some());
        let on = On::quick(r, h.machine.as_deref());
        if crate::host::agents(&on).is_err() {
            continue; // an unreachable host is no gone root
        }
        for root in listed(cfg, name, home, &ctx.worktrees) {
            let s = root.to_string_lossy();
            let used = |e: &Event| e.kind == "spawn" && e.str("host") == Some(name) && e.str("root") == Some(&s);
            let mark = |e: &&Event| KINDS.contains(&e.kind.as_str()) && e.str("root") == Some(&s);
            let last = events.iter().rev().find(mark);
            let gone = state(&events, &on, name, &root) == "gone";
            let kind = match last.map(|e| e.kind.as_str()) {
                Some("root-gone") if !gone => "root-back",
                Some("root-gone") => continue,
                _ if gone && events.iter().any(used) => "root-gone",
                _ => continue,
            };
            if gone {
                writeln!(out, "! root {s} gone")?;
            }
            let d = json!({"host": name, "root": root});
            emit(&ctx.state, cfg, Event::new(ctx.now, "herdsman", None, kind, d))?;
            if gone {
                restart(ctx, cfg, r, out, (name, &root), true)?;
            }
        }
    }
    Ok(())
}

/// Each open issue's lanes in `root` on host `name`: unsaved work to `wip/<agent>-<stamp>` (as `kill`), the worktree
/// removed (`gone`: pruned), and the issue's active lane there spawned again on the next root.
fn restart(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (name, root): (&str, &Path),
    gone: bool,
) -> Result<()> {
    let events = read(&ctx.state)?;
    let log = |s: &crate::events::IssueState, kind: &str, d| {
        emit(&ctx.state, cfg, Event::new(ctx.now, &s.fleet, Some(s.issue), kind, d))
    };
    for s in fold(&events).values().filter(|s| !s.done()) {
        let Some(f) = cfg.fleet.get(&s.fleet) else { continue };
        let mut seen = std::collections::BTreeSet::new();
        let mine: Vec<&Event> = (events.iter().rev())
            .filter(|e| e.is("spawn", &s.fleet, s.issue) && e.str("host").unwrap_or("local") == name && inside(e, root))
            .filter(|e| seen.insert(e.str("worktree")))
            .collect();
        let Some(first) = mine.first() else { continue };
        let on = On::of(r, Some(first));
        let home = crate::hosts::cached(ctx)
            .remove(name)
            .map(|h| h.home)
            .filter(|_| on.machine.is_some());
        let local = under(&f.local, home.as_deref()).display().to_string();
        let mut lost = vec![];
        for e in &mine {
            let (wt, agent) = (
                e.str("worktree").unwrap_or_default(),
                e.str("agent").unwrap_or_default(),
            );
            let git = |a: &[&str]| ok(&on, "git", &[&["-C", wt][..], a].concat());
            if gone || !on.exists(Path::new(wt)) {
                let b = e.str("branch").unwrap_or_default();
                let n = ok(
                    &on,
                    "git",
                    &["-C", &local, "rev-list", "--count", b, "--not", "--remotes"],
                );
                let n = n.unwrap_or_default();
                lost.push(format!("{agent}: uncommitted work, {} commit(s) not pushed", n.trim()));
                continue;
            }
            let _ = crate::herdr::close(&on, e.str("pane").unwrap_or_default());
            let ahead = git(&["rev-list", "--count", "HEAD", "--not", "--remotes"])?;
            if crate::spawn::changes(&git(&["status", "--porcelain"])?) > 0 || ahead.trim() != "0" {
                let w = crate::check::wip_branch(ctx, r, agent);
                if let Err(err) = crate::check::save_wip(&on, wt, &w, "wip: eject snapshot") {
                    writeln!(out, "kept {wt}: {err:#}")?;
                    continue;
                }
                writeln!(out, "saved {wt} to {w}")?;
                log(s, "wip-saved", json!({"agent": agent, "branch": w}))?;
            }
            ok(&on, "git", &["-C", &local, "worktree", "remove", "--force", wt])?;
        }
        let _ = ok(&on, "git", &["-C", &local, "worktree", "prune"]);
        let Some((role, agent)) = s
            .active_lane()
            .filter(|l| mine.iter().any(|e| e.str("agent") == Some(l.1)))
        else {
            continue;
        };
        let d = json!({"agent": agent, "host": name, "root": root, "lost": lost.join("; ")});
        log(s, "root-moved", d)?;
        let o = crate::spawn::Opts {
            supersede: true,
            ..Default::default()
        };
        if let Err(e) = crate::spawn::spawn(ctx, cfg, r, out, (&s.fleet, s.issue, role), &o) {
            writeln!(out, "{agent}: restart failed: {e:#}")?;
        }
    }
    Ok(())
}

/// `herdsman migrate`: each worktree under `~/.herdr/worktrees` moves to `<root>/<repo>/<lane>` (`git worktree move`),
/// a live lane's after it ends; once all have, the old build caches (`~/.cache/herdsman/target`) go.
pub fn migrate(ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let old = ctx.home.join(".herdr/worktrees");
    let ls = |p: &Path| std::fs::read_dir(p).into_iter().flatten().flatten().map(|e| e.path());
    let wts: Vec<PathBuf> = ls(&old)
        .flat_map(|d| ls(&d))
        .filter(|w| w.join(".git").is_file())
        .collect();
    let agents = crate::host::agents(r).ok();
    let mut left = 0;
    for wt in &wts {
        let to = ctx.worktrees.join(wt.strip_prefix(&old)?);
        let cwd = |a: &Value| a["cwd"].as_str().is_some_and(|c| Path::new(c).starts_with(wt));
        let live = agents.as_ref().is_none_or(|a| a.iter().any(cwd));
        let w = wt.to_string_lossy();
        let moved = !live && std::fs::create_dir_all(to.parent().unwrap_or(&to)).is_ok() && {
            ok(r, "git", &["-C", &w, "worktree", "move", &w, &to.to_string_lossy()]).is_ok()
        };
        left += usize::from(!moved);
        let how = ["could not move", "live, left for later:", "moved"][usize::from(live) + 2 * usize::from(moved)];
        writeln!(out, "{how} {w} -> {}", to.display())?;
    }
    let cache = ctx.home.join(".cache/herdsman/target");
    if left == 0 && cache.is_dir() {
        std::fs::remove_dir_all(&cache)?;
        writeln!(out, "dropped the old build caches {}", cache.display())?;
    }
    Ok(())
}
