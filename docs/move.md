# Moving drover to a new host

A move is copy, regenerate, then `drover doctor` until every line is ✓. Paths below are the defaults
(`FLEET_CONFIG`, `FLEET_STATE` and `FLEET_WORKTREES` override them).

## Before: finish the old host

- Let lanes finish, or push their work: `drover kill <agent> --yes` pushes a lane's unpushed or dirty
  work to `wip/<agent>-<stamp>`.
- `drover clean --yes` removes merged and closed lanes' worktrees.
- Stop the old watchdog and page so both hosts never drive the same fleet: `drover timer remove`
  (or remove the old cron line) and `launchctl bootout gui/$(id -u)/<label>` the serve job.

## Copy

| What | Path | Why |
|---|---|---|
| Config | `~/.config/drover/fleet.toml` | fleets, roles, notifiers, `[serve]` |
| State database (#344) | `~/.local/state/drover/state.db` (copy it with drover stopped, or `sqlite3 state.db .backup`) | the event log (queue, lanes, verdicts, asks), paired phones (hashes only), push subscriptions |
| VAPID key | `~/.local/state/drover/webpush/vapid.key` | keep it and paired phones keep getting push |
| Secrets (#441) | the main host's OS store, `drover/<fleet>/<NAME>` | not a file: on the new host re-save secrets by request; accounts sign in with their own login (#683) |

Copy the secret files with their mode (`scp -p` or `rsync -a`), never through chat or a repo, then
`chmod 600` `vapid.key`, `subs.json` and `devices.json` on the new host.

## Regenerate, don't copy

- `~/.config/drover/claude/` (the per-role Claude profiles) and the Hermes profiles' hooks:
  `drover profiles sync`.
- `gh auth login`, shed (and its job), and each fleet's `local` checkout (`git clone https://github.com/<repo>.git <local>`).
- Then `drover setup` ([setup.md](setup.md#4-watchdog)): the tick and serve jobs, `tailscale serve`
  (with `[serve] tailscale_https_port` set) and the profiles, then doctor.

## Don't copy

- Worktrees (each workspace root): their branches are on GitHub; new lanes make fresh ones.
- Lock files, `check.at` / `drive.at` and `tick.log`: the new host writes its own.

## New hostname

The tailnet name changes with the host unless you keep it:

- Put the new `*.ts.net` name in `[serve].allowed_hosts`.
- Re-open the PWA from the new URL and re-add it to the Home Screen. A new origin is a new site to the
  phone: pair it again (`drover serve approve <code>`) and re-enable alerts. Keeping the hostname
  (rename the new machine to the old name in Tailscale) keeps pairing and push as they were.

## Check

Run `drover doctor` on the new host and fix each ✗ with the command it prints, until all lines are ✓
(a `!` is a warning: no phone paired yet, no push subscription, tailscale missing). `--json` gives the
same rows for a script.
