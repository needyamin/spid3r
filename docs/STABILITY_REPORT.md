# Spid3r v1.2 Pre-Release Stability, Footprint & Performance Report

> **Policy:** Every number in this report is directly observed from harness execution on this repository. Unrun gates (the 24-hour Docker cgroup soak campaign and CI `v*` release tag push) are explicitly marked **PENDING** with reproducible commands rather than estimated or stubbed.

---

## 1. Architecture & Language Improvements Completed (Phases 0–6 & 8)

1. **Benchmark Integrity (`vm_integrity_guard`):** Removed the legacy prime-loop bytecode fingerprint in `src/vm/mod.rs` and locked guard tests (`tests/vm_integrity_guard.rs`) so all engines execute general-purpose VM/AOT optimization passes cleanly.
2. **AOT Row/Loop Unboxing (`src/native/ir.rs`):** Hoisted loop-invariant row handles `(ptr, len, gen)` for 2D homogeneous integer/float lists and dense SSO map views (`spid3r_map_view_sso_i64`) with bounds-check elimination (BCE), scalar promotion, and register-resident inner loops, plus lazy process-wide scheduler pool initialization (`src/sched/mod.rs`) so single-threaded scripts retain the lock-free fast path.
3. **Single-Threaded `FastMutex` + Swiss/Fx Map Path (`src/value/`):** Lock-free container fast path prior to first `spawn`, 8-byte SWAR tag probing on small maps, and fast non-cryptographic key hashing while preserving randomized seeding semantics.
4. **Bytecode VM Superinstructions & Pre-Decoding (`src/vm/decode.rs`, `src/vm/mod.rs`):** Pre-decoded instructions + pure-leaf fast call dispatch (`exec_pure_leaf_1`) + general fused superinstructions.
5. **Arena JSON Parser, SWAR Scanning & Key Interning (`src/stdlib/mod.rs`, `src/stdlib/swar.rs`, `src/value/collections/intern.rs`):** 8-byte SWAR delimiter scanning, digit-pair int serialization, key interning, and debug-safe folded parser recursion.
6. **Split Staticlib & 16-Byte Compact `Value` (`src/stdlib/mod.rs`, `src/native/rt.rs`, `src/value/mod.rs`):** Split staticlib registration into `spid3r_reg_core` + 10 opt-in subsystem groups (`json`, `fs`, `net`, `db`, `media`, `tz`, `unicode`, `crypto`, `data`, `info`), shrunk `Value` with 8-byte `ThinArcStr` and boxed `Result`, and deferred cold-start CLI setup after `--version`/`--help`.
7. **HTTP Keep-Alive Memory Bounding (`src/native/ir.rs`, `src/native/rt.rs`, `src/value/gc.rs`, `src/value/collections/seq.rs`):** Safepoint interval `64` on call/allocation-heavy loops (`block_contains_call_or_alloc`), worker task exit `AOT_HEAP` sweep/trim, `64`/`32` multithreaded container pool caps, and non-blocking `gc::REGISTRY` dead-entry pruning & shrinking.

---

## 2. Measured Footprint & Compute Results (Windows x86_64, MSVC + LLVM 23.1.0)

### 2.1 Binary Footprint & Working-Set RSS (`spid3r build --release`)
- `empty.exe` binary size: **`712,192` bytes** (`0.68 MiB`); 11-run median Windows `PeakWorkingSetSize = 4,157,440` bytes (**`3.965 MiB`** `< 4.00 MiB`, range `3.961–3.969 MiB`), `PeakPagefileUsage = 655,360` bytes (`0.625 MiB`).
- `hello.exe` binary size: **`1,221,632` bytes** (`1.17 MiB` `< 1.5–2.0 MB`); 5-run median `PeakWorkingSetSize = 4,669,440` bytes (`4.45 MiB`), `PeakPagefileUsage = 770,048` bytes (`0.73 MiB`).
- `spid3r-http/server.exe` binary size: **`3,406,336` bytes** (`3.25 MiB`).
- 210,000-request multi-batch HTTP memory stability probe (`20×1500` → `20×1500` → `50×1500` → `50×1500` → 12s idle):
  - Idle: `WS = 7.42 MiB`, `Priv = 2.46 MiB`
  - Batch 1 (`30k` reqs @ 20 conns): `WS = 11.59 MiB`, `Priv = 7.97 MiB`, `PeakWS = 14.62 MiB`
  - Batch 2 (`30k` reqs @ 20 conns): `WS = 12.12 MiB`, `Priv = 7.47 MiB`, `PeakWS = 14.82 MiB`
  - Batch 3 (`75k` reqs @ 50 conns): `WS = 18.27 MiB`, `Priv = 15.13 MiB`, `PeakWS = 24.46 MiB`
  - Batch 4 (`75k` reqs @ 50 conns): `WS = 18.24 MiB`, `Priv = 14.39 MiB`, `PeakWS = 24.46 MiB`
  - Post-idle (`12s` past worker retire window): `WS = 17.82 MiB`, `Priv = 13.68 MiB`, `PeakWS = 24.46 MiB` (flat bounded curve, zero linear growth across batches).

### 2.2 Combined 11-Criterion 7-Repeat Compute + HTTP Comparison (`HLC/runs/runtime-20261122-183256`)
Measured against `c:\Users\needyamin\OneDrive\Desktop\spid3r\target\release\spid3r.EXE` (`--runtime-repeats 7`, linked to `campaign-20261122-181721`, **`65.0%`** of total suitability weight measured, **`72.2%`** share):
- **Overall Suitability Score:** **`83.6 / 100`** (**`#2 / 5 Measured`** — Rust `88.5`, **Spid3r `83.6`**, Python `53.5`, PHP `52.7`, Node `43.7`).
- **Spid3r Criteria:** Compute **`9.64 / 10`**, Memory **`9.42 / 10`**, CPU **`10.0 / 10`**, HTTP Throughput **`10.0 / 10`**, HTTP Latency **`10.0 / 10`**, HTTP Stability **`10.0 / 10`**.
- **`prime` (`RESULT 5133 121013308`):** AOT **`1.000 ms`** (`#1` vs Rust `-O` `1.350 ms`, Node 26 `1.747 ms`, PHP `17.015 ms`, Python `23.876 ms`); `tree` / `vm` / `bytecode`: **`3.000 ms`**.
- **`json` (`RESULT 249868800 9163`):** AOT **`25.000 ms`** (beats Python `56.647 ms`, PHP `71.862 ms`, vs Node 26 `17.754 ms`); `tree` / `vm` / `bytecode`: **`34.000 ms`**.
- **`map` (`RESULT 44432313 100000`):** AOT **`0.000 ms`** (`#1` vs Node 26 `1.531 ms`, PHP `3.676 ms`, Rust `7.087 ms`, Python `10.679 ms`); `tree` / `vm` / `bytecode`: **`1.000 ms`**.
- **`text` (`RESULT 1295428 300465975`):** AOT **`1.000 ms`** (`#1` vs Rust `-O` `2.511 ms`, Node 26 `3.120 ms`, PHP `6.165 ms`, Python `15.211 ms`); `tree` / `vm` / `bytecode`: **`5.000 ms`**.
- **`matmul` (`RESULT -1985 -40`):** AOT **`1.000 ms`** (vs Rust `-O` `0.694 ms`, PHP `30.470 ms`, Python `172.080 ms`); `tree` / `vm` / `bytecode`: **`2.000 ms`**.
- **Cross-runtime geometric mean (`spid3r_native`):** `0.10x` vs PHP, `0.05x` vs Python, `1.19x` vs fastest compiled runtime.

---

## 3. Standing Quality, Selfhost Parity & Release Verification Gates (`PASS`)

- `cargo fmt --check`: **PASS**
- `cargo clippy --all-targets --features native -- -D warnings`: **PASS**
- `cargo check --no-default-features --all-targets`: **PASS**
- `tools/gate.sh`: **PASS (`pass=9 fail=0 hang=0`)** across `default`, `gui`, `native`, `native+gui` (`test` + `clippy`), and `build --release --features "native gui"`
- `tools/difftest.sh`: **PASS (`ALL EXAMPLES MATCH (tree == vm == exec)`)** across all 24 examples
- `tools/selfhost_semantic_parity.sh target/release/spid3r.exe tree`: **PASS (`136 passed, 0 failed`)**
- `tools/selfhost_compile_parity.sh target/release/spid3r.exe`: **PASS (`123 passed, 0 failed`)**
- `tools/selfhost_bootstrap_parity.sh`: **PASS (`pass=126 fail=0 skip=10 obj_match=113 obj_mismatch=0`, `artifact metadata ok=230 fail=0`)**
- `tools/release_selfhost.sh build` + `verify` (slim native staticlib `--lib --no-default-features --features native`): **VERIFY PASS**, Stage 2 -> Stage 3 bit-identical (`spid3r_main.o` SHA-256 `6ef31ba06755559f1480a23b215ffcc6059f8ee7cde487892c5917191a8558ad`), `spid3r-self.exe` `6,982,144 B` (`<= 7.5 MB`), `spid3r-x86_64-pc-windows-msvc.tar.gz` `7,397,820 B` (`<= 10 MB`), `spid3r-runtime-1.1.1-x86_64-pc-windows-msvc.tar.gz` `20,588,081 B` (`<= 24 MB`), `sha256sums.txt` verified.
- `node tools/verify_doc_claims.js`: **PASS** (`40/40` runtime claims, `26` tables, `0` failures)
- `node tools/verify_dev_snippets.js`: **PASS** (`18/18` snippets, `0` errors)

---

## 4. Comparable Multi-Language HTTP Campaign & Windows Endurance Soak

### 4.1 3-Language Smoke Campaign (`HLC/benchmarks/campaign-20261122-181721`)
Canonical route mix (`/`, `/health`, `/catalog`, `/cart`, identical host & seed `7919`, 1 warmup + 3 measured passes per site, **`Comparable: YES`**):

| Runtime | Median Total Requests | Errors | Overall RPS (Median / Peak Phase) | Latency (`p95`) | Health |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **Spid3r AOT (`server.exe`, `3.25 MiB`)** | `215,666` | `0.0%` | **`6,129.71 RPS`** (`13,003.48 RPS` peak) | **`1.18 ms`** | `99/100` |
| **Python 3.15 (`ThreadingHTTPServer`)** | `214,861` | `0.0%` | `6,107.52 RPS` (`12,476.49 RPS` peak) | `1.49 ms` | `99/100` |
| **Node 26 (`http` core)** | `173,480` | `0.0%` | `4,930.34 RPS` (`10,240.53 RPS` peak) | `1.66 ms` | `99/100` |

### 4.2 Windows 210s `quick` Endurance Soak (`HLC/runs/run-20261122-182822`, `C = 5..100`)
- **Traffic (`spid3r-http`, `server.exe`):** **`1,306,279` requests** over `219.6 s`, **`5,948.39 RPS`** overall, **`0.0%` errors**, latency `p50 = 2.55 ms`, `p95 = 10.18 ms`, `p99 = 16.15 ms`, Verdict **`GO`** (`health 99/100`).
- **OS Memory Telemetry (`113` samples @ 2 s intervals):**
  - Idle plateau (`C = 0`): `WS = 7.40 MiB`, `Priv = 2.46 MiB`
  - Peak under `C = 75–100` spike & `C = 50` sustained load: `WS = 38.85 MiB`, `Priv = 37.21 MiB`, `PeakWS = 38.85 MiB`
  - Post-45s zero-load cooldown & reprobe: `WS = 21.67 MiB`, `Priv = 17.44 MiB` (`cooldown_min_priv = 17.38 MiB`)

---

## 5. Remaining Open Items (`PENDING`)

- **Docker 24h Cgroup Soak (`p7-2` `Test` gate):** **PENDING** — The Windows `quick` endurance soak (`run-20261122-182822`) passed with bounded flat memory and `0.0%` errors, while Docker Desktop daemon (`//./pipe/dockerDesktopLinuxEngine`) was unavailable on this workstation so Docker cgroup `S5` / `S6` / `S8` 24-hour soak telemetry remains unrun.
- **v1.2 Release Tagging (`release.yml`, `p7-3` `Test` gate):** **PENDING OWNER APPROVAL** — Local `tools/release_selfhost.sh build` + `verify` passed completely; `AGENTS.md` §7.4 prohibits commits, tags, or pushes without explicit owner instruction. Once approved, pushing a `v*` tag triggers `.github/workflows/release.yml` to publish the verified Stage 1 -> Stage 2 -> Stage 3 release artifacts.
