mod common;
use common::*;
use drover::events::{append, read, Event};
use drover::serve::Server;
use serde_json::json;

const ADDR: &str = "127.0.0.1:7777";
const T: u64 = 1_000_000;
const CMD: &str = "uptime";

fn fixture(name: &str) -> (drover::Ctx, OnShed) {
    let (c, r) = v1_board(name);
    seed(&c, &r);
    (c, r)
}

fn seed(c: &drover::Ctx, r: &Fake) {
    assert_eq!(run(c, r, &["queue", "sendmeter", "add", "30"]).0, 0);
    let ev = |issue, kind, d| append(&c.state, &Event::new(T - 50, "sendmeter", issue, kind, d)).unwrap();
    let q = json!({"id": "q1", "agent": IMPL, "question": "Which database?", "options": ["SQLite", "Postgres"]});
    ev(Some(12), "needs-decision", q);
    for id in ["r1", "r2"] {
        let d = json!({"thread": "t1", "id": id, "cmd": CMD, "why": "check load", "blast": "read-only",
            "dir": "/srv", "host": "main", "timeout": 60, "sha": drover::devices::sha256(CMD.as_bytes())});
        ev(None, "run-card", d);
    }
}

fn req(s: &Server, method: &str, query: &str, body: &str, cookie: &str) -> (String, String) {
    let r = format!(
        "{method} /api/v1/lifeboat{query} HTTP/1.1\r\nHost: {ADDR}\r\n{cookie}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let res = String::from_utf8_lossy(&s.respond(&r, T)).into_owned();
    let (head, body) = res.split_once("\r\n\r\n").unwrap();
    (head.lines().next().unwrap().to_string(), body.to_string())
}

fn last(c: &drover::Ctx, kind: &str) -> Event {
    read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .rfind(|e| e.kind == kind)
        .unwrap()
}

#[test]
fn an_unpaired_device_sees_the_pair_hint_and_nothing_else() {
    let (c, r) = fixture("lifeboat-unpaired");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let (status, page) = req(&s, "GET", "", "", "");
    assert_eq!(status, "HTTP/1.1 401 Unauthorized");
    assert!(
        page.contains("Pair this device first") && page.contains("action=/pair/request"),
        "{page}"
    );
    for secret in ["sendmeter", IMPL, "Which database", CMD, "r1"] {
        assert!(!page.contains(secret), "{secret} leaked: {page}");
    }
    let (status, _) = req(&s, "POST", "", "op=pause&fleet=sendmeter", "");
    assert_eq!(status, "HTTP/1.1 401 Unauthorized");
    assert!(!read(&c.state).unwrap().iter().any(|e| e.kind == "pause"));
}

#[test]
fn a_paired_device_sees_queue_lanes_asks_and_cards_and_a_load_runs_no_subprocess() {
    let (c, r) = fixture("lifeboat-paired");
    assert_eq!(run(&c, &r, &["drive", "--fast"]).0, 0);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let ck = pair(&s, &c, "Mozilla/5.0 (iPhone)", T);
    let before = r.calls.borrow().len();
    let (status, page) = req(&s, "GET", "", "", &ck);
    assert_eq!(
        r.calls.borrow()[before..],
        [] as [String; 0],
        "a page load ran a subprocess"
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    let sha = &drover::devices::sha256(CMD.as_bytes())[..12];
    for want in [
        "sendmeter · running <form method=post action=lifeboat><input type=hidden name=op value=\"pause\">",
        "sendmeter #30 ",
        IMPL,
        REV,
        "Which database?",
        "<button>1. SQLite</button>",
        "<button>2. Postgres</button>",
        "name=choice placeholder",
        "<pre>uptime</pre>why: check load<br>touches: read-only",
        &format!("<input type=hidden name=sha value=\"{sha}\"><button>approve</button>"),
        "<button>deny</button>",
    ] {
        assert!(page.contains(want), "no {want:?} in {page}");
    }
}

#[test]
fn each_post_logs_the_cli_verbs_event_via_lifeboat() {
    let (c, r) = fixture("lifeboat-posts");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let ck = pair(&s, &c, "Mozilla/5.0 (iPhone)", T);
    let post = |body: &str| {
        let (status, page) = req(&s, "POST", "", body, &ck);
        assert_eq!(status, "HTTP/1.1 200 OK", "{page}");
        assert!(
            page.contains("http-equiv=refresh content=\"0;url=lifeboat?msg="),
            "{page}"
        );
    };
    post("op=pause&fleet=sendmeter");
    let e = last(&c, "pause");
    assert_eq!((e.fleet.as_str(), e.str("via")), ("sendmeter", Some("lifeboat")));
    let page = req(&s, "GET", "?msg=sendmeter%3A%20paused", "", &ck).1;
    assert!(
        page.contains("<p class=b>sendmeter: paused</p>") && page.contains("paused <form"),
        "{page}"
    );
    post("op=resume&fleet=sendmeter");
    assert_eq!(last(&c, "resume").str("via"), Some("lifeboat"));
    post("op=answer&fleet=sendmeter&id=q1&choice=2");
    let d = last(&c, "decision");
    assert_eq!(
        (d.str("id"), d.str("choice"), d.str("by"), d.str("via")),
        (Some("q1"), Some("Postgres"), Some("owner"), Some("lifeboat"))
    );
    let sha = &drover::devices::sha256(CMD.as_bytes())[..12];
    post(&format!("op=approve&id=r1&sha={sha}"));
    let e = last(&c, "run-settled");
    assert_eq!(
        (e.str("id"), e.val("approved").clone(), e.str("via")),
        (Some("r1"), json!(true), Some("lifeboat"))
    );
    assert!(r.called("sh -c"), "approve ran the card's command");
    post("op=deny&id=r2");
    let e = last(&c, "run-settled");
    assert_eq!(
        (e.str("id"), e.val("approved").clone(), e.str("via")),
        (Some("r2"), json!(false), Some("lifeboat"))
    );
    let page = req(&s, "GET", "", "", &ck).1;
    assert!(
        !page.contains("<button>approve</button>") && !page.contains("Which database?"),
        "{page}"
    );
    let (_, back) = req(&s, "POST", "", "op=deny&id=r2", &ck);
    assert!(back.contains("REFUSED%20ALREADY%5FSETTLED"), "{back}");
}

#[test]
fn the_page_for_the_ui_audit() {
    let Some(site) = std::env::var_os("LIFEBOAT_SITE") else {
        return;
    };
    let (c, r) = fixture("lifeboat-ui");
    assert_eq!(run(&c, &r, &["drive", "--fast"]).0, 0);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let ck = pair(&s, &c, "Mozilla/5.0 (iPhone)", T);
    let page = req(
        &s,
        "GET",
        "?msg=sendmeter%20q1%3A%20Postgres%20-%3E%20sendmeter-impl-12",
        "",
        &ck,
    )
    .1;
    let audit = r#"<script>if (location.search.includes("check=1")) addEventListener("load", () => {
  const o = [], c = [...document.querySelectorAll("button,input:not([type=hidden])")];
  if (document.documentElement.scrollWidth > innerWidth) o.push(`sideways scroll: ${document.documentElement.scrollWidth}px`);
  for (const b of c) { const r = b.getBoundingClientRect(); if (r.height < 44) o.push(`${b.outerHTML.slice(0, 60)}: ${r.height}px tall`); }
  const p = document.createElement("pre");
  p.id = "audit";
  p.textContent = JSON.stringify({view: "lifeboat", controls: c.length, outliers: o});
  document.body.append(p);
});</script>"#;
    let dir = std::path::Path::new(&site).join("lifeboat");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("index.html"),
        page.replace("</body>", &format!("{audit}</body>")),
    )
    .unwrap();
}
