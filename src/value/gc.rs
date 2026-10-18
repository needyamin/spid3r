//! Reference-cycle collection (see `docs/GC.md`).
//!
//! The language is reference-counted (`Arc<Mutex<_>>`), so acyclic garbage is
//! reclaimed eagerly, but a *cycle* never reaches ref-count 0 and leaks. This
//! module reclaims cycles with an **external-reference mark-sweep** that needs
//! no engine root enumeration:
//!
//! ```text
//! external(cell) = strong_count(cell) - internal_edges_into(cell)
//! roots          = cells with external > 0   (held from OUTSIDE the registry:
//!                  envs, Rust locals, globals, upvalue cells, task captures)
//! mark from roots via internal edges; sweep the unmarked (closed cycles).
//! ```
//!
//! Safety: any live `Value` held anywhere outside the tracked set is an external
//! strong reference, so its cell has `external > 0` and is never collected — a
//! transient Rust local during an in-progress expression included. An
//! UNDER-counting `gc_trace` only leaks (never frees live memory); only an
//! OVER-counting one could, so `gc_trace` counts each strong `Value` child once,
//! skips weak references, and skips container types it does not track.
//!
//! Auto-collection is gated behind `SPID3R_GC` (default off => byte-identical
//! behavior); `gc_collect()` can always be called explicitly. Tracking currently
//! covers `list`/`vec` (`VecData`) — the common self-cycle former; other
//! container types are simply untracked (safe: they just leak as before).

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::value::collections::{MapData, SetData, VecData};
use crate::value::{InstanceData, Value, VmInstanceData};

type TraceFn = fn(&Arc<dyn Any + Send + Sync>, &mut dyn FnMut(&Value));
type ClearFn = fn(&Arc<dyn Any + Send + Sync>);

struct Entry {
    /// Identity = the cell allocation's address (`Arc::as_ptr` of the cell).
    id: usize,
    weak: Weak<dyn Any + Send + Sync>,
    trace: TraceFn,
    clear: ClearFn,
}

static REGISTRY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// A container cell whose children can be enumerated (for marking) and cleared
/// (for sweeping). Implemented only for tracked types; anything else is left
/// untracked and simply leaks as before.
pub trait GcTrace {
    /// Call `f` once per strong `Value` child. Must NEVER over-count (a missed
    /// child only leaks; a spurious one could free live memory).
    fn gc_trace(&self, f: &mut dyn FnMut(&Value));
    /// Empty the cell's contents so the cycle's internal strong refs are dropped.
    fn gc_clear(&mut self);
}

impl GcTrace for VecData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        for v in &self.items {
            f(v);
        }
    }
    fn gc_clear(&mut self) {
        self.items.clear();
    }
}

impl GcTrace for SetData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        for v in &self.items {
            f(v);
        }
    }
    fn gc_clear(&mut self) {
        self.items.clear();
    }
}

impl GcTrace for MapData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        // Keys are `MapKey` (scalars), never containers, so only values matter.
        for (_, v) in &self.entries {
            f(v);
        }
    }
    fn gc_clear(&mut self) {
        // `index` holds no Values; it is dropped with the map.
        self.entries.clear();
    }
}

impl GcTrace for InstanceData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        for v in self.fields.values() {
            f(v);
        }
    }
    fn gc_clear(&mut self) {
        self.fields.clear();
    }
}

impl GcTrace for VmInstanceData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        for v in self.fields.values() {
            f(v);
        }
    }
    fn gc_clear(&mut self) {
        self.fields.clear();
    }
}

fn trace_erased<T: GcTrace + Send + 'static>(
    arc: &Arc<dyn Any + Send + Sync>,
    f: &mut dyn FnMut(&Value),
) {
    if let Some(m) = arc.downcast_ref::<Mutex<T>>() {
        if let Ok(g) = m.lock() {
            g.gc_trace(f);
        }
    }
}

fn clear_erased<T: GcTrace + Send + 'static>(arc: &Arc<dyn Any + Send + Sync>) {
    if let Some(m) = arc.downcast_ref::<Mutex<T>>() {
        if let Ok(mut g) = m.lock() {
            g.gc_clear();
        }
    }
}

/// Register a cell for cycle tracking and hand it back. Called at construction
/// sites (so the registry holds a *weak* handle; it never keeps a cell alive).
pub fn track<T: GcTrace + Send + 'static>(cell: Arc<Mutex<T>>) -> Arc<Mutex<T>> {
    let id = Arc::as_ptr(&cell) as *const () as usize;
    let erased: Arc<dyn Any + Send + Sync> = cell.clone();
    let weak = Arc::downgrade(&erased);
    drop(erased);
    let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    reg.push(Entry {
        id,
        weak,
        trace: trace_erased::<T>,
        clear: clear_erased::<T>,
    });
    // Safety valve: programs that never collect (e.g. the native runtime, which
    // has no auto-trigger) still keep the registry bounded — prune dead handles
    // once it grows, so it tracks live cells rather than all-time allocations.
    if reg.len() > 8192 {
        reg.retain(|e| e.weak.strong_count() > 0);
    }
    drop(reg);
    cell
}

/// The identity of a tracked cell held by `v`, if `v` is a tracked container.
fn value_cell_id(v: &Value) -> Option<usize> {
    match v {
        Value::List(a) | Value::VecDyn(a) => Some(Arc::as_ptr(a) as *const () as usize),
        Value::Map(a) => Some(Arc::as_ptr(a) as *const () as usize),
        Value::Set(a) => Some(Arc::as_ptr(a) as *const () as usize),
        Value::Instance(a) => Some(Arc::as_ptr(a) as *const () as usize),
        Value::VInstance(a) => Some(Arc::as_ptr(a) as *const () as usize),
        _ => None,
    }
}

/// Number of currently-live tracked cells (dead entries pruned first). Used by
/// the `gc_live_cells()` diagnostic and tests.
pub fn live_cells() -> usize {
    let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    reg.retain(|e| e.weak.strong_count() > 0);
    reg.len()
}

/// Run one collection pass; returns the number of cells swept. Safe to call at
/// any single-threaded point.
pub fn collect() -> usize {
    // Snapshot live entries (weak handles only — never upgrade during counting).
    let snapshot: Vec<(usize, Weak<dyn Any + Send + Sync>, TraceFn, ClearFn)> = {
        let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        reg.retain(|e| e.weak.strong_count() > 0);
        reg.iter()
            .map(|e| (e.id, e.weak.clone(), e.trace, e.clear))
            .collect()
    };
    let n = snapshot.len();
    if n == 0 {
        return 0;
    }

    // Strong counts, read before any upgrade (so our own temporary refs are not
    // counted).
    let strong: Vec<usize> = snapshot.iter().map(|(_, w, _, _)| w.strong_count()).collect();

    let mut index: HashMap<usize, usize> = HashMap::with_capacity(n);
    for (i, (id, _, _, _)) in snapshot.iter().enumerate() {
        index.insert(*id, i);
    }

    // Count internal edges and build the adjacency for marking.
    let mut internal = vec![0usize; n];
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); n];
    for i in 0..n {
        if let Some(arc) = snapshot[i].1.upgrade() {
            let mut kids: Vec<usize> = Vec::new();
            (snapshot[i].2)(&arc, &mut |v: &Value| {
                if let Some(cid) = value_cell_id(v) {
                    if let Some(&j) = index.get(&cid) {
                        internal[j] += 1;
                        kids.push(j);
                    }
                }
            });
            edges[i] = kids;
        }
    }

    // Roots = cells held from outside the tracked set (external > 0). Mark all
    // cells reachable from a root through internal edges.
    let mut marked = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..n {
        if strong[i] > internal[i] {
            marked[i] = true;
            stack.push(i);
        }
    }
    while let Some(i) = stack.pop() {
        for &j in &edges[i] {
            if !marked[j] {
                marked[j] = true;
                stack.push(j);
            }
        }
    }

    // Sweep: clear the contents of every unmarked cell (they are reachable only
    // from other unmarked cells — a closed, externally-unreferenced cycle).
    let mut freed = 0;
    for i in 0..n {
        if !marked[i] {
            if let Some(arc) = snapshot[i].1.upgrade() {
                (snapshot[i].3)(&arc);
                freed += 1;
            }
        }
    }
    drop(snapshot);

    // Prune the cells that just died.
    let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    reg.retain(|e| e.weak.strong_count() > 0);
    freed
}

static TICKS: AtomicUsize = AtomicUsize::new(0);

/// Is automatic collection enabled? **On by default**; `SPID3R_GC=0` disables it.
fn auto_enabled() -> bool {
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| std::env::var("SPID3R_GC").map(|v| v != "0").unwrap_or(true))
}

/// Called at interpreter statement boundaries. Collects every 4096 statements,
/// and only while the scheduler has no live tasks (a worker could clone a cell
/// concurrently, racing the strong counts).
pub fn maybe_collect() {
    if !auto_enabled() {
        return;
    }
    if TICKS.fetch_add(1, Ordering::Relaxed).is_multiple_of(4096)
        && crate::sched::Scheduler::shared().live_tasks() == 0
    {
        collect();
    }
}
