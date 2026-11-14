# Hosts: lanes on more than one machine (#157)

One **main host** runs drover, the board (`drover serve`) and the event log. It starts lanes on any host over ssh
(shed there through `shed connect`), picks the least-busy host that fits, and shows every host on the board. With no `[hosts]` in
fleet.toml nothing changes: every lane runs on the main host, as before.

## Add a host

1. Make the machine an SSH host name, e.g. an entry in `~/.ssh/config`. drover reaches the host as `ssh <name> …`
   for everything (git, probes, file reads and writes, and shed through `shed connect`). Key-based login only, because drover runs
   ssh with `BatchMode=yes` (#337: and `ControlMaster=auto`, one connection kept 10 minutes). The board's reads of a
   host (#337) are one ssh call per step. A pass checks all of a host's
   lane worktrees with one ssh call (#880); a host whose call runs past its limit shows in the tick's log as
   `tick: <host>: over budget; skipped for the rest of this pass`, and the pass goes on for the other hosts.
2. On the new host, install and log in to:
   - shed (its server job running; `hosts bootstrap` installs it);
   - git;
   - `gh` (`gh auth login`): lanes there open their PRs with it;
   - git access to each fleet repo: the host clones each repo itself the first time a lane goes there, with plain
     `git clone https://github.com/<repo>.git <local>`, so its git config (`insteadOf`, credential helper) applies;
   - the agent harness (Claude Code logged in);
   - drover itself. The lane's tool-guard hook calls `drover hook`, and the Claude profile lives under that host's
     own home: each spawn there syncs its lane's profile (`profiles sync --profile <p> --role <r>`, #592).

   drover never copies credentials to another host: a role's `account` token is only exported to lanes on the
   main host; a lane on another host runs on that host's own Claude login.
3. Add the host to fleet.toml:

```toml
[hosts.mini]            # the main host: no `machine`
tags = ["fast"]

[hosts.bazzite]
machine = "bazzite"     # its SSH name
tags = ["gpu"]          # hand tags, added to the detected ones
root = "~/lanes"        # workspace root on that host (default ~/.local/share/drover/work)
enabled = true          # false: no new lanes go there (see below)
```

## Workspace roots (#630)

Everything drover makes on a host lives under its workspace root: worktrees at `<root>/<repo>/<lane>`, the
each lane's Cargo build at `<root>/.build/<agent>` (cloned from its fleet's warmest one on the same root) and its
scratch under `<root>/.build/.scratch`.

`roots = ["/Volumes/NVMe/drover", "~/.local/share/drover/work"]` lists several in order (an external drive
first): a new lane takes the first that is mounted, writable and has `min_free_gb`; its spawn logs that `root`, and
an issue's later lanes stay on it while it can take them. `drover hosts eject <host> [<root>]` (default the first)
takes the root out at once: each lane there is saved (`wip/<agent>-<stamp>` for anything unpushed or uncommitted),
its worktree removed, the live one restarted on the next root from that save; then it prints
`safe to unplug <root>` (exit 1 while an agent still works there). A root pulled without eject is noted once by
`check` (`! root <path> gone`): its live lane restarts on the next root from its branch, and the issue's waiting line
says what was lost. Mounted again, the root takes new lanes; nothing moves back on its own. `drover status`, the
lane sheet and each host card (with `/api/v1`'s host data) show the roots, their state (`active`, `ejecting`,
`gone`) and free space.

A fleet's `local` that starts with `~/` points to that path under each host's own home. An absolute `local` is used
as written on every host.

## Tags

Each `drover check` (and `drover hosts refresh`) looks at every host. Once an hour it probes the host's tags with
one shell line: `os:macos` or `os:linux`, `arch:arm64` or `arch:x86_64`, `xcode` (when `xcodebuild -version` works) and
`docker` (when `docker info` works). The results go to `<state dir>/hosts.json`, along with the host's `$HOME`,
whether it answered, and its load/cpu, free memory and free disk. A host's tags are the detected ones plus its hand
`tags`.

## Needs

A lane needs the union of three lists:

- the fleet's `needs` (`[fleet.<n>] needs = ["xcode"]`);
- the role's `needs` (`[fleet.<n>.role.design] needs = ["os:macos"]`);
- the queue item's needs (`drover queue <fleet> add 12 --needs docker,gpu` adds to them;
  `drover queue <fleet> set 12 needs docker` replaces them; `queue ls` shows them).

## Placement

`drover spawn` picks the host like this:

1. `--host <name>` forces that host.
2. Otherwise, a role pinned with `[fleet.<n>.role.<r>] host = "main"` (or a host name) goes there. `main` is the host
   without `machine`. A `design` lane always runs on the main host: it defaults to `main`,
   and `--host` or a pin naming another host is refused `DESIGN_MAIN_ONLY`.
3. Otherwise, an issue that already had a lane stays on that lane's host, because its worktree is there. This holds
   even when the host has been turned off.
4. Otherwise, drover takes the enabled hosts whose tags cover the needs, and picks the least busy: lowest load per
   CPU first, then the most free memory.

Admission runs per host: `DISK_LOW`, `MEM_LOW`, then `HOST_BUSY`. A host that fails admission (or is unreachable) is
skipped. A `DISK_LOW` names the done lanes' worktrees `drover clean --yes` would remove on that host (or says there
are none and points to `drover reclaim`), and a spawn still starting toward a full host holds no `max_parallel_spawns`
slot, so it never holds back spawns bound for another host (#416). `drover clean` covers every host's merged or
closed lanes (shown as `<host>:<path>`) and lists the orphan folders under each enabled host's worktree root;
`--orphans --yes` removes them. When every fitting host fails, the spawn is refused with the least-busy host's reason. The load in `HOST_BUSY` leaves out
drover's own processes and the `cp` clones they run (#825), and a refusal on load names the 3 busiest other processes
(`top: rustc 900%, …`). A tapped start held by load alone waits and starts on the first pass the host admits it; its
card never comes back for the load. When no host fits at
all (or only a cloud host over its budget fits), it is refused with `REFUSED NO_HOST: no host with docker`, plus
` (cloud host stopped or over budget)` when fleet.toml has a cloud host; the Host tab reads it as `waiting`. drive
retries on its next pass.

### Working-lane and simulator slots (#984)

`[hosts.<name>] max_working = 12` caps the lanes running a turn on that host, across all fleets; `max_simulators = 1`
caps the live lanes that need the `sim` tag (by fleet, role or `queue add --needs sim`). Unset means no cap. A lane
idle at its prompt, or whose issue is `ready`, `needs-human`, `parked`, `blocked` or done, holds no working slot; a
`sim` lane holds its simulator slot while its pane lives (unless its issue is in one of those states). Over a cap a
start is not refused for good: drive skips it with `HOST_FULL` (`waiting for a slot: 12/12 working on mini`), counting
spawns still starting, and starts it in queue order once a slot frees; a hand-typed `drover spawn` is not held. `drover status` prints `mini: working N/12 ·
simulators N/1` for each capped host. Once its verdict is in, a reviewer's idle pane closes on the next `check`.
Likewise, a builder's idle pane closes once a review started after it. Their worktrees stay until merge.

The `spawn` event records the lane's `host` and `machine`. From then on, every shed call for that lane goes to
that host's shed (pane reads, prompts, stop, kill, close), and its worktree work runs over ssh on that host:
`git worktree`, the brief and MCP files, the reviewer's `verdict.md`, the dirty and unpushed checks, and `clean`.

`spawn --dry-run` places the lane first and prints the commands exactly as they are sent: `--machine`, that host's
paths and home, and no account token.

## A remote lane's hook rules (#592)

Only the main host has a fleet.toml. Spawn resolves a remote lane's hook rules there (protected branches, the fleet's
`deny_commands` under the host's `[hosts.<name>]` rules, the reviewer flag) and writes them to
`<worktree>.rules.json` on the lane's host, exported as `DROVER_RULES`, and syncs the lane's harness profile there
by name. The hook there reads only that file (the lane
may not edit it); a missing or unreadable one fails closed `HOOK_FAILED_CLOSED`. It holds no notifier, token or env
value. The rules are frozen at spawn: after a config change that alters them, the board marks the lane `rules stale`
until a respawn. A fleet.toml left on a remote host is no longer read: `drover doctor` shows
`! stale fleet.toml on <host>: no longer read`; remove it by hand. #647: bootstrap removes one an old bootstrap wrote
(only `[fleet.*]` and `[hosts.<host>]`); doctor's host row names the failed check (`unreachable (ssh: …)`,
`! shed: …`).

## Asks, answers and verdicts from another host (#275, #389)

A lane on another host never connects to the main host: it holds no key, token or ssh route to it. Spawn exports
`DROVER_OUTBOX=1`, and its `drover ask`, `answer`, `verdict`, `done` and its own events (`hook-denied`,
`coauthor-trailer`) are appended to `<worktree>/.drover/outbox.jsonl` (exit 0: `queued for the main host: drive
delivers it within 1 min`). Each fast drive pass pulls every live remote lane's outbox over main → host ssh and runs
each line as that lane (from its spawn event, never the line), once per lane and timestamp; a line for another issue
or role, or a verb a lane doesn't run itself, is refused and logged `outbox`. `check` raises `outbox-stale` once when a
line sits there over 5 min. So the main host's log is the only one written and only the main host pushes.
That host's own log (#344: its own `state.db`) is never written by a lane. Each
`check` imports the asks a lane left in a remote host's own log (lanes started before this; read through its
`drover events --jsonl`, or its `events.jsonl` on an older build) under a new id, with `from` naming the old one.

#345: with `[service] url` set, a remote lane gets `DROVER_SERVICE` and its own token instead of the outbox, and reports over
HTTP to `drover service` (docs/service.md); no ssh back to the main host.

An answer reaches a remote lane through that host's shed (`send`, then Enter).

## Turn a host off

`drover hosts off <name>` (or the `[off]` button on the host's card in the Host tab, paired devices only) sets
`enabled = false` in fleet.toml and logs `host-off` with `by`. `hosts on` and `[on]` undo it, logging `host-on`.
The buttons ask first (`turn air off? no new lanes; 2 running lanes finish`).

An off host gets no new issues. It keeps being monitored, and its running lanes drain there: their reviews and fix
rounds still go to it. The Host tab shows such a host grey, as `off · draining 2 lanes` while lanes remain, then
`off · not used for lanes`. The main host (no `machine`) runs drover, the board and the log: it has no `[off]`, and
`hosts off` refuses it `MAIN_HOST`; set `enabled = false` in fleet.toml by hand if you mean it.

#939: once an off host has no open lane left, `drive`, `check`, `status` and the tick never contact it (no ssh at
all); its lanes are still finished, cleaned and saved while any remain. Every remote call in a pass is killed with its
ssh's process group after `ssh_call_secs` (20 s), output still held open included; the host is then offline for the
rest of that pass and the other hosts and fleets go on. An ssh refused with `REMOTE HOST IDENTIFICATION HAS CHANGED`
logs `host-key-changed <host>` once (again only after the host was back online) and is not retried in that pass.

## Monitoring

- `drover hosts` prints, per host: on or off, reachable or offline, its tags, load/cpu, memory, disk, and its lanes.
- `drover status` gets a `HOST` column once fleet.toml has `[hosts]`; #609: it shows a live lane's `where`
  (`mini · shed`, docs/serve.md), as `board` and `waiting` do.
- The Host tab's HOSTS section has one card per host, main first, then A→Z, each with the same rows (#234): load/cpu,
  swap-out, memory, disk, top memory, lanes and tags, the state and why its dot is amber or red in its head. Remote
  values (and top memory, with its age) come from the 5-minute `check`. An unreachable host is grey
  (`offline · last seen 33m`) in the same layout, and its lanes never page (`lane-dead` and `stall` skip them).
- #478: while a host is unreachable, a spawn onto it is refused `HOST_OFFLINE` (other hosts' spawns go on), its lanes
  show `host offline: <host>` in `status`, `board` and `waiting` (drive neither respawns nor supersedes them), and
  `check` logs `host-offline` once when it drops and `host-online` once when it is back. A supersede that could not
  close the old pane there logs it (`stale_host`, `stale_pane`); drive kills it once the host answers (wip first, as
  `kill` does) and logs `stale-pane-killed`. `drover kill|close --host <name> <agent>` picks that host's agent when
  another host has a lane of the same name.
- #938: a runtime breaker per host. `[limits] runtime_down_failures` (5) lanes there failing to start or ending
  without a push within `runtime_down_minutes` (30) make drive log one `runtime-down` (host, count, a sample reason;
  one alert). While it is open a spawn there is refused `RUNTIME_DOWN`, those lanes show `runtime down on <host> since
  <HH:MM> UTC` in `status` and `waiting`, and drive neither respawns them nor parks them needs-human. Each slow pass
  runs one canary there (a throwaway shed session echoing a computed word);
  once it passes drive logs `runtime-up`, the held issues' earlier respawns stop counting, and spawns resume paced by
  `max_parallel_spawns`.
- History (#234): `<state dir>/host-history.jsonl` keeps 24 h of `{ts, host, load_cpu, pressure, swap_out, mem_free_gb,
  disk_free_gb, lanes}`: the main host a row a minute (`drive --fast`), a remote host a row every 5 minutes (`check`'s
  survey, no extra SSH). Rows past 24 h are dropped when it is rewritten (at most hourly), and it stays under
  about 180 KB a host. Each row on a card has a 24 h sparkline; a tap opens a 1 h / 6 h / 24 h chart with the limit
  dashed and the lane count as a step line. Downsampling keeps the worst row of each bucket (the max; free space the
  min), so a peak is never hidden.
- Watched sampling (#352): only while a board has the Host tab open (its `/live?tab=host` stream stamps
  `<state dir>/host-watch.at`; seen in the last 2 min) does the main host's row also carry `procs`: the MB `ps` sees
  in five fixed groups (lanes, CI/Colima, browsers, drover, other), never per PID. With nobody watching the fast
  step never walks the process table. The file is also capped at 1,600 rows a host on rewrite.
- Each lane card, and each horse in the Herd, shows its host name.

## Health (#235)

`drover doctor` checks every enabled remote host, and `check` does the same once an hour (with the tag probe). A
failure turns the host red on the Host tab (`unhealthy · <reason>`) and keeps new lanes off it: placement refuses it
with `NO_HOST: <host>: unhealthy: <reason>`. The checks, each with its fix:

- **git**: `ssh <host> git ls-remote https://github.com/<repo>.git` works for each fleet whose needs the host fits.
  drover's own git over SSH never has the keychain, so HTTPS fails with `could not read Username for
  'https://github.com'`. A failure reads `✗ git <fleet> — can't reach <repo>: <first error line>` and keeps only
  that fleet's lanes off the host; placement moves them to the next host. Fix: add the host's SSH key to GitHub
  and run `git config --global url.git@github.com:.insteadOf https://github.com/` there.
- **gh** and **claude**: `gh auth status` and `claude auth status` succeed in a login shell on the host
  (`sh -lc`). Lanes there need `gh` for their PRs, so `gh` logged out is
  a red host problem. Fix: `ssh <host> gh auth login` for gh; for claude, log in from a terminal there.
- **drover**: `drover --version` there prints the same version and git sha as on the main host. A mismatch is
  a warning (amber), not a failure.
- **shed**: its lanes list answers there; a failure is a `! shed: …` warning.

## Auto-update

Each `check` (every tick) reads `drover --version` on every enabled, reachable remote host. When it differs from the
main host's, and no install already runs there, it installs the main host's sha the way `hosts bootstrap` does:
`cargo install --git <repo> --rev <sha> --locked --root <root> --force`, then `drover profiles sync` if a fleet.toml
is there. The install runs in the background (output in `~/.drover-sync.log` on the host), so the tick never waits
on it. An offline or switched-off host gets no ssh for it.

The next tick logs the result as `host-sync` (host, from, to, ok). A failure alerts `host-sync-failed` once until an
install there succeeds, and the tick after retries it. Until the versions match, a shed-acp host takes no lanes
(`acp guard`). `drover hosts` shows each remote host's drover sha, with `✓` when it matches the main host's.

## When the main host is down

Nothing new starts. Lanes on other hosts keep running in their own shed servers. When the main host comes back,
`check` finds those lanes again from the log. To move the main host, follow docs/move.md.

## Cloud hosts (#254)

A cloud host is a `[hosts.<name>]` with a `cloud` recipe. drover starts it when a lane needs it and stops it when idle.

```toml
[hosts.boat1]
machine = "boat1"          # the ssh alias drover writes
cloud = "boat"             # the built-in recipe; or <config>/recipes/<name>.toml
size = "small"             # a key of the recipe's [rate_usd_hour]: boat's small|default|large|xlarge
budget_usd_month = 50      # hard: none set means 0, so it never starts
max_hours_day = 6          # optional
idle_stop_minutes = 20     # the default
tags = ["docker"]

[limits]
overflow_after_minutes = 10  # the default
```

- **The recipe** is a TOML file of `sh -c` templates: `create`, `start`, `stop`, `status` (prints `running` when up,
  nothing when there is no such sandbox), `address` (prints `user@host port`, then the host key line), `destroy`.
  A start whose `status` is empty runs `create` (`create_from` once the host was snapshotted) instead of `start`. It also sets `key_env`, the provider key's
  variable, and `[rate_usd_hour]` per size. `{name}` is the host's name and `{size}` its size. The built-in `boat`
  recipe is assets/recipes/boat.toml. Check its commands and rates against docs.boat.dev, and copy it to
  `<config>/recipes/boat.toml` to change them. #635: it targets the current boat CLI (`boat new --type --name`,
  `list --all --json`, `resume`, `stop`, `delete`, `snapshot <id> <name>`, `new --from`; it needs `jq`) and finds the
  sandbox by name. `user@host port` is the sandbox's `sshEndpoint`, the host key comes from `ssh-keyscan` at the first
  start and stays pinned: a start or survey that sees another key fails closed, and `hosts bootstrap <name>` re-pins
  it (logged `cloud-repin` with both keys). `identity` (boat: `~/.ssh/ascii_box_ed25519`, the only key a new sandbox
  takes) adds `IdentityFile` and `IdentitiesOnly yes` to the host block; doctor checks the file exists. `id` prints the
  sandbox's id and `{id}` in a line runs it: boat reads an empty id as the sandbox it runs in, so each recipe line
  refuses an empty one, and drover runs no `stop` or `destroy` when `id` prints nothing. Sizes
  are `small|default|large|xlarge`; an old `size = "medium"` is run as `default` and `drover doctor` shows a `!`
  row for it. doctor also checks each `boat` subcommand the recipe uses against `boat --help` (✗ with the recipe line).
- **The provider key** (#707) is optional: the `boat` CLI owns its own login, so a recipe command runs with no key.
  With `key_env` set, the key is read on the main host from the environment, else the 0600 file
  `<config dir>/secrets/<key_env>`, and each command gets it on stdin and exports it (the `[ci_runners]` API calls use
  it as `$BOAT_API_KEY`). `drover doctor` names the one it found (`✓ cloud boat1 key — env`), or warns `!` with none.
  It never appears in argv, fleet.toml or the event log.
- **Start on demand.** Placement uses a cloud host, stopped or running, in two cases only: every local host has been
  refused (busy, low memory or disk) for `overflow_after_minutes`, or the lane needs a tag only the cloud host has (#256:
  local Macs first, even when the cloud host is idle). The `spawn` event's `why` says which (`overflow: every local host
  busy 12 min`, `needs docker: no local host has it`), and `drover status` shows it as `on <host>: <why>`. A respawn on a lane's
  own host, `--host`, or a role `host` pin starts it too. `spawn` runs `start`, then polls `status` for up to 5 min.
  It then rewrites the host's block in `~/.ssh/config.d/herdsman` and its own known_hosts file
  (`~/.ssh/herdsman/<name>.known`, outside `config.d`) from `address`, and then spawns. A dry run never starts a host.
  Bootstrap of a running host, and each survey of a cloud host whose `status` says running, reads `address` again and
  rewrites the block when it moved.
  Add `Include config.d/herdsman` at the top of `~/.ssh/config` once: name only that file, since ssh reads every
  included file as config. doctor flags an `Include` that reads a `.known` file (the pre-#635 place); start
  removes those.
- **Idle stop.** `check` stops a running cloud host after `idle_stop_minutes` with no live agent on it. First, each
  lane worktree there with unpushed or dirty work is pushed to `wip/<host>-<lane>`, after the secret scan. A secret
  or a failed push aborts the stop (logged `cloud-abort`). The disk is kept. Lanes there show `paused: cloud host
  stopped` on the Host card, and their next spawn starts the host again.
- **Spend** is the running time (`cloud-start` → `cloud-stop` in the log) × the size's rate, per UTC month and day.
  The Host card shows `spent $12 of $50 · 3.1 h today of 6`, the address age and the last start and stop. The row
  turns amber at 80 % and red at the limit. At the limit, placement refuses `CLOUD_BUDGET` and `check` stops a running
  host (after the wip save). Only an owner edit to fleet.toml raises a limit.
- **Destroy** is owner only: `drover host destroy <name>` does the wip save, then runs the recipe's `destroy`.

## Bootstrap, snapshot and the GitHub token (#255)

`drover hosts bootstrap <name>` (owner only; it starts a stopped cloud host first) sets up a fresh host over SSH.
Each step checks first and is skipped when already done, then is read back after its install, so it is safe to run
again. In order:

1. git, gh, Claude Code, Rust with a C linker (apt and the official install scripts), then shed at drover's pin
   and its job (on a `shed-acp` host also node and the ACP adapter, below). Other toolchains a fleet needs go in your
   own snapshot.
2. **drover** at this host's exact sha: `cargo install --git <drover repo> --rev <sha>` into the same root as
   here (e.g. `~/.local`).
3. **Logins**, once and by you: for each of Claude and gh that is logged out, bootstrap opens a shed terminal on the
   host (a sign-in on the board) and stops. In the Claude terminal run the login; in the gh terminal
   paste the host's fine-grained token and press Enter (read without echo, never on a command line or in a log). Then
   run bootstrap again. drover never copies a token from this host.
4. **git over HTTPS** with that token (`gh auth setup-git`): never your SSH key.
5. A clone of each fleet's repo the host fits. Logged `host-bootstrap`. #592: no fleet.toml goes there; each spawn
   syncs its lane's profile (a full `profiles sync` runs only where an old fleet.toml is left; one an old bootstrap
   wrote is removed first, #647). A failed step prints its command, exit code and last 20 output lines, and logs them
   in a `host-bootstrap` event.

**The token.** Make one fine-grained token per cloud host, limited to the fleets' repos, with contents and pull
requests write and nothing else (no admin). It lives only on that host. Each health look reads it with one
`gh api -i` there: the Host card shows `github: token · 2 repos · expires in 80d`, and from 14 days before the
expiry the host turns amber (`! github — token expires in 12d`). When gh there can't use its token (expired, revoked
or not logged in yet) the card says `github: unreadable` and the host turns red, so placement skips it.

**Health on every start.** After a cloud host starts and its address is refreshed, drover runs the health checks
above (and the token look) before placement uses it, so an unhealthy host never takes the lane.

**Snapshot.** Once `drover doctor` is all ✓, `drover hosts snapshot <name>` runs a fresh health look and, with no
✗, the recipe's `snapshot` command (logged `cloud-snapshot`). The recipe's `create_from` template makes a new
sandbox from it that starts ready. Check both against docs.boat.dev.

## ACP hosts: `runtime = "shed-acp"` (#637)

```toml
[hosts.boat1]
machine = "boat1"
runtime = "shed-acp"   # unset: the fleet's runtime, as before

[proxy]                # all optional
# port = 41990         # the token proxy, here and (through a tunnel) on each shed-acp host
# upstream = "https://api.anthropic.com"
# key_hours = 12       # how long a lane key lives
```

A `shed-acp` host runs every lane as an ACP session in its own shed (`shed run --acp claude-acp … -- <adapter>`
over ssh). The brief is the first prompt, and every later prompt goes through the same remote shed.

- **Survey.** The host is up when its shed answers (`ls` through `shed connect`); a host whose shed is down is
  offline. Load, memory and disk come from the
  same ssh look as on any host.
- **Health.** git for each fleet it fits, `gh auth status` over ssh, and for each Claude
  role's profile the ACP guard below. A failed guard is a `✗ acp guard <profile>` row, which keeps new lanes off.
- **The guard, proven there (hard gate, as #565).** Before a lane starts, spawn syncs the lane's profile there
  (`drover profiles sync --profile …`) and checks three things: that drover there is this build's version (#573),
  that the profile's `settings.json` has drover's `PreToolUse` hook, and that `shed agents` there lists
  `claude-acp`. If any check fails, spawn refuses `ACP_UNGUARDED <host>: <why>` before anything is made, and records
  the `✗ acp guard` row so the next placement picks another host.
- **No token there: the token proxy.** The lane's adapter gets `ANTHROPIC_BASE_URL=http://127.0.0.1:<port>` and a
  per-lane key as `ANTHROPIC_AUTH_TOKEN`. The key reaches it only through the 0600 secrets file over ssh stdin, which
  is removed once the shell has read it. Port `<port>` there is an ssh reverse tunnel (`ssh -N -R`, started detached
  by spawn and each survey, its pid in `<state>/tunnel-<host>.pid`) to `drover proxy` here, which spawn starts when
  it does not answer. The proxy checks the key, then forwards the call to `upstream` with the role's `[accounts]`
  token (#707: the account's hand-placed `[accounts]` `token-file:<path>`, 0600, [accounts](accounts.md); only the
  proxy reads it) and the `oauth-2025-04-20` beta added. curl sends it, with the token in its config on stdin and never in
  argv, and the answer streams back as it comes. The account token never leaves the main host.
- **Lane keys.** They are kept in `<state>/proxy-keys.json` as sha256 only. A key signs while it is `live`: it is not
  `revoked`, not `expired` (`key_hours`), and not `ended` (its issue merged or closed, its lane closed, dead or
  spawned again, the last of which revokes the older key at once). `drover hosts keys` lists them with their state.
  `drover hosts keys revoke <id>` (owner only) ends one at once and logs `proxy-key-revoked`. A refused key gets a
  401 and nothing goes upstream.
- **Bootstrap.** `hosts bootstrap <name>` on a `shed-acp` host: git, gh, Claude Code,
  rust, shed at drover's pin (`cargo install --git …/shed --rev <shed::REV>`, read back as `shed <proto version>`),
  shed's job (`shed install`, then systemd `enable --now` or launchd `bootstrap`; `shed doctor` is the read-back),
  node and npx, `shed agents add claude-acp`, and drover. No Claude login is opened there. gh is the only login,
  and it must be done by hand over ssh until the git relay lands. `hosts snapshot` saves that state as before.

**Proof (2026-11-08, the mini).** A throwaway proxy on 127.0.0.1 swapped a lane key for the subscription token
(`CLAUDE_CODE_OAUTH_TOKEN`) and added `anthropic-beta: oauth-2025-04-20`. A Claude Code 2.1.311 client was started
with no credentials of its own: `env -i`, a fresh `HOME` and `CLAUDE_CONFIG_DIR`, only `ANTHROPIC_BASE_URL` and
`ANTHROPIC_AUTH_TOKEN=<lane key>`. Run as `claude -p` it answered `PROXY-OK` (exit 0, upstream 200). Run as a shed
ACP session (`shed run --acp claude-acp`, claude-agent-acp 0.101.0, its own shed server, no token in its env) it
answered `ACP-PROXY-OK` (upstream 200). With a wrong key the proxy answered 401 every time, so nothing went
upstream, and the client retried until killed. No `sk-ant` string was in either config folder afterwards.

**Not yet (follow-ups).** The git relay (the lane's origin through the Mini, pushes only to its own branch, PRs from
the Mini, no gh on any remote host). shed's machine registry (`shed machine add`, `--on`), one event stream per
machine, the host grace window, the doctor's cooee warning, and shed-acp as the default runtime. Bootstrap from
sha256-checked prebuilt binaries (needs release artifacts). The `where` field (#609) is not in this repo yet.

## CI runner VMs (#537)

`[ci_runners]` (owner-set; the hook guards fleet.toml) describes a repo's self-hosted GitHub runners on cloud VMs:

```toml
[ci_runners]
provider = "boat"        # a recipe, like a cloud host's: the built-in boat, or <config>/recipes/<name>.toml
repo = "owner/app"       # the repo the runners register to
label = "app-boat"       # the VM runners' label
size = "large"           # a key of the recipe's [rate_usd_hour]
ttl_hours = 0            # 0: no TTL
snapshots = false
slots = 3                # runner slots per VM
max_vms = 5
usd_day = 30             # the $/day cap: VMs × rate × 24
vms = ["bx_1", "bx_2"]   # the VMs made by hand; `runners add|remove` change the set in the log
service = "ci-runner"    # the VMs' runner service (the default), the recipe's `{service}`
local_label = "app-self"                          # optional: this host's own runners (`retire-local`)
local_stop = "~/ci/ci-runners.sh down --now"      # optional: the command that stops them
```

The recipe's `runner_*` commands (assets/recipes/boat.toml) create, read and delete a VM and run its VM-side steps.
The verbs, each logged `runners-<verb>`; an orchestrator runs them while its `runners` group is on, no other lane may:

- `drover runners status`: the health line (`9/9 online`: online VM runners of `slots × VMs`), each VM's state and
  each runner.
- `runners revive <vm>`: a fresh registration token to that VM and a restart of its runner service.
- `runners add [n]`: n VMs to the spec, each revived. Past `max_vms` or `usd_day` it refuses `RUNNERS_CAP`.
- `runners remove <vm|name>`: drain the VM (no job running, 30 min at most), deregister its runners, delete it; or
  deregister one idle runner by name. The last VM with online runners refuses `RUNNERS_LAST`.
- `runners tokens`: push a fresh token to every VM now. `drover check` (the 5-minute timer) is the refresher: it
  does the same every 30 min.
- `runners retire-local`: run `local_stop`, then deregister the idle `local_label` runners. While VM runners are
  below expected it refuses `RUNNERS_LOW`.

Past a refusal the only way is an owner ask: `drover ask <fleet> "drover runners add 2: ok?" --option yes --option
no --kind owner`; once the owner picks the first option, run the verb again with `--approved <id>` (one yes lets one run on).

The registration token comes from `gh api` on this host and reaches the VM on stdin, never in argv, the log or the
output; the provider key never leaves this host, and no other credential goes to a VM. `check` alerts `runners-low`
once when online VM runners drop below `slots × VMs`, and `runners-ok` once when they are back.

## Limits

Remote lanes run the Claude harness. The dsh harness writes its pre-push hook file on the main host only.

A stale binary, fleet.toml drift or shed on a remote host: see [troubleshooting](troubleshooting.md).
