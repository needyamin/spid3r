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
