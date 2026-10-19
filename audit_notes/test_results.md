# Spid3r — Validation Audit: Test & Measurement Results

- Date: 2026-10-19
- Repo: `C:\Users\needyamin\OneDrive\Desktop\GaliLang` (OneDrive-synced)
- Shell: PowerShell 5.1, minimal environment; absolute paths used throughout
- `profile.dev` `incremental = false` left as-is (OneDrive requirement; not touched)
- Toolchain: **cargo 1.98.1** (797e8a9bc 2026-08-05) / **rustc 1.98.1** (48a229cea 2026-09-01)
- Scope: validation only — no source, test, example, doc, or packaging file was modified. The only file written is this notes file; `target\` artifacts were rebuilt by cargo.

## Results

| # | Check | Command | Outcome | Evidence |
|---|-------|---------|---------|----------|
| 1 | Core build (`--no-default-features`) | `cargo build --no-default-features` | **PASS** | First invocation was fully cached: `Finished dev profile ... in 0.15s`, exit 0. To measure a real compile, `cargo clean -p spid3r` (removed 447 files, 765.4 MiB) then rebuild: `Compiling spid3r v1.0.0 ... Finished ... in 5.33s`, wall 5.4 s, exit 0. Only warnings: pre-existing `output filename collision` for `spid3r.pdb` (bin and lib share the name `spid3r`; cargo notes it may become a hard error — cargo#6313). |
| 2 | Core test suite | `cargo test --no-default-features` | **PASS** | **298 passed; 0 failed; 0 ignored; 0 filtered out** across 28 test targets (27 binaries + doc-tests). Total wall 49.1 s (includes compiling test binaries). No failing test names, no failure excerpts (none failed). Second confirmation run produced identical counts. **Claimed 298 → measured 298, delta 0.** |
| 3a | Differential via cargo | `cargo test --test difftest --no-default-features` | **PASS** | `running 3 tests` → `3 passed; 0 failed; 0 ignored`, 0.64 s, exit 0. |
| 3b | Differential via CLI | `.\target\debug\spid3r.exe test` | **PASS** | `23 ok, 0 failed`, exit 0, 0.6 s. Every example line reads `ok <name>.matrix (tree==vm==exec)`; `math_util.matrix` reports `ok (library: checked)`. **Claimed 23/23 → measured 23/23, delta 0.** |
| 4a | Default build (gui + native) | `cargo build` | **PASS** | Exit 0, `Compiling spid3r v1.0.0 ... Finished dev profile ... in 7.76s` (wall 7.8 s). wry/tao/inkwell rlibs were already present in `target\debug` from earlier builds, so no fresh crate download occurred this run; `build.rs` LLVM C-API linking succeeded (no link/LLVM error). |
| 4b | GUI tests (only because 4a passed) | `cargo test --test gui_tests` | **PASS** | `running 10 tests` → `10 passed; 0 failed; 0 ignored`, 2.18 s, exit 0. **Claimed 10 → measured 10, delta 0.** |
| 5a | Version | `.\target\debug\spid3r.exe version` | **PASS** | Prints `spid3r 1.0.0 (language 1.0)` + `Rust host, no runtime dependencies.`; exit 0. |
| 5b | hello.matrix, tree engine | `.\target\debug\spid3r.exe run --engine tree examples\hello.matrix` | **PASS** | Output: `hello` (no trailing newline emitted); exit 0. |
| 5c | hello.matrix, VM engine | `.\target\debug\spid3r.exe run --engine vm examples\hello.matrix` | **PASS** | Output: `hello` (no trailing newline emitted); exit 0 — byte-identical to the tree run. |
| 5d | Formatter on directory | `.\target\debug\spid3r.exe fmt --check examples` | **FAIL (observed quirk)** | Exit 2: `error[usage]: usage error: cannot read 'examples': Access is denied. (os error 5)`. `fmt` does not expand directories — it attempts to read the argument as a file. Recorded, **not fixed** per instructions. |
| 5e | Formatter on files | `.\target\debug\spid3r.exe fmt --check <6 files>` and all 23 `examples\*.matrix` via shell glob | **PASS** | 6/6 and then 23/23 report `unchanged`; exit 0 both times. All shipped examples are already formatted. |
| 5f | Benchmark, VM | `.\target\debug\spid3r.exe bench examples\loops.matrix --engine vm --runs 5` | **PASS** (indicative only) | Header `best of 5 runs`; `loops  11.381ms`; `iters/sec` column prints `-`. Exit 0. |
| 5g | Benchmark, tree | `.\target\debug\spid3r.exe bench examples\loops.matrix --engine tree --runs 5` | **PASS** (indicative only) | Header `best of 5 runs`; `loops  11.486ms`; exit 0. |
| 6 | Strict clippy (core) | `cargo clippy --no-default-features -- -D warnings` | **PASS** | Exit 0, `Finished dev profile ... in 2.97s` (wall 3.1 s), zero `warning:` lines emitted. **Claimed clean → confirmed clean.** |
| 7 | Toolchain versions | `cargo --version`; `rustc --version` | **Recorded** | cargo 1.98.1 (797e8a9bc 2026-08-05); rustc 1.98.1 (48a229cea 2026-09-01). |

### Per-suite breakdown of the 298 core tests (check 2)

Counts map to targets by positional pairing with cargo's fixed target order; the three entries most likely to be misread (`audit_regressions`, `pkg_tests`, `gui_tests`) were each re-run standalone and matched.

| Target | Tests | | Target | Tests |
|---|---|---|---|---|
| unittests `src\lib.rs` | 33 | | `integration_tests.rs` | 13 |
| unittests `src\main.rs` | 0 | | `interpreter_tests.rs` | 18 |
| `audit_regressions.rs` | 39 (verified standalone) | | `lexer_tests.rs` | 12 |
| `bundle_tests.rs` | 4 | | `native_tests.rs` | 0 (feature-gated) |
| `const_module_tests.rs` | 5 | | `net_tests.rs` | 4 |
| `cycle_tests.rs` | 5 | | `oop_tests.rs` | 13 |
| `data_structure_tests.rs` | 23 | | `opt_tests.rs` | 4 |
| `debug_tests.rs` | 7 | | `parser_tests.rs` | 13 |
| `difftest.rs` | 3 (verified standalone) | | `pkg_tests.rs` | 39 (verified standalone) |
| `error_tests.rs` | 3 | | `sched_tests.rs` | 9 |
| `fmt_tests.rs` | 11 | | `vm_dispatch_tests.rs` | 5 |
| `fuzz_tests.rs` | 10 | | `wasm_tests.rs` | 6 |
| `gradual_typing_tests.rs` | 3 | | `weak_tests.rs` | 6 |
| `gui_tests.rs` | 0 (feature-gated; 10 with default features) | | Doc-tests | 0 |

Sum: 33+0+39+4+5+5+23+7+3+3+11+10+3+0+13+18+12+0+4+13+4+13+39+9+5+6+6+0 = **298**.

### Benchmark methodology (checks 5f/5g)

Debug build (`unoptimized + debuginfo`), shared machine, OneDrive-synced workspace, wall-clock best of 5 runs as reported by the tool itself. These numbers are **indicative only** — they are not a performance claim, not comparable to a release build, and the tool prints no iteration counts for this benchmark, so iters/sec is unavailable.

## Claimed vs measured

The claimed state holds exactly on every quantitative point measured. Core suite: 298/298 green (delta 0, zero ignored), across 28 targets; GUI suite: 10/10 green (total 308 = claim). The 3-engine differential is 23/23 both through `cargo test --test difftest` (3 test functions, all green) and through the CLI (`23 ok, 0 failed`, every example `tree==vm==exec`). Strict `clippy -D warnings` with `--no-default-features` is clean (0 warnings). The default-feature build (gui + native) succeeds and LLVM C-API linking works; with it, all 10 GUI tests pass. Tooling smoke is green except one reproducible usability quirk: `fmt --check` rejects a directory argument with exit 2 `Access is denied (os error 5)` instead of expanding `*.matrix` — per-file checks over all 23 examples pass. Two non-blocking observations: cargo emits a pre-existing bin/lib `spid3r.pdb` output-filename-collision warning, and `sitemap.txt` describes `audit_regressions.rs` as 30 tests while it actually contains 39 (sitemap generated 2026-10-18; stale count, not a failure). No check was skipped and nothing was fixed.

## Not verified / caveats

- Fresh dependency resolution for gui/native was not exercised: wry/tao/inkwell build artifacts were already cached in `target\debug` before this run, so a cold `cargo fetch`/compile of third-party crates was not observed (Cargo.lock is committed; crate downloads did not occur).
- LLVM version was not probed directly (no `llvm-config` query was part of the brief); evidence is limited to `build.rs` linking succeeding under the `native` feature.
- Benchmarks are debug-build, shared-machine, best-of-5 wall-clock — indicative only.
- No long-running or hang-prone path was hit; every command completed well under the 15-minute cap (slowest: core test suite, 49.1 s).
