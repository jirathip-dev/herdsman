# Hosts: lanes on more than one machine (#157)

One **main host** runs herdsman, the board (`herdsman serve`) and the event log. It starts lanes on any host through
herdr's `--machine`, picks the least-busy host that fits, and shows every host on the board. With no `[hosts]` in
fleet.toml nothing changes: every lane runs on the main host, as before.

## Add a host

1. On the main host, save the machine in herdr: `herdr machine add` (an SSH target), then check it with
   `herdr machine status`. herdsman reaches the host two ways under that one name: `herdr --machine <name> …` for
   herdr commands, and `ssh <name> …` for everything else (git, probes, file reads and writes). So the machine's label
   must also work as an SSH host name, e.g. an entry in `~/.ssh/config`. Key-based login only, because herdsman runs
   ssh with `BatchMode=yes` (#337: and `ControlMaster=auto`, one connection kept 10 minutes). The board's reads of a
   host (#337) are one ssh call per step, its herdr run as `~/.local/bin/herdr` there.
2. On the new host, install and log in to:
   - herdr (its server running);
   - git;
   - `gh` (`gh auth login`): lanes there open their PRs with it;
   - git access to each fleet repo: the host clones each repo itself the first time a lane goes there, with plain
     `git clone https://github.com/<repo>.git <local>`, so its git config (`insteadOf`, credential helper) applies;
   - the agent harness (Claude Code logged in);
   - herdsman itself. The lane's tool-guard hook calls `herdsman hook`, and the Claude profile lives under that host's
     own home: each spawn there syncs its lane's profile (`profiles sync --profile <p> --role <r>`, #592).

   herdsman never copies credentials to another host: a role's `account` token is only exported to lanes on the
   main host; a lane on another host runs on that host's own Claude login.
3. Add the host to fleet.toml:

```toml
[hosts.mini]            # the main host: no `machine`
tags = ["fast"]

[hosts.bazzite]
machine = "bazzite"     # the herdr machine label (also its SSH name)
tags = ["gpu"]          # hand tags, added to the detected ones
root = "~/lanes"        # workspace root on that host (default ~/.local/share/herdsman/work)
enabled = true          # false: no new lanes go there (see below)
```

## Workspace roots (#630)

Everything herdsman makes on a host lives under its workspace root: worktrees at `<root>/<repo>/<lane>`, the
shared Cargo builds and lane scratch under `<root>/.build`. `herdsman migrate` moves the worktrees from the old
`~/.herdr/worktrees` (a live lane's on a later run, once it has ended), then drops `~/.cache/herdsman/target`.

`roots = ["/Volumes/NVMe/herdsman", "~/.local/share/herdsman/work"]` lists several in order (an external drive
first): a new lane takes the first that is mounted, writable and has `min_free_gb`; its spawn logs that `root`, and
an issue's later lanes stay on it while it can take them. `herdsman hosts eject <host> [<root>]` (default the first)
takes the root out at once: each lane there is saved (`wip/<agent>-<stamp>` for anything unpushed or uncommitted),
its worktree removed, the live one restarted on the next root from that save; then it prints
`safe to unplug <root>` (exit 1 while an agent still works there). A root pulled without eject is noted once by
`check` (`! root <path> gone`): its live lane restarts on the next root from its branch, and the issue's waiting line
says what was lost. Mounted again, the root takes new lanes; nothing moves back on its own. `herdsman status`, the
lane sheet and each host card (with `/api/v1`'s host data) show the roots, their state (`active`, `ejecting`,
`gone`) and free space.

A fleet's `local` that starts with `~/` points to that path under each host's own home. An absolute `local` is used
as written on every host.

## Tags

Each `herdsman check` (and `herdsman hosts refresh`) looks at every host. Once an hour it probes the host's tags with
one shell line: `os:macos` or `os:linux`, `arch:arm64` or `arch:x86_64`, `xcode` (when `xcodebuild -version` works) and
`docker` (when `docker info` works). The results go to `<state dir>/hosts.json`, along with the host's `$HOME`,
whether it answered, and its load/cpu, free memory and free disk. A host's tags are the detected ones plus its hand
`tags`.

## Needs

A lane needs the union of three lists:

- the fleet's `needs` (`[fleet.<n>] needs = ["xcode"]`);
- the role's `needs` (`[fleet.<n>.role.design] needs = ["os:macos"]`);
- the queue item's needs (`herdsman queue <fleet> add 12 --needs docker,gpu` adds to them;
  `herdsman queue <fleet> set 12 needs docker` replaces them; `queue ls` shows them).

## Placement

`herdsman spawn` picks the host like this:

1. `--host <name>` forces that host.
2. Otherwise, a role pinned with `[fleet.<n>.role.<r>] host = "main"` (or a host name) goes there. `main` is the host
   without `machine`. A `design` lane always runs on the main host, in the owner's herdr: it defaults to `main`,
   and `--host` or a pin naming another host is refused `DESIGN_MAIN_ONLY`.
3. Otherwise, an issue that already had a lane stays on that lane's host, because its worktree is there. This holds
   even when the host has been turned off.
4. Otherwise, herdsman takes the enabled hosts whose tags cover the needs, and picks the least busy: lowest load per
   CPU first, then the most free memory.

Admission runs per host: `DISK_LOW`, `MEM_LOW`, then `HOST_BUSY`. A host that fails admission (or is unreachable) is
skipped. A `DISK_LOW` names the done lanes' worktrees `herdsman clean --yes` would remove on that host (or says there
are none and points to `herdsman reclaim`), and a spawn still starting toward a full host holds no `max_parallel_spawns`
slot, so it never holds back spawns bound for another host (#416). `herdsman clean` covers every host's merged or
closed lanes (shown as `<host>:<path>`) and lists the orphan folders under each enabled host's worktree root;
`--orphans --yes` removes them. When every fitting host fails, the spawn is refused with the least-busy host's reason. When no host fits at
all (or only a cloud host over its budget fits), it is refused with `REFUSED NO_HOST: no host with docker`, plus
` (cloud host stopped or over budget)` when fleet.toml has a cloud host; the Host tab reads it as `waiting`. drive
retries on its next pass.

The `spawn` event records the lane's `host` and `machine`. From then on, every herdr call for that lane carries
`--machine` (pane reads, prompts, stop, kill, close), and its worktree work runs over ssh on that host:
`git worktree`, the brief and MCP files, the reviewer's `verdict.md`, the dirty and unpushed checks, and `clean`.

`spawn --dry-run` places the lane first and prints the commands exactly as they are sent: `--machine`, that host's
paths and home, and no account token. If the agent fails to start on another host, its new workspace is closed.

## A remote lane's hook rules (#592)

Only the main host has a fleet.toml. Spawn resolves a remote lane's hook rules there (protected branches, the fleet's
`deny_commands` under the host's `[hosts.<name>]` rules, the reviewer flag) and writes them to
`<worktree>.rules.json` on the lane's host, exported as `HERDSMAN_RULES`, and syncs the lane's harness profile there
by name. The hook there reads only that file (the lane
may not edit it); a missing or unreadable one fails closed `HOOK_FAILED_CLOSED`. It holds no notifier, token or env
value. The rules are frozen at spawn: after a config change that alters them, the board marks the lane `rules stale`
until a respawn. A fleet.toml left on a remote host is no longer read: `herdsman doctor` shows
`! stale fleet.toml on <host>: no longer read`; remove it by hand (herdsman never does).

## Asks, answers and verdicts from another host (#275, #389)

A lane on another host never connects to the main host: it holds no key, token or ssh route to it. Spawn exports
`HERDSMAN_OUTBOX=1`, and its `herdsman ask`, `answer`, `verdict`, `done` and its own events (`hook-denied`,
`coauthor-trailer`) are appended to `<worktree>/.herdsman/outbox.jsonl` (exit 0: `queued for the main host: drive
delivers it within 1 min`). Each fast drive pass pulls every live remote lane's outbox over main → host ssh and runs
each line as that lane (from its spawn event, never the line), once per lane and timestamp; a line for another issue
or role, or a verb a lane doesn't run itself, is refused and logged `outbox`. `check` raises `outbox-stale` once when a
line sits there over 5 min. So the main host's log is the only one written and only the main host pushes.
That host's own log (#344: its own `state.db`) is never written by a lane. Each
`check` imports the asks a lane left in a remote host's own log (lanes started before this; read through its
`herdsman events --jsonl`, or its `events.jsonl` on an older build) under a new id, with `from` naming the old one.

#345: with `[service] url` set, a remote lane gets `HERDSMAN_SERVICE` and its own token instead of the outbox, and reports over
HTTP to `herdsman service` (docs/service.md); no ssh back to the main host.

An answer reaches a remote lane with `herdr --machine <m> agent prompt`, then Enter. When `agent start` gives up
(`timeout`, `agent_name_not_found`, `agent_launch_pending`; on any host, the main one under load too), herdsman
confirms the pane over 120 s (#359): the harness's process runs there and its startup title (Claude: `✳`) or a
reported state has been seen; any change restarts the window. A confirmed pane is named (`agent_launch_pending` on
the rename waits and retries) and adopted (`spawn-adopted`); one that never confirms is closed and started once more
on a fresh pane, and closed again if that fails too. It never closes the pane or workspace of a harness that came up.

## Turn a host off

`herdsman hosts off <name>` (or the `[off]` button on the host's card in the Host tab, paired devices only) sets
`enabled = false` in fleet.toml and logs `host-off` with `by`. `hosts on` and `[on]` undo it, logging `host-on`.
The buttons ask first (`turn air off? no new lanes; 2 running lanes finish`).

An off host gets no new issues. It keeps being monitored, and its running lanes drain there: their reviews and fix
rounds still go to it. The Host tab shows such a host grey, as `off · draining 2 lanes` while lanes remain, then
`off · not used for lanes`. The main host (no `machine`) runs herdsman, the board and the log: it has no `[off]`, and
`hosts off` refuses it `MAIN_HOST`; set `enabled = false` in fleet.toml by hand if you mean it.

## Monitoring

- `herdsman hosts` prints, per host: on or off, reachable or offline, its tags, load/cpu, memory, disk, and its lanes.
- `herdsman status` gets a `HOST` column once fleet.toml has `[hosts]`.
- The Host tab's HOSTS section has one card per host, main first, then A→Z, each with the same rows (#234): load/cpu,
  swap-out, memory, disk, top memory, lanes and tags, the state and why its dot is amber or red in its head. Remote
  values (and top memory, with its age) come from the 5-minute `check`. An unreachable host is grey
  (`offline · last seen 33m`) in the same layout, and its lanes never page (`lane-dead` and `stall` skip them).
- #478: while a host is unreachable, a spawn onto it is refused `HOST_OFFLINE` (other hosts' spawns go on), its lanes
  show `host offline: <host>` in `status`, `board` and `waiting` (drive neither respawns nor supersedes them), and
  `check` logs `host-offline` once when it drops and `host-online` once when it is back. A supersede that could not
  close the old pane there logs it (`stale_host`, `stale_pane`); drive kills it once the host answers (wip first, as
  `kill` does) and logs `stale-pane-killed`. `herdsman kill|close --host <name> <agent>` picks that host's agent when
  another host has a lane of the same name.
- History (#234): `<state dir>/host-history.jsonl` keeps 24 h of `{ts, host, load_cpu, pressure, swap_out, mem_free_gb,
  disk_free_gb, lanes}`: the main host a row a minute (`drive --fast`), a remote host a row every 5 minutes (`check`'s
  survey, no extra SSH). Rows past 24 h are dropped when it is rewritten (at most hourly), and it stays under
  about 180 KB a host. Each row on a card has a 24 h sparkline; a tap opens a 1 h / 6 h / 24 h chart with the limit
  dashed and the lane count as a step line. Downsampling keeps the worst row of each bucket (the max; free space the
  min), so a peak is never hidden.
- Watched sampling (#352): only while a board has the Host tab open (its `/live?tab=host` stream stamps
  `<state dir>/host-watch.at`; seen in the last 2 min) does the main host's row also carry `procs`: the MB `ps` sees
  in five fixed groups (lanes, CI/Colima, browsers, herdsman, other), never per PID. With nobody watching the fast
  step never walks the process table. The file is also capped at 1,600 rows a host on rewrite.
- Each lane card, and each horse in the Herd, shows its host name.

## Health (#235)

`herdsman doctor` checks every enabled remote host, and `check` does the same once an hour (with the tag probe). A
failure turns the host red on the Host tab (`unhealthy · <reason>`) and keeps new lanes off it: placement refuses it
with `NO_HOST: <host>: unhealthy: <reason>`. The checks, each with its fix:

- **herdr**: the server's version equals the installed client's (`herdr status` on the host). An old server keeps
  running after an update, and `herdr machine add` then fails with "remote server is not ready": update herdr on the
  host and restart its server.
- **herdr session**: on macOS the server must run under the user's launchd job (the login session). A server
  restarted from an SSH shell runs outside it, with the keychain locked: `gh` says "token invalid", private clones
  fail and Claude can't read its login. Fix: `launchctl kickstart -k gui/<uid>/<label>` (the label of the job that
  starts `herdr server`).
- **git**: `ssh <host> git ls-remote https://github.com/<repo>.git` works for each fleet whose needs the host fits.
  herdsman's own git over SSH never has the keychain, so HTTPS fails with `could not read Username for
  'https://github.com'`. A failure reads `✗ git <fleet> — can't reach <repo>: <first error line>` and keeps only
  that fleet's lanes off the host; placement moves them to the next host. Fix: add the host's SSH key to GitHub
  and run `git config --global url.git@github.com:.insteadOf https://github.com/` there.
- **gh** and **claude**: `gh auth status` and `claude auth status` succeed inside a herdr pane on the host (a
  throwaway `herdsman-health` workspace, closed again). Lanes there need `gh` for their PRs, so `gh` logged out is
  a red host problem. Fix: `ssh <host> gh auth login` for gh; for claude, log in from a pane there.
- **herdsman**: `herdsman --version` there prints the same version and git sha as on the main host. A mismatch is
  a warning (amber), not a failure.

## Auto-update

When `check` sees an older herdsman on a remote host, and fleet.toml has the herdsman fleet, it rebuilds the main
host's sha there in the background: `git fetch`, `git checkout --detach <sha>` and
`cargo install --path crates/cli --locked --root <root> --force` in that fleet's checkout on the host, then
`herdsman profiles sync` if an old fleet.toml is left there (#592). Output goes to `~/.herdsman-update.log` on the host. Each host and sha is tried once, and
logged as `host-update` (with the error when the ssh call failed). Ten minutes later `check` reads the version back.
A host still on the old version stays amber. A failed update never blocks the main host.

## When the main host is down

Nothing new starts. Lanes on other hosts keep running in their own herdr servers. When the main host comes back,
`check` finds those lanes again from the log. To move the main host, follow docs/move.md.

## Cloud hosts (#254)

A cloud host is a `[hosts.<name>]` with a `cloud` recipe. herdsman starts it when a lane needs it and stops it when idle.

```toml
[hosts.boat1]
machine = "boat1"          # the ssh alias herdsman writes, and the herdr machine label
cloud = "boat"             # the built-in recipe; or <config>/recipes/<name>.toml
size = "small"             # a key of the recipe's [rate_usd_hour]
budget_usd_month = 50      # hard: none set means 0, so it never starts
max_hours_day = 6          # optional
idle_stop_minutes = 20     # the default
tags = ["docker"]

[limits]
overflow_after_minutes = 10  # the default
```

- **The recipe** is a TOML file of `sh -c` templates: `create`, `start`, `stop`, `status` (prints `running` when up),
  `address` (prints `user@host port`, then the host key line), `destroy`. It also sets `key_env`, the provider key's
  variable, and `[rate_usd_hour]` per size. `{name}` is the host's name and `{size}` its size. The built-in `boat`
  recipe is assets/recipes/boat.toml. Check its commands and rates against docs.boat.dev, and copy it to
  `<config>/recipes/boat.toml` to change them.
- **The provider key** is read on the main host from the environment, else from the store's `herdsman/all/<key_env>`. Each
  command gets it on stdin and exports it. It never appears in argv, fleet.toml or the event log.
- **Start on demand.** Placement uses a cloud host, stopped or running, in two cases only: every local host has been
  refused (busy, low memory or disk) for `overflow_after_minutes`, or the lane needs a tag only the cloud host has (#256:
  local Macs first, even when the cloud host is idle). The `spawn` event's `why` says which (`overflow: every local host
  busy 12 min`, `needs docker: no local host has it`), and `herdsman status` shows it as `on <host>: <why>`. A respawn on a lane's
  own host, `--host`, or a role `host` pin starts it too. `spawn` runs `start`, then polls `status` for up to 5 min.
  It then rewrites the host's block in `~/.ssh/config.d/herdsman` and its own known_hosts file
  (`~/.ssh/config.d/herdsman-<name>.known`) from `address`, and then spawns. A dry run never starts a host. Add
  `Include config.d/*` at the top of `~/.ssh/config` once.
- **Idle stop.** `check` stops a running cloud host after `idle_stop_minutes` with no live agent on it. First, each
  lane worktree there with unpushed or dirty work is pushed to `wip/<host>-<lane>`, after the secret scan. A secret
  or a failed push aborts the stop (logged `cloud-abort`). The disk is kept. Lanes there show `paused: cloud host
  stopped` on the Host card, and their next spawn starts the host again.
- **Spend** is the running time (`cloud-start` → `cloud-stop` in the log) × the size's rate, per UTC month and day.
  The Host card shows `spent $12 of $50 · 3.1 h today of 6`, the address age and the last start and stop. The row
  turns amber at 80 % and red at the limit. At the limit, placement refuses `CLOUD_BUDGET` and `check` stops a running
  host (after the wip save). Only an owner edit to fleet.toml raises a limit.
- **Destroy** is owner only: `herdsman host destroy <name>` does the wip save, then runs the recipe's `destroy`.

## Bootstrap, snapshot and the GitHub token (#255)

`herdsman hosts bootstrap <name>` (owner only; it starts a stopped cloud host first) sets up a fresh host over SSH.
Each step checks first and is skipped when already done, then is read back after its install, so it is safe to run
again. In order:

1. **herdr** at this host's version (`herdr machine add <machine>` from here), then git, gh, Claude Code and Rust with
   a C linker (apt and the official install scripts). Other toolchains a fleet needs go in your own snapshot.
2. **herdsman** at this host's exact sha: `cargo install --git <herdsman repo> --rev <sha>` into the same root as
   here (e.g. `~/.local`).
3. **Logins**, once and by you: for each of Claude and gh that is logged out, bootstrap opens a herdr pane on the
   host (visible in herdr and as a sign-in on the board) and stops. In the Claude pane run the login; in the gh pane
   paste the host's fine-grained token and press Enter (read without echo, never on a command line or in a log). Then
   run bootstrap again. herdsman never copies a token from this host.
4. **git over HTTPS** with that token (`gh auth setup-git`): never your SSH key.
5. A clone of each fleet's repo the host fits. Logged `host-bootstrap`. #592: no fleet.toml goes there; each spawn
   syncs its lane's profile (a full `profiles sync` runs only where an old fleet.toml is left).

**The token.** Make one fine-grained token per cloud host, limited to the fleets' repos, with contents and pull
requests write and nothing else (no admin). It lives only on that host. Each health look reads it with one
`gh api -i` there: the Host card shows `github: token · 2 repos · expires in 80d`, and from 14 days before the
expiry the host turns amber (`! github — token expires in 12d`). When gh there can't use its token (expired, revoked
or not logged in yet) the card says `github: unreadable` and the host turns red, so placement skips it.

**Health on every start.** After a cloud host starts and its address is refreshed, herdsman runs the health checks
above (and the token look) before placement uses it, so an unhealthy host never takes the lane.

**Snapshot.** Once `herdsman doctor` is all ✓, `herdsman hosts snapshot <name>` runs a fresh health look and, with no
✗, the recipe's `snapshot` command (logged `cloud-snapshot`). The recipe's `create_from` template makes a new
sandbox from it that starts ready. Check both against docs.boat.dev.

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
The verbs, each logged `runners-<verb>`; a conductor runs them while its `runners` group is on, no other lane may:

- `herdsman runners status`: the health line (`9/9 online`: online VM runners of `slots × VMs`), each VM's state and
  each runner.
- `runners revive <vm>`: a fresh registration token to that VM and a restart of its runner service.
- `runners add [n]`: n VMs to the spec, each revived. Past `max_vms` or `usd_day` it refuses `RUNNERS_CAP`.
- `runners remove <vm|name>`: drain the VM (no job running, 30 min at most), deregister its runners, delete it; or
  deregister one idle runner by name. The last VM with online runners refuses `RUNNERS_LAST`.
- `runners tokens`: push a fresh token to every VM now. `herdsman check` (the 5-minute timer) is the refresher: it
  does the same every 30 min.
- `runners retire-local`: run `local_stop`, then deregister the idle `local_label` runners. While VM runners are
  below expected it refuses `RUNNERS_LOW`.

Past a refusal the only way is an owner ask: `herdsman ask <fleet> "herdsman runners add 2: ok?" --option yes --option
no --kind owner`; once the owner picks the first option, run the verb again with `--approved <id>` (one yes lets one run on).

The registration token comes from `gh api` on this host and reaches the VM on stdin, never in argv, the log or the
output; the provider key never leaves this host, and no other credential goes to a VM. `check` alerts `runners-low`
once when online VM runners drop below `slots × VMs`, and `runners-ok` once when they are back.

## Limits

Remote lanes run the Claude harness. The dsh harness writes its pre-push hook file on the main host only.

A stale binary, fleet.toml drift or shed on a remote host: see [troubleshooting](troubleshooting.md).
