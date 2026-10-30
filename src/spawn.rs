//! `herdsman spawn` (worktree + herdr pane + harness launch + verified kick) and
//! `herdsman profiles sync` (each harness's per-role profile files; see `harness`).
use crate::access;
use crate::config::{Config, Fleet};
use crate::events::{emit, fold, read, Event};
use crate::gate::refuse;
use crate::harness::{self, hook_command};
use crate::host;
use crate::runner::{argv, json, ok, Runner};
use crate::Ctx;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn role_prompt(role: &str) -> &'static str {
    match role {
        "orch" => include_str!("../roles/orch.md"),
        "impl" => include_str!("../roles/impl.md"),
        _ => include_str!("../roles/rev.md"),
    }
}

pub fn profile_dir(ctx: &Ctx, profile: &str) -> PathBuf {
    ctx.claude_dir().join(profile)
}

/// Write every harness's files for every role of every fleet (Claude settings +
/// role prompt, the Hermes profile's `hooks:` key, the DSH patch), so a role's
/// harness can be switched with `herdsman set` alone.
pub fn profiles_sync(ctx: &Ctx, cfg: &Config, usage: &str, out: &mut dyn Write) -> Result<()> {
    let mut done = std::collections::BTreeSet::new();
    for f in cfg.fleet.values() {
        for (role, r) in &f.role {
            let profile = r.profile_or(role);
            if !done.insert(profile.clone()) {
                continue;
            }
            for h in harness::NAMES {
                harness::get(h).sync(ctx, role, &profile, usage, out)?;
            }
            harness::set_mcp(ctx, &profile, &access::mcp_servers(cfg, f, r)?)?;
        }
    }
    Ok(())
}

/// `herdsman profiles sync --check`: each fleet Hermes profile must list its herdsman hook
/// as `✓ allowed` in `hermes -p <profile> hooks list`, run with `HERDSMAN_LANE` set as
/// in a fleet lane. False otherwise.
pub fn profiles_check(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<bool> {
    let (mut good, mut done) = (true, std::collections::BTreeSet::new());
    for (name, f) in &cfg.fleet {
        for (role, rc) in &f.role {
            let profile = rc.profile_or(role);
            if !done.insert(profile.clone()) || !ctx.hermes.join(&profile).join("config.yaml").exists() {
                continue;
            }
            let lane = format!("HERDSMAN_LANE={name}:-:{role}");
            let o = r.run("env", &argv(&[&lane, "hermes", "-p", &profile, "hooks", "list"]))?;
            let cmd = hook_command(ctx, role);
            let allowed = o.code == 0 && o.stdout.lines().any(|l| l.contains(&cmd) && l.contains("✓ allowed"));
            writeln!(out, "{} {profile} ({role})", if allowed { "✓ allowed" } else { "✗ not allowed" })?;
            good &= allowed;
        }
    }
    Ok(good)
}

pub struct Plan {
    pub agent: String,
    pub role: String,
    pub harness: String,
    pub profile: String,
    pub model: Option<String>,
    pub branch: String,
    pub base: String,
    pub worktree: PathBuf,
    pub brief: PathBuf,
    pub kick: String,
    /// `HERDSMAN_LANE` value (`<fleet>:<issue>:<role>`); arms `herdsman hook` in the lane.
    pub lane: String,
    /// Claude lanes: the account name and the shell assignment exporting its token.
    pub account: Option<(String, String)>,
    /// The only MCP servers the lane gets, and the lane-local file Claude reads them from.
    pub mcp: serde_json::Map<String, Value>,
    pub mcp_file: PathBuf,
}

impl Plan {
    /// Exports `HERDSMAN_LANE` in the pane's shell, so the harness and its hooks inherit it.
    /// Claude lanes with an account also export `CLAUDE_CODE_OAUTH_TOKEN` there.
    pub fn lane_cmd(&self, pane: &str) -> Vec<String> {
        let mut c = argv(&["pane", "run", pane, "export", &format!("HERDSMAN_LANE='{}'", self.lane)]);
        c.extend(self.account.iter().map(|a| a.1.clone()));
        c
    }

    pub fn start_cmd(&self, ctx: &Ctx, pane: &str) -> Vec<String> {
        harness::get(&self.harness).start_cmd(ctx, self, pane)
    }

    pub fn kick_cmds(&self, pane: &str) -> Vec<Vec<String>> {
        harness::get(&self.harness).kick_cmds(self, pane)
    }
}

pub fn plan(ctx: &Ctx, cfg: &Config, fleet: &str, issue: Option<u64>, role: &str) -> Result<Plan> {
    let f = cfg.fleet(fleet)?;
    let r = f.role(role)?;
    let (agent, branch, base, worktree, brief) = match issue {
        None => {
            anyhow::ensure!(role == "orch", "only the orch lane runs without an issue");
            let brief = ctx.state.parent().unwrap_or(&ctx.state).join(format!("orch-{fleet}.md"));
            (format!("{fleet}-orch"), f.integration.clone(), String::new(), f.local_path(), brief)
        }
        Some(issue) => {
            anyhow::ensure!(role != "orch", "the orch lane takes no issue: `herdsman spawn {fleet} - orch`");
            let st = fold(&read(&ctx.state)?).remove(&(fleet.to_string(), issue));
            if st.as_ref().is_some_and(|s| s.state == "merged") {
                return Err(refuse("MERGED", format!("{fleet}#{issue} is merged; a merged issue never reopens")).into());
            }
            let impl_branch = st.as_ref().and_then(|s| s.branch.clone()).unwrap_or_else(|| format!("issue-{issue}"));
            let (agent, branch, base) = if role == "rev" {
                let k = st.as_ref().map_or(0, |s| s.rounds) + 1;
                (format!("{fleet}-rev-{issue}-r{k}"), format!("rev-{issue}-r{k}"), format!("origin/{impl_branch}"))
            } else {
                (format!("{fleet}-{role}-{issue}"), impl_branch, format!("origin/{}", f.integration))
            };
            let wt = ctx.worktrees.join(f.repo_name()).join(&branch);
            (agent, branch, base, wt.clone(), wt.join(".brief.md"))
        }
    };
    let kick = format!("Read {} and execute it fully.", brief.display());
    let lane = format!("{fleet}:{}:{role}", issue.map_or("-".into(), |n| n.to_string()));
    let account = match r.harness.as_str() {
        "claude" => access::pick(cfg, &read(&ctx.state)?, r, ctx.now)?,
        _ => None,
    };
    let mcp_file = match issue {
        None => brief.with_file_name(format!("orch-{fleet}.mcp.json")),
        Some(_) => worktree.join(".mcp.lane.json"),
    };
    Ok(Plan {
        agent,
        role: role.into(),
        harness: r.harness.clone(),
        profile: r.profile_or(role),
        model: r.model.clone(),
        branch,
        base,
        worktree,
        brief,
        kick,
        lane,
        account,
        mcp: access::mcp_servers(cfg, f, r)?,
        mcp_file,
    })
}

fn brief(f: &Fleet, issue: u64, p: &Plan, title: &str, body: &str, fix: Option<&str>) -> String {
    let contract = if p.role == "rev" {
        format!(
            "- Read-only: no edits, no commits, no push.\n- Review the diff `{}...HEAD` against the issue's acceptance criteria.\n\
             - Write `{}/verdict.md`: first line `PASS` or `FAIL`, then findings (`blocking`/`non-blocking`, file:line).\n\
             - Report the exact head sha you reviewed (`git rev-parse HEAD`).",
            format_args!("origin/{}", f.integration),
            p.worktree.display()
        )
    } else {
        format!(
            "- Work on branch `{}` only. Commit, push it, open a PR to `{}` whose body says `Closes #{issue}`.\n\
             - Never merge, never push to `{}` or main — merging happens only via `herdsman merge`.\n\
             - Report: what changed, exact test commands + exit codes, head sha.",
            p.branch, f.integration, f.integration
        )
    };
    let fix = fix.map_or(String::new(), |v| format!("## Review findings to fix (latest FAIL verdict)\n\n{v}\n\n"));
    format!(
        "# Brief — {} ({}#{issue})\n\n{}\n\n## Issue #{issue}: {title}\n\n{body}\n\n{fix}## Output contract\n\n{contract}\n",
        p.agent,
        f.repo,
        role_prompt(&p.role)
    )
}

fn orch_brief(f: &Fleet, fleet: &str, p: &Plan) -> String {
    format!(
        "# Brief — {} (fleet `{fleet}`)\n\n{}\n\n## Fleet\n\n- repo: `{}`\n- local checkout: `{}`\n\
         - integration branch: `{}`\n\nStart with `herdsman board {fleet}`; take work only from `herdsman next {fleet}`.\n",
        p.agent,
        role_prompt("orch"),
        f.repo,
        f.local_path().display(),
        f.integration
    )
}

fn plan_json(ctx: &Ctx, f: &Fleet, p: &Plan) -> Value {
    let local = f.local_path().to_string_lossy().into_owned();
    let wt = p.worktree.to_string_lossy();
    let mut cmds = if p.role == "orch" {
        vec![argv(&["herdr", "workspace", "create", "--cwd", &local, "--label", &p.agent, "--no-focus"])]
    } else {
        vec![
            argv(&["git", "-C", &local, "fetch", "origin", p.base.trim_start_matches("origin/")]),
            argv(&["herdr", "worktree", "create", "--cwd", &local, "--branch", &p.branch, "--base", &p.base])
                .into_iter()
                .chain(argv(&["--path", &wt, "--label", &p.agent, "--no-focus"]))
                .collect(),
        ]
    };
    let herdr = |c: Vec<String>| std::iter::once("herdr".to_string()).chain(c).collect::<Vec<_>>();
    cmds.push(herdr(p.lane_cmd("<pane>")));
    cmds.push(herdr(p.start_cmd(ctx, "<pane>")));
    cmds.extend(p.kick_cmds("<pane>").into_iter().map(herdr));
    json!({
        "agent": p.agent, "role": p.role, "harness": p.harness, "profile": p.profile,
        "model": p.model, "branch": p.branch, "base": p.base, "account": p.account.as_ref().map(|a| &a.0),
        "worktree": p.worktree, "brief": p.brief, "kick": p.kick, "commands": cmds,
    })
}

/// Pick an idle shell pane in the workspace, splitting one off if all are busy.
fn free_pane(r: &dyn Runner, ws: &str, wt: &Path) -> Result<String> {
    let panes = json(r, "herdr", &["pane", "list", "--workspace", ws])?;
    let list = panes["result"]["panes"].as_array().cloned().unwrap_or_default();
    if let Some(p) = list.iter().find(|p| p["agent"].is_null()) {
        return Ok(p["pane_id"].as_str().unwrap_or_default().into());
    }
    let first = list.first().and_then(|p| p["pane_id"].as_str()).context("workspace has no panes")?;
    let wt = wt.to_string_lossy();
    let s = json(r, "herdr", &["pane", "split", first, "--direction", "right", "--cwd", &wt, "--no-focus"])?;
    Ok(s["result"]["pane"]["pane_id"].as_str().context("pane split: no pane_id")?.into())
}

/// Untracked files fleet itself writes into a worktree.
pub const OWN_FILES: [&str; 4] = [".brief.md", "verdict.md", ".role-prompt.md", ".mcp.lane.json"];

/// Why a worktree must not be reused or removed: uncommitted changes (fleet's own
/// `OWN_FILES` aside) or commits that are on no remote.
pub fn dirty(r: &dyn Runner, wt: &Path) -> Option<String> {
    let wt = wt.to_string_lossy();
    let st = match ok(r, "git", &["-C", &wt, "status", "--porcelain"]) {
        Ok(s) => s,
        Err(e) => return Some(format!("unreadable git status ({e:#})")),
    };
    let changed = st.lines().filter(|l| !OWN_FILES.iter().any(|f| l.trim_end() == format!("?? {f}"))).count();
    if changed > 0 {
        return Some(format!("{changed} uncommitted change(s)"));
    }
    match ok(r, "git", &["-C", &wt, "rev-list", "--count", "HEAD", "--not", "--remotes"]) {
        Ok(n) if n.trim() == "0" => None,
        Ok(n) => Some(format!("{} unpushed commit(s)", n.trim())),
        Err(e) => Some(format!("unreadable unpushed count ({e:#})")),
    }
}

#[derive(Default)]
pub struct Opts {
    pub dry_run: bool,
    pub json: bool,
    /// Spawn even when the host is busy (logged as an `override` event).
    pub force: bool,
    /// Kill the live/unfinished lane in the way and spawn anyway (logged).
    pub supersede: bool,
}

pub fn spawn(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (fleet, issue, role): (&str, Option<u64>, &str),
    o: &Opts,
) -> Result<bool> {
    let f = cfg.fleet(fleet)?;
    let p = plan(ctx, cfg, fleet, issue, role)?;
    let mut pj = plan_json(ctx, f, &p);
    if o.dry_run {
        pj["dry_run"] = json!(true);
        print_plan(out, &pj, o.json)?;
        return Ok(true);
    }
    let h = harness::get(&p.harness);
    h.ready(ctx, r, &p)?;
    let wt = p.worktree.to_string_lossy().into_owned();
    let event = |kind: &str, d: Value| emit(&ctx.state, cfg, Event::new(ctx.now, fleet, issue, kind, d));

    // Duplicate guard.
    let events = read(&ctx.state)?;
    let agents = host::agents(r).context("cannot list herdr agents for the duplicate guard")?;
    let live = host::live_in(&agents, &p.agent, &p.worktree);
    // An agent whose start timed out has no name; name it by its pane.
    let names: Vec<&str> =
        live.iter().filter_map(|a| a["name"].as_str().filter(|n| !n.is_empty()).or(a["pane_id"].as_str())).collect();
    let mut dup = vec![];
    if !live.is_empty() {
        dup.push(("LANE_LIVE", format!("live agent(s) {names:?} in {wt}")));
    }
    if let Some(s) = issue.and_then(|n| fold(&events).remove(&(fleet.to_string(), n))).filter(|s| s.unfinished(role)) {
        dup.push(("LANE_LIVE", format!("unfinished {role} lane {:?} for #{} ({})", s.lanes.get(role), s.issue, s.state)));
    }
    let known = events.iter().any(|e| e.kind == "spawn" && e.str("worktree") == Some(&wt));
    if role != "orch" && p.worktree.exists() && !known {
        if let Some(why) = dirty(r, &p.worktree) {
            dup.push(("WORKTREE_DIRTY", format!("{wt} is no fleet lane and has {why}")));
        }
    }
    if let Some((code, msg)) = dup.first().filter(|_| !o.supersede) {
        return Err(refuse(code, format!("{msg}; `--supersede` replaces it")).into());
    }

    // Host-load gate, before anything is created.
    if let Err(why) = host::admit(&host::sample(r), &cfg.limits) {
        if !o.force {
            return Err(refuse("HOST_BUSY", format!("{why}; retry later or pass --force")).into());
        }
        event("override", json!({"code": "HOST_BUSY", "reason": why, "role": role, "agent": p.agent}))?;
    }
    if o.supersede && !dup.is_empty() {
        for a in &live {
            if let Some(pane) = a["pane_id"].as_str() {
                ok(r, "herdr", &["pane", "close", pane])?;
            }
        }
        let why: Vec<&str> = dup.iter().map(|d| d.1.as_str()).collect();
        event("supersede", json!({"role": role, "agent": p.agent, "killed": names, "reason": why.join("; ")}))?;
    }

    let local = f.local_path().to_string_lossy().into_owned();
    let (created, brief_text) = match issue {
        None => {
            let ws = json(r, "herdr", &["workspace", "create", "--cwd", &local, "--label", &p.agent, "--no-focus"])?;
            (ws, orch_brief(f, fleet, &p))
        }
        Some(n) => {
            let iss = crate::gh::spec(ctx, r, f, fleet, n, true)?;
            ok(r, "git", &["-C", &local, "fetch", "origin", p.base.trim_start_matches("origin/")])?;
            let created = if p.worktree.exists() {
                json(r, "herdr", &["worktree", "open", "--cwd", &local, "--path", &wt])?
            } else {
                let a = ["worktree", "create", "--cwd", &local, "--branch", &p.branch, "--base", &p.base];
                let tail = ["--path", &wt, "--label", &p.agent, "--no-focus"];
                json(r, "herdr", &[&a[..], &tail[..]].concat())?
            };
            let (title, body) = (iss["title"].as_str().unwrap_or(""), iss["body"].as_str().unwrap_or(""));
            // A fix round's brief carries the findings of the latest verdict if it was FAIL.
            let last = events.iter().rev().find(|e| e.kind == "verdict" && e.fleet == fleet && e.issue == Some(n));
            let fix = last.filter(|e| role == "impl" && e.str("verdict") == Some("FAIL")).and_then(|e| e.str("body"));
            (created, brief(f, n, &p, title, body, fix))
        }
    };
    let ws = created["result"]["workspace"]["workspace_id"].as_str().context("herdr: no workspace_id")?;
    if let Some(d) = p.brief.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&p.brief, brief_text).context("writing the brief")?;
    h.prepare(ctx, r, &p)?;
    let pane = free_pane(r, ws, &p.worktree)?;
    let lane = p.lane_cmd(&pane);
    ok(r, "herdr", &lane.iter().map(String::as_str).collect::<Vec<_>>())?;
    let start = r.run("herdr", &p.start_cmd(ctx, &pane))?;
    if start.code != 0 {
        // Under load herdr can time out while the harness still comes up: keep it as the lane.
        let timeout = [&start.stderr, &start.stdout]
            .iter()
            .any(|s| serde_json::from_str::<Value>(s.trim()).is_ok_and(|v| v["error"]["code"] == "timeout"));
        if !timeout {
            bail!("herdr agent start failed: {}", start.stderr.trim());
        }
        if harness_in(r, &pane).is_none() {
            let _ = r.run("herdr", &argv(&["pane", "close", &pane]));
            bail!("herdr agent start timed out and no harness came up; closed pane {pane}: {}", start.stderr.trim());
        }
        r.run("herdr", &argv(&["agent", "rename", &pane, &p.agent]))?;
    }
    let confirmed = h.kick(r, &p, &pane)?;
    let mut d = lane_data(&p, &pane, ws);
    d["confirmed"] = json!(confirmed);
    event("spawn", d)?;
    pj["pane"] = json!(pane);
    pj["confirmed"] = json!(confirmed);
    print_plan(out, &pj, o.json)?;
    Ok(confirmed)
}

/// The pane's `herdr pane get` info when a harness (any detected agent) runs in it.
fn harness_in(r: &dyn Runner, pane: &str) -> Option<Value> {
    let v = json(r, "herdr", &["pane", "get", pane]).ok()?;
    Some(v["result"]["pane"].clone()).filter(|p| !p["agent"].is_null())
}

/// The `spawn` event's data for a lane of plan `p` running in `pane`.
fn lane_data(p: &Plan, pane: &str, ws: &str) -> Value {
    json!({"role": p.role, "harness": p.harness, "profile": p.profile, "model": p.model, "agent": p.agent,
        "pane": pane, "workspace": ws, "worktree": p.worktree, "branch": p.branch, "account": p.account.as_ref().map(|a| &a.0)})
}

/// `herdsman adopt`: record a lane already running in `pane` (a spawn that never got
/// logged), naming its agent as planned. Nothing is started or kicked.
pub fn adopt(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (fleet, issue, role): (&str, Option<u64>, &str),
    pane: &str,
) -> Result<()> {
    let p = plan(ctx, cfg, fleet, issue, role)?;
    let info = harness_in(r, pane).with_context(|| format!("no harness running in pane {pane}"))?;
    ok(r, "herdr", &["agent", "rename", pane, &p.agent])?;
    let ws = info["workspace_id"].as_str().or_else(|| pane.split(':').next()).unwrap_or_default();
    let mut d = lane_data(&p, pane, ws);
    d["adopted"] = json!(true);
    emit(&ctx.state, cfg, Event::new(ctx.now, fleet, issue, "spawn", d))?;
    writeln!(out, "adopted {pane} as {} ({} @ {})", p.agent, p.branch, p.worktree.display())?;
    Ok(())
}

fn print_plan(out: &mut dyn Write, pj: &Value, as_json: bool) -> Result<()> {
    if as_json {
        writeln!(out, "{}", serde_json::to_string_pretty(pj)?)?;
    } else {
        writeln!(out, "{} [{} {}] {} @ {}", pj["agent"], pj["harness"], pj["profile"], pj["branch"], pj["worktree"])?;
        for c in pj["commands"].as_array().into_iter().flatten() {
            let words: Vec<&str> = c.as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            writeln!(out, "  $ {}", words.join(" "))?;
        }
        if pj.get("confirmed") == Some(&json!(false)) {
            writeln!(out, "WARNING: prompt not confirmed landed; check the pane")?;
        }
    }
    Ok(())
}
