# Changelog

All notable changes to Spid3r are documented here. Spid3r is free software
under GPL-3.0-or-later. The language version (`LANGUAGE_VERSION = "1.0"`) is
independent of the toolchain release version and is unchanged by this release.

## 1.1.0 — 2026-11-11

Performance program P0 → P1 → P2 → P3, plus the Phase 4 verification/ship gate.
Shipped under the Phase 4 ship certificate
(`deliverables/gstack/ship-certificate-phase4-2026-11-11.md`): two consecutive
fully-green `tools/gate.sh 900` matrices, **1314** discovered tests, zero
feature removal, zero new crate dependencies (zero-dependency core preserved).

### Performance & correctness

- **P0 — Measurement, instrumentation, baseline lock.** Harness-side external
  microsecond timing (no new builtin, no language-surface change), checksum-
  first workloads, 9 warmups / 21 samples / 2 independent rounds, `MAD/median`
  and p95 dispersion, ±3% round-consistency verdict, and a blocking ~5%
  sensitivity self-test. Commit-over-commit median comparison
  (`tools/phase0_compare.py`). Vendored Heavy-Load Checker fixtures under
  `benchmarks/compare/` with a SHA-256 manifest and owner attestation
  (`license_status: OPEN → CONFIRMED`), plus a single ubuntu-only `phase0` CI
  job. **`src/` and `Cargo.*` were unchanged by P0.** All measurements are
  `ADVISORY_ONLY` — the host is unpinned/shared, absolute milliseconds are never
  frozen as a baseline, only ratios/deltas are portable.
- **P1 — VM fused superinstructions & cheap wins.** Three runtime fused
  superinstruction patterns in `src/vm/mod.rs` (peekhole matches on *existing*
  opcode bytes — no new `OpCode`, no `.matrixc` change), guarded so `--prof`,
  `--coverage`, debug and captured-frame paths fall back to the generic
  handlers. Small-integer string cache and allocation-reduced JSON writer/parser
  in `src/stdlib/mod.rs`, with `int_to_string_value` as the single authority for
  the `string` builtin and the VM inline path. VM `compute_ms` (P0→P1 medians):
  prime 116.765 → 34.247 ms, json 225.475 → 199.014 ms, map 32.684 → 23.890 ms.
- **P2 — Compile-time loop rotation, security fix, cache epoch.** A real
  optimizer-only `while` back-edge rotation pass in `src/vm/opt.rs` (delivered
  the `--no-opt ≥5%` structural gate on prime at +8.4%; `--prof --no-opt` remains
  unchanged, proving no opcode is hidden by a runtime fast path). Reproduced and
  fixed a `security.disable_natives` policy bypass: VM `Call` inlining now uses
  `std::ptr::fn_addr_eq` on installed function pointers instead of matching
  native names. Added a compiler/bytecode cache epoch
  (`spid3r-bytecode-cache-v2`) so stale caches cannot be loaded. Regression
  coverage in `tests/phase2_perf_security.rs` and `tests/engine_matrix.rs`.
- **P3 — Native/AOT inline i64 arithmetic & hybrid int compare.** Rewrote
  `emit_i64_binop` in `src/native/ir.rs` to inline Add/Sub/Mul via
  `llvm.*.with.overflow.i64`, with guarded Div/Mod/Neg that fall back to the
  original trampoline on overflow / divide-by-zero / `i64::MIN % -1` (error
  messages and exit codes unchanged). Hybrid int compare compiles boxed int
  leaves to inline register compares via two pure helpers in `src/native/rt.rs`
  (`spid3r_is_int`, `spid3r_int_of`), with an unchanged generic slow path.
  Located and fixed a non-deterministic loop-bounds defect whose root cause was
  a missing `load` — a stack-slot address was passed where a boxed value pointer
  was required — and pinned the defect class with the `hybrid_loop_bounds`
  regression in `tests/engine_matrix.rs`. `docs/NATIVE.md` updated. On prime the
  native engine reached 9.404 ms vs VM 30.624 ms (3.26x faster); map/json native
  targets were **not** met (see deviations).

### Docs, wasm & deployment sync (each phase)

- The canonical deployment set (`web/spid3r.wasm`, `web/spid3r.js`,
  `web/spid3r_wasm_data.js`) was rebuilt and the embedded base64 module
  regenerated; `test_wasm_embedded --against fresh` verified 25 presets
  behaviorally equivalent on both engines. `pub mod native` is now compiled
  unconditionally so P3's runtime helpers are present in the wasm build.
- `README.md`, `docs/html/doc.html`, `docs/html/benchmark.html`,
  `docs/html/playground.html`, `docs/STANDARD_LIBRARY.md`, `docs/BYTECODE.md`,
  `docs/NATIVE.md`, `ENGINE_CONTRACT.md`, `BYTECODE_STABILITY.md`,
  `SECURITY.md` and `docs/diagrams/*.svg` were synced with the shipped code and
  re-verified by `verify_doc_claims.js` (37 claims, 0 failures) and the snippet
  gate. `benchmarks/report-data.json` and the benchmark page were regenerated
  together (four-engine comparison, 2026-11-11 21:10).

### Approved written deviations & limitations (certificate §5)

These are registered, not silently relaxed; **no unmet gate is claimed as met**.

- P1 `default VM ≥3% faster than --no-opt` (best 0.63%) and `json ≤199 ms`
  (199.014 boundary) were later resolved by the P2 rotation pass (prime +8.4%,
  json 192–195 ms).
- P2 stretch targets — `map ≤7.6`, `json ≤142`, compute geomean `≤2.0x`,
  tree-walk `≤1.5x` — were pre-registered as not met before measurement; the
  full `Value`-layout rewrite was declined; a staged, reversible slice shipped
  instead. The `--no-opt ≥5%` "≥1 workload" scope was met on prime only.
- P3 `map/json native ≥1.3x faster than VM` was not met (map 2.19x / json 1.11x
  slower): collection operations dominate those hot loops and are not covered
  by arithmetic inlining. Backlog: collection fast path + map inline cache.
- The Heavy-Load Checker **HTTP section was not re-run** (registered
  limitation): the target demo app was removed from the repository on
  2026-11-09 and the Docker daemon was not running. Its reference run
  `run-20261110-235227` is retained. The compute section is covered by the
  in-repo protocol.
- Performance numbers remain `ADVISORY_ONLY` on this unpinned host; a dedicated
  pinned-core host is required to lift them to a frozen baseline.

### Housekeeping

- Release version `1.0.0 → 1.1.0` in `Cargo.toml`, `Cargo.lock`, and
  `packaging/nsis/spid3r.nsi`. `src/lib.rs::VERSION` is derived from
  `env!("CARGO_PKG_VERSION")`; `LANGUAGE_VERSION = "1.0"` is unchanged.
- `tools/qa_wasm_test.js` now asserts the wasm version against the crate
  version instead of a hardcoded literal, so a future bump cannot silently
  break the wasm QA gate.

## Unreleased — Phase 5 gap closure (2026-11-12)

No version bump: `LANGUAGE_VERSION = "1.0"` and the `1.1.0` release version are
unchanged. Phase 5 closes the remaining audit gaps and refreshes the
documentation, without changing the language surface or lifting a constraint.

- **Plan / docs accuracy.** `plan.html` (a local-only, gitignored working
  document) had its §1–§14 factual errors corrected and gained §15, an
  as-shipped v1.1.0 scorecard and Phase 5 close-out: retracted the incorrect
  "investigator report not delivered" claim, reconciled the test inventory
  (1314 shipped in v1.1.0; four new Phase 5 parity tests bring it to 1318 =
  1295 core / 13 gui / 10 aot), corrected the false "AOT consumes the same Chunk" claim
  (`src/native/ir.rs:4498` lowers from the AST via `crate::lower::lower`), and
  refreshed stale §9 `file:line` references. `docs/ENGINE_CONTRACT.md` gap #2
  now states that `--max-call-depth` applies to tree + VM only (AOT excluded),
  matching `--help` and `src/cli.rs`.
- **Formal target retirement.** `docs/PERF_TARGET_RETIREMENT.md` quotes every
  original `plan.html` §7 number verbatim, records the as-shipped results, and
  formally retires the P2 stretch / P3 aspirational numeric targets that would
  require a `Value` / GC layout change, a JIT, a new crate, or weakened safety
  semantics. No RED LINE was relaxed and no feature was removed.
- **Two full Heavy-Load Checker runs.** `run-20261112-003601` and
  `run-20261112-003649`: both **GO / PASS / 100-of-100 health / 0.0000%
  errors**, RPS 86.80 vs 85.62 (1.36% apart), `Comparable: YES`. TTFB p95
  differs by 6.21%, which is **disclosed** rather than reported as a ±3% pass
  (shared unpinned host, legacy-shell cgroup fallback). **Disclosure:** both
  runs exercise the pre-existing `spid3rweb-app` container image and the HLC
  host `spid3r` 1.0.1, i.e. **pre-v1.1.0** binaries. They are an HTTP
  non-regression check only; their §11 compute numbers are never attributed to
  v1.1.0, and `docs/html/benchmark.html` is not regenerated from them. The
  Phase 4 certificate's "Docker daemon not running" limitation is stale.
- **Performance (safe, additive).** A scalar-only JSON writer fast path that
  keeps the documented snapshot-clone recursion as its fallback (removing
  those clones outright would reintroduce a non-reentrant `Mutex` deadlock on
  self-referential containers); a `MapData.generation` counter plus a bounded,
  per-VM map inline cache, which is layout-safe because `Value::Map` is
  pointer-sized; and `opt_enabled` threaded from the CLI so `--no-opt` also
  disables VM runtime peeks. No `Value` / GC layout change, no new `unsafe`,
  no new crate, no compiler / bytecode-format change (so no
  `BYTECODE_CACHE_EPOCH` bump), and no `BENCH_FLOORS` change.
- **A4 closed without an edit.** `tests/engine_matrix.rs::CASES` and
  `tools/phase0_fixtures.py --verify` already provide verbatim three-engine
  parity for the HLC fixtures. Adding them to `tools/difftest.sh` would break
  its byte-exact diff because the verbatim sources emit a non-deterministic
  `TIME_US` line.
- **Measurements remain `ADVISORY_ONLY`.** Two consecutive clean sessions,
  `benchmarks/results.phase3-run3.json` and `benchmarks/results.phase3-run4.json`
  (best-effort `--pin-core 2` process affinity on a still-shared host,
  `--advisory`, nothing frozen): both `numeric_band: PASS`,
  `selftest_pass: true`, all 12 workload x variant rows `mad_ok=true / PASS`,
  zero `INVALID_RERUN`, and two-run agreement within ±3% (worst prime/exec
  +2.97%). The frozen `benchmarks/results.phase3-run2.json` (top-level
  `numeric_band: BLOCK`) is never edited or re-baselined; the clean rerun
  supersedes it. No MAD / ±3% / sensitivity threshold was loosened: four
  invalidate-and-rerun attempts (two no-artifact calibration timeouts, two
  self-test flakes) were discarded and rerun, each preserved as evidence.
  Commit-over-commit comparison (`tools/phase0_compare.py`): the
  pinned-vs-unpinned baseline comparison is disclosed as
  **config-incomparable** (the untouched tree engine drifted +30% under the
  pinned session), and the config-matched unpinned comparison is **22 of 24
  metric rows OK** - the only confirmed regression is `prime/no-opt`, the
  intentional `--no-opt` semantics change on the A/B diagnostic path, not a
  performance target. `spid3r bench --gate --compare --runs 5`: 8/8
  `BENCH_FLOORS` PASS.
- **Wasm / doc sync.** `web/spid3r.wasm` rebuilt (3,717,132 B) and
  `web/spid3r_wasm_data.js` regenerated (4,956,565 B); the runtime reports
  `1.1.0 / 1.0`. `tools/verify_dev_snippets.js` 19/0,
  `tools/verify_doc_claims.js` 37/37, `tools/qa_wasm_test.js` 8/8.
  `benchmarks/report-data.json` was deliberately not regenerated: it is the
  separate `spid3r bench` micro-benchmark data source, whose floor rates
  still match today's gate output within 0.1%.
- **Instrumentation fidelity fix.** The `GetLocal a; GetLocal b; Less;
  JumpIfFalse` and `GetLocal a; GetLocal b; Add; SetLocal; Pop` runtime
  fusions were the only fusion family not gated on `--prof` / `--coverage`,
  so the profiler counted a fused pair as one instruction and under-counted
  the original opcode sequence by ~23%. They now obey the same gate as every
  other fusion, which is what `docs/BYTECODE.md` already documents. The
  pinned `--prof` totals become 21242 (default path) and 22306
  (`--prof --no-opt`), and the difference is exactly the 1064 `Jump`
  dispatches the compile-time loop rotation removes -- the rotation guard in
  `tests/engine_matrix.rs` is exact again. Non-instrumented execution is
  unchanged.
- **Test isolation fixes.** The CSV round-trip test wrote to the fixed path
  `target/test_out.csv`; an unrelated process holding that file open made
  the write fail on Windows with ERROR_SHARING_VIOLATION, which failed the
  gate in all four feature configurations. The two parallel tests in
  `tests/tls_fetch.rs` shared one temp `.matrix` file and deleted each
  other's file mid-run. Both now use per-process / per-invocation unique
  paths; the assertions are unchanged.
- **Verification gates (WS8, all observed).** `bash tools/gate.sh 900` twice
  consecutively: `pass=9 fail=0 hang=0` both times, identical counts.
  `tools/count_tests.sh`: 1318 tests (1295 core / 13 gui / 10 aot).
  `tools/difftest.sh`: ALL EXAMPLES MATCH (tree == vm == exec).
  `tools/native_diff.sh examples/*.matrix`: 24 pass / 0 fail / 1 skip
  (`math_util` has no `fn main`) / 0 hang. `tools/phase0_fixtures.py
  --verify` PASS (35 checks), `tools/phase0_manifest.py --verify` PASS (14
  files), `tools/phase0_selftest.py` PASS on attempt 3 (two attempts
  invalidated by host noise and rerun; no threshold touched).
  Exit-code contract re-verified: valid `.matrixc` -> 0, checksum-corrupted
  container -> 4. `--sandbox` denies filesystem, network, process and
  environment. `spid3r bench --gate --compare --runs 5`: 8/8 `BENCH_FLOORS`
  PASS.
  Final gate and verification results are recorded in `plan.html` §15 and
  in the Phase 5 commit message.
