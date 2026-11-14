use crate::config::Config;
use crate::events::{Event, States};
use crate::hosts::{quote, On};
use crate::lanes::{self, Lane};
use crate::runner::{argv, Out, Runner};
use crate::waiting::Waiting;
use crate::{term, Ctx};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// Each read's stdout by command line (None: it failed), and when the step took them.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub ts: u64,
    pub reads: BTreeMap<String, Option<String>>,
    /// #447: the step's waits, and what they were worked out from (`wait_key`).
    pub waits: Vec<Waiting>,
    pub key: String,
}

pub fn key(prog: &str, args: &[String]) -> String {
    format!("{prog} {}", args.join(" "))
}

/// A remote read with stdin (`shed connect`) by its command line and its requests.
pub fn key_in(prog: &str, args: &[String], input: &str) -> String {
    format!("{}\n{input}", key(prog, args))
}

/// A read through another host (ssh).
pub fn remote(prog: &str) -> bool {
    prog == "ssh"
}

/// #344: the `board` row of the database's snapshots.
pub fn load(ctx: &Ctx) -> Option<Snapshot> {
    crate::db::get(&ctx.state, "board").map(|s| s.1)
}

pub fn out(v: Option<String>) -> Out {
    let code = i32::from(v.is_none());
    Out {
        code,
        stdout: v.unwrap_or_default(),
        ..Out::default()
    }
}

/// What a read the snapshot lacks does: `Collect` (the step's look: ssh is batched, answered from the last snapshot meanwhile; the rest runs and is kept), `Live` (runs), `Local` (a remote one fails, the rest runs).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Miss {
    Collect,
    Live,
    Local,
}

pub struct Snap<'a> {
    r: &'a dyn Runner,
    miss: Miss,
    pub reads: Mutex<BTreeMap<String, Option<String>>>,
    prev: BTreeMap<String, Option<String>>,
    want: Mutex<BTreeMap<String, (String, String)>>,
    pub ts: Option<u64>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl<'a> Snap<'a> {
    /// `status` and `hosts`: the saved snapshot, a remote read it lacks failing; none saved: every read live.
    pub fn read(ctx: &Ctx, r: &'a dyn Runner) -> Self {
        let s = load(ctx);
        let miss = if s.is_some() { Miss::Local } else { Miss::Live };
        Snap::new(r, miss, s.as_ref().map(|s| s.reads.clone()), s.map(|s| s.ts))
    }
    fn new(r: &'a dyn Runner, miss: Miss, reads: Option<BTreeMap<String, Option<String>>>, ts: Option<u64>) -> Self {
        let reads = reads.unwrap_or_default();
        let (reads, prev) = if miss == Miss::Collect {
            (BTreeMap::new(), reads)
        } else {
            (reads, BTreeMap::new())
        };
        Snap {
            r,
            miss,
            reads: Mutex::new(reads),
            prev,
            want: Mutex::default(),
            ts,
        }
    }
    fn fetch(&self) {
        let mut by: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for (k, (m, line)) in std::mem::take(&mut *lock(&self.want)) {
            by.entry(m).or_default().push((k, line));
        }
        for (m, cmds) in by {
            let script: String = (cmds.iter())
                .map(|(_, l)| format!("{l} </dev/null 2>/dev/null; printf '\\036%s\\036' \"$?\"\n"))
                .collect();
            let text = On::new(self.r, Some(&m)).run("sh", &argv(&["-c", &script]));
            let text = text.map(|o| o.stdout).unwrap_or_default();
            let mut parts = text.split('\x1e');
            for (k, _) in cmds {
                let (o, code) = (parts.next(), parts.next());
                let o = o.filter(|_| code == Some("0")).map(String::from);
                lock(&self.reads).insert(k, o);
            }
        }
    }
}

impl Runner for Snap<'_> {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        let k = key(prog, args);
        let there = match args {
            [.., m, _, line] if prog == "ssh" => Some((m.clone(), line.clone())),
            _ => None,
        };
        if let Some(t) = there.filter(|_| self.miss == Miss::Collect) {
            lock(&self.want).insert(k.clone(), t);
            return Ok(out(self.prev.get(&k).cloned().flatten()));
        }
        if let Some(v) = lock(&self.reads).get(&k) {
            return Ok(out(v.clone()));
        }
        if self.miss == Miss::Local && remote(prog) {
            return Ok(out(None));
        }
        let o = self.r.run(prog, args)?;
        if self.miss == Miss::Collect {
            lock(&self.reads).insert(k, (o.code == 0).then(|| o.stdout.clone()));
        }
        Ok(o)
    }
    fn sleep(&self, secs: u64) {
        self.r.sleep(secs);
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> anyhow::Result<Out> {
        if input.is_empty() {
            return self.run(prog, args);
        }
        if !remote(prog) {
            return self.r.run_in(prog, args, input);
        }
        let k = key_in(prog, args, input);
        if let ([.., m, _, line], Miss::Collect) = (args, self.miss) {
            let words: Vec<String> = std::iter::once("%s\\n".into())
                .chain(input.lines().map(String::from))
                .collect();
            let piped = format!("({} | {line})", quote("printf", &words));
            lock(&self.want).insert(k.clone(), (m.clone(), piped));
            return Ok(out(self.prev.get(&k).cloned().flatten()));
        }
        if let Some(v) = lock(&self.reads).get(&k) {
            return Ok(out(v.clone()));
        }
        if self.miss == Miss::Local {
            return Ok(out(None));
        }
        self.r.run_in(prog, args, input)
    }
    fn shed(&self) -> Option<crate::shed::Shed> {
        self.r.shed()
    }
}

/// The board's lanes: the live agents (`list`, every remote host's added) joined with the log.
pub fn lanes(cfg: &Config, r: &dyn Runner, ctx: &Ctx, (events, st): (&[Event], &States), list: &Value) -> Vec<Lane> {
    let mut agents = list["result"]["agents"].as_array().cloned().unwrap_or_default();
    agents.extend(crate::hosts::remote_agents(cfg, r));
    lanes::join(cfg, r, &ctx.worktrees, events, st, &agents)
}

/// #447: what waits are worked out from: the log's length and last row, and each lane's state.
pub fn wait_key(events: &[Event], all: &[Lane]) -> String {
    let last = events.last().map_or(0, |e| e.seq);
    let lanes: Vec<_> = all.iter().map(|l| format!("{}={}", l.name, l.state)).collect();
    format!("{last} {}", lanes.join(" "))
}

/// The step's waits while its snapshot saw this log and these lanes; else worked out now.
pub fn saved_waits(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    snap: &Snapshot,
    (events, all): (&[Event], &[Lane]),
) -> Vec<Waiting> {
    if snap.key == wait_key(events, all) {
        return snap.waits.clone();
    }
    waits(ctx, cfg, r, events, all)
}

/// Each lane's wait reason, from the cached PR reads.
pub fn waits(ctx: &Ctx, cfg: &Config, r: &dyn Runner, events: &[Event], all: &[Lane]) -> Vec<Waiting> {
    let live: Vec<_> = (all.iter())
        .map(|l| json!({"name": l.name, "agent_status": l.state}))
        .collect();
    let w = crate::waiting::scan(ctx, cfg, r, events, &live, false);
    w.into_iter().map(|x| x.1).collect()
}

/// The step's look: the reads the board makes (lanes, waits, now-lines, Host rows), remote ones batched; saved, and returned for the step's own scan (a read it lacks runs live).
pub fn take<'a>(ctx: &Ctx, cfg: &Config, r: &'a dyn Runner) -> Snap<'a> {
    let s = Snap::new(r, Miss::Collect, load(ctx).map(|s| s.reads), Some(ctx.now));
    let events = crate::events::working(&ctx.state).unwrap_or_default();
    if let Ok(list) = term::list(&s) {
        let all = lanes(cfg, &s, ctx, (&events, &crate::events::fold(&events)), &list);
        waits(ctx, cfg, &s, &events, &all);
        for l in &all {
            l.pane.as_ref().map(|p| term::read(&l.on(&s), p));
        }
    }
    crate::timer::services(ctx, cfg, &s, Some(String::new()));
    crate::hosts::cards(cfg, &s);
    crate::outbox::look(&s, &events);
    s.fetch();
    let s = Snap { miss: Miss::Live, ..s };
    let (mut key, mut ws) = (String::new(), vec![]);
    if let Ok(list) = term::list(&s) {
        let all = lanes(cfg, &s, ctx, (&events, &crate::events::fold(&events)), &list);
        (key, ws) = (wait_key(&events, &all), waits(ctx, cfg, &s, &events, &all));
    }
    let reads = lock(&s.reads).clone();
    let snap = Snapshot {
        ts: ctx.now,
        reads,
        waits: ws,
        key,
    };
    let _ = crate::db::put(&ctx.state, "board", ctx.now, &snap);
    s
}
