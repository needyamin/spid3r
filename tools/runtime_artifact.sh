#!/usr/bin/env bash
# Formalize and verify Spid3r's prebuilt runtime staticlib artifact
# (roadmap Phase 9.1, docs/selfhost/SELFHOST.md SH-6, docs/selfhost/RUNTIME_ABI.md).
#
# Usage:
#   bash tools/runtime_artifact.sh package <staticlib_path> <out_dir> [options]
#     --profile <debug|release>     Build profile label (auto-inferred if omitted)
#     --target <triple>             Target triple (default: rustc host triple)
#     --llvm-version <ver>          LLVM version override
#     --version <ver>               Crate version override (default: Cargo.toml)
#     --no-tar                      Skip .tar.gz archive creation
#
#   bash tools/runtime_artifact.sh verify <dir_or_manifest>
#     Verify runtime-manifest.json, provenance fields, .sha256 sidecar,
#     and actual SHA-256 checksum of the staticlib (fail-closed).
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

sha256_file() {
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

json_escape() {
  printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/\r//g' -e ':a;N;$!ba;s/\n/\\n/g'
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
  echo "$(uname -m)-$(uname -s | tr '[:upper:]' '[:lower:]')"
}

detect_llvm_version() {
  if [ -n "${SPID3R_LLVM_VERSION:-}" ]; then
    echo "$SPID3R_LLVM_VERSION"
    return 0
  fi
  for cfg in llvm-config-23 llvm-config "C:/LLVM-23/bin/llvm-config.exe" "C:/Program Files/LLVM/bin/llvm-config.exe"; do
    if command -v "$cfg" >/dev/null 2>&1 || [ -x "$cfg" ]; then
      local v
      v=$("$cfg" --version 2>/dev/null | tr -d '\r' | head -n 1 || true)
      if [ -n "$v" ]; then
        echo "$v"
        return 0
      fi
    fi
  done
  for cmake in "C:/LLVM-23/lib/cmake/llvm/LLVMConfig.cmake" "C:/Program Files/LLVM/lib/cmake/llvm/LLVMConfig.cmake"; do
    if [ -f "$cmake" ]; then
      local v
      v=$(sed -n 's/^set(LLVM_PACKAGE_VERSION \([^)]*\)).*/\1/p' "$cmake" | head -n 1 | tr -d '\r')
      if [ -n "$v" ]; then
        echo "$v"
        return 0
      fi
    fi
  done
  echo "23.1 (llvm-config absent; crate pins llvm23-1)"
}

count_rt_trampolines() {
  local n
  n=$(grep -c 'pub extern "C" fn spid3r_' "$ROOT/src/native/rt.rs" || true)
  echo "${n:-0}"
}

cmd_package() {
  if [ "$#" -lt 2 ]; then
    echo "usage: runtime_artifact.sh package <staticlib_path> <out_dir> [--profile P] [--target T] [--llvm-version V] [--version V] [--no-tar]" >&2
    exit 2
  fi
  local lib_path="$1"
  local out_dir="$2"
  shift 2

  local profile=""
  local target=""
  local llvm_ver=""
  local version=""
  local make_tar=1

  while [ "$#" -gt 0 ]; do
    case "$1" in
      --profile)
        profile="${2:?--profile requires a value}"; shift 2 ;;
      --target)
        target="${2:?--target requires a value}"; shift 2 ;;
      --llvm-version)
        llvm_ver="${2:?--llvm-version requires a value}"; shift 2 ;;
      --version)
        version="${2:?--version requires a value}"; shift 2 ;;
      --no-tar)
        make_tar=0; shift ;;
      *)
        echo "error[usage]: unknown option '$1'" >&2
        exit 2 ;;
    esac
  done

  if [ ! -f "$lib_path" ]; then
    echo "error[usage]: runtime staticlib not found: $lib_path" >&2
    exit 2
  fi

  if [ -z "$profile" ]; then
    case "$lib_path" in
      */release/*|*\release\*) profile="release" ;;
      */debug/*|*\debug\*)     profile="debug" ;;
      *)                       profile="unknown" ;;
    esac
  fi
  [ -n "$version" ] || version="$(read_crate_version)"
  [ -n "$target" ] || target="$(detect_target_triple)"
  [ -n "$llvm_ver" ] || llvm_ver="$(detect_llvm_version)"

  local rustc_ver="unknown"
  if command -v rustc >/dev/null 2>&1; then
    rustc_ver=$(rustc --version 2>/dev/null | tr -d '\r' | head -n 1 || echo "unknown")
  fi

  local git_commit="unknown"
  local git_dirty="false"
  if git -C "$ROOT" rev-parse HEAD >/dev/null 2>&1; then
    git_commit=$(git -C "$ROOT" rev-parse HEAD 2>/dev/null | tr -d '\r')
    if [ -n "$(git -C "$ROOT" status --porcelain 2>/dev/null)" ]; then
      git_dirty="true"
    fi
  fi

  local lib_name
  lib_name="$(basename "$lib_path")"
  local artifact_tar="spid3r-runtime-${version}-${target}.tar.gz"

  mkdir -p "$out_dir"
  local dest_lib="$out_dir/$lib_name"
  local lib_abs dest_abs
  lib_abs="$(cd "$(dirname "$lib_path")" && pwd)/$(basename "$lib_path")"
  dest_abs="$(cd "$out_dir" && pwd)/$lib_name"
  if [ "$lib_abs" != "$dest_abs" ]; then
    cp -f "$lib_path" "$dest_lib"
  fi

  local lib_sha
  lib_sha="$(sha256_file "$dest_lib")"
  local size_bytes
  size_bytes="$(wc -c < "$dest_lib" | tr -d '[:space:]')"
  local tramp_count
  tramp_count="$(count_rt_trampolines)"
  local created_at
  created_at="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown)"

  printf "%s  %s\n" "$lib_sha" "$lib_name" > "$out_dir/${lib_name}.sha256"

  local manifest="$out_dir/runtime-manifest.json"
  cat > "$manifest" <<EOF
{
  "schema": 1,
  "package": "spid3r",
  "version": "$(json_escape "$version")",
  "language_version": "1.1.0",
  "abi_version": 1,
  "target": "$(json_escape "$target")",
  "profile": "$(json_escape "$profile")",
  "artifact_name": "$(json_escape "$artifact_tar")",
  "staticlib_name": "$(json_escape "$lib_name")",
  "staticlib_sha256": "$lib_sha",
  "size_bytes": $size_bytes,
  "provenance": {
    "git_commit": "$(json_escape "$git_commit")",
    "git_dirty": $git_dirty,
    "llvm_version": "$(json_escape "$llvm_ver")",
    "rustc_version": "$(json_escape "$rustc_ver")"
  },
  "interface": {
    "trampolines": $tramp_count,
    "stdlib_natives": 257
  },
  "created_at_utc": "$(json_escape "$created_at")"
}
EOF
  cp -f "$manifest" "$out_dir/${lib_name}.manifest.json"
  if [ -f "$ROOT/docs/selfhost/RUNTIME_ABI.md" ]; then
    cp -f "$ROOT/docs/selfhost/RUNTIME_ABI.md" "$out_dir/RUNTIME_ABI.md"
  fi

  if [ "$make_tar" -eq 1 ]; then
    local files=("$lib_name" "${lib_name}.sha256" "runtime-manifest.json" "${lib_name}.manifest.json")
    if [ -f "$out_dir/RUNTIME_ABI.md" ]; then
      files+=("RUNTIME_ABI.md")
    fi
    if tar --version 2>/dev/null | grep -qi "GNU tar"; then
      tar --sort=name --mtime="@0" --owner=0 --group=0 --numeric-owner \
        -czf "$out_dir/$artifact_tar" -C "$out_dir" "${files[@]}"
    else
      tar -czf "$out_dir/$artifact_tar" -C "$out_dir" "${files[@]}"
    fi
    local tar_sha
    tar_sha="$(sha256_file "$out_dir/$artifact_tar")"
    printf "%s  %s\n" "$tar_sha" "$artifact_tar" > "$out_dir/${artifact_tar}.sha256"
    echo "packaged runtime artifact: $out_dir/$artifact_tar (sha256=$tar_sha, staticlib_sha256=$lib_sha)"
  else
    echo "pinned runtime manifest: $manifest (staticlib_sha256=$lib_sha)"
  fi
}

cmd_verify() {
  if [ "$#" -ne 1 ]; then
    echo "usage: runtime_artifact.sh verify <dir_or_manifest>" >&2
    exit 2
  fi
  local target_arg="$1"
  local manifest=""
  if [ -d "$target_arg" ]; then
    manifest="$target_arg/runtime-manifest.json"
  else
    manifest="$target_arg"
  fi
  if [ ! -f "$manifest" ]; then
    echo "error[corrupt]: runtime manifest not found: $manifest" >&2
    exit 4
  fi
  local mdir
  mdir="$(cd "$(dirname "$manifest")" && pwd)"

  local schema lib_name expected_sha git_commit llvm_ver
  schema=$(sed -n 's/.*"schema":[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$manifest" | head -n 1)
  lib_name=$(sed -n 's/.*"staticlib_name":[[:space:]]*"\([^"]*\)".*/\1/p' "$manifest" | head -n 1)
  expected_sha=$(sed -n 's/.*"staticlib_sha256":[[:space:]]*"\([0-9a-fA-F]\{64\}\)".*/\1/p' "$manifest" | head -n 1 | tr '[:upper:]' '[:lower:]')
  git_commit=$(sed -n 's/.*"git_commit":[[:space:]]*"\([^"]*\)".*/\1/p' "$manifest" | head -n 1)
  llvm_ver=$(sed -n 's/.*"llvm_version":[[:space:]]*"\([^"]*\)".*/\1/p' "$manifest" | head -n 1)

  if [ "$schema" != "1" ] || [ -z "$lib_name" ] || [ "${#expected_sha}" -ne 64 ]; then
    echo "error[corrupt]: malformed runtime manifest '$manifest'" >&2
    exit 4
  fi
  if [ -z "$git_commit" ] || [ -z "$llvm_ver" ]; then
    echo "error[corrupt]: runtime manifest missing provenance (git_commit / llvm_version)" >&2
    exit 4
  fi
  case "$lib_name" in
    */*|*\\*|*..*)
      echo "error[corrupt]: unsafe staticlib_name '$lib_name' in manifest" >&2
      exit 4 ;;
  esac

  local lib_file="$mdir/$lib_name"
  if [ ! -f "$lib_file" ]; then
    echo "error[usage]: pinned runtime staticlib missing: $lib_file" >&2
    exit 2
  fi

  if [ -f "$mdir/${lib_name}.sha256" ]; then
    local side_sha
    side_sha=$(awk 'NR==1 {print tolower($1)}' "$mdir/${lib_name}.sha256")
    if [ "$side_sha" != "$expected_sha" ]; then
      echo "error[corrupt]: sidecar ${lib_name}.sha256 ($side_sha) disagrees with manifest ($expected_sha)" >&2
      exit 4
    fi
  fi

  local actual_sha
  actual_sha="$(sha256_file "$lib_file")"
  if [ "$actual_sha" != "$expected_sha" ]; then
    echo "error[corrupt]: runtime staticlib integrity check failed for '$lib_file': expected $expected_sha, got $actual_sha" >&2
    exit 4
  fi

  echo "verified runtime staticlib: $lib_file (sha256=$actual_sha, commit=$git_commit, llvm=$llvm_ver)"
}

case "${1:-}" in
  package)
    shift
    cmd_package "$@" ;;
  verify)
    shift
    cmd_verify "$@" ;;
  *)
    echo "usage: runtime_artifact.sh <package|verify> ..." >&2
    exit 2 ;;
esac
