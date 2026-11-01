# herdsman

A small Rust CLI that runs a fleet of coding agents over [herdr](https://herdr.dev):
one implementer and one reviewer per issue, and a merge only when the gates pass.

## Why

If the rules live in a prompt, sooner or later an agent talks its way past them. herdsman
keeps the state in an append-only event log and owns every step that matters: spawn,
verdict, merge. Agents act only through it, and it refuses anything that breaks a rule.

## Quick start
```sh
cargo install --path .
mkdir -p ~/.config/herdsman
cat > ~/.config/herdsman/fleet.toml <<'EOF'
[fleet.app]
repo = "you/app"
local = "~/src/app"
integration = "staging"

[fleet.app.role.impl]
harness = "claude"
profile = "lane-impl"
EOF
herdsman profiles sync                # per-role Claude Code folders and the lane hook
herdsman spawn app 12 impl --dry-run  # print the plan; drop --dry-run to launch it
herdsman status
herdsman serve --addr 127.0.0.1:7777  # phone page at http://127.0.0.1:7777
```
Full setup, from install to the board on your phone over Tailscale: [docs/setup.md](docs/setup.md).

## Commands

| Command | What it does |
|---|---|
| `spawn <fleet> <issue> <role>` | worktree + herdr pane + brief, launches the role's harness |
| `adopt` | record a lane already running in a pane |
| `verdict <fleet> <issue> PASS\|FAIL` | record a review verdict pinned to the exact head sha |
| `merge <fleet> <issue>` | merge into the integration branch if every gate passes |
| `status` / `board` / `serve` | per-issue state; compact board; phone page / PWA ([docs](docs/serve.md)) |
| `pause` / `resume` | hold a fleet: `next` offers nothing, impl/rev spawns refuse `PAUSED` |
| `queue` / `next` | ordered work queue; the first issue ready to start |
| `brief` / `pr` / `wait` | cached issue spec; cached PR and CI; wait for CI or merge |
| `check` / `ask` / `answer` / `asks` | alerts (stall, dead lane, unmerged ready, silent orch hold); ask for a decision, answer it |
| `clean` / `stop` / `kill` / `close` | reclaim merged or closed lanes; Esc an agent; close its pane, unpushed work to `wip/` first (dry runs unless `--yes`); close a non-lane agent's pane |
| `set` / `profiles` | switch a role's harness or model; write per-role folders and hooks |
| `accounts` | Claude accounts: cap, usage, lane-hours, lanes; DeepSeek balance ([docs](docs/providers.md)) |
| `protection` | is branch protection on for every fleet repo? |
| `migrate` | move pre-rename `fleet` config and state to the herdsman paths |
| `help --agent` | the usage doc agents get; refusals exit 3 with `REFUSED <CODE>: why` |

## Rules it enforces

- **Exact-head review.** `merge` needs a PASS verdict and green CI for the PR's current
  head sha. A new commit makes the old verdict stale. A fleet's `required_checks` narrows
  "green CI" to those check names, latest run of each.
- **Round limit.** At most 3 review rounds (`[limits].review_rounds`), then `needs-human`.
- **Host-load admission.** `spawn` refuses with `HOST_BUSY` under memory pressure, swap-out
  or high load. `--force` overrides and is logged. `DISK_LOW` under `[limits] min_free_gb` free.
- **Protected branches.** Merges go only to the integration branch, never main.
  `herdsman protection` checks that GitHub protection is on.
- **The lane hook.** Every lane runs a pre-tool hook that blocks `gh pr merge`, issue and PR
  comments, and pushes to main, staging or the integration branch. Reviewers are read-only.

## Config

`~/.config/herdsman/fleet.toml` (or `FLEET_CONFIG`); every key is in [fleet.example.toml](fleet.example.toml).

```toml
[fleet.app]
repo = "<owner>/<repo>"
local = "~/src/app"
integration = "staging"           # merge target; main never is

[fleet.app.role.impl]
harness = "claude"                # claude | hermes | dsh
profile = "lane-impl"
model = "opus"

[[notify]]
cmd = "notify <chat>"                # any command; one line on stdin
on = ["stall", "lane-dead", "merged", "needs-human"]
```

More: [architecture](docs/architecture.md), [lanes and hooks](docs/lanes.md), [dsh](docs/dsh.md), [phone page](docs/serve.md), [providers and keys](docs/providers.md).

## License

MIT, see [LICENSE](LICENSE).
