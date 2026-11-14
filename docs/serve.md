# The phone page

`drover serve --addr <host:port>` is one phone page (Home Screen app, works offline with a
`last update hh:mm` line) with Queue · Lanes · Chat · Host · Events tabs (Chat: docs/orchestrator.md). `--read-only` serves GET only.

Frozen for features (#552): the board takes bug fixes only, until it retires once the parity list passes. New chat
UI goes to cooee core, new workflow UI (Queue, Asks, Fleets tabs) to Drover's screens, which drover serves itself (#814);
both read `/api/v1`, which stays open. CI's
`board-freeze` fails a PR touching the board's UI files (`assets/serve.*`, `assets/herd.*`, `assets/terminal/`, `web/`,
`crates/server/src/serve.rs`, `serve/` but `v1.rs`) unless its issue has a line starting `board-fix`.

The board is live: the page keeps one `GET /live` Server-Sent Events stream open (same
Host/Origin check; #893: at most 4 streams per device (its paired token, else its address), its fifth closes its
oldest; a `: ping` every `[limits] serve_heartbeat_secs` (15); a stream whose bytes stay untaken through 2 pings is
dropped; a closed client frees its slot within ~1 s). A watcher thread checks the
event log's size/mtime every 1 s and the agent list every 2 s (uncached); on a change (or
every 15 s, for ages) it reads the needs-you cards and the five tabs again and pushes only the changed ones, as their
API v1 data (#269: the server builds no HTML but the page shell; the TypeScript board in `web/` draws every tab):
`event: queue` and `event: host` (the same to every device), `event: needs`, `event: lanes` and `event: chat` per
audience (paired or not; a stream whose device was removed is closed at the next tick, and the page reconnects
unpaired). The board draws each and serve.js swaps it in place: open rows stay open, changed rows flash. #259: every event in the log has a monotonic `seq` (#344: the database's row number;
old rows are numbered by line), each new one goes out as `event: ev` (`{seq, ts, k}`, `k` the moment), and every
message carries `id: <seq>`. A reconnect sends `Last-Event-ID` or `/live?since=N` and gets every event after N
once (over 500: one `event: refresh`, a full refresh); the page keeps the tab's last seq, drops any seq it has seen
and plays a moment only within 2 minutes of its event. The dot next to ↻ is green (live; it pulses on an update), amber
(reconnecting, ↻ spins; back-off 1, 2, 5, 10 s, then a refresh every 10 s after 30 s
down) or red (offline). Tapping ↻, or pulling down from the top (past ~70 px), reads the needs cards, the open tab and
the board's other tabs from API v1 again.

Snapshot (#337): the fast step (every minute) and every kick (a lane push, a verdict, an ask, a board tap) take one
look at everything the board and `status` show and save it as the `board` row of `state.db`'s snapshots (#344): each read's output by
command line. Every remote host's reads (verdict files, `git` facts, its shed's agent list and pane reads) go in one
ssh call per host per step, a small script that prints each output and exit code; drover's ssh keeps one connection
per host for 10 minutes (`ControlMaster=auto`, `ControlPersist=10m`, whatever `~/.ssh/config` says). Only what decides
something is read: a verdict file only until its verdict is recorded, `git log` only for an impl/fix not seen pushed
(or a review with no spawn sha), never a merged or closed lane. A request (`/`, `/api/v1/…`, `/herd.json`, `/live`) and
`status`/`hosts` replay the snapshot: a remote read it lacks fails, it never runs on a request; only a lane's chat
(`/lane`) reads that one pane live (5 s cache). Its age shows in the strip under the header once it is over 2 minutes old (#450: `as of
5m ago`), and `status` ends with it. With no snapshot saved (a new install), reads run live as before; the
watcher pushes on each new snapshot instead of polling shed. `tests/perf.rs` is the gate (budgets: [performance.md](performance.md)): 30 lanes on 2 hosts, +300 ms
per remote call, at most one remote call per host per step, `GET /` < 150 ms (< 40 KB), `/herd.json` < 100 ms,
`status` < 300 ms, first paint in WebKit < 300 ms (warm); a miss fails CI and the numbers go to the job summary.

Speed (#57): a background thread samples the host every 10 s (the 2 s `vm_stat` delta happens there),
so a request never waits on it. Requests share the watcher's cache of pane reads and the
per-issue `git log`; the watcher re-reads at a third of a request's ttl, so a request finds them warm. The icons and
manifest are `public, max-age=86400`. The page is HTML only (`no-store`); its scripts, styles and horses are
`/a/<hash8>/serve.js`, `serve.css`, `herd.js`, `herd.css` and the built-in themes (`themes/…`; `<hash8>` is FNV-1a over
their content at build time) with `cache-control: public, max-age=31536000, immutable`, precached by
the service worker. The SW serves the page stale-while-revalidate: the cached copy paints at once (the
dot amber) and the board's first API read fills it in (green). In scene mode a Lanes avatar shows its
glyph until herd.js has built the horse symbols from the theme's `sprites.svg`.

Page size (#60, #337, #269): `GET /` is the page shell only; the board reads the needs-you cards and the open tab's
part (`GET /api/v1/needs`, `lanes`, `chat`) on load, and another tab's when it first opens (`?all=1`: all at once), its
last copy from this session (sessionStorage) painted first, as the Herd scene's last data is. A copy over 2 minutes old
shows `as of HH:MM` in the header until fresh data lands. A load, refresh or action over 300 ms shows the theme's
`loading` sprite at 96 px, centred near the top of the content over a light scrim (terminal: a spinning ↻); it takes
no taps, the header stays clear, `[↻]` spins its plain ↻ meanwhile, and it goes as soon as the data paints. Animations
pause while the app is in the background. `GET /` also leaves out what the first paint does not show. A fleet's `roles` row is empty
until opened; opening it reads `GET /frag?id=roles&fleet=<fleet>` (the model/effort forms; kept across
live swaps, re-read on ↻). #266: the Events tab is the TypeScript board's (`web/src/events.ts`): on load it reads
`/api/v1/state` and the newest 500 events (`events?since=&until=`), then the next 500 back while the open view has no
more rows than it shows; each `ev` on the live stream reads the new ones (`events?since=`), ↻ reads it all again. #351:
each Events view opens with its last 100 rows; its **older** button shows the next 100. `/?all=1` reads the whole log,
unfolded. No compression:
a repeat visit downloads the page HTML only, the assets come from the service worker's cache. A
`serve --export` inlines both, so the static demo needs no `/frag`; it writes `api/v1/state`, `api/v1/events` (every
event), `api/v1/host` (#267), `api/v1/queue` (#268) and `api/v1/needs`, `lanes` and `chat` (#269) as files for the board.

Queue tab (#115): each fleet's rows are a `git log --graph` tree in queue order. A row blocked by another row in
view hangs off it (`│ ├─ └─`; past depth 3 it clamps to `┆─` and an `on #b` tag), a blocker out of view is a red
`! #b` tag. A dot per row: `●` a lane works on it, `○` waiting, `✓` ready, `!` needs you. An issue in a serial group shows
`🔒<name>` per group (docs/config.md). `[?]` opens the legend sheet: every state word in plain words (the same line is
each state word's title on Queue, Lanes and Herd), the dots, and each fleet's serial paths. ▲/▼ move a row among its
siblings. #268: the tab is the TypeScript board's (`web/src/queue.ts`), drawn from API v1's `queue` on load and ↻; when
its data changes the live stream sends it as `event: queue` (the JSON itself, to every device), and the board draws it
again. A fleet's [config] sheet is drawn from the same data (no `/frag` read); serve.js then groups, graphs and folds
the rows as before.

One repo at a time (#367): chips on top, `[repo n]` with a red dot when it needs you, and `[all]` for every repo's
needs-you rows. A tap, a sideways swipe or ←/→ switches; the pick is kept per phone. A repo's rows are grouped (needs
you, in progress, queued), each tree drawn inside its group. A running row's time reads `in impl 1h 47m`, a needs-you row's `1h 47m`. Queued rows
show their place (`#1`…); after the first 5, `+ N more` opens the rest in place. A blocked chain is one row laid
out as a queued row (#391): the root's number, `chain · 6` as its state, its place and the root's title, then
`▸ #265 → #356 · next: #266 (in impl)` (first → last step; next: the root's open blocker, else the root, with its state
when it is on the board); a tap opens its steps. A page of an older build (the service worker's copy) loads again on
`/live`'s first `build` event. `queue add` reads an added issue's title right away; a row queued from the backlog shows
the backlog read's title until then. Titles start in one column at every depth.

Backlog (#365): under a repo's queue, `backlog (N)` (closed by default) lists every open issue of the repo that is not
queued and has no lane, newest first, with its labels, age and a `stale` tag when untouched for 90 days. The tick's pass
(every 5 min) reads the repo's open issues in one GraphQL call, then only those updated since (a closed one drops out);
`[Drive now]` reads it again at once. The board renders the last read only, never GitHub. A search box and quick filters (`new`,
`>30 days`, `>90 days`, `has AC`, `in a closed PR`, a label) narrow it on the phone. Ticking issues shows `[queue N]`
above the tab bar: it queues them in tick order, each marked `needs-check` (its spec not checked yet). Picking an issue
starts nothing; only queueing does.

Its only writes are ▲/▼ (`queue mv`), `[queue N]` (`queue add`, paired), Pause/Resume and a role's model/effort (`set`, choices
from `models = [...]`), each through the CLI's own code path, logged with `"via":"web"`.

A POST must name the bind address (or a `[serve].allowed_hosts` name, e.g. the
`tailscale serve` `*.ts.net` host) in `Host`/`Origin`, and is refused `STALE` when
fleet.toml changed since the page was rendered.

Lanes are grouped like Corral: state sections blocked → working → idle → done → parked → dead, each
split into fleet subgroups (alphabetical, Others last), newest-changed first. A lane the log has open
but shed shows no pane for is `parked` (neutral, `parked · work on PR #n` or `parked · pushed <sha>`)
when its worktree is clean with nothing unpushed, and `dead` (red, with the log/pane mismatch line)
when work is at risk. #422: a thread's orchestrator (`orchestrator-<thread>`) is a lane of the thread's repo (an
`all` thread's in `all repos`), role `orchestrator`, first in its subgroup, named `<thread title> · <thread>`; once
its pane closed it stays, dimmed, as `asleep`, and its row opens the thread in the chat tab. On a paused fleet a parked lane reads `parked: fleet paused (resume to review
PR #n)`; once resumed, the usual `pushed <sha>, waiting for drive (next pass ≤ 5 min) to spawn review` applies. Tapping a lane
opens a bottom sheet that slides up to 78 % and drags like an iOS sheet (it follows the finger from the
56 px handle, or from the conversation's top; snaps to 50 %, 78 % or full by position and flick; below 50 %, a
down flick, the backdrop or Esc slide it closed) with
its details card and a transcript: `/lane?agent=&lines=200&turns=10&before=<n>` returns up to 200 masked
lines with their terminal colours (the cap is server-side), `live` (how many end lines are the screen's
still-changing last block) and `status` (the spinner line, shown under the chat). #138: the runtime keeps only
the recent screen (Claude draws in the alternate screen), so drover keeps a **rolling transcript** per
lane, `<state dir>/transcripts/<agent>.log` (mode 0600, the last 5,000 lines): every read (the sheet,
`drover check`) appends the lines above the last block past their longest overlap with the file's tail,
or after a `┄ not captured: scrolled past between reads ┄` line when the reads do not meet. `clean` and
`kill` delete it. **Load more** (or scrolling to the top) pages back through it (`more` while older lines exist). #351: a chat opens
bounded: the lines that fit in 32 KB and the session's last `turns` (10) within 3.2 KB a turn, ~64 KB in all; each
Load more asks 10 turns more (`older` while the session has more). The sheet groups
the lines into Corral-style blocks (docs/design/chat-blocks): you, the assistant (minimal markdown:
bold, `code`, lists, headings; escaped, never HTML), tool runs (`$ cmd` collapsed, output in a tinted
panel capped at 20 lines, the head and tail on a failure, `exit N`, `running`) and quiet status lines;
colours map to the board's tokens. Refreshed every 5 s; a block's open state is kept per lane in
`sessionStorage`.
Paired devices only, same Host/Origin checks; an unpaired phone sees "Pair this phone to see the
conversation" with the code entry.

The sheet header has **Stop** (`drover stop`: Esc, the agent pauses), **Kill** (a confirm shows
`drover kill`'s dry-run line, e.g. `impl-42 · impl · 3 unpushed commit(s), 0 dirty file(s) →
wip/… · pane closes, worktree kept`) and **Clean** (`drover clean <fleet> --lane <agent>`;
disabled with the reason under the actions, e.g. "only after merge (PR #42 open)", "live agent";
a lane with no pane hides Stop and Kill and reads no transcript). An Others agent (a shed agent that is no
drover lane) gets none of the three: one **Close pane** asks "close <agent>'s pane? its worktree and files
stay", then POSTs `/act {agent: <pane>, op: close}` (`drover close`), and `p.st` reads "not a drover lane:
drover can only close its pane"; read-only pages and unpaired phones don't get it. The sheet JSON names the
actions: `"acts": ["close"]` (with `"pane"`) for Others, `["stop", "kill", "clean"]` for lanes. Each POSTs `/act {agent, op, confirm}`:
a paired device, the Host/Origin checks and the 10-a-minute limit, logged
`{"type":"act","via":"web","agent","device","op"}` (dry runs are not logged). Typing `/` first in
the reply box lists `/stop`, `/kill`, `/clean` (the actions), `/compact`, `/clear`, `/status`
(sent to the agent as text) and `/report` (report mode, below), filtered as you type; tap or Enter picks. Inputs are 16 px (iOS
never zooms), the page under the sheet is scroll-locked and the sheet follows `visualViewport`,
so the composer stays above the keyboard and is still there when it closes.

The sheet's reply box sends text to that agent (`agent prompt` + Enter; dsh lanes:
`pane send-text` + Enter). A device must be paired once (#59, docs/setup.md): **Pair this
phone** (header, blocked cards, the sheet) POSTs `/pair/request`, which shows a 6-digit code and sets
an HttpOnly, Secure, SameSite=Strict `hd=` cookie with a random token for 10 minutes. The token pairs
nothing until `drover serve approve <code>` on the Mac (`NO_SUCH_CODE` / `EXPIRED` otherwise, single
use; `serve pending` lists requests). The approval is logged, so the waiting page's live stream
sends its `ev` (and the page's 2 s claim poll, #288, sees it) and the page POSTs `/pair/claim`, which keeps the same cookie for a year;
each page load claims again, renewing it, and a device unused for a year (365 days) no longer counts as paired
(`drover serve devices` lists each paired device's days left). Opened on the host itself (a loopback peer whose
`Host` is `127.0.0.1`/`localhost`, no `X-Forwarded-For` or `Tailscale-User-Login`), the panel also shows the
exact `drover serve approve <code>` line with a `[copy]` button: paste it in Terminal (a browser can't run a
command; there is no `drover://` link). Local doesn't mean trusted: lanes run on the host too, so the lane
hook refuses `serve approve`, `serve unpair` and `serve devices --revoke` (`REFUSED OWNER_ONLY`). At most 4 requests
are open, 2 per peer address (behind `tailscale serve`, a loopback peer, the `X-Forwarded-For` address or
`Tailscale-User-Login`; trusted from loopback only), and a new code never repeats an open one. A re-tap of
Pair replaces the browser's own open request, and the panel's `[cancel]` (`/pair/cancel`) drops it. Only the token's SHA-256 is kept, in `<state>/serve/devices.json`
(0600); the Host tab's ALERTS group lists paired devices with a Remove button. Approving a browser that pairs again
(same device label but its hash suffix, same address) replaces its older row (#501).
A 401 never latches the page unpaired (#501): it asks `GET /pair/status` (`{"paired": bool}`, no secrets) and only
a "no" shows the Pair UI; any later answer to a paired device (its `X-Paired: 1` header) takes it away again. A
database error reading the devices answers 503 `board busy, retrying` (API v1: `BUSY`), never 401: the page retries
a send once after 2 s and says "reconnecting". Each 401 and 503 is one `serve.log` line, `auth <unix secs> <status>
<method> <route>: <reason>` (`no cookie`, `unknown token`, `expired`, `db error: <kind>`; never the token), and
`drover doctor` shows the last 24 h's counts.
The panel reads `Waiting for approval on the Mac: drover serve approve <code>`. An unpaired page,
while any device is paired, adds "Safari and the home-screen app pair separately" (iOS keeps their cookies apart). An optional `[serve] approve_hint`
(a string, `{code}` replaced by the code) is appended after ` · `, e.g.
`approve_hint = "or send 'pair {code}' to the owner"`; `/pair/request` returns it as a second line and
the page sets it as text, never HTML.
Sends pass the same Host/Origin checks, are limited to 10 a minute per device, and are refused
`PAUSED` (paused fleet), `REV_READONLY` (reviewer lanes) or `BAD_VALUE` (empty, over 2,000
chars, or a control character other than newline). Removing a device also needs a paired cookie. Each send logs `{"type":"prompt","via":"web","agent","device","chars"}`, never the
text. Secure cookies need https (`tailscale serve`) or localhost.

Open asks (`drover ask`, but an orchestrator's `ask --thread`: those wait in their thread) show as a red **needs you** card above every tab: the question, option
buttons (recommended first) and a free-text box, tagged `routine · defaults to 2 in 24m` or `owner · waits
for you` (#99, [docs/lanes.md](lanes.md#asks)). Answering POSTs `op=answer` (`drover answer … --by owner`,
logged `"via":"web"`) under the same Host/Origin checks and needs a paired device (401 otherwise). In the
Queue tab the asked issue shows `⏸ waiting: decision`.

A **blocked** lane gets a needs-you card too (and the same card in its sheet). Pane text reaches
paired devices only (API v1's `needs` and `lanes`, `/live`): an unpaired viewer sees who is blocked, the
activity line and a "pair this phone to answer" hint. For a Claude
pane showing a numbered menu (`❯ 1. Yes`, permission prompts, question lists) the options are
buttons, each carrying a hash of the menu as the card shows it: a tap POSTs `/answer`, which
re-reads the pane (past every cache), refuses `STALE: menu changed — refreshed` with no key when
that menu is no longer the one on the pane, else sends that digit with `pane send-keys`,
re-reads it again and says `sent` or `still waiting`. Either way the cards re-render from a fresh
pane read (pushed to `/live`, and the tapping page refreshes), so stale buttons go at once. It needs a paired
device and the same Host/Origin check and 10-a-minute limit as the reply box, and is refused
`PAUSED`, `BAD_VALUE` (no menu parsed, or not one of its numbers: never a guessed key) or
`REV_READONLY` (a reviewer, except on a "Do you want to …?" permission prompt). Each logs
`{"type":"answer","via":"web","agent","device","choice"}`, never the question. A free-text
question, a dsh/Hermes lane or an unparsed pane shows the question (or the last 6 masked
lines) and a Reply button that opens the sheet's reply box.

A menu's card (#127) also shows the prompt's block above it, masked: its first line as the title
(`Bash command`), the reason (`Dangerous rm operation on possibly-empty variable path …`, 2 lines), the `│`
command box (mono 11 px, 3 lines, tap to expand) and, for a prompt the harness auto-denies, `auto-denies in
0:14` counting down from the pane read. Such a card carries `data-push=0` (no phone alert) until its countdown
has run out. When the next read shows no prompt and the pane is working again, the card goes.

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
stream down, the 10 s refresh reads it too. Pull to refresh works on a Herd page.

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

**Ranch light (#71)**, Corral's Herd environment: `[A]`/`[☀]`/`[☾]` in the strip, left of `[×]`, cycles
Auto → Day → Night, kept on this phone (`localStorage` `herd-light`, default Auto). Auto is the phone's clock:
Day 07:00–19:00, re-checked every minute and on resume. Night is Corral's night scene: every fill takes its
`color(day, dark)` dark pair (`#herd.night` swaps the custom properties), a dark sky, the seeded star field and
Milky Way drifting as one at 0.7 pt/s, the moon fixed at (306, 56) (above the strip's crop on a short screen),
clouds at 0.055 opacity, and the cast light from the moon's side. Storm and fog compose with it. Debug:
`/?light=auto|day|night`.

### Themes and colour schemes (#172, #241, #244)

The page's look is data, no code: see [themes.md](themes.md) for the contract (per-role actions, looks, load scene,
rail, moments, row sprites, icons, ASCII words, Lanes modes), the per-phone `[◐]` look sheet, `[serve] theme` (its
default) and the built-in `farm`, `kitchen`, `garden` (pinned from the shared theme repo, #928) and `terminal`. `serve` serves `/themes/<name>/<file>`; `--export`
writes them to `themes/`. Colours are a separate per-phone pick in the same sheet: 8 built-in schemes plus your own
`<config dir>/schemes/<name>.json`, each contrast-gated ([themes.md](themes.md#colour-schemes-244)).

## Report mode (#363)

`/report` in the reply box's `/` menu closes the sheet and turns the board into report mode: a tap outlines an
element (a tap on the toast stops), a second tap on it opens a one-line note box at the bottom. **Send** POSTs
`/report`: the element's outerHTML (trimmed to 8 KB), its key computed styles, the board's context (tab, theme,
scheme, style, viewport and the last `/live` id) and a PNG of the element plus 24 px around it, drawn on a canvas
from the layout (each box's background and border, then each word; no images or gradients). A paired device only,
with the Host/Origin checks and the 10-a-minute limit; nothing leaves the Mac. `serve` keeps the bundle as
`state/reports/<id>/bundle.json` (plus the build) and `shot.png`, and emits a `report` event whose line names the
bundle path to every notifier but web push, whatever its `on` (the first fleet's notifiers). `drover report` lists the reports,
newest first; `drover report <id>` prints one bundle.

## Alerts (web push)

The Host tab's **Enable alerts** button shows only in the Home Screen app (iOS allows web push
there alone). Tapping it asks for notification permission, subscribes with the server's VAPID
public key and `POST /push/subscribe`s the subscription; that and the Host tab's per-device
**Remove** (`POST /push/unsubscribe`) need the same Host/Origin check and a paired device (401
otherwise). Delivery goes Mac → Apple/Google push service → phone, outbound only; the page stays
tailnet-only.

#370: the subscription carries the phone's theme words (`w_merged`, `w_needs`), so a merge reads `Order up: #2957
merged` (kitchen) or `Over the fence: #2957 merged` (farm), and needs-human / an owner ask `The bell: api #2961 needs
you` / `At the gate: …`; terminal sends none and keeps today's titles. A theme switch re-sends them with `quiet=1`
(saved, no test push). Only the title changes; the body stays.

```toml
[[notify]]
type = "webpush"
sub = "mailto:you@example.com"   # the VAPID `sub` claim; required
# on = [...]   # default: blocked, stall, lane-dead, ready-unmerged, merged, needs-decision, needs-human, ask-waiting, answer-undelivered, state-write-failed
```

It works per fleet too (`[[fleet.<name>.notify]]`), next to any command notifier, which is
unchanged. Each event is encrypted per device (RFC 8291 aes128gcm), signed with an ES256 VAPID
JWT (12 h) and sent once with `curl -sS --max-time 10 -X POST`, the body on stdin. A 404 or 410
prunes that device; any other failure logs one `notify-failed` (no retry). Tapping an alert
opens the page on its tab with the issue's row open. `drover push test` sends a test alert to
every subscribed device.

The subscribe tells the truth (#87): with a webpush notifier it pushes one "alerts are on" test to
that device alone and answers `alerts on` (or why the test push failed); without one it still keeps
the subscription but answers, under the button, "alerts are saved but off: add `[[notify]] type =
"webpush"` to fleet.toml (docs/serve.md)". The Host tab's ALERTS `push` row reads `on · 1 device · last
sent 3m ago`, `off: no webpush notifier`, or `on · last failure: <reason>` while a webpush
`notify-failed` is newer than the last delivery. Each delivery logs one passive `push-sent` with the
device count only (no endpoint, no key). `drover check` logs one `alerts-off` (again only when the
device count changes) while `subs.json` has devices but fleet.toml has no webpush notifier.

Secrets: the VAPID key (`<state>/webpush/vapid.key`) and the subscriptions
(`<state>/webpush/subs.json`, endpoint, keys, device label, creation time) are 0600 and never
logged, printed or served; the page gets only the public key and each device's label.

Dependencies: the allowed crates are clap, serde, serde_json, toml and anyhow/thiserror, plus, for web
push only, the pure-Rust RustCrypto set p256, aes-gcm, hkdf, sha2 and base64 (no async, no HTTP
client), ts-rs behind the `ts` feature for the board's TS types (#375, never in the release build), and, for the
shed runtime only, shed-proto (shed's serde wire types, no I/O; docs/runtimes.md), and rusqlite (bundled SQLite)
for the state database (#344, docs/architecture.md), keyring for the main host's secret store
(#441, owner-approved; docs/providers.md), and tokio in `crates/service` only (#345, docs/service.md). CI's deps gate
fails on any other direct dependency.

## GitHub webhooks (#626)

serve also listens on `[serve] hooks_addr` (default `127.0.0.1:7778`, `""` turns it off; a `--read-only` serve has
none). That listener answers `POST /hooks/github` only: every other path is a bare 404, so publishing it never
publishes the board. It is the one path on Tailscale Funnel; the board stays tailnet-only on `tailscale serve`. Funnel
opens a whole HTTPS port, so give it one the board does not use (443, 8443 or 10000):

```
tailscale funnel --bg --https=10000 --set-path /hooks/github http://127.0.0.1:7778/hooks/github
```

In the repo's GitHub settings add a webhook: payload URL `https://<mac>.<tailnet>.ts.net:10000/hooks/github`,
content type `application/json`, the secret, and the events *Pull requests*, *Check suites*, *Check runs*,
*Statuses* and *Pushes*. The secret is the 0600 file `[serve] webhook_secret_file` names (#707; one for every fleet, refused
`SECRET_FILE_OPEN` if others can read it); without it the endpoint answers 503 and doctor shows
`! webhook — no secret`.

Each delivery's `X-Hub-Signature-256` (HMAC-SHA256 of the raw body) is checked in constant time; a missing or bad one
gets 401 and is only counted (doctor's `webhook` row). A body over GitHub's 25 MB is refused (413) before it is read.
`ping` answers 200. A `pull_request`, `check_suite`, `check_run`, `status` or `push` event naming an open fleet
issue's branch or PR (a merge push: its `(#N)`) re-reads that PR into the cache `drover pr` reads and kicks drive
for the issue at once (200); anything else is ignored with 202. While a fleet has the secret, the fast step's PR read
becomes a catch-up poll every `[limits] webhook_poll_minutes` (default 10), for what a sleeping Mac or a down Funnel
missed.

## JSON API v1 (#265)

`drover serve` answers JSON under `/api/v1/`, the frozen TypeScript board's (`web/`) every read, and Drover's screens' and cooee core's (below). Every route checks Host/Origin like `/live`; the routes the board keeps for a paired device (a
transcript, a setup screen, and the paired ops: answer, drive, override, extend, park, unpair, orphans, reap, host, bqueue, cond, and
#555's send, menu, act, setup, chat-about, chat, close) refuse an unpaired one.

#725: `/api/v1/lifeboat` is a plain HTML page, not JSON: the fallback for when cooee is down ([lifeboat.md](lifeboat.md)).

| route | answer |
|---|---|
| `GET state` | the board in one read: `seq` (newest event), `now` and `tz` (the server's clock and UTC offset, s), `age` (the step's snapshot, null when none), `mtime`, `theme`, `fleets` (with `repo`), `queue`, `lanes`, `asks`, `hosts`, `services`, `alerts` |
| `GET health` | #709: `ok`, `config` (`ok`, or `invalid` with its `error`, `config invalid: <error> (fleet.toml:<line>)`); no answer: the process is dead. An invalid fleet.toml leaves the board reading the last good one (kept as `fleet.good.toml` beside the log), with that line as a red banner above every tab (`needs`' `config`); `check` alerts `config-invalid` once per distinct error, `config-ok` once it loads again |
| `GET host` | #267: the Host tab: `hosts` (each card's dot, words, rows with their gauge, value, sparkline series and, in an export, chart), `disk`, `orphans`, `leftovers`, `spawns`, `services`, `theme`, `accounts`, `deepseek`, `fleets`, `push`, `vendors` (#830: `accounts` and `setups`, each account's and each provider login's `{name, vendor}`, from the providers), and on a live board `key`, `subs`, `devices`; `rw` and `live` say which forms show and whether they post |
| `GET queue` | #268: the Queue tab: a card per fleet (needing you first) with its `k` colour, serial groups, `need`, `paused` line or `drive` line and red `notes`, its `rows` (state, age and `t`, stage chips, lane, PR line, notes, a defaulted decision and its `ask`, ▲/▼ targets, the `lanes` list), #576 their `marks` by issue (`design`, `verify` `on`/`off`/null, `repro`, `serial`, `needs`, the verify `note`), `done` today, the `backlog` and the `config` sheet's data; `rw` and `live` say which forms show and whether they post |
| `GET issue/<fleet>/<n>` | #576: the issue's recorded verdicts, newest first: `role` (the stage: `rev`, `verify`), `round`, `sha`, `verdict`, `findings` (verdict.md after its first line), the newest one's screenshot `shots` (`/shot/<fleet>/<n>/<i>` serves the `i`th) and a `--repro` PASS's `repro` block (`kind` `run` or `verify`, its `text`) |
| `GET lessons` | #576: per fleet its `lessons` (`weekly`, `off`), the `last` lesson lane (`issue`, its spawn `ts`, the issue's `state`) and `next`, when tick runs the next (null: `off`) |
| `GET lessons/<fleet>/digest?days=N` | #576: the CLI's `lesson digest` (default 7 days) as `text` |
| `GET needs` | #269: the needs-you cards for this device: `snap` (#450: the step's look's time, 0 for none; the strip under the header shows its age once over 2 min), the open `asks` (`who`, `kind`, imported `from`), each `blocked` lane's card (unpaired: its `activity`; paired: the harness `menu` or the pane's `tail`), the `needs-human` `issues` (one more round `pending`, the last verdict, its screenshot `shots` and #969 `shots_at`, the verdict's sha that `/shot/<fleet>/<n>/<i>/<sha>` names, the PR line, `merge`), `choosing` (`[chat·N]`); `rw`, `live`, `paired` and `pair` say which forms and buttons show |
| `GET lanes` | #269: the Lanes tab for this device: state `sections` with their repo groups and each lane's row (avatar `state`, `act`, `atone`, `coat`; `state` in `tone` for `ago`, `why`, the `activity` or `now` line) and hidden card (`head`, `shown`, `facts`, the PR line, `dead`, the `blocked` card); `error` (no fleet.toml) or `failed` (shed unread); #611: for a paired device, `shed`, the folded read-only `Other in shed (N)` section's rows (`GET shed`'s) |
| `GET shed` | #611 (paired only, else 401): every enabled host's shed sessions no lane or conductor in the log names and no `fleet` label marks, each `{name, host, runtime (shed or acp), labels, state (shed's), last (an ACP session's last reply, a terminal's last non-empty screen line), age (since the board first saw that state)}`; a host whose shed fails is one `{host, error}` entry, and stays up. One `ls` per host, cached 10 s like the lane list |
| `GET chat` | #269: the [chat] tab: per repo (`all` first) its thread `rows` and `archived` ones, then each thread's view (`ds`, `state`, its last 30 messages by `kind`, `older` or the `started` line); `rw`: a paired device on the live board writes; #620: serve keeps the folded tab by the log's last seq, fleet.toml's mtime, the agents and the minute, and a request in between reuses it without reading the log |
| `GET thread/<t>?before=N&n=30` | #408, #269: a thread's messages before seq `N`, as `chat`'s pages. #681: a handoff's message carries one `handoff` item (cooee's agent→agent message): `kind: "handoff"`, `id` (its seq; a repo handoff's is its card's), `from` (`{agent: "orchestrator", thread, repo}`), `to` (the thread id once delivered, else `{repo, pending: true}`), `title`, `summary` (the first line), `text`, `state` (`pending`, `delivered`, `failed` with its `error`, `forwarded`), `folded` (the sender's thread; the receiver's is the message form) and, while pending, `choices`: the card's rows (`n`, `text`, the `thread` it goes to, `new` for a new thread), each picked by `POST <op>` (`answer`) with its `fleet`, `id`, `choice` = `n` and `State.mtime`; the outcome shows in both threads' items. A repo handoff's own `kind: "handoff"` row after the pick has no item (its card's is the one) |
| `GET repo/<fleet>/threads` | #681: the repo's orchestrator threads, newest-active first: `id`, `title`, `last` (`line`, `ts`), `unread`, `awake` (its orchestrator not asleep), `archived`, and `newest: true` on the ongoing one (the first not archived), so cooee shows it as the agent's chat and the rest as side threads; `all` for the all-repos threads; 404 `NOT_FOUND` for no such fleet |
| `GET events?since=N` | #259's contract: the events after `N` (#266: `&until=M`, up to `M`) in `list`; over 500, `refresh: true` and none (read `state` again). Each as the Events tab says it: `glyph`, `tone`, `text`, `imp`, the row sprite's `act` and `coat`, `plain`, and its issue's `pr` or checks `run` |
| `GET lane/<agent>` | the lane's transcript page, the sheet's `/lane` JSON (`?before=N` pages back) plus #749's `blocks`: an ACP lane's transcript from shed's events (`prompt`, `reply`, `tool`, `plan`, `approval`; `?before=<seq>&n=N` pages back), a PTY lane's one `screen` block (its `lines`) |
| `POST <op>` | a form op (`pause`, `resume`, `mv`, `set`, `cond`, `bqueue`, `answer`, `drive`, `override`, `extend`, `park`, `unpark`, `host`, `reap`, …) with the HTML form's fields and `State.mtime`, as the form or a JSON object (#555: a list joins with `,`, `true` is `1`); answers `{ok, code, message}` |
| `GET herd` | #555: Herd mode's data, the same as `/herd.json` |
| `GET roles?fleet=F` | #555: the fleet's [config] sheet (`config`: its roles with the model/effort pickers, the orchestrator) and `chat`, `[chat]`'s new-thread defaults with the harnesses it may name |
| `GET setup/<pane>` | #555: a setup terminal's screen (`lines`, `urls`, `closed`), the old board's `/lane?setup=` |
| `POST send` | #555: `{agent, text}` to a lane, or `{setup, text}` into a setup terminal: the old board's `/send` (10 a minute per device: `RATE_LIMIT`; `PAUSED`, `REV_READONLY`, `BAD_VALUE`) |
| `POST menu` | #555: `{agent, choice, h}`, a blocked lane's menu option: `/answer` (`STALE` when the menu is not the card's `h`) |
| `POST act` | #555: `{agent, op, confirm}`, stop, kill, clean or close: `/act` (kill and clean dry-run without `confirm`) |
| `POST setup` | #555: `{host, name}`, start a configured setup terminal: `/setup` (`NO_SUCH_SETUP`) |
| `POST chat-about` | #555: `{fleet, issue}`, a backlog row's [chat about]: a new thread in the fleet's repo whose first message names the issue |
| `POST chat` | #555: `{harness, model, effort, mtime}`, `[chat]`'s defaults written into fleet.toml (`BAD_VALUE`, `STALE`, `DOCTOR` as `cond`) |
| `POST close` | #555: `{fleet, issue}`, a backlog issue's [close]: `gh issue close`, logged `closed`; `NOT_BACKLOG` for a queued issue or one with a lane |
| `POST qset` | #576 (paired): `fleet`, `issue`, `key` (`design`, `verify`, `repro`), `value` (`on`, `off`): the CLI's `queue <fleet> set <n> <key> <value>` |
| `POST lesson` | #576 (paired): `fleet`: the CLI's `lesson <fleet>`, its queue entry logged `by` the device |
| `POST orchestrator-attach` | #633 (paired): `{fleet, thread}`, a cooee thread as the repo's orchestrator thread (`orchestrator-attached`): `ask --thread` takes it, lane asks route to it, its asks and decisions are `events`; answers `OrchestratorAttach`: `brief`, `settings` (the synced orchestrator Claude settings), `env` (`DROVER_LANE`, `DROVER_THREAD`, `DROVER_PORT`), `guarded` (#565's ACP guard holds) and `why` not |
| `POST orchestrator-detach` | #633 (paired): `{fleet, thread}`, the link ends (`orchestrator-detached`; archiving the thread ends it too); `NO_SUCH_THREAD` unless attached |

Every answer is JSON, a refusal too: `{ok: false, code, message}` with 401 `UNPAIRED`, 503 `BUSY` (#501), 403 `FORBIDDEN`, 405
`READ_ONLY`, 409 and the `REFUSED` code (`STALE`, `DRIVE_RUNNING`, …), 404 `NOT_FOUND`. The theme is the phone's own
choice (localStorage): `state.theme` is the board's default, and no route writes it. The service worker
never caches `api/`. A breaking change is `/api/v2/`.

The types are defined once, in `crates/server/src/api.rs` beside the board's (#375): `just types` writes them to
`assets/types/api.d.ts`, which `web/` imports, and CI fails when that file is stale. The handlers are in
`crates/server/src/serve/v1.rs`. Each endpoint's JSON is golden-tested in `crates/cli/tests/api_v1.rs`. #555: the contract with
cooee core and Drover's screens is `crates/cli/tests/api_v1_contract.rs`: a fixture per route in
`crates/cli/tests/golden/api_v1/` (the route, its Rust type, the type's schema generated from its default, and a recorded
response). A response whose shape (keys, each value's JSON type) or a type whose schema
changes fails CI until the fixtures are updated in the same PR (`BLESS=1 cargo test --test api_v1_contract`).

`web/` is TypeScript with no framework: `npm run check` (`tsc --strict --noEmit`) and `npm run build` (esbuild:
`dist/board.js` and `dist/board.css`, the CSS from `assets/`; the themes stay in `assets/swag/` and `assets/terminal/`). The only dev
dependencies are `typescript` and `esbuild`, at pinned versions. `web/dist` is committed and embedded in the binary
(`/a/<hash>/board.js`), so `cargo install` needs no Node. CI's `web` job rebuilds it and fails on a diff, so run `npm run
build` after a UI edit.

### Who consumes this

- Drover's tabs (Queue, Asks, Fleets), served by drover to cooee (#814): they read and act only through `/api/v1`;
- cooee core, where it reads it (the lanes it shows as Workflow chats, cooee#33);
- the frozen `web/` board (#552), until #553's parity rows pass.

#609: every lane in `state`, `lanes` and `herd` (`/herd.json`), and each `queue` row (its active lane's, empty with
none), carries `runtime` (`shed` or `shed-acp`, from the lane's spawn: a config change later never moves it),
`host` (its `[hosts]` name, the main host's for a local lane, `local` with no `[hosts]`; `herd`'s stays null without
one) and `where`, the one label every surface shows: the host while 2 or more hosts are enabled (`air`, or
empty: no chip). The old board's Queue and Lanes rows show it as a chip; `status`'s HOST column, a `board` row's
`on …` line and `waiting`'s age show the same string. Each shape is in `crates/cli/tests/golden/api_v1/where.json`.

The rule: a screen that needs data drover doesn't send gets the `/api/v1` field here first, with its fixture in
`crates/cli/tests/api_v1_contract.rs`, then the consumer reads it. Consumers never derive engine state themselves.

## cooee's native screens (#814)

drover is cooee's `drover` plugin: `cooee-plugin.toml` at the repo root names its places (the Conductor's Queue and
Events chips, its chat cards, the drover menu row) and `[permissions]`. cooee runs `drover serve --addr
127.0.0.1:$PORT` and calls it with `Authorization: Bearer $COOEE_PLUGIN_TOKEN`; the board's own Host check and a paired
device work too. Beside `/api/v1`, never in the board's pages, it answers JSON made of cooee's blocks
(cooee#125's screens-as-data design):

- `GET /screens/queue` (`?repo=<fleet>`), `/screens/events` (`?kind=merge,verdict,lane,ask`, `?before=<unix time>`),
  `/screens/fleets` (the Fleets sheet) and `/screens/cards` (the open asks, design reviews, needs-human issues and run
  cards as chat cards); q1235: each ask card carries `fold`, its folded line (`#N · kind · question cut short · age`)
  and its `main` button (`{block, action, value, label}`, the recommended answer or one more round), which the app
  shows until a tap opens the full card; a second tap folds it again;
- `GET /screens/<x>/stream`: server-sent `{seq, ops}` events, a `screen` op first, then each change as `merge`,
  `replace`, `remove` and `insert` ops by block id, and the Queue chip's `badge` count;
- `POST /action` `{screen, block, action, values}`: an action not in the manifest's `[permissions]` actions is 403;
  each other runs the board's own op (a refusal is 409 with its line).

Tests: `crates/cli/tests/cooee_screens.rs` against cooee's fixtures in `crates/cli/tests/golden/cooee/`.

## Host tab (#141)

Five groups in a fixed order (#234): HOSTS (a card per host, main first, then A→Z: load/cpu, swap-out, memory, disk,
top memory, lanes, tags, each with a 24 h sparkline that opens a 1 h / 6 h / 24 h chart; docs/hosts.md), FLEET DISK
(the worktree budget, orphan folders, processes left in closed lanes), SERVICES (board, tick, accounts,
deepseek), FLEETS (each fleet's last `drover drive` result in plain words, the raw line on tap) and ALERTS (push,
alert devices, paired devices). A group takes its worst row's status, red over amber over green (a paused fleet is
grey and counts as none); problem groups open, green ones collapse to one summary line. A tap's
open/closed holds for the browser session until the group's status changes (`sessionStorage`, `host:<GROUP>`).
Load/cpu and swap-out show a 12-cell gauge against their limit (green under 80 %, amber to 100 %, red past it with
`▶` and the percentage); memory's and disk's bars show the used part (`free 12 GB of 228`), their status from the
floor (`min_free_mem_gb`, `min_free_gb`). The words
come from one table in `crates/server/src/serve.rs` (`WORDS`, golden-tested in crates/cli/tests/host_words.rs). The tab bar is
`[queue·N] [lanes] [host] [events]`: the active tab is filled with the accent, and N counts the needs-you cards.
#267: the tab is the TypeScript board's (`web/src/host.ts`), drawn from API v1's `host` on load and ↻; when its data
changes the live stream sends it as `event: host` (the JSON itself, to every device), and the board draws it again.

## Static export and the public demo

`drover serve --export <dir>` writes the page as static files instead of serving it: `index.html`,
`herd.json`, `manifest.webmanifest`, `sw.js`, the icons and `a/<hash8>/` assets, rendered by the same code as the live page.
Every URL in them is relative and the service worker keys its paths and cache on its registration scope,
so the files work under a sub-path (the live server at `/` behaves as before). A plain export is the
read-only view, without pane text or transcripts.

`--demo` (#39) is the public demo at <https://jirathip-dev.github.io/herdsman/>, built from the synthetic
`demo/` (3 made-up repos, `fleet.toml`, `events.jsonl`, `cache/` for issue/PR titles and CI, and
`runner.json`: the canned agent list, pane text and host values by command-line prefix). Nothing
runs: no shed, no gh, no network, and `demo/` is never written (#344: the export imports its files into a scratch database). "Now" is a minute after the log's last event, so ages never go stale.
The page carries a `demo` badge and a one-line note; every form is `inert` and every button, select and
the reply box disabled; no POST is ever sent, there is no `/live` stream (the dot is a static blue), no
pairing and no "Enable alerts". The sheet's transcript reads `lane/<agent>.json`, Herd reads the static
`herd.json`, and a calm / gusty / storm / fog switcher sets the weather client-side.
`.github/workflows/pages.yml` publishes it on every push to `staging`; CI exports it too and runs the
leak gate over `docs/`, `demo/` and the export.
