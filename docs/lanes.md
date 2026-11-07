# Lanes, hooks and notifiers

A lane is one agent working one issue in its own worktree and herdr pane: `impl`, `rev`
or the owner-only `design`. No lane runs the fleet: `herdsman drive` does ([Drive](#drive)). Agents get their usage doc from `herdsman help --agent`.

## Typical flow

```sh
herdsman queue app add 12 13          # ordered queue (mv / rm / block --on N [--on M] / unblock / ls)
herdsman spawn app 12 impl            # worktree + pane + brief + verified kick
herdsman spawn app 12 rev             # after the implementer pushed
herdsman verdict app 12 PASS --sha <head> --file <rev worktree>/verdict.md
herdsman merge app 12                 # exact-head PASS + green CI + round limit
herdsman clean --yes                  # reclaim merged or closed-issue lanes (the tick runs it hourly)
herdsman clean --orphans --yes        # also remove folders under the worktree root of no fleet
```

## Spawn

`herdsman spawn` refuses before creating anything:

- `HOST_BUSY`: memory pressure warn or worse, load per CPU over `[limits]`, or a sustained
  swap-out rate (the median of the last 5 samples) over `[limits]` while pressure is warn or worse;
  one spike admits. `--force` overrides and logs an `override` event.
- `MEM_LOW`: free + inactive memory under `[limits] min_free_mem_gb` (default 2 GB); a lane needs about 0.5–1.5 GB.
- `DISK_LOW`: free disk on `/` under `[limits] min_free_gb` (default 10 GB). `herdsman clean --yes`
  frees the build folders (`target/`, `node_modules/`, `.next/`, `dist/`) of merged or closed lanes,
  even kept ones (never a live lane's, never one git tracks), and reports the bytes freed (approximate: a
  hard-linked file counts once per link).
- `DISK_BUDGET`: worktrees over `[limits] worktree_budget_gb` (default 25 GB) by drive's last walk, after it freed the idlest
  lanes' build folders. A lane's build folders also go at its verdict (reviewer) or PASS (builder).
- `herdsman reclaim` (#361) lists, per worktree idle 7 days, the build folders that would go (`target/`, `node_modules/`,
  `.next/`, `dist/`, `.cache/` and the fleet's `build_dirs`, only git-ignored ones) and their sizes, then asks; `--apply`
  skips the question. Idle means no live lane, no process with its cwd there, HEAD not moved and no changed or
  untracked file modified in 7 days. Sources, branches and worktree registrations stay. Sizes are logical: an
  APFS-cloned folder shares blocks, so freeing it may save less.
- `LANE_LIVE`: a herdr agent is live in the worktree, or the log has an unfinished lane.
- `WORKTREE_DIRTY`: unpushed or uncommitted work from no known lane.
- `SERIAL_BUSY`, `CAP`, `MCP_FORBIDDEN`, `MERGED`: see `herdsman help --agent`.

A fleet's `env` table is exported into every lane's shell. #438: every lane shares one Cargo build folder per
host and repo, `CARGO_TARGET_DIR=<[limits] build_root>/<repo>` (default `~/.cache/herdsman/target/<repo>`; `""`
turns it off), so N lanes cost one `target/` plus deltas: new lanes skip full rebuilds, and lanes building at once
wait on cargo's lock (`Blocking waiting for file lock on build directory`). A fleet's own `CARGO_TARGET_DIR` in `env`
or `shared_cache` (#361) replaces it. Over `[limits] build_budget_gb` (default 20), drive's pass removes the
folder's profiles (`<repo>/debug`, ...) whose `.cargo-lock` no cargo holds, never one building: on the main host
(and in `reclaim`) directly, on each `[hosts]` machine over ssh (there `lsof` tells a building one). A host short
of disk (`DISK_LOW`) first loses every such profile, then its disk is read again. #536: each lane also gets a private scratch
dir, `HERDSMAN_SCRATCH_TARGET=<build_root>/.scratch/<agent>`, for second builds and checkouts (base, staging, head);
the hook refuses a `CARGO_TARGET_DIR` or `--target-dir` outside `build_root` (`REFUSED SCRATCH_TARGET`). Scratch dirs
count toward `build_budget_gb` (oldest go first); drive deletes a lane's once it ended (no herdr agent of its name,
not an open issue's active lane). `herdsman clean` lists the Cargo folders lanes left in `[limits] stray_roots`
(`/tmp`, `$TMPDIR`) untouched a day, and with `--yes` removes them and each removed worktree's Claude temp dir. Node fleets: pnpm's store is
already shared; for npm set `npm_config_cache`. `clone_build = ["target"]` (or `["node_modules"]`) APFS-clones (`cp -cR`) each listed
folder of the local checkout into a new lane (never a reopened one) when its lockfile matches: `Cargo.lock`
for `target`, `package-lock.json` and `pnpm-lock.yaml` for `node_modules`; other names are logged and skipped.
It runs beside the kick once the lane has started, never over a folder the lane already has, and logs
`build-cloned {dirs, secs, errors}`. With no clone available (another volume; Linux without reflinks) it skips
and logs the error, never a full copy. The worktree budget and Host disk row count blocks used (`du -k`), not
apparent size. `clone_node_modules = true` is the old spelling of `clone_build = ["node_modules"]`, kept for
one release.

Every lane gets `HERDSMAN_PORT` (#187): a stable hash of `<fleet>/<branch>` into the fleet's `port_range`
(default `"41000-41999"`), the next free one if a lane of an open issue already holds it; the spawn event
logs it and the Lanes tab shows it (`:41234`). Lanes start dev servers on it, never a fixed port; `env`
values expand it: `[fleet.web.env] PORT = "$HERDSMAN_PORT"`. `scripts/ui-audit.sh` serves on it when set.

`herdsman close <agent>` closes the pane of a herdr agent that is not a herdsman lane (by name or pane id),
never touching files or worktrees, and logs a passive `closed-other`; a logged lane is refused `IS_LANE`
(use stop, kill or clean). `herdsman stop <agent>` sends Esc (logs `stop`). `herdsman kill <agent>` prints what is lost
(state, unpushed commits, dirty files, open PR); with `--yes` it commits dirty files as `wip: kill
snapshot`, pushes the branch to `wip/<agent>-<yyyymmdd-hhmm>` (never main/staging, never force;
refused `SECRET_IN_WIP` when the diff has a secret-looking value), closes the pane and logs
`lane-killed`. The worktree stays; a killed impl goes back
to `queued` (or `fix(n)`), a killed review to the state before it, both re-spawnable.
`herdsman clean <fleet> --lane <agent>` applies `clean`'s safety to one lane and refuses
`UNSAFE_CLEAN` with the reason. #605: a merged or closed issue's lane whose agent is not `working` for
`[limits] idle_merged_minutes` (default 15) has its pane closed by `clean` (and so by drive's pass) the `kill --yes`
way, logged `lane-closed` with `why: merged-idle`, then its worktree removed; the dry run says `would close`. A
`SECRET_IN_WIP` or failed snapshot keeps the pane, one `lane-kept-dirty`, never retried. A working agent, an open
issue's lane, a conductor or any other agent in the folder stays `keep …: live agent`.

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

The hook fails closed (#508): in a lane, a call whose input isn't JSON or names no tool, under a fleet.toml that
doesn't load, or from a lane whose fleet isn't in it is denied `REFUSED HOOK_FAILED_CLOSED: <what failed>; fix it,
or ask the owner`. Each logs `hook-failed-closed` (lane and cause, never the command), alerted once per cause an
hour. Its rules come from the binary and the loaded fleet.toml only, never from a file in the lane's worktree.

### Kicks (#304)

The same hook runs on Claude `PostToolUse` (Bash) and `Stop`, and on Hermes `post_tool_call`: a
`git push` that went through, or a finished turn, runs `herdsman kick <fleet> <issue> --why push|stop`.
So do `herdsman ask`, `verdict` and `answer`, and the board's answer, override, extend, park and queue
moves. A kick returns at once: a detached `kick --now` (output in `<state>/kick.log`) drives that issue's
due steps alone under the drive lock, logs `kick`, and coalesces kicks within 10 s (or while the lock is
busy) into one follow-up. A lane on another host kicks the main host. The tick and fast timers stay the
safety net for CI, outside merges and stalls; the Host tab shows the last kick.

### Denied commands (#216)

The hook also refuses every `deny_commands` entry run as a command word: in a pipeline, after `&&`
or `;`, inside `$( )`, through `sh|bash|zsh -c`, `env`, `sudo`, `exec`, `xargs`, `nice`, `timeout`,
`eval`, `then`/`do`/`{`, past their flags (`sudo -u root`), after `VAR=…`, or by full path
(`/usr/local/bin/docker`). Matching is lexical and best-effort: `D=docker; $D ps`, `ssh h docker ps`
or `open /Applications/Docker.app` pass. An entry is a command word (`"docker"`) or a command plus
subcommand prefix (`"supabase start"`, `"git push --force"`); an entry ending in `=` (`"DOCKER_HOST="`)
refuses that assignment. `{ cmd = "…", why = "…" }` makes `why` the whole message; a bare entry says
``REFUSED NO_COMMAND: lanes never run `<cmd>` ``. The same word as an argument (`grep docker file`) passes.

The default (`[limits] deny_commands`, listed in `assets/deny_commands.toml`) is the owner's
no-containers rule: `docker`, `docker-compose`, `podman`, `nerdctl`, `colima`, `orb`, `orbctl`,
`limactl`, `supabase start`, `supabase db start`, `DOCKER_HOST=…` and `open -a Docker|OrbStack`, each
refused with `REFUSED NO_CONTAINERS: lanes never start containers (owner rule). Use the repo's
non-container test path, or ask the conductor.` `[fleet.<n>] deny_commands` replaces the list (`[]` turns
it off, noted on every config load), `deny_commands_add` extends whichever applies. The board's [config]
sheet shows the active list. Each refusal is logged `hook-denied` (`rule` = the message) and shows on Events.

Per host (#256): spawn exports `HERDSMAN_HOST=<[hosts] name>`, and the hook applies that host's rules on top:
`[hosts.<n>] deny_commands` replaces the fleet's list, `deny_commands_add` extends it, and `allow_commands` drops
entries. A cloud host (`cloud = …`) defaults to allowing `docker`, `docker compose` and `supabase start` (a disposable
machine; `allow_commands = []` keeps the ban); a Mac keeps the ban. `hosts bootstrap` writes that host's rules, its
cloud default spelled out, into the config it leaves there, since the lane's hook reads that host's fleet.toml.

Enforcement by harness: **claude** (PreToolUse) and **hermes** (`pre_tool_call`, the same
`herdsman hook`) refuse the command mechanically. **dsh** has no pre-tool hook: its role prompt states
the rule, and its `pre-push` hook refuses a push whose new commits add a script line (`*.sh`, `*.bash`,
`*.zsh`, `justfile` and `Makefile` lines that start with the command, e.g. recipe lines; not
`package.json` scripts, nor `.github/workflows`, where CI may use Docker) running a denied command. A dsh
lane can still run one directly.

## Drive

Every fleet is driven by herdsman itself. `herdsman drive [fleet]` does one pass, from cron after `herdsman check`
([setup.md](setup.md)) or the board's **Drive now** (paired devices only). Per issue, at most one step a pass, through the same gates as the CLI:

| State | Step |
| --- | --- |
| queued, unblocked, no lane (every such item, in queue order) | `spawn impl` (start) |
| impl/fix lane idle, head pushed, PR open, not reviewed at that head | `spawn rev` (review) |
| `verdict.md` written since the rev spawn, first line `PASS` or `FAIL`, rev spawned at the PR head | `verdict --sha <head>` |
| the same, but the PR head moved since the rev spawn (its logged `sha`) | `spawn rev --supersede` at the new head (review) |
| `ready`, CI green | `merge`; CI pending: wait |
| `ready`, CI red | `verdict FAIL` at the same head (no new round), the failed check's log tail (secrets masked) as the finding (ci-red) |
| `fix(n)` after a FAIL | `spawn impl --supersede` (fix); the brief carries the findings verbatim |
| `ready` or a pushed fix, PR `CONFLICTING` or `BEHIND` the integration branch | `spawn impl --supersede --update` (conflict): the brief says merge `origin/<integration>` (no rebase, no force-push), resolve keeping both sides, run the gates, push, report the resolved files; a fresh review follows; a PASS counts toward `update_rounds`, a FAIL as a normal round; past `update_rounds` it logs `needs-human UPDATE_LIMIT` |
| `FAIL (form-only)`: every `blocking [<category>]` finding in `form_categories` | a fix round that doesn't count toward `review_rounds` (`fix(n·f)`); a blocker untagged or of another category: a plain FAIL (`form-only-rejected`); past `form_rounds` per issue they count (`form-rounds-exhausted`) |
| FAIL in round `review_rounds` | `verdict` logs `needs-human` with the findings; drive stops |
| a lane killed | `spawn impl` (respawn) |
| merged lanes left | `clean --yes` |

**`needs-human` is the owner's** (#130). It pushes to phones (`#96 needs you: review rounds ran out (4/3)`,
then the last verdict's first finding), and the page shows a needs-you card for it on the Queue tab and in the
Herd strip (the issue's horse, or a gate horse when it has no lane, wears a red `!`). A paired phone's buttons:

- **merge anyway** (PASS at the PR head, CI green, mergeable; confirmed) runs
  `herdsman merge <fleet> <issue> --owner-override`. The flag lifts **only** `ROUND_LIMIT`: an exact-head
  PASS, green CI and the serial gate still apply. The `merged` event records `by: owner, override: "ROUND_LIMIT"`
  and the closing comment says `Owner override: ROUND_LIMIT`.
- **one more round** logs `round-extended` (this issue's limit is `review_rounds` + 1 per extension) and
  spawns `impl --supersede`.
- **park** logs `parked`: the issue leaves `needs-human` and shows as parked.

This replaces raising `review_rounds` in fleet.toml, merging and restoring it by hand.

Paused fleets are skipped (no pass, no `last pass`); one fleet's error doesn't skip the others. drive never answers asks (a lane's question stays a needs-you card),
never passes `--force`, and never bypasses a refusal: `REFUSED` (e.g. `HOST_BUSY`, `CAP`,
`SERIAL_BUSY`) is printed and logged once per item an hour (cron stays quiet), and the step is retried
next pass. Starts fan out: a pass tries every startable item in queue order, each spawn sampling the host
afresh. An item-specific refusal (`SERIAL_BUSY`, `MCP_FORBIDDEN`, …) skips that item for the pass; it keeps
its queue place and status reads `skipped this pass: <CODE> (<why>)` (board lane `skip <CODE>`). `CAP` ends
the fleet's starts; `HOST_BUSY` and `ALL_CAPPED` end starting for every later fleet in the pass too (same Mac). Every action logs a passive `driven` event (step, command, result). A pass holds
the `drive.lock` lease (#344: taken over once its pid is gone, or after an hour), so cron and the button never overlap; the
board shows `driver: herdsman · last pass <age> ago`.

A fix lane may push back on a finding in its report with file:line proof; that is not a fix,
and the next (fresh) reviewer decides.

## Asks

A lane that needs a decision runs `herdsman ask <fleet> <issue> "<question>" --option … --recommend <n> --kind routine|owner`
and waits for a `DECISION <id>: <text>` prompt.

- **owner** (the default; an unmarked ask is one) always waits for a person. Scope or feature changes, spend,
  security or secret trade-offs, anything touching main or production, data deletion, a new dependency. The
  phone gets a push for each one.
- **routine** needs `--recommend` (else `NO_RECOMMENDATION`). Fix rounds, ordering, size or cap targets already set
  by an issue, already-decided specs, test or tooling choices inside the issue's scope. If nobody answers within
  `[limits] ask_default_minutes` (30), `herdsman check` takes the recommended option through the same path as
  `herdsman answer`: a `decision` with `by: "default"`, and the lane gets
  `DECISION <id>: <option> (default after 30 min, nobody replied)`. The phone hears of the default taken, never of
  the routine ask merely waiting, and a routine ask raises no `ask-waiting`.
- A routine ask whose question or options use an owner word (`main`, `production`/`prod`, `secret`, `token`,
  `delete`, `spend`, `cost`, `$`, `dependency`, `scope`, or a plural or inflection such as `secrets`, `deleting`) is
  refused `OWNER_TOPIC`. The list is a backstop, not the definition.

The needs-you card tags each ask `routine · defaults to 2 in 24m` or `owner · waits for you`. A defaulted
decision shows on its issue row as `decided by default: <option>` with **[change]**: answering again (also
`herdsman answer`) logs a new `decision` and prompts the lane, or the next lane's brief carries it. A decision
someone made is final (`ALREADY_ANSWERED`).

#584: an ask closes on every real ending, and `herdsman asks --all` says which: `answered` (a tap, `herdsman answer`,
or `answer <fleet> <id> --typed "<line>"`, which a conductor runs with the owner's exact line from the ask's thread,
logged `by: owner (typed)`; any other line is `NOT_TYPED`), `withdrawn → q<new>` (`herdsman ask withdraw`, by its own
thread's conductor), `delivered to t<n>` or `failed` (a handoff card), `expired`, `closed: merged`, `closed: lane
gone`. `herdsman check` expires an ask open over `[limits] ask_expire_hours` (24) that nobody waits on: a thread ask
with no lane, a handoff card, an ask whose lane is no live agent. A live lane's ask never expires; `ask-waiting`
repeats for it once a day. The first `check` after #584 sweeps the log once (`asks-swept`, with the count).

## Check and notifiers

`herdsman check` alerts once per state on `stall`, `lane-dead` (a parked lane, clean and pushed with no
pane, gets one `lane-dead` and never a `stall`), `ready-unmerged`, `ci-missing` (a PASSed PR head with no
checks `[limits] ci_start_minutes` (10) after its push: `CI broken: <workflow> did not start` when a failed run has
no jobs, else `CI missing`; once per head, and drive sends the lane a fix round as for red CI) and
`account-capped`. It is idempotent and exits 0, so it can run from cron:

```cron
*/5 * * * * herdsman check >/dev/null 2>>"$HOME/Library/Logs/herdsman-check.log"
```

Each `[[notify]]` entry gets one plain line on stdin for the events in its `on` list, or
the raw event JSON with `format = "json"`. A fleet's own `[[fleet.<name>.notify]]`
replaces the global list for its events; `notify_global = true` sends to both. Notifiers
run without the caller's `HERMES_*` environment.
