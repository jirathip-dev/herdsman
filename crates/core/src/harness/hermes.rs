//! Hermes: the `hooks:` key + its consent in the role's existing profile; the kick is typed at the `❯` prompt.
use super::{attempts, hook_command, Credential, Dialect, Harness};
use crate::herdr;
use crate::runner::{argv, Runner};
use crate::spawn::Plan;
use crate::Ctx;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;

const HERMES_HOOKS: &str = include_str!("../../hooks/hermes-hooks.yaml");

pub struct Hermes;

/// Hermes' `pre_tool_call` / `post_tool_call` / `on_session_end` shell hooks (older ones: `args`, no event name).
static DIALECT: Dialect = Dialect {
    sent: |v| {
        let ev = v["hook_event_name"].as_str().unwrap_or_default();
        ["pre_tool_call", "post_tool_call", "on_session_end"].contains(&ev) || v.get("args").is_some()
    },
    input: &["tool_input", "args"],
    shell: "terminal",
    edits: &["patch", "write_file"],
    pre: "pre_tool_call",
    post: "post_tool_call",
    stop: "on_session_end",
    blocked: "",
    deny: |reason| json!({"action": "block", "message": reason}),
    prompt: "",
    again: None,
};

impl Harness for Hermes {
    fn sync(
        &self,
        ctx: &Ctx,
        role: &str,
        _prompt: &str,
        profile: &str,
        _usage: &str,
        out: &mut dyn Write,
    ) -> Result<()> {
        let yaml = ctx.hermes.join(profile).join("config.yaml");
        let Ok(text) = std::fs::read_to_string(&yaml) else {
            return Ok(());
        };
        let hook_cmd = serde_json::to_string(&hook_command(ctx, role))?;
        let new = set_block(
            &drop_reasoning_effort(&text),
            "hooks:",
            &HERMES_HOOKS.replace("{{FLEET_HOOK}}", &hook_cmd),
        );
        if new != text {
            std::fs::write(&yaml, new)?;
        }
        writeln!(out, "synced {} hooks ({role})", yaml.display())?;
        approve_hook(
            &ctx.hermes.join(profile).join("shell-hooks-allowlist.json"),
            &hook_command(ctx, role),
        )
    }

    fn start_cmd(&self, _ctx: &Ctx, p: &Plan, pane: &str) -> Vec<String> {
        // Unattended lanes cannot answer Hermes' first-use hook consent prompt.
        let mut a = argv(&["-p", &p.profile, "--accept-hooks"]);
        if let Some(e) = &p.effort {
            a.extend(["--reasoning".into(), e.clone()]);
        }
        herdr::start_argv(&p.agent, "hermes", pane, a)
    }

    /// Wait for `❯` (an early prompt is dropped), prompt + Enter, confirm by `working` or the screen leaving `ctx --`.
    fn kick_cmds(&self, p: &Plan, pane: &str) -> Vec<Vec<String>> {
        vec![
            herdr::wait_output_argv(pane, "❯", "60000"),
            herdr::prompt_argv(&p.agent, &p.kick),
            herdr::keys_argv(pane, "Enter"),
            herdr::wait_working_argv(&p.agent, "10000"),
            herdr::read_argv(pane),
        ]
    }

    fn kick(&self, r: &dyn Runner, p: &Plan, pane: &str) -> Result<bool> {
        let c = self.kick_cmds(p, pane);
        herdr::exec(r, &c[0])?;
        for _ in attempts(r, 3, 10) {
            herdr::exec(r, &c[1])?;
            herdr::exec(r, &c[2])?;
            if herdr::exec(r, &c[3])?.code == 0 {
                return Ok(true);
            }
            let screen = herdr::exec(r, &c[4])?;
            if screen.code == 0 && screen.stdout.contains("ctx ") && !screen.stdout.contains("ctx --") {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn credential(&self) -> Credential {
        Credential::Own
    }
    fn hook_dialect(&self) -> Option<&'static Dialect> {
        Some(&DIALECT)
    }
    fn now_line(&self, line: &str) -> bool {
        line.contains('💻') || line.starts_with('┊')
    }
    /// `┊ 💻 cargo test` (or `$ …`) as a shell call.
    fn tool_line(&self, line: &str) -> Option<(String, String)> {
        let t = line.trim_start_matches(['┊', ' ']);
        let cmd = t.strip_prefix("💻").or(t.strip_prefix("$ "))?.trim_start();
        Some(("$".into(), cmd.strip_prefix("$ ").unwrap_or(cmd).into()))
    }
}

/// Record Hermes' first-use consent for the hook (unconsented hooks never register on non-TTY runs), keeping others.
fn approve_hook(path: &Path, cmd: &str) -> Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut data: Value = serde_json::from_str(&text)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    if !data["approvals"].is_array() {
        data["approvals"] = json!([]);
    }
    let list = data["approvals"].as_array_mut().context("approvals")?;
    // #304: `post_tool_call` kicks the issue after a push.
    let n = list.len();
    for ev in ["pre_tool_call", "post_tool_call"] {
        if !list.iter().any(|e| e["event"] == ev && e["command"] == cmd) {
            list.push(json!({"event": ev, "command": cmd}));
        }
    }
    if list.len() == n {
        return Ok(());
    }
    crate::config::write_atomic(path, &serde_json::to_string_pretty(&data)?)
}

/// Set an existing profile's `mcp_servers:` to only the role's servers (removed when there are none).
pub fn set_mcp(ctx: &Ctx, profile: &str, servers: &serde_json::Map<String, Value>) -> Result<()> {
    let yaml = ctx.hermes.join(profile).join("config.yaml");
    let Ok(text) = std::fs::read_to_string(&yaml) else {
        return Ok(());
    };
    // JSON is YAML flow syntax: one line per server.
    let block: String = servers
        .iter()
        .map(|(k, v)| format!("  {}: {v}\n", serde_json::to_string(k).unwrap()))
        .collect();
    let block = if block.is_empty() {
        block
    } else {
        format!("mcp_servers:\n{block}")
    };
    let new = set_block(&text, "mcp_servers:", &block);
    if new != text {
        std::fs::write(&yaml, new)?;
    }
    Ok(())
}

/// Replace the top-level `key` block by `block` (appended when absent, removed when empty); other bytes kept.
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

/// Drop the direct child `agent.reasoning_effort` (fleet.toml owns effort); every other byte is kept.
fn drop_reasoning_effort(text: &str) -> String {
    let (mut out, mut in_agent, mut child) = (String::new(), false, None);
    for line in text.split_inclusive('\n') {
        let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
        let body = &line[indent..];
        if !line.trim().is_empty() && indent == 0 {
            in_agent = line.starts_with("agent:");
            child = None;
        } else if in_agent && !body.trim().is_empty() && !body.starts_with('#') {
            let c = *child.get_or_insert(indent);
            if indent == c && body.starts_with("reasoning_effort:") {
                continue;
            }
        }
        out.push_str(line);
    }
    out
}
