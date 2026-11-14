use super::{attempts, Credential, Provider};
use crate::term;
use crate::runner::{ok, Runner};
use crate::spawn::Plan;
use crate::Ctx;
use anyhow::{bail, Context, Result};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The dsh-TUI profile version lanes run; `drover spawn` refuses any other.
pub const DSH_TUI_VERSION: &str = "0.17.0";
const LABEL: &str = "dsh-tui";
const WIZARD: &[&str] = &["Welcome to dsh-TUI", "欢迎使用 dsh-TUI"];

pub struct Dsh;
pub static PROVIDER: Dsh = Dsh;

fn patch_file(ctx: &Ctx, profile: &str) -> PathBuf {
    ctx.config.with_file_name("dsh").join(format!("{profile}.patch.yml"))
}

fn patch(role: &str) -> String {
    let mode = if role == "rev" {
        "workspace-write"
    } else {
        "danger-full-access"
    };
    format!(
        "# Written by `drover profiles sync` for {role} lanes; edits are overwritten.\n\
         - id: sandbox-policy\n  config:\n    mode: {mode}\n    workspaceRoot: !!js process.cwd()\n\
         - id: approval\n  config:\n    policy: never\n"
    )
}

impl Provider for Dsh {
    fn efforts(&self) -> &'static [&'static str] {
        &["none"]
    }
    fn sync(
        &self,
        ctx: &Ctx,
        role: &str,
        _prompt: &str,
        profile: &str,
        _usage: &str,
        out: &mut dyn Write,
    ) -> Result<()> {
        let f = patch_file(ctx, profile);
        std::fs::create_dir_all(f.parent().context("patch dir")?)?;
        std::fs::write(&f, patch(role))?;
        writeln!(out, "synced {} ({role})", f.display())?;
        Ok(())
    }

    fn ready(&self, ctx: &Ctx, r: &dyn Runner, p: &Plan) -> Result<()> {
        if !patch_file(ctx, &p.profile).exists() {
            bail!("DSH profile {} missing; run `drover profiles sync`", p.profile);
        }
        let v = ok(r, "dsh-tui", &["version"]).context("dsh-tui is not installed")?;
        let profile = v
            .lines()
            .find_map(|l| l.strip_prefix("profile:"))
            .and_then(|l| l.split_whitespace().next());
        if profile != Some(DSH_TUI_VERSION) {
            bail!(
                "dsh-tui profile is {}, fleet pins {DSH_TUI_VERSION}",
                profile.unwrap_or("unknown")
            );
        }
        Ok(())
    }

    fn prepare(&self, ctx: &Ctx, on: &crate::hosts::On, p: &Plan) -> Result<()> {
        if p.branch.is_empty() {
            return Ok(());
        }
        install_pre_push(on, &ctx.exe, &p.worktree, p.family)
    }

    fn start_cmd(&self, ctx: &Ctx, p: &Plan, pane: &str) -> Vec<String> {
        let patch = format!("'{}'", patch_file(ctx, &p.profile).display());
        term::run_argv(pane, &["dsh-tui", "--patch", &patch])
    }

    fn kick_cmds(&self, p: &Plan, pane: &str) -> Vec<Vec<String>> {
        vec![
            term::pane_get_argv(pane),
            term::read_argv(pane),
            term::keys_argv(pane, "esc"),
            term::send_text_argv(pane, &p.kick),
            term::pane_get_argv(pane),
            term::keys_argv(pane, "enter"),
        ]
    }

    fn kick(&self, r: &dyn Runner, p: &Plan, pane: &str) -> Result<bool> {
        let c = self.kick_cmds(p, pane);
        if !poll(r, &c[0], &["idle"], 90) {
            return Ok(false);
        }
        let screen = term::exec(r, &c[1])?.stdout;
        if WIZARD.iter().any(|w| screen.contains(w)) {
            term::exec(r, &c[2])?;
            r.sleep(2);
            if !poll(r, &c[0], &["idle"], 30) {
                return Ok(false);
            }
        }
        let typed: String = p.kick.chars().take(20).collect();
        for attempt in attempts(r, 3, 5) {
            if attempt == 0 || !term::exec(r, &c[1])?.stdout.contains(&typed) {
                term::exec(r, &c[3])?;
            }
            term::exec(r, &c[5])?;
            if poll(r, &c[4], &["working", "blocked", "done"], 10) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn credential(&self) -> Credential {
        Credential::Own
    }
    fn process_names(&self) -> &'static [&'static str] {
        &[LABEL]
    }
}

fn poll(r: &dyn Runner, get: &[String], want: &[&str], tries: u32) -> bool {
    for _ in attempts(r, tries, 1) {
        let pane = term::json(r, get)
            .map(|v| v["result"]["pane"].clone())
            .unwrap_or_default();
        if pane["agent"].is_string() && want.iter().any(|w| pane["agent_status"] == *w) {
            return true;
        }
    }
    false
}

/// A worktree-only `pre-push` hook (`core.hooksPath`) running `drover hook --git-pre-push`.
pub fn install_pre_push(r: &dyn Runner, exe: &str, wt: &Path, role: &str) -> Result<()> {
    let w = wt.to_string_lossy();
    let gd = PathBuf::from(ok(r, "git", &["-C", &w, "rev-parse", "--absolute-git-dir"])?.trim());
    if !gd.is_absolute() {
        bail!("{w}: no git dir for the pre-push hook");
    }
    let dir = gd.join("fleet-hooks");
    std::fs::create_dir_all(&dir)?;
    let hook = dir.join("pre-push");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\nexec '{exe}' hook --role {role} --git-pre-push\n"),
    )?;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
    ok(r, "git", &["-C", &w, "config", "extensions.worktreeConfig", "true"])?;
    let hooks = dir.to_string_lossy();
    ok(r, "git", &["-C", &w, "config", "--worktree", "core.hooksPath", &hooks])?;
    Ok(())
}
