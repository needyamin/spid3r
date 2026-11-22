//! Runtime for AOT-compiled Spid3r binaries (the LLVM backend's `native` half).
//!
//! Generated machine code never touches Rust types directly: it calls the
//! `extern "C"` trampolines below with *opaque* `*mut Value` handles. This
//! module owns
//!
//! - the global-variable table (natives + user globals + classes),
//! - value construction and operators (delegating to the exact same
//!   `vm_eval_binary`/`vm_eval_unary`/`values_equal`/`truthy` code the VM uses,
//!   so semantics stay bit-identical),
//! - calls: natively compiled functions (`Value::Cfn`), builtin collection
//!   constructors, native stdlib functions, and natively compiled classes,
//! - collections/methods/indexing/for-in, delegating to
//!   `value::collections::*` like the VM does,
//! - process startup (`spid3r_rt_start`) which installs the stdlib, runs the
//!   generated `spid3r_entry`, then calls `main`.
//!
//! Memory strategy: values are heap boxes that live for the whole process
//! (documented in docs/NATIVE.md). Pointers are immutable handles; mutation
//! always happens through interior mutability (`Arc<Mutex<_>>`), so aliasing
//! is harmless. Closures capture variables as `UpvalueCell`s — the exact
//! mechanism the bytecode VM uses.

// The trampolines below are an FFI boundary, not a general-purpose API: their
// raw-pointer arguments come from LLVM-generated machine code (or from this
// crate's own tests), never from arbitrary Rust callers, and every
// dereference is defensively validated where a check is possible (`deref` and
// `cstr` null-check and abort cleanly instead of faulting). That is why they
// are safe functions rather than `unsafe` ones — marking the ABI `unsafe`
// would ripple through every generated call site for no mechanical benefit.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::io::Write;
use std::os::raw::{c_char, c_void};
use std::sync::{Mutex, OnceLock};

use crate::ast::{BinOp, UnOp};
use crate::sched::Scheduler;
use crate::value::collections;
use crate::value::{new_ref, CompiledFn, CompiledInstanceData, NativeCtx, Value, ValueV2};
use crate::vm::{vm_eval_binary, vm_eval_unary};

// ----------------------------------------------------------------------------
// global variable table (natives + user globals + classes)
// ----------------------------------------------------------------------------

fn globals() -> &'static Mutex<HashMap<String, Value>> {
    static G: OnceLock<Mutex<HashMap<String, Value>>> = OnceLock::new();
    G.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Runtime error: print like the VM and exit with a non-zero status.
/// (`*mut Value` cannot express `Result`, so runtime failures abort.)
fn abort(msg: &str) -> ! {
    let _ = std::io::stdout().flush();
    eprintln!("error[runtime]: {}", msg);
    std::process::exit(70);
}

/// Read a 16-byte `ValueV2` from a caller stack or pinned slot. Aborts cleanly on null.
#[inline(always)]
fn read_v2(p: *const ValueV2) -> ValueV2 {
    if p.is_null() {
        abort("native runtime: null value pointer");
    }
    // SAFETY: `p` points to a valid 16-byte `ValueV2` slot (caller stack or pinned global).
    unsafe { *p }
}

/// Write a 16-byte `ValueV2` into the caller-allocated `out` slot.
#[inline(always)]
fn write_out(out: *mut ValueV2, v: ValueV2) {
    if !out.is_null() {
        // SAFETY: `out` is a caller-allocated 16-byte `ValueV2` stack slot.
        unsafe { out.write(v) };
    }
}

/// Convert a `ValueV2` into an owned Rust `Value` (cloning heap objects).
#[inline]
fn v2_to_value(v: ValueV2) -> Value {
    match v.tag {
        ValueV2::TAG_NULL => Value::Null,
        ValueV2::TAG_BOOL => Value::Bool(v.payload != 0),
        ValueV2::TAG_INT => Value::Int(v.payload as i64),
        ValueV2::TAG_FLOAT => Value::Float(f64::from_bits(v.payload)),
        ValueV2::TAG_HEAP => {
            let ptr = v.as_heap_ptr();
            if ptr.is_null() {
                abort("native runtime: null heap value pointer");
            }
            // SAFETY: `ptr` is a live `AOT_HEAP` or pinned `Box<Value>`.
            unsafe { (*ptr).clone() }
        }
        ValueV2::TAG_ROPE | ValueV2::TAG_ROPE_ASCII => {
            with_rope_str(v, |s| Value::Str(crate::value::ThinArcStr::from(s)))
                .unwrap_or_else(|| abort("native runtime: stale rope reference"))
        }
        _ if v.is_sso() => v
            .with_sso_str(|s| Value::Str(crate::value::ThinArcStr::from(s)))
            .unwrap_or_else(|| abort("native runtime: invalid SSO UTF-8")),
        other => abort(&format!("native runtime: invalid ValueV2 tag {}", other)),
    }
}

/// Convert a `*const ValueV2` into an owned Rust `Value`.
#[inline]
fn to_owned_v2(p: *const ValueV2) -> Value {
    v2_to_value(read_v2(p))
}

/// Borrow the underlying `&Value` when `v` is a `TAG_HEAP` value.
#[inline]
fn deref_heap<'a>(v: ValueV2) -> Option<&'a Value> {
    if v.tag == ValueV2::TAG_HEAP {
        let ptr = v.as_heap_ptr();
        if ptr.is_null() {
            abort("native runtime: null heap value pointer");
        }
        // SAFETY: `ptr` is a live `AOT_HEAP` or pinned `Box<Value>`.
        Some(unsafe { &*ptr })
    } else {
        None
    }
}

/// Heap collection threshold (number of live thread-local `TAG_HEAP` boxes
/// before `spid3r_gc_safepoint` sweeps unreachable objects).
pub const ARENA_SLAB_LEN: usize = 4096;

/// Per-call and process-wide allocation accounting (R4.0 / DP-1 Value v2).
/// Under `ValueV2`, `slabs_allocated` is always 0 (no leaked slabs) and
/// `*_call_allocs` counts `TAG_HEAP` `Box<Value>` allocations (0 for pure scalars).
#[derive(Debug, Clone, Copy, Default)]
pub struct SlabAllocStats {
    pub slabs_allocated: u64,
    pub current_call_allocs: u64,
    pub last_call_allocs: u64,
    pub max_call_allocs: u64,
    pub completed_call_allocs: u64,
}

static SLAB_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LAST_CALL_SLAB_ALLOCS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static MAX_CALL_SLAB_ALLOCS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static COMPLETED_CALL_SLAB_ALLOCS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

struct AotHeapState {
    live: Vec<*mut Value>,
    free: Vec<*mut Value>,
}

impl AotHeapState {
    const fn new() -> Self {
        Self {
            live: Vec::new(),
            free: Vec::new(),
        }
    }
}

const ROPE_RING_LEN: usize = 8;

struct RopeSlot {
    epoch: u32,
    buf: String,
    ascii: bool,
}

impl RopeSlot {
    const fn new() -> Self {
        Self {
            epoch: 0,
            buf: String::new(),
            ascii: false,
        }
    }
}

struct AotStrRope {
    slots: [RopeSlot; ROPE_RING_LEN],
    active: usize,
    next_epoch: u32,
    history: crate::value::collections::FxHashMap<u32, std::sync::Arc<str>>,
}

impl AotStrRope {
    fn new() -> Self {
        Self {
            slots: [const { RopeSlot::new() }; ROPE_RING_LEN],
            active: 0,
            next_epoch: 0,
            history: crate::value::collections::FxHashMap::default(),
        }
    }

    #[inline]
    fn lookup_slice(&self, ep: u32, len: usize) -> Option<&str> {
        for slot in &self.slots {
            if slot.epoch == ep {
                return slot.buf.get(..len);
            }
        }
        self.history.get(&ep).and_then(|s| s.get(..len))
    }
}

thread_local! {
    static CALL_SLAB_ALLOCS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static CALL_ALLOC_STACK: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
    /// All reclaimable heap `Box<Value>` pointers allocated on this thread plus
    /// recycled dead boxes (`free`) reused in LIFO order by `alloc_heap_v2`.
    static AOT_HEAP: RefCell<AotHeapState> = const { RefCell::new(AotHeapState::new()) };
    /// Thread-local 8-slot ring buffer + overflow history for `ValueV2::TAG_ROPE` /
    /// `ValueV2::TAG_ROPE_ASCII`: reuses ring buffers in-place with zero heap
    /// allocation when overwritten epochs are no longer rooted in `ROOT_FRAMES`.
    static AOT_ROPE: RefCell<AotStrRope> = RefCell::new(AotStrRope::new());
    /// Active compiled function frame root arrays: `(base_ptr, num_slots)`.
    static ROOT_FRAMES: RefCell<Vec<(*const ValueV2, usize)>> = const { RefCell::new(Vec::new()) };
    /// Pinned static string literal cache keyed by `(ptr_addr, byte_len)`.
    static STATIC_STR_CACHE: RefCell<crate::value::collections::FxHashMap<(usize, usize), *mut Value>> =
        RefCell::new(crate::value::collections::FxHashMap::default());
}

/// Snapshot of allocation accounting counters (current thread + process-wide).
pub fn slab_alloc_stats() -> SlabAllocStats {
    SlabAllocStats {
        slabs_allocated: SLAB_COUNT.load(std::sync::atomic::Ordering::Relaxed),
        current_call_allocs: CALL_SLAB_ALLOCS.with(|c| c.get()),
        last_call_allocs: LAST_CALL_SLAB_ALLOCS.load(std::sync::atomic::Ordering::Relaxed),
        max_call_allocs: MAX_CALL_SLAB_ALLOCS.load(std::sync::atomic::Ordering::Relaxed),
        completed_call_allocs: COMPLETED_CALL_SLAB_ALLOCS
            .load(std::sync::atomic::Ordering::Relaxed),
    }
}

#[inline]
fn is_rope_epoch_rooted(ep: u32) -> bool {
    ROOT_FRAMES.with(|rf| {
        let frames = rf.borrow();
        if frames.is_empty() {
            // Direct Rust unit tests/callbacks without frame registration archive safely.
            return true;
        }
        for &(base, len) in frames.iter() {
            // SAFETY: registered frame roots point to live stack `[len x ValueV2]`.
            let slice = unsafe { std::slice::from_raw_parts(base, len) };
            for v in slice {
                if v.is_rope() && v.rope_epoch() == ep {
                    return true;
                }
            }
        }
        false
    })
}

fn alloc_rope_into(reserve: usize, ascii: bool, fill: impl FnOnce(&mut String)) -> ValueV2 {
    AOT_ROPE.with(|cell| {
        let mut rope = cell.borrow_mut();
        let idx = rope.active;
        rope.active = (idx + 1) % ROPE_RING_LEN;
        if rope.slots[idx].epoch != 0 && !rope.slots[idx].buf.is_empty() {
            let old_ep = rope.slots[idx].epoch;
            if is_rope_epoch_rooted(old_ep) {
                let old_buf = std::mem::take(&mut rope.slots[idx].buf);
                rope.history
                    .insert(old_ep, std::sync::Arc::<str>::from(old_buf));
            } else {
                rope.slots[idx].buf.clear();
            }
        }
        loop {
            rope.next_epoch = rope.next_epoch.wrapping_add(1);
            let cand = rope.next_epoch;
            if cand != 0
                && !rope.slots.iter().any(|s| s.epoch == cand)
                && !rope.history.contains_key(&cand)
            {
                rope.slots[idx].epoch = cand;
                break;
            }
        }
        let slot = &mut rope.slots[idx];
        if slot.buf.capacity() < reserve {
            let need = reserve.saturating_sub(slot.buf.len());
            slot.buf.reserve(need);
        }
        fill(&mut slot.buf);
        let ep = slot.epoch;
        slot.ascii = ascii;
        let byte_len = slot.buf.len() as u32;
        if ascii {
            ValueV2::from_rope_ascii(ep, byte_len)
        } else {
            ValueV2::from_rope(ep, byte_len)
        }
    })
}

#[inline]
fn alloc_rope_from_str(s: &str, ascii: bool) -> ValueV2 {
    if s.len() <= 15 {
        return ValueV2::from_sso_str(s);
    }
    if s.len() > u32::MAX as usize {
        return alloc_heap_v2(Value::Str(crate::value::ThinArcStr::from(s)));
    }
    alloc_rope_into(s.len(), ascii, |buf| buf.push_str(s))
}

#[inline]
fn with_rope_str<R>(v: ValueV2, f: impl FnOnce(&str) -> R) -> Option<R> {
    if !v.is_rope() {
        return None;
    }
    let ep = v.rope_epoch();
    let len = v.rope_len();
    AOT_ROPE.with(|cell| {
        let rope = cell.borrow();
        rope.lookup_slice(ep, len).map(f)
    })
}

#[inline]
fn with_str_v2<R>(v: ValueV2, f: impl FnOnce(&str) -> R) -> Option<R> {
    if v.is_sso() {
        #[cfg(target_endian = "little")]
        {
            let len = v.sso_len();
            // SAFETY: `v` is a `#[repr(C)]` `ValueV2 { u64, u64 }` (16 bytes). On
            // little-endian targets, bytes `1..1 + len` (`len <= 15`) hold valid
            // UTF-8 written exclusively by `ValueV2` SSO constructors.
            let s = unsafe {
                let slice =
                    std::slice::from_raw_parts((&v as *const ValueV2 as *const u8).add(1), len);
                std::str::from_utf8_unchecked(slice)
            };
            return Some(f(s));
        }
        #[cfg(not(target_endian = "little"))]
        {
            return v.with_sso_str(f);
        }
    }
    if v.is_rope() {
        return with_rope_str(v, f);
    }
    if let Some(Value::Str(s)) = deref_heap(v) {
        return Some(f(s.as_ref()));
    }
    None
}

/// Allocate a reclaimable `TAG_HEAP` `Box<Value>` tracked by `AOT_HEAP`,
/// reusing recycled boxes from the free pool before hitting the global allocator.
#[inline]
fn alloc_heap_v2(v: Value) -> ValueV2 {
    CALL_SLAB_ALLOCS.with(|c| c.set(c.get() + 1));
    let ptr = AOT_HEAP.with(|h| {
        let mut st = h.borrow_mut();
        let ptr = if let Some(recycled) = st.free.pop() {
            // SAFETY: `recycled` was boxed on this thread via `Box::into_raw`,
            // cleared to `Value::Null` at sweep, and removed from `st.live`.
            unsafe { std::ptr::write(recycled, v) };
            recycled
        } else {
            Box::into_raw(Box::new(v))
        };
        st.live.push(ptr);
        ptr
    });
    ValueV2::from_heap_ptr(ptr)
}

/// Convert a `&Value` reference into `ValueV2`: scalars and `<= 15`-byte
/// strings stay inline without allocation or `Arc::clone`.
#[inline]
fn value_ref_to_v2(v: &Value) -> ValueV2 {
    match v {
        Value::Null => ValueV2::NULL,
        Value::Bool(b) => ValueV2::from_bool(*b),
        Value::Int(n) => ValueV2::from_int(*n),
        Value::Float(f) => ValueV2::from_float(*f),
        Value::Str(s) if s.len() <= 15 => ValueV2::from_sso_str(s.as_ref()),
        other => alloc_heap_v2(other.clone()),
    }
}

/// Convert a Rust `Value` to `ValueV2`: scalars and short strings are inline
/// (zero allocation), larger objects allocate a reclaimable `AOT_HEAP` box.
#[inline]
fn from_value_v2(v: Value) -> ValueV2 {
    match v {
        Value::Null => ValueV2::NULL,
        Value::Bool(b) => ValueV2::from_bool(b),
        Value::Int(n) => ValueV2::from_int(n),
        Value::Float(f) => ValueV2::from_float(f),
        Value::Str(s) if s.len() <= 15 => ValueV2::from_sso_str(s.as_ref()),
        other => alloc_heap_v2(other),
    }
}

/// Convert a Rust `Value` to a pinned `ValueV2` (never swept by `AOT_HEAP`),
/// used for global cache slots.
#[inline]
fn alloc_pinned_v2(v: Value) -> ValueV2 {
    match v {
        Value::Null => ValueV2::NULL,
        Value::Bool(b) => ValueV2::from_bool(b),
        Value::Int(n) => ValueV2::from_int(n),
        Value::Float(f) => ValueV2::from_float(f),
        other => ValueV2::from_heap_ptr(Box::into_raw(Box::new(other))),
    }
}

/// Register a compiled function's stack-allocated `[len x ValueV2]` frame
/// slots as precise GC roots for the duration of the call (DP-1 / RT-2).
#[no_mangle]
pub extern "C" fn spid3r_frame_enter(slots: *const ValueV2, len: usize) {
    if !slots.is_null() && len > 0 {
        ROOT_FRAMES.with(|rf| rf.borrow_mut().push((slots, len)));
    }
}

/// Unregister the current compiled function's frame roots on return/unwind.
#[no_mangle]
pub extern "C" fn spid3r_frame_exit() {
    ROOT_FRAMES.with(|rf| {
        rf.borrow_mut().pop();
    });
}

/// Loop-backedge / statement GC safepoint (DP-1 / RT-2). Sweeps unreachable
/// `AOT_HEAP` `Box<Value>` allocations and stale `AOT_ROPE` history epochs
/// when the thread-local heap reaches `ARENA_SLAB_LEN` (4096) objects.
#[no_mangle]
pub extern "C" fn spid3r_gc_safepoint() {
    let heap_due = AOT_HEAP.with(|h| h.borrow().live.len() >= ARENA_SLAB_LEN);
    let has_rope_history = AOT_ROPE.with(|r| !r.borrow().history.is_empty());
    let rope_due = has_rope_history && AOT_ROPE.with(|r| r.borrow().history.len() >= 1024);
    if !heap_due && !rope_due {
        return;
    }
    let mut live_small = [0usize; 64];
    let mut live_small_len = 0usize;
    let mut live_large: Option<crate::value::collections::FxHashSet<usize>> = None;
    let mut live_epochs = crate::value::collections::FxHashSet::default();
    ROOT_FRAMES.with(|rf| {
        for &(base, len) in rf.borrow().iter() {
            // SAFETY: each active frame registered its stack-allocated
            // `[len x ValueV2]` array on entry and pops it before returning.
            let slice = unsafe { std::slice::from_raw_parts(base, len) };
            for v in slice {
                if v.is_heap() {
                    let addr = v.as_heap_ptr() as usize;
                    if let Some(set) = &mut live_large {
                        set.insert(addr);
                    } else if !live_small[..live_small_len].contains(&addr) {
                        if live_small_len < live_small.len() {
                            live_small[live_small_len] = addr;
                            live_small_len += 1;
                        } else {
                            let mut set = crate::value::collections::FxHashSet::default();
                            for &p in &live_small {
                                set.insert(p);
                            }
                            set.insert(addr);
                            live_large = Some(set);
                        }
                    }
                } else if has_rope_history && v.is_rope() {
                    live_epochs.insert(v.rope_epoch());
                }
            }
        }
    });
    let live_slice = &live_small[..live_small_len];
    AOT_HEAP.with(|h| {
        let mut st = h.borrow_mut();
        let mut i = 0;
        while i < st.live.len() {
            let ptr = st.live[i];
            let is_live = match &live_large {
                Some(set) => set.contains(&(ptr as usize)),
                None => live_slice.contains(&(ptr as usize)),
            };
            if is_live {
                i += 1;
            } else {
                st.live.swap_remove(i);
                if st.free.len() < ARENA_SLAB_LEN {
                    // SAFETY: `ptr` was allocated via `Box::into_raw(Box::new(v))`
                    // on this thread and is not referenced by any active frame root.
                    let old = unsafe { std::mem::replace(&mut *ptr, Value::Null) };
                    collections::recycle_or_drop(old);
                    st.free.push(ptr);
                } else {
                    // SAFETY: same as above; pool is full so free the box.
                    unsafe {
                        drop(Box::from_raw(ptr));
                    }
                }
            }
        }
    });
    if has_rope_history {
        AOT_ROPE.with(|r| {
            r.borrow_mut()
                .history
                .retain(|ep, _| live_epochs.contains(ep));
        });
    }
}

// ----------------------------------------------------------------------------
// exception handling — try / catch / finally / throw
//
// The native backend implements exceptions with a thread-local "pending throw"
// flag and *compiled* control flow, not native unwinding: `setjmp`/`longjmp`
// are not exposed by libc on Windows, and Rust panics cannot reliably unwind
// through LLVM-generated frames. Instead, an operation that would throw records
// the value in `THROWN` and returns a placeholder handle; the generated code
// checks `spid3r_threw` right after each such operation and, when a throw is
// pending, branches to the enclosing `try`'s handler (or, with no handler in
// the current function, returns so the throw propagates up to one). The
// handler consumes the value, runs `catch`/`finally`, and re-throws if the
// exception is still pending — mirroring the tree/VM semantics exactly, while
// staying entirely within the compiled frame.
// ----------------------------------------------------------------------------

thread_local! {
    /// The value of the most recent, not-yet-taken Spid3r `throw`.
    static THROWN: RefCell<Option<Value>> = const { RefCell::new(None) };
    /// Lock-free 1-byte shadow flag mirroring `THROWN.is_some()` for O(1) `spid3r_threw()`.
    static THROWN_FLAG: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    /// True when the pending throw came from [`rt_throw`] — a runtime *error*
    /// ("division by zero", an out-of-bounds index, …) rather than a user
    /// `throw`. The tree/VM top-level handlers render these differently
    /// (`error[runtime]: <message>` for errors vs `error[runtime]: uncaught
    /// throw (thrown: <value>)` for user throws), so the uncaught path needs
    /// the distinction. Cleared whenever the pending throw is replaced by a
    /// user throw or consumed by a handler.
    static RUNTIME_ERROR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Raise a thrown value (inline `throw` / rethrow): record it in `THROWN`. The
/// surrounding generated code's post-operation `spid3r_threw` guard then routes
/// control to the enclosing handler.
#[no_mangle]
pub extern "C" fn spid3r_throw(v: *const ValueV2) {
    RUNTIME_ERROR.with(|f| f.set(false));
    let val = if v.is_null() {
        Value::Null
    } else {
        to_owned_v2(v)
    };
    THROWN.with(|t| *t.borrow_mut() = Some(val));
    THROWN_FLAG.with(|f| f.set(1));
}

/// Cancellation check for generated code: returns 1 and raises "task
/// cancelled" when the running task was cancelled, 0 otherwise. Loop bodies
/// call it periodically, so a cancelled native task stops at the next poll
/// instead of running to completion.
#[no_mangle]
pub extern "C" fn spid3r_cancel_check() -> u8 {
    spid3r_gc_safepoint();
    if crate::sched::current_task_cancelled() {
        rt_throw_v2("task cancelled");
        1
    } else {
        0
    }
}

/// Non-zero when a `throw` is pending. Generated code calls this after every
/// operation that may throw and branches to its handler when it returns non-zero.
#[no_mangle]
pub extern "C" fn spid3r_threw() -> u8 {
    THROWN_FLAG.with(|f| f.get())
}

/// Take the pending thrown value (clearing it) and write it into `out`.
#[no_mangle]
pub extern "C" fn spid3r_take_thrown(out: *mut ValueV2) {
    RUNTIME_ERROR.with(|f| f.set(false));
    THROWN_FLAG.with(|f| f.set(0));
    let v = THROWN
        .with(|t| t.borrow_mut().take())
        .map(from_value_v2)
        .unwrap_or(ValueV2::NULL);
    write_out(out, v);
}

/// Take the pending thrown Rust `Value` directly (internal Rust helper).
fn take_thrown_value() -> Option<Value> {
    RUNTIME_ERROR.with(|f| f.set(false));
    THROWN_FLAG.with(|f| f.set(0));
    THROWN.with(|t| t.borrow_mut().take())
}

/// Report an uncaught throw: print the pending exception and exit with the
/// "user-program error" status (1), matching the tree/VM top-level message.
#[no_mangle]
pub extern "C" fn spid3r_uncaught() -> ! {
    let _ = std::io::stdout().flush();
    let is_runtime_error = RUNTIME_ERROR.with(|f| f.get());
    let shown = THROWN.with(|t| match t.borrow().as_ref() {
        Some(v) => crate::value::display_value(v),
        None => "unknown".to_string(),
    });
    if is_runtime_error {
        eprintln!("error[runtime]: {}", shown);
    } else {
        eprintln!("error[runtime]: uncaught throw (thrown: {})", shown);
    }
    std::process::exit(1);
}

/// Record a runtime error message as a thrown value and return `ValueV2::NULL`.
fn rt_throw_v2(msg: &str) -> ValueV2 {
    RUNTIME_ERROR.with(|f| f.set(true));
    THROWN.with(|t| *t.borrow_mut() = Some(Value::Str(msg.into())));
    THROWN_FLAG.with(|f| f.set(1));
    ValueV2::NULL
}

/// Raise a thrown user value (not just its display string) as the pending
/// exception, so an enclosing `try` rebinds the original value (audit A-9).
fn rt_throw_value_v2(v: Value) -> ValueV2 {
    RUNTIME_ERROR.with(|f| f.set(false));
    THROWN.with(|t| *t.borrow_mut() = Some(v));
    THROWN_FLAG.with(|f| f.set(1));
    ValueV2::NULL
}

/// Run `f` at an `extern "C"` boundary (audit A-12).
///
/// A Rust panic inside the native runtime is an internal bug, and unwinding into
/// generated machine code is undefined; a non-unwinding ABI aborts instead, which
/// loses the script-level diagnostic. Catching the panic here reports it through
/// the runtime's normal error channel: `on_panic` turns it into a pending throw
/// (so the generated `spid3r_threw` guard unwinds the script and the program
/// exits 1 with a message), or supplies a safe sentinel for setup calls.
fn ffi_guard<T>(what: &str, on_panic: impl FnOnce(String) -> T, f: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(payload) => {
            let detail = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            eprintln!(
                "spid3r: internal error in the native runtime ({}): {}",
                what, detail
            );
            on_panic(detail)
        }
    }
}

/// `ffi_guard` for a call that writes a `ValueV2` into `out`: a panic becomes
/// a pending throw, so the script fails cleanly instead of aborting.
fn ffi_v2(out: *mut ValueV2, what: &str, f: impl FnOnce() -> ValueV2) {
    let v = ffi_guard(
        what,
        |detail| rt_throw_v2(&format!("internal error in the native runtime: {}", detail)),
        f,
    );
    write_out(out, v);
}

/// Borrow a C string from generated code without allocating when valid UTF-8.
#[inline]
fn cstr_ref<'a>(p: *const c_char) -> std::borrow::Cow<'a, str> {
    // SAFETY: generated code passes NUL-terminated string constants; null aborts cleanly.
    unsafe {
        if p.is_null() {
            abort("native runtime: null string pointer");
        }
        CStr::from_ptr(p).to_string_lossy()
    }
}

/// Copy a C string from generated code.
fn cstr(p: *const c_char) -> String {
    cstr_ref(p).into_owned()
}

// ----------------------------------------------------------------------------
// stdlib installation
// ----------------------------------------------------------------------------

type FullStdlibResolver = fn(&str) -> Option<std::sync::Arc<crate::value::NativeFn>>;
type JsonWriteHook = fn(&Value) -> Result<crate::value::ThinArcStr, String>;
type JsonParseHook = fn(&str) -> Result<Value, String>;

static FULL_STDLIB_RESOLVER: OnceLock<FullStdlibResolver> = OnceLock::new();
static JSON_WRITE_HOOK: OnceLock<JsonWriteHook> = OnceLock::new();
static JSON_PARSE_HOOK: OnceLock<JsonParseHook> = OnceLock::new();
static RT_CORE_INIT: OnceLock<()> = OnceLock::new();

fn install_aot_eventloop_invoker() {
    crate::stdlib::eventloop::set_aot_invoker(std::sync::Arc::new(|cb, args| {
        let mut frame_vals: Vec<ValueV2> = Vec::with_capacity(args.len() + 2);
        frame_vals.push(from_value_v2(cb));
        for a in args {
            frame_vals.push(from_value_v2(a));
        }
        frame_vals.push(ValueV2::NULL); // out slot
        let f_ptr = &frame_vals[0] as *const ValueV2;
        let arg_ptrs: Vec<*const ValueV2> = (1..frame_vals.len() - 1)
            .map(|i| &frame_vals[i] as *const ValueV2)
            .collect();
        spid3r_frame_enter(frame_vals.as_ptr(), frame_vals.len());
        let res = spid3r_call_inner_dispatch(f_ptr, arg_ptrs.len(), arg_ptrs.as_ptr());
        spid3r_frame_exit();
        if spid3r_threw() != 0 {
            match take_thrown_value() {
                None => Err("event loop callback threw".to_string()),
                Some(v) => Err(crate::value::display_value(&v)),
            }
        } else {
            Ok(v2_to_value(res))
        }
    }));
}

fn rt_init_core() {
    RT_CORE_INIT.get_or_init(|| {
        let mut g = globals().lock().unwrap();
        if !g.is_empty() {
            return;
        }
        let object = std::sync::Arc::new(crate::value::CompiledClass {
            name: "Object".to_string(),
            parent: None,
            fields: Vec::new(),
            methods: HashMap::new(),
            ctor: None,
            class_id: 0,
            vtable: Vec::new(),
            vtable_slots: HashMap::new(),
        });
        g.insert("Object".to_string(), Value::CClass(object));
        // `none` is a builtin global (an `Option` holding nothing), matching the
        // tree/VM engines — so `none.unwrap()` throws "unwrap on none" instead of
        // the generic "null values have no methods".
        g.insert("none".to_string(), Value::Option(None));
    });
}

/// Install the full native stdlib + `Object` into the global table. Called by
/// legacy/selfhost `spid3r_rt_start`; slim builds use `spid3r_rt_start_slim` plus
/// targeted `spid3r_reg_<group>` calls so unused subsystems can be dead-stripped.
pub fn rt_init() {
    rt_init_core();
    let _ = FULL_STDLIB_RESOLVER.set(crate::stdlib::resolve_builtin_native);
    let _ = JSON_WRITE_HOOK.set(crate::stdlib::json_write_arc);
    let _ = JSON_PARSE_HOOK.set(crate::stdlib::json_parse_text);
    install_aot_eventloop_invoker();
}

fn install_native_defs(
    defs: Vec<crate::stdlib::NativeDef>,
    guard_lookup: Option<fn(&str) -> Option<crate::vm::NativeImpl>>,
) {
    let checked = match guard_lookup {
        Some(lookup) => crate::stdlib::policy_checked_defs_with_guard(defs, lookup),
        None => crate::stdlib::policy_checked_defs(defs),
    };
    let mut g = globals().lock().unwrap();
    for (name, func) in checked {
        g.entry(name.to_string()).or_insert_with(|| {
            Value::Native(std::sync::Arc::new(crate::value::NativeFn { name, func }))
        });
    }
    GLOBAL_GEN.fetch_add(1, std::sync::atomic::Ordering::Release);
}

#[no_mangle]
pub extern "C" fn spid3r_reg_core() {
    install_native_defs(crate::stdlib::core_defs(), None);
}

#[no_mangle]
pub extern "C" fn spid3r_reg_json() {
    let _ = JSON_WRITE_HOOK.set(crate::stdlib::json_write_arc);
    let _ = JSON_PARSE_HOOK.set(crate::stdlib::json_parse_text);
    install_native_defs(crate::stdlib::json_defs(), None);
}

#[no_mangle]
pub extern "C" fn spid3r_reg_fs() {
    install_native_defs(
        crate::stdlib::fs_defs(),
        Some(crate::stdlib::fs_policy::guarded_fs),
    );
}

#[no_mangle]
pub extern "C" fn spid3r_reg_net() {
    install_aot_eventloop_invoker();
    install_native_defs(
        crate::stdlib::net_defs(),
        Some(crate::stdlib::fs_policy::guarded_net),
    );
}

#[no_mangle]
pub extern "C" fn spid3r_reg_db() {
    install_native_defs(
        crate::stdlib::db_defs(),
        Some(crate::stdlib::fs_policy::guarded_db),
    );
}

#[no_mangle]
pub extern "C" fn spid3r_reg_media() {
    install_native_defs(crate::stdlib::media_defs(), None);
}

#[no_mangle]
pub extern "C" fn spid3r_reg_tz() {
    install_native_defs(crate::stdlib::tz_defs(), None);
}

#[no_mangle]
pub extern "C" fn spid3r_reg_unicode() {
    install_native_defs(crate::stdlib::unicode_defs(), None);
}

#[no_mangle]
pub extern "C" fn spid3r_reg_crypto() {
    install_native_defs(crate::stdlib::crypto_defs(), None);
}

#[no_mangle]
pub extern "C" fn spid3r_reg_data() {
    install_native_defs(
        crate::stdlib::data_defs(),
        Some(crate::stdlib::fs_policy::guarded_data),
    );
}

#[no_mangle]
pub extern "C" fn spid3r_reg_info() {
    install_native_defs(crate::stdlib::info_defs(), None);
}

// ----------------------------------------------------------------------------
// extern "C" trampolines — value construction
// ----------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn spid3r_value_int(out: *mut ValueV2, v: i64) {
    write_out(out, ValueV2::from_int(v));
}

#[no_mangle]
pub extern "C" fn spid3r_value_float(out: *mut ValueV2, v: f64) {
    write_out(out, ValueV2::from_float(v));
}

#[no_mangle]
pub extern "C" fn spid3r_value_bool(out: *mut ValueV2, v: u8) {
    write_out(out, ValueV2::from_bool(v != 0));
}

#[no_mangle]
pub extern "C" fn spid3r_value_null(out: *mut ValueV2) {
    write_out(out, ValueV2::NULL);
}

#[no_mangle]
pub extern "C" fn spid3r_i64_add(a: i64, b: i64) -> i64 {
    match a.checked_add(b) {
        Some(v) => v,
        None => {
            rt_throw_v2(&format!("integer overflow: {} + {}", a, b));
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn spid3r_i64_sub(a: i64, b: i64) -> i64 {
    match a.checked_sub(b) {
        Some(v) => v,
        None => {
            rt_throw_v2(&format!("integer overflow: {} - {}", a, b));
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn spid3r_i64_mul(a: i64, b: i64) -> i64 {
    match a.checked_mul(b) {
        Some(v) => v,
        None => {
            rt_throw_v2(&format!("integer overflow: {} * {}", a, b));
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn spid3r_i64_div(a: i64, b: i64) -> i64 {
    if b == 0 {
        rt_throw_v2("division by zero");
        return 0;
    }
    match a.checked_div(b) {
        Some(v) => v,
        None => {
            rt_throw_v2(&format!("integer overflow: {} / {}", a, b));
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn spid3r_i64_mod(a: i64, b: i64) -> i64 {
    if b == 0 {
        rt_throw_v2("modulo by zero");
        return 0;
    }
    a.wrapping_rem(b)
}

#[no_mangle]
pub extern "C" fn spid3r_i64_neg(a: i64) -> i64 {
    match a.checked_neg() {
        Some(v) => v,
        None => {
            rt_throw_v2(&format!("integer overflow: -{}", a));
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn spid3r_to_i64(v: *const ValueV2) -> i64 {
    let val = read_v2(v);
    if val.tag == ValueV2::TAG_INT {
        val.payload as i64
    } else {
        rt_throw_v2("expected integer");
        0
    }
}

/// 1 when the value is an `Int`, 0 otherwise. Never throws.
#[no_mangle]
pub extern "C" fn spid3r_is_int(v: *const ValueV2) -> i8 {
    (read_v2(v).tag == ValueV2::TAG_INT) as i8
}

/// Raw i64 payload of an `Int` value. Call only after `spid3r_is_int`
/// returned 1; any other value yields 0 rather than reading garbage.
#[no_mangle]
pub extern "C" fn spid3r_int_of(v: *const ValueV2) -> i64 {
    let val = read_v2(v);
    if val.tag == ValueV2::TAG_INT {
        val.payload as i64
    } else {
        0
    }
}

#[no_mangle]
pub extern "C" fn spid3r_to_f64(v: *const ValueV2) -> f64 {
    let val = read_v2(v);
    match val.tag {
        ValueV2::TAG_FLOAT => f64::from_bits(val.payload),
        ValueV2::TAG_INT => (val.payload as i64) as f64,
        _ => {
            rt_throw_v2("expected number");
            0.0
        }
    }
}

#[no_mangle]
pub extern "C" fn spid3r_cstr(v: *const ValueV2) -> *mut c_char {
    let val = read_v2(v);
    if val.tag == ValueV2::TAG_NULL {
        return match std::ffi::CString::new("") {
            Ok(c) => c.into_raw(),
            Err(_) => std::ptr::null_mut(),
        };
    }
    if let Some(res) = with_str_v2(val, |text| std::ffi::CString::new(text)) {
        return match res {
            Ok(c) => c.into_raw(),
            Err(_) => {
                rt_throw_v2("string contains a NUL byte");
                std::ptr::null_mut()
            }
        };
    }
    rt_throw_v2("expected string");
    std::ptr::null_mut()
}

#[no_mangle]
pub extern "C" fn spid3r_value_from_cstr(out: *mut ValueV2, p: *const c_char) {
    if p.is_null() {
        write_out(out, ValueV2::NULL);
        return;
    }
    // SAFETY: caller passes a NUL-terminated C string from `extern fn`.
    let text = unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy();
    let v = if text.len() <= 15 {
        ValueV2::from_sso_str(text.as_ref())
    } else {
        alloc_heap_v2(Value::Str(text.into_owned().into()))
    };
    write_out(out, v);
}

/// Build a string from raw bytes (LLVM globals hold the literal bytes).
/// Short literals (`<= 15` bytes) pack inline into `ValueV2` SSO with zero
/// heap or cache lookup; longer literals stay pinned in `STATIC_STR_CACHE`.
#[no_mangle]
pub extern "C" fn spid3r_value_str(out: *mut ValueV2, ptr: *const u8, len: usize) {
    if ptr.is_null() {
        write_out(out, ValueV2::NULL);
        return;
    }
    if len <= 15 {
        // SAFETY: `ptr` points to a compiler-emitted static byte slice of `len` bytes.
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        if let Ok(valid) = std::str::from_utf8(bytes) {
            write_out(out, ValueV2::from_sso_str(valid));
            return;
        }
        let lossy = String::from_utf8_lossy(bytes);
        if lossy.len() <= 15 {
            write_out(out, ValueV2::from_sso_str(lossy.as_ref()));
            return;
        }
        let p = Box::into_raw(Box::new(Value::Str(lossy.into_owned().into())));
        write_out(out, ValueV2::from_heap_ptr(p));
        return;
    }
    let key = (ptr as usize, len);
    let cached = STATIC_STR_CACHE.with(|c| c.borrow().get(&key).copied());
    if let Some(p) = cached {
        write_out(out, ValueV2::from_heap_ptr(p));
        return;
    }
    // SAFETY: `ptr` points to a compiler-emitted static byte slice of `len` bytes.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    let s: crate::value::ThinArcStr = match std::str::from_utf8(bytes) {
        Ok(valid) => crate::value::ThinArcStr::from(valid),
        Err(_) => String::from_utf8_lossy(bytes).into(),
    };
    let p = Box::into_raw(Box::new(Value::Str(s)));
    STATIC_STR_CACHE.with(|c| {
        c.borrow_mut().insert(key, p);
    });
    write_out(out, ValueV2::from_heap_ptr(p));
}

#[no_mangle]
pub extern "C" fn spid3r_value_from_ptr(out: *mut ValueV2, p: *mut c_void) {
    write_out(out, ValueV2::from_int(p as usize as i64));
}

#[no_mangle]
pub extern "C" fn spid3r_to_ptr(v: *const ValueV2) -> *mut c_void {
    let val = read_v2(v);
    if val.tag == ValueV2::TAG_INT {
        val.payload as usize as *mut c_void
    } else {
        rt_throw_v2("expected pointer");
        std::ptr::null_mut()
    }
}

// ----------------------------------------------------------------------------
// extern "C" trampolines — operators
// ----------------------------------------------------------------------------

/// BinOp discriminant, matching `ast::BinOp` declaration order.
pub fn binop_from_u8(op: u8) -> Option<BinOp> {
    Some(match op {
        0 => BinOp::Add,
        1 => BinOp::Sub,
        2 => BinOp::Mul,
        3 => BinOp::Div,
        4 => BinOp::Mod,
        5 => BinOp::Eq,
        6 => BinOp::Ne,
        7 => BinOp::Lt,
        8 => BinOp::Le,
        9 => BinOp::Gt,
        10 => BinOp::Ge,
        _ => return None,
    })
}

pub fn unop_from_u8(op: u8) -> Option<UnOp> {
    Some(match op {
        0 => UnOp::Not,
        1 => UnOp::Neg,
        _ => return None,
    })
}

/// The dunder pair a binary operator overloads, or `None` for operators with
/// no overload (`%`, comparisons) — the same set the tree interpreter
/// (`operator_call`) and the VM (`try_operator`) dispatch.
fn overload_dunders(op: BinOp) -> Option<(&'static str, &'static str)> {
    Some(match op {
        BinOp::Add => ("__add__", "__radd__"),
        BinOp::Sub => ("__sub__", "__rsub__"),
        BinOp::Mul => ("__mul__", "__rmul__"),
        BinOp::Div => ("__div__", "__rdiv__"),
        BinOp::Mod => ("__mod__", "__rmod__"),
        _ => return None,
    })
}

/// Invoke a callable value natively (the map-dunder route of operator
/// overloading). Arity checking, `Native`/`NativeClosure` dispatch and
/// construction all go through the shared `spid3r_call_inner`, so the
/// semantics — including the arity diagnostic — match a direct call.
fn native_call_value(f: Value, args: Vec<Value>) -> Result<Value, String> {
    let mut frame_vals: Vec<ValueV2> = Vec::with_capacity(args.len() + 1);
    frame_vals.push(from_value_v2(f));
    for a in args {
        frame_vals.push(from_value_v2(a));
    }
    let f_ptr = &frame_vals[0] as *const ValueV2;
    let argv: Vec<*const ValueV2> = (1..frame_vals.len())
        .map(|i| &frame_vals[i] as *const ValueV2)
        .collect();
    spid3r_frame_enter(frame_vals.as_ptr(), frame_vals.len());
    let r = spid3r_call_inner(f_ptr, argv.len(), argv.as_ptr());
    spid3r_frame_exit();
    Ok(v2_to_value(r))
}

fn call_cfn_method(cfn: &CompiledFn, method: &str, args: &[Value]) -> Result<Value, String> {
    let argc = args.len();
    if argc < cfn.min_arity || argc > cfn.arity {
        return Err(format!(
            "{} expects {} argument(s), got {}",
            method, cfn.arity, argc
        ));
    }
    let mut frame_vals: Vec<ValueV2> = args.iter().map(|a| from_value_v2(a.clone())).collect();
    let argv: Vec<*const ValueV2> = frame_vals.iter().map(|v| v as *const ValueV2).collect();
    let mut out = ValueV2::NULL;
    if !enter_call() {
        return Err("stack overflow: call depth limit reached".to_string());
    }
    spid3r_frame_enter(frame_vals.as_ptr(), frame_vals.len());
    // SAFETY: `cfn.code` is a compiled Spid3r function pointer with matching ABI.
    unsafe { (cfn.code)(&mut out, cfn.env, argc, argv.as_ptr()) };
    spid3r_frame_exit();
    leave_call();
    let _ = &mut frame_vals;
    Ok(v2_to_value(out))
}

/// Operator overloading in compiled code — the native half of the VM's
/// `OpCode::Add` dispatch (`try_operator`): a map carries the dunder as a
/// value, a compiled class instance carries it as a method resolved through
/// the class chain. `None` means "no overload": the caller falls back to the
/// shared `vm_eval_binary`/`vm_eval_unary` and reports its usual type error,
/// exactly like the tree and VM engines.
fn native_try_operator(l: &Value, r: &Value, method: &str) -> Option<Result<Value, String>> {
    match l {
        Value::Map(_) => {
            let f = collections::index_get(l, &Value::Str(method.into())).ok()?;
            // Same callable filter as the engines: a non-callable dunder is
            // "no overload", not an error.
            if !matches!(
                f,
                Value::Cfn(_) | Value::Native(_) | Value::NativeClosure(_) | Value::CClass(_)
            ) {
                return None;
            }
            Some(native_call_value(f, vec![l.clone(), r.clone()]))
        }
        Value::CInstance(inst) => {
            let class = inst.lock().unwrap().class.clone();
            let (_, cfn) = find_method_cclass(&class, method)?;
            Some(call_cfn_method(&cfn, method, &[l.clone(), r.clone()]))
        }
        _ => None,
    }
}

/// Unary counterpart (`__neg__`): the receiver alone, no right-hand operand.
fn native_try_unary_operator(v: &Value, method: &str) -> Option<Result<Value, String>> {
    match v {
        Value::Map(_) => {
            let f = collections::index_get(v, &Value::Str(method.into())).ok()?;
            if !matches!(
                f,
                Value::Cfn(_) | Value::Native(_) | Value::NativeClosure(_) | Value::CClass(_)
            ) {
                return None;
            }
            Some(native_call_value(f, vec![v.clone()]))
        }
        Value::CInstance(inst) => {
            let class = inst.lock().unwrap().class.clone();
            let (_, cfn) = find_method_cclass(&class, method)?;
            Some(call_cfn_method(&cfn, method, std::slice::from_ref(v)))
        }
        _ => None,
    }
}

#[inline]
fn is_str_v2(v: ValueV2) -> bool {
    v.is_sso() || v.is_rope() || matches!(deref_heap(v), Some(Value::Str(_)))
}

fn concat_str_v2(lv: ValueV2, rv: ValueV2) -> Option<ValueV2> {
    if lv.is_sso() && rv.is_sso() {
        let llen = lv.sso_len();
        let rlen = rv.sso_len();
        let total = llen + rlen;
        if total <= 7 {
            let l_bytes = lv.tag >> 8;
            let r_bytes = rv.tag >> 8;
            let new_tag =
                (ValueV2::TAG_SSO_BASE + total as u64) | ((l_bytes | (r_bytes << (llen * 8))) << 8);
            return Some(ValueV2 {
                tag: new_tag,
                payload: 0,
            });
        }
        if total <= 15 {
            let mut lbuf = [0u8; 16];
            let mut rbuf = [0u8; 16];
            lv.write_sso_bytes(&mut lbuf);
            rv.write_sso_bytes(&mut rbuf);
            let mut combined = [0u8; 15];
            combined[..llen].copy_from_slice(&lbuf[1..1 + llen]);
            combined[llen..total].copy_from_slice(&rbuf[1..1 + rlen]);
            return Some(ValueV2::from_sso_bytes(&combined[..total]));
        }
    }
    if !(is_str_v2(lv) && is_str_v2(rv)) {
        return None;
    }

    let mut l_sso_buf = [0u8; 16];
    let mut r_sso_buf = [0u8; 16];
    let l_sso_slice: Option<&str> = if lv.is_sso() {
        let llen = lv.write_sso_bytes(&mut l_sso_buf);
        std::str::from_utf8(&l_sso_buf[1..1 + llen]).ok()
    } else {
        None
    };
    let r_sso_slice: Option<&str> = if rv.is_sso() {
        let rlen = rv.write_sso_bytes(&mut r_sso_buf);
        std::str::from_utf8(&r_sso_buf[1..1 + rlen]).ok()
    } else {
        None
    };
    let l_heap: Option<&str> = match deref_heap(lv) {
        Some(Value::Str(s)) => Some(s.as_ref()),
        _ => None,
    };
    let r_heap: Option<&str> = match deref_heap(rv) {
        Some(Value::Str(s)) => Some(s.as_ref()),
        _ => None,
    };

    let direct_total: Option<Option<ValueV2>> = AOT_ROPE.with(|cell| {
        let mut rope = cell.borrow_mut();
        let l_len = if let Some(s) = l_sso_slice.or(l_heap) {
            s.len()
        } else {
            lv.rope_len()
        };
        let r_len = if let Some(s) = r_sso_slice.or(r_heap) {
            s.len()
        } else {
            rv.rope_len()
        };
        let total = l_len + r_len;

        if total <= 15 {
            let mut combined = [0u8; 15];
            let l_src: &str = match l_sso_slice.or(l_heap) {
                Some(s) => s,
                None => match rope.lookup_slice(lv.rope_epoch(), l_len) {
                    Some(s) => s,
                    None => return None,
                },
            };
            combined[..l_len].copy_from_slice(l_src.as_bytes());
            let r_src: &str = match r_sso_slice.or(r_heap) {
                Some(s) => s,
                None => match rope.lookup_slice(rv.rope_epoch(), r_len) {
                    Some(s) => s,
                    None => return None,
                },
            };
            combined[l_len..total].copy_from_slice(r_src.as_bytes());
            return Some(Some(ValueV2::from_sso_bytes(&combined[..total])));
        }

        // O(1) amortized tail append on a matching ring slot buffer.
        if lv.is_rope() && total <= u32::MAX as usize {
            let lep = lv.rope_epoch();
            if let Some(i) = rope
                .slots
                .iter()
                .position(|s| s.epoch == lep && s.buf.len() == l_len)
            {
                let r_owned: Option<String> = if r_sso_slice.is_some() || r_heap.is_some() {
                    None
                } else {
                    match rope.lookup_slice(rv.rope_epoch(), r_len) {
                        Some(s) if rv.is_rope() && rv.rope_epoch() == lep => Some(s.to_owned()),
                        Some(_) => None,
                        None => return None,
                    }
                };
                if let Some(rs) = r_sso_slice.or(r_heap).or(r_owned.as_deref()) {
                    let r_ascii = rs.is_ascii();
                    rope.slots[i].buf.push_str(rs);
                    rope.slots[i].ascii &= r_ascii;
                } else {
                    let rep = rv.rope_epoch();
                    let r_idx = rope.slots.iter().position(|s| s.epoch == rep);
                    if let Some(j) = r_idx {
                        let (src, dst) = if j < i {
                            let (lo, hi) = rope.slots.split_at_mut(i);
                            (&lo[j], &mut hi[0])
                        } else {
                            let (lo, hi) = rope.slots.split_at_mut(j);
                            (&hi[0], &mut lo[i])
                        };
                        let rs = src.buf.get(..r_len)?;
                        let r_ascii = rs.is_ascii();
                        dst.buf.push_str(rs);
                        dst.ascii &= r_ascii;
                    } else {
                        let arc = rope.history.get(&rep).cloned()?;
                        let rs = arc.get(..r_len)?;
                        let r_ascii = rs.is_ascii();
                        rope.slots[i].buf.push_str(rs);
                        rope.slots[i].ascii &= r_ascii;
                    }
                }
                let new_len = rope.slots[i].buf.len() as u32;
                let ascii = rope.slots[i].ascii;
                return Some(Some(if ascii {
                    ValueV2::from_rope_ascii(lep, new_len)
                } else {
                    ValueV2::from_rope(lep, new_len)
                }));
            }
        }

        if total > u32::MAX as usize {
            let mut s = String::with_capacity(total);
            let l_src = match l_sso_slice.or(l_heap) {
                Some(ls) => ls,
                None => match rope.lookup_slice(lv.rope_epoch(), l_len) {
                    Some(ls) => ls,
                    None => return None,
                },
            };
            s.push_str(l_src);
            let r_src = match r_sso_slice.or(r_heap) {
                Some(rs) => rs,
                None => match rope.lookup_slice(rv.rope_epoch(), r_len) {
                    Some(rs) => rs,
                    None => return None,
                },
            };
            s.push_str(r_src);
            return Some(Some(alloc_heap_v2(Value::Str(s.into()))));
        }

        if total <= 256 {
            let mut buf = [0u8; 256];
            let l_src = match l_sso_slice.or(l_heap) {
                Some(ls) => ls,
                None => match rope.lookup_slice(lv.rope_epoch(), l_len) {
                    Some(ls) => ls,
                    None => return None,
                },
            };
            buf[..l_len].copy_from_slice(l_src.as_bytes());
            let r_src = match r_sso_slice.or(r_heap) {
                Some(rs) => rs,
                None => match rope.lookup_slice(rv.rope_epoch(), r_len) {
                    Some(rs) => rs,
                    None => return None,
                },
            };
            buf[l_len..total].copy_from_slice(r_src.as_bytes());
            let all_ascii = l_src.is_ascii() && r_src.is_ascii();
            drop(rope);
            // SAFETY: `l_src` and `r_src` are valid UTF-8.
            let combined = unsafe { std::str::from_utf8_unchecked(&buf[..total]) };
            return Some(Some(alloc_rope_from_str(combined, all_ascii)));
        }

        let mut next_buf = String::with_capacity(total);
        let l_src = match l_sso_slice.or(l_heap) {
            Some(ls) => ls,
            None => match rope.lookup_slice(lv.rope_epoch(), l_len) {
                Some(ls) => ls,
                None => return None,
            },
        };
        next_buf.push_str(l_src);
        let r_src = match r_sso_slice.or(r_heap) {
            Some(rs) => rs,
            None => match rope.lookup_slice(rv.rope_epoch(), r_len) {
                Some(rs) => rs,
                None => return None,
            },
        };
        next_buf.push_str(r_src);
        let all_ascii = next_buf.is_ascii();
        drop(rope);
        Some(Some(alloc_rope_into(next_buf.len(), all_ascii, |b| {
            b.push_str(&next_buf)
        })))
    });
    direct_total.flatten()
}

#[no_mangle]
pub extern "C" fn spid3r_binop(out: *mut ValueV2, l: *const ValueV2, r: *const ValueV2, op: i64) {
    let op = match binop_from_u8(op as u8) {
        Some(o) => o,
        None => abort("native runtime: unknown binop"),
    };
    let lv = read_v2(l);
    let rv = read_v2(r);
    if lv.tag == ValueV2::TAG_INT && rv.tag == ValueV2::TAG_INT {
        let a = lv.payload as i64;
        let b = rv.payload as i64;
        let res = match op {
            BinOp::Add => match a.checked_add(b) {
                Some(v) => ValueV2::from_int(v),
                None => rt_throw_v2(&format!("integer overflow: {} + {}", a, b)),
            },
            BinOp::Sub => match a.checked_sub(b) {
                Some(v) => ValueV2::from_int(v),
                None => rt_throw_v2(&format!("integer overflow: {} - {}", a, b)),
            },
            BinOp::Mul => match a.checked_mul(b) {
                Some(v) => ValueV2::from_int(v),
                None => rt_throw_v2(&format!("integer overflow: {} * {}", a, b)),
            },
            BinOp::Div => {
                if b == 0 {
                    rt_throw_v2("division by zero")
                } else {
                    match a.checked_div(b) {
                        Some(v) => ValueV2::from_int(v),
                        None => rt_throw_v2(&format!("integer overflow: {} / {}", a, b)),
                    }
                }
            }
            BinOp::Mod => {
                if b == 0 {
                    rt_throw_v2("modulo by zero")
                } else {
                    ValueV2::from_int(a.wrapping_rem(b))
                }
            }
            BinOp::Eq => ValueV2::from_bool(a == b),
            BinOp::Ne => ValueV2::from_bool(a != b),
            BinOp::Lt => ValueV2::from_bool(a < b),
            BinOp::Le => ValueV2::from_bool(a <= b),
            BinOp::Gt => ValueV2::from_bool(a > b),
            BinOp::Ge => ValueV2::from_bool(a >= b),
        };
        write_out(out, res);
        return;
    }
    if lv.tag == ValueV2::TAG_FLOAT && rv.tag == ValueV2::TAG_FLOAT {
        let a = f64::from_bits(lv.payload);
        let b = f64::from_bits(rv.payload);
        let res = match op {
            BinOp::Add => ValueV2::from_float(a + b),
            BinOp::Sub => ValueV2::from_float(a - b),
            BinOp::Mul => ValueV2::from_float(a * b),
            BinOp::Div => {
                if b == 0.0 {
                    rt_throw_v2("division by zero")
                } else {
                    ValueV2::from_float(a / b)
                }
            }
            BinOp::Mod => {
                if b == 0.0 {
                    rt_throw_v2("modulo by zero")
                } else {
                    ValueV2::from_float(a % b)
                }
            }
            BinOp::Eq => ValueV2::from_bool(a == b),
            BinOp::Ne => ValueV2::from_bool(a != b),
            BinOp::Lt => ValueV2::from_bool(a < b),
            BinOp::Le => ValueV2::from_bool(a <= b),
            BinOp::Gt => ValueV2::from_bool(a > b),
            BinOp::Ge => ValueV2::from_bool(a >= b),
        };
        write_out(out, res);
        return;
    }
    if op == BinOp::Add {
        if let Some(res) = concat_str_v2(lv, rv) {
            write_out(out, res);
            return;
        }
    } else if (op == BinOp::Eq || op == BinOp::Ne) && lv.is_sso() && rv.is_sso() {
        let eq = lv.tag == rv.tag && lv.payload == rv.payload;
        write_out(
            out,
            ValueV2::from_bool(if op == BinOp::Eq { eq } else { !eq }),
        );
        return;
    }
    let l = v2_to_value(lv);
    let r = v2_to_value(rv);
    if let Some((fwd, rev)) = overload_dunders(op) {
        let over = native_try_operator(&l, &r, fwd).or_else(|| native_try_operator(&r, &l, rev));
        if let Some(res) = over {
            let v = match res {
                Ok(v) => from_value_v2(v),
                Err(msg) => rt_throw_v2(&msg),
            };
            write_out(out, v);
            return;
        }
    }
    let v = match vm_eval_binary(op, l, r) {
        Ok(v) => from_value_v2(v),
        Err(msg) => rt_throw_v2(&msg),
    };
    write_out(out, v);
}

#[no_mangle]
pub extern "C" fn spid3r_unop(out: *mut ValueV2, v: *const ValueV2, op: i64) {
    let op = match unop_from_u8(op as u8) {
        Some(o) => o,
        None => abort("native runtime: unknown unop"),
    };
    let vv = read_v2(v);
    if op == UnOp::Not {
        write_out(out, ValueV2::from_bool(!v2_truthy(vv)));
        return;
    }
    if op == UnOp::Neg {
        if vv.tag == ValueV2::TAG_INT {
            let a = vv.payload as i64;
            let res = match a.checked_neg() {
                Some(n) => ValueV2::from_int(n),
                None => rt_throw_v2(&format!("integer overflow: -{}", a)),
            };
            write_out(out, res);
            return;
        }
        if vv.tag == ValueV2::TAG_FLOAT {
            write_out(out, ValueV2::from_float(-f64::from_bits(vv.payload)));
            return;
        }
    }
    let val = v2_to_value(vv);
    if op == UnOp::Neg {
        if let Some(res) = native_try_unary_operator(&val, "__neg__") {
            let r = match res {
                Ok(v) => from_value_v2(v),
                Err(msg) => rt_throw_v2(&msg),
            };
            write_out(out, r);
            return;
        }
    }
    let r = match vm_eval_unary(op, val) {
        Ok(v) => from_value_v2(v),
        Err(msg) => rt_throw_v2(&msg),
    };
    write_out(out, r);
}

#[inline]
fn v2_truthy(v: ValueV2) -> bool {
    match v.tag {
        ValueV2::TAG_NULL => false,
        ValueV2::TAG_BOOL => v.payload != 0,
        ValueV2::TAG_INT => v.payload != 0,
        ValueV2::TAG_FLOAT => f64::from_bits(v.payload) != 0.0,
        ValueV2::TAG_ROPE | ValueV2::TAG_ROPE_ASCII => v.rope_len() != 0,
        _ if v.is_sso() => v.sso_len() != 0,
        ValueV2::TAG_HEAP => match deref_heap(v) {
            Some(val) => crate::value::truthy(val),
            None => false,
        },
        _ => false,
    }
}

#[no_mangle]
pub extern "C" fn spid3r_truthy(v: *const ValueV2) -> u8 {
    v2_truthy(read_v2(v)) as u8
}

#[no_mangle]
pub extern "C" fn spid3r_eq(l: *const ValueV2, r: *const ValueV2) -> u8 {
    let lv = read_v2(l);
    let rv = read_v2(r);
    if lv.tag == ValueV2::TAG_INT && rv.tag == ValueV2::TAG_INT {
        return (lv.payload == rv.payload) as u8;
    }
    if lv.tag == ValueV2::TAG_NULL && rv.tag == ValueV2::TAG_NULL {
        return 1;
    }
    if lv.tag == ValueV2::TAG_BOOL && rv.tag == ValueV2::TAG_BOOL {
        return ((lv.payload != 0) == (rv.payload != 0)) as u8;
    }
    if lv.is_sso() && rv.is_sso() {
        return (lv.tag == rv.tag && lv.payload == rv.payload) as u8;
    }
    if let Some(res) = with_str_v2(lv, |ls| with_str_v2(rv, |rs| ls == rs)) {
        return res.unwrap_or(false) as u8;
    }
    crate::value::values_equal(&v2_to_value(lv), &v2_to_value(rv)) as u8
}

/// Display a value the way `print` does. Returns a malloc'd C string that the
/// caller frees with `spid3r_cstr_free`.
#[no_mangle]
pub extern "C" fn spid3r_display(v: *const ValueV2) -> *mut c_char {
    let vv = read_v2(v);
    let s = match vv.tag {
        ValueV2::TAG_NULL => "null".to_string(),
        ValueV2::TAG_BOOL => if vv.payload != 0 { "true" } else { "false" }.to_string(),
        ValueV2::TAG_INT => (vv.payload as i64).to_string(),
        ValueV2::TAG_FLOAT => {
            crate::value::display_value(&Value::Float(f64::from_bits(vv.payload)))
        }
        ValueV2::TAG_ROPE | ValueV2::TAG_ROPE_ASCII => {
            with_rope_str(vv, |text| text.to_string()).unwrap_or_else(|| "null".to_string())
        }
        _ if vv.is_sso() => vv
            .with_sso_str(|text| text.to_string())
            .unwrap_or_else(|| "null".to_string()),
        ValueV2::TAG_HEAP => match deref_heap(vv) {
            Some(val) => crate::value::display_value(val),
            None => "null".to_string(),
        },
        _ => "null".to_string(),
    };
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn spid3r_cstr_free(p: *mut c_char) {
    if !p.is_null() {
        unsafe { drop(CString::from_raw(p)) };
    }
}

// ----------------------------------------------------------------------------
// extern "C" trampolines — globals
// ----------------------------------------------------------------------------

static GLOBAL_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[repr(C)]
pub struct GlobalCacheSlot {
    pub gen: std::sync::atomic::AtomicU64,
    pub val: std::sync::atomic::AtomicPtr<Value>,
}

fn fetch_global_value(name: *const c_char) -> Value {
    let key = cstr_ref(name);
    let hit = globals().lock().unwrap().get(key.as_ref()).cloned();
    match hit {
        Some(v) => v,
        None => FULL_STDLIB_RESOLVER
            .get()
            .and_then(|f| f(key.as_ref()))
            .map(Value::Native)
            .unwrap_or(Value::Null),
    }
}

#[no_mangle]
pub extern "C" fn spid3r_global_get(out: *mut ValueV2, name: *const c_char) {
    write_out(out, from_value_v2(fetch_global_value(name)));
}

/// Generation-guarded cached global lookup (`{ i64 gen, ptr val }` slot).
/// Fast path hits with zero mutexes, zero HashMap lookups, and zero heap
/// allocations (returning a pinned `ValueV2` directly).
#[no_mangle]
pub extern "C" fn spid3r_global_cached(
    out: *mut ValueV2,
    name: *const c_char,
    slot: *mut GlobalCacheSlot,
) {
    use std::sync::atomic::Ordering;
    let cur_gen = GLOBAL_GEN.load(Ordering::Acquire);
    // SAFETY: `slot` is a compiler-emitted static `GlobalCacheSlot`.
    let s = unsafe { &*slot };
    if s.gen.load(Ordering::Acquire) == cur_gen {
        let ptr = s.val.load(Ordering::Acquire);
        if !ptr.is_null() {
            // SAFETY: `ptr` was allocated via `Box::into_raw` and stays pinned.
            let v2 = match unsafe { &*ptr } {
                Value::Null => ValueV2::NULL,
                Value::Bool(b) => ValueV2::from_bool(*b),
                Value::Int(n) => ValueV2::from_int(*n),
                Value::Float(f) => ValueV2::from_float(*f),
                _ => ValueV2::from_heap_ptr(ptr),
            };
            write_out(out, v2);
            return;
        }
    }
    static CACHE_MISS_LOCK: Mutex<()> = Mutex::new(());
    let _guard = CACHE_MISS_LOCK.lock().unwrap();
    let gen_before = GLOBAL_GEN.load(Ordering::Acquire);
    let val = fetch_global_value(name);
    let v2 = match &val {
        Value::Null => ValueV2::NULL,
        Value::Bool(b) => ValueV2::from_bool(*b),
        Value::Int(n) => ValueV2::from_int(*n),
        Value::Float(f) => ValueV2::from_float(*f),
        _ => {
            let boxed = Box::into_raw(Box::new(val.clone()));
            if GLOBAL_GEN.load(Ordering::Acquire) == gen_before {
                s.val.store(boxed, Ordering::Release);
                s.gen.store(gen_before, Ordering::Release);
            }
            write_out(out, ValueV2::from_heap_ptr(boxed));
            return;
        }
    };
    let boxed = Box::into_raw(Box::new(val));
    if GLOBAL_GEN.load(Ordering::Acquire) == gen_before {
        s.val.store(boxed, Ordering::Release);
        s.gen.store(gen_before, Ordering::Release);
    }
    write_out(out, v2);
}

#[no_mangle]
pub extern "C" fn spid3r_global_set(name: *const c_char, v: *const ValueV2) {
    let key = cstr_ref(name);
    let val = to_owned_v2(v);
    {
        let mut g = globals().lock().unwrap();
        if let Some(slot) = g.get_mut(key.as_ref()) {
            *slot = val;
        } else {
            g.insert(key.into_owned(), val);
        }
    }
    GLOBAL_GEN.fetch_add(1, std::sync::atomic::Ordering::Release);
}

/// Define a global if it does not exist yet (used for top-level `let`).
#[no_mangle]
pub extern "C" fn spid3r_global_def(name: *const c_char, v: *const ValueV2) {
    let key = cstr_ref(name);
    let val = to_owned_v2(v);
    let mut changed = false;
    {
        let mut g = globals().lock().unwrap();
        if !g.contains_key(key.as_ref()) {
            g.insert(key.into_owned(), val);
            changed = true;
        }
    }
    if changed {
        GLOBAL_GEN.fetch_add(1, std::sync::atomic::Ordering::Release);
    }
}

#[no_mangle]
pub extern "C" fn spid3r_global_exists(name: *const c_char) -> u8 {
    ffi_guard(
        "global_exists",
        |_| 0,
        || {
            let key = cstr_ref(name);
            (globals().lock().unwrap().contains_key(key.as_ref())
                || FULL_STDLIB_RESOLVER
                    .get()
                    .and_then(|f| f(key.as_ref()))
                    .is_some()) as u8
        },
    )
}

// ----------------------------------------------------------------------------
// extern "C" trampolines — calls
// ----------------------------------------------------------------------------

unsafe fn args_vec(argc: usize, argv: *const *const ValueV2) -> Vec<Value> {
    let mut v = Vec::with_capacity(argc);
    for i in 0..argc {
        let p = *argv.add(i);
        v.push(to_owned_v2(p));
    }
    v
}

/// Call any callable value: compiled function, native stdlib function, or
/// compiled class (construct an instance and run its constructor).
#[no_mangle]
pub extern "C" fn spid3r_call(
    out: *mut ValueV2,
    f: *const ValueV2,
    argc: usize,
    argv: *const *const ValueV2,
) {
    if argc == 1 && !argv.is_null() {
        if let Some(Value::Native(n)) = deref_heap(read_v2(f)) {
            // SAFETY: `argc == 1 && !argv.is_null()` guarantees `*argv` is readable.
            let arg0_ptr = unsafe { *argv };
            if !arg0_ptr.is_null() {
                if n.name == "string" {
                    let arg0 = read_v2(arg0_ptr);
                    if arg0.tag == ValueV2::TAG_INT {
                        let n_i64 = arg0.payload as i64;
                        let v = ValueV2::from_int_decimal(n_i64).unwrap_or_else(|| {
                            alloc_heap_v2(Value::Str(crate::stdlib::int_to_string_value(n_i64)))
                        });
                        write_out(out, cancel_guard(v));
                        return;
                    }
                } else if n.name == "json_write" {
                    if let Some(write_fn) = JSON_WRITE_HOOK.get() {
                        let arg0 = read_v2(arg0_ptr);
                        let owned;
                        let val_ref = if let Some(v) = deref_heap(arg0) {
                            v
                        } else {
                            owned = v2_to_value(arg0);
                            &owned
                        };
                        let res = match write_fn(val_ref) {
                            Ok(arc) => from_value_v2(Value::Str(arc)),
                            Err(msg) => rt_throw_v2(&msg),
                        };
                        write_out(out, cancel_guard(res));
                        return;
                    }
                } else if n.name == "json_parse" {
                    if let Some(parse_fn) = JSON_PARSE_HOOK.get() {
                        let arg0 = read_v2(arg0_ptr);
                        if let Some(parsed) = with_str_v2(arg0, *parse_fn) {
                            let res = match parsed {
                                Ok(v) => from_value_v2(v),
                                Err(msg) => rt_throw_v2(&msg),
                            };
                            write_out(out, cancel_guard(res));
                            return;
                        }
                    }
                }
            }
        }
    }
    ffi_v2(out, "call", || spid3r_call_inner(f, argc, argv));
}

fn cancel_guard(r: ValueV2) -> ValueV2 {
    if THROWN_FLAG.with(|f| f.get()) == 0 && crate::sched::current_task_cancelled() {
        return rt_throw_v2("task cancelled");
    }
    r
}

fn spid3r_call_inner(f: *const ValueV2, argc: usize, argv: *const *const ValueV2) -> ValueV2 {
    cancel_guard(spid3r_call_inner_dispatch(f, argc, argv))
}

fn spid3r_call_inner_dispatch(
    f: *const ValueV2,
    argc: usize,
    argv: *const *const ValueV2,
) -> ValueV2 {
    let fv = read_v2(f);
    let Some(f_val) = deref_heap(fv) else {
        let owned = v2_to_value(fv);
        return rt_throw_v2(&format!(
            "{} is not callable",
            crate::value::type_name(&owned)
        ));
    };
    match f_val {
        Value::Cfn(c) => {
            if argc < c.min_arity || argc > c.arity {
                return rt_throw_v2(&format!(
                    "{} expects {} argument(s), got {}",
                    c.name.as_deref().unwrap_or("<fn>"),
                    c.arity,
                    argc
                ));
            }
            if !enter_call() {
                return rt_throw_v2("stack overflow: call depth limit reached");
            }
            let mut out = ValueV2::NULL;
            // SAFETY: `c.code` is a compiled Spid3r function pointer with matching ABI.
            unsafe { (c.code)(&mut out, c.env, argc, argv) };
            leave_call();
            out
        }
        Value::Native(n) => {
            if n.name == "string" && argc == 1 {
                // SAFETY: `argc == 1` guarantees `argv` has at least 1 valid pointer.
                let arg0 = read_v2(unsafe { *argv });
                if arg0.tag == ValueV2::TAG_INT {
                    return alloc_heap_v2(Value::Str(crate::stdlib::int_to_string_value(
                        arg0.payload as i64,
                    )));
                }
            }
            let func = n.func;
            let args = unsafe { args_vec(argc, argv) };
            let ctx = NativeCtx {
                sched: Scheduler::shared(),
            };
            match func(&ctx, args) {
                Ok(v) => from_value_v2(v),
                Err(msg) => rt_throw_v2(&msg),
            }
        }
        Value::NativeClosure(n) => {
            let func = n.func.clone();
            let args = unsafe { args_vec(argc, argv) };
            let ctx = NativeCtx {
                sched: Scheduler::shared(),
            };
            match func(&ctx, args) {
                Ok(v) => from_value_v2(v),
                Err(msg) => rt_throw_v2(&msg),
            }
        }
        Value::CClass(c) => construct_cinstance(c.clone(), unsafe { args_vec(argc, argv) }),
        other => rt_throw_v2(&format!(
            "{} is not callable",
            crate::value::type_name(other)
        )),
    }
}

/// Arity check shared by calls of compiled functions (methods/constructors).
/// Returns the catchable error message on a mismatch (`None` = ok) instead of
/// aborting — a wrong-arity callback is a script-level runtime error on the
/// tree/VM engines, so native must not kill the process (F-9).
pub fn check_arity(name: &str, arity: usize, got: usize) -> Option<String> {
    if arity != got {
        return Some(format!(
            "{} expects {} argument(s), got {}",
            name,
            arity.saturating_sub(1),
            got
        ));
    }
    None
}

// ----------------------------------------------------------------------------
// call-depth guard (mirrors MAX_CALL_DEPTH in the VM and tree interpreter)
// ----------------------------------------------------------------------------

thread_local! {
    static CALL_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Enter a compiled call. Returns `false` (recording nothing) when the depth
/// limit is hit: the caller must NOT enter generated code in that case and
/// instead raises the same catchable diagnostic the VM/tree interpreters
/// produce (F-9) — a native stack overflow would otherwise kill the process
/// silently, and `abort` made the error uncatchable by `try`/`catch`.
fn enter_call() -> bool {
    let ok = CALL_DEPTH.with(|d| {
        let n = d.get() + 1;
        if n > crate::vm::MAX_CALL_DEPTH {
            return false;
        }
        d.set(n);
        true
    });
    if ok {
        let prev = CALL_SLAB_ALLOCS.with(|c| {
            let p = c.get();
            c.set(0);
            p
        });
        CALL_ALLOC_STACK.with(|s| s.borrow_mut().push(prev));
    }
    ok
}

fn leave_call() {
    CALL_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    let used = CALL_SLAB_ALLOCS.with(|c| {
        let u = c.get();
        let prev = CALL_ALLOC_STACK.with(|s| s.borrow_mut().pop().unwrap_or(0));
        c.set(prev);
        u
    });
    LAST_CALL_SLAB_ALLOCS.store(used, std::sync::atomic::Ordering::Relaxed);
    MAX_CALL_SLAB_ALLOCS.fetch_max(used, std::sync::atomic::Ordering::Relaxed);
    COMPLETED_CALL_SLAB_ALLOCS.fetch_add(used, std::sync::atomic::Ordering::Relaxed);
}

/// Build a `Value::Cfn` for a compiled function (called from closures).
#[no_mangle]
pub extern "C" fn spid3r_make_fn(
    out: *mut ValueV2,
    name: *const c_char,
    arity: usize,
    min_arity: usize,
    code: *const c_void,
    env: *mut c_void,
) {
    ffi_v2(out, "make_fn", || {
        make_fn_inner(name, arity, min_arity, code, env)
    });
}

fn make_fn_inner(
    name: *const c_char,
    arity: usize,
    min_arity: usize,
    code: *const c_void,
    env: *mut c_void,
) -> ValueV2 {
    let code: unsafe extern "C" fn(*mut ValueV2, *mut c_void, usize, *const *const ValueV2) =
        unsafe { std::mem::transmute(code) };
    let name = if name.is_null() {
        None
    } else {
        let s = cstr(name);
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    };
    alloc_heap_v2(Value::Cfn(std::sync::Arc::new(CompiledFn {
        name,
        arity,
        min_arity,
        code,
        env,
    })))
}

// ----------------------------------------------------------------------------
// extern "C" trampolines — builtin constructors, methods, indexing, fields
// ----------------------------------------------------------------------------

/// `list(...)`, `map(...)`, `stack(...)` ... via the shared collections layer.
#[no_mangle]
pub extern "C" fn spid3r_construct(
    out: *mut ValueV2,
    name: *const c_char,
    argc: usize,
    argv: *const *const ValueV2,
) {
    let name = cstr(name);
    let args = unsafe { args_vec(argc, argv) };
    let v = match collections::construct(&name, args) {
        Ok(v) => from_value_v2(v),
        Err(msg) => rt_throw_v2(&msg),
    };
    write_out(out, v);
}

/// `obj.method(args)` — class methods for compiled instances, everything else
/// delegates to the shared collections layer (strings, lists, maps, ...).
#[no_mangle]
pub extern "C" fn spid3r_method(
    out: *mut ValueV2,
    obj: *const ValueV2,
    name: *const c_char,
    argc: usize,
    argv: *const *const ValueV2,
) {
    ffi_v2(out, "method", || {
        cancel_guard(spid3r_method_inner(obj, name, argc, argv))
    });
}

fn invoke_cfn(
    obj: *const ValueV2,
    cfn: &CompiledFn,
    name: &str,
    argc: usize,
    argv: *const *const ValueV2,
) -> ValueV2 {
    if argc + 1 < cfn.min_arity || argc + 1 > cfn.arity {
        let want = if cfn.min_arity == cfn.arity {
            format!("{} argument(s)", cfn.arity - 1)
        } else {
            format!("{} to {} argument(s)", cfn.min_arity - 1, cfn.arity - 1)
        };
        return rt_throw_v2(&format!("method {} expects {}, got {}", name, want, argc));
    }
    let env = cfn.env;
    let code = cfn.code;
    let mut call_args: Vec<*const ValueV2> = Vec::with_capacity(argc + 1);
    call_args.push(obj);
    for i in 0..argc {
        let p = unsafe { *argv.add(i) };
        call_args.push(p);
    }
    if !enter_call() {
        return rt_throw_v2("stack overflow: call depth limit reached");
    }
    let mut out = ValueV2::NULL;
    unsafe { (code)(&mut out, env, argc + 1, call_args.as_ptr()) };
    leave_call();
    out
}

/// Slot-based devirtualized method dispatch (`self.m(...)`). Falls back to
/// `spid3r_method_inner` when `slot < 0` or the receiver's runtime class ID
/// differs from `expected_class_id` (e.g. a subclass instance).
#[no_mangle]
pub extern "C" fn spid3r_method_slot(
    out: *mut ValueV2,
    obj: *const ValueV2,
    expected_class_id: u64,
    slot: i64,
    name: *const c_char,
    argc: usize,
    argv: *const *const ValueV2,
) {
    ffi_v2(out, "method_slot", || {
        cancel_guard({
            if slot >= 0 && expected_class_id != 0 {
                if let Some(Value::CInstance(inst)) = deref_heap(read_v2(obj)) {
                    let cfn = {
                        let g = inst.lock().unwrap();
                        if g.class.class_id == expected_class_id {
                            g.class.vtable.get(slot as usize).cloned()
                        } else {
                            None
                        }
                    };
                    if let Some(cfn) = cfn {
                        let name_s = cstr_ref(name);
                        return invoke_cfn(obj, &cfn, name_s.as_ref(), argc, argv);
                    }
                }
            }
            spid3r_method_inner(obj, name, argc, argv)
        })
    });
}

fn spid3r_method_inner(
    obj: *const ValueV2,
    name: *const c_char,
    argc: usize,
    argv: *const *const ValueV2,
) -> ValueV2 {
    let ov = read_v2(obj);
    let name = cstr_ref(name);
    if let Some(object) = deref_heap(ov) {
        let maybe_class = match object {
            Value::CInstance(inst) => Some(inst.lock().unwrap().class.clone()),
            Value::CClass(c) => Some(c.clone()),
            _ => None,
        };
        if let Some(class) = maybe_class {
            if let Some((_, cfn)) = find_method_cclass(&class, name.as_ref()) {
                return invoke_cfn(obj, &cfn, name.as_ref(), argc, argv);
            }
            return rt_throw_v2(&format!("class {} has no method '{}'", class.name, name));
        }
        let args = unsafe { args_vec(argc, argv) };
        return match collections::call_method_host(
            &mut NativeCallbackHost,
            object,
            name.as_ref(),
            args,
        ) {
            Ok(v) => from_value_v2(v),
            Err(msg) => match crate::sched::take_error_thrown() {
                Some(v) => rt_throw_value_v2(v),
                None => rt_throw_v2(&msg),
            },
        };
    }
    if ov.is_sso() || ov.is_rope() {
        let args = unsafe { args_vec(argc, argv) };
        if let Some(res) = with_str_v2(ov, |s| collections::string_method(s, name.as_ref(), &args))
        {
            return match res {
                Ok(v) => from_value_v2(v),
                Err(msg) => match crate::sched::take_error_thrown() {
                    Some(v) => rt_throw_value_v2(v),
                    None => rt_throw_v2(&msg),
                },
            };
        }
    }
    let owned_obj = v2_to_value(ov);
    let args = unsafe { args_vec(argc, argv) };
    match collections::call_method_host(&mut NativeCallbackHost, &owned_obj, name.as_ref(), args) {
        Ok(v) => from_value_v2(v),
        Err(msg) => match crate::sched::take_error_thrown() {
            Some(v) => rt_throw_value_v2(v),
            None => rt_throw_v2(&msg),
        },
    }
}

#[no_mangle]
pub extern "C" fn spid3r_super_call(
    out: *mut ValueV2,
    recv: *const ValueV2,
    parent_name: *const c_char,
    method_name: *const c_char,
    argc: usize,
    argv: *const *const ValueV2,
) {
    ffi_v2(out, "super", || {
        spid3r_super_call_inner(recv, parent_name, method_name, argc, argv)
    });
}

fn spid3r_super_call_inner(
    recv: *const ValueV2,
    parent_name: *const c_char,
    method_name: *const c_char,
    argc: usize,
    argv: *const *const ValueV2,
) -> ValueV2 {
    let object = to_owned_v2(recv);
    match object {
        Value::CInstance(_) => {}
        _ => {
            return rt_throw_v2("super is only available inside a class method");
        }
    }
    let parent = cstr(parent_name);
    if parent.is_empty() {
        return rt_throw_v2("super is not available: this class has no parent class");
    }
    let class = {
        let g = globals().lock().unwrap();
        match g.get(&parent) {
            Some(Value::CClass(c)) => c.clone(),
            Some(_) => return rt_throw_v2(&format!("'{}' is not a class", parent)),
            None => return rt_throw_v2(&format!("unknown class '{}'", parent)),
        }
    };
    let args = unsafe { args_vec(argc, argv) };
    if method_name.is_null() {
        let mut cur = Some(class.clone());
        let mut found = None;
        while let Some(c) = cur {
            if let Some(ct) = &c.ctor {
                found = Some((c.name.clone(), ct.clone()));
                break;
            }
            cur = c.parent.clone();
        }
        let (ctor_class_name, ctor) = match found {
            Some(pair) => pair,
            None => {
                return rt_throw_v2(&format!("class '{}' has no constructor", class.name));
            }
        };
        if argc + 1 < ctor.min_arity || argc + 1 > ctor.arity {
            let want = if ctor.min_arity == ctor.arity {
                format!("{} argument(s)", ctor.arity.saturating_sub(1))
            } else {
                format!(
                    "{} to {} argument(s)",
                    ctor.min_arity.saturating_sub(1),
                    ctor.arity.saturating_sub(1)
                )
            };
            return rt_throw_v2(&format!(
                "constructor {}::new expects {}, got {}",
                ctor_class_name, want, argc
            ));
        }
        let mut frame_vals: Vec<ValueV2> = Vec::with_capacity(argc + 1);
        frame_vals.push(from_value_v2(object.clone()));
        for a in &args {
            frame_vals.push(from_value_v2(a.clone()));
        }
        let call_args: Vec<*const ValueV2> =
            frame_vals.iter().map(|v| v as *const ValueV2).collect();
        let env = ctor.env;
        let code = ctor.code;
        if !enter_call() {
            return rt_throw_v2("stack overflow: call depth limit reached");
        }
        let mut out = ValueV2::NULL;
        spid3r_frame_enter(frame_vals.as_ptr(), frame_vals.len());
        unsafe { (code)(&mut out, env, argc + 1, call_args.as_ptr()) };
        spid3r_frame_exit();
        leave_call();
        return ValueV2::NULL;
    }
    let m = cstr(method_name);
    let (_, f) = match find_method_cclass(&class, &m) {
        Some(x) => x,
        None => {
            return rt_throw_v2(&format!("class {} has no method '{}'", class.name, m));
        }
    };
    if argc + 1 < f.min_arity || argc + 1 > f.arity {
        let want = if f.min_arity == f.arity {
            format!("{} argument(s)", f.arity.saturating_sub(1))
        } else {
            format!(
                "{} to {} argument(s)",
                f.min_arity.saturating_sub(1),
                f.arity.saturating_sub(1)
            )
        };
        return rt_throw_v2(&format!("method {} expects {}, got {}", m, want, argc));
    }
    let mut frame_vals: Vec<ValueV2> = Vec::with_capacity(argc + 1);
    frame_vals.push(from_value_v2(object.clone()));
    for a in &args {
        frame_vals.push(from_value_v2(a.clone()));
    }
    let call_args: Vec<*const ValueV2> = frame_vals.iter().map(|v| v as *const ValueV2).collect();
    let env = f.env;
    let code = f.code;
    if !enter_call() {
        return rt_throw_v2("stack overflow: call depth limit reached");
    }
    let mut out = ValueV2::NULL;
    spid3r_frame_enter(frame_vals.as_ptr(), frame_vals.len());
    unsafe { (code)(&mut out, env, argc + 1, call_args.as_ptr()) };
    spid3r_frame_exit();
    leave_call();
    out
}

struct NativeCallbackHost;

impl collections::CallbackHost for NativeCallbackHost {
    fn call_callback(&mut self, f: &Value, args: Vec<Value>) -> Result<Value, String> {
        match f {
            Value::Cfn(cfn) => {
                if args.len() < cfn.min_arity || args.len() > cfn.arity {
                    let want = if cfn.min_arity == cfn.arity {
                        format!("{} argument(s)", cfn.arity)
                    } else {
                        format!("{} to {} argument(s)", cfn.min_arity, cfn.arity)
                    };
                    return Err(format!(
                        "{} expects {}, got {}",
                        cfn.name.as_deref().unwrap_or("fn"),
                        want,
                        args.len()
                    ));
                }
                let frame_vals: Vec<ValueV2> =
                    args.iter().map(|a| from_value_v2(a.clone())).collect();
                let argv: Vec<*const ValueV2> =
                    frame_vals.iter().map(|v| v as *const ValueV2).collect();
                let code = cfn.code;
                let env = cfn.env;
                if !enter_call() {
                    return Err("stack overflow: call depth limit reached".to_string());
                }
                let mut out = ValueV2::NULL;
                spid3r_frame_enter(frame_vals.as_ptr(), frame_vals.len());
                unsafe { (code)(&mut out, env, args.len(), argv.as_ptr()) };
                spid3r_frame_exit();
                leave_call();
                if spid3r_threw() != 0 {
                    let msg = THROWN.with(|t| match t.borrow().as_ref() {
                        Some(v) => crate::value::display_value(v),
                        None => "throw in callback".to_string(),
                    });
                    return Err(msg);
                }
                Ok(v2_to_value(out))
            }
            Value::Native(n) => {
                let ctx = crate::value::NativeCtx {
                    sched: crate::sched::Scheduler::shared(),
                };
                (n.func)(&ctx, args)
            }
            Value::NativeClosure(n) => {
                let ctx = crate::value::NativeCtx {
                    sched: crate::sched::Scheduler::shared(),
                };
                (n.func)(&ctx, args)
            }
            Value::CClass(c) => {
                let r = construct_cinstance(c.clone(), args);
                if spid3r_threw() != 0 {
                    let msg = THROWN.with(|t| match t.borrow().as_ref() {
                        Some(v) => crate::value::display_value(v),
                        None => "throw in constructor".to_string(),
                    });
                    return Err(msg);
                }
                Ok(v2_to_value(r))
            }
            other => Err(format!(
                "{} is not callable in the native backend (interpreted closures \
                 from the tree engine cannot run in compiled code)",
                crate::value::type_name(other)
            )),
        }
    }
}

fn find_method_cclass(
    class: &std::sync::Arc<crate::value::CompiledClass>,
    name: &str,
) -> Option<(crate::ast::Vis, std::sync::Arc<CompiledFn>)> {
    let mut cur = Some(class.clone());
    while let Some(c) = cur {
        if let Some((vis, f)) = c.methods.get(name) {
            return Some((*vis, f.clone()));
        }
        cur = c.parent.clone();
    }
    None
}

#[inline]
fn index_get_inner(out: *mut ValueV2, obj: *const ValueV2, idx: *const ValueV2) -> i8 {
    let ov = read_v2(obj);
    let iv = read_v2(idx);
    if let Some(object) = deref_heap(ov) {
        let res = match object {
            Value::Map(m) if iv.is_sso() => {
                let mut sso_buf = [0u8; 16];
                let sso_len = iv.write_sso_bytes(&mut sso_buf);
                let raw = &sso_buf[1..1 + sso_len];
                let guard = crate::value::lock_ref(m);
                return match guard.find_str_bytes(raw) {
                    Some(i) => {
                        write_out(out, value_ref_to_v2(&guard.entries[i].1));
                        0
                    }
                    None => {
                        let k = std::str::from_utf8(raw).unwrap_or("");
                        rt_throw_v2(&format!("missing key {}", k));
                        1
                    }
                };
            }
            Value::Map(m) if iv.tag == ValueV2::TAG_INT => {
                let n = iv.payload as i64;
                let guard = crate::value::lock_ref(m);
                return match guard.find(&crate::value::MapKey::Int(n)) {
                    Some(i) => {
                        write_out(out, value_ref_to_v2(&guard.entries[i].1));
                        0
                    }
                    None => {
                        rt_throw_v2(&format!("missing key {}", n));
                        1
                    }
                };
            }
            Value::Map(m) => {
                if let Some(found) = with_str_v2(iv, |k| {
                    let guard = crate::value::lock_ref(m);
                    match guard.find_str(k) {
                        Some(i) => Ok(value_ref_to_v2(&guard.entries[i].1)),
                        None => Err(format!("missing key {}", k)),
                    }
                }) {
                    return match found {
                        Ok(v) => {
                            write_out(out, v);
                            0
                        }
                        Err(msg) => {
                            rt_throw_v2(&msg);
                            1
                        }
                    };
                }
                collections::index_get(object, &v2_to_value(iv))
            }
            Value::List(d) | Value::VecDyn(d) if iv.tag == ValueV2::TAG_INT => {
                let guard = crate::value::lock_ref(d);
                let items = guard.as_slice();
                let len = items.len();
                let i = iv.payload as i64;
                let resolved = if i < 0 { len as i64 + i } else { i };
                return if resolved >= 0 && (resolved as usize) < len {
                    write_out(out, value_ref_to_v2(&items[resolved as usize]));
                    0
                } else {
                    rt_throw_v2(&format!("index {} out of bounds for length {}", i, len));
                    1
                };
            }
            Value::Bytes(b) if iv.tag == ValueV2::TAG_INT => {
                let guard = crate::value::lock_ref(b);
                let len = guard.len();
                let i = iv.payload as i64;
                let resolved = if i < 0 { len as i64 + i } else { i };
                if resolved >= 0 && (resolved as usize) < len {
                    write_out(out, ValueV2::from_int(guard[resolved as usize] as i64));
                    return 0;
                }
                drop(guard);
                collections::index_get(object, &v2_to_value(iv))
            }
            _ => collections::index_get(object, &v2_to_value(iv)),
        };
        return match res {
            Ok(v) => {
                write_out(out, from_value_v2(v));
                0
            }
            Err(msg) => {
                rt_throw_v2(&msg);
                1
            }
        };
    }
    match collections::index_get(&v2_to_value(ov), &v2_to_value(iv)) {
        Ok(v) => {
            write_out(out, from_value_v2(v));
            0
        }
        Err(msg) => {
            rt_throw_v2(&msg);
            1
        }
    }
}

#[no_mangle]
pub extern "C" fn spid3r_index_get(out: *mut ValueV2, obj: *const ValueV2, idx: *const ValueV2) {
    let _ = index_get_inner(out, obj, idx);
}

/// Phase 2: index lookup returning the throw flag (`0` ok, `1` threw) so
/// generated code avoids a separate `spid3r_threw()` FFI call.
#[no_mangle]
pub extern "C" fn spid3r_index_get_chk(
    out: *mut ValueV2,
    obj: *const ValueV2,
    idx: *const ValueV2,
) -> i8 {
    index_get_inner(out, obj, idx)
}

#[no_mangle]
pub extern "C" fn spid3r_list_len(out: *mut ValueV2, obj: *const ValueV2) {
    let ov = read_v2(obj);
    if ov.is_sso() {
        let n = ov
            .with_sso_str(|s| {
                if s.is_ascii() {
                    s.len()
                } else {
                    s.chars().count()
                }
            })
            .unwrap_or(0);
        write_out(out, ValueV2::from_int(n as i64));
        return;
    }
    if ov.is_rope() {
        if let Some(n) = with_rope_str(ov, |s| {
            if s.is_ascii() {
                s.len()
            } else {
                s.chars().count()
            }
        }) {
            write_out(out, ValueV2::from_int(n as i64));
            return;
        }
    }
    if let Some(object) = deref_heap(ov) {
        match object {
            Value::List(d) | Value::VecDyn(d) => {
                write_out(
                    out,
                    ValueV2::from_int(crate::value::lock_ref(d).len() as i64),
                );
                return;
            }
            Value::Str(s) => {
                let n = if s.is_ascii() {
                    s.len()
                } else {
                    s.chars().count()
                };
                write_out(out, ValueV2::from_int(n as i64));
                return;
            }
            Value::Map(m) => {
                let guard = crate::value::lock_ref(m);
                if !guard.namespace {
                    write_out(out, ValueV2::from_int(guard.entries.len() as i64));
                    return;
                }
            }
            Value::Bytes(b) => {
                write_out(
                    out,
                    ValueV2::from_int(crate::value::lock_ref(b).len() as i64),
                );
                return;
            }
            _ => {}
        }
    }
    ffi_v2(out, "list_len", || {
        cancel_guard(spid3r_method_inner(
            obj,
            c"len".as_ptr(),
            0,
            std::ptr::null(),
        ))
    });
}

#[no_mangle]
pub extern "C" fn spid3r_list_push(out: *mut ValueV2, obj: *const ValueV2, val: *const ValueV2) {
    if let Some(Value::List(d) | Value::VecDyn(d)) = deref_heap(read_v2(obj)) {
        let owned = to_owned_v2(val);
        let non_scalar = !collections::seq::is_non_recursive_scalar(&owned);
        if non_scalar {
            collections::seq::track_if_untracked(&owned);
        }
        let track_self = {
            let mut g = crate::value::lock_ref(d);
            let nt = non_scalar && g.needs_track_for(&owned);
            g.push_item(owned);
            nt
        };
        if track_self {
            crate::value::gc::track(d.clone());
        }
        write_out(out, ValueV2::NULL);
        return;
    }
    ffi_v2(out, "list_push", || {
        let argv = [val];
        cancel_guard(spid3r_method_inner(obj, c"push".as_ptr(), 1, argv.as_ptr()))
    });
}

/// Phase 2: format `i64` in decimal into a stack buffer with no heap allocation.
#[inline]
fn format_i64_stack(n: i64, buf: &mut [u8; 24]) -> &[u8] {
    let mut mag = n.unsigned_abs();
    let mut pos = buf.len();
    loop {
        pos -= 1;
        buf[pos] = b'0' + (mag % 10) as u8;
        mag /= 10;
        if mag == 0 {
            break;
        }
    }
    if n < 0 {
        pos -= 1;
        buf[pos] = b'-';
    }
    &buf[pos..]
}

/// Phase 2: convert an `i64` directly into an inline SSO or `TAG_ROPE_ASCII`
/// string without heap/Arc/call overhead.
#[no_mangle]
pub extern "C" fn spid3r_str_from_i64(out: *mut ValueV2, val: i64) {
    if let Some(v) = ValueV2::from_int_decimal(val) {
        write_out(out, v);
        return;
    }
    let mut buf = [0u8; 24];
    let slice = format_i64_stack(val, &mut buf);
    // SAFETY: `format_i64_stack` emits only ASCII digits and `-`.
    let text = unsafe { std::str::from_utf8_unchecked(slice) };
    write_out(out, alloc_rope_from_str(text, true));
}

/// Phase 2: build `<literal><i64>` directly into SSO or an `AOT_ROPE` ring slot.
#[no_mangle]
pub extern "C" fn spid3r_str_lit_i64(
    out: *mut ValueV2,
    lit_ptr: *const u8,
    lit_len: usize,
    val: i64,
) {
    // SAFETY: `lit_ptr` points to a compiler-emitted static valid UTF-8 literal.
    let lit = if lit_len == 0 || lit_ptr.is_null() {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(lit_ptr, lit_len) }
    };
    if lit_len <= 13 && (0..100).contains(&val) {
        let mut raw = [0u8; 16];
        let n = val as u8;
        if n < 10 {
            let total = lit_len + 1;
            raw[0] = (ValueV2::TAG_SSO_BASE as u8) + (total as u8);
            raw[1..1 + lit_len].copy_from_slice(lit);
            raw[1 + lit_len] = b'0' + n;
        } else {
            let total = lit_len + 2;
            raw[0] = (ValueV2::TAG_SSO_BASE as u8) + (total as u8);
            raw[1..1 + lit_len].copy_from_slice(lit);
            raw[1 + lit_len] = b'0' + (n / 10);
            raw[2 + lit_len] = b'0' + (n % 10);
        }
        write_out(
            out,
            ValueV2 {
                tag: u64::from_le_bytes([
                    raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
                ]),
                payload: u64::from_le_bytes([
                    raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15],
                ]),
            },
        );
        return;
    }
    let mut num_buf = [0u8; 24];
    let digits = format_i64_stack(val, &mut num_buf);
    let total = lit.len() + digits.len();
    if total <= 15 {
        let mut combined = [0u8; 15];
        combined[..lit.len()].copy_from_slice(lit);
        combined[lit.len()..total].copy_from_slice(digits);
        write_out(out, ValueV2::from_sso_bytes(&combined[..total]));
        return;
    }
    let lit_str = unsafe { std::str::from_utf8_unchecked(lit) };
    let digits_str = unsafe { std::str::from_utf8_unchecked(digits) };
    let ascii = lit_str.is_ascii();
    if total > u32::MAX as usize {
        let mut s = String::with_capacity(total);
        s.push_str(lit_str);
        s.push_str(digits_str);
        write_out(out, alloc_heap_v2(Value::Str(s.into())));
        return;
    }
    let v = alloc_rope_into(total, ascii, |buf| {
        buf.push_str(lit_str);
        buf.push_str(digits_str);
    });
    write_out(out, v);
}

/// Phase 2: descriptor element passed by generated code to [`spid3r_str_build`].
/// `kind == 0`: static UTF-8 `(ptr, len)`;
/// `kind == 1`: raw `i64` value `(val, 0)`;
/// `kind == 2`: `*const ValueV2` handle `(ptr, 0)`.
#[repr(C)]
pub struct StrPart {
    pub kind: u64,
    pub a: u64,
    pub b: u64,
}

#[cold]
#[inline(never)]
fn str_build_slow_fallback(out: *mut ValueV2, parts: &[StrPart]) {
    let mut vals: Vec<ValueV2> = Vec::with_capacity(parts.len());
    for p in parts {
        let v = match p.kind {
            0 => {
                let mut tmp = ValueV2::NULL;
                spid3r_value_str(&mut tmp, p.a as usize as *const u8, p.b as usize);
                tmp
            }
            1 => {
                let mut tmp = ValueV2::NULL;
                spid3r_str_from_i64(&mut tmp, p.a as i64);
                tmp
            }
            _ => read_v2(p.a as usize as *const ValueV2),
        };
        vals.push(v);
    }
    if vals.is_empty() {
        write_out(out, ValueV2::from_sso_bytes(b""));
        return;
    }
    let mut acc = vals[0];
    let add_op = binop_code_add();
    for item in &vals[1..] {
        let mut next = ValueV2::NULL;
        spid3r_binop(&mut next, &acc, item, add_op);
        if spid3r_threw() != 0 {
            write_out(out, next);
            return;
        }
        acc = next;
    }
    write_out(out, acc);
}

#[inline]
const fn binop_code_add() -> i64 {
    // Matches `BinOp::Add => 0` in `binop_from_u8`.
    0
}

/// Phase 2: concatenate N heterogeneous string/int parts directly into an SSO or
/// ring `AOT_ROPE` value, falling back to left-to-right `spid3r_binop` if any
/// `ValueV2` part is non-string (preserving `__radd__` and type-error semantics).
#[no_mangle]
pub extern "C" fn spid3r_str_build(out: *mut ValueV2, parts: *const StrPart, count: usize) {
    if count == 0 {
        write_out(out, ValueV2::from_sso_bytes(b""));
        return;
    }
    if parts.is_null() {
        abort("native runtime: null StrPart array");
    }
    // SAFETY: caller passes `count` initialized `StrPart` descriptors on stack.
    let slice = unsafe { std::slice::from_raw_parts(parts, count) };
    let mut stack_buf = [0u8; 256];
    let mut stack_len = 0usize;
    let mut overflow: Option<String> = None;
    let mut all_ascii = true;

    for p in slice {
        match p.kind {
            0 => {
                let ptr = p.a as usize as *const u8;
                let len = p.b as usize;
                let bytes = if len == 0 || ptr.is_null() {
                    &[][..]
                } else {
                    // SAFETY: compiler-emitted static valid UTF-8 literal.
                    unsafe { std::slice::from_raw_parts(ptr, len) }
                };
                // SAFETY: compiler-emitted literals are valid UTF-8.
                let s = unsafe { std::str::from_utf8_unchecked(bytes) };
                all_ascii &= s.is_ascii();
                if let Some(ref mut ob) = overflow {
                    ob.push_str(s);
                } else if stack_len + bytes.len() <= stack_buf.len() {
                    stack_buf[stack_len..stack_len + bytes.len()].copy_from_slice(bytes);
                    stack_len += bytes.len();
                } else {
                    let mut ob = String::with_capacity(stack_len + bytes.len() + 64);
                    // SAFETY: accumulated prefix consists solely of valid UTF-8 parts.
                    ob.push_str(unsafe { std::str::from_utf8_unchecked(&stack_buf[..stack_len]) });
                    ob.push_str(s);
                    overflow = Some(ob);
                }
            }
            1 => {
                let mut nbuf = [0u8; 24];
                let digits = format_i64_stack(p.a as i64, &mut nbuf);
                if let Some(ref mut ob) = overflow {
                    // SAFETY: digits are pure ASCII.
                    ob.push_str(unsafe { std::str::from_utf8_unchecked(digits) });
                } else if stack_len + digits.len() <= stack_buf.len() {
                    stack_buf[stack_len..stack_len + digits.len()].copy_from_slice(digits);
                    stack_len += digits.len();
                } else {
                    let mut ob = String::with_capacity(stack_len + digits.len() + 64);
                    ob.push_str(unsafe { std::str::from_utf8_unchecked(&stack_buf[..stack_len]) });
                    ob.push_str(unsafe { std::str::from_utf8_unchecked(digits) });
                    overflow = Some(ob);
                }
            }
            _ => {
                let v = read_v2(p.a as usize as *const ValueV2);
                let ok = with_str_v2(v, |s| {
                    all_ascii &= v.tag == ValueV2::TAG_ROPE_ASCII || s.is_ascii();
                    let bytes = s.as_bytes();
                    if let Some(ref mut ob) = overflow {
                        ob.push_str(s);
                    } else if stack_len + bytes.len() <= stack_buf.len() {
                        stack_buf[stack_len..stack_len + bytes.len()].copy_from_slice(bytes);
                        stack_len += bytes.len();
                    } else {
                        let mut ob = String::with_capacity(stack_len + bytes.len() + 64);
                        ob.push_str(unsafe {
                            std::str::from_utf8_unchecked(&stack_buf[..stack_len])
                        });
                        ob.push_str(s);
                        overflow = Some(ob);
                    }
                });
                if ok.is_none() {
                    str_build_slow_fallback(out, slice);
                    return;
                }
            }
        }
    }

    if let Some(ob) = overflow {
        write_out(out, alloc_rope_from_str(&ob, all_ascii));
    } else {
        // SAFETY: every appended chunk was valid UTF-8.
        let s = unsafe { std::str::from_utf8_unchecked(&stack_buf[..stack_len]) };
        write_out(out, alloc_rope_from_str(s, all_ascii));
    }
}

#[inline]
fn map_get_str_inner(
    out: *mut ValueV2,
    obj: *const ValueV2,
    key_ptr: *const u8,
    key_len: usize,
    hash_opt: Option<u64>,
) -> i8 {
    let ov = read_v2(obj);
    // SAFETY: `key_ptr` points to a compiler-emitted static valid UTF-8 string
    // literal of `key_len` bytes (non-null when `key_len > 0`).
    let key = if key_len == 0 {
        ""
    } else {
        unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(key_ptr, key_len)) }
    };
    if let Some(Value::Map(m)) = deref_heap(ov) {
        let h = hash_opt.unwrap_or_else(|| collections::fx_hash_str(key));
        let guard = crate::value::lock_ref(m);
        return match guard.find_str_with_hash(key, h) {
            Some(i) => {
                write_out(out, value_ref_to_v2(&guard.entries[i].1));
                0
            }
            None => {
                rt_throw_v2(&format!("missing key {}", key));
                1
            }
        };
    }
    let idx_val = Value::Str(crate::value::ThinArcStr::from(key));
    match collections::index_get(&v2_to_value(ov), &idx_val) {
        Ok(v) => {
            write_out(out, from_value_v2(v));
            0
        }
        Err(msg) => {
            rt_throw_v2(&msg);
            1
        }
    }
}

#[no_mangle]
pub extern "C" fn spid3r_map_get_str(
    out: *mut ValueV2,
    obj: *const ValueV2,
    key_ptr: *const u8,
    key_len: usize,
) {
    let _ = map_get_str_inner(out, obj, key_ptr, key_len, None);
}

#[no_mangle]
pub extern "C" fn spid3r_map_get_str_h(
    out: *mut ValueV2,
    obj: *const ValueV2,
    key_ptr: *const u8,
    key_len: usize,
    hash: u64,
) -> i8 {
    map_get_str_inner(out, obj, key_ptr, key_len, Some(hash))
}

#[no_mangle]
pub extern "C" fn spid3r_index_set(obj: *const ValueV2, idx: *const ValueV2, v: *const ValueV2) {
    let ov = read_v2(obj);
    let iv = read_v2(idx);
    let val = to_owned_v2(v);
    let res = if let Some(object) = deref_heap(ov) {
        collections::index_set(object, &v2_to_value(iv), val)
    } else {
        collections::index_set(&v2_to_value(ov), &v2_to_value(iv), val)
    };
    if let Err(msg) = res {
        rt_throw_v2(&msg);
    }
}

#[no_mangle]
pub extern "C" fn spid3r_field_get(out: *mut ValueV2, obj: *const ValueV2, name: *const c_char) {
    let ov = read_v2(obj);
    let name = cstr_ref(name);
    let v = if let Some(object) = deref_heap(ov) {
        match object {
            Value::CInstance(inst) => {
                let inst = inst.lock().unwrap();
                match inst.fields.get(name.as_ref()) {
                    Some(v) => value_ref_to_v2(v),
                    None => rt_throw_v2(&format!(
                        "instance of {} has no field '{}'",
                        inst.class.name, name
                    )),
                }
            }
            Value::VInstance(inst) => {
                let inst = inst.lock().unwrap();
                match inst
                    .class
                    .slot_index
                    .get(name.as_ref())
                    .copied()
                    .and_then(|i| inst.slots.get(i))
                {
                    Some(v) => value_ref_to_v2(v),
                    None => rt_throw_v2(&format!(
                        "instance of {} has no field '{}'",
                        inst.class.name, name
                    )),
                }
            }
            Value::Map(m) => {
                let guard = crate::value::lock_ref(m);
                match guard.find_str(name.as_ref()) {
                    Some(i) => value_ref_to_v2(&guard.entries[i].1),
                    None => rt_throw_v2(&format!("missing key '{}'", name)),
                }
            }
            other => rt_throw_v2(&crate::value::no_field_error(other, name.as_ref())),
        }
    } else {
        let owned = v2_to_value(ov);
        rt_throw_v2(&crate::value::no_field_error(&owned, name.as_ref()))
    };
    write_out(out, v);
}

#[no_mangle]
pub extern "C" fn spid3r_field_set(obj: *const ValueV2, name: *const c_char, v: *const ValueV2) {
    let ov = read_v2(obj);
    let name = cstr_ref(name);
    let val = to_owned_v2(v);
    if let Some(Value::CInstance(inst)) = deref_heap(ov) {
        let mut inst = inst.lock().unwrap();
        if let Some(slot) = inst.fields.get_mut(name.as_ref()) {
            *slot = val;
        } else {
            rt_throw_v2(&format!(
                "instance of {} has no field '{}'",
                inst.class.name, name
            ));
        }
        return;
    }
    let owned = v2_to_value(ov);
    rt_throw_v2(&crate::value::no_field_error(&owned, name.as_ref()));
}

#[no_mangle]
pub extern "C" fn spid3r_field_get_soft(
    out: *mut ValueV2,
    obj: *const ValueV2,
    name: *const c_char,
) {
    let ov = read_v2(obj);
    if ov.tag == ValueV2::TAG_NULL {
        write_out(out, ValueV2::NULL);
        return;
    }
    if let Some(Value::Map(m)) = deref_heap(ov) {
        let name_s = cstr_ref(name);
        let guard = crate::value::lock_ref(m);
        let v = match guard.find_str(name_s.as_ref()) {
            Some(i) => value_ref_to_v2(&guard.entries[i].1),
            None => ValueV2::NULL,
        };
        write_out(out, v);
        return;
    }
    spid3r_field_get(out, obj, name);
}

#[no_mangle]
pub extern "C" fn spid3r_len(v: *const ValueV2) -> i64 {
    let vv = read_v2(v);
    if let Some(n) = with_str_v2(vv, |s| {
        if s.is_ascii() {
            s.len()
        } else {
            s.chars().count()
        }
    }) {
        return n as i64;
    }
    let res = if let Some(val) = deref_heap(vv) {
        collections::len_of(val)
    } else {
        collections::len_of(&v2_to_value(vv))
    };
    match res {
        Ok(n) => n as i64,
        Err(msg) => {
            rt_throw_v2(&msg);
            0
        }
    }
}

struct ForInState {
    items: Vec<Value>,
    next: usize,
}

#[no_mangle]
pub extern "C" fn spid3r_forin_start(obj: *const ValueV2) -> *mut c_void {
    let ov = read_v2(obj);
    let res = if let Some(val) = deref_heap(ov) {
        collections::iter_values(val)
    } else {
        collections::iter_values(&v2_to_value(ov))
    };
    match res {
        Ok(items) => Box::into_raw(Box::new(ForInState { items, next: 0 })) as *mut c_void,
        Err(msg) => {
            rt_throw_v2(&msg);
            std::ptr::null_mut()
        }
    }
}

/// Write the next element into `out` and return `1`, or return `0` when exhausted.
#[no_mangle]
pub extern "C" fn spid3r_forin_next(out: *mut ValueV2, iter: *mut c_void) -> u8 {
    if iter.is_null() {
        return 0;
    }
    let st = unsafe { &mut *(iter as *mut ForInState) };
    if st.next >= st.items.len() {
        return 0;
    }
    let v = st.items[st.next].clone();
    st.next += 1;
    write_out(out, from_value_v2(v));
    1
}

#[no_mangle]
pub extern "C" fn spid3r_forin_end(iter: *mut c_void) {
    if !iter.is_null() {
        unsafe { drop(Box::from_raw(iter as *mut ForInState)) };
    }
}

/// A heap slot holding a `Mutex<Value>`; captured variables live in one of
/// these so closures share the exact same cell safely across threads without
/// holding raw `AOT_HEAP` pointers (DP-1 / RT-2).
pub struct UpvalueSlot {
    pub value: Mutex<Value>,
}

#[no_mangle]
pub extern "C" fn spid3r_cell_new(initial: *const ValueV2) -> *mut UpvalueSlot {
    let v = if initial.is_null() {
        Value::Null
    } else {
        to_owned_v2(initial)
    };
    Box::into_raw(Box::new(UpvalueSlot {
        value: Mutex::new(v),
    }))
}

#[no_mangle]
pub extern "C" fn spid3r_cell_get(out: *mut ValueV2, cell: *mut UpvalueSlot) {
    if cell.is_null() {
        abort("native runtime: null upvalue cell");
    }
    // SAFETY: `cell` was created by `spid3r_cell_new` and lives for process lifetime.
    let v = unsafe { (*cell).value.lock().unwrap().clone() };
    write_out(out, from_value_v2(v));
}

#[no_mangle]
pub extern "C" fn spid3r_cell_set(cell: *mut UpvalueSlot, v: *const ValueV2) {
    if cell.is_null() {
        abort("native runtime: null upvalue cell");
    }
    let val = to_owned_v2(v);
    // SAFETY: `cell` was created by `spid3r_cell_new` and lives for process lifetime.
    unsafe {
        *(*cell).value.lock().unwrap() = val;
    }
}

/// Allocate the captured-variable array of a closure environment.
///
/// Layout (private to this module): slot 0 holds the element count as a raw
/// integer, followed by `count` cell pointers. The header lets `spid3r_env_slot`
/// bounds-check an index instead of trusting generated code (audit A-12).
#[no_mangle]
pub extern "C" fn spid3r_env_alloc(count: usize) -> *mut c_void {
    let mut boxed = vec![std::ptr::null_mut::<UpvalueSlot>(); count + 1].into_boxed_slice();
    boxed[0] = count as *mut UpvalueSlot;
    Box::into_raw(boxed) as *mut c_void
}

/// Number of slots in an environment array (the header written by
/// `spid3r_env_alloc`).
unsafe fn env_len(env: *mut c_void) -> usize {
    let arr = env as *const *mut UpvalueSlot;
    *arr as usize
}

#[no_mangle]
pub extern "C" fn spid3r_env_slot(env: *mut c_void, index: usize) -> *mut UpvalueSlot {
    if env.is_null() {
        abort("native runtime: null closure environment");
    }
    unsafe {
        if index >= env_len(env) {
            abort("native runtime: closure environment index out of range");
        }
        let arr = env as *mut *mut UpvalueSlot;
        *arr.add(index + 1)
    }
}

/// Write a cell pointer into an environment slot (closure capture).
#[no_mangle]
pub extern "C" fn spid3r_env_set_slot(env: *mut c_void, index: usize, cell: *mut UpvalueSlot) {
    if env.is_null() {
        abort("native runtime: null closure environment");
    }
    if cell.is_null() {
        abort("native runtime: captured a null upvalue cell");
    }
    unsafe {
        if index >= env_len(env) {
            abort("native runtime: closure environment index out of range");
        }
        let arr = env as *mut *mut UpvalueSlot;
        *arr.add(index + 1) = cell;
    }
}

// ----------------------------------------------------------------------------
// extern "C" trampolines — class registration (called by generated startup)
// ----------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn spid3r_class_register(
    out: *mut ValueV2,
    name: *const c_char,
    parent_name: *const c_char,
) {
    let name = cstr(name);
    let parent = if parent_name.is_null() {
        None
    } else {
        let pname = cstr(parent_name);
        let g = globals().lock().unwrap();
        match g.get(&pname) {
            Some(Value::CClass(c)) => Some(c.clone()),
            _ => abort(&format!("unknown class '{}'", pname)),
        }
    };
    let class = std::sync::Arc::new(crate::value::CompiledClass {
        name,
        parent,
        fields: Vec::new(),
        methods: HashMap::new(),
        ctor: None,
        class_id: 0,
        vtable: Vec::new(),
        vtable_slots: HashMap::new(),
    });
    // Classes live for process lifetime; allocate pinned so they are never swept.
    write_out(out, alloc_pinned_v2(Value::CClass(class)));
}

static NEXT_CLASS_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn class_tables() -> &'static Mutex<HashMap<u64, std::sync::Arc<crate::value::CompiledClass>>> {
    static TABLES: std::sync::OnceLock<
        Mutex<HashMap<u64, std::sync::Arc<crate::value::CompiledClass>>>,
    > = std::sync::OnceLock::new();
    TABLES.get_or_init(|| Mutex::new(HashMap::new()))
}

#[no_mangle]
pub extern "C" fn spid3r_class_publish(class: *const ValueV2) -> u64 {
    let id = NEXT_CLASS_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    {
        let c = class_mut(class);
        let mut vtable: Vec<std::sync::Arc<CompiledFn>> = Vec::new();
        let mut slots: HashMap<String, usize> = HashMap::new();
        let mut chain: Vec<&crate::value::CompiledClass> = Vec::new();
        let mut cur = c.parent.as_deref();
        while let Some(p) = cur {
            chain.push(p);
            cur = p.parent.as_deref();
        }
        for anc in chain.into_iter().rev() {
            let mut names: Vec<&String> = anc.methods.keys().collect();
            names.sort();
            for name in names {
                let cfn = anc.methods[name].1.clone();
                if let Some(&slot) = slots.get(name) {
                    vtable[slot] = cfn;
                } else {
                    let slot = vtable.len();
                    slots.insert(name.clone(), slot);
                    vtable.push(cfn);
                }
            }
        }
        let mut own_names: Vec<String> = c.methods.keys().cloned().collect();
        own_names.sort();
        for name in own_names {
            let cfn = c.methods[&name].1.clone();
            if let Some(&slot) = slots.get(&name) {
                vtable[slot] = cfn;
            } else {
                let slot = vtable.len();
                slots.insert(name, slot);
                vtable.push(cfn);
            }
        }
        c.class_id = id;
        c.vtable = vtable;
        c.vtable_slots = slots;
    }
    if let Some(Value::CClass(arc)) = deref_heap(read_v2(class)) {
        class_tables().lock().unwrap().insert(id, arc.clone());
    }
    id
}

#[no_mangle]
pub extern "C" fn spid3r_class_slot_of(class_id: u64, name: *const c_char) -> i64 {
    if class_id == 0 {
        return -1;
    }
    let name = cstr_ref(name);
    let g = class_tables().lock().unwrap();
    g.get(&class_id)
        .and_then(|c| c.vtable_slots.get(name.as_ref()).copied())
        .map(|s| s as i64)
        .unwrap_or(-1)
}

fn class_mut<'a>(class: *const ValueV2) -> &'a mut crate::value::CompiledClass {
    let cv = read_v2(class);
    if !cv.is_heap() {
        abort("native runtime: class registration with a non-class value");
    }
    let class_val: &mut Value = unsafe { &mut *cv.as_heap_ptr() };
    if let Value::CClass(c) = class_val {
        match std::sync::Arc::get_mut(c) {
            Some(c) => c,
            None => abort("native runtime: class registration raced another reference"),
        }
    } else {
        abort("native runtime: class registration with a non-class value")
    }
}

#[no_mangle]
pub extern "C" fn spid3r_class_add_field(class: *const ValueV2, name: *const c_char, vis: u8) {
    let name = cstr(name);
    let vis = if vis == 0 {
        crate::ast::Vis::Private
    } else {
        crate::ast::Vis::Public
    };
    class_mut(class).fields.push((name, vis));
}

#[no_mangle]
pub extern "C" fn spid3r_class_add_method(
    class: *const ValueV2,
    name: *const c_char,
    arity: usize,
    min_arity: usize,
    code: *const c_void,
) {
    let name = cstr(name);
    let code: unsafe extern "C" fn(*mut ValueV2, *mut c_void, usize, *const *const ValueV2) =
        unsafe { std::mem::transmute(code) };
    let f = std::sync::Arc::new(CompiledFn {
        name: Some(name.clone()),
        arity,
        min_arity,
        code,
        env: std::ptr::null_mut(),
    });
    class_mut(class)
        .methods
        .insert(name, (crate::ast::Vis::Public, f));
}

#[no_mangle]
pub extern "C" fn spid3r_class_set_ctor(
    class: *const ValueV2,
    arity: usize,
    min_arity: usize,
    code: *const c_void,
) {
    let code: unsafe extern "C" fn(*mut ValueV2, *mut c_void, usize, *const *const ValueV2) =
        unsafe { std::mem::transmute(code) };
    let c = class_mut(class);
    let f = std::sync::Arc::new(CompiledFn {
        name: Some(format!("{}.new", c.name)),
        arity,
        min_arity,
        code,
        env: std::ptr::null_mut(),
    });
    c.ctor = Some(f);
}

pub fn construct_cinstance(
    class: std::sync::Arc<crate::value::CompiledClass>,
    args: Vec<Value>,
) -> ValueV2 {
    let mut fields: HashMap<String, Value> = HashMap::new();
    let mut chain: Vec<std::sync::Arc<crate::value::CompiledClass>> = Vec::new();
    let mut cur = Some(class.clone());
    while let Some(c) = cur {
        cur = c.parent.clone();
        chain.push(c);
    }
    for c in chain.iter().rev() {
        for (fname, _) in &c.fields {
            fields.insert(fname.clone(), Value::Null);
        }
    }
    let instance = new_ref(CompiledInstanceData {
        class: class.clone(),
        fields,
    });
    let obj = Value::CInstance(instance);
    let obj_v2 = alloc_heap_v2(obj);
    let ctor = {
        let mut cur = Some(class.clone());
        let mut found = None;
        while let Some(c) = cur {
            if let Some(ct) = &c.ctor {
                found = Some(ct.clone());
                break;
            }
            cur = c.parent.clone();
        }
        found
    };
    if let Some(ctor) = ctor {
        if args.len() + 1 < ctor.min_arity || args.len() + 1 > ctor.arity {
            let want = if ctor.min_arity == ctor.arity {
                format!("{} argument(s)", ctor.arity - 1)
            } else {
                format!("{} to {} argument(s)", ctor.min_arity - 1, ctor.arity - 1)
            };
            return rt_throw_v2(&format!(
                "new {} expects {}, got {}",
                class.name,
                want,
                args.len()
            ));
        }
        let mut frame_vals: Vec<ValueV2> = Vec::with_capacity(args.len() + 1);
        frame_vals.push(obj_v2);
        for a in &args {
            frame_vals.push(from_value_v2(a.clone()));
        }
        let call_args: Vec<*const ValueV2> =
            frame_vals.iter().map(|v| v as *const ValueV2).collect();
        if !enter_call() {
            return rt_throw_v2("stack overflow: call depth limit reached");
        }
        let mut out = ValueV2::NULL;
        spid3r_frame_enter(frame_vals.as_ptr(), frame_vals.len());
        unsafe {
            (ctor.code)(&mut out, ctor.env, args.len() + 1, call_args.as_ptr());
        }
        spid3r_frame_exit();
        leave_call();
    }
    obj_v2
}

fn join_display(args: &[Value]) -> String {
    args.iter()
        .map(crate::value::display_value)
        .collect::<Vec<_>>()
        .join(" ")
}

#[no_mangle]
pub extern "C" fn spid3r_print(argc: usize, argv: *const *const ValueV2) {
    let args = unsafe { args_vec(argc, argv) };
    print!("{}", join_display(&args));
}

#[no_mangle]
pub extern "C" fn spid3r_println(argc: usize, argv: *const *const ValueV2) {
    let args = unsafe { args_vec(argc, argv) };
    println!("{}", join_display(&args));
}

#[no_mangle]
pub extern "C" fn spid3r_eprintln(argc: usize, argv: *const *const ValueV2) {
    let args = unsafe { args_vec(argc, argv) };
    eprintln!("{}", join_display(&args));
}

#[no_mangle]
pub extern "C" fn spid3r_rt_start(entry: extern "C" fn(*mut ValueV2)) -> i32 {
    rt_init();
    crate::stdlib::set_script_args(std::env::args().skip(1).collect());
    ffi_guard("rt_start", |_| 70, || run_program(entry))
}

#[no_mangle]
pub extern "C" fn spid3r_rt_start_slim(entry: extern "C" fn(*mut ValueV2)) -> i32 {
    rt_init_core();
    crate::stdlib::set_script_args(std::env::args().skip(1).collect());
    ffi_guard("rt_start_slim", |_| 70, || run_program(entry))
}

#[no_mangle]
pub extern "C" fn spid3r_rt_start_freestanding(entry: extern "C" fn(*mut ValueV2)) -> i32 {
    install_native_defs(crate::stdlib::freestanding_defs(), None);
    ffi_guard("rt_start_freestanding", |_| 70, || run_program(entry))
}

fn run_program(entry: extern "C" fn(*mut ValueV2)) -> i32 {
    let _active = crate::sched::enter_active();
    let mut entry_out = ValueV2::NULL;
    entry(&mut entry_out);
    let main = {
        let g = globals().lock().unwrap();
        g.get("main").cloned()
    };
    let code = match main {
        Some(Value::Cfn(f)) => {
            let argv: [*const ValueV2; 0] = [];
            let mut ret = ValueV2::NULL;
            unsafe { (f.code)(&mut ret, f.env, 0, argv.as_ptr()) };
            if spid3r_threw() != 0 {
                spid3r_uncaught();
            }
            if ret.tag == ValueV2::TAG_INT {
                (ret.payload as i64) as i32
            } else {
                0
            }
        }
        Some(other) => {
            eprintln!(
                "error[semantic]: main must be a function: fn main() {{ ... }} (found {})",
                crate::value::type_name(&other)
            );
            1
        }
        None => {
            eprintln!(
                "error[semantic]: this program has no fn main; add one to run it as a program"
            );
            1
        }
    };
    let _ = crate::sched::join_tasks_at_exit();
    crate::sched::shutdown();
    let _ = std::io::stdout().flush();
    code
}

#[derive(Clone, Copy)]
struct SendCfn {
    code: unsafe extern "C" fn(*mut ValueV2, *mut c_void, usize, *const *const ValueV2),
    env: *mut c_void,
}
unsafe impl Send for SendCfn {}

#[no_mangle]
pub extern "C" fn spid3r_spawn(out: *mut ValueV2, f: *const ValueV2) {
    crate::value::mark_multithreaded();
    let v = match to_owned_v2(f) {
        Value::Cfn(c) => {
            let cfn = SendCfn {
                code: c.code,
                env: c.env,
            };
            let sched = Scheduler::shared();
            let handle = sched.spawn_job(Box::new(move || {
                let cfn = cfn;
                let argv: [*const ValueV2; 0] = [];
                let mut r = ValueV2::NULL;
                unsafe { (cfn.code)(&mut r, cfn.env, 0usize, argv.as_ptr()) };
                if spid3r_threw() != 0 {
                    match take_thrown_value() {
                        None => crate::sched::TaskOutcome {
                            result: Err("task threw".to_string()),
                            thrown: None,
                        },
                        Some(v) => crate::sched::TaskOutcome {
                            thrown: Some(v.clone()),
                            result: Err(crate::value::display_value(&v)),
                        },
                    }
                } else {
                    crate::sched::TaskOutcome {
                        result: Ok(v2_to_value(r)),
                        thrown: None,
                    }
                }
            }));
            alloc_heap_v2(Value::Task(handle))
        }
        other => rt_throw_v2(&format!(
            "spawn expects a function, found {}",
            crate::value::type_name(&other)
        )),
    };
    write_out(out, v);
}

#[no_mangle]
pub extern "C" fn spid3r_await(out: *mut ValueV2, v: *const ValueV2) {
    let res = match to_owned_v2(v) {
        Value::Task(t) => match t.join() {
            Ok(val) => from_value_v2(val),
            Err(msg) => match t.thrown_value() {
                Some(v) => rt_throw_value_v2(v),
                None => rt_throw_v2(&format!("task failed: {}", msg)),
            },
        },
        Value::Channel(c) => from_value_v2(c.recv()),
        other => rt_throw_v2(&format!(
            "await expects a task or a channel, found {}",
            crate::value::type_name(&other)
        )),
    };
    write_out(out, res);
}

#[no_mangle]
pub extern "C" fn spid3r_abort(msg: *const c_char) -> ! {
    let msg = cstr(msg);
    abort(&msg)
}

#[no_mangle]
pub extern "C" fn spid3r_runtime_version() -> *mut c_char {
    match CString::new(crate::VERSION) {
        Ok(c) => c.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn spid3r_struct_box(
    out: *mut ValueV2,
    class_name: *const c_char,
    count: usize,
    field_names: *const *const c_char,
    field_vals: *const *const ValueV2,
) {
    let cname = cstr(class_name);
    let cls_val = {
        let g = globals().lock().unwrap();
        g.get(&cname).cloned()
    };
    let Some(cls_val) = cls_val else {
        write_out(
            out,
            rt_throw_v2(&format!("unknown struct/class '{}'", cname)),
        );
        return;
    };
    let mut pairs = Vec::with_capacity(count);
    for i in 0..count {
        let (fname_ptr, val_ptr) = unsafe { (*field_names.add(i), *field_vals.add(i)) };
        let fname = cstr(fname_ptr);
        let val = to_owned_v2(val_ptr);
        pairs.push((fname, val));
    }
    let res = match cls_val {
        Value::VClass(cls) => {
            let mut slots = vec![Value::Null; cls.slot_names.len()];
            for (fname, val) in pairs {
                if let Some(&idx) = cls.slot_index.get(&fname) {
                    if let Some(slot) = slots.get_mut(idx) {
                        *slot = val;
                    }
                }
            }
            alloc_heap_v2(Value::VInstance(new_ref(crate::value::VmInstanceData {
                class: cls,
                slots,
            })))
        }
        Value::CClass(cls) => {
            let mut fields = HashMap::new();
            for (fname, val) in pairs {
                fields.insert(fname, val);
            }
            alloc_heap_v2(Value::CInstance(new_ref(CompiledInstanceData {
                class: cls,
                fields,
            })))
        }
        other => rt_throw_v2(&format!(
            "'{}' is not a class/struct, found {}",
            cname,
            crate::value::type_name(&other)
        )),
    };
    write_out(out, res);
}

#[cfg_attr(not(any(windows, unix)), allow(unused_variables))]
#[no_mangle]
pub extern "C" fn spid3r_dl_open(out: *mut ValueV2, path: *const ValueV2) {
    ffi_v2(out, "dl_open", || {
        let pv = read_v2(path);
        let s = match with_str_v2(pv, |s| s.to_string()) {
            Some(s) => s,
            None => {
                let owned = v2_to_value(pv);
                return rt_throw_v2(&format!(
                    "dl_open expects a string path, found {}",
                    crate::value::type_name(&owned)
                ));
            }
        };
        let Ok(cs) = CString::new(s.as_str()) else {
            return rt_throw_v2("dl_open: path contains interior NUL byte");
        };
        #[cfg(windows)]
        let handle = {
            extern "system" {
                fn LoadLibraryA(lp_lib_file_name: *const c_char) -> *mut c_void;
            }
            unsafe { LoadLibraryA(cs.as_ptr()) }
        };
        #[cfg(unix)]
        let handle = {
            extern "C" {
                fn dlopen(filename: *const c_char, flag: i32) -> *mut c_void;
            }
            const RTLD_LAZY: i32 = 1;
            unsafe { dlopen(cs.as_ptr(), RTLD_LAZY) }
        };
        #[cfg(not(any(windows, unix)))]
        let handle: *mut c_void = std::ptr::null_mut();

        if handle.is_null() {
            return rt_throw_v2(&format!("dl_open: failed to load '{}'", s));
        }
        ValueV2::from_int(handle as usize as i64)
    });
}

#[cfg_attr(not(any(windows, unix)), allow(unused_variables))]
#[no_mangle]
pub extern "C" fn spid3r_dl_sym(
    out: *mut ValueV2,
    handle_val: *const ValueV2,
    sym: *const ValueV2,
) {
    ffi_v2(out, "dl_sym", || {
        let hv = read_v2(handle_val);
        let h = if hv.tag == ValueV2::TAG_INT && hv.payload != 0 {
            hv.payload as usize as *mut c_void
        } else {
            return rt_throw_v2("dl_sym: expected a valid library handle");
        };
        let sv = read_v2(sym);
        let s = match with_str_v2(sv, |s| s.to_string()) {
            Some(s) => s,
            None => {
                let owned = v2_to_value(sv);
                return rt_throw_v2(&format!(
                    "dl_sym expects a string symbol name, found {}",
                    crate::value::type_name(&owned)
                ));
            }
        };
        let Ok(cs) = CString::new(s.as_str()) else {
            return rt_throw_v2("dl_sym: symbol name contains interior NUL byte");
        };
        #[cfg(windows)]
        let addr = {
            extern "system" {
                fn GetProcAddress(h_module: *mut c_void, lp_proc_name: *const u8) -> *mut c_void;
            }
            unsafe { GetProcAddress(h, cs.as_ptr() as *const u8) }
        };
        #[cfg(unix)]
        let addr = {
            extern "C" {
                fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
            }
            unsafe { dlsym(h, cs.as_ptr()) }
        };
        #[cfg(not(any(windows, unix)))]
        let addr: *mut c_void = std::ptr::null_mut();

        if addr.is_null() {
            return rt_throw_v2(&format!("dl_sym: symbol '{}' not found", s));
        }
        ValueV2::from_int(addr as usize as i64)
    });
}

#[cfg_attr(not(any(windows, unix)), allow(unused_variables))]
#[no_mangle]
pub extern "C" fn spid3r_dl_close(out: *mut ValueV2, handle_val: *const ValueV2) {
    ffi_v2(out, "dl_close", || {
        let hv = read_v2(handle_val);
        let h = if hv.tag == ValueV2::TAG_INT && hv.payload != 0 {
            hv.payload as usize as *mut c_void
        } else {
            return rt_throw_v2("dl_close: expected a valid library handle");
        };
        #[cfg(windows)]
        let ok = {
            extern "system" {
                fn FreeLibrary(h_lib_module: *mut c_void) -> i32;
            }
            unsafe { FreeLibrary(h) != 0 }
        };
        #[cfg(unix)]
        let ok = {
            extern "C" {
                fn dlclose(handle: *mut c_void) -> i32;
            }
            unsafe { dlclose(h) == 0 }
        };
        #[cfg(not(any(windows, unix)))]
        let ok = false;

        ValueV2::from_bool(ok)
    });
}

// ----------------------------------------------------------------------------
// unit tests
// ----------------------------------------------------------------------------

#[inline]
fn resolve_ulist_index(len: usize, idx: i64) -> Result<usize, String> {
    let i = if idx < 0 {
        (len as i128) + (idx as i128)
    } else {
        idx as i128
    };
    if i < 0 || i >= (len as i128) {
        Err(format!("index {} out of bounds for length {}", idx, len))
    } else {
        Ok(i as usize)
    }
}

/// C-ABI header layout shared with AOT inline `UList` indexing: field 0 is
/// the data pointer and field 1 is the current element count.
#[repr(C)]
struct RawUList<T> {
    data: *mut T,
    len: i64,
    vec: Vec<T>,
}

impl<T> RawUList<T> {
    fn new_ptr() -> *mut c_void {
        let mut vec = Vec::<T>::new();
        let data = vec.as_mut_ptr();
        Box::into_raw(Box::new(RawUList { data, len: 0, vec })) as *mut c_void
    }

    fn sync(&mut self) {
        self.data = self.vec.as_mut_ptr();
        self.len = self.vec.len() as i64;
    }
}

/// R10.3: allocate an unboxed `list<int>` backing buffer (`RawUList<i64>`).
#[no_mangle]
pub extern "C" fn spid3r_ulist_i64_new() -> *mut c_void {
    RawUList::<i64>::new_ptr()
}

/// R10.3: push an unboxed `i64` onto a `RawUList<i64>` handle.
#[no_mangle]
pub extern "C" fn spid3r_ulist_i64_push(p: *mut c_void, val: i64) {
    // SAFETY: `p` was allocated by `spid3r_ulist_i64_new` and is only accessed on the owning thread.
    let u = unsafe { &mut *(p as *mut RawUList<i64>) };
    u.vec.push(val);
    u.sync();
}

/// R10.3: read an unboxed `i64` element from a `RawUList<i64>` handle.
#[no_mangle]
pub extern "C" fn spid3r_ulist_i64_get(p: *mut c_void, idx: i64) -> i64 {
    // SAFETY: `p` was allocated by `spid3r_ulist_i64_new`.
    let u = unsafe { &*(p as *const RawUList<i64>) };
    match resolve_ulist_index(u.vec.len(), idx) {
        Ok(i) => u.vec[i],
        Err(msg) => {
            rt_throw_v2(&msg);
            0
        }
    }
}

/// R10.3: write an unboxed `i64` element into a `RawUList<i64>` handle.
#[no_mangle]
pub extern "C" fn spid3r_ulist_i64_set(p: *mut c_void, idx: i64, val: i64) {
    // SAFETY: `p` was allocated by `spid3r_ulist_i64_new`.
    let u = unsafe { &mut *(p as *mut RawUList<i64>) };
    match resolve_ulist_index(u.vec.len(), idx) {
        Ok(i) => u.vec[i] = val,
        Err(msg) => {
            rt_throw_v2(&msg);
        }
    }
}

/// R10.3: length of an unboxed `RawUList<i64>` handle.
#[no_mangle]
pub extern "C" fn spid3r_ulist_i64_len(p: *mut c_void) -> i64 {
    // SAFETY: `p` was allocated by `spid3r_ulist_i64_new`.
    let u = unsafe { &*(p as *const RawUList<i64>) };
    u.vec.len() as i64
}

/// R10.3: pop the last element from an unboxed `RawUList<i64>` handle.
#[no_mangle]
pub extern "C" fn spid3r_ulist_i64_pop(p: *mut c_void) -> i64 {
    // SAFETY: `p` was allocated by `spid3r_ulist_i64_new`.
    let u = unsafe { &mut *(p as *mut RawUList<i64>) };
    let out = u.vec.pop();
    u.sync();
    match out {
        Some(x) => x,
        None => {
            rt_throw_v2("pop from empty list");
            0
        }
    }
}

/// R10.3: allocate an unboxed `list<float>` backing buffer (`RawUList<f64>`).
#[no_mangle]
pub extern "C" fn spid3r_ulist_f64_new() -> *mut c_void {
    RawUList::<f64>::new_ptr()
}

/// R10.3: push an unboxed `f64` onto a `RawUList<f64>` handle.
#[no_mangle]
pub extern "C" fn spid3r_ulist_f64_push(p: *mut c_void, val: f64) {
    // SAFETY: `p` was allocated by `spid3r_ulist_f64_new`.
    let u = unsafe { &mut *(p as *mut RawUList<f64>) };
    u.vec.push(val);
    u.sync();
}

/// R10.3: read an unboxed `f64` element from a `RawUList<f64>` handle.
#[no_mangle]
pub extern "C" fn spid3r_ulist_f64_get(p: *mut c_void, idx: i64) -> f64 {
    // SAFETY: `p` was allocated by `spid3r_ulist_f64_new`.
    let u = unsafe { &*(p as *const RawUList<f64>) };
    match resolve_ulist_index(u.vec.len(), idx) {
        Ok(i) => u.vec[i],
        Err(msg) => {
            rt_throw_v2(&msg);
            0.0
        }
    }
}

/// R10.3: write an unboxed `f64` element into a `RawUList<f64>` handle.
#[no_mangle]
pub extern "C" fn spid3r_ulist_f64_set(p: *mut c_void, idx: i64, val: f64) {
    // SAFETY: `p` was allocated by `spid3r_ulist_f64_new`.
    let u = unsafe { &mut *(p as *mut RawUList<f64>) };
    match resolve_ulist_index(u.vec.len(), idx) {
        Ok(i) => u.vec[i] = val,
        Err(msg) => {
            rt_throw_v2(&msg);
        }
    }
}

/// R10.3: length of an unboxed `RawUList<f64>` handle.
#[no_mangle]
pub extern "C" fn spid3r_ulist_f64_len(p: *mut c_void) -> i64 {
    // SAFETY: `p` was allocated by `spid3r_ulist_f64_new`.
    let u = unsafe { &*(p as *const RawUList<f64>) };
    u.vec.len() as i64
}

/// R10.3: pop the last element from an unboxed `RawUList<f64>` handle.
#[no_mangle]
pub extern "C" fn spid3r_ulist_f64_pop(p: *mut c_void) -> f64 {
    // SAFETY: `p` was allocated by `spid3r_ulist_f64_new`.
    let u = unsafe { &mut *(p as *mut RawUList<f64>) };
    let out = u.vec.pop();
    u.sync();
    match out {
        Some(x) => x,
        None => {
            rt_throw_v2("pop from empty list");
            0.0
        }
    }
}

/// Phase 1: borrowed contiguous slice view into a homogeneous `list<int>` or
/// `list<float>` for loop-hoisted AOT indexing.
#[repr(C)]
pub struct ListView {
    pub ptr: *const c_void,
    pub len: i64,
    pub gen: u64,
}

fn write_list_view<T>(
    obj: *const ValueV2,
    out: *mut ListView,
    ensure: impl FnOnce(&mut collections::VecData) -> Option<(*const T, usize, u64)>,
) -> i8 {
    if out.is_null() || obj.is_null() || crate::value::is_multithreaded() {
        return 0;
    }
    let ov = read_v2(obj);
    if let Some(Value::List(d) | Value::VecDyn(d)) = deref_heap(ov) {
        let mut guard = crate::value::lock_ref(d);
        if let Some((ptr, len, gen)) = ensure(&mut guard) {
            if let Ok(len_i64) = i64::try_from(len) {
                // SAFETY: caller provides a valid stack-allocated `ListView` destination.
                unsafe {
                    out.write(ListView {
                        ptr: ptr as *const c_void,
                        len: len_i64,
                        gen,
                    });
                }
                return 1;
            }
        }
    }
    0
}

/// Phase 1: obtain a contiguous `i64` slice view for a homogeneous integer list.
/// Returns `1` on success and `0` when fallback to generic indexing is required.
#[no_mangle]
pub extern "C" fn spid3r_list_view_i64(obj: *const ValueV2, out: *mut ListView) -> i8 {
    write_list_view(obj, out, collections::VecData::ensure_dense_i64)
}

/// Phase 1: obtain a contiguous `f64` slice view for a homogeneous float list.
/// Returns `1` on success and `0` when fallback to generic indexing is required.
#[no_mangle]
pub extern "C" fn spid3r_list_view_f64(obj: *const ValueV2, out: *mut ListView) -> i8 {
    write_list_view(obj, out, collections::VecData::ensure_dense_f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(v: ValueV2) -> Value {
        v2_to_value(v)
    }

    #[test]
    fn value_makers_roundtrip() {
        let mut out = ValueV2::NULL;
        spid3r_value_int(&mut out, 42);
        assert!(matches!(owned(out), Value::Int(42)));
        spid3r_value_bool(&mut out, 1);
        assert!(matches!(owned(out), Value::Bool(true)));
        spid3r_value_bool(&mut out, 0);
        assert!(matches!(owned(out), Value::Bool(false)));
        spid3r_value_null(&mut out);
        assert!(matches!(owned(out), Value::Null));
        spid3r_value_float(&mut out, 2.5);
        if let Value::Float(f) = owned(out) {
            assert!((f - 2.5).abs() < f64::EPSILON);
        } else {
            panic!("expected a float");
        }
    }

    #[test]
    fn str_is_nul_terminated_safe() {
        let s = CString::new("héllo").unwrap();
        let mut v = ValueV2::NULL;
        spid3r_value_str(&mut v, s.as_ptr() as *const u8, "héllo".len());
        match owned(v) {
            Value::Str(t) => assert_eq!(&*t, "héllo"),
            other => panic!("expected a string, got {:?}", other),
        }
    }

    #[test]
    fn binop_uses_vm_semantics() {
        let a = ValueV2::from_int(7);
        let b = ValueV2::from_int(3);
        let (mut sum, mut quo, mut lt) = (ValueV2::NULL, ValueV2::NULL, ValueV2::NULL);
        spid3r_binop(&mut sum, &a, &b, 0); // Add
        spid3r_binop(&mut quo, &a, &b, 3); // Div
        spid3r_binop(&mut lt, &a, &b, 7); // Lt
        assert!(matches!(owned(sum), Value::Int(10)));
        assert!(matches!(owned(quo), Value::Int(2))); // int division like the VM
        assert!(matches!(owned(lt), Value::Bool(false))); // 7 < 3 is false
    }

    #[test]
    fn binop_string_concat() {
        let (mut a, mut b, mut r) = (ValueV2::NULL, ValueV2::NULL, ValueV2::NULL);
        spid3r_value_str(&mut a, c"ab".as_ptr() as *const u8, 2);
        spid3r_value_str(&mut b, c"cd".as_ptr() as *const u8, 2);
        spid3r_binop(&mut r, &a, &b, 0);
        match owned(r) {
            Value::Str(s) => assert_eq!(&*s, "abcd"),
            other => panic!("expected a string, got {:?}", other),
        }
    }

    #[test]
    fn truthy_matches_vm_rules() {
        let z = ValueV2::from_int(0);
        let one = ValueV2::from_int(1);
        let n = ValueV2::NULL;
        let t = ValueV2::from_bool(true);
        assert_eq!(spid3r_truthy(&z), 0);
        assert_eq!(spid3r_truthy(&one), 1);
        assert_eq!(spid3r_truthy(&n), 0);
        assert_eq!(spid3r_truthy(&t), 1);
    }

    #[test]
    fn globals_set_get_def_exists() {
        rt_init();
        let name = CString::new("__rt_test_g").unwrap();
        let v = ValueV2::from_int(9);
        spid3r_global_set(name.as_ptr(), &v);
        let mut got = ValueV2::NULL;
        spid3r_global_get(&mut got, name.as_ptr());
        assert!(matches!(owned(got), Value::Int(9)));
        assert_ne!(spid3r_global_exists(name.as_ptr()), 0);
        let other = CString::new("__rt_test_absent").unwrap();
        assert_eq!(spid3r_global_exists(other.as_ptr()), 0);
    }

    #[test]
    fn forin_iterates_a_list() {
        let v1 = ValueV2::from_int(1);
        let v2 = ValueV2::from_int(2);
        let v3 = ValueV2::from_int(3);
        let items: [*const ValueV2; 3] = [&v1, &v2, &v3];
        let mut list = ValueV2::NULL;
        spid3r_construct(
            &mut list,
            CString::new("list").unwrap().as_ptr(),
            3,
            items.as_ptr(),
        );
        let it = spid3r_forin_start(&list);
        let mut seen = Vec::new();
        loop {
            let mut next = ValueV2::NULL;
            if spid3r_forin_next(&mut next, it) == 0 {
                break;
            }
            match owned(next) {
                Value::Int(i) => seen.push(i),
                other => panic!("unexpected element {:?}", other),
            }
        }
        spid3r_forin_end(it);
        assert_eq!(seen, vec![1, 2, 3]);
    }

    #[test]
    fn cells_share_mutation() {
        let v1 = ValueV2::from_int(1);
        let v2 = ValueV2::from_int(2);
        let cell = spid3r_cell_new(&v1);
        spid3r_cell_set(cell, &v2);
        let mut out = ValueV2::NULL;
        spid3r_cell_get(&mut out, cell);
        assert!(matches!(owned(out), Value::Int(2)));
    }

    #[test]
    fn method_dispatch_on_list_len() {
        let name = CString::new("len").unwrap();
        let v1 = ValueV2::from_int(1);
        let v2 = ValueV2::from_int(2);
        let items: [*const ValueV2; 2] = [&v1, &v2];
        let mut list = ValueV2::NULL;
        spid3r_construct(
            &mut list,
            CString::new("list").unwrap().as_ptr(),
            2,
            items.as_ptr(),
        );
        let mut r = ValueV2::NULL;
        spid3r_method(&mut r, &list, name.as_ptr(), 0, std::ptr::null());
        assert!(matches!(owned(r), Value::Int(2)));
    }

    #[test]
    fn class_registration_and_construction() {
        unsafe {
            rt_init();
            let mut cls = ValueV2::NULL;
            spid3r_class_register(
                &mut cls,
                CString::new("__RtPoint").unwrap().as_ptr(),
                std::ptr::null(),
            );
            spid3r_class_add_field(&cls, CString::new("x").unwrap().as_ptr(), 1);
            set_probe_ctor(&cls);
            spid3r_global_set(CString::new("__RtPoint").unwrap().as_ptr(), &cls);

            let arg = ValueV2::from_int(77);
            let inst = construct_cinstance(
                match &owned(cls) {
                    Value::CClass(c) => c.clone(),
                    other => panic!("expected a class, got {:?}", other),
                },
                vec![owned(arg)],
            );
            let mut x = ValueV2::NULL;
            spid3r_field_get(&mut x, &inst, CString::new("x").unwrap().as_ptr());
            assert!(matches!(owned(x), Value::Int(77)));
        }
    }

    #[test]
    fn gc_safepoint_sweeps_unreachable_and_preserves_roots() {
        let mut frame = [ValueV2::NULL; 2];
        spid3r_frame_enter(frame.as_ptr(), frame.len());
        // Root one string in frame[0], then allocate 5000 unrooted strings.
        frame[0] = alloc_heap_v2(Value::Str("kept".into()));
        for i in 0..5000 {
            let _ = alloc_heap_v2(Value::Str(format!("tmp-{}", i).into()));
        }
        spid3r_gc_safepoint();
        let remaining = AOT_HEAP.with(|h| h.borrow().live.len());
        assert_eq!(remaining, 1, "only the rooted heap object should survive");
        match owned(frame[0]) {
            Value::Str(s) => assert_eq!(&*s, "kept"),
            other => panic!("expected kept string, got {:?}", other),
        }
        spid3r_frame_exit();
    }

    #[test]
    fn phase2_str_trampolines_ring_and_prehashed_map_get() {
        let mut frame = [ValueV2::NULL; 16];
        spid3r_frame_enter(frame.as_ptr(), frame.len());

        // Short i64 hits inline SSO; big i64 hits TAG_ROPE_ASCII.
        spid3r_str_from_i64(&mut frame[0], 42);
        assert!(frame[0].is_sso());
        assert_eq!(crate::value::display_value(&owned(frame[0])), "42");

        spid3r_str_from_i64(&mut frame[1], i64::MIN);
        assert_eq!(frame[1].tag, ValueV2::TAG_ROPE_ASCII);
        assert_eq!(
            crate::value::display_value(&owned(frame[1])),
            i64::MIN.to_string()
        );

        let lit = b"user:";
        spid3r_str_lit_i64(&mut frame[2], lit.as_ptr(), lit.len(), 123);
        assert!(frame[2].is_sso());
        assert_eq!(crate::value::display_value(&owned(frame[2])), "user:123");

        // > 8 live ropes in ROOT_FRAMES archive evicted epochs cleanly to history.
        let long_lit = b"prefix_longer_than_fifteen:";
        for (i, slot) in frame[3..15].iter_mut().enumerate() {
            spid3r_str_lit_i64(slot, long_lit.as_ptr(), long_lit.len(), i as i64);
            assert_eq!(slot.tag, ValueV2::TAG_ROPE_ASCII);
        }
        for (i, slot) in frame[3..15].iter().enumerate() {
            assert_eq!(
                crate::value::display_value(&owned(*slot)),
                format!("prefix_longer_than_fifteen:{}", i)
            );
        }

        // spid3r_str_build with non-ASCII keeps TAG_ROPE (not TAG_ROPE_ASCII) and preserves UTF-8.
        let p0 = "café:".as_bytes();
        let p1 = b"|suffix_exceeding_sso";
        let parts = [
            StrPart {
                kind: 0,
                a: p0.as_ptr() as usize as u64,
                b: p0.len() as u64,
            },
            StrPart {
                kind: 1,
                a: 77,
                b: 0,
            },
            StrPart {
                kind: 0,
                a: p1.as_ptr() as usize as u64,
                b: p1.len() as u64,
            },
        ];
        spid3r_str_build(&mut frame[15], parts.as_ptr(), parts.len());
        assert_eq!(frame[15].tag, ValueV2::TAG_ROPE);
        assert_eq!(
            crate::value::display_value(&owned(frame[15])),
            "café:77|suffix_exceeding_sso"
        );

        spid3r_frame_exit();

        // Pre-hashed map lookup agrees with direct key lookup.
        let mut mk = ValueV2::NULL;
        let mut mv = ValueV2::NULL;
        spid3r_value_str(&mut mk, b"price".as_ptr(), 5);
        spid3r_value_int(&mut mv, 999);
        let pairs: [*const ValueV2; 2] = [&mk, &mv];
        let mut m = ValueV2::NULL;
        spid3r_construct(
            &mut m,
            CString::new("map").unwrap().as_ptr(),
            2,
            pairs.as_ptr(),
        );
        let mut got = ValueV2::NULL;
        let h = crate::value::collections::fx_hash_str("price");
        assert_eq!(
            spid3r_map_get_str_h(&mut got, &m, b"price".as_ptr(), 5, h),
            0
        );
        assert!(matches!(owned(got), Value::Int(999)));
    }

    unsafe fn set_probe_ctor(cls: *const ValueV2) {
        extern "C" fn ctor(
            out: *mut ValueV2,
            _env: *mut c_void,
            argc: usize,
            argv: *const *const ValueV2,
        ) {
            assert_eq!(argc, 2);
            unsafe {
                let recv = *argv;
                let arg = *argv.add(1);
                let field = CString::new("x").unwrap();
                spid3r_field_set(recv, field.as_ptr(), arg);
                spid3r_value_null(out);
            }
        }
        let code: extern "C" fn(*mut ValueV2, *mut c_void, usize, *const *const ValueV2) = ctor;
        spid3r_class_set_ctor(cls, 2, 2, code as *const c_void);
    }
}
