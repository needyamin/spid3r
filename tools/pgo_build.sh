#!/usr/bin/env bash
# Two-stage LTO + split-PGO toolchain and runtime staticlib build driver
# (roadmap Phase 11 P-4, docs/SELFHOST.md SH-6).
#
# Usage:
#   bash tools/pgo_build.sh [options]
#     --out-dir <dir>             Destination dir for hardened binaries (default: target/release)
#     --work-dir <dir>            Scratch build/profile dir (default: mktemp; removed on exit)
#     --keep-work                 Keep --work-dir (retains s0.profdata / rt.profdata)
#     --bin-features <list>       Cargo features for `spid3r` binary (default: auto)
#     --lib-features <list>       Cargo features for `spid3r_lang` runtime staticlib (default: native)
#     --build-selfhost <path>     Also emit Stage-1 self-hosted compiler via PGO Stage 0 + runtime
#     --verify-fixed-point        Build Stage 2/3 via Stage 1 and assert byte-identical spid3r_main.o
#     --llvm-profdata <path>      Override llvm-profdata executable
#     --llvm-ar <path>            Override llvm-lib (MSVC) or llvm-ar/ar archive combiner
#     --profiler-builtins <path>  Override libprofiler_builtins-*.rlib path
#     --help                      Show this help and exit 0
#
# Exit codes (README.md contract): 0 = success, 2 = usage/tooling/profile failure.
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) IS_WIN=1 ;;
  *)                    IS_WIN=0 ;;
esac

OUT_DIR="$ROOT/target/release"
WORK_DIR=""
KEEP_WORK=0
BIN_FEATURES=""
LIB_FEATURES="native"
SELFHOST_OUT=""
VERIFY_FP=0
LLVM_PROFDATA=""
LLVM_AR=""
PROFILER_RLIB=""
TARGET_TRIPLE=""

usage() {
  sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'
  exit 0
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --out-dir)            OUT_DIR="${2:?error[usage]: --out-dir requires a value}"; shift 2 ;;
    --work-dir)           WORK_DIR="${2:?error[usage]: --work-dir requires a value}"; shift 2 ;;
    --keep-work)          KEEP_WORK=1; shift ;;
    --bin-features)       BIN_FEATURES="${2:?error[usage]: --bin-features requires a value}"; shift 2 ;;
    --lib-features)       LIB_FEATURES="${2:?error[usage]: --lib-features requires a value}"; shift 2 ;;
    --target)             TARGET_TRIPLE="${2:?error[usage]: --target requires a value}"; shift 2 ;;
    --build-selfhost)     SELFHOST_OUT="${2:?error[usage]: --build-selfhost requires a value}"; shift 2 ;;
    --verify-fixed-point) VERIFY_FP=1; shift ;;
    --llvm-profdata)      LLVM_PROFDATA="${2:?error[usage]: --llvm-profdata requires a value}"; shift 2 ;;
    --llvm-ar)            LLVM_AR="${2:?error[usage]: --llvm-ar requires a value}"; shift 2 ;;
    --profiler-builtins)  PROFILER_RLIB="${2:?error[usage]: --profiler-builtins requires a value}"; shift 2 ;;
    --help|-h)            usage ;;
    *)
      echo "error[usage]: unknown option '$1'" >&2
      exit 2 ;;
  esac
done

if [ -z "$BIN_FEATURES" ]; then
  if [ "$IS_WIN" -eq 1 ]; then
    BIN_FEATURES="gui,native,tls"
  else
    BIN_FEATURES="native"
  fi
fi

EXE_SUF=""
LIB_NAME="libspid3r_lang.a"
if [ "$IS_WIN" -eq 1 ]; then
  EXE_SUF=".exe"
  LIB_NAME="spid3r_lang.lib"
fi

if [ -z "$WORK_DIR" ]; then
  WORK_DIR="$(mktemp -d)"
  AUTO_WORK=1
else
  mkdir -p "$WORK_DIR"
  AUTO_WORK=0
fi

cleanup() {
  if [ "$KEEP_WORK" -eq 0 ] && [ "$AUTO_WORK" -eq 1 ] && [ -n "$WORK_DIR" ]; then
    rm -rf "$WORK_DIR"
  fi
}
trap cleanup EXIT

# Normalize native path for MSVC/LLVM CLI arguments on Git Bash/MSYS2.
to_native_path() {
  if [ "$IS_WIN" -eq 1 ] && command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1"
  else
    printf '%s' "$1"
  fi
}

find_llvm_profdata() {
  if [ -n "$LLVM_PROFDATA" ]; then
    if [ ! -x "$LLVM_PROFDATA" ] && ! command -v "$LLVM_PROFDATA" >/dev/null 2>&1; then
      echo "error[usage]: --llvm-profdata not found or not executable: $LLVM_PROFDATA" >&2
      exit 2
    fi
    echo "$LLVM_PROFDATA"
    return 0
  fi
  local host_triple sysroot
  host_triple="$(rustc -vV 2>/dev/null | awk '/^host:/ { print $2; exit }')"
  sysroot="$(rustc --print sysroot 2>/dev/null || true)"
  for c in \
    "llvm-profdata" \
    "llvm-profdata-23" \
    "C:/LLVM-23/bin/llvm-profdata.exe" \
    "C:/Program Files/LLVM/bin/llvm-profdata.exe" \
    "${sysroot}/lib/rustlib/${host_triple}/bin/llvm-profdata${EXE_SUF}" \
    "/usr/lib/llvm-23/bin/llvm-profdata" \
    "/opt/homebrew/opt/llvm/bin/llvm-profdata" \
    "/usr/local/opt/llvm/bin/llvm-profdata"; do
    if command -v "$c" >/dev/null 2>&1 || [ -x "$c" ]; then
      echo "$c"
      return 0
    fi
  done
  echo "error[usage]: llvm-profdata not found (install LLVM 23 or rustup component llvm-tools)" >&2
  exit 2
}

find_archive_tool() {
  if [ -n "$LLVM_AR" ]; then
    if [ ! -x "$LLVM_AR" ] && ! command -v "$LLVM_AR" >/dev/null 2>&1; then
      echo "error[usage]: --llvm-ar not found or not executable: $LLVM_AR" >&2
      exit 2
    fi
    echo "$LLVM_AR"
    return 0
  fi
  if [ "$IS_WIN" -eq 1 ]; then
    for c in "C:/LLVM-23/bin/llvm-lib.exe" "C:/Program Files/LLVM/bin/llvm-lib.exe" "llvm-lib"; do
      if command -v "$c" >/dev/null 2>&1 || [ -x "$c" ]; then
        echo "$c"
        return 0
      fi
    done
  fi
  for c in "llvm-ar" "llvm-ar-23" "C:/LLVM-23/bin/llvm-ar.exe" "ar"; do
    if command -v "$c" >/dev/null 2>&1 || [ -x "$c" ]; then
      echo "$c"
      return 0
    fi
  done
  echo "error[usage]: llvm-lib / llvm-ar / ar not found for PGO runtime archive bundling" >&2
  exit 2
}

find_profiler_builtins() {
  if [ -n "$PROFILER_RLIB" ]; then
    if [ ! -f "$PROFILER_RLIB" ]; then
      echo "error[usage]: --profiler-builtins file not found: $PROFILER_RLIB" >&2
      exit 2
    fi
    echo "$PROFILER_RLIB"
    return 0
  fi
  local libdir f
  libdir="$(rustc --print target-libdir 2>/dev/null || true)"
  if [ -n "$libdir" ] && [ -d "$libdir" ]; then
    for f in "$libdir"/libprofiler_builtins-*.rlib; do
      if [ -f "$f" ]; then
        echo "$f"
        return 0
      fi
    done
  fi
  echo "error[usage]: libprofiler_builtins-*.rlib not found in rustc target-libdir ($libdir)" >&2
  exit 2
}

merge_and_verify_profile() {
  local profraw_dir="$1"
  local out_profdata="$2"
  local label="$3"
  local files=()
  local f
  for f in "$profraw_dir"/*.profraw; do
    [ -f "$f" ] || continue
    if [ ! -s "$f" ]; then
      echo "error[usage]: empty 0-byte .profraw file for $label: $f" >&2
      exit 2
    fi
    files+=("$f")
  done
  if [ "${#files[@]}" -eq 0 ]; then
    echo "error[usage]: no .profraw files generated for $label in $profraw_dir" >&2
    exit 2
  fi
  local win_out
  win_out="$(to_native_path "$out_profdata")"
  local native_inputs=()
  for f in "${files[@]}"; do
    native_inputs+=("$(to_native_path "$f")")
  done
  MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*' "$PROFDATA_BIN" merge -o "$win_out" "${native_inputs[@]}"
  if [ ! -s "$out_profdata" ]; then
    echo "error[usage]: merged profile is missing or empty: $out_profdata" >&2
    exit 2
  fi
  MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*' "$PROFDATA_BIN" show "$win_out" >/dev/null || {
    echo "error[usage]: corrupt profile rejected by llvm-profdata: $out_profdata" >&2
    exit 2
  }
}

combine_inst_staticlib() {
  local raw_lib="$1"
  local prof_rlib="$2"
  local out_lib="$3"
  local tool_base
  tool_base="$(basename "$AR_BIN" | tr '[:upper:]' '[:lower:]')"
  case "$tool_base" in
    llvm-lib*)
      MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*' "$AR_BIN" \
        "/OUT:$(to_native_path "$out_lib")" \
        "$(to_native_path "$raw_lib")" \
        "$(to_native_path "$prof_rlib")"
      ;;
    *)
      local mri="$WORK_DIR/combine_inst.mri"
      cat > "$mri" <<EOF
CREATE $out_lib
ADDLIB $raw_lib
ADDLIB $prof_rlib
SAVE
END
EOF
      "$AR_BIN" -M < "$mri"
      ;;
  esac
  if [ ! -s "$out_lib" ]; then
    echo "error[usage]: failed to build instrumented runtime staticlib: $out_lib" >&2
    exit 2
  fi
}

cargo_build_with_flags() {
  local extra_flag="$1"
  shift
  local rf=""
  if [ "$IS_WIN" -eq 1 ]; then
    # Preserve .cargo/config.toml +crt-static invariant whenever RUSTFLAGS is set.
    rf="-C target-feature=+crt-static $extra_flag"
  else
    rf="$extra_flag"
  fi
  local args=(cargo build --release --locked)
  if [ -n "$TARGET_TRIPLE" ]; then
    args+=(--target "$TARGET_TRIPLE")
  fi
  args+=("$@")
  SPID3R_NO_INSTALLER=1 RUSTFLAGS="$rf" "${args[@]}"
}

target_rel_dir() {
  local tdir="$1"
  if [ -n "$TARGET_TRIPLE" ]; then
    echo "$tdir/$TARGET_TRIPLE/release"
  else
    echo "$tdir/release"
  fi
}

sha256_hex() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print tolower($1)}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print tolower($1)}'
  else
    echo "error[usage]: sha256sum / shasum not found" >&2
    exit 2
  fi
}

PROFDATA_BIN="$(find_llvm_profdata)"
AR_BIN="$(find_archive_tool)"
PROFILER_RLIB="$(find_profiler_builtins)"

mkdir -p "$OUT_DIR" \
  "$WORK_DIR/prof_s0" "$WORK_DIR/prof_rt" "$WORK_DIR/tmp" \
  "$WORK_DIR/t_inst_bin" "$WORK_DIR/t_inst_lib" \
  "$WORK_DIR/t_pgo_bin" "$WORK_DIR/t_pgo_lib"

echo "[pgo_build] work_dir=$WORK_DIR out_dir=$OUT_DIR"
echo "[pgo_build] bin_features=$BIN_FEATURES lib_features=$LIB_FEATURES"
echo "[pgo_build] llvm-profdata=$PROFDATA_BIN profiler_builtins=$PROFILER_RLIB"

echo "[pgo_build] 1/5 Building instrumented Stage-0 compiler..."
cargo_build_with_flags "-C profile-generate=$(to_native_path "$WORK_DIR/prof_s0")" \
  --bin spid3r --no-default-features --features "$BIN_FEATURES" \
  --target-dir "$WORK_DIR/t_inst_bin"

INST_S0="$(target_rel_dir "$WORK_DIR/t_inst_bin")/spid3r${EXE_SUF}"
[ -x "$INST_S0" ] || { echo "error[usage]: instrumented Stage-0 missing: $INST_S0" >&2; exit 2; }

echo "[pgo_build] 2/5 Building instrumented runtime staticlib + Stage-1 compiler..."
cargo_build_with_flags "-C profile-generate=$(to_native_path "$WORK_DIR/prof_rt")" \
  --lib --no-default-features --features "$LIB_FEATURES" \
  --target-dir "$WORK_DIR/t_inst_lib"

RAW_INST_LIB="$(target_rel_dir "$WORK_DIR/t_inst_lib")/$LIB_NAME"
[ -f "$RAW_INST_LIB" ] || { echo "error[usage]: instrumented runtime staticlib missing: $RAW_INST_LIB" >&2; exit 2; }

INST_LIB="$WORK_DIR/$LIB_NAME"
combine_inst_staticlib "$RAW_INST_LIB" "$PROFILER_RLIB" "$INST_LIB"

INST_S1="$WORK_DIR/s1_inst${EXE_SUF}"
SPID3R_RUNTIME_LIB="$(to_native_path "$INST_LIB")" \
  "$INST_S0" build --release "$ROOT/selfhost/main.matrix" -o "$INST_S1" >/dev/null
[ -x "$INST_S1" ] || { echo "error[usage]: failed to build instrumented Stage 1: $INST_S1" >&2; exit 2; }

echo "[pgo_build] 3/5 Collecting split profiles (Stage-0 compiler/VM + Stage-1 self-compile/runtime)..."
# Drop stale .profraw emitted by build-scripts / compile step before workload run
rm -f "$WORK_DIR/prof_s0"/*.profraw "$WORK_DIR/prof_rt"/*.profraw

S0_PROF_PAT="$(to_native_path "$WORK_DIR/prof_s0")/s0_%p_%m.profraw"
RT_PAT_S1="$(to_native_path "$WORK_DIR/prof_rt")/rt_%p_%m.profraw"
INST_LIB_NAT="$(to_native_path "$INST_LIB")"

# Stage-0 workloads: unopt build (2x matches phase0 self_compile), release build, lex, check, VM
LLVM_PROFILE_FILE="$S0_PROF_PAT" SPID3R_RUNTIME_LIB="$INST_LIB_NAT" \
  "$INST_S0" build "$ROOT/selfhost/main.matrix" -o "$WORK_DIR/tmp/s0_dbg${EXE_SUF}" >/dev/null 2>&1
LLVM_PROFILE_FILE="$S0_PROF_PAT" SPID3R_RUNTIME_LIB="$INST_LIB_NAT" \
  "$INST_S0" build "$ROOT/selfhost/main.matrix" -o "$WORK_DIR/tmp/s0_dbg${EXE_SUF}" >/dev/null 2>&1
LLVM_PROFILE_FILE="$S0_PROF_PAT" SPID3R_RUNTIME_LIB="$INST_LIB_NAT" \
  "$INST_S0" build --release "$ROOT/selfhost/main.matrix" -o "$WORK_DIR/tmp/s0_rel${EXE_SUF}" >/dev/null 2>&1
LLVM_PROFILE_FILE="$S0_PROF_PAT" \
  "$INST_S0" tokens --json "$ROOT/benchmarks/selfhost/lex_workload.matrix" >/dev/null
LLVM_PROFILE_FILE="$S0_PROF_PAT" \
  "$INST_S0" check "$ROOT/selfhost/main.matrix" >/dev/null 2>&1
LLVM_PROFILE_FILE="$S0_PROF_PAT" \
  "$INST_S0" run --engine vm "$ROOT/benchmarks/compare/prime.matrix" < /dev/null >/dev/null
LLVM_PROFILE_FILE="$S0_PROF_PAT" \
  "$INST_S0" run --engine vm "$ROOT/benchmarks/compare/map.matrix" < /dev/null >/dev/null

# Stage-1 self-compile + AOT runtime workloads (separate profile so rt loops do not dwarf Stage 0)
LLVM_PROFILE_FILE="$RT_PAT_S1" SPID3R_RUNTIME_LIB="$INST_LIB_NAT" \
  "$INST_S1" build "$ROOT/selfhost/main.matrix" -o "$WORK_DIR/tmp/s2_dbg${EXE_SUF}" >/dev/null 2>&1
LLVM_PROFILE_FILE="$RT_PAT_S1" SPID3R_RUNTIME_LIB="$INST_LIB_NAT" \
  "$INST_S1" build "$ROOT/selfhost/main.matrix" -o "$WORK_DIR/tmp/s2_dbg${EXE_SUF}" >/dev/null 2>&1
LLVM_PROFILE_FILE="$RT_PAT_S1" SPID3R_RUNTIME_LIB="$INST_LIB_NAT" \
  "$INST_S1" build --release "$ROOT/selfhost/main.matrix" -o "$WORK_DIR/tmp/s2_rel${EXE_SUF}" >/dev/null 2>&1
LLVM_PROFILE_FILE="$RT_PAT_S1" \
  "$INST_S1" tokens "$ROOT/benchmarks/selfhost/lex_workload.matrix" >/dev/null
for w in prime json map; do
  LLVM_PROFILE_FILE="$RT_PAT_S1" SPID3R_RUNTIME_LIB="$INST_LIB_NAT" \
    "$INST_S1" build --release "$ROOT/benchmarks/compare/${w}.matrix" -o "$WORK_DIR/tmp/${w}${EXE_SUF}" >/dev/null
  LLVM_PROFILE_FILE="$RT_PAT_S1" "$WORK_DIR/tmp/${w}${EXE_SUF}" < /dev/null >/dev/null
done

S0_PROFDATA="$WORK_DIR/s0.profdata"
RT_PROFDATA="$WORK_DIR/rt.profdata"
merge_and_verify_profile "$WORK_DIR/prof_s0" "$S0_PROFDATA" "Stage-0 compiler"
merge_and_verify_profile "$WORK_DIR/prof_rt" "$RT_PROFDATA" "runtime/self-compile"

echo "[pgo_build] 4/5 Building ThinLTO + PGO hardened Stage-0 compiler & runtime staticlib..."
cargo_build_with_flags "-C profile-use=$(to_native_path "$S0_PROFDATA")" \
  --bin spid3r --no-default-features --features "$BIN_FEATURES" \
  --target-dir "$WORK_DIR/t_pgo_bin"

cargo_build_with_flags "-C profile-use=$(to_native_path "$RT_PROFDATA")" \
  --lib --no-default-features --features "$LIB_FEATURES" \
  --target-dir "$WORK_DIR/t_pgo_lib"

PGO_S0="$(target_rel_dir "$WORK_DIR/t_pgo_bin")/spid3r${EXE_SUF}"
PGO_LIB="$(target_rel_dir "$WORK_DIR/t_pgo_lib")/$LIB_NAME"
[ -x "$PGO_S0" ] || { echo "error[usage]: PGO Stage-0 missing: $PGO_S0" >&2; exit 2; }
[ -f "$PGO_LIB" ] || { echo "error[usage]: PGO staticlib missing: $PGO_LIB" >&2; exit 2; }

cp -f "$PGO_S0" "$OUT_DIR/spid3r${EXE_SUF}"
cp -f "$PGO_LIB" "$OUT_DIR/$LIB_NAME"

if [ "$IS_WIN" -eq 1 ]; then
  S0_LLVM_DLL="$(dirname "$PGO_S0")/LLVM-C.dll"
  if [ -f "$S0_LLVM_DLL" ] && [ ! -f "$OUT_DIR/LLVM-C.dll" ]; then
    cp -f "$S0_LLVM_DLL" "$OUT_DIR/LLVM-C.dll"
  fi
fi

S0_BYTES="$(wc -c < "$OUT_DIR/spid3r${EXE_SUF}" | tr -d '[:space:]')"
LIB_BYTES="$(wc -c < "$OUT_DIR/$LIB_NAME" | tr -d '[:space:]')"
echo "[pgo_build] Hardened Stage 0: $OUT_DIR/spid3r${EXE_SUF} (${S0_BYTES} B)"
echo "[pgo_build] Hardened runtime: $OUT_DIR/$LIB_NAME (${LIB_BYTES} B)"

if [ -n "$SELFHOST_OUT" ] || [ "$VERIFY_FP" -eq 1 ]; then
  S1_FINAL="$SELFHOST_OUT"
  if [ -z "$S1_FINAL" ]; then
    S1_FINAL="$WORK_DIR/spid3r-self-s1${EXE_SUF}"
  fi
  echo "[pgo_build] 5/5 Building Stage-1 self-hosted compiler against PGO runtime..."
  mkdir -p "$(dirname "$S1_FINAL")"
  SPID3R_RUNTIME_LIB="$(to_native_path "$OUT_DIR/$LIB_NAME")" \
    "$OUT_DIR/spid3r${EXE_SUF}" build --release "$ROOT/selfhost/main.matrix" -o "$S1_FINAL"
  S1_BYTES="$(wc -c < "$S1_FINAL" | tr -d '[:space:]')"
  echo "[pgo_build] Hardened Stage 1: $S1_FINAL (${S1_BYTES} B)"

  if [ "$VERIFY_FP" -eq 1 ]; then
    mkdir -p "$WORK_DIR/fp_s2" "$WORK_DIR/fp_s3"
    SPID3R_RUNTIME_LIB="$(to_native_path "$OUT_DIR/$LIB_NAME")" \
      "$S1_FINAL" build --release "$ROOT/selfhost/main.matrix" -o "$WORK_DIR/fp_s2/spid3r-self${EXE_SUF}"
    SPID3R_RUNTIME_LIB="$(to_native_path "$OUT_DIR/$LIB_NAME")" \
      "$WORK_DIR/fp_s2/spid3r-self${EXE_SUF}" build --release "$ROOT/selfhost/main.matrix" -o "$WORK_DIR/fp_s3/spid3r-self${EXE_SUF}"
    H2="$(sha256_hex "$WORK_DIR/fp_s2/spid3r_main.o")"
    H3="$(sha256_hex "$WORK_DIR/fp_s3/spid3r_main.o")"
    echo "[pgo_build] Stage-2 spid3r_main.o: $H2"
    echo "[pgo_build] Stage-3 spid3r_main.o: $H3"
    if [ "$H2" != "$H3" ]; then
      echo "FAIL: Stage-2 and Stage-3 spid3r_main.o differ!" >&2
      exit 1
    fi
    S2_BYTES="$(wc -c < "$WORK_DIR/fp_s2/spid3r-self${EXE_SUF}" | tr -d '[:space:]')"
    echo "[pgo_build] Fixed point VERIFIED: Stage 2 (${S2_BYTES} B) <-> Stage 3 bit-identical"
  fi
else
  echo "[pgo_build] 5/5 Skipped Stage-1 emit (pass --build-selfhost <path> or --verify-fixed-point)"
fi

echo "[pgo_build] COMPLETE"
