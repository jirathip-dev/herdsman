use crate::config::{expand, set_in, write_atomic, Config, Fleet, HostCfg};
use crate::events::{age, append, emit, fold, last_spawn, open_asks, working, Event, IssueState};
use crate::gate::refused;
use crate::host::{self, Host};
use crate::providers;
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
    "-oControlPath=/tmp/drover-ssh-%C",
    "-oControlPersist=10m",
];

/// #880: `On::existing`'s script: the paths on stdin, a line each.
pub const EXISTS: &str = "while IFS= read -r p; do test -e \"$p\" && echo y || echo n; done";

/// #880: the worktrees of `spawns` that exist, as `(machine, worktree)`: one `existing` call per host.
pub fn present<'e>(
    r: &dyn Runner,
    spawns: impl IntoIterator<Item = &'e Event>,
) -> std::collections::BTreeSet<(Option<&'e str>, &'e str)> {
    let mut by: BTreeMap<Option<&str>, Vec<&str>> = BTreeMap::new();
    for e in spawns {
        if let Some(w) = e.str("worktree") {
            by.entry(e.str("machine")).or_default().push(w);
        }
    }
    let mut there = std::collections::BTreeSet::new();
    for (m, mut ws) in by {
        ws.sort_unstable();
        ws.dedup();
        there.extend(On::new(r, m).existing(&ws).into_iter().map(|w| (m, w)));
    }
    there
}

/// A lane's host; `machine` None is this one (every call and file exactly as before).
pub struct On<'a> {
    pub r: &'a dyn Runner,
    pub machine: Option<&'a str>,
    connect: u64,
}

impl<'a> On<'a> {
    pub fn new(r: &'a dyn Runner, machine: Option<&'a str>) -> Self {
        On {
            r,
            machine,
            connect: crate::config::limits().ssh_connect_secs,
        }
    }
    /// #485: a maintenance call that must not hold up a pass: a 5 s connect timeout.
    pub fn quick(r: &'a dyn Runner, machine: Option<&'a str>) -> Self {
        On {
            connect: crate::config::limits().ssh_quick_secs,
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
        let line = quote(prog, args);
        let connect = format!("-oConnectTimeout={}", self.connect);
        let ssh = argv(&["-oBatchMode=yes", &connect, CM[0], CM[1], CM[2], m, "--", &line]);
        ("ssh".into(), ssh)
    }
    pub(crate) fn sh(&self, script: &str, p: &Path, input: &str) -> Result<Out> {
        self.run_in("sh", &argv(&["-c", script, "sh", &p.to_string_lossy()]), input)
    }
    pub fn exists(&self, p: &Path) -> bool {
        match self.machine {
            None => p.exists(),
            Some(_) => self.sh("test -e \"$1\"", p, "").is_ok_and(|o| o.code == 0),
        }
    }
    /// #880: which of `paths` exist here: one call (one ssh to a remote host), a `y` or `n` line back per path.
    pub fn existing<'p>(&self, paths: &[&'p str]) -> std::collections::BTreeSet<&'p str> {
        if self.machine.is_none() || paths.is_empty() {
            return paths.iter().copied().filter(|p| Path::new(p).exists()).collect();
        }
        let input: String = paths.iter().map(|p| format!("{p}\n")).collect();
        let o = self.run_in("sh", &argv(&["-c", EXISTS]), &input);
        let o = o.ok().filter(|o| o.code == 0).map(|o| o.stdout).unwrap_or_default();
        let seen: Vec<&str> = o.lines().collect();
        if seen.len() != paths.len() {
            return std::collections::BTreeSet::new();
        }
        paths
            .iter()
            .zip(seen)
            .filter(|(_, y)| *y == "y")
            .map(|(p, _)| *p)
            .collect()
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
        if p == "ssh" {
            return self.r.run_for(&p, &a, "", crate::runner::limit(prog, args));
        }
        self.r.run(&p, &a)
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> Result<Out> {
        let (p, a) = self.wrap(prog, args);
        if p == "ssh" {
            return self.r.run_for(&p, &a, input, crate::runner::limit(prog, args));
        }
        self.r.run_in(&p, &a, input)
    }
    fn run_for(&self, prog: &str, args: &[String], input: &str, limit: std::time::Duration) -> Result<Out> {
        let (p, a) = self.wrap(prog, args);
        self.r.run_for(&p, &a, input, limit)
    }
    fn sleep(&self, secs: u64) {
        self.r.sleep(secs);
    }
    fn shed(&self) -> Option<crate::shed::Shed> {
        let b = self.r.shed()?;
        let wire = if self.machine.is_some() {
            crate::shed::Wire::Connect
        } else {
            b.wire
        };
        Some(crate::shed::Shed { wire, ..b })
    }
}

/// #561: a check, clean or drive pass's runner. Each remote host is probed once (`ssh … true`, 5 s, no shared connection); an unreachable one answers 255 at once for the rest of the pass. Every remote call is killed after 15 s; one through a dead shared connection drops its socket and runs once more without it. `mark` (the tick's `tick.step`, with the tick's start) names the step and the child it waits on, for `tick-stuck`.
pub struct Pass<'a> {
    r: &'a dyn Runner,
    probe: bool,
    up: std::sync::Mutex<BTreeMap<String, bool>>,
    mark: Option<(PathBuf, u64)>,
    step: std::sync::Mutex<(String, u64)>,
    t0: std::time::Instant,
    pub until: Option<std::time::Instant>,
    late: std::sync::Mutex<std::collections::BTreeSet<String>>,
    off: std::collections::BTreeSet<String>,
    log: Option<(&'a Ctx, &'a Config)>,
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
            t0: std::time::Instant::now(),
            until: None,
            late: std::sync::Mutex::default(),
            off: std::collections::BTreeSet::new(),
            log: None,
        }
    }
    /// #939: never contact the machines `off`; a host's changed key logs `host-key-changed` once (with `log`).
    #[must_use]
    pub fn sparing(self, off: std::collections::BTreeSet<String>, log: Option<(&'a Ctx, &'a Config)>) -> Self {
        Pass { off, log, ..self }
    }
    pub fn over(&self) -> bool {
        self.until.is_some_and(|t| std::time::Instant::now() >= t)
    }
    /// #880: the hosts over budget this pass: a call there killed at its limit, or held back past the pass's budget.
    pub fn late(&self) -> Vec<String> {
        self.late.lock().unwrap().iter().cloned().collect()
    }
    /// The fast step's: #337's one remote call per host, no probe; a call killed at 15 s marks its host down.
    pub fn fast(r: &'a dyn Runner) -> Self {
        Pass {
            probe: false,
            ..Self::new(r, None)
        }
    }
    pub fn step(&self, s: &str) {
        debug_assert!(
            crate::timer::TICK_STEPS.iter().any(|t| t.0 == s),
            "{s} is not in TICK_STEPS"
        );
        *self.step.lock().unwrap() = (s.into(), self.t0.elapsed().as_secs());
        self.note("");
    }
    fn note(&self, child: &str) {
        if let Some((p, since)) = &self.mark {
            let (step, at) = self.step.lock().unwrap().clone();
            let at = since + at;
            let m = json!({"since": since, "at": at, "step": step, "child": child, "pid": crate::sim::pid()});
            let _ = std::fs::write(p, m.to_string());
        }
    }
    fn host<'b>(prog: &str, args: &'b [String]) -> Option<&'b str> {
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
        let secs = crate::config::limits().ssh_quick_secs;
        let connect = format!("-oConnectTimeout={secs}");
        let probe = argv(&["-oBatchMode=yes", &connect, "-oControlPath=none", m, "--", "true"]);
        let o = self.r.run_for("ssh", &probe, "", std::time::Duration::from_secs(secs));
        if let Ok(o) = &o {
            self.key_changed(m, o);
        }
        if o.as_ref().is_ok_and(|o| o.code == 124) {
            self.late.lock().unwrap().insert(m.into());
        }
        let u = o.is_ok_and(|o| !matches!(o.code, 124 | 255));
        self.up.lock().unwrap().insert(m.into(), u);
        u
    }
    fn call(&self, prog: &str, args: &[String], input: Option<&str>) -> Result<Out> {
        let Some(m) = Self::host(prog, args) else {
            return input.map_or_else(|| self.r.run(prog, args), |i| self.r.run_in(prog, args, i));
        };
        if self.off.contains(m) {
            let stderr = format!("{m}: not sent (host disabled, no lane there)");
            return Ok(Out {
                code: 255,
                stderr,
                ..Out::default()
            });
        }
        self.note(&quote(prog, args));
        let over = self.over();
        if over {
            self.late.lock().unwrap().insert(m.into());
        }
        if over || !self.up(m) {
            let stderr = format!("{m}: not sent (host offline this pass, or the tick past its budget)");
            return Ok(Out {
                code: 255,
                stderr,
                ..Out::default()
            });
        }
        let limit = std::time::Duration::from_secs(crate::config::limits().ssh_call_secs);
        let input = input.unwrap_or_default();
        let refused =
            |o: &Out| o.code == 255 && (o.stderr.contains("connect to host") || o.stderr.contains(KEY_CHANGED));
        let mut o = self.r.run_for(prog, args, input, limit)?;
        if matches!(o.code, 124 | 255) && !refused(&o) && args.iter().any(|a| a == CM[1]) {
            let g = ok(self.r, "ssh", &["-G", CM[1], m]).unwrap_or_default();
            if let Some(sock) = g.lines().find_map(|l| l.strip_prefix("controlpath ")) {
                let _ = std::fs::remove_file(sock);
            }
            let bare = args.iter().filter(|a| !CM.contains(&a.as_str())).cloned();
            let bare: Vec<String> = std::iter::once("-oControlPath=none".into()).chain(bare).collect();
            o = self.r.run_for(prog, &bare, input, limit)?;
        }
        self.key_changed(m, &o);
        if matches!(o.code, 124 | 255) {
            self.up.lock().unwrap().insert(m.into(), false);
        }
        if o.code == 124 {
            self.late.lock().unwrap().insert(m.into());
        }
        self.note("");
        Ok(o)
    }
    fn key_changed(&self, m: &str, o: &Out) {
        if !o.stderr.contains(KEY_CHANGED) {
            return;
        }
        self.up.lock().unwrap().insert(m.into(), false);
        let Some((ctx, cfg)) = self.log else {
            return;
        };
        let host = cfg
            .hosts
            .iter()
            .find(|h| h.1.machine.as_deref() == Some(m))
            .map_or(m, |h| h.0.as_str());
        let events = working(&ctx.state).unwrap_or_default();
        let kinds = ["host-key-changed", "host-online"];
        let last = events
            .iter()
            .rev()
            .find(|e| e.str("host") == Some(host) && kinds.contains(&e.kind.as_str()));
        if last.is_some_and(|e| e.kind == kinds[0]) {
            return;
        }
        let why = format!("host-key-changed {host}: ssh refuses its new host key; check it, then `ssh-keygen -R {m}`");
        let d = json!({"host": host, "reason": why});
        let _ = emit(&ctx.state, cfg, Event::new(ctx.now, "drover", None, kinds[0], d));
    }
}

const KEY_CHANGED: &str = "REMOTE HOST IDENTIFICATION HAS CHANGED";

/// #939: the machines of disabled hosts with no open lane logged there: drive, check and status never contact them.
pub fn idle_off(cfg: &Config, events: &[Event]) -> std::collections::BTreeSet<String> {
    let idle = |(n, h): &(&String, &HostCfg)| !h.enabled && lanes_on(cfg, events, n).is_empty();
    cfg.hosts
        .iter()
        .filter(idle)
        .filter_map(|h| h.1.machine.clone())
        .collect()
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
}

/// What the last look at a host saw (`<state dir>/hosts.json`): detected tags and `$HOME` (probed hourly), reachability, load/cpu, free memory and disk in GB.
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
    /// #630: its set workspace roots.
    pub roots: Vec<crate::roots::Root>,
    /// #917: a remote host's `drover --version` at the last look.
    pub drover: String,
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

/// Look at every host: its live agents (None: unreachable), load, memory and disk, its tags when an hour old (or `refresh`); saved for the board, `hosts` and placement.
pub fn survey(ctx: &Ctx, cfg: &Config, r: &dyn Runner, refresh: bool) -> BTreeMap<String, Option<Vec<Value>>> {
    let (mut all, mut agents, mut rows) = (cached(ctx), BTreeMap::new(), vec![]);
    if cfg.hosts.is_empty() {
        return agents;
    }
    let events = working(&ctx.state).unwrap_or_default();
    let off = idle_off(cfg, &events);
    for (name, h) in &cfg.hosts {
        if h.machine.as_ref().is_some_and(|m| off.contains(m)) {
            continue;
        }
        let on = On::new(r, h.machine.as_deref());
        let s = all.entry(name.clone()).or_default();
        let stopped = h.cloud.is_some() && crate::cloud::state(&events, name) == "stopped";
        if let Some(Err(e)) = (h.cloud.is_some() && !stopped).then(|| crate::cloud::moved(ctx, cfg, r, name, false)) {
            eprintln!("cloud {name}: {e:#}");
        }
        let a = (!stopped).then(|| host::agents(&on).ok()).flatten();
        s.up = a.is_some();
        let live = a.as_ref().is_some_and(|a| !a.is_empty());
        s.busy = if live { ctx.now } else { s.busy };
        if s.up {
            s.seen = ctx.now;
            if let Some(Err(e)) = h
                .machine
                .as_deref()
                .filter(|_| cfg.host_acp(name).is_some())
                .map(|m| crate::proxy::ensure(ctx, cfg, r, (name, m)))
            {
                eprintln!("proxy {name}: {e:#}");
            }
            if refresh || s.probed + 3600 <= ctx.now {
                if let Some((tags, home)) = probe(&on) {
                    (s.tags, s.home, s.probed) = (tags, home, ctx.now);
                }
                if h.machine.is_some() && h.enabled {
                    s.health = health(ctx, cfg, &on, &tags(h, Some(&*s)), &s.home);
                    if h.cloud.is_some() {
                        let (card, warn) = crate::cloud::github(&on, ctx.now);
                        (s.github, ()) = (card, s.health.extend(warn));
                    }
                }
            }
            if h.machine.is_some() && h.enabled && !s.home.is_empty() {
                s.drover = sync(ctx, &on, name, &s.home);
            }
            let hs = host::sample(&on);
            (s.load, s.mem, s.mem_total) = (hs.load_per_cpu, hs.free_mem_gb, hs.mem_total_gb);
            ((s.disk, s.disk_total), s.swap) = (host::df(&on).unzip(), hs.swapout);
            s.pressure = hs.pressure.as_deref().map(level);
            s.roots = crate::roots::rows(cfg, &events, &on, name, &s.home);
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

fn import(ctx: &Ctx, cfg: &Config, on: &On, name: &str, home: &str) {
    let exe = rehome(vec![ctx.exe.clone()], Some(home)).remove(0);
    let text = ok(on, &exe, &["events", "--jsonl"]).ok();
    let Some(text) = text.or_else(|| on.read(&Path::new(home).join(".local/state/drover/events.jsonl"))) else {
        return;
    };
    let events = working(&ctx.state).unwrap_or_default();
    let (states, max_age) = (fold(&events), cfg.limits.import_max_age_hours * 3600);
    let mark = |e: &&Event| e.kind == "asks-imported" && e.str("host") == Some(name);
    let upto = events.iter().rev().find(mark).and_then(|e| e.val("upto_ts").as_u64());
    let remote: Vec<Event> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let new = |e: &&Event| e.kind == "needs-decision" && Some(e.ts) > upto;
    let open = open_asks(&remote);
    for e in remote.iter().filter(new) {
        let skip = e.ts + max_age < ctx.now || !open.iter().any(|o| std::ptr::eq(*o, e));
        let done = e.issue.and_then(|i| states.get(&(e.fleet.clone(), i)));
        let from = format!("{name}:{}", e.str("id").unwrap_or_default());
        if skip || done.is_some_and(IssueState::done) || events.iter().any(|m| m.str("from") == Some(&from)) {
            continue;
        }
        let lane = (events.iter().rev()).find(|s| s.kind == "spawn" && s.fleet == e.fleet && s.issue == e.issue);
        let mut d = Value::Object(e.data.clone());
        d["from"] = json!(from);
        if d["agent"].is_null() {
            d["agent"] = json!(lane
                .filter(|s| s.str("host") == Some(name))
                .and_then(|s| s.str("agent")));
        }
        let _ = crate::db::ask(
            &ctx.state,
            vec![Event::new(e.ts, &e.fleet, e.issue, "needs-decision", d)],
            None,
        );
    }
    if let Some(t) = remote.iter().filter(new).map(|e| e.ts).max() {
        let d = json!({"host": name, "upto_ts": t});
        let _ = append(&ctx.state, &Event::new(ctx.now, "drover", None, "asks-imported", d));
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

const IDLE: [&str; 6] = ["ready", "needs-human", "parked", "blocked", "merged", "closed"];
const SLOTS: [&str; 2] = ["working", "simulators"];

fn sim(cfg: &Config, events: &[Event], e: &Event) -> bool {
    let (role, n) = (e.str("role").unwrap_or_default(), e.issue.unwrap_or_default());
    let need = |f| needs(f, role, events, (&e.fleet, n));
    cfg.fleet(&e.fleet).is_ok_and(|f| need(f).iter().any(|t| t == "sim"))
}

/// #984: a host's live lanes (`agents`) but `skip`'s: those running a turn, and those holding a simulator.
pub fn held_slots(cfg: &Config, events: &[Event], agents: &[Value], skip: Option<(&str, u64)>) -> [usize; 2] {
    let st = fold(events);
    let mut n = [0; 2];
    for a in agents {
        let sp = a["name"].as_str().and_then(|x| crate::events::last_spawn(events, x));
        let Some((sp, i)) = sp.and_then(|e| Some((e, e.issue?))) else {
            continue;
        };
        let s = st.get(&(sp.fleet.clone(), i));
        if s.is_some_and(|s| !IDLE.contains(&s.state.as_str())) && skip != Some((sp.fleet.as_str(), i)) {
            n[0] += usize::from(!matches!(a["agent_status"].as_str(), Some("idle" | "done" | "blocked")));
            n[1] += usize::from(sim(cfg, events, sp));
        }
    }
    n
}

/// #984: drive's check before a start: `waiting for a slot` while every enabled host the lane fits is full.
pub fn slot_before(ctx: &Ctx, cfg: &Config, r: &dyn Runner, ev: &[Event], me: Lane, start: &[Event]) -> Option<String> {
    let need = needs(cfg.fleet(me.0).ok()?, me.2, ev, (me.0, me.1));
    let (seen, sims) = (cached(ctx), need.iter().any(|t| t == "sim"));
    let others = || start.iter().filter(|e| e.fleet != me.0 || e.issue != Some(me.1));
    let start = [others().count(), others().filter(|e| sim(cfg, ev, e)).count()];
    let fit = |(n, h): &(&String, &HostCfg)| h.enabled && need.iter().all(|t| tags(h, seen.get(*n)).contains(t));
    let mut full = None;
    for (name, h) in cfg.hosts.iter().filter(fit) {
        let caps = [h.max_working, h.max_simulators.filter(|_| sims)];
        (caps != [None; 2]).then_some(())?;
        let agents = host::agents(&On::new(r, h.machine.as_deref())).unwrap_or_default();
        let held = held_slots(
            cfg,
            &crate::events::working(&ctx.state).unwrap_or_default(),
            &agents,
            Some((me.0, me.1)),
        );
        let used = [held[0] + start[0], held[1] + start[1]];
        let (k, c) = (0..2).find_map(|k| caps[k].filter(|&c| used[k] >= c).map(|c| (k, c)))?;
        full.get_or_insert(format!("waiting for a slot: {}/{c} {} on {name}", used[k], SLOTS[k]));
    }
    full
}

/// #984: each capped host's `name: working N/cap · simulators N/cap` line, from its live `agents` (`host`-tagged remote).
pub fn slots_line(cfg: &Config, events: &[Event], agents: &[Value]) -> String {
    let mut out = String::new();
    for (name, h) in &cfg.hosts {
        let on = |a: &&Value| a["host"].as_str().map_or(h.machine.is_none(), |x| x == name);
        let mine: Vec<Value> = agents.iter().filter(on).cloned().collect();
        let held = held_slots(cfg, events, &mine, None);
        let caps = [h.max_working, h.max_simulators];
        let v: Vec<String> = (0..2)
            .filter_map(|k| Some(format!("{} {}/{}", SLOTS[k], held[k], caps[k]?)))
            .collect();
        if !v.is_empty() {
            out += &format!("{name}: {}\n", v.join(" · "));
        }
    }
    out
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

/// #637: add `row` to host `name`'s last-seen health (a `✗` keeps new lanes off it until the next health look).
pub fn sick(ctx: &Ctx, name: &str, row: &str) {
    let mut all = cached(ctx);
    let s = all.entry(name.into()).or_default();
    if !s.health.iter().any(|w| w == row) {
        s.health.push(row.into());
        let _ = std::fs::write(file(ctx), serde_json::to_string(&all).unwrap_or_default());
    }
}

/// #478: the host spawn `sp`'s lane runs on, when the last look (`survey`) could not reach it.
pub fn offline<'a>(ctx: &Ctx, sp: Option<&'a Event>) -> Option<&'a str> {
    let h = sp.filter(|e| e.str("machine").is_some())?.str("host")?;
    cached(ctx).get(h).is_some_and(|s| !s.up).then_some(h)
}

/// #877: spawn `sp`'s lane is on a host that is `off` or failed its last probe: a pass never reaches into it.
pub fn held(ctx: &Ctx, cfg: &Config, sp: &Event) -> bool {
    let off = |h: &str| cfg.hosts.get(h).is_some_and(|c| !c.enabled);
    offline(ctx, Some(sp)).is_some()
        || sp.str("machine").is_some() && sp.str("host").is_some_and(off)
        || crate::roots::skip(ctx, cfg, sp).is_some()
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
    let (min, events) = (cfg.limits.min_free_gb, working(&ctx.state).unwrap_or_default());
    let low = || host::free_gb(on).filter(|g| *g < min);
    let mut gb = low()?;
    let live = match on.machine {
        None => crate::check::live_lanes(ctx, cfg, on).ok(),
        Some(_) => Some(crate::check::Live::new()),
    };
    let trim = |l| crate::check::trim_on(ctx, on, cfg, 0.0, l, &mut std::io::sink()).unwrap_or(0);
    if live.as_ref().is_some_and(|l| trim(l) > 0) {
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
    let there = on.existing(&wts);
    wts.retain(|w| there.contains(w));
    let names: Vec<&str> = wts.iter().filter_map(|w| w.rsplit('/').next()).collect();
    let free = match names.len() {
        0 => "nothing for `drover clean --yes` there; `drover reclaim` frees idle build folders".into(),
        n => format!(
            "`drover clean --yes` frees {n} lane worktree(s) there: {}",
            names.join(", ")
        ),
    };
    Some(format!("{gb:.1} GB free < {min}; {free}"))
}

/// Where a lane goes: its host's name and machine, the worktree root and repo checkout there, the remote `$HOME`, and the host's admission refusal (spawn refuses it, or `--force`s a `HOST_BUSY`).
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

type Pre<'a> = (&'a [Event], Option<&'a str>);

/// #984: a lane by fleet, issue and role.
pub type Lane<'a> = (&'a str, u64, &'a str);

fn at(ctx: &Ctx, cfg: &Config, f: &Fleet, r: &dyn Runner, (name, machine): (&str, Option<&str>), pre: Pre) -> Placed {
    let on = On::new(r, machine);
    let mut seen = cached(ctx).remove(name).unwrap_or_default();
    let cloud = crate::cloud::held(ctx, cfg, name);
    if machine.is_some() && seen.home.is_empty() && cloud.is_none() {
        seen.home = probe(&on).map(|p| p.1).unwrap_or_default();
    }
    let home = machine.and(Some(seen.home.clone()).filter(|h| !h.is_empty()));
    let ex = |p: &str| under(p, home.as_deref());
    let def = if home.is_some() {
        ex(crate::roots::DEFAULT)
    } else {
        ctx.worktrees.clone()
    };
    let roots = crate::roots::listed(cfg, name, home.as_deref(), &def);
    let root = match &roots[..] {
        [one] => one.clone(),
        _ => crate::roots::pick(cfg, pre.0, &on, name, &roots, pre.1),
    };
    let reach = format!("can't reach {}:", f.repo);
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
        root,
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

pub fn health(ctx: &Ctx, cfg: &Config, on: &On, tags: &[String], home: &str) -> Vec<String> {
    let at = |h: &(&String, &HostCfg)| h.1.machine.is_some() && h.1.machine.as_deref() == on.machine;
    if let Some(name) = cfg
        .hosts
        .iter()
        .find(at)
        .map(|h| h.0)
        .filter(|n| cfg.host_acp(n).is_some())
    {
        return acp_health(ctx, cfg, on, (name, tags, home));
    }
    let mut bad = vec![];
    let mut fail = |pass: bool, row: String| bad.extend((!pass).then_some(row));
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
    logins.extend(providers::lanes().filter_map(|h| h.health_check()));
    for (tool, c, fix) in logins {
        fail(
            ok(on, "sh", &["-lc", c]).is_ok(),
            format!("✗ {tool} — logged out → {fix}"),
        );
    }
    let exe = real_exe(ctx, home);
    let theirs = ok(on, &exe, &["--version"]).map_or("missing".into(), |v| v.trim().to_string());
    let mine = format!("drover {}", crate::VERSION);
    let line = format!("! drover — {theirs} there, {mine} here → the next `check` updates it (docs/hosts.md)");
    fail(theirs == mine, line);
    if let Err(e) = host::agents(on) {
        bad.push(format!("! shed: {e:#}"));
    }
    bad
}

fn acp_health(ctx: &Ctx, cfg: &Config, on: &On, (name, tags, home): (&str, &[String], &str)) -> Vec<String> {
    let mut bad = vec![];
    let fits = cfg.fleet.iter().filter(|f| f.1.needs.iter().all(|t| tags.contains(t)));
    let mut seen = std::collections::BTreeSet::new();
    for (fleet, f) in fits {
        let url = format!("https://github.com/{}.git", f.repo);
        if !on
            .run("git", &argv(&["ls-remote", &url, "HEAD"]))
            .is_ok_and(|o| o.code == 0)
        {
            bad.push(format!(
                "✗ git {fleet} — can't reach {}: → hosts bootstrap {name}",
                f.repo
            ));
        }
        for (role, rc) in &f.role {
            let profile = rc.profile_or(role);
            let Some(id) = providers::get(&rc.harness)
                .acp()
                .filter(|_| seen.insert(profile.clone()))
            else {
                continue;
            };
            if let Some(why) = acp_guard(ctx, on, home, (&profile, f.family(role), id)) {
                bad.push(format!("✗ acp guard {profile} — {why}"));
            }
        }
    }
    if ok(on, "sh", &["-lc", "gh auth status"]).is_err() {
        bad.push(format!(
            "✗ gh — logged out → ssh {} gh auth login",
            on.machine.unwrap_or_default()
        ));
    }
    bad
}

/// #637: why a remote ACP lane of `profile` would run unguarded on `on` (`$HOME` `home`), proven there: its drover is this one's version (#573), `profiles sync` gave the profile drover's `PreToolUse` hook for `family`, and its shed lists the hooked adapter `id`. None: guarded.
pub fn acp_guard(ctx: &Ctx, on: &On, home: &str, (profile, family, id): (&str, &str, &str)) -> Option<String> {
    let theirs = ok(on, &real_exe(ctx, home), &["--version"]).map_or("missing".into(), |v| v.trim().to_string());
    let mine = format!("drover {}", crate::VERSION);
    if theirs != mine {
        return Some(format!("drover there is {theirs}, not {mine}"));
    }
    let dir = rehome(
        vec![providers::profiles_root(ctx).join(profile).display().to_string()],
        Some(home),
    )
    .remove(0);
    let s: Value = on
        .read(&Path::new(&dir).join("settings.json"))
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let pre = s["hooks"]["PreToolUse"].as_array().into_iter().flatten();
    let mut hooks = pre.flat_map(|m| m["hooks"].as_array().into_iter().flatten());
    let hook = format!("drover' hook --role {family}");
    if !hooks.any(|h| {
        h["command"]
            .as_str()
            .is_some_and(|c| c.starts_with('\'') && c.ends_with(&hook))
    }) {
        return Some(format!(
            "profile {profile} there has no drover PreToolUse hook; `drover profiles sync` there"
        ));
    }
    let agents = crate::shed::cli(on, &["agents", "--json"]).ok();
    let agents: Value = agents
        .and_then(|o| serde_json::from_str(&o.stdout).ok())
        .unwrap_or_default();
    if !agents.as_array().into_iter().flatten().any(|a| a["id"] == id) {
        return Some(format!("shed there lists no {id} (`shed agents add {id}` there)"));
    }
    None
}

fn real_exe(ctx: &Ctx, home: &str) -> String {
    let exe = std::fs::canonicalize(&ctx.exe).map_or(ctx.exe.clone(), |p| p.to_string_lossy().into());
    rehome(vec![exe], Some(home)).remove(0)
}

/// #917: `cargo install` of drover at `sha` into `root`, with its `herdsman` link: `hosts bootstrap`'s and `sync`'s.
pub fn install_line(root: &str, sha: &str) -> String {
    let repo = env!("CARGO_PKG_REPOSITORY");
    format!("cargo install --git {repo} --rev {sha} --locked --root '{root}' --force && ln -sf drover '{root}/bin/herdsman'")
}

const SYNC_STATUS: &str = "\"$1\" --version 2>/dev/null || echo missing; d=\"$HOME/.drover-sync\"; \
if [ -f \"$d.done\" ]; then echo \"done $(cat \"$d.done\")\"; rm -f \"$d.done\"; fi; \
[ -f \"$d.pid\" ] && kill -0 \"$(cat \"$d.pid\")\" 2>/dev/null && echo busy; true";
const SYNC_START: &str =
    "V=\"$2\" nohup sh -c \"$1\" </dev/null >\"$HOME/.drover-sync.log\" 2>&1 & echo $! >\"$HOME/.drover-sync.pid\"";

fn sha_of(v: &str) -> &str {
    v.rsplit_once('(').and_then(|p| p.1.strip_suffix(')')).unwrap_or(v)
}

/// #917: host `name`'s drover `--version`, its last install logged `host-sync`, a mismatch's install started there.
pub fn sync(ctx: &Ctx, on: &On, name: &str, home: &str) -> String {
    let (exe, sha) = (real_exe(ctx, home), env!("DROVER_SHA"));
    let Ok(o) = ok(on, "sh", &["-c", SYNC_STATUS, "sh", &exe]) else {
        return String::new();
    };
    let mut lines = o.lines().map(str::trim);
    let theirs = lines.next().unwrap_or_default().to_string();
    let mut busy = false;
    for l in lines {
        busy |= l == "busy";
        if let Some((code, rest)) = l.strip_prefix("done ").and_then(|l| l.split_once(' ')) {
            let (to, from) = rest.split_once(' ').unwrap_or((rest, ""));
            let d = json!({"host": name, "from": sha_of(from), "to": to, "ok": code == "0"});
            let _ = append(&ctx.state, &Event::new(ctx.now, "drover", None, "host-sync", d));
        }
    }
    if busy || theirs == format!("drover {}", crate::VERSION) {
        return theirs;
    }
    let conf = rehome(vec![ctx.config.display().to_string()], Some(home)).remove(0);
    let root = Path::new(&exe).parent().and_then(Path::parent).map(Path::display);
    let install = install_line(&root.map_or(exe.clone(), |r| r.to_string()), sha);
    let job = format!(
        "{}{{ {install} && {{ test ! -e '{conf}' || '{exe}' profiles sync; }}; }}; echo \"$? {sha} $V\" >\"$HOME/.drover-sync.done\"",
        crate::cloud::SH
    );
    if let Err(e) = ok(on, "sh", &["-c", SYNC_START, "sh", &job, &theirs]) {
        let d = json!({"host": name, "from": sha_of(&theirs), "to": sha, "ok": false, "error": format!("{e:#}")});
        let _ = append(&ctx.state, &Event::new(ctx.now, "drover", None, "host-sync", d));
    }
    theirs
}

/// #157: the host a new lane runs on (None without `[hosts]`): `--host`, else the host of the issue's last lane while it is enabled, up and admitting (#676), else the least-busy enabled host whose tags cover the needs.
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
    let last = events.iter().rev().find(|e| e.is("spawn", fleet, issue));
    let pre = (events, last.and_then(|e| e.str("root")));
    let design = crate::events::STAGES.contains(&f.family(role));
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
                format!("host {h}: {role} lanes run on the main host only"),
            );
        }
        return Ok(Some(at(ctx, cfg, f, r, (h, machine), pre)));
    }
    if let Some(e) = last {
        let was = (e.str("host").unwrap_or("local"), e.str("machine"));
        let mut p = at(ctx, cfg, f, r, was, pre);
        let code = p.refusal.as_ref().map(|x| x.0);
        let busy = matches!(code, Some("DISK_LOW" | "HOST_BUSY" | "MEM_LOW"));
        let wt = last_spawn(events, &format!("{fleet}-{role}-{issue}")).filter(|l| l.str("machine") == was.1);
        let wt = wt.and_then(|l| l.str("worktree")).map(Path::new);
        let unpushed = busy && wt.is_some_and(|w| crate::check::unsaved(&On::new(r, was.1), w));
        if unpushed {
            p.refusal.as_mut().unwrap().1 += &format!("; stays on {}: its worktree there has unpushed work", was.0);
        }
        if cfg.hosts.get(was.0).is_none_or(|h| h.enabled) && (unpushed || !busy && code != Some("HOST_OFFLINE")) {
            return Ok(Some(p));
        }
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
        .map(|(n, h)| at(ctx, cfg, f, r, (n, h.machine.as_deref()), pre))
        .collect();
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
    let local = |p: &Placed| cfg.hosts.get(&p.name).is_some_and(|h| h.cloud.is_none());
    let held = held_for(ctx, on.iter().filter(|p| local(p)).all(|p| p.refusal.is_some()));
    let wake = held >= cfg.limits.overflow_after_minutes * 60 || !on.iter().any(local);
    let i = (!wake && !local(&on[0])).then(|| on.iter().position(local)).flatten();
    let mut p = on.remove(i.unwrap_or(0));
    let why = match on.iter().any(local) {
        true => format!("overflow: every local host busy {} min", held / 60),
        false if needs.is_empty() => "no local host fits".into(),
        false => format!("needs {}: no local host has it", needs.join(", ")),
    };
    p.why = (!local(&p)).then_some(why);
    Ok(Some(p))
}

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
                ..at(ctx, cfg, f.0, r, (&p.name, p.machine.as_deref()), (&[], None))
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

/// #840: a remote host unreachable 15 min straight: its open lanes restart on the main host from their pushed branch.
pub fn evacuate(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<()> {
    let seen = cached(ctx);
    for (name, h) in cfg.hosts.iter().filter(|h| h.1.machine.is_some()) {
        let events = working(&ctx.state)?;
        let stopped = h.cloud.is_some() && crate::cloud::state(&events, name) == "stopped";
        if stopped
            || !seen
                .get(name)
                .is_some_and(|s| !s.up && s.seen + cfg.limits.host_offline_minutes * 60 <= ctx.now)
        {
            continue;
        }
        let mut moved = vec![];
        for s in fold(&events).values().filter(|s| !s.done()) {
            let (Some((role, agent)), Ok(f)) = (s.active_lane(), cfg.fleet(&s.fleet)) else {
                continue;
            };
            if last_spawn(&events, agent).is_none_or(|e| e.str("host") != Some(name)) {
                continue;
            }
            let o = crate::spawn::Opts {
                supersede: true,
                host: Some("main".into()),
                ..Default::default()
            };
            if let Err(e) = crate::spawn::spawn(ctx, cfg, r, out, (&s.fleet, s.issue, role), &o) {
                writeln!(out, "{agent}: restart off {name} failed: {e:#}")?;
                continue;
            }
            let now = working(&ctx.state)?;
            let Some(to) = last_spawn(&now, agent).filter(|e| e.str("host") != Some(name)) else {
                continue;
            };
            let local = under(&f.local, None).display().to_string();
            let branch = format!("origin/{}", to.str("branch").unwrap_or_default());
            let sha = ok(r, "git", &["-C", &local, "rev-parse", "--verify", "--quiet", &branch]).ok();
            let d =
                json!({"agent": agent, "host": name, "to": to.str("host"), "sha": sha.map(|s| s.trim().to_string())});
            emit(
                &ctx.state,
                cfg,
                Event::new(ctx.now, &s.fleet, Some(s.issue), "lane-moved", d),
            )?;
            moved.push(agent.to_string());
        }
        if !moved.is_empty() {
            let why = format!(
                "{name} offline 15 min: {} restarted on the main host from the pushed branch; unpushed work stayed on {name}",
                moved.join(", ")
            );
            writeln!(out, "{why}")?;
            let d = json!({"host": name, "lanes": moved, "reason": why});
            emit(&ctx.state, cfg, Event::new(ctx.now, "drover", None, "lanes-moved", d))?;
        }
    }
    Ok(())
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

/// `drover hosts [refresh]`: a fresh look at each host; `refresh` probes the tags again.
pub fn list(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write, refresh: bool) -> Result<()> {
    if cfg.hosts.is_empty() {
        return Ok(writeln!(out, "no [hosts] in fleet.toml: every lane runs on this host")?);
    }
    if refresh || cfg.hosts.keys().any(|h| !cached(ctx).contains_key(h)) {
        survey(ctx, cfg, r, refresh);
    }
    let (seen, events) = (cached(ctx), working(&ctx.state)?);
    let n = |x: Option<f64>, p: usize| x.map_or("?".into(), |x| format!("{x:.p$}"));
    for (name, h) in &cfg.hosts {
        let s = seen.get(name).cloned().unwrap_or_default();
        let lanes = lanes_on(cfg, &events, name);
        let (words, t) = (
            shown(h.enabled, &s, lanes.len(), ctx.now).1,
            tags(h, Some(&s)).join(","),
        );
        let (l, m, d, k, ls) = (n(s.load, 2), n(s.mem, 1), n(s.disk, 0), lanes.len(), lanes.join(" "));
        let v = match sha_of(&s.drover) {
            "" => String::new(),
            x if x == env!("DROVER_SHA") => format!(" · drover {x} ✓"),
            x => format!(" · drover {x} ≠ {}", env!("DROVER_SHA")),
        };
        writeln!(
            out,
            "{name} · {words} · tags {t}{v} · load/cpu {l} · mem {m} GB · disk {d} GB free · {k} lane(s) {ls} · seen {} ago",
            age(ctx.now.saturating_sub(s.seen))
        )?;
    }
    Ok(())
}

/// `drover hosts on|off <name>` (and the board's button): `[hosts.<name>] enabled`, logged `host-on`/`host-off`.
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
    emit(&ctx.state, &cfg, Event::new(ctx.now, "drover", None, &kind, d))?;
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

/// #234: append `rows`; once the oldest row is over 25 h old or the file over `BOUND` for each of `hosts`, it is rewritten to the last 24 h and 1,600 rows each (#352), newest first within 9/10 of the bound: once an hour at most.
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

/// #234: host `h`'s key `k` over the `secs` up to `now` in `n` buckets, each its worst row (the max; for free space the min) and that row's time, so a peak is never averaged away; None where no row fell.
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

/// #234: an open chart's series, `|` apart: key `k`, the lanes and the memory pressure with every range, then the minutes ago of each of `k`'s worst rows (the read-out's time).
pub fn chart(rows: &[Row], h: &str, k: &str, now: u64) -> String {
    let c = [k, "lanes", "pressure"]
        .map(|x| series(rows, h, x, now, true))
        .join("|");
    c + "|"
        + &ranges(rows, (h, k, now, true), &|(_, ts)| {
            (now.saturating_sub(ts) / 60).to_string()
        })
}
