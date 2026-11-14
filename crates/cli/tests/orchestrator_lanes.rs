mod common;
use common::*;
use drover::events::{append, read, Event};
use drover::serve::Server;
use serde_json::{json, Value};

const T: u64 = 1_000_000;

fn setup(name: &str) -> (drover::Ctx, FakeShed) {
    let cfg =
        format!("{CONFIG}\n[fleet.drover]\nrepo = \"o/drover\"\nlocal = \"/p/drover\"\nintegration = \"staging\"\n");
    let c = ctx(&tmpdir(name), &cfg);
    let ev = |ts: u64, fleet: &str, issue: Option<u64>, kind: &str, d: Value| {
        append(&c.state, &Event::new(ts, fleet, issue, kind, d)).unwrap()
    };
    let spawn = json!({"role": "impl", "agent": "drover-impl-5", "harness": "claude", "model": "sonnet"});
    ev(900_500, "drover", Some(5), "spawn", spawn);
    for (id, fleet, title, ts) in [
        ("t6", "drover", "Orchestrator-run design lanes", 900_200),
        ("t7", "all", "Every repo", 900_100),
        ("t8", "drover", "Old chat", 900_000),
    ] {
        ev(ts, fleet, None, "thread-new", json!({"thread": id, "title": title}));
        let agent = format!("orchestrator-{id}");
        let d = json!({"thread": id, "agent": agent, "pane": agent, "harness": "claude", "model": "opus"});
        ev(ts, fleet, None, "orchestrator-start", d);
    }
    ev(
        900_300,
        "drover",
        None,
        "orchestrator-sleep",
        json!({"thread": "t8", "agent": "orchestrator-t8", "why": "idle"}),
    );
    ev(
        900_300,
        "drover",
        None,
        "thread-new",
        json!({"thread": "t9", "title": "Never started"}),
    );
    let b = FakeShed::new();
    b.attach(&c);
    for (name, status) in [
        ("drover-impl-5", "idle"),
        ("orchestrator-t6", "idle"),
        ("orchestrator-t7", "working"),
        ("orchestrator-t99", "idle"),
    ] {
        b.lane(name, status, "/x");
    }
    b.add("side", json!({"fleet": "side-project", "harness": "codex"}));
    b.set("side", |t| {
        t["cwd"] = json!("/Users/me/side");
        t["reported"] = json!({"state": "idle"});
    });
    (c, b)
}

#[test]
fn orchestrators_are_lanes_of_their_repo_first_in_its_group_and_asleep_once_closed() {
    let (c, b) = setup("orchestrator-lanes");
    let agents = drover::term::agents(&b.on(&Fake::default())).unwrap();
    let cfg = drover::config::Config::load(&c.config).unwrap();
    let events = read(&c.state).unwrap();
    let st = drover::events::fold(&events);
    let lanes = drover::lanes::join(&cfg, &Fake::default(), &c.worktrees, &events, &st, &agents);
    let row = |l: &drover::lanes::Lane| {
        let s = |v: &Option<String>| v.clone().unwrap_or_default();
        (
            s(&l.fleet),
            l.name.clone(),
            l.role.clone(),
            l.state.clone(),
            s(&l.model),
            s(&l.thread),
            l.issue,
        )
    };
    let got: Vec<_> = lanes.iter().map(row).collect();
    let r = |f: &str, n: &str, role: &str, st: &str, m: &str, t: &str, i| {
        (f.into(), n.into(), role.into(), st.into(), m.into(), t.into(), i)
    };
    assert_eq!(
        got,
        [
            r("drover", "drover-impl-5", "impl", "idle", "sonnet", "", Some(5)),
            r(
                "drover",
                "Orchestrator-run design lanes · t6",
                "orchestrator",
                "idle",
                "opus",
                "t6",
                None
            ),
            r("all", "Every repo · t7", "orchestrator", "working", "opus", "t7", None),
            r("", "orchestrator-t99", "other", "idle", "", "", None),
            r("", "side", "other", "idle", "", "", None),
            r("drover", "Old chat · t8", "orchestrator", "asleep", "opus", "t8", None),
        ]
    );
    let groups = drover::lanes::group(&lanes, |l| l.since);
    let names = |sec: &str, f: &str| -> Vec<String> {
        let subs = &groups.iter().find(|g| g.0 == sec).unwrap().1;
        subs.iter()
            .find(|s| s.0 == f)
            .unwrap()
            .1
            .iter()
            .map(|l| l.name.clone())
            .collect()
    };
    let idle: Vec<&str> = groups
        .iter()
        .find(|g| g.0 == "idle")
        .unwrap()
        .1
        .iter()
        .map(|s| s.0.as_str())
        .collect();
    assert_eq!(idle, ["drover", "Others"]);
    assert_eq!(
        names("idle", "drover"),
        ["Orchestrator-run design lanes · t6", "Old chat · t8", "drover-impl-5"]
    );
    assert_eq!(names("working", "all repos"), ["Every repo · t7"]);
    assert_eq!(names("idle", "Others"), ["orchestrator-t99", "side"]);
}

#[test]
fn the_lanes_tab_renders_orchestrator_rows_with_their_thread() {
    let (c, b) = setup("orchestrator-lanes-page");
    let f = Fake::default();
    let r = b.on(&f);
    let s = Server::new(&c, &r, None);
    let page = with_queue(
        &String::from_utf8_lossy(&s.respond("GET /?all=1 HTTP/1.1\r\n\r\n", T)),
        &s,
        "",
        T,
    );
    let at = |s: &str| page.find(s).unwrap_or_else(|| panic!("{s} missing"));
    let idle = &page[at("<h2><span class=a>idle (")..];
    let herd = &idle[idle.find("<h3>drover (3)</h3>").unwrap()..];
    assert!(
        herd.starts_with("<h3>drover (3)</h3><div class=lane data-agent=\"Orchestrator-run design lanes · t6\" data-role=orchestrator data-thread=t6>"),
        "{herd}"
    );
    assert!(
        page.contains("<div class=lane data-agent=\"Old chat · t8\" data-role=orchestrator data-gone data-thread=t8>")
    );
    assert!(page.contains("<span class=\"nw m\">asleep"), "asleep reads muted");
    assert!(page.contains(
        "<h3>all repos (1)</h3><div class=lane data-agent=\"Every repo · t7\" data-role=orchestrator data-thread=t7>"
    ));
    let others = &page[at("<h3>Others (2)</h3>")..];
    let others = &others[..others.find("<h2>").unwrap_or(others.len())];
    assert!(
        !others.contains("data-thread") && others.contains("orchestrator-t99"),
        "{others}"
    );
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/serve.css")).unwrap();
    assert!(css.contains(".lane[data-thread][data-gone]>.lr{opacity:.55}"));
}

#[test]
fn a_tap_on_a_orchestrator_row_opens_its_thread_in_the_chat_tab() {
    if no_webkit() {
        return;
    }
    let (c, b) = setup("orchestrator-lanes-tap");
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let at = addr.clone();
    std::thread::spawn(move || {
        let f = Fake::default();
        let r = b.on(&f);
        let s = Server::new(&c, &r, Some(at));
        for mut st in l.incoming().flatten() {
            let mut buf = [0u8; 8192];
            let n = std::io::Read::read(&mut st, &mut buf).unwrap_or(0);
            let _ = std::io::Write::write_all(&mut st, &s.respond(&String::from_utf8_lossy(&buf[..n]), T));
        }
    });
    let js = "$('.lane[data-thread=\"t8\"] .lr').click(); \
        const v = $('#chat .cvp[data-t=\"t8\"]'); \
        window.late = [+(document.documentElement.dataset.tab === 'chat'), +!$('#chat').hidden, +!!v, \
          +(v && getComputedStyle(v).display !== 'none'), +(THR === 't8'), +!!(history.state && history.state.thr === 't8'), \
          +!$('#sheet.on'), errs.length];";
    let png = tmpdir("orchestrator-lanes-png").join("tap.png");
    let o = webview()
        .args([
            &format!("http://{addr}/?all=1"),
            "390",
            "lanes",
            "scene",
            png.to_str().unwrap(),
            js,
        ])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out} {}", String::from_utf8_lossy(&o.stderr));
    let m: Vec<f64> = out.split_whitespace().skip(2).filter_map(|n| n.parse().ok()).collect();
    assert_eq!(m, [1., 1., 1., 1., 1., 1., 1., 0.], "{out}");
}
