#!/usr/bin/env bash
# Self-host `test` runner parity (roadmap Phase 8, step 8.1, SH-3):
# compares `spid3r-self test` against Stage 0 (`spid3r test`) across:
#   1. repo `examples/` (`--list`, `--filter`, `-f`, full 25-file differential)
#   2. frozen-corpus slice copied into temp `examples/` (positives + failures)
#   3. `spider.toml` project + workspace member mode (`tests/*.matrix`)
#   4. CLI error cases (`--filter` missing arg, unknown flag, no examples/)
#
# Usage: bash tools/selfhost_test_parity.sh [selfhost-bin] [oracle-bin]
set -u
cd "$(dirname "$0")/.."

SH="${1:-selfhost/main_native}"
[ -x "$SH" ] || [ -f "$SH" ] || SH="${SH}.exe"
ORACLE="${2:-target/debug/spid3r}"
[ -x "$ORACLE" ] || [ -f "$ORACLE" ] || ORACLE="${ORACLE}.exe"

ABS_SH="$(cd "$(dirname "$SH")" && pwd)/$(basename "$SH")"
ABS_ORACLE="$(cd "$(dirname "$ORACLE")" && pwd)/$(basename "$ORACLE")"
export SPID3R_ORACLE="$ABS_ORACLE"
export SPID3R_RUNTIME_LIB="${SPID3R_RUNTIME_LIB:-$(pwd)/target/debug/spid3r_lang.lib}"

OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

pass=0
fail=0

check_case() {
  local label="$1"
  local workdir="$2"
  shift 2
  (cd "$workdir" && "$ABS_ORACLE" test "$@" < /dev/null > "$OUT/o.out" 2> "$OUT/o.err")
  local orc=$?
  (cd "$workdir" && "$ABS_SH" test "$@" < /dev/null > "$OUT/s.out" 2> "$OUT/s.err")
  local src=$?

  if [ "$orc" != "$src" ]; then
    echo "FAIL(rc) $label: oracle=$orc selfhost=$src"
    cat "$OUT/s.err" | head -5
    fail=$((fail + 1))
    return
  fi
  if [ "$orc" = 2 ]; then
    # Usage errors: compare first stderr line.
    if [ "$(head -1 "$OUT/o.err")" != "$(head -1 "$OUT/s.err")" ]; then
      echo "FAIL(usage) $label"
      diff -u <(head -1 "$OUT/o.err") <(head -1 "$OUT/s.err") | head -6
      fail=$((fail + 1))
      return
    fi
    pass=$((pass + 1))
    return
  fi
  if ! cmp -s <(sed 's/\r//; s|^\\\\?\\||' "$OUT/o.out") <(sed 's/\r//; s|^\\\\?\\||' "$OUT/s.out"); then
    echo "FAIL(stdout) $label"
    diff -u "$OUT/o.out" "$OUT/s.out" | head -12
    fail=$((fail + 1))
    return
  fi
  pass=$((pass + 1))
}

# 1. Repo examples/: --list, --filter, -f, full run, usage errors
check_case "repo/--list" "." --list
check_case "repo/--filter-math" "." --filter math_util
check_case "repo/-f-hello" "." -f hello
check_case "repo/full" "."
check_case "repo/usage-unknown" "." --bogus
check_case "repo/usage-filter-missing" "." --filter

# 2. Frozen corpus examples + edge in temp examples/
mkdir -p "$OUT/corpus_ws/examples"
cp selfhost/corpus/examples/*.matrix "$OUT/corpus_ws/examples/"
cp selfhost/corpus/engine_matrix/phase0_hlc_prime.matrix "$OUT/corpus_ws/examples/"
cp selfhost/corpus/engine_matrix/hybrid_loop_bounds.matrix "$OUT/corpus_ws/examples/"
cp selfhost/corpus/engine_matrix/closures.matrix "$OUT/corpus_ws/examples/"
check_case "corpus/all-pos" "$OUT/corpus_ws"

# 3. Failure detection (runtime assert/div0 + compile error + library error)
mkdir -p "$OUT/fail_ws/examples"
cp selfhost/corpus/examples/hello.matrix "$OUT/fail_ws/examples/"
cp selfhost/corpus/negative/runtime_division_by_zero.matrix "$OUT/fail_ws/examples/"
cp selfhost/corpus/negative/semantic_unknown_variable.matrix "$OUT/fail_ws/examples/"
cat > "$OUT/fail_ws/examples/lib_bad.matrix" <<'EOF'
fn helper() -> int {
    return "not_an_int";
}
EOF
check_case "failure-detection" "$OUT/fail_ws"

# 4. Project mode (spider.toml + tests/) & missing examples/ error
mkdir -p "$OUT/proj/tests"
cat > "$OUT/proj/spider.toml" <<'EOF'
[package]
name = "demo"
version = "0.1.0"
EOF
cp selfhost/corpus/examples/hello.matrix "$OUT/proj/tests/"
cp selfhost/corpus/examples/math_util.matrix "$OUT/proj/tests/"
check_case "project/tests" "$OUT/proj"

mkdir -p "$OUT/empty_dir"
check_case "no-examples" "$OUT/empty_dir"

echo "test runner parity: pass=$pass fail=$fail"
if [ "$fail" != 0 ]; then
  exit 1
fi
exit 0
