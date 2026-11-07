//! #626: serve's Funnel listener (`[serve] hooks_addr`): `POST /hooks/github` and nothing else; every other path is a
//! bare 404, so the board stays tailnet-only whatever Funnel maps here (docs/serve.md).
use crate::config::Config;
use crate::runner::Runner;
use crate::{webhook, Ctx};
use anyhow::Result;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Duration;

/// `herdsman serve`: the board on `addr` plus, unless read-only, the Funnel listener on `[serve] hooks_addr` (#626).
pub fn serve(ctx: &Ctx, r: &dyn Runner, addr: &str, read_only: bool) -> Result<()> {
    let hooks = Config::load(&ctx.config)
        .map(|c| c.serve.hooks_addr())
        .unwrap_or_default();
    let hooks = (!read_only && !hooks.is_empty()).then(|| TcpListener::bind(&hooks).map_err(|e| (hooks, e)));
    std::thread::scope(|sc| {
        match hooks {
            Some(Ok(h)) => {
                eprintln!("herdsman serve: webhooks on http://{}/hooks/github", h.local_addr()?);
                sc.spawn(move || listen(ctx, r, &h));
            }
            Some(Err((a, e))) => eprintln!("herdsman serve: no webhooks: {a}: {e}"),
            None => {}
        }
        crate::serve::serve(ctx, r, addr, read_only)
    })
}

/// Serve `l` for good: a thread per delivery, at most 4 at once (then 503).
pub fn listen(ctx: &Ctx, r: &dyn Runner, l: &TcpListener) {
    let busy = &AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for mut s in l.incoming().flatten() {
            if busy.fetch_add(1, Relaxed) >= 4 {
                busy.fetch_sub(1, Relaxed);
                drop(s.write_all(&status(503, "busy")));
                continue;
            }
            sc.spawn(move || {
                let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
                let res = reply(ctx, r, &mut s);
                drop(s.write_all(&res));
                busy.fetch_sub(1, Relaxed);
            });
        }
    });
}

/// The response to the one request read from `s`. A body over GitHub's 25 MB is refused from its `Content-Length`
/// before any of it is read.
pub fn reply(ctx: &Ctx, r: &dyn Runner, s: &mut dyn Read) -> Vec<u8> {
    let (mut buf, mut chunk) = (vec![], [0u8; 8192]);
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        match s.read(&mut chunk) {
            Ok(n) if n > 0 && buf.len() < 16_384 => buf.extend_from_slice(&chunk[..n]),
            _ => return status(400, "bad request"),
        }
    };
    let head = String::from_utf8_lossy(&buf[..end]).into_owned();
    let mut w = head.split_whitespace();
    let (method, path) = (w.next().unwrap_or_default(), w.next().unwrap_or_default());
    if path != "/hooks/github" {
        return status(404, "not found");
    }
    if method != "POST" {
        return status(405, "POST only");
    }
    let Some(n) = header(&head, "content-length").and_then(|n| n.parse::<usize>().ok()) else {
        return status(411, "length required");
    };
    if n > webhook::MAX_BODY {
        return status(413, "over GitHub's 25 MB");
    }
    let mut body = buf.split_off(end + 4);
    body.truncate(n);
    let left = (n - body.len()) as u64;
    if s.take(left).read_to_end(&mut body).is_err() || body.len() < n {
        return status(400, "short body");
    }
    let ctx = Ctx {
        now: crate::now(),
        ..ctx.clone()
    };
    let (event, sig) = (header(&head, "x-github-event"), header(&head, "x-hub-signature-256"));
    let (code, msg) = webhook::deliver(&ctx, r, event.unwrap_or_default(), sig, &body);
    status(code, &msg)
}

fn status(code: u16, msg: &str) -> Vec<u8> {
    let why = match code {
        200 => "OK",
        202 => "Accepted",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        503 => "Service Unavailable",
        _ => "Bad Request",
    };
    let len = msg.len() + 1;
    format!("HTTP/1.1 {code} {why}\r\nContent-Type: text/plain\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{msg}\n")
        .into_bytes()
}

fn header<'h>(head: &'h str, name: &str) -> Option<&'h str> {
    head.lines().skip(1).find_map(|l| {
        l.split_once(':')
            .filter(|(k, _)| k.trim().eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim())
    })
}
