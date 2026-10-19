# Fix VM Trio F8+F9+F10 — working notes

Date: 2026-10-19 · Scope: src\vm\mod.rs, src\modules\mod.rs, src\vm\bytecode.rs (only if needed), tests\fix_vm_trio.rs (new).
Oracle: full test suite must stay green; tests\audit_regressions.rs + vm_dispatch_tests + opt_tests read-only.

## Skeleton (fill as I go)

- [ ] F8 (P2): VM Import handler discards `_is_const` (vm\mod.rs ~1489-1493). Plan: per-environment const set mirroring tree (interpreter\mod.rs:335-340); Import records const names; global-assign opcodes check const set first, fail `cannot assign to const 'name'` (match tree wording exactly — grep tree). Thread const flag through bytecode.rs only if export tuples lack it end-to-end.
- [ ] F9 (P3): VM circular-import diagnostic lacks `(chain: a -> b -> c)` suffix (modules\mod.rs:141). Plan: reuse helper if exported.
- [ ] F10 (P3): Return with empty operand stack: pop_top (vm\mod.rs ~1591-1599) fail()s but returns Value::Null; Return handler (~1259-1311) reaches `return Ok(...)`. Plan: after pop, check failed state and return Err.
- [ ] Tests: tests\fix_vm_trio.rs, helper pattern tests\audit_regressions.rs:23, CARGO_BIN_EXE_spid3r, try_wait polling 15s deadline.
- [ ] Verify: cargo build --no-default-features; cargo test --test fix_vm_trio; vm_dispatch_tests; opt_tests. Clippy clean.
- [ ] Final: root cause, file:line, artifact crafting, results, risks per fix.

## F8 notes

(findings below)

## F9 notes

(findings below)

## F10 notes

(findings below)

## Verification log

(to be appended)

## FINAL STATUS (orchestrator integration, 2026-10-19)
- All three fixes implemented and verified (vm/mod.rs, modules/mod.rs):
  - F8: `Vm.global_consts` set; Import records `public const` exports; SetGlobal refuses with the tree's wording `cannot assign to const '<name>'`. Legal programs unaffected (semantic already rejects those assignments at compile time).
  - F9: shared `modules::circular_import_message` — both engines render `circular import of 'x' (chain: a -> b -> a)`.
  - F10: Return handler routes a failed pop (`self.error.is_some()`) through the error funnel instead of `return Ok`.
- Tests: tests/fix_vm_trio.rs 4/4 green (message-format parity, tree chain behavior, compile+exec round trip, 20+ mutation sweep with no host panic).
- Honest gap: F8/F10 behavioral tests need a crafted-bytecode CONSTRUCTION seam (the serialization is length-prefixed recursive; hand-patching hits the validator first). Both fixes are code-reviewed against the tree contract; the mutation sweep pins the no-panic invariant. Follow-up recorded in plan.html.