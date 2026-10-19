# fix_repl_cli.md — F7 + F11 + F13 (REPL/CLI fix trio)

Date: 2026-10-19. Scope: only `src\repl\mod.rs`, `src\cli.rs` (fmt handling), new test file
`tests\fix_repl_cli.rs`. Interpreter, semantic, value/*, Cargo.toml, docs, existing tests untouched.

## Status

- [x] Skeleton written before any code change.
- [ ] F7: `:reset` rebuilds full per-session state (work in progress).
- [ ] F11: bare expressions may omit the trailing `;`.
- [ ] F13: `fmt` expands directory arguments.
- [ ] Tests: `tests\fix_repl_cli.rs`.
- [ ] Verification: build + tests + clippy, oracles `fmt_tests`, `debug_tests`.

## F7 — REPL `:reset` clears only the name list (P2)

- Symptom (frontend_audit.md #3): `const c=1;` → `:reset` → `let c=2;` → `c=3;` →
  `error[runtime]: cannot assign to const 'c'`.
- Root cause: const names live in `EnvironmentData.consts` (`src\environment\mod.rs:25`,
  filled by `define_const`); the REPL shares that env via `interp.globals.clone()`
  (`src\repl\mod.rs`, startup). `:reset` only did `known.clear()` (semantic layer), so the
  runtime const set survived.
- Plan: extract `fresh_session()` building `(Interp, Env)` exactly as startup did; `:reset`
  reassigns both and clears `known`. Startup and `:reset` cannot drift.
- Decisions: keep the `bindings cleared` message and `:help`/`:quit` behavior unchanged.

## F11 — REPL bare expressions require `;` (P3)

- Symptom (frontend_audit.md #4): `2 + 2` errors although `is_bare_expr` says the trailing
  `;` is optional echo noise.
- Plan: inside `eval_line`, when `parser::parse` fails AND `is_bare_expr(src)` AND the source
  does not already end with `;`, retry the parse once with `;` appended to the trimmed source.
  If the retry also fails, report the ORIGINAL diagnostic (accurate error for broken input).
  Retry triggers only on parse failure, so nothing is ever executed twice.
- Unaffected by design: declarations/statements (`is_bare_expr` excludes them), multi-line
  input (buffer balanced first, retry operates on the whole buffer), error reporting.

## F13 — `fmt` does not expand directory arguments (P3)

- Symptom (test_results.md 5d): `spid3r fmt --check examples` → exit 2
  `cannot read 'examples': Access is denied. (os error 5)`.
- Plan: in `cmd_fmt`, after arg collection, expand each arg that `is_dir()` in place into its
  `*.matrix` files (`read_dir`, files only, extension == "matrix", sorted by name,
  non-recursive), then run the unchanged per-file loop. Directory with no `.matrix` files →
  clean usage error (exit 2). Non-directory args keep the exact old behavior.
- Decisions: usage-error message `no .matrix files found in directory '<arg>'`; HELP text left
  as `[files...]` (minimal diff; noted here).

## Tests (tests\fix_repl_cli.rs)

Std-only, `CARGO_BIN_EXE_spid3r`, piped stdin for the REPL, all CLI invocations guarded by
`try_wait` polling with a 15 s deadline (kill + panic, never hang).

- (a) F7 sequence `const c=1; / :reset / let c=2; / c=3; / c` ends with echo `3`, stderr clean.
- (b) `2 + 2` echoes `4`; `2 + 2;` echoes `4`; `let x = 5` (no `;`) still errors; `2 +` still
  errors; multi-line bare expr without final `;` echoes.
- (c) `fmt --check <dir>`: 2 formatted files → `unchanged` x2, exit 0, sorted, non-recursive
  (nested dir + .txt ignored); empty dir (no .matrix) → exit 2 clean usage error; dir + file
  arg mixed works; single FILE behavior unchanged (unchanged/would reformat/rewrite + exit
  codes as before).

## Verification log

- (to be appended with exact commands and counts)

## FINAL STATUS (orchestrator integration, 2026-10-19)
- All three fixes implemented and verified (repl/mod.rs fresh_session() shared by startup and :reset; eval_line semicolon-retry for bare expressions; cli.rs cmd_fmt directory expansion).
- Tests: tests/fix_repl_cli.rs 11/11 green (F7 reset sequence, F11 echo cases incl. multi-line and broken-input reporting, F13 dir expansion/empty-dir/mixed args/single-file unchanged).
- Orchestrator re-verification: `spid3r fmt --check examples` (directory form) now exits 0 with all 23 examples unchanged.