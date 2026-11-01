# Queue graph — design mockup (#115, phase 1)

`index.html` is the Queue tab drawn as a `git log --graph` style tree. It uses `assets/serve.css` unchanged
(linked relatively) plus one `<style>` block of graph rules that only use tokens. It is static: rows open
natively. Screenshots: `queue-graph-390.png` (390 px, DPR 3, full page) and `queue-graph-1440.png`.

**Data:** synergy's queue from `herdsman queue synergy ls --json` and `herdsman board synergy` on 2026-11-01:
16 queued items in queue order, `#2714` (impl, not queued) last as today, the serial marks, the states and
ages, and `#2683`'s real PR line (PR #2885, CI ✓, rev PASS r2). Titles are neutral placeholders. The header's
stall line and the `done today` rows are illustrative. Below synergy, a labelled **fixture** fleet shows
the edge cases synergy doesn't have today: a fork (`├─` `└─`), a 5-deep chain clamped with `┆`, a blocker
outside the queue (`! #9`) and a red state (`!`).

## What a row is

```
✓ #2683  ready                         8h   ← gutter · #n · state + tags · age (summary line, as today)
│ Placeholder: split the settings …         ← .tl title line, unchanged
└─○ #2841  queued [db]                      ← hangs off its blocker
  └─○ #2684  queued
    └─○ #2685  queued                       ← depth 3: gutter is 6 columns + the dot
```

- **Gutter:** the node line's prefix in box-drawing text (`│ ├─ └─`) plus the state dot, in `--mute`.
  Each solid glyph also draws its stroke as a 1 px CSS line down the whole row, so the tree has no gaps
  at line height 1.5 and runs past an open card. The row separator starts at the `#n` column, so it never
  crosses the graph.
- **Dots:** `●` a lane works on it (impl / review(n) / fix(n)), `○` queued, `✓` ready, `!` a red state
  (blocked, needs-human, dead). The dot takes the state word's colour (`tone()`).
- **Tags:** `db` (11 px, muted) for an item queued `--serial`; `! #b` (red) when the blocker isn't in the
  queue view; `on #b` (muted) on a clamped row.
- **Depth:** the dot sits at column 2·d. Depth ≤ 3 draws in full, so the gutter is at most 6 columns.
  Deeper rows draw at depth 3 with a dashed `┆─` elbow and an `on #b` tag.
- **Open row:** the same card as today (state · lane · age, PR line, `blocked by #b`, `[▲ up] [▼ down]`),
  indented to the `#n` column so the graph line runs down its left.

## The rules, measured on the mockup

At 390 and 1440 px, headless Chrome: no sideways scroll, nothing past the right edge, text only 11 or 13 px,
every control 28 px, every row ≥ 44 px, and the widest gutter is 7 characters (6 columns + the dot). The
hit rings are the existing `button::after`, unchanged.

## Algorithm (phase 2)

1. Take `queue::view` in queue order. Item `i` with `blocked_by = b`: if `b` is in the view, `i` is a child
   of `b`; otherwise it's a root and gets the `! #b` tag.
2. Roots keep queue order, then the non-queued open issues (as today). Children keep queue order under their
   parent. Rows go out depth-first.
3. Per row, the server emits its gutter cells: for each ancestor level, `│` if that ancestor has later
   siblings, else blank; then `├─` or `└─` for the row itself; then the dot. The dot carries `k` when the row
   has children. Rows with `d > 3` emit levels 1–2, then `┆─`.
4. CSS (moves from the page's `<style>` into `serve.css`): `details.q.d0…d3`, `--gw`, the 4-column summary
   grid, and the stroke pseudo-elements.

Serve.js needs no change: the first `summary>span` is still `#n` (the gutter is an `<i>`), so `?issue=`
deep links, `key()` and #65's in-place patching work as before. `sig()` now includes the gutter text, so
a change in the tree re-renders the row, which is what we want.

## Open questions for the owner

1. **Display order vs. queue order.** A child is drawn under its blocker, even when the queue has it
   earlier. Synergy has a case today: `#2870` (queue position 12) is blocked by `#2753` (position 13),
   so it's drawn after `#2753`. That's the order drive can actually start them. OK?
2. **▲/▼ on the graph.** Today ▲/▼ move past the neighbour in the flat queue. On the graph, a flat
   neighbour is often in another branch. For example, `#2752 ▲` would move it before `#2685`, and the graph
   wouldn't change, so the tap looks like it did nothing. The mockup's card moves a row among its
   **siblings** instead (`#2683 ▼` → after `#2752`, the next root). That's still `mv --before/--after`, with
   no CLI change, and an only child gets no arrows. Recommend: siblings.
3. **`!` dot vs. a waiting child.** The mockup draws a queued child as `○`, since the hang already says it
   waits; `!` is kept for red states and the `! #b` tag. The alternative is `!` on every blocked item, which
   would turn every chain red. Recommend: `○`.
4. **remove / top.** The brief says the card has move/remove/top. The current card has only ▲/▼. The
   mockup keeps the card as it is. Should phase 2 add `[remove]` and `[top]`? They'd need `rm` and `mv --top`
   web ops, which costs `src/` lines against the +25 budget.
5. **Pre-existing bug, not this issue:** `.g` text (ready, PASS, CI ✓) renders in the default text colour.
   `@property --g` (Corral's gait angle) registers `--g` as an `<angle>`, which voids the green token.
   `.pairp.g` already works around it with the hex value. That's why `ready` and `✓` look white in these
   screenshots. Fix separately?
