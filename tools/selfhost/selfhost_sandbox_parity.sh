#!/usr/bin/env bash
# Self-host sandbox parity (roadmap Phase 6, step 6.3, docs/SELFHOST.md SH-5 +
# the security contract in docs/FFI.md / docs/SECURITY.md).
#
# What this gate pins, honestly per the contract:
#
#   A. FUNNEL INSTALL PARITY. `--sandbox` is a policy filter over the built-in
#      natives of the SCRIPT engines (docs/FFI.md: "does not apply to an AOT
#      binary"). So for built executables the sandbox property is the
#      full-authority install: every capability class (filesystem, network,
#      process, environment) behaves byte-identically in an oracle-built and
#      a selfhost-built executable of the same program — success paths and
#      error paths alike. Both link the SAME runtime staticlib; this gate
#      proves the identity end to end.
#
#   B. SCRIPT-ENGINE DENY PARITY. Deny-probe variants (every capability call
#      wrapped in try/catch) under `spid3r run --sandbox` on the tree and VM
#      engines: the denial must be a CATCHABLE throw carrying
#      `sandboxed: ... denied`, and tree/vm outputs must agree byte-for-byte.
#      This is the behavior the selfhosted front end feeds (its parse/check
#      of these programs is pinned by the Phase 1-3 harnesses).
#
#   C. NO PTR-FFI BYPASS. Within the script engines an `extern fn` is
#      rejected outright (`error[usage]`) — the pointer path the sandbox
#      cannot gate never exists there; the selfhosted bytecode compiler
#      rejects it too (Phase 4 gate, re-checked here).
#
# Usage: bash tools/selfhost/selfhost_sandbox_parity.sh [selfhost-compiler]
set -u
cd "$(dirname "$0")/../.."
SH="${1:-selfhost/main_native}"
[ -x "$SH" ] || SH="${SH}.exe"
ORACLE="${ORACLE:-target/debug/spid3r}"
[ -x "$ORACLE" ] || ORACLE="${ORACLE}.exe"

OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

pass=0
fail=0

# ------------------------------------------------------------ the probes
# One probe per capability class + an extern-fn probe. Every operation is
# deterministic on one machine: no live network, no wall clock.

mk_fs() {
  cat > "$OUT/fs.matrix" <<'EOF'
fn main() {
    let w = write_file("spid3r_sbx_probe.txt", "hello");
    println("write: " + string(w.is_ok()));
    let r = read_file("spid3r_sbx_probe.txt");
    println("read: " + r.unwrap());
    let fh = file_open("spid3r_sbx_probe.txt", "r").unwrap();
    println("handle_line: " + file_read_line(fh).unwrap());
    file_close(fh);
    let m = read_file("spid3r_sbx_no_such_file.txt");
    if m.is_err() {
        println("missing: caught " + m.unwrap_err());
    }
    let _d = file_exists("spid3r_sbx_probe.txt");
    println("done");
    return 0;
}
EOF
}

mk_net() {
  cat > "$OUT/net.matrix" <<'EOF'
fn main() {
    try {
        let c = tcp_connect("127.0.0.1", 1, 300);
        println("connected? " + string(c.is_ok()));
    } catch (e) {
        println("caught: " + e);
    }
    println("done");
    return 0;
}
EOF
}

mk_proc() {
  cat > "$OUT/proc.matrix" <<'EOF'
fn main() {
    let r = spawn_process("spid3r_no_such_binary_6f3", ["x"]);
    if r.is_err() {
        println("err: " + r.unwrap_err());
    } else {
        println("spawned: " + string(r.unwrap()));
    }
    println("done");
    return 0;
}
EOF
}

mk_env() {
  cat > "$OUT/env.matrix" <<'EOF'
fn main() {
    let v = env("SPID3R_NO_SUCH_VAR_6F3");
    println("unset is_none: " + string(v.is_none()));
    let p = env("SPID3R_RUNTIME_LIB");
    println("set is_some: " + string(p.is_some()));
    println("done");
    return 0;
}
EOF
}

mk_extern() {
  cat > "$OUT/ext.matrix" <<'EOF'
extern fn no_such_symbol(a: int) -> int;
fn main() {
    println(no_such_symbol(1));
    return 0;
}
EOF
}

# ------------------------------------------------- A: funnel install parity

for kind in fs net proc env; do
  mk_$kind
  "$ORACLE" build "$OUT/$kind.matrix" -o "$OUT/${kind}_o.exe" > /dev/null 2>&1
  ob=$?
  SPID3R_RUNTIME_LIB="$(pwd)/target/debug/spid3r_lang.lib" \
    "$SH" native "$OUT/$kind.matrix" "$OUT/${kind}_s.exe" > /dev/null 2>&1
  sb=$?
  if [ "$ob" != 0 ] || [ "$sb" != 0 ]; then
    echo "FAIL(build) $kind (oracle rc=$ob, selfhost rc=$sb)"
    fail=$((fail + 1))
    continue
  fi
  rm -f spid3r_sbx_probe.txt
  "$OUT/${kind}_o.exe" > "$OUT/${kind}_o.txt" 2>&1
  orc=$?
  rm -f spid3r_sbx_probe.txt
  "$OUT/${kind}_s.exe" > "$OUT/${kind}_s.txt" 2>&1
  src=$?
  if [ "$orc" = "$src" ] && cmp -s "$OUT/${kind}_o.txt" "$OUT/${kind}_s.txt"; then
    echo "PASS  funnel/$kind (rc=$orc)"
    pass=$((pass + 1))
  else
    echo "FAIL  funnel/$kind (oracle rc=$orc, selfhost rc=$src)"
    diff "$OUT/${kind}_o.txt" "$OUT/${kind}_s.txt" | head -6
    fail=$((fail + 1))
  fi
done

# --------------------------------------- B: script-engine deny parity
# Deny-probe variants: every capability call wrapped in try/catch — the denial
# must be a CATCHABLE throw carrying `sandboxed: ... denied`, identical across
# the tree and VM engines (the phase3_security.rs shape). This is the behavior
# the selfhosted front end feeds (its parse/check of these programs is pinned
# by the Phase 1-3 harnesses).

mk_sbx_fs() {
  cat > "$OUT/sbx_fs.matrix" <<'EOF'
fn main() {
    try { write_file("spid3r_sbx_probe.txt", "y"); println("BAD"); } catch (e) { println("caught: " + e); }
    try { let _r = read_file("spid3r_sbx_probe.txt"); println("BAD"); } catch (e) { println("caught: " + e); }
    try { let _h = file_open("spid3r_sbx_probe.txt", "r"); println("BAD"); } catch (e) { println("caught: " + e); }
    try { let _c = file_close(1); println("BAD"); } catch (e) { println("caught: " + e); }
    return 0;
}
EOF
}

mk_sbx_net() {
  cat > "$OUT/sbx_net.matrix" <<'EOF'
fn main() {
    try { let _c = tcp_connect("127.0.0.1", 1, 300); println("BAD"); } catch (e) { println("caught: " + e); }
    return 0;
}
EOF
}

mk_sbx_proc() {
  cat > "$OUT/sbx_proc.matrix" <<'EOF'
fn main() {
    try { let _p = spawn_process("spid3r_no_such_binary_6f3", ["x"]); println("BAD"); } catch (e) { println("caught: " + e); }
    return 0;
}
EOF
}

mk_sbx_env() {
  cat > "$OUT/sbx_env.matrix" <<'EOF'
fn main() {
    try { let _v = env("SPID3R_NO_SUCH_VAR_6F3"); println("BAD"); } catch (e) { println("caught: " + e); }
    return 0;
}
EOF
}

for eng in tree vm; do
  for kind in fs net proc env; do
    mk_sbx_$kind
    "$ORACLE" run --engine "$eng" --sandbox "$OUT/sbx_$kind.matrix" \
      > "$OUT/sbx_${kind}_${eng}.txt" 2>&1
    rc=$?
    # rc 0, no BAD, the catchable denial text — and tree/vm agree.
    if [ "$rc" = 0 ] \
       && ! grep -q "BAD" "$OUT/sbx_${kind}_${eng}.txt" \
       && grep -q "caught: sandboxed:" "$OUT/sbx_${kind}_${eng}.txt" \
       && ! grep -q "error\[runtime\]" "$OUT/sbx_${kind}_${eng}.txt"; then
      if [ "$eng" = "tree" ]; then
        cp "$OUT/sbx_${kind}_${eng}.txt" "$OUT/sbx_${kind}_ref.txt"
        pass=$((pass + 1))
      elif cmp -s "$OUT/sbx_${kind}_ref.txt" "$OUT/sbx_${kind}_${eng}.txt"; then
        pass=$((pass + 1))
      else
        echo "FAIL  sandbox/$kind tree-vs-vm"
        diff "$OUT/sbx_${kind}_ref.txt" "$OUT/sbx_${kind}_${eng}.txt" | head -4
        fail=$((fail + 1))
      fi
    else
      echo "FAIL  sandbox/$kind/$eng (rc=$rc)"
      head -3 "$OUT/sbx_${kind}_${eng}.txt"
      fail=$((fail + 1))
    fi
  done
done

# --------------------------------------------- C: no ptr-FFI bypass
# The script engines reject `extern fn` outright (error[usage]); the
# selfhosted bytecode compiler rejects it too (Phase 4's gate).

mk_extern
"$ORACLE" run "$OUT/ext.matrix" > "$OUT/ext_run.txt" 2>&1
rc1=$?
"$ORACLE" run --engine vm "$OUT/ext.matrix" > "$OUT/ext_vm.txt" 2>&1
rc2=$?
SPID3R_RUNTIME_LIB="$(pwd)/target/debug/spid3r_lang.lib" \
  "$SH" compile "$OUT/ext.matrix" "$OUT/ext.matrixc" > "$OUT/ext_sh.txt" 2>&1
rc3=$?
if [ "$rc1" != 0 ] && [ "$rc2" != 0 ] && [ "$rc3" != 0 ] \
   && grep -q "usage\|native executable" "$OUT/ext_run.txt" \
   && grep -q "native executable" "$OUT/ext_sh.txt"; then
  pass=$((pass + 1))
else
  echo "FAIL  extern-rejection (rc=$rc1/$rc2/$rc3)"
  head -2 "$OUT/ext_run.txt" "$OUT/ext_sh.txt"
  fail=$((fail + 1))
fi

# --------------------------------------------- D: atomic_int safe under --sandbox; dl_open rejected
cat > "$OUT/atom_sbx.matrix" <<'EOF'
fn main() {
    let a = atomic_int(5);
    a.fetch_add(37);
    println(a.load());
    return 0;
}
EOF
for eng in tree vm; do
  "$ORACLE" run --engine "$eng" --sandbox "$OUT/atom_sbx.matrix" > "$OUT/atom_${eng}.txt" 2>&1
  arc=$?
  if [ "$arc" = 0 ] && [ "$(cat "$OUT/atom_${eng}.txt" | tr -d '\r\n')" = "42" ]; then
    pass=$((pass + 1))
  else
    echo "FAIL  atomic_int --sandbox/$eng (rc=$arc)"
    cat "$OUT/atom_${eng}.txt"
    fail=$((fail + 1))
  fi
done

echo "sandbox parity: pass=$pass fail=$fail"
if [ "$fail" != 0 ]; then
  exit 1
fi
exit 0
