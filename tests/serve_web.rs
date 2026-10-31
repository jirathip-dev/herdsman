//! #16: the phone page: POST writes run the CLI's own paths (+ `via:web`), STALE, Host/Origin,
//! read-only, pause, the Lanes join/grouping, masking, PWA assets, 390 px layout.
mod common;
use common::*;
use herdsman::events::{append, read, Event};
use herdsman::serve::Server;
use serde_json::json;

const ADDR: &str = "127.0.0.1:7777";

fn config() -> String {
    format!("models = [\"opus\", \"sonnet\"]\n\n[serve]\nallowed_hosts = [\"mac.tail1.ts.net\"]\n\n{CONFIG}")
}

fn get(s: &Server, path: &str) -> String {
    String::from_utf8_lossy(&s.respond(&format!("GET {path} HTTP/1.1\r\nHost: {ADDR}\r\n\r\n"), 1_000_000)).into_owned()
}

/// The hidden mtime the page renders into every form.
fn mtime(s: &Server) -> String {
    let page = get(s, "/");
    let i = page.find("name=mtime value=").expect("no mtime field") + 17;
    page[i..].chars().take_while(char::is_ascii_digit).collect()
}

fn post(s: &Server, body: &str, extra: &str) -> String {
    let req = format!("POST / HTTP/1.1\r\nHost: {ADDR}\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len());
    String::from_utf8_lossy(&s.respond(&req, 1_000_000)).into_owned()
}

fn location(res: &str) -> String {
    let l = res.lines().find_map(|l| l.strip_prefix("Location: ")).unwrap_or_default().as_bytes();
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

/// The last event, minus `via`, and its `via`.
fn last(c: &herdsman::Ctx) -> (serde_json::Value, Option<String>) {
    let mut e = read(&c.state).unwrap().pop().unwrap();
    let via = e.data.remove("via").and_then(|v| v.as_str().map(String::from));
    (json!({"type": e.kind, "fleet": e.fleet, "data": e.data}), via)
}

fn pair(name: &str) -> (herdsman::Ctx, herdsman::Ctx) {
    let (a, b) = (tmpdir(&format!("{name}-cli")), tmpdir(&format!("{name}-web")));
    let (a, b) = (ctx(&a, &config()), ctx(&b, &config()));
    for c in [&a, &b] {
        assert_eq!(run(c, &Fake::default(), &["queue", "sendmeter", "add", "17", "18", "12"]).0, 0);
    }
    (a, b)
}

fn order(c: &herdsman::Ctx) -> Vec<u64> {
    herdsman::queue::replay(&read(&c.state).unwrap(), "sendmeter").iter().map(|i| i.issue).collect()
}

#[test]
fn queue_buttons_move_like_queue_mv() {
    let (cli, web) = pair("mv");
    let r = Fake::default();
    let s = Server::new(&web, &r, Some(ADDR.into()));
    let page = get(&s, "/");
    assert!(page.contains("name=before value=18") && page.contains("▲ up"), "▲ on #12 targets #18");
    for (args, body, msg) in [
        (vec!["mv", "12", "--before", "18"], "op=mv&fleet=sendmeter&issue=12&before=18", "moved #12 before #18"),
        (vec!["mv", "17", "--after", "18"], "op=mv&fleet=sendmeter&issue=17&after=18", "moved #17 after #18"),
    ] {
        assert_eq!(run(&cli, &Fake::default(), &[&["queue", "sendmeter"][..], &args].concat()).0, 0);
        let res = post(&s, &format!("{body}&mtime={}", mtime(&s)), "");
        assert!(res.starts_with("HTTP/1.1 303 See Other\r\n"), "{res}");
        // #66: a short toast line (≤ 60 chars), not the CLI's queue dump.
        assert_eq!(location(&res), format!("/?msg={msg}"), "{res}");
        assert!(msg.len() <= 60);
        assert_eq!(order(&cli), order(&web));
        assert_eq!(last(&cli), (last(&web).0, None));
        assert_eq!(last(&web).1.as_deref(), Some("web"));
    }
    assert_eq!(order(&web), [12, 18, 17]);
    // A refusal from the shared path is shown, not hidden.
    let res = post(&s, &format!("op=mv&fleet=sendmeter&issue=99&before=12&mtime={}", mtime(&s)), "");
    assert!(location(&res).starts_with("/?msg=error: #99 is not queued"), "an error is kept: {res}");
    assert!(get(&s, "/").contains("events"), "page still renders");
}

#[test]
fn pause_button_matches_cli_and_blocks_next_and_spawn() {
    let (cli, web) = pair("pause");
    let r = Fake::default();
    let s = Server::new(&web, &r, Some(ADDR.into()));
    assert_eq!(run(&cli, &Fake::default(), &["pause", "sendmeter"]), (0, "sendmeter: paused\n".into()));
    let res = post(&s, &format!("op=pause&fleet=sendmeter&mtime={}", mtime(&s)), "");
    assert_eq!(location(&res), "/?msg=sendmeter paused", "{res}");
    let text = |c: &herdsman::Ctx| std::fs::read_to_string(&c.config).unwrap();
    assert_eq!(text(&cli), text(&web));
    assert!(text(&web).contains("integration = \"staging\"   # never main\npaused = true\n"), "{}", text(&web));
    assert_eq!(last(&cli), (last(&web).0, None));
    assert_eq!(last(&web), (json!({"type": "pause", "fleet": "sendmeter", "data": {}}), Some("web".into())));
    assert!(get(&s, "/").contains(">paused</span>") && get(&s, "/").contains(">Resume</button>"));
    // Paused: next offers nothing, impl/rev spawns refuse, orch still spawns its plan.
    for c in [&cli, &web] {
        assert_eq!(run(c, &Fake::default(), &["next", "sendmeter"]), (1, "sendmeter: paused\n".into()));
        for role in ["impl", "rev"] {
            let (code, out) = run(c, &Fake::default(), &["spawn", "sendmeter", "17", role]);
            assert_eq!(code, 3, "{out}");
            assert!(out.starts_with("REFUSED PAUSED: "), "{out}");
        }
        assert_eq!(run(c, &Fake::default(), &["spawn", "sendmeter", "-", "orch", "--dry-run"]).0, 0);
    }
    assert_eq!(run(&cli, &Fake::default(), &["resume", "sendmeter"]).0, 0);
    let res = post(&s, &format!("op=resume&fleet=sendmeter&mtime={}", mtime(&s)), "");
    assert_eq!(location(&res), "/?msg=sendmeter resumed");
    assert_eq!(text(&cli), text(&web));
    assert!(text(&web).contains("paused = false\n"));
    assert_eq!(last(&web), (json!({"type": "resume", "fleet": "sendmeter", "data": {}}), Some("web".into())));
    assert_eq!(run(&web, &Fake::default(), &["next", "sendmeter"]), (0, "17\n".into()));
}

#[test]
fn role_dropdowns_set_like_herdsman_set() {
    let (cli, web) = pair("set");
    let r = Fake::default();
    let s = Server::new(&web, &r, Some(ADDR.into()));
    // #60: a fleet's role forms load when its row is opened.
    assert!(get(&s, "/").contains("<details class=roles data-src=\"frag?id=roles&amp;fleet=sendmeter\"><summary><span>roles</span></summary></details>"));
    let page = get(&s, "/frag?id=roles&fleet=sendmeter");
    assert!(page.contains("<option selected>opus</option><option>sonnet</option>"), "only `models`");
    assert!(page.contains("<select name=effort aria-label=effort><option value=\"\" selected>default (none)</option><option>none</option>"));
    assert_eq!(run(&cli, &Fake::default(), &["set", "sendmeter", "impl", "--model", "sonnet", "--effort", "high"]).0, 0);
    let res = post(&s, &format!("op=set&fleet=sendmeter&role=impl&model=sonnet&effort=high&mtime={}", mtime(&s)), "");
    assert_eq!(location(&res), "/?msg=sendmeter impl: model sonnet, effort high", "{res}");
    let text = |c: &herdsman::Ctx| std::fs::read_to_string(&c.config).unwrap();
    assert_eq!(text(&cli), text(&web));
    assert!(text(&web).contains("model = \"sonnet\"\neffort = \"high\"\n"));
    let sets = |c: &herdsman::Ctx| read(&c.state).unwrap().into_iter().filter(|e| e.kind == "set").collect::<Vec<_>>();
    assert_eq!(sets(&web).len(), 2);
    for (a, b) in sets(&cli).iter().zip(sets(&web)) {
        let mut b = b.clone();
        assert_eq!(b.data.remove("via"), Some(json!("web")));
        assert_eq!(a.data, b.data);
    }
    // Same validation as the CLI: a model outside `models`, an effort the harness lacks.
    let before = text(&web);
    let res = post(&s, &format!("op=set&fleet=sendmeter&role=impl&model=gpt&mtime={}", mtime(&s)), "");
    assert!(location(&res).starts_with("/?msg=REFUSED BAD_VALUE: model \"gpt\" is not in `models`"), "{res}");
    let res = post(&s, &format!("op=set&fleet=sendmeter&role=impl&effort=max&mtime={}", mtime(&s)), "");
    assert!(location(&res).contains("effort \"max\" must be one of"), "{res}");
    assert_eq!(text(&web), before, "no write");
    assert_eq!(run(&cli, &Fake::default(), &["set", "sendmeter", "impl", "--model", "gpt"]).0, 3);
}

#[test]
fn set_refuses_toml_injection_on_web_and_cli() {
    let (cli, web) = pair("inject");
    let r = Fake::default();
    let s = Server::new(&web, &r, Some(ADDR.into()));
    let text = |c: &herdsman::Ctx| std::fs::read_to_string(&c.config).unwrap();
    let (before, log) = (text(&web), std::fs::read_to_string(&web.state).unwrap());
    // `opus"\n[[notify]]\ncmd = "touch /tmp/pwned"\n#`, form-encoded, as model and as effort.
    let evil = "opus%22%0A%5B%5Bnotify%5D%5D%0Acmd+%3D+%22touch+%2Ftmp%2Fpwned%22%0A%23";
    for field in ["model", "effort"] {
        let res = post(&s, &format!("op=set&fleet=sendmeter&role=impl&{field}={evil}&mtime={}", mtime(&s)), "");
        assert!(location(&res).starts_with(&format!("/?msg=REFUSED BAD_VALUE: {field} ")), "{res}");
    }
    assert_eq!(text(&web), before, "fleet.toml byte-identical");
    assert_eq!(std::fs::read_to_string(&web.state).unwrap(), log, "no event");
    let before = text(&cli);
    for v in ["opus\"\n[[notify]]\ncmd = \"touch /tmp/pwned\"\n#", "x\"\nprofile = \"evil", "a\\b", ""] {
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
    let (cfg, log) = (std::fs::read_to_string(&web.config).unwrap(), std::fs::read_to_string(&web.state).unwrap());
    for body in ["op=pause&fleet=sendmeter&mtime=1", "op=mv&fleet=sendmeter&issue=12&before=17&mtime=1", "op=set&fleet=sendmeter&role=impl&model=sonnet"] {
        let res = post(&s, body, "");
        assert!(res.starts_with("HTTP/1.1 303 "), "{res}");
        assert!(location(&res).starts_with("/?msg=REFUSED STALE: "), "{res}");
    }
    assert_eq!(std::fs::read_to_string(&web.config).unwrap(), cfg);
    assert_eq!(std::fs::read_to_string(&web.state).unwrap(), log);
    assert!(get(&s, "/?msg=REFUSED%20STALE%3A%20x").contains("<p class=\"msg r\">REFUSED STALE: x</p>"));
}

#[test]
fn foreign_host_or_origin_is_403_and_read_only_is_405() {
    let (_, web) = pair("origin");
    let r = Fake::default();
    let s = Server::new(&web, &r, Some(ADDR.into()));
    let body = format!("op=pause&fleet=sendmeter&mtime={}", mtime(&s));
    let log = std::fs::read_to_string(&web.state).unwrap();
    for extra in ["Origin: https://evil.example\r\n", "Origin: http://127.0.0.1:7777.evil\r\n", "Origin: null\r\n"] {
        assert!(post(&s, &body, extra).starts_with("HTTP/1.1 403 "), "{extra}");
    }
    let wrong = format!("POST / HTTP/1.1\r\nHost: evil.example\r\nContent-Length: {}\r\n\r\n{body}", body.len());
    assert!(String::from_utf8_lossy(&s.respond(&wrong, 1_000_000)).starts_with("HTTP/1.1 403 "));
    assert_eq!(std::fs::read_to_string(&web.state).unwrap(), log, "nothing written");
    // `tailscale serve`: the ts.net name from [serve].allowed_hosts, over https.
    let ts = format!(
        "POST / HTTP/1.1\r\nHost: mac.tail1.ts.net:7443\r\nOrigin: https://mac.tail1.ts.net:7443\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    assert!(String::from_utf8_lossy(&s.respond(&ts, 1_000_000)).starts_with("HTTP/1.1 303 "));
    assert!(post(&s, &format!("op=resume&fleet=sendmeter&mtime={}", mtime(&s)), &format!("Origin: http://{ADDR}\r\n")).starts_with("HTTP/1.1 303 "));
    // --read-only: today's behaviour, no buttons.
    let ro = Server::new(&web, &r, None);
    assert!(post(&ro, &body, "").starts_with("HTTP/1.1 405 "));
    let page = get(&ro, "/");
    assert!(!page.contains("<form method=post") && page.contains("data-ro=1"), "no buttons");
}

#[test]
fn page_reads_only_cached_pr_data_and_serves_pwa_assets() {
    let d = tmpdir("pwa");
    let c = ctx(&d, &config());
    run(&c, &Fake::default(), &["queue", "sendmeter", "add", "12"]);
    append(&c.state, &Event::new(999_000, "sendmeter", Some(12), "spawn", json!({"role": "impl", "agent": "sendmeter-impl-12"}))).unwrap();
    let cache = c.state.parent().unwrap().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let pr = json!({"number": 34, "state": "OPEN", "statusCheckRollup": [{"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "SUCCESS"}]});
    std::fs::write(cache.join("pr-sendmeter-12.json"), json!({"ts": 1, "branch": "issue-12", "pr": pr}).to_string()).unwrap();
    let r = Fake::default();
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let page = get(&s, "/");
    assert!(page.contains("pull/34\">PR #34</a> · CI <span class=g>✓</span>"), "{page}");
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
        assert!(head.starts_with("HTTP/1.1 200 OK") && head.contains(ctype), "{path}: {head}");
    }
    for n in [180u32, 192, 512] {
        let res = s.respond(&format!("GET /icon-{n}.png HTTP/1.1\r\n\r\n"), 1);
        let png = &res[res.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4..];
        let dim = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n") && (dim(16), dim(20)) == (n, n), "icon-{n}.png dimensions");
    }
    assert!(get(&s, "/manifest.webmanifest").contains("\"display\": \"standalone\""));
    assert!(page.contains("rel=manifest href=/manifest.webmanifest") && std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/serve.js")).unwrap().contains("serviceWorker.register(R + 'sw.js')"));
    assert!(page.contains("data-ts=1000000"), "render time for the stale badge");
}

/// Two registered fleets plus one unregistered dir; orch/design/impl/rev/other placed.
#[test]
fn lanes_group_by_fleet_with_others() {
    let d = tmpdir("lanes");
    let two = format!("{CONFIG}\n[fleet.synergy]\nrepo = \"o/synergy-apps\"\nlocal = \"/p/synergy-apps\"\nintegration = \"staging\"\n");
    let c = ctx(&d, &two);
    let cfg = herdsman::config::Config::load(&c.config).unwrap();
    let ev = |fleet: &str, issue: Option<u64>, d: serde_json::Value| append(&c.state, &Event::new(900_000, fleet, issue, "spawn", d)).unwrap();
    ev("sendmeter", None, json!({"role": "orch", "agent": "sendmeter-orch", "harness": "hermes"}));
    ev("sendmeter", Some(12), json!({"role": "impl", "agent": "sendmeter-impl-12", "harness": "claude", "model": "opus", "effort": "high"}));
    ev("sendmeter", Some(9), json!({"role": "rev", "agent": "sendmeter-rev-9-r1", "harness": "hermes"}));
    ev("synergy", Some(5), json!({"role": "design", "agent": "synergy-design-5", "harness": "claude"}));
    ev("synergy", Some(7), json!({"role": "impl", "agent": "synergy-impl-7", "harness": "claude", "pane": "w5:p1"}));
    let events = read(&c.state).unwrap();
    let st = herdsman::events::fold(&events);
    let wt = c.worktrees.join("synergy-apps/designer").to_string_lossy().into_owned();
    let agents = json!([
        {"name": "sendmeter-orch", "agent_status": "idle", "pane_id": "w1:p1", "cwd": "/x"},
        {"name": "sendmeter-impl-12", "agent_status": "working", "pane_id": "w1:p2", "cwd": "/x"},
        {"name": "synergy-design-5", "agent_status": "done", "pane_id": "w2:p1", "cwd": "/x"},
        {"agent": "claude", "agent_status": "working", "pane_id": "w3:p1", "cwd": wt, "workspace_id": "w3"},
        {"agent": "codex", "agent_status": "idle", "pane_id": "w4:p1", "cwd": "/Users/me/Projects/side", "workspace_id": "w4"},
        {"agent": "dsh-tui", "agent_status": "blocked", "pane_id": "w5:p1", "cwd": "/p/synergy-apps", "workspace_id": "w5"},
    ]);
    let ws = [("w4".to_string(), "side-project".to_string())].into();
    let lanes = herdsman::lanes::join(&cfg, &c.worktrees, &events, &st, agents.as_array().unwrap(), &ws);
    let got: Vec<(Option<&str>, &str, &str, &str, bool)> =
        lanes.iter().map(|l| (l.fleet.as_deref(), l.name.as_str(), l.role.as_str(), l.state.as_str(), l.mismatch)).collect();
    assert_eq!(
        got,
        [
            (Some("sendmeter"), "sendmeter-orch", "orch", "idle", false),
            (Some("sendmeter"), "sendmeter-impl-12", "impl", "working", false),
            (Some("synergy"), "synergy-design-5", "design", "done", false),
            (Some("synergy"), "w3", "other", "working", false),
            (None, "side-project", "other", "idle", false),
            // Unnamed dsh-TUI agent: matched to its lane by pane.
            (Some("synergy"), "synergy-impl-7", "impl", "blocked", false),
            // The log has rev #9 open; herdr has no pane for it.
            (Some("sendmeter"), "sendmeter-rev-9-r1", "rev", "dead", true),
        ]
    );
    assert_eq!((lanes[1].model.as_deref(), lanes[1].effort.as_deref(), lanes[1].issue), (Some("opus"), Some("high"), Some(12)));
    // Rendered: state sections, fleet subgroups (Others last), the mismatch flagged.
    let list = json!({"result": {"agents": agents}}).to_string();
    let r = Fake::default().reply("herdr agent list", &list).reply("herdr pane read w1:p2", "⏺ Bash(cargo test)\n> \n");
    let page = get(&Server::new(&c, &r, None), "/");
    let at = |s: &str| page.find(s).unwrap_or_else(|| panic!("{s} missing"));
    let working = &page[at("<h2><span class=g>working (2)</span></h2><h3>sendmeter (1)</h3>")..];
    let rest = &working[4..];
    assert!(rest.find("<h3>synergy (1)</h3>") < rest.find("<h2>"), "synergy subgroup inside working");
    assert!(page.contains("⚠ log/pane mismatch") && page.contains("<span class=act>⏺ Bash(cargo test)</span>"));
    // One herdr agent list per 10 s, one pane read per 30 s.
    let s = Server::new(&c, &r, None);
    for t in [0, 5, 29] {
        s.respond("GET / HTTP/1.1\r\n\r\n", 1_000_000 + t);
    }
    assert_eq!(r.count("herdr agent list"), 1 + 2);
    assert_eq!(r.count("herdr pane read w1:p2"), 1 + 1);
}

#[test]
fn activity_lines_per_harness_are_masked_and_cut() {
    use herdsman::lanes::{activity, mask, tail};
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
        ("export ANTHROPIC_API_KEY=sk-ant-api03-abcdefghijklmnop", "export ANTHROPIC_API_KEY=•••"),
        ("curl -H token sk-ant-api03-abcdefghijklmnop", "curl -H token sk-•••"),
        ("gh auth ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789", "gh auth ghp_•••"),
        ("gho_16C7e42F292c6912E7710c838347Ae178B4a done", "gho_••• done"),
        ("slack xoxb-1234567890-abcdefghij", "slack xox•••"),
        ("jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abc123def", "jwt •••"),
        ("sha 0123456789abcdef0123456789abcdef01234567 ok", "sha ••• ok"),
        ("b64 QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVowMTIzNDU2Nzg5", "b64 •••"),
        ("GITHUB_TOKEN=\"abc def\" next", "GITHUB_TOKEN=\"••• def\" next"),
        ("DB_PASSWORD=hunter2 cargo test", "DB_PASSWORD=••• cargo test"),
        ("cargo test --release -p herdsman serve_web", "cargo test --release -p herdsman serve_web"),
        ("/Users/jirathip/.herdr/worktrees/herdsman/issue-16/src", "/Users/jirathip/.herdr/worktrees/herdsman/issue-16/src"),
    ];
    for (raw, want) in secrets {
        assert_eq!(mask(raw), want, "{raw}");
    }
    assert_eq!(activity("claude", "⏺ Bash(export OPENAI_API_KEY=sk-proj-abcdefghijklmnopqrstuvwxyz)"), "⏺ Bash(export OPENAI_API_KEY=•••)");
    let lines: String = (1..=14).map(|i| format!("line {i}\n\n")).collect::<String>() + "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789\n";
    let t = tail(&lines, 10);
    assert_eq!((t.len(), t[0].as_str(), t[9].as_str()), (10, "line 6", "ghp_•••"));
}

/// AC3 + amendment A.2: no table, every queue row a <details>, and (where WebKit is at hand)
/// `body.scrollWidth` ≤ 390 at 390 px with every row open, in all three lane modes.
#[test]
fn page_fits_390px_without_horizontal_scroll() {
    let d = tmpdir("phone");
    let c = ctx(&d, &config());
    run(&c, &Fake::default(), &["queue", "sendmeter", "add", "17", "18", "12"]);
    // #36: the Queue rows' title line (issue + differing PR title) stays one line at 390 px.
    let cache = d.join("state/cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("issue-sendmeter-12.json"), json!({"title": "Queue rows show issue and PR titles from the cached reads"}).to_string()).unwrap();
    std::fs::write(cache.join("pr-sendmeter-12.json"), json!({"pr": {"number": 33, "title": "src: titles in Queue"}}).to_string()).unwrap();
    let long = "sendmeter-impl-12-with-a-very-long-agent-name-that-would-push-a-table-wide";
    append(&c.state, &Event::new(999_000, "sendmeter", Some(12), "spawn", json!({"role": "impl", "agent": long, "branch": "issue-12-a-very-long-branch-name-without-any-break-points-at-all"}))).unwrap();
    append(&c.state, &Event::new(999_100, "sendmeter", Some(12), "blocked", json!({"code": "CI_NOT_GREEN", "reason": "x".repeat(300)}))).unwrap();
    let list = json!({"result": {"agents": [{"name": long, "agent_status": "working", "pane_id": "w1:p1"}]}}).to_string();
    let screen = format!("⏺ Bash({})\n{}\n", "y".repeat(200), "z".repeat(400));
    let r = Fake::default().reply("herdr agent list", &list).reply("herdr pane read", &screen);
    let page = get(&Server::new(&c, &r, Some(ADDR.into())), "/");
    let html = &page[page.find("<!doctype").unwrap()..];
    assert!(!html.contains("<table") && !html.contains("<pre"), "no wide blocks");
    assert!(html.contains("width=device-width") && std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/serve.css")).unwrap().contains("overflow-wrap:anywhere"));
    assert!(html.contains("<details class=\"q\"><summary><span>#12</span>"), "rows are details");
    assert!(html.contains("class=tl>Queue rows show issue and PR titles from the …"), "title line");
    for t in ["queue", "lanes", "host", "events"] {
        assert!(html.contains(&format!("<section id={t}>")) && html.contains(&format!("<a href=#{t}>")), "{t} tab");
    }
    if std::process::Command::new("swift").arg("--version").output().is_err() || !cfg!(target_os = "macos") {
        eprintln!("skipping the WebKit scrollWidth check: no swift/WebKit here");
        return;
    }
    let file = d.join("page.html");
    std::fs::write(&file, html).unwrap();
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/webview.swift");
    for tab in ["queue", "lanes", "host", "events"] {
        for mode in ["horse", "ascii", "glyph"] {
            let o = swift().args([script, file.to_str().unwrap(), "390", tab, mode]).output().unwrap();
            let out = String::from_utf8_lossy(&o.stdout);
            let w: Vec<u32> = out.split_whitespace().filter_map(|n| n.parse().ok()).collect();
            assert!(o.status.success() && w.len() == 2, "{tab}/{mode}: {out} {}", String::from_utf8_lossy(&o.stderr));
            assert!(w[0] <= 390 && w[1] == 390, "{tab}/{mode}: scrollWidth {} at {}", w[0], w[1]);
        }
    }
}

/// Amendment C: a retired fleet in the log, 30 merged issues, a paused fleet with a dead lane.
#[test]
fn queue_shows_open_work_only_and_folds_paused_retired_and_noise() {
    let d = tmpdir("short");
    let two = format!("{}\n[fleet.synergy]\nrepo = \"o/synergy-apps\"\nlocal = \"/p/synergy-apps\"\nintegration = \"staging\"\npaused = true\n", config());
    let c = ctx(&d, &two);
    let ev = |fleet: &str, issue: u64, ts: u64, kind: &str, d: serde_json::Value| {
        append(&c.state, &Event::new(ts, fleet, Some(issue), kind, d)).unwrap()
    };
    // The retired `fleet` key: log only.
    append(&c.state, &Event::new(1, "fleet", None, "queue", json!({"op": "add", "issues": [3]}))).unwrap();
    ev("fleet", 3, 2, "spawn", json!({"role": "impl", "agent": "fleet-impl-3"}));
    run(&c, &Fake::default(), &["queue", "sendmeter", "add", "17", "18"]);
    for n in 100..130 {
        run(&c, &Fake::default(), &["queue", "sendmeter", "add", &n.to_string()]);
        ev("sendmeter", n, 990_000 + n, "merged", json!({"pr": (n + 1000).to_string(), "merge_sha": "abc", "into": "staging"}));
    }
    ev("sendmeter", 17, 999_000, "spawn", json!({"role": "impl", "agent": "sendmeter-impl-17"}));
    run(&c, &Fake::default(), &["queue", "synergy", "add", "1039", "1040"]);
    ev("synergy", 1039, 999_000, "spawn", json!({"role": "impl", "agent": "synergy-impl-1039"}));
    for k in 0..4 {
        ev("sendmeter", 17, 999_500 + k, if k % 2 == 0 { "clean" } else { "notify-failed" }, json!({}));
    }
    let list = json!({"result": {"agents": [{"name": "sendmeter-impl-17", "agent_status": "working", "pane_id": "w1:p1"}]}}).to_string();
    let r = Fake::default().reply("herdr agent list", &list);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let page = get(&s, "/");
    let section = |id: &str| {
        let a = page.find(&format!("<section id={id}>")).unwrap();
        page[a..a + page[a..].find("</section>").unwrap()].to_string()
    };
    let queue = section("queue");
    assert!(!queue.contains("fleet-impl-3") && !queue.contains("<span>fleet</span>"), "no retired fleet");
    let rows = queue.matches("<details class=\"q\">").count();
    assert_eq!(rows, 2 + 2, "open items only: sendmeter #17 #18, synergy #1039 #1040");
    assert!(queue.contains("<span>done today: 30</span>"), "{queue}");
    let done = &queue[queue.find("done today").unwrap()..];
    let done = &done[..done.find("</details>").unwrap()];
    assert_eq!(done.matches("<p>#").count(), 5, "at most 5: {done}");
    assert!(done.starts_with("done today: 30</span></summary><p>#129 ") && done.contains("pull/1129\">PR #1129</a>"));
    // Paused fleet: one collapsed line, its dead lane red with no buttons.
    assert!(queue.contains("<details class=paused><summary><span>synergy · <span class=r>paused</span> · 1 lane (dead)</span></summary>"));
    let syn = &queue[queue.find("class=paused").unwrap()..];
    assert!(syn.contains("<span>#1039</span><span class=r>dead</span>"), "{syn}");
    let dead_row = &syn[syn.find("<span>#1039</span>").unwrap()..];
    let dead_row = &dead_row[..dead_row.find("</details>").unwrap()];
    assert!(!dead_row.contains("<button"), "no action on a dead lane: {dead_row}");
    assert!(queue.contains("<span>#17</span><span class=a>impl</span>"), "live lane stays impl");
    assert!(queue.contains("stall check: #17 in"), "paused fleet's lane is not counted");
    // Events: noise folded into one count, `?all=1` unfolds it.
    assert_eq!(section("events"), "<section id=events>", "#60: Events loads on tab open");
    assert!(get(&s, "/frag?id=events").contains("4 × clean / notify-failed"));
    let all = get(&s, "/?all=1");
    assert!(!all.contains("× clean") && all.matches("<p class=ev>").count() > 50);
}

/// #57: a request never samples the host (serve's 10 s thread does); the page links its scripts,
/// styles and horses under `/a/<hash8>/` (immutable, the SW's precache list) and stays `no-store`.
#[test]
fn page_never_samples_the_host_and_links_immutable_assets() {
    let c = ctx(&tmpdir("assets"), &config());
    let r = Fake::default().reply("vm_stat", "Swapouts: 100.\n").reply("sysctl", "1\n");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let page = get(&s, "/");
    assert_eq!(r.count("vm_stat"), 0, "{:?}", r.calls.borrow());
    assert!(!r.called("sysctl") && page.contains("Cache-Control: no-store") && !page.contains("<style>") && !page.contains("<script>"));
    let i = page.find("<script src=/a/").unwrap() + 15;
    let v = &page[i..i + 8];
    assert!(v.chars().all(|c| c.is_ascii_hexdigit()), "{v}");
    let sw = get(&s, "/sw.js");
    for (f, ctype) in [("serve.css", "text/css"), ("herd.css", "text/css"), ("serve.js", "text/javascript"), ("herd.js", "text/javascript"), ("horses.svg", "image/svg+xml")] {
        let res = get(&s, &format!("/a/{v}/{f}"));
        assert!(res.starts_with("HTTP/1.1 200 OK") && res.contains(ctype) && res.contains("Cache-Control: public, max-age=31536000, immutable\r\n"), "{f}");
        assert!(page.contains(&format!("/a/{v}/{f}")) || f == "horses.svg", "{f} linked");
        assert!(sw.contains(&format!("'{f}'")) && sw.contains(&format!("P + 'a/{v}/'")), "{f} precached");
    }
    assert!(get(&s, "/a/00000000/serve.js").starts_with("HTTP/1.1 404 "), "another build's hash");
    assert!(get(&s, "/icon-192.png").contains("Cache-Control: public, max-age=86400\r\n") && get(&s, "/sw.js").contains("Cache-Control: no-store\r\n"));
    assert_eq!(r.count("vm_stat"), 0);
}

/// #67: the fetch-only reads answer `fetch()` (no Sec-Fetch headers) with UTF-8 JSON; a navigation to any of
/// them is sent to the board, so a restored or typed fetch URL never becomes the page.
#[test]
fn fetch_only_reads_redirect_navigations_to_the_board() {
    let (c, r) = (ctx(&tmpdir("frags-nav"), &config()), Fake::default());
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let send = |path: &str, extra: &str| String::from_utf8_lossy(&s.respond(&format!("GET {path} HTTP/1.1\r\nHost: {ADDR}\r\n{extra}\r\n"), 1_000_000)).into_owned();
    for path in ["/frags", "/?frag=all", "/herd.json"] {
        let res = send(path, "");
        assert!(res.starts_with("HTTP/1.1 200 OK") && res.contains("Content-Type: application/json; charset=utf-8\r\n"), "{path}: {res}");
        let j: serde_json::Value = serde_json::from_str(&res[res.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert!(j.is_object(), "{path}");
    }
    assert!(send("/frags", "").contains("\"queue\""), "frags by element id");
    for path in ["/frags", "/?frag=all", "/herd.json", "/lane?agent=sendmeter-impl-12"] {
        for nav in ["Sec-Fetch-Mode: navigate\r\n", "Sec-Fetch-Dest: document\r\n", "Accept: text/html,application/xhtml+xml\r\n"] {
            let res = send(path, nav);
            assert!(res.starts_with("HTTP/1.1 303 See Other\r\nLocation: /\r\n"), "{path} {nav}: {res}");
        }
        assert!(!send(path, "Sec-Fetch-Mode: cors\r\nAccept: */*\r\n").starts_with("HTTP/1.1 303"), "{path}: fetch() is not redirected");
    }
    assert!(send("/", "Sec-Fetch-Mode: navigate\r\n").starts_with("HTTP/1.1 200 OK"), "the board itself");
    // The SW answers a navigation off the page with the page, and never caches `frags`; the page fetches `frags`.
    let sw = send("/sw.js", "");
    assert!(sw.contains("e.request.mode === 'navigate' && u.pathname !== P) return e.respondWith(caches.match(P).then(m => m || fetch(P)))"), "{sw}");
    assert!(sw.contains("P + 'frags'") && !sw.contains("frag=all"));
    let v = send("/", "");
    let v = &v[v.find("/a/").unwrap()..][..11];
    assert!(send(&format!("{v}/serve.js"), "").contains("fetch(R + 'frags'"));
}
