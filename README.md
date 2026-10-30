# herdsman

Small Rust CLI that owns fleet state, gates and the harness toggle over
[herdr](https://herdr.dev). **Agents act only through `herdsman`; `herdsman` owns state and
refuses anything that breaks a rule. Prose never enforces a rule.** Spec: issue #1.

```sh
cargo install --path .
cp fleet.example.toml ~/.config/herdsman/fleet.toml
herdsman profiles sync                       # per-role Claude Code folders + hook, Hermes profile hooks
herdsman set sendmeter impl --harness claude # switch (and back with --harness hermes; dsh: docs/dsh.md)
herdsman spawn sendmeter - orch              # the fleet's orchestrator lane (acts only via herdsman)
herdsman spawn sendmeter 12 impl             # worktree + pane + brief + verified kick
herdsman spawn sendmeter 12 rev
herdsman verdict sendmeter 12 PASS --sha <head> --file <rev worktree>/verdict.md
herdsman merge sendmeter 12                  # exact-head PASS + green CI + round limit
herdsman status
herdsman check                               # stall / lane-dead / ready-unmerged, once per state
herdsman protection                          # read-only: is branch protection on? exit 1 unless all are
herdsman clean [--yes]                       # reclaim merged or closed-issue lanes (dry run by default)
herdsman adopt sendmeter 12 impl --pane w9:p1 # record a lane already running in a pane
herdsman help --agent                        # the one usage doc agents get
herdsman queue sendmeter add 12 13 [--top]   # ordered queue (mv / rm / block --on N / ls)
herdsman next sendmeter                      # first unblocked queued issue with no lane
herdsman brief sendmeter 12                  # the issue spec cached at spawn
herdsman pr sendmeter 12 [--json]            # one cached gh pr view per 60 s
herdsman wait sendmeter 12 --until ci-green  # 0 reached · 1 failed/closed · 124 timeout
herdsman board                               # ≤ 60-col board for Telegram
herdsman serve --addr 127.0.0.1:7777         # read-only HTML view (GET only)
```

GitHub stays clean: issues are the spec, PRs the code. Lanes never comment (the Claude
and Hermes hook denies `gh issue|pr comment` and `gh api` comment writes); routing, queue, lane and
verdict state live in the event log and reach Telegram through the notifier. `herdsman merge`
writes the one closing comment (merge sha + verdict sha), then closes the issue's lane
panes and reclaims their worktrees; a lane with uncommitted or unpushed work is kept and
reported. Merging into `jirathip-dev/herdsman` itself ends with the `cargo install` line to
reinstall `herdsman` (printed, not run). Notifiers run without the
caller's `HERMES_*` environment. A merged issue stays `merged`; spawn/adopt on it is
refused with `MERGED`.

`herdsman spawn` refuses before creating anything: `HOST_BUSY` (memory pressure warn+,
swap-out rate or load/cpu over `[limits]`; `--force` overrides and logs an
`override` event), `LANE_LIVE` (a herdr agent in the worktree, or an unfinished lane in
the log) and `WORKTREE_DIRTY` (unpushed/uncommitted work from no known lane);
`--supersede` kills the old agent and logs it. If `herdr agent start` times out but the
harness came up in the pane, spawn names the agent and logs the lane anyway; if nothing
came up it closes the pane and logs nothing. `herdsman adopt` repairs a lane by hand.

`herdsman check` is idempotent and exits 0, so it can run from cron every 5 minutes:

```cron
*/5 * * * * ~/.cargo/bin/herdsman check >/dev/null 2>>~/Library/Logs/herdsman-check.log
```

| Path | What |
|---|---|
| `~/.config/herdsman/fleet.toml` (`FLEET_CONFIG`) | config: fleets, roles, limits, notifiers |
| `~/.local/state/herdsman/events.jsonl` (`FLEET_STATE`) | append-only event log; state and queue are folds over it |
| `~/.local/state/herdsman/cache/` | cached issue specs and PR/CI reads |
| `~/.config/herdsman/claude/<profile>/` | `settings.json` (PreToolUse hook), `role.md`, `fleet.md` |
| `~/.hermes/profiles/<profile>/config.yaml` (`FLEET_HERMES_PROFILES`) | sync rewrites its `hooks:` key (pre_tool_call hook) and drops `agent.reasoning_effort` (fleet.toml `effort` owns it) |
| `~/.config/herdsman/dsh/<profile>.patch.yml` | DSH patch layer for `harness = "dsh"` lanes ([docs/dsh.md](docs/dsh.md)) |
| `roles/`, `hooks/` | role prompts and hook settings, compiled into the binary |

Renamed from `fleet` (#27): only `herdsman migrate` moves `~/.config/fleet` and `~/.local/state/fleet`
to the paths above (both or neither; `HERDSMAN_HOME` overrides `$HOME` for it). Until then other
commands read the old dirs in place with a one-line stderr hint; `hook` stays silent. Lanes export `HERDSMAN_LANE`; `FLEET_LANE`
still arms the hook for one release, and `HERDSMAN_LANE` wins when both are set.

Hard cap: ≤ 5,000 lines of Rust under `src/` (tests live in `tests/`); CI enforces it.
