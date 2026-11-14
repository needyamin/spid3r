#!/usr/bin/env bash
# Self-host compile parity (roadmap Phase 4, steps 4.2 + 4.3, SH-5): the
# Spid3r-written bytecode compiler + container writer vs the stage-0 oracle
# (`spid3r compile --no-opt`), artifact byte-for-byte over the frozen corpus.
#
#   * byte-equality applies where the oracle's artifact is deterministic;
#   * KNOWN ORACLE INSTABILITY (ledger 2026-11-14): a module with >= 2 public
#     names embeds its public set from a Rust HashSet whose iteration order
#     is randomized per process — two oracle runs on the same source produce
#     different bytes (found in Phase 4; exec behavior identical). Such files
#     are gated on exec-equivalence instead: the port's artifact must load
#     and run identically via `spid3r exec`.
#
# Usage: bash tools/selfhost_compile_parity.sh [spid3r-binary] [engine]
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
  "$BIN" compile "$f" -o "$OUT/o.matrixc" --no-opt </dev/null > /dev/null 2> "$OUT/o.err"
  ocode=$?
  "$BIN" run --engine "$ENGINE" "$SELFHOST" compile "$f" "$OUT/s.matrixc" </dev/null > /dev/null 2> "$OUT/s.err"
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
  if cmp -s "$OUT/o.matrixc" "$OUT/s.matrixc"; then
    pass=$((pass + 1))
    continue
  fi
  # Byte-difference: sample the oracle 5 times. A match with ANY sample
  # passes (the port is one valid serialization of the same program; the
  # oracle randomizes its embedded module public-set order per process).
  # Otherwise exec-equivalence gates the artifact.
  matched_any=0
  oracle_differs=0
  for try in 1 2 3 4 5; do
    "$BIN" compile "$f" -o "$OUT/ot$try.matrixc" --no-opt </dev/null > /dev/null 2>&1
    if cmp -s "$OUT/ot$try.matrixc" "$OUT/s.matrixc"; then
      matched_any=1
    fi
    if [ $try -gt 1 ] && ! cmp -s "$OUT/ot$try.matrixc" "$OUT/ot1.matrixc"; then
      oracle_differs=1
    fi
  done
  if [ "$matched_any" = 1 ]; then
    echo "ORACLE-UNSTABLE (port matches an oracle ordering): $base"
    pass=$((pass + 1))
    continue
  fi
  if [ "$oracle_differs" = 1 ]; then
    "$BIN" exec "$OUT/o.matrixc" </dev/null > "$OUT/o.run" 2>&1
    oec=$?
    "$BIN" exec "$OUT/s.matrixc" </dev/null > "$OUT/s.run" 2>&1
    sec=$?
    if [ "$oec" = "$sec" ] && cmp -s "$OUT/o.run" "$OUT/s.run"; then
      echo "ORACLE-UNSTABLE (exec-equivalent): $base"
      pass=$((pass + 1))
      continue
    fi
    echo "PARITY FAIL (unstable + exec differs): $base"
    fail=$((fail + 1))
    continue
  fi
  echo "PARITY FAIL (bytes): $base"
  fail=$((fail + 1))
done

echo "selfhost compile parity: $pass passed, $fail failed"
if [ "$fail" -ne 0 ]; then
  exit 1
fi
echo "SELFHOST COMPILE PARITY: ALL MATCH"
