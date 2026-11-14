#!/usr/bin/env bash
# Self-host parser parity (roadmap Phase 2, docs/SELFHOST.md SH-5): the
# Spid3r-written parser (selfhost/parser.matrix) vs the stage-0 oracle
# (`spid3r parse --dump-ast-json`), AST dump by AST dump, over the frozen
# corpus.
#
#   positives: the two JSON AST dumps must be byte-identical.
#   negatives: both sides reject the file; the diagnostic's first line and
#     `-->` span line must match (the phase may be lexer for lexical errors —
#     both sides run the same ported lexer).
#
# Usage: bash tools/selfhost_parse_parity.sh [spid3r-binary] [engine]
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
  "$BIN" parse "$f" --dump-ast-json </dev/null > "$OUT/o.out" 2> "$OUT/o.err"
  ocode=$?
  "$BIN" run --engine "$ENGINE" "$SELFHOST" parse "$f" </dev/null > "$OUT/s.out" 2> "$OUT/s.err"
  scode=$?

  if [ "$ocode" = 0 ]; then
    if [ "$scode" != 0 ]; then
      echo "PARITY FAIL (selfhost errored): $base"
      head -2 "$OUT/s.err" | sed 's/^/    /'
      fail=$((fail + 1))
      continue
    fi
    if ! cmp -s "$OUT/o.out" "$OUT/s.out"; then
      echo "PARITY FAIL (AST): $base"
      fail=$((fail + 1))
      continue
    fi
  else
    # Both sides must reject with the same first line + span line.
    o1=$(head -1 "$OUT/o.err")
    o2=$(sed -n '2p' "$OUT/o.err")
    s1=$(head -1 "$OUT/s.err")
    s2=$(sed -n '2p' "$OUT/s.err")
    if [ "$o1" != "$s1" ] || [ "$o2" != "$s2" ]; then
      echo "PARITY FAIL (diagnostic): $base"
      echo "    oracle:   $o1 | $o2"
      echo "    selfhost: $s1 | $s2"
      fail=$((fail + 1))
      continue
    fi
  fi
  pass=$((pass + 1))
done

echo "selfhost parser parity: $pass passed, $fail failed"
if [ "$fail" -ne 0 ]; then
  exit 1
fi
echo "SELFHOST PARSER PARITY: ALL MATCH"
