# Audit: state desynchronization and type confusion at cache/allocation boundaries

**Date:** 2026-10-22 · **Scope:** Spid3r runtime (`src/`) · **Trigger:** a
report of "PHP2Shell"-class vulnerabilities (CVE-2026-109618 / CVE-2026-109619).

> **Note on the CVE references.** Those specific identifiers could not be
> verified against any advisory available to this audit, so nothing below
> depends on their details. What *is* auditable — and what this note covers —
> is the mechanism described in the report: **a build path that mutates
> shared/global state (indices, registries, caches) before its final
> allocation is known to have succeeded, so a mid-build failure leaves a
> half-committed entry that a later worker reads with the wrong type.**

The good news up front: this runtime is written in Rust, so the *classic*
PHP-style failure (a `malloc` returning NULL while an index was already
advanced, then dereferencing a stale pointer) cannot occur — Rust aborts on
allocation failure and has no nullable pointers in safe code. The equivalent
hazards here are **logical**: partial commits, unvalidated cached artifacts,
and silent fallbacks. The audit found four such sites; three are now fixed
and one is a documented, bounded behavior.

---

## 1. Where state changes before success is guaranteed

### 1a. Fixed: module load was check-then-act (the exact reported pattern)

`modules::load` used to consult the cache, check `in_progress`, then *after*
parsing and checking the module, push `in_progress` and only then insert into
`loaded`. Two tasks importing the same new module could both pass the check
and both execute the body; and the in-progress marker was pushed **after**
fallible work (read, parse, check, lower), so a failure there left no marker
at all while the module's top-level statements had already run against the
**shared globals**.

Now (`src/modules/mod.rs`, `claim_load` + `LoadClaim`):

```rust
// Claim and cache check happen under ONE lock, before any fallible work.
let _claim = match claim_load(&interp.modules, &resolved, path) {
    Claim::AlreadyLoaded(exports) => return Ok(exports),   // served from cache
    Claim::Cyclic(msg)           => return Err(/* genuine cycle */),
    Claim::Acquired(claim)       => claim,                 // this thread owns it
};
let src = std::fs::read_to_string(&resolved)?;   // failure: guard drops
// ...parse, check, lower, execute...
_claim.complete(exports.clone());                // single-lock commit + notify
```

`LoadClaim::drop` releases the path and wakes waiters **on any early return
and on panic**, so there is no path that leaves the path claimed. The same
claim is used by the VM's embedded and source import paths
(`src/vm/mod.rs`), so both engines share one transaction boundary.

### 1b. Fixed: bytecode validation was incomplete (see §3)

A cached artifact could reference a constant as a *name* without the loader
checking it was a string, read an upvalue cell that does not exist, or carry
`SuperCtor`/`SuperMethod` operands that were never validated at all. These
are "state read with the wrong type" at the cache boundary; fixed in §3.

### 1c. Verified clean (no change needed)

| Site | Why it is safe |
|---|---|
| `sched::spawn_worker` (`src/sched/mod.rs`) | Reserves `alive += 1` under the accounting lock, and on `thread::spawn` failure **rolls the reservation back** (`alive -= 1`), so the cap stays exact. |
| Network handle table (`src/stdlib/net.rs`) | `register()` is called **only after** the connect/accept succeeded; handles are typed (`enum Sock { Listener, Stream }`) and `get_stream`/`get_listener` reject a handle of the wrong tag. `tcp_close` removes the entry. |
| GC registry (`src/value/gc.rs`) | `track()` is called with an already-allocated `Arc`; the push cannot fail and there is no "reserved but unbuilt" window. |
| VM compilation | Constant/function/class pools are per-`Chunk`, built on the stack, and the whole chunk is discarded if compilation returns `Err`; nothing is published until the program is fully compiled. |
| Class building (`build_class`) | Each method closure is built and registered first; the class value is defined into the environment only after the whole class exists. |

### 1d. Documented, bounded behavior: a failed module body re-executes

If a module's top-level body throws, the module is **not** cached and the
claim is released, so a retry re-runs the body. Its earlier bindings are in
the shared globals but are not statically reachable (`import` failed, no
exports were published), and re-execution is serialized by the claim, so no
second worker observes a half-built module. The observable effect is
**duplicated side effects on retry** (e.g. a `println` in the module body).
This matches the "failed import, try again" semantics of Python/JS; if you
need strict once-only side effects, the recipe in §2 applies (build into a
staging table and commit atomically — the reason it is not done here is that
module globals are shared *by design*, so staging them would change the
module ABI).

---

## 2. The all-or-nothing (transactional) recipe

The runtime's reference implementation is `LoadClaim`. The rules it encodes,
generalized to any "allocate in a shared pool, then advance global state"
path:

1. **Claim first, under the same lock that guards the committed state.** The
   check (`is it already there?`) and the reservation (`it is mine now`) must
   be one critical section. Two workers must never both win.
2. **Build into locals.** Read, parse, validate, allocate into a local
   buffer/`Vec`/struct. Nothing global is touched yet.
3. **Commit once, under the lock.** Publish with a single insert/assign —
   after the last fallible step. Cache and "in progress" marker change in the
   same critical section.
4. **Roll back on drop, not on the error path.** A guard object whose `Drop`
   releases the reservation covers every early return, every `?`, and
   panics. Error-path-only cleanup is what leaves stale claims behind.
5. **Notify waiters from the same critical section** (condvar `notify_all`
   right after the state change), so a waiter can never observe the state
   change without the wakeup.

Sketch to apply to a hypothetical object pool:

```rust
pub struct PoolClaim { pool: Arc<Mutex<Pool>>, slot: usize, done: bool }
impl Drop for PoolClaim {
    fn drop(&mut self) {
        if !self.done {
            let mut p = self.pool.lock().unwrap();
            p.reserved.remove(&self.slot);   // rollback: reservation only
            p.cv.notify_all();
        }
    }
}
// claim: lock → reserve slot → unlock → build the object locally
//        → lock → insert(object) + release reservation + notify → done = true
```

Applied in this repo: `modules::claim_load` / `LoadClaim`
(`src/modules/mod.rs`), and the same shape in `sched::spawn_worker` (reserve
→ spawn → roll back the reservation on failure).

---

## 3. Type-tag validation before cached items execute

Validation now happens **before** any instruction of a cached artifact runs
(`validate_chunk` in `src/vm/bytecode.rs`, called by `deserialize_vmfunc` and
`spid3r exec`). Added in this audit:

| Check | What a crafted artifact could otherwise do |
|---|---|
| Name operands (`GetGlobal`, `SetGlobal`, `DefineGlobal`, `DefineConst`, `Import`, `FieldGet`, `FieldSet`) must be **string** constants | Run with a float/int as a global or field name; the VM rejects it mid-execution (safe but late), and the diagnostic points at run time instead of load time |
| `GetUpvalue` / `SetUpvalue` index < the prototype's declared upvalue count | Read a cell that does not exist — the VM yields `null`, a value the compiler never intended (silent wrong-value, the closest thing to type confusion in this design) |
| `SuperCtor` / `SuperMethod` parent-class and method operands: in range **and** strings | Execute with a non-string class/method reference |
| (pre-existing) `Constant` pool entries must be `Int`/`Float`/`Str`; `ImportAs`, `Construct`, `Invoke` name operands must be strings | — |

Validation is **recursive**: nested function prototypes, class methods, and
embedded module functions are each validated with their own prototype's
upvalue count (`validate_chunk_inner(chunk, upvalues, where)`), so a cached
class method cannot carry an unvalidated body.

Belt-and-braces at run time (already present, verified): every name operand
is read as `match ... { Value::Str(s) => ..., _ => self.fail(...) }`, upvalue
reads use `.get()` with a `Null` fallback, and the network handle table is a
typed enum with tag-checked lookups. A validator bug can therefore never turn
into memory unsafety — at worst a clean runtime error.

**Recipe for new cached items** (modules, closures, classes, prepared
statements, native handles):

1. Give the cached item an explicit **type tag** in its serialized form and
   in its in-memory representation (this codebase: `OpCode::from_u8` returns
   `Option`, the `Sock` enum, `Value`'s variant tags).
2. Validate **kinds, not just ranges**, for every operand the loader
   resolves, recursively into nested structures.
3. Reject unknown tags explicitly (`from_u8 → None`, never a `transmute`).
4. Keep the runtime defensive anyway: a tag check at the *use* site
   (`match`, `.get()`) converts a loader bug into an error instead of UB.
5. Never let a cache entry become visible before it is complete (§2).

---

## Verification

- 5 new unit tests in `src/vm/bytecode.rs`: non-string global name, non-string
  field name, out-of-range upvalue, non-string `SuperCtor`/`SuperMethod`
  operands, and a positive case proving valid `super` bytecode still loads.
- Full suite **669 passed, 0 failed**; 3-engine differential **25/25**
  (artifacts produced by `compile` still load and execute on all paths);
  clippy `-D warnings` clean; rustfmt clean.

## Files touched

| File | Change |
|---|---|
| `src/vm/bytecode.rs` | `validate_chunk_inner(chunk, upvalues, where)`; name-kind checks; upvalue range checks; `Super*` operand validation; 5 tests |
| `src/modules/mod.rs`, `src/vm/mod.rs` | (previous round) `claim_load` / `LoadClaim` — the transactional reference |
| `docs/BYTECODE.md` | validator section updated to list the new checks |
