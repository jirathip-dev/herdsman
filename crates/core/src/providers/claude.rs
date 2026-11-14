mod meter;

use super::{hook_command, Accounts, Credential, Dialect, Header, Provider, Proxy, Usage};
use crate::config::Config;
use crate::term;
use crate::lanes::{chat, mask, strip};
use crate::runner::Runner;
use crate::spawn::{role_prompt, Plan};
use crate::Ctx;
use anyhow::{bail, Result};
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};

const SETTINGS: &str = include_str!("../../hooks/claude-settings.json");

pub struct Claude;
pub static PROVIDER: Claude = Claude;

const BETA_HEADER: &str = "anthropic-beta";
const OAUTH_BETA: &str = "oauth-2025-04-20";
/// #770: what `acp_profile` copies into a worktree (the lane's own files, never its work) and the folder it makes.
pub const LOCAL_FILES: [&str; 2] = ["CLAUDE.local.md", ".claude/settings.local.json"];
pub const LOCAL_DIR: &str = ".claude/";

static PROXY: Proxy = Proxy {
    upstream: "https://api.anthropic.com",
    base_url_env: "ANTHROPIC_BASE_URL",
    auth_token_env: "ANTHROPIC_AUTH_TOKEN",
    headers,
};

fn headers(kept: &[Header]) -> Vec<Header> {
    let sent = kept.iter().find(|h| h.0 == BETA_HEADER).map_or("", |h| h.1.as_str());
    let mut beta: Vec<&str> = sent.split(',').map(str::trim).filter(|b| !b.is_empty()).collect();
    if !beta.contains(&OAUTH_BETA) {
        beta.push(OAUTH_BETA);
    }
    let head = (BETA_HEADER.to_string(), beta.join(","));
    let rest = kept.iter().filter(|h| h.0 != BETA_HEADER).cloned();
    std::iter::once(head).chain(rest).collect()
}

const CAP_PATTERN: &str = "usage limit reached|hit your limit|limit reached ∙ resets|limit reached · resets";

const AUTH_PATTERN: &str = "please run /login|invalid api key|oauth token has expired|not logged in|api error: 401|\
    authentication_error|token has been revoked";

fn own_lines(screen: &str) -> String {
    let body: Vec<String> = chat(screen).0.into_iter().map(strip).collect();
    let head = body.iter().rposition(|l| l.starts_with(|c: char| !c.is_whitespace()));
    let block = &body[head.unwrap_or(0)..];
    let theirs = block.first().is_some_and(|l| l.starts_with(['⏺', '•']));
    let own = block.iter().filter(|l| !theirs && !l.starts_with(['❯', '>']));
    own.map(String::as_str).collect::<Vec<_>>().join("\n")
}

static DIALECT: Dialect = Dialect {
    sent: |v| {
        let ev = v["hook_event_name"].as_str().unwrap_or_default();
        ["PreToolUse", "PostToolUse", "Stop"].contains(&ev) && v.get("args").is_none()
    },
    input: &["tool_input"],
    shell: "Bash",
    edits: &["Edit", "Write", "MultiEdit", "NotebookEdit"],
    pre: "PreToolUse",
    post: "PostToolUse",
    stop: "Stop",
    blocked: "Notification",
    deny: |reason| json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}}),
    prompt: "UserPromptSubmit",
    again: Some(|reason| json!({"decision": "block", "reason": reason})),
    rewrite: Some(|ti| json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": ti}})),
};

/// `<config dir>/claude`, the generated per-role profiles.
pub fn root(ctx: &Ctx) -> PathBuf {
    ctx.config.with_file_name("claude")
}

impl Provider for Claude {
    fn vendor(&self) -> Option<&'static str> {
        Some("anthropic")
    }
    fn brands(&self) -> &'static [&'static str] {
        &["claude", "anthropic"]
    }
    fn proxy(&self) -> Option<&'static Proxy> {
        Some(&PROXY)
    }
    fn sync(&self, ctx: &Ctx, role: &str, prompt: &str, profile: &str, usage: &str, out: &mut dyn Write) -> Result<()> {
        let dir = root(ctx).join(profile);
        std::fs::create_dir_all(&dir)?;
        let hook_cmd = serde_json::to_string(&hook_command(ctx, role))?;
        let days = crate::config::Config::load(&ctx.config).map_or(crate::config::limits().transcript_days, |c| c.limits.transcript_days);
        let line = serde_json::to_string(&format!("'{}' statusline", ctx.exe))?;
        let settings = SETTINGS
            .replace("\"{{FLEET_HOOK}}\"", &hook_cmd)
            .replace("\"{{FLEET_STATUSLINE}}\"", &line)
            .replace("\"{{TRANSCRIPT_DAYS}}\"", &days.to_string());
        std::fs::write(dir.join("settings.json"), settings)?;
        let role_md = format!("{}\n\n{usage}", role_prompt(prompt));
        std::fs::write(dir.join("role.md"), role_md)?;
        std::fs::write(dir.join("fleet.md"), usage)?;
        writeln!(out, "synced {} ({prompt})", dir.display())?;
        Ok(())
    }

    fn ready(&self, ctx: &Ctx, _r: &dyn Runner, p: &Plan) -> Result<()> {
        if !root(ctx).join(&p.profile).join("settings.json").exists() {
            bail!("Claude profile {} missing; run `drover profiles sync`", p.profile);
        }
        Ok(())
    }

    fn prepare(&self, _ctx: &Ctx, on: &crate::hosts::On, p: &Plan) -> Result<()> {
        on.write(
            &p.mcp_file,
            &serde_json::to_string_pretty(&serde_json::json!({"mcpServers": p.mcp}))?,
        )
    }

    fn start_cmd(&self, ctx: &Ctx, p: &Plan, pane: &str) -> Vec<String> {
        let dir = root(ctx).join(&p.profile);
        let mut a = vec![
            "--dangerously-skip-permissions".to_string(),
            "--settings".into(),
            dir.join("settings.json").to_string_lossy().into(),
            "--append-system-prompt-file".into(),
            dir.join("role.md").to_string_lossy().into(),
        ];
        if let Some(m) = &p.model {
            a.extend(["--model".into(), m.clone()]);
        }
        if let Some(e) = p.effort.as_deref().filter(|e| *e != "none") {
            a.extend(["--effort".into(), e.into()]);
        }
        a.extend(["--name".into(), p.title.clone()]);
        a.extend([
            "--mcp-config".into(),
            p.mcp_file.to_string_lossy().into(),
            "--strict-mcp-config".into(),
        ]);
        if let Some((id, resume)) = &p.session {
            a.extend([if *resume { "--resume" } else { "--session-id" }.into(), id.clone()]);
        }
        a.push(p.kick.clone());
        term::start_argv("claude", pane, &a)
    }

    fn resumes(&self) -> bool {
        true
    }

    fn takes_model(&self) -> bool {
        true
    }

    fn kick_cmds(&self, p: &Plan, _pane: &str) -> Vec<Vec<String>> {
        vec![term::wait_working_argv(&p.agent, "20000")]
    }

    fn kick(&self, r: &dyn Runner, p: &Plan, pane: &str) -> Result<bool> {
        Ok(term::exec(r, &self.kick_cmds(p, pane)[0])?.code == 0)
    }

    fn env(&self) -> &'static [&'static str] {
        &["CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1", "DISABLE_AUTOUPDATER=1"]
    }
    fn acp(&self) -> Option<&'static str> {
        Some("claude-acp")
    }
    fn acp_env(&self, model: Option<&str>, _effort: Option<&str>) -> Vec<(&'static str, String)> {
        model.map(|m| ("ANTHROPIC_MODEL", m.to_string())).into_iter().collect()
    }
    fn signed_in(&self, r: &dyn Runner, dir: &Path) -> Option<bool> {
        let dir = format!("CLAUDE_CONFIG_DIR={}", dir.display());
        let a = ["-u", "CLAUDE_CODE_OAUTH_TOKEN", &dir, "claude", "auth", "status"];
        let o = r.run("env", &crate::runner::argv(&a)).ok()?;
        serde_json::from_str::<serde_json::Value>(&o.stdout).ok()?["loggedIn"].as_bool()
    }
    fn acp_profile(&self, settings: &str, role: &str) -> String {
        let [md, json] = LOCAL_FILES;
        format!(" && mkdir -p .claude && cp {settings} {json} && cp {role} {md}")
    }
    fn credential(&self) -> Credential {
        Credential::Account(Accounts {
            provider: "claude",
            config_env: "CLAUDE_CONFIG_DIR",
            token_env: Some("CLAUDE_CODE_OAUTH_TOKEN"),
        })
    }
    fn login(&self) -> Option<&'static str> {
        Some("claude /login")
    }
    fn install(&self) -> Option<&'static str> {
        Some("curl -fsSL https://claude.ai/install.sh | bash")
    }
    fn hook_dialect(&self) -> Option<&'static Dialect> {
        Some(&DIALECT)
    }
    fn usage(&self) -> Option<&'static dyn Usage> {
        Some(&meter::Meter)
    }
    fn usage_limited(&self, cfg: &Config, r: &dyn Runner, screen: &str, now: u64) -> Option<u64> {
        let pat = cfg.accounts.cap_pattern.as_deref().unwrap_or(CAP_PATTERN);
        let s = own_lines(screen);
        crate::access::banner(pat, &s).then(|| crate::access::cap_until(cfg, r, &s, now))
    }
    fn signed_out(&self, screen: &str) -> bool {
        crate::access::banner(AUTH_PATTERN, &own_lines(screen))
    }

    fn status_word(&self, screen: &str) -> Option<(String, Option<String>)> {
        let s = strip(chat(screen).1?);
        let (w, rest) = s.split_once(" (")?;
        let mins = |t: &&str| t.strip_suffix(['m', 'h']).is_some_and(|n| n.parse::<u32>().is_ok());
        let t = rest.split([' ', ')']).find(mins).map(String::from);
        w.trim().ends_with('…').then(|| (mask(w.trim()), t))
    }
    fn now_line(&self, line: &str) -> bool {
        line.starts_with('⏺')
    }
    fn tool_line(&self, line: &str) -> Option<(String, String)> {
        let (n, a) = line.strip_prefix(['⏺', '•'])?.trim_start().split_once('(')?;
        let word = !n.is_empty() && n.chars().all(char::is_alphanumeric);
        word.then(|| (n.into(), a.rsplit_once(')').map_or(a, |x| x.0).into()))
    }
    fn pending_menu(&self, screen: &str) -> Option<(String, Vec<String>)> {
        pending(screen)
    }
    fn transcript(&self, home: &Path, wt: &str) -> Option<PathBuf> {
        let d = home.join(".claude/projects");
        let all = std::fs::read_dir(d.join(wt.replace(|c: char| !c.is_ascii_alphanumeric(), "-"))).ok()?;
        let paths = all.flatten().map(|e| e.path());
        let jsonl = paths.filter(|p| p.extension().is_some_and(|x| x == "jsonl"));
        jsonl.max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok())
    }
    fn health_check(&self) -> Option<(&'static str, &'static str, &'static str)> {
        Some(("claude", "claude auth status", "claude auth login in a pane there"))
    }
    fn process_names(&self) -> &'static [&'static str] {
        &["claude"]
    }
}

fn pending(screen: &str) -> Option<(String, Vec<String>)> {
    let lines: Vec<&str> = screen
        .lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '│'))
        .filter(|l| !l.is_empty())
        .collect();
    let opt = |l: &str| {
        let (n, t) = l.trim_start_matches(['❯', '›']).trim_start().split_once(". ")?;
        Some((n.parse::<usize>().ok()?, t.trim().to_string()))
    };
    let question = |ls: &[&str]| {
        ls.iter()
            .rev()
            .take(6)
            .find(|l| l.contains('?'))
            .map(std::string::ToString::to_string)
    };
    let (mut opts, mut i, mut gap) = (
        vec![],
        lines.iter().rposition(|l| opt(l).is_some()).map_or(0, |i| i + 1),
        0,
    );
    while i > 0 && gap <= 2 && opts.last().is_none_or(|o: &(usize, String)| o.0 > 1) {
        i -= 1;
        match opt(lines[i]) {
            Some(o) => (opts.push(o), gap = 0).1,
            None => gap += 1,
        }
    }
    opts.reverse();
    let menu = opts.len() >= 2
        && opts.iter().zip(1..).all(|(o, n)| o.0 == n)
        && lines[i..].iter().any(|l| l.starts_with('❯') && opt(l).is_some());
    if menu {
        Some((
            question(&lines[..i]).or(lines[..i].last().map(std::string::ToString::to_string))?,
            opts.into_iter().map(|o| o.1).collect(),
        ))
    } else {
        Some((question(&lines)?, vec![]))
    }
}

/// #536: removes Claude Code's temp dir for worktree `wt`, `<root>/claude-<uid>/<wt, each non-alphanumeric as ->`.
pub fn remove_tmp(roots: &[PathBuf], wt: &str) {
    let key: String = wt
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    for root in roots {
        let ls = std::fs::read_dir(root).into_iter().flatten().flatten();
        for d in ls.filter(|e| e.file_name().to_string_lossy().starts_with("claude-")) {
            let _ = std::fs::remove_dir_all(d.path().join(&key));
        }
    }
}
