# Stage 7 — lint warnings

Date: 2026-10-21 · Toolchain: rustc/cargo 1.99.1 (pinned) · Base: `bcf900b`
Scope: the checker could only fail a build. There was no way to say "this is
suspicious" without rejecting the program.

```
warning[semantic]: unused parameter 'req'
  --> app.matrix:13:16
   |
13 |     app.use(fn(req) {
   |                ^^^
```

## 1. Four lints

| Lint | Reports | Notes |
|---|---|---|
| `unused-variable` | a local that is never read | `_`-prefixed names are exempt |
| `unused-parameter` | a parameter that is never read | same exemption, which is the callback convention |
| `unused-import` | an import whose names are never used | the only lint that fires at top level |
| `unreachable-code` | a statement after `return`/`throw`/`break`/`continue` | reported once per block, not once per statement |

## 2. How it is wired

`Diagnostic` gained `severity` and `lint`. Both are one-byte enums and fit in the
padding after `phase`, so the struct **did not grow** — which matters, because
the previous stage established that its size is multiplied by the recursion depth
of everything that propagates a `Result`.

`check_file_with_warnings` returns the warnings; `check_file` stays
validity-only, so the VM, the AOT backend and the module loader are untouched and
no existing caller had to change.

Warnings are sorted by position before returning: they surface *inside-out* as
scopes close, so the raw order would read 11, 5, 1 instead of 1, 5, 11.

## 3. The policy decisions

**Scope 0 only reports unused imports.** A private top-level `let` may be used
from another module, and a public one is an export, so "unused" there is not
knowable from one file. Imports are local to the file, so they are safe to
report.

**`_`-prefixed names are always exempt.** A callback must match the signature the
framework calls it with; ignoring an argument is normal. This is the Rust/Dart
convention, and the examples now demonstrate it.

**A failing file reports no warnings.** `check_program` returns on the first
error, so warnings are only collected from a program that checks. Mixing
"this is broken" with "this is unused" makes the broken part harder to see.

## 4. The false positive this caught in its own tests

The first version reported `let f = add; ... f(1, 2)` as an unused variable. The
`Expr::Call` check arm looked the callee up in `funcs`/`lookup` but never
*visited* it, so `mark_used` never ran for the most common way a closure is
used — calling it.

That would have made the lint useless for exactly the code it should help with.
Fixed by marking the callee used in that arm; `calling_a_closure_counts_as_using_it`
pins it.

The same run found two real issues in the example corpus:

- `examples/functions.matrix` — both flagged bindings *were* used, which is how
  the bug above surfaced.
- `examples/web_service.matrix` — three of four route handlers genuinely ignore
  `req`. They are now `_req`, which both silences the lint and documents the
  convention to anyone reading the example. The fourth handler reads
  `req.params["id"]` and kept its name.

After the fix the whole 25-file example corpus is warning-free.

## 5. Gates

| Gate | Result |
|---|---|
| `cargo test --no-default-features` | **577 passed, 0 failed** |
| `spid3r test` (three-engine differential) | **25 ok, 0 failed** |
| example corpus, `check` on every file | **0 warnings** |
| clippy `-D warnings` (core + native) | 0 |
| `cargo fmt --all -- --check` | clean |

Canonical counts: **593 tests** (577 core + 10 GUI + 6 AOT).

New coverage: `tests/stage7_lints.rs` (9).

## 6. Deliberate limits

| Not linted | Why |
|---|---|
| Shadowing | A same-scope redeclaration is already an *error*; shadowing an outer scope is idiomatic and warning about it is noise. |
| Unused function / class | Needs a whole-program reachability analysis (a `public` one is an export, and a dynamic language can call anything by name). |
| Write-only variable (`x = 1` but never read) | The assignment target is visited as an expression, so it currently counts as a use. Fixing it means distinguishing lvalues from reads in `check_expr`. |
| Unreachable after an `if` where every branch returns | Requires control-flow analysis; only direct terminators are handled. |
| Warnings from `run` | `run` compiles and executes; warnings would interleave with program output. `check` is the lint entry point. |
