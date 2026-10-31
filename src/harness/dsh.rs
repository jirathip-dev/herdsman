//! dsh-TUI lanes (docs/dsh.md): a per-role patch layer, a pinned `dsh-tui` typed into the shell, a git pre-push hook.
use super::Harness;
use crate::runner::{argv, json, ok, Runner};
use crate::spawn::Plan;
use crate::Ctx;
use anyhow::{bail, Context, Result};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The dsh-TUI profile version lanes run; `herdsman spawn` refuses any other.
pub const DSH_TUI_VERSION: &str = "0.17.0";
/// The agent label dsh-TUI reports to herdr.
const LABEL: &str = "dsh-tui";
/// The first-run wizard's title (en, zh); it reports idle but eats the prompt.
const WIZARD: [&str; 2] = ["Welcome to dsh-TUI", "欢迎使用 dsh-TUI"];

pub struct Dsh;

/// `<config dir>/dsh/<profile>.patch.yml`, the lane's `--patch` overlay.
fn patch_file(ctx: &Ctx, profile: &str) -> PathBuf {
    ctx.claude_dir().with_file_name("dsh").join(format!("{profile}.patch.yml"))
}

/// Reviewers get the workspace-write sandbox; escalations are never asked (nobody answers them).
fn patch(role: &str) -> String {
    let mode = if role == "rev" { "workspace-write" } else { "danger-full-access" };
    format!(
        "# Written by `herdsman profiles sync` for {role} lanes; edits are overwritten.\n\
         - id: sandbox-policy\n  config:\n    mode: {mode}\n    workspaceRoot: !!js process.cwd()\n\
         - id: approval\n  config:\n    policy: never\n"
    )
}

impl Harness for Dsh {
    fn sync(&self, ctx: &Ctx, role: &str, profile: &str, _usage: &str, out: &mut dyn Write) -> Result<()> {
        let f = patch_file(ctx, profile);
        std::fs::create_dir_all(f.parent().context("patch dir")?)?;
        std::fs::write(&f, patch(role))?;
        writeln!(out, "synced {} ({role})", f.display())?;
        Ok(())
    }

    fn ready(&self, ctx: &Ctx, r: &dyn Runner, p: &Plan) -> Result<()> {
        if !patch_file(ctx, &p.profile).exists() {
            bail!("DSH profile {} missing; run `herdsman profiles sync`", p.profile);
        }
        let v = ok(r, "dsh-tui", &["version"]).context("dsh-tui is not installed")?;
        let profile = v.lines().find_map(|l| l.strip_prefix("profile:")).and_then(|l| l.split_whitespace().next());
        if profile != Some(DSH_TUI_VERSION) {
            bail!("dsh-tui profile is {}, fleet pins {DSH_TUI_VERSION}", profile.unwrap_or("unknown"));
        }
        Ok(())
    }

    /// Issue lanes only: the orch lane runs in the fleet's main checkout.
    fn prepare(&self, ctx: &Ctx, r: &dyn Runner, p: &Plan) -> Result<()> {
        if p.role == "orch" {
            return Ok(());
        }
        install_pre_push(r, &ctx.exe, &p.worktree, &p.role)
    }

    /// herdr has no `dsh` kind: type the launch into the lane's shell.
    fn start_cmd(&self, ctx: &Ctx, p: &Plan, pane: &str) -> Vec<String> {
        let patch = format!("'{}'", patch_file(ctx, &p.profile).display());
        argv(&["pane", "run", pane, "dsh-tui", "--patch", &patch])
    }

    /// Settle idle (Esc skips the wizard), name the agent, type kick + Enter, confirm working/blocked/done.
    fn kick_cmds(&self, p: &Plan, pane: &str) -> Vec<Vec<String>> {
        vec![
            argv(&["pane", "get", pane]),
            argv(&["pane", "read", pane, "--source", "visible"]),
            argv(&["pane", "send-keys", pane, "esc"]),
            argv(&["agent", "rename", pane, &p.agent]),
            argv(&["pane", "send-text", pane, &p.kick]),
            argv(&["pane", "get", pane]),
            argv(&["pane", "send-keys", pane, "enter"]),
        ]
    }

    /// Up to 3 tries 5 s apart; a kick left typed in the input only gets Enter.
    fn kick(&self, r: &dyn Runner, p: &Plan, pane: &str) -> Result<bool> {
        let c = self.kick_cmds(p, pane);
        if !poll(r, &c[0], &["idle"], 90) {
            return Ok(false);
        }
        let screen = r.run("herdr", &c[1])?.stdout;
        if WIZARD.iter().any(|w| screen.contains(w)) {
            r.run("herdr", &c[2])?;
            r.sleep(2);
            if !poll(r, &c[0], &["idle"], 30) {
                return Ok(false);
            }
        }
        r.run("herdr", &c[3])?;
        let typed: String = p.kick.chars().take(20).collect();
        for attempt in 0..3 {
            if attempt > 0 {
                r.sleep(5);
            }
            if attempt == 0 || !r.run("herdr", &c[1])?.stdout.contains(&typed) {
                r.run("herdr", &c[4])?;
            }
            r.run("herdr", &c[6])?;
            if poll(r, &c[5], &["working", "blocked", "done"], 10) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Re-read the pane (1 s apart, `tries` times) until dsh-TUI reports one of `want`.
fn poll(r: &dyn Runner, get: &[String], want: &[&str], tries: u32) -> bool {
    for i in 0..tries {
        if i > 0 {
            r.sleep(1);
        }
        let pane = json(r, "herdr", get).map(|v| v["result"]["pane"].clone()).unwrap_or_default();
        if pane["agent"] == LABEL && want.iter().any(|w| pane["agent_status"] == *w) {
            return true;
        }
    }
    false
}

/// A worktree-only `pre-push` hook (`core.hooksPath`) running `herdsman hook --git-pre-push`.
pub fn install_pre_push(r: &dyn Runner, exe: &str, wt: &Path, role: &str) -> Result<()> {
    let w = wt.to_string_lossy();
    let gd = PathBuf::from(ok(r, "git", &["-C", &w, "rev-parse", "--absolute-git-dir"])?.trim());
    if !gd.is_absolute() {
        bail!("{w}: no git dir for the pre-push hook");
    }
    let dir = gd.join("fleet-hooks");
    std::fs::create_dir_all(&dir)?;
    let hook = dir.join("pre-push");
    std::fs::write(&hook, format!("#!/bin/sh\nexec '{exe}' hook --role {role} --git-pre-push\n"))?;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
    ok(r, "git", &["-C", &w, "config", "extensions.worktreeConfig", "true"])?;
    ok(r, "git", &["-C", &w, "config", "--worktree", "core.hooksPath", &dir.to_string_lossy()])?;
    Ok(())
}
