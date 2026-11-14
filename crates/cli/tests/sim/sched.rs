use crate::model::Model;
use drover::runner::{Out, Runner};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// SplitMix64: the seed's only source of choices.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `0..n` (`n` > 0).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }
    pub fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

struct Abort;

pub struct Proc {
    pub name: String,
    pub args: Vec<String>,
    pub pid: u32,
    /// True time it may run again.
    pub wake: u64,
    /// True time it first ran (its `Ctx.now`).
    pub started: Option<u64>,
    pub done: bool,
    /// Crashed by the scenario: never scheduled again, its pid dead.
    pub killed: bool,
    pub out: String,
    /// The pane whose agent it started and has not logged a `spawn` for yet (I5's quiet points).
    pub starting: Option<String>,
}

impl Proc {
    pub fn live(&self) -> bool {
        !self.done && !self.killed
    }
}

/// One state.db append as the yield hook saw it: who wrote it, when (true time), and the event.
pub struct Wrote {
    pub proc: usize,
    pub at: u64,
    pub ev: drover::events::Event,
    /// The host was busy (load ≥ 25) when it was written.
    pub busy: bool,
}

pub struct World {
    pub now: u64,
    pub turn: Option<usize>,
    pub procs: Vec<Proc>,
    pub rng: Rng,
    pub model: Model,
    pub trace: Vec<String>,
    pub wrote: Vec<Wrote>,
    pub abort: bool,
    pub failure: Option<String>,
    pub root: PathBuf,
    pub step: u64,
    next_pid: u32,
    /// #686: a replay records each violation (once per invariant and subject) with its step and runs on.
    pub keep_going: bool,
    pub violations: Vec<String>,
    /// Every `(agent, pane)` a spawn event logged (I5's index).
    pub spawned: std::collections::BTreeSet<(String, String)>,
    /// `(wrote, merges)` at the last full check of I4's merged sha and I6 (both read only those).
    pub checked: (usize, usize),
}

pub struct Sim {
    pub w: Mutex<World>,
    cv: Condvar,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

fn cost(prog: &str, args: &[String]) -> u64 {
    match (prog, args.first().map(String::as_str), args.get(1).map(String::as_str)) {
        ("herdr", Some("agent"), Some("start")) => 3,
        ("gh", ..) | ("ssh", ..) => 1,
        _ => 0,
    }
}

fn atomic(prog: &str) -> bool {
    matches!(prog, "kill" | "ps")
}

impl Sim {
    pub fn new(seed: u64, now: u64, model: Model, root: PathBuf) -> Arc<Sim> {
        Arc::new(Sim {
            w: Mutex::new(World {
                now,
                turn: None,
                procs: vec![],
                rng: Rng::new(seed),
                model,
                trace: vec![],
                wrote: vec![],
                abort: false,
                failure: None,
                root,
                step: 0,
                next_pid: 40_000,
                keep_going: false,
                violations: vec![],
                spawned: Default::default(),
                checked: (usize::MAX, 0),
            }),
            cv: Condvar::new(),
            threads: Mutex::new(vec![]),
        })
    }

    pub fn lock(&self) -> MutexGuard<'_, World> {
        self.w.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Start process `name` running `drover <args>` at the next step it is picked; its pid.
    pub fn spawn(self: &Arc<Sim>, w: &mut World, name: &str, args: &[&str]) -> u32 {
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        w.next_pid += 1;
        let pid = w.next_pid;
        let me = w.procs.len();
        w.procs.push(Proc {
            name: format!("{name}#{me}"),
            args: args.clone(),
            pid,
            wake: w.now,
            started: None,
            done: false,
            killed: false,
            out: String::new(),
            starting: None,
        });
        let line = format!("spawn {} pid {pid}: drover {}", w.procs[me].name, args.join(" "));
        w.log(line);
        let sim = Arc::clone(self);
        let h = std::thread::Builder::new()
            .name(format!("sim-{me}"))
            .stack_size(16 << 20)
            .spawn(move || sim.body(me, pid, args))
            .expect("thread");
        self.threads.lock().unwrap().push(h);
        pid
    }

    fn body(self: Arc<Sim>, me: usize, pid: u32, args: Vec<String>) {
        if self.wait_turn(me).is_err() {
            return;
        }
        let (ctx, root) = {
            let mut w = self.lock();
            let now = w.now;
            w.procs[me].started = Some(now);
            (crate::scenario::ctx(&w.root, now), w.root.clone())
        };
        let (a, b) = (Arc::clone(&self), Arc::clone(&self));
        drover::sim::set(Some(drover::sim::Hooks {
            now: Box::new(move || a.lock().now),
            pid,
            point: Box::new(move |p| b.point(me, p)),
        }));
        let r = SimRunner {
            sim: Arc::clone(&self),
            me,
        };
        let mut argv = vec!["drover".to_string()];
        argv.extend(args);
        let mut out = vec![];
        let res = std::panic::catch_unwind(AssertUnwindSafe(|| {
            drover::main_with(argv, &ctx, &r, &mut out, &mut std::io::empty())
        }));
        drover::sim::set(None);
        let mut w = self.lock();
        let text = String::from_utf8_lossy(&out).replace(&root.display().to_string(), "$D");
        w.procs[me].out = text.clone();
        w.procs[me].done = true;
        w.procs[me].starting = None;
        if !w.abort {
            crate::invariants::on_exit(&mut w, me, pid);
        }
        match res {
            Ok(code) => {
                let name = w.procs[me].name.clone();
                let first = text.lines().last().unwrap_or_default().to_string();
                w.log(format!("exit {name} {code}: {first:.140}"));
            }
            Err(p) if p.is::<Abort>() => {}
            Err(p) => {
                let why = p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(ToString::to_string))
                    .unwrap_or_default();
                let name = w.procs[me].name.clone();
                w.fail(format!("panic in {name}: {why}"));
            }
        }
        if w.turn == Some(me) {
            w.turn = None;
        }
        drop(w);
        self.cv.notify_all();
    }

    fn wait_turn(&self, me: usize) -> Result<(), ()> {
        let mut w = self.lock();
        loop {
            if w.abort {
                return Err(());
            }
            if w.turn == Some(me) {
                return Ok(());
            }
            w = self.cv.wait(w).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn yield_now(&self, me: usize, secs: u64) {
        {
            let mut w = self.lock();
            if w.abort {
                drop(w);
                return self.unwind();
            }
            w.procs[me].wake = w.now + secs;
            w.turn = None;
        }
        self.cv.notify_all();
        if self.wait_turn(me).is_err() {
            self.unwind();
        }
    }

    fn unwind(&self) {
        if !std::thread::panicking() {
            std::panic::resume_unwind(Box::new(Abort));
        }
    }

    fn point(&self, me: usize, p: &drover::sim::Point) {
        {
            let mut w = self.lock();
            if w.abort {
                return;
            }
            let at = w.now;
            let name = w.procs[me].name.clone();
            match p {
                drover::sim::Point::Append(ev) => {
                    let who = ev.str("agent").map(|a| format!(" {a}")).unwrap_or_default();
                    w.log(format!(
                        "{name} append {} #{}{who} ts={}",
                        ev.kind,
                        ev.issue.unwrap_or(0),
                        ev.ts
                    ));
                    if ev.kind == "spawn" {
                        w.procs[me].starting = None;
                        let (a, p) = (ev.str("agent"), ev.str("pane"));
                        w.spawned
                            .insert((a.unwrap_or_default().into(), p.unwrap_or_default().into()));
                    }
                    let busy = w.model.load >= 25;
                    w.wrote.push(Wrote {
                        proc: me,
                        at,
                        ev: (*ev).clone(),
                        busy,
                    });
                    crate::invariants::on_append(&mut w, me);
                }
                drover::sim::Point::Lease(k) => w.log(format!("{name} lease {k}")),
            }
        }
        self.yield_now(me, 0);
    }

    fn call(self: &Arc<Sim>, me: usize, prog: &str, args: &[String]) -> Out {
        if !atomic(prog) {
            self.yield_now(me, cost(prog, args));
        }
        let mut w = self.lock();
        if w.abort {
            return Out {
                code: 1,
                ..Out::default()
            };
        }
        let o = crate::model::answer(self, &mut w, me, prog, args);
        let root = w.root.display().to_string();
        let line = format!("{prog} {}", args.join(" ")).replace(&root, "$D");
        let short: String = line.chars().take(160).collect();
        let reply = o.stdout.trim().replace('\n', "⏎").replace(&root, "$D");
        let reply: String = reply.chars().take(80).collect();
        let name = w.procs[me].name.clone();
        w.log(format!("{name} {short} → {} {reply}", o.code));
        o
    }

    /// Run until `until` (true time) or `steps` in all, checking the invariants after every step; the failure, if any.
    pub fn run(self: &Arc<Sim>, until: u64, steps: u64) -> Option<String> {
        loop {
            let mut w = self.lock();
            crate::invariants::after_step(&mut w);
            if w.failure.is_some() || w.step >= steps {
                break;
            }
            let ready: Vec<usize> = (0..w.procs.len())
                .filter(|&i| w.procs[i].live() && w.procs[i].wake <= w.now)
                .collect();
            if ready.is_empty() {
                let wake = w.procs.iter().filter(|p| p.live()).map(|p| p.wake).min();
                let next = [wake, crate::scenario::next(&w)].into_iter().flatten().min();
                match next {
                    Some(t) if t <= until => {
                        let dt = t.saturating_sub(w.now);
                        let idle = w.model.live().filter(|p| p.status != "working").count() as u64;
                        w.model.idle += idle * dt;
                        w.now = w.now.max(t);
                        crate::scenario::due(self, &mut w);
                        continue;
                    }
                    _ => break,
                }
            }
            let pick = ready[w.rng.below(ready.len() as u64) as usize];
            w.turn = Some(pick);
            w.step += 1;
            drop(w);
            self.cv.notify_all();
            let mut w = self.lock();
            while w.turn == Some(pick) {
                w = self.cv.wait(w).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
        self.lock().failure.clone()
    }

    /// End the run: every parked thread unwinds (its guards run, answered at once) and is joined.
    pub fn stop(&self) {
        self.lock().abort = true;
        self.cv.notify_all();
        let hs: Vec<_> = std::mem::take(&mut *self.threads.lock().unwrap());
        for h in hs {
            let _ = h.join();
        }
    }
}

impl World {
    pub fn log(&mut self, line: String) {
        let line = line.replace(&self.root.display().to_string(), "$D");
        let line = format!("{:>5} t+{:<6} {line}", self.step, self.now - crate::scenario::T0);
        if std::env::var_os("DROVER_SIM_LIVE").is_some() {
            eprintln!("{line}");
        }
        self.trace.push(line);
    }
    pub fn fail(&mut self, why: String) {
        if self.keep_going {
            let key: Vec<&str> = why.split_whitespace().take(4).collect();
            if !(self.violations.iter()).any(|v| v.split_whitespace().skip(3).take(4).eq(key.iter().copied())) {
                self.log(format!("FAIL {why}"));
                let at = format!("step {} t+{}:", self.step, self.now - crate::scenario::T0);
                self.violations.push(format!("{at} {why}"));
            }
            return;
        }
        if self.failure.is_none() {
            self.log(format!("FAIL {why}"));
            self.failure = Some(why);
        }
    }
    pub fn proc_of_pid(&self, pid: u32) -> Option<&Proc> {
        self.procs.iter().find(|p| p.pid == pid)
    }
}

/// The Runner each process sees: every call is a yield, then the model's answer.
pub struct SimRunner {
    sim: Arc<Sim>,
    me: usize,
}

impl Runner for SimRunner {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        Ok(self.sim.call(self.me, prog, args))
    }
    fn sleep(&self, secs: u64) {
        {
            let mut w = self.sim.lock();
            let name = w.procs[self.me].name.clone();
            w.log(format!("{name} sleep {secs}"));
            if secs == 1 {
                w.model.polled += 1;
            }
        }
        self.sim.yield_now(self.me, secs);
    }
    fn detach(&self, exe: &str, args: &[String], _log: &std::path::Path) -> anyhow::Result<u32> {
        let _ = exe;
        self.sim.yield_now(self.me, 0);
        let mut w = self.sim.lock();
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let name = a.first().copied().unwrap_or("proc").to_string();
        Ok(self.sim.spawn(&mut w, &name, &a))
    }
}
