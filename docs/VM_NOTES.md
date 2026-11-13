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
