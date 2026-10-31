#![allow(dead_code)]
use herdsman::runner::{Out, Runner};
use herdsman::Ctx;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

pub const CONFIG: &str = r#"# fleet config (test)
[fleet.sendmeter]
repo = "jirathip-dev/sendmeter"
local = "/tmp/does-not-matter/sendmeter"
integration = "staging"   # never main

[fleet.sendmeter.role.orch]
harness = "hermes"
profile = "fleet-orch"

[fleet.sendmeter.role.impl]
harness  =  "hermes"    # toggled in tests
profile = "fleet-impl"
model = "opus"

[fleet.sendmeter.role.rev]
harness = "hermes"
profile = "fleet-rev"

[limits]
review_rounds = 3
stall_minutes = 30
"#;

/// RefCell's API over an RwLock: runners are `Sync` (serve's watcher has its own thread).
#[derive(Default)]
pub struct Shared<T>(RwLock<T>);

impl<T: Copy> Shared<T> {
    pub fn new(v: T) -> Self {
        Shared(RwLock::new(v))
    }
    pub fn get(&self) -> T {
        *self.borrow()
    }
    pub fn set(&self, v: T) {
        *self.borrow_mut() = v;
    }
}

impl<T> Shared<T> {
    pub fn borrow(&self) -> RwLockReadGuard<'_, T> {
        self.0.read().unwrap()
    }
    pub fn borrow_mut(&self) -> RwLockWriteGuard<'_, T> {
        self.0.write().unwrap()
    }
}

/// Scripted fake: one-shot replies (`once`) are consumed first, in order; then the
/// first reply whose prefix matches the command line wins; unscripted commands
/// fail with exit 1. `sleep` is recorded as a `sleep <secs>` call and returns at once.
#[derive(Default)]
pub struct Fake {
    pub calls: Shared<Vec<String>>,
    pub replies: Vec<(String, Out)>,
    pub onces: Shared<Vec<(String, Out)>>,
}

impl Fake {
    pub fn reply(mut self, prefix: &str, stdout: &str) -> Self {
        self.replies.push((prefix.into(), Out { code: 0, stdout: stdout.into(), stderr: String::new() }));
        self
    }
    pub fn once(self, prefix: &str, code: i32, stdout: &str) -> Self {
        self.onces.borrow_mut().push((prefix.into(), Out { code, stdout: stdout.into(), stderr: String::new() }));
        self
    }
    pub fn once_err(self, prefix: &str, code: i32, stderr: &str) -> Self {
        self.onces.borrow_mut().push((prefix.into(), Out { code, stdout: String::new(), stderr: stderr.into() }));
        self
    }
    pub fn called(&self, prefix: &str) -> bool {
        self.calls.borrow().iter().any(|c| c.starts_with(prefix))
    }
    pub fn count(&self, prefix: &str) -> usize {
        self.calls.borrow().iter().filter(|c| c.starts_with(prefix)).count()
    }
    pub fn index(&self, prefix: &str) -> Option<usize> {
        self.calls.borrow().iter().position(|c| c.starts_with(prefix))
    }
}

impl Runner for Fake {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        let line = format!("{prog} {}", args.join(" "));
        self.calls.borrow_mut().push(line.clone());
        let mut onces = self.onces.borrow_mut();
        if let Some(i) = onces.iter().position(|(p, _)| line.starts_with(p.as_str())) {
            return Ok(onces.remove(i).1);
        }
        for (p, o) in &self.replies {
            if line.starts_with(p.as_str()) {
                return Ok(o.clone());
            }
        }
        Ok(Out { code: 1, stdout: String::new(), stderr: format!("unscripted: {line}") })
    }
    fn sleep(&self, secs: u64) {
        self.calls.borrow_mut().push(format!("sleep {secs}"));
    }
}

/// `herdr agent list` JSON for (name, status, cwd, pane) rows.
pub fn agents(rows: &[(&str, &str, &str, &str)]) -> String {
    let list: Vec<_> = rows
        .iter()
        .map(|(n, s, cwd, p)| serde_json::json!({"name": n, "agent_status": s, "cwd": cwd, "pane_id": p}))
        .collect();
    serde_json::json!({"result": {"agents": list}}).to_string()
}

pub const NO_AGENTS: &str = r#"{"result":{"agents":[]}}"#;

pub fn tmpdir(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let d = std::env::temp_dir().join(format!(
        "fleet-test-{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub fn ctx(dir: &Path, config: &str) -> Ctx {
    let cfgdir = dir.join("config");
    std::fs::create_dir_all(&cfgdir).unwrap();
    std::fs::write(cfgdir.join("fleet.toml"), config).unwrap();
    Ctx {
        config: cfgdir.join("fleet.toml"),
        state: dir.join("state/events.jsonl"),
        worktrees: dir.join("worktrees"),
        hermes: dir.join("hermes"),
        exe: "/opt/herdsman/bin/herdsman".into(),
        now: 1_000_000,
        home: dir.join("home"),
        hints: vec![],
    }
}

pub fn run(ctx: &Ctx, r: &dyn Runner, args: &[&str]) -> (i32, String) {
    let mut out = Vec::new();
    let mut argv = vec!["herdsman".to_string()];
    argv.extend(args.iter().map(|s| s.to_string()));
    let code = herdsman::main_with(argv, ctx, r, &mut out, &mut std::io::empty());
    (code, String::from_utf8(out).unwrap())
}

/// `swift` for tests/support/webview.swift, its page based at a local server of serve's `/a/<hash>/`
/// assets (#57: the page links them; a same-origin base lets herd.js fetch horses.svg).
pub fn swift() -> std::process::Command {
    static BASE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let base = BASE.get_or_init(|| {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/", l.local_addr().unwrap());
        std::thread::spawn(move || {
            let (c, r) = (ctx(&tmpdir("assets"), CONFIG), Fake::default());
            let s = herdsman::serve::Server::new(&c, &r, None);
            for mut st in l.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let n = std::io::Read::read(&mut st, &mut buf).unwrap_or(0);
                let _ = std::io::Write::write_all(&mut st, &s.respond(&String::from_utf8_lossy(&buf[..n]), 1));
            }
        });
        base
    });
    let mut c = std::process::Command::new("swift");
    c.env("HERDSMAN_BASE", base);
    c
}
