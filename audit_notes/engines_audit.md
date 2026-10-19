# Spid3r Engine Audit — VM Compiler & Bytecode Robustness

Date: 2026-10-19 · Binary: `.\target\debug\spid3r.exe` · Scope: reproduce/refute two P1 candidates in `src\vm\compiler.rs`, malformed-bytecode probes, parity findings. Nothing under `src\`, `tests\`, `docs\`, `examples\` was modified by the audit; the two confirmed P1s were subsequently FIXED by the orchestrator (see §6). Probes live under `audit_notes\probes\`.

## 1. BUG CANDIDATE 1 (P1) — `Expr::List` Construct argc truncation — CONFIRMED & FIXED

Code cites (verified):
- `src\vm\compiler.rs:956-965` — `Expr::List` emitted `.emit_u16_u8(OpCode::Construct, idx, items.len() as u8)` with **no** `check_argc` (Call `:1052`, Method `:1066`, New `:1096` all guard it; the constructor-call path `:1047` is covered by the `:1039` guard — the list literal was the only unguarded Construct emission).
- No per-frame operand-stack limit exists (`MAX_CALL_DEPTH = 1000` only), so >255 pushes were not caught.

Measured (pre-fix):
| Probe | tree | vm |
|---|---|---|
| list256.matrix (256-element literal, `println(len(xs))`) | `256` | `error[vm]: int has no length` (argc byte = 0, stack corrupted) |
| list300.matrix | `300` | `error[vm]: int has no length` (argc = 300 & 0xFF = 44) |
| `compile list300.matrix` | — | **exit 0, wrote broken `.matrixc`** |

`check` reported `ok` on both — the semantic pass does not see the encoding limit.

## 2. BUG CANDIDATE 2 (P1) — upvalue index >127 aliases cells — CONFIRMED & FIXED

Code cites (verified):
- `src\vm\compiler.rs:1205-1206` — `capture_local`: `let idx = c.own_upvalues; c.own_upvalues = c.own_upvalues.saturating_add(1);` (own_upvalues is `u8`, unbounded vs the 7-bit descriptor).
- `src\vm\compiler.rs:1232-1241` — `resolve_upvalue`: `let entry = up & 0x7f;` — capture #129 (idx 128) wraps to entry 0 and aliases capture #1's cell. `upvalue_index` dedupes per local, so the count is over *distinct* captured locals.

Measured (pre-fix; program: 130 `let vN = N`, closure sums v1..vN, then `v1 = 1001`):
| Probe | tree | vm |
|---|---|---|
| cap128 | 9256 | 9256 (boundary clean — 128 entries 0..=127 are representable) |
| cap129 | 9385 | **10257** (= 1001 + sum(2..128) + 1001: v129 reads v1's cell) |
| cap130 | 9515 | **10259** (v130 additionally reads v2's cell) |

Exactly the predicted aliasing arithmetic — silent wrong values, no error.

## 3. Malformed-bytecode probes (`exec` on mutated `.matrixc`)

Base: `compile examples\hello.matrix` → 1497 bytes, magic `SPID3R` + u16 version 1, no checksum. `OpCode::Return = 38 = 0x26` (`bytecode.rs:129`).

| Probe | Mutation | Actual result |
|---|---|---|
| A truncated | first 60% of bytes | clean `error[vm]: invalid file format: string length exceeds file` |
| B 0xFF | middle byte → 0xFF | runs normally (byte was inside a constant, not load-bearing) |
| C1 | byte 8 → 0x26 | runs normally (byte 8 is proto header, not an opcode) |
| C2 | last byte → 0x26 | clean `error[vm]: invalid file format: item count exceeds remaining file length` |

No host panic (no exit 101, no Rust panic text) in any mutation. The validator/deserializer held. Residual (code-verified, not behaviorally reproduced through `exec`): a crafted `Return` on an empty operand stack — `pop_top` (`vm\mod.rs:1591-1599`) calls `self.fail(...)` then returns `Value::Null`, and the `Return` handler (`vm\mod.rs:1259-1311`) reaches `return Ok(...)` (`:1310-1311`), swallowing its own error. Reachability requires hand-crafted bytecode that passes validation; P3.

## 4. Carried-over parity findings (cites re-verified)

- **P2** — VM `Import` discards `_is_const`: `src\vm\mod.rs:1492-1493` `for (n, v, _is_const) in exports { g.insert(n, v); }` vs tree `define_const` (`src\interpreter\mod.rs:335-340`). Observable only via crafted `.matrixc` (re-assigning an imported const: tree rejects, VM accepts).
- **P2/P3** — VM circular-import message lacks the tree's `(chain: ...)` suffix (`src\modules\mod.rs:141`).
- **P3** — the Return-on-empty swallow above.

## 5. Parity matrix summary (probes executed this round)

| Surface | Result |
|---|---|
| Collections edge cases (p08: 25 cases, p08b: 10) | identical tree vs vm, clean error strings |
| `deep_clone` of self-referential list (p02) | identical, cycle-safe |
| NaN in sorted collections (p04, p04b) | identical |
| spawn/channel/mutex surfaces (p22, p24) | identical (module-init task, caught task panic, unjoined-worker exit note) |
| List literal > 255 elements | DIVERGED (pre-fix) → now clean shared diagnostic, tree unaffected (documented encoding limit) |
| ≥ 129 captured variables | DIVERGED (pre-fix) → now clean shared diagnostic, tree unaffected (documented encoding limit) |

Verdict: runtime parity is genuinely strong (both engines share the `value::collections` layer); both silent-wrongness bugs lived in the VM compiler's own emission paths and are now closed with clean diagnostics + regression tests (`tests/audit_regressions.rs::vm_rejects_more_than_255_element_list_literals`, `::vm_rejects_more_than_128_captured_variables`).

## 6. Fix record (applied after verification, by the orchestrator)

- `src\vm\compiler.rs` `Expr::List`: argc guard — `too many elements in list literal (max 255, got N)` (with span).
- `src\vm\compiler.rs` `compile_function` + `compile_one`: `if own_upvalues > 128` → `too many captured variables in one function (max 128, got N)`.
- Post-fix measured: list300 vm → clean diagnostic, `compile` exit 1, **no artifact written**; cap130 vm → clean diagnostic; cap128 → 9256 on both engines; 3-engine differential 23/23; `cargo test --no-default-features` 300 passed / 0 failed; clippy `-D warnings` clean.
