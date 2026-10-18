# Spid3r — Comprehensive Audit, Debug & QA Report

*Date: 2026-10-18 · Auditor: independent full-repo audit (AutoClaw) · Baseline commit `eeac01f`*

Every claim below was executed against the code, not inferred: 236 pre-existing Rust
tests were run as the baseline, a 30-program differential probe suite (tree vs VM vs
compile+exec) was written and run, and every fix was verified by re-running the full
suite plus the probes. Two follow-up investigators audited stdlib and lexer/parser
(read-only); their full reports are in `audit/findings/`.

---

## 1. Project overview

Spid3r (crate `spid3r`, zero dependencies) is a dynamically-typed language with two
execution engines that must behave identically:

```
lexer → parser → semantic checker ─┬─ tree-walking interpreter (reference)
                                   └─ bytecode compiler → stack VM (+ optimizer,
                                      .matrixc serialize/exec, line debugger)
```

plus modules (import/privacy), tasks (spawn/await/channels/mutexes on an OS thread
pool), 20+ builtin collections, a GC with cycle collector, a project layer
(spider.toml / install / registry / pack), a formatter, REPL, and an optional LLVM
native backend and webview GUI layer (out of scope here — feature-gated).

CLI contract: exit 0 success, 1 user-program error, 2 usage error.

## 2. Bugs discovered and fixed

Severity is the worst plausible consequence. Every fix lists file(s) and a
repro that failed before the fix and passes now.

### A. VM closure/upvalue system — 4 interlocking engine divergences [CRITICAL]

The tree engine (reference) and the VM disagreed on every non-trivial closure. Root
causes, all in `src/vm/mod.rs` + `src/vm/compiler.rs`:

| # | Bug | Repro (before → after) |
|---|---|---|
| A1 | Captured-local cells lived on the shared `VmFunc` instance, so two activations of one closure (repeat calls, recursion) shared state | `make_counter()` twice: tree `3,1` → VM `3,4` → now `3,1` both |
| A2 | `CaptureSync` was idempotent, so all closures created in a `for x in …` body captured one cell and saw the last value | for-in capture: tree `0,1,2` → VM `2,2,2` → now `0,1,2` both |
| A3 | Captured-slot routing was never removed when the owning scope ended; later locals reusing the slot silently read/wrote the stale cell (up to `error[vm]: int has no length` crashes in mixed programs) | probe 12 crashed on VM, ran on tree → now MATCH3 |
| A4 | `resolve_upvalue` ignored `own_upvalues` when assigning `GetUpvalue` indices, so a function that both captured an enclosing variable and had a local captured by a nested function aliased the two | `r + g` read r's cell twice: tree `6` → VM `2` → now `6` both |

**Fix:** per-activation own cells (`CallFrame.own_cells`), fresh cell per capture
activation, a new `OpCode::Uncapture` (byte 53, operand width 1, disassembler +
`from_u8` + validation) emitted at scope exit, on break/continue, and at each
`for`-in iteration start; enclosing-upvalue index offset; parent-cell lookup via
`own_cells` for indices below the parent's `own_upvalues`. While-loops and C-style
`for` keep the tree interpreter's shared-binding semantics (verified: `3,3`).

### B. Package/dependency names used as filesystem paths [CRITICAL, security]

`src/pkg/mod.rs`: a dependency key `..` reached `vendor_dep`, which
`remove_dir_all`s `vendor/<name>` → `spid3r install` on a hostile manifest deleted
the project root. Also allowed `/`, `\`, `:` (git checkout dir, dist names).

**Fix:** `pkg::valid_pkg_name` allowlist (`[A-Za-z0-9_.-]`, no leading `.`) enforced
in `vendor_dep` and `fetch_git`, plus a `dest.starts_with(vendor/)` containment
assert before any delete.

### C. `unpack` arbitrary write from a hostile archive [CRITICAL, security]

The traversal guard missed Windows drive/ADS forms: an entry named
`C:/Users/.../x.matrix` makes `Path::join` discard the destination.
**Fix:** reject any name containing `:` and require `target.starts_with(dest)` after
the join (`src/pkg/mod.rs`).

### D. Module instance identity [MAJOR]

Modules were cached by the un-canonicalized path: `import "lib/util"` and
`import "./lib/util"` (or Windows case variants) produced two instances and evaded
cycle detection; the VM additionally executed an embedded module body once per
import *literal* (probe 24 printed `util init` twice on VM, once on tree), and
`compile_one` `mem::take`-stole the accumulated embedded table into nested module
chunks, so a second import spelling made `exec` fail with `module not found`.

**Fix:** all three key sites (interpreter cache, VM cache, VM compiler) now use the
canonicalized path (`modules::canonical_key`, `\\?\` prefix stripped for local
drives); `Import` opcodes embed the resolved path so spellings share one entry;
`in_progress` is popped after each module compile (also removes a latent
cross-spelling diamond false positive); only the top-level compile claims the
embedded table. (`src/modules/mod.rs`, `src/vm/mod.rs`, `src/vm/compiler.rs`)

### E. Documented stdlib contract violations [MAJOR]

| Bug | Doc | Fix |
|---|---|---|
| `read_file` returned a bare string/threw; `write_file` returned bare `null`; `spawn_process` returned a bare int/threw | `ok(...)/err(...)` results | all three now return results (`src/stdlib/mod.rs`) |
| `m[k] = v` threw "map does not support index assignment" | documented as `set` alias | implemented via `MapData::set` (`src/value/collections/mod.rs`) |
| `task.status()` unknown method; `task.join()` missing; `join()` consumed the result so a second join blocked forever | `status()`/`join()` documented | both added; join repeatable (clone, not take) (`src/sched/mod.rs`, `src/value/collections/mod.rs`) |
| a spawned task's thrown value was lost: `join` raised generic "uncaught throw" instead of `boom` | error fidelity | thrown value now crosses the task boundary (`src/interpreter/mod.rs`, `src/vm/mod.rs`) |
| `args()` returned the interpreter's argv (`["run", "file"]`) | "arguments passed to the program" | CLI installs post-script-path args; every subcommand gets a default (`src/cli.rs`, `src/stdlib/mod.rs`) |
| `random_int` seeded from a compile-time constant (identical sequence every run, correlated across threads) and `(hi-lo+1) as u64` panicked on wide ranges | "Random integer" | per-thread entropy seed; u128 span math; full i64 range supported |
| `json_write` deadlocked (non-reentrant re-lock) on self-referential lists/maps; unbounded recursion in parse and write | robustness | snapshot-under-lock + 512-level depth caps both directions |
| `json_write` emitted `NaN`/`inf` (invalid JSON), degraded `2.0` to `2`, `1e30` failed to reparse; `\uXXXX` escapes rejected; map keys unescaped | JSON codec | `{:?}` float form + non-finite error, i64→f64 fallback, surrogate-pair `\u` decoding, shared key/string escaper |

### F. Tooling [MAJOR]

- `git` argument injection via manifest `url`/`rev` (leading `-`) — rejected +
  `--` separator (`src/pkg/mod.rs`).
- `sha256.txt` integrity workflow unusable: `pack` printed the archive hash while
  `install` verifies the vendored-tree hash — `pack` now also prints
  `sha256(tree): …` and the docs say which one goes into `sha256.txt`.
- `spid3r init <new-dir>` always wrote `name = "app"` (canonicalize-before-create)
  — name now derived from the literal argument (`src/cli.rs`).

### G. Second-wave deep audit fixes (2026-10-18)

| # | Area | Bug & Root Cause | Fix |
|---|---|---|---|
| G1 | VM Closures [CRITICAL] | `resolve_upvalue` added `own_upvalues` at compile time (before later local captures incremented `own_upvalues`), and `VmFuncProto.upvalues` could not distinguish direct parent local captures from parent enclosing upvalue captures in 3-level nested closures; `emit_pending_syncs` also dropped `CaptureSync` when an earlier closure was inside an untaken `if` branch | Current-frame locals always use `GetLocal`/`SetLocal` (routed through `frame.captured`); `VmFuncProto.upvalues` encodes direct parent locals with bit 7 = `0` (`parent.own_cells[u & 0x7f]`) and parent enclosing upvalues with bit 7 = `1` (`parent.func.upvalues[u & 0x7f]`); `emit_closure_syncs` emits `CaptureSync` for all direct local captures of each closure (`src/vm/compiler.rs`, `src/vm/mod.rs`) |
| G2 | VM `finally` [CRITICAL] | `return`, `break`, and `continue` inside `try` or `catch` bypassed enclosing `finally` blocks on the VM and left stale `Handler` entries on `self.handlers` | `OpCode::Return` routes through enclosing `finally` blocks in the current frame (`Handler.pending_return`), and `emit_loop_exit` unwinds loop-internal `try` blocks via `EnterFinally` + inline `finally` + `EndTry` on `break`/`continue` (`src/vm/mod.rs`, `src/vm/compiler.rs`, `src/interpreter/mod.rs`) |
| G3 | Bytecode Security [CRITICAL] | `Reader::value()` indexed `self.b[self.p..self.p+8]` without bounds checks on tags `0`/`1` (panic on truncated `.matrixc`); `Vec::with_capacity(n)` trusted unvalidated item counts in `.matrixc` headers (OOM abort); `validate_chunk` cast negative jump targets `target < 0` to `usize` before bounds checking and skipped embedded `chunk.modules` | Bounds-checked tags `0`/`1` in `Reader::value()`; added `Reader::count(min_bytes)` cap before all `Vec::with_capacity` allocations; checked `target < 0` and recursively validated & optimized `chunk.modules` (`src/vm/bytecode.rs`, `src/vm/opt.rs`) |
| G4 | Value & Collections [HIGH] | Self-referential containers deadlocked/overflowed in `display_value`, `values_equal`, and `deep_clone`; `xs.append(xs)`, `s.union(s)`/`intersection(s)`/`difference(s)`, and `m.add(m)`/`mul(m)` deadlocked by locking receiver and argument simultaneously; `linked_list.pop_back()` set `tail = None` on length >= 2; `bst.insert` / `heap.push` accepted non-comparable values when empty; `string.pad_left`/`pad_right` wrapped negative widths to `usize::MAX`; `deep_clone` did not deep-copy `VInstance` or register clones with `gc::track` | Cycle-safe visited tracking and lock-before-recurse in `src/value/mod.rs`; self-lock avoidance in `src/value/collections/{mod,nodes,misc}.rs`; fixed `ListData::pop_back` tail pointer; upfront comparability check in `BstData::insert` and `HeapData::push`; negative width validation and allocation caps on `repeat`/`pad_left`/`pad_right` |
| G5 | Parser & Semantic [HIGH] | Deeply nested expressions/blocks overflowed the host stack; local named `fn name() {}` statements inside blocks failed to parse/check/compile for recursion; `new ClassWithNoCtor(1)` and inherited `new` arity were not checked; `now()` was typed `float` instead of `int` | Added `MAX_PARSE_DEPTH = 96` in `src/parser/mod.rs`; supported local named `Stmt::Fn` across parser, semantic checker, and VM compiler; validated inherited/default constructor arity in semantic checker, tree interpreter, and VM; typed `now()` as `Ty::Int` (`src/parser/mod.rs`, `src/semantic/mod.rs`, `src/interpreter/mod.rs`, `src/vm/mod.rs`) |
| G6 | Lexer, JSON, Modules, GUI [MEDIUM] | Lexer `self.col - 2` underflowed on `\` + `\n` in strings, overflowing float literals became `inf`, CRLF comments kept `\r`, non-ASCII unexpected bytes printed as Latin-1; `json_parse` accepted malformed numbers (`1.`, `1e`, `1e999`) and duplicated keys in `MapData`; `SPID3R_PATH` split on `:` corrupted Windows `C:\` paths; GUI `dispatch` dropped cascading events and `file_url` omitted leading `/` on Windows | Fixed in `src/lexer/mod.rs`, `src/stdlib/mod.rs`, `src/modules/mod.rs`, `src/native/modules.rs`, and `src/gui/window.rs` |
| G7 | VM Stack-Trace Line Attribution [MEDIUM] | `Vm::run_loop` advanced `frame.ip` past the current instruction before executing it, so `cur_span()` and `capture_trace()` resolved `span_at(f.ip)` to the *next* instruction (e.g. reporting line 1 on a `throw` at the end of a function or the statement after a multi-line call), and `cur_span()` underflowed if `frames` was empty | Added `inst_ip` to `CallFrame` (`src/vm/mod.rs`), restored parent AST spans before emitting opcodes in `src/vm/compiler.rs`, and guarded `cur_span()` when `frames` is empty |
| G8 | Standalone Native Runtime Discovery [MEDIUM] | `find_staticlib` (`src/native/link.rs`) only searched `<exe_dir>` and `<cwd>/target`, failing when invoked outside the Cargo workspace or from `deps/` | Added `spid3r::native::{find_staticlib, staticlib_candidates}` (`src/native/mod.rs`, `src/native/link.rs`) supporting `SPID3R_RUNTIME_LIB`, `SPID3R_LIB_DIR`, `MATRIX_RLIB_DIR`, `<exe_dir>/../lib`, and `<exe_dir>/../lib/spid3r` |
| G9 | Continuous Integration & Fuzzing [QUALITY] | No GitHub Actions CI matrix or fuzzing harness existed for lexer, parser, and `.matrixc` deserializer | Added `.github/workflows/ci.yml` (Windows/Linux/macOS core + GUI jobs), `fuzz/` (`cargo-fuzz` targets `fuzz_lexer`, `fuzz_parser`, `fuzz_bytecode`), and deterministic mutation fuzz suite `tests/fuzz_tests.rs` |

## 3. Files changed

`src/lexer/mod.rs`, `src/parser/mod.rs`, `src/semantic/mod.rs`,
`src/interpreter/mod.rs`, `src/vm/{mod,compiler,bytecode,opt}.rs`,
`src/value/mod.rs`, `src/value/collections/{mod,nodes,misc}.rs`,
`src/stdlib/mod.rs`, `src/modules/mod.rs`, `src/native/{mod,link,modules}.rs`,
`src/gui/window.rs`, `src/pkg/mod.rs`, `src/cli.rs`, `src/sched/mod.rs`,
`.github/workflows/ci.yml`, `fuzz/Cargo.toml`, `fuzz/fuzz_targets/fuzz_{lexer,parser,bytecode}.rs`,
`docs/{STANDARD_LIBRARY,GRAMMAR,BYTECODE,ROADMAP}.md`, `doc.html`, `sitemap.txt`,
`tests/audit_regressions.rs` (30 regression tests), and `tests/fuzz_tests.rs` (3 fuzz suites).

## 4. Test results

| Check | Result |
|---|---|
| Baseline before any change | 236 Rust tests passed, 0 failed; examples differential 23/23 |
| Final `cargo test --no-default-features` | **269 passed, 0 failed** (including 30 audit regression tests + 3 fuzz suites) |
| Final `cargo test` (with `gui` feature) | **279 passed, 0 failed** (269 core + 10 `gui_tests`) |
| Final `cargo clippy --all-targets -- -D warnings` | **0 warnings, 0 errors** (both `--no-default-features` and `--features gui`) |
| Examples differential (`spid3r test`, tree == vm == exec) | **23 ok, 0 failed** |

## 5. Security findings

**Fixed (confirmed vulnerable before):**
1. Dependency-name path traversal → arbitrary `remove_dir_all` (T1).
2. `unpack` drive-letter/ADS arbitrary write (T2).
3. `git` option injection from manifest `url`/`rev` (T3).
4. `.matrixc` deserializer slice out-of-bounds panic on truncated `Int`/`Float` constants and OOM allocation abort on crafted item counts (T4).
5. `.matrixc` validator negative jump offset cast-to-`usize` bypass and unvalidated embedded module chunks (T5).
6. Host stack overflow DoS via deeply nested expressions/blocks/types in parser (`MAX_PARSE_DEPTH = 96`) and cyclic container printing/comparison/cloning (T6).
7. Unbounded string allocation in `string.repeat` / `pad_left` / `pad_right` (capped at 64 MiB / 16M chars) (T7).

## 6. Remaining known limitations (by design)

- Lexer float literals require decimal form (`1.5`, not scientific notation `1.5e3`), and `-9223372036854775808` (`i64::MIN`) cannot be written as a positive literal before unary negation.
- Cycle collector (`src/value/gc.rs`) traces `list`, `vec`, `set`, `map`, and class instances; cycles through `tree`, `graph`, `heap`, or `matrix` containers require `weak`/`deref`.
- Native AOT backend (`--features native`, requires external LLVM 18) rejects classes declared inside functions and higher-order collection callbacks (`list.map(f)`).
- Tar archive reader/writer (`src/pkg/mod.rs`) supports standard 100-byte ustar entry names without long-name `prefix` extension headers.

## 7. Overall QA status

**Production-ready within documented scope.** All confirmed critical, high, and medium bugs across the lexer, parser, semantic analyzer, tree-walk interpreter, bytecode compiler/deserializer/validator/optimizer/VM, value & collection layer, standard library, module loader, and package manager have been resolved at their root causes and pinned by 30 regression tests in `tests/audit_regressions.rs` and 3 deterministic fuzz suites in `tests/fuzz_tests.rs`. All 279 Rust tests and all 23 three-engine differential tests pass with zero clippy warnings.

