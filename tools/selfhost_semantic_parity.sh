#!/usr/bin/env bash
# Self-host semantic parity (roadmap Phase 3, docs/SELFHOST.md SH-5): the
# Spid3r-written checker (selfhost/semantic.matrix) vs the stage-0 oracle.
#
#   positives (examples/, engine_matrix/, edge/): the declaration registry
#     JSON must be byte-identical (oracle `check --dump-registry-json`).
#   negatives (negative/): files that fail the semantic check must produce
#     the same first line + `-->` span line. Files that PASS the check
#     (runtime negatives) are compared in check-clean mode: both sides must
#     succeed.
#
# Usage: bash tools/selfhost_semantic_parity.sh [spid3r-binary] [engine]
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
         selfhost/corpus/edge/*.matrix \
         selfhost/corpus/negative/*.matrix; do
  base=$(basename "$f" .matrix)
  "$BIN" check "$f" --dump-registry-json </dev/null > "$OUT/o.out" 2> "$OUT/o.err"
  ocode=$?
  "$BIN" run --engine "$ENGINE" "$SELFHOST" registry "$f" </dev/null > "$OUT/s.out" 2> "$OUT/s.err"
  scode=$?

  if [ "$ocode" = 0 ]; then
    if [ "$scode" != 0 ]; then
      echo "PARITY FAIL (selfhost errored): $base"
      head -2 "$OUT/s.err" | sed 's/^/    /'
      fail=$((fail + 1))
      continue
    fi
    if ! cmp -s "$OUT/o.out" "$OUT/s.out"; then
      echo "PARITY FAIL (registry): $base"
      fail=$((fail + 1))
      continue
    fi
  elif grep -q "^error\[semantic\]:" "$OUT/o.err"; then
    # A semantic diagnostic: first line + span line must match exactly.
    "$BIN" run --engine "$ENGINE" "$SELFHOST" check "$f" </dev/null > /dev/null 2> "$OUT/s.err"
    scode=$?
    o1=$(head -1 "$OUT/o.err")
    o2=$(sed -n '2p' "$OUT/o.err")
    s1=$(head -1 "$OUT/s.err")
    s2=$(sed -n '2p' "$OUT/s.err")
    if [ "$scode" = 0 ] || [ "$o1" != "$s1" ] || [ "$o2" != "$s2" ]; then
      echo "PARITY FAIL (diagnostic): $base"
      echo "    oracle:   $o1 | $o2"
      echo "    selfhost: $s1 | $s2"
      fail=$((fail + 1))
      continue
    fi
  else
    # The oracle failed before the semantic pass (lex/parse) — those surfaces
    # have their own harnesses; skip here.
    pass=$((pass + 1))
    continue
  fi
  pass=$((pass + 1))
done

echo "selfhost semantic parity: $pass passed, $fail failed"
if [ "$fail" -ne 0 ]; then
  exit 1
fi
echo "SELFHOST SEMANTIC PARITY: ALL MATCH"
