//! `herdsman serve`, the phone page (docs/serve.md): std `TcpListener`, one request at a time.
use crate::config::{Config, Role};
use crate::events::{age, append, fold, plain, read, Event, IssueState};
use crate::gate::{refused, Refusal};
use crate::host::{self, Host};
use crate::runner::{argv, Runner};
use crate::{devices, gh, lanes, queue, Ctx};
use anyhow::{bail, Context, Result};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

const CSS: &str = include_str!("../assets/serve.css");
const JS: &str = include_str!("../assets/serve.js");
const SW: &str = include_str!("../assets/sw.js");
const MANIFEST: &str = include_str!("../assets/manifest.webmanifest");
const ICON_180: &[u8] = include_bytes!("../assets/icon-180.png");
const ICON_192: &[u8] = include_bytes!("../assets/icon-192.png");
const ICON_512: &[u8] = include_bytes!("../assets/icon-512.png");
const HORSES: &str = include_str!("../assets/horses.svg");

type States = BTreeMap<(String, u64), IssueState>;

pub fn serve(ctx: &Ctx, r: &dyn Runner, addr: &str, read_only: bool) -> Result<()> {
    let l = TcpListener::bind(addr)?;
    let mode = if read_only { "read-only view" } else { "phone page" };
    eprintln!("herdsman serve: {mode} on http://{}/", l.local_addr()?);
    serve_on(ctx, r, l, read_only)
}

pub fn serve_on(ctx: &Ctx, r: &dyn Runner, l: TcpListener, read_only: bool) -> Result<()> {
    let s = Server::new(ctx, r, (!read_only).then(|| l.local_addr().map(|a| a.to_string())).transpose()?);
    for st in l.incoming().flatten() {
        let _ = s.handle(st);
    }
    Ok(())
}

pub struct Server<'a> {
    ctx: &'a Ctx,
    r: &'a dyn Runner,
    /// The bind address a POST's `Host` must name; None: read-only.
    addr: Option<String>,
    /// Command stdout by argv, with its time: herdr reads are rate-limited.
    cache: RefCell<BTreeMap<String, (u64, Option<String>)>>,
    host: RefCell<Option<(u64, Host)>>,
    /// Reply-box send times per device hash (10 a minute).
    sent: RefCell<BTreeMap<String, Vec<u64>>>,
}

fn header<'h>(head: &'h str, name: &str) -> Option<&'h str> {
    head.lines().skip(1).find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim()))
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
    s.split('&').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (dec(k), dec(v))).collect()
}

fn enc(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// fleet.toml's mtime (ns), rendered into every form: a POST refuses STALE when it moved.
fn mtime(ctx: &Ctx) -> String {
    let t = std::fs::metadata(&ctx.config).and_then(|m| m.modified()).ok();
    t.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(String::new(), |d| d.as_nanos().to_string())
}

/// An error as the page shows it: `REFUSED <CODE>: …` or `error: …`.
fn shown(e: &anyhow::Error) -> String {
    e.downcast_ref::<Refusal>().map_or_else(|| format!("error: {e:#}"), |r| r.to_string())
}

/// `v` is `name`, or `name:<port>`.
fn names(name: &str, v: &str) -> bool {
    v == name || v.rsplit_once(':').is_some_and(|(n, p)| n == name && p.chars().all(|c| c.is_ascii_digit()))
}

/// green = merged/PASS/ready/working, amber = queued/impl/review/fix/idle, red = blocked/dead/FAIL.
fn tone(state: &str) -> &'static str {
    match state.split('(').next().unwrap_or_default() {
        "merged" | "ready" | "PASS" | "working" | "done" | "ok" | "normal" => "g",
        "blocked" | "needs-human" | "dead" | "FAIL" | "critical" | "capped" => "r",
        _ => "a",
    }
}

/// The non-empty HTML lines, one `<p>` each.
fn card(lines: &[String]) -> String {
    lines.iter().filter(|l| !l.is_empty()).map(|l| format!("<p>{l}</p>")).collect()
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

/// The non-empty fields, ` · `-joined (no `-` placeholders).
fn join(parts: &[&str]) -> String {
    parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join(" · ")
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
    ("8a5a33", "6f4527", "2e2019", false), ("a05c2c", "7d4520", "5d3317", true), ("3b3b44", "2a2a31", "17161a", false),
    ("b9bcc6", "989ca9", "7d8089", false), ("c89a56", "a37a3c", "e8dcc0", true), ("b08d5e", "8e6f45", "41321f", true),
    ("96685a", "77524a", "4a342c", false), ("bd8f4e", "96703a", "241a10", true),
];

/// The state as an avatar in all three modes (CSS shows `body[data-mode]`'s); coat stable per name.
pub fn avatar(name: &str, state: &str) -> String {
    let (pose, glyph, art) = match state {
        "working" => (1, "▶", format!("<i class=\"asc run\"><b>{}</b><b>{}</b></i>", GALLOP[0], GALLOP[1])),
        "done" => (3, "✓", format!("<i class=asc><b>{REST}</b></i>")),
        "blocked" | "dead" => (4, "✗", format!("<i class=asc><b>{REAR}</b></i>")),
        _ => (2, "⏸", format!("<i class=asc><b>{STAND}</b></i>")),
    };
    let (c, d, m, blaze) = COATS[usize::from_str_radix(&devices::sha256(name.as_bytes())[..2], 16).unwrap_or(0) % 8];
    let coat = format!("--c:#{c};--d:#{d};--m:#{m};--b:{}", if blaze { "#efe7dc" } else { "transparent" });
    let horse = HORSES.lines().nth(pose).unwrap_or_default();
    format!("<span class=\"av {} {state}\"><i class=gl>{glyph}</i>{art}<svg class=px viewBox=\"0 0 132 100\" style=\"{coat}\">{horse}</svg></span>", tone(state))
}

/// The device kind from `User-Agent`, for the Host tab's device list.
fn kind(head: &str) -> &'static str {
    let ua = header(head, "user-agent").unwrap_or_default();
    ["iPhone", "iPad", "Android", "Mac", "Windows", "Linux"].into_iter().find(|k| ua.contains(k)).unwrap_or("device")
}

impl<'a> Server<'a> {
    pub fn new(ctx: &'a Ctx, r: &'a dyn Runner, addr: Option<String>) -> Self {
        Server { ctx, r, addr, cache: RefCell::default(), host: RefCell::default(), sent: RefCell::default() }
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
        s.write_all(&self.respond(&String::from_utf8_lossy(&buf), crate::now()))
    }

    /// stdout of `prog args` if it ran OK within the last `ttl` seconds (else run it now).
    fn cached(&self, now: u64, ttl: u64, prog: &str, args: &[&str]) -> Option<String> {
        let key = format!("{prog} {}", args.join(" "));
        if let Some((_, out)) = self.cache.borrow().get(&key).filter(|(t, _)| now >= *t && now - t < ttl) {
            return out.clone();
        }
        let out = self.r.run(prog, &argv(args)).ok().filter(|o| o.code == 0).map(|o| o.stdout);
        self.cache.borrow_mut().insert(key, (now, out.clone()));
        out
    }

    /// The full HTTP response to one request.
    pub fn respond(&self, req: &str, now: u64) -> Vec<u8> {
        let (head, body) = req.split_once("\r\n\r\n").unwrap_or((req, ""));
        let mut w = head.split_whitespace();
        let (method, target) = (w.next().unwrap_or_default(), w.next().unwrap_or_default());
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let ctx = &Ctx { now, ..self.ctx.clone() };
        let text = |s: &str| s.as_bytes().to_vec();
        let mut extra = String::new();
        let (status, ctype, body) = match (method, path) {
            ("GET", "/") => match self.page(ctx, &form(query)) {
                Ok(b) => ("200 OK", "text/html; charset=utf-8", b.into_bytes()),
                Err(e) => ("500 Internal Server Error", "text/plain", format!("{e:#}\n").into_bytes()),
            },
            ("GET", "/lane") if !self.allowed(ctx, head) => ("403 Forbidden", "text/plain", text("forbidden: foreign Host or Origin\n")),
            ("GET", "/lane") if self.device(ctx, head).is_none() => ("401 Unauthorized", "text/plain", text("pair this device to read transcripts\n")),
            ("GET", "/lane") => match self.transcript(ctx, form(query).get("agent").map_or("", String::as_str)) {
                Some(j) => ("200 OK", "application/json", j.into_bytes()),
                None => ("404 Not Found", "text/plain", text("no such agent pane\n")),
            },
            ("GET", "/sw.js") => ("200 OK", "text/javascript", text(SW)),
            ("GET", "/manifest.webmanifest") => ("200 OK", "application/manifest+json", text(MANIFEST)),
            ("GET", "/icon-180.png") => ("200 OK", "image/png", ICON_180.to_vec()),
            ("GET", "/icon-192.png") => ("200 OK", "image/png", ICON_192.to_vec()),
            ("GET", "/icon-512.png") => ("200 OK", "image/png", ICON_512.to_vec()),
            ("GET", _) => ("404 Not Found", "text/plain", text("not found\n")),
            ("POST", _) if self.addr.is_none() => ("405 Method Not Allowed", "text/plain", text("read-only: GET only\n")),
            ("POST", _) if !self.allowed(ctx, head) => ("403 Forbidden", "text/plain", text("forbidden: foreign Host or Origin\n")),
            ("POST", "/") if form(body.trim()).get("op").is_some_and(|o| o == "unpair" || o == "answer") && self.device(ctx, head).is_none() => {
                ("401 Unauthorized", "text/plain", text("only a paired device may remove devices or answer asks\n"))
            }
            ("POST", "/") => {
                let msg = self.write(ctx, &form(body.trim())).unwrap_or_else(|e| shown(&e));
                let loc = format!("/?msg={}", enc(&msg));
                return format!("HTTP/1.1 303 See Other\r\nLocation: {loc}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes();
            }
            ("POST", "/pair") => match devices::redeem(ctx, form(body.trim()).get("code").map_or("", String::as_str), kind(head)) {
                Ok(Some(t)) => {
                    extra = format!("Set-Cookie: hd={t}; Path=/; Max-Age=31536000; HttpOnly; Secure; SameSite=Strict\r\n");
                    ("200 OK", "text/plain", text("paired\n"))
                }
                _ => ("403 Forbidden", "text/plain", text("wrong or expired code: run `herdsman serve pair` for a new one\n")),
            },
            ("POST", "/send") => {
                let (status, msg) = self.send(ctx, head, &form(body.trim()));
                (status, "text/plain", format!("{msg}\n").into_bytes())
            }
            ("POST", _) => ("404 Not Found", "text/plain", text("not found\n")),
            _ => ("405 Method Not Allowed", "text/plain", text("GET or POST only\n")),
        };
        let allow = if self.addr.is_some() { "GET, POST" } else { "GET" };
        let mut res = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nAllow: {allow}\r\n{extra}Cache-Control: no-store\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        res.extend(body);
        res
    }

    /// `Host` names the bind address or a `[serve].allowed_hosts` name, and any `Origin` is one of those.
    fn allowed(&self, ctx: &Ctx, head: &str) -> bool {
        let mut hosts = vec![self.addr.clone().unwrap_or_default()];
        hosts.extend(Config::load(&ctx.config).map(|c| c.serve.allowed_hosts).unwrap_or_default());
        let host = header(head, "host").unwrap_or_default();
        let origin = header(head, "origin").map(|o| o.strip_prefix("https://").or(o.strip_prefix("http://")).unwrap_or("\0"));
        hosts.iter().any(|h| names(h, host)) && origin.is_none_or(|o| hosts.iter().any(|h| names(h, o)))
    }

    /// The paired device whose token the request's `Cookie` carries.
    fn device(&self, ctx: &Ctx, head: &str) -> Option<devices::Device> {
        devices::find(ctx, header(head, "cookie").unwrap_or_default())
    }

    /// The reply box: a paired device (cookie) prompts one agent, at most 10 sends a minute.
    fn send(&self, ctx: &Ctx, head: &str, f: &BTreeMap<String, String>) -> (&'static str, String) {
        let Some(dev) = self.device(ctx, head) else {
            return ("401 Unauthorized", "pair this device: run `herdsman serve pair` on the Mac".into());
        };
        let mut sent = self.sent.borrow_mut();
        let times = sent.entry(dev.hash.clone()).or_default();
        times.retain(|t| ctx.now < t + 60);
        if times.len() >= 10 {
            return ("429 Too Many Requests", "10 sends a minute: wait a moment".into());
        }
        times.push(ctx.now);
        match self.prompt(ctx, &dev, f) {
            Ok(msg) => ("200 OK", msg),
            Err(e) => ("409 Conflict", shown(&e)),
        }
    }

    /// `herdr agent prompt` (dsh: `pane send-text`) + Enter; logs a `prompt` event without the text.
    fn prompt(&self, ctx: &Ctx, dev: &devices::Device, f: &BTreeMap<String, String>) -> Result<String> {
        let g = |k: &str| f.get(k).map_or("", String::as_str);
        let (agent, text) = (g("agent"), g("text"));
        let (cfg, events) = (Config::load(&ctx.config)?, read(&ctx.state)?);
        let all = self.lanes(ctx.now, &cfg, &events, &fold(&events)).context("herdr agent list failed")?;
        let l = all.iter().find(|l| l.name == agent).with_context(|| format!("no agent `{agent}`"))?;
        let (Some(fleet), Some(pane)) = (&l.fleet, &l.pane) else { bail!("`{agent}` is not a live fleet lane") };
        if cfg.fleet.get(fleet).is_some_and(|f| f.paused) {
            return refused("PAUSED", format!("{fleet} is paused (`herdsman resume {fleet}`)"));
        }
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
        // No `--`: herdr 0.9 reads the 2nd positional as literal text (even `-x`), and a `--` there would be sent as text.
        let first = if l.harness == "dsh" { argv(&["pane", "send-text", pane, text]) } else { argv(&["agent", "prompt", agent, text]) };
        for a in [first, argv(&["pane", "send-keys", pane, "Enter"])] {
            let o = self.r.run("herdr", &a)?;
            if o.code != 0 {
                bail!("herdr {} {}: {}", a[0], a[1], o.stderr.trim());
            }
        }
        let d = serde_json::json!({"via": "web", "agent": agent, "device": dev.label, "chars": n});
        append(&ctx.state, &Event::new(ctx.now, fleet, l.issue, "prompt", d))?;
        Ok(format!("sent to {agent}"))
    }

    /// The sheet's transcript: the agent's last 40 pane lines, masked, as JSON.
    fn transcript(&self, ctx: &Ctx, agent: &str) -> Option<String> {
        let (cfg, events) = (Config::load(&ctx.config).ok()?, read(&ctx.state).ok()?);
        let l = self.lanes(ctx.now, &cfg, &events, &fold(&events))?.into_iter().find(|l| l.name == agent)?;
        let screen = self.cached(ctx.now, 5, "herdr", &["pane", "read", l.pane.as_deref()?, "--source", "recent", "--lines", "80"])?;
        Some(serde_json::json!({"harness": l.harness, "lines": lanes::tail(&screen, 40)}).to_string())
    }

    /// Run one form write; the line shown on the page (CLI output, or `REFUSED <CODE>: …` via `shown`).
    fn write(&self, ctx: &Ctx, f: &BTreeMap<String, String>) -> Result<String> {
        let g = |k: &str| f.get(k).map_or("", String::as_str);
        if g("mtime") != mtime(ctx) {
            return refused("STALE", "fleet.toml changed since the page was rendered; reload and retry");
        }
        let (fleet, mut out) = (g("fleet"), vec![]);
        match g("op") {
            "mv" => {
                let n = |k: &str| g(k).parse::<u64>().ok();
                let issue = n("issue").context("no issue")?;
                let d = queue::op("mv", &[issue], false, n("before"), n("after"), None);
                writeln!(out, "{}", crate::queue_change(ctx, &Config::load(&ctx.config)?, fleet, d, Some("web"))?)?;
            }
            op @ ("pause" | "resume") => crate::pause(ctx, &mut out, fleet, op == "pause", Some("web"))?,
            "set" => {
                let wanted = ["model", "effort"].into_iter().filter(|k| !g(k).is_empty()).map(|k| (k, g(k).to_string()));
                crate::set(ctx, &mut out, fleet, g("role"), wanted.collect(), Some("web"))?
            }
            "answer" => {
                let choice = if g("choice").is_empty() { g("text") } else { g("choice") };
                crate::answer(ctx, &Config::load(&ctx.config)?, self.r, &mut out, (fleet, g("id")), choice, ("owner", Some("web")))?
            }
            "unpair" => {
                let mut all = devices::load(ctx);
                all.retain(|d| d.hash != g("hash"));
                devices::store(ctx, &all)?;
                writeln!(out, "device removed")?;
            }
            op => bail!("unknown op `{op}`"),
        }
        Ok(String::from_utf8_lossy(&out).trim().replace('\n', "; "))
    }

    fn page(&self, ctx: &Ctx, q: &BTreeMap<String, String>) -> Result<String> {
        let events = read(&ctx.state)?;
        let cfg = Config::load(&ctx.config).ok();
        let st = fold(&events);
        let msg = q.get("msg").map_or(String::new(), |m| {
            let bad = m.starts_with("REFUSED") || m.starts_with("error");
            format!("<p class=\"msg {}\">{}</p>", if bad { "r" } else { "g" }, esc(m))
        });
        let ro = if self.addr.is_some() { "" } else { " data-ro=1" };
        Ok(format!(
            "<!doctype html><html lang=en><head><meta charset=utf-8>\
             <meta name=viewport content=\"width=device-width,initial-scale=1,viewport-fit=cover\">\
             <meta name=theme-color content=\"#1f2a38\"><meta name=apple-mobile-web-app-capable content=yes>\
             <link rel=manifest href=/manifest.webmanifest><link rel=icon href=/icon-192.png><link rel=apple-touch-icon href=/icon-180.png>\
             <title>herdsman</title><style>{CSS}</style></head>\
             <body data-ts={} data-mode=horse{ro}><header><b>herdsman</b><span id=stale hidden></span>\
             <button id=mode type=button aria-label=\"lane display mode\">horse</button></header>{msg}{}<main>\
             <section id=queue>{}</section><section id=lanes>{}</section><section id=host>{}</section>\
             <section id=events>{}</section></main><nav><a href=#queue>Queue</a><a href=#lanes>Lanes</a>\
             <a href=#host>Host</a><a href=#events>Events</a></nav><script>{JS}</script></body></html>\n",
            ctx.now,
            self.needs_you(ctx, &events),
            self.queue_tab(ctx, cfg.as_ref(), &events, &st),
            self.lanes_tab(ctx, cfg.as_ref(), &events, &st),
            self.host_tab(ctx, cfg.as_ref(), &events),
            events_tab(ctx, cfg.as_ref(), &events, &st, q.get("all").is_some_and(|v| v == "1")),
        ))
    }

    /// Each open ask as a red card above every tab: options (recommended first) and a free-text box.
    fn needs_you(&self, ctx: &Ctx, events: &[Event]) -> String {
        crate::events::open_asks(events).into_iter().map(|a| {
            let s = |k| a.str(k).unwrap_or_default();
            let mut opts: Vec<(u64, &str)> = a.data["options"].as_array().into_iter().flatten().zip(1..).map(|(o, n)| (n, o.as_str().unwrap_or("?"))).collect();
            opts.sort_by_key(|o| Some(o.0) != a.data["recommend"].as_u64());
            let btns: String = opts.iter().map(|(n, o)| format!("<button name=choice value={n}>{n}. {}</button>", esc(o))).collect();
            let form = format!("<form method=post>{}<input type=hidden name=id value=\"{}\"><input name=text aria-label=answer placeholder=\"or type an answer\"><button>Send</button>{btns}</form>", hidden("answer", &a.fleet, &mtime(ctx)), esc(s("id")));
            let who = join(&[&a.fleet, &a.issue.map_or(String::new(), |n| format!("#{n}")), s("agent"), &age(ctx.now.saturating_sub(a.ts))]);
            format!("<div class=ask><p><b>needs you</b> <span class=m>{}</span></p><p>{}</p>{}</div>", esc(&who), esc(s("question")), if self.addr.is_some() { form } else { String::new() })
        }).collect()
    }

    /// `herdr <args>` JSON, read at most every 10 s (None when herdr cannot be read).
    fn herdr_json(&self, now: u64, args: &[&str]) -> Option<serde_json::Value> {
        serde_json::from_str(&self.cached(now, 10, "herdr", args)?).ok()
    }

    /// Names of the live herdr agents (None when herdr cannot be read).
    fn live(&self, now: u64) -> Option<BTreeSet<String>> {
        let v = self.herdr_json(now, &["agent", "list"])?;
        Some(v["result"]["agents"].as_array()?.iter().filter_map(|a| a["name"].as_str().map(String::from)).collect())
    }

    /// Per configured fleet: open work in queue order, other open issues, `done today`; paused = one line.
    fn queue_tab(&self, ctx: &Ctx, cfg: Option<&Config>, events: &[Event], st: &States) -> String {
        let (now, mt, rw) = (ctx.now, mtime(ctx), self.addr.is_some());
        let Some(cfg) = cfg else { return "<p class=r>cannot read fleet.toml</p>".into() };
        let live = self.live(now);
        let dead = |s: &IssueState| s.active_lane().is_some_and(|(_, a)| live.as_ref().is_some_and(|l| !l.contains(a)));
        let mut h = String::new();
        for (f, fc) in &cfg.fleet {
            let q = queue::view(events, st, f);
            let mine: Vec<&IssueState> = st.values().filter(|s| s.fleet == *f).collect();
            let queued = |n: u64| q.iter().any(|i| i.issue == n);
            let rest: Vec<u64> = mine.iter().filter(|s| !queued(s.issue) && s.state != "merged" && s.state != "queued").map(|s| s.issue).collect();
            let items: Vec<(u64, Option<u64>, Option<usize>)> = q.iter().enumerate().map(|(k, i)| (i.issue, i.blocked_by, Some(k)))
                .chain(rest.iter().map(|&n| (n, None, None)))
                .collect();
            let mut body = String::new();
            for &(n, blocked_by, k) in &items {
                let s = st.get(&(f.clone(), n));
                let is_dead = s.is_some_and(dead);
                let waiting = crate::events::open_asks(events).iter().any(|e| e.fleet == *f && e.issue == Some(n));
                let state = if is_dead { "dead" } else if waiting { "⏸ waiting: decision" } else { s.map_or("queued", |s| s.state.as_str()) };
                let a = s.filter(|s| s.last_ts > 0).map_or(String::new(), |s| age(now.saturating_sub(s.last_ts)));
                let lane = s.and_then(|s| s.lane.as_deref()).unwrap_or_default();
                let logged = s.map_or("queued", |s| s.state.as_str());
                let mut card = vec![esc(&join(&[&format!("#{n} {logged}"), lane, &a])), pr_line(ctx, Some(fc), f, n, s)];
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
                    let up = if k > 0 { mv("▲ up", "before", q[k - 1].issue) } else { String::new() };
                    let down = q.get(k + 1).map(|x| mv("▼ down", "after", x.issue)).unwrap_or_default();
                    card.push(format!("<span class=btns>{up}{down}</span>"));
                }
                body += &row("q", "", &format!("#{n}"), state, &a, &card);
            }
            if items.is_empty() {
                body += "<p class=m>no open work</p>";
            }
            let mut done: Vec<&&IssueState> = mine.iter().filter(|s| s.state == "merged" && now.saturating_sub(s.last_ts) < 86_400).collect();
            done.sort_by_key(|s| std::cmp::Reverse(s.last_ts));
            if !done.is_empty() {
                let rows: String = done.iter().take(5).map(|s| {
                    let pr = events.iter().rev().find(|e| e.is("merged", f, s.issue)).and_then(|e| e.str("pr"));
                    let link = pr.map_or(String::new(), |p| format!(" <a href=\"https://github.com/{}/pull/{}\">PR #{}</a>", esc(&fc.repo), esc(p), esc(p)));
                    format!("<p>#{} <span class=g>merged</span> {}{link}</p>", s.issue, age(now.saturating_sub(s.last_ts)))
                }).collect();
                body += &format!("<details class=done><summary><span>done today: {}</span></summary>{rows}</details>", done.len());
            }
            body += &format!("<details class=roles><summary><span>roles</span></summary>{}</details>",
                fc.role.iter().map(|(name, r)| role_form(cfg, f, name, r, rw, &mt)).collect::<String>());
            let button = |op: &str, label: &str| if rw { format!("<form method=post>{}<button>{label}</button></form>", hidden(op, f, &mt)) } else { String::new() };
            if fc.paused {
                let lanes: Vec<&&IssueState> = mine.iter().filter(|s| s.active_lane().is_some()).collect();
                let nd = lanes.iter().filter(|s| dead(s)).count();
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
            h += &format!("<h2><span>{}</span>{}</h2>", esc(f), button("pause", "Pause"));
            let stall = cfg.limits.stall_minutes * 60;
            let next = mine.iter().filter(|s| s.active_lane().is_some() && !dead(s)).map(|s| ((s.last_ts + stall).saturating_sub(now), s.issue)).min();
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
        let list = self.herdr_json(now, &["agent", "list"])?;
        let agents = list["result"]["agents"].as_array().cloned().unwrap_or_default();
        let wsl = self.herdr_json(now, &["workspace", "list"]).unwrap_or_default();
        let ws: BTreeMap<String, String> = wsl["result"]["workspaces"].as_array().into_iter().flatten()
            .filter_map(|w| Some((w["workspace_id"].as_str()?.into(), w["label"].as_str()?.into())))
            .collect();
        Some(lanes::join(cfg, &self.ctx.worktrees, events, st, &agents, &ws))
    }

    /// State sections, fleet subgroups inside; a row opens the bottom sheet (serve.js) with its hidden card.
    fn lanes_tab(&self, ctx: &Ctx, cfg: Option<&Config>, events: &[Event], st: &States) -> String {
        let Some(cfg) = cfg else { return "<p class=m>no fleet.toml</p>".into() };
        let Some(all) = self.lanes(ctx.now, cfg, events, st) else { return "<p class=r>herdr agent list failed</p>".into() };
        let state = |l: &lanes::Lane| l.issue.zip(l.fleet.as_ref()).and_then(|(n, f)| st.get(&(f.clone(), n)));
        let mut h = String::new();
        for (sec, subs) in lanes::group(&all, |l| state(l).map_or(l.since, |s| s.last_ts.max(l.since))) {
            h += &format!("<h2><span class={}>{sec} ({})</span></h2>", tone(sec), subs.iter().map(|s| s.1.len()).sum::<usize>());
            for (repo, ls) in subs {
                h += &format!("<h3>{} ({})</h3>", esc(&repo), ls.len());
                for l in ls {
                    h += &self.lane_row(ctx, cfg, l, state(l));
                }
            }
        }
        if h.is_empty() { "<p class=m>no agents</p>".into() } else { h }
    }

    fn lane_row(&self, ctx: &Ctx, cfg: &Config, l: &lanes::Lane, s: Option<&IssueState>) -> String {
        let screen = l.pane.as_ref().and_then(|p| self.cached(ctx.now, 30, "herdr", &["pane", "read", p, "--source", "visible"]));
        let a = if l.since > 0 { age(ctx.now.saturating_sub(l.since)) } else { String::new() };
        let title = l.issue.zip(l.fleet.as_ref()).and_then(|(n, f)| gh::cached_spec(ctx, f, n)).and_then(|v| v["title"].as_str().map(String::from));
        let issue = l.issue.map(|n| format!("#{n}")).unwrap_or_default();
        let round = s.map(|s| format!("round {}", s.state.split_once('(').map_or(s.rounds.to_string(), |r| r.1.trim_end_matches(')').into())));
        let o = |v: &Option<String>| v.clone().unwrap_or_default();
        let facts = [l.role.clone(), o(&round), l.harness.clone(), o(&l.model), o(&l.effort), o(&l.account), a.clone()];
        let mut card = vec![esc(format!("{issue} {}", o(&title)).trim()), esc(&join(&facts.iter().map(String::as_str).collect::<Vec<_>>()))];
        if let (Some(n), Some(f)) = (l.issue, &l.fleet) {
            card.push(pr_line(ctx, cfg.fleet.get(f), f, n, s));
        }
        if l.mismatch {
            card.push("<span class=r>⚠ log/pane mismatch: the log has this lane open, herdr has no pane</span>".into());
        }
        let card = self::card(&card);
        let act = esc(&lanes::activity(&l.harness, &screen.unwrap_or_default()));
        format!(
            "<div class=lane data-agent=\"{}\" data-role={}><button type=button class=lr>{}<span>{}</span><span class={}>{}</span>\
             <span class=m>{a}</span><span class=act>{act}</span></button><div class=card hidden>{card}</div></div>",
            esc(&l.name),
            esc(&l.role),
            avatar(&l.name, &l.state),
            esc(&l.name),
            tone(&l.state),
            esc(&l.state)
        )
    }

    fn host_tab(&self, ctx: &Ctx, cfg: Option<&Config>, events: &[Event]) -> String {
        let fresh = self.host.borrow().as_ref().filter(|(t, _)| ctx.now >= *t && ctx.now - t < 10).map(|h| h.1.clone());
        let hs = fresh.unwrap_or_else(|| {
            let h = host::sample(self.r);
            *self.host.borrow_mut() = Some((ctx.now, h.clone()));
            h
        });
        let limits = cfg.map(|c| &c.limits);
        let lim = |f: &dyn Fn(&crate::config::Limits) -> f64| limits.map_or(String::new(), |l| format!(" (limit {})", f(l)));
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
                let usage = crate::usage::meter(ctx, c, self.r, name).map(|m| crate::usage::shown(self.r, &m, &c.accounts.usage[name], ctx.now));
                let usage = match usage {
                    Some(Ok((parts, pace))) => {
                        let bar = |(t, p): &(String, f64)| format!(" · {} <meter min=0 max=100 low=75 high=89.9 optimum=0 value={p:.0}></meter>", esc(t));
                        parts.iter().map(bar).collect::<String>() + &pace.map_or(String::new(), |p| format!(" · <b class=a>{}</b>", esc(&p)))
                    }
                    Some(Err(hint)) => format!(" · <span class=a>{}</span>", esc(&hint)),
                    None => String::new(),
                };
                let hours = crate::usage::lane_hours(events, name, ctx.now);
                h += &format!("<p class=kv>{} · <b class={}>{}</b>{usage}</p><p class=m>{}</p>", esc(name), tone(cap.split(' ').next().unwrap_or_default()), esc(&cap), esc(&hours));
            }
        }
        if let Some((b, low)) = cfg.and_then(|c| crate::usage::deepseek(ctx, c, self.r)) {
            let (class, word) = if low { ("a", "low") } else { ("g", "ok") };
            h += &format!("<p class=kv>deepseek · {} {:.2} · <b class={class}>{word}</b></p>", esc(&b.currency), b.total);
        }
        if self.addr.is_some() {
            let (all, mt) = (devices::load(ctx), mtime(ctx));
            h += "<h2>Paired devices</h2>";
            if all.is_empty() {
                h += "<p class=m>none yet: the reply box asks for a `herdsman serve pair` code</p>";
            }
            for d in all {
                let when = format!("paired {} ago · used {} ago", age(ctx.now.saturating_sub(d.created)), age(ctx.now.saturating_sub(d.used)));
                h += &format!(
                    "<form method=post class=role>{}<input type=hidden name=hash value={}><span>{} <span class=m>{when}</span></span><button>Remove</button></form>",
                    hidden("unpair", "", &mt),
                    esc(&d.hash),
                    esc(&d.label)
                );
            }
        }
        h
    }
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
        let ci = gh::ci(&gh::required(&pr["statusCheckRollup"], fc.map(|c| c.required_checks.as_slice()).unwrap_or_default()));
        let mark = match ci { "green" => "✓", "failed" => "✗", _ => "…" };
        parts.push(format!("CI <span class={}>{mark}</span>", tone(if ci == "failed" { "FAIL" } else if ci == "green" { "PASS" } else { "" })));
    }
    if let Some((s, (_, v))) = s.and_then(|s| s.verdicts.last().map(|v| (s, v))) {
        parts.push(format!("rev <span class={}>{}</span> r{}", tone(v), esc(v), s.rounds));
    }
    parts.join(" · ")
}

fn role_form(cfg: &Config, fleet: &str, role: &str, r: &Role, rw: bool, mt: &str) -> String {
    let head = format!("<b>{}</b> <span class=m>{}</span> ", esc(role), esc(&r.harness));
    if !rw {
        return format!("<p>{head}{}</p>", esc(&join(&[r.model.as_deref().unwrap_or_default(), r.effort.as_deref().unwrap_or_default()])));
    }
    // Unset: what the lane actually gets (the profile's/harness's model, `[limits].effort`).
    let model = if r.harness == "hermes" { "profile".to_string() } else { r.harness.clone() };
    let effort = cfg.limits.effort.clone().unwrap_or_else(|| "none".into());
    let select = |name: &str, cur: &Option<String>, def: &str, opts: &[&str]| {
        let mut s = format!("<select name={name} aria-label={name}>");
        if cur.as_ref().is_none_or(|c| !opts.contains(&c.as_str())) {
            s += &format!("<option value=\"\" selected>{}</option>", esc(&cur.clone().unwrap_or(format!("default ({def})"))));
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
            h += &format!("<p class=ev><span class=m>{} · {n} × clean / notify-failed</span></p>", age(ctx.now.saturating_sub(e.ts)));
            continue;
        }
        let mut links = vec![];
        if let (Some(r), Some(n)) = (repo(&e.fleet), e.issue) {
            links.push(format!("<a href=\"https://github.com/{r}/issues/{n}\">issue</a>"));
            if let Some(pr) = e.data.get("pr").map(|p| p.to_string().trim_matches('"').to_string()) {
                links.push(format!("<a href=\"https://github.com/{r}/pull/{}\">PR</a>", esc(&pr)));
            }
            if let Some(b) = st.get(&(e.fleet.clone(), n)).and_then(|s| s.branch.as_deref()) {
                links.push(format!("<a href=\"https://github.com/{r}/actions?query=branch%3A{}\">runs</a>", esc(b)));
            }
        }
        let via = if e.str("via") == Some("web") { " <span class=m>(web)</span>" } else { "" };
        h += &format!(
            "<p class=ev><span class=m>{}</span> {}{via} {}</p>",
            age(ctx.now.saturating_sub(e.ts)),
            esc(&plain(e)),
            links.join(" ")
        );
    }
    if !all {
        h += "<p><a href=\"/?all=1\">all events, unfolded</a></p>";
    }
    h
}
