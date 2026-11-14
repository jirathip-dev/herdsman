mod common;
use common::*;
use drover::events::{read, Event};
use drover::serve::Server;
use serde_json::{json, Value};

const ADDR: &str = "127.0.0.1:7777";
const T: u64 = 1_000_000;
fn req(s: &Server, method: &str, path: &str, body: &str, extra: &str) -> (String, Value) {
    let r = format!(
        "{method} {path} HTTP/1.1\r\nHost: {ADDR}\r\n{extra}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let res = String::from_utf8_lossy(&s.respond(&r, T)).into_owned();
    let (head, body) = res.split_once("\r\n\r\n").unwrap();
    assert!(head.contains("Content-Type: application/json"), "{res}");
    (
        head.lines().next().unwrap()[9..].to_string(),
        serde_json::from_str(body).unwrap(),
    )
}

fn post(s: &Server, op: &str, v: Value, cookie: &str) -> (u16, String, String) {
    let (status, r) = req(s, "POST", &format!("/api/v1/{op}"), &v.to_string(), cookie);
    assert_eq!(r["ok"], json!(r["code"] == "OK"), "{r}");
    let s = |k: &str| r[k].as_str().unwrap().to_string();
    (status[..3].parse().unwrap(), s("code"), s("message"))
}

fn mtime(s: &Server) -> String {
    req(s, "GET", "/api/v1/state", "", "").1["mtime"]
        .as_str()
        .unwrap()
        .into()
}

fn kinds(c: &drover::Ctx, kind: &str) -> Vec<Event> {
    read(&c.state)
        .unwrap()
        .to_vec()
        .into_iter()
        .filter(|e| e.kind == kind)
        .collect()
}

#[test]
fn send_prompts_a_lane_with_the_boards_refusals_and_rate_limit() {
    let (c, r) = v1_board("v1-send");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let msg = json!({"agent": IMPL, "text": "run the tests again"});
    assert_eq!(post(&s, "send", msg.clone(), "").1, "UNPAIRED");
    assert!(r.1.sent(IMPL).is_empty());
    let ck = pair(&s, &c, "Mozilla/5.0 (iPhone)", T);
    assert_eq!(
        post(&s, "send", msg.clone(), &ck),
        (200, "OK".into(), format!("sent to {IMPL}"))
    );
    assert_eq!(r.1.sent(IMPL), ["run the tests again"]);
    let e = kinds(&c, "prompt").pop().unwrap();
    assert_eq!(
        (e.str("via"), e.str("agent"), e.data.get("text")),
        (Some("web"), Some(IMPL), None)
    );
    let (status, code, _) = post(&s, "send", json!({"agent": IMPL, "text": " "}), &ck);
    assert_eq!((status, code.as_str()), (409, "BAD_VALUE"));
    let (_, code, why) = post(&s, "send", json!({"agent": REV, "text": "hi"}), &ck);
    assert_eq!(code, "REV_READONLY", "{why}");
    let (status, code, _) = post(&s, "send", json!({"agent": "nobody", "text": "hi"}), &ck);
    assert_eq!((status, code.as_str()), (400, "ERROR"));
    for _ in 0..6 {
        post(&s, "send", msg.clone(), &ck);
    }
    assert_eq!(post(&s, "send", msg, &ck).1, "RATE_LIMIT");
}

#[test]
fn menu_answers_the_card_it_was_shown_and_refuses_a_stale_one() {
    let (c, r) = v1_board("v1-menu");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let ck = pair(&s, &c, "Mozilla/5.0 (iPhone)", T);
    let needs = req(&s, "GET", "/api/v1/needs", "", &ck).1;
    let h = needs["blocked"][0]["menu"]["h"].as_str().unwrap().to_string();
    let pick = |choice: &str, h: &str| json!({"agent": REV, "choice": choice, "h": h});
    assert_eq!(post(&s, "menu", pick("1", &h), "").1, "UNPAIRED");
    assert_eq!(post(&s, "menu", pick("1", "123"), &ck).1, "STALE");
    assert_eq!(post(&s, "menu", pick("7", &h), &ck).1, "BAD_VALUE");
    assert!(r.1.all("keys").is_empty(), "never a guessed key");
    let (status, code, _) = post(&s, "menu", pick("1", &h), &ck);
    assert_eq!((status, code.as_str()), (200, "OK"));
    assert_eq!(r.1.all("keys"), [json!({"name": REV, "keys": ["1"]})]);
    let e = kinds(&c, "answer").pop().unwrap();
    assert_eq!(
        (e.issue, e.str("choice"), e.str("via")),
        (Some(9), Some("1"), Some("web"))
    );
}

#[test]
fn act_stops_a_lane_dry_runs_a_kill_and_refuses_unknowns() {
    let (c, r) = v1_board("v1-act");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    assert_eq!(post(&s, "act", json!({"agent": IMPL, "op": "stop"}), "").1, "UNPAIRED");
    let ck = pair(&s, &c, "Mozilla/5.0 (iPhone)", T);
    let (status, code, msg) = post(&s, "act", json!({"agent": IMPL, "op": "stop"}), &ck);
    assert_eq!(
        (status, code.as_str(), msg.as_str()),
        (200, "OK", "sent Esc to sendmeter-impl-12")
    );
    assert_eq!(r.1.all("keys"), [json!({"name": IMPL, "keys": ["esc"]})]);
    assert_eq!(kinds(&c, "act").pop().unwrap().str("op"), Some("stop"));
    assert_eq!(
        post(&s, "act", json!({"agent": "nobody", "op": "stop"}), &ck).1,
        "NO_SUCH_AGENT"
    );
    assert_eq!(post(&s, "act", json!({"agent": IMPL, "op": "rm"}), &ck).1, "BAD_VALUE");
    assert_eq!(kinds(&c, "act").len(), 1);
}

struct Bazzite(OnShed, FakeShed);

impl drover::runner::Runner for Bazzite {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<drover::runner::Out> {
        self.0.run(prog, args)
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> anyhow::Result<drover::runner::Out> {
        if prog == "ssh" && args.join(" ").contains("bazzite -- 'shed' 'connect'") {
            let stdout = self.1.connect(input);
            return Ok(drover::runner::Out {
                stdout,
                ..Default::default()
            });
        }
        self.0.run_in(prog, args, input)
    }
    fn sleep(&self, secs: u64) {
        self.0.sleep(secs);
    }
    fn shed(&self) -> Option<drover::shed::Shed> {
        self.0.shed()
    }
}

#[test]
fn setup_starts_a_configured_terminal_and_reads_its_screen() {
    let (c, r) = v1_board_on("v1-setup", Fake::default());
    let r = Bazzite(r, FakeShed::new());
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let start = json!({"host": "bazzite", "name": "claude B"});
    let pane = "drover-setup-claude-B";
    let screen_of = format!("/api/v1/setup/{pane}");
    assert_eq!(post(&s, "setup", start.clone(), "").1, "UNPAIRED");
    assert_eq!(req(&s, "GET", &screen_of, "", "").1["code"], "UNPAIRED");
    assert!(r.1.methods().is_empty(), "nothing started unpaired");
    let ck = pair(&s, &c, "Mozilla/5.0 (iPhone)", T);
    let (_, code, _) = post(&s, "setup", json!({"host": "bazzite", "name": "rm -rf ~"}), &ck);
    assert_eq!(code, "NO_SUCH_SETUP");
    assert_eq!(post(&s, "setup", start, &ck), (200, "OK".into(), pane.into()));
    assert!(
        r.1.called("run", pane) && r.0 .1.all("run").is_empty(),
        "on bazzite, never here"
    );
    r.1.screen(pane, "Paste code here >\n");
    let (status, screen) = req(&s, "GET", &screen_of, "", &ck);
    assert_eq!(
        (status.as_str(), &screen["lines"][0]),
        ("200 OK", &json!("Paste code here >"))
    );
    let typed = json!({"setup": pane, "text": "sk-SECRET-4242"});
    assert_eq!(post(&s, "send", typed, &ck).1, "OK");
    assert!(
        r.1.sent(pane).iter().any(|t| t == "sk-SECRET-4242"),
        "{:?}",
        r.1.sent(pane)
    );
    assert!(!drover::events::jsonl(&c.state).unwrap().contains("SECRET"));
}

#[test]
fn herd_and_roles_read_as_the_old_board_draws_them() {
    let (c, r) = v1_board("v1-reads");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let (status, herd) = req(&s, "GET", "/api/v1/herd", "", "");
    assert_eq!(status, "200 OK");
    let old = String::from_utf8_lossy(&s.respond(&format!("GET /herd.json HTTP/1.1\r\nHost: {ADDR}\r\n\r\n"), T))
        .into_owned();
    assert_eq!(
        herd,
        serde_json::from_str::<Value>(old.split_once("\r\n\r\n").unwrap().1).unwrap()
    );
    assert_eq!(herd["fleets"][0]["name"], "sendmeter");
    let evil = "Origin: https://evil.example\r\n";
    assert_eq!(req(&s, "GET", "/api/v1/herd", "", evil).1["code"], "FORBIDDEN");
    let (status, roles) = req(&s, "GET", "/api/v1/roles?fleet=sendmeter", "", "");
    assert_eq!(status, "200 OK");
    let names: Vec<_> = roles["config"]["roles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].clone())
        .collect();
    assert_eq!(names, [json!("impl"), json!("orchestrator"), json!("rev")]);
    assert_eq!(roles["config"]["roles"][0]["model"], "opus");
    assert_eq!(
        (
            &roles["chat"]["harness"],
            roles["chat"]["harnesses"].as_array().map(Vec::len).unwrap() > 1
        ),
        (&Value::Null, true)
    );
    let (status, v) = req(&s, "GET", "/api/v1/roles?fleet=nope", "", "");
    assert_eq!((status.as_str(), &v["code"]), ("404 Not Found", &json!("NOT_FOUND")));
}

#[test]
fn set_takes_json_and_chat_writes_the_new_thread_defaults() {
    let (c, r) = v1_board("v1-chat");
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let set = json!({"fleet": "sendmeter", "role": "rev", "model": "sonnet", "mtime": mtime(&s)});
    assert_eq!(post(&s, "set", set, "").1, "OK");
    let roles = |s: &Server| req(s, "GET", "/api/v1/roles?fleet=sendmeter", "", "").1;
    assert_eq!(roles(&s)["config"]["roles"][2]["model"], "sonnet");
    let chat = |m: &str, v: Value| {
        let mut b = json!({"mtime": m});
        b.as_object_mut().unwrap().extend(v.as_object().unwrap().clone());
        b
    };
    let m = mtime(&s);
    assert_eq!(
        post(&s, "chat", chat(&m, json!({"harness": "claude"})), "").1,
        "UNPAIRED"
    );
    let ck = pair(&s, &c, "Mozilla/5.0 (iPhone)", T);
    assert_eq!(
        post(&s, "chat", chat("1", json!({"harness": "claude"})), &ck).1,
        "STALE"
    );
    assert_eq!(
        post(&s, "chat", chat(&m, json!({"model": "gpt-9"})), &ck).1,
        "BAD_VALUE"
    );
    assert_eq!(
        post(&s, "chat", chat(&m, json!({"harness": "emacs"})), &ck).1,
        "BAD_VALUE"
    );
    assert_eq!(post(&s, "chat", chat(&m, json!({})), &ck).1, "BAD_VALUE");
    let toml = std::fs::read_to_string(&c.config).unwrap();
    assert!(!toml.contains("[chat]"), "a refusal writes nothing");
    let (status, code, msg) = post(&s, "chat", chat(&m, json!({"harness": "claude", "model": "opus"})), &ck);
    assert_eq!(
        (status, code.as_str(), msg.as_str()),
        (200, "OK", "chat defaults: harness claude, model opus")
    );
    let after = roles(&s)["chat"].clone();
    assert_eq!((&after["harness"], &after["model"]), (&json!("claude"), &json!("opus")));
    assert!(
        std::fs::read_to_string(&c.config).unwrap().starts_with(&toml),
        "the rest of fleet.toml stays"
    );
    let e = kinds(&c, "set").pop().unwrap();
    assert_eq!(
        (e.str("role"), e.str("key"), e.str("new")),
        (Some("chat"), Some("model"), Some("opus"))
    );
    let m = mtime(&s);
    assert_eq!(post(&s, "chat", chat(&m, json!({"effort": "high"})), &ck).1, "OK");
    let toml = std::fs::read_to_string(&c.config).unwrap();
    assert_eq!(toml.matches("[chat]").count(), 1, "{toml}");
    assert_eq!(roles(&s)["chat"]["effort"], "high");
}

#[test]
fn a_backlog_issue_gets_a_thread_or_closes() {
    let sh = FakeShed::new();
    let cfg = format!(
        "{CONFIG}\n[fleet.sendmeter.role.conductor]\nharness = \"claude\"\naccount = \"C\"\n\n\
         [fleet.sendmeter.runtime]\nkind = \"shed-acp\"\nsocket = \"{}\"\n\n[accounts]\nC = \"C\"\n",
        sh.sock.display()
    );
    let mut c = ctx(&tmpdir("v1-backlog"), &cfg);
    c.exe = env!("CARGO_BIN_EXE_drover").into();
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    let d = json!({"role": "impl", "agent": IMPL, "harness": "claude", "pane": IMPL});
    drover::events::append(&c.state, &Event::new(T - 100, "sendmeter", Some(12), "spawn", d)).unwrap();
    let agents =
        json!([{"id": "claude-acp", "version": "0.101.0", "distribution": "npx", "cmd": ["echo", "claude-agent-acp"]}]);
    let host = Fake::default()
        .reply("shed agents --json", &agents.to_string())
        .reply("shed", "")
        .reply("gh issue close", "");
    let cli = sh.cli(&host);
    let r = drover::shed::With(&cli, sh.shed());
    let cache = c.state.parent().unwrap().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let bl = json!({"ts": T, "issues": {"30": {"number": 30, "title": "Meter export", "state": "OPEN"}}});
    std::fs::write(cache.join("backlog-sendmeter-0.json"), bl.to_string()).unwrap();
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let about = json!({"fleet": "sendmeter", "issue": 30});
    assert_eq!(post(&s, "chat-about", about.clone(), "").1, "UNPAIRED");
    assert_eq!(post(&s, "close", about.clone(), "").1, "UNPAIRED");
    let ck = pair(&s, &c, "Mozilla/5.0 (iPhone)", T);
    let (status, code, msg) = post(&s, "chat-about", about.clone(), &ck);
    assert_eq!((status, code.as_str()), (200, "OK"), "{msg}");
    let t = msg.strip_suffix(" started").unwrap();
    let said = kinds(&c, "thread-msg").pop().unwrap();
    assert_eq!(
        (said.str("thread"), said.str("text")),
        (Some(t), Some("Let's talk about #30: Meter export"))
    );
    assert_eq!(
        post(&s, "chat-about", json!({"fleet": "nope", "issue": 30}), &ck).1,
        "NO_ORCHESTRATOR"
    );
    assert_eq!(
        post(&s, "close", json!({"fleet": "sendmeter", "issue": 12}), &ck).1,
        "NOT_BACKLOG"
    );
    assert_eq!(post(&s, "close", json!({"fleet": "sendmeter"}), &ck).1, "BAD_VALUE");
    assert!(!host.called("gh issue close"));
    assert_eq!(post(&s, "close", about, &ck), (200, "OK".into(), "closed #30".into()));
    assert!(host.called("gh issue close 30 -R jirathip-dev/sendmeter"));
    let e = kinds(&c, "closed").pop().unwrap();
    assert_eq!(
        (e.issue, e.str("why"), e.str("via")),
        (Some(30), Some("backlog"), Some("web"))
    );
    let st = drover::events::fold(&read(&c.state).unwrap());
    assert_eq!(st[&("sendmeter".to_string(), 30)].state, "closed");
}
