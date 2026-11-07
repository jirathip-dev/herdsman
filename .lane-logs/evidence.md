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

