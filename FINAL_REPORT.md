# Spid3r — Final Verification Report

*Consolidated after the completion of the 18-phase plan (see `docs/ROADMAP.md` for
per-phase status and `plan.txt` for the completed-phase plan). Every claim below
is backed by the test suite (226 Rust tests) and the three-engine differential
run over all shipped examples. No stubs, no fakes — features were exercised, not
asserted from reading code.*

## Language and engines

- **Tree-walk interpreter** (reference "oracle") and **bytecode VM** produce
  identical behaviour: `spid3r test` runs all 23 shipped `examples/*.matrix`
  through `run --engine tree`, `run --engine vm`, and `compile`+`exec`,
  requiring identical stdout — **23 ok, 0 failed** at the time of writing.
- Full language surface: closures/upvalues (including capture across loops),
  classes with single inheritance, interfaces, privacy (`public`/private),
  `try`/`catch`/`finally` with rethrow, `const` enforcement (including
  imported `public const`), 20+ collection types with higher-order methods
  (`map`/`filter`/`reduce`…), rich string methods, `spawn`/channels with
  synchronous callbacks routed through the VM's own code paths.
- Modules: `import "path"` resolves next to the importer and through module
  directories; modules are semantically checked with their real path;
  `public const` exports are immutable in the importer.

## Bytecode pipeline

- Compiler (AST → `Chunk` with constant/function/class pools and per-instruction
  spans), VM with safe opcode decoding (`OpCode::from_u8`, no `unsafe
  transmute`) and `validate_chunk` (rejects invalid opcodes, truncated
  instructions, out-of-range pool references, misaligned jump targets — each
  with unit tests).
- **Self-contained `.matrixc`**: imports are recursively compiled and embedded,
  so `exec` runs with no `.matrix` source on disk (proved by deleting the
  imported file and running). Serialize/deserialize plus a disassembler.
- **Optimizer** (`src/vm/opt.rs`): constant folding through the interpreter's
  own helpers (so results *and* errors match exactly — `1/0` is never folded),
  peephole (`Jump +0`, `<push>; Pop`), branch folding (static conditions
  collapse; `not not x` becomes `Truthy`), dead-code removal (skipped for
  chunks containing `try`). Verified by an opt-vs-`--no-opt` differential over
  every example; corpus bytecode 6.9% smaller.
- **VM performance**: measured, evidence-backed. Arithmetic ≈ **8.5M iterations/
  s** (~11ns/instruction); plain call ≈97ns, method call ≈346ns (2M-call
  micro-benchmarks). Three contained optimization formulations were
  implemented, measured, and two reverted as neutral-or-negative (see
  ROADMAP); the remaining headroom needs a redesign (register VM / threaded
  dispatch), recorded as closed.

## Tooling

- **`spid3r fmt`** — token-stream formatter with two machine-checked
  invariants (identical re-lexed token stream; idempotence), comment and
  literal preservation, and line wrapping for over-wide parenthesised groups.
- **`spid3r debug`** — line debugger over the VM: breakpoints, step/next/
  finish, backtrace, locals, source listing; fully scriptable via
  `--commands FILE` (7 tests drive it, including closure-churn cache-safety).
- **Runtime error traces** — uncaught errors and `throw` on both engines print
  a call stack (`at f (file:line:col)`).
- **REPL**, differential `test`, benchmark harness (`spid3r bench [--compare]`,
  best-of-N, opt/`--no-opt` switchable).
- **Gradual typing (PH-4)**: `spid3r check` enforces optional `: Type`
  annotations on `let`/`const`, parameters, return types (`fn f() -> int`), and
  class fields — a best-effort static type is computed per expression and
  mismatches (assignment, call-argument, return, field-write) are rejected at
  compile time. Unannotated names stay `Any` (dynamic), so existing programs are
  untouched. Subtype/implements-aware; `Int` widens to `Float`; `any` opts out.

## Projects and dependencies (`spider.toml`)

- `init` (scaffold + `.gitignore`), `install` (path and **git** dependencies —
  cloned, checked out at the declared `rev`, recorded as `git+url#<commit>` in
  the lockfile), **version constraints** (`^`/`>=`/`=`/exact, checked against
  the dependency's own manifest), deterministic **`spider.lock`** with SHA-256
  over vendored contents (hand-written SHA-256, tested against the standard
  vectors).
- **Workspaces**: `[workspace] members = […]`, one shared `vendor/` + lockfile,
  name-conflict detection across members, `used-by` provenance in the lock,
  and ancestor walk-up so member directories work transparently.
- **Distribution**: `dist` (self-contained `.matrixc`), `pack`/`unpack`
  (deterministic uncompressed ustar tar, path-traversal-guarded extraction,
  SHA-256 printed) — pack a library, unpack beside an app, depend on it, run.
- **Project-aware CLI**: `run` (no file) runs the manifest entry; `test`
  differentially runs `tests/*.matrix` (workspace roots iterate members);
  `assert`/`assert_eq` builtins make failing tests self-explanatory.

## Known limitations (honest)

- **Reference cycles:** `list`/`vec`, `set`, `map`, and `instance` cycles are
  collected (v1.1) — see `docs/GC.md`. `gc_collect()` / `gc_live_cells()` are
  builtins; statement-boundary auto-collection is **on by default** (`SPID3R_GC=0`
  disables it). Cycles through trees/graph/heap/matrix etc. still leak until the
  trace is widened; `weak`/`deref` remain the manual escape hatch. (The VM has no
  auto-trigger yet — `gc_collect()` works there — to avoid regressing its
  throughput.)
- The native AOT backend (`native` feature) does not yet compile a class
  declared inside a function (rejected with a clear diagnostic — run those
  programs with `--engine vm`). Everything else works in compiled code:
  `try`/`catch`/`finally`/`throw`, `import`, closures, classes, higher-order
  methods, and `spawn`/`async`/`await` (tasks run on the shared worker pool) —
  22 of 23 examples match the interpreter. The native backend also needs an
  LLVM-enabled build.
- **Dependencies** are path, git, or **local registry** (`{ version = "…" }`
  resolved from `$SPID3R_REGISTRY`, an offline directory of `<name>/<version>/`
  packages — see `docs/REGISTRY.md`). There is no *remote* HTTP registry, and
  registry packages' own dependencies are not resolved transitively (declare
  what you use). Distribution is also by tarball (`pack`/`unpack`).
- **GUI testing:** the native window (tao/wry + WebView2) is not auto-tested —
  headless webview automation is not feasible on Windows/CI here, so it is
  manual. Everything under it is tested headlessly and hardened: the IPC decode
  (`JSON.stringify` body → `json_field` → dispatch), JSON escaping/round-trips,
  event ordering, unknown-event dropping, payload fidelity, and
  `gui_set_html`/handler-queued JS (`src/gui/bridge.rs` unit tests +
  `tests/gui_tests.rs` `--dry-run` replay).
- `Value::Str` is `Arc<str>` and the VM is safe Rust with bounds-checked
  indexing: further VM speed would require a redesign (register VM / threaded
  dispatch), recorded in ROADMAP as closed-with-evidence.

## Verification summary

| Check | Result |
|---|---|
| Rust test suites | **226 passed, 0 failed** |
| Example differential (tree == vm == exec) | **23 ok, 0 failed** |
| Native AOT differential (VM vs `spid3r build`) | **22 pass, 0 fail, 1 skip** |
| Strict clippy (`--all-targets`, `-D warnings`) | clean |
| Full gate (test+clippy × 4 feature configs + release build) | **PASSED 9/9** |
