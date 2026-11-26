#!/usr/bin/env bash
# Self-host bootstrap three-way parity (roadmap Phase 7, step 7.2, SH-1):
# compares executables produced by Stage 0 (Rust `spid3r build`),
# Stage 1 (`selfhost/main_native.exe build`), and Stage 2
# (`spid3r-self-2.exe build`) over the frozen corpus (SH-4).
#
# Usage: bash tools/selfhost/selfhost_bootstrap_parity.sh [stage1] [stage2] [stage0]
#   defaults:
#     stage1 = selfhost/main_native(.exe)
#     stage2 = built into temp dir by stage1 compiling selfhost/main.matrix
#     stage0 = target/debug/spid3r(.exe)
set -u
cd "$(dirname "$0")/../.."

S1="${1:-selfhost/main_native}"
[ -x "$S1" ] || S1="${S1}.exe"
S2="${2:-}"
S0="${3:-target/debug/spid3r}"
[ -x "$S0" ] || S0="${S0}.exe"

export SPID3R_RUNTIME_LIB="${SPID3R_RUNTIME_LIB:-$(pwd)/target/debug/spid3r_lang.lib}"

OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT
mkdir -p "$OUT/o" "$OUT/s1" "$OUT/s2"

if [ -z "$S2" ]; then
  S2="$OUT/spid3r-self-2.exe"
  echo "building Stage 2 via Stage 1 -> $S2"
  "$S1" build selfhost/main.matrix -o "$S2" > "$OUT/s2_build.out" 2> "$OUT/s2_build.err"
  if [ "$?" != 0 ]; then
    echo "FAIL(stage2-bootstrap): Stage 1 failed to build Stage 2"
    cat "$OUT/s2_build.out" "$OUT/s2_build.err"
    exit 1
  fi
else
  [ -x "$S2" ] || [ -f "$S2" ] || S2="${S2}.exe"
fi

meta_ok=0
meta_fail=0
meta_probe() {
  local label="$1"
  local tag="$2"
  local candidate="$3"
  if ! command -v powershell > /dev/null 2>&1; then
    return 0
  fi
  local verdict
  verdict=$(powershell -NoProfile -ExecutionPolicy Bypass -File tools/release/pe_resource_probe.ps1 \
    "$(cygpath -w "$OUT/o/p.exe")" "$(cygpath -w "$candidate")" 2>&1)
  if [ "$?" = 0 ]; then
    meta_ok=$((meta_ok + 1))
  else
    echo "FAIL(metadata-$tag) $label: $verdict"
    meta_fail=$((meta_fail + 1))
    fail=$((fail + 1))
  fi
}

pass=0
fail=0
skip=0
obj_match=0
obj_mismatch=0

for f in selfhost/corpus/examples/*.matrix \
         selfhost/corpus/engine_matrix/*.matrix \
         selfhost/corpus/edge/*.matrix; do
  base=$(basename "$f" .matrix)
  if [ -n "${CORPUS_FILTER:-}" ] && [ "$base" != "$CORPUS_FILTER" ]; then
    continue
  fi
  rm -f "$OUT/o/p.exe" "$OUT/s1/p.exe" "$OUT/s2/p.exe" \
        "$OUT/s1/spid3r_${base}.o" "$OUT/s2/spid3r_${base}.o"

  "$S0" build "$f" -o "$OUT/o/p.exe" > /dev/null 2>&1
  ocode=$?
  if [ "$ocode" != 0 ]; then
    skip=$((skip + 1))
    continue
  fi

  "$S1" build "$f" -o "$OUT/s1/p.exe" > "$OUT/s1.bout" 2> "$OUT/s1.berr"
  s1code=$?
  "$S2" build "$f" -o "$OUT/s2/p.exe" > "$OUT/s2.bout" 2> "$OUT/s2.berr"
  s2code=$?

  if [ "$s1code" != 0 ] || [ "$s2code" != 0 ]; then
    echo "FAIL(build) $base (s1 rc=$s1code, s2 rc=$s2code)"
    cat "$OUT/s1.berr" "$OUT/s2.berr"
    fail=$((fail + 1))
    continue
  fi

  if [ -f "$OUT/s1/spid3r_${base}.o" ] && [ -f "$OUT/s2/spid3r_${base}.o" ]; then
    if cmp -s "$OUT/s1/spid3r_${base}.o" "$OUT/s2/spid3r_${base}.o"; then
      obj_match=$((obj_match + 1))
    else
      echo "FAIL(obj-mismatch) $base: Stage 1 vs Stage 2 COFF .o differ"
      obj_mismatch=$((obj_mismatch + 1))
      fail=$((fail + 1))
      continue
    fi
  fi

  "$OUT/o/p.exe"  < /dev/null > "$OUT/o.stdout"  2> "$OUT/o.stderr"
  orc=$?
  "$OUT/s1/p.exe" < /dev/null > "$OUT/s1.stdout" 2> "$OUT/s1.stderr"
  s1rc=$?
  "$OUT/s2/p.exe" < /dev/null > "$OUT/s2.stdout" 2> "$OUT/s2.stderr"
  s2rc=$?

  if [ "$orc" = "$s1rc" ] && [ "$orc" = "$s2rc" ] \
     && cmp -s "$OUT/o.stdout" "$OUT/s1.stdout" \
     && cmp -s "$OUT/o.stdout" "$OUT/s2.stdout" \
     && cmp -s "$OUT/o.stderr" "$OUT/s1.stderr" \
     && cmp -s "$OUT/o.stderr" "$OUT/s2.stderr"; then
    pass=$((pass + 1))
    meta_probe "$base" "s1" "$OUT/s1/p.exe"
    meta_probe "$base" "s2" "$OUT/s2/p.exe"
  else
    echo "FAIL(exec) $base (s0 rc=$orc, s1 rc=$s1rc, s2 rc=$s2rc)"
    diff -u "$OUT/o.stdout" "$OUT/s1.stdout" | head -8
    diff -u "$OUT/o.stdout" "$OUT/s2.stdout" | head -8
    diff -u "$OUT/o.stderr" "$OUT/s2.stderr" | head -8
    fail=$((fail + 1))
  fi
done

for f in selfhost/corpus/negative/*.matrix; do
  base=$(basename "$f" .matrix)
  if [ -n "${CORPUS_FILTER:-}" ] && [ "$base" != "$CORPUS_FILTER" ]; then
    continue
  fi
  rm -f "$OUT/o/p.exe" "$OUT/s1/p.exe" "$OUT/s2/p.exe" \
        "$OUT/s1/spid3r_${base}.o" "$OUT/s2/spid3r_${base}.o"

  "$S0" build "$f" -o "$OUT/o/p.exe" > "$OUT/ob.txt" 2>&1
  ocode=$?
  "$S1" build "$f" -o "$OUT/s1/p.exe" > "$OUT/s1b.txt" 2>&1
  s1code=$?
  "$S2" build "$f" -o "$OUT/s2/p.exe" > "$OUT/s2b.txt" 2>&1
  s2code=$?

  if [ "$ocode" != 0 ] || [ "$s1code" != 0 ] || [ "$s2code" != 0 ]; then
    o1=$(head -1 "$OUT/ob.txt")
    s1_1=$(head -1 "$OUT/s1b.txt")
    s2_1=$(head -1 "$OUT/s2b.txt")
    if [ "$ocode" = "$s1code" ] && [ "$ocode" = "$s2code" ] \
       && [ "$o1" = "$s1_1" ] && [ "$o1" = "$s2_1" ]; then
      pass=$((pass + 1))
    else
      echo "FAIL(build-diag) $base (s0 rc=$ocode, s1 rc=$s1code, s2 rc=$s2code)"
      echo "  s0: $o1"
      echo "  s1: $s1_1"
      echo "  s2: $s2_1"
      fail=$((fail + 1))
    fi
    continue
  fi

  "$OUT/o/p.exe"  < /dev/null > "$OUT/o.stdout"  2> "$OUT/o.stderr"
  orc=$?
  "$OUT/s1/p.exe" < /dev/null > "$OUT/s1.stdout" 2> "$OUT/s1.stderr"
  s1rc=$?
  "$OUT/s2/p.exe" < /dev/null > "$OUT/s2.stdout" 2> "$OUT/s2.stderr"
  s2rc=$?

  if [ "$orc" = "$s1rc" ] && [ "$orc" = "$s2rc" ] \
     && cmp -s "$OUT/o.stdout" "$OUT/s1.stdout" \
     && cmp -s "$OUT/o.stdout" "$OUT/s2.stdout" \
     && cmp -s "$OUT/o.stderr" "$OUT/s1.stderr" \
     && cmp -s "$OUT/o.stderr" "$OUT/s2.stderr"; then
    pass=$((pass + 1))
    meta_probe "$base" "s1" "$OUT/s1/p.exe"
    meta_probe "$base" "s2" "$OUT/s2/p.exe"
  else
    echo "FAIL(exec-diag) $base (s0 rc=$orc, s1 rc=$s1rc, s2 rc=$s2rc)"
    diff -u "$OUT/o.stderr" "$OUT/s2.stderr" | head -6
    fail=$((fail + 1))
  fi
done

echo "bootstrap 3-way parity: pass=$pass fail=$fail skip=$skip obj_match=$obj_match obj_mismatch=$obj_mismatch"
echo "artifact metadata (Stage 1 + Stage 2 vs Stage 0): ok=$meta_ok fail=$meta_fail"
if [ "$fail" != 0 ]; then
  exit 1
fi
exit 0
