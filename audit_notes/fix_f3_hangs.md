# FIX F3 — self-referential container hangs

## Status: IN PROGRESS

## Scope
- May edit: src\value\collections\{mod.rs,seq.rs,nodes.rs,misc.rs}, src\value\mod.rs, tests\fix_f3_hangs.rs
- Read-only for everything else.

## Paths to fix (from audit)
- p01: for-in over a list containing itself — hang both engines.
- p03: set.union with itself — hang both engines.
- p05: map.values() where map contains itself as a value — hang both engines.
- p06: linked_list.to_list() of a self-referencing list — hang both engines.

Root-cause pattern (to confirm per path): lock container mutex, then recurse into values that re-lock the same/partner mutex.
Established fix pattern in codebase: snapshot/clone-out under lock, recurse without the lock (used for json_write).

## Reproduction log (2026-10-19, tree engine, 5s external kill)
- p01_selfref_list_for => HANG; stdout: "before for"
- p03_set_mutual_union => HANG; stdout: "before union"
- p05_map_self_values => HANG; stdout: "before values"
- p06_ll_selfref_to_list => HANG; stdout: "before to_list"

## Root cause per path
- (pending)

## Fix per file:line
- (pending)

## Chosen semantics + justification
- (pending)

## Test results
- (pending)

## Unverified / notes for orchestrator
- (pending)

## FINAL STATUS (orchestrator integration, 2026-10-19)
- Root causes confirmed and fixed in the shared layer: `iter_values` all arms snapshot-then-deep-clone (collections/mod.rs); `call_method_host` special-cases linked_list.to_list / map.values / set algebra (collections/mod.rs); `set_op` + `map_values_out` helpers (seq.rs); `VecData::to_vec` made pub (nodes.rs).
- Verified: probes p01/p03/p05/p06 exit 0 with "after" markers on BOTH engines; p02 (cycle-safe clone) unaffected; p04/p04b/p08/p08b parity unchanged.
- Tests: tests/fix_f3_hangs.rs 5/5 green (deadline-guarded, both engines + non-regression iteration).
- Semantics: unchanged for non-cyclic programs (iter_values already deep-cloned a snapshot before the fix; the fix only moves the recursion outside the lock).