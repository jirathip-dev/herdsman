use crate::config::{Config, HostCfg};
use crate::events::{emit, fold, working, Event};
use crate::hosts::{under, On};
use crate::runner::{ok, Runner};
use crate::Ctx;
use anyhow::Result;
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DEFAULT: &str = "~/.local/share/drover/work";
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
        None if !crate::runner::answers(root) => return "unreachable",
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

/// The root a new lane on host `name` takes: `prefer` (its issue's last) while it is active with room, else the first that is; none: the last.
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

/// This host's roots (the one `[hosts]` entry without a `machine`), whether they answer or not.
pub fn local(ctx: &Ctx, cfg: &Config) -> Vec<PathBuf> {
    let name = cfg
        .hosts
        .iter()
        .find(|h| h.1.machine.is_none())
        .map_or("", |h| h.0.as_str());
    listed(cfg, name, None, &ctx.worktrees)
}

/// #972: this host's roots whose listing answered this pass.
pub fn answering(ctx: &Ctx, cfg: &Config) -> Vec<PathBuf> {
    local(ctx, cfg)
        .into_iter()
        .filter(|r| crate::runner::answers(r))
        .collect()
}

/// #976: why a pass skips `sp`'s worktree: `off-root`, `root unreachable` or `root-hung` (its `.git` read timed out).
pub fn skip(ctx: &Ctx, cfg: &Config, sp: &Event) -> Option<&'static str> {
    let wt = sp.str("worktree").filter(|_| sp.str("machine").is_none())?;
    let (roots, set) = (local(ctx, cfg), cfg.hosts.values().find(|h| h.machine.is_none()));
    let root = roots.iter().find(|r| inside(sp, r));
    if root.is_none() && set.is_some_and(|h| !h.roots().is_empty()) {
        return Some("off-root");
    }
    if root.is_some_and(|r| !crate::runner::answers(r)) {
        return Some("root unreachable");
    }
    let wait = std::time::Duration::from_secs(cfg.limits.lane_read_secs);
    let hung = crate::runner::hung(wt, wait, |d| {
        use std::os::unix::fs::MetadataExt;
        let dev = std::fs::metadata(d).ok()?.dev();
        drop(std::fs::read(Path::new(d).join(".git")));
        Some(dev)
    });
    let mut open = HANGS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let open = open.entry(ctx.state.clone()).or_insert_with(|| {
        let mut set = std::collections::BTreeSet::new();
        for e in crate::events::working(&ctx.state).iter().flat_map(|l| l.iter()) {
            match (e.kind.as_str(), e.str("worktree")) {
                ("root-hung", Some(w)) => set.insert(w.to_string()),
                ("root-answers", Some(w)) => set.remove(w),
                _ => false,
            };
        }
        set
    });
    if hung != open.contains(wt) {
        let (kind, how) = [("root-answers", "answers again"), ("root-hung", "not answering")][usize::from(hung)];
        let d = json!({"worktree": wt, "under": root, "reason": format!("lane worktree {wt} {how}")});
        let _ = emit(&ctx.state, cfg, Event::new(ctx.now, &sp.fleet, sp.issue, kind, d));
        let _ = if hung { open.insert(wt.into()) } else { open.remove(wt) };
    }
    hung.then_some("root-hung")
}

static HANGS: std::sync::Mutex<std::collections::BTreeMap<PathBuf, std::collections::BTreeSet<String>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// #972: the roots whose last `root-hung` / `root-answers` is `root-hung`.
pub fn hung_now(events: &[Event]) -> Vec<&str> {
    let mut last = std::collections::BTreeMap::new();
    for e in events
        .iter()
        .filter(|e| matches!(e.kind.as_str(), "root-hung" | "root-answers"))
    {
        if let Some(r) = e.str("root") {
            last.insert(r, e.kind == "root-hung");
        }
    }
    last.into_iter().filter(|x| x.1).map(|x| x.0).collect()
}

/// #972: the hung root (of `hung_now`) spawn `sp`'s lane on this host is under.
pub fn hung_under<'a>(hung: &[&'a str], sp: Option<&Event>) -> Option<&'a str> {
    let sp = sp.filter(|e| e.str("machine").is_none())?;
    hung.iter().copied().find(|r| inside(sp, Path::new(r)))
}

fn inside(e: &Event, root: &Path) -> bool {
    let wt = e.str("worktree").map(Path::new);
    e.str("root")
        .map_or(wt.is_some_and(|w| w.starts_with(root)), |r| Path::new(r) == root)
}

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
    emit(&ctx.state, cfg, Event::new(ctx.now, "drover", None, "root-eject", d))?;
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

/// #630: drive's look at each host's roots: one that vanished under a lane is noted once (`! root <path> gone`); #790: each pass, a lane still there restarts on the next root; one mounted again is logged back (new lanes use it).
pub fn watch(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let events = working(&ctx.state)?;
    let cache = crate::hosts::cached(ctx);
    for (name, h) in cfg.hosts.iter().filter(|h| h.1.enabled && !h.1.roots().is_empty()) {
        let home = cache.get(name).map(|s| s.home.as_str()).filter(|_| h.machine.is_some());
        let on = On::quick(r, h.machine.as_deref());
        if crate::host::agents(&on).is_err() {
            continue;
        }
        for root in listed(cfg, name, home, &ctx.worktrees) {
            let s = root.to_string_lossy();
            let used = |e: &Event| e.kind == "spawn" && e.str("host") == Some(name) && e.str("root") == Some(&s);
            let mark = |e: &&Event| KINDS.contains(&e.kind.as_str()) && e.str("root") == Some(&s);
            let last = events.iter().rev().find(mark);
            let gone = state(&events, &on, name, &root) == "gone";
            let kind = match last.map(|e| e.kind.as_str()) {
                Some("root-gone") if !gone => Some("root-back"),
                Some("root-gone") => None,
                _ if gone && events.iter().any(used) => Some("root-gone"),
                _ => None,
            };
            if let Some(kind) = kind {
                if gone {
                    writeln!(out, "! root {s} gone")?;
                }
                let d = json!({"host": name, "root": root});
                emit(&ctx.state, cfg, Event::new(ctx.now, "drover", None, kind, d))?;
            }
            if gone {
                restart(ctx, cfg, r, out, (name, &root), true)?;
            }
        }
    }
    Ok(())
}

fn restart(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (name, root): (&str, &Path),
    gone: bool,
) -> Result<()> {
    let events = working(&ctx.state)?;
    let log = |s: &crate::events::IssueState, kind: &str, d| {
        emit(&ctx.state, cfg, Event::new(ctx.now, &s.fleet, Some(s.issue), kind, d))
    };
    for s in fold(&events).values().filter(|s| !s.done()) {
        let Some(f) = cfg.fleet.get(&s.fleet) else { continue };
        let here = |e: &&Event| e.str("host").unwrap_or("local") == name && inside(e, root);
        let Some((role, agent)) = s.active_lane() else { continue };
        let latest = |events: &[Event]| -> Option<Event> {
            events
                .iter()
                .rfind(|e| e.is("spawn", &s.fleet, s.issue) && e.str("agent") == Some(agent))
                .cloned()
        };
        let Some(first) = latest(&events).filter(|e| here(&e)) else {
            continue;
        };
        let mut seen = std::collections::BTreeSet::new();
        let mine: Vec<&Event> = (events.iter().rev())
            .filter(|e| e.is("spawn", &s.fleet, s.issue) && here(e))
            .filter(|e| seen.insert(e.str("worktree")))
            .collect();
        let on = On::of(r, Some(&first));
        let home = crate::hosts::cached(ctx)
            .remove(name)
            .map(|h| h.home)
            .filter(|_| on.machine.is_some());
        let local = under(&f.local, home.as_deref()).display().to_string();
        let hung = on.machine.is_none() && !crate::runner::answers(root);
        let mut lost = vec![];
        for e in &mine {
            let (wt, agent) = (
                e.str("worktree").unwrap_or_default(),
                e.str("agent").unwrap_or_default(),
            );
            let git = |a: &[&str]| ok(&on, "git", &[&["-C", wt][..], a].concat());
            if hung {
                let _ = crate::term::close(&on, e.str("pane").unwrap_or_default());
                forget(Path::new(&local), Path::new(wt), e.str("branch").unwrap_or_default())?;
                writeln!(out, "not saved {wt}: root hung, unsaved work stays on the disk")?;
                lost.push(format!("{agent}: not saved, root hung"));
                log(s, "wip-skipped", json!({"agent": agent, "worktree": wt, "root": root}))?;
                continue;
            }
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
            let _ = crate::term::close(&on, e.str("pane").unwrap_or_default());
            let ahead = git(&["rev-list", "--count", "HEAD", "--not", "--remotes"])?;
            if crate::spawn::changes(&git(&["status", "--porcelain"])?) > 0 || ahead.trim() != "0" {
                let w = crate::check::wip_branch(ctx, &on, agent);
                let w = match crate::check::save_wip(&on, wt, &w, "wip: eject snapshot") {
                    Ok(w) => w,
                    Err(err) => {
                        writeln!(out, "kept {wt}: {err:#}")?;
                        continue;
                    }
                };
                writeln!(out, "saved {wt} to {w}")?;
                log(s, "wip-saved", json!({"agent": agent, "branch": w}))?;
            }
            ok(&on, "git", &["-C", &local, "worktree", "remove", "--force", wt])?;
        }
        if !hung {
            let _ = ok(&on, "git", &["-C", &local, "worktree", "prune"]);
        }
        let o = crate::spawn::Opts {
            supersede: true,
            ..Default::default()
        };
        if let Err(e) = crate::spawn::spawn(ctx, cfg, r, out, (&s.fleet, s.issue, role), &o) {
            writeln!(out, "{agent}: restart failed: {e:#}")?;
        }
        if let Some(to) = latest(&working(&ctx.state)?).filter(|e| !here(&e)) {
            let lost = lost.join("; ");
            let d = json!({"agent": agent, "host": name, "root": root, "to": to.str("root"), "to_host": to.str("host"), "lost": lost});
            log(s, "root-moved", d)?;
        }
    }
    Ok(())
}

fn forget(local: &Path, wt: &Path, branch: &str) -> Result<()> {
    let head = format!("ref: refs/heads/{branch}");
    for d in std::fs::read_dir(local.join(".git/worktrees"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let read = |f: &str| std::fs::read_to_string(d.path().join(f)).unwrap_or_default();
        if Path::new(read("gitdir").trim()).starts_with(wt) || !branch.is_empty() && read("HEAD").trim() == head {
            std::fs::remove_dir_all(d.path())?;
        }
    }
    Ok(())
}
