mod common;
use common::*;
use drover::events::{append, Event};
use drover::runner::{Out, Runner};
use serde_json::json;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const NOW: u64 = 1_000_000;
const ADDR: &str = "127.0.0.1:7777";

struct Slow(
    Fake,
    Mutex<Vec<String>>,
    AtomicUsize,
    Option<&'static FakeShed>,
    &'static FakeShed,
);
impl Slow {
    fn remote(&self, prog: &str, args: &[String]) {
        self.2.fetch_add(1, Relaxed);
        if let Some(h) = args.iter().rev().nth(2).filter(|_| prog == "ssh") {
            std::thread::sleep(Duration::from_millis(300));
            self.1.lock().unwrap().push(h.clone());
        }
    }
}
impl Runner for Slow {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        self.remote(prog, args);
        self.0.run(prog, args)
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> anyhow::Result<Out> {
        if prog != "ssh" || !args.iter().any(|a| a.contains("'shed' 'connect'")) {
            return self.run(prog, args);
        }
        self.remote(prog, args);
        Ok(Out {
            stdout: self.4.connect(input),
            ..Out::default()
        })
    }
    fn sleep(&self, _: u64) {}
    fn shed(&self) -> Option<drover::shed::Shed> {
        self.3.map(FakeShed::shed)
    }
}

fn shed() -> &'static FakeShed {
    Box::leak(Box::new(FakeShed::new()))
}

fn fixture() -> (drover::Ctx, Slow) {
    let base = CONFIG.replace("harness  =  \"hermes\"", "harness = \"claude\"");
    let cfg = format!("{base}\n[hosts.mini]\n\n[hosts.bazzite]\nmachine = \"bazzite\"\n");
    let c = ctx(&tmpdir("perf"), &cfg);
    let st = c.state.parent().unwrap();
    std::fs::create_dir_all(st).unwrap();
    let seen = json!({"mini": {"home": "/Users/me", "probed": NOW, "seen": NOW, "up": true},
                      "bazzite": {"home": "/home/me", "probed": NOW, "seen": NOW, "up": true}});
    std::fs::write(st.join("hosts.json"), seen.to_string()).unwrap();
    let (mini, bazzite) = (shed(), shed());
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
        let mut d = json!({"role": "impl", "agent": agent, "harness": "claude", "pane": agent, "runtime": "shed",
                           "worktree": wt, "branch": format!("issue-{n}")});
        if remote {
            (d["host"], d["machine"]) = (json!("bazzite"), json!("bazzite"));
        }
        append(&c.state, &Event::new(NOW - 900, "sendmeter", Some(n), "spawn", d)).unwrap();
        let sh = if remote { bazzite } else { mini };
        sh.lane(&agent, "idle", &wt);
        sh.screen(&agent, "⏺ Working on it\n\n❯ \n");
    }
    let f = Fake::default()
        .reply("git -C", &format!("{} {}\n", "a".repeat(40), NOW - 600))
        .reply("ssh -oBatchMode=yes -oConnectTimeout=10 -oControlMaster=auto -oControlPath=/tmp/drover-ssh-%C -oControlPersist=10m bazzite -- 'sh' '-c' 'd=", "")
        .reply("ssh", &format!("{} {}\n", "a".repeat(40), NOW - 600));
    (c, Slow(f, Mutex::default(), AtomicUsize::new(0), Some(mini), bazzite))
}

fn ms(t: Instant) -> u128 {
    t.elapsed().as_millis()
}

fn serve<R: Runner + 'static>(c: drover::Ctx, r: R) -> (&'static drover::Ctx, &'static R, std::net::SocketAddr) {
    let (c, r): (&'static drover::Ctx, &'static R) = (Box::leak(Box::new(c)), Box::leak(Box::new(r)));
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || drover::serve::serve_on(c, r, l, false));
    get(addr, "/sw.js");
    (c, r, addr)
}

fn get(addr: std::net::SocketAddr, path: &str) -> (u128, usize) {
    let (t, text) = fetch(addr, path);
    (t, text.len())
}

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
    assert!(drover::snapshot::load(&c).is_some_and(|s| s.reads.keys().any(|k| k.contains("issue-20"))));
    let before = r.2.load(Relaxed);
    let (c, r, addr) = serve(c, r);
    get(addr, "/");
    r.1.lock().unwrap().clear();
    let (page, size) = get(addr, "/");
    let (herd, _) = get(addr, "/herd.json");
    let (lanes, _) = get(addr, "/api/v1/lanes");
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

#[test]
fn a_recorded_verdict_and_a_merged_lane_are_never_read() {
    let (c, r) = fixture();
    let rev = "/home/me/.herdr/worktrees/sendmeter/rev-16-r1";
    let d = json!({"role": "rev", "agent": "sendmeter-rev-16-r1", "harness": "claude", "pane": "sendmeter-rev-16-r1",
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
    let keys: Vec<String> = drover::snapshot::load(&c).unwrap().reads.into_keys().collect();
    assert!(keys.iter().any(|k| k.contains("issue-18")), "{keys:?}");
    assert!(!keys.iter().any(|k| k.contains("rev-16-r1/verdict.md")), "{keys:?}");
    assert!(!keys.iter().any(|k| k.contains("issue-17'")), "{keys:?}");
}

#[test]
fn the_page_carries_no_tab_and_each_part_is_its_own_read() {
    let (c, r) = fixture();
    assert_eq!(run(&c, &r, &["drive", "--fast"]).0, 0);
    let s = drover::serve::Server::new(&c, &r, Some(ADDR.into()));
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

#[test]
fn the_loader_and_the_age_are_one_strip_under_the_header() {
    let (c, r) = fixture();
    assert_eq!(run(&c, &r, &["drive", "--fast"]).0, 0);
    let s = drover::serve::Server::new(&c, &r, Some(ADDR.into()));
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
    let ts = drover::snapshot::load(&c).unwrap().ts;
    assert!(
        ts > 0 && needs.contains(&format!("\"snap\":{ts},")) && !needs.contains("\"age\""),
        "{needs}"
    );
    assert!(!page.contains("as of the step's look"));
}

#[test]
fn the_first_paint_in_webkit_is_under_300ms() {
    if no_webkit() {
        return;
    }
    let (c, r) = fixture();
    assert_eq!(run(&c, &r, &["drive", "--fast"]).0, 0);
    let base = format!("http://{}/", serve(c, r).2);
    let file = tmpdir("perf-fcp").join("page.png");
    let fcp = "Object.defineProperty(window, 'late', {get: () => new Promise(r => { const t = Date.now(), f = () => { const e = performance.getEntriesByName('first-contentful-paint')[0]; if (e || Date.now() - t > 5000) r([e ? e.startTime : -1]); else setTimeout(f, 20); }; f(); })})";
    let paint = |warm: bool| {
        let mut sw = webview();
        if warm {
            sw.env("DROVER_WARM", "1");
        }
        let o = sw
            .args([&base, "390", "-", "horse"])
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

fn big() -> (drover::Ctx, Fake, &'static FakeShed) {
    big_with(200)
}

fn big_with(lanes: u64) -> (drover::Ctx, Fake, &'static FakeShed) {
    let c = ctx(&tmpdir("perf-big"), CONFIG);
    let sh = shed();
    sh.attach(&c);
    let ev = |ts, n, kind, d| serde_json::to_string(&Event::new(ts, "sendmeter", Some(n), kind, d)).unwrap();
    let mut log = vec![];
    for n in 1..=lanes {
        let (agent, t) = (format!("sendmeter-impl-{n}"), NOW - 9_000 + n);
        let d = json!({"role": "impl", "agent": agent, "pane": agent, "runtime": "shed", "worktree": format!("/wt/{agent}")});
        log.push(ev(t, n, "spawn", d));
        let ask = json!({"id": format!("a{n}"), "agent": agent, "question": "?", "options": [], "recommend": null});
        log.push(ev(t + 1, n, "needs-decision", ask));
        if n > 10 {
            log.push(ev(t + 2, n, "decision", json!({"id": format!("a{n}"), "answer": "1"})));
        }
        log.push(ev(t + 3, n, "pushed", json!({"sha": "a".repeat(40)})));
        let rev = format!("sendmeter-rev-{n}-r1");
        let d = json!({"role": "rev", "agent": rev, "pane": rev, "runtime": "shed", "worktree": format!("/wt/{rev}"), "sha": "a".repeat(40)});
        log.push(ev(t + 4, n, "spawn", d));
        log.push(ev(
            t + 5,
            n,
            "verdict",
            json!({"verdict": "FAIL", "sha": "a".repeat(40)}),
        ));
        sh.lane(&agent, "idle", &format!("/wt/{agent}"));
        sh.screen(&agent, "⏺ Working on it\n\n❯ \n");
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
    let f = Fake::default().reply("git -C", &format!("{} {}\n", "a".repeat(40), NOW - 600));
    (c, f, sh)
}

#[test]
fn waits_and_the_lanes_tab_stay_fast_on_a_30k_event_log() {
    let (c, r, sh) = big();
    let cfg = drover::config::Config::load(&c.config).unwrap();
    let events = drover::events::read(&c.state).unwrap();
    assert_eq!(events.len(), 30_000);
    let on = sh.on(&r);
    let list = drover::term::list(&on).unwrap();
    let st = drover::events::fold(&events);
    let all = drover::snapshot::lanes(&cfg, &on, &c, (&events, &st), &list);
    assert_eq!(all.len(), 200);
    let t = Instant::now();
    let w = drover::snapshot::waits(&c, &cfg, &on, &events, &all);
    let waits = ms(t);
    let owner: Vec<_> = w
        .iter()
        .filter(|w| w.who == "owner" && w.lane.contains("impl"))
        .collect();
    assert_eq!(owner.len(), 10, "{owner:?}");
    assert_eq!(run(&c, &r, &["drive", "--fast"]).0, 0);
    let mut snap = drover::snapshot::load(&c).unwrap();
    assert_eq!(snap.waits.len(), w.len(), "the step saves the waits the board reads");
    for w in &mut snap.waits {
        w.what = format!("snaphit {}", w.what);
    }
    drover::db::put(&c.state, "board", snap.ts, &snap).unwrap();
    let (_, _, addr) = serve(c, Slow(r, Mutex::default(), AtomicUsize::new(0), None, shed()));
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

fn cpu_ms() -> f64 {
    let mut u = [0i64; 18];
    assert_eq!(unsafe { getrusage(0, &mut u) }, 0);
    let tv = |s: i64, us: i64| s as f64 * 1e3 + (us as u32) as f64 / 1e3;
    tv(u[0], u[1]) + tv(u[2], u[3])
}

#[test]
fn a_hundred_board_requests_on_a_30k_event_store_stay_under_the_cpu_budget() {
    let (c, r, _sh) = big_with(20);
    assert_eq!(run(&c, &r, &["drive", "--fast"]).0, 0);
    let state = c.state.clone();
    let (_, _, addr) = serve(c, Slow(r, Mutex::default(), AtomicUsize::new(0), None, shed()));
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

const FLEETS: [&str; 5] = ["sendmeter", "alpha", "bravo", "charlie", "delta"];

fn huge() -> (drover::Ctx, Fake) {
    let sh = shed();
    let mut cfg = CONFIG.to_string();
    for f in &FLEETS[1..] {
        cfg += &format!(
            "\n[fleet.{f}]\nrepo = \"o/{f}\"\nlocal = \"/tmp/does-not-matter/{f}\"\nintegration = \"staging\"\n\
             [fleet.{f}.role.impl]\nharness = \"claude\"\n[fleet.{f}.role.rev]\nharness = \"claude\"\n"
        );
    }
    let c = ctx(&tmpdir("perf-huge"), &cfg);
    sh.attach(&c);
    let mut log = vec![];
    let ev = |ts, f: &str, n: Option<u64>, kind, d| serde_json::to_string(&Event::new(ts, f, n, kind, d)).unwrap();
    for f in FLEETS {
        log.push(ev(
            NOW - 9_500,
            f,
            None,
            "queue",
            json!({"op": "add", "issues": (1..=40).collect::<Vec<u64>>()}),
        ));
        for t in 1..=10 {
            let id = format!("{f}-t{t}");
            log.push(ev(
                NOW - 9_400,
                f,
                None,
                "thread-new",
                json!({"thread": id, "title": id}),
            ));
            for m in 0..100 {
                let from = if m % 2 == 0 { "owner" } else { "orchestrator" };
                log.push(ev(
                    NOW - 9_300 + m,
                    f,
                    None,
                    "thread-msg",
                    json!({"thread": id, "from": from, "text": "hi"}),
                ));
            }
        }
        for n in 1..=40u64 {
            let (agent, t) = (format!("{f}-impl-{n}"), NOW - 9_000 + n);
            let d = json!({"role": "impl", "agent": agent, "pane": agent, "runtime": "shed", "worktree": format!("/wt/{agent}")});
            log.push(ev(t, f, Some(n), "spawn", d));
            let mut ask = json!({"id": format!("{f}-q{n}"), "agent": agent, "question": "?", "options": ["yes", "no"], "recommend": 1});
            if n % 4 == 0 {
                ask["thread"] = json!(format!("{f}-t{}", n / 4));
            }
            log.push(ev(t + 1, f, Some(n), "needs-decision", ask));
            if n % 4 != 1 {
                log.push(ev(
                    t + 2,
                    f,
                    Some(n),
                    "decision",
                    json!({"id": format!("{f}-q{n}"), "choice": "yes", "by": "owner"}),
                ));
            }
            log.push(ev(t + 3, f, Some(n), "pushed", json!({"sha": "a".repeat(40)})));
            let rev = format!("{f}-rev-{n}-r1");
            let d = json!({"role": "rev", "agent": rev, "pane": rev, "runtime": "shed", "worktree": format!("/wt/{rev}"), "sha": "a".repeat(40)});
            log.push(ev(t + 4, f, Some(n), "spawn", d));
            log.push(ev(
                t + 5,
                f,
                Some(n),
                "verdict",
                json!({"verdict": "FAIL", "sha": "a".repeat(40)}),
            ));
            sh.lane(&agent, "idle", &format!("/wt/{agent}"));
            sh.screen(&agent, "⏺ Working on it\n\n❯ \n");
        }
    }
    for i in 0..150_000 - log.len() as u64 {
        let f = FLEETS[i as usize % 5];
        let d = json!({"step": "review", "code": "EXIT", "result": "exit 1: boom"});
        log.push(ev(NOW - 5_000 + i / 30, f, Some(1 + i % 40), "driven", d));
    }
    std::fs::create_dir_all(c.state.parent().unwrap()).unwrap();
    std::fs::write(&c.state, log.join("\n") + "\n").unwrap();
    let f = Fake::default().reply("git -C", &format!("{} {}\n", "a".repeat(40), NOW - 600));
    (c, f)
}

fn send(addr: std::net::SocketAddr, req: &str) -> String {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut res = vec![];
    drop(s.read_to_end(&mut res));
    String::from_utf8_lossy(&res).into_owned()
}

fn pair_on(addr: std::net::SocketAddr, c: &drover::Ctx) -> String {
    let post = |path: &str, extra: &str| {
        send(
            addr,
            &format!("POST {path} HTTP/1.1\r\nHost: {addr}\r\n{extra}Content-Length: 0\r\n\r\n"),
        )
    };
    let res = post("/pair/request", "User-Agent: perf\r\n");
    let code = res
        .rsplit("\r\n\r\n")
        .next()
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    let set = res.lines().find_map(|l| l.strip_prefix("Set-Cookie: ")).unwrap();
    let cookie = format!("Cookie: {}\r\n", &set[..set.find(';').unwrap()]);
    let now = drover::now();
    let (rc, out) = run(
        &drover::Ctx { now, ..c.clone() },
        &Fake::default(),
        &["serve", "approve", &code],
    );
    assert_eq!(rc, 0, "{out}");
    assert!(post("/pair/claim", &cookie).starts_with("HTTP/1.1 200"));
    cookie
}

fn op(addr: std::net::SocketAddr, cookie: &str, body: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(&api_body(&fetch(addr, "/api/v1/state").1)).unwrap();
    let mtime = v["mtime"].as_str().unwrap();
    let body = format!("{body}&mtime={mtime}");
    let res = send(
        addr,
        &format!(
            "POST / HTTP/1.1\r\nHost: {addr}\r\n{cookie}Content-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    );
    res.lines()
        .find_map(|l| l.strip_prefix("Location: "))
        .unwrap_or(&res)
        .to_string()
}

#[test]
fn the_state_read_stays_under_300ms_on_a_150k_event_log() {
    let live = std::env::var("DROVER_PERF_DB");
    let (c, r) = match &live {
        Ok(db) => {
            let cfg =
                std::env::var("DROVER_PERF_CONFIG").map_or(CONFIG.into(), |p| std::fs::read_to_string(p).unwrap());
            let c = ctx(&tmpdir("perf-live"), &cfg);
            std::fs::create_dir_all(c.state.parent().unwrap()).unwrap();
            std::fs::copy(db, c.state.with_file_name("state.db")).unwrap();
            (c, Fake::default())
        }
        Err(_) => huge(),
    };
    let (c, _, addr) = serve(c, Slow(r, Mutex::default(), AtomicUsize::new(0), None, shed()));
    let first = fetch(addr, "/api/v1/state").0;
    let rebuilt = drover::events::REBUILDS.load(Relaxed);
    let mut t: Vec<u128> = (0..5).map(|_| fetch(addr, "/api/v1/state").0).collect();
    t.sort_unstable();
    let n = drover::events::read(&c.state).unwrap().len();
    report(&format!(
        "#859 perf ({n} events): GET /api/v1/state first {first} ms, runs {t:?} ms, median {} ms",
        t[2]
    ));
    if live.is_ok() {
        return;
    }
    let cookie = pair_on(addr, c);
    for p in [
        "/api/v1/needs",
        "/api/v1/lanes",
        "/api/v1/chat",
        "/api/v1/queue",
        "/api/v1/host",
        "/herd.json",
        "/",
    ] {
        let res = send(addr, &format!("GET {p} HTTP/1.1\r\nHost: {addr}\r\n{cookie}\r\n"));
        assert!(res.starts_with("HTTP/1.1 200"), "{p}: {}", &res[..res.len().min(200)]);
    }
    assert_eq!(
        drover::events::REBUILDS.load(Relaxed),
        rebuilt,
        "a board read built an ask index from a copied slice of the log"
    );
    let asks = |body: &str| -> Vec<String> {
        let v: serde_json::Value = serde_json::from_str(&body[body.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        v["asks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert!(asks(&fetch(addr, "/api/v1/state").1).contains(&"alpha-q1".to_string()));
    let msg = op(addr, &cookie, "op=answer&fleet=alpha&id=alpha-q1&choice=yes");
    assert!(msg.contains("answered"), "{msg}");
    let (after, body) = fetch(addr, "/api/v1/state");
    assert!(
        !asks(&body).contains(&"alpha-q1".to_string()),
        "the answered ask is still listed"
    );
    report(&format!(
        "#859 perf: GET /api/v1/state right after an answer {after} ms"
    ));
    if cfg!(debug_assertions) {
        return eprintln!("debug build: budgets apply in release (cargo test --release --test perf)");
    }
    assert!(t[2] < 300, "/api/v1/state took {} ms (median of 5; target < 300)", t[2]);
}

struct NoSpawn(Slow, Mutex<Vec<String>>);
impl Runner for NoSpawn {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        if drover::runner::in_request() {
            let line = format!("{prog} {}", args.join(" "));
            self.1.lock().unwrap().push(line.clone());
            panic!("#859: `{line}` started on a board request");
        }
        self.0.run(prog, args)
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> anyhow::Result<Out> {
        if drover::runner::in_request() {
            let line = format!("{prog} {}", args.join(" "));
            self.1.lock().unwrap().push(line.clone());
            panic!("#859: `{line}` started on a board request");
        }
        self.0.run_in(prog, args, input)
    }
    fn sleep(&self, _: u64) {}
    fn shed(&self) -> Option<drover::shed::Shed> {
        self.0.shed()
    }
}

#[test]
fn no_board_request_starts_a_process() {
    let (c, r) = fixture();
    let (c, r, addr) = serve(c, NoSpawn(r, Mutex::default()));
    let cookie = pair_on(addr, c);
    let paths = [
        "/",
        "/api/v1/state",
        "/api/v1/health",
        "/api/v1/host",
        "/api/v1/queue",
        "/api/v1/needs",
        "/api/v1/lanes",
        "/api/v1/chat",
        "/api/v1/events",
        "/api/v1/lessons",
        "/api/v1/config",
        "/api/v1/issue/sendmeter/1",
        "/api/v1/lane/sendmeter-impl-1",
        "/api/v1/lane/sendmeter-impl-20",
        "/lane?agent=sendmeter-impl-1",
        "/herd.json",
        "/screens/queue",
        "/screens/cards",
        "/screens/events",
        "/screens/fleets",
        "/pair/status",
    ];
    for p in paths {
        let res = send(addr, &format!("GET {p} HTTP/1.1\r\nHost: {addr}\r\n{cookie}\r\n"));
        assert!(
            res.starts_with("HTTP/1.1 "),
            "{p}: no answer (its handler panicked: {:?})",
            r.1.lock().unwrap()
        );
    }
    assert_eq!(
        *r.1.lock().unwrap(),
        Vec::<String>::new(),
        "processes started on a request"
    );
    let v: serde_json::Value = serde_json::from_str(&api_body(&fetch(addr, "/api/v1/state").1)).unwrap();
    assert_eq!(v["lanes"].as_array().unwrap().len(), 30, "{}", v["lanes"]);
    assert!(v["sources"]["remote"].is_u64(), "{}", v["sources"]);
}

fn api_body(res: &str) -> String {
    res[res.find("\r\n\r\n").unwrap() + 4..].to_string()
}

#[test]
fn a_big_fold_holds_up_no_other_logs_read() {
    let (big, _) = huge();
    let (small, _) = fixture();
    let p = big.state.clone();
    let h = std::thread::spawn(move || drover::events::shared(&p).map(|l| l.len()));
    std::thread::sleep(Duration::from_millis(50));
    let (t, folding) = (Instant::now(), !h.is_finished());
    append(&small.state, &Event::new(NOW, "sendmeter", None, "note", json!({}))).unwrap();
    drover::events::shared(&small.state).unwrap();
    let waited = ms(t);
    assert_eq!(h.join().unwrap().unwrap(), 150_000);
    assert!(
        folding,
        "the big fold ended before the small read began: nothing measured"
    );
    assert!(
        waited < 500,
        "a small log's read waited {waited} ms on another log's fold"
    );
}
