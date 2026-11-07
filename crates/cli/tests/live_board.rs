//! #334 gate 2: the live board in WebKit against a real `serve` on a socket (fake herdr with a Claude Code screen as
//! herdr prints it: ANSI, CRLF, rules): the chat sheet of a local Claude lane with a session, an Air lane and a lane
//! with no transcript; the ascii frames across a `/live` update.
mod common;
use common::*;
use herdsman::events::{append, Event};
use serde_json::json;
use std::path::Path;

/// A Claude Code pane as `herdr pane read --format ansi` gives it.
fn screen(prompt: &str, reply: &str) -> String {
    let rule = "─".repeat(100);
    [
        "\r",
        "\x1b[0m\x1b[38;2;215;119;87m ▐▛███▜▌\x1b[0m   \x1b[1mClaude Code \x1b[0m\x1b[38;2;153;153;153mv2.1.311\x1b[0m\r",
        "",
        &format!("\x1b[0m\x1b[38;2;153;153;153m❯ \x1b[0m{prompt}\r"),
        "",
        "\x1b[0m\x1b[38;2;255;255;255m⏺\x1b[0m \x1b[1mBash\x1b[0m(cargo test)\r",
        "  ⎿  test result: ok. 12 passed\r",
        "",
        &format!("\x1b[0m\x1b[38;2;255;255;255m⏺\x1b[0m {reply}\r"),
        "",
        "\x1b[38;2;215;119;87m✻\x1b[0m Harmonizing… (7m 5s · ↓ 27.6k tokens)\r",
        "",
        &format!("\x1b[38;2;136;136;136m{rule}\x1b[0m\r"),
        "❯ \r",
        &format!("\x1b[38;2;136;136;136m{rule}\x1b[0m\r"),
        "  herdsman #12 · 7d 53%\r",
        "  ⏵⏵ bypass permissions on (shift+tab to cycle)\r",
    ]
    .join("\n")
}

fn spawn(c: &herdsman::Ctx, ts: u64, issue: u64, agent: &str, machine: Option<&str>) {
    let mut d = json!({"role": "impl", "agent": agent, "harness": "claude", "worktree": format!("/wt/{agent}")});
    if let Some(m) = machine {
        d["machine"] = json!(m);
        d["host"] = json!(m);
    }
    append(&c.state, &Event::new(ts, "sendmeter", Some(issue), "spawn", d)).unwrap();
}

/// A real `serve` with three working Claude lanes: 12 (local, a session transcript under `home`), 13 (on the Air)
/// and 14 (local, no transcript); returns its URL and a paired cookie (`name=value`).
fn board(name: &str, home: &Path) -> (herdsman::Ctx, String, String) {
    let c = ctx(&tmpdir(name), CONFIG);
    spawn(&c, 900_100, 12, "sendmeter-impl-12", None);
    spawn(&c, 900_200, 13, "sendmeter-impl-13", Some("air"));
    spawn(&c, 900_300, 14, "sendmeter-impl-14", None);
    let proj = home.join(".claude/projects/-wt-sendmeter-impl-12");
    std::fs::create_dir_all(&proj).unwrap();
    let said = [
        json!({"type": "user", "timestamp": "2026-11-04T03:00:00Z", "message": {"content": "fix the flaky test"}}),
        json!({"type": "assistant", "timestamp": "2026-11-04T03:01:00Z", "message": {"content": [{"type": "text", "text": "Fixed: the test **passes** now."}]}}),
    ];
    std::fs::write(proj.join("s.jsonl"), said.map(|v| v.to_string()).join("\n") + "\n").unwrap();
    let a = |n: &str, p: &str| json!({"name": n, "agent_status": "working", "pane_id": p, "cwd": "/x"});
    let list = json!({"result": {"agents": [a("sendmeter-impl-12", "w1:p1"), a("sendmeter-impl-13", "w2:p1"), a("sendmeter-impl-14", "w3:p1")]}});
    let r = Fake::default()
        .reply("herdr agent list", &list.to_string())
        .reply("herdr workspace list", r#"{"result":{"workspaces":[]}}"#)
        .reply(
            "herdr pane read w1:p1",
            &screen("fix the flaky test", "Fixed: the test passes now."),
        )
        .reply(
            "herdr pane read w3:p1",
            &screen("add the ascii frames", "Done: two frames per lane."),
        )
        .reply(
            "herdr --machine air pane read",
            &screen("port the sheet", "Ported: the sheet reads the Air."),
        );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let cookie = pair(
        &herdsman::serve::Server::new(&c, &Fake::default(), Some("127.0.0.1:7777".into())),
        &c,
        "Safari",
        now,
    );
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", l.local_addr().unwrap());
    let c2 = c.clone();
    std::thread::spawn(move || herdsman::serve::serve_on(&c2, &r, l, false));
    (c, url, cookie.trim().trim_start_matches("Cookie: ").to_string())
}

fn webkit() -> bool {
    let ok = cfg!(target_os = "macos") && std::process::Command::new("swift").arg("--version").output().is_ok();
    if !ok {
        eprintln!("skipping: no swift/WebKit here");
    }
    ok
}

/// The page at `url` in WebKit, `js` run after `tab(tab); mode(mode)`; `window.late` (numbers) read `wait` s later;
/// `pre` runs at document start.
#[allow(clippy::too_many_arguments)]
fn page(url: &str, cookie: &str, tab: &str, mode: &str, pre: &str, js: &str, wait: &str, png: &Path) -> Vec<f64> {
    let o = swift()
        .env("HERDSMAN_COOKIE", cookie)
        .env("HERDSMAN_WAIT", wait)
        .env("HERDSMAN_PRE", pre)
        .args([
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/webview.swift"),
            url,
            "390",
            tab,
            mode,
            png.to_str().unwrap(),
            js,
        ])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out} {}", String::from_utf8_lossy(&o.stderr));
    out.split_whitespace().skip(2).filter_map(|n| n.parse().ok()).collect()
}

/// `js` once the Lanes rows are in (#337: a tab the page left out loads after it, from `/frags`).
fn rowed(js: &str) -> String {
    format!("(function go() {{ if (!document.querySelector('.lane[data-agent]')) return setTimeout(go, 20); {js} }})()")
}

#[test]
fn live_board_chat_and_ascii_in_webkit() {
    if !webkit() {
        return;
    }
    let _lock = ui_lock();
    let home = tmpdir("home334");
    // Lane::session() reads $HOME/.claude; no other test here reads it.
    std::env::set_var("HOME", &home);
    let (c, url, cookie) = board("live334", &home);
    let shots = std::env::var("HERDSMAN_SNAPSHOTS").map_or_else(|_| tmpdir("live334-png"), std::path::PathBuf::from);
    // Bug 2: each sheet's default view shows ≥ 1 message, the full view ≥ 1 block, no page error, via the real /lane.
    for agent in ["sendmeter-impl-12", "sendmeter-impl-13", "sendmeter-impl-14"] {
        let js = format!(
            "open($('.lane[data-agent=\"{agent}\"]')); setTimeout(() => {{ const m = $$('.chat>.now .blk.me, .chat>.now .blk.ai').length; \
             $('.fv').click(); window.late = [m, $$('.chat>.now .blk').length, errs.length + $$('#lanes .blk').length]; if (errs.length) console.log(errs); }}, 2000)"
        );
        let js = rowed(&js);
        let m = page(
            &url,
            &cookie,
            "lanes",
            "scene",
            "",
            &js,
            "2.5",
            &shots.join(format!("chat-{agent}.png")),
        );
        assert!(
            m.len() == 3 && m[0] >= 1.0 && m[1] >= 1.0 && m[2] == 0.0,
            "{agent}: default msgs, full blocks, errors: {m:?}"
        );
    }
    // Bugs 1 and 3: in ascii mode a working lane's frames change, one always shows (no blank sample: no flash), and no
    // node under the Lanes rows is removed or inserted while a `/live` update (a new lane in the log) lands.
    for (i, theme) in ["farm", "kitchen"].into_iter().enumerate() {
        let js = format!(
            "window.lv = 0; (() => {{ const f0 = frag; frag = (id, h) => {{ if (id === 'lanes') lv++; f0(id, h); }}; \
             const row = () => $('.lane[data-agent=\"sendmeter-impl-12\"]'), asc = row().querySelector('.asc'), an = asc.getAnimations({{subtree: true}}); \
             window.mut = 0; new MutationObserver(ms => ms.forEach(m => {{ if (m.target.closest('.av')) mut++; }})).observe($('#lanes'), {{childList: true, subtree: true, characterData: true}}); \
             const seen = new Set(); window.blank = 0; window.n = 0; const t = setInterval(() => {{ n++; \
               const v = [...asc.children].filter(b => getComputedStyle(b).visibility !== 'hidden').map(b => b.textContent).join('|'); if (v) seen.add(v); else blank++; }}, 40); \
             setTimeout(() => {{ clearInterval(t); const a2 = asc.getAnimations({{subtree: true}}); \
               window.late = [seen.size, blank, n, mut, lv, row().querySelector('.asc') === asc ? 1 : 0, an.length === a2.length && an.every((x, i) => x === a2[i]) ? 1 : 0, '{theme}' === 'farm' ? 1 : 0]; }}, 5000); }})()"
        );
        let js = rowed(&js);
        let c2 = c.clone();
        let later = std::thread::spawn(move || {
            // #269: after the board's first draw of Lanes (the page's own read, then the probe's start).
            std::thread::sleep(std::time::Duration::from_millis(2500));
            spawn(
                &c2,
                900_400 + i as u64,
                15 + i as u64,
                &format!("sendmeter-impl-{}", 15 + i),
                None,
            );
        });
        let m = page(
            &format!("{url}?theme={theme}"),
            &cookie,
            "lanes",
            "ascii",
            "",
            &js,
            "5.5",
            &shots.join(format!("ascii-{theme}.png")),
        );
        later.join().unwrap();
        eprintln!("{theme} ascii: frames, blank, samples, mutations, /live lanes, same node, same animations: {m:?}");
        assert!(m.len() == 8, "{theme}: {m:?}");
        let (frames, blank, mutated, lives, same, kept) = (m[0], m[1], m[3], m[4], m[5], m[6]);
        assert!(
            blank == 0.0 && mutated == 0.0 && same == 1.0 && kept == 1.0 && lives >= 1.0,
            "{theme}: frames, blank, samples, mutations, /live lanes, same node, same animations: {m:?}"
        );
        if theme == "farm" {
            assert!(frames >= 2.0, "farm: the legs move: {m:?}");
        }
    }
}

/// Amendment A.4: the Herd scene paints from the last data this phone saw, never waiting on `/herd.json` (held 3 s here,
/// as a slow host's reads hold it): the first lane horse within 300 ms of the page arriving once that data is kept; the
/// first visit (nothing kept) waits for it. Amendment B: `[↻]` is the plain ↻ in every state, spinning only while it
/// loads (as before #305), no sprite in it, and no two header controls draw the same sprite.
#[test]
fn herd_paints_at_once_and_refresh_is_the_plain_spinner() {
    if !webkit() {
        return;
    }
    let _lock = ui_lock();
    let (_c, url, cookie) = board("herd334", &tmpdir("home-herd334"));
    let shots = std::env::var("HERDSMAN_SNAPSHOTS").map_or_else(|_| tmpdir("herd334-png"), std::path::PathBuf::from);
    let pre = "localStorage.herd = '1'; const f = fetch; window.fetch = (u, o) => String(u).includes('herd.json') ? \
               new Promise(r => setTimeout(r, 3000)).then(() => f(u, o)) : f(u, o); \
               new MutationObserver((_, ob) => { if (document.querySelector('#herd .hh')) { ob.disconnect(); \
                 window.first = performance.now() - performance.getEntriesByType('navigation')[0].responseEnd; } }).observe(document, {childList: true, subtree: true})";
    let js = "setTimeout(() => { if (!sessionStorage.cold) { sessionStorage.cold = String(window.first ?? -1); location.reload(); } \
              else window.late = [+sessionStorage.cold, window.first ?? -1]; }, 3800)";
    let m = page(
        &url,
        &cookie,
        "queue",
        "scene",
        pre,
        js,
        "9",
        &shots.join("herd-kept.png"),
    );
    eprintln!("herd first paint ms (nothing kept, kept): {m:?}");
    assert!(
        m.len() == 2 && m[0] >= 2500.0 && (0.0..300.0).contains(&m[1]),
        "first paint ms: nothing kept, kept: {m:?}"
    );
    for theme in ["farm", "kitchen"] {
        for (state, set) in [("idle", "sync('g')"), ("busy", "busy = true; sync('a')")] {
            let js = format!(
                "themed.then(() => {{ const i = $('#rf').firstChild, spin = () => getComputedStyle(i).animationName === 's' ? 1 : 0; \
                 busy = false; sync('g'); const idle = spin(); busy = true; sync('a'); const load = spin(); {set}; \
                 const uses = [...$$('header button svg use')].filter(u => u.closest('svg').checkVisibility({{visibilityProperty: true}})).map(u => u.getAttribute('href')); \
                 window.late = [$('#rf svg') ? 1 : 0, i.textContent === '↻' ? 1 : 0, idle, load, uses.length - new Set(uses).size]; }})"
            );
            let m = page(
                &format!("{url}?theme={theme}"),
                &cookie,
                "queue",
                "scene",
                "",
                &js,
                "1",
                &shots.join(format!("header-{theme}-{state}.png")),
            );
            assert_eq!(
                m,
                [0.0, 1.0, 0.0, 1.0, 0.0],
                "{theme}/{state}: a sprite in [↻], its ↻, spinning idle, spinning loading, repeated header sprites"
            );
        }
    }
}
