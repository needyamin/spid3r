# FIX F4 — pathological deep-nesting stall (GC interaction)

## Status: IN PROGRESS

## Hypothesis (from audit)
Statement-boundary auto-GC does a full-graph rescan per collection window => O(N^2)
total on a linear chain of N live cells. Both engines stall identically during the
build loop itself.

## Suspected cost centers (to verify by measurement + code read)
1. `maybe_collect()` trigger (gc.rs, bottom): fires once `ALLOCS >= 1024` since last
   collect — fixed threshold => N/1024 collections, each O(live cells) => O(N^2).
2. `track()` safety valve (gc.rs): when registry > 8192 entries it prunes on EVERY
   allocation — O(registry len) per alloc => O(N^2) even with SPID3R_GC=0.
   (If this shows up, GC=0 will NOT be fast at large depths — measure will tell.)

## Measurement matrix (tree engine, 60 s kill, wall-clock seconds)

| depth | GC unset (before) | GC=0 (before) | GC unset (after) | GC=0 (after) |
|------:|------------------:|--------------:|-----------------:|-------------:|
| 100k  | pending | pending | pending | pending |
| 400k  | pending | pending | pending | pending |
| 1M    | pending | pending | pending | pending |
| 4M    | pending | pending | pending | pending |

## Code findings
- `src/interpreter/mod.rs:322` — sole `maybe_collect()` call site (statement
  boundary, tree engine only). The VM engine (src/vm/**) has NO gc hook at all
  (grep: zero matches) — yet the audit measured the VM stalling identically,
  so the primary cost center must be engine-independent shared code.
- `src/value/gc.rs::maybe_collect()` — fires when `ALLOCS >= 1024` (fixed
  threshold since last collect) => N/1024 full collections while building N
  cells, each O(live) => O(N^2/1024). Tree engine only.
- `src/value/gc.rs::track()` — once registry > 8192 entries it runs a full
  `retain` scan (Weak::strong_count + HashSet insert per entry) on EVERY
  allocation. In the chain build every cell stays live, so nothing is ever
  pruned and the scan repeats per alloc => O(N^2) on BOTH engines, even with
  SPID3R_GC=0. Predicted dominant cost; measurement to confirm.
- one `collect()` on N live cells: snapshot+retain O(N), strong_count each,
  per-cell trace (mutex lock) + HashMap index + Vec<Vec> edges, mark, sweep,
  final prune — all O(N) with heavy constants.
- tracked-cell entry point: `value::new_ref` -> `gc::track` (every container
  construction in both engines; 1 registry entry per list).

## Fix design (both cost centers amortized, inside gc.rs only)
1. `NEXT_GC: AtomicUsize` — auto-collect threshold, starts at 1024. At the end
   of every `collect()` set it to `max(1024, snapshot_len - freed)` (surviving
   cells). Growing heap => threshold doubles => O(log N) collections for a
   linear build, O(N) total re-scan work. Churn (everything garbage) keeps
   threshold at the 1024 baseline, matching current behavior.
2. `PRUNE_AT: AtomicUsize` — registry prune watermark, starts 8192. `track`
   prunes only when `reg.len() > PRUNE_AT`, then raises the watermark to
   `max(8192, 2 * post-prune len)` => amortized O(1) per allocation, registry
   still bounded (~2x live + 8192 dead slack).
- interpreter/mod.rs trigger line needs NO change (threshold logic lives in
  gc.rs); keep that file untouched => smaller diff.
- Contract preserved: `gc_collect()`/`gc_live_cells()` builtins unchanged,
  SPID3R_GC=0 still disables auto-collect, statement-boundary auto-collection
  still happens (amortized), collection correctness (mark/sweep) untouched.

## Measurement matrix (tree engine, 60 s kill, wall-clock seconds)
Script: audit_notes\probes\run_fx4_gc.ps1 (probe files fx4_build_*.matrix)

| depth | GC unset (before) | GC=0 (before) | GC unset (after) | GC=0 (after) |
|------:|------------------:|--------------:|-----------------:|-------------:|
| 10k   | pending | pending | pending | pending |
| 100k  | pending | pending | pending | pending |
| 400k  | pending | pending | pending | pending |
| 1M    | pending | pending | pending | pending |
| 4M    | pending | pending | pending | pending |

## FINAL STATUS (orchestrator integration, 2026-10-19)
- Before-fix measurement (background log): 10k 5.1s; 100k TIMEOUT >60s with GC on AND GC=0 (the track() prune is GC-independent — two cost centers); 400k crashed at ~51s; 1M/4M TIMEOUT.
- Fix (gc.rs, as designed): `NEXT_GC` exponential backoff after each collect + `PRUNE_AT` watermark for the track() safety valve. Contract preserved (gc_collect/gc_live_cells, SPID3R_GC=0, statement-boundary auto-collect).
- SECOND defect uncovered once the stall was gone: teardown of a deep singly-owned chain overflowed the host stack (~10^5 levels) — pre-existing, previously masked by the stall. Fixed with iterative `Drop` for VecData / MapData / SetData (seq.rs): sole-owned children are drained into a worklist; shared cells fall back to normal drop (depth bounded by sharing).
- After (debug build, tree, GC on): 100k 0.6s · 400k 2.4s · 1M 6.5s · 4M 27.3s — all exit 0, "built cells=N" printed, no overflow. VM engine 400k exit 0. GC=0 100k exit 0.
- Tests: tests/fix_f4_gc.rs 1/1 green (400k <10s per engine, GC on). Known residual: deep chains through SHARED cells (e.g. shallow-cloned heads) still recurse; container types other than list/vec/map/set keep recursive drops (documented in plan.html).
