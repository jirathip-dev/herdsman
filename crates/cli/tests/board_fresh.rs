mod common;
use common::*;
use drover::events::read;
use drover::runner::{Out, Runner};
use drover::serve::Server;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

const ADDR: &str = "127.0.0.1:7777";
const NOW: u64 = 1_000_000;

fn get(s: &Server, now: u64, path: &str) -> Value {
    let res = s.respond(&format!("GET {path} HTTP/1.1\r\nHost: {ADDR}\r\n\r\n"), now);
    let res = String::from_utf8_lossy(&res);
    assert!(res.starts_with("HTTP/1.1 200"), "{path}: {res}");
    serde_json::from_str(&res[res.find("\r\n\r\n").unwrap() + 4..]).unwrap()
}

struct Connect(Fake, FakeShed);
impl Runner for Connect {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        self.0.run(prog, args)
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> anyhow::Result<Out> {
        if prog != "shed" {
            return self.0.run_in(prog, args, input);
        }
        self.0.calls.borrow_mut().push(format!("{prog} {}", args.join(" ")));
        Ok(Out {
            stdout: self.1.connect(input),
            ..Out::default()
        })
    }
    fn sleep(&self, _: u64) {}
    fn shed(&self) -> Option<drover::shed::Shed> {
        let wire = drover::shed::Wire::Connect;
        Some(drover::shed::Shed { wire, ..self.1.shed() })
    }
}

#[test]
fn drover_shows_the_data_age_once_the_background_read_is_over_10s_old() {
    let c = ctx(&tmpdir("fresh-age"), CONFIG);
    std::fs::create_dir_all(c.state.parent().unwrap()).unwrap();
    let r = Connect(Fake::default(), FakeShed::new());
    let s = Server::new(&c, &r, Some(ADDR.into())).deferring();
    let age = |now| {
        let v = get(&s, now, "/screens/queue");
        let b = v["blocks"].as_array().unwrap().iter().find(|b| b["id"] == "age");
        b.map(|b| b["markdown"].clone())
    };
    get(&s, NOW, "/api/v1/state");
    assert!(r.1.methods().is_empty(), "the request ran shed: {:?}", r.1.methods());
    assert_eq!(age(NOW), None, "nothing read yet: no age to show");
    s.refresh(NOW);
    assert!(
        r.0.called("shed connect --autostart") && r.1.methods().contains(&"ls".to_string()),
        "the background read what the request asked for"
    );
    assert_eq!(get(&s, NOW + 5, "/api/v1/state")["sources"]["shed"], NOW);
    assert_eq!(age(NOW + 5), None);
    assert_eq!(age(NOW + 11), Some(json!("data 11s old")));
    s.refresh(NOW + 11);
    assert_eq!(age(NOW + 12), None, "read again: fresh");
}

struct Slow(Fake, AtomicU64);
impl Runner for Slow {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        std::thread::sleep(std::time::Duration::from_millis(self.1.load(Relaxed)));
        self.0.run(prog, args)
    }
    fn sleep(&self, _: u64) {}
}

#[test]
fn a_slow_api_for_10_minutes_alerts_once_and_never_after_it_recovers() {
    let dir = tmpdir("slo");
    let sink = dir.join("push.txt");
    let cfg = format!(
        "{CONFIG}slo_api_p95_ms = 20\n\n[[notify]]\ncmd = \"cat >> '{}'\"\non = [\"slo-api\"]\n",
        sink.display()
    );
    let c = ctx(&dir, &cfg);
    std::fs::create_dir_all(c.state.parent().unwrap()).unwrap();
    let _sh = FakeShed::attached(&c);
    let r = Slow(Fake::default(), AtomicU64::new(40));
    let s = Server::new(&c, &r, Some(ADDR.into()));
    let pushes = || std::fs::read_to_string(&sink).unwrap_or_default();
    let card = |now| {
        let v = get(&s, now, "/screens/cards");
        v.as_array().unwrap().iter().find(|c| c["kind"] == "alert").cloned()
    };
    for m in 0..9 {
        get(&s, NOW + 60 * m, "/api/v1/state");
        s.slo(NOW + 60 * m);
        assert_eq!(pushes(), "", "minute {}: under 10 min is no alert", m + 1);
    }
    get(&s, NOW + 530, "/api/v1/health");
    let h = get(&s, NOW + 530, "/api/v1/health");
    let state = h["routes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["route"] == "/api/v1/state")
        .unwrap_or_else(|| panic!("{h}"))
        .clone();
    assert_eq!(state["count"], 9, "{h}");
    assert!(state["p95"].as_u64().unwrap() >= 40, "{h}");
    assert!(
        h["routes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["route"] == "/api/v1/health"),
        "{h}"
    );
    get(&s, NOW + 540, "/api/v1/state");
    s.slo(NOW + 540);
    s.slo(NOW + 570);
    let lines: Vec<String> = pushes().lines().map(String::from).collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("/api/v1/state"), "{lines:?}");
    let alerts: Vec<_> = (read(&c.state).unwrap().to_vec().into_iter())
        .filter(|e| e.kind.starts_with("slo-api"))
        .collect();
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].str("route"), Some("/api/v1/state"));
    let shown = card(NOW + 570).expect("the slo card").to_string();
    assert!(shown.contains("/api/v1/state p95 "), "{shown}");
    r.1.store(0, Relaxed);
    for m in 10..13 {
        get(&s, NOW + 60 * m, "/api/v1/state");
        s.slo(NOW + 60 * m);
    }
    let kinds: Vec<String> = (read(&c.state).unwrap().to_vec().into_iter())
        .filter(|e| e.kind.starts_with("slo-api"))
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, ["slo-api", "slo-api-ok"]);
    assert_eq!(pushes().lines().count(), 1);
    assert_eq!(card(NOW + 900), None);
}
