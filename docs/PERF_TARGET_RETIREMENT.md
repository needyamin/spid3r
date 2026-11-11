# Performance target retirement (Phase 5, 2026-11-12)

**Status:** owner-approved. **Scope:** this document records the disposition
of the original 2026-11-11 numeric performance targets after the v1.1.0 ship
(`69f9dfa`). It is a disclosure and decision record, **not** a change to the
language, the CLI, the standard library, the engines or any gate.

## 1. What this document does and does not do

- **Does:** quote the original `plan.html` §7 / §8 targets verbatim, state
  which were met, and formally retire those that cannot be reached without
  violating a hard constraint.
- **Does:** record the owner-approved rationale for each retirement.
- **Does not:** re-base, relax, hide or delete any original number. The
  original table in `plan.html` §7 is preserved verbatim; the as-shipped
  scorecard is a separate §15.
- **Does not:** lift a RED LINE. `plan.html` §9 forbids a new `unsafe` in the
  value / GC / interpreter / VM core, GC-root uncertainty, new core
  dependencies, and an unwasm-gated backend. The project invariants
  (zero-dependency core, zero feature removal, three engines + wasm, unchanged
  exit-code / error-phase / checksum / sandbox semantics) all remain in force.
- **Does not:** remove a feature, a builtin, a check, an engine or a platform.

## 2. Original targets, quoted verbatim from `plan.html` §7

| Phase | prime VM | json VM | map VM | geomean compute | wall geomean | Structural gates | Label |
| --- | --- | --- | --- | --- | --- | --- | --- |
| P0 freeze | ≤ 117 | ≤ 221 | ≤ 33 | ≤ 5.705x | ≤ 2.27x | ±5% no regression; tree ≤ 244/289/82; native ≤ 123/231/51; bytecode exec ≤ 121/229/35 | committed |
| P1 cheap wins + optimizer proof | ≤ 94 (-20%) | ≤ 199 (-10%) | ≤ 26 (-20%) | ≤ 4.7x | track | default VM ≥ 3% faster than `--no-opt` on ≥ 1 gated workload; native diagnosis delivered (no numeric gate) | committed |
| P2 halve the gap | ≤ 34 | ≤ 142 | ≤ 7.6 | ≤ 2.0x | ≤ 1.5x | `--no-opt` ≥ 5% slower; tree-walk ≤ 1.5x VM on all three | **stretch** |
| P3 parity / research track | ≤ 17.2 | ≤ 71.1 | ≤ 3.755 | ≤ 1.0x | track | `native --release` ≥ 1.3x faster than VM (≤ 0.77x); tree-walk ≤ 1.5x VM | **aspirational** |

Additional structural targets from §7: "native --release must become ≥ 1.3x
faster than the VM on prime and map by P3 (today: 0.96x / 0.65x)";
"Tree-walk ≤ 1.5x VM by P2 (today 2.08x / 1.31x / 2.48x)"; "Wall geomean vs
PHP tracks alongside compute in every phase gate".

The plan itself already labelled P3 "a north-star research track, **not** a
committed gate; Go / No-Go is tied to P0-P2 only".

## 3. As-shipped v1.1.0 results (`ADVISORY_ONLY`)

External harness-side `time.perf_counter_ns()` timing, 9 warmups / 21 samples /
2 passes, MAD + p95, ±3% two-run agreement. **All absolute milliseconds are
advisory on this shared, unpinned Windows host; only ratios and deltas are
portable.**

| Measurement | prime | json | map |
| --- | --- | --- | --- |
| P0 baseline (VM) | 116.76 ms | 225.47 ms | 32.68 ms |
| P1 final (VM) | 34.25 ms | 199.01 ms | 23.89 ms |
| P2 clean run2 (VM) | 30.54 ms | 194.95 ms | 23.15 ms |
| P3 run2 (VM) | 30.54 ms | 191.25 ms | 22.16 ms |
| P3 run2 (tree) | 249.94 ms | 259.79 ms | 74.61 ms |
| P3 run2 (`exec`) | 30.75 ms | 190.01 ms | 22.22 ms |
| P3 run2 (native) | 9.03 ms | 213.19 ms | 48.15 ms |
| Focused native-prime rerun | **9.404 ms** | — | — |

Shipped gains: P1 prime −70.7%, map −26.9%, json −11.7%; P2 `rotate_loops`
prime +8.4% vs `--no-opt`, wall geomean 1.417x; P3 native prime 9.404 ms vs
VM 30.624 ms = **3.26x faster** (15.4x from the 139.09 ms historical native
baseline). P4 certificate: 2 consecutive green gate matrices, 1314 tests,
7 approved written deviations, HLC licence `CONFIRMED`, zero feature removal.
Phase 5 adds `tests/phase5_perf_parity.rs` (4 differential tests), so the
inventory is now **1318** (1295 core / 13 gui / 10 aot); the 1314 figure above
is the historical as-shipped v1.1.0 count.

## 4. Formal retirements

| # | Original target | Disposition | Owner-approved rationale |
| --- | --- | --- | --- |
| R1 | P2 stretch: json ≤ 142 ms, map ≤ 7.6 ms, geomean ≤ 2.0x, wall ≤ 1.5x, tree-walk ≤ 1.5x VM | **RETIRED — research track** | The remaining json / map gap is collection-dominated, not dispatch-dominated. `Value::Map(Ref<MapData>)` is `Arc<Mutex<MapData>>` (`src/value/collections/seq.rs`) and AOT `Index` still routes through `spid3r_index_get` with a per-lookup heap `leak()` (`src/native/rt.rs`). Closing the gap needs a tagged / inline `Value` rewrite, typed slots, a JIT or a register-SSA lowering — all forbidden by §9 RED LINES and by the zero-dependency / zero-feature-removal invariants. Params entry-unboxing was ruled out: the VM errors at the use site, and `analyze_int_locals` excludes params by design. |
| R2 | P3 aspirational PHP parity: prime ≤ 17.2, json ≤ 71.1, map ≤ 3.755 ms, geomean ≤ 1.0x | **RETIRED — was never a gate** | §7 already labelled P3 a north-star research track. Matching PHP 8.3 (a mature C VM) on every workload from a hand-written zero-dependency bytecode VM with no JIT requires exactly the changes R1 forbids. §7's own "Required speedups" box calls this "a north-star research track, not a committed gate". |
| R3 | `native --release` ≥ 1.3x faster than VM on prime **and** map | **PARTIALLY MET / map RETIRED** | Met decisively on prime (3.26x). Not met on map (native 48.15 ms vs VM 22.16 ms). The P3 arithmetic inlining does not cover the collection hot loop; a per-lookup heap leak dominates. Retiring the map half is a scope decision, not a claim of success. |
| R4 | Tree-walk ≤ 1.5x VM; wall geomean ≤ 1.5x | **RETIRED as a blanket gate** | Tree-walk is a supported engine that must not be sacrificed; it improved but remains 249.94 / 259.79 / 74.61 ms. Wall geomean reached 1.417x (P2) but is host-noise sensitive and `ADVISORY_ONLY`. No engine is to be removed, deprioritised or made a "sacrificial" path to satisfy a ratio. |

**Structural verdict:** json / map AOT parity with the VM, PHP / Node /
`rustc -O` parity, and anything requiring a `Value` / GC layout change, a JIT,
a new crate, feature removal, or weakened checksum / sandbox /
`overflow-checks` / error-phase semantics are **STRUCTURALLY UNREACHABLE
under the standing hard constraints**. These are deliberate owner-approved
retirements, not execution failures.

## 5. What was NOT retired

- The three-engine differential contract (identical stdout, error phase and
  exit codes 0 / 1 / 2 / 4 across tree, VM, `exec` and the AOT subset).
- The exit-code contract, `.matrixc` v3 checksum, `--sandbox` deny lists,
  `overflow-checks = true`, and the crypto known-answer vectors.
- Zero-dependency core, zero feature removal, wasm behavioural parity.
- The P0 measurement protocol: no frozen baseline on this host, no loosened
  MAD / ±3% / ~5% sensitivity thresholds. A flaky self-test is invalidated
  and rerun, never loosened.
- The `ADVISORY_ONLY` marking. `docs/html/benchmark.html` is **not**
  regenerated from pre-v1.1.0 HTTP-run compute numbers.

## 6. Disclosures carried forward

- `benchmarks/results.phase3-run2.json` retains its historical top-level
  `numeric_band: BLOCK` (`prime/native` `mad_ok=false` / `INVALID_RERUN`).
  It is a frozen artifact and is **never edited or re-baselined**; it is
  superseded by a clean rerun. No threshold was loosened.
- The two Phase 5 Heavy-Load Checker runs (`run-20261112-003601`,
  `run-20261112-003649`) exercise the pre-existing `spid3rweb-app` container;
  its image and the HLC host `spid3r` (1.0.1) **predate v1.1.0**.
  `examples/spid3r_web/` was removed on 2026-11-09, so no v1.1.0 image was
  built. Those runs are an **HTTP non-regression** check only. Their §11
  compute numbers are flagged pre-v1.1.0 and are never attributed to v1.1.0;
  v1.1.0 compute comes exclusively from `tools/phase0_*.py` external timing.
- The Phase 4 certificate's "Docker daemon not running" limitation is stale:
  on 2026-11-12 the daemon is up and `/`, `/catalog`, `/health` all return
  HTTP 200.

## 6a. WS6 clean measurement rerun - observed results (2026-11-12)

Two consecutive clean sessions, best-effort `--pin-core 2` process affinity
(disclosed: the host stays shared; no baseline is frozen), written as
`benchmarks/results.phase3-run3.json` and `benchmarks/results.phase3-run4.json`:

- Both: top-level `ADVISORY_ONLY`, `numeric_band: PASS`,
  `selftest_pass: true`, and all 12 workload x variant rows
  `mad_ok=true / verdict=PASS` with zero `INVALID_RERUN`.
- Two-run agreement: all 12 compute medians within +/-3%
  (worst prime/exec +2.97%; json/vm +0.22%; map/vm -2.46%; every
  other row within +/-2.4%).
- The frozen `benchmarks/results.phase3-run2.json` was not edited or
  re-baselined; its historical top-level `numeric_band: BLOCK` is resolved by
  these clean artifacts. No MAD / +/-3% / sensitivity threshold was loosened.
- Invalidate-and-rerun was exercised, not bypassed: two harness attempts
  produced no artifact (120 s calibration timeout under host contention) and
  two attempts were blocked by the flaky sensitivity self-test. Every blocked
  attempt is preserved as evidence (`deliverables/gstack/phase5-*-BLOCK-evidence.json`,
  `phase5-ws6-attempts-evidence.log`) and none was edited.

Commit-over-commit comparison (mandatory DoD caveat), two layers:

1. **Pinned sessions vs the only `medians` baseline (`baseline-69b513a.json`,
   ADVISORY_ONLY, unpinned session)**: `tools/phase0_compare.py` reports
   8 flagged rows. This comparison is **config-incomparable and disclosed as a
   false alarm**: the untouched tree-walk engine drifted +30% under the pinned
   session (prime/tree 319.5 ms pinned vs 244.9 ms unpinned vs 244.88 ms
   baseline), which isolates host/config mismatch, not code. The tool itself
   marks such a comparison "informational, never a gate" for an unfrozen
   baseline. Report kept: `benchmarks/phase5-compare.json`.
2. **Config-matched comparison** (unpinned Phase 5 evidence sessions
   `phase5-unpinned-runA/B-BLOCK-evidence.json` vs the same unpinned
   baseline, same tool): **22 of 24 metric rows OK** - prime/vm 3.89x,
   prime/exec 3.85x, map/vm 0.72x, json/vm 0.83x - and prime/tree ratio
   1.0001, proving comparability. The **only confirmed regression** is
   `prime/no-opt` (compute 1.173x, wall 1.146x): the intentional WS5c
   semantic change (`--no-opt` now disables the runtime fused-superinstruction
   peeks, reverting to the unfused interpreter) plus the enlarged dispatch
   loop. `--no-opt` is an A/B diagnostic variant, not a performance target;
   every default-path row improved or stayed neutral. Report:
   `deliverables/gstack/phase5-compare-unpinned-configmatched.json`.

`spid3r bench --gate --compare --runs 5`: **8/8 BENCH_FLOORS PASS**.

WS8 full verification (all observed, nothing predicted):

- `bash tools/gate.sh 900` **twice consecutively**: `pass=9 fail=0 hang=0`
  both times, identical counts (4 feature configurations x tests + clippy,
  plus the release build).
- `tools/count_tests.sh`: **1318** tests (1295 core / 13 gui / 10 aot).
- `tools/difftest.sh`: **ALL EXAMPLES MATCH (tree == vm == exec)**.
- `tools/native_diff.sh examples/*.matrix`: **24 pass / 0 fail / 1 skip**
  (`math_util` has no `fn main`), 0 hang.
- `tools/phase0_fixtures.py --verify`: PASS (35 checks);
  `tools/phase0_manifest.py --verify`: PASS (14 files);
  `tools/phase0_selftest.py`: PASS on attempt 3 (two attempts invalidated by
  host noise and rerun; no threshold touched).
- Exit-code contract: valid `.matrixc` -> 0, checksum-corrupted container ->
  **4** ("bytecode integrity check failed"). `--sandbox` denies all four
  capability classes: filesystem, network, process and environment.

Two defects the gate surfaced in Phase 5, both fixed and disclosed:

1. **Instrumentation fidelity (production code).** The
   `GetLocal a; GetLocal b; Less; JumpIfFalse` and
   `GetLocal a; GetLocal b; Add; SetLocal; Pop` runtime fusions were the one
   fusion family that was NOT gated on `--prof` / `--coverage`, so the
   profiler counted a fused pair as a single instruction and under-counted
   the original opcode sequence by ~23%. They now obey the same gate as every
   other fusion, which is what `docs/BYTECODE.md` already documents. The
   pinned `--prof` totals are now 21242 (default path) and 22306
   (`--prof --no-opt`), and their difference is exactly the 1064 `Jump`
   dispatches that the compile-time loop rotation removes -- the rotation
   guard in `tests/engine_matrix.rs` is exact again. Non-instrumented
   execution is unchanged (the added conditions are true in normal runs).
2. **Test isolation (tests only).** Two tests wrote to fixed shared paths:
   the CSV round-trip wrote `target/test_out.csv`, which an unrelated process
   (LibreOffice had it open) made unwritable on Windows
   (ERROR_SHARING_VIOLATION), and the two parallel tests in
   `tests/tls_fetch.rs` shared one temp `.matrix` file and deleted each
   other's file mid-run. Both now use per-process / per-invocation unique
   paths; the assertions themselves are unchanged.

WS7 wasm sync: `web/spid3r.wasm` rebuilt (3,717,132 B) and
`web/spid3r_wasm_data.js` regenerated (4,956,565 B); the runtime reports
`1.1.0 / 1.0`. Gates: `verify_dev_snippets.js` 19/0, `verify_doc_claims.js`
37/37, `qa_wasm_test.js` 8/8. `benchmarks/report-data.json` was deliberately
NOT regenerated: it is the `spid3r bench` micro-benchmark report (separate
data source whose floor rates still match today's gate output within 0.1%),
and the pinned phase0 session numbers are not comparable to its unpinned
session basis.

## 7. Backlog (research track, explicitly not a commitment)

Collection fast paths for map / json beyond the Phase 5 scalar-only JSON
path and the per-VM map inline cache, HLC HTTP re-run against a rebuilt
v1.1.0 demo image, Ed25519-signed
`.matrixc`, `BCryptGenRandom`, `hmac_sha512`, and the `spid3r debug` exit-3
review. None of these may proceed by lifting a §9 RED LINE.
