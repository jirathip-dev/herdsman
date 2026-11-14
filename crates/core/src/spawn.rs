use crate::access;
use crate::config::{expand, Config, Fleet, Role};
use crate::events::{emit, fold, last_spawn, plain, working, Event};
use crate::gate::refused;
use crate::host;
use crate::providers;
use crate::runner::{argv, ok, Runner};
use crate::term;
use crate::Ctx;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn role_prompt(role: &str) -> &'static str {
    match role {
        "impl" => include_str!("../roles/impl.md"),
        "design" => include_str!("../roles/design.md"),
        "architect" => include_str!("../roles/architect.md"),
        "orchestrator" => include_str!("../roles/orchestrator.md"),
        "lesson" => include_str!("../roles/lesson.md"),
        "verify" => include_str!("../roles/verify.md"),
        _ => include_str!("../roles/rev.md"),
    }
}

fn lane_prompt(f: &Fleet, role: &str) -> String {
    let extra =
        f.role.get(role).and_then(|r| r.prompt.as_deref()).map(|p| {
            std::fs::read_to_string(expand(p)).unwrap_or_else(|e| format!("(prompt file {p} unreadable: {e})"))
        });
    let base = role_prompt(prompt_kind(f, role));
    extra.map_or_else(|| base.into(), |x| format!("{}\n\n{}", base.trim_end(), x.trim_end()))
}

fn prompt_kind<'a>(f: &'a Fleet, role: &'a str) -> &'a str {
    let own = role == "lesson" || role == "verify" && f.family(role) == "rev";
    if own {
        role
    } else {
        f.family(role)
    }
}

pub(crate) fn profiles(cfg: &Config) -> Vec<(&str, &Fleet, &str, &Role, String)> {
    let mut done = std::collections::BTreeSet::new();
    let all = cfg.fleet.iter().flat_map(|(name, f)| {
        f.role
            .iter()
            .map(move |(role, r)| (name.as_str(), f, role.as_str(), r, r.profile_or(role)))
    });
    all.filter(|p| done.insert(p.4.clone())).collect()
}

/// Write every harness's files for every role's profile, so `drover set` alone can switch a role's harness; a generated profile no role names any more (#155: a retired role's) is removed.
pub fn profiles_sync(ctx: &Ctx, cfg: &Config, usage: &str, out: &mut dyn Write) -> Result<()> {
    let all = profiles(cfg);
    for (_, f, role, r, profile) in &all {
        for h in providers::known() {
            providers::get(h).sync(ctx, f.family(role), prompt_kind(f, role), profile, usage, out)?;
        }
        providers::set_mcp(ctx, profile, &access::mcp_servers(cfg, f, r)?)?;
    }
    let root = providers::profiles_root(ctx);
    for d in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let (name, dir) = (d.file_name().to_string_lossy().into_owned(), d.path());
        if dir.join("role.md").exists() && !all.iter().any(|p| p.4 == name) {
            std::fs::remove_dir_all(&dir)?;
            writeln!(out, "removed stale {}", dir.display())?;
        }
    }
    Ok(())
}

/// `profiles sync --check`: no stray hook, and each provider's profiles allow their hook (`Provider::hooks_allowed`).
pub fn profiles_check(ctx: &Ctx, cfg: &Config, r: &dyn Runner, out: &mut dyn Write) -> Result<bool> {
    let stray = stray_hooks(ctx, cfg);
    let mut good = stray.is_empty();
    for s in stray {
        writeln!(out, "✗ {s} → drover profiles sync")?;
    }
    for p in providers::lanes() {
        good &= p.hooks_allowed(ctx, cfg, r, out)?;
    }
    Ok(good)
}

/// #737: each synced profile's drover hook whose program is not the running binary: `<profile> hook runs <exe>`.
pub fn stray_hooks(ctx: &Ctx, cfg: &Config) -> Vec<String> {
    let mut stray = vec![];
    for (_, _, _, _, profile) in profiles(cfg) {
        let path = providers::profiles_root(ctx).join(&profile).join("settings.json");
        let settings: Value = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let pre = settings["hooks"]["PreToolUse"].as_array().into_iter().flatten();
        for h in pre.flat_map(|m| m["hooks"].as_array().into_iter().flatten()) {
            let cmd = h["command"].as_str().unwrap_or_default();
            if let Some(exe) = providers::hook_program(cmd).filter(|_| !providers::runs_this(ctx, cmd)) {
                stray.push(format!("{profile} hook runs {exe}, not {}", ctx.exe));
            }
        }
    }
    stray
}

pub struct Plan {
    pub agent: String,
    pub role: String,
    /// #173: the pipeline stage it runs and the role's kind (`impl`, `rev` for a check, `design`).
    pub stage: String,
    pub family: &'static str,
    pub harness: String,
    pub profile: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub branch: String,
    pub base: String,
    pub worktree: PathBuf,
    pub brief: PathBuf,
    pub kick: String,
    /// `DROVER_LANE` value (`<fleet>:<issue>:<role>`); arms `drover hook` in the lane.
    pub lane: String,
    /// `[accounts]` lanes: the account name and (#683) the env it signs in with (`access::account_env`); that env is empty on a remote host (its lanes sign in through the main host's token proxy, #637).
    pub account: Option<access::Account>,
    pub env: std::collections::BTreeMap<String, String>,
    pub port: u16,
    /// The only MCP servers the lane gets, and the lane-local file a harness reads them from.
    pub mcp: serde_json::Map<String, Value>,
    pub mcp_file: PathBuf,
    /// Display title head (`<fleet> · <role> #<issue>`) and the full lane title built from it.
    pub head: String,
    pub title: String,
    /// #302: the card's `on <entry> (<entries> resting until` and that time, when a `fallback` entry carries on.
    pub resting: Option<(String, u64)>,
    /// #355: an orchestrator's harness session id, and whether to resume it (`Harness::resumes`).
    pub session: Option<(String, bool)>,
    /// #637: a remote ACP lane's proxy key, its only credential (the secrets file's `Proxy::auth_token_env`).
    pub lane_key: Option<String>,
    /// #707: a local lane's `token-file:` account: the env var and the file `deliver` exports it from.
    pub token: Option<(String, PathBuf)>,
}

/// #979: a build or check lane's shell and all it starts under macOS background `QoS`, else `nice 10` and idle I/O.
pub const BACKGROUND: &str = "&& if [ -x /usr/sbin/taskpolicy ]; then /usr/sbin/taskpolicy -b -p $$; else renice -n 10 -p $$ >/dev/null; ionice -c3 -p $$ 2>/dev/null; true; fi";

impl Plan {
    /// #979: build and check lanes run in the background; design, architect and orchestrator lanes do not.
    pub fn qos(&self) -> Option<&'static str> {
        matches!(self.family, "impl" | "rev").then_some("background")
    }

    /// Exports `DROVER_LANE`, `DROVER_PORT` and the fleet's `env`; #441: sources the secrets `deliver` wrote.
    pub fn lane_cmd(&self, pane: &str) -> Vec<String> {
        let (lane, port) = (format!("DROVER_LANE='{}'", self.lane), self.port.to_string());
        let mut c = term::run_argv(pane, &["export", &lane, &format!("DROVER_PORT={port}")]);
        let v = |v: &str| {
            expand(v)
                .display()
                .to_string()
                .replace("$DROVER_PORT", &port)
                .replace("$HERDSMAN_PORT", &port)
        };
        let plain = self.env.iter().filter(|(_, x)| !x.starts_with("secret:"));
        c.extend(plain.map(|(k, x)| format!("{k}='{}'", v(x))));
        c.extend(self.account.iter().map(|a| format!("DROVER_ACCOUNT='{}'", a.0)));
        let acct = self.account.iter().flat_map(|a| &a.1);
        c.extend(acct.map(|(k, d)| format!("{k}='{}'", d.replace('\'', r"'\''"))));
        c.extend(providers::get(&self.harness).env().iter().map(|v| String::from(*v)));
        if let (None, providers::Credential::Account(providers::Accounts { token_env: Some(v), .. })) =
            (&self.account, providers::get(&self.harness).credential())
        {
            c.push(format!("&& unset {v}"));
        }
        if !self.secrets().is_empty() || self.lane_key.is_some() || self.token.is_some() {
            let f = self.secrets_file().display().to_string().replace('\'', r"'\''");
            c.push(format!("&& . '{f}'"));
        }
        c.extend(self.qos().map(|_| BACKGROUND.to_string()));
        c
    }

    fn secrets(&self) -> Vec<(String, &str, String)> {
        let fleet = self.lane.split(':').next().unwrap_or_default();
        let env = self
            .env
            .iter()
            .filter_map(|(k, x)| Some((k.clone(), fleet, x.strip_prefix("secret:")?.into())));
        env.collect()
    }

    /// The file `deliver` writes beside the worktree, on the lane's host.
    pub fn secrets_file(&self) -> PathBuf {
        PathBuf::from(format!("{}.env", self.worktree.display()))
    }

    /// #592: a remote lane's hook rules, beside its worktree on its host.
    pub fn rules_file(&self) -> PathBuf {
        PathBuf::from(format!("{}.rules.json", self.worktree.display()))
    }

    pub fn start_cmd(&self, ctx: &Ctx, pane: &str) -> Vec<String> {
        providers::get(&self.harness).start_cmd(ctx, self, pane)
    }

    pub fn kick_cmds(&self, pane: &str) -> Vec<Vec<String>> {
        providers::get(&self.harness).kick_cmds(self, pane)
    }

    fn at(&mut self, root: &Path, f: &Fleet) {
        self.env.extend(shared_target(root, f, &self.agent));
        self.worktree = root.join(f.repo_name()).join(&self.branch);
        self.brief = self.worktree.join(".brief.md");
        self.mcp_file = self.worktree.join(".mcp.lane.json");
        self.kick = format!("Read {} and execute it fully.", self.brief.display());
    }
}

/// #707: the lane's secrets read from 0600 files on the main host (`<config>/secrets/<NAME>`, its `token-file:`) and written 0600 beside its worktree on its host (over the ssh channel's stdin to a remote one), never in argv, a pane or the log; the guard removes the file when dropped.
pub fn deliver<'a>(ctx: &Ctx, on: &'a crate::hosts::On<'a>, cfg: &Config, p: &Plan) -> Result<Delivered<'a>> {
    let mut text = String::new();
    let named = p
        .secrets()
        .into_iter()
        .map(|(var, _, n)| (var, crate::secret::path(ctx, &n), n));
    let token = p
        .token
        .iter()
        .map(|(var, f)| (var.clone(), f.clone(), "token-file".into()));
    for (var, f, name) in named.chain(token) {
        let v = crate::secret::file(&f)?.or_else(|| cfg.legacy_secret(&name));
        let why = || {
            format!(
                "{var}: secret {name} is not in {} (`drover secret request`)",
                f.display()
            )
        };
        text += &format!("export {var}='{}'\n", v.with_context(why)?.replace('\'', r"'\''"));
    }
    text += &p.lane_key.as_ref().map_or_else(String::new, |k| {
        let var = crate::providers::proxied().map_or("", |px| px.auth_token_env);
        format!("export {var}='{k}'\n")
    });
    let file = (!text.is_empty()).then(|| p.secrets_file());
    if let Some(f) = &file {
        on.write_private(f, &text)?;
    }
    Ok(Delivered(on, file))
}

/// The secrets file `deliver` wrote; removed on drop (the lane's shell has read it by then).
pub struct Delivered<'a>(&'a crate::hosts::On<'a>, Option<PathBuf>);

impl Drop for Delivered<'_> {
    fn drop(&mut self) {
        if let Some(f) = &self.1 {
            let _ = self.0.remove(f);
        }
    }
}

const TITLE_MAX: usize = 60;

/// `<head> · <short issue title>`, ≤ 60 chars (not bytes): `feat:`/`P0:` prefixes dropped, cut at a word with `…`.
pub fn lane_title(head: &str, issue_title: &str) -> String {
    let mut t = issue_title.trim();
    while let Some((pre, rest)) = t.split_once(':') {
        let pre = pre.strip_suffix('!').unwrap_or(pre);
        let tok = pre
            .split_once('(')
            .map_or(pre, |(a, b)| if b.ends_with(')') { a } else { "" });
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

/// #187: `<fleet>/<branch>` hashed (FNV-1a) into `range` (default 41000-41999); a `taken` port takes the next free one.
pub fn lane_port(range: Option<&str>, key: &str, taken: &[u16]) -> u16 {
    let r = range.and_then(|r| r.split_once('-'));
    let r = r.and_then(|(a, b)| Some((a.trim().parse::<u16>().ok()?, b.trim().parse::<u16>().ok()?)));
    let (lo, hi) = r.filter(|(a, b)| a <= b).unwrap_or((41000, 41999));
    let n = u32::from(hi - lo) + 1;
    let fnv = |h: u32, b: u8| (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    let h = key.bytes().fold(0x811c_9dc5, fnv) % n;
    let at = |i: u32| lo + u16::try_from((h + i) % n).unwrap_or_default();
    (0..n).map(at).find(|p| !taken.contains(p)).unwrap_or(at(0))
}

pub fn plan(ctx: &Ctx, cfg: &Config, fleet: &str, issue: u64, role: &str) -> Result<Plan> {
    let f = cfg.fleet(fleet)?;
    let (r, resting) = access::fallback(cfg, &working(&ctx.state)?, f.role(role)?, ctx.now)?;
    let r = &r;
    let (agent, branch, base, worktree, brief, head) = {
        let ev = working(&ctx.state)?;
        let st = fold(&ev).remove(&(fleet.to_string(), issue));
        if st.as_ref().is_some_and(|s| s.state == "merged") {
            return refused(
                "MERGED",
                format!("{fleet}#{issue} is merged; a merged issue never reopens"),
            );
        }
        let impl_branch = crate::gate::branch_of(st.as_ref(), issue);
        let (agent, branch, base, head) = if f.family(role) == "rev" {
            let rounds = st.as_ref().and_then(|s| s.stage_rounds.get(f.flow().stage_of(role)));
            let k = rounds.copied().unwrap_or_default() + 1;
            let head = format!("{fleet} · {role} #{issue} r{k}");
            (
                format!("{fleet}-{role}-{issue}-r{k}"),
                format!("{role}-{issue}-r{k}"),
                format!("origin/{impl_branch}"),
                head,
            )
        } else if crate::events::STAGES.contains(&role) {
            let head = format!("{fleet} · {role} #{issue}");
            let pre = st
                .as_ref()
                .is_none_or(|s| crate::events::DESIGN.contains(&s.state.as_str()));
            let pre = pre
                && crate::queue::replay(&ev, fleet)
                    .iter()
                    .any(|i| i.issue == issue && (i.design || i.architect));
            (
                format!("{fleet}-{role}-{issue}"),
                if pre { impl_branch } else { format!("{role}-{issue}") },
                format!("origin/{}", f.integration),
                head,
            )
        } else {
            (
                format!("{fleet}-{role}-{issue}"),
                impl_branch,
                format!("origin/{}", f.integration),
                format!("{fleet} · {role} #{issue}"),
            )
        };
        let wt = ctx.worktrees.join(f.repo_name()).join(&branch);
        (agent, branch, base, wt.clone(), wt.join(".brief.md"), head)
    };
    let kick = format!("Read {} and execute it fully.", brief.display());
    let lane = format!("{fleet}:{issue}:{role}");
    let account = access::pick(ctx, cfg, &working(&ctx.state)?, r)?;
    let mcp_file = worktree.join(".mcp.lane.json");
    let events = working(&ctx.state)?;
    let st = fold(&events);
    let open = st.values().filter(|s| !s.done()).flat_map(|s| s.lanes.values());
    let taken: Vec<u16> = open
        .filter_map(|a| last_spawn(&events, a))
        .filter(|e| e.fleet != fleet || e.str("branch") != Some(&branch))
        .filter_map(|e| e.data.get("port")?.as_u64()?.try_into().ok())
        .collect();
    let port = lane_port(f.port_range.as_deref(), &format!("{fleet}/{branch}"), &taken);
    let title = lane_title(&head, &crate::gh::cached_title(ctx, fleet, issue).unwrap_or_default());
    let env = shared_target(&ctx.worktrees, f, &agent)
        .chain(f.env.clone())
        .chain(f.shared_cache.clone())
        .collect();
    Ok(Plan {
        agent,
        role: role.into(),
        stage: f.flow().stage_of(role).into(),
        family: f.family(role),
        harness: r.harness.clone(),
        profile: r.profile_or(f.role_key(role)),
        model: r.model.clone(),
        effort: r.effort_or(&cfg.limits),
        branch,
        base,
        worktree,
        brief,
        kick,
        lane,
        account,
        env,
        port,
        mcp: access::mcp_servers(cfg, f, r)?,
        mcp_file,
        head,
        title,
        resting,
        session: None,
        lane_key: None,
        token: None,
    })
}

fn shared_target<'a>(root: &Path, f: &'a Fleet, agent: &str) -> impl Iterator<Item = (String, String)> + 'a {
    let b = root.join(crate::roots::BUILD);
    let d = b.join(agent).display().to_string();
    let s = b.join(SCRATCH).join(agent).display().to_string();
    let vars = [("CARGO_TARGET_DIR".into(), d), ("DROVER_SCRATCH_TARGET".into(), s)];
    vars.into_iter()
        .filter(|v| !f.env.contains_key(&v.0) && !f.shared_cache.contains_key(&v.0))
}

/// #536: the folder under a root's `.build` holding each lane's scratch.
pub const SCRATCH: &str = ".scratch";

const PUSHBACK: &str = "If a finding is wrong or out of scope, say so in your report with file:line proof; \
that is not a fix. The next reviewer decides.";

fn brief(
    f: &Fleet,
    issue: u64,
    p: &Plan,
    title: &str,
    body: &str,
    (fix, rules, repro, play): (Option<&str>, Option<&str>, bool, Option<String>),
    recipe: &crate::design::Recipe,
) -> String {
    let (branch, integration) = (&p.branch, &f.integration);
    let contract = match p.family {
        "rev" => format!(
            include_str!("../roles/brief-rev.md"),
            integration = integration,
            worktree = p.worktree.display(),
            form = f.form_categories.as_deref().unwrap_or_default().join(", ")
        ),
        "design" => format!(
            include_str!("../roles/brief-design.md"),
            branch = branch,
            integration = integration,
            medium = recipe.medium,
            paths = (recipe.folders.iter())
                .map(|p| format!("`{p}/`"))
                .collect::<Vec<_>>()
                .join(", "),
            render = recipe
                .render
                .as_ref()
                .map_or("render a PNG of each page or view".into(), |c| format!("run `{c}`")),
            sizes = recipe.sizes,
            clip = [
                "",
                " The design moves: also a short clip of it (`clip.gif` or `clip.mp4`) beside the images."
            ][usize::from(recipe.clip)]
        ),
        "architect" => {
            let hot = ["", include_str!("../roles/brief-hotspots-architect.md")];
            format!(
                include_str!("../roles/brief-architect.md"),
                branch = branch,
                integration = integration
            )
            .trim_end()
            .to_string()
                + "\n"
                + hot[usize::from(title.starts_with(crate::hotspots::TITLE))]
        }
        _ => format!(
            include_str!("../roles/brief-impl.md"),
            branch = branch,
            integration = integration,
            issue = issue
        ),
    };
    let steps = match (repro, p.family, p.stage.as_str()) {
        (false, ..) | (_, "design" | "architect", _) => "",
        (_, "rev", "verify") => include_str!("../roles/brief-repro-verify.md"),
        (_, "rev", _) => include_str!("../roles/brief-repro-rev.md"),
        _ => include_str!("../roles/brief-repro-impl.md"),
    };
    let play = match (play.as_deref(), p.family, p.stage.as_str()) {
        (None, ..) | (_, "design", _) | (_, "rev", "verify") => "",
        (Some("perf"), "rev", _) => include_str!("../roles/brief-perf-rev.md"),
        (Some("perf"), ..) => include_str!("../roles/brief-perf-impl.md"),
        (_, "rev", _) => include_str!("../roles/brief-refactor-rev.md"),
        _ => include_str!("../roles/brief-refactor-impl.md"),
    };
    let contract = contract.trim_end().to_string() + "\n" + steps + play;
    let head = "## Review findings to fix (latest FAIL verdict)";
    let fix = fix.map_or(String::new(), |v| format!("{head}\n\n{PUSHBACK}\n\n{v}\n\n"));
    let rules = rules.map_or(String::new(), |r| {
        format!("## Repo rules (.drover/rules.md)\n\n{}\n\n", r.trim_end())
    });
    let shared = include_str!("../rules/shared.md").trim_end();
    let rules = format!("## Shared rules (rules/shared.md)\n\n{shared}\n\n{rules}");
    format!(
        include_str!("../roles/brief.md"),
        agent = p.agent,
        repo = f.repo,
        issue = issue,
        role = lane_prompt(f, &p.role),
        title = title,
        body = body,
        fix = fix,
        rules = rules,
        contract = contract.trim_end()
    )
}

fn labels(p: &Plan, acp: Option<(&Fleet, u32)>) -> Vec<(&'static str, String)> {
    let issue = p.lane.split(':').nth(1).unwrap_or_default().to_string();
    let fleet = p.lane.split(':').next().unwrap_or_default().to_string();
    let Some((f, round)) = acp else {
        return vec![
            ("fleet", fleet),
            ("issue", issue),
            ("role", p.role.clone()),
            ("harness", p.harness.clone()),
        ];
    };
    let role = if p.family == "impl" && round > 0 {
        "fix"
    } else {
        p.family
    }
    .to_string();
    let url = format!("https://github.com/{}/issues/{issue}", f.repo);
    vec![
        ("fleet", fleet.clone()),
        ("issue", issue.clone()),
        ("role", role.clone()),
        ("harness", p.harness.clone()),
        ("source", "drover".into()),
        ("group", f.repo_name().into()),
        ("item", format!("#{issue}")),
        ("round", round.to_string()),
        ("url", url),
        ("drover.fleet", fleet),
        ("drover.issue", issue),
        ("drover.role", role),
        ("drover.round", round.to_string()),
    ]
}

fn round(events: &[Event], fleet: &str, issue: u64, p: &Plan) -> u32 {
    let k = p.agent.rsplit_once("-r").and_then(|(_, k)| k.parse().ok());
    let n = fold(events).remove(&(fleet.into(), issue)).map_or(0, |s| s.rounds);
    k.filter(|_| p.family == "rev").unwrap_or(n)
}

/// #565: why drover's hook would not guard ACP agent `id` running `harness` on `profile` for `family` (with an account; #637: on a remote host, its profile checked there instead); empty when it would. #633: an attached cooee conductor's `guarded` too.
pub fn unguarded(
    ctx: &Ctx,
    (profile, family, h): (&str, &str, &str),
    id: &str,
    (account, remote): (bool, bool),
) -> String {
    let settings: Value = std::fs::read_to_string(providers::profiles_root(ctx).join(profile).join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let pre = settings["hooks"]["PreToolUse"].as_array().into_iter().flatten();
    let mut hooks = pre.flat_map(|m| m["hooks"].as_array().into_iter().flatten());
    if providers::get(h).acp() != Some(id) {
        format!("ACP agent {id} runs no {h} hook")
    } else if !remote && !hooks.any(|h| h["command"].as_str().is_some_and(|c| providers::guards(ctx, c, family))) {
        format!("profile {profile} has no drover PreToolUse hook; run `drover profiles sync`")
    } else if !account && remote {
        "an ACP lane on another host signs in with its role's [accounts] account, through the main host's proxy".into()
    } else {
        String::new()
    }
}

/// #565: an ACP lane's adapter (its agent's command in `shed agents`), run by `sh` after the lane's exports and secrets file, with its account's env as its sign-in (#683), its profile's settings and role as the worktree's local project settings and memory, and no `$SHED` (its state is the session's, not the hook's). Refused `ACP_UNGUARDED` when drover's hook would not guard it (#508: fail closed).
pub fn acp_cmd(ctx: &Ctx, r: &dyn Runner, p: &Plan, id: &str, remote: Option<&str>) -> Result<Vec<String>> {
    let dir = providers::profiles_root(ctx).join(&p.profile);
    let home = |a: Vec<String>| crate::hosts::rehome(a, remote);
    let why = unguarded(
        ctx,
        (&p.profile, p.family, &p.harness),
        id,
        (
            p.account
                .as_ref()
                .is_some_and(|a| !a.1.is_empty() || p.lane_key.is_some()),
            remote.is_some(),
        ),
    );
    if !why.is_empty() {
        return refused("ACP_UNGUARDED", format!("{why}; the lane does not start"));
    }
    let agents: Value = serde_json::from_str(&crate::shed::cli(r, &["agents", "--json"])?.stdout)?;
    let agent = agents.as_array().into_iter().flatten().find(|a| a["id"] == id);
    let cmd: Vec<String> = agent
        .and_then(|a| serde_json::from_value(a["cmd"].clone()).ok())
        .with_context(|| format!("ACP agent {id} is not installed; `shed agents add {id}`"))?;
    let mut words = home(p.lane_cmd("")[3..].to_vec());
    let at = words.iter().position(|w| w.starts_with("&&")).unwrap_or(words.len());
    let q = |v: &str| format!("'{}'", v.replace('\'', r"'\''"));
    let dir = PathBuf::from(home(vec![dir.display().to_string()]).remove(0));
    let env = providers::get(&p.harness).acp_env(p.model.as_deref(), p.effort.as_deref());
    let own: Vec<String> = env.iter().map(|(k, v)| format!("{k}={}", q(v))).collect();
    words.splice(at..at, own);
    let (set, role) = (dir.join("settings.json"), dir.join("role.md"));
    let (set, role) = (q(&set.to_string_lossy()), q(&role.to_string_lossy()));
    let profile = providers::get(&p.harness).acp_profile(&set, &role);
    let line = words.join(" ") + &profile + " && unset SHED BERTH && exec \"$@\"";
    Ok([argv(&["sh", "-c", &line, "sh"]), cmd].concat())
}

fn remote_acp(
    ctx: &Ctx,
    cfg: &Config,
    on: &crate::hosts::On,
    p: &mut Plan,
    (pl, id): (&crate::hosts::Placed, &str),
    dry: bool,
) -> Result<()> {
    let home = pl.home.as_deref().unwrap_or_default();
    if !dry {
        let exe = crate::hosts::rehome(vec![ctx.exe.clone()], Some(home)).remove(0);
        ok(
            on,
            &exe,
            &["profiles", "sync", "--profile", &p.profile, "--role", p.family],
        )?;
    }
    if let Some(why) = crate::hosts::acp_guard(ctx, on, home, (&p.profile, p.family, id)) {
        crate::hosts::sick(ctx, &pl.name, &format!("✗ acp guard {} — {why}", p.profile));
        return refused(
            "ACP_UNGUARDED",
            format!(
                "{}: {why}; the lane does not start, the next placement skips the host",
                pl.name
            ),
        );
    }
    if let Some(px) = crate::providers::proxied() {
        p.env.insert(px.base_url_env.into(), crate::proxy::url(cfg));
    }
    p.lane_key = Some(crate::proxy::new_key()?);
    Ok(())
}

fn checked_out(r: &dyn Runner, local: &str, branch: &str) -> Option<String> {
    let list = ok(r, "git", &["-C", local, "worktree", "list", "--porcelain"]).ok()?;
    let want = format!("branch refs/heads/{branch}");
    let held = list.split("\n\n").skip(1).find(|b| b.lines().any(|l| l == want))?;
    held.lines().next()?.strip_prefix("worktree ").map(String::from)
}

fn worktree_add(local: &str, p: &Plan, have: bool) -> Vec<String> {
    let wt = p.worktree.to_string_lossy();
    if have {
        argv(&["-C", local, "worktree", "add", &wt, &p.branch])
    } else {
        argv(&["-C", local, "worktree", "add", "-b", &p.branch, &wt, &p.base])
    }
}

fn plan_json(ctx: &Ctx, p: &Plan, on: &crate::hosts::On, (local, reopen): (&str, bool), rt: Runtime) -> Value {
    let (home, bt, labels, acp) = rt;
    let sent = |prog: &str, a: Vec<String>| {
        let (prog, a) = on.wrap(prog, &a);
        [vec![prog], a].concat()
    };
    let home = |a| crate::hosts::rehome(a, home);
    let mut cmds = vec![sent(
        "git",
        argv(&["-C", local, "fetch", "origin", p.base.trim_start_matches("origin/")]),
    )];
    cmds.extend((!reopen).then(|| sent("git", worktree_add(local, p, false))));
    if bt.0.acp.is_some() {
        let run = bt.acp_argv(&p.agent, labels, &p.worktree.to_string_lossy(), acp);
        cmds.push(sent("shed", run));
        cmds.push(sent("shed", argv(&["send", &p.agent, "<the brief>"])));
    } else {
        let pane = p.agent.as_str();
        let run = term::Shed::run_params(pane, labels, &p.worktree.to_string_lossy());
        cmds.push(sent("shed", argv(&["run", &json!(run).to_string()])));
        let typed = [home(p.lane_cmd(pane)), term::typed(pane, &home(p.start_cmd(ctx, pane)))];
        let shown = |a: Vec<String>| term::shed_line(&a)[1..].to_vec();
        cmds.extend(
            typed
                .into_iter()
                .chain(p.kick_cmds(pane))
                .map(|a| sent("shed", shown(a))),
        );
    }
    if let Some(t) = p.env.get("DROVER_TOKEN") {
        cmds.iter_mut()
            .flatten()
            .for_each(|w| *w = w.replace(t.as_str(), "•••"));
    }
    json!({
        "agent": p.agent, "role": p.role, "harness": p.harness, "profile": p.profile, "machine": on.machine,
        "model": p.model, "effort": p.effort, "branch": p.branch, "base": p.base, "account": p.account.as_ref().map(|a| &a.0),
        "worktree": p.worktree, "brief": p.brief, "kick": p.kick, "title": p.title, "port": p.port, "commands": cmds,
    })
}

type Runtime<'a> = (
    Option<&'a str>,
    &'a term::Shed,
    &'a [(&'static str, String)],
    &'a [String],
);

/// Lane scratch: untracked files fleet itself writes into a worktree (some gitignored). Never dirty work.
pub const OWN_FILES: &[&str] = &[
    ".brief.md",
    ".addendum.md",
    ".role-prompt.md",
    ".mcp.lane.json",
    "verdict.md",
    ".review-brief.md",
    "why.md",
    ".verify-browser.mjs",
];

/// #770: `OWN_FILES` and the harness files spawn copies in (`providers::LOCAL_FILES`).
pub fn own_files() -> impl Iterator<Item = &'static str> {
    OWN_FILES.iter().chain(&providers::LOCAL_FILES).copied()
}

/// #518: screenshot folders a lane leaves behind; `clean` removes them with `OWN_FILES`.
pub const SHOT_DIRS: &[&str] = &["ui-390/", "ui-1440/"];

/// #646: a drover-made clone of `repo` in `dir`: `branch` only, then every branch but `wip/*` on each later fetch.
pub fn clone_sh(repo: &str, branch: &str, dir: &str) -> String {
    let all = "'+refs/heads/*:refs/remotes/origin/*'";
    format!(
        "git clone --single-branch --branch '{branch}' https://github.com/{repo}.git '{dir}' && git -C '{dir}' config \
         remote.origin.fetch {all} && git -C '{dir}' config --add remote.origin.fetch '^refs/heads/wip/*'"
    )
}

/// `git status --porcelain` lines that are not lane scratch or generated (#248: screenshots, packages).
pub fn changes(st: &str) -> usize {
    let own = |l: &&str| {
        (own_files()
            .chain(SHOT_DIRS.iter().copied())
            .chain(["node_modules/", providers::LOCAL_DIR]))
        .any(|f| l.trim_end() == format!("?? {f}"))
    };
    st.lines().filter(|l| !own(l)).count()
}

/// #103: some lockfile exists, and each one reads the same in the local checkout and the new lane.
pub fn lockfiles_match(locks: &[&str], local: &Path, wt: &Path) -> bool {
    let read = |dir: &Path, l: &str| std::fs::read(dir.join(l)).ok();
    locks.iter().any(|l| read(local, l).is_some()) && locks.iter().all(|l| read(local, l) == read(wt, l))
}

/// #185: clone each `clone_build` folder whose lockfile matches (`Cargo.lock` for `target`, the package ones for `node_modules`); the `build-cloned` data, if any. Never a full copy: macOS `cp -c` copies across volumes, so it clones within one; elsewhere `--reflink=always` fails instead. Never over a folder the lane's agent made: the clone lands beside it and renames in only while the name is free.
pub fn clone_build(r: &dyn Runner, dirs: &[String], local: &Path, wt: &Path) -> Option<Value> {
    use std::os::unix::fs::MetadataExt;
    let dev = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
    let (t, mut done, mut errs) = (std::time::Instant::now(), vec![], vec![]);
    for d in dirs {
        let locks: &[&str] = match d.as_str() {
            "target" => &["Cargo.lock"],
            "node_modules" => &["package-lock.json", "pnpm-lock.yaml"],
            _ => {
                errs.push(format!("{d}: not target or node_modules"));
                continue;
            }
        };
        let (src, dst, tmp) = (local.join(d), wt.join(d), wt.join(format!(".{d}.clone")));
        if !src.is_dir() || dst.exists() || !lockfiles_match(locks, local, wt) {
            continue;
        }
        let (s, t) = (src.to_string_lossy(), tmp.to_string_lossy());
        let res = match cfg!(target_os = "macos") {
            true if dev(local) != dev(wt) => Err(anyhow::anyhow!("another volume, no clone")),
            true => ok(r, "cp", &["-cR", &s, &t]),
            false => ok(r, "cp", &["-R", "--reflink=always", &s, &t]),
        };
        match res.and_then(|_| Ok(std::fs::rename(&tmp, &dst)?)) {
            Ok(()) => done.push(d),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&tmp);
                errs.push(format!("{d}: {}", first_lines(&e)));
            }
        }
    }
    let secs = t.elapsed().as_secs();
    (!done.is_empty() || !errs.is_empty()).then(|| json!({"dirs": done, "secs": secs, "errors": errs}))
}

fn first_lines(e: &anyhow::Error) -> String {
    format!("{e:#}").lines().take(5).collect::<Vec<_>>().join("\n")
}

fn reuse(p: &Plan, dirs: &[String], target: Option<&String>, agents: &[Value]) -> Option<Value> {
    let (stem, k) = p.agent.rsplit_once("-r")?;
    let k = k.parse::<u64>().ok().filter(|k| *k > 1)?;
    let prev = format!("{stem}-r{}", k - 1);
    if agents.iter().any(|a| a["name"] == prev.as_str()) {
        return None;
    }
    let wt = p.worktree.to_string_lossy();
    let old = wt
        .strip_suffix(&format!("-r{k}"))
        .map(|w| PathBuf::from(format!("{w}-r{}", k - 1)));
    let mut pairs: Vec<(PathBuf, PathBuf)> = (dirs.iter())
        .filter_map(|d| Some((old.as_ref()?.join(d), p.worktree.join(d))))
        .collect();
    pairs.extend(target.map(|t| (Path::new(t).with_file_name(&prev), PathBuf::from(t))));
    let moved: Vec<String> = (pairs.into_iter())
        .filter(|(from, to)| from.is_dir() && !to.exists() && std::fs::rename(from, to).is_ok())
        .map(|(_, to)| to.display().to_string())
        .collect();
    (!moved.is_empty()).then(|| json!({"dirs": moved, "from": prev}))
}

/// #630: a new lane's build folder `t` (under its root's `.build`) cloned from the newest of its fleet's lanes' there: one volume, so copy-on-write, never a full copy. The `build-cloned` data, if any.
pub fn clone_warm(r: &dyn Runner, fleet: &str, t: &Path) -> Option<Value> {
    let b = t.parent().filter(|b| b.ends_with(crate::roots::BUILD) && !t.exists())?;
    let mine = |e: &std::fs::DirEntry| {
        let n = e.file_name().to_string_lossy().into_owned();
        n.strip_prefix(fleet).is_some_and(|x| x.starts_with('-')) && e.path().is_dir()
    };
    let ls = std::fs::read_dir(b).ok()?.flatten().filter(mine);
    let src = ls.map(|e| e.path()).max_by_key(|p| crate::check::mtime(p))?;
    let tmp = b.join(format!(".{}.clone", t.file_name()?.to_string_lossy()));
    let at = std::time::Instant::now();
    let (s, d) = (src.to_string_lossy(), tmp.to_string_lossy());
    let args: &[&str] = if cfg!(target_os = "macos") {
        &["-cR"]
    } else {
        &["-R", "--reflink=always"]
    };
    let res = ok(r, "cp", &[args, &[&s, &d]].concat());
    let res = res.and_then(|_| Ok(std::fs::rename(&tmp, t)?));
    let errs: Vec<String> = res.err().map(|e| first_lines(&e)).into_iter().collect();
    let _ = std::fs::remove_dir_all(&tmp);
    let dirs: Vec<String> = errs.is_empty().then(|| t.display().to_string()).into_iter().collect();
    Some(json!({"dirs": dirs, "from": src, "secs": at.elapsed().as_secs(), "errors": errs}))
}

/// #630: the ignored files `.drover/worktree-include` lists (one path per line, `#` comments), copied from the main checkout into a new lane's worktree when missing there. Never secrets: those stay in `secret:` env.
pub fn copy_included(local: &Path, wt: &Path) -> Result<()> {
    let list = std::fs::read_to_string(crate::repo_path(&local.join(".drover/worktree-include"))).unwrap_or_default();
    for l in list
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let (src, dst) = (local.join(l), wt.join(l));
        if l.split('/').any(|c| c == "..") || !src.is_file() || dst.exists() {
            continue;
        }
        std::fs::create_dir_all(dst.parent().unwrap_or(wt))?;
        std::fs::copy(&src, &dst).with_context(|| format!("worktree-include: {l}"))?;
    }
    Ok(())
}

/// #630: worktree `wt`'s unsaved work, readable: its uncommitted files (`OWN_FILES` aside) and unpushed commits.
pub fn unsaved(r: &dyn Runner, wt: &Path) -> Option<(u64, u64)> {
    let wt = wt.to_string_lossy();
    let st = ok(r, "git", &["-C", &wt, "status", "--porcelain"]).ok()?;
    let n = ok(
        r,
        "git",
        &["-C", &wt, "rev-list", "--count", "HEAD", "--not", "--remotes"],
    )
    .ok()?;
    Some((changes(&st) as u64, n.trim().parse().ok()?))
}

/// #630: `+<files> ⇡<commits>` of unsaved work `u`, none when there is none.
pub fn unsaved_text(u: Option<(u64, u64)>) -> Option<String> {
    let (f, c) = u.filter(|u| *u != (0, 0))?;
    Some(format!("+{f} ⇡{c}"))
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
    match ok(
        r,
        "git",
        &["-C", &wt, "rev-list", "--count", "HEAD", "--not", "--remotes"],
    ) {
        Ok(n) if n.trim() == "0" => None,
        Ok(n) => Some(format!("{} unpushed commit(s)", n.trim())),
        Err(e) => Some(format!("unreadable unpushed count ({e:#})")),
    }
}

/// `spawn`'s flags.
#[allow(clippy::struct_excessive_bools)]
#[derive(Default, clap::Args)]
pub struct Opts {
    /// Print the plan; change nothing
    #[arg(long)]
    pub dry_run: bool,
    /// Machine-readable output
    #[arg(long)]
    pub json: bool,
    /// Spawn although the host is busy: the owner or an orchestrator (logs an `override` event with `by`)
    #[arg(long)]
    pub force: bool,
    /// Kill the live agent / unfinished lane in the way, then spawn (logged)
    #[arg(long)]
    pub supersede: bool,
    /// Run the lane on this `[hosts.<name>]` (default: the least-busy enabled host whose tags cover the needs)
    #[arg(long)]
    pub host: Option<String>,
    /// Fix round that merges the integration branch into a conflicting or behind PR
    #[arg(long)]
    pub update: bool,
}

pub fn spawn(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (fleet, issue, role): (&str, u64, &str),
    o: &Opts,
) -> Result<bool> {
    let f = cfg.fleet(fleet)?;
    if f.paused && !crate::events::STAGES.contains(&role) {
        return refused("PAUSED", format!("{fleet} is paused; `drover resume {fleet}` first"));
    }
    if o.update && f.family(role) != "impl" {
        return refused("BAD_VALUE", "--update is an impl fix round");
    }
    let mut p = plan(ctx, cfg, fleet, issue, role)?;
    let ev = working(&ctx.state)?;
    let placed = crate::hosts::place(ctx, cfg, r, &ev, (fleet, issue, role), o.host.as_deref())?;
    if let Some(why) = crate::breaker::says(&ev, placed.as_ref().map_or("local", |pl| pl.name.as_str())) {
        return refused("RUNTIME_DOWN", why);
    }
    let placed = crate::hosts::wake(ctx, cfg, r, (f, o.dry_run), placed)?;
    let on = crate::hosts::On::new(r, placed.as_ref().and_then(|pl| pl.machine.as_deref()));
    let (base, r): (&dyn Runner, &dyn Runner) = (r, &on);
    if let Some(pl) = &placed {
        p.at(&pl.root, f);
        p.env.insert("DROVER_HOST".into(), pl.name.clone());
        if pl.machine.is_some() {
            match &cfg.service.url {
                Some(url) => p.env.insert("DROVER_SERVICE".into(), url.clone()),
                None => p.env.insert("DROVER_OUTBOX".into(), "1".into()),
            };
            p.env
                .insert("DROVER_RULES".into(), p.rules_file().display().to_string());
        }
    }
    if on.machine.is_none() && p.account.is_some() {
        let fr = access::fallback(cfg, &working(&ctx.state)?, f.role(role)?, ctx.now)?.0;
        access::signed(ctx, cfg, base, (fleet, &fr, o.dry_run), &mut p)?;
    }
    let hm = placed.as_ref().and_then(|pl| pl.home.as_deref());
    let home = |a: Vec<String>| crate::hosts::rehome(a, hm);
    let wt = p.worktree.to_string_lossy().into_owned();
    let local = placed.as_ref().map_or_else(|| f.local_path(), |pl| pl.local.clone());
    let local = local.to_string_lossy().into_owned();
    let reopen = on.exists(&p.worktree);
    let bt = {
        let pl = placed.as_ref().map(|pl| pl.name.as_str());
        let b = cfg.shed_on(fleet, pl).context(crate::config::NO_RUNTIME)?;
        let wire = on.shed().map_or(b.wire.clone(), |w| w.wire);
        let acp = b
            .acp
            .clone()
            .map(|a| f.role(role).ok().and_then(|r| r.agent.clone()).unwrap_or(a));
        term::Shed(crate::shed::Shed { wire, acp, ..b })
    };
    if on.machine.is_none() && ok(r, "which", &["shed"]).is_err() {
        let path = std::env::var("PATH").unwrap_or_default();
        let fix = "`drover timer install` from a shell where `which shed` finds it";
        return refused("SHED_MISSING", format!("shed is not on PATH ({path}); {fix}"));
    }
    let acp_id = bt.0.acp.clone();
    let n = round(&working(&ctx.state)?, fleet, issue, &p);
    let labels = labels(&p, acp_id.as_ref().map(|_| (f, n)));
    if let (Some(id), Some(pl)) = (&acp_id, placed.as_ref().filter(|pl| pl.machine.is_some())) {
        remote_acp(ctx, cfg, &on, &mut p, (pl, id), o.dry_run)?;
    }
    let acp = match &acp_id {
        Some(id) => acp_cmd(ctx, r, &p, id, hm.filter(|_| on.machine.is_some()))?,
        None => vec![],
    };
    let mut pj = plan_json(ctx, &p, &on, (&local, reopen), (hm, &bt, &labels, &acp));
    if o.dry_run {
        pj["dry_run"] = json!(true);
        print_plan(out, &pj, o.json)?;
        return Ok(true);
    }
    if let Some(("HOST_OFFLINE", why)) = placed.as_ref().and_then(|pl| pl.refusal.clone()) {
        return refused("HOST_OFFLINE", why);
    }
    if p.env.contains_key("DROVER_SERVICE") {
        let g = crate::service::Grant {
            fleet: fleet.into(),
            issue,
            role: role.into(),
            agent: p.agent.clone(),
            lane: p.lane.clone(),
            at: ctx.now,
        };
        p.env.insert("DROVER_TOKEN".into(), crate::service::grant(ctx, &g)?);
    }
    let h = providers::get(&p.harness);
    h.ready(ctx, r, &p)?;
    let event = |kind: &str, d: Value| emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(issue), kind, d));

    let events = working(&ctx.state)?;
    let agents = host::agents(r).context("cannot list the lanes for the duplicate guard")?;
    let live = host::live_in(&agents, &p.agent, &p.worktree);
    let names: Vec<&str> = live
        .iter()
        .filter_map(|a| a["name"].as_str().filter(|n| !n.is_empty()).or(a["pane_id"].as_str()))
        .collect();
    let mut dup = vec![];
    let stale = live.iter().all(|a| a["agent_status"] != "working") && orphan(&agents, &events, &p, true).is_some();
    if stale {
        let last = events
            .iter()
            .rfind(|e| e.fleet == fleet && e.issue == Some(issue) && !crate::events::PASSIVE.contains(&e.kind.as_str()))
            .map_or(0, |e| e.ts);
        let idle = crate::events::age(ctx.now.saturating_sub(last));
        dup.push((
            "LANE_LIVE",
            format!(
                "stale pane {} (idle {idle}): drover adopt or --supersede",
                names.join(", ")
            ),
        ));
    }
    let unfinished = fold(&events)
        .remove(&(fleet.to_string(), issue))
        .filter(|s| s.unfinished(role, f.family(role) == "rev"));
    let over = |a: &&Value| {
        let sp = (events.iter()).rposition(|e| e.is("spawn", fleet, issue) && e.str("agent") == a["name"].as_str());
        let after = |i: usize| {
            events[i + 1..]
                .iter()
                .any(|e| e.is("verdict", fleet, issue) || e.is("spawn", fleet, issue))
        };
        matches!(a["agent_status"].as_str(), Some("done" | "idle")) && sp.is_some_and(after)
    };
    if !stale && !live.is_empty() {
        let st = live[0]["agent_status"].as_str().unwrap_or_default();
        let finished = if unfinished.is_none() && live.iter().all(over) {
            format!("finished ({st}) lane(s) ")
        } else {
            "live agent(s) ".into()
        };
        dup.push(("LANE_LIVE", format!("{finished}{names:?} in {wt}")));
    }
    if let Some(s) = unfinished {
        dup.push((
            "LANE_LIVE",
            format!(
                "unfinished {role} lane {:?} for #{} ({})",
                s.lanes.get(role),
                s.issue,
                s.state
            ),
        ));
    }
    let prior = events
        .iter()
        .rfind(|e| e.kind == "spawn" && e.str("worktree") == Some(&wt));
    let unavailable =
        |e: &Event, sp: &Event| e.issue == Some(issue) && e.ts >= sp.ts && e.str("agent") == sp.str("agent");
    let rest = |e: &&Event| crate::access::rests(&e.kind);
    let chain = !f.role(role)?.fallback.is_empty();
    let gone = prior.filter(|sp| chain && events.iter().filter(rest).any(|e| unavailable(e, sp)));
    let entry = |h: Option<&str>, a: Option<&str>| [h, a].into_iter().flatten().collect::<Vec<_>>().join(":");
    if on.exists(&p.worktree) && prior.is_none() {
        if let Some(why) = dirty(r, &p.worktree) {
            dup.push(("WORKTREE_DIRTY", format!("{wt} is no fleet lane and has {why}")));
        }
    }
    if let Some(pane) = orphan(&agents, &events, &p, false).filter(|_| live.len() == 1 && !o.supersede) {
        return adopt(ctx, cfg, r, out, (fleet, issue, role), &pane, &json!({})).map(|()| true);
    }
    if let Some((code, msg)) = dup.first().filter(|_| !o.supersede) {
        return refused(
            code,
            format!("{msg}{}", if stale { "" } else { "; `--supersede` replaces it" }),
        );
    }

    lane_admit(ctx, (cfg, r), &events, fleet, issue, role)?;
    respawn_guard(ctx, cfg, &events, (fleet, issue, role))?;
    let updates = fold(&events).remove(&(fleet.into(), issue)).map_or(0, |s| s.updates);
    if o.update && updates >= f.flow().update_limit() {
        let why = format!(
            "PR still conflicts after {updates}/{} update rounds",
            f.flow().update_limit()
        );
        event("needs-human", json!({"code": "UPDATE_LIMIT", "reason": why}))?;
        return refused("UPDATE_LIMIT", why);
    }

    let refusal = match &placed {
        Some(pl) => pl.refusal.clone(),
        None => crate::hosts::gate(ctx, cfg, &on).1,
    };
    let budget = cfg.limits.worktree_budget_gb;
    if let Some(gb) = crate::check::worktree_gb(ctx).filter(|g| on.machine.is_none() && *g > budget) {
        let why = crate::check::worktree_why(ctx);
        return refused("DISK_BUDGET", format!("worktrees {gb:.1} GB > {budget} GB; {why}"));
    }
    let extend = crate::pending::extending(&events, fleet, issue);
    match refusal {
        Some(("HOST_BUSY", why)) if o.force || extend => {
            let by = crate::orchestrator::acting().map_or_else(|| "owner".into(), |id| format!("orchestrator-{id}"));
            let mut d = json!({"code": "HOST_BUSY", "reason": why, "role": role, "agent": p.agent, "by": by});
            if extend {
                d["why"] = "owner-extend".into();
            }
            event("override", d)?;
        }
        Some(("HOST_BUSY", why)) => return refused("HOST_BUSY", format!("{why}; retry later or pass --force")),
        Some((code, why)) => return refused(code, why),
        None => {}
    }
    let step = ["spawn", "supersede"][usize::from(o.supersede && !dup.is_empty())];
    let round = events
        .iter()
        .rfind(|e| e.is("spawn", fleet, issue))
        .map_or(0, |e| e.seq);
    let probe = |i: &Event| -> Result<Option<Value>> {
        let logged = working(&ctx.state)?
            .iter()
            .any(|e| e.seq > i.seq && e.is("spawn", fleet, issue));
        let up = (step == "spawn").then(|| host::agents(r).ok()).flatten();
        let up = up.is_some_and(|a| !host::live_in(&a, &p.agent, &p.worktree).is_empty());
        Ok((logged || up).then(|| json!({"agent": p.agent})))
    };
    let (key, secs) = (format!("{role}@{round}"), SPAWN_SECS + 300);
    let inputs = json!({"role": role, "agent": p.agent, "worktree": p.worktree});
    let intent =
        crate::action::begin(r, &ctx.state, ctx.now, (fleet, issue, step, &key), inputs, secs, &probe)?.act()?;
    let mut wip = None;
    if o.supersede && !dup.is_empty() {
        let was = last_spawn(&events, &p.agent);
        let old = crate::hosts::On::of(base, was);
        let owt = was.and_then(|e| e.str("worktree")).unwrap_or(&wt);
        let listed = host::agents(&old);
        let stale = was.filter(|_| listed.is_err() && old.machine.is_some());
        let hung =
            was.is_some_and(|e| matches!(crate::roots::skip(ctx, cfg, e), Some("root unreachable" | "root-hung")));
        let cause = crate::shed::exit_cause(&events, &p.agent).filter(|c| crate::shed::RESUMABLE.contains(c));
        let here = cause.is_some() && owt == wt && old.machine == on.machine;
        if stale.is_none() && !here && !hung {
            wip = crate::check::save_lane(ctx, &old, &p.agent, owt)?;
        }
        for a in host::live_in(listed.as_deref().unwrap_or_default(), &p.agent, Path::new(owt)) {
            if let Some(pane) = a["pane_id"].as_str() {
                term::close(&old, pane)?;
            }
        }
        if stale.is_none() && !hung {
            crate::check::reap(ctx, cfg, &old, (fleet, Some(issue)), owt)?;
        }
        let why: Vec<&str> = dup.iter().map(|d| d.1.as_str()).collect();
        let mut d = json!({"role": role, "agent": p.agent, "killed": names, "reason": why.join("; "), "wip": wip});
        if let Some(e) = stale {
            (d["stale_host"], d["stale_pane"]) = (json!(e.str("host")), json!(e.str("pane")));
        }
        event("supersede", d)?;
    }
    crate::check::stop_rounds(ctx, cfg, base, (fleet, issue, role), &p.agent)?;
    let moved = (placed.as_ref())
        .and_then(|pl| last_spawn(&events, &p.agent).filter(|e| e.str("machine") != pl.machine.as_deref()));
    if let Some((e, owt)) = moved
        .and_then(|e| Some((e, e.str("worktree")?)))
        .filter(|_| wip.is_none())
    {
        wip = crate::check::save_lane(ctx, &crate::hosts::On::of(base, Some(e)), &p.agent, owt)?;
    }

    let mut sha = None;
    let note;
    let brief_text = {
        let iss = crate::gh::spec(ctx, base, f, fleet, issue, true)?;
        if on.machine.is_some() && !on.exists(&Path::new(&local).join(".git")) {
            ok(r, "sh", &["-c", &clone_sh(&f.repo, &f.integration, &local)])?;
        }
        ok(
            r,
            "git",
            &["-C", &local, "fetch", "origin", p.base.trim_start_matches("origin/")],
        )?;
        let body = iss["body"].as_str().unwrap_or("");
        let recipe = match p.family {
            "design" => crate::design::Recipe::at(r, &local, &p.base, body),
            _ => crate::design::Recipe::of("", body),
        };
        crate::design::tools(ctx, cfg, r, (fleet, issue, &p.agent), &recipe)?;
        let pushed = || ok(r, "git", &["-C", &local, "fetch", "origin", &p.branch]).is_ok();
        if moved.is_some() && p.family != "rev" && pushed() {
            p.base = format!("origin/{}", p.branch);
        }
        let make = || -> Result<()> {
            if !reopen {
                let have = r
                    .run(
                        "git",
                        &argv(&["-C", &local, "rev-parse", "--verify", "--quiet", &p.branch]),
                    )?
                    .code
                    == 0;
                ok(r, "git", &worktree_add(&local, &p, have))?;
            }
            Ok(())
        };
        match make() {
            Err(e) if !reopen => {
                let Some(held) = checked_out(r, &local, &p.branch) else {
                    return Err(e);
                };
                let live = host::agents(r)?;
                let names: Vec<&str> = (host::live_in(&live, "\0", Path::new(&held)).iter())
                    .filter_map(|a| a["name"].as_str().filter(|n| !n.is_empty()).or(a["pane_id"].as_str()))
                    .collect();
                if !names.is_empty() {
                    let why = format!("live agent(s) {names:?} in {held}, where {} is checked out", p.branch);
                    return refused("LANE_LIVE", why);
                }
                let git = |a: &[&str]| ok(r, "git", &[&["-C", &local, "worktree"][..], a].concat());
                if on.exists(Path::new(&held)) {
                    wip = wip.or(crate::check::save_lane(ctx, &on, &p.agent, &held)?);
                    if git(&["remove", &held]).is_err() {
                        git(&["remove", "--force", &held])?;
                    }
                } else {
                    git(&["prune"])?;
                }
                let d = json!({"role": role, "agent": p.agent, "old": held, "worktree": wt, "wip": wip});
                event("worktree-moved", d)?;
                make()?;
            }
            c => c?,
        }
        if p.family == "rev" && reopen {
            ok(r, "git", &["-C", &wt, "reset", "--hard", &p.base])?;
            let _ = on.remove(&p.worktree.join("verdict.md"));
        }
        if p.family == "rev" || crate::events::STAGES.contains(&p.family) {
            sha = ok(r, "git", &["-C", &wt, "rev-parse", "HEAD"])
                .ok()
                .map(|h| h.trim().to_string());
        }
        let (title, body) = (iss["title"].as_str().unwrap_or(""), iss["body"].as_str().unwrap_or(""));
        p.title = lane_title(&p.head, title);
        note = prior
            .filter(|_| gone.is_some() || o.supersede && !dup.is_empty())
            .map(|prev| handover(ctx, r, &events, prev, &p, (fleet, issue, title)));
        let cause = prior
            .filter(|_| o.supersede)
            .and_then(|sp| crate::shed::exit_cause(&events, sp.str("agent")?));
        let resume = cause.filter(|c| crate::shed::RESUMABLE.contains(c)).map(|c| {
            format!(
                "Resume: your previous session in this worktree stopped mid-turn ({c}). Its local commits and staged \
                 changes are kept here, and drover pushed the commits to `{}`: continue from them and push.",
                p.branch
            )
        });
        let last = events.iter().rev().find(|e| e.is("verdict", fleet, issue));
        let fix = last
            .filter(|e| p.family == "impl" && e.str("verdict") == Some("FAIL"))
            .and_then(|e| e.str("body"));
        let update = o
            .update
            .then(|| format!(include_str!("../roles/brief-update.md"), f.integration));
        let adr = events
            .iter()
            .rfind(|e| e.is("design-approved", fleet, issue) && e.str("role") == Some("architect"))
            .and_then(|e| e.str("adr"))
            .filter(|_| p.family != "architect")
            .map(|a| match p.family {
                "rev" => format!("Architecture: the owner approved the ADR in `{a}/`; check the diff against it."),
                _ => format!("Architecture: the owner approved the ADR in `{a}/` on this branch; build to it, and add any `.drover/arch.toml` lines it ends with in this PR."),
            });
        let decided = events
            .iter()
            .filter(|e| e.is("decision", fleet, issue))
            .map(plain)
            .chain(update)
            .chain(adr)
            .chain((!crate::events::STAGES.contains(&p.family)).then(|| {
                let rev = (p.family == "rev").then(|| sha.as_deref().unwrap_or_default());
                crate::why::brief(r, f, &events, (fleet, issue), rev, body)
            }).flatten())
            .chain(note.clone())
            .chain(resume)
            .chain(wip.as_ref().map(|w| {
                format!("Saved work: the last lane's uncommitted and unpushed work is on `origin/{w}`; fetch and merge it first.")
            }))
            .map(|t| format!("\n\n{t}"));
        brief(
            f,
            issue,
            &p,
            title,
            &decided.fold(body.to_string(), |b, d| b + &d),
            (
                fix,
                crate::repo_read("rules.md", |f| on.read(&p.worktree.join(f))).as_deref(),
                crate::queue::repro(&events, fleet, issue),
                crate::queue::playbook(&events, fleet, issue),
            ),
            &recipe,
        )
    };
    on.write(&p.brief, &brief_text).context("writing the brief")?;
    if p.role == "verify" {
        let vb = include_str!("../../../scripts/verify-browser.mjs");
        on.write(&p.worktree.join(".verify-browser.mjs"), vb)?;
    }
    if on.machine.is_none() {
        copy_included(&f.local_path(), &p.worktree)?;
    }
    h.prepare(ctx, &on, &p)?;
    if on.machine.is_some() && p.lane_key.is_none() {
        let exe = home(vec![ctx.exe.clone()]).remove(0);
        ok(
            r,
            &exe,
            &["profiles", "sync", "--profile", &p.profile, "--role", p.family],
        )?;
    }
    if let (Some(k), Some(pl)) = (&p.lane_key, &placed) {
        let acct = p.account.as_ref().and_then(|a| cfg.accounts.names.get(&a.0));
        let acct = acct.map_or("", String::as_str);
        crate::proxy::mint(ctx, cfg, k, (fleet, issue, &p.agent), (&pl.name, acct))?;
        crate::proxy::ensure(ctx, cfg, base, (&pl.name, pl.machine.as_deref().unwrap_or_default()))?;
    }
    let _secrets = deliver(ctx, &on, cfg, &p)?;
    let rules = placed.as_ref().filter(|pl| pl.machine.is_some());
    let rules = rules.map(|pl| crate::hook::Rules::resolve(cfg, fleet, role, &pl.name));
    if let Some(rl) = &rules {
        on.write(&p.rules_file(), &serde_json::to_string(rl)?)?;
    }
    let pane = p.agent.clone();
    let host = placed.as_ref().map(|pl| (&pl.name, &pl.machine));
    let d = json!({"agent": p.agent, "role": role, "host": host.map(|h| h.0), "machine": host.and_then(|h| h.1.as_ref()),
        "worktree": p.worktree, "pane": pane});
    event("spawn-started", d)?;
    pj = plan_json(ctx, &p, &on, (&local, reopen), (hm, &bt, &labels, &acp));
    bt.start(r, &pane, &labels, &wt, &acp)?;
    if bt.0.acp.is_none() {
        term::ok(r, &home(p.lane_cmd(&pane)))?;
        term::ok(r, &term::typed(&pane, &home(p.start_cmd(ctx, &pane))))?;
    }
    let mut dirs: Vec<String> = f.clone_build.clone();
    dirs.extend(f.clone_node_modules.then(|| "node_modules".into()));
    dirs.sort();
    dirs.dedup();
    let dirs = if reopen || on.machine.is_some() {
        &[][..]
    } else {
        &dirs[..]
    };
    let target = p.env.get("CARGO_TARGET_DIR").filter(|_| on.machine.is_none());
    if let Some(d) = (p.family == "rev").then(|| reuse(&p, dirs, target, &agents)).flatten() {
        event("build-reused", d)?;
    }
    let (res, cloned) = std::thread::scope(|s| {
        let c = s.spawn(|| {
            let warm = target.and_then(|t| clone_warm(r, fleet, Path::new(t)));
            [clone_build(r, dirs, &f.local_path(), &p.worktree), warm]
        });
        let res = (|| -> Result<bool> {
            let kicked = match bt.0.acp {
                Some(_) => bt.kick_acp(r, &pane, &brief_text),
                None => h.kick(r, &p, &pane),
            };
            let mut d = lane_data(&p, &pane);
            d["runtime"] = json!(["shed", "shed-acp"][usize::from(bt.0.acp.is_some())]);
            let confirmed = kicked.as_ref().is_ok_and(|c| *c);
            d["confirmed"] = json!(confirmed);
            d["error"] = json!(kicked.as_ref().err().map(|e| format!("{e:#}")));
            d["update"] = json!(o.update);
            d["handover"] = json!(note);
            if let Some(pl) = &placed {
                (d["host"], d["machine"], d["root"]) = (json!(pl.name), json!(pl.machine), json!(pl.root));
                if let Some(w) = &pl.why {
                    d["why"] = json!(w);
                }
            }
            if let Some(h) = sha.filter(|h| !h.is_empty()) {
                d["sha"] = json!(h);
            }
            if let Some(rl) = &rules {
                d["rules"] = json!(rl);
            }
            let until = p.resting.as_ref().map(|r| crate::usage::date(base, r.1, "+%H:%M"));
            d["resting"] = json!(p.resting.as_ref().zip(until).map(|(r, t)| format!("{} {t})", r.0)));
            let to = entry(d["harness"].as_str(), d["account"].as_str());
            if let Some((agent, old)) = prior.and_then(|e| Some((e.str("agent")?, e.str("pane")?))) {
                if o.supersede && confirmed && old != pane && !agents.iter().any(|a| a["pane_id"] == old) {
                    let _ = term::close(r, old);
                    let c = json!({"agent": agent, "reason": "superseded", "pane": old});
                    event("lane-closed", c)?;
                }
            }
            intent.done(json!({"agent": p.agent, "pane": pane}))?;
            event("spawn", d)?;
            if let Some(sp) = gone {
                let from = entry(sp.str("harness"), sp.str("account"));
                event("fallback", json!({"agent": p.agent, "from": from, "to": to}))?;
            }
            kicked?;
            pj["pane"] = json!(pane);
            pj["confirmed"] = json!(confirmed);
            print_plan(out, &pj, o.json)?;
            Ok(confirmed)
        })();
        (res, c.join().ok())
    });
    for d in cloned.into_iter().flatten().flatten() {
        event("build-cloned", d)?;
    }
    res
}

const SPAWN_SECS: u64 = 3 * term::CONFIRM_SECS;

fn orphan(agents: &[Value], events: &[Event], p: &Plan, unnamed: bool) -> Option<String> {
    let mine = |a: &&Value| a["name"] == p.agent.as_str() || unnamed && a["name"].as_str().is_none_or(str::is_empty);
    let pane = host::live_in(agents, "\0", &p.worktree).into_iter().find(mine)?["pane_id"].as_str()?;
    let logged = events.iter().any(|e| e.kind == "spawn" && e.str("pane") == Some(pane));
    (!logged).then(|| pane.into())
}

pub(crate) fn lane_admit(
    ctx: &Ctx,
    (cfg, r): (&Config, &dyn Runner),
    events: &[Event],
    fleet: &str,
    issue: u64,
    role: &str,
) -> Result<()> {
    let f = cfg.fleet(fleet)?;
    let events = &[events, &crate::drive::pending(ctx, cfg, r)].concat();
    let st = fold(events);
    let others: Vec<_> = st.values().filter(|s| s.fleet == fleet && s.issue != issue).collect();
    let building = |s: &str| s == "impl" || s.starts_with("fix");
    let gs = crate::queue::groups(&crate::queue::replay(events, fleet), events, f, fleet, issue);
    for g in gs.into_iter().filter(|_| role == "impl") {
        let h = crate::queue::holders(events, &st, f, fleet, &g.name);
        if !h.iter().any(|s| s.issue == issue) && h.len() >= g.max {
            let why = format!("{}; retry after they merge", crate::queue::holds(&g, &h));
            return refused("SERIAL_BUSY", why);
        }
    }
    let fam = f.family(role);
    let (cap, n) = match fam {
        "impl" => (f.max_impl, others.iter().filter(|s| building(&s.state)).count()),
        "rev" => (
            f.max_rev,
            others.iter().filter(|s| s.state.starts_with("review")).count(),
        ),
        _ => (None, 0),
    };
    if let Some(cap) = cap.filter(|&c| n >= c) {
        return refused("CAP", format!("{n} {role} lane(s) open in {fleet}, max_{fam} = {cap}"));
    }
    Ok(())
}

fn starts<'a>(
    events: &'a [Event],
    fleet: &str,
    issue: u64,
    role: &str,
) -> (Vec<(u64, Option<&'a str>)>, Option<&'a str>) {
    let (mut v, mut asked, mut agent) = (vec![], None, None);
    let mut asks = vec![];
    let fail = |v: &mut Vec<(u64, Option<&str>)>, why| {
        if let Some(s) = v.last_mut().filter(|s| s.1.is_none()) {
            s.1 = Some(why);
        }
    };
    for e in events.iter().filter(|e| e.fleet == fleet && e.issue == Some(issue)) {
        let mine = e.str("role") == Some(role);
        match e.kind.as_str() {
            "spawn" if mine => {
                agent = e.str("agent");
                v.push((
                    e.ts,
                    (e.data.get("confirmed") == Some(&json!(false))).then_some("prompt never landed"),
                ));
            }
            "pushed" => (v, asked) = (vec![], None),
            "runtime-cleared" => v.clear(),
            "turn-end" if mine => (v, asked) = (vec![], None),
            "lane-dead" if agent.is_some_and(|a| e.str("key").is_some_and(|k| k.ends_with(&format!(":{a}")))) => {
                fail(&mut v, "lane died");
            }
            "supersede" | "lane-killed" if mine => fail(&mut v, "killed"),
            "needs-decision" if e.str("respawn_loop") == Some(role) => {
                asks.extend(e.str("id"));
                asked = e.str("id");
            }
            "decision" if e.str("choice") == Some("try again") && asks.contains(&e.str("id").unwrap_or_default()) => {
                (v, asked) = (vec![], None);
            }
            _ => {}
        }
    }
    (v, asked)
}

fn respawn_guard(ctx: &Ctx, cfg: &Config, events: &[Event], (fleet, issue, role): (&str, u64, &str)) -> Result<()> {
    let l = &cfg.limits;
    let (v, asked) = starts(events, fleet, issue, role);
    let since = ctx.now.saturating_sub(l.respawn_window_minutes * 60);
    let failed: Vec<String> = (v.iter())
        .filter(|s| s.0 >= since)
        .filter_map(|s| Some(format!("{} ago: {}", crate::events::age(ctx.now - s.0), s.1?)))
        .collect();
    if failed.len() < l.respawn_max as usize {
        return Ok(());
    }
    let why = format!(
        "{} failed {role} starts within {} min ({}); answer the owner ask to try again or park",
        failed.len(),
        l.respawn_window_minutes,
        failed.join("; ")
    );
    if asked.is_none() {
        let q = format!("#{issue}'s {role} lane keeps failing at start: {}", failed.join("; "));
        let d = json!({"question": q, "options": ["try again", "park"], "recommend": null, "agent": null,
            "kind": "owner", "respawn_loop": role});
        let ev = Event::new(ctx.now, fleet, Some(issue), "needs-decision", d);
        crate::events::ask(&ctx.state, cfg, vec![ev], None)?;
    }
    refused("RESPAWN_LOOP", why)
}

fn handover(
    ctx: &Ctx,
    r: &dyn Runner,
    events: &[Event],
    prev: &Event,
    p: &Plan,
    (fleet, issue, title): (&str, u64, &str),
) -> String {
    let acct = p.account.as_ref().map(|a| a.0.as_str());
    let mine = |e: &&Event| e.fleet == fleet && e.issue == Some(issue);
    let spawns: Vec<&Event> = events
        .iter()
        .filter(mine)
        .filter(|e| e.kind == "spawn" && e.str("role") == Some(&p.role))
        .collect();
    let ran = spawns
        .iter()
        .rposition(|e| e.str("harness") == Some(&p.harness) && e.str("account") == acct);
    let since = ran.and_then(|i| spawns.get(i + 1)).map(|e| e.ts);
    let at = since.map(iso);
    let after = |t: &Value| at.as_ref().is_none_or(|s| t.as_str().is_none_or(|t| t > s.as_str()));
    let (wt, h) = (
        p.worktree.to_string_lossy(),
        providers::get(prev.str("harness").unwrap_or_default()),
    );
    let (mut prompts, mut replies, mut cmds) = (vec![], vec![], vec![]);
    let jsonl = h
        .transcript(&ctx.home, &wt)
        .and_then(|f| crate::lanes::read_tail(&f, 1 << 20));
    let log = crate::lanes::log(&ctx.state, prev.str("agent").unwrap_or_default());
    if let Some(t) = &jsonl {
        for m in crate::lanes::said(t).0.into_iter().filter(|m| after(&m["at"])) {
            let text = m["text"].as_str().unwrap_or_default().to_string();
            [&mut replies, &mut prompts][usize::from(m["k"] == "me")].push(text);
        }
        let rows = t.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok());
        for v in rows.filter(|v| after(&v["timestamp"])) {
            let parts = v["message"]["content"].as_array().cloned().unwrap_or_default();
            cmds.extend(
                parts
                    .iter()
                    .filter_map(|c| c["input"]["command"].as_str())
                    .map(crate::lanes::mask),
            );
        }
    } else if let Some(text) = log.and_then(|f| std::fs::read_to_string(f).ok()) {
        let mut open = false;
        for l in text.lines().map(crate::lanes::strip) {
            let l = l.trim();
            let glyph = |g: &[char]| {
                l.strip_prefix(g)
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
            };
            open = match (h.tool_line(l), glyph(&['❯', '›', '>']), glyph(&['⏺', '•'])) {
                (Some((t, a)), ..) => {
                    cmds.extend((t == "Bash" || t == "$").then_some(a));
                    false
                }
                (_, Some(t), _) => {
                    prompts.push(t);
                    false
                }
                (.., Some(t)) => {
                    replies.push(t);
                    true
                }
                _ if open && !l.is_empty() => {
                    replies.last_mut().into_iter().for_each(|r| *r += &format!(" {l}"));
                    true
                }
                _ => false,
            };
        }
    }
    let git = |a: &[&str]| ok(r, "git", &[&["-C", &*wt][..], a].concat()).unwrap_or_default();
    let files = match since {
        Some(s) => {
            git(&["log", &format!("--since=@{s}"), "--name-only", "--format="]) + &git(&["diff", "--name-only", "HEAD"])
        }
        None => git(&["diff", "--name-only", &p.base]),
    };
    let mut files: Vec<String> = files
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(String::from)
        .collect();
    files.sort();
    files.dedup();
    let later = |e: &&Event| since.is_none_or(|s| e.ts > s);
    let asks = events
        .iter()
        .filter(mine)
        .filter(later)
        .filter(|e| matches!(e.kind.as_str(), "needs-decision" | "decision"));
    let asks: Vec<String> = asks.map(plain).collect();
    let notes = spawns
        .iter()
        .copied()
        .filter(later)
        .filter_map(|e| e.str("handover"))
        .map(String::from);
    let mut out =
        "Handover: Summary of the previous lane; its tool state and approvals are not carried over.".to_string();
    if let Some(s) = since {
        let who = [Some(p.harness.as_str()), acct]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(":");
        out += &format!(
            "\nOnly what happened since {who}'s last lane on this issue was replaced ({} UTC).",
            iso(s).replace('T', " ")
        );
    }
    out += &format!("\nIssue: {}", handover_item(title));
    let sections = [
        ("Prompts", prompts),
        ("Final replies", replies),
        ("Commands run", cmds),
        ("Files changed", files),
        ("Asks and answers", asks),
        ("Prior handover notes", notes.collect()),
    ];
    for (name, items) in sections.into_iter().filter(|s| !s.1.is_empty()) {
        out += &format!("\n{name}:");
        for i in &items[items.len().saturating_sub(20)..] {
            out += &format!("\n- {}", handover_item(i));
        }
    }
    out + "\nContinue; don't redo."
}

fn handover_item(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match s.char_indices().nth(237) {
        Some((i, _)) if s.chars().count() > 240 => format!("{}...", &s[..i]),
        _ => s,
    }
}

fn iso(t: u64) -> String {
    let (d, s) = (t / 86_400, t % 86_400);
    let (m, day) = access::civil_month_day(d);
    format!(
        "{}-{m:02}-{day:02}T{:02}:{:02}:{:02}",
        access::civil_year(d),
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

fn lane_data(p: &Plan, pane: &str) -> Value {
    json!({"role": p.role, "stage": p.stage, "check": p.family == "rev", "harness": p.harness, "profile": p.profile, "model": p.model, "effort": p.effort, "agent": p.agent,
        "pane": pane, "worktree": p.worktree, "branch": p.branch, "account": p.account.as_ref().map(|a| &a.0), "port": p.port, "qos": p.qos()})
}

pub fn adopt(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    (fleet, issue, role): (&str, u64, &str),
    pane: &str,
    extra: &Value,
) -> Result<()> {
    let p = plan(ctx, cfg, fleet, issue, role)?;
    term::harness_in(r, pane).with_context(|| format!("no harness running in pane {pane}"))?;
    let mut d = lane_data(&p, pane);
    d["adopted"] = json!(true);
    extra
        .as_object()
        .into_iter()
        .flatten()
        .for_each(|(k, v)| d[k] = v.clone());
    writeln!(out, "adopted {pane} as {} ({} @ {})", p.agent, p.branch, d["worktree"])?;
    emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(issue), "spawn", d))
}

pub fn shaped<'a>(cfg: &'a Config, a: &Value) -> Option<(&'a str, u64, Option<String>, Option<String>)> {
    let name = a["name"].as_str().unwrap_or_default();
    let cwd = Path::new(a["cwd"].as_str().or(a["foreground_cwd"].as_str()).unwrap_or_default());
    let parts: Vec<&str> = cwd.iter().filter_map(|c| c.to_str()).collect();
    cfg.fleet.iter().find_map(|(fl, f)| {
        let lane = |b: &str, issue: bool| {
            let mut w = b.split('-');
            let role = w
                .next()
                .filter(|r| f.role.contains_key(*r) && (issue || f.family(r) == "rev"))?;
            Some((Some(role.to_string()), w.next()?.parse::<u64>().ok()?))
        };
        let named = name.strip_prefix(&format!("{fl}-")).and_then(|w| lane(w, true));
        let at = parts
            .windows(3)
            .position(|w| matches!(w[0], "worktrees" | "work") && w[1] == f.repo_name());
        let by_cwd = at.and_then(|i| match parts[i + 2].strip_prefix("issue-") {
            Some(n) => Some((None, n.parse().ok()?)),
            None => lane(parts[i + 2], false),
        });
        let (role, n) = named.or(by_cwd.clone())?;
        let wt = at.filter(|_| by_cwd.is_some_and(|c| c.1 == n));
        let wt = wt.map(|i| {
            parts[..i + 3]
                .iter()
                .collect::<PathBuf>()
                .to_string_lossy()
                .into_owned()
        });
        Some((fl.as_str(), n, role, wt))
    })
}

pub fn orphans(ctx: &Ctx, cfg: &Config, (r, seen): (&dyn Runner, &dyn Runner), out: &mut dyn Write) -> Result<bool> {
    let agents = crate::hosts::all_agents(cfg, seen)?;
    let events = working(&ctx.state)?;
    let (st, mut any) = (fold(&events), false);
    for a in agents {
        let (Some(pane), m, name) = (a["pane_id"].as_str(), a["machine"].as_str(), a["name"].as_str()) else {
            continue;
        };
        let named = |e: &&Event| name.is_none_or(str::is_empty) || e.str("agent") == name;
        let at =
            |e: &&Event| e.str("pane") == Some(pane) && e.str("machine") == m && named(e) || e.str("agent") == name;
        let young = |e: &Event| e.kind == "spawn-started" && ctx.now.saturating_sub(e.ts) < SPAWN_SECS;
        let logged = (events.iter())
            .filter(|e| matches!(e.kind.as_str(), "spawn" | "spawn-adopted") || young(e))
            .any(|e| at(&e));
        let Some((fleet, n, role, wt)) = shaped(cfg, &a).filter(|_| !logged) else {
            continue;
        };
        let Some(s) = st.get(&(fleet.to_string(), n)) else {
            continue;
        };
        let f = cfg.fleet(fleet)?;
        let started = events.iter().filter(|e| e.kind == "spawn-started").rfind(at);
        let role = role.or(s.build.clone()).unwrap_or(f.flow().start().into());
        let wt = wt.or(started.and_then(|e| e.str("worktree")).map(String::from));
        let on = crate::hosts::On::new(r, m);
        let rev = wt.as_deref().filter(|_| f.family(&role) == "rev");
        let verdict = rev.and_then(|w| on.read(&Path::new(w).join("verdict.md")));
        let git = |w: &str| ok(&on, "git", &["-C", w, "rev-parse", "HEAD"]).ok();
        let named = verdict.map(|v| {
            crate::waiting::reviewed_head(&v)
                .map(String::from)
                .or_else(|| git(rev?))
        });
        let branch = crate::gate::branch_of(Some(s), n);
        let head = |x: String| {
            Some((
                x,
                crate::gh::pr(ctx, r, f, (fleet, n), &branch, ctx.now).ok()?["headRefOid"]
                    .as_str()?
                    .into(),
            ))
        };
        let old = named
            .flatten()
            .and_then(head)
            .filter(|(x, h): &(String, String)| !x.eq_ignore_ascii_case(h));
        let due = s
            .stage_rounds
            .get(f.flow().stage_of(&role))
            .copied()
            .unwrap_or_default()
            + 1;
        let lane = name.filter(|n| !n.is_empty()).or(wt.as_deref());
        let round = lane.and_then(|l| l.rsplit_once("-r")?.1.parse::<u32>().ok());
        let round = round.filter(|k| f.family(&role) == "rev" && *k != due);
        let stale = match old {
            _ if s.done() => Some(format!("{fleet}#{n} is {}", s.state)),
            _ if let Some(k) = round => Some(format!("round r{k}, r{due} is due")),
            Some((x, h)) => Some(format!("verdict for {x:.7}, head is {h:.7}")),
            None => None,
        };
        let agent = name.filter(|n| !n.is_empty()).unwrap_or(pane);
        let res = if let Some(why) = stale {
            (|| {
                if let Some(w) = wt.as_deref().filter(|w| dirty(&on, Path::new(w)).is_some()) {
                    let b = crate::check::wip_branch(ctx, &on, &format!("{fleet}-{role}-{n}"));
                    crate::check::save_wip(&on, w, &b, "wip: stale orphan lane, saved by drover")?;
                }
                term::close(&on, pane)?;
                writeln!(out, "closed stale orphan {agent} ({why})")?;
                let d = json!({"agent": agent, "pane": pane, "host": a["host"], "why": "stale-orphan", "reason": why});
                emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(n), "lane-closed", d))
            })()
        } else {
            let since = started.map_or(0, |e| e.ts);
            let mut d = json!({"why": "orphan", "since": since, "worktree": wt, "host": a["host"], "machine": m});
            d.as_object_mut()
                .into_iter()
                .for_each(|o| o.retain(|_, v| !v.is_null()));
            adopt(ctx, cfg, &on, out, (fleet, n, &role), pane, &d).and_then(|()| {
                let d = json!({"agent": agent, "pane": pane, "why": "orphan"});
                emit(&ctx.state, cfg, Event::new(ctx.now, fleet, Some(n), "spawn-adopted", d))
            })
        };
        match res {
            Ok(()) => any = true,
            Err(e) => writeln!(out, "{fleet} #{n} orphan {agent}: {e:#}")?,
        }
    }
    Ok(any)
}

fn print_plan(out: &mut dyn Write, pj: &Value, as_json: bool) -> Result<()> {
    if as_json {
        writeln!(out, "{}", serde_json::to_string_pretty(pj)?)?;
    } else {
        writeln!(
            out,
            "{} [{} {}] {} @ {}",
            pj["agent"], pj["harness"], pj["profile"], pj["branch"], pj["worktree"]
        )?;
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
