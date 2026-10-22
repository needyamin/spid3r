# VM notes: known miscompiles

## `not (A or B)` when B has to be evaluated

**Status:** open (worked around in `stdlib/yaml.matrix`)

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

**Workaround:** bind the logical to a local before negating it.

```spidr
let is_item = t == "-" or t.starts_with("- ");
if not is_item { ... }
```

That form is correct on both engines and is what `stdlib/yaml.matrix` does.

**Where to look:** `compile_expr` for `Expr::Logical` in `src/vm/compiler.rs`
emits `JumpIfTrue` / `Truthy` / `Jump` / `True`; `compile_expr` for
`Expr::Unary { UnOp::Not }` then emits `Not`. `JumpIfTrue` pops the left
operand, so the two paths (short-circuit vs right-evaluated) should each leave
exactly one value, but the right-evaluated path evidently does not. Dumping the
chunk for the snippet above and single-stepping `Not` will show which value it
consumes.

A regression test belongs in `tests/engine_matrix.rs` once fixed (the file
compares tree and VM output for each construct).
