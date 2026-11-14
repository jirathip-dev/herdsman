mod common;
use common::*;
use drover::devices;
use drover::events::{append, read, Event};
use drover::serve::Server;
use serde_json::json;

const ADDR: &str = "127.0.0.1:7777";

fn config() -> String {
    format!("models = [\"opus\", \"sonnet\"]\n\n[serve]\nallowed_hosts = [\"mac.tail1.ts.net\"]\n\n{CONFIG}")
}

fn get(s: &Server, path: &str) -> String {
    let res = String::from_utf8_lossy(&s.respond(&format!("GET {path} HTTP/1.1\r\nHost: {ADDR}\r\n\r\n"), 1_000_000))
        .into_owned();
    with_queue(&res, s, ADDR, 1_000_000)
}

fn mtime(s: &Server) -> String {
    let page = get(s, "/");
    let i = page.find("name=mtime value=").expect("no mtime field") + 17;
    page[i..].chars().take_while(char::is_ascii_digit).collect()
}

fn post(s: &Server, body: &str, extra: &str) -> String {
    let req = format!(
        "POST / HTTP/1.1\r\nHost: {ADDR}\r\n{extra}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    String::from_utf8_lossy(&s.respond(&req, 1_000_000)).into_owned()
}

fn location(res: &str) -> String {
    let l = res
        .lines()
        .find_map(|l| l.strip_prefix("Location: "))
        .unwrap_or_default()
        .as_bytes();
    let (mut out, mut i) = (vec![], 0);
    while i < l.len() {
        if l[i] == b'%' {
            out.push(u8::from_str_radix(std::str::from_utf8(&l[i + 1..i + 3]).unwrap(), 16).unwrap());
            i += 3;
        } else {
            out.push(l[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

fn last(c: &drover::Ctx) -> (serde_json::Value, Option<String>) {
    let mut e = read(&c.state).unwrap().to_vec().pop().unwrap();
    let via = e.data.remove("via").and_then(|v| v.as_str().map(String::from));
    (json!({"type": e.kind, "fleet": e.fleet, "data": e.data}), via)
}

fn pair(name: &str) -> (drover::Ctx, drover::Ctx) {
    let (a, b) = (tmpdir(&format!("{name}-cli")), tmpdir(&format!("{name}-web")));
    let (a, b) = (ctx(&a, &config()), ctx(&b, &config()));
    for c in [&a, &b] {
        assert_eq!(
            run(c, &Fake::default(), &["queue", "sendmeter", "add", "17", "18", "12"]).0,
            0
        );
    }
    (a, b)
}

fn order(c: &drover::Ctx) -> Vec<u64> {
    drover::queue::replay(&read(&c.state).unwrap(), "sendmeter")
        .iter()
        .map(|i| i.issue)
        .collect()
}

#[test]
fn queue_buttons_move_like_queue_mv() {
    let (cli, web) = pair("mv");
    let r = Fake::default();
    let s = Server::new(&web, &r, Some(ADDR.into()));
    let page = get(&s, "/");
    assert!(
        page.contains("name=before value=18") && page.contains("▲ up"),
        "▲ on #12 targets #18"
    );
    for (args, body, msg) in [
        (
            vec!["mv", "12", "--before", "18"],
            "op=mv&fleet=sendmeter&issue=12&before=18",
            "moved #12 before #18",
        ),
        (
            vec!["mv", "17", "--after", "18"],
            "op=mv&fleet=sendmeter&issue=17&after=18",
            "moved #17 after #18",
        ),
    ] {
        assert_eq!(
            run(&cli, &Fake::default(), &[&["queue", "sendmeter"][..], &args].concat()).0,
            0
        );
        let res = post(&s, &format!("{body}&mtime={}", mtime(&s)), "");
        assert!(res.starts_with("HTTP/1.1 303 See Other\r\n"), "{res}");
        assert_eq!(location(&res), format!("/?msg={msg}"), "{res}");
        assert!(msg.len() <= 60);
        assert_eq!(order(&cli), order(&web));
        assert_eq!(last(&cli), (last(&web).0, None));
        assert_eq!(last(&web).1.as_deref(), Some("web"));
    }
    assert_eq!(order(&web), [12, 18, 17]);
    let res = post(
        &s,
        &format!("op=mv&fleet=sendmeter&issue=99&before=12&mtime={}", mtime(&s)),
        "",
    );
    assert!(
        location(&res).starts_with("/?msg=error: #99 is not queued"),
        "an error is kept: {res}"
    );
    assert!(get(&s, "/").contains("</nav>"), "page still renders");
}

#[test]
fn pause_button_matches_cli_and_blocks_next_and_spawn() {
    let (cli, web) = pair("pause");
    let r = Fake::default();
    let s = Server::new(&web, &r, Some(ADDR.into()));
    assert_eq!(
        run(&cli, &Fake::default(), &["pause", "sendmeter"]),
        (0, "sendmeter: paused\n".into())
    );
    let res = post(&s, &format!("op=pause&fleet=sendmeter&mtime={}", mtime(&s)), "");
    assert_eq!(location(&res), "/?msg=sendmeter paused", "{res}");
    let text = |c: &drover::Ctx| std::fs::read_to_string(&c.config).unwrap();
    assert_eq!(text(&cli), text(&web));
    assert!(
        text(&web).contains("integration = \"staging\"   # never main\npaused = true\n"),
        "{}",
        text(&web)
    );
    assert_eq!(last(&cli), (last(&web).0, None));
    assert_eq!(
        last(&web),
        (
            json!({"type": "pause", "fleet": "sendmeter", "data": {}}),
            Some("web".into())
        )
    );
    assert!(get(&s, "/").contains("> paused · ") && get(&s, "/").contains(">Resume</button>"));
    for c in [&cli, &web] {
        assert_eq!(
            run(c, &Fake::default(), &["next", "sendmeter"]),
            (1, "sendmeter: paused\n".into())
        );
        for role in ["impl", "rev"] {
            let (code, out) = run(c, &Fake::default(), &["spawn", "sendmeter", "17", role]);
            assert_eq!(code, 3, "{out}");
            assert!(out.starts_with("REFUSED PAUSED: "), "{out}");
        }
    }
    assert_eq!(run(&cli, &Fake::default(), &["resume", "sendmeter"]).0, 0);
    let res = post(&s, &format!("op=resume&fleet=sendmeter&mtime={}", mtime(&s)), "");
    assert_eq!(location(&res), "/?msg=sendmeter resumed");
    assert_eq!(text(&cli), text(&web));
    assert!(text(&web).contains("paused = false\n"));
    assert_eq!(
        last(&web),
        (
            json!({"type": "resume", "fleet": "sendmeter", "data": {}}),
            Some("web".into())
        )
    );
    assert_eq!(run(&web, &Fake::default(), &["next", "sendmeter"]), (0, "17\n".into()));
}

#[test]
fn role_dropdowns_set_like_drover_set() {
    let (cli, web) = pair("set");
    let r = Fake::default();
    let s = Server::new(&web, &r, Some(ADDR.into()));
    assert!(get(&s, "/").contains("<button type=button class=cfg data-f=\"sendmeter\">Config</button>"));
    let page = queue_js(&s, ADDR, 1_000_000, "sheet(d, 'sendmeter')");
    assert!(
        page.contains("<option selected>opus</option><option>sonnet</option>"),
        "only `models`"
    );
    assert!(page.contains("<select name=effort aria-label=effort><option value=\"\" selected>default (none)</option><option>none</option>"));
    assert_eq!(
        run(
            &cli,
            &Fake::default(),
            &["set", "sendmeter", "impl", "--model", "sonnet", "--effort", "high"]
        )
        .0,
        0
    );
    let res = post(
        &s,
        &format!(
            "op=set&fleet=sendmeter&role=impl&model=sonnet&effort=high&mtime={}",
            mtime(&s)
        ),
        "",
    );
    assert_eq!(
        location(&res),
        "/?msg=sendmeter impl: model sonnet, effort high",
        "{res}"
    );
    let text = |c: &drover::Ctx| std::fs::read_to_string(&c.config).unwrap();
    assert_eq!(text(&cli), text(&web));
    assert!(text(&web).contains("model = \"sonnet\"\neffort = \"high\"\n"));
    let sets = |c: &drover::Ctx| {
        read(&c.state)
            .unwrap()
            .to_vec()
            .into_iter()
            .filter(|e| e.kind == "set")
            .collect::<Vec<_>>()
    };
    assert_eq!(sets(&web).len(), 2);
    for (a, b) in sets(&cli).iter().zip(sets(&web)) {
        let mut b = b.clone();
        assert_eq!(b.data.remove("via"), Some(json!("web")));
        assert_eq!(a.data, b.data);
    }
    let before = text(&web);
    let res = post(
        &s,
        &format!("op=set&fleet=sendmeter&role=impl&model=gpt&mtime={}", mtime(&s)),
        "",
    );
    assert!(
        location(&res).starts_with("/?msg=REFUSED BAD_VALUE: model \"gpt\" is not in `models`"),
        "{res}"
    );
    let res = post(
        &s,
        &format!("op=set&fleet=sendmeter&role=impl&effort=max&mtime={}", mtime(&s)),
        "",
    );
    assert!(location(&res).contains("effort \"max\" must be one of"), "{res}");
    assert_eq!(text(&web), before, "no write");
    assert_eq!(
        run(&cli, &Fake::default(), &["set", "sendmeter", "impl", "--model", "gpt"]).0,
        3
    );
}

#[test]
fn set_refuses_toml_injection_on_web_and_cli() {
    let (cli, web) = pair("inject");
    let r = Fake::default();
    let s = Server::new(&web, &r, Some(ADDR.into()));
    let text = |c: &drover::Ctx| std::fs::read_to_string(&c.config).unwrap();
    let (before, log) = (text(&web), drover::events::jsonl(&web.state).unwrap());
    let evil = "opus%22%0A%5B%5Bnotify%5D%5D%0Acmd+%3D+%22touch+%2Ftmp%2Fpwned%22%0A%23";
    for field in ["model", "effort"] {
        let res = post(
            &s,
            &format!("op=set&fleet=sendmeter&role=impl&{field}={evil}&mtime={}", mtime(&s)),
            "",
        );
        assert!(
            location(&res).starts_with(&format!("/?msg=REFUSED BAD_VALUE: {field} ")),
            "{res}"
        );
    }
    assert_eq!(text(&web), before, "fleet.toml byte-identical");
    assert_eq!(drover::events::jsonl(&web.state).unwrap(), log, "no event");
    let before = text(&cli);
    for v in [
        "opus\"\n[[notify]]\ncmd = \"touch /tmp/pwned\"\n#",
        "x\"\nprofile = \"evil",
        "a\\b",
        "",
    ] {
        for flag in ["--model", "--effort", "--profile"] {
            let (code, out) = run(&cli, &Fake::default(), &["set", "sendmeter", "impl", flag, v]);
            assert_eq!(code, 3, "{flag} {v:?}: {out}");
            assert!(out.contains("REFUSED BAD_VALUE"), "{out}");
        }
    }
    assert_eq!(text(&cli), before, "fleet.toml byte-identical");
}

#[test]
fn stale_mtime_refuses_without_writing() {
    let (_, web) = pair("stale");
    let r = Fake::default();
    let s = Server::new(&web, &r, Some(ADDR.into()));
    let (cfg, log) = (
        std::fs::read_to_string(&web.config).unwrap(),
        drover::events::jsonl(&web.state).unwrap(),
    );
    for body in [
        "op=pause&fleet=sendmeter&mtime=1",
        "op=mv&fleet=sendmeter&issue=12&before=17&mtime=1",
        "op=set&fleet=sendmeter&role=impl&model=sonnet",
    ] {
        let res = post(&s, body, "");
        assert!(res.starts_with("HTTP/1.1 303 "), "{res}");
        assert!(location(&res).starts_with("/?msg=REFUSED STALE: "), "{res}");
    }
    assert_eq!(std::fs::read_to_string(&web.config).unwrap(), cfg);
    assert_eq!(drover::events::jsonl(&web.state).unwrap(), log);
    assert!(get(&s, "/?msg=REFUSED%20STALE%3A%20x").contains("<p class=\"msg r\">REFUSED STALE: x</p>"));
}

#[test]
fn foreign_host_or_origin_is_403_and_read_only_is_405() {
    let (_, web) = pair("origin");
    let r = Fake::default();
    let s = Server::new(&web, &r, Some(ADDR.into()));
    let body = format!("op=pause&fleet=sendmeter&mtime={}", mtime(&s));
    let log = drover::events::jsonl(&web.state).unwrap();
    for extra in [
        "Origin: https://evil.example\r\n",
        "Origin: http://127.0.0.1:7777.evil\r\n",
        "Origin: null\r\n",
    ] {
        assert!(post(&s, &body, extra).starts_with("HTTP/1.1 403 "), "{extra}");
    }
    let wrong = format!(
        "POST / HTTP/1.1\r\nHost: evil.example\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    assert!(String::from_utf8_lossy(&s.respond(&wrong, 1_000_000)).starts_with("HTTP/1.1 403 "));
    assert_eq!(drover::events::jsonl(&web.state).unwrap(), log, "nothing written");
    let ts = format!(
        "POST / HTTP/1.1\r\nHost: mac.tail1.ts.net:7443\r\nOrigin: https://mac.tail1.ts.net:7443\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    assert!(String::from_utf8_lossy(&s.respond(&ts, 1_000_000)).starts_with("HTTP/1.1 303 "));
    assert!(post(
        &s,
        &format!("op=resume&fleet=sendmeter&mtime={}", mtime(&s)),
        &format!("Origin: http://{ADDR}\r\n")
    )
    .starts_with("HTTP/1.1 303 "));
    let ro = Server::new(&web, &r, None);
    assert!(post(&ro, &body, "").starts_with("HTTP/1.1 405 "));
    let page = get(&ro, "/");
    assert!(
        !page.contains("<form method=post") && page.contains("data-ro=1"),
        "no buttons"
    );
}

#[test]
fn page_reads_only_cached_pr_data_and_serves_pwa_assets() {
    let d = tmpdir("pwa");
    let c = ctx(&d, &config());
    run(&c, &Fake::default(), &["queue", "sendmeter", "add", "12"]);
    append(
        &c.state,
        &Event::new(
            999_000,
            "sendmeter",
            Some(12),
            "spawn",
            json!({"role": "impl", "agent": "sendmeter-impl-12"}),
        ),
    )
    .unwrap();
    let cache = c.state.parent().unwrap().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let pr = json!({"number": 34, "state": "OPEN", "statusCheckRollup": [{"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "SUCCESS"}]});
    std::fs::write(
        cache.join("pr-sendmeter-12.json"),
        json!({"ts": 1, "branch": "issue-12", "pr": pr}).to_string(),
    )
    .unwrap();
    let r = Fake::default();
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let page = get(&s, "/");
    assert!(
        page.contains("pull/34\">PR #34</a>] [<a href=\"https://github.com/jirathip-dev/sendmeter/pull/34/checks\">checks</a>] · CI <span class=g>✓</span>"),
        "{page}"
    );
    assert!(!r.called("gh"), "no gh call per page load: {:?}", r.calls.borrow());
    for (path, ctype) in [
        ("/manifest.webmanifest", "application/manifest+json"),
        ("/sw.js", "text/javascript"),
        ("/icon-180.png", "image/png"),
        ("/icon-192.png", "image/png"),
        ("/icon-512.png", "image/png"),
    ] {
        let res = s.respond(&format!("GET {path} HTTP/1.1\r\n\r\n"), 1);
        let head = String::from_utf8_lossy(&res[..res.len().min(200)]).into_owned();
        assert!(
            head.starts_with("HTTP/1.1 200 OK") && head.contains(ctype),
            "{path}: {head}"
        );
    }
    for n in [180u32, 192, 512] {
        let res = s.respond(&format!("GET /icon-{n}.png HTTP/1.1\r\n\r\n"), 1);
        let png = &res[res.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4..];
        let dim = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
        assert!(
            png.starts_with(b"\x89PNG\r\n\x1a\n") && (dim(16), dim(20)) == (n, n),
            "icon-{n}.png dimensions"
        );
    }
    let body = |p: &str| {
        let res = s.respond(&format!("GET {p} HTTP/1.1\r\n\r\n"), 1);
        let at = res.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        (String::from_utf8_lossy(&res[..at]).into_owned(), res[at + 4..].to_vec())
    };
    for (path, icon) in [
        ("/apple-touch-icon.png", "/icon-180.png"),
        ("/apple-touch-icon-precomposed.png", "/icon-180.png"),
        ("/favicon.ico", "/icon-192.png"),
    ] {
        let (head, png) = body(path);
        assert!(
            head.starts_with("HTTP/1.1 200 OK") && head.contains("image/png"),
            "{path}: {head}"
        );
        assert!(png == body(icon).1, "{path}: not the {icon} bytes");
    }
    let v = page.split("/a/").nth(1).unwrap().split('/').next().unwrap();
    assert!(
        page.contains(&format!("rel=icon href=/icon-192.png?v={v}"))
            && page.contains(&format!("rel=apple-touch-icon href=/icon-180.png?v={v}")),
        "{page}"
    );
    assert!(get(&s, "/manifest.webmanifest").contains("\"display\": \"standalone\""));
    assert!(
        page.contains("rel=manifest href=/manifest.webmanifest")
            && std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/serve.js"))
                .unwrap()
                .contains("serviceWorker.register(R + 'sw.js')")
    );
    assert!(page.contains("data-ts=1000000"), "render time for the stale badge");
}

#[test]
fn lanes_group_by_fleet_with_others() {
    let d = tmpdir("lanes");
    let two = format!("{CONFIG}\n[fleet.synergy]\nrepo = \"o/synergy-apps\"\nlocal = \"/p/synergy-apps\"\nintegration = \"staging\"\n");
    let c = ctx(&d, &two);
    let cfg = drover::config::Config::load(&c.config).unwrap();
    let ev = |fleet: &str, issue: Option<u64>, d: serde_json::Value| {
        append(&c.state, &Event::new(900_000, fleet, issue, "spawn", d)).unwrap()
    };
    ev(
        "sendmeter",
        Some(12),
        json!({"role": "impl", "agent": "sendmeter-impl-12", "harness": "claude", "model": "opus", "effort": "high"}),
    );
    ev(
        "sendmeter",
        Some(9),
        json!({"role": "rev", "agent": "sendmeter-rev-9-r1", "harness": "hermes"}),
    );
    ev(
        "synergy",
        Some(5),
        json!({"role": "design", "agent": "synergy-design-5", "harness": "claude"}),
    );
    ev(
        "synergy",
        Some(7),
        json!({"role": "impl", "agent": "synergy-impl-7", "harness": "claude", "pane": "w5:p1"}),
    );
    let events = read(&c.state).unwrap();
    let st = drover::events::fold(&events);
    let wt = c.worktrees.join("synergy-apps/designer").to_string_lossy().into_owned();
    let agents = json!([
        {"name": "sendmeter-impl-12", "agent_status": "working", "pane_id": "w1:p2", "cwd": "/x"},
        {"name": "synergy-design-5", "agent_status": "done", "pane_id": "w2:p1", "cwd": "/x"},
        {"name": "w3", "agent": "claude", "agent_status": "working", "pane_id": "w3", "cwd": wt},
        {"name": "side-project", "agent": "codex", "agent_status": "idle", "pane_id": "side-project", "cwd": "/Users/me/Projects/side"},
        {"agent": "dsh-tui", "agent_status": "blocked", "pane_id": "w5:p1", "cwd": "/p/synergy-apps"},
    ]);
    let lanes = drover::lanes::join(
        &cfg,
        &Fake::default(),
        &c.worktrees,
        &events,
        &st,
        agents.as_array().unwrap(),
    );
    let got: Vec<(Option<&str>, &str, &str, &str, bool)> = lanes
        .iter()
        .map(|l| {
            (
                l.fleet.as_deref(),
                l.name.as_str(),
                l.role.as_str(),
                l.state.as_str(),
                l.parked.is_some(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (Some("sendmeter"), "sendmeter-impl-12", "impl", "working", false),
            (Some("synergy"), "synergy-design-5", "design", "done", false),
            (Some("synergy"), "w3", "other", "working", false),
            (None, "side-project", "other", "idle", false),
            (Some("synergy"), "synergy-impl-7", "impl", "blocked", false),
            (Some("sendmeter"), "sendmeter-rev-9-r1", "rev", "dead", false),
        ]
    );
    assert_eq!(
        (lanes[0].model.as_deref(), lanes[0].effort.as_deref(), lanes[0].issue),
        (Some("opus"), Some("high"), Some(12))
    );
    let sh = FakeShed::attached(&c);
    sh.lane("sendmeter-impl-12", "working", "/x");
    sh.screen(
        "sendmeter-impl-12",
        "⏺ Bash(cd ~/w; cargo test 2>&1 | tail -5)\n\n✻ Herding… (4m 2s · ↓ 1k tokens)\n────────────────────\n> \n────────────────────\n",
    );
    sh.lane("synergy-design-5", "done", "/x");
    for (name, harness, cwd, state) in [
        ("w3", "claude", wt.as_str(), "working"),
        ("side-project", "codex", "/Users/me/Projects/side", "idle"),
    ] {
        sh.add(name, json!({"fleet": "", "harness": harness}));
        sh.set(name, |t| {
            (t["cwd"], t["reported"]) = (json!(cwd), json!({"state": state}))
        });
    }
    sh.lane("synergy-impl-7", "blocked", "/p/synergy-apps");
    let f = Fake::default();
    let r = sh.on(&f);
    let page = get(&Server::new(&c, &r, None), "/?all=1");
    let at = |s: &str| page.find(s).unwrap_or_else(|| panic!("{s} missing"));
    let working = &page[at("<h2><span class=g>working (2)</span></h2><h3>sendmeter (1)</h3>")..];
    let rest = &working[4..];
    assert!(
        rest.find("<h3>synergy (1)</h3>") < rest.find("<h2>"),
        "synergy subgroup inside working"
    );
    assert!(
        page.contains("⚠ log/pane mismatch")
            && page.contains(concat!(
                "<span class=\"act nw el now\" title=\"✻ Herding… · now: running cd ~/w; cargo test 2&gt;&amp;1 | tail -5 · 4m\">",
                "<span class=el>✻ Herding…</span><i> · </i><span class=el>running cargo test</span><i> · </i><span class=t>4m</span></span>"
            ))
    );
}

#[test]
fn one_lane_per_agent_name() {
    let c = ctx(&tmpdir("lanes-dup"), CONFIG);
    let cfg = drover::config::Config::load(&c.config).unwrap();
    for ts in [900_000, 950_000] {
        let d = json!({"role": "impl", "agent": "sendmeter-impl-3061", "harness": "claude", "pane": "w1:p1"});
        append(&c.state, &Event::new(ts, "sendmeter", Some(3061), "spawn", d)).unwrap();
    }
    let events = read(&c.state).unwrap();
    let agents = json!([
        {"name": "sendmeter-impl-3061", "agent_status": "working", "pane_id": "w1:p1", "cwd": "/x"},
        {"name": "sendmeter-impl-3061", "agent_status": "idle", "pane_id": "w9:p1", "cwd": "/x", "host": "mini"},
    ]);
    let st = drover::events::fold(&events);
    let fake = Fake::default();
    let lanes = drover::lanes::join(&cfg, &fake, &c.worktrees, &events, &st, agents.as_array().unwrap());
    let got: Vec<_> = lanes
        .iter()
        .map(|l| (l.name.as_str(), l.state.as_str(), l.pane.as_deref()))
        .collect();
    assert_eq!(got, [("sendmeter-impl-3061", "working", Some("w1:p1"))]);
}

#[test]
fn activity_lines_per_harness_are_masked_and_cut() {
    use drover::lanes::{activity, mask, tail};
    let claude = "⏺ Read(src/serve.rs)\n  ⎿ Read 120 lines\n⏺ Bash(cargo test --release)\n  ⎿ Running…\n╭──────╮\n│ > │\n╰──────╯\n⏵⏵ bypass permissions on\n";
    assert_eq!(activity("claude", claude), "⏺ Bash(cargo test --release)");
    let hermes = "┊ 💻 $ git status  0.2s\nthinking...\n┊ 📖 read  src/lib.rs  0.1s\n────\n❯ \n";
    assert_eq!(activity("hermes", hermes), "┊ 📖 read  src/lib.rs  0.1s");
    let dsh = "Applying patch to src/a.rs\n─────────────\n> type here\n";
    assert_eq!(activity("dsh", dsh), "Applying patch to src/a.rs");
    let long = format!("⏺ Bash(echo {})", "x".repeat(80));
    let cut = activity("claude", &long);
    assert_eq!((cut.chars().count(), cut.ends_with('…')), (60, true));
    let secrets = [
        (
            concat!("export ANTHROPIC_API_KEY=sk", "-ant-api03-abcdefghijklmnop"),
            "export ANTHROPIC_API_KEY=•••",
        ),
        (
            concat!("curl -H token sk", "-ant-api03-abcdefghijklmnop"),
            "curl -H token sk-•••",
        ),
        (
            concat!("gh auth gh", "p_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"),
            "gh auth ghp_•••",
        ),
        (
            concat!("gh", "o_16C7e42F292c6912E7710c838347Ae178B4a done"),
            "gho_••• done",
        ),
        (concat!("slack xo", "xb-1234567890-abcdefghij"), "slack xox•••"),
        (
            concat!("jwt ey", "JhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abc123def"),
            "jwt •••",
        ),
        ("sha 0123456789abcdef0123456789abcdef01234567 ok", "sha ••• ok"),
        ("b64 QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVowMTIzNDU2Nzg5", "b64 •••"),
        ("GITHUB_TOKEN=\"abc def\" next", "GITHUB_TOKEN=\"••• def\" next"),
        ("DB_PASSWORD=hunter2 cargo test", "DB_PASSWORD=••• cargo test"),
        (
            "cargo test --release -p drover serve_web",
            "cargo test --release -p drover serve_web",
        ),
        (
            "/Users/jirathip/.herdr/worktrees/drover/issue-16/src",
            "/Users/jirathip/.herdr/worktrees/drover/issue-16/src",
        ),
    ];
    for (raw, want) in secrets {
        assert_eq!(mask(raw), want, "{raw}");
    }
    assert_eq!(
        activity(
            "claude",
            concat!("⏺ Bash(export OPENAI_API_KEY=sk", "-proj-abcdefghijklmnopqrstuvwxyz)")
        ),
        "⏺ Bash(export OPENAI_API_KEY=•••)"
    );
    let lines: String = (1..=14).map(|i| format!("line {i}\n\n")).collect::<String>()
        + concat!("gh", "p_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789\n");
    let t = tail(&lines, 10);
    assert_eq!((t.len(), t[0].as_str(), t[9].as_str()), (10, "line 6", "ghp_•••"));
}

#[test]
fn page_fits_390px_without_horizontal_scroll() {
    let d = tmpdir("phone");
    let c = ctx(&d, &config());
    run(&c, &Fake::default(), &["queue", "sendmeter", "add", "17", "18", "12"]);
    let cache = d.join("state/cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(
        cache.join("issue-sendmeter-12.json"),
        json!({"title": "Queue rows show issue and PR titles from the cached reads"}).to_string(),
    )
    .unwrap();
    std::fs::write(
        cache.join("pr-sendmeter-12.json"),
        json!({"pr": {"number": 33, "title": "src: titles in Queue"}}).to_string(),
    )
    .unwrap();
    let long = "sendmeter-impl-12-with-a-very-long-agent-name-that-would-push-a-table-wide";
    append(&c.state, &Event::new(999_000, "sendmeter", Some(12), "spawn", json!({"role": "impl", "agent": long, "branch": "issue-12-a-very-long-branch-name-without-any-break-points-at-all"}))).unwrap();
    append(
        &c.state,
        &Event::new(
            999_100,
            "sendmeter",
            Some(12),
            "blocked",
            json!({"code": "CI_NOT_GREEN", "reason": "x".repeat(300)}),
        ),
    )
    .unwrap();
    let screen = format!("⏺ Bash({})\n{}\n", "y".repeat(200), "z".repeat(400));
    let sh = FakeShed::new();
    sh.attach(&c);
    sh.lane(long, "working", "/x");
    sh.screen(long, &screen);
    let f = Fake::default();
    let r = sh.on(&f);
    let page = get(&Server::new(&c, &r, Some(ADDR.into())), "/");
    let html = &page[page.find("<!doctype").unwrap()..];
    assert!(!html.contains("<table") && !html.contains("<pre"), "no wide blocks");
    assert!(
        html.contains("width=device-width")
            && std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/serve.css"))
                .unwrap()
                .contains("overflow-wrap:anywhere")
    );
    assert!(
        html.contains("<details class=\"q\" data-q data-act=blocked data-grow=blocked data-i=12 style=\"--rc:#")
            && html.contains(
                "\"><summary><span><a href=\"https://github.com/jirathip-dev/sendmeter/issues/12\">#12</a></span>"
            ),
        "rows are details"
    );
    assert!(
        html.contains("class=tl title=\"Queue rows show issue and PR titles from the cached reads · PR #33 · src: titles in Queue\">"),
        "title line"
    );
    for t in ["queue", "lanes", "chat"] {
        assert!(
            html.contains(&format!("<section id={t}>")) && html.contains(&format!("<a href=#{t}>")),
            "{t} tab"
        );
    }
    if no_webkit() {
        return;
    }
    let file = d.join("page.html");
    std::fs::write(&file, html).unwrap();
    for tab in ["queue", "lanes"] {
        for mode in ["horse", "ascii", "glyph"] {
            let o = webview()
                .args([file.to_str().unwrap(), "390", tab, mode])
                .output()
                .unwrap();
            let out = String::from_utf8_lossy(&o.stdout);
            let w: Vec<u32> = out.split_whitespace().filter_map(|n| n.parse().ok()).collect();
            assert!(
                o.status.success() && w.len() == 2,
                "{tab}/{mode}: {out} {}",
                String::from_utf8_lossy(&o.stderr)
            );
            assert!(
                w[0] <= 390 && w[1] == 390,
                "{tab}/{mode}: scrollWidth {} at {}",
                w[0],
                w[1]
            );
        }
    }
}

#[test]
fn queue_shows_open_work_only_and_folds_paused_retired_and_noise() {
    let d = tmpdir("short");
    let two = format!("{}\n[fleet.synergy]\nrepo = \"o/synergy-apps\"\nlocal = \"/p/synergy-apps\"\nintegration = \"staging\"\npaused = true\n", config());
    let c = ctx(&d, &two);
    let ev = |fleet: &str, issue: u64, ts: u64, kind: &str, d: serde_json::Value| {
        append(&c.state, &Event::new(ts, fleet, Some(issue), kind, d)).unwrap()
    };
    append(
        &c.state,
        &Event::new(1, "fleet", None, "queue", json!({"op": "add", "issues": [3]})),
    )
    .unwrap();
    ev("fleet", 3, 2, "spawn", json!({"role": "impl", "agent": "fleet-impl-3"}));
    run(&c, &Fake::default(), &["queue", "sendmeter", "add", "17", "18"]);
    for n in 100..130 {
        run(&c, &Fake::default(), &["queue", "sendmeter", "add", &n.to_string()]);
        ev(
            "sendmeter",
            n,
            990_000 + n,
            "merged",
            json!({"pr": (n + 1000).to_string(), "merge_sha": "abc", "into": "staging"}),
        );
    }
    ev(
        "sendmeter",
        17,
        999_000,
        "spawn",
        json!({"role": "impl", "agent": "sendmeter-impl-17"}),
    );
    run(&c, &Fake::default(), &["queue", "synergy", "add", "1039", "1040"]);
    ev(
        "synergy",
        1039,
        999_000,
        "spawn",
        json!({"role": "impl", "agent": "synergy-impl-1039"}),
    );
    for k in 0..4 {
        ev(
            "sendmeter",
            17,
            999_500 + k,
            if k % 2 == 0 { "clean" } else { "notify-failed" },
            json!({"why": "merged"}),
        );
    }
    let sh = FakeShed::new();
    sh.attach(&c);
    sh.lane("sendmeter-impl-17", "working", "/x");
    let f = Fake::default();
    let r = sh.on(&f);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let page = get(&s, "/");
    let section = |id: &str| {
        let a = page.find(&format!("<section id={id}>")).unwrap();
        page[a..a + page[a..].find("</section>").unwrap()].to_string()
    };
    let queue = section("queue");
    assert!(
        !queue.contains("fleet-impl-3") && !queue.contains("<span>fleet</span>"),
        "no retired fleet"
    );
    let rows = queue.matches("<details class=\"q\"").count();
    assert_eq!(rows, 2 + 2, "open items only: sendmeter #17 #18, synergy #1039 #1040");
    assert!(queue.contains("<span>done today: 30</span>"), "{queue}");
    let done = &queue[queue.find("done today").unwrap()..];
    let done = &done[..done.find("</details>").unwrap()];
    assert_eq!(done.matches("<p>#").count(), 5, "at most 5: {done}");
    assert!(done.starts_with("done today: 30</span></summary><p>#129 ") && done.contains("pull/1129\">PR #1129</a>"));
    assert!(
        queue.contains(
            "<summary><i class=cv></i><span class=fn><b>synergy</b> <i class=m>●</i> paused · 1 lane (dead)</span>"
        ),
        "{queue}"
    );
    let syn = &queue[queue.find("class=\"fc off").unwrap()..];
    assert!(
        syn.contains(
            "<span><a href=\"https://github.com/o/synergy-apps/issues/1039\">#1039</a></span><span class=r>dead</span>"
        ),
        "{syn}"
    );
    let dead_row = &syn[syn
        .find("<span><a href=\"https://github.com/o/synergy-apps/issues/1039\">#1039</a></span>")
        .unwrap()..];
    let dead_row = &dead_row[..dead_row.find("</details>").unwrap()];
    assert!(!dead_row.contains("<button"), "no action on a dead lane: {dead_row}");
    assert!(
        queue.contains(
            "<a href=\"https://github.com/jirathip-dev/sendmeter/issues/17\">#17</a></span><span class=a>impl</span>"
        ),
        "live lane stays impl"
    );
    assert!(
        queue.contains("stall check: #17 in"),
        "paused fleet's lane is not counted"
    );
    assert!(
        !page.contains("id=events"),
        "#60: Events loads when opened; #440: in System (serve.js makes its section)"
    );
    if let Some(ev) = events_tab(&s, ADDR, 1_000_000, "{ f: { k: 'all', fl: [] } }") {
        let rows: Vec<&str> = ev.split("<details class=\"er").skip(1).collect();
        assert!(rows
            .iter()
            .filter(|r| r.contains("alert failed"))
            .all(|r| r.contains("t-r\" data-act=blocked>✗") && !r.contains("clean")));
        assert!(ev.contains("alert failed") && !ev.contains("clean ×"), "{ev}");
        let all = events_tab(&s, ADDR, 1_000_000, "{ all: true }").unwrap();
        assert!(
            all.matches("<details class=\"er").count() > 2 * 50,
            "unfolded, unlimited"
        );
    }
}

#[test]
fn page_never_samples_the_host_and_links_immutable_assets() {
    let c = ctx(&tmpdir("assets"), &config());
    let r = Fake::default()
        .reply("vm_stat", "Swapouts: 100.\n")
        .reply("sysctl", "1\n");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let page = get(&s, "/");
    assert_eq!(r.count("vm_stat"), 0, "{:?}", r.calls.borrow());
    assert!(
        !r.called("sysctl")
            && page.contains("Cache-Control: no-store")
            && !page.contains("<style>")
            && page.matches("<script>").count() == 1
            && page.contains("<script>let h=document.documentElement,t=location.hash")
    );
    let i = page.find("<script src=/a/").unwrap() + 15;
    let v = &page[i..i + 8];
    assert!(v.chars().all(|c| c.is_ascii_hexdigit()), "{v}");
    let sw = get(&s, "/sw.js");
    for (f, ctype) in [
        ("serve.css", "text/css"),
        ("herd.css", "text/css"),
        ("serve.js", "text/javascript"),
        ("herd.js", "text/javascript"),
        ("themes/farm/sprites.svg", "image/svg+xml"),
        ("themes/farm/theme.json", "application/json"),
    ] {
        let res = get(&s, &format!("/a/{v}/{f}"));
        assert!(
            res.starts_with("HTTP/1.1 200 OK")
                && res.contains(ctype)
                && res.contains("Cache-Control: public, max-age=31536000, immutable\r\n"),
            "{f}"
        );
        assert!(
            page.contains(&format!("/a/{v}/{f}")) || f.starts_with("themes/"),
            "{f} linked"
        );
        assert!(
            sw.contains(&format!("'{f}'")) && sw.contains(&format!("P + 'a/{v}/'")),
            "{f} precached"
        );
    }
    assert!(
        get(&s, "/a/00000000/serve.js").starts_with("HTTP/1.1 404 "),
        "another build's hash"
    );
    assert!(
        get(&s, "/icon-192.png").contains("Cache-Control: public, max-age=86400\r\n")
            && get(&s, "/sw.js").contains("Cache-Control: no-store\r\n")
    );
    assert_eq!(r.count("vm_stat"), 0);
}

#[test]
fn fetch_only_reads_redirect_navigations_to_the_board() {
    let (c, r) = (ctx(&tmpdir("frags-nav"), &config()), Fake::default());
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let send = |path: &str, extra: &str| {
        String::from_utf8_lossy(&s.respond(
            &format!("GET {path} HTTP/1.1\r\nHost: {ADDR}\r\n{extra}\r\n"),
            1_000_000,
        ))
        .into_owned()
    };
    for path in ["/herd.json", "/api/v1/needs", "/api/v1/lanes"] {
        let res = send(path, "");
        assert!(
            res.starts_with("HTTP/1.1 200 OK") && res.contains("Content-Type: application/json; charset=utf-8\r\n"),
            "{path}: {res}"
        );
        let j: serde_json::Value = serde_json::from_str(&res[res.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert!(j.is_object(), "{path}");
    }
    assert!(send("/frags", "").starts_with("HTTP/1.1 404"), "no HTML parts");
    for path in ["/herd.json", "/lane?agent=sendmeter-impl-12"] {
        for nav in [
            "Sec-Fetch-Mode: navigate\r\n",
            "Sec-Fetch-Dest: document\r\n",
            "Accept: text/html,application/xhtml+xml\r\n",
        ] {
            let res = send(path, nav);
            assert!(
                res.starts_with("HTTP/1.1 303 See Other\r\nLocation: /\r\n"),
                "{path} {nav}: {res}"
            );
        }
        assert!(
            !send(path, "Sec-Fetch-Mode: cors\r\nAccept: */*\r\n").starts_with("HTTP/1.1 303"),
            "{path}: fetch() is not redirected"
        );
    }
    assert!(
        send("/", "Sec-Fetch-Mode: navigate\r\n").starts_with("HTTP/1.1 200 OK"),
        "the board itself"
    );
    let sw = send("/sw.js", "");
    assert!(sw.contains("e.request.mode === 'navigate' && u.pathname !== P && !u.pathname.startsWith(P + 'thread/')) return e.respondWith(caches.match(P).then(m => m || fetch(P)))"), "{sw}");
    assert!(sw.contains("u.pathname.startsWith(P + 'api/')") && !sw.contains("frags"));
    let v = send("/", "");
    let v = &v[v.find("/a/").unwrap()..][..11];
    assert!(!send(&format!("{v}/serve.js"), "").contains("frags"));
}

#[test]
fn frag_parts_redirect_navigations_and_404_unknown_ids() {
    let (c, r) = (ctx(&tmpdir("frag-part"), &config()), Fake::default());
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let send = |path: &str, extra: &str| {
        String::from_utf8_lossy(&s.respond(
            &format!("GET {path} HTTP/1.1\r\nHost: {ADDR}\r\n{extra}\r\n"),
            1_000_000,
        ))
        .into_owned()
    };
    for path in ["/frag?id=hist&host=mini&k=load"] {
        assert!(send(path, "").starts_with("HTTP/1.1 200 OK"), "{path}");
        for nav in [
            "Sec-Fetch-Mode: navigate\r\n",
            "Sec-Fetch-Dest: document\r\n",
            "Accept: text/html,application/xhtml+xml\r\n",
        ] {
            assert!(
                send(path, nav).starts_with("HTTP/1.1 303 See Other\r\nLocation: /\r\n"),
                "{path} {nav}"
            );
        }
    }
    for path in [
        "/frag?id=nope",
        "/frag",
        "/frag?id=roles&fleet=sendmeter",
        "/frag?id=config",
        "/frag?id=config&fleet=nope",
        "/frag?id=config&fleet=sendmeter",
    ] {
        let res = send(path, "");
        assert!(
            res.starts_with("HTTP/1.1 404 Not Found") && res.ends_with("no such part\n"),
            "{path}: {res}"
        );
    }
}

#[test]
fn pair_request_carries_the_optional_approve_hint() {
    let js = include_str!("../../../assets/serve.js");
    assert!(
        js.contains("`Waiting for approval on the Mac: drover serve approve ${t}` + (h ? ` · ${h}` : '')"),
        "the Mac line, hint after ` · `"
    );
    assert!(
        js.contains("el('p', 'm', `Waiting"),
        "`el` sets textContent: the hint is never HTML"
    );
    let request = |c: &drover::Ctx| {
        let r = Fake::default();
        let s = Server::new(c, &r, Some(ADDR.into()));
        let req =
            format!("POST /pair/request HTTP/1.1\r\nHost: {ADDR}\r\nX-Peer: 10.0.0.1\r\nContent-Length: 0\r\n\r\n");
        let res = String::from_utf8_lossy(&s.respond(&req, 1_000_000)).into_owned();
        assert!(res.starts_with("HTTP/1.1 200 "), "{res}");
        res.rsplit("\r\n\r\n").next().unwrap().to_string()
    };
    let body = request(&ctx(&tmpdir("hint-off"), &config()));
    assert!(body.len() == 8 && body.ends_with("\n\n"), "code only: {body:?}");
    let hinted = config().replace(
        "[serve]\n",
        "[serve]\napprove_hint = \"or send 'pair {code}' to <b>ops</b>\"\n",
    );
    let body = request(&ctx(&tmpdir("hint-on"), &hinted));
    let code = &body[..6];
    assert_eq!(
        body,
        format!("{code}\nor send 'pair {code}' to <b>ops</b>\n"),
        "text/plain; the page sets it as text"
    );
}

#[test]
fn host_local_pairing_shows_the_approve_line_and_lasts_a_year() {
    let js = include_str!("../../../assets/serve.js");
    assert!(
        js.contains("el('button', 'pairc', 'copy')") && js.contains("paste in Terminal"),
        "the copy button"
    );
    assert!(
        js.contains("setInterval(claim, 2000)"),
        "the page pairs itself once approved"
    );
    let c = ctx(&tmpdir("pair-host"), &config());
    let r = Fake::default();
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let request = |extra: &str| {
        let req = format!("POST /pair/request HTTP/1.1\r\n{extra}Content-Length: 0\r\n\r\n");
        let res = String::from_utf8_lossy(&s.respond(&req, 1_000_000)).into_owned();
        assert!(res.starts_with("HTTP/1.1 200 "), "{res}");
        devices::cancel(&c, res.lines().find_map(|l| l.strip_prefix("Set-Cookie: ")).unwrap()).unwrap();
        res.rsplit("\r\n\r\n").next().unwrap().to_string()
    };
    let body = request("X-Peer: 127.0.0.1\r\nHost: 127.0.0.1:7777\r\n");
    assert_eq!(body, format!("{0}\n\ndrover serve approve {0}\n", &body[..6]));
    for extra in [
        "X-Peer: 127.0.0.1\r\nHost: 127.0.0.1:7777\r\nX-Forwarded-For: 100.64.0.2\r\n",
        "X-Peer: 127.0.0.1\r\nHost: 127.0.0.1:7777\r\nTailscale-User-Login: a@b\r\n",
        "X-Peer: 10.0.0.1\r\nHost: 127.0.0.1:7777\r\n",
        "X-Peer: 127.0.0.1\r\nHost: mac.tail1.ts.net\r\n",
    ] {
        assert_eq!(request(extra).len(), 8, "not on the host: code only: {extra:?}");
    }
    let cookie = common::pair(&s, &c, "Mac Safari", 1_000_000);
    let devs = |now| run(&drover::Ctx { now, ..c.clone() }, &r, &["serve", "devices"]).1;
    assert!(devs(1_000_000).ends_with(" expires in 365d\n"), "{}", devs(1_000_000));
    let paired = |now| devices::find(&drover::Ctx { now, ..c.clone() }, &cookie[8..]).is_some();
    assert!(paired(1_000_000 + 300 * 86_400), "used: renewed for a year from now");
    assert!(paired(1_000_000 + 600 * 86_400), "still within a year of that use");
    assert!(!paired(1_000_000 + 966 * 86_400), "a year unused: unpaired");
    assert_eq!(devs(1_000_000 + 966 * 86_400), "");
}
