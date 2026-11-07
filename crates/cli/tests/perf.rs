//! #337: the board's speed on a fixture of 30 lanes on 2 hosts (mini, this host, and bazzite, remote), every remote
//! call 300 ms slow: a step makes at most one remote call per host, and the page, the scene data, `status` and the
//! first paint in WebKit stay under their targets (hard: a miss fails). The numbers go to `$GITHUB_STEP_SUMMARY`.
//! #351: every timing goes through the real server (`serve_on` on a socket, its sampler and watcher threads up, this
//! fixture's runner), never a hand-built page: a call-count probe on the runner proves it; and a steady-state `/live`
//! change is the changed part's data only (#269: JSON the board draws), under 5 KB. Budgets: docs/performance.md.
mod common;
use common::*;
use herdsman::events::{append, Event};
use herdsman::runner::{Out, Runner};
use serde_json::json;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const NOW: u64 = 1_000_000;
const ADDR: &str = "127.0.0.1:7777";

/// The fixture's runner: a remote call (ssh, `herdr --machine`) sleeps 300 ms and is counted by host; every call
/// counts in the probe (`.2`).
struct Slow(Fake, Mutex<Vec<String>>, AtomicUsize);
impl Runner for Slow {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        self.2.fetch_add(1, Relaxed);
        let host = match prog {
            "ssh" => args.iter().rev().nth(2).cloned(),
            "herdr" if args.first().is_some_and(|a| a == "--machine") => args.get(1).cloned(),
            _ => None,
        };
        if let Some(h) = host {
            std::thread::sleep(Duration::from_millis(300));
            self.1.lock().unwrap().push(h);
        }
        self.0.run(prog, args)
    }
    fn sleep(&self, _: u64) {}
}

/// 30 impl lanes, #1–#15 on mini and #16–#30 on bazzite, each idle with a pane and an unpushed worktree.
fn fixture() -> (herdsman::Ctx, Slow) {
    let base = CONFIG.replace("harness  =  \"hermes\"", "harness = \"claude\"");
    let cfg = format!("{base}\n[hosts.mini]\n\n[hosts.bazzite]\nmachine = \"bazzite\"\n");
    let c = ctx(&tmpdir("perf"), &cfg);
    let st = c.state.parent().unwrap();
    std::fs::create_dir_all(st).unwrap();
    let seen = json!({"mini": {"home": "/Users/me", "probed": NOW, "seen": NOW, "up": true},
                      "bazzite": {"home": "/home/me", "probed": NOW, "seen": NOW, "up": true}});
    std::fs::write(st.join("hosts.json"), seen.to_string()).unwrap();
    let (mut mini, mut bazzite) = (vec![], vec![]);
    for n in 1..=30u64 {
        let remote = n > 15;
        let wt = if remote {
            format!("/home/me/.herdr/worktrees/sendmeter/issue-{n}")
        } else {
            c.worktrees
                .join(format!("sendmeter/issue-{n}"))
                .to_string_lossy()
                .into()
        };
        let agent = format!("sendmeter-impl-{n}");
        let mut d = json!({"role": "impl", "agent": agent, "harness": "claude", "pane": format!("w{n}:p1"),
                           "workspace": format!("w{n}"), "worktree": wt, "branch": format!("issue-{n}")});
        if remote {
            (d["host"], d["machine"]) = (json!("bazzite"), json!("bazzite"));
        }
        append(&c.state, &Event::new(NOW - 900, "sendmeter", Some(n), "spawn", d)).unwrap();
        let a = json!({"name": agent, "agent_status": "idle", "pane_id": format!("w{n}:p1"), "cwd": wt});
        if remote { &mut bazzite } else { &mut mini }.push(a);
    }
    let list = |a: &[serde_json::Value]| json!({"result": {"agents": a}}).to_string();
    let f = Fake::default()
        .reply("herdr agent list", &list(&mini))
        .reply("herdr --machine bazzite agent list", &list(&bazzite))
        .reply("herdr workspace list", r#"{"result":{"workspaces":[]}}"#)
        .reply("herdr pane read", "⏺ Working on it\n\n❯ \n")
        .reply("herdr --machine bazzite pane read", "⏺ Working on it\n\n❯ \n")
        .reply("git -C", &format!("{} {}\n", "a".repeat(40), NOW - 600))
        // #389: no lane has queued anything for the main host.
        .reply("ssh -oBatchMode=yes -oConnectTimeout=10 -oControlMaster=auto -oControlPath=/tmp/herdsman-ssh-%C -oControlPersist=10m bazzite -- 'sh' '-c' 'd=", "")
        .reply("ssh", &format!("{} {}\n", "a".repeat(40), NOW - 600));
    (c, Slow(f, Mutex::default(), AtomicUsize::new(0)))
}

fn ms(t: Instant) -> u128 {
    t.elapsed().as_millis()
}

/// `serve_on` (the real server: socket, sampler, watcher) on this fixture, as `herdsman serve` runs it; its address.
fn serve(c: herdsman::Ctx, r: Slow) -> (&'static herdsman::Ctx, &'static Slow, std::net::SocketAddr) {
    let (c, r): (&'static herdsman::Ctx, &'static Slow) = (Box::leak(Box::new(c)), Box::leak(Box::new(r)));
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || herdsman::serve::serve_on(c, r, l, false));
    (c, r, addr)
}

/// One GET over the socket: (ms, response bytes).
fn get(addr: std::net::SocketAddr, path: &str) -> (u128, usize) {
    let (t, text) = fetch(addr, path);
    (t, text.len())
}

/// One GET over the socket: (ms, response text).
fn fetch(addr: std::net::SocketAddr, path: &str) -> (u128, String) {
    let t = Instant::now();
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
        .unwrap();
    let mut res = vec![];
    s.read_to_end(&mut res).unwrap();
    let text = String::from_utf8_lossy(&res);
    assert!(
        text.starts_with("HTTP/1.1 200"),
        "{path}: {}",
        &text[..text.len().min(300)]
    );
    (ms(t), text.into_owned())
}

/// What a `/live` stream sends within `ms`.
fn drain(s: &mut std::net::TcpStream, ms: u64) -> String {
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

/// Appends `line` to the job summary on CI (the numbers on the PR's checks page), and prints it.
fn report(line: &str) {
    eprintln!("{line}");
    if let Ok(p) = std::env::var("GITHUB_STEP_SUMMARY") {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).create(true).open(p).unwrap();
        writeln!(f, "{line}").unwrap();
    }
}

#[test]
fn a_step_makes_one_remote_call_per_host_and_requests_make_none() {
    let (c, r) = fixture();
    let t = Instant::now();
    let (code, out) = run(&c, &r, &["drive", "--fast"]);
    let step = ms(t);
    assert_eq!(code, 0, "{out}");
    let calls = std::mem::take(&mut *r.1.lock().unwrap());
    assert!(calls.len() <= 1, "remote calls in one step: {calls:?}");
    assert!(herdsman::snapshot::load(&c).is_some_and(|s| s.reads.keys().any(|k| k.contains("issue-20"))));
    let before = r.2.load(Relaxed);
    let (c, r, addr) = serve(c, r);
    get(addr, "/"); // warm, as a phone's second load is
    let (page, size) = get(addr, "/");
    let (herd, _) = get(addr, "/herd.json");
    let (lanes, _) = get(addr, "/api/v1/lanes"); // #269: the Lanes tab's data
                                                 // The probe: these timings ran `serve_on` on this fixture's runner (its renders read through it).
    assert!(r.2.load(Relaxed) > before, "serve_on never called the fixture's runner");
    let t = Instant::now();
    let (code, out) = run(c, r, &["status", "sendmeter"]);
    let status = ms(t);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("as of the step's look"), "{out}");
    let calls = r.1.lock().unwrap().clone();
    assert!(calls.is_empty(), "remote calls on a request or status: {calls:?}");
    report(&format!(
        "#337 perf (30 lanes, 2 hosts, +300 ms per remote call): step {step} ms (drive --fast) · via serve_on's socket: \
         GET / {page} ms ({} KB) · /herd.json {herd} ms · /api/v1/lanes {lanes} ms · status {status} ms (the CLI)",
        size / 1024
    ));
    assert!(page < 150, "GET / took {page} ms (target < 150)");
    assert!(herd < 100, "/herd.json took {herd} ms (target < 100)");
    assert!(status < 300, "status took {status} ms (target < 300)");
    assert!(size < 40 * 1024, "GET / is {size} bytes (target < 40 KB)");
}

/// #337: only what decides something is read: a recorded verdict's file never again, a merged lane's worktree never.
#[test]
fn a_recorded_verdict_and_a_merged_lane_are_never_read() {
    let (c, r) = fixture();
    let rev = "/home/me/.herdr/worktrees/sendmeter/rev-16-r1";
    let d = json!({"role": "rev", "agent": "sendmeter-rev-16-r1", "harness": "claude", "pane": "w40:p1", "workspace": "w40",
                   "worktree": rev, "branch": "rev-16-r1", "sha": "b".repeat(40), "host": "bazzite", "machine": "bazzite"});
    append(&c.state, &Event::new(NOW - 800, "sendmeter", Some(16), "spawn", d)).unwrap();
    let v = json!({"verdict": "FAIL", "sha": "b".repeat(40), "branch": "issue-16"});
    append(&c.state, &Event::new(NOW - 700, "sendmeter", Some(16), "verdict", v)).unwrap();
    append(
        &c.state,
        &Event::new(NOW - 600, "sendmeter", Some(17), "merged", json!({})),
    )
    .unwrap();
    assert_eq!(run(&c, &r, &["drive", "--fast"]).0, 0);
    let keys: Vec<String> = herdsman::snapshot::load(&c).unwrap().reads.into_keys().collect();
    assert!(keys.iter().any(|k| k.contains("issue-18")), "{keys:?}");
    assert!(!keys.iter().any(|k| k.contains("rev-16-r1/verdict.md")), "{keys:?}");
    assert!(!keys.iter().any(|k| k.contains("issue-17'")), "{keys:?}");
}

/// #337: the page carries no tab (#269: the TypeScript board draws each from API v1); API v1's Lanes is the lanes, its
/// needs cards the cards only.
#[test]
fn the_page_carries_no_tab_and_each_part_is_its_own_read() {
    let (c, r) = fixture();
    assert_eq!(run(&c, &r, &["drive", "--fast"]).0, 0);
    let s = herdsman::serve::Server::new(&c, &r, Some(ADDR.into()));
    let body = |path: &str, extra: &str| {
        let res = s.respond(&format!("GET {path} HTTP/1.1\r\nHost: {ADDR}\r\n{extra}\r\n"), NOW);
        String::from_utf8_lossy(&res).into_owned()
    };
    let page = body("/", "Cookie: tab=lanes\r\n");
    assert!(
        page.contains("<div id=needs></div>")
            && page.contains("<section id=queue></section><section id=lanes></section><section id=chat></section>")
            && !page.contains("sendmeter-impl-20"),
        "{page}"
    );
    assert!(body("/api/v1/lanes", "").contains("\"name\":\"sendmeter-impl-20\""));
    let needs = body("/api/v1/needs", "");
    assert!(
        needs.contains("\"asks\":[") && !needs.contains("sendmeter-impl-20"),
        "{needs}"
    );
}

/// #450: no full-screen loader: one strip right under the header holds the loading mark (the chat's `.tld`), its word and
/// the age; API v1's needs carry the step's look as a time for it (`snap`), never the old `as of the step's look` box.
#[test]
fn the_loader_and_the_age_are_one_strip_under_the_header() {
    let (c, r) = fixture();
    assert_eq!(run(&c, &r, &["drive", "--fast"]).0, 0);
    let s = herdsman::serve::Server::new(&c, &r, Some(ADDR.into()));
    let res = s.respond(&format!("GET / HTTP/1.1\r\nHost: {ADDR}\r\n\r\n"), NOW + 600);
    let page = String::from_utf8_lossy(&res);
    let (head, rest) = page.split_once("</header>").unwrap();
    assert!(!head.contains("id=stale") && !head.contains("id=ld"), "{head}");
    let strip = rest
        .split_once("<div id=strip>")
        .unwrap()
        .1
        .split_once("</div>")
        .unwrap()
        .0;
    assert!(rest.starts_with("<i id=ptr hidden>↻</i><div id=strip>"), "{rest}");
    assert!(
        strip.starts_with("<span id=ld class=tld hidden aria-hidden=true><svg class=lds"),
        "{strip}"
    );
    assert!(
        strip.ends_with("<span class=ldw>loading</span><span id=stale hidden></span>"),
        "{strip}"
    );
    assert_eq!(page.matches("id=ld ").count(), 1);
    let res = s.respond(
        &format!("GET /api/v1/needs HTTP/1.1\r\nHost: {ADDR}\r\n\r\n"),
        NOW + 600,
    );
    let needs = String::from_utf8_lossy(&res);
    let ts = herdsman::snapshot::load(&c).unwrap().ts;
    assert!(
        ts > 0 && needs.contains(&format!("\"snap\":{ts},")) && !needs.contains("\"age\""),
        "{needs}"
    );
    assert!(!page.contains("as of the step's look"));
}

/// #337: the first contentful paint in WebKit (warm: the second load in one WebKit, its caches and service worker up;
/// the cold one, a new WebKit process, is reported) of the page a real board serves from the snapshot, under 300 ms. Runs where WebKit is at hand (the macOS CI job).
#[test]
fn the_first_paint_in_webkit_is_under_300ms() {
    if std::process::Command::new("swift").arg("--version").output().is_err() || !cfg!(target_os = "macos") {
        eprintln!("skipping: no swift/WebKit here");
        return;
    }
    let (c, r) = fixture();
    assert_eq!(run(&c, &r, &["drive", "--fast"]).0, 0);
    let base = format!("http://{}/", serve(c, r).2);
    let file = tmpdir("perf-fcp").join("page.png");
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/webview.swift");
    // Read after the shot's wait (`window.late`): the entry lands once the frame has painted.
    let fcp = "Object.defineProperty(window, 'late', {get: () => [(performance.getEntriesByName('first-contentful-paint')[0] || {startTime: -1}).startTime]})";
    let paint = |warm: bool| {
        let mut sw = swift();
        if warm {
            sw.env("HERDSMAN_WARM", "1");
        }
        let o = sw
            .args([script, &base, "390", "-", "horse"])
            .arg(&file)
            .arg(fcp)
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout).into_owned();
        let v: Vec<f64> = out.split_whitespace().filter_map(|n| n.parse().ok()).collect();
        assert!(
            o.status.success() && v.len() == 3 && v[2] >= 0.0,
            "{out} {}",
            String::from_utf8_lossy(&o.stderr)
        );
        v[2]
    };
    let (cold, warm) = (paint(false), paint(true));
    report(&format!(
        "#337 perf: first contentful paint in WebKit {warm:.0} ms warm ({cold:.0} ms cold)"
    ));
    assert!(warm < 300.0, "first contentful paint {warm:.0} ms (target < 300)");
}

/// #351: after a part's first copy on a stream, `/live` sends only what changed: an ask is one `needs` (#269: the
/// cards' data, the TypeScript board draws it; #266: Events, the old probe, is the board's), no Lanes and no [chat],
/// under 5 KB, on the 30-lane board served by `serve_on`.
#[test]
fn a_steady_state_live_change_is_one_part_under_5kb() {
    let (c, r) = fixture();
    assert_eq!(run(&c, &r, &["drive", "--fast"]).0, 0);
    let ask = |id: String, q: String| json!({"id": id, "question": q, "options": ["yes", "no"], "recommend": null});
    for i in 0..10 {
        let d = ask(format!("s{i}"), format!("seed-{i}?"));
        append(&c.state, &Event::new(NOW - 60, "sendmeter", None, "needs-decision", d)).unwrap();
    }
    let before = r.2.load(Relaxed);
    let (c, r, addr) = serve(c, r);
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.write_all(format!("GET /live HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
        .unwrap();
    s.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    drain(&mut s, 2_500);
    let note = |i: u64| {
        let d = ask(format!("q{i}"), format!("probe-{i}?"));
        append(&c.state, &Event::new(NOW + i, "sendmeter", None, "needs-decision", d)).unwrap();
    };
    note(1);
    let first = drain(&mut s, 2_500);
    assert!(
        first.contains("event: needs\n") && first.contains("probe-1"),
        "the first change: {first}"
    );
    note(2);
    let got = drain(&mut s, 2_500);
    report(&format!(
        "#351 perf: a steady-state /live change is {} bytes (target < 5 KB)",
        got.len()
    ));
    assert!(r.2.load(Relaxed) > before, "serve_on never called the fixture's runner");
    let parts: Vec<&str> = got
        .split("\n\n")
        .filter_map(|m| m.split_once("\nevent: ")?.1.split('\n').next())
        .filter(|k| *k != "ev")
        .collect();
    assert_eq!(parts, ["needs"], "{got}");
    assert!(
        got.contains("probe-2") && !got.contains("sendmeter-impl-"),
        "unchanged lanes stay home: {got}"
    );
    assert!(
        got.len() < 5 * 1024,
        "a steady-state /live change is {} bytes (target < 5 KB)",
        got.len()
    );
    // The board draws the part from that data: the same cards it draws from API v1's `needs` now.
    if std::process::Command::new("node").arg("--version").output().is_err() {
        return eprintln!("skipping the client half: no node here");
    }
    let data = got
        .split("event: needs\ndata: ")
        .nth(1)
        .unwrap()
        .split("\n\n")
        .next()
        .unwrap();
    let mut now = std::net::TcpStream::connect(addr).unwrap();
    now.write_all(format!("GET /api/v1/needs HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
        .unwrap();
    let mut res = String::new();
    now.read_to_string(&mut res).unwrap();
    let drawn = board_js(&[
        (data, "needs.tab(d)"),
        (&res[res.find("\r\n\r\n").unwrap() + 4..], "needs.tab(d)"),
    ]);
    assert!(
        drawn[0].contains("probe-2?") && drawn[0].contains("seed-9?"),
        "{}",
        drawn[0]
    );
    assert_eq!(drawn[0], drawn[1]);
}

/// #447: a 30,000-event log and 200 idle lanes, each with an answered ask, a review round and the step's chatter;
/// ten asks still open.
fn big() -> (herdsman::Ctx, Fake) {
    big_with(200)
}

/// `big` with `lanes` lanes (#560: 20, a busy fleet's).
fn big_with(lanes: u64) -> (herdsman::Ctx, Fake) {
    let c = ctx(&tmpdir("perf-big"), CONFIG);
    let ev = |ts, n, kind, d| serde_json::to_string(&Event::new(ts, "sendmeter", Some(n), kind, d)).unwrap();
    let (mut log, mut agents) = (vec![], vec![]);
    for n in 1..=lanes {
        let (agent, t) = (format!("sendmeter-impl-{n}"), NOW - 9_000 + n);
        let d = json!({"role": "impl", "agent": agent, "pane": format!("w{n}:p1"), "worktree": format!("/wt/{agent}")});
        log.push(ev(t, n, "spawn", d));
        let ask = json!({"id": format!("a{n}"), "agent": agent, "question": "?", "options": [], "recommend": null});
        log.push(ev(t + 1, n, "needs-decision", ask));
        if n > 10 {
            log.push(ev(t + 2, n, "decision", json!({"id": format!("a{n}"), "answer": "1"})));
        }
        log.push(ev(t + 3, n, "pushed", json!({"sha": "a".repeat(40)})));
        let rev = format!("sendmeter-rev-{n}-r1");
        let d = json!({"role": "rev", "agent": rev, "pane": format!("w{n}:p2"), "worktree": format!("/wt/{rev}"), "sha": "a".repeat(40)});
        log.push(ev(t + 4, n, "spawn", d));
        log.push(ev(
            t + 5,
            n,
            "verdict",
            json!({"verdict": "FAIL", "sha": "a".repeat(40)}),
        ));
        agents.push(json!({"name": agent, "agent_status": "idle", "pane_id": format!("w{n}:p1"), "cwd": format!("/wt/{agent}")}));
    }
    for i in 0..30_000 - log.len() as u64 {
        log.push(ev(
            NOW - 5_000 + i / 10,
            1 + i % lanes,
            "driven",
            json!({"step": "review", "code": "EXIT", "result": "exit 1: boom"}),
        ));
    }
    std::fs::create_dir_all(c.state.parent().unwrap()).unwrap();
    std::fs::write(&c.state, log.join("\n") + "\n").unwrap();
    let f = Fake::default()
        .reply("herdr agent list", &json!({"result": {"agents": agents}}).to_string())
        .reply("herdr workspace list", r#"{"result":{"workspaces":[]}}"#)
        .reply("herdr pane read", "⏺ Working on it\n\n❯ \n")
        .reply("git -C", &format!("{} {}\n", "a".repeat(40), NOW - 600));
    (c, f)
}

/// #447: the waits are linear in the log: `snapshot::waits` under 200 ms, and the Lanes tab's data
/// (`/api/v1/lanes`, `/herd.json`, served from the step's snapshot) under 300 ms on 30,000 events and 200 lanes, in a release build (CI's perf step; a debug build
/// reports only).
#[test]
fn waits_and_the_lanes_tab_stay_fast_on_a_30k_event_log() {
    let (c, r) = big();
    let cfg = herdsman::config::Config::load(&c.config).unwrap();
    let events = herdsman::events::read(&c.state).unwrap();
    assert_eq!(events.len(), 30_000);
    let list = herdsman::herdr::list(&r, "agent").unwrap();
    let st = herdsman::events::fold(&events);
    let all = herdsman::snapshot::lanes(&cfg, &r, &c, (&events, &st), &list);
    assert_eq!(all.len(), 200);
    let t = Instant::now();
    let w = herdsman::snapshot::waits(&c, &cfg, &r, &events, &all);
    let waits = ms(t);
    let owner: Vec<_> = w
        .iter()
        .filter(|w| w.who == "owner" && w.lane.contains("impl"))
        .collect();
    assert_eq!(owner.len(), 10, "{owner:?}");
    assert_eq!(run(&c, &r, &["drive", "--fast"]).0, 0);
    let mut snap = herdsman::snapshot::load(&c).unwrap();
    assert_eq!(snap.waits.len(), w.len(), "the step saves the waits the board reads");
    // Tag the saved waits: a request that shows the tag read the snapshot instead of scanning the log.
    for w in &mut snap.waits {
        w.what = format!("snaphit {}", w.what);
    }
    herdsman::db::put(&c.state, "board", snap.ts, &snap).unwrap();
    let (_, _, addr) = serve(c, Slow(r, Mutex::default(), AtomicUsize::new(0)));
    let mut lanes = 0;
    for path in ["/api/v1/lanes", "/herd.json"] {
        fetch(addr, path);
        let (t, body) = fetch(addr, path);
        assert!(
            body.contains("snaphit"),
            "{path} worked the waits out again instead of reading the snapshot"
        );
        lanes = lanes.max(t);
    }
    report(&format!(
        "#447 perf (30,000 events, 200 lanes): snapshot::waits {waits} ms · GET /api/v1/lanes, /herd.json {lanes} ms"
    ));
    if cfg!(debug_assertions) {
        return eprintln!("debug build: budgets apply in release (cargo test --release --test perf)");
    }
    assert!(waits < 200, "snapshot::waits took {waits} ms (target < 200)");
    assert!(lanes < 300, "the lanes reads took {lanes} ms (target < 300)");
}

extern "C" {
    fn getrusage(who: i32, usage: *mut [i64; 18]) -> i32;
}

/// #560: This process's user + system CPU time so far, in ms (`getrusage(RUSAGE_SELF)`: two `timeval`s first).
fn cpu_ms() -> f64 {
    let mut u = [0i64; 18];
    assert_eq!(unsafe { getrusage(0, &mut u) }, 0);
    let tv = |s: i64, us: i64| s as f64 * 1e3 + (us as u32) as f64 / 1e3;
    tv(u[0], u[1]) + tv(u[2], u[3])
}

/// #560: the board's reads over a 30,000-event store (20 lanes) cost under 2 s of CPU per 100 requests (about 1 s on the mini, 3.4 s before #560), in a release
/// build (a debug build reports only): each request reads only the events appended since the last, never the whole log.
#[test]
fn a_hundred_board_requests_on_a_30k_event_store_stay_under_the_cpu_budget() {
    let (c, r) = big_with(20);
    assert_eq!(run(&c, &r, &["drive", "--fast"]).0, 0);
    let state = c.state.clone();
    let (_, _, addr) = serve(c, Slow(r, Mutex::default(), AtomicUsize::new(0)));
    let paths = [
        "/api/v1/state",
        "/api/v1/host",
        "/api/v1/queue",
        "/api/v1/needs",
        "/api/v1/lanes",
        "/api/v1/chat",
        "/herd.json",
        "/api/v1/events?since=29990",
        "/",
        "/api/v1/queue",
    ];
    paths.iter().for_each(|p| drop(fetch(addr, p)));
    let t = cpu_ms();
    for i in 0..100 {
        // The fleet keeps logging: every tenth request has a new event to read.
        if i % 10 == 0 {
            append(
                &state,
                &Event::new(
                    NOW,
                    "sendmeter",
                    Some(1),
                    "driven",
                    json!({"step": "review", "result": "ok"}),
                ),
            )
            .unwrap();
        }
        fetch(addr, paths[i % paths.len()]);
    }
    let cpu = cpu_ms() - t;
    report(&format!(
        "#560 perf (30,000 events, 20 lanes): 100 board requests {cpu:.0} ms of CPU"
    ));
    if cfg!(debug_assertions) {
        return eprintln!("debug build: budgets apply in release (cargo test --release --test perf)");
    }
    assert!(cpu < 2000.0, "100 requests took {cpu:.0} ms of CPU (budget 2000)");
}
