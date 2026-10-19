# Fix notes — semantic privacy (F6) + import shadowing (F12)

Status: IN PROGRESS. Append-only log; final summary at the bottom.

## Scope

- Editable: `src/semantic/mod.rs`, `src/semantic/ty.rs` (if needed), `tests/fix_semantic_priv.rs`.
- Read-only oracles: `tests/oop_tests.rs` (13), `tests/gradual_typing_tests.rs`, `tests/const_module_tests.rs` (5), other pkg/import tests.
- Invariants: user code never panics the host; diagnostics `Phase::Semantic`, lowercase messages, spans via `err`; no Cargo.toml changes.

## Facts established (read-only recon)

- Parser forces imports before all other declarations in one file (`src/parser/mod.rs:128-142`,
  "import must appear before other declarations") and forbids nested imports. So in FILE mode the
  only possible source order is import(s) first, then locals. The reverse order (local first, then
  import) is possible in REPL chunks (`check_repl`, `src/repl/mod.rs:202`) where `known` names from
  earlier lines sit in scope[0].
- Checker has two passes (`check_program`): pass 1 pre-declares top-level `fn` names into scope[0]
  (`mod.rs` ~278) and dup-checks all top-level names; pass 2 walks statements in source order.
  Top-level `let`/`const` are NOT pre-declared; they declare when pass 2 reaches them. Top-level
  `fn` re-declaration does not happen in pass 2 (`Stmt::Fn` only declares when `scopes.len() > 1`).
- F6 hole: `Expr::Method` and `Expr::Field` arms compute `recv_class` ONLY for `Expr::Ident`
  receivers via `VarInfo.class`. `Expr::Assign` with a `Field` target has NO privacy check at all
  (not even for Ident receivers). `type_of()` (best-effort static type, used by gradual typing)
  already resolves `Expr::New` → `Ty::Class`, annotated call returns, annotated field/index reads.
- F12 hole: `Stmt::Import` inserts module public names with raw `scope.insert` (`mod.rs:~541`),
  silently overwriting a pass-1 pre-declared top-level `fn` of the same name (probe p09). A
  top-level `let`/`const` of the same name after an import currently dies with the generic
  "'x' is already declared in this scope".
- `VarInfo` is constructed at 15 sites (repl-known, pass-1 fn, import, nested fn, fn params,
  ctor self/params, method self/params, let, const, for-in var, for-c init, catch var, lambda
  params).
- Diagnostic format: `error[semantic]: <message>` (+ span lines), `Phase::Semantic` via `err`.
- CLI: `spid3r check` = parse + `semantic::check_file` (`src/cli.rs:295-307`); `run`/`compile`/etc.
  also run `check_file` via `front_end` (`src/cli.rs:180-185`) → checker rejections gate BOTH
  engines.
- Test helpers: in-process `parse_file` + `check_file` (oop_tests.rs:10-35) for rejections;
  `CARGO_BIN_EXE_spid3r` + `run --engine tree|vm` (audit_regressions.rs:23) for both-engine runs.

## F6 — privacy by resolved receiver type (design)

New helper `receiver_class(e)`:
- `Ident("self")` → `current_class` (same as the old Field-arm special case).
- `Ident(n)` → `lookup(n)` → `v.class` OR `v.ty` when it is `Ty::Class` (covers annotated consts,
  which previously set neither).
- any other expression → `type_of(e)`; `Ty::Class(c)` → `Some(c)`; else `None` (dynamic →
  unchecked, documented gradual-typing behavior).

Applied to:
1. `Expr::Method` arm — replaces the Ident-only match → privacy + arity + arg-type + no-method
   checks now run for ANY receiver whose static class resolves (`new C().hide()`, `(expr).hide()`,
   chained calls).
2. `Expr::Field` arm — same replacement → privacy + no-field checks for any typed receiver.
3. `Expr::Assign` (Field target) arm — ADDS the privacy check that was entirely missing (field
   writes). Message identical to the read arm: `field 'x' of class 'C' is private`.

## F12 — import/local collision diagnostics (design)

- `VarInfo.is_import: bool` marks bindings created by `Stmt::Import`.
- New `Checker.predeclared: HashSet<String>` records top-level fn names pass 1 declared (they are
  in scope BEFORE any import is processed, even though they come later in source order).
- `Stmt::Import`: same-name local already in the CURRENT scope (and not a pass-1 pre-declared fn of
  this chunk) → `import 'x' shadows a local binding` (this is the REPL direction: local from an
  earlier chunk). Re-import of an imported name is allowed (last import wins).
- `Stmt::Fn` (any depth): `lookup(name)` says the name is held by an import binding →
  `local 'x' shadows an imported binding` (fires for the file-mode p09 case because the import
  overwrote the pass-1 entry).
- `Stmt::Let` / `Stmt::Const` / for-c `let` init: same lookup-based check before `declare`.
- `declare()`: when the existing same-scope entry is an import binding, say
  `local 'x' shadows an imported binding` instead of the generic duplicate message (covers REPL
  pass-1 re-declaration).
- NOT flagged (per task): ordinary local shadowing (inner let over local, params, catch vars,
  for-in vars), class/interface names, import-vs-import overwrites.

## Test plan (tests/fix_semantic_priv.rs)

F6: new-expr private method rejected; new-expr private field read rejected; private field WRITE
rejected (Ident + typed non-Ident receiver); Ident-receiver regressions (p04 shapes); public
members via new-expr + Ident still run on BOTH engines; inside-class private use still legal;
chained/typed-call receivers checked; dynamic (unannotated factory) receiver stays unchecked and
runs (documented gradual behavior).
F12: import-then-local-fn rejected; import-then-local-let rejected; local-then-import rejected
(REPL direction via `check_repl_in`); unrelated names unaffected (runs both engines); two imports
exporting the same name OK; re-import OK; ordinary local shadowing still legal.

## Log

- (append as work proceeded — superseded by the orchestrator integration below)

## FINAL STATUS (orchestrator integration, 2026-10-19)
- The skeleton broke the build mid-edit (Stmt::Class arm header lost) — repaired; VarInfo.is_import threaded through all 15 construction sites; F6 (receiver_class helper + Method/Field arms + the previously-missing Assign-Field privacy check) and F12 (import/local collision diagnostics both directions, same-scope only; nested-scope shadowing stays legal) completed by the orchestrator.
- F5 (typing contract) implemented by the orchestrator on top: `Ty::Fn` now carries `Option<Arc<FnSig>>` (ty.rs); declaration names, nested fns, lambdas, and unannotated `let g = <fn value>` propagate signatures; the Call arm validates arity + annotated argument types through ANY statically-typed callee; return types flow into downstream annotations. Unannotated functions remain fully dynamic.
- CONTRACT CHANGE (documented): external writes to private fields are now rejected (`field 'x' of class 'C' is private`), consistent with the pre-existing read behavior. tests/gradual_typing_tests.rs::annotated_field_read_has_type was updated to a public field with a citation (sitemap invariant #5: documented contract change).
- Tests: tests/fix_semantic_priv.rs 19/19 green (F6 rejections incl. writes, no-regressions, dynamic-receiver boundary; F12 both directions + nested-scope legality + VM smoke runs; F5 bypasses closed + dynamic escape hatch intact).
- Full suite 340 core green (was 300), clippy -D warnings clean, 3-engine differential 23/23, GUI 10/10.