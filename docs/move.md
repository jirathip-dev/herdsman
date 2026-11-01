# Moving herdsman to a new host

A move is copy, regenerate, then `herdsman doctor` until every line is ✓. Paths below are the defaults
(`FLEET_CONFIG`, `FLEET_STATE` and `FLEET_WORKTREES` override them).

## Before: finish the old host

- Let lanes finish, or push their work: `herdsman kill <agent> --yes` pushes a lane's unpushed or dirty
  work to `wip/<agent>-<stamp>`.
- `herdsman clean --yes` removes merged and closed lanes' worktrees.
- Stop the old watchdog and page so both hosts never drive the same fleet: remove the cron line and
  `launchctl bootout gui/$(id -u)/<label>` the serve job.

## Copy

| What | Path | Why |
|---|---|---|
| Config | `~/.config/herdsman/fleet.toml` | fleets, roles, notifiers, `[serve]` |
| Event log | `~/.local/state/herdsman/events.jsonl` | queue, lanes, verdicts, asks |
| Paired phones | `~/.local/state/herdsman/serve/devices.json` | the page's reply cookies (hashes only) |
| VAPID key | `~/.local/state/herdsman/webpush/vapid.key` | keep it and paired phones keep getting push |
| Push subscriptions | `~/.local/state/herdsman/webpush/subs.json` | the subscribed devices |
| Accounts file | `[accounts].accounts_env` | the tokens; keep it mode 0600 |

Copy the secret files with their mode (`scp -p` or `rsync -a`), never through chat or a repo, then
`chmod 600` the accounts file, `vapid.key`, `subs.json` and `devices.json` on the new host.

## Regenerate, don't copy

- `~/.config/herdsman/claude/` (the per-role Claude profiles) and the Hermes profiles' hooks:
  `herdsman profiles sync`.
- The launchd serve job: the plist in [setup.md](setup.md) under `~/Library/LaunchAgents`, then
  `launchctl bootstrap gui/$(id -u) <plist>`.
- `tailscale serve --bg --https=8443 http://127.0.0.1:7777`.
- The cron line from [setup.md](setup.md#4-watchdog): `herdsman check` (and `herdsman drive`).
- `gh auth login`, `herdr`, and each fleet's `local` checkout (`gh repo clone <repo> <local>`).

## Don't copy

- Worktrees (`~/.herdr/worktrees/`): their branches are on GitHub; new lanes make fresh ones.
- Lock files and `check.at` / `drive.at`: the new host writes its own.

## New hostname

The tailnet name changes with the host unless you keep it:

- Put the new `*.ts.net` name in `[serve].allowed_hosts`.
- Re-open the PWA from the new URL and re-add it to the Home Screen. A new origin is a new site to the
  phone: pair it again (`herdsman serve approve <code>`) and re-enable alerts. Keeping the hostname
  (rename the new machine to the old name in Tailscale) keeps pairing and push as they were.

## Check

Run `herdsman doctor` on the new host and fix each ✗ with the command it prints, until all lines are ✓
(a `!` is a warning: no phone paired yet, no push subscription, tailscale missing). `--json` gives the
same rows for a script.
