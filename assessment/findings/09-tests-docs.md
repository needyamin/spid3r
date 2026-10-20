# 09 — Tests, fuzz, benchmarks, and documentation consistency

> **Status 2026-10-21 (verification round).** This file is the 2026-10-20
> static record and is not rewritten. What changed since:
>
> - **Every claim in the table below has been resolved** (audit A-18 / F3):
>   - The four circulating test totals (447 / 454 / 467 / 425 / 279) are replaced by one
>     canonical counter, `tools/count_tests.sh` — currently **500 tests** (484 core + 10
>     GUI + 6 AOT), **25 example files** (24 programs + 1 shared module), **3 fuzz
>     targets** — and README, doc.html, `docs/AUDIT_REPORT.md`, `sitemap.txt` and the
>     audit notes quote its output.
>   - "23 examples" → 25 files (24 runnable + `math_util.matrix`).
>   - Fuzz surfaces: three targets, stated as three everywhere; `fuzz_tests=10` corrected
>     to 3; a nightly `.github/workflows/fuzz.yml` now actually runs them.
>   - `audit_regressions` count 30 → 83 (and documented in `sitemap.txt`).
>   - `tools/bench_compare.py` no longer hardcodes a host string or the PHP path, and no
>     longer deletes a root `benchmarks.html`.
>
> Full status table: `assessment/AUDIT_2026-10-20.md` §9. Reproduce the
> dynamic checks with `python audit_notes/probes/verify_2026-10-21.py`.


Author note: the dispatched child exhausted its budget before it could write this file; its
full findings were recovered from the run transcript (`.zwork/runs/38dfa112/8-研究员.md`)
and are transcribed here by the coordinator, then cross-checked against the coordinator's
own dynamic baseline (cargo test runs, 2026-10-20).

## Claim-by-claim verification table

| README / docs claim | Status | Evidence |
|---|---|---|
| "447 Rust tests (418 core + 29 GUI/AOT-headless)" | **Different number** | Static `#[test]` count = 467 (400 in tests/ across 32 files + 67 in src/). Coordinator dynamic run (core config): 440 passed incl. 2 doc-tests = 438 non-doc; +10 gui_tests (`--features gui`) +4 native_tests (`--features native`) = 454 executed total. The sub-claim "418 core" matches neither 438 nor 400; "29 GUI/AOT" reproduces only if you count bridge src-unit tests (13) + native/modules (2) that compile only under features (10 gui + 13 bridge + 4 native + 2 = 29). |
| "10 headless GUI bridge tests" | **Verified** | `cargo test --no-default-features --features gui --test gui_tests` → 10 passed (coordinator run). |
| "25/25 differential example programs" | **Verified** | `cargo run --no-default-features -- test` → "25 ok, 0 failed"; 25 top-level examples confirmed. |
| "23 runnable example programs/libraries" (repository layout) | **False** | 27 example .matrix files exist; also self-contradicts the 25/25 differential claim (25 runnable mains vs "23 examples"). |
| "8 benchmarks" | **Verified** | 8 files in benchmarks/. |
| Fuzz targets for "lexer, parser, semantic checker, formatter, and bytecode verifier" | **Partially false** | fuzz/ has 3 targets only: fuzz_lexer, fuzz_parser, fuzz_bytecode. No formatter target; semantic is only reached chained behind the parser target. |
| `spid3r test` = 3-way differential (tree == vm == exec) | **Verified** | Output lines "ok <name> (tree==vm==exec)"; library modules checked only. |
| difftest.sh / bench_compare.py exist | **Verified** | Both exist under tools/. difftest.sh hardcodes `.exe`. bench_compare.py (uncommitted local changes) hardcodes a host string in results and auto-deletes root benchmarks.html; was run with runs=1. |
| CI wasm32 toolchain installed in CI | **Verified** | rustup target add in .github/workflows/ci.yml wasm job. |
| fmt/clippy workflow "matches CI" | **Verified** | ci.yml core job runs exactly those commands; coordinator reproduced both green locally. |
| 0 `#[ignore]` / `todo!()` / `unimplemented!()` / `#[should_panic]` in tests | **Verified** | Static grep found none. |
| docs/AUDIT_REPORT.md "30 regressions" | **Stale** | tests/audit_regressions.rs now has 83 tests; 42 newest are undocumented anywhere. |
| audit_notes' "fuzz_tests=10" | **False** | tests/fuzz_tests.rs contains 3 tests. |

## Top consistency concerns (ranked)

1. **Four mutually inconsistent test totals circulate** (static 467 / README 447 / docs 308 / notes 340) — none matches the dynamically executed 454. Any "all tests pass" statement is unverifiable without a canonical count. Severity MED, confidence HIGH.
2. **42 newest audit_regressions tests undocumented** — the regression suite grew 30→83 (commit "First-Party Standard Libraries") without README/AUDIT_REPORT updates. Severity LOW-MED, confidence HIGH.
3. **README fuzz-coverage overstatement** — claims five fuzz surfaces; three exist, formatter uncovered. Repeated as "fuzz_tests=10" in two audit notes. Severity LOW-MED, confidence HIGH.
4. **README 23-vs-25/27 example self-contradiction** — layout table says 23 examples; differential suite runs 25; 27 files exist. Severity LOW, confidence HIGH.
5. **tools/bench_compare.py (uncommitted changes)** — hardcodes host string into results.json, was last run with runs=1 (statistically meaningless), and auto-deletes root benchmarks.html (the working tree shows exactly that deletion pending). Severity LOW-MED, confidence HIGH.

## Coordinator cross-check (dynamic, 2026-10-20)

- `cargo test --no-default-features`: **440 passed, 0 failed, 0 ignored** (52 lib unit + 386 integration + 2 doc-tests; gui_tests/native_tests compile to 0 tests in core config).
- `--features gui`: gui_tests 10/10. `--features native`: native_tests 4/4. Executed total 454.
- `cargo run -- test` (differential): 25 ok / 0 failed.
- Standalone `cargo fuzz` harness **not runnable here**: no nightly toolchain, cargo-fuzz not installed (limitation; fuzz targets verified present and read, not executed).
