# Terminal runtimes: herdr or shed (#298)

A lane is a coding agent running in a terminal. herdsman starts that terminal, types into it, reads its screen and
closes it through one adapter, `crates/core/src/herdr.rs`. There are two runtimes behind it:

- **herdr** (the default): panes in herdr workspaces, as always.
- **shed** (a separate project): a per-user server that keeps
  terminals alive, reads their screens and reports their state.

```toml
[runtime]
kind = "shed"               # "herdr" (default), "shed" or "shed-acp" (ACP lanes, below)
# socket = "~/x/shed.sock"    # default: shed's per-user socket
# acp = "claude-acp"          # ACP mode: run this installed ACP agent instead of the harness's terminal UI
# acp_policy = "ask"          # ACP permission requests: ask (default), allow or deny
```

With no `[runtime]` nothing changes. The kind decides where **new** lanes start. Lanes already running stay where
they are and are still reached: a herdr pane id always has a colon (`w4:p1`) and a shed terminal name never does.
An agent named in a call (`agent prompt`, `agent wait`) goes to shed only if shed runs a terminal of that name.

## How herdsman talks to shed

herdsman depends on `shed-proto`, shed's wire types crate (serde only, no I/O). It is a git dependency pinned to
shed's rename merge commit `1f08d10`; shed has no release tag yet, so switch the pin to a tag once one exists. Each
call is one connection to shed's Unix socket: a `hello` line (product and version must match exactly, else
`SHED_VERSION`), then one JSON-RPC request line, then one answer line. There is no daemon and no async.

On another host (`[hosts.<name>]` with a `machine`, docs/hosts.md) the same lines go through
`ssh <machine> shed connect --autostart`, one ssh call per request, over the host's shared ssh connection.

herdsman keeps speaking in herdr's commands. The shed runtime answers each one the way herdr would, with herdr's
JSON shapes and error codes (`pane_not_found`, `timeout`), so the rest of herdsman needs no change:

| herdsman does          | herdr                        | shed                                                   |
| ---------------------- | ---------------------------- | ------------------------------------------------------- |
| start a lane           | worktree, pane, `agent start` | `git worktree add`, `run` (name = the lane's agent)    |
| read the screen        | `pane read`                  | `read` (`history` + `lines` for the recent window)      |
| type a prompt          | `agent prompt` + Enter       | `send` + `keys enter`                                   |
| press a key            | `pane send-keys`             | `keys` (`Escape` becomes `esc`)                         |
| wait for the agent     | `agent wait --until`         | `wait --until` (reported states)                        |
| wait for screen output | `pane wait-output`           | `wait --output`                                         |
| close a lane           | `pane close`                 | `kill`, then `rm` (so the next lane can use the name)   |
| list lanes             | `agent list`                 | `ls`, keeping running terminals labelled `fleet`        |

A shed lane's terminal is labelled `fleet`, `issue`, `role` and `harness`, kept (`keep`) across a shed server
restart, and runs the user's login shell (`$SHELL -l`, in shed's environment). herdsman then types the lane's
exports and the harness command (`exec <harness> <args>`) into that shell, as it types them into a herdr pane. So no
token is ever in the terminal's command line, in `ps` or in shed's state file. `exec` makes the harness the
terminal's program, so the terminal ends when the harness exits, and the lane drops out of the agent list (lane-dead,
as with herdr).

`herdsman spawn --dry-run` shows the shed requests it would send (`shed <method> <params>`).

## State is reported, not scraped

In a shed terminal `$SHED` names the terminal. The lane's hook (`herdsman hook`, the same one that guards tools)
then reports the turn's state with shed's `state` method:

- `working` when a prompt is submitted or a tool is used (Claude Code `UserPromptSubmit`, `PreToolUse`,
  `PostToolUse`; Hermes `pre_tool_call`, `post_tool_call`);
- `blocked` when the harness waits on a person (Claude Code `Notification`);
- `done` at a turn's end (Claude Code `Stop`; Hermes `on_session_end`).

`herdsman profiles sync` adds the `UserPromptSubmit` and `Notification` hooks to the Claude profiles. A failed report
is printed on stderr and never blocks the tool call. The kick's confirmation (`wait --until working blocked`) and the
board's agent status read these states.

## drive and shed events

Each drive pass (not the fast step) reads shed's `events` stream since the last `seq` it saw (kept in `shed.seq`
beside the event log), stopping once the stream is quiet for 0.3 s. Every lane's `state`, `exited` and `killed`
event is logged as a passive `lane-state` event (`agent`, `state`, `note` or `code`, `shed_seq`); #529: an exit or
kill is `state: gone` (`why`: `exited` or `killed`). A running `herdsman service` follows the stream instead, as it
comes (docs/service.md). Only this host's
server is read. When it does not answer, drive prints `shed events: …` and carries on: the agent list it polls each
pass still covers every lane, here and on other hosts. shed keeps only its last 1,000 events, so a long gap loses
the older ones, and the poll still sees the current state.

## ACP mode

With `acp = "<agent id>"` a lane runs that ACP agent (install it with `shed agents add <id>` on this host) as a
structured session, not a terminal UI. `kind = "shed-acp"` (#565) is the same with `acp = "claude-acp"` and
`acp_policy = "allow"` as defaults. It may be set for one fleet only, so a pilot leaves every other fleet on herdr:

```toml
[fleet.herdsman.runtime]
kind = "shed-acp"
```

A fleet's `[fleet.<n>.runtime]` decides where its new lanes start, over the global `[runtime]`. herdsman reaches
shed (the agent list, drive's event intake) when either names it.

herdsman uses shed's CLI for the two steps shed owns. `shed run --acp <id> --policy <acp_policy> --name … --cwd …
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
lane's exports (`HERDSMAN_LANE`, `HERDSMAN_PORT`, the fleet's `env`, `HERDSMAN_ACCOUNT`). That shell sources the
0600 secrets file `deliver` wrote, so the role's account token reaches the adapter as `CLAUDE_CODE_OAUTH_TOKEN`, the
same way a pane gets it. It also sets `CLAUDE_CONFIG_DIR` to the lane's synced Claude profile
(`~/.config/herdsman/claude/<profile>`) and the role's model as `ANTHROPIC_MODEL`, and unsets `$SHED`. The
claude-acp adapter runs Claude Code with its user, project and local settings, so the profile's `settings.json`
applies:

- its `PreToolUse` hook is `herdsman hook`, the same guard as in a pane, so protected pushes, `gh pr merge`,
  comments, containers, `pkill`/`killall`, protected paths and the reviewer's read-only rule are all refused;
- its `permissions.defaultMode = "bypassPermissions"` is what the pane's `--dangerously-skip-permissions` does, so
  tools do not wait on `shed approve` (the hook still runs first);
- its `CLAUDE.md` is the role's prompt and the usage doc (the pane's `--append-system-prompt-file`).

The lane's state is the session's own: the bridge reports `working`, `blocked` and `done` from the ACP turn, and the
hook does not report it, since `$SHED` is unset. drive logs these as `lane-state` events, and nothing is read off a
screen. The board's lane sheet and `/api/v1` read the lane through shed's `read`, which for an ACP terminal is its
ACP event history (prompts, tool calls with their status, replies), not a screen capture.

**Fail closed (#508).** A lane that herdsman's hook would not guard does not start: spawn, and its dry run, refuse
`ACP_UNGUARDED` before anything is made. That happens when the harness and adapter pair is not one known to run the
hook (today only `claude` with `claude-acp`), when the profile's `settings.json` has no herdsman `PreToolUse` hook
(run `herdsman profiles sync`), when the role has no `[accounts]` token (its config folder is the profile, which holds
no login), or when the lane is placed on another host (`shed run --acp` runs on its own host only). shed's
`--policy` has no per-command rules, so a hook-less adapter has no fallback guard. Not carried over from a pane
yet: the role's `effort` and its `mcp` servers (the bridge's `session/new` passes none).

**Conductors (#608).** A thread whose fleet (for an `all` thread, the fleet whose conductor role it runs on) is on
`shed-acp` starts its conductor the same way: `shed run --acp <acp> --policy <acp_policy> --name conductor-<thread>
--cwd <state dir>/conductor/<thread>/` with the labels `fleet`, `issue=-`, `role=conductor`, `harness`,
`source=drover`, `group=<repo>` and `thread=<thread>`, so cooee lists it. The brief, the kick and the message are its
first prompt; every later line (an owner message, a `DECISION`, a handoff, a system line) goes in through `shed send`,
multi-line as it is. The guard is a lane's: `HERDSMAN_LANE=<fleet>:-:conductor` and `HERDSMAN_THREAD` reach the
profile's hook, so owner-only verbs stay refused. An unguardable setup refuses `ACP_UNGUARDED`, posts the reason in the
thread as a `system` line, and never falls back to herdr. The idle sleep kills and removes the session, and a session
shed no longer runs is asleep on the next `check`. shed's bridge loads no saved session yet (`session/load`), so the
next message starts a fresh one pointed at `.thread.md`. A fleet on herdr keeps herdr conductors.

**Proof (2026-11-06, the mini).** shed 0.0.0 with claude-acp 0.101.0. A session in a scratch repo had herdsman's
hook as its project settings' `PreToolUse` hook and `HERDSMAN_LANE=herdsman:565:impl`. It was asked to run
`git push origin staging`, `pkill -f …` and `gh pr merge 1`. Each tool call ended `failed` with herdsman's refusal
(`pushing to staging is blocked`, `REFUSED NO_BROAD_KILL`, `merging is gated`), and `hook-denied` was logged for
the pkill. A lane's settings come from user settings (`CLAUDE_CONFIG_DIR`) rather than project settings; the
adapter loads both the same way. The tests in `crates/cli/tests/shed.rs` cover spawn's wiring of that environment.

## Setup

1. Install shed on every host that runs lanes, and start its server (`shed serve --detach`, or shed's launchd or
   systemd unit). `shed connect --autostart` starts it on another host when needed.
2. Set `[runtime] kind = "shed"` in fleet.toml on the main host (and on another host's fleet.toml if its lane hooks
   should use a non-default socket; without one, the hook uses shed's default socket).
3. Run `herdsman profiles sync` so the Claude profiles get the state hooks.

## Live trial

The acceptance trial is one herdsman lane on shed end to end (spawn, push, review, merge), run on a real host. Its result goes here.

- Status: not run yet.

## berth, shed's old name (#455)

shed was called berth. For one release herdsman still accepts `kind = "berth"`: it runs shed, prints a one-line
deprecation warning, and `herdsman doctor` flags it. The lane hook reads `$BERTH` when `$SHED` is unset, and drive
reads `berth.seq` once when `shed.seq` is missing.
