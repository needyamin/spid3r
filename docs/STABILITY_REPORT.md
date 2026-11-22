# Spid3r v1.2 Pre-Release Stability, Footprint & Performance Report

> **Policy:** Every number in this report is directly observed from harness execution on this repository. Unrun gates (the 24-hour Docker cgroup soak campaign and CI release tagging) are explicitly marked **PENDING** with reproducible commands rather than estimated or stubbed.

---

## 1. Architecture & Language Improvements Completed (Phases 0–6)

1. **Benchmark Integrity (`vm_integrity_guard`):** Removed the legacy prime-loop bytecode fingerprint in `src/vm/mod.rs` and locked guard tests (`tests/vm_integrity_guard.rs`) so all engines execute general-purpose VM/AOT optimization passes cleanly.
2. **AOT Row/Loop Unboxing (`src/native/ir.rs`):** Hoisted loop-invariant row handles `(ptr, len, gen)` for 2D homogeneous integer/float lists with bounds-check elimination (BCE) and register-resident inner loops.
3. **Single-Threaded `FastMutex` + Swiss/Fx Map Path (`src/value/`):** Lock-free container fast path prior to first `spawn`, plus fast non-cryptographic key hashing for internal/non-adversarial hot paths while preserving randomized seeding semantics.
4. **Bytecode VM Superinstructions & Pre-Decoding (`src/vm/opt.rs`, `src/vm/mod.rs`):** Eliminated instruction-decode overhead and bytecode peeking with general fused superinstructions and typed fast paths.
5. **Arena JSON Parser, SWAR Scanning & Key Interning (`src/stdlib/serialize.rs`, `src/stdlib/swar.rs`, `src/value/collections/intern.rs`):** 8-byte SWAR delimiter scanning + key interning for low-allocation JSON parsing.
6. **Split Staticlib & 16-Byte Compact `Value` (`src/stdlib/mod.rs`, `src/native/rt.rs`, `src/value/mod.rs`):** Split staticlib registration into `spid3r_reg_core` + 10 opt-in subsystem groups (`json`, `fs`, `net`, `db`, `media`, `tz`, `unicode`, `crypto`, `data`, `info`), severed cross-module linker anchors, shrunk `Value` with 8-byte `ThinArcStr` and boxed `Result`, and deferred cold-start CLI setup after `--version`/`--help`.
7. **HTTP/1.1 Keep-Alive Event-Loop & Buffer Pooling (`src/stdlib/net.rs`, `stdlib/http.matrix`):** SWAR HTTP header boundary scan, per-connection 4 KiB buffer recycling (64 KiB shrink cap), cached peer/method/version tokens, `WSAPoll`/`poll` blocking under `sched::enter_blocked()`, `O(1)` static route dispatch (`static_routes`), and default 4-worker accept pool for unbounded non-TLS listeners.

---

## 2. Measured Footprint & Compute Results (Windows x86_64, MSVC + LLVM 23.1.0)

### 2.1 Binary Footprint & Cold Start (`spid3r build --release`)
- `hello.exe` binary size: **`1,201,152` bytes** (`1.15 MiB`, down from `3,664,896` bytes prior to Phase 5 staticlib splitting)
- `empty.exe` binary size: **`1,143,808` bytes** (`1.09 MiB`)
- `empty.exe` Peak Working Set RSS: **`4,829,184` bytes** (`4.61 MiB`, measured via OS process metrics; target `< 4.0 MiB` left open in `plan.html` `p5-4`)
- `empty.exe` `procmetrics` cold-start wall time: **`3.62 ms`**

### 2.2 Compute Workload Verification (HLC 7-Repeat Median Comparison)
All 5 HLC compute checksums verified exact against pinned expected results (`spid3r_native`, `--runtime-repeats 7`, archive `HLC/runs/runtime-20261122-075039`):
- **`prime`:** `1.000 ms` (`spid3r_native`) vs Rust `-O` `1.357 ms`, Node 26 `1.705 ms`, PHP `16.863 ms`, Python `23.745 ms`
- **`json`:** `45.000 ms` (`spid3r_native`), `47.000 ms` (`tree`/`vm`/`exec`) vs Python `56.473 ms`, PHP `71.515 ms`, Node 26 `17.898 ms`
- **`map`:** `2.000 ms` (`spid3r_native`) vs PHP `3.692 ms`, Rust `7.079 ms`, Python `10.712 ms`, Node 26 `1.609 ms`
- **`text`:** `1.000 ms` (`spid3r_native`) vs Rust `-O` `2.519 ms`, Node 26 `3.356 ms`, PHP `6.109 ms`, Python `15.225 ms`
- **`matmul`:** `1.000 ms` (`spid3r_native`), `2.000 ms` (`tree`/`vm`/`exec`) vs Rust `-O` `0.693 ms`, Node 26 `4.855 ms`, PHP `30.216 ms`, Python `184.971 ms` (checksum `RESULT -1985 -40`)
- **Cross-runtime geometric mean:** `0.16x` vs PHP, `0.07x` vs Python, `1.35x` vs fastest compiled runtime.

---

## 3. Standing Quality & Parity Gates Verified

- `cargo fmt --check`: PASS
- `cargo check --no-default-features --all-targets`: PASS (`0` warnings)
- `tools/gate.sh`: `8/9` in initial pass (`test [default]`, `clippy [default]`, `test [gui]`, `clippy [gui]`, `test [native]`, `clippy [native]`, `clippy [native+gui]`, `build --release --features "native gui"` PASS; one transient failure in `phase5_hardening::broken_pin_blocks_program_commands_but_keeps_diagnostics_available`, which passed `6/6` on direct re-run and passed full `cargo test --features "native gui"` with `0` failed)
- `tools/difftest.sh`: `ALL EXAMPLES MATCH (tree == vm == exec)`
- `tools/selfhost_semantic_parity.sh`: `136 passed, 0 failed` (`ALL MATCH`)
- `tools/selfhost_compile_parity.sh`: `123 passed, 0 failed` (`ALL MATCH`)
- `node tools/verify_doc_claims.js`: `40/40` runtime claims, `0` failures
- `node tools/verify_dev_snippets.js`: `18/18` snippets, `0` errors

---

## 3. Measured HTTP Smoke Campaign (Sequential Identical Hardware)

Canonical route mix (`/`, `/health`, `/catalog`, `/cart`) run via `HLC/cli.py --profile smoke --no-runtime-comparison`:

| Runtime | Requests | Errors | Error Rate | Overall RPS (Sustained / Spike) | Latency (p50 / p95 / p99) | Memory (Idle / Peak / Post) |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: |
| **Spid3r AOT (`server.exe`, `3,327,488 B`)** | `11,488` | `0` | `0.0%` | **`334.9 RPS`** (`538.58` / `715.23`) | **`13.43 / 32.74 / 43.54 ms`** | `7.75 / 60.26 / 60.26 MiB` |
| **Python 3.15** | `10,224` | `0` | `0.0%` | `298.2 RPS` | `15.10 / 37.40 / 49.38 ms` | `22.00 / 25.28 / 22.32 MiB` |
| **Node 26** | `10,156` | `0` | `0.0%` | `296.2 RPS` | `15.15 / 37.69 / 49.90 ms` | `54.65 / 64.75 / 58.06 MiB` |

*(Archived local run IDs: Spid3r `run-20261122-062301`, Node `run-20261122-062336`, Python `run-20261122-062411`.)*

---

## 4. 24-Hour Docker Cgroup Soak & Release Tagging Status (`PENDING`)

- **Docker 24h Cgroup Soak (`p6-6`, `p7-2`):** **NOT YET RUN** — Docker Desktop daemon was not running on the workstation (`docker info` exited `1`), so Docker cgroup `S5` / `S6` / `S8` soak telemetry has not been captured and those gates remain **unchecked** in `plan.html`.
- **Reproducible Soak Command (once Docker Desktop daemon is started):**
  ```powershell
  $env:PATH = "C:\Users\needyamin\OneDrive\Desktop\spid3r\target\release;" + $env:PATH
  python C:\Users\needyamin\OneDrive\Desktop\HLC\cli.py --campaign C:\Users\needyamin\OneDrive\Desktop\HLC\benchmarks\spid3r-vs-all.json
  ```
- **v1.2 Release Tagging (`release.yml`, `p7-3`):** **PENDING OWNER APPROVAL** — `AGENTS.md` §7.4 prohibits automated commits, tags, or pushes without explicit owner instruction. Once reviewed, `.github/workflows/release.yml` builds Stage 0 -> Stage 1 -> Stage 2 -> Stage 3 fixed-point release artifacts on push of a `v*` tag or `workflow_dispatch`.
