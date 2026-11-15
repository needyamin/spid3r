#!/usr/bin/env bash
# Self-host native parity (roadmap Phase 5, step 5.2, SH-5): the Spid3r-
# written LLVM-C codegen (selfhost/codegen.matrix, driven by
# `selfhost/main.matrix native`) vs the stage-0 oracle (`spid3r build`), over
# the frozen corpus (SH-4). Both pipelines compile the same source to a
# native executable; the gate is IDENTICAL EXECUTION: stdout, stderr and the
# exit code must match byte-for-byte. (The object bytes are not compared: the
# oracle's module name and symbol ordering make COFF bytes unstable, and the
# contract is program behavior — SH-5's parity discipline applied to the AOT
# surface.)
#
# Pipeline under test (all Spid3r): parse -> check -> lower -> LLVM-C codegen
# -> LLVMTargetMachineEmitToFile -> link.matrix (the Phase-6.1 MSVC argument
# set) -> run.
#
# Expected non-participants:
#   * oracle-rejected files (negative corpus: diagnostics parity is the
#     parse/semantic harnesses' surface);
#   * library modules (no `main` — the oracle refuses to build an
#     executable);
#   * import-dependent files: the native pipeline refuses `import` with a
#     pointer at Phase 6.2 (module resolution is the build driver's step).
#
# Usage: bash tools/selfhost_native_parity.sh [selfhost-compiler]
# The selfhost compiler must have been built first:
#   spid3r build selfhost/main.matrix -o selfhost/main_native.exe
set -u
cd "$(dirname "$0")/.."
SH="${1:-selfhost/main_native}"
[ -x "$SH" ] || SH="${SH}.exe"
ORACLE="${ORACLE:-target/debug/spid3r}"
[ -x "$ORACLE" ] || ORACLE="${ORACLE}.exe"

OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT
mkdir -p "$OUT/o" "$OUT/s"

# Phase 6.1 gate: every built artifact carries the Windows resource metadata
# (manifest + icon + VERSIONINFO) and it is identical to the oracle's. Both
# executables are named p.exe (in separate dirs) because the metadata embeds
# the output's file stem. Skipped (and reported) off Windows / without
# powershell, where the native pipeline itself does not run.
meta_ok=0
meta_fail=0
meta_probe() {
  # $1 = label
  if ! command -v powershell > /dev/null 2>&1; then
    return 0
  fi
  local verdict
  verdict=$(powershell -NoProfile -ExecutionPolicy Bypass -File tools/pe_resource_probe.ps1 \
    "$(cygpath -w "$OUT/o/p.exe")" "$(cygpath -w "$OUT/s/p.exe")" 2>&1)
  if [ "$?" = 0 ]; then
    meta_ok=$((meta_ok + 1))
  else
    echo "FAIL(metadata) $1: $verdict"
    meta_fail=$((meta_fail + 1))
    fail=$((fail + 1))
  fi
}

pass=0
fail=0
skip=0
defer=0
for f in selfhost/corpus/examples/*.matrix \
         selfhost/corpus/engine_matrix/*.matrix \
         selfhost/corpus/edge/*.matrix; do
  base=$(basename "$f" .matrix)
  "$ORACLE" build "$f" -o "$OUT/o/p.exe" > /dev/null 2>&1
  ocode=$?
  if [ "$ocode" != 0 ]; then
    # The oracle refuses this file (negative corpus or a library module
    # without main): not an executable-parity target.
    skip=$((skip + 1))
    continue
  fi
  SPID3R_RUNTIME_LIB="$(pwd)/target/debug/spid3r_lang.lib" \
    "$SH" native "$f" "$OUT/s/p.exe" > /dev/null 2> "$OUT/s.err"
  scode=$?
  if [ "$scode" != 0 ]; then
    if grep -q "module resolution is the build driver's Phase 6.2" "$OUT/s.err"; then
      # The native pipeline refuses imports BY DESIGN until Phase 6.2 (module
      # resolution). Counted separately: a visible defer, not a hidden pass.
      defer=$((defer + 1))
      continue
    fi
    echo "FAIL(build) $base"
    cat "$OUT/s.err"
    fail=$((fail + 1))
    continue
  fi
  "$OUT/o/p.exe" > "$OUT/o.stdout" 2> "$OUT/o.stderr"
  ocode=$?
  "$OUT/s/p.exe" > "$OUT/s.stdout" 2> "$OUT/s.stderr"
  scode=$?
  if [ "$ocode" = "$scode" ] \
     && cmp -s "$OUT/o.stdout" "$OUT/s.stdout" \
     && cmp -s "$OUT/o.stderr" "$OUT/s.stderr"; then
    pass=$((pass + 1))
    meta_probe "$base"
  else
    echo "FAIL(exec) $base (oracle rc=$ocode, selfhost rc=$scode)"
    diff "$OUT/o.stdout" "$OUT/s.stdout" | head -10
    fail=$((fail + 1))
  fi
done

# Phase 5.3: the negative corpus. Compile-time negatives (lexer/parser/
# semantic) and the link-time negative must produce the same build verdict:
# identical stderr and exit code. The two negative-RUNTIME programs build
# everywhere and fail at run: compare the full stderr and exit code (the
# oracle and the selfhost link the SAME runtime staticlib, so the
# error[runtime] first lines and the rc are pinned end to end).
for f in selfhost/corpus/negative/*.matrix; do
  base=$(basename "$f" .matrix)
  rm -f "$OUT/o/p.exe" "$OUT/s/p.exe"
  "$ORACLE" build "$f" -o "$OUT/o/p.exe" > "$OUT/ob.txt" 2>&1
  ocode=$?
  SPID3R_RUNTIME_LIB="$(pwd)/target/debug/spid3r_lang.lib"     "$SH" native "$f" "$OUT/s/p.exe" > "$OUT/sb.txt" 2>&1
  scode=$?
  if [ "$ocode" != 0 ] || [ "$scode" != 0 ]; then
    # A build failure on either side: first diagnostic line + exit code —
    # the caret/gutter decoration the oracle renders is a documented gap.
    if [ "$ocode" = "$scode" ]        && [ "$(head -1 "$OUT/ob.txt")" = "$(head -1 "$OUT/sb.txt")" ]; then
      pass=$((pass + 1))
    else
      echo "FAIL(build-diag) $base (oracle rc=$ocode, selfhost rc=$scode)"
      diff <(head -1 "$OUT/ob.txt") <(head -1 "$OUT/sb.txt") | head -4
      fail=$((fail + 1))
    fi
    continue
  fi
  "$OUT/o/p.exe" > "$OUT/o.stdout" 2> "$OUT/o.stderr"
  ocode=$?
  "$OUT/s/p.exe" > "$OUT/s.stdout" 2> "$OUT/s.stderr"
  scode=$?
  if [ "$ocode" = "$scode" ]      && cmp -s "$OUT/o.stdout" "$OUT/s.stdout"      && cmp -s "$OUT/o.stderr" "$OUT/s.stderr"; then
    pass=$((pass + 1))
    meta_probe "$base"
  else
    echo "FAIL(exec-diag) $base (oracle rc=$ocode, selfhost rc=$scode)"
    diff "$OUT/o.stderr" "$OUT/s.stderr" | head -6
    fail=$((fail + 1))
  fi
done

echo "native parity: pass=$pass fail=$fail skip=$skip defer=$defer (Phase-6.2 module resolution)"
echo "artifact metadata (6.1: manifest+icon+VERSIONINFO identical to oracle): ok=$meta_ok fail=$meta_fail"
if [ "$fail" != 0 ]; then
  exit 1
fi
exit 0
