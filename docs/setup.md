# Full local setup

From zero to the board as an app on your phone's Home Screen. Every step ends with a check.
Placeholders: `<owner>`, `<repo>`, `<home>` (your home directory), `<machine>`, `<tailnet>`,
`<chat>`. Swap in your own values; keep them out of anything you commit.

## 1. Install

You need `git`, `gh` (logged in), shed (its server job running, [runtimes](runtimes.md)), a Rust toolchain, and
at least one harness: Claude Code and/or Hermes. Every lane terminal call goes through `crates/core/src/term.rs`, the
one adapter.

```sh
gh auth status
shed --version
cargo build --release --locked -p drover
# macOS (#781): sign with one local identity, so a Keychain item's access list trusts every build, not one.
sh scripts/signing-identity.sh   # once: creates "drover-local", lets codesign use it (asks the login password)
# #968: sign a copy beside the installed file, then rename it over: a running serve keeps its old file.
b=~/.local/share/drover/bin; mkdir -p "$b"; n=target/release/drover; cp "$n" "$b/drover.new"
sh scripts/signing-identity.sh sign "$b/drover.new" && mv "$b/drover.new" "$b/drover"   # fails, never hangs
echo 'export PATH="$HOME/.local/share/drover/bin:$PATH"' >> ~/.zshrc   # or ~/.bashrc
```

Check it worked: open a new shell and run `drover --version`. Once the config exists (step 2 on), reinstall with
`sh scripts/install-gate.sh target/release/drover ~/.local/share/drover/bin/drover`: it also refuses a build that
can't load the live config.

### Signed builds (#781)

macOS trusts a Keychain item's reader by its designated requirement. `cargo install` signs ad hoc (`cdhash H"…"`, new
every build), so each build was a new app and "Always Allow" came back. Signed with "drover-local" the requirement is
`identifier drover and certificate leaf = H"<the cert's SHA-1>"`, the same for every build. The self-update job, the
install gate and the reinstall line `drover merge` prints all sign; with no identity (or a codesign stuck on a
dialog, killed after 10 s) they keep the installed binary. `doctor`'s `signature` row names the identity and is ✗ (exit
1) for an unsigned build. drover itself keeps no Keychain item (#707); shed and cooee sign with the same identity.

The check, run on the Mini at #781 (b1 the installed staging build, b2 this branch's, u b2 unsigned; a scratch keychain
holds an item created with `security add-generic-password -T b1`):

```text
b1 designated => identifier drover and certificate leaf = H"412eb86c9f99cd0f5277ec9c2c0400e59534cc7c"
b2 designated => identifier drover and certificate leaf = H"412eb86c9f99cd0f5277ec9c2c0400e59534cc7c"
u # designated => cdhash H"4835b3c6a0856e5f722d045f7a9454a76e793c51"
stored requirement: identifier drover and certificate leaf = H"412eb86c9f99cd0f5277ec9c2c0400e59534cc7c"
codesign -v -R="$REQ" b1 → exit 0;  b2 → exit 0;  u → exit 3
✓ signature — b2 signed by "drover-local"
✗ signature — u signed by "drover-local" → sh scripts/signing-identity.sh … sign u
```

A program reading that item with Keychain dialogs off (`SecKeychainSetUserInteractionAllowed(false)`,
`SecKeychainFindGenericPassword`), built twice and signed the same way, read it both times with no prompt; the ad hoc
build was refused (`-25293`, errSecAuthFailed).

## 2. Config

One repo as fleet `app`, three roles. Save it as `~/.config/drover/fleet.toml`:

```toml
models = ["opus", "sonnet"]

[fleet.app]
repo = "<owner>/<repo>"
local = "~/src/<repo>"
integration = "staging"                      # merge target; main never is
required_checks = ["test"]                   # gate on these checks only; unset = all
merge_label = "merge-ready"                  # the full CI runs only with this label; drover adds it on a PASS at the head

[[fleet.app.serial]]                         # one-at-a-time paths (docs/config.md); any number of groups
name = "db"                                  # its Queue mark, 🔒db; ≤ 8 characters
paths = ["db/migrations/**"]
max = 1                                      # issues in flight at once; default 1

[fleet.app.role.impl]
harness = "claude"
profile = "lane-impl"
model = "opus"
effort = "high"                              # none | low | medium | high | xhigh
mcp = ["<dev-server>"]                       # servers from the repo's .mcp.json; default none

[fleet.app.role.rev]
harness = "claude"
profile = "lane-rev"
model = "sonnet"
```

Then write the per-role folders and the lane hook:

```sh
drover profiles sync
```

Check it worked: `drover status` prints no error. Every other key is in
[fleet.example.toml](../fleet.example.toml). Claude accounts sign in with their own login folders, and secrets live in
0600 files, never in fleet.toml: see [accounts.md](accounts.md) and [providers.md](providers.md).

`merge_label` goes with the affected-only CI recipe in [ci.md](ci.md): pull requests run only the checks the change
can affect, and the full run starts when drover adds the label. `drover setup` creates the label in the repo.

## 3. First lane

```sh
drover queue app add 12
drover drive app
drover status
```

`drover drive` starts the queue's issues and moves each lane on; the watchdog ([4](#4-watchdog))
runs it every 5 minutes. Check it worked:
`drover status` shows #12 moving past `queued`, and `drover board` shows its lane.

## 4. Watchdog

One command, safe to run again: `drover setup`. It writes and loads the tick job and the board
job ([6](#6-board-on-the-phone-tailscale)), sets up `tailscale serve`, runs `drover profiles sync`,
plus the self-update and daily clean jobs ([below](#self-update-and-the-daily-clean-557)), makes sure each fleet's
`merge_label` exists in its repo ([ci.md](ci.md)), then runs
`drover doctor` and exits with its code. Each step prints `✓ done`, `= already` or
`✗ <error> → <fix>`; `--no-load` writes the jobs without loading them.

Run it every 5 minutes: `drover timer install`. That is the tick job alone: `drover tick` every
5 minutes runs `drover check` (alerts once per stall, dead lane or unmerged ready issue), then
`drover drive` (every fleet's loop, [lanes.md](lanes.md#drive)). No cron and
no Hermes needed.

- macOS: `~/Library/LaunchAgents/dev.drover.tick.plist` (`--label` picks another label),
  `StartInterval 300`, `RunAtLoad`, loaded with `launchctl bootstrap gui/$(id -u) <plist>`.
- Linux: `~/.config/systemd/user/dev.drover.tick.{service,timer}`, loaded with
  `systemctl --user enable --now dev.drover.tick.timer`.
- Its PATH is explicit: the directories of the harness binaries, `gh`, `shed` and drover, then
  `/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin` (`sysctl` and `vm_stat` live in `/usr/sbin`).
- Both streams go to `~/.local/state/drover/tick.log`; the tick empties it past 1 MB.
- A refusal (`HOST_BUSY`, …) is logged and the tick exits 0. A real error (config, shed missing)
  logs `tick-failed`, which notifiers send at most once an hour. A red row in the Host tab's SERVICES
  group sends one `service-red` an hour too.

`drover timer status` prints loaded or not and the last tick. Undo: `drover timer remove`
(unloads it and deletes its files). Check it worked: `drover doctor` shows `✓ timer` and a
`watchdog check` that ran under 15 minutes ago.

### Self-update and the daily clean (#557)

drover needs no Hermes: `drover setup` (or `timer install`) also writes two jobs of its own.

- `dev.drover.update` runs `drover self-update` every 5 minutes. It fetches `[self_update] branch` (default
  `main`, #894; `"staging"` for a dev box) of the drover fleet (the fleet whose repo is `…/drover`) and installs a
  new head only once its CI is all green: it builds in `<state dir>/update/src` (a worktree of the fleet's `local`),
  never over the live binary, runs the new binary's `doctor --config-only` against the live fleet.toml (#539), swaps
  it in atomically, then runs `profiles sync` and hands serve over: the running serve starts the new binary on its
  listening sockets (the board's and the webhook port), stops accepting once that one is warm, closes each `/live`
  stream (the page reconnects with `?since=<seq>` and misses no event), drains its requests and execs the new binary
  in place, so launchd keeps its pid. A serve that does not answer on `<state dir>/serve-<port>.sock` (an older
  build) is restarted instead. Any failed step keeps the old binary and alerts `self-update-failed` once per head,
  with the exact failure. `drover self-update --dry-run` prints what it would do; `--pause` / `--resume` toggle it
  (logged). The job's PATH includes `cargo`'s directory.
- `[self_update] staging_serve = true` (off by default) also installs the green staging head into
  `<state dir>/update-staging/bin/drover` and runs it as `dev.drover.serve-staging`: `serve --read-only` on
  `staging_addr` (default `127.0.0.1:7787`) against the live config and state, every write refused, to try a change
  before promoting it.
- `dev.drover.clean` runs `drover clean --scheduled` daily at 03:30: worktrees of merged or closed issues idle
  3 days or more, at most 10 a run, never a live one; unsaved work goes to `wip/` first, as `clean --yes`. A refusal or failure alerts `clean-failed` once; a clean
  no-op is silent.

Moving off Hermes: once both jobs have run (`self-updated` in the log after a staging merge),
remove the old Hermes fleet jobs, once:

```sh
hermes -p orchestrator cron remove d8bbecd490d7   # fleet-check (auto-install)
hermes -p orchestrator cron remove 064cdd00dc56   # the old worktree-prune job
```

Other Hermes cron jobs (a one-off reminder) stay. `scripts/install-gate.sh` stays for a manual install.

## 5. Alerts

A notifier is any command that reads one line on stdin. Global ones get every fleet's
events; a fleet's own list replaces them for that fleet.

```toml
[[notify]]
cmd = "notify-telegram <chat>"
on = ["stall", "lane-dead", "ready-unmerged", "merged", "needs-human", "evidence-missing"]

[[fleet.app.notify]]
cmd = "notify-telegram <chat>"
on = ["stall", "lane-dead", "needs-human"]
```

Never put tokens in fleet.toml. Keep them in a file only the sender reads, like this
placeholder `notify-telegram`:

```sh
#!/bin/sh
# notify-telegram <chat>: send stdin's line to a Telegram chat.
. ~/.config/notify/telegram.env   # TELEGRAM_TOKEN=...
curl -s "https://api.telegram.org/bot$TELEGRAM_TOKEN/sendMessage" \
  --data-urlencode "chat_id=$1" --data-urlencode "text=$(cat)" > /dev/null
```

Check it worked: `echo hello | notify-telegram <chat>` arrives on your phone.

## 6. Board on the phone (Tailscale)

`drover setup` writes and loads this job for you: label `[serve] launchd_label` (default
`dev.drover.serve`), address `[serve] addr` (default `127.0.0.1:7777`), `KeepAlive`, `ProcessType Interactive` (#893), the tick's PATH,
logs to `~/.local/state/drover/serve.log`. If a job under another label already runs
`drover serve`, setup adopts it (`= adopted <plist>`) and never makes a second one. Re-labelling is
manual: `launchctl bootout gui/$(id -u)/<old label>`, delete its plist, run `drover setup` again.
Undo: `launchctl bootout gui/$(id -u)/dev.drover.serve` and delete the plist (Linux:
`systemctl --user disable --now dev.drover.serve.service` and delete the unit).

By hand: run `drover serve` as a service on `127.0.0.1:7777`. Its PATH must include the
directory that holds `shed`; without it the Lanes tab shows no lanes.

macOS, `~/Library/LaunchAgents/dev.drover.serve.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>dev.drover.serve</string>
  <key>ProgramArguments</key>
  <array>
    <string><home>/.local/share/drover/bin/drover</string>
    <string>serve</string>
    <string>--addr</string>
    <string>127.0.0.1:7777</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string><home>/.local/share/drover/bin:<shed-dir>:/usr/bin:/bin</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
```

```sh
launchctl load ~/Library/LaunchAgents/dev.drover.serve.plist
```

Linux, `~/.config/systemd/user/drover-serve.service`:

```ini
[Unit]
Description=drover board

[Service]
Environment=PATH=%h/.local/share/drover/bin:<shed-dir>:/usr/bin:/bin
ExecStart=%h/.local/share/drover/bin/drover serve --addr 127.0.0.1:7777
Restart=always

[Install]
WantedBy=default.target
```

```sh
systemctl --user enable --now drover-serve
```

Check it worked: `curl -s http://127.0.0.1:7777/ | head -1` prints HTML.

Now publish it to your tailnet over HTTPS. Turn on MagicDNS and HTTPS certificates in the
Tailscale admin console first. Point Tailscale at `127.0.0.1`, not at the tailnet IP:

```sh
tailscale serve --bg --https=8443 http://127.0.0.1:7777
```

`drover setup` runs this for you when `[serve] tailscale_https_port` is set (and `tailscale` is on
PATH; otherwise it skips the step with a note). Undo: `tailscale serve --https=8443 off`.

Tell drover that name is allowed. Without it, every button answers
`forbidden: foreign Host or Origin`:

```toml
[serve]
allowed_hosts = ["<machine>.<tailnet>.ts.net"]
```

Check it worked: on your phone, with Tailscale on, open
`https://<machine>.<tailnet>.ts.net:8443` and press ▲ on a queued issue.

Add it to the Home Screen: Safari's Share → Add to Home Screen (iOS), or Chrome's menu →
Add to Home screen (Android). To pick up a new icon, remove it and add it again.

### Pair the phone

Replies, answers and lane actions need a paired phone. You never type on the phone:

1. On the page (or the Home Screen app), tap **Pair this phone** in the header. It shows a
   6-digit code, e.g. `482913`.
2. On the Mac run `drover serve approve 482913`, or ask the owner to (it runs the
   same command; a `[serve] approve_hint` in fleet.toml adds
   your own way to the panel, e.g. `"or send 'pair {code}' to the owner"`). `drover serve pending` lists the open requests.
3. The phone flips to "paired as iPhone Safari" by itself.

A code works once, for 10 minutes. At most 4 requests are open at a time, 2 per address
(behind `tailscale serve` every phone comes from `127.0.0.1`, so the forwarded address counts instead);
tapping Pair again replaces your own open request. The
device cookie only goes to the browser that tapped Pair, so someone else who sees the code
can't use it. The log records `pair-request` and `pair-approve` with the device label, never
the code or the token. `drover serve pair` now only prints these steps.

Check it worked: the Host tab lists the phone under ALERTS.

## 7. Web push

Not merged yet; see [#18](https://github.com/jirathip-dev/herdsman/issues/18). Once it
lands, you turn it on from the installed app.

## 8. Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| the Lanes tab shows no lanes | the service's PATH has no `shed` | add `<shed-dir>` to PATH in the plist or unit, restart it |
| `forbidden: foreign Host or Origin` | the tailnet name is not allowed | add it to `[serve] allowed_hosts` |
| `STALE` on a button | fleet.toml changed after the page loaded | reload the page |
| HTTPS times out | serve is bound to the tailnet IP | bind `127.0.0.1:7777` and point `tailscale serve` there |
| `CI_NOT_GREEN` from a check that is red on purpose | every check counts by default | list the ones that matter in `required_checks` |
| a fix on staging isn't there | the installed binary is older than staging | reinstall (step 1, or the script in step 4) |

## Coming from herdsman (#456)

drover was called herdsman. An installed herdsman cannot update itself across the rename, so move each host by hand,
the main host first, from its checkout at the new code:

```sh
launchctl bootout gui/$(id -u)/dev.herdsman.tick; launchctl bootout gui/$(id -u)/dev.herdsman.fast   # no pass mid-move
cargo build --release --locked -p drover
sh scripts/signing-identity.sh                                         # once: creates "drover-local"
b=~/.local/share/herdsman/bin; n=target/release/drover; cp "$n" "$b/drover.new"  # signed beside, renamed over (#968)
sh scripts/signing-identity.sh sign "$b/drover.new" && mv "$b/drover.new" "$b/drover"
ln -sf "$b/drover" "$b/herdsman"                                       # the old name, for one release
~/.local/share/herdsman/bin/drover migrate --live                      # config, state and share trees; old names become links
drover timer install        # writes dev.drover.*; prints the unload, rm and load line of each old job: run them
drover profiles sync
drover setup --no-load      # the serve restart: writes dev.drover.serve and prints the swap lines the same way
drover doctor
```

Put `~/.local/share/drover/bin` on `PATH` in place of the herdsman one. Lanes already running keep working: their
`HERDSMAN_*` variables, their hook's old path and their worktree paths all still resolve. What reads the old names
for one release is listed in [architecture](architecture.md) (Rename from `herdsman`).
