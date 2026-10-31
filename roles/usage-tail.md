
## Exit codes

0 ok · 1 error (incl. merged but evidence comment missing) · 2 usage · 3 refused · 4 spawned but prompt not confirmed
`herdsman next`: 0 found · 1 empty · 2 top item blocked. `herdsman wait`: 0 reached · 1 failed/closed · 124 timeout.

## GitHub

The issue is the spec: read it with `herdsman brief`; PR/CI with `herdsman pr` / `herdsman wait` (cached), never
`gh issue view` / `gh pr checks` loops. Never comment on issues or PRs; `herdsman merge` writes the one closing comment.

## Tool guard, accounts, MCP

Lanes skip permission prompts; the Claude/Hermes hook blocks `gh pr merge`, `gh issue|pr comment`, `gh pr review`,
`gh api` comment writes, `git push` to main/master/staging/integration, edits to fleet, fleet-operations and Hermes
profile config. Reviewers are read-only: no edits but `verdict.md`, no `git commit|push|merge|rebase|reset`. `herdsman spawn … design` is conductor-only: denied from every lane, orch included; never in verdict or merge.
Accounts (`herdsman accounts`): a Claude role's `account`/`accounts` exports the first uncapped one as `CLAUDE_CODE_OAUTH_TOKEN`;
an `account-capped` one waits `cap_reset_minutes` or `herdsman accounts reset <name>`. MCP: only the role's `mcp` (default none).

## Events

`herdsman check` emits `stall`, `lane-dead`, `ready-unmerged`, `account-capped` (limit banner) once per state; `herdsman merge` emits
`evidence-missing` (issue left open) if the evidence comment fails twice. Notifiers get one plain line, or raw event JSON with `format = "json"`. A fleet's own `[[fleet.<name>.notify]]` (same fields)
replaces the global `[[notify]]` for its events; `notify_global = true` in `[fleet.<name>]` sends to both.
Owner-only: `herdsman pause|resume <fleet>` (PAUSED) and `herdsman serve`, the phone page (its writes log `"via":"web"`).
