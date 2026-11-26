#!/usr/bin/env bash
# Self-host Phase 8.2 parity: `spid3r-self fmt` and `spid3r-self check`
# vs stage-0 Rust `spid3r`.
#
# Usage: bash tools/selfhost/selfhost_fmt_check_parity.sh [stage0] [stage1]
set -u
cd "$(dirname "$0")/../.."
S0="${1:-target/debug/spid3r}"
S1="${2:-selfhost/main_native}"
[ -x "$S0" ] || S0="${S0}.exe"
[ -x "$S1" ] || S1="${S1}.exe"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

pass=0
fail=0

run_cmp_stdout_rc() {
  local name="$1"; shift
  local -a args=("$@")
  "$S0" "${args[@]}" </dev/null >"$TMP/o.out" 2>"$TMP/o.err"
  local oc=$?
  "$S1" "${args[@]}" </dev/null >"$TMP/s.out" 2>"$TMP/s.err"
  local sc=$?
  if [ "$oc" != "$sc" ] || ! cmp -s "$TMP/o.out" "$TMP/s.out"; then
    echo "FAIL ($name): oc=$oc sc=$sc"
    diff -u "$TMP/o.out" "$TMP/s.out" | head -30 | sed 's/^/  /'
    echo "--- s0 err ---"; head -5 "$TMP/o.err" | sed 's/^/  /'
    echo "--- s1 err ---"; head -5 "$TMP/s.err" | sed 's/^/  /'
    fail=$((fail + 1))
    return 1
  fi
  pass=$((pass + 1))
  return 0
}

# 1. Default `fmt --check` (examples/ + benchmarks/)
run_cmp_stdout_rc "fmt-default-check" fmt --check
# 2. Directory expansion `fmt --check examples`
run_cmp_stdout_rc "fmt-dir-examples" fmt --check examples

# 3. CLI error paths for fmt
"$S0" fmt --nope </dev/null >/dev/null 2>&1; oc=$?
"$S1" fmt --nope </dev/null >/dev/null 2>&1; sc=$?
if [ "$oc" = 2 ] && [ "$sc" = 2 ]; then
  pass=$((pass + 1))
else
  echo "FAIL (fmt-unknown-flag): oc=$oc sc=$sc"
  fail=$((fail + 1))
fi

mkdir -p "$TMP/empty_dir"
"$S0" fmt "$TMP/empty_dir" </dev/null >/dev/null 2>&1; oc=$?
"$S1" fmt "$TMP/empty_dir" </dev/null >/dev/null 2>&1; sc=$?
if [ "$oc" = 2 ] && [ "$sc" = 2 ]; then
  pass=$((pass + 1))
else
  echo "FAIL (fmt-empty-dir): oc=$oc sc=$sc"
  fail=$((fail + 1))
fi

# 4. Synthetic unformatted corner cases (in-place format byte-for-byte + idempotence)
cat >"$TMP/raw_messy.matrix" <<'EOF'
// leading comment   
fn add(a:int,b:int)->int{let x=-1;let y=a+-2;if x<0{return y;}else{return b;}}
fn long_call(first_argument:int,second_argument:int,third_argument:int,fourth_argument:int,fifth_argument:int){return first_argument+second_argument+third_argument+fourth_argument+fifth_argument;}
fn main(){let xs=[1,2,3];let ys=xs.map(fn(z){return z+1;});print(add(1,2));print(long_call(100,200,300,400,500));print(ys);}  // trailing
EOF
printf 'fn crlf(){\r\nlet a=1;\r\n}\r\n' >"$TMP/crlf.matrix"
printf '#!/usr/bin/env spid3r\nfn main(){print(1);}\n' >"$TMP/shebang.matrix"
printf '/* block */\nfn main(){let x=1;/* inline */let y=2;}\n' >"$TMP/block_comment.matrix"

for casefile in raw_messy.matrix crlf.matrix shebang.matrix block_comment.matrix; do
  cp "$TMP/$casefile" "$TMP/o_$casefile"
  cp "$TMP/$casefile" "$TMP/s_$casefile"
  "$S0" fmt "$TMP/o_$casefile" </dev/null >"$TMP/o.f" 2>"$TMP/o.fe"
  oc=$?
  "$S1" fmt "$TMP/s_$casefile" </dev/null >"$TMP/s.f" 2>"$TMP/s.fe"
  sc=$?
  if [ "$oc" != "$sc" ] || ! cmp -s "$TMP/o_$casefile" "$TMP/s_$casefile"; then
    echo "FAIL (fmt-case $casefile): oc=$oc sc=$sc"
    diff -u "$TMP/o_$casefile" "$TMP/s_$casefile" | head -30 | sed 's/^/  /'
    fail=$((fail + 1))
  else
    "$S1" fmt --check "$TMP/s_$casefile" </dev/null >/dev/null 2>&1
    if [ $? = 0 ]; then
      pass=$((pass + 1))
    else
      echo "FAIL (fmt-idempotent $casefile)"
      fail=$((fail + 1))
    fi
  fi
done

# 5. Full corpus `fmt` byte-for-byte parity (both reformatted file bytes and exit codes)
mkdir -p "$TMP/c0" "$TMP/c1"
corpus_ok=0
corpus_fail=0
for f in selfhost/corpus/examples/*.matrix \
         selfhost/corpus/engine_matrix/*.matrix \
         selfhost/corpus/edge/*.matrix \
         selfhost/corpus/negative/*.matrix; do
  bn="$(basename "$(dirname "$f")")_$(basename "$f")"
  cp "$f" "$TMP/c0/$bn"
  cp "$f" "$TMP/c1/$bn"
  "$S0" fmt "$TMP/c0/$bn" </dev/null >/dev/null 2>"$TMP/o.ce"
  oc=$?
  "$S1" fmt "$TMP/c1/$bn" </dev/null >/dev/null 2>"$TMP/s.ce"
  sc=$?
  if [ "$oc" != "$sc" ]; then
    echo "FAIL (corpus fmt rc $bn): oc=$oc sc=$sc"
    corpus_fail=$((corpus_fail + 1))
  elif ! cmp -s "$TMP/c0/$bn" "$TMP/c1/$bn"; then
    echo "FAIL (corpus fmt bytes $bn)"
    diff -u "$TMP/c0/$bn" "$TMP/c1/$bn" | head -20 | sed 's/^/  /'
    corpus_fail=$((corpus_fail + 1))
  else
    corpus_ok=$((corpus_ok + 1))
  fi
done
if [ "$corpus_fail" = 0 ]; then
  pass=$((pass + 1))
else
  fail=$((fail + 1))
fi

# 6. `check` driver parity: clean output, `--dump-registry-json`, `--deny-warnings`, `--allow`, `--json`
run_cmp_stdout_rc "check-clean" check examples/hello.matrix
run_cmp_stdout_rc "check-registry-flag" check examples/hello.matrix --dump-registry-json

cat >"$TMP/warns.matrix" <<'EOF'
import "math";
fn helper(unused_param: int) -> int {
    let unused_var = 42;
    return 1;
    let dead = 2;
}
fn main() {
    print(helper(1));
    main();
}
EOF

"$S0" check --deny-warnings "$TMP/warns.matrix" </dev/null >"$TMP/o.wo" 2>"$TMP/o.we"; oc=$?
"$S1" check --deny-warnings "$TMP/warns.matrix" </dev/null >"$TMP/s.wo" 2>"$TMP/s.we"; sc=$?
if [ "$oc" != 0 ] && [ "$sc" != 0 ] && [ "$oc" = "$sc" ]; then
  pass=$((pass + 1))
else
  echo "FAIL (check-deny-warnings): oc=$oc sc=$sc"
  fail=$((fail + 1))
fi

cat >"$TMP/one_warn.matrix" <<'EOF'
fn main() {
    let unused_x = 1;
}
EOF
"$S0" check --deny-warnings --allow unused-variable "$TMP/one_warn.matrix" </dev/null >"$TMP/o.ao" 2>"$TMP/o.ae"; oc=$?
"$S1" check --deny-warnings --allow unused-variable "$TMP/one_warn.matrix" </dev/null >"$TMP/s.ao" 2>"$TMP/s.ae"; sc=$?
if [ "$oc" = 0 ] && [ "$sc" = 0 ] && cmp -s "$TMP/o.ao" "$TMP/s.ao"; then
  pass=$((pass + 1))
else
  echo "FAIL (check-allow-lint): oc=$oc sc=$sc"
  diff -u "$TMP/o.ao" "$TMP/s.ao" | sed 's/^/  /'
  fail=$((fail + 1))
fi

# --json warnings + no `: ok` line
"$S0" check --json "$TMP/one_warn.matrix" </dev/null >"$TMP/o.jo" 2>"$TMP/o.je"; oc=$?
"$S1" check --json "$TMP/one_warn.matrix" </dev/null >"$TMP/s.jo" 2>"$TMP/s.je"; sc=$?
if [ "$oc" = "$sc" ] && cmp -s "$TMP/o.jo" "$TMP/s.jo"; then
  pass=$((pass + 1))
else
  echo "FAIL (check-json-warn): oc=$oc sc=$sc"
  diff -u "$TMP/o.jo" "$TMP/s.jo" | sed 's/^/  /'
  fail=$((fail + 1))
fi

# --json on all 4 lint kinds (`warns.matrix`) comparing exact sorted JSON lines
"$S0" check --json --deny-warnings "$TMP/warns.matrix" </dev/null >"$TMP/o.jall" 2>/dev/null; oc=$?
"$S1" check --json --deny-warnings "$TMP/warns.matrix" </dev/null >"$TMP/s.jall" 2>/dev/null; sc=$?
sort "$TMP/o.jall" > "$TMP/o.jall.s"
sort "$TMP/s.jall" > "$TMP/s.jall.s"
if [ "$oc" = "$sc" ] && cmp -s "$TMP/o.jall.s" "$TMP/s.jall.s"; then
  pass=$((pass + 1))
else
  echo "FAIL (check-json-all-lints): oc=$oc sc=$sc"
  diff -u "$TMP/o.jall.s" "$TMP/s.jall.s" | sed 's/^/  /'
  fail=$((fail + 1))
fi

# --json semantic error
cat >"$TMP/sem_err.matrix" <<'EOF'
fn main() {
    let x: int = "bad";
}
EOF
"$S0" check --json "$TMP/sem_err.matrix" </dev/null >"$TMP/o.eo" 2>/dev/null; oc=$?
"$S1" check --json "$TMP/sem_err.matrix" </dev/null >"$TMP/s.eo" 2>/dev/null; sc=$?
if [ "$oc" = "$sc" ] && cmp -s "$TMP/o.eo" "$TMP/s.eo"; then
  pass=$((pass + 1))
else
  echo "FAIL (check-json-error): oc=$oc sc=$sc"
  diff -u "$TMP/o.eo" "$TMP/s.eo" | sed 's/^/  /'
  fail=$((fail + 1))
fi

# check bad flags -> exit 2
"$S0" check --bad-flag examples/hello.matrix </dev/null >/dev/null 2>&1; oc=$?
"$S1" check --bad-flag examples/hello.matrix </dev/null >/dev/null 2>&1; sc=$?
if [ "$oc" = 2 ] && [ "$sc" = 2 ]; then
  pass=$((pass + 1))
else
  echo "FAIL (check-unknown-flag): oc=$oc sc=$sc"
  fail=$((fail + 1))
fi

echo "fmt+check parity: pass=$pass fail=$fail (corpus_ok=$corpus_ok corpus_fail=$corpus_fail)"
if [ "$fail" -ne 0 ]; then
  exit 1
fi
echo "SELFHOST FMT+CHECK PARITY: ALL MATCH"
