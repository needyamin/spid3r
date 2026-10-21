#!/bin/bash
# Differential test: VM (tree) vs native (AOT) across all examples.
# Usage: bash tools/native_diff.sh [example...]
#
# Every step is wrapped in `timeout` so a single hang is reported (HANG) and
# skipped instead of wedging the whole run. Tune via env vars:
#   VM_TIMEOUT (default 30), BUILD_TIMEOUT (default 180), RUN_TIMEOUT (default 30)
#
# NOTE: this needs the debug spid3r built WITH the native backend
# (cargo build --features native). Plain `cargo test`/`cargo build` overwrites
# that binary with a non-native one and every example would SKIP.
cd "$(dirname "$0")/.."
# The binary name carries .exe only on Windows (Git Bash included).
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) EXE=spid3r.exe ;;
    *)                    EXE=spid3r     ;;
esac
SPID3R=./target/debug/$EXE
OUT=target/isolated
VM_TIMEOUT=${VM_TIMEOUT:-30}
BUILD_TIMEOUT=${BUILD_TIMEOUT:-180}
RUN_TIMEOUT=${RUN_TIMEOUT:-30}
mkdir -p "$OUT"
pass=0; fail=0; skip=0; hang=0
native_warned=0
for f in "$@"; do
    [ -z "$f" ] && continue
    name=$(basename "$f" .matrix)
    # VM reference output (tree interpreter); stdin closed so interactive
    # programs see EOF immediately
    vm_out=$(timeout "$VM_TIMEOUT" "$SPID3R" run "$f" </dev/null 2>&1); vm_rc=$?
    if [ "$vm_rc" = "124" ]; then
        echo "HANG  $name (VM run exceeded ${VM_TIMEOUT}s)"
        hang=$((hang+1)); continue
    fi
    # native build + run
    timeout "$BUILD_TIMEOUT" "$SPID3R" build "$f" -o "$OUT/$name.exe" >/dev/null 2>"$OUT/$name.buildlog"
    brc=$?
    if [ "$brc" = "124" ]; then
        echo "HANG  $name (native build exceeded ${BUILD_TIMEOUT}s)"
        hang=$((hang+1)); continue
    fi
    if [ "$brc" != "0" ]; then
        if [ "$native_warned" = "0" ] && grep -q "not enabled in this build" "$OUT/$name.buildlog"; then
            echo "NOTE: $SPID3R lacks the native backend; rebuild with: cargo build --features native"
            native_warned=1
        fi
        echo "SKIP  $name (build failed: $(head -1 "$OUT/$name.buildlog" | cut -c1-90))"
        skip=$((skip+1)); continue
    fi
    nat_out=$(timeout "$RUN_TIMEOUT" "$OUT/$name.exe" </dev/null 2>&1); nat_rc=$?
    if [ "$nat_rc" = "124" ]; then
        echo "HANG  $name (native run exceeded ${RUN_TIMEOUT}s)"
        hang=$((hang+1)); continue
    fi
    if [ "$vm_out" == "$nat_out" ] && [ "$vm_rc" == "$nat_rc" ]; then
        echo "PASS  $name"
        pass=$((pass+1))
    else
        echo "FAIL  $name"
        echo "  vm  (rc=$vm_rc): $(echo "$vm_out" | head -3 | tr '\n' '|')"
        echo "  nat (rc=$nat_rc): $(echo "$nat_out" | head -3 | tr '\n' '|')"
        fail=$((fail+1))
    fi
done
echo "---- pass=$pass fail=$fail skip=$skip hang=$hang"
