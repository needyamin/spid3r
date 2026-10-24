# The Spid3r Engine Contract (ARC-06)

Status: **Adopted** (2026-10-22). This is the acceptance bar every execution
engine must meet, and the definition of "a language feature is implemented."
It is the measurable gate for ADR-001 (everything on `Chunk`) and ARC-05
(native consumes `Chunk`): adding a backend = passing this contract, nothing
else.

## The contract

For a source program `P`, let `C` be the canonical artifact the front end
produces (`source → lexer → parser → semantic → lower → Chunk`). Every backend
`B` ∈ { tree (oracle), vm, native/AOT, wasm, future JIT } **must produce
identical observable behavior** for `P`:

1. **stdout** — byte-for-byte identical.
2. **stderr** — identical `Phase` label (`error[runtime]`, `error[semantic]`,
   …) and identical message text, after normalizing source paths/spans. The
   *decoration* differs by documented, listed gaps (below); the phase and the
   first message line must not.
3. **exit code** — identical (0 success, 1 user-program error, 2 usage/
   compile-time error).
4. **side effects on granted capabilities** — files written, bytes sent, host
   natives called: identical sequence of observable effects (within the
   sandbox model of `docs/API_BOUNDARIES.md`).
5. **resource accounting** — within backend-declared tolerances (a JIT may be
   faster; it may not do *more*).

"A feature is implemented" means: **once** in `lower` + the `Chunk` compiler +
`value::collections`, and **every** backend passes the contract for the new
construct. A construct a backend cannot run must be **rejected at compile time
with a clean `error[usage]` diagnostic** — a construct that compiles but
computes a different answer is the worst possible failure.

## The executable form

The contract is not prose — it is gated:

- `tests/engine_matrix.rs` — one case per statement/expression form in
  `src/ast/mod.rs`; asserts expected stdout, identical stderr phase+message,
  identical exit code on tree/vm, plus an `aot: true` subset. **A new language
  construct requires a new row here** (with `aot: true` unless the backend
  rejects it — and a rejection needs a diagnostic row too).
- `tests/difftest.rs` + `spid3r test` — the 25-example differential suite
  (tree == vm == exec). `spid3r test` compares stdout only, which is why the
  matrix must also exist.
- `tools/count_tests.sh` — the only accepted test counter; pinned in CI
  (CI-03).
- Future (CI-02/CI-06): native + wasm in the PR differential matrix; a nightly
  all-backend contract job.

## Documented, accepted gaps (contract exemptions)

These are the *only* permitted divergences; each must be listed here or the
conformance test fails:

1. **Native diagnostic decoration** — an AOT binary cannot reproduce source
   spans, snippets, or call-stack traces (see `spid3r_uncaught` in
   `src/native/rt.rs`). Phase + message must still match; uncaught native
   errors additionally print `uncaught throw (thrown: …)`.
2. **Native `MAX_CALL_DEPTH` accounting** — the depth limit is shared, but
   native frames consume host stack differently; the abort message matches,
   the trigger point may differ by a small constant.
3. **Timing/concurrency interleaving** — `spawn` scheduling is nondeterministic
   on every engine; programs must not depend on it (the test suite orders
   handoffs explicitly).

## Rules for backend authors

1. Consume the canonical artifact, never the raw AST (ADR-001). Re-implementing
   semantics is a contract violation *by construction*, even if tests pass.
2. Delegate data semantics to `value::collections`; error messages come from
   that one implementation, not from a per-backend re-wording (the PAR-04
   finding: `check_arity` vs `method {name} expects …`).
3. Errors are catchable where tree/vm make them catchable (`rt_throw`, not
   process exit — the missing-method `abort` fix).
4. New runtime trampoline symbols require a `cargo build` before the AOT tests
   (cargo test does not rebuild the staticlib).
5. Defensive rejections stay in the code even when the parser makes them
   unreachable (PAR-01 finding: class-in-function is rejected by the parser on
   all engines; the native guard remains).
