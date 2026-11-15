#!/usr/bin/env bash
# Self-host Phase 8.3 parity: `pack`, `unpack`, `verify`, `dist` vs Stage 0.
#
# Usage: bash tools/selfhost_pkg_parity.sh [stage0] [stage1]
set -u
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
S0_REL="${1:-target/debug/spid3r}"
S1_REL="${2:-selfhost/main_native}"
[ -x "$S0_REL" ] || S0_REL="${S0_REL}.exe"
[ -x "$S1_REL" ] || S1_REL="${S1_REL}.exe"
S0="$ROOT/$S0_REL"
S1="$ROOT/$S1_REL"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

pass=0
fail=0

# ---------------------------------------------------------------- 1. Single project pack/unpack
P0="$TMP/proj0"
P1="$TMP/proj1"
mkdir -p "$P0/src/a" "$P0/src/a-b" "$P0/src/assets"
cat >"$P0/spider.toml" <<'EOF'
# comment line
[package]
name = "demo_pkg"
version = "1.2.3"
entry = "src/main.matrix"
EOF
cat >"$P0/src/main.matrix" <<'EOF'
import "a/util";
fn main() {
    println(hello_pkg());
}
EOF
cat >"$P0/src/a/util.matrix" <<'EOF'
public fn hello_pkg() -> str {
    return "from demo_pkg";
}
EOF
echo 'public fn other() -> int { return 7; }' >"$P0/src/a-b/other.matrix"
# Non-UTF-8 binary payload in src/assets/bin.dat
printf '\377\000\177\200' >"$P0/src/assets/bin.dat"
# Path > 100 chars cleanly splittable for ustar prefix:
LONG_DIR="src/$(printf 'x%.0s' $(seq 1 90))"
mkdir -p "$P0/$LONG_DIR"
echo 'let dummy = 1;' >"$P0/$LONG_DIR/file.matrix"

cp -r "$P0" "$P1"

(cd "$P0" && "$S0" pack </dev/null >"$TMP/pack0.out" 2>"$TMP/pack0.err"); oc=$?
(cd "$P1" && "$S1" pack </dev/null >"$TMP/pack1.out" 2>"$TMP/pack1.err"); sc=$?
if [ "$oc" = 0 ] && [ "$sc" = 0 ] \
   && cmp -s "$P0/dist/demo_pkg-1.2.3.tar" "$P1/dist/demo_pkg-1.2.3.tar"; then
  pass=$((pass + 1))
else
  echo "FAIL (single-pack byte-identity): oc=$oc sc=$sc"
  cat "$TMP/pack0.err" "$TMP/pack1.err" | sed 's/^/  /'
  fail=$((fail + 1))
fi

# Verify sha256(tree) and archive sha256 stdout lines match too
t0="$(grep '^sha256(tree):' "$TMP/pack0.out" | tr -d '\r')"
t1="$(grep '^sha256(tree):' "$TMP/pack1.out" | tr -d '\r')"
h0="$(grep '^sha256:' "$TMP/pack0.out" | tr -d '\r')"
h1="$(grep '^sha256:' "$TMP/pack1.out" | tr -d '\r')"
if [ -n "$t0" ] && [ "$t0" = "$t1" ] && [ -n "$h0" ] && [ "$h0" = "$h1" ]; then
  pass=$((pass + 1))
else
  echo "FAIL (single-pack hashes): t0=[$t0] t1=[$t1] h0=[$h0] h1=[$h1]"
  fail=$((fail + 1))
fi

# Unpack round-trip
"$S0" unpack "$P0/dist/demo_pkg-1.2.3.tar" "$TMP/unp0" </dev/null >"$TMP/unp0.out" 2>"$TMP/unp0.err"; oc=$?
"$S1" unpack "$P1/dist/demo_pkg-1.2.3.tar" "$TMP/unp1" </dev/null >"$TMP/unp1.out" 2>"$TMP/unp1.err"; sc=$?
if [ "$oc" = 0 ] && [ "$sc" = 0 ] && diff -qr "$TMP/unp0" "$TMP/unp1" >/dev/null; then
  pass=$((pass + 1))
else
  echo "FAIL (unpack roundtrip): oc=$oc sc=$sc"
  cat "$TMP/unp0.err" "$TMP/unp1.err" | sed 's/^/  /'
  fail=$((fail + 1))
fi

# Refuses clobbering existing dir
"$S0" unpack "$P0/dist/demo_pkg-1.2.3.tar" "$TMP/unp0" </dev/null >/dev/null 2>&1; oc=$?
"$S1" unpack "$P1/dist/demo_pkg-1.2.3.tar" "$TMP/unp1" </dev/null >/dev/null 2>&1; sc=$?
if [ "$oc" = 2 ] && [ "$sc" = 2 ]; then
  pass=$((pass + 1))
else
  echo "FAIL (unpack no-clobber): oc=$oc sc=$sc"
  fail=$((fail + 1))
fi

# Corrupt tar header checksum detection
cp "$P1/dist/demo_pkg-1.2.3.tar" "$TMP/bad.tar"
printf 'X' | dd of="$TMP/bad.tar" bs=1 seek=0 count=1 conv=notrunc 2>/dev/null
"$S0" unpack "$TMP/bad.tar" "$TMP/bad_out0" </dev/null >/dev/null 2>&1; oc=$?
"$S1" unpack "$TMP/bad.tar" "$TMP/bad_out1" </dev/null >/dev/null 2>&1; sc=$?
if [ "$oc" = 2 ] && [ "$sc" = 2 ]; then
  pass=$((pass + 1))
else
  echo "FAIL (unpack corrupted checksum): oc=$oc sc=$sc"
  fail=$((fail + 1))
fi

# ---------------------------------------------------------------- 2. Workspace pack
W0="$TMP/ws0"
W1="$TMP/ws1"
mkdir -p "$W0/alpha/src" "$W0/beta/src"
cat >"$W0/spider.toml" <<'EOF'
[workspace]
members = ["alpha", "beta"]
EOF
cat >"$W0/alpha/spider.toml" <<'EOF'
[package]
name = "alpha_lib"
version = "0.1.0"
EOF
echo 'public fn a() -> int { return 1; }' >"$W0/alpha/src/main.matrix"
cat >"$W0/beta/spider.toml" <<'EOF'
[package]
name = "beta_app"
version = "0.2.0"
EOF
echo 'fn main() { println(42); }' >"$W0/beta/src/main.matrix"
cp -r "$W0" "$W1"

(cd "$W0" && "$S0" pack </dev/null >"$TMP/wpack0.out" 2>"$TMP/wpack0.err"); oc=$?
(cd "$W1" && "$S1" pack </dev/null >"$TMP/wpack1.out" 2>"$TMP/wpack1.err"); sc=$?
if [ "$oc" = 0 ] && [ "$sc" = 0 ] \
   && cmp -s "$W0/dist/alpha_lib-0.1.0.tar" "$W1/dist/alpha_lib-0.1.0.tar" \
   && cmp -s "$W0/dist/beta_app-0.2.0.tar" "$W1/dist/beta_app-0.2.0.tar"; then
  pass=$((pass + 1))
else
  echo "FAIL (workspace-pack): oc=$oc sc=$sc"
  cat "$TMP/wpack0.err" "$TMP/wpack1.err" | sed 's/^/  /'
  fail=$((fail + 1))
fi

# ---------------------------------------------------------------- 3. Lockfile / verify
VP="$TMP/vproj"
mkdir -p "$VP/src" "$VP/vendor/dep_ok/src"
cat >"$VP/spider.toml" <<'EOF'
[package]
name = "vproj"
version = "0.1.0"
EOF
echo 'fn main() {}' >"$VP/src/main.matrix"
echo 'public fn k() -> int { return 9; }' >"$VP/vendor/dep_ok/src/lib.matrix"
# Compute the exact vendor content hash using stage-0 pack tree/content hash or spid3r itself
mkdir -p "$VP/dep_src/src"
cat >"$VP/dep_src/spider.toml" <<'EOF'
[package]
name = "dep_ok"
version = "0.5.0"
EOF
cp "$VP/vendor/dep_ok/src/lib.matrix" "$VP/dep_src/src/lib.matrix"
# Compute exact vendor/dep_ok hash via stage-0 `install` using a file-based local registry or path?
# Using a local registry dir gives a real registry+ entry with sha256 + files count!
REG="$TMP/reg"
mkdir -p "$REG/dep_ok"
(cd "$VP/dep_src" && "$S0" pack -o "$REG/dep_ok/dep_ok-0.5.0.tar" </dev/null >"$TMP/dep_tree.out")
DEP_HASH="$(grep '^sha256(tree):' "$TMP/dep_tree.out" | awk '{print $2}' | tr -d '\r')"
cat >"$VP/spider.lock" <<EOF
[[package]]
name = "dep_ok"
version = "0.5.0"
source = "registry+local"
files = 1
sha256 = "$DEP_HASH"
EOF
rm -rf "$VP/vendor/dep_ok/src"
mkdir -p "$VP/vendor/dep_ok"
cp "$VP/dep_src/src/lib.matrix" "$VP/vendor/dep_ok/lib.matrix"

(cd "$VP" && "$S0" verify </dev/null >"$TMP/v0.out" 2>"$TMP/v0.err"); oc=$?
(cd "$VP" && "$S1" verify </dev/null >"$TMP/v1.out" 2>"$TMP/v1.err"); sc=$?
if [ "$oc" = 0 ] && [ "$sc" = 0 ] && cmp -s "$TMP/v0.out" "$TMP/v1.out"; then
  pass=$((pass + 1))
else
  echo "FAIL (verify-clean): oc=$oc sc=$sc"
  diff -u "$TMP/v0.out" "$TMP/v1.out" | sed 's/^/  /'
  cat "$TMP/v0.err" "$TMP/v1.err" | sed 's/^/  /'
  fail=$((fail + 1))
fi

# Tamper vendor/dep_ok/lib.matrix -> verify must exit 1
echo '// tampered!' >> "$VP/vendor/dep_ok/lib.matrix"
(cd "$VP" && "$S0" verify </dev/null >/dev/null 2>"$TMP/vt0.err"); oc=$?
(cd "$VP" && "$S1" verify </dev/null >/dev/null 2>"$TMP/vt1.err"); sc=$?
if [ "$oc" = 1 ] && [ "$sc" = 1 ] && grep -q "does not match spider.lock" "$TMP/vt1.err"; then
  pass=$((pass + 1))
else
  echo "FAIL (verify-tampered): oc=$oc sc=$sc"
  cat "$TMP/vt1.err" | sed 's/^/  /'
  fail=$((fail + 1))
fi

# Extra unlocked vendor dir -> verify must exit 1
mkdir -p "$VP/vendor/rogue_dep"
echo 'let x = 1;' >"$VP/vendor/rogue_dep/mod.matrix"
(cd "$VP" && "$S0" verify </dev/null >/dev/null 2>&1); oc=$?
(cd "$VP" && "$S1" verify </dev/null >/dev/null 2>"$TMP/vu1.err"); sc=$?
if [ "$oc" = 1 ] && [ "$sc" = 1 ] && grep -q "absent from spider.lock" "$TMP/vu1.err"; then
  pass=$((pass + 1))
else
  echo "FAIL (verify-unlocked): oc=$oc sc=$sc"
  cat "$TMP/vu1.err" | sed 's/^/  /'
  fail=$((fail + 1))
fi

# ---------------------------------------------------------------- 4. dist
DP="$TMP/dproj"
mkdir -p "$DP/src"
cat >"$DP/spider.toml" <<'EOF'
[package]
name = "dist_app"
version = "0.1.0"
entry = "src/main.matrix"
EOF
cat >"$DP/src/main.matrix" <<'EOF'
import "helper";
fn main() {
    println(greet());
}
EOF
cat >"$DP/src/helper.matrix" <<'EOF'
public fn greet() -> string {
    return "dist_ok";
}
EOF

(cd "$DP" && "$S0" dist </dev/null >"$TMP/d0.out" 2>"$TMP/d0.err"); oc=$?
mv "$DP/target/dist_app.matrixc" "$TMP/dist_s0.matrixc"
(cd "$DP" && "$S1" dist --no-opt </dev/null >"$TMP/d1.out" 2>"$TMP/d1.err"); sc=$?
mv "$DP/target/dist_app.matrixc" "$TMP/dist_s1.matrixc"
r0="$("$S0" exec "$TMP/dist_s0.matrixc" </dev/null 2>&1)"
r1="$("$S0" exec "$TMP/dist_s1.matrixc" </dev/null 2>&1)"
if [ "$oc" = 0 ] && [ "$sc" = 0 ] && [ "$r0" = "dist_ok" ] && [ "$r1" = "dist_ok" ]; then
  pass=$((pass + 1))
else
  echo "FAIL (dist): oc=$oc sc=$sc r0=[$r0] r1=[$r1]"
  cat "$TMP/d0.err" "$TMP/d1.err" | sed 's/^/  /'
  fail=$((fail + 1))
fi

# dist missing vendor error -> exit 2
cat >"$DP/spider.toml" <<'EOF'
[package]
name = "dist_app"
version = "0.1.0"
[dependencies]
foo = "0.1.0"
EOF
(cd "$DP" && "$S0" dist </dev/null >/dev/null 2>&1); oc=$?
(cd "$DP" && "$S1" dist </dev/null >/dev/null 2>&1); sc=$?
if [ "$oc" = 2 ] && [ "$sc" = 2 ]; then
  pass=$((pass + 1))
else
  echo "FAIL (dist-missing-vendor): oc=$oc sc=$sc"
  fail=$((fail + 1))
fi

echo "pkg parity: pass=$pass fail=$fail"
if [ "$fail" -ne 0 ]; then
  exit 1
fi
echo "SELFHOST PKG PARITY: ALL MATCH"
