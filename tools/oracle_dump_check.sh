#!/usr/bin/env bash
# Oracle dump determinism over the frozen corpus (docs/SELFHOST.md SH-5,
# roadmap 0.4/0.5). Runs `spid3r parse --dump-ast-json` and `spid3r tokens
# --json` twice over every positive corpus program and demands byte-identical
# output across the two passes. Used by the CI selfhost-parity job and by hand.
#
# Usage: bash tools/oracle_dump_check.sh [path-to-spid3r-binary]
# Exit 0 = deterministic; exit 1 = a dump differed or failed.
set -u
cd "$(dirname "$0")/.."
BIN="${1:-target/release/spid3r}"
[ -x "$BIN" ] || BIN="${BIN}.exe"   # Windows: cargo writes spid3r.exe
OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT
mkdir -p "$OUT/1" "$OUT/2"

fail=0
n=0
for f in selfhost/corpus/examples/*.matrix \
         selfhost/corpus/engine_matrix/*.matrix \
         selfhost/corpus/edge/*.matrix; do
  key=$(echo "$f" | sed 's|selfhost/corpus/||; s|/|__|g; s|\.matrix$||')
  "$BIN" parse "$f" --dump-ast-json </dev/null > "$OUT/1/$key.ast" 2>/dev/null \
    || { echo "PARSE DUMP FAILED: $f"; fail=1; continue; }
  "$BIN" tokens "$f" --json </dev/null > "$OUT/1/$key.tok" 2>/dev/null \
    || { echo "TOKENS DUMP FAILED: $f"; fail=1; continue; }
  "$BIN" parse "$f" --dump-ast-json </dev/null > "$OUT/2/$key.ast" 2>/dev/null
  "$BIN" tokens "$f" --json </dev/null > "$OUT/2/$key.tok" 2>/dev/null
  for ext in ast tok; do
    n=$((n + 1))
    if ! cmp -s "$OUT/1/$key.$ext" "$OUT/2/$key.$ext"; then
      echo "DUMP DIFF ($ext): $f"
      fail=1
    fi
  done
done
if [ "$fail" -ne 0 ]; then
  echo "oracle dump determinism FAILED over $n dumps"
  exit 1
fi
echo "oracle dumps deterministic: $n dumps, two passes, byte-identical"
