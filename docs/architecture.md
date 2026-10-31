# Architecture and state

herdsman has no daemon and no database. Every command reads the config, folds the event
log into the current state, acts, and appends what it did.

## Event log

`~/.local/state/herdsman/events.jsonl` (`FLEET_STATE`) is append-only, one JSON object per
line, fsynced on every write. A `spawn` event, shortened:

```json
{"ts":1761800000,"fleet":"app","issue":12,"type":"spawn","role":"impl","agent":"app-impl-12","pane":"w1:p2"}
```

`ts` is seconds since the epoch, `type` is the event kind, and the other fields are the
event's data. Issue state (`queued → impl → review(n) → fix(n) → ready → merged`, plus
`blocked` and `needs-human`) and the queue are folds over this log. A writer that died
mid-line leaves a torn tail; the next append terminates it first so the two records never
merge. A merged issue stays `merged`; `spawn` and `adopt` on it are refused with `MERGED`.

## Paths

| Path | What |
|---|---|
| `~/.config/herdsman/fleet.toml` (`FLEET_CONFIG`) | config: fleets, roles, limits, notifiers |
| `~/.local/state/herdsman/events.jsonl` (`FLEET_STATE`) | the event log |
| `~/.local/state/herdsman/cache/` | cached issue specs and PR/CI reads |
| `~/.herdr/worktrees/` (`FLEET_WORKTREES`) | lane worktrees, `<repo>/issue-<n>` |
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

## Size cap

At most 6,100 lines of Rust under `src/` (tests live in `tests/`). CI enforces it. #60 aimed for 5,000; without joining
lines or dropping features it ended at 6,080 (the herdr adapter, `src/herdr.rs`, included), so the cap is that count rounded up.
