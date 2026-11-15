# Self-hosting bootstrap ADR (SH)

Status: **Adopted** (2026-11-14). This is the architecture decision record for
making Spid3r compile its own compiler, standard library toolchain and core
build tools through the LLVM Native AOT backend. It defines the vocabulary the
roadmap (`full_plan.html`) uses: every roadmap checkbox cites one of the
decision IDs below, and a change to any of them is a new ledger entry, not a
silent edit.

**Progress (2026-11-15, evidence in the roadmap ledger):** the Spid3r-written
toolchain covers the lexer, parser, semantic checker (with lint warnings),
lowering, bytecode compiler + container writer, the LLVM-C codegen (with the
unboxed-i64 locals system, hybrid compares, and clean C++ resource disposal),
the build driver's link + module-flattening halves, the **Stage-1 / Stage-2
self-hosting bootstrap**, the core dev tools (`test`, `fmt`, extended
`check`, `pack`/`unpack`/`verify`/`dist`), SHA-256 runtime pinning
(`verify-runtime`, `docs/RUNTIME_ABI.md`, `tools/runtime_artifact.sh`), and the
**Phase 10.2 retirement review** (`docs/SELFHOST_RETIREMENT.md`) — 33/51 roadmap
steps (Phases 0–9 + 10.2 closed; 10.1 verified locally on Windows via
`tools/release_selfhost.sh`, with GitHub CI Linux `aot` + 4-tarball
`release.yml` verification awaiting push and `workflow_dispatch`/tag run).
`spid3r build selfhost/main.matrix -o selfhost/main_native.exe` builds Stage 1
(`selfhost/main_native.exe`), `selfhost/main_native.exe build
selfhost/main.matrix` builds Stage 2 with zero Rust compiler invocation, and
Stage 2 compiles Stage 3 emitting a byte-identical COFF `spid3r_main.o`
(SHA-256 `03e10595...ce4d2fc` on the Phase 10 17-file closure). Three-way
frozen-corpus parity across Stage 0, Stage 1, and Stage 2
(`tools/selfhost_bootstrap_parity.sh`: `pass=124 fail=0 skip=10 obj_match=111
obj_mismatch=0`, PE metadata `ok=226 fail=0`) and Tier-B performance (`0.996×`
Stage-2 compute geomean vs Stage 0) pass alongside all Phase 8/9 parity
harnesses (`selfhost_test_parity.sh` 10/10, `selfhost_fmt_check_parity.sh` 17/17
+ 134/134 corpus byte-identical, `selfhost_pkg_parity.sh` 11/11,
`selfhost_runtime_parity.sh` 16/16). Next real work: push Phase 10 to verify
`ci.yml` + `release.yml` (closing `10.1`), then Phase 11.

The ground rule that governs all of it lives in the roadmap and is repeated
here because everything else depends on it: **a checkbox may be checked only
when its Gate has actually run and passed on the real repository, with the
evidence (date, command, result) appended to the roadmap's progress ledger.**
No fake implementations, no fake results, no speed claim without a benchmark
under the phase0 protocol.

## The bootstrap chain (SH-1)

Three stages, exactly as in GCC / Go / Rust's own histories:

```
stage 0  spid3r (Rust)          the compiler that exists today; builds anything
   │
   ▼
stage 1  spid3r-self            selfhost/*.matrix compiled BY stage 0's AOT
   │                            backend; a native executable, not an interpreter
   ▼
stage 2  spid3r-self-2          spid3r-self compiles its OWN sources again;
                                the fixed point — must behave identically
```

- **Stage 0** is today's `spid3r` binary: the Rust implementation in `src/`.
  It stays in-tree, pinned by tags, for the whole program. It is both the
  bootstrap and the **parity oracle**: every self-hosted subsystem is compared
  against it, never against a re-implementation's own idea of correctness.
- **Stage 1** is the Spid3r-written compiler: sources in `selfhost/`, built
  with `spid3r build selfhost/main.matrix`. It links the prebuilt runtime
  staticlib (SH-6) and drives LLVM directly through the C API (SH-7).
- **Stage 2** is the fixed point: `spid3r-self build selfhost/main.matrix`
  producing a binary that also works. The three-way comparison
  (stage-0-built vs stage-1-built vs stage-2-built outputs) recorded with
  checksums is the evidence that the chain is closed.

## What "no Rust" means, and when (SH-2)

The phrase is used precisely, per milestone:

| Milestone | "No Rust" means |
|---|---|
| Phase 7 (self-compile works) | building a **compiler** no longer requires invoking cargo/rustc — `spid3r-self` is produced by Spid3r tooling alone |
| Phase 10 (release switch) | the **release artifacts** (tarballs, installer contents) are produced by the self-hosted toolchain; stage 0 leaves the build path |
| Phase 9 (decision point) | rebuilding the **runtime itself** may remain a stage-0-produced, checksummed binary artifact — that is an explicit, recorded decision, not an oversight |

Rust never disappears from the repository. It remains forever as the stage-0
bootstrap and the semantic oracle — the same role GCC's original C sources or
Go's `gc` bootstrap play in their histories. What ends is the *requirement*
that future compiler work begins with a Rust toolchain.

Two things are deliberately excluded from "no Rust" forever: the Rust-side
developer tools (LSP, GUI panel, agent, debugger, REPL — owner-confirmed
scope, porting them is an optional late phase), and the harness scripts under
`tools/` (Python/bash test and benchmark tooling; they verify the toolchain,
they do not ship in releases).

## Source layout (SH-3)

Self-hosted sources live under `selfhost/`, one directory per Rust subsystem,
mirroring `src/`'s boundaries so the parity mapping stays mechanical:

```
selfhost/
  token.matrix         Phase 1 — the token model (port of src/lexer/token)
  lexer.matrix         Phase 1 — the scanner (port of src/lexer; token parity)
  lexer_json.matrix    Phase 1 — token-stream JSON (the parity surface)
  ast.matrix           Phase 2 — the AST model + dump serializer
  parser.matrix        Phase 2 — the grammar (port of src/parser; AST parity)
  semantic.matrix      Phase 3 — the checker (port of src/semantic; registry parity)
  lower.matrix         Phase 4 — the lowering pass (port of src/lower)
  vmc.matrix           Phase 4 — the bytecode compiler (port of the VM's compiler)
  bytecode_matrix      Phase 4 — the v3 checksummed container writer
  llvm.matrix          Phase 5.1 — the LLVM-C extern surface (SH-7) + emission
  codegen.matrix       Phase 5.2 — the codegen (port of src/native/ir.rs):
                       the SLOW path plus the unboxed-i64 locals system and
                       the hybrid compares; driven by `main.matrix native`
                       (parse → check → lower → LLVM-C codegen → verify →
                       emit → link.matrix)
  link.matrix          Phase 6.1 — the build driver's linking half (MSVC)
  flatten.matrix       Phase 6.2 — AOT module flattening (plain/aliased/
                       selective imports; __mod_/__sel_ mangling; the
                       no-leak export rule)
  link_test.matrix     the 6.1 gate harness (exe-exits-42 proof)
  main.matrix          entry: parity surfaces + `compile` (bytecode) + `native`
  roundtrip_*.matrix   the model round-trip gate tools (2.1)
  corpus/              Phase 0 — the frozen differential corpus (SH-4)
```

Amendment (ledger 2026-11-14, Phase 2): the original sketch had one
directory per subsystem, but Spid3r module imports resolve against the
importer's directory and reject parent escapes (`../` is a rejected spec) —
a multi-root tree cannot import across its own subdirectories. The subsystem
separation lives in the module boundaries instead: one module root, one file
per Rust subsystem, entry points at the root. The parity mapping to `src/`
is unchanged.

Each phase ships working code: sources are compiled and exercised by stage 0
(`spid3r build` / `spid3r run`) from the moment they exist, so a phase's
checkboxes move only when its gates have run against real binaries.

## The frozen corpus contract (SH-4)

Phase 0 locks `selfhost/corpus/` — the exact program set every parity gate in
Phases 1–7 runs against:

- the 24 runnable `examples/*.matrix` programs,
- the 87 engine-matrix programs (extracted from `tests/engine_matrix.rs` into
  files, so the corpus is data, not Rust source),
- a hand-built edge corpus (closures, try/finally, integer edges, unicode,
  OOP, concurrency, namespaces),
- negative programs covering every error diagnostic class.

A committed SHA-256 manifest pins the set. **Changing the corpus requires a
new ledger entry stating why** — a gate may not silently start testing
different inputs than the ones its evidence cites. A CI step re-verifies the
manifest so drift fails the build.

## Parity discipline and oracle tooling (SH-5)

Stage 0 is the oracle; the Spid3r-written front end is compared to it, dump by
dump, on the frozen corpus. To make that possible stage 0 grows a small,
explicitly-marked dump surface (`spid3r tokens --json`,
`spid3r parse --dump-ast-json`, diagnostic dumps): bootstrap tooling whose
only consumers are the parity harness and CI. It never changes user-visible
behavior, and its exit codes follow the standard 0/1/2/4 contract
(`README.md` "Exit codes"). The tree interpreter remains the semantic oracle
throughout (docs/ENGINE_CONTRACT.md); `tools/gate.sh` 9/9 and
`tools/difftest.sh` ALL MATCH must stay green after every merged step.

A parity claim is only ever made against the frozen corpus (SH-4) with both
sides' evidence in the ledger. Differential-fuzz results (≥1,000 cases,
zero differences) are ledger-recorded alongside corpus parity, and every
parity harness is teeth-proven: deliberately broken once, shown RED, restored.

## The runtime artifact (SH-6)

Self-hosted compilers link the existing runtime as a **prebuilt, versioned,
SHA-256-pinned staticlib artifact** (`spid3r_lang.lib` / `libspid3r_lang.a`,
with legacy fallback names `spid3r.lib` / `libspid3r.a`: the 66 AOT trampolines
plus the 229 stdlib natives). This is what makes "compilers need no Rust"
reachable in one program: rebuilding a compiler never rebuilds the runtime it
links. The full C ABI and native table specification live in
`docs/RUNTIME_ABI.md`; `tools/runtime_artifact.sh` packages the versioned
release tarball (`spid3r-runtime-<version>-<target>.tar.gz`) together with its
`.sha256` and `runtime-manifest.json` provenance (`version`, `git_commit`,
`llvm_version`, `rustc_version`), and `selfhost/link.matrix` verifies the
SHA-256 pin before invoking the linker (exiting `4` cleanly on any mismatch
or malformed pin, SEC-B).

### Phase 9.2 ADR decision — keep trampolines in Rust (do not port `rt.rs`)

Phase 9.2 evaluated porting the ~2.2k-line trampoline layer (`src/native/rt.rs`)
into Spid3r while keeping the scheduler and stdlib in the shared staticlib, and
**explicitly decided against porting** (owner-confirmed 2026-11-15):

- Every trampoline in `src/native/rt.rs` directly manipulates Rust internal
  types that remain in `spid3r_lang` (`Value`, `Box::into_raw`,
  `vm_eval_binary`, `vm_eval_unary`, `value::collections`,
  `stdlib::builtin_defs()`, `Scheduler`, and thread-local `THROWN`).
- Porting `rt.rs` into Spid3r would force inventing and maintaining a second
  low-level C ABI just to wrap those Rust internals, still linking the same
  staticlib artifact while adding indirection and risking semantic drift from
  the VM/tree engines (`docs/ENGINE_CONTRACT.md`).
- Keeping `src/native/rt.rs` inside the versioned, SHA-256-pinned runtime
  staticlib satisfies "no Rust to build compilers" with one semantic source of
  truth and verified checksum pinning (`docs/RUNTIME_ABI.md`).

## The LLVM-C boundary (SH-7)

The AOT backend needs raw pointers across `extern fn` (it builds IR, not
arithmetic), so Phase 0 first extends the FFI with `ptr` and `usize` boundary
types — an opaque pointer as an i64-encoded handle, sandbox-unware by
construction because FFI is tier T5 trusted code (docs/FFI.md). The
self-hosted codegen (Phase 5) then drives the LLVM C API directly through
that `ptr` FFI: inkwell is a Rust crate and cannot come along. The exact
LLVM-C subset is enumerated from stage 0's inkwell call sites and kept in one
auditable `selfhost/llvm/` module.

## Rollback and retirement (SH-8)

Stage 0 is never removed while the program runs. Every phase leaves the tree
in a state where `tools/gate.sh` passes 9/9 and `tools/difftest.sh` reports
ALL MATCH — so the rollback story at any point is: `git revert` the offending
commits and rebuild with stage 0, exactly as today. Phase 10's retirement
review records what still requires Rust and pins the last fully-Rust-built
release by tag before the pipeline switches.

## Decision index

| ID | Decision |
|---|---|
| SH-1 | the stage-0 / stage-1 / stage-2 bootstrap chain and the three-way fixed-point evidence |
| SH-2 | the per-milestone meaning of "no Rust", and what stays Rust-side by scope decision |
| SH-3 | `selfhost/` layout, one directory per subsystem, parity mapping to `src/` |
| SH-4 | the frozen corpus + SHA-256 manifest; corpus changes need a ledger entry |
| SH-5 | parity discipline: stage-0 dumps as bootstrap-only oracle tooling; teeth proofs |
| SH-6 | prebuilt checksummed runtime staticlib; runtime port is Phase 9's decision |
| SH-7 | `ptr`/`usize` FFI boundary types as the LLVM-C bridge; T5 trust tier |
| SH-8 | stage 0 stays until Phase 10; gate.sh/difftest.sh green after every step |

## Governing documents

docs/ENGINE_CONTRACT.md (the three-engine bar) · docs/NATIVE.md (AOT subset,
memory model, exit codes) · docs/RUNTIME_ABI.md (pinned staticlib ABI,
provenance & native table) · docs/SELFHOST_RETIREMENT.md (Phase 10.2
retirement boundary, rollback tags & maintenance rules) · docs/FFI.md
(extended by SH-7) · docs/GC.md · docs/SECURITY.md ·
docs/PERF_TARGET_RETIREMENT.md (target discipline) · full_plan.html (the
roadmap and the append-only evidence ledger).
