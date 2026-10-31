# The phone page

`herdsman serve --addr <host:port>` is one phone page (Home Screen app, works offline with a
`last update hh:mm` line) with Queue · Lanes · Host · Events tabs. `--read-only` serves GET only.

The board is live: the page keeps one `GET /live` Server-Sent Events stream open (same
Host/Origin check, at most 8 clients, then 503; a `: ping` every 15 s; a closed client frees its
slot within ~1 s). A watcher thread checks the
event log's size/mtime every 1 s and `herdr agent list` every 2 s (uncached); on a change (or
every 15 s, for ages) it re-renders the needs-you cards and the four tabs and pushes only
the changed ones as `event: frag` (`{id, html}`), rendered per audience (paired or not; a stream whose device was removed is closed at the next
tick, and the page reconnects unpaired), swapped in place: open rows stay open,
changed rows flash. The dot next to ↻ is green (live; it pulses on an update), amber
(reconnecting, ↻ spins; back-off 1, 2, 5, 10 s, then a `/?frag=all` poll every 10 s after 30 s
down) or red (offline). Tapping ↻, or pulling down from the top (past ~70 px), fetches
`/?frag=all` from the network.

Speed (#57): a background thread samples the host every 10 s (the 2 s `vm_stat` delta happens there),
so a request never waits on it. Requests share the watcher's cache of `herdr pane read` and the
per-issue `git log`; the watcher re-reads at a third of a request's ttl, so a request finds them warm. The icons and
manifest are `public, max-age=86400`. The page is HTML only (`no-store`); its scripts, styles and horses are
`/a/<hash8>/serve.js`, `serve.css`, `herd.js`, `herd.css` and `horses.svg` (`<hash8>` is FNV-1a over
their content at build time) with `cache-control: public, max-age=31536000, immutable`, precached by
the service worker. The SW serves the page stale-while-revalidate: the cached copy paints at once (the
dot amber) and the live stream's first `?frag=all` fills it in (green). In horse mode a Lanes avatar shows its
glyph until herd.js has built the horse symbols from `horses.svg`.

Its only writes are ▲/▼ (`queue mv`), Pause/Resume and a role's model/effort (`set`, choices
from `models = [...]`), each through the CLI's own code path, logged with `"via":"web"`.

A POST must name the bind address (or a `[serve].allowed_hosts` name, e.g. the
`tailscale serve` `*.ts.net` host) in `Host`/`Origin`, and is refused `STALE` when
fleet.toml changed since the page was rendered.

Lanes are grouped like Corral: state sections blocked → working → idle → done → dead, each
split into fleet subgroups (alphabetical, Others last), newest-changed first. Tapping a lane
opens a bottom sheet (78 % high, drag up for full, swipe down / backdrop / Esc to close) with
its details card and a transcript: `/lane?agent=&lines=200&before=<n>` returns up to 200 masked
pane lines (the cap is server-side) grouped per turn (a typed prompt opens one); tool calls
(Claude `⏺ Bash(…)`/`Read(…)` plus their indented output, Hermes `┊` runs) collapse to one line
that taps open, long lines wrap. Refreshed every 5 s; **Load more** at the top reads older lines
(`herdr pane read --source recent --lines <before+200>`; herdr 0.9 has no scrollback source, only
visible|recent|recent-unwrapped|detection, so history ends where herdr's recent window does, at most
2,000 lines back: once a read comes back short the reply says `more: false` and the button goes).
Paired devices only, same Host/Origin checks; an unpaired phone sees "Pair this phone to see the
conversation" with the code entry.

The sheet header has **Stop** (`herdsman stop`: Esc, the agent pauses), **Kill** (a confirm shows
`herdsman kill`'s dry-run line, e.g. `impl-42 · impl · 3 unpushed commit(s), 0 dirty file(s) →
wip/… · pane closes, worktree kept`) and **Clean** (`herdsman clean <fleet> --lane <agent>`;
disabled with the reason, e.g. "not merged", "live agent"). Each POSTs `/act {agent, op, confirm}`:
a paired device, the Host/Origin checks and the 10-a-minute limit, logged
`{"type":"act","via":"web","agent","device","op"}` (dry runs are not logged). Typing `/` first in
the reply box lists `/stop`, `/kill`, `/clean` (the actions) and `/compact`, `/clear`, `/status`
(sent to the agent as text), filtered as you type; tap or Enter picks. Inputs are 16 px (iOS
never zooms), the page under the sheet is scroll-locked and the sheet follows `visualViewport`,
so the composer stays above the keyboard and is still there when it closes.

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

## Herd mode

The 🐎 button in the header switches to **Herd mode**, a port of Corral's Herd screen
(jirathip-dev/corral, `ios/FleetNotifier/UI/Herd/`: `HerdView.swift`, `HerdArt.swift`,
`RanchEnvironment.swift`, `RanchWind.swift`; MIT © jirathip-dev): full screen, no tabs, one page
per registered fleet (swipe left/right; an Others page only when unregistered agents run), ✕
returns to the tabs. The mode and the page are kept in `localStorage`. Each page: a strip with
`repo · N here`, page dots and `5 queued·3 working·2 idle·4 done` (zero counts left out, so it fits 390 px unclipped; queue items waiting on another issue count as queued, not blocked); the fleet's needs-you
cards (same per-audience rules as the tabs); Corral's ranch (sky, clouds, hills, barn, fence,
trees, grass); then the pasture in bands (working in gait, idle, done, blocked/dead in red),
each horse at Corral's size (a fixed 132×100 px drawing in a 156 px slot; rows wrap centred: 2 on a
phone, 4 at 768 px, 6 at 1024 px) with a cream label at most 164 px wide, 2 lines (agent; state as
the Lanes row shows it incl. the #46 waiting reason, and age; the issue title is its tooltip) and a
coat stable per name. Queued issues stand at a gate above the working band, 40 % size (53×40 px in a
60 px slot, up to 6 a row on a phone), in queue order, `#issue` with `⛔#blocker` on a second line;
tapping one shows its title and blocker. Tapping a horse opens the Lanes sheet (card, transcript, Stop/Kill/Clean, reply).

Data: `GET /herd.json` (same Host/Origin check; only the fields an unpaired Lanes row shows, never
pane text): `{fleets:[{name, lanes:[{name, issue, title, role, harness, model, effort, account,
state, shown, tone, age, coat}], queue:[{issue, title, blocked_by, coat}]}], others:[…],
host:{load, limit, pressure, swapouts, swap_limit, gate}}` (`gate`: null when the spawn gate
admits, else its reason). While Herd is open the page reads it once, then takes the `/live`
stream's `event: herd` (the same body, re-read every 10 s while a client listens); with the
stream down, the `/?frag=all` poll reads it too. Pull to refresh works on a Herd page.

**Updates never move the scroll (#65).** A Herd update patches each page in place: horses are keyed by
agent (queued ones by issue), relabelled and reposed in place, re-parented between bands, and added or
removed one by one; a band comes or goes only with its first or last horse. The tabs' sections are
patched the same way (rows by key, `open` kept), and `scrollY` is restored in the same frame. Only pull to
refresh and ↻ start from the top. Gate 2's `herd-scroll` / `queue-scroll` views prove it (docs/ui.md).

Weather is the host: wind = load/cpu ÷ limit (0..1) scales Corral's `RanchWind` canopy/grass
sway ×1..3 and the cloud drift; a refusing spawn gate is a storm (dark sky, rain, manes blown,
lightning at most every 20 s); memory pressure `warn`/`critical` is light/thick fog; a solid 28 px
bar at the bottom shows `load 0.39 · mem normal · swap 0`, and the page is padded past it. One `requestAnimationFrame` loop drives all motion
(current page only, paused when hidden); horses are `<use>` instances. Under
`prefers-reduced-motion` nothing moves and the weather is a tint only. Debug:
`/?weather=calm|gusty|storm|fog` forces the host values on a loopback `Host` only (ignored
elsewhere); `/?fps=1` adds an fps reading to the caption.

## Alerts (web push)

The Host tab's **Enable alerts** button shows only in the Home Screen app (iOS allows web push
there alone). Tapping it asks for notification permission, subscribes with the server's VAPID
public key and `POST /push/subscribe`s the subscription; that and the Host tab's per-device
**Remove** (`POST /push/unsubscribe`) need the same Host/Origin check and a paired device (401
otherwise). Delivery goes Mac → Apple/Google push service → phone, outbound only; the page stays
tailnet-only.

```toml
[[notify]]
type = "webpush"
sub = "mailto:you@example.com"   # the VAPID `sub` claim; required
# on = [...]   # default: blocked, stall, lane-dead, ready-unmerged, merged, needs-decision, orch-stuck, ask-waiting
```

It works per fleet too (`[[fleet.<name>.notify]]`), next to the Telegram notifier, which is
unchanged. Each event is encrypted per device (RFC 8291 aes128gcm), signed with an ES256 VAPID
JWT (12 h) and sent once with `curl -sS --max-time 10 -X POST`, the body on stdin. A 404 or 410
prunes that device; any other failure logs one `notify-failed` (no retry). Tapping an alert
opens the page on its tab with the issue's row open. `herdsman push test` sends a test alert to
every subscribed device.

Secrets: the VAPID key (`<state>/webpush/vapid.key`) and the subscriptions
(`<state>/webpush/subs.json`, endpoint, keys, device label, creation time) are 0600 and never
logged, printed or served; the page gets only the public key and each device's label.

Dependencies: the allowed crates are clap, serde, serde_json, toml and anyhow/thiserror, plus, for web
push only, the pure-Rust RustCrypto set p256, aes-gcm, hkdf, sha2 and base64 (no async, no HTTP
client). CI's deps gate fails on any other direct dependency.

## Static export and the public demo

`herdsman serve --export <dir>` writes the page as static files instead of serving it: `index.html`,
`herd.json`, `manifest.webmanifest`, `sw.js`, the icons and `a/<hash8>/` assets, rendered by the same code as the live page.
Every URL in them is relative and the service worker keys its paths and cache on its registration scope,
so the files work under a sub-path (the live server at `/` behaves as before). A plain export is the
read-only view, without pane text or transcripts.

`--demo` (#39) is the public demo at <https://jirathip-dev.github.io/herdsman/>, built from the synthetic
`demo/` (3 made-up repos, `fleet.toml`, `events.jsonl`, `cache/` for issue/PR titles and CI, and
`runner.json`: the canned `herdr agent list`, pane text and host values by command-line prefix). Nothing
runs: no herdr, no gh, no network. "Now" is a minute after the log's last event, so ages never go stale.
The page carries a `demo` badge and a one-line note; every form is `inert` and every button, select and
the reply box disabled; no POST is ever sent, there is no `/live` stream (the dot is a static blue), no
pairing and no "Enable alerts". The sheet's transcript reads `lane/<agent>.json`, Herd reads the static
`herd.json`, and a calm / gusty / storm / fog switcher sets the weather client-side.
`.github/workflows/pages.yml` publishes it on every push to `staging`; CI exports it too and runs the
leak gate over `docs/`, `demo/` and the export.
