// Generated from the Rust payload types by `just types` (#375); never edit by hand.

/**
 * `/herd.json` (Herd mode, docs/serve.md).
 */
export type Herd = { fleets: Array<HerdFleet>, others: Array<HerdLane>, host: HerdHost, };

export type HerdFleet = { name: string, color: string, lanes: Array<HerdLane>, queue: Array<HerdQueued>, needs: Array<number>, };

/**
 * A lane in the Herd; #423: `grow`, its issue's growth stage (a theme's `stages`).
 */
export type HerdLane = { name: string, act: string, grow: string, issue?: number | null, title?: string | null, role: string, harness: string, model?: string | null, effort?: string | null, account?: string | null, host?: string | null, state: string, shown: string, tone: string, age: string, coat: string, gone: boolean, };

export type HerdQueued = { issue: number, title?: string | null, blocked_by?: number | null, coat: string, };

export type HerdHost = { load?: number | null, limit: number, pressure?: string | null, swapouts?: number | null, swap_limit: number, gate?: string | null, };

/**
 * `/lane?agent=`: the sheet's chat. A lane with no pane sends no `said`, `notices`, `acts`, `pane` or `status`.
 */
export type Chat = { said?: Array<JsonValue> | null, older: boolean, notices: Array<Notice>, harness: string, acts: Array<string>, pane?: string | null, lines: Array<string>, live: number, status?: string | null, before: number, more: boolean, };

export type Notice = { at: number, text: string, };

/**
 * `/live`'s `ev`: an event's seq, time and theme moment (`serve::moment`); #370 its fleet and issue, for row moments.
 */
export type Moment = { seq: number, ts: number, k?: string | null, f: string, n?: number | null, };

/**
 * #269: API v1's `needs`, the cards above every tab as this device sees them: the snapshot's time `snap` (0: none;
 * #450: the strip under the header shows its age once over 2 min), the open asks, each blocked lane's card, the
 * `needs-human` issues, and `choosing` (`[chat·N]`). `rw`: the board takes forms (the answer boxes); `live`: and is no
 * export; `paired`: the device may answer a lane or an issue; `pair`: the page offers Pair this phone.
 */
export type NeedsTab = { now: number, rw: boolean, live: boolean, paired: boolean, pair: boolean, mtime: string, snap: number, choosing: number, asks: Array<NeedAsk>, blocked: Array<Blocked>, issues: Array<NeedIssue>, };

/**
 * An open ask's card: `who` (fleet · #issue · agent · age), its `kind` line, the host it was imported `from`; #412: a
 * design's PNGs by name (`/design/<fleet>/<n>/<i>` serves the `i`th); #517: the `thread` it was asked in, its `title`.
 */
export type NeedAsk = { ask: Ask, who: string, kind: string, from?: string | null, pngs: Array<string>, thread?: string | null, title?: string | null, };

/**
 * A blocked lane's card (`who`: fleet · #issue · lane; `push` 0 while a countdown runs): unpaired, its `activity`;
 * else a harness `menu`, or the pane's last lines (`tail`).
 */
export type Blocked = { name: string, who: string, push: boolean, activity?: string | null, menu?: Menu | null, tail?: string | null, };

/**
 * A harness menu: its title, reason and command, the `countdown` (s) before it auto-denies, the masked question and
 * options, `h` the hash of the menu the card shows (`POST /answer`).
 */
export type Menu = { title: string, reason: string, command: string, countdown?: number | null, question: string, options: Array<string>, h: string, };

/**
 * #130: a `needs-human` issue's card (`pending`: one more round recorded, no lane yet): `who`, title, `why`, the last
 * verdict (`pass`: its sha, `stale` not the head; else `fail`'s finding), the PR line (`repo`: none, the fleet is
 * not configured), `mergeable` and `merge` (merge anyway is offered); #417: the verdict's screenshot `shots`.
 */
export type NeedIssue = { fleet: string, issue: number, pending: boolean, who: string, title: string, why: string, pass?: string | null, stale: boolean, fail: string, pr: PrLine, repo?: string | null, mergeable: string, merge: boolean, shots: Array<string>, };

/**
 * #269: API v1's `lanes`, the Lanes tab as this device sees it: state sections, repo groups inside (`error`: no
 * fleet.toml, `failed`: herdr unread). `rw`, `live`, `paired`, `pair` as `NeedsTab`'s.
 */
export type LanesTab = { now: number, rw: boolean, live: boolean, paired: boolean, pair: boolean, error?: string | null, failed: boolean, sections: Array<LaneSection>, };

/**
 * A state section (`tone`) and its repo groups.
 */
export type LaneSection = { name: string, tone: string, repos: Array<LaneRepo>, };

export type LaneRepo = { name: string, lanes: Array<LaneRow>, };

/**
 * A Lanes row: its avatar (`avatar`, herdr's `state` with `act`, #423 `grow`, `atone` and `coat`), name, `state` in
 * `tone` for `ago`, the `why` line (`why_tone`), the activity (`activity`, or the `now` parts), `gone` (no pane), the
 * conductor `thread` it opens; the hidden card: `head`, `shown`, `facts`, the PR line (`issue` and `pr`, `repo` none when its
 * fleet is not configured), `dead` and the blocked card.
 */
export type LaneRow = { name: string, role: string, state: string, act: string, grow: string, atone: string, coat: string, tone: string, ago: string, why?: string | null, why_tone: string, activity: string, now?: LaneNow | null, gone: boolean, thread?: string | null, head: string, shown: string, facts: string, issue?: number | null, pr?: PrLine | null, repo?: string | null, dead: boolean, blocked?: Blocked | null, };

/**
 * #276: a `now:` line: the spinner word `w`, the action `n`, the elapsed time `t`, the full text as `title`.
 */
export type LaneNow = { title: string, w?: string | null, n?: string | null, t?: string | null, };

/**
 * #269: API v1's `chat`, the [chat] tab (#386): a card per repo (`all` first) with its threads, then every thread's
 * view (`error`: fleet.toml unread). `rw`: a paired device on the live board writes.
 */
export type ChatTab = { now: number, rw: boolean, error?: string | null, repos: Array<ChatRepo>, threads: Array<ThreadView>, };

/**
 * A repo's card in colour `k`: its threads newest first, the archived ones apart; #404: its empty thread's
 * `harness` chip.
 */
export type ChatRepo = { repo: string, k: string, rows: Array<ThreadRow>, archived: Array<ThreadRow>, harness: Chip, };

/**
 * #404: a composer chip: `name`'s current value `cur` (none: the default) and its choices `all`.
 */
export type Chip = { name: string, cur?: string | null, all: Array<string>, };

/**
 * A thread's row: unread, title, age, its `state` (choose, working, asleep, …), the threads it links to, the last line;
 * #460: the ids of the config proposals `waiting` for the owner.
 */
export type ThreadRow = { id: string, title: string, age: string, unread: boolean, state: string, waiting: Array<string>, links: Array<Link>, line: string, };

/**
 * #413: a linked thread's chip: `from t5`, `continued in t6`.
 */
export type Link = { w: string, t: string, };

/**
 * One thread: its bar (`ds`: fleet · harness · model · state), its last messages, the live line's `state`; #407:
 * working, the turn's start `since`, and the `queued` messages above the box; #460: the config proposals `waiting`; #404:
 * its model and effort `chips`.
 */
export type ThreadView = { id: string, title: string, ds: string, state: string, waiting: Array<string>, since?: number | null, queued: Array<QueuedMsg>, chips: Array<Chip>, page: ThreadPage, };

/**
 * #407: a message queued for the turn's end, by its id `q`.
 */
export type QueuedMsg = { q: number, text: string, };

/**
 * #408: a page of a thread's messages; `older`: the seq to load the messages before, else the `started` line's time
 * and conductor folder `dir`; #618: the thread's `id`, and whether it is `archived` (opened read-only, [restore]).
 */
export type ThreadPage = { id: string, archived: boolean, msgs: Array<Msg>, older?: number | null, started: number, dir: string, };

/**
 * A thread's message (its event's `seq` and `ts`) by `kind`, `text` its words: `note` (a system line: `retry`,
 * `signin` on `host`; #541: a conductor's answer, its `ask` and `open` while the owner may [change] it), `handoff` (the title), `msg` (`from` who wrote it, its `files`), `cmd`, `ask` (the question;
 * `answer` once decided, else `open`), `config` (the diff, `from` the proposal's id; #460: `open` unless stale), `secret` (why, `from` its name),
 * `resumed` (the session), `sleep`, #407 `fold` (a finished turn's run of commands, `cmds`); `link` the thread it
 * links to.
 */
export type Msg = { seq: number, ts: number, kind: string, text: string, link?: Link | null, from: string, files: Array<FileRef>, retry: boolean, signin?: string | null, host: string, ask?: Ask | null, routine: boolean, answer?: Answered | null, open: boolean, cmds: Array<string>, };

export type FileRef = { name: string, size: number, };

/**
 * A decided ask: the option `n` chosen (none: an "other" answer), its text, by whom, when.
 */
export type Answered = { n?: number | null, choice: string, by?: string | null, ts: number, };

/**
 * #365: one open issue of the Queue tab's backlog (`data-bl`, by number), as `gh::backlog` reads it.
 */
export type BacklogIssue = { number: number, title: string, state: string, created: string, updated: string, labels: Array<string>, ac: boolean, pr: boolean, };

/**
 * `POST /send`: a reply to lane `agent`, or to the setup pane `setup`.
 */
export type Send = { agent: string, setup?: string | null, text: string, };

/**
 * `POST /answer`: menu option `choice` of the menu hashed `h`.
 */
export type Answer = { agent: string, choice: string, h: string, };

/**
 * `POST /act`: stop | kill | clean | close; `confirm` `1` does what a dry run showed.
 */
export type Act = { agent: string, op: string, confirm: string, };

/**
 * `POST /setup`: start the setup pane `name` on `host`.
 */
export type Setup = { host: string, name: string, };

/**
 * #265: API v1's `state`, the board in one read: the newest event's `seq`, the step's snapshot age (none saved: null) and the
 * `mtime` a POST sends back (STALE when fleet.toml changed since). #266: the server's clock `now` and UTC offset `tz` (s),
 * the Events tab's time groups.
 */
export type State = { seq: number, now: number, tz: number, age?: number | null, mtime: string, theme: string, fleets: Array<Fleet>, queue: Array<Queued>, lanes: Array<Lane>, asks: Array<Ask>, hosts: Array<Row>, services: Array<Row>, alerts: Array<Row>, };

export type Fleet = { name: string, repo: string, color: string, paused: boolean, };

export type Queued = { fleet: string, issue: number, title?: string | null, state: string, blocked_by?: number | null, };

/**
 * `state` is herdr's (or `dead`, `parked`); `shown` and `tone` (g, a, r, m) are the Lanes row's words.
 */
export type Lane = { name: string, fleet?: string | null, issue?: number | null, title?: string | null, role: string, harness: string, model?: string | null, state: string, shown: string, tone: string, since: number, };

export type Ask = { id: string, fleet: string, issue?: number | null, question: string, options: Array<string>, recommend?: number | null, ts: number, };

/**
 * A Host tab row: HOSTS' (`detail`: its state in words), SERVICES' and ALERTS' (push); `fix` clears a red or amber one.
 */
export type Row = { name: string, tone: string, detail: string, fix: string, };

/**
 * #267: API v1's `host`, the Host tab at `now`: HOSTS (`hosts`), FLEET DISK (`disk`, `orphans`, `leftovers`, `spawns`),
 * SERVICES (`services`, `theme`: `detail` its error, `accounts`, `deepseek`), FLEETS and ALERTS (`push`, and on a live
 * board its `key`, `subs` and paired `devices`). `rw`: the board takes forms; `live`: and is no export (an export's
 * forms are inert); `mtime` the forms send back.
 */
export type HostTab = { now: number, rw: boolean, live: boolean, mtime: string, hosts: Array<HostCard>, disk?: Disk | null, orphans: number, leftovers?: Leftovers | null, spawns?: Row | null, services: Array<Row>, theme: Row, accounts: Array<Account>, deepseek?: Balance | null, fleets: Array<FleetRow>, push: Row, key: string, subs: Array<Sub>, devices: Array<Paired>, notify: Array<Notifier>, };

/**
 * #234: a host's card: its dot and state `words` in `tone`, the lanes' 24 h series `n`, its rows, `to` the [off]/[on]
 * its head offers (with the `lanes` running on it), and the logins its [sign in ▾] lists.
 */
export type HostCard = { name: string, main: boolean, dot: string, tone: string, words: string, n: string, rows: Array<Cell>, to?: string | null, lanes: number, setups: Array<string>, };

/**
 * #234: a host card's row, status `t`: a gauge's `bar` then its value `b` (bold, in `bt`), `text`, the limit `of` and
 * muted `notes`. `key`: a metric with history: its 24 h sparkline `s` (memory: pressure's `p`), its limit `l` and, in an
 * export, its chart `x` inline.
 */
export type Cell = { k: string, t: string, key: string, bar?: Bar | null, b?: string | null, bt: string, text: string, of: string, notes: Array<string>, s: string, p: string, l?: number | null, x: string, };

/**
 * #141: a 12-cell text gauge: its status `t`, `n` cells full, and how far past the limit (`pct`, else empty).
 */
export type Bar = { t: string, n: number, pct: string, };

/**
 * #164: the worktree total (`gb`, `?` unread) against the budget, and how long ago it was walked.
 */
export type Disk = { bar: Bar, gb: string, budget: number, ago?: string | null, };

/**
 * #186: processes still running in closed lanes' worktrees, and the memory they hold.
 */
export type Leftovers = { n: number, mb: string, };

/**
 * An account: `capped`, its cap line in `tone`, its usage `meters` and `pace` (or the `hint` why none), its lane hours.
 */
export type Account = { name: string, capped: boolean, tone: string, cap: string, meters: Array<Meter>, pace?: string | null, hint?: string | null, hours: string, };

export type Meter = { label: string, pct: number, };

/**
 * The `DeepSeek` balance (currency and amount) and whether it is `low`, else the `hint` why none was read.
 */
export type Balance = { balance?: string | null, low: boolean, hint: string, };

/**
 * #141: a fleet's last `driven` result in words (`word` in `tone`, the plain rest `text`), its age and `raw` line.
 */
export type FleetRow = { name: string, tone: string, word: string, text: string, ago: string, raw?: string | null, };

/**
 * #18: a webpush subscription, removable by `id`.
 */
export type Sub = { id: string, label: string, created: number, };

/**
 * #59: a paired device, removable by `hash`.
 */
export type Paired = { hash: string, label: string, created: number, used: number, };

/**
 * #440: a global `[[notify]]` in words (System → Notifications): `web push` or `command`, and its events.
 */
export type Notifier = { kind: string, on: string, };

/**
 * #268: API v1's `queue`, the Queue tab at `now`: a card per fleet, the ones needing you first (`error`: fleet.toml
 * unread). `rw`: the board takes forms; `live`: and is no export (an export's forms are inert); `mtime` they send back.
 */
export type QueueTab = { now: number, rw: boolean, live: boolean, mtime: string, error?: string | null, fleets: Array<QueueCard>, };

/**
 * #162: a fleet's card in its colour `k`: its serial groups' names and paths, `need` rows needing you, a paused one's
 * line (`paused`: since and lanes), else its drive line and red `notes`; its rows, `done` today (the newest 5 of
 * `done_n`), the backlog and the [config] sheet.
 */
export type QueueCard = { name: string, k: string, serial: Array<string>, paths: Array<string>, need: number, paused?: string | null, drive: Drive, notes: Array<string>, rows: Array<QueueRow>, done: Array<DoneRow>, done_n: number, backlog?: Backlog | null, config: ConfigSheet, marks: { [key in string]: Marks }, };

/**
 * #576: a queued row's marks by its issue (`QueueCard.marks`): `design`, `verify` (`on`, `off`, none: by `ui_paths`),
 * `repro`, its serial groups and `needs` tags, and the stage `note` its row shows (`verify(n)`, `verify skipped: …`).
 */
export type Marks = { design: boolean, verify?: string | null, repro: boolean, serial: Array<string>, needs: Array<string>, note?: string | null, };

/**
 * #166: a fleet's drive line in `tone`: its words, a tap's `meta` lines then the last pass's actions (`pass`: one
 * ran, so none is `nothing was due`).
 */
export type Drive = { tone: string, line: string, meta: Array<string>, pass: boolean, acts: Array<DriveAct>, };

/**
 * A pass's action: its issue (`#N`) or step, in words, its raw `code` on tap.
 */
export type DriveAct = { who: string, words: string, code: string, };

/**
 * A Queue row: `#issue` in its shown `state` (`tone`) for `age`, counting from `t`; its row sprite's `act`, #423 `grow`
 * (the growth stage) and `coat`; `queued` (with `check`: from the backlog), its serial `groups`, `blocked_by`; titles; the open card: the
 * pipeline `stage`, the current `lane` (`live`: herdr lists it), the PR line, `dead`, red `notes` (a land note, an
 * answer not delivered), the decision taken by default
 * (and its ask to change it), #517 its `open` ask (answerable here), [▲]/[▼] (`up`, `down`: the rows to move before or after) and the lanes list.
 */
export type QueueRow = { issue: number, state: string, tone: string, age: string, t: number, act: string, grow: string, coat: string, queued: boolean, check: boolean, groups: Array<string>, blocked_by?: number | null, title?: string | null, stage: Array<Stage>, lane: string, live: boolean, pr: PrLine, dead: boolean, notes: Array<string>, decided?: string | null, ask?: Ask | null, open?: NeedAsk | null, mv: boolean, up?: number | null, down?: number | null, lanes: Array<QueueLane>, };

/**
 * #173: a pipeline chip: its kind's glyph `g` and class `k`, `at` before (`-1`), on (`0`) or after (`1`) where the
 * issue is, a check's PASS at the PR head `ok`.
 */
export type Stage = { name: string, k: string, g: string, at: number, ok: boolean, };

/**
 * #169: `[issue] [PR #M] [checks]` and CI (`green`, `failed`, else pending; none: no PR read yet), then the last
 * verdict in `vtone` and its round, `form` the form-only rounds used.
 */
export type PrLine = { pr?: number | null, run?: string | null, ci?: string | null, verdict?: string | null, vtone: string, rounds: number, form?: string | null, };

/**
 * #205: a lane the issue had, newest first: herdr's `state` (`gone`) in `tone`, its spawn's age; `link`: `live`
 * opens its Lanes sheet, `gone` its stored transcript, empty none.
 */
export type QueueLane = { name: string, state: string, tone: string, ago: string, link: string, };

/**
 * A row of `done today`: its land note (else state), age and merged PR.
 */
export type DoneRow = { issue: number, note: string, age: string, pr?: string | null, };

/**
 * #365: the open issues not queued and with no lane, by number, as last read `ago`.
 */
export type Backlog = { issues: { [key in string]: BacklogIssue }, ago: string, };

/**
 * #162: a fleet's [config] sheet: the config file (`path`, `file`), GENERAL, the Pipeline group's data (`pipe`),
 * LIMITS, ROLES (with `rw`, pickers from `models`, `harnesses` and their efforts), the Conductor, ALERTS.
 */
export type ConfigSheet = { path: string, file: string, repo: string, local: string, integration: string, effort?: string | null, color_set: boolean, env: Array<string>, node_modules: boolean, builtin: boolean, review_rounds: number, update_rounds: number, stall_minutes: number, ask_minutes: number, max_impl?: number | null, max_rev?: number | null, line_cap?: number | null, serial: Array<string>, deny: Array<string>, required: Array<string>, merge_label?: string | null, notify: Array<string>, global: Array<string>, pipe: JsonValue, models: Array<string>, harnesses: Array<Harness>, roles: Array<RoleRow>, conductor?: Conductor | null, };

export type Harness = { name: string, efforts: Array<string>, };

/**
 * A role as set: harness (taking `efforts`), model and effort (unset: what the lane gets, `model_default`,
 * `effort_default`).
 */
export type RoleRow = { name: string, profile: string, harness: string, model?: string | null, effort?: string | null, efforts: Array<string>, model_default: string, effort_default: string, accounts: Array<string>, mcp: Array<string>, };

/**
 * #401: the fleet's conductor (`default`: impl's), its choices: models the fleet's roles name.
 */
export type Conductor = { default: boolean, profile: string, harness: string, model?: string | null, effort?: string | null, efforts: Array<string>, models: Array<string>, };

/**
 * #259's contract: the events after `since` (#266: up to `until`) in `list`; over 500, none and `refresh` (read `state` again).
 */
export type Events = { seq: number, refresh: boolean, list: Array<Ev>, };

/**
 * One logged event; `k` is the theme moment it plays (merged, fail, needs-you). #266: as the Events tab says it
 * (`serve::words`): `glyph`, `tone`, `text`, shown under [important] (`imp`), its row sprite's `act` and `coat` (its
 * agent's), `plain` (a fold's line), and its issue's `pr` (else the branch's checks `run`).
 */
export type Ev = { seq: number, ts: number, fleet: string, issue?: number | null, type: string, data: { [key in string]: JsonValue }, k?: string | null, glyph: string, tone: string, text: string, imp: boolean, act: string, coat: string, plain: string, pr?: number | null, run?: string | null, };

/**
 * #576: API v1's `issue/<fleet>/<n>`: its recorded verdicts, newest first.
 */
export type IssueView = { fleet: string, issue: number, verdicts: Array<Verdict>, };

/**
 * A verdict: its `role` (the stage: `rev`, `verify`, or the gate's), round, sha, `verdict` (PASS or FAIL), `findings`
 * (verdict.md after its first line); the newest one's screenshot `shots` by name (`/shot/<fleet>/<n>/<i>` serves the
 * `i`th); a `--repro` PASS's `Repro:` block.
 */
export type Verdict = { role: string, round: number, sha: string, verdict: string, ts: number, findings: string, shots: Array<string>, repro?: Repro | null, };

/**
 * #420: a `Repro:` block: `run` (the commands and both outputs) or `verify` (the quoted reason), its lines as `text`.
 */
export type Repro = { kind: string, text: string, };

/**
 * #576: API v1's `lessons`: per fleet, its `lessons` setting (`weekly`, `off`), the `last` lesson run and when `tick`
 * runs the `next` (none: `off`).
 */
export type Lessons = { fleets: Array<LessonFleet>, };

export type LessonFleet = { fleet: string, lessons: string, last?: LessonRun | null, next?: number | null, };

/**
 * A lesson issue: its lane's spawn time `ts` and the issue's state now.
 */
export type LessonRun = { issue: number, ts: number, state: string, };

/**
 * #576: `lessons/<fleet>/digest?days=N`: `herdsman lesson digest`'s text.
 */
export type Digest = { fleet: string, days: number, text: string, };

/**
 * #555: API v1's `setup/<pane>` (the old board's `/lane?setup=`): the setup pane's masked `lines` and every
 * `https://` URL in it (a wrapped one joined), or `closed`.
 */
export type SetupScreen = { lines: Array<string>, urls: Array<string>, closed: boolean, };

/**
 * #555: API v1's `roles?fleet=`: the fleet's [config] sheet (its roles, pickers and conductor) and `[chat]`'s
 * new-thread defaults (`POST chat` writes them).
 */
export type Roles = { fleet: string, config: ConfigSheet, chat: ChatDefaults, };

/**
 * #404: `[chat]`'s harness, model and effort (none: the conductor role's), and the `harnesses` it may name.
 */
export type ChatDefaults = { harness?: string | null, model?: string | null, effort?: string | null, harnesses: Array<Harness>, };

/**
 * A POST's answer: `code` is OK, a `REFUSED` code, or ERROR; `message` the line the board toasts.
 */
export type Reply = { ok: boolean, code: string, message: string, };

export type Event = { 
/**
 * #259: the log's monotonic number, set on append (an old row's is its line number); 0 before.
 */
seq?: number, ts: number, fleet: string, issue?: number | null, type: string, } & ({ [key in string]: number | string | boolean | Array<JsonValue> | { [key in string]: JsonValue } | null });

export type IssueState = { fleet: string, issue: number, state: string, lane?: string | null, branch?: string | null, 
/**
 * The latest lane's reasoning effort, as spawned.
 */
effort?: string | null, rounds: number, 
/**
 * Extra rounds the owner allowed (`round-extended`, #130); #309: the latest grants the round after any stage's.
 */
extra: number, granted: number, 
/**
 * #123: passed `--update` rounds, apart from `rounds`; `updating` while one is open (state `fix(n)+uk`).
 */
updates: number, updating: boolean, last_ts: number, last_event: string, 
/**
 * Latest agent per role (impl, rev).
 */
lanes: { [key in string]: string }, 
/**
 * (sha, PASS|FAIL, the check stage whose rounds it counts) in log order; logs before #173: `rev`.
 */
verdicts: Array<[string, string, string]>, 
/**
 * #173: the pipeline stage of the latest spawn, the latest check and build roles, review rounds per check stage.
 */
stage: string, check?: string | null, build?: string | null, stage_rounds: { [key in string]: number }, 
/**
 * #228: `FAIL (form-only)` verdicts that did not count toward the rounds.
 */
form: number, };

export type Waiting = { fleet: string, issue: number, 
/**
 * The lane this row is for; every lane of an issue gets one.
 */
lane: string, 
/**
 * drive | owner | ci | none
 */
who: string, what: string, since: number, next_cmd?: string | null, };

/**
 * What the last look at a host saw (`<state dir>/hosts.json`): detected tags and `$HOME` (probed hourly), reachability,
 * load/cpu, free memory and disk in GB.
 */
export type HostSeen = { tags: Array<string>, home: string, probed: number, seen: number, up: boolean, load?: number | null, mem?: number | null, disk?: number | null, 
/**
 * #235: a remote host's failed checks from `health` (`✗` keeps lanes off it, `!` warns), with the probe.
 */
health: Array<string>, 
/**
 * #234: memory pressure 0 normal, 1 warn, 2 critical; swap-out pages/2s; the top memory users (remote hosts).
 */
pressure?: number | null, swap?: number | null, top: Array<string>, 
/**
 * #234: memory and disk size in GB (a bar shows the used part).
 */
mem_total?: number | null, disk_total?: number | null, 
/**
 * #254: when it last had a live agent (a cloud host's idle stop).
 */
busy: number, 
/**
 * #255: a cloud host's gh token, as its card says it.
 */
github: string, 
/**
 * #630: its set workspace roots.
 */
roots: Array<HostRoot>, };

/**
 * A root as the last look saw it (`hosts.json`, the host card, `/api/v1`).
 */
export type HostRoot = { path: string, state: string, free_gb?: number | null, };

/**
 * #234: one row a minute per host in `<state dir>/host-history.jsonl`, written by the fast step; 24 h kept.
 */
export type HostRow = { ts: number, host: string, load_cpu?: number | null, pressure?: number | null, swap_out?: number | null, mem_free_gb?: number | null, disk_free_gb?: number | null, lanes: number, 
/**
 * #352: MB per `host::groups` group, only while `/live?tab=host` was seen in the last 2 min (else no `ps` walk).
 */
procs?: [number, number, number, number, number] | null, };

export type JsonValue = number | string | boolean | Array<JsonValue> | { [key in string]: JsonValue } | null;
