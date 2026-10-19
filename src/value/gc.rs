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
//! be called explicitly. Tracking
//! covers every container and instance type that can form reference cycles
//! (20+ `*Data` types below); closures/environments are not tracked yet
//! (cycles through them leak — see GC.md).

use std::any::Any;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::value::collections::{
    BagData, BinTreeData, BstData, DequeData, GraphData, HashTableData, HeapData, ListData,
    MapData, MatrixData, PrioQueueData, QueueData, RefBoxData, RingData, SetData, StackData,
    TrieData, VecData,
};
use crate::value::{CompiledInstanceData, InstanceData, Value, VmInstanceData};

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
/// (for sweeping). Implemented for all container and instance types.
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

impl GcTrace for Vec<Value> {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        for v in self {
            f(v);
        }
    }
    fn gc_clear(&mut self) {
        self.clear();
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
        for v in self.fields.values() {
            f(v);
        }
    }
    fn gc_clear(&mut self) {
        self.fields.clear();
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
    // Amortized (F-4): prune only past the `PRUNE_AT` watermark and then double
    // it. The old prune-on-every-allocation-above-8192 cost O(registry) per
    // allocation — quadratic on a deep build where the registry stays large and
    // live, on BOTH engines (they share `new_ref` → `track`).
    if reg.len() > PRUNE_AT.load(Ordering::Relaxed) {
        let mut seen = HashSet::new();
        reg.retain(|e| e.weak.strong_count() > 0 && seen.insert(e.id));
        PRUNE_AT.store(reg.len().saturating_mul(2).max(8192), Ordering::Relaxed);
    }
    drop(reg);
    ALLOCS.fetch_add(1, Ordering::Relaxed);
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
        Value::Ring(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Bag(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::RefBox(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Instance(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::VInstance(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::CInstance(a) => f(Arc::as_ptr(a) as *const () as usize),
        Value::Option(Some(inner)) => for_each_cell_id(inner, f),
        Value::Result(Ok(inner) | Err(inner)) => for_each_cell_id(inner, f),
        _ => {}
    }
}

/// Number of currently-live tracked cells (dead and duplicate entries pruned first).
/// Used by the `gc_live_cells()` diagnostic and tests.
pub fn live_cells() -> usize {
    let mut reg = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    let mut seen = HashSet::new();
    reg.retain(|e| e.weak.strong_count() > 0 && seen.insert(e.id));
    reg.len()
}

/// Run one collection pass; returns the number of cells swept. Safe to call at
/// any single-threaded point.
pub fn collect() -> usize {
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
                for_each_cell_id(v, &mut |cid| {
                    if let Some(&j) = index.get(&cid) {
                        internal[j] += 1;
                        kids.push(j);
                    }
                });
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
    let mut seen = HashSet::new();
    reg.retain(|e| e.weak.strong_count() > 0 && seen.insert(e.id));
    // Amortized re-arm (F-4): the next automatic collection waits until this
    // many more cells have been allocated, scaled to what survived this pass,
    // so collections double in interval on a growing heap (O(log N) of them
    // for a linear build) while churny garbage keeps the 1024 baseline.
    NEXT_GC.store(n.saturating_sub(freed).max(1024), Ordering::Relaxed);
    freed
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

/// Is automatic collection enabled? **On by default**; `SPID3R_GC=0` disables it.
fn auto_enabled() -> bool {
    static E: OnceLock<bool> = OnceLock::new();
    *E.get_or_init(|| std::env::var("SPID3R_GC").map(|v| v != "0").unwrap_or(true))
}

/// Called at interpreter statement boundaries. Collects once at least
/// `NEXT_GC` tracked cells have been allocated since the last collection
/// (1024 at first, then the previous pass's surviving cell count — amortized
/// exponential backoff, see F-4), and only while the scheduler has no live
/// tasks (a worker could clone a cell concurrently, racing the strong counts).
#[inline]
pub fn maybe_collect() {
    if ALLOCS.load(Ordering::Relaxed) >= NEXT_GC.load(Ordering::Relaxed)
        && auto_enabled()
        && crate::sched::Scheduler::shared().live_tasks() == 0
    {
        collect();
    }
}
