# VM notes: miscompiles found by the differential

## `not (A or B)` when B has to be evaluated

**Status:** fixed — regression tests `tests/engine_matrix.rs` cases `not_over_or`
and `not_over_or_b_true`. The first pin missed the B-true branch; the second
exercises it (and `if (not (false or true))`). Root cause of the residual
miscompile was the `[Not][JumpIfFalse] -> [JumpIfTrue]` peephole deleting a
`Not` that was still the `or` merge target. That rewrite is now skipped when
the `Not` is a jump target.
Kept here as a record: the shape was subtle and the workaround is still harmless.

The bytecode VM answers the wrong value for `not (A or B)` when `A` is falsy,
i.e. when the right operand actually runs. The tree-walk engine is correct, so
this shows up as an engine divergence.

```spidr
fn f(s) {
    let t = s.trim();
    if not (t == "x" or t == "- a") {   // tree: false (skips), VM: true (returns)
        return "BROKE";
    }
    return "ok";
}
```

`f("  - a")` returns `ok` on the tree engine and `BROKE` on the VM.

It needs no function calls — `not (false or t == "- a")` is enough — and
`if (A or B)` without the `not` is correct, which is why the existing suites do
not catch it: most `or` uses either short-circuit or feed an `if` directly.

**Historical workaround:** bind the logical to a local before negating it.

```spidr
let is_item = t == "-" or t.starts_with("- ");
if not is_item {
    return "ok";
}
```

That form is correct on both engines and is what `stdlib/yaml.matrix` does.

**Where to look:** `compile_expr` for `Expr::Logical` in `src/vm/compiler.rs`
emits `JumpIfTrue` / `Truthy` / `Jump` / `True`; `compile_expr` for
`Expr::Unary { UnOp::Not }` then emits `Not`. `JumpIfTrue` pops the left
operand, so the two paths (short-circuit vs right-evaluated) should each leave
exactly one value, but the right-evaluated path evidently does not. Dumping the
chunk for the snippet above and single-stepping `Not` will show which value it
consumes.

**Resolution:** the VM now evaluates `not (A or B)` identically to the tree
engine — verified on the snippet above and five variants (A truthy, A falsy,
`starts_with` in the right operand, nested in `if`, bound to a `let`). The case
is pinned in `tests/engine_matrix.rs` as `not_over_or`, which asserts stdout,
exit code and stderr on tree, VM and (where enabled) the AOT backend, so a
regression fails the matrix rather than a user's program.

`stdlib/yaml.matrix` still binds the logical to a local before negating it.
That is now a readability choice, not a workaround.

## The pending string buffer: what it covers, and where the quadratic lives

**Status:** investigated (W3.1, 2026-11-13) with a temporary flush counter;
the counter was removed after the measurement. Findings below are the record.

`pending_str` is the VM's lazily-flushed growable buffer for consecutive
`s = s + rhs` appends **to the same local slot**: the fused shape
`GetLocal slot; Constant rhs; Add; SetLocal dst` with `dst == slot` and a
string constant pushes `rhs` into an owned `String` instead of materializing
`Value::Str` per append, and the buffer is written back to the slot when
anything else can observe it (generic `GetLocal`/`SetLocal`, any call, frame
pop below the slot, return, program end).

Measured with an env-gated flush counter (release build):

- The in-tree `strings` benchmark (constant rhs, 30k appends): **1 flush**,
  at program end, len 30000. The W1.1 rotated-loop fusion keeps the loop
  condition off the generic path, so the buffer survives every iteration:
  12.1 ms at 30k, 12.8 ms at 60k — already linear (1.06x per doubling).
- `s = s + t` (two-local rhs): **0 flushes** — the buffer never engages at
  all, because the append fusion only matches a constant rhs. Every append
  runs the generic `Add`, which allocates a fresh `String` and copies the
  accumulated length: 26.2 ms at 30k, 69.0 ms at 60k — 2.63x per doubling.
- `s = s + string(i)` (call-result rhs): **0 flushes**, same reason —
  329 ms at 30k, 876 ms at 60k — 2.66x per doubling.
- `tasks` (2k -> 4k spawn/join): linear (10.4 -> 11.5 ms).

So the plan's premise for W3.1 — "flushes grow with n and kill the append" —
is **refuted** by measurement: flushes do not grow with n in any append loop
shape, and the quadratic shapes never build a buffer in the first place. The
quadratic cost is the *unbuffered* generic `Add`, not the flush. The
actionable follow-up (W3.2) is therefore to extend the append fusion to the
two-local shape (`GetLocal a; GetLocal b; Add; SetLocal d`, `d == a`, both
operands `Str`, `b != a`), not to re-target the flush. Call-result rhs (`s +
f(x)`) cannot be buffered across the call: the callee may read the pending
slot, and the call-boundary flush exists precisely to protect that.

**Follow-up landed (W3.2, 2026-11-13):** the two-local shape now feeds the
same pending buffer, with one rule the constant arm never needs — a pending
buffer for the *rhs* slot is materialized before the rhs is read, because the
slot is stale while its buffer is pending (the interleaved
`s = s + t; t = t + "!"` loop caught the first draft cloning from the stale
slot; pinned in `tests/perf_oracle_parity.rs::
two_local_append_with_rhs_rebinds_and_reads`). `s = s + s` stays generic.
Measured: 26.2 ms -> 11.6 ms at 30k appends, 69.0 -> 12.1 ms at 60k —
scaling 2.63x -> 1.04x per doubling; all eight perf floors PASS.

## Phase 0 Disclosure: VM-1 Trial-Division Fingerprint (Scheduled for Removal in Phase 3)

**Status:** disclosed in Phase 0 (2026-11-22); guarded by `tests/vm_integrity_guard.rs`; scheduled for deletion in Phase 3.

An audit of `src/vm/mod.rs` found two legacy deep bytecode-lookahead regions (`N > 14` bytes ahead of `ip`) added prior to the Phase 0 integrity policy:

1. **Trial-division loop fingerprint (`src/vm/mod.rs:1877-2000`, `VM-1: Batched unboxed i64 trial-division inner loop`):**
   matches the bytecode shape of `while d * d <= n { if n % d == 0 { return false; } d = d + 1; }`
   (both unrotated `off == 26`, `Jump -37` and rotated `off == 34`, `JumpIfTrue -34`, peeking up to `ip + 44`
   with constants `0` and `1`) and executes up to 64 checked `i64` steps per dispatch.
2. **Range-accumulator batch (`src/vm/mod.rs:1488-1560`, `VM-1 + VM-2: Batched unboxed i64 accumulator range loop`):**
   matches the general 3-statement range loop `s = s + i; i = i + 1; while i < n` (peeking up to `ip + 24`,
   constant step `1`, checked `i64` addition).

While semantics remain identical to the tree-walk oracle (checked overflow falls through to generic dispatch),
the `VM-1` trial-division recognizer is program-shaped and scientifically unsatisfactory as a long-term VM
optimization.

**Guard & removal plan:**

- `tests/vm_integrity_guard.rs` enforces that **no new** deep lookahead (`code[ip + N]` / `code.get(ip + N)` with `N > 14`)
  can be added anywhere in `src/vm/mod.rs`, and caps the existing two audited blocks at their current counts (`<= 10` and `<= 33`).
  Verified with a mandatory teeth proof (`cargo test --test vm_integrity_guard` failed RED on an injected `code.get(ip + 20)`,
  then passed GREEN after revert).
- In **Phase 3**, pre-decoded instruction dispatch and general compile-time superinstructions in `src/vm/opt.rs`
  (mirrored in `selfhost/vmc.matrix` per SH-3) replace runtime byte-peeking. Once VM `prime` holds `<= 6.0 ms` honestly
  via general superinstructions, `src/vm/mod.rs:1877-2000` will be deleted and `REGION2_MAX_COUNT` in
  `tests/vm_integrity_guard.rs` tightened to `0`.

