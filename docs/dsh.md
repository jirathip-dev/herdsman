# dsh lanes (beta, reviewer pilot)

`harness = "dsh"` runs [dsh-TUI](https://github.com/ccch1mneyyy/dsh-TUI) (third-party,
public beta) over DeepSeek Harness in a lane pane. Only the **reviewer** role is
piloted; no dsh implementer until that passes.

```toml
[fleet.app.role.rev]
harness = "dsh"
profile = "lane-rev"
```

- `drover profiles sync` writes `~/.config/drover/dsh/<profile>.patch.yml`, a DSH patch
  layer: reviewers get the `workspace-write` sandbox, other roles `danger-full-access`;
  approvals are `never` (an unattended lane cannot answer them).
- `drover spawn` refuses unless `dsh-tui version` reports profile `0.17.0`
  (`providers::DSH_TUI_VERSION`). It types `dsh-tui --patch <file>` into the lane shell,
  waits until the pane shows agent `dsh-tui` with state `idle` (dsh-TUI reports its own
  state), presses Esc if the first-run wizard is up, types the kick with `pane send-text` +
  Enter and confirms by
  `working`, `blocked` or `done`, retrying up to 3 times.
- dsh-TUI's own state reports can drop the agent name (seen once in the pilot;
  the final pilot lane kept it), so the agent list may show the lane as an unnamed
  `dsh-tui` agent; `drover check` then matches it by the lane's spawn pane.
- Before launch, spawn installs a git `pre-push` hook for an issue lane's worktree only
  (`extensions.worktreeConfig` + `core.hooksPath` in the worktree's config) that runs
  `drover hook --role <role> --git-pre-push`. It refuses pushes to main, master, staging and
  every integration branch, and every push from a reviewer lane.

## What dsh lanes can't enforce, compared with claude/hermes

DSH 0.2.1-rc.3 / dsh-TUI 0.17.0 document no pre-tool hook, so the Claude
`PreToolUse` / Hermes `pre_tool_call` hook (`drover hook`) never sees a dsh tool call.

| Rule | claude / hermes | dsh |
|---|---|---|
| `git push` to main/master/staging/integration | hook denies the command | `pre-push` hook refuses; bypassed by `git push --no-verify`, by `-c core.hooksPath=…`, or by pushing from another checkout |
| Reviewer: no `git push` | hook denies | `pre-push` hook refuses (same bypasses) |
| Reviewer: no `git commit/merge/rebase/reset/…` | hook denies | **not enforced** by drover; the `workspace-write` sandbox should keep the worktree's git dir (outside the worktree) unwritable, which is not verified |
| Reviewer: no edits except `verdict.md` | hook denies | **not enforced** inside the worktree; the sandbox only stops writes outside it |
| `gh pr merge`, `gh api …/merge` | hook denies | **not enforced** by drover; `drover merge` stays the only gate drover honours, and GitHub branch protection is the backstop |
| `gh issue/pr comment`, `gh pr review`, `gh api` comment writes | hook denies | **not enforced** |
| Edits to fleet config, the event log, fleet-operations, Hermes profiles | hook denies | **not enforced** for non-reviewer roles (`danger-full-access`); reviewers' sandbox keeps them outside the writable root |
| `deny_commands` (#216: containers by default) | hook denies | **not enforced** at run time; the role prompt states it, and the `pre-push` hook refuses a push adding a script line that runs one (`.github/workflows` excluded; same bypasses) |
| Hook inert outside fleet lanes (`DROVER_LANE`) | yes | n/a: the `pre-push` hook only exists in fleet lane worktrees and always applies there |

Server-side protection is the real backstop; turn it on for every fleet repo's main and staging.

`drover protection` re-runs this measurement: for each fleet repo it reads
`gh api repos/<repo>/branches/<branch>/protection` for `main`, `staging` and the integration
branch and prints `✓ protected` only when both required status checks and required PR reviews
are on. Protection with only one or neither of them is `△ protected but incomplete: missing …`.
It also prints `✗ not protected` or `✗ unavailable: <reason> (HTTP <status>)`, plus a
`⚠ enforce_admins off` warning line, which does not fail the run. It skips a branch that does
not exist, but a repo with no branch verified fails. It exits 1 unless every existing branch is
fully protected. It runs on demand only (never
from `drover check`). **drover only verifies; it never enables protection.** That is
the owner's step. Paste this once per repo and branch, with `<check>` set to the CI check name
(`rust` for drover):

```sh
gh api -X PUT repos/<owner>/<repo>/branches/<branch>/protection --input - <<'JSON'
{"required_status_checks": {"strict": true, "contexts": ["<check>"]},
 "enforce_admins": true,
 "required_pull_request_reviews": {"required_approving_review_count": 0},
 "restrictions": null}
JSON
```

Until the owner enables protection (PR + CI check required on `staging` and `main`) or
records risk acceptance, "`drover merge` is the only path to staging" is a convention
for dsh lanes, not an enforced rule, so dsh stays reviewer-only. Tracked in
[#4](https://github.com/jirathip-dev/herdsman/issues/4).
