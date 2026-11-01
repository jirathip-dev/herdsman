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

## Code rules

- **Format:** rustfmt with `max_width = 120` (`rustfmt.toml`); CI runs `cargo fmt --check`. The formatter decides
  layout, not the lane: no golf, no joined or packed lines to fit the cap. Run `cargo fmt` before committing.
- **Lints:** `#![warn(clippy::pedantic)]` at the crate roots, and CI's `-D warnings` makes it an error. Other
  exceptions sit inline at the narrowest scope as `#[allow(clippy::<lint>)] // reason`. The crate-level allows, all
  style noise for a CLI:
  - `must_use_candidate`: return values here are read or deliberately dropped; `#[must_use]` everywhere is noise.
  - `missing_errors_doc`: nearly every fn returns `anyhow::Result`, and the error is the message it prints.
  - `missing_panics_doc`: the few `unwrap`s are on invariants, not inputs.
  - `too_many_lines`: dispatch and page renderers are long matches; splitting them only moves lines.
  - `format_push_string`: the HTML page is built with `+= &format!(..)`; one extra allocation per row is nothing.
  - `format_collect`: `.map(|x| format!(..)).collect()` reads better than a `fold` with `write!`.
  - `many_single_char_names`: short names in small math and parsing closures (dates, hex, crypto).
- **Size cap:** at most `line_cap` (in `ci/cap.toml`) formatted lines of non-test Rust under `src/` (tests live in
  `tests/`), counted after `cargo fmt`. CI runs `herdsman cap-check --config ci/cap.toml`; a fleet can set its own
  `line_cap` under `[fleet.<name>]`. Raising the cap needs the owner's OK in an issue.

History: one file per change in `docs/cap-history/<date>-<issue>.md`, so no two PRs edit the same line.
