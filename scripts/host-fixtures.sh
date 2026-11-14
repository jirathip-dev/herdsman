#!/usr/bin/env bash
# #141 gate 2: the Host tab's two fixtures, exported beside the demo as <site>/busy and <site>/healthy for
# scripts/ui-audit.sh. Each is a copy of demo/ with a webpush device and 42 GB free; busy: load/cpu 4.24 over 2.5,
# docs' drive refused SERIAL_BUSY; healthy: load/cpu 1.84, docs' drive started #10; hosts (#157): healthy
# with two `[hosts]`, mini (every lane on it) and bazzite (offline, last seen 33m), and 24 h of host history (#234).
# Usage: scripts/host-fixtures.sh [site-dir] [drover-binary]
set -euo pipefail
site=${1:-site} bin=${2:-./target/release/drover}
for fx in busy healthy hosts; do
  d=$(mktemp -d)
  cp -R demo/. "$d"
  python3 - "$d" "$fx" <<'EOF'
import json, sys
d, fx = sys.argv[1:]
busy = fx == "busy"
r = json.load(open(f"{d}/runner.json"))
r["sysctl -n vm.loadavg"] = "{ 33.92 30.10 28.40 }\n" if busy else "{ 14.72 13.10 12.40 }\n"
r["df -k /"] = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/disk3s1 488245288 400000000 44040192 91% /\n"
if fx == "healthy":  # #237: both own jobs loaded (their files below), the fast step 40 s before export's now
    r["launchctl print"] = r["systemctl --user is-active"] = ""
    ts = max(json.loads(l)["ts"] for l in open(f"{d}/events.jsonl") if l.strip())
    open(f"{d}/drive-fast.at", "w").write(f"{ts + 20} 2.1\n")
json.dump(r, open(f"{d}/runner.json", "w"))
ts = max(json.loads(l)["ts"] for l in open(f"{d}/events.jsonl") if l.strip())
if busy:
    why = "serial #6 is unmerged (impl), #7 is unmerged (fix(1)), #8 is unmerged (review(1)); retry after they merge"
    d_ = {"step": "start", "cmd": "drover spawn docs 10 impl", "result": f"REFUSED SERIAL_BUSY: {why}", "code": "SERIAL_BUSY"}
else:
    d_ = {"step": "start", "cmd": "drover spawn docs 10 impl", "result": '"docs-impl-10" ["claude" "fleet-impl"]', "code": None}
with open(f"{d}/events.jsonl", "a") as f:
    f.write(json.dumps({"ts": ts - 420, "fleet": "docs", "issue": 10, "type": "driven", **d_}) + "\n")
toml = open(f"{d}/fleet.toml").read().replace("[fleet.docs]\n", '[fleet.docs]\nserial_label = "database"\n')
# #183: a paused fleet with an 11-char name, the longest label the Host tab's label column fits.
toml += '\n[fleet.acme-mobile]\nrepo = "acme-example/acme-mobile"\nlocal = "/srv/acme/acme-mobile"\nintegration = "staging"\npaused = true\n'
if fx == "hosts":
    toml += '\n[hosts.mini]\ntags = ["fast"]\n\n[hosts.bazzite]\nmachine = "bazzite"\n'
    seen = {"mini": {"tags": ["os:macos", "arch:arm64", "xcode"], "home": "/Users/demo", "probed": ts, "seen": ts, "up": True, "load": 1.84, "mem": 9.5, "disk": 42.0},
            "bazzite": {"tags": ["os:linux", "arch:x86_64", "docker"], "home": "/home/demo", "probed": ts - 1980, "seen": ts - 1980, "up": False,
                        "load": 0.41, "mem": 21.8, "mem_total": 32.0, "disk": 340.0, "disk_total": 476.0, "pressure": 0, "swap": 0}}
    json.dump(seen, open(f"{d}/hosts.json", "w"))
    # #234: 24 h of history (export's now is the last event + 60 s): mini a row a minute, 9 lanes from 5 h to 3 h ago
    # (load past 2.5, swap bursts, pressure warn), its disk falling 52 to 42 GB; bazzite every 5 min until 33 min ago.
    import random
    rnd, now, rows = random.Random(234), ts + 60, []
    for i in range(1440, 0, -1):
        t, hot = now - i * 60 + 1, 180 <= i < 300
        lanes = 9 if hot else 5 if 300 <= i < 600 else 3
        load = max(0.05, 0.18 + 0.27 * lanes + (rnd.random() - 0.5) * 0.25 + (rnd.random() * 0.9 if rnd.random() < 0.04 else 0))
        swap = 600 + rnd.random() * 3400 if hot and rnd.random() < 0.3 else 0
        mem = max(1.3, 13 - 1.15 * lanes + (rnd.random() - 0.5) * 0.8)
        rows.append({"ts": t, "host": "mini", "load_cpu": round(load, 2), "pressure": int(hot and 200 <= i < 260), "swap_out": round(swap),
                     "mem_free_gb": round(mem, 1), "disk_free_gb": round(52 - 10 * (1440 - i) / 1440, 1), "lanes": lanes})
        if i % 5 == 0 and i > 33:
            rows.append({"ts": t, "host": "bazzite", "load_cpu": round(0.15 + rnd.random() * 0.3, 2), "pressure": None, "swap_out": None,
                         "mem_free_gb": 21.8, "disk_free_gb": 340.0, "lanes": 0})
    open(f"{d}/host-history.jsonl", "w").write("".join(json.dumps(r) + "\n" for r in rows))
    evs = [json.loads(l) for l in open(f"{d}/events.jsonl") if l.strip()]
    open(f"{d}/events.jsonl", "w").write("".join(json.dumps({**e, "host": "mini"} if e["type"] == "spawn" else e) + "\n" for e in evs))
open(f"{d}/fleet.toml", "w").write(toml + '\n[[notify]]\ntype = "webpush"\nsub = "mailto:demo@example.com"\n')
import os
os.makedirs(f"{d}/webpush", exist_ok=True)
sub = {"endpoint": "https://web.push.apple.com/demo", "p256dh": "p", "auth": "a", "label": "iPhone Safari", "created": ts - 28800}
json.dump([sub], open(f"{d}/webpush/subs.json", "w"))
EOF
  export HOME="$d" DROVER_HOME="$d" FLEET_CONFIG="$d/fleet.toml" FLEET_STATE="$d/events.jsonl"
  # #461: install logs `timer-install` at the wall clock; dropped, so export's now stays the fixture's last event + 60 s.
  if [ "$fx" = healthy ]; then
    "$bin" timer install --no-load >/dev/null
    # #344: the install imported the log (and subs.json) into state.db; back to the files without its row.
    "$bin" events --jsonl | grep -v '"type":"timer-install"' >"$d/ev"
    rm -f "${d:?}/state.db" "${d:?}/state.db-wal" "${d:?}/state.db-shm" "${d:?}/events.jsonl.migrated"
    mv "$d/ev" "$FLEET_STATE" && mv "$d/webpush/subs.json.migrated" "$d/webpush/subs.json"
  fi
  "$bin" serve --export "$site/$fx" --demo
  rm -rf "${d:?}"
done
# #367 gate 2: <site>/queue, the Queue tab's fixture: 3 repos (acme-core: 2 running, 1 needing you, 40 queued with a 6-step
# chain at #3; acme-api 3 queued and a 100-issue backlog; docs 1), every row titled; shed lists the 2 running lanes alone.
d=$(mktemp -d)
cp -R demo/. "$d"
python3 - "$d" <<'EOF'
import json, sys
d = sys.argv[1]
ts = max(json.loads(l)["ts"] for l in open(f"{d}/events.jsonl") if l.strip())
fleets = {"acme-core": list(range(300, 340)), "acme-api": [51, 52, 53], "docs": [7]}
toml = open(f"{d}/fleet.toml").read().split("[fleet.")[0]
for f in fleets:
    toml += f'[fleet.{f}]\nrepo = "acme-example/{f}"\nlocal = "/srv/acme/{f}"\nintegration = "staging"\n\n'
open(f"{d}/fleet.toml", "w").write(toml)
ev = lambda t, f, kind, issue=None, **k: {"ts": ts - t, "fleet": f, **({"issue": issue} if issue else {}), "type": kind, **k}
evs = [ev(9000, f, "queue", issues=ns, op="add") for f, ns in fleets.items()]
# the chain: 302 → 303 → 304 → 305 → 306 → 307, its root blocked by the running #400
evs += [ev(8900, "acme-core", "queue", issues=[n], on=n - 1, op="block") for n in range(303, 308)]
evs.append(ev(8900, "acme-core", "queue", issues=[302], on=400, op="block"))
evs += [ev(6420, "acme-core", "spawn", 400, role="impl", agent="acme-core-impl-400", harness="claude"),
        ev(1500, "acme-core", "spawn", 401, role="rev", agent="acme-core-rev-401", harness="claude"),
        ev(3600, "acme-core", "needs-human", 402, code="ROUND_LIMIT", reason="review rounds ran out (4/3)")]
open(f"{d}/events.jsonl", "w").write("".join(json.dumps(e) + "\n" for e in evs))
r = json.load(open(f"{d}/runner.json"))
r["shed ls"] = json.dumps([{"name": a, "labels": {"fleet": "acme-core", "harness": "claude"}, "cwd": f"/srv/acme/worktrees/{a}", "cmd": ["sh"],
    "keep": True, "state": "running", "exit": None, "pid": 7, "reported": {"state": "working"}} for a in ["acme-core-impl-400", "acme-core-rev-401"]])
json.dump(r, open(f"{d}/runner.json", "w"))
for f, ns in fleets.items():
    for n in ns + ([400, 401, 402] if f == "acme-core" else []):
        t = "decoupling: split the serve module" if 302 <= n <= 307 else f"{f} issue {n}: one line of title text"
        json.dump({"title": t, "body": "", "updatedAt": "2026-01-01T00:00:00Z"}, open(f"{d}/cache/issue-{f}-{n}.json", "w"))
# #365: acme-api's backlog, 100 open issues from 0 to 198 days old, some labelled, with AC or in a closed PR.
labels = [["bug"], ["ui", "phone"], [], ["docs"], ["infra", "ci"]]
iso = lambda t: __import__("datetime").datetime.fromtimestamp(t, __import__("datetime").timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
iss = {str(n): {"title": f"acme-api backlog {n}: " + ("a title that runs well past one phone line, cut by CSS" if n % 7 == 0 else "one line"),
    "labels": labels[n % 5], "created": iso(ts - (599 - n) * 2 * 86400), "updated": iso(ts - (599 - n) * 2 * 86400), "ac": n % 3 == 0, "pr": n % 10 == 0}
    for n in range(500, 600)}
json.dump({"ts": ts, "since": "2026-01-01T00:00:00Z", "issues": iss}, open(f"{d}/cache/backlog-acme-api-0.json", "w"))
EOF
HOME="$d" DROVER_HOME="$d" FLEET_CONFIG="$d/fleet.toml" FLEET_STATE="$d/events.jsonl" "$bin" serve --export "$site/queue" --demo
rm -rf "${d:?}"
