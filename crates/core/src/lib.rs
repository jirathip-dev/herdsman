pub mod access;
pub mod action;
pub mod amend;
pub mod arch;
pub mod breaker;
pub mod card;
pub mod check;
#[allow(clippy::doc_markdown)]
mod cli;
pub mod cloud;
pub mod config;
pub mod db;
pub mod design;
pub mod devices;
pub mod diagram;
pub mod doctor;
pub mod drive;
pub mod events;
pub mod gate;
pub mod gh;
pub mod heavy;
pub mod hook;
pub mod host;
pub mod hosts;
pub mod hotspots;
pub mod land;
pub mod lanes;
pub mod lesson;
pub mod orchestrator;
pub mod outbox;
pub mod overlap;
pub mod pending;
pub mod pipeline;
pub mod promise;
pub mod propose;
pub mod protection;
pub mod providers;
pub mod proxy;
pub mod queue;
pub mod rename;
pub mod report;
pub mod roots;
pub mod runner;
pub mod runners;
pub mod secret;
pub mod service;
pub mod setup;
pub mod shed;
pub mod sim;
pub mod slo;
pub mod snapshot;
pub mod spawn;
pub mod stall;
pub mod term;
pub mod timer;
pub mod update;
pub mod usage;
pub mod verify;
pub mod waiting;
pub mod webhook;
pub mod webpush;
pub mod why;

use anyhow::{bail, Context, Result};
pub use cli::{main_with, usage_doc, HELP_AGENT_MAX, SERVE, SERVICE};
use config::{home, set_in, set_value, write_atomic, Config};
use events::{emit, working, Event};
use runner::Runner;
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;

/// #235: `drover --version`: the crate version and the git sha it is built from (build.rs).
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("DROVER_SHA"), ")");

#[derive(Clone)]
pub struct Ctx {
    pub config: PathBuf,
    pub state: PathBuf,
    pub worktrees: PathBuf,
    pub homes: std::collections::BTreeMap<&'static str, PathBuf>,
    pub exe: String,
    pub now: u64,
    /// Tree `drover migrate` works on: `DROVER_HOME`, else `$HOME`.
    pub home: PathBuf,
    /// Pre-rename fallbacks in use this run; data commands print them on stderr.
    pub hints: Vec<String>,
}

impl Ctx {
    pub fn from_env() -> Ctx {
        for (k, v) in std::env::vars_os() {
            let new = k
                .to_str()
                .and_then(|k| k.strip_prefix("HERDSMAN_"))
                .map(|k| format!("DROVER_{k}"));
            if let Some(new) = new.filter(|n| std::env::var_os(n).is_none()) {
                std::env::set_var(new, v);
            }
        }
        let env = |k: &str, d: PathBuf| var(k).map_or(d, PathBuf::from);
        let root = var("DROVER_HOME").map_or_else(home, PathBuf::from);
        let mut old = vec![];
        let mut path = |k: &str, d: &str, file: &str| {
            if let Some(p) = var(k) {
                return PathBuf::from(p);
            }
            let (was, new) = (root.join(d).join("herdsman"), root.join(d).join("drover"));
            if was.is_dir() && !new.exists() {
                old.push(was.display().to_string());
                return was.join(file);
            }
            new.join(file)
        };
        let config = path("FLEET_CONFIG", ".config", "fleet.toml");
        let state = path("FLEET_STATE", ".local/state", "events.jsonl");
        let hint = format!(
            "drover: {} is from before the rename: run `drover migrate` first",
            old.join(", ")
        );
        Ctx {
            config,
            state,
            worktrees: env("FLEET_WORKTREES", home().join(roots::DEFAULT.trim_start_matches("~/"))),
            homes: providers::all()
                .filter_map(|p| Some((p.name, p.home()?.resolve(&home()))))
                .collect(),
            exe: std::env::current_exe().map_or("drover".into(), |p| p.to_string_lossy().into()),
            now: now(),
            home: root,
            hints: if old.is_empty() { vec![] } else { vec![hint] },
        }
    }
}

fn var(k: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(k).filter(|v| !v.is_empty())
}

/// `drover migrate` (docs/architecture.md): both moves or neither.
pub fn migrate(home: &std::path::Path, out: &mut dyn Write) -> Result<()> {
    let tree = |d| (home.join(d).join("herdsman"), home.join(d).join("drover"));
    let trees = [".config", ".local/state", ".local/share"].map(tree);
    if let Some((old, new)) = trees
        .iter()
        .find(|(old, new)| new.exists() && old.is_dir() && !same(old, new))
    {
        bail!(
            "both {} and {} exist: nothing moved; keep one",
            old.display(),
            new.display()
        );
    }
    let mut moved: Vec<&(PathBuf, PathBuf)> = vec![];
    for t in &trees {
        let (old, new) = t;
        if new.exists() || !old.is_dir() {
            let why = if new.exists() {
                format!("{} is in place", new.display())
            } else {
                format!("no {}", old.display())
            };
            writeln!(out, "nothing to move: {why}")?;
            continue;
        }
        if let Err(e) = std::fs::rename(old, new) {
            let mut msg = format!("moving {} -> {} failed: {e}", old.display(), new.display());
            for (o, n) in moved.iter().rev() {
                msg += &match std::fs::rename(n, o) {
                    Ok(()) => format!("; rolled back {} -> {}", n.display(), o.display()),
                    Err(e) => format!("; ROLLBACK FAILED {} -> {}: {e}", n.display(), o.display()),
                };
            }
            bail!(msg);
        }
        writeln!(out, "moved {} -> {}", old.display(), new.display())?;
        moved.push(t);
    }
    for (old, new) in moved {
        std::os::unix::fs::symlink(new, old)?;
    }
    Ok(())
}

fn same(a: &std::path::Path, b: &std::path::Path) -> bool {
    a.canonicalize().is_ok_and(|a| b.canonicalize().is_ok_and(|b| a == b))
}

/// #456: a repo's `.drover/…` path, or its pre-rename `.herdsman/…` twin while only that exists (one release).
pub fn repo_path(p: &std::path::Path) -> PathBuf {
    let old = |c| if c == ".drover" { ".herdsman".as_ref() } else { c };
    let old: PathBuf = p.iter().map(old).collect();
    if !p.exists() && old.exists() {
        old
    } else {
        p.to_path_buf()
    }
}

/// The same for a read off the local disk (git, the API, another host): the first folder that answers.
pub fn repo_read<T>(name: &str, read: impl Fn(&str) -> Option<T>) -> Option<T> {
    [".drover", ".herdsman"]
        .iter()
        .find_map(|d| read(&format!("{d}/{name}")))
}

/// Seconds since the epoch.
pub fn now() -> u64 {
    if let Some(t) = sim::now() {
        return t;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Apply one queue change and log it (`drover queue`, and the page's ▲/▼ with `via`); the new order as shown.
pub fn queue_change(ctx: &Ctx, cfg: &Config, fleet: &str, d: Value, via: Option<&str>) -> Result<String> {
    cfg.fleet(fleet)?;
    let events = working(&ctx.state)?;
    let mut q = queue::replay(&events, fleet);
    queue::apply(&mut q, &d)?;
    for b in queue::refs(&d["on"])
        .iter()
        .filter(|b| d["op"] == "block" && !b.contains('/'))
    {
        cfg.fleet(b.split('#').next().unwrap_or_default())?;
    }
    let n = d["issues"][0]
        .as_u64()
        .filter(|_| d["op"] == "block" || d["op"] == "unblock");
    let s = n.and_then(|n| events::fold(&events).remove(&(fleet.into(), n)));
    let on = q.iter().find(|i| Some(i.issue) == n).map(|i| i.blockers.clone());
    let held = s.and_then(|s| s.active_lane().map(|l| json!({"agent": l.1, "on": on})));
    emit(
        &ctx.state,
        cfg,
        Event::new(ctx.now, fleet, None, "queue", with_via(d, via)),
    )?;
    if let Some(h) = held {
        emit(&ctx.state, cfg, Event::new(ctx.now, fleet, n, "held", h))?;
    }
    let order: Vec<String> = q.iter().map(|i| format!("#{}", i.issue)).collect();
    let order = if order.is_empty() {
        "empty".into()
    } else {
        order.join(" ")
    };
    Ok(format!("{fleet} queue: {order}"))
}

/// `drover set` (and the page's model/effort dropdowns, with `via`): `wanted` is (key, new value).
pub fn set(
    ctx: &Ctx,
    out: &mut dyn Write,
    fleet: &str,
    role: &str,
    wanted: Vec<(&str, String)>,
    via: Option<&str>,
) -> Result<()> {
    let text = std::fs::read_to_string(&ctx.config)?;
    let cfg = Config::parse(&text)?;
    let cur = cfg.fleet(fleet)?.role(role)?.clone();
    if wanted.is_empty() {
        bail!("nothing to set: pass --harness, --model, --profile or --effort");
    }
    if let Some((k, v)) = wanted
        .iter()
        .find(|(_, v)| v.is_empty() || v.chars().any(|c| c == '"' || c == '\\' || c.is_control()))
    {
        return gate::refused(
            "BAD_VALUE",
            format!("{k} {v:?}: empty, or has a quote, backslash or control char"),
        );
    }
    if let Some((_, m)) = wanted.iter().find(|(k, m)| *k == "model" && !cfg.models.contains(m)) {
        return gate::refused("BAD_VALUE", format!("model {m:?} is not in `models` {:?}", cfg.models));
    }
    let mut new = text.clone();
    let mut changes = vec![];
    for (key, val) in wanted {
        let old = match key {
            "harness" => Some(cur.harness.clone()),
            "model" => cur.model.clone(),
            "profile" => cur.profile.clone(),
            _ => cur.effort.clone(),
        };
        let old = old.unwrap_or_else(|| "-".into());
        writeln!(out, "{fleet}.{role} {key}: {old} -> {val}")?;
        if old != val {
            new = set_value(&new, fleet, role, key, &val)?;
            changes.push(with_via(json!({"role": role, "key": key, "old": old, "new": val}), via));
        }
    }
    let new_cfg = Config::parse(&new)?;
    if new != text {
        write_atomic(&ctx.config, &new)?;
    }
    for c in changes {
        emit(&ctx.state, &new_cfg, Event::new(ctx.now, fleet, None, "set", c))?;
    }
    Ok(())
}

fn with_via(mut d: Value, via: Option<&str>) -> Value {
    if let Some(v) = via {
        d["via"] = json!(v);
    }
    d
}

/// `drover pause|resume <fleet>` (and the page's button, with `via`): `paused = true|false`.
pub fn pause(ctx: &Ctx, out: &mut dyn Write, fleet: &str, paused: bool, via: Option<&str>) -> Result<()> {
    let text = std::fs::read_to_string(&ctx.config)?;
    let cfg = Config::parse(&text)?;
    let word = if paused { "paused" } else { "resumed" };
    if cfg.fleet(fleet)?.paused == paused {
        writeln!(out, "{fleet}: already {word}")?;
        return Ok(());
    }
    let new = set_in(&text, &format!("[fleet.{fleet}]"), "paused", &paused.to_string())?;
    let new_cfg = Config::parse(&new)?;
    write_atomic(&ctx.config, &new)?;
    emit(
        &ctx.state,
        &new_cfg,
        Event::new(
            ctx.now,
            fleet,
            None,
            if paused { "pause" } else { "resume" },
            with_via(json!({}), via),
        ),
    )?;
    writeln!(out, "{fleet}: {word}")?;
    Ok(())
}

const OWNER_WORDS: &str = "main production prod prods secret secrets token tokens delete deleted deletes deleting \
    deletion spend spends spending spent cost costs costly dependency dependencies scope scopes";

fn owner_topic(text: &str) -> bool {
    let text = text.to_lowercase();
    let mut words = text.split(|c: char| !c.is_alphanumeric());
    text.contains('$') || words.any(|w| OWNER_WORDS.split_whitespace().any(|o| o == w))
}

/// `drover ask`: log a `needs-decision` for the asking lane (`DROVER_LANE`, else none), notify, print its id. #541: a lane's ask (not a conductor's) goes to its repo's conductor thread first (`routed`, no push); with none, or the conductor not reached, to the owner.
pub fn ask(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (fleet, issue): (&str, Option<u64>),
    (q, opts): (&str, &[String]),
    (rec, kind, thread): (Option<usize>, &str, Option<&str>),
) -> Result<()> {
    cfg.fleet(fleet)?;
    if kind == "routine" && rec.is_none() {
        return gate::refused("NO_RECOMMENDATION", "a routine ask needs --recommend <n>");
    }
    if kind == "routine" && owner_topic(&format!("{q} {}", opts.join(" "))) {
        return gate::refused("OWNER_TOPIC", "this ask touches an owner topic; ask it as --kind owner");
    }
    if q.trim().is_empty() || opts.len() > 4 || rec.is_some_and(|n| n == 0 || n > opts.len()) {
        return gate::refused(
            "BAD_VALUE",
            "an ask is a non-empty question, at most 4 options, --recommend one of them",
        );
    }
    let events = working(&ctx.state)?;
    if let Some(t) = thread {
        orchestrator::fleet_of(&events, t)?;
    }
    let lane = std::env::var("DROVER_LANE").unwrap_or_default();
    let at = |e: &Event| {
        format!(
            "{}:{}:{}",
            e.fleet,
            e.issue.map_or("-".into(), |n| n.to_string()),
            e.str("role").unwrap_or_default()
        )
    };
    let starts = |e: &&Event| matches!(e.kind.as_str(), "spawn" | "spawn-started");
    let spawned = events.iter().rev().filter(starts).find(|e| at(e) == lane);
    let (a, h) = spawned.map_or((None, None), |e| (e.str("agent"), e.str("host")));
    let mut d = json!({"question": q, "options": opts, "recommend": rec, "agent": a, "host": h, "kind": kind});
    if let Some(t) = thread {
        d["thread"] = json!(t);
    }
    let from_lane = lane
        .rsplit(':')
        .next()
        .is_some_and(|r| !r.is_empty() && !orchestrator::is_role(r));
    let routed = orchestrator::route(cfg, &events, fleet).filter(|_| from_lane && thread.is_none());
    if let Some(t) = routed {
        d["routed"] = json!(t);
    }
    let ev = Event::new(ctx.now, fleet, issue, "needs-decision", d);
    let ev = events::ask(&ctx.state, cfg, vec![ev], None)?
        .pop()
        .context("ask not logged")?;
    let id = ev.str("id").unwrap_or_default();
    if let Some(t) = routed.filter(|t| orchestrator::attached(&events, t).is_none()) {
        if let Err(e) = orchestrator::lane_ask(ctx, cfg, r, t, &ev) {
            orchestrator::to_owner(ctx, cfg, t, &ev, None, &format!("orchestrator not reached: {e:#}"))?;
        }
    }
    Ok(writeln!(out, "{id}")?)
}

/// #541: how long the owner may change a conductor's answer, in seconds (the board reads it).
pub const CHANGE: u64 = config::ORCHESTRATOR_CHANGE_MINUTES * 60;

fn chosen<'a>(ask: &'a Event, choice: &'a str) -> &'a str {
    let n = choice.parse::<usize>().ok();
    n.and_then(|n| ask.val("options").get(n.checked_sub(1)?)?.as_str())
        .unwrap_or(choice)
}

/// #541: `drover answer --basis`, run by thread `t`'s conductor: answered `by: conductor-<t>` only if the ask is a lane ask on its repo, names no owner topic, and `basis` quotes the issue's spec (3 words or more, verbatim) or names an answered decision; else `OWNER_ASK`, and a lane ask on its repo is put to the owner in the thread.
pub fn orchestrator_answer(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (fleet, id): (&str, &str),
    choice: &str,
    (t, basis): (&str, &str),
) -> Result<()> {
    let events = working(&ctx.state)?;
    let by = format!("orchestrator-{t}");
    let ask = the_ask(&events, (fleet, id), &|e| in_thread(e, &by))?;
    let mine = ask.str("routed").is_some() || crate::design::is_ask(ask) || ask.data.get("extend").is_some();
    if !mine || orchestrator::fleet_of(&events, t)? != ask.fleet {
        return gate::refused("OWNER_ASK", format!("{id} is no lane ask on {t}'s repo; ask the owner"));
    }
    let text = chosen(ask, choice);
    let flat = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let quote = flat(basis.trim_matches(|c: char| c.is_whitespace() || "\"'>“”".contains(c)));
    let decided = |e: &Event| e.kind == "decision" && e.fleet == fleet && e.str("id") == Some(&quote);
    let in_spec = || -> Result<bool> {
        let (Some(n), true) = (ask.issue, quote.split(' ').count() >= 3) else {
            return Ok(false);
        };
        let v = gh::spec(ctx, r, cfg.fleet(fleet)?, fleet, n, false)?;
        Ok(flat(v["body"].as_str().unwrap_or_default()).contains(&quote))
    };
    let why = if owner_topic(&format!("{} {text}", ask.str("question").unwrap_or_default())) {
        "it names an owner topic"
    } else if ask.data.get("extend").is_some() && text == "park" {
        "parking is the owner's"
    } else if quote.is_empty() {
        "no --basis"
    } else if quote != id && events.iter().any(decided) || in_spec()? {
        ""
    } else {
        "--basis is neither a verbatim quote of the issue's spec nor an answered decision id"
    };
    if !why.is_empty() {
        let opts = ask.val("options").as_array().into_iter().flatten();
        let rec = (opts.zip(1..)).find(|o| o.0 == text).map(|o| o.1);
        orchestrator::to_owner(ctx, cfg, t, ask, rec, why)?;
        return gate::refused("OWNER_ASK", format!("{why}; ask the owner"));
    }
    answer_with(ctx, cfg, r, out, ask, choice, (&by, None), (Some(&quote), None))
}

/// #817: `drover extend` by thread `t`'s orchestrator: the owner's `typed` line, else its extend ask answered on `basis`.
#[allow(clippy::too_many_arguments)]
pub fn orchestrator_extend(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (fleet, n): (&str, u64),
    spawn: bool,
    t: &str,
    (typed, basis): (Option<&str>, Option<&str>),
) -> Result<()> {
    let events = working(&ctx.state)?;
    if orchestrator::fleet_of(&events, t)? != fleet {
        return gate::refused("OWNER_ASK", format!("#{n} is not on {t}'s repo; ask the owner"));
    }
    gate::held(ctx, fleet, n)?;
    let card = events.iter().rfind(|e| e.is("needs-human", fleet, n));
    if let Some(line) = typed {
        let said = |m: &Event| m.kind == "thread-msg" && m.str("thread") == Some(t) && m.str("from") == Some("owner");
        let after = |m: &Event| said(m) && m.str("text") == Some(line) && card.is_some_and(|c| m.seq > c.seq);
        if !events.iter().any(after) {
            return gate::refused(
                "NOT_TYPED",
                format!("no owner message in {t} says that line after #{n}'s card"),
            );
        }
        return gate::needs_human(ctx, cfg, r, out, (fleet, n), "extend", (spawn, TYPED, None));
    }
    let open = |e: &&Event| e.is("needs-decision", fleet, n) && e.data.get("extend").is_some();
    let ask = events
        .iter()
        .rfind(open)
        .filter(|a| events::closed(&events, a).is_none());
    let ask = if let Some(a) = ask {
        a.clone()
    } else {
        let why = card.and_then(|c| c.str("reason")).unwrap_or("needs-human");
        let q = format!("One more round for #{n}? ({why})");
        let d = json!({"question": q, "options": ["one more round", "park"], "recommend": 1,
            "kind": "owner", "extend": true, "spawn": spawn});
        let ev = Event::new(ctx.now, fleet, Some(n), "needs-decision", d);
        db::ask(&ctx.state, vec![ev], None)?
            .and_then(|mut v| v.pop())
            .context("ask not logged")?
    };
    let id = ask.str("id").unwrap_or_default();
    orchestrator_answer(ctx, cfg, r, out, (fleet, id), "1", (t, basis.unwrap_or_default()))
}

pub(crate) const TYPED: &str = "owner (typed)";

/// #584: `drover answer --typed <line>`: the owner's words, accepted only when `line` is the owner's message in the ask's thread (its own, or the one its lane ask was put to the owner in) after the ask; else `NOT_TYPED`.
pub fn answer_typed(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn runner::Runner,
    out: &mut dyn Write,
    (fleet, id): (&str, &str),
    (line, on): (&str, Option<&str>),
    (choice, note): (Option<&str>, Option<&str>),
) -> Result<()> {
    let events = working(&ctx.state)?;
    let is = |e: &&Event, k: &str| e.kind == k && e.fleet == fleet && e.str("id") == Some(id);
    let on = on.filter(|t| orchestrator::fleet_of(&events, t).ok() == Some(fleet));
    let asked: Vec<(&str, u64)> = (events.iter())
        .filter(|e| is(e, "needs-decision") || is(e, "ask-owner"))
        .flat_map(|e| {
            let own = on.filter(|_| crate::design::is_ask(e));
            e.str("thread").into_iter().chain(own).map(|t| (t, e.seq))
        })
        .collect();
    let said: Vec<&Event> = (events.iter())
        .filter(|e| e.kind == "thread-msg" && e.str("from") == Some("owner") && e.str("text") == Some(line))
        .collect();
    let at = |m: &Event| {
        (asked.iter())
            .filter(|a| Some(a.0) == m.str("thread"))
            .map(|a| a.1)
            .min()
    };
    let there: Vec<_> = said.iter().filter_map(|m| Some((m.seq, at(m)?))).collect();
    let why = if asked.is_empty() {
        format!("{id} was asked in no thread")
    } else if said.is_empty() {
        "no owner message in a thread says that line".into()
    } else if there.is_empty() {
        let t: std::collections::BTreeSet<_> = said.iter().filter_map(|m| m.str("thread")).collect();
        let t = t.into_iter().collect::<Vec<_>>().join(", ");
        format!("that line is in {t}, not {id}'s thread")
    } else if !there.iter().any(|(m, a)| m > a) {
        format!("that line came before {id} was asked")
    } else {
        String::new()
    };
    if !why.is_empty() {
        return gate::refused("NOT_TYPED", why);
    }
    let choice = choice.unwrap_or(line);
    if note.is_some_and(|n| n.trim().is_empty() || n.chars().any(char::is_control)) {
        return gate::refused("BAD_VALUE", "a note is one line of text");
    }
    let typed_in = |e: &Event| {
        let own = on.filter(|_| crate::design::is_ask(e));
        let t = |m: &&&Event| m.str("thread") == e.str("thread") || m.str("thread") == own;
        said.iter().filter(t).any(|m| m.seq > e.seq)
    };
    let ask = the_ask(&events, (fleet, id), &typed_in)?;
    let text = chosen(ask, choice);
    let revise = note.filter(|_| text == "revise").map(|n| format!("revise: {n}"));
    let said = revise.as_deref().unwrap_or(choice);
    answer_with(ctx, cfg, r, out, ask, said, (TYPED, None), (None, note))?;
    let n = ask.issue.filter(|_| text == "approve");
    n.zip(note)
        .map_or(Ok(()), |(n, note)| amend::amend(ctx, cfg, r, out, (fleet, n), id, note))
}

/// #584: `drover ask withdraw`: the ask's own thread's orchestrator (`by`) closes an ask it replaced (`for`).
pub fn withdraw(
    ctx: &Ctx,
    cfg: &Config,
    out: &mut dyn Write,
    (fleet, id): (&str, &str),
    by: &str,
    new: Option<&str>,
) -> Result<()> {
    let events = working(&ctx.state)?;
    let is = |e: &&Event, q: &str| e.kind == "needs-decision" && e.fleet == fleet && e.str("id") == Some(q);
    let ask = the_ask(&events, (fleet, id), &|e| in_thread(e, by))?;
    if let Some(q) = new.filter(|q| !events.iter().any(|e| is(&e, q))) {
        return gate::refused("NO_SUCH_ASK", format!("{fleet} has no ask {q}"));
    }
    let t = ask.str("thread").unwrap_or_default();
    if t.is_empty() || by.strip_prefix("orchestrator-").unwrap_or(by) != t {
        return gate::refused(
            "BAD_VALUE",
            format!("only {id}'s own thread's orchestrator withdraws it"),
        );
    }
    if let Some(w) = events::closed(&events, ask) {
        return gate::refused("ALREADY_ANSWERED", format!("{id} is closed: {w}"));
    }
    let d = json!({"id": id, "ask_seq": ask.seq, "by": by, "for": new});
    emit(
        &ctx.state,
        cfg,
        Event::new(ctx.now, fleet, ask.issue, "ask-withdrawn", d),
    )?;
    let to = new.map_or(String::new(), |q| format!(" → {q}"));
    Ok(writeln!(out, "{fleet} {id}: withdrawn{to}")?)
}

/// `drover answer` (and the page's needs-you card, with `via`): log the decision, prompt the asking agent.
pub fn answer(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn runner::Runner,
    out: &mut dyn Write,
    (fleet, id): (&str, &str),
    choice: &str,
    (by, via): (&str, Option<&str>),
) -> Result<()> {
    let events = working(&ctx.state)?;
    let ask = the_ask(&events, (fleet, id), &|e| in_thread(e, by))?;
    answer_with(ctx, cfg, r, out, ask, choice, (by, via), (None, None))
}

fn the_ask<'a>(events: &'a [Event], (fleet, id): (&str, &str), mine: &dyn Fn(&Event) -> bool) -> Result<&'a Event> {
    let is = |e: &&Event| e.kind == "needs-decision" && e.fleet == fleet && e.str("id") == Some(id);
    let all: Vec<&Event> = events.iter().filter(is).collect();
    let picked: Vec<&Event> = all.iter().copied().filter(|e| mine(e)).collect();
    match (all.as_slice(), picked.as_slice()) {
        ([], _) => gate::refused("NO_SUCH_ASK", format!("{fleet} has no ask {id}")),
        ([a], _) | (_, [a]) => Ok(a),
        _ => {
            let seqs: Vec<String> = all.iter().map(|e| e.seq.to_string()).collect();
            let why = format!(
                "{id} names the asks at seqs {}; answer it from its own thread",
                seqs.join(", ")
            );
            gate::refused("AMBIGUOUS_ASK", why)
        }
    }
}

fn in_thread(e: &Event, by: &str) -> bool {
    by.strip_prefix("orchestrator-")
        .is_some_and(|t| e.str("thread") == Some(t))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn answer_with(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn runner::Runner,
    out: &mut dyn Write,
    ask: &Event,
    choice: &str,
    (by, via): (&str, Option<&str>),
    (basis, note): (Option<&str>, Option<&str>),
) -> Result<()> {
    let events = working(&ctx.state)?;
    let (fleet, id) = (ask.fleet.as_str(), ask.str("id").unwrap_or_default());
    let last = (events.iter().rev()).find(|e| e.kind == "decision" && events::names(e, ask));
    let orchestrator = |b: &str| b.starts_with("orchestrator-");
    let change = |d: &&Event| {
        d.str("by").is_some_and(orchestrator)
            && !orchestrator(by)
            && ctx.now < d.ts + cfg.limits.orchestrator_change_minutes * 60
    };
    let handoff = ask.data.get("handoff").is_some();
    if let Some(d) = last.filter(|d| !handoff && d.str("by") != Some("default") && !change(d)) {
        return gate::refused(
            "ALREADY_ANSWERED",
            format!(
                "{id} was answered by {}: {}",
                d.str("by").unwrap_or("?"),
                d.str("choice").unwrap_or("?")
            ),
        );
    }
    let text = chosen(ask, choice);
    if text.trim().is_empty() || text.chars().any(char::is_control) || text.chars().count() > 2000 {
        let why = "an answer is an option number or one line of text, at most 2,000 chars";
        return gate::refused("BAD_VALUE", why);
    }
    if handoff {
        let (to, fresh) = orchestrator::land(ctx, cfg, r, ask, choice)?;
        let text = match last.and_then(|d| d.str("choice")) {
            Some(first) if !fresh => first,
            _ => text,
        };
        if fresh {
            let d = with_via(
                json!({"id": id, "ask_seq": ask.seq, "choice": text, "by": by, "to": to}),
                via,
            );
            emit(&ctx.state, cfg, Event::new(ctx.now, fleet, None, "decision", d))?;
        }
        return Ok(writeln!(out, "{fleet} {id}: {text} -> {to}")?);
    }
    let design = crate::design::is_ask(ask);
    if design {
        crate::design::decide(ctx, cfg, r, ask, text, by)?;
    }
    let extend = ask.issue.filter(|_| ask.data.get("extend").is_some());
    if let Some(n) = extend.filter(|_| text == "park") {
        gate::park(ctx, cfg, r, out, (fleet, n), by, None)?;
    } else if let Some(n) = extend.filter(|_| last.is_none()) {
        let spawn = *ask.val("spawn") != json!(false);
        gate::needs_human(ctx, cfg, r, out, (fleet, n), "extend", (spawn, by, None))?;
    }
    let mut d =
        json!({"id": id, "ask_seq": ask.seq, "choice": text, "by": by, "basis": basis, "note": note, "via": via});
    d.as_object_mut().iter_mut().for_each(|m| m.retain(|_, v| !v.is_null()));
    emit(&ctx.state, cfg, Event::new(ctx.now, fleet, ask.issue, "decision", d))?;
    if ask.data.get("respawn_loop").is_some() && text == "park" {
        emit(
            &ctx.state,
            cfg,
            Event::new(ctx.now, fleet, ask.issue, "parked", json!({"by": by})),
        )?;
    }
    if let (Some(b), Some(t)) = (basis, by.strip_prefix("orchestrator-")) {
        let at = ask.issue.map_or(String::new(), |n| format!(" for #{n}"));
        let text = format!("answered {id}{at}: {text}, because {b}");
        let d = json!({"thread": t, "from": "system", "text": text, "change": id});
        emit(&ctx.state, cfg, Event::new(ctx.now, fleet, None, "thread-msg", d))?;
    }
    if extend.is_some() {
        return Ok(writeln!(out, "{fleet} {id}: {text}")?);
    }
    let why = via.map_or("answer", |_| "board");
    if let Some(n) = ask.issue {
        let id = format!("{id}-{by}");
        let _ = crate::drive::kick_once(ctx, r, (fleet, n), why, Some(&id));
    }
    let mins = cfg.limits.ask_default_minutes;
    let note = (by == "default").then(|| format!(" (default after {mins} min, nobody replied)"));
    let msg = format!("DECISION {id}: {text}{}", note.unwrap_or_default());
    if let Some(t) = ask
        .str("thread")
        .filter(|t| orchestrator::attached(&events, t).is_some())
    {
        return Ok(writeln!(out, "{fleet} {id}: {text} -> {t}")?);
    }
    if let Some(t) = ask.str("thread").filter(|_| by != TYPED) {
        let agent = orchestrator::say(ctx, cfg, r, (t, &msg, &[]), (by, true))?;
        let q = ask.str("question").unwrap_or_default();
        orchestrator::flow(ctx, cfg, t, &format!("answered {id}: {q} → {text}"))?;
        return Ok(writeln!(out, "{fleet} {id}: {text} -> {agent}")?);
    }
    if design {
        return Ok(writeln!(out, "{fleet} {id}: {text}")?);
    }
    let Some(agent) = deliver(ctx, cfg, r, ask, &msg, true)? else {
        let merged = events::closed(&events, ask) == Some("closed: merged");
        let open = ask.issue.filter(|_| !merged);
        if let Some(n) = open {
            let st = events::fold(&events).remove(&(fleet.to_string(), n));
            let role = st
                .and_then(|s| s.build)
                .unwrap_or(cfg.fleet(fleet)?.flow().start().into());
            crate::pending::record(ctx, (fleet, n), "answer", &role, events.last().map(|e| e.seq))?;
        }
        let next = open.map(|n| format!("; the next lane on #{n} gets it in its brief"));
        let gone = format!("recorded; asking agent gone, not prompted{}", next.unwrap_or_default());
        return Ok(writeln!(out, "{fleet} {id}: {text} {gone}")?);
    };
    Ok(writeln!(out, "{fleet} {id}: {text} -> {agent}")?)
}

fn answer_lane<'a>(events: &'a [Event], ask: &Event) -> Option<&'a Event> {
    let open = |sp: &&Event| {
        let lane = ["lane-closed", "lane-killed", "lane-reaped"];
        let ends = |e: &Event| lane.contains(&e.kind.as_str()) && events::ends(e, sp);
        let mut later = events.iter().skip_while(|e| !std::ptr::eq(*e, *sp));
        !later.any(|e| e.fleet == ask.fleet && (ends(e) || e.kind == "merged" && e.issue == ask.issue))
    };
    let on_issue = |e: &&Event| ask.issue.is_some_and(|n| e.is("spawn", &ask.fleet, n));
    let own = ask.str("agent").and_then(|a| events::last_spawn(events, a));
    own.filter(open).or_else(|| events.iter().rfind(on_issue)).filter(open)
}

/// #325: type the answer `msg` into its lane, Enter; log `answer-delivered`, or (`first`: it must land) `-undelivered`.
pub fn deliver(ctx: &Ctx, cfg: &Config, r: &dyn Runner, ask: &Event, msg: &str, first: bool) -> Result<Option<String>> {
    let events = working(&ctx.state)?;
    let sp = answer_lane(&events, ask);
    let was = ask.str("agent").and_then(|a| events::last_spawn(&events, a));
    let agent = sp.or(was).and_then(|e| e.str("agent")).unwrap_or_default();
    let on = &crate::hosts::On::of(r, sp.or(was));
    let wt = sp.map_or(was.and_then(|e| e.str("worktree")), |_| None);
    let all = wt.and_then(|_| crate::host::agents(on).ok()).unwrap_or_default();
    let inside = crate::host::live_in(&all, "", std::path::Path::new(wt.unwrap_or_default()));
    let live = inside.into_iter().find(|a| a["name"] == agent);
    let pane = sp.and_then(|e| e.str("pane")).or_else(|| live?["pane_id"].as_str());
    let typed = |p| {
        term::awake(on, agent)
            .and_then(|()| term::prompt(on, agent, msg))
            .and_then(|_| term::keys(on, p, "Enter"))
    };
    let why = match pane.map(|p| (p, typed(p))) {
        None => "no open lane for this ask".into(),
        Some((_, Err(e))) => format!("{e:#}"),
        Some((p, Ok(_))) if first && !term::landed(on, agent, p, msg) => "the agent did not pick it up".into(),
        Some(_) => String::new(),
    };
    let kind = match (why.is_empty(), live) {
        (false, _) => "answer-undelivered",
        (_, Some(_)) => "answer-delivered-live",
        _ => "answer-delivered",
    };
    let d = json!({"id": ask.str("id"), "agent": pane.and(Some(agent)), "reason": why, "text": msg});
    if first || why.is_empty() {
        emit(&ctx.state, cfg, Event::new(ctx.now, &ask.fleet, ask.issue, kind, d))?;
    }
    Ok(why.is_empty().then(|| agent.into()))
}

/// #325: answers whose last delivery failed, newest first (one per ask).
pub fn undelivered<'a>(events: &'a [Event], fleet: &str) -> Vec<&'a Event> {
    let tries = |e: &&Event| e.fleet == fleet && e.kind.starts_with("answer-");
    let mut seen = std::collections::BTreeSet::new();
    let last = events.iter().rev().filter(tries).filter(|e| seen.insert(e.str("id")));
    last.filter(|e| e.kind == "answer-undelivered").collect()
}

/// `drover asks`: the open asks (`all`: closed ones too, with why), oldest first, with age.
pub fn asks(ctx: &Ctx, out: &mut dyn Write, fleet: Option<&str>, as_json: bool, all: bool) -> Result<()> {
    let events = events::working(&ctx.state)?;
    let shown = |e: &&Event| fleet.is_none_or(|f| f == e.fleet) && (all || events::closed(&events, e).is_none());
    let open: Vec<&Event> = events
        .iter()
        .filter(|e| e.kind == "needs-decision")
        .filter(shown)
        .collect();
    if as_json {
        let mut rows = Vec::new();
        for e in &open {
            let mut v = serde_json::to_value(e)?;
            v["closed"] = json!(events::closed(&events, e));
            rows.push(v);
        }
        writeln!(out, "{}", serde_json::to_string_pretty(&rows)?)?;
    }
    for e in open.iter().filter(|_| !as_json) {
        let at = e.issue.map_or("-".into(), |n| format!("#{n}"));
        let s = |k| e.str(k).unwrap_or_default();
        let why = events::closed_row(&events, e).map_or(String::new(), |w| format!(" [{w}]"));
        writeln!(
            out,
            "{} {} {at} {} {}: {}{why}",
            e.fleet,
            s("id"),
            events::age(ctx.now.saturating_sub(e.ts)),
            s("agent"),
            s("question")
        )?;
    }
    Ok(())
}
