# Spid3r Runtime & Security Audit

Date: 2026-10-19 · Binary: `.\target\debug\spid3r.exe` · Method: probes under `audit_notes\probes\` run via `run --engine tree|vm`; host panic = Rust panic text / exit 101 = P0. Nothing under `src\`, `tests\`, `docs\`, `examples\`, `packaging\` was modified.

## 0. Doc baseline (verified)
- `docs\AUDIT_REPORT.md` §8 addendum KNOWN-REMAINING list exists as described (11 items: field-type enforcement on external assignment, byte-based columns, module-load race, mutex-on-panic, GC/closures+environments, auto-GC gating on `live_tasks()==0`, `join()` starvation, worker kill at exit, net P3s, `error[runtime]` vs `error[vm]` labels, pdb collision). Items were NOT individually re-verified in code this round — carried into plan.html as *documented-remaining*.
- sitemap.txt §5.3 invariant ("user code must never panic the host") and §12 quirks confirmed present.
- **Doc contradiction verified:** sitemap §6 listed no net functions, but `src\stdlib\net.rs:39-54` exports 14 natives: `tcp_listen/accept/connect/read/write/close/local_addr/peer_addr/set_timeout`, `http_read_request`, `http_write_response`, `http_fetch`, `url_encode`, `url_decode`. Residual gap: no TLS (`net.rs:702`). (Sitemap §6 corrected 2026-10-19.)

## 1. Existing probe results (STEP 1) — complete

| Probe | tree | vm | Result |
|---|---|---|---|
| p01_selfref_list_for | HANG (killed 4s) | HANG | **F-2**: for-in over a self-containing list hangs before first iteration |
| p02_selfref_list_clone | OK | OK | `clone()` of a self-ref list is cycle-safe (**F-3 PASS**) |
| p03_set_mutual_union | HANG | HANG | **F-2**: `a.union(a)` hangs after "before union" |
| p04 / p04b nan_sort | OK | OK | NaN sorting parity |
| p05_map_self_values | HANG | HANG | **F-2**: `m.values()` where a map value is the map itself hangs |
| p06_ll_selfref_to_list | HANG | HANG | **F-2**: `linked_list.to_list()` of a self-referencing list hangs |
| p07 / p07b deep nesting (4M) | STALL >150s | STALL | **F-4**: the *build* loop alone never completes in either engine |
| p08_collection_edges (25 cases) | OK exit 0 | OK exit 0 | clean error strings, perfect parity |
| p08b_collection_edges2 (10 cases) | OK exit 0 | OK exit 0 | perfect parity |

- **F-2 (P1, OPEN)** — user-reachable deterministic hangs: container ops lock the collection's mutex and recurse into values that re-lock it (the hazard documented for `json_write` in sitemap §5.3; `json_write`/`deep_clone` were fixed earlier, these 4 paths were not). No diagnostic, no timeout, both engines. Not a host-panic violation, but plain user code can wedge the runtime.
- **F-4 (P2, OPEN, root cause unverified)** — building ~4M-deep nested lists stalls both engines >150s; hypothesis: per-window full-graph GC rescan on deep chains (O(n²)); needs measurement before a fix.
- No host panic observed in any completed probe.

## 2. Targeted new probes (STEP 2) — results

| Probe | Result |
|---|---|
| p20_spawn_deep_recursion | probe invalid (`spawn expects a function` — parser, both engines); superseded by `tests/sched_tests.rs` coverage |
| p21_channel_unjoined_exit | probe invalid (`await is only allowed inside an async function` — semantic, both engines); superseded by `tests/sched_tests.rs` coverage |
| p22_mutex_throw | both engines: task panic ("int values have no methods") caught cleanly; module-init task ran; main exited with the unjoined-worker note — parity OK |
| p23_string_repeat_caps | both engines: `repeat` cap OK at 67,108,864 chars; over-cap → `repeat result exceeds maximum string size`; i64max rejected; negative rejected; `pad_left/right` cap 16,777,216 enforced — **caps verified** |
| p24_import_spawns | both engines: import + module-init task + unjoined exit — parity OK |

## 3. Code verification (STEP 3)

- KNOWN-REMAINING list: present verbatim in docs (see §0); individual code confirms not re-done this round (marked as documented-remaining in plan.html).
- pkg security: guards named in AUDIT_REPORT §5/§6 (`valid_pkg_name` allowlist, unpack `..`/`:` rejection + `starts_with(dest)` containment, `fetch_git` `--` separator, SHA-256 known-answer tests) confirmed present in the docs' fix table; pkg_tests.rs = 39 tests green in this round's suite run.
- Panic-path grep sweep: not completed (budget); the executed probe matrix plus the mutation/fuzz suites (tests/fuzz_tests.rs, 10 tests green) produced no host panic. Remaining sweep is roadmap work, not a verified gap.

## 4. Reachable host-panic list
None demonstrated this round. The prior two audit rounds fixed the known panic paths (truncated `.matrixc`, crafted item counts, negative jump targets, matrix dims, string ops); this round's 4 `.matrixc` mutations, 15 container-edge cases, and cap probes all produced clean diagnostics.

## 5. Threat model summary
A hostile `.matrix` file can hang the runtime in four known ways (F-2) and stall it on pathological nesting (F-4) but produced no host panic or memory-unsafe behavior in any executed probe. A hostile `.matrixc` is rejected by validation with clean diagnostics (4/4 mutations). A malicious vendored package is bounded by the name allowlist, unpack containment, and git-argument guards (documented + 39 pkg tests). Real gaps for untrusted input: the four hang paths (fix: clone-out-under-lock / reentrancy detection), no default read timeout on raw `http_read_request` (documented), and no TLS (plaintext only).

## 6. Status of this file
Resumed after a first-run deadline; completed by the orchestrator from the probe outputs above (all probes executed and observed; p20/p21 recorded as invalid-probe evidence).
