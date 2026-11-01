//! #42: the live board: `/live` SSE frags, the client cap, the blocked-prompt parser and
//! answering a blocked Claude menu from the needs-you card.
mod common;
use common::*;
use herdsman::events::{append, read, Event};
use herdsman::lanes::pending;
use herdsman::serve::Server;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

const ADDR: &str = "127.0.0.1:7777";
const T: u64 = 1_000_000;

/// A server on a real socket (fake herdr: no agents); returns its address.
fn live_server(name: &str) -> (herdsman::Ctx, std::net::SocketAddr) {
    let c = ctx(&tmpdir(name), CONFIG);
    append(&c.state, &Event::new(T, "sendmeter", None, "note", json!({}))).unwrap();
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let c2 = c.clone();
    std::thread::spawn(move || {
        herdsman::serve::serve_on(&c2, &Fake::default().reply("herdr agent list", NO_AGENTS), l, false)
    });
    (c, addr)
}

fn subscribe(addr: std::net::SocketAddr, host: &str) -> TcpStream {
    let mut s = TcpStream::connect(addr).unwrap();
    s.write_all(format!("GET /live HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes())
        .unwrap();
    s.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    s
}

/// What the stream sends within `ms`.
fn drain(s: &mut TcpStream, ms: u64) -> String {
    let (end, mut out, mut b) = (Instant::now() + Duration::from_millis(ms), vec![], [0u8; 65_536]);
    while Instant::now() < end {
        match s.read(&mut b) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&b[..n]),
            Err(_) => {}
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[test]
fn live_pushes_one_frag_per_changed_section() {
    let (c, addr) = live_server("live");
    let mut s = subscribe(addr, &addr.to_string());
    let hello = drain(&mut s, 1_500);
    assert!(
        hello.starts_with("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n") && hello.ends_with(": ping\n\n"),
        "{hello}"
    );
    assert!(!hello.contains("event:"), "nothing changed yet: {hello}");
    let quiet = drain(&mut s, 2_500);
    assert!(
        quiet.lines().all(|l| l.is_empty() || l == ": ping"),
        "no change: pings only: {quiet}"
    );
    append(&c.state, &Event::new(T + 5, "sendmeter", None, "note", json!({}))).unwrap();
    let t0 = Instant::now();
    let mut got = String::new();
    while !got.contains("\n\n") && t0.elapsed() < Duration::from_millis(1_500) {
        got += &drain(&mut s, 50);
    }
    assert!(
        t0.elapsed() < Duration::from_millis(1_500),
        "a frag within 1.5 s: {got}"
    );
    got += &drain(&mut s, 1_200);
    let frags: Vec<&str> = got.lines().filter(|l| l.starts_with("data: ")).collect();
    assert_eq!(got.matches("event: frag").count(), 1, "{got}");
    let v: serde_json::Value = serde_json::from_str(&frags[0][6..]).unwrap();
    assert_eq!(v["id"], "events", "{v}");
    assert!(v["html"].as_str().unwrap().contains("Last 50 events"));
}

#[test]
fn live_caps_clients_and_checks_host() {
    let (_c, addr) = live_server("cap");
    let mut foreign = subscribe(addr, "evil.example");
    assert!(drain(&mut foreign, 500).starts_with("HTTP/1.1 403 "));
    let mut held: Vec<TcpStream> = (0..8).map(|_| subscribe(addr, &addr.to_string())).collect();
    for s in &mut held {
        assert!(drain(s, 300).starts_with("HTTP/1.1 200 OK"));
    }
    let mut ninth = subscribe(addr, &addr.to_string());
    assert!(drain(&mut ninth, 500).starts_with("HTTP/1.1 503 "), "the 9th client");
    drop(held);
    std::thread::sleep(Duration::from_millis(1_500));
    let mut again = subscribe(addr, &addr.to_string());
    assert!(
        drain(&mut again, 300).starts_with("HTTP/1.1 200 OK"),
        "closed clients free their slots"
    );
    let all = TcpStream::connect(addr).and_then(|mut s| {
        s.write_all(format!("GET /?frag=all HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())?;
        let mut b = String::new();
        s.read_to_string(&mut b).map(|_| b)
    });
    let all = all.unwrap();
    let j: serde_json::Value = serde_json::from_str(&all[all.find("\r\n\r\n").unwrap() + 4..]).unwrap();
    for id in ["needs", "queue", "lanes", "host", "events"] {
        assert!(j[id].is_string(), "{id} in ?frag=all");
    }
}

const MENU: &str = "⏺ I need to pick a database.\n\n Which database should the meter use?\n\n❯ 1. SQLite\n  2. Postgres\n  3. Type something.\n\nEnter to select · ↑/↓ to navigate · Esc to cancel\n";
const PERMISSION: &str = "╭──────────────────────────────────────────╮\n│ Bash command                             │\n│   cargo test --all                       │\n│ Do you want to proceed?                  │\n│ ❯ 1. Yes                                 │\n│   2. Yes, and don't ask again for cargo  │\n│      commands in /p/sendmeter            │\n│   3. No, and tell Claude what to do      │\n│      differently (esc)                   │\n╰──────────────────────────────────────────╯\n";

#[test]
fn prompt_parser_fixtures() {
    assert_eq!(
        pending(MENU),
        Some((
            "Which database should the meter use?".into(),
            vec!["SQLite".into(), "Postgres".into(), "Type something.".into()]
        ))
    );
    let (q, opts) = pending(PERMISSION).unwrap();
    assert_eq!(q, "Do you want to proceed?");
    assert_eq!(
        opts,
        [
            "Yes",
            "Yes, and don't ask again for cargo",
            "No, and tell Claude what to do"
        ]
    );
    assert_eq!(
        pending("⏺ Done with the migration.\n\nShould I also update the README?\n\n> \n"),
        Some(("Should I also update the README?".into(), vec![]))
    );
    assert_eq!(
        pending("dsh> cargo build\n   Compiling herdsman\n    Finished dev\ndsh> "),
        None,
        "dsh pane"
    );
    // A numbered list in Claude's own reply (no `❯` cursor) is not a menu.
    assert_eq!(pending("Plan:\n1. Read the spec\n2. Write the code\n"), None);
    for junk in [
        "",
        "\n\n\n",
        "❯",
        "❯ 1.",
        "1. \n2. ",
        "❯ 99999999999999999999. x\n2. y",
        "\u{0}\u{1b}[2J?",
        "│││",
        "❯ 2. b\n❯ 1. a",
    ] {
        let _ = pending(junk);
    }
    assert_eq!(
        pending("❯ 2. b\n  1. a\nWhich?"),
        Some(("Which?".into(), vec![])),
        "out of order: no menu, never a guess"
    );
}

/// sendmeter-impl-12 (claude) blocked on a permission prompt; sendmeter-rev-9-r1 too.
fn blocked(name: &str, screens: &[&str]) -> (herdsman::Ctx, Fake) {
    let c = ctx(&tmpdir(name), CONFIG);
    for (issue, role, agent) in [(12, "impl", "sendmeter-impl-12"), (9, "rev", "sendmeter-rev-9-r1")] {
        append(
            &c.state,
            &Event::new(
                T - 100,
                "sendmeter",
                Some(issue),
                "spawn",
                json!({"role": role, "agent": agent, "harness": "claude"}),
            ),
        )
        .unwrap();
    }
    let list = agents(&[
        ("sendmeter-impl-12", "blocked", "/x", "w1:p2"),
        ("sendmeter-rev-9-r1", "blocked", "/x", "w1:p3"),
    ]);
    let mut r = Fake::default()
        .reply("herdr agent list", &list)
        .reply("herdr workspace list", "{}")
        .reply("herdr pane send-keys", "");
    for s in screens {
        r = r.once("herdr pane read", 0, s);
    }
    (c, r.reply("herdr pane read", PERMISSION))
}

fn req(s: &Server, method: &str, path: &str, body: &str, extra: &str) -> String {
    let r = format!(
        "{method} {path} HTTP/1.1\r\nHost: {ADDR}\r\n{extra}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    String::from_utf8_lossy(&s.respond(&r, T)).into_owned()
}

/// The `data-h` the needs-you card's menu buttons carry (the menu as the card shows it).
fn shown_h(page: &str) -> String {
    let i = page.find("data-h=").expect("menu buttons") + 7;
    page[i..].chars().take_while(char::is_ascii_digit).collect()
}

fn cookie(s: &Server, c: &herdsman::Ctx) -> String {
    pair(s, c, "", T)
}

#[test]
fn answer_sends_the_digit_and_logs_no_question() {
    let (c, r) = blocked("answer", &[]);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let ck = cookie(&s, &c);
    let page = req(&s, "GET", "/", "", &ck);
    let needs = &page[page.find("<div id=needs>").unwrap()..page.find("<main>").unwrap()];
    let h = shown_h(needs);
    assert!(
        needs.contains("Do you want to proceed?")
            && needs.contains(&format!(
                "<button type=button class=pick value=2 data-h={h}>2. Yes, and don"
            )),
        "{needs}"
    );
    assert!(needs.contains("data-ask=\"sendmeter-impl-12\""), "{needs}");
    let res = req(&s, "POST", "/answer", "agent=sendmeter-impl-12&choice=2", "");
    assert!(res.starts_with("HTTP/1.1 401 "), "no device cookie: {res}");
    assert!(!r.called("herdr pane send-keys"));
    let foreign = req(
        &s,
        "POST",
        "/answer",
        "agent=sendmeter-impl-12&choice=2",
        &format!("{ck}Origin: https://evil.example\r\n"),
    );
    assert!(foreign.starts_with("HTTP/1.1 403 "), "{foreign}");
    for bad in ["0", "4", "12", "2%0A", "Enter"] {
        let res = req(
            &s,
            "POST",
            "/answer",
            &format!("agent=sendmeter-impl-12&choice={bad}"),
            &ck,
        );
        assert!(res.contains("REFUSED BAD_VALUE"), "{bad}: {res}");
    }
    assert!(!r.called("herdr pane send-keys"), "never a guessed key");
    for screen in [PERMISSION, "⏺ Bash(cargo test --all)\n  ⎿ ok"] {
        r.onces.borrow_mut().push((
            "herdr pane read".into(),
            herdsman::runner::Out {
                code: 0,
                stdout: screen.into(),
                stderr: String::new(),
            },
        ));
    }
    let res = req(
        &s,
        "POST",
        "/answer",
        &format!("agent=sendmeter-impl-12&choice=2&h={h}"),
        &ck,
    );
    assert!(
        res.starts_with("HTTP/1.1 200 OK") && res.contains("sent to sendmeter-impl-12"),
        "{res}"
    );
    assert_eq!(r.count("herdr pane send-keys"), 1);
    assert!(r.called("herdr pane send-keys w1:p2 2"));
    let e = read(&c.state).unwrap().pop().unwrap();
    assert_eq!((e.kind.as_str(), e.issue), ("answer", Some(12)));
    assert_eq!(
        serde_json::Value::Object(e.data.clone()),
        json!({"via": "web", "agent": "sendmeter-impl-12", "device": e.str("device").unwrap(), "choice": "2"})
    );
    assert!(
        !std::fs::read_to_string(&c.state).unwrap().contains("proceed"),
        "never the question text"
    );
    // The prompt is still there after the key: "still waiting". A reviewer may answer a permission prompt.
    let res = req(
        &s,
        "POST",
        "/answer",
        &format!("agent=sendmeter-rev-9-r1&choice=1&h={h}"),
        &ck,
    );
    assert!(res.contains("still waiting: sendmeter-rev-9-r1"), "{res}");
    assert!(r.called("herdr pane send-keys w1:p3 1"));
}

/// The card showed menu A; the pane (read afresh, past every cache) now shows menu B: no key, and
/// "menu changed". After a send, the next render reads the pane again (no cached stale menu).
#[test]
fn answer_refuses_a_changed_menu_and_rerenders_fresh() {
    let (c, r) = blocked("stale-menu", &[]);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let ck = cookie(&s, &c);
    let h = shown_h(&req(&s, "GET", "/", "", &ck));
    let b = PERMISSION.replace("for cargo  ", "for git    ");
    r.onces.borrow_mut().push((
        "herdr pane read".into(),
        herdsman::runner::Out {
            code: 0,
            stdout: b,
            stderr: String::new(),
        },
    ));
    let res = req(
        &s,
        "POST",
        "/answer",
        &format!("agent=sendmeter-impl-12&choice=2&h={h}"),
        &ck,
    );
    assert!(res.contains("REFUSED STALE: menu changed — refreshed"), "{res}");
    assert_eq!(
        r.count("herdr pane send-keys"),
        0,
        "menu A tapped, menu B on the pane: no key"
    );
    let res = req(&s, "POST", "/answer", "agent=sendmeter-impl-12&choice=2", &ck);
    assert!(res.contains("REFUSED STALE"), "no h: {res}");
    assert_eq!(r.count("herdr pane send-keys"), 0);
    let reads = r.count("herdr pane read");
    r.onces.borrow_mut().push((
        "herdr pane read".into(),
        herdsman::runner::Out {
            code: 0,
            stdout: PERMISSION.into(),
            stderr: String::new(),
        },
    ));
    r.onces.borrow_mut().push((
        "herdr pane read".into(),
        herdsman::runner::Out {
            code: 0,
            stdout: "⏺ Bash(cargo test --all)\n  ⎿ ok".into(),
            stderr: String::new(),
        },
    ));
    r.onces.borrow_mut().push((
        "herdr pane read".into(),
        herdsman::runner::Out {
            code: 0,
            stdout: "⏺ Bash(cargo test --all)\n  ⎿ ok".into(),
            stderr: String::new(),
        },
    ));
    let res = req(
        &s,
        "POST",
        "/answer",
        &format!("agent=sendmeter-impl-12&choice=2&h={h}"),
        &ck,
    );
    assert!(
        res.contains("sent to sendmeter-impl-12") && r.called("herdr pane send-keys w1:p2 2"),
        "same menu: the digit: {res}"
    );
    // Same second (cache TTLs not expired): the page still reads the pane afresh and drops the old buttons.
    let page = req(&s, "GET", "/", "", &ck);
    assert!(r.count("herdr pane read") > reads + 2, "a fresh read after the send");
    let card = &page
        [page.find("data-ask=\"sendmeter-impl-12\"").unwrap()..page.find("data-ask=\"sendmeter-rev-9-r1\"").unwrap()];
    assert!(!card.contains("class=pick") && card.contains("⎿ ok"), "{card}");
}

#[test]
fn answer_refuses_reviewer_menus_and_unparsed_panes() {
    let (c, r) = blocked("answer-rev", &[MENU, "garbage, no prompt"]);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let ck = cookie(&s, &c);
    let res = req(&s, "POST", "/answer", "agent=sendmeter-rev-9-r1&choice=1", &ck);
    assert!(res.contains("REFUSED REV_READONLY"), "{res}");
    let res = req(&s, "POST", "/answer", "agent=sendmeter-impl-12&choice=1", &ck);
    assert!(res.contains("REFUSED BAD_VALUE: no menu"), "{res}");
    assert!(!r.called("herdr pane send-keys"));
}

#[test]
fn unparsed_blocked_pane_shows_masked_tail_and_reply() {
    let (c, r) = blocked(
        "tail",
        &[concat!(
            "l1\nl2\nl3\nl4\nl5\nl6\nl7 token=gh",
            "p_abcdefghijklmnopqrstuvwxyz0123456789\nl8"
        )],
    );
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let ck = cookie(&s, &c);
    let page = req(&s, "GET", "/", "", &ck);
    let needs = &page[page.find("<div id=needs>").unwrap()..page.find("<main>").unwrap()];
    assert!(
        needs.contains("<pre>l3\nl4") && needs.contains("class=rp>Reply") && !needs.contains("l2\n"),
        "{needs}"
    );
    assert!(
        !needs.contains(concat!("gh", "p_abcdefghijklmnopqrstuvwxyz0123456789")),
        "masked: {needs}"
    );
}

/// The PR's 390 px screenshots (fixture config): the sync dot's three states, a pull in
/// progress, a blocked agent's permission menu on the needs-you card and in its sheet, and
/// the card after an answer; each with no horizontal scroll. Runs where WebKit is at hand;
/// PNGs go to `$HERDSMAN_SNAPSHOTS` when set.
#[test]
fn live_board_fits_390px() {
    if std::process::Command::new("swift").arg("--version").output().is_err() || !cfg!(target_os = "macos") {
        eprintln!("skipping: no swift/WebKit here");
        return;
    }
    let (c, r) = blocked("live-shots", &[]);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let ck = cookie(&s, &c);
    let page = req(&s, "GET", "/", "", &ck);
    let d = tmpdir("live-png");
    let file = d.join("page.html");
    std::fs::write(&file, &page[page.find("<!doctype").unwrap()..]).unwrap();
    let stub =
        "window.fetch = async u => new Response(u === '/answer' ? 'sent to sendmeter-impl-12' : '{}', {status: 200})";
    let scenes = [
        ("sync-green", "queue", "sync('g'); pulse()".to_string()),
        ("sync-amber", "queue", "sync('a')".into()),
        ("sync-red", "queue", "last = Date.now() - 600000; sync('r')".into()),
        ("pull", "queue", "sync('g'); M.style.transform = 'translateY(60px)'; ptr.hidden = false; ptr.style.transform = 'rotate(240deg)'".into()),
        ("needs-you-menu", "queue", "sync('g')".into()),
        ("answer-sent", "queue", format!("sync('g'); {stub}; document.querySelector('.ask .pick[value=\"1\"]').click()")),
        ("sheet-blocked", "lanes", "sync('g'); open(document.querySelector('.lane[data-agent=\"sendmeter-impl-12\"]'))".into()),
    ];
    let dir = std::env::var("HERDSMAN_SNAPSHOTS").ok();
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/webview.swift");
    for (name, tab, js) in scenes {
        let png = dir.as_ref().map_or(d.join(format!("{name}.png")), |p| {
            std::path::Path::new(p).join(format!("{name}.png"))
        });
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
        let w: Vec<u32> = out.split_whitespace().filter_map(|n| n.parse().ok()).collect();
        assert!(
            o.status.success() && w.len() == 2,
            "{name}: {out} {}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(w[0] <= 390 && w[1] == 390, "{name}: scrollWidth {} at {}", w[0], w[1]);
    }
}

/// Pane text (the question, its options, the tail) only reaches a paired device: GET /,
/// `?frag=all` and `/live` show an unpaired viewer who is blocked, the activity and a pairing hint.
#[test]
fn unpaired_viewers_see_no_pane_text() {
    let (c, r) = blocked("unpaired", &[]);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let ck = cookie(&s, &c);
    let pane = [
        "proceed",
        "class=pick",
        "class=rp",
        "cargo test --all",
        "don&#39;t ask",
        "don't ask",
    ];
    for path in ["/", "/?frag=all"] {
        let res = req(&s, "GET", path, "", "");
        assert!(
            pane.iter().all(|p| !res.contains(p)) && res.contains("pair this phone to answer"),
            "{path}: {res}"
        );
        assert!(
            req(&s, "GET", path, "", &ck).contains("Do you want to proceed?"),
            "{path} paired"
        );
    }
    // /live: the blocked lane appears after the watcher's first look; each audience gets its own render.
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let c2 = c.clone();
    std::thread::spawn(move || herdsman::serve::serve_on(&c2, &r.once("herdr agent list", 0, NO_AGENTS), l, false));
    let sub = |extra: &str| {
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(format!("GET /live HTTP/1.1\r\nHost: {addr}\r\n{extra}\r\n").as_bytes())
            .unwrap();
        s.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        s
    };
    let (mut anon, mut dev) = (sub(""), sub(&ck));
    let (a, d) = (drain(&mut anon, 4_000), drain(&mut dev, 500));
    assert!(
        a.contains("\"id\":\"needs\"") && a.contains("pair this phone to answer"),
        "{a}"
    );
    assert!(pane.iter().all(|p| !a.contains(p)), "unpaired stream: {a}");
    assert!(
        d.contains("Do you want to proceed?") && d.contains("class=pick"),
        "paired stream: {d}"
    );
    // The device is removed: its stream closes before the next push, so no more pane text reaches it.
    herdsman::devices::store(&c, &[]).unwrap();
    append(&c.state, &Event::new(T + 5, "sendmeter", None, "note", json!({}))).unwrap();
    let after = drain(&mut dev, 2_500);
    assert!(pane.iter().all(|p| !after.contains(p)), "revoked device: {after}");
    assert!(matches!(dev.read(&mut [0; 1]), Ok(0)), "revoked device: stream closed");
}

/// #57: serve's sampler thread fills the host (`/herd.json`) without a request sampling it.
#[test]
fn the_sampler_thread_fills_the_host() {
    let c = ctx(&tmpdir("sampler"), CONFIG);
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let r = Fake::default()
        .reply("herdr agent list", NO_AGENTS)
        .reply("vm_stat", "Swapouts: 100.\n")
        .reply("sysctl", "4\n");
    std::thread::spawn(move || herdsman::serve::serve_on(&c, &r, l, false));
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(format!("GET /herd.json HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
            .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        if out.contains(r#""pressure":"critical""#) {
            break;
        }
        assert!(Instant::now() < end, "the host never filled: {out}");
        std::thread::sleep(Duration::from_millis(50));
    }
}
