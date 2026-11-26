# Spid3r Benchmarks

Reproducible, checksum-verified benchmark suite organized strictly by language (`spid3r/`, `python/`, `php/`, `nodejs/`, `rust/`) with all JSON datasets, frozen baselines, budgets, and manifests consolidated in [`benchmarks/json/`](json/).

## Directory Layout

```text
benchmarks/
├── spid3r/                      # Spid3r (.matrix) workloads
│   ├── arith.matrix             # 2M-iteration integer loop (gated)
│   ├── closures.matrix          # 40K closure allocations & calls (gated)
│   ├── json.matrix              # 2K JSON write/parse round-trips (gated)
│   ├── list.matrix              # 200K list push/pop churn (gated)
│   ├── map.matrix               # 40K map insert/lookup churn (gated)
│   ├── methods.matrix           # 200K object method calls (gated)
│   ├── strings.matrix           # 30K string concatenations (gated)
│   ├── tasks.matrix             # 2K spawn/channel round-trips (gated)
│   ├── set.matrix               # Set add/contains churn
│   ├── equality.matrix          # Scalar equality churn
│   ├── stress_suite.matrix      # Mixed correctness suite (ungated)
│   └── compute/                 # Checksum-pinned Phase 0 compute workloads + calibration
├── python/                      # Python 3 reference suite + compute/
├── php/                         # PHP 8.3 reference suite + compute/
├── nodejs/                      # Node.js reference suite + compute/
├── rust/                        # Rust (-O) compute reference (compute/prime.rs)
├── selfhost/                    # Frozen compiler-throughput corpus inputs & MANIFEST.sha256
└── json/                        # All JSON measurements, baselines, budgets & manifests
    ├── micro-suite.json         # Latest multi-runtime micro-suite data
    ├── compute-baseline.json    # Frozen Phase 11 P-1 3-way compute baseline
    ├── compute-latest.json      # Freshly measured Phase 0 compute snapshot
    ├── selfhost-baseline.json   # Frozen Stage-0 vs Stage-1 compiler throughput baseline
    ├── footprint-budgets.json   # Startup, RSS, commit & binary size budgets
    ├── compute-manifest.json    # SHA-256 workload provenance & integrity manifest
    └── SOURCES.sha256           # sha256sum ledger for compute-manifest.json
```

## Quick Commands

```bash
# 1. Measure all runtimes (Spid3r AOT/VM/Tree, Node.js, PHP, Python) and render docs/html/benchmark.html
python tools/bench/bench_report.py

# 2. Re-render docs/html/benchmark.html from existing benchmarks/json/*.json without re-measuring
python tools/bench/bench_report.py --render

# 3. Verify report figures & table sync against frozen JSON baselines
python tools/bench/bench_report.py --verify-sync

# 4. In-tree differential + perf gate (tree == vm stdout)
spid3r bench --gate --compare --runs 5

# 5. Phase 0 integrity, fixture conformance & sensitivity gates
python tools/bench/phase0_manifest.py --verify
python tools/bench/phase0_fixtures.py --verify
python tools/bench/phase0_selftest.py
python tools/bench/phase0_bench.py --verify-selfhost
python tools/bench/phase0_bench.py --measure --advisory --quick --out benchmarks/json/compute-latest.json
```

## Measurement Discipline

- **Fresh-process timing:** Every timing sample spawns a fresh OS process and measures startup overhead separately via a no-op invocation, computing both raw wall-clock and startup-adjusted compute time.
- **Deterministic verification:** Every workload prints a deterministic output or pinned `RESULT` checksum checked on every run.
- **Zero-dependency parity:** `spid3r bench --compare` enforces byte-identical stdout across `spvm` bytecode VM and the `tree` oracle.
