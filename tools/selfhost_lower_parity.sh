#!/usr/bin/env bash
# Self-host lowering parity (roadmap Phase 4, step 4.1, SH-5): the
# Spid3r-written lowering pass (selfhost/lower.matrix) vs the stage-0 oracle
# (`spid3r parse --dump-lowered-json`), lowered-form dump by dump, over the
# frozen corpus.
#
# Both sides parse + lower WITHOUT the semantic check (the oracle surface's
# pipeline); the corpus positives all parse, so the comparison is exact.
#
# Usage: bash tools/selfhost_lower_parity.sh [spid3r-binary] [engine]
set -u
cd "$(dirname "$0")/.."
BIN="${1:-target/debug/spid3r}"
ENGINE="${2:-tree}"
[ -x "$BIN" ] || BIN="${BIN}.exe"

SELFHOST="selfhost/main.matrix"
OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

pass=0
fail=0
for f in selfhost/corpus/examples/*.matrix \
         selfhost/corpus/engine_matrix/*.matrix \
         selfhost/corpus/edge/*.matrix; do
  base=$(basename "$f" .matrix)
  "$BIN" parse "$f" --dump-lowered-json </dev/null > "$OUT/o.out" 2> "$OUT/o.err"
  ocode=$?
  "$BIN" run --engine "$ENGINE" "$SELFHOST" lowered "$f" </dev/null > "$OUT/s.out" 2> "$OUT/s.err"
  scode=$?
  if [ "$ocode" != 0 ] || [ ! -s "$OUT/o.out" ]; then
    # The oracle rejects this file (a diagnostic-parity case for the
    # parse/semantic harnesses, not a byte-parity target here).
    pass=$((pass + 1))
    continue
  fi
  if [ "$scode" != 0 ]; then
    echo "PARITY FAIL (selfhost errored): $base"
    head -2 "$OUT/s.err" | sed 's/^/    /'
    fail=$((fail + 1))
    continue
  fi
  if ! cmp -s "$OUT/o.out" "$OUT/s.out"; then
    echo "PARITY FAIL (lowered form): $base"
    fail=$((fail + 1))
    continue
  fi
  pass=$((pass + 1))
done

echo "selfhost lowering parity: $pass passed, $fail failed"
if [ "$fail" -ne 0 ]; then
  exit 1
fi
echo "SELFHOST LOWERING PARITY: ALL MATCH"
