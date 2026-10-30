# Full local setup

From zero to the board as an app on your phone's Home Screen. Every step ends with a check.
Placeholders: `<owner>`, `<repo>`, `<home>` (your home directory), `<machine>`, `<tailnet>`,
`<chat>`. Swap in your own values; keep them out of anything you commit.

## 1. Install

You need `git`, `gh` (logged in), [herdr](https://herdr.dev), a Rust toolchain, and at
least one harness: Claude Code and/or Hermes.

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
serial_paths = ["db/migrations/**"]          # one `--serial` lane at a time

[fleet.app.role.orch]
harness = "hermes"
profile = "fleet-orch"

[fleet.app.role.impl]
harness = "claude"
profile = "fleet-impl"
model = "opus"
effort = "high"                              # none | low | medium | high | xhigh
mcp = ["<dev-server>"]                       # servers from the repo's .mcp.json; default none

[fleet.app.role.rev]
harness = "claude"
profile = "fleet-rev"
model = "sonnet"
```

Then write the per-role folders and the lane hook:

```sh
herdsman profiles sync
```

Check it worked: `herdsman status` prints no error. Every other key is in
[fleet.example.toml](../fleet.example.toml).

## 3. First lane

```sh
herdsman queue app add 12
herdsman spawn app - orch
herdsman status
```

The orchestrator picks issues off the queue and spawns the lanes. Check it worked:
`herdsman status` shows #12 moving past `queued`, and `herdsman board` shows its lane.

## 4. Watchdog

`herdsman check` alerts once per stall, dead lane or unmerged ready issue. Run it every
5 minutes. With cron (`crontab -e`):

```sh
*/5 * * * * PATH=<home>/.local/share/herdsman/bin:<herdr-dir>:/usr/bin:/bin herdsman check
```

`<herdr-dir>` is the directory `command -v herdr` prints. Check it worked: `herdsman check`
by hand exits 0 and prints nothing when all is well.

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

Run `herdsman serve` as a service on `127.0.0.1:7777`. Its PATH must include the
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
