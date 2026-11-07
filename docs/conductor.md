# Conductor chat (#355, steps 1 to 3)

Talk to a **conductor** through herdsman itself, with no Hermes needed. A **thread** belongs to one fleet's repo, or
to `all` for cross-cutting work. Each thread is talked through by one conductor lane.

```sh
herdsman thread new acme "Grill #12"          # prints t1
herdsman thread say t1 "what should #12 cover?"
herdsman thread show t1 [--json]              # the conversation; marks it read
herdsman thread ls [FLEET] [--json]           # per repo: title, last message, unread •, awake/asleep
```

## The conductor lane

- **Role `conductor`, any harness:** `[fleet.<n>.role.conductor]` takes the usual role keys (`harness`, `model`,
  `effort`, `account`/`accounts`, `fallback`, `mcp`). An `all` thread uses the first fleet that defines one. Without
  one (#401), a fleet's conductor is its `impl` role on profile `lane-conductor` (which `profiles sync` writes; a
  Hermes impl needs `hermes profile create lane-conductor`); only a fleet with neither refuses `NO_CONDUCTOR`.
  `herdsman spawn … conductor` refuses `BAD_VALUE`.
- **[config] → Conductor** shows the role in effect (`default: impl` or `set`) with tappable harness, model (the
  models the fleet's roles use) and effort choices. A tap from a paired device writes `[fleet.<n>.role.conductor]` in
  place (the first one copies the impl role's keys), after saving the old file as `fleet.toml.bak`; it is refused
  `DOCTOR` when `herdsman doctor` with the new file shows a ✗ it did not show before. The next conductor start uses it.
- **Harness, model and effort (#404):** the conductor role keeps its powers; the owner picks only these. `[chat]`
  sets new threads' defaults over the role (each key unset: the role's):

  ```toml
  [chat]
  harness = "claude"   # claude | hermes | dsh; another harness than the role's drops its model and accounts
  model = "opus"
  effort = "high"
  account = "A"        # or accounts = ["A", "B"]
  ```

  A thread keeps the harness it starts on for good (`herdsman thread new … --harness <h>`, or the empty thread's
  harness chip; a thread from before #404 has the default). Its composer's model and effort chips offer the models
  that roles (or `[chat]`) on its harness use and the harness's efforts (no model chip where a profile owns the
  model: Hermes, dsh). A pick logs `thread-model`, shows `model sonnet · effort low from the next turn`, and the
  next message resumes the conductor's session on it. The header reads `repo · harness · model`.
- **No worktree:** it runs in `<state dir>/conductor/<thread>/` (or `<[conductor] dir>/<thread>/`) in its own herdr workspace, as agent
  `conductor-<thread>`, with `HERDSMAN_LANE=<fleet>:-:conductor` (so the lane hook guards it) and
  `HERDSMAN_THREAD=<thread>`.
- **Its brief** (`.brief.md` there, rewritten at each start) is `roles/conductor.md`, the thread and repo, then the
  `[conductor]` profile:

  ```toml
  [conductor]
  dir = "~/conductor"                            # optional: the threads' folders' root (default <state dir>/conductor)
  instructions = "~/notes/conductor/AGENTS.md"   # inlined
  memory = "~/notes/conductor/memory"            # each *.md inlined
  skills = "~/notes/conductor/skills"            # listed, read on demand
  allow = ["design", "clean", "hosts"]           # #538: capability groups (default: all but pause and set)
  ```

- **Sleep and resume:** `check` closes an awake conductor whose thread has been quiet for 30 min and that is not
  working (`conductor-sleep`). The next message starts it again with the same session id. Claude Code is started
  with `--session-id <uuid>` and resumed with `--resume <uuid>`, so the full conversation carries over. A harness
  that can't resume gets the thread so far in its kick instead.

## Messages and asks

- `thread say` logs `thread-msg` (from `owner`), then types `[<thread>] owner: <text>` into the conductor (waking it
  first). The conductor answers with `herdsman thread reply <thread> "<text>"`.
- **Choices:** the conductor runs `herdsman ask <fleet> - "<question>" --thread <thread> --option … --recommend N`.
  The thread shows the ask with its options, the recommended one first. `herdsman answer <fleet> <id> <n>` (the
  tap later) logs the decision, adds `DECISION <id>: <text>` to the thread and types it into the conductor.
  `--thread` with an unknown thread refuses `NO_SUCH_THREAD`.
- **Lane asks (#541):** an `ask` from an impl, rev, fix or design lane (no `--thread`) goes to its repo's conductor
  thread first: `[fleet.<n>] ask_thread = "t6"`, else the repo's most recently active thread (archived ones never).
  It is logged `routed` (no push) and typed into that conductor (woken as by `say`) as a system line the board hides;
  it stays open on the board and the issue card. No thread (or the conductor not reached: `ask-owner`), the owner
  gets it as before. The conductor answers with `herdsman answer <fleet> <id> <n> --basis "<quote | q<id>>"`, logged
  `by: conductor-<thread>` with its `basis`, and only if the ask is a lane ask on its repo, its question and choice
  name no owner topic (the `OWNER_TOPIC` words), and the basis is 3+ words found verbatim in the issue's spec
  (Amendments included) or an answered decision id. Else `REFUSED OWNER_ASK: <why>; ask the owner`, and the ask is
  logged `ask-owner` in the thread: a choice row (the conductor's pick recommended) that pushes like an ask. Each
  conductor answer posts `answered <id> for #<n>: <choice>, because <basis>` with [change] for 30 min (no push); the
  owner's pick there re-sends `DECISION <id>: <choice>` to the lane, `by: owner`, replacing it.
- **Amendments (#574):** `herdsman amend <fleet> <issue> --basis q<id> "<text>"` (conductors and the owner; the lane
  hook refuses it for impl, rev and design lanes) writes an owner's decision into the spec: it appends
  `- **Amendment (q<id>)**: <text>` to the issue body's `## Amendments` section (made once, at the end), logs `amend`
  `{id, text}` and re-runs the open PR's failed spec-reading checks (board-freeze). `q<id>` must be an ask on that
  issue answered `by: owner` (or a routine ask's `by: default`), else `REFUSED NO_DECISION` before any GitHub call.
  A conductor runs it as soon as an owner decision names an amendment; the thread shows it as a tool row.

- **Files:** `herdsman thread reply <thread> "<text>" --file <path>` (repeatable) shows the files with the message:
  images inline, any other file as a row. Only a file in the thread's conductor folder is taken (anything else is
  refused: copy it in first), and the board serves only those. Files the owner attaches on the board (#405: several, up to 10 a message,
  each at most 10 MB; picked at once or added with `[+]`, waiting above the box as thumbnails with `[×]`) or with
  `herdsman thread say <thread> "<text>" --file <path>` (repeatable) are saved in the conductor's folder under
  `uploads/` and each path is typed in with the message (`(attached: <path>)`), so the conductor can read them.
- **Handoff (#413):** `herdsman thread handoff <from> <repo|thread> <title> <text>` starts a thread titled `<title>`
  in a repo (or `all`), or reuses an existing thread (`t6`; `<title>` is the handoff row's subject). It logs
  `thread-handoff` `{from, to, title, text}`, then types `[<to>] <from>: <text>` into the target's conductor (started or
  resumed, as with `say`). The source shows `continued in <to>` and the target `from <from>` with the text. While the
  link exists, the target's filed issues (`gh issue create`: `filed #<n>: <title>`, or the title alone) and answered
  thread asks (`answered <id>: <question> → <answer>`) each post one line back into the source. Any conductor may
  hand off to any repo; a conductor's plain `thread new` refuses `USE_HANDOFF`, so every thread a conductor starts is
  logged with its source. The owner's `thread new` (CLI or board) is unchanged.
- **Commands:** the lane hook logs each `herdsman` or `gh` command the conductor runs (`thread-cmd`, not `thread
  reply`); the board shows them as closed tool rows.

## The [chat] tab (#386)

Built from the approved mockup (`docs/design/conductor-chat/`): the middle tab, `[queue] [lanes] [chat] [host]
[events]`, reading `[chat·N]` with N the threads where a choice waits.

- **The list:** "all repos", then each fleet by name, a card each with its 3 newest threads (the rest fold under
  "N older threads") and `[+ thread]`, which opens an empty thread (#405): nothing is logged until its first message,
  which starts the thread (titled with that message's first line, cut to one row) and its conductor, and is the only
  thing the conductor gets. After its first reply the conductor retitles it once with a short summary
  (`herdsman thread title <thread> "<title>"`, logged `thread-title`). A row has the unread dot, the title, the age,
  then `! choose`, `✻ working` or `☾ asleep` and the last message.
- **Archive (#405):** a row swiped left shows `[archive]`: the thread's conductor stops (its pane closes; the folder,
  uploads and log stay) and the thread folds under "N archived" at the bottom of its card, where `[restore]` brings
  it back; its next message resumes the same session. CLI: `herdsman thread archive|restore <thread>` (logged
  `thread-archive` / `thread-restore`). Nothing is deleted.
- **A thread** is the lane sheet's chat blocks: your messages in the accent colour, the conductor's purple, its
  commands as closed tool rows, system lines (resumed, asleep, account switch), images and file rows
  (`GET /thread/<thread>/<name>`), plain links. An open ask shows its choice rows (recommended first) and an "other"
  box, with an amber edge; a tap answers it (`DECISION` typed into the conductor); answered, a green `✓` line with
  who and when stays. Thread asks never join the red needs-you stack. Below: the live line and the composer (`[+]`
  attach, the text, `[send]`). Opening a thread marks it read.
- **Linked threads (#413):** each handoff row, message from another thread and flow-back line carries a
  `from t5` / `continued in t6` chip, and so does a linked thread's row in the list; a tap opens the other thread.
- **At 1000 px and up** the list is a 400 px column with the open thread beside it.
- **Not a paired owner device:** the thread reads the same; the choice rows, `[+ thread]` and the composer are off.
- **Writes** are `POST /thread` (`op` = `new`, `say` with an optional base64 file of at most 10 MB, `answer`,
  `read`, `retry`; #407 `queue`, `unqueue`, `steer`, `stop`) from a paired device, 10 a minute. #403: `new` and `say` answer once the thread or message is
  logged; starting or resuming the conductor and typing the line run after the reply. A failed delivery is a red
  system line with `[retry]` (`retry` types the same line again, logging no copy). No chat post shows the loading
  overlay.
- **On a phone (#403):** `[send]` clears the box at once and shows the message `sending…`, then `✓`; failed, red
  with `[retry]` and the text (and file) back in the box. The box grows to 6 lines, then scrolls; with the keyboard
  up the tab bar hides and (#443) the composer sits flush on the keyboard: the page never scrolls, only the
  messages do, and the message just above the box stays there as the keyboard opens or closes or the box grows. Opening a thread pushes a history entry: back, `[‹]` or a swipe right from the left edge
  returns to the list. A working thread shows the theme's loading art (sprite, ascii frames or the glyph) inline
  above the box.
- **Comfort (#407, from t3code's mobile app):**
  - *Scrolling:* the thread follows its end on a live update until your own drag (or wheel) leaves the end; back at
    the very end, or on any send, it follows again. A reply streaming in never pulls you down while you read.
  - *Pinned send:* after a send your message sits ~16 px under the top (under a bar that sticks there, else the
    screen's top) and the reply grows below it; the room under it stays empty until the reply fills it.
  - *Folded turns:* in a finished turn each run of commands is one tappable `▸ N commands` row (open, its rows);
    replies stay shown. The live turn's commands stay rows. Each fold keeps its open state across live updates.
  - *The pill:* one floating pill above the box: `working 1m 23s` with the theme's loading art and `[stop]` (Esc to
    the conductor, `op=stop`), then `stopping…` until the turn ends; `reconnecting…` while the live stream is down,
    `offline · tap to retry` after 30 s or offline. Idle and connected, none (it replaces the working line).
  - *Queue or steer:* while a turn runs `[send]` is `[queue]` ("delivered when this turn ends · hold [queue] to
    steer"): `op=queue` logs `thread-queue` and the server keeps it; when the conductor is no longer working the
    board's watcher types the queued ones in, oldest first, each logged as your message (`thread-unqueue`, why
    `sent`). A long press offers `[queue]` and `[steer]` (`op=steer`: Esc, then the message now), each with its
    line. Queued messages sit above the box with `[send now]` (steered in), `[edit]` (the box shows `editing queued
    message · [cancel]`; its send replaces it, `op=queue&q=`, and your draft comes back) and `[remove]`
    (`op=unqueue`, why `removed`).
  - *Undo:* `[archive]` after a swipe shows `archived · [undo]` for 5 s; `[undo]` restores it. One row is open at a time.
  - *Drafts:* each thread keeps its draft text (localStorage) and picked files (IndexedDB) on the device, across tab
    switches, closing the app and lost signal; a failed send keeps them.
  - *Message menu:* a long press (or right click) on a message: `[copy]` (its text), `[copy as markdown]` (its
    source), `[quote]` (`> ` lines into the box), then `copied`.
  - *No push while you look:* the page's live stream names the thread open on a shown page (`/live?thread=`); while
    it is connected the server stamps `<state dir>/watch-<thread>.at` each second, and a reply or ask in that thread
    pushes nothing (within 5 s of the stamp). A hidden page's stream drops it.

**Push** (a `webpush` notifier, default `on`): a conductor's reply is `<repo> · <thread title>: <its first line>`, a
choice `<repo> · <thread title> asks: <question>`; a tap opens the thread. The owner's messages, commands and system
lines never push, nor (#407) anything in a thread a board has open.

Events: `thread-new`, `thread-msg` (`files`; #413 `via`: a flow-back line), `thread-handoff`, `thread-cmd`, `thread-read`, #407 `thread-queue` (`text`, `files`, `q`: the one an edit replaces) and `thread-unqueue` (`q`, why: sent, removed), `conductor-start` (session, resumed,
account), `conductor-sleep` (why: idle, gone, usage limit, signed out); #387: `config-propose`, `config-settled`,
`secret-request`, `secret-set`, `secret-dismissed`, `secret-rm` (names only).

## Config changes (#387)

`herdsman config propose <thread> <file>`: `<file>` is the whole new fleet.toml (the conductor edits a copy). It is
validated like fleet.toml itself, and its diff shows in the thread as a card with **[apply] [discard]**. Only the
owner applies it: a tap on a paired device, or `herdsman config apply|discard <id>` outside a lane (the lane hook
refuses both inside one, `OWNER_ONLY`). Applying keeps the old file as `fleet.toml.bak-<time>`, writes the
new one and runs `doctor`. A failing `doctor` does not roll back: the line says `doctor ✗` and the backup is there
to restore. If fleet.toml changed since the proposal, it refuses `STALE`, and the card shows `stale: fleet.toml
changed since` instead of its buttons (#460). Settled, the card folds to the thread's system line, e.g. `config c1
applied by owner, backup kept, doctor ✓` (the conductor gets it too). #460: while a card waits, the thread's bar and
its row in the list say `config c1 waiting` (a tap on the bar's scrolls to the card), and the thread's first page
always carries it: past the last 30 messages it stays right under the older-messages marker.

## Capability groups and run cards (#538)

- **Groups:** `[conductor] allow = [...]` switches on owner-level herdsman verbs for conductors; the lane hook allows
  a group's verbs from a conductor and refuses them from every other lane (`OWNER_ONLY`), and refuses an off group's
  `GROUP_OFF`. Unset, every group but `pause` and `set` is on. The groups: `design` (`spawn … design`), `clean`
  (`clean --yes`, `--orphans`, `--lane`), `hosts` (`hosts on|off|refresh`), `reclaim`, `accounts` (`accounts
  reset`), `runners` (#537's `runners …`), `pause` (`pause`, `resume`), `set`. The brief lists each, on or off. The
  conductor never edits `allow`: it proposes the change as a config card the owner applies. Its group verbs show in
  the thread as tool rows with their group (`thread-cmd`'s `group`).
- **Run cards:** `herdsman card post <thread> --cmd "<command>" --why "…" --blast "…" --dir <folder> [--timeout
  600]` posts the exact command, why, what it touches and where it runs (the main host, that folder) as card
  `r<n>` (`run-card`, with the command's sha). The owner settles it: `herdsman card approve <id> [--sha <prefix>]`
  or `card deny <id>` (owner-only, `OWNER_ONLY` in any lane), or the card's Telegram buttons. An approval covers the
  exact text (`CARD_CHANGED` if the sha the owner saw differs), runs once (`ALREADY_SETTLED`) and only within 15
  min of the post (`EXPIRED`). herdsman runs it with `sh` in the folder under the timeout, then posts `run r1
  approved by owner, ran in <dir>: exit <code>` and the output's last 20 lines (secrets masked) to the thread.
  Events: `run-card`, `run-settled` (`by: owner`, id, sha, approved), `run-done` (code, tail).
- **Hard nos** (`CARD_FORBIDDEN`, at posting and, from a conductor's own shell, in the hook): printing or sending a
  secret's value, deleting production data, force-pushing or rewriting history, turning off branch protection or
  herdsman's gates. It is a pattern guard over the command's words, best effort like `deny_commands`.

## Accounts (#387)

The conductor uses its role's `account`/`accounts` or #302's `fallback` chain. When `check` sees the awake conductor's
usage-limit banner (or a sign-out), it rests that account (else that harness), closes the pane, and resumes the
**same session** on the next one (Claude Code `--resume <id>`). The thread shows `switched to claude:B (claude:A
resting until 18:40)`. A harness that can't resume gets the thread so far in its kick. A sign-out also adds a
`<harness> is signed out: [sign in]` line; the button opens #303's setup terminal on the main host. The conductor
never edits the account list itself: that change is a `config propose` card.

## Secrets (#387)

- `herdsman secret request <thread> <NAME> --why "…"` shows a masked-input card with [save] [dismiss], on paired
  owner devices only (elsewhere the card says so).
- [save] stores the value in the main host's OS store (#441, docs/providers.md) as `herdsman/<fleet>/<NAME>`
  (`<fleet>` is the thread's, or `all`), never in argv. Events, the chat and lanes never see it. The thread records
  only `secret <NAME> set ✓ (by owner)`.
- A cloud host's provider key (docs/hosts.md, e.g. `BOAT_API_KEY`) works saved either way: from the fleet's thread
  (`herdsman/<fleet>/<NAME>`, for a host only that fleet fits) or as `all`.
- A fleet's `env` refers to it as `KEY = "secret:<NAME>"`. At spawn herdsman delivers it (the fleet's item, else
  `all`'s) through a 0600 file the pane's shell sources, deleted after the start (docs/providers.md).
- `herdsman secret ls` and `herdsman secret rm <fleet|all> <NAME>` are owner-only.

## Telegram (removed, #557)

The Telegram bridge (#387) is gone: the board and web push are the only owner UI. A leftover `[telegram]` table in
fleet.toml loads with a warning (`herdsman doctor` shows a `!` row); drop it.

