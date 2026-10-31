# The phone page

`herdsman serve --addr <host:port>` is one phone page (Home Screen app, works offline with a
`last update hh:mm` line) with Queue · Lanes · Host · Events tabs. `--read-only` serves GET only.

The board is live: the page keeps one `GET /live` Server-Sent Events stream open (same
Host/Origin check, at most 8 clients, then 503; a `: ping` every 15 s; a closed client frees its
slot within ~1 s). A watcher thread checks the
event log's size/mtime every 1 s and `herdr agent list` every 2 s (uncached); on a change (or
once a minute, for ages) it re-renders the needs-you cards and the four tabs and pushes only
the changed ones as `event: frag` (`{id, html}`), rendered per audience (paired or not; a stream whose device was removed is closed at the next
tick, and the page reconnects unpaired), swapped in place: open rows stay open,
changed rows flash. The dot next to ↻ is green (live; it pulses on an update), amber
(reconnecting, ↻ spins; back-off 1, 2, 5, 10 s, then a `/?frag=all` poll every 10 s after 30 s
down) or red (offline). Tapping ↻, or pulling down from the top (past ~70 px), fetches
`/?frag=all` from the network.

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

A **blocked** lane gets a needs-you card too (and the same card in its sheet). Pane text reaches
paired devices only (page, `/?frag=all`, `/live`): an unpaired viewer sees who is blocked, the
activity line and a "pair this phone to answer" hint. For a Claude
pane showing a numbered menu (`❯ 1. Yes`, permission prompts, question lists) the options are
buttons, each carrying a hash of the menu as the card shows it: a tap POSTs `/answer`, which
re-reads the pane (past every cache), refuses `STALE: menu changed — refreshed` with no key when
that menu is no longer the one on the pane, else sends that digit with `herdr pane send-keys`,
re-reads it again and says `sent` or `still waiting`. Either way the cards re-render from a fresh
pane read (pushed to `/live`, and the tapping page refreshes), so stale buttons go at once. It needs a paired
device and the same Host/Origin check and 10-a-minute limit as the reply box, and is refused
`PAUSED`, `BAD_VALUE` (no menu parsed, or not one of its numbers: never a guessed key) or
`REV_READONLY` (a reviewer, except on a "Do you want to …?" permission prompt). Each logs
`{"type":"answer","via":"web","agent","device","choice"}`, never the question. A free-text
question, a dsh/Hermes lane or an unparsed pane shows the question (or the last 6 masked
lines) and a Reply button that opens the sheet's reply box.

Lane modes: `horse` (default), `ascii` (a 3-line horse, 2-frame gallop when working) and
`glyph`. The vector horses are ported from Corral (jirathip-dev/corral,
`ios/FleetNotifier/UI/Herd/HerdArt.swift` and `ios/tools/herd-art/horsesvg.py`, MIT ©
jirathip-dev): same shapes and 8 coats, a stable coat per agent name, the diagonal leg gait
while working (off under `prefers-reduced-motion`), dimmed when done, red when blocked or dead.

The Home Screen icon is Corral's Palomino app icon (jirathip-dev/corral, `ios/tools/herd-art/appicon-masters/treatment-a/palomino-1024.png`, MIT © jirathip-dev).
