mod common;
use common::fake_shed::Cli;
use common::*;
use drover::serve::Server;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;
use std::time::Duration;

const ADDR: &str = "127.0.0.1:7777";
const T: u64 = 1_000_000;

fn acp_setup(name: &str, b: &FakeShed) -> drover::Ctx {
    let d = tmpdir(name);
    let cfg = format!(
        "{CONFIG}\n[fleet.sendmeter.role.orchestrator]\nharness = \"claude\"\nmodel = \"opus\"\naccount = \"C\"\n\n\
         [fleet.sendmeter.runtime]\nkind = \"shed-acp\"\nsocket = \"{}\"\n\n[accounts]\nC = \"C\"\n",
        b.sock.display()
    );
    let mut c = ctx(&d, &cfg);
    c.exe = env!("CARGO_BIN_EXE_drover").into();
    assert_eq!(run(&c, &Fake::default(), &["profiles", "sync"]).0, 0);
    assert_eq!(
        run(&c, &Fake::default(), &["thread", "new", "sendmeter", "Look"]).1,
        "t1\n"
    );
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

fn say(c: &drover::Ctx, r: &dyn drover::runner::Runner, text: &str) -> String {
    let s = Server::new(c, r, Some(ADDR.into()));
    let cookie = pair(&s, c, "Mozilla/5.0 (iPhone)", T);
    let body = format!("op=say&t=t1&text={text}");
    let req = format!(
        "POST /thread HTTP/1.1\r\nHost: {ADDR}\r\n{cookie}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let res = String::from_utf8_lossy(&s.respond(&req, T)).into_owned();
    assert!(res.ends_with("\r\n\r\nsent\n"), "{res}");
    drover::events::jsonl(&c.state).unwrap()
}

fn starts(c: &drover::Ctx) -> Vec<drover::events::Event> {
    let all = drover::events::read(&c.state).unwrap().to_vec();
    all.into_iter().filter(|e| e.kind == "orchestrator-start").collect()
}

struct Slow<'a> {
    cli: Cli<'a>,
    began: Mutex<Sender<()>>,
    go: Mutex<Receiver<()>>,
}
impl drover::runner::Runner for Slow<'_> {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<drover::runner::Out> {
        if prog == "shed" && args[0] == "run" {
            self.began.lock().unwrap().send(()).unwrap();
            self.go.lock().unwrap().recv().unwrap();
        }
        self.cli.run(prog, args)
    }
}

#[test]
fn two_messages_while_a_start_is_in_flight_start_one_orchestrator_and_reach_it_in_order() {
    let b = FakeShed::new();
    let c = acp_setup("adopt-serial", &b);
    let (began, began_rx) = channel();
    let (go, go_rx) = channel();
    let f = acp();
    let r = Slow {
        cli: b.cli(&f),
        began: Mutex::new(began),
        go: Mutex::new(go_rx),
    };
    let lists = || b.methods().iter().filter(|m| *m == "ls").count();
    std::thread::scope(|s| {
        let a = s.spawn(|| run(&c, &r, &["thread", "say", "t1", "one"]));
        began_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let before = lists();
        let second = s.spawn(|| run(&c, &r, &["thread", "say", "t1", "two"]));
        std::thread::sleep(Duration::from_millis(500));
        let raced = lists() > before;
        go.send(()).unwrap();
        go.send(()).unwrap();
        let (a, second) = (a.join().unwrap(), second.join().unwrap());
        assert_eq!((a.0, second.0), (0, 0), "{a:?} {second:?}");
        assert!(!raced, "the second delivery did not wait: {:?}", f.calls.borrow());
    });
    assert_eq!(f.count("shed run"), 1, "{:?}", f.calls.borrow());
    let dir = c.state.with_file_name("orchestrator").join("t1");
    assert_eq!(std::fs::read_to_string(dir.join(".thread.md")).unwrap(), "owner: one\n");
    let typed = f.index("shed send orchestrator-t1 [t1] owner: two");
    assert!(typed > f.index("shed run"), "{:?}", f.calls.borrow());
}

#[test]
fn a_failed_start_is_tried_once_more_and_shows_one_row() {
    let b = FakeShed::new();
    let c = acp_setup("adopt-retry", &b);
    let f = acp()
        .once_err("shed run", 1, "bridge down")
        .once_err("shed run", 1, "bridge down");
    let log = say(&c, &b.on(&b.cli(&f)), "hello");
    assert_eq!(log.matches("✗ not delivered").count(), 1, "{log}");
    assert!(log.contains("bridge down"), "{log}");
    assert_eq!(f.count("shed run"), 2, "{:?}", f.calls.borrow());
    assert!(starts(&c).is_empty());
    let c = acp_setup("adopt-retry-once", &b);
    let f = acp().once_err("shed run", 1, "bridge down");
    let log = say(&c, &b.on(&b.cli(&f)), "hello");
    assert!(!log.contains("not delivered"), "{log}");
    assert_eq!(f.count("shed run"), 2, "{:?}", f.calls.borrow());
    assert_eq!(starts(&c)[0].str("runtime"), Some("shed-acp"));
}
