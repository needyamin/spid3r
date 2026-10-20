# 03 — Runtime Core Static Assessment (interpreter + environments + values + cycle GC)

> **Status 2026-10-21 (verification round).** This file is the 2026-10-20
> static record and is not rewritten. What changed since:
>
> - **Fixed:** **F1** (`gc_collect()` collected concurrently with live tasks — audit A-4;
>   the gate is now `sched::gc_quiescent()`, and a probe confirms it refuses while another
>   thread runs) and **F2** (thrown values were degraded to strings across task/callback
>   seams — audit A-9; a thrown map now reaches `catch` intact).
> - **Still open:** **F3** (closure/environment cycles leak — audit A-13; the collector
>   still only tracks value containers and instances), **F4** (`deep_clone` of a cyclic
>   value aliases the original — audit A-14, **reproduced**: mutating through the clone's
>   inner element changed the original), **F5** (poisoned-mutex panic cascade), F6
>   (`is_callable`), F7 (assorted smalls).
>
> Full status table: `assessment/AUDIT_2026-10-20.md` §9. Reproduce the
> dynamic checks with `python audit_notes/probes/verify_2026-10-21.py`.


Scope owned: `src/interpreter/mod.rs`, `src/environment/mod.rs`, `src/value/mod.rs`, `src/value/gc.rs`.
Method: static read of all 3,378 lines of owned code; sibling files (`value/collections/`, `vm/`, `semantic/`, `stdlib/`, `sched/`) read only where a one-line cross-check was needed for an owned-code claim (cited as such). No build/run performed (forbidden). Docs' claims (README trial-deletion, "identical engine output") were treated as unverified inputs and checked against code.
Date: 2026-10-20.

---

## 1. Conclusion summary

The runtime core is **structurally sound on the axes it was most suspected of failing**: the call-depth guard covers every interpreter recursion path, integer arithmetic is explicitly wrapping (no overflow panics), and the cycle collector's mark-sweep core fails safe (under-counting can only leak, never free live data) — **high confidence**, all from first-party code.

The real risks sit at the edges: an explicit `gc_collect()` bypasses the concurrency guard that protects auto-collection and can wipe live containers while tasks run (F1, highest severity); thrown values are silently degraded to display strings across task/callback boundaries so `try/catch` cannot recover structured errors (F2); closure/environment cycles leak by design gap (F3); `deep_clone` silently aliases cyclic originals into the "copy" (F4); and a poisoned-mutex panic cascade is uncatchable by user code (F5).

Overall confidence: **high** for all findings — every one is a complete code-path read inside owned files; none rests on hearsay or docs. Execution was not attempted (forbidden), so findings marked "not executed" carry the residual risk that a runtime detail outside the read path changes reachability.

---

## 2. Findings (ranked by severity × confidence × user impact)

### F1 · HIGH severity · HIGH confidence — `gc_collect()` collects concurrently with live tasks and can erase live containers

- **Facts.** Auto-collection is explicitly guarded against concurrency: `src/value/gc.rs:570-576` — `maybe_collect` fires only when `crate::sched::Scheduler::shared().live_tasks() == 0`, and the comment (gc.rs:568-570) states the reason: *"a worker could clone a cell concurrently, racing the strong counts."* The user-callable builtin has **no such guard**: `src/stdlib/mod.rs:933-935` — `fn n_gc_collect(...) { Ok(Value::Int(crate::value::gc::collect() as i64)) }` (stdlib read as a one-line cross-check for the wiring; the unguarded entry point itself is `gc.rs::collect`, gc.rs:454).
- **Mechanism (all lines in owned code).** `collect()` snapshots strong counts once (gc.rs:473: `let strong: Vec<usize> = snapshot.iter().map(|(_, w, _, _)| w.strong_count()).collect();`), then traces children later (gc.rs:480-495), computes `external = strong − internal` and roots as `strong[i] > internal[i]` (gc.rs:498-503), and sweeps unmarked cells by clearing their contents (gc.rs:517-526). If a task thread clones a `Value` into cell X between the snapshot and the trace, the snapshot under-counts X's external refs → X is classified as garbage even though a task now holds it → sweep clears X's contents while the task's clone still references the cell. Result is not memory-unsafety (the `Arc` survives) but **silent logical corruption: a live list/map/instance the task is using is emptied under it**. The symmetric race (a task dropping a ref mid-collect) only over-counts externals → missed collection (leak), which is the safe direction.
- **Evidence tier:** first-party code (gc.rs + wiring). The concurrency premise is established by the code's own comment (gc.rs:568-570) and the worker pool that motivates it (`sched/mod.rs:183-193`, workers = `available_parallelism`, each running jobs that construct `Interp::shared` and share globals — `interpreter/mod.rs:611-624`).
- **Trigger:** a Spid3r program that spawns tasks (or awaits pending ones) and calls `gc_collect()` — the builtin is documented API ("`gc_collect()` can always be called explicitly", gc.rs:31-32).
- **Fix direction:** apply the same `live_tasks() == 0` gate (or a GC-wide "quiescence" flag) inside `collect()` itself, so the builtin inherits the guard instead of bypassing it.

### F2 · HIGH severity (semantic) · HIGH confidence — thrown values are degraded to strings across task and callback boundaries; `try/catch` cannot recover them

- **Facts.** A user `throw` produces `Flow::Error` with `d.thrown = Some(value)` (`src/interpreter/mod.rs:672-678`). But three boundaries stringify it:
  1. Task spawn — `interpreter/mod.rs:617-620` (Stmt::Spawn) and `:909-916` (Expr::Spawn): `Err(Flow::Error(d)) => Err(match &d.thrown { Some(v) => crate::value::display_value(v), None => d.message.clone() })`.
  2. Task join — `interpreter/mod.rs:922-927` (Expr::Await): `t.join().map_err(|msg| self.rt(span, format!("task failed: {}", msg)))` — the string is re-wrapped as a plain diagnostic; `.thrown` is never restored.
  3. Collection callbacks — `interpreter/mod.rs:82-93` (`InterpCallbackHost::call_callback`): identical stringification for `list.map(f)` / `filter` / `reduce` etc.
- **User impact.** `try { await t } catch(e) { e.code }` binds a **string** (the rendered display form), not the thrown object; structured error handling across `spawn`/`await` and higher-order collection methods is impossible. Additionally, `break`/`continue` inside a callback becomes `Err("invalid control flow in a callback")` (interpreter/mod.rs:91) — a `break` intended for an enclosing loop surfaces as a generic runtime error instead of propagating (defensible for lambdas, but it *is* observable and undocumented).
- **Evidence tier:** first-party code; complete path read (throw → spawn → join → catch). Note the code's own comment at interpreter/mod.rs:614-615 shows awareness ("Surface the user-thrown value across the task boundary") — the surfacing stops at `display_value`.
- **Fix direction:** carry `thrown: Option<Value>` through `TaskHandle::join`'s error channel instead of `String`.

### F3 · MEDIUM severity · HIGH confidence — closure/environment cycles leak unconditionally; the collector cannot see them

- **Facts.** The registry tracks only container/instance cells: gc.rs:23-28 — *"Tracking covers every container and instance type … (20+ `*Data` types below); **closures/environments are not tracked yet (cycles through them leak** — see GC.md)."* Function and env allocations confirm it: `interpreter/mod.rs:316-321` (Stmt::Fn) and `:712-716` (Expr::Fn) build `Arc::new(FunctionData { proto, env })` directly (not `new_ref`/`track`), and `environment/mod.rs:20-23` defines `Env = Arc<Mutex<EnvironmentData>>` plain.
- **Consequence.** The most idiomatic cycle — `f = fn() { f }` (function value stored in the very environment it captures), or a closure stored in an instance field it captures — forms env → `FunctionData` → env. `for_each_cell_id` (gc.rs:409-446) has no function/env arms, so these cells are invisible to marking **even when the user calls `gc_collect()` explicitly**. In a long-running REPL, embedded host, or task-heavy program this is unbounded growth; `SPID3R_GC` and `gc_live_cells()` give the user no signal, since the leaked cells are never registered.
- **Evidence tier:** first-party code (admitted in gc.rs header + verified at both allocation sites). Not executed.
- **Related (same root cause):** instance→field→closure→captured-env→instance cycles (methods creating closures over `self` and storing them on `self`) leak the instance too, even though instances *are* tracked — the closure side of the cycle is invisible, so the instance looks root-held only via the untracked cycle; the mark phase still frees it only if external refs hit 0, which the invisible closure edge prevents… precisely because the edge is uncounted *in the wrong direction*: the closure's env holds the instance strongly, but since the closure cell is not in the registry, the instance's `external` count includes that env reference → instance looks externally rooted → never collected while the closure cycle lives. Either way: leak.
- **Fix direction:** track envs (or env-backed function cells) with a `GcTrace` that walks `vars` + `parent`, as GC.md apparently already plans.

### F4 · MEDIUM severity · HIGH confidence — `deep_clone` of a cyclic structure silently aliases the **original** into the "copy"

- **Facts.** `src/value/mod.rs:1153-1162`:
  ```rust
  pub fn deep_clone(v: &Value) -> Value { ... }
  fn deep_clone_inner(v: &Value, seen: &mut HashSet<u64>) -> Value {
      if let Some(id) = ref_id(v) {
          if !seen.insert(id) {
              return v.clone();          // value/mod.rs:1162 — returns the ORIGINAL
          }
      }
  ```
  The cycle-detection shortcut returns the *source* value on revisit instead of linking to the *cloned* counterpart (there is no two-pass/fix-up construction). `clone()` routes here (`value/collections/mod.rs:440` — cross-checked one line).
- **Repro chain (not executed):** `a = []; a.push(a); b = clone(a);` → `b` is a new list whose element 0 **is** `a`. `b[0].push(1)` mutates `a`. This directly contradicts the function's own contract (value/mod.rs:1151: *"reference types are rebuilt so **mutating the clone never touches the original**"*).
- **Evidence tier:** first-party code; mechanism fully contained in the read path. Confidence high on semantics; only execution could add counter-evidence and none is expected (the alias path is unconditional on revisit).
- **Fix direction:** two-phase clone — pre-bind ids to freshly allocated cells, then fill.

### F5 · MEDIUM severity · MEDIUM-HIGH confidence — poisoned-mutex panic cascade is uncatchable by `try/catch` and discards the panic payload

- **Facts.** Env and container access `.lock().unwrap()` pervasively: `src/environment/mod.rs:54, 69, 76, 80, 96, 100, 105, 113, 122`; `src/value/mod.rs:396, 454` (weak paths) and throughout `values_equal` / `display_value` / `deep_clone` (e.g. 847, 920-1259); `src/interpreter/mod.rs:287, 1081, 1092, 1108, 1169, 1178, 1367, 1407`. By contrast, the GC deliberately recovers from poisoning: gc.rs:395, 444, 460 use `unwrap_or_else(|e| e.into_inner())`.
- **Mechanism.** (i) Any panic while a lock is held (e.g. a native fn panicking under a container guard reached via a higher-order method) poisons that mutex; (ii) every subsequent operation on that value panics; (iii) `Stmt::Try` (interpreter/mod.rs:628-666) catches only `Flow::Error` — Rust panics are not `Flow`, so user `try/catch` cannot intercept; (iv) the panic is caught only at the program-thread boundary and reduced to a generic message, **discarding the payload**: `interpreter/mod.rs:194-199` — `Err(_) => Err(Diagnostic::new(Phase::Runtime, "the program thread panicked; this is a bug in spid3r"))`.
- **Evidence tier:** first-party code, mechanism verified line-by-line; the *trigger* (first panic under lock) depends on sibling code (native/stdlib bugs), hence MEDIUM-HIGH not HIGH confidence on real-world frequency.
- **Fix direction:** adopt the gc.rs pattern (`into_inner`) for value/env locks, and include `panic.payload_downcast::<&str/String>()` in the run-thread diagnostic.

### F6 · LOW-MEDIUM severity · HIGH confidence — `is_callable` accepts VFunc/Cfn/CClass that the interpreter cannot actually call (engine-interop seam)

- **Facts.** `src/value/mod.rs:263-273` — `is_callable` includes `VFunc`, `Cfn`, `CClass`. But `call_value` (`interpreter/mod.rs:1129-1150`) matches only `Function | Native | NativeClosure | Class`, else `"{type} is not callable"` (interpreter/mod.rs:1146-1150). `Expr::New` (interpreter/mod.rs:846-861) likewise only accepts `Value::Class`.
- **Impact.** Any path that moves a VM- or LLVM-produced value into the tree interpreter (embed API, module state, a mixed-engine host) passes a type-check that then fails at the call site — the "identical output" engine claim is not what breaks; *cross-engine composition* does. Low practical impact if engines are strictly exclusive (which the current entry points suggest), hence LOW-MEDIUM.
- **Evidence tier:** first-party code, both sides owned. Trigger paths (`vm/`, `native/`, `embed.rs`) out of scope — flagged, not audited.

### F7 · LOW severity · HIGH confidence — assorted confirmed smalls

1. **README ≠ implementation on GC algorithm.** README (established fact) claims a *trial-deletion* collector; the code is an **external-reference mark-sweep** — gc.rs:5-16 (module doc: "reclaims cycles with an **external-reference mark-sweep**"), gc.rs:454 (`collect`). No runtime impact; a doc/intent-vs-code conflict recorded per evidence rules.
2. **In-code comment inverted.** `interpreter/mod.rs:311-312`: "no-op unless SPID3R_GC" — actually collection is **on by default** and `SPID3R_GC=0` disables it (gc.rs:557-561, `auto_enabled`). Misleads future maintainers at the exact spot auto-GC is wired.
3. **Top-level statements never trigger auto-collection.** `run_program_inner`'s loop (interpreter/mod.rs:217-224) calls `exec_stmt` directly; `maybe_collect()` fires only inside `exec_block` (interpreter/mod.rs:313). A garbage-heavy top-level phase delays all collection until the first nested block statement. Perf/coverage gap, not soundness.
4. **Exit-code truncation.** `interpreter/mod.rs:252`: `Value::Int(i) => i as i32` — `main` returning `4294967297` silently yields exit code 1 (truncation, not an error).
5. **`hash_value` catch-all collapses many types.** `value/mod.rs:519-601` — every type without an explicit arm (`Option`, `Result`, `Trie`, `Matrix`, `Bag`, `Ring`, `RefBox`, `Weak`, `Task`, `Channel`, `MutexVal`, `Cfn`, `CClass`, `CInstance`, `Heap`, `PrioQueue`, `BinTree`, `Bst`, `HashTable`) hashes to the single constant `255u8` (value/mod.rs:600). Equality disambiguates, so this is collisions/perf only wherever hash buckets back a keyed structure — flagged for the `hash_table` path as a perf suspicion, correctness unaffected on the read path.
6. **Duplicated spawn blocks.** Stmt::Spawn (interpreter/mod.rs:592-626) and Expr::Spawn (:880-918) are copy-paste duplicates including the F2 bug — fixing F2 requires both sites (or one should delegate).

---

## 3. What was verified SOUND (with the check that earned it)

1. **MAX_CALL_DEPTH covers all interpreter recursion paths — SOUND (high).**
   Plain calls: `interpreter/mod.rs:1155-1172` (`fetch_add` → `>= MAX_CALL_DEPTH` → clean `Err`, balanced `fetch_sub` on all paths). Constructors: `interpreter/mod.rs:1325-1329` (checked before pushing the ctor frame; push/pop balanced at 1330/1343-1345). Instance methods: `interpreter/mod.rs:1388-1392` (balanced at 1395/1407-1410). Closures: `Expr::Fn` values are `Value::Function` and call through `call_value → call_function` (1129-1131) — same guard. Native→Spid3r re-entry: higher-order collection methods call back through `InterpCallbackHost::call_callback → call_value → call_function` (interpreter/mod.rs:83-89) — counted. Native-only calls (Value::Native at 1132-1138) don't recurse into the interpreter, and pure-Rust recursion in stdlib is outside this scope. Per-task depth counters are fresh (`Interp::shared`, interpreter/mod.rs:139-153) but each task has its own 32 MB worker stack sized for the same guard (`sched/mod.rs:165-168`, `WORKER_STACK_BYTES = 32MB`, applied at 190-193); the main program runs on 64 MB (interpreter/mod.rs:22, 186-192). Residual: a panic between `fetch_add`/`fetch_sub` leaks one depth unit per Interp, but that Interp's thread is dying anyway (main) or transient (task) — not exploitable.
2. **Arithmetic overflow is wrapping, never panicking — SOUND (high), by explicit choice.** `wrapping_add/sub/mul/div/rem` at interpreter/mod.rs:957, 977, 991, 1006, 1022; `wrapping_neg` at 714. Division/modulo by zero (int and float) are clean runtime errors (1004-1005, 1020-1021). So: no overflow panics; but overflow is *silent wraparound*, not checked — a language-semantics decision to document, not a bug.
3. **GC core, single-threaded auto path — SOUND (high).**
   - *Fail-safe direction of the tracer:* an under-counting `gc_trace` inflates `external` → cell looks rooted → leaks, never frees (gc.rs:498-503 + header rationale gc.rs:17-21). All skip paths are under-count paths: poisoned/absent downcast in `trace_erased` (gc.rs:358-367), early `break` on poisoned node lock in `ListData::gc_trace` (gc.rs:~171).
   - *No over-count found:* each `GcTrace` impl visits each strong child exactly once (verified impl-by-impl for all 24 container/instance types, gc.rs:47-356; bag counts are `(Value, n)` pairs — one strong ref per distinct value); weak references are excluded from edges (`Value::Weak` unmatched in `for_each_cell_id`, gc.rs:409-446); `Option`/`Result` are peeled so inline values attribute their edge to the owning tracked cell (gc.rs:437-438).
   - *Registry identity:* address-reuse across dead/live entries is handled because the retain predicate short-circuits — a dead handle fails `strong_count() > 0` before `seen.insert(e.id)` runs (gc.rs:398-401, 448, 460, 533), so a live cell at a reused address is never dropped from the registry (which could only have leaked, anyway).
   - *No dangling references / no reentrancy:* this is mark-sweep-by-clearing, not refcount-mutating trial deletion: the sweeper holds an upgraded `Arc` while clearing (gc.rs:520-526), a cell cannot vanish mid-clear; `gc_clear` only empties contents (drops Arcs) — no user code, no natives, no `Drop` logic runs during `collect`, so there is no reentrancy vector; cells that die mid-sweep are skipped via `upgrade()` returning `None` (gc.rs:521-524).
   - *Bookkeeping:* saturating arithmetic throughout (gc.rs:401, 536-540); `SPID3R_GC=0` disables only auto-collect, explicit `gc_collect()` still works (gc.rs:557-576) — matches the documented contract.
   - *Weak/deref hazards:* `make_weak` restricted to tracked cell kinds (value/mod.rs:337-383); `deref` after target death returns `None` (value/mod.rs:396-399) rather than a dangling strong value; weak cells are untracked so holding a weak never roots a cell. The only weak-related hazard is the concurrency one already covered by F1.
4. **try/catch/finally core flow — SOUND (high) on the paths asked about.** `finally` overriding in-flight `Return`/`Break`/`Continue`/`Error` is deliberate and mainstream-consistent: `interpreter/mod.rs:656-664` (`if !matches!(fr, Flow::Value(_)) { result = fr; }`); a `return` under `finally` correctly discards the body's return, and `break`/`continue` emerging from `try` reach the enclosing loop (`Flow::Break` handled at 473-481, 505-513, 551-553) or become the documented "break outside a loop" error at function boundaries (1226, 1347, 1411). Mutex-guard release is on every exit path of try/catch/finally (635, 637, 649, 660). `catch` binding covers internal errors (thrown=None → message string, 642-645). The genuine holes are the cross-boundary ones in F2, not the local control flow.
5. **Environment chains — SOUND (high).** Parent links are created strictly toward older environments (environment/mod.rs:29-39) → acyclic → the lock-per-level walk (41-47) cannot loop; values are cloned out before user code runs, so no lock is held across evaluation (environment/mod.rs:41-47); `const` enforcement on assignment (55-74); `set_global` walks to root without holding two locks at once (76-82).

---

## 4. 待证项 (open leads, not findings)

- **Whether any non-task thread can hold `Value` clones** (GUI bridge, REPL worker, embed host) — if yes, even the auto-collect guard (`live_tasks() == 0`) is insufficient, upgrading F1 to affect auto-GC too. Check `src/gui/bridge.rs` and `src/repl/mod.rs` for `Value` shipment across threads.
- **`GraphData.adj` representation** (`Vec<Vec<usize>>` vs strong node refs): if it holds strong `Arc`s to node cells *not* wrapped as `Value`, graph internal edges are under-counted — leak-direction only, but it would make graph-heavy cycle garbage immortal. One look at `src/value/collections/misc.rs` settles it (sibling-owned; not audited here).
- **Whether `Cfn` values can actually cross into the interpreter** in shipping configurations (embed/mixed engine) — determines whether F6 is latent or reachable. Owner: embed/vm.
- **`VInstance`/`CInstance` under `VClass`/`CClass` dispatch in the VM/LLVM engines** — same seam as F6 on the other side; engine owners should confirm parity.

## 5. Suggested next steps (highest value first)

1. Fix F1: move the `live_tasks()` quiescence check (or an atomic "collecting" flag) into `collect()` itself.
2. Fix F2: carry structured `thrown: Option<Value>` through `TaskHandle`'s error channel; deduplicate the two Spawn sites while there.
3. Decide the F4 contract: either document clone-on-cycles as "aliases at cycle points" or implement two-phase deep clone.
4. Track envs/functions in the GC (F3) per GC.md's existing plan; until then, expose the gap in `gc_live_cells()` diagnostics so users can see untracked growth.
5. Sweep `.lock().unwrap()` → `into_inner` recovery in value/environment/interpreter (F5) and capture the panic payload at interpreter/mod.rs:194-199.
