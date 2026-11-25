#!/bin/bash
# Full verification gate: every feature configuration, tests + clippy, plus a
# release build. Usage: bash tools/gate.sh [step-timeout-seconds]
#
# Feature configurations ALL write target/debug/spid3r, so the steps run
# strictly sequentially here — launching a second cargo build with a different
# feature set while one is running silently overwrites the binary and produces
# bogus failures. Every step is wrapped in `timeout` (default 900s) and
# reported as PASS / FAIL / HANG so one slow step cannot wedge the gate.
cd "$(dirname "$0")/.."
STEP_TIMEOUT=${1:-900}

pass=0; fail=0; hang=0
failed_steps=()

# step <label> <cmd...>
step() {
    local label="$1"; shift
    local log rc
    log=$(mktemp)
    timeout "$STEP_TIMEOUT" "$@" >"$log" 2>&1
    rc=$?
    if [ "$rc" = "124" ]; then
        echo "HANG  $label (exceeded ${STEP_TIMEOUT}s)"
        hang=$((hang+1))
        failed_steps+=("$label")
    elif [ "$rc" != "0" ]; then
        echo "FAIL  $label"
        # surface the useful tail: warning/error lines if present, else raw tail
        grep -E "^(error|warning|test result|---- )" "$log" | tail -n 8
        grep -E "^(error|warning|test result|---- )" "$log" >/dev/null || tail -n 8 "$log"
        echo "      full log: $log"
        fail=$((fail+1))
        failed_steps+=("$label")
    else
        # report the test summary (or the clippy finish line) so a passing run
        # still shows its numbers
        local summary
        summary=$(grep -E "test result|Finished \`" "$log" | tail -n 2 | tr '\n' ' ')
        echo "PASS  $label   ${summary:-ok}"
        rm -f "$log"
        pass=$((pass+1))
    fi
}

for cfg in "default" "gui" "native" "native+gui"; do
    case "$cfg" in
        default)   feat_args=() ;;
        gui)       feat_args=(--features gui) ;;
        native)    feat_args=(--features native) ;;
        native+gui) feat_args=(--features "native gui") ;;
    esac
    echo "== configuration: $cfg =="
    step "test [$cfg]"   cargo test "${feat_args[@]}"
    step "clippy [$cfg]" cargo clippy --all-targets "${feat_args[@]}" -- -D warnings
done

echo "== unsafe / SAFETY invariant =="
PY_CMD=""
if command -v python3 >/dev/null 2>&1 && python3 --version >/dev/null 2>&1; then
    PY_CMD="python3"
elif command -v python >/dev/null 2>&1 && python --version >/dev/null 2>&1; then
    PY_CMD="python"
fi
if [ -n "$PY_CMD" ]; then
    step "unsafe/SAFETY gate" "$PY_CMD" tools/check_unsafe_safety.py
else
    step "unsafe/SAFETY gate" sh -c 'echo "neither python3 nor python found" >&2; exit 2'
fi

echo "== release build =="
step "build --release --features \"native gui\"" cargo build --release --features "native gui"

echo "---- pass=$pass fail=$fail hang=$hang"
if [ "$fail" != "0" ] || [ "$hang" != "0" ]; then
    echo "gate FAILED: ${failed_steps[*]}"
    exit 1
fi
echo "gate PASSED"
