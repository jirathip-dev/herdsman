# Full local setup

From zero to the board as an app on your phone's Home Screen. Every step ends with a check.
Placeholders: `<owner>`, `<repo>`, `<home>` (your home directory), `<machine>`, `<tailnet>`,
`<chat>`. Swap in your own values; keep them out of anything you commit.

## 1. Install

You need `git`, `gh` (logged in), [herdr](https://herdr.dev) 0.9.0 or newer, a Rust toolchain, and at
least one harness: Claude Code and/or Hermes. Every herdr call goes through `src/herdr.rs`, the one
adapter: a herdr CLI change touches that file only.

```sh
gh auth status
herdr --version
cargo install --path . --locked --root ~/.local/share/herdsman
echo 'export PATH="$HOME/.local/share/herdsman/bin:$PATH"' >> ~/.zshrc   # or ~/.bashrc
```

Check it worked: open a new shell and run `herdsman --version`.

## 2. Config

One repo as fleet `app`, three roles. Save it as `~/.config/herdsman/fleet.toml`:

```toml
models = ["opus", "sonnet"]

[fleet.app]
repo = "<owner>/<repo>"
local = "~/src/<repo>"
integration = "staging"                      # merge target; main never is
required_checks = ["test"]                   # gate on these checks only; unset = all
serial_paths = ["db/migrations/**"]          # `--serial` lanes: serial_max (default 1) at a time

[fleet.app.role.orch]
harness = "hermes"
profile = "lane-orch"

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
herdsman profiles sync
```

Check it worked: `herdsman status` prints no error. Every other key is in
[fleet.example.toml](../fleet.example.toml). Claude accounts and the DeepSeek key live in env files, never
in fleet.toml: see [providers.md](providers.md).

## 3. First lane

```sh
herdsman queue app add 12
herdsman spawn app - orch
herdsman status
```

The orchestrator picks issues off the queue and spawns the lanes. Check it worked:
`herdsman status` shows #12 moving past `queued`, and `herdsman board` shows its lane.

## 4. Watchdog

One command, safe to run again: `herdsman setup`. It writes and loads the tick job and the board
job ([6](#6-board-on-the-phone-tailscale)), sets up `tailscale serve`, runs `herdsman profiles sync`,
then runs `herdsman doctor` and exits with its code. Each step prints `✓ done`, `= already` or
`✗ <error> → <fix>`; `--no-load` writes the jobs without loading them.

Run it every 5 minutes: `herdsman timer install`. That is the tick job alone: `herdsman tick` every
5 minutes runs `herdsman check` (alerts once per stall, dead lane or unmerged ready issue), then
`herdsman drive` (the loop of `driver = "herdsman"` fleets, [lanes.md](lanes.md#drive)). No cron and
no Hermes needed.

- macOS: `~/Library/LaunchAgents/dev.herdsman.tick.plist` (`--label` picks another label),
  `StartInterval 300`, `RunAtLoad`, loaded with `launchctl bootstrap gui/$(id -u) <plist>`.
- Linux: `~/.config/systemd/user/dev.herdsman.tick.{service,timer}`, loaded with
  `systemctl --user enable --now dev.herdsman.tick.timer`.
- Its PATH is explicit: the directories of the harness binaries, `gh`, `herdr` and herdsman, then
  `/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin` (`sysctl` and `vm_stat` live in `/usr/sbin`).
- Both streams go to `~/.local/state/herdsman/tick.log`; the tick empties it past 1 MB.
- A refusal (`HOST_BUSY`, …) is logged and the tick exits 0. A real error (config, herdr missing)
  logs `tick-failed`, which notifiers send at most once an hour. A red row in the Host tab's Services
  block sends one `service-red` an hour too.

`herdsman timer status` prints loaded or not and the last tick. Undo: `herdsman timer remove`
(unloads it and deletes its files). Check it worked: `herdsman doctor` shows `✓ timer` and a
`watchdog check` that ran under 15 minutes ago.

Optional: reinstall when staging goes green, so the binary never lags it. A short script
you can add to the same crontab:

```sh
#!/bin/sh
# reinstall-herdsman: rebuild from staging when its latest CI run is green.
set -e
cd ~/src/herdsman
git fetch -q origin staging
sha=$(git rev-parse origin/staging)
[ "$sha" = "$(cat ~/.local/share/herdsman/sha 2>/dev/null)" ] && exit 0
ok=$(gh run list --branch staging --commit "$sha" --json conclusion -q '.[0].conclusion')
[ "$ok" = "success" ] || exit 0
git checkout -q --detach "$sha"
cargo install --path . --locked --root ~/.local/share/herdsman
echo "$sha" > ~/.local/share/herdsman/sha
```

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

`herdsman setup` writes and loads this job for you: label `[serve] launchd_label` (default
`dev.herdsman.serve`), address `[serve] addr` (default `127.0.0.1:7777`), `KeepAlive`, the tick's PATH,
logs to `~/.local/state/herdsman/serve.log`. If a job under another label already runs
`herdsman serve`, setup adopts it (`= adopted <plist>`) and never makes a second one. Re-labelling is
manual: `launchctl bootout gui/$(id -u)/<old label>`, delete its plist, run `herdsman setup` again.
Undo: `launchctl bootout gui/$(id -u)/dev.herdsman.serve` and delete the plist (Linux:
`systemctl --user disable --now dev.herdsman.serve.service` and delete the unit).

By hand: run `herdsman serve` as a service on `127.0.0.1:7777`. Its PATH must include the
directory that holds `herdr`; without it the Lanes tab says `herdr agent list failed`.

macOS, `~/Library/LaunchAgents/dev.herdsman.serve.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>dev.herdsman.serve</string>
  <key>ProgramArguments</key>
  <array>
    <string><home>/.local/share/herdsman/bin/herdsman</string>
    <string>serve</string>
    <string>--addr</string>
    <string>127.0.0.1:7777</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string><home>/.local/share/herdsman/bin:<herdr-dir>:/usr/bin:/bin</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict>
</plist>
```

```sh
launchctl load ~/Library/LaunchAgents/dev.herdsman.serve.plist
```

Linux, `~/.config/systemd/user/herdsman-serve.service`:

```ini
[Unit]
Description=herdsman board

[Service]
Environment=PATH=%h/.local/share/herdsman/bin:<herdr-dir>:/usr/bin:/bin
ExecStart=%h/.local/share/herdsman/bin/herdsman serve --addr 127.0.0.1:7777
Restart=always

[Install]
WantedBy=default.target
```

```sh
systemctl --user enable --now herdsman-serve
```

Check it worked: `curl -s http://127.0.0.1:7777/ | head -1` prints HTML.

Now publish it to your tailnet over HTTPS. Turn on MagicDNS and HTTPS certificates in the
Tailscale admin console first. Point Tailscale at `127.0.0.1`, not at the tailnet IP:

```sh
tailscale serve --bg --https=8443 http://127.0.0.1:7777
```

`herdsman setup` runs this for you when `[serve] tailscale_https_port` is set (and `tailscale` is on
PATH; otherwise it skips the step with a note). Undo: `tailscale serve --https=8443 off`.

Tell herdsman that name is allowed. Without it, every button answers
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
2. On the Mac run `herdsman serve approve 482913`, or ask the owner to (it runs the
   same command; a `[serve] approve_hint` in fleet.toml adds
   your own way to the panel, e.g. `"or send 'pair {code}' to the owner"`). `herdsman serve pending` lists the open requests.
3. The phone flips to "paired as iPhone Safari" by itself.

A code works once, for 10 minutes. At most 3 requests are open at a time, 2 per address
(behind `tailscale serve` every phone comes from `127.0.0.1`, so they share that limit). The
device cookie only goes to the browser that tapped Pair, so someone else who sees the code
can't use it. The log records `pair-request` and `pair-approve` with the device label, never
the code or the token. `herdsman serve pair` now only prints these steps.

Check it worked: the Host tab lists the phone under Paired devices.

## 7. Web push

Not merged yet; see [#18](https://github.com/jirathip-dev/herdsman/issues/18). Once it
lands, you turn it on from the installed app.

## 8. Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `herdr agent list failed` | the service's PATH has no `herdr` | add `<herdr-dir>` to PATH in the plist or unit, restart it |
| `forbidden: foreign Host or Origin` | the tailnet name is not allowed | add it to `[serve] allowed_hosts` |
| `STALE` on a button | fleet.toml changed after the page loaded | reload the page |
| HTTPS times out | serve is bound to the tailnet IP | bind `127.0.0.1:7777` and point `tailscale serve` there |
| `CI_NOT_GREEN` from a check that is red on purpose | every check counts by default | list the ones that matter in `required_checks` |
| a fix on staging isn't there | the installed binary is older than staging | reinstall (step 1, or the script in step 4) |
