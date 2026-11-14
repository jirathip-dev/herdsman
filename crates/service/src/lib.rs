use anyhow::{bail, Result};
use drover_core::config::Config;
use drover_core::events::{append, emit, Event};
use drover_core::runner::{argv, Real, Runner};
use drover_core::{devices, service, Ctx};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::sync::{watch, Semaphore};
use tokio::time::{interval, timeout};

mod lanes;

const JOBS: usize = 16;
const CONNS: usize = 64;
const BODY: usize = 4 << 20;
const REPLAY: usize = 500;
const KICK_SECS: u64 = 900;
const CMD_SECS: u64 = 150;
const POLL_SECS: u64 = 15;
const STALL_SECS: u64 = 30;

pub struct Service {
    ctx: Ctx,
    r: &'static dyn Runner,
    clock: fn() -> u64,
    beat: AtomicU64,
    jobs: Semaphore,
    conns: Arc<Semaphore>,
    polling: AtomicBool,
    seq: watch::Sender<u64>,
    streams: std::sync::Mutex<std::collections::BTreeMap<&'static str, bool>>,
}

/// `drover service`: refuses a second one, then runs until killed.
pub fn main(ctx: &Ctx, api: &str, board: Option<&str>) -> Result<()> {
    if let Some(pid) = service::up(ctx) {
        bail!("a service already runs (pid {pid})");
    }
    if let Some(b) = board.map(String::from) {
        let c = ctx.clone();
        std::thread::spawn(move || {
            if let Err(e) = drover_server::hooks::board(&c, &Real, &b, false, false) {
                eprintln!("drover service: board on {b}: {e:#}");
            }
        });
    }
    let api = std::net::TcpListener::bind(api)?;
    run(ctx.clone(), &Real, drover_core::now, api)
}

/// The service on `api` and the state dir's socket, until the process ends; #529: lane state from shed's stream.
pub fn run(ctx: Ctx, r: &'static dyn Runner, clock: fn() -> u64, api: std::net::TcpListener) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        api.set_nonblocking(true)?;
        let api = TcpListener::from_std(api)?;
        let path = service::sock(&ctx);
        let _ = std::fs::remove_file(&path);
        let local = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        let s = Arc::new(Service {
            beat: AtomicU64::new(drover_core::now()),
            jobs: Semaphore::new(JOBS),
            conns: Arc::new(Semaphore::new(CONNS)),
            polling: AtomicBool::new(false),
            seq: watch::channel(drover_core::db::last_seq(&ctx.state)).0,
            streams: std::sync::Mutex::default(),
            ctx,
            r,
            clock,
        });
        let d = json!({"pid": std::process::id(), "api": api.local_addr()?.to_string()});
        append(&s.ctx.state, &s.event("service-started", d))?;
        s.watchdog();
        tokio::spawn(s.clone().watch_log());
        tokio::spawn(s.clone().poll());
        tokio::spawn(s.clone().local(local));
        if let Some(w) = Config::load(&s.ctx.config).ok().and_then(|c| c.shed()) {
            let me = s.clone();
            std::thread::spawn(move || me.shed(&w.wire));
        }
        loop {
            if let Ok((c, peer)) = api.accept().await {
                tokio::spawn(s.clone().api(c, peer));
            }
        }
    })
}

impl Service {
    fn event(&self, kind: &str, d: Value) -> Event {
        Event::new((self.clock)(), "drover", None, kind, d)
    }

    async fn job(&self, args: Vec<String>, secs: u64) -> Option<i32> {
        let _slot = self.jobs.acquire().await.ok()?;
        let c = Ctx {
            now: (self.clock)(),
            ..self.ctx.clone()
        };
        let (r, a) = (self.r, args.clone());
        let full = std::iter::once("drover".to_string()).chain(a).collect();
        let run = move || drover_core::main_with(full, &c, r, &mut std::io::sink(), &mut std::io::empty());
        if let Ok(code) = timeout(Duration::from_secs(secs), tokio::task::spawn_blocking(run)).await {
            return code.ok();
        }
        let d = json!({"job": args.join(" "), "secs": secs});
        let _ = append(&self.ctx.state, &self.event("service-timeout", d));
        None
    }

    fn watchdog(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            let mut t = interval(Duration::from_secs(1));
            loop {
                t.tick().await;
                me.beat.store(drover_core::now(), Relaxed);
            }
        });
        let me = self.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(5));
            let late = drover_core::now().saturating_sub(me.beat.load(Relaxed));
            if late > STALL_SECS {
                let ev = me.event("service-stalled", json!({"secs": late, "pid": std::process::id()}));
                let _ = Config::load(&me.ctx.config).map(|cfg| emit(&me.ctx.state, &cfg, ev));
                std::process::exit(70);
            }
        });
    }

    async fn watch_log(self: Arc<Self>) {
        let mut t = interval(Duration::from_millis(250));
        loop {
            t.tick().await;
            let state = self.ctx.state.clone();
            let seq = tokio::task::spawn_blocking(move || drover_core::db::last_seq(&state));
            if let Ok(Ok(n)) = timeout(Duration::from_secs(5), seq).await {
                self.seq.send_if_modified(|s| std::mem::replace(s, n) != n);
            }
        }
    }

    async fn poll(self: Arc<Self>) {
        let mut t = interval(Duration::from_secs(POLL_SECS));
        loop {
            t.tick().await;
            if !self.polling.swap(true, Relaxed) {
                let me = self.clone();
                tokio::spawn(async move {
                    me.job(argv(&["drive", "--fast"]), KICK_SECS).await;
                    me.polling.store(false, Relaxed);
                });
            }
        }
    }

    async fn local(self: Arc<Self>, l: UnixListener) {
        loop {
            if let Ok((c, _)) = l.accept().await {
                tokio::spawn(self.clone().line(c));
            }
        }
    }

    async fn line(self: Arc<Self>, c: UnixStream) {
        let (rd, mut wr) = c.into_split();
        let mut line = String::new();
        let mut rd = BufReader::new(rd.take(64 << 10));
        let read = rd.read_line(&mut line);
        if !matches!(timeout(Duration::from_secs(2), read).await, Ok(Ok(_))) {
            return;
        }
        let _ = wr
            .write_all(format!("{}\n", json!({"pid": std::process::id()})).as_bytes())
            .await;
        let v: Value = serde_json::from_str(&line).unwrap_or_default();
        let k = &v["kick"];
        if let (Some(f), Some(n)) = (k["fleet"].as_str(), k["issue"].as_u64()) {
            let why = k["why"].as_str().unwrap_or("kick");
            let id = k["id"].as_str().map(|i| ["--id".to_string(), i.into()]);
            let mut k = argv(&["kick", f, &n.to_string(), "--why", why, "--now"]);
            k.extend(id.into_iter().flatten());
            if self.job(k, KICK_SECS).await.is_none() {
                drover_core::db::release(&self.ctx.state, &format!("spawn-{f}-{n}.pending"), None);
            }
        }
    }

    async fn api(self: Arc<Self>, mut c: TcpStream, peer: SocketAddr) {
        let Ok(_slot) = self.conns.clone().try_acquire_owned() else {
            let _ = c.write_all(&reply(503, &json!({"error": "busy"}))).await;
            return;
        };
        let Ok(Ok(Some((head, body)))) = timeout(Duration::from_secs(10), request(&mut c)).await else {
            return;
        };
        let mut w = head.split(' ');
        let (method, target) = (w.next().unwrap_or_default(), w.next().unwrap_or_default());
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let out = match (method, path) {
            ("GET", "/api/v1/health") => {
                let late = drover_core::now().saturating_sub(self.beat.load(Relaxed));
                let busy = JOBS - self.jobs.available_permits();
                let d = json!({"ok": true, "pid": std::process::id(), "version": drover_core::VERSION, "jobs": busy, "beat_secs": late});
                reply(200, &d)
            }
            ("GET", "/api/v1/events") if peer.ip().is_loopback() || self.paired(&head) => {
                let after = query.split('&').find_map(|kv| kv.strip_prefix("after=")?.parse().ok());
                return self.stream(c, after.unwrap_or(0)).await;
            }
            ("GET", "/api/v1/events") => reply(403, &json!({"error": "pair this device first"})),
            ("POST", "/api/v1/cmd") => self.cmd(&body).await,
            _ => reply(404, &json!({"error": "no such path"})),
        };
        let _ = c.write_all(&out).await;
    }

    fn paired(&self, head: &str) -> bool {
        let cookie = head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("cookie").then_some(v.trim())
        });
        devices::find(&self.ctx, cookie.unwrap_or_default()).is_some()
    }

    async fn stream(self: Arc<Self>, mut c: TcpStream, mut after: u64) {
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n\r\n";
        if c.write_all(head.as_bytes()).await.is_err() {
            return;
        }
        let mut rx = self.seq.subscribe();
        loop {
            let ctx = self.ctx.clone();
            let rows = tokio::task::spawn_blocking(move || service::since(&ctx, after));
            let rows = match timeout(Duration::from_secs(5), rows).await {
                Ok(Ok(Ok(v))) => v,
                _ => vec![],
            };
            let mut out = String::new();
            for e in &rows[rows.len().saturating_sub(REPLAY)..] {
                after = e.seq;
                out += &format!(
                    "id: {}\ndata: {}\n\n",
                    e.seq,
                    serde_json::to_string(e).unwrap_or_default()
                );
            }
            if !out.is_empty() && c.write_all(out.as_bytes()).await.is_err() {
                return;
            }
            if timeout(Duration::from_secs(15), rx.changed()).await.is_err()
                && c.write_all(b": idle\n\n").await.is_err()
            {
                return;
            }
        }
    }

    async fn cmd(self: &Arc<Self>, body: &[u8]) -> Vec<u8> {
        let v: Value = serde_json::from_slice(body).unwrap_or_default();
        let token = v["token"].as_str().unwrap_or_default().to_string();
        let mut args: Vec<String> = v["args"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a.as_str().map(String::from))
            .collect();
        let (ctx, r, files) = (self.ctx.clone(), self.r, v["files"].clone());
        let run = move || -> Result<(u16, Value)> {
            let ctx = Ctx {
                now: drover_core::now(),
                ..ctx
            };
            let cfg = Config::load(&ctx.config)?;
            let lane = match service::authorize(&ctx, &cfg, &token, &args) {
                Ok(l) => l,
                Err(e) => return Ok((403, json!({"code": 3, "stdout": format!("{e}\n"), "stderr": ""}))),
            };
            let dir = ctx.state.with_file_name("service-files");
            for i in 1..args.len() {
                if args[i - 1] == "--file" {
                    let Some(text) = files[&args[i]].as_str() else {
                        let e = format!("drover: --file {} came without its text\n", args[i]);
                        return Ok((400, json!({"code": 2, "stdout": "", "stderr": e})));
                    };
                    std::fs::create_dir_all(&dir)?;
                    let p = dir.join(drover_core::devices::sha256(text.as_bytes()) + ".md");
                    std::fs::write(&p, text)?;
                    args[i] = p.display().to_string();
                }
            }
            let a = [argv(&[&format!("DROVER_LANE={lane}"), &ctx.exe]), args].concat();
            let o = r.run("env", &a)?;
            Ok((200, json!({"code": o.code, "stdout": o.stdout, "stderr": o.stderr})))
        };
        let Ok(_slot) = self.jobs.acquire().await else {
            return reply(503, &json!({"error": "closing"}));
        };
        match timeout(Duration::from_secs(CMD_SECS), tokio::task::spawn_blocking(run)).await {
            Ok(Ok(Ok((status, v)))) => reply(status, &v),
            Ok(Ok(Err(e))) => reply(
                500,
                &json!({"code": 1, "stdout": "", "stderr": format!("drover: {e:#}\n")}),
            ),
            _ => reply(
                504,
                &json!({"code": 1, "stdout": "", "stderr": "drover service: timed out\n"}),
            ),
        }
    }
}

async fn request(c: &mut TcpStream) -> std::io::Result<Option<(String, Vec<u8>)>> {
    let (mut buf, mut chunk) = (vec![], [0u8; 8192]);
    loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..i]).into_owned();
            let len = head.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse::<usize>().ok())?
            });
            let len = len.unwrap_or(0);
            if len > BODY {
                return Ok(None);
            }
            let mut body = buf.split_off(i + 4);
            while body.len() < len {
                let n = c.read(&mut chunk).await?;
                if n == 0 {
                    return Ok(None);
                }
                body.extend_from_slice(&chunk[..n]);
            }
            body.truncate(len);
            return Ok(Some((head, body)));
        }
        let n = c.read(&mut chunk).await?;
        if n == 0 || buf.len() > 64 << 10 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn reply(status: u16, v: &Value) -> Vec<u8> {
    let body = v.to_string();
    let len = body.len();
    format!("HTTP/1.1 {status} {status}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}").into_bytes()
}
