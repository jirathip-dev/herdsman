//! Hermes: the `hooks:` key (pre_tool_call hook) + its consent in the role's existing
//! profile; boots idle, so the kick is typed once the `❯` prompt is up.
use super::{agent_start, hook_command, wait_working, Harness};
use crate::runner::{argv, Runner};
use crate::spawn::Plan;
use crate::Ctx;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;

const HERMES_HOOKS: &str = include_str!("../../hooks/hermes-hooks.yaml");

pub struct Hermes;

impl Harness for Hermes {
    fn sync(&self, ctx: &Ctx, role: &str, profile: &str, _usage: &str, out: &mut dyn Write) -> Result<()> {
        let yaml = ctx.hermes.join(profile).join("config.yaml");
        let Ok(text) = std::fs::read_to_string(&yaml) else { return Ok(()) };
        let hook_cmd = serde_json::to_string(&hook_command(ctx, role))?;
        let new = set_hooks(&text, &HERMES_HOOKS.replace("{{FLEET_HOOK}}", &hook_cmd));
        if new != text {
            std::fs::write(&yaml, new)?;
        }
        writeln!(out, "synced {} hooks ({role})", yaml.display())?;
        approve_hook(&ctx.hermes.join(profile).join("shell-hooks-allowlist.json"), &hook_command(ctx, role))
    }

    fn start_cmd(&self, _ctx: &Ctx, p: &Plan, pane: &str) -> Vec<String> {
        // Unattended lanes cannot answer Hermes' first-use hook consent prompt.
        agent_start(p, "hermes", pane, argv(&["-p", &p.profile, "--accept-hooks"]))
    }

    /// Hermes boots idle and drops a prompt sent before its input is ready: wait for
    /// the `❯` prompt, prompt + Enter, then confirm by `working` or by the screen
    /// leaving `ctx --`.
    fn kick_cmds(&self, p: &Plan, pane: &str) -> Vec<Vec<String>> {
        vec![
            argv(&["pane", "wait-output", pane, "--match", "❯", "--source", "visible", "--timeout", "60000"]),
            argv(&["agent", "prompt", &p.agent, &p.kick]),
            argv(&["pane", "send-keys", pane, "Enter"]),
            wait_working(p, "10000"),
            argv(&["pane", "read", pane, "--source", "visible"]),
        ]
    }

    fn kick(&self, r: &dyn Runner, p: &Plan, pane: &str) -> Result<bool> {
        let c = self.kick_cmds(p, pane);
        r.run("herdr", &c[0])?;
        for attempt in 0..3 {
            if attempt > 0 {
                r.sleep(10);
            }
            r.run("herdr", &c[1])?;
            r.run("herdr", &c[2])?;
            if r.run("herdr", &c[3])?.code == 0 {
                return Ok(true);
            }
            let screen = r.run("herdr", &c[4])?;
            if screen.code == 0 && screen.stdout.contains("ctx ") && !screen.stdout.contains("ctx --") {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Record Hermes' consent for the fleet `pre_tool_call` hook, as its first-use
/// prompt would: unconsented hooks never register on non-TTY runs. Other
/// approvals are kept; an existing identical approval leaves the file untouched.
fn approve_hook(path: &Path, cmd: &str) -> Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut data: Value = serde_json::from_str(&text).ok().filter(Value::is_object).unwrap_or_else(|| json!({}));
    if !data["approvals"].is_array() {
        data["approvals"] = json!([]);
    }
    let list = data["approvals"].as_array_mut().context("approvals")?;
    if list.iter().any(|e| e["event"] == "pre_tool_call" && e["command"] == cmd) {
        return Ok(());
    }
    list.push(json!({"event": "pre_tool_call", "command": cmd}));
    crate::config::write_atomic(path, &serde_json::to_string_pretty(&data)?)
}

/// Write only the role's MCP servers under the fleet profile's `mcp_servers:`
/// (removed when there are none). Profiles that do not exist are left alone.
pub fn set_mcp(ctx: &Ctx, profile: &str, servers: &serde_json::Map<String, Value>) -> Result<()> {
    let yaml = ctx.hermes.join(profile).join("config.yaml");
    let Ok(text) = std::fs::read_to_string(&yaml) else { return Ok(()) };
    // JSON is YAML flow syntax: one line per server.
    let block: String = servers.iter().map(|(k, v)| format!("  {}: {v}\n", serde_json::to_string(k).unwrap())).collect();
    let block = if block.is_empty() { block } else { format!("mcp_servers:\n{block}") };
    let new = set_block(&text, "mcp_servers:", &block);
    if new != text {
        std::fs::write(&yaml, new)?;
    }
    Ok(())
}

/// Replace the top-level `hooks:` block of a Hermes config.yaml with `block`, in
/// place (appended when absent); every other line is kept byte for byte.
pub fn set_hooks(text: &str, block: &str) -> String {
    set_block(text, "hooks:", block)
}

/// Replace the top-level block starting with `key` by `block` (appended when absent;
/// an empty `block` removes it).
fn set_block(text: &str, key: &str, block: &str) -> String {
    let (mut out, mut skip, mut done) = (String::new(), false, false);
    for line in text.split_inclusive('\n') {
        if !line.trim().is_empty() && !line.starts_with([' ', '\t']) {
            skip = line.starts_with(key);
            if skip && !done {
                out.push_str(block);
                done = true;
            }
        }
        if !skip {
            out.push_str(line);
        }
    }
    if !done && !block.is_empty() {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(block);
    }
    out
}
