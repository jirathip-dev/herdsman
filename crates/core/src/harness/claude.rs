//! Claude Code: per-role settings.json (`PreToolUse` hook) + role.md; the kick goes in as argv.
use super::{hook_command, Credential, Dialect, Harness, UsageMeter};
use crate::config::Config;
use crate::herdr;
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

/// `|`-separated, case-insensitive substrings of Claude Code's usage-limit banner (`[accounts].cap_pattern` overrides).
pub const CAP_PATTERN: &str = "usage limit reached|hit your limit|limit reached ∙ resets|limit reached · resets";

/// Claude Code's signed-out errors (#302), as `CAP_PATTERN`.
const AUTH_PATTERN: &str = "please run /login|invalid api key|oauth token has expired|not logged in";

/// Claude Code's `PreToolUse` / `PostToolUse` / `Stop` hooks.
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
};

/// `<config dir>/claude`, the generated per-role profiles.
pub fn root(ctx: &Ctx) -> PathBuf {
    ctx.config.with_file_name("claude")
}

impl Harness for Claude {
    fn sync(&self, ctx: &Ctx, role: &str, prompt: &str, profile: &str, usage: &str, out: &mut dyn Write) -> Result<()> {
        let dir = root(ctx).join(profile);
        std::fs::create_dir_all(&dir)?;
        let hook_cmd = serde_json::to_string(&hook_command(ctx, role))?;
        let days = crate::config::Config::load(&ctx.config).map_or(30, |c| c.limits.transcript_days);
        let line = serde_json::to_string(&format!("'{}' statusline", ctx.exe))?;
        let settings = SETTINGS
            .replace("\"{{FLEET_HOOK}}\"", &hook_cmd)
            .replace("\"{{FLEET_STATUSLINE}}\"", &line)
            .replace("\"{{TRANSCRIPT_DAYS}}\"", &days.to_string());
        std::fs::write(dir.join("settings.json"), settings)?;
        let role_md = format!("{}\n\n{usage}", role_prompt(prompt));
        std::fs::write(dir.join("role.md"), &role_md)?;
        // #565: an ACP lane (the profile as `CLAUDE_CONFIG_DIR`, `bypassPermissions` as the CLI's flag) reads its role
        // as user memory, since the adapter takes no `--append-system-prompt-file`.
        std::fs::write(dir.join("CLAUDE.md"), role_md)?;
        std::fs::write(dir.join("fleet.md"), usage)?;
        writeln!(out, "synced {} ({prompt})", dir.display())?;
        Ok(())
    }

    fn ready(&self, ctx: &Ctx, _r: &dyn Runner, p: &Plan) -> Result<()> {
        if !root(ctx).join(&p.profile).join("settings.json").exists() {
            bail!("Claude profile {} missing; run `herdsman profiles sync`", p.profile);
        }
        Ok(())
    }

    /// The lane's MCP file: only the role's servers.
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
        // Display name = terminal title on the herdr board (else Claude titles it from the kick).
        a.extend(["--name".into(), p.title.clone()]);
        // `--mcp-config` is variadic: `--strict-mcp-config` ends it before the kick.
        a.extend([
            "--mcp-config".into(),
            p.mcp_file.to_string_lossy().into(),
            "--strict-mcp-config".into(),
        ]);
        if let Some((id, resume)) = &p.session {
            a.extend([if *resume { "--resume" } else { "--session-id" }.into(), id.clone()]);
        }
        a.push(p.kick.clone());
        herdr::start_argv(&p.agent, "claude", pane, a)
    }

    fn resumes(&self) -> bool {
        true
    }

    fn takes_model(&self) -> bool {
        true
    }

    /// Claude gets the kick as argv, so it only waits.
    fn kick_cmds(&self, p: &Plan, _pane: &str) -> Vec<Vec<String>> {
        vec![herdr::wait_working_argv(&p.agent, "20000")]
    }

    fn kick(&self, r: &dyn Runner, p: &Plan, pane: &str) -> Result<bool> {
        Ok(herdr::exec(r, &self.kick_cmds(p, pane)[0])?.code == 0)
    }

    /// #248: no updater, telemetry or other background traffic (names as of Claude Code 2.1.311).
    fn env(&self) -> &'static [&'static str] {
        &["CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1", "DISABLE_AUTOUPDATER=1"]
    }
    /// #565: claude-agent-acp loads Claude Code's user settings, so a lane whose `CLAUDE_CONFIG_DIR` is its profile
    /// runs that profile's `PreToolUse` guard.
    fn acp(&self) -> Option<&'static str> {
        Some("claude-acp")
    }
    fn acp_env(&self, profile: &Path, model: Option<&str>) -> Vec<(&'static str, String)> {
        let mut env = vec![("CLAUDE_CONFIG_DIR", profile.to_string_lossy().into_owned())];
        env.extend(model.map(|m| ("ANTHROPIC_MODEL", m.to_string())));
        env
    }
    /// The env var Claude Code reads its OAuth token from.
    fn credential(&self) -> Credential {
        Credential::Token("CLAUDE_CODE_OAUTH_TOKEN")
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
    fn usage_meter(&self) -> Option<&'static dyn UsageMeter> {
        Some(&super::claude_meter::Meter)
    }
    fn usage_limited(&self, cfg: &Config, r: &dyn Runner, screen: &str, now: u64) -> Option<u64> {
        let pat = cfg.accounts.cap_pattern.as_deref().unwrap_or(CAP_PATTERN);
        crate::access::banner(pat, screen).then(|| crate::access::cap_until(cfg, r, screen, now))
    }
    fn signed_out(&self, screen: &str) -> bool {
        AUTH_PATTERN.split('|').any(|p| screen.to_lowercase().contains(p))
    }

    /// #276: `✻ Herding… (4m 12s · ↓ 3k tokens)` is (`✻ Herding…`, `4m`).
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
    /// `⏺ Bash(cargo test)` (or `•`).
    fn tool_line(&self, line: &str) -> Option<(String, String)> {
        let (n, a) = line.strip_prefix(['⏺', '•'])?.trim_start().split_once('(')?;
        let word = !n.is_empty() && n.chars().all(char::is_alphanumeric);
        word.then(|| (n.into(), a.rsplit_once(')').map_or(a, |x| x.0).into()))
    }
    fn pending_menu(&self, screen: &str) -> Option<(String, Vec<String>)> {
        pending(screen)
    }
    /// #463: a new folder's trust dialog or the bypass-permissions warning (Claude Code 2.x): its "Yes" option, by its
    /// number or (#476: an unnumbered `❯` menu) by moving the cursor to it, then Enter.
    fn startup_prompt(&self, screen: &str) -> Option<(&'static str, Vec<String>)> {
        let (name, _) = FIRST_RUN.iter().find(|(_, t)| screen.contains(t))?;
        let yes = |o: &Vec<String>| o.iter().position(|o| o.starts_with("Yes"));
        if let Some(n) = pending(screen).and_then(|p| yes(&p.1)) {
            return Some((name, vec![self.answer_keys(n + 1)]));
        }
        let (at, opts) = cursor_menu(screen)?;
        let n = yes(&opts)?;
        let key = if n > at { "Down" } else { "Up" };
        let mut keys = vec![key.to_string(); n.abs_diff(at)];
        keys.push("Enter".into());
        Some((name, keys))
    }

    /// #260: `<home>/.claude/projects/<worktree, non-alphanumerics as `-`>/*.jsonl`.
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
    fn startup_title(&self) -> Option<&'static str> {
        Some("✳")
    }
    fn process_names(&self) -> &'static [&'static str] {
        &["claude"]
    }
}

/// #463: first-start screens by a line they show: `Do you trust the files in this folder?` (1.x), `Yes, I trust this
/// folder` (2.x) and the `--dangerously-skip-permissions` warning, whose "No, exit" comes first.
const FIRST_RUN: [(&str, &str); 3] = [
    ("folder-trust", "trust the files in this folder"),
    ("folder-trust", "trust this folder"),
    ("bypass-permissions", "Bypass Permissions mode"),
];

/// #476: an unnumbered menu: the rows at the `❯` cursor's indent next to it, and the cursor's row among them.
/// #484: the last `❯` on screen, not the shell prompt's (starship's `❯ claude …`) above the dialog.
fn cursor_menu(screen: &str) -> Option<(usize, Vec<String>)> {
    let lines: Vec<&str> = screen.lines().collect();
    let c = lines.iter().rposition(|l| l.trim_start().starts_with('❯'))?;
    let indent = |l: &str| l.chars().take_while(|c| c.is_whitespace()).count();
    let col = indent(lines[c]) + 2;
    let row = |l: &&&str| indent(l) == col && !l.trim().is_empty();
    let top = c - lines[..c].iter().rev().take_while(row).count();
    let end = c + 1 + lines[c + 1..].iter().take_while(row).count();
    let text = |l: &str| l.trim_start().trim_start_matches('❯').trim().to_string();
    Some((c - top, lines[top..end].iter().map(|l| text(l)).collect()))
}

/// A blocked pane's pending prompt: the question and a Claude menu's options (a `❯`-marked run
/// numbered 1, 2, …; wrapped lines skipped), or no options for a free-text question. None: no prompt seen.
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
