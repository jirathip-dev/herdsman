# Lanes, hooks and notifiers

A lane is one agent working one issue in its own worktree and shed terminal: `impl`, `rev`
or the owner-only `design` and `architect`. No lane runs the fleet: `drover drive` does ([Drive](#drive)). Agents get their usage doc from `drover help --agent`.

## Design and architect (#412, #730)

Two roles run before impl, started by the owner, an orchestrator thread or drive, on the main host only. Neither writes
product code; the owner approves, revises (a note in the next brief) or rejects their pushed work, and impl starts
only after approve.

- `design` (UI, `queue add N --design`): the medium the issue asks for. By default HTML mockups on the repo's own
  design system in `docs/design/<slug>/`; an issue naming SwiftUI views in a design path (`apple/design/<slug>/`) gets
  preview-only views and fixtures there (#868), and the hook keeps the lane's edits to `docs/design/` and those paths.
  A PNG of each page or view at the sizes the issue asks for (default 1440 and 390 wide; iPhone and Mac frames for
  native) and a README mapping each PNG to the decision it shows; at most 3 pages unless the issue asks for variants.
  #873: a repo in another medium sets it once in `.drover/design.md` (`medium:`, `folder:`, `render:`, `sizes:`,
  `clip: yes` for a short gif or mp4 beside the images, `tools:` host tags); the issue's own design path overrides it.
  The lane writes only under that folder, and a tool the main host lacks ends its turn `blocked` before it starts.
- `architect` (`[fleet.<n>.role.architect]`, `queue add N --architect`, `queue set N architect on|off`): in
  `docs/architecture/<slug>/` an ADR (question, options, decision, consequences), the data shapes as types or a
  schema, the module boundaries (which module may call which), a statechart where state matters and the callers'
  usage sketched first. With both marks the architect runs first, then the design, then impl. Every later lane's brief
  links the approved ADR; rev checks the diff against it. A shed lane's role label is `architect`.

## Typical flow

```sh
drover queue app add 12 13          # ordered queue (mv / rm / block --on N [--on M] / unblock / ls)
drover spawn app 12 impl            # worktree + pane + brief + verified kick
drover spawn app 12 rev             # after the implementer pushed
drover verdict app 12 PASS --sha <head> --file <rev worktree>/verdict.md
drover merge app 12                 # exact-head PASS + green CI + round limit
drover clean --yes                  # reclaim merged or closed-issue lanes (the tick runs it hourly)
drover clean --orphans --yes        # also remove folders under the worktree root of no fleet
```

## Spawn

`drover spawn` refuses before creating anything:

- `HOST_BUSY`: memory pressure warn or worse, load per CPU over `[limits]`, or a sustained
  swap-out rate (the median of the last 5 samples) over `[limits]` while pressure is warn or worse;
  one spike admits. `--force` overrides and logs an `override` event with `by` (`owner` or `orchestrator-<id>`);
  only the owner and orchestrators pass it, the lane hook refuses it in any other lane (#908).
- `MEM_LOW`: free + inactive memory under `[limits] min_free_mem_gb` (default 2 GB); a lane needs about 0.5–1.5 GB.
- `DISK_LOW`: free disk on `/` under `[limits] min_free_gb` (default 10 GB). `drover clean --yes`
  frees the build folders (`target/`, `node_modules/`, `.next/`, `dist/`) of merged or closed lanes,
  even kept ones (never a live lane's, never one git tracks), and reports the bytes freed (approximate: a
  hard-linked file counts once per link).
- `DISK_BUDGET`: worktrees over `[limits] worktree_budget_gb` (default 25 GB) by drive's last walk, after it freed the idlest
  lanes' build folders. A lane's build folders also go at its verdict (reviewer) or PASS (builder).
- `drover reclaim` (#361) lists, per worktree idle 7 days, the build folders that would go (`target/`, `node_modules/`,
  `.next/`, `dist/`, `.cache/` and the fleet's `build_dirs`, only git-ignored ones) and their sizes, then asks; `--apply`
  skips the question. Idle means no live lane, no process with its cwd there, HEAD not moved and no changed or
  untracked file modified in 7 days. Sources, branches and worktree registrations stay. Sizes are logical: an
  APFS-cloned folder shares blocks, so freeing it may save less.
- `LANE_LIVE`: a live agent is in the worktree, or the log has an unfinished lane.
- `WORKTREE_DIRTY`: unpushed or uncommitted work from no known lane.
- `SERIAL_BUSY`, `CAP`, `MCP_FORBIDDEN`, `MERGED`: see `drover help --agent`.

A fleet's `env` table is exported into every lane's shell. #630: every lane gets its own Cargo build folder,
`CARGO_TARGET_DIR=<root>/.build/<agent>` under its workspace root (docs/hosts.md). At spawn it is cloned
copy-on-write (`cp -c`, `--reflink=always`; a `build-cloned` event with `from`) from the newest build folder of the
same fleet's lanes on that root, never across volumes, so a new lane skips a full rebuild and lanes never wait on
each other's cargo lock. A fleet's own `CARGO_TARGET_DIR` in `env`
or `shared_cache` (#361) replaces it. Over `[limits] build_budget_gb` (default 20), drive's pass removes the
build folders' profiles (`<agent>/debug`, ...), oldest first, whose `.cargo-lock` no cargo holds, never one
building nor a live lane's: on the main host
(and in `reclaim`) directly, on each `[hosts]` machine over ssh (there `lsof` tells a building one). A host short
of disk (`DISK_LOW`) first loses every such profile, then its disk is read again. #536: each lane also gets a private scratch
dir, `DROVER_SCRATCH_TARGET=<root>/.build/.scratch/<agent>`, for second builds and checkouts (base, staging, head);
the hook refuses a `CARGO_TARGET_DIR` or `--target-dir` outside `<root>/.build` (`REFUSED SCRATCH_TARGET`). Scratch dirs
count toward `build_budget_gb` (oldest go first); drive deletes a lane's once it ended. #651: on every enabled host
(remote ones in the tick, after their trim; `[limits] sweep_remote = false` keeps it to this one) a scratch dir stays only
while its agent is working, it is an open issue's latest lane of its role, or a live agent no spawn names yet holds it.
Under `min_free_gb` every non-working lane's goes, largest first; each removal is logged with host, size and reason. `drover clean` lists the Cargo folders lanes left in `[limits] stray_roots`
(`/tmp`, `$TMPDIR`) untouched a day, and with `--yes` removes them and each removed worktree's Claude temp dir. Node fleets: pnpm's store is
already shared; for npm set `npm_config_cache`. `clone_build = ["target"]` (or `["node_modules"]`) APFS-clones (`cp -cR`) each listed
folder of the local checkout into a new lane (never a reopened one) when its lockfile matches: `Cargo.lock`
for `target`, `package-lock.json` and `pnpm-lock.yaml` for `node_modules`; other names are logged and skipped.
It runs beside the kick once the lane has started, never over a folder the lane already has, and logs
`build-cloned {dirs, secs, errors}`. With no clone available (another volume; Linux without reflinks) it skips
and logs the error, never a full copy. The worktree budget and Host disk row count blocks used (`du -k`), not
apparent size. `clone_node_modules = true` is the old spelling of `clone_build = ["node_modules"]`, kept for
one release.

#630: a repo's optional `.drover/worktree-include` lists ignored files (one path per line, `#` comments), such as
local env templates or tool config, that each new lane's worktree gets copied from the main checkout when missing.
Never secrets: those stay in `secret:` env. `drover status`, the lane sheet and `/api/v1/lanes` (its `unsaved` list:
`root`, `dirty`, `unpushed` per lane) show each lane's unsaved work, `+<uncommitted files> ⇡<unpushed commits>`, before a kill, supersede
or eject.

Every lane gets `DROVER_PORT` (#187): a stable hash of `<fleet>/<branch>` into the fleet's `port_range`
(default `"41000-41999"`), the next free one if a lane of an open issue already holds it; the spawn event
logs it and the Lanes tab shows it (`:41234`). Lanes start dev servers on it, never a fixed port; `env`
values expand it: `[fleet.web.env] PORT = "$DROVER_PORT"`. `scripts/ui-audit.sh` serves on it when set.

`drover close <agent>` closes the terminal of a shed agent that is not a drover lane (by name or pane id),
never touching files or worktrees, and logs a passive `closed-other`; a logged lane is refused `IS_LANE`
(use stop, kill or clean). `drover stop <agent>` sends Esc (logs `stop`). `drover kill <agent>` prints what is lost
(state, unpushed commits, dirty files, open PR); with `--yes` it commits dirty files as `wip: kill
snapshot`, pushes the branch to `wip/<agent>-<yyyymmdd-hhmm>` (never main/staging, never force;
refused `SECRET_IN_WIP` when the diff has a secret-looking value), closes the pane and logs
`lane-killed`. The worktree stays; a killed impl goes back
to `queued` (or `fix(n)`), a killed review to the state before it, both re-spawnable.
#646: every wip snapshot (kill, supersede, clean's auto-save, a cloud stop) stages tracked changes and new source
files only. It always skips `target*/`, `node_modules/` and `.lane-logs/` (whatever `.gitignore` says), any file over
5 MB, and the largest files once the snapshot passes 50 MB. The skipped files are listed on the kill output and in
the wip commit message, never committed. `drover wip prune <fleet> --dry-run` lists origin's `wip/*` branches
with the size each adds over the integration branch, its issue's state and whether it carries build output, after
the counts per issue. Without `--dry-run` (the owner's, through a run card) it deletes on origin those of a merged or
closed issue but the newest per issue and role (#779), and that one too if it carries build output; an open issue's
stay. `drover clean` lists the same, and deletes them with `--yes`: a branch delete, never a history rewrite or force-push. A drover-made clone (`hosts bootstrap`, a lane's
first use of a host) takes the integration branch only, and its later fetches skip `wip/*`.
`drover clean <fleet> --lane <agent>` applies `clean`'s safety to one lane and refuses
`UNSAFE_CLEAN` with the reason. #605: a merged or closed issue's lane whose agent is not `working` for
`[limits] idle_merged_minutes` (default 15) has its pane closed by `clean` (and so by drive's pass) the `kill --yes`
way, logged `lane-closed` with `why: merged-idle`, then its worktree removed; the dry run says `would close`. A
`SECRET_IN_WIP` or failed snapshot keeps the pane, one `lane-kept-dirty`, never retried. A working agent, an open
issue's lane, an orchestrator or any other agent in the folder stays `keep …: live agent`.

`--supersede` kills the old agent and logs it. `drover adopt` records a lane by hand.

Harnesses: `claude` runs Claude Code with the synced profile folder, `hermes` runs a Hermes
profile with its hook accepted, and `dsh` is a reviewer-only beta ([dsh.md](dsh.md)).
`drover set <fleet> <role> --harness <name>` switches one role.

## GitHub stays clean

Issues are the spec, PRs the code. Lanes never comment: the Claude and Hermes hook denies
`gh issue|pr comment` and `gh api` comment writes. Routing, queue, lane and verdict state
live in the event log. `drover merge` writes the one closing comment (merge sha and
verdict sha), then closes the issue's lane panes and reclaims their worktrees; a lane with
uncommitted or unpushed work is kept and reported. Merging into drover's own repo ends by
printing the `cargo install` line to reinstall it (printed, not run).

## The lane hook

`drover profiles sync` installs `drover hook --role <role>` as a PreToolUse hook for
Claude lanes and a `pre_tool_call` hook for Hermes profiles. Spawn exports `DROVER_LANE`
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
`git push` that went through, or a finished turn, runs `drover kick <fleet> <issue> --why push|stop`.
So do `drover ask`, `verdict` and `answer`, and the board's answer, override, extend, park and queue
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
non-container test path, or ask the orchestrator.` `[fleet.<n>] deny_commands` replaces the list (`[]` turns
it off, noted on every config load), `deny_commands_add` extends whichever applies. The board's [config]
sheet shows the active list. Each refusal is logged `hook-denied` (`rule` = the message) and shows on Events.

Per host (#256): spawn exports `DROVER_HOST=<[hosts] name>`, and the hook applies that host's rules on top:
`[hosts.<n>] deny_commands` replaces the fleet's list, `deny_commands_add` extends it, and `allow_commands` drops
entries. A cloud host (`cloud = …`) defaults to allowing `docker`, `docker compose` and `supabase start` (a disposable
machine; `allow_commands = []` keeps the ban); a Mac keeps the ban. `hosts bootstrap` writes that host's rules, its
cloud default spelled out, into the config it leaves there, since the lane's hook reads that host's fleet.toml.

Heavy checks (#909): `[fleet.<n>.heavy]` maps a kind to command patterns (`*` matches any text), e.g.
`swiftlint = ["swiftlint analyze*"]`, `vitest = ["*vitest*"]`, `cargo = ["cargo test*", "cargo check*"]`. A lane's
Bash call with a command matching one (claude only: its hook can rewrite the call) runs under `drover heavy`, which
takes one of the host's slots first and frees it when the command exits or its holder dies, even by SIGKILL.
`[hosts.<n>] heavy_max` (default 3) is the slots every lane on the host shares, whatever its fleet;
`heavy_kind_max = { swiftlint = 1 }` caps a kind lower. A full host keeps the command waiting in line, first come
first served (a kind at its own cap lets later kinds by); after `heavy_wait_minutes` (default 30) it runs anyway
and logs `heavy-timeout`. Each command in line is a file in `<state>/heavy/` naming its `lane`, `kind`, `cmd`,
`since` and `running`. Other commands are unchanged.

Enforcement by harness: **claude** (PreToolUse) and **hermes** (`pre_tool_call`, the same
`drover hook`) refuse the command mechanically. **dsh** has no pre-tool hook: its role prompt states
the rule, and its `pre-push` hook refuses a push whose new commits add a script line (`*.sh`, `*.bash`,
`*.zsh`, `justfile` and `Makefile` lines that start with the command, e.g. recipe lines; not
`package.json` scripts, nor `.github/workflows`, where CI may use Docker) running a denied command. A dsh
lane can still run one directly.

## Drive

Every fleet is driven by drover itself. `drover drive [fleet]` does one pass, from cron after `drover check`
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
| a live lane idle `stall_minutes` with no push, verdict or turn (#666) | `spawn <role> --supersede` once (respawn `lane stalled`); stalled again: `needs-human LANE_STALLED` with its last screen lines |
| a live lane whose pane or session shows no output 10 min after its spawn | an ACP session gets its brief again once; then `spawn <role> --supersede` once (respawn `failed start`); then `needs-human START_FAILED` |
| an ACP lane (build, check or design stage) mid-turn with no session event `silent_minutes` (10) and no tool call still running (#792) | `shed cancel` and one `continue` prompt (`nudged`); still silent 10 min later: `spawn <role> --supersede` once (`respawned-silent`); silent again within the hour: `needs-human LANE_SILENT` |
| a design or architect lane in `error` or gone with no commit of its own (past its spawn, not on integration) under its stage's folders (#915) | `needs-human LANE_FAILED` naming the error; no `design-review`, no ask; a respawn is the owner's `spawn`, under `RESPAWN_LOOP` |
| a dead, stalled, silent or released lane's respawn (#908) | admitted like any spawn: `HOST_BUSY`, `MEM_LOW` and `DISK_LOW` hold it to a later pass, and `waiting` names the hold (`held: <code> <why>`); at most `[limits] respawn_per_pass` (default 2; `respawn_per_pass` in `[fleet.<n>]` overrides it) start a pass, the rest wait `RESPAWN_PACED`, still ahead of new starts |
| a prompt drive or check sends a lane shed has asleep (`hibernated`: nudge, DECISION, turn, release, heads-up) (#908) | waking it starts a whole agent, so the host is admitted first; busy, the prompt is held and sent on a later pass |
| a lane the hook refused twice (`hook-failed-closed`) since its spawn | its pane closed, `needs-human HOOK_REFUSED` with the refusal |
| merged lanes left | `clean --yes` |

**`needs-human` is the owner's** (#130). It pushes to phones (`#96 needs you: review rounds ran out (4/3)`,
then the last verdict's first finding), and the page shows a needs-you card for it on the Queue tab and in the
Herd strip (the issue's horse, or a gate horse when it has no lane, wears a red `!`). A paired phone's buttons:

- **merge anyway** (PASS at the PR head, CI green, mergeable; confirmed) runs
  `drover merge <fleet> <issue> --owner-override`. The flag lifts **only** `ROUND_LIMIT`: an exact-head
  PASS, green CI and the serial gate still apply. The `merged` event records `by: owner, override: "ROUND_LIMIT"`
  and the closing comment says `Owner override: ROUND_LIMIT`.
- **one more round** logs `round-extended` (this issue's limit is `review_rounds` + 1 per extension) and
  spawns `impl --supersede`. #817: an orchestrator grants it in its repo's thread with
  `drover extend <fleet> <issue> --typed "<owner line>"` (`by: owner (typed)`) or `--basis "<spec quote>"`
  (`by: orchestrator-<thread>`, an FYI line whose [change] parks the issue for 30 min).
- **park** logs `parked` (`by`, and `drover park <fleet> <issue> --why <text>`'s reason): the issue leaves
  `needs-human` and shows as parked; `queue ls` and its Queue row show the reason.
- **unpark** (#712; a parked Queue or Lanes row's button, or `drover unpark <fleet> <issue> [--by]`, owner only)
  logs `unparked` with the state it goes back to: the state before the `needs-human` the park closed (normally
  `queued` or its round). Its queue place is kept; out of the queue it goes last; drive's next pass picks it up.
  Anything but a parked issue is `REFUSED NOT_PARKED`.

This replaces raising `review_rounds` in fleet.toml, merging and restoring it by hand.

Paused fleets are skipped (no pass, no `last pass`), but their live lanes still get delivery: the fast step drains their outbox and retypes undelivered answers (#714); one fleet's error doesn't skip the others. drive never answers asks (a lane's question stays a needs-you card),
never passes `--force`, and never bypasses a refusal: `REFUSED` (e.g. `HOST_BUSY`, `CAP`,
`SERIAL_BUSY`) is printed and logged once per item an hour (cron stays quiet), and the step is retried
next pass. Starts fan out: a pass tries every startable item in queue order, each spawn sampling the host
afresh. An item-specific refusal (`SERIAL_BUSY`, `MCP_FORBIDDEN`, …) skips that item for the pass; it keeps
its queue place and status reads `skipped this pass: <CODE> (<why>)` (board lane `skip <CODE>`). `CAP` ends
the fleet's starts; `HOST_BUSY` and `ALL_CAPPED` end starting for every later fleet in the pass too (same Mac). Every action logs a passive `driven` event (step, command, result). A pass holds
the `drive.lock` lease (#344: taken over once its pid is gone, or after an hour), so cron and the button never overlap; the
board shows `driver: drover · last pass <age> ago`.

A fix lane may push back on a finding in its report with file:line proof; that is not a fix,
and the next (fresh) reviewer decides.

## Asks

A lane that needs a decision runs `drover ask <fleet> <issue> "<question>" --option … --recommend <n> --kind routine|owner`
and waits for a `DECISION <id>: <text>` prompt.

- **owner** (the default; an unmarked ask is one) always waits for a person. Scope or feature changes, spend,
  security or secret trade-offs, anything touching main or production, data deletion, a new dependency. The
  phone gets a push for each one.
- **routine** needs `--recommend` (else `NO_RECOMMENDATION`). Fix rounds, ordering, size or cap targets already set
  by an issue, already-decided specs, test or tooling choices inside the issue's scope. If nobody answers within
  `[limits] ask_default_minutes` (30), `drover check` takes the recommended option through the same path as
  `drover answer`: a `decision` with `by: "default"`, and the lane gets
  `DECISION <id>: <option> (default after 30 min, nobody replied)`. The phone hears of the default taken, never of
  the routine ask merely waiting, and a routine ask raises no `ask-waiting`.
- A routine ask whose question or options use an owner word (`main`, `production`/`prod`, `secret`, `token`,
  `delete`, `spend`, `cost`, `$`, `dependency`, `scope`, or a plural or inflection such as `secrets`, `deleting`) is
  refused `OWNER_TOPIC`. The list is a backstop, not the definition.

The needs-you card tags each ask `routine · defaults to 2 in 24m` or `owner · waits for you`. A defaulted
decision shows on its issue row as `decided by default: <option>` with **[change]**: answering again (also
`drover answer`) logs a new `decision` and prompts the lane, or the next lane's brief carries it. A decision
someone made is final (`ALREADY_ANSWERED`).

#584: an ask closes on every real ending, and `drover asks --all` says which: `answered` (a tap, `drover answer`,
or `answer <fleet> <id> --typed "<line>"`, which an orchestrator runs with the owner's exact line from the ask's thread,
logged `by: owner (typed)`; any other line is `NOT_TYPED`; #838: `answer <fleet> <id> <choice> --typed "<line>" --note
"<note>"` names the owner's pick, the line is then evidence only, and an approve's note also goes under the issue's
`## Amendments`; a typed answer is an `amend --basis`), `withdrawn → q<new>` (`drover ask withdraw`, by its own
thread's orchestrator), `delivered to t<n>` or `failed` (a handoff card), `expired`, `closed: merged`, `closed: lane
gone`. `drover check` expires an ask open over `[limits] ask_expire_hours` (24) that nobody waits on: a thread ask
with no lane, a handoff card, an ask whose lane is no live agent. A live lane's ask never expires; `ask-waiting`
repeats for it once a day. The first `check` after #584 sweeps the log once (`asks-swept`, with the count).

## Check and notifiers

#854: every waiting or next-step line drive prints (`merging next pass`, `waiting for drive (next pass ≤ 5 min) to …`,
`waiting for a spawn slot`) is a promise with a deadline: `[limits] promise_passes` drive passes for its kind
(`next-pass`, `merging`, `spawn-pending`, `waiting-on`; default 2 each, `slow_secs` per pass). Each pass logs a new
one as `promise`; one past its deadline whose issue kept its state and head is logged once as `promise-missed` (a
notifier can subscribe to it) with the reason drive sees then, and `waiting`, `status` and the board show
`⚠ missed <age>: <reason>` in place of the stale line.

#855: the fast step also checks two fleet SLOs: `slo-idle` (no lane working for `[limits] slo_idle_minutes` (20)
while the queue has spawnable work) and `slo-tick` (a tick running past `slo_tick_minutes` (10), replacing
`tick-stuck`). On a breach or a missed promise drive first tries the known heal and logs `self-heal`: a drover
tick or drive pid it can name gets SIGTERM, then SIGKILL after 10 s; a DISK_BUDGET hold is recounted and the
queue's first spawn retried; drive's own pass already respawns a silent lane and marks a PASSed draft ready.
Still breached a pass (`slow_secs`) after the heal, or with no heal known, it emits one `stuck` per episode
(pushed, and an owner card naming the issue or fleet, the reason and what was tried); `slo-ok` ends the episode
once the issue moves or the SLO recovers, and the card clears.

`drover check` alerts once per state on `stall`, `lane-dead` (a parked lane, clean and pushed with no
pane, gets one `lane-dead` and never a `stall`), `ready-unmerged`, `ci-missing` (a PASSed PR head with no
checks `[limits] ci_start_minutes` (10) after its push: `CI broken: <workflow> did not start` when a failed run has
no jobs, else `CI missing`; once per head, and drive sends the lane a fix round as for red CI) and
`account-capped`. #640: a limit banner counts only on the harness's own lines (the pane's last block, unless a `⏺`
reply or tool call heads it; prompt lines never), and caps only when the account's usage reading agrees: none
fresh (10 min), or a window the plan has at 100%; otherwise it logs `cap-banner-ignored` with both numbers.
`claude is signed out` (or a 401 usage reading) is `account-signed-out`, an owner card with no reset time: sign
in or replace the token, then `drover accounts reset <name>`. `drover accounts` shows each state's evidence. It is idempotent and exits 0, so it can run from cron:

```cron
*/5 * * * * drover check >/dev/null 2>>"$HOME/Library/Logs/drover-check.log"
```

Each `[[notify]]` entry gets one plain line on stdin for the events in its `on` list, or
the raw event JSON with `format = "json"`. A fleet's own `[[fleet.<name>.notify]]`
replaces the global list for its events; `notify_global = true` sends to both. Notifiers
run without the caller's `HERMES_*` environment.
