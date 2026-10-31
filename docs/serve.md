# The phone page

`herdsman serve --addr <host:port>` is one phone page (Home Screen app, works offline with a
`stale` badge) with Queue · Lanes · Host · Events tabs. `--read-only` serves GET only.

Its only writes are ▲/▼ (`queue mv`), Pause/Resume and a role's model/effort (`set`, choices
from `models = [...]`), each through the CLI's own code path, logged with `"via":"web"`.

A POST must name the bind address (or a `[serve].allowed_hosts` name, e.g. the
`tailscale serve` `*.ts.net` host) in `Host`/`Origin`, and is refused `STALE` when
fleet.toml changed since the page was rendered.

Lanes are grouped like Corral: state sections blocked → working → idle → done → dead, each
split into fleet subgroups (alphabetical, Others last), newest-changed first. Tapping a lane
opens a bottom sheet (78 % high, drag up for full, swipe down / backdrop / Esc to close) with
its details card and a transcript of the last ~40 masked pane lines, refreshed every 5 s
(paired devices only, same Host/Origin checks; otherwise just the row's activity line).

The sheet's reply box sends text to that agent (`herdr agent prompt` + Enter; dsh lanes:
`herdr pane send-text` + Enter). A device must be paired once: `herdsman serve pair` prints a
6-digit code (5 minutes, one attempt); the first send asks for it and sets an HttpOnly,
Secure, SameSite=Strict cookie with a random token. Only the token's SHA-256 is kept, in
`<state>/serve/devices.json` (0600); the Host tab lists paired devices with a Remove button.
Sends pass the same Host/Origin checks, are limited to 10 a minute per device, and are refused
`PAUSED` (paused fleet), `REV_READONLY` (reviewer lanes) or `BAD_VALUE` (empty, over 2,000
chars, or a control character other than newline). Removing a device also needs a paired cookie. Each send logs `{"type":"prompt","via":"web","agent","device","chars"}`, never the
text. Secure cookies need https (`tailscale serve`) or localhost.

Open asks (`herdsman ask`) show as a red **needs you** card above every tab: the question, option
buttons (recommended first) and a free-text box. Answering POSTs `op=answer` (`herdsman answer … --by owner`,
logged `"via":"web"`) under the same Host/Origin checks and needs a paired device (401 otherwise). In the
Queue tab the asked issue shows `⏸ waiting: decision`.

Lane modes: `horse` (default), `ascii` (a 3-line horse, 2-frame gallop when working) and
`glyph`. The vector horses are ported from Corral (jirathip-dev/corral,
`ios/FleetNotifier/UI/Herd/HerdArt.swift` and `ios/tools/herd-art/horsesvg.py`, MIT ©
jirathip-dev): same shapes and 8 coats, a stable coat per agent name, the diagonal leg gait
while working (off under `prefers-reduced-motion`), dimmed when done, red when blocked or dead.

The Home Screen icon is Corral's Palomino app icon (jirathip-dev/corral, `ios/tools/herd-art/appicon-masters/treatment-a/palomino-1024.png`, MIT © jirathip-dev).
