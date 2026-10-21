# Stage 1 — one front end: the lowering seam + the conformance matrix

Date: 2026-10-21 · Toolchain: rustc/cargo 1.99.1 (pinned) · Base: `fd0dd15`
Scope: introduce the seam every later syntax feature is written against, and a
test that makes engine divergence impossible to miss. No new language syntax.

## 1. Why this stage exists

Every language feature currently has to be implemented three times — tree
interpreter, bytecode compiler, AOT lowering — because each consumes the parser's
AST independently. `grep -r "fn desugar\|fn lower" src/` returned nothing.

`src/lower/` is that missing stage. Today it is the identity:

```rust
pub fn lower(program: &Program) -> Cow<'_, Program> {
    Cow::Borrowed(program)
}
```

`Cow` is deliberate: the identity case allocates nothing, and only a rule that
actually rewrites pays for a clone. The point is not the function body; it is the
**invariant** that no backend sees the parser's output directly.

| Backend | Where it lowers |
|---|---|
| tree interpreter | `Interp::run_program_inner` |
| imported modules | `modules::load` (a module is a program too) |
| bytecode VM | `compiler::compile_program_unoptimized` (covers `--no-opt` and the optimizer's own tests) |
| LLVM AOT | `native::ir::build_module` |

Contract, written in the module docs: **idempotent** (the VM compiles module
programs through the same entry, so a program may arrive already lowered),
**span-preserving** (diagnostics must keep pointing at what the user wrote) and
**behaviour-preserving**.

`spid3r check FILE --dump-lowered` prints the tree the backends receive, so a
desugaring rule's output is inspectable without reading engine code.

## 2. The conformance matrix

`tests/engine_matrix.rs` has 35 cases covering every `Stmt` and `Expr` variant in
`src/ast/mod.rs` (import, fn, class, interface, let/const, if, while, for-in,
for-C, return, break, continue, block, expr-stmt, spawn, try, throw; int/float/
str/bool/null, ident, list, closure, unary, binary, logical, assign, call,
method, index, field, new, await, spawn-expr).

It asserts three things the existing differential harness does not:

1. **Expected output, not just agreement.** `spid3r test` compares tree == vm ==
   exec. That catches divergence but not "both wrong". Every case here carries
   the exact expected text, so a wrong answer that all engines share still fails.
2. **Identical failure.** stderr's first line must match: same phase, same
   message, same exit code.
3. **The AOT subset** (`--features native`): one case per construct family the
   native backend claims to support — including `try/finally`, `throw` of a map
   value, `spawn`+`join`, higher-order methods, inheritance with virtual
   dispatch, and float formatting. A construct that compiles but computes a
   different answer is worse than one that is rejected.

The lowering pass itself is pinned by two assertions: it must be the identity
today (so a Stage-2 rule shows up as a deliberate test change) and it must be
idempotent.

## 3. The bug the matrix found on its first run

The same program reported a **different error phase** depending on `--engine`:

```
$ spid3r run --engine tree div0.matrix
error[runtime]: division by zero
$ spid3r run --engine vm   div0.matrix
error[vm]: division by zero
```

`VmError::to_diag` hardcoded `Phase::Vm`. `VmError` is only ever raised while
*executing* (compile-time failures return a `Diagnostic` directly), so those are
runtime errors and now report `Phase::Runtime`. `Phase::Vm` stays reserved for
malformed bytecode — verified: `spid3r exec` on a garbage `.matrixc` still says
`error[vm]: invalid file format: file too short…`.

This survived because `spid3r test` compares **stdout only**; nothing compared
stderr between engines. Both `err_index_oob` and `err_div_zero` now pin the
correct text.

## 4. Cases that documented a language constraint

Writing the matrix surfaced four things worth recording (they are constraints,
not bugs, and each is pinned by a case or a note):

| Finding | Detail |
|---|---|
| C-style `for` has an unusual shape | `for let i = 0; i < 4; i = i + 1 { }` — `let` after `for`, no parentheses around the header. `for (let i = 0; …)` is a parse error. |
| Interface methods must take `self` | `fn name(self) -> string;` — `fn name() -> string;` is rejected with "interface method 'name' must take self". |
| `await` is only valid inside `async fn` | and an `async fn` called from sync code returns its value directly rather than a task, so `await` is only useful on `spawn` tasks and channels. |
| Same-scope shadowing is rejected | `let x = 1; let x = x + 1;` → `error[semantic]: 'x' is already declared in this scope`. |
| `reduce` takes the initial value first | `xs.reduce(0, fn(a, b) { … })`; passing the function first reports "reduce expects a function, found int". |

These are roadmap input: shadowing and `await` placement are ordinary ergonomics
gaps, not design choices a user would defend.

## 5. Gates

| Gate | Result |
|---|---|
| `cargo test --no-default-features` | **507 passed, 0 failed** |
| `cargo test --features native --test engine_matrix` | 4/4 (AOT subset included) |
| GUI / AOT suites | 10/10, 6/6 |
| `spid3r test` (differential) | **25 ok, 0 failed** |
| clippy `-D warnings` (core + native) | 0 |
| `cargo fmt --all -- --check` | clean |

Canonical counts (`tools/count_tests.sh`): **523 tests** (507 core + 10 GUI +
6 AOT).

## 6. What Stage 1 does *not* do

- It does not add syntax. `lower` is the identity, by design; the matrix proves
  the pipeline is unchanged, which is what makes Stage 2's first desugaring rule
  a small, reviewable diff.
- It does not make the AOT backend cover the full surface. `interface`, `for-in`
  over a map, and `async`/`await` are marked `aot: false` in the table because the
  backend does not claim them; the cases still run on tree and VM.
- It does not fix the ergonomics findings in §4 — they are recorded for Stage 2/3.

## 7. Next

Stage 2 lands the first desugaring rules in `src/lower/`: expression-oriented
`if`/blocks, string interpolation, `?.`/`??`, bitwise operators (as stdlib calls),
destructuring `let`, `for (k, v)` over maps, default parameters, and `spawn` of
any callable. Each is one rule in `lower` plus one row in the matrix.
