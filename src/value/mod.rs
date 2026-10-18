//! The Spid3r value model. Reference types are `Arc<Mutex<_>>` so values can
//! cross task boundaries; assignment aliases, `clone()` deep-copies.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::os::raw::c_void;
use std::any::Any;
use std::sync::{Arc, Mutex};

use crate::ast::{FnProto, Vis};
use crate::environment::Env;

pub mod collections;
pub mod gc;

pub type Ref<T> = Arc<Mutex<T>>;

pub fn new_ref<T>(v: T) -> Ref<T> {
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
pub struct VmFunc {
    pub name: Option<String>,
    pub arity: usize,
    pub is_async: bool,
    pub chunk: std::sync::Arc<crate::vm::bytecode::Chunk>,
    pub upvalues: Vec<UpvalueCell>,
    pub own_upvalues: u8,
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

/// Keys allowed by `map`, `set`, `bag`, `hash_table`, and graph node ids:
/// null, bool, int, string. Floats are rejected on purpose.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MapKey {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
}

impl MapKey {
    pub fn display(&self) -> String {
        match self {
            MapKey::Null => "null".to_string(),
            MapKey::Bool(b) => b.to_string(),
            MapKey::Int(i) => i.to_string(),
            MapKey::Str(s) => s.clone(),
        }
    }
}

pub fn as_map_key(v: &Value) -> Result<MapKey, String> {
    match v {
        Value::Null => Ok(MapKey::Null),
        Value::Bool(b) => Ok(MapKey::Bool(*b)),
        Value::Int(i) => Ok(MapKey::Int(*i)),
        Value::Str(s) => Ok(MapKey::Str(s.to_string())),
        other => Err(format!(
            "{} is not a valid key; keys must be null, bool, int, or string",
            type_name(other)
        )),
    }
}

pub fn key_to_value(k: &MapKey) -> Value {
    match k {
        MapKey::Null => Value::Null,
        MapKey::Bool(b) => Value::Bool(*b),
        MapKey::Int(i) => Value::Int(*i),
        MapKey::Str(s) => Value::Str(s.as_str().into()),
    }
}

/// A user-defined function: its AST prototype plus the environment it closed
/// over. Closures capture the defining scope, same as the reference runner.
pub struct FunctionData {
    pub proto: FnProto,
    pub env: Env,
}

/// Native (Rust) functions exposed to Spid3r.
pub struct NativeFn {
    pub name: &'static str,
    pub func: fn(&NativeCtx, Vec<Value>) -> Result<Value, String>,
}

pub struct NativeCtx {
    pub sched: Arc<crate::sched::Scheduler>,
}

pub struct ClassData {
    pub name: String,
    pub parent: Option<Arc<ClassData>>,
    pub interfaces: Vec<String>,
    pub fields: Vec<(String, crate::ast::Vis)>,
    pub methods: std::collections::HashMap<String, (crate::ast::Vis, Arc<FunctionData>)>,
    pub ctor: Option<Arc<FunctionData>>,
}

pub struct InstanceData {
    pub class: Arc<ClassData>,
    pub fields: std::collections::HashMap<String, Value>,
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
    pub code: unsafe extern "C" fn(*mut c_void, usize, *const *mut Value) -> *mut Value,
    pub env: *mut c_void,
}

// SAFETY: the `env` allocation is only ever touched by the single thread that
// created the closure; the code pointer is static. Spid3r values are otherwise
// freely `Send`, so keep the compiled-function handle `Send`/`Sync` too.
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
    Ring(Ref<crate::value::collections::RingData>),
    Bag(Ref<crate::value::collections::BagData>),
    RefBox(Ref<crate::value::collections::RefBoxData>),
    Option(Option<Box<Value>>),
    Result(std::result::Result<Box<Value>, Box<Value>>),

    Function(Arc<FunctionData>),
    Native(Arc<NativeFn>),

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
    MutexVal(Ref<crate::sched::MutexData>),

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
    Ok(Value::Weak(new_ref(WeakCell { w, kind })))
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
        BinTree => Value::BinTree(downcast_cell::<crate::value::collections::BinTreeData>(&any)?),
        Bst => Value::Bst(downcast_cell::<crate::value::collections::BstData>(&any)?),
        Heap => Value::Heap(downcast_cell::<crate::value::collections::HeapData>(&any)?),
        PrioQueue => {
            Value::PrioQueue(downcast_cell::<crate::value::collections::PrioQueueData>(&any)?)
        }
        Graph => Value::Graph(downcast_cell::<crate::value::collections::GraphData>(&any)?),
        HashTable => {
            Value::HashTable(downcast_cell::<crate::value::collections::HashTableData>(&any)?)
        }
        Trie => Value::Trie(downcast_cell::<crate::value::collections::TrieData>(&any)?),
        Matrix => Value::Matrix(downcast_cell::<crate::value::collections::MatrixData>(&any)?),
        Ring => Value::Ring(downcast_cell::<crate::value::collections::RingData>(&any)?),
        Bag => Value::Bag(downcast_cell::<crate::value::collections::BagData>(&any)?),
        RefBox => Value::RefBox(downcast_cell::<crate::value::collections::RefBoxData>(&any)?),
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
        Err(_) => Err(
            "weak reference target has an unexpected type (internal error)".to_string(),
        ),
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
        Value::Ring(r) => ref_id_of!(r),
        Value::Bag(r) => ref_id_of!(r),
        Value::RefBox(r) => ref_id_of!(r),
        Value::Instance(r) => ref_id_of!(r),
        Value::VInstance(r) => ref_id_of!(r),
        Value::CInstance(r) => ref_id_of!(r),
        Value::MutexVal(r) => ref_id_of!(r),
        Value::Function(f) => ref_id_of!(f),
        Value::Native(f) => ref_id_of!(f),
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
    use Value::*;
    // Identity fast path: a reference value is equal to itself. Without this,
    // `xs == xs` locked the same non-reentrant Mutex twice and deadlocked.
    if let (Some(x), Some(y)) = (ref_id(a), ref_id(b)) {
        if x == y {
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
            (Some(x), Some(y)) => values_equal(x, y),
            (None, None) => true,
            _ => false,
        },
        (Result(x), Result(y)) => match (x, y) {
            (Ok(x), Ok(y)) => values_equal(x, y),
            (Err(x), Err(y)) => values_equal(x, y),
            _ => false,
        },
        (List(x), List(y))
        | (VecDyn(x), VecDyn(y))
        | (List(x), VecDyn(y))
        | (VecDyn(x), List(y)) => {
            let x = x.lock().unwrap();
            let y = y.lock().unwrap();
            x.items.len() == y.items.len()
                && x.items
                    .iter()
                    .zip(y.items.iter())
                    .all(|(a, b)| values_equal(a, b))
        }
        (Stack(x), Stack(y)) => {
            let x = x.lock().unwrap();
            let y = y.lock().unwrap();
            x.items.len() == y.items.len()
                && x.items
                    .iter()
                    .zip(y.items.iter())
                    .all(|(a, b)| values_equal(a, b))
        }
        (Queue(x), Queue(y)) => {
            let x = x.lock().unwrap();
            let y = y.lock().unwrap();
            x.items.len() == y.items.len()
                && x.items
                    .iter()
                    .zip(y.items.iter())
                    .all(|(a, b)| values_equal(a, b))
        }
        (Deque(x), Deque(y)) => {
            let x = x.lock().unwrap();
            let y = y.lock().unwrap();
            x.items.len() == y.items.len()
                && x.items
                    .iter()
                    .zip(y.items.iter())
                    .all(|(a, b)| values_equal(a, b))
        }
        (Tuple(x), Tuple(y)) => {
            let x = x.lock().unwrap();
            let y = y.lock().unwrap();
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| values_equal(a, b))
        }
        (Map(x), Map(y)) => {
            let x = x.lock().unwrap();
            let y = y.lock().unwrap();
            if x.entries.len() != y.entries.len() {
                return false;
            }
            x.entries.iter().all(|(k, v)| {
                y.entries
                    .iter()
                    .find(|(k2, _)| k2 == k)
                    .map(|(_, v2)| values_equal(v, v2))
                    .unwrap_or(false)
            })
        }
        (Set(x), Set(y)) => {
            let x = x.lock().unwrap();
            let y = y.lock().unwrap();
            x.items.len() == y.items.len()
                && x.items
                    .iter()
                    .all(|v| y.items.iter().any(|w| values_equal(v, w)))
        }
        (Instance(x), Instance(y)) => ptr_id(x) == ptr_id(y),
        (VInstance(x), VInstance(y)) => ptr_id(x) == ptr_id(y),
        (Function(x), Function(y)) => arc_id(x) == arc_id(y),
        (VFunc(x), VFunc(y)) => arc_id(x) == arc_id(y),
        (Class(x), Class(y)) => arc_id(x) == arc_id(y),
        (VClass(x), VClass(y)) => arc_id(x) == arc_id(y),
        (Task(x), Task(y)) => Arc::ptr_eq(x, y),
        (Channel(x), Channel(y)) => Arc::ptr_eq(x, y),
        (MutexVal(x), MutexVal(y)) => ptr_id(x) == ptr_id(y),
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
        Value::Ring(_) => "ring_buffer",
        Value::Bag(_) => "bag",
        Value::RefBox(_) => "ref",
        Value::Option(_) => "option",
        Value::Result(_) => "result",
        Value::Function(_) => "function",
        Value::Native(_) => "function",
        Value::VFunc(_) => "function",
        Value::Class(_) => "class",
        Value::VClass(_) => "class",
        Value::Instance(_) => "instance",
        Value::VInstance(_) => "instance",
        Value::Task(_) => "task",
        Value::Channel(_) => "channel",
        Value::MutexVal(_) => "mutex",
        Value::Cfn(_) => "function",
        Value::CClass(_) => "class",
        Value::CInstance(_) => "instance",
        Value::Weak(_) => "weak",
    }
}

/// Debug display used by `print`, error messages, and `string()`.
pub fn display_value(v: &Value) -> String {
    match v {
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
            let d = d.lock().unwrap();
            format!(
                "[{}]",
                d.items
                    .iter()
                    .map(display_value)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::Stack(d) => {
            let d = d.lock().unwrap();
            let mut items: Vec<String> = d.items.iter().map(display_value).collect();
            items.reverse(); // top first
            format!("stack({})", items.join(", "))
        }
        Value::Queue(d) => {
            let d = d.lock().unwrap();
            format!(
                "queue({})",
                d.items
                    .iter()
                    .map(display_value)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::Deque(d) => {
            let d = d.lock().unwrap();
            format!(
                "deque({})",
                d.items
                    .iter()
                    .map(display_value)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::LinkedList(d) | Value::SinglyList(d) | Value::DoublyList(d) => {
            let d = d.lock().unwrap();
            let mut out = Vec::new();
            let mut cur = d.head.clone();
            while let Some(n) = cur {
                let node = n.lock().unwrap();
                out.push(display_value(&node.value));
                cur = node.next.clone();
            }
            let kind = match v {
                Value::LinkedList(_) => "linked_list",
                Value::SinglyList(_) => "singly_linked_list",
                _ => "doubly_linked_list",
            };
            format!("{}[{}]", kind, out.join(", "))
        }
        Value::Map(d) => {
            let d = d.lock().unwrap();
            let body = d
                .entries
                .iter()
                .map(|(k, v)| format!("{}: {}", k.display(), display_value(v)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("map{{{}}}", body)
        }
        Value::Set(d) => {
            let d = d.lock().unwrap();
            let body = d
                .items
                .iter()
                .map(display_value)
                .collect::<Vec<_>>()
                .join(", ");
            format!("set{{{}}}", body)
        }
        Value::Tuple(t) => {
            let t = t.lock().unwrap();
            let body = t.iter().map(display_value).collect::<Vec<_>>().join(", ");
            format!("({})", body)
        }
        Value::BinTree(d) => {
            let d = d.lock().unwrap();
            format!(
                "binary_tree[{}]",
                d.inorder()
                    .iter()
                    .map(display_value)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::Bst(d) => {
            let d = d.lock().unwrap();
            format!(
                "bst[{}]",
                d.inorder()
                    .iter()
                    .map(display_value)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::Heap(d) => {
            let d = d.lock().unwrap();
            format!(
                "heap[{}]",
                d.items
                    .iter()
                    .map(display_value)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::PrioQueue(d) => {
            let d = d.lock().unwrap();
            let body = d
                .items
                .iter()
                .map(|it| format!("{}@{}", display_value(&it.value), display_prio(it.prio)))
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
            let d = d.lock().unwrap();
            let rows = (0..d.rows)
                .map(|r| {
                    let row: Vec<String> = (0..d.cols)
                        .map(|c| display_value(&Value::Float(d.data[r * d.cols + c])))
                        .collect();
                    format!("[{}]", row.join(", "))
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("matrix[{}]", rows)
        }
        Value::Ring(d) => {
            let d = d.lock().unwrap();
            format!(
                "ring_buffer[{}]",
                d.items
                    .iter()
                    .map(display_value)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::Bag(d) => {
            let d = d.lock().unwrap();
            let body = d
                .counts
                .iter()
                .map(|(v, n)| format!("{}: {}", display_value(v), n))
                .collect::<Vec<_>>()
                .join(", ");
            format!("bag{{{}}}", body)
        }
        Value::RefBox(d) => {
            let d = d.lock().unwrap();
            format!("ref({})", display_value(&d.value))
        }
        Value::Option(o) => match o {
            Some(v) => format!("some({})", display_value(v)),
            None => "none".to_string(),
        },
        Value::Result(r) => match r {
            Ok(v) => format!("ok({})", display_value(v)),
            Err(v) => format!("err({})", display_value(v)),
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
        Value::Class(c) => format!("<class {}>", c.name),
        Value::VClass(c) => format!("<vclass {}>", c.name),
        Value::Instance(i) => {
            let i = i.lock().unwrap();
            if i.fields.is_empty() {
                format!("{} {{}}", i.class.name)
            } else {
                let body = i
                    .fields
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, display_value(v)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} {{{}}}", i.class.name, body)
            }
        }
        Value::VInstance(i) => {
            let i = i.lock().unwrap();
            if i.fields.is_empty() {
                format!("{} {{}}", i.class.name)
            } else {
                let body = i
                    .fields
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, display_value(v)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} {{{}}}", i.class.name, body)
            }
        }
        Value::Task(t) => format!("<task {}>", t.id),
        Value::Channel(_) => "<channel>".to_string(),
        Value::MutexVal(_) => "<mutex>".to_string(),
        Value::Cfn(f) => match &f.name {
            Some(n) => format!("<fn {}>", n),
            None => "<fn>".to_string(),
        },
        Value::CClass(c) => format!("<class {}>", c.name),
        Value::CInstance(i) => {
            let i = i.lock().unwrap();
            if i.fields.is_empty() {
                format!("{} {{}}", i.class.name)
            } else {
                let body = i
                    .fields
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, display_value(v)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} {{{}}}", i.class.name, body)
            }
        }
    }
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
    use crate::value::collections as c;
    match v {
        Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Str(_) | Value::Null => v.clone(),
        Value::Weak(_) => v.clone(),
        Value::List(d) => {
            let d = d.lock().unwrap();
            Value::List(new_ref(c::VecData {
                items: d.items.iter().map(deep_clone).collect(),
            }))
        }
        Value::VecDyn(d) => {
            let d = d.lock().unwrap();
            Value::VecDyn(new_ref(c::VecData {
                items: d.items.iter().map(deep_clone).collect(),
            }))
        }
        Value::Stack(d) => {
            let d = d.lock().unwrap();
            Value::Stack(new_ref(c::StackData {
                items: d.items.iter().map(deep_clone).collect(),
            }))
        }
        Value::Queue(d) => {
            let d = d.lock().unwrap();
            Value::Queue(new_ref(c::QueueData {
                items: d.items.iter().map(deep_clone).collect(),
            }))
        }
        Value::Deque(d) => {
            let d = d.lock().unwrap();
            Value::Deque(new_ref(c::DequeData {
                items: d.items.iter().map(deep_clone).collect(),
            }))
        }
        Value::LinkedList(_) | Value::SinglyList(_) | Value::DoublyList(_) => {
            let (d, kind) = match v {
                Value::LinkedList(d) => (d.lock().unwrap(), 0),
                Value::SinglyList(d) => (d.lock().unwrap(), 1),
                Value::DoublyList(d) => (d.lock().unwrap(), 2),
                _ => unreachable!(),
            };
            let mut items = Vec::new();
            let mut cur = d.head.clone();
            while let Some(n) = cur {
                let node = n.lock().unwrap();
                items.push(deep_clone(&node.value));
                cur = node.next.clone();
            }
            drop(d);
            let mut nd = match kind {
                0 => c::ListData::linked(),
                1 => c::ListData::singly(),
                _ => c::ListData::doubly(),
            };
            for it in items {
                nd.push_back(it);
            }
            match kind {
                0 => Value::LinkedList(new_ref(nd)),
                1 => Value::SinglyList(new_ref(nd)),
                _ => Value::DoublyList(new_ref(nd)),
            }
        }
        Value::Map(d) => {
            let d = d.lock().unwrap();
            Value::Map(new_ref(c::MapData::from_entries(
                d.entries
                    .iter()
                    .map(|(k, v)| (k.clone(), deep_clone(v)))
                    .collect(),
            )))
        }
        Value::Set(d) => {
            let d = d.lock().unwrap();
            Value::Set(new_ref(c::SetData {
                items: d.items.iter().map(deep_clone).collect(),
            }))
        }
        Value::Tuple(t) => {
            let t = t.lock().unwrap();
            Value::Tuple(new_ref(t.iter().map(deep_clone).collect()))
        }
        Value::Bst(d) => {
            let d = d.lock().unwrap();
            let mut nd = c::BstData::new();
            for it in d.inorder() {
                let _ = nd.insert(it);
            }
            Value::Bst(new_ref(nd))
        }
        Value::BinTree(d) => {
            let d = d.lock().unwrap();
            let mut nd = c::BinTreeData::new();
            for it in d.level_order() {
                let _ = nd.insert(it);
            }
            Value::BinTree(new_ref(nd))
        }
        Value::Heap(d) => {
            let d = d.lock().unwrap();
            Value::Heap(new_ref(c::HeapData {
                items: d.items.iter().map(deep_clone).collect(),
                min: d.min,
            }))
        }
        Value::PrioQueue(d) => {
            let d = d.lock().unwrap();
            let mut nd = c::PrioQueueData::new(d.min);
            for it in &d.items {
                let _ = nd.push(it.value.clone(), it.prio);
            }
            Value::PrioQueue(new_ref(nd))
        }
        Value::Graph(d) => {
            let d = d.lock().unwrap();
            Value::Graph(new_ref(d.deep_clone()))
        }
        Value::HashTable(d) => {
            let d = d.lock().unwrap();
            Value::HashTable(new_ref(c::HashTableData {
                buckets: d
                    .buckets
                    .iter()
                    .map(|b| b.iter().map(|(k, v)| (k.clone(), deep_clone(v))).collect())
                    .collect(),
                size: d.size,
            }))
        }
        Value::Trie(d) => {
            let words = d.lock().unwrap().all_words();
            let mut nd = c::TrieData::new();
            for w in words {
                let _ = nd.insert(&w);
            }
            Value::Trie(new_ref(nd))
        }
        Value::Matrix(d) => {
            let d = d.lock().unwrap();
            Value::Matrix(new_ref(c::MatrixData {
                rows: d.rows,
                cols: d.cols,
                data: d.data.clone(),
            }))
        }
        Value::Ring(d) => {
            let d = d.lock().unwrap();
            Value::Ring(new_ref(c::RingData {
                cap: d.cap,
                items: d.items.iter().map(deep_clone).collect(),
            }))
        }
        Value::Bag(d) => {
            let d = d.lock().unwrap();
            Value::Bag(new_ref(c::BagData {
                counts: d.counts.iter().map(|(v, n)| (deep_clone(v), *n)).collect(),
            }))
        }
        Value::RefBox(d) => {
            let d = d.lock().unwrap();
            Value::RefBox(new_ref(c::RefBoxData {
                value: deep_clone(&d.value),
            }))
        }
        Value::Instance(i) => {
            let i = i.lock().unwrap();
            Value::Instance(new_ref(InstanceData {
                class: i.class.clone(),
                fields: i
                    .fields
                    .iter()
                    .map(|(k, v)| (k.clone(), deep_clone(v)))
                    .collect(),
            }))
        }
        // identity types: the clone aliases, matching reference semantics
        Value::Function(_)
        | Value::Native(_)
        | Value::VFunc(_)
        | Value::Class(_)
        | Value::VClass(_)
        | Value::VInstance(_)
        | Value::CInstance(_)
        | Value::Cfn(_)
        | Value::CClass(_)
        | Value::Task(_)
        | Value::Channel(_)
        | Value::MutexVal(_) => v.clone(),
        Value::Option(o) => Value::Option(o.as_ref().map(|v| Box::new(deep_clone(v)))),
        Value::Result(r) => Value::Result(match r {
            Ok(v) => Ok(Box::new(deep_clone(v))),
            Err(v) => Err(Box::new(deep_clone(v))),
        }),
    }
}
