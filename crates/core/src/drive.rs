use crate::config::Config;
use crate::events::{append, emit, fold, working, Event, IssueState};
use crate::overlap::git;
use crate::runner::{ok, Runner};
use crate::waiting::Waiting;
use crate::{gh, queue, stall, waiting, Ctx};
use anyhow::Result;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// #845: one pass's steps in order (`lap` names each, `pass-slow` the slowest) and the `[limits]` keys that time them.
pub const STEPS: [(&str, &[&str]); 8] = [
    ("read log, host sample", &[]),
    (
        "disk budget",
        &["shed_read_millis", "worktree_budget_gb", "build_budget_gb"],
    ),
    ("roots, redeliver", &["host_offline_minutes"]),
    (
        "look, orphans",
        &["ssh_quick_secs", "ssh_call_secs", "import_max_age_hours"],
    ),
    ("repair, outbox", &[]),
    (
        "PR reads, gates, steps",
        &[
            "webhook_poll_minutes",
            "silent_minutes",
            "ci_start_minutes",
            "gate_check_minutes",
            "review_rounds",
            "update_rounds",
        ],
    ),
    (
        "spawns",
        &[
            "max_parallel_spawns",
            "spawn_pending_max_secs",
            "respawn_max",
            "respawn_window_minutes",
            "fallback_rest_minutes",
            "overflow_after_minutes",
        ],
    ),
    ("backlog, checkpoint", &["rotate_mb"]),
];
/// #677: drive's one prompt on a `done` whose why note was refused.
pub const WHY_NUDGE: &str = "your `turn done` was refused: ";
const STALE_LOCK_SECS: u64 = 3600;

struct Lock(std::path::PathBuf, crate::db::Lease);
impl Drop for Lock {
    fn drop(&mut self) {
        crate::db::release(&self.0, &self.1.key, Some(&self.1));
    }
}

fn take(ctx: &Ctx, r: &dyn Runner, key: &str, secs: u64, data: &str) -> Option<Lock> {
    let now = ctx.now;
    let dead = |l: &crate::db::Lease| gone(r, l, now);
    crate::db::take(&ctx.state, key, now, secs, data, &dead).map(|l| Lock(ctx.state.clone(), l))
}

fn held(ctx: &Ctx, key: &str) -> Option<crate::db::Lease> {
    let mut all = crate::db::leases(&ctx.state, key).into_iter();
    all.find(|l| l.key == key && l.expires_at > ctx.now)
}

fn gone(r: &dyn Runner, l: &crate::db::Lease, now: u64) -> bool {
    l.pid != 0 && l.host == crate::db::host() && !alive(r, &l.pid.to_string(), now.saturating_sub(l.at))
}

fn alive(r: &dyn Runner, pid: &str, age: u64) -> bool {
    let up = |s: String| {
        let (d, hms) = s
            .trim()
            .split_once('-')
            .map_or(Some((0, s.trim())), |(d, t)| Some((d.parse::<u64>().ok()?, t)))?;
        Some(
            d * 86400
                + hms
                    .split(':')
                    .try_fold(0, |a, x| Some(a * 60 + x.parse::<u64>().ok()?))?,
        )
    };
    let up = || ok(r, "ps", &["-o", "etime=", "-p", pid]).ok().and_then(up);
    ok(r, "kill", &["-0", pid]).is_ok() && up().is_none_or(|up| up + 5 >= age)
}

/// #333: each spawn marker and kick lock (#344: leases), live or stale: stale once its pid is gone (or reused) or it expired. `remove`: release each stale one, log `stale-marker`.
pub fn markers(ctx: &Ctx, cfg: &Config, r: &dyn Runner, remove: bool) -> (Vec<String>, Vec<Event>) {
    let (mut live, mut stale) = (vec![], vec![]);
    let all = [
        crate::db::leases(&ctx.state, "spawn-"),
        crate::db::leases(&ctx.state, "kick-"),
    ]
    .concat();
    for l in all {
        let name = l.key.clone();
        let spawn = name.strip_prefix("spawn-").and_then(|n| n.strip_suffix(".pending"));
        let Some(id) = spawn.or_else(|| name.strip_prefix("kick-")?.strip_suffix(".lock")) else {
            continue;
        };
        let age = ctx.now.saturating_sub(l.at);
        if holds(ctx, cfg, r, &l) {
            live.push(name);
            continue;
        }
        let (fleet, n) = id.rsplit_once('-').map_or((id, None), |(f, n)| (f, n.parse().ok()));
        let e = Event::new(
            ctx.now,
            fleet,
            n,
            "stale-marker",
            json!({"file": name, "pid": l.pid, "age": age}),
        );
        if remove && crate::db::release(&ctx.state, &l.key, Some(&l)) {
            let _ = append(&ctx.state, &e);
        }
        stale.push(e);
    }
    (live, stale)
}

fn holds(ctx: &Ctx, cfg: &Config, r: &dyn Runner, l: &crate::db::Lease) -> bool {
    l.expires_at > ctx.now && ctx.now.saturating_sub(l.at) <= cfg.limits.spawn_pending_max_secs && !gone(r, l, ctx.now)
}

fn starting(ctx: &Ctx, cfg: &Config, r: &dyn Runner) -> Vec<crate::db::Lease> {
    let mut v: Vec<_> = (crate::db::leases(&ctx.state, "spawn-").into_iter())
        .filter(|l| l.key.ends_with(".pending") && holds(ctx, cfg, r, l))
        .collect();
    v.sort_by_key(|l| l.at);
    v
}

/// #812: what holds the spawn slots, each marker's key and age (`spawn-a-1.pending 4m, …`); empty when none does.
pub fn held_by(ctx: &Ctx, cfg: &Config, r: &dyn Runner) -> String {
    let at = |l: &crate::db::Lease| format!("{} {}", l.key, crate::events::age(ctx.now.saturating_sub(l.at)));
    starting(ctx, cfg, r).iter().map(at).collect::<Vec<_>>().join(", ")
}

/// #333: `spawns starting N/<max>` and the stale markers swept in the last day: doctor's and the Host tab's line.
pub fn spawns_line(ctx: &Ctx, cfg: &Config, r: &dyn Runner, events: &[Event]) -> String {
    let n = markers(ctx, cfg, r, false)
        .0
        .iter()
        .filter(|m| m.starts_with("spawn-"))
        .count();
    let day = |e: &&Event| e.kind == "stale-marker" && ctx.now < e.ts + 86400;
    let mut seen: Vec<(&str, usize)> = vec![];
    for f in events.iter().filter(day).filter_map(|e| e.str("file")) {
        match seen.iter_mut().find(|s| s.0 == f) {
            Some(s) => s.1 += 1,
            None => seen.push((f, 1)),
        }
    }
    let more = seen.len().saturating_sub(5);
    let mut gone: Vec<String> = seen
        .iter()
        .take(5)
        .map(|(f, n)| if *n > 1 { format!("{f} ×{n}") } else { f.to_string() })
        .collect();
    gone.extend((more > 0).then(|| format!("+{more} more")));
    let gone = (!gone.is_empty()).then(|| format!(" · stale removed (24 h): {}", gone.join(", ")));
    format!(
        "spawns starting {n}/{}{}",
        cfg.limits.max_parallel_spawns,
        gone.unwrap_or_default()
    )
}

/// #246: the last fast step: (started, secs) from `drive-fast.at`.
pub fn fast_step(ctx: &Ctx) -> Option<(u64, Option<f64>)> {
    let s = std::fs::read_to_string(ctx.state.with_file_name("drive-fast.at")).ok()?;
    let mut w = s.split_whitespace();
    Some((w.next()?.parse().ok()?, w.next().and_then(|x| x.parse().ok())))
}

/// `<state dir>/drive-<fleet>.last`: the time of the fleet's last pass (the board's `last pass`).
pub fn last_pass(ctx: &Ctx, fleet: &str) -> Option<u64> {
    std::fs::read_to_string(state_dir(ctx).join(format!("drive-{fleet}.last")))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn state_dir(ctx: &Ctx) -> &Path {
    ctx.state.parent().unwrap_or(&ctx.state)
}

/// #140: a pass's lines as one toast, per action (`merged #1, #2 · started #3`), then `2 skipped (<codes>)`.
pub fn summary(fleet: &str, out: &str) -> String {
    if out.starts_with("drive: ") {
        return out.trim().into();
    }
    let (mut by, mut skip) = (BTreeMap::<&str, Vec<&str>>::new(), vec![]);
    for (head, res) in out.lines().filter_map(|l| l.split_once(": ")) {
        let (step, mut w) = (head.rsplit(' ').next().unwrap_or(head), res.split([' ', ':']));
        let past = if res == "pending" {
            &"pending"
        } else {
            PAST.iter().find(|d| d.starts_with(step)).unwrap_or(&step)
        };
        let issue = head.split(' ').find(|n| n.starts_with('#'));
        match w.next().filter(|c| ["REFUSED", "exit", "error"].contains(c)) {
            Some(c) => skip.extend(if c == "REFUSED" { w.next() } else { Some(c) }),
            None => by.entry(past).or_default().extend(issue),
        }
    }
    let n = skip.len();
    skip.sort_unstable();
    skip.dedup();
    let mut parts: Vec<String> = by.iter().map(|(v, ns)| format!("{v} {}", ns.join(", "))).collect();
    parts.extend((n > 0).then(|| format!("{n} skipped ({})", skip.join(", "))));
    parts.extend(parts.is_empty().then(|| "nothing due".into()));
    format!("{fleet}: {}", parts.join(" · ").replace("  ", " ").trim_end())
}
const PAST: [&str; 3] = ["started", "merged", "reviewed"];

thread_local! {
    static ACTS: RefCell<BTreeMap<String, Vec<Value>>> = const { RefCell::new(BTreeMap::new()) };
    static KICK: Cell<Option<u64>> = const { Cell::new(None) };
    static IN_ACT: Cell<bool> = const { Cell::new(false) };
    static ROOM: RefCell<(bool, Option<(&'static str, String)>)> = const { RefCell::new((false, None)) };
    static SLOT: Cell<bool> = const { Cell::new(false) };
}

/// #877: a pass's own command runs (its `clean` leaves disabled hosts alone).
pub fn in_act() -> bool {
    IN_ACT.get()
}

/// #304: `drover kick`: drive `issue`'s due steps now in a detached `kick --now` (#389: none from a lane on another host), output to `kick.log`; a no-op inside a pass's own command. #333: the detached kick's pid; #340: in its own session.
pub fn kick(ctx: &Ctx, r: &dyn Runner, fleet: &str, issue: u64, why: &str) -> Option<u32> {
    kick_once(ctx, r, (fleet, issue), why, None)
}

/// #509: `kick`, carrying its trigger's `id` (a verdict's sha, a decision): the same trigger is driven once.
pub fn kick_once(ctx: &Ctx, r: &dyn Runner, (fleet, issue): (&str, u64), why: &str, id: Option<&str>) -> Option<u32> {
    let mut a = crate::runner::argv(&["kick", fleet, &issue.to_string(), "--why", why]);
    a.extend(id.into_iter().flat_map(|i| ["--id".to_string(), i.into()]));
    if IN_ACT.get() || crate::outbox::lane().is_some() || crate::cli::forward(r, &a).is_some() {
        return None;
    }
    if let Some(pid) = crate::service::kick(ctx, fleet, issue, why, id) {
        return Some(pid);
    }
    a.insert(5, "--now".into());
    r.detach(&ctx.exe, &a, &state_dir(ctx).join("kick.log")).ok()
}

/// #304: `kick --now`: one pass of `issue` alone under the drive lock. A kick while one runs, or within the 10 s after it, is coalesced into one follow-up; a busy lock is retried as one too.
pub fn kicked(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, k: Kicked) -> Result<()> {
    let (fleet, n, why, now, id) = k;
    if !now {
        cfg.fleet(fleet)?;
        kick_once(ctx, r, (fleet, n), why, id);
        return Ok(());
    }
    if let Some(id) = id {
        if !crate::db::processed(&ctx.state, &format!("{fleet}#{n}:{why}:{id}"), ctx.now)? {
            return Ok(writeln!(out, "kick {why} {id}: already processed")?);
        }
    }
    let (again, ttl) = (format!("kick-{fleet}-{n}.again"), cfg.limits.spawn_pending_max_secs);
    let pending = (why == "drive").then(|| held(ctx, &format!("spawn-{fleet}-{n}.pending")));
    let mut pending = pending.flatten().map(|l| Lock(ctx.state.clone(), l));
    let flag = || crate::db::take(&ctx.state, &again, ctx.now, ttl, why, &|_| true).map(drop);
    let Some(k) = take(ctx, r, &format!("kick-{fleet}-{n}.lock"), ttl, fleet) else {
        flag();
        return Ok(());
    };
    let start = crate::now();
    for _ in 0..30 {
        let ctx = &Ctx {
            now: ctx.now + crate::now().saturating_sub(start),
            ..ctx.clone()
        };
        let expires_at = ctx.now + ttl;
        crate::db::renew(
            &ctx.state,
            &k.1,
            &crate::db::Lease {
                expires_at,
                ..k.1.clone()
            },
        );
        let mut buf = vec![];
        KICK.set(Some(n));
        SLOT.set(pending.is_some());
        let res = drive(ctx, cfg, r, &mut buf, Some(fleet), false, false);
        KICK.set(None);
        SLOT.set(false);
        if pending.take().map(drop).is_some() {
            update(ctx, r, out, fleet, n)?;
        }
        let text = String::from_utf8_lossy(&buf).into_owned();
        out.write_all(&buf)?;
        if text.starts_with("drive: ") {
            flag();
        } else {
            let d = json!({"why": why, "result": summary(fleet, &text)});
            append(&ctx.state, &Event::new(ctx.now, fleet, Some(n), "kick", d))?;
        }
        res?;
        r.sleep(10);
        if !crate::db::release(&ctx.state, &again, None) {
            break;
        }
    }
    Ok(())
}

/// `kick --now`'s fleet, issue, why, `--now` and trigger id.
pub type Kicked<'a> = (&'a str, u64, &'a str, bool, Option<&'a str>);

fn launch(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    ev: &[Event],
    (fleet, n): (&str, u64),
    c: &[&str],
    start: bool,
) -> Result<i32> {
    let role = c[3];
    markers(ctx, cfg, r, true);
    let live = pending(ctx, cfg, r);
    let low =
        |e: &Event| crate::host::free_gb(&crate::hosts::On::of(r, Some(e))).is_some_and(|g| g < cfg.limits.min_free_gb);
    let bound = |p: &Event| {
        ev.iter()
            .rev()
            .find(|e| p.issue.is_some_and(|n| e.is("spawn", &p.fleet, n)))
    };
    let n_live = live.iter().filter(|p| !bound(p).is_some_and(low)).count();
    let max = cfg.limits.max_parallel_spawns;
    let mine = live
        .iter()
        .filter(|p| p.fleet == fleet && !bound(p).is_some_and(low))
        .count();
    let extend = crate::pending::extending(ev, fleet, n);
    if live.iter().any(|e| e.fleet == fleet && e.issue == Some(n))
        || !extend
            && (n_live >= max - usize::from(start && max > 1)
                || (mine >= max.div_ceil(2) && waits_elsewhere(ctx, cfg, ev, fleet)))
    {
        let by = Some(held_by(ctx, cfg, r)).filter(|b| !b.is_empty());
        let by = by.map_or(String::new(), |b| format!(": {b}"));
        return crate::gate::refused("SPAWN_PENDING", format!("{n_live} spawns starting{by}"));
    }
    crate::spawn::lane_admit(ctx, (cfg, r), ev, fleet, n, role)?;
    if let Some(why) = crate::hosts::slot_before(ctx, cfg, r, ev, (fleet, n, role), &live) {
        return crate::gate::refused("HOST_FULL", why);
    }
    if let Some((code, why)) = room(ctx, cfg, r).filter(|x| !(x.0 == "HOST_BUSY" && extend)) {
        return crate::gate::refused(code, why);
    }
    let mut d = json!({"role": role, "check": cfg.fleet(fleet)?.flow().is_check(role)});
    let (key, ttl) = (format!("spawn-{fleet}-{n}.pending"), cfg.limits.spawn_pending_max_secs);
    let ev = |d: &Value| serde_json::to_string(&Event::new(ctx.now, fleet, Some(n), "spawn", d.clone()));
    let l = crate::db::take(&ctx.state, &key, ctx.now, ttl, &ev(&d)?, &|_| true);
    let l = l.ok_or_else(|| anyhow::anyhow!("cannot lease {key}"))?;
    let pid = kick(ctx, r, fleet, n, "drive");
    d["pid"] = pid.into();
    let data = if pid.is_some() { ev(&d)? } else { l.data.clone() };
    let pid = pid.unwrap_or(0);
    crate::db::renew(&ctx.state, &l, &crate::db::Lease { pid, data, ..l.clone() });
    Ok(0)
}

fn waits_elsewhere(ctx: &Ctx, cfg: &Config, ev: &[Event], fleet: &str) -> bool {
    let others = cfg.fleet.iter().filter(|(g, f)| *g != fleet && !f.paused);
    others.map(|(g, _)| g.as_str()).any(|g| {
        let start = |n: &String| n.parse().is_ok_and(|n| backoff(ctx, ev, g, n).is_none());
        let pass = |e: &&Event| e.kind == "drive-pass" && e.fleet == g && e.data.get("kick").is_none_or(Value::is_null);
        let held = ev
            .iter()
            .rfind(pass)
            .and_then(|e| e.data.get("actions")?.as_array().cloned());
        queue::startable(ev, g).iter().any(start) || held.into_iter().flatten().any(|a| a["code"] == "SPAWN_PENDING")
    })
}

pub(crate) fn failures<'a>(ev: &'a [Event], fleet: &str, n: u64) -> Vec<&'a Event> {
    let mine = ev.iter().rev().filter(|e| e.fleet == fleet && e.issue == Some(n));
    let mut v: Vec<&Event> = mine
        .take_while(|e| e.kind != "spawn")
        .filter(|e| e.kind == "spawn-error")
        .collect();
    v.reverse();
    v
}

pub(crate) fn same_run<'a>(f: &[&'a Event]) -> Option<(usize, &'a str)> {
    let last = f.last()?;
    let same = |e: &&&Event| e.str("error") == last.str("error") && e.str("head") == last.str("head");
    Some((f.iter().rev().take_while(same).count(), last.str("error")?))
}

/// #724: a spawn whose last attempt failed with the same error as the one before waits before its next try: 5 min, doubling to 1 h. A first or changed error, or a new push, retries at once (a kick never waits). (secs left, error).
pub fn backoff(ctx: &Ctx, ev: &[Event], fleet: &str, n: u64) -> Option<(u64, String)> {
    let f = failures(ev, fleet, n);
    let (k, error) = same_run(&f)?;
    let last = f.last()?;
    let pushed = gh::cached_pr(ctx, fleet, n)["headRefOid"].as_str() != last.str("head");
    let wait = (300u64 << k.checked_sub(2)?.min(4)).min(3600);
    let left = (last.ts + wait).saturating_sub(ctx.now);
    (!pushed && left > 0).then(|| (left, error.to_string()))
}

/// #724: `retrying in Nm: <error>` while `fleet` #`n` backs off.
pub fn retrying(ctx: &Ctx, ev: &[Event], fleet: &str, n: u64) -> Option<String> {
    backoff(ctx, ev, fleet, n).map(|(left, e)| format!("retrying in {}m: {e}", left.div_ceil(60)))
}

/// #724: passes run (one started within 10 min) and `fleet` #`n`'s last try waited only for a spawn slot: the line naming how many failing retries hold them.
pub fn slot_wait(ctx: &Ctx, ev: &[Event], fleet: &str, n: u64) -> Option<String> {
    let at = std::fs::read_to_string(ctx.state.with_file_name("drive.at")).ok()?;
    (ctx.now < at.split_whitespace().next()?.parse::<u64>().ok()? + 600).then_some(())?;
    let pass = |e: &&Event| e.kind == "drive-pass" && e.fleet == fleet;
    let acts = ev.iter().filter(pass).filter_map(|e| e.data.get("actions")?.as_array());
    let last = acts.flatten().rfind(|a| a["issue"] == n)?;
    (last["code"] == "SPAWN_PENDING").then_some(())?;
    let ids: std::collections::BTreeSet<_> = ev.iter().filter_map(|e| Some((&e.fleet, e.issue?))).collect();
    let failing = |(f, i): &(&String, u64)| failures(ev, f, *i).last().is_some_and(|e| ctx.now < e.ts + 3600);
    let k = ids.iter().filter(|x| failing(x)).count();
    Some(crate::promise::Say::SpawnSlot(None).line() + &format!(" ({k} failing retries hold them)"))
}

fn room(ctx: &Ctx, cfg: &Config, r: &dyn Runner) -> Option<(&'static str, String)> {
    if cfg.hosts.values().any(|h| h.enabled && h.machine.is_some()) {
        return None;
    }
    ROOM.with_borrow_mut(|s| {
        if !s.0 {
            *s = (true, crate::hosts::gate(ctx, cfg, &crate::hosts::On::new(r, None)).1);
        }
        s.1.clone()
    })
}

const SETTLE_SECS: u32 = 20;

fn launched(acts: &[Value]) -> usize {
    acts.iter().filter(|a| a["spawned"] == true).count()
}

fn settle(ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let key = |f: &str, a: &Value| format!("spawn-{f}-{}.pending", a["issue"]);
    let open = |all: bool| {
        let live = |(f, a): (&String, &Value)| a["outcome"] == "pending" && (all || held(ctx, &key(f, a)).is_some());
        ACTS.with_borrow(|m| m.iter().flat_map(|(f, v)| v.iter().map(move |a| (f, a))).any(live))
    };
    if !open(true) {
        return Ok(());
    }
    for _ in 0..SETTLE_SECS {
        if !open(false) {
            break;
        }
        r.sleep(1);
    }
    let ev = working(&ctx.state)?;
    ACTS.with_borrow_mut(|m| {
        for (f, acts) in m.iter_mut() {
            for a in acts.iter_mut().filter(|a| a["outcome"] == "pending") {
                if held(ctx, &key(f, a)).is_none() {
                    ended(&ev, f, a, ctx.now);
                }
                writeln!(out, "{}", line(f, a))?;
            }
        }
        Ok(())
    })
}

fn ended(ev: &[Event], fleet: &str, a: &mut Value, since: u64) {
    let Some(n) = a["issue"].as_u64() else { return };
    let step = a["step"].as_str().unwrap_or_default().to_string();
    let after = |e: &&Event| e.ts >= since && e.fleet == fleet;
    if ev.iter().filter(after).any(|e| e.is("spawn", fleet, n)) {
        let past = PAST.iter().find(|d| d.starts_with(&step)).map_or(&*step, |d| d);
        (a["outcome"], a["words"], a["spawned"]) = (past.into(), past.into(), true.into());
        return;
    }
    let pass = |e: &&Event| e.kind == "drive-pass" && e.data.get("kick") == Some(&json!(n));
    let mine = ev
        .iter()
        .filter(after)
        .rfind(pass)
        .and_then(|e| e.data.get("actions")?.as_array());
    if let Some(k) = mine.and_then(|v| v.iter().rfind(|k| k["issue"] == n)) {
        for f in ["outcome", "code", "words", "why"] {
            a[f] = k[f].clone();
        }
    } else {
        (a["outcome"], a["words"]) = ("waiting".into(), "its kick started nothing".into());
    }
}

fn update(ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write, fleet: &str, n: u64) -> Result<()> {
    for _ in 0..120 {
        if held(ctx, "drive.lock").is_none() {
            break;
        }
        r.sleep(1);
    }
    let ev = working(&ctx.state)?;
    let theirs =
        |e: &&Event| e.kind == "drive-pass" && e.fleet == fleet && e.data.get("kick").is_none_or(Value::is_null);
    let Some(e) = ev.iter().rfind(theirs) else {
        return Ok(());
    };
    let mut d = Value::Object(e.data.clone());
    let acts = d["actions"].as_array_mut();
    let Some(a) = acts.and_then(|v| v.iter_mut().rfind(|a| a["issue"] == n && a["outcome"] == "pending")) else {
        return Ok(());
    };
    ended(&ev, fleet, a, e.ts);
    writeln!(out, "{}", line(fleet, a))?;
    d["spawns_launched"] = launched(d["actions"].as_array().map_or(&[], Vec::as_slice)).into();
    d["updated"] = ctx.now.into();
    append(&ctx.state, &Event::new(e.ts, fleet, None, "drive-pass", d))
}

pub(crate) fn pending(ctx: &Ctx, cfg: &Config, r: &dyn Runner) -> Vec<Event> {
    let live = starting(ctx, cfg, r).into_iter();
    live.filter_map(|l| serde_json::from_str(&l.data).ok()).collect()
}

pub(crate) fn words(code: Option<&str>, step: &str, result: &str) -> String {
    let why = result.split_once(": ").map_or(result, |p| p.1);
    let why = why.split("; retry later").next().unwrap_or(why);
    match code {
        None => PAST.iter().find(|d| d.starts_with(step)).map_or(step, |d| d).into(),
        Some("HOST_BUSY") => format!("Mac busy ({why})"),
        Some("DISK_LOW") => format!("disk nearly full ({why})"),
        Some("CAP" | "ALL_CAPPED") => "lane limit or usage cap reached".into(),
        Some("CI_NOT_GREEN") => "waiting for CI".into(),
        Some("SPAWN_PENDING") => match why.split_once(": ") {
            Some((_, by)) => crate::promise::Say::SpawnSlot(Some(by.into())).line(),
            None => crate::promise::Say::SpawnSlot(None).line(),
        },
        Some("SPAWN_BACKOFF" | "HOST_FULL") => why.into(),
        Some("SERIAL_BUSY") => "another one-at-a-time issue is in flight".into(),
        Some("IN_FLIGHT" | "ALREADY_DONE") => "skipped: another pass is on it or did it".into(),
        Some("LANE_LIVE") if why.starts_with("stale pane") => why.into(),
        Some("LANE_LIVE") => match why.strip_prefix("finished (").and_then(|w| w.split_once(')')) {
            Some((st, _)) => format!(
                "an old {} ({st}) is in the way: replacing it",
                if step == "review" { "reviewer" } else { "lane" }
            ),
            None => "a lane is already on it".into(),
        },
        Some("ROUND_LIMIT" | "UPDATE_LIMIT") => "out of rounds: needs you".into(),
        Some("EXIT" | "ERROR") => format!("failed: {result}"),
        Some(c) => format!("held back ({c})"),
    }
}

/// #166: the fleet's result line from a `drive-pass`: (tone, `HH:MM · started N · merged N · M waiting: <top reason>`). Red on an error, green if it started or merged something, amber if everything was held back, else plain.
pub fn result(e: &Event) -> (&'static str, String) {
    let acts = e
        .data
        .get("actions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let n = |o: &str| acts.iter().filter(|a| a["outcome"] == o).count();
    let held: Vec<&str> = acts
        .iter()
        .filter(|a| a["outcome"] == "waiting")
        .filter_map(|a| a["words"].as_str())
        .collect();
    let top = held.iter().rev().max_by_key(|w| held.iter().filter(|x| x == w).count());
    let (started, merged, failed) = (n("started"), n("merged"), n("error"));
    let mut line = format!("{} · started {started} · merged {merged}", e.str("at").unwrap_or("?"));
    line += &top.map_or(String::new(), |w| format!(" · {} waiting: {w}", held.len()));
    if failed > 0 {
        line += &format!(" · {failed} failed");
    }
    let tone = match () {
        () if failed > 0 => "r",
        () if started + merged > 0 => "g",
        () if !held.is_empty() => "a",
        () => "m",
    };
    (tone, line)
}

/// #166: a pass is driving `fleet` now (the lock names the fleet a manual press drives; empty: every fleet).
pub fn running(ctx: &Ctx, fleet: &str) -> bool {
    held(ctx, "drive.lock").is_some_and(|l| l.data.is_empty() || fleet.is_empty() || l.data == fleet)
}

fn line(fleet: &str, a: &Value) -> String {
    let who = a["issue"]
        .as_u64()
        .map_or(fleet.to_string(), |n| format!("{fleet} #{n}"));
    let (step, w) = (
        a["step"].as_str().unwrap_or_default(),
        a["words"].as_str().unwrap_or_default(),
    );
    match a["code"].as_str() {
        None => format!("{who} {step}: {w}"),
        Some("EXIT" | "ERROR") => format!("{who} {step}: error: {w}"),
        Some(c) => format!("{who} {step}: REFUSED {c}: {}", a["why"].as_str().unwrap_or(w)),
    }
}

fn wait(ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write, lock: &str, only: Option<&str>) -> Result<()> {
    for _ in 0..120 {
        if held(ctx, lock).is_none() {
            break;
        }
        r.sleep(1);
    }
    if held(ctx, lock).is_some() {
        return Ok(writeln!(out, "drive: still driving · result will appear here")?);
    }
    let events = working(&ctx.state)?;
    let done = |e: &&Event| e.kind == "drive-pass" && only.is_none_or(|f| e.fleet == f);
    let ended = |e: &&Event| e.data.get("ended").and_then(Value::as_u64) >= Some(ctx.now);
    let Some(e) = events.iter().rev().filter(done).find(ended) else {
        return Ok(writeln!(out, "drive: the other pass ended without driving it")?);
    };
    let acts = e
        .data
        .get("actions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    acts.iter().try_for_each(|a| writeln!(out, "{}", line(&e.fleet, a)))?;
    Ok(())
}

/// One pass over every fleet (or `only`), under `<state dir>/drive.lock`; `manual`: a press. #212 `fast`: verdicts and merges only, PRs read afresh, under `drive-fast.lock`, so a slow pass starting lanes never holds it up.
pub fn drive(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    only: Option<&str>,
    manual: bool,
    fast: bool,
) -> Result<()> {
    let t0 = std::time::Instant::now();
    let mut slow = (t0, String::new(), 0.0);
    let i = usize::from(fast);
    let lock = ["drive.lock", "drive-fast.lock"][i];
    let mark = ctx.state.with_file_name(format!("{lock}.step"));
    let mut lap = |step: &str| {
        debug_assert!(STEPS.iter().any(|(s, _)| step.ends_with(s)), "{step} is not in STEPS");
        if KICK.get().is_none() {
            let _ = std::fs::write(&mark, step);
        }
        let secs = slow.0.elapsed().as_secs_f64();
        if secs > slow.2 {
            (slow.1, slow.2) = (step.to_string(), secs);
        }
        slow.0 = std::time::Instant::now();
    };
    only.map(|f| cfg.fleet(f)).transpose()?;
    std::fs::create_dir_all(state_dir(ctx))?;
    gh::FORCE.set(fast || KICK.get().is_some());
    gh::CATCHUP.set(if fast { cfg.limits.webhook_poll_minutes * 60 } else { 0 });
    let stamp = ctx.state.with_file_name(["drive.at", "drive-fast.at"][i]);
    let _ = std::fs::write(&stamp, ctx.now.to_string());
    let max = 3 * cfg.limits.pass_max_secs;
    let stuck = (crate::db::leases(&ctx.state, lock).into_iter())
        .find(|l| l.key == lock && l.expires_at <= ctx.now && !gone(r, l, ctx.now))
        .filter(|_| KICK.get().is_none());
    let mine = || take(ctx, r, lock, max, only.unwrap_or_default()).map(Some);
    let Some(_held) = KICK.get().map_or_else(mine, |_| Some(None)) else {
        if manual {
            return wait(ctx, r, out, lock, only);
        }
        return Ok(writeln!(out, "drive: another pass holds {lock}; nothing done")?);
    };
    if let Some(l) = stuck {
        let at = std::fs::read_to_string(&mark).unwrap_or_default();
        let d = json!({"lock": lock, "pid": l.pid, "since": l.at, "step": format!("after {}", at.trim())});
        writeln!(out, "! a pass held {lock} past {max} s, {}: taken over", d["step"])?;
        let _ = append(&ctx.state, &Event::new(ctx.now, "drover", None, "pass-stuck", d));
    }
    if KICK.get().is_none() {
        let _ = std::fs::write(&mark, "start");
    }
    if let Some((step, secs)) = crate::timer::stuck_step(ctx, cfg).filter(|_| fast) {
        writeln!(
            out,
            "! tick stuck in {step} for {}: drover doctor",
            crate::events::age(secs)
        )?;
    }
    if fast {
        if let Err(e) = crate::hosts::sample(ctx, cfg, r, &crate::events::working(&ctx.state)?) {
            writeln!(out, "host history: {e:#}")?;
        }
    }
    lap("read log, host sample");
    if let Some(b) = r.shed().filter(|_| !fast) {
        if let Err(e) = crate::shed::intake(ctx, cfg, &b.wire) {
            writeln!(out, "shed events: {e:#}")?;
        }
    }
    if only.is_none() {
        if let Err(e) = crate::check::budget(ctx, cfg, r, out) {
            writeln!(out, "budget: {e:#}")?;
        }
    }
    lap("disk budget");
    if only.is_none() && !fast {
        if let Err(e) = crate::roots::watch(ctx, cfg, r, out) {
            writeln!(out, "roots: {e:#}")?;
        }
        if let Err(e) = crate::hosts::evacuate(ctx, cfg, r, out) {
            writeln!(out, "offline hosts: {e:#}")?;
        }
    }
    let live: Vec<&str> = (cfg.fleet.keys().map(String::as_str))
        .filter(|n| only.is_none_or(|o| o == *n))
        .collect();
    let fleets: Vec<&str> = live.iter().copied().filter(|n| !cfg.fleet[*n].paused).collect();
    let events = fast
        .then(|| crate::events::working(&ctx.state).ok())
        .flatten()
        .unwrap_or_default();
    for u in live.iter().flat_map(|f| crate::undelivered(&events, f)) {
        if let Err(e) = crate::deliver(ctx, cfg, r, u, u.str("text").unwrap_or_default(), false) {
            writeln!(out, "redeliver: {e:#}")?;
        }
    }
    let (mut res, mut start) = (Ok(()), true);
    ACTS.with_borrow_mut(BTreeMap::clear);
    ROOM.set((false, None));
    let mut fail = |name: &str, e: anyhow::Error| {
        let w = format!("failed: {e:#}");
        let a = json!({"step": "pass", "outcome": "error", "code": "ERROR", "words": w});
        ACTS.with_borrow_mut(|m| m.entry(name.into()).or_default().push(a));
        res = Err(e);
    };
    lap("roots, redeliver");
    let log = || crate::db::last_seq(&ctx.state);
    let mut seen = if fleets.is_empty() && (!fast || live.is_empty()) {
        crate::snapshot::Snap::read(ctx, r)
    } else {
        crate::snapshot::take(ctx, cfg, r)
    };
    if !fleets.is_empty() && KICK.get().is_none() {
        match crate::spawn::orphans(ctx, cfg, (r, &seen), out) {
            Ok(true) => seen = crate::snapshot::take(ctx, cfg, r),
            Ok(false) => {}
            Err(e) => writeln!(out, "orphans: {e:#}")?,
        }
    }
    lap("look, orphans");
    if !fast {
        if let Err(e) = repair(ctx, cfg, (r, &seen), out) {
            writeln!(out, "repair: {e:#}")?;
        }
    }
    if !fast && KICK.get().is_none() && !fleets.is_empty() {
        crate::breaker::probe(ctx, cfg, r, out).unwrap_or_else(|e| drop(writeln!(out, "runtime canary: {e:#}")));
    }
    let grew = log();
    if fast {
        crate::outbox::drain(ctx, (r, &seen), out, &live).unwrap_or_else(|e| drop(writeln!(out, "outbox: {e:#}")));
    }
    let mut due = vec![];
    lap("repair, outbox");
    for name in &fleets {
        match pass(ctx, cfg, (r, &seen), out, name, fast) {
            Ok(go) => {
                start &= go;
                due.push(*name);
            }
            Err(e) => fail(name, e),
        }
        lap(&format!("{name}: PR reads, gates, steps"));
    }
    if !fast {
        for (name, e) in starts(ctx, cfg, r, out, &due, start) {
            fail(name, e);
        }
    } else if only.is_none() && KICK.get().is_none() {
        crate::slo::pass(ctx, cfg, (r, &seen), out, &fleets).unwrap_or_else(|e| drop(writeln!(out, "slo: {e:#}")));
    }
    lap("spawns");
    if log() != grew {
        crate::snapshot::take(ctx, cfg, r);
    }
    let secs = (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0;
    if fast {
        let _ = std::fs::write(&stamp, format!("{} {secs}", ctx.now));
    }
    settle(ctx, r, out)?;
    for &name in &fleets {
        let acts = ACTS.with_borrow_mut(|m| m.remove(name)).unwrap_or_default();
        if !fast && KICK.get().is_none() {
            let _ = std::fs::write(state_dir(ctx).join(format!("drive-{name}.last")), ctx.now.to_string());
        } else if acts.is_empty() {
            continue;
        }
        let end = crate::now();
        let (by, at) = (
            ["auto", "manual", "fast"][usize::from(manual) + 2 * usize::from(fast)],
            crate::usage::date(r, end, "+%H:%M"),
        );
        let d = json!({"by": by, "started": ctx.now, "ended": end, "at": at, "secs": secs,
            "spawns_launched": launched(&acts), "actions": acts, "kick": KICK.get()});
        let _ = append(&ctx.state, &Event::new(ctx.now, name, None, "drive-pass", d));
    }
    if !fast && KICK.get().is_none() {
        for n in &fleets {
            crate::gh::backlog(ctx, r, cfg.fleet(n)?, n).unwrap_or_else(|e| eprintln!("drover: {n} backlog: {e:#}"));
        }
        crate::check::stale(ctx, cfg, r, out).unwrap_or_else(|e| eprintln!("drover: stale panes: {e:#}"));
        crate::check::dedup(ctx, cfg, r, out).unwrap_or_else(|e| eprintln!("drover: lane-dup: {e:#}"));
        let max = cfg.limits.rotate_mb << 20;
        let cp = crate::events::checkpoint(&ctx.state).and_then(|_| crate::events::rotate(&ctx.state, max, ctx.now));
        cp.unwrap_or_else(|e| eprintln!("drover: checkpoint: {e:#}"));
    }
    lap("backlog, checkpoint");
    let secs = t0.elapsed().as_secs_f64();
    if t0.elapsed().as_secs() > cfg.limits.pass_max_secs {
        let r1 = |x: f64| (x * 10.0).round() / 10.0;
        let d = json!({"fast": fast, "secs": r1(secs), "step": slow.1, "step_secs": r1(slow.2), "kick": KICK.get()});
        let _ = append(&ctx.state, &Event::new(ctx.now, "drover", None, "pass-slow", d));
    }
    res
}

fn repair(ctx: &Ctx, cfg: &Config, (r, seen): (&dyn Runner, &dyn Runner), out: &mut dyn Write) -> Result<()> {
    let events = working(&ctx.state)?;
    let log = crate::waiting::Log::new(&events);
    let mut agents: Option<Vec<Value>> = None;
    for s in fold(&events).into_values().filter(|s| cfg.fleet.contains_key(&s.fleet)) {
        let on = log.blockers(&s.fleet, s.issue);
        if on.is_empty() || !gh::cached_pr(ctx, &s.fleet, s.issue)["number"].is_null() {
            continue;
        }
        if !(matches!(s.state.as_str(), "needs-human" | "impl") || s.state.starts_with("fix")) {
            continue;
        }
        if s.state == "impl" {
            let live = match &mut agents {
                Some(a) => a,
                None => agents.insert(crate::hosts::all_agents(cfg, seen)?),
            };
            let lane = s
                .active_lane()
                .is_some_and(|(_, l)| live.iter().any(|a| a["name"] == l));
            if lane
                || pending(ctx, cfg, r)
                    .iter()
                    .any(|p| p.fleet == s.fleet && p.issue == Some(s.issue))
            {
                continue;
            }
        }
        let why = format!("blocked by {on}");
        writeln!(out, "{} #{} repaired: back to queued ({why})", s.fleet, s.issue)?;
        let d = json!({"state": "queued", "reason": why});
        emit(
            &ctx.state,
            cfg,
            Event::new(ctx.now, &s.fleet, Some(s.issue), "repaired", d),
        )?;
    }
    let events = working(&ctx.state)?;
    let reset = ["pushed", "verdict", "needs-human", "round-extended"];
    for s in fold(&events)
        .into_values()
        .filter(|s| s.state == "needs-human" && cfg.fleet.contains_key(&s.fleet))
    {
        let mine: Vec<(usize, &Event)> = (events.iter().enumerate())
            .filter(|(_, e)| e.fleet == s.fleet && e.issue == Some(s.issue))
            .collect();
        let Some(at) = mine.iter().rposition(|(_, e)| e.kind == "needs-human") else {
            continue;
        };
        if s.build.as_deref() == Some("lesson") && mine[at].1.str("code") == Some("LANE_ENDS") {
            crate::lesson::done(ctx, cfg, r, out, (&s.fleet, s.issue), "no repeated class")?;
            continue;
        }
        let from = mine[..at].iter().rposition(|(_, e)| reset.contains(&e.kind.as_str()));
        let round = &mine[from.map_or(0, |i| i + 1)..at];
        let exited = |e: &Event| e.kind == "lane-state" && e.str("state") == Some("gone");
        let trimmed = round
            .windows(2)
            .any(|w| w[0].1.kind == "lane-trimmed" && exited(w[1].1));
        let again = mine
            .iter()
            .any(|(_, e)| e.kind == "repaired" && e.val("trimmed") == true);
        if mine[at].1.str("code") == Some("LANE_ENDS") && trimmed && !again {
            let Some(was) = fold(&events[..mine[at].0])
                .remove(&(s.fleet.clone(), s.issue))
                .map(|b| b.state)
            else {
                continue;
            };
            writeln!(
                out,
                "{} #{} repaired: back to {was} (its lane ended after an idle trim)",
                s.fleet, s.issue
            )?;
            let why = "the lane ended after an idle trim killed its agent: one respawn in its worktree";
            let d = json!({"code": "LANE_ENDS", "state": was, "reason": why, "trimmed": true});
            emit(
                &ctx.state,
                cfg,
                Event::new(ctx.now, &s.fleet, Some(s.issue), "repaired", d),
            )?;
            continue;
        }
        let adopted = |e: &Event| crate::events::design(e) && e.data.get("adopted") == Some(&json!(true));
        let Some(first) = round.iter().position(|(_, e)| e.kind == "respawn") else {
            continue;
        };
        let gone = (round.iter())
            .all(|(_, e)| e.kind != "respawn" || e.str("why").is_some_and(|w| w.starts_with("lane gone")));
        if mine[at].1.str("code") != Some("LANE_ENDS") || !gone || !round[..first].iter().any(|(_, e)| adopted(e)) {
            continue;
        }
        let key = (s.fleet.clone(), s.issue);
        let Some(was) = fold(&events[..mine[at].0]).remove(&key).map(|b| b.state) else {
            continue;
        };
        writeln!(
            out,
            "{} #{} repaired: back to {was} (LANE_ENDS came from a design lane's adoption)",
            s.fleet, s.issue
        )?;
        let why = "the lane gone came from a design lane's adoption, not the build round";
        let d = json!({"code": "LANE_ENDS", "state": was, "reason": why});
        emit(
            &ctx.state,
            cfg,
            Event::new(ctx.now, &s.fleet, Some(s.issue), "repaired", d),
        )?;
    }
    Ok(())
}

fn save(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    f: &crate::config::Fleet,
    (events, n, lane): (&[Event], u64, &str),
    why: &str,
) -> Result<()> {
    let sp = crate::events::last_spawn(events, lane);
    let (Some(wt), Some(branch)) = (sp.and_then(|e| e.str("worktree")), sp.and_then(|e| e.str("branch"))) else {
        return Ok(());
    };
    if [f.integration.as_str(), "main", "master"].contains(&branch) {
        return Ok(());
    }
    let on = crate::hosts::On::of(r, sp);
    let to = format!("HEAD:refs/heads/{branch}");
    let o = on.run("git", &crate::runner::argv(&["-C", wt, "push", "origin", &to]))?;
    let fleet = sp.map_or("", |e| e.fleet.as_str());
    if o.code != 0 {
        return Ok(writeln!(
            out,
            "{fleet} #{n} push of {lane}'s commits failed: {}",
            o.stderr.trim()
        )?);
    }
    if o.stderr.contains("up-to-date") {
        return Ok(());
    }
    writeln!(out, "{fleet} #{n} pushed {lane}'s local commits to {branch} ({why})")?;
    let d = json!({"agent": lane, "branch": branch, "why": why});
    emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(n), "lane-pushed", d))
}

fn silent(
    ctx: &Ctx,
    cfg: &Config,
    (r, seen): (&dyn Runner, &dyn Runner),
    out: &mut dyn Write,
    agents: &[Value],
    fleet: &str,
) -> Result<()> {
    let ev = working(&ctx.state)?;
    let found = stall::silent(ctx, cfg, &ev, agents, fleet);
    for (n, lane, a) in found.into_iter().filter(|x| KICK.get().is_none_or(|k| k == x.0)) {
        match a {
            stall::Silent::Nudge(why) => stall::nudge(ctx, r, out, &ev, (fleet, n), &lane, &why)?,
            stall::Silent::Respawn(role, why) => {
                let num = n.to_string();
                let args = ["spawn", fleet, &num, &role, "--supersede"];
                if act(ctx, cfg, r, out, &ev, (fleet, Some(n), "respawn"), &args)?.is_none() {
                    let d = json!({"role": role, "agent": lane, "why": why});
                    append(&ctx.state, &Event::new(ctx.now, fleet, Some(n), "respawned-silent", d))?;
                }
            }
            stall::Silent::Escalate(why) => escalate(ctx, cfg, out, (fleet, n), (&lane, None), "LANE_SILENT", &why)?,
        }
    }
    let reviews = fold(&ev)
        .into_values()
        .filter(|s| s.fleet == fleet && s.state.starts_with("review"));
    for s in reviews.filter(|s| KICK.get().is_none_or(|k| k == s.issue)) {
        let Some((role, old)) = s.active_lane() else { continue };
        if !agents
            .iter()
            .any(|a| a["name"] == old && a["agent_status"] == "working")
        {
            continue;
        }
        let Some(mins) = stall::stale_check(ctx, seen, &ev, agents, (fleet, s.issue), old) else {
            continue;
        };
        let (n, step) = (s.issue.to_string(), (fleet, Some(s.issue), "review"));
        if act(ctx, cfg, r, out, &ev, step, &["spawn", fleet, &n, role, "--supersede"])?.is_none() {
            stale_replaced(ctx, cfg, out, (fleet, s.issue), role, old, mins)?;
        }
    }
    Ok(())
}

fn stale_replaced(
    ctx: &Ctx,
    cfg: &Config,
    out: &mut dyn Write,
    (fleet, n): (&str, u64),
    role: &str,
    old: &str,
    mins: u64,
) -> Result<()> {
    let ev = working(&ctx.state)?;
    let new = |e: &&Event| e.is("spawn", fleet, n) && e.str("role") == Some(role) && e.ts >= ctx.now;
    let new = ev
        .iter()
        .rfind(new)
        .and_then(|e| e.str("agent"))
        .unwrap_or("a lane still starting");
    let why = format!("{old} quiet {mins}m with no verdict: replaced by {new}");
    writeln!(out, "{fleet} #{n} {why}")?;
    let d = json!({"agent": old, "role": role, "idle_minutes": mins, "new": new, "reason": why});
    emit(
        &ctx.state,
        cfg,
        Event::new(ctx.now, fleet, Some(n), "rev-stale-replaced", d),
    )
}

fn escalate(
    ctx: &Ctx,
    cfg: &Config,
    out: &mut dyn Write,
    (fleet, n): (&str, u64),
    (lane, on): (&str, Option<&str>),
    code: &str,
    why: &str,
) -> Result<()> {
    writeln!(out, "{fleet} #{n} needs-human: {why}")?;
    let d = json!({"code": code, "reason": why, "agent": lane, "failed_on": on});
    emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(n), "needs-human", d))
}

fn pass(
    ctx: &Ctx,
    cfg: &Config,
    (r, seen): (&dyn Runner, &dyn Runner),
    out: &mut dyn Write,
    fleet: &str,
    fast: bool,
) -> Result<bool> {
    let f = cfg.fleet(fleet)?;
    let agents = crate::hosts::all_agents(cfg, seen)?;
    let ev = working(&ctx.state)?;
    let st = fold(&ev);
    for s in st.values().filter(|s| s.fleet == fleet) {
        let on = crate::events::held_on(&ev, s);
        let open = |b: &u64| st.get(&(fleet.into(), *b)).is_none_or(|b| b.state != "merged");
        if on.is_empty() || on.iter().any(open) {
            continue;
        }
        let merged = ev.iter().filter(|e| e.kind == "merged" && e.fleet == fleet);
        let at = merged
            .filter(|e| e.issue.is_some_and(|b| on.contains(&b)))
            .map(|e| e.ts)
            .max();
        let outlived = ev
            .iter()
            .any(|e| e.kind == "drive-pass" && e.fleet == fleet && at.is_some_and(|t| e.ts > t));
        let mut episode = ev.iter().rev().take_while(|e| !e.is("held", fleet, s.issue));
        if outlived && !episode.any(|e| e.is("hold-stale", fleet, s.issue)) {
            let d = json!({"on": on, "merged_at": at});
            emit(
                &ctx.state,
                cfg,
                Event::new(ctx.now, fleet, Some(s.issue), "hold-stale", d),
            )?;
        }
        let Some((role, lane)) = s.active_lane() else {
            let e = Event::new(ctx.now, fleet, Some(s.issue), "released", json!({"on": on}));
            emit(&ctx.state, cfg, e)?;
            continue;
        };
        let sp = crate::events::last_spawn(&ev, lane);
        let live = agents.iter().any(|a| a["name"] == lane);
        let pane = sp.and_then(|e| e.str("pane")).filter(|_| live);
        let away = crate::hosts::offline(ctx, sp).is_some();
        if fast && !live || away {
            continue;
        }
        let text = format!(
            "{} merged; rebase on {} and continue",
            crate::queue::hashes(&on),
            f.integration
        );
        let (dsh, n) = (sp.and_then(|e| e.str("harness")) == Some("dsh"), s.issue.to_string());
        let c = ["spawn", fleet, &n, role, "--supersede"];
        let woke = match pane {
            Some(p) => crate::term::tell(&crate::hosts::On::of(r, sp), dsh, lane, p, &text).is_ok(),
            None => act(ctx, cfg, r, out, &ev, (fleet, None, "respawn"), &c)?.is_none(),
        };
        let d = json!({"agent": lane, "on": on, "respawn": !live});
        let e = Event::new(ctx.now, fleet, Some(s.issue), "released", d);
        if woke {
            emit(&ctx.state, cfg, e)?;
        }
    }
    for s in st.values().filter(|s| s.fleet == fleet && s.state == "design" && !fast) {
        crate::design::handoff(ctx, cfg, r, out, &agents, s)?;
    }
    if !fast {
        crate::stall::refused(ctx, cfg, r, out, &working(&ctx.state)?, fleet)?;
        silent(ctx, cfg, (r, seen), out, &agents, fleet)?;
        if let Err(e) = crate::hotspots::sync(ctx, cfg, r, fleet) {
            writeln!(out, "{fleet} hotspots: {e:#}")?;
        }
    }
    let events = working(&ctx.state)?;
    let one = |(s, _): &(IssueState, Waiting)| KICK.get().is_none_or(|k| k == s.issue);
    let mut waits = waiting::scan(ctx, cfg, seen, &events, &agents, true);
    waits.retain(one);
    crate::promise::pass(ctx, cfg, fleet, &events, &waits)?;
    external(ctx, cfg, r, out, fleet)?;
    ext_closed(ctx, cfg, r, out, fleet)?;
    if KICK.get().is_none() {
        if let Err(e) = crate::land::watch(ctx, cfg, r, out, fleet) {
            writeln!(out, "{fleet} land: {e:#}")?;
        }
    }
    if !fast && handoff(ctx, cfg, r, out, fleet, &events, &waits)? {
        waits = waiting::scan(ctx, cfg, seen, &working(&ctx.state)?, &agents, false);
        waits.retain(one);
    }
    let preview = || crate::overlap::preview(ctx, cfg, r, fleet, &events).ok();
    let conflicts = (!fast).then(preview).flatten().unwrap_or_default();
    let rev = |w: &Waiting| {
        w.next_cmd
            .as_deref()
            .and_then(|c| c.split(' ').nth(4))
            .is_some_and(|r| f.family(r) == "rev")
    };
    let verdict = |w: &Waiting| w.next_cmd.as_deref().is_some_and(|c| c.starts_with("drover verdict "));
    let merge = |w: &Waiting| w.next_cmd.as_deref().is_some_and(|c| c.starts_with("drover merge "));
    waits.sort_by_key(|(s, w)| (!verdict(w), !merge(w), !rev(w), conflicts.contains_key(&s.issue)));
    let (mut go, mut updated) = (true, false);
    for (s, w) in waits
        .iter()
        .filter(|(s, w)| s.fleet == fleet && s.lane.as_ref() == Some(&w.lane))
    {
        let lock = format!("drive-{fleet}-{}.lock", s.issue);
        let Some(_mine) = take(ctx, r, &lock, STALE_LOCK_SECS, fleet) else {
            continue;
        };
        let mut pr = gh::cached_pr(ctx, fleet, s.issue);
        crate::land::mark(ctx, Some(r), f, (fleet, s.issue), &events, &mut pr);
        let ci = gh::ci(&gh::required(&pr["statusCheckRollup"], f.checks(&pr)));
        let gone = gh::missing(ctx, None, cfg, (fleet, s.issue), &events, &pr).filter(|m| w.what.contains(m));
        let n = s.issue.to_string();
        if pr["state"] == "OPEN"
            && !queue::serials(&events, f, fleet).is_empty()
            && serial(ctx, cfg, r, out, (fleet, s), &pr, fast)?
        {
            continue;
        }
        if let Err(e) = crate::gate::sync_label(ctx, cfg, r, out, s, &pr) {
            writeln!(out, "{fleet} #{n} label: {e:#}")?;
        }
        if crate::gate::ready_draft(ctx, cfg, r, out, s, &pr)? && merge(w) {
            continue;
        }
        let now = |e: &Event| e.is("auto-updated", fleet, s.issue) && e.ts == ctx.now;
        if let Some(e) = crate::events::working(&ctx.state)?.iter().rfind(|e| now(e)).cloned() {
            let to = e.str("to").unwrap_or_default();
            writeln!(out, "{fleet} #{n} ready (CI pending at {to:.7})")?;
        } else if w.who == "drive" && w.what == waiting::LESSON_PROMPT && !fast {
            let Some(sent) = tell(r, &events, &w.lane, &crate::lesson::prompt(fleet, s.issue)) else {
                continue;
            };
            writeln!(
                out,
                "{fleet} #{n} lesson lane prompted: {}",
                if sent { "sent" } else { "not sent" }
            )?;
            let d = json!({"agent": w.lane, "sent": sent});
            append(
                &ctx.state,
                &Event::new(ctx.now, fleet, Some(s.issue), "lesson-prompted", d),
            )?;
        } else if w.who == "drive" && matches!(w.what.as_str(), waiting::TURN_NUDGE | waiting::TURN_GO_ON) && !fast {
            let (state, text) = if w.what == waiting::TURN_NUDGE {
                let text = "drover: you reported `done`, but nothing new is pushed at the PR head (or there is no \
                    PR). Push and open the PR, then `drover turn done`; or `drover turn blocked --why <reason>`.";
                ("done", text)
            } else {
                (
                    "working",
                    "drover: continue the task; end the turn with `drover turn …`.",
                )
            };
            let Some(sent) = tell(r, &events, &w.lane, text) else {
                continue;
            };
            writeln!(
                out,
                "{fleet} #{n} turn {state}: {}",
                if sent { "prompted" } else { "not sent" }
            )?;
            let d = json!({"agent": w.lane, "state": state, "sent": sent});
            append(
                &ctx.state,
                &Event::new(ctx.now, fleet, Some(s.issue), "turn-prompted", d),
            )?;
        } else if w.who == "drive" && w.what.starts_with(waiting::TURN_BLOCKED) && !fast {
            let why = &w.what[waiting::TURN_BLOCKED.len()..];
            writeln!(out, "{fleet} #{n} needs-human: lane blocked: {why}")?;
            let d = json!({"code": "LANE_BLOCKED", "reason": format!("lane blocked: {why}"), "agent": w.lane});
            emit(
                &ctx.state,
                cfg,
                Event::new(ctx.now, fleet, Some(s.issue), "needs-human", d),
            )?;
        } else if w.who == "drive" && w.what == waiting::LESSON_CLOSE && !fast {
            crate::lesson::done(ctx, cfg, r, out, (fleet, s.issue), "no repeated class")?;
        } else if w.who == "drive" {
            let gone = w.what.split_once(waiting::NO_PUSH);
            let ends =
                gone.filter(|g| (w.next_cmd.is_none() || g.1.starts_with("respawning")) && !waiting::resumed(g.0));
            if let Some(g) = ends.filter(|_| !fast) {
                if crate::breaker::failed(ctx, cfg, out, fleet, &w.lane, g.0)? {
                    continue;
                }
            }
            if gone.is_some() && w.next_cmd.is_none() && !fast {
                let why = "lane keeps ending without a push";
                writeln!(out, "{fleet} #{n} needs-human: {why}")?;
                let on = crate::breaker::blame(&events, &w.lane);
                let d = json!({"code": "LANE_ENDS", "reason": why, "agent": w.lane, "failed_on": on});
                emit(
                    &ctx.state,
                    cfg,
                    Event::new(ctx.now, fleet, Some(s.issue), "needs-human", d),
                )?;
            }
            if gone.is_some_and(|g| !g.1.starts_with("respawning")) {
                continue;
            }
            let Some(cmd) = &w.next_cmd else { continue };
            let args: Vec<&str> = cmd.split_whitespace().skip(1).collect();
            let step = match (args[0], args.get(3).copied()) {
                _ if gone.is_some() => "respawn",
                ("spawn", Some(r)) if f.flow().is_check(r) => "review",
                ("spawn", _) if cmd.ends_with("--update") => "conflict",
                ("spawn", _) if cmd.ends_with("--supersede") => "fix",
                ("spawn", _) => "respawn",
                (v, _) => v,
            };
            if fast && !matches!(step, "verdict" | "merge") {
                continue;
            }
            if step == "review" && args[3] == "verify" && !crate::verify::due(ctx, cfg, r, out, s, &pr)? {
                continue;
            }
            if step == "verdict" && args[3] == "FAIL" {
                if let Some(code) = no_proof(
                    ctx,
                    cfg,
                    r,
                    out,
                    (&events, &agents),
                    (fleet, s.issue, &w.lane),
                    &args,
                    fast,
                )? {
                    go &= !matches!(code, Some("HOST_BUSY" | "DISK_LOW" | "MEM_LOW"));
                    continue;
                }
            }
            let repro =
                (step == "verdict" && args[3] == "PASS").then(|| repro_seen(r, &events, (fleet, s.issue), &args));
            if repro.as_ref().is_some_and(|x| x.1) {
                continue;
            }
            if w.what.starts_with("reviewer exited") {
                let d = json!({"role": args.get(3), "why": "exited without a verdict"});
                append(&ctx.state, &Event::new(ctx.now, fleet, Some(s.issue), "lane-lost", d))?;
            }
            let cause = crate::shed::exit_cause(&events, &w.lane).filter(|c| crate::shed::RESUMABLE.contains(c));
            if let Some(why) = cause.filter(|_| args[0] == "spawn") {
                save(ctx, cfg, r, out, f, (&events, s.issue, &w.lane), why)?;
            }
            let mut code = act(ctx, cfg, r, out, &events, (fleet, Some(s.issue), step), &args)?;
            let stale = (code == Some("LANE_LIVE") && step == "review")
                .then(|| s.lanes.get(args[3]))
                .flatten()
                .and_then(|l| Some((l, stall::stale_check(ctx, r, &events, &agents, (fleet, s.issue), l)?)));
            if code == Some("LANE_LIVE") && args[0] == "spawn" && (last_words(fleet).contains(OLD) || stale.is_some()) {
                let a = [&args[..], &["--supersede"]].concat();
                code = act(ctx, cfg, r, out, &events, (fleet, Some(s.issue), step), &a)?;
            }
            if let Some((old, mins)) = stale.filter(|_| code.is_none()) {
                stale_replaced(ctx, cfg, out, (fleet, s.issue), args[3], old, mins)?;
            }
            if let Some((hash, _)) = repro.filter(|_| code == Some("NO_REPRO")) {
                repro_refused(ctx, cfg, r, (&events, &agents), (fleet, s.issue, &w.lane), &args, &hash)?;
            }
            if let Some((why, _)) = gone.filter(|_| code.is_none()) {
                let on = crate::breaker::blame(&events, &w.lane);
                let d = json!({"role": args.get(3), "agent": w.lane, "why": why, "failed_on": on});
                append(&ctx.state, &Event::new(ctx.now, fleet, Some(s.issue), "respawn", d))?;
            }
            go &= !matches!(code, Some("HOST_BUSY" | "DISK_LOW" | "MEM_LOW" | "RESPAWN_PACED"));
            let mut merge = step == "merge";
            let full = f.merge_label.is_some() && pr["landing"] != "fast-first";
            let green =
                ci == "green" && pr["isDraft"] != true && pr["headRefOid"].as_str() == args.get(5).copied() && !full;
            let verified = || -> Result<bool> {
                let st = fold(&working(&ctx.state)?).remove(&(fleet.to_string(), s.issue));
                Ok(crate::gate::verified(f, st.as_ref(), args[5], false).is_ok())
            };
            if step == "verdict" && code.is_none() && args[3] == "PASS" && green && verified()? {
                merge = true;
                code = if pr["mergeable"] == "CONFLICTING" {
                    Some("NOT_MERGEABLE")
                } else {
                    act(
                        ctx,
                        cfg,
                        r,
                        out,
                        &events,
                        (fleet, Some(s.issue), "merge"),
                        &["merge", fleet, &n],
                    )?
                };
            }
            if merge && !fast && conflicted(fleet, code) {
                let code = update_lane(ctx, cfg, r, out, &events, fleet, s.issue)?;
                go &= !matches!(code, Some("HOST_BUSY" | "DISK_LOW" | "MEM_LOW"));
            } else if merge && !fast && code.is_none() {
                updated = true;
                if let Err(e) = auto_update(ctx, cfg, r, out, fleet) {
                    writeln!(out, "{fleet} auto-update: {e:#}")?;
                }
            }
        } else if let Some(a) = (!fast)
            .then(|| stall::stalled(ctx, cfg, r, &events, &agents, w))
            .flatten()
        {
            let start = matches!(
                &a,
                stall::Act::Respawn(_, "failed start") | stall::Act::Escalate("START_FAILED", _)
            );
            if start && crate::breaker::failed(ctx, cfg, out, fleet, &w.lane, "no output after its spawn")? {
                continue;
            }
            match a {
                stall::Act::Respawn(role, why) => {
                    let args = ["spawn", fleet, &n, &role, "--supersede"];
                    let code = act(ctx, cfg, r, out, &events, (fleet, Some(s.issue), "respawn"), &args)?;
                    if code.is_none() {
                        let on = crate::breaker::blame(&events, &w.lane);
                        let d = json!({"role": role, "agent": w.lane, "why": why, "failed_on": on});
                        append(&ctx.state, &Event::new(ctx.now, fleet, Some(s.issue), "respawn", d))?;
                    }
                    go &= !matches!(code, Some("HOST_BUSY" | "DISK_LOW" | "MEM_LOW" | "RESPAWN_PACED"));
                }
                stall::Act::Escalate(code, why) => {
                    let on = crate::breaker::blame(&events, &w.lane);
                    escalate(ctx, cfg, out, (fleet, s.issue), (&w.lane, on), code, &why)?;
                }
            }
        } else if s.state == "blocked" && s.last_event != "no-action" {
            writeln!(out, "{fleet} #{n} no-action: {}", w.what)?;
            let d = json!({"why": w.what});
            append(&ctx.state, &Event::new(ctx.now, fleet, Some(s.issue), "no-action", d))?;
        } else if !fast && s.state == "ready" && (ci == "failed" || gone.is_some()) {
            let head = pr["headRefOid"].as_str().unwrap_or_default();
            let file = state_dir(ctx).join(format!("ci-{fleet}-{n}.md"));
            let gone = gone.map(|m| format!("FAIL\n\n{m} at {head}: no checks; fix the workflow.\n"));
            std::fs::write(&file, gone.unwrap_or_else(|| ci_findings(r, f, &pr, head)))?;
            let file = file.to_string_lossy();
            let args = ["verdict", fleet, &n, "FAIL", "--sha", head, "--file", &file];
            act(ctx, cfg, r, out, &events, (fleet, Some(s.issue), "ci-red"), &args)?;
        }
    }
    if !updated && !fast {
        if let Err(e) = auto_update(ctx, cfg, r, out, fleet) {
            writeln!(out, "{fleet} auto-update: {e:#}")?;
        }
    }
    Ok(go)
}

#[allow(clippy::too_many_arguments)]
fn no_proof(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (ev, agents): (&[Event], &[Value]),
    (fleet, n, lane): (&str, u64, &str),
    args: &[&str],
    fast: bool,
) -> Result<Option<Code>> {
    let sp = ev.iter().rfind(|e| e.is("spawn", fleet, n) && crate::events::checks(e));
    let on = crate::hosts::On::of(r, sp);
    let body = on.read(Path::new(args[7])).unwrap_or_default();
    let Some(why) = crate::gate::no_proof(&body) else {
        return Ok(None);
    };
    let hash = &crate::devices::sha256(body.as_bytes())[..16];
    let mine: Vec<&Event> = (ev.iter())
        .filter(|e| e.is("proof-refused", fleet, n) && sp.is_some_and(|p| e.ts >= p.ts))
        .collect();
    let seen = mine
        .iter()
        .any(|e| e.str("sha") == Some(args[5]) && e.str("hash") == Some(hash));
    let pane = sp.and_then(|e| e.str("pane"));
    let open = pane.filter(|_| agents.iter().any(|a| a["name"] == lane));
    let told = mine.iter().any(|e| e.val("prompted") == true);
    if !seen {
        act(ctx, cfg, r, out, ev, (fleet, Some(n), "verdict"), args)?;
        let text = format!("drover verdict refused your verdict.md: REFUSED NO_PROOF: {why}. Add the Proof blocks.");
        let tell =
            |p| crate::term::reply(&on, false, lane, p, &text).is_ok() && crate::term::landed(&on, lane, p, &text);
        let prompted = open.filter(|_| !told).is_some_and(tell);
        let d = json!({"sha": args[5], "hash": hash, "agent": lane, "prompted": prompted});
        emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(n), "proof-refused", d))?;
        if prompted {
            return Ok(Some(Some("NO_PROOF")));
        }
    }
    if fast {
        return Ok(Some(Some("NO_PROOF")));
    }
    let role = sp.and_then(|e| e.str("role")).unwrap_or("rev");
    let (num, why) = (n.to_string(), "verdict refused NO_PROOF: re-review");
    let c = ["spawn", fleet, &num, role, "--supersede"];
    let code = act(ctx, cfg, r, out, ev, (fleet, Some(n), "review"), &c)?;
    if code.is_none() {
        let d = json!({"role": role, "agent": lane, "why": why});
        append(&ctx.state, &Event::new(ctx.now, fleet, Some(n), "lane-lost", d))?;
    }
    Ok(Some(code))
}

fn repro_seen(r: &dyn Runner, ev: &[Event], (fleet, n): (&str, u64), args: &[&str]) -> (String, bool) {
    let sp = ev.iter().rfind(|e| e.is("spawn", fleet, n) && crate::events::checks(e));
    let body = crate::hosts::On::of(r, sp).read(Path::new(args[7])).unwrap_or_default();
    let hash = crate::devices::sha256(body.as_bytes())[..16].to_string();
    let seen = ev.iter().any(|e| {
        e.is("repro-refused", fleet, n)
            && sp.is_some_and(|p| e.ts >= p.ts)
            && e.str("sha") == Some(args[5])
            && e.str("hash") == Some(&hash)
    });
    (hash, seen)
}

fn repro_refused(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    (ev, agents): (&[Event], &[Value]),
    (fleet, n, lane): (&str, u64, &str),
    args: &[&str],
    hash: &str,
) -> Result<()> {
    let sp = ev.iter().rfind(|e| e.is("spawn", fleet, n) && crate::events::checks(e));
    let on = crate::hosts::On::of(r, sp);
    let why = ACTS.with_borrow(|m| m.get(fleet).and_then(|a| a.last()?["why"].as_str().map(String::from)));
    let why = why.unwrap_or_default();
    let told = ev
        .iter()
        .any(|e| e.is("repro-refused", fleet, n) && sp.is_some_and(|p| e.ts >= p.ts) && e.val("prompted") == true);
    let pane = sp
        .and_then(|e| e.str("pane"))
        .filter(|_| !told && agents.iter().any(|a| a["name"] == lane));
    let text = format!("drover verdict refused your verdict.md: REFUSED NO_REPRO: {why}. Fix the Repro block.");
    let tell = |p| crate::term::reply(&on, false, lane, p, &text).is_ok() && crate::term::landed(&on, lane, p, &text);
    let prompted = pane.is_some_and(tell);
    let d = json!({"sha": args[5], "hash": hash, "agent": lane, "why": why, "prompted": prompted});
    emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(n), "repro-refused", d))
}

fn tell(r: &dyn Runner, events: &[Event], lane: &str, text: &str) -> Option<bool> {
    let sp = crate::events::last_spawn(events, lane);
    let dsh = sp.and_then(|e| e.str("harness")) == Some("dsh");
    let pane = sp.and_then(|e| e.str("pane")).unwrap_or_default();
    let on = crate::hosts::On::of(r, sp);
    crate::term::awake(&on, lane).ok()?;
    Some(crate::term::reply(&on, dsh, lane, pane, text).is_ok())
}

pub(crate) const OLD: &str = "is in the way: replacing it";

fn last_words(fleet: &str) -> String {
    let last = ACTS.with_borrow(|m| m.get(fleet).and_then(|a| a.last()).map(|a| a["words"].to_string()));
    last.unwrap_or_default()
}

fn conflicted(fleet: &str, code: Code) -> bool {
    let w = last_words(fleet).to_lowercase();
    code == Some("NOT_MERGEABLE") || code.is_some() && (w.contains("not mergeable") || w.contains("conflict"))
}

fn serial(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (fleet, s): (&str, &IssueState),
    pr: &Value,
    fast: bool,
) -> Result<bool> {
    let (f, n, ev) = (cfg.fleet(fleet)?, s.issue, working(&ctx.state)?);
    let (st, q) = (fold(&ev), queue::replay(&ev, fleet));
    let mine = queue::groups(&q, &ev, f, fleet, n);
    let (mut wait, mut text) = (mine.iter().any(|g| queue::held(&ev, fleet, n, &g.name)), None);
    let (num, head) = (pr["number"].to_string(), pr["headRefOid"].as_str().unwrap_or_default());
    for g in queue::groups(&q, &[], f, fleet, n) {
        let has = |e: &Event| e.is("serial-joined", fleet, n) && e.str("group") == Some(&g.name);
        if !ev.iter().any(has) {
            let d = json!({"group": g.name, "by": "mark"});
            emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(n), "serial-joined", d))?;
        }
    }
    let new = crate::gate::unjoined(ctx, r, f, (fleet, n), (&num, head)).unwrap_or_default();
    let old = s.state.starts_with("review") || s.state == "ready";
    let mut by = ev
        .iter()
        .rev()
        .find(|e| e.is("serial-held", fleet, n))
        .and_then(|e| e.val("holder").as_u64());
    for (g, p) in &new {
        writeln!(out, "{fleet} #{n} serial-joined {}: {p}", g.name)?;
        let d = json!({"group": g.name, "by": "diff", "path": p});
        emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(n), "serial-joined", d))?;
        let h = queue::holders(&ev, &st, f, fleet, &g.name);
        if !old && (wait || h.len() >= g.max) {
            let h = h.first().map(|h| h.issue).or(by).unwrap_or(0);
            by = Some(h);
            let d = json!({"group": g.name, "holder": h});
            emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(n), "serial-held", d))?;
            let stop = format!(
                "stop: {} is held by #{h}; you'll get an update round when it merges",
                g.name
            );
            (wait, text) = (true, text.or(Some(stop)));
        }
    }
    let (now, mut blocker) = (working(&ctx.state)?, None);
    for g in queue::groups(&q, &now, f, fleet, n) {
        let h = queue::held(&now, fleet, n, &g.name);
        if let (false, Some(b)) = (h, blocker) {
            let d = json!({"group": g.name, "holder": b, "by": "order"});
            emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(n), "serial-held", d))?;
        } else if h && blocker.is_none() {
            let last = now
                .iter()
                .rev()
                .find(|e| e.is("serial-held", fleet, n) && e.str("group") == Some(&g.name));
            blocker = Some(last.and_then(|e| e.val("holder").as_u64()).unwrap_or(0));
        }
    }
    if let (Some(text), Some(lane)) = (text, s.lane.as_deref()) {
        writeln!(out, "{fleet} #{n} serial-held: {text}")?;
        let sp = crate::events::last_spawn(&ev, lane);
        let pane = sp.and_then(|e| e.str("pane")).unwrap_or_default();
        let dsh = sp.and_then(|e| e.str("harness")) == Some("dsh");
        crate::term::tell(&crate::hosts::On::of(r, sp), dsh, lane, pane, &text).ok();
    }
    if !wait || fast || !new.is_empty() {
        return Ok(wait);
    }
    let pos = |i: u64| q.iter().position(|x| x.issue == i).unwrap_or(usize::MAX);
    for g in mine.into_iter().filter(|g| queue::held(&ev, fleet, n, &g.name)) {
        let first_held = |i: u64| {
            queue::groups(&q, &ev, f, fleet, i)
                .into_iter()
                .find(|y| queue::held(&ev, fleet, i, &y.name))
        };
        let waits = |x: &&IssueState| {
            x.fleet == fleet
                && queue::in_flight(&x.state)
                && queue::held(&ev, fleet, x.issue, &g.name)
                && (x.issue == n || first_held(x.issue).is_some_and(|y| y.name == g.name))
        };
        let first = st.values().filter(waits).min_by_key(|x| (pos(x.issue), x.issue));
        if queue::holders(&ev, &st, f, fleet, &g.name).len() >= g.max || first.map(|x| x.issue) != Some(n) {
            return Ok(true);
        }
        let e = Event::new(ctx.now, fleet, Some(n), "serial-slot", json!({"group": g.name}));
        emit(&ctx.state, cfg, e)?;
    }
    update_lane(ctx, cfg, r, out, &ev, fleet, n)?;
    Ok(true)
}

fn update_lane(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    ev: &[Event],
    fleet: &str,
    issue: u64,
) -> Result<Code> {
    let p = cfg.fleet(fleet)?.flow();
    let b = p
        .next(p.gate(), "conflict")
        .and_then(|t| p.role(t.0))
        .unwrap_or(p.start());
    let n = issue.to_string();
    act(
        ctx,
        cfg,
        r,
        out,
        ev,
        (fleet, Some(issue), "conflict"),
        &["spawn", fleet, &n, b, "--supersede", "--update"],
    )
}

fn handoff(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    fleet: &str,
    ev: &[Event],
    waits: &[(IssueState, Waiting)],
) -> Result<bool> {
    let f = cfg.fleet(fleet)?;
    let mut any = false;
    for (s, w) in waits
        .iter()
        .filter(|(s, w)| s.fleet == fleet && s.lane.as_ref() == Some(&w.lane))
    {
        let pr = gh::cached_pr(ctx, fleet, s.issue);
        let head = pr["headRefOid"].as_str().unwrap_or_default();
        let handed = ev
            .iter()
            .any(|e| e.is("pushed", fleet, s.issue) && e.str("sha") == Some(head));
        let building = matches!(s.state.split('(').next(), Some("impl" | "fix"));
        let ci = gh::ci(&gh::required(&pr["statusCheckRollup"], &f.required_checks));
        let turn = waiting::turn(ev, s);
        let done = s.reviewed(head) && waiting::fix_done(ev, fleet, s.issue).is_some()
            || turn.is_some_and(|t| t.str("state") == Some("done"));
        let unlabelled = f.merge_label.as_deref().is_some_and(|l| !gh::has_label(&pr, l));
        let quiet = w.who == "none" || w.what.contains(waiting::NO_PUSH) || turn.is_some();
        let idle = done || quiet && ctx.now >= w.since + 120;
        if !(idle && building && (done || unlabelled || ci != "pending") && pr["state"] == "OPEN") || head.is_empty() {
            continue;
        }
        if s.reviewed(head) {
            any |= no_change(ctx, cfg, out, (fleet, s.issue), ev, head, ci)?;
            continue;
        }
        if handed {
            continue;
        }
        if let Some(why) = turn.and_then(|t| t.str("note").map(|w| (t.ts, w))) {
            let sp = crate::events::last_spawn(ev, &w.lane).map_or(0, |e| e.ts);
            let told = |e: &&Event| e.is("why-nudged", fleet, s.issue) && e.str("agent") == Some(&w.lane) && e.ts >= sp;
            match ev.iter().rfind(told) {
                None => {
                    let text = format!("drover: {WHY_NUDGE}{}. Fix it, then `drover turn done`.", why.1);
                    let Some(sent) = tell(r, ev, &w.lane, &text) else {
                        continue;
                    };
                    writeln!(out, "{fleet} #{} why note refused: nudged", s.issue)?;
                    let d = json!({"agent": w.lane, "note": why.1, "sent": sent});
                    append(&ctx.state, &Event::new(ctx.now, fleet, Some(s.issue), "why-nudged", d))?;
                    continue;
                }
                Some(p) if why.0 <= p.ts && ctx.now < p.ts + 600 => continue,
                Some(_) => {}
            }
        }
        writeln!(out, "{fleet} #{} pushed: {head:.7} via pr", s.issue)?;
        let d = json!({"sha": head, "via": "pr"});
        emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(s.issue), "pushed", d))?;
        any = true;
    }
    Ok(any)
}

fn no_change(
    ctx: &Ctx,
    cfg: &Config,
    out: &mut dyn Write,
    (fleet, n): (&str, u64),
    ev: &[Event],
    head: &str,
    ci: &str,
) -> Result<bool> {
    let Some(i) = ev.iter().rposition(|e| e.is("verdict", fleet, n)) else {
        return Ok(false);
    };
    let (v, since) = (&ev[i], &ev[i + 1..]);
    let at = v.str("sha").is_some_and(|x| x.eq_ignore_ascii_case(head));
    if v.str("verdict") != Some("FAIL") || !at || since.iter().any(|e| e.is("fix-no-change", fleet, n)) {
        return Ok(false);
    }
    let gate = cfg
        .fleet(fleet)?
        .flow()
        .get(v.str("stage").unwrap_or_default())
        .is_some_and(|g| g.kind == "gate");
    let pass = ci == "green" && (gate || crate::gate::form_only(v.str("body").unwrap_or_default(), &["ci".into()]));
    let done = since
        .iter()
        .rfind(|e| e.is("fix-done", fleet, n))
        .and_then(|e| e.str("why"));
    let Some(why) = done.or(pass.then_some("CI-only FAIL, CI green at the same head")) else {
        return Ok(false);
    };
    writeln!(out, "{fleet} #{n} fix-no-change at {head:.7}: {why} (CI {ci})")?;
    let d = json!({"sha": head, "why": why, "ci": ci});
    emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(n), "fix-no-change", d))?;
    if pass {
        let body = format!("PASS\n\nNo new commit: the CI-only FAIL at {head} is resolved, CI is green there now.\n");
        let d = json!({"verdict": "PASS", "sha": head, "stage": v.str("stage"), "loop": v.str("loop"), "body": body,
            "by": "drive"});
        emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(n), "verdict", d))?;
    }
    Ok(true)
}

fn external(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, fleet: &str) -> Result<()> {
    let events = working(&ctx.state)?;
    for s in fold(&events)
        .into_values()
        .filter(|s| s.fleet == fleet && !s.done() && s.state != "parked" && KICK.get().is_none_or(|k| k == s.issue))
    {
        let pr = gh::cached_pr(ctx, fleet, s.issue);
        let num = pr["number"].to_string();
        let seen = |e: &Event| e.is("blocked", fleet, s.issue) && e.str("pr") == Some(&num);
        let back = |e: &Event| e.issue == Some(s.issue) && e.fleet == fleet && e.str("landed_pr") == Some(&num);
        let closed = || gh::closed(ctx, r, cfg.fleet(fleet)?, fleet, ctx.now).map(|c| c.contains(&s.issue));
        let (kind, mut d) = match pr["state"].as_str() {
            Some("MERGED") if crate::gate::unlogged_merge(&events, fleet, s.issue).is_some() => continue,
            Some("MERGED") if !events.iter().any(back) => {
                let m = json!({"into": pr["baseRefName"], "merge_sha": pr["mergeCommit"]["oid"], "merged_at": pr["mergedAt"]});
                ("merged", m)
            }
            Some("CLOSED") if closed()? => ("closed", json!({})),
            Some("CLOSED") if !events.iter().any(seen) => (
                "blocked",
                json!({"code": "PR_CLOSED", "reason": "PR closed outside drover"}),
            ),
            _ => continue,
        };
        (d["pr"], d["by"]) = (json!(num), json!("external"));
        writeln!(out, "{fleet} #{} {kind} outside drover (PR #{num})", s.issue)?;
        emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(s.issue), kind, d))?;
    }
    Ok(())
}

fn ext_closed(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, fleet: &str) -> Result<()> {
    let events = working(&ctx.state)?;
    let refs: std::collections::BTreeSet<String> = queue::view(&events, &fold(&events), fleet)
        .into_iter()
        .flat_map(|i| i.ext)
        .collect();
    for b in refs {
        let Some((repo, n)) = b.rsplit_once('#') else { continue };
        let repo = match cfg.fleet.get(repo) {
            Some(f) => &f.repo,
            None => repo,
        };
        let p = gh::cache_path(ctx, "blocker", &b.replace(['/', '#'], "-"), 0);
        let ts = crate::usage::load::<Value>(&p).and_then(|c| c["ts"].as_u64());
        if ts.is_some_and(|t| ctx.now < t + 300) {
            continue;
        }
        let v = crate::runner::json(r, "gh", &["issue", "view", n, "-R", repo, "--json", "state"]);
        gh::store(&p, &json!({"ts": ctx.now}))?;
        if v.is_ok_and(|v| v["state"] == "CLOSED") {
            writeln!(out, "{fleet}: blocker {b} closed")?;
            emit(
                &ctx.state,
                cfg,
                Event::new(ctx.now, fleet, None, "blocker-closed", json!({"ref": b})),
            )?;
        }
    }
    Ok(())
}

/// #638: each pending spawn's fleet with its count, most first (`cooee 3, berth 2`): who holds the slots.
pub fn holders(ctx: &Ctx, cfg: &Config, r: &dyn Runner) -> String {
    let mut by = BTreeMap::<String, usize>::new();
    for p in pending(ctx, cfg, r) {
        *by.entry(p.fleet).or_default() += 1;
    }
    let mut by: Vec<_> = by.into_iter().collect();
    by.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let by: Vec<String> = by.iter().map(|(f, n)| format!("{f} {n}")).collect();
    by.join(", ")
}

fn starts<'a>(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    fleets: &[&'a str],
    mut go: bool,
) -> Vec<(&'a str, anyhow::Error)> {
    let mut errs = vec![];
    let events = match working(&ctx.state) {
        Ok(e) => e,
        Err(e) => return vec![(fleets.first().copied().unwrap_or_default(), e)],
    };
    let rr = state_dir(ctx).join("drive.first");
    let last = std::fs::read_to_string(&rr).unwrap_or_default();
    let mut order = fleets.to_vec();
    order.rotate_left(fleets.iter().position(|f| *f == last).map_or(0, |i| i + 1) % fleets.len().max(1));
    let one = |n: &String| KICK.get().is_none_or(|k| n == &k.to_string());
    let mut todo: Vec<(&str, std::collections::VecDeque<String>)> = order
        .iter()
        .map(|f| (*f, queue::startable(&events, f).into_iter().filter(one).collect()))
        .filter(|(_, q): &(_, std::collections::VecDeque<_>)| go && !q.is_empty())
        .collect();
    let max = cfg.limits.max_parallel_spawns;
    let weight = |f: &str| cfg.fleet.get(f).and_then(|f| f.weight).unwrap_or(1).max(1);
    let total: usize = todo.iter().map(|(f, _)| weight(f)).sum();
    let share = |f: &str| (max - usize::from(max > 1)) * weight(f);
    let (mut first, mut i) = (None, 0);
    while go && !todo.is_empty() {
        i %= todo.len();
        let fleet = todo[i].0;
        let held = || pending(ctx, cfg, r).iter().filter(|p| p.fleet == fleet).count();
        let full = max > 0 && KICK.get().is_none() && todo.len() > 1 && held() * total >= share(fleet);
        let Some(n) = todo[i].1.pop_front().filter(|_| !full) else {
            todo.remove(i);
            continue;
        };
        let at = (fleet, n.parse().ok(), "start");
        let design = n.parse().ok().and_then(|n| queue::wants_design(&events, fleet, n));
        let f = &cfg.fleet[fleet];
        let c = match design {
            Some(role) => ["spawn", fleet, &n, role, "--supersede"],
            None => ["spawn", fleet, &n, f.flow().start(), ""],
        };
        let code = match act(ctx, cfg, r, out, &events, at, &c[..4 + usize::from(design.is_some())]) {
            Ok(c) => c,
            Err(e) => {
                errs.push((fleet, e));
                todo.remove(i);
                continue;
            }
        };
        first = first.or(code.is_none().then_some(fleet));
        go &= !matches!(
            code,
            Some("HOST_BUSY" | "DISK_LOW" | "DISK_BUDGET" | "MEM_LOW" | "ALL_CAPPED")
        );
        if matches!(code, Some("CAP")) {
            todo.remove(i);
            continue;
        }
        i += 1;
    }
    if let Some(f) = first.filter(|_| KICK.get().is_none()) {
        let _ = std::fs::write(&rr, f);
    }
    for fleet in fleets {
        if let Err(e) = clean_merged(ctx, cfg, r, out, &events, fleet) {
            errs.push((fleet, e));
        }
    }
    errs
}

fn clean_merged(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    events: &[Event],
    fleet: &str,
) -> Result<()> {
    let st = fold(events);
    let merged = |e: &&Event| {
        e.issue
            .is_some_and(|n| st.get(&(fleet.into(), n)).is_some_and(IssueState::done))
    };
    let left = (events.iter())
        .filter(|e| matches!(e.kind.as_str(), "spawn" | "spawn-started") && e.fleet == fleet)
        .filter(merged)
        .filter(|e| !crate::hosts::held(ctx, cfg, e));
    if KICK.get().is_none() && !crate::hosts::present(r, left).is_empty() {
        let c = (fleet, None, "clean");
        act(ctx, cfg, r, out, events, c, &["clean", fleet, "--yes"])?;
    }
    Ok(())
}

/// #129: after a merge, merge integration into every other open ready PR whose lane isn't working, oldest PR first, in a scratch worktree (#207: never one in review, whose verdict would go stale). A clean merge is pushed (`auto-updated`) only when needed (#285: `update_ready`), else the PR merges as it is; a ready PR's conflict gets an `--update` lane.
pub fn auto_update(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, fleet: &str) -> Result<()> {
    if KICK.get().is_some() {
        return Ok(());
    }
    let (f, events, ag) = (
        cfg.fleet(fleet)?,
        working(&ctx.state)?,
        crate::hosts::all_agents(cfg, r).unwrap_or_default(),
    );
    let busy = |l: &str| ag.iter().any(|a| a["name"] == l && a["agent_status"] == "working");
    let open = |s: &IssueState| {
        let pr = gh::cached_pr(ctx, fleet, s.issue);
        pr["number"].as_u64().filter(|_| pr["state"] == "OPEN")
    };
    let mut prs: Vec<(u64, IssueState)> = (fold(&events).into_values())
        .filter(|s| s.fleet == fleet && s.state == "ready")
        .filter(|s| !s.lane.as_deref().is_some_and(busy))
        .filter_map(|s| Some((open(&s)?, s)))
        .collect();
    if prs.is_empty() {
        return Ok(());
    }
    prs.sort_by_key(|p| p.0);
    let (dir, wt) = (f.local_path(), ctx.state.with_file_name(format!("update-{fleet}")));
    let (wts, into) = (wt.to_string_lossy(), format!("origin/{}", f.integration));
    git(r, &dir, &["fetch", "-q", "origin"])?;
    let checks = crate::gate::premerge_checks(ctx, f);
    let tree = |a: &[&str]| git(r, &dir, &[&["worktree"][..], a].concat());
    for (_, s) in prs {
        let (n, branch) = (s.issue.to_string(), crate::gate::branch_of(Some(&s), s.issue));
        let from = git(r, &dir, &["rev-parse", &format!("origin/{branch}")])?;
        let _ = tree(&["remove", "--force", &wts]);
        tree(&["add", "-f", "--detach", &wts, &from])?;
        let clean = git(r, &wt, &["merge", "--no-edit", &into]).is_ok();
        let to = git(r, &wt, &["rev-parse", "HEAD"])?;
        let mut pr = gh::cached_pr(ctx, fleet, s.issue);
        crate::land::mark(ctx, Some(r), f, (fleet, s.issue), &events, &mut pr);
        let push = to != from && (pr["landing"] == "fast-first" || gh::update_ready(ctx, Some(r), f, fleet));
        let fail = crate::gate::run_checks(r, &wt, &into, if clean && push { &checks[..] } else { &[] });
        let res = if !clean {
            update_lane(ctx, cfg, r, out, &events, fleet, s.issue).map(drop)
        } else if let Some((c, o)) = fail {
            let at = format!("{from:.7} updated onto {into}");
            let rf = crate::gate::premerge_failed(ctx, cfg, r, out, (fleet, s.issue), (&from, &at), (c, &o));
            rf.and_then(|rf| Ok(writeln!(out, "{fleet} #{n} {rf}")?))
        } else if push {
            git(r, &wt, &["push", "-q", "origin", &format!("HEAD:{branch}")]).and_then(|_| {
                gh::forget_pr(ctx, fleet, s.issue);
                writeln!(out, "{fleet} #{n} auto-updated: {from:.7} → {to:.7}")?;
                let d = json!({"from": from, "to": to});
                emit(
                    &ctx.state,
                    cfg,
                    Event::new(ctx.now, fleet, Some(s.issue), "auto-updated", d),
                )?;
                pr["headRefOid"] = json!(to);
                crate::gate::sync_label(ctx, cfg, r, out, &s, &pr)
            })
        } else {
            Ok(())
        };
        let _ = tree(&["remove", "--force", &wts]);
        res?;
    }
    Ok(())
}

fn ci_findings(r: &dyn Runner, f: &crate::config::Fleet, pr: &serde_json::Value, head: &str) -> String {
    let checks = gh::required(&pr["statusCheckRollup"], &f.required_checks);
    let failed = checks
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| gh::check_state(c) == "failed");
    let url = failed.and_then(|c| c["detailsUrl"].as_str()).unwrap_or_default();
    let run = url
        .split("/runs/")
        .nth(1)
        .and_then(|t| t.split('/').next())
        .unwrap_or_default();
    let log = (!run.is_empty()).then(|| ok(r, "gh", &["run", "view", run, "-R", &f.repo, "--log-failed"]).ok());
    let log = log.flatten().unwrap_or_default();
    let tail: Vec<&str> = log
        .lines()
        .rev()
        .take(40)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let what = gh::not_green(&checks).unwrap_or_else(|| "a check failed".into());
    format!(
        "FAIL\n\nCI is red at {head}: {what}.\n\n```\n{}\n```\n",
        crate::lanes::mask(&tail.join("\n"))
    )
}

fn paced(cfg: &Config, fleet: &str) -> Option<String> {
    let max = cfg
        .fleet
        .get(fleet)
        .and_then(|f| f.respawn_per_pass)
        .unwrap_or(cfg.limits.respawn_per_pass);
    let ran = |a: &Value| a["step"] == "respawn" && a["code"].is_null();
    let n = ACTS.with_borrow(|m| m.get(fleet).map_or(0, |v| v.iter().filter(|a| ran(a)).count()));
    (n >= max).then(|| format!("{n} respawns this pass (respawn_per_pass {max}); the rest wait their turn"))
}

type At<'a> = (&'a str, Option<u64>, &'a str);
type Code = Option<&'static str>;

fn act(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, ev: &[Event], at: At, c: &[&str]) -> Result<Code> {
    let (fleet, issue, step) = at;
    let mut buf = vec![];
    let kicks = c[0] == "spawn" && KICK.get().is_none() && cfg.limits.max_parallel_spawns > 0;
    IN_ACT.set(!kicks);
    let spawn = issue.filter(|_| c[0] == "spawn");
    let back = spawn
        .filter(|_| KICK.get().is_none())
        .and_then(|n| retrying(ctx, ev, fleet, n));
    let paced = (c[0] == "spawn" && step == "respawn" && KICK.get().is_none())
        .then(|| paced(cfg, fleet))
        .flatten();
    let full = (spawn.filter(|_| KICK.get().is_none() && !kicks).zip(c.get(3)))
        .and_then(|(n, role)| crate::hosts::slot_before(ctx, cfg, r, ev, (fleet, n, role), &[]));
    let res = match (back, paced, issue.filter(|_| kicks)) {
        (Some(why), _, _) => crate::gate::refused("SPAWN_BACKOFF", why),
        (None, Some(why), _) => crate::gate::refused("RESPAWN_PACED", why),
        (None, None, None) if full.is_some() => crate::gate::refused("HOST_FULL", full.unwrap_or_default()),
        (None, None, Some(n)) => launch(ctx, cfg, r, ev, (fleet, n), c, step == "start"),
        (None, None, None) => crate::cli::run(c, ctx, r, &mut buf),
    };
    IN_ACT.set(false);
    let text = String::from_utf8_lossy(&buf);
    let (result, code) = match res {
        Ok(0) => (text.lines().next().unwrap_or("ok").to_string(), None),
        Ok(c) => (
            format!("exit {c}: {}", text.lines().next().unwrap_or_default()),
            Some("EXIT"),
        ),
        Err(e) => match e.downcast_ref::<crate::gate::Refusal>() {
            Some(rf) => (rf.to_string(), Some(rf.code)),
            None => (format!("error: {e:#}"), Some("ERROR")),
        },
    };
    if let Some(n) = issue.filter(|_| code.is_some() && c[0] == "spawn" && SLOT.get()) {
        crate::db::release(&ctx.state, &format!("spawn-{fleet}-{n}.pending"), None);
    }
    if let Some(n) = spawn.filter(|_| matches!(code, Some("EXIT" | "ERROR"))) {
        let why = result
            .split_once(": ")
            .map_or(&*result, |p| p.1)
            .lines()
            .next()
            .unwrap_or_default();
        let error = why.split("; closed ").next().unwrap_or(why);
        let head = gh::cached_pr(ctx, fleet, n)["headRefOid"].clone();
        let d = json!({"step": step, "error": error, "head": head});
        append(&ctx.state, &Event::new(ctx.now, fleet, Some(n), "spawn-error", d))?;
    }
    if step == "clean" && code.is_none() && !text.contains("removed ") {
        return Ok(None);
    }
    let outcome = match code {
        None if kicks && issue.is_some() => "pending",
        None => PAST.iter().find(|d| d.starts_with(step)).map_or(step, |d| d),
        Some("EXIT" | "ERROR") => "error",
        Some(_) => "waiting",
    };
    let w = if outcome == "pending" {
        outcome.into()
    } else {
        words(code, step, &result)
    };
    let why = code.map(|_| result.split_once(": ").map_or(&*result, |p| p.1));
    if let Some((n, k)) = spawn
        .zip(code)
        .filter(|_| !cfg.fleet(fleet).is_ok_and(|f| f.flow().is_check(c[3])))
    {
        crate::pending::refused(ctx, (fleet, n), k, why.unwrap_or_default())?;
    }
    ACTS.with_borrow_mut(|a| {
        a.entry(fleet.into()).or_default().push(
            json!({"issue": issue, "step": step, "outcome": outcome, "code": code, "words": w, "why": why, "kick": kicks}),
        );
    });
    if outcome == "pending" {
        return Ok(None);
    }
    let who = issue.map_or(fleet.to_string(), |n| format!("{fleet} #{n}"));
    let recent = |e: &Event| e.kind == "driven" && e.fleet == fleet && e.issue == issue && ctx.now < e.ts + 3600;
    let same = |e: &Event| e.str("step") == Some(step) && e.str("code") == code;
    if code.is_some() && ev.iter().any(|e| recent(e) && same(e)) {
        return Ok(code);
    }
    writeln!(out, "{who} {step}: {result}")?;
    let d = json!({"step": step, "cmd": format!("drover {}", c.join(" ")), "result": result, "code": code});
    emit(&ctx.state, cfg, Event::new(ctx.now, fleet, issue, "driven", d)).map(|()| code)
}
