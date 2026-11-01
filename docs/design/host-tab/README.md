# Host tab — design mockup (#141, phase 1)

`index.html` shows the Host tab as 4 status-ranked groups with text gauges and plain-language results, plus the
bracket tab bar. It links `assets/serve.css` unchanged (by a relative path) and adds one `<style>` block whose rules
use tokens only. Those rules move to `serve.css` in phase 2. The page is static: groups and rows open natively
(`<details>`), with no script. Screenshots: `host-tab-390.png` (390 px, DPR 3, full page) and `host-tab-1440.png`.
At 1440 px the mockup puts the three fixtures side by side so they fit one screen. The real board keeps its one
column.

**Data.** There are three fixtures, stacked (the brief asks for two, busy and healthy; the third shows a group
floating up):

| fixture | source |
|---|---|
| **busy** | The owner's Host-tab screenshot (staging, 2026-11-01): load/cpu 4.24 against 2.5, `HOST_BUSY`, disk 42 GB, worktrees 12.2 GB, board up 59m, tick 0s, herdr 0.9.5/0.9.0, `exit 4: "herdsman-impl-138" …`, synergy's `REFUSED SERIAL_BUSY …`, 1 device, sent 11m ago. Memory pressure (normal), swap-out (0), the accounts and the `8m` ages were not in the screenshot, so they come from the live board. |
| **now** | The live board (`127.0.0.1:7777`, Host tab, about 19:15): load/cpu 1.84, normal, 0, 42 GB free, worktrees 12.4 GB, board up 65m, herdr still 0.9.5/0.9.0, herdsman `"herdsman-rev-129-r2" …` 8m ago, synergy `SERIAL_BUSY` 8m ago, sendmeter paused, accounts A/B/C 0 of 3 capped, one push device and one paired device. |
| **healthy** | The live values with two changes: herdr restarted (server 0.9.5) and synergy's serial slots clear. |

Paths are `~`-masked.

## The groups (Q1)

```
● MACHINE   spawns held · load 4.24 / 2.5      ▾   ← header: dot, name, one-line summary (hidden while open), chevron
load/cpu  ▕████████████▏ 4.24 / 2.5                ← label 9ch · gauge 14ch · value
memory    normal pressure
disk      ▕███░░░░░░░░░▏ 42 GB free
          floor 10 GB · worktrees 12.2 GB          ← meta 11 px under the value
spawns    held · no new lanes until load/cpu  ▾    ← a row with a raw line: tap shows it, wrapping
          is under 2.5
          ┌ spawn gate HOST_BUSY: load/cpu 4.24 > 2.5 ┐
```

| group | rows (from what the page already reads) |
|---|---|
| **MACHINE** | load/cpu, memory pressure, swap-out, disk (+ worktrees), spawn gate (`host::admit`) |
| **SERVICES** | board, tick, herdr (Services, #98), accounts (folded: one row `0 of 3 capped`, tap for A/B/C with lane-hours and usage), deepseek balance when configured |
| **FLEETS** | one row per fleet: the last `driven` result in plain words, its age, the raw line on tap; an orch-driven fleet shows `orch · running` |
| **ALERTS** | push on/off, devices, last sent (#87); the alert subscriptions and paired devices with their `[remove]` / `[unpair]` |

- **Group status** is the worst row in the group: red, then amber, then green. A paused fleet is grey and does not
  count (see the open questions).
- **Order:** problem groups first, red before amber. Ties, and healthy groups, keep the order MACHINE, SERVICES,
  FLEETS, ALERTS. In **now**, SERVICES and FLEETS (amber) sit above MACHINE (green).
- **Open state:** problem groups start open and healthy ones start collapsed, one 44 px line each. A tap toggles a
  group, and the choice is kept in `sessionStorage` (`host:<group>` → open). In **now**, MACHINE is green but open,
  as if tapped. Once a group's status changes, its default applies again: a stored choice records the status it was
  made under, and is dropped when that status changes, so a group that turns red always opens.
- **Header:** the whole width is the tap target (44 px). It shows an 8 px dot, the name in caps (bold, letter-spaced),
  the one-line summary at 11 px (ellipsised, hidden while open) and a chevron. A problem header takes its colour for
  the dot, name and summary, on a 12 % tint of that colour.
- **Rows:** a 3-column grid: the label at 9ch (muted), the gauge at 14ch, and the value. A row with no gauge spans
  the last two columns. Status words (`up 65m`, `held`, `waiting`, `started`, `paused`) take a colour; the rest is
  default text. The fix is one 11 px line under the value: `→ <what to do>: <command>`, with the command in the
  accent (the #138 code style).

## Gauges (Q2)

`▕` + 12 cells + `▏`, in the mono font. `█` filled, `░` empty in `#30363d`; the fill and both edges take the status
colour.

| gauge | fill | green | amber | red |
|---|---|---|---|---|
| load/cpu | load ÷ `limits.load_per_cpu` | < 80 % | 80–100 % | > 100 % (full bar) |
| swap-out | pages/2s ÷ `limits.swapout_rate` | < 80 % | 80–100 % | > 100 % |
| disk (inverted) | `min_free_gb` ÷ free: the bar fills as free space shrinks toward the floor | free > 12.5 GB | 10–12.5 GB | < 10 GB (`DISK_LOW`) |

The fill rounds to whole cells (`round(12 × ratio)`, clamped to 0–12). Memory pressure is a word
(normal/warn/critical), with no gauge.

## The tab bar (Q3)

`[queue·2] [lanes] [host] [events]`. Every label is in brackets, like the board's controls.

- **Inactive tabs** are muted.
- **The active tab** looks like a pressed key: the label box (28 px tall) takes the accent fill `--l` with `--bg`
  text, and a 2 px `--l` bar runs along the top of its whole cell. Today the active tab is default text with a
  green top bar.
- **A tab that needs you** shows `·N` in `--r` bold. On the active tab, the count turns dark (`--bg`) on the fill,
  because red on blue reads poorly.
- The count is the same "needs you" total the cards use (`#needs`).
- Each cell is the full quarter of the bar and 52 px tall, so the tap target is bigger than 44 px. `[queue·2]` fits
  in 97 px at 390.

## Plain-language mapping (Q4), for phase 2's table in `serve.rs`

A `driven` event carries `step`, `issue`, `code` and `result`. The row's text comes from (`code`, `step`); the raw
`result` stays for the tap. `{…}` values are parsed from the raw line, which keeps the same wording today's
refusals use.

| code / outcome | word (colour) | plain text | what clears it |
|---|---|---|---|
| ok, step `spawn` impl | started (green) | `#138 (impl)` | — |
| ok, step `spawn` rev / fix | started (green) | `review #129 (round 2)` / `fix #129 (round 1)` | — |
| ok, step `merge` | merged (green) | `#N into staging` | — |
| ok, step `clean` | cleaned (green) | `#N's lane` | — |
| `EXIT` 4 | started (green) | `#138 (impl) · prompt not confirmed yet` | check the lane (open question 2) |
| `SERIAL_BUSY` | waiting (amber) | `{n} database slots full` | `(#2755, #2782, #2891 must merge first)` |
| `HOST_BUSY` | held (amber) | `the Mac is busy: load/cpu 4.24 over 2.5` | `starts again when it drops` |
| `DISK_LOW` | held (amber) | `{gb} GB free, under the {floor} GB floor` | `herdsman clean --yes` |
| `ALL_CAPPED` | waiting (amber) | `every {role} account is capped` | `until {reset}` |
| `CAP` | waiting (amber) | `{n} of {max} {role} lanes busy` | `a lane finishes first` |
| `PAUSED` | paused (grey) | — | `herdsman resume {fleet}` starts it again |
| `ROUND_LIMIT` / `UPDATE_LIMIT` | needs you (red) | `#N used its {n} review rounds` | `your call on the card` |
| `WORKTREE_DIRTY` | needs you (red) | `#N's worktree has unsaved work from no lane` | `herdsman kill` it, or spawn `--supersede` |
| `CI_NOT_GREEN` | waiting (amber) | `#N's CI is not green yet` | — |
| `NO_VERDICT` / `STALE_VERDICT` / `VERDICT_FAIL` | waiting (amber) | `#N has no PASS for its head` | `a review round` |
| `ERROR`, other `EXIT` n | failed (red) | the raw first line, shortened | — |
| no `driven` yet | — (grey) | `nothing yet` | — |

The herdr row on a version mismatch shows `update half done`, with the fix line
`→ restart herdr to finish the update: herdr update --handoff`. Per the conductor note, it names no scheduled job.

## Rules (#63) as checked

- Text is 13 px and meta is 11 px.
- Group headers and rows with a raw line are at least 44 px tall. The `[remove]`/`[unpair]` buttons are the board's
  28 px controls with their 44 px ring.
- No sideways scroll: `scrollWidth` equals the viewport at 390 and at 1440, and no element passes the right edge
  (measured in headless Chrome).
- Free text wraps (a plain row is clamped at 3 lines). A raw line opens on tap inside its row, at 11 px on the
  `#161b22` surface, and wraps anywhere.

## Phase 2 notes

- **Line budget.** `src/` net ≤ +60 is tight. The mapping table is about 20 lines. The group renderer can reuse the
  `services()` rows (which already carry `item, tone, detail, fix`) by tagging each row with its group and splitting
  MACHINE out of `host_tab`. The disk row moves from `services()` to MACHINE. The gauge is a 5-line helper. Most of
  the rest is CSS (`assets/` does not count) and about 10 lines of `serve.js` for open-state and ordering. Ordering
  could also be server-side, which costs `src/` lines instead.
- **Green token.** `@property --g` (the gait angle) voids `--g` as a colour, so `.g` text renders in default text
  colour today. The mockup uses `--ok:#3fb950`. The owner's #138 decision (q43) renames the gait property to
  `--gait`, after which `--ok` can go and `var(--g)` works. Whichever lands second should drop the workaround.
- **Audit (Gate 2).** Two checks: the groups' `data-t` ranks never increase down the tab, and `nav a.on` is exactly
  one element, with a computed background equal to `--l`.

## Open questions for the owner

1. **Paused fleets:** grey and not a problem (the mockup), or amber as today? Amber keeps FLEETS amber for as long as
   sendmeter is paused, so the tab would never show all green.
2. **`exit 4`** (spawned, prompt not confirmed): green `started` plus the clause `prompt not confirmed yet` (the
   mockup), or amber?
3. **Where accounts and paired devices go:** the mockup folds accounts into SERVICES as one row and puts paired
   devices under ALERTS. Should either be its own group (that would make 5)?
4. **Disk amber threshold:** the brief's 80 % rule puts amber under 12.5 GB free. Today's disk row turns amber under
   20 GB (`host::AMBER_FREE_GB`). Should both use the 80 % rule (recommended: one rule for every gauge), or should
   the gauge keep 20 GB?
5. **Badge on the active tab:** a dark `·2` on the accent fill (the mockup), or a small red chip?
