# Crate public-API boundaries (ARC-04)

Status: **Adopted** (2026-10-22). Today the project is one crate, so these
boundaries are a *convention* enforced by review; at the workspace split
(ADR-002) they become *cargo-enforced* privacy. This document is the contract
the split implements — write code against it now and the extraction is
mechanical.

Legend per subsystem: **Exposes** = the only symbols other subsystems may
call; **Forbidden** = cross-talk the review (and later cargo) rejects.

## Dependency direction (downward only)

```
cli ──▶ lsp ─▶ frontend(+semantic)          cli ──▶ pkg ─▶ stdlib(net)
 │              │                            │
 └─▶ codegen-* ─┴─▶ vm ──▶ ir ◀── frontend   └─▶ runtime ─▶ {vm, value, frontend}
                       ▲        ▲
                       └── value (depends on no project crate)
```

Nothing may depend on `cli`. `value` depends on nothing.

## Per-subsystem contract

### frontend (lexer, parser, ast, semantic, lower)
- **Exposes**: `lex`, `parse`, `Program`/`Stmt`/`Expr`/`FnProto`/`Span`,
  `check_file_with_warnings` / `check_file`, `Diagnostic`, `lower`,
  `program_needs_lowering`, `format_source`.
- **Forbidden**: knowing that engines exist. No `Value` in public signatures
  beyond diagnostics; no imports from `vm`, `native`, `sched`, `stdlib`.
- Note: expression-local rewrites (interpolation, bitwise) currently happen in
  the parser by design (they produce no AST node); ARC-03 moves them into
  `lower` — until then this is a documented exception, not license for more.

### ir (Chunk)
- **Exposes**: `Chunk`, `OpCode` (+ `from_u8`, `operand_width`), the
  constant-pool types, `validate_chunk`, `serialize_vmfunc` /
  `deserialize_vmfunc`, the optimizer entry, disassembler.
- **Forbidden**: depending on `value`'s collection internals (constants are
  int/float/string only); importing frontend types. Span tables are plain
  data.

### value
- **Exposes**: `Value`, `collections` (construct/method/index/equality/
  display/deep_clone/drop_iteratively), `gc_*`, `type_of`. This is the
  semantic source of truth — engines *must* delegate here rather than
  reimplement.
- **Forbidden**: depending on any other project crate; taking locks on engine
  data; engine-specific behavior branches.

### vm
- **Exposes**: `compile_program_unoptimized` (AST→Chunk), `run`/`run_program`,
  the debugger hooks, `Phase::Vm` diagnostics (malformed bytecode only —
  execution failures are `Phase::Runtime`).
- **Forbidden**: exposing `CallFrame`/slot internals to callers; a second
  implementation of any collection method.

### codegen-llvm / codegen-wasm
- **Exposes**: `build_module` / `compile_program` and an entry-point runner;
  nothing else.
- **Forbidden**: importing the AST (after ARC-05 lands, these consume `Chunk`
  only); re-implementing data semantics (callbacks go through the shared
  `value` layer / callback hosts).

### runtime (sched, environment, modules, embed, metering)
- **Exposes**: the scheduler API (`spawn`/join/channel/mutex primitives),
  `Environment`/`FunctionData`/`ClassData` (GC-registered), module resolution,
  `embed::Engine`, and `Engine::deny` for capability policy.
- **Not provided**: resource budgets. There is no instruction/step limit and no
  memory ceiling; a script can loop or allocate until the host stops it. Wrap the
  run in a watchdog (a thread with a deadline, or a process you can kill). This
  line previously claimed "resource budgets", which was never implemented
  (audit F-17).
- **Forbidden**: reaching into `vm` frames; the GC calls *into* `value`, never
  the reverse (see ADR-002 cycle note).

### stdlib
- **Exposes**: `native_defs()` and the native function table; module-level
  `.matrix` sources.
- **Forbidden**: engine dispatch (it receives `Value`s and a context, not an
  engine); diagnostics that disagree with the shared `Diagnostic` type.

### pkg
- **Exposes**: manifest parse, `install`/`vendor`/`pack`, `validate_git_url`,
  lockfile, tar+sha256 helpers.
- **Forbidden**: executing user code; the only subsystem allowed to touch the
  network/filesystem beyond stdlib's defined natives.

### lsp
- **Exposes**: the JSON-RPC server loop over stdio.
- **Forbidden**: its own lexer/parser/checker/formatter — it reuses
  `frontend`'s (one checker ⇒ the editor cannot disagree with `spid3r check`).

### cli
- **Exposes**: `main`; `run()` subcommand dispatch; `HELP`.
- **Forbidden**: logic. Subcommand bodies stay thin wrappers over the crates
  above; anything testable lives one level down.

## Enforcement plan

1. Now: this document + review. New `pub` items outside these surfaces need
   justification in the PR description.
2. At the split: each crate gets a `lib.rs` that `pub use`s only the Exposes
   list; `#![deny(missing_docs)]` on the public surface; `cargo modules`
   / `cargo-deny`-style graph check in CI asserts the dependency direction.
3. The embed facade (`spid3r_lang`) re-exports only: `embed::Engine`,
   `Diagnostic`, the `Value` type, and the checker entry points.
