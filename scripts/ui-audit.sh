#!/usr/bin/env bash
# #63 gate 2 (docs/ui.md): headless Chrome opens the exported demo (`site/`) at 390×844, DPR 3, on each view,
# saves a screenshot and asserts the page's `?audit=<view>&check=1` JSON has "outliers":[].
# Usage: scripts/ui-audit.sh [site-dir] [shot-dir]; CHROME=<binary> overrides the browser. Use chrome-headless-shell:
# Chrome's new headless keeps windows ≥ 500 px wide, and the page fails the audit unless it is 390 (#68).
set -euo pipefail
site=${1:-site} shots=${2:-ui-390}
chrome=${CHROME:-$(command -v google-chrome || command -v chromium || command -v chromium-browser || echo "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")}
port=${UI_AUDIT_PORT:-8390}
mkdir -p "$shots" && shots=$(cd "$shots" && pwd)
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$site" >/dev/null 2>&1 &
srv=$!
trap 'kill $srv' EXIT
for _ in $(seq 50); do curl -fs "http://127.0.0.1:$port/" >/dev/null && break; sleep 0.1; done
# #71: a Chrome run that never returns (virtual time stuck) is killed after UI_AUDIT_TIMEOUT s and fails loudly,
# with its stderr tail, instead of hanging the job (`timeout` where the host has it: coreutils on the runner).
limit=${UI_AUDIT_TIMEOUT:-60} to=$(command -v timeout || command -v gtimeout || true)
run() { # url, extra args
  local prof rc=0; prof=$(mktemp -d)
  ${to:+"$to" -k 5 "$limit"} "$chrome" --headless --no-sandbox --disable-gpu --hide-scrollbars --user-data-dir="$prof" --no-first-run \
    --window-size=390,844 --force-device-scale-factor=3 --virtual-time-budget=8000 "${@:2}" "$1" 2>"$prof.err" || rc=$?
  if [ "$rc" != 0 ]; then
    echo "$v: chrome ${2%%=*} exited $rc$([ "$rc" = 124 ] || [ "$rc" = 137 ] && echo ", killed after ${limit} s: hung")" >&2
    tail -5 "$prof.err" >&2
  fi
  rm -rf "$prof" "$prof.err"
  return "$rc"
}
fail=0
for v in queue queue-open lanes sheet sheet-drag host events herd1 herd2 herd-night herd-night-storm herd-scroll queue-scroll toast; do
  url="http://127.0.0.1:$port/index.html?audit=$v"
  run "$url" --screenshot="$shots/$v.png" >/dev/null || fail=1
  run "$url&check=1" --dump-dom > "$shots/$v.html" || fail=1
  if ! python3 - "$shots/$v.html" "$v" <<'EOF'
import html, json, re, sys
m = re.search(r'<pre id="audit">(.*?)</pre>', open(sys.argv[1]).read(), re.S)
r = json.loads(html.unescape(m.group(1))) if m else {"outliers": ["no audit output"]}
print(f"{r.get('view', sys.argv[2])}: {r.get('controls', 0)} controls, {len(r['outliers'])} outliers")
for o in r["outliers"]: print("  " + o)
sys.exit(1 if r["outliers"] else 0)
EOF
  then fail=1; fi
  rm "$shots/$v.html"
done
exit $fail
