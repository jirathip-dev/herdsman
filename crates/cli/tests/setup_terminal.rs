mod common;
use common::*;
use drover::events::read;
use drover::serve::Server;

const ADDR: &str = "127.0.0.1:7777";
const T: u64 = 1_000_000;

fn config() -> String {
    format!("{CONFIG}\n[hosts.mini]\n\n[hosts.bazzite]\nmachine = \"bazzite\"\n\n[[setup]]\nname = \"claude B\"\ncmd = \"claude /login\"\nenv = {{CLAUDE_CONFIG_DIR = \"~/accounts/B\"}}\n")
}

fn log(c: &drover::Ctx) -> String {
    drover::events::jsonl(&c.state).unwrap_or_default()
}

#[test]
fn only_configured_names_run_and_a_remote_terminal_runs_there() {
    let c = ctx(&tmpdir("setup-names"), &config());
    let (here, there, f) = (FakeShed::attached(&c), FakeShed::new(), Fake::default());
    let r = OverSsh {
        f: &f,
        hosts: vec![("bazzite", &there)],
    };
    let (code, out) = run(&c, &r, &["setup", "bazzite", "rm -rf ~"]);
    assert_eq!(code, 3, "{out}");
    assert!(
        out.contains("REFUSED NO_SUCH_SETUP") && there.methods().is_empty(),
        "{out}"
    );
    assert_eq!(
        run(&c, &r, &["setup", "nowhere", "gh"]).0,
        3,
        "an unknown host is refused"
    );
    let (code, out) = run(&c, &r, &["setup", "bazzite", "claude B"]);
    assert_eq!(code, 0, "{out}");
    let name = "drover-setup-claude-B";
    assert!(out.contains(name), "{out}");
    assert_eq!(there.last("run")["cwd"], "/tmp");
    assert_eq!(
        there.sent(name),
        ["cd; CLAUDE_CONFIG_DIR=~/'accounts/B' claude /login; exit\r"]
    );
    assert!(here.methods().is_empty(), "nothing on this host: {:?}", here.methods());
    assert!(run(&c, &r, &["setup", "mini", "gh"]).1.contains("drover-setup-gh"));
    assert_eq!(
        here.sent("drover-setup-gh"),
        ["cd; gh auth login; exit\r"],
        "a built-in, on this host"
    );
    let e = read(&c.state).unwrap();
    assert_eq!(
        (e.len(), e[0].kind.as_str(), e[0].str("machine")),
        (2, "setup", Some("bazzite"))
    );
    assert!(!log(&c).contains("/login"), "the command line is not logged");
}

#[test]
fn the_board_starts_by_name_types_unlogged_and_shows_the_screen() {
    let c = ctx(&tmpdir("setup-web"), &config());
    let screen =
        "Open this link to sign in:\nhttps://claude.ai/oauth?code=abc&state=12345678\n90abcdef\nPaste code here >\n";
    let (here, there, f) = (FakeShed::attached(&c), FakeShed::new(), Fake::default());
    let over = OverSsh {
        f: &f,
        hosts: vec![("bazzite", &there)],
    };
    let r = here.on(&over);
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let post = |path: &str, body: &str, cookie: &str| {
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: {ADDR}\r\n{cookie}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        String::from_utf8_lossy(&s.respond(&req, T)).into_owned()
    };
    assert!(
        post("/setup", "host=bazzite&name=gh", "").starts_with("HTTP/1.1 401"),
        "unpaired"
    );
    let cookie = pair(&s, &c, "", T);
    assert!(post("/setup", "host=bazzite&name=claude+%2Flogin", &cookie).contains("REFUSED NO_SUCH_SETUP"));
    let res = post("/setup", "host=bazzite&name=claude+B", &cookie);
    assert!(
        res.starts_with("HTTP/1.1 200") && res.ends_with("drover-setup-claude-B\n"),
        "{res}"
    );
    there.screen("drover-setup-claude-B", screen);
    let res = post("/send", "setup=drover-setup-claude-B&text=sk-SECRET-CODE-4242", &cookie);
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    assert_eq!(
        there.sent("drover-setup-claude-B").last().unwrap(),
        "sk-SECRET-CODE-4242"
    );
    assert_eq!(
        there.last("keys"),
        serde_json::json!({"name": "drover-setup-claude-B", "keys": ["Enter"]})
    );
    assert!(!log(&c).contains("SECRET"), "typed input is never in the event log");
    let res = String::from_utf8_lossy(&s.respond(
        &format!("GET /lane?setup=drover-setup-claude-B HTTP/1.1\r\nHost: {ADDR}\r\n{cookie}\r\n"),
        T + 5,
    ))
    .into_owned();
    let j: serde_json::Value = serde_json::from_str(&res[res.find("\r\n\r\n").unwrap() + 4..]).unwrap();
    assert_eq!(
        j["urls"][0], "https://claude.ai/oauth?code=abc&state=1234567890abcdef",
        "a wrapped URL is joined"
    );
    assert_eq!(
        (j["closed"].as_bool(), j["lines"][3].as_str()),
        (Some(false), Some("Paste code here >"))
    );
    let page = host_tab(&s, ADDR, T);
    assert!(page.contains("data-su=\"claude B\" data-host=\"bazzite\"") && page.contains("data-su=\"codex\""));
}

#[test]
fn a_setup_closes_on_its_timeout() {
    let c = ctx(&tmpdir("setup-timeout"), &config());
    let (here, there, f) = (FakeShed::attached(&c), FakeShed::new(), Fake::default());
    let over = OverSsh {
        f: &f,
        hosts: vec![("bazzite", &there)],
    };
    let r = here.on(&over);
    assert_eq!(run(&c, &r, &["setup", "bazzite", "gh"]).0, 0);
    let at = |now| drover::Ctx { now, ..c.clone() };
    drover::setup::reap(&at(T + 599), &r, &read(&c.state).unwrap()).unwrap();
    assert!(there.has("drover-setup-gh"));
    drover::setup::reap(&at(T + 600), &r, &read(&c.state).unwrap()).unwrap();
    assert!(!there.has("drover-setup-gh"));
    assert_eq!(there.all("kill").len(), 1);
    let e = read(&c.state).unwrap();
    assert_eq!((e[1].kind.as_str(), e[1].str("why")), ("setup-closed", Some("timeout")));
    drover::setup::reap(&at(T + 900), &r, &e).unwrap();
    assert_eq!(there.all("kill").len(), 1, "closed once");
    assert!(
        drover::setup::send(&at(T + 900), &r, "drover-setup-gh", "x").is_err(),
        "no typing into a closed setup"
    );
}
