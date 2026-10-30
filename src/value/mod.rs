//! The Spid3r value model. Reference types are `Arc<Mutex<_>>` so values can
//! cross task boundaries; assignment aliases, `clone()` deep-copies.

use std::any::Any;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::os::raw::c_void;
use std::sync::{Arc, Mutex};

use crate::ast::{FnProto, Vis};
use crate::environment::Env;

pub mod collections;
pub mod gc;
use crate::value::gc::GcTrace;

pub type Ref<T> = Arc<Mutex<T>>;

pub fn new_ref<T: gc::GcTrace + Send + 'static>(v: T) -> Ref<T> {
    gc::track(Arc::new(Mutex::new(v)))
}

pub fn new_untracked_ref<T>(v: T) -> Ref<T> {
    Arc::new(Mutex::new(v))
}

/// A captured variable for the bytecode VM: a heap cell shared by an open
/// closure and its defining frame. Because it is `Arc<Mutex<_>>` it is `Send`
/// and survives after the defining frame returns (and across spawned tasks).
pub type UpvalueCell = Arc<Mutex<Value>>;

/// A function compiled to bytecode. `upvalues` holds the upvalue *cells*
/// captured from the enclosing frame (read via `OpGetUpvalue`); `own_upvalues`
/// is how many upvalue cells this function must allocate for its *own* captured
/// locals when it is called. The chunk is shared by `Arc`: every closure
/// created from the same prototype reuses the same bytecode.
#[derive(Clone)]
pub struct VmFunc {
    pub name: Option<String>,
    pub arity: usize,
    /// Smallest accepted argument count (trailing constant defaults).
    pub min_arity: usize,
    /// Baked constant defaults for the trailing `defaults.len()` parameters.
    pub defaults: Vec<Value>,
    pub is_async: bool,
    pub chunk: std::sync::Arc<crate::vm::bytecode::Chunk>,
    pub upvalues: Vec<UpvalueCell>,
    pub own_upvalues: u8,
    /// The globals map this function was defined in. A module's functions
    /// resolve names against their own module scope, so two modules may
    /// export the same function name without one shadowing the other.
    /// `None` on prototypes built before the VM fills it in (the compiler's
    /// and the bytecode reader's paths); the VM then uses its current map.
    pub globals: Option<Arc<Mutex<HashMap<String, Value>>>>,
    /// Local names and live ranges, for the debugger. Empty for a function
    /// that came from a `.matrixc` artifact (the format carries no source).
    pub debug_locals: Vec<crate::vm::bytecode::DebugLocal>,
}

/// A class compiled to bytecode. Mirrors `ClassData` but methods are `VmFunc`.
pub struct VmClass {
    pub name: String,
    pub parent: Option<Arc<VmClass>>,
    pub fields: Vec<(String, Vis)>,
    pub methods: std::collections::HashMap<String, (Vis, Arc<VmFunc>)>,
    pub ctor: Option<Arc<VmFunc>>,
}

/// An instance of a bytecode class. Mirrors `InstanceData` but the class is a
/// `VmClass`.
pub struct VmInstanceData {
    pub class: Arc<VmClass>,
    pub fields: std::collections::HashMap<String, Value>,
}

impl Drop for VmInstanceData {
    fn drop(&mut self) {
        // A-17: same iterative teardown as `InstanceData`.
        crate::value::collections::drop_iteratively(self.fields.drain().map(|(_, v)| v).collect());
    }
}

/// Keys allowed by `map`, `set`, `bag`, `hash_table`, and graph node ids:
/// null, bool, int, string. Floats are rejected on purpose.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MapKey {
    Null,
    Bool(bool),
    Int(i64),
    Str(Arc<str>),
}

impl MapKey {
    pub fn display(&self) -> String {
        match self {
            MapKey::Null => "null".to_string(),
            MapKey::Bool(b) => b.to_string(),
            MapKey::Int(i) => i.to_string(),
            MapKey::Str(s) => s.to_string(),
        }
    }
}

#[inline]
pub fn as_map_key(v: &Value) -> Result<MapKey, String> {
    match v {
        Value::Null => Ok(MapKey::Null),
        Value::Bool(b) => Ok(MapKey::Bool(*b)),
        Value::Int(i) => Ok(MapKey::Int(*i)),
        Value::Str(s) => Ok(MapKey::Str(s.clone())),
        other => Err(format!(
            "{} is not a valid key; keys must be null, bool, int, or string",
            type_name(other)
        )),
    }
}

#[inline]
pub fn key_to_value(k: &MapKey) -> Value {
    match k {
        MapKey::Null => Value::Null,
        MapKey::Bool(b) => Value::Bool(*b),
        MapKey::Int(i) => Value::Int(*i),
        MapKey::Str(s) => Value::Str(s.clone()),
    }
}

/// A user-defined function: its AST prototype plus the environment it closed
/// over. Closures capture the defining scope, same as the reference runner.
pub struct FunctionData {
    pub proto: FnProto,
    pub env: Env,
}

impl GcTrace for FunctionData {
    /// A closure's only non-`Value` child is its captured environment
    /// (audit A-13): `env -> closure -> env` is the cycle being collected.
    /// `gc_trace`/`gc_clear` are empty on purpose: the prototype holds no
    /// values, and the cycle is broken from the environment's side (clearing
    /// its vars drops the last Arcs, which reclaims this cell naturally).
    fn gc_trace_cells(&self, f: &mut dyn FnMut(usize)) {
        f(Arc::as_ptr(&self.env) as *const () as usize);
    }
    fn gc_trace(&self, _f: &mut dyn FnMut(&Value)) {}
    fn gc_clear(&mut self) {}
}

/// Construct a **tracked** closure. The single sanctioned way to build a
/// `FunctionData`: an unregistered closure cell cannot be traversed, which
/// would turn the closure's environment into a phantom root (a leak) or, with
/// a partially-tracked graph, risk sweeping live memory.
pub fn new_function(proto: crate::ast::FnProto, env: Env) -> Arc<FunctionData> {
    gc::track_immutable(Arc::new(FunctionData { proto, env }))
}

/// Native (Rust) functions exposed to Spid3r.
pub struct NativeFn {
    pub name: &'static str,
    pub func: fn(&NativeCtx, Vec<Value>) -> Result<Value, String>,
}

pub struct NativeCtx {
    pub sched: Arc<crate::sched::Scheduler>,
}

/// A host-registered closure (embedding API): like [`NativeFn`], but carrying
/// Rust state, so a host application can expose its own functionality with
/// captured data. Registered through `spid3r_lang::embed::Engine::register`.
#[derive(Clone)]
pub struct NativeClosureData {
    pub name: String,
    pub func: Arc<NativeClosureFn>,
}

/// The closure signature behind [`NativeClosureData`] (factored out so host
/// APIs can name it without a clippy type-complexity complaint).
pub type NativeClosureFn = dyn Fn(&NativeCtx, Vec<Value>) -> Result<Value, String> + Send + Sync;

pub struct ClassData {
    pub name: String,
    pub parent: Option<Arc<ClassData>>,
    pub interfaces: Vec<String>,
    pub fields: Vec<(String, crate::ast::Vis)>,
    pub methods: std::collections::HashMap<String, (crate::ast::Vis, Arc<FunctionData>)>,
    pub ctor: Option<Arc<FunctionData>>,
}

impl GcTrace for ClassData {
    /// A class's methods and constructor are closure cells, and the parent is
    /// a class cell: an instance's class must stay traversable (audit A-13).
    fn gc_trace_cells(&self, f: &mut dyn FnMut(usize)) {
        for (_, fd) in self.methods.values() {
            f(Arc::as_ptr(fd) as *const () as usize);
        }
        if let Some(c) = &self.ctor {
            f(Arc::as_ptr(c) as *const () as usize);
        }
        if let Some(p) = &self.parent {
            f(Arc::as_ptr(p) as *const () as usize);
        }
    }
    fn gc_trace(&self, _f: &mut dyn FnMut(&Value)) {}
    /// Empty on purpose: the class is reclaimed when its last instance (or
    /// the scope that declared it) drops its Arc.
    fn gc_clear(&mut self) {}
}

/// Construct a **tracked** class (see [`new_function`] for why registration
/// is mandatory).
#[allow(clippy::too_many_arguments)]
pub fn new_class(
    name: String,
    parent: Option<Arc<ClassData>>,
    interfaces: Vec<String>,
    fields: Vec<(String, Vis)>,
    methods: std::collections::HashMap<String, (Vis, Arc<FunctionData>)>,
    ctor: Option<Arc<FunctionData>>,
) -> Arc<ClassData> {
    gc::track_immutable(Arc::new(ClassData {
        name,
        parent,
        interfaces,
        fields,
        methods,
        ctor,
    }))
}

pub struct InstanceData {
    pub class: Arc<ClassData>,
    pub fields: std::collections::HashMap<String, Value>,
}

impl Drop for InstanceData {
    fn drop(&mut self) {
        // A-17: `class Node { next }` chains are the most idiomatic deep
        // structure in a dynamic language; drain the field map iteratively
        // instead of recursing once per link.
        crate::value::collections::drop_iteratively(self.fields.drain().map(|(_, v)| v).collect());
    }
}

/// A function compiled to native machine code by the LLVM backend.
///
/// `code` is the machine-code entry point (`extern "C"`), `env` points at the
/// captured-variable slots of a closure (null for a plain function). The raw
/// pointer is sound because the environment outlives every call: it is
/// heap-allocated when the closure is created and released with it.
#[derive(Clone)]
pub struct CompiledFn {
    pub name: Option<String>,
    pub arity: usize,
    /// Smallest accepted argument count (trailing constant defaults; the
    /// generated prologue fills omitted arguments itself, so dispatchers only
    /// range-check).
    pub min_arity: usize,
    pub code: unsafe extern "C" fn(*mut c_void, usize, *const *mut Value) -> *mut Value,
    pub env: *mut c_void,
}

// SAFETY: `code` points at static machine code and is immutable. `env` is a
// heap block of `*mut UpvalueSlot`, and every slot is an
// `AtomicPtr<Value>` accessed with `SeqCst` loads and stores
// (`rt::spid3r_cell_get` / `spid3r_cell_set`), so a torn `Value` cannot be
// observed and the block is never mutated after allocation — only the cells it
// points at, atomically. The block itself is leaked for the life of the
// process, so there is no double-free when a closure crosses a thread.
//
// What this does NOT give you is atomicity of a *read-modify-write*:
// `total = total + 1` on a captured variable is a load followed by a store, so
// two threads calling the same closure lose updates — exactly as they do on the
// tree and VM engines (measured: 28777 / 33678 / 23993 for 40000 increments,
// one per engine). That is a language-level contract, not a memory-safety
// problem: shared mutable state needs `mutex()`. See docs/SECURITY.md.
//
// An earlier version of this comment claimed the env was "only ever touched by
// the single thread that created the closure". That is false — a closure
// captured by `spawn` runs on a worker thread — and it was the wrong reason to
// believe the impl is sound. Audit F-08.
unsafe impl Send for CompiledFn {}
unsafe impl Sync for CompiledFn {}

/// A class compiled to native machine code (LLVM backend).
pub struct CompiledClass {
    pub name: String,
    pub parent: Option<Arc<CompiledClass>>,
    pub fields: Vec<(String, crate::ast::Vis)>,
    pub methods: std::collections::HashMap<String, (crate::ast::Vis, Arc<CompiledFn>)>,
    pub ctor: Option<Arc<CompiledFn>>,
}

/// An instance of a natively compiled class.
pub struct CompiledInstanceData {
    pub class: Arc<CompiledClass>,
    pub fields: std::collections::HashMap<String, Value>,
}

impl Drop for CompiledInstanceData {
    fn drop(&mut self) {
        // A-17: same iterative teardown as `InstanceData`.
        crate::value::collections::drop_iteratively(self.fields.drain().map(|(_, v)| v).collect());
    }
}

#[derive(Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    /// Strings are `Arc<str>`: cloning a `Value::Str` (which happens on every
    /// variable read, name lookup, and collection get) is a refcount bump
    /// instead of a heap allocation + copy.
    Str(std::sync::Arc<str>),
    Null,

    List(Ref<crate::value::collections::VecData>),
    VecDyn(Ref<crate::value::collections::VecData>),
    Stack(Ref<crate::value::collections::StackData>),
    Queue(Ref<crate::value::collections::QueueData>),
    Deque(Ref<crate::value::collections::DequeData>),
    LinkedList(Ref<crate::value::collections::ListData>),
    SinglyList(Ref<crate::value::collections::ListData>),
    DoublyList(Ref<crate::value::collections::ListData>),
    Map(Ref<crate::value::collections::MapData>),
    Set(Ref<crate::value::collections::SetData>),
    Tuple(Ref<Vec<Value>>),
    BinTree(Ref<crate::value::collections::BinTreeData>),
    Bst(Ref<crate::value::collections::BstData>),
    Heap(Ref<crate::value::collections::HeapData>),
    PrioQueue(Ref<crate::value::collections::PrioQueueData>),
    Graph(Ref<crate::value::collections::GraphData>),
    HashTable(Ref<crate::value::collections::HashTableData>),
    Trie(Ref<crate::value::collections::TrieData>),
    Matrix(Ref<crate::value::collections::MatrixData>),
    Vector(Ref<crate::value::collections::VectorData>),
    Bytes(Ref<Vec<u8>>),
    Ring(Ref<crate::value::collections::RingData>),
    Bag(Ref<crate::value::collections::BagData>),
    RefBox(Ref<crate::value::collections::RefBoxData>),
    Option(Option<Box<Value>>),
    Result(std::result::Result<Box<Value>, Box<Value>>),

    Function(Arc<FunctionData>),
    Native(Arc<NativeFn>),
    /// A host-registered closure with captured Rust state (embedding API).
    NativeClosure(Arc<NativeClosureData>),

    /// A function compiled to bytecode (the VM's closure representation).
    VFunc(Arc<VmFunc>),
    /// A class compiled to bytecode (the VM's class representation).
    VClass(Arc<VmClass>),

    Class(Arc<ClassData>),
    Instance(Ref<InstanceData>),
    /// An instance of a bytecode class.
    VInstance(Ref<VmInstanceData>),

    Task(Arc<crate::sched::TaskHandle>),
    Channel(Arc<crate::sched::ChannelData>),
    MutexVal(std::sync::Arc<crate::sched::MutexData>),
    /// A handle to an open SQLite database (`db_open`). Methods
    /// (`query`/`run`/`exec`/`close`) route through the shared dispatcher;
    /// the connection lives in `stdlib::db::DbData` and closes on drop.
    Database(std::sync::Arc<std::sync::Mutex<crate::stdlib::db::DbData>>),

    /// A type-erased weak reference (`weak(x)` / `deref(w)`): the manual
    /// escape hatch for reference cycles, which would otherwise leak (the
    /// value model is reference-counted; see ROADMAP "Known limitations").
    Weak(Arc<Mutex<WeakCell>>),
    /// A function compiled to native machine code (LLVM backend).
    Cfn(Arc<CompiledFn>),
    /// A class compiled to native machine code (LLVM backend).
    CClass(Arc<CompiledClass>),
    /// An instance of a natively compiled class.
    CInstance(Ref<CompiledInstanceData>),
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Value({})", display_value(self))
    }
}

/// Returns true if the value can be invoked like a function.
pub fn is_callable(v: &Value) -> bool {
    matches!(
        v,
        Value::Function(_)
            | Value::VFunc(_)
            | Value::Native(_)
            | Value::NativeClosure(_)
            | Value::Class(_)
            | Value::VClass(_)
            | Value::Cfn(_)
            | Value::CClass(_)
    )
}

/// Stable pointer identity for reference types (used by `==`).
pub fn ptr_id<T>(rc: &Arc<Mutex<T>>) -> u64 {
    Arc::as_ptr(rc) as usize as u64
}

pub fn arc_id<T>(rc: &Arc<T>) -> u64 {
    Arc::as_ptr(rc) as usize as u64
}

// ------------------------------------------------------------------ weak refs

/// Which container kind a weak reference points at. Some collection types
/// share an inner representation (`List`/`VecDyn` are both `Ref<VecData>`),
/// so the kind is recorded when the weak is created and used to re-materialize
/// the strong value on `deref`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeakKind {
    List,
    VecDyn,
    Stack,
    Queue,
    Deque,
    LinkedList,
    SinglyList,
    DoublyList,
    Map,
    Set,
    Tuple,
    BinTree,
    Bst,
    Heap,
    PrioQueue,
    Graph,
    HashTable,
    Trie,
    Matrix,
    Vector,
    Bytes,
    Ring,
    Bag,
    RefBox,
    Instance,
    VInstance,
    CInstance,
}

/// The payload of a [`Value::Weak`]: a type-erased weak pointer plus its kind.
pub struct WeakCell {
    pub w: std::sync::Weak<dyn Any + Send + Sync>,
    pub kind: WeakKind,
}

/// Erase a reference-counted container into a `dyn Any` trait object.
fn erase<T: Any + Send>(r: &Arc<Mutex<T>>) -> Arc<dyn Any + Send + Sync> {
    r.clone()
}

/// `weak(x)` — create a weak reference to a container (`x` must be a
/// reference-counted value: a collection, instance, or ref box). Scalars,
/// strings, and option/result values cannot be weak.
pub fn make_weak(v: &Value) -> Result<Value, String> {
    use WeakKind::*;
    let (kind, w) = match v {
        Value::List(r) | Value::VecDyn(r) => (
            if matches!(v, Value::List(_)) { List } else { VecDyn },
            Arc::downgrade(&erase(r)),
        ),
        Value::Stack(r) => (Stack, Arc::downgrade(&erase(r))),
        Value::Queue(r) => (Queue, Arc::downgrade(&erase(r))),
        Value::Deque(r) => (Deque, Arc::downgrade(&erase(r))),
        Value::LinkedList(r) | Value::SinglyList(r) | Value::DoublyList(r) => (
            match v {
                Value::LinkedList(_) => LinkedList,
                Value::SinglyList(_) => SinglyList,
                _ => DoublyList,
            },
            Arc::downgrade(&erase(r)),
        ),
        Value::Map(r) => (Map, Arc::downgrade(&erase(r))),
        Value::Set(r) => (Set, Arc::downgrade(&erase(r))),
        Value::Tuple(r) => (Tuple, Arc::downgrade(&erase(r))),
        Value::BinTree(r) => (BinTree, Arc::downgrade(&erase(r))),
        Value::Bst(r) => (Bst, Arc::downgrade(&erase(r))),
        Value::Heap(r) => (Heap, Arc::downgrade(&erase(r))),
        Value::PrioQueue(r) => (PrioQueue, Arc::downgrade(&erase(r))),
        Value::Graph(r) => (Graph, Arc::downgrade(&erase(r))),
        Value::HashTable(r) => (HashTable, Arc::downgrade(&erase(r))),
        Value::Trie(r) => (Trie, Arc::downgrade(&erase(r))),
        Value::Matrix(r) => (Matrix, Arc::downgrade(&erase(r))),
        Value::Vector(r) => (Vector, Arc::downgrade(&erase(r))),
        Value::Bytes(r) => (Bytes, Arc::downgrade(&erase(r))),
        Value::Ring(r) => (Ring, Arc::downgrade(&erase(r))),
        Value::Bag(r) => (Bag, Arc::downgrade(&erase(r))),
        Value::RefBox(r) => (RefBox, Arc::downgrade(&erase(r))),
        Value::Instance(r) => (Instance, Arc::downgrade(&erase(r))),
        Value::VInstance(r) => (VInstance, Arc::downgrade(&erase(r))),
        Value::CInstance(r) => (CInstance, Arc::downgrade(&erase(r))),
        other => {
            return Err(format!(
                "cannot make a weak reference to a {} (only collections, instances, and ref boxes can be weak)",
                type_name(other)
            ))
        }
    };
    Ok(Value::Weak(new_untracked_ref(WeakCell { w, kind })))
}

/// `deref(w)` — the strong value the weak points at, or `None` when the
/// target is gone (the caller maps that to `null`).
pub fn weak_upgrade(v: &Value) -> Result<Option<Value>, String> {
    let cell = match v {
        Value::Weak(c) => c,
        other => {
            return Err(format!(
                "deref expects a weak reference, got a {}",
                type_name(other)
            ))
        }
    };
    let guard = cell.lock().unwrap();
    let Some(any) = guard.w.upgrade() else {
        return Ok(None);
    };
    use WeakKind::*;
    let strong = match guard.kind {
        List => Value::List(downcast_cell::<crate::value::collections::VecData>(&any)?),
        VecDyn => Value::VecDyn(downcast_cell::<crate::value::collections::VecData>(&any)?),
        Stack => Value::Stack(downcast_cell::<crate::value::collections::StackData>(&any)?),
        Queue => Value::Queue(downcast_cell::<crate::value::collections::QueueData>(&any)?),
        Deque => Value::Deque(downcast_cell::<crate::value::collections::DequeData>(&any)?),
        LinkedList => {
            Value::LinkedList(downcast_cell::<crate::value::collections::ListData>(&any)?)
        }
        SinglyList => {
            Value::SinglyList(downcast_cell::<crate::value::collections::ListData>(&any)?)
        }
        DoublyList => {
            Value::DoublyList(downcast_cell::<crate::value::collections::ListData>(&any)?)
        }
        Map => Value::Map(downcast_cell::<crate::value::collections::MapData>(&any)?),
        Set => Value::Set(downcast_cell::<crate::value::collections::SetData>(&any)?),
        Tuple => Value::Tuple(downcast_cell::<Vec<Value>>(&any)?),
        BinTree => Value::BinTree(downcast_cell::<crate::value::collections::BinTreeData>(
            &any,
        )?),
        Bst => Value::Bst(downcast_cell::<crate::value::collections::BstData>(&any)?),
        Heap => Value::Heap(downcast_cell::<crate::value::collections::HeapData>(&any)?),
        PrioQueue => Value::PrioQueue(downcast_cell::<crate::value::collections::PrioQueueData>(
            &any,
        )?),
        Graph => Value::Graph(downcast_cell::<crate::value::collections::GraphData>(&any)?),
        HashTable => Value::HashTable(downcast_cell::<crate::value::collections::HashTableData>(
            &any,
        )?),
        Trie => Value::Trie(downcast_cell::<crate::value::collections::TrieData>(&any)?),
        Matrix => Value::Matrix(downcast_cell::<crate::value::collections::MatrixData>(
            &any,
        )?),
        Vector => Value::Vector(downcast_cell::<crate::value::collections::VectorData>(
            &any,
        )?),
        Bytes => Value::Bytes(downcast_cell::<Vec<u8>>(&any)?),
        Ring => Value::Ring(downcast_cell::<crate::value::collections::RingData>(&any)?),
        Bag => Value::Bag(downcast_cell::<crate::value::collections::BagData>(&any)?),
        RefBox => Value::RefBox(downcast_cell::<crate::value::collections::RefBoxData>(
            &any,
        )?),
        Instance => Value::Instance(downcast_cell::<InstanceData>(&any)?),
        VInstance => Value::VInstance(downcast_cell::<VmInstanceData>(&any)?),
        CInstance => Value::CInstance(downcast_cell::<CompiledInstanceData>(&any)?),
    };
    Ok(Some(strong))
}

/// True when the weak reference's target is still alive.
pub fn weak_alive(v: &Value) -> Result<bool, String> {
    let cell = match v {
        Value::Weak(c) => c,
        other => {
            return Err(format!(
                "deref expects a weak reference, got a {}",
                type_name(other)
            ))
        }
    };
    let guard = cell.lock().unwrap();
    Ok(guard.w.upgrade().is_some())
}

fn downcast_cell<T: Any + Send + Sync>(
    any: &Arc<dyn Any + Send + Sync>,
) -> Result<Arc<Mutex<T>>, String> {
    match Arc::downcast(any.clone()) {
        Ok(arc) => Ok(arc),
        Err(_) => Err("weak reference target has an unexpected type (internal error)".to_string()),
    }
}

/// Stable identity of any reference-carrying value (`Some`), or `None` for
/// plain scalars. Used by `values_equal`'s self-comparison fast path.
macro_rules! ref_id_of {
    ($r:expr) => {
        Some(Arc::as_ptr($r) as *const u8 as *const () as u64)
    };
}

fn ref_id(v: &Value) -> Option<u64> {
    match v {
        Value::List(r) | Value::VecDyn(r) => ref_id_of!(r),
        Value::Stack(r) => ref_id_of!(r),
        Value::Queue(r) => ref_id_of!(r),
        Value::Deque(r) => ref_id_of!(r),
        Value::LinkedList(r) | Value::SinglyList(r) | Value::DoublyList(r) => ref_id_of!(r),
        Value::Map(r) => ref_id_of!(r),
        Value::Set(r) => ref_id_of!(r),
        Value::Tuple(r) => ref_id_of!(r),
        Value::BinTree(r) => ref_id_of!(r),
        Value::Bst(r) => ref_id_of!(r),
        Value::Heap(r) => ref_id_of!(r),
        Value::PrioQueue(r) => ref_id_of!(r),
        Value::Graph(r) => ref_id_of!(r),
        Value::HashTable(r) => ref_id_of!(r),
        Value::Trie(r) => ref_id_of!(r),
        Value::Matrix(r) => ref_id_of!(r),
        Value::Vector(r) => ref_id_of!(r),
        Value::Bytes(r) => ref_id_of!(r),
        Value::Ring(r) => ref_id_of!(r),
        Value::Bag(r) => ref_id_of!(r),
        Value::RefBox(r) => ref_id_of!(r),
        Value::Instance(r) => ref_id_of!(r),
        Value::VInstance(r) => ref_id_of!(r),
        Value::CInstance(r) => ref_id_of!(r),
        Value::MutexVal(r) => ref_id_of!(r),
        Value::Database(r) => ref_id_of!(r),
        Value::Function(f) => ref_id_of!(f),
        Value::Native(f) => ref_id_of!(f),
        Value::NativeClosure(f) => ref_id_of!(f),
        Value::VFunc(f) => ref_id_of!(f),
        Value::VClass(c) => ref_id_of!(c),
        Value::Class(c) => ref_id_of!(c),
        Value::Task(t) => ref_id_of!(t),
        Value::Channel(c) => ref_id_of!(c),
        Value::Cfn(f) => ref_id_of!(f),
        Value::CClass(c) => ref_id_of!(c),
        Value::Weak(c) => ref_id_of!(c),
        _ => None,
    }
}

pub fn hash_value(v: &Value) -> u64 {
    let mut h = DefaultHasher::new();
    match v {
        Value::Null => 0u8.hash(&mut h),
        Value::Bool(b) => {
            1u8.hash(&mut h);
            b.hash(&mut h);
        }
        Value::Int(i) => {
            2u8.hash(&mut h);
            i.hash(&mut h);
        }
        Value::Str(s) => {
            3u8.hash(&mut h);
            s.hash(&mut h);
        }
        Value::Float(f) => {
            4u8.hash(&mut h);
            f.to_bits().hash(&mut h);
        }
        Value::List(d) => {
            5u8.hash(&mut h);
            ptr_id(d).hash(&mut h);
        }
        Value::VecDyn(d) => {
            6u8.hash(&mut h);
            ptr_id(d).hash(&mut h);
        }
        Value::Stack(d) => {
            7u8.hash(&mut h);
            ptr_id(d).hash(&mut h);
        }
        Value::Queue(d) => {
            8u8.hash(&mut h);
            ptr_id(d).hash(&mut h);
        }
        Value::Deque(d) => {
            9u8.hash(&mut h);
            ptr_id(d).hash(&mut h);
        }
        Value::Map(d) => {
            10u8.hash(&mut h);
            ptr_id(d).hash(&mut h);
        }
        Value::Set(d) => {
            11u8.hash(&mut h);
            ptr_id(d).hash(&mut h);
        }
        Value::Tuple(t) => {
            12u8.hash(&mut h);
            ptr_id(t).hash(&mut h);
        }
        Value::Instance(i) => {
            13u8.hash(&mut h);
            ptr_id(i).hash(&mut h);
        }
        Value::VInstance(i) => {
            15u8.hash(&mut h);
            ptr_id(i).hash(&mut h);
        }
        Value::Function(f) => {
            14u8.hash(&mut h);
            arc_id(f).hash(&mut h);
        }
        Value::VFunc(f) => {
            16u8.hash(&mut h);
            arc_id(f).hash(&mut h);
        }
        Value::VClass(c) => {
            17u8.hash(&mut h);
            arc_id(c).hash(&mut h);
        }
        Value::Vector(d) => {
            18u8.hash(&mut h);
            ptr_id(d).hash(&mut h);
        }
        Value::Bytes(d) => {
            19u8.hash(&mut h);
            ptr_id(d).hash(&mut h);
        }
        _ => {
            255u8.hash(&mut h);
        }
    }
    h.finish()
}

/// Deep structural equality. Primitives compare by value (int/float compare
/// numerically); collections compare element-wise; everything with identity
/// (functions, instances, tasks, channels, mutexes, classes) compares by
/// pointer.
pub fn values_equal(a: &Value, b: &Value) -> bool {
    let mut seen = std::collections::HashSet::new();
    values_equal_inner(a, b, &mut seen)
}

/// Is `a` the same slot value as `b`, decided **without locking any container**?
/// True when both are the same reference (same cell) or equal scalars.
///
/// The detached collection methods (audit A-7) resolve an index on a snapshot
/// taken with the receiver's mutex released, then commit under the lock. They
/// use this to confirm the slot still holds the snapshot's element: re-running
/// `values_equal` there could re-lock the receiver and hang.
pub fn same_slot(a: &Value, b: &Value) -> bool {
    if let (Some(x), Some(y)) = (ref_id(a), ref_id(b)) {
        return x == y;
    }
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Int(x), Value::Float(y)) => (*x as f64) == *y,
        (Value::Float(x), Value::Int(y)) => *x == (*y as f64),
        (Value::Str(x), Value::Str(y)) => x == y,
        _ => false,
    }
}

fn values_equal_inner(
    a: &Value,
    b: &Value,
    seen: &mut std::collections::HashSet<(u64, u64)>,
) -> bool {
    use Value::*;
    // Identity fast path: a reference value is equal to itself. Without this,
    // `xs == xs` locked the same non-reentrant Mutex twice and deadlocked.
    if let (Some(x), Some(y)) = (ref_id(a), ref_id(b)) {
        if x == y {
            return true;
        }
        let pair = if x <= y { (x, y) } else { (y, x) };
        if !seen.insert(pair) {
            return true;
        }
    }
    match (a, b) {
        (Null, Null) => true,
        (Bool(x), Bool(y)) => x == y,
        (Int(x), Int(y)) => x == y,
        (Float(x), Float(y)) => x == y,
        (Int(x), Float(y)) => (*x as f64) == *y,
        (Float(x), Int(y)) => *x == (*y as f64),
        (Str(x), Str(y)) => x == y,
        (Option(x), Option(y)) => match (x, y) {
            (Some(x), Some(y)) => values_equal_inner(x, y, seen),
            (None, None) => true,
            _ => false,
        },
        (Result(x), Result(y)) => match (x, y) {
            (Ok(x), Ok(y)) => values_equal_inner(x, y, seen),
            (Err(x), Err(y)) => values_equal_inner(x, y, seen),
            _ => false,
        },
        (List(x), List(y))
        | (VecDyn(x), VecDyn(y))
        | (List(x), VecDyn(y))
        | (VecDyn(x), List(y)) => {
            let xi = x.lock().unwrap().items.clone();
            let yi = y.lock().unwrap().items.clone();
            xi.len() == yi.len()
                && xi
                    .iter()
                    .zip(yi.iter())
                    .all(|(a, b)| values_equal_inner(a, b, seen))
        }
        (Stack(x), Stack(y)) => {
            let xi = x.lock().unwrap().items.clone();
            let yi = y.lock().unwrap().items.clone();
            xi.len() == yi.len()
                && xi
                    .iter()
                    .zip(yi.iter())
                    .all(|(a, b)| values_equal_inner(a, b, seen))
        }
        (Queue(x), Queue(y)) => {
            let xi = x.lock().unwrap().items.clone();
            let yi = y.lock().unwrap().items.clone();
            xi.len() == yi.len()
                && xi
                    .iter()
                    .zip(yi.iter())
                    .all(|(a, b)| values_equal_inner(a, b, seen))
        }
        (Deque(x), Deque(y)) => {
            let xi = x.lock().unwrap().items.clone();
            let yi = y.lock().unwrap().items.clone();
            xi.len() == yi.len()
                && xi
                    .iter()
                    .zip(yi.iter())
                    .all(|(a, b)| values_equal_inner(a, b, seen))
        }
        (Tuple(x), Tuple(y)) => {
            let xi = x.lock().unwrap().clone();
            let yi = y.lock().unwrap().clone();
            xi.len() == yi.len()
                && xi
                    .iter()
                    .zip(yi.iter())
                    .all(|(a, b)| values_equal_inner(a, b, seen))
        }
        (Map(x), Map(y)) => {
            let xe = x.lock().unwrap().entries.clone();
            let ye = y.lock().unwrap().entries.clone();
            if xe.len() != ye.len() {
                return false;
            }
            xe.iter().all(|(k, v)| {
                ye.iter()
                    .find(|(k2, _)| k2 == k)
                    .map(|(_, v2)| values_equal_inner(v, v2, seen))
                    .unwrap_or(false)
            })
        }
        (Set(x), Set(y)) => {
            let xi = x.lock().unwrap().items.clone();
            let yi = y.lock().unwrap().items.clone();
            xi.len() == yi.len()
                && xi
                    .iter()
                    .all(|v| yi.iter().any(|w| values_equal_inner(v, w, seen)))
        }
        (Vector(x), Vector(y)) => {
            let xi = x.lock().unwrap().data.clone();
            let yi = y.lock().unwrap().data.clone();
            xi == yi
        }
        (Bytes(x), Bytes(y)) => {
            let xi = x.lock().unwrap().clone();
            let yi = y.lock().unwrap().clone();
            xi == yi
        }
        (Matrix(x), Matrix(y)) => {
            let x = x.lock().unwrap();
            let y = y.lock().unwrap();
            x.rows == y.rows && x.cols == y.cols && x.data == y.data
        }
        (Instance(x), Instance(y)) => ptr_id(x) == ptr_id(y),
        (VInstance(x), VInstance(y)) => ptr_id(x) == ptr_id(y),
        (Function(x), Function(y)) => arc_id(x) == arc_id(y),
        (VFunc(x), VFunc(y)) => arc_id(x) == arc_id(y),
        (Class(x), Class(y)) => arc_id(x) == arc_id(y),
        (VClass(x), VClass(y)) => arc_id(x) == arc_id(y),
        (Task(x), Task(y)) => Arc::ptr_eq(x, y),
        (Channel(x), Channel(y)) => Arc::ptr_eq(x, y),
        (MutexVal(x), MutexVal(y)) => arc_id(x) == arc_id(y),
        (Database(x), Database(y)) => arc_id(x) == arc_id(y),
        (Cfn(x), Cfn(y)) => arc_id(x) == arc_id(y),
        (CClass(x), CClass(y)) => arc_id(x) == arc_id(y),
        (CInstance(x), CInstance(y)) => ptr_id(x) == ptr_id(y),
        _ => false,
    }
}

/// Truthiness: false, null, none, empty string, and numeric zero are false.
/// Everything else is true.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::Int(i) => *i != 0,
        Value::Float(f) => *f != 0.0,
        Value::Str(s) => !s.is_empty(),
        Value::Option(o) => o.is_some(),
        _ => true,
    }
}

pub fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Bool(_) => "bool",
        Value::Str(_) => "string",
        Value::Null => "null",
        Value::List(_) => "list",
        Value::VecDyn(_) => "vec",
        Value::Stack(_) => "stack",
        Value::Queue(_) => "queue",
        Value::Deque(_) => "deque",
        Value::LinkedList(_) => "linked_list",
        Value::SinglyList(_) => "singly_linked_list",
        Value::DoublyList(_) => "doubly_linked_list",
        Value::Map(_) => "map",
        Value::Set(_) => "set",
        Value::Tuple(_) => "tuple",
        Value::BinTree(_) => "binary_tree",
        Value::Bst(_) => "bst",
        Value::Heap(_) => "heap",
        Value::PrioQueue(_) => "priority_queue",
        Value::Graph(_) => "graph",
        Value::HashTable(_) => "hash_table",
        Value::Trie(_) => "trie",
        Value::Matrix(_) => "matrix",
        Value::Vector(_) => "vector",
        Value::Bytes(_) => "bytes",
        Value::Ring(_) => "ring_buffer",
        Value::Bag(_) => "bag",
        Value::RefBox(_) => "ref",
        Value::Option(_) => "option",
        Value::Result(_) => "result",
        Value::Function(_) => "function",
        Value::Native(_) => "function",
        Value::NativeClosure(_) => "function",
        Value::VFunc(_) => "function",
        Value::Class(_) => "class",
        Value::VClass(_) => "class",
        Value::Instance(_) => "instance",
        Value::VInstance(_) => "instance",
        Value::Task(_) => "task",
        Value::Channel(_) => "channel",
        Value::MutexVal(_) => "mutex",
        Value::Database(_) => "database",
        Value::Cfn(_) => "function",
        Value::CClass(_) => "class",
        Value::CInstance(_) => "instance",
        Value::Weak(_) => "weak",
    }
}

/// Whether the value has a length the `len(x)` builtin (or the `x.len()`
/// method) can report. Used only to decide whether the "no field" diagnostic
/// should point at the length helpers.
fn has_len(v: &Value) -> bool {
    matches!(
        v,
        Value::Str(_)
            | Value::List(_)
            | Value::VecDyn(_)
            | Value::Stack(_)
            | Value::Queue(_)
            | Value::Deque(_)
            | Value::LinkedList(_)
            | Value::SinglyList(_)
            | Value::DoublyList(_)
            | Value::Set(_)
            | Value::Tuple(_)
            | Value::BinTree(_)
            | Value::Bst(_)
            | Value::Heap(_)
            | Value::PrioQueue(_)
            | Value::HashTable(_)
            | Value::Trie(_)
            | Value::Matrix(_)
            | Value::Vector(_)
            | Value::Bytes(_)
            | Value::Ring(_)
            | Value::Bag(_)
    )
}

/// The diagnostic for a field read or write on a value that has no fields.
///
/// Shared by the tree engine and the VM so the wording can never diverge
/// between engines. Reading `xs.len` is the mistake newcomers make most
/// often — the length is the method `xs.len()` or the builtin `len(xs)` — so
/// container values get that pointer; every other value just names the field
/// that was not found.
pub fn no_field_error(value: &Value, field: &str) -> String {
    let t = type_name(value);
    if has_len(value) {
        format!(
            "{} has no field '{}' (use len(x) or x.len() for its length)",
            t, field
        )
    } else {
        format!("{} has no field '{}'", t, field)
    }
}

/// Checked integer arithmetic shared by every engine (the interpreter, the VM,
/// and the native backend's `spid3r_binop` all route through here). Integer
/// overflow is a runtime error, not a silent wrap: `9223372036854775807 + 1`
/// must not quietly become the smallest i64 (Python raises, PHP converts to
/// float - Spid3r previously corrupted the value, which the audit flagged).
pub fn checked_int(op: &'static str, a: i64, b: i64) -> Result<i64, String> {
    let r = match op {
        "+" => a.checked_add(b),
        "-" => a.checked_sub(b),
        "*" => a.checked_mul(b),
        _ => unreachable!("checked_int only covers +, -, *"),
    };
    r.ok_or_else(|| format!("integer overflow: {} {} {}", a, op, b))
}

/// Checked integer negation (`-i64::MIN` overflows).
pub fn checked_neg(a: i64) -> Result<i64, String> {
    a.checked_neg()
        .ok_or_else(|| format!("integer overflow: -{}", a))
}

/// Debug display used by `print`, error messages, and `string()`.
pub fn display_value(v: &Value) -> String {
    let mut seen = std::collections::HashSet::new();
    display_value_inner(v, &mut seen)
}

fn display_value_inner(v: &Value, seen: &mut std::collections::HashSet<u64>) -> String {
    if let Some(id) = ref_id(v) {
        if !seen.insert(id) {
            return match v {
                Value::List(_) | Value::VecDyn(_) => "[...]".to_string(),
                Value::Map(_) => "map{...}".to_string(),
                Value::Set(_) => "set{...}".to_string(),
                Value::Tuple(_) => "(...)".to_string(),
                _ => "...".to_string(),
            };
        }
    }
    let out = match v {
        Value::Int(i) => i.to_string(),
        Value::Float(f) => {
            if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
                format!("{:.1}", f)
            } else {
                format!("{}", f)
            }
        }
        Value::Bool(b) => b.to_string(),
        Value::Str(s) => s.to_string(),
        Value::Null => "null".to_string(),
        Value::Weak(_) => "weak".to_string(),
        Value::List(d) | Value::VecDyn(d) => {
            let items = d.lock().unwrap().items.clone();
            format!(
                "[{}]",
                items
                    .iter()
                    .map(|it| display_value_inner(it, seen))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::Stack(d) => {
            let raw = d.lock().unwrap().items.clone();
            let mut items: Vec<String> =
                raw.iter().map(|it| display_value_inner(it, seen)).collect();
            items.reverse(); // top first
            format!("stack({})", items.join(", "))
        }
        Value::Queue(d) => {
            let items: Vec<Value> = d.lock().unwrap().items.iter().cloned().collect();
            format!(
                "queue({})",
                items
                    .iter()
                    .map(|it| display_value_inner(it, seen))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::Deque(d) => {
            let items: Vec<Value> = d.lock().unwrap().items.iter().cloned().collect();
            format!(
                "deque({})",
                items
                    .iter()
                    .map(|it| display_value_inner(it, seen))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::LinkedList(d) | Value::SinglyList(d) | Value::DoublyList(d) => {
            let mut vals = Vec::new();
            let mut cur = d.lock().unwrap().head.clone();
            while let Some(n) = cur {
                let (val, next) = {
                    let node = n.lock().unwrap();
                    (node.value.clone(), node.next.clone())
                };
                vals.push(val);
                cur = next;
            }
            let out: Vec<String> = vals
                .iter()
                .map(|it| display_value_inner(it, seen))
                .collect();
            let kind = match v {
                Value::LinkedList(_) => "linked_list",
                Value::SinglyList(_) => "singly_linked_list",
                _ => "doubly_linked_list",
            };
            format!("{}[{}]", kind, out.join(", "))
        }
        Value::Map(d) => {
            let entries = d.lock().unwrap().entries.clone();
            // A value type may carry a precomputed display string
            // (`__display__`): bigint/decimal/rational/complex set it so they
            // print as "0.30" instead of their internal map.
            for (k, v) in &entries {
                if k.display() == "__display__" {
                    if let Value::Str(s) = v {
                        return s.to_string();
                    }
                }
            }
            // Keys starting with `__` are internal (operator methods such as
            // `__add__`); they are not part of the value's visible shape.
            let body = entries
                .iter()
                .filter(|(k, _)| !k.display().starts_with("__"))
                .map(|(k, v)| format!("{}: {}", k.display(), display_value_inner(v, seen)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("map{{{}}}", body)
        }
        Value::Set(d) => {
            let items = d.lock().unwrap().items.clone();
            let body = items
                .iter()
                .map(|it| display_value_inner(it, seen))
                .collect::<Vec<_>>()
                .join(", ");
            format!("set{{{}}}", body)
        }
        Value::Tuple(t) => {
            let items = t.lock().unwrap().clone();
            let body = items
                .iter()
                .map(|it| display_value_inner(it, seen))
                .collect::<Vec<_>>()
                .join(", ");
            format!("({})", body)
        }
        Value::BinTree(d) => {
            let items = d.lock().unwrap().inorder();
            format!(
                "binary_tree[{}]",
                items
                    .iter()
                    .map(|it| display_value_inner(it, seen))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::Bst(d) => {
            let items = d.lock().unwrap().inorder();
            format!(
                "bst[{}]",
                items
                    .iter()
                    .map(|it| display_value_inner(it, seen))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::Heap(d) => {
            let items = d.lock().unwrap().items.clone();
            format!(
                "heap[{}]",
                items
                    .iter()
                    .map(|it| display_value_inner(it, seen))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::PrioQueue(d) => {
            let pairs: Vec<(Value, f64)> = d
                .lock()
                .unwrap()
                .items
                .iter()
                .map(|it| (it.value.clone(), it.prio))
                .collect();
            let body = pairs
                .iter()
                .map(|(val, prio)| {
                    format!("{}@{}", display_value_inner(val, seen), display_prio(*prio))
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("priority_queue({})", body)
        }
        Value::Graph(d) => {
            let d = d.lock().unwrap();
            format!(
                "graph({} nodes, {} edges, {})",
                d.nodes.len(),
                d.edge_count(),
                if d.directed { "directed" } else { "undirected" }
            )
        }
        Value::HashTable(d) => {
            let d = d.lock().unwrap();
            format!(
                "hash_table({} entries, {} buckets)",
                d.size,
                d.buckets.len()
            )
        }
        Value::Trie(d) => {
            let d = d.lock().unwrap();
            format!("trie({} words)", d.count)
        }
        Value::Matrix(d) => {
            let (rows_n, cols_n, data) = {
                let d = d.lock().unwrap();
                (d.rows, d.cols, d.data.clone())
            };
            let rows = (0..rows_n)
                .map(|r| {
                    let row: Vec<String> = (0..cols_n)
                        .map(|c| display_value_inner(&Value::Float(data[r * cols_n + c]), seen))
                        .collect();
                    format!("[{}]", row.join(", "))
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("matrix[{}]", rows)
        }
        Value::Vector(d) => {
            let data = d.lock().unwrap().data.clone();
            let items: Vec<String> = data
                .iter()
                .map(|v| display_value_inner(&Value::Float(*v), seen))
                .collect();
            format!("vector[{}]", items.join(", "))
        }
        Value::Bytes(d) => {
            let len = d.lock().unwrap().len();
            format!("<bytes len={}>", len)
        }
        Value::Ring(d) => {
            let items: Vec<Value> = d.lock().unwrap().items.iter().cloned().collect();
            format!(
                "ring_buffer[{}]",
                items
                    .iter()
                    .map(|it| display_value_inner(it, seen))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::Bag(d) => {
            let counts = d.lock().unwrap().counts.clone();
            let body = counts
                .iter()
                .map(|(v, n)| format!("{}: {}", display_value_inner(v, seen), n))
                .collect::<Vec<_>>()
                .join(", ");
            format!("bag{{{}}}", body)
        }
        Value::RefBox(d) => {
            let inner = d.lock().unwrap().value.clone();
            format!("ref({})", display_value_inner(&inner, seen))
        }
        Value::Option(o) => match o {
            Some(v) => format!("some({})", display_value_inner(v, seen)),
            None => "none".to_string(),
        },
        Value::Result(r) => match r {
            Ok(v) => format!("ok({})", display_value_inner(v, seen)),
            Err(v) => format!("err({})", display_value_inner(v, seen)),
        },
        Value::Function(f) => match &f.proto.name {
            Some(n) => format!("<fn {}>", n),
            None => "<fn>".to_string(),
        },
        Value::VFunc(f) => match &f.name {
            Some(n) => format!("<vfn {}>", n),
            None => "<vfn>".to_string(),
        },
        Value::Native(n) => format!("<native {}>", n.name),
        Value::NativeClosure(n) => format!("<native {}>", n.name),
        Value::Class(c) => format!("<class {}>", c.name),
        Value::VClass(c) => format!("<vclass {}>", c.name),
        Value::Instance(i) => {
            let (cname, fields) = {
                let i = i.lock().unwrap();
                (i.class.name.clone(), i.fields.clone())
            };
            if fields.is_empty() {
                format!("{} {{}}", cname)
            } else {
                let body = fields
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, display_value_inner(v, seen)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} {{{}}}", cname, body)
            }
        }
        Value::VInstance(i) => {
            let (cname, fields) = {
                let i = i.lock().unwrap();
                (i.class.name.clone(), i.fields.clone())
            };
            if fields.is_empty() {
                format!("{} {{}}", cname)
            } else {
                let body = fields
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, display_value_inner(v, seen)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} {{{}}}", cname, body)
            }
        }
        Value::Task(t) => format!("<task {}>", t.id),
        Value::Channel(_) => "<channel>".to_string(),
        Value::MutexVal(_) => "<mutex>".to_string(),
        Value::Database(d) => {
            let path = d.lock().map(|g| g.path.clone()).unwrap_or_default();
            format!("<database {}>", path)
        }
        Value::Cfn(f) => match &f.name {
            Some(n) => format!("<fn {}>", n),
            None => "<fn>".to_string(),
        },
        Value::CClass(c) => format!("<class {}>", c.name),
        Value::CInstance(i) => {
            let (cname, fields) = {
                let i = i.lock().unwrap();
                (i.class.name.clone(), i.fields.clone())
            };
            if fields.is_empty() {
                format!("{} {{}}", cname)
            } else {
                let body = fields
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, display_value_inner(v, seen)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} {{{}}}", cname, body)
            }
        }
    };
    if let Some(id) = ref_id(v) {
        seen.remove(&id);
    }
    out
}

fn display_prio(p: f64) -> String {
    if p.fract() == 0.0 && p.abs() < 1e15 {
        format!("{:.1}", p)
    } else {
        format!("{}", p)
    }
}

/// Deep copy. Primitives copy; reference types are rebuilt so mutating the
/// clone never touches the original.
pub fn deep_clone(v: &Value) -> Value {
    let mut memo = std::collections::HashMap::new();
    deep_clone_inner(v, &mut memo)
}

/// Deep clone that preserves cycles (audit A-14).
///
/// `memo` maps the identity of a container to the **clone** standing in for it.
/// A back-reference therefore resolves to the new value, so the copy is a
/// cyclic copy rather than an alias of the original. The previous version kept
/// a `HashSet` of visited ids and returned `v.clone()` on a revisit, which meant
/// `a.push(a); b = a.clone()` left `b[0]` pointing at `a` — mutating through the
/// clone silently mutated the original.
///
/// Containers are registered *before* their children are cloned; that is what
/// lets a self-reference resolve to the half-built clone. It also means a value
/// reachable by two paths is shared in the copy rather than duplicated, which
/// is what `copy.deepcopy`-style semantics call for.
fn deep_clone_inner(v: &Value, memo: &mut std::collections::HashMap<u64, Value>) -> Value {
    use crate::value::collections as c;
    if let Some(id) = ref_id(v) {
        if let Some(done) = memo.get(&id) {
            return done.clone();
        }
    }
    // Publish `handle` as the clone of `v` before filling it in.
    macro_rules! register {
        ($handle:expr) => {
            if let Some(id) = ref_id(v) {
                memo.insert(id, $handle.clone());
            }
        };
    }
    match v {
        Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Str(_) | Value::Null => v.clone(),
        Value::Weak(_) => v.clone(),
        Value::List(d) => {
            let items = d.lock().unwrap().items.clone();
            let nd = new_ref(c::VecData::default());
            register!(Value::List(nd.clone()));
            let items: Vec<Value> = items.iter().map(|it| deep_clone_inner(it, memo)).collect();
            nd.lock().unwrap().items = items;
            Value::List(nd)
        }
        Value::VecDyn(d) => {
            let items = d.lock().unwrap().items.clone();
            let nd = new_ref(c::VecData::default());
            register!(Value::VecDyn(nd.clone()));
            let items: Vec<Value> = items.iter().map(|it| deep_clone_inner(it, memo)).collect();
            nd.lock().unwrap().items = items;
            Value::VecDyn(nd)
        }
        Value::Stack(d) => {
            let items = d.lock().unwrap().items.clone();
            let nd = new_ref(c::StackData::default());
            register!(Value::Stack(nd.clone()));
            let items: Vec<Value> = items.iter().map(|it| deep_clone_inner(it, memo)).collect();
            nd.lock().unwrap().items = items;
            Value::Stack(nd)
        }
        Value::Queue(d) => {
            let items: Vec<Value> = d.lock().unwrap().items.iter().cloned().collect();
            let nd = new_ref(c::QueueData::default());
            register!(Value::Queue(nd.clone()));
            let items: Vec<Value> = items.iter().map(|it| deep_clone_inner(it, memo)).collect();
            nd.lock().unwrap().items = items.into();
            Value::Queue(nd)
        }
        Value::Deque(d) => {
            let items: Vec<Value> = d.lock().unwrap().items.iter().cloned().collect();
            let nd = new_ref(c::DequeData::default());
            register!(Value::Deque(nd.clone()));
            let items: Vec<Value> = items.iter().map(|it| deep_clone_inner(it, memo)).collect();
            nd.lock().unwrap().items = items.into();
            Value::Deque(nd)
        }
        Value::LinkedList(_) | Value::SinglyList(_) | Value::DoublyList(_) => {
            let (head, kind) = match v {
                Value::LinkedList(d) => (d.lock().unwrap().head.clone(), 0),
                Value::SinglyList(d) => (d.lock().unwrap().head.clone(), 1),
                Value::DoublyList(d) => (d.lock().unwrap().head.clone(), 2),
                _ => unreachable!(),
            };
            let mut raw = Vec::new();
            let mut cur = head;
            while let Some(n) = cur {
                let (val, next) = {
                    let node = n.lock().unwrap();
                    (node.value.clone(), node.next.clone())
                };
                raw.push(val);
                cur = next;
            }
            let nd = new_ref(match kind {
                0 => c::ListData::linked(),
                1 => c::ListData::singly(),
                _ => c::ListData::doubly(),
            });
            let handle = match kind {
                0 => Value::LinkedList(nd.clone()),
                1 => Value::SinglyList(nd.clone()),
                _ => Value::DoublyList(nd.clone()),
            };
            register!(handle);
            let cloned: Vec<Value> = raw.iter().map(|it| deep_clone_inner(it, memo)).collect();
            {
                let mut w = nd.lock().unwrap();
                for it in cloned {
                    w.push_back(it);
                }
            }
            handle
        }
        Value::Map(d) => {
            let (entries, namespace, frozen) = {
                let guard = d.lock().unwrap();
                (guard.entries.clone(), guard.namespace, guard.frozen.clone())
            };
            let nd = new_ref(c::MapData::default());
            register!(Value::Map(nd.clone()));
            let cloned: Vec<(c::MapKey, Value)> = entries
                .iter()
                .map(|(k, vv)| (k.clone(), deep_clone_inner(vv, memo)))
                .collect();
            // A cloned module namespace stays a namespace (read-only, with
            // the same frozen constants).
            *nd.lock().unwrap() = if namespace {
                c::MapData::namespace(cloned, frozen)
            } else {
                c::MapData::from_entries(cloned)
            };
            Value::Map(nd)
        }
        Value::Set(d) => {
            let items = d.lock().unwrap().items.clone();
            let nd = new_ref(c::SetData::default());
            register!(Value::Set(nd.clone()));
            let items: Vec<Value> = items.iter().map(|it| deep_clone_inner(it, memo)).collect();
            nd.lock().unwrap().items = items;
            Value::Set(nd)
        }
        Value::Tuple(t) => {
            let items = t.lock().unwrap().clone();
            let nd = new_ref(Vec::<Value>::new());
            register!(Value::Tuple(nd.clone()));
            let items: Vec<Value> = items.iter().map(|it| deep_clone_inner(it, memo)).collect();
            *nd.lock().unwrap() = items;
            Value::Tuple(nd)
        }
        Value::Bst(d) => {
            let items = d.lock().unwrap().inorder();
            let nd = new_ref(c::BstData::new());
            register!(Value::Bst(nd.clone()));
            let cloned: Vec<Value> = items.iter().map(|it| deep_clone_inner(it, memo)).collect();
            {
                let mut w = nd.lock().unwrap();
                for it in cloned {
                    let _ = w.insert(it);
                }
            }
            Value::Bst(nd)
        }
        Value::BinTree(d) => {
            let items = d.lock().unwrap().level_order();
            let nd = new_ref(c::BinTreeData::new());
            register!(Value::BinTree(nd.clone()));
            let cloned: Vec<Value> = items.iter().map(|it| deep_clone_inner(it, memo)).collect();
            {
                let mut w = nd.lock().unwrap();
                for it in cloned {
                    let _ = w.insert(it);
                }
            }
            Value::BinTree(nd)
        }
        Value::Heap(d) => {
            let (items, min) = {
                let d = d.lock().unwrap();
                (d.items.clone(), d.min)
            };
            let nd = new_ref(c::HeapData {
                items: Vec::new(),
                min,
            });
            register!(Value::Heap(nd.clone()));
            let items: Vec<Value> = items.iter().map(|it| deep_clone_inner(it, memo)).collect();
            nd.lock().unwrap().items = items;
            Value::Heap(nd)
        }
        Value::PrioQueue(d) => {
            let (items, min) = {
                let d = d.lock().unwrap();
                (
                    d.items
                        .iter()
                        .map(|it| (it.value.clone(), it.prio))
                        .collect::<Vec<_>>(),
                    d.min,
                )
            };
            let nd = new_ref(c::PrioQueueData::new(min));
            register!(Value::PrioQueue(nd.clone()));
            let cloned: Vec<(Value, f64)> = items
                .iter()
                .map(|(val, prio)| (deep_clone_inner(val, memo), *prio))
                .collect();
            {
                let mut w = nd.lock().unwrap();
                for (val, prio) in cloned {
                    let _ = w.push(val, prio);
                }
            }
            Value::PrioQueue(nd)
        }
        Value::Graph(d) => {
            // Snapshot under the lock, then recurse with the lock released
            // (audit A-7): a node value can be the graph itself, and
            // `GraphData::deep_clone` would re-lock the same non-reentrant
            // mutex the caller holds.
            let (directed, nodes, adj) = {
                let g = d.lock().unwrap();
                (g.directed, g.nodes.clone(), g.adj.clone())
            };
            let nd = new_ref(c::GraphData {
                directed,
                nodes: Vec::new(),
                adj,
            });
            register!(Value::Graph(nd.clone()));
            let nodes: Vec<Value> = nodes.iter().map(|n| deep_clone_inner(n, memo)).collect();
            nd.lock().unwrap().nodes = nodes;
            Value::Graph(nd)
        }
        Value::HashTable(d) => {
            let (buckets, size) = {
                let d = d.lock().unwrap();
                (d.buckets.clone(), d.size)
            };
            let nd = new_ref(c::HashTableData {
                buckets: Vec::new(),
                size,
            });
            register!(Value::HashTable(nd.clone()));
            let cloned: Vec<Vec<(c::MapKey, Value)>> = buckets
                .iter()
                .map(|b| {
                    b.iter()
                        .map(|(k, vv)| (k.clone(), deep_clone_inner(vv, memo)))
                        .collect()
                })
                .collect();
            nd.lock().unwrap().buckets = cloned;
            Value::HashTable(nd)
        }
        Value::Trie(d) => {
            let words = d.lock().unwrap().all_words();
            let nd = new_ref(c::TrieData::new());
            register!(Value::Trie(nd.clone()));
            {
                let mut w = nd.lock().unwrap();
                for wd in words {
                    let _ = w.insert(&wd);
                }
            }
            Value::Trie(nd)
        }
        Value::Matrix(d) => {
            let d = d.lock().unwrap();
            Value::Matrix(new_ref(c::MatrixData {
                rows: d.rows,
                cols: d.cols,
                data: d.data.clone(),
            }))
        }
        Value::Vector(d) => {
            let d = d.lock().unwrap();
            Value::Vector(new_ref(c::VectorData {
                data: d.data.clone(),
            }))
        }
        Value::Bytes(d) => {
            let data = d.lock().unwrap().clone();
            Value::Bytes(new_ref(data))
        }
        Value::Ring(d) => {
            let (cap, items) = {
                let d = d.lock().unwrap();
                (d.cap, d.items.iter().cloned().collect::<Vec<_>>())
            };
            let nd = new_ref(c::RingData {
                cap,
                items: Default::default(),
            });
            register!(Value::Ring(nd.clone()));
            let items: Vec<Value> = items.iter().map(|it| deep_clone_inner(it, memo)).collect();
            nd.lock().unwrap().items = items.into();
            Value::Ring(nd)
        }
        Value::Bag(d) => {
            let counts = d.lock().unwrap().counts.clone();
            let nd = new_ref(c::BagData::default());
            register!(Value::Bag(nd.clone()));
            let counts: Vec<(Value, usize)> = counts
                .iter()
                .map(|(vv, n)| (deep_clone_inner(vv, memo), *n))
                .collect();
            nd.lock().unwrap().counts = counts;
            Value::Bag(nd)
        }
        Value::RefBox(d) => {
            let inner = d.lock().unwrap().value.clone();
            let nd = new_ref(c::RefBoxData { value: Value::Null });
            register!(Value::RefBox(nd.clone()));
            nd.lock().unwrap().value = deep_clone_inner(&inner, memo);
            Value::RefBox(nd)
        }
        Value::Instance(i) => {
            let (class, fields) = {
                let i = i.lock().unwrap();
                (i.class.clone(), i.fields.clone())
            };
            let nd = new_ref(InstanceData {
                class,
                fields: std::collections::HashMap::new(),
            });
            register!(Value::Instance(nd.clone()));
            let fields: std::collections::HashMap<String, Value> = fields
                .iter()
                .map(|(k, vv)| (k.clone(), deep_clone_inner(vv, memo)))
                .collect();
            nd.lock().unwrap().fields = fields;
            Value::Instance(nd)
        }
        Value::VInstance(i) => {
            let (class, fields) = {
                let i = i.lock().unwrap();
                (i.class.clone(), i.fields.clone())
            };
            let nd = new_ref(VmInstanceData {
                class,
                fields: std::collections::HashMap::new(),
            });
            register!(Value::VInstance(nd.clone()));
            let fields: std::collections::HashMap<String, Value> = fields
                .iter()
                .map(|(k, vv)| (k.clone(), deep_clone_inner(vv, memo)))
                .collect();
            nd.lock().unwrap().fields = fields;
            Value::VInstance(nd)
        }
        Value::CInstance(i) => {
            let (class, fields) = {
                let i = i.lock().unwrap();
                (i.class.clone(), i.fields.clone())
            };
            let nd = new_ref(CompiledInstanceData {
                class,
                fields: std::collections::HashMap::new(),
            });
            register!(Value::CInstance(nd.clone()));
            let fields: std::collections::HashMap<String, Value> = fields
                .iter()
                .map(|(k, vv)| (k.clone(), deep_clone_inner(vv, memo)))
                .collect();
            nd.lock().unwrap().fields = fields;
            Value::CInstance(nd)
        }
        // identity types: the clone aliases, matching reference semantics
        Value::Function(_)
        | Value::Native(_)
        | Value::NativeClosure(_)
        | Value::VFunc(_)
        | Value::Class(_)
        | Value::VClass(_)
        | Value::Cfn(_)
        | Value::CClass(_)
        | Value::Task(_)
        | Value::Channel(_)
        | Value::MutexVal(_)
        | Value::Database(_) => v.clone(),
        Value::Option(o) => Value::Option(o.as_ref().map(|v| Box::new(deep_clone_inner(v, memo)))),
        Value::Result(r) => Value::Result(match r {
            Ok(v) => Ok(Box::new(deep_clone_inner(v, memo))),
            Err(v) => Err(Box::new(deep_clone_inner(v, memo))),
        }),
    }
}
