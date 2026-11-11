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
