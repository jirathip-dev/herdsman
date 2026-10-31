# UI rules (#58, #63)

The phone page (`herdsman serve`) has one look. Two CI gates enforce it; a red gate blocks
`herdsman merge` (required checks, #24).

## Tokens

The first `:root` block of `assets/serve.css` is the only place a raw px length may appear; `herd.css` uses the same tokens.

| token | value | use |
|---|---|---|
| `--fs` | 13px | all text |
| `--fs-meta` | 11px | secondary text: ages, reasons, captions |
| `--fs-input` | 16px | inputs, textareas, selects (iOS zooms below 16 px) |
| `--fs-code` | 32px | the pairing code's digits (#59) |
| `--ctl` | 28px | the visible height of every control, the Herd strip and `[×]` |
| `--hit` | 44px | the minimum tap area |
| `--s1` … `--s4` | 4 / 8 / 12 / 16px | margins, paddings, gaps, offsets |

Other lengths are sums of tokens (`calc(var(--hit) + var(--s2))`); radii, borders and widths stay literal.

## Controls

- **Bracket style:** a control is its lowercase label in the accent colour between `[` and `]`
  (CSS pseudo-elements), 28 px tall, no fill; inverted while pressed. A select hides behind
  its `[opus ▾]` label.
- **Hit area:** the `]` pseudo-element spans an 8 px ring around the control, so the tap area is
  ≥ 44×44 px. Controls sit 16 px apart so rings never overlap. Rows (queue `summary`, lane rows,
  horses) are full-width tap rows at least 44 px tall, not 28 px controls.
- **Choices (the one exception, owner-approved 2026-10-31):** ask and menu options are sentences:
  full width, wrapping, ≤ 4 lines then an ellipsis (full text in `title`), `[`/`]` on the first
  line, stacked 8 px apart, the recommended one first with an 11 px `(recommended)`.
- **Type scale:** text is 13 or 11 px, nothing else; inputs 16 px (iOS zooms below 16 px).
- **Herd strip:** one 28 px bar on top of each Herd page with `[×]` inside it on the right; the
  counts share its line or wrap to an 11 px second line in the same bar, and are never ellipsised (#65 A).
- **Labels, not sentences (#68):** a button holds its label (in a span that ellipsises, so `[`/`]` never
  overlap text). A control's runtime result or error (alerts, pair, stop/kill/clean, send, answer) is an 11 px
  `p.st` under it that wraps, red on error; #66's toast is for page actions. Unpaired, `[enable alerts]` dims
  with "pair this phone first" under it, and a Pair this phone button opens the pairing panel (#59): in the flow under the header (or the sheet's
  Pair button), never over a card; a tap closes it, and after 10 min it says "code expired" with a Pair button.
- **No text past the screen (#68):** every text box shrinks (`min-width:0`) and wraps anywhere (paths, shas,
  URLs, `code`); one-line cells (`.nw`, `.el`, `.tl`) ellipsise.
- Names never wrap vertically (`.nw` cells stay on one line, horse labels clamp at 2 lines);
  text that does not fit ellipsises. Nothing scrolls sideways at 390 px.

## Gates

1. **Style lint** — `cargo test --test ui_tokens`: a sizing property (`font-size`, `font`,
   `height`, `min-height`, `line-height`, `margin*`, `padding*`, `gap`, `top/left/right/bottom`,
   `inset`) with a px value outside the token block, a font size that is not a `--fs` token, a control
   `height` that is not `--ctl`, or an inline `style=` with px in the rendered page fails (`file:line value`).
2. **Browser audit** — the `ui` CI job: headless Chrome opens the exported demo at 390×844, DPR 3,
   on Queue, Queue with a row open, Lanes, a Lanes sheet, a parked lane's sheet, the sheet dragged, Host, Events, Herd page 1 and page 2 (by day), Herd at night and at night in a storm.
   `assets/ui-audit.js` (only in the `--demo` export, `?audit=<view>&check=1`) fails on a control
   not 28 px tall (a choice: over 4 lines), text not 11/13 px (inputs 16), a tap area under 44 px, sideways scroll
   (on Herd pages too: no box past the page's sides), a name over 2 lines, text overflowing its box without an
   ellipsis, a Herd strip/`[A]`/`[×]` not 28 px or `[A]` not inside the strip left of `[×]` (#71), a lane horse not 132±1 px wide or a queued one not 53±1 (#65), or
   text under the weather caption or the demo's weather switcher with the page scrolled to its end, or ellipsised
   strip text, also with a fixture of the owner's counts (13 queued, 11 done, 5 idle, 3 working) (#65 A).
   Two more views (#65) prove updates never move the scroll: `herd-scroll` scrolls Herd page 1 to 400 px and
   pushes a changed fixture through `herdData()` (a label, a band move, a lane and a queued issue gone, one lane
   added); `queue-scroll` opens every Queue row, scrolls to 400 px and pushes a changed section through `frag()`.
   Each fails if the scroll moved, a row closed, or the MutationObserver saw more nodes leave than were removed.
   `sheet-drag` (#70): the sheet's handle is ≥ 56 px; a synthetic −200 px pointer drag moves the sheet mid-drag,
   release lands on a detent (50 %, 78 %, full), and a close leaves it shown and sliding off-screen for a frame
   before `hidden`. Virtual time runs no frames, so there rAF is a 16 ms timer and the slide is read as its target.
   `herd-night` (#71) checks Auto at 06:59/07:00/18:59/19:00 (night/day/day/night), taps `[A]` through
   day → night → auto → day → night (label, icon, stored value, scene each time), then loads the page again in an
   iframe (virtual time runs no reload) and fails unless it comes back at night.
   `toast` (#66) submits a ▲ form through the page's fetch path with a fixture redirect and fails unless the
   one-line toast showed, is gone 5 s later, and `location.search` has no `msg`.
   `sheet-parked` (#83) opens the demo's parked lane (no pane, its work on an open PR) and fails unless the card
   has its `parked · work on PR #n` line, `[clean]` is disabled with its reason in `p.st`, `[stop]` and `[kill]`
   are hidden, the chat says there is no pane, and nothing in the sheet is in the red (danger) colour.
   #68: on every view, a box whose content is wider than it (`scrollWidth > clientWidth + 1`, tap rings off)
   fails unless it ellipsises or scrolls by design, and so does any box past the screen's right edge. `host` and
   `sheet` run each inline control's failure path with a canned long reply (Alerts: an error, then the old 401
   pair text, leaving it unpaired; kill refused, kill's dry run, clean refused, a send error) and re-check after
   each step. The demo fixture carries a 60-char agent name, a 200-char title, a Thai title, an 80-char path
   and a 140-char event line.
   Screenshots are uploaded as the `ui-390` artifact on every run.
   A Chrome run that does not return in `UI_AUDIT_TIMEOUT` s (default 60; needs `timeout`) is killed and fails its view
   with its stderr tail (#71); the job itself stops at 20 min.

Run both locally:
```sh
cargo test --test ui_tokens
cargo build --release && HERDSMAN_HOME=demo FLEET_CONFIG=demo/fleet.toml FLEET_STATE=demo/events.jsonl \
  ./target/release/herdsman serve --export site --demo && scripts/ui-audit.sh site ui-390
```
`scripts/ui-audit.sh` finds `google-chrome`/`chromium`; set `CHROME=<binary>` otherwise.

**Exceptions:** each is listed by name with its reason in `tests/ui_tokens.rs` (`EXCEPTIONS`). **Adding an
exception needs the owner's OK.**
