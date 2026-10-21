# Stage 2c — the deferred ergonomics: destructuring, `for (k, v)`, `spawn`

Date: 2026-10-21 · Toolchain: rustc/cargo 1.99.1 (pinned) · Base: `127c968`
Scope: the three items Stage 2 listed and did not deliver. All three are
parser/lowering work — no backend gained a capability.

```spidr
let (lo, hi) = min_max(xs);          // destructuring
for (k, v) in scores { total += v; } // map entries
let t = spawn worker;                // any callable
```

## 1. Destructuring `let (a, b) = expr`

New `Stmt::LetTuple`, expanded by `lower`:

```spidr
let __d = <expr>;      // evaluated ONCE
let a = __d[0];
let b = __d[1];
```

Binding it this way keeps the value single-evaluation (a naive
`let a = e[0]; let b = e[1];` would run `e` twice) and needs nothing from the
engines. `tests/stage2c_ergonomics.rs::destructuring_evaluates_its_value_once`
pins that with a side-effecting producer.

The names are ordinary bindings — they can be reassigned, shadowed in a nested
block, and destructured again.

## 2. `for (k, v) in map`

`Stmt::ForIn` gained `var2: Option<String>`, and `lower` rewrites:

```spidr
let __src = <iterable>;
for k in __src {
    let v = __src[k];
    <body>
}
```

The single-variable form is untouched (a map still yields keys, a list still
yields elements), so the two forms stay unambiguous.

**A bug this exposed in my own fast path.** `lower` returns `Cow::Borrowed` when
a program contains nothing it needs to rewrite. The scan for `Stmt::ForIn` used
`..` and therefore ignored `var2` — so a program whose *only* new construct was
`for (k, v) in m` was passed through unchanged, the engines ignored the second
variable, and the body failed at run time with `unknown name 'v'`. The test
caught it; the scan now tests `var2.is_some()`. The lesson is recorded in the
module docs: **every new field that requires a rewrite must be added to the
scan, not just to the rewriter.**

## 3. `spawn` of any callable

The parser and the checker both required `matches!(callable, Expr::Fn(_))`:

```
error[semantic]: spawn expects a function: spawn fn() { ... };
```

The *runtime* never had that restriction — it evaluates the callable and checks
the value's type. So the fix was to delete two checks, and `spawn worker` /
`spawn closure_var` now work. A non-callable still fails cleanly at run time:
`spawn expects a function, found int`.

One consequence worth knowing: `spawn f()` parses (it is just an expression) and
then fails at run time because the *result* is not a function. The error names
the actual type, which is enough to make the mistake obvious.

## 4. Gates

| Gate | Result |
|---|---|
| `cargo test --no-default-features` | **562 passed, 0 failed** |
| `cargo test --features native --test engine_matrix` | 5/5 — `destructure`, `for_kv` and `spawn_named` all run through AOT |
| `spid3r test` (three-engine differential) | **25 ok, 0 failed** |
| clippy `-D warnings` (core + native) | 0 |
| `cargo fmt --all -- --check` | clean |

Canonical counts: **578 tests** (562 core + 10 GUI + 6 AOT).

New coverage: `tests/stage2c_ergonomics.rs` (8) and three matrix rows.

## 5. What is still on the Stage-2 list

| Item | Why it is not here |
|---|---|
| Default parameters `fn f(a, b = 1)` | **Not desugarable.** The arity check happens before the body runs, in all three engines, so a callee prologue cannot fill a missing argument; it needs either a `MISSING` sentinel passed by every call site (which requires knowing the callee's defaults, impossible across modules) or a change to the arity rule in three engines. |
| `obj?.m()` / `a?[i]` | The argument list (or index) must be skipped when the receiver is null, so the arguments have to be hoisted *inside* the conditional. `?.` currently rejects a call with a clear message. |
| Named arguments | Interacts with the same arity machinery as defaults. |

Of these, default parameters is the one users will ask for first, and it is the
one that cannot be done in the lowering pass alone.
