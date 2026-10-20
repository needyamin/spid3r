# Stage 4 — toolchain & docs hygiene (AUDIT_2026-10-20 §6)

Date: 2026-10-21 · Scope: A-10 (semantic checker bypasses), F2 (`spid3r.pdb`
collision), F1 (rustfmt drift), A-18/F3 (counts & docs), A-12 (AOT `unsafe`
surface) + the fuzz job that never ran.

## 1. What was fixed

| Item | Defect | Fix |
|---|---|---|
| A-10 | `a[ghost()] = 1` passed `check` — the index expression of an index-assign target was never visited | `check_assign_target` now calls `check_expr(index)`, so unknown names, arity errors and privacy violations in that position fail statically |
| A-10 | `fn main(x)` passed `check` and failed at startup | Declaration-time validation: `main` must take no parameters; its return type must be absent or `int` (unknown names still report `unknown type`) |
| A-10 | `Ty::Fn(_) → Ty::Fn(_)` was always assignable, so signatures were ignored | `assignable` compares `FnSig`s (arity, invariant annotated params, covariant return; `Fn(None)` stays dynamic) and the diagnostic prints `expected fn(int) -> int, got fn(int) -> string` |
| F2 | bin and lib both produced `target/debug/spid3r.pdb` (cargo #6313) | **Lib target renamed to `spid3r_lang`** (the cdylib was the colliding artifact — verified: `["rlib","cdylib"]` collides, `["rlib","staticlib"]` does not). Package and bin stay `spid3r`; staticlib is `spid3r_lang.lib` / `libspid3r_lang.a`, and `find_staticlib` still probes the pre-rename name so old installs keep working |
| F1 | `cargo fmt --check` failed on ~70 files | one `cargo fmt` normalization commit; `cargo fmt --check` added to CI so it cannot drift again |
| A-18 | Four different test totals in the docs (418+/447/454/467), "23 examples" vs 25 files, fuzz "5 surfaces" vs 3 targets, `fuzz_tests=10` vs 3, `audit_regressions` 30/78 vs 83 | `tools/count_tests.sh` is the single source of truth (asks cargo, prints tests/examples/fuzz); every quoted number replaced with its output |
| A-18 | `tools/bench_compare.py` hardcoded `C:\Program Files\PHP\v8.3`, wrote a hardcoded "Microsoft Windows 11 x64" host string into the report, and **deleted** a root `benchmarks.html` it did not create | PHP discovered via `SPID3R_PHP_DIR` → PATH → usual Windows installs; host string is the real `platform.system()/release()/machine()`; the root file is reported, never deleted |
| A-12 | AOT: non-atomic `UpvalueSlot.value` (spawned tasks share captured cells → data race), `env_slot` unbounded index, no `catch_unwind` at `extern "C"` | `AtomicPtr<Value>` with SeqCst load/store; environment arrays carry a length header and both slot accessors bounds-check; `ffi_guard` wraps `rt_start`, `call`, `method`, `make_fn`, `global_exists` so a panic becomes a script error (or a safe sentinel) instead of an abort |
| A-18 | `fuzz/` shipped 3 targets that nothing ran | `.github/workflows/fuzz.yml`: nightly + cargo-fuzz matrix (weekly, on demand, and on PRs touching lexer/parser/semantic/bytecode), crash artifacts uploaded; targets updated for the lib rename |

## 2. Evidence

```
case                                   before                      after
a[ghost()] = 1                         check: ok                   unknown name 'ghost'
fn main(x)                             check: ok                   fn main must take no parameters (found 1)
fn main() -> string                    check: ok                   fn main must return int ... not string
f = g (fn(int)->int vs fn(int)->string) check: ok                  expected fn(int) -> int, got fn(int) -> string
cargo build (any)                     2x "output filename collision"  no warning
cargo fmt --check                     FAIL (70 files)             PASS
README / doc.html test totals         418+ / 447                 484 core / 500 total (script output)
audit_regressions documented count    30, then 78                83
fuzz_tests documented count           10                          3
tools/bench_compare.py host card      hardcoded Windows 11 x64    actual host + best-of-N
tools/bench_compare.py root file      deleted                     reported, untouched
AOT captured cell                     plain *mut (race)           AtomicPtr, SeqCst
```

## 3. Tests added / updated

* `tests/fix_semantic_priv.rs` +3: index-assignment index checking (unknown
  name, arity, privacy), `main` signature validation, function-type signature
  comparison.
* `tests/native_tests.rs` +1: `native_matches_tree_on_captured_state_and_spawned_tasks`
  — closure counters and a spawned task mutating a captured variable must match
  the tree engine (exercises the atomic cells and the env header).
* `tests/sched_limits.rs`, `tests/artifact_hardening.rs`, `tests/pkg_tests.rs`,
  `tests/stage2_concurrency.rs`: unchanged behaviour, updated for the lib rename
  (`use spid3r_lang::…`).
* `tools/count_tests.sh`: not a test, but the mechanism that keeps the counts
  honest (`--run` for executed, `--static` for the grep number).

## 4. Gates (executed, sequential)

| Gate | Result |
|---|---|
| `cargo build --no-default-features --all-targets` | OK, **no** `spid3r.pdb` collision warning |
| `cargo clippy --no-default-features --all-targets -- -D warnings` | 0 warnings |
| `cargo clippy --no-default-features --features native --all-targets -- -D warnings` | 0 warnings |
| `cargo test --no-default-features` | **484 passed, 0 failed** |
| `cargo test --no-default-features --features gui --test gui_tests` | 10/10 |
| `cargo test --no-default-features --features native --test native_tests` | 6/6 |
| `spid3r test` (tree == vm == exec) | 25 ok, 0 failed |
| `cargo build --target wasm32-unknown-unknown --release --lib --no-default-features` | OK → `spid3r_lang.wasm` (1.63 MB) |
| `cargo fmt --check` | clean (was: 70 files) |
| `spid3r fmt --check` | all unchanged, exit 0 |

`bash tools/count_tests.sh` → 484 core + 10 GUI + 6 AOT = **500 tests**, 25
example files (24 programs + 1 shared module), 3 fuzz targets.

## 5. Deliberate decisions

* **The lib rename is the breaking part of this stage.** `use spid3r::…` becomes
  `use spid3r_lang::…` (documented in `docs/EMBEDDING.md`), the staticlib is
  `spid3r_lang.lib`, and the wasm artifact is `spid3r_lang.wasm` (the deployed
  `web/spid3r.wasm` keeps its name; only the copy source changed, and the
  checked-in module was rebuilt). The alternative — renaming the CLI binary —
  would break every user script; the audit allowed either. The old staticlib name
  is still searched so an installed toolchain keeps working.
* **`cargo fmt` runs as its own commit.** Formatting 70 files in the same commit
  as behavioural changes would make review impossible, so the Stage-4 changes are
  committed first and the normalization is a separate, diff-only commit.
* **`cargo fmt --check` is now gated in CI.** The tree is formatted, so the gate
  is cheap; without it the drift simply returns.
* **A-13…A-17 are not in this stage's numbered plan** (closure/environment cycle
  leaks, `deep_clone` of cyclic values aliasing the original, the quadratic regex
  matcher, `net.rs` buffer growth / slowloris, deep-recursive-container `Drop`).
  They are runtime correctness/DoS findings, not toolchain/docs hygiene, and each
  needs its own design decision; they remain open.
* **Fuzz targets are not run locally.** No nightly toolchain / cargo-fuzz on this
  machine (the audit noted the same), so the job is CI-only; the targets were
  updated for the lib rename but have not been executed here.
