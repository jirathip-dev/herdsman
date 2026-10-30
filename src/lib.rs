//! fleet: agents act only through this CLI; it owns state and refuses rule breaks.
pub mod access;
pub mod check;
pub mod config;
pub mod events;
pub mod gate;
pub mod gh;
pub mod harness;
pub mod hook;
pub mod host;
pub mod protection;
pub mod queue;
pub mod runner;
pub mod serve;
pub mod spawn;

use anyhow::{bail, Result};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use config::{home, set_value, write_atomic, Config};
use events::{emit, fold, read, status_line, Event};
use runner::Runner;
use serde_json::json;
use std::io::{Read, Write};
use std::path::PathBuf;

pub struct Ctx {
    pub config: PathBuf,
    pub state: PathBuf,
    pub worktrees: PathBuf,
    /// Hermes profiles root (`<root>/<profile>/config.yaml`).
    pub hermes: PathBuf,
    pub exe: String,
    pub now: u64,
    /// Tree `herdsman migrate` works on: `HERDSMAN_HOME`, else `$HOME`.
    pub home: PathBuf,
    /// Pre-rename fallbacks in use this run; data commands print them on stderr.
    pub hints: Vec<String>,
}

impl Ctx {
    pub fn from_env() -> Ctx {
        let env = |k: &str, d: PathBuf| std::env::var_os(k).map(PathBuf::from).unwrap_or(d);
        let root = std::env::var_os("HERDSMAN_HOME").map_or_else(home, PathBuf::from);
        let mut hints = vec![];
        // Read the pre-rename dir, never move it, while the new one is missing.
        let mut path = |k: &str, what: &str, d: &str, file: &str| {
            if let Some(p) = std::env::var_os(k) {
                return PathBuf::from(p);
            }
            let (old, new) = (root.join(d).join("fleet"), root.join(d).join("herdsman"));
            if old.is_dir() && !new.exists() {
                hints.push(format!("herdsman: found pre-rename {what} at {}; run 'herdsman migrate' to move it", old.display()));
                return old.join(file);
            }
            new.join(file)
        };
        let config = path("FLEET_CONFIG", "config", ".config", "fleet.toml");
        let state = path("FLEET_STATE", "state", ".local/state", "events.jsonl");
        Ctx {
            config,
            state,
            worktrees: env("FLEET_WORKTREES", home().join(".herdr/worktrees")),
            hermes: env("FLEET_HERMES_PROFILES", home().join(".hermes/profiles")),
            exe: std::env::current_exe().map_or("herdsman".into(), |p| p.to_string_lossy().into()),
            now: now(),
            home: root,
            hints,
        }
    }
    pub fn claude_dir(&self) -> PathBuf {
        self.config.parent().unwrap_or(&self.config).join("claude")
    }
}

/// `herdsman migrate`: move `<home>/{.config,.local/state}/fleet` to `.../herdsman` where the
/// old dir exists and the new one doesn't. A failed second move puts the first one back.
pub fn migrate(home: &std::path::Path, out: &mut dyn Write) -> Result<()> {
    let mut moved: Vec<(PathBuf, PathBuf)> = vec![];
    for d in [".config", ".local/state"] {
        let (old, new) = (home.join(d).join("fleet"), home.join(d).join("herdsman"));
        if new.exists() {
            writeln!(out, "nothing to move: {} already exists", new.display())?;
            continue;
        }
        if !old.is_dir() {
            writeln!(out, "nothing to move: no {}", old.display())?;
            continue;
        }
        if let Err(e) = std::fs::rename(&old, &new) {
            let mut msg = format!("moving {} -> {} failed: {e}", old.display(), new.display());
            for (o, n) in moved.iter().rev() {
                msg += &match std::fs::rename(n, o) {
                    Ok(()) => format!("; rolled back {} -> {}", n.display(), o.display()),
                    Err(e) => format!("; ROLLBACK FAILED {} -> {}: {e}", n.display(), o.display()),
                };
            }
            bail!(msg);
        }
        writeln!(out, "moved {} -> {}", old.display(), new.display())?;
        moved.push((old, new));
    }
    Ok(())
}

/// Seconds since the epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[derive(Parser)]
#[command(name = "herdsman", version, about = "State, gates and harness toggle over herdr", disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum Harness {
    Claude,
    Hermes,
    Dsh,
}

#[derive(Clone, Copy, ValueEnum)]
enum SpawnRole {
    Orch,
    Impl,
    Rev,
}

#[derive(Clone, Copy, ValueEnum)]
enum Verdict {
    #[value(name = "PASS")]
    Pass,
    #[value(name = "FAIL")]
    Fail,
}

#[derive(Clone, Copy, ValueEnum)]
enum Until {
    CiDone,
    CiGreen,
    Merged,
}

#[derive(Subcommand)]
enum Cmd {
    /// Switch a role's harness/model/profile in fleet.toml; prints old -> new
    Set {
        fleet: String,
        /// orch | impl | rev
        role: String,
        /// claude | hermes | dsh
        #[arg(long, value_enum)]
        harness: Option<Harness>,
        /// Model name (claude harness; Hermes profiles own their model)
        #[arg(long)]
        model: Option<String>,
        /// Profile name (default fleet-<role>)
        #[arg(long)]
        profile: Option<String>,
    },
    /// Create worktree + herdr pane, write .brief.md, launch the role's harness, verify the prompt landed;
    /// ISSUE `-` with role orch starts the fleet's orchestrator
    Spawn {
        fleet: String,
        /// Issue number, or `-` for the orch lane
        issue: String,
        #[arg(value_enum)]
        role: SpawnRole,
        /// Print the plan; change nothing
        #[arg(long)]
        dry_run: bool,
        /// Machine-readable output
        #[arg(long)]
        json: bool,
        /// Spawn although the host is busy (logs an `override` event)
        #[arg(long)]
        force: bool,
        /// Kill the live agent / unfinished lane in the way, then spawn (logged)
        #[arg(long)]
        supersede: bool,
    },
    /// Record a lane already running in a herdr pane (repair a spawn that was never logged)
    Adopt {
        fleet: String,
        /// Issue number, or `-` for the orch lane
        issue: String,
        #[arg(value_enum)]
        role: SpawnRole,
        /// The herdr pane the harness runs in
        #[arg(long)]
        pane: String,
    },
    /// Record a review verdict pinned to the branch's exact head sha
    Verdict {
        fleet: String,
        issue: u64,
        #[arg(value_enum)]
        verdict: Verdict,
        /// Full 40-hex head sha that was reviewed
        #[arg(long)]
        sha: String,
        /// The reviewer's verdict.md
        #[arg(long)]
        file: String,
    },
    /// Merge the issue's PR into the integration branch if every gate passes; writes the one closing comment, closes the issue
    Merge { fleet: String, issue: u64 },
    /// Per-issue state, lane and last-event age
    Status {
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
        until: Until,
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
    /// Read-only HTML page: boards, queues, last 50 events with GitHub links (GET only)
    Serve {
        /// Listen address
        #[arg(long, default_value = "127.0.0.1:7777")]
        addr: String,
    },
    /// Alert once per state on stall, lane-dead and ready-unmerged issues; idempotent, safe from cron
    Check,
    /// Each `[accounts]` account: ok / capped until hh:mm, and the lanes on it (names only, never tokens)
    Accounts {
        #[command(subcommand)]
        cmd: Option<AccountsCmd>,
    },
    /// Read-only: is GitHub branch protection on for main, staging and the integration branch of each fleet repo? Exit 1 unless all are
    Protection,
    /// Remove worktree + pane of merged lanes (never live, dirty or unpushed ones); dry run unless --yes
    Clean {
        fleet: Option<String>,
        /// Actually remove
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

pub const STATES: &str = "queued → impl → review(n) → fix(n) → ready → merged; plus blocked, needs-human";

/// The one usage doc both harnesses get. Generated from the CLI definition.
pub fn usage_doc() -> String {
    let mut cmd = Cli::command();
    cmd.build();
    let mut s = String::from(
        "# herdsman — usage for agents\n\nAct only through `herdsman`. It owns state (event log) and refuses anything that\n\
         breaks a rule. A refusal is final: report it, never work around it.\n\n## Commands\n",
    );
    let subs = cmd.get_subcommands().filter(|c| !c.is_hide_set());
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
    s += &format!("\n## Issue states\n\n{STATES}\n\n## Refusal codes (exit 3, `REFUSED <CODE>: why`)\n\n");
    for (c, why) in gate::CODES {
        s += &format!("- {c}: {why}\n");
    }
    s += "\n## Exit codes\n\n0 ok · 1 error (incl. merged but evidence comment missing) · 2 usage · 3 refused · 4 spawned but prompt not confirmed\n\
          `herdsman next`: 0 found · 1 empty · 2 top item blocked. `herdsman wait`: 0 reached · 1 failed/closed · 124 timeout.\n\n\
          ## GitHub\n\nThe issue is the spec: read it with `herdsman brief`; PR/CI with `herdsman pr` / `herdsman wait` (cached), never\n\
          `gh issue view` / `gh pr checks` loops. Never comment on issues or PRs; `herdsman merge` writes the one closing comment.\n\n\
          ## Tool guard, accounts, MCP (lanes skip permission prompts; the Claude/Hermes hook is the guard)\n\n\
          Hard blocks: `gh pr merge`, `gh issue|pr comment`, `gh pr review`, `gh api` comment writes, `git push` to\n\
          main/master/staging/integration, edits to fleet, fleet-operations and Hermes profile config. Reviewers are\n\
          read-only: no edits but `verdict.md`, no `git commit|push|merge|rebase|reset`.\n\
          Accounts: a Claude role's `account`/`accounts` picks the first uncapped `[accounts]` name as `CLAUDE_CODE_OAUTH_TOKEN`;\n\
          `herdsman check` emits `account-capped <name>` on the limit banner; spawns skip it for `cap_reset_minutes` or until\n\
          `herdsman accounts reset <name>`; `herdsman accounts` lists them. MCP: a lane gets only its role's `mcp` (default none).\n\n\
          ## Events\n\n`herdsman check` emits `stall`, `lane-dead`, `ready-unmerged` once per state; `herdsman merge` emits\n\
          `evidence-missing` (issue left open) if the evidence comment fails twice. Notifiers get one plain\n\
          line, or the raw event JSON with `format = \"json\"`.\n";
    s
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
        Cmd::Migrate => migrate(&ctx.home, out)?,
        Cmd::Help { agent: true } => write!(out, "{}", usage_doc())?,
        Cmd::Help { agent: false } => write!(out, "{}", Cli::command().render_long_help())?,
        Cmd::Hook { role, git_pre_push: true } => return pre_push(ctx, out, stdin, role.as_deref() == Some("rev")),
        Cmd::Hook { role, git_pre_push: false } => run_hook(ctx, r, out, stdin, role.as_deref() == Some("rev"))?,
        Cmd::Status { fleet, json } => {
            let all = fold(&read(&ctx.state)?);
            let rows: Vec<_> = all.values().filter(|s| fleet.as_ref().is_none_or(|f| *f == s.fleet)).collect();
            if json {
                writeln!(out, "{}", serde_json::to_string_pretty(&rows)?)?;
            } else if rows.is_empty() {
                writeln!(out, "no tracked issues")?;
            } else {
                writeln!(out, "{:<12} {:<6} {:<12} {:<24} AGE", "FLEET", "ISSUE", "STATE", "LANE")?;
                for s in rows {
                    writeln!(out, "{}", status_line(s, ctx.now))?;
                }
            }
        }
        Cmd::Set { fleet, role, harness, model, profile } => set(ctx, out, &fleet, &role, harness, model, profile)?,
        Cmd::Next { fleet } => {
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
        Cmd::Serve { addr } => serve::serve(ctx, &addr)?,
        Cmd::Queue { fleet, cmd: QueueCmd::Ls { json } } => {
            let rows = queue::ls_json(&read(&ctx.state)?, &fleet);
            if json {
                writeln!(out, "{}", serde_json::to_string_pretty(&rows)?)?;
            }
            for r in rows.as_array().into_iter().flatten().filter(|_| !json) {
                let on = r["blocked_by"].as_u64().map_or(String::new(), |b| format!("  blocked by #{b}"));
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
                Cmd::Profiles { cmd: ProfilesCmd::Sync { check: false } } => {
                    spawn::profiles_sync(ctx, &cfg, &usage_doc(), out)?
                }
                Cmd::Spawn { fleet, issue, role, dry_run, json, force, supersede } => {
                    let (issue, role) = lane(&issue, role)?;
                    let o = spawn::Opts { dry_run, json, force, supersede };
                    if !spawn::spawn(ctx, &cfg, r, out, (&fleet, issue, role), &o)? {
                        return Ok(4);
                    }
                }
                Cmd::Adopt { fleet, issue, role, pane } => {
                    let (issue, role) = lane(&issue, role)?;
                    spawn::adopt(ctx, &cfg, r, out, (&fleet, issue, role), &pane)?
                }
                Cmd::Check => check::check(ctx, &cfg, r, out)?,
                Cmd::Accounts { cmd: None } => access::list(ctx, &cfg, r, out)?,
                Cmd::Accounts { cmd: Some(AccountsCmd::Reset { name }) } => access::reset(ctx, &cfg, out, &name)?,
                Cmd::Protection => return protection::protection(&cfg, r, out),
                Cmd::Clean { fleet, yes } => check::clean(ctx, &cfg, r, out, fleet.as_deref(), None, yes)?,
                Cmd::Verdict { fleet, issue, verdict, sha, file } => {
                    let pass = matches!(verdict, Verdict::Pass);
                    gate::verdict(ctx, &cfg, r, out, (&fleet, issue), pass, &sha, &file)?
                }
                Cmd::Merge { fleet, issue } => gate::merge(ctx, &cfg, r, out, &fleet, issue)?,
                Cmd::Brief { fleet, issue } => {
                    let v = gh::spec(ctx, r, cfg.fleet(&fleet)?, &fleet, issue, false)?;
                    let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
                    writeln!(out, "# Issue #{issue}: {}\n\n{}", s("title"), s("body"))?;
                }
                Cmd::Pr { fleet, issue, json } => {
                    let branch = gate::branch_of(&gate::issue_state(ctx, &fleet, issue)?, issue);
                    let pr = gh::pr(ctx, r, cfg.fleet(&fleet)?, (&fleet, issue), &branch, ctx.now)?;
                    if json {
                        writeln!(out, "{}", serde_json::to_string_pretty(&pr)?)?;
                    } else {
                        writeln!(out, "{}", gh::summary(&pr, &branch))?;
                    }
                    if pr.is_null() {
                        return Ok(1);
                    }
                }
                Cmd::Wait { fleet, issue, until, timeout } => {
                    let until = match until {
                        Until::CiDone => gh::Until::CiDone,
                        Until::CiGreen => gh::Until::CiGreen,
                        Until::Merged => gh::Until::Merged,
                    };
                    let branch = gate::branch_of(&gate::issue_state(ctx, &fleet, issue)?, issue);
                    return gh::wait(ctx, r, out, cfg.fleet(&fleet)?, (&fleet, issue), &branch, until, timeout);
                }
                Cmd::Queue { fleet, cmd } => {
                    cfg.fleet(&fleet)?;
                    let d = match cmd {
                        QueueCmd::Add { issues, top, after } => queue::op("add", &issues, top, None, after, None),
                        QueueCmd::Mv { issue, top, before, after } => queue::op("mv", &[issue], top, before, after, None),
                        QueueCmd::Rm { issue } => queue::op("rm", &[issue], false, None, None, None),
                        QueueCmd::Block { issue, on } => queue::op("block", &[issue], false, None, None, Some(on)),
                        QueueCmd::Ls { .. } => unreachable!(),
                    };
                    let mut q = queue::replay(&read(&ctx.state)?, &fleet);
                    queue::apply(&mut q, &d)?;
                    emit(&ctx.state, &cfg, Event::new(ctx.now, &fleet, None, "queue", d))?;
                    let order: Vec<String> = q.iter().map(|i| format!("#{}", i.issue)).collect();
                    writeln!(out, "{fleet} queue: {}", if order.is_empty() { "empty".into() } else { order.join(" ") })?;
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(0)
}

/// Parse a lane's `<ISSUE> <ROLE>` arguments (`-` is the orch lane's issue).
fn lane(issue: &str, role: SpawnRole) -> Result<(Option<u64>, &'static str)> {
    let role = match role {
        SpawnRole::Orch => "orch",
        SpawnRole::Impl => "impl",
        SpawnRole::Rev => "rev",
    };
    let issue = match issue {
        "-" => None,
        n => Some(n.parse().map_err(|_| anyhow::anyhow!("issue must be a number or `-`, not `{n}`"))?),
    };
    Ok((issue, role))
}

fn set(
    ctx: &Ctx,
    out: &mut dyn Write,
    fleet: &str,
    role: &str,
    harness: Option<Harness>,
    model: Option<String>,
    profile: Option<String>,
) -> Result<()> {
    let text = std::fs::read_to_string(&ctx.config)?;
    let cfg = Config::parse(&text)?;
    let cur = cfg.fleet(fleet)?.role(role)?.clone();
    let harness = harness.map(|h| match h {
        Harness::Claude => "claude".to_string(),
        Harness::Hermes => "hermes".into(),
        Harness::Dsh => "dsh".into(),
    });
    let wanted = [("harness", harness, Some(cur.harness)), ("model", model, cur.model), ("profile", profile, cur.profile)];
    if wanted.iter().all(|w| w.1.is_none()) {
        bail!("nothing to set: pass --harness, --model or --profile");
    }
    let mut new = text.clone();
    let mut changes = vec![];
    for (key, val, old) in wanted {
        let Some(val) = val else { continue };
        let old = old.unwrap_or_else(|| "-".into());
        writeln!(out, "{fleet}.{role} {key}: {old} -> {val}")?;
        if old != val {
            new = set_value(&new, fleet, role, key, &val)?;
            changes.push(json!({"role": role, "key": key, "old": old, "new": val}));
        }
    }
    let new_cfg = Config::parse(&new)?;
    if new != text {
        write_atomic(&ctx.config, &new)?;
    }
    for c in changes {
        emit(&ctx.state, &new_cfg, Event::new(ctx.now, fleet, None, "set", c))?;
    }
    Ok(())
}

fn run_hook(ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write, stdin: &mut dyn Read, reviewer: bool) -> Result<()> {
    let mut buf = String::new();
    stdin.read_to_string(&mut buf)?;
    // Inert outside fleet lanes: non-fleet lanes share the Hermes profiles.
    // `FLEET_LANE` is the pre-rename name, honoured for one release; when both are
    // set, `HERDSMAN_LANE` wins, even empty.
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

/// git `pre-push` hook for lanes without a pre-tool hook (dsh): exit 1 refuses a push
/// to a protected branch, or any push from a reviewer lane.
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
