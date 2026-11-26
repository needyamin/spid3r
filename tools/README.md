# Spid3r Developer Tools (`tools/`)

Scripts and automation tooling are organized by functional domain so developers can locate CI gates, benchmarks, self-hosting parity suites, and release pipelines immediately.

## Directory Layout

| Folder | Scripts | Purpose |
| :--- | :---: | :--- |
| **`tools/ci/`** | 6 | Local & CI verification gates (`gate.sh`, `ci_local.sh`, `difftest.sh`, `native_diff.sh`, `check_unsafe_safety.py`, `count_tests.sh`). |
| **`tools/bench/`** | 11 | Phase 0 compute harness (`phase0_*.py`), Stage-0 vs selfhost throughput (`{lex,parse,check}_bench.py`), and `docs/html/benchmark.html` generator (`bench_report*.py`). |
| **`tools/selfhost/`** | 17 | Frozen corpus manifest (`corpus_manifest.py`), determinism & parity harnesses (`selfhost_*_parity.sh`), and grammar/semantic fuzzers (`fuzz_*_parity.py`). |
| **`tools/release/`** | 4 | Self-hosted release packager (`release_selfhost.sh`), SHA-256 staticlib manifest/verifier (`runtime_artifact.sh`), PGO builder (`pgo_build.sh`), and Windows PE probe (`pe_resource_probe.ps1`). |
| **`tools/wasm/`** | 5 | Embedded base64 WASM bundle builder (`gen_wasm_data.js`) and browser/Node runtime checks (`test_wasm_*.js`, `test_playground_presets.js`, `qa_wasm_test.js`). |
| **`tools/docs/`** | 3 | Live doc-claim verifier (`verify_doc_claims.js`), HTML code snippet verifier (`verify_dev_snippets.js`), and playground preset updater (`write_presets.py`). |
| **`tools/codegen/`** | 4 | Static table generators for `src/stdlib/` and `tests/data/` (`gen_blowfish_pi.py`, `gen_tzdata.py`, `gen_unicode_tables.py`, `gen_unicode_tz_cases.py`). |
| **`tools/db/`** | 2 | Protocol-accurate Redis/PostgreSQL/MySQL test servers (`db_test_servers.py`) and local smoke checker (`check_db_servers.py`). |

## Common Commands

```bash
# Core CI & differential gates
bash tools/ci/gate.sh
bash tools/ci/difftest.sh
python tools/ci/check_unsafe_safety.py
bash tools/ci/count_tests.sh

# Selfhost corpus & parity
python tools/selfhost/corpus_manifest.py verify
bash tools/selfhost/selfhost_semantic_parity.sh target/release/spid3r tree

# Benchmarks & enterprise HTML report
python tools/bench/phase0_manifest.py --verify
python tools/bench/phase0_fixtures.py --verify
python tools/bench/bench_report.py --render
python tools/bench/bench_report.py --verify-sync

# WASM & Documentation checks
node tools/wasm/test_wasm_runtime.js
node tools/wasm/test_wasm_embedded.js
node tools/docs/verify_doc_claims.js
node tools/docs/verify_dev_snippets.js
```
