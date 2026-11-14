use crate::config::Config;
use crate::events::{append, Event};
use crate::runner::{argv, Out, Runner};
use crate::Ctx;
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use shed_proto::{EventKind, EventsParams, Hello, Method, Request, Response};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

pub use shed_proto as proto;

/// #637: the shed a `shed-acp` host's bootstrap installs: the build proven with drover on the main host. shed's repo, beside drover's own.
pub fn git() -> String {
    env!("CARGO_PKG_REPOSITORY").replace("/herdsman", "/shed")
}
pub const REV: &str = "9b88eb5be5bc9ba8c5c75480483b9861ddc6e993";

/// Where a host's shed server answers.
#[derive(Clone, Debug, PartialEq)]
pub enum Wire {
    Socket(PathBuf),
    Connect,
}

/// The shed runtime a runner's lanes use: its wire, and the ACP agent its lanes run (`shed run --acp`), if any.
#[derive(Clone, Debug)]
pub struct Shed {
    pub wire: Wire,
    pub acp: Option<String>,
    pub policy: String,
}

/// shed's per-user socket (as `shed_server::Paths::user`): macOS `~/Library/Caches/shed/shed.sock`, elsewhere `$XDG_RUNTIME_DIR/shed.sock`.
pub fn default_socket() -> PathBuf {
    if cfg!(target_os = "macos") {
        return crate::config::home().join("Library/Caches/shed/shed.sock");
    }
    PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_default()).join("shed.sock")
}

fn lines(m: Method, params: Value) -> String {
    let hello = json!(Hello::ours());
    let [a, b] = [Request::new(0, Method::Hello, hello), Request::new(1, m, params)].map(|q| json!(q).to_string());
    format!("{a}\n{b}\n")
}

fn answer(text: &str) -> Result<Value> {
    let mut it = text.lines().map(serde_json::from_str::<Response>);
    let mut next = || -> Result<Response> { Ok(it.next().context("shed: connection closed")??) };
    let hello = next()?;
    if let Some(e) = hello.error {
        bail!("{}", e.message);
    }
    let theirs = hello.result.and_then(|v| serde_json::from_value::<Hello>(v).ok());
    let side = |h: &Hello| format!("{} ({}) protocol {}", h.version, h.label(), h.protocol);
    ensure!(
        theirs.as_ref().is_some_and(Hello::compatible),
        "SHED_VERSION: drover speaks shed {}, the server is {}",
        side(&Hello::ours()),
        theirs.as_ref().map_or("?".into(), side)
    );
    match next()? {
        Response { error: Some(e), .. } => bail!("{}", e.message),
        Response { result: Some(v), .. } => Ok(v),
        _ => bail!("shed: neither result nor error"),
    }
}

/// One call on a fresh connection (`hello`, then `m`): its result.
pub fn call(r: &dyn Runner, w: &Wire, m: Method, params: Value) -> Result<Value> {
    let text = match w {
        Wire::Socket(p) => {
            let mut s = UnixStream::connect(p).with_context(|| format!("no shed server on {}", p.display()))?;
            s.write_all(lines(m, params).as_bytes())?;
            let (mut rd, mut text) = (BufReader::new(s), String::new());
            for _ in 0..2 {
                rd.read_line(&mut text)?;
            }
            text
        }
        Wire::Connect => {
            let o = r.run_in("shed", &argv(&["connect", "--autostart"]), &lines(m, params))?;
            ensure!(
                !o.stdout.is_empty(),
                "shed connect failed ({}): {}",
                o.code,
                o.stderr.trim()
            );
            o.stdout
        }
    };
    answer(&text)
}

/// The `shed` CLI (ACP mode only: its bridge and prompt framing are shed's own); fails on a non-zero exit.
pub fn cli(r: &dyn Runner, a: &[&str]) -> Result<Out> {
    let o = r.run("shed", &argv(a))?;
    ensure!(
        o.code == 0,
        "shed {} failed ({}): {}",
        a.join(" "),
        o.code,
        o.stderr.trim()
    );
    Ok(o)
}

/// The kept events after `since` (shed keeps the last 1,000), read until the stream is quiet for `quiet`.
pub fn events(w: &Wire, since: u64, quiet: Duration) -> Result<Vec<shed_proto::Event>> {
    let mut out = vec![];
    follow(w, since, Some(quiet), |e| out.extend(e))?;
    Ok(out)
}

/// #529: `events` kept open: `f(None)` once the server took it, then `f(Some(event))` for each kept event after `since` and each new one, until the stream ends (or is quiet for `quiet`).
pub fn follow(
    w: &Wire,
    since: u64,
    quiet: Option<Duration>,
    mut f: impl FnMut(Option<shed_proto::Event>),
) -> Result<()> {
    let Wire::Socket(p) = w else {
        bail!("shed events: this host's server only");
    };
    let mut s = UnixStream::connect(p).with_context(|| format!("no shed server on {}", p.display()))?;
    let params = EventsParams {
        since: Some(since),
        ..Default::default()
    };
    s.write_all(lines(Method::Events, json!(params)).as_bytes())?;
    s.set_read_timeout(quiet)?;
    let (mut rd, mut head) = (BufReader::new(s), String::new());
    for _ in 0..2 {
        rd.read_line(&mut head)?;
    }
    answer(&head)?;
    f(None);
    let mut l = String::new();
    while rd.read_line(&mut l).is_ok_and(|n| n > 0) {
        if let Ok(e) = serde_json::from_str(&l) {
            f(Some(e));
        }
        l.clear();
    }
    Ok(())
}

/// The last shed `seq` taken in (`shed.seq` beside the log).
pub fn seen(ctx: &Ctx) -> u64 {
    std::fs::read_to_string(ctx.state.with_file_name("shed.seq"))
        .or_else(|_| std::fs::read_to_string(ctx.state.with_file_name("berth.seq")))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// #662: an ACP lane's exit causes drover resumes it from (same worktree, next account, never `LANE_ENDS`).
pub const RESUMABLE: [&str; 2] = ["account limit", "adapter crash"];

/// #662: why lane `agent`'s latest session ended, as the `gone` lane-state after its last spawn logged it.
pub fn exit_cause<'a>(events: &'a [Event], agent: &str) -> Option<&'a str> {
    let gone = |e: &&Event| e.kind == "lane-state" && e.str("agent") == Some(agent) && e.str("state") == Some("gone");
    let at = events
        .iter()
        .rposition(|e| e.kind == "spawn" && e.str("agent") == Some(agent))?;
    events[at..].iter().rev().find(gone)?.str("cause")
}

fn last_out(ctx: &Ctx, agent: &str, f: impl FnOnce(&mut Value)) -> Result<Value> {
    let path = ctx.state.with_file_name("shed-out.json");
    let mut all: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}));
    let was = all[agent].take();
    let mut now = was.clone();
    f(&mut now);
    if now.is_null() {
        all.as_object_mut().map(|m| m.remove(agent));
    } else {
        all[agent] = now;
    }
    std::fs::write(&path, all.to_string())?;
    Ok(was)
}

/// #792: ACP lane `agent`'s session as drive's intake last saw it (`turn` open, `at`, `said`, open `tools`, last `cpu`); null if none.
pub fn session(ctx: &Ctx, agent: &str) -> Value {
    let t = std::fs::read_to_string(ctx.state.with_file_name("shed-out.json")).unwrap_or_default();
    serde_json::from_str::<Value>(&t).map_or(Value::Null, |mut v| v[agent].take())
}

/// #785: the ACP lanes with no event back `mins` after their first turn: (agent, when that turn went in).
pub fn silent(ctx: &Ctx, mins: u64) -> Vec<(String, u64)> {
    let all: Value = crate::usage::load(&ctx.state.with_file_name("shed-out.json")).unwrap_or_default();
    let start = |(a, v): (&String, &Value)| Some((a.clone(), v["start"].as_u64()?));
    let all = all.as_object().into_iter().flatten().filter_map(start);
    all.filter(|s| ctx.now >= s.1 + mins * 60).collect()
}

/// One shed event as its lane's passive `lane-state` (#529: an exit or kill is `gone`; #662: with its `cause`, and an account limit caps the lane's account); whether it was a lane's.
pub fn log(ctx: &Ctx, cfg: &Config, e: &shed_proto::Event) -> Result<bool> {
    let issue = e.labels.get("issue").and_then(|i| i.parse().ok());
    let Some(fleet) = e.labels.get("fleet").filter(|f| cfg.fleet.contains_key(*f)) else {
        return Ok(false);
    };
    if issue.is_none() && !e.labels.contains_key("thread") {
        return Ok(false);
    }
    let more = match &e.kind {
        EventKind::Turn { .. } => Some(("", true, false)),
        EventKind::Text { text } => Some((text.as_str(), true, true)),
        EventKind::Reply { text, .. } => Some((text.as_str(), false, false)),
        _ => None,
    };
    if e.kind.is_acp() && !e.replay {
        last_out(ctx, &e.name, |v| {
            v["at"] = json!(ctx.now);
            match &e.kind {
                EventKind::Turn { .. } => {}
                EventKind::ToolCall { id, status, .. } if matches!(status.as_str(), "pending" | "in_progress") => {
                    (v["said"], v["tools"][id]) = (json!(ctx.now), json!(status));
                }
                EventKind::ToolCall { id, .. } => {
                    v["said"] = json!(ctx.now);
                    v["tools"].as_object_mut().map(|m| m.remove(id));
                }
                EventKind::Reply { .. } => (v["said"], v["tools"]) = (json!(ctx.now), Value::Null),
                _ => v["said"] = json!(ctx.now),
            }
        })?;
    }
    if let EventKind::Resources(u) = &e.kind {
        last_out(ctx, &e.name, |v| {
            if !v.is_null() {
                v["cpu"] = json!(u.cpu_pct);
            }
        })?;
    }
    if let Some((t, turn, add)) = more {
        last_out(ctx, &e.name, |v| {
            let start = (turn && !add).then(|| v.get("start").map_or(ctx.now.into(), Value::clone));
            let all = [v["text"].as_str().filter(|_| add).unwrap_or_default(), t].concat();
            let tail: String = all
                .chars()
                .rev()
                .take(500)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            (v["text"], v["turn"], v["start"]) = (json!(tail), json!(turn), json!(start));
        })?;
        return Ok(false);
    }
    let Some(issue) = issue else {
        return Ok(false);
    };
    let mut d = match &e.kind {
        EventKind::State(s) => json!({"state": s.state, "note": s.note}),
        EventKind::Exited { code } => {
            let out = last_out(ctx, &e.name, |v| *v = Value::Null)?;
            let banner = crate::providers::accounts()
                .and_then(|p| p.usage())
                .map(crate::providers::Usage::cap_banner);
            let pat = cfg.accounts.cap_pattern.as_deref().or(banner).unwrap_or_default();
            let cause = if crate::access::banner(pat, out["text"].as_str().unwrap_or_default()) {
                RESUMABLE[0]
            } else if *code != Some(0) || out["turn"] == true {
                RESUMABLE[1]
            } else {
                "ended normally"
            };
            json!({"state": "gone", "why": "exited", "code": code, "cause": cause})
        }
        EventKind::Killed => {
            last_out(ctx, &e.name, |v| *v = Value::Null)?;
            json!({"state": "gone", "why": "killed", "cause": "killed"})
        }
        _ => return Ok(false),
    };
    (d["agent"], d["shed_seq"]) = (json!(e.name), json!(e.seq));
    let cause = d["cause"].as_str().map(String::from);
    append(&ctx.state, &Event::new(ctx.now, fleet, Some(issue), "lane-state", d))?;
    let sp = (cause.as_deref() == Some(RESUMABLE[0]))
        .then(|| crate::events::working(&ctx.state))
        .transpose()?;
    let sp = sp.as_deref().and_then(|ev| crate::events::last_spawn(ev, &e.name));
    if let Some((sp, a)) = sp.and_then(|sp| Some((sp, sp.str("account")?))) {
        let why = format!("{a}: lane {} hit the usage limit; spawns skip {a} for now", e.name);
        let d = json!({"key": format!("{}:{a}", e.name), "reason": why, "agent": e.name, "account": a,
            "harness": sp.str("harness"), "host": sp.str("host")});
        crate::events::emit(
            &ctx.state,
            cfg,
            Event::new(ctx.now, fleet, Some(issue), "account-capped", d),
        )?;
    }
    Ok(true)
}

/// #298: drive's intake of shed's state changes since the last pass (`shed.seq` beside the log): each lane's reported state, exit or kill as a passive `lane-state` event. Only this host's server; the agent list every pass polls still sees every lane, here and elsewhere. #529: a running service follows the stream and keeps `shed.seq`.
pub fn intake(ctx: &Ctx, cfg: &Config, w: &Wire) -> Result<usize> {
    let got = events(w, seen(ctx), Duration::from_millis(cfg.limits.shed_read_millis))?;
    let mut n = 0;
    for e in &got {
        n += usize::from(log(ctx, cfg, e)?);
    }
    if let Some(last) = got.iter().map(|e| e.seq).max() {
        std::fs::write(ctx.state.with_file_name("shed.seq"), last.to_string())?;
    }
    Ok(n)
}

/// A runner whose lanes run on shed: `r` for every program, `b` for the runtime.
pub struct With<'a>(pub &'a dyn Runner, pub Shed);

impl Runner for With<'_> {
    fn run(&self, prog: &str, args: &[String]) -> Result<Out> {
        self.0.run(prog, args)
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> Result<Out> {
        self.0.run_in(prog, args, input)
    }
    fn run_for(&self, prog: &str, args: &[String], input: &str, limit: std::time::Duration) -> Result<Out> {
        self.0.run_for(prog, args, input, limit)
    }
    fn sleep(&self, secs: u64) {
        self.0.sleep(secs);
    }
    fn detach(&self, exe: &str, args: &[String], log: &std::path::Path) -> Result<u32> {
        self.0.detach(exe, args, log)
    }
    fn shed(&self) -> Option<Shed> {
        Some(self.1.clone())
    }
}

/// #611: each enabled host's shed sessions that are no lane or conductor, or `{host, error}` for a host whose shed fails.
pub fn others(cfg: &Config, r: &dyn Runner, events: &[Event]) -> Vec<Value> {
    let ours = |e: &&Event| matches!(e.kind.as_str(), "spawn" | "orchestrator-start");
    let ours: std::collections::BTreeSet<&str> = events.iter().filter(ours).filter_map(|e| e.str("agent")).collect();
    let mut out = vec![];
    for (host, h) in crate::hosts::cards(cfg, r) {
        let on = crate::hosts::On::new(r, h.and_then(|h| h.machine.as_deref()));
        let Some(w) = on.shed().map(|b| b.wire).filter(|_| h.is_none_or(|h| h.enabled)) else {
            continue;
        };
        let ls = call(&on, &w, Method::Ls, json!(proto::LsParams::default()));
        match ls.and_then(|v| Ok(serde_json::from_value::<Vec<proto::TerminalInfo>>(v)?)) {
            Ok(ts) => out.extend(
                ts.into_iter()
                    .filter(|t| !ours.contains(t.name.as_str()) && !t.labels.contains_key("fleet"))
                    .map(|t| {
                        let state = match (t.state, &t.reported) {
                            (proto::State::Running, Some(s)) => s.state.as_str(),
                            (proto::State::Running, None) => "running",
                            (proto::State::Exited, _) => "exited",
                        };
                        json!({"name": t.name, "host": host, "runtime": if t.acp.is_some() { "acp" } else { "shed" },
                    "labels": t.labels, "state": state, "last": last_line(&on, &w, &t)})
                    }),
            ),
            Err(e) => out.push(json!({"host": host, "error": format!("{e:#}")})),
        }
    }
    out
}

fn last_line(r: &dyn Runner, w: &Wire, t: &proto::TerminalInfo) -> String {
    let acp = t.acp.is_some();
    let p = proto::ReadParams {
        name: t.name.clone(),
        lines: None,
        history: false,
        events: acp,
        before: None,
        n: acp.then_some(50),
    };
    let read = call(r, w, Method::Read, json!(p)).and_then(|v| Ok(serde_json::from_value::<proto::ReadResult>(v)?));
    let Ok(read) = read else {
        return String::new();
    };
    let reply = |e: &proto::Logged| match &e.kind {
        EventKind::Reply { text, .. } => Some(text.clone()),
        _ => None,
    };
    let text = if acp {
        read.events.iter().rev().find_map(reply).unwrap_or_default()
    } else {
        read.text
    };
    text.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .into()
}
