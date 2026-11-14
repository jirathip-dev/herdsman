# #375: the board's TS types, `assets/types/api.d.ts`, from the Rust payload types (crates/server/src/api.rs and the
# core rows); CI fails when the committed file differs.
types:
    cargo test -p drover-server --features ts export_bindings

# #375: no board TS under assets/ (but assets/types/) declares its own copy of a generated type.
types-lint:
    #!/usr/bin/env bash
    set -euo pipefail
    names=$(grep -oE '^export type [A-Za-z]+' assets/types/api.d.ts | cut -d' ' -f3 | paste -sd'|' -)
    bad=$(grep -rnE --include='*.ts' "\b(type|interface|class) ($names)\b" assets | grep -v '^assets/types/' || true)
    test -z "$bad" || { echo "hand-written copy of a generated type (import it from assets/types/api): $bad"; exit 1; }

# #269: crates/server builds no HTML but the page shell (`page`, its `pairb`, and the export's copy of it): every other
# Rust string with a tag that has attributes or a closing tag fails; the board (web/) draws the rest from API v1.
# #725: but the lifeboat (crates/server/src/lifeboat.rs), plain server-rendered HTML by design.
html-lint:
    #!/usr/bin/env bash
    set -euo pipefail
    tags='p|div|span|b|i|a|button|form|input|details|summary|section|pre|code|h[1-6]|svg|use|small|figure|figcaption|img|ul|li|textarea|select|option|label|br|script|style|link|meta|html|head|body|header|main|nav|template'
    bad=$(find crates/server/src -name '*.rs' ! -name lifeboat.rs | sort | xargs awk -v t="$tags" '
      /^[[:space:]]*(pub(\([a-z]+\))? )?fn [a-z_0-9]+/ { match($0, /fn [a-z_0-9]+/); f = substr($0, RSTART + 3, RLENGTH - 3) }
      $0 ~ ("\"[^\"]*(</(" t ")>|<(" t ") [a-z-]+=)") && f !~ /^(page|pairb|export)$/ { print FILENAME ":" FNR ": " $0 }')
    test -z "$bad" || { echo "HTML built outside the page shell (draw it in web/ from API v1): $bad"; exit 1; }

# #680: the deterministic simulation's long batch: `just sim [n]` runs n (default 500) random seeds from DROVER_SIM_FROM
# (default 1000), each failing seed printed with its trace; replay one with `DROVER_SIM_SEED=<n> cargo test -p
# drover --test sim`. CI runs the fixed seed set (crates/cli/tests/sim/seeds.rs) in `cargo test`;
# .github/workflows/sim.yml runs `just sim 2000` and `just sim --hunt 10` nightly.
# #688: `just sim --hunt [minutes]` (default 10) is the coverage-guided search: its corpus and each minimised failure
# (a seeds.rs line, `DROVER_SIM_PLAN=…` replays it) go to target/sim-corpus.
# #685: `just sim --metrics [N] [--json]` prints the fleet metrics over N seeds (default 50) as a table, or JSON;
# DROVER_SIM_BASELINE=<metrics.json> adds a baseline column and a warning per metric over 20% worse.
# #686: `just sim --replay <state.db|scenario.json> [--days 7] [--fleet herdsman] [--save scenario.json]` replays a real
# window of a fleet's log (read-only) in the sim: the invariants' result and the real-vs-sim metrics table.
sim *args:
    #!/usr/bin/env bash
    set -euo pipefail
    set -- {{args}}
    abs() { echo "$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"; }
    if [ "${1:-}" = --replay ]; then
        export DROVER_SIM_REPLAY; DROVER_SIM_REPLAY=$(abs "$2"); shift 2
        while [ $# -gt 1 ]; do
            case $1 in
                --days) export DROVER_SIM_DAYS="$2" ;;
                --fleet) export DROVER_SIM_FLEET="$2" ;;
                --save) export DROVER_SIM_SAVE; DROVER_SIM_SAVE=$(abs "$2") ;;
                *) echo "just sim --replay: unknown $1" >&2; exit 2 ;;
            esac
            shift 2
        done
        test $# -eq 0 || { echo "just sim --replay: $1 needs a value" >&2; exit 2; }
        exec cargo test --release --locked -p drover --test sim replay_db -- --ignored --nocapture
    fi
    if [ "${1:-}" = --hunt ]; then
        DROVER_SIM_HUNT_MINUTES="${2:-10}" DROVER_SIM_CORPUS="${DROVER_SIM_CORPUS:-{{justfile_directory()}}/target/sim-corpus}" \
            exec cargo test --release --locked -p drover --test sim hunt -- --ignored --exact --nocapture
    fi
    if [ "${1:-}" != --metrics ]; then
        DROVER_SIM_N="${1:-500}" exec cargo test --release --locked -p drover --test sim random_seeds -- --ignored
    fi
    n=50 ext=md
    for a in "${@:2}"; do if [ "$a" = --json ]; then ext=json; else n="$a"; fi; done
    out="${DROVER_SIM_OUT:-${CARGO_TARGET_DIR:-target}/sim-metrics}"
    mkdir -p "$out" && out=$(cd "$out" && pwd)
    DROVER_SIM_N="$n" DROVER_SIM_OUT="$out" cargo test --release --locked -p drover --test sim metrics_batch -- --ignored >&2
    cat "$out/metrics.$ext"
