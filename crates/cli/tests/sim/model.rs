use crate::sched::{Sim, World};
use drover::runner::Out;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

pub const FLEET: &str = "drover";
pub const REPO: &str = "jirathip-dev/herdsman";

/// What a lane's agent does once its prompt lands.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum Script {
    Push {
        secs: u64,
        green: bool,
        #[serde(default)]
        ci: Option<u64>,
    },
    Review {
        secs: u64,
        pass: bool,
        proof: bool,
        heeds: bool,
    },
    Die {
        secs: u64,
    },
    Hang,
}

#[derive(Clone, Debug)]
pub enum Ev {
    Timer { args: Vec<String>, every: u64 },
    Run(Vec<String>),
    Lane { pane: String, step: u8 },
    Ci { sha: String, green: bool },
    Load(u64),
    Crash(String),
}

#[derive(Clone, Debug)]
pub struct Pane {
    pub id: String,
    pub ws: String,
    pub cwd: String,
    pub agent: Option<String>,
    pub status: String,
    pub screen: String,
    pub born: u64,
    pub closed: bool,
    pub script: Option<Script>,
    /// Prompts it was sent (a NO_PROOF refusal among them).
    pub told: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Pr {
    pub number: u64,
    pub head: String,
    pub base: String,
    pub state: String,
    pub merged: Option<String>,
}

/// One accepted `gh pr merge`, as the model checked it (I4).
#[derive(Clone, Debug)]
pub struct Merge {
    pub issue: u64,
    pub head: String,
}

#[derive(Default)]
pub struct Model {
    pub panes: Vec<Pane>,
    ws: u32,
    pub prs: BTreeMap<String, Pr>,
    /// Each head's CI: `None` pending, else green.
    pub ci: BTreeMap<String, Option<bool>>,
    /// Branch heads on origin, and each worktree's checked-out head.
    pub origin: BTreeMap<String, String>,
    pub heads: BTreeMap<String, String>,
    /// The integration head each commit is based on (a behind PR's is older than integration's).
    pub base: BTreeMap<String, String>,
    pub load: u64,
    pub agenda: BTreeMap<(u64, u64), Ev>,
    seq: u64,
    shas: u64,
    /// Scripts queued per role (or `<role>#<issue>`): the next lane takes the first, the last is kept.
    pub scripts: BTreeMap<String, Vec<Script>>,
    pub merges: Vec<Merge>,
    pub closed: Vec<u64>,
    pub version: u64,
    /// Lanes whose pane the scenario killed (I9 allows their respawns).
    pub died: Vec<String>,
    /// Test-only break switches (AC 3).
    pub sabotage: Option<crate::invariants::Sabotage>,
    /// #685: every head's CI takes this long (else 1–15 min, drawn).
    pub ci_secs: Option<u64>,
    /// #685 ground truth: when each pending head's CI started, the seconds heads spent pending, pane-seconds a live lane spent not working, process-seconds in 1 s polls (drive.lock and kick waits), and the panes that pushed a head or wrote a verdict drover takes.
    pub ci_since: BTreeMap<String, u64>,
    pub ci_wait: u64,
    pub idle: u64,
    pub polled: u64,
    pub produced: Vec<String>,
}

impl Model {
    pub fn at(&mut self, t: u64, e: Ev) {
        self.seq += 1;
        self.agenda.insert((t, self.seq), e);
    }
    /// A new head's CI starts at `now`; its end is `secs`, `ci_secs` or drawn from `rng`.
    pub fn ci_start(&mut self, sha: &str, now: u64, secs: Option<u64>, rng: &mut crate::sched::Rng) -> u64 {
        self.ci.insert(sha.into(), None);
        self.ci_since.insert(sha.into(), now);
        now + secs.or(self.ci_secs).unwrap_or_else(|| 60 * rng.range(1, 15))
    }
    pub fn sha(&mut self) -> String {
        self.shas += 1;
        format!("{:040x}", 0xc0ffee_u64 * 1_000_003 + self.shas)
    }
    pub fn live(&self) -> impl Iterator<Item = &Pane> {
        self.panes.iter().filter(|p| !p.closed && p.agent.is_some())
    }
    fn pane(&mut self, id: &str) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id && !p.closed)
    }
    fn by_agent(&mut self, name: &str) -> Option<&mut Pane> {
        self.panes
            .iter_mut()
            .find(|p| !p.closed && p.agent.as_deref() == Some(name))
    }
    fn agent_json(p: &Pane) -> Value {
        json!({"name": p.agent, "agent_status": p.status, "cwd": p.cwd, "pane_id": p.id, "agent": "claude",
            "workspace_id": p.ws})
    }
    fn pr_json(&self, branch: &str, p: &Pr) -> Value {
        let ci = self.ci.get(&p.head).copied().flatten();
        let check = match ci {
            None => json!({"__typename": "CheckRun", "name": "ci", "status": "IN_PROGRESS", "conclusion": ""}),
            Some(g) => json!({"__typename": "CheckRun", "name": "ci", "status": "COMPLETED",
                "conclusion": if g { "SUCCESS" } else { "FAILURE" }}),
        };
        let n = branch.trim_start_matches("issue-");
        json!({"number": p.number, "title": format!("#{n}"), "body": format!("Closes #{n}"), "state": p.state,
            "baseRefName": p.base, "headRefName": branch, "headRefOid": p.head, "mergeable": "MERGEABLE",
            "mergeStateStatus": if self.base.get(&p.head) == self.origin.get(&p.base) { "CLEAN" } else { "BEHIND" }, "labels": [], "statusCheckRollup": [check],
            "url": format!("https://github.com/{REPO}/pull/{}", p.number),
            "mergeCommit": p.merged.as_ref().map(|m| json!({"oid": m}))})
    }
}

fn ok(stdout: impl Into<String>) -> Out {
    Out {
        code: 0,
        stdout: stdout.into(),
        stderr: String::new(),
    }
}

fn err(code: i32, stderr: &str) -> Out {
    Out {
        code,
        stdout: String::new(),
        stderr: stderr.into(),
    }
}

fn flag<'a>(a: &'a [&str], f: &str) -> Option<&'a str> {
    a.iter().position(|x| *x == f).and_then(|i| a.get(i + 1).copied())
}

/// Process `me`'s call `prog args`, answered from (and applied to) the world.
pub fn answer(sim: &Arc<Sim>, w: &mut World, me: usize, prog: &str, args: &[String]) -> Out {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let o = match prog {
        "herdr" => {
            let o = herdr(sim, w, &a);
            if let (["agent", "start", _, _, _, _, pane, ..], 0) = (&a[..], o.code) {
                w.procs[me].starting = Some((*pane).into());
                let agent = Some(a[2].to_string());
                let starts = w.model.panes.iter().filter(|p| p.agent == agent).count();
                let planted = w.model.sabotage == Some(crate::invariants::Sabotage::Planted) && starts >= 3;
                if planted || w.model.sabotage == Some(crate::invariants::Sabotage::DoubleLane) {
                    crate::invariants::double(w, pane);
                }
            }
            o
        }
        "gh" => gh(w, &a),
        "git" => git(w, &a),
        "kill" => {
            let pid: u32 = a.last().and_then(|p| p.parse().ok()).unwrap_or(0);
            let live = w.proc_of_pid(pid).is_some_and(crate::sched::Proc::live);
            if live {
                ok("")
            } else {
                err(1, "no such process")
            }
        }
        "ps" if a.first() == Some(&"-axo") => ok(""),
        "ps" => {
            let pid: u32 = a.last().and_then(|p| p.parse().ok()).unwrap_or(0);
            match w.proc_of_pid(pid).filter(|p| p.live()) {
                Some(p) => {
                    let up = w.now - p.started.unwrap_or(w.now);
                    ok(format!("{:02}:{:02}:{:02}\n", up / 3600, up / 60 % 60, up % 60))
                }
                None => err(1, ""),
            }
        }
        "sysctl" => match a.last().copied() {
            Some("kern.memorystatus_vm_pressure_level") => ok("1\n"),
            Some("vm.loadavg") => {
                let l = w.model.load;
                ok(format!("{{ {l}.00 {l}.00 {l}.00 }}\n"))
            }
            Some("hw.ncpu") => ok("10\n"),
            Some("hw.memsize") => ok("68719476736\n"),
            _ => err(1, "sysctl"),
        },
        "vm_stat" => ok("Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free: 400000.\nPages inactive: 400000.\nPages speculative: 0.\nSwapouts: 100.\n"),
        "df" => ok("Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/disk3s5 1000000000 100000000 900000000 10% /\n"),
        "date" => err(1, "date"),
        "lsof" => ok(""),
        "hostname" => ok("sim\n"),
        _ => err(127, "unmodelled"),
    };
    if o.code == 127 && o.stderr == "unmodelled" {
        let _ = me;
        w.fail(format!("unmodelled call: {prog} {}", args.join(" ")));
    }
    o
}

fn unmodelled() -> Out {
    err(127, "unmodelled")
}

fn herdr(sim: &Arc<Sim>, w: &mut World, a: &[&str]) -> Out {
    let now = w.now;
    let m = &mut w.model;
    match a {
        ["agent", "list"] => {
            let list: Vec<Value> = m.live().map(Model::agent_json).collect();
            ok(json!({"result": {"agents": list}}).to_string())
        }
        ["worktree", "create" | "open", ..] => {
            let path = flag(a, "--path").unwrap_or_default().to_string();
            let _ = std::fs::create_dir_all(&path);
            std::fs::create_dir_all(std::path::Path::new(&path).join(".git")).ok();
            m.ws += 1;
            let ws = format!("w{}", m.ws);
            let base = flag(a, "--base").map(|b| b.trim_start_matches("origin/").to_string());
            let head = base
                .and_then(|b| m.origin.get(&b).cloned())
                .or_else(|| flag(a, "--branch").and_then(|b| m.origin.get(b).cloned()));
            if let Some(h) = head {
                m.heads.entry(path.clone()).or_insert(h);
            }
            m.panes.push(Pane {
                id: format!("{ws}:p1"),
                ws: ws.clone(),
                cwd: path,
                agent: None,
                status: "idle".into(),
                screen: String::new(),
                born: now,
                closed: false,
                script: None,
                told: vec![],
            });
            ok(
                json!({"result": {"workspace": {"workspace_id": ws}, "root_pane": {"pane_id": format!("{ws}:p1")}}})
                    .to_string(),
            )
        }
        ["workspace", "rename", ..] | ["agent", "rename", ..] => ok("{}"),
        ["workspace", "close", ws] => {
            for p in m.panes.iter_mut().filter(|p| p.ws == *ws) {
                p.closed = true;
            }
            ok("{}")
        }
        ["worktree", "remove", "--workspace", ws] => {
            for p in m.panes.iter_mut().filter(|p| p.ws == *ws) {
                p.closed = true;
                let _ = std::fs::remove_dir_all(&p.cwd);
                m.heads.remove(&p.cwd);
            }
            ok("{}")
        }
        ["pane", "list", "--workspace", ws] => {
            let list: Vec<Value> = (m.panes.iter())
                .filter(|p| p.ws == *ws && !p.closed)
                .map(|p| json!({"pane_id": p.id, "agent": p.agent.as_ref().map(|_| "claude"), "cwd": p.cwd}))
                .collect();
            if list.is_empty() {
                return err(1, "workspace_not_found");
            }
            ok(json!({"result": {"panes": list}}).to_string())
        }
        ["pane", "run", pane, ..] => match m.pane(pane) {
            Some(_) => ok("{}"),
            None => err(1, "pane_not_found"),
        },
        ["agent", "start", agent, "--kind", _, "--pane", pane, ..] => {
            let role = agent.split('-').nth(1).unwrap_or("impl").to_string();
            let own = format!("{role}#{}", agent.split('-').nth(2).unwrap_or_default());
            let queued = match m.scripts.get_mut(&own) {
                Some(v) if v.len() > 1 => Some(v.remove(0)),
                Some(v) => v.first().cloned(),
                None => m.scripts.get_mut(&role).filter(|v| !v.is_empty()).map(|v| v.remove(0)),
            };
            let script = queued.unwrap_or_else(|| crate::scenario::default_script(&mut w.rng, &role));
            w.log(format!("model: {agent} runs {script:?}"));
            let m = &mut w.model;
            let Some(p) = m.pane(pane) else {
                return err(1, "pane_not_found");
            };
            if p.agent.is_some() {
                return err(1, "agent_pane_busy");
            }
            (p.agent, p.status, p.script, p.born) = (Some((*agent).into()), "working".into(), Some(script), now);
            p.screen = "⏺ reading the brief\n".into();
            let id = p.id.clone();
            m.at(now + 1, Ev::Lane { pane: id, step: 0 });
            ok(json!({"result": {"agent": {"name": agent, "pane_id": pane}}}).to_string())
        }
        ["agent", "wait", agent, ..] => match m.by_agent(agent) {
            Some(p) if p.status == "working" || p.status == "blocked" => ok("{}"),
            Some(_) => err(1, "timeout"),
            None => err(1, "agent_not_found"),
        },
        ["pane", "get", pane] => match m.pane(pane) {
            Some(p) => ok(
                json!({"result": {"pane": {"pane_id": p.id, "agent": p.agent.as_ref().map(|_| "claude"),
                "agent_status": p.status, "terminal_title": p.agent, "cwd": p.cwd}}})
                .to_string(),
            ),
            None => err(1, "pane_not_found"),
        },
        ["pane", "read", pane, ..] => match m.pane(pane) {
            Some(p) => ok(p.screen.clone()),
            None => err(1, "pane_not_found"),
        },
        ["pane", "close", pane] => match m.pane(pane) {
            Some(p) => {
                p.closed = true;
                ok("{}")
            }
            None => err(1, "pane_not_found"),
        },
        ["agent", "prompt", agent, text] => match m.by_agent(agent) {
            Some(p) => {
                p.told.push((*text).into());
                p.screen += &format!("❯ {text}\n");
                let id = p.id.clone();
                crate::scenario::told(m, &id, now);
                ok("{}")
            }
            None => err(1, "agent_not_found"),
        },
        ["pane", "send-text", pane, text] => match m.pane(pane) {
            Some(p) => {
                p.told.push((*text).into());
                p.screen += &format!("❯ {text}\n");
                ok("{}")
            }
            None => err(1, "pane_not_found"),
        },
        ["pane", "send-keys", pane, _] => match m.pane(pane) {
            Some(_) => ok("{}"),
            None => err(1, "pane_not_found"),
        },
        ["status"] => ok("herdr 0.5.0\nserver 0.5.0 running\n"),
        ["workspace", "list"] => ok(json!({"result": {"workspaces": []}}).to_string()),
        _ => {
            let _ = sim;
            unmodelled()
        }
    }
}

fn gh(w: &mut World, a: &[&str]) -> Out {
    let m = &mut w.model;
    match a {
        ["api", "graphql", "--paginate", "--jq", jq, ..] if jq.contains("pullRequests") => {
            let open = m.prs.iter().filter(|(_, p)| p.state == "OPEN");
            ok(open.map(|(b, p)| format!("{}\n", m.pr_json(b, p))).collect::<String>())
        }
        ["api", "graphql", "--paginate", "--jq", jq, ..] if jq.contains("issues") => ok(""),
        ["api", "-i", ..] => ok(format!("HTTP/2.0 200 OK\netag: \"v{}\"\n\n[]", m.version)),
        ["api", path, "--jq", _] if path.contains("/rules/branches/") => ok("true\n"),
        ["api", path, "--jq", ".commit.sha"] => {
            let b = path.rsplit('/').next().unwrap_or_default();
            match m.origin.get(b) {
                Some(h) => ok(format!("{h}\n")),
                None => err(1, "HTTP 404: Branch not found"),
            }
        }
        ["pr", "view", which, ..] => {
            let by = |(b, p): &(&String, &Pr)| *b == which || p.number.to_string() == *which;
            match m.prs.iter().find(by) {
                Some((b, p)) => ok(m.pr_json(b, p).to_string()),
                None => err(1, "no pull requests found"),
            }
        }
        ["issue", "view", n, ..] => {
            let closed = n.parse().is_ok_and(|n: u64| m.closed.contains(&n));
            ok(json!({"title": format!("issue {n}"), "body": "## Spec\n- do it\n", "updatedAt": "2026-11-01T00:00:00Z",
                "comments": [], "state": if closed { "CLOSED" } else { "OPEN" }, "labels": []})
            .to_string())
        }
        ["pr", "merge", n, ..] => {
            let n: u64 = n.parse().unwrap_or(0);
            let Some((b, p)) = m.prs.iter_mut().find(|(_, p)| p.number == n) else {
                return err(1, "no such PR");
            };
            p.state = "MERGED".into();
            let (head, base) = (p.head.clone(), p.base.clone());
            let issue = b.trim_start_matches("issue-").parse().unwrap_or(0);
            m.merges.push(Merge {
                issue,
                head: head.clone(),
            });
            let new = m.sha();
            if let Some(p) = m.prs.values_mut().find(|p| p.number == n) {
                p.merged = Some(new.clone());
            }
            m.origin.insert(base.clone(), new);
            m.version += 1;
            crate::invariants::on_merge(w, issue, &head, &base);
            ok("")
        }
        ["issue", "close", n, ..] => {
            if let Ok(n) = n.parse() {
                m.closed.push(n);
            }
            ok("")
        }
        ["issue", "comment", ..] | ["pr", "edit", ..] | ["label", "create", ..] => ok(""),
        ["run", "list", ..] => ok("[]"),
        ["issue", "list", ..] => ok("[]"),
        _ => unmodelled(),
    }
}

fn git(w: &mut World, a: &[&str]) -> Out {
    let m = &mut w.model;
    let (dir, rest) = match a {
        ["-C", d, rest @ ..] => (*d, rest),
        rest => ("", rest),
    };
    match rest {
        ["fetch", ..] => ok(""),
        ["rev-parse", "HEAD"] => match m.heads.get(dir) {
            Some(h) => ok(format!("{h}\n")),
            None => err(128, "fatal: not a git repository"),
        },
        ["rev-parse", "--short=7", "HEAD"] => ok(m.heads.get(dir).map_or(String::new(), |h| format!("{:.7}\n", h))),
        ["rev-parse", "--verify", "--quiet", b] | ["rev-parse", b] => {
            match m.origin.get(b.trim_start_matches("origin/")) {
                Some(h) => ok(format!("{h}\n")),
                None => err(1, ""),
            }
        }
        ["log", "-1", "--format=%H %ct"] => match m.heads.get(dir) {
            Some(h) => ok(format!("{h} {}\n", w.now)),
            None => err(128, "fatal"),
        },
        ["status", "--porcelain", ..] => ok(""),
        ["rev-list", "--objects", "--disk-usage", ..] | ["rev-list", "--count", ..] => ok("0\n"),
        ["rev-list", ..] | ["log", ..] | ["diff", ..] | ["branch", ..] | ["ls-files", ..] => ok(""),
        ["reset", "--hard", b] => {
            if let Some(h) = m.origin.get(b.trim_start_matches("origin/")).cloned() {
                m.heads.insert(dir.into(), h);
            }
            ok("")
        }
        ["ls-remote", "origin", r] => match m.heads.get(dir) {
            Some(h) => ok(format!("{h}\t{r}\n")),
            None => ok(""),
        },
        ["for-each-ref", _, "refs/remotes/origin/wip/"] => ok(m
            .origin
            .keys()
            .filter(|b| b.starts_with("wip/"))
            .map(|b| format!("{b}\n"))
            .collect::<String>()),
        ["write-tree"] => ok(format!("{:040x}\n", 8)),
        ["push", "-q", "origin", to] if to.starts_with("HEAD:") && !to.contains("refs/") => {
            let (branch, Some(head)) = (&to[5..], m.heads.get(dir).cloned()) else {
                return err(1, "no head");
            };
            m.origin.insert(branch.into(), head.clone());
            if let Some(p) = m.prs.get_mut(branch) {
                p.head = head.clone();
            }
            m.version += 1;
            let t = w.model.ci_start(&head, w.now, None, &mut w.rng);
            let green = w.rng.chance(90);
            w.model.at(t, Ev::Ci { sha: head, green });
            ok("")
        }
        ["merge", "--no-edit", into] => {
            let (Some(h), Some(s)) = (
                m.heads.get(dir).cloned(),
                m.origin.get(into.trim_start_matches("origin/")).cloned(),
            ) else {
                return err(1, "merge: nothing to merge");
            };
            if m.base.get(&h) == Some(&s) {
                return ok("Already up to date.\n");
            }
            let n = m.sha();
            m.base.insert(n.clone(), s);
            m.heads.insert(dir.into(), n);
            ok("")
        }
        ["add", ..] | ["reset", "-q", ..] | ["ls-tree", ..] | ["commit", ..] | ["push", ..] | ["stash", ..] => ok(""),
        ["merge-tree", ..] => ok(format!("{:040x}\n", 7)),
        ["show", _] => err(128, "fatal: path does not exist"),
        ["worktree", "add", .., path, sha] => {
            let _ = std::fs::create_dir_all(path);
            m.heads.insert((*path).into(), (*sha).into());
            ok("")
        }
        ["worktree", "remove", "--force", path] => {
            let _ = std::fs::remove_dir_all(path);
            m.heads.remove(*path);
            ok("")
        }
        ["worktree", "prune"] | ["remote", ..] | ["config", ..] => ok(""),
        _ => unmodelled(),
    }
}
