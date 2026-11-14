use crate::config::Limits;
use crate::runner::{in_request, Out, Runner};
use anyhow::Result;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

/// #859: where a read goes: shed, another host (ssh), the rest (git, gh, date).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Shed,
    Remote,
    Other,
}

impl Source {
    pub const ALL: [Source; 3] = [Source::Shed, Source::Remote, Source::Other];

    fn of(prog: &str) -> Source {
        match prog {
            "ssh" => Source::Remote,
            "shed" => Source::Shed,
            _ => Source::Other,
        }
    }

    /// Its refresh interval in seconds (the rest: 30 s, the board's old cache age).
    pub fn every(self, l: &Limits) -> u64 {
        match self {
            Source::Shed => l.serve_shed_secs,
            Source::Remote => l.serve_remote_secs,
            Source::Other => 30,
        }
    }

    fn name(self) -> Option<&'static str> {
        match self {
            Source::Shed => Some("shed"),
            Source::Remote => Some("remote"),
            Source::Other => None,
        }
    }
}

type Key = (String, Vec<String>, String);

struct Read {
    out: Option<Out>,
    at: u64,
    asked: u64,
}

pub struct Fresh<'a> {
    pub r: &'a dyn Runner,
    pub defer: bool,
    reads: Mutex<BTreeMap<Key, Read>>,
    host: Mutex<u64>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl<'a> Fresh<'a> {
    pub fn new(r: &'a dyn Runner) -> Self {
        Fresh {
            r,
            defer: false,
            reads: Mutex::default(),
            host: Mutex::default(),
        }
    }

    fn deferred(&self) -> bool {
        self.defer && in_request()
    }

    fn last(&self, prog: &str, args: &[String], input: &str) -> Out {
        let mut m = lock(&self.reads);
        let key = (prog.to_string(), args.to_vec(), input.to_string());
        let r = m.entry(key).or_insert(Read {
            out: None,
            at: 0,
            asked: 0,
        });
        r.asked = crate::now();
        r.out.clone().unwrap_or_else(|| Out {
            code: 1,
            stderr: format!("{prog}: not read yet (the board reads it in the background)"),
            ..Out::default()
        })
    }

    /// Read `src`'s reads older than `every` s (a new one at once) at `now`; drop those unasked for 10 min.
    pub fn refresh(&self, src: Source, every: u64, now: u64) -> usize {
        let due: Vec<Key> = lock(&self.reads)
            .iter()
            .filter(|(k, r)| Source::of(&k.0) == src && (r.out.is_none() || r.at + every <= now))
            .map(|(k, _)| k.clone())
            .collect();
        for k in &due {
            let out = match k.2.as_str() {
                "" => self.r.run(&k.0, &k.1),
                input => self.r.run_in(&k.0, &k.1, input),
            };
            let out = out.unwrap_or_else(|e| Out {
                code: 1,
                stderr: format!("{e:#}"),
                ..Out::default()
            });
            if let Some(r) = lock(&self.reads).get_mut(k) {
                (r.out, r.at) = (Some(out), now);
            }
        }
        lock(&self.reads).retain(|_, r| r.asked + 600 >= now);
        due.len()
    }

    /// The host sample was taken at `now`.
    pub fn sampled(&self, now: u64) {
        *lock(&self.host) = now;
    }

    /// Each source's `refreshed_at`: its oldest read's time, and the host sample's.
    pub fn refreshed(&self) -> BTreeMap<String, u64> {
        let mut m = BTreeMap::new();
        for (k, r) in lock(&self.reads).iter().filter(|r| r.1.out.is_some()) {
            if let Some(n) = Source::of(&k.0).name() {
                let at = m.entry(n.to_string()).or_insert(r.at);
                *at = (*at).min(r.at);
            }
        }
        let host = *lock(&self.host);
        if host > 0 {
            m.insert("host".into(), host);
        }
        m
    }
}

impl Runner for Fresh<'_> {
    fn run(&self, prog: &str, args: &[String]) -> Result<Out> {
        if self.deferred() {
            return Ok(self.last(prog, args, ""));
        }
        self.r.run(prog, args)
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> Result<Out> {
        if self.deferred() {
            return Ok(self.last(prog, args, input));
        }
        self.r.run_in(prog, args, input)
    }
    fn run_for(&self, prog: &str, args: &[String], input: &str, limit: Duration) -> Result<Out> {
        if self.deferred() {
            return Ok(self.last(prog, args, input));
        }
        self.r.run_for(prog, args, input, limit)
    }
    fn sleep(&self, secs: u64) {
        self.r.sleep(secs);
    }
    fn detach(&self, exe: &str, args: &[String], log: &std::path::Path) -> Result<u32> {
        self.r.detach(exe, args, log)
    }
    fn shed(&self) -> Option<crate::shed::Shed> {
        self.r.shed()
    }
    fn secret(&self, svc: &str) -> Result<Option<String>> {
        self.r.secret(svc)
    }
    fn set_secret(&self, svc: &str, value: Option<&str>) -> Result<()> {
        self.r.set_secret(svc, value)
    }
}
