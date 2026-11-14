mod common;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use common::*;
use drover::serve::Server;

const ADDR: &str = "127.0.0.1:7777";
const T: u64 = 1_000_000;

fn req(s: &Server, method: &str, path: &str, body: &str, extra: &str, now: u64) -> String {
    let r = format!(
        "{method} {path} HTTP/1.1\r\nHost: {ADDR}\r\n{extra}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    String::from_utf8_lossy(&s.respond(&r, now)).into_owned()
}

fn body(res: &str) -> &str {
    &res[res.find("\r\n\r\n").unwrap() + 4..]
}

fn frags(s: &Server, cookie: &str, now: u64) -> (String, String) {
    let [needs, _, chat] = parts(s, ADDR, cookie, now);
    (chat, needs)
}

fn demo() -> drover::Ctx {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::env::set_current_dir(&root).unwrap();
    let d = tmpdir("chat-demo");
    let c = ctx(&d, &std::fs::read_to_string(root.join("demo/fleet.toml")).unwrap());
    std::fs::create_dir_all(c.state.parent().unwrap()).unwrap();
    std::fs::copy(root.join("demo/events.jsonl"), &c.state).unwrap();
    c
}

#[test]
fn the_tab_shows_each_repo_with_its_threads_and_a_thread_as_chat_blocks() {
    let c = demo();
    let r = Fake::default();
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let now = 1_793_429_670;
    let cookie = pair(&s, &c, "Mozilla/5.0 (iPhone)", now);
    let page = req(&s, "GET", "/", "", &format!("{cookie}Cookie: tab=chat\r\n"), now);
    assert!(
        page.contains("<a href=#lanes><span>lanes</span></a><a href=#chat><span>chat</span></a></nav>"),
        "[chat] is the last of three tabs (#440)"
    );
    let (chat, needs) = frags(&s, &cookie, now);
    let at = |s: &str| chat.find(s).unwrap_or_else(|| panic!("{s} not in {chat}"));
    assert!(at("<b>all repos</b>") < at("<b>acme-api</b>") && at("<b>acme-api</b>") < at("<b>acme-web</b>"));
    assert!(chat.contains(
        "<a class=\"tr\" href=#chat data-t=\"t4\"><i class=ud>●</i><span class=tt><span>Rate limits for /v1/search</span><small class=tid>· t4</small></span>"
    ));
    assert!(chat.contains("<b class=r>! choose</b>conductor: Return 429 with `Retry-After` only"));
    assert!(
        chat.contains("<a class=\"tr rd\" href=#chat data-t=\"t2\">"),
        "t2 was read"
    );
    assert!(at("data-t=\"t4\"><i") < at("data-t=\"t2\"><i"), "newest first");
    assert!(chat.contains("<span class=btns><button type=button class=ntb data-repo=\"all\">+ thread</button></span>"));
    let t4 = &chat[at("<div class=cvp data-t=\"t4\">")..];
    let t4 = &t4[..t4.find("</form></div>").unwrap()];
    for want in [
        "<span class=ds><i class=dt></i>acme-api · claude · opus · awake</span>",
        "conductor in ",
        " class=\"blk me\" open><summary><span class=k>you</span>",
        " class=\"blk ai\" open><summary><span class=k>conductor</span><span class=pv>I see it:",
        "<code>/v1/search</code>",
        "<figure class=im><img src=\"thread/t4/IMG%5F2041%2EPNG\"",
        "<figcaption>IMG_2041.PNG · 218 KB</figcaption>",
        " class=\"blk run\"><summary><span class=k>$</span><span class=cmd>drover brief acme-api 19</span>",
        "<p class=ans>✓ 1. per API key <span class=m>· you · ",
        "<p class=ans>✓ other: \"600 a minute, burst 100\" <span class=m>· you · ",
        "<a class=tf href=\"thread/t4/429%2Dbody%2Ejson\"><span>▤ 429-body.json</span><span class=m>87 B</span></a>",
        "<a href=\"https://docs.github.com/rest/using-the-rest-api/rate-limits-for-the-rest-api\">",
        " class=\"qs note\">⇄ switched to account B",
        "<form class=chq data-id=\"q33\" data-fleet=\"acme-api\"><button type=button name=choice value=1><span>1. both:",
        "<small>(recommended)</small>",
        "placeholder=\"other: type an answer\"",
        "<p class=tlv><b>●</b> waiting for your answer",
        "<form class=tw data-t=\"t4\"><input type=file multiple hidden>",
    ] {
        assert!(t4.contains(want), "{want} not in {t4}");
    }
    assert!(!t4.contains("DECISION q31"), "a DECISION line shows as the ✓ line only");
    assert!(
        chat.contains("<span class=ds>all · claude · opus · asleep</span>"),
        "{chat}"
    );
    assert!(!t4.contains("disabled"), "a paired owner device writes");
    assert!(needs.contains("Rate-limit the public search endpoint") && needs.contains("X-RateLimit"));
    assert!(
        needs.contains("asked in <a href=#chat class=ath data-go=\"t4\">Rate limits for /v1/search · t4</a>"),
        "{needs}"
    );
    assert!(needs.contains("<i id=chn hidden data-n=2></i>"), "{needs}");
}

fn setup(name: &str) -> drover::Ctx {
    let d = tmpdir(name);
    let c = ctx(
        &d,
        &format!("{CONFIG}\n[fleet.sendmeter.role.orchestrator]\nharness = \"claude\"\nmodel = \"opus\"\n"),
    );
    let profile = c.config.with_file_name("claude").join("fleet-orchestrator");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("settings.json"), "{}").unwrap();
    c
}

fn acp_setup(name: &str, b: &FakeShed) -> drover::Ctx {
    let cfg = format!(
        "{CONFIG}\n[fleet.sendmeter.role.orchestrator]\nharness = \"claude\"\nmodel = \"opus\"\naccount = \"C\"\n\n\
         [fleet.sendmeter.runtime]\nkind = \"shed-acp\"\nsocket = \"{}\"\n\n[accounts]\nC = \"C\"\n",
        b.sock.display()
    );
    let mut c = ctx(&tmpdir(name), &cfg);
    c.exe = env!("CARGO_BIN_EXE_drover").into();
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    c
}

fn acp() -> Fake {
    let agents =
        serde_json::json!([{"id": "claude-acp", "version": "0.101.0", "distribution": "npx", "cmd": ["echo"]}]);
    Fake::default()
        .reply("shed agents --json", &agents.to_string())
        .reply("shed run", "")
        .reply("shed send", "")
}

const START_T1: &str = "shed run --acp claude-acp --policy allow --name orchestrator-t1";

fn leaked() -> &'static FakeShed {
    Box::leak(Box::new(FakeShed::new()))
}

#[test]
fn a_thread_starts_from_the_board_and_a_tapped_choice_types_its_decision_into_the_conductor() {
    let b = FakeShed::new();
    let c = acp_setup("chat-ask", &b);
    let f = acp();
    let cli = b.cli(&f);
    let r = b.on(&cli);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let cookie = pair(&s, &c, "Mozilla/5.0 (iPhone)", T);
    let res = req(
        &s,
        "POST",
        "/thread",
        "op=new&repo=sendmeter&text=Grill+%2312",
        &cookie,
        T,
    );
    assert!(
        res.starts_with("HTTP/1.1 200 OK") && body(&res) == "t1 started\n",
        "{res}"
    );
    assert!(f.called(START_T1), "{:?}", f.calls.borrow());
    assert!(b.has("orchestrator-t1"));
    let ask = [
        "ask",
        "sendmeter",
        "-",
        "Which scope?",
        "--option",
        "small",
        "--option",
        "full",
        "--recommend",
        "2",
        "--thread",
        "t1",
    ];
    assert_eq!(run(&c, &r, &ask).1, "q1\n");
    let (chat, _) = frags(&s, &cookie, T + 5);
    assert!(
        chat.contains("<form class=chq data-id=\"q1\" data-fleet=\"sendmeter\"><button type=button name=choice value=2><span>2. full <small>(recommended)</small></span></button><button type=button name=choice value=1><span>1. small</span></button>"),
        "{chat}"
    );
    let res = req(
        &s,
        "POST",
        "/thread",
        "op=answer&fleet=sendmeter&id=q1&choice=1",
        &cookie,
        T + 10,
    );
    assert_eq!(body(&res), "answered q1\n", "{res}");
    assert!(
        f.called("shed send orchestrator-t1 DECISION q1: small"),
        "{:?}",
        f.calls.borrow()
    );
    let (chat, _) = frags(&s, &cookie, T + 20);
    assert!(!chat.contains("data-id=\"q1\""), "the rows fold");
    assert!(
        chat.contains("<p class=ans>✓ 1. small <span class=m>· you · "),
        "{chat}"
    );
    let ask2 = ["ask", "sendmeter", "-", "How fast?", "--option", "a", "--thread", "t1"];
    assert_eq!(run(&c, &r, &ask2).1, "q2\n");
    let res = req(
        &s,
        "POST",
        "/thread",
        "op=answer&fleet=sendmeter&id=q2&text=600+a+minute",
        &cookie,
        T + 30,
    );
    assert_eq!(body(&res), "answered q2\n", "{res}");
    assert!(f.called("shed send orchestrator-t1 DECISION q2: 600 a minute"));
    assert!(frags(&s, &cookie, T + 40).0.contains("✓ other: \"600 a minute\""));
}

#[test]
fn an_attached_image_reaches_the_conductor_and_its_reply_file_shows_inline() {
    let b = FakeShed::new();
    let c = acp_setup("chat-img", &b);
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Look"]).1,
        "t1\n"
    );
    let f = acp();
    let cli = b.cli(&f);
    assert_eq!(
        run(&c, &cli, &["thread", "say", "t1", "hi"]).0,
        0,
        "its conductor is up"
    );
    let r = b.on(&cli);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let cookie = pair(&s, &c, "Mozilla/5.0 (iPhone)", T);
    let png: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
    let data: String = url_enc(&B64.encode(&png));
    let res = req(
        &s,
        "POST",
        "/thread",
        &format!("op=say&t=t1&text=see+this&name=IMG_1.PNG&data={data}"),
        &cookie,
        T,
    );
    assert!(res.starts_with("HTTP/1.1 200 OK"), "{res}");
    let saved = c
        .state
        .with_file_name("orchestrator")
        .join("t1/uploads")
        .join(format!("{T}-IMG_1.PNG"));
    assert_eq!(std::fs::read(&saved).unwrap(), png, "in the conductor's folder");
    assert!(
        f.called(&format!(
            "shed send orchestrator-t1 [t1] owner: see this (attached: {})",
            saved.display()
        )),
        "the conductor gets its path: {:?}",
        f.calls.borrow()
    );
    let got = s.respond(
        &format!("GET /thread/t1/IMG%5F1%2EPNG HTTP/1.1\r\nHost: {ADDR}\r\n\r\n"),
        T,
    );
    let at = got.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    assert!(String::from_utf8_lossy(&got[..at]).contains("Content-Type: image/png"));
    assert_eq!(&got[at..], &png[..]);

    let folder = c.state.with_file_name("orchestrator").join("t1");
    let mock = folder.join("mockup-390.png");
    std::fs::write(&mock, b"\x89PNG mock").unwrap();
    let (code, out) = run(
        &c,
        &r,
        &["thread", "reply", "t1", "Here it is.", "--file", mock.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{out}");
    let (chat, _) = frags(&s, &cookie, T + 5);
    assert!(chat.contains("<img src=\"thread/t1/IMG%5F1%2EPNG\""), "{chat}");
    assert!(chat.contains("<img src=\"thread/t1/mockup%2D390%2Epng\""), "{chat}");
    let got = req(&s, "GET", "/thread/t1/mockup-390.png", "", "", T + 5);
    assert!(got.ends_with("\r\n\r\n\u{FFFD}PNG mock"), "{got}");
    let res = req(&s, "GET", "/thread/t1/fleet.toml", "", "", T + 5);
    assert!(res.starts_with("HTTP/1.1 404"), "{res}");
    let (code, out) = run(&c, &r, &["thread", "reply", "t1", "x", "--file", "/no/such.png"]);
    assert_ne!(code, 0, "{out}");
    let toml = c.config.to_str().unwrap();
    let (code, out) = run(&c, &r, &["thread", "reply", "t1", "x", "--file", toml]);
    assert_ne!(code, 0, "{out}");
    assert!(!drover::events::jsonl(&c.state).unwrap().contains(toml), "never logged");
    std::os::unix::fs::symlink(&c.config, folder.join("link.toml")).unwrap();
    let line = |n: &str, p: &str| {
        format!("{{\"ts\":{T},\"fleet\":\"sendmeter\",\"type\":\"thread-msg\",\"thread\":\"t1\",\"from\":\"conductor\",\"text\":\"x\",\"files\":[{{\"name\":\"{n}\",\"path\":\"{p}\",\"size\":1}}]}}\n")
    };
    let mut log = drover::events::jsonl(&c.state).unwrap();
    log += &line("keys.toml", toml);
    log += &line("link.toml", folder.join("link.toml").to_str().unwrap());
    rewrite_log(&c.state, &log);
    for n in ["keys.toml", "link.toml"] {
        let res = req(&s, "GET", &format!("/thread/t1/{n}"), "", "", T + 5);
        assert!(res.starts_with("HTTP/1.1 404"), "{n}: {res}");
    }
}

#[test]
fn a_reply_file_from_a_lane_worktree_is_copied_into_the_thread_folder() {
    let c = setup("chat-wt-file");
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Look"]).1,
        "t1\n"
    );
    let design = c.worktrees.join("sendmeter/issue-12/docs/design/inbox");
    std::fs::create_dir_all(&design).unwrap();
    let png = design.join("inbox-390.png");
    std::fs::write(&png, b"\x89PNG design").unwrap();
    let r = Fake::default();
    let (code, out) = run(
        &c,
        &r,
        &["thread", "reply", "t1", "The mockup.", "--file", png.to_str().unwrap()],
    );
    assert_eq!(code, 0, "{out}");
    let folder = c.state.with_file_name("orchestrator").join("t1");
    let log = drover::events::jsonl(&c.state).unwrap();
    let msg = log.lines().rfind(|l| l.contains("The mockup.")).unwrap();
    let e: serde_json::Value = serde_json::from_str(msg).unwrap();
    let f = &e["files"][0];
    assert_eq!(f["name"], "inbox-390.png", "{msg}");
    let copy = std::path::PathBuf::from(f["path"].as_str().unwrap());
    assert!(copy.starts_with(folder.canonicalize().unwrap()), "{msg}");
    assert_eq!(std::fs::read(&copy).unwrap(), b"\x89PNG design");

    let away = c.worktrees.parent().unwrap().join("elsewhere");
    for p in [away.join("x.png"), c.worktrees.join("other/issue-1/x.png")] {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(&p, b"x").unwrap();
        let (code, out) = run(&c, &r, &["thread", "reply", "t1", "x", "--file", p.to_str().unwrap()]);
        assert_ne!(code, 0, "{}: {out}", p.display());
        assert!(!drover::events::jsonl(&c.state).unwrap().contains(p.to_str().unwrap()));
    }
}

fn url_enc(s: &str) -> String {
    s.replace('+', "%2B").replace('/', "%2F").replace('=', "%3D")
}

#[test]
fn a_device_that_is_not_paired_reads_the_thread_but_cannot_write() {
    let b = FakeShed::new();
    let c = acp_setup("chat-guest", &b);
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Grill"]).1,
        "t1\n"
    );
    let ask = [
        "ask",
        "sendmeter",
        "-",
        "Which?",
        "--option",
        "a",
        "--option",
        "b",
        "--thread",
        "t1",
    ];
    assert_eq!(run(&c, &Fake::default(), &ask).1, "q1\n");
    let f = acp();
    let cli = b.cli(&f);
    let r = b.on(&cli);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let (chat, _) = frags(&s, "", T);
    assert!(
        chat.contains("<p class=\"ns m\">pair this phone to answer or write</p>"),
        "{chat}"
    );
    assert!(
        chat.contains("<button type=button name=choice value=1 disabled>"),
        "{chat}"
    );
    assert!(
        chat.contains("<textarea name=text rows=1 aria-label=message placeholder=\"message the conductor\" disabled>")
    );
    assert!(
        !chat.contains("class=ntb") && !chat.contains("class=nt "),
        "no [+ thread]"
    );
    for b in [
        "op=say&t=t1&text=hi",
        "op=answer&fleet=sendmeter&id=q1&choice=1",
        "op=new&repo=sendmeter&title=x",
    ] {
        let res = req(&s, "POST", "/thread", b, "", T);
        assert!(res.starts_with("HTTP/1.1 401"), "{b}: {res}");
    }
    assert!(
        !f.called("shed send") && !f.called("shed run"),
        "{:?}",
        f.calls.borrow()
    );
}

#[test]
fn a_large_attachment_arrives_whole_over_the_socket() {
    use std::io::{Read, Write};
    let b = leaked();
    let c = acp_setup("chat-big", b);
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Look"]).1,
        "t1\n"
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let cookie = pair(
        &Server::new(&c, &Fake::default(), Some(ADDR.into())),
        &c,
        "Mozilla/5.0 (iPhone)",
        now,
    );
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let c2 = c.clone();
    std::thread::spawn(move || {
        let f = acp();
        drover::serve::serve_on(&c2, &b.cli(&f), l, false)
    });
    let png: Vec<u8> = (0..600_000u32).map(|i| (i % 253) as u8).collect();
    let body = format!("op=say&t=t1&text=big&name=big.png&data={}", url_enc(&B64.encode(&png)));
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    write!(
        s,
        "POST /thread HTTP/1.1\r\nHost: {addr}\r\n{cookie}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut res = String::new();
    s.read_to_string(&mut res).unwrap();
    assert!(res.starts_with("HTTP/1.1 200 OK"), "{res}");
    let up = c.state.with_file_name("orchestrator").join("t1/uploads");
    let saved = std::fs::read_dir(&up).unwrap().next().unwrap().unwrap().path();
    assert_eq!(std::fs::read(saved).unwrap(), png);
}

#[test]
fn the_service_worker_answers_a_thread_file_navigation_with_the_file() {
    if std::process::Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipping: no node here");
        return;
    }
    let c = setup("chat-sw");
    let sw = req(
        &Server::new(&c, &Fake::default(), Some(ADDR.into())),
        "GET",
        "/sw.js",
        "",
        "",
        T,
    );
    let js = format!(
        "const L = {{}}, log = [];
globalThis.self = {{ registration: {{ scope: 'http://h/' }}, addEventListener: (k, f) => {{ L[k] = f; }} }};
globalThis.location = {{ origin: 'http://h' }};
globalThis.fetch = async r => {{ log.push('net ' + (r.url ?? r)); return new Response('body of ' + (r.url ?? r)); }};
globalThis.caches = {{ match: async k => {{ log.push('cache ' + (k.url ?? k)); }}, open: async () => ({{ put() {{}} }}) }};
{}
const nav = async url => {{
  log.length = 0;
  let p = null;
  L.fetch({{ request: {{ url, method: 'GET', mode: 'navigate' }}, respondWith: r => {{ p = r; }}, waitUntil() {{}} }});
  return [p && await (await p).text(), ...log];
}};
(async () => console.log(JSON.stringify([await nav('http://h/thread/t1/IMG%5F1%2EPNG'), await nav('http://h/lane')])))();",
        body(&sw)
    );
    let f = tmpdir("chat-sw-js").join("sw.js");
    std::fs::write(&f, js).unwrap();
    let o = std::process::Command::new("node").arg(&f).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let got: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let file = "http://h/thread/t1/IMG%5F1%2EPNG";
    assert_eq!(
        got[0],
        serde_json::json!([
            format!("body of {file}"),
            format!("cache {file}"),
            format!("net {file}")
        ]),
        "the file, never the page"
    );
    assert_eq!(got[1], serde_json::json!(["body of /", "cache /", "net /"]), "the page");
}

#[test]
fn an_image_tap_opens_the_viewer_and_a_page_at_a_nested_path_still_loads_its_images() {
    if no_webkit() {
        return;
    }
    let c = setup("chat-viewer");
    let folder = c.state.with_file_name("orchestrator").join("t1");
    std::fs::create_dir_all(&folder).unwrap();
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
    for f in ["icon-180.png", "icon-192.png"] {
        std::fs::copy(assets.join(f), folder.join(f)).unwrap();
    }
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Look"]).1,
        "t1\n"
    );
    let (a, b) = (folder.join("icon-180.png"), folder.join("icon-192.png"));
    let reply = [
        "thread",
        "reply",
        "t1",
        "Two shots.",
        "--file",
        a.to_str().unwrap(),
        "--file",
        b.to_str().unwrap(),
    ];
    let (code, out) = run(&c, &Fake::default(), &reply);
    assert_eq!(code, 0, "{out}");
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let (c2, at) = (c.clone(), addr.clone());
    std::thread::spawn(move || {
        let r = Fake::default();
        let s = Server::new(&c2, &r, Some(at));
        for mut st in l.incoming().flatten() {
            let mut buf = [0u8; 8192];
            let n = std::io::Read::read(&mut st, &mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).replacen("GET /deep/thread/t1/icon-180.png ", "GET / ", 1);
            let _ = std::io::Write::write_all(&mut st, &s.respond(&head, T));
        }
    });
    let js = "const ptr = (k, x, y) => IV.dispatchEvent(new PointerEvent(k, { pointerId: 1, clientX: x, clientY: y, bubbles: true })); \
        const swipe = (x0, y0, x1, y1) => { ptr('pointerdown', x0, y0); ptr('pointermove', x1, y1); ptr('pointerup', x1, y1); }; \
        const tap2 = () => { for (const k of [0, 1]) { ptr('pointerdown', 195, 400); ptr('pointerup', 195, 400); } }; \
        (function go() { const im = [...$$('#chat .cvp[data-t=\"t1\"] .im img')]; \
          if (im.length < 2 || !im.every(i => i.complete)) return setTimeout(go, 50); \
          const href = location.href, ok = () => im.filter(i => i.naturalWidth > 0).length, at = [im.length, ok(), +(location.pathname === '/')]; \
          im[0].click(); \
          at.push(+!IV.hidden, +(location.href === href), +(IVI.src === im[0].src), +($('.ivn').textContent === '1 / 2')); \
          swipe(300, 400, 100, 400); \
          at.push(+(IVI.src === im[1].src), +($('.ivn').textContent === '2 / 2')); \
          tap2(); \
          at.push(+IVI.style.transform.includes('scale(2.5)')); \
          $('.ivx').click(); \
          at.push(+IV.hidden); \
          im[1].click(); swipe(200, 300, 200, 500); \
          at.push(+IV.hidden, ok(), +(location.href === href), errs.length); \
          window.late = at; })()";
    let png = tmpdir("chat-viewer-png").join("thread.png");
    let o = webview()
        .env("DROVER_WAIT", "3")
        .args([
            &format!("http://{addr}/deep/thread/t1/icon-180.png"),
            "390",
            "chat",
            "scene",
            png.to_str().unwrap(),
            &format!("chatOpen('t1'); {{ {js} }}"),
        ])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out} {}", String::from_utf8_lossy(&o.stderr));
    let m: Vec<f64> = out.split_whitespace().skip(2).filter_map(|n| n.parse().ok()).collect();
    assert_eq!(m, [2., 2., 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 2., 1., 0.], "{out}");
}

struct Held<'a>(
    common::fake_shed::Cli<'a>,
    std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    std::sync::Mutex<std::sync::mpsc::Sender<String>>,
);
impl drover::runner::Runner for Held<'_> {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<drover::runner::Out> {
        let typing = prog == "shed" && args[0] == "send";
        if typing {
            self.1.lock().unwrap().recv().unwrap();
        }
        let out = self.0.run(prog, args);
        if typing {
            self.2
                .lock()
                .unwrap()
                .send(format!("{prog} {}", args.join(" ")))
                .unwrap();
        }
        out
    }
}

#[test]
fn a_message_is_answered_once_logged_and_typed_into_the_conductor_after() {
    use std::io::{Read, Write};
    let b = leaked();
    let c = acp_setup("chat-async", b);
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Look"]).1,
        "t1\n"
    );
    assert_eq!(
        run(&c, &b.cli(&acp()), &["thread", "say", "t1", "hi"]).0,
        0,
        "its conductor is up"
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let cookie = pair(
        &Server::new(&c, &Fake::default(), Some(ADDR.into())),
        &c,
        "Mozilla/5.0 (iPhone)",
        now,
    );
    let (go, held) = std::sync::mpsc::channel();
    let (typed, seen) = std::sync::mpsc::channel();
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let c2 = c.clone();
    std::thread::spawn(move || {
        let f = acp();
        let r = Held(b.cli(&f), held.into(), typed.into());
        drover::serve::serve_on(&c2, &r, l, false)
    });
    let body = "op=say&t=t1&text=a+long+message";
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    write!(
        s,
        "POST /thread HTTP/1.1\r\nHost: {addr}\r\n{cookie}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    let mut res = String::new();
    s.read_to_string(&mut res).unwrap();
    assert!(
        res.starts_with("HTTP/1.1 200 OK") && res.ends_with("\r\n\r\nsent\n"),
        "{res}"
    );
    assert!(
        drover::events::jsonl(&c.state)
            .unwrap()
            .contains("\"from\":\"owner\",\"text\":\"a long message\""),
        "logged"
    );
    assert!(
        seen.try_recv().is_err(),
        "the reply came while the conductor was still not typed into"
    );
    go.send(()).unwrap();
    let line = seen.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    assert_eq!(line, "shed send orchestrator-t1 [t1] owner: a long message");
}

#[test]
fn a_failed_delivery_shows_red_with_retry_and_retry_types_it_again() {
    let b = FakeShed::new();
    let c = acp_setup("chat-retry", &b);
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Look"]).1,
        "t1\n"
    );
    let down = Fake::default();
    let on = b.on(&down);
    let s = Server::new(&c, &on, Some(ADDR.into()));
    let cookie = pair(&s, &c, "Mozilla/5.0 (iPhone)", T);
    let res = req(&s, "POST", "/thread", "op=say&t=t1&text=hello", &cookie, T);
    assert_eq!(body(&res), "sent\n", "{res}");
    let (chat, _) = frags(&s, &cookie, T + 5);
    let at = chat
        .find(" class=\"qs note r\">✗ not delivered: ")
        .unwrap_or_else(|| panic!("{chat}"));
    let line = &chat[at..at + chat[at..].find("</p>").unwrap()];
    let n = line.split("data-n=").nth(1).and_then(|x| x.split('>').next()).unwrap();
    assert!(
        line.ends_with(&format!(" <button type=button class=rty data-n={n}>retry</button>")),
        "{line}"
    );
    let f = acp();
    let cli = b.cli(&f);
    let r = b.on(&cli);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let res = req(&s, "POST", "/thread", &format!("op=retry&t=t1&n={n}"), &cookie, T + 10);
    assert_eq!(body(&res), "retrying\n", "{res}");
    assert!(f.called(START_T1), "{:?}", f.calls.borrow());
    assert!(
        f.called("shed send orchestrator-t1 ") && f.calls.borrow().iter().any(|l| l.ends_with("[t1] owner: hello"))
    );
    let log = drover::events::jsonl(&c.state).unwrap();
    assert_eq!(log.matches("\"from\":\"owner\",\"text\":\"hello\"").count(), 1, "{log}");
    let res = req(&s, "POST", "/thread", "op=retry&t=t1&n=1", &cookie, T + 20);
    assert!(body(&res).contains("nothing to retry"), "{res}");
    b.set("orchestrator-t1", |t| {
        t["reported"] = serde_json::json!({"state": "working"})
    });
    let none = Fake::default();
    let busy = b.on(&none);
    let (chat, _) = frags(&Server::new(&c, &busy, Some(ADDR.into())), &cookie, T + 30);
    assert!(
        chat.contains("<p class=\"tlv wk\" data-since=1000000><i class=tld aria-hidden=true></i> working</p>"),
        "{chat}"
    );
}

fn long(name: &str, n: u64, now: u64) -> (drover::Ctx, String) {
    fill(setup(name), n, now)
}

fn fill(c: drover::Ctx, n: u64, now: u64) -> (drover::Ctx, String) {
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Long"]).1,
        "t1\n"
    );
    for i in 1..=n {
        let d = serde_json::json!({"thread": "t1", "from": "conductor", "text": format!("m{i}")});
        let e = drover::events::Event::new(T + i, "sendmeter", None, "thread-msg", d);
        drover::events::append(&c.state, &e).unwrap();
    }
    let cookie = pair(
        &Server::new(&c, &Fake::default(), Some(ADDR.into())),
        &c,
        "Mozilla/5.0 (iPhone)",
        now,
    );
    (c, cookie)
}

#[test]
fn a_long_thread_carries_its_last_30_messages_and_older_ones_come_30_at_a_time() {
    let (c, cookie) = long("chat-window", 100, T);
    let r = Fake::default();
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let msgs = |h: &str| -> Vec<String> {
        h.split("<span class=pv>")
            .skip(1)
            .map(|p| p[..p.find('<').unwrap()].into())
            .collect()
    };
    let want = |a: u64, b: u64| (a..=b).map(|i| format!("m{i}")).collect::<Vec<_>>();
    let b = |h: &str| {
        h.split("class=\"qs tmo\" data-b=")
            .nth(1)
            .map(|s| s[..s.find('>').unwrap()].to_string())
    };
    let (chat, _) = frags(&s, &cookie, T + 200);
    let view = &chat[chat.find("<div class=cvp data-t=\"t1\">").unwrap()..];
    assert_eq!(msgs(view), want(71, 100));
    assert!(!view.contains("thread started"), "{view}");
    assert!(view.contains(" data-n="), "each block names its event");
    let page = |b: &str| {
        let j = api(&s, ADDR, &cookie, T + 200, &format!("thread/t1?before={b}&n=30"));
        board_js(&[(&j, &format!("chat.page({}, 't1', d, true)", T + 200))]).remove(0)
    };
    let p1 = page(&b(view).unwrap());
    assert_eq!(msgs(&p1), want(41, 70));
    let p2 = page(&b(&p1).unwrap());
    assert_eq!(msgs(&p2), want(11, 40));
    let p3 = page(&b(&p2).unwrap());
    assert_eq!(msgs(&p3), want(1, 10));
    assert!(
        p3.starts_with("<p class=qs>thread started by owner") && b(&p3).is_none(),
        "{p3}"
    );
    let no = req(&s, "GET", "/api/v1/thread/t9?before=5", "", &cookie, T + 200);
    assert!(no.starts_with("HTTP/1.1 404"), "{no}");
}

#[test]
fn scrolling_up_loads_older_messages_in_place_and_a_live_update_keeps_them() {
    if no_webkit() {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let b = leaked();
    let (c, cookie) = fill(acp_setup("chat-window-webkit", b), 100, now);
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        let f = acp();
        drover::serve::serve_on(&c, &b.cli(&f), l, false)
    });
    let js = "const tc = () => $('#chat .cvp[data-t=\"t1\"] .tc'), n = () => tc() ? tc().querySelectorAll(':scope>[data-n]').length : 0; \
        const until = f => new Promise(r => (function go() { f() ? r() : setTimeout(go, 50); })()); \
        (async () => { const at = window.late = []; await until(() => n()); at.push(n()); \
          for (const k of [60, 90, 100]) { tc().scrollTop = 1e6; await new Promise(r => setTimeout(r, 100)); tc().scrollTop = 0; const a = tc().querySelector(':scope>[data-n]'), y = a.getBoundingClientRect().top; \
            await until(() => n() >= k); at.push(n(), +(Math.abs(a.getBoundingClientRect().top - y) > 1)); await new Promise(r => setTimeout(r, 200)); } \
          at.push(+!!tc().querySelector('.tmo'), +tc().firstElementChild.textContent.startsWith('thread started')); \
          tc().scrollTop = tc().scrollHeight / 2; await new Promise(r => setTimeout(r, 200)); \
          const th = $('#chat .cvp[data-t=\"t1\"] .th'), r = th.getBoundingClientRect(); \
          at.push(Math.round(r.top - $('header').getBoundingClientRect().bottom), +th.contains(document.elementFromPoint(r.left + 40, r.top + r.height / 2)), +(tc().scrollTop > innerHeight && !document.scrollingElement.scrollTop)); \
          const f = $('#chat .tw[data-t=\"t1\"]'); f.elements.text.value = 'a new one'; f.requestSubmit(); \
          await until(() => [...tc().querySelectorAll(':scope>.blk.me:not(.snt) .pv')].some(p => p.textContent === 'a new one')); \
          at.push(+(tc().querySelector(':scope>[data-n] .pv').textContent === 'm1'), +tc().firstElementChild.textContent.startsWith('thread started'), +(n() > 100), errs.length); })()";
    let png = tmpdir("chat-window-png").join("thread.png");
    let o = webview()
        .env("DROVER_WAIT", "12")
        .env("DROVER_COOKIE", cookie.trim().trim_start_matches("Cookie: "))
        .args([
            &format!("http://{addr}/"),
            "390",
            "chat",
            "scene",
            png.to_str().unwrap(),
            &format!("chatOpen('t1'); {{ {js} }}"),
        ])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out} {}", String::from_utf8_lossy(&o.stderr));
    let m: Vec<f64> = out.split_whitespace().skip(2).filter_map(|n| n.parse().ok()).collect();
    assert_eq!(
        m,
        [30., 60., 0., 90., 0., 100., 0., 0., 1., 0., 1., 1., 1., 1., 1., 0.],
        "{out}"
    );
}

const CODE: &str = "Run this:\n\n```sh\n  if [ 1 < 2 ] && true; then\n\n    echo \"b & c\"\nfi\n```\nThen check `x`.\n\n```\nunclosed <tail>\n  more";

fn coded(name: &str) -> drover::Ctx {
    let c = setup(name);
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Code"]).1,
        "t1\n"
    );
    let (code, out) = run(&c, &Fake::default(), &["thread", "reply", "t1", CODE]);
    assert_eq!(code, 0, "{out}");
    c
}

#[test]
fn a_fenced_block_renders_as_one_box_with_its_exact_text_and_copy() {
    let (c, r) = (coded("chat-code"), Fake::default());
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let cookie = pair(&s, &c, "Mozilla/5.0 (iPhone)", T);
    let (chat, _) = frags(&s, &cookie, T);
    let md = CODE
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;");
    let want = "<p>Run this:</p>\
        <div class=cb><pre>  if [ 1 &lt; 2 ] &amp;&amp; true; then\n\n    echo &quot;b &amp; c&quot;\nfi</pre><button type=button class=cpy>copy</button></div>\
        <p>Then check <code>x</code>.</p>\
        <div class=cb><pre>unclosed &lt;tail&gt;\n  more</pre><button type=button class=cpy>copy</button></div></div>";
    assert!(
        chat.contains(&format!("<div class=md data-md=\"{md}\">{want}")),
        "{chat}"
    );
}

#[test]
fn copy_puts_the_exact_text_on_the_clipboard_flashes_copied_and_selects_without_one() {
    if no_webkit() {
        return;
    }
    let c = coded("chat-copy");
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let (c2, at) = (c.clone(), addr.clone());
    std::thread::spawn(move || {
        let r = Fake::default();
        let s = Server::new(&c2, &r, Some(at));
        for mut st in l.incoming().flatten() {
            let mut buf = [0u8; 8192];
            let n = std::io::Read::read(&mut st, &mut buf).unwrap_or(0);
            let _ = std::io::Write::write_all(&mut st, &s.respond(&String::from_utf8_lossy(&buf[..n]), T));
        }
    });
    let pre = "window.copied = []; Object.defineProperty(navigator, 'clipboard', { configurable: true, \
        value: { writeText: t => (copied.push(t), Promise.resolve()) } })";
    let (first, second) = (
        "  if [ 1 < 2 ] && true; then\n\n    echo \"b & c\"\nfi",
        "unclosed <tail>\n  more",
    );
    let js = format!(
        "(async () => {{ try {{ const q = () => [...$$('#chat .cvp[data-t=\"t1\"] .cpy')], timers = [], st = window.setTimeout; \
          while (!q().length) await new Promise(r => st(r, 50)); \
          const b = q(); \
          window.setTimeout = (f, ms) => (ms === 1500 && timers.push(f), ms === 1500 ? 0 : st(f, ms)); \
          b[0].click(); await new Promise(r => st(r, 50)); \
          const flash = b[0].textContent; timers.forEach(f => f()); \
          Object.defineProperty(navigator, 'clipboard', {{ configurable: true, value: undefined }}); \
          b[1].click(); \
          window.late = [b.length, copied.length, +(copied[0] === {}), +(flash === 'copied'), +(b[0].textContent === 'copy'), \
            timers.length, +(getSelection().toString() === {}), errs.length]; }} catch (e) {{ window.late = [-1]; }} }})()",
        serde_json::json!(first),
        serde_json::json!(second)
    );
    let o = webview()
        .env("DROVER_WAIT", "3")
        .env("DROVER_PRE", pre)
        .args([
            &format!("http://{addr}/"),
            "390",
            "chat",
            "scene",
            tmpdir("chat-copy-png").join("t.png").to_str().unwrap(),
            &format!("chatOpen('t1'); {{ {js} }}"),
        ])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out} {}", String::from_utf8_lossy(&o.stderr));
    let m: Vec<f64> = out.split_whitespace().skip(2).filter_map(|n| n.parse().ok()).collect();
    assert_eq!(m, [2., 1., 1., 1., 1., 1., 1., 0.], "{out}");
}

fn log(c: &drover::Ctx) -> String {
    drover::events::jsonl(&c.state).unwrap_or_default()
}

#[test]
fn a_first_message_starts_the_thread_titled_by_its_first_line_and_thread_title_retitles_it() {
    let b = FakeShed::new();
    let c = acp_setup("chat-first", &b);
    let f = acp();
    let cli = b.cli(&f);
    let r = b.on(&cli);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let cookie = pair(&s, &c, "Mozilla/5.0 (iPhone)", T);
    let (chat, _) = frags(&s, &cookie, T);
    assert!(
        chat.contains("<div class=cvp data-t=\"new-sendmeter\">")
            && chat.contains("<form class=tw data-t=\"new-sendmeter\" data-repo=\"sendmeter\">")
            && chat.contains("<button type=button class=ntb data-repo=\"sendmeter\">+ thread</button>"),
        "{chat}"
    );
    assert!(!chat.contains("name=title"), "no title form");
    assert!(
        !log(&c).contains("thread-"),
        "an empty thread is never logged: {}",
        log(&c)
    );
    let res = req(
        &s,
        "POST",
        "/thread",
        "op=new&repo=sendmeter&text=Grill+me+on+%2312%0Athe+land+mode+part",
        &cookie,
        T,
    );
    assert_eq!(body(&res), "t1 started\n", "{res}");
    let events = drover::events::read(&c.state).unwrap();
    let new = events.iter().find(|e| e.kind == "thread-new").unwrap();
    assert_eq!(new.str("title"), Some("Grill me on #12"));
    let msgs: Vec<_> = events.iter().filter(|e| e.kind == "thread-msg").collect();
    assert_eq!(msgs.len(), 1, "{}", log(&c));
    assert_eq!(msgs[0].str("text"), Some("Grill me on #12\nthe land mode part"));
    let thread = c.state.with_file_name("orchestrator").join("t1/.thread.md");
    assert_eq!(
        std::fs::read_to_string(thread).unwrap(),
        "owner: Grill me on #12\nthe land mode part\n",
        "the conductor gets the message only"
    );
    assert!(f.called(START_T1), "{:?}", f.calls.borrow());
    let (code, out) = run(&c, &r, &["thread", "title", "t1", "Land mode for #12"]);
    assert_eq!(code, 0, "{out}");
    let t = drover::events::read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .rfind(|e| e.kind == "thread-title")
        .unwrap();
    assert_eq!(
        (t.str("thread"), t.str("title")),
        (Some("t1"), Some("Land mode for #12"))
    );
    assert!(run(&c, &r, &["thread", "show", "t1"])
        .1
        .starts_with("t1 sendmeter: Land mode for #12\n"));
    let (chat, _) = frags(&s, &cookie, T + 5);
    assert!(
        chat.contains("<span class=tt><span>Land mode for #12</span><small class=tid>· t1</small></span>"),
        "{chat}"
    );
    assert!(
        chat.contains("<div class=tn><b>Land mode for #12</b><small class=tid>· t1</small></div>"),
        "{chat}"
    );
    for bad in ["two\nlines", " "] {
        assert_ne!(run(&c, &r, &["thread", "title", "t1", bad]).0, 0, "{bad:?}");
    }
    let long = "word ".repeat(30);
    let res = req(
        &s,
        "POST",
        "/thread",
        &format!("op=new&repo=sendmeter&text={}", long.replace(' ', "+")),
        &cookie,
        T + 9,
    );
    assert_eq!(body(&res), "t2 started\n", "{res}");
    let t2 = drover::events::read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .rfind(|e| e.kind == "thread-new")
        .unwrap();
    assert_eq!(t2.str("title").unwrap(), format!("{}…", &long[..60]));
}

#[test]
fn archive_stops_the_conductor_and_folds_the_thread_and_restore_resumes_it() {
    let b = FakeShed::new();
    let c = acp_setup("chat-archive", &b);
    let f = acp();
    let cli = b.cli(&f);
    let r = b.on(&cli);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let cookie = pair(&s, &c, "Mozilla/5.0 (iPhone)", T);
    let png = url_enc(&B64.encode(b"\x89PNG one"));
    let res = req(
        &s,
        "POST",
        "/thread",
        &format!("op=new&repo=sendmeter&text=hi&name=a.png&data={png}"),
        &cookie,
        T,
    );
    assert_eq!(body(&res), "t1 started\n", "{res}");
    let dir = c.state.with_file_name("orchestrator").join("t1");
    let files = |d: &std::path::Path| {
        let mut v = vec![];
        let mut todo = vec![d.to_path_buf()];
        while let Some(p) = todo.pop() {
            for e in std::fs::read_dir(&p).unwrap().flatten() {
                if e.path().is_dir() {
                    todo.push(e.path());
                } else {
                    v.push(e.path());
                }
            }
        }
        v.sort();
        v
    };
    let before = files(&dir);
    assert!(
        before.iter().any(|f| f.ends_with(format!("uploads/{T}-a.png"))),
        "{before:?}"
    );
    let starts = |c: &drover::Ctx| {
        let s: Vec<_> = drover::events::read(&c.state)
            .unwrap()
            .to_vec()
            .into_iter()
            .filter(|e| e.kind == "orchestrator-start")
            .collect();
        s.iter()
            .map(|e| (e.str("runtime").unwrap().to_string(), e.val("resumed") == true))
            .collect::<Vec<_>>()
    };
    let res = req(&s, "POST", "/thread", "op=archive&t=t1", &cookie, T + 10);
    assert_eq!(body(&res), "t1 archived\n", "{res}");
    assert!(
        b.called("kill", "orchestrator-t1") && !b.has("orchestrator-t1"),
        "{:?}",
        b.methods()
    );
    let sleep = drover::events::read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .rfind(|e| e.kind == "orchestrator-sleep")
        .unwrap();
    assert_eq!(sleep.str("why"), Some("archived"));
    let (chat, _) = frags(&s, &cookie, T + 11);
    let fold = &chat[chat
        .find("<details class=\"done arc\"><summary><span>1 archived</span>")
        .unwrap_or_else(|| panic!("{chat}"))..];
    assert!(fold.starts_with("<details class=\"done arc\"><summary><span>1 archived</span><i class=cv></i></summary><div class=sw><a class=\"tr"), "{fold}");
    assert!(
        fold.contains("<button type=button class=sb name=restore data-t=\"t1\">restore</button></div></details></div>"),
        "{fold}"
    );
    assert!(
        chat.contains("<b>sendmeter</b><span class=ds>no threads</span>"),
        "{chat}"
    );
    let res = req(&s, "POST", "/thread", "op=restore&t=t1", &cookie, T + 20);
    assert_eq!(body(&res), "t1 restored\n", "{res}");
    let (chat, _) = frags(&s, &cookie, T + 21);
    assert!(!chat.contains("class=\"done arc\""), "{chat}");
    assert!(
        chat.contains("<button type=button class=sb name=archive data-t=\"t1\">archive</button>"),
        "{chat}"
    );
    let res = req(&s, "POST", "/thread", "op=say&t=t1&text=back+again", &cookie, T + 30);
    assert_eq!(body(&res), "sent\n", "{res}");
    let st = starts(&c);
    assert_eq!(st.len(), 2, "{st:?}");
    assert_eq!(st[1], ("shed-acp".to_string(), false), "{st:?}");
    assert_eq!(f.count(START_T1), 2, "{:?}", f.calls.borrow());
    assert_eq!(run(&c, &cli, &["thread", "archive", "t1"]).0, 0);
    assert_eq!(b.all("kill").len(), 2, "{:?}", b.methods());
    assert_eq!(run(&c, &cli, &["thread", "restore", "t1"]).0, 0);
    let l = log(&c);
    assert_eq!(
        (
            l.matches("\"thread-archive\"").count(),
            l.matches("\"thread-restore\"").count()
        ),
        (2, 2),
        "{l}"
    );
    let after = files(&dir);
    assert!(before.iter().all(|f| after.contains(f)), "{before:?} vs {after:?}");
}

#[test]
fn a_restored_shed_thread_resumes_its_orchestrators_session() {
    let c = setup("chat-archive-shed");
    let b = FakeShed::attached(&c);
    let r = Fake::default();
    assert_eq!(run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Look"]).0, 0);
    assert_eq!(run(&c, &r, &["thread", "say", "t1", "hi"]).0, 0);
    assert!(b.called("run", "orchestrator-t1"), "{:?}", b.methods());
    assert_eq!(run(&c, &r, &["thread", "archive", "t1"]).0, 0);
    assert!(
        b.called("kill", "orchestrator-t1") && !b.has("orchestrator-t1"),
        "{:?}",
        b.methods()
    );
    assert_eq!(run(&c, &r, &["thread", "restore", "t1"]).0, 0);
    assert_eq!(run(&c, &r, &["thread", "say", "t1", "back again"]).0, 0);
    let st: Vec<_> = drover::events::read(&c.state)
        .unwrap()
        .iter()
        .filter(|e| e.kind == "orchestrator-start")
        .map(|e| (e.str("session").unwrap().to_string(), e.val("resumed") == true))
        .collect();
    assert_eq!(st.len(), 2, "{st:?}");
    assert!(st[1].1 && st[1].0 == st[0].0, "the same session resumed: {st:?}");
}

#[test]
fn a_message_with_three_images_delivers_all_three_and_eleven_are_refused() {
    let b = FakeShed::new();
    let c = acp_setup("chat-multi", &b);
    let f = acp();
    let cli = b.cli(&f);
    let r = b.on(&cli);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let cookie = pair(&s, &c, "Mozilla/5.0 (iPhone)", T);
    let many = |n: usize| {
        let names: Vec<String> = (0..n).map(|i| format!("IMG_{i}.PNG")).collect();
        let data: Vec<String> = (0..n).map(|i| url_enc(&B64.encode(format!("png {i}")))).collect();
        format!("name={}&data={}", names.join("%2F"), data.join("%2C"))
    };
    let res = req(
        &s,
        "POST",
        "/thread",
        &format!("op=new&repo=sendmeter&text=x&{}", many(11)),
        &cookie,
        T,
    );
    assert!(body(&res).contains("at most 10 files a message"), "{res}");
    assert!(!log(&c).contains("thread-new"), "no thread: {}", log(&c));
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Look"]).1,
        "t1\n"
    );
    assert_eq!(
        run(&c, &cli, &["thread", "say", "t1", "hi"]).0,
        0,
        "its conductor is up"
    );
    let res = req(
        &s,
        "POST",
        "/thread",
        &format!("op=say&t=t1&text=x&{}", many(11)),
        &cookie,
        T,
    );
    assert!(body(&res).contains("at most 10 files a message"), "{res}");
    let up = c.state.with_file_name("orchestrator").join("t1/uploads");
    assert_eq!(std::fs::read_dir(&up).map_or(0, |d| d.count()), 0, "nothing saved");
    let res = req(
        &s,
        "POST",
        "/thread",
        &format!("op=say&t=t1&text=&{}", many(3)),
        &cookie,
        T + 1,
    );
    assert_eq!(body(&res), "sent\n", "{res}");
    let saved: Vec<_> = (0..3).map(|i| up.join(format!("{}-IMG_{i}.PNG", T + 1))).collect();
    for (i, p) in saved.iter().enumerate() {
        assert_eq!(std::fs::read(p).unwrap(), format!("png {i}").as_bytes());
    }
    let att: String = saved.iter().map(|p| format!(" (attached: {})", p.display())).collect();
    assert!(
        f.called(&format!(
            "shed send orchestrator-t1 [t1] owner: IMG_0.PNG, IMG_1.PNG, IMG_2.PNG{att}"
        )),
        "{:?}",
        f.calls.borrow()
    );
    let (chat, _) = frags(&s, &cookie, T + 2);
    for i in 0..3 {
        assert!(
            chat.contains(&format!("<img src=\"thread/t1/IMG%5F{i}%2EPNG\"")),
            "{chat}"
        );
    }
    let a = c.state.with_file_name("a.png");
    std::fs::write(&a, b"A").unwrap();
    let a = a.to_str().unwrap();
    let (code, out) = run(&c, &cli, &["thread", "say", "t1", "two", "--file", a, "--file", a]);
    assert_eq!(code, 0, "{out}");
    assert!(
        f.calls
            .borrow()
            .iter()
            .any(|l| l.starts_with("shed send orchestrator-t1 ")
                && l.contains("owner: two (attached: ")
                && l.contains("-a.png) (attached: ")
                && l.contains("-2-a.png)")),
        "{:?}",
        f.calls.borrow()
    );
}

#[test]
fn the_keyboard_keeps_the_composer_on_it_the_page_still_and_the_thread_in_place() {
    if no_webkit() {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let b = leaked();
    let (c, cookie) = fill(acp_setup("chat-kb", b), 30, now);
    b.add(
        "orchestrator-t1",
        serde_json::json!({"fleet": "sendmeter", "role": "orchestrator", "thread": "t1"}),
    );
    working(b, false);
    let st = serde_json::json!({"thread": "t1", "agent": "orchestrator-t1", "pane": "orchestrator-t1", "runtime": "shed-acp", "harness": "claude"});
    let e = drover::events::Event::new(T, "sendmeter", None, "orchestrator-start", st);
    drover::events::append(&c.state, &e).unwrap();
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        let f = acp();
        drover::serve::serve_on(&c, &b.cli(&f), l, false)
    });
    let js = "(async () => { try { const st = window.setTimeout, v = () => $('#chat .cvp[data-t=\"t1\"]'); \
          while (!(v() && v().querySelectorAll('.tc .blk').length > 20)) await new Promise(r => st(r, 50)); \
          chatOpen('t1'); \
          const frame = () => new Promise(r => { requestAnimationFrame(() => requestAnimationFrame(r)); st(r, 300); }); await frame(); \
          const tc = v().querySelector('.tc'), tw = v().querySelector('.tw'), box = tw.elements.text, vv = visualViewport; \
          const above = () => { const y = tw.getBoundingClientRect().top; const b = [...tc.querySelectorAll('.blk')].filter(b => b.getBoundingClientRect().top < y - 1).pop(); return [b, b.getBoundingClientRect().top - y]; }; \
          const H = innerHeight, kb = (h, ev) => { for (const [o, k] of [[vv, 'height'], [window, 'innerHeight']]) Object.defineProperty(o, k, { configurable: true, get: () => H - h }); if (ev) vv.dispatchEvent(new Event('resize')); }; \
          const [b0, d0] = above(), at = [tc.scrollTop > 0 ? 1 : 0]; \
          const look = async (y = () => vv.offsetTop + vv.height) => { await frame(); \
            const m = () => { const [b, d] = above(); return [Math.abs(tw.getBoundingClientRect().bottom - y()), document.scrollingElement.scrollTop, +(b === b0), Math.abs(d - d0)]; }; \
            const ok = r => r[0] <= 1 && !r[1] && r[2] && r[3] <= 1, paint = () => new Promise(q => requestAnimationFrame(() => requestAnimationFrame(q))); \
            let r = m(); for (let i = 0; i < 40; i++) { const was = ok(r) && String(r); await (was ? paint() : new Promise(q => st(q, 50))); r = m(); if (was === String(r)) break; } at.push(...r); }; \
          box.focus(); await new Promise(r => st(r, 100)); kb(336); await look(); \
          box.value = 'one\\ntwo\\nthree'; box.dispatchEvent(new Event('input', { bubbles: true })); await look(); \
          box.value = ''; box.dispatchEvent(new Event('input', { bubbles: true })); box.blur(); await new Promise(r => st(r, 100)); kb(0); await look(() => $('nav').getBoundingClientRect().top); \
          box.focus(); kb(336, true); box.value = 'grill me on the land mode'; box.dispatchEvent(new Event('input', { bubbles: true })); await look(); \
          at.push(errs.length); window.late = at; } catch (e) { window.late = [-1]; } })()";
    let png = std::env::var("DROVER_SNAPSHOTS").map_or(tmpdir("chat-kb-png").join("chat-kb.png"), |p| {
        std::path::Path::new(&p).join("chat-kb.png")
    });
    let o = webview()
        .env("DROVER_WAIT", "12")
        .env("DROVER_COOKIE", cookie.trim().trim_start_matches("Cookie: "))
        .args([
            &format!("http://{addr}/"),
            "390",
            "chat",
            "scene",
            png.to_str().unwrap(),
            js,
        ])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out} {}", String::from_utf8_lossy(&o.stderr));
    let m: Vec<f64> = out.split_whitespace().skip(2).filter_map(|n| n.parse().ok()).collect();
    assert_eq!(m.len(), 18, "{out}");
    assert_eq!(m[0], 1.0, "the thread starts scrolled to its end: {out}");
    for (i, s) in m[1..17].chunks(4).enumerate() {
        let what = ["keyboard up", "box grown", "keyboard down", "keyboard up again"][i];
        assert!(
            s[0] <= 1.0,
            "{what}: the composer is {} px off the keyboard or tab bar ({out})",
            s[0]
        );
        assert_eq!(s[1], 0.0, "{what}: the page scrolled ({out})");
        assert_eq!(s[2], 1.0, "{what}: another message above the box ({out})");
        assert!(
            s[3] <= 1.0,
            "{what}: the message above the box moved {} px ({out})",
            s[3]
        );
    }
    assert_eq!(m[17], 0.0, "page errors: {out}");
}

fn working(b: &FakeShed, on: bool) {
    let st = ["idle", "working"][usize::from(on)];
    b.set("orchestrator-t1", |t| t["reported"] = serde_json::json!({"state": st}));
}

struct Typing<'a>(
    common::fake_shed::Cli<'a>,
    &'a FakeShed,
    std::sync::Mutex<Vec<(String, usize)>>,
);
impl Typing<'_> {
    fn esc(&self) -> usize {
        let keys = self.1.all("keys");
        keys.iter()
            .filter(|k| k["name"] == "orchestrator-t1" && k["keys"] == serde_json::json!(["esc"]))
            .count()
    }
}
impl drover::runner::Runner for Typing<'_> {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<drover::runner::Out> {
        if prog == "shed" && args[0] == "send" {
            let line = format!("{prog} {}", args.join(" "));
            self.2.lock().unwrap().push((line, self.esc()));
        }
        self.0.run(prog, args)
    }
}

fn post_to(addr: &std::net::SocketAddr, cookie: &str, body: &str) -> String {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    let head = format!(
        "POST /thread HTTP/1.1\r\nHost: {addr}\r\n{cookie}Content-Length: {}\r\n\r\n",
        body.len()
    );
    s.write_all((head + body).as_bytes()).unwrap();
    let mut res = String::new();
    s.read_to_string(&mut res).unwrap();
    body_of(&res)
}
fn body_of(res: &str) -> String {
    res[res.find("\r\n\r\n").unwrap() + 4..].into()
}

fn queued_ids(c: &drover::Ctx) -> Vec<(u64, String)> {
    let ev = drover::events::read(&c.state).unwrap();
    let q = ev
        .iter()
        .filter(|e| e.kind == "thread-queue" && e.data.get("q").is_none());
    q.map(|e| (e.seq, e.str("text").unwrap_or_default().to_string()))
        .collect()
}

fn until(f: impl Fn() -> bool) -> bool {
    (0..100).any(|_| f() || (std::thread::sleep(std::time::Duration::from_millis(100)), false).1)
}

#[test]
fn a_queued_message_waits_for_the_turn_to_end_and_steer_interrupts_and_sends_now() {
    let b = leaked();
    let c = acp_setup("chat-queue", b);
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Look"]).1,
        "t1\n"
    );
    assert_eq!(
        run(&c, &b.cli(&acp()), &["thread", "say", "t1", "hi"]).0,
        0,
        "its conductor is up"
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let cookie = pair(
        &Server::new(&c, &Fake::default(), Some(ADDR.into())),
        &c,
        "Mozilla/5.0 (iPhone)",
        now,
    );
    working(b, true);
    let f: &'static Fake = Box::leak(Box::new(acp()));
    let r: &'static Typing = Box::leak(Box::new(Typing(b.cli(f), b, Default::default())));
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let c2 = c.clone();
    std::thread::spawn(move || drover::serve::serve_on(&c2, r, l, false));
    let typed = |t: &str| f.called(&format!("shed send orchestrator-t1 [t1] owner: {t}"));
    for t in ["one", "two", "three"] {
        assert_eq!(post_to(&addr, &cookie, &format!("op=queue&t=t1&text={t}")), "queued\n");
    }
    let ids = queued_ids(&c);
    assert_eq!(
        ids.iter().map(|q| q.1.as_str()).collect::<Vec<_>>(),
        ["one", "two", "three"]
    );
    let edit = format!("op=queue&t=t1&q={}&text=one+edited", ids[0].0);
    assert_eq!(post_to(&addr, &cookie, &edit), "queued\n");
    assert_eq!(
        post_to(&addr, &cookie, &format!("op=unqueue&t=t1&q={}", ids[2].0)),
        "removed\n"
    );
    assert_eq!(
        post_to(&addr, &cookie, "op=unqueue&t=t1&q=1"),
        "error: no such queued message\n"
    );
    let on = b.on(r);
    let (chat, _) = frags(&Server::new(&c, &on, Some(ADDR.into())), &cookie, now);
    let row = |q: u64, t: &str| {
        format!("<div class=tq data-q={q}><p title=\"{t}\">{t}</p><span class=btns><button type=button name=qn>send now</button><button type=button name=qe>edit</button><button type=button name=qx>remove</button></span></div>")
    };
    let rows = row(ids[0].0, "one edited") + &row(ids[1].0, "two");
    assert!(
        chat.contains(&format!("<form class=tw data-t=\"t1\">{rows}<input type=file")),
        "{chat}"
    );
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(!typed("one") && !typed("two"), "typed mid-turn: {:?}", f.calls.borrow());
    working(b, false);
    assert!(until(|| typed("two")), "{:?}", f.calls.borrow());
    let calls = f.calls.borrow().clone();
    let at = |t: &str| calls.iter().position(|c| c.ends_with(&format!("owner: {t}")));
    assert!(
        at("one edited") < at("two") && at("three").is_none() && at("one").is_none(),
        "{calls:?}"
    );
    let l = log(&c);
    assert!(
        l.contains("\"from\":\"owner\",\"text\":\"one edited\"") && l.contains("\"from\":\"owner\",\"text\":\"two\"")
    );
    assert_eq!(l.matches("\"why\":\"sent\"").count(), 2, "{l}");
    let (chat, _) = frags(&Server::new(&c, &on, Some(ADDR.into())), &cookie, now);
    assert!(!chat.contains("class=tq"), "the queue is empty");
    working(b, true);
    assert_eq!(r.esc(), 0);
    assert_eq!(post_to(&addr, &cookie, "op=steer&t=t1&text=stop+that"), "sent\n");
    assert!(until(|| typed("stop that")), "{:?}", f.calls.borrow());
    let seen = r.2.lock().unwrap().clone();
    let esc = seen.iter().find(|(l, _)| l.ends_with("owner: stop that")).map(|s| s.1);
    assert_eq!(esc, Some(1), "Esc before the message: {seen:?}");
    assert_eq!(post_to(&addr, &cookie, "op=queue&t=t1&text=four"), "queued\n");
    let four = queued_ids(&c).last().unwrap().0;
    assert_eq!(post_to(&addr, &cookie, &format!("op=steer&t=t1&q={four}")), "sent\n");
    assert!(until(|| typed("four")), "{:?}", f.calls.borrow());
    assert_eq!(r.esc(), 2);
    assert_eq!(log(&c).matches("\"why\":\"sent\"").count(), 3);
    assert_eq!(post_to(&addr, &cookie, "op=stop&t=t1"), "stopping\n");
    assert_eq!(r.esc(), 3, "[stop] is Esc alone");
}

#[test]
fn a_finished_turns_commands_fold_and_the_live_turns_stay_open() {
    let b = FakeShed::new();
    let c = acp_setup("chat-fold", &b);
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Look"]).1,
        "t1\n"
    );
    let cmd = |n: &str| {
        let d = serde_json::json!({"thread": "t1", "cmd": format!("drover brief sendmeter {n}")});
        drover::events::append(
            &c.state,
            &drover::events::Event::new(T, "sendmeter", None, "thread-cmd", d),
        )
        .unwrap();
    };
    let f = acp();
    let cli = b.cli(&f);
    assert_eq!(run(&c, &cli, &["thread", "say", "t1", "go"]).0, 0);
    cmd("1");
    cmd("2");
    assert_eq!(run(&c, &cli, &["thread", "reply", "t1", "done"]).0, 0);
    assert_eq!(run(&c, &cli, &["thread", "say", "t1", "again"]).0, 0);
    assert!(
        f.called("shed send orchestrator-t1 [t1] owner: again"),
        "{:?}",
        f.calls.borrow()
    );
    cmd("3");
    let row = |n: &str| {
        format!("<details class=\"blk run\"><summary><span class=k>$</span><span class=cmd>drover brief sendmeter {n}</span><i class=cv></i></summary></details>")
    };
    let ev = drover::events::read(&c.state).unwrap();
    let seq = |n: &str| {
        ev.iter()
            .find(|e| e.str("cmd").is_some_and(|x| x.ends_with(n)))
            .unwrap()
            .seq
    };
    let since = ev.iter().rfind(|e| e.str("text") == Some("again")).unwrap().ts;
    let chat = |st: &str| {
        b.set("orchestrator-t1", |t| t["reported"] = serde_json::json!({"state": st}));
        let f = Fake::default();
        let r = b.on(&f);
        let s = Server::new(&c, &r, Some(ADDR.into()));
        frags(&s, &pair(&s, &c, "Mozilla/5.0 (iPhone)", T), T).0
    };
    let fold = |f: u64, n: &str, rows: &str| {
        format!("<details data-n={f} class=\"blk fold\"><summary><span class=pv>{n}</span></summary>{rows}</details>")
    };
    let w = chat("working");
    assert!(
        w.contains(&fold(seq("1"), "2 commands", &(row("1") + &row("2")))),
        "{w}"
    );
    assert!(
        w.contains(&row("3").replacen("<details", &format!("<details data-n={}", seq("3")), 1))
            && !w.contains(&fold(seq("3"), "1 command", &row("3"))),
        "the live turn's row stays: {w}"
    );
    assert!(w.contains(&format!("<p class=\"tlv wk\" data-since={since}>")), "{w}");
    let i = chat("idle");
    assert!(
        i.contains(&fold(seq("3"), "1 command", &row("3"))) && !i.contains("tlv wk"),
        "{i}"
    );
}

#[test]
fn a_stream_with_a_thread_open_stamps_its_watch_file() {
    use std::io::{Read, Write};
    let c = setup("chat-watch");
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Look"]).1,
        "t1\n"
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let cookie = pair(
        &Server::new(&c, &Fake::default(), Some(ADDR.into())),
        &c,
        "Mozilla/5.0 (iPhone)",
        now,
    );
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let c2 = c.clone();
    let b = leaked();
    b.attach(&c);
    std::thread::spawn(move || drover::serve::serve_on(&c2, &Fake::default(), l, false));
    let open = |q: &str| {
        let mut s = std::net::TcpStream::connect(addr).unwrap();
        write!(s, "GET /live?tab=chat{q} HTTP/1.1\r\nHost: {addr}\r\n{cookie}\r\n").unwrap();
        let mut b = [0u8; 64];
        let _ = s.read(&mut b).unwrap();
        s
    };
    let (w, x) = (
        c.state.with_file_name("watch-t1.at"),
        c.state.with_file_name("watch-t2.at"),
    );
    let _plain = open("");
    let s = open("&thread=t1");
    let at = || {
        std::fs::read_to_string(&w)
            .ok()
            .and_then(|t| t.trim().parse::<u64>().ok())
            .unwrap_or(0)
    };
    assert!(until(|| at() > 0), "no stamp");
    let first = at();
    assert!(first >= now, "{first} < {now}");
    assert!(until(|| at() > first), "not stamped again");
    let _bad = open("&thread=../t2");
    drop(s);
    std::thread::sleep(std::time::Duration::from_secs(3));
    let last = at();
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert_eq!(at(), last, "a closed stream stamps no more");
    assert!(
        !x.exists()
            && std::fs::read_dir(c.state.parent().unwrap())
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("watch-"))
                .count()
                == 1
    );
}

#[test]
fn the_thread_follows_its_end_until_you_drag_and_drafts_survive_a_reload() {
    if no_webkit() {
        return;
    }
    let c = coded("chat-follow");
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let (c2, at) = (c.clone(), addr.clone());
    std::thread::spawn(move || {
        let r = Fake::default();
        let s = Server::new(&c2, &r, Some(at));
        for mut st in l.incoming().flatten() {
            let mut buf = [0u8; 8192];
            let n = std::io::Read::read(&mut st, &mut buf).unwrap_or(0);
            let _ = std::io::Write::write_all(&mut st, &s.respond(&String::from_utf8_lossy(&buf[..n]), T));
        }
    });
    let pre = "(() => { window.urls = []; const E = window.EventSource; window.EventSource = function (u) { urls.push(String(u)); return new E(u); }; \
        const F = window.fetch; window.fetch = (u, o) => o && o.method === 'POST' ? Promise.resolve(new Response('error: down', { status: 409 })) : F(u, o); })()";
    let js = "(async () => { try { \
          const nap = ms => new Promise(r => setTimeout(r, ms)), at = window.late = []; \
          for (let k = 0; k < 60 && !$('#chat .cvp[data-t=\"t1\"] .tc'); k++) await nap(50); \
          at.push(+(THR === 't1'), +(document.documentElement.dataset.tab === 'chat'), +urls.some(u => u.includes('thread=t1'))); \
          const n = urls.length; Object.defineProperty(document, 'hidden', { configurable: true, get: () => true }); document.dispatchEvent(new Event('visibilitychange')); \
          if (!es) connect(); /* the stream was between retries (this server's /live ends at once): its retry */ \
          at.push(+urls.slice(n).some(u => !u.includes('thread='))); \
          Object.defineProperty(document, 'hidden', { configurable: true, get: () => false }); \
          const tev = k => Object.defineProperty(new Event(k), 'touches', { value: [{ clientX: 200, clientY: 400 }] }); \
          const sc = () => $('#chat .cvp[data-t=\"t1\"] .tc'), end = () => +(Math.abs(sc().scrollTop + sc().clientHeight - sc().scrollHeight) <= 2); \
          const more = () => { const k = $('#chat').cloneNode(true); for (let i = 0; i < 8; i++) k.querySelector('.cvp[data-t=\"t1\"] .tc').append(Object.assign(document.createElement('p'), { className: 'qs', textContent: 'a streamed line ' + i })); frag('chat', k.innerHTML); }; \
          more(); more(); await nap(100); at.push(end()); \
          sc().scrollTop = 200; await nap(100); more(); await nap(100); at.push(end()); \
          dispatchEvent(tev('touchstart')); sc().scrollTop = 200; await nap(100); dispatchEvent(tev('touchend')); \
          more(); await nap(100); at.push(+(sc().scrollTop === 200), +!FOL); \
          sc().scrollTop = sc().scrollHeight; await nap(100); more(); await nap(100); at.push(end(), +FOL); \
          dispatchEvent(tev('touchstart')); sc().scrollTop = 100; await nap(100); dispatchEvent(tev('touchend')); \
          const f = $('#chat .tw[data-t=\"t1\"]'), box = f.elements.text; \
          box.value = 'a draft'; box.dispatchEvent(new Event('input', { bubbles: true })); \
          const i = f.querySelector('input[type=file]'), d = new DataTransfer(); d.items.add(new File(['x'], 'notes.txt', { type: 'text/plain' })); \
          Object.defineProperty(i, 'files', { configurable: true, value: d.files }); i.dispatchEvent(new Event('change', { bubbles: true })); \
          await nap(300); \
          const fr = document.body.appendChild(Object.assign(document.createElement('iframe'), { src: '/?thread=t1#chat', style: 'width:390px;height:844px' })); \
          let w; for (let k = 0; k < 100 && !(w = fr.contentWindow) .document?.querySelector('#chat .tw[data-t=\"t1\"] .att'); k++) await nap(50); \
          const fb = w.document.querySelector('#chat .tw[data-t=\"t1\"]'); \
          at.push(+(fb?.elements.text.value === 'a draft'), fb ? fb.querySelectorAll('.att').length : -2, +(fb?.querySelector('.att span')?.textContent === 'notes.txt')); \
          f.requestSubmit(); at.push(+(box.value === ''), end(), +FOL); \
          await nap(300); \
          at.push(+(box.value === 'a draft'), +(localStorage.getItem('draft:t1') === 'a draft'), f.querySelectorAll('.att').length); \
          window.late = [...at, errs.length]; } catch (e) { window.late = [-1, window.late.length]; } })()";
    let o = webview()
        .env("DROVER_WAIT", "10")
        .env("DROVER_PRE", pre)
        .args([
            &format!("http://{addr}/?thread=t1#chat"),
            "390",
            "chat",
            "scene",
            tmpdir("chat-follow-png").join("t.png").to_str().unwrap(),
            js,
        ])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{out} {}", String::from_utf8_lossy(&o.stderr));
    let m: Vec<f64> = out.split_whitespace().skip(2).filter_map(|n| n.parse().ok()).collect();
    assert_eq!(
        m,
        [1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 1., 0.],
        "{out}"
    );
}
