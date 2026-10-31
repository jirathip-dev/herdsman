//! Claude PreToolUse / Hermes pre_tool_call hook: the tool guard of `herdsman help --agent`.
use serde_json::{json, Value};

pub struct Policy {
    pub branches: Vec<String>,
    /// Absolute path prefixes that agents may not modify.
    pub paths: Vec<String>,
    /// Reviewer lane: read-only (no edits but `verdict.md`, no commits, no pushes).
    pub reviewer: bool,
}

/// Claude and Hermes file-writing tools.
const EDIT_TOOLS: [&str; 6] = ["Edit", "Write", "MultiEdit", "NotebookEdit", "patch", "write_file"];
/// git subcommands that make commits or move refs; denied to reviewers.
const GIT_WRITES: [&str; 8] = ["commit", "push", "merge", "rebase", "cherry-pick", "revert", "am", "reset"];
const WRITERS: [&str; 9] = ["rm", "mv", "cp", "tee", "truncate", "chmod", "ln", "install", "dd"];

/// Some(reason) denies Claude's (`Bash`) or Hermes' (`terminal`, `args`) call; `cur_branch` resolves HEAD.
pub fn decide(input: &Value, p: &Policy, cur_branch: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let tool = input["tool_name"].as_str().unwrap_or_default();
    let ti = if input["tool_input"].is_object() { &input["tool_input"] } else { &input["args"] };
    if tool == "Bash" || tool == "terminal" {
        let cmd = ti["command"].as_str().unwrap_or_default();
        let cwd = ti["workdir"].as_str().or(input["cwd"].as_str()).unwrap_or(".");
        return bash(cmd, cwd, p, cur_branch);
    }
    if !EDIT_TOOLS.contains(&tool) {
        return None;
    }
    // A V4A `patch` names its files inside the patch text.
    let file = ["file_path", "notebook_path", "path", "patch"].iter().find_map(|k| ti[k].as_str()).unwrap_or_default();
    if let Some(pp) = protected_path(file, p) {
        return Some(format!("{file} is protected ({pp}); change it only through `herdsman` (e.g. `herdsman set`)"));
    }
    (p.reviewer && !file.ends_with("/verdict.md") && file != "verdict.md")
        .then(|| format!("reviewers are read-only: only verdict.md may be written, not {file}"))
}

fn protected_path<'a>(s: &str, p: &'a Policy) -> Option<&'a str> {
    let home = crate::config::home().to_string_lossy().into_owned();
    let s = s.replace("$HOME", &home).replace('~', &home);
    p.paths.iter().map(String::as_str).find(|pp| s.contains(pp))
}

fn bash(cmd: &str, cwd: &str, p: &Policy, cur_branch: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let seg_breaks: &[char] = &[';', '|', '&', '\n', '(', ')', '`'];
    for seg in cmd.split(seg_breaks) {
        let toks: Vec<String> = seg.split_whitespace().map(|t| t.trim_matches(['"', '\'']).to_string()).filter(|t| !t.is_empty()).collect();
        let t: Vec<&str> = toks.iter().map(String::as_str).collect();
        let found = gh(&t).or_else(|| git_push(&t, cwd, p, cur_branch)).or_else(|| reviewer_git(&t, p));
        if let Some(r) = found.or_else(|| design_spawn(&t)) {
            return Some(r);
        }
        if let Some(pp) = protected_path(seg, p) {
            let writes = seg.contains('>')
                || t.iter().any(|w| WRITERS.contains(w))
                || (t.iter().any(|w| *w == "sed" || *w == "perl") && t.iter().any(|w| w.starts_with("-i")));
            if writes {
                return Some(format!("writing under {pp} is blocked; use `herdsman set` / `herdsman profiles sync`"));
            }
        }
    }
    None
}

fn gh(t: &[&str]) -> Option<String> {
    let i = t.iter().position(|w| *w == "gh")?;
    let rest = &t[i + 1..];
    let merge = rest.windows(2).any(|w| w == ["pr", "merge"])
        || (rest.first() == Some(&"api") && rest.iter().any(|w| w.contains("/merge")));
    if merge {
        return Some("merging is gated: run `herdsman merge <fleet> <issue>` (needs exact-head PASS + green CI)".into());
    }
    let api = rest.first() == Some(&"api");
    let comment = rest.windows(2).any(|w| w == ["issue", "comment"] || w == ["pr", "comment"] || w == ["pr", "review"])
        || (api && rest.iter().any(|w| w.contains("/comments") || w.contains("addComment")) && api_writes(rest));
    comment.then(|| "lanes never comment on GitHub: report in your pane; fleet logs routing, verdicts and the merge".into())
}

/// `herdsman spawn … design` (or pre-rename `fleet`): design lanes are conductor-only.
fn design_spawn(t: &[&str]) -> Option<String> {
    let i = t.iter().position(|w| matches!(w.rsplit('/').next(), Some("herdsman" | "fleet")))?;
    (t.get(i + 1) == Some(&"spawn") && t[i + 2..].contains(&"design"))
        .then(|| "design lanes are conductor-only: only the owner runs `herdsman spawn … design`, never a lane".into())
}

/// `gh api` sends a write: a non-GET method, or fields/input (which default to POST).
fn api_writes(rest: &[&str]) -> bool {
    rest.iter().enumerate().any(|(i, w)| {
        let method = match *w {
            "-X" | "--method" => rest.get(i + 1).copied(),
            _ => w.strip_prefix("--method=").or(w.strip_prefix("-X").filter(|m| !m.is_empty())),
        };
        method.is_some_and(|m| !m.eq_ignore_ascii_case("GET"))
            || w.starts_with("-f") || w.starts_with("-F")
            || ["--field", "--raw-field", "--input"].iter().any(|f| w.split('=').next() == Some(f))
    })
}

/// Index of the git subcommand and the repo dir, past global options (`-C <dir>`, `-c <k=v>`, `--git-dir=…`).
fn git_sub(t: &[&str], cwd: &str) -> Option<(usize, String)> {
    let g = t.iter().position(|w| *w == "git")?;
    let mut dir = cwd.to_string();
    let mut i = g + 1;
    while i < t.len() && t[i].starts_with('-') {
        if t[i] == "-C" && i + 1 < t.len() {
            dir = t[i + 1].to_string();
        }
        i += if (t[i] == "-C" || t[i] == "-c") && i + 1 < t.len() { 2 } else { 1 };
    }
    (i < t.len()).then_some((i, dir))
}

fn reviewer_git(t: &[&str], p: &Policy) -> Option<String> {
    let (i, _) = git_sub(t, ".").filter(|_| p.reviewer)?;
    GIT_WRITES.contains(&t[i]).then(|| format!("reviewers are read-only: no `git {}`; write verdict.md and report in your pane", t[i]))
}

fn git_push(t: &[&str], cwd: &str, p: &Policy, cur_branch: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let (i, dir) = git_sub(t, cwd)?;
    if t[i] != "push" {
        return None;
    }
    let deny = |b: &str| Some(format!("pushing to `{b}` is blocked; integration branches change only via `herdsman merge`"));
    let mut pos = vec![];
    let mut j = i + 1;
    while j < t.len() {
        let w = t[j];
        if w == "--all" || w == "--mirror" {
            return deny(w);
        }
        if ["-o", "--push-option", "--repo", "--receive-pack", "--exec"].contains(&w) {
            j += 2;
            continue;
        }
        if !w.starts_with('-') {
            pos.push(w);
        }
        j += 1;
    }
    let protected = |b: &str| {
        let b = b.trim_start_matches('+');
        let dst = b.rsplit(':').next().unwrap_or(b);
        let dst = dst.trim_start_matches("refs/heads/");
        p.branches.iter().any(|x| x == dst).then(|| dst.to_string())
    };
    if pos.len() <= 1 {
        let head = cur_branch(&dir)?;
        return protected(&head).and_then(|b| deny(&b));
    }
    for r in &pos[1..] {
        let name = if *r == "HEAD" { cur_branch(&dir).unwrap_or_default() } else { r.to_string() };
        if let Some(b) = protected(&name) {
            return deny(&b);
        }
    }
    None
}

/// The deny answer in the caller's dialect: Hermes `block`, Claude `deny`.
pub fn deny_json(input: &Value, reason: &str) -> Value {
    if input["hook_event_name"] == "pre_tool_call" || input.get("args").is_some() {
        return json!({"action": "block", "message": format!("herdsman: {reason}")});
    }
    let reason = format!("herdsman: {reason}");
    json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}})
}
