# Spid3r Benchmarks

Reproducible micro-benchmarks for the Spid3r language: eight gated workloads
run on both execution engines (bytecode VM and tree-walk interpreter), plus
optional cross-language comparisons against Python and PHP reference
implementations.

## Directory layout

```
benchmarks/
├── *.matrix                 Spid3r benchmark sources (one file per workload)
│   ├── arith.matrix         2M-iteration integer accumulation loop
│   ├── strings.matrix       30K string concatenations and slices
│   ├── list.matrix          200K pushes then pops, summing values
│   ├── map.matrix           40K hash inserts and lookups
│   ├── methods.matrix       200K method calls on objects
│   ├── closures.matrix      40K closure allocations and calls
│   ├── json.matrix          2K encode/decode round-trips
│   ├── tasks.matrix         2K spawn/join task round-trips
│   └── stress_suite.matrix  Mixed workload, correctness only (ungated)
├── python/*.py              Python reference implementations (same workloads)
├── php/*.php                PHP reference implementations (same workloads)
├── compare/                 Phase 0 verbatim HLC compute workloads (subdirectory)
│   ├── prime/json/map.matrix     Verbatim timed sources (pinned checksums)
│   ├── *.php / *.py / *.js / .rs Verbatim reference implementations
│   ├── phase0_calibration.matrix Startup calibration only (role: calibration)
│   ├── manifest.json             SHA-256 of all 14 files + roles + provenance
│   ├── SOURCES.sha256            Same digests in sha256sum -c format
│   └── README.md                 Protocol, provenance and deviations
├── results.phase0.json      Phase 0 external compute snapshot (frozen, pinned host)
├── baseline-<sha>.json      Phase 0 frozen medians for commit-over-commit comparison
├── report-data.json         Latest measured data (written by the report tool)
└── results.json             Historical comparison set (kept for reference)
```

`compare/` is a **subdirectory on purpose**: `spid3r bench`, `spid3r fmt --check`
and `spid3r test` all scan `benchmarks/` and `examples/` **non-recursively**.
Keeping the verbatim HLC sources there means `bench --gate --compare --runs 5`
still sees exactly the original 8 gated workloads, and the formatter does not
reject the verbatim prime/json sources. The two `map.matrix` / `json.matrix`
files at the top level are the original in-tree workloads, **not** the HLC ones.

Every `.matrix` workload prints deterministic output. The harness uses this so
each timing run doubles as a correctness check.

## Prerequisites (on any machine)

1. **Rust** — to build the interpreter. Check `rust-toolchain.toml` in the repo
   root for the exact toolchain version, then run:

   ```
   cargo build --release
   ```

   The benchmark harness expects the binary at `target/release/spid3r(.exe)`.

2. **Python 3** — required to run the full report (`tools/bench_report.py`).
   Any modern Python 3 works; no third-party packages are needed.

3. **PHP CLI** *(optional)* — only needed if you want the PHP columns in the
   report. If `php` is not on your `PATH`, the tool simply skips PHP and the
   report shows Spid3r vs Python only.

## Quick run: just the numbers

From the repo root:

```
spid3r bench                      # bytecode VM (the default engine), best of N
spid3r bench --engine tree        # tree-walk interpreter
spid3r bench --engine vm --runs 5
spid3r bench --compare            # require tree and vm to print identical output
spid3r bench --gate               # exit non-zero if a perf floor regresses
spid3r bench --gate --compare --runs 5   # local perf-gate check (not a CI step)
```

Output is a table per workload: `iters`, best wall-clock time, and iters/sec.

## Full report (the docs/html/benchmark.html page)

`tools/bench_report.py` measures everything and renders the report page. It
spawns a fresh process per run and takes the best of N (5 for Spid3r, 3 for
tree/Python/PHP). Run it from the repo root:

```
# measure + render in one go
python tools/bench_report.py

# or in two phases
python tools/bench_report.py --measure   # writes benchmarks/report-data.json
python tools/bench_report.py --render    # writes docs/html/benchmark.html

# more samples for the non-Spid3r runtimes
python tools/bench_report.py --runs 5 --other-runs 5
```

`--render` alone re-renders the page from the existing
`benchmarks/report-data.json` without re-measuring — useful for tweaking the
page layout.

## Performance gates

Each gated workload has a floor in `src/cli.rs` (`BENCH_FLOORS`), calibrated at
roughly one third of the worst rate observed on the reference machine. The
floor exists to catch catastrophic regressions (a 3x+ slowdown) on shared CI
runners — passing the gate is a floor, not a target. A new benchmark added to
`BENCH_ITERS` without a floor is simply not gated.

## Comparing results across machines

Absolute numbers are **host-specific**. CPU power plans, boost behavior, and
background load can shift results 25–30%. When comparing machines:

- **Compare ratios, not milliseconds** — e.g. VM-vs-tree speedup per workload,
  or Spid3r-vs-Python ratio.
- Use the same `--runs`/`--other-runs` on both machines.
- Prefer a consistent power profile (e.g. "Balanced" on Windows) between runs.
- The startup-adjusted section of the report subtracts each runtime's measured
  no-op startup time; treat differences under ~1 ms as noise.

## Phase 0 external compute protocol

Phase 0 of `plan.html` adds a **separate, JSON-only** measurement path for the
three Heavy-Load Checker compute workloads. It is deliberately *not* wired into
`spid3r bench`: the HLC `.matrix` sources print a non-deterministic `TIME_US`
line, which the bench harness's byte-exact stdout contract would reject. See
[`compare/README.md`](compare/README.md) for the full protocol, provenance,
SHA-256 digests and the four recorded deviations from the literal plan.

```
python tools/phase0_manifest.py --verify        # fail-closed digest check
python tools/phase0_fixtures.py --verify        # pinned RESULT on tree/vm/exec
python tools/phase0_selftest.py                 # 5% sensitivity self-test (blocking)
python tools/bench_report.py --mode compute --measure
python tools/phase0_compare.py \
    --baseline benchmarks/baseline-<sha>.json \
    --current benchmarks/results.phase0.json benchmarks/results.phase0.json
```

Key points:

- **Option A** — external `time.perf_counter_ns()` around a fresh subprocess.
  No new builtin, no stdlib/CLI/Cargo/wasm change. The self-reported `TIME_US`
  is 1 ms-quantized (`quantized_us: 1000`, `authoritative: false`) and is never
  used for a delta.
- **Checksum-first** — a run is rejected unless it exits 0, prints exactly one
  pinned `RESULT` line and one `TIME_US` line. Pins: `5133 121013308`,
  `249868800 9163`, `44432313 100000`.
- **Statistics** — 9 warmup + 21 measured + 2 independent passes;
  `min / median / p95 / MAD / stdev / MAD÷median` on both `compute_ms` and
  `wall_ms`; `MAD÷median ≤ 5%`; `startup_mad_ms ≤ 2 ms`; two-run agreement
  `≤3% PASS / 3–5% INVALID_RERUN / >5% BLOCK`.
- **Commit-over-commit** — `tools/phase0_compare.py` enforces the ±5% /
  `3×` pooled-MAD regression rule, because `bench --gate` floors only catch 3×+
  regressions and `--compare` only checks tree == vm output.
- **Unpinned shared host ⇒ `ADVISORY_ONLY`, never frozen.** Only a pinned host
  with two agreeing runs may freeze `results.phase0.json` /
  `baseline-<sha>.json`. CI writes a distinct
  `results.phase0.ci-advisory.json` and can never overwrite the frozen snapshot.
- The 8 gated in-tree workloads, `BENCH_FLOORS` and `src/cli.rs` are
  **unchanged** — Phase 0 has a zero `src/` diff.

## Phase 1 (cheap wins) measurement

Phase 1 reuses the Phase 0 protocol unchanged and adds one rule: results are
compared **commit-over-commit** with `tools/phase0_compare.py` against the
frozen `baseline-<sha>.json`. On an unpinned shared host every result is
`ADVISORY_ONLY` and must never overwrite the frozen baseline, so pass
`--advisory` together with an explicit `--out` inside the workspace:

```
python tools/phase0_bench.py --measure --advisory --out benchmarks/results.phase1.json
python tools/phase0_compare.py \
    --baseline benchmarks/baseline-<sha>.json \
    --current benchmarks/results.phase1.json benchmarks/results.phase1-run2.json \
    --out benchmarks/phase1-compare.json
```

Phase 1 changes are engine/stdlib internals only: no new builtin, CLI flag,
opcode or `.matrixc` format change. The public-facing prose in `README.md`,
`docs/html/doc.html`, `docs/STANDARD_LIBRARY.md` and `docs/BYTECODE.md`
therefore stays valid, but `tools/verify_doc_claims.js` still gates it, and
`web/spid3r.wasm` + `web/spid3r_wasm_data.js` are rebuilt after any core
change.

**Sensitivity caveat.** The blocking self-test injects 5% of the *current*
compute median of `benchmarks/compare/map.matrix`. Phase 1 made that workload
faster, so the injected delta shrank from ~1.6 ms to ~1.15 ms — close to the
~1 ms scheduling noise on an unpinned shared host. **No threshold is
loosened**: noisy attempts are invalidated and rerun (up to 3), and the whole
measurement is repeated until the gate passes. On a dedicated pinned host the
gate is stable. A future phase that makes the workload faster still should size
the injection from the plan's fixed 30 ms reference instead of the moving
compute median.

## Phase 2 (value / dispatch slice) measurement

Phase 2 reuses the same Phase 0 protocol (`--measure --advisory`, 9 warmups /
21 samples / 2 independent passes, external `perf_counter_ns`, checksum-first,
`ADVISORY_ONLY`, no frozen baseline). Pre-registered before measurement, four
stretch targets are **declared unmet** for this slice: `map ≤ 7.6 ms`,
`json ≤ 142 ms`, compute geomean `≤ 2.0x` vs PHP 8.3, and tree-walk
`≤ 1.5x VM` on all three workloads. The tracked gates are three-engine
byte-identity, `--no-opt ≥ 5% slower` than the default VM, `prime ≤ 34 ms`
(borderline) and wall geomean `≤ 1.5x` (already 1.499x in Phase 1 = hold).

**Two Phase 2-specific rules.** (1) The only speedup is a **compile-time
optimizer pass** (`src/vm/opt.rs` loop back-edge rotation) that runs on the
default path only, so the `vm` and `no-opt` variants are now genuinely
different code, not just different cache behaviour. (2) The bytecode cache key
gained a compiler epoch in Phase 2 precisely so a pass change cannot silently
serve pre-upgrade bytecode — but a configured `cache.bytecode_dir` may still
contain old-key files. For CI-equivalent numbers, point `SPID3R_INI` at a temp
ini with a **fresh** `cache.bytecode_dir`, then also verify one warm cache-hit
run so both paths are exercised. `--no-opt` bypasses the cache entirely.

Scope note: no new builtin, CLI flag, `OpCode`, `Chunk` field, dependency or
`.matrixc` format change; `src/cli.rs` does change (cache epoch), unlike Phase
1's zero `src/` CLI diff. The map inline cache, tree-walk environment cache
and any `Value` layout change were deliberately **cut** from this slice and
are carried forward as backlog. Because `--prof` on the default path now
executes one fewer `Jump` per loop iteration, `tests/engine_matrix.rs` pins
the exact instruction-count delta (1064 for its 64 + 1000 loop iterations)
while `--prof --no-opt` stays at the Phase 1 count of 18050.

## What is NOT measured here

- The AOT/native backend (needs LLVM, runs in its own CI job). Phase 0 marks
  it `not_measured` and freezes it from the external report instead.
- The WebAssembly build (see `docs/html/playground.html`). The Phase 0 harness
  is native-only: on wasm32 `now_ms()` is an `AtomicI64` call counter and
  `TIME_US` carries no elapsed-time information.
- Startup-to-first-byte for real applications, memory ceilings, or I/O — these
  are micro-benchmarks, not application benchmarks.
