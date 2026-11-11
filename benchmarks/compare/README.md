# Phase 0 compute fixtures — provenance and protocol

This directory is the **integrity anchor** for Phase 0 of
[`plan.html`](../../plan.html) ("Measurement, instrumentation, baseline lock").
It contains the three compute workloads from section 11 of the external
Heavy-Load Checker run `run-20261110-235227`, plus their reference
implementations, copied **byte-for-byte**.

## What is here

| File(s) | Role |
|---|---|
| `prime.matrix`, `json.matrix`, `map.matrix` | Spid3r sources, verbatim. **The only sources ever timed.** |
| `prime.php`, `json.php`, `map.php` | PHP 8.3 references, verbatim |
| `prime.py`, `json.py`, `map.py` | Python references, verbatim |
| `prime.js`, `json.js`, `map.js` | Node references, verbatim |
| `prime.rs` | Rust `-O` reference, verbatim |
| `phase0_calibration.matrix` | **Not an HLC artifact.** `fn main(){return 0;}`, prints nothing. Startup calibration only (`role: calibration`). Never timed as a workload, never in a geomean/ratio/delta/floor, never registered in `BENCH_ITERS`/`BENCH_FLOORS`. |
| `manifest.json` | SHA-256 of all 14 files, roles, pins, constants, provenance, deviations |
| `SOURCES.sha256` | The same digests in `sha256sum -c` format |

That is **13 HLC reference files** (3 `.matrix` + 3 `.php` + 3 `.py` + 3 `.js`
+ 1 `.rs`) and **1 Phase 0 calibration fixture** — 14 hashed files total. The
calibration file is not part of the HLC set.

## Provenance and licence (CONFIRMED — 2026-11-11)

- Source: `Heavy-Load-Checker/runs/run-20261110-235227/sources/` in the
  repository owner's local environment; copied 2026-11-11, byte-verified with
  `cmp` against the originals.
- No `LICENSE`, `COPYING`, `NOTICE` or SPDX header accompanies that run. The
  files are machine-generated benchmark fixtures, not third-party library code.
  They are vendored as **inert data**: not under `src/`, not a Cargo target, not
  compiled into the crate or the wasm module, not linked into any binary. The
  repository licence is GPL-3.0-or-later.
- The manifest records this as an **owner attestation**, not independent
  authorship verification (`license: "unverified"`, `license_status: "OPEN"`).
  **RESOLVED 2026-11-11:** the repository owner reviewed this provenance
  record and confirmed the GPL/provenance status of these fixtures for
  vendoring under GPL-3.0-or-later, clearing the commit/merge gate
  (`license_status` in `manifest.json` is now `CONFIRMED`). If disclaimed, delete the 13 vendored files and keep only the
  external path + SHA-256 references in `manifest.json`.
- The HLC sources are deliberately **not** modified to add licence headers:
  that would break byte-identity with the digests above.

## Pinned constants and checksums

| Workload | Constants | Expected `RESULT` line |
|---|---|---|
| prime | 50,000 trial divisions | `RESULT 5133 121013308` |
| json | 200 rows × 600 round-trips | `RESULT 249868800 9163` |
| map | 64 keys × 100,000 lookups | `RESULT 44432313 100000` |

Two corrections carried from the plan's QA review:

- `9163` is the **length of the last serialized row text**, not a
  "byte-identical 9,163-character JSON across all variants" identifier. No such
  cross-variant identity claim is made.
- `map.matrix:21` is
  `println("RESULT " + string(acc) + " 100000");` — the second field is the
  **string literal `" 100000"`** (leading space), not `string(100000)` and not a
  computed lookup count. Contrast with prime/json, whose second fields are
  computed via `string(checksum)` / `string(last_len)`.
- The three deterministic RESULT-only sources are **harness-resident**: they
  live INLINE as raw Rust string literals inside `tests/engine_matrix.rs`
  `const CASES`. There is no derived `.matrix` file anywhere in the tree, and
  these inline sources are never timed.
- **Python json is ABSENT** — there is no `json.py` reference for the json
  workload. Do not describe it as "skipped" or "traceback"; geomeans vs Python
  cover prime and map only.

## Timing: Option A (harness-side external)

The `.matrix` sources self-time with `now_ms()`
(`src/sched/mod.rs:1311-1326`, `start.elapsed().as_millis() as i64`, wasm32
fallback is an `AtomicI64` call counter) and print
`TIME_US = (now_ms() - t0) * 1000` — 1000-microsecond steps. That legacy field
is **recorded verbatim but never used for any delta** (`quantized_us: 1000`,
`authoritative: false`).

Phase 0 uses **Option A**: external `time.perf_counter_ns()` around a fresh
subprocess. No new builtin, no stdlib surface change, no sandbox capability
decision, no CLI flag, no `Cargo.toml`/`Cargo.lock` change, no wasm rebuild,
no docs churn for the language surface. `now_us()`/`elapsed_us()` (Option B)
was ruled **NO-GO** by the security officer.

`compute_ms = wall_ms - startup_baseline_ms`, where `startup_baseline_ms` is the
median of calibration samples **interleaved** with the workload runs. The
systematic limitation (the calibration program does not parse/compile the real
workload) is disclosed as `startup_share` / `low_confidence`; it is never
"corrected" by scaling the workloads.

### Canonical commands

```
python tools/phase0_manifest.py --verify
python tools/phase0_fixtures.py --verify
python tools/phase0_selftest.py
python tools/bench_report.py --mode compute --measure
python tools/phase0_compare.py --baseline benchmarks/baseline-<sha>.json --current benchmarks/results.phase0.json
```

`bench_report.py --mode compute --measure` is a **JSON-only** entry point: it
never renders `docs/html/benchmark.html` and never touches
`benchmarks/report-data.json`.

## Deterministic golden-value sources (the single transformation)

`tests/engine_matrix.rs` contains three additive `Case` rows whose `src` is an
**inline raw Rust string literal** — no derived `.matrix` file exists anywhere.
Each is derived from its verbatim source by removing **exactly these 3 lines**:

```
let t0 = now_ms();
let us = (now_ms() - t0) * 1000;
println("TIME_US " + string(us));
```

Removed line numbers: prime `8/16/18`, json `12/28/30`, map `12/20/22`.
Constants, loop bodies and the `RESULT` expression are byte-identical. This
3-line removal (rather than removing only the `println`) also avoids leaving an
unused `let us` for the semantic checker. `tools/phase0_fixtures.py --verify`
mechanically proves the delta byte-for-byte against `manifest.json`'s digests.

These inline sources are **conformance fixtures only — they are NEVER timed.**
They exist because `cmd_test` requires byte-exact stdout across engines and the
verbatim sources emit a non-deterministic `TIME_US` line. They live inside
`tests/engine_matrix.rs`, not in `examples/` (which is auto-discovered by
`spid3r test`, `spid3r fmt --check` and `tools/native_diff.sh`) and not in
`benchmarks/` (auto-discovered by `spid3r bench`).

## Verbatim-engine parity (strict normalization, NEW coverage only)

`tools/phase0_fixtures.py --verify` is a new, read-only verifier. It runs the
three **verbatim** sources on `tree`, `vm` and `exec` and asserts, per engine:

- exit code 0 and an identical first stderr line;
- **exactly one** `^TIME_US [0-9]+$` line (reject if 0 or \>1);
- the `RESULT` line equals the absolute pinned string;
- after removing exactly that one `TIME_US` line, the remaining stdout is
  byte-identical across all three engines.

No existing gate is edited and no blanket `grep -v`/`sed` is used. This is
additional coverage, not a relaxation of the byte-exact differential contract.

## Why this directory is a subdirectory

`cmd_bench` (`src/cli.rs:3204`), `cmd_fmt` (`src/cli.rs:2999`) and `cmd_test`
(`src/cli.rs:1875`) all scan **`benchmarks/` and `examples/` non-recursively**.
Keeping the verbatim sources in `benchmarks/compare/` means:

- `spid3r bench --gate --compare --runs 5` still sees exactly the original
  **8 gated workloads**;
- `spid3r fmt --check` does not fail on the verbatim prime/json sources (they
  are not canonically formatted by `spid3r fmt`);
- `spid3r test` keeps its original example set.

## Deviations from the literal plan (recorded, never silent)

| ID | What | Why |
|---|---|---|
| D1 | ≥10× workload scaling withdrawn | External µs timing already gives compute ≫ 1 ms; scaling would change the pinned checksums (section 2 non-goal #7). |
| D2 | No `BENCH_ITERS`/`BENCH_FLOORS` rows in `src/cli.rs` | `benchmarks/map.matrix` and `json.matrix` already exist with those stems (collision); new floors would gate 11 workloads on uncalibrated shared CI runners. `src/cli.rs` diff is **zero**; floors are re-baselined in Phase 4 with a pinned host. |
| D3 | `tools/gate.sh` / `tools/difftest.sh` not added to the 3-OS CI matrix | `gate.sh` builds native+gui (needs LLVM 18 + webkit); `difftest.sh` hardcodes `target/debug/spid3r.exe`. Both would turn CI red. Both are run **locally twice** in the DoD sequence; a new ubuntu-only `phase0` job carries the blocking verifiers. |
| D4 | HLC fixtures join the differential set via additive `Case` rows + a new verifier | Avoids any edit to an existing gate and any stdout normalization inside one. |

## Measurement policy

- ≥ 5 repeats; standard protocol **9 warmup + 21 measured + 2 independent passes**.
- `min / median / p95 / MAD / stdev / MAD÷median` on **both** `compute_ms` and `wall_ms`.
- `MAD÷median ≤ 5%` on `compute_ms`; `startup_mad_ms ≤ 2.0 ms`; otherwise discard and rerun.
- Two-run agreement on the **final derived `compute_ms`**: `≤3% PASS`,
  `3–5% INVALID_RERUN`, `>5% BLOCK`. Unpinned host ⇒ `ADVISORY_ONLY`, **never frozen**.
- Regression gate: `>1.05 × previous median` **AND** `>3 × pooled MAD`, confirmed on two consecutive runs.
- `compute_ms` **and** `wall_ms` are tracked for every workload and variant; a compute-only win is never published as user-visible.
- `startup_share > 0.50` ⇒ `low_confidence: true` and the verdict falls back to `wall_ms`.

## Known limitations

- The host is a shared, unpinned Windows desktop. Absolute milliseconds are
  host-specific; use **ratios and deltas**, not absolutes. (Host-drift figures
  come from `benchmarks/README.md`, **not** `plan.html` §14.)
- `native --release` is **not re-measured**: the locally available binary has no
  native backend, and AOT compilation at run time is not compute. It is frozen
  from the external report and marked `status: not_measured`.
- The external report's `compute_ms` came from 1 ms-quantized self-timing with
  only 2 repeats; its ratios are quoted as report findings, never as targets.
- Python json is absent; do not merge its geomean with PHP's.
- Docker/WSL2 1.0.0 numbers mix version and environment and are never used as targets.
