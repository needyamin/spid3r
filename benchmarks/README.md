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
├── report-data.json         Latest measured data (written by the report tool)
└── results.json             Historical comparison set (kept for reference)
```

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
spid3r bench                      # tree engine, best of N
spid3r bench --engine vm          # bytecode VM
spid3r bench --engine vm --runs 5
spid3r bench --compare            # require tree and vm to print identical output
spid3r bench --gate               # exit non-zero if a perf floor regresses
spid3r bench --gate --compare --runs 5   # what CI runs
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

## What is NOT measured here

- The AOT/native backend (needs LLVM, runs in its own CI job).
- The WebAssembly build (see `docs/html/playground.html`).
- Startup-to-first-byte for real applications, memory ceilings, or I/O — these
  are micro-benchmarks, not application benchmarks.
