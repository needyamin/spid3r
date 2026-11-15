#!/usr/bin/env bash
# Self-host runtime artifact formalization & checksum-pinning parity
# (roadmap Phase 9.1, docs/SELFHOST.md SH-6, docs/RUNTIME_ABI.md, SEC-B).
#
# Usage: bash tools/selfhost_runtime_parity.sh [selfhost-compiler] [staticlib]
set -u
cd "$(dirname "$0")/.."

SH="${1:-selfhost/main_native}"
[ -x "$SH" ] || SH="${SH}.exe"
ORACLE="${ORACLE:-target/debug/spid3r}"
[ -x "$ORACLE" ] || ORACLE="${ORACLE}.exe"

STATICLIB="${2:-}"
if [ -z "$STATICLIB" ]; then
  for cand in \
    target/release/spid3r_lang.lib \
    target/release/libspid3r_lang.a \
    target/debug/spid3r_lang.lib \
    target/debug/libspid3r_lang.a; do
    if [ -f "$cand" ]; then
      STATICLIB="$cand"
      break
    fi
  done
fi

if [ ! -x "$SH" ]; then
  echo "FAIL: selfhost compiler not found: $SH" >&2
  exit 1
fi
if [ -z "$STATICLIB" ] || [ ! -f "$STATICLIB" ]; then
  echo "FAIL: runtime staticlib not found" >&2
  exit 1
fi

OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

pass=0
fail=0

ok() {
  echo "  PASS: $1"
  pass=$((pass + 1))
}

bad() {
  echo "  FAIL: $1"
  fail=$((fail + 1))
}

cat > "$OUT/probe.matrix" <<'EOF'
fn main() {
    let h = sha256("spid3r");
    println("rt_ok:" + h + ":" + string(6 * 7));
    return 0;
}
EOF

"$ORACLE" run --engine tree "$OUT/probe.matrix" > "$OUT/oracle.out" 2> "$OUT/oracle.err" < /dev/null
orc=$?
if [ "$orc" != "0" ]; then
  echo "FAIL: oracle failed on probe.matrix (rc=$orc)"
  cat "$OUT/oracle.err"
  exit 1
fi

# 1. Package & verify runtime artifact + provenance
mkdir -p "$OUT/bundle"
if bash tools/runtime_artifact.sh package "$STATICLIB" "$OUT/bundle" > "$OUT/pkg.out" 2> "$OUT/pkg.err" < /dev/null; then
  ok "package runtime artifact bundle"
else
  bad "package runtime artifact bundle"
  cat "$OUT/pkg.out" "$OUT/pkg.err"
fi

MANIFEST="$OUT/bundle/runtime-manifest.json"
LIBNAME="$(basename "$STATICLIB")"
if [ -f "$MANIFEST" ] \
   && grep -q '"schema": 1' "$MANIFEST" \
   && grep -q '"git_commit":' "$MANIFEST" \
   && grep -q '"llvm_version":' "$MANIFEST" \
   && grep -q '"trampolines": 66' "$MANIFEST" \
   && grep -q '"stdlib_natives": 229' "$MANIFEST"; then
  ok "manifest contains version, provenance (commit/LLVM), and interface counts"
else
  bad "manifest missing expected fields/provenance"
  cat "$MANIFEST" 2>/dev/null || true
fi

if bash tools/runtime_artifact.sh verify "$OUT/bundle" > "$OUT/sh_verify.out" 2>&1 < /dev/null; then
  ok "runtime_artifact.sh verify passes"
else
  bad "runtime_artifact.sh verify failed"
  cat "$OUT/sh_verify.out"
fi

# 2. Selfhost verify-runtime CLI (dir & explicit manifest)
"$SH" verify-runtime "$OUT/bundle" > "$OUT/v1.out" 2> "$OUT/v1.err" < /dev/null
if [ "$?" = "0" ] && grep -q "verified runtime staticlib:" "$OUT/v1.out" && grep -q "commit=" "$OUT/v1.out"; then
  ok "selfhost verify-runtime <dir>"
else
  bad "selfhost verify-runtime <dir>"
  cat "$OUT/v1.out" "$OUT/v1.err"
fi

"$SH" verify-runtime "$MANIFEST" > "$OUT/v2.out" 2> "$OUT/v2.err" < /dev/null
if [ "$?" = "0" ] && grep -q "verified runtime staticlib:" "$OUT/v2.out"; then
  ok "selfhost verify-runtime <manifest.json>"
else
  bad "selfhost verify-runtime <manifest.json>"
  cat "$OUT/v2.out" "$OUT/v2.err"
fi

# 3. Consume pinned artifact via SPID3R_RUNTIME_MANIFEST + SPID3R_RUNTIME_REQUIRE_PIN=1
SPID3R_RUNTIME_MANIFEST="$MANIFEST" SPID3R_RUNTIME_REQUIRE_PIN=1 \
  "$SH" build "$OUT/probe.matrix" -o "$OUT/p1.exe" > "$OUT/b1.out" 2> "$OUT/b1.err" < /dev/null
b1rc=$?
if [ "$b1rc" = "0" ] && [ -f "$OUT/p1.exe" ]; then
  "$OUT/p1.exe" > "$OUT/p1.out" 2> "$OUT/p1.err" < /dev/null
  if [ "$?" = "0" ] && cmp -s "$OUT/oracle.out" "$OUT/p1.out"; then
    ok "selfhost build consumes SPID3R_RUNTIME_MANIFEST by checksum (output matches oracle)"
  else
    bad "p1.exe output differs from oracle"
  fi
else
  bad "selfhost build with SPID3R_RUNTIME_MANIFEST failed (rc=$b1rc)"
  cat "$OUT/b1.out" "$OUT/b1.err"
fi

SPID3R_RUNTIME_MANIFEST="$MANIFEST" SPID3R_RUNTIME_REQUIRE_PIN=1 \
  "$SH" native "$OUT/probe.matrix" "$OUT/p2.exe" > "$OUT/b2.out" 2> "$OUT/b2.err" < /dev/null
b2rc=$?
if [ "$b2rc" = "0" ] && [ -f "$OUT/p2.exe" ]; then
  "$OUT/p2.exe" > "$OUT/p2.out" 2> "$OUT/p2.err" < /dev/null
  if [ "$?" = "0" ] && cmp -s "$OUT/oracle.out" "$OUT/p2.out"; then
    ok "selfhost native consumes SPID3R_RUNTIME_MANIFEST by checksum"
  else
    bad "p2.exe output differs from oracle"
  fi
else
  bad "selfhost native with SPID3R_RUNTIME_MANIFEST failed (rc=$b2rc)"
  cat "$OUT/b2.out" "$OUT/b2.err"
fi

# Sidecar discovery via SPID3R_RUNTIME_LIB (no SPID3R_RUNTIME_MANIFEST)
SPID3R_RUNTIME_LIB="$OUT/bundle/$LIBNAME" SPID3R_RUNTIME_REQUIRE_PIN=1 \
  "$SH" build "$OUT/probe.matrix" -o "$OUT/p3.exe" > "$OUT/b3.out" 2> "$OUT/b3.err" < /dev/null
if [ "$?" = "0" ] && [ -f "$OUT/p3.exe" ]; then
  ok "selfhost build discovers & verifies sidecar manifest/sha256"
else
  bad "selfhost build sidecar verification failed"
  cat "$OUT/b3.out" "$OUT/b3.err"
fi

# 4. Tarball archive unpack + checksum verification + build
TARFILE="$(ls "$OUT"/bundle/spid3r-runtime-*.tar.gz | head -n 1)"
if [ -n "$TARFILE" ] && [ -f "${TARFILE}.sha256" ]; then
  (cd "$OUT/bundle" && sha256sum -c "$(basename "${TARFILE}.sha256")") > "$OUT/tar_sha.out" 2>&1
  if [ "$?" = "0" ]; then
    mkdir -p "$OUT/unpacked"
    tar -xzf "$TARFILE" -C "$OUT/unpacked"
    if [ -f "$OUT/unpacked/RUNTIME_ABI.md" ] \
       && "$SH" verify-runtime "$OUT/unpacked" > "$OUT/uv.out" 2>&1 < /dev/null \
       && SPID3R_RUNTIME_MANIFEST="$OUT/unpacked/runtime-manifest.json" SPID3R_RUNTIME_REQUIRE_PIN=1 \
          "$SH" build "$OUT/probe.matrix" -o "$OUT/p4.exe" > "$OUT/b4.out" 2>&1 < /dev/null; then
      ok "versioned runtime tarball unpack + sha256 + selfhost build roundtrip"
    else
      bad "unpacked tarball verify/build failed"
      cat "$OUT/uv.out" "$OUT/b4.out" 2>/dev/null || true
    fi
  else
    bad "tarball sha256 verification failed"
    cat "$OUT/tar_sha.out"
  fi
else
  bad "versioned tarball missing"
fi

# 5. Tamper / SEC-B negative gates (must exit 4 cleanly and never produce output .exe)
tamper_one_byte() {
  local f="$1"
  local pybin="python3"
  command -v python3 >/dev/null 2>&1 || pybin="python"
  "$pybin" - "$f" <<'PY'
import sys
p = sys.argv[1]
with open(p, "r+b") as fh:
    fh.seek(64)
    b = fh.read(1)
    fh.seek(64)
    fh.write(bytes([b[0] ^ 0xFF]))
PY
}

# 5a: Tampered staticlib with manifest
mkdir -p "$OUT/tamper1"
cp "$OUT/bundle/$LIBNAME" "$OUT/bundle/${LIBNAME}.sha256" "$OUT/bundle/runtime-manifest.json" "$OUT/bundle/${LIBNAME}.manifest.json" "$OUT/tamper1/"
tamper_one_byte "$OUT/tamper1/$LIBNAME"

SPID3R_RUNTIME_MANIFEST="$OUT/tamper1/runtime-manifest.json" \
  "$SH" build "$OUT/probe.matrix" -o "$OUT/t1.exe" > "$OUT/t1.out" 2> "$OUT/t1.err" < /dev/null
t1rc=$?
if [ "$t1rc" = "4" ] && [ ! -e "$OUT/t1.exe" ] && grep -q "error\[corrupt\]: runtime staticlib integrity check failed" "$OUT/t1.err"; then
  ok "tampered staticlib rejected cleanly with exit 4 (no output .exe)"
else
  bad "tampered staticlib not rejected cleanly (rc=$t1rc, exists=$( [ -e "$OUT/t1.exe" ] && echo yes || echo no ))"
  cat "$OUT/t1.out" "$OUT/t1.err"
fi

"$SH" verify-runtime "$OUT/tamper1" > "$OUT/t1v.out" 2> "$OUT/t1v.err" < /dev/null
if [ "$?" = "4" ]; then
  ok "selfhost verify-runtime rejects tampered staticlib with exit 4"
else
  bad "selfhost verify-runtime did not exit 4 on tampered staticlib"
fi

if ! bash tools/runtime_artifact.sh verify "$OUT/tamper1" > /dev/null 2>&1 < /dev/null; then
  ok "runtime_artifact.sh verify rejects tampered staticlib"
else
  bad "runtime_artifact.sh verify accepted tampered staticlib"
fi

# 5b: Tampered staticlib with sidecar .sha256 only
mkdir -p "$OUT/tamper2"
cp "$OUT/tamper1/$LIBNAME" "$OUT/bundle/${LIBNAME}.sha256" "$OUT/tamper2/"
SPID3R_RUNTIME_LIB="$OUT/tamper2/$LIBNAME" \
  "$SH" build "$OUT/probe.matrix" -o "$OUT/t2.exe" > "$OUT/t2.out" 2> "$OUT/t2.err" < /dev/null
t2rc=$?
if [ "$t2rc" = "4" ] && [ ! -e "$OUT/t2.exe" ] && grep -q "error\[corrupt\]: runtime staticlib integrity check failed" "$OUT/t2.err"; then
  ok "sidecar .sha256 tamper rejected cleanly with exit 4"
else
  bad "sidecar .sha256 tamper not rejected (rc=$t2rc)"
  cat "$OUT/t2.out" "$OUT/t2.err"
fi

# 5c: Corrupt manifest JSON
printf "{corrupt\n" > "$OUT/bad_manifest.json"
SPID3R_RUNTIME_MANIFEST="$OUT/bad_manifest.json" \
  "$SH" build "$OUT/probe.matrix" -o "$OUT/t3.exe" > "$OUT/t3.out" 2> "$OUT/t3.err" < /dev/null
t3rc=$?
if [ "$t3rc" = "4" ] && [ ! -e "$OUT/t3.exe" ] && grep -q "error\[corrupt\]:" "$OUT/t3.err"; then
  ok "corrupt manifest JSON rejected with exit 4"
else
  bad "corrupt manifest JSON failed wrong (rc=$t3rc)"
  cat "$OUT/t3.out" "$OUT/t3.err"
fi

# 5d: Unpinned staticlib when SPID3R_RUNTIME_REQUIRE_PIN=1
mkdir -p "$OUT/unpinned"
cp "$OUT/bundle/$LIBNAME" "$OUT/unpinned/$LIBNAME"
SPID3R_RUNTIME_LIB="$OUT/unpinned/$LIBNAME" SPID3R_RUNTIME_REQUIRE_PIN=1 \
  "$SH" build "$OUT/probe.matrix" -o "$OUT/t4.exe" > "$OUT/t4.out" 2> "$OUT/t4.err" < /dev/null
t4rc=$?
if [ "$t4rc" = "4" ] && [ ! -e "$OUT/t4.exe" ] && grep -q "error\[corrupt\]:.*no SHA-256 pin" "$OUT/t4.err"; then
  ok "unpinned staticlib with SPID3R_RUNTIME_REQUIRE_PIN=1 rejected with exit 4"
else
  bad "unpinned staticlib not rejected under SPID3R_RUNTIME_REQUIRE_PIN=1 (rc=$t4rc)"
  cat "$OUT/t4.out" "$OUT/t4.err"
fi

# 5e: Pin conflict (manifest vs mismatched SPID3R_RUNTIME_SHA256)
SPID3R_RUNTIME_MANIFEST="$MANIFEST" \
SPID3R_RUNTIME_SHA256="0000000000000000000000000000000000000000000000000000000000000000" \
  "$SH" build "$OUT/probe.matrix" -o "$OUT/t5.exe" > "$OUT/t5.out" 2> "$OUT/t5.err" < /dev/null
t5rc=$?
if [ "$t5rc" = "4" ] && [ ! -e "$OUT/t5.exe" ] && grep -q "error\[corrupt\]:.*conflict" "$OUT/t5.err"; then
  ok "checksum pin conflict rejected with exit 4"
else
  bad "checksum pin conflict not rejected (rc=$t5rc)"
  cat "$OUT/t5.out" "$OUT/t5.err"
fi

echo "SUMMARY: pass=$pass fail=$fail"
if [ "$fail" != "0" ]; then
  exit 1
fi
exit 0
