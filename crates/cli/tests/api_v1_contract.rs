mod common;
use common::*;
use drover::serve::Server;
use drover_server::api::*;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::path::Path;

const ADDR: &str = "127.0.0.1:7777";
const T: u64 = 1_000_000;

const POSTS: &[&str] = &[
    "mv",
    "pause",
    "resume",
    "set",
    "cond",
    "answer",
    "drive",
    "override",
    "extend",
    "park",
    "unpark",
    "orphans",
    "reap",
    "host",
    "bqueue",
    "unpair",
    "send",
    "menu",
    "act",
    "setup",
    "chat-about",
    "chat",
    "close",
    "qset",
    "lesson",
    "conductor-detach",
];

fn shape(v: &Value) -> Value {
    match v {
        Value::Null => json!("null"),
        Value::Bool(_) => json!("boolean"),
        Value::Number(_) => json!("number"),
        Value::String(_) => json!("string"),
        Value::Array(a) => json!(a.iter().map(shape).reduce(merge).into_iter().collect::<Vec<_>>()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), shape(v))).collect()),
    }
}

fn merge(a: Value, b: Value) -> Value {
    match (a, b) {
        (Value::Object(mut a), Value::Object(b)) => {
            for (k, v) in b {
                let m = match a.remove(&k) {
                    Some(o) => merge(o, v),
                    None => v,
                };
                a.insert(k, m);
            }
            Value::Object(a)
        }
        (Value::Array(a), Value::Array(b)) => {
            json!(a.into_iter().chain(b).reduce(merge).into_iter().collect::<Vec<_>>())
        }
        (a, b) if a == json!("null") => b,
        (a, _) => a,
    }
}

fn diff(at: &str, want: &Value, got: &Value, out: &mut Vec<String>) {
    match (want, got) {
        _ if want == "null" || got == "null" => {}
        (Value::Object(w), Value::Object(g)) => {
            out.extend(
                w.keys()
                    .filter(|k| !g.contains_key(*k))
                    .map(|k| format!("{at}.{k}: gone")),
            );
            out.extend(
                g.keys()
                    .filter(|k| !w.contains_key(*k))
                    .map(|k| format!("{at}.{k}: new")),
            );
            for (k, w) in w {
                g.get(k).inspect(|g| diff(&format!("{at}.{k}"), w, g, out));
            }
        }
        (Value::Array(w), Value::Array(g)) => {
            if let (Some(w), Some(g)) = (w.first(), g.first()) {
                diff(&format!("{at}[]"), w, g, out);
            }
        }
        _ if want != got => out.push(format!("{at}: {want} → {got}")),
        _ => {}
    }
}

fn schema<T: Serialize + Default>() -> Value {
    shape(&serde_json::to_value(T::default()).unwrap())
}

fn check(name: &str, route: &str, (ty, schema): (&str, Value), got: Value, fails: &mut Vec<String>) {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/golden/api_v1/{name}.json"));
    let fresh = json!({"route": route, "type": ty, "schema": schema, "response": got});
    if std::env::var_os("BLESS").is_some() {
        std::fs::write(&file, serde_json::to_string_pretty(&fresh).unwrap() + "\n").unwrap();
    }
    let Some(old) = std::fs::read_to_string(&file)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        return fails.push(format!("{name}: no fixture {}", file.display()));
    };
    let mut out = vec![];
    if old["route"] != route || old["type"] != ty || old["schema"] != schema {
        out.push(format!(
            "route or type schema changed: {} {}",
            old["route"], old["type"]
        ));
    }
    check_shapes(&schema, &old["response"], &got, &mut out);
    fails.extend(out.into_iter().map(|o| format!("{name} ({route}): {o}")));
}

fn call(s: &Server, c: &drover::Ctx, (method, path): (&str, &str), body: &str, cookie: &str) -> Value {
    let r = format!(
        "{method} {path} HTTP/1.1\r\nHost: {ADDR}\r\n{cookie}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let res = String::from_utf8_lossy(&s.respond(&r, T)).into_owned();
    let tmp = c.state.parent().unwrap().parent().unwrap().display().to_string();
    let body = res.split_once("\r\n\r\n").unwrap().1.replace(&tmp, "<tmp>");
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("{method} {path}: {e}: {res}"))
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
fn every_route_keeps_its_fixtures_shape() {
    let (c, r) = v1_board_on("v1-contract", Fake::default().reply("gh", ""));
    let r = Bazzite(r, FakeShed::new());
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let ck = pair(&s, &c, "Mozilla/5.0 (iPhone)", T);
    let post = |op: &str, v: Value| call(&s, &c, ("POST", &format!("/api/v1/{op}")), &v.to_string(), &ck);
    post("chat-about", json!({"fleet": "sendmeter", "issue": 30}));
    let ev = drover::events::read(&c.state).unwrap();
    let t = ev.iter().rev().find_map(|e| e.str("thread")).unwrap().to_string();
    post("setup", json!({"host": "bazzite", "name": "claude B"}));
    r.1.screen("drover-setup-claude-B", "Paste code here >\n");
    let mut fails = vec![];
    let get = |p: &str| call(&s, &c, ("GET", p), "", &ck);
    macro_rules! gets {
        ($(($name:literal, $path:expr, $ty:ty)),*) => {$(
            let route = format!("GET /api/v1/{}", $name);
            let file = $name.split('/').filter(|s| !s.starts_with('<')).collect::<Vec<_>>().join("-");
            check(&file, &route, (stringify!($ty), schema::<$ty>()), get(&$path), &mut fails);
        )*};
    }
    gets!(
        ("state", "/api/v1/state", State),
        ("events", "/api/v1/events?since=0", Events),
        ("host", "/api/v1/host", HostTab),
        ("queue", "/api/v1/queue", QueueTab),
        ("needs", "/api/v1/needs", NeedsTab),
        ("lanes", "/api/v1/lanes", LanesTab),
        ("shed", "/api/v1/shed", Vec<ShedRow>)
    );
    let cfg = drover::config::Config::load(&c.config).unwrap();
    let h = [t.as_str(), "sendmeter", "Facets", "add facets\nthen reindex"];
    drover::orchestrator::handoff(&c, &cfg, &r, h).unwrap();
    gets!(
        ("chat", "/api/v1/chat", ChatTab),
        ("thread/<t>", format!("/api/v1/thread/{t}"), ThreadPage),
        ("repo/<fleet>/threads", "/api/v1/repo/sendmeter/threads", RepoThreads),
        ("lane/<agent>", format!("/api/v1/lane/{IMPL}"), LaneChat),
        ("herd", "/api/v1/herd", Herd),
        ("roles", "/api/v1/roles?fleet=sendmeter", Roles),
        ("setup/<pane>", "/api/v1/setup/drover-setup-claude-B", SetupScreen),
        ("issue/<fleet>/<n>", "/api/v1/issue/sendmeter/12", IssueView),
        ("lessons", "/api/v1/lessons", Lessons),
        (
            "lessons/<fleet>/digest",
            "/api/v1/lessons/sendmeter/digest?days=7",
            Digest
        )
    );
    let m = get("/api/v1/state")["mtime"].clone();
    for op in POSTS {
        let got = post(
            op,
            json!({"fleet": "sendmeter", "agent": IMPL, "mtime": m, "text": "hi"}),
        );
        let route = format!("POST /api/v1/{op}");
        check(
            &format!("post-{op}"),
            &route,
            ("Reply", schema::<Reply>()),
            got,
            &mut fails,
        );
    }
    let since = get("/api/v1/events?since=0")["seq"].clone();
    let attach = json!({"fleet": "sendmeter", "thread": "cooee-7f3a"});
    let ty = ("ConductorAttach", schema::<ConductorAttach>());
    check(
        "post-conductor-attach",
        "POST /api/v1/conductor-attach",
        ty,
        post("conductor-attach", attach),
        &mut fails,
    );
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
    ];
    assert_eq!(run(&c, &r, &[&ask[..], &["--thread", "cooee-7f3a"]].concat()).0, 0);
    let got = get(&format!("/api/v1/events?since={since}"));
    check(
        "events-conductor-attach",
        "GET /api/v1/events",
        ("Events", schema::<Events>()),
        got,
        &mut fails,
    );
    assert!(
        fails.is_empty(),
        "API v1 shapes changed (`BLESS=1` updates the fixtures):\n{}",
        fails.join("\n")
    );
}

#[test]
fn a_changed_shape_fails_the_contract() {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/api_v1/state.json");
    let old: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    let mut got = old["response"].clone();
    let schema = schema::<State>();
    let fails = |got: &Value| {
        let mut out = vec![];
        check_shapes(&schema, &old["response"], got, &mut out);
        out
    };
    assert_eq!(fails(&got), Vec::<String>::new(), "the recorded response fits");
    let fleets = got["fleets"].as_array_mut().unwrap();
    let f: &mut Map<String, Value> = fleets[0].as_object_mut().unwrap();
    let repo = f.remove("repo").unwrap();
    f.insert("repository".into(), repo);
    got["seq"] = json!("12");
    assert_eq!(
        fails(&got),
        [
            ".seq: \"number\" → \"string\"",
            ".fleets[].repo: gone",
            ".fleets[].repository: new",
            ".seq: \"number\" → \"string\""
        ]
    );
}

fn check_shapes(schema: &Value, old: &Value, got: &Value, out: &mut Vec<String>) {
    diff("", schema, &shape(got), out);
    diff("", &shape(old), &shape(got), out);
}
