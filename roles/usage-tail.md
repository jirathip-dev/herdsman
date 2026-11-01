
## Decisions

Never hold silently. If you stop for any decision, run `herdsman ask` with ≤4 options and your recommendation first, then wait. Read a `DECISION` line as final. `--kind routine` (needs `--recommend`; unanswered for 30 min it takes your recommendation, `by: default`, still changeable): fix rounds, ordering, size or cap targets an issue already set, already-decided specs, test or tooling choices inside the issue's scope. `--kind owner` (the default) always waits: scope or feature changes, spend, security or secret trade-offs, anything touching main or production, data deletion, a new dependency.

## Issue states

queued → impl → review(n) → fix(n) → ready → merged; plus blocked, needs-human

## Refusal codes (exit 3, `REFUSED <CODE>: why`)

- MAIN_TARGET: integration or PR base is main/master; fleet never merges to main
- NO_PR: no open PR for the issue branch
- BASE_MISMATCH: PR base is not the fleet's integration branch
- NO_VERDICT: no review verdict recorded
- STALE_VERDICT: verdicts exist, but none for the PR's current head sha
- VERDICT_FAIL: latest verdict for the head sha is FAIL
- ROUND_LIMIT: review rounds exceed [limits].review_rounds (→ needs-human)
- CI_NOT_GREEN: a check at the head sha is missing, pending or failed
- BAD_SHA: verdict --sha is not 40 hex chars
- STALE_SHA: verdict --sha is not the branch head
- HOST_BUSY: spawn: memory pressure, swap-out or load/cpu over [limits]; --force overrides (logged). DISK_LOW: under [limits] min_free_gb free (default 10); `herdsman clean --yes`
- LANE_LIVE: spawn: a herdr agent is live in the worktree, or the log has an unfinished lane for it; --supersede
- WORKTREE_DIRTY: spawn: the worktree has unpushed or uncommitted work from no known lane; --supersede
- MERGED: spawn/adopt: the issue is merged; a merged issue never reopens
- SERIAL_BUSY: spawn: a `--serial` impl waits while the fleet's `serial_max` (default 1) other serial issues have a live impl/fix lane or sit at review/ready
- SERIAL_UNMARKED: merge: the PR touches the fleet's serial_paths but the issue was not queued `--serial`
- CAP: spawn: the fleet's optional max_impl / max_rev lane cap is reached (unset = no cap)
- PAUSED: spawn: the fleet is paused (`herdsman resume <fleet>`); impl/rev lanes wait, live lanes are untouched
- BAD_VALUE: set: a value is empty or has a quote/backslash/control char, or the model is not in `models`; ask: an empty question, over 4 options
- NO_SUCH_ASK / ALREADY_ANSWERED / NO_RECOMMENDATION / OWNER_TOPIC: answer: the fleet has no such ask id, or it was answered already (a `by: default` decision can be changed once); ask: `--kind routine` without `--recommend`, or with an owner word or its plural/inflection (main, prod, secret, token, delete, spend, cost, $, dependency, scope)
- STALE: serve: fleet.toml changed since the page was rendered; reload and retry
- NO_SUCH_AGENT / IS_LANE / SECRET_IN_WIP / SNAPSHOT_FAILED / UNSAFE_CLEAN: stop/kill: no such live lane; close: a logged lane (use stop/kill/clean); kill: a secret-looking value in the wip diff, or the wip snapshot failed (index restored, nothing pushed, pane stays); clean --lane: not merged/closed, dirty, unpushed or live
- MCP_FORBIDDEN: spawn/profiles sync: a role's `mcp` names a server matching `mcp_deny` (default `*prod*`)

## Exit codes

0 ok · 1 error (incl. merged but evidence comment missing) · 2 usage · 3 refused · 4 spawned but prompt not confirmed
`herdsman next`: 0 found · 1 empty · 2 top item blocked. `herdsman wait`: 0 reached · 1 failed/closed · 124 timeout.

## GitHub

The issue is the spec: read it with `herdsman brief`; PR/CI with `herdsman pr` / `herdsman wait` (cached), never
`gh issue view` / `gh pr checks` loops. Never comment on issues or PRs; `herdsman merge` writes the one closing comment.
UI PRs: confirm the `ui` job is green and open its `ui-390` screenshots. Any visual change not in the issue is a FAIL. Code: write normal code, `cargo fmt` before committing; the formatter decides line breaks, never pack lines to fit the cap.

## Tool guard, accounts, MCP

Lanes skip permission prompts; the Claude/Hermes hook blocks `gh pr merge`, `gh issue|pr comment`, `gh pr review`,
`gh api` comment writes, `git push` to main/master/staging/integration, edits to fleet, fleet-operations and Hermes
profile config. Reviewers are read-only: no edits but `verdict.md`, no `git commit|push|merge|rebase|reset`. `herdsman spawn … design` is owner-only: denied from every lane, orch included; never in verdict or merge.
Accounts (`herdsman accounts`): a Claude role's `account`/`accounts` exports the first uncapped one as `CLAUDE_CODE_OAUTH_TOKEN`;
an `account-capped` one waits `cap_reset_minutes` or `herdsman accounts reset <name>`. MCP: only the role's `mcp` (default none).

## Events

`herdsman check` emits `stall`, `lane-dead`, `ready-unmerged`, `account-capped` (limit banner), `orch-holding` (orch idle >15 min,
startable work, no open ask), `ask-waiting` (an owner ask open >30 min), a `by: default` decision for each routine ask open `ask_default_minutes` (30), `orphan-lane` (a herdr agent named like a lane with no spawn event; `spawn` adopts it) and `alerts-off` (page alerts, no webpush notifier) once per state; `orch-nudged` (an item waiting on an idle
orch >5 min: its pane gets the exact next command, once per issue, head and state), then `orch-stuck` 10 min later; `herdsman merge` emits
`evidence-missing` (issue left open) if the evidence comment fails twice. Notifiers get one plain line, or raw event JSON with `format = "json"`. A fleet's own `[[fleet.<name>.notify]]` (same fields)
replaces the global `[[notify]]` for its events; `notify_global = true` in `[fleet.<name>]` sends to both.
Owner-only: `herdsman pause|resume <fleet>` (PAUSED), `herdsman drive` (cron), `herdsman doctor` (host check) and `herdsman serve`, the phone page (its writes log `"via":"web"`).
