#![allow(dead_code)]
use herdsman::runner::{Out, Runner};
use herdsman::Ctx;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

pub const CONFIG: &str = r#"# fleet config (test)
[fleet.sendmeter]
repo = "jirathip-dev/sendmeter"
local = "/tmp/does-not-matter/sendmeter"
integration = "staging"   # never main

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
max_parallel_spawns = 0   # spawns in the pass; tests/drive_kicks.rs runs them as kicks (#318)
build_root = ""           # #438: no shared CARGO_TARGET_DIR (and no trim of the real one) unless a test sets it
stray_roots = []          # #536: never clean the real /tmp or $TMPDIR
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
    /// #441: the fake OS store (service -> secret); `no_store` makes it unusable, as on a host without one.
    pub store: Shared<std::collections::BTreeMap<String, String>>,
    pub no_store: bool,
    /// Each `run_in`'s command line and its stdin.
    pub inputs: Shared<Vec<(String, String)>>,
}

impl Fake {
    pub fn reply(mut self, prefix: &str, stdout: &str) -> Self {
        self.replies.push((
            prefix.into(),
            Out {
                code: 0,
                stdout: stdout.into(),
                stderr: String::new(),
            },
        ));
        self
    }
    pub fn once(self, prefix: &str, code: i32, stdout: &str) -> Self {
        self.onces.borrow_mut().push((
            prefix.into(),
            Out {
                code,
                stdout: stdout.into(),
                stderr: String::new(),
            },
        ));
        self
    }
    pub fn once_err(self, prefix: &str, code: i32, stderr: &str) -> Self {
        self.onces.borrow_mut().push((
            prefix.into(),
            Out {
                code,
                stdout: String::new(),
                stderr: stderr.into(),
            },
        ));
        self
    }
    /// #441: `svc` holds `value` in the fake store.
    pub fn stored(self, svc: &str, value: &str) -> Self {
        self.store.borrow_mut().insert(svc.into(), value.into());
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

/// #337's batch suffix: each remote read in the step's one ssh script ends with it.
pub const BATCH: &str = " </dev/null 2>/dev/null; printf '\\036%s\\036' \"$?\"";

impl Runner for Fake {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        let line = format!("{prog} {}", args.join(" "));
        self.calls.borrow_mut().push(line.clone());
        // #337: a batched ssh script answers each of its reads as the one call it stands for (also logged).
        let script = args
            .last()
            .and_then(|a| a.strip_prefix("'sh' '-c' '")?.strip_suffix('\''));
        if let Some(script) = script.filter(|s| prog == "ssh" && s.contains("\\036")) {
            let mut out = String::new();
            for l in script.replace("'\\''", "'").lines() {
                let l = l.trim_end_matches(BATCH);
                // A herdr read there stands for `herdr --machine <host> …`.
                let o = match l.strip_prefix("PATH=\"$HOME/.local/bin:$PATH\" 'herdr' '") {
                    Some(h) => {
                        let m = &args[args.len() - 3];
                        let words = h.trim_end_matches('\'').split("' '").map(String::from);
                        self.run(
                            "herdr",
                            &[["--machine".into(), m.clone()].to_vec(), words.collect()].concat(),
                        )?
                    }
                    None => self.run(prog, &[&args[..args.len() - 1], &[l.to_string()]].concat())?,
                };
                out += &format!("{}\x1e{}\x1e", o.stdout, o.code);
            }
            return Ok(Out {
                code: 0,
                stdout: out,
                stderr: String::new(),
            });
        }
        let mut onces = self.onces.borrow_mut();
        if let Some(i) = onces.iter().position(|(p, _)| line.starts_with(p.as_str())) {
            return Ok(onces.remove(i).1);
        }
        for (p, o) in &self.replies {
            if line.starts_with(p.as_str()) {
                return Ok(o.clone());
            }
        }
        if line.starts_with("gh api graphql") {
            return Ok(open_prs(&mut onces, &self.replies));
        }
        Ok(Out {
            code: 1,
            stdout: String::new(),
            stderr: format!("unscripted: {line}"),
        })
    }
    fn sleep(&self, secs: u64) {
        self.calls.borrow_mut().push(format!("sleep {secs}"));
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> anyhow::Result<Out> {
        let line = format!("{prog} {}", args.join(" "));
        self.inputs.borrow_mut().push((line, input.into()));
        self.run(prog, args)
    }
    fn secret(&self, svc: &str) -> anyhow::Result<Option<String>> {
        anyhow::ensure!(!self.no_store, "no secret store (fake)");
        Ok(self.store.borrow().get(svc).cloned())
    }
    fn set_secret(&self, svc: &str, value: Option<&str>) -> anyhow::Result<()> {
        anyhow::ensure!(!self.no_store, "no secret store (fake)");
        let mut m = self.store.borrow_mut();
        let _ = match value {
            Some(v) => m.insert(svc.into(), v.into()),
            None => m.remove(svc),
        };
        Ok(())
    }
}

/// #246: the batched open-PR read, built from the `gh pr view <branch>` stubs (a `once` is spent, as one read was):
/// one JSON line per PR, `headRefName` the stub's branch.
fn open_prs(onces: &mut Vec<(String, Out)>, replies: &[(String, Out)]) -> Out {
    fn branch(p: &str) -> Option<&str> {
        p.strip_prefix("gh pr view ")
            .map(str::trim)
            .filter(|b| b.starts_with("issue-"))
    }
    let mut seen: Vec<(String, Out)> = vec![];
    onces.retain(|(p, o)| match branch(p) {
        Some(b) if !seen.iter().any(|s| s.0 == b) => {
            seen.push((b.into(), o.clone()));
            false
        }
        _ => true,
    });
    for (p, o) in replies {
        if let Some(b) = branch(p).filter(|b| !seen.iter().any(|s| s.0 == *b)) {
            seen.push((b.into(), o.clone()));
        }
    }
    let line = |(b, o): (String, Out)| {
        let mut v: serde_json::Value = serde_json::from_str(&o.stdout).ok().filter(|_| o.code == 0)?;
        v.as_object()?;
        v["headRefName"] = b.into();
        Some(format!("{v}\n"))
    };
    Out {
        stdout: seen.into_iter().filter_map(line).collect(),
        ..Out::default()
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

/// #535: the dirs `tmpdir` made on this thread; removed when it ends. libtest runs each test
/// on its own thread, so a test's dirs go when it ends, pass or fail.
struct Made(std::cell::RefCell<Vec<PathBuf>>);

impl Drop for Made {
    fn drop(&mut self) {
        for d in self.0.borrow().iter() {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

thread_local!(static MADE: Made = const { Made(std::cell::RefCell::new(Vec::new())) });

/// Every dir `tmpdir` made in this process, swept again at exit: a test's background
/// threads (a server's sampler, drive's workers) can write one back after its test ends.
static ALL: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

extern "C" fn sweep() {
    if let Ok(all) = ALL.try_lock() {
        for d in all.iter() {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

extern "C" {
    fn atexit(f: extern "C" fn()) -> std::ffi::c_int;
}

pub fn tmpdir(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let d = std::env::temp_dir().join(format!(
        "fleet-test-{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    MADE.with(|m| m.0.borrow_mut().push(d.clone()));
    static AT_EXIT: std::sync::Once = std::sync::Once::new();
    AT_EXIT.call_once(|| assert_eq!(unsafe { atexit(sweep) }, 0));
    ALL.lock().unwrap().push(d.clone());
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
/// assets (#57: the page links them; a same-origin base lets herd.js fetch horses.svg). #560: it runs under
/// scripts/ui-lock.sh, one WebKit audit at a time on the host.
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
    // #560: WebKit under the user's home, never a test's temporary `$HOME` (live_board's, deleted when it ends).
    if HELD.load(Ordering::SeqCst) {
        let mut c = std::process::Command::new("swift");
        c.env("HERDSMAN_BASE", base).env("HOME", user_home());
        return c;
    }
    let mut c = std::process::Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/ui-lock.sh"));
    c.arg("swift").env("HERDSMAN_BASE", base).env("HOME", user_home());
    c
}

/// The user's home from the password database, as scripts/ui-lock.sh finds it (a test may point `$HOME` elsewhere).
fn user_home() -> PathBuf {
    let sh = ["-c", "eval echo ~$(id -un)"];
    let home = std::process::Command::new("sh").args(sh).output().unwrap();
    PathBuf::from(String::from_utf8_lossy(&home.stdout).trim())
}

static HELD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// #560: scripts/ui-lock.sh's host lock held for a whole WebKit test whose steps are timed (a `/live` update 2.5 s
/// in), so its clock starts once WebKit is its own; `swift()` meanwhile runs without the wrapper. Every WebKit test
/// of a binary that takes it must take it.
pub struct UiLock(std::fs::File, std::sync::MutexGuard<'static, ()>);

pub fn ui_lock() -> UiLock {
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let one = ONE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let path =
        std::env::var_os("UI_AUDIT_LOCK").map_or(user_home().join(".cache/herdsman/ui-audit.lock"), PathBuf::from);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap();
    if f.try_lock().is_err() {
        let by = std::fs::read_to_string(&path).unwrap_or_default();
        eprintln!("waiting for the UI audit lock (held by {})", by.trim());
        f.lock().unwrap();
    }
    let me = std::env::var("HERDSMAN_LANE").unwrap_or_else(|_| format!("pid {}", std::process::id()));
    f.set_len(0).unwrap();
    std::io::Write::write_all(&mut f, format!("{me}\n").as_bytes()).unwrap();
    HELD.store(true, Ordering::SeqCst);
    UiLock(f, one)
}

impl Drop for UiLock {
    fn drop(&mut self) {
        HELD.store(false, Ordering::SeqCst);
    }
}

/// #59: pair a browser at `now` as the page does (Pair → `serve approve <code>` → claim): its `Cookie:` line.
pub fn pair(s: &herdsman::serve::Server, c: &Ctx, ua: &str, now: u64) -> String {
    let post = |path: &str, extra: &str| {
        String::from_utf8_lossy(&s.respond(
            &format!("POST {path} HTTP/1.1\r\nHost: 127.0.0.1:7777\r\n{extra}Content-Length: 0\r\n\r\n"),
            now,
        ))
        .into_owned()
    };
    let res = post("/pair/request", &format!("User-Agent: {ua}\r\n"));
    assert!(res.starts_with("HTTP/1.1 200 OK"), "{res}");
    let code = res.rsplit("\r\n\r\n").next().unwrap().trim().to_string();
    let set = res.lines().find_map(|l| l.strip_prefix("Set-Cookie: ")).unwrap();
    let cookie = format!("Cookie: {}\r\n", &set[..set.find(';').unwrap()]);
    let (rc, out) = run(
        &Ctx { now, ..c.clone() },
        &Fake::default(),
        &["serve", "approve", &code],
    );
    assert_eq!(rc, 0, "{out}");
    let res = post("/pair/claim", &cookie);
    assert!(
        res.starts_with("HTTP/1.1 200 OK") && res.contains("Max-Age=31536000"),
        "{res}"
    );
    cookie
}

/// #138 gate 2: a sheet with every block type: you, assistant prose with markdown, a status line, a passing and a
/// failing tool run (capped, with its tail), a diff with long lines, the gap line, a running call and the live line.
pub fn chat_fixture() -> serde_json::Value {
    let g = |s: &str| format!("\x1b[38;2;153;153;153m{s}\x1b[0m");
    let mut l = vec![
        "❯ Read ~/.herdr/worktrees/acme/issue-7/.brief.md and execute it fully.".to_string(),
        g("  Read 1 file, ran 3 shell commands"),
        "⏺ I read the brief. The overlap check skips **merged** issues only, so a closed one still counts.".into(),
        "⏺ Bash(cargo test --locked)".into(),
        "  ⎿  test result: \x1b[38;2;80;200;80mok\x1b[0m. 218 passed; 0 failed".into(),
        herdsman::lanes::GAP.into(),
        "⏺ # Plan".into(),
        "  Base is green. The bug is in `src/overlap.rs`: the list keeps every issue that is not **merged**.".into(),
        "  1. Swap `s.state != \"merged\"` for `!s.done()`, which the board already uses everywhere.".into(),
        "  2. Run the gates: `cargo test`, `cargo fmt --check`, `cargo clippy --all-targets --locked`.".into(),
        "  - the merge path and its refusal codes stay as they are".into(),
        "⏺ Update(src/overlap.rs)".into(),
        "  ⎿  Updated src/overlap.rs with 1 addition and 1 removal".into(),
        "      49      git(r, &dir, &[\"fetch\", \"-q\", \"origin\"])?;".into(),
        "      52 \x1b[48;2;61;1;0m-        .filter(|s| s.fleet == fleet && \x1b[48;2;92;2;0ms.state != \"merged\"\x1b[48;2;61;1;0m && s.issue != 0 && s.lane.is_some())\x1b[0m".into(),
        "      52 \x1b[48;2;2;40;0m+        .filter(|s| s.fleet == fleet && \x1b[48;2;4;71;0m!s.done()\x1b[48;2;2;40;0m && s.issue != 0 && s.lane.is_some())\x1b[0m".into(),
        "\x1b[38;2;255;107;128m⏺\x1b[0m Bash(cargo test --locked overlap -- --nocapture --test-threads=1 2>&1 | tee /tmp/a-very-long-log-name.log)".into(),
        "  ⎿  Error: Exit code 101".into(),
    ];
    l.extend((0..26).map(|i| format!("     test overlap::tests::case_{i} ... \x1b[38;2;80;200;80mok\x1b[0m")));
    l.push("     thread 'overlap::tests::skips_closed' panicked at src/overlap.rs:214:9: assertion `left == right` failed: a closed issue's branch is not in flight".into());
    l.push("     test result: \x1b[38;2;220;90;90mFAILED\x1b[0m. 26 passed; 1 failed".into());
    l.push(g("  Ran 2 shell commands"));
    l.push(
        "⏺ Bash(L=~/scratchpad; cargo build --release --locked >$L/build.log 2>&1; echo build=$?; cargo fmt --check)"
            .into(),
    );
    serde_json::json!({"harness": "claude", "acts": ["stop", "kill", "clean"], "lines": l, "live": 1,
        "status": "✽ Gusting… (2m 47s · ↓ 1.8k tokens)", "before": l.len(), "more": true})
}

/// #138: page JS that opens sendmeter-impl-12's sheet full and draws `chat_fixture()` at once (the poll answers the
/// same) in the full view (#315: not the one WebKit's stored `chat:full` says), every block open with [show all]
/// pressed, or all collapsed.
pub fn chat_scene(open: bool) -> String {
    let fx = chat_fixture();
    format!(
        "window.fetch = async u => u.includes('lane') ? new Response({j}) : new Response('', {{status: 404}}); \
         open(document.querySelector('.lane[data-agent=\"sendmeter-impl-12\"]')); snap('full'); \
         full = true; hist = {l}.slice(0, -1); tail = {l}.slice(-1); harness = 'claude'; spin = true; paint(); \
         $('.live').hidden = false; $('.live').innerHTML = '<b>✽</b> Gusting… (2m 47s · ↓ 1.8k tokens)'; \
         document.querySelectorAll('.blk').forEach(d => d.open = {open}); \
         if ({open}) document.querySelectorAll('.more2 button').forEach(b => b.click())",
        j = serde_json::to_string(&fx.to_string()).unwrap(),
        l = fx["lines"]
    )
}

/// #266: the Events tab as the TypeScript board draws it: web/src/events.ts under node, from `s`'s `/api/v1/state` and
/// `/api/v1/events` at `now`; `opts` (a JS object) overrides the draw's `all`, `shown` or `f`. None where node is missing.
pub fn events_tab(s: &herdsman::serve::Server, host: &str, now: u64, opts: &str) -> Option<String> {
    if std::process::Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipping: no node here");
        return None;
    }
    let get = |p: &str| {
        let r = s.respond(&format!("GET {p} HTTP/1.1\r\nHost: {host}\r\n\r\n"), now);
        let r = String::from_utf8_lossy(&r).into_owned();
        assert!(r.starts_with("HTTP/1.1 200"), "{p}: {r}");
        r[r.find("\r\n\r\n").unwrap() + 4..].to_string()
    };
    let d = tmpdir("events-ts");
    std::fs::write(d.join("state.json"), get("/api/v1/state")).unwrap();
    std::fs::write(d.join("events.json"), get("/api/v1/events?since=0")).unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/events.ts");
    let js = format!(
        "import {{ tab }} from {src:?}; import fs from 'node:fs';\n\
         const st = JSON.parse(fs.readFileSync({:?})), ev = JSON.parse(fs.readFileSync({:?}));\n\
         process.stdout.write(tab(ev.list, {{ now: st.now, tz: st.tz, fleets: st.fleets, all: false, \
         shown: {{ imp: 100, all: 100 }}, f: {{ k: 'imp', fl: [] }}, ...{opts} }}));",
        d.join("state.json"),
        d.join("events.json")
    );
    std::fs::write(d.join("t.mjs"), js).unwrap();
    let o = std::process::Command::new("node")
        .arg(d.join("t.mjs"))
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    Some(String::from_utf8(o.stdout).unwrap())
}

/// #267: the Host tab as the TypeScript board draws it: web/src/host.ts under node, from `s`'s `/api/v1/host` at `now`
/// (`host` the request's Host: a read-only server takes an empty one).
pub fn host_tab(s: &herdsman::serve::Server, host: &str, now: u64) -> String {
    let r = s.respond(&format!("GET /api/v1/host HTTP/1.1\r\nHost: {host}\r\n\r\n"), now);
    let r = String::from_utf8_lossy(&r).into_owned();
    assert!(r.starts_with("HTTP/1.1 200"), "/api/v1/host: {r}");
    host_html(&r[r.find("\r\n\r\n").unwrap() + 4..])
}

/// #267: web/src/host.ts's drawing of API v1 `host` data `json`, under node; #440: then what System draws from the same
/// data (web/src/system.ts): Agent providers and Notifications & devices.
pub fn host_html(json: &str) -> String {
    let d = tmpdir("host-ts");
    std::fs::write(d.join("host.json"), json).unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/host.ts");
    let sys = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/system.ts");
    let js = format!(
        "import {{ tab }} from {src:?}; import {{ providers, notifications }} from {sys:?}; import fs from 'node:fs';\n\
         const d = JSON.parse(fs.readFileSync({:?})); process.stdout.write(tab(d) + providers(d) + notifications(d));",
        d.join("host.json")
    );
    std::fs::write(d.join("t.mjs"), js).unwrap();
    let o = std::process::Command::new("node")
        .arg(d.join("t.mjs"))
        .output()
        .expect("node (24+) runs web/src/host.ts");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

/// #268: `expr` (JS, over API v1 `queue` data `d`: `tab(d)`, `sheet(d, fleet)`) from web/src/queue.ts under node, from
/// `s`'s `/api/v1/queue` at `now` (`host` the request's Host: a read-only server takes an empty one).
pub fn queue_js(s: &herdsman::serve::Server, host: &str, now: u64, expr: &str) -> String {
    let r = s.respond(&format!("GET /api/v1/queue HTTP/1.1\r\nHost: {host}\r\n\r\n"), now);
    let r = String::from_utf8_lossy(&r).into_owned();
    assert!(r.starts_with("HTTP/1.1 200"), "/api/v1/queue: {r}");
    queue_html(&r[r.find("\r\n\r\n").unwrap() + 4..], expr)
}

/// #268: the Queue tab as the TypeScript board draws it (`queue_js`'s `tab(d)`).
pub fn queue_tab(s: &herdsman::serve::Server, host: &str, now: u64) -> String {
    queue_js(s, host, now, "tab(d)")
}

/// #268: `expr` over API v1 `queue` data `json` in web/src/queue.ts, under node.
pub fn queue_html(json: &str, expr: &str) -> String {
    let d = tmpdir("queue-ts");
    std::fs::write(d.join("queue.json"), json).unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/queue.ts");
    let js = format!(
        "import {{ tab, sheet }} from {src:?}; import fs from 'node:fs';\n\
         const d = JSON.parse(fs.readFileSync({:?})); process.stdout.write(String({expr}));",
        d.join("queue.json")
    );
    std::fs::write(d.join("t.mjs"), js).unwrap();
    let o = std::process::Command::new("node")
        .arg(d.join("t.mjs"))
        .output()
        .expect("node (24+) runs web/src/queue.ts");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

/// #440: a `<script>` painting what the TypeScript board draws into the System sheet (main.ts), for a page cut before
/// board.js: Hosts & health from `s`'s `/api/v1/host`, Agent providers and Notifications from the same, Fleets and
/// Conductor from `/api/v1/queue`, Activity from its events (`events`, `events_tab`'s HTML).
pub fn system_frags(s: &herdsman::serve::Server, host: &str, now: u64, events: &str) -> String {
    let get = |p: &str| {
        let r =
            String::from_utf8_lossy(&s.respond(&format!("GET {p} HTTP/1.1\r\nHost: {host}\r\n\r\n"), now)).into_owned();
        assert!(r.starts_with("HTTP/1.1 200"), "{p}: {r}");
        r[r.find("\r\n\r\n").unwrap() + 4..].to_string()
    };
    let d = tmpdir("system-ts");
    std::fs::write(d.join("host.json"), get("/api/v1/host")).unwrap();
    std::fs::write(d.join("queue.json"), get("/api/v1/queue")).unwrap();
    std::fs::write(d.join("events.html"), events).unwrap();
    let src = |f: &str| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src").join(f);
    let js = format!(
        "import {{ tab }} from {:?}; import * as y from {:?}; import fs from 'node:fs';\n\
         const h = JSON.parse(fs.readFileSync({:?})), q = JSON.parse(fs.readFileSync({:?}));\n\
         process.stdout.write(JSON.stringify({{ host: tab(h), 'sy-providers': y.providers(h), 'sy-notifications': y.notifications(h), \
         'sy-fleets': y.fleets(q), 'sy-conductor': y.conductor(q), events: fs.readFileSync({:?}, 'utf8') }}));",
        src("host.ts"),
        src("system.ts"),
        d.join("host.json"),
        d.join("queue.json"),
        d.join("events.html")
    );
    std::fs::write(d.join("t.mjs"), js).unwrap();
    let o = std::process::Command::new("node")
        .arg(d.join("t.mjs"))
        .output()
        .expect("node (24+) runs web/src/system.ts");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let parts = String::from_utf8(o.stdout).unwrap().replace("</", "<\\/");
    format!("<script>{{ const p = {parts}; for (const id in p) frag(id, p[id]); }}</script>")
}

/// #268: a page response `res` with its Queue tab as the board draws it (`queue_tab`) in `<section id=queue>` (`host`
/// the request's Host, else an empty one: a read-only server's); #269: and every other part the board draws
/// (`with_board`, for an unpaired request).
pub fn with_queue(res: &str, s: &herdsman::serve::Server, host: &str, now: u64) -> String {
    with_board(res, s, host, "", now)
}

/// #269: `exprs` (JS over `d`, `lanes`, `needs` and `chat`: web/src/{lanes,needs,chat}.ts's exports) each over API v1
/// data `json` (the first `d`, …), under node: their strings.
pub fn board_js(parts: &[(&str, &str)]) -> Vec<String> {
    let d = tmpdir("board-ts");
    let src = |f: &str| Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../web/src/{f}.ts"));
    let mut js = format!(
        "import * as lanes from {:?}; import * as needs from {:?}; import * as chat from {:?}; import fs from 'node:fs';\nconst out = [];\n",
        src("lanes"),
        src("needs"),
        src("chat")
    );
    for (i, (json, expr)) in parts.iter().enumerate() {
        std::fs::write(d.join(format!("{i}.json")), json).unwrap();
        js += &format!(
            "{{ const d = JSON.parse(fs.readFileSync({:?})); out.push(String({expr})); }}\n",
            d.join(format!("{i}.json"))
        );
    }
    js += "process.stdout.write(JSON.stringify(out));";
    std::fs::write(d.join("t.mjs"), js).unwrap();
    let o = std::process::Command::new("node")
        .arg(d.join("t.mjs"))
        .output()
        .expect("node (24+) runs web/src/*.ts");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

/// #269: API v1 `path` from `s` for a request with Host `host` (none taken: an empty one, a read-only server's) and
/// `extra` header lines (a `Cookie: …\r\n`) at `now`: the body.
pub fn api(s: &herdsman::serve::Server, host: &str, extra: &str, now: u64, path: &str) -> String {
    let req = |h: &str| format!("GET /api/v1/{path} HTTP/1.1\r\nHost: {h}\r\n{extra}\r\n");
    let mut r = String::from_utf8_lossy(&s.respond(&req(host), now)).into_owned();
    if !r.starts_with("HTTP/1.1 200") {
        r = String::from_utf8_lossy(&s.respond(&req(""), now)).into_owned();
    }
    assert!(r.starts_with("HTTP/1.1 200"), "/api/v1/{path}: {r}");
    r[r.find("\r\n\r\n").unwrap() + 4..].to_string()
}

/// #269: the needs cards, the Lanes tab and the [chat] tab as the board draws them for that request (`api`'s).
pub fn parts(s: &herdsman::serve::Server, host: &str, extra: &str, now: u64) -> [String; 3] {
    let [n, l, c] = ["needs", "lanes", "chat"].map(|p| api(s, host, extra, now, p));
    let v = board_js(&[(&n, "needs.tab(d)"), (&l, "lanes.tab(d)"), (&c, "chat.tab(d)")]);
    [v[0].clone(), v[1].clone(), v[2].clone()]
}

/// #269: what `/frags?tab=` sent before the board drew them: `needs` and the tab's part (`lanes`, `chat`) by id.
pub fn frags(s: &herdsman::serve::Server, host: &str, extra: &str, now: u64) -> serde_json::Value {
    let [needs, lanes, chat] = parts(s, host, extra, now);
    serde_json::json!({ "needs": needs, "lanes": lanes, "chat": chat })
}

/// #269: a page response `res` with every tab the board draws in it (the Queue tab's as `with_queue`), as the page shows
/// them once drawn, for the request with Host `host` and `extra` header lines.
pub fn with_board(res: &str, s: &herdsman::serve::Server, host: &str, extra: &str, now: u64) -> String {
    let req = |h: &str| format!("GET /api/v1/queue HTTP/1.1\r\nHost: {h}\r\n\r\n");
    let ok = String::from_utf8_lossy(&s.respond(&req(host), now)).starts_with("HTTP/1.1 200");
    let q = queue_tab(s, if ok { host } else { "" }, now);
    let [needs, lanes, chat] = parts(s, host, extra, now);
    res.replacen(
        "<section id=queue></section>",
        &format!("<section id=queue>{q}</section>"),
        1,
    )
    .replacen("<div id=needs></div>", &format!("<div id=needs>{needs}</div>"), 1)
    .replacen(
        "<section id=lanes></section>",
        &format!("<section id=lanes>{lanes}</section>"),
        1,
    )
    .replacen(
        "<section id=chat></section>",
        &format!("<section id=chat>{chat}</section>"),
        1,
    )
}

/// #269: the Lanes row's avatar (web/src/lanes.ts's `avatar`) for each (name, role, herdr state), under node.
pub fn avatars(of: &[(&str, &str, &str)]) -> Vec<String> {
    use herdsman::serve::{act, coat, tone};
    let j: Vec<String> = of
        .iter()
        .map(|(n, r, s)| {
            serde_json::json!({"state": s, "act": act(r, s), "atone": tone(s), "coat": coat(n)}).to_string()
        })
        .collect();
    board_js(&j.iter().map(|j| (j.as_str(), "lanes.avatar(d)")).collect::<Vec<_>>())
}

/// #269: a page response `res` as the board leaves it drawn (`with_board`), for a static copy (the WebKit tests' page
/// file): without the board's module, which would read the API of the server the copy's assets come from.
pub fn drawn(res: &str, s: &herdsman::serve::Server, host: &str, extra: &str, now: u64) -> String {
    let page = with_board(res, s, host, extra, now);
    let at = page.find("<script type=module").expect("the board's module");
    let end = at + page[at..].find("</script>").unwrap() + 9;
    format!("{}{}", &page[..at], &page[end..])
}

/// #344: replace the whole event log with JSONL `text` (as a test edits a log): the database goes, the next read
/// imports `text`. Leases, devices and snapshots go with it.
pub fn rewrite_log(state: &Path, text: &str) {
    for f in ["state.db", "state.db-wal", "state.db-shm"] {
        let _ = std::fs::remove_file(state.with_file_name(f));
    }
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    std::fs::write(state, text).unwrap();
}

/// #344: lease `key` as process `pid` would hold it: taken at `at` for `secs`.
pub fn lease_as(c: &Ctx, key: &str, pid: u32, at: u64, secs: u64) {
    let l = herdsman::db::take(&c.state, key, at, secs, "", &|_| true).unwrap();
    assert!(herdsman::db::renew(
        &c.state,
        &l,
        &herdsman::db::Lease { pid, ..l.clone() }
    ));
}

/// #344: the lease on `key`, expired or not.
pub fn lease_on(c: &Ctx, key: &str) -> Option<herdsman::db::Lease> {
    herdsman::db::leases(&c.state, key).into_iter().find(|l| l.key == key)
}

/// #441: `.0`, except that a `pane run <pane> export …` line also runs in a real `sh`, as the pane's shell would
/// (spawn's secrets file exists only meanwhile); `.2` gets each run's `$<.1>`.
pub struct Shell<'a>(pub &'a Fake, pub &'static str, pub Shared<Vec<String>>);

impl Runner for Shell<'_> {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        if let Some(i) = args.iter().position(|a| a == "export").filter(|_| prog == "herdr") {
            let line = format!("{}; printf %s \"${}\"", args[i..].join(" "), self.1);
            let o = std::process::Command::new("sh").arg("-c").arg(line).output()?;
            self.2.borrow_mut().push(String::from_utf8_lossy(&o.stdout).into());
        }
        self.0.run(prog, args)
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> anyhow::Result<Out> {
        self.0.run_in(prog, args, input)
    }
    fn sleep(&self, secs: u64) {
        self.0.sleep(secs);
    }
    fn secret(&self, svc: &str) -> anyhow::Result<Option<String>> {
        self.0.secret(svc)
    }
    fn set_secret(&self, svc: &str, value: Option<&str>) -> anyhow::Result<()> {
        self.0.set_secret(svc, value)
    }
}

pub const IMPL: &str = "sendmeter-impl-12";
pub const REV: &str = "sendmeter-rev-9-r1";
const V1_WS: &str = r#"{"result": {"root_pane": {"pane_id": "w9:p1"}}}"#;
pub const V1_PERMISSION: &str = "╭──────────────────────────────────────────╮\n│ Bash command                             │\n│   cargo test --all                       │\n│ Do you want to proceed?                  │\n│ ❯ 1. Yes                                 │\n│   2. Yes, and don't ask again for cargo  │\n│      commands in /p/sendmeter            │\n│   3. No, and tell Claude what to do      │\n│      differently (esc)                   │\n╰──────────────────────────────────────────╯\n";

/// #555: API v1's board: sendmeter: #12's impl lane working on w1:p2, #9's rev lane blocked on a permission menu on w1:p3, a setup
/// terminal `claude B` on host bazzite; gh and herdr's writes succeed.
pub fn v1_board(name: &str) -> (herdsman::Ctx, Fake) {
    let cfg = format!("models = [\"opus\", \"sonnet\"]\n{CONFIG}\n[hosts.bazzite]\nmachine = \"bazzite\"\n\n[[setup]]\nname = \"claude B\"\ncmd = \"claude /login\"\n");
    let c = ctx(&tmpdir(name), &cfg);
    for (issue, role, agent, pane) in [(12, "impl", IMPL, "w1:p2"), (9, "rev", REV, "w1:p3")] {
        let d = serde_json::json!({"role": role, "agent": agent, "harness": "claude", "pane": pane});
        herdsman::events::append(
            &c.state,
            &herdsman::events::Event::new(1_000_000 - 100, "sendmeter", Some(issue), "spawn", d),
        )
        .unwrap();
    }
    let list = agents(&[(IMPL, "working", "/x", "w1:p2"), (REV, "blocked", "/x", "w1:p3")]);
    let r = Fake::default()
        .reply("herdr agent list", &list)
        .reply("herdr pane read", V1_PERMISSION)
        .reply("herdr --machine bazzite pane read", "Paste code here >\n")
        .reply("herdr", V1_WS)
        .reply("gh issue close", "");
    (c, r)
}
