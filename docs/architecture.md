# Architecture and state

herdsman has no daemon. Every command reads the config, folds the event log into the current state, acts, and
appends what it did.

#345 adds one optional long-running process, `herdsman service` (docs/service.md): kicks driven at once, the
GitHub poll and API v1. With it stopped every command still works this way.

## State database (#344)

The state lives in one SQLite file, `state.db` beside the `FLEET_STATE` path, in WAL mode with a busy timeout, mode
0600 (`crates/core/src/db.rs`). This lifts the old "no database" rule for one dependency, `rusqlite` (bundled), for
two reasons: **transactions** (an append is one insert, so concurrent writers never tear or lose a row, and an
import or a device change is all or nothing) and **leases** (a lock can no longer go stale).

| Table | What |
|---|---|
| `events` | the log: `seq`, `ts`, `fleet`, `issue`, `type`, the event's JSON, `archived`; indexed by (fleet, issue) and type |
| `leases` | every lock and marker: `drive.lock`, `drive-fast.lock`, `drive-<fleet>-<n>.lock`, `kick-<fleet>-<n>.lock` and `.again`, `spawn-<fleet>-<n>.pending` |
| `devices`, `push` | paired phones and web push subscriptions (were `serve/devices.json`, `webpush/subs.json`) |
| `snapshot` | `board`: #337's reads; `states`: #343's fold checkpoint |
| `processed` | #509: the trigger ids (`kick --id`: a verdict's sha, a decision) already driven, each driven once |

**Retry-safe side effects (#509, `crates/core/src/action.rs`):** spawn, supersede and merge log an `intent` with a
stable `action_id` (`<fleet>#<issue>:<step>@<key>`; key: the head sha for a merge, the role and the issue's last
spawn seq for a spawn) before they act, and `done` or `failed` with the same id after. The intent is taken in one
transaction that first reads the id's latest outcome: `done` skips (`ALREADY_DONE`), an intent younger than the
step's timeout skips (`IN_FLIGHT`), an older one (a crash) is recovered from the real world (the PR merged, the lane
logged or up) as `done`, or else `failed` and acted on again. A per-issue worker (#345) gets the same ids: each
step derives its own.

**Leases:** a row names its holder (pid and host), when it was taken and when it expires. Taking one is an atomic
insert-if-free in one transaction: free means no row, an expired row, or a holder pid on this host that no longer
runs. A dropped guard deletes its own row; a holder that died leaves a row that the next taker replaces, so
nothing has to sweep it first.

**Migration:** the first open of the database (any command) and `herdsman migrate` import whatever is left beside it
from before #344: `events.jsonl`, its archives `events-YYYYMM.jsonl.gz` (their rows `archived`), `serve/devices.json`
and `webpush/subs.json`. The import runs in one transaction and is checked: the fold of the database's events plus
the files' must equal the fold after the import, or nothing is imported. Each file is then renamed to
`<name>.migrated`, which is the backup. A second run finds nothing to import. Rows keep their seq unless the database
already has a later one (an old build that wrote `events.jsonl` again: its rows are added after).
The owner's own state dir (`~/.local/state/herdsman`, or the pre-rename `fleet`) is the exception: no command imports
there on its own, and `herdsman migrate` refuses it without `--live`. A test build run without `FLEET_STATE` would
otherwise take the log away from the installed build, which still reads `events.jsonl`.

**Export:** `herdsman events --jsonl` prints the whole log in its old format, one event per line with its seq, for
readers outside herdsman (scripts, a cron watchdog). Remote hosts each keep their own database; a lane there
forwards its writes to the main host (#275), and `check` reads a host's asks through its export (or its old log
file, on an older build).

## Event log

Events are append-only. A `spawn` event, shortened:

```json
{"ts":1761800000,"fleet":"app","issue":12,"type":"spawn","role":"impl","agent":"app-impl-12","pane":"w1:p2"}
```

`ts` is seconds since the epoch, `type` is the event kind, and the other fields are the
event's data. Issue state (`queued → impl → review(n) → fix(n) → ready → merged`, plus
`blocked` and `needs-human`) and the queue are folds over this log. An old JSONL log whose
writer died mid-line has a torn tail; its import skips it. A merged issue stays `merged`; `spawn` and `adopt` on it are refused with `MERGED`.

## Paths

| Path | What |
|---|---|
| `~/.config/herdsman/fleet.toml` (`FLEET_CONFIG`) | config: fleets, roles, limits, notifiers |
| `~/.local/state/herdsman/events.jsonl` (`FLEET_STATE`) | names the state dir; `state.db` beside it holds the log (#344), the file itself only before its import |
| `~/.local/state/herdsman/cache/` | cached issue specs and PR/CI reads |
| `~/.local/share/herdsman/work/` (`FLEET_WORKTREES`; a host's `root`/`roots`, #630) | lane worktrees, `<repo>/issue-<n>`; builds under `.build/` |
| `~/.config/herdsman/claude/<profile>/` | `settings.json` (PreToolUse hook), `role.md`, `fleet.md` |
| `~/.hermes/profiles/<profile>/config.yaml` (`FLEET_HERMES_PROFILES`) | sync rewrites its `hooks:` key and drops `agent.reasoning_effort` (fleet.toml `effort` owns it) |
| `~/.config/herdsman/dsh/<profile>.patch.yml` | DSH patch layer for `harness = "dsh"` lanes ([dsh.md](dsh.md)) |
| `roles/`, `hooks/` | role prompts, the tail of `help --agent` (`usage-tail.md`) and hook settings, compiled into the binary |

A path variable that is set but empty counts as unset.

## Rename from `fleet`

herdsman was called `fleet` (#27). Only `herdsman migrate` moves `~/.config/fleet` and
`~/.local/state/fleet` to the paths above, both or neither; `HERDSMAN_HOME` overrides
`$HOME` for it. Until then other commands read the old dirs in place and print a one-line
hint on stderr (`hook` stays silent). Lanes export `HERDSMAN_LANE`; `FLEET_LANE` still arms
the hook for one release, and `HERDSMAN_LANE` wins when both are set.

## Module map

Three crates (#264): `crates/core` (every module below but `serve`), `crates/server` (`serve`, over core only) and
`crates/cli` (the `herdsman` binary: core plus server, and the integration tests). `cargo install --path crates/cli`.
Core never depends on server or cli: `herdsman serve` reaches it through the `SERVE` hook `main.rs` sets. CI checks
both rules: no server or cli crate in core's dependency tree, and `std::process::Command` in core only in `runner`
(the `Runner` implementation), `events` (notifiers) and `webpush` (curl).
The split is a pure move by what compiles today, so core still holds I/O modules (`drive`, `spawn`, `harness`,
`devices`, `webpush`): their cycles (`gate` → `spawn`/`drive`/`check`, `config` → `harness`/`devices`,
`events` → `webpush`, `serve` → `drive` → `cli`) run through them. #265–#269 move them out step by step.

Every module under `crates/*/src/`, one job each. External tools go through one seam: `runner` (any program), `herdr` (every
herdr argv) and `gh` (cached GitHub reads).

| Module | Job |
|---|---|
| `cli` | clap CLI, `help --agent`, dispatch (`crates/cli`: `main.rs` sets the `SERVE` hook) |
| `lib` | `Ctx` (paths, clock); `ask`, `answer`, `asks` |
| `config` | fleet.toml: load, validate, in-place edits that keep every other byte |
| `events` | the append-only log, the state fold, notifiers |
| `db` | the state database: events, leases, devices, push, snapshots; the import of the old files (#344) |
| `pipeline` | a fleet's stages and where each outcome goes ([pipeline.md](pipeline.md)) |
| `queue` | `queue`, `next`, `board` |
| `drive`, `timer` | one pass of the loop; the jobs that run it (`setup`, `timer`, `tick`) and the Services rows |
| `spawn`, `harness/*` | `spawn`, `adopt`, `profiles sync`; one file per harness (claude, hermes, dsh) |
| `gate` | `verdict` and `merge` |
| `check` | `check` alerts, `clean`, `stop`, `kill`, `close`, the worktree budget |
| `waiting` | what an idle lane waits on (`waiting`) |
| `host`, `hosts` | host-load admission and live agents; multi-host placement ([hosts.md](hosts.md)) |
| `access`, `usage` | a lane's account and MCP servers; account and provider usage (`accounts`) |
| `hook` | the lanes' pre-tool hook |
| `overlap` | conflict preview between open branches, `cap-check` |
| `serve`, `lanes`, `devices`, `webpush` | the phone page, its Lanes tab, paired devices, web push |
| `doctor`, `protection` | read-only host and branch-protection checks |
| `runner`, `herdr`, `gh` | the seams to external programs |

## API types

Every JSON shape the page reads or posts has one definition, in Rust: `crates/server/src/api.rs` (`/herd.json`, the
lane chat, `/live`'s `ev`, API v1's tabs (#269: the needs cards, Lanes, [chat]), the `/send`, `/answer`, `/act` and `/setup` bodies) and the core rows the
board shows (`Event` for events and asks, `IssueState`, `Waiting`, `HostSeen`, `HostRow`). `ts-rs`, behind the server's
`ts` feature (never in the release build; CI checks `cargo tree -e normal`), turns them into `assets/types/api.d.ts`:
`just types` writes it (`cargo test -p herdsman-server --features ts export_bindings`). The types follow serde's JSON:
u64 is `number`, `Option<T>` is `field?: T | null`, `rename` and `flatten` as serde does them. CI regenerates the file
and fails on any diff, then `serde_json_fits_the_types` type-checks the demo export's real payloads and each type's
default with `tsc --noEmit --strict`, and `just types-lint` fails a hand-written copy of a generated type in the board's
TS: it imports them from `assets/types/api`.

## Code rules

- **Format:** rustfmt with `max_width = 120` (`rustfmt.toml`); CI runs `cargo fmt --check`. The formatter decides
  layout, not the lane: no golf, no joined or packed lines to fit the cap. Run `cargo fmt` before committing.
- **Lints:** clippy `pedantic` as warnings: `[workspace.lints]` in `Cargo.toml` for core and server (#264), `#![warn(clippy::pedantic)]` in the binary's `main.rs`; CI's `-D warnings` makes it an error. Other
  exceptions sit inline at the narrowest scope as `#[allow(clippy::<lint>)] // reason`. The crate-level allows, all
  style noise for a CLI:
  - `must_use_candidate`: return values here are read or deliberately dropped; `#[must_use]` everywhere is noise.
  - `missing_errors_doc`: nearly every fn returns `anyhow::Result`, and the error is the message it prints.
  - `missing_panics_doc`: the few `unwrap`s are on invariants, not inputs.
  - `too_many_lines`: dispatch and page renderers are long matches; splitting them only moves lines.
  - `format_push_string`: the HTML page is built with `+= &format!(..)`; one extra allocation per row is nothing.
  - `format_collect`: `.map(|x| format!(..)).collect()` reads better than a `fold` with `write!`.
  - `many_single_char_names`: short names in small math and parsing closures (dates, hex, crypto).
- **Size cap:** at most `line_cap` (in `ci/cap.toml`) formatted lines of non-test Rust under `src/` and `crates/*/src/` (tests
  live in `crates/cli/tests/`), counted after `cargo fmt`. CI runs `herdsman cap-check --config ci/cap.toml`; a fleet can set its own
  `line_cap` under `[fleet.<name>]`. Raising the cap needs the owner's OK in an issue.

History: one file per change in `docs/cap-history/<date>-<issue>.md`, so no two PRs edit the same line.
