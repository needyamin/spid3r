#!/usr/bin/env bash
# run_r3.sh - run every round-3 probe on tree and vm, capture outcomes.
# Usage: bash tools/run_r3.sh [filter]
set -uo pipefail
ROOT="/c/Users/needyamin/OneDrive/Desktop/GaliLang"
BIN="$ROOT/target/release/spid3r.exe"
PROBES="$ROOT/audit/probes_r3"
OUT="$ROOT/audit/probes_r3/results"
mkdir -p "$OUT"
export SPID3R_PATH="$ROOT/stdlib;$PROBES"

FILTER="${1:-}"
for f in "$PROBES"/*.matrix; do
    name="$(basename "$f" .matrix)"
    case "$name" in r3_circ_*) continue ;; esac
    if [ -n "$FILTER" ] && [[ "$name" != *"$FILTER"* ]]; then continue; fi
    for eng in tree vm; do
        out="$OUT/${name}.${eng}.out"
        err="$OUT/${name}.${eng}.err"
        timeout 60 "$BIN" run --engine "$eng" "$f" >"$out" 2>"$err"
        code=$?
        echo "$name [$eng] exit=$code out=$(wc -c <"$out")B err=$(wc -c <"$err")B"
    done
    # compare engines
    if ! diff -q "$OUT/${name}.tree.out" "$OUT/${name}.vm.out" >/dev/null 2>&1; then
        echo "  ^^ DIVERGENCE tree vs vm for $name"
    fi
done
echo "--- done ---"