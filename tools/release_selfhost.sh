#!/usr/bin/env bash
# End-to-end self-hosted release build & verification script (roadmap Phase 10.1, SH-2).
#
# Uses `spid3r-self` (Stage-1/Stage-2 self-hosted compiler) rather than Cargo
# to produce:
#   - Stage-2 `spid3r-self` binary + Stage-3 COFF/ELF/Mach-O fixed-point proof
#   - Versioned prebuilt runtime staticlib tarball + manifest + .sha256
#   - Platform release tarball (`spid3r-<target>.tar.gz` + `.sha256`) containing
#     `spid3r-self` (plus retained `spid3r` VM/interpreter/tools if supplied)
#   - Windows NSIS installer (`Spid3r-Setup-<version>.exe` + `.sha256`) bundling
#     Stage-2 `spid3r-self.exe` + pinned runtime + LLVM-C + retained tools
#   - Combined `sha256sums.txt` verified via sha256sum/shasum
#   - Smoke tests on the released compiler binary + archive extraction
#
# Usage:
#   bash tools/release_selfhost.sh build [options]
#     --out-dir <dir>         Staging output directory (default: dist/release-<ver>)
#     --stage1 <path>         Stage-1 spid3r-self executable (built via --stage0 if omitted)
#     --stage0 <path>         Stage-0 spid3r binary (for retained VM/CLI tools + optional Stage-1 bootstrap)
#     --control <path>        Optional spid3r-control binary (Windows GUI panel)
#     --staticlib <path>      Prebuilt runtime staticlib (auto-detected if omitted)
#     --target <triple>       Target triple (default: host triple)
#     --llvm-version <ver>    LLVM version override
#     --pgo                   Build ThinLTO + split-PGO Stage 0, runtime, and Stage 1 via tools/pgo_build.sh
#     --skip-installer        Do not build Windows NSIS installer
#     --smoke-install         On Windows (CI / disposable host), run silent NSIS /S /D= test
#
#   bash tools/release_selfhost.sh verify <out_dir>
#     Fail-closed verification of a staged release directory (`sha256sums.txt`,
#     runtime bundle, platform tarball with `spid3r-self`, installer payload,
#     and tarball smoke tests).
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

sha256_hex() {
  local f="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$f" | awk '{print tolower($1)}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$f" | awk '{print tolower($1)}'
  else
    echo "error[usage]: neither sha256sum nor shasum found" >&2
    exit 2
  fi
}

write_sha256_file() {
  local dir="$1"
  local file="$2"
  (
    cd "$dir"
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum "$file" > "${file}.sha256"
    else
      shasum -a 256 "$file" > "${file}.sha256"
    fi
  )
}

verify_sha256sums() {
  local dir="$1"
  (
    cd "$dir"
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum -c sha256sums.txt
    else
      shasum -a 256 -c sha256sums.txt
    fi
  )
}

to_abs_path() {
  local p="$1"
  local dir base
  dir="$(dirname "$p")"
  base="$(basename "$p")"
  if [ -d "$dir" ]; then
    echo "$(cd "$dir" && pwd)/$base"
  else
    case "$p" in
      /*|[A-Za-z]:*) echo "$p" ;;
      *) echo "$ROOT/$p" ;;
    esac
  fi
}

to_win_path() {
  local p
  p="$(to_abs_path "$1")"
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$p"
  else
    echo "$p"
  fi
}

read_crate_version() {
  awk -F'"' '/^version *= *"/ { print $2; exit }' "$ROOT/Cargo.toml"
}

detect_target_triple() {
  if command -v rustc >/dev/null 2>&1; then
    local h
    h=$(rustc -vV 2>/dev/null | awk '/^host:/ { print $2; exit }')
    if [ -n "$h" ]; then
      echo "$h"
      return 0
    fi
  fi
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*|Windows_NT) echo "x86_64-pc-windows-msvc" ;;
    Darwin)
      if [ "$(uname -m)" = "arm64" ] || [ "$(uname -m)" = "aarch64" ]; then
        echo "aarch64-apple-darwin"
      else
        echo "x86_64-apple-darwin"
      fi
      ;;
    Linux)
      echo "$(uname -m)-unknown-linux-gnu"
      ;;
    *)
      echo "$(uname -m)-$(uname -s | tr '[:upper:]' '[:lower:]')"
      ;;
  esac
}

find_default_staticlib() {
  for c in \
    target/release/spid3r_lang.lib \
    target/release/libspid3r_lang.a \
    target/debug/spid3r_lang.lib \
    target/debug/libspid3r_lang.a; do
    if [ -f "$c" ]; then
      echo "$c"
      return 0
    fi
  done
  return 1
}

find_default_stage0() {
  for c in \
    target/release/spid3r.exe \
    target/release/spid3r \
    target/debug/spid3r.exe \
    target/debug/spid3r; do
    if [ -x "$c" ]; then
      echo "$c"
      return 0
    fi
  done
  return 1
}

find_makensis() {
  for c in \
    "$ROOT/packaging/nsis/nsis/nsis-3.10/makensis.exe" \
    "$ROOT/packaging/nsis/nsis/makensis.exe" \
    "/c/Program Files (x86)/NSIS/makensis.exe" \
    "/c/Program Files/NSIS/makensis.exe"; do
    if [ -f "$c" ]; then
      echo "$c"
      return 0
    fi
  done
  if command -v makensis.exe >/dev/null 2>&1; then
    command -v makensis.exe
    return 0
  fi
  if command -v makensis >/dev/null 2>&1; then
    command -v makensis
    return 0
  fi
  return 1
}

verify_formatter_vsix() {
  local vsix="$ROOT/packaging/vscode/spid3r-formatter.vsix"
  local sha="$ROOT/packaging/vscode/spid3r-formatter.vsix.sha256"
  if [ ! -f "$vsix" ] && [ ! -f "$sha" ]; then
    return 1
  fi
  if [ ! -f "$vsix" ] || [ ! -f "$sha" ]; then
    echo "FAIL: partial formatter VSIX pin (need both spid3r-formatter.vsix and .sha256)" >&2
    exit 1
  fi
  local expected actual
  expected="$(awk '{print tolower($1); exit}' "$sha")"
  if ! printf '%s' "$expected" | grep -qE '^[0-9a-f]{64}$'; then
    echo "FAIL: invalid digest in $sha" >&2
    exit 1
  fi
  actual="$(sha256_hex "$vsix")"
  if [ "$actual" != "$expected" ]; then
    echo "FAIL: spid3r-formatter.vsix sha256 mismatch (expected $expected, got $actual)" >&2
    exit 1
  fi
  return 0
}

run_smoke_cmd() {
  local tmp_dir="$1"
  local label="$2"
  local desc="$3"
  shift 3
  local log="$tmp_dir/smoke.log"
  local rc=0
  "$@" </dev/null >"$log" 2>&1 || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "FAIL: $label $desc failed (rc=$rc):" >&2
    head -n 40 "$log" | sed 's/^/  /' >&2
    rm -rf "$tmp_dir"
    exit 1
  fi
}

smoke_test_compiler_binary() {
  local bin="$1"
  local rt_dir="$2"
  local label="$3"

  echo "[release_selfhost] smoke-testing $label ($bin)"
  local tmp
  tmp="$(mktemp -d)"

  run_smoke_cmd "$tmp" "$label" "--version" "$bin" --version
  if ! grep -q "spid3r-self" "$tmp/smoke.log"; then
    echo "FAIL: $label --version missing 'spid3r-self'" >&2
    head -n 20 "$tmp/smoke.log" | sed 's/^/  /' >&2
    rm -rf "$tmp"
    exit 1
  fi
  run_smoke_cmd "$tmp" "$label" "--help" "$bin" --help
  run_smoke_cmd "$tmp" "$label" "verify-runtime" \
    env SPID3R_RUNTIME_REQUIRE_PIN=1 "$bin" verify-runtime "$rt_dir"
  run_smoke_cmd "$tmp" "$label" "check hello.matrix" \
    "$bin" check "$ROOT/examples/hello.matrix"
  run_smoke_cmd "$tmp" "$label" "fmt --check selfhost/main.matrix" \
    "$bin" fmt --check "$ROOT/selfhost/main.matrix"

  local exe_out="$tmp/hello_smoke"
  case "$TARGET" in
    *windows*) exe_out="$tmp/hello_smoke.exe" ;;
  esac

  run_smoke_cmd "$tmp" "$label" "build --release hello.matrix" \
    env SPID3R_RUNTIME_MANIFEST="$rt_dir/runtime-manifest.json" SPID3R_RUNTIME_REQUIRE_PIN=1 \
    "$bin" build --release "$ROOT/examples/hello.matrix" -o "$exe_out"
  [ -x "$exe_out" ] || [ -f "$exe_out" ] || {
    echo "FAIL: $label did not emit $exe_out" >&2
    rm -rf "$tmp"
    exit 1
  }
  local got
  got="$("$exe_out" < /dev/null | tr -d '\r')"
  if [ "$got" != "hello" ]; then
    echo "FAIL: $label built binary printed '$got', expected 'hello'" >&2
    rm -rf "$tmp"
    exit 1
  fi
  rm -rf "$tmp"
  echo "[release_selfhost] smoke OK: $label"
}

cmd_build() {
  local version
  version="$(read_crate_version)"
  local target=""
  local out_dir=""
  local stage1=""
  local stage0=""
  local control_bin=""
  local staticlib=""
  local llvm_ver=""
  local skip_installer=0
  local smoke_install=0
  local use_pgo=0

  while [ "$#" -gt 0 ]; do
    case "$1" in
      --out-dir)        out_dir="${2:?--out-dir requires a value}"; shift 2 ;;
      --stage1)         stage1="${2:?--stage1 requires a value}"; shift 2 ;;
      --stage0)         stage0="${2:?--stage0 requires a value}"; shift 2 ;;
      --control)        control_bin="${2:?--control requires a value}"; shift 2 ;;
      --staticlib)      staticlib="${2:?--staticlib requires a value}"; shift 2 ;;
      --target)         target="${2:?--target requires a value}"; shift 2 ;;
      --llvm-version)   llvm_ver="${2:?--llvm-version requires a value}"; shift 2 ;;
      --pgo)            use_pgo=1; shift ;;
      --skip-installer) skip_installer=1; shift ;;
      --smoke-install)  smoke_install=1; shift ;;
      *)
        echo "error[usage]: unknown option '$1'" >&2
        exit 2 ;;
    esac
  done

  [ -n "$target" ] || target="$(detect_target_triple)"
  TARGET="$target"
  [ -n "$out_dir" ] || out_dir="$ROOT/dist/release-${version}"

  local is_win=0
  local exe_suf=""
  case "$target" in
    *windows*)
      is_win=1
      exe_suf=".exe"
      ;;
  esac

  rm -rf "$out_dir"
  mkdir -p "$out_dir/runtime" "$out_dir/_work/s1" "$out_dir/_work/s2" "$out_dir/_work/s3" "$out_dir/_work/tar"

  if [ "$use_pgo" -eq 1 ]; then
    local lib_name="libspid3r_lang.a"
    [ "$is_win" -eq 1 ] && lib_name="spid3r_lang.lib"
    local pgo_dir="$out_dir/_work/pgo"
    echo "[release_selfhost] 0. Running LTO + split-PGO toolchain build (tools/pgo_build.sh)..."
    bash "$ROOT/tools/pgo_build.sh" \
      --out-dir "$pgo_dir" \
      --build-selfhost "$pgo_dir/spid3r-self${exe_suf}"
    stage0="$pgo_dir/spid3r${exe_suf}"
    staticlib="$pgo_dir/$lib_name"
    stage1="$pgo_dir/spid3r-self${exe_suf}"
  else
    [ -n "$staticlib" ] || staticlib="$(find_default_staticlib || true)"
    [ -n "$stage0" ] || stage0="$(find_default_stage0 || true)"
  fi

  if [ -z "$staticlib" ] || [ ! -f "$staticlib" ]; then
    echo "error[usage]: runtime staticlib not found; pass --staticlib" >&2
    exit 2
  fi

  if [ -n "$stage1" ] && [ ! -f "$stage1" ]; then
    echo "error[usage]: Stage-1 spid3r-self not found: $stage1" >&2
    exit 2
  fi

  if [ -z "$stage1" ]; then
    if [ -x "$ROOT/selfhost/main_native${exe_suf}" ]; then
      stage1="$ROOT/selfhost/main_native${exe_suf}"
    elif [ -n "$stage0" ] && [ -x "$stage0" ]; then
      stage1="$out_dir/_work/s1/spid3r-self${exe_suf}"
      echo "[release_selfhost] bootstrapping Stage 1 via Stage 0 ($stage0)"
      SPID3R_RUNTIME_LIB="$staticlib" "$stage0" build --release "$ROOT/selfhost/main.matrix" -o "$stage1"
    else
      echo "error[usage]: neither --stage1 nor --stage0 found to run release build" >&2
      exit 2
    fi
  fi

  if [ ! -f "$stage1" ]; then
    echo "error[usage]: Stage-1 spid3r-self not found: $stage1" >&2
    exit 2
  fi

  echo "[release_selfhost] 1. Packaging & verifying pinned runtime staticlib..."
  local rt_args=(bash "$ROOT/tools/runtime_artifact.sh" package "$staticlib" "$out_dir/runtime" --target "$target" --version "$version")
  if [ -n "$llvm_ver" ]; then
    rt_args+=(--llvm-version "$llvm_ver")
  fi
  "${rt_args[@]}"
  bash "$ROOT/tools/runtime_artifact.sh" verify "$out_dir/runtime"

  local rt_manifest="$out_dir/runtime/runtime-manifest.json"
  local rt_lib_name
  rt_lib_name="$(basename "$staticlib")"
  local rt_lib_copy="$out_dir/runtime/$rt_lib_name"
  local rt_sha_copy="$out_dir/runtime/${rt_lib_name}.sha256"

  # Copy versioned runtime tarball + sha256 into release output root
  cp -f "$out_dir/runtime"/spid3r-runtime-*.tar.gz "$out_dir/"
  cp -f "$out_dir/runtime"/spid3r-runtime-*.tar.gz.sha256 "$out_dir/"

  echo "[release_selfhost] 2. Compiling Stage-2 release compiler BY spid3r-self (pinned runtime)..."
  local stage2_bin="$out_dir/_work/s2/spid3r-self${exe_suf}"
  local s2_args=("$stage1" build --release)
  if [ -n "$target" ]; then
    s2_args+=(--target "$target")
  fi
  s2_args+=("$ROOT/selfhost/main.matrix" -o "$stage2_bin")
  SPID3R_RUNTIME_MANIFEST="$rt_manifest" SPID3R_RUNTIME_REQUIRE_PIN=1 "${s2_args[@]}"

  echo "[release_selfhost] 3. Proving Stage-2 -> Stage-3 fixed point..."
  local stage3_bin="$out_dir/_work/s3/spid3r-self${exe_suf}"
  local s3_args=("$stage2_bin" build --release)
  if [ -n "$target" ]; then
    s3_args+=(--target "$target")
  fi
  s3_args+=("$ROOT/selfhost/main.matrix" -o "$stage3_bin")
  SPID3R_RUNTIME_MANIFEST="$rt_manifest" SPID3R_RUNTIME_REQUIRE_PIN=1 "${s3_args[@]}"

  local obj2="$out_dir/_work/s2/spid3r_main.o"
  local obj3="$out_dir/_work/s3/spid3r_main.o"
  if [ ! -f "$obj2" ] || [ ! -f "$obj3" ]; then
    echo "FAIL: expected spid3r_main.o missing ($obj2 / $obj3)" >&2
    exit 1
  fi
  local h2 h3
  h2="$(sha256_hex "$obj2")"
  h3="$(sha256_hex "$obj3")"
  echo "  Stage-2 spid3r_main.o: $h2"
  echo "  Stage-3 spid3r_main.o: $h3"
  if [ "$h2" != "$h3" ]; then
    echo "FAIL: Stage-2 and Stage-3 spid3r_main.o differ!" >&2
    exit 1
  fi

  smoke_test_compiler_binary "$stage2_bin" "$out_dir/runtime" "Stage-2 spid3r-self"

  echo "[release_selfhost] 4. Packaging release tarball (spid3r-${target}.tar.gz)..."
  cp -f "$stage2_bin" "$out_dir/_work/tar/spid3r-self${exe_suf}"
  local tar_members=("spid3r-self${exe_suf}")
  if [ -n "$stage0" ] && [ -x "$stage0" ]; then
    cp -f "$stage0" "$out_dir/_work/tar/spid3r${exe_suf}"
    tar_members+=("spid3r${exe_suf}")
    if [ "$is_win" -eq 0 ]; then
      for alias_bin in sp yamc yamcvm spvm spkg; do
        ln -sf spid3r "$out_dir/_work/tar/${alias_bin}"
        tar_members+=("${alias_bin}")
      done
    fi
  fi
  # Bundle libLLVM shared library on Linux so the prebuilt tarball runs on
  # clean Linux hosts without system libllvm23 installed ($ORIGIN:$ORIGIN/../lib).
  case "$target" in
    *linux*)
      local llvm_so=""
      if [ -n "$stage0" ] && [ -x "$stage0" ] && command -v ldd >/dev/null 2>&1; then
        llvm_so="$(ldd "$stage0" 2>/dev/null | awk '/libLLVM/ && /=>/ { print $3; exit }')"
      fi
      if [ -z "$llvm_so" ] && command -v ldd >/dev/null 2>&1; then
        llvm_so="$(ldd "$stage2_bin" 2>/dev/null | awk '/libLLVM/ && /=>/ { print $3; exit }')"
      fi
      if [ -n "$llvm_so" ] && [ -f "$llvm_so" ]; then
        local llvm_soname
        llvm_soname="$(basename "$llvm_so")"
        if command -v readelf >/dev/null 2>&1; then
          local son
          son="$(readelf -d "$llvm_so" 2>/dev/null | awk -F'[][]' '/SONAME/ { print $2; exit }')"
          if [ -n "$son" ]; then
            llvm_soname="$son"
          fi
        fi
        mkdir -p "$out_dir/_work/tar/lib"
        cp -L -f "$llvm_so" "$out_dir/_work/tar/lib/$llvm_soname"
        # Also bundle non-base direct & transitive shared libs of spid3r,
        # spid3r-self, and libLLVM.so.* (e.g. libz3, libxml2, libedit, libffi)
        # so clean minimal Ubuntu 24.04 containers without apt packages can
        # resolve every dependency via DT_RPATH.
        if command -v ldd >/dev/null 2>&1; then
          local dep_queue=("$out_dir/_work/tar/lib/$llvm_soname")
          if [ -n "$stage0" ] && [ -x "$stage0" ]; then
            dep_queue+=("$stage0")
          fi
          if [ -x "$stage2_bin" ]; then
            dep_queue+=("$stage2_bin")
          fi
          local q_idx=0
          while [ "$q_idx" -lt "${#dep_queue[@]}" ]; do
            local cur_so="${dep_queue[$q_idx]}"
            q_idx=$((q_idx + 1))
            while IFS=$'\t' read -r needed_name dep_path; do
              [ -n "$dep_path" ] && [ -f "$dep_path" ] || continue
              local dep_name="${needed_name:-$(basename "$dep_path")}"
              if command -v readelf >/dev/null 2>&1; then
                local dson
                dson="$(readelf -d "$dep_path" 2>/dev/null | awk -F'[][]' '/SONAME/ { print $2; exit }')"
                if [ -n "$dson" ]; then
                  dep_name="$dson"
                fi
              fi
              case "$dep_name" in
                linux-vdso*|ld-linux*|libc.so*|libm.so*|libdl.so*|librt.so*|libpthread.so*|libresolv.so*|libgcc_s.so*|libstdc++.so*|libz.so*|libtinfo.so*|libzstd.so*)
                  continue
                  ;;
              esac
              if [ ! -f "$out_dir/_work/tar/lib/$dep_name" ]; then
                cp -L -f "$dep_path" "$out_dir/_work/tar/lib/$dep_name"
                dep_queue+=("$out_dir/_work/tar/lib/$dep_name")
                echo "  Bundled Linux transitive shared library: lib/$dep_name"
              fi
            done < <(ldd "$cur_so" 2>/dev/null | awk '/=>/ && $3 ~ /^\// { printf "%s\t%s\n", $1, $3 }')
          done
        fi
        tar_members+=("lib")
        echo "  Bundled Linux LLVM shared library: lib/$llvm_soname"
      fi
      ;;
  esac
  local has_vsix=0
  if verify_formatter_vsix; then
    has_vsix=1
    cp -f "$ROOT/packaging/vscode/spid3r-formatter.vsix" "$out_dir/_work/tar/spid3r-formatter.vsix"
    cp -f "$ROOT/packaging/vscode/spid3r-formatter.vsix.sha256" "$out_dir/_work/tar/spid3r-formatter.vsix.sha256"
    tar_members+=("spid3r-formatter.vsix" "spid3r-formatter.vsix.sha256")
  fi
  local tarball_name="spid3r-${target}.tar.gz"
  tar czf "$out_dir/$tarball_name" -C "$out_dir/_work/tar" "${tar_members[@]}"
  write_sha256_file "$out_dir" "$tarball_name"

  if [ "$is_win" -eq 1 ] && [ "$skip_installer" -eq 0 ]; then
    echo "[release_selfhost] 5. Building Windows NSIS installer via makensis (spid3r-self payload)..."
    local makensis
    makensis="$(find_makensis || true)"
    if [ -z "$makensis" ]; then
      echo "error[usage]: makensis.exe not found; pass --skip-installer or install NSIS" >&2
      exit 2
    fi
    local llvm_dll="" llvm_lib=""
    for p in "C:/LLVM-23" "C:/Program Files/LLVM" "/c/LLVM-23" "/c/Program Files/LLVM"; do
      if [ -f "$p/bin/LLVM-C.dll" ] && [ -f "$p/lib/LLVM-C.lib" ]; then
        llvm_dll="$p/bin/LLVM-C.dll"
        llvm_lib="$p/lib/LLVM-C.lib"
        break
      fi
    done
    if [ -z "$llvm_dll" ] || [ -z "$llvm_lib" ]; then
      echo "error[usage]: LLVM-C.dll / LLVM-C.lib not found under C:/LLVM-23 or C:/Program Files/LLVM" >&2
      exit 2
    fi

    if [ -z "$stage0" ] || [ ! -x "$stage0" ]; then
      echo "error[usage]: retained spid3r.exe (--stage0) required for NSIS installer payload" >&2
      exit 2
    fi

    local inst_name="Spid3r-Setup-${version}.exe"
    local inst_out="$out_dir/$inst_name"
    local nsis_args=(
      /V2
      "/DAPPVERSION=${version}"
      "/DOUTFILE=$(to_win_path "$inst_out")"
      /DWITH_NATIVE=1
      "/DSPID3R_EXE=$(to_win_path "$stage0")"
      "/DSPID3R_SELF_EXE=$(to_win_path "$stage2_bin")"
      "/DRUNTIME_LIB=$(to_win_path "$rt_lib_copy")"
      "/DRUNTIME_SHA256=$(to_win_path "$rt_sha_copy")"
      "/DRUNTIME_MANIFEST=$(to_win_path "$rt_manifest")"
      "/DLLVM_DLL=$(to_win_path "$llvm_dll")"
      "/DLLVM_LIB=$(to_win_path "$llvm_lib")"
    )
    if [ -z "$control_bin" ] && [ -f "$(dirname "$stage0")/spid3r-control.exe" ]; then
      control_bin="$(dirname "$stage0")/spid3r-control.exe"
    fi
    local with_gui=0
    if [ -n "$control_bin" ] && [ -f "$control_bin" ]; then
      with_gui=1
      nsis_args+=("/DCONTROL_EXE=$(to_win_path "$control_bin")")
    fi
    nsis_args+=("/DWITH_GUI=${with_gui}")
    if [ "$has_vsix" -eq 1 ]; then
      nsis_args+=("/DFORMATTER_VSIX=$(to_win_path "$ROOT/packaging/vscode/spid3r-formatter.vsix")")
    else
      nsis_args+=("/DFORMATTER_VSIX=")
    fi
    nsis_args+=("$(to_win_path "$ROOT/packaging/nsis/spid3r.nsi")")

    MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*' "$makensis" "${nsis_args[@]}"
    [ -s "$inst_out" ] || {
      echo "FAIL: installer not created at $inst_out" >&2
      exit 1
    }
    local isize
    isize="$(wc -c < "$inst_out")"
    echo "  Installer: $inst_name (${isize} bytes)"
    if [ "$isize" -lt 15000000 ]; then
      echo "FAIL: installer smaller than 15 MB ($isize bytes) — LLVM payload missing!" >&2
      exit 1
    fi
    write_sha256_file "$out_dir" "$inst_name"

    if [ "$smoke_install" -eq 1 ]; then
      echo "[release_selfhost] Running silent NSIS install smoke test..."
      local sin_dir="$out_dir/_work/installed"
      mkdir -p "$sin_dir"
      local win_sin
      win_sin="$(to_win_path "$sin_dir")"
      MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*' "$inst_out" /S /NOEDITOR "/D=${win_sin}"
      for req in spid3r-self.exe spid3r.exe sp.exe yamc.exe yamcvm.exe spvm.exe spkg.exe spid3r_lang.lib spid3r_lang.lib.sha256 runtime-manifest.json LLVM-C.dll LLVM-C.lib; do
        [ -f "$sin_dir/$req" ] || {
          echo "FAIL: installed file missing: $sin_dir/$req" >&2
          exit 1
        }
      done
      # Verify multicall argv[0] persona dispatch and VM round-trip on installed copies.
      for comp in yamc yamcvm spvm spkg; do
        local comp_ver
        comp_ver="$("$sin_dir/${comp}.exe" --version)"
        case "$comp_ver" in
          *"$comp"*) ;;
          *)
            echo "FAIL: installed ${comp}.exe --version unexpected: $comp_ver" >&2
            exit 1
            ;;
        esac
      done
      local cli_smoke_src="$out_dir/_work/cli_smoke.mt"
      local cli_smoke_bc="$out_dir/_work/cli_smoke.matrixc"
      printf 'fn main() -> int { println("nsis-cli-ok"); return 0; }\n' > "$cli_smoke_src"
      "$sin_dir/yamcvm.exe" "$cli_smoke_src" -o "$cli_smoke_bc" >/dev/null
      local cli_smoke_out
      cli_smoke_out="$("$sin_dir/spvm.exe" "$cli_smoke_bc" | tr -d '\r')"
      if [ "$cli_smoke_out" != "nsis-cli-ok" ]; then
        echo "FAIL: installed yamcvm/spvm smoke expected 'nsis-cli-ok', got '$cli_smoke_out'" >&2
        exit 1
      fi
      if [ "$has_vsix" -eq 1 ]; then
        [ -f "$sin_dir/editor/spid3r-formatter.vsix" ] || {
          echo "FAIL: installed offline formatter VSIX missing: $sin_dir/editor/spid3r-formatter.vsix" >&2
          exit 1
        }
      fi
      smoke_test_compiler_binary "$sin_dir/spid3r-self.exe" "$sin_dir" "installed spid3r-self.exe"
      if [ -f "$sin_dir/uninstall.exe" ]; then
        MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*' "$sin_dir/uninstall.exe" /S _?="${win_sin}" || true
      fi
    fi
  fi

  echo "[release_selfhost] 6. Generating & verifying sha256sums.txt..."
  (
    cd "$out_dir"
    cat *.sha256 > sha256sums.txt
  )
  verify_sha256sums "$out_dir"

  # Retain clean release deliverables while dropping scratch _work
  rm -rf "$out_dir/_work"

  cmd_verify "$out_dir"
  echo "[release_selfhost] COMPLETE: $out_dir"
}

check_size_budget() {
  local label="$1"
  local file="$2"
  local max_bytes="$3"
  local sz
  sz="$(wc -c < "$file")"
  echo "  size budget: $label = ${sz} B (ceiling ${max_bytes} B)"
  if [ "$sz" -gt "$max_bytes" ]; then
    echo "FAIL: $label (${sz} B) exceeds P-5 size budget (${max_bytes} B)" >&2
    exit 1
  fi
}

cmd_verify() {
  local out_dir="${1:?usage: release_selfhost.sh verify <out_dir>}"
  [ -d "$out_dir" ] || {
    echo "FAIL: release dir not found: $out_dir" >&2
    exit 1
  }
  [ -f "$out_dir/sha256sums.txt" ] || {
    echo "FAIL: missing sha256sums.txt in $out_dir" >&2
    exit 1
  }

  echo "[release_selfhost] verifying checksums..."
  verify_sha256sums "$out_dir"

  echo "[release_selfhost] verifying runtime bundle..."
  bash "$ROOT/tools/runtime_artifact.sh" verify "$out_dir/runtime"

  local skip_budgets="${SPID3R_SKIP_SIZE_BUDGETS:-0}"
  if [ "$skip_budgets" -eq 0 ] && [ -f "$out_dir/runtime/runtime-manifest.json" ]; then
    if grep -qE '"profile"[[:space:]]*:[[:space:]]*"debug"' "$out_dir/runtime/runtime-manifest.json"; then
      skip_budgets=1
      echo "  size budget: skipped release ceilings (runtime profile=debug)"
    fi
  fi

  local found_tar=0
  local t
  for t in "$out_dir"/spid3r-*.tar.gz; do
    case "$t" in
      *spid3r-runtime-*) continue ;;
    esac
    [ -f "$t" ] || continue
    found_tar=1
    local listing
    listing="$(tar tzf "$t")"
    if ! printf '%s\n' "$listing" | grep -qE '^(\./)?spid3r-self(\.exe)?$'; then
      echo "FAIL: $t does not contain spid3r-self" >&2
      exit 1
    fi
    local expect_vsix=0
    if verify_formatter_vsix; then
      expect_vsix=1
      if ! printf '%s\n' "$listing" | grep -qE '^(\./)?spid3r-formatter\.vsix$' \
        || ! printf '%s\n' "$listing" | grep -qE '^(\./)?spid3r-formatter\.vsix\.sha256$'; then
        echo "FAIL: $t does not contain spid3r-formatter.vsix (+ .sha256)" >&2
        exit 1
      fi
    fi
    local ext_tmp
    ext_tmp="$(mktemp -d)"
    tar xzf "$t" -C "$ext_tmp"
    if [ "$expect_vsix" -eq 1 ]; then
      local vsix_exp vsix_act vsix_repo
      vsix_exp="$(awk '{print tolower($1); exit}' "$ext_tmp/spid3r-formatter.vsix.sha256")"
      vsix_repo="$(awk '{print tolower($1); exit}' "$ROOT/packaging/vscode/spid3r-formatter.vsix.sha256")"
      vsix_act="$(sha256_hex "$ext_tmp/spid3r-formatter.vsix")"
      if [ "$vsix_act" != "$vsix_exp" ] || [ "$vsix_act" != "$vsix_repo" ]; then
        echo "FAIL: unpacked $base_tar spid3r-formatter.vsix checksum mismatch" >&2
        rm -rf "$ext_tmp"
        exit 1
      fi
    fi
    local self_bin=""
    if [ -f "$ext_tmp/spid3r-self.exe" ]; then
      self_bin="$ext_tmp/spid3r-self.exe"
    elif [ -f "$ext_tmp/spid3r-self" ]; then
      self_bin="$ext_tmp/spid3r-self"
    fi
    if [ -z "$self_bin" ]; then
      echo "FAIL: unpacked tarball missing spid3r-self" >&2
      rm -rf "$ext_tmp"
      exit 1
    fi
    chmod +x "$self_bin"
    local base_tar
    base_tar="$(basename "$t")"
    TARGET="${base_tar#spid3r-}"
    TARGET="${TARGET%.tar.gz}"
    smoke_test_compiler_binary "$self_bin" "$out_dir/runtime" "unpacked $base_tar"
    if [ "$skip_budgets" -eq 0 ]; then
      check_size_budget "spid3r-self ($base_tar)" "$self_bin" "${SPID3R_MAX_SELF_BYTES:-7500000}"
      local max_tar="${SPID3R_MAX_TAR_BYTES:-10000000}"
      if [ -d "$ext_tmp/lib" ] && [ -z "${SPID3R_MAX_TAR_BYTES:-}" ]; then
        # Linux tarballs bundle libLLVM.so.23.1 and its non-base transitive
        # deps (e.g. libz3, libxml2/libicu) so clean hosts need no system libllvm23.
        max_tar=95000000
      fi
      check_size_budget "$base_tar" "$t" "$max_tar"
    fi
    rm -rf "$ext_tmp"
  done

  if [ "$found_tar" -ne 1 ]; then
    echo "FAIL: no platform spid3r-<target>.tar.gz found in $out_dir" >&2
    exit 1
  fi

  if [ "$skip_budgets" -eq 0 ]; then
    local rt_tar inst_exe
    for rt_tar in "$out_dir"/spid3r-runtime-*.tar.gz; do
      [ -f "$rt_tar" ] || continue
      check_size_budget "$(basename "$rt_tar")" "$rt_tar" "${SPID3R_MAX_RT_TAR_BYTES:-24000000}"
    done
    for inst_exe in "$out_dir"/Spid3r-Setup-*.exe; do
      [ -f "$inst_exe" ] || continue
      check_size_budget "$(basename "$inst_exe")" "$inst_exe" "${SPID3R_MAX_INSTALLER_BYTES:-48000000}"
    done
  fi

  echo "[release_selfhost] VERIFY PASS: $out_dir"
}

if [ "$#" -lt 1 ]; then
  echo "usage: release_selfhost.sh <build|verify> ..." >&2
  exit 2
fi

sub="$1"
shift
case "$sub" in
  build)  cmd_build "$@" ;;
  verify) cmd_verify "$@" ;;
  *)
    echo "error[usage]: unknown subcommand '$sub' (expected build|verify)" >&2
    exit 2 ;;
esac
