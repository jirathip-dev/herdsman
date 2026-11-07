
## Decisions

Decide routine things yourself and say what you decided in the PR body. `herdsman ask` is only for scope or feature changes, spend, security or secret trade-offs, anything touching main or production, data deletion, or a new dependency. Never hold silently. A `conflict-predicted` note means another open PR touches the same files: don't wait for it; finish, run the gates, push. If the other PR merges first, herdsman gives you an update round. Real ordering needs are `--serial` groups' job, never a lane's own wait. An issue's line budget is a guide, not a gate: never ask about it; state `src/ net +N (budget +M)` in the PR body with one line on why. The cap in `ci/cap.toml` is a total for the repo and the only hard limit: ask only when your own PR, merged onto current integration, would put the total over `line_cap`; trim first, then ask `--kind owner`. Being near the cap, or another PR's growth, is never a reason to ask. A `STAGING_WOULD_BREAK` from the cap after another merge is a fix round: trim, or ask under the same rule. If you stop for any decision, run `herdsman ask` with ≤4 options and your recommendation first, then wait. Read a `DECISION` line as final. `--kind routine` (needs `--recommend`; unanswered for 30 min it takes your recommendation, `by: default`, still changeable): fix rounds, ordering, already-decided specs, test or tooling choices inside the issue's scope. `--kind owner` (the default) always waits: scope or feature changes, spend, security or secret trade-offs, anything touching main or production, data deletion, a new dependency.

## Issue states

queued → impl → review(n) → fix(n) → ready → merged; plus blocked, needs-human
An impl, fix or update lane ends every turn with `herdsman turn done|blocked|need-input|working [--why <text>]`; a turn that ends without one is told once. Drive acts on it: `done` with a new push gets its review at once, `done` with nothing new pushed is nudged once and then blocked; `blocked` (`--why` required) is an owner card with the reason; `need-input` waits on the `herdsman ask` you file; `working` gets one "continue" prompt after the stall threshold, never a respawn.
An issue queued `--design` first goes queued → design → design-review: its design ask's `approve` lets impl start, `revise` (or a free-text note) redoes the design, `reject` reverts it (needs-human).
A fleet with `[fleet.<n>.role.verify]` adds a verify check after rev's PASS (state `verify(n)`, same round limit as rev): it drives the real app from the repo's `.herdsman/verify.md` and records PASS/FAIL with 1440/390 screenshots listed in `verdict.md`. It runs for a design-stage issue, a PR touching `ui_paths`, or `queue add N --verify` (`--verify off` / `queue set N verify on|off` force it); else `verify-skipped`, or `verify-no-recipe` without the recipe, and the issue goes on to the gates. An issue queued `--repro` (`queue set N repro on|off`) is a bug fix: impl's commit 1 is only the failing regression test, and rev's PASS `verdict.md` carries a `Repro: run` block (the test command fenced then its fenced failing output on the base plus commit 1, then the same at the head, passing) or, when the PR body's `## Repro` section says why there is no cheap test, a `Repro: verify` block quoting that reason with `>`; that forces the verify stage, whose PASS at the head the merge needs.

## Refusal codes (exit 3, `REFUSED <CODE>: why`)

- MAIN_TARGET: integration or PR base is main/master; fleet never merges to main
- NO_PR: no open PR for the issue branch
- BASE_MISMATCH: PR base is not the fleet's integration branch
- NO_VERDICT: no review verdict recorded
- STALE_VERDICT: verdicts exist, but none for the PR's current head sha
- VERDICT_FAIL: latest verdict for the head sha is FAIL
- ROUND_LIMIT: review rounds exceed [limits].review_rounds (→ needs-human). UPDATE_LIMIT: spawn --update after [limits].update_rounds (default 3) PASSed update rounds (→ needs-human)
- CI_NOT_GREEN: a check at the head sha is missing, pending or failed
- BAD_SHA: verdict --sha is not 40 hex chars
- STALE_SHA: verdict --sha is not the branch head. NO_PROOF: verdict FAIL: a `- blocking [<category>] file:line: what` finding lacks a `Proof:` block right under it: `Proof: run` (a fenced command, then its output; or a screenshot path) or `Proof: spec` (a `>` quoted AC, amendment or rule line, plus a `file:line`). Names each finding; notes and PASS are free-form. NO_REPRO: verdict PASS on a `--repro` issue: no `Repro:` block, a `Repro: run` without the fenced command and both fenced outputs (base plus commit 1 failing, then head), or a `Repro: verify` without a `>` quoted reason from the PR's `## Repro` section; merge: a `Repro: verify` PASS has no verify PASS at the head
- HOST_BUSY: spawn: memory pressure, sustained swap-out (with pressure) or load/cpu over [limits]; --force overrides (logged). MEM_LOW: under [limits] min_free_mem_gb memory free (default 2). DISK_LOW: under [limits] min_free_gb free (default 10); `herdsman clean --yes`. DISK_BUDGET: worktrees over [limits] worktree_budget_gb (default 25) after drive freed the idlest lanes' build folders. HOST_OFFLINE: the lane's host is unreachable; its lanes show `host offline` and wait for it
- LANE_LIVE: spawn: a herdr agent is live in the worktree, or the log has an unfinished lane for it; --supersede
- WORKTREE_DIRTY: spawn: the worktree has unpushed or uncommitted work from no known lane; --supersede
- MERGED: spawn/adopt: the issue is merged; a merged issue never reopens. NOT_NEEDS_HUMAN: extend/park: the issue is not needs-human (once per episode)
- SERIAL_BUSY: spawn: a `--serial` impl waits while the fleet's `serial_max` (default 1) other serial issues have a live impl/fix lane or sit at review/ready. IN_FLIGHT / ALREADY_DONE: spawn/merge: the same step (#509: its `action_id`) is in flight in another live pass, or done; skipped
- SERIAL_UNMARKED: merge: the PR touches the fleet's serial_paths but the issue was not queued `--serial`. STAGING_WOULD_BREAK / STAGING_MOVED: merge: a `premerge` check fails on the PR merged with current integration (→ fix round, not counted) / integration moved during the checks twice; retried next pass
- CAP / ALL_CAPPED: spawn: the fleet's optional max_impl / max_rev lane cap is reached (unset = no cap) / every account of the role is capped (names the first reset)
- PAUSED: spawn: the fleet is paused (`herdsman resume <fleet>`); impl/rev lanes wait, live lanes are untouched. NO_STORE: serve: the config names a secret but the main host has no usable OS secret store (Keychain, Secret Service, Credential Manager); there is no plain-file fallback
- BAD_VALUE: set: a value is empty or has a quote/backslash/control char, or the model is not in `models`; ask: an empty question, over 4 options
- NO_SUCH_ASK / ALREADY_ANSWERED / NO_RECOMMENDATION / OWNER_TOPIC: answer: the fleet has no such ask id, or it was answered already (a `by: default` decision can be changed once); ask: `--kind routine` without `--recommend`, or with an owner word or its plural/inflection (main, prod, secret, token, delete, spend, cost, $, dependency, scope). OWNER_ASK: a conductor's answer to an ask that is no lane ask on its repo, names an owner word, or has no `--basis` quoting the spec or naming a decision (#541: a lane's ask goes to its repo's conductor thread first). NOT_TYPED: `answer --typed`: the line is no owner message in the ask's thread after the ask. NO_DECISION: amend: `--basis` is no ask on that issue answered by the owner (or a routine default)
- STALE: serve: fleet.toml changed since the page was rendered; reload and retry
- NO_SUCH_AGENT / IS_LANE / SECRET_IN_WIP / SNAPSHOT_FAILED / UNSAFE_CLEAN: stop/kill: no such live lane; close: a logged lane (use stop/kill/clean); kill: a secret-looking value in the wip diff, or the wip snapshot failed (index restored, nothing pushed, pane stays); clean --lane: not merged/closed, dirty, unpushed or live
- DESIGN_MAIN_ONLY: spawn: a design lane runs on the main host only; `--host` or a role `host` pin named another
- MCP_FORBIDDEN: spawn/profiles sync: a role's `mcp` names a server matching `mcp_deny` (default `*prod*`). ACP_UNGUARDED: spawn: an ACP lane (`shed-acp`) herdsman's hook would not guard (an adapter that runs no hook, no synced hook, no account token, another host) does not start. SHED_MISSING: spawn: a shed runtime's `shed` is not on the spawning PATH (drive's is the timer's); nothing is made

## Lessons

`herdsman lesson <fleet>` (conductors and the owner only; a conductor runs it when the owner says "correct") files `lessons: <fleet> week of <date>`, queues it on top and spawns a `lesson` lane (the impl role's harness unless `[fleet.<n>.role.lesson]` sets one). `herdsman tick` runs it every 7 days per fleet: `lessons = "weekly"` (default) or `"off"` in `[fleet.<n>]`. Any lane may read `herdsman lesson digest <fleet> [--days 7]`. #510: a lesson check starts in shadow (`shadow since <date>` on its rules line: reported, never failing); `herdsman lesson apply <fleet>` in the lesson lane promotes or retires it by its history, never touching hand-written lines.
A conductor may run `herdsman timer install` (only that host verb; logged `timer-install`, `by` its lane) when `herdsman doctor` reports the timer outdated or missing; any other lane is `REFUSED CONDUCTOR_ONLY`.
`.herdsman/rules.md` in a repo: at most 30 "don't X" lines (CI's cap check fails over that), each naming its enforcer or `none`; every impl and rev brief carries it. Follow it.

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
profile config. Reviewers are read-only: no edits but `verdict.md`, no `git commit|push|merge|rebase|reset`. Design lanes are owner-only: started by the owner or a conductor thread (`herdsman spawn … design`), never by a lane; never in verdict or merge.
The hook also refuses `deny_commands` (default: containers: docker, colima, orb, podman, `supabase start`… → `REFUSED NO_CONTAINERS`) and, for every lane, `pkill`, `killall` and `pgrep … | xargs kill` (`REFUSED NO_BROAD_KILL`); a call it can't judge (input not JSON, fleet.toml not loading, an unknown fleet) `REFUSED HOOK_FAILED_CLOSED`.
Accounts (`herdsman accounts`): a Claude role's `account`/`accounts` exports the first uncapped one as `CLAUDE_CODE_OAUTH_TOKEN`;
an `account-capped` one is skipped until the banner's reset time (else `cap_reset_minutes`, a day if weekly) or `herdsman accounts reset <name>`. Secrets (tokens, `secret:<NAME>` env) live only in the main host's OS store as `herdsman/<fleet>/<NAME>`; spawn writes a lane's to a 0600 file beside its worktree (over ssh stdin to a remote host), the pane's shell sources it, spawn deletes it. `herdsman accounts import [tokens.env]` (owner only) copies the old file's tokens in, checked by sha256. MCP: only the role's `mcp` (default none).

## Events

`herdsman check` emits `stall`, `lane-dead`, `ready-unmerged`, `ci-missing` (a PR head with no checks `ci_start_minutes` (10) after its push, once per head), `account-capped` (limit banner), `ask-waiting` (an owner ask open >30 min), a `by: default` decision for each routine ask open `ask_default_minutes` (30), `orphan-lane` (a herdr agent named like a lane with no spawn event; `spawn` adopts it), `outbox-stale` (a remote lane's outbox line undelivered for 5 min) and `alerts-off` (page alerts, no webpush notifier) once per state; `herdsman merge` emits
`evidence-missing` (issue left open) if the evidence comment fails twice. Notifiers get one plain line, or raw event JSON with `format = "json"`. A fleet's own `[[fleet.<name>.notify]]` (same fields)
replaces the global `[[notify]]` for its events; `notify_global = true` in `[fleet.<name>]` sends to both.
Owner-only: `herdsman pause|resume <fleet>` (PAUSED), `herdsman drive`, `herdsman setup|tick`, `herdsman timer` (the 5-minute job; `timer install` a conductor's too), `herdsman doctor` (host check), `herdsman reclaim` (idle worktrees' build folders), `herdsman host destroy <name>` (a cloud host, after a wip save), `herdsman serve`, the phone page (its writes log `"via":"web"`), and `herdsman report [id]`, its bug reports.
