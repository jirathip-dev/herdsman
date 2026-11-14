use crate::runner::{argv, Out, Runner};
use crate::shed;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use shed_proto::{KeysParams, Logged, LsParams, Method, ReadParams, ReadResult, RunParams, SendParams, Target};
use shed_proto::{TerminalInfo, WaitParams};

/// #298: shed's lanes: terminals named as their agents, labelled `fleet`, `issue`, `role` and `harness`.
pub struct Shed(pub shed::Shed);

impl Shed {
    fn call(&self, r: &dyn Runner, m: Method, params: impl serde::Serialize) -> Result<Value> {
        shed::call(r, &self.0.wire, m, json!(params))
    }
    fn get(&self, r: &dyn Runner, name: &str) -> Result<TerminalInfo> {
        Ok(serde_json::from_value(self.call(
            r,
            Method::Get,
            Target { name: name.into() },
        )?)?)
    }
    pub fn transcript(&self, r: &dyn Runner, name: &str, before: Option<u64>, n: usize) -> Result<Option<Vec<Logged>>> {
        if self.get(r, name)?.acp.is_none() {
            return Ok(None);
        }
        let p = ReadParams {
            name: name.into(),
            lines: None,
            history: false,
            events: true,
            before,
            n: Some(n),
        };
        Ok(Some(
            serde_json::from_value::<ReadResult>(self.call(r, Method::Read, p)?)?.events,
        ))
    }
    /// `run` params for lane `name` in `cwd`: a kept terminal of the user's login shell, which `typed` lines then drive.
    pub fn run_params(name: &str, labels: &[(&str, String)], cwd: &str) -> RunParams {
        let labels = labels.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
        let cmd = argv(&["sh", "-c", "exec \"${SHELL:-sh}\" -l"]);
        RunParams {
            name: Some(name.into()),
            labels,
            cwd: cwd.into(),
            keep: true,
            idle_after: None,
            cmd,
            acp: None,
            resume: None,
            mcp_servers: vec![],
            no_hibernate: false,
            policy: None,
            meta: None,
            machine: None,
            parent: None,
            rows: None,
            cols: None,
        }
    }
    /// Start lane `name` (`run_params`), or with `[runtime] acp` that ACP agent (shed's CLI makes its bridge), #565: its adapter `cmd` after `--`. A terminal of that name an earlier lane left exited is removed first.
    pub fn start(
        &self,
        r: &dyn Runner,
        name: &str,
        labels: &[(&str, String)],
        cwd: &str,
        cmd: &[String],
    ) -> Result<()> {
        if self.get(r, name).is_ok_and(|t| t.state == shed_proto::State::Exited) {
            self.call(r, Method::Rm, Target { name: name.into() })?;
        }
        if self.0.acp.is_none() {
            return self.call(r, Method::Run, Self::run_params(name, labels, cwd)).map(drop);
        }
        let a = self.acp_argv(name, labels, cwd, cmd);
        shed::cli(r, &a.iter().map(String::as_str).collect::<Vec<_>>()).map(drop)
    }
    /// #899: its exited lane terminals (a `fleet` label), kept until removed.
    pub fn exited(&self, r: &dyn Runner) -> Result<Vec<TerminalInfo>> {
        let all: Vec<TerminalInfo> = serde_json::from_value(self.call(r, Method::Ls, LsParams::default())?)?;
        let gone = |t: &TerminalInfo| t.state == shed_proto::State::Exited && t.labels.contains_key("fleet");
        Ok(all.into_iter().filter(gone).collect())
    }
    /// #899: remove terminal `name`.
    pub fn rm(&self, r: &dyn Runner, name: &str) -> Result<()> {
        self.call(r, Method::Rm, Target { name: name.into() }).map(drop)
    }
    /// #938: a throwaway terminal running the breaker's canary shows its output: shed starts sessions again.
    pub fn canary(&self, r: &dyn Runner) -> bool {
        let name = "drover-canary";
        let _ = self.rm(r, name);
        let mut p = Self::run_params(name, &[], "/");
        p.cmd = argv(&["sh", "-c", crate::breaker::CANARY]);
        let wait = WaitParams {
            children: None,
            kind: None,
            name: name.into(),
            until: vec![],
            output: Some(crate::breaker::CANARY_OUT.into()),
            timeout: Some(20.0),
        };
        let ok = self.call(r, Method::Run, p).is_ok()
            && self
                .call(r, Method::Wait, wait)
                .is_ok_and(|v| v["outcome"] == "matched");
        let _ = self.rm(r, name);
        ok
    }
    /// The `shed run --acp` words that start ACP lane `name`.
    pub fn acp_argv(&self, name: &str, labels: &[(&str, String)], cwd: &str, cmd: &[String]) -> Vec<String> {
        let id = self.0.acp.as_deref().unwrap_or_default();
        let mut a = argv(&[
            "run",
            "--acp",
            id,
            "--policy",
            &self.0.policy,
            "--name",
            name,
            "--cwd",
            cwd,
        ]);
        a.extend(labels.iter().flat_map(|(k, v)| ["--label".into(), format!("{k}={v}")]));
        a.extend(
            (!cmd.is_empty())
                .then(|| "--".to_string())
                .into_iter()
                .chain(cmd.iter().cloned()),
        );
        a
    }
    /// The ACP lane's kick: once its session is ready, the prompt (framed by shed's CLI), confirmed by its state.
    pub fn kick_acp(&self, r: &dyn Runner, name: &str, text: &str) -> Result<bool> {
        let ready = WaitParams {
            children: None,
            kind: None,
            name: name.into(),
            until: vec![],
            output: Some("ACP session ready".into()),
            timeout: Some(60.0),
        };
        if self.call(r, Method::Wait, ready)?["outcome"] != "matched" {
            return Ok(false);
        }
        shed::cli(r, &["send", name, text])?;
        let up = WaitParams {
            children: None,
            kind: None,
            name: name.into(),
            until: argv(&["working", "blocked", "done"]),
            output: None,
            timeout: Some(20.0),
        };
        Ok(self.call(r, Method::Wait, up)?["outcome"] == "matched")
    }
}

fn agent_of(t: &TerminalInfo) -> Value {
    let up = t.state == shed_proto::State::Running;
    json!({"name": t.name, "pane_id": t.name, "cwd": t.cwd, "agent": t.labels.get("harness").filter(|_| up),
        "agent_status": t.reported.as_ref().map_or("unknown", |s| s.state.as_str()), "terminal_title": "", "runtime": "shed", "pid": t.pid,
        "cpu": t.resources.map(|u| u.cpu_pct)})
}

fn request(a: &[String]) -> Option<(Method, Value)> {
    let w: Vec<&str> = a.iter().map(String::as_str).collect();
    let flag = |f: &str| w.windows(2).find(|p| p[0] == f).map(|p| p[1]);
    let secs = flag("--timeout")
        .and_then(|ms| ms.parse::<f64>().ok())
        .map(|ms| ms / 1000.0);
    Some(match w[..] {
        ["pane", "read", t, ..] => {
            let lines = flag("--lines").and_then(|n| n.parse().ok());
            (
                Method::Read,
                json!(ReadParams {
                    name: t.into(),
                    lines,
                    history: lines.is_some(),
                    events: false,
                    before: None,
                    n: None,
                }),
            )
        }
        ["pane", "send-keys", t, k] => {
            let k = if k == "Escape" { "esc" } else { k };
            (
                Method::Keys,
                json!(KeysParams {
                    name: t.into(),
                    keys: vec![k.into()]
                }),
            )
        }
        ["agent", "prompt", t, text] | ["pane", "send-text", t, text] => (
            Method::Send,
            json!(SendParams {
                name: t.into(),
                text: text.into(),
                ..Default::default()
            }),
        ),
        ["pane", "run", t, ref line @ ..] => (
            Method::Send,
            json!(SendParams {
                name: t.into(),
                text: line.join(" ") + "\r",
                ..Default::default()
            }),
        ),
        ["agent", "wait", t, ..] => {
            let until = w
                .windows(2)
                .filter(|p| p[0] == "--until")
                .map(|p| p[1].to_string())
                .collect();
            (
                Method::Wait,
                json!(WaitParams {
                    children: None,
                    kind: None,
                    name: t.into(),
                    until,
                    output: None,
                    timeout: secs
                }),
            )
        }
        ["pane", "wait-output", t, ..] => {
            let output = flag("--match").map(String::from);
            (
                Method::Wait,
                json!(WaitParams {
                    children: None,
                    kind: None,
                    name: t.into(),
                    until: vec![],
                    output,
                    timeout: secs
                }),
            )
        }
        ["pane", "close", t] => (Method::Kill, json!(Target { name: t.into() })),
        ["pane", "get", t] => (Method::Get, json!(Target { name: t.into() })),
        _ => return None,
    })
}

fn failed(code: &str, message: &str) -> Out {
    let stderr = json!({"error": {"code": code, "message": message}}).to_string();
    Out {
        code: 1,
        stdout: String::new(),
        stderr,
    }
}

impl Shed {
    /// Run one pane word on shed; the caller reads the exit code.
    pub fn exec(&self, r: &dyn Runner, a: &[String]) -> Result<Out> {
        let Some((m, params)) = request(a) else {
            return Ok(failed("unsupported", &format!("shed runs no `{}`", a.join(" "))));
        };
        let name = params["name"].as_str().unwrap_or_default().to_string();
        let acp = matches!(m, Method::Send | Method::Keys) && self.get(r, &name).is_ok_and(|t| t.acp.is_some());
        let got = match m {
            Method::Send if acp => {
                shed::cli(r, &["send", &name, params["text"].as_str().unwrap_or_default()]).map(|_| json!({}))
            }
            Method::Keys
                if acp
                    && params["keys"][0]
                        .as_str()
                        .is_some_and(|k| k.eq_ignore_ascii_case("enter")) =>
            {
                Ok(json!({}))
            }
            Method::Kill => self.call(r, m, &params).and_then(|_| self.call(r, Method::Rm, &params)),
            _ => self.call(r, m, &params),
        };
        let v = match got {
            Ok(v) => v,
            Err(e) if format!("{e:#}").starts_with("NO_SUCH_TERMINAL") => {
                return Ok(failed("pane_not_found", &format!("{e:#}")))
            }
            Err(e) => return Ok(failed("shed", &format!("{e:#}"))),
        };
        let stdout = match m {
            Method::Read => v["text"].as_str().unwrap_or_default().to_string(),
            Method::Get => json!({"result": {"pane": agent_of(&serde_json::from_value(v)?)}}).to_string(),
            Method::Wait if v["outcome"] != "matched" => {
                let code = if v["outcome"] == "timeout" {
                    "timeout"
                } else {
                    "agent_exited"
                };
                return Ok(failed(code, &format!("shed wait {name}: {}", v["outcome"])));
            }
            _ => v.to_string(),
        };
        Ok(Out {
            code: 0,
            stdout,
            stderr: String::new(),
        })
    }
    /// Its live lanes, as agent rows.
    pub fn agents(&self, r: &dyn Runner) -> Result<Vec<Value>> {
        let all: Vec<TerminalInfo> = serde_json::from_value(self.call(r, Method::Ls, LsParams::default())?)?;
        let lanes = all
            .iter()
            .filter(|t| t.state == shed_proto::State::Running && t.labels.contains_key("fleet"));
        Ok(lanes.map(agent_of).collect())
    }
}

/// #298: a harness launch (`start_cmd`, a `pane run` line) as the line a lane's shell runs: `exec <line>`.
pub fn typed(pane: &str, start: &[String]) -> Vec<String> {
    run_argv(pane, &["exec", &start.get(3..).unwrap_or_default().join(" ")])
}

/// #298: pane word `a` as the shed request a plan shows (`shed <method> <params>`).
pub fn shed_line(a: &[String]) -> Vec<String> {
    request(a).map_or_else(|| a.to_vec(), |(m, v)| argv(&["shed", m.name(), &v.to_string()]))
}

fn shed(r: &dyn Runner) -> Result<Shed> {
    r.shed()
        .map(Shed)
        .context("no shed runtime: set `[runtime] kind = \"shed-acp\"` (docs/runtimes.md)")
}

/// Run a pane word built here (`*_argv`) on shed; the caller reads the exit code.
pub fn exec(r: &dyn Runner, a: &[String]) -> Result<Out> {
    shed(r)?.exec(r, a)
}

/// `exec`, failing on a non-zero exit; returns stdout.
pub fn ok(r: &dyn Runner, a: &[String]) -> Result<String> {
    let o = exec(r, a)?;
    if o.code != 0 {
        bail!("{} failed ({}): {}", a.join(" "), o.code, o.stderr.trim());
    }
    Ok(o.stdout)
}

/// `exec` as JSON, failing on a non-zero exit.
pub fn json(r: &dyn Runner, a: &[String]) -> Result<Value> {
    serde_json::from_str(&ok(r, a)?).with_context(|| format!("{}: not JSON", a.join(" ")))
}

/// Live lanes, as agent rows: shed's on `r`.
pub fn agents(r: &dyn Runner) -> Result<Vec<Value>> {
    shed(r)?.agents(r)
}

/// The agent list as `{"result": {"agents": [...]}}`.
pub fn list(r: &dyn Runner) -> Result<Value> {
    Ok(json!({"result": {"agents": agents(r)?}}))
}

/// The agent list as JSON text when it answered (serve's watcher hashes and caches it).
pub fn agent_list(r: &dyn Runner) -> Option<String> {
    list(r).ok().map(|v| v.to_string())
}

pub fn read_argv(pane: &str) -> Vec<String> {
    argv(&["pane", "read", pane, "--source", "visible"])
}

/// The pane's visible screen.
pub fn read(r: &dyn Runner, pane: &str) -> Result<String> {
    ok(r, &read_argv(pane))
}

/// The pane's last `lines` lines, with their SGR colours (#138).
pub fn recent(r: &dyn Runner, pane: &str, lines: &str) -> Result<String> {
    let a = format!("pane read {pane} --source recent --lines {lines} --format ansi");
    ok(r, &argv(&a.split(' ').collect::<Vec<_>>()))
}

pub fn keys_argv(pane: &str, key: &str) -> Vec<String> {
    argv(&["pane", "send-keys", pane, key])
}

/// Send one key (`Enter`, `Escape`, a menu digit) to the pane.
pub fn keys(r: &dyn Runner, pane: &str, key: &str) -> Result<String> {
    ok(r, &keys_argv(pane, key))
}

pub fn prompt_argv(agent: &str, text: &str) -> Vec<String> {
    argv(&["agent", "prompt", agent, text])
}

/// Type `text` into the agent's prompt (no Enter).
pub fn prompt(r: &dyn Runner, agent: &str, text: &str) -> Result<String> {
    ok(r, &prompt_argv(agent, text))
}

pub fn send_text_argv(pane: &str, text: &str) -> Vec<String> {
    argv(&["pane", "send-text", pane, text])
}

/// The page's reply: the text (dsh: `pane send-text`, else `agent prompt`), then Enter.
pub fn reply(r: &dyn Runner, dsh: bool, agent: &str, pane: &str, text: &str) -> Result<()> {
    let first = if dsh {
        send_text_argv(pane, text)
    } else {
        prompt_argv(agent, text)
    };
    for a in [first, keys_argv(pane, "Enter")] {
        let o = exec(r, &a)?;
        if o.code != 0 {
            bail!("{} {}: {}", a[0], a[1], o.stderr.trim());
        }
    }
    Ok(())
}

/// #908: Err `HOST_BUSY` (the prompt held to a later pass) while shed has `agent` asleep and the host has no room to wake it.
pub fn awake(r: &dyn Runner, agent: &str) -> Result<()> {
    let t = r.shed().map(Shed).and_then(|s| s.get(r, agent).ok());
    if t.and_then(|t| t.reported).is_none_or(|s| s.state != "hibernated") {
        return Ok(());
    }
    let (l, h) = (crate::config::limits(), crate::host::sample(r));
    let low = (h.free_mem_gb.filter(|g| *g < l.min_free_mem_gb)).map(|g| format!("only {g:.1} GB of memory free"));
    match low.or_else(|| crate::host::admit(&h, &l).err()) {
        Some(why) => bail!("REFUSED HOST_BUSY: {agent} is asleep and waking it starts its agent: {why}; held"),
        None => Ok(()),
    }
}

/// #908: `reply` once `awake` lets drover's prompt through.
pub fn tell(r: &dyn Runner, dsh: bool, agent: &str, pane: &str, text: &str) -> Result<()> {
    awake(r, agent)?;
    reply(r, dsh, agent, pane, text)
}

/// #143: whether a prompt the caller just entered moved the agent off idle within 20 s, typing it once more if not.
pub fn landed(r: &dyn Runner, agent: &str, pane: &str, text: &str) -> bool {
    let up = || exec(r, &wait_working_argv(agent, "20000")).is_ok_and(|o| o.code == 0);
    up() || (reply(r, false, agent, pane, text).is_ok() && up())
}

/// Close the pane; one already gone (#249: a worktree remove closed its workspace) is closed.
pub fn close(r: &dyn Runner, pane: &str) -> Result<String> {
    let o = exec(r, &argv(&["pane", "close", pane]))?;
    if o.code != 0 && ![&o.stderr, &o.stdout].iter().any(|s| s.contains("pane_not_found")) {
        bail!("pane close {pane} failed ({}): {}", o.code, o.stderr.trim());
    }
    Ok(o.stdout)
}

/// A `{"error": {"code": <code>}}` answer (on stderr or stdout), e.g. `timeout`.
pub fn failed_with(o: &Out, code: &str) -> bool {
    [&o.stderr, &o.stdout]
        .iter()
        .any(|s| serde_json::from_str::<Value>(s.trim()).is_ok_and(|v| v["error"]["code"] == code))
}

/// #477: a failed call's `error.message` (the JSON envelope), else its stderr.
pub fn message(o: &Out) -> String {
    let m = [&o.stderr, &o.stdout].iter().find_map(|s| {
        let v: Value = serde_json::from_str(s.trim()).ok()?;
        Some(v["error"]["message"].as_str()?.to_string())
    });
    m.unwrap_or_else(|| o.stderr.trim().into())
}

/// #359: how long a lane's startup is waited for.
pub const CONFIRM_SECS: u64 = 120;

pub fn pane_get_argv(pane: &str) -> Vec<String> {
    argv(&["pane", "get", pane])
}

/// `pane get`'s pane, when a harness (any detected agent) runs in it.
pub fn harness_in(r: &dyn Runner, pane: &str) -> Option<Value> {
    let v = json(r, &pane_get_argv(pane)).ok()?;
    Some(v["result"]["pane"].clone()).filter(|p| !p["agent"].is_null())
}

/// `pane run <pane> <words…>`: type a line into the pane's shell.
pub fn run_argv(pane: &str, words: &[&str]) -> Vec<String> {
    argv(&[&["pane", "run", pane][..], words].concat())
}

/// The harness `kind` launched in `pane` with `args`, as its shell line.
pub fn start_argv(kind: &str, pane: &str, args: &[String]) -> Vec<String> {
    run_argv(pane, &[&crate::hosts::quote(kind, args)])
}

/// `agent wait` until the kick shows as working or blocked.
pub fn wait_working_argv(agent: &str, ms: &str) -> Vec<String> {
    let a = format!("agent wait {agent} --until working --until blocked --timeout {ms}");
    argv(&a.split(' ').collect::<Vec<_>>())
}

pub fn wait_output_argv(pane: &str, pattern: &str, ms: &str) -> Vec<String> {
    argv(&[
        "pane",
        "wait-output",
        pane,
        "--match",
        pattern,
        "--source",
        "visible",
        "--timeout",
        ms,
    ])
}
