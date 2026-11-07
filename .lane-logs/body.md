Closes #555

## For humans
The drover plugins can now do through herdsman's API everything they used to borrow from the old board: reply to a lane, answer a lane's menu, stop/kill/clean a lane, start and use a setup terminal, read the Herd scene and the role pickers, set the new-thread chat defaults, and start a chat about or close a backlog issue.
The old board keeps working unchanged. A new contract test fails CI whenever an API reply changes shape without its recorded fixture being updated, and the plugins can copy those fixtures.

## What changed
- `crates/server/src/serve/v1.rs` (the only server file outside the board freeze): new routes, each calling the old board's own code:
  - `GET herd` (= `/herd.json`), `GET roles?fleet=` (the [config] sheet + `[chat]` defaults), `GET setup/<pane>` (= `/lane?setup=`)
  - `POST send` / `menu` / `act` / `setup` go through the old `send()` path (same 10-a-minute limit, PAUSED / REV_READONLY / STALE / BAD_VALUE / NO_SUCH_AGENT / NO_SUCH_SETUP), mapped to `{ok, code, message}` (`RATE_LIMIT` for 429)
  - `POST chat-about`: `/thread`'s `new` with the issue named in the first message; `POST chat`: `[chat]` defaults write (new `conductor::chat`, the same doctor-checked write as `cond`, now shared as `conductor::save`); `POST close`: `gh issue close` for a backlog issue only (`NOT_BACKLOG` otherwise), logged as `closed`
  - every POST takes a JSON object as well as the form body; all new POSTs are paired-device only (401 `UNPAIRED`)
- `api.rs`: new types `Roles`, `ChatDefaults`, `SetupScreen`; `assets/types/api.d.ts` regenerated.
- `crates/cli/tests/api_v1_contract.rs` + `tests/golden/api_v1/*.json` (one fixture per route: route, Rust type, schema generated from the type's serde default, recorded response). It fails on a key added/removed or a JSON type change against either the schema or the recorded response; `BLESS=1` rewrites them. `a_changed_shape_fails_the_contract` proves the rejection path.
- `crates/cli/tests/api_v1_ops.rs`: per route, the success path, the refusal codes and the unpaired 401.
- docs/serve.md: the API v1 table lists the new routes and the contract test.

## Decisions
- q641 (owner): no #496–#498 server halves here (accounts list, setup-token capture, DeepSeek keys, [use]); row 79 is only the existing setup terminal (start/read/type), promoted to v1. Accounts/keys/[use] follow cooee#30 in a later issue.
- Auth: the existing paired-device cookie. The code has no plugin bearer token (#345 shipped per-lane tokens for `herdsman service` only), so I added none.
- Not covered by the contract: `herdsman service`'s own `/api/v1/health|events|cmd` (a different server, lane tokens).
- `serde` added to cli's **dev**-dependencies (already a workspace dep; the deps gate reads `[dependencies]` only); Cargo.lock +1 line.

## Budget
src/ net +222 (no budget line in the issue): 27772 → 27994 / cap 31500.

## Evidence (head 096282f47b6863d96513013994a093ed68b18edb)
- `cargo build --release --locked`: exit 0
- `herdsman cap-check --config ci/cap.toml`: exit 0 (27994 / 31500)
- `cargo clippy --all-targets --locked -- -D warnings`: exit 0
- `cargo fmt --check`: exit 0
- `cargo test --locked`: every test binary built by `cargo test --no-run` was run directly (see below): 125 binaries, 124 exit 0 on the first pass. `ui_audit` got SIGTERM (exit -15) in that pass and was rerun alone: exit 0, 3 passed. `cargo test --doc`: exit 0.
- `cargo test -p herdsman-server --features ts export_bindings` + `serde_json_fits_the_types` (tsc 7): exit 0; `git diff` of assets/types is committed.
- html-lint, types-lint, harness-neutral grep, board-freeze script on the changed files: all clean.
- Deliberate shape change: `#[serde(rename = "msg")]` on `Reply.message` → `cargo test --test api_v1_contract` exit 101 with `post-mv (POST /api/v1/mv): .message: gone` / `.msg: new` / `route or type schema changed` for every POST (reverted).

Host note: the host's build trimmer (`check::trim_builds`) deletes a profile folder whose `.cargo-lock` no cargo holds. Cargo drops that lock while it runs test binaries, so a plain `cargo test` here lost its binaries partway through three times. I ran the built binaries directly while holding the lock. I didn't change that behaviour in this PR.
