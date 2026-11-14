use crate::config::{home, Config};
use crate::events::{emit, status_line, working, Event};
use crate::hosts::all_agents;
use crate::runner::{self, Runner};
use crate::{
    access, check, devices, gate, gh, hook, migrate, pause, protection, providers, queue, queue_change, set, spawn, Ctx,
};
use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use serde_json::json;
use std::io::{Read, Write};
use std::path::PathBuf;

/// #264: (`serve`, `serve --export`), set by the binary: the server crate depends on this one, never the reverse.
pub static SERVE: std::sync::OnceLock<(Serve, Export)> = std::sync::OnceLock::new();
type Serve = fn(&Ctx, &dyn Runner, &str, bool) -> Result<()>;
type Export = fn(&Ctx, &dyn Runner, &std::path::Path, bool) -> Result<()>;
/// #345: `drover service` (api addr, board addr), set by the binary from the service crate.
pub static SERVICE: std::sync::OnceLock<Service> = std::sync::OnceLock::new();
type Service = fn(&Ctx, &str, Option<&str>) -> Result<()>;

#[derive(Parser)]
#[command(
    name = "drover",
    version = crate::VERSION,
    about = "State, gates and harness toggle over shed",
    disable_help_subcommand = true
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    #[command(about = "Switch a role's harness/model/profile/effort in fleet.toml; prints old -> new")]
    Set {
        fleet: String,
        #[arg(help = "impl | rev | design")]
        role: String,
        #[arg(long, help = providers::lane_names().join(" | "), value_parser = clap::builder::PossibleValuesParser::new(providers::lane_names()))]
        harness: Option<String>,
        #[arg(long, help = "Model name (ignored where the provider's profile owns the model)")]
        model: Option<String>,
        #[arg(help = "Profile name (default fleet-<role>)")]
        #[arg(long)]
        profile: Option<String>,
        #[arg(help = "none | low | medium | high | xhigh")]
        #[arg(long)]
        effort: Option<String>,
    },
    #[command(
        about = "Pause a fleet: `next` offers nothing and impl/rev spawns refuse PAUSED; live lanes are untouched"
    )]
    Pause { fleet: String },
    #[command(about = "Resume a paused fleet")]
    Resume { fleet: String },
    #[command(
        about = "Create worktree + shed terminal, write .brief.md, launch the role's harness, verify the prompt landed"
    )]
    Spawn {
        fleet: String,
        issue: String,
        role: String,
        #[command(flatten)]
        o: spawn::Opts,
    },
    #[command(about = "Record a lane already running in a shed terminal (repair a spawn that was never logged)")]
    Adopt {
        fleet: String,
        issue: u64,
        role: String,
        #[arg(help = "The shed terminal the harness runs in")]
        #[arg(long)]
        pane: String,
    },
    #[command(about = "Record a review verdict pinned to the branch's exact head sha")]
    Verdict {
        fleet: String,
        issue: u64,
        #[arg(value_parser = ["PASS", "FAIL"])]
        verdict: String,
        #[arg(help = "Full 40-hex head sha that was reviewed")]
        #[arg(long)]
        sha: String,
        #[arg(help = "The reviewer's verdict.md")]
        #[arg(long)]
        file: String,
    },
    #[command(
        about = "Merge the issue's PR into the integration branch if every gate passes; writes the one closing comment, closes the issue"
    )]
    Merge {
        fleet: String,
        issue: u64,
        #[arg(help = "Owner only: merge past ROUND_LIMIT (never past PASS, CI or serial); logged `by: owner`")]
        #[arg(long)]
        owner_override: bool,
    },
    #[command(
        about = "Owner only: one more review round for a needs-human issue (its card's [one more round]); logs `round-extended`"
    )]
    Extend {
        fleet: String,
        issue: u64,
        #[arg(help = "Who approved it, as the log shows it")]
        #[arg(long, default_value = "owner")]
        by: String,
        #[arg(help = "Record the extra round only; leave the live lane alone (no impl spawn)")]
        #[arg(long)]
        no_spawn: bool,
        #[arg(
            long,
            value_name = "LINE",
            conflicts_with = "basis",
            help = "An orchestrator's: the owner's exact line in its thread, after the card: logged by: owner (typed)"
        )]
        typed: Option<String>,
        #[arg(
            long,
            value_name = "QUOTE|ID",
            help = "An orchestrator's: a verbatim quote of the issue's spec, or an answered decision id; the owner may [change] it"
        )]
        basis: Option<String>,
    },
    #[command(
        about = "End a fix round with no new commit: drive reviews the same head again (a CI-only FAIL with green CI merges)"
    )]
    Done {
        fleet: String,
        issue: u64,
        #[arg(help = "Why nothing changed (the finding adjudicated away, CI-infra red)")]
        #[arg(long, value_name = "WHY")]
        no_change: String,
    },
    #[command(about = "An impl lane's end of turn, for its own issue; logs `turn-end` and drive acts on it at once")]
    Turn {
        #[arg(
            help = "done (pushed, ready for review) | blocked (--why) | need-input (`ask` first) | working (mid-task)"
        )]
        #[arg(value_parser = ["done", "blocked", "need-input", "working"])]
        state: String,
        #[arg(help = "Why: the blocker, the question, what is left")]
        #[arg(long, required_if_eq("state", "blocked"))]
        why: Option<String>,
    },
    #[command(about = "Owner only: park a needs-human issue (its card's [park]); the card clears")]
    Park {
        fleet: String,
        issue: u64,
        #[arg(help = "Why it is parked: `queue ls` and the board show it")]
        #[arg(long)]
        why: Option<String>,
        #[arg(help = "Who parked it, as the log shows it")]
        #[arg(long, default_value = "owner")]
        by: String,
    },
    #[command(
        about = "Owner only: a parked issue back to the state it had, in its queue place (else last); logs `unparked`"
    )]
    Unpark {
        fleet: String,
        issue: u64,
        #[arg(help = "Who unparked it, as the log shows it")]
        #[arg(long, default_value = "owner")]
        by: String,
    },
    #[command(
        about = "Hold for a decision: log it, notify the fleet with numbered options and the answer command, print its id"
    )]
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    Ask {
        #[command(subcommand)]
        cmd: Option<AskCmd>,
        #[arg(required = true)]
        fleet: Option<String>,
        #[arg(required = true)]
        issue: Option<String>,
        #[arg(required = true)]
        question: Option<String>,
        #[arg(help = "One choice (at most 4)")]
        #[arg(long = "option", value_name = "TEXT")]
        options: Vec<String>,
        #[arg(help = "The recommended option's number")]
        #[arg(long, value_name = "N")]
        recommend: Option<usize>,
        #[arg(help = "routine (defaults to --recommend after [limits].ask_default_minutes) | owner (always waits)")]
        #[arg(long, default_value = "owner", value_parser = ["routine", "owner"])]
        kind: String,
        #[arg(help = "Ask in this orchestrator thread: its options become choice rows, the answer goes back into it")]
        #[arg(long, value_name = "ID")]
        thread: Option<String>,
    },
    #[command(
        about = "Answer an open ask (option number or free text); prompts the asking agent `DECISION <id>: <text>`"
    )]
    Answer {
        fleet: String,
        id: String,
        #[arg(required_unless_present = "typed")]
        choice: Option<String>,
        #[arg(help = "Who answered, as the board shows it (e.g. owner, alice)")]
        #[arg(long, default_value = "owner")]
        by: String,
        #[arg(
            help = "An orchestrator's answer to a lane ask: a verbatim quote of the issue's spec, or an answered decision id"
        )]
        #[arg(long, value_name = "QUOTE|ID")]
        basis: Option<String>,
        #[arg(help = "The owner's exact line in the ask's thread, after the ask: logged by: owner (typed)")]
        #[arg(long, value_name = "LINE")]
        typed: Option<String>,
        #[arg(long, requires_all = ["typed", "choice"], help = "A typed choice's note; an approve's goes in Amendments")]
        note: Option<String>,
    },
    #[command(
        about = "Orchestrators and the owner: write an owner-decided amendment into the issue body's `## Amendments`; re-runs the PR's failed spec checks"
    )]
    Amend {
        fleet: String,
        issue: u64,
        text: String,
        #[arg(help = "The ask the owner answered (or a routine ask's default) on this issue that decided it")]
        #[arg(long, value_name = "ID")]
        basis: String,
    },
    #[command(about = "The board's bug reports (#363), newest first: id, note, bundle path; with an id, that bundle")]
    Report { id: Option<String> },
    #[command(
        about = "Orchestrator chat threads per repo (or `all`), each talked through by one orchestrator lane (docs/orchestrator.md)"
    )]
    Thread {
        #[command(subcommand)]
        cmd: ThreadCmd,
    },
    #[command(about = "#387: fleet.toml changes as thread cards only the owner applies (docs/orchestrator.md)")]
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    #[command(about = "#935: a fleet's name")]
    Fleet {
        #[command(subcommand)]
        cmd: FleetCmd,
    },
    #[command(about = "#646: the fleet repo's `wip/*` branches")]
    Wip {
        #[command(subcommand)]
        cmd: WipCmd,
    },
    #[command(
        about = "#387: secrets in the host's keychain as `drover/<fleet>/<NAME>`; fleet `env` reads them as `secret:<NAME>`"
    )]
    Secret {
        #[command(subcommand)]
        cmd: SecretCmd,
    },
    #[command(
        about = "#538: run cards: an owner-level command an orchestrator posts in its thread; drover runs it once on the owner's [approve]"
    )]
    Card {
        #[command(subcommand)]
        cmd: CardCmd,
    },
    #[command(about = "The open asks, oldest first, with age")]
    Asks {
        #[command(flatten)]
        scope: Scope,
        #[arg(help = "Closed asks too, with why: answered, closed: merged, closed: lane gone")]
        #[arg(long)]
        all: bool,
    },
    #[command(about = "Per-issue state, lane and last-event age")]
    Status(Scope),
    #[command(
        about = "Every open issue's latest lane: what it waits on, with its next command (the owner's watchdog)"
    )]
    Waiting(Scope),
    #[command(about = "Print the issue spec cached at spawn; read this instead of `gh issue view`")]
    Brief { fleet: String, issue: u64 },
    #[command(
        about = "Why a line is as it is: blame's commit, its issue, the why note on it, the issue's asks and answers"
    )]
    Why {
        #[arg(help = "The line, as `<file>:<line>`")]
        at: String,
        #[arg(help = "Machine-readable output")]
        #[arg(long)]
        json: bool,
    },
    #[command(about = "The issue PR's head sha, base, mergeable and check rollup: one `gh pr view` per 60 s")]
    Pr {
        fleet: String,
        issue: u64,
        #[arg(help = "Machine-readable output")]
        #[arg(long)]
        json: bool,
    },
    #[command(
        about = "#668: the queue-wide line-cap view: total, cap and headroom at integration, each open PR's `src/ net`, the queued issues with no PR, the projected total and the one raise to ask for"
    )]
    Cap { fleet: String },
    #[command(
        about = "#889: files 5+ merged PRs touched in 14 days; opens or updates the week's issue, queued `--architect`"
    )]
    Hotspots { fleet: Option<String> },
    #[command(about = "#845: the issue statechart and the timers' pass chart as Mermaid (docs/workflow.md)")]
    Diagram {
        #[arg(
            long,
            help = "Per-state issue counts (of FLEET, else all) and the config's `[limits]` values"
        )]
        live: bool,
        fleet: Option<String>,
    },
    #[command(
        about = "Poll `drover pr` with backoff (30 s → 5 min); exit 0 reached, 1 failed check/closed PR, 124 timeout"
    )]
    Wait {
        fleet: String,
        issue: u64,
        #[arg(help = "ci-done | ci-green | merged")]
        #[arg(long, value_enum)]
        until: gh::Until,
        #[arg(help = "Give up after this many seconds (exit 124)")]
        #[arg(long, default_value_t = 3600)]
        timeout: u64,
    },
    #[command(about = "The fleet's ordered work queue (kept as queue events in the log)")]
    Queue {
        fleet: String,
        #[command(subcommand)]
        cmd: QueueCmd,
    },
    #[command(
        about = "Print the first queued issue that has no lane and is not blocked; exit 0 found, 1 empty, 2 top item blocked"
    )]
    Next { fleet: String },
    #[command(about = "Compact board (≤ 60 cols): queue order, state, lane, age, last event")]
    Board { fleet: Option<String> },
    #[command(
        about = "File this week's lessons issue, queue it on top and spawn its `lesson` lane (orchestrators and the owner only)"
    )]
    #[command(args_conflicts_with_subcommands = true)]
    Lesson {
        #[command(subcommand)]
        cmd: Option<LessonCmd>,
        fleet: Option<String>,
    },
    #[command(about = "Each `[hosts]` host: on/off, reachable, tags, load, memory, disk and lanes (docs/hosts.md)")]
    #[command(alias = "host")]
    Hosts {
        #[command(subcommand)]
        cmd: Option<HostsCmd>,
    },
    #[command(
        about = "Owner only: the token proxy remote ACP lanes' model calls go through, on `[proxy] port` (#637, docs/hosts.md)"
    )]
    #[command(hide = true)]
    Proxy,
    #[command(
        about = "Phone page (PWA): queue, lanes, host, events; buttons for queue order, pause/resume, role model/effort"
    )]
    Serve {
        #[command(subcommand)]
        cmd: Option<ServeCmd>,
        #[arg(help = "Listen address")]
        #[arg(long, default_value = "127.0.0.1:7777")]
        addr: String,
        #[arg(help = "GET only: no buttons, every POST is 405")]
        #[arg(long)]
        read_only: bool,
        #[arg(help = "Write the page as static files into this dir instead of serving (docs/serve.md)")]
        #[arg(long)]
        export: Option<std::path::PathBuf>,
        #[arg(help = "With --export: the inert demo, its shed/gh/host reads canned in runner.json next to fleet.toml")]
        #[arg(long, requires = "export")]
        demo: bool,
    },
    #[command(about = "Web push to the page's subscribed devices")]
    Push {
        #[command(subcommand)]
        cmd: PushCmd,
    },
    #[command(
        about = "One pass of the loop for every unpaused fleet: do every step that's due, then exit; safe from cron"
    )]
    Drive {
        fleet: Option<String>,
        #[arg(help = "#212: only record verdicts and merge PASS + green (the every-minute job); no lane starts")]
        #[arg(long)]
        fast: bool,
    },
    #[command(
        about = "Drive one issue's due steps now, detached (lane hooks and board taps run it); the timers stay the safety net"
    )]
    Kick {
        fleet: String,
        issue: u64,
        #[arg(help = "What happened: push, verdict, ask, stop, board")]
        #[arg(long, default_value = "cli")]
        why: String,
        #[arg(long, hide = true)]
        now: bool,
        #[arg(help = "The triggering event's id: the same id is driven once")]
        #[arg(long)]
        id: Option<String>,
    },
    #[command(
        about = "CI: formatted lines of src/**/*.rs against `line_cap` in --config; exit 1 over it or a --rules file over 30 lines"
    )]
    #[command(hide = true)]
    CapCheck {
        #[arg(long)]
        config: std::path::PathBuf,
        #[arg(long, default_value = ".drover/rules.md")]
        rules: std::path::PathBuf,
    },
    #[command(about = "CI (#734): the layers, bans and size limits in --config; exit 1 on a violation")]
    #[command(hide = true)]
    ArchCheck {
        #[arg(long, default_value = ".drover/arch.toml")]
        config: PathBuf,
        #[arg(help = "Write today's size offenders to the baseline file instead")]
        #[arg(long)]
        write_baseline: bool,
        #[arg(long, help = "The git revision whose added lines the comment and doc checks judge")]
        base: Option<String>,
        #[arg(long, requires = "base", help = "The PR body (`no doc: <why>` excuses)")]
        pr_body: Option<PathBuf>,
    },
    #[command(
        about = "CI: run the lesson checks in --rules; a shadow one's failure is reported (job summary), an active one's exits 1"
    )]
    #[command(hide = true)]
    LessonCheck {
        #[arg(long, default_value = ".drover/rules.md")]
        rules: std::path::PathBuf,
    },
    #[command(
        about = "Write and load the tick and board jobs, tailscale serve, profiles sync, then doctor (its exit code); safe to rerun. `setup <host> <name>` (#303): run that configured login in a shed terminal on the host instead"
    )]
    Setup {
        #[arg(requires = "name")]
        host: Option<String>,
        name: Option<String>,
        #[arg(help = "Write the jobs without loading them")]
        #[arg(long)]
        no_load: bool,
    },
    #[command(about = "The 5-minute `drover tick` job: a launchd agent on macOS, a systemd user timer on Linux")]
    Timer {
        #[arg(value_parser = ["install", "remove", "status"])]
        action: String,
        #[arg(help = "The job's label")]
        #[arg(long, default_value = crate::timer::TICK)]
        label: String,
        #[arg(help = "install: write the job without loading it")]
        #[arg(long)]
        no_load: bool,
    },
    #[command(
        about = "One timer pass: check, then drive; refusals exit 0, a real error logs `tick-failed` (alerts at most hourly)"
    )]
    Tick,
    #[command(
        about = "One supervised process: the board, kicks driven at once, the GitHub poll, API v1 for remote lanes (docs/service.md)"
    )]
    Service {
        #[arg(help = "API v1 listen address (remote lanes reach it over the tailnet)")]
        #[arg(long, default_value = "127.0.0.1:7778")]
        api: String,
        #[arg(help = "Also serve the board here (the `serve` job's address)")]
        #[arg(long)]
        board: Option<String>,
        #[arg(help = "Write its launchd/systemd unit and print the commands that load it; loads nothing")]
        #[arg(long)]
        install: bool,
    },
    #[command(
        about = "Alert once per state on stall, lane-dead, ready-unmerged, ci-missing, ask-waiting, orphan-lane, outbox-stale; idempotent, safe from cron"
    )]
    Check,
    #[command(
        about = "Each `[accounts]` account: ok / capped until hh:mm, and the lanes on it (names only, never tokens)"
    )]
    Accounts {
        #[command(subcommand)]
        cmd: Option<AccountsCmd>,
    },
    #[command(
        about = "Read-only host check: shed, gh, config, accounts, harnesses, serve, phone, watchdog; ✓/✗/! with the fix per line, exit 1 on any ✗"
    )]
    Doctor {
        #[arg(long)]
        json: bool,
        #[arg(
            help = "Only load the config (the pre-install gate, #539): exit 1 if this binary can't, warnings are fine"
        )]
        #[arg(long)]
        config_only: bool,
    },
    #[command(
        about = "Read-only: is GitHub branch protection on for main, staging and the integration branch of each fleet repo? Exit 1 unless all are"
    )]
    Protection,
    #[command(
        about = "Remove worktree + pane of merged or closed-issue lanes (never live ones; unsaved work goes to `wip/` first, kept on a secret) and list orphan folders; dry run unless --yes"
    )]
    Clean {
        fleet: Option<String>,
        #[arg(help = "Only this agent's worktree; refuses UNSAFE_CLEAN with the reason")]
        #[arg(long, value_name = "AGENT")]
        lane: Option<String>,
        #[arg(help = "Actually remove")]
        #[arg(long)]
        yes: bool,
        #[arg(help = "With --yes, remove the orphans too: folders under the worktree root no fleet owns")]
        #[arg(long)]
        orphans: bool,
        #[arg(help = "The daily job (#557): merged/closed lanes idle 3+ days, at most 10")]
        #[arg(long, hide = true, conflicts_with_all = ["fleet", "lane", "yes", "orphans"])]
        scheduled: bool,
    },
    #[command(
        about = "Install the `[self_update] branch` head (default main) once its CI is green: build, sign, doctor-gate, swap, hand serve over (#557, #894)"
    )]
    SelfUpdate {
        #[arg(help = "Print what it would do; change nothing")]
        #[arg(long)]
        dry_run: bool,
        #[arg(help = "Stop installing until --resume (logged)")]
        #[arg(long, conflicts_with = "resume")]
        pause: bool,
        #[arg(help = "Install again after --pause (logged)")]
        #[arg(long)]
        resume: bool,
    },
    #[command(
        about = "Build folders (target, node_modules, .cache, `build_dirs`) of worktrees idle 7 days: preview, then apply on `y`"
    )]
    Reclaim {
        #[arg(long, help = "Remove without asking")]
        apply: bool,
    },
    #[command(
        about = "#537: the `[ci_runners]` VMs and their GitHub runners (an orchestrator's `runners` group, its brief names the verbs); each verb logged `runners-*`"
    )]
    #[command(hide = true)]
    Runners {
        #[command(subcommand)]
        cmd: RunnersCmd,
    },
    #[command(about = "Send Esc to an agent's pane (it pauses); logs `stop`")]
    Stop { agent: String },
    #[command(
        about = "Close the terminal of a shed agent that is not a drover lane (IS_LANE for lanes); files and worktrees stay"
    )]
    Close {
        agent: String,
        #[arg(help = "The agent on this `[hosts.<name>]` (when another host has one of the same name)")]
        #[arg(long)]
        host: Option<String>,
    },
    #[command(
        about = "Close an agent's pane, pushing unpushed or dirty work to `wip/<agent>-<stamp>` first; the worktree stays. Dry run unless --yes"
    )]
    Kill {
        agent: String,
        #[arg(help = "Actually kill")]
        #[arg(long)]
        yes: bool,
        #[arg(help = "Its lane on this `[hosts.<name>]` (a pane a supersede left there under the same name)")]
        #[arg(long)]
        host: Option<String>,
    },
    #[command(about = "Manage per-role provider profiles")]
    Profiles {
        #[command(subcommand)]
        cmd: ProfilesCmd,
    },
    #[command(
        about = "Move the pre-rename herdsman config, state and share trees to their drover paths (DROVER_HOME selects the tree); all or nothing"
    )]
    Migrate {
        #[arg(help = "Allow it on the live state dir (~/.local/state/drover); without it migrate refuses there")]
        #[arg(long)]
        live: bool,
    },
    #[command(about = "The event log in its JSONL format (one event a line, with its seq), for external readers")]
    #[command(hide = true)]
    Events {
        #[arg(help = "Print it as JSONL (the only format)")]
        #[arg(long, required = true)]
        jsonl: bool,
    },
    #[command(about = "Landing numbers: `stats land <fleet>`")]
    Stats {
        #[command(subcommand)]
        cmd: StatsCmd,
    },
    #[command(about = "Help; --agent prints the usage doc agents get")]
    Help {
        #[arg(help = "Print the agent usage doc")]
        #[arg(long)]
        agent: bool,
    },
    #[command(hide = true, about = "A provider's pre-tool-call hook (reads hook JSON on stdin)")]
    Hook {
        #[arg(help = "The lane's role; `rev` is read-only")]
        #[arg(long)]
        role: Option<String>,
        #[arg(help = "Act as git's pre-push hook (reads `<local ref> <sha> <remote ref> <sha>` lines on stdin)")]
        #[arg(long)]
        git_pre_push: bool,
    },
    #[command(
        hide = true,
        about = "A provider's status line: records the lane account's 5h/7d limits (reads status JSON on stdin)"
    )]
    Statusline,
    #[command(
        about = "#275: log a lane's own event (`hook-denied`, `coauthor-trailer`), forwarded from a lane on another host"
    )]
    #[command(hide = true)]
    LaneEvent { kind: String, data: String },
    #[command(
        hide = true,
        about = "Run a lane's heavy check in a host slot, waiting in line for one (#909)"
    )]
    Heavy {
        kind: String,
        #[arg(long)]
        max: usize,
        #[arg(long)]
        kind_max: Option<usize>,
        #[arg(long)]
        wait: u64,
        #[arg(last = true)]
        cmd: String,
    },
}

#[derive(clap::Args)]
struct Scope {
    fleet: Option<String>,
    #[arg(help = "Machine-readable output")]
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
enum AskCmd {
    #[command(about = "Close an ask its own thread's orchestrator replaced; logged `ask-withdrawn`")]
    Withdraw {
        fleet: String,
        id: String,
        #[arg(help = "The orchestrator withdrawing it: orchestrator-<thread>")]
        #[arg(long)]
        by: String,
        #[arg(help = "The ask that replaces it")]
        #[arg(long = "for", value_name = "ID")]
        new: Option<String>,
    },
}

#[derive(Subcommand)]
enum ThreadCmd {
    #[command(
        about = "Start a thread for a fleet's repo (or `all`); prints its id. An orchestrator uses `thread handoff`"
    )]
    New {
        repo: String,
        title: String,
        #[arg(help = "Its orchestrator's harness for good (default `[chat]`'s, else the orchestrator role's)")]
        #[arg(long)]
        harness: Option<String>,
        #[arg(help = "The `[orchestrator.<id>]` it talks to (default: the first whose fleets cover the repo)")]
        #[arg(long)]
        orchestrator: Option<String>,
    },
    #[command(
        about = "An orchestrator hands off to thread <to> at once, or to a repo (or `all`): the owner taps one of its threads or a new one; linked both ways, <text> wakes its orchestrator as from <from>"
    )]
    Handoff {
        from: String,
        to: String,
        title: String,
        text: String,
    },
    #[command(
        about = "The owner's message: logged, then typed into the thread's orchestrator (started or resumed first)"
    )]
    Say {
        id: String,
        text: String,
        #[arg(help = "Who wrote it, as the thread shows it")]
        #[arg(long, default_value = "owner")]
        by: String,
        #[arg(help = "A file or image to send with it (repeatable, at most 10)")]
        #[arg(long)]
        file: Vec<PathBuf>,
    },
    #[command(about = "Retitle a thread: the orchestrator's short summary, once after its first reply")]
    Title { id: String, title: String },
    #[command(about = "Fold a thread into its repo's archived list; its orchestrator stops, nothing is deleted")]
    Archive { id: String },
    #[command(about = "Bring an archived thread back; its next message resumes the same session")]
    Restore { id: String },
    #[command(about = "The orchestrator's reply to the owner")]
    Reply {
        id: String,
        text: String,
        #[arg(help = "A file or image to show with it (repeatable)")]
        #[arg(long)]
        file: Vec<PathBuf>,
    },
    #[command(about = "The conversation, asks with their options; marks it read")]
    Show {
        id: String,
        #[arg(help = "Machine-readable output")]
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Per repo, its threads: title, last message, unread dot, orchestrator awake/asleep")]
    Ls(Scope),
}

#[derive(Subcommand)]
enum ConfigCmd {
    #[command(about = "Every [limits] key with its value and what it times or caps")]
    Show,
    #[command(
        about = "Propose <file> as the whole new fleet.toml in a thread: validated, its diff a card with [apply] [discard]"
    )]
    Propose { thread: String, file: PathBuf },
    #[command(about = "Owner only: apply a proposal (backup kept, doctor run)")]
    Apply { id: String },
    #[command(about = "Owner only: discard a proposal")]
    Discard { id: String },
}

#[derive(Subcommand)]
enum FleetCmd {
    #[command(
        about = "Owner only (a run card): move a paused fleet's queue, history, asks, secrets and state files to a new name"
    )]
    Rename {
        old: String,
        new: String,
        #[arg(long, help = "Apply the changes (default: print them)")]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum WipCmd {
    #[command(
        about = "Owner only (a run card): list origin's `wip/*` branches (size, issue state, build output); delete those of a merged or closed issue that carry build output"
    )]
    Prune {
        fleet: String,
        #[arg(help = "List only; delete nothing")]
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum SecretCmd {
    #[command(about = "Ask the owner for a secret in a thread: a masked card on paired owner devices")]
    Request {
        thread: String,
        name: String,
        #[arg(help = "What it is for, as the card shows it")]
        #[arg(long)]
        why: String,
    },
    #[command(about = "Owner only: the secrets set, by fleet")]
    Ls,
    #[command(about = "Owner only: delete a secret")]
    Rm { fleet: String, name: String },
}

#[derive(Subcommand)]
enum CardCmd {
    #[command(
        about = "Post a run card in a thread: the exact command, why, what it touches and its folder; CARD_FORBIDDEN for a hard no"
    )]
    Post {
        thread: String,
        #[arg(help = "The exact command (an approval covers only this text)")]
        #[arg(long = "cmd")]
        command: String,
        #[arg(help = "Why it should run")]
        #[arg(long)]
        why: String,
        #[arg(help = "What it touches: repo/branch, cloud resources, cost")]
        #[arg(long)]
        blast: String,
        #[arg(help = "The folder it runs in on the main host")]
        #[arg(long)]
        dir: String,
        #[arg(help = "Seconds before the run is killed")]
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    #[command(
        about = "Owner only: run a card's command once (within 15 min of its post); its exit code and output tail go to the thread"
    )]
    Approve {
        id: String,
        #[arg(help = "The command's sha (a prefix) the owner saw; CARD_CHANGED unless it matches")]
        #[arg(long)]
        sha: Option<String>,
    },
    #[command(about = "Owner only: deny a card; nothing runs")]
    Deny { id: String },
    #[command(hide = true)]
    Run { id: String },
}

#[derive(Subcommand)]
enum QueueCmd {
    #[command(about = "Append issues to the queue (or put them on top / after N)")]
    Add {
        issues: Vec<u64>,
        #[arg(help = "Put them at the top")]
        #[arg(long, conflicts_with = "after")]
        top: bool,
        #[arg(help = "Put them right after issue N")]
        #[arg(long, value_name = "N")]
        after: Option<u64>,
        #[arg(
            help = "They touch serial group G's paths (plain: the first group): its `max` (default 1) in flight at a time"
        )]
        #[arg(long, value_name = "G", num_args = 0..=1, default_missing_value = "")]
        serial: Option<String>,
        #[arg(help = "Host tags their lanes need besides the fleet's (`docker,gpu`)")]
        #[arg(long, value_delimiter = ',')]
        needs: Vec<String>,
        #[arg(help = "A design lane first; impl once the owner approves the design")]
        #[arg(long)]
        design: bool,
        #[arg(help = "An architect lane first (before any design); impl once the owner approves its ADR")]
        #[arg(long)]
        architect: bool,
        #[arg(help = "Force the verify stage on (plain) or off, whatever the PR touches")]
        #[arg(long, num_args = 0..=1, default_missing_value = "on", value_parser = ["on", "off"])]
        verify: Option<String>,
        #[arg(
            help = "A bug fix: impl's first commit is a failing test, rev proves it fails on base and passes at head"
        )]
        #[arg(long)]
        repro: bool,
        #[arg(
            help = "A speed-up: impl's first commit records a baseline, rev's PASS gives before/after (`NO_BASELINE`)"
        )]
        #[arg(long, conflicts_with = "refactor")]
        perf: bool,
        #[arg(help = "Behaviour unchanged: tests only moved or renamed, checked on rev's PASS (`TESTS_CHANGED`)")]
        #[arg(long)]
        refactor: bool,
    },
    #[command(about = "Move a queued issue")]
    #[command(group(clap::ArgGroup::new("to").required(true).args(["top", "before", "after"])))]
    Mv {
        issue: u64,
        #[arg(help = "Move it to the top")]
        #[arg(long)]
        top: bool,
        #[arg(help = "Move it right before issue N")]
        #[arg(long, value_name = "N")]
        before: Option<u64>,
        #[arg(help = "Move it right after issue N")]
        #[arg(long, value_name = "N")]
        after: Option<u64>,
    },
    #[command(about = "Drop an issue from the queue")]
    Rm { issue: u64 },
    #[command(
        about = "Replace a queued issue's `needs` host tags, serial groups, design, architect, verify or repro mark (`queue set 12 needs docker,gpu`; `design on|off`)"
    )]
    Set {
        issue: u64,
        #[arg(value_parser = ["needs", "serial", "design", "architect", "verify", "repro"])]
        key: String,
        #[arg(value_delimiter = ',')]
        tags: Vec<String>,
    },
    #[command(about = "Hold an issue until others are merged; each `--on` adds a blocker")]
    Block {
        issue: u64,
        #[arg(
            help = "A blocking issue (repeatable): `N`, another fleet's `<fleet>#N` or another repo's `<owner/repo>#N`"
        )]
        #[arg(long, value_name = "ISSUE", required = true)]
        on: Vec<String>,
    },
    #[command(about = "Drop blockers of an issue: those `--on` names, else all")]
    Unblock {
        issue: u64,
        #[arg(help = "A blocker to drop (repeatable)")]
        #[arg(long, value_name = "ISSUE")]
        on: Vec<String>,
    },
    #[command(about = "List the queue in order")]
    Ls {
        #[arg(help = "Machine-readable output")]
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ServeCmd {
    #[command(about = "Now an alias: prints how to pair (tap Pair on the page, then `serve approve <code>`)")]
    Pair,
    #[command(
        about = "Pair the phone showing this 6-digit code (single use, 10 min; NO_SUCH_CODE / EXPIRED otherwise)"
    )]
    Approve { code: String },
    #[command(about = "The open pairing requests: code, device, minutes left")]
    Pending,
    #[command(about = "The paired devices and the days until each expires (a year after its last use)")]
    Devices,
}

#[derive(Subcommand)]
enum PushCmd {
    #[command(
        about = "Send a test alert to every subscribed device (needs a `type = \"webpush\"` notifier for its `sub`)"
    )]
    Test,
}

#[derive(Subcommand)]
enum StatsCmd {
    #[command(
        about = "#350: per landing mode, median PASS → landed, landings per day, reverts, check time; red minutes on integration"
    )]
    Land { fleet: String },
}

#[derive(Subcommand)]
enum AccountsCmd {
    #[command(about = "Let spawns use a capped account again")]
    Reset { name: String },
}

#[derive(Subcommand)]
enum HostsCmd {
    #[command(about = "Probe every host's tags again (else hourly, by `check`)")]
    Refresh,
    #[command(about = "Let new lanes go to this host again; logs `host-on`")]
    On { name: String },
    #[command(about = "No new lanes on this host (its lanes finish there); still monitored; logs `host-off`")]
    Off { name: String },
    #[command(
        about = "Empty a root (default the host's first): new lanes skip it, its lanes are saved and restarted on the next root"
    )]
    Eject { name: String, root: Option<String> },
    #[command(
        about = "Owner only: a cloud host's lanes' work to `wip/` first, then its recipe's `destroy`; logs `cloud-stop`"
    )]
    #[command(hide = true)]
    Destroy { name: String },
    #[command(
        hide = true,
        about = "Owner only: install shed, drover, git, gh, the lane providers there over SSH, open its logins, clone its fleets"
    )]
    Bootstrap { name: String },
    #[command(about = "Owner only: a healthy cloud host saved as its recipe's snapshot; logs `cloud-snapshot`")]
    #[command(hide = true)]
    Snapshot { name: String },
    #[command(
        about = "The token proxy's per-lane keys (#637): id, state, lane, host; `revoke <id>` (owner only) ends one at once"
    )]
    Keys {
        #[command(subcommand)]
        cmd: Option<KeysCmd>,
    },
}

#[derive(Subcommand)]
enum KeysCmd {
    #[command(about = "Owner only: this key stops signing at once; logs `proxy-key-revoked`")]
    Revoke { id: String },
}

#[derive(Subcommand)]
enum RunnersCmd {
    #[command(about = "The GitHub runners, each VM's state and a health line (`9/9 online`)")]
    Status,
    #[command(about = "A fresh registration token and a runner-service restart on that VM")]
    Revive { vm: String },
    #[command(
        about = "Create N VMs to the spec, within `max_vms` and the $/day cap (past it: RUNNERS_CAP, an owner ask)"
    )]
    Add {
        #[arg(default_value_t = 1)]
        n: u64,
        #[arg(help = "The owner ask that approved going past the cap")]
        #[arg(long, value_name = "ID")]
        approved: Option<String>,
    },
    #[command(about = "Drain (no job running), deregister, then delete a VM; or deregister one idle runner by name")]
    Remove {
        target: String,
        #[arg(help = "The owner ask that approved removing the last online VM (RUNNERS_LAST)")]
        #[arg(long, value_name = "ID")]
        approved: Option<String>,
    },
    #[command(
        about = "Push a fresh registration token to every VM now (`check` repeats it every 30 min) and report the refresher"
    )]
    Tokens,
    #[command(about = "Stop and deregister this host's self-hosted runners (`local_label`, `local_stop`)")]
    RetireLocal {
        #[arg(help = "The owner ask that approved it while VM runners are below expected (RUNNERS_LOW)")]
        #[arg(long, value_name = "ID")]
        approved: Option<String>,
    },
}

#[derive(Subcommand)]
enum LessonCmd {
    #[command(
        about = "The fleet's recent mistakes as plain text: rev FAIL findings, the owner's thread lines and `by: owner` events, reverts"
    )]
    Digest {
        fleet: String,
        #[arg(help = "The window in days")]
        #[arg(long, default_value_t = 7)]
        days: u64,
    },
    #[command(
        about = "Promote or retire the lesson checks in a rules file by their history (the lesson lane, in its worktree)"
    )]
    Apply {
        fleet: String,
        #[arg(long, default_value = ".drover/rules.md")]
        rules: std::path::PathBuf,
    },
}

#[derive(Subcommand)]
enum ProfilesCmd {
    #[command(
        about = "Write each provider's per-role profile (settings + hooks, role prompt, usage doc) and hook approvals"
    )]
    Sync {
        #[arg(
            long,
            help = "Write nothing; exit 1 unless every fleet profile lists its hook as `✓ allowed`"
        )]
        check: bool,
        #[arg(
            help = "#592: only this profile, of role family `--role`, without fleet.toml (spawn's, on a lane's remote host)"
        )]
        #[arg(long, requires = "role", hide = true)]
        profile: Option<String>,
        #[arg(long, requires = "profile", hide = true)]
        role: Option<String>,
    },
}

/// Line budget of `help --agent` (tests/spawn.rs): raise it here, next to the text it counts (+1 for #130's `--owner-override`, +8 for #151's `extend`/`park`, +1 for #164's `--orphans`, +14 for #157's `hosts`, `queue set`, `--needs`, `--host`, +4 for #319's `done`, +19 for #355's `thread` and `ask --thread`, +1 for #386's `thread reply --file`, +19 for #387's `config` and `secret`, +3 for #413's `thread handoff`, +10 for #405's `thread title`/`archive`/`restore` and `thread say --file`, +9 for #419's `lesson`, +2 for #412's `queue add --design` and the design states, +2 for #417's `queue add --verify` and the verify stage, +1 for #461's orchestrator `timer install`, +1 for #420's `queue add --repro`, +1 for #344's `migrate --live`, +2 for #478's `kill`/`close --host`, +1 for #404's `thread new --harness`, +1 for #541's `answer --basis`, +15 for #538's `card`, +4 for #510's `lesson apply`, +4 for #505's `queue unblock`, +5 for #511's `turn`, +4 for #574's `amend`, +4 for #646's `wip prune`, +3 for #630's `hosts eject`, +2 for #668's `cap`, +4 for #677's `why`, +3 for #637's `hosts keys`, +2 for #730's `queue add --architect` and the two roles, +2 for #734's `queue add --perf`/`--refactor`, +6 for #712's `unpark` and `park --why`/`--by`, +1 for #780's `thread new --orchestrator`, +2 for #817's `extend --typed`/`--basis`, +1 for #838's `answer --note`, +3 for #844's `config show`, +3 for #889's `hotspots`, +4 for #845's `diagram`).
pub const HELP_AGENT_MAX: usize = 366;

/// The one usage doc both harnesses get: the commands generated from the CLI definition, the rest from `roles/usage-*.md`.
pub fn usage_doc() -> String {
    let mut cmd = Cli::command();
    cmd.build();
    let mut s = String::from(include_str!("../roles/usage-head.md"));
    let owner = [
        "accounts",
        "doctor",
        "drive",
        "fleet",
        "kick",
        "pause",
        "reclaim",
        "report",
        "resume",
        "serve",
        "service",
        "setup",
        "stats",
        "tick",
        "timer",
        "self-update",
    ];
    let subs = cmd
        .get_subcommands()
        .filter(|c| !c.is_hide_set() && !owner.contains(&c.get_name()));
    let subs: Vec<_> = subs
        .flat_map(|c| {
            if c.has_subcommands() {
                c.get_subcommands().filter(|c| !c.is_hide_set()).collect()
            } else {
                vec![c]
            }
        })
        .collect();
    for sub in subs {
        let mut sub = sub.clone();
        let about = sub
            .get_about()
            .map(std::string::ToString::to_string)
            .unwrap_or_default();
        let usage = sub.render_usage().to_string().replace("Usage: ", "");
        s += &format!("\n`{}`\n  {about}\n", usage.trim());
        for a in sub
            .get_arguments()
            .filter(|a| !a.is_hide_set() && a.get_long().is_some_and(|l| l != "help"))
        {
            let h = a.get_help().map(std::string::ToString::to_string).unwrap_or_default();
            s += &format!("  --{}  {h}\n", a.get_long().unwrap());
        }
    }
    s + include_str!("../roles/usage-tail.md")
}

/// Entry point used by main and tests. Returns the process exit code.
pub fn main_with(args: Vec<String>, ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write, stdin: &mut dyn Read) -> i32 {
    let cli = match Cli::try_parse_from(&args) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { 2 } else { 0 };
        }
    };
    let quiet = matches!(
        cli.cmd,
        Cmd::Hook { .. }
            | Cmd::LaneEvent { .. }
            | Cmd::Heavy { .. }
            | Cmd::Statusline
            | Cmd::Migrate { .. }
            | Cmd::Help { .. }
    ) || matches!(
        cli.cmd,
        Cmd::CapCheck { .. } | Cmd::ArchCheck { .. } | Cmd::LessonCheck { .. }
    );
    if !quiet && args.first().is_some_and(|a| a.ends_with("herdsman")) {
        eprintln!("herdsman is now drover: the old name works for one more release");
    }
    if !quiet && !ctx.hints.is_empty() {
        eprintln!("{}", ctx.hints.join("\n"));
        return 1;
    }
    let lane_write = matches!(
        cli.cmd,
        Cmd::Ask { .. } | Cmd::Answer { .. } | Cmd::Verdict { .. } | Cmd::Done { .. } | Cmd::Turn { .. }
    );
    if let (Cmd::Turn { .. }, Ok(lane)) = (&cli.cmd, std::env::var("DROVER_LANE")) {
        let _ = std::fs::write(hook::turn_mark(&ctx.state, &lane), "turn");
    }
    let mut sent: Vec<String> = args.into_iter().skip(1).collect();
    for i in 1..sent.len() {
        if sent[i - 1] == "--file" {
            sent[i] = std::path::absolute(&sent[i]).map_or(sent[i].clone(), |p| p.display().to_string());
        }
    }
    let with = (r.shed().is_none())
        .then(|| Config::load(&ctx.config).ok()?.shed())
        .flatten()
        .map(|b| crate::shed::With(r, b));
    let r: &dyn Runner = with.as_ref().map_or(r, |w| w);
    let res = match lane_write.then(|| forward(r, &sent)).flatten() {
        Some(o) => o.map(|o| {
            let _ = write!(out, "{}", o.stdout);
            eprint!("{}", o.stderr);
            o.code
        }),
        None => dispatch(cli.cmd, ctx, r, out, stdin),
    };
    match res {
        Ok(code) => code,
        Err(e) => {
            if let Some(rf) = e.downcast_ref::<gate::Refusal>() {
                let _ = writeln!(out, "{rf}");
                3
            } else {
                eprintln!("drover: {e:#}");
                1
            }
        }
    }
}

pub(crate) fn forward(r: &dyn Runner, args: &[String]) -> Option<Result<runner::Out>> {
    if let (Ok(url), Ok(token)) = (std::env::var("DROVER_SERVICE"), std::env::var("DROVER_TOKEN")) {
        return Some(crate::service::post(r, &url, &token, args));
    }
    let lane = crate::outbox::lane()?;
    Some(crate::outbox::queue(&lane, args).map(|p| runner::Out {
        code: 0,
        stdout: crate::outbox::QUEUED.into(),
        stderr: format!("drover: queued in {}\n", p.display()),
    }))
}

pub(crate) fn foreign(args: &[String], lane: &str, events: &[Event]) -> Option<String> {
    let cli = Cli::try_parse_from(std::iter::once("drover").chain(args.iter().map(String::as_str)));
    let is = |e: &&Event, f: &str, id: &str| e.kind == "needs-decision" && e.fleet == f && e.str("id") == Some(id);
    let asked = |f: &str, id: &str| events.iter().rfind(|e| is(e, f, id))?.issue;
    let (f, n) = match cli.map(|c| c.cmd) {
        Err(e) => return Some(format!("unparsable: {}", e.kind())),
        Ok(Cmd::LaneEvent { .. } | Cmd::Turn { .. }) => return None,
        Ok(Cmd::Ask {
            fleet: Some(fleet),
            issue: Some(issue),
            ..
        }) => (fleet, issue),
        Ok(Cmd::Done { fleet, issue, .. } | Cmd::Verdict { fleet, issue, .. } | Cmd::Kick { fleet, issue, .. }) => {
            (fleet, issue.to_string())
        }
        Ok(Cmd::Answer { fleet, id, .. }) => (fleet.clone(), asked(&fleet, &id).map_or("-".into(), |n| n.to_string())),
        _ => return Some(format!("`{}` is no lane's own verb", args[0])),
    };
    (!lane.starts_with(&format!("{f}:{n}:"))).then(|| format!("names {f}#{n}, not the lane's {lane}"))
}

pub(crate) fn run(args: &[&str], ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write) -> Result<i32> {
    let cli = Cli::try_parse_from(std::iter::once("drover").chain(args.iter().copied()))?;
    dispatch(cli.cmd, ctx, r, out, &mut std::io::empty())
}

fn dispatch(cmd: Cmd, ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write, stdin: &mut dyn Read) -> Result<i32> {
    match cmd {
        Cmd::Migrate { .. } if std::env::var_os("DROVER_HOME").is_some_and(|v| v.is_empty()) => {
            bail!("DROVER_HOME is set but empty; unset it or point it at a home tree")
        }
        Cmd::Migrate { live: false } if crate::db::live(&ctx.state) => bail!(
            "{} is the live state dir: `drover migrate --live` to migrate it",
            ctx.state.parent().unwrap_or(std::path::Path::new(".")).display()
        ),
        Cmd::Migrate { live } => {
            crate::db::LIVE.store(live, std::sync::atomic::Ordering::Relaxed);
            migrate(&ctx.home, out)?;
            let old = ctx.home.join(".local/state/herdsman");
            let state = match ctx.state.strip_prefix(&old) {
                Ok(rest) => ctx.home.join(".local/state/drover").join(rest),
                _ => ctx.state.clone(),
            };
            crate::db::migrate(&state, out)?;
            crate::secret::retired(&state, out)?;
        }
        Cmd::Events { .. } => write!(out, "{}", crate::events::jsonl(&ctx.state)?)?,
        Cmd::Diagram { live, fleet } => crate::diagram::run(ctx, live, fleet.as_deref(), out)?,
        Cmd::CapCheck { config, rules } => return crate::overlap::cap_check(&config, &crate::repo_path(&rules), out),
        Cmd::ArchCheck {
            config,
            write_baseline,
            base,
            pr_body,
        } => {
            return crate::arch::check(
                &crate::repo_path(&config),
                write_baseline,
                base.as_deref(),
                pr_body.as_deref(),
                r,
                out,
            )
        }
        Cmd::LessonCheck { rules } => return crate::lesson::ci(r, &crate::repo_path(&rules), out),
        Cmd::Help { agent: true } => write!(out, "{}", usage_doc())?,
        Cmd::Help { agent: false } => write!(out, "{}", Cli::command().render_long_help())?,
        Cmd::Hook {
            role,
            git_pre_push: true,
        } => return pre_push(ctx, r, out, stdin, role.as_deref() == Some("rev")),
        Cmd::Hook {
            role,
            git_pre_push: false,
        } => run_hook(ctx, r, out, stdin, role.as_deref() == Some("rev"))?,
        Cmd::Statusline => {
            let mut buf = String::new();
            let _ = stdin.read_to_string(&mut buf);
            let env = |k: &str| std::env::var(k).unwrap_or_default();
            crate::usage::statusline(ctx, &env("DROVER_ACCOUNT"), &env("DROVER_LANE"), &buf, out);
        }
        Cmd::LaneEvent { kind, data } => lane_event(ctx, &kind, serde_json::from_str(&data)?),
        Cmd::Heavy {
            kind,
            max,
            kind_max,
            wait,
            cmd,
        } => {
            let me = crate::heavy::Ticket {
                lane: std::env::var("DROVER_LANE").unwrap_or_default(),
                kind,
                cmd,
                since: crate::now(),
                running: false,
                kind_max,
            };
            let timeout = |t: &crate::heavy::Ticket| {
                lane_event(
                    ctx,
                    "heavy-timeout",
                    json!({"kind": t.kind, "cmd": t.cmd, "since": t.since}),
                );
            };
            return crate::heavy::run(r, &ctx.state.with_file_name("heavy"), me, max, wait, timeout);
        }
        Cmd::Status(Scope { fleet, json }) => {
            let fleet = fleet.map(|f| crate::rename::current(&ctx.state, &f));
            let (events, all) = (working(&ctx.state)?, crate::events::states(&ctx.state)?);
            let rows: Vec<_> = all
                .values()
                .filter(|s| fleet.as_ref().is_none_or(|f| *f == s.fleet))
                .collect();
            if json {
                let rows = rows.iter().map(|s| {
                    let mut v = json!(s);
                    if let Some(c) = crate::overlap::cached(ctx, &s.fleet).remove(&s.issue) {
                        v["conflicts"] = json!(c);
                    }
                    v
                });
                writeln!(out, "{}", serde_json::to_string_pretty(&rows.collect::<Vec<_>>())?)?;
            } else if rows.is_empty() {
                writeln!(out, "no tracked issues")?;
            } else {
                let cfg = Config::load(&ctx.config).ok();
                let serial = |f: &str| queue::serial_use(&events, &all, f, cfg.as_ref()).map(|s| format!("{f}: {s}\n"));
                let mut serial: Vec<_> = rows.iter().filter_map(|s| serial(&s.fleet)).collect();
                serial.dedup();
                let off = cfg.as_ref().map(|c| crate::hosts::idle_off(c, &events));
                let spared = crate::hosts::Pass::fast(r).sparing(off.unwrap_or_default(), None);
                let look = crate::snapshot::Snap::read(ctx, &spared);
                let skip = |s: &crate::events::IssueState| {
                    let f = cfg.as_ref().and_then(|c| c.fleet.get(&s.fleet));
                    let skip = queue::skipped(&events, &all, f, &s.fleet, s.issue, ctx.now);
                    skip.map(|s| format!("skipped this pass: {}", s.1))
                };
                let skips: Vec<_> = rows.iter().map(|s| skip(s)).collect();
                let waits = cfg.map(|mut c| {
                    c.fleet.retain(|k, _| fleet.as_ref().is_none_or(|f| f == k));
                    crate::waiting::scan(
                        ctx,
                        &c,
                        &look,
                        &events,
                        &all_agents(&c, &look).unwrap_or_default(),
                        false,
                    )
                });
                let cfg = Config::load(&ctx.config).ok();
                let at = cfg.as_ref().map(|c| places(ctx, c, &look, &events)).unwrap_or_default();
                let hosts = cfg.as_ref().is_some_and(|c| !c.hosts.is_empty()) || at.values().any(|p| !p.is_empty());
                let pad = [5, 63][usize::from(hosts)];
                let c = |h: &str| format!(" {h:<14}").repeat(usize::from(hosts));
                let head = c("HOST");
                let host = |s: &crate::events::IssueState| {
                    let e = events.iter().rev().find(|e| e.is("spawn", &s.fleet, s.issue));
                    let place = s.active_lane().and_then(|l| at.get(l.1)).filter(|p| !p.is_empty());
                    c(place.map_or_else(|| e.map_or("-", |e| e.str("host").unwrap_or("local")), |p| p))
                };
                let root = |s: &crate::events::IssueState| {
                    let e = events.iter().rev().find(|e| e.is("spawn", &s.fleet, s.issue));
                    let root = e.and_then(|e| e.str("root")).map(|r| format!(" · root {r}"));
                    let wt = e
                        .filter(|e| !s.done() && e.str("machine").is_none())
                        .and_then(|e| e.str("worktree"));
                    let u = wt.and_then(|wt| crate::spawn::unsaved(r, std::path::Path::new(wt)));
                    let u = crate::spawn::unsaved_text(u).map(|u| format!(" · {u}"));
                    let qos = e.filter(|_| !s.done()).and_then(|e| e.str("qos"));
                    let qos = qos.map(|q| format!(" · qos {q}"));
                    root.unwrap_or_default() + &u.unwrap_or_default() + &qos.unwrap_or_default()
                };
                writeln!(
                    out,
                    "{:<12} {:<6} {:<12} {:<24} {:<5}{} WAITING",
                    "FLEET", "ISSUE", "STATE", "LANE", "AGE", head
                )?;
                for (s, skip) in rows.into_iter().zip(skips) {
                    let pending = crate::pending::of(&events, &s.fleet, s.issue).and_then(|p| p.line(ctx.now));
                    let wait = waits
                        .iter()
                        .flatten()
                        .find(|(x, _)| x.fleet == s.fleet && x.issue == s.issue);
                    let w = pending
                        .or_else(|| wait.map(|w| w.1.what.clone()))
                        .or(skip)
                        .or_else(|| {
                            let e = events.iter().rev().find(|e| e.is("spawn", &s.fleet, s.issue))?;
                            Some(format!("on {}: {}", e.str("host")?, e.str("why")?))
                        })
                        .unwrap_or_else(|| "-".into());
                    writeln!(out, "{:<pad$}{} {w}{}", status_line(s, ctx.now), host(s), root(s))?;
                }
                write!(out, "{}", serial.concat())?;
                if let Some(c) = &cfg {
                    let agents = all_agents(c, &look).unwrap_or_default();
                    write!(out, "{}", crate::hosts::slots_line(c, &events, &agents))?;
                }
                if let Some(t) = look.ts {
                    writeln!(
                        out,
                        "as of the step's look {} ago",
                        crate::events::age(ctx.now.saturating_sub(t))
                    )?;
                }
            }
        }
        Cmd::Set {
            fleet,
            role,
            harness,
            model,
            profile,
            effort,
        } => {
            let wanted = [
                ("harness", harness),
                ("model", model),
                ("profile", profile),
                ("effort", effort),
            ];
            set(
                ctx,
                out,
                &fleet,
                &role,
                wanted.into_iter().filter_map(|(k, v)| Some((k, v?))).collect(),
                None,
            )?;
        }
        Cmd::Pause { fleet } => pause(ctx, out, &fleet, true, None)?,
        Cmd::Resume { fleet } => pause(ctx, out, &fleet, false, None)?,
        Cmd::Next { fleet } => {
            if Config::load(&ctx.config).is_ok_and(|c| c.fleet.get(&fleet).is_some_and(|f| f.paused)) {
                writeln!(out, "{fleet}: paused")?;
                return Ok(1);
            }
            let (code, line) = queue::next(&working(&ctx.state)?, &fleet);
            writeln!(out, "{line}")?;
            return Ok(code);
        }
        Cmd::Board { fleet } => {
            let events = working(&ctx.state)?;
            let all = fleet.map_or_else(
                || queue::fleets(&events),
                |f| vec![crate::rename::current(&ctx.state, &f)],
            );
            let cfg = Config::load(&ctx.config).ok();
            let look = crate::snapshot::Snap::read(ctx, r);
            let at = cfg.as_ref().map(|c| places(ctx, c, &look, &events)).unwrap_or_default();
            let board = |f: &String| queue::board(ctx, &events, f, cfg.as_ref(), &crate::overlap::cached(ctx, f), &at);
            let boards: Vec<_> = all.iter().map(board).collect();
            for (k, _) in cfg.iter().flat_map(Config::warnings) {
                writeln!(out, "! {k} is retired → drover doctor")?;
            }
            if let Some(l) = crate::update::paused_line(r, &events) {
                writeln!(out, "{l}")?;
            }
            write!(out, "{}", boards.join("\n"))?;
        }
        Cmd::Report { id } => crate::report::show(ctx, out, id.as_deref())?,
        Cmd::Asks {
            scope: Scope { fleet, json },
            all,
        } => {
            let fleet = fleet.map(|f| crate::rename::current(&ctx.state, &f));
            crate::asks(ctx, out, fleet.as_deref(), json, all)?;
        }
        Cmd::Serve {
            cmd: Some(ServeCmd::Pair),
            ..
        } => writeln!(out, "now: open the page, tap Pair, then run serve approve <code>")?,
        Cmd::Serve {
            cmd: Some(ServeCmd::Approve { code }),
            ..
        } => writeln!(out, "paired {}", devices::approve(ctx, &code)?)?,
        Cmd::Serve {
            cmd: Some(ServeCmd::Devices),
            ..
        } => {
            for d in devices::load(ctx).into_iter().filter(|d| d.code.is_none()) {
                let left = (d.used + devices::YEAR).saturating_sub(ctx.now);
                if left > 0 {
                    writeln!(out, "{} expires in {}d", d.label, left / 86_400)?;
                }
            }
        }
        Cmd::Serve {
            cmd: Some(ServeCmd::Pending),
            ..
        } => devices::pending(ctx).iter().try_for_each(|d| {
            writeln!(
                out,
                "{} {} {}m left",
                d.code.as_deref().unwrap_or_default(),
                d.label,
                (d.created + 600 - ctx.now).div_ceil(60)
            )
        })?,
        Cmd::Serve {
            export: Some(dir),
            demo,
            ..
        } => (SERVE.get().context("no serve")?.1)(ctx, r, &dir, demo)?,
        Cmd::Serve { addr, read_only, .. } => {
            (SERVE.get().context("no serve")?.0)(ctx, r, &addr, read_only)?;
        }
        Cmd::Push { cmd: PushCmd::Test } => {
            let cfg = Config::load(&ctx.config)?;
            let Some(sub) = cfg.webpush_sub() else {
                bail!("no `type = \"webpush\"` notifier in fleet.toml (docs/serve.md)")
            };
            let ev = crate::events::Event::new(ctx.now, "drover", None, "push-test", json!({"reason": "test alert"}));
            writeln!(out, "{}", crate::webpush::send(&ctx.state, sub, &ev, None)?)?;
        }
        Cmd::Queue {
            fleet,
            cmd: QueueCmd::Ls { json },
        } => {
            let fleet = crate::rename::current(&ctx.state, &fleet);
            let rows = queue::ls_json(&working(&ctx.state)?, &crate::events::states(&ctx.state)?, &fleet);
            if json {
                writeln!(out, "{}", serde_json::to_string_pretty(&rows)?)?;
            }
            for r in rows.as_array().into_iter().flatten().filter(|_| !json) {
                let i = queue::Item {
                    blockers: serde_json::from_value(r["blocked_by"].clone())?,
                    ext: serde_json::from_value(r["blocked_ext"].clone())?,
                    ..queue::Item::default()
                };
                let mut on = String::new();
                if !i.ext.is_empty() || !i.blockers.is_empty() {
                    on = format!("  blocked by {}", queue::labels(&i));
                }
                if r["serial"] == true {
                    on += "  serial";
                }
                if r["design"] == true {
                    on += "  design";
                }
                if r["architect"] == true {
                    on += "  architect";
                }
                if r["repro"] == true {
                    on += "  repro";
                }
                if let Some(p) = r["playbook"].as_str() {
                    on += &format!("  {p}");
                }
                if let Some(v) = r["verify"].as_bool() {
                    on += if v { "  verify on" } else { "  verify off" };
                }
                if r["needs"].as_array().is_some_and(|n| !n.is_empty()) {
                    on += &format!("  needs {}", r["needs"].to_string().replace(['[', ']', '"'], ""));
                }
                if let Some(w) = r["why"].as_str() {
                    on += &format!("  parked: {w}");
                }
                let n = |k: &str| r[k].as_u64().unwrap_or_default();
                writeln!(
                    out,
                    "{:>2} #{:<5} {}{on}",
                    n("pos"),
                    n("issue"),
                    r["state"].as_str().unwrap_or_default()
                )?;
            }
        }
        Cmd::Doctor { json, config_only } => return crate::doctor::doctor(ctx, r, out, json, config_only),
        Cmd::Setup { host, name, no_load } => {
            let Some((h, n)) = host.zip(name) else {
                return crate::timer::setup(ctx, r, out, !no_load);
            };
            let pane = crate::setup::start(ctx, &Config::load(&ctx.config)?, r, &h, &n, "cli")?;
            writeln!(
                out,
                "{n} on {h}: shed terminal {pane}; it closes when the login exits or in 10 min"
            )?;
        }
        Cmd::Tick => return crate::timer::tick(ctx, r, out),
        Cmd::Profiles {
            cmd:
                ProfilesCmd::Sync {
                    profile: Some(p),
                    role: Some(role),
                    ..
                },
        } => {
            for h in crate::providers::known() {
                crate::providers::get(h).sync(ctx, &role, &role, &p, &usage_doc(), out)?;
            }
        }
        Cmd::Service {
            api,
            board,
            install: true,
        } => crate::timer::service_unit(ctx, r, out, &api, board.as_deref())?,
        Cmd::Service { api, board, .. } => (SERVICE.get().context("no service")?)(ctx, &api, board.as_deref())?,
        cmd => {
            let cfg = match cmd {
                Cmd::Check => check::config(ctx)?,
                _ => Config::load(&ctx.config)?,
            };
            let fast = matches!(cmd, Cmd::Drive { fast: true, .. });
            let pass = if fast {
                crate::hosts::Pass::fast(r)
            } else {
                crate::hosts::Pass::new(r, None)
            };
            let off = match cmd {
                Cmd::Check | Cmd::Drive { .. } => crate::hosts::idle_off(&cfg, &working(&ctx.state)?),
                _ => std::collections::BTreeSet::new(),
            };
            let pass = pass.sparing(off, Some((ctx, &cfg)));
            let r: &dyn Runner = match cmd {
                Cmd::Check | Cmd::Clean { .. } | Cmd::Drive { .. } | Cmd::Reclaim { .. } => &pass,
                _ => r,
            };
            let kick = |f: &str, n, why| {
                let _ = crate::drive::kick(ctx, r, f, n, why);
            };
            match cmd {
                Cmd::Profiles {
                    cmd: ProfilesCmd::Sync { check: true, .. },
                } => {
                    if !spawn::profiles_check(ctx, &cfg, r, out)? {
                        return Ok(1);
                    }
                }
                Cmd::Profiles {
                    cmd: ProfilesCmd::Sync { check: false, .. },
                } => spawn::profiles_sync(ctx, &cfg, &usage_doc(), out)?,
                Cmd::Spawn { fleet, issue, role, o } => {
                    if crate::orchestrator::is_role(&role) {
                        return gate::refused("BAD_VALUE", "an orchestrator starts from `drover thread say`");
                    }
                    if role == "orch" {
                        let why = "drover drives fleets itself; see drover timer";
                        return gate::refused("NO_ORCH", why);
                    }
                    let issue = issue
                        .parse()
                        .map_err(|_| anyhow::anyhow!("issue must be a number, not `{issue}`"))?;
                    if !spawn::spawn(ctx, &cfg, r, out, (&fleet, issue, &role), &o)? {
                        return Ok(4);
                    }
                }
                Cmd::Adopt {
                    fleet,
                    issue,
                    role,
                    pane,
                } => {
                    spawn::adopt(ctx, &cfg, r, out, (&fleet, issue, &role), &pane, &serde_json::json!({}))?;
                }
                Cmd::Ask {
                    cmd: Some(AskCmd::Withdraw { fleet, id, by, new }),
                    ..
                } => crate::withdraw(
                    ctx,
                    &cfg,
                    out,
                    (&fleet, &id),
                    &crate::orchestrator::by(&by),
                    new.as_deref(),
                )?,
                Cmd::Ask {
                    fleet: Some(fleet),
                    issue: Some(issue),
                    question: Some(question),
                    options,
                    recommend: n,
                    kind,
                    thread,
                    ..
                } => {
                    let at = lane(&issue)?;
                    let how = (n, kind.as_str(), thread.as_deref());
                    crate::ask(ctx, &cfg, r, out, (&fleet, at), (&question, &options), how)?;
                    at.inspect(|n| kick(&fleet, *n, "ask"));
                }
                Cmd::Answer {
                    fleet,
                    id,
                    choice,
                    typed: Some(line),
                    note,
                    ..
                } => {
                    let on = std::env::var("DROVER_THREAD").ok();
                    let how = (choice.as_deref(), note.as_deref());
                    crate::answer_typed(ctx, &cfg, r, out, (&fleet, &id), (&line, on.as_deref()), how)?;
                }
                Cmd::Answer {
                    fleet,
                    id,
                    choice: Some(choice),
                    by,
                    basis,
                    ..
                } => match std::env::var("DROVER_THREAD").ok().filter(|t| !t.is_empty()) {
                    Some(t) => {
                        let how = (t.as_str(), basis.as_deref().unwrap_or_default());
                        crate::orchestrator_answer(ctx, &cfg, r, out, (&fleet, &id), &choice, how)?;
                    }
                    None => crate::answer(
                        ctx,
                        &cfg,
                        r,
                        out,
                        (&fleet, &id),
                        &choice,
                        (&crate::orchestrator::by(&by), None),
                    )?,
                },
                Cmd::Amend {
                    fleet,
                    issue,
                    text,
                    basis,
                } => crate::amend::amend(ctx, &cfg, r, out, (&fleet, issue), &basis, &text)?,
                Cmd::Check => check::check(ctx, &cfg, r, out)?,
                Cmd::Thread { cmd } => match cmd {
                    ThreadCmd::New { .. } if std::env::var("DROVER_THREAD").is_ok_and(|t| !t.is_empty()) => {
                        gate::refused("USE_HANDOFF", "an orchestrator starts threads with `thread handoff`")?;
                    }
                    ThreadCmd::New {
                        repo,
                        title,
                        harness,
                        orchestrator,
                    } => {
                        let o = (harness.as_deref(), orchestrator.as_deref());
                        let id = crate::orchestrator::new_with(ctx, &cfg, &repo, &title, o)?;
                        writeln!(out, "{id}")?;
                    }
                    ThreadCmd::Handoff { from, to, title, text } => {
                        let to = crate::orchestrator::handoff(ctx, &cfg, r, [&from, &to, &title, &text])?;
                        writeln!(out, "{to}")?;
                    }
                    ThreadCmd::Say { id, text, by, file } => {
                        let name = |p: &PathBuf| p.file_name().map_or(String::new(), |n| n.to_string_lossy().into());
                        let load =
                            |p: &PathBuf| Ok((name(p), std::fs::read(p).with_context(|| p.display().to_string())?));
                        let files = file.iter().map(load).collect::<Result<Vec<_>>>()?;
                        crate::orchestrator::find(&working(&ctx.state)?, &id)?;
                        let files = crate::orchestrator::upload(ctx, &cfg, &id, &files)?;
                        let agent = crate::orchestrator::say(ctx, &cfg, r, (&id, &text, &files), (&by, false))?;
                        writeln!(out, "{id}: -> {agent}")?;
                    }
                    ThreadCmd::Title { id, title } => crate::orchestrator::retitle(ctx, &cfg, &id, &title)?,
                    ThreadCmd::Archive { id } => crate::orchestrator::archive(ctx, &cfg, r, &id, true)?,
                    ThreadCmd::Restore { id } => crate::orchestrator::archive(ctx, &cfg, r, &id, false)?,
                    ThreadCmd::Reply { id, text, file } => crate::orchestrator::reply(ctx, &cfg, &id, &text, &file)?,
                    ThreadCmd::Show { id, json } => crate::orchestrator::show(ctx, &cfg, out, &id, json)?,
                    ThreadCmd::Ls(Scope { fleet, json }) => {
                        crate::orchestrator::ls(ctx, &cfg, out, fleet.as_deref(), json)?;
                    }
                },
                Cmd::Config { cmd } => {
                    let (id, apply) = match cmd {
                        ConfigCmd::Show => return crate::config::show(&cfg, out).map(|()| 0),
                        ConfigCmd::Propose { thread, file } => {
                            return crate::propose::propose(ctx, &cfg, out, &thread, &file).map(|()| 0)
                        }
                        ConfigCmd::Apply { id } => (id, true),
                        ConfigCmd::Discard { id } => (id, false),
                    };
                    writeln!(out, "{}", crate::propose::settle(ctx, &cfg, r, &id, apply)?)?;
                }
                Cmd::Fleet {
                    cmd: FleetCmd::Rename { old, new, yes },
                } => crate::rename::run(ctx, r, out, (&old, &new), yes)?,
                Cmd::Wip {
                    cmd: WipCmd::Prune { fleet, dry_run },
                } => check::prune(ctx, &cfg, r, out, &fleet, dry_run)?,
                Cmd::Secret { cmd } => match cmd {
                    SecretCmd::Request { thread, name, why } => {
                        crate::secret::request(ctx, &cfg, &thread, &name, &why)?;
                    }
                    SecretCmd::Ls => crate::secret::ls(ctx, out)?,
                    SecretCmd::Rm { fleet, name } => crate::secret::rm(ctx, &cfg, &fleet, &name)?,
                },
                Cmd::Card { cmd } => {
                    let line = match cmd {
                        CardCmd::Post {
                            thread,
                            command,
                            why,
                            blast,
                            dir,
                            timeout,
                        } => {
                            let c = crate::card::Card {
                                cmd: &command,
                                why: &why,
                                blast: &blast,
                                dir: &dir,
                                timeout,
                            };
                            crate::card::post(ctx, &cfg, &thread, &c)?
                        }
                        CardCmd::Approve { id, sha } => crate::card::settle(ctx, &cfg, r, &id, true, sha.as_deref())?,
                        CardCmd::Deny { id } => crate::card::settle(ctx, &cfg, r, &id, false, None)?,
                        CardCmd::Run { id } => crate::card::run(ctx, &cfg, r, &id)?,
                    };
                    writeln!(out, "{line}")?;
                }
                Cmd::Hosts { cmd } => match cmd {
                    None => crate::hosts::list(ctx, &cfg, r, out, false)?,
                    Some(HostsCmd::Refresh) => crate::hosts::list(ctx, &cfg, r, out, true)?,
                    Some(HostsCmd::On { name }) => crate::hosts::toggle(ctx, out, &name, true, "cli")?,
                    Some(HostsCmd::Off { name }) => crate::hosts::toggle(ctx, out, &name, false, "cli")?,
                    Some(HostsCmd::Eject { name, root }) => {
                        return crate::roots::eject(ctx, &cfg, r, out, &name, root.as_deref());
                    }
                    Some(HostsCmd::Destroy { name }) => {
                        let wip = crate::cloud::stop(ctx, &cfg, r, &name, "destroy", "owner")?;
                        writeln!(out, "{name}: destroyed; wip {}", wip.join(" "))?;
                    }
                    Some(HostsCmd::Bootstrap { name }) => crate::cloud::bootstrap(ctx, &cfg, r, &name, out)?,
                    Some(HostsCmd::Snapshot { name }) => crate::cloud::snapshot(ctx, &cfg, r, &name)?,
                    Some(HostsCmd::Keys { cmd: None }) => crate::proxy::list(ctx, out)?,
                    Some(HostsCmd::Keys {
                        cmd: Some(KeysCmd::Revoke { id }),
                    }) => crate::proxy::revoke(ctx, &cfg, &id, out)?,
                },
                Cmd::Proxy => crate::proxy::serve(ctx, &cfg, r)?,
                Cmd::SelfUpdate { dry_run, pause, resume } => {
                    return crate::update::self_update(ctx, &cfg, r, out, (dry_run, pause, resume));
                }
                Cmd::Timer { action, label, no_load } => {
                    return crate::timer::timer(ctx, &cfg, r, out, (&action, &label, !no_load));
                }
                Cmd::Drive { fleet, fast } => crate::drive::drive(ctx, &cfg, r, out, fleet.as_deref(), false, fast)?,
                Cmd::Kick {
                    fleet,
                    issue,
                    why,
                    now,
                    id,
                } => {
                    crate::drive::kicked(ctx, &cfg, r, out, (&fleet, issue, &why, now, id.as_deref()))?;
                }
                Cmd::Waiting(Scope { fleet, json }) => {
                    let (events, agents) = (working(&ctx.state)?, all_agents(&cfg, r).unwrap_or_default());
                    let at = places(ctx, &cfg, r, &events);
                    let all = crate::waiting::scan(ctx, &cfg, r, &events, &agents, true);
                    let mut rows: Vec<_> = all
                        .into_iter()
                        .filter(|(s, w)| s.lane.as_ref() == Some(&w.lane) && !s.done() && s.state != "parked")
                        .map(|x| x.1)
                        .collect();
                    let (log, st) = (crate::waiting::Log::new(&events), crate::events::fold(&events));
                    for s in crate::waiting::guarded(&events, &cfg) {
                        let (f, pr) = (&cfg.fleet[&s.fleet], crate::gh::cached_pr(ctx, &s.fleet, s.issue));
                        let un = crate::waiting::undriven(&log, &s, f, &pr, &agents, ctx.now);
                        let note = un.map_or(String::new(), |t| {
                            format!(" · nothing drives this (since {})", crate::events::age(ctx.now - t))
                        });
                        if let Some(w) = rows.iter_mut().find(|w| w.fleet == s.fleet && w.issue == s.issue) {
                            w.what += &note;
                            continue;
                        }
                        let (who, what) = match &s.lane {
                            Some(l) => {
                                let how = crate::waiting::recovered(&events, l, ctx.now);
                                ("none", format!("waiting on {l} (working){how}{note}"))
                            }
                            None => match crate::drive::retrying(ctx, &events, &s.fleet, s.issue)
                                .map(|w| (String::new(), w))
                                .or_else(|| crate::queue::skipped(&events, &st, Some(f), &s.fleet, s.issue, ctx.now))
                            {
                                Some((_, why)) => ("drive", format!("next in the queue, held back: {why}{note}")),
                                None => ("drive", format!("next in the queue: drive starts it{note}")),
                            },
                        };
                        let slot = crate::drive::slot_wait(ctx, &events, &s.fleet, s.issue);
                        let why = match crate::drive::last_pass(ctx, &s.fleet) {
                            _ if s.lane.is_some() => None,
                            _ if f.paused => Some(format!("paused: drover resume {}", s.fleet)),
                            None => Some(format!("drive hasn't passed this fleet yet: drover drive {}", s.fleet)),
                            Some(t) if ctx.now.saturating_sub(t) > 600 && slot.is_some() => slot,
                            Some(_) if let Some(since) = crate::timer::running(ctx, &cfg) => Some(format!(
                                "drive's pass running for {}",
                                crate::events::age(ctx.now.saturating_sub(since))
                            )),
                            Some(t) => {
                                let late = ctx.now.saturating_sub(t);
                                let stuck = if late > crate::timer::stuck_secs(&cfg) {
                                    ": tick stuck? drover doctor"
                                } else {
                                    ""
                                };
                                let last = format!(" (last {} ago){stuck}", crate::events::age(late));
                                Some(crate::promise::Say::Pass(last).line())
                            }
                        };
                        let (fleet, issue, since, next_cmd) = (s.fleet, s.issue, s.last_ts, why);
                        let lane = s.lane.unwrap_or_default();
                        rows.push(crate::waiting::Waiting {
                            fleet,
                            issue,
                            lane,
                            who,
                            what,
                            since,
                            next_cmd,
                            promise: None,
                        });
                    }
                    rows.retain(|w| fleet.as_ref().is_none_or(|f| *f == w.fleet));
                    for w in &mut rows {
                        if let Some(l) = crate::pending::of(&events, &w.fleet, w.issue).and_then(|p| p.line(ctx.now)) {
                            (w.who, w.what) = ("drive", l);
                        }
                    }
                    let slots = crate::drive::holders(ctx, &cfg, r);
                    if !json && !slots.is_empty() {
                        writeln!(out, "slots: {slots}")?;
                    }
                    if json {
                        writeln!(out, "{}", serde_json::to_string_pretty(&rows)?)?;
                    }
                    for w in rows.iter().filter(|_| !json) {
                        let place = at
                            .get(&w.lane)
                            .filter(|p| !p.is_empty())
                            .map_or(String::new(), |p| format!(" · {p}"));
                        writeln!(
                            out,
                            "{} #{} {}: {} ({}{place}){}",
                            w.fleet,
                            w.issue,
                            w.who,
                            w.what,
                            crate::events::age(ctx.now.saturating_sub(w.since)),
                            w.next_cmd
                                .as_deref()
                                .or((w.who == "drive").then_some("?"))
                                .map_or(String::new(), |c| format!(" → {c}"))
                        )?;
                    }
                }
                Cmd::Accounts { cmd: None } => access::list(ctx, &cfg, r, out)?,
                Cmd::Accounts {
                    cmd: Some(AccountsCmd::Reset { name }),
                } => access::reset(ctx, &cfg, out, &name)?,
                Cmd::Protection => return protection::protection(&cfg, r, out),
                Cmd::Stats {
                    cmd: StatsCmd::Land { fleet },
                } => crate::land::stats(ctx, &cfg, out, &fleet)?,
                Cmd::Clean {
                    fleet,
                    lane,
                    yes,
                    orphans,
                    scheduled,
                } => {
                    if scheduled {
                        return check::scheduled(ctx, &cfg, r, out).map(|()| 0);
                    }
                    let what = (None, lane.as_deref(), false, !crate::drive::in_act());
                    check::clean_all(ctx, &cfg, r, out, fleet.as_deref(), what, yes)?;
                    if lane.is_none() {
                        check::orphans(ctx, &cfg, out, orphans && yes)?;
                        check::strays(ctx, &cfg, out, yes)?;
                        check::remote_orphans(&cfg, r, out, orphans && yes)?;
                        for f in cfg.fleet.keys().filter(|f| fleet.as_ref().is_none_or(|x| x == *f)) {
                            if let Err(e) = check::prune(ctx, &cfg, r, out, f, !yes) {
                                writeln!(out, "{f}: wip prune failed: {e:#}")?;
                            }
                        }
                    }
                }
                Cmd::Runners { cmd } => {
                    use crate::runners::Verb;
                    let (verb, id) = match cmd {
                        RunnersCmd::Status => (Verb::Status, None),
                        RunnersCmd::Revive { vm } => (Verb::Revive(vm), None),
                        RunnersCmd::Add { n, approved } => (Verb::Add(n), approved),
                        RunnersCmd::Remove { target, approved } => (Verb::Remove(target), approved),
                        RunnersCmd::Tokens => (Verb::Tokens, None),
                        RunnersCmd::RetireLocal { approved } => (Verb::RetireLocal, approved),
                    };
                    crate::runners::run(ctx, &cfg, r, out, verb, id.as_deref())?;
                }
                Cmd::Reclaim { apply } => check::reclaim(ctx, &cfg, r, out, stdin, apply)?,
                Cmd::Stop { agent } => check::stop(ctx, &cfg, r, out, &agent)?,
                Cmd::Close { agent, host } => check::close(ctx, &cfg, r, out, &agent, host.as_deref())?,
                Cmd::Kill { agent, yes, host } => check::kill(ctx, &cfg, r, out, (&agent, host.as_deref()), yes)?,
                Cmd::Verdict {
                    fleet,
                    issue,
                    verdict,
                    sha,
                    file,
                } => {
                    let pass = verdict == "PASS";
                    gate::verdict(ctx, &cfg, r, out, (&fleet, issue), pass, &sha, &file)?;
                    let id = format!("{sha}-{verdict}");
                    let _ = crate::drive::kick_once(ctx, r, (&fleet, issue), "verdict", Some(&id));
                }
                Cmd::Merge {
                    fleet,
                    issue,
                    owner_override: o,
                } => gate::merge(ctx, &cfg, r, out, &fleet, issue, o)?,
                Cmd::Extend {
                    fleet,
                    issue,
                    by,
                    no_spawn,
                    typed,
                    basis,
                } => match std::env::var("DROVER_THREAD").ok().filter(|t| !t.is_empty()) {
                    Some(t) => {
                        let how = (typed.as_deref(), basis.as_deref());
                        crate::orchestrator_extend(ctx, &cfg, r, out, (&fleet, issue), !no_spawn, &t, how)?;
                    }
                    None if typed.is_some() || basis.is_some() => {
                        gate::refused(
                            "BAD_VALUE",
                            "--typed and --basis are an orchestrator's, run in its thread",
                        )?;
                    }
                    None => gate::needs_human(ctx, &cfg, r, out, (&fleet, issue), "extend", (!no_spawn, &by, None))?,
                },
                Cmd::Done {
                    fleet,
                    issue,
                    no_change,
                } => {
                    let e = Event::new(ctx.now, &fleet, Some(issue), "fix-done", json!({"why": no_change}));
                    emit(&ctx.state, &cfg, e)?;
                    writeln!(out, "recorded {fleet}#{issue} fix-done: {no_change}")?;
                    crate::lesson::done(ctx, &cfg, r, out, (&fleet, issue), &no_change)?;
                }
                Cmd::Turn { state, why } => {
                    let lane = std::env::var("DROVER_LANE").unwrap_or_default();
                    let mut it = lane.splitn(3, ':');
                    let (Some(f), Some(Ok(n)), Some(role)) = (it.next(), it.next().map(str::parse), it.next()) else {
                        bail!("`drover turn` is a lane's own verb: DROVER_LANE names no <fleet>:<issue>:<role>");
                    };
                    if cfg.fleet(f)?.family(role) != "impl" {
                        bail!("a {role} lane reports no turn: impl, fix and update lanes do");
                    }
                    let note = (state == "done").then(|| crate::why::turn(ctx, &cfg, r, out, (f, n), role));
                    let note = note.transpose()?.flatten();
                    let mut d = json!({"state": state, "why": why, "role": role});
                    if let Some((code, w)) = &note {
                        d["note"] = json!(format!("{code}: {w}"));
                    }
                    emit(&ctx.state, &cfg, Event::new(ctx.now, f, Some(n), "turn-end", d))?;
                    writeln!(out, "recorded {f}#{n} turn-end: {state}")?;
                    if state == "need-input" {
                        writeln!(out, "ask the question with `drover ask {f} {n} …` if you have not")?;
                    }
                    kick(f, n, "turn");
                    if let Some((code, w)) = note {
                        return gate::refused(code, w);
                    }
                }
                Cmd::Why { at, json } => crate::why::show(ctx, &cfg, r, out, &at, json)?,
                Cmd::Lesson {
                    cmd: Some(LessonCmd::Digest { fleet, days }),
                    ..
                } => write!(out, "{}", crate::lesson::digest(ctx, &cfg, r, &fleet, days)?)?,
                Cmd::Lesson {
                    cmd: Some(LessonCmd::Apply { fleet, rules }),
                    ..
                } => crate::lesson::apply(ctx, &cfg, r, out, &fleet, &crate::repo_path(&rules))?,
                Cmd::Lesson { fleet, .. } => {
                    let fleet = fleet.context("usage: drover lesson <FLEET> | drover lesson digest <FLEET>")?;
                    if !crate::lesson::run(ctx, &cfg, r, out, &fleet, None)? {
                        return Ok(4);
                    }
                }
                Cmd::Park { fleet, issue, why, by } => {
                    gate::needs_human(ctx, &cfg, r, out, (&fleet, issue), "park", (false, &by, why.as_deref()))?;
                }
                Cmd::Unpark { fleet, issue, by } => gate::unpark(ctx, &cfg, out, (&fleet, issue), &by)?,
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
                        writeln!(
                            out,
                            "{}",
                            gh::summary(&pr, &branch, &cfg.fleet(&fleet)?.required_checks)
                        )?;
                    }
                    if pr.is_null() {
                        return Ok(1);
                    }
                }
                Cmd::Cap { fleet } => return crate::overlap::cap_view(ctx, &cfg, r, &fleet, out),
                Cmd::Hotspots { fleet } => {
                    for name in fleet.map_or_else(|| cfg.fleet.keys().cloned().collect(), |f| vec![f]) {
                        crate::hotspots::run(ctx, &cfg, r, out, &name)?;
                    }
                }
                Cmd::Wait {
                    fleet,
                    issue,
                    until,
                    timeout,
                } => {
                    let branch = gate::branch_of(gate::issue_state(ctx, &fleet, issue)?.as_ref(), issue);
                    return gh::wait(
                        ctx,
                        r,
                        out,
                        cfg.fleet(&fleet)?,
                        (&fleet, issue),
                        &branch,
                        until,
                        timeout,
                    );
                }
                Cmd::Queue { fleet, cmd } => {
                    let blockers = |on: &[String]| -> Vec<serde_json::Value> {
                        on.iter()
                            .map(|b| b.parse::<u64>().map_or(json!(b), |n| json!(n)))
                            .collect()
                    };
                    let groups = &cfg.fleet(&fleet)?.serial;
                    let known = |g: &String| g.is_empty() || groups.iter().any(|x| x.name == *g);
                    let d = match cmd {
                        QueueCmd::Add {
                            mut issues,
                            top,
                            after,
                            serial,
                            needs,
                            design,
                            architect,
                            verify,
                            repro,
                            perf,
                            refactor,
                        } => {
                            let g =
                                serial.map(|g| g.parse().map(|n| issues.insert(0, n)).map_or(g, |()| String::new()));
                            anyhow::ensure!(
                                g.iter().all(known),
                                "no serial group `{}` in fleet.{fleet}",
                                g.unwrap_or_default()
                            );
                            let mut d = queue::op("add", &issues, top, None, after, None);
                            for &i in &issues {
                                let _ = gh::spec(ctx, r, cfg.fleet(&fleet)?, &fleet, i, false);
                            }
                            if let Some(g) = g {
                                d["serial"] = if g.is_empty() { json!(true) } else { json!(g) };
                            }
                            if !needs.is_empty() {
                                d["needs"] = json!(needs);
                            }
                            if design {
                                d["design"] = json!(true);
                            }
                            if architect {
                                d["architect"] = json!(true);
                            }
                            if let Some(v) = verify {
                                d["verify"] = json!(v);
                            }
                            if repro {
                                d["repro"] = json!(true);
                            }
                            if perf || refactor {
                                d["playbook"] = json!(if perf { "perf" } else { "refactor" });
                            }
                            d
                        }
                        QueueCmd::Set { issue, key, tags } => {
                            anyhow::ensure!(
                                key != "serial" || tags.iter().all(known),
                                "no such serial group: {tags:?}"
                            );
                            let on = ["on", "off"].iter().any(|o| tags == [*o]);
                            anyhow::ensure!(
                                !["design", "architect", "verify", "repro"].contains(&&*key) || on,
                                "{key} is `on` or `off`"
                            );
                            let mut d = queue::op("set", &[issue], false, None, None, None);
                            d[key.as_str()] = json!(tags);
                            d
                        }
                        QueueCmd::Mv {
                            issue,
                            top,
                            before,
                            after,
                        } => queue::op("mv", &[issue], top, before, after, None),
                        QueueCmd::Rm { issue } => queue::op("rm", &[issue], false, None, None, None),
                        QueueCmd::Block { issue, on } => json!({"op": "block", "issues": [issue], "on": blockers(&on)}),
                        QueueCmd::Unblock { issue, on } => {
                            json!({"op": "unblock", "issues": [issue], "on": blockers(&on)})
                        }
                        QueueCmd::Ls { .. } => unreachable!(),
                    };
                    let mut d = d;
                    if let Some(t) = std::env::var("DROVER_THREAD")
                        .ok()
                        .filter(|_| d.get("design").is_some() || d.get("architect").is_some())
                    {
                        d["design_thread"] = json!(t);
                    }
                    writeln!(out, "{}", queue_change(ctx, &cfg, &fleet, d, None)?)?;
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(0)
}

fn lane(issue: &str) -> Result<Option<u64>> {
    match issue {
        "-" => Ok(None),
        n => {
            Ok(Some(n.parse().map_err(|_| {
                anyhow::anyhow!("issue must be a number or `-`, not `{n}`")
            })?))
        }
    }
}

fn run_hook(ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write, stdin: &mut dyn Read, reviewer: bool) -> Result<()> {
    let mut buf = String::new();
    stdin.read_to_string(&mut buf)?;
    let lane = std::env::var_os("DROVER_LANE").or_else(|| std::env::var_os("FLEET_LANE"));
    let Some(lane) = lane.filter(|v| !v.is_empty()).map(|v| v.to_string_lossy().into_owned()) else {
        return Ok(());
    };
    let Ok(input) = serde_json::from_str::<serde_json::Value>(&buf) else {
        return fail_closed(ctx, out, &serde_json::Value::Null, "its input is not JSON");
    };
    let term = std::env::var("SHED").or_else(|_| std::env::var("BERTH"));
    if let (Ok(name), Some(state)) = (term, hook::reported(&input)) {
        let w = r
            .shed()
            .map_or_else(|| crate::shed::Wire::Socket(crate::shed::default_socket()), |b| b.wire);
        let report = shed_proto::Reported {
            state: state.into(),
            ..Default::default()
        };
        if let Err(e) = crate::shed::call(
            r,
            &w,
            shed_proto::Method::State,
            json!(shed_proto::StateParams { name, report }),
        ) {
            eprintln!("drover hook: shed state: {e:#}");
        }
    }
    if let Some(why) = hook::kick_why(&input) {
        let lane = std::env::var("DROVER_LANE").unwrap_or_default();
        let mut it = lane.split(':');
        if let (Some(f), Some(n)) = (it.next(), it.next().and_then(|n| n.parse().ok())) {
            let id = input["tool_use_id"].as_str().filter(|_| why == "push");
            crate::drive::kick_once(ctx, r, (f, n), why, id);
        }
    }
    let builds = || lane_rules(ctx, &lane).is_ok_and(|r| r.builds);
    if let Some(v) = hook::turn_end(&input, &hook::turn_mark(&ctx.state, &lane), builds) {
        writeln!(out, "{v}")?;
        return Ok(());
    }
    let thread = std::env::var("DROVER_THREAD").unwrap_or_default();
    if let Some(text) = hook::issue_filed(&input).filter(|_| !thread.is_empty()) {
        if let Err(e) = Config::load(&ctx.config).and_then(|cfg| crate::orchestrator::flow(ctx, &cfg, &thread, &text)) {
            eprintln!("drover hook: {e:#}");
        }
    }
    if hook::after(&input) {
        return Ok(());
    }
    if input["tool_name"].as_str().is_none_or(str::is_empty) {
        if hook::pre_call(&input) {
            return fail_closed(ctx, out, &input, "its input has no tool name");
        }
        return Ok(());
    }
    let rules = match lane_rules(ctx, &lane) {
        Ok(r) => r,
        Err(cause) => return fail_closed(ctx, out, &input, &cause),
    };
    let mut it = lane.split(':');
    let (fleet, n) = (it.next().unwrap_or_default(), it.next().and_then(|n| n.parse().ok()));
    let cap = rules.builds
        && !n
            .and_then(|n| crate::gh::cached_title(ctx, fleet, n))
            .is_some_and(|t| t.starts_with("cap:"));
    let dir = |p: &PathBuf| p.parent().unwrap_or(p).to_string_lossy().into_owned();
    let h = home();
    let mut paths = vec![
        dir(&ctx.config),
        dir(&ctx.state),
        h.join(".config/fleet-operations").to_string_lossy().into(),
    ];
    paths.extend(
        providers::all()
            .filter_map(|p| Some(p.home()?.default))
            .map(|d| h.join(d).to_string_lossy().into()),
    );
    paths.extend(std::env::var("DROVER_RULES").ok());
    let orchestrator =
        std::env::var("DROVER_LANE").is_ok_and(|l| l.split(':').nth(2).is_some_and(crate::orchestrator::is_role));
    let thread = std::env::var("DROVER_THREAD").unwrap_or_default();
    let cfg = Config::load(&ctx.config)
        .ok()
        .filter(|_| orchestrator && !thread.is_empty() && !thread.contains(['/', '.']));
    let own = cfg.map(|cfg| {
        crate::orchestrator::dir(ctx, &cfg, &thread)
            .to_string_lossy()
            .into_owned()
    });
    let heavy = rules.heavy;
    let policy = hook::Policy {
        branches: rules.branches,
        paths,
        reviewer: reviewer || rules.reviewer,
        deny: rules.deny,
        orchestrator,
        own,
        groups: rules.groups,
        scratch: std::env::var("DROVER_SCRATCH_TARGET").ok().filter(|s| !s.is_empty()),
        cap,
        design: (lane.split(':').nth(2) == Some("design")).then(|| crate::design::scope(ctx, r, &input, fleet, n)),
    };
    let cur = |d: &str| {
        runner::ok(r, "git", &["-C", d, "rev-parse", "--abbrev-ref", "HEAD"])
            .ok()
            .map(|s| s.trim().to_string())
    };
    if let Some(reason) = hook::decide(&input, &policy, &cur) {
        if policy.deny.iter().any(|d| d.reason() == reason) {
            lane_event(ctx, "hook-denied", json!({"rule": reason}));
        }
        writeln!(out, "{}", hook::deny_json(&input, &reason))?;
        return Ok(());
    }
    if let Some(v) = heavy.and_then(|h| hook::heavy(&input, &h, &ctx.exe)) {
        writeln!(out, "{v}")?;
        return Ok(());
    }
    if let Some(dir) = hook::push_dir(&input) {
        coauthor_warn(ctx, r, &dir, "HEAD");
    }
    if let Some(cmd) = hook::fleet_cmd(&input).filter(|_| !thread.is_empty()) {
        let mut d = json!({"thread": thread, "cmd": cmd});
        if let Some(g) = hook::group_of(cmd) {
            d["group"] = json!(g);
        }
        lane_event(ctx, "thread-cmd", d);
    }
    Ok(())
}

fn lane_rules(ctx: &Ctx, lane: &str) -> std::result::Result<hook::Rules, String> {
    if let Some(p) = std::env::var_os("DROVER_RULES") {
        let p = PathBuf::from(p);
        let bad = |e: &dyn std::fmt::Display| format!("rules file {} is unreadable ({e})", p.display());
        let text = std::fs::read_to_string(&p).map_err(|e| bad(&e))?;
        return serde_json::from_str(&text).map_err(|e| bad(&e));
    }
    let cfg = Config::load(&ctx.config).map_err(|e| {
        eprintln!("drover hook: {e:#}");
        "fleet.toml failed to load".to_string()
    })?;
    let mut it = lane.split(':');
    let (fleet, role) = (it.next().unwrap_or_default(), it.nth(1).unwrap_or_default());
    if !cfg.fleet.contains_key(fleet) {
        return Err(format!("fleet `{fleet}` is not in fleet.toml"));
    }
    let host = std::env::var("DROVER_HOST").unwrap_or_default();
    Ok(hook::Rules::resolve(&cfg, fleet, role, &host))
}

fn fail_closed(ctx: &Ctx, out: &mut dyn Write, input: &serde_json::Value, cause: &str) -> Result<()> {
    lane_event(ctx, "hook-failed-closed", json!({"cause": cause}));
    let reason = format!("REFUSED HOOK_FAILED_CLOSED: {cause}; fix it, or ask the owner");
    writeln!(out, "{}", hook::deny_json(input, &reason))?;
    Ok(())
}

fn coauthor_warn(ctx: &Ctx, r: &dyn Runner, dir: &str, rev: &str) {
    let msgs = runner::ok(r, "git", &["-C", dir, "log", rev, "--not", "--remotes", "--format=%B"]);
    let Some(trailer) = msgs.as_deref().ok().and_then(hook::ai_coauthor) else {
        return;
    };
    eprintln!("drover: warning: a pushed commit carries `{trailer}`; lanes add no AI co-author trailers");
    lane_event(ctx, "coauthor-trailer", json!({"line": trailer}));
}

fn lane_event(ctx: &Ctx, kind: &str, mut d: serde_json::Value) {
    if forward(
        &crate::runner::Real,
        &runner::argv(&["lane-event", kind, &d.to_string()]),
    )
    .is_some()
    {
        return;
    }
    let lane = std::env::var("DROVER_LANE").unwrap_or_default();
    let mut it = lane.split(':');
    let (fleet, issue) = (it.next().unwrap_or_default(), it.next().and_then(|n| n.parse().ok()));
    d["lane"] = json!(lane);
    let ev = Event::new(ctx.now, fleet, issue, kind, d);
    let again = |e: &Event| e.kind == ev.kind && e.str("cause") == ev.str("cause") && e.ts + 3600 > ev.ts;
    let quiet = kind == "hook-failed-closed" && crate::events::working(&ctx.state).is_ok_and(|l| l.iter().any(again));
    let _ = match Config::load(&ctx.config) {
        Ok(cfg) if !quiet => emit(&ctx.state, &cfg, ev),
        _ => crate::events::append(&ctx.state, &ev),
    };
}

fn pre_push(ctx: &Ctx, r: &dyn Runner, out: &mut dyn Write, stdin: &mut dyn Read, reviewer: bool) -> Result<i32> {
    let mut buf = String::new();
    stdin.read_to_string(&mut buf)?;
    let lane = std::env::var("DROVER_LANE").unwrap_or_default();
    let rules = lane_rules(ctx, &lane).unwrap_or_else(|_| {
        let (mut it, host) = (lane.split(':'), std::env::var("DROVER_HOST").unwrap_or_default());
        let (fleet, role) = (it.next().unwrap_or_default(), it.nth(1).unwrap_or_default());
        hook::Rules::resolve(&Config::load(&ctx.config).unwrap_or_default(), fleet, role, &host)
    });
    let reviewer = reviewer || rules.reviewer;
    for dst in buf.lines().filter_map(|l| l.split_whitespace().nth(2)) {
        let b = dst.trim_start_matches("refs/heads/");
        if reviewer {
            writeln!(
                out,
                "drover: reviewers are read-only: no `git push` (to `{b}`); write verdict.md and report in your pane"
            )?;
            return Ok(1);
        }
        if rules.branches.iter().any(|x| x == b) {
            writeln!(
                out,
                "drover: pushing to `{b}` is blocked; integration branches change only via `drover merge`"
            )?;
            return Ok(1);
        }
    }
    let deny = rules.deny;
    for sha in buf.lines().filter_map(|l| l.split_whitespace().nth(1)) {
        let specs = ["*.sh", "*.bash", "*.zsh", "*justfile", "*Justfile", "*Makefile"];
        let mut args = vec!["log", "-p", "--format=", sha, "--not", "--remotes", "--"];
        args.extend(specs.iter().chain(&[":!.github/workflows"]));
        let diff = runner::ok(r, "git", &args).unwrap_or_default();
        let mut hit = diff.lines().filter(|l| l.starts_with('+') && !l.starts_with("+++"));
        if let Some(d) = hit.find_map(|l| hook::denied(&l[1..], &deny)) {
            lane_event(ctx, "hook-denied", json!({"rule": d.reason()}));
            writeln!(out, "drover: a pushed script runs `{}`: {}", d.cmd(), d.reason())?;
            return Ok(1);
        }
        coauthor_warn(ctx, r, ".", sha);
    }
    Ok(0)
}

fn places(ctx: &Ctx, cfg: &Config, r: &dyn Runner, events: &[Event]) -> std::collections::BTreeMap<String, String> {
    let (st, list) = (crate::events::fold(events), crate::term::list(r).unwrap_or_default());
    let ls = crate::snapshot::lanes(cfg, r, ctx, (events, &st), &list);
    ls.into_iter().map(|l| (l.name, l.place)).collect()
}
