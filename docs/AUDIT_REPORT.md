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
- Tar archive reader/writer (`src/pkg/mod.rs`) supports POSIX ustar `prefix`
  long names (<= 255 bytes), GNU `@LongLink` headers for longer names, and
  header checksum verification.

## 7. Overall QA status

**Production-ready within documented scope.** All confirmed critical, high, and medium bugs across the lexer, parser, semantic analyzer, tree-walk interpreter, bytecode compiler/deserializer/validator/optimizer/VM, value & collection layer, standard library, module loader, and package manager have been resolved at their root causes and pinned by 30 regression tests in `tests/audit_regressions.rs` and 3 deterministic fuzz suites in `tests/fuzz_tests.rs`. All 279 Rust tests and all 23 three-engine differential tests pass with zero clippy warnings.


---

## 8. 2026-10-19 addendum — second full audit (commit `9e86135` baseline)

A second evidence-based audit (parallel: build validation, front end, engines,
runtime/security, market research) confirmed the fixes above still hold and
found new issues, fixed in this addendum. Every fix was verified by a repro
that failed before and passes now, plus 10 new regression tests
(`tests/audit_regressions.rs` 30 -> 39; net unit tests +1).

| ID | Severity | Bug (root cause) | Fix |
|---|---|---|---|
| R1 | CRITICAL | Cross-task `mutex` deadlock: `lock()` parked on the internal condvar while holding the outer `Arc<Mutex<MutexData>>` guard; the unlocking task needed that same guard (`interpreter/mod.rs:836` vs `:299`). First contention hung both tasks. | `Value::MutexVal` now carries `Arc<MutexData>` directly (no outer wrapper mutex); `lock`/`unlock` need no dispatch guard anywhere (interpreter, VM, and the shared method host). |
| R2 | CRITICAL | `matrix(rows, cols, fill)` had no allocation cap: `matrix(2e9, 2e9, 0)` aborted the host (capacity overflow), larger dims overflowed `rows*cols` (`misc.rs:853`). | `check_matrix_dims` (checked multiply + 16,777,216-cell cap, mirroring the string-op caps) enforced in the constructor and in `mul`. |
| R3 | CRITICAL (dev/CI) | ~30+ nested parens/blocks aborted debug builds with STATUS_STACK_OVERFLOW in the semantic walk (parser's MAX_PARSE_DEPTH=96 bounds AST depth but semantic/interpreter frames are far larger than a 1 MiB Windows main stack). Release builds handled all tested inputs. | `cli::run` dispatches every subcommand on a dedicated 512 MiB-stack worker thread and joins it (exit codes and panic behavior preserved). |
| R4 | CRITICAL | Crafted `.matrixc` with a string length near `usize::MAX` wrapped `self.p + n` past the bounds check and panicked the host (`bytecode.rs:733`, exit 101). | `Reader::str` uses `checked_add` before slicing. |
| R5 | CRITICAL | VM compiler emitted argc/slot indices with `as u8`: >255 call arguments truncated silently (stack corruption, wrong results); >255 locals aliased slots (silent wrong reads). | Clean VM diagnostics: "too many arguments in call (max 255, got N)" (Construct/Call/Invoke) and "too many locals in one function (max 255)" (`add_local` now returns `Result`). |
| R6 | HIGH | Jump offsets cast to `i16`: >32 KiB function bodies mis-jumped, and `compile` wrote the broken `.matrixc` (exit 0). | `patch_jump` errors with "function body too large" when the offset does not fit i16; broken artifacts can no longer be written. |
| S1 | MEDIUM | `http_fetch` interpolated URL path/authority into the outbound request without CR/LF checks (header injection/request smuggling). | Control characters (`< 0x20`, 0x7F) rejected before connecting. |
| S2 | MEDIUM | Chunked decoder cap used unchecked `body.len() + size` — a hostile chunk size near `usize::MAX` wrapped past the 8 MiB cap (unbounded memory; debug panic). | `checked_add` before the cap comparison. |
| S3 | MEDIUM | `bundle --single-file` escaped only exact-case `</script>`; `</ScRiPt>`-style end tags broke out of the embedded source element. | Every `</` in the embedded source is neutralized (`<\/`). |
| S4 | MEDIUM | Module imports accepted absolute paths, drive letters, and `..` (escaping the documented search roots). | `modules::check_import_path` enforced in both the semantic checker (`public_names_of`) and the loader (`resolve`). |
| T1 | MEDIUM | Framing headers: user-supplied `transfer-encoding` passed through to fetch requests and responses (CL+TE ambiguity). | Stripped alongside `content-length`/`connection`. |
| D1 | MINOR | Debugger `--commands` file: UTF-8 BOM made the first command "unknown command '<BOM>bt'". | BOM stripped like the `.matrix` lexer does. |
| DOC | MINOR | Stale docs: sitemap test counts (269/279 -> 298/308), BYTECODE.md GC paragraph (said only list/vec/map/set traced), GC.md "map (values + keys)" (keys are scalar), AUDIT_REPORT §6 tar line (prefix + @LongLink ARE supported), gc.rs header comment. | All updated to match the code. |

Verification (2026-10-19, post-fix): `cargo test --no-default-features` **298 passed, 0 failed**; GUI **10/10**; `cargo clippy --no-default-features --all-targets -- -D warnings` **0 warnings**; `spid3r fmt --check` **31 files unchanged**; 3-engine differential **23 ok, 0 failed**; deadlock/matrix/argc/locals/jump/crafted-bytecode/module-path/http_fetch repros all now produce clean, catchable diagnostics; post-fix `spid3r bench --engine vm --runs 3` consistent with `benchmarks/results.json` (no regression).

Known-remaining (documented, not fixed here): field type annotations are not enforced on external `obj.field = v` assignments (checked on `self.field`); diagnostic columns are byte-based (shift on lines with multi-byte chars); module loading has a check-then-act race across concurrent tasks; a panic while holding a user mutex leaves it locked (no Drop guard); GC does not trace closures/environments (cycles through them leak); auto-GC is gated on `live_tasks()==0`; `join()` can starve the worker pool; worker threads are killed mid-flight at process exit; net hardening P3s (no default read timeout for raw `http_read_request`, handle-table lifecycle, wasm FFI `catch_unwind`); `error[runtime]` vs `error[vm]` phase label divergence; cargo bin/lib `spid3r.pdb` output-collision warning (cargo #6313).

---

## 9. 2026-10-19 addendum 2 — independent verification round (2 new fixes)

A third-party verification round re-ran the full validation matrix (all green
before any change: 298 core / 10 GUI / 23-23 differential / clippy clean) and
confirmed two new VM-compiler bugs by execution; both are fixed in this
addendum, each pinned by a new regression test.

| ID | Severity | Bug (root cause) | Fix |
|---|---|---|---|
| V1 | HIGH (P1) | `Expr::List` emitted `items.len() as u8` into `Construct` with no argc guard (Call/Method/New all had `check_argc`): a 256-element literal truncated the argc byte to 0 and corrupted the value stack (`error[vm]: int has no length`), and `compile` still wrote the broken `.matrixc` with exit 0. Repro: `audit_notes/probes/list{256,300}.matrix`. | argc guard in `src/vm/compiler.rs`: clean `too many elements in list literal (max 255, got N)`; `compile` now exits 1 and writes no artifact. |
| V2 | HIGH (P1) | `capture_local` let `own_upvalues` exceed the 7-bit descriptor encoding and `resolve_upvalue` masked with `& 0x7f`, so capture #129 silently aliased capture #1's cell (129-capture probe: tree 9385 vs vm 10257; 130: 9515 vs 10259; 128 is clean on both). Repro: `audit_notes/probes/cap{128,129,130}.matrix`. | 128-capture limit enforced at both finalization sites (`compile_function`, `compile_one` in `src/vm/compiler.rs`): `too many captured variables in one function (max 128, got N)`. |

New OPEN findings (recorded in `plan.html`, not fixed in this round):
user-reachable hangs with no diagnostic on self-referential containers through
for-in, `set.union`, `map.values`, and `linked_list.to_list` on BOTH engines
(`.clone()` is cycle-safe; the earlier json_write fix did not cover these
paths); a multi-minute stall building multi-million-deep nested lists in both
engines (no allocation/GC bound halts progress; root cause unverified);
gradual typing is bypassable through rebound function values and nested `fn`
declarations; privacy is enforced only on bare-identifier receivers; REPL
`:reset` keeps const sets; VM `Import` drops const-ness (observable only via
crafted `.matrixc`); `fmt --check` does not expand directories. The bytecode
deserializer/validator held against four hand-crafted mutations (truncation,
0xFF opcode byte, misplaced `Return` bytes): clean diagnostics, no host panic.
Residual code-verified P3: a crafted `Return` on an empty operand stack
swallows its own error (`src/vm/mod.rs` Return handler reaches
`return Ok(...)` after `fail`).

Post-fix verification (2026-10-19): `cargo test --no-default-features`
**300 passed, 0 failed** (`audit_regressions` 39 -> 41); 3-engine differential
**23 ok, 0 failed**; `cargo clippy --no-default-features --all-targets --
-D warnings` **0 warnings**; list300/cap130 probes now fail with the clean
diagnostics above; cap128 still prints 9256 on both engines; `compile` of the
300-element literal exits 1 and writes no artifact.

---

## 10. 2026-10-19 fix round — all open audit findings resolved

The findings from plan.html (F3–F13) were implemented and verified the same
day. Full validation after the round: `cargo test --no-default-features`
**340 passed, 0 failed** (40 new regression tests in five new files);
GUI **10/10**; 3-engine differential **23 ok, 0 failed**;
`cargo clippy --no-default-features --all-targets -- -D warnings` **0
warnings**; `spid3r fmt --check examples` (directory form, F13) exit 0.

| ID | Fix (files) | Verification |
|---|---|---|
| F3 | Snapshot-under-lock in the shared value layer: `iter_values` all arms; `call_method_host` to_list/values/set-algebra special cases; `set_op`/`map_values_out`/`to_vec` helpers (value/collections/*) | p01/p03/p05/p06 exit 0 on both engines; tests/fix_f3_hangs.rs 5/5 (deadline-guarded) |
| F4 | Two quadratic cost centers amortized in gc.rs (`NEXT_GC` backoff, `PRUNE_AT` watermark) + iterative `Drop` for VecData/MapData/SetData (seq.rs) — the teardown recursion (~10^5 levels) was a SECOND, previously masked defect | Before: 100k stalls >60s even with SPID3R_GC=0. After: 100k 0.6s / 400k 2.4s / 1M 6.5s / 4M 27.3s, all exit 0; tests/fix_f4_gc.rs 1/1 |
| F5 | `Ty::Fn` carries `Option<Arc<FnSig>>` (semantic/ty.rs); signatures propagate through declaration names, nested fns, lambdas, and `let g = <fn value>`; the Call arm checks arity + annotated argument types through any statically-typed callee; unannotated functions stay fully dynamic | Bypass probes rejected at check time (gates both engines); tests/fix_semantic_priv.rs 19/19 |
| F6 | `receiver_class` resolves any receiver expression's static class; Method/Field arms + the previously-missing Assign-Field privacy check (semantic/mod.rs) | `new C().hide()`, private-field reads AND writes rejected; dynamic receivers unchecked (documented); tests green |
| F7 | REPL `:reset` rebuilds the full session via a shared `fresh_session()` (repl/mod.rs) | Reset sequence test green (tests/fix_repl_cli.rs 11/11) |
| F8 | `Vm.global_consts`: Import records `public const` exports; SetGlobal refuses with the tree's wording (vm/mod.rs) | Code-reviewed against the tree contract; behavioral test needs a crafted-bytecode construction seam (follow-up in plan.html) |
| F9 | Shared `modules::circular_import_message` used by both engines | Format-parity test + tree behavioral test green |
| F10 | Return handler routes a failed pop through the error funnel (vm/mod.rs) | Mutation sweep (20+ hand-mutated .matrixc) — clean diagnostics, no host panic; crafted-Return behavioral test needs the same seam as F8 |
| F11 | REPL bare expressions retry the parse with `;` appended (parse-time only) | Echo tests green incl. broken-input reporting |
| F12 | Import/local collision diagnostics both directions, same-scope only (semantic/mod.rs, `VarInfo.is_import`, `predeclared`) | p09 case rejected; nested-scope shadowing stays legal; REPL direction via `check_repl_in` |
| F13 | `cmd_fmt` expands directory args (*.matrix, sorted, non-recursive) (cli.rs) | `fmt --check examples` exit 0; empty-dir usage error tested |

CONTRACT CHANGE (documented per sitemap invariant #5): external writes to
private fields are now rejected, consistent with the pre-existing read
behavior. tests/gradual_typing_tests.rs::annotated_field_read_has_type was
updated to a public field with a citation (its purpose — the field TYPE
contract — is unchanged; the private-write rejection is pinned in
tests/fix_semantic_priv.rs).

Known residuals after the round: deep container chains through SHARED cells
still recurse on drop (sole-owned chains are iterative); container types
outside list/vec/map/set keep recursive drops; F8/F10 lack behavioral
crafted-bytecode tests (construction seam follow-up); the mutex-on-panic
guard, module-load race, and GC tracing of closures/environments remain on
the documented roadmap (R16).

---

## 11. 2026-10-20 — Embedding API v0 (`spid3r::embed`, roadmap R7)

A host-facing scripting surface: `Engine::register/set_global/add_module_dir/
set_script_args` + `Engine::run` (parse -> check -> execute on a 512 MiB
worker, main-thread output captured) + `conv` value conversions. Core
additions: `Value::NativeClosure` (stateful host callbacks — the tree
interpreter, VM, AOT runtime, GUI callback list, and collection callback host
all dispatch them), `semantic::check_file_with` + `Externals` (host names are
dynamic `Any` bindings; `module_dirs` extend the import search path via the
new `modules::public_names_of_in`), and `Interp::run_program_inline` reuse for
thread-correct capture. API reference: `docs/EMBEDDING.md`; runnable demo:
`cargo run --example embed_hello` (new `examples_rust/` example target).

Verification (2026-10-20, per-suite): `tests/embed_tests.rs` **10 passed,
0 failed**; `--lib` **41 passed, 0 failed** (incl. 6 new embed unit tests);
3-engine differential **23 ok, 0 failed**; clippy `-D warnings` **0 warnings**;
GUI **10/10**; `cargo run --example embed_hello` produces the expected host
demo output. The consolidated full-suite re-run was blocked part-way by the
machine's **Windows Application Control policy** ("An Application Control
policy has blocked this file", os error 4551, on freshly-built unsigned test
binaries) — several suites re-verified green under the block
(audit_regressions 41, bundle 4, const_module 5, cycle 5) before it widened;
the blocked suites are unchanged by this round except additively. Re-run
`cargo test --no-default-features` after adding a policy exclusion for
`target\` to consolidate.

Known residuals (plan.html R7 follow-ups): VM/bytecode embedding path;
per-run script args (args() is process-global); native closures in the AOT
runtime's install path; Smart-App-Control interaction is environmental, not a
crate defect.
