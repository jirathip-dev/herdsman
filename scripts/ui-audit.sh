#!/usr/bin/env bash
# #63 gate 2 (docs/ui.md): headless Chrome opens the exported demo (`site/`) at 390×844, DPR 3, on each view,
# saves a screenshot and asserts the page's `?audit=<view>&check=1` JSON has "outliers":[].
# Usage: scripts/ui-audit.sh [site-dir] [shot-dir]; CHROME=<binary> overrides the browser. Use chrome-headless-shell:
# Chrome's new headless keeps windows ≥ 500 px wide, and the page fails the audit unless it is 390 (#68).
set -euo pipefail
# #560: one UI audit at a time on this host (scripts/ui-lock.sh).
[ -n "${UI_AUDIT_LOCKED:-}" ] || UI_AUDIT_LOCKED=1 exec "$(dirname "$0")/ui-lock.sh" "$0" "$@"
site=${1:-site} shots=${2:-ui-390}
chrome=${CHROME:-$(command -v google-chrome || command -v chromium || command -v chromium-browser || echo "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")}
port=${UI_AUDIT_PORT:-${HERDSMAN_PORT:-8390}}
mkdir -p "$shots" && shots=$(cd "$shots" && pwd)
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$site" >/dev/null 2>&1 &
srv=$!
trap 'kill $srv' EXIT
for _ in $(seq 50); do curl -fs "http://127.0.0.1:$port/" >/dev/null && break; sleep 0.1; done
# #71: a Chrome run that never returns (virtual time stuck) is killed after UI_AUDIT_TIMEOUT s and fails loudly,
# with its stderr tail, instead of hanging the job (`timeout` where the host has it: coreutils on the runner; else perl's alarm).
# #566: a hung run is retried twice, each on a fresh profile. Every hang goes to <shots>/hangs.txt; a run still hung
# after that is no failure: it goes to <shots>/flaky.txt (CI's soft `ui-flaky` check), and a hung check run skips the view's check.
limit=${UI_AUDIT_TIMEOUT:-60} to=$(command -v timeout || command -v gtimeout || true)
: > "$shots/hangs.txt" && : > "$shots/flaky.txt"
hung() { [ "$1" = 124 ] || [ "$1" = 137 ] || [ "$1" = 142 ]; }
run() { # url, extra args
  local rc=0 n
  for n in 1 2 3; do
    rc=0; once "$@" || rc=$?
    hung "$rc" || return "$rc"
    echo "$v ${2%%=*}: try $n hung" >> "$shots/hangs.txt"
  done
  echo "$v ${2%%=*}" >> "$shots/flaky.txt"
}
once() {
  local prof rc=0 hold=(perl -e 'alarm shift; exec @ARGV' "$limit"); prof=$(mktemp -d)
  [ -n "$to" ] && hold=("$to" -k 5 "$limit")
  "${hold[@]}" "$chrome" --headless --no-sandbox --disable-gpu --hide-scrollbars --user-data-dir="$prof" --no-first-run \
    --window-size="${size:-390,844}" --force-device-scale-factor="${dpr:-3}" --virtual-time-budget=8000 "${@:2}" "$1" 2>"$prof.err" || rc=$?
  if [ "$rc" != 0 ]; then
    echo "$v: chrome ${2%%=*} exited $rc$(hung "$rc" && echo ", killed after ${limit} s: hung")" >&2
    tail -5 "$prof.err" >&2
  fi
  rm -rf "$prof" "$prof.err"
  return "$rc"
}
fail=0
# #141: `busy/host-ranked` is the `host-ranked` view of <site>/busy (scripts/host-fixtures.sh), shot as busy-host-ranked.png.
# #157: `hosts/host` and `hosts/sheet`: the host cards (one offline) and a lane card naming its host; #234 `hosts/host-chart`:
# mini's load/cpu chart open. #267: the chart and the busy and healthy fixtures at 1440 too.
# #367: `queue/queue-repos` and `queue/queue-chain` (also @1440): the Queue fixture's chips, groups, chain and `+ N more`.
# #370: every tab at 1440 in farm, kitchen and terminal; `row-loop` (a row sprite's two frames, the same node across a
# patch), `moment-row` (a moment on its row, once, resumed after a patch), each `-still` under reduced motion; `empty`.
# #386: `chat` and `chat-thread` (also @1440): the [chat] tab's thread list and one thread. #397: `chat-error`, a start error over it.
# #406: `chat-viewer`, `chat-viewer-zoom`, `chat-viewer-multi`: the image viewer fitted, zoomed and on the second of two.
# #403: `chat-send`, `chat-long`, `chat-kb`, `chat-back` and `chat-wait` (its themes, ascii and still): the phone composer.
# #408: `chat-older` and `chat-mid` (also @1440): a long thread's older page loaded, then scrolled half way, its header pinned.
# #409: `chat-code` and `chat-copied`: a reply's short and long-line code boxes, and [copy]'s `copied` flash.
# #405: `chat-new`, `chat-archive`, `chat-archived`, `chat-thumbs`: the empty new thread, a row swiped left to [archive],
# the archived fold and three images waiting above the box.
# #407: `chat-pin`, `chat-fold`, `chat-pill` (and -stopping, -reconnecting, -offline), `chat-queue`, `chat-queue-edit`,
# `chat-steer`, `chat-menu`, `chat-undo`: the pinned send, a folded turn, the pill's states, the queue and its edit, the
# long-press menus and the undo toast.
# #423: the garden: `herd-garden` (-busy, -held, -still: the shop window's sun, cloud and rain), its tabs and moments,
# Queue and Lanes in garden-dark too (`^garden-dark`), and both garden schemes on the contact sheet.
# #492: `chat-handoff`: a handoff's choice card, the target repo's threads and [new thread].
# #516: `herd-dupes`: herd.json repeating a lane's name, refreshed 5 times: one card per name.
# #365: `queue/queue-backlog`: acme-api's 100-issue backlog searched, filtered and ticked.
# #450: `loading` (a fast load shows no strip, a slow one the strip), `loading-tap` (a tap during a load opens its sheet),
# `stale` (`as of 3m ago`) and `first-load` (the empty outlines).
for v in queue queue-open queue-lanes queue-change queue-drive queue-result config lanes lanes-now sheet sheet-full sheet-parked sheet-other sheet-drag host busy/host-ranked healthy/host-ranked hosts/host hosts/host-chart hosts/sheet hosts/host-chart@1440 busy/host-ranked@1440 healthy/host-ranked@1440 events pipeline pipeline-custom herd1 herd2 herd-kitchen herd-kitchen-busy herd-kitchen-held herd-kitchen-still herd-garden herd-garden-busy herd-garden-held herd-garden-still herd-nodone herd-night herd-night-storm herd-scroll herd-dupes herd-nav queue-scroll queue/queue-repos queue/queue-chain queue/queue-backlog queue/queue-repos@1440 queue/queue-chain@1440 toast needs-later needs-pending chat chat-thread chat-thread@1440 chat-error chat-viewer chat-viewer-zoom chat-viewer-multi chat-code chat-copied chat-handoff chat-send chat-long chat-kb chat-back chat-older chat-mid chat-mid@1440 chat-wait chat-wait~kitchen chat-wait~terminal chat-wait-still ascii-chat-wait chat-new chat-archive chat-archived chat-thumbs chat-pin chat-fold chat-pill chat-pill-stopping chat-pill-reconnecting chat-pill-offline chat-queue chat-queue-edit chat-steer chat-menu chat-undo \
  queue-rows events-rows events@1440 events-rows@1440 events-open events-open@1440 events-all queue-rows~kitchen events-rows~kitchen queue~kitchen lanes~kitchen host~kitchen events~kitchen queue~garden lanes~garden host~garden events~garden queue~garden^garden-dark lanes~garden^garden-dark queue~terminal lanes~terminal lanes-now~terminal host~terminal events~terminal theme-switch look look~kitchen moment-still loading loading~kitchen loading~terminal loading~garden loading-still loading-tap stale first-load loading@1440 loading~kitchen@1440 loading~terminal@1440 \
  $(for v in queue lanes host events loading; do echo "ascii-$v ascii-$v~kitchen"; done) ascii-queue~terminal ascii-lanes~terminal \
  $(for v in queue lanes host events; do echo "$v@1440 $v~kitchen@1440 $v~terminal@1440"; done) row-loop row-loop~kitchen row-loop-still moment-row moment-row~kitchen moment-row-still moment-row~garden moment-row~garden^garden-dark row-loop~garden empty empty~kitchen empty~terminal \
  $(for s in github-dark solarized-dark gruvbox nord dracula paper farm kitchen garden-light garden-dark; do echo "queue^$s busy/host-ranked^$s"; done); do
  [ -n "${UI_AUDIT_VIEWS:-}" ] && [[ " $UI_AUDIT_VIEWS " != *" $v "* ]] && continue # #266: only these views (space-separated)
  # #241: `<view>~<theme>` is the view in that theme (`?theme=`), shot as <view>-<theme>.png. #244: `<view>^<scheme>`
  # in that colour scheme (`?scheme=`), shot as <view>-scheme-<scheme>.png: the contact sheet, from the real CSS.
  # #337: `<view>@1440` at the desktop width (1440×900, DPR 2), shot as <view>-1440.png.
  wd=$([[ $v == *@* ]] && echo "${v#*@}" || true) v=${v%%@*} size=390,844 dpr=3
  [ -n "$wd" ] && size=$wd,900 dpr=2
  sc=$([[ $v == *^* ]] && echo "${v#*^}" || true) v=${v%%^*}
  th=$([[ $v == *~* ]] && echo "${v#*~}" || true) v=${v%%~*} flags=()
  dir=$([[ $v == */* ]] && echo "${v%/*}/" || true) view=${v##*/} v=${v//\//-}${th:+-$th}${sc:+-scheme-$sc}
  v=$v${wd:+-$wd}
  url="http://127.0.0.1:$port/${dir}index.html?audit=$view${th:+&theme=$th}${sc:+&scheme=$sc}${wd:+&w=$wd}"
  case $v in herd-garden*) url+="&theme=garden" ;; esac # #423
  case $v in herd-kitchen*) url+="&theme=kitchen" ;; herd-nodone) url+="&theme=kitchen&nopose=done" ;; esac # #172, #239
  case $v in moment-still|herd-kitchen-still|herd-garden-still|herd-nav|loading-still|chat-wait-still|row-loop-still|moment-row-still) flags=(--force-prefers-reduced-motion) ;; esac # #241, #239, #291, #305, #370
  run "$url&shot=1" --screenshot="$shots/$v.png" ${flags[@]+"${flags[@]}"} >/dev/null || fail=1
  run "$url&check=1" --dump-dom ${flags[@]+"${flags[@]}"} > "$shots/$v.html" || fail=1
  if grep -qx "$v --dump-dom" "$shots/flaky.txt"; then echo "$v: hung 3 times, not checked (ui-flaky)"
  elif ! python3 - "$shots/$v.html" "$v" "$shots/$v.png" "$dpr" <<'EOF'
import html, json, os, re, struct, sys, zlib
m = re.search(r'<pre id="audit">(.*?)</pre>', open(sys.argv[1]).read(), re.S)
r = json.loads(html.unescape(m.group(1))) if m else {"outliers": ["no audit output"]}
def png(path):  # 8-bit RGB(A), non-interlaced: Chrome's screenshots
    b = open(path, "rb").read()
    i, idat, w = 8, b"", 0
    while i < len(b):
        n, t = struct.unpack(">I4s", b[i:i + 8])
        if t == b"IHDR": w, h, _, ct = struct.unpack(">IIBB", b[i + 8:i + 18])
        if t == b"IDAT": idat += b[i + 8:i + 8 + n]
        i += n + 12
    bpp, raw, rows, prev = (4 if ct == 6 else 3), zlib.decompress(idat), [], bytearray(w * (4 if ct == 6 else 3))
    for y in range(h):
        f, line = raw[y * (len(prev) + 1)], bytearray(raw[y * (len(prev) + 1) + 1:(y + 1) * (len(prev) + 1)])
        for x in range(len(line)):
            a, c, u = line[x - bpp] if x >= bpp else 0, prev[x - bpp] if x >= bpp else 0, prev[x]
            p = a + u - c
            line[x] = (line[x] + [0, a, u, (a + u) // 2, a if abs(p - a) <= abs(p - u) and abs(p - a) <= abs(p - c) else u if abs(p - u) <= abs(p - c) else c][f]) & 255
        rows.append([tuple(line[x:x + 3]) for x in range(0, len(line), bpp)])
        prev = line
    return rows
# #281: a row sprite's pixels are crisp: an art pixel is 4.5 device px (1.5 css px at DPR 3), so no device pixel inside its
# box is a blend of its two neighbours (smoothing paints those along every edge).
if r.get("sprites") and os.path.exists(sys.argv[3]):
    px, blend, d = png(sys.argv[3]), 0, int(sys.argv[4])  # d: device px per css px (#266: 2 at 1440)
    mid = lambda a, p, b: a != b and p not in (a, b) and all(min(x, z) <= y <= max(x, z) for x, y, z in zip(a, p, b))
    for x0, y0, w, h in r["sprites"]:
        for y in range(round(y0 * d) + 1, round((y0 + h) * d) - 1):
            for x in range(round(x0 * d) + 1, round((x0 + w) * d) - 1):
                blend += mid(px[y][x - 1], px[y][x], px[y][x + 1]) or mid(px[y - 1][x], px[y][x], px[y + 1][x])
    print(f"{sys.argv[2]}: {len(r['sprites'])} row sprites, {blend} smoothed pixels")
    if blend: r["outliers"].append(f"{blend} smoothed pixels in the row sprites")
print(f"{r.get('view', sys.argv[2])}: {r.get('controls', 0)} controls, {len(r['outliers'])} outliers")
for o in r["outliers"]: print("  " + o)
sys.exit(1 if r["outliers"] else 0)
EOF
  then fail=1; fi
  rm "$shots/$v.html"
done
# #166: the chat sheet opened and swipe-closed under a CDP screencast: no near-black frame, the page never moves.
# #453: one retry, for the runner's Chrome renderer crash; a real regression fails both runs.
close() { ${to:+"$to" -k 5 90} node "$(dirname "$0")/ui-close.mjs" "http://127.0.0.1:$port/index.html" "$chrome" "$shots"; }
close || { echo "ui-close: retrying once" >&2; close; } || fail=1
exit $fail
