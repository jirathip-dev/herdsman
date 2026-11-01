//! `herdsman serve`, the phone page (docs/serve.md): std `TcpListener`, one request at a time;
//! `/live` SSE clients get a thread each, fed by the 1 s watcher thread.
use crate::config::{Config, Role};
use crate::events::{age, append, fold, plain, read, Event, IssueState, States};
use crate::gate::{refused, Refusal};
use crate::host::{self, Host};
use crate::runner::{Out, Runner};
use crate::{devices, gh, herdr, lanes, queue, Ctx};
use anyhow::{bail, Context, Result};
use serde_json::json;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::io::ErrorKind::{TimedOut, WouldBlock};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SW: &str = include_str!("../assets/sw.js");
/// #57: served under `/a/<VER>/` as immutable (the SW precaches them); the page links them.
const ASSETS: [(&str, &str, &[u8]); 5] = [
    ("serve.css", "text/css", include_bytes!("../assets/serve.css")),
    ("herd.css", "text/css", include_bytes!("../assets/herd.css")),
    ("serve.js", "text/javascript", include_bytes!("../assets/serve.js")),
    ("herd.js", "text/javascript", include_bytes!("../assets/herd.js")),
    ("horses.svg", "image/svg+xml", include_bytes!("../assets/horses.svg")),
];
/// At the root, the fixed paths the manifest and iOS name: cached a day (`/sw.js`, VER filled in, is no-store).
const FILES: [(&str, &str, &[u8]); 4] = [
    (
        "manifest.webmanifest",
        "application/manifest+json",
        include_bytes!("../assets/manifest.webmanifest"),
    ),
    ("icon-180.png", "image/png", include_bytes!("../assets/icon-180.png")),
    ("icon-192.png", "image/png", include_bytes!("../assets/icon-192.png")),
    ("icon-512.png", "image/png", include_bytes!("../assets/icon-512.png")),
];
/// FNV-1a over the assets at build time: their `/a/<hash8>/` path.
const VER: u32 = {
    let (mut h, mut k, mut i) = (0x811c_9dc5u32, 0, 0);
    while k < 5 {
        let b = ASSETS[k].2;
        while i < b.len() {
            h = (h ^ b[i] as u32).wrapping_mul(0x0100_0193);
            i += 1;
        }
        (k, i) = (k + 1, 0);
    }
    h
};

type Clients = Arc<Mutex<Vec<(Sender<String>, Option<String>)>>>;
type Cache = Arc<Mutex<BTreeMap<String, (u64, Option<String>)>>>;

pub fn serve(ctx: &Ctx, r: &dyn Runner, addr: &str, read_only: bool) -> Result<()> {
    let l = TcpListener::bind(addr)?;
    let mode = if read_only { "read-only view" } else { "phone page" };
    eprintln!("herdsman serve: {mode} on http://{}/", l.local_addr()?);
    serve_on(ctx, r, l, read_only)
}

#[allow(clippy::needless_pass_by_value)] // public API: the listener is handed over and lives as long as the server
pub fn serve_on(ctx: &Ctx, r: &dyn Runner, l: TcpListener, read_only: bool) -> Result<()> {
    let s = Server::new(
        ctx,
        r,
        (!read_only)
            .then(|| l.local_addr().map(|a| a.to_string()))
            .transpose()?,
    );
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
    std::thread::scope(|sc| {
        // #57: the host every 10 s (its 2 s vm_stat delta included) here, never on a request.
        sc.spawn(move || loop {
            *lock(&h) = host::sample(r);
            std::thread::sleep(Duration::from_secs(10));
        });
        sc.spawn(move || loop {
            w.tick();
            std::thread::sleep(Duration::from_secs(1));
        });
        l.incoming().flatten().for_each(|st| drop(s.handle(st)));
    });
    Ok(())
}

/// `Server::cached` as a Runner (runner, cache, now, ttl): also `waiting::scan`'s per-issue `git log` on a request.
struct Memo<'a>(&'a dyn Runner, &'a Cache, u64, u64);
impl Runner for Memo<'_> {
    fn run(&self, prog: &str, args: &[String]) -> Result<Out> {
        let (key, cache) = (format!("{prog} {}", args.join(" ")), || lock(self.1));
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
        Ok(out.map_or(
            Out {
                code: 1,
                ..Out::default()
            },
            |stdout| Out {
                stdout,
                ..Out::default()
            },
        ))
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
    sent: RefCell<BTreeMap<String, Vec<u64>>>,
    /// `/live` client threads (at most 8; the paired device's hash, shared with the watcher), the
    /// watcher's state, and `kick`: an answer was tapped, the watcher re-reads panes and pushes now.
    clients: Clients,
    kick: Arc<AtomicBool>,
    watch: RefCell<Watch>,
    /// `serve --export` (#39): Some(demo), static files with relative URLs; `true` is the inert demo.
    export: Option<bool>,
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

/// green = merged/PASS/ready/working, amber = queued/impl/review/fix/idle, red = blocked/dead/FAIL.
fn tone(state: &str) -> &'static str {
    match state.split('(').next().unwrap_or_default() {
        "merged" | "ready" | "PASS" | "working" | "done" | "ok" | "normal" => "g",
        "blocked" | "needs-human" | "dead" | "FAIL" | "critical" | "capped" => "r",
        "parked" => "m",
        _ => "a",
    }
}

/// The non-empty HTML lines, one `<p>` each.
fn card(lines: &[String]) -> String {
    lines
        .iter()
        .filter(|l| !l.is_empty())
        .map(|l| format!("<p>{l}</p>"))
        .collect()
}

/// One narrow 3-field row (`a`, state, age) that expands to the stacked card `lines`.
fn row(class: &str, lead: &str, a: &str, state: &str, age: &str, lines: &[String]) -> String {
    let card = card(lines);
    format!(
        "<details class=\"{class}\"><summary>{lead}<span>{}</span><span class={}>{}</span><span class=m>{}</span></summary>{card}</details>",
        esc(a),
        tone(state),
        esc(state),
        esc(age)
    )
}

/// Form ops only a paired device may POST.
const PAIRED_OPS: [&str; 3] = ["unpair", "answer", "drive"];

/// The non-empty fields, ` · `-joined (no `-` placeholders).
fn join(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ")
}

fn hidden(op: &str, fleet: &str, mt: &str) -> String {
    format!("<input type=hidden name=op value={op}><input type=hidden name=fleet value=\"{}\"><input type=hidden name=mtime value={mt}>", esc(fleet))
}

const GALLOP: [&str; 2] = ["    _/>\n~(___)\n /  \\", "    _/>\n~(___)\n  ><"];
const STAND: &str = "    _/>\n~(___)\n  | |";
const REST: &str = "\n    _/>\n~(___)_";
const REAR: &str = "     />\n ~(_/\n  / \\";

/// Corral's 8 coats (HerdArt.swift): body, shade, mane; chestnut, palomino, dun and buckskin carry a blaze.
const COATS: [(&str, &str, &str, bool); 8] = [
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
fn coat(name: &str) -> String {
    let (c, d, m, blaze) = COATS[usize::from_str_radix(&devices::sha256(name.as_bytes())[..2], 16).unwrap_or(0) % 8];
    format!(
        "--c:#{c};--d:#{d};--m:#{m};--b:{}",
        if blaze { "#efe7dc" } else { "transparent" }
    )
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
        .map_or((tone, shown), |h| ("m", format!("parked · {}", on(h))));
    (title, tone, shown, ago(ctx.now, l.since))
}

/// The lane's wait reason while herdr shows it idle, done or unknown.
fn wait_of(l: &lanes::Lane, waits: &[crate::waiting::Waiting]) -> Option<crate::waiting::Waiting> {
    let w = waits
        .iter()
        .find(|w| w.lane == l.name)
        .cloned()
        .or_else(|| crate::waiting::orch(waits, l.fleet.as_deref()?).filter(|_| l.role == "orch"));
    w.filter(|_| matches!(l.state.as_str(), "idle" | "done" | "unknown" | "parked"))
}

/// The state as an avatar in all three modes (CSS shows `body[data-mode]`'s); coat stable per name.
pub fn avatar(name: &str, state: &str) -> String {
    let (pose, glyph, art) = match state {
        "working" => (
            1,
            "▶",
            format!("<i class=\"asc run\"><b>{}</b><b>{}</b></i>", GALLOP[0], GALLOP[1]),
        ),
        "done" => (3, "✓", format!("<i class=asc><b>{REST}</b></i>")),
        "blocked" | "dead" => (4, "✗", format!("<i class=asc><b>{REAR}</b></i>")),
        _ => (2, "⏸", format!("<i class=asc><b>{STAND}</b></i>")),
    };
    format!("<span class=\"av {} {state}\"><i class=gl>{glyph}</i>{art}<svg class=px viewBox=\"0 0 132 100\" style=\"{}\"><use href=#hp{pose} /></svg></span>", tone(state), coat(name))
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
            sent: RefCell::default(),
            clients: Arc::default(),
            kick: Arc::default(),
            watch: RefCell::default(),
            export: None,
        }
    }

    fn handle(&self, mut s: TcpStream) -> std::io::Result<()> {
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        let (mut buf, mut chunk) = (vec![], [0u8; 2048]);
        let done = |b: &[u8]| {
            let t = String::from_utf8_lossy(b);
            t.split_once("\r\n\r\n").is_some_and(|(h, body)| {
                body.len() >= header(h, "content-length").and_then(|n| n.parse().ok()).unwrap_or(0)
            })
        };
        while !done(&buf) && buf.len() < 32_768 {
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
        let mut clients = lock(&self.clients);
        clients.retain(|c| c.0.send(String::new()).is_ok());
        if req.starts_with("GET /live ") && self.allowed(self.ctx, &req) && clients.len() < 8 {
            s.set_write_timeout(Some(Duration::from_secs(10)))?;
            s.set_read_timeout(Some(Duration::from_millis(1)))?;
            let (tx, rx) = channel::<String>();
            clients.push((
                tx,
                self.device(
                    &Ctx {
                        now: crate::now(),
                        ..self.ctx.clone()
                    },
                    &req,
                )
                .map(|d| d.hash),
            ));
            let mut msg =
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n\r\n: ping\n\n"
                    .to_string();
            // Each second: a frag, else a peek (the peer closed: free the slot), else a ping every 15 s.
            std::thread::spawn(move || {
                while s.write_all(msg.as_bytes()).is_ok() {
                    let t = Instant::now();
                    msg = loop {
                        match rx.recv_timeout(Duration::from_secs(1)) {
                            Ok(m) if !m.is_empty() => break m,
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
        s.write_all(&self.respond(&req, crate::now()))
    }

    /// The watcher's 1 s tick (docs/serve.md): on a change, re-render per audience and push only the changed fragments.
    fn tick(&self) {
        let mut w = self.watch.borrow_mut();
        let now = crate::now();
        let known: Vec<String> = devices::load(self.ctx).into_iter().map(|d| d.hash).collect();
        lock(&self.clients).retain(|c| c.1.as_ref().is_none_or(|h| known.contains(h)));
        let log = std::fs::metadata(&self.ctx.state)
            .ok()
            .map(|m| (m.len(), m.modified().ok()));
        let kicked = self.kick.swap(false, Relaxed);
        if kicked {
            self.purge();
        }
        let mut changed = kicked || log != w.log || now >= w.rendered + 15;
        w.log = log;
        if now >= w.herdr.0 + 2 {
            let out = herdr::agent_list(self.r);
            changed |= hash(&out) != w.herdr.1;
            w.herdr = (now, hash(&out));
            lock(&self.cache).insert("herdr agent list".into(), (now, out));
        }
        // #37: Herd's data rides the same stream, re-read every 10 s (host weather) while anyone listens;
        // like a frag, pushed only on a change (the page reads `/herd.json` when Herd opens).
        if (changed || now >= w.herd.0 + 10) && !lock(&self.clients).is_empty() {
            let j = self
                .herd(
                    &Ctx {
                        now,
                        ..self.ctx.clone()
                    },
                    None,
                )
                .map(|v| v.to_string())
                .unwrap_or_default();
            if w.herd.0 > 0 && hash(&j) != w.herd.1 && !j.is_empty() {
                lock(&self.clients).retain(|c| c.0.send(format!("event: herd\ndata: {j}\n\n")).is_ok());
            }
            w.herd = (now, hash(&j));
        }
        if !changed {
            return;
        }
        w.rendered = now;
        for paired in [false, true] {
            for (id, html) in self
                .frags(
                    &Ctx {
                        now,
                        ..self.ctx.clone()
                    },
                    false,
                    paired,
                )
                .unwrap_or_default()
            {
                if w.frags
                    .insert((paired, id), hash(&html))
                    .is_some_and(|h| h != hash(&html))
                {
                    let msg = format!("event: frag\ndata: {}\n\n", serde_json::json!({"id": id, "html": html}));
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
        Memo(self.r, &self.cache, now, if self.fresh { ttl / 3 } else { ttl })
    }

    /// The full HTTP response to one request.
    pub fn respond(&self, req: &str, now: u64) -> Vec<u8> {
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
        let frags = path == "/frags" || path == "/" && query == "frag=all"; // `?frag=all`: old cached JS, one release
        if method == "GET" && nav && (frags || ["/herd.json", "/lane", "/frag"].contains(&path)) {
            return see("/");
        }
        // Host/Origin-checked: the live stream, transcripts, Herd's data and every write.
        let guarded = method == "GET" && ["/live", "/lane", "/herd.json"].contains(&path)
            || method == "POST" && self.addr.is_some();
        let paired_op = form(body.trim())
            .get("op")
            .is_some_and(|o| PAIRED_OPS.contains(&o.as_str()));
        let (status, ctype, body) = match (method, path) {
            ("GET", _) if frags => self.frags(ctx, false, self.device(ctx, head).is_some()).map_or_else(fail, |f| {
                ("200 OK", JSON, serde_json::to_vec(&f.into_iter().collect::<BTreeMap<_, _>>()).unwrap_or_default())
            }),
            ("POST", _) if self.addr.is_none() => ("405 Method Not Allowed", "text/plain", text("read-only: GET only\n")),
            _ if guarded && !self.allowed(ctx, head) => ("403 Forbidden", "text/plain", text("forbidden: foreign Host or Origin\n")),
            ("GET", "/live") => ("503 Service Unavailable", "text/plain", text("8 live clients already: the page polls instead\n")),
            ("GET", "/") => self.page(ctx, &form(query), self.device(ctx, head).is_some())
                .map_or_else(fail, |b| ("200 OK", HTML, b.into_bytes())),
            ("GET", "/frag") => self.part(ctx, &form(query))
                .map_or(("404 Not Found", "text/plain", text("no such part\n")), |h| ("200 OK", HTML, h.into_bytes())),
            ("GET", "/lane") if self.device(ctx, head).is_none() => ("401 Unauthorized", "text/plain", text("pair this device to read transcripts\n")),
            ("GET", "/lane") => self.transcript(ctx, &form(query)).map_or(
                ("404 Not Found", "text/plain", text("no such agent pane\n")),
                |j| ("200 OK", JSON, j.into_bytes()),
            ),
            ("GET", "/herd.json") => {
                let loopback = header(head, "host").is_some_and(|h| ["127.0.0.1", "localhost", "[::1]"].iter().any(|l| names(l, h)));
                let weather = form(query).get("weather").filter(|_| loopback).cloned();
                self.herd(ctx, weather.as_deref()).map_or_else(fail, |j| ("200 OK", JSON, j.to_string().into_bytes()))
            }
            ("GET", "/sw.js") => ("200 OK", "text/javascript", text(&SW.replace("@V", &format!("{VER:08x}")))),
            ("GET", p) => {
                let a = ASSETS.iter().find(|a| p == format!("/a/{VER:08x}/{}", a.0)).inspect(|_| cache = "public, max-age=31536000, immutable");
                a.or_else(|| FILES.iter().find(|f| p.strip_prefix('/') == Some(f.0)).inspect(|_| cache = "public, max-age=86400")).map_or(("404 Not Found", "text/plain", text("not found\n")), |f| ("200 OK", f.1, f.2.to_vec()))
            }
            ("POST", "/") if paired_op && self.device(ctx, head).is_none() => {
                ("401 Unauthorized", "text/plain", text("only a paired device may unpair, answer or drive\n"))
            }
            ("POST", "/") => {
                let msg = self.write(ctx, &form(body.trim())).unwrap_or_else(|e| shown(&e));
                return see(&format!("/?msg={}", enc(&msg)));
            }
            // #59: the request's `hd=` token (10 min) works once `serve approve` clears its code; `/pair/claim` then keeps it a year.
            ("POST", "/pair/request") => match devices::request(ctx, &kind(head), header(head, "x-peer").unwrap_or_default()) {
                Ok(Some((code, t))) => {
                    extra = format!("Set-Cookie: hd={t}; Path=/; Max-Age=600; HttpOnly; Secure; SameSite=Strict\r\n");
                    let hint = Config::load(&ctx.config).ok().and_then(|c| c.serve.approve_hint).unwrap_or_default();
                    ("200 OK", "text/plain", format!("{code}\n{}\n", hint.replace("{code}", &code)).into_bytes())
                }
                Ok(None) => ("429 Too Many Requests", "text/plain", text("3 pairing requests open (2 per address), or a code clash: approve one, tap Pair again or wait 10 min\n")),
                Err(e) => fail(e),
            },
            ("POST", "/pair/claim") => match self.device(ctx, head) {
                Some(d) => (extra = format!("Set-Cookie: {}; Path=/; Max-Age=31536000; HttpOnly; Secure; SameSite=Strict\r\n", header(head, "cookie").unwrap_or_default().split(';').map(str::trim).find(|c| c.starts_with("hd=")).unwrap_or_default()), ("200 OK", "text/plain", format!("paired as {}\n", d.label).into_bytes())).1,
                None => ("403 Forbidden", "text/plain", text("not approved yet: `herdsman serve approve <code>` on the Mac\n")),
            },
            ("POST", "/push/subscribe" | "/push/unsubscribe") if self.device(ctx, head).is_none() => ("401 Unauthorized", "text/plain", text("pair this device first\n")),
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
            ("POST", p @ ("/send" | "/answer" | "/act")) => {
                let (status, msg) = self.send(ctx, head, &form(body.trim()), p);
                (status, "text/plain", format!("{msg}\n").into_bytes())
            }
            ("POST", _) => ("404 Not Found", "text/plain", text("not found\n")),
            _ => ("405 Method Not Allowed", "text/plain", text("GET or POST only\n")),
        };
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

    /// The paired device whose token the request's `Cookie` carries.
    #[allow(clippy::unused_self)] // a method so every route reads `self.device(..)` alike
    fn device(&self, ctx: &Ctx, head: &str) -> Option<devices::Device> {
        devices::find(ctx, header(head, "cookie").unwrap_or_default())
    }

    /// The reply box (or a blocked menu's option, or a sheet action): a paired device (cookie) prompts one agent, at most 10 sends a minute.
    fn send(&self, ctx: &Ctx, head: &str, f: &BTreeMap<String, String>, path: &str) -> (&'static str, String) {
        let Some(dev) = self.device(ctx, head) else {
            return ("401 Unauthorized", "pair this device: tap Pair this phone".into());
        };
        let mut sent = self.sent.borrow_mut();
        let times = sent.entry(dev.hash.clone()).or_default();
        times.retain(|t| ctx.now < t + 60);
        if times.len() >= 10 {
            return ("429 Too Many Requests", "10 sends a minute: wait a moment".into());
        }
        times.push(ctx.now);
        let (pick, act) = (path == "/answer", path == "/act");
        let res = if pick {
            self.pick(ctx, &dev, f)
        } else if act {
            self.act(ctx, &dev, f)
        } else {
            self.prompt(ctx, &dev, f)
        };
        if pick || act {
            self.purge();
            self.kick.store(true, Relaxed);
        }
        res.map_or_else(|e| ("409 Conflict", shown(&e)), |msg| ("200 OK", msg))
    }

    /// The live fleet lane `agent` with its fleet and pane; refused PAUSED when its fleet is.
    fn lane(&self, ctx: &Ctx, agent: &str) -> Result<(lanes::Lane, String, String)> {
        let (cfg, events) = (Config::load(&ctx.config)?, read(&ctx.state)?);
        let all = self
            .lanes(ctx.now, &cfg, &events, &fold(&events))
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

    /// A blocked Claude menu's option: its digit via `pane send-keys`, then a re-read ("sent" or
    /// "still waiting"); logs an `answer` event without the question. No menu parsed, or not the
    /// menu the tapped card showed (`h`, read afresh): no key.
    fn pick(&self, ctx: &Ctx, dev: &devices::Device, f: &BTreeMap<String, String>) -> Result<String> {
        let (agent, choice) = (
            f.get("agent").map_or("", String::as_str),
            f.get("choice").map_or("", String::as_str),
        );
        let (l, fleet, pane) = self.lane(ctx, agent)?;
        let screen = || herdr::read(self.r, &pane);
        let Some((q, opts)) = lanes::pending(&screen()?).filter(|p| l.harness == "claude" && !p.1.is_empty()) else {
            return refused(
                "BAD_VALUE",
                format!("no menu on {agent}'s pane: menu changed — refreshed; answer with the reply box"),
            );
        };
        if l.role == "rev" && !q.starts_with("Do you want") {
            return refused("REV_READONLY", "reviewer lanes answer permission prompts only");
        }
        if !choice
            .parse()
            .is_ok_and(|n: usize| (1..=opts.len().min(9)).contains(&n))
            || choice.len() != 1
        {
            return refused("BAD_VALUE", format!("choose 1 to {}", opts.len()));
        }
        if f.get("h") != Some(&seen(&q, &opts).to_string()) {
            return refused("STALE", "menu changed — refreshed");
        }
        herdr::keys(self.r, &pane, choice)?;
        let d = serde_json::json!({"via": "web", "agent": agent, "device": dev.label, "choice": choice});
        append(&ctx.state, &Event::new(ctx.now, &fleet, l.issue, "answer", d))?;
        self.r.sleep(1);
        Ok(if lanes::pending(&screen()?) == Some((q, opts)) {
            format!("still waiting: {agent}")
        } else {
            format!("sent to {agent}")
        })
    }

    /// `herdr agent prompt` (dsh: `pane send-text`) + Enter; logs a `prompt` event without the text.
    fn prompt(&self, ctx: &Ctx, dev: &devices::Device, f: &BTreeMap<String, String>) -> Result<String> {
        let g = |k: &str| f.get(k).map_or("", String::as_str);
        let (agent, text) = (g("agent"), g("text"));
        let (l, fleet, pane) = self.lane(ctx, agent)?;
        let (fleet, pane) = (&fleet, &pane);
        if l.role == "rev" {
            return refused("REV_READONLY", "reviewer lanes are not steered mid-review");
        }
        let n = text.chars().count();
        if text.trim().is_empty() || n > 2000 {
            return refused("BAD_VALUE", format!("a message is 1 to 2000 chars, not {n}"));
        }
        if text.chars().any(|c| (c < ' ' && c != '\n') || c == '\x7f') {
            return refused("BAD_VALUE", "a message has no control characters (newlines are fine)");
        }
        herdr::reply(self.r, l.harness == "dsh", agent, pane, text)?;
        let d = serde_json::json!({"via": "web", "agent": agent, "device": dev.label, "chars": n});
        append(&ctx.state, &Event::new(ctx.now, fleet, l.issue, "prompt", d))?;
        Ok(format!("sent to {agent}"))
    }

    /// A sheet action (`herdsman stop|kill|clean --lane`; kill and clean dry-run without `confirm=1`); logs `act` unless a dry run.
    fn act(&self, ctx: &Ctx, dev: &devices::Device, f: &BTreeMap<String, String>) -> Result<String> {
        let (g, mut out, cfg) = (
            |k: &str| f.get(k).map_or("", String::as_str),
            vec![],
            Config::load(&ctx.config)?,
        );
        let (agent, yes) = (g("agent"), g("confirm") == "1");
        let sp = crate::events::last_spawn(&read(&ctx.state)?, agent).map(|e| (e.fleet.clone(), e.issue));
        let Some((fleet, issue)) = sp else {
            return refused("NO_SUCH_AGENT", format!("no lane `{agent}` in the log"));
        };
        match g("op") {
            "stop" => crate::check::stop(ctx, &cfg, self.r, &mut out, agent)?,
            "kill" => crate::check::kill(ctx, &cfg, self.r, &mut out, agent, yes)?,
            "clean" => crate::check::clean(ctx, &cfg, self.r, &mut out, Some(&fleet), (None, Some(agent)), yes)?,
            op => return refused("BAD_VALUE", format!("op is stop, kill or clean, not `{op}`")),
        }
        if yes || g("op") == "stop" {
            let d = serde_json::json!({"via": "web", "agent": agent, "device": dev.label, "op": g("op")});
            append(&ctx.state, &Event::new(ctx.now, &fleet, issue, "act", d))?;
        }
        Ok(String::from_utf8_lossy(&out).trim().replace('\n', "; "))
    }

    /// The sheet's transcript: up to 200 masked lines `before` the newest (load more), grouped per turn, as JSON.
    fn transcript(&self, ctx: &Ctx, q: &BTreeMap<String, String>) -> Option<String> {
        let n = |k: &str, d: usize| q.get(k).and_then(|v| v.parse().ok()).unwrap_or(d);
        let (lines, before) = (n("lines", 200).min(200), n("before", 0).min(2000));
        let (cfg, events) = (Config::load(&ctx.config).ok()?, read(&ctx.state).ok()?);
        let l = self
            .lanes(ctx.now, &cfg, &events, &fold(&events))?
            .into_iter()
            .find(|l| Some(&l.name) == q.get("agent"))?;
        let want = (before + lines).to_string();
        let screen = herdr::recent(&self.cached(ctx.now, 5), l.pane.as_deref()?, &want).ok()?;
        let all = lanes::tail(&lanes::chat(&screen).0.join("\n"), before + lines);
        let page = &all[..all.len().saturating_sub(before)];
        // `recent` is herdr's whole window: a short read (or the 2000 clamp) means nothing older.
        let more = all.len() == before + lines && before + lines <= 2000;
        Some(serde_json::json!({"harness": l.harness, "turns": lanes::turns(page), "before": before + page.len(), "more": more}).to_string())
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
                format!(
                    "moved #{issue} {}",
                    n("before").map_or_else(|| format!("after #{}", g("after")), |b| format!("before #{b}"))
                )
            }
            op @ ("pause" | "resume") => {
                crate::pause(ctx, &mut out, fleet, op == "pause", Some("web"))?;
                format!("{fleet} {op}d")
            }
            "set" => {
                let wanted: Vec<_> = ["model", "effort"]
                    .into_iter()
                    .filter(|k| !g(k).is_empty())
                    .map(|k| (k, g(k).to_string()))
                    .collect();
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
                crate::drive::drive(ctx, &Config::load(&ctx.config)?, self.r, &mut out, Some(fleet))?;
                let line = String::from_utf8_lossy(&out).lines().collect::<Vec<_>>().join("; ");
                if line.is_empty() {
                    format!("{fleet}: nothing due")
                } else {
                    line
                }
            }
            "unpair" => {
                let mut all = devices::load(ctx);
                all.retain(|d| d.hash != g("hash"));
                devices::store(ctx, &all)?;
                "device removed".into()
            }
            op => bail!("unknown op `{op}`"),
        })
    }

    /// The live sections' inner HTML by element id (the page, `/?frag=all`, `/live`); pane text only when `paired`.
    fn frags(&self, ctx: &Ctx, all: bool, paired: bool) -> Result<Vec<(&'static str, String)>> {
        let events = read(&ctx.state)?;
        let (cfg, st) = (Config::load(&ctx.config).ok(), fold(&events));
        Ok(vec![
            ("needs", self.needs_you(ctx, cfg.as_ref(), &events, &st, paired)),
            ("queue", self.queue_tab(ctx, cfg.as_ref(), &events, &st)),
            (
                "lanes",
                "<p class=lh><button id=mode type=button aria-label=\"lane display mode\">horse</button></p>"
                    .to_string()
                    + &self.lanes_tab(ctx, cfg.as_ref(), &events, &st, paired),
            ),
            ("host", self.host_tab(ctx, cfg.as_ref(), &events)),
            ("events", events_tab(ctx, cfg.as_ref(), &events, &st, all)),
        ])
    }

    /// #60: a part the page loads on demand: a fleet's role forms (`id=roles&fleet=`), the Events tab (`id=events`).
    fn part(&self, ctx: &Ctx, q: &BTreeMap<String, String>) -> Option<String> {
        let (events, cfg) = (read(&ctx.state).ok()?, Config::load(&ctx.config).ok());
        match q.get("id")?.as_str() {
            "events" => Some(events_tab(ctx, cfg.as_ref(), &events, &fold(&events), false)),
            "roles" => {
                let (c, f) = (cfg?, q.get("fleet")?);
                Some(roles(&c, f, c.fleet.get(f)?, self.addr.is_some(), &mtime(ctx)))
            }
            _ => None,
        }
    }

    fn page(&self, ctx: &Ctx, q: &BTreeMap<String, String>, paired: bool) -> Result<String> {
        let all = q.get("all").is_some_and(|v| v == "1");
        let mut f = self.frags(ctx, all, paired)?;
        // #60: the Events tab body loads when the tab opens (`/frag?id=events`), unless `?all=1` asked for it.
        if self.export.is_none() && !all {
            f[4].1.clear();
        }
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
        let msg = if demo {
            "<p class=\"msg a\">Demo with fake data. Install herdsman to run your own.</p>".to_string()
        } else {
            msg
        };
        let badge = if demo {
            "<span class=tag id=demo>demo</span>"
        } else {
            ""
        };
        Ok(format!(
            "<!doctype html><html lang=en><head><meta charset=utf-8>\
             <meta name=viewport content=\"width=device-width,initial-scale=1,viewport-fit=cover\">\
             <meta name=theme-color content=\"#1f2a38\"><meta name=apple-mobile-web-app-capable content=yes>\
             <link rel=manifest href={root}manifest.webmanifest><link rel=icon href={root}icon-192.png><link rel=apple-touch-icon href={root}icon-180.png>\
             <title>herdsman</title><link rel=stylesheet href={root}a/{VER:08x}/serve.css><link rel=stylesheet href={root}a/{VER:08x}/herd.css></head>\
             <body data-ts={} data-mode=horse{ro}><header><b>herdsman</b>{badge}<span id=stale hidden></span><i id=dot class=a></i>\
             <button id=rf type=button aria-label=refresh>↻</button><button id=herd-on type=button aria-label=\"herd mode\">🐎</button>{}</header><i id=ptr hidden>↻</i>{msg}<div id=needs>{}</div><main>\
             <section id=queue>{}</section><section id=lanes>{}</section><section id=host>{}</section>\
             <section id=events>{}</section></main><nav><a href=#queue>Queue</a><a href=#lanes>Lanes</a>\
             <a href=#host>Host</a><a href=#events>Events</a></nav><script src={root}a/{VER:08x}/serve.js></script><script src={root}a/{VER:08x}/herd.js></script></body></html>\n",
            ctx.now, if paired { "" } else { self.pairb() }, f[0].1, f[1].1, f[2].1, f[3].1, f[4].1
        ))
    }

    /// Each open ask as a red card above every tab: options (recommended first) and a free-text box;
    /// then each blocked lane's pending prompt.
    fn needs_you(&self, ctx: &Ctx, cfg: Option<&Config>, events: &[Event], st: &States, paired: bool) -> String {
        let all = cfg.and_then(|c| self.lanes(ctx.now, c, events, st)).unwrap_or_default();
        let blocked: String = all
            .iter()
            .filter(|l| l.state == "blocked")
            .map(|l| self.blocked(ctx, l, paired))
            .collect();
        crate::events::open_asks(events).into_iter().map(|a| {
            let s = |k| a.str(k).unwrap_or_default();
            let mut opts: Vec<(u64, &str)> = a.data["options"].as_array().into_iter().flatten().zip(1..).map(|(o, n)| (n, o.as_str().unwrap_or("?"))).collect();
            opts.sort_by_key(|o| Some(o.0) != a.data["recommend"].as_u64());
            let rec = |n: &u64| if Some(*n) == a.data["recommend"].as_u64() { " <small>(recommended)</small>" } else { "" };
            let btns: String = opts.iter().map(|(n, o)| format!("<button name=choice value={n}>{n}. {}{}</button>", esc(o), rec(n))).collect();
            let form = format!("<form method=post>{}<input type=hidden name=id value=\"{}\"><input name=text aria-label=answer placeholder=\"or type an answer\"><button>Send</button>{btns}</form>", hidden("answer", &a.fleet, &mtime(ctx)), esc(s("id")));
            let who = join(&[&a.fleet, &a.issue.map_or(String::new(), |n| format!("#{n}")), s("agent"), &age(ctx.now.saturating_sub(a.ts))]);
            format!("<div class=ask><p><b>needs you</b> <span class=m>{}</span></p><p>{}</p>{}</div>", esc(&who), esc(s("question")), if self.addr.is_some() { form } else { String::new() })
        }).collect::<String>() + &blocked
    }

    /// A blocked lane's card: a Claude menu's options as buttons (`/answer`), else the question
    /// (or the last 6 masked lines) and a Reply button that opens the lane's sheet. Unpaired: no
    /// pane text, only the 1-line activity and a pairing hint.
    fn blocked(&self, ctx: &Ctx, l: &lanes::Lane, paired: bool) -> String {
        let screen = l
            .pane
            .as_ref()
            .and_then(|p| herdr::read(&self.cached(ctx.now, 2), p).ok())
            .unwrap_or_default();
        let rw = self.addr.is_some() && paired;
        let body = match lanes::pending(&screen).filter(|_| l.harness == "claude") {
            _ if !paired => format!(
                "<p class=m>{}</p><p class=m>pair this phone to answer{}</p>",
                esc(&lanes::activity(&l.harness, &screen)),
                self.pairb()
            ),
            Some((q, opts)) => {
                format!("<p>{}</p>", esc(&lanes::mask(&q)))
                    + &opts
                        .iter()
                        .zip(1..)
                        .filter(|_| rw)
                        .map(|(o, n)| {
                            format!(
                                "<button type=button class=pick value={n} data-h={}>{n}. {}</button>",
                                seen(&q, &opts),
                                esc(&lanes::mask(o))
                            )
                        })
                        .collect::<String>()
            }
            None => format!("<pre>{}</pre>", esc(&lanes::tail(&screen, 6).join("\n"))),
        };
        let who = join(&[
            l.fleet.as_deref().unwrap_or_default(),
            &l.issue.map_or(String::new(), |n| format!("#{n}")),
            &l.name,
        ]);
        let reply = if rw {
            "<button type=button class=rp>Reply</button><p class=as></p>"
        } else {
            ""
        };
        format!(
            "<div class=ask data-ask=\"{}\"><p><b>needs you</b> <span class=m>{}</span></p>{body}{reply}</div>",
            esc(&l.name),
            esc(&who)
        )
    }

    /// `herdr agent|workspace list` JSON, read at most every 10 s (None when herdr cannot be read).
    fn herdr_json(&self, now: u64, what: &str) -> Option<serde_json::Value> {
        herdr::list(&self.cached(now, 10), what).ok()
    }

    /// Per configured fleet: open work in queue order, other open issues, `done today`; paused = one line.
    fn queue_tab(&self, ctx: &Ctx, cfg: Option<&Config>, events: &[Event], st: &States) -> String {
        let (now, mt, rw) = (ctx.now, mtime(ctx), self.addr.is_some());
        let Some(cfg) = cfg else {
            return "<p class=r>cannot read fleet.toml</p>".into();
        };
        // The state of an open lane herdr shows no pane for: `dead`, or `parked` (#83).
        let ls = self.lanes(now, cfg, events, st).unwrap_or_default();
        let gone = |s: &IssueState| {
            s.active_lane()
                .and_then(|x| ls.iter().find(|l| l.name == x.1 && l.pane.is_none()))
        };
        let mut h = String::new();
        for (f, fc) in &cfg.fleet {
            let q = queue::view(events, st, f);
            let mine: Vec<&IssueState> = st.values().filter(|s| s.fleet == *f).collect();
            let queued = |n: u64| q.iter().any(|i| i.issue == n);
            let rest: Vec<u64> = mine
                .iter()
                .filter(|s| !queued(s.issue) && s.state != "merged" && s.state != "queued")
                .map(|s| s.issue)
                .collect();
            let items: Vec<(u64, Option<u64>, Option<usize>)> = q
                .iter()
                .enumerate()
                .map(|(k, i)| (i.issue, i.blocked_by, Some(k)))
                .chain(rest.iter().map(|&n| (n, None, None)))
                .collect();
            let mut body = String::new();
            for &(n, blocked_by, k) in &items {
                let s = st.get(&(f.clone(), n));
                let waiting = crate::events::open_asks(events)
                    .iter()
                    .any(|e| e.fleet == *f && e.issue == Some(n));
                let logged = s.map_or("queued", |s| s.state.as_str());
                let shown = if waiting { "⏸ waiting: decision" } else { logged };
                let state = s.and_then(gone).map_or(shown, |l| l.state.as_str());
                let is_dead = state == "dead";
                let a = ago(now, s.map_or(0, |s| s.last_ts));
                let lane = s.and_then(|s| s.lane.as_deref()).unwrap_or_default();
                let mut card = vec![
                    esc(&join(&[&format!("#{n} {logged}"), lane, &a])),
                    pr_line(ctx, Some(fc), f, n, s),
                ];
                if is_dead {
                    card.push("<span class=r>dead: the log has this lane open, herdr has no such agent</span>".into());
                }
                if let Some(b) = blocked_by {
                    card.push(format!("<span class=r>blocked by #{b}</span>"));
                }
                if let (true, false, Some(k)) = (rw, is_dead, k) {
                    let mv = |label: &str, key: &str, to: u64| {
                        format!("<form method=post>{}<input type=hidden name=issue value={n}><input type=hidden name={key} value={to}><button>{label}</button></form>", hidden("mv", f, &mt))
                    };
                    let up = if k > 0 {
                        mv("▲ up", "before", q[k - 1].issue)
                    } else {
                        String::new()
                    };
                    let down = q.get(k + 1).map(|x| mv("▼ down", "after", x.issue)).unwrap_or_default();
                    card.push(format!("<span class=btns>{up}{down}</span>"));
                }
                let row = row("q", "", &format!("#{n}"), state, &a, &card);
                body += &match titles(ctx, f, n) {
                    Some(t) => row.replacen("</summary>", &format!("<span class=tl>{}</span></summary>", esc(&t)), 1),
                    None => row,
                };
            }
            if items.is_empty() {
                body += "<p class=m>no open work</p>";
            }
            let mut done: Vec<&&IssueState> = mine
                .iter()
                .filter(|s| s.state == "merged" && now.saturating_sub(s.last_ts) < 86_400)
                .collect();
            done.sort_by_key(|s| std::cmp::Reverse(s.last_ts));
            if !done.is_empty() {
                let rows: String = done
                    .iter()
                    .take(5)
                    .map(|s| {
                        let pr = events
                            .iter()
                            .rev()
                            .find(|e| e.is("merged", f, s.issue))
                            .and_then(|e| e.str("pr"));
                        let link = pr.map_or(String::new(), |p| {
                            format!(
                                " <a href=\"https://github.com/{}/pull/{}\">PR #{}</a>",
                                esc(&fc.repo),
                                esc(p),
                                esc(p)
                            )
                        });
                        format!(
                            "<p>#{} <span class=g>merged</span> {}{link}</p>",
                            s.issue,
                            age(now.saturating_sub(s.last_ts))
                        )
                    })
                    .collect();
                body += &format!(
                    "<details class=done><summary><span>done today: {}</span></summary>{rows}</details>",
                    done.len()
                );
            }
            // #60: the live page loads a fleet's role forms when the row is opened (`/frag`); an export inlines them.
            body += &if self.export.is_some() {
                format!(
                    "<details class=roles><summary><span>roles</span></summary>{}</details>",
                    roles(cfg, f, fc, rw, &mt)
                )
            } else {
                let src = format!("frag?id=roles&amp;fleet={}", enc(f));
                format!("<details class=roles data-src=\"{src}\"><summary><span>roles</span></summary></details>")
            };
            let button = |op: &str, label: &str| {
                if rw {
                    format!(
                        "<form method=post>{}<button>{label}</button></form>",
                        hidden(op, f, &mt)
                    )
                } else {
                    String::new()
                }
            };
            if fc.paused {
                let lanes: Vec<&&IssueState> = mine.iter().filter(|s| s.active_lane().is_some()).collect();
                let nd = lanes
                    .iter()
                    .filter(|s| gone(s).is_some_and(|l| l.state == "dead"))
                    .count();
                let what = match (lanes.len(), nd) {
                    (0, _) => "no lane".to_string(),
                    (n, d) if d == n => format!("{n} lane{} (dead)", if n == 1 { "" } else { "s" }),
                    (n, 0) => format!("{n} lane{}", if n == 1 { "" } else { "s" }),
                    (n, d) => format!("{n} lanes ({d} dead)"),
                };
                h += &format!(
                    "<details class=paused><summary><span>{} · <span class=r>paused</span> · {what}</span></summary>{}{body}</details>",
                    esc(f),
                    button("resume", "Resume")
                );
                continue;
            }
            let drive = if fc.driven() {
                button("drive", "Drive now")
            } else {
                String::new()
            };
            h += &format!("<h2><span>{}</span>{drive}{}</h2>", esc(f), button("pause", "Pause"));
            if fc.driven() {
                let last = crate::drive::last_pass(ctx, f).map(|t| age(now.saturating_sub(t)));
                let last = last.map_or("no pass yet".into(), |a| format!("last pass {a} ago"));
                h += &format!("<p class=m>driver: herdsman · {last}</p>");
            }
            let stall = cfg.limits.stall_minutes * 60;
            let live = mine.iter().filter(|s| s.active_lane().is_some() && gone(s).is_none());
            let next = live.map(|s| ((s.last_ts + stall).saturating_sub(now), s.issue)).min();
            h += &match next {
                Some((0, n)) => format!("<p class=m>stall check: #{n} is due now</p>"),
                Some((left, n)) => format!("<p class=m>stall check: #{n} in {}</p>", age(left)),
                None => "<p class=m>no live lane</p>".into(),
            };
            h += &body;
        }
        h
    }

    /// herdr's agents joined with the log (None when herdr cannot be read).
    fn lanes(&self, now: u64, cfg: &Config, events: &[Event], st: &States) -> Option<Vec<lanes::Lane>> {
        let list = self.herdr_json(now, "agent")?;
        let agents = list["result"]["agents"].as_array().cloned().unwrap_or_default();
        let wsl = self.herdr_json(now, "workspace").unwrap_or_default();
        let ws: BTreeMap<String, String> = wsl["result"]["workspaces"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|w| Some((w["workspace_id"].as_str()?.into(), w["label"].as_str()?.into())))
            .collect();
        Some(lanes::join(
            cfg,
            &self.cached(now, 30),
            &self.ctx.worktrees,
            events,
            st,
            &agents,
            &ws,
        ))
    }

    fn waits(&self, ctx: &Ctx, cfg: &Config, events: &[Event], all: &[lanes::Lane]) -> Vec<crate::waiting::Waiting> {
        let live: Vec<_> = all
            .iter()
            .map(|l| json!({"name": l.name, "agent_status": l.state}))
            .collect();
        crate::waiting::scan(ctx, cfg, &self.cached(ctx.now, 30), events, &live, false)
            .into_iter()
            .map(|x| x.1)
            .collect()
    }

    /// #37: Herd mode's data (`/herd.json`, the `herd` event; docs/serve.md).
    /// `weather` (loopback only) forces the host values.
    fn herd(&self, ctx: &Ctx, weather: Option<&str>) -> Result<serde_json::Value> {
        let events = read(&ctx.state)?;
        let (cfg, st) = (Config::load(&ctx.config)?, fold(&events));
        let all = self.lanes(ctx.now, &cfg, &events, &st).unwrap_or_default();
        let waits = self.waits(ctx, &cfg, &events, &all);
        let lanes = |f: Option<&String>| -> Vec<_> {
            all.iter().filter(|l| l.fleet.as_ref() == f).map(|l| {
                let (title, tone, shown, a) = view(ctx, l, &waits);
                json!({"name": l.name, "issue": l.issue, "title": title, "role": l.role, "harness": l.harness,
                       "model": l.model, "effort": l.effort, "account": l.account, "state": l.state, "shown": shown, "tone": tone, "age": a, "coat": coat(&l.name)})
            }).collect()
        };
        let queued = |f: &String| -> Vec<_> {
            let waiting = queue::view(&events, &st, f)
                .into_iter()
                .filter(|i| st.get(&(f.clone(), i.issue)).is_none_or(|s| s.state == "queued"));
            waiting.map(|i| json!({"issue": i.issue, "title": gh::cached_title(ctx, f, i.issue), "blocked_by": i.blocked_by, "coat": coat(&format!("{f}#{}", i.issue))})).collect()
        };
        let fleets: Vec<_> = cfg
            .fleet
            .keys()
            .map(|f| json!({"name": f, "lanes": lanes(Some(f)), "queue": queued(f)}))
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
            },
        );
        let host = json!({"load": h.load_per_cpu, "limit": lim.load_per_cpu, "pressure": h.pressure, "swapouts": h.swapout, "swap_limit": lim.swapout_rate, "gate": host::admit(&h, lim).err()});
        Ok(json!({"fleets": fleets, "others": lanes(None), "host": host}))
    }

    /// State sections, fleet subgroups inside; a row opens the bottom sheet (serve.js) with its hidden card.
    fn lanes_tab(&self, ctx: &Ctx, cfg: Option<&Config>, events: &[Event], st: &States, paired: bool) -> String {
        let Some(cfg) = cfg else {
            return "<p class=m>no fleet.toml</p>".into();
        };
        let Some(all) = self.lanes(ctx.now, cfg, events, st) else {
            return "<p class=r>herdr agent list failed</p>".into();
        };
        let state = |l: &lanes::Lane| l.issue.zip(l.fleet.as_ref()).and_then(|(n, f)| st.get(&(f.clone(), n)));
        let waits = self.waits(ctx, cfg, events, &all);
        let mut h = String::new();
        for (sec, subs) in lanes::group(&all, |l| state(l).map_or(l.since, |s| s.last_ts.max(l.since))) {
            h += &format!(
                "<h2><span class={}>{sec} ({})</span></h2>",
                tone(sec),
                subs.iter().map(|s| s.1.len()).sum::<usize>()
            );
            for (repo, ls) in subs {
                h += &format!("<h3>{} ({})</h3>", esc(&repo), ls.len());
                for l in ls {
                    h += &self.lane_row(ctx, cfg, l, state(l), paired, &waits);
                }
            }
        }
        if h.is_empty() {
            "<p class=m>no agents</p>".into()
        } else {
            h
        }
    }

    fn lane_row(
        &self,
        ctx: &Ctx,
        cfg: &Config,
        l: &lanes::Lane,
        s: Option<&IssueState>,
        paired: bool,
        waits: &[crate::waiting::Waiting],
    ) -> String {
        let screen = l
            .pane
            .as_ref()
            .and_then(|p| herdr::read(&self.cached(ctx.now, 30), p).ok());
        let (title, tone, shown, a) = view(ctx, l, waits);
        let issue = l.issue.map(|n| format!("#{n}")).unwrap_or_default();
        let round = s.map(|s| {
            format!(
                "round {}",
                s.state
                    .split_once('(')
                    .map_or(s.rounds.to_string(), |r| r.1.trim_end_matches(')').into())
            )
        });
        let o = |v: &Option<String>| v.clone().unwrap_or_default();
        let facts = [
            l.role.clone(),
            o(&round),
            l.harness.clone(),
            o(&l.model),
            o(&l.effort),
            o(&l.account),
            a.clone(),
        ];
        let mut card = vec![
            esc(format!("{issue} {}", o(&title)).trim()),
            esc(&join(&facts.iter().map(String::as_str).collect::<Vec<_>>())),
        ];
        if let (Some(n), Some(f)) = (l.issue, &l.fleet) {
            card.push(pr_line(ctx, cfg.fleet.get(f), f, n, s));
        }
        if l.state == "dead" {
            card.push("<span class=r>⚠ log/pane mismatch: the log has this lane open, herdr has no pane</span>".into());
        }
        card.insert(1, esc(&shown));
        let card = self::card(&card)
            + &if l.state == "blocked" {
                self.blocked(ctx, l, paired)
            } else {
                String::new()
            };
        let act = esc(&lanes::activity(&l.harness, &screen.unwrap_or_default()));
        // #53: line 1 name (one line) + `state age`; line 2 the reason (no age), or `superseded by` on an issue's older lane.
        let w = wait_of(l, waits);
        let sup = s.and_then(|s| s.lane.as_ref()).filter(|x| **x != l.name && w.is_some());
        let why = match (sup, &w) {
            (Some(x), _) => format!("<span class=\"why m\">superseded by {}</span>", esc(x)),
            (None, Some(w)) => format!("<span class=\"why {tone}\">{}</span>", esc(&w.what)),
            _ => String::new(),
        };
        let tone = if sup.is_some() { "m" } else { tone };
        let since = w.filter(|_| sup.is_none()).map_or(l.since, |w| w.since);
        let a = ago(ctx.now, since);
        format!(
            "<div class=lane data-agent=\"{}\" data-role={}><button type=button class=lr>{}<span class=\"nw el\">{}</span><span class=\"nw {}\">{} {a}</span>\
             {why}<span class=act>{act}</span></button><div class=card hidden>{card}</div></div>",
            esc(&l.name),
            esc(&l.role) + if l.pane.is_none() { " data-gone" } else { "" },
            avatar(&l.name, &l.state),
            esc(&l.name),
            tone,
            esc(&l.state)
        )
    }

    fn sample(&self) -> Host {
        lock(&self.host).clone()
    }

    fn host_tab(&self, ctx: &Ctx, cfg: Option<&Config>, events: &[Event]) -> String {
        let hs = self.sample();
        let limits = cfg.map(|c| &c.limits);
        let lim =
            |f: &dyn Fn(&crate::config::Limits) -> f64| limits.map_or(String::new(), |l| format!(" (limit {})", f(l)));
        let v = |x: Option<f64>, p: usize| x.map_or("?".into(), |x| format!("{x:.p$}"));
        let gate = limits.map(|l| host::admit(&hs, l));
        let mut h = format!(
            "<h2>Host</h2><p class=kv>load/cpu <b>{}</b>{}</p><p class=kv>memory pressure <b class={}>{}</b></p>\
             <p class=kv>swap-out <b>{}</b> pages/2s{}</p>",
            v(hs.load_per_cpu, 2),
            lim(&|l| l.load_per_cpu),
            tone(hs.pressure.as_deref().unwrap_or("?")),
            esc(hs.pressure.as_deref().unwrap_or("?")),
            v(hs.swapout, 0),
            lim(&|l| l.swapout_rate),
        );
        h += &match gate {
            Some(Ok(())) => "<p class=kv>spawn gate <b class=g>admits</b></p>".into(),
            Some(Err(why)) => format!("<p class=kv>spawn gate <b class=r>HOST_BUSY</b>: {}</p>", esc(&why)),
            None => String::new(),
        };
        if let Some(c) = cfg.filter(|c| !c.accounts.names.is_empty()) {
            h += "<h2>Accounts</h2>";
            for name in c.accounts.names.keys() {
                let cap = crate::access::cap_line(c, self.r, events, name, ctx.now);
                let usage = crate::usage::meter(ctx, c, self.r, name)
                    .map(|m| crate::usage::shown(self.r, &m, &c.accounts.usage[name], ctx.now));
                let usage = match usage {
                    Some(Ok((parts, pace))) => {
                        let bar = |(t, p): &(String, f64)| {
                            format!(
                                " · {} <meter min=0 max=100 low=75 high=89.9 optimum=0 value={p:.0}></meter>",
                                esc(t)
                            )
                        };
                        parts.iter().map(bar).collect::<String>()
                            + &pace.map_or(String::new(), |p| format!(" · <b class=a>{}</b>", esc(&p)))
                    }
                    Some(Err(hint)) => format!(" · <span class=a>{}</span>", esc(&hint)),
                    None => String::new(),
                };
                let hours = crate::usage::lane_hours(events, name, ctx.now);
                h += &format!(
                    "<p class=kv>{} · <b class={}>{}</b>{usage}</p><p class=m>{}</p>",
                    esc(name),
                    tone(cap.split(' ').next().unwrap_or_default()),
                    esc(&cap),
                    esc(&hours)
                );
            }
        }
        match cfg.and_then(|c| crate::usage::deepseek(ctx, c, self.r)) {
            Some(Ok((b, low))) => {
                let (class, word) = if low { ("a", "low") } else { ("g", "ok") };
                h += &format!(
                    "<p class=kv>deepseek · {} {:.2} · <b class={class}>{word}</b></p>",
                    esc(&b.currency),
                    b.total
                );
            }
            Some(Err(hint)) => h += &format!("<p class=m>deepseek · {}</p>", esc(&hint)),
            None => {}
        }
        if self.addr.is_some() && self.export.is_none() {
            let (all, mt) = (
                devices::load(ctx)
                    .into_iter()
                    .filter(|d| d.code.is_none())
                    .collect::<Vec<_>>(),
                mtime(ctx),
            );
            h += "<h2>Paired devices</h2>";
            if all.is_empty() {
                h += "<p class=m>none yet: tap Pair this phone, then `herdsman serve approve &lt;code&gt;` here</p>";
            }
            for d in all {
                let when = format!(
                    "paired {} ago · used {} ago",
                    age(ctx.now.saturating_sub(d.created)),
                    age(ctx.now.saturating_sub(d.used))
                );
                h += &format!("<form method=post class=role>{}<input type=hidden name=hash value={}><span>{} <span class=m>{when}</span></span><button>Remove</button></form>", hidden("unpair", "", &mt), esc(&d.hash), esc(&d.label));
            }
            // #18: the button shows only in the Home Screen app (serve.js); endpoints and keys stay on the Mac.
            let key = crate::webpush::vapid(&ctx.state)
                .map(|k| crate::webpush::public(&k))
                .unwrap_or_default();
            // #87: on/off from fleet.toml; the last `push-sent`, unless a webpush `notify-failed` is newer.
            let (subs, sent) = (
                crate::webpush::load(&ctx.state),
                events.iter().rfind(|e| e.kind == "push-sent"),
            );
            let failed = events
                .iter()
                .rfind(|e| e.kind == "notify-failed" && e.str("cmd") == Some("webpush"));
            let when = sent.map_or("never sent".into(), |e| format!("last sent {} ago", ago(ctx.now, e.ts)));
            let row = match (
                cfg.and_then(Config::webpush_sub),
                failed.filter(|f| sent.is_none_or(|e| f.ts >= e.ts)),
            ) {
                (None, _) => "off: no webpush notifier".to_string(),
                (_, Some(f)) => format!("on · last failure: {}", f.str("error").unwrap_or("?")),
                _ => format!(
                    "on · {} device{} · {when}",
                    subs.len(),
                    if subs.len() == 1 { "" } else { "s" }
                ),
            };
            let key = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, key);
            h += &format!("<h2>Alerts</h2><p class=kv>{}</p>", esc(&row));
            h += &format!("<button id=push type=button hidden data-key={key}>Enable alerts</button>");
            for s in subs {
                h += &format!(
                    "<form method=post action=/push/unsubscribe class=role><input type=hidden name=id value={}><span>{} <span class=m>alerts on {} ago</span></span><button>Remove</button></form>",
                    s.id(),
                    esc(&s.label),
                    age(ctx.now.saturating_sub(s.created))
                );
            }
        }
        h
    }
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

/// `serve --export <dir> [--demo]` (#39, docs/serve.md): the page and its files as static files with relative URLs.
pub fn export(ctx: &Ctx, r: &dyn Runner, dir: &std::path::Path, demo: bool) -> Result<()> {
    let canned = if demo {
        Canned(serde_json::from_str(&std::fs::read_to_string(
            ctx.config.with_file_name("runner.json"),
        )?)?)
    } else {
        Canned::default()
    };
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
        s.page(ctx, &BTreeMap::new(), demo)?
            .replace("<form method=post", "<form inert"),
        s.herd(ctx, None)?.to_string(),
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
    std::fs::create_dir_all(dir.join(&v))?;
    let files = [
        ("index.html", page.as_bytes()),
        ("herd.json", herd.as_bytes()),
        ("sw.js", sw.as_bytes()),
    ]
    .map(|(n, b)| (n.to_string(), b));
    for (name, body) in files
        .into_iter()
        .chain(FILES.map(|f| (f.0.to_string(), f.2)))
        .chain(ASSETS.map(|a| (v.clone() + a.0, a.2)))
    {
        std::fs::write(dir.join(name), body)?;
    }
    let (cfg, st) = (Config::load(&ctx.config)?, fold(&events));
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

/// `PR #11 · CI ✓ · rev PASS r1`, from the cached `herdsman pr` read only (never calls `gh`).
fn pr_line(ctx: &Ctx, fc: Option<&crate::config::Fleet>, fleet: &str, issue: u64, s: Option<&IssueState>) -> String {
    let pr = gh::cached_pr(ctx, fleet, issue);
    let mut parts = vec![];
    if pr.is_null() {
        parts.push("no PR read yet".to_string());
    } else {
        let n = pr["number"].as_u64().unwrap_or_default();
        parts.push(match fc {
            Some(c) => format!("<a href=\"https://github.com/{}/pull/{n}\">PR #{n}</a>", esc(&c.repo)),
            None => format!("PR #{n}"),
        });
        let ci = gh::ci(&gh::required(
            &pr["statusCheckRollup"],
            fc.map(|c| c.required_checks.as_slice()).unwrap_or_default(),
        ));
        let mark = match ci {
            "green" => "✓",
            "failed" => "✗",
            _ => "…",
        };
        parts.push(format!(
            "CI <span class={}>{mark}</span>",
            tone(if ci == "failed" {
                "FAIL"
            } else if ci == "green" {
                "PASS"
            } else {
                ""
            })
        ));
    }
    if let Some((s, (_, v))) = s.and_then(|s| s.verdicts.last().map(|v| (s, v))) {
        parts.push(format!("rev <span class={}>{}</span> r{}", tone(v), esc(v), s.rounds));
    }
    parts.join(" · ")
}

/// The queue row's dim line from the caches only: the issue title, then the PR's when it differs; one 390 px line.
fn titles(ctx: &Ctx, fleet: &str, issue: u64) -> Option<String> {
    let spec = gh::cached_title(ctx, fleet, issue);
    let pr = gh::cached_pr(ctx, fleet, issue);
    let pr = pr["number"]
        .as_u64()
        .zip(pr["title"].as_str())
        .filter(|p| Some(p.1) != spec.as_deref())
        .map(|(n, p)| format!("PR #{n} · {p}"));
    let t = [spec, pr].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    if t.is_empty() {
        return None;
    }
    if t.chars().count() <= 50 {
        return Some(t);
    }
    let cut: String = t.chars().take(51).collect();
    Some(format!(
        "{} …",
        cut.rsplit_once(' ')
            .map_or(cut.as_str(), |c| c.0)
            .trim_end_matches([' ', '·'])
    ))
}

/// A fleet's role rows: the model/effort pickers (`rw`), else the settings as text.
fn roles(cfg: &Config, fleet: &str, fc: &crate::config::Fleet, rw: bool, mt: &str) -> String {
    fc.role
        .iter()
        .map(|(name, r)| role_form(cfg, fleet, name, r, rw, mt))
        .collect()
}

fn role_form(cfg: &Config, fleet: &str, role: &str, r: &Role, rw: bool, mt: &str) -> String {
    let head = format!("<b>{}</b> <span class=m>{}</span> ", esc(role), esc(&r.harness));
    if !rw {
        return format!(
            "<p>{head}{}</p>",
            esc(&join(&[
                r.model.as_deref().unwrap_or_default(),
                r.effort.as_deref().unwrap_or_default()
            ]))
        );
    }
    // Unset: what the lane actually gets (the profile's/harness's model, `[limits].effort`).
    let model = if r.harness == "hermes" {
        "profile".to_string()
    } else {
        r.harness.clone()
    };
    let effort = cfg.limits.effort.clone().unwrap_or_else(|| "none".into());
    let select = |name: &str, cur: &Option<String>, def: &str, opts: &[&str]| {
        let mut s = format!("<select name={name} aria-label={name}>");
        if cur.as_ref().is_none_or(|c| !opts.contains(&c.as_str())) {
            s += &format!(
                "<option value=\"\" selected>{}</option>",
                esc(&cur.clone().unwrap_or(format!("default ({def})")))
            );
        }
        for o in opts {
            let sel = if cur.as_deref() == Some(o) { " selected" } else { "" };
            s += &format!("<option{sel}>{}</option>", esc(o));
        }
        s + "</select>"
    };
    let models: Vec<&str> = cfg.models.iter().map(String::as_str).collect();
    format!(
        "<form method=post class=role>{}<input type=hidden name=role value={}>{head}{}{}<button>Set</button></form>",
        hidden("set", fleet, mt),
        esc(role),
        select("model", &r.model, &model, &models),
        select("effort", &r.effort, &effort, crate::harness::efforts(&r.harness)),
    )
}

/// Noise folded into one count per run of consecutive events unless `all`.
const NOISE: [&str; 2] = ["clean", "notify-failed"];

fn events_tab(ctx: &Ctx, cfg: Option<&Config>, events: &[Event], st: &States, all: bool) -> String {
    let repo = |fleet: &str| cfg.and_then(|c| c.fleet.get(fleet)).map(|f| f.repo.clone());
    let mut h = String::from("<h2><span>Last 50 events</span></h2>");
    let (mut rows, mut it) = (0, events.iter().rev().peekable());
    while let Some(e) = it.next() {
        if rows == 50 && !all {
            break;
        }
        rows += 1;
        if !all && NOISE.contains(&e.kind.as_str()) {
            let mut n = 1;
            while it.next_if(|x| NOISE.contains(&x.kind.as_str())).is_some() {
                n += 1;
            }
            h += &format!(
                "<p class=ev><span class=m>{} · {n} × clean / notify-failed</span></p>",
                age(ctx.now.saturating_sub(e.ts))
            );
            continue;
        }
        let mut links = vec![];
        if let (Some(r), Some(n)) = (repo(&e.fleet), e.issue) {
            links.push(format!("<a href=\"https://github.com/{r}/issues/{n}\">issue</a>"));
            if let Some(pr) = e.data.get("pr").map(|p| p.to_string().trim_matches('"').to_string()) {
                links.push(format!("<a href=\"https://github.com/{r}/pull/{}\">PR</a>", esc(&pr)));
            }
            if let Some(b) = st.get(&(e.fleet.clone(), n)).and_then(|s| s.branch.as_deref()) {
                links.push(format!(
                    "<a href=\"https://github.com/{r}/actions?query=branch%3A{}\">runs</a>",
                    esc(b)
                ));
            }
        }
        let via = if e.str("via") == Some("web") {
            " <span class=m>(web)</span>"
        } else {
            ""
        };
        h += &format!(
            "<p class=ev><span class=m>{}</span> {}{via} {}</p>",
            age(ctx.now.saturating_sub(e.ts)),
            esc(&plain(e)),
            links.join(" ")
        );
    }
    if !all {
        h += "<p><a href=\"?all=1\">all events, unfolded</a></p>";
    }
    h
}
