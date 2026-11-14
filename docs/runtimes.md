# Terminal runtimes: shed (#298, #721)

A lane is a coding agent running in a terminal. drover starts that terminal, types into it, reads its screen and
closes it through one adapter, `crates/core/src/term.rs`, on **shed** (a separate project): a per-user server that
keeps terminals alive, reads their screens and reports their state. shed is the only runtime (#721).

```toml
[runtime]
kind = "shed"               # "shed" (a shed terminal per lane) or "shed-acp" (ACP lanes, below)
# socket = "~/x/shed.sock"    # default: shed's per-user socket
# acp = "claude-acp"          # ACP mode: run this installed ACP agent instead of the harness's terminal UI
# acp_policy = "ask"          # ACP permission requests: ask (default), allow or deny
```

The kind decides where **new** lanes start. With no `[runtime]` (and no fleet or host runtime) there is none: spawn
and the orchestrator refuse with "no shed runtime".

**Migrating from herdr (#721).** `kind = "herdr"` (in `[runtime]`, a `[fleet.<n>.runtime]` or a host's `runtime`)
no longer loads: fleet.toml fails with a message naming `shed-acp`. Set `kind = "shed-acp"` (or `"shed"`) there, or
`runtime = "shed-acp"` on a `[hosts.<name>]`; let live herdr lanes finish (or merge) first, since drover no longer
reaches them.

## How drover talks to shed

drover depends on `shed-proto`, shed's wire types crate (serde only, no I/O). It is a git dependency pinned to
shed's rename merge commit `1f08d10`; shed has no release tag yet, so switch the pin to a tag once one exists. Each
call is one connection to shed's Unix socket: a `hello` line (product and version must match exactly, else
`SHED_VERSION`), then one JSON-RPC request line, then one answer line. There is no daemon and no async.

On another host (`[hosts.<name>]` with a `machine`, docs/hosts.md) the same lines go through
`ssh <machine> shed connect --autostart`, one ssh call per request, over the host's shared ssh connection.

drover speaks in pane words (`term.rs`); shed answers each with JSON shapes and error codes (`pane_not_found`,
`timeout`):

| drover does            | shed                                                   |
| ---------------------- | ------------------------------------------------------- |
| start a lane           | `git worktree add`, `run` (name = the lane's agent)    |
| read the screen        | `read` (`history` + `lines` for the recent window)      |
| type a prompt          | `send` + `keys enter`                                   |
| press a key            | `keys` (`Escape` becomes `esc`)                         |
| wait for the agent     | `wait --until` (reported states)                        |
| wait for screen output | `wait --output`                                         |
| close a lane           | `kill`, then `rm` (so the next lane can use the name)   |
| list lanes             | `ls`, keeping running terminals labelled `fleet`        |

A shed lane's terminal is labelled `fleet`, `issue`, `role` and `harness`, kept (`keep`) across a shed server
restart, and runs the user's login shell (`$SHELL -l`, in shed's environment). drover then types the lane's
exports and the harness command (`exec <harness> <args>`) into that shell. So no
token is ever in the terminal's command line, in `ps` or in shed's state file. `exec` makes the harness the
terminal's program, so the terminal ends when the harness exits, and the lane drops out of the agent list (lane-dead).

`drover spawn --dry-run` shows the shed requests it would send (`shed <method> <params>`).

## State is reported, not scraped

In a shed terminal `$SHED` names the terminal. The lane's hook (`drover hook`, the same one that guards tools)
then reports the turn's state with shed's `state` method:

- `working` when a prompt is submitted or a tool is used (Claude Code `UserPromptSubmit`, `PreToolUse`,
  `PostToolUse`; Hermes `pre_tool_call`, `post_tool_call`);
- `blocked` when the harness waits on a person (Claude Code `Notification`);
- `done` at a turn's end (Claude Code `Stop`; Hermes `on_session_end`).

`drover profiles sync` adds the `UserPromptSubmit` and `Notification` hooks to the Claude profiles. A failed report
is printed on stderr and never blocks the tool call. The kick's confirmation (`wait --until working blocked`) and the
board's agent status read these states.

## drive and shed events

Each drive pass (not the fast step) reads shed's `events` stream since the last `seq` it saw (kept in `shed.seq`
beside the event log), stopping once the stream is quiet for 0.3 s. Every lane's `state`, `exited` and `killed`
event is logged as a passive `lane-state` event (`agent`, `state`, `note` or `code`, `shed_seq`); #529: an exit or
kill is `state: gone` (`why`: `exited` or `killed`). A running `drover service` follows the stream instead, as it
comes (docs/service.md). Only this host's
server is read. When it does not answer, drive prints `shed events: …` and carries on: the agent list it polls each
pass still covers every lane, here and on other hosts. shed keeps only its last 1,000 events, so a long gap loses
the older ones, and the poll still sees the current state.

## ACP mode

With `acp = "<agent id>"` a lane runs that ACP agent (install it with `shed agents add <id>` on this host) as a
structured session, not a terminal UI. `kind = "shed-acp"` (#565) is the same with `acp = "claude-acp"` and
`acp_policy = "allow"` as defaults. It may be set for one fleet only:

```toml
[fleet.drover.runtime]
kind = "shed-acp"
```

A fleet's `[fleet.<n>.runtime]` decides where its new lanes start, over the global `[runtime]`. drover reaches
shed (the agent list, drive's event intake) when either names it.

drover uses shed's CLI for the two steps shed owns. `shed run --acp <id> --policy <acp_policy> --name … --cwd …
--label … -- <adapter>` builds the agent's bridge, and `shed send` frames each prompt. The kick waits for `ACP session
ready`, sends the brief's text as the session's first prompt, and waits for `working`, `blocked` or `done`. Later
prompts (a DECISION line, a nudge, a reply from the board) go through `shed send` too, because the terminal itself is
an ACP one. No Enter is pressed after a prompt. Everything else (read, wait, kill, list) is the same socket call as
above.

**Labels.** An ACP lane has the shed lane labels (`fleet`, `issue`, `role`, `harness`) and the generic ones cooee's
Workflow list reads: `source=drover`, `group=<repo name>`, `item=#<issue>`, `role` (`impl`, `fix` for a fix round,
`rev`, `design`), `round=<n>` and `url=<issue URL>`. It also has `drover.fleet`, `drover.issue`, `drover.role` and
`drover.round`. No token is ever in a label, an event, the brief or the adapter's command line.

**The guard.** The adapter after `--` is the installed agent's command (`shed agents --json`), run by `sh` after the
lane's exports (`DROVER_LANE`, `DROVER_PORT`, the fleet's `env`, `DROVER_ACCOUNT`, and #683 the role's
account env, `account_env`'s or its folder as `CLAUDE_CONFIG_DIR`, so the adapter signs in with that login and gets no
token; a role with no account signs in with the host's own login, an inherited token unset, and only a lane on another
host still needs an account). It
sets the role's model as `ANTHROPIC_MODEL`, copies the lane's synced profile (`~/.config/drover/claude/<profile>`)
into the worktree as `.claude/settings.local.json` and `CLAUDE.local.md`, and unsets `$SHED`. The claude-acp adapter
runs Claude Code with its user, project and local settings, so the profile's `settings.json` applies:

- its `PreToolUse` hook is `drover hook`, the same guard as in a pane, so protected pushes, `gh pr merge`,
  comments, containers, `pkill`/`killall`, protected paths and the reviewer's read-only rule are all refused;
- its `permissions.defaultMode = "bypassPermissions"` is what the pane's `--dangerously-skip-permissions` does, so
  tools do not wait on `shed approve` (the hook still runs first);
- its `role.md`, as `CLAUDE.local.md`, is the role's prompt and the usage doc (the pane's `--append-system-prompt-file`).

The lane's state is the session's own: the bridge reports `working`, `blocked` and `done` from the ACP turn, and the
hook does not report it, since `$SHED` is unset. drive logs these as `lane-state` events, and nothing is read off a
screen. The board's lane sheet and `/api/v1` read the lane through shed's `read`, which for an ACP terminal is its
ACP event history (prompts, tool calls with their status, replies), not a screen capture.

**Fail closed (#508).** A lane that drover's hook would not guard does not start: spawn, and its dry run, refuse
`ACP_UNGUARDED` before anything is made. That happens when the harness and adapter pair is not one known to run the
hook (today only `claude` with `claude-acp`), when the profile's `settings.json` has no drover `PreToolUse` hook
(run `drover profiles sync`), or when the role has no `[accounts]` token (its config folder is the profile, which
holds no login). #637: on another host an ACP lane runs only on a `runtime = "shed-acp"` host, where its guard is
proven, and it signs in through the token proxy, never with a token there (docs/hosts.md). shed's
`--policy` has no per-command rules, so a hook-less adapter has no fallback guard. Not carried over from a pane
yet: the role's `effort` and its `mcp` servers (the bridge's `session/new` passes none).

**Agent and provider per role (#732).** A role may name its own ACP agent over the runtime's `acp`, with its `model`
and `effort`; `harness` then follows from the agent (`claude-acp` is `claude`, `codex-acp` is `codex`):

```toml
[fleet.drover.role.rev]
agent = "codex-acp"
account = "X"        # an [accounts] folder holding Codex's own login (`CODEX_HOME`)
model = "gpt-5.5"
effort = "high"
```

Each agent's provider is known (`claude-acp` anthropic, `codex-acp` openai, `gemini-acp` google). Once a fleet's
`impl` or `rev` names an `agent`, its config refuses to load (`REVIEW_SAME_PROVIDER`) while `rev` runs on `impl`'s
provider (a role without `agent` counts as the runtime's), unless the fleet sets `rev_same_provider = true # <why>`;
the reason comment on that line is required. The verify role may share either. A fleet naming no `agent` loads as
before.

A codex lane signs in with its account's env (`account_env`'s with `{provider}` = `codex`, else its folder as
`CODEX_HOME`). Its guard is the same `drover hook`, given to codex-acp in its session config (`CODEX_CONFIG`, from
the profile's `codex-config.json`) with Codex's hook-trust review bypassed, since a lane has no one to answer it;
Codex hooks speak Claude Code's protocol, and an `apply_patch` edit is judged on its patch text. The role prompt goes
first in `AGENTS.override.md`, ahead of the repo's own `AGENTS.md`. `drover config apply` runs `profiles sync`
itself, so a role it adds is guarded (and a Hermes profile's hook approved) before doctor looks.

**Orchestrators (#608).** A thread whose fleet (for an `all` thread, the fleet whose orchestrator role it runs on) is on
`shed-acp` starts its orchestrator the same way: `shed run --acp <acp> --policy <acp_policy> --name orchestrator-<thread>
--cwd <state dir>/orchestrator/<thread>/` with the labels `fleet`, `issue=-`, `role=orchestrator`, `harness`,
`source=drover`, `group=<repo>` and `thread=<thread>`, so cooee lists it. The brief, the kick and the message are its
first prompt; every later line (an owner message, a `DECISION`, a handoff, a system line) goes in through `shed send`,
multi-line as it is. The guard is a lane's: `DROVER_LANE=<fleet>:-:orchestrator` and `DROVER_THREAD` reach the
profile's hook, so owner-only verbs stay refused. An unguardable setup refuses `ACP_UNGUARDED`, posts the reason in the
thread as a `system` line, and never falls back to a shed terminal. The idle sleep kills and removes the session, and a session
shed no longer runs is asleep on the next `check`. shed's bridge loads no saved session yet (`session/load`), so the
next message starts a fresh one pointed at `.thread.md`. A `shed` fleet's orchestrator is a shed terminal (#721).

**shed on the timer's PATH (#600).** drive spawns with the timer jobs' PATH, not your shell's. `drover timer
install` puts the folder of the `shed` your shell resolves on it, `drover doctor` checks `shed` and each ACP
adapter's command there, and spawn refuses `SHED_MISSING` (with the PATH it searched) before anything is made.

**Proof (2026-11-06, the mini).** shed 0.0.0 with claude-acp 0.101.0. A session in a scratch repo had drover's
hook as its project settings' `PreToolUse` hook and `DROVER_LANE=drover:565:impl`. It was asked to run
`git push origin staging`, `pkill -f …` and `gh pr merge 1`. Each tool call ended `failed` with drover's refusal
(`pushing to staging is blocked`, `REFUSED NO_BROAD_KILL`, `merging is gated`), and `hook-denied` was logged for
the pkill. A lane's settings come from user settings (`CLAUDE_CONFIG_DIR`) rather than project settings; the
adapter loads both the same way. The tests in `crates/cli/tests/shed.rs` cover spawn's wiring of that environment.

## Setup

1. Install shed on every host that runs lanes, and start its server (`shed serve --detach`, or shed's launchd or
   systemd unit). `shed connect --autostart` starts it on another host when needed.
2. Set `[runtime] kind = "shed"` in fleet.toml on the main host (and on another host's fleet.toml if its lane hooks
   should use a non-default socket; without one, the hook uses shed's default socket).
3. Run `drover profiles sync` so the Claude profiles get the state hooks.

## Live trial

The acceptance trial is one drover lane on shed end to end (spawn, push, review, merge), run on a real host. Its result goes here.

- Status: not run yet.

## berth, shed's old name (#455)

shed was called berth. For one release drover still accepts `kind = "berth"`: it runs shed, prints a one-line
deprecation warning, and `drover doctor` flags it. The lane hook reads `$BERTH` when `$SHED` is unset, and drive
reads `berth.seq` once when `shed.seq` is missing.
