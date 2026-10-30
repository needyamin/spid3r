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
`tests/audit_regressions.rs` (83 regression tests), and `tests/fuzz_tests.rs` (3 deterministic fuzz suites).

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

**Production-ready within documented scope.** All confirmed critical, high, and medium bugs across the lexer, parser, semantic analyzer, tree-walk interpreter, bytecode compiler/deserializer/validator/optimizer/VM, value & collection layer, standard library, module loader, and package manager have been resolved at their root causes and pinned by 83 regression tests in `tests/audit_regressions.rs` and 3 deterministic fuzz suites in `tests/fuzz_tests.rs`. All 500 Rust tests (484 core + 10 GUI + 6 AOT) and all 25 example files (24 programs + 1 shared module) pass the three-engine differential with zero clippy warnings.


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

Known-remaining (documented, not fixed here): field type annotations are not enforced on external `obj.field = v` assignments (checked on `self.field`); diagnostic columns are byte-based (shift on lines with multi-byte chars); module loading has a check-then-act race across concurrent tasks; a panic while holding a user mutex leaves it locked (no Drop guard); GC does not trace closures/environments (cycles through them leak); net hardening P3s (no default read timeout for raw `http_read_request`, handle-table lifecycle, wasm FFI `catch_unwind`); `error[runtime]` vs `error[vm]` phase label divergence; cargo bin/lib `spid3r.pdb` output-collision warning (cargo #6313).

Stage-2 (2026-10-20) closed three items from that list — see
`assessment/AUDIT_2026-10-20.md` §6 and the concurrency contract in
`sitemap.txt` §12: the automatic collector's quiescence gate is now mirrored by
`gc_collect()` (audit A-4); `join()`/`recv`/mutex waits no longer starve the
pool because it grows on demand (A-3); and program exit drains outstanding tasks
(bounded by `SPID3R_TASK_DRAIN_MS`) instead of killing workers mid-flight (A-3).
Structured thrown values also cross task/callback seams on both engines and the
AOT backend (A-9), and the remaining self-referential container methods no longer
re-lock the receiver (A-7). The 2026-10-21 limits round then bounded the ready
queue with producer backpressure, added a stall watchdog at the worker ceiling,
made `shutdown()` join the worker threads at exit (CLI + AOT), replaced the
blanket "no live tasks" GC gate with the precise "no other *running* thread"
gate, and made concurrent `set`/`bag` adds commit through a verified snapshot.
Regressions: `tests/stage2_concurrency.rs` (process-level watchdog tests, both
engines), `tests/sched_limits.rs` (isolated pools), the CAS unit tests in
`src/value/collections/seq.rs`, and
`tests/native_tests.rs::native_matches_tree_on_structured_task_errors`.

Stage-0 (2026-10-21) then closed the correctness debt the audit left open — see
`audit_notes/stage0_correctness.md`. A 200k-deep `stack`/`queue`/`deque`/`bag`/
instance chain no longer aborts the process in `Drop` (one iterative teardown
worklist now covers every container and instance type, and mixed chains are
handled); `deep_clone` preserves cycles instead of aliasing the original into the
copy; the regex engine scans once, turning a 20 000-character failing search from
a >30 s timeout into 4 ms with 532 differential cases byte-identical; HTTP framing
has a deadline (`SPID3R_NET_TIMEOUT_MS`) so a silent client cannot hold a worker,
and the chunked-body buffer compacts; `.npy` shape arithmetic is checked. The
low-severity batch is closed too (empty token stream, character-based diagnostic
columns, CSV BOM, `[^a]` versus newline, tar entry types, non-UTF-8 CLI
arguments, `spid3r test --filter/--list`). **A-13 (closure/environment cycles)
remains open**: measured at one leaked cell per closure, with the required design
recorded in `docs/GC.md`.

Stage-4 (2026-10-21) finished the toolchain and docs pass — see
`audit_notes/stage4_toolchain_docs.md`: `spid3r check` now validates the index
expression of an index-assign target, the `main` signature, and function-type
signatures (A-10); the lib target was renamed to `spid3r_lang` so its cdylib no
longer collides with the binary's `spid3r.pdb` (F2); the tree is rustfmt-clean
and CI gates `cargo fmt --check` (F1); `tools/count_tests.sh` is the canonical
counter and every quoted number in README/doc.html/this file now comes from it
(A-18/F3); `tools/bench_compare.py` no longer hardcodes a host path/name or
deletes files it did not create; and the AOT runtime got atomic upvalue cells,
bounds-checked environment slots and `catch_unwind` at its FFI boundary (A-12),
with a nightly cargo-fuzz workflow that actually runs the three fuzz targets.

Stage-3 (2026-10-21) then hardened the artifact and dependency paths — see
`audit_notes/stage3_artifact_hardening.md`: `spid3r install` no longer accepts
git's `ext::` command transport (only https/ssh/scp by default, A-5), the
lockfile's git commits are enforced on re-install (`install --update` to move)
and vendor hashes now cover every file under `src/` as raw bytes (A-13); a
crafted `.matrixc` is rejected by the validator instead of panicking the VM
(terminator rule, instruction-table consistency, trailing bytes, nesting depth,
linear-time jump checks, bounds-checked `ip`, A-8); and the installer's WebView2
bootstrapper is an opt-in, SHA-256-verified download that fails closed, so a
plain `cargo build --release` performs no network access (A-11).

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

## 12. 2026-10-21 — Language server (`spid3r lsp`, roadmap R20)

Adoption barrier #1 in `audit_notes/market_analysis.md` is "no LSP / editor
support" — table stakes for a dynamic language. This round closes it **without
adding a second implementation of anything**.

**Design.** `src/lsp/mod.rs` is a thin JSON-RPC 2.0 server over stdio that
reuses the compiler's own front end: the same lexer, the same parser,
`semantic::check_file_with_warnings` (so lints are published too, not just
errors) and `fmt::format_source`. There is exactly one checker and one
formatter in the crate, so an editor can never disagree with `spid3r check`.
Output JSON is a hand-written lock-free `Json` enum; input JSON is parsed by the
stdlib's already-tested `JsonParser`, exposed as `pub(crate) json_parse_text`.
No third-party crates (the crate is zero-dependency by policy).

**Symbol resolution.** A flat declaration index (`SymKind`, `Decl`) with
*visibility extents*: top-level declarations resolve over the whole file, a
function body's parameters and locals resolve over that body, and the cursor
picks the **smallest extent containing it** — so a local shadows a global with
no scope algebra at all. Positions are character-based, matching Spid3r's
`Span`, so the 0-based LSP conversion is pure arithmetic (with a UTF-16 caveat
documented in `docs/LSP.md`).

**Surface.** `initialize`/`initialized`/`shutdown`/`exit`,
`didOpen`/`didChange`/`didClose` (Full sync), `hover`, `definition`,
`documentSymbol`, `completion`, `formatting`, `publishDiagnostics`. A VS Code
client ships in `packaging/vscode/` — dependency-free, no build step.
Deliberately **not** implemented (documented, not silently missing): rename,
references, semantic tokens, incremental sync, workspace symbols, signature
help, code actions.

### 12a. Bug found and fixed while building it

- **`analyze` swallowed the error diagnostic.** The first version used
  `unwrap_or_default()`, so a file with a type error published *no*
  diagnostics and broken code looked clean in the editor. Fixed with an
  explicit `match` that keeps `vec![first_error]`; verified against
  `spid3r check`, which does report the same error.
- **Extent computation ended at `return`.** `Stmt::Return`'s own span covers
  only the keyword, so a function's extent stopped there and its parameters
  never resolved. Fixed by growing the extent over the return value, and by
  simplifying `expr_extent` to grow over `Expr::span()` (an expression's span
  already covers its whole text).

### 12b. Pre-existing flake found by this round's gate run (not caused by it)

A full-suite run failed once in `tests/stage2_concurrency.rs`
(`a_task_can_collect_when_no_other_thread_is_running`), then passed on every
later run. Investigated rather than re-rolled, because a load-dependent
failure is a real defect somewhere:

- **The runtime is correct.** `gc_quiescent()` is `active_threads() == 1`, and
  `sched::sleep_ms`/recv/join/mutex each hold a `BlockedGuard` for their whole
  wait, so a parked thread never holds a concurrent collection back. Proven
  with a probe where the main thread stays active after `spawn`: the task's
  `gc_collect()` then correctly fails with "gc_collect: 1 other thread(s) are
  running; join or finish them before collecting" on **both** engines.
- **The test's expectation was the defect.** It awaited the task immediately,
  so the main thread was still counted active between `spawn` and the `await`;
  when the machine was loaded the worker legitimately saw two running threads
  and refused. Fixed deterministically with the idiom this file already uses
  elsewhere (`spawn` → `sleep_ms(200)` → `t.join()`): the main thread parks for
  the sleep and again for the join, so every interleaving lands on exactly one
  running thread. Verified 160/160 under 16-way parallel load and 4/4
  consecutive full-suite runs (previously ~1 failure in 5).

### 12c. Verification

Per-suite: `tests/lsp_tests.rs` **18 passed, 0 failed** (handshake,
diagnostics on open/change, hover on declaration/doc/local/keyword, definition,
outline nesting, completion, formatting, unknown method `-32601`, exit codes,
stdin close) plus **6 unit tests** in `src/lsp/mod.rs` (parameter/top-level/local
resolution, doc-comment attachment, type-error capture, syntax-error capture).
Full core suite **608 passed, 0 failed** (was 584), GUI **10/10**, AOT **6/6**,
3-engine differential **25 ok, 0 failed**, wasm32 build clean, `fmt --check`
clean, clippy `-D warnings` clean. Canonical counts from `tools/count_tests.sh`:
**624 total** (608 core + 10 GUI + 6 AOT), 25 examples, 3 fuzz targets.

---

## 13. 2026-10-21 addendum — external audit (commit `88aac74` baseline)

A fresh differential-audit round (32 probe programs over the newest language
surface: interpolation, bitwise, `??`/`?.`, destructuring, `for (k, v)`,
`match`/enums, option/result, elif chains, negative indexing, try/finally,
tasks+mutexes, while-spill, C-style `for`, closures, floats, JSON, classes,
modules, string/list methods, formatter idempotence).

### Fixed

| ID | Severity | Bug (root cause) | Fix |
|---|---|---|---|
| O1 | CRITICAL | **VM optimizer mis-folded static-condition `elif` chains.** The branch fold rewrote `[False][JumpIfFalse -> A]` into `[Jump -> A]`; a later pass then removed the `[True][JumpIfFalse]` pair that lived at A (the elif condition) without noticing A had become a live jump target (it only guarded the jump instruction's own offset). The dead-code pass ignored the dangling target, marked the elif arm unreachable, deleted it, and the encode-time forward-fill re-pointed the jump onto the `else` arm. On the VM, `if (false) {X} elif (true) {Y} else {Z}` executed **Z** (or **nothing** without an `else`); also reproduced with foldable comparisons (`1 < 2`, `"a" == "a"`, `2 + 2 == 4`). Tree engine correct. | The dead-code pass in `src/vm/opt.rs` now resolves any target that is not a surviving instruction start by forward-filling to the **next surviving instruction** — the same remap semantics the encode step already applies. Applied both to the seed-target loop and to targets followed during the reachability walk. |

Repro probes: `target/probes/z_min{,2,3,4}.matrix`, `z7.matrix` (pre-fix VM
output was `else`/empty; post-fix byte-identical to the tree engine).

Verification: `cargo test --no-default-features` **613 passed, 0 failed**
(5 new regression tests in `tests/audit_regressions.rs`:
`opt_static_elif_with_else_takes_elif_arm`, `opt_static_elif_without_else_takes_elif_arm`,
`opt_folded_comparison_conditions_in_elif_chain`, `opt_static_true_while_still_terminates`,
`opt_dynamic_if_chain_unaffected`); 3-engine differential **25/25**;
`spid3r fmt --check` clean on all probes; clippy `-D warnings` clean.
Docs counts refreshed (624 -> 629 total) in README.md, doc.html, sitemap.txt.

### Open findings recorded (not fixed in this round)

| ID | Severity | Finding (evidence) |
|---|---|---|
| F-1 | HIGH | **`super` is not implemented.** `super(args)` in a constructor and `super.method(self)` both fail with `error[semantic]: unknown name 'super'` on both engines (probes `z_super2.matrix`, `z_super.matrix`); no `super` handling exists in `src/semantic`, `src/interpreter`, or `src/vm`. Consequence: a subclass constructor must re-initialize every parent field, and an overridden method can never delegate to the parent implementation (calling `Parent.method(self)` is also unsupported: "class values have no methods"). This is the largest single OOP gap vs PHP (`parent::`), Python (`super()`), and Java (`super.`). Recommended shape: parse `super` as a dedicated expression only valid inside class bodies; lower to a parent-class method/constructor lookup resolved at semantic time (single lowering, both engines consume the same AST — no new opcode needed for the tree engine; the VM compiler needs a `GetSuperMethod`-equivalent emitted as an existing closure/constant pattern). |
| F-2 | LOW | Stack-trace frame naming diverges: the tree engine labels an overridden method frame `Dog::speak`, the VM labels it `speak` (probe `q02_class.matrix`). Cosmetic diagnostic parity; fix by storing the qualified name (class + method) in `VmFuncProto`/method compilation. |
| F-3 | LOW | `.len`-as-field (without parens) on containers produces "list has no fields"; the message does not suggest the documented `len(x)` / `xs.len()`. A "did you mean" hint would remove the top newcomer stumbling block observed while writing the probes. |
| F-4 | INFO | Default member visibility is private (`fn speak(self)` in a class is class-private, including when overriding a public parent method). Consistent and enforced on both engines, but worth calling out prominently in GRAMMAR.md — it differs from PHP/Python/Java defaults. |
| F-5 | INFO | Module namespaces are read-only maps; `mu.VERSION = 8` errors with "map has no fields" rather than "module bindings are immutable". Message-quality item only. |

### Audit methodology note

Probe programs live in `target/probes/` (p*.matrix language surface,
q*.matrix stdlib/class/module surface, z*.matrix minimizations) with
`target/probes/run.sh` diffing tree vs vm stdout+stderr and exit codes. The
corpus confirmed: interpolation holes, `{{` literal handling (per the
documented "hole only when `{` is immediately followed by an identifier"
rule), bitwise precedence and shift bounds, `??`/`?.` lowering (including
while-condition spill), destructuring and closure capture, `for (k, v)`
capture, match guards and fall-through, option/result combinators and `?`
propagation, mixed `elif`/`else if`, negative indexing on list/string/tuple,
`return`/`break`/`continue` through `finally`, task throw fidelity and mutex
accumulation, C-style `for` variants, per-activation closure cells, float
literals/promotion, JSON round-trips (incl. non-ASCII and escapes), class
privacy (static, inherited-private enforced), module const/fn export +
privacy, and formatter idempotence on all new syntax. All byte-identical
across engines post-fix.

---

## 14. 2026-10-21 addendum 2 — findings implemented (F-1 through F-5, plus the defects the work uncovered)

This round implemented the open findings from §13, working one issue at a
time with tests re-run after each change. Two **new** confirmed defects were
found and fixed along the way (N1, N2 below).

### Fixed

| ID | Severity | Finding → Fix |
|---|---|---|
| F-1 | HIGH | **`super` was entirely unimplemented** (§13 F-1). Implemented end-to-end: `super` is now a keyword (`src/lexer/token.rs`), parses as `Expr::Super` (`src/parser/mod.rs`), is validated against the class table by the checker (`src/semantic/mod.rs`: parent exists, the parent provides the method/constructor, arity checked — all identical on both engines), resolved by the lowering into `Expr::SuperCall { parent }` (`src/lower/mod.rs` — one seam, both engines consume it), executed by the tree interpreter (`call_super`/`call_class_method`/`call_parent_constructor`) and by the VM via two new additive opcodes `SuperCtor` (56) and `SuperMethod` (57) with the parent class resolved by name in the globals. The AOT backend rejects it with the standard `unsupported` wording (documented). Semantics are Java/Python-correct: `super(args)` runs the parent constructor on the *same* instance; `super.m(args)` starts the lookup at the *defining* class's parent, so a subclass that does not override a method reaches the grandparent instead of recursing. Pinned by `tests/super_tests.rs` (8 tests: constructor chaining, method delegation, the three-level unoverridden-method chain, and four error cases, each on tree + VM + compiled artifact) and an `engine_matrix.rs` case. |
| F-2 | LOW | **Stack-trace frame naming diverged** (tree `Dog::speak` vs VM `speak`). `CallFrame` now carries a `display` name set at the call site (`Class::method` / `Class::new`), used by `capture_trace`; method and constructor traces are byte-identical across engines. Pinned by two `tests/language_surface.rs` cases that compare full stderr. |
| F-3 | LOW | **`xs.len` produced "list has no fields"** with no pointer to the fix. A shared `value::no_field_error(value, field)` now produces `list has no field 'len' (use len(x) or x.len() for its length)` for every length-capable type, and names the field for every other type — identical on both engines (both call the same helper). |
| F-5 | MED | **A module's `public const` was mutable through the import alias**: `mu["VERSION"] = 9` and `mu.set("VERSION", 9)` succeeded and changed the value the module itself saw. An import alias is now a **module namespace** (`MapData::namespace`, with its `public const` exports frozen): the checker rejects rebinding a const export with a literal key (both engines, identical message), the runtime rejects it for a computed key (`mutation_error`), and the namespace rejects all mutation. The VM now learns an export's const-ness through the new `DefineConst` opcode (55): the module VM records its own top-level consts while running and reports them with its exports, so embedded modules (`.matrixc`) freeze namespaces exactly like source-loaded ones. Pinned by `tests/module_namespace.rs` (6 tests). |
| N1 | MED | **Found while implementing F-5: an exported function was shadowed by a built-in map method.** A module exporting `get`, `set`, `len`, `keys`, … was unreachable through the alias (`mu.get()` called `map.get` and failed its arity check). A namespace's exported names now win over the map's own methods in the shared dispatcher (`call_method_host`) and in the VM's `Invoke` (which routes namespace maps through the shared path). Pinned by `tests/module_namespace.rs::exported_names_shadow_built_in_map_methods`. |
| N2 | CRITICAL | **Found while implementing the panic-guard fix: the VM never recorded `mu.lock()` at all.** A mutex locked inside a function stayed locked forever after the function returned on the VM (a deterministic hang; the tree engine released at the call boundary). Both engines now record a `sched::MutexGuardToken` per `lock()` — the token **unlocks on drop**, so a panic unwinding through the engine (an embedding host runs many scripts in one process) also cannot leave a mutex locked — and an explicit `unlock()` disarms the token so the frame-exit release cannot free a lock another task re-acquired. The tree engine sweeps its guards at the entry points (`run_program_inner`, each spawned task, module load); the VM's tokens live on the `CallFrame` and unlock when the frame is dropped. Pinned by two watchdog tests in `tests/stage2_concurrency.rs` (child processes under a timeout, per the project's concurrency-test convention). |

### Verified along the way

- **Error phase labels**: spot-checked six runtime/semantic error programs —
  `error[runtime]` / `error[semantic]` are identical across engines (the
  earlier rounds' unification holds; `error[vm]` remains reserved for
  malformed bytecode). No change needed.
- **`??` semantics**: confirmed null-coalescing by design (implementation and
  lowering) and documented it in GRAMMAR.md — `none ?? x` yields `none`.
- **Formatter idempotence** on the new syntax, and **AOT backend compile**
  restored (`Super`/`SuperCall` arms added to `src/native/ir.rs`; native_tests 6/6).

### Regression caught during the work (and fixed)

The first version of the panic-guard fix placed a sweep object in every
`exec_block`; its stack cost multiplied once per recursion level and pushed
`recursive_depth_guard_stops_infinite_recursion` (in-process, 1000 levels) over
the thread stack. The sweeps now live only at the interpreter's entry points,
the super-call evaluation is in `#[inline(never)]` helpers so the (large)
error-path locals never land in `eval`'s frame, and `Interp::rt` is
`#[inline(never)]` (cold path). The depth-guard test passes again at
`MAX_CALL_DEPTH = 1000`.

### Verification

`cargo test --no-default-features` **653 passed, 0 failed** (canonical count
from `tools/count_tests.sh`: **669 total** = 653 core + 10 GUI + 6 AOT);
3-engine differential **25/25**; `cargo test --features native` AOT **6/6**;
clippy clean on the default-feature build. Docs refreshed: GRAMMAR.md
(`super`, default member visibility, `??` semantics), KEYWORDS.md (`super`),
BYTECODE.md (opcodes 55-57), README/doc.html/sitemap test counts.

### Remaining backlog (recorded 2026-10-21; items 1, 2, 5, 7, 11 implemented the same day)

Still open:
1. **Field type annotations (LOW)** — not enforced on external
   `obj.field = v` assignments (checked on `self.field` only).
2. **Net hardening P3s (LOW)** — no default read timeout for raw
   `http_read_request`; handle-table lifecycle; wasm FFI `catch_unwind`.
3. ~~**Tuple literals (FEATURE)**~~ — **closed 2026-10-22 (PAR-05)**:
   `(1, 2, 3)` parses as a tuple literal (desugared to `tuple(...)`);
   `(e)` stays a parenthesized expression, `(e,)` a one-element tuple, `()`
   the empty tuple.
4. ~~**`as` casts (FEATURE)**~~ — **closed 2026-10-22 (PAR-06)**: `expr as
   int/float/string/bool` (the keyword was already reserved); desugared to
   the conversion builtins, binds tighter than `*` (Rust-style).
5. **Interpolation holes (FEATURE)** — a hole is `{name}` / `{obj.field}`
   only; `{len(xs)}` stays literal text. Extending needs a compat story with
   the documented `{{ user }}` template rule (stdlib/web.matrix).
6. ~~**AOT `super` (PARITY)**~~ — **closed 2026-10-22 (PAR-03)**: compiled
   classes support `super(...)` and `super.m(...)` via the
   `spid3r_super_call` runtime trampoline (parent resolved in the globals,
   same messages as tree/vm); `class_super` joined the AOT subset of
   `tests/engine_matrix.rs`, and the dynamic method-arity message was aligned
   with tree/vm while there (PAR-04, `method_arity_dynamic` case).

Implemented in this round (details above and in the sections that follow):
- ~~A-13~~ — closure/environment/class cycles are now collected
  (`gc_trace_cells`, `track_immutable`, registering constructors;
  `tests/closure_cycle_leak.rs` asserts reclamation + liveness).
- ~~Module-load race~~ — `claim_load`/`LoadClaim`: atomic claim, cross-task
  waiters wait on a condvar, same-thread re-entry stays the circular-import
  error, a panicked loader releases the claim on drop.
- ~~`for (x) in list`~~ — the parenthesized single-variable form parses.
- ~~Hex literals~~ — `0x`/`0o`/`0b` (either case) plus `_` digit separators
  everywhere (`1_000`, `0xDEAD_BEEF`, `1_0.5_0`), clean lexer errors for
  malformed separators; `tests/literal_syntax.rs` (6 tests).
- ~~Container-method docs~~ — the full matrix is in
  STANDARD_LIBRARY.md ("Container methods").

---

## 15. 2026-10-21 addendum 3 — persistence + web framework (the CMS/ERP blockers)

The audit report identified the absence of **any database connectivity** as
the single blocker standing between the language and CMS/ERP-class
applications. Implemented in this round:

### SQLite driver (`src/stdlib/db.rs`, new)

`db_open(path)` loads the real SQLite C library at runtime through the OS
loader (`LoadLibraryW` on Windows, `dlopen` on POSIX) — **zero crate
dependencies, nothing downloaded at build time**; the wasm target compiles
without it. New `Value::Database` handle variant (modeled on `MutexVal`;
touches `ref_id`/equality/`type_name`/`display`/`deep_clone` plus a
`call_method_host` arm). Parameterized statements only (injection-safe);
WAL + foreign_keys ON by default; TEXT/INTEGER/REAL/BLOB/NULL map to
string/int/float/bytes/null in both directions. Errors are catchable
strings carrying `sqlite3_errmsg`.

**Defect found and fixed while building it**: the SQLite build shipped with
CPython on Windows (3.53.1) **faults when `sqlite3_open` triggers lazy
auto-initialization** inside a host process. The driver now calls
`sqlite3_initialize()` explicitly after loading — documented as always safe
and idempotent. Found by bisecting: `sqlite3_libversion()` worked,
`sqlite3_open` access-violated, `initialize` + `open` worked.

Tests: inline Rust test (loader + open/close + statement round trip),
`tests/db_tests.rs` — full CRUD lifecycle (create/insert/query/update/error
handling/close-then-use) on **tree, VM, and the compiled artifact**. Every
db test **skips cleanly when no SQLite library is installed**, so CI runners
without one never fail.

### WebApp framework upgrades (`stdlib/web.matrix` + `src/stdlib/net.rs`)

- `app.static_files(dir, url_prefix)` — content-typed static serving with
  binary bodies (`http_write_response` now writes `Value::Bytes` verbatim),
  `..` traversal → 403, misses → 404.
- Cookies — response `cookies` map → individual `Set-Cookie` headers
  (`header_pairs` accepts a list of `{name, value}` maps so several
  Set-Cookie headers can coexist); `cookie(name, value, opts)` builder.
- `SessionStore` — stateless signed-cookie sessions over
  `jwt_sign`/`jwt_verify`.

Tests: `tests/web_framework_tests.rs` — the full round trip (route, login
sets HttpOnly signed cookie, the cookie authenticates a profile route,
static content-type + binary body, 403/404/401) on both engines.

### Verification

Core suite green (664 tests at the time of writing), differential 25/25,
clippy `-D warnings` clean on core and gui, rustfmt clean, native + wasm
builds compile. CI gates unchanged (no new crates, no downloads).

---

## Single-file `.exe` pipeline audit — 2026-10-29

Scope: `spid3r build --embed/--bytecode/--bundle-runtime`, `spid3r gui
--payload`, the payload format, runtime extraction, the Windows link step, and
the GUI subsystem. Every item below was reproduced against a release build on
Windows before it was fixed, and each fix has a test in
`tests/single_file_app.rs` (end-to-end) or the unit tests named in the table.

### Findings

| # | Severity | Finding | Fix |
|---|---|---|---|
| SF-1 | **HIGH** | An executable produced by `spid3r build` imported `LLVM-C.dll` whenever it was linked without `--release` (`-opt:noref` kept the staticlib's inkwell references). It died with `0xC0000135 STATUS_DLL_NOT_FOUND` on any machine without LLVM — the exact machine `--bundle-runtime` exists for. Only `--release` dead-stripping hid it. | `link_windows` now passes `/DELAYLOAD:LLVM-C.dll` + `delayimp.lib`, mirroring `build.rs`. Verified by parsing the PE import table. |
| SF-2 | **HIGH** | Both `spid3r.exe` and every built app imported `VCRUNTIME140.dll` / `VCRUNTIME140_1.dll`, i.e. depended on the Visual C++ Redistributable, which is not part of a bare Windows install. | `.cargo/config.toml` builds MSVC with `+crt-static`; `link_windows` links `libcmt`/`libvcruntime`/`libucrt` when the toolchain is static. Both binaries now import system DLLs only. |
| SF-3 | **HIGH** | A payload that ships `.matrix` **source** could not `import` a single stdlib module on a clean machine (`module 'semver' not found`): the stdlib table was compiled for `wasm32` only, and the extracted runtime lives alone in the user cache. | `stdlib_embed` is compiled on every target and `modules::embedded_module` uses it as the fallback after the filesystem search (a project-local `stdlib/` still wins). |
| SF-4 | **HIGH** | `cache_runtime` wrote the extracted runtime straight to its final path with a size-only check. Two launches at once could execute a partially written file, and a same-size file planted in the cache was executed without question. | Atomic write (unique temp file + rename) with a winner/loser handshake, and the existing file is re-hashed before use. Prunes stale runtime directories (keep current + two newest). |
| SF-5 | **MEDIUM** | `payload::decode_body` used unchecked `at + len` arithmetic on attacker-controlled lengths: a crafted trailer/body wrapped and panicked on a slice index instead of returning an error. | Checked arithmetic throughout; entry names must be relative, slash-separated and duplicate-free; the writer rejects duplicates. |
| SF-6 | **MEDIUM** | A build could succeed and leave an unusable app: `--bytecode` was not validated, duplicate `--embed` names silently collapsed in the reader's map, and `--bundle-runtime` accepted a payload with no program. All three failed at launch, where a windows-subsystem exe cannot print anything. | Payload collection and validation moved **before** compile/link; each case is a `error[build]` diagnostic. A failed validation no longer leaves a half-built exe. |
| SF-7 | **MEDIUM** | `--icon missing.ico` silently produced an icon-less binary; an `rc.exe` failure was swallowed. `--product-name`/`--version` were interpolated into the `.rc` file unescaped (a quote could start a new directive). | Missing/empty icon and `rc.exe` failure are build errors when `--icon` was given; RC strings are escaped. |
| SF-8 | **MEDIUM** | `spid3r gui app.matrix --html page.html` passed `["--html", "page.html"]` to the program's `args()`; the filter removed flag *values* by string match instead of parsing. | Flags are consumed by the parser; positionals after the program file become `args()` (as with `run`), and `--payload` + a source file is now an error. |
| SF-9 | **MEDIUM** | `gui --payload <exe> --html page.html` used the string as a payload entry name unconditionally and served 404 for every request, including the page. | `--html` accepts a payload entry or a file on disk; anything else is an error that lists the payload's `.html` entries. |
| SF-10 | **LOW** | `-o app` wrote an extensionless file Windows will not launch; a foreign `--target` compiled and then failed in the linker with an unrelated message. | Extensionless output gains the platform suffix; `--target` must be the host triple, with a diagnostic that says why. |
| SF-11 | **LOW** | `--bundle-runtime` made the GUI side copy the ~10 MB runtime entry into its asset map, and the launcher copy it again; the exe grew by a full copy per rebuild in the user cache. | `payload::read_filtered` parses without copying entries the caller does not need; both sides filter the runtime entry; the cache is pruned. |
| SF-12 | **LOW** | Failures in a packaged app were invisible (no console) — the launcher's `run_payload` error and the runtime's diagnostics went to handles nobody reads. | `src/logging.rs`: append-only log at `%LOCALAPPDATA%\Spid3r\logs\spid3r.log` (1 MiB rotation, `SPID3R_LOG=0` opt-out, `SPID3R_LOG_FILE` override). The `gui` command, the launcher and extraction record to it; console commands keep using stderr. |
| SF-13 | **LOW** | `stdlib_embed`'s test asserted a module (`json`) that no longer exists in `stdlib/`; it never ran because the module was wasm-only. | Test now uses a real module and checks every entry. |
| SF-14 | **INFO** | The `run_payload` entry name was spelled in two places (`build` writer, runtime reader). | One constant, `payload::RUNTIME_ENTRY`. |

### Verification

- `tests/single_file_app.rs` (10 tests): payload contents, bytecode execution via
  `gui --payload --dry-run`, clean-machine stdlib resolution, truncated-payload
  handling, PE import table of a built app, build-time validation, extensionless
  output, `--payload`/file conflict, `--html` resolution, `args()` isolation.
- Unit tests: `native::payload::tests` (round trip, truncation, impossible
  lengths, unsafe names, duplicates), `stdlib::runtime_cache_tests` (layout,
  8-way concurrent extraction, tamper replacement, pruning), `logging::tests`.
- Regression suites re-run green: `gui_tests`, `bundle_tests`, `native_tests`,
  `native_catchability`, `embed_tests`, `artifact_hardening`,
  `issues_resolution_tests`, `bug_report_fixes`, `f2_cli_outputs`, lib unit tests.
- Manual end-to-end on Windows: build a GUI app with
  `--bytecode --embed ui --bundle-runtime --subsystem windows`, launch it twice
  concurrently, confirm both windows run and one intact runtime is extracted;
  run the same app from an isolated toolchain copy with no `stdlib/` beside it.

---

## Pre-launch review — single-file `.exe` pipeline (2026-10-30)

An independent three-leg review (code review, security audit, QA) of the change
set above, run against the real release build. Full report:
`deliverables/gstack/pre-launch-check-spid3r-single-file-2026-10-30.md`.

Blocking items found and fixed:

| # | Finding | Fix |
|---|---|---|
| PL-1 | `LLVM-C.dll` was resolved by bare name through the unhardened loader search, so a DLL placed beside `spid3r.exe` **or in the project directory** ran its `DllMain` inside the toolchain (reproduced: 38-byte garbage DLL → `0xC06D007E`) | `cmd_build` preloads the genuine DLL by absolute path (`LoadLibraryExW` + `LOAD_WITH_ALTERED_SEARCH_PATH`) from a hardened search that excludes CWD entirely and ranks the app directory last |
| PL-2 | A corrupt payload was swallowed and fell back to `%SPID3R_EXE%` / the install / `PATH` | `resolve_runtime` distinguishes "no payload" from "payload cannot be parsed"; the latter is fatal |
| PL-3 | Cutting the 24-byte trailer made the payload vanish, with the same fallback | new `run_payload_strict()` native; the generated `--bytecode` stub calls it and treats a missing payload as fatal |
| PL-4 | `gui --payload App.exe alpha` was rejected as a source-file conflict, so a payload app could never receive `args()` | positionals after `--payload` go to `script_args` |
| PL-5 | `.cargo/config.toml` was untracked; `git commit -am` would silently drop static-CRT | staged, plus a test asserting `+crt-static` on MSVC |
| PL-6 | Missing `LLVM-C.dll` aborted with `0xC06D007E` and no diagnostic | pre-flight in `cmd_build` with an actionable `error[build]` |
| PL-7 | CRT import-lib set was chosen from the compiler's cfg, which can disagree with the staticlib actually linked | link retries with the other set on CRT-class linker errors |
| PL-8 | `CacheState::Unreadable` executed the unverifiable file (a deny-read ACE bypassed the hash check) | extracts the held bytes to a fresh directory instead; never runs unverified bytes |
| PL-9 | Every app shared one WebView2 profile and origin; concurrent launches failed ~50% (`0x80070057`) | per-PID profile for ephemeral runs, per-app hash for `--persist`; 3 concurrent launches now pass |
| PL-10 | `--embed` followed junctions and had no depth/size caps | `symlink_metadata` + 32 levels / 10k entries / 512 MiB |
| PL-11 | `--html ../site/page.html` served 403 for the entry document (blank window) | root/entry derived from the canonical path; CWD-root preserved for pages under CWD |
| PL-12 | Test suite could pass while testing nothing (import test only built `--release`; `can_link_native` skipped silently) | non-release import assertion; only environmental failures skip, everything else panics |

Two findings were **withdrawn by their authors** after challenge: FNV-1a
invertibility (a 2^64 second-preimage, not an algebraic solve — SHA-256 kept as
hygiene) and the "make `find_llvm_lib_dir` optional" advice (the process aborts
before the linker runs, so the pre-flight is the only place a message can be
produced).

Verification after the fixes: `cargo test --release` 96 suites green, clippy
clean in all three configurations, rustfmt clean, wasm builds, single-file app
imports only system DLLs, three concurrent launches succeed, and a source
payload with `import "semver"` runs from a directory containing only
`spid3r.exe`.
