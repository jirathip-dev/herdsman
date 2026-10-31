//! The clap CLI, `herdsman help --agent` and dispatch to the modules.
use crate::config::{self, home, Config};
use crate::events::{fold, read, status_line};
use crate::runner::{self, Runner};
use crate::{access, check, devices, gate, gh, harness, hook, migrate, pause, protection, queue, queue_change, serve, set, spawn, Ctx};
use anyhow::{bail, Result};
use clap::{CommandFactory, Parser, Subcommand};
use serde_json::json;
use std::io::{Read, Write};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "herdsman", version, about = "State, gates and harness toggle over herdr", disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Switch a role's harness/model/profile/effort in fleet.toml; prints old -> new
    Set {
        fleet: String,
        /// orch | impl | rev
        role: String,
        /// claude | hermes | dsh
        #[arg(long, value_parser = harness::NAMES)]
        harness: Option<String>,
        /// Model name (claude harness; Hermes profiles own their model)
        #[arg(long)]
        model: Option<String>,
        /// Profile name (default fleet-<role>)
        #[arg(long)]
        profile: Option<String>,
        /// none | low | medium | high | xhigh
        #[arg(long)]
        effort: Option<String>,
    },
    /// Pause a fleet: `next` offers nothing and impl/rev spawns refuse PAUSED; live lanes are untouched
    Pause { fleet: String },
    /// Resume a paused fleet
    Resume { fleet: String },
    /// Create worktree + herdr pane, write .brief.md, launch the role's harness, verify the prompt landed;
    /// ISSUE `-` with role orch starts the fleet's orchestrator
    Spawn {
        fleet: String,
        /// Issue number, or `-` for the orch lane
        issue: String,
        #[arg(value_parser = config::ROLES)]
        role: String,
        #[command(flatten)]
        o: spawn::Opts,
    },
    /// Record a lane already running in a herdr pane (repair a spawn that was never logged)
    Adopt {
        fleet: String,
        /// Issue number, or `-` for the orch lane
        issue: String,
        #[arg(value_parser = config::ROLES)]
        role: String,
        /// The herdr pane the harness runs in
        #[arg(long)]
        pane: String,
    },
    /// Record a review verdict pinned to the branch's exact head sha
    Verdict {
        fleet: String,
        issue: u64,
        #[arg(value_parser = ["PASS", "FAIL"])]
        verdict: String,
        /// Full 40-hex head sha that was reviewed
        #[arg(long)]
        sha: String,
        /// The reviewer's verdict.md
        #[arg(long)]
        file: String,
    },
    /// Merge the issue's PR into the integration branch if every gate passes; writes the one closing comment, closes the issue
    Merge { fleet: String, issue: u64 },
    /// Hold for a decision: log it, notify the fleet with numbered options and the answer command, print its id
    Ask {
        fleet: String,
        issue: String,
        question: String,
        /// One choice (at most 4)
        #[arg(long = "option", value_name = "TEXT")]
        options: Vec<String>,
        /// The recommended option's number
        #[arg(long, value_name = "N")]
        recommend: Option<usize>,
    },
    /// Answer an open ask (option number or free text); prompts the asking agent `DECISION <id>: <text>`
    Answer {
        fleet: String,
        id: String,
        choice: String,
        /// owner | conductor
        #[arg(long, default_value = "owner", value_parser = ["owner", "conductor"])]
        by: String,
    },
    /// The open asks, oldest first, with age
    Asks {
        fleet: Option<String>,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Per-issue state, lane and last-event age
    Status {
        fleet: Option<String>,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Every lane waiting on the orch or the owner, with its next command (the conductor's watchdog)
    Waiting {
        fleet: Option<String>,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Print the issue spec cached at spawn; read this instead of `gh issue view`
    Brief { fleet: String, issue: u64 },
    /// The issue PR's head sha, base, mergeable and check rollup: one `gh pr view` per 60 s
    Pr {
        fleet: String,
        issue: u64,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Poll `herdsman pr` with backoff (30 s → 5 min); exit 0 reached, 1 failed check/closed PR, 124 timeout
    Wait {
        fleet: String,
        issue: u64,
        /// ci-done | ci-green | merged
        #[arg(long, value_enum)]
        until: gh::Until,
        /// Give up after this many seconds (exit 124)
        #[arg(long, default_value_t = 3600)]
        timeout: u64,
    },
    /// The fleet's ordered work queue (kept as queue events in the log)
    Queue {
        fleet: String,
        #[command(subcommand)]
        cmd: QueueCmd,
    },
    /// Print the first queued issue that has no lane and is not blocked; exit 0 found, 1 empty, 2 top item blocked
    Next { fleet: String },
    /// Compact board (≤ 60 cols): queue order, state, lane, age, last event
    Board { fleet: Option<String> },
    /// Phone page (PWA): queue, lanes, host, events; buttons for queue order, pause/resume, role model/effort
    Serve {
        #[command(subcommand)]
        cmd: Option<ServeCmd>,
        /// Listen address
        #[arg(long, default_value = "127.0.0.1:7777")]
        addr: String,
        /// GET only: no buttons, every POST is 405
        #[arg(long)]
        read_only: bool,
    },
    /// Web push to the page's subscribed devices
    Push {
        #[command(subcommand)]
        cmd: PushCmd,
    },
    /// Alert once per state on stall, lane-dead, ready-unmerged, orch-holding, ask-waiting; nudge an idle orch; idempotent, safe from cron
    Check,
    /// Each `[accounts]` account: ok / capped until hh:mm, and the lanes on it (names only, never tokens)
    Accounts {
        #[command(subcommand)]
        cmd: Option<AccountsCmd>,
    },
    /// Read-only: is GitHub branch protection on for main, staging and the integration branch of each fleet repo? Exit 1 unless all are
    Protection,
    /// Remove worktree + pane of merged or closed-issue lanes (never live, dirty or unpushed ones); dry run unless --yes
    Clean {
        fleet: Option<String>,
        /// Only this agent's worktree; refuses UNSAFE_CLEAN with the reason
        #[arg(long, value_name = "AGENT")]
        lane: Option<String>,
        /// Actually remove
        #[arg(long)]
        yes: bool,
    },
    /// Send Esc to an agent's pane (it pauses); logs `stop`
    Stop { agent: String },
    /// Close an agent's pane, pushing unpushed or dirty work to `wip/<agent>-<stamp>` first; the worktree stays. Dry run unless --yes
    Kill {
        agent: String,
        /// Actually kill
        #[arg(long)]
        yes: bool,
    },
    /// Manage per-role Claude Code config folders
    Profiles {
        #[command(subcommand)]
        cmd: ProfilesCmd,
    },
    /// Move pre-rename ~/.config/fleet and ~/.local/state/fleet to their herdsman paths (HERDSMAN_HOME selects the tree); all or nothing
    Migrate,
    /// Help; --agent prints the usage doc agents get
    Help {
        /// Print the agent usage doc
        #[arg(long)]
        agent: bool,
    },
    /// Claude Code PreToolUse / Hermes pre_tool_call hook (reads hook JSON on stdin)
    #[command(hide = true)]
    Hook {
        /// The lane's role; `rev` is read-only
        #[arg(long)]
        role: Option<String>,
        /// Act as git's pre-push hook (reads `<local ref> <sha> <remote ref> <sha>` lines on stdin)
        #[arg(long)]
        git_pre_push: bool,
    },
}

#[derive(Subcommand)]
enum QueueCmd {
    /// Append issues to the queue (or put them on top / after N)
    Add {
        #[arg(required = true)]
        issues: Vec<u64>,
        /// Put them at the top
        #[arg(long, conflicts_with = "after")]
        top: bool,
        /// Put them right after issue N
        #[arg(long, value_name = "N")]
        after: Option<u64>,
        /// They touch the fleet's serial_paths (DB migrations): one serial impl at a time
        #[arg(long)]
        serial: bool,
    },
    /// Move a queued issue
    #[command(group(clap::ArgGroup::new("to").required(true).args(["top", "before", "after"])))]
    Mv {
        issue: u64,
        /// Move it to the top
        #[arg(long)]
        top: bool,
        /// Move it right before issue N
        #[arg(long, value_name = "N")]
        before: Option<u64>,
        /// Move it right after issue N
        #[arg(long, value_name = "N")]
        after: Option<u64>,
    },
    /// Drop an issue from the queue
    Rm { issue: u64 },
    /// Hold an issue until another one is merged
    Block {
        issue: u64,
        /// The blocking issue
        #[arg(long, value_name = "ISSUE")]
        on: u64,
    },
    /// List the queue in order
    Ls {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ServeCmd {
    /// Print a one-time 6-digit code (5 min) that pairs a phone's reply box
    Pair,
}

#[derive(Subcommand)]
enum PushCmd {
    /// Send a test alert to every subscribed device (needs a `type = "webpush"` notifier for its `sub`)
    Test,
}

#[derive(Subcommand)]
enum AccountsCmd {
    /// Let spawns use a capped account again
    Reset { name: String },
}

#[derive(Subcommand)]
enum ProfilesCmd {
    /// Write ~/.config/herdsman/claude/<profile>/ (settings.json + hooks, role.md, fleet.md) and Hermes profiles' `hooks:` + hook approval
    Sync {
        /// Write nothing; exit 1 unless every fleet Hermes profile lists its hook as `✓ allowed`
        #[arg(long)]
        check: bool,
    },
}


/// The one usage doc both harnesses get: the commands generated from the CLI definition, the rest from `roles/usage-*.md`.
pub fn usage_doc() -> String {
    let mut cmd = Cli::command();
    cmd.build();
    let mut s = String::from(include_str!("../roles/usage-head.md"));
    // The owner's commands; the sections below name them.
    let owner = ["accounts", "pause", "resume", "serve"];
    let subs = cmd.get_subcommands().filter(|c| !c.is_hide_set() && !owner.contains(&c.get_name()));
    let subs: Vec<_> = subs.flat_map(|c| if c.has_subcommands() { c.get_subcommands().collect() } else { vec![c] }).collect();
    for sub in subs {
        let mut sub = sub.clone();
        let about = sub.get_about().map(|a| a.to_string()).unwrap_or_default();
        let usage = sub.render_usage().to_string().replace("Usage: ", "");
        s += &format!("\n`{}`\n  {about}\n", usage.trim());
        for a in sub.get_arguments().filter(|a| a.get_long().is_some_and(|l| l != "help")) {
            let h = a.get_help().map(|h| h.to_string()).unwrap_or_default();
            s += &format!("  --{}  {h}\n", a.get_long().unwrap());
        }
    }
    s + include_str!("../roles/usage-tail.md")
}

/// Entry point used by main and tests. Returns the process exit code.
pub fn main_with(args: Vec<String>, ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write, stdin: &mut dyn Read) -> i32 {
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { 2 } else { 0 };
        }
    };
    if !matches!(cli.cmd, Cmd::Hook { .. } | Cmd::Migrate | Cmd::Help { .. }) {
        for h in &ctx.hints {
            eprintln!("{h}");
        }
    }
    match dispatch(cli.cmd, ctx, r, out, stdin) {
        Ok(code) => code,
        Err(e) => match e.downcast_ref::<gate::Refusal>() {
            Some(rf) => {
                let _ = writeln!(out, "{rf}");
                3
            }
            None => {
                eprintln!("herdsman: {e:#}");
                1
            }
        },
    }
}

fn dispatch(cmd: Cmd, ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write, stdin: &mut dyn Read) -> Result<i32> {
    match cmd {
        Cmd::Migrate if std::env::var_os("HERDSMAN_HOME").is_some_and(|v| v.is_empty()) => {
            bail!("HERDSMAN_HOME is set but empty; unset it or point it at a home tree")
        }
        Cmd::Migrate => migrate(&ctx.home, out)?,
        Cmd::Help { agent: true } => write!(out, "{}", usage_doc())?,
        Cmd::Help { agent: false } => write!(out, "{}", Cli::command().render_long_help())?,
        Cmd::Hook { role, git_pre_push: true } => return pre_push(ctx, out, stdin, role.as_deref() == Some("rev")),
        Cmd::Hook { role, git_pre_push: false } => run_hook(ctx, r, out, stdin, role.as_deref() == Some("rev"))?,
        Cmd::Status { fleet, json } => {
            let events = read(&ctx.state)?;
            let all = fold(&events);
            let rows: Vec<_> = all.values().filter(|s| fleet.as_ref().is_none_or(|f| *f == s.fleet)).collect();
            if json {
                writeln!(out, "{}", serde_json::to_string_pretty(&rows)?)?;
            } else if rows.is_empty() {
                writeln!(out, "no tracked issues")?;
            } else {
                let cfg = Config::load(&ctx.config).ok();
                let waits = cfg.map(|c| crate::waiting::scan(ctx, &c, r, &events, &crate::host::agents(r).unwrap_or_default(), false));
                writeln!(out, "{:<12} {:<6} {:<12} {:<24} {:<5} WAITING", "FLEET", "ISSUE", "STATE", "LANE", "AGE")?;
                for s in rows {
                    let w = waits.iter().flatten().find(|(x, _)| x.fleet == s.fleet && x.issue == s.issue).map_or("-", |w| w.1.what.as_str());
                    writeln!(out, "{:<5} {w}", status_line(s, ctx.now))?;
                }
            }
        }
        Cmd::Set { fleet, role, harness, model, profile, effort } => {
            let wanted = [("harness", harness), ("model", model), ("profile", profile), ("effort", effort)];
            set(ctx, out, &fleet, &role, wanted.into_iter().filter_map(|(k, v)| Some((k, v?))).collect(), None)?
        }
        Cmd::Pause { fleet } => pause(ctx, out, &fleet, true, None)?,
        Cmd::Resume { fleet } => pause(ctx, out, &fleet, false, None)?,
        Cmd::Next { fleet } => {
            if Config::load(&ctx.config).is_ok_and(|c| c.fleet.get(&fleet).is_some_and(|f| f.paused)) {
                writeln!(out, "{fleet}: paused")?;
                return Ok(1);
            }
            let (code, line) = queue::next(&read(&ctx.state)?, &fleet);
            writeln!(out, "{line}")?;
            return Ok(code);
        }
        Cmd::Board { fleet } => {
            let events = read(&ctx.state)?;
            let all = fleet.map_or_else(|| queue::fleets(&events), |f| vec![f]);
            let boards: Vec<_> = all.iter().map(|f| queue::board(&events, f, ctx.now)).collect();
            write!(out, "{}", boards.join("\n"))?;
        }
        Cmd::Asks { fleet, json } => crate::asks(ctx, out, fleet.as_deref(), json)?,
        Cmd::Serve { cmd: Some(ServeCmd::Pair), .. } => writeln!(out, "pairing code {} (5 min, one use)", devices::new_code(ctx)?)?,
        Cmd::Serve { addr, read_only, .. } => serve::serve(ctx, r, &addr, read_only)?,
        Cmd::Push { cmd: PushCmd::Test } => {
            let cfg = Config::load(&ctx.config)?;
            let n = cfg.notify.iter().chain(cfg.fleet.values().flat_map(|f| &f.notify)).find(|n| n.webpush());
            let Some(sub) = n.and_then(|n| n.sub.as_deref()) else { bail!("no `type = \"webpush\"` notifier in fleet.toml (docs/serve.md)") };
            let ev = crate::events::Event::new(ctx.now, "herdsman", None, "push-test", json!({"reason": "test alert"}));
            writeln!(out, "{}", crate::webpush::send(&ctx.state, sub, &ev)?)?;
        }
        Cmd::Queue { fleet, cmd: QueueCmd::Ls { json } } => {
            let rows = queue::ls_json(&read(&ctx.state)?, &fleet);
            if json {
                writeln!(out, "{}", serde_json::to_string_pretty(&rows)?)?;
            }
            for r in rows.as_array().into_iter().flatten().filter(|_| !json) {
                let mut on = r["blocked_by"].as_u64().map_or(String::new(), |b| format!("  blocked by #{b}"));
                if r["serial"] == true {
                    on += "  serial";
                }
                let n = |k: &str| r[k].as_u64().unwrap_or_default();
                writeln!(out, "{:>2} #{:<5} {}{on}", n("pos"), n("issue"), r["state"].as_str().unwrap_or_default())?;
            }
        }
        cmd => {
            let cfg = Config::load(&ctx.config)?;
            match cmd {
                Cmd::Profiles { cmd: ProfilesCmd::Sync { check: true } } => {
                    if !spawn::profiles_check(ctx, &cfg, r, out)? {
                        return Ok(1);
                    }
                }
                Cmd::Profiles { cmd: ProfilesCmd::Sync { check: false } } => spawn::profiles_sync(ctx, &cfg, &usage_doc(), out)?,
                Cmd::Spawn { fleet, issue, role, o } => {
                    let issue = lane(&issue)?;
                    if !spawn::spawn(ctx, &cfg, r, out, (&fleet, issue, &role), &o)? {
                        return Ok(4);
                    }
                }
                Cmd::Adopt { fleet, issue, role, pane } => {
                    let issue = lane(&issue)?;
                    spawn::adopt(ctx, &cfg, r, out, (&fleet, issue, &role), &pane)?
                }
                Cmd::Ask { fleet, issue, question, options, recommend } => crate::ask(ctx, &cfg, out, (&fleet, lane(&issue)?), &question, &options, recommend)?,
                Cmd::Answer { fleet, id, choice, by } => crate::answer(ctx, &cfg, r, out, (&fleet, &id), &choice, (&by, None))?,
                Cmd::Check => check::check(ctx, &cfg, r, out)?,
                Cmd::Waiting { fleet, json } => {
                    let all = crate::waiting::scan(ctx, &cfg, r, &read(&ctx.state)?, &crate::host::agents(r).unwrap_or_default(), true);
                    let rows: Vec<_> = all.into_iter().filter(|(s, w)| s.lane.as_ref() == Some(&w.lane)).map(|x| x.1).filter(|w| matches!(w.who, "orch" | "owner") && fleet.as_ref().is_none_or(|f| *f == w.fleet)).collect();
                    if json {
                        writeln!(out, "{}", serde_json::to_string_pretty(&rows)?)?;
                    }
                    for w in rows.iter().filter(|_| !json) {
                        writeln!(out, "{} #{} {}: {} ({}){}", w.fleet, w.issue, w.who, w.what, crate::events::age(ctx.now.saturating_sub(w.since)), w.next_cmd.as_deref().or((w.who == "orch").then_some("?")).map_or(String::new(), |c| format!(" → {c}")))?;
                    }
                }
                Cmd::Accounts { cmd: None } => access::list(ctx, &cfg, r, out)?,
                Cmd::Accounts { cmd: Some(AccountsCmd::Reset { name }) } => access::reset(ctx, &cfg, out, &name)?,
                Cmd::Protection => return protection::protection(&cfg, r, out),
                Cmd::Clean { fleet, lane, yes } => check::clean(ctx, &cfg, r, out, fleet.as_deref(), (None, lane.as_deref()), yes)?,
                Cmd::Stop { agent } => check::stop(ctx, &cfg, r, out, &agent)?,
                Cmd::Kill { agent, yes } => check::kill(ctx, &cfg, r, out, &agent, yes)?,
                Cmd::Verdict { fleet, issue, verdict, sha, file } => {
                    let pass = verdict == "PASS";
                    gate::verdict(ctx, &cfg, r, out, (&fleet, issue), pass, &sha, &file)?
                }
                Cmd::Merge { fleet, issue } => gate::merge(ctx, &cfg, r, out, &fleet, issue)?,
                Cmd::Brief { fleet, issue } => {
                    let v = gh::spec(ctx, r, cfg.fleet(&fleet)?, &fleet, issue, false)?;
                    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
                    writeln!(out, "# Issue #{issue}: {}\n\n{}", s("title"), s("body"))?;
                }
                Cmd::Pr { fleet, issue, json } => {
                    let branch = gate::branch_of(gate::issue_state(ctx, &fleet, issue)?.as_ref(), issue);
                    let pr = gh::pr(ctx, r, cfg.fleet(&fleet)?, (&fleet, issue), &branch, ctx.now)?;
                    if json {
                        writeln!(out, "{}", serde_json::to_string_pretty(&pr)?)?;
                    } else {
                        writeln!(out, "{}", gh::summary(&pr, &branch, &cfg.fleet(&fleet)?.required_checks))?;
                    }
                    if pr.is_null() {
                        return Ok(1);
                    }
                }
                Cmd::Wait { fleet, issue, until, timeout } => {
                    let branch = gate::branch_of(gate::issue_state(ctx, &fleet, issue)?.as_ref(), issue);
                    return gh::wait(ctx, r, out, cfg.fleet(&fleet)?, (&fleet, issue), &branch, until, timeout);
                }
                Cmd::Queue { fleet, cmd } => {
                    cfg.fleet(&fleet)?;
                    let d = match cmd {
                        QueueCmd::Add { issues, top, after, serial } => {
                            let mut d = queue::op("add", &issues, top, None, after, None);
                            if serial {
                                d["serial"] = json!(true);
                            }
                            d
                        }
                        QueueCmd::Mv { issue, top, before, after } => queue::op("mv", &[issue], top, before, after, None),
                        QueueCmd::Rm { issue } => queue::op("rm", &[issue], false, None, None, None),
                        QueueCmd::Block { issue, on } => queue::op("block", &[issue], false, None, None, Some(on)),
                        QueueCmd::Ls { .. } => unreachable!(),
                    };
                    writeln!(out, "{}", queue_change(ctx, &cfg, &fleet, d, None)?)?;
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(0)
}

/// Parse a lane's `<ISSUE>` argument (`-` is the orch lane's issue).
fn lane(issue: &str) -> Result<Option<u64>> {
    match issue {
        "-" => Ok(None),
        n => Ok(Some(n.parse().map_err(|_| anyhow::anyhow!("issue must be a number or `-`, not `{n}`"))?)),
    }
}

fn run_hook(ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write, stdin: &mut dyn Read, reviewer: bool) -> Result<()> {
    let mut buf = String::new();
    stdin.read_to_string(&mut buf)?;
    // Inert outside fleet lanes (they share the Hermes profiles); `HERDSMAN_LANE` beats `FLEET_LANE`, even empty.
    let lane = std::env::var_os("HERDSMAN_LANE").or_else(|| std::env::var_os("FLEET_LANE"));
    if lane.is_none_or(|v| v.is_empty()) {
        return Ok(());
    }
    let Ok(input) = serde_json::from_str::<serde_json::Value>(&buf) else {
        eprintln!("herdsman hook: input is not JSON; allowing");
        return Ok(());
    };
    let branches = protected_branches(ctx);
    let dir = |p: &PathBuf| p.parent().unwrap_or(p).to_string_lossy().into_owned();
    let h = home();
    let paths = vec![
        dir(&ctx.config),
        dir(&ctx.state),
        h.join(".config/fleet-operations").to_string_lossy().into(),
        h.join(".hermes/profiles").to_string_lossy().into(),
    ];
    let policy = hook::Policy { branches, paths, reviewer };
    let cur = |d: &str| {
        runner::ok(r, "git", &["-C", d, "rev-parse", "--abbrev-ref", "HEAD"]).ok().map(|s| s.trim().to_string())
    };
    if let Some(reason) = hook::decide(&input, &policy, &cur) {
        writeln!(out, "{}", hook::deny_json(&input, &reason))?;
    }
    Ok(())
}

/// main, master, staging and every fleet's integration branch.
fn protected_branches(ctx: &Ctx) -> Vec<String> {
    let mut branches: Vec<String> = ["main", "master", "staging"].map(String::from).to_vec();
    if let Ok(cfg) = Config::load(&ctx.config) {
        branches.extend(cfg.fleet.values().map(|f| f.integration.clone()));
    }
    branches
}

/// git `pre-push` hook for dsh lanes: exit 1 refuses a protected-branch push, or any reviewer push.
fn pre_push(ctx: &Ctx, out: &mut dyn Write, stdin: &mut dyn Read, reviewer: bool) -> Result<i32> {
    let mut buf = String::new();
    stdin.read_to_string(&mut buf)?;
    let branches = protected_branches(ctx);
    for dst in buf.lines().filter_map(|l| l.split_whitespace().nth(2)) {
        let b = dst.trim_start_matches("refs/heads/");
        if reviewer {
            writeln!(out, "herdsman: reviewers are read-only: no `git push` (to `{b}`); write verdict.md and report in your pane")?;
            return Ok(1);
        }
        if branches.iter().any(|x| x == b) {
            writeln!(out, "herdsman: pushing to `{b}` is blocked; integration branches change only via `herdsman merge`")?;
            return Ok(1);
        }
    }
    Ok(0)
}
