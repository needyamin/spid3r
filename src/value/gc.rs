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
//! Auto-collection runs at statement boundaries by default, amortized so a
//! deep build pays O(N) total collection work instead of O(N²) (see
//! `maybe_collect`); `SPID3R_GC=0` disables it and `gc_collect()` can always
//! be called explicitly. Tracking covers every container and instance type
//! that can form reference cycles (20+ `*Data` types below) plus, since audit
//! A-13, environments and the tree's `FunctionData`/`ClassData` — and, since
//! 2026-11-14, the VM's closure model: `VmFunc`/`VmClass` cells and the
//! capture cells, without which a nested named function's
//! `VmFunc -> cell -> VFunc(same VmFunc)` recursion cycle leaked one island
//! per call (see the "VM mirror" note in GC.md).

use std::any::Any;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::value::collections::{
    BagData, BinTreeData, BstData, DequeData, GraphData, HashTableData, HeapData, ListData,
    MapData, MatrixData, PrioQueueData, QueueData, RefBoxData, RingData, SetData, StackData,
    TrieData, VecData, VectorData,
};
use crate::value::{CompiledInstanceData, InstanceData, Value, VmInstanceData};

type TraceFn = fn(&Arc<dyn Any + Send + Sync>, &mut dyn FnMut(&Value), &mut dyn FnMut(usize));
type ClearFn = fn(&Arc<dyn Any + Send + Sync>);

struct Entry {
    /// Identity = the cell allocation's address (`Arc::as_ptr` of the cell).
    id: usize,
    weak: Weak<dyn Any + Send + Sync>,
    trace: TraceFn,
    clear: ClearFn,
}

static REGISTRY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

const LOCAL_REGISTRY_CAP: usize = 8192;

thread_local! {
    /// Single-thread registration buffer (Track 3 RT-1): avoids locking
    /// global `REGISTRY` on short-lived containers while `!is_multithreaded()`.
    static LOCAL_REGISTRY: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
}

/// Flush any pending single-thread entries from this thread's `LOCAL_REGISTRY`
/// into the shared `REGISTRY`, discarding entries whose cells already died.
pub fn flush_local_registry() {
    let drained = LOCAL_REGISTRY.with(|local| {
        let mut v = local.borrow_mut();
        if v.is_empty() {
            Vec::new()
        } else {
            v.retain(|e| e.weak.strong_count() > 0);
            std::mem::take(&mut *v)
        }
    });
    if drained.is_empty() {
        return;
    }
    let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    reg.extend(drained);
    prune_registry_locked(&mut reg);
}

#[inline]
fn prune_registry_locked(reg: &mut Vec<Entry>) {
    if reg.len() > PRUNE_AT.load(Ordering::Relaxed) {
        reg.retain(|e| e.weak.strong_count() > 0);
        if reg.capacity() > 16384 && reg.capacity() > reg.len().saturating_mul(4) {
            let target = reg.len().saturating_mul(2).max(8192);
            reg.shrink_to(target);
        }
        PRUNE_AT.store(reg.len().saturating_mul(2).max(8192), Ordering::Relaxed);
    }
}

/// Best-effort non-blocking prune and capacity shrink of dead weak entries
/// in `REGISTRY` after thread-local AOT sweeps release short-lived containers.
pub fn maybe_prune_dead_entries() {
    if let Ok(mut reg) = REGISTRY.try_lock() {
        if reg.len() >= 4096 || (reg.capacity() > 16384 && reg.len() < reg.capacity() / 4) {
            reg.retain(|e| e.weak.strong_count() > 0);
            if reg.capacity() > 16384 && reg.capacity() > reg.len().saturating_mul(4) {
                let target = reg.len().saturating_mul(2).max(8192);
                reg.shrink_to(target);
            }
            PRUNE_AT.store(reg.len().saturating_mul(2).max(8192), Ordering::Relaxed);
        }
    }
}

#[inline]
fn register_entry(entry: Entry) {
    if !crate::value::is_multithreaded() {
        let overflow = LOCAL_REGISTRY.with(|local| {
            let mut v = local.borrow_mut();
            if v.capacity() == 0 {
                v.reserve(LOCAL_REGISTRY_CAP);
            }
            v.push(entry);
            if v.len() >= LOCAL_REGISTRY_CAP {
                v.retain(|e| e.weak.strong_count() > 0);
                if v.len() >= LOCAL_REGISTRY_CAP * 3 / 4 {
                    Some(std::mem::take(&mut *v))
                } else {
                    None
                }
            } else {
                None
            }
        });
        if let Some(batch) = overflow {
            let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
            reg.extend(batch);
            prune_registry_locked(&mut reg);
        }
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    reg.push(entry);
    prune_registry_locked(&mut reg);
    drop(reg);
    ALLOCS.fetch_add(1, Ordering::Relaxed);
}

/// A container cell whose children can be enumerated (for marking) and cleared
/// (for sweeping). Implemented for all container and instance types.
pub trait GcTrace {
    /// Call `f` once per strong `Value` child. Must NEVER over-count (a missed
    /// child only leaks; a spurious one could free live memory).
    fn gc_trace(&self, f: &mut dyn FnMut(&Value));
    /// Call `f` once per strong child **cell** that is not reachable through a
    /// `Value` — a closure's captured environment, a class's method cells.
    /// Only cell identities (`Arc::as_ptr`) are reported; the collector maps
    /// them through the registry. A missed child only leaks; a spurious one
    /// could free live memory.
    fn gc_trace_cells(&self, f: &mut dyn FnMut(usize)) {
        let _ = f;
    }
    /// Empty the cell's contents so the cycle's internal strong refs are dropped.
    fn gc_clear(&mut self);
}

impl GcTrace for VecData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        for v in self.as_slice() {
            f(v);
        }
    }
    fn gc_clear(&mut self) {
        self.clear_items();
    }
}

impl GcTrace for StackData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        for v in &self.items {
            f(v);
        }
    }
    fn gc_clear(&mut self) {
        self.items.clear();
    }
}

impl GcTrace for QueueData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        for v in &self.items {
            f(v);
        }
    }
    fn gc_clear(&mut self) {
        self.items.clear();
    }
}

impl GcTrace for DequeData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        for v in &self.items {
            f(v);
        }
    }
    fn gc_clear(&mut self) {
        self.items.clear();
    }
}

impl GcTrace for ListData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        let mut cur = self.head.clone();
        while let Some(node) = cur {
            if let Ok(b) = node.lock() {
                f(&b.value);
                cur = b.next.clone();
            } else {
                break;
            }
        }
    }
    fn gc_clear(&mut self) {
        self.clear_nodes();
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

impl GcTrace for crate::value::TupleData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        for v in &self.0 {
            f(v);
        }
    }
    fn gc_clear(&mut self) {
        crate::value::collections::drop_iteratively(std::mem::take(&mut self.0));
    }
}

impl GcTrace for BinTreeData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        let mut queue = VecDeque::new();
        if let Some(r) = &self.root {
            queue.push_back(r.clone());
        }
        while let Some(node) = queue.pop_front() {
            if let Ok(b) = node.lock() {
                f(&b.value);
                if let Some(l) = &b.left {
                    queue.push_back(l.clone());
                }
                if let Some(r) = &b.right {
                    queue.push_back(r.clone());
                }
            }
        }
    }
    fn gc_clear(&mut self) {
        self.root = None;
        self.open.clear();
        self.len = 0;
    }
}

impl GcTrace for BstData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        let mut stack = Vec::new();
        if let Some(r) = &self.root {
            stack.push(r.clone());
        }
        while let Some(node) = stack.pop() {
            if let Ok(b) = node.lock() {
                f(&b.value);
                if let Some(l) = &b.left {
                    stack.push(l.clone());
                }
                if let Some(r) = &b.right {
                    stack.push(r.clone());
                }
            }
        }
    }
    fn gc_clear(&mut self) {
        self.root = None;
        self.len = 0;
    }
}

impl GcTrace for HeapData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        for v in &self.items {
            f(v);
        }
    }
    fn gc_clear(&mut self) {
        self.items.clear();
    }
}

impl GcTrace for PrioQueueData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        for it in &self.items {
            f(&it.value);
        }
    }
    fn gc_clear(&mut self) {
        self.items.clear();
    }
}

impl GcTrace for GraphData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        for v in &self.nodes {
            f(v);
        }
    }
    fn gc_clear(&mut self) {
        self.nodes.clear();
        self.adj.clear();
    }
}

impl GcTrace for HashTableData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        for b in &self.buckets {
            for (_, v) in b {
                f(v);
            }
        }
    }
    fn gc_clear(&mut self) {
        for b in &mut self.buckets {
            b.clear();
        }
        self.size = 0;
    }
}

impl GcTrace for TrieData {
    fn gc_trace(&self, _f: &mut dyn FnMut(&Value)) {}
    fn gc_clear(&mut self) {
        if let Ok(mut b) = self.root.lock() {
            b.children.clear();
            b.end = false;
        }
        self.count = 0;
    }
}

impl GcTrace for MatrixData {
    fn gc_trace(&self, _f: &mut dyn FnMut(&Value)) {}
    fn gc_clear(&mut self) {
        self.data.clear();
        self.rows = 0;
        self.cols = 0;
    }
}

impl GcTrace for VectorData {
    fn gc_trace(&self, _f: &mut dyn FnMut(&Value)) {}
    fn gc_clear(&mut self) {
        self.data.clear();
    }
}

impl GcTrace for Vec<u8> {
    fn gc_trace(&self, _f: &mut dyn FnMut(&Value)) {}
    fn gc_clear(&mut self) {
        self.clear();
    }
}

impl GcTrace for RingData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        for v in &self.items {
            f(v);
        }
    }
    fn gc_clear(&mut self) {
        self.items.clear();
    }
}

impl GcTrace for BagData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        for (v, _) in &self.counts {
            f(v);
        }
    }
    fn gc_clear(&mut self) {
        self.counts.clear();
    }
}

impl GcTrace for RefBoxData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        f(&self.value);
    }
    fn gc_clear(&mut self) {
        self.value = Value::Null;
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
        for v in &self.slots {
            f(v);
        }
    }
    fn gc_clear(&mut self) {
        self.slots.clear();
    }
}

impl GcTrace for CompiledInstanceData {
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
    cells: &mut dyn FnMut(usize),
) {
    if let Some(m) = arc.downcast_ref::<crate::value::FastMutex<T>>() {
        if let Ok(g) = m.lock() {
            g.gc_trace(f);
            g.gc_trace_cells(cells);
        }
    }
}

fn clear_erased<T: GcTrace + Send + 'static>(arc: &Arc<dyn Any + Send + Sync>) {
    if let Some(m) = arc.downcast_ref::<crate::value::FastMutex<T>>() {
        if let Ok(mut g) = m.lock() {
            g.gc_clear();
        }
    }
}

/// Register a cell for cycle tracking and hand it back. Called at construction
/// sites (so the registry holds a *weak* handle; it never keeps a cell alive).
pub fn track<T: GcTrace + Send + 'static>(
    cell: Arc<crate::value::FastMutex<T>>,
) -> Arc<crate::value::FastMutex<T>> {
    let id = Arc::as_ptr(&cell) as *const () as usize;
    let weak: Weak<dyn Any + Send + Sync> =
        Arc::downgrade(&cell) as Weak<crate::value::FastMutex<T>>;
    register_entry(Entry {
        id,
        weak,
        trace: trace_erased::<T>,
        clear: clear_erased::<T>,
    });
    cell
}

/// Register an **immutable** cell — one whose contents never change after
/// construction (`FunctionData`, `ClassData`). Such a cell needs no lock for
/// tracing (the collector runs only while this thread is the only *running*
/// one, and nobody else can be mutating an immutable cell), and its
/// `gc_clear` must be a no-op: reclamation happens through ordinary `Arc`
/// refcounts once the cycle's mutable cells are cleared. An immutable cell
/// that needed clearing could not use this path.
pub fn track_immutable<T: GcTrace + Send + Sync + 'static>(cell: Arc<T>) -> Arc<T> {
    fn trace_immutable<T: GcTrace + 'static>(
        arc: &Arc<dyn Any + Send + Sync>,
        f: &mut dyn FnMut(&Value),
        cells: &mut dyn FnMut(usize),
    ) {
        if let Some(c) = arc.downcast_ref::<T>() {
            c.gc_trace(f);
            c.gc_trace_cells(cells);
        }
    }
    fn no_clear(_arc: &Arc<dyn Any + Send + Sync>) {}

    let id = Arc::as_ptr(&cell) as *const () as usize;
    let weak: Weak<dyn Any + Send + Sync> = Arc::downgrade(&cell) as Weak<T>;
    register_entry(Entry {
        id,
        weak,
        trace: trace_immutable::<T>,
        clear: no_clear,
    });
    cell
}

/// Visit the cell identity of `v` (peeling inline `Option`/`Result` boxes).
fn for_each_cell_id(v: &Value, f: &mut dyn FnMut(usize)) {
    match v {
        Value::List(a) | Value::VecDyn(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Stack(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Queue(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Deque(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::LinkedList(a) | Value::SinglyList(a) | Value::DoublyList(a) => {
            f(Arc::as_ptr(a) as *const () as usize)
        }
        Value::Map(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Set(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Tuple(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::BinTree(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Bst(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Heap(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::PrioQueue(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Graph(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::HashTable(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Trie(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Matrix(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Vector(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Bytes(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Ring(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Bag(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::RefBox(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Instance(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::VInstance(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::CInstance(a) => f(Arc::as_ptr(a) as *const () as usize),
        // Closure and class cells (audit A-13): an environment holds its
        // closures as `Value::Function`, so the `env -> closure` edge is only
        // visible when function values map to their registered cells. The
        // mirror `closure -> env` edge comes from `FunctionData::gc_trace_cells`.
        // `VFunc` / `VClass` are the bytecode VM's counterparts: a VM closure
        // is registered by `make_closure` (track_immutable) and its upvalue
        // cells are registered capture cells, so the
        // `VmFunc -> cell -> VFunc(same VmFunc)` recursion cycle of a nested
        // named function is fully visible and collectible.
        Value::Function(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Class(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::VFunc(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::VClass(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Option(Some(a)) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Result(a) => f(Arc::as_ptr(a) as *const () as usize),
        _ => {}
    }
}

/// Number of currently-live tracked cells (dead and duplicate entries pruned first).
/// Used by the `gc_live_cells()` diagnostic and tests.
pub fn live_cells() -> usize {
    flush_local_registry();
    let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let mut seen = HashSet::new();
    reg.retain(|e| e.weak.strong_count() > 0 && seen.insert(e.id));
    reg.len()
}

/// Run one collection pass; returns the number of cells swept. Safe to call at
/// any single-threaded point.
pub fn collect() -> usize {
    flush_local_registry();
    ALLOCS.store(0, Ordering::Relaxed);
    // Snapshot live entries (weak handles only — never upgrade during counting).
    let snapshot: Vec<(usize, Weak<dyn Any + Send + Sync>, TraceFn, ClearFn)> = {
        let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        let mut seen = HashSet::new();
        reg.retain(|e| e.weak.strong_count() > 0 && seen.insert(e.id));
        reg.iter()
            .map(|e| (e.id, e.weak.clone(), e.trace, e.clear))
            .collect()
    };
    let n = snapshot.len();
    if n == 0 {
        NEXT_GC.store(1024, Ordering::Relaxed);
        return 0;
    }

    // Strong counts, read before any upgrade (so our own temporary refs are not
    // counted).
    let strong: Vec<usize> = snapshot
        .iter()
        .map(|(_, w, _, _)| w.strong_count())
        .collect();

    let mut index: HashMap<usize, usize> = HashMap::with_capacity(n);
    for (i, (id, _, _, _)) in snapshot.iter().enumerate() {
        index.insert(*id, i);
    }

    // Count internal edges and build the adjacency for marking.
    let mut internal = vec![0usize; n];
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); n];
    for i in 0..n {
        if let Some(arc) = snapshot[i].1.upgrade() {
            let mut value_children: Vec<usize> = Vec::new();
            let mut cell_children: Vec<usize> = Vec::new();
            (snapshot[i].2)(
                &arc,
                &mut |v: &Value| {
                    for_each_cell_id(v, &mut |cid| value_children.push(cid));
                },
                &mut |cid: usize| {
                    // Non-`Value` children (a closure's environment, a class's
                    // method cells) carry the same strong refs as `Value` ones.
                    cell_children.push(cid);
                },
            );
            let mut child_ids = value_children;
            child_ids.extend(cell_children);
            let mut kids: Vec<usize> = Vec::with_capacity(child_ids.len());
            for cid in child_ids {
                if let Some(&j) = index.get(&cid) {
                    internal[j] += 1;
                    kids.push(j);
                }
            }
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
    let mut seen = HashSet::new();
    reg.retain(|e| e.weak.strong_count() > 0 && seen.insert(e.id));
    // Amortized re-arm (F-4): the next automatic collection waits until this
    // many more cells have been allocated, scaled to what survived this pass,
    // so collections double in interval on a growing heap (O(log N) of them
    // for a linear build) while churny garbage keeps the 1024 baseline.
    NEXT_GC.store(n.saturating_sub(freed).max(1024), Ordering::Relaxed);
    drop(reg);
    // The collection pass just walked every cell: refresh the heap estimate
    // for `runtime.memory_limit_mb` at no extra walk cost.
    rescan_memory();
    freed
}

// ------------------------------------------------------- heap accounting

/// Cached estimate of the total heap used by tracked script values, in
/// bytes. Every tracked cell contributes its own overhead plus the inline
/// sizes of its direct children; a child that is itself a tracked cell
/// contributes only a slot (its contents are counted once, at its own
/// entry). Free-standing values in untracked environments (plain `let`
/// scalars and strings that never live inside a tracked container) are not
/// visible to the registry and are therefore not counted — see docs/GC.md.
static MEM_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Monotonic ms (sched::now_ms origin) of the last full rescan.
static LAST_MEM_SCAN_MS: AtomicI64 = AtomicI64::new(i64::MIN);

/// Minimum ms between full heap rescans: the engines' limit checks run at
/// every statement boundary and every post-native-call guard, and each check
/// must stay O(1) amortized. The cap is soft, so a rescan interval's worth of
/// overshoot is acceptable (and the interval keeps a push-loop's O(N) rescan
/// from turning into O(N²) total work).
const MEM_RESCAN_INTERVAL_MS: i64 = 25;

/// Approximate inline byte size of a value's *own* storage, not counting
/// separately-tracked cells it references. Strings count their byte length;
/// scalars are inline; every tracked cell contributes a slot only.
fn child_bytes(v: &Value) -> usize {
    match v {
        Value::Str(s) => s.len() + 24,
        Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Null => 0,
        // Tracked cells (including Arc-backed Option/Result): slot cost only;
        // contents are counted at their own registry entry without recursion.
        _ => 8,
    }
}

/// One full pass over the registry: prune dead handles, sum the approximate
/// size of every live cell, cache the result. O(live cells).
fn rescan_memory() {
    flush_local_registry();
    let now = crate::sched::now_ms();
    let mut total = 0usize;
    {
        let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        let mut seen = HashSet::new();
        reg.retain(|e| e.weak.strong_count() > 0 && seen.insert(e.id));
        for e in reg.iter() {
            let Some(arc) = e.weak.upgrade() else {
                continue;
            };
            // A Cell because the two trace callbacks are simultaneous
            // `&mut dyn FnMut` borrows.
            let bytes = std::cell::Cell::new(64usize); // cell overhead (Arc + Mutex + header)
            (e.trace)(
                &arc,
                &mut |v: &Value| bytes.set(bytes.get() + child_bytes(v)),
                &mut |_: usize| bytes.set(bytes.get() + 8),
            );
            total += bytes.get();
        }
    }
    MEM_BYTES.store(total, Ordering::Relaxed);
    LAST_MEM_SCAN_MS.store(now, Ordering::Relaxed);
}

/// Lock-free read of the last cached tracked-heap byte estimate.
///
/// Safe to call while holding any container `Mutex` (unlike
/// [`memory_usage_bytes`], which may trigger `rescan_memory()` and lock
/// every tracked container in `REGISTRY`).
#[inline]
pub fn cached_memory_usage_bytes() -> usize {
    MEM_BYTES.load(Ordering::Relaxed)
}

/// Total heap used by tracked script values, in bytes (approximate).
/// Recomputed at most once per [`MEM_RESCAN_INTERVAL_MS`], and only while
/// this thread is the only one executing Spid3r code (the same gate the
/// cycle collector uses); otherwise the cached value is returned.
pub fn memory_usage_bytes() -> usize {
    let now = crate::sched::now_ms();
    let last = LAST_MEM_SCAN_MS.load(Ordering::Relaxed);
    if now.saturating_sub(last) >= MEM_RESCAN_INTERVAL_MS && crate::sched::gc_quiescent() {
        rescan_memory();
    }
    MEM_BYTES.load(Ordering::Relaxed)
}

/// Monotonic ms when an OOM diagnostic was last raised.
static LAST_OOM_MS: AtomicI64 = AtomicI64::new(i64::MIN);

/// After an OOM diagnostic is raised, the cap check stays silent for this
/// long so a program that *caught* the error can run its recovery statements
/// (typically dropping the offending container). Enforcement resumes on the
/// next fresh rescan: if the heap really is still over the cap, the next
/// allocation-heavy operation re-raises — the error stays catchable and the
/// loop bounded, exactly like PHP's catchable memory limit.
const OOM_GRACE_MS: i64 = 250;

/// The catchable OOM diagnostic when the heap estimate exceeds `cap_mb`
/// (MiB), else `None`. The message is part of the engines' contract:
/// `out of memory: script exceeded memory_limit_mb (N)`.
pub fn check_memory_cap(cap_mb: i64) -> Option<String> {
    if cap_mb <= 0 {
        return None;
    }
    let now = crate::sched::now_ms();
    if now.saturating_sub(LAST_OOM_MS.load(Ordering::Relaxed)) < OOM_GRACE_MS {
        // Grace window after a raised OOM: let the catch handler run.
        return None;
    }
    let used = memory_usage_bytes();
    if used > cap_mb as usize * 1024 * 1024 {
        LAST_OOM_MS.store(now, Ordering::Relaxed);
        Some(format!(
            "out of memory: script exceeded memory_limit_mb ({})",
            cap_mb
        ))
    } else {
        None
    }
}

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

/// `ALLOCS` level at which the next automatic collection fires. Amortized
/// (F-4): 1024 at first, then raised to the surviving cell count of each
/// collection, so building a linear chain of N cells needs only O(log N)
/// collections and O(N) total re-scan work instead of O(N²) at a fixed
/// threshold.
static NEXT_GC: AtomicUsize = AtomicUsize::new(1024);

/// Registry watermark for the `track` safety valve: dead entries are pruned
/// only once the registry grows past this, and the watermark then doubles
/// (min 8192). Keeps `track` O(1) amortized instead of O(registry) per
/// allocation while the registry stays bounded at roughly twice the live set
/// plus the 8192 baseline (F-4).
static PRUNE_AT: AtomicUsize = AtomicUsize::new(8192);

/// Is automatic collection enabled? **On by default**; `SPID3R_GC=0` or
/// `spid3r.gc = false` in `spid3r.ini` disables it.
fn auto_enabled() -> bool {
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| {
        if let Ok(v) = std::env::var("SPID3R_GC") {
            return v != "0";
        }
        crate::config::get("spid3r.gc")
            .map(|v| crate::config::schema::parse_bool(&v))
            .unwrap_or(true)
    })
}

/// Configured `memory.gc_threshold` from `spid3r.ini` (`0` = default amortized backoff).
fn gc_threshold() -> usize {
    static T: OnceLock<usize> = OnceLock::new();
    *T.get_or_init(|| {
        crate::config::get("memory.gc_threshold")
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0)
    })
}

/// Called at interpreter statement boundaries. Collects once at least
/// `NEXT_GC` tracked cells have been allocated since the last collection
/// (or when `memory.gc_threshold > 0` and `ALLOCS >= gc_threshold()`),
/// and only while this thread is the only one executing Spid3r code
/// (`sched::gc_quiescent`).
#[inline]
pub fn maybe_collect() {
    let allocs = ALLOCS.load(Ordering::Relaxed);
    let thresh = gc_threshold();
    let due = if thresh > 0 {
        allocs >= thresh || allocs >= NEXT_GC.load(Ordering::Relaxed)
    } else {
        allocs >= NEXT_GC.load(Ordering::Relaxed)
    };
    if due && auto_enabled() && crate::sched::gc_quiescent() {
        collect();
    }
}
