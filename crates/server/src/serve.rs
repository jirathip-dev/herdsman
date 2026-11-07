//! `herdsman serve`, the phone page (docs/serve.md): std `TcpListener`, a thread per request (at most 16, then 503);
//! `/live` SSE clients get a thread each, fed by the 1 s watcher thread.
use crate::api::{
    body, Account, Backlog, BacklogIssue, Balance, Bar, Blocked, Cell, Chat, Conductor, ConfigSheet, Disk, DoneRow,
    Drive, DriveAct, FleetRow, Harness, Herd, HerdFleet, HerdHost, HerdLane, HerdQueued, HostCard, HostTab, LaneNow,
    LaneRepo, LaneRow, LaneSection, LanesTab, Leftovers, Menu, Meter, Moment, NeedAsk, NeedIssue, NeedsTab, Notice,
    Paired, PrLine, QueueCard, QueueLane, QueueRow, QueueTab, RoleRow, Row, Stage, Sub,
};
use crate::config::{Config, Limits, Role};
// #632: every read is the cached log (`events::log`): its events, ask index and fold.
use crate::events::{age, append, log as read, Event, IssueState, States};
use crate::gate::{gate, refused, Refusal};
use crate::host::{self, Host};
use crate::runner::{Out, Runner};
use crate::snapshot::Snapshot;
use crate::{devices, gh, herdr, lanes, queue, Ctx};
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::io::ErrorKind::{TimedOut, WouldBlock};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod chat;
mod v1;

const SW: &str = include_str!("../assets/sw.js");
macro_rules! th {
    ($f:literal, $t:expr) => {
        (
            concat!("themes/", $f),
            $t,
            include_bytes!(concat!("../assets/themes/", $f)),
        )
    };
}
/// #57: served under `/a/<VER>/` as immutable (the SW precaches them); the page links them.
const ASSETS: &[(&str, &str, &[u8])] = &[
    ("serve.css", "text/css", include_bytes!("../assets/serve.css")),
    ("herd.css", "text/css", include_bytes!("../assets/herd.css")),
    ("serve.js", "text/javascript", include_bytes!("../assets/serve.js")),
    ("herd.js", "text/javascript", include_bytes!("../assets/herd.js")),
    // #265: the TypeScript board's build (web/dist, committed: `cargo install` needs no Node).
    ("board.js", "text/javascript", include_bytes!("../web/dist/board.js")),
    ("board.css", "text/css", include_bytes!("../web/dist/board.css")),
    // #172: the built-in themes; `herdsman serve` serves the active one at `/theme/<file>` (`theme`).
    th!("farm/sprites.svg", SVG),
    th!("farm/theme.json", JSON),
    th!("kitchen/sprites.svg", SVG),
    th!("kitchen/theme.json", JSON),
    th!("kitchen/scene.svg", SVG),
    th!("kitchen/icon-180.png", PNG),
    th!("kitchen/icon-192.png", PNG),
    th!("kitchen/icon-512.png", PNG),
    // #423: the houseplant shop (docs/design/garden/build.mjs writes it).
    th!("garden/sprites.svg", SVG),
    th!("garden/theme.json", JSON),
    th!("garden/scene.svg", SVG),
    th!("garden/icon-180.png", PNG),
    th!("garden/icon-192.png", PNG),
    th!("garden/icon-512.png", PNG),
    th!("terminal/theme.json", JSON),
    th!("terminal/icon-180.png", PNG),
    th!("terminal/icon-192.png", PNG),
    th!("terminal/icon-512.png", PNG),
];
/// #244: the built-in colour schemes, `assets/schemes/<name>.json` (docs/themes.md).
macro_rules! sc {
    ($($n:literal),*) => { [$(($n, include_bytes!(concat!("../assets/schemes/", $n, ".json")).as_slice())),*] };
}
const SCHEMES: [(&str, &[u8]); 10] = sc![
    "github-dark",
    "solarized-dark",
    "gruvbox",
    "nord",
    "dracula",
    "paper",
    "farm",
    "kitchen",
    "garden-light",
    "garden-dark"
];
const PNG: &str = "image/png";
const SVG: &str = "image/svg+xml";
const I180: &[u8] = include_bytes!("../assets/icon-180.png");
const I192: &[u8] = include_bytes!("../assets/icon-192.png");
/// At the root, the fixed paths the manifest and iOS name: cached a day (`/sw.js`, VER filled in, is no-store).
const FILES: &[(&str, &str, &[u8])] = &[
    (
        "manifest.webmanifest",
        "application/manifest+json",
        include_bytes!("../assets/manifest.webmanifest"),
    ),
    ("icon-180.png", "image/png", I180),
    ("icon-192.png", "image/png", I192),
    ("icon-512.png", "image/png", include_bytes!("../assets/icon-512.png")),
    // #195: the root icons Safari probes; a 404 there falls back to another service's icon on the same host.
    ("apple-touch-icon.png", "image/png", I180),
    ("apple-touch-icon-precomposed.png", "image/png", I180),
    ("favicon.ico", "image/png", I192),
];
/// FNV-1a over the assets and root files at build time: their `/a/<hash8>/` path and the icons' `?v=<hash8>`.
#[allow(long_running_const_eval)] // ~300 KB of assets outgrow rustc's const-eval step budget
const VER: u32 = {
    let (mut h, mut k, mut i) = (0x811c_9dc5u32, 0, 0);
    while k < ASSETS.len() + FILES.len() {
        let b = if k < ASSETS.len() {
            ASSETS[k].2
        } else {
            FILES[k - ASSETS.len()].2
        };
        // #423: four bytes a step (the garden's sprites outgrow the budget a byte at a time)
        while i < b.len() {
            let w = if i + 4 <= b.len() {
                u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
            } else {
                b[i] as u32
            };
            h = (h ^ w).wrapping_mul(0x0100_0193);
            i += if i + 4 <= b.len() { 4 } else { 1 };
        }
        (k, i) = (k + 1, 0);
    }
    h
};

/// #501: the request's paired device, else why not (`devices::check`).
type Auth = std::result::Result<devices::Device, String>;
/// #501: the devices could not be read: the board is busy (503), not the device unpaired (401).
fn busy(a: &Auth) -> bool {
    a.as_ref().is_err_and(|w| w.starts_with("db error: "))
}
type Clients = Arc<Mutex<Vec<(Sender<String>, Option<String>)>>>;
type Cache = Arc<Mutex<BTreeMap<String, (u64, Option<String>)>>>;
/// A form op for `serve_on`'s job thread: (ctx, form, fleet).
type Job = (Ctx, BTreeMap<String, String>, String);
/// When this board started (the Services board row's uptime).
static STARTED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

pub fn serve(ctx: &Ctx, r: &dyn Runner, addr: &str, read_only: bool) -> Result<()> {
    let l = TcpListener::bind(addr)?;
    let mode = if read_only { "read-only view" } else { "phone page" };
    eprintln!("herdsman serve: {mode} on http://{}/", l.local_addr()?);
    serve_on(ctx, r, l, read_only)
}

#[allow(clippy::needless_pass_by_value)] // public API: the listener is handed over and lives as long as the server
pub fn serve_on(ctx: &Ctx, r: &dyn Runner, l: TcpListener, read_only: bool) -> Result<()> {
    let addr = (!read_only)
        .then(|| l.local_addr().map(|a| a.to_string()))
        .transpose()?;
    let (tx, rx) = channel();
    let s = Server {
        jobs: Some(tx),
        ..Server::new(ctx, r, addr)
    };
    let (h, w) = (
        s.host.clone(),
        Server {
            fresh: true,
            cache: s.cache.clone(),
            clients: s.clients.clone(),
            kick: s.kick.clone(),
            host: s.host.clone(),
            ..Server::new(ctx, r, s.addr.clone())
        },
    );
    let (s, busy) = (&s, &AtomicUsize::new(0));
    STARTED.get_or_init(|| ctx.now);
    std::thread::scope(|sc| {
        // #57: the host every 10 s (its 2 s vm_stat delta included) here, never on a request.
        sc.spawn(move || loop {
            let mut hs = host::sample(r);
            host::window(&ctx.state, &mut hs);
            *lock(&h) = hs;
            std::thread::sleep(Duration::from_secs(10));
        });
        sc.spawn(move || loop {
            w.tick();
            std::thread::sleep(Duration::from_secs(1));
        });
        // #140: drive and the needs-human ops run here, one at a time; the board toasts the result.
        sc.spawn(move || {
            for (c, f, fleet) in rx {
                let msg = json!(s.write(&c, &f).unwrap_or_else(|e| shown(&e)));
                s.kick.store(lock(&s.running).remove(&fleet), Relaxed);
                let id = lock(&s.watch).seen.unwrap_or(0);
                lock(&s.clients).retain(|c| c.0.send(format!("id: {id}\nevent: toast\ndata: {msg}\n\n")).is_ok());
            }
        });
        for mut st in l.incoming().flatten() {
            if busy.fetch_add(1, Relaxed) < 16 {
                sc.spawn(move || (drop(s.handle(st)), busy.fetch_sub(1, Relaxed)));
            } else {
                busy.fetch_sub(1, Relaxed);
                drop(st.write_all(b"HTTP/1.1 503 Service Unavailable\r\nRetry-After: 1\r\nContent-Length: 0\r\n\r\n"));
            }
        }
    });
    Ok(())
}

/// `Server::cached` as a Runner (runner, cache, now, ttl): also `waiting::scan`'s per-issue `git log` on a request.
/// A read the step's snapshot (#337) has comes from it; one it lacks on another host fails, never runs on a request.
struct Memo<'a>(&'a dyn Runner, &'a Cache, u64, u64, Arc<Snapshot>);
impl Runner for Memo<'_> {
    fn run(&self, prog: &str, args: &[String]) -> Result<Out> {
        let (key, cache) = (crate::snapshot::key(prog, args), || lock(self.1));
        match self.4.reads.get(&key) {
            Some(v) => return Ok(crate::snapshot::out(v.clone())),
            None if self.4.ts > 0 && crate::snapshot::remote(prog, args) => return Ok(crate::snapshot::out(None)),
            None => {}
        }
        let hit = cache()
            .get(&key)
            .filter(|(t, _)| self.2 >= *t && self.2 - t < self.3)
            .cloned();
        let (t, out) = hit.unwrap_or_else(|| {
            (
                self.2,
                self.0.run(prog, args).ok().filter(|o| o.code == 0).map(|o| o.stdout),
            )
        });
        cache().insert(key, (t, out.clone()));
        Ok(crate::snapshot::out(out))
    }
    /// #298: a call with stdin (`shed connect`, a file write) is never a cached read.
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> Result<Out> {
        if input.is_empty() {
            return self.run(prog, args);
        }
        self.0.run_in(prog, args, input)
    }
    fn shed(&self) -> Option<crate::shed::Shed> {
        self.0.shed()
    }
    fn secret(&self, svc: &str) -> Result<Option<String>> {
        self.0.secret(svc)
    }
    fn set_secret(&self, svc: &str, value: Option<&str>) -> Result<()> {
        self.0.set_secret(svc, value)
    }
}

/// The mutex's data, even after a thread panicked holding it.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn hash(s: &impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// A menu as its card shows it (masked question and options): `/answer` refuses a tap on any other.
fn seen(q: &str, opts: &[String]) -> u64 {
    hash(&(lanes::mask(q), opts.iter().map(|o| lanes::mask(o)).collect::<Vec<_>>()))
}

/// The watcher's last look: the log's (size, mtime), the `herdr agent list` read (secs, hash), last render, per-(paired, fragment) hashes.
#[derive(Default)]
struct Watch {
    log: Option<(u64, Option<std::time::SystemTime>)>,
    herdr: (u64, u64),
    rendered: u64,
    /// The last `herd` push: (time, hash of the body).
    herd: (u64, u64),
    frags: BTreeMap<(bool, &'static str), u64>,
    /// #267: the hash of the last Host tab data (its `now` left out).
    host: u64,
    /// #268: the Queue tab's likewise.
    queue: u64,
    /// #166: `drive.lock` was there: a pass starting or ending re-renders (`⟳ driving…`).
    driving: bool,
    /// #241, #259: the log's last seq at the last look (none yet: nothing to play).
    seen: Option<u64>,
}

/// #241: the theme moment an event plays on the page (an `ev`'s `k` on `/live`): merged, fail or needs-you.
pub fn moment(e: &Event) -> Option<&'static str> {
    match e.kind.as_str() {
        "merged" => Some("merged"),
        "verdict" if e.str("verdict") == Some("FAIL") => Some("fail"),
        "needs-human" | "needs-decision" | "keychain-locked" => Some("needs-you"),
        _ => None,
    }
}

/// #259: an event on `/live` (`ev`, also replayed after `Last-Event-ID` / `?since=`): its seq, time and moment.
fn sse(e: &Event) -> String {
    format!(
        "id: {}\nevent: ev\ndata: {}\n\n",
        e.seq,
        json!(Moment {
            seq: e.seq,
            ts: e.ts,
            k: moment(e).map(Into::into),
            f: e.fleet.clone(),
            n: e.issue
        })
    )
}

/// #351: a chat's window: the last at most `n` of `v` within `kb` KB, sized as `{:?}` (no less than JSON; at least one).
fn window<T: std::fmt::Debug>(v: &[T], n: usize, kb: usize) -> &[T] {
    let (mut size, mut k) = (0, 0);
    for x in v.iter().rev().take(n) {
        size += format!("{x:?}").len();
        k += usize::from(k == 0 || size <= kb << 10);
    }
    &v[v.len() - k..]
}

pub struct Server<'a> {
    ctx: &'a Ctx,
    r: &'a dyn Runner,
    /// The bind address a POST's `Host` must name; None: read-only.
    addr: Option<String>,
    /// Command stdout by argv, with its time: rate-limited reads, shared with the watcher, which (`fresh`) re-reads at a third of the ttl.
    cache: Cache,
    fresh: bool,
    /// The background sampler's last host read (`serve_on`); a request only reads it.
    pub host: Arc<Mutex<Host>>,
    /// Reply-box send times per device hash (10 a minute).
    sent: Mutex<BTreeMap<String, Vec<u64>>>,
    /// `/live` client threads (at most 8; the paired device's hash, shared with the watcher), the
    /// watcher's state, and `kick`: an answer was tapped, the watcher re-reads panes and pushes now.
    clients: Clients,
    kick: Arc<AtomicBool>,
    watch: Mutex<Watch>,
    /// `serve_on`'s background queue for ops that can spawn or merge, and the fleets with one in flight.
    jobs: Option<Sender<Job>>,
    running: Mutex<BTreeSet<String>>,
    /// `serve --export` (#39): Some(demo), static files with relative URLs; `true` is the inert demo.
    export: Option<bool>,
    /// #337: the step's snapshot as last loaded, by the file's mtime.
    snap: Mutex<(Option<u64>, Arc<Snapshot>)>,
    /// #403: the conductor lines `POST /thread` logged, typed in once its reply is out.
    later: Mutex<Vec<(Ctx, (Event, String))>>,
    /// #620: the [chat] tab's fold, by its key (`chat_tab`).
    chat: Mutex<(serde_json::Value, serde_json::Value)>,
}

const JSON: &str = "application/json; charset=utf-8";
const HTML: &str = "text/html; charset=utf-8";

/// A `303 See Other` to `loc`.
fn see(loc: &str) -> Vec<u8> {
    format!("HTTP/1.1 303 See Other\r\nLocation: {loc}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()
}

fn header<'h>(head: &'h str, name: &str) -> Option<&'h str> {
    head.lines().skip(1).find_map(|l| {
        l.split_once(':')
            .filter(|(k, _)| k.trim().eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim())
    })
}

/// #199: the asking client's address. Behind `tailscale serve` every peer is loopback: then the forwarded address (the
/// proxy appends it last) or the tailnet login; those headers count from a loopback peer only.
fn client(head: &str) -> &str {
    let peer = header(head, "x-peer").unwrap_or_default();
    let fwd = header(head, "x-forwarded-for")
        .and_then(|f| f.rsplit(',').next())
        .map(str::trim);
    let fwd = fwd
        .or_else(|| header(head, "tailscale-user-login"))
        .filter(|f| !f.is_empty());
    peer.parse::<std::net::IpAddr>()
        .is_ok_and(|p| p.is_loopback())
        .then_some(fwd)
        .flatten()
        .unwrap_or(peer)
}

/// `application/x-www-form-urlencoded` (also the query string) into a map.
fn form(s: &str) -> BTreeMap<String, String> {
    let dec = |v: &str| {
        let (b, mut out, mut i) = (v.as_bytes(), vec![], 0);
        while i < b.len() {
            match (b[i], v.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())) {
                (b'%', Some(x)) => (out.push(x), i += 2).1,
                (b'+', _) => out.push(b' '),
                (c, _) => out.push(c),
            }
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    s.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (dec(k), dec(v)))
        .collect()
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// #166, #162 A: a fleet card's drive line: `⟳ driving…`, else the last pass's age and result in its colour
/// (`drive::result`); a tap lists `meta` (stall check, lanes, serial), then each action in plain words, a tap on one
/// its raw code.
fn drive(ctx: &Ctx, events: &[Event], fleet: &str, meta: Vec<String>) -> Drive {
    let pass = events.iter().rev().find(|e| e.kind == "drive-pass" && e.fleet == fleet);
    let (tone, line) = match pass {
        _ if crate::drive::running(ctx, fleet) => ("m", "⟳ driving…".to_string()),
        None => ("a", "drive · no pass yet".to_string()),
        Some(e) => {
            let (tone, line) = crate::drive::result(e);
            let rest = line.split_once(" · ").map_or("", |r| r.1).to_string();
            (
                tone,
                format!("drive · {} ago · {rest}", age(ctx.now.saturating_sub(e.ts))),
            )
        }
    };
    let acts = pass
        .and_then(|e| e.data.get("actions").and_then(serde_json::Value::as_array).cloned())
        .unwrap_or_default();
    let act = |a: &serde_json::Value| DriveAct {
        who: a["issue"]
            .as_u64()
            .map_or_else(|| a["step"].as_str().unwrap_or_default().into(), |n| format!("#{n}")),
        words: a["words"].as_str().unwrap_or_default().into(),
        code: a["code"].as_str().unwrap_or("ok").into(),
    };
    Drive {
        tone: tone.into(),
        line,
        meta,
        pass: pass.is_some(),
        acts: acts.iter().map(act).collect(),
    }
}

/// #386: a thread file's type by its extension.
fn ctype(name: &str) -> &'static str {
    match chat::ext(name).as_str() {
        "png" => PNG,
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "json" => JSON,
        "txt" | "md" | "log" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// fleet.toml's mtime (ns), rendered into every form: a POST refuses STALE when it moved.
fn mtime(ctx: &Ctx) -> String {
    let t = std::fs::metadata(&ctx.config).and_then(|m| m.modified()).ok();
    t.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(String::new(), |d| d.as_nanos().to_string())
}

/// An error as the page shows it: `REFUSED <CODE>: …` or `error: …`.
fn shown(e: &anyhow::Error) -> String {
    e.downcast_ref::<Refusal>()
        .map_or_else(|| format!("error: {e:#}"), std::string::ToString::to_string)
}

/// `v` is `name`, or `name:<port>`.
fn names(name: &str, v: &str) -> bool {
    v == name
        || v.rsplit_once(':')
            .is_some_and(|(n, p)| n == name && p.chars().all(|c| c.is_ascii_digit()))
}

/// green = merged/PASS/ready/working, amber = queued/impl/review/fix/idle, red = blocked/dead/FAIL/needs you.
pub fn tone(state: &str) -> &'static str {
    match state.split('(').next().unwrap_or_default() {
        "merged" | "ready" | "PASS" | "working" | "done" | "ok" | "normal" => "g",
        "blocked"
        | "needs-human"
        | "design-review"
        | "⏸ waiting: decision"
        | "dead"
        | "FAIL"
        | "critical"
        | "capped" => "r",
        "parked" | "asleep" => "m",
        _ => "a",
    }
}

/// Form ops only a paired device may POST.
const PAIRED_OPS: &[&str] = &[
    "unpair", "answer", "drive", "override", "extend", "park", "orphans", "reap", "host", "bqueue", "cond",
];

/// The non-empty fields, ` · `-joined (no `-` placeholders).
fn join(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// An open ask's card (the needs strip's, #517 its issue's queue row's): `who`, its `kind` line, the thread it was
/// asked in.
fn need_ask(now: u64, mins: u64, ev: &[Event], a: &Event) -> NeedAsk {
    let s = |k| a.str(k).unwrap_or_default();
    let rec = a.data["recommend"].as_u64().filter(|_| s("kind") == "routine");
    let left = age((a.ts + 60 * mins).saturating_sub(now));
    let issue = a.issue.map_or(String::new(), |n| format!("#{n}"));
    NeedAsk {
        ask: v1::ask(a),
        who: join(&[&a.fleet, &issue, s("agent"), &age(now.saturating_sub(a.ts))]),
        kind: rec.map_or("owner · waits for you".into(), |n| {
            format!("routine · defaults to {n} in {left}")
        }),
        from: a.str("from").and_then(|f| f.split(':').next()).map(Into::into),
        pngs: chat::files_of(a)
            .map(|f| f["name"].as_str().unwrap_or_default().into())
            .collect(),
        thread: a.str("thread").map(Into::into),
        title: a.str("thread").map(|t| crate::conductor::title(ev, t).into()),
    }
}

/// Corral's 8 coats (HerdArt.swift): body, shade, mane; chestnut, palomino, dun and buckskin carry a blaze.
const COATS: &[(&str, &str, &str, bool)] = &[
    ("8a5a33", "6f4527", "2e2019", false),
    ("a05c2c", "7d4520", "5d3317", true),
    ("3b3b44", "2a2a31", "17161a", false),
    ("b9bcc6", "989ca9", "7d8089", false),
    ("c89a56", "a37a3c", "e8dcc0", true),
    ("b08d5e", "8e6f45", "41321f", true),
    ("96685a", "77524a", "4a342c", false),
    ("bd8f4e", "96703a", "241a10", true),
];

/// The coat as CSS vars, stable per name.
pub fn coat(name: &str) -> String {
    let (c, d, m, blaze) = COATS[usize::from_str_radix(&devices::sha256(name.as_bytes())[..2], 16).unwrap_or(0) % 8];
    format!(
        "--c:#{c};--d:#{d};--m:#{m};--b:{}",
        if blaze { "#efe7dc" } else { "transparent" }
    )
}

/// The Services board row: this board's uptime and version.
fn board(ctx: &Ctx) -> String {
    let up = age(ctx.now.saturating_sub(*STARTED.get_or_init(|| ctx.now)));
    format!("up {up} · v{}", env!("CARGO_PKG_VERSION"))
}

/// `age` since `t`, or nothing when `t` is unknown (0).
fn ago(now: u64, t: u64) -> String {
    if t > 0 {
        age(now.saturating_sub(t))
    } else {
        String::new()
    }
}

/// A lane as the Lanes row, its sheet card and herd.json show it: (cached title, tone, shown state, age).
/// #46: an idle lane shows what it waits on, coloured by who; a working one keeps its state.
type View = (Option<String>, &'static str, String, String);
fn view(ctx: &Ctx, l: &lanes::Lane, waits: &[crate::waiting::Waiting]) -> View {
    let title = l
        .issue
        .zip(l.fleet.as_ref())
        .and_then(|(n, f)| gh::cached_title(ctx, f, n));
    let (tone, shown) = wait_of(l, waits).map_or_else(
        || (tone(&l.state), l.state.clone()),
        |w| (w.tone(ctx.now), w.label(ctx.now)),
    );
    let pr = l
        .issue
        .zip(l.fleet.as_ref())
        .map(|(n, f)| gh::cached_pr(ctx, f, n))
        .filter(|p| p["state"] == "OPEN");
    let on = |h: &str| {
        pr.as_ref().map_or(format!("pushed {}", h.trim()), |p| {
            format!("work on PR #{}", p["number"])
        })
    };
    let (tone, shown) = l
        .parked
        .as_ref()
        .filter(|_| !shown.contains(crate::waiting::SLOT))
        .map_or((tone, shown), |h| ("m", format!("parked · {}", on(h))));
    let kept = |k: &String| format!("{shown} · kept open: {k}");
    let shown = l.kept.as_ref().map_or(shown.clone(), kept);
    (title, tone, shown, ago(ctx.now, l.since))
}

/// The lane's wait reason while herdr shows it idle, done or unknown.
fn wait_of(l: &lanes::Lane, waits: &[crate::waiting::Waiting]) -> Option<crate::waiting::Waiting> {
    let w = waits.iter().find(|w| w.lane == l.name).cloned();
    w.filter(|_| matches!(l.state.as_str(), "idle" | "done" | "unknown" | "parked"))
}

/// #241: the theme action (docs/themes.md) a lane, issue or event shows: its role (`impl`, `fix`, `rev`, `update`)
/// while it works, else `idle`, `done` or `blocked`. `state` is a lane's (herdr) or an issue's (the log).
pub fn act(role: &str, state: &str) -> &'static str {
    let busy = |r: &str| match r {
        "fix" => "fix",
        "update" => "update",
        "rev" | "review" => "rev",
        _ => "impl",
    };
    match state.split('(').next().unwrap_or_default() {
        "blocked" | "dead" | "needs-human" | "⏸ waiting: decision" => "blocked",
        "done" | "ready" | "merged" => "done",
        "working" => busy(role),
        "fix" if role == "update" => "update",
        s @ ("impl" | "fix" | "review") => busy(s),
        _ => "idle",
    }
}

/// The device kind and browser from `User-Agent` ("iPhone Safari"), for the Host tab's device lists.
fn kind(head: &str) -> String {
    let ua = header(head, "user-agent").unwrap_or_default();
    let k = |l: &[&'static str], d| l.iter().copied().find(|k| ua.contains(k)).unwrap_or(d);
    format!(
        "{} {}",
        k(&["iPhone", "iPad", "Android", "Mac", "Windows", "Linux"], "device"),
        k(&["Firefox", "Edg", "Chrome", "Safari"], "")
    )
    .trim_end()
    .into()
}

impl<'a> Server<'a> {
    pub fn new(ctx: &'a Ctx, r: &'a dyn Runner, addr: Option<String>) -> Self {
        Server {
            ctx,
            r,
            addr,
            cache: Arc::default(),
            fresh: false,
            host: Arc::default(),
            sent: Mutex::default(),
            clients: Arc::default(),
            kick: Arc::default(),
            watch: Mutex::default(),
            jobs: None,
            running: Mutex::default(),
            export: None,
            snap: Mutex::default(),
            later: Mutex::default(),
            chat: Mutex::default(),
        }
    }

    fn handle(&self, mut s: TcpStream) -> std::io::Result<()> {
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        // #386: a thread's message carries attached files (base64, at most 10 MB each): its POST may be that long.
        let (mut buf, mut chunk, mut want) = (vec![], vec![0u8; 1 << 16], None);
        loop {
            let end = buf.windows(4).position(|w| w == b"\r\n\r\n");
            if let Some(i) = end.filter(|_| want.is_none()) {
                let h = String::from_utf8_lossy(&buf[..i]);
                let n: usize = header(&h, "content-length").and_then(|n| n.parse().ok()).unwrap_or(0);
                let most = if h.starts_with("POST /thread ") {
                    140 << 20 // #405: up to 10 files a message
                } else if h.starts_with("POST /report ") {
                    3 << 20 // #363: a report carries its screenshot
                } else {
                    32_768
                };
                want = Some((i + 4 + n).min(most));
            }
            if want.map_or(buf.len() >= 32_768, |w| buf.len() >= w) {
                break;
            }
            match s.read(&mut chunk)? {
                0 => break,
                n => buf.extend_from_slice(&chunk[..n]),
            }
        }
        let req = String::from_utf8_lossy(&buf).replacen(
            "\r\n",
            &format!(
                "\r\nX-Peer: {}\r\n",
                s.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default()
            ),
            1,
        );
        let target = req.split_whitespace().nth(1).unwrap_or_default();
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let mut clients = lock(&self.clients);
        clients.retain(|c| c.0.send(String::new()).is_ok());
        let live = req.starts_with("GET ") && path == "/live" && self.allowed(self.ctx, &req) && clients.len() < 8;
        let ctx = Ctx {
            now: crate::now(),
            ..self.ctx.clone()
        };
        // #501: a database error is no unpaired stream: the reply below says 503 and the page reconnects.
        let dev = live.then(|| devices::check(&ctx, header(&req, "cookie").unwrap_or_default()));
        if let Some(dev) = dev.filter(|d| !busy(d)) {
            s.set_write_timeout(Some(Duration::from_secs(10)))?;
            s.set_read_timeout(Some(Duration::from_millis(1)))?;
            let (tx, rx) = channel::<String>();
            clients.push((tx, dev.ok().map(|d| d.hash)));
            let mut msg = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n\r\n: ping\n\nevent: build\ndata: {VER:08x}\n\n"
            );
            // #259: a reconnect replays every event after the last seq it saw; over 500, a full refresh instead.
            let q = form(query);
            let (host, st) = (q.get("tab").is_some_and(|t| t == "host"), self.ctx.state.clone());
            // #407: a stream with a thread open stamps `watch-<thread>.at` each second: no push for it (`events::emit`).
            let t = q
                .get("thread")
                .filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric()));
            let watch = t.map(|t| st.with_file_name(format!("watch-{t}.at")));
            let since = header(&req, "last-event-id").or(q.get("since").map(String::as_str));
            let mut top = 0;
            if let Some(n) = since.and_then(|n| n.parse::<u64>().ok()) {
                let ev = read(&self.ctx.state).unwrap_or_default();
                top = ev.last().map_or(n, |e| e.seq);
                let after: Vec<_> = ev.iter().filter(|e| e.seq > n).collect();
                msg += &match after.last() {
                    Some(l) if after.len() > 500 => format!("id: {0}\nevent: refresh\ndata: {0}\n\n", l.seq),
                    _ => after.into_iter().map(sse).collect(),
                };
            }
            // Each second: a message (an `ev` the replay sent already: skipped), else a peek (the peer closed: free
            // the slot), else a ping every 15 s; #352: a `?tab=host` stream stamps `host-watch.at` (`hosts::sample`).
            std::thread::spawn(move || {
                while s.write_all(msg.as_bytes()).is_ok() {
                    let _ = host.then(|| std::fs::write(st.with_file_name("host-watch.at"), crate::now().to_string()));
                    let t = Instant::now();
                    msg = loop {
                        let _ = watch.as_ref().map(|f| std::fs::write(f, crate::now().to_string()));
                        match rx.recv_timeout(Duration::from_secs(1)) {
                            Ok(m)
                                if !m.is_empty()
                                    && !m
                                        .strip_prefix("id: ")
                                        .and_then(|r| r.split_once("\nevent: ev\n"))
                                        .is_some_and(|(id, _)| id.parse::<u64>().is_ok_and(|i| i <= top)) =>
                            {
                                break m
                            }
                            Err(RecvTimeoutError::Disconnected) => return,
                            _ if s
                                .peek(&mut [0])
                                .map_or_else(|e| !matches!(e.kind(), WouldBlock | TimedOut), |n| n == 0) =>
                            {
                                return
                            }
                            _ if t.elapsed() >= Duration::from_secs(15) => break ": ping\n\n".into(),
                            _ => {}
                        }
                    };
                }
            });
            return Ok(());
        }
        drop(clients);
        s.write_all(&self.reply(&req, crate::now()))?;
        drop(s);
        self.deliver();
        Ok(())
    }

    /// #403: type in the lines `POST /thread` logged (`chat::deliver`).
    fn deliver(&self) {
        let jobs = std::mem::take(&mut *lock(&self.later));
        for (c, job) in jobs {
            chat::deliver(&c, self.r, &job);
            self.kick.store(true, Relaxed);
        }
    }

    /// The watcher's 1 s tick (docs/serve.md): on a change, re-render per audience and push only the changed fragments.
    fn tick(&self) {
        let mut w = lock(&self.watch);
        let now = crate::now();
        let known: Vec<String> = devices::load(self.ctx).into_iter().map(|d| d.hash).collect();
        lock(&self.clients).retain(|c| c.1.as_ref().is_none_or(|h| known.contains(h)));
        let log = Some((crate::db::last_seq(&self.ctx.state), None));
        let kicked = self.kick.swap(false, Relaxed);
        if kicked {
            self.purge();
        }
        let driving = crate::drive::running(self.ctx, "");
        if log != w.log {
            let ev = read(&self.ctx.state).unwrap_or_default();
            let top = ev.last().map_or(0, |e| e.seq);
            for e in ev.iter().filter(|e| e.seq > w.seen.unwrap_or(top)) {
                lock(&self.clients).retain(|c| c.0.send(sse(e)).is_ok());
            }
            w.seen = Some(top);
        }
        let mut changed = kicked || log != w.log || driving != w.driving || now >= w.rendered + 15;
        (w.log, w.driving) = (log, driving);
        // #337: a new snapshot is the change to show; none saved, herdr's agent list every 2 s as before.
        let at = self.snapshot().0;
        if at.is_some() {
            changed |= hash(&at) != w.herdr.1;
            w.herdr = (now, hash(&at));
        } else if now >= w.herdr.0 + 2 {
            let out = herdr::agent_list(self.r);
            changed |= hash(&out) != w.herdr.1;
            w.herdr = (now, hash(&out));
            lock(&self.cache).insert("herdr agent list".into(), (now, out));
        }
        // #37: Herd's data rides the same stream, re-read every 10 s (host weather) while anyone listens;
        // like a frag, pushed only on a change (the page reads `/herd.json` when Herd opens).
        let at = &Ctx {
            now,
            ..self.ctx.clone()
        };
        if (changed || now >= w.herd.0 + 10) && !lock(&self.clients).is_empty() {
            let j = self.herd(at, None).map(|v| json!(v).to_string()).unwrap_or_default();
            if w.herd.0 > 0 && hash(&j) != w.herd.1 && !j.is_empty() {
                let msg = format!("id: {}\nevent: herd\ndata: {j}\n\n", w.seen.unwrap_or(0));
                lock(&self.clients).retain(|c| c.0.send(msg.clone()).is_ok());
            }
            w.herd = (now, hash(&j));
        }
        // #407: a turn that ended (the log or herdr changed) gets its thread's queued messages.
        let agents = (changed && self.addr.is_some())
            .then(|| self.herdr_json(now, "agent"))
            .flatten();
        for job in agents
            .and_then(|j| j["result"]["agents"].as_array().map(|a| chat::flush(at, a)))
            .unwrap_or_default()
        {
            chat::deliver(at, self.r, &job);
        }
        if !changed {
            return;
        }
        w.rendered = now;
        // #267: the Host tab is the TypeScript board's: a change (like a frag's, never the first render) sends its
        // data (`host`), the same to every device.
        let mut h = self.host_tab(at);
        h.now = 0;
        let j = hash(&json!(h).to_string());
        if ![0, j].contains(&std::mem::replace(&mut w.host, j)) {
            h.now = now;
            let msg = format!("id: {}\nevent: host\ndata: {}\n\n", w.seen.unwrap_or(0), json!(h));
            lock(&self.clients).retain(|c| c.0.send(msg.clone()).is_ok());
        }
        // #268: the Queue tab's likewise (`queue`).
        let mut q = self.queue_tab(at);
        q.now = 0;
        let j = hash(&json!(q).to_string());
        if ![0, j].contains(&std::mem::replace(&mut w.queue, j)) {
            q.now = now;
            let msg = format!("id: {}\nevent: queue\ndata: {}\n\n", w.seen.unwrap_or(0), json!(q));
            lock(&self.clients).retain(|c| c.0.send(msg.clone()).is_ok());
        }
        // #269: the needs cards, Lanes and [chat] likewise, per audience (an unpaired device sees no pane text).
        for paired in [false, true] {
            for (id, j) in self.parts(at, paired) {
                let h = hash(&j.replace(&format!("\"now\":{now},"), ""));
                if w.frags.insert((paired, id), h).is_some_and(|o| o != h) {
                    let msg = format!("id: {}\nevent: {id}\ndata: {j}\n\n", w.seen.unwrap_or(0));
                    lock(&self.clients).retain(|c| c.1.is_some() != paired || c.0.send(msg.clone()).is_ok());
                }
            }
        }
    }

    /// Forget cached pane reads: the next render reads each pane afresh.
    fn purge(&self) {
        lock(&self.cache).retain(|k, _| !k.starts_with("herdr pane read"));
    }

    /// The runner whose reads that ran OK within the last `ttl` seconds come from the cache (else run now).
    fn cached(&self, now: u64, ttl: u64) -> Memo<'_> {
        Memo(
            self.r,
            &self.cache,
            now,
            if self.fresh { ttl / 3 } else { ttl },
            self.snapshot().1,
        )
    }

    /// #337: the step's snapshot (reloaded when a new one is saved) and its version (#344); none: empty.
    fn snapshot(&self) -> (Option<u64>, Arc<Snapshot>) {
        let at = crate::db::stamp(&self.ctx.state, "board");
        let mut s = lock(&self.snap);
        if s.0 != at {
            *s = (at, Arc::new(crate::snapshot::load(self.ctx).unwrap_or_default()));
        }
        s.clone()
    }

    /// The full HTTP response to one request, its conductor deliveries done (the socket's `handle` sends it first).
    pub fn respond(&self, req: &str, now: u64) -> Vec<u8> {
        let res = self.reply(req, now);
        self.deliver();
        res
    }

    fn reply(&self, req: &str, now: u64) -> Vec<u8> {
        let (head, body) = req.split_once("\r\n\r\n").unwrap_or((req, ""));
        let mut w = head.split_whitespace();
        let (method, target) = (w.next().unwrap_or_default(), w.next().unwrap_or_default());
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let ctx = &Ctx {
            now,
            ..self.ctx.clone()
        };
        let text = |s: &str| s.as_bytes().to_vec();
        let fail = |e: anyhow::Error| {
            (
                "500 Internal Server Error",
                "text/plain",
                format!("{e:#}\n").into_bytes(),
            )
        };
        let (mut extra, mut cache) = (String::new(), "no-store");
        // #67: a fetch-only read never becomes the page; `fetch()` sends neither header.
        let nav = header(head, "sec-fetch-mode") == Some("navigate")
            || header(head, "sec-fetch-dest") == Some("document")
            || header(head, "accept").is_some_and(|a| a.starts_with("text/html"));
        if method == "GET" && nav && ["/herd.json", "/lane", "/frag"].contains(&path) {
            return see("/");
        }
        // Host/Origin-checked: the live stream, transcripts, Herd's data and every write.
        let guarded = method == "GET"
            && (["/live", "/lane", "/herd.json"].contains(&path)
                || path.starts_with("/thread/")
                || path.starts_with("/design/")
                || path.starts_with("/shot/"))
            || method == "POST" && self.addr.is_some();
        let paired_op = form(body.trim())
            .get("op")
            .is_some_and(|o| PAIRED_OPS.contains(&o.as_str()));
        let loopback =
            header(head, "host").is_some_and(|h| ["127.0.0.1", "localhost", "[::1]"].iter().any(|l| names(l, h)));
        // #501: the request's paired device, read once: a database error answers 503 ("board busy"), never 401.
        let a = OnceCell::new();
        let dev = || self.auth(ctx, head, &a).as_ref().ok();
        let deny = |msg: &str| {
            let busy = a.get().is_some_and(busy);
            let status = if busy {
                "503 Service Unavailable"
            } else {
                "401 Unauthorized"
            };
            (
                status,
                "text/plain",
                text(if busy { "board busy, retrying\n" } else { msg }),
            )
        };
        let (status, ctype, body) = match (method, path) {
            (_, p) if p.starts_with("/api/") => self.api(ctx, head, (method, p), query, body, self.auth(ctx, head, &a)),
            ("POST", _) if self.addr.is_none() => ("405 Method Not Allowed", "text/plain", text("read-only: GET only\n")),
            _ if guarded && !self.allowed(ctx, head) => ("403 Forbidden", "text/plain", text("forbidden: foreign Host or Origin\n")),
            // #501: `/pair/status` asks whether this browser is paired (no secrets): the page asks it on a 401.
            ("GET", "/live" | "/pair/status") if busy(self.auth(ctx, head, &a)) => deny(""),
            ("GET", "/live") => ("503 Service Unavailable", "text/plain", text("8 live clients already: the page polls instead\n")),
            ("GET", "/") => self.page(ctx, &form(query), dev().is_some())
                .map_or_else(fail, |b| ("200 OK", HTML, b.into_bytes())),
            ("GET", "/frag") => Self::part(ctx, &form(query))
                .map_or(("404 Not Found", "text/plain", text("no such part\n")), |h| ("200 OK", HTML, h.into_bytes())),
            ("GET", "/lane") if dev().is_none() => deny("pair this device to read transcripts\n"),
            ("GET", "/pair/status") => ("200 OK", JSON, json!({"paired": dev().is_some()}).to_string().into_bytes()),
            ("GET", "/lane") => self.transcript(ctx, &form(query)).map_or(
                ("404 Not Found", "text/plain", text("no such agent pane\n")),
                |j| ("200 OK", JSON, j.into_bytes()),
            ),
            ("GET", "/herd.json") => {
                let weather = form(query).get("weather").filter(|_| loopback).cloned();
                self.herd(ctx, weather.as_deref()).map_or_else(fail, |j| ("200 OK", JSON, json!(j).to_string().into_bytes()))
            }
            // #241: any theme by name (the phone picks it), theme.json resolved; an unknown name is farm.
            ("GET", p) if p.starts_with("/themes/") => match p[8..].split_once('/').filter(|(n, f)| !n.is_empty() && THEME.contains(f)) {
                Some((n, f)) => theme(ctx, n, f).0.map_or(("404 Not Found", "text/plain", text("not in this theme\n")), |b| {
                    ("200 OK", [SVG, JSON, PNG][usize::from(f.ends_with("json")) + 2 * usize::from(f.ends_with("png"))], b)
                }),
                None => ("404 Not Found", "text/plain", text("not a theme file\n")),
            },
            // #386: a file a thread's message carries.
            ("GET", p) if p.starts_with("/thread/") => {
                let (t, name) = p[8..].split_once('/').unwrap_or_default();
                let name = form(&format!("n={name}")).remove("n").unwrap_or_default();
                let at = read(&ctx.state).ok().and_then(|ev| chat::file(ctx, &ev, t, &name, false));
                match at.and_then(|p| std::fs::read(p).ok()) {
                    Some(b) => ("200 OK", ctype(&name), b),
                    None => ("404 Not Found", "text/plain", text("no such file in the thread\n")),
                }
            }
            // #412: a PNG of the design waiting for the owner's decision.
            ("GET", p) if p.starts_with("/design/") => {
                let mut it = p[8..].splitn(3, '/');
                let (f, n, i) = (it.next().unwrap_or_default(), it.next().unwrap_or_default(), it.next().unwrap_or_default());
                let f = form(&format!("f={f}")).remove("f").unwrap_or_default();
                let ask = |e: &&Event| crate::design::is_ask(e) && e.fleet == f && e.issue.map(|n| n.to_string()).as_deref() == Some(n);
                let ev = read(&ctx.state).unwrap_or_default();
                let at = ev.iter().rfind(ask).and_then(|a| chat::files_of(a).nth(i.parse().ok()?)?["path"].as_str().map(String::from));
                match at.and_then(|p| std::fs::read(p).ok()) {
                    Some(b) => ("200 OK", PNG, b),
                    None => ("404 Not Found", "text/plain", text("no such design file\n")),
                }
            }
            // #417: a screenshot the issue's last verdict lists.
            ("GET", p) if p.starts_with("/shot/") => {
                let mut it = p[6..].splitn(3, '/');
                let (f, n, i) = (it.next().unwrap_or_default(), it.next().unwrap_or_default(), it.next().unwrap_or_default());
                let f = form(&format!("f={f}")).remove("f").unwrap_or_default();
                let ev = read(&ctx.state).unwrap_or_default();
                let shots = n.parse().map(|n| crate::verify::served(&ev, (f.as_str(), n))).unwrap_or_default();
                let at = i.parse().ok().and_then(|i: usize| shots.into_iter().nth(i));
                match at.and_then(|(_, p)| std::fs::read(p).ok()) {
                    Some(b) => ("200 OK", PNG, b),
                    None => ("404 Not Found", "text/plain", text("no such screenshot\n")),
                }
            }
            ("GET", "/sw.js") => ("200 OK", "text/javascript", text(&SW.replace("@V", &format!("{VER:08x}")))),
            ("GET", p) => {
                let a = ASSETS.iter().find(|a| p == format!("/a/{VER:08x}/{}", a.0)).inspect(|_| cache = "public, max-age=31536000, immutable");
                a.or_else(|| FILES.iter().find(|f| p.strip_prefix('/') == Some(f.0)).inspect(|_| cache = "public, max-age=86400")).map_or(("404 Not Found", "text/plain", text("not found\n")), |f| ("200 OK", f.1, f.2.to_vec()))
            }
            ("POST", "/") if paired_op && dev().is_none() => deny("only a paired device may unpair, answer or drive\n"),
            ("POST", "/") => {
                let msg = self.op(ctx, form(body.trim())).unwrap_or_else(|e| shown(&e));
                return see(&format!("/?msg={}", enc(&msg)));
            }
            // #59: the request's `hd=` token (10 min) works once `serve approve` clears its code; `/pair/claim` then keeps it a year.
            // #199: a re-tap replaces this browser's own open request, so repeated taps never lock it out. #288: opened on the
            // host itself (a loopback peer naming localhost, no `tailscale serve` headers), the reply adds the approve line.
            ("POST", "/pair/request") => match devices::cancel(ctx, header(head, "cookie").unwrap_or_default())
                .and_then(|()| devices::request(ctx, &kind(head), client(head)))
            {
                Ok(Some((code, t))) => {
                    extra = format!("Set-Cookie: hd={t}; Path=/; Max-Age=600; HttpOnly; Secure; SameSite=Strict\r\n");
                    let hint = Config::load(&ctx.config).ok().and_then(|c| c.serve.approve_hint).unwrap_or_default();
                    let host = loopback && client(head).parse::<std::net::IpAddr>().is_ok_and(|p| p.is_loopback());
                    let line = if host { format!("herdsman serve approve {code}\n") } else { String::new() };
                    ("200 OK", "text/plain", format!("{code}\n{}\n{line}", hint.replace("{code}", &code)).into_bytes())
                }
                Ok(None) => ("429 Too Many Requests", "text/plain", text("4 pairing requests open (2 per address), or a code clash: approve one, tap Pair again or wait 10 min\n")),
                Err(e) => fail(e),
            },
            ("POST", "/pair/cancel") => match devices::cancel(ctx, header(head, "cookie").unwrap_or_default()) {
                Ok(()) => ("200 OK", "text/plain", text("pairing request cancelled\n")),
                Err(e) => fail(e),
            },
            ("POST", "/pair/claim") => match dev() {
                Some(d) => (extra = format!("Set-Cookie: {}; Path=/; Max-Age=31536000; HttpOnly; Secure; SameSite=Strict\r\n", header(head, "cookie").unwrap_or_default().split(';').map(str::trim).find(|c| c.starts_with("hd=")).unwrap_or_default()), ("200 OK", "text/plain", format!("paired as {}\n", d.label).into_bytes())).1,
                None => ("403 Forbidden", "text/plain", text("not approved yet: `herdsman serve approve <code>` on the Mac\n")),
            },
            ("POST", "/push/subscribe" | "/push/unsubscribe") if dev().is_none() => deny("pair this device first\n"),
            ("POST", "/push/subscribe") => {
                let sub = Config::load(&ctx.config).ok().and_then(|c| c.webpush_sub().map(String::from));
                match crate::webpush::subscribe(&ctx.state, &form(body.trim()), &kind(head), now, sub.as_deref()) {
                    Ok(msg) => ("200 OK", "text/plain", format!("{msg}\n").into_bytes()),
                    Err(e) => ("400 Bad Request", "text/plain", format!("{}\n", shown(&e)).into_bytes()),
                }
            }
            ("POST", "/push/unsubscribe") => {
                let id = form(body.trim()).get("id").cloned().unwrap_or_default();
                let msg = crate::webpush::remove(&ctx.state, &[id]).map_or_else(|e| shown(&e), |n| format!("{n} alert device removed"));
                return see(&format!("/?msg={}", enc(&msg)));
            }
            ("POST", p @ ("/send" | "/answer" | "/act" | "/setup" | "/thread" | "/report")) => {
                let (status, msg) = self.send(ctx, head, &form(body.trim()), p, &a);
                (status, "text/plain", format!("{msg}\n").into_bytes())
            }
            ("POST", _) => ("404 Not Found", "text/plain", text("not found\n")),
            _ => ("405 Method Not Allowed", "text/plain", text("GET or POST only\n")),
        };
        // #501: the page's sign that this browser is paired after all (serve.js `repaired`).
        if a.get().is_some_and(Result::is_ok) {
            extra += "X-Paired: 1\r\n";
        }
        if ["401", "503"].iter().any(|c| status.starts_with(c)) {
            let why = match a.get() {
                Some(Err(w)) if status.starts_with("401") || w.starts_with("db error: ") => w.clone(),
                _ => String::from_utf8_lossy(&body)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            };
            devices::denied(ctx, &status[..3], method, path, &why);
        }
        let allow = if self.addr.is_some() { "GET, POST" } else { "GET" };
        let mut res = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nAllow: {allow}\r\n{extra}Cache-Control: {cache}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        res.extend(body);
        res
    }

    /// `Host` names the bind address or a `[serve].allowed_hosts` name, and any `Origin` is one of those.
    fn allowed(&self, ctx: &Ctx, head: &str) -> bool {
        let mut hosts = vec![self.addr.clone().unwrap_or_default()];
        hosts.extend(
            Config::load(&ctx.config)
                .map(|c| c.serve.allowed_hosts)
                .unwrap_or_default(),
        );
        let host = header(head, "host").unwrap_or_default();
        let origin =
            header(head, "origin").map(|o| o.strip_prefix("https://").or(o.strip_prefix("http://")).unwrap_or("\0"));
        hosts.iter().any(|h| names(h, host)) && origin.is_none_or(|o| hosts.iter().any(|h| names(h, o)))
    }

    /// The Pair this phone button (#59), on the live, writable page only.
    fn pairb(&self) -> &'static str {
        if self.addr.is_some() && self.export.is_none() {
            " <button type=button class=pairb>Pair this phone</button>"
        } else {
            ""
        }
    }

    /// The paired device whose token the request's `Cookie` carries, else why not (`devices::check`), read once into `a`.
    #[allow(clippy::unused_self)] // a method so every route reads `self.auth(..)` alike
    fn auth<'c>(&self, ctx: &Ctx, head: &str, a: &'c OnceCell<Auth>) -> &'c Auth {
        a.get_or_init(|| devices::check(ctx, header(head, "cookie").unwrap_or_default()))
    }

    /// The reply box (or a blocked menu's option, a sheet action, a report): a paired device (cookie) prompts one agent, at most 10 sends a minute.
    fn send(
        &self,
        ctx: &Ctx,
        head: &str,
        f: &BTreeMap<String, String>,
        path: &str,
        a: &OnceCell<Auth>,
    ) -> (&'static str, String) {
        let dev = match self.auth(ctx, head, a) {
            Ok(d) => d,
            e if busy(e) => return ("503 Service Unavailable", "board busy, retrying".into()),
            Err(_) => return ("401 Unauthorized", "pair this device: tap Pair this phone".into()),
        };
        let mut sent = lock(&self.sent);
        let times = sent.entry(dev.hash.clone()).or_default();
        times.retain(|t| ctx.now < t + 60);
        if times.len() >= 10 {
            return ("429 Too Many Requests", "10 sends a minute: wait a moment".into());
        }
        times.push(ctx.now);
        let (pick, act) = (path == "/answer", path == "/act");
        let res = if path == "/thread" {
            let mut later = vec![];
            let res = chat::post(ctx, self.r, f, &mut later);
            lock(&self.later).extend(later.into_iter().map(|l| (ctx.clone(), l)));
            res
        } else if path == "/setup" {
            let (via, s) = (format!("web {}", dev.label), body::<crate::api::Setup>(f));
            Config::load(&ctx.config).and_then(|c| crate::setup::start(ctx, &c, self.r, &s.host, &s.name, &via))
        } else if pick {
            self.pick(ctx, dev, &body(f))
        } else if act {
            self.act(ctx, dev, &body(f))
        } else if path == "/report" {
            Config::load(&ctx.config).and_then(|c| crate::report::save(ctx, &c, f, &dev.label))
        } else {
            let s = body::<crate::api::Send>(f);
            s.setup.as_deref().map_or_else(
                || self.prompt(ctx, dev, &s),
                |p| crate::setup::send(ctx, self.r, p, &s.text),
            )
        };
        if pick || act || path == "/thread" {
            self.purge();
            self.kick.store(true, Relaxed);
        }
        res.map_or_else(|e| ("409 Conflict", shown(&e)), |msg| ("200 OK", msg))
    }

    /// The live fleet lane `agent` with its fleet and pane; refused PAUSED when its fleet is.
    fn lane(&self, ctx: &Ctx, agent: &str) -> Result<(lanes::Lane, String, String)> {
        let (cfg, events) = (Config::load(&ctx.config)?, read(&ctx.state)?);
        let all = self
            .lanes(ctx.now, &cfg, &events, &events.states)
            .context("herdr agent list failed")?;
        let l = all
            .into_iter()
            .find(|l| l.name == agent)
            .with_context(|| format!("no agent `{agent}`"))?;
        let (Some(fleet), Some(pane)) = (l.fleet.clone(), l.pane.clone()) else {
            bail!("`{agent}` is not a live fleet lane")
        };
        if cfg.fleet.get(&fleet).is_some_and(|f| f.paused) {
            return refused("PAUSED", format!("{fleet} is paused (`herdsman resume {fleet}`)"));
        }
        Ok((l, fleet, pane))
    }

    /// A blocked lane's menu option (`Harness::pending_menu`): its keys (`answer_keys`) via `pane send-keys`, then a re-read ("sent" or
    /// "still waiting"); logs an `answer` event without the question. No menu parsed, or not the
    /// menu the tapped card showed (`h`, read afresh): no key.
    fn pick(&self, ctx: &Ctx, dev: &devices::Device, f: &crate::api::Answer) -> Result<String> {
        let (agent, choice) = (f.agent.as_str(), f.choice.as_str());
        let (l, fleet, pane) = self.lane(ctx, agent)?;
        let on = l.on(self.r);
        let screen = || herdr::read(&on, &pane);
        let h = crate::harness::get(&l.harness);
        let menu = h.pending_menu(&screen()?);
        let Some((q, opts)) = menu.filter(|p| !p.1.is_empty()) else {
            return refused(
                "BAD_VALUE",
                format!("no menu on {agent}'s pane: menu changed — refreshed; answer with the reply box"),
            );
        };
        if reviewer(ctx, &fleet, &l.role) && !q.starts_with("Do you want") {
            return refused("REV_READONLY", "reviewer lanes answer permission prompts only");
        }
        if !choice
            .parse()
            .is_ok_and(|n: usize| (1..=opts.len().min(9)).contains(&n))
            || choice.len() != 1
        {
            return refused("BAD_VALUE", format!("choose 1 to {}", opts.len()));
        }
        if f.h != seen(&q, &opts).to_string() {
            return refused("STALE", "menu changed — refreshed");
        }
        herdr::keys(&on, &pane, &h.answer_keys(choice.parse()?))?;
        let d = serde_json::json!({"via": "web", "agent": agent, "device": dev.label, "choice": choice});
        append(&ctx.state, &Event::new(ctx.now, &fleet, l.issue, "answer", d))?;
        // #337: a board tap is a kick: the snapshot is looked at again at once.
        if let Some(n) = l.issue {
            crate::drive::kick(ctx, self.r, &fleet, n, "board");
        }
        self.r.sleep(1);
        Ok(if h.pending_menu(&screen()?) == Some((q, opts)) {
            format!("still waiting: {agent}")
        } else {
            format!("sent to {agent}")
        })
    }

    /// `herdr agent prompt` (dsh: `pane send-text`) + Enter; logs a `prompt` event without the text.
    fn prompt(&self, ctx: &Ctx, dev: &devices::Device, f: &crate::api::Send) -> Result<String> {
        let (agent, text) = (f.agent.as_str(), f.text.as_str());
        let (l, fleet, pane) = self.lane(ctx, agent)?;
        let (fleet, pane) = (&fleet, &pane);
        if reviewer(ctx, fleet, &l.role) {
            return refused("REV_READONLY", "reviewer lanes are not steered mid-review");
        }
        let n = text.chars().count();
        if text.trim().is_empty() || n > 2000 {
            return refused("BAD_VALUE", format!("a message is 1 to 2000 chars, not {n}"));
        }
        if text.chars().any(|c| (c < ' ' && c != '\n') || c == '\x7f') {
            return refused("BAD_VALUE", "a message has no control characters (newlines are fine)");
        }
        herdr::reply(&l.on(self.r), l.harness == "dsh", agent, pane, text)?;
        let d = serde_json::json!({"via": "web", "agent": agent, "device": dev.label, "chars": n});
        append(&ctx.state, &Event::new(ctx.now, fleet, l.issue, "prompt", d))?;
        if let Some(n) = l.issue {
            crate::drive::kick(ctx, self.r, fleet, n, "board");
        }
        Ok(format!("sent to {agent}"))
    }

    /// A sheet action (`herdsman stop|kill|clean --lane`, or `close <pane>` for an Others agent; kill and clean dry-run without `confirm=1`); logs `act` unless a dry run.
    fn act(&self, ctx: &Ctx, dev: &devices::Device, f: &crate::api::Act) -> Result<String> {
        let (mut out, cfg) = (vec![], Config::load(&ctx.config)?);
        let (agent, yes, op) = (f.agent.as_str(), f.confirm == "1", f.op.as_str());
        let sp = crate::events::last_spawn(&read(&ctx.state)?, agent).map(|e| (e.fleet.clone(), e.issue));
        let Some((fleet, issue)) = sp.or_else(|| (op == "close").then(|| (String::new(), None))) else {
            return refused("NO_SUCH_AGENT", format!("no lane `{agent}` in the log"));
        };
        match op {
            "stop" => crate::check::stop(ctx, &cfg, self.r, &mut out, agent)?,
            "kill" => crate::check::kill(ctx, &cfg, self.r, &mut out, (agent, None), yes)?,
            "clean" => crate::check::clean(
                ctx,
                &cfg,
                self.r,
                &mut out,
                Some(&fleet),
                (None, Some(agent), false),
                yes,
            )?,
            "close" => crate::check::close(ctx, &cfg, self.r, &mut out, agent, None)?,
            op => return refused("BAD_VALUE", format!("op is stop, kill, clean or close, not `{op}`")),
        }
        if yes || op == "stop" {
            let d = serde_json::json!({"via": "web", "agent": agent, "device": dev.label, "op": op});
            append(&ctx.state, &Event::new(ctx.now, &fleet, issue, "act", d))?;
        }
        Ok(String::from_utf8_lossy(&out).trim().replace('\n', "; "))
    }

    /// The sheet's transcript: up to 200 masked ANSI lines `before` the newest (load more) of the lane's rolling
    /// transcript (#138; the export reads the screen only), the live block's length and the status line, as JSON.
    fn transcript(&self, ctx: &Ctx, q: &BTreeMap<String, String>) -> Option<String> {
        if let Some(p) = q.get("setup") {
            return crate::setup::screen(ctx, self.r, p).ok().map(|j| j.to_string());
        }
        let n = |k: &str, d: usize| q.get(k).and_then(|v| v.parse().ok()).unwrap_or(d);
        let (lines, before) = (n("lines", 200).min(200), n("before", 0));
        let (cfg, events) = (Config::load(&ctx.config).ok()?, read(&ctx.state).ok()?);
        let agent = q.get("agent")?;
        let ls = self.lanes(ctx.now, &cfg, &events, &events.states)?;
        let file = lanes::log(&ctx.state, agent).filter(|_| self.export.is_none());
        // #218: a lane with no pane: its stored transcript (#248: Claude's saved session first), no live block.
        let Some(l) = ls.into_iter().find(|l| &l.name == agent && l.pane.is_some()) else {
            let wt = crate::events::last_spawn(&events, agent).and_then(|e| e.str("worktree"));
            let saved = wt.and_then(|wt| lanes::session(&ctx.home, wt));
            let rolled = || Some(std::fs::read_to_string(file?).ok()?.lines().map(String::from).collect());
            let all: Vec<String> = saved.filter(|_| self.export.is_none()).or_else(rolled)?;
            let page = window(&all[..all.len().saturating_sub(before)], lines, 32);
            return serde_json::to_string(&Chat {
                lines: page.to_vec(),
                before: before + page.len(),
                more: all.len() > before + page.len(),
                ..Chat::default()
            })
            .ok();
        };
        // #337: a lane's chat always reads its pane live (5 s cache), never from the snapshot.
        let live = Memo(self.r, &self.cache, ctx.now, 5, Arc::default());
        let screen = herdr::recent(&l.on(&live), l.pane.as_deref()?, "200").ok()?;
        let body = lanes::body(&screen);
        let (all, live) = file.map_or((body.clone(), body.len()), |f| lanes::roll(&f, &body));
        let end = all.len().saturating_sub(before);
        let page = window(&all[..end], lines, 32);
        let status = lanes::chat(&screen).1.map(lanes::strip);
        // #260: the default view's source: Claude's session transcript, else (null) the pane's role runs.
        let ok = self.export.is_none();
        // #351: the last `turns` (10, more as the sheet scrolls up) within 3.2 KB each: a chat opens with ~64 KB at most.
        let said = l.session().filter(|_| ok).and_then(|p| lanes::read_tail(&p, 1 << 21));
        let (said, turns) = (said.map(|s| lanes::said(&s).0), n("turns", 10));
        let win = said.as_deref().map(|s| window(s, turns, turns * 16 / 5));
        let note = |e: &&Event| {
            let t = match e.kind.as_str() {
                _ if Some(&e.fleet) != l.fleet.as_ref() || e.issue != l.issue => None,
                "spawn" => Some(format!("spawned {}", e.str("agent")?)),
                "needs-decision" => Some(format!("ask: {}", e.str("question")?)),
                "verdict" => Some(format!("verdict {}", e.str("verdict")?)),
                k => (k == "merged").then(|| k.into()),
            };
            Some(Notice {
                at: e.ts,
                text: lanes::mask(&t?),
            })
        };
        let notices: Vec<_> = events.iter().filter_map(|e| note(&e)).collect();
        let acts: &[&str] = if l.role == "other" {
            &["close"]
        } else {
            &["stop", "kill", "clean"]
        };
        let older = win.zip(said.as_ref()).is_some_and(|(w, s)| w.len() < s.len());
        serde_json::to_string(&Chat {
            said: win.map(<[_]>::to_vec),
            older,
            notices,
            harness: l.harness.clone(),
            acts: acts.iter().map(|&a| a.into()).collect(),
            pane: l.pane.clone().filter(|_| l.role == "other"),
            lines: page.to_vec(),
            live: if before == 0 { live.min(page.len()) } else { 0 },
            status,
            before: before + page.len(),
            more: end > page.len(),
        })
        .ok()
    }

    /// Run one form write; the line shown on the page (CLI output, or `REFUSED <CODE>: …` via `shown`).
    fn write(&self, ctx: &Ctx, f: &BTreeMap<String, String>) -> Result<String> {
        let g = |k: &str| f.get(k).map_or("", String::as_str);
        if g("mtime") != mtime(ctx) {
            return refused(
                "STALE",
                "fleet.toml changed since the page was rendered; reload and retry",
            );
        }
        // #66: a short line for the toast, not the CLI's stdout (a queue move prints the whole queue).
        let (fleet, mut out) = (g("fleet"), vec![]);
        Ok(match g("op") {
            "mv" => {
                let n = |k: &str| g(k).parse::<u64>().ok();
                let issue = n("issue").context("no issue")?;
                let d = queue::op("mv", &[issue], false, n("before"), n("after"), None);
                crate::queue_change(ctx, &Config::load(&ctx.config)?, fleet, d, Some("web"))?;
                crate::drive::kick(ctx, self.r, fleet, issue, "board");
                format!(
                    "moved #{issue} {}",
                    n("before").map_or_else(|| format!("after #{}", g("after")), |b| format!("before #{b}"))
                )
            }
            op @ ("pause" | "resume") => {
                crate::pause(ctx, &mut out, fleet, op == "pause", Some("web"))?;
                format!("{fleet} {op}d")
            }
            op @ ("set" | "cond") => {
                let wanted: Vec<_> = ["harness", "model", "effort"]
                    .into_iter()
                    .filter(|k| !g(k).is_empty())
                    .map(|k| (k, g(k).to_string()))
                    .collect();
                if op == "cond" {
                    return crate::conductor::set(ctx, self.r, fleet, &wanted); // #401
                }
                let line = wanted
                    .iter()
                    .map(|(k, v)| format!("{k} {v}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                crate::set(ctx, &mut out, fleet, g("role"), wanted, Some("web"))?;
                format!("{fleet} {}: {line}", g("role"))
            }
            "answer" => {
                let choice = if g("choice").is_empty() { g("text") } else { g("choice") };
                crate::answer(
                    ctx,
                    &Config::load(&ctx.config)?,
                    self.r,
                    &mut out,
                    (fleet, g("id")),
                    choice,
                    ("owner", Some("web")),
                )?;
                format!("answered {}", g("id"))
            }
            "drive" => {
                let cfg = Config::load(&ctx.config)?;
                crate::drive::drive(ctx, &cfg, self.r, &mut out, Some(fleet), true, false)?;
                crate::drive::summary(fleet, &String::from_utf8_lossy(&out))
            }
            // #130: a needs-human card's buttons.
            op @ ("override" | "extend" | "park") => {
                let n = g("issue").parse().context("no issue")?;
                crate::gate::needs_human(
                    ctx,
                    &Config::load(&ctx.config)?,
                    self.r,
                    &mut out,
                    (fleet, n),
                    op,
                    (true, "owner"),
                )?;
                crate::drive::kick(ctx, self.r, fleet, n, "board");
                format!("#{n}: {op}")
            }
            "orphans" => {
                let n = crate::check::orphans(ctx, &Config::load(&ctx.config)?, &mut out, true)?;
                format!("removed {n} orphan folder(s)")
            }
            "reap" => {
                let cfg = Config::load(&ctx.config)?;
                let n = crate::check::leftovers(ctx, &cfg, self.r, &herdr::agents(self.r)?, true)?.len();
                format!("reaped {n} leftover process(es)")
            }
            "host" => {
                let on = g("to") == "on";
                crate::hosts::toggle(ctx, &mut out, g("host"), on, "web")?;
                format!("{} {}", g("host"), if on { "on" } else { "off" })
            }
            "bqueue" => {
                let ns: Vec<u64> = g("issues").split(',').filter_map(|n| n.trim().parse().ok()).collect();
                let mut d = queue::op("add", &ns, false, None, None, None);
                d["check"] = json!(true);
                crate::queue_change(ctx, &Config::load(&ctx.config)?, fleet, d, Some("web"))? + " (needs-check)"
            }
            "unpair" => {
                let _g = devices::lock();
                let mut all = devices::load(ctx);
                all.retain(|d| d.hash != g("hash"));
                devices::store(ctx, &all)?;
                "device removed".into()
            }
            op => bail!("unknown op `{op}`"),
        })
    }

    /// #269: the needs cards, the Lanes tab and the [chat] tab as API v1 sends them (JSON), for a device (`paired`).
    fn parts(&self, ctx: &Ctx, paired: bool) -> [(&'static str, String); 3] {
        let rw = paired && self.addr.is_some();
        [
            ("needs", json!(self.needs_tab(ctx, paired)).to_string()),
            ("lanes", json!(self.lanes_tab(ctx, paired)).to_string()),
            ("chat", json!(self.chat_tab(ctx, rw)).to_string()),
        ]
    }

    /// #60: a part the page loads on demand: a Host chart's series (`id=hist&host=&k=`, #234). #266: the Events tab is
    /// the TypeScript board's (web/src/events.ts); #268: the Queue tab's, its [config] sheets with it.
    fn part(ctx: &Ctx, q: &BTreeMap<String, String>) -> Option<String> {
        // #234: an open chart's series: the small history file only (#160).
        q.get("id").filter(|i| *i == "hist")?;
        let rows = crate::hosts::history(ctx);
        Some(crate::hosts::chart(&rows, q.get("host")?, q.get("k")?, ctx.now))
    }

    fn page(&self, ctx: &Ctx, q: &BTreeMap<String, String>, paired: bool) -> Result<String> {
        let msg = q.get("msg").map_or(String::new(), |m| {
            let bad = m.starts_with("REFUSED") || m.starts_with("error");
            format!("<p class=\"msg {}\">{}</p>", if bad { "r" } else { "g" }, esc(m))
        });
        let ro = if self.addr.is_some() { "" } else { " data-ro=1" };
        let (root, demo) = if self.export.is_some() {
            ("", self.export == Some(true))
        } else {
            ("/", false)
        };
        let ro = format!(
            "{ro}{}{}",
            if root.is_empty() { " data-root=\"\"" } else { "" },
            if demo { " data-demo=1" } else { "" }
        );
        // #199: iOS keeps Safari's and the home-screen app's cookies apart.
        let msg = if !paired && !self.pairb().is_empty() && devices::load(ctx).iter().any(|d| d.code.is_none()) {
            msg + "<p class=m>Safari and the home-screen app pair separately</p>"
        } else {
            msg
        };
        let msg = if demo {
            "<p class=\"msg a\">Demo with fake data. Install herdsman to run your own.</p>".to_string()
        } else {
            msg
        };
        // #241: the phone's theme (localStorage, before the first paint), default `[serve] theme`; #440: System's
        // Appearance picks it. [⚙] opens the System sheet (serve.js), its dot red while a host card is.
        let name = active(ctx);
        let th = serde_json::to_string(&name)?.replace('<', "\\u003c");
        let names = BUILT.iter().copied().chain([name.as_str()]).collect::<BTreeSet<_>>();
        let pick =
            "<button id=sysb type=button aria-haspopup=dialog aria-label=system>⚙<i class=sd hidden></i></button>";
        // #244: every colour scheme (serve.js gates them) and each theme's default; the first paint is the phone's last
        // scheme (serve.js keeps its block), else the board theme's built-in default.
        let def: BTreeMap<_, serde_json::Value> = names
            .iter()
            .map(|n| (*n, theme(ctx, n, "theme.json").0.unwrap_or_default()))
            .map(|(n, j)| {
                (
                    n,
                    serde_json::from_slice(&j).map_or(json!(null), |t: serde_json::Value| t["scheme"].clone()),
                )
            })
            .collect();
        // #423: a `{light, dark}` default paints the phone's own by `prefers-color-scheme`.
        let block = |want: &serde_json::Value| -> Result<String> {
            let b = SCHEMES.iter().find(|s| *want == s.0).unwrap_or(&SCHEMES[0]);
            let b: serde_json::Value = serde_json::from_slice(b.1)?;
            let tokens = b["tokens"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, v)| format!("{k}:{};", v.as_str().unwrap_or_default()));
            Ok(format!(
                ":root{{{}color-scheme:{}}}",
                tokens.collect::<String>(),
                b["color-scheme"].as_str().unwrap_or("dark")
            ))
        };
        let first = match &def[name.as_str()] {
            d @ serde_json::Value::Object(_) => {
                format!(
                    "{}@media(prefers-color-scheme:dark){{{}}}",
                    block(&d["light"])?,
                    block(&d["dark"])?
                )
            }
            d => block(d)?,
        };
        let sd = json!({"t": def, "s": schemes(ctx)}).to_string().replace('<', "\\u003c");
        // #406: the page opened at any other path (the SW's copy) moves to the root before the body, so its
        // relative URLs (a thread's files) resolve there.
        let home = if root.is_empty() {
            ""
        } else {
            ";if(location.pathname!=='/')history.replaceState(null,'','/'+location.search+location.hash)"
        };
        let badge = if demo {
            "<span class=tag id=demo>demo</span>"
        } else {
            ""
        };
        Ok(format!(
            "<!doctype html><html lang=en><head><meta charset=utf-8>\
             <meta name=viewport content=\"width=device-width,initial-scale=1,maximum-scale=1,user-scalable=no,viewport-fit=cover\">\
             <meta name=theme-color content=\"#1f2a38\"><meta name=apple-mobile-web-app-capable content=yes>\
             <link rel=manifest href={root}manifest.webmanifest><link rel=icon href={root}icon-192.png?v={VER:08x}><link rel=apple-touch-icon href={root}icon-180.png?v={VER:08x}>\
             <title>herdsman</title><style id=sch>{first}</style><link rel=stylesheet href={root}a/{VER:08x}/serve.css><link rel=stylesheet href={root}a/{VER:08x}/herd.css>\
             <script>let h=document.documentElement,t=location.hash.slice(1),m=new URLSearchParams(location.search).get('theme');try{{t=t||localStorage.tab;m=m||localStorage.theme}}catch(e){{}}h.dataset.tab=['lanes','chat'].includes(t)?t:'queue';h.dataset.theme=m||{th};\
             let SD={sd},sb;try{{sb=localStorage.sb}}catch(e){{}}if(sb)document.getElementById('sch').textContent=sb{home}</script></head>\
             <body data-ts={} data-mode=scene{ro}><header><b>herdsman</b>{badge}<i id=dot class=a></i>\
             <button type=button class=qhelp aria-label=\"what the marks mean\">?</button><button id=rf type=button aria-label=refresh>↻</button><button id=herd-on type=button aria-label=\"herd mode\">🐎</button>{pick}{}</header><i id=ptr hidden>↻</i><div id=strip><span id=ld class=tld hidden aria-hidden=true><svg class=lds viewBox=\"0 0 132 100\" style=\"--c:#c89a56;--d:#a37a3c;--m:#e8dcc0;--b:#efe7dc\"><use href=#hload /></svg><i></i></span><span class=ldw>loading</span><span id=stale hidden></span></div>{msg}<div id=needs></div><main>\
             <section id=queue></section><section id=lanes></section><section id=chat></section></main>\
             <nav><a href=#queue><span>queue</span></a><a href=#lanes><span>lanes</span></a><a href=#chat><span>chat</span></a></nav><script src={root}a/{VER:08x}/serve.js></script><script src={root}a/{VER:08x}/herd.js></script><script type=module src={root}a/{VER:08x}/board.js></script></body></html>\n",
            ctx.now, if paired { "" } else { self.pairb() }
        ))
    }

    /// #269: API v1's `needs` for a device (`paired`: pane text and the lane and issue forms), the cards above every
    /// tab (web/src/needs.ts draws them): the open asks (options recommended first, a free-text box), each blocked
    /// lane's pending prompt, then the `needs-human` issues.
    fn needs_tab(&self, ctx: &Ctx, paired: bool) -> NeedsTab {
        let events = read(&ctx.state).unwrap_or_default();
        let (cfg, st) = (Config::load(&ctx.config).ok(), events.states.clone());
        let (cfg, ev, st) = (cfg.as_ref(), &events, &st);
        let all = cfg.and_then(|c| self.lanes(ctx.now, c, ev, st)).unwrap_or_default();
        let mins = cfg.map_or(Limits::default().ask_default_minutes, |c| c.limits.ask_default_minutes);
        // #386: a conductor's asks wait in its thread (amber), not in this stack; #412: a design's here too.
        // #517: but one tied to an issue: its card is answerable here too.
        let asks = crate::events::open_asks(ev)
            .into_iter()
            .filter(|a| a.str("thread").is_none() || a.issue.is_some() || crate::design::is_ask(a))
            .map(|a| need_ask(ctx.now, mins, ev, a));
        NeedsTab {
            now: ctx.now,
            rw: self.addr.is_some(),
            live: self.addr.is_some() && self.export.is_none(),
            paired,
            pair: !self.pairb().is_empty(),
            mtime: mtime(ctx),
            snap: self.snapshot().1.ts,
            choosing: chat::choosing(ev),
            asks: asks.collect(),
            blocked: (all.iter().filter(|l| l.state == "blocked"))
                .filter_map(|l| self.blocked(ctx, l, paired))
                .collect(),
            issues: Self::needs_human(ctx, cfg, ev, st),
        }
    }

    /// #130: each `needs-human` issue's card: why, the last verdict, then (paired) merge anyway (PASS at head,
    /// CI green, mergeable), one more round, park.
    fn needs_human(ctx: &Ctx, cfg: Option<&Config>, events: &[Event], st: &States) -> Vec<NeedIssue> {
        let card = |s: &IssueState, pending: bool| {
            let (f, n) = (&s.fleet, s.issue);
            let (pr, fc) = (gh::cached_pr(ctx, f, n), cfg.and_then(|c| c.fleet.get(f)));
            let why = events
                .iter()
                .rev()
                .find(|e| ["needs-human", "extend-failed"].iter().any(|k| e.is(k, f, n)));
            let m = pr["mergeable"].as_str().unwrap_or("?");
            let pass = s.verdicts.last().filter(|v| v.1 == "PASS").map(|v| v.0.clone());
            NeedIssue {
                fleet: f.clone(),
                issue: n,
                pending,
                who: format!("{f} · #{n} · {}", age(ctx.now.saturating_sub(s.last_ts))),
                title: gh::cached_title(ctx, f, n).unwrap_or_default(),
                why: why.and_then(|e| e.str("reason")).unwrap_or("gate").into(),
                stale: pass
                    .as_ref()
                    .is_some_and(|sha| pr["headRefOid"].as_str() != Some(sha.as_str())),
                pass,
                fail: crate::events::finding(events, f, n).unwrap_or_default().to_string(),
                pr: pr_data(ctx, fc, f, n, None),
                repo: fc.map(|c| c.repo.clone()),
                mergeable: m.to_lowercase(),
                merge: fc.is_some_and(|c| gate(c, &pr, Some(s), true).is_ok()) && m == "MERGEABLE",
                // #417: the last verdict's screenshots (a verify FAIL's), as `/shot/<fleet>/<n>/<i>` serves them.
                shots: crate::verify::served(events, (f, n)).into_iter().map(|x| x.0).collect(),
            }
        };
        // #217: a failed extend's card is back; a recorded one with no lane yet is one muted line, no buttons.
        let pend = |s: &IssueState| match s.last_event.as_str() {
            "round-extended" if crate::waiting::unspawned(events, s) => Some(card(s, true)),
            _ if s.state == "needs-human" || s.last_event == "extend-failed" => Some(card(s, false)),
            _ => None,
        };
        st.values().filter_map(pend).collect()
    }

    /// A blocked lane's card: a harness menu's options under its title, reason, command and countdown (#127: `push`
    /// false while that runs; none once the pane works again), else the pane's last 6 masked lines. Unpaired: no pane
    /// text, only the 1-line activity.
    fn blocked(&self, ctx: &Ctx, l: &lanes::Lane, paired: bool) -> Option<Blocked> {
        let screen = l
            .pane
            .as_ref()
            .and_then(|p| herdr::read(&l.on(&self.cached(ctx.now, 2)), p).ok())
            .unwrap_or_default();
        let rw = self.addr.is_some() && paired;
        let h = crate::harness::get(&l.harness);
        let c = lanes::prompt(&screen).unwrap_or_default();
        let menu = h.pending_menu(&screen);
        if menu.is_none() && h.status_word(&screen).is_some() {
            return None;
        }
        let s = |k: &str| c[k].as_str().unwrap_or_default().to_string();
        let menu = menu.filter(|_| paired).map(|(q, opts)| Menu {
            title: s("title"),
            reason: s("reason"),
            command: s("command"),
            countdown: c["countdown"].as_u64(),
            question: lanes::mask(&q),
            options: opts.iter().filter(|_| rw).map(|o| lanes::mask(o)).collect(),
            h: seen(&q, &opts).to_string(),
        });
        let issue = l.issue.map_or(String::new(), |n| format!("#{n}"));
        Some(Blocked {
            name: l.name.clone(),
            who: join(&[l.fleet.as_deref().unwrap_or_default(), &issue, &l.name]),
            push: c["push"].as_bool().unwrap_or(true),
            activity: (!paired).then(|| lanes::activity(&l.harness, &screen)),
            tail: (paired && menu.is_none()).then(|| lanes::tail(&screen, 6).join("\n")),
            menu,
        })
    }

    /// `herdr agent|workspace list` JSON, read at most every 10 s (None when herdr cannot be read).
    fn herdr_json(&self, now: u64, what: &str) -> Option<serde_json::Value> {
        herdr::list(&self.cached(now, 10), what).ok()
    }

    /// #268: API v1's `queue`, the Queue tab's data (web/src/queue.ts draws it): per configured fleet a card (#162),
    /// open work (needs you, ready, working, queued), `done today`; paused = one line.
    fn queue_tab(&self, ctx: &Ctx) -> QueueTab {
        let (now, rw) = (ctx.now, self.addr.is_some());
        let mut d = QueueTab {
            now,
            rw,
            live: rw && self.export.is_none(),
            mtime: mtime(ctx),
            ..QueueTab::default()
        };
        // #483: the error's own text, config or log (e.g. `migrate --live`), not a fixed line.
        let (cfg, events) = match (Config::load(&ctx.config), read(&ctx.state)) {
            (Ok(c), Ok(e)) => (c, e),
            (Err(e), _) | (_, Err(e)) => {
                d.error = Some(format!("{e:#}"));
                return d;
            }
        };
        let (cfg, events, st) = (&cfg, &events, &events.states);
        // The state of an open lane herdr shows no pane for: `dead`, or `parked` (#83).
        let ls = self.lanes(now, cfg, events, st).unwrap_or_default();
        let gone = |s: &IssueState| {
            s.active_lane()
                .and_then(|x| ls.iter().find(|l| l.name == x.1 && l.pane.is_none()))
        };
        // #205: a lane herdr still lists opens its Lanes sheet; #218: a gone one its stored transcript, else none.
        let link = |a: &str| match ls.iter().any(|l| l.name == a) {
            true => "live",
            false if lanes::log(&ctx.state, a).is_some_and(|f| f.is_file()) => "gone",
            false => "",
        };
        let asks = crate::events::open_asks(events);
        for (f, fc) in &cfg.fleet {
            let q = queue::view(events, st, f);
            let mine: Vec<&IssueState> = st.values().filter(|s| s.fleet == *f).collect();
            let queued = |n: u64| q.iter().any(|i| i.issue == n);
            let rest: Vec<u64> = mine
                .iter()
                .filter(|s| !queued(s.issue) && !s.done() && s.state != "queued")
                .map(|s| s.issue)
                .collect();
            let items: Vec<(u64, Option<u64>, Option<usize>)> = q
                .iter()
                .enumerate()
                .map(|(k, i)| (i.issue, i.blocked_by, Some(k)))
                .chain(rest.iter().map(|&n| (n, None, None)))
                .collect();
            let mut rows = vec![];
            for &(n, blocked_by, k) in &items {
                let s = st.get(&(f.clone(), n));
                let waiting = asks.iter().any(|e| e.fleet == *f && e.issue == Some(n));
                let logged = s.map_or("queued", |s| s.state.as_str());
                let shown = if waiting { "⏸ waiting: decision" } else { logged };
                let state = s.and_then(gone).map_or(shown, |l| l.state.as_str());
                let dead = state == "dead";
                // #162: needs you, then ready, then working, then queued; serve.js keeps follow-ups under their parent.
                let rank = match state.split('(').next().unwrap_or_default() {
                    "needs-human" | "⏸ waiting: decision" => 0,
                    "ready" => 1,
                    "impl" | "fix" | "review" => 2,
                    _ => 3,
                };
                let lane = s.and_then(|s| s.lane.as_deref()).unwrap_or_default();
                let pr = gh::cached_pr(ctx, f, n);
                let head = pr["headRefOid"].as_str().unwrap_or_default();
                let mut seen = BTreeSet::new();
                let spawns = events.iter().rev().filter(|e| e.is("spawn", f, n));
                let qls = spawns
                    .filter_map(|e| e.str("agent").filter(|a| seen.insert(*a)).map(|a| (a, e.ts)))
                    .map(|(a, ts)| {
                        let st = ls.iter().find(|l| l.name == a).map_or("gone", |l| l.state.as_str());
                        QueueLane {
                            name: a.into(),
                            state: st.into(),
                            tone: if st == "gone" { "m" } else { tone(st) }.into(),
                            ago: ago(now, ts),
                            link: link(a).into(),
                        }
                    })
                    .collect();
                let mut notes: Vec<String> = crate::land::note(events, f, n).into_iter().collect();
                // #417: a verify stage passed on at this head for want of the repo's recipe.
                let bare = |e: &&Event| e.is("verify-no-recipe", f, n) && e.str("sha") == Some(head);
                notes.extend(
                    events
                        .iter()
                        .rfind(bare)
                        .map(|_| format!("verify skipped: no {}", crate::verify::RECIPE)),
                );
                // #325: an answer that never reached the lane stays red until drive types it in.
                for u in crate::undelivered(events, f).into_iter().filter(|u| u.issue == Some(n)) {
                    let (id, why) = (u.str("id").unwrap_or("?"), u.str("reason").unwrap_or("?"));
                    notes.push(format!("answer {id} not delivered: {why}"));
                }
                // #99: a decision taken by default, with [change]: a new decision on the same ask.
                let last = events.iter().rev().find(|e| e.is("decision", f, n));
                let decided = last.filter(|d| d.str("by") == Some("default"));
                let asked = |d: &Event| {
                    events
                        .iter()
                        .find(|e| e.is("needs-decision", f, n) && e.str("id") == d.str("id"))
                };
                let ask = decided.filter(|_| rw).and_then(asked);
                let open = (asks.iter().rfind(|e| e.fleet == *f && e.issue == Some(n)))
                    .map(|a| need_ask(now, cfg.limits.ask_default_minutes, events, a));
                let up = s.is_some_and(|s| s.updating);
                let mv = rw && !dead && k.is_some();
                rows.push((
                    rank,
                    QueueRow {
                        issue: n,
                        state: state.into(),
                        tone: tone(state).into(),
                        age: ago(now, s.map_or(0, |s| s.last_ts)),
                        // #367: the time it counts from too (fixed, so /live sends no change a second).
                        t: s.map_or(now, |s| s.last_ts),
                        act: act(["", "update"][usize::from(up)], state).into(),
                        grow: stage(state, act(["", "update"][usize::from(up)], state)).into(),
                        coat: if lane.is_empty() {
                            String::new()
                        } else {
                            coat(lane).replace("--", "--r")
                        },
                        queued: k.is_some(),
                        check: k.is_some_and(|k| q[k].check),
                        groups: queue::groups(&q, events, fc, f, n)
                            .iter()
                            .map(|g| g.name.clone())
                            .collect(),
                        blocked_by,
                        title: titles(ctx, f, n),
                        stage: s.map_or(vec![], |s| stages(fc, s, head)),
                        lane: lane.into(),
                        live: ls.iter().any(|l| l.name == lane),
                        pr: pr_data(ctx, Some(fc), f, n, s),
                        dead,
                        notes,
                        decided: decided.map(|d| d.str("choice").unwrap_or("?").into()),
                        ask: ask.map(v1::ask),
                        open,
                        mv,
                        up: k.filter(|&k| mv && k > 0).map(|k| q[k - 1].issue),
                        down: k.filter(|_| mv).and_then(|k| q.get(k + 1)).map(|x| x.issue),
                        lanes: qls,
                    },
                ));
            }
            rows.sort_by_key(|r| r.0);
            let need = rows.iter().filter(|r| r.0 == 0).count();
            let mut done: Vec<&&IssueState> = mine
                .iter()
                .filter(|s| s.done() && now.saturating_sub(s.last_ts) < 86_400)
                .collect();
            done.sort_by_key(|s| std::cmp::Reverse(s.last_ts));
            let done_row = |s: &&&IssueState| DoneRow {
                issue: s.issue,
                note: crate::land::note(events, f, s.issue).unwrap_or_else(|| s.state.clone()),
                age: age(now.saturating_sub(s.last_ts)),
                pr: events
                    .iter()
                    .rev()
                    .find(|e| e.is("merged", f, s.issue))
                    .and_then(|e| e.str("pr"))
                    .map(Into::into),
            };
            // #365: open issues not queued, with no lane, from the last read; serve.js draws, filters, ticks (`bqueue`) them.
            let bl: serde_json::Value = crate::usage::load(&gh::cache_path(ctx, "backlog", f, 0)).unwrap_or_default();
            let mut open: BTreeMap<String, BacklogIssue> =
                serde_json::from_value(bl["issues"].clone()).unwrap_or_default();
            open.retain(|n, _| n.parse().is_ok_and(|n| !queued(n) && !st.contains_key(&(f.clone(), n))));
            let backlog = bl["issues"].is_object().then(|| Backlog {
                issues: open,
                ago: age(now.saturating_sub(bl["ts"].as_u64().unwrap_or(now))),
            });
            let lanes: Vec<&&IssueState> = mine.iter().filter(|s| s.active_lane().is_some()).collect();
            let plural = |n: usize| if n == 1 { "" } else { "s" };
            let mut card = QueueCard {
                name: f.clone(),
                k: fc.color(f).into(),
                serial: fc.serial.iter().map(|g| g.name.clone()).collect(),
                // #294: a group's paths (serve.js splits the groups by ` | `).
                paths: fc.serial.iter().map(|g| g.paths.join(", ")).collect(),
                need,
                rows: rows.into_iter().map(|r| r.1).collect(),
                done: done.iter().take(5).map(done_row).collect(),
                done_n: done.len(),
                backlog,
                config: config_sheet(ctx, cfg, f, fc, st),
                ..QueueCard::default()
            };
            if fc.paused {
                let nd = lanes
                    .iter()
                    .filter(|s| gone(s).is_some_and(|l| l.state == "dead"))
                    .count();
                let what = match (lanes.len(), nd) {
                    (0, _) => "no lane".to_string(),
                    (n, d) if d == n => format!("{n} lane{} (dead)", plural(n)),
                    (n, 0) => format!("{n} lane{}", plural(n)),
                    (n, d) => format!("{n} lanes ({d} dead)"),
                };
                let since = events
                    .iter()
                    .rev()
                    .find(|e| e.kind == "pause" && e.fleet == *f && e.issue.is_none());
                let since = since.map_or(String::new(), |e| format!(" · {}", age(now.saturating_sub(e.ts))));
                card.paused = Some(format!("paused{since} · {what}"));
                d.fleets.push(card);
                continue;
            }
            let stall = cfg.limits.stall_minutes * 60;
            let live = lanes.iter().filter(|s| gone(s).is_none());
            let next = live.map(|s| ((s.last_ts + stall).saturating_sub(now), s.issue)).min();
            let mut meta = vec![match next {
                Some((0, n)) => format!("stall check: #{n} is due now"),
                Some((left, n)) => format!("stall check: #{n} in {}", age(left)),
                None => "no live lane".into(),
            }];
            if !lanes.is_empty() {
                meta.push(format!("{} lane{}", lanes.len(), plural(lanes.len())));
            }
            meta.extend(queue::serial_use(events, st, f, Some(cfg)));
            card.drive = drive(ctx, events, f, meta);
            // #173: a refused pipeline stops this fleet only; its card says so in red.
            if !fc.bad.is_empty() {
                let n = fc.bad.len();
                card.notes.push(format!(
                    "! BAD_PIPELINE · {n} error{} in fleet.toml · nothing moves until it is fixed",
                    plural(n)
                ));
            }
            // #350: fast-first landings wait while integration is red.
            if let Some(p) = crate::land::paused(events, f) {
                let at = p.str("at").unwrap_or("?");
                card.notes
                    .push(format!("landing paused: {} red since {at}", fc.integration));
            }
            // #429: `check`'s measurement of integration within `warn_within` of its line cap.
            let cap: serde_json::Value = crate::usage::load(&gh::cache_path(ctx, "cap", f, 0)).unwrap_or_default();
            let left = cap["cap"].as_i64().zip(cap["lines"].as_i64()).map(|(c, n)| c - n);
            if let Some(l) = left.filter(|l| *l < cap["warn"].as_i64().unwrap_or(500)) {
                card.notes
                    .push(format!("cap-near: {l} lines of headroom on {}", fc.integration));
            }
            d.fleets.push(card);
        }
        // #162: fleets that need you first, then by name.
        d.fleets.sort_by_key(|c| c.need == 0);
        d
    }

    /// herdr's agents joined with the log (None when herdr cannot be read).
    fn lanes(&self, now: u64, cfg: &Config, events: &[Event], st: &States) -> Option<Vec<lanes::Lane>> {
        let list = self.herdr_json(now, "agent")?;
        Some(crate::snapshot::lanes(
            cfg,
            &self.cached(now, 30),
            self.ctx,
            (events, st),
            &list,
        ))
    }

    fn waits(&self, ctx: &Ctx, cfg: &Config, events: &[Event], all: &[lanes::Lane]) -> Vec<crate::waiting::Waiting> {
        let snap = self.snapshot().1;
        crate::snapshot::saved_waits(ctx, cfg, &self.cached(ctx.now, 30), &snap, (events, all))
    }

    /// #37: Herd mode's data (`/herd.json`, the `herd` event; docs/serve.md).
    /// `weather` (loopback only) forces the host values.
    fn herd(&self, ctx: &Ctx, weather: Option<&str>) -> Result<Herd> {
        let events = read(&ctx.state)?;
        let (cfg, st) = (Config::load(&ctx.config)?, events.states.clone());
        let all = self.lanes(ctx.now, &cfg, &events, &st).unwrap_or_default();
        let waits = self.waits(ctx, &cfg, &events, &all);
        let lanes = |f: Option<&String>| -> Vec<_> {
            all.iter()
                .filter(|l| l.fleet.as_ref() == f && l.state != "asleep") // #422: no asleep conductor in the herd
                .map(|l| {
                    let (title, tone, shown, a) = view(ctx, l, &waits);
                    // #239: a fix round's lane has role impl; the issue's state says it is fixing.
                    let is = l.issue.and_then(|i| st.get(&(f?.clone(), i)));
                    let is = is.map_or("", |s| s.state.as_str());
                    let act = match (act(&l.role, &l.state), act(&l.role, is)) {
                        ("impl", b @ ("fix" | "update")) => b,
                        (a, _) => a,
                    };
                    HerdLane {
                        name: l.name.clone(),
                        act: act.into(),
                        grow: stage(is, act).into(),
                        issue: l.issue,
                        title,
                        role: l.role.clone(),
                        harness: l.harness.clone(),
                        model: l.model.clone(),
                        effort: l.effort.clone(),
                        account: l.account.clone(),
                        host: l.host.clone(),
                        state: l.state.clone(),
                        shown,
                        tone: tone.into(),
                        age: a,
                        coat: coat(&l.name),
                        gone: matches!(is, "merged" | "closed"),
                    }
                })
                .collect()
        };
        let queued = |f: &String| -> Vec<_> {
            let open = |s: &IssueState| s.state == "queued" || s.state == "needs-human";
            let waiting = queue::view(&events, &st, f)
                .into_iter()
                .filter(|i| st.get(&(f.clone(), i.issue)).is_none_or(open));
            waiting
                .map(|i| HerdQueued {
                    issue: i.issue,
                    title: gh::cached_title(ctx, f, i.issue),
                    blocked_by: i.blocked_by,
                    coat: coat(&format!("{f}#{}", i.issue)),
                })
                .collect()
        };
        let fleets: Vec<_> = cfg
            .fleet
            .iter()
            .map(|(f, fc)| HerdFleet {
                name: f.clone(),
                color: fc.color(f).into(),
                lanes: lanes(Some(f)),
                queue: queued(f),
                needs: st
                    .values()
                    .filter(|s| &s.fleet == f && s.state == "needs-human")
                    .map(|s| s.issue)
                    .collect(),
            })
            .collect();
        let lim = &cfg.limits;
        let sky = [
            ("calm", 0.1, "normal"),
            ("gusty", 0.7, "normal"),
            ("storm", 1.3, "normal"),
            ("fog", 0.3, "warn"),
        ];
        let forced = sky.iter().find(|w| Some(w.0) == weather).map(|w| (w.1, w.2));
        let h = forced.map_or_else(
            || self.sample(),
            |(k, p)| Host {
                load_per_cpu: Some(k * lim.load_per_cpu),
                pressure: Some(p.into()),
                swapout: Some(0.0),
                ..Host::default()
            },
        );
        Ok(Herd {
            fleets,
            others: lanes(None),
            host: HerdHost {
                load: h.load_per_cpu,
                limit: lim.load_per_cpu,
                gate: host::admit(&h, lim).err(),
                pressure: h.pressure,
                swapouts: h.swapout,
                swap_limit: lim.swapout_rate,
            },
        })
    }

    /// #269: API v1's `lanes` for a device (`paired`), the Lanes tab (web/src/lanes.ts draws it): state sections, fleet
    /// subgroups inside; a row opens the bottom sheet (serve.js) with its hidden card.
    fn lanes_tab(&self, ctx: &Ctx, paired: bool) -> LanesTab {
        let mut t = LanesTab {
            now: ctx.now,
            rw: self.addr.is_some(),
            live: self.addr.is_some() && self.export.is_none(),
            paired,
            pair: !self.pairb().is_empty(),
            ..LanesTab::default()
        };
        let events = read(&ctx.state).unwrap_or_default();
        let (Ok(cfg), st) = (Config::load(&ctx.config), events.states.clone()) else {
            t.error = Some("no fleet.toml".into());
            return t;
        };
        let Some(all) = self.lanes(ctx.now, &cfg, &events, &st) else {
            t.failed = true;
            return t;
        };
        let state = |l: &lanes::Lane| l.issue.zip(l.fleet.as_ref()).and_then(|(n, f)| st.get(&(f.clone(), n)));
        let waits = self.waits(ctx, &cfg, &events, &all);
        let groups = lanes::group(&all, |l| state(l).map_or(l.since, |s| s.last_ts.max(l.since)));
        let row = |l: &lanes::Lane| self.lane_row(ctx, &cfg, l, state(l), paired, &waits);
        let repo = |(name, ls): (String, Vec<&lanes::Lane>)| LaneRepo {
            name,
            lanes: ls.into_iter().map(row).collect(),
        };
        t.sections = groups
            .into_iter()
            .map(|(sec, subs)| LaneSection {
                name: sec.into(),
                tone: tone(sec).into(),
                repos: subs.into_iter().map(repo).collect(),
            })
            .collect();
        t
    }

    fn lane_row(
        &self,
        ctx: &Ctx,
        cfg: &Config,
        l: &lanes::Lane,
        s: Option<&IssueState>,
        paired: bool,
        waits: &[crate::waiting::Waiting],
    ) -> LaneRow {
        let screen = l
            .pane
            .as_ref()
            .and_then(|p| herdr::read(&l.on(&self.cached(ctx.now, 30)), p).ok());
        let (title, tone, shown, a) = view(ctx, l, waits);
        let issue = l.issue.map(|n| format!("#{n}")).unwrap_or_default();
        let round = s.map(|s| {
            format!(
                "round {}",
                s.state
                    .split_once('(')
                    .map_or(s.rounds.to_string(), |r| r.1.replace(')', ""))
            )
        });
        let o = |v: &Option<String>| v.clone().unwrap_or_default();
        let facts = [
            l.role.clone(),
            o(&l.host),
            o(&round),
            l.harness.clone(),
            o(&l.model),
            o(&l.effort),
            o(&l.account),
            o(&l.resting),
            o(&l.port),
            a,
        ];
        let screen = screen.unwrap_or_default();
        // #276: a `now:` line is one line from the server (full text on long-press): Claude's spinner word, the action (a
        // command summarised; cut first), the elapsed time.
        let (now, spin) = (lanes::now_of(l, &screen), lanes::spin(&l.harness, &screen));
        let busy = (now.is_some() || spin.is_some()).then(|| {
            let (w, t) = spin.map_or((None, None), |(w, t)| (Some(w), t));
            let n = now.as_ref().map(|n| match n.strip_prefix("now: running ") {
                Some(c) => format!("{}running {}", if w.is_some() { "" } else { "now: " }, lanes::short(c)),
                None => n.strip_prefix("now: ").filter(|_| w.is_some()).unwrap_or(n).to_string(),
            });
            let title: Vec<_> = [&w, &now, &t].into_iter().flatten().cloned().collect();
            LaneNow {
                title: title.join(" · "),
                w,
                n,
                t,
            }
        });
        // #53: line 1 name (one line) + `state age`; line 2 the reason (no age), or `superseded by` on an issue's older lane.
        let w = wait_of(l, waits);
        let sup = s.and_then(|s| s.lane.as_ref()).filter(|x| **x != l.name && w.is_some());
        let (why, why_tone) = match (sup, &w) {
            (Some(x), _) => (Some(format!("superseded by {x}")), "m"),
            (None, Some(w)) => (Some(w.what.clone()), tone),
            _ => (None, ""),
        };
        let since = w.filter(|_| sup.is_none()).map_or(l.since, |w| w.since);
        let fc = l.fleet.as_ref().and_then(|f| cfg.fleet.get(f));
        LaneRow {
            name: l.name.clone(),
            role: l.role.clone(),
            state: l.state.clone(),
            act: act(&l.role, &l.state).into(),
            grow: stage(s.map_or("", |s| s.state.as_str()), act(&l.role, &l.state)).into(),
            atone: self::tone(&l.state).into(),
            coat: coat(&l.name),
            tone: if sup.is_some() { "m" } else { tone }.into(),
            ago: ago(ctx.now, since),
            why,
            why_tone: why_tone.into(),
            activity: if busy.is_none() {
                lanes::activity(&l.harness, &screen)
            } else {
                String::new()
            },
            now: busy,
            gone: l.pane.is_none(),
            thread: l.thread.clone(), // #422: a tap opens it
            head: format!("{issue} {}", o(&title)).trim().into(),
            shown,
            facts: join(&facts.iter().map(String::as_str).collect::<Vec<_>>()),
            issue: l.issue.filter(|_| l.fleet.is_some()),
            pr: l.issue.zip(l.fleet.as_ref()).map(|(n, f)| pr_data(ctx, fc, f, n, s)),
            repo: fc.map(|c| c.repo.clone()),
            dead: l.state == "dead",
            blocked: (l.state == "blocked").then(|| self.blocked(ctx, l, paired)).flatten(),
        }
    }

    fn sample(&self) -> Host {
        lock(&self.host).clone()
    }

    /// #141: the Host tab as data (API v1's `host`; web/src/host.ts draws it): the host cards, then FLEET DISK, SERVICES,
    /// FLEETS and ALERTS rows with a status; serve.js ranks them.
    fn host_tab(&self, ctx: &Ctx) -> HostTab {
        let (cfg, events) = (Config::load(&ctx.config).ok(), read(&ctx.state).unwrap_or_default());
        let (cfg, events) = (cfg.as_ref(), &events);
        let (hs, r, dl, dc) = (
            self.sample(),
            &self.cached(ctx.now, 60),
            Limits::default(),
            Config::default(),
        );
        let lim = cfg.map_or(&dl, |c| &c.limits);
        let live = self.addr.is_some() && self.export.is_none();
        let mut d = HostTab {
            now: ctx.now,
            rw: self.addr.is_some(),
            live,
            mtime: mtime(ctx),
            hosts: self.cards(ctx, cfg.unwrap_or(&dc), r, events, &hs),
            ..HostTab::default()
        };
        // #164: the worktree total from the last walk (and its age, #160) against the budget, and folders of no
        // fleet under the worktree root, removed only from here (paired) or `clean --orphans --yes`.
        if let Some(c) = cfg {
            let budget = lim.worktree_budget_gb;
            let (gb, at) = crate::check::worktree_du(ctx).unzip();
            d.disk = Some(Disk {
                bar: gauge(gb.unwrap_or(0.0) / budget),
                gb: gb.map_or("?".into(), |gb| format!("{gb:.1}")),
                budget,
                ago: at.flatten().map(|at| ago(ctx.now, at)),
            });
            d.orphans = crate::check::orphan_dirs(ctx, c).len();
        }
        // #186: processes still running in closed lanes' worktrees; `[reap]` TERMs, then KILLs them.
        let left = cfg.zip(herdr::agents(r).ok()).map_or(vec![], |(c, a)| {
            crate::check::leftovers(ctx, c, r, &a, false).unwrap_or_default()
        });
        if !left.is_empty() {
            let pids = left.iter().map(|p| p.0.as_str()).collect::<Vec<_>>().join(",");
            let kb: f64 = crate::runner::ok(r, "ps", &["-o", "rss=", "-p", &pids]).map_or(0.0, |s| {
                s.split_whitespace().filter_map(|k| k.parse::<f64>().ok()).sum()
            });
            let mb = format!("{:.0}", kb / 1024.0);
            d.leftovers = Some(Leftovers { n: left.len(), mb });
        }
        // #333: spawns starting against `max_parallel_spawns`, and the stale markers swept in the last day.
        if let Some(c) = cfg {
            let sl = crate::drive::spawns_line(ctx, c, r, events);
            let (k, v) = sl.split_once(' ').unwrap_or_default();
            d.spawns = Some(Row {
                name: k.into(),
                tone: ["g", "a"][usize::from(sl.contains(" · stale"))].into(),
                detail: v.into(),
                ..Row::default()
            });
        }
        // #98: green, amber or red per service, what clears it otherwise.
        let svc = |s: &crate::timer::Row| ["board", "tick", "fast step", herdr::BIN].contains(&s.0.as_str());
        let rows = cfg.map_or(vec![], |c| crate::timer::services(ctx, c, r, Some(board(ctx))));
        let row = |(name, t, detail, fix): crate::timer::Row| Row {
            name,
            tone: t.into(),
            detail,
            fix,
        };
        d.services = rows.into_iter().filter(svc).map(row).collect();
        // #172: the board's theme; amber when its theme.json is invalid (farm shows), with the first problem (#241).
        let name = active(ctx);
        let err = theme(ctx, &name, "theme.json").1.err();
        d.theme = Row {
            tone: ["g", "a"][usize::from(err.is_some())].into(),
            detail: err.unwrap_or_default(),
            name,
            ..Row::default()
        };
        for (c, name) in cfg
            .into_iter()
            .flat_map(|c| c.accounts.names.keys().map(move |n| (c, n)))
        {
            let cap = crate::access::cap_line(c, self.r, events, name, ctx.now);
            let login = c.accounts.usage.get(name).map_or("", String::as_str);
            let usage = crate::usage::reading(ctx, name, || crate::usage::meter(ctx, c, self.r, name))
                .map(|m| crate::usage::shown(self.r, &m, login, ctx.now));
            let (meters, pace, hint) = match usage {
                Some(Ok((parts, pace))) => (
                    parts.into_iter().map(|(label, pct)| Meter { label, pct }).collect(),
                    pace,
                    None,
                ),
                Some(Err(hint)) => (vec![], None, Some(hint)),
                None => (vec![], None, None),
            };
            d.accounts.push(Account {
                name: name.clone(),
                capped: crate::access::capped_until(c, events, name, ctx.now).is_some(),
                tone: tone(cap.split(' ').next().unwrap_or_default()).into(),
                hours: crate::usage::lane_hours(events, name, ctx.now),
                cap,
                meters,
                pace,
                hint,
            });
        }
        d.deepseek = cfg
            .and_then(|c| crate::usage::deepseek(ctx, c, self.r))
            .map(|b| match b {
                Ok((b, low)) => Balance {
                    balance: Some(format!("{} {:.2}", b.currency, b.total)),
                    low,
                    hint: String::new(),
                },
                Err(hint) => Balance {
                    hint,
                    ..Balance::default()
                },
            });
        // Each fleet's last `driven` result in plain words, the raw line on tap; an owner's pause is grey (no problem).
        for (name, fc) in cfg.map(|c| &c.fleet).into_iter().flatten() {
            let last = events.iter().rfind(|e| e.kind == "driven" && e.fleet == *name);
            let (t, w, x) = match last {
                _ if fc.paused => ("m", "paused", format!("`herdsman resume {name}` starts it again")),
                Some(e) => in_words(e, fc.serial_label()),
                None => ("m", "nothing yet", String::new()),
            };
            let e = last.filter(|_| !fc.paused);
            d.fleets.push(FleetRow {
                name: name.clone(),
                tone: t.into(),
                word: w.into(),
                text: x,
                ago: e.map_or(String::new(), |e| age(ctx.now.saturating_sub(e.ts))),
                raw: e.and_then(|e| e.str("result")).map(String::from),
            });
        }
        // #87: on/off from fleet.toml; the last `push-sent`, unless a webpush `notify-failed` is newer.
        let (t, detail) = crate::timer::phone(ctx, cfg, events);
        d.push = Row {
            name: "push".into(),
            tone: t.into(),
            detail,
            ..Row::default()
        };
        // #440: the global notifiers in words, for System's Notifications.
        if let Some(c) = cfg {
            let notifier = |n: &crate::config::Notify| crate::api::Notifier {
                kind: if n.kind.as_deref() == Some("webpush") {
                    "web push"
                } else {
                    "command"
                }
                .into(),
                on: if n.on.is_empty() {
                    "default events".into()
                } else {
                    n.on.join(", ")
                },
            };
            d.notify = c.notify.iter().map(notifier).collect();
        }
        if live {
            // #18: the button shows only in the Home Screen app (serve.js); endpoints and keys stay on the Mac.
            let key = crate::webpush::vapid(&ctx.state)
                .map(|k| crate::webpush::public(&k))
                .unwrap_or_default();
            d.key = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, key);
            let sub = |s: crate::webpush::Sub| Sub {
                id: s.id(),
                label: s.label,
                created: s.created,
            };
            d.subs = crate::webpush::load(&ctx.state).into_iter().map(sub).collect();
            let paired = |d: devices::Device| Paired {
                hash: d.hash,
                label: d.label,
                created: d.created,
                used: d.used,
            };
            d.devices = devices::load(ctx)
                .into_iter()
                .filter(|d| d.code.is_none())
                .map(paired)
                .collect();
        }
        d
    }

    /// #234: one card per host (main first, then A→Z), each with the same rows: the state, why its dot is amber or red
    /// and [off]/[on] in its head, then load/cpu, swap-out, memory, disk, top memory, lanes and tags. A row with history
    /// carries its 24 h sparkline (worst per bucket); a tap loads its chart (`frag?id=hist`, an export inline).
    fn cards(&self, ctx: &Ctx, cfg: &Config, r: &dyn Runner, events: &[Event], hs: &Host) -> Vec<HostCard> {
        let (lim, seen, hist, now) = (
            &cfg.limits,
            crate::hosts::cached(ctx),
            crate::hosts::history(ctx),
            ctx.now,
        );
        let mut out = vec![];
        for (name, h) in crate::hosts::cards(cfg, r) {
            let (main, on) = (h.is_none_or(|h| h.machine.is_none()), h.is_none_or(|h| h.enabled));
            let mut mem = vec![];
            let mut s = seen.get(&name).cloned().unwrap_or_default();
            let word = |p: Option<u8>| p.and_then(|p| ["normal", "warn", "critical"].get(usize::from(p)).copied());
            let mut gate = Host {
                pressure: word(s.pressure).map(String::from),
                swapout: s.swap,
                load_per_cpu: s.load,
                free_mem_gb: s.mem,
                ..Host::default()
            };
            if main {
                gate = hs.clone();
                (s.up, s.seen, s.load, s.mem, s.swap) = (true, now, hs.load_per_cpu, hs.free_mem_gb, hs.swapout);
                ((s.disk, s.disk_total), s.mem_total) = (host::df(r).unzip(), hs.mem_total_gb);
                let all = herdr::agents(r).unwrap_or_default();
                s.top = host::top(r, &all);
                // #248, #316: what each harness's lanes hold (their pane trees), working and done apart, so done-lane
                // memory shows; an agent is its harness's by `Harness::process_names`, else by its own label.
                let mut by: BTreeMap<String, Vec<&serde_json::Value>> = BTreeMap::new();
                for (a, label) in all
                    .iter()
                    .filter_map(|a| Some((a, a["agent"].as_str().filter(|l| !l.is_empty())?)))
                {
                    let h = crate::harness::all().find(|h| h.1.process_names().contains(&label));
                    by.entry(h.map_or(label, |h| h.0).into()).or_default().push(a);
                }
                for (h, ours) in by {
                    let n = |st: &str| ours.iter().filter(|a| a["agent_status"] == st).count();
                    let mb: u64 = host::held(r, &ours).iter().sum();
                    let gb = format!("{}.{} GB", mb / 1024, mb % 1024 * 10 / 1024);
                    let (w, d) = (n("working"), n("done"));
                    mem.push((h, gb, format!("({} lanes: working {w} / done {d})", ours.len())));
                }
                s.pressure = hs.pressure.as_deref().map(crate::hosts::level);
            }
            let lanes = crate::hosts::lanes_on(cfg, events, &name);
            let (mut t, mut words) = crate::hosts::shown(on, &s, lanes.len(), now);
            let cloud = crate::cloud::card(ctx, cfg, events, (&name, lanes.len()));
            let title = cloud.as_ref().map(|c| c[0].clone()).filter(|w| !w.is_empty());
            (t, words) = title.map_or((t, words), |w| ("m", w));
            let held = t == "g" && host::admit(&gate, lim).is_err();
            if t == "g" {
                (t, words) = match host::admit(&gate, lim) {
                    Ok(()) => ("g", "up · admits lanes".into()),
                    Err(why) => ("r", format!("held · {why}")),
                };
            }
            let grey = |x: &'static str| if on && s.up { x } else { "m" };
            let inline = self.export.is_some();
            let row = |k: &str, key: &str, t: &str, v: Cell, l: Option<f64>| {
                let sr = |k: &str| crate::hosts::series(&hist, &name, k, now, false);
                let x = [String::new(), crate::hosts::chart(&hist, &name, key, now)];
                Cell {
                    k: k.into(),
                    t: t.into(),
                    key: key.into(),
                    s: sr(key),
                    p: [String::new(), sr("pressure")][usize::from(key == "mem")].clone(),
                    l,
                    x: x[usize::from(inline)].clone(),
                    ..v
                }
            };
            // A gauge row's value, one line (an offline host's age is in its head): (status, cell). `used`: (used, size) when the bar shows the used part (free
            // space, its status from the floor `l`); else the bar is the value against the limit `l`.
            let val = |x: Option<f64>,
                       g: Option<f64>,
                       l: f64,
                       used: Option<(f64, f64)>,
                       (p, pre, unit, of, notes): (usize, &str, &str, String, Vec<String>)| {
                let Some(x) = x else {
                    let b = Some(format!("{pre}?{unit}"));
                    return (
                        "m",
                        Cell {
                            bar: Some(gauge(0.0)),
                            b,
                            of,
                            notes,
                            ..Cell::default()
                        },
                    );
                };
                let g = g.unwrap_or(x);
                let mut bar = gauge(if used.is_some() { l / g } else { g / l });
                if let Some((u, n)) = used {
                    bar.n = gauge(u / n).n;
                }
                let (t, b) = (
                    ["g", "a", "r"]["gar".find(bar.t.as_str()).unwrap_or(0)],
                    format!("{pre}{x:.p$}{unit}"),
                );
                (
                    t,
                    Cell {
                        bt: bar.t.clone(),
                        bar: Some(bar),
                        b: Some(b),
                        of,
                        notes,
                        ..Cell::default()
                    },
                )
            };
            let of = |l: f64| format!("/ {l}");
            let (lt, lv) = val(
                s.load,
                s.load,
                lim.load_per_cpu,
                None,
                (2, "", "", of(lim.load_per_cpu), vec![]),
            );
            // #168: swap-out's status is the window's median (the rule, with pressure warn/critical); `now` is one sample.
            let avg = gate.swap_median.map(|m| format!("· {m:.0} avg"));
            let sw = (0, "", "", of(lim.swapout_rate), avg.into_iter().collect());
            let (st, sv) = val(s.swap, gate.swap_median.or(s.swap), lim.swapout_rate, None, sw);
            let size = |n: Option<f64>, p: usize| n.map(|n| format!("of {n:.p$}"));
            let pw = word(s.pressure).unwrap_or("?");
            let used = |f: Option<f64>, n: Option<f64>| f.zip(n).map(|(f, n)| (n - f, n));
            // The pressure word only when it is up, so the value stays one line at 390.
            let pr = (pw != "normal").then(|| format!("· {pw}"));
            let mm = (
                1,
                "free ",
                " GB",
                String::new(),
                size(s.mem_total, 0).into_iter().chain(pr).collect(),
            );
            let (mt, mv) = val(s.mem, s.mem, lim.min_free_mem_gb, used(s.mem, s.mem_total), mm);
            let mt = ["g", "a", "r"][[mt, tone(pw)]
                .map(|t| "gar".find(t).unwrap_or(0))
                .into_iter()
                .max()
                .unwrap_or(0)];
            let dd = (
                0,
                "free ",
                " GB",
                String::new(),
                size(s.disk_total, 0).into_iter().collect(),
            );
            let (dt, dv) = val(s.disk, s.disk, lim.min_free_gb, used(s.disk, s.disk_total), dd);
            let plain = |k: &str, t: &str, b: Option<String>, text: String, notes: Vec<String>| Cell {
                k: k.into(),
                t: t.into(),
                b,
                text,
                notes,
                ..Cell::default()
            };
            let mut rows = vec![
                row("load/cpu", "load", grey(lt), lv, Some(lim.load_per_cpu)),
                row("swap-out", "swap", grey(st), sv, Some(lim.swapout_rate)),
                row("memory", "mem", grey(mt), mv, Some(lim.min_free_mem_gb)),
            ];
            rows.extend(
                mem.into_iter()
                    .map(|(h, gb, ls)| plain(&h, grey("g"), Some(gb), String::new(), vec![ls])),
            );
            rows.push(row("disk", "disk", grey(dt), dv, Some(lim.min_free_gb)));
            // #223: the top memory users and their owner, so a hog outside the lanes shows at a glance.
            if !s.top.is_empty() {
                let at = (!main).then(|| format!("· {} ago", age(now.saturating_sub(s.seen))));
                rows.push(plain(
                    "top memory",
                    grey("g"),
                    None,
                    s.top.join(" · "),
                    at.into_iter().collect(),
                ));
            }
            rows.extend(cloud.map(|c| plain("cloud", &c[1], None, c[2].clone(), vec![])));
            let ls = [lanes.join(" · "), "none".into()][usize::from(lanes.is_empty())].clone();
            let ln = plain("", "", Some(lanes.len().to_string()), String::new(), vec![ls]);
            rows.push(row("lanes", "lanes", grey("g"), ln, None));
            let tags = h.map_or(s.tags.clone(), |h| crate::hosts::tags(h, Some(&s))).join(" ");
            rows.push(plain("tags", "m", None, String::new(), vec![tags]));
            // Why the dot is amber or red, in words: the rows at the worst status.
            let rank = |t: &str| "gar".find(t).unwrap_or(0);
            let worst = [lt, st, mt, dt].map(rank).into_iter().max().unwrap_or(0);
            let dot = grey(["g", "a", "r"][worst]);
            let why: Vec<String> = [
                (lt, "load", "limit"),
                (st, "swap-out", "limit"),
                (mt, "memory", "floor"),
                (dt, "disk", "floor"),
            ]
            .into_iter()
            .filter(|(t, ..)| worst > 0 && rank(t) == worst)
            .map(|(_, k, l)| {
                format!(
                    "{k} {} {l}",
                    ["near", if l == "floor" { "under" } else { "over" }][worst - 1]
                )
            })
            .collect();
            if dot != "m" && !held && !why.is_empty() {
                (t, words) = (dot, format!("{words} · {}", why.join(", ")));
            }
            out.push(HostCard {
                n: crate::hosts::series(&hist, &name, "lanes", now, false),
                main,
                dot: dot.into(),
                tone: t.into(),
                words,
                rows,
                to: h
                    .filter(|_| !(main && on))
                    .map(|_| ["on", "off"][usize::from(on)].into()),
                lanes: lanes.len(),
                // #303: [sign in ▾]: the configured logins by name; the sheet runs one in a pane on this host.
                setups: crate::setup::all(cfg).into_iter().map(|(s, _)| s).collect(),
                name,
            });
        }
        out
    }
}

/// #141: a 12-cell text gauge of `x` (value ÷ limit; disk: floor ÷ free): its status, cells full, how far past the limit.
/// Green under 80 %, amber to 100 %, red past it (the value then leads with `▶`).
fn gauge(x: f64) -> Bar {
    let t = ["g", "a", "r"][usize::from(x >= 0.8) + usize::from(x > 1.0)];
    let n = (1..=12).filter(|i| f64::from(*i) <= x * 12.0 + 0.5).count();
    let pct = Some(x).filter(|x| *x > 1.0).map(|x| format!("{:.0}", x * 100.0));
    Bar {
        t: t.into(),
        n,
        pct: pct.unwrap_or_default(),
    }
}

/// #141: a `driven` result as the Host tab says it, by code (`ok:<step>` when it ran): status, word, the plain rest with
/// what clears it. `{why}`/`{then}`: the raw reason's first two `;` parts; `{busy}`: the issues in the way.
const WORDS: &str = "HOST_BUSY|a|held|the host is busy: {why} · starts again when it drops
DISK_LOW|a|held|{why} GB floor · `herdsman clean --yes`
ALL_CAPPED|a|waiting|{why} · {then}
CAP|a|waiting|{why} · starts when a lane finishes
NO_HOST HOST_OFFLINE|a|waiting|{why}
SERIAL_BUSY|a|waiting|{slots} full ({busy} must merge first)
ROUND_LIMIT UPDATE_LIMIT|r|needs you|#{n}: {why} · your call on its card
WORKTREE_DIRTY|r|needs you|#{n}: {why} · `herdsman kill` it, or spawn `--supersede`
CI_NOT_GREEN|a|waiting|#{n}'s CI is not green yet
NO_VERDICT STALE_VERDICT VERDICT_FAIL|a|waiting|#{n} has no PASS for its head · a new round
PAUSED|m|paused|{then}
EXIT4|g|started|#{n} · prompt not confirmed yet
ok:merge|g|merged|#{n}
ok:clean|g|cleaned|merged lanes
ok:ci-red|a|sent back|#{n}: red CI, a fix round next
ok:review|g|started|review #{n}
ok:fix|g|started|fix #{n}
ok:conflict|g|started|update #{n}
ok:|g|started|#{n}";

/// #141: a `driven` event in plain words (WORDS, else `failed` and the raw `result`); `serial`: the fleet's label.
pub fn in_words(e: &Event, serial: &str) -> (&'static str, &'static str, String) {
    let (raw, n) = (e.str("result").unwrap_or_default(), e.issue.unwrap_or(0).to_string());
    let body = raw.split_once(": ").map_or(raw, |x| x.1);
    let (why, then) = body.split_once("; ").unwrap_or((body, ""));
    let busy: Vec<_> = raw.split(' ').filter(|w| w.starts_with('#')).collect();
    let noun = format!("serial ({serial})").replace("serial (serial)", "serial");
    let slots = format!("{} {noun} slot{}", busy.len(), ["s", ""][usize::from(busy.len() == 1)]);
    let code = match e.str("code") {
        Some("EXIT") if raw.starts_with("exit 4:") => "EXIT4".into(),
        Some(c) => c.to_string(),
        None => format!("ok:{}", e.str("step").unwrap_or_default()),
    };
    let ok = |l: &&str| code.starts_with("ok:") && l.starts_with("ok:|");
    let row = |l: &&str| l.split('|').next().unwrap_or_default().split(' ').any(|c| c == code) || ok(l);
    let w = WORDS.lines().find(row).unwrap_or("|r|failed|{raw}");
    let w: Vec<_> = w.split('|').collect();
    let x = w[3].replace("{why}", why).replace("{then}", then).replace("{n}", &n);
    let x = x.replace("{busy}", &busy.join(", ")).replace("{slots}", &slots);
    (w[1], w[2], x.replace("{raw}", raw))
}

/// `--demo`'s runner (#39): `runner.json`'s stdout for the longest matching command-line prefix, else exit 1.
#[derive(Default)]
struct Canned(BTreeMap<String, String>);

impl Runner for Canned {
    fn run(&self, prog: &str, args: &[String]) -> Result<crate::runner::Out> {
        let line = format!("{prog} {}", args.join(" "));
        let out = self
            .0
            .iter()
            .filter(|(k, _)| line.starts_with(k.as_str()))
            .max_by_key(|(k, _)| k.len());
        Ok(crate::runner::Out {
            code: i32::from(out.is_none()),
            stdout: out.map_or(String::new(), |o| o.1.clone()),
            stderr: String::new(),
        })
    }
    fn sleep(&self, _: u64) {}
}

/// #172: the files a theme folder may hold; `/themes/<name>/<file>` serves nothing else (#241: and its icons).
const THEME: [&str; 6] = [
    "sprites.svg",
    "theme.json",
    "scene.svg",
    "icon-180.png",
    "icon-192.png",
    "icon-512.png",
];
/// #241: the built-in themes, the `[theme]` picker's choices (a `[serve] theme` of your own joins them).
const BUILT: [&str; 4] = ["farm", "kitchen", "garden", "terminal"];
/// #241: the actions a theme draws (docs/themes.md), each with the pose it falls back to.
const ACTS: [(&str, &str); 7] = [
    ("impl", "working"),
    ("fix", "working"),
    ("rev", "working"),
    ("update", "working"),
    ("idle", "idle"),
    ("done", "done"),
    ("blocked", "blocked"),
];

/// #423: an issue's growth stages, a theme's `stages` keys.
const STAGES: [&str; 7] = ["queued", "impl", "review", "fix", "ready", "merged", "blocked"];

/// #423: the growth stage of an issue in `state` whose lane draws action `act`: its pipeline state, wilted while it is
/// stuck (blocked, dead, needs you); a lane's action where the state says nothing more.
pub fn stage(state: &str, act: &str) -> &'static str {
    match (state.split('(').next().unwrap_or_default(), act) {
        ("blocked" | "dead" | "needs-human" | "⏸ waiting: decision", _) | (_, "blocked") => "blocked",
        ("queued", _) => "queued",
        ("fix", _) | ("", "fix") => "fix",
        ("ready", _) | ("", "done") => "ready",
        ("merged" | "closed", _) => "merged",
        ("review" | "verify", _) | ("", "rev" | "update") => "review",
        _ => "impl",
    }
}

/// The board's theme: `[serve] theme`, default `farm`.
fn active(ctx: &Ctx) -> String {
    Config::load(&ctx.config)
        .ok()
        .and_then(|c| c.serve.theme)
        .unwrap_or_else(|| "farm".into())
}

/// #241: a contract rule: does a value pass.
type Check<'a> = &'a dyn Fn(&serde_json::Value) -> bool;

/// #241: a theme.json against the contract (docs/themes.md); the first problem in words.
pub fn validate(j: &[u8]) -> Result<serde_json::Value, String> {
    use serde_json::Value;
    let v: Value = serde_json::from_slice(j).map_err(|e| format!("not JSON ({e})"))?;
    let o = v.as_object().ok_or("not a JSON object")?;
    let acts: Vec<&str> = ACTS.iter().map(|a| a.0).collect();
    let rows = [&acts[..], &["queued", "needs", "merged", "fail"]].concat(); // #370: Queue and Events kinds too
    let id = |x: &Value| x.is_string();
    let moment = |x: &Value| {
        x.as_object().is_some_and(|m| {
            m.iter().all(|(k, y)| match (k.as_str(), y.as_str()) {
                ("sprite", Some(_)) => true,
                ("anim", Some(a)) => ["hop", "shake", "pulse"].contains(&a),
                _ => false,
            })
        })
    };
    let icon = |x: &Value| x.as_str().is_some_and(|f| f.starts_with("icon-") && THEME.contains(&f));
    // Column-width-safe: at most 3 lines of 8 characters, none of them wide (CJK, emoji) or a control.
    let narrow = |x: &Value| {
        x.as_str().is_some_and(|t| {
            t.lines().count() <= 3
                && t.lines().all(|l| {
                    l.chars().count() <= 8
                        && l.chars()
                            .all(|c| matches!(c as u32, 0x20..=0x7e | 0xa0..=0x10ff | 0x1160..=0x2e7f))
                })
        })
    };
    // #335: a row figure is one line of 3 (its 24 px box), the Host miniature's load one line of 5.
    let line = |x: &Value, n| narrow(x) && x.as_str().is_some_and(|t| !t.contains('\n') && t.chars().count() <= n);
    let (row, load) = (|x: &Value| line(x, 3), |x: &Value| line(x, 5));
    let maps: [(&str, &[&str], Check, &str); 12] = [
        ("poses", &["working", "idle", "done", "blocked"], &id, "a sprite id"),
        ("stages", &STAGES, &id, "a sprite id"), // #423
        ("glyphs", &[], &id, "a string"),
        ("words", &[], &id, "a string"),
        ("actions", &acts, &id, "a sprite id"),
        ("rail", &["item", "needs"], &id, "a sprite id"),
        ("row_sprites", &rows, &id, "a sprite id"),
        (
            "moments",
            &["merged", "fail", "needs-you"],
            &moment,
            "{\"sprite\": id, \"anim\": hop|shake|pulse}",
        ),
        (
            "icons",
            &["180", "192", "512", "favicon"],
            &icon,
            "icon-180.png, icon-192.png or icon-512.png",
        ),
        ("ascii", &acts, &narrow, "at most 3 lines of 8 narrow characters"),
        ("ascii_rows", &acts, &row, "one line of at most 3 narrow characters"),
        (
            "ascii_load",
            &["calm", "busy", "held"],
            &load,
            "one line of at most 5 narrow characters",
        ),
    ];
    for (k, keys, ok, want) in maps {
        let m = match o.get(k) {
            None => continue,
            Some(Value::Bool(false)) if matches!(k, "row_sprites" | "moments") => continue,
            Some(Value::Object(m)) => m,
            _ => return Err(format!("`{k}` must be an object")),
        };
        if let Some((x, _)) = m
            .iter()
            .find(|(x, y)| !keys.is_empty() && !keys.contains(&x.as_str()) || !ok(y))
        {
            let of = if keys.is_empty() {
                String::new()
            } else {
                format!(" (keys: {})", keys.join(", "))
            };
            return Err(format!("`{k}.{x}` must be {want}{of}"));
        }
    }
    let look = |x: &Value| {
        x.as_object()
            .is_some_and(|m| m.iter().all(|(k, y)| k.starts_with("--") && y.is_string()))
    };
    let levels = |x: &Value| {
        x.as_object().is_some_and(|m| {
            m.iter()
                .all(|(k, y)| ["calm", "busy", "held"].contains(&k.as_str()) && y.is_string())
        })
    };
    // #423: a species is a look with its sprite suffix, `id`.
    let species = |x: &Value| {
        x["id"].is_string()
            && x.as_object().is_some_and(|m| {
                m.iter()
                    .all(|(k, y)| (k == "id" || k.starts_with("--")) && y.is_string())
            })
    };
    let scheme = |x: &Value| {
        x.is_string()
            || x.as_object().is_some_and(|m| {
                m.len() == 2
                    && ["light", "dark"]
                        .iter()
                        .all(|k| m.get(*k).is_some_and(Value::is_string))
            })
    };
    let ls = |x: &Value| {
        ["weather", "heat", "light", "none"].contains(&x["kind"].as_str().unwrap_or_default())
            && x.get("levels").is_none_or(levels)
    };
    let modes = |x: &Value| {
        x.as_array().is_some_and(|a| {
            let m: BTreeSet<&str> = a
                .iter()
                .filter_map(Value::as_str)
                .filter(|m| ["scene", "ascii", "glyph"].contains(m))
                .collect();
            !a.is_empty() && m.len() == a.len()
        })
    };
    let rules: [(&str, Check, &str); 7] = [
        ("scene", &Value::is_object, "an object"),
        (
            "scheme",
            &scheme,
            "a colour scheme's name, or {\"light\": name, \"dark\": name}",
        ),
        (
            "species",
            &|x| x.as_array().is_some_and(|a| !a.is_empty() && a.iter().all(species)),
            "a list of {\"id\": suffix, \"--var\": \"value\"} objects",
        ),
        (
            "looks",
            &|x| x.as_array().is_some_and(|a| a.iter().all(look)),
            "a list of {\"--var\": \"value\"} objects",
        ),
        (
            "load_scene",
            &ls,
            "{\"kind\": weather|heat|light|none, \"levels\": {calm, busy, held}}",
        ),
        ("modes", &modes, "a list of scene, ascii, glyph, each at most once"),
        (
            "ascii_loading",
            &narrow,
            "at most 3 lines (frames) of 8 narrow characters",
        ),
    ];
    match rules.iter().find(|r| o.get(r.0).is_some_and(|x| !r.1(x))) {
        Some((k, _, want)) => Err(format!("`{k}` must be {want}")),
        None => Ok(v),
    }
}

/// #241: a valid theme.json with what it lacks filled in (docs/themes.md): an action is the theme's own, else its
/// pose's sprite, else farm's; a row sprite its action's; the rail item its idle action, the needs variant the item;
/// `loading` only the theme's own; the rest farm's, key by key.
pub fn resolve(own: &serde_json::Value, farm: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    let mut t = farm.clone();
    for (k, v) in own.as_object().into_iter().flatten() {
        t[k] = match (&t[k], v) {
            (Value::Object(f), Value::Object(o)) => Value::Object(f.clone().into_iter().chain(o.clone()).collect()),
            _ => v.clone(),
        };
    }
    let pick = |k: &str, x: &str| own[k][x].as_str().map(Value::from);
    for (a, pose) in ACTS {
        t["actions"][a] = pick("actions", a)
            .or_else(|| pick("poses", pose))
            .unwrap_or_else(|| farm["actions"][a].clone());
        if t["row_sprites"] != false {
            t["row_sprites"][a] = pick("row_sprites", a).unwrap_or_else(|| t["actions"][a].clone());
        }
    }
    t["words"] = own.get("words").cloned().unwrap_or_else(|| json!({})); // #370: a word it lacks is the plain one
    t["scheme"] = own.get("scheme").cloned().unwrap_or_else(|| "github-dark".into());
    t["loading"] = own.get("loading").cloned().unwrap_or_default(); // #305: none is the spinning ↻, never farm's
    t["rail"]["item"] = pick("rail", "item").unwrap_or_else(|| t["actions"]["idle"].clone());
    t["rail"]["needs"] = pick("rail", "needs").unwrap_or_else(|| t["rail"]["item"].clone());
    t
}

/// #244: every colour scheme by name: the built-ins, then `<config dir>/schemes/<name>.json` (one named like a
/// built-in wins). Unchecked here: the page gates each one (serve.js) before it offers it; unreadable JSON is `null`.
fn schemes(ctx: &Ctx) -> BTreeMap<String, serde_json::Value> {
    let own = std::fs::read_dir(ctx.config.with_file_name("schemes"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let (p, ok) = (e.path(), |b: u8| b.is_ascii_alphanumeric() || b == b'-');
            let n = p.file_stem()?.to_str()?.to_string();
            (p.extension()? == "json" && n.bytes().all(ok)).then(|| (n, std::fs::read(&p).unwrap_or_default()))
        });
    let all = SCHEMES.iter().map(|(n, j)| (n.to_string(), j.to_vec())).chain(own);
    all.map(|(n, j)| (n, serde_json::from_slice(&j).unwrap_or_default()))
        .collect()
}

/// Theme `name`'s `file` (`<config dir>/themes/<name>/`, then the built-in `<name>`, then farm's; an icon then the
/// board's), and its theme.json checked: an invalid or missing one means farm's files. `theme.json` comes resolved.
fn theme(ctx: &Ctx, name: &str, file: &str) -> (Option<Vec<u8>>, Result<(), String>) {
    let get = |n: &str, f: &str| {
        let own = ctx.config.with_file_name("themes").join(n).join(f);
        let built = ASSETS
            .iter()
            .find(|a| a.0 == format!("themes/{n}/{f}"))
            .map(|a| a.2.to_vec());
        (!n.contains(['/', '\\', '.']))
            .then(|| std::fs::read(own).ok())
            .flatten()
            .or(built)
    };
    let farm = validate(&get("farm", "theme.json").unwrap_or_default()).unwrap_or_default();
    let own = get(name, "theme.json")
        .ok_or_else(|| "no theme.json".to_string())
        .and_then(|j| validate(&j));
    let n = if own.is_ok() { name } else { "farm" };
    let body = match file {
        "theme.json" => Some(resolve(own.as_ref().unwrap_or(&farm), &farm).to_string().into_bytes()),
        f => get(n, f)
            .or_else(|| get("farm", f))
            .or_else(|| FILES.iter().find(|x| x.0 == f).map(|x| x.2.to_vec())),
    };
    (body, own.map(|_| ()))
}

/// `serve --export <dir> [--demo]` (#39, docs/serve.md): the page and its files as static files with relative URLs.
pub fn export(ctx: &Ctx, r: &dyn Runner, dir: &std::path::Path, demo: bool) -> Result<()> {
    let canned = if demo {
        Canned(serde_json::from_str(&std::fs::read_to_string(
            ctx.config.with_file_name("runner.json"),
        )?)?)
    } else {
        Canned::default()
    };
    // #344: the demo's log goes into a scratch database, so `demo/` stays as committed.
    let _scratch = demo.then(crate::db::Scratch::start);
    let (r, events): (&dyn Runner, _) = (if demo { &canned } else { r }, read(&ctx.state)?);
    let ctx = &Ctx {
        now: if demo {
            events.iter().map(|e| e.ts).max().unwrap_or(0) + 60
        } else {
            ctx.now
        },
        ..ctx.clone()
    };
    let s = Server {
        export: Some(demo),
        host: Arc::new(Mutex::new(host::sample(r))),
        ..Server::new(ctx, r, demo.then(|| "demo".into()))
    };
    let (page, herd) = (
        s.page(ctx, &BTreeMap::new(), demo)?,
        json!(s.herd(ctx, None)?).to_string(),
    );
    let (sw, v) = (SW.replace("@V", &format!("{VER:08x}")), format!("a/{VER:08x}/"));
    // #63: the demo-only `?audit=<view>` UI gate (scripts/ui-audit.sh); never in the live page.
    let page = if demo {
        page.replace(
            "</body>",
            &format!("<script>{}</script></body>", include_str!("../assets/ui-audit.js")),
        )
    } else {
        page
    };
    std::fs::create_dir_all(dir.join("lane"))?;
    // #266: the API reads the TypeScript board makes (Events, #267 Host, #268 Queue, #269 the needs cards, Lanes and
    // [chat]), as files: every event, whatever the query.
    let (state, all) = (
        json!(s.state(ctx)?).to_string(),
        json!(v1::events(ctx, 0, u64::MAX, usize::MAX)),
    );
    let (all, host) = (all.to_string(), json!(s.host_tab(ctx)).to_string());
    let queue = json!(s.queue_tab(ctx)).to_string();
    let [(_, needs), (_, lanes), (_, chat)] = s.parts(ctx, demo);
    let files = [
        ("index.html", page.as_bytes()),
        ("herd.json", herd.as_bytes()),
        ("sw.js", sw.as_bytes()),
        ("api/v1/state", state.as_bytes()),
        ("api/v1/events", all.as_bytes()),
        ("api/v1/host", host.as_bytes()),
        ("api/v1/queue", queue.as_bytes()),
        ("api/v1/needs", needs.as_bytes()),
        ("api/v1/lanes", lanes.as_bytes()),
        ("api/v1/chat", chat.as_bytes()),
    ]
    .map(|(n, b)| (n.to_string(), b));
    for (name, body) in files
        .into_iter()
        .chain(FILES.iter().map(|f| (f.0.to_string(), f.2)))
        .chain(ASSETS.iter().map(|a| (v.clone() + a.0, a.2)))
    {
        std::fs::create_dir_all(dir.join(&name).with_file_name(""))?;
        std::fs::write(dir.join(name), body)?;
    }
    // #241: every theme the picker offers, as `/themes/<name>/<file>` serves it.
    let name = active(ctx);
    for n in BUILT
        .iter()
        .copied()
        .chain([name.as_str()])
        .filter(|n| !n.contains(['/', '\\', '.']))
    {
        std::fs::create_dir_all(dir.join("themes").join(n))?;
        for f in THEME {
            theme(ctx, n, f)
                .0
                .map_or(Ok(()), |b| std::fs::write(dir.join("themes").join(n).join(f), b))?;
        }
    }
    // #386: the files threads carry, as `/thread/<t>/<name>` serves them.
    for e in events.iter().filter(|e| e.kind == "thread-msg") {
        for f in chat::files_of(e) {
            let (t, name) = (
                e.str("thread").unwrap_or_default(),
                f["name"].as_str().unwrap_or_default(),
            );
            let to = dir.join("thread").join(t);
            let b = chat::file(ctx, &events, t, name, demo).and_then(|p| std::fs::read(p).ok());
            if let Some(b) = b.filter(|_| !name.contains('/')) {
                std::fs::create_dir_all(&to)?;
                std::fs::write(to.join(name), b)?;
            }
        }
    }
    let (cfg, st) = (Config::load(&ctx.config)?, events.states.clone());
    for l in s
        .lanes(ctx.now, &cfg, &events, &st)
        .unwrap_or_default()
        .into_iter()
        .filter(|_| demo)
    {
        let safe = !l.name.starts_with('.') && l.name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
        if let Some(j) = s
            .transcript(ctx, &BTreeMap::from([("agent".to_string(), l.name.clone())]))
            .filter(|_| safe)
        {
            std::fs::write(dir.join("lane").join(format!("{}.json", l.name)), j)?;
        }
    }
    Ok(())
}

/// #268: an issue's PR line as data (web/src/queue.ts draws it), from the cached `herdsman pr` read only (never calls
/// `gh`); #269: `fc` none, a lane's fleet not configured (no links, no required checks).
fn pr_data(ctx: &Ctx, fc: Option<&crate::config::Fleet>, fleet: &str, issue: u64, s: Option<&IssueState>) -> PrLine {
    let pr = gh::cached_pr(ctx, fleet, issue);
    let n = pr["number"].as_u64();
    let v = s.and_then(|s| s.verdicts.last().map(|v| (s, v.1.as_str())));
    let req = fc.map(|c| c.required_checks.as_slice()).unwrap_or_default();
    PrLine {
        pr: n,
        run: (n.is_none() && fc.is_some())
            .then(|| gh::cached_run(ctx, fleet, issue))
            .flatten(),
        ci: (!pr.is_null()).then(|| gh::ci(&gh::required(&pr["statusCheckRollup"], req)).into()),
        verdict: v.map(|v| v.1.into()),
        vtone: v.map_or("", |v| tone(v.1)).into(),
        rounds: s.map_or(0, |s| s.rounds),
        form: v
            .filter(|v| v.1.ends_with(crate::events::FORM))
            .map(|(s, _)| format!("{}/{}", s.form, fc.and_then(|c| c.form_rounds).unwrap_or_default())),
    }
}

/// The queue row's dim line from the caches only: the issue title, then the PR's when it differs (#331: CSS ellipsises it).
fn titles(ctx: &Ctx, fleet: &str, issue: u64) -> Option<String> {
    let spec = gh::cached_title(ctx, fleet, issue);
    let pr = gh::cached_pr(ctx, fleet, issue);
    let pr = pr["number"]
        .as_u64()
        .zip(pr["title"].as_str())
        .filter(|p| Some(p.1) != spec.as_deref())
        .map(|(n, p)| format!("PR #{n} · {p}"));
    let t = [spec, pr].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    (!t.is_empty()).then_some(t)
}

/// #162: a fleet's [config] sheet's data: GENERAL, LIMITS, ROLES (the `set` pickers), ALERTS, HOSTS / NEEDS
/// (web/src/queue.ts draws it). Only the role pickers (through `set`) and #401's Conductor choices write; the rest is
/// read-only, edited in the config file named on top.
fn config_sheet(ctx: &Ctx, cfg: &Config, fleet: &str, fc: &crate::config::Fleet, st: &States) -> ConfigSheet {
    let path = ctx.config.display().to_string();
    let home = crate::config::home().display().to_string();
    let l = &cfg.limits;
    let line_cap = std::fs::read_to_string(fc.local_path().join("ci/cap.toml"))
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
        .and_then(|t| t.get("fleet")?.get(fleet)?.get("line_cap")?.as_integer());
    let notify = |n: &[crate::config::Notify]| {
        let on = |n: &crate::config::Notify| {
            if n.on.is_empty() {
                "default events".into()
            } else {
                n.on.join(", ")
            }
        };
        let kind = |n: &crate::config::Notify| if n.webpush() { "web push" } else { "command" };
        n.iter().map(|n| format!("{} ({})", kind(n), on(n))).collect()
    };
    let strs = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<_>>();
    let role = |(name, r): (&String, &Role)| RoleRow {
        name: name.clone(),
        profile: r.profile_or(name),
        harness: r.harness.clone(),
        model: r.model.clone(),
        effort: r.effort.clone(),
        efforts: strs(crate::harness::efforts(&r.harness)),
        // Unset: what the lane actually gets (the profile's/harness's model, `[limits].effort`).
        model_default: if r.harness == "hermes" {
            "profile".into()
        } else {
            r.harness.clone()
        },
        effort_default: l.effort.clone().unwrap_or_else(|| "none".into()),
        accounts: r.account_list(),
        mcp: r.mcp.clone(),
    };
    let conductor = fc.role.get("conductor").map(|r| Conductor {
        default: r.default,
        profile: r.profile_or("conductor"),
        harness: r.harness.clone(),
        model: r.model.clone(),
        effort: r.effort.clone(),
        efforts: strs(crate::harness::efforts(&r.harness)),
        models: strs(&crate::conductor::models(fc)),
    });
    let harness = |h: &&str| Harness {
        name: (*h).into(),
        efforts: strs(crate::harness::efforts(h)),
    };
    ConfigSheet {
        path: path.strip_prefix(&home).map_or(path.clone(), |p| format!("~{p}")),
        file: ctx
            .config
            .file_name()
            .map_or("fleet.toml".into(), |f| f.to_string_lossy().into_owned()),
        repo: fc.repo.clone(),
        local: fc.local.clone(),
        integration: fc.integration.clone(),
        effort: l.effort.clone(),
        color_set: fc.color.is_some(),
        env: fc.env.keys().cloned().collect(),
        node_modules: fc.clone_node_modules,
        // #173: the built-in pipeline's defaults; a fleet with its own pipeline sets them per stage.
        builtin: fc.flow().builtin,
        review_rounds: l.review_rounds,
        update_rounds: l.update_rounds,
        stall_minutes: l.stall_minutes,
        ask_minutes: l.ask_default_minutes,
        max_impl: fc.max_impl,
        max_rev: fc.max_rev,
        line_cap,
        serial: (fc.serial.iter())
            .map(|g| format!("🔒{} {} at a time: {}", g.name, g.max, g.paths.join(", ")))
            .collect(),
        deny: fc.deny(l).iter().map(|d| d.cmd().to_string()).collect(),
        required: fc.required_checks.clone(),
        merge_label: fc.merge_label.clone(),
        notify: notify(&fc.notify),
        global: notify(&cfg.notify),
        pipe: pipe(fleet, fc, st),
        models: cfg.models.clone(),
        harnesses: crate::harness::NAMES.iter().map(harness).collect(),
        roles: fc.role.iter().filter(|r| r.0 != "conductor").map(role).collect(),
        conductor,
    }
}

type Words = (&'static str, &'static str, String, bool);

/// #170: an event as the Events tab says it: (glyph, tone, plain words, shown under `[important]`). `blocked` and
/// `driven` take #141's WORDS; the capacity holds stay out of `[important]` (herdsman waiting for room).
pub fn words(e: &Event, serial: &str) -> Words {
    let s = |k: &str| e.str(k).unwrap_or_default();
    let (r, round) = (
        e.str("reason").map_or(String::new(), |r| format!(" · {r}")),
        &e.data.get("round"),
    );
    let round = round.map_or(String::new(), |n| format!(" · round {n}"));
    match e.kind.as_str() {
        "merged" => ("✓", "g", format!("merged into {} · PR #{}", s("into"), s("pr")), true),
        // #417: the verify stage's verdicts say so.
        "verdict" if s("verdict") == "PASS" => ("✓", "g", format!("{} PASS{round}", check(s("stage"))), true),
        "verdict" => ("✗", "r", format!("{} FAIL{round}", check(s("stage"))), true),
        "needs-human" => ("!", "r", format!("needs you{r}"), true),
        "needs-decision" => ("!", "r", format!("asks you ({}) · {}", s("id"), s("question")), true),
        "ask-waiting" => ("!", "r", format!("still waiting on you{r}"), true),
        "notify-failed" => (
            "✗",
            "r",
            format!("alert failed · the {} alert: {}", s("event"), s("error")),
            true,
        ),
        "lane-dead" => ("✗", "r", format!("lane gone{r}"), true),
        "hook-denied" => ("!", "a", format!("hook denied · {}", s("rule")), false),
        "blocked" | "driven" => {
            let mut b = e.clone();
            b.data.entry("result").or_insert_with(|| e.data["reason"].clone());
            let (t, w, x) = in_words(&b, serial);
            let txt = format!("{w} · {x}");
            let hold = matches!(
                e.str("code"),
                Some("HOST_BUSY" | "SERIAL_BUSY" | "CAP" | "ALL_CAPPED" | "PAUSED")
            );
            match (e.str("code"), t) {
                (None, _) => ("·", "m", format!("drive · {txt}"), false),
                (_, "r") if w == "needs you" => ("!", "r", txt, true),
                (_, "r") => ("✗", "r", txt, true),
                (_, "g") => ("▸", "l", txt, true),
                _ => ("⊘", "a", txt, !hold),
            }
        }
        "spawn" => ("▸", "l", format!("{} started{round}", s("role")), false),
        "push-sent" => ("↗", "m", format!("alert sent to {} phone(s)", e.data["devices"]), false),
        "clean" => ("·", "m", format!("lane cleaned ({})", s("why")), false),
        "decision" => ("✓", "m", format!("you answered {} · {}", s("id"), s("choice")), false),
        "override" => ("⊘", "m", format!("override{r}"), false),
        "queue" => {
            let n: Vec<String> = e.data["issues"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|n| format!("#{n}"))
                .collect();
            ("·", "m", format!("queue · {} {}", s("op"), n.join(" ")), false)
        }
        k if "stall orphan-lane orphan-issue conflict-predicted heads-up ready-unmerged ci-missing verify-no-recipe"
            .split(' ')
            .any(|x| x == k) || k.starts_with("orch-") =>
        {
            ("!", "a", format!("{k}{r}"), false)
        }
        k => ("·", "m", format!("{k}{r}"), false),
    }
}

/// #417: a verdict's word for its stage: `verify`, else `review`.
fn check(stage: &str) -> &'static str {
    if stage == "verify" {
        "verify"
    } else {
        "review"
    }
}

/// A check role's lane (`rev`, or #173 any check stage's): read-only from the page.
fn reviewer(ctx: &Ctx, fleet: &str, role: &str) -> bool {
    let cfg = Config::load(&ctx.config).ok();
    role == "rev" || cfg.and_then(|c| Some(c.fleet.get(fleet)?.family(role) == "rev")) == Some(true)
}

/// #173: where an issue is in its fleet's pipeline: (stage, the mode or round its chip shows); `None` off the graph.
fn stage_at<'a>(fc: &'a crate::config::Fleet, s: &'a IssueState) -> Option<(&'a str, String)> {
    let p = fc.flow();
    let build = p.stage_of(s.build.as_deref().unwrap_or(p.start()));
    let n = s.state.split(['(', ')']).nth(1).unwrap_or_default();
    match s.state.split('(').next()? {
        "impl" => Some((build, String::new())),
        "fix" if s.updating => Some((build, "update".into())),
        "fix" => Some((build, "fix".into())),
        "review" => Some((s.stage.as_str(), format!("r{n}"))),
        "ready" | "blocked" => Some((p.gate(), String::new())),
        _ => None,
    }
}

/// Kind → (glyph class, glyph): ▲ build, ◆ check, ◫ gate, ✓ merge, ! park.
fn glyph(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "build" => ("b", "▲"),
        "check" => ("c", "◆"),
        "gate" => ("g", "◫"),
        "merge" => ("m", "✓"),
        _ => ("x", "!"),
    }
}

/// #173: the card's "where it is now": `○ ─ ▲ impl ─ ◆ rev ─ ◫ gates ─ ✓ merge`, the current stage lit, the stages
/// before it plain, a check with a PASS at the PR head ticked (web/src/queue.ts draws it); none off the graph.
fn stages(fc: &crate::config::Fleet, s: &IssueState, head: &str) -> Vec<Stage> {
    let Some((at, mode)) = stage_at(fc, s) else {
        return vec![];
    };
    let p = fc.flow();
    let pos = p.stage.iter().position(|x| x.name == at).unwrap_or(0);
    let chip = |(i, x): (usize, &crate::pipeline::Stage)| {
        let (k, g) = glyph(&x.kind);
        let ok = x.kind == "check"
            && s.verdicts
                .iter()
                .rev()
                .find(|v| v.2 == x.name && v.0 == head)
                .is_some_and(|v| v.1 == "PASS");
        Stage {
            name: if i == pos && !mode.is_empty() {
                format!("{} · {mode}", x.name)
            } else {
                x.name.clone()
            },
            k: k.into(),
            g: g.into(),
            at: i.cmp(&pos) as i8,
            ok: ok && i != pos,
        }
    };
    p.stage.iter().enumerate().map(chip).collect()
}

/// #173: the fleet's Pipeline group's data; serve.js draws it to docs/design/pipeline: the validity line or the broken
/// rules, the graph with `n here` per stage, the legend, the pipeline as fleet.toml text.
fn pipe(fleet: &str, fc: &crate::config::Fleet, st: &States) -> serde_json::Value {
    let p = fc.flow();
    let mine: Vec<&IssueState> = st.values().filter(|s| s.fleet == fleet && !s.done()).collect();
    let count = |f: &dyn Fn(&IssueState) -> bool| mine.iter().filter(|s| f(s)).count();
    let stages: Vec<serde_json::Value> = (p.stage.iter())
        .map(|s| {
            let run = s.role.as_deref().and_then(|r| fc.role.get(r)).map(|r| {
                let m = r.model.as_deref().unwrap_or("default");
                format!("{} · {m} · effort {}", r.harness, r.effort.as_deref().unwrap_or("none"))
            });
            let here = count(&|x| stage_at(fc, x).is_some_and(|a| a.0 == s.name));
            json!({"name": s.name, "kind": s.kind, "role": s.role, "run": run, "next": s.next, "rounds": s.rounds,
                "line": s.line, "here": here, "loop": p.loop_of(&s.name)})
        })
        .collect();
    let bad: Vec<serde_json::Value> = (fc.bad.iter())
        .map(|b| json!({"rule": b.rule, "msg": b.msg, "stage": b.stage, "line": b.line}))
        .collect();
    let checks: Vec<&str> = p.checks().map(|c| c.name.as_str()).collect();
    json!({"fleet": fleet, "into": fc.integration, "builtin": p.builtin, "stages": stages, "bad": bad,
        "checks": checks, "required": fc.required_checks, "queued": count(&|s| s.state == "queued"),
        "parked": count(&|s| s.state == "needs-human")})
}
