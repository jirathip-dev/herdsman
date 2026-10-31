//! `herdsman spawn`, `adopt` and `profiles sync` (docs/lanes.md); per-harness parts live in `harness`.
use crate::access;
use crate::config::{Config, Fleet, Role};
use crate::events::{emit, fold, read, Event};
use crate::gate::refused;
use crate::harness::{self, hook_command};
use crate::herdr;
use crate::host;
use crate::runner::{argv, ok, Runner};
use crate::Ctx;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn role_prompt(role: &str) -> &'static str {
    match role {
        "orch" => include_str!("../roles/orch.md"),
        "impl" => include_str!("../roles/impl.md"),
        "design" => include_str!("../roles/design.md"),
        _ => include_str!("../roles/rev.md"),
    }
}

pub fn profile_dir(ctx: &Ctx, profile: &str) -> PathBuf {
    ctx.claude_dir().join(profile)
}

/// Each profile once, at its first (fleet, role) in fleet.toml order.
fn profiles(cfg: &Config) -> Vec<(&str, &Fleet, &str, &Role, String)> {
    let mut done = std::collections::BTreeSet::new();
    let all = cfg.fleet.iter().flat_map(|(name, f)| f.role.iter().map(move |(role, r)| (name.as_str(), f, role.as_str(), r, r.profile_or(role))));
    all.filter(|p| done.insert(p.4.clone())).collect()
}

/// Write every harness's files for every role's profile, so `herdsman set` alone can switch a role's harness.
pub fn profiles_sync(ctx: &Ctx, cfg: &Config, usage: &str, out: &mut dyn Write) -> Result<()> {
    for (_, f, role, r, profile) in profiles(cfg) {
        for h in harness::NAMES {
            harness::get(h).sync(ctx, role, &profile, usage, out)?;
        }
        harness::set_mcp(ctx, &profile, &access::mcp_servers(cfg, f, r)?)?;
    }
    Ok(())
}

/// `profiles sync --check`: each Hermes profile's `hooks list` (run as in a lane) shows its hook `✓ allowed`.
pub fn profiles_check(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<bool> {
    let mut good = true;
    for (name, _, role, _, profile) in profiles(cfg) {
        if !ctx.hermes.join(&profile).join("config.yaml").exists() {
            continue;
        }
        let lane = format!("HERDSMAN_LANE={name}:-:{role}");
        let o = r.run("env", &argv(&[&lane, "hermes", "-p", &profile, "hooks", "list"]))?;
        let cmd = hook_command(ctx, role);
        let allowed = o.code == 0 && o.stdout.lines().any(|l| l.contains(&cmd) && l.contains("✓ allowed"));
        writeln!(out, "{} {profile} ({role})", if allowed { "✓ allowed" } else { "✗ not allowed" })?;
        good &= allowed;
    }
    Ok(good)
}

pub struct Plan {
    pub agent: String,
    pub role: String,
    pub harness: String,
    pub profile: String,
    pub model: Option<String>,
    pub effort: Option<String>,
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
    /// Display title head (`<fleet> · <role> #<issue>`) and the full lane title built from it.
    pub head: String,
    pub title: String,
}

impl Plan {
    /// Exports `HERDSMAN_LANE` (and an account's `CLAUDE_CODE_OAUTH_TOKEN`) in the pane's shell for the harness.
    pub fn lane_cmd(&self, pane: &str) -> Vec<String> {
        let mut c = herdr::run_argv(pane, &["export", &format!("HERDSMAN_LANE='{}'", self.lane)]);
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

/// Longest lane title, in chars.
const TITLE_MAX: usize = 60;

/// `<head> · <short issue title>`, ≤ 60 chars (not bytes): `feat:`/`P0:` prefixes dropped, cut at a word with `…`.
pub fn lane_title(head: &str, issue_title: &str) -> String {
    let mut t = issue_title.trim();
    // `feat(x)!:`, `spawn:`, `P0:` — one ASCII token, an optional scope, then a colon.
    while let Some((pre, rest)) = t.split_once(':') {
        let pre = pre.strip_suffix('!').unwrap_or(pre);
        let tok = pre.split_once('(').map_or(pre, |(a, b)| if b.ends_with(')') { a } else { "" });
        if tok.is_empty() || !tok.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            break;
        }
        t = rest.trim_start();
    }
    if t.is_empty() {
        return head.to_string();
    }
    let room = TITLE_MAX.saturating_sub(head.chars().count() + 3);
    if t.chars().count() <= room {
        return format!("{head} · {t}");
    }
    let cut: String = t.chars().take(room.saturating_sub(1)).collect();
    let cut = match cut.rfind(char::is_whitespace) {
        Some(i) if i > 0 => &cut[..i],
        _ => &cut,
    };
    match cut.trim_end() {
        "" => head.to_string(),
        c => format!("{head} · {c}…"),
    }
}

pub fn plan(ctx: &Ctx, cfg: &Config, fleet: &str, issue: Option<u64>, role: &str) -> Result<Plan> {
    let f = cfg.fleet(fleet)?;
    let r = f.role(role)?;
    let (agent, branch, base, worktree, brief, head) = match issue {
        None => {
            anyhow::ensure!(role == "orch", "only the orch lane runs without an issue");
            let brief = ctx.state.parent().unwrap_or(&ctx.state).join(format!("orch-{fleet}.md"));
            (format!("{fleet}-orch"), f.integration.clone(), String::new(), f.local_path(), brief, format!("{fleet} · orch"))
        }
        Some(issue) => {
            anyhow::ensure!(role != "orch", "the orch lane takes no issue: `herdsman spawn {fleet} - orch`");
            let st = fold(&read(&ctx.state)?).remove(&(fleet.to_string(), issue));
            if st.as_ref().is_some_and(|s| s.state == "merged") {
                return refused("MERGED", format!("{fleet}#{issue} is merged; a merged issue never reopens"));
            }
            let impl_branch = crate::gate::branch_of(st.as_ref(), issue);
            let (agent, branch, base, head) = if role == "rev" {
                let k = st.as_ref().map_or(0, |s| s.rounds) + 1;
                let head = format!("{fleet} · rev #{issue} r{k}");
                (format!("{fleet}-rev-{issue}-r{k}"), format!("rev-{issue}-r{k}"), format!("origin/{impl_branch}"), head)
            } else if role == "design" {
                let head = format!("{fleet} · design #{issue}");
                (format!("{fleet}-design-{issue}"), format!("design-{issue}"), format!("origin/{}", f.integration), head)
            } else {
                (format!("{fleet}-{role}-{issue}"), impl_branch, format!("origin/{}", f.integration), format!("{fleet} · {role} #{issue}"))
            };
            let wt = ctx.worktrees.join(f.repo_name()).join(&branch);
            (agent, branch, base, wt.clone(), wt.join(".brief.md"), head)
        }
    };
    let kick = format!("Read {} and execute it fully.", brief.display());
    let lane = format!("{fleet}:{}:{role}", issue.map_or("-".into(), |n| n.to_string()));
    let account = if r.harness == "claude" { access::pick(ctx, cfg, &read(&ctx.state)?, r)? } else { None };
    let mcp_file = match issue {
        None => brief.with_file_name(format!("orch-{fleet}.mcp.json")),
        Some(_) => worktree.join(".mcp.lane.json"),
    };
    let cached = issue.and_then(|n| crate::gh::cached_spec(ctx, fleet, n));
    let title = lane_title(&head, cached.as_ref().and_then(|v| v["title"].as_str()).unwrap_or(""));
    Ok(Plan {
        agent,
        role: role.into(),
        harness: r.harness.clone(),
        profile: r.profile_or(role),
        model: r.model.clone(),
        effort: r.effort_or(&cfg.limits),
        branch,
        base,
        worktree,
        brief,
        kick,
        lane,
        account,
        mcp: access::mcp_servers(cfg, f, r)?,
        mcp_file,
        head,
        title,
    })
}

/// The issue lane's `.brief.md` (`roles/brief*.md` templates).
fn brief(f: &Fleet, issue: u64, p: &Plan, title: &str, body: &str, fix: Option<&str>) -> String {
    let (branch, integration) = (&p.branch, &f.integration);
    let contract = match p.role.as_str() {
        "rev" => format!(include_str!("../roles/brief-rev.md"), integration = integration, worktree = p.worktree.display()),
        "design" => format!(include_str!("../roles/brief-design.md"), branch = branch, integration = integration),
        _ => format!(include_str!("../roles/brief-impl.md"), branch = branch, integration = integration, issue = issue),
    };
    let fix = fix.map_or(String::new(), |v| format!("## Review findings to fix (latest FAIL verdict)\n\n{v}\n\n"));
    format!(
        include_str!("../roles/brief.md"),
        agent = p.agent, repo = f.repo, issue = issue, role = role_prompt(&p.role),
        title = title, body = body, fix = fix, contract = contract.trim_end()
    )
}

fn orch_brief(f: &Fleet, fleet: &str, p: &Plan) -> String {
    format!(
        include_str!("../roles/brief-orch.md"),
        agent = p.agent, fleet = fleet, role = role_prompt("orch"),
        repo = f.repo, local = f.local_path().display(), integration = f.integration
    )
}

/// The herdr argv that creates the lane's workspace (orch) or worktree.
fn create_argv(local: &str, p: &Plan) -> Vec<String> {
    match p.role.as_str() {
        "orch" => herdr::workspace_create_argv(local, &p.agent),
        _ => herdr::worktree_create_argv(local, &p.branch, &p.base, &p.worktree.to_string_lossy(), &p.agent),
    }
}

fn plan_json(ctx: &Ctx, f: &Fleet, p: &Plan) -> Value {
    let local = f.local_path().to_string_lossy().into_owned();
    let fetch = argv(&["git", "-C", &local, "fetch", "origin", p.base.trim_start_matches("origin/")]);
    let mut cmds = if p.role == "orch" { vec![] } else { vec![fetch] };
    cmds.push(herdr::line(create_argv(&local, p)));
    cmds.push(herdr::line(p.lane_cmd("<pane>")));
    cmds.push(herdr::line(p.start_cmd(ctx, "<pane>")));
    cmds.extend(p.kick_cmds("<pane>").into_iter().map(herdr::line));
    json!({
        "agent": p.agent, "role": p.role, "harness": p.harness, "profile": p.profile,
        "model": p.model, "effort": p.effort, "branch": p.branch, "base": p.base, "account": p.account.as_ref().map(|a| &a.0),
        "worktree": p.worktree, "brief": p.brief, "kick": p.kick, "title": p.title, "commands": cmds,
    })
}

/// Lane scratch: untracked files fleet itself writes into a worktree (some gitignored). Never dirty work.
pub const OWN_FILES: [&str; 6] = [".brief.md", ".addendum.md", ".role-prompt.md", ".mcp.lane.json", "verdict.md", ".review-brief.md"];

/// `git status --porcelain` lines that are not lane scratch.
pub fn changes(st: &str) -> usize {
    st.lines().filter(|l| !OWN_FILES.iter().any(|f| l.trim_end() == format!("?? {f}"))).count()
}

/// Why a worktree must not be reused or removed: uncommitted changes (`OWN_FILES` aside) or unpushed commits.
pub fn dirty(r: &dyn Runner, wt: &Path) -> Option<String> {
    let wt = wt.to_string_lossy();
    let st = match ok(r, "git", &["-C", &wt, "status", "--porcelain"]) {
        Ok(s) => s,
        Err(e) => return Some(format!("unreadable git status ({e:#})")),
    };
    let changed = changes(&st);
    if changed > 0 {
        return Some(format!("{changed} uncommitted change(s)"));
    }
    match ok(r, "git", &["-C", &wt, "rev-list", "--count", "HEAD", "--not", "--remotes"]) {
        Ok(n) if n.trim() == "0" => None,
        Ok(n) => Some(format!("{} unpushed commit(s)", n.trim())),
        Err(e) => Some(format!("unreadable unpushed count ({e:#})")),
    }
}

/// `spawn`'s flags.
#[derive(Default, clap::Args)]
pub struct Opts {
    /// Print the plan; change nothing
    #[arg(long)]
    pub dry_run: bool,
    /// Machine-readable output
    #[arg(long)]
    pub json: bool,
    /// Spawn although the host is busy (logs an `override` event)
    #[arg(long)]
    pub force: bool,
    /// Kill the live agent / unfinished lane in the way, then spawn (logged)
    #[arg(long)]
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
    if f.paused && matches!(role, "impl" | "rev") {
        return refused("PAUSED", format!("{fleet} is paused; `herdsman resume {fleet}` first"));
    }
    let mut p = plan(ctx, cfg, fleet, issue, role)?;
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
        return refused(code, format!("{msg}; `--supersede` replaces it"));
    }

    if let Some(n) = issue {
        lane_admit(f, &events, fleet, n, role)?;
    }

    // Host-load gate, before anything is created.
    if let Err(why) = host::admit(&host::sample(r), &cfg.limits) {
        if !o.force {
            return refused("HOST_BUSY", format!("{why}; retry later or pass --force"));
        }
        event("override", json!({"code": "HOST_BUSY", "reason": why, "role": role, "agent": p.agent}))?;
    }
    if o.supersede && !dup.is_empty() {
        for a in &live {
            if let Some(pane) = a["pane_id"].as_str() {
                herdr::close(r, pane)?;
            }
        }
        let why: Vec<&str> = dup.iter().map(|d| d.1.as_str()).collect();
        event("supersede", json!({"role": role, "agent": p.agent, "killed": names, "reason": why.join("; ")}))?;
    }

    let local = f.local_path().to_string_lossy().into_owned();
    let (created, brief_text) = match issue {
        None => (herdr::json(r, &create_argv(&local, &p))?, orch_brief(f, fleet, &p)),
        Some(n) => {
            let iss = crate::gh::spec(ctx, r, f, fleet, n, true)?;
            ok(r, "git", &["-C", &local, "fetch", "origin", p.base.trim_start_matches("origin/")])?;
            let open = herdr::worktree_open_argv(&local, &wt);
            let created = herdr::json(r, &if p.worktree.exists() { open } else { create_argv(&local, &p) })?;
            let (title, body) = (iss["title"].as_str().unwrap_or(""), iss["body"].as_str().unwrap_or(""));
            p.title = lane_title(&p.head, title);
            // A fix round's brief carries the findings of the latest verdict if it was FAIL.
            let last = events.iter().rev().find(|e| e.is("verdict", fleet, n));
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
    let pane = herdr::free_pane(r, ws, &p.worktree)?;
    pj = plan_json(ctx, f, &p);
    herdr::ok(r, &p.lane_cmd(&pane))?;
    let start = herdr::exec(r, &p.start_cmd(ctx, &pane))?;
    if start.code != 0 {
        // Under load herdr can time out while the harness still comes up: keep it as the lane.
        let timeout = [&start.stderr, &start.stdout]
            .iter()
            .any(|s| serde_json::from_str::<Value>(s.trim()).is_ok_and(|v| v["error"]["code"] == "timeout"));
        if !timeout {
            bail!("herdr agent start failed: {}", start.stderr.trim());
        }
        if herdr::harness_in(r, &pane).is_none() {
            let _ = herdr::close(r, &pane);
            bail!("herdr agent start timed out and no harness came up; closed pane {pane}: {}", start.stderr.trim());
        }
        herdr::exec(r, &herdr::rename_argv(&pane, &p.agent))?;
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

/// SERIAL_BUSY (another `--serial` issue in flight) and CAP (`max_impl` / `max_rev`).
fn lane_admit(f: &Fleet, events: &[Event], fleet: &str, issue: u64, role: &str) -> Result<()> {
    let st = fold(events);
    let others: Vec<_> = st.values().filter(|s| s.fleet == fleet && s.issue != issue).collect();
    let building = |s: &str| s == "impl" || s.starts_with("fix");
    if role == "impl" {
        let serial = crate::queue::serial(events, fleet);
        if serial.contains(&issue) {
            let busy = others.iter().find(|s| {
                serial.contains(&s.issue) && (building(&s.state) || s.state.starts_with("review") || s.state == "ready")
            });
            if let Some(s) = busy {
                return refused("SERIAL_BUSY", format!("serial #{} is unmerged ({}); retry after it merges", s.issue, s.state));
            }
        }
    }
    let (cap, n) = match role {
        "impl" => (f.max_impl, others.iter().filter(|s| building(&s.state)).count()),
        "rev" => (f.max_rev, others.iter().filter(|s| s.state.starts_with("review")).count()),
        _ => (None, 0),
    };
    if let Some(cap) = cap.filter(|&c| n >= c) {
        return refused("CAP", format!("{n} {role} lane(s) open in {fleet}, max_{role} = {cap}"));
    }
    Ok(())
}

/// The `spawn` event's data for a lane of plan `p` running in `pane`.
fn lane_data(p: &Plan, pane: &str, ws: &str) -> Value {
    json!({"role": p.role, "harness": p.harness, "profile": p.profile, "model": p.model, "effort": p.effort, "agent": p.agent,
        "pane": pane, "workspace": ws, "worktree": p.worktree, "branch": p.branch, "account": p.account.as_ref().map(|a| &a.0)})
}

/// `herdsman adopt`: log the lane running in `pane` and name its agent as planned; nothing is started.
pub fn adopt(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (fleet, issue, role): (&str, Option<u64>, &str),
    pane: &str,
) -> Result<()> {
    let p = plan(ctx, cfg, fleet, issue, role)?;
    let info = herdr::harness_in(r, pane).with_context(|| format!("no harness running in pane {pane}"))?;
    herdr::ok(r, &herdr::rename_argv(pane, &p.agent))?;
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
