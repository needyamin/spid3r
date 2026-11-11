#!/usr/bin/env bash
# Differential test: run every examples/*.matrix on the tree-walk interpreter
# and the bytecode VM, plus a compile->exec round-trip, and diff the output.
set -u
cd "$(dirname "$0")/.."
BIN=target/debug/spid3r.exe
TMO=40            # per-invocation timeout (seconds) so a hang can't stall CI
OUT=/tmp/difftest
mkdir -p "$OUT"
fail=0
# Always close stdin. examples/server_report.matrix streams read_line() until
# EOF; when the harness inherits an open stdin pipe (CI, background shells),
# tree/vm/exec all block until TMO and the gate reports fake timeouts. Closing
# stdin matches tools/native_diff.sh and gives all three engines the same EOF.
for f in examples/*.matrix; do
  base=$(basename "$f" .matrix)
  echo "  ... $base"
  if grep -q "fn main" "$f"; then
    timeout $TMO "$BIN" run --engine tree "$f" </dev/null > "$OUT/${base}.tree" 2>&1
    tcode=$?
    timeout $TMO "$BIN" run --engine vm   "$f" </dev/null > "$OUT/${base}.vm"   2>&1
    vcode=$?
    # Compile co-located with the source (mirrors `default_matrixc`) so that
    # module resolution during `exec` finds sibling modules like `math_util`.
    mcpath="examples/${base}.matrixc"
    timeout $TMO "$BIN" compile "$f" -o "$mcpath" </dev/null >/dev/null 2>&1
    timeout $TMO "$BIN" exec "$mcpath" </dev/null > "$OUT/${base}.exec" 2>&1
    ecode=$?
    rm -f "$mcpath"
    [ "$tcode" = 124 ] && { echo "TIMEOUT tree $base"; fail=1; }
    [ "$vcode" = 124 ] && { echo "TIMEOUT vm   $base"; fail=1; }
    [ "$ecode" = 124 ] && { echo "TIMEOUT exec $base"; fail=1; }
    if ! diff -q "$OUT/${base}.tree" "$OUT/${base}.vm" >/dev/null; then
      echo "DIFF tree<->vm   : $base (tree_exit=$tcode vm_exit=$vcode)"
      fail=1
    fi
    if ! diff -q "$OUT/${base}.tree" "$OUT/${base}.exec" >/dev/null; then
      echo "DIFF tree<->exec : $base (exec_exit=$ecode)"
      fail=1
    fi
  else
    timeout $TMO "$BIN" check "$f" </dev/null >/dev/null 2>&1 || { echo "CHECK FAIL: $base"; fail=1; }
  fi
done
if [ "$fail" -eq 0 ]; then
  echo "ALL EXAMPLES MATCH (tree == vm == exec)"
else
  echo "FAILURES DETECTED"
fi
exit $fail
