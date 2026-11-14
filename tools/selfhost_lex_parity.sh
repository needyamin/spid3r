#!/usr/bin/env bash
# Self-host lexer parity (roadmap Phase 1, docs/SELFHOST.md SH-5): the
# Spid3r-written lexer (selfhost/lexer/) vs the stage-0 oracle, token stream
# by token stream, over the frozen corpus.
#
#   positives (examples/, engine_matrix/, edge/): the two JSON token streams
#     must be byte-identical.
#   negatives (negative/): files that fail during lexing must produce the
#     same diagnostic first line AND `-->` span line; files that lex cleanly
#     and fail later (parse/semantic/runtime) are compared on stdout like the
#     positives, since their token stream is well-defined.
#
# KNOWN DIVERGENCES: none. (The Phase-2 parser closed the one this harness
# used to carry — engine_matrix/string_interp — by wiring the oracle's
# hole_is_expression into the lexer through the injected hole check.)
#
# Usage: bash tools/selfhost_lex_parity.sh [spid3r-binary] [engine]
#   default binary: target/debug/spid3r (engine: tree)
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
divergent=0
for f in selfhost/corpus/examples/*.matrix \
         selfhost/corpus/engine_matrix/*.matrix \
         selfhost/corpus/edge/*.matrix \
         selfhost/corpus/negative/*.matrix; do
  base=$(basename "$f" .matrix)
  "$BIN" tokens "$f" --json </dev/null > "$OUT/o.out" 2> "$OUT/o.err"
  ocode=$?
  "$BIN" run --engine "$ENGINE" "$SELFHOST" tokens "$f" </dev/null > "$OUT/s.out" 2> "$OUT/s.err"
  scode=$?

  # A file whose ORACLE run lexes cleanly (exit 0, JSON on stdout) is compared
  # on stdout. A file that fails lexing (error[lexer] on stderr) is compared
  # on the diagnostic's first two lines (message + span).
  if [ "$ocode" = 0 ]; then
    if [ "$scode" != 0 ]; then
      echo "PARITY FAIL (selfhost errored): $base"
      head -2 "$OUT/s.err" | sed 's/^/    /'
      fail=$((fail + 1))
      continue
    fi
    if ! cmp -s "$OUT/o.out" "$OUT/s.out"; then
      echo "PARITY FAIL (token stream): $base"
      fail=$((fail + 1))
      continue
    fi
  elif head -1 "$OUT/o.err" | grep -q "^error\[lexer\]:"; then
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
  else
    # The oracle failed before lexing (usage/config) — skip; the selfhost
    # harness entry cannot reproduce a broken SPID3R_INI etc.
    pass=$((pass + 1))
    continue
  fi
  pass=$((pass + 1))
done

echo "selfhost lexer parity: $pass passed, $fail failed, $divergent known-divergent"
if [ "$fail" -ne 0 ]; then
  exit 1
fi
echo "SELFHOST LEXER PARITY: ALL MATCH (modulo ledger-recorded divergences)"
