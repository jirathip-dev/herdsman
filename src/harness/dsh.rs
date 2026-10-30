//! dsh-TUI (beta TUI over DeepSeek Harness): a per-role DSH patch layer, a pinned
//! `dsh-tui` typed into the lane's shell, and a git `pre-push` hook in place of the
//! pre-tool hook DSH lacks. dsh-TUI reports its own state to herdr (agent `dsh-tui`).
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

/// Reviewers get the workspace-write sandbox (git metadata, ~/.cargo and the rest of
/// the disk stay unwritable); escalations are never asked, since nobody answers them.
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

    /// Settle on `dsh-tui` reporting idle (Esc skips the first-run wizard), name the
    /// lane's agent (best effort: dsh-TUI's own reports drop the name), type the kick
    /// and Enter (`herdr agent prompt` takes only agents herdr started), then confirm
    /// by the label going working, blocked or (a quick turn) done.
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
        let args: Vec<&str> = get.iter().map(String::as_str).collect();
        let pane = json(r, "herdr", &args).map(|v| v["result"]["pane"].clone()).unwrap_or_default();
        if pane["agent"] == LABEL && want.iter().any(|w| pane["agent_status"] == *w) {
            return true;
        }
    }
    false
}

/// Install a `pre-push` hook for this worktree only (`core.hooksPath` in its
/// worktree config) that runs `herdsman hook --git-pre-push`: pushes to protected
/// branches are refused, and every push from a reviewer lane.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::Out;

    /// Answers `pane get` idle, then working; records every herdr call.
    struct Fake(std::cell::RefCell<Vec<String>>);

    impl Runner for Fake {
        fn run(&self, _: &str, args: &[String]) -> Result<Out> {
            let mut calls = self.0.borrow_mut();
            calls.push(args.join(" "));
            let gets = calls.iter().filter(|c| c.starts_with("pane get")).count();
            let status = if gets == 1 { "idle" } else { "working" };
            let stdout = if args[1] == "get" {
                format!(r#"{{"result":{{"pane":{{"agent":"dsh-tui","agent_status":"{status}"}}}}}}"#)
            } else {
                String::new()
            };
            Ok(Out { code: 0, stdout, stderr: String::new() })
        }
        fn sleep(&self, _: u64) {}
    }

    /// Byte 20 of this kick falls inside `語`: the typed-kick check must not panic.
    #[test]
    fn kick_with_non_ascii_path_does_not_panic() {
        let wt = PathBuf::from("/Users/日本語/wt");
        let s = String::new;
        let p = Plan {
            agent: "a".into(), role: "rev".into(), harness: "dsh".into(), profile: s(), model: None, effort: None,
            branch: s(), base: s(), worktree: wt.clone(), brief: wt.join(".brief.md"),
            kick: "Read /Users/日本語/wt/.brief.md and execute it fully.".into(), lane: s(),
            account: None, mcp: Default::default(), mcp_file: wt.join(".mcp.lane.json"),
        };
        assert!(!p.kick.is_char_boundary(20));
        let r = Fake(Default::default());
        assert!(Dsh.kick(&r, &p, "w9:p1").unwrap());
        assert!(r.0.borrow().iter().any(|c| c.starts_with("pane send-text w9:p1 Read /Users/日本語")));
    }
}
