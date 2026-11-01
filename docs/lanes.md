# Lanes, hooks and notifiers

A lane is one agent working one issue in its own worktree and herdr pane: `impl`, `rev`,
`orch` (the fleet's orchestrator, `herdsman spawn <fleet> - orch`) or the owner-only
`design`. Agents get their usage doc from `herdsman help --agent`.

## Typical flow

```sh
herdsman queue app add 12 13          # ordered queue (mv / rm / block --on N / ls)
herdsman spawn app 12 impl            # worktree + pane + brief + verified kick
herdsman spawn app 12 rev             # after the implementer pushed
herdsman verdict app 12 PASS --sha <head> --file <rev worktree>/verdict.md
herdsman merge app 12                 # exact-head PASS + green CI + round limit
herdsman clean --yes                  # reclaim merged or closed-issue lanes
```

## Spawn

`herdsman spawn` refuses before creating anything:

- `HOST_BUSY`: memory pressure warn or worse, swap-out rate or load per CPU over
  `[limits]`. `--force` overrides and logs an `override` event.
- `LANE_LIVE`: a herdr agent is live in the worktree, or the log has an unfinished lane.
- `WORKTREE_DIRTY`: unpushed or uncommitted work from no known lane.
- `SERIAL_BUSY`, `CAP`, `MCP_FORBIDDEN`, `MERGED`: see `herdsman help --agent`.

`herdsman close <agent>` closes the pane of a herdr agent that is not a herdsman lane (by name or pane id),
never touching files or worktrees, and logs a passive `closed-other`; a logged lane is refused `IS_LANE`
(use stop, kill or clean). `herdsman stop <agent>` sends Esc (logs `stop`). `herdsman kill <agent>` prints what is lost
(state, unpushed commits, dirty files, open PR); with `--yes` it commits dirty files as `wip: kill
snapshot`, pushes the branch to `wip/<agent>-<yyyymmdd-hhmm>` (never main/staging, never force;
refused `SECRET_IN_WIP` when the diff has a secret-looking value), closes the pane and logs
`lane-killed`. The worktree stays (an orch's checkout is never snapshotted); a killed impl goes back
to `queued` (or `fix(n)`), a killed review to the state before it, both re-spawnable.
`herdsman clean <fleet> --lane <agent>` applies `clean`'s safety to one lane and refuses
`UNSAFE_CLEAN` with the reason.

`--supersede` kills the old agent and logs it. If `herdr agent start` times out but the
harness came up in the pane, spawn names the agent and logs the lane anyway; if nothing
came up it closes the pane and logs nothing. `herdsman adopt` records a lane by hand.

Harnesses: `claude` runs Claude Code with the synced profile folder, `hermes` runs a Hermes
profile with its hook accepted, and `dsh` is a reviewer-only beta ([dsh.md](dsh.md)).
`herdsman set <fleet> <role> --harness <name>` switches one role.

## GitHub stays clean

Issues are the spec, PRs the code. Lanes never comment: the Claude and Hermes hook denies
`gh issue|pr comment` and `gh api` comment writes. Routing, queue, lane and verdict state
live in the event log. `herdsman merge` writes the one closing comment (merge sha and
verdict sha), then closes the issue's lane panes and reclaims their worktrees; a lane with
uncommitted or unpushed work is kept and reported. Merging into herdsman's own repo ends by
printing the `cargo install` line to reinstall it (printed, not run).

## The lane hook

`herdsman profiles sync` installs `herdsman hook --role <role>` as a PreToolUse hook for
Claude lanes and a `pre_tool_call` hook for Hermes profiles. Spawn exports `HERDSMAN_LANE`
(`<fleet>:<issue>:<role>`) in the lane shell, which arms the hook. It blocks `gh pr merge`,
issue and PR comments, `gh pr review`, `git push` to main, master, staging or the
integration branch, and edits to fleet and Hermes profile config. Reviewer lanes are
read-only: no edits except `verdict.md`, no `git commit|push|merge|rebase|reset`.

## Drive

A fleet with `driver = "herdsman"` (default `"orch"`) has no orchestrator lane: `herdsman drive [fleet]`
does one pass, from cron after `herdsman check` ([setup.md](setup.md)) or the board's **Drive now**
(paired devices only). Per issue, at most one step a pass, through the same gates as the CLI:

| State | Step |
| --- | --- |
| queued, unblocked, no lane (every such item, in queue order) | `spawn impl` (start) |
| impl/fix lane idle, head pushed, PR open, not reviewed at that head | `spawn rev` (review) |
| `verdict.md` written since the rev spawn, first line `PASS` or `FAIL`, rev spawned at the PR head | `verdict --sha <head>` |
| the same, but the PR head moved since the rev spawn (its logged `sha`) | `spawn rev --supersede` at the new head (review) |
| `ready`, CI green | `merge`; CI pending: wait |
| `ready`, CI red | `verdict FAIL` at the same head (no new round), the failed check's log tail (secrets masked) as the finding (ci-red) |
| `fix(n)` after a FAIL | `spawn impl --supersede` (fix); the brief carries the findings verbatim |
| `ready` or a pushed fix, PR `CONFLICTING` or `BEHIND` the integration branch | `spawn impl --supersede --update` (conflict): the brief says merge `origin/<integration>` (no rebase, no force-push), resolve keeping both sides, run the gates, push, report the resolved files; a fix round, so a fresh review follows; past `review_rounds` it logs `needs-human` |
| FAIL in round `review_rounds` | `verdict` logs `needs-human` with the findings; drive stops |
| a lane killed | `spawn impl` (respawn) |
| merged lanes left | `clean --yes` |

Paused fleets are skipped (no pass, no `last pass`); one fleet's error doesn't skip the others. drive never answers asks (a lane's question stays a needs-you card),
never passes `--force`, and never bypasses a refusal: `REFUSED` (e.g. `HOST_BUSY`, `CAP`,
`SERIAL_BUSY`) is printed and logged once per item an hour (cron stays quiet), and the step is retried
next pass. Starts fan out: a pass tries every startable item in queue order, each spawn sampling the host
afresh. An item-specific refusal (`SERIAL_BUSY`, `MCP_FORBIDDEN`, …) skips that item for the pass; it keeps
its queue place and status reads `skipped this pass: <CODE> (<why>)` (board lane `skip <CODE>`). `CAP` ends
the fleet's starts; `HOST_BUSY` ends starting for every later fleet in the pass too (same Mac). Every action logs a passive `driven` event (step, command, result). A pass holds
`<state dir>/drive.lock` (taken over after 30 min), so cron and the button never overlap; the
board shows `driver: herdsman · last pass <age> ago`.

A fix lane may push back on a finding in its report with file:line proof; that is not a fix,
and the next (fresh) reviewer decides.

## Check and notifiers

`herdsman check` alerts once per state on `stall`, `lane-dead` (a parked lane, clean and pushed with no
pane, gets one `lane-dead` and never a `stall`), `ready-unmerged` and
`account-capped`. It is idempotent and exits 0, so it can run from cron:

```cron
*/5 * * * * herdsman check >/dev/null 2>>"$HOME/Library/Logs/herdsman-check.log"
```

Each `[[notify]]` entry gets one plain line on stdin for the events in its `on` list, or
the raw event JSON with `format = "json"`. A fleet's own `[[fleet.<name>.notify]]`
replaces the global list for its events; `notify_global = true` sends to both. Notifiers
run without the caller's `HERMES_*` environment.
