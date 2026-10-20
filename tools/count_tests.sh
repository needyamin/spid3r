#!/usr/bin/env bash
# Canonical test / example / fuzz counts (audit A-18).
#
# The docs used to quote four different totals (static `#[test]` grep, "418+",
# 447, 467) against an executed 454. This script is the single source of truth:
# it asks cargo, not a grep, and prints the exact numbers to quote.
#
#   tools/count_tests.sh          # discover counts (fast: `--list`, no tests run)
#   tools/count_tests.sh --run    # executed counts (runs every suite)
#   tools/count_tests.sh --static # raw `#[test]` occurrences (for comparison)
#
# Requires: bash, cargo, awk, grep (Git Bash on Windows is fine).

set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

mode="list"
for arg in "$@"; do
  case "$arg" in
    --run) mode="run" ;;
    --static) mode="static" ;;
    -h|--help)
      sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

# Sum the `N passed` numbers from `test result: ok. N passed; ...` lines.
sum_passed() {
  awk '/^test result:/ {
         for (i = 1; i <= NF; i++) if ($i == "passed;") { n += $(i-1) }
       } END { print n + 0 }'
}

# Count `name: test` lines produced by `cargo test -- --list`.
count_listed() {
  awk '/: test$/ { n++ } END { print n + 0 }'
}

core_args=(--no-default-features)
gui_args=(--no-default-features --features gui --test gui_tests)
native_args=(--no-default-features --features native --test native_tests)

if [ "$mode" = "run" ]; then
  core=$(cargo test "${core_args[@]}" 2>&1 | sum_passed)
  gui=$(cargo test "${gui_args[@]}" 2>&1 | sum_passed)
  native=$(cargo test "${native_args[@]}" 2>&1 | sum_passed)
  label="executed"
elif [ "$mode" = "static" ]; then
  core=$(grep -r --include='*.rs' -h '#\[test\]' src tests | wc -l | tr -d ' ')
  gui=$(grep -c '#\[test\]' tests/gui_tests.rs 2>/dev/null || echo 0)
  native=$(grep -c '#\[test\]' tests/native_tests.rs 2>/dev/null || echo 0)
  label="static #[test]"
else
  core=$(cargo test "${core_args[@]}" -- --list 2>/dev/null | count_listed)
  gui=$(cargo test "${gui_args[@]}" -- --list 2>/dev/null | count_listed)
  native=$(cargo test "${native_args[@]}" -- --list 2>/dev/null | count_listed)
  label="discovered"
fi

total=$((core + gui + native))

examples_total=$(find examples -maxdepth 1 -name '*.matrix' | wc -l | tr -d ' ')
examples_runnable=$(grep -l '^fn main' examples/*.matrix 2>/dev/null | wc -l | tr -d ' ')
fuzz_targets=$(find fuzz/fuzz_targets -name '*.rs' 2>/dev/null | wc -l | tr -d ' ')

printf 'Spid3r counts (%s, %s)\n' "$label" "$(date -u +%Y-%m-%dT%H:%MZ)"
printf '  core (--no-default-features) : %s\n' "$core"
printf '  gui  (gui_tests)             : %s\n' "$gui"
printf '  aot  (native_tests)          : %s\n' "$native"
printf '  --------------------------------------------------\n'
printf '  TOTAL tests                  : %s\n' "$total"
printf '  examples/*.matrix files      : %s (%s with `fn main`, i.e. runnable programs)\n' \
  "$examples_total" "$examples_runnable"
printf '  fuzz targets                 : %s\n' "$fuzz_targets"
