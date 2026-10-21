# Stage 2b — the hoisting pass: value-position `if`, `?.` and `??`

Date: 2026-10-21 · Toolchain: rustc/cargo 1.99.1 (pinned) · Base: `6f0adb6`
Scope: the three surface features that need a **conditional expression**, and the
lowering machinery that makes them sound. No backend changed.

## 1. Why this needed machinery, not a shortcut

All three constructs need "evaluate one of two things", which the AST has no node
for. Two shortcuts were available and both are wrong:

| Shortcut | Why it fails |
|---|---|
| Desugar to an immediately-invoked closure `(fn() { if (c) { return a } return b })()` | `return` inside a branch would exit the *closure*, so `let x = if (c) { return 5 } else { 1 }` would bind 5 instead of returning from the function. |
| Desugar `a ?? b` to a native `coalesce(a, b)` | Evaluates `b` eagerly, so `x ?? expensive()` calls `expensive()` even when `x` is present. |

So the pass lowers each construct to a **temporary plus ordinary statements**,
and hoists those statements to the enclosing statement.

## 2. The hoisting rule

The subtle part is evaluation order. Consider:

```spidr
g(h(), if (c) { a } else { b })
```

Hoisting the `if` before the call would run `c`/`a`/`b` before `h()`. So the rule
is: **if any child of a composite expression needs a prelude, every child is
spilled into a temporary in source order.**

```spidr
let __spidr_t0 = h();
let __spidr_t1 = null; if (c) { __spidr_t1 = a } else { __spidr_t1 = b }
g(__spidr_t0, __spidr_t1)
```

That is ANF — but only where it is needed. A statement containing none of the
three nodes is copied through untouched, and a *whole program* containing none of
them returns `Cow::Borrowed`, so no existing program is copied or changed. That
property is asserted directly:

```rust
assert!(matches!(lowered, Cow::Borrowed(_)),
        "a program without if-expressions, ?. or ?? must not be copied");
```

Spilling is required even for a plain identifier: in
`g(x, if (c) { x = 5; 1 } else { 2 })` the source reads `x` *before* the branch,
so the spill must happen before it. Reading `x` lazily would observe the new
value.

## 3. Control flow that must not become eager

`and`/`or` return the **operand**, not a bool (`false and boom()` prints
`false`; `true and boom()` prints `boom`'s result). Spilling both sides would
evaluate the right one unconditionally. Instead, when either side needs a
prelude:

```spidr
let __t = <left>; if (bool(__t))      { __t = <right> }   // and
let __t = <left>; if (not bool(__t))  { __t = <right> }   // or
```

A `while` condition with a prelude keeps its once-per-iteration semantics:

```spidr
while (true) { <prelude>; if (not <cond>) { break } <body> }
```

and `for let i = …; cond; post` with a prelude in `cond` or `post` becomes the
equivalent `while (true)` loop. Both are exercised by tests
(`a_loop_condition_with_a_prelude_runs_each_iteration`).

## 4. The features

```spidr
let grade = if (n >= 90) { "A" } else if (n >= 80) { "B" } else { "C" };
let name  = user?.name ?? "anonymous";
```

- **Value-position `if`** — a branch yields its last expression, and that final
  `;` may be omitted (a `value_block_depth` flag in the parser allows it only
  there). A missing `else` yields `null`. A branch that ends in `return`/`throw`
  needs no value; a branch that ends in a declaration is a semantic error
  ("an if-expression branch must end with an expression").
- **`??`** — binds looser than `or`, tighter than assignment. The right side runs
  only when the left is null. `0` and `false` are not null, so they win.
- **`?.`** — the receiver is evaluated exactly once; a null receiver yields null.
  A call through `?.` (`obj?.m()`) is rejected with a clear message rather than
  silently evaluating the arguments — that needs a different lowering.

## 5. Two bugs found along the way

**A stack overflow, in the parser.** Adding the `??` precedence level pushed the
parser over the edge: at `MAX_PARSE_DEPTH = 96` with ~13 frames per nesting
level, a 2 MB thread stack (Rust's default for spawned threads, and what the
test harness uses) overflowed — `audit_regressions::parser_deep_nesting_*` died
with `STATUS_STACK_OVERFLOW`. Fixed two ways: `??` is folded into `parse_assign`
instead of adding a frame per level, and the cap is now **64**, which leaves
real margin. A diagnostic is still returned for deeper input.

**`map.field` differed per engine.** The matrix's `opt_field` case failed on AOT
with `cannot read field 'k' of map`, while tree and VM read the key. This is a
**pre-existing** divergence that `?.` merely exposed: `spid3r_field_get` in
`native/rt.rs` handled instances only. It now has the map arm, matching the
interpreter.

## 6. Gates

| Gate | Result |
|---|---|
| `cargo test --no-default-features` | **532 passed, 0 failed** |
| `cargo test --features native --test engine_matrix` | 5/5 — the AOT subset runs `if_expression`, `null_coalesce`, `opt_field`, `hoist_preserves_order` |
| `spid3r test` (three-engine differential) | **25 ok, 0 failed** |
| clippy `-D warnings` (core + native) | 0 |
| `cargo fmt --all -- --check` | clean |
| GUI / AOT suites | 10/10, 6/6 |

Canonical counts: **548 tests** (532 core + 10 GUI + 6 AOT).

New coverage: `tests/stage2b_hoisting.rs` (14) and six matrix rows
(`if_expression`, `null_coalesce`, `opt_field`, `hoist_preserves_order`,
`err_if_branch_has_no_value`, plus the two lowering properties).

## 7. What is still not here

| Item | Why |
|---|---|
| `obj?.m()` (call through `?.`) | The argument list must be skipped when the receiver is null, so it needs the arguments hoisted *inside* the conditional. Different lowering; rejected with a clear message today. |
| `a?[i]` (index through `?.`) | Same shape as the call case. |
| `for (k, v) in map` | Needs `Stmt::ForIn` to carry a second binding, then a `lower` rule; not attempted. |
| Destructuring `let (a, b) = t` | Needs `Stmt::Let` to carry a pattern. |
| Default parameters | Needs a `MISSING` sentinel that user code cannot observe. |
| `spawn` of any callable | Needs a decision on whether the callee is evaluated at spawn time or inside the task. |

The hoisting machinery built here is the prerequisite for all of them: `match`
arms in expression position (Stage 3) will reuse `seq` and `value_block`
directly.
