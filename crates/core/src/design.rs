use crate::config::Config;
use crate::events::{self, emit, last_spawn, working, Event, IssueState};
use crate::runner::{ok, Runner};
use crate::Ctx;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;

/// The design ask's options; free text is a revise note.
pub const OPTIONS: [&str; 3] = ["approve", "revise", "reject"];

/// #868: a design lane's hook scope: its worktree top (from the hook input's `cwd`) and its issue's design paths.
pub fn scope(ctx: &Ctx, r: &dyn Runner, input: &Value, fleet: &str, n: Option<u64>) -> crate::hook::Design {
    let here = std::env::current_dir()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let cwd = input["cwd"].as_str().unwrap_or(&here);
    let top = ok(r, "git", &["-C", cwd, "rev-parse", "--show-toplevel"]);
    let spec = n
        .and_then(|n| crate::gh::cached_spec(ctx, fleet, n))
        .unwrap_or_default();
    let root = top.map_or_else(|_| cwd.to_string(), |t| t.trim().to_string());
    let text = std::fs::read_to_string(crate::repo_path(&Path::new(&root).join(RECIPE))).unwrap_or_default();
    let allow = Recipe::of(&text, spec["body"].as_str().unwrap_or_default()).folders;
    crate::hook::Design { root, allow }
}

/// #873: a repo's design recipe.
pub const RECIPE: &str = ".drover/design.md";

/// #873: what a design lane draws, where, how it renders and the host tags it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    pub medium: String,
    pub folders: Vec<String>,
    pub render: Option<String>,
    pub sizes: String,
    pub clip: bool,
    pub tools: Vec<String>,
}

impl Recipe {
    /// The issue `body`'s own design path (#868), else the recipe `text`'s `key: value` lines, else HTML at 1440/390.
    pub fn of(text: &str, body: &str) -> Recipe {
        let mut r = Recipe {
            medium: medium(body).into(),
            folders: paths(body),
            render: None,
            sizes: "1440 and 390 wide".into(),
            clip: false,
            tools: vec![],
        };
        if r.folders.len() > 1 {
            return r;
        }
        for l in text.lines() {
            let Some((k, v)) = l.trim().trim_start_matches(['-', '*', ' ']).split_once(':') else {
                continue;
            };
            let v = v.trim().trim_matches('`').trim();
            let key = k.trim().trim_matches('*').to_ascii_lowercase();
            match key.as_str() {
                _ if v.is_empty() => {}
                "medium" => r.medium = v.into(),
                "folder" if !v.starts_with(['/', '~', '$']) && !v.split('/').any(|c| c == "..") => {
                    r.folders = vec![v.trim_end_matches('/').into()];
                }
                "render" => r.render = Some(v.into()),
                "sizes" => r.sizes = v.into(),
                "clip" => r.clip = !matches!(v.to_ascii_lowercase().as_str(), "no" | "none" | "false"),
                "tools" => {
                    r.tools = v
                        .split([',', ' '])
                        .filter(|t| !t.is_empty())
                        .map(String::from)
                        .collect();
                }
                _ => {}
            }
        }
        r
    }

    /// The recipe at `rev` of the checkout `dir`, for an issue `body`.
    pub fn at(r: &dyn Runner, dir: &str, rev: &str, body: &str) -> Recipe {
        let show = |p: &str| ok(r, "git", &["-C", dir, "show", &format!("{rev}:{p}")]).ok();
        Recipe::of(&crate::repo_read("design.md", show).unwrap_or_default(), body)
    }
}

fn paths(body: &str) -> Vec<String> {
    let mut out = vec!["docs/design".to_string()];
    for span in body.split('`').skip(1).step_by(2) {
        let keep = |c: &&str| !c.contains(['*', '<', '{', '…']) && !c.ends_with("...");
        let parts: Vec<&str> = span
            .trim()
            .split('/')
            .take_while(keep)
            .filter(|c| !c.is_empty())
            .collect();
        let named = parts.contains(&"design") && !span.starts_with(['/', '~', '$']);
        let p = parts.join("/");
        if named
            && !parts.iter().any(|c| *c == ".." || c.contains(char::is_whitespace))
            && !out.iter().any(|o| Path::new(&p).starts_with(o))
        {
            out.push(p);
        }
    }
    out
}

fn medium(body: &str) -> &'static str {
    let native = ["SwiftUI", "UIKit", "AppKit", "Jetpack Compose"];
    let own = paths(body).len() > 1;
    native.into_iter().find(|m| own && body.contains(m)).unwrap_or("HTML")
}

/// A design's ask (`needs-decision` with `design: true`).
pub fn is_ask(e: &Event) -> bool {
    e.kind == "needs-decision" && e.data.get("design") == Some(&Value::Bool(true))
}

fn pre<'a>(ev: &'a [Event], f: &str, n: u64, role: &str) -> Option<&'a str> {
    let from = ev
        .iter()
        .rposition(|e| e.is("design-rejected", f, n))
        .map_or(0, |i| i + 1);
    let design = |e: &&Event| e.is("spawn", f, n) && e.str("role") == Some(role);
    ev[from..]
        .iter()
        .filter(design)
        .find_map(|e| e.str("sha").filter(|s| !s.is_empty()))
}

/// A design lane that ended its turn (not working) on a head that carries its stage's files and was never handed off: `design-review`, then one ask that the board card and the conductor thread which queued it (if any) both answer.
pub fn handoff(
    ctx: &Ctx,
    cfg: &Config,
    r: &dyn Runner,
    out: &mut dyn Write,
    agents: &[Value],
    s: &IssueState,
) -> Result<()> {
    let ev = working(&ctx.state)?;
    let Some(sp) = s
        .lane
        .as_deref()
        .filter(|_| s.state == "design")
        .and_then(|l| last_spawn(&ev, l))
    else {
        return Ok(());
    };
    let busy = agents
        .iter()
        .any(|a| sp.str("agent") == a["name"].as_str() && a["agent_status"] == "working");
    let Some(wt) = sp.str("worktree").filter(|_| !busy) else {
        return Ok(());
    };
    let log = ok(r, "git", &["-C", wt, "log", "-1", "--format=%H"]).unwrap_or_default();
    let head = log.trim();
    let (f, n) = (s.fleet.as_str(), s.issue);
    let handed = ev
        .iter()
        .any(|e| e.is("design-review", f, n) && e.str("sha") == Some(head));
    if head.is_empty() || crate::spawn::dirty(r, Path::new(wt)).is_some() {
        return Ok(());
    }
    let role = sp.str("role").unwrap_or("design");
    if handed {
        return failed(ctx, cfg, out, &ev, sp, (f, n, role));
    }
    let pre = pre(&ev, f, n, role).unwrap_or(head);
    let dirs = if role == "architect" {
        vec!["docs/architecture".to_string()]
    } else {
        let spec = crate::gh::cached_spec(ctx, f, n).unwrap_or_default();
        Recipe::at(r, wt, head, spec["body"].as_str().unwrap_or_default()).folders
    };
    let (range, int) = (
        format!("{pre}..{head}"),
        format!("^origin/{}", cfg.fleet(f)?.integration),
    );
    let mut args = vec!["-C", wt, "log", "--format=", "--name-only", &range, &int, "--"];
    args.extend(dirs.iter().map(String::as_str));
    let log = ok(r, "git", &args).unwrap_or_default();
    let mut names: Vec<&str> = vec![];
    for l in log.lines().filter(|l| !l.is_empty()) {
        if !names.contains(&l) {
            names.push(l);
        }
    }
    if names.is_empty() {
        return failed(ctx, cfg, out, &ev, sp, (f, n, role));
    }
    let dir = folder(names[0], &dirs);
    let adr = (role == "architect").then(|| dir.clone());
    let png = |l: &&str| {
        [".png", ".gif", ".mp4"]
            .iter()
            .any(|x| l.to_ascii_lowercase().ends_with(x))
    };
    let files: Vec<Value> = names
        .iter()
        .copied()
        .filter(png)
        .map(|l| json!({"name": l, "path": Path::new(wt).join(l)}))
        .collect();
    let d = json!({"sha": head, "pre": pre, "agent": sp.str("agent"), "files": files, "role": role, "adr": adr});
    let review = Event::new(ctx.now, f, Some(n), "design-review", d);
    let mark = |e: &&Event| {
        e.kind == "queue"
            && e.fleet == f
            && (e.data.get("design").is_some() || e.data.get("architect").is_some())
            && e.val("issues").as_array().is_some_and(|a| a.contains(&json!(n)))
    };
    let thread = ev
        .iter()
        .rfind(mark)
        .and_then(|e| e.str("design_thread"))
        .filter(|t| !t.is_empty());
    let what = if role == "architect" {
        "Architecture (ADR)"
    } else {
        "Design"
    };
    let q = format!("{what} for #{n} is ready ({head:.7}): approve, revise (type a note) or reject?");
    let link = format!("https://github.com/{}/tree/{head}/{dir}/", cfg.fleet(f)?.repo);
    let summary = summary(r, wt, head, &dir);
    let q = summary.as_ref().map_or(q.clone(), |s| format!("{s}\n\n{link}\n\n{q}"));
    let d = json!({"question": q, "options": OPTIONS, "recommend": null, "kind": "owner", "design": true, "files": files,
        "thread": thread, "role": role, "summary": summary, "link": link});
    let ask = Event::new(ctx.now, f, Some(n), "needs-decision", d);
    let once = crate::action::id(f, n, &format!("{role}-ask"), head);
    if !events::ask(&ctx.state, cfg, vec![review, ask], Some(&once))?.is_empty() {
        writeln!(out, "{f} #{n} design-review: {head:.7}")?;
    }
    Ok(())
}

fn failed(
    ctx: &Ctx,
    cfg: &Config,
    out: &mut dyn Write,
    ev: &[Event],
    sp: &Event,
    (f, n, role): (&str, u64, &str),
) -> Result<()> {
    let agent = sp.str("agent").unwrap_or_default();
    let mine = |e: &&Event| e.kind == "lane-state" && e.str("agent") == Some(agent) && e.ts >= sp.ts;
    let Some(end) = ev
        .iter()
        .rfind(mine)
        .filter(|e| matches!(e.str("state"), Some("error" | "gone")))
    else {
        return Ok(());
    };
    let note = ["note", "cause", "why"]
        .iter()
        .find_map(|k| end.str(k))
        .unwrap_or("no reason given");
    let why = format!(
        "{role} lane {agent} ended ({}: {note}) with no {role} commit of its own",
        end.str("state").unwrap_or_default()
    );
    writeln!(out, "{f} #{n} needs-human: {why}")?;
    let d = json!({"code": "LANE_FAILED", "reason": why, "agent": agent, "role": role});
    emit(&ctx.state, cfg, Event::new(ctx.now, f, Some(n), "needs-human", d))
}

fn folder(name: &str, dirs: &[String]) -> String {
    let under = dirs
        .iter()
        .find_map(|d| Some((d, name.strip_prefix(d.as_str())?.strip_prefix('/')?)));
    match under {
        Some((d, rest)) => match rest.split_once('/') {
            Some((slug, _)) => format!("{d}/{slug}"),
            None => d.clone(),
        },
        None => name.rsplit_once('/').map_or("", |p| p.0).to_string(),
    }
}

fn section(md: &str, name: &str) -> Option<String> {
    let level = |l: &str| l.len() - l.trim_start_matches('#').len();
    let lines: Vec<&str> = md.lines().collect();
    let at = lines
        .iter()
        .position(|l| level(l) > 0 && l.trim_start_matches('#').trim().eq_ignore_ascii_case(name))?;
    let body: Vec<&str> = lines[at + 1..]
        .iter()
        .take_while(|l| !(1..=level(lines[at])).contains(&level(l)))
        .copied()
        .collect();
    Some(body.join("\n").trim().to_string()).filter(|b| !b.is_empty())
}

fn summary(r: &dyn Runner, wt: &str, head: &str, dir: &str) -> Option<String> {
    let show = |f: &str| ok(r, "git", &["-C", wt, "show", &format!("{head}:{dir}/{f}")]).unwrap_or_default();
    let adr = show("adr.md");
    let own = section(&show("README.md"), "Summary").or_else(|| section(&adr, "Summary"));
    let text = own.or_else(|| {
        let open = section(&adr, "Open questions").unwrap_or_else(|| "none".into());
        Some(format!(
            "Decision: {}\nOpen questions: {open}",
            section(&adr, "Decision")?
        ))
    })?;
    let mut out = String::new();
    for l in text.lines() {
        if out.chars().count() + l.chars().count() > 600 {
            let cut: String = l.chars().take(600_usize.saturating_sub(out.chars().count())).collect();
            return Some(format!("{out}{}…", if out.is_empty() { cut } else { String::new() }));
        }
        out = format!("{out}{l}\n");
    }
    Some(out.trim_end().to_string())
}

/// The owner's answer `text` to design ask `ask`: approve, reject, or (anything else) revise with it as the note. Approve and reject end the design lane (its pane closes); reject pushes one commit back to the pre-design tree.
pub fn decide(ctx: &Ctx, cfg: &Config, r: &dyn Runner, ask: &Event, text: &str, by: &str) -> Result<()> {
    let (f, n) = (ask.fleet.as_str(), ask.issue.context("a design ask names its issue")?);
    let ev = working(&ctx.state)?;
    let review = ev.iter().rfind(|e| e.is("design-review", f, n));
    let sp = review.and_then(|e| last_spawn(&ev, e.str("agent")?));
    let (kind, mut d) = match text {
        "approve" => ("design-approved", json!({})),
        "reject" => ("design-rejected", json!({})),
        t => (
            "design-revise",
            json!({"note": t.trim_start_matches("revise").trim_start_matches(':').trim()}),
        ),
    };
    if let Some((agent, pane)) = sp
        .and_then(|e| Some((e.str("agent")?, e.str("pane")?)))
        .filter(|_| kind != "design-revise")
    {
        let _ = crate::term::close(r, pane);
        let c = json!({"agent": agent, "pane": pane, "reason": kind});
        emit(&ctx.state, cfg, Event::new(ctx.now, f, Some(n), "lane-closed", c))?;
    }
    if kind == "design-rejected" {
        let at = |k| sp.and_then(|e| e.str(k)).context("no design lane to revert");
        let pre = review.and_then(|e| e.str("pre")).context("no pre-design head")?;
        let (wt, branch) = (at("worktree")?, at("branch")?);
        let git = |a: &[&str]| ok(r, "git", &[&["-C", wt], a].concat()).map(|o| o.trim().to_string());
        let tree = git(&["rev-parse", &format!("{pre}^{{tree}}")])?;
        let msg = format!("design: revert the rejected design of #{n} (back to {pre:.7})");
        let c = git(&["commit-tree", &tree, "-p", "HEAD", "-m", &msg])?;
        git(&["reset", "--hard", &c])?;
        git(&["push", "origin", &format!("{c}:refs/heads/{branch}")])?;
        (d["pre"], d["sha"]) = (json!(pre), json!(c));
    }
    (d["id"], d["by"]) = (ask.val("id").clone(), json!(by));
    (d["role"], d["adr"]) = (
        ask.val("role").clone(),
        review.map_or(Value::Null, |e| e.val("adr").clone()),
    );
    emit(&ctx.state, cfg, Event::new(ctx.now, f, Some(n), kind, d))?;
    let (action, role) = match kind {
        "design-approved" => ("approve", cfg.fleet(f)?.flow().start()),
        "design-revise" => ("revise", ask.str("role").unwrap_or("design")),
        _ => return Ok(()),
    };
    crate::pending::record(ctx, (f, n), action, role, None)
}

/// #873: a recipe tool tag the main host lacks ends the design lane's turn `blocked`, naming it, before anything is drawn.
pub fn tools(ctx: &Ctx, cfg: &Config, r: &dyn Runner, (f, n, agent): (&str, u64, &str), rc: &Recipe) -> Result<()> {
    if rc.tools.is_empty() {
        return Ok(());
    }
    let mut have = crate::hosts::probe(r).map(|p| p.0).unwrap_or_default();
    have.extend(
        cfg.hosts
            .values()
            .filter(|h| h.machine.is_none())
            .flat_map(|h| h.tags.clone()),
    );
    let lacks: Vec<&str> = rc
        .tools
        .iter()
        .filter(|t| !have.contains(t))
        .map(String::as_str)
        .collect();
    if lacks.is_empty() {
        return Ok(());
    }
    let why = format!(
        "missing tool {}: the design recipe ({RECIPE}) needs it on the main host; install it, then spawn the design lane again",
        lacks.join(", ")
    );
    let d = json!({"state": "blocked", "why": why, "role": "design", "agent": agent});
    emit(&ctx.state, cfg, Event::new(ctx.now, f, Some(n), "turn-end", d))?;
    let d = json!({"code": "LANE_BLOCKED", "reason": format!("lane blocked: {why}"), "agent": agent});
    emit(&ctx.state, cfg, Event::new(ctx.now, f, Some(n), "needs-human", d))?;
    crate::gate::refused("MISSING_TOOL", why)
}
