# dsh lanes (beta, reviewer pilot)

`harness = "dsh"` runs [dsh-TUI](https://github.com/ccch1mneyyy/dsh-TUI) (third-party,
public beta) over DeepSeek Harness in a lane pane. Only the **reviewer** role is
piloted; no dsh orchestrator or implementer until that passes.

```toml
[fleet.fleet.role.rev]
harness = "dsh"
profile = "fleet-rev"
```

- `herdsman profiles sync` writes `~/.config/herdsman/dsh/<profile>.patch.yml`, a DSH patch
  layer: reviewers get the `workspace-write` sandbox, other roles `danger-full-access`;
  approvals are `never` (an unattended lane cannot answer them).
- `herdsman spawn` refuses unless `dsh-tui version` reports profile `0.17.0`
  (`harness::DSH_TUI_VERSION`). It types `dsh-tui --patch <file>` into the lane shell
  (herdr has no `dsh` agent kind), waits until herdr shows the pane as agent `dsh-tui`
  with state `idle` (dsh-TUI reports its own state to herdr), presses Esc if the
  first-run wizard is up, names the agent, types the kick with `herdr pane send-text` +
  Enter (`herdr agent prompt` refuses agents herdr did not start) and confirms by
  `working`, `blocked` or `done`, retrying up to 3 times.
- dsh-TUI's own state reports can drop the herdr agent name (seen once in the pilot;
  the final pilot lane kept it), so `herdr agent list` may show the lane as an unnamed
  `dsh-tui` agent; `herdsman check` then matches it by the lane's spawn pane.
- Before launch, spawn installs a git `pre-push` hook for an issue lane's worktree only
  (`extensions.worktreeConfig` + `core.hooksPath` in the worktree's config) that runs
  `herdsman hook --role <role> --git-pre-push`. It refuses pushes to main, master, staging and
  every integration branch, and every push from a reviewer lane.

## What dsh lanes can't enforce, compared with claude/hermes

DSH 0.2.1-rc.3 / dsh-TUI 0.17.0 document no pre-tool hook, so the Claude
`PreToolUse` / Hermes `pre_tool_call` hook (`herdsman hook`) never sees a dsh tool call.

| Rule | claude / hermes | dsh |
|---|---|---|
| `git push` to main/master/staging/integration | hook denies the command | `pre-push` hook refuses; bypassed by `git push --no-verify`, by `-c core.hooksPath=…`, or by pushing from another checkout |
| Reviewer: no `git push` | hook denies | `pre-push` hook refuses (same bypasses) |
| Reviewer: no `git commit/merge/rebase/reset/…` | hook denies | **not enforced** by herdsman; the `workspace-write` sandbox should keep the worktree's git dir (outside the worktree) unwritable, which is not verified |
| Reviewer: no edits except `verdict.md` | hook denies | **not enforced** inside the worktree; the sandbox only stops writes outside it |
| `gh pr merge`, `gh api …/merge` | hook denies | **not enforced** by herdsman; `herdsman merge` stays the only gate herdsman honours, and GitHub branch protection is the backstop |
| `gh issue/pr comment`, `gh pr review`, `gh api` comment writes | hook denies | **not enforced** |
| Edits to fleet config, the event log, fleet-operations, Hermes profiles | hook denies | **not enforced** for non-reviewer roles (`danger-full-access`); reviewers' sandbox keeps them outside the writable root |
| Hook inert outside fleet lanes (`HERDSMAN_LANE`) | yes | n/a: the `pre-push` hook only exists in fleet lane worktrees and always applies there |

Server-side protection is the real backstop. As of 2026-10-30, `jirathip-dev/sendmeter`
has no branch protection or rulesets on main or staging, and `jirathip-dev/herdsman` is
private on a plan without branch protection (HTTP 403), so neither repo enforces this on
the server yet. Tracked in [#34](https://github.com/jirathip-dev/herdsman/issues/34).
