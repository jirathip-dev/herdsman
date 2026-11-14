mod common;
use common::*;
use drover::events::{append, read, Event};
use drover::runner::{argv, Out, Runner};
use drover::service::{grant, Grant};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;

const NOW: u64 = 1_000_000;

struct Lane(Fake, drover::Ctx);

impl Runner for Lane {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        if prog != "env" {
            return self.0.run(prog, args);
        }
        assert!(args[0].starts_with("DROVER_LANE=") && args[1] == self.1.exe, "{args:?}");
        let (code, stdout) = run(
            &self.1,
            &self.0,
            &args[2..].iter().map(String::as_str).collect::<Vec<_>>(),
        );
        Ok(Out {
            code,
            stdout,
            stderr: String::new(),
        })
    }
}

fn post(addr: &str, body: &Value) -> (u16, Value) {
    let mut s = TcpStream::connect(addr).unwrap();
    let b = body.to_string();
    let req = format!(
        "POST /api/v1/cmd HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{b}",
        b.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut res = String::new();
    s.read_to_string(&mut res).unwrap();
    let status = res[9..12].parse().unwrap();
    (
        status,
        serde_json::from_str(res.split("\r\n\r\n").nth(1).unwrap()).unwrap(),
    )
}

fn lane(c: &drover::Ctx, role: &str, at: u64) -> String {
    let agent = format!("sendmeter-{role}-12");
    let d = json!({"role": role, "agent": agent, "pane": format!("p-{role}")});
    append(&c.state, &Event::new(at, "sendmeter", Some(12), "spawn", d)).unwrap();
    let g = Grant {
        fleet: "sendmeter".into(),
        issue: 12,
        role: role.into(),
        agent: agent.clone(),
        lane: format!("sendmeter:12:{role}"),
        at,
    };
    grant(c, &g).unwrap()
}

#[test]
fn the_service_runs_a_lanes_command_with_its_token_and_refuses_any_other() {
    let c = ctx(&tmpdir("service"), CONFIG);
    std::fs::create_dir_all(c.state.parent().unwrap()).unwrap();
    append(
        &c.state,
        &Event::new(NOW - 9, "sendmeter", Some(12), "queued", json!({})),
    )
    .unwrap();
    let imp = lane(&c, "impl", NOW - 5);
    let rev = lane(&c, "rev", NOW - 4);
    let other = {
        let g = Grant {
            fleet: "sendmeter".into(),
            issue: 13,
            role: "impl".into(),
            agent: "sendmeter-impl-13".into(),
            lane: "sendmeter:13:impl".into(),
            at: NOW - 3,
        };
        grant(&c, &g).unwrap()
    };
    let api = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = api.local_addr().unwrap().to_string();
    let sh = FakeShed::new();
    sh.attach(&c);
    let r: &'static Lane = Box::leak(Box::new(Lane(Fake::default(), c.clone())));
    let sc = c.clone();
    std::thread::spawn(move || drover_service::run(sc, r, || NOW, api));
    let mut pid = None;
    for _ in 0..100 {
        pid = drover::service::up(&c);
        if pid.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(pid, Some(u64::from(std::process::id())));
    assert_eq!(
        drover::service::kick(&c, "sendmeter", 12, "push", Some("t1")),
        Some(std::process::id())
    );

    let ask = json!([
        "ask",
        "sendmeter",
        "12",
        "Which colour?",
        "--option",
        "red",
        "--option",
        "blue"
    ]);
    let asks = |c: &drover::Ctx| {
        read(&c.state)
            .unwrap()
            .iter()
            .filter(|e| e.kind == "needs-decision")
            .count()
    };
    for (token, args) in [
        (&other, ask.clone()),
        (&"f".repeat(64), ask.clone()),
        (
            &imp,
            json!([
                "verdict",
                "sendmeter",
                "12",
                "PASS",
                "--sha",
                "a".repeat(40),
                "--file",
                "v.md"
            ]),
        ),
    ] {
        let (status, v) = post(&addr, &json!({"token": token, "args": args}));
        assert_eq!(status, 403, "{v}");
        assert!(v["stdout"].as_str().unwrap().starts_with("REFUSED TOKEN"), "{v}");
    }
    assert_eq!(asks(&c), 0);

    let (status, v) = post(&addr, &json!({"token": imp, "args": []}));
    assert_eq!(status, 403, "{v}");
    let secret = c.state.with_file_name("secret.md");
    std::fs::write(&secret, "the main host's own file").unwrap();
    let v_args = json!([
        "verdict",
        "sendmeter",
        "12",
        "FAIL",
        "--sha",
        "a".repeat(40),
        "--file",
        secret
    ]);
    let (status, v) = post(&addr, &json!({"token": rev, "args": v_args, "files": {}}));
    assert_eq!(status, 400, "{v}");
    assert!(read(&c.state).unwrap().iter().all(|e| e.kind != "verdict"), "{v}");

    let (status, v) = post(&addr, &json!({"token": imp, "args": ask}));
    assert_eq!((status, v["code"].as_i64()), (200, Some(0)), "{v}");
    assert_eq!(asks(&c), 1);

    let d = json!({"agent": "sendmeter-rev-12", "pane": "p-rev"});
    append(&c.state, &Event::new(NOW, "sendmeter", Some(12), "lane-closed", d)).unwrap();
    let (status, v) = post(
        &addr,
        &json!({"token": rev, "args": ["done", "sendmeter", "12", "--no-change", "x"]}),
    );
    assert_eq!(status, 403, "{v}");
    assert!(v["stdout"].as_str().unwrap().contains("lane is closed"), "{v}");

    let mut s = TcpStream::connect(&addr).unwrap();
    s.write_all(b"GET /api/v1/events?after=0 HTTP/1.1\r\nHost: x\r\n\r\n")
        .unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let (mut got, mut buf) = (String::new(), [0u8; 8192]);
    while !(got.contains("\"service-started\"") && got.contains("Which colour?")) {
        let n = s.read(&mut buf).unwrap();
        assert!(n > 0, "{got}");
        got += &String::from_utf8_lossy(&buf[..n]);
    }
    assert!(
        got.starts_with("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream"),
        "{got}"
    );
}

struct Proc(std::process::Child);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn service(c: &drover::Ctx, bin: &std::path::Path, api: &str) -> (Proc, u64) {
    let home = c.state.parent().unwrap().parent().unwrap();
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_drover"))
        .args(["service", "--api", api])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("HOME", home)
        .env("DROVER_HOME", home)
        .env("FLEET_CONFIG", &c.config)
        .env("FLEET_STATE", &c.state)
        .env("FLEET_WORKTREES", &c.worktrees)
        .env("FLEET_HERMES_PROFILES", &c.homes["hermes"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let p = Proc(child);
    for _ in 0..200 {
        if let Some(pid) = drover::service::up(c) {
            return (p, pid);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("the service never answered");
}

fn until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..400 {
        if f() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("never: {what}");
}

#[test]
fn a_service_killed_mid_spawn_leaves_no_stale_state_and_a_new_one_resumes() {
    let d = tmpdir("service-chaos");
    let cfg = CONFIG.replace(
        "max_parallel_spawns = 0",
        "max_parallel_spawns = 2\nmin_free_gb = 0\nmin_free_mem_gb = 0",
    );
    let mut c = ctx(&d, &cfg);
    c.now = drover::now();
    assert_eq!(run(&c, &Fake::default(), &["queue", "sendmeter", "add", "1"]).0, 0);
    let (bin, log) = (d.join("bin"), d.join("gh.log"));
    std::fs::create_dir_all(&bin).unwrap();
    let tools = [
        (
            "gh",
            format!(
                "case \"$*\" in *'issue view'*) echo \"$*\" >> '{}'; exec sleep 60;; *) echo '[]';; esac",
                log.display()
            ),
        ),
        ("hermes", "exit 0".into()),
        ("shed", "exit 0".into()),
    ];
    for (name, body) in tools {
        let p = bin.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
    let sh = FakeShed::new();
    sh.attach(&c);
    let pass = |r: &Fake| assert_eq!(run(&c, r, &["drive"]).0, 0);
    let host = || {
        Fake::default()
            .reply("sysctl -n kern.memorystatus_vm_pressure_level", "1\n")
            .reply("sysctl -n vm.loadavg", "{ 1.0 1.0 1.0 }\n")
            .reply("sysctl -n hw.ncpu", "10\n")
            .reply("vm_stat", "Pages free: 1.\nSwapouts:                     100.\n")
            .reply("git -C", "")
    };
    let cfgv = drover::config::Config::load(&c.config).unwrap();
    let live = || drover::drive::markers(&c, &cfgv, &drover::runner::Real, false).0;
    let marker = || lease_on(&c, "spawn-sendmeter-1.pending");
    let tok = lane(&c, "impl", c.now);
    let ask = argv(&["ask", "sendmeter", "12", "Which colour?", "--option", "red"]);

    let (a, pid_a) = service(&c, &bin, "127.0.0.1:0");
    pass(&host());
    assert_eq!(marker().map(|l| u64::from(l.pid)), Some(pid_a));
    until("the spawn reached GitHub", || {
        std::fs::read_to_string(&log).is_ok_and(|s| s.contains("issue view 1"))
    });
    assert_eq!(live(), ["spawn-sendmeter-1.pending", "kick-sendmeter-1.lock"]);

    let api_a = read(&c.state)
        .unwrap()
        .iter()
        .rfind(|e| e.kind == "service-started")
        .and_then(|e| e.str("api").map(String::from))
        .unwrap();
    drop(a);
    assert!(live().is_empty(), "{:?}", live());
    assert_eq!(drover::service::up(&c), None);
    let t0 = std::time::Instant::now();
    let down = drover::service::post(&drover::runner::Real, &format!("http://{api_a}"), &tok, &ask);
    assert!(down.is_err() && t0.elapsed().as_secs() < 10, "{down:?}");

    let (_b, pid_b) = service(&c, &bin, "127.0.0.1:0");
    assert_ne!(pid_a, pid_b);
    pass(&host());
    let ev = read(&c.state).unwrap();
    assert!(ev.iter().any(|e| e.kind == "stale-marker" && e.issue == Some(1)));
    assert_eq!(marker().map(|l| u64::from(l.pid)), Some(pid_b));
    until("the spawn was retried", || {
        std::fs::read_to_string(&log).is_ok_and(|s| s.matches("issue view 1").count() == 2)
    });
    let api_b = read(&c.state)
        .unwrap()
        .iter()
        .rfind(|e| e.kind == "service-started")
        .and_then(|e| e.str("api").map(String::from))
        .unwrap();
    let up = drover::service::post(&drover::runner::Real, &format!("http://{api_b}"), &tok, &ask).unwrap();
    assert_eq!(up.code, 0, "{}{}", up.stdout, up.stderr);
}
