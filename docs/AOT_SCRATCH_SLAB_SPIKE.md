# Phase R4.1 — AOT Scratch-Slab Design Spike

- **Status:** Spike complete — **concludes infeasible without DP-1 (`Value v2`)**
- **Date:** 2026-11-18
- **Scope:** Evaluate whether trampoline-allocated `*mut Value` boxes in `src/native/rt.rs` can be safely reclaimed via a scratch slab (per-call or statement-local epoch) without changing the `*mut Value` ABI.

## 1. Current AOT Allocation Model

Every boxed AOT value is a `*mut Value` allocated either in:
- Static caches (`SMALL_INT_SLOTS` for `-1..=1023`, `TRUE_VAL`, `FALSE_VAL`, `NULL_VAL` in `src/native/rt.rs`), or
- `VALUE_SLAB` (`SlabCursor::alloc` in `src/native/rt.rs`), which bump-allocates into thread-local `Box::leak`-ed `4096`-slot slabs (`ARENA_SLAB_LEN = 4096`).

In R4.0, per-call allocation accounting (`CALL_SLAB_ALLOCS` / `CALL_ALLOC_STACK` around `enter_call()` / `leave_call()`) was added and exposed via `spid3rinfo()["AOT Memory"]`.

## 2. Escape Paths That Break Scratch-Slab Reclamation

Every trampoline returns an opaque `*mut Value` with `'static` lifetime assumptions in generated LLVM IR (`src/native/ir.rs`):

1. **Callee return values:** Every compiled function (`spid3r_fn.*`), closure (`spid3r_make_fn`), method (`spid3r_method`, `spid3r_method_slot`), and native (`spid3r_native`) returns `*mut Value` allocated *inside* the callee. Resetting a scratch slab on `leave_call()` would immediately dangle the return value.
2. **Closure environments (`env: *mut c_void`):** `spid3r_make_fn` (`src/native/rt.rs`) captures raw `*mut Value` pointers into a heap `env` block. Closures can outlive the creating frame, be returned, stored in globals/containers, or sent across threads via `spid3r_spawn` (`SendCfn`).
3. **Structured exception propagation (`LAST_THROWN`):** `spid3r_throw` / `spid3r_take_thrown` stores a `*mut Value` in thread-local/task exception state that unwinds across multiple call frames until a `try/catch` or `spid3r_await` consumes it.
4. **Cross-task sharing (`spid3r_spawn`):** Spawned tasks execute compiled closures concurrently on `Scheduler::shared()`. Any `*mut Value` captured in `env` or stored in `globals()` can be dereferenced on another thread after the creating thread's call/epoch ends.

## 3. Why a Restricted Statement-Local Subset Also Fails the Cost/Benefit Bar

Consider restricting a scratch slab strictly to leaf conversions (e.g., `spid3r_value_from_cstr`, `spid3r_value_float`, `spid3r_binop` intermediates) consumed within the same statement:

- **No static non-escape guarantee in current IR:** `src/native/ir.rs` lowers expressions to uniform `*mut Value` SSA values without escape/liveness metadata. Any expression result can flow into `let` bindings, `return`, `throw`, closure capture `env`, `globals()`, or `list`/`map` storage.
- **Copy-on-escape defeats the win:** Ensuring a scratch-slab pointer never escapes requires runtime pointer-range checks and cloning into the permanent slab at every potential escape sink (`return`, `throw`, `spid3r_make_fn`, `spid3r_global_set`, `spid3r_construct`, `spid3r_method`, `spid3r_native`).
- **Hot-path integers are already unboxed:** `int_locals` and `top_ints` in `src/native/ir.rs` (and `SMALL_INT_SLOTS` in `src/native/rt.rs`) already avoid `VALUE_SLAB` allocations for unboxed `i64` arithmetic and small constants.

## 4. Kill-Criteria Verdict

Per the R4.1 kill criteria (*"any case where a scratch-slab box can be observed after reset ⇒ spike concludes infeasible, recorded honestly; bounded memory joins DP-1's expected payoff"*):

- **Verdict:** **Infeasible without DP-1 (`Value v2`).**
- **Recommendation:** Keep `VALUE_SLAB` `'static` leak semantics for `*mut Value` until **DP-1** replaces `*mut Value` with a 64-bit tagged/NaN-boxed value representation (eliminating heap boxing for scalars and enabling RC/GC tracing for heap objects). Measure and track the current leak rate in **R4.2 (`rss_over_time`)**.
