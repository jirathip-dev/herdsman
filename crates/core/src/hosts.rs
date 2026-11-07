//! #157 multi-host lanes (docs/hosts.md): `[hosts.<name>]`, their detected tags, where a new lane goes, and a lane's host
//! as a Runner: herdr gets `--machine`, every other tool runs over `ssh`. No `[hosts]` = this host only, as before.
use crate::config::{expand, set_in, write_atomic, Config, Fleet, HostCfg};
use crate::events::{age, append, emit, fold, last_spawn, open_asks, read, Event, IssueState};
use crate::gate::refused;
use crate::harness;
use crate::host::{self, Host};
use crate::runner::{argv, ok, Out, Runner};
use crate::Ctx;
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cmp::Ordering::Equal;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// `prog args` as one shell line, each word single-quoted.
pub fn quote(prog: &str, args: &[String]) -> String {
    let q = |a: &str| format!("'{}'", a.replace('\'', "'\\''"));
    std::iter::once(q(prog))
        .chain(args.iter().map(|a| q(a)))
        .collect::<Vec<_>>()
        .join(" ")
}

const CM: [&str; 3] = [
    "-oControlMaster=auto",
    "-oControlPath=/tmp/herdsman-ssh-%C",
    "-oControlPersist=10m",
];

/// A lane's host; `machine` None is this one (every call and file exactly as before).
pub struct On<'a> {
    pub r: &'a dyn Runner,
    pub machine: Option<&'a str>,
    connect: &'static str,
}

impl<'a> On<'a> {
    pub fn new(r: &'a dyn Runner, machine: Option<&'a str>) -> Self {
        On {
            r,
            machine,
            connect: "-oConnectTimeout=10",
        }
    }
    /// #485: a maintenance call that must not hold up a pass: a 5 s connect timeout.
    pub fn quick(r: &'a dyn Runner, machine: Option<&'a str>) -> Self {
        On {
            connect: "-oConnectTimeout=5",
            ..Self::new(r, machine)
        }
    }
    /// The host a `spawn` event's lane runs on (none: this one).
    pub fn of(r: &'a dyn Runner, e: Option<&'a Event>) -> Self {
        Self::new(r, e.and_then(|e| e.str("machine")))
    }
    /// The program and argv actually run for `prog args` on this host.
    pub fn wrap(&self, prog: &str, args: &[String]) -> (String, Vec<String>) {
        let Some(m) = self.machine else {
            return (prog.into(), args.to_vec());
        };
        if prog == crate::herdr::BIN {
            return (prog.into(), [argv(&["--machine", m]), args.to_vec()].concat());
        }
        let line = quote(prog, args);
        // #337: one connection per host, kept 10 min, whatever the user's ssh config says.
        let ssh = argv(&["-oBatchMode=yes", self.connect, CM[0], CM[1], CM[2], m, "--", &line]);
        ("ssh".into(), ssh)
    }
    fn sh(&self, script: &str, p: &Path, input: &str) -> Result<Out> {
        self.run_in("sh", &argv(&["-c", script, "sh", &p.to_string_lossy()]), input)
    }
    pub fn exists(&self, p: &Path) -> bool {
        match self.machine {
            None => p.exists(),
            Some(_) => self.sh("test -e \"$1\"", p, "").is_ok_and(|o| o.code == 0),
        }
    }
    pub fn read(&self, p: &Path) -> Option<String> {
        match self.machine {
            None => std::fs::read_to_string(p).ok(),
            Some(_) => ok(self, "cat", &[p.to_string_lossy()]).ok(),
        }
    }
    /// Write `text` to `p`, its folder made first.
    pub fn write(&self, p: &Path, text: &str) -> Result<()> {
        if self.machine.is_none() {
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d)?;
            }
            return Ok(std::fs::write(p, text)?);
        }
        let o = self.sh("mkdir -p \"$(dirname \"$1\")\" && cat > \"$1\"", p, text)?;
        ensure!(o.code == 0, "writing {}: {}", p.display(), o.stderr.trim());
        Ok(())
    }
    /// #441: `write` a file only its owner reads (a lane's secrets).
    pub fn write_private(&self, p: &Path, text: &str) -> Result<()> {
        if self.machine.is_none() {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d)?;
            }
            let mut f = std::fs::File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(p)?;
            return Ok(f.write_all(text.as_bytes())?);
        }
        let o = self.sh("umask 077 && mkdir -p \"$(dirname \"$1\")\" && cat > \"$1\"", p, text)?;
        ensure!(o.code == 0, "writing {}: {}", p.display(), o.stderr.trim());
        Ok(())
    }
    /// Remove the file `p`; a missing one is fine.
    pub fn remove(&self, p: &Path) -> Result<()> {
        match self.machine {
            None if p.exists() => std::fs::remove_file(p)?,
            None => {}
            Some(_) => drop(ok(self, "rm", &["-f".into(), p.to_string_lossy()])?),
        }
        Ok(())
    }
}

impl Runner for On<'_> {
    fn run(&self, prog: &str, args: &[String]) -> Result<Out> {
        let (p, a) = self.wrap(prog, args);
        self.r.run(&p, &a)
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> Result<Out> {
        let (p, a) = self.wrap(prog, args);
        self.r.run_in(&p, &a, input)
    }
    fn sleep(&self, secs: u64) {
        self.r.sleep(secs);
    }
    /// #298: shed on another host is its own server there, through `shed connect` over ssh.
    fn shed(&self) -> Option<crate::shed::Shed> {
        let b = self.r.shed()?;
        let wire = if self.machine.is_some() {
            crate::shed::Wire::Connect
        } else {
            b.wire
        };
        Some(crate::shed::Shed { wire, ..b })
    }
    fn local(&self) -> bool {
        self.machine.is_none() && self.r.local()
    }
    /// #441: the store is the main host's; remote hosts keep none.
    fn secret(&self, svc: &str) -> Result<Option<String>> {
        self.r.secret(svc)
    }
    fn set_secret(&self, svc: &str, value: Option<&str>) -> Result<()> {
        self.r.set_secret(svc, value)
    }
}

/// #561: a check, clean or drive pass's runner. Each remote host is probed once (`ssh … true`, 5 s, no shared
/// connection); an unreachable one answers 255 at once for the rest of the pass. Every remote call is killed after 15 s;
/// one through a dead shared connection drops its socket and runs once more without it. `mark` (the tick's
/// `tick.step`, with the tick's start) names the step and the child it waits on, for `tick-stuck`.
pub struct Pass<'a> {
    r: &'a dyn Runner,
    probe: bool,
    up: std::sync::Mutex<BTreeMap<String, bool>>,
    mark: Option<(PathBuf, u64)>,
    step: std::sync::Mutex<String>,
}

impl<'a> Pass<'a> {
    pub fn new(r: &'a dyn Runner, mark: Option<(PathBuf, u64)>) -> Self {
        let (up, step) = (std::sync::Mutex::default(), std::sync::Mutex::default());
        Pass {
            r,
            probe: true,
            up,
            mark,
            step,
        }
    }
    /// The fast step's: #337's one remote call per host, no probe; a call killed at 15 s marks its host down.
    pub fn fast(r: &'a dyn Runner) -> Self {
        Pass {
            probe: false,
            ..Self::new(r, None)
        }
    }
    pub fn step(&self, s: &str) {
        *self.step.lock().unwrap() = s.into();
        self.note("");
    }
    fn note(&self, child: &str) {
        if let Some((p, since)) = &self.mark {
            let step = self.step.lock().unwrap().clone();
            let _ = std::fs::write(p, json!({"since": since, "step": step, "child": child}).to_string());
        }
    }
    /// The remote host `prog args` runs on: `herdr --machine m …` or `ssh [-o…] m -- …`.
    fn host<'b>(prog: &str, args: &'b [String]) -> Option<&'b str> {
        if prog == crate::herdr::BIN {
            return args.get(1).filter(|_| args[0] == "--machine").map(String::as_str);
        }
        let end = args.iter().position(|a| a == "--").filter(|_| prog == "ssh")?;
        args[..end].last().filter(|m| !m.starts_with('-')).map(String::as_str)
    }
    fn up(&self, m: &str) -> bool {
        if let Some(&u) = self.up.lock().unwrap().get(m) {
            return u;
        }
        if !self.probe {
            return true;
        }
        let probe = argv(&[
            "-oBatchMode=yes",
            "-oConnectTimeout=5",
            "-oControlPath=none",
            m,
            "--",
            "true",
        ]);
        let o = self.r.run_for("ssh", &probe, "", std::time::Duration::from_secs(5));
        let u = o.is_ok_and(|o| !matches!(o.code, 124 | 255));
        self.up.lock().unwrap().insert(m.into(), u);
        u
    }
    fn call(&self, prog: &str, args: &[String], input: Option<&str>) -> Result<Out> {
        let Some(m) = Self::host(prog, args) else {
            return input.map_or_else(|| self.r.run(prog, args), |i| self.r.run_in(prog, args, i));
        };
        self.note(&quote(prog, args));
        if !self.up(m) {
            let stderr = format!("{m}: host offline (no answer to `ssh {m} true` this pass)");
            return Ok(Out {
                code: 255,
                stderr,
                ..Out::default()
            });
        }
        let (limit, input) = (std::time::Duration::from_secs(15), input.unwrap_or_default());
        let mut o = self.r.run_for(prog, args, input, limit)?;
        if matches!(o.code, 124 | 255) && args.iter().any(|a| a == CM[1]) {
            let g = ok(self.r, "ssh", &["-G", CM[1], m]).unwrap_or_default();
            if let Some(sock) = g.lines().find_map(|l| l.strip_prefix("controlpath ")) {
                let _ = std::fs::remove_file(sock);
            }
            let bare = args.iter().filter(|a| !CM.contains(&a.as_str())).cloned();
            let bare: Vec<String> = std::iter::once("-oControlPath=none".into()).chain(bare).collect();
            o = self.r.run_for(prog, &bare, input, limit)?;
        }
        if o.code == 124 {
            self.up.lock().unwrap().insert(m.into(), false);
        }
        self.note("");
        Ok(o)
    }
}

impl Runner for Pass<'_> {
    fn run(&self, prog: &str, args: &[String]) -> Result<Out> {
        self.call(prog, args, None)
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> Result<Out> {
        self.call(prog, args, Some(input))
    }
    fn sleep(&self, secs: u64) {
        self.r.sleep(secs);
    }
    fn detach(&self, exe: &str, args: &[String], log: &Path) -> Result<u32> {
        self.r.detach(exe, args, log)
    }
    fn shed(&self) -> Option<crate::shed::Shed> {
        self.r.shed()
    }
    fn local(&self) -> bool {
        self.r.local()
    }
    fn secret(&self, svc: &str) -> Result<Option<String>> {
        self.r.secret(svc)
    }
    fn set_secret(&self, svc: &str, value: Option<&str>) -> Result<()> {
        self.r.set_secret(svc, value)
    }
}

/// What the last look at a host saw (`<state dir>/hosts.json`): detected tags and `$HOME` (probed hourly), reachability,
/// load/cpu, free memory and disk in GB.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(optional_fields = nullable, rename = "HostSeen"))]
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Seen {
    pub tags: Vec<String>,
    pub home: String,
    pub probed: u64,
    pub seen: u64,
    pub up: bool,
    pub load: Option<f64>,
    pub mem: Option<f64>,
    pub disk: Option<f64>,
    /// #235: a remote host's failed checks from `health` (`✗` keeps lanes off it, `!` warns), with the probe.
    pub health: Vec<String>,
    /// #234: memory pressure 0 normal, 1 warn, 2 critical; swap-out pages/2s; the top memory users (remote hosts).
    pub pressure: Option<u8>,
    pub swap: Option<f64>,
    pub top: Vec<String>,
    /// #234: memory and disk size in GB (a bar shows the used part).
    pub mem_total: Option<f64>,
    pub disk_total: Option<f64>,
    /// #254: when it last had a live agent (a cloud host's idle stop).
    pub busy: u64,
    /// #255: a cloud host's gh token, as its card says it.
    pub github: String,
}

fn file(ctx: &Ctx) -> PathBuf {
    ctx.state.with_file_name("hosts.json")
}

pub fn cached(ctx: &Ctx) -> BTreeMap<String, Seen> {
    let text = std::fs::read_to_string(file(ctx)).unwrap_or_default();
    serde_json::from_str(&text).unwrap_or_default()
}

const PROBE: &str = "uname -s; uname -m; echo \"$HOME\"; xcodebuild -version >/dev/null 2>&1 && echo xcode; \
docker info >/dev/null 2>&1 && echo docker; true";

/// The tags a host detects about itself (`os:macos|os:linux`, `arch:arm64|x86_64`, `xcode`, `docker`) and its `$HOME`.
pub fn probe(r: &dyn Runner) -> Option<(Vec<String>, String)> {
    let out = ok(r, "sh", &["-c", PROBE]).ok()?;
    let mut l = out.lines().map(str::trim);
    let os = l.next()?.replace("Darwin", "macos").to_lowercase();
    let arch = l.next()?.replace("aarch64", "arm64");
    let (home, mut tags) = (l.next()?.to_string(), vec![format!("os:{os}"), format!("arch:{arch}")]);
    tags.extend(l.filter(|t| !t.is_empty()).map(String::from));
    Some((tags, home))
}

/// Look at every host: its live agents (None: unreachable), load, memory and disk, its tags when an hour old (or
/// `refresh`); saved for the board, `hosts` and placement.
pub fn survey(ctx: &Ctx, cfg: &Config, r: &dyn Runner, refresh: bool) -> BTreeMap<String, Option<Vec<Value>>> {
    let (mut all, mut agents, mut rows) = (cached(ctx), BTreeMap::new(), vec![]);
    if cfg.hosts.is_empty() {
        return agents;
    }
    let events = read(&ctx.state).unwrap_or_default();
    for (name, h) in &cfg.hosts {
        let on = On::new(r, h.machine.as_deref());
        let s = all.entry(name.clone()).or_default();
        // #254: a stopped cloud host is not looked at (its lanes are paused, never paged).
        let stopped = h.cloud.is_some() && crate::cloud::state(&events, name) == "stopped";
        let a = (!stopped).then(|| host::agents(&on).ok()).flatten();
        s.up = a.is_some();
        let live = a.as_ref().is_some_and(|a| !a.is_empty());
        s.busy = if live { ctx.now } else { s.busy };
        if s.up {
            s.seen = ctx.now;
            if refresh || s.probed + 3600 <= ctx.now {
                if let Some((tags, home)) = probe(&on) {
                    (s.tags, s.home, s.probed) = (tags, home, ctx.now);
                }
                if h.machine.is_some() && h.enabled {
                    s.health = health(ctx, cfg, &on, &tags(h, Some(&*s)), &s.home);
                    // #255: a cloud host's GitHub token too.
                    if h.cloud.is_some() {
                        let (card, warn) = crate::cloud::github(&on, ctx.now);
                        (s.github, ()) = (card, s.health.extend(warn));
                    }
                    if s.health.iter().any(|w| w.starts_with("! herdsman")) && update(ctx, cfg, &on, name, &s.home) {
                        s.probed = ctx.now.saturating_sub(3000); // read its version back in 10 minutes
                    }
                }
            }
            let hs = host::sample(&on);
            (s.load, s.mem, s.mem_total) = (hs.load_per_cpu, hs.free_mem_gb, hs.mem_total_gb);
            ((s.disk, s.disk_total), s.swap) = (host::df(&on).unzip(), hs.swapout);
            s.pressure = hs.pressure.as_deref().map(level);
            // #234: a remote host's top memory users and its history row come from this 5-minute look (no extra SSH).
            if let Some(ag) = a.as_ref().filter(|_| h.machine.is_some()) {
                s.top = host::top(&on, ag);
                rows.push(row(ctx.now, name, s, (lanes_on(cfg, &events, name).len(), None)));
                import(ctx, cfg, &on, name, &s.home);
            }
        }
        agents.insert(name.clone(), a);
    }
    let _ = std::fs::write(file(ctx), serde_json::to_string(&all).unwrap_or_default());
    if let Err(e) = record(ctx, &rows, cfg.hosts.len()) {
        eprintln!("host history: {e:#}");
    }
    agents
}

/// #275: the asks a lane on host `name` logged in that host's own log (before lanes forwarded them), each imported once
/// under a new id with `from`, agent its newest lane there; #283: only unanswered, recent, open, unmarked ones, no push.
fn import(ctx: &Ctx, cfg: &Config, on: &On, name: &str, home: &str) {
    // #344: the host's own database through its export; a host on an older build: its log file.
    let exe = rehome(vec![ctx.exe.clone()], Some(home)).remove(0);
    let text = ok(on, &exe, &["events", "--jsonl"]).ok();
    let Some(text) = text.or_else(|| on.read(&Path::new(home).join(".local/state/herdsman/events.jsonl"))) else {
        return;
    };
    let events = read(&ctx.state).unwrap_or_default();
    let (states, max_age) = (fold(&events), cfg.limits.import_max_age_hours * 3600);
    let mut n = events.iter().filter(|e| e.kind == "needs-decision").count();
    let mark = |e: &&Event| e.kind == "asks-imported" && e.str("host") == Some(name);
    let upto = events.iter().rev().find(mark).and_then(|e| e.data["upto_ts"].as_u64());
    let remote: Vec<Event> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let new = |e: &&Event| e.kind == "needs-decision" && Some(e.ts) > upto;
    let open = open_asks(&remote); // #632: one pass, not one per ask
    for e in remote.iter().filter(new) {
        let skip = e.ts + max_age < ctx.now || !open.iter().any(|o| std::ptr::eq(*o, e)); // answered or merged there
        let done = e.issue.and_then(|i| states.get(&(e.fleet.clone(), i)));
        let from = format!("{name}:{}", e.str("id").unwrap_or_default());
        if skip || done.is_some_and(IssueState::done) || events.iter().any(|m| m.str("from") == Some(&from)) {
            continue;
        }
        let lane = (events.iter().rev()).find(|s| s.kind == "spawn" && s.fleet == e.fleet && s.issue == e.issue);
        let mut d = Value::Object(e.data.clone());
        n += 1;
        (d["id"], d["from"]) = (json!(format!("q{n}")), json!(from));
        if d["agent"].is_null() {
            d["agent"] = json!(lane
                .filter(|s| s.str("host") == Some(name))
                .and_then(|s| s.str("agent")));
        }
        let _ = append(&ctx.state, &Event::new(e.ts, &e.fleet, e.issue, "needs-decision", d));
    }
    if let Some(t) = remote.iter().filter(new).map(|e| e.ts).max() {
        let d = json!({"host": name, "upto_ts": t});
        let _ = append(&ctx.state, &Event::new(ctx.now, "herdsman", None, "asks-imported", d));
    }
}

/// Detected tags plus the hand ones from fleet.toml.
pub fn tags(h: &HostCfg, s: Option<&Seen>) -> Vec<String> {
    let mut t = s.map(|s| s.tags.clone()).unwrap_or_default();
    t.extend(h.tags.clone());
    t
}

/// The tags a lane of `role` on `fleet#issue` needs: the fleet's, the role's and its queue item's.
pub fn needs(f: &Fleet, role: &str, events: &[Event], (fleet, issue): (&str, u64)) -> Vec<String> {
    let mut n = f.needs.clone();
    n.extend(
        f.role
            .get(f.role_key(role))
            .map(|r| r.needs.clone())
            .unwrap_or_default(),
    );
    n.extend(crate::queue::needs_of(events, fleet, issue));
    n.sort();
    n.dedup();
    n
}

/// Every remote host's live agents, each tagged `host`; an unreachable host adds none.
pub fn remote_agents(cfg: &Config, r: &dyn Runner) -> Vec<Value> {
    let mut all = vec![];
    for (name, m) in cfg.hosts.iter().filter_map(|(n, h)| Some((n, h.machine.as_deref()?))) {
        for mut a in host::agents(&On::new(r, Some(m))).unwrap_or_default() {
            (a["host"], a["machine"]) = (json!(name), json!(m));
            all.push(a);
        }
    }
    all
}

/// #478: the host spawn `sp`'s lane runs on, when the last look (`survey`) could not reach it.
pub fn offline<'a>(ctx: &Ctx, sp: Option<&'a Event>) -> Option<&'a str> {
    let h = sp.filter(|e| e.str("machine").is_some())?.str("host")?;
    cached(ctx).get(h).is_some_and(|s| !s.up).then_some(h)
}

/// This host's live agents, then the remote ones.
pub fn all_agents(cfg: &Config, r: &dyn Runner) -> Result<Vec<Value>> {
    Ok([host::agents(r)?, remote_agents(cfg, r)].concat())
}

/// Admission on one host, in order `DISK_LOW`, `MEM_LOW`, `HOST_BUSY`: its sample and the first refusal.
pub fn gate(ctx: &Ctx, cfg: &Config, on: &On) -> (Host, Option<(&'static str, String)>) {
    if let Some(why) = disk_low(ctx, cfg, on) {
        return (Host::default(), Some(("DISK_LOW", why)));
    }
    let mut hs = host::sample(on);
    if on.machine.is_none() {
        host::window(&ctx.state, &mut hs);
    }
    let mem = cfg.limits.min_free_mem_gb;
    if let Some(gb) = hs.free_mem_gb.filter(|g| *g < mem) {
        let why = format!("only {gb:.1} GB of memory free (floor {mem} GB); a lane needs about 0.5–1.5 GB");
        return (hs, Some(("MEM_LOW", format!("{why}; retry when a lane finishes"))));
    }
    let busy = host::admit(&hs, &cfg.limits).err().map(|w| ("HOST_BUSY", w));
    (hs, busy)
}

/// #416: the host under `min_free_gb`: its free GB and what `clean --yes` would free there, else `reclaim`.
pub fn disk_low(ctx: &Ctx, cfg: &Config, on: &On) -> Option<String> {
    let (min, events) = (cfg.limits.min_free_gb, read(&ctx.state).unwrap_or_default());
    let low = || host::free_gb(on).filter(|g| *g < min);
    let mut gb = low()?;
    // #438: short of disk, the shared build folder's idle profiles go first; then the disk is read again.
    if crate::check::trim_on(on, cfg, 0.0, &mut std::io::sink()).unwrap_or(0) > 0 {
        gb = low()?;
    }
    let st = fold(&events);
    let lane = |e: &&Event| matches!(e.kind.as_str(), "spawn" | "spawn-started") && e.str("machine") == on.machine;
    let done = |e: &&Event| {
        e.issue
            .and_then(|n| st.get(&(e.fleet.clone(), n)))
            .is_some_and(IssueState::done)
    };
    let mut wts: Vec<&str> = events
        .iter()
        .filter(lane)
        .filter(done)
        .filter_map(|e| e.str("worktree"))
        .collect();
    wts.sort_unstable();
    wts.dedup();
    wts.retain(|w| on.exists(Path::new(w)));
    let names: Vec<&str> = wts.iter().filter_map(|w| w.rsplit('/').next()).collect();
    let free = match names.len() {
        0 => "nothing for `herdsman clean --yes` there; `herdsman reclaim` frees idle build folders".into(),
        n => format!(
            "`herdsman clean --yes` frees {n} lane worktree(s) there: {}",
            names.join(", ")
        ),
    };
    Some(format!("{gb:.1} GB free < {min}; {free}"))
}

/// Where a lane goes: its host's name and machine, the worktree root and repo checkout there, the remote `$HOME`, and the
/// host's admission refusal (spawn refuses it, or `--force`s a `HOST_BUSY`).
pub struct Placed {
    pub name: String,
    pub machine: Option<String>,
    pub root: PathBuf,
    pub local: PathBuf,
    pub home: Option<String>,
    pub refusal: Option<(&'static str, String)>,
    pub why: Option<String>,
    rank: (bool, f64, f64),
}

fn at(ctx: &Ctx, cfg: &Config, f: &Fleet, r: &dyn Runner, (name, machine): (&str, Option<&str>)) -> Placed {
    let on = On::new(r, machine);
    let mut seen = cached(ctx).remove(name).unwrap_or_default();
    let cloud = crate::cloud::held(ctx, cfg, name);
    if machine.is_some() && seen.home.is_empty() && cloud.is_none() {
        seen.home = probe(&on).map(|p| p.1).unwrap_or_default();
    }
    let home = machine.and(Some(seen.home.clone()).filter(|h| !h.is_empty()));
    let ex = |p: &str| under(p, home.as_deref());
    let def = if home.is_some() {
        ex("~/.herdr/worktrees")
    } else {
        ctx.worktrees.clone()
    };
    let root = cfg.hosts.get(name).and_then(|h| h.root.as_deref());
    let reach = format!("can't reach {}:", f.repo); // #307: a repo only keeps its own fleet's lanes off
    let sick = (seen.health.iter()).find(|w| w.starts_with('✗') && (!w.starts_with("✗ git ") || w.contains(&reach)));
    let (hs, refusal) = match cloud.map_or_else(|| host::agents(&on).map_err(|_| None), |c| Err(Some(c))) {
        Err(Some(c)) => (Host::default(), Some(c)),
        Err(None) => (Host::default(), Some(("HOST_OFFLINE", "unreachable".to_string()))),
        Ok(_) if sick.is_some() => (Host::default(), sick.map(|w| ("NO_HOST", format!("unhealthy: {w}")))),
        Ok(_) => gate(ctx, cfg, &on),
    };
    Placed {
        name: name.into(),
        machine: machine.map(String::from),
        root: root.map_or(def, ex),
        local: ex(&f.local),
        rank: (
            refusal.is_some(),
            hs.load_per_cpu.unwrap_or(0.0),
            -hs.free_mem_gb.unwrap_or(0.0),
        ),
        refusal: refusal.map(|(c, w)| (c, format!("{name}: {w}"))),
        why: None,
        home,
    }
}

/// `p` with a leading `~/` under the host's `home` (none: this one's).
pub fn under(p: &str, home: Option<&str>) -> PathBuf {
    match (p.strip_prefix("~/"), home) {
        (Some(rest), Some(h)) => Path::new(h).join(rest),
        _ => expand(p),
    }
}

/// #235: is the herdr server in the login session (macOS: a `gui/<uid>` launchd job), where the keychain is open?
const LOGIN: &str = "p=$(pgrep -f 'herdr server$' | head -1); [ -n \"$p\" ] || exit 1; \
command -v launchctl >/dev/null || exit 0; launchctl print gui/$(id -u) | awk -v p=\"$p\" '$1 == p { f = 1 } END { exit !f }'";

/// #235: what is wrong with a reachable remote host, a `✗|! <check> — <detail> → <fix>` row each (none: healthy):
/// its herdr server's version and session, git over SSH for each fleet its `tags` fit, gh and each harness's login
/// (`Harness::health_check`) in a herdr pane, and its herdsman's version against this one's.
pub fn health(ctx: &Ctx, cfg: &Config, on: &On, tags: &[String], home: &str) -> Vec<String> {
    let mut bad = vec![];
    let mut fail = |pass: bool, row: String| bad.extend((!pass).then_some(row));
    let status = ok(on, "sh", &["-c", "PATH=\"$HOME/.local/bin:$PATH\" herdr status"]).unwrap_or_default();
    let (client, server) = crate::timer::versions(&status).map_or(("?".into(), "none".into()), |v| (v.0, v.1));
    let line =
        format!("✗ herdr — server {server}, client {client} → herdr update on the host, then restart its server");
    fail(client == server, line);
    let fix = "restart the herdr server from its launchd job: `launchctl kickstart -k gui/<uid>/<label>`";
    let line = format!("✗ herdr session — the server runs outside the login session (keychain locked) → {fix}");
    fail(ok(on, "sh", &["-c", LOGIN]).is_ok(), line);
    for (name, f) in cfg.fleet.iter().filter(|f| f.1.needs.iter().all(|t| tags.contains(t))) {
        let url = format!("https://github.com/{}.git", f.repo);
        let fix = "add the host's SSH key to GitHub and set `git config --global url.git@github.com:.insteadOf https://github.com/`";
        let o = on.run("git", &argv(&["ls-remote", &url, "HEAD"])).ok();
        let first = o.as_ref().and_then(|o| o.stderr.lines().next()).unwrap_or("no stderr");
        let line = format!("✗ git {name} — can't reach {}: {first} → {fix}", f.repo);
        fail(o.is_some_and(|o| o.code == 0), line);
    }
    let gh = format!("ssh {} gh auth login", on.machine.unwrap_or_default());
    let mut logins = vec![("gh", "gh auth status", gh.as_str())];
    logins.extend(harness::all().filter_map(|h| h.1.health_check()));
    // #235: typed into a fresh pane: logins as a lane sees them; `hs:done` is not in the echoed line.
    let probe = |(t, c, _): &(&str, &str, &str)| format!("{c} >/dev/null 2>&1; echo \"{t}:$?\"; ");
    let pane = logins.iter().map(probe).collect::<String>() + "echo \"hs:\"done";
    let text = crate::herdr::in_pane(on, home, &pane, "hs:done").unwrap_or_default();
    for (tool, _, fix) in logins {
        let line = format!("✗ {tool} — logged out in a herdr pane → {fix} (else: herdr session)");
        fail(text.lines().any(|l| l.trim() == format!("{tool}:0")), line);
    }
    let exe = rehome(vec![ctx.exe.clone()], Some(home)).remove(0);
    let theirs = ok(on, &exe, &["--version"]).map_or("missing".into(), |v| v.trim().to_string());
    let mine = format!("herdsman {}", crate::VERSION);
    let line = format!("! herdsman — {theirs} there, {mine} here → the next `check` updates it (docs/hosts.md)");
    fail(theirs == mine, line);
    // #601: shed there is optional; its failure warns, it never makes the host offline.
    if let Some(Err(e)) = on
        .shed()
        .map(|b| crate::herdr::Runtime::agents(&crate::herdr::Shed(b), on))
    {
        bad.push(format!("! shed: {e:#}"));
    }
    bad
}

/// #235: build this herdsman's sha on the host in the background (log: `~/.herdsman-update.log` there), then
/// `profiles sync` there; logged `host-update` once per host and sha. Whether it started.
pub fn update(ctx: &Ctx, cfg: &Config, on: &On, name: &str, home: &str) -> bool {
    let (sha, events) = (env!("HERDSMAN_SHA"), read(&ctx.state).unwrap_or_default());
    let f = cfg.fleet.values().find(|f| f.repo.ends_with("/herdsman"));
    let done = |e: &Event| e.kind == "host-update" && e.str("host") == Some(name) && e.str("sha") == Some(sha);
    let Some(f) = f.filter(|_| !sha.is_empty() && !events.iter().any(done)) else {
        return false;
    };
    let [exe, conf] =
        [ctx.exe.clone(), ctx.config.display().to_string()].map(|p| rehome(vec![p], Some(home)).remove(0));
    let root = Path::new(&exe)
        .parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new(&exe));
    let script = format!(
        "cd '{}' && git fetch -q origin && git checkout -q --detach {sha} && PATH=\"$HOME/.cargo/bin:$PATH\" \
cargo install --path crates/cli --locked --root '{}' --force && {{ test ! -e '{conf}' || '{exe}' profiles sync; }}",
        under(&f.local, Some(home)).display(),
        root.display()
    );
    let bg = "nohup sh -c \"$1\" </dev/null >\"$HOME/.herdsman-update.log\" 2>&1 &";
    let res = ok(on, "sh", &["-c", bg, "sh", &script]);
    let d = json!({"host": name, "sha": sha, "error": res.as_ref().err().map(|e| format!("{e:#}"))});
    let _ = emit(&ctx.state, cfg, Event::new(ctx.now, "herdsman", None, "host-update", d));
    res.is_ok()
}

/// #157: the host a new lane runs on (None without `[hosts]`): `--host`, else the host of the issue's earlier lanes (its
/// worktree is there, even on a host turned off), else the least-busy enabled host whose tags cover the needs.
pub fn place(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    events: &[Event],
    (fleet, issue, role): (&str, u64, &str),
    force: Option<&str>,
) -> Result<Option<Placed>> {
    if cfg.hosts.is_empty() {
        return match force {
            Some(h) => refused("BAD_VALUE", format!("--host {h}: fleet.toml has no [hosts]")),
            None => Ok(None),
        };
    }
    let f = cfg.fleet(fleet)?;
    // #236: `--host`, else the role's `host` pin; a design lane runs in the main host's herdr only.
    let design = f.family(role) == "design";
    let pin = f.role.get(f.role_key(role)).and_then(|r| r.host.as_deref());
    if let Some(h) = force.or(pin).or(design.then_some("main")) {
        let main = cfg
            .hosts
            .iter()
            .find(|h| h.1.machine.is_none())
            .map_or("local", |h| h.0.as_str());
        let h = if h == "main" { main } else { h };
        let machine = match cfg.hosts.get(h) {
            Some(c) => c.machine.as_deref(),
            None if h == main => None,
            None => return refused("BAD_VALUE", format!("--host {h}: no [hosts.{h}]")),
        };
        if design && machine.is_some() {
            return refused(
                "DESIGN_MAIN_ONLY",
                format!("host {h}: design lanes run on the main host only"),
            );
        }
        return Ok(Some(at(ctx, cfg, f, r, (h, machine))));
    }
    if let Some(e) = events.iter().rev().find(|e| e.is("spawn", fleet, issue)) {
        let was = (e.str("host").unwrap_or("local"), e.str("machine"));
        return Ok(Some(at(ctx, cfg, f, r, was)));
    }
    let needs = needs(f, role, events, (fleet, issue));
    let seen = cached(ctx);
    let fits: Vec<_> = cfg
        .hosts
        .iter()
        .filter(|(n, h)| needs.iter().all(|t| tags(h, seen.get(*n)).contains(t)))
        .collect();
    let mut on: Vec<Placed> = fits
        .iter()
        .filter(|h| h.1.enabled)
        .map(|(n, h)| at(ctx, cfg, f, r, (n, h.machine.as_deref())))
        .collect();
    // #256: a cloud host over its budget is no host for a need only it covers.
    if on
        .iter()
        .all(|p| p.refusal.as_ref().is_some_and(|r| r.0 == "CLOUD_BUDGET"))
    {
        let has = if needs.is_empty() {
            "enabled host".into()
        } else {
            format!("host with {}", needs.join(", "))
        };
        let cloud =
            ["", " (cloud host stopped or over budget)"][usize::from(cfg.hosts.values().any(|h| h.cloud.is_some()))];
        return refused("NO_HOST", format!("no {has}{cloud}"));
    }
    on.sort_by(|a, b| a.rank.partial_cmp(&b.rank).unwrap_or(Equal));
    // #254: a cloud host only when every local host has been refused `overflow_after_minutes`, or it alone fits.
    let local = |p: &Placed| cfg.hosts.get(&p.name).is_some_and(|h| h.cloud.is_none());
    let held = held_for(ctx, on.iter().filter(|p| local(p)).all(|p| p.refusal.is_some()));
    let wake = held >= cfg.limits.overflow_after_minutes * 60 || !on.iter().any(local);
    let i = (!wake && !local(&on[0])).then(|| on.iter().position(local)).flatten();
    let mut p = on.remove(i.unwrap_or(0));
    // #256: why it went to the cloud, for the spawn event and `status`.
    let why = match on.iter().any(local) {
        true => format!("overflow: every local host busy {} min", held / 60),
        false if needs.is_empty() => "no local host fits".into(),
        false => format!("needs {}: no local host has it", needs.join(", ")),
    };
    p.why = (!local(&p)).then_some(why);
    Ok(Some(p))
}

/// #254: how long every local host has been refused (`<state dir>/held-since`: `since last`, cleared once one admits;
/// a mark not seen again within 15 min, three drive passes, is stale and the clock starts over).
fn held_for(ctx: &Ctx, held: bool) -> u64 {
    let p = ctx.state.with_file_name("held-since");
    let s = std::fs::read_to_string(&p).unwrap_or_default();
    let mut n = s.split_whitespace().map(|w| w.parse::<u64>().ok());
    let since = match (n.next().flatten(), n.next().flatten()) {
        (Some(a), Some(b)) if ctx.now.saturating_sub(b) <= 900 => a,
        _ => ctx.now,
    };
    let _ = if held {
        std::fs::write(&p, format!("{since} {}", ctx.now))
    } else {
        std::fs::remove_file(&p)
    };
    [0, ctx.now.saturating_sub(since)][usize::from(held)]
}

/// #254: start the stopped cloud host `place` picked (not on a dry run), then place on it afresh.
pub fn wake(ctx: &Ctx, cfg: &Config, r: &dyn Runner, f: (&Fleet, bool), p: Option<Placed>) -> Result<Option<Placed>> {
    match p {
        Some(p) if !f.1 && p.refusal.as_ref().is_some_and(|x| x.0 == "CLOUD_STOPPED") => {
            crate::cloud::start(ctx, cfg, r, &p.name)?;
            let why = p.why.clone();
            Ok(Some(Placed {
                why,
                ..at(ctx, cfg, f.0, r, (&p.name, p.machine.as_deref()))
            }))
        }
        p => Ok(p),
    }
}

/// `a` with this host's `$HOME` moved to the lane host's `home` (a remote lane's profile, prompt and env paths).
pub fn rehome(a: Vec<String>, home: Option<&str>) -> Vec<String> {
    let mine = crate::config::home().to_string_lossy().into_owned();
    match home.filter(|_| mine.len() > 1) {
        Some(h) => a.into_iter().map(|w| w.replace(&mine, h)).collect(),
        None => a,
    }
}

/// The agents of the open lanes on host `name` (a spawn without `host` ran on the machine-less one, or the only one).
pub fn lanes_on(cfg: &Config, events: &[Event], name: &str) -> Vec<String> {
    let here = cfg.hosts.get(name).is_none_or(|h| h.machine.is_none());
    let on = |l: &&str| last_spawn(events, l).is_some_and(|e| e.str("host").map_or(here, |h| h == name));
    let st = fold(events);
    let open = st.values().filter(|s| !s.done()).filter_map(|s| s.active_lane());
    open.map(|l| l.1).filter(on).map(String::from).collect()
}

/// A host as the board and `hosts` say it: (tone, words). Offline and off are grey: neither pages.
pub fn shown(enabled: bool, s: &Seen, lanes: usize, now: u64) -> (&'static str, String) {
    if !s.up {
        let last = [age(now.saturating_sub(s.seen)), "never".into()][usize::from(s.seen == 0)].clone();
        return ("m", format!("offline · last seen {last}"));
    }
    let sick = |c: char| s.health.iter().find_map(|w| w.strip_prefix(c)).map(str::trim);
    match (enabled, lanes) {
        (false, 0) => ("m", "off · not used for lanes".into()),
        (false, n) => ("m", format!("off · draining {n} lane{}", if n == 1 { "" } else { "s" })),
        _ => match (sick('✗'), sick('!')) {
            (Some(w), _) => ("r", format!("unhealthy · {w}")),
            (_, Some(w)) => ("a", format!("up · {w}")),
            _ => ("g", "up".into()),
        },
    }
}

/// `herdsman hosts [refresh]`: a fresh look at each host; `refresh` probes the tags again.
pub fn list(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, refresh: bool) -> Result<()> {
    if cfg.hosts.is_empty() {
        return Ok(writeln!(out, "no [hosts] in fleet.toml: every lane runs on this host")?);
    }
    // #337: the last look (the tick's, every 5 min) unless asked to refresh or none is saved.
    if refresh || cfg.hosts.keys().any(|h| !cached(ctx).contains_key(h)) {
        survey(ctx, cfg, r, refresh);
    }
    let (seen, events) = (cached(ctx), read(&ctx.state)?);
    let n = |x: Option<f64>, p: usize| x.map_or("?".into(), |x| format!("{x:.p$}"));
    for (name, h) in &cfg.hosts {
        let s = seen.get(name).cloned().unwrap_or_default();
        let lanes = lanes_on(cfg, &events, name);
        let (words, t) = (
            shown(h.enabled, &s, lanes.len(), ctx.now).1,
            tags(h, Some(&s)).join(","),
        );
        let (l, m, d, k, ls) = (n(s.load, 2), n(s.mem, 1), n(s.disk, 0), lanes.len(), lanes.join(" "));
        writeln!(
            out,
            "{name} · {words} · tags {t} · load/cpu {l} · mem {m} GB · disk {d} GB free · {k} lane(s) {ls} · seen {} ago",
            age(ctx.now.saturating_sub(s.seen))
        )?;
    }
    Ok(())
}

/// `herdsman hosts on|off <name>` (and the board's button): `[hosts.<name>] enabled`, logged `host-on`/`host-off`.
pub fn toggle(ctx: &Ctx, out: &mut dyn Write, name: &str, on: bool, by: &str) -> Result<()> {
    let text = std::fs::read_to_string(&ctx.config)?;
    let cfg = Config::parse(&text)?;
    match cfg.hosts.get(name) {
        None => bail!("no [hosts.{name}] in fleet.toml"),
        Some(h) if !on && h.machine.is_none() => return refused("MAIN_HOST", "main host: edit fleet.toml by hand"),
        _ => {}
    }
    let new = set_in(&text, &format!("[hosts.{name}]"), "enabled", &on.to_string())?;
    write_atomic(&ctx.config, &new)?;
    let word = if on { "on" } else { "off" };
    let (kind, d) = (format!("host-{word}"), json!({"host": name, "by": by}));
    emit(&ctx.state, &cfg, Event::new(ctx.now, "herdsman", None, &kind, d))?;
    Ok(writeln!(out, "{name}: {word} for new lanes")?)
}

/// #234: memory pressure as the history keeps it: 0 normal, 1 warn, 2 critical.
pub fn level(p: &str) -> u8 {
    match p {
        "warn" => 1,
        "critical" => 2,
        _ => 0,
    }
}

/// #234: the Host tab's cards in order: the main host (no `machine`; `hostname -s` without `[hosts]`), then A→Z.
pub fn cards<'a>(cfg: &'a Config, r: &dyn Runner) -> Vec<(String, Option<&'a HostCfg>)> {
    if cfg.hosts.is_empty() {
        let name = ok(r, "hostname", &["-s"]).map(|s| s.trim().to_string()).ok();
        return vec![(
            name.filter(|n| !n.is_empty()).unwrap_or_else(|| "this host".into()),
            None,
        )];
    }
    let mut v: Vec<_> = cfg.hosts.iter().map(|(n, h)| (n.clone(), Some(h))).collect();
    v.sort_by_key(|(_, h)| h.is_some_and(|h| h.machine.is_some()));
    v
}

/// #234: one row a minute per host in `<state dir>/host-history.jsonl`, written by the fast step; 24 h kept.
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(optional_fields = nullable, rename = "HostRow"))]
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Row {
    pub ts: u64,
    pub host: String,
    pub load_cpu: Option<f64>,
    pub pressure: Option<u8>,
    pub swap_out: Option<f64>,
    pub mem_free_gb: Option<f64>,
    pub disk_free_gb: Option<f64>,
    pub lanes: usize,
    /// #352: MB per `host::groups` group, only while `/live?tab=host` was seen in the last 2 min (else no `ps` walk).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub procs: Option<[u64; 5]>,
}

impl Row {
    /// Key `k` (load, swap, mem, disk, pressure, lanes): its value, and true when low is bad (free space).
    pub fn get(&self, k: &str) -> (Option<f64>, bool) {
        match k {
            "load" => (self.load_cpu, false),
            "swap" => (self.swap_out, false),
            "mem" => (self.mem_free_gb, true),
            "disk" => (self.disk_free_gb, true),
            "pressure" => (self.pressure.map(f64::from), false),
            _ => (Some(f64::from(u32::try_from(self.lanes).unwrap_or(u32::MAX))), false),
        }
    }
}

const DAY: u64 = 86_400;
/// #234: the history's size bound per host: 1,600 rows (#352: also its row cap) of up to 200 B, so 25 h fit.
pub const BOUND: usize = 320_000;

fn history_file(ctx: &Ctx) -> PathBuf {
    ctx.state.with_file_name("host-history.jsonl")
}

pub fn history(ctx: &Ctx) -> Vec<Row> {
    let text = std::fs::read_to_string(history_file(ctx)).unwrap_or_default();
    text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// #234: append `rows`; once the oldest row is over 25 h old or the file over `BOUND` for each of `hosts`, it is
/// rewritten to the last 24 h and 1,600 rows each (#352), newest first within 9/10 of the bound: once an hour at most.
pub fn record(ctx: &Ctx, rows: &[Row], hosts: usize) -> Result<()> {
    let (p, bound) = (history_file(ctx), BOUND * hosts.max(1));
    let new: String = rows
        .iter()
        .map(|r| serde_json::to_string(r).unwrap_or_default() + "\n")
        .collect();
    let ts = |l: &str| serde_json::from_str::<Row>(l).map_or(0, |r| r.ts);
    let first = std::fs::File::open(&p)
        .ok()
        .and_then(|f| std::io::BufRead::lines(std::io::BufReader::new(f)).next())
        .and_then(Result::ok);
    let len = std::fs::metadata(&p).map_or(0, |m| usize::try_from(m.len()).unwrap_or(usize::MAX)) + new.len();
    let fresh = first.is_none_or(|l| ts(&l) + DAY + 3600 >= ctx.now);
    if fresh && len <= bound {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p)?;
        return Ok(f.write_all(new.as_bytes())?);
    }
    let (text, mut size) = (std::fs::read_to_string(&p)? + &new, 0);
    let keep: Vec<&str> = (text.lines().rev().filter(|l| ts(l) + DAY >= ctx.now))
        .take(1_600 * hosts.max(1))
        .take_while(|l| {
            size += l.len() + 1;
            size <= bound / 10 * 9
        })
        .collect();
    write_atomic(&p, &keep.iter().rev().map(|l| format!("{l}\n")).collect::<String>())
}

/// #234: a history row of `s` at `ts`, rounded so a row stays about 125 B.
fn row(ts: u64, host: &str, s: &Seen, (lanes, procs): (usize, Option<[u64; 5]>)) -> Row {
    let q = |x: Option<f64>, m: f64| x.map(|x| (x * m).round() / m);
    Row {
        ts,
        host: host.into(),
        load_cpu: q(s.load, 100.0),
        pressure: s.pressure,
        swap_out: q(s.swap, 1.0),
        mem_free_gb: q(s.mem, 10.0),
        disk_free_gb: q(s.disk, 10.0),
        lanes,
        procs,
    }
}

/// #234 the fast step: this host's history row, a minute apart (remote hosts' rows come from `survey`); #352: `procs`.
pub fn sample(ctx: &Ctx, cfg: &Config, r: &dyn Runner, events: &[Event]) -> Result<()> {
    let hs = host::sample(r);
    let s = Seen {
        load: hs.load_per_cpu,
        mem: hs.free_mem_gb,
        disk: host::free_gb(r),
        pressure: hs.pressure.as_deref().map(level),
        swap: hs.swapout,
        ..Seen::default()
    };
    let cards = cards(cfg, r);
    let main = cards.iter().find(|c| c.1.is_none_or(|h| h.machine.is_none()));
    let watch = std::fs::read_to_string(ctx.state.with_file_name("host-watch.at")).unwrap_or_default();
    let procs = (watch.trim().parse::<u64>().is_ok_and(|t| t + 120 >= ctx.now)).then(|| host::groups(r));
    let rows: Vec<Row> = main
        .map(|(n, _)| row(ctx.now, n, &s, (lanes_on(cfg, events, n).len(), procs)))
        .into_iter()
        .collect();
    record(ctx, &rows, cards.len())
}

/// #234: host `h`'s key `k` over the `secs` up to `now` in `n` buckets, each its worst row (the max; for free space the
/// min) and that row's time, so a peak is never averaged away; None where no row fell.
pub fn spark(rows: &[Row], h: &str, k: &str, (now, secs, n): (u64, u64, usize)) -> Vec<Option<(f64, u64)>> {
    let (mut out, from) = (vec![None; n], now.saturating_sub(secs));
    for r in rows.iter().filter(|r| r.host == h && r.ts > from && r.ts <= now) {
        let (Some(v), low) = r.get(k) else { continue };
        let i = usize::try_from((r.ts - from - 1) * n as u64 / secs.max(1)).unwrap_or(0);
        let o: &mut Option<(f64, u64)> = &mut out[i.min(n - 1)];
        if o.is_none_or(|(w, _)| if low { v < w } else { v > w }) {
            *o = Some((v, r.ts));
        }
    }
    out
}

/// #234: host `h`'s key `k` as the board sends it, comma-separated (empty: no row): the 24 h sparkline (120 buckets),
/// with `all` also the chart's 6 h (120 buckets) and 1 h (60 minutes), `;` apart; `f` prints a bucket.
fn ranges(rows: &[Row], (h, k, now, all): (&str, &str, u64, bool), f: &dyn Fn((f64, u64)) -> String) -> String {
    let one = |&(secs, n): &(u64, usize)| {
        let v = spark(rows, h, k, (now, secs, n));
        v.into_iter()
            .map(|v| v.map_or(String::new(), f))
            .collect::<Vec<_>>()
            .join(",")
    };
    let rs = [(DAY, 120), (21_600, 120), (3_600, 60)];
    rs[..[1, 3][usize::from(all)]]
        .iter()
        .map(one)
        .collect::<Vec<_>>()
        .join(";")
}

/// #234: `ranges` of the values, as few decimals as the row has.
pub fn series(rows: &[Row], h: &str, k: &str, now: u64, all: bool) -> String {
    let p = match k {
        "load" => 2,
        "mem" | "disk" => 1,
        _ => 0,
    };
    ranges(rows, (h, k, now, all), &|(v, _)| format!("{v:.p$}"))
}

/// #234: an open chart's series, `|` apart: key `k`, the lanes and the memory pressure with every range, then the
/// minutes ago of each of `k`'s worst rows (the read-out's time).
pub fn chart(rows: &[Row], h: &str, k: &str, now: u64) -> String {
    let c = [k, "lanes", "pressure"]
        .map(|x| series(rows, h, x, now, true))
        .join("|");
    c + "|"
        + &ranges(rows, (h, k, now, true), &|(_, ts)| {
            (now.saturating_sub(ts) / 60).to_string()
        })
}
