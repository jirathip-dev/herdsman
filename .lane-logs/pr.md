Closes #565

## For humans

A fleet can now run its lanes as ACP chat sessions in shed instead of terminal panes, so they can show up in cooee's Workflow list. You switch one fleet with a small config block, and every other fleet stays on herdr.
A lane only starts if herdsman's command guard is wired into it. Otherwise spawn refuses with a clear reason, and nothing is created.
The guard was checked live: in a real claude-acp session, a protected push, a pattern kill and a PR merge were all refused.

## What changed

- **Runtime** (`config.rs`): new kind `shed-acp`, which defaults `acp` to the first harness's hooked adapter (`claude-acp`) and `acp_policy = "allow"`. It can be set globally or per fleet as `[fleet.<n>.runtime]`. `Config::shed_for(fleet)` decides where a fleet's new lanes start. `Config::shed()` is the server lanes are reached on, so a fleet-only pilot is still listed, intaken and prompted.
- **Spawn** (`spawn.rs`): an ACP lane starts with `shed run --acp claude-acp --policy allow --name … --cwd … --label … -- sh -c '<lane exports> && . <secrets file> && unset SHED BERTH && exec "$@"' sh <installed adapter cmd>`. The adapter command is read from `shed agents --json`. The shell adds `CLAUDE_CONFIG_DIR=<synced profile>` and `ANTHROPIC_MODEL`. The account token reaches the adapter as `CLAUDE_CODE_OAUTH_TOKEN` through the existing 0600 secrets file, so it never appears in argv, labels, events or the brief. The **brief's text is the first prompt**, sent with `shed send`.
- **Labels** (amendment A): `source=drover`, `group=<repo name>`, `item=#<n>`, `role=impl|fix|rev|design`, `round=<n>`, `url=<issue URL>`, plus `drover.fleet/issue/role/round`. The shed lane labels (`fleet`, `issue`, `role`, `harness`) are kept.
- **Guard, fail closed**: spawn and its dry run refuse `ACP_UNGUARDED` before anything is made in these cases: an adapter not known to run the harness hook (only `claude` + `claude-acp` is known), a profile whose `settings.json` has no herdsman `PreToolUse` hook, a role with no account token, or a lane placed on another host. shed's `--policy` has no per-command rules, so there is no policy-file fallback; an unguardable lane simply doesn't start.
- **Profile** (`hooks/claude-settings.json`, `claude.rs`): `permissions.defaultMode = "bypassPermissions"` (the same as the pane's `--dangerously-skip-permissions`; the hook still runs first), and `CLAUDE.md` = role.md, because the adapter takes no `--append-system-prompt-file`.
- **Prompts** (`herdr.rs`): `shed send` framing and "no Enter" are now chosen by the terminal's own `acp` field, not the global config. So DECISION lines, nudges and board replies to an ACP lane go in as prompts.
- **State and transcripts**: unchanged code paths. The bridge reports `working`/`blocked`/`done` from ACP turns, the hook no longer reports state (`$SHED` is unset), and drive's intake logs those as `lane-state`. The lane sheet and `/api/v1` read the lane with shed `read`, which for an ACP terminal renders its ACP event history. Documented in `docs/runtimes.md`.

## Decisions (routine, mine)

- Every ACP lane needs the hook, whatever its `acp_policy`. The older unguarded `kind = "shed"` + `acp` mode now refuses as well (#508). No fleet uses it today.
- `group` is the repo's short name (`herdsman`). `role` is `fix` for an impl round after a FAIL.
- Not carried over to ACP lanes yet: the role's `effort` and its `mcp` servers (shed's bridge sends `session/new` with none). Both are documented.

## Evidence (head 78e23b1)

Fix round 2 (FAIL at ae517c0): a stale `evidence` check run from before the body edit stayed red in the rollup. A re-run replays the old body, so it can't clear it. No code change: I merged `origin/staging` (4a0629d) cleanly for a fresh head, and re-ran the gates at 78e23b1.

Fix round 1 (FAIL at 333b2f5, the CI step "harness-neutral core (#316)"): the ACP adapter id, its config-folder and model env, and the hooked-adapter table are now harness trait methods (`Harness::acp`, `Harness::acp_env`, `harness::default_acp`). `config.rs` and `spawn.rs` no longer name a harness. After #529, an ACP lane's exit shows as `gone`, and the state test expects that.

- CI's harness-neutral grep (`.github/workflows/ci.yml:48`): no output (grep exit 1, clean)
- `cargo fmt --check`: exit 0
- `cargo clippy --all-targets --locked -- -D warnings`: exit 0
- `cargo build --release --locked`: exit 0
- `herdsman cap-check --config ci/cap.toml`: exit 0, 30365 / 31500
- `cargo test --locked`: the full run can't finish on this host. Twice the shared build folder was deleted mid-run ("extern location … does not exist"), and the host killed background runs (exit 144). So I ran all 125 `crates/cli/tests` binaries in batches of 20 (`cargo test --locked -p herdsman --test …`), and six of the seven batches exited 0. The seventh exited 101 only because the shared target held another lane's `shed` binary: it ran `an_acp_lane_starts_and_is_prompted_through_sheds_cli`, a test this PR replaces. Rebuilt in an isolated `CARGO_TARGET_DIR`, `--test shed` exited 0 (16 passed). `-p herdsman-core -p herdsman-server -p herdsman-service --lib --tests`: exit 0. `chat_tab` failed one scroll assertion under full-suite load, then passed 21/21 on its own (exit 0); this PR doesn't touch it. The new tests are in `crates/cli/tests/shed.rs`: `a_shed_acp_fleet_starts_its_lane_as_an_acp_session_with_the_brief_as_its_first_prompt` (labels, brief as first prompt, DECISION delivered as a prompt), `a_guarded_command_is_refused_inside_an_acp_lane` (`git push origin staging`, `pkill x`, `gh pr merge` denied by the hook run in the adapter's real env), `an_unguardable_acp_lane_refuses_to_start`, and `an_acp_lanes_state_follows_its_turn_events`.
- Live guard proof (shed 0.0.0, claude-acp 0.101.0, this host): a scratch session with herdsman's hook as its project `PreToolUse` hook. All three commands ended `failed` with herdsman's refusal text, and `hook-denied` was logged. Details are in `docs/runtimes.md`.

## Not verified

- AC 3 (live impl → rev → merge with both lanes as ACP sessions, visible in cooee's Workflow chip) needs the owner to set `[fleet.herdsman.runtime] kind = "shed-acp"` and run the pilot.
- The live proof used project settings on the owner's default login. The `CLAUDE_CONFIG_DIR` user-settings path a lane uses needs an account token; spawn's wiring of it is covered by tests only.

src/ net +172 (no budget stated): a new runtime kind, labels, the fail-closed adapter wrapper, and the harness trait methods for ACP.


