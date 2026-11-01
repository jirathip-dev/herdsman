# Chat blocks — design mockup (#138, phase 1)

`index.html` is the lane sheet's chat drawn as Corral-style role blocks, at the sheet's full detent. It uses
`assets/serve.css` unchanged (linked relatively) plus one `<style>` block of block rules that use tokens only.
Those rules move to `serve.css` in phase 2. The page is static: blocks open natively (`<details>`), and one inline
handler toggles `[show all]`. Screenshots: `chat-blocks-390.png` (390 px, DPR 3, full page) and
`chat-blocks-1440.png`.

**Data:** lane `impl #129` (this repo), read on 2026-11-01 with `herdr pane read --source recent --format ansi`.
These parts are real, with their real SGR colours mapped as below: the `Update src/overlap.rs` diff (lines 49–55,
including the word-level `-`/`+` highlights), `Now run the gates.`, the running gates command (`L=…/scratchpad`
masked), `Ran 2 shell commands` and the spinner `✽ Gusting… (2m 47s · ↓ 1.8k tokens)`. The prose style
(**bold** and `code` runs) comes from lane `impl #130`'s closing summary. These parts are illustrative, added to
cover every block type: the Plan text with its heading and lists, the failing `cargo test … overlap` output, the
`src/state.rs` update and the gap line. Paths are `~`-masked.

## What a block is

```
┃ claude                                   ▾   ← header: role in its colour, chevron; the whole width toggles
┃ Plan                                           ← prose, 13 px, minimal markdown
┃ Base is green. The bug is in `src/…`
┃ $ cargo test --locked overlap   exit 101 ▾   ← a tool run's header is its call; `$` in the accent
┃ ┌ output panel, 11 px, tinted ──────────┐
┃ │ test …::skips_closed ... FAILED        │    ← terminal colours, mapped to tokens
┃ └ [show all]  20 of 29 lines ───────────┘
  Ran 2 shell commands                          ← status: quiet, recessed, no header
```

- **Roles and edges** (2 px left edge on a `#161b22` rounded surface, which is the existing `.b` bubble colour):
  you `--l` blue · assistant `--p` purple (new, Q1) · tool run `--a` amber · status `--mute` at 40 %, on a
  surface recessed toward `--bg`.
- **Header:** the whole width, 28 px visible with an 8 px ring above and below for a 44 px tap (#58). It shows
  the role (`you`, the lane's harness name such as `claude`, `dsh` or `hermes`) and a chevron. Collapsed, it adds
  a one-line ellipsised preview of the first line.
  A tool run's header is its call: `$ cmd` for a shell call, or the tool name in the accent plus its argument
  (`Update src/overlap.rs`). Open, the command wraps. A failed call shows a red `exit N` on the right (Q4), and a
  running one shows its elapsed time.
- **Collapse (Q4 as picked):** tool runs start collapsed; you and assistant blocks start open; status lines are
  not collapsible. A tap toggles a block. The state is kept per lane in `sessionStorage`
  (`chat:<agent>` → `{<block key>: open}`), where the block key is the block's first line plus its line offset
  in the transcript file. It is stable while the file grows, and changes only when the 5,000-line cap trims
  past the block.
- **Tool output:** a tinted panel (`--a` at 6 % over `--bg`), 11 px, with lines that **wrap inside the panel**
  and never scroll sideways (Q3). A line with a gutter (`52 -`, `52 +`, `49`) is a 3-column grid, so wrapped
  code stays to the right of its line number. Output over 20 lines shows its first 20 **logical** lines (counted
  before wrapping) plus `[show all]` and an 11 px `20 of N lines`; `[show all]` becomes `[show less]`.
- **Assistant markdown:** **bold**, `code` (blue on a 12 % blue tint), `-`/`1.` lists and `#` headings (bold
  13 px, since the type scale has no bigger size). The text is escaped and only spans are added; HTML never
  passes through.
- **Live status line (#105):** the spinner line sits between the chat and the composer, in one 11 px line,
  with the glyph in `--a`. It is chrome, not transcript, so it never scrolls away.
- **Gap line:** where two reads have no overlap (the lane scrolled past unread), a dashed quiet line
  `┄ not captured: scrolled past between reads ┄` (see Q5).
- `[load more]` stays at the top and pages back through the rolling file.

## ANSI → tokens (measured on the two lanes)

Each colour maps to the nearest token by hue; terminal hex never reaches the page. A colour with low saturation
(max−min < 40) maps to the default text when it is light and to `.t-m` when it is dark or dim (SGR 2). Hue
bands: red < 12° or ≥ 330°, amber 12–70°, green 70–160°, cyan 160–200°, blue 200–255°, purple 255–330°.

| SGR seen | where | class |
|---|---|---|
| `38;2;248;248;242`, `255;255;255` | Claude default text, `⏺` of prose | none (`--fg`) |
| `38;2;153;153;153`, `136;136;136`, `117;113;94`, `2` | status, comments, line numbers | `.t-m` |
| `38;2;177;185;249` | Claude's rendered inline `code` | `.t-l` (drawn as the code span in prose) |
| `1` | Claude's rendered `**bold**` and headings | `.t-b` |
| `38;2;80;200;80`, `166;226;46`, `78;186;101` | diff `+`, identifiers, `⏺` of a passing call | `.t-g` |
| `38;2;220;90;90`, `255;107;128` | diff `-`, errors | `.t-r` |
| `38;2;230;219;116`, `215;119;87`, `220;129;97`, `255;193;7` | strings, the spinner | `.t-a` |
| `38;2;102;217;239`, `0;204;204` | keywords | `.t-c` (new token, Q1) |
| `48;2;61;1;0` / `2;40;0` (dark) | diff line background | `.bg-r` / `.bg-g` (15 % tint) |
| `48;2;92;2;0` / `4;71;0` (brighter) | changed words | `.bg-r2` / `.bg-g2` (35 % tint) |

The basic (30–37, 90–97) and 256-colour SGRs go through the same rule.

**Claude pre-renders markdown.** Its screen has no `**` or backticks: bold arrives as SGR 1 and inline code as a
`177;185;249` run. So for Claude the markdown is the ANSI mapping. The `**`/`` ` ``/`#` parser is for harnesses
that print raw markdown (dsh, Hermes), and both paths render the same spans.

## Phase 2 notes from the measurements

- **Why only ~40 lines:** herdr isn't capping it. A plain shell pane returns its scrollback (`recent` gave 112
  lines: 18 rows + 94 scrollback). Every Claude lane shows `scroll.max_offset_from_bottom: 0`: Claude Code draws
  in the alternate screen, so herdr has no scrollback for it, and `--source recent-unwrapped`/`visible` don't
  either. The rolling transcript is therefore the right fix, but it only holds what a read saw (Q5).
- **The screen is rewritten in place, not appended to:** `Running … · 2m 15s` becomes a finished call,
  `… +47 lines` folds, and the spinner moves. So append only the lines **above** the last block of #105's
  `chat()` body. The last block (from the last `⏺`/`❯`/`$` head down) stays provisional and is re-sent live,
  never written to the file until a newer head appears below it. Otherwise each read appends another
  `Running …` copy.
- **Overlap:** use the longest suffix of the file that equals a prefix of the new committed lines, compared with
  SGR codes stripped (a re-render can change the codes but not the text). No overlap writes one gap marker line.
- **Blocks from lines:** a block opens on `❯`/`>`/`›` (you), `⏺` + text (assistant), `⏺ Name(…)` / `$` / `┊`
  (tool run: the head line becomes the header, indented `⎿` lines become the panel with `⎿` dropped), or a
  muted-only line such as `Ran 2 shell commands` or `✻ Cogitated for 37s` (status). Claude colours `⏺` green
  for a passing call, red for a failed one and grey for a running one. That colour gives `exit N`, or `failed`
  when no code is printed.

## The rules, measured on the mockup

At 390 and 1440 px in headless Chrome, with every block as shown, all open with `[show all]` expanded, and all
collapsed: no sideways scroll, nothing past either edge, text only 11 or 13 px (the composer 16), every
control 28 px with a ≥ 44 px tap, and every block header 28 px visible with a 44 px tap.

## Open questions for the owner

1. **Two new colour tokens.** The board has blue, amber, red, green and grey, but no purple or cyan, and both
   Q2 (assistant = purple) and phase 2's map (magenta, cyan) need them. The mockup adds `--p:#bc8cff` and
   `--c:#39c5cf` to the token block, from the same GitHub dark palette as the existing four. The alternative,
   existing tokens only, gives the assistant edge `--mute` and maps cyan to `--l` and magenta to `--r`. Tool
   runs use `--a` (amber) for "orange"; a true orange would be a third token. **Recommend: add `--p` and `--c`.**
2. **`--g` is broken on today's board.** `@property --g` (the horse gait angle, `<angle>`) has the same name as
   the green token, so `var(--g)` computes to `0deg`: `.g` text renders as plain `--fg` and the active nav
   tab's underline has no colour (checked in headless Chrome: `.g` → `rgb(201, 209, 217)`, `--g` = `0deg`).
   The pairing panel already works around it with the hex. The mockup does the same for green. **Recommend:**
   rename the gait property to `--gait` in phase 2 (about 6 lines across `serve.css` and `herd.js`). That fixes `.g`
   everywhere, but it is a visible change on the board (green comes back), so it needs your OK.
3. **Wrap vs. scroll inside panels.** The mockup wraps (a gutter line keeps its number column). A sideways
   scroll per panel would keep code aligned, but `.chat` is `touch-action: pan-y`, so iOS blocks the inner
   pan-x unless the chat allows it too. **Recommend: wrap.**
4. **`exit N` on a failed tool header.** This is not in the issue, but a collapsed failing run otherwise looks
   the same as a passing one. Keep it, or drop it?
5. **Gaps.** The sheet reads every 5 s while it is open, but `check` reads every 5 min, and a busy Claude lane
   scrolls well over 40 lines in 5 min. So the file has gaps for the time nobody watched. Options:
   (a) accept the gaps and show the gap line, as mocked;
   (b) read working lanes more often in `check` (cheap: one `pane read` per lane);
   (c) for Claude lanes only, backfill from Claude's own session log, which is harness-specific and so against
   Q1.
   **Recommend: (a) now, (b) if the gaps are frequent in use.**
6. **The 1440 layout.** The sheet is full-width there today, so prose lines run about 180 characters. Cap the
   chat at about 100ch on wide screens, or leave it as is (not in the issue)?
