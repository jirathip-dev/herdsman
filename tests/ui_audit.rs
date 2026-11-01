//! #58 amendment B: one terminal-style control system. The `tests/ui/audit.js` console audit on every
//! screen (Queue with rows and roles open, Lanes, Host, Events, the sheet, Herd): zero outliers. 390 px
//! screenshots go to `$HERDSMAN_SNAPSHOTS`.
mod common;
use common::*;
use herdsman::events::{append, Event};
use herdsman::serve::Server;
use serde_json::json;

const ADDR: &str = "127.0.0.1:7777";
const T: u64 = 1_000_000;
const MENU: &str = "⏺ I need to pick a database.\n\n Which database should the meter use?\n\n❯ 1. SQLite\n  2. Postgres, with a long option label that has to ellipsise\n  3. Type something.\n\nEnter to select · ↑/↓ to navigate · Esc to cancel\n";

fn req(s: &Server, method: &str, path: &str, body: &str, extra: &str) -> String {
    String::from_utf8_lossy(&s.respond(
        &format!(
            "{method} {path} HTTP/1.1\r\nHost: {ADDR}\r\n{extra}Content-Length: {}\r\n\r\n{body}",
            body.len()
        ),
        T,
    ))
    .into_owned()
}

#[test]
fn every_control_passes_the_audit_at_390px() {
    if std::process::Command::new("swift").arg("--version").output().is_err() || !cfg!(target_os = "macos") {
        eprintln!("skipping: no swift/WebKit here");
        return;
    }
    let cfg = format!("models = [\"opus\", \"sonnet\", \"claude-haiku-4-5-20251001\"]\n\n{CONFIG}");
    let c = ctx(&tmpdir("ui-audit"), &cfg);
    for (issue, agent, harness) in [(12, "sendmeter-impl-12", "claude"), (13, "sendmeter-impl-13", "claude")] {
        append(
            &c.state,
            &Event::new(
                900_000,
                "sendmeter",
                Some(issue),
                "spawn",
                json!({"role": "impl", "agent": agent, "harness": harness}),
            ),
        )
        .unwrap();
    }
    assert_eq!(
        run(&c, &Fake::default(), &["queue", "sendmeter", "add", "18", "19", "20"]).0,
        0
    );
    let a = |n: &str, st: &str, p: &str| json!({"name": n, "agent_status": st, "pane_id": p, "cwd": "/x"});
    let list = json!({"result": {"agents": [a("sendmeter-impl-12", "working", "w1:p1"), a("sendmeter-impl-13", "blocked", "w1:p2")]}});
    let r = Fake::default()
        .reply("herdr agent list", &list.to_string())
        .reply("herdr pane read", MENU);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let page = req(&s, "GET", "/", "", &pair(&s, &c, "Mozilla/5.0 (iPhone)", T));
    // #60: the parts the live page loads on demand, spliced in as the page shows them once loaded.
    let part = |p: &str| {
        let r = req(&s, "GET", p, "", "");
        r[r.find("\r\n\r\n").unwrap() + 4..].to_string()
    };
    let html = page[page.find("<!doctype").unwrap()..]
        .replace(
            "<summary><span>roles</span></summary></details>",
            &format!(
                "<summary><span>roles</span></summary>{}</details>",
                part("/frag?id=roles&fleet=sendmeter")
            ),
        )
        .replace(
            "<section id=events></section>",
            &format!("<section id=events>{}</section>", part("/frag?id=events")),
        );
    let html = &html;
    assert!(
        html.contains("▲ up") && html.contains("class=pick") && html.contains("<select name=model"),
        "fixture has arrows, a menu and pickers"
    );
    let herd = req(&s, "GET", "/herd.json?weather=calm", "", "");
    let herd = &herd[herd.find("\r\n\r\n").unwrap() + 4..];
    let d = tmpdir("ui-audit-png");
    let file = d.join("page.html");
    std::fs::write(&file, html).unwrap();
    let stub = "window.fetch = async () => new Response('no pane output', {status: 404})";
    let roles =
        "document.querySelector('details.roles').open = true; document.querySelector('details.roles').scrollIntoView()";
    let scenes = [
        // The header (`[↻]` and the horse) is in every tab's frame and audit.
        ("ui-queue", "queue", "body", String::new()),
        ("ui-roles", "queue", "body", roles.to_string()),
        ("ui-lanes", "lanes", "body", String::new()),
        ("ui-host", "host", "body", String::new()),
        ("ui-events", "events", "body", String::new()),
        (
            "ui-sheet",
            "lanes",
            "#sheet",
            "open(document.querySelector('.lane[data-agent=\"sendmeter-impl-12\"]'))".to_string(),
        ),
        ("ui-herd", "lanes", "#herd", format!("herd(true); herdData({herd})")),
        ("ui-chat-open", "lanes", "#sheet", chat_scene(true)),
        ("ui-chat-collapsed", "lanes", "#sheet", chat_scene(false)),
    ];
    let audit = include_str!("ui/audit.js");
    for (name, tab, root, then) in scenes {
        // The audit scrolls and opens rows, so the scene is set up again for the screenshot.
        // #138 (the chat scenes): no sideways scroll inside the chat or a panel, no chat text but 11 or 13 px; the blocks drawn.
        let fit = "const c = document.querySelector('.chat'); window.measure.push(c.scrollWidth - c.clientWidth \
            + [...c.querySelectorAll('pre, .blk')].filter(e => e.scrollWidth > e.clientWidth).length, \
            [...c.querySelectorAll('*')].filter(e => e.childNodes.length && [...e.childNodes].some(n => n.nodeType === 3 && n.textContent.trim()) && !['11px', '13px'].includes(getComputedStyle(e).fontSize)).length, c.querySelectorAll('.blk, .qs').length, c.querySelectorAll('.blk[open]').length)";
        let js = format!("{stub}; {audit}; {then}; window.measure = [audit('{root}')]; tab('{tab}'); {then}; {fit}");
        let png = std::env::var("HERDSMAN_SNAPSHOTS").map_or(d.join(format!("{name}.png")), |p| {
            std::path::Path::new(&p).join(format!("{name}.png"))
        });
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/webview.swift");
        let o = swift()
            .args([
                script,
                file.to_str().unwrap(),
                "390",
                tab,
                "horse",
                png.to_str().unwrap(),
                &js,
            ])
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        let v: Vec<f64> = out.split_whitespace().filter_map(|n| n.parse().ok()).collect();
        assert!(
            o.status.success() && v.len() == 7,
            "{name}: {out} {}",
            String::from_utf8_lossy(&o.stderr)
        );
        if name.starts_with("ui-chat") {
            let open = if name == "ui-chat-open" { 7.0 } else { 0.0 };
            assert_eq!(
                (v[3], v[4], v[5], v[6]),
                (0.0, 0.0, 10.0, open),
                "{name}: chat overflow, text not 11/13 px, blocks, open blocks"
            );
        }
        assert!(
            v[0] <= 390.0 && v[1] == 390.0,
            "{name}: scrollWidth {} at {}",
            v[0],
            v[1]
        );
        assert!(v[2] > 0.0, "{name}: no controls audited");
    }
}
