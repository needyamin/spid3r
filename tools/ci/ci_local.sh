#!/usr/bin/env bash
# Single-command local CI + Release preflight mirroring the blocking checks in
# `.github/workflows/ci.yml` and `.github/workflows/release.yml` sequentially so
# pushing/tagging never relies on GitHub Actions as the primary debugger.
#
# Usage:
#   bash tools/ci/ci_local.sh [--quick]
#     default: full CI + release parity (includes release staticlib/binaries,
#              WASM target checks, selfhost Stage 1->2->3 fixed point, and
#              P-5 size budgets)
#     --quick: fast loop using debug staticlib/binaries and filtered engine_matrix
set -eu

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

QUICK=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --quick) QUICK=1; shift ;;
    -h|--help)
      sed -n '2,12p' "$0" | sed 's/^# \?//'
      exit 0 ;;
    *)
      echo "error[usage]: unknown option '$1'" >&2
      exit 2 ;;
  esac
done

TMP_DIR="$(mktemp -d)"
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

PY_CMD=""
if command -v python3 >/dev/null 2>&1 && python3 --version >/dev/null 2>&1; then
  PY_CMD="python3"
elif command -v python >/dev/null 2>&1 && python --version >/dev/null 2>&1; then
  PY_CMD="python"
else
  echo "FAIL: neither python3 nor python found" >&2
  exit 2
fi

if [ -d "C:/LLVM-23" ] && [ -z "${LLVM_SYS_231_PREFIX:-}" ]; then
  export LLVM_SYS_231_PREFIX="C:/LLVM-23"
  export SPID3R_LLVM_LIB_DIR="${SPID3R_LLVM_LIB_DIR:-C:/LLVM-23/lib}"
  export PATH="$PATH:/c/LLVM-23/bin"
fi

EXE_SUF=""
LIB_NAME="libspid3r_lang.a"
HOST_TRIPLE="$(rustc -vV | awk '/^host:/ { print $2; exit }')"
case "$HOST_TRIPLE" in
  *windows*)
    EXE_SUF=".exe"
    LIB_NAME="spid3r_lang.lib"
    ;;
esac

step() {
  local label="$1"
  shift
  echo "==> $label"
  "$@"
}

# 1-4: Format, unsafe/SAFETY invariant, & Clippy across core, gui, and native feature sets.
step "Rustfmt check" cargo fmt --all -- --check
step "Unsafe / SAFETY invariant gate" "$PY_CMD" tools/ci/check_unsafe_safety.py
step "Clippy (core, zero-deps)" cargo clippy --no-default-features --all-targets -- -D warnings
step "Clippy (gui feature)" cargo clippy --no-default-features --features gui --all-targets -- -D warnings
step "Clippy (native feature)" cargo clippy --all-targets --features native -- -D warnings
INSTALLED_TARGETS="$(rustup target list --installed 2>/dev/null || true)"
for cross_t in x86_64-unknown-linux-gnu x86_64-apple-darwin; do
  if [ "$cross_t" != "$HOST_TRIPLE" ] && printf '%s\n' "$INSTALLED_TARGETS" | grep -qx "$cross_t"; then
    step "Clippy (cross-target $cross_t, zero-deps)" \
      cargo clippy --target "$cross_t" --lib --no-default-features -- -D warnings
  fi
done

# 5-7: Core, GUI, and Native test suites.
step "Tests (core, zero-deps)" cargo test --no-default-features
step "Tests (gui integration)" cargo test --no-default-features --features gui --test gui_tests
if [ "$QUICK" -eq 1 ]; then
  step "Tests (native engine_matrix)" cargo test --no-default-features --features native --test engine_matrix
else
  step "Tests (native full suite)" cargo test --no-default-features --features native
fi

# 8-9: Differential suite + formatting gates on examples, benchmarks, and selfhost/main.matrix.
if [ "$QUICK" -eq 1 ]; then
  step "Build debug stage0 (native+gui)" cargo build --bins --features "native gui"
  STAGE0="target/debug/spid3r${EXE_SUF}"
else
  step "Build slim release runtime staticlib (native)" \
    cargo build --release --locked --lib --no-default-features --features native
  mkdir -p "$TMP_DIR/rt-native"
  cp -f "target/release/${LIB_NAME}" "$TMP_DIR/rt-native/${LIB_NAME}"
  step "Build release stage0 (gui+native+tls)" \
    cargo build --release --locked --bins --no-default-features --features gui,native,tls
  STAGE0="target/release/spid3r${EXE_SUF}"
fi

step "3-engine differential (spid3r test)" "$STAGE0" test
step "Format check (examples/ + benchmarks/)" "$STAGE0" fmt --check
step "Format check (selfhost/main.matrix)" "$STAGE0" fmt --check selfhost/main.matrix

# 10: WebAssembly CI parity.
step "WASM Clippy (wasm32-unknown-unknown)" \
  cargo clippy --target wasm32-unknown-unknown --lib --no-default-features -- -D warnings
step "WASM Build (release)" \
  cargo build --target wasm32-unknown-unknown --release --lib --no-default-features
step "WASM embedded parity vs fresh build" \
  node tools/wasm/test_wasm_embedded.js --against target/wasm32-unknown-unknown/release/spid3r_lang.wasm
step "WASM runtime smoke test" node tools/wasm/test_wasm_runtime.js
step "WASM playground presets" node tools/wasm/test_playground_presets.js
step "Docs code snippets & anchors" node tools/docs/verify_dev_snippets.js
step "Docs prose claims" node tools/docs/verify_doc_claims.js
step "WASM embedded JS bundle fresh check" \
  bash -c "node tools/wasm/gen_wasm_data.js '$TMP_DIR/spid3r_wasm_data.js' && cmp -s web/spid3r_wasm_data.js '$TMP_DIR/spid3r_wasm_data.js'"

# 11: Manifest & fixture integrity checks.
step "Selfhost frozen corpus manifest" "$PY_CMD" tools/selfhost/corpus_manifest.py verify
step "Phase 0 vendored source digests" "$PY_CMD" tools/bench/phase0_manifest.py --verify
step "Phase 0 checksum fixtures" "$PY_CMD" tools/bench/phase0_fixtures.py --verify

# 12: Selfhost Stage 1 -> Stage 2 -> Stage 3 release fixed point + smoke tests.
STATICLIB="target/debug/${LIB_NAME}"
if [ "$QUICK" -eq 0 ]; then
  STATICLIB="$TMP_DIR/rt-native/${LIB_NAME}"
fi
step "Selfhost release pipeline (Stage 1->2->3 + smoke + budgets)" \
  bash tools/release/release_selfhost.sh build \
    --target "$HOST_TRIPLE" \
    --stage0 "$STAGE0" \
    --staticlib "$STATICLIB" \
    --out-dir "$TMP_DIR/release-out" \
    --skip-installer

echo "CI_LOCAL PASS: all blocking CI and release gates succeeded locally."
