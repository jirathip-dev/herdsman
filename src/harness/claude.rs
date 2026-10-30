//! Claude Code: per-role settings.json (PreToolUse hook) + role.md; the kick goes in as argv.
use super::{agent_start, hook_command, wait_working, Harness};
use crate::runner::Runner;
use crate::spawn::{profile_dir, role_prompt, Plan};
use crate::Ctx;
use anyhow::{bail, Result};
use std::io::Write;

const SETTINGS: &str = include_str!("../../hooks/claude-settings.json");

pub struct Claude;

impl Harness for Claude {
    fn sync(&self, ctx: &Ctx, role: &str, profile: &str, usage: &str, out: &mut dyn Write) -> Result<()> {
        let dir = profile_dir(ctx, profile);
        std::fs::create_dir_all(&dir)?;
        let hook_cmd = serde_json::to_string(&hook_command(ctx, role))?;
        let settings = SETTINGS.replace("\"{{FLEET_HOOK}}\"", &hook_cmd);
        std::fs::write(dir.join("settings.json"), settings)?;
        std::fs::write(dir.join("role.md"), format!("{}\n\n{usage}", role_prompt(role)))?;
        std::fs::write(dir.join("fleet.md"), usage)?;
        writeln!(out, "synced {} ({role})", dir.display())?;
        Ok(())
    }

    fn ready(&self, ctx: &Ctx, _r: &dyn Runner, p: &Plan) -> Result<()> {
        if !profile_dir(ctx, &p.profile).join("settings.json").exists() {
            bail!("Claude profile {} missing; run `herdsman profiles sync`", p.profile);
        }
        Ok(())
    }

    fn start_cmd(&self, ctx: &Ctx, p: &Plan, pane: &str) -> Vec<String> {
        let dir = profile_dir(ctx, &p.profile);
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
        a.push(p.kick.clone());
        agent_start(p, "claude", pane, a)
    }

    /// Claude gets the kick as argv, so it only waits.
    fn kick_cmds(&self, p: &Plan, _pane: &str) -> Vec<Vec<String>> {
        vec![wait_working(p, "20000")]
    }

    fn kick(&self, r: &dyn Runner, p: &Plan, pane: &str) -> Result<bool> {
        Ok(r.run("herdr", &self.kick_cmds(p, pane)[0])?.code == 0)
    }
}
