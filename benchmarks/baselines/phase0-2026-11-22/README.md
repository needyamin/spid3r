# Phase 0 Clean Post-Cleanup Baseline (`2026-11-22`)

Archived clean baseline captured after removing the 6 non-language subsystems (`removed.txt`), enforcing the Phase 0 VM bytecode-fingerprint guard (`tests/vm_integrity_guard.rs`), and verifying all standing compiler/engine gates.

## Provenance

- **Command:** `python cli.py --runtime-comparison --runtime-repeats 7` (from `C:\Users\needyamin\OneDrive\Desktop\HLC`)
- **Spid3r binary:** `c:\Users\needyamin\OneDrive\Desktop\spid3r\target\release\spid3r.exe` (`1.1.1`, MSVC release `--features "native gui"`)
- **HLC run archive:** `runs/runtime-20261122-015236`
- **Generated UTC:** `2026-11-21T19:51:52+00:00`
- **Status:** `ok`, `workloads_verified: 5 / 5`, `variants: 44`, `warnings: []`
- **Environment:** Host (`windows`), installed host toolchains (`spid3r`, `rust 1.99.1`, `node 26`, `php 8.3`, `python 3.15`, plus comparison variants)

## 7-Repeat Compute & Memory Baseline

All 5 workloads passed checksum verification (`verified: true`):

| Workload | Checksum | Spid3r native `--release` | Spid3r VM | Spid3r bytecode exec | Spid3r tree-walk | Spid3r VM `--no-opt` | Rust `-O` | Node 26 | PHP 8.3 | Python 3.15 |
| :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| **A. Prime (`N=50,000`)** | `RESULT 5133 121013308` | **1.000 ms** *(6.55 MB)* | **8.000 ms** *(11.25 MB)* | **8.000 ms** *(10.62 MB)* | **8.000 ms** *(11.25 MB)* | 125.000 ms *(11.19 MB)* | 1.361 ms *(4.32 MB)* | 1.835 ms *(38.66 MB)* | 16.943 ms *(14.87 MB)* | 23.914 ms *(11.28 MB)* |
| **B. JSON (`200 x 600`)** | `RESULT 249868800 9163` | **65.000 ms** *(10.45 MB)* | **93.000 ms** *(11.66 MB)* | **93.000 ms** *(11.06 MB)* | **93.000 ms** *(11.70 MB)* | 95.000 ms *(11.60 MB)* | — | 18.118 ms *(41.23 MB)* | 71.719 ms *(15.25 MB)* | 56.673 ms *(12.93 MB)* |
| **C. Map (`64 x 100,000`)** | `RESULT 44432313 100000` | **3.000 ms** *(6.66 MB)* | **7.000 ms** *(11.32 MB)* | **7.000 ms** *(10.67 MB)* | **7.000 ms** *(11.29 MB)* | 27.000 ms *(11.28 MB)* | 7.102 ms *(4.48 MB)* | 1.637 ms *(38.70 MB)* | 3.735 ms *(14.86 MB)* | 10.857 ms *(11.29 MB)* |
| **D. Text (`50,000 rows`)** | `RESULT 1295428 300465975` | **8.000 ms** *(6.79 MB)* | **28.000 ms** *(11.27 MB)* | **28.000 ms** *(10.70 MB)* | **28.000 ms** *(11.28 MB)* | 37.000 ms *(11.19 MB)* | 2.496 ms *(4.32 MB)* | 3.877 ms *(43.11 MB)* | 6.136 ms *(14.80 MB)* | 15.324 ms *(11.24 MB)* |
| **E. Matmul (`128 x 128`)** | `RESULT -1985 -40` | **86.000 ms** *(7.68 MB)* | **168.000 ms** *(11.82 MB)* | **167.000 ms** *(11.15 MB)* | **168.000 ms** *(11.80 MB)* | 352.000 ms *(11.70 MB)* | 0.695 ms *(4.40 MB)* | 4.733 ms *(41.48 MB)* | 30.324 ms *(15.54 MB)* | 182.554 ms *(11.82 MB)* |

Notes from `runtime_comparison.json` and `suitability.json`:
- `now_ms()` 1 ms timer floor flags (`< 10.0 ms`): flagged on `A` (`spid3r_native`, `spid3r_tw`, `spid3r_vm`, `spid3r_bytecode`), `C` (`spid3r_native`, `spid3r_tw`, `spid3r_vm`, `spid3r_bytecode`), and `D` (`spid3r_native`); `worst_cv_pct: 12.41%` across all 44 variants (below the 15% noise threshold).
- Geomean slowdowns: `Spid3r 1.1.1 native --release` is `0.692x` PHP, `0.318x` Python, `4.822x` fastest; `Spid3r 1.1.1 VM` is `1.961x` PHP, `0.902x` Python, `13.663x` fastest.
- Suitability (`suitability.json`): rank **#14**, overall score **22.8 / 100** (`compute_performance: 0.38`, `memory_efficiency: 6.12` [geomean 10.4 MiB], `cpu_efficiency: 0.0`, `ecosystem_maturity: 3`, `ecosystem_libraries: 3`, `http_throughput/latency/stability`: unmeasured / `null`).

## Integrity Disclosure (`VM-1`)

At Phase 0 capture time, `src/vm/mod.rs:1877-2000` still contains the legacy `VM-1: Batched unboxed i64 trial-division inner loop` bytecode lookahead affecting interpreted `prime` (`8.000 ms` VM vs `125.000 ms` VM `--no-opt`). It is locked against growth by `tests/vm_integrity_guard.rs`, disclosed in `docs/VM_NOTES.md`, and scheduled for deletion in **Phase 3** once pre-decoded dispatch and general compile-time superinstructions reach `<= 6.0 ms` honestly.
