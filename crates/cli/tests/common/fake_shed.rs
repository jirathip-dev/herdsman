use super::{Fake, Shared};
use drover::runner::{Out, Runner};
use drover::shed::proto::{ErrorCode, Hello, Request, Response, RpcError, VERSION};
use drover::shed::{Shed, Wire};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// The server's state, shared with its connection threads.
#[derive(Default)]
pub struct State {
    /// Every request but `hello`: (method, params).
    pub seen: Vec<(String, Value)>,
    /// Its terminals by name, as `get` answers them.
    pub terms: BTreeMap<String, Value>,
    /// The kept events `events` streams (those after its `since`); a test adds more as its lanes exit.
    pub events: Vec<Value>,
    /// A `wait`'s outcome per terminal (default `matched`).
    pub waits: BTreeMap<String, String>,
    /// What it answers `hello` with.
    pub hello: Option<Hello>,
}

pub struct FakeShed {
    pub sock: PathBuf,
    pub state: Arc<Mutex<State>>,
}

impl Drop for FakeShed {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.sock);
    }
}

impl FakeShed {
    /// A server at drover's shed version, no terminals and no events.
    pub fn new() -> FakeShed {
        FakeShed::start(VERSION, vec![])
    }
    /// A server answering `hello` with shed `version`, its kept `events`.
    pub fn start(version: &str, events: Vec<Value>) -> FakeShed {
        static N: AtomicU32 = AtomicU32::new(0);
        let sock = PathBuf::from(format!(
            "/tmp/hsb-{}-{}.sock",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&sock);
        let l = UnixListener::bind(&sock).unwrap();
        let state = Arc::new(Mutex::new(State {
            events,
            hello: Some(Hello {
                version: version.into(),
                ..Hello::ours()
            }),
            ..State::default()
        }));
        let s = state.clone();
        std::thread::spawn(move || {
            for c in l.incoming().flatten() {
                let s = s.clone();
                std::thread::spawn(move || conn(c, &s));
            }
        });
        FakeShed { sock, state }
    }
    /// The runtime on this server, as `[runtime] kind = "shed"` makes it.
    pub fn shed(&self) -> Shed {
        Shed {
            wire: Wire::Socket(self.sock.clone()),
            acp: None,
            policy: "ask".into(),
        }
    }
    /// Runner `r` with its lanes on this server, as the CLI and `serve` wrap theirs (for a caller that skips them).
    pub fn on<'a>(&self, r: &'a dyn Runner) -> drover::shed::With<'a> {
        drover::shed::With(r, self.shed())
    }
    /// Fleet config `base` with its lanes on this server (`[runtime] kind = "shed"`).
    pub fn config(&self, base: &str) -> String {
        format!(
            "{base}\n[runtime]\nkind = \"shed\"\nsocket = \"{}\"\n",
            self.sock.display()
        )
    }
    pub fn attach(&self, c: &drover::Ctx) {
        let base = std::fs::read_to_string(&c.config).unwrap();
        let base = base.split("\n[runtime]\nkind = \"shed\"\n").next().unwrap_or_default();
        std::fs::write(&c.config, self.config(base)).unwrap();
    }
    /// A new server, ctx `c`'s lanes on it (`attach`).
    pub fn attached(c: &drover::Ctx) -> FakeShed {
        let sh = FakeShed::new();
        sh.attach(c);
        sh
    }
    pub fn path_in(dir: &std::path::Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("shed-bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("shed"), "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(bin.join("shed"), std::fs::Permissions::from_mode(0o755)).unwrap();
        format!("{}:/usr/bin:/bin", bin.display())
    }
    pub fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }
    /// Each request's method, in order.
    pub fn methods(&self) -> Vec<String> {
        self.lock().seen.iter().map(|s| s.0.clone()).collect()
    }
    /// Every `method` request's params, in order.
    pub fn all(&self, method: &str) -> Vec<Value> {
        let st = self.lock();
        st.seen.iter().filter(|s| s.0 == method).map(|s| s.1.clone()).collect()
    }
    /// The last `method` request's params (null: none).
    pub fn last(&self, method: &str) -> Value {
        self.all(method).pop().unwrap_or_default()
    }
    /// The text each `send` typed into terminal `name`, in order.
    pub fn sent(&self, name: &str) -> Vec<String> {
        let all = self.all("send").into_iter().filter(|p| p["name"] == name);
        all.map(|p| p["text"].as_str().unwrap_or_default().to_string())
            .collect()
    }
    /// Whether a `method` request named terminal `name`.
    pub fn called(&self, method: &str, name: &str) -> bool {
        self.all(method).iter().any(|p| p["name"] == name)
    }
    /// A running terminal `name` with `labels`, its reported state `working`, in `/w/issue-12`.
    pub fn add(&self, name: &str, labels: Value) {
        let t = json!({"name": name, "labels": labels, "cwd": "/w/issue-12", "cmd": ["sh"], "keep": true,
            "state": "running", "exit": null, "pid": 7, "reported": {"state": "working"}});
        self.lock().terms.insert(name.into(), t);
    }
    pub fn lane(&self, name: &str, status: &str, cwd: &str) {
        let mut w: Vec<&str> = name.split('-').collect();
        if w.len() > 3 && w[w.len() - 1].starts_with('r') {
            w.pop();
        }
        let (issue, role) = (w.pop().unwrap_or_default(), w.pop().unwrap_or_default());
        let labels = json!({"fleet": w.join("-"), "issue": issue, "role": role, "harness": "claude"});
        self.add(name, labels);
        self.set(name, |t| {
            t["cwd"] = json!(cwd);
            t["reported"] = json!({"state": status});
        });
    }
    /// Change terminal `name` (which must exist).
    pub fn set(&self, name: &str, f: impl FnOnce(&mut Value)) {
        f(self.lock().terms.get_mut(name).expect("no such terminal"));
    }
    /// Terminal `name`'s screen, as `read` answers it (default `screen of <name>`).
    pub fn screen(&self, name: &str, text: &str) {
        self.set(name, |t| t["text"] = json!(text));
    }
    /// Terminal `name` exited (`ls` no longer lists it as a lane; `get` still finds it).
    pub fn exit(&self, name: &str) {
        self.set(name, |t| t["state"] = json!("exited"));
    }
    /// A `wait` on terminal `name` ends `outcome` (`timeout`, `exited`) instead of `matched`.
    pub fn wait_ends(&self, name: &str, outcome: &str) {
        self.lock().waits.insert(name.into(), outcome.into());
    }
    /// Terminal `name` is gone (`NO_SUCH_TERMINAL` to every call).
    pub fn remove(&self, name: &str) {
        self.lock().terms.remove(name);
    }
    /// Whether terminal `name` exists.
    pub fn has(&self, name: &str) -> bool {
        self.lock().terms.contains_key(name)
    }
    /// The answer lines `shed connect` prints for its stdin's request lines.
    pub fn connect(&self, input: &str) -> String {
        let lines = input
            .lines()
            .map(|l| answer(&self.state, &serde_json::from_str(l).unwrap()));
        lines.map(|l| l + "\n").collect()
    }
}

fn answer(st: &Mutex<State>, q: &Request) -> String {
    let mut st = st.lock().unwrap();
    if q.method != "hello" {
        st.seen.push((q.method.clone(), q.params.clone()));
    }
    let res = match q.method.as_str() {
        "hello" => Ok(json!(st.hello)),
        m => handle(m, &q.params, &mut st),
    };
    let id = q.id.clone().unwrap();
    let resp = res.map_or_else(|e| Response::err(id.clone(), e), |v| Response::ok(id.clone(), v));
    serde_json::to_string(&resp).unwrap()
}

fn conn(c: UnixStream, st: &Mutex<State>) {
    let mut w = c.try_clone().unwrap();
    for line in BufReader::new(c).lines().map_while(Result::ok) {
        let q: Request = serde_json::from_str(&line).unwrap();
        writeln!(w, "{}", answer(st, &q)).unwrap();
        if q.method == "events" {
            let since = q.params["since"].as_u64().unwrap_or(0);
            let new: Vec<Value> = st.lock().unwrap().events.clone();
            let new = new.into_iter().filter(|e| e["seq"].as_u64() > Some(since));
            new.for_each(|e| drop(writeln!(w, "{e}")));
            std::thread::sleep(std::time::Duration::from_secs(3));
            return;
        }
    }
}

fn handle(m: &str, p: &Value, st: &mut State) -> Result<Value, RpcError> {
    let name = p["name"].as_str().unwrap_or_default().to_string();
    let gone = || RpcError::new(ErrorCode::NoSuchTerminal, &name);
    let terms = &mut st.terms;
    match m {
        "run" => {
            let t = json!({"name": name, "labels": p["labels"], "cwd": p["cwd"], "cmd": p["cmd"], "keep": p["keep"],
                "state": "running", "exit": null, "pid": 9});
            terms.insert(name, t.clone());
            Ok(t)
        }
        "ls" => Ok(json!(terms.values().collect::<Vec<_>>())),
        "events" => Ok(json!({})),
        _ if !terms.contains_key(&name) => Err(gone()),
        "get" => Ok(terms[&name].clone()),
        "read" if p["events"] == true && terms[&name]["acp"].is_string() => {
            let before = p["before"].as_u64().unwrap_or(u64::MAX);
            let all: Vec<&Value> = terms[&name]["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["seq"].as_u64() < Some(before))
                .collect();
            let n = p["n"].as_u64().map_or(all.len(), |n| n as usize);
            Ok(json!({"text": "", "events": all[all.len().saturating_sub(n)..]}))
        }
        "read" => Ok(json!({"text": terms[&name]["text"].as_str().map_or(format!("screen of {name}"), String::from)})),
        "send" | "keys" => Ok(json!({})),
        "wait" => {
            let outcome = st.waits.get(&name).map_or("matched", String::as_str);
            Ok(json!({"outcome": outcome, "terminal": terms[&name]}))
        }
        "kill" => {
            terms.get_mut(&name).unwrap()["state"] = json!("exited");
            Ok(terms[&name].clone())
        }
        "rm" => Ok(terms.remove(&name).unwrap()),
        "state" => {
            terms.get_mut(&name).unwrap()["reported"] = json!({"state": p["state"]});
            Ok(terms[&name].clone())
        }
        _ => Err(RpcError::new(ErrorCode::MethodNotFound, m)),
    }
}

pub struct OverSsh<'a> {
    pub f: &'a Fake,
    pub hosts: Vec<(&'a str, &'a FakeShed)>,
}

impl OverSsh<'_> {
    fn batch(&self, prog: &str, args: &[String]) -> Option<anyhow::Result<Out>> {
        let script = args.last()?.strip_prefix("'sh' '-c' '")?.strip_suffix('\'')?;
        let host = self.hosts.iter().find(|(h, _)| args.iter().any(|a| a == h))?;
        let script = script.replace("'\\''", "'");
        if prog != "ssh" || !script.contains("'shed' 'connect'") {
            return None;
        }
        self.f.calls.borrow_mut().push(format!("{prog} {}", args.join(" ")));
        let mut out = String::new();
        for l in script.lines() {
            let l = l.trim_end_matches(super::BATCH);
            let words = l
                .strip_prefix("('printf' '%s\\n' '")
                .and_then(|w| w.strip_suffix("' | 'shed' 'connect' '--autostart')"));
            let o = match words {
                Some(w) => {
                    let input: String = w.split("' '").map(|x| format!("{x}\n")).collect();
                    self.f.inputs.borrow_mut().push((l.into(), input.clone()));
                    Out {
                        stdout: host.1.connect(&input),
                        ..Out::default()
                    }
                }
                None => match self.f.run(prog, &[&args[..args.len() - 1], &[l.to_string()]].concat()) {
                    Ok(o) => o,
                    Err(e) => return Some(Err(e)),
                },
            };
            out += &format!("{}\x1e{}\x1e", o.stdout, o.code);
        }
        Some(Ok(Out {
            stdout: out,
            ..Out::default()
        }))
    }
}

impl Runner for OverSsh<'_> {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        self.batch(prog, args).unwrap_or_else(|| self.f.run(prog, args))
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> anyhow::Result<Out> {
        if let Some(o) = self.batch(prog, args).filter(|_| input.is_empty()) {
            return o;
        }
        let line = format!("{prog} {}", args.join(" "));
        let host = self
            .hosts
            .iter()
            .find(|(h, _)| line.contains(&format!("{h} -- 'shed' 'connect'")));
        match host {
            Some((_, s)) => {
                self.f.inputs.borrow_mut().push((line.clone(), input.into()));
                self.f.calls.borrow_mut().push(line);
                Ok(Out {
                    stdout: s.connect(input),
                    ..Out::default()
                })
            }
            None => self.f.run_in(prog, args, input),
        }
    }
    fn sleep(&self, secs: u64) {
        self.f.sleep(secs);
    }
}

pub struct Cli<'a> {
    pub f: &'a Fake,
    pub state: Arc<Mutex<State>>,
    pub out: Shared<Vec<String>>,
}

impl FakeShed {
    /// shed's CLI on this server, around `f` (`Cli`).
    pub fn cli<'a>(&self, f: &'a Fake) -> Cli<'a> {
        Cli {
            f,
            state: self.state.clone(),
            out: Shared::default(),
        }
    }
}

impl Runner for Cli<'_> {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        if prog == "shed" && args[0] == "run" {
            let at = |k: &str| &args[args.iter().position(|a| a == k).unwrap() + 1];
            let name = at("--name");
            let ls = args.iter().zip(&args[1..]).filter(|(k, _)| *k == "--label");
            let labels: serde_json::Map<String, Value> = ls
                .filter_map(|(_, kv)| kv.split_once('='))
                .map(|(k, v)| (k.to_string(), json!(v)))
                .collect();
            let t = json!({"name": name, "labels": labels, "cwd": "/w", "cmd": ["shed"], "keep": false,
                "state": "running", "exit": null, "pid": 3, "acp": "claude-acp"});
            self.state.lock().unwrap().terms.insert(name.clone(), t);
            if let Some(i) = args.iter().position(|a| a == "--") {
                let o = std::process::Command::new(&args[i + 1])
                    .current_dir(at("--cwd"))
                    .args(&args[i + 2..])
                    .env_clear()
                    .env("PATH", std::env::var("PATH").unwrap())
                    .env("SHED", name)
                    .output()
                    .unwrap();
                self.out.borrow_mut().push(String::from_utf8(o.stdout).unwrap());
            }
        }
        self.f.run(prog, args)
    }
    fn sleep(&self, secs: u64) {
        self.f.sleep(secs);
    }
}

pub struct OnShed(pub Fake, pub FakeShed);

impl OnShed {
    pub fn new(f: Fake) -> OnShed {
        OnShed(f, FakeShed::new())
    }
}

impl std::ops::Deref for OnShed {
    type Target = Fake;
    fn deref(&self) -> &Fake {
        &self.0
    }
}

impl Runner for OnShed {
    fn run(&self, prog: &str, args: &[String]) -> anyhow::Result<Out> {
        self.0.run(prog, args)
    }
    fn run_in(&self, prog: &str, args: &[String], input: &str) -> anyhow::Result<Out> {
        self.0.run_in(prog, args, input)
    }
    fn sleep(&self, secs: u64) {
        self.0.sleep(secs);
    }
    fn shed(&self) -> Option<Shed> {
        Some(self.1.shed())
    }
}
