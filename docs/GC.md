# Reference-cycle collection — design

> Status: **v1.2 implemented** — **all container and instance types**
> (`list`/`vec`, `stack`, `queue`, `deque`, `linked_list`/`singly_linked_list`/`doubly_linked_list`,
> `set`, `map`, `tuple`, `binary_tree`, `bst`, `heap`, `priority_queue`, `graph`,
> `hash_table`, `trie`, `matrix`, `ring_buffer`, `bag`, `ref`, `option`/`result`,
> and class instances) are tracked and cycle-collected automatically through
> `value::new_ref`. `gc_collect()` / `gc_live_cells()` are builtins on **every**
> engine (tree, vm, native/AOT — see "The native backend" below for what they
> mean there); `gc_collect()` refuses to run only while another thread is
> executing Spid3r code (audit A-4); automatic collection at statement boundaries
> is **on by default** (`SPID3R_GC=0` disables it).

## The problem

The language is reference-counted (`Arc<Mutex<_>>`), so acyclic garbage is
reclaimed eagerly, but a *cycle* (two or more values referencing each other, or
a value that contains itself) never reaches ref-count 0 and leaks until the
process exits. `weak`/`deref` were the manual escape hatch; this module collects
cycles automatically/safely.

## Why the obvious approaches are wrong here

- **Native unwinding / `setjmp`** — irrelevant (this is a memory-management
  problem, not control flow).
- **Mark-sweep with an explicit root set** (walk the interpreter's environments,
  VM frame stacks, globals, upvalue cells, guards, scheduler values) — this is
  the textbook GC, but it is the **use-after-free hazard**: any live `Value` the
  collector fails to see (a transient in a Rust local mid-expression, a slot in
  a frame it didn't enumerate) gets swept. Missing one root = crash. Enumerating
  every root in *both* engines correctly is fragile and not verifiable cheaply.
- **Trial deletion (Bacon–Rajan)** — correct and root-free, but requires
  intercepting every `Arc`/`Rc` increment/decrement to maintain trial counts,
  i.e. replacing the whole ref-count layer. Too invasive.

## The safe algorithm: external-reference mark-sweep (no root enumeration)

The trick is to detect "held from outside the collectable set" **without**
enumerating roots, by comparing each cell's real strong count to the number of
references it receives from *other tracked cells*:

```
external(cell) = strong_count(cell) - internal_edges_into(cell)   (saturating at 0)
```

`internal_edges_into(cell)` is computed by tracing every tracked cell and
counting the strong `Value` edges that point at `cell`. Everything else that
holds a strong reference — an interpreter environment binding, a Rust stack
local, a global, a captured upvalue cell, a spawned task's closure env — is by
definition **outside** the tracked set, so it shows up as extra `strong_count`
and therefore as `external(cell) > 0`.

Then:

1. **Roots** = cells with `external > 0` (held from somewhere outside the set).
2. **Mark** every cell reachable from a root through internal edges.
3. **Sweep** the unmarked cells: they are reachable *only* from other unmarked
   cells — i.e. a closed cycle with no external reference — so they are garbage.
   Dropping them breaks the cycle and frees the whole SCC.

### Why this is safe (never frees live memory)

- A `Value` held anywhere outside the registry (env, local, global, upvalue,
  task capture, stack slot) is a strong reference, so its cell has
  `external > 0` → it is a root → marked → **never** collected. Transient Rust
  locals during an in-progress expression are exactly such external references.
- **Trace under-counting is safe; over-counting is not.** If `trace` misses an
  edge, `internal_edges` is *smaller*, so `external` is *larger*, so the target
  looks like a root and is kept → a leak, never a crash. If `trace` ever counts
  an edge that is not a real strong reference, `external` could be wrongly 0 and
  a live cell could be swept → UAF. **Therefore `trace` must never over-count:**
  count each strong `Value` child exactly once, never count `weak` references,
  and for any container whose children you are unsure about, simply do **not**
  trace it (its children then look like roots → safe leak).

### The one real hazard: concurrent mutation

`strong_count` must be read while nothing else clones/drops the tracked cells.
The interpreter and VM run a single thread per program, but `spawn` runs tasks on
the shared worker pool. **Gate collection on "no live/queued tasks"** (the
scheduler's live count is 0 and nothing is queued). When the program is
single-threaded at the safe point, the counts are stable and the algorithm is
sound. Programs that use `spawn` simply don't collect until they quiesce (a
documented, safe restriction — the leak persists while tasks run).

## Implementation plan

1. **Registry.** Add a per-process registry of weak handles to tracked cells,
   keyed by identity, each with a `trace` capability. Hook it at the single
   allocation choke point `value::new_ref` (and the `Arc::new` sites for
   `UpvalueCell`/task captures). Registration must not change value semantics or
   measurable performance of the hot path (a `Vec<Weak<..>>` push per container
   allocation is acceptable; measure with `spid3r bench`).
2. **Trace.** A `trace(&self, f: &mut impl FnMut(&Value))` for every container
   `*Data` that enumerates its strong child `Value`s: list/vec/deque/stack/
   queue/linked lists (items), map (values; keys are scalar `MapKey`s and are
   not traced), set, trees/bst/trie/graph/
   heap/prio-queue/matrix/ring/bag, tuple, option/result, instance/`VInstance`
   (fields), class/closure (upvalues). Start with the common cycle-formers
   (list, instance, map) and leave the rest untraced (safe) — then widen.
3. **Collector.** Build `internal_edges` by tracing all cells; compute
   `external = strong − internal`; mark from roots; drop unmarked cells (take
   them out of the registry and let their `Arc`s drop).
4. **Trigger + gating.** Run at safe points — the interpreter's statement loop
   and the VM's opcode dispatch boundary — only when `scheduler.live == 0`.
   Throttle (e.g. every N allocations, or when the registry has grown past a
   watermark) so it is cheap when there is nothing to collect.
5. **Rollout.** Land behind `SPID3R_GC` (default off → byte-identical behavior
   and zero risk), prove it on the suite + a stress test, then enable by default.

## Test strategy (the red/green target)

- **Self-cycle reclaimed:** build `a = list(); a.push(a)` in a loop (top-level
  and inside `main`), drop each, and assert memory stops growing across many
  iterations (via a live-cell counter builtin and/or process RSS). Same for an
  instance whose field points at itself, and a two-node mutual cycle.
- **Safety:** a stress test that churns many cycles + live acyclic values and
  asserts outputs are unchanged and nothing crashes (run under the full suite).
- **Parity:** `spid3r test` (tree == vm == exec) must stay 23/23; the gate 9/9.
- A `gc_live_cells()` diagnostic builtin (returns the registry size) makes the
  reclamation assertion precise and portable, and doubles as a leak probe.

## Current status

**v1.2 implemented** (`src/value/gc.rs`): external-reference mark-sweep with
`GcTrace` implemented for **all 20+ container and instance types** (hooked
automatically inside `value::new_ref`, plus `Option`/`Result` inline box
peeling and `ListData` drop-time node-cycle breaking). Builtins `gc_collect()` /
`gc_live_cells()` work on both engines. Auto-collection at interpreter statement
boundaries is **on by default** (`SPID3R_GC=0` disables it), throttled every 4096
statements and skipped while another thread is running Spid3r code.
**`gc_collect()` uses the same precise gate**: a mark-sweep reads each cell's
strong count and then walks its children, so it is race-free exactly when no
*other* thread is executing (`sched::gc_quiescent`, which counts threads that are
not parked in `recv`/`join`/`mutex.lock`/`sleep`). Tasks parked in a blocking
wait hold their values on their own stacks — those are external references, i.e.
roots — so they do not block a collection and do not get swept. While another
thread *is* running, `gc_collect()` fails with a clean
`gc_collect: N other thread(s) are running; join or finish them before
collecting` error instead of corrupting memory (audit A-4). A safety valve prunes
dead and duplicate handles so the registry stays bounded even where collection
never runs (the native runtime).

On the native backend the same builtins report engine-internal numbers and
`gc_collect()` reclaims nothing by construction — that is the documented arena
trade, not a collector bug (see "The native backend" above and the
ENGINE_CONTRACT exemption #4).

Verified: all 23 examples produce byte-identical output with the collector on vs
off (it never frees live data); `tests/cycle_tests.rs` proves cycles across all
container and instance types are reclaimed while live cycles survive.



## Closures and environments ARE collected (audit A-13, implemented 2026-10-21)

> Historical note: the text below described the leak while it was open. The
> fix landed exactly along the recorded design; the "why" paragraph is kept
> because it explains the shape of the implementation.

The collector tracks value containers and instances. A **closure's environment**
(`Env = Arc<Mutex<EnvironmentData>>`, holding `vars: HashMap<String, Value>` and
`parent: Option<Env>`) is not a registered cell, and neither is
`FunctionData`/`VmFunc`/`ClassData`. A cycle through them therefore leaks.

Measured on the current tree — the leak is one cell per closure, not a slow
drift:

```spid3r
fn make() {
    let data = list();      // a tracked cell
    data.push(1);
    let f = fn() { return data; };   // f is bound in the env it captures
    return f;                        // env -> f -> env: a cycle
}
fn main() {
    let i = 0;
    while (i < 20000) { let f = make(); i = i + 1; }
    println(gc_live_cells());        // 20000, not 0
}
```

Every nested function and every closure that is bound to a name in its own
captured scope has this shape, so a long-running program that creates closures in
a loop grows without bound. `tests/closure_cycle_leak.rs` pins the current
number so a future fix is visible.

**Why registering environments alone does not fix it.** The collector's root rule
is `external = strong_count − internal_edges`. With only envs registered, the
single strong reference to an env comes from an *untraced* `FunctionData`, so
`internal_edges == 0` and `external == 1` — the env looks like a root and is
kept. Registering only `FunctionData` has the mirror problem. Both ends of the
cycle must be cells, which means:

1. Register `EnvironmentData` (one place: `environment::child`, plus
   `new_global`), tracing `vars` values and `parent`.
2. Register `FunctionData` (3 construction sites), tracing `env`.
3. Register `ClassData`/`VmClass` (their `methods` map holds `FunctionData`, so a
   class→method→env path must be traversable) and `VmFunc`.
4. Keep registration *complete*: a container holding a `Value::Function` whose
   cell is unregistered cannot be traversed, so an env reachable only through it
   would be swept while still live. This is the one failure mode that is a
   use-after-free rather than a leak, and it is why the change needs the fields
   made private (or a `new_closure`/`new_class` constructor) so a future
   construction site cannot silently skip registration.

Until then the workaround is explicit: break the cycle with `weak()`/`deref()`
(the manual escape hatch) or hoist the closure out of the scope it captures.

**Implemented as designed.** The collector gained *cell-level* edges
(`GcTrace::gc_trace_cells`, called with the registry's id callback in addition
to the value callback), and three kinds of cells were registered:

1. `EnvironmentData` (`environment::new_global` / `child` / `child_with_vars` —
   the only construction sites): `gc_trace` yields the vars' values,
   `gc_trace_cells` the parent environment, `gc_clear` empties `vars`.
2. `FunctionData` / `ClassData` — **immutable** cells (no field is ever
   written after construction), registered through `value::new_function` /
   `value::new_class` via `gc::track_immutable`, which traces without a lock
   and never clears (their reclamation happens through ordinary `Arc`
   refcounts once the cycle's mutable side is cleared). The single-constructor
   rule is what makes the registration complete: a future construction site
   cannot silently skip it.
3. `for_each_cell_id` maps `Value::Function` and `Value::Class` to their cell
   ids, so the `env -> closure` edge is visible from the environment's side
   (the mirror `closure -> env` edge comes from `FunctionData::gc_trace_cells`).

Both directions of `env -> closure -> env` are therefore registry edges: the
cycle is unmarked (no external refs) and swept from the environment's side.
`tests/closure_cycle_leak.rs` — which used to pin the leak — now asserts
reclamation, including the safety half (captured containers survive
collections and keep working). The VM needs none of this: its closures carry
upvalue *cells* (untracked external refs, no env cycle).

## The native backend: arena model and GC observability (policy, decided 2026-10-25)

The native AOT backend has **no reclamation at all** — by design
(`docs/NATIVE.md`, "Memory model"): every value handle is a `Box<Value>`
leaked for the whole process (`rt.rs::leak`). This interacts with the
collector in a precise, predictable way:

- Native cells **are tracked** in the same registry (they are built by the
  same `value::collections` constructors), so `gc_live_cells()` on native
  reports roughly the **all-time** tracked-cell count, not the live set
  (e.g. the field-test churn workload: tree 380 / vm 159 / native 300 462 —
  `report.html` F-3).
- `gc_collect()` on native is a **safe no-op by construction**: every leaked
  handle's inner `Arc` is a strong reference from outside the tracked set, so
  *every* cell has `external > 0`, every cell is a root, nothing is ever
  unmarked. This is the collector's safety guarantee working exactly as
  designed — it can never free live memory, and on native nothing looks dead.
  It completes, returns `int` (0 swept), and all live data survives.

**Policy (ENGINE_CONTRACT exemption #4):** `gc_live_cells()` and the swept
count are **engine-internal memory observability** and are *excluded* from the
byte-identical-parity contract. The portable subset every engine guarantees —
and the parity bar for programs using these builtins — is: both builtins
exist, return `int`, `gc_collect()` completes safely, and live data always
survives a collection. `tests/gc_portability.rs` pins this on all three
engines.

**Why mid-run reclamation on native is future work, not a flag.** Reclaiming
cycles on native requires the *handles* (the leaked `Box<Value>`s) to be
freed when they die — i.e. last-use handle-lifetime management in the LLVM
codegen (free on scope exit, on local overwrite, and for expression
temporaries). Without that, no collector change can help: the handles root
everything. With it, the existing shared collector would start reclaiming on
native unchanged (external-reference mark-sweep needs no root enumeration).
Until then, long-running native processes keep acyclic garbage bounded only
by the stream-processing shape (constant live set, per-item garbage is
allocation-only) and leak cycles until process exit — the documented arena
trade.
