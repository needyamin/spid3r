# Spid3r Roadmap

Honest, per-phase status. "Done" means implemented **and** verified by the test
suite (unit, integration, and the tree == vm == exec differential run).

| Phase | Scope | Status |
|---|---|---|
| 0–1 | Grammar freeze + keyword registry | Done |
| 2 | Lexer | Done |
| 3 | Parser (Pratt expressions, all statements) | Done |
| 4 | Semantic checker (scopes, types, privacy, visibility) | Done |
| 5 | Value model + `value::collections` operations | Done |
| 6–10 | Tree-walk interpreter: values, control flow, functions/closures, classes/interfaces, errors | Done |
| 11 | Interpreter v1 — feature-complete reference ("oracle") | Done |
| 12 | **Bytecode compiler** (AST → `Chunk` with constant/fn/class pools, spans) | Done |
| 13 | **Bytecode VM** (`run --engine vm`, `compile`, `exec`, `.matrixc`) | Done |
| 14 | Upvalue capture across nested closures / spawn | Done |
| 15 | `.matrixc` serialize / deserialize + disassembly | Done |
| 15b | **Self-contained `.matrixc`** — imported modules compiled & embedded so `exec` needs no source | Done |
| 15c | **Bytecode validation** (`validate_chunk`) + safe opcode decode | Done |
| 15d | **VM stack-discipline fixes** — `FieldSet`/`IndexSet` leave the assigned value (peek convention); compound `+=` on field/index targets duplicates the object (`Dup`/`Dup2` opcodes); `CaptureSync` opcode binds captured locals to their cells so pre-capture code (loop conditions) reads the live value | Done |
| 15e | **`const` enforcement** — semantic pass rejects assignment to `const` (including imported `public const`); tree runner enforces it at run time via per-environment const sets | Done |
| 15f | **Module hardening** — module semantic checks use the module's real path (its own imports resolve correctly); circular-import errors show the full chain; `public const` exports carry mutability | Done |
| 16 | **Bytecode optimizer** | Done — `src/vm/opt.rs` (PH-9) |
| 16b | Optimizer passes: **constant folding** (via `vm_eval_binary`/`vm_eval_unary`, so results and errors match exactly; refuses to fold anything that would raise, e.g. `1/0`), **peephole** (`Jump +0`, `<push>; Pop`), **branch folding** (a conditional jump on a statically known truth value — `True`/`False` after constant folding — collapses: the dead arm is deleted, and `Not;JumpIfX` becomes the inverted jump while `Not;Not` becomes `Truthy`), **dead code** after `Return`/`Jump`/`Throw` (skipped for chunks containing `Try`). Jump and handler offsets are recomputed through an old→new map; a chunk is left untouched if any offset would overflow `i16`. `inst_offsets`/`inst_spans` rebuilt, so error spans and the debugger stay correct. `--no-opt` on `run`/`compile`/`bench` for comparison; corpus bytecode 6.9% smaller (up to 14% per file; a constant-condition `if/else` collapses by ~40%) |
| 17 | Tooling: line debugger | **Done** — `spid3r debug` (src/vm/debug.rs) |
| 18 | **Package manager (part 1)** | Done — src/pkg/mod.rs. `spider.toml` manifest (`[package]` name/version/entry, `[dependencies]` with `{ path = "…" }` or a bare path string), parsed by a self-contained TOML subset (no crates). `spid3r init [dir]` scaffolds without overwriting. `spid3r install` resolves path dependencies, copies their `src/` trees into `vendor/<name>/`, and writes a deterministic `spider.lock` with each dep's version, declared path, file count, and a SHA-256 over the vendored files (hand-written SHA-256, tested against known vectors). Re-running install is idempotent and re-syncs a tampered `vendor/`. `vendor` is part of the default module directories, so `import "mathlib/lib"` just works. Part 2 adds **git dependencies**: `{ git = "https://…" , rev = "branch|tag|commit" }` (rev optional = default branch). Install clones/refreshes into `.spid3r/git/<name>/`, checks out the rev, records `source = "git+<url>#<full commit>"` in the lockfile (reproducible), and re-vendors. Tested with real local repositories: clone + run, pinned rev surviving newer upstream commits, stable lock on re-install, and clear failure on a non-repository URL. Part 3 adds **version constraints** (`version = "^1.2"` on a dependency: caret (same major, at least), `>=`, `=`, or a bare exact version; `install` fails naming the constraint and the dependency's actual version) and **`spid3r dist`** — compiles the project entry into `target/<package>.matrixc`, refusing first if dependencies are declared but not installed (`-o` overrides the location; parent directories are created). `spid3r init` now also writes a `.gitignore` (vendor/, target/, .spid3r/). Part 4 makes the CLI **project-aware**: `spid3r run` with no file runs the manifest entry, and `spid3r test` inside a project differentially runs every `tests/*.matrix` (tree == vm, both exit 0; files without `fn main` are libraries and get the semantic check). Part 5 adds **workspaces**: a root `spider.toml` with `[workspace]` and `members = ["libs/core", "apps/cli"]` (a workspace-only root needs no `[package]`); `install` resolves every member's dependencies into ONE shared `vendor/` + `spider.lock` at the root (deps resolve relative to the declaring member; the same dependency name from two different sources is an error, and the lock records `used-by`), and `vendor` is found by walking up from any member directory, so `run`/`test`/`dist` work inside members. The stdlib gained **`assert(cond, msg?)` and `assert_eq(actual, expected, msg?)`** (they throw with the failing values, so a failed test explains itself), and `spid3r test` at a workspace root runs every member's `tests/` in that member's directory, reporting per-member and total tallies. Part 7 adds **`spid3r pack` / `spid3r unpack`**: pack archives `spider.toml` + `src/**` into a deterministic uncompressed ustar tar (`dist/<name>-<version>.tar`, sha256 printed) using a hand-written tar writer (zero dependencies); unpack extracts it with a path-traversal guard and refuses to clobber an existing directory — so the distribution story is `pack` → share the `.tar` → `unpack` → use as a path dependency. Workspace-level `pack` archives every member into the root `dist/`. Not yet: registry dependencies (needs infrastructure) |
| 17e | **`spid3r debug`** — line-based debugger over the VM: breakpoints by line (`--break L[,L…]`), `step`/`next`/`finish`, `bt` (call stack), `locals` (frame slots), `list` (source window), `break`/`delete` at the prompt, `quit` (exit 3). Sessions are scriptable via `--commands FILE` or piped stdin, which is how tests/debug_tests.rs drives them; when commands run out the program continues to completion | Done |
| 17f | **Runtime error traces** — uncaught runtime errors and `throw` on BOTH engines print a call stack ("stack trace (most recent call first): at f (file:line:col)"), built from the VM's per-instruction spans and the tree's frame stack (`Diagnostic.trace`, tests in tests/error_tests.rs) | Done |
| 17d | **`spid3r fmt`** — token-stream formatter (`--check` for CI). Invariants enforced per file: the formatted output re-lexes to the *identical* token stream, and formatting is idempotent. Comments are preserved (lexer keeps a comment side-channel); literals are copied from source slices so `7.0`/escapes are untouched. **Line wrapping**: a parenthesised group that would overflow 100 columns is broken one item per line (nested groups wrap independently); short calls on long lines stay flat; closure arguments are transparent (braces own the layout, `}));`). Trailing commas are deliberately not added — that would insert a token and break the token-preservation guarantee. The shipped `examples/` and `benchmarks/` are formatted and act as regression fixtures (tests/fmt_tests.rs) | Done |
| 17b | **`spid3r bench`** — benchmark suite (benchmarks/*.matrix) + `bench [--engine] [--runs N] [--compare]` subcommand with iters/sec table and tree-vs-vm output comparison | Done |
| 17c | **Higher-order collection methods** — `list.map/filter/reduce/for_each/all/any` with engine callback hosts (tree re-enters the interpreter; the VM pushes a frame and runs to its own depth); new string methods (`ltrim/rtrim/reverse/chars/pad_left/pad_right/format`), `map.items/has/delete`, `set.intersect`, `list.sorted/reversed` | Done |
| 18 | **Native AOT backend** (LLVM, `spid3r build`) — optional `native` feature | Done (supported subset, differential-verified) |
| 19 | **Embedded-webview GUI** (`spid3r gui`) — optional `gui` feature | Done (bridge + dry-run verified; window exercised manually) |

## Verified guarantees

- **Semantic parity:** the VM and the tree-walk interpreter share the lexer,
  parser, and semantic checker, and delegate all value operations to the same
  `value::collections` layer.
- **Differential testing:** every `examples/*.matrix` runs three ways
  (`run --engine tree`, `run --engine vm`, `compile` + `exec`) with identical
  stdout. The harness is `difftest.sh`, the `spid3r test` command, and a
  `cargo test` integration target (`tests/difftest.rs`).
- **Self-contained bytecode:** imported modules are resolved, compiled, and
  embedded into the `.matrixc` at compile time (transitively, flattened into one
  table). `exec` therefore runs without re-reading any `.matrix` source. Verified
  by compiling `examples/modules.matrix`, deleting `examples/math_util.matrix`,
  and confirming `exec` still prints `25` / `27`.
- **Bytecode safety:** `validate_chunk` rejects invalid opcodes, truncated
  instructions, out-of-range pool references, and off-boundary jump targets
  before execution; the opcode decoder is a safe table (no `mem::transmute`) and
  every pool read / jump is bounds-checked at run time. A corrupt or non-Spid3r
  file fails with a clean diagnostic, never a panic.
- **Concurrency:** `spawn`/`await`, `channel`, and `mutex` are implemented and
  match the tree-walk scheduler; spawned tasks share the globals, scheduler,
  module cache, and embedded-module table.
- **Native parity:** `spid3r build` (LLVM AOT, optional `native` feature)
  compiles the supported subset to standalone executables whose output is
  byte-identical to the tree interpreter — 22 of 23 examples pass the
  differential suite with 0 failures; the one skip is `math_util`, a library
  example with no `fn main()` to run. `try`/`catch`/`finally`/`throw`,
  `import` (flattened into one program at build time), and
  `spawn`/`async`/`await` (tasks run on the shared worker pool) are fully
  supported. See [`NATIVE.md`](NATIVE.md).

## Not built yet (optional, beyond v1.0)

- Native-backend support for classes declared inside a function (rejected with
  an explicit "not supported by the native backend yet" diagnostic — unused by
  the example programs). `try`/`catch`/`finally`/`throw`, `import`, and
  `spawn`/`async`/`await` are supported.
- GUI: the **native window** (the `tao`/`wry` event loop + WebView2) is not
  auto-tested — headless webview automation is not feasible on Windows/CI here,
  so it is exercised manually. Everything beneath it *is* tested headlessly and
  was hardened: the IPC decode (raw `JSON.stringify` body → `json_field` →
  dispatch), JSON escaping/round-trips (unicode/control chars), event ordering,
  unknown-event dropping, payload fidelity, and `gui_set_html`/handler-queued JS
  — via `bridge.rs` unit tests and the `spid3r gui --dry-run` replay
  (`tests/gui_tests.rs`). GUI natives are not available to the other engines or
  the AOT backend. See [`GUI.md`](GUI.md).
- A *remote* (HTTP) package registry. A **local** registry is supported: a
  `{ version = "…" }` dependency resolves from `$SPID3R_REGISTRY`, an offline
  directory of `<name>/<version>/` packages with optional `sha256.txt` integrity
  checks (see [`REGISTRY.md`](REGISTRY.md)). A hosted/remote index needs an HTTP
  client, which the zero-dependency default avoids. Registry packages' own
  dependencies are not resolved transitively (declare what you use).
- Cycle collection (v1.2): **all container and instance types** (`list`/`vec`,
  `stack`, `queue`, `deque`, `linked_list`/`singly_linked_list`/`doubly_linked_list`,
  `set`, `map`, `tuple`, `binary_tree`, `bst`, `heap`, `priority_queue`, `graph`,
  `hash_table`, `trie`, `matrix`, `ring_buffer`, `bag`, `ref`, `option`/`result`,
  and class `instance`s) are tracked automatically via `value::new_ref` and
  cycle-collected (`gc_collect()` / `gc_live_cells()` builtins; statement-boundary
  auto-collection **on by default**, `SPID3R_GC=0` disables it) — see
  [`GC.md`](GC.md).

### Shipped (audited 2026-10-18 — no longer "not built")

- **Bytecode optimizer** (`src/vm/opt.rs`): constant folding (through the
  interpreter's own helpers, so results *and* errors match), peephole, branch
  folding, and dead-code elimination; verified by an opt-vs-`--no-opt`
  differential over every example.
- **`spid3r fmt`** — token-stream formatter (identical re-lex + idempotence) —
  and **`spid3r debug`** — a line debugger over the VM (breakpoints, step/next/
  finish, backtrace, locals; scriptable via `--commands FILE`).
- **Package manager**: `init`/`install` (path + git deps, version constraints,
  deterministic `spider.lock` with SHA-256), `pack`/`unpack` (with POSIX ustar
  `prefix`, GNU `@LongLink`, and header checksum verification), and workspaces.
- **Gradual typing (PH-4).** Optional `: Type` annotations on `let`/`const`
  bindings, function parameters, return types (`fn f() -> int`), and class fields
  are enforced by `spid3r check` at compile time. The checker computes a
  best-effort static type for each expression and rejects mismatches (e.g.
  `let x: int = "s"`), wrong call-argument types, return-type violations, and
  field-assignment mismatches. Unannotated names stay `Any` (fully dynamic), so
  existing programs are unaffected. Supported types: `int`, `float`, `bool`,
  `string`, the collection names (`list`/`map`/`set`/`tuple`/…), class and
  interface names (subtype/implements aware), and `any` (dynamic escape hatch).
  `Int` widens to `Float`. See `tests/gradual_typing_tests.rs`.

## Performance

Baseline + current numbers from `spid3r bench` (release build, engine vm, best
of 3, iters/sec — each "iter" is one full body of the benchmark file):

| benchmark | baseline (2026-10-16) | after map index + Arc chunks | current release (best of 5) |
|---|---|---|---|
| arith | 8.06M | 8.10M | **46.42M (5.7x)** |
| closures | 528k | 557k | **2.31M (4.1x)** |
| json | 28.5k | 28.1k | **113.7k (4.0x)** |
| list | 1.94M | 2.01M | **6.59M (3.3x)** |
| map | 103k | 474k (4.6x) | **1.60M (3.4x)** |
| methods | 1.27M | 1.30M | **3.25M (2.5x)** |
| strings | 317k | 328k | **2.60M (7.9x)** |
| tasks | 30.6k | 30.2k | **165.2k (5.5x)** |

Changes so far: `MapData` got an O(1) side hash index (was a linear scan per
get/set — quadratic overall), nested function prototypes / bytecode chunks are
shared via `Arc` (closure creation no longer deep-copies the bytecode), and
`Invoke` has a monomorphic inline cache (bounded, chunk-pinned; the per-call
argument Vec is now reused for the receiver instead of allocating a second one).

**Where the time actually goes** (measured with 2M-call micro-benchmarks, so
the numbers are stable): a plain function call costs ~97 ns/iteration including
loop overhead, while a method call costs ~346 ns — i.e. the `Invoke` path adds
~250 ns per call. Profiling by ablation shows that is NOT the class-chain walk
(the inline cache removes it for ~3% at most) but the frame model itself:
per-call argument allocation, the instance mutex, and repeated
`self.frames[nf]` indexing through a large `CallFrame` struct. The two known
structural fixes, both scheduled as a dedicated session:

1. change `Value::Str(String)` to `Arc<str>` — every global/field/method name
   lookup allocated a `String` (`const_at` clones): **DONE** (session 7).
   Cloning a string value is now a refcount bump; global/field assignment
   writes in place (`get_mut`) instead of re-inserting an owned key.
   Measured (best of 5): methods +8%, list +6.5%, arith +5%, strings +3.6%.
2. keep `ip` in a local across the dispatch loop instead of writing
   `self.frames[nf].ip` on every instruction — **implemented, measured, and
   reverted**: 54 write sites classified (26 converted to a local, 28 kept
   explicit for error paths and frame changes), the full suite passed on the
   transformed VM, and the measurement was arith 8.42M → 7.96M iters/s (-5%):
   LLVM already merges the redundant stores, and the per-iteration write-back
   check costs more than the saved stores.

**VM performance is closed**: all contained formulations were tried and
measured neutral-or-negative (chunk-in-frame, invocation allocation reuse kept
as it also simplified correctness, ip-hoisting reverted). Reaching the next
level would require a redesign — a register VM, threaded dispatch, or raw
pointer stacks — which is high-effort and high-risk while the interpreter
delivers 8M+ simple iterations per second.

Next up per plan.txt: all v1.0/v1.2 items (gradual typing, `spid3r pack`/unpack
with long names & checksums, local registry, and full-container cycle collection)
are done.

## Known limitations

- Higher-order collection methods (`list.map(f)` etc.) **are** supported by the
  native AOT backend (`src/native/rt.rs`); `spid3r build` + run of
  `xs.map(fn(x){ ... })` matches the interpreter. (This entry claimed otherwise
  until 2026-10-21.)
- A `throw` crossing a task/await/join/callback seam carries the thrown **value**
  (audit A-9), so an enclosing `catch` rebinds the original map/object rather
  than a message string.
- Error messages from the VM reuse the shared diagnostics layer, so their wording
  matches the tree-walk interpreter by construction.

See [`BYTECODE.md`](BYTECODE.md) for the `.matrixc` format, VM design,
concurrency model, and memory strategy in detail; [`NATIVE.md`](NATIVE.md) for
the LLVM AOT backend, [`GUI.md`](GUI.md) for the embedded webview layer, and
[`BUILDING.md`](BUILDING.md) for building from source (including the optional
`native` and `gui` features and their platform prerequisites).
