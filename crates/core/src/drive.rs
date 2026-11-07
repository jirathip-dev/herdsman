//! #92 `herdsman drive`: one pass of the fleet loop, no AI; each action logs a passive `driven` event.
use crate::config::Config;
use crate::events::{append, emit, fold, read, Event, IssueState};
use crate::overlap::git;
use crate::runner::{ok, Runner};
use crate::waiting::Waiting;
use crate::{gh, queue, waiting, Ctx};
use anyhow::Result;
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// #168: a pass's lease outlives a hung pass by this much; #344: a dead holder's goes at once.
const STALE_LOCK_SECS: u64 = 3600;

/// #344: a held lease (`drive.lock`, a kick's): dropping the guard (error or panic included) releases it.
struct Lock(std::path::PathBuf, crate::db::Lease);
impl Drop for Lock {
    fn drop(&mut self) {
        crate::db::release(&self.0, &self.1.key, Some(&self.1));
    }
}

/// #344: lease `key` for `secs` with `data`, unless a live holder has it: one whose pid on this host is gone (or
/// reused) is dead at once, any other (or one without a pid, 0) once it expires.
fn take(ctx: &Ctx, r: &dyn Runner, key: &str, secs: u64, data: &str) -> Option<Lock> {
    let now = ctx.now;
    let dead = |l: &crate::db::Lease| gone(r, l, now);
    crate::db::take(&ctx.state, key, now, secs, data, &dead).map(|l| Lock(ctx.state.clone(), l))
}

/// #344: lease `key` is held (unexpired).
fn held(ctx: &Ctx, key: &str) -> Option<crate::db::Lease> {
    let mut all = crate::db::leases(&ctx.state, key).into_iter();
    all.find(|l| l.key == key && l.expires_at > ctx.now)
}

/// #344: `l`'s holder is a pid on this host that no longer runs (or was reused).
fn gone(r: &dyn Runner, l: &crate::db::Lease, now: u64) -> bool {
    l.pid != 0 && l.host == crate::db::host() && !alive(r, &l.pid.to_string(), now.saturating_sub(l.at))
}

/// #333: `pid` runs and started no later than `age` secs ago (a reused pid started after its file); no `ps`: alive.
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

/// #333: each spawn marker and kick lock (#344: leases), live or stale: stale once its pid is gone (or reused) or it
/// expired. `remove`: release each stale one, log `stale-marker`.
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
        if l.expires_at > ctx.now && age <= cfg.limits.spawn_pending_max_secs && !gone(r, &l, ctx.now) {
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

/// #333: `spawns starting N/<max>` and the stale markers swept in the last day: doctor's and the Host tab's line.
pub fn spawns_line(ctx: &Ctx, cfg: &Config, r: &dyn Runner, events: &[Event]) -> String {
    let n = markers(ctx, cfg, r, false)
        .0
        .iter()
        .filter(|m| m.starts_with("spawn-"))
        .count();
    let day = |e: &&Event| e.kind == "stale-marker" && ctx.now < e.ts + 86400;
    let gone: Vec<&str> = events.iter().filter(day).filter_map(|e| e.str("file")).collect();
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

/// #217: `drive.lock` over an extend's spawn, so no pass starts the same round meanwhile; `None` while a pass runs.
pub fn hold(ctx: &Ctx, r: &dyn Runner, fleet: &str) -> Option<impl Drop> {
    take(ctx, r, "drive.lock", STALE_LOCK_SECS, fleet)
}

fn state_dir(ctx: &Ctx) -> &Path {
    ctx.state.parent().unwrap_or(&ctx.state)
}

/// #140: a pass's lines as one toast, per action (`merged #1, #2 · started #3`), then `2 skipped (<codes>)`.
pub fn summary(fleet: &str, out: &str) -> String {
    if out.starts_with("drive: ") {
        return out.trim().into(); // another pass holds the lock
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
    // An action without an issue (clean) leaves a double space.
    format!("{fleet}: {}", parts.join(" · ").replace("  ", " ").trim_end())
}
const PAST: [&str; 3] = ["started", "merged", "reviewed"];

thread_local! {
    /// #166: each fleet's actions in the running pass, a silent repeat too: its `drive-pass` record.
    static ACTS: RefCell<BTreeMap<String, Vec<Value>>> = const { RefCell::new(BTreeMap::new()) };
    /// #304: the one issue a kick drives (`None`: all); `IN_ACT` while a pass runs a command, which then kicks nothing.
    static KICK: Cell<Option<u64>> = const { Cell::new(None) };
    static IN_ACT: Cell<bool> = const { Cell::new(false) };
    /// #560: the pass's one look at this host's admission (`room`), taken at its first launch: (looked, refusal).
    static ROOM: RefCell<(bool, Option<(&'static str, String)>)> = const { RefCell::new((false, None)) };
}

/// #304: `herdsman kick`: drive `issue`'s due steps now in a detached `kick --now` (#389: none from a lane on
/// another host), output to `kick.log`; a no-op inside a pass's own command. #333: the detached kick's pid; #340: in its
/// own session.
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
    // #345: a running service drives it in its own process at once; else the detached kick below (direct mode).
    if let Some(pid) = crate::service::kick(ctx, fleet, issue, why, id) {
        return Some(pid);
    }
    a.insert(5, "--now".into()); // after `--why <why>`, before any `--id`
    r.detach(&ctx.exe, &a, &state_dir(ctx).join("kick.log")).ok()
}

/// #304: `kick --now`: one pass of `issue` alone under the drive lock. A kick while one runs, or within the 10 s
/// after it, is coalesced into one follow-up; a busy lock is retried as one too.
pub fn kicked(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, k: Kicked) -> Result<()> {
    let (fleet, n, why, now, id) = k;
    if !now {
        cfg.fleet(fleet)?;
        kick_once(ctx, r, (fleet, n), why, id);
        return Ok(());
    }
    // #509: a trigger seen before (`processed` in state.db) is not driven again.
    if let Some(id) = id {
        if !crate::db::processed(&ctx.state, &format!("{fleet}#{n}:{why}:{id}"), ctx.now)? {
            return Ok(writeln!(out, "kick {why} {id}: already processed")?);
        }
    }
    let (again, ttl) = (format!("kick-{fleet}-{n}.again"), cfg.limits.spawn_pending_max_secs);
    let pending = (why == "drive").then(|| held(ctx, &format!("spawn-{fleet}-{n}.pending")));
    let mut pending = pending.flatten().map(|l| Lock(ctx.state.clone(), l));
    // #344: a busy kick hears of this one by its `.again` lease.
    let flag = || crate::db::take(&ctx.state, &again, ctx.now, ttl, why, &|_| true).map(drop);
    let Some(k) = take(ctx, r, &format!("kick-{fleet}-{n}.lock"), ttl, fleet) else {
        flag();
        return Ok(());
    };
    for _ in 0..30 {
        // #333: a running kick's lease stays unexpired.
        let expires_at = crate::now().max(ctx.now) + ttl;
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
        let res = drive(ctx, cfg, r, &mut buf, Some(fleet), false, false);
        KICK.set(None);
        // #318: a launched kick's marker goes after its first run; #604: its result then goes to its pass.
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

/// #318: a pass's spawn as its own detached kick, at most `max_parallel_spawns` at once, admitted counting those starting.
/// #513: a new `start` never takes the last free slot (unless it is the only one): in-flight work keeps it.
fn launch(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    ev: &[Event],
    (fleet, n): (&str, u64),
    role: &str,
    start: bool,
) -> Result<i32> {
    markers(ctx, cfg, r, true); // #333: only live kicks count; a dead or old marker goes first
    let live = pending(ctx);
    // #416: one bound for a full host (its earlier lanes' worktree is there) is refused there: it holds no slot.
    let low =
        |e: &Event| crate::host::free_gb(&crate::hosts::On::of(r, Some(e))).is_some_and(|g| g < cfg.limits.min_free_gb);
    let bound = |p: &Event| {
        ev.iter()
            .rev()
            .find(|e| p.issue.is_some_and(|n| e.is("spawn", &p.fleet, n)))
    };
    let n_live = live.iter().filter(|p| !bound(p).is_some_and(low)).count();
    let max = cfg.limits.max_parallel_spawns;
    if n_live >= max - usize::from(start && max > 1) || live.iter().any(|e| e.fleet == fleet && e.issue == Some(n)) {
        return crate::gate::refused("SPAWN_PENDING", format!("{n_live} spawns starting"));
    }
    crate::spawn::lane_admit(ctx, cfg.fleet(fleet)?, ev, fleet, n, role)?;
    if let Some((code, why)) = room(ctx, cfg, r) {
        return crate::gate::refused(code, why);
    }
    let mut d = json!({"role": role, "check": cfg.fleet(fleet)?.flow().is_check(role)});
    let (key, ttl) = (format!("spawn-{fleet}-{n}.pending"), cfg.limits.spawn_pending_max_secs);
    let ev = |d: &Value| serde_json::to_string(&Event::new(ctx.now, fleet, Some(n), "spawn", d.clone()));
    let l = crate::db::take(&ctx.state, &key, ctx.now, ttl, &ev(&d)?, &|_| true);
    let l = l.ok_or_else(|| anyhow::anyhow!("cannot lease {key}"))?;
    // #333: the marker passes to the detached kick's own pid: its death makes the marker stale at once; no pid (0):
    // it goes by age alone.
    let pid = kick(ctx, r, fleet, n, "drive");
    d["pid"] = pid.into();
    let data = if pid.is_some() { ev(&d)? } else { l.data.clone() };
    let pid = pid.unwrap_or(0);
    crate::db::renew(&ctx.state, &l, &crate::db::Lease { pid, data, ..l.clone() });
    Ok(0)
}

/// #560: a host short of room launches nothing: its refusal holds every spawn of the pass, so a review the order put
/// first never loses the room to a start whose kick happened to sample a quieter moment. Only without a remote host
/// to place on (each kick still places and admits its own spawn).
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

/// #604: a pass waits this long for its kicks to end before it logs one still running `pending`.
const SETTLE_SECS: u32 = 20;

/// #604: the spawns a pass's kicks started (their `spawn` logged, the prompt confirmed or not).
fn launched(acts: &[Value]) -> usize {
    acts.iter().filter(|a| a["spawned"] == true).count()
}

/// #604: each `pending` kicked spawn of this pass takes its kick's outcome once that kick ends (waited for up to
/// `SETTLE_SECS`), else stays `pending`; one line each.
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
    let ev = read(&ctx.state)?;
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

/// #604: pending kicked spawn `a` as its kick ended, from the log since `since`: its step's past (`started`) when its
/// `spawn` was logged, else the kick's own action on the issue (a refusal's code and words).
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

/// #604: once the pass that launched kick `fleet` #`n` is over (`drive.lock` free, at most 120 s), its `pending`
/// action takes the kick's outcome: that pass is logged again, updated.
fn update(ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write, fleet: &str, n: u64) -> Result<()> {
    for _ in 0..120 {
        if held(ctx, "drive.lock").is_none() {
            break;
        }
        r.sleep(1);
    }
    let ev = read(&ctx.state)?;
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

/// #318: the `spawn` in each live `spawn-<fleet>-<n>.pending`, one a launched kick is still starting.
pub(crate) fn pending(ctx: &Ctx) -> Vec<Event> {
    let all = crate::db::leases(&ctx.state, "spawn-").into_iter();
    let live = all.filter(|l| l.key.ends_with(".pending") && l.expires_at > ctx.now);
    live.filter_map(|l| serde_json::from_str(&l.data).ok()).collect()
}

/// #166: a code in plain words (#141's table, here until it lands); the host's own reason follows a busy Mac.
pub(crate) fn words(code: Option<&str>, step: &str, result: &str) -> String {
    let why = result.split_once(": ").map_or(result, |p| p.1);
    let why = why.split("; retry later").next().unwrap_or(why);
    match code {
        None => PAST.iter().find(|d| d.starts_with(step)).map_or(step, |d| d).into(),
        Some("HOST_BUSY") => format!("Mac busy ({why})"),
        Some("DISK_LOW") => format!("disk nearly full ({why})"),
        Some("CAP" | "ALL_CAPPED") => "lane limit or usage cap reached".into(),
        Some("CI_NOT_GREEN") => "waiting for CI".into(),
        Some("SPAWN_PENDING") => crate::waiting::SLOT.into(),
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

/// #166: the fleet's result line from a `drive-pass`: (tone, `HH:MM · started N · merged N · M waiting: <top reason>`).
/// Red on an error, green if it started or merged something, amber if everything was held back, else plain.
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

/// A recorded action as `drive` prints it (`<fleet> #<n> <step>: <words>`, a refusal's code before its words).
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

/// #166: a manual press that finds the lock held waits for that pass (at most 120 s), then prints its actions.
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
    let events = read(&ctx.state)?;
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

/// One pass over every fleet (or `only`), under `<state dir>/drive.lock`; `manual`: a press. #212 `fast`: verdicts
/// and merges only, PRs read afresh, under `drive-fast.lock`, so a slow pass starting lanes never holds it up.
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
    only.map(|f| cfg.fleet(f)).transpose()?;
    std::fs::create_dir_all(state_dir(ctx))?;
    let i = usize::from(fast);
    gh::FORCE.set(fast || KICK.get().is_some());
    let stamp = ctx.state.with_file_name(["drive.at", "drive-fast.at"][i]);
    let _ = std::fs::write(&stamp, ctx.now.to_string());
    let lock = ["drive.lock", "drive-fast.lock"][i];
    // #318: a kick's lease is `kick-<fleet>-<n>.lock`
    let mine = || take(ctx, r, lock, STALE_LOCK_SECS, only.unwrap_or_default()).map(Some);
    let Some(_held) = KICK.get().map_or_else(mine, |_| Some(None)) else {
        if manual {
            return wait(ctx, r, out, lock, only);
        }
        return Ok(writeln!(out, "drive: another pass holds {lock}; nothing done")?);
    };
    // #234: the fast step samples every host into the 24 h history (the Host tab's sparklines).
    if fast {
        crate::timer::stuck(ctx, cfg).unwrap_or_else(|e| drop(writeln!(out, "tick-stuck: {e:#}")));
        if let Err(e) = crate::hosts::sample(ctx, cfg, r, &crate::events::read(&ctx.state)?) {
            writeln!(out, "host history: {e:#}")?;
        }
    }
    // #298: shed's state changes since the last pass, logged per lane; the agent list each pass polls is the fallback.
    if let Some(b) = r.shed().filter(|_| !fast) {
        if let Err(e) = crate::shed::intake(ctx, cfg, &b.wire) {
            writeln!(out, "shed events: {e:#}")?;
        }
    }
    // #164: the disk budget before any start (the tick's pass only: a page's one-fleet drive skips the walk).
    if only.is_none() {
        if let Err(e) = crate::check::budget(ctx, cfg, r, out) {
            writeln!(out, "budget: {e:#}")?;
        }
    }
    let mine = |(n, f): &(&String, &crate::config::Fleet)| !f.paused && only.is_none_or(|o| o == n.as_str());
    let fleets: Vec<&str> = cfg.fleet.iter().filter(mine).map(|(n, _)| n.as_str()).collect();
    // #325: an answer that never reached its lane is typed again each fast step, until it lands or the lane is gone.
    let events = fast
        .then(|| crate::events::read(&ctx.state).ok())
        .flatten()
        .unwrap_or_default();
    for u in fleets.iter().flat_map(|f| crate::undelivered(&events, f)) {
        if let Err(e) = crate::deliver(ctx, cfg, r, u, u.str("text").unwrap_or_default(), false) {
            writeln!(out, "redeliver: {e:#}")?;
        }
    }
    // A paused fleet gets no pass (and no `last pass`); one fleet's error doesn't skip the others.
    let (mut res, mut start) = (Ok(()), true);
    ACTS.with_borrow_mut(BTreeMap::clear);
    ROOM.set((false, None));
    let mut fail = |name: &str, e: anyhow::Error| {
        let w = format!("failed: {e:#}");
        let a = json!({"step": "pass", "outcome": "error", "code": "ERROR", "words": w});
        ACTS.with_borrow_mut(|m| m.entry(name.into()).or_default().push(a));
        res = Err(e);
    };
    // #337: one look per step (each remote host in one ssh call), saved for the board; the passes scan it.
    let log = || crate::db::last_seq(&ctx.state);
    let mut seen = if fleets.is_empty() {
        crate::snapshot::Snap::read(ctx, r) // every fleet paused: nothing to look at
    } else {
        crate::snapshot::take(ctx, cfg, r)
    };
    // #376: lane-shaped panes no spawn logged are adopted (or closed, stale) before any step reads the lanes.
    if !fleets.is_empty() && KICK.get().is_none() {
        match crate::spawn::orphans(ctx, cfg, (r, &seen), out) {
            Ok(true) => seen = crate::snapshot::take(ctx, cfg, r),
            Ok(false) => {}
            Err(e) => writeln!(out, "orphans: {e:#}")?,
        }
    }
    if !fast {
        if let Err(e) = repair(ctx, cfg, r, out) {
            writeln!(out, "repair: {e:#}")?;
        }
    }
    let grew = log();
    // #389: remote lanes' commands that found no route here, as the look read them.
    if fast {
        crate::outbox::drain(ctx, (r, &seen), out, &fleets).unwrap_or_else(|e| drop(writeln!(out, "outbox: {e:#}")));
    }
    // #182: every fleet's due steps (reviews first) before any fleet starts new work.
    let mut due = vec![];
    for name in &fleets {
        match pass(ctx, cfg, (r, &seen), out, name, fast) {
            Ok(go) => {
                start &= go;
                due.push(*name);
            }
            Err(e) => fail(name, e),
        }
    }
    for name in due.into_iter().filter(|_| !fast) {
        match starts(ctx, cfg, r, out, name, start) {
            Ok(go) => start = go,
            Err(e) => fail(name, e),
        }
    }
    // #337: a step that logged something is looked at again, so the board shows what it did.
    if log() != grew {
        crate::snapshot::take(ctx, cfg, r);
    }
    // #246: the step's length beside its start (`drive-fast.at`: `<started> <secs>`), for the Host tab.
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
            continue; // a fast step or kick that did nothing logs no pass
        }
        // #166: one `drive-pass` per fleet and pass, manual or automatic: the board's result line.
        let end = crate::now();
        let (by, at) = (
            ["auto", "manual", "fast"][usize::from(manual) + 2 * usize::from(fast)],
            crate::usage::date(r, end, "+%H:%M"),
        );
        let d = json!({"by": by, "started": ctx.now, "ended": end, "at": at, "secs": secs,
            "spawns_launched": launched(&acts), "actions": acts, "kick": KICK.get()});
        let _ = append(&ctx.state, &Event::new(ctx.now, name, None, "drive-pass", d));
    }
    // #343: a slow pass folds the log into its checkpoint, and rotates a log over `rotate_mb`; #365: reads backlogs.
    if !fast && KICK.get().is_none() {
        for n in &fleets {
            crate::gh::backlog(ctx, r, cfg.fleet(n)?, n).unwrap_or_else(|e| eprintln!("herdsman: {n} backlog: {e:#}"));
        }
        crate::check::stale(ctx, cfg, r, out).unwrap_or_else(|e| eprintln!("herdsman: stale panes: {e:#}"));
        let max = cfg.limits.rotate_mb << 20;
        let cp = crate::events::checkpoint(&ctx.state).and_then(|_| crate::events::rotate(&ctx.state, max, ctx.now));
        cp.unwrap_or_else(|e| eprintln!("herdsman: checkpoint: {e:#}"));
    }
    res
}

/// One fleet: each item's due step, reviews first. Each spawn samples the host afresh; false when a spawn met a
/// busy host or full disk, so no fleet starts new work this pass. `fast`: verdicts and merges only (#212).
/// #500: an issue parked `LANE_ENDS` whose every `lane gone` respawn followed a design lane's adoption (before #500 that
/// adoption hid the issue's verdict) goes back to the state it had (`repaired`); once installed, none is left to find.
/// #517: a lesson issue parked `LANE_ENDS` is closed as "no repeated class".
fn repair(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let events = read(&ctx.state)?;
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
        let adopted = |e: &Event| crate::events::design(e) && e.data.get("adopted") == Some(&json!(true));
        let Some(first) = round.iter().position(|(_, e)| e.kind == "respawn") else {
            continue;
        };
        let gone = (round.iter()).all(|(_, e)| e.kind != "respawn" || e.str("why") == Some("lane gone"));
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
    // #425: a lane held on an issue that merged is woken once (`released`): prompted in its pane, or respawned
    // `--supersede` when its pane is gone (a slow pass's).
    let ev = read(&ctx.state)?;
    let st = fold(&ev);
    for s in st.values().filter(|s| s.fleet == fleet) {
        let on = crate::events::held_on(&ev, s);
        let Some((role, lane)) = s.active_lane().filter(|_| !on.is_empty()) else {
            continue;
        };
        let sp = crate::events::last_spawn(&ev, lane);
        let live = agents.iter().any(|a| a["name"] == lane);
        let pane = sp.and_then(|e| e.str("pane")).filter(|_| live);
        let away = crate::hosts::offline(ctx, sp).is_some(); // #478: woken once its host is back
        let open = |b: &u64| st.get(&(fleet.into(), *b)).is_none_or(|b| b.state != "merged");
        if on.iter().any(open) || fast && !live || away {
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
            Some(p) => crate::herdr::reply(&crate::hosts::On::of(r, sp), dsh, lane, p, &text).is_ok(),
            None => act(ctx, cfg, r, out, &ev, (fleet, None, "respawn"), &c)?.is_none(),
        };
        let d = json!({"agent": lane, "on": on, "respawn": !live});
        let e = Event::new(ctx.now, fleet, Some(s.issue), "released", d);
        if woke {
            emit(&ctx.state, cfg, e)?;
        }
    }
    // #412: a design lane done with its pushed design hands it to the owner.
    for s in st.values().filter(|s| s.fleet == fleet && s.state == "design" && !fast) {
        crate::design::handoff(ctx, cfg, r, out, &agents, s)?;
    }
    let events = read(&ctx.state)?;
    let one = |(s, _): &(IssueState, Waiting)| KICK.get().is_none_or(|k| k == s.issue);
    let mut waits = waiting::scan(ctx, cfg, seen, &events, &agents, true);
    waits.retain(one);
    external(ctx, cfg, r, out, fleet)?;
    if KICK.get().is_none() {
        if let Err(e) = crate::land::watch(ctx, cfg, r, out, fleet) {
            writeln!(out, "{fleet} land: {e:#}")?;
        }
    }
    if !fast && handoff(ctx, cfg, out, fleet, &events, &waits)? {
        waits = waiting::scan(ctx, cfg, seen, &read(&ctx.state)?, &agents, false);
        waits.retain(one);
    }
    // #129: ready PRs that conflict with nothing merge first; of a conflicting pair the older, the other is updated.
    let preview = || crate::overlap::preview(ctx, cfg, r, fleet, &events).ok();
    let conflicts = (!fast).then(preview).flatten().unwrap_or_default();
    // #173: a review is a spawn of any check stage's role, not only `rev`.
    let rev = |w: &Waiting| {
        w.next_cmd
            .as_deref()
            .and_then(|c| c.split(' ').nth(4))
            .is_some_and(|r| f.family(r) == "rev")
    };
    // #207: every written verdict is recorded before any merge (and the auto-update after it) moves a head.
    let verdict = |w: &Waiting| {
        w.next_cmd
            .as_deref()
            .is_some_and(|c| c.starts_with("herdsman verdict "))
    };
    // #560: verdicts, then merges, then reviews of pushed heads, then fix and update rounds and respawns (new starts
    // follow every fleet's pass): a host short of room spends it in that order.
    let merge = |w: &Waiting| w.next_cmd.as_deref().is_some_and(|c| c.starts_with("herdsman merge "));
    waits.sort_by_key(|(s, w)| (!verdict(w), !merge(w), !rev(w), conflicts.contains_key(&s.issue)));
    let (mut go, mut updated) = (true, false);
    for (s, w) in waits
        .iter()
        .filter(|(s, w)| s.fleet == fleet && s.lane.as_ref() == Some(&w.lane))
    {
        // #212: the fast and slow steps never act on one issue at once.
        let lock = format!("drive-{fleet}-{}.lock", s.issue);
        let Some(_mine) = take(ctx, r, &lock, STALE_LOCK_SECS, fleet) else {
            continue;
        };
        let mut pr = gh::cached_pr(ctx, fleet, s.issue);
        crate::land::mark(ctx, Some(r), f, (fleet, s.issue), &events, &mut pr);
        let ci = gh::ci(&gh::required(&pr["statusCheckRollup"], f.checks(&pr)));
        // #388: CI that never starts is red too, as the card says it (an unlabelled PR has no full CI by design).
        let gone = gh::missing(ctx, None, cfg, (fleet, s.issue), &events, &pr).filter(|m| w.what.contains(m));
        let n = s.issue.to_string();
        // #294: a PR's diff joins its serial groups; an issue waiting for a slot does nothing else this pass.
        if pr["state"] == "OPEN" && !f.serial.is_empty() && serial(ctx, cfg, r, out, (fleet, s), &pr, fast)? {
            continue;
        }
        if let Err(e) = crate::gate::sync_label(ctx, cfg, r, out, s, &pr) {
            writeln!(out, "{fleet} #{n} label: {e:#}")?;
        }
        // #148: a head auto-updated this pass has no CI yet; its merge waits for the next pass.
        let now = |e: &Event| e.is("auto-updated", fleet, s.issue) && e.ts == ctx.now;
        if let Some(e) = read(&ctx.state)?.into_iter().rev().find(now) {
            let to = e.str("to").unwrap_or_default();
            writeln!(out, "{fleet} #{n} ready (CI pending at {to:.7})")?;
        } else if w.who == "drive" && w.what == waiting::LESSON_PROMPT && !fast {
            // #517: a lesson lane's one prompt to end with `done --no-change`.
            let sent = tell(r, &events, &w.lane, &crate::lesson::prompt(fleet, s.issue));
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
            // #511: a `done` with nothing new pushed is nudged once; a `working` lane gone idle is told to go on.
            let (state, text) = if w.what == waiting::TURN_NUDGE {
                let text = "herdsman: you reported `done`, but nothing new is pushed at the PR head (or there is no \
                    PR). Push and open the PR, then `herdsman turn done`; or `herdsman turn blocked --why <reason>`.";
                ("done", text)
            } else {
                (
                    "working",
                    "herdsman: continue the task; end the turn with `herdsman turn …`.",
                )
            };
            let sent = tell(r, &events, &w.lane, text);
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
            // #511: a lane that reports itself blocked is the owner's: a card with its reason.
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
            // #444: a gone lane with no push is respawned once due; past two respawns it needs a human.
            let gone = w.what.split_once(waiting::NO_PUSH);
            if gone.is_some() && w.next_cmd.is_none() && !fast {
                let why = "lane keeps ending without a push";
                writeln!(out, "{fleet} #{n} needs-human: {why}")?;
                let d = json!({"code": "LANE_ENDS", "reason": why, "agent": w.lane});
                emit(
                    &ctx.state,
                    cfg,
                    Event::new(ctx.now, fleet, Some(s.issue), "needs-human", d),
                )?;
            }
            if gone.is_some_and(|g| !g.1.starts_with("respawning")) {
                continue;
            }
            // `waiting` names the exact next command; drive runs it.
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
            // #417: a verify stage the issue doesn't need (or has no recipe for) is logged passed instead.
            if step == "review" && args[3] == "verify" && !crate::verify::due(ctx, cfg, r, out, s, &pr)? {
                continue;
            }
            // #491: a FAIL refused NO_PROOF is never retried unchanged; its reviewer is told once, or re-run once ended.
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
            // #607: a PASS refused NO_REPRO is never retried unchanged either; its reviewer is told once.
            let repro =
                (step == "verdict" && args[3] == "PASS").then(|| repro_seen(r, &events, (fleet, s.issue), &args));
            if repro.as_ref().is_some_and(|x| x.1) {
                continue;
            }
            if w.what.starts_with("reviewer exited") {
                let d = json!({"role": args.get(3), "why": "exited without a verdict"});
                append(&ctx.state, &Event::new(ctx.now, fleet, Some(s.issue), "lane-lost", d))?;
            }
            let mut code = act(ctx, cfg, r, out, &events, (fleet, Some(s.issue), step), &args)?;
            // #534: a finished lane of the issue in the way is replaced in the same pass; a working one still waits.
            if code == Some("LANE_LIVE") && args[0] == "spawn" && last_words(fleet).contains(OLD) {
                let a = [&args[..], &["--supersede"]].concat();
                code = act(ctx, cfg, r, out, &events, (fleet, Some(s.issue), step), &a)?;
            }
            if let Some((hash, _)) = repro.filter(|_| code == Some("NO_REPRO")) {
                repro_refused(ctx, cfg, r, (&events, &agents), (fleet, s.issue, &w.lane), &args, &hash)?;
            }
            if let Some((why, _)) = gone.filter(|_| code.is_none()) {
                let d = json!({"role": args.get(3), "agent": w.lane, "why": why});
                append(&ctx.state, &Event::new(ctx.now, fleet, Some(s.issue), "respawn", d))?;
            }
            go &= !matches!(code, Some("HOST_BUSY" | "DISK_LOW" | "MEM_LOW"));
            // #182: a PASS recorded this pass merges in it once CI is green at that head (a merge label's full run
            // starts only now, so that one waits for the next pass).
            let mut merge = step == "merge";
            let full = f.merge_label.is_some() && pr["landing"] != "fast-first";
            let green = ci == "green" && pr["headRefOid"].as_str() == args.get(5).copied() && !full;
            // #599: a PASS that goes on to another check (verify) merges only after that check, next pass.
            let then = f.flow().next(&s.stage, "PASS").and_then(|t| f.flow().get(t.0));
            let last = then.is_none_or(|t| t.kind != "check");
            if step == "verdict" && code.is_none() && args[3] == "PASS" && green && last {
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
            // #214: a merge that conflicts (known before, or failed on it) gets its `--update` lane instead.
            if merge && !fast && conflicted(fleet, code) {
                let code = update_lane(ctx, cfg, r, out, &events, fleet, s.issue)?;
                go &= !matches!(code, Some("HOST_BUSY" | "DISK_LOW" | "MEM_LOW"));
            } else if merge && !fast && code.is_none() {
                updated = true;
                if let Err(e) = auto_update(ctx, cfg, r, out, fleet) {
                    writeln!(out, "{fleet} auto-update: {e:#}")?;
                }
            }
        } else if s.state == "blocked" && s.last_event != "no-action" {
            // #214: a block drive has no step for is logged once, never left silent.
            writeln!(out, "{fleet} #{n} no-action: {}", w.what)?;
            let d = json!({"why": w.what});
            append(&ctx.state, &Event::new(ctx.now, fleet, Some(s.issue), "no-action", d))?;
        } else if !fast && s.state == "ready" && (ci == "failed" || gone.is_some()) {
            // Red CI after a PASS is a FAIL at the same head (no new round), the failed checks as the finding.
            let head = pr["headRefOid"].as_str().unwrap_or_default();
            let file = state_dir(ctx).join(format!("ci-{fleet}-{n}.md"));
            let gone = gone.map(|m| format!("FAIL\n\n{m} at {head}: no checks; fix the workflow.\n"));
            std::fs::write(&file, gone.unwrap_or_else(|| ci_findings(r, f, &pr, head)))?;
            let file = file.to_string_lossy();
            let args = ["verdict", fleet, &n, "FAIL", "--sha", head, "--file", &file];
            act(ctx, cfg, r, out, &events, (fleet, Some(s.issue), "ci-red"), &args)?;
        }
    }
    // #214: clean catch-ups run every slow pass, not only after a merge.
    if !updated && !fast {
        if let Err(e) = auto_update(ctx, cfg, r, out, fleet) {
            writeln!(out, "{fleet} auto-update: {e:#}")?;
        }
    }
    Ok(go)
}

/// #491: `None` for a FAIL with its Proof blocks (recorded as usual). A proofless one is refused once per head and
/// file content (`proof-refused`); its reviewer, still in herdr, is prompted with the refusal once; an ended one
/// gets a fresh review of the same head, which no verdict makes a new round.
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
    // `waiting` offers a verdict only once its reviewer stopped working: one still in herdr is told once; gone, or idle
    // again after that, it has ended.
    let pane = sp.and_then(|e| e.str("pane"));
    let open = pane.filter(|_| agents.iter().any(|a| a["name"] == lane));
    let told = mine.iter().any(|e| e.data["prompted"] == true);
    if !seen {
        act(ctx, cfg, r, out, ev, (fleet, Some(n), "verdict"), args)?;
        let text = format!("herdsman verdict refused your verdict.md: REFUSED NO_PROOF: {why}. Add the Proof blocks.");
        let tell =
            |p| crate::herdr::reply(&on, false, lane, p, &text).is_ok() && crate::herdr::landed(&on, lane, p, &text);
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

/// #607: verdict.md's content hash, and whether a `repro-refused` since its reviewer's spawn names it at this head.
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

/// #607: log a `NO_REPRO` refusal (its why for `waiting`); a reviewer still in herdr is prompted with it once.
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
        .any(|e| e.is("repro-refused", fleet, n) && sp.is_some_and(|p| e.ts >= p.ts) && e.data["prompted"] == true);
    let pane = sp
        .and_then(|e| e.str("pane"))
        .filter(|_| !told && agents.iter().any(|a| a["name"] == lane));
    let text = format!("herdsman verdict refused your verdict.md: REFUSED NO_REPRO: {why}. Fix the Repro block.");
    let tell = |p| crate::herdr::reply(&on, false, lane, p, &text).is_ok() && crate::herdr::landed(&on, lane, p, &text);
    let prompted = pane.is_some_and(tell);
    let d = json!({"sha": args[5], "hash": hash, "agent": lane, "why": why, "prompted": prompted});
    emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(n), "repro-refused", d))
}

/// #517 #511: type `text` into `lane`'s pane (its last spawn's); true if sent.
fn tell(r: &dyn Runner, events: &[Event], lane: &str, text: &str) -> bool {
    let sp = crate::events::last_spawn(events, lane);
    let dsh = sp.and_then(|e| e.str("harness")) == Some("dsh");
    let pane = sp.and_then(|e| e.str("pane")).unwrap_or_default();
    crate::herdr::reply(&crate::hosts::On::of(r, sp), dsh, lane, pane, text).is_ok()
}

/// #534: the words of a `LANE_LIVE` on a finished lane.
pub(crate) const OLD: &str = "is in the way: replacing it";

/// The words of the fleet's last action this pass.
fn last_words(fleet: &str) -> String {
    let last = ACTS.with_borrow(|m| m.get(fleet).and_then(|a| a.last()).map(|a| a["words"].to_string()));
    last.unwrap_or_default()
}

/// #214: the merge just tried was refused or failed on a conflict (its words say so).
fn conflicted(fleet: &str, code: Code) -> bool {
    let w = last_words(fleet).to_lowercase();
    code == Some("NOT_MERGEABLE") || code.is_some() && (w.contains("not mergeable") || w.contains("conflict"))
}

/// #294: join `s` to each serial group its PR's diff first touches (`serial-joined`); joining a full one, or any after
/// one it waits for, holds it there (`serial-held`) and tells its lane to stop. A slot that frees goes to the first held
/// issue in queue order (`serial-slot`); once it holds all its groups (name order: no deadlock) it gets an update round.
/// True while it waits.
fn serial(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (fleet, s): (&str, &IssueState),
    pr: &Value,
    fast: bool,
) -> Result<bool> {
    let (f, n, ev) = (cfg.fleet(fleet)?, s.issue, read(&ctx.state)?);
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
    // Drive sees a push before its review, so a first join at review/ready is work from before the group (grandfathered).
    let old = s.state.starts_with("review") || s.state == "ready";
    let mut by = ev
        .iter()
        .rev()
        .find(|e| e.is("serial-held", fleet, n))
        .and_then(|e| e.data["holder"].as_u64());
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
    // Ordered locking: held in one group, it lets go of its slots in later ones, so no two issues hold-and-wait crossed.
    let (now, mut blocker) = (read(&ctx.state)?, None);
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
            blocker = Some(last.and_then(|e| e.data["holder"].as_u64()).unwrap_or(0));
        }
    }
    if let (Some(text), Some(lane)) = (text, s.lane.as_deref()) {
        writeln!(out, "{fleet} #{n} serial-held: {text}")?;
        let sp = crate::events::last_spawn(&ev, lane);
        let pane = sp.and_then(|e| e.str("pane")).unwrap_or_default();
        let dsh = sp.and_then(|e| e.str("harness")) == Some("dsh");
        crate::herdr::reply(&crate::hosts::On::of(r, sp), dsh, lane, pane, &text).ok();
    }
    if !wait || fast || !new.is_empty() {
        return Ok(wait);
    }
    let pos = |i: u64| q.iter().position(|x| x.issue == i).unwrap_or(usize::MAX);
    for g in mine.into_iter().filter(|g| queue::held(&ev, fleet, n, &g.name)) {
        // Another waiter is a candidate only once this is its first held group (its earlier slots taken).
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

/// A ready PR's conflict: the `--update` round of the role the gate's `conflict` outcome goes to.
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

/// #182: an impl/fix lane idle ≥ 2 min whose open PR head moved past its last review or handoff, CI done there: the
/// PR hands the head off (`pushed`, via pr) though the lane never did, and its review is due. True if one was logged.
fn handoff(
    ctx: &Ctx,
    cfg: &Config,
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
        // #445: a recorded `done --no-change` is due at once; a check that runs only once labelled never blocks it.
        // #511: so is a head its lane reported `done` at; any reported turn is idle.
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
        writeln!(out, "{fleet} #{} pushed: {head:.7} via pr", s.issue)?;
        let d = json!({"sha": head, "via": "pr"});
        emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(s.issue), "pushed", d))?;
        any = true;
    }
    Ok(any)
}

/// #319: a settled fix lane left the head of the FAIL as it was. A CI-only FAIL (a gate's CI red, or every blocker tagged
/// `[ci]`) with CI green there now is resolved: a PASS at that head, so it merges. Any other FAIL after `herdsman done
/// --no-change` is reviewed again at the same head (`waiting`). True if `fix-no-change` was logged.
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

/// #285: a tracked issue whose PR (as last read) was merged or closed outside herdsman is logged once, no comment:
/// `merged`/`closed` `by: external`, or `blocked` while its issue is open; `clean` takes its lanes.
fn external(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, fleet: &str) -> Result<()> {
    let events = read(&ctx.state)?;
    for s in fold(&events)
        .into_values()
        .filter(|s| s.fleet == fleet && !s.done() && s.state != "parked" && KICK.get().is_none_or(|k| k == s.issue))
    {
        let pr = gh::cached_pr(ctx, fleet, s.issue);
        let num = pr["number"].to_string();
        let seen = |e: &Event| e.is("blocked", fleet, s.issue) && e.str("pr") == Some(&num);
        // #350: the PR of a landing reverted (or fixed forward) stays merged; its issue is in a fix round.
        let back = |e: &Event| e.issue == Some(s.issue) && e.fleet == fleet && e.str("landed_pr") == Some(&num);
        let closed = || gh::closed(ctx, r, cfg.fleet(fleet)?, fleet, ctx.now).map(|c| c.contains(&s.issue));
        let (kind, mut d) = match pr["state"].as_str() {
            Some("MERGED") if !events.iter().any(back) => {
                let m = json!({"into": pr["baseRefName"], "merge_sha": pr["mergeCommit"]["oid"], "merged_at": pr["mergedAt"]});
                ("merged", m)
            }
            Some("CLOSED") if closed()? => ("closed", json!({})),
            Some("CLOSED") if !events.iter().any(seen) => (
                "blocked",
                json!({"code": "PR_CLOSED", "reason": "PR closed outside herdsman"}),
            ),
            _ => continue,
        };
        (d["pr"], d["by"]) = (json!(num), json!("external"));
        writeln!(out, "{fleet} #{} {kind} outside herdsman (PR #{num})", s.issue)?;
        emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(s.issue), kind, d))?;
    }
    Ok(())
}

/// One fleet's starts (every startable queued issue in order), then clean merged lanes. A refused item is skipped,
/// `CAP` ends this fleet's starts, `HOST_BUSY`/`DISK_LOW`/`ALL_CAPPED` all fleets' (false).
fn starts(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, fleet: &str, start: bool) -> Result<bool> {
    let (events, f) = (read(&ctx.state)?, cfg.fleet(fleet)?);
    let mut go = start;
    let one = |n: &String| KICK.get().is_none_or(|k| n == &k.to_string());
    for n in queue::startable(&events, fleet)
        .into_iter()
        .filter(one)
        .take_while(|_| start)
    {
        let at = (fleet, n.parse().ok(), "start");
        // #412: a design-marked issue gets its design lane (again, after a revise) until the owner approves it.
        let design = n.parse().is_ok_and(|n| queue::wants_design(&events, fleet, n));
        let c = if design {
            ["spawn", fleet, &n, "design", "--supersede"]
        } else {
            ["spawn", fleet, &n, f.flow().start(), ""]
        };
        let code = act(ctx, cfg, r, out, &events, at, &c[..4 + usize::from(design)])?;
        go &= !matches!(
            code,
            Some("HOST_BUSY" | "DISK_LOW" | "DISK_BUDGET" | "MEM_LOW" | "ALL_CAPPED")
        );
        if matches!(
            code,
            Some("HOST_BUSY" | "DISK_LOW" | "DISK_BUDGET" | "MEM_LOW" | "CAP" | "ALL_CAPPED")
        ) {
            break;
        }
    }
    let st = fold(&events);
    let merged = |e: &&Event| {
        e.issue
            .is_some_and(|n| st.get(&(fleet.into(), n)).is_some_and(IssueState::done))
    };
    let left = |e: &&Event| {
        e.str("worktree")
            .is_some_and(|w| crate::hosts::On::of(r, Some(e)).exists(Path::new(w)))
    };
    if KICK.get().is_none()
        && events
            .iter()
            .filter(|e| matches!(e.kind.as_str(), "spawn" | "spawn-started") && e.fleet == fleet)
            .filter(merged)
            .any(|e| left(&e))
    {
        let c = (fleet, None, "clean");
        act(ctx, cfg, r, out, &events, c, &["clean", fleet, "--yes"])?;
    }
    Ok(go)
}

/// #129: after a merge, merge integration into every other open ready PR whose lane isn't working, oldest PR first, in
/// a scratch worktree (#207: never one in review, whose verdict would go stale). A clean merge is pushed (`auto-updated`)
/// only when needed (#285: `update_ready`), else the PR merges as it is; a ready PR's conflict gets an `--update` lane.
pub fn auto_update(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, fleet: &str) -> Result<()> {
    if KICK.get().is_some() {
        return Ok(()); // #304: a kick leaves every other issue to the timers
    }
    let (f, events, ag) = (
        cfg.fleet(fleet)?,
        read(&ctx.state)?,
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
    // A scratch `git worktree`, not a herdr one: no pane, no lane.
    let tree = |a: &[&str]| git(r, &dir, &[&["worktree"][..], a].concat());
    for (_, s) in prs {
        let (n, branch) = (s.issue.to_string(), crate::gate::branch_of(Some(&s), s.issue));
        let from = git(r, &dir, &["rev-parse", &format!("origin/{branch}")])?;
        let _ = tree(&["remove", "--force", &wts]);
        tree(&["add", "-f", "--detach", &wts, &from])?;
        let clean = git(r, &wt, &["merge", "--no-edit", &into]).is_ok();
        let to = git(r, &wt, &["rev-parse", "HEAD"])?;
        // #350: a behind fast-first PR is always caught up: it lands only on the integration its fast checks ran on.
        let mut pr = gh::cached_pr(ctx, fleet, s.issue);
        crate::land::mark(ctx, Some(r), f, (fleet, s.issue), &events, &mut pr);
        let push = to != from && (pr["landing"] == "fast-first" || gh::update_ready(ctx, Some(r), f, fleet));
        // #373: the premerge checks re-run on the updated head before it is pushed; a failure is its fix round.
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
                // #285: the PASS carries to the new head: its merge label goes back on now, so the full run starts.
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

/// The finding for red CI: the first failing check, then the tail of its run's failed-step log.
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
    // The tail lands in events and the fix brief: mask secret-looking runs first.
    format!(
        "FAIL\n\nCI is red at {head}: {what}.\n\n```\n{}\n```\n",
        crate::lanes::mask(&tail.join("\n"))
    )
}

/// (fleet, issue, step) of one action.
type At<'a> = (&'a str, Option<u64>, &'a str);
/// A refused or failed action's code (`REFUSED <code>`, `EXIT`, `ERROR`); `None` when it ran.
type Code = Option<&'static str>;

/// Run one CLI command in process (its own gates and refusals apply); print one line, log `driven`.
/// A refusal or error is logged once per item, step and outcome per hour, and retried next pass; returns its code.
fn act(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, ev: &[Event], at: At, c: &[&str]) -> Result<Code> {
    let (fleet, issue, step) = at;
    let mut buf = vec![];
    let kicks = c[0] == "spawn" && KICK.get().is_none() && cfg.limits.max_parallel_spawns > 0;
    IN_ACT.set(!kicks);
    let res = match issue.filter(|_| kicks) {
        Some(n) => launch(ctx, cfg, r, ev, (fleet, n), c[3], step == "start"),
        None => crate::cli::run(c, ctx, r, &mut buf),
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
    // `clean` with nothing removable (a dirty merged lane) is not an action.
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
    ACTS.with_borrow_mut(|a| {
        a.entry(fleet.into()).or_default().push(
            json!({"issue": issue, "step": step, "outcome": outcome, "code": code, "words": w, "why": why, "kick": kicks}),
        );
    });
    // #438 #604: a launched spawn's outcome is its kick's: `settle` logs it at the pass's end.
    if outcome == "pending" {
        return Ok(None);
    }
    let who = issue.map_or(fleet.to_string(), |n| format!("{fleet} #{n}"));
    let recent = |e: &Event| e.kind == "driven" && e.fleet == fleet && e.issue == issue && ctx.now < e.ts + 3600;
    let same = |e: &Event| e.str("step") == Some(step) && e.str("code") == code;
    // A repeated refusal is silent too (no cron mail every pass).
    if code.is_some() && ev.iter().any(|e| recent(e) && same(e)) {
        return Ok(code);
    }
    writeln!(out, "{who} {step}: {result}")?;
    let d = json!({"step": step, "cmd": format!("herdsman {}", c.join(" ")), "result": result, "code": code});
    emit(&ctx.state, cfg, Event::new(ctx.now, fleet, issue, "driven", d)).map(|()| code)
}
