# `drover service` (#345, Phase C)

ADR, decided by the owner in #345's q355 (2026-11-05): "Build the full service now … Approved: the tokio dependency
and the cap raise … Shape: API-first." This lifts the "no daemon / no async" rule for one process and one
dependency, `tokio`, in its own crate (`crates/service`); every other crate stays synchronous.

## Shape

- **One process**, `drover service --api <addr> [--board <addr>]`, kept alive by launchd (`KeepAlive`) or systemd
  (`Restart=always`). A second one refuses to start. It logs `service-started`; a stall logs `service-stalled`, which
  every notifier gets, and the process exits so the supervisor starts it again.
- **Local socket** `service.sock` beside `state.db` (mode 0600). A kick (a lane's push, stop or ask, a verdict, an
  answer, a board tap) goes there and is driven in the service's process at once, not as a detached `kick --now`.
  With no service listening, every command goes on in today's direct mode: nothing is ever stuck on it.
- **GitHub** is polled by the fast step (`drive --fast`) every 15 s inside the service; the `dev.drover.fast` job
  is no longer needed while it runs (`timer` doesn't reinstall it, `doctor` shows it "by the service"). The slow
  `tick` stays as the safety net. #530: each fleet's open PRs, check rollups and labels come in one GraphQL query; a
  fast step first sends three conditional REST reads (open PRs latest-updated first, Actions runs, integration's
  branch) with their last ETags, and when all three answer 304 (free of rate limit) it keeps the last query's
  result. It queries anyway 5 min after the last query, or while a PR's mergeability is still UNKNOWN.
- **API v1** (the contract the board and the future cooee plugin use):
  - `GET /api/v1/health`: pid, version, busy jobs, heartbeat age.
  - `GET /api/v1/events?after=<seq>`: Server-Sent Events, the log after `seq` (the last 500), then each new event as
    it is logged; this host or a paired device's cookie only.
  - `POST /api/v1/cmd` `{"token", "args", "files"}`: a remote lane's command (below).
- **The board** runs in the same process with `--board` (the `serve` job's address); it is not grown here.
- **Lane state** (#529) comes from shed's `events` stream, not from polling: the service follows it (keeping
  `shed.seq`, so drive's intake finds nothing left). Each lane's change is logged as a passive `lane-state`
  (`working`, `idle`, `done`, `blocked`; `gone` once its process exits) as it happens. A dropped stream logs
  `lane-stream` (`state: down`, `why`), the Host tab's `shed lane events` row turns amber ("down …: why · polling"),
  and the service reconnects every 5 s.

## Remote lanes over HTTP

With `[service] url = "http://<main host on the tailnet>:7778"` in fleet.toml, a lane spawned on another host gets
`DROVER_SERVICE` (that url) and `DROVER_TOKEN` instead of #389's outbox (`DROVER_OUTBOX`), so its commands
arrive at once. Its `ask`, `answer`, `verdict`, `done`, `kick` and its own events go to `/api/v1/cmd` with curl; a
`--file` travels as its text.
The main host keeps controlling other hosts over ssh (spawn, read, kill).

**The token** is 32 random bytes, issued by spawn, stored only as its sha256 in `state.db`, and never printed or
logged. It runs a command only when:

- the command is one the outbox would run as that lane (#389's check): its own fleet and issue, an answer's issue
  being its ask's;
- a verdict comes from a check role;
- the token's spawn is still the issue's latest for its role, its lane is not closed or dead, and the issue is open.

Anything else is `REFUSED TOKEN` (HTTP 403) and runs nothing.

## Robustness

Every job runs on a blocking thread in one of 16 slots with a timeout (a kick or poll 900 s, a lane command 150 s);
a timeout logs `service-timeout` and frees its slot, so a stuck job never blocks the others. At most 64 API
connections, 4 MiB a request, 64 KiB a socket line. A watchdog thread exits the process when the event loop's
heartbeat is 30 s late.

A timed-out kick frees its issue's spawn slot (the marker's pid is the service's own, which outlives the kick). A
`--file` must come with its text: a lane never makes the service read a path on the main host.

Killed mid-spawn (`kill -9`), the service leaves nothing that blocks: its spawn marker and kick lock name its pid, so
they are stale at once; the CLI goes on in direct mode, and a remote lane's command fails fast instead of hanging. The
next service starts over the dead one's socket, the next pass sweeps the marker and spawns again
(`crates/cli/tests/service.rs`).

## Install

`drover service --install [--api <addr>] [--board <addr>]` writes the unit (`dev.drover.service`) and prints the
commands that swap it in; it loads nothing itself. The API listens on `127.0.0.1:7778` by default; for remote lanes give
`--api` the main host's tailnet address (the one `[service] url` names). On macOS, for example:

```
launchctl bootout gui/$(id -u)/dev.drover.fast
launchctl bootout gui/$(id -u)/dev.drover.serve      # with --board only
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.drover.service.plist
```

To go back to direct mode, boot the service out and run `drover timer install`.

## Line cap

The service's budget is the planned +3,000, inside the 31,500 cap the owner set in q391 (#514).

## Deferred (q409)

The owner decided in #345's q409 (2026-11-06): "Defer (a)-(e) to a follow-up issue; list them in docs/service.md as
deferred with this decision; merge #519 as the service base." Not built here:

- (a) lane state from the runtime's event stream: built in #529 (see Lane state above);
- (b) batched GraphQL polling of GitHub with ETags: built in #530 (see GitHub above);
- (c) one kept-open SSH connection per host and one GitHub client;
- (d) the board served from memory with one live stream to each phone; the board still polls, and the future cooee
  plugin is to use `/api/v1/events`;
- (e) the week-long soak with flat memory and the measured latencies (board < 10 ms, push → review < 5 s,
  verdict → merge < 5 s).
