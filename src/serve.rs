//! `herdsman serve`: a read-only HTML view of boards, queues and recent events.
//! std `TcpListener`, one request at a time; the log is re-read per request.
use crate::config::Config;
use crate::events::{fold, plain, read};
use crate::queue;
use crate::Ctx;
use anyhow::Result;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

pub fn serve(ctx: &Ctx, addr: &str) -> Result<()> {
    let l = TcpListener::bind(addr)?;
    eprintln!("herdsman serve: read-only view on http://{}/", l.local_addr()?);
    serve_on(ctx, l)
}

pub fn serve_on(ctx: &Ctx, l: TcpListener) -> Result<()> {
    for s in l.incoming().flatten() {
        let _ = handle(ctx, s);
    }
    Ok(())
}

fn handle(ctx: &Ctx, mut s: TcpStream) -> std::io::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    let (mut buf, mut chunk) = (vec![], [0u8; 2048]);
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16_384 {
        match s.read(&mut chunk)? {
            0 => break,
            n => buf.extend_from_slice(&chunk[..n]),
        }
    }
    s.write_all(respond(ctx, &String::from_utf8_lossy(&buf), crate::now()).as_bytes())
}

/// The full HTTP response to one request. Only `GET /` serves the page.
pub fn respond(ctx: &Ctx, req: &str, now: u64) -> String {
    let mut w = req.split_whitespace();
    let (method, path) = (w.next().unwrap_or_default(), w.next().unwrap_or_default());
    let (status, ctype, body) = if method != "GET" {
        ("405 Method Not Allowed", "text/plain", "read-only: GET only\n".to_string())
    } else if path != "/" && !path.starts_with("/?") {
        ("404 Not Found", "text/plain", "not found\n".to_string())
    } else {
        match page(ctx, now) {
            Ok(b) => ("200 OK", "text/html; charset=utf-8", b),
            Err(e) => ("500 Internal Server Error", "text/plain", format!("{e:#}\n")),
        }
    };
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nAllow: GET\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn page(ctx: &Ctx, now: u64) -> Result<String> {
    let events = read(&ctx.state)?;
    let cfg = Config::load(&ctx.config).ok();
    let repo = |fleet: &str| cfg.as_ref().and_then(|c| c.fleet.get(fleet)).map(|f| f.repo.clone());
    let st = fold(&events);
    let mut h = String::from(
        "<!doctype html><html><head><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\">\
         <meta http-equiv=refresh content=30><title>herdsman board</title><style>\
         :root{color-scheme:light dark}body{font:14px system-ui,sans-serif;margin:16px;max-width:60rem}\
         pre{font:13px ui-monospace,monospace;overflow-x:auto}td{padding:2px 8px 2px 0;vertical-align:top}\
         </style></head><body><h1>fleet</h1>",
    );
    for f in queue::fleets(&events) {
        h += &format!("<h2>{}</h2><pre>{}</pre>", esc(&f), esc(&queue::board(&events, &f, now)));
    }
    h += "<h2>Last 50 events</h2><table>";
    for e in events.iter().rev().take(50) {
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
        h += &format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
            crate::events::age(now.saturating_sub(e.ts)),
            esc(&plain(e)),
            links.join(" ")
        );
    }
    h += "</table></body></html>\n";
    Ok(h)
}
