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
use crate::value::{new_ref, CompiledFn, CompiledInstanceData, NativeCtx, NativeFn, Value};
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

/// Dereference a value handle. Panics cleanly (via `abort`) on null.
fn deref<'a>(p: *const Value) -> &'a Value {
    unsafe {
        if p.is_null() {
            abort("native runtime: null value pointer");
        }
        &*p
    }
}

fn to_owned(p: *const Value) -> Value {
    deref(p).clone()
}

/// Box a value and hand out the raw pointer (the box lives for the process).
fn leak(v: Value) -> *mut Value {
    Box::into_raw(Box::new(v))
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
    static THROWN: RefCell<Option<*mut Value>> = const { RefCell::new(None) };
}

/// Raise a thrown value (inline `throw` / rethrow): record it in `THROWN`. The
/// surrounding generated code's post-operation `spid3r_threw` guard then routes
/// control to the enclosing handler.
#[no_mangle]
pub extern "C" fn spid3r_throw(v: *mut Value) {
    THROWN.with(|t| *t.borrow_mut() = Some(v));
}

/// Cancellation check for generated code: returns 1 and raises "task
/// cancelled" when the running task was cancelled, 0 otherwise. Loop bodies
/// call it periodically, so a cancelled native task stops at the next poll
/// instead of running to completion.
#[no_mangle]
pub extern "C" fn spid3r_cancel_check() -> u8 {
    if crate::sched::current_task_cancelled() {
        let v = leak(Value::Str("task cancelled".into()));
        spid3r_throw(v);
        1
    } else {
        0
    }
}

/// Non-zero when a `throw` is pending. Generated code calls this after every
/// operation that may throw and branches to its handler when it returns non-zero.
#[no_mangle]
pub extern "C" fn spid3r_threw() -> u8 {
    THROWN.with(|t| t.borrow().is_some()) as u8
}

/// Take the pending thrown value (clearing it). Returns null when nothing is
/// pending. A `try` handler calls this to obtain the exception it caught.
#[no_mangle]
pub extern "C" fn spid3r_take_thrown() -> *mut Value {
    THROWN
        .with(|t| t.borrow_mut().take())
        .unwrap_or(std::ptr::null_mut())
}

/// Read the pending thrown value without clearing it (for the uncaught path).
fn peek_thrown() -> Option<*mut Value> {
    THROWN.with(|t| *t.borrow())
}

/// Report an uncaught throw: print the pending exception and exit with the
/// "user-program error" status (1), matching the tree/VM top-level message. The
/// interpreter additionally prints a per-instruction call-stack trace, which an
/// AOT binary cannot reproduce (a documented native diagnostic gap).
#[no_mangle]
pub extern "C" fn spid3r_uncaught() -> ! {
    let _ = std::io::stdout().flush();
    let shown = match peek_thrown() {
        Some(p) if !p.is_null() => crate::value::display_value(deref(p)),
        _ => "unknown".to_string(),
    };
    eprintln!("error[runtime]: uncaught throw (thrown: {})", shown);
    std::process::exit(1);
}

/// Record a runtime error message as a thrown value and return the placeholder
/// handle an operation returns on failure. Callers run `spid3r_threw` guards and
/// discard the placeholder. Turns operation errors such as "division by zero"
/// into catchable exceptions (matching the tree/VM, which throw string values).
fn rt_throw(msg: &str) -> *mut Value {
    THROWN.with(|t| *t.borrow_mut() = Some(leak(Value::Str(msg.into()))));
    leak(Value::Null)
}

/// Raise a thrown user value (not just its display string) as the pending
/// exception, so an enclosing `try` rebinds the original value (audit A-9).
fn rt_throw_value(v: Value) -> *mut Value {
    THROWN.with(|t| *t.borrow_mut() = Some(leak(v)));
    leak(Value::Null)
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

/// `ffi_guard` for a call that returns a value handle: a panic becomes a
/// pending throw, so the script fails cleanly instead of aborting.
fn ffi_value(what: &str, f: impl FnOnce() -> *mut Value) -> *mut Value {
    ffi_guard(
        what,
        |detail| rt_throw(&format!("internal error in the native runtime: {}", detail)),
        f,
    )
}

/// Copy a C string from generated code.
fn cstr(p: *const c_char) -> String {
    unsafe {
        if p.is_null() {
            abort("native runtime: null string pointer");
        }
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

// ----------------------------------------------------------------------------
// stdlib installation
// ----------------------------------------------------------------------------

/// Install the native stdlib + `Object` into the global table. Called by
/// `spid3r_rt_start` before any user code runs, so `print`, `len`, `json_parse`
/// etc. resolve exactly like they do in the VM.
pub fn rt_init() {
    let mut g = globals().lock().unwrap();
    if !g.is_empty() {
        return; // already initialized
    }
    for (name, func) in crate::stdlib::native_defs() {
        let native = NativeFn { name, func };
        g.insert(name.to_string(), Value::Native(std::sync::Arc::new(native)));
    }
    let object = std::sync::Arc::new(crate::value::CompiledClass {
        name: "Object".to_string(),
        parent: None,
        fields: Vec::new(),
        methods: HashMap::new(),
        ctor: None,
    });
    g.insert("Object".to_string(), Value::CClass(object));
    // `none` is a builtin global (an `Option` holding nothing), matching the
    // tree/VM engines — so `none.unwrap()` throws "unwrap on none" instead of
    // the generic "null values have no methods".
    g.insert("none".to_string(), Value::Option(None));
}

// ----------------------------------------------------------------------------
// extern "C" trampolines — value construction
// ----------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn spid3r_value_int(v: i64) -> *mut Value {
    leak(Value::Int(v))
}

#[no_mangle]
pub extern "C" fn spid3r_value_float(v: f64) -> *mut Value {
    leak(Value::Float(v))
}

static TRUE_VAL: OnceLock<usize> = OnceLock::new();
static FALSE_VAL: OnceLock<usize> = OnceLock::new();
static NULL_VAL: OnceLock<usize> = OnceLock::new();

#[no_mangle]
pub extern "C" fn spid3r_value_bool(v: u8) -> *mut Value {
    if v != 0 {
        *TRUE_VAL.get_or_init(|| Box::into_raw(Box::new(Value::Bool(true))) as usize) as *mut Value
    } else {
        *FALSE_VAL.get_or_init(|| Box::into_raw(Box::new(Value::Bool(false))) as usize)
            as *mut Value
    }
}

#[no_mangle]
pub extern "C" fn spid3r_value_null() -> *mut Value {
    *NULL_VAL.get_or_init(|| Box::into_raw(Box::new(Value::Null)) as usize) as *mut Value
}

#[no_mangle]
pub extern "C" fn spid3r_i64_add(a: i64, b: i64) -> i64 {
    match a.checked_add(b) {
        Some(v) => v,
        None => {
            let msg = leak(Value::Str(
                format!("integer overflow: {} + {}", a, b).into(),
            ));
            spid3r_throw(msg);
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn spid3r_i64_sub(a: i64, b: i64) -> i64 {
    match a.checked_sub(b) {
        Some(v) => v,
        None => {
            let msg = leak(Value::Str(
                format!("integer overflow: {} - {}", a, b).into(),
            ));
            spid3r_throw(msg);
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn spid3r_i64_mul(a: i64, b: i64) -> i64 {
    match a.checked_mul(b) {
        Some(v) => v,
        None => {
            let msg = leak(Value::Str(
                format!("integer overflow: {} * {}", a, b).into(),
            ));
            spid3r_throw(msg);
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn spid3r_i64_div(a: i64, b: i64) -> i64 {
    if b == 0 {
        let msg = leak(Value::Str("division by zero".into()));
        spid3r_throw(msg);
        return 0;
    }
    match a.checked_div(b) {
        Some(v) => v,
        None => {
            let msg = leak(Value::Str(
                format!("integer overflow: {} / {}", a, b).into(),
            ));
            spid3r_throw(msg);
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn spid3r_i64_mod(a: i64, b: i64) -> i64 {
    if b == 0 {
        let msg = leak(Value::Str("modulo by zero".into()));
        spid3r_throw(msg);
        return 0;
    }
    a.wrapping_rem(b)
}

#[no_mangle]
pub extern "C" fn spid3r_i64_neg(a: i64) -> i64 {
    match a.checked_neg() {
        Some(v) => v,
        None => {
            let msg = leak(Value::Str(format!("integer overflow: -{}", a).into()));
            spid3r_throw(msg);
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn spid3r_to_i64(v: *const Value) -> i64 {
    match deref(v) {
        Value::Int(n) => *n,
        _ => {
            let msg = leak(Value::Str("expected integer".into()));
            spid3r_throw(msg);
            0
        }
    }
}

// ---------------------------------------------------------------- FFI bridge
//
// `extern fn` marshals only these four shapes across the C boundary. Each pair
// is explicit: the AOT side calls the `to_*` form to get a C value and the
// `value_*` form to box a result back into a Spidr value.

#[no_mangle]
pub extern "C" fn spid3r_to_f64(v: *const Value) -> f64 {
    match deref(v) {
        Value::Float(f) => *f,
        // A C function taking `double` accepts an int argument without
        // complaint, and a script should not have to write `1.0` to pass `1`.
        Value::Int(n) => *n as f64,
        _ => {
            let msg = leak(Value::Str("expected number".into()));
            spid3r_throw(msg);
            0.0
        }
    }
}

/// Copy a Spidr string into a NUL-terminated C string.
///
/// The buffer is **owned by the caller** and released with the existing
/// `spid3r_cstr_free` (the same one `spid3r_display` uses) right after the call
/// returns. Copying rather than borrowing is deliberate: `Arc<str>` is not
/// NUL-terminated, and a C function may only keep the pointer for the duration
/// of the call.
#[no_mangle]
pub extern "C" fn spid3r_cstr(v: *const Value) -> *mut c_char {
    let text: &str = match deref(v) {
        Value::Str(s) => s,
        Value::Null => "",
        _ => {
            let msg = leak(Value::Str("expected string".into()));
            spid3r_throw(msg);
            return std::ptr::null_mut();
        }
    };
    match std::ffi::CString::new(text) {
        Ok(c) => c.into_raw(),
        // An interior NUL cannot be represented in C; report it rather than
        // silently truncating the string at the boundary.
        Err(_) => {
            let msg = leak(Value::Str("string contains a NUL byte".into()));
            spid3r_throw(msg);
            std::ptr::null_mut()
        }
    }
}

/// Read a C string returned by an `extern fn` into a Spidr value.
///
/// The buffer is **not** freed: a C function may return a string literal or a
/// static buffer, and freeing it would be a double-free waiting to happen. A
/// null pointer becomes `null`.
#[no_mangle]
pub extern "C" fn spid3r_value_from_cstr(p: *const c_char) -> *mut Value {
    if p.is_null() {
        return spid3r_value_null();
    }
    let text = unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy();
    leak(Value::Str(text.into_owned().into()))
}

/// Build a string from raw bytes (LLVM globals hold the literal bytes).
#[no_mangle]
pub extern "C" fn spid3r_value_str(ptr: *const u8, len: usize) -> *mut Value {
    if ptr.is_null() {
        return leak(Value::Null);
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    let s = String::from_utf8_lossy(bytes).into_owned();
    leak(Value::Str(s.into()))
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
    let argc = args.len();
    let mut argv: Vec<*mut Value> = args.iter().map(|a| leak(a.clone())).collect();
    let r = spid3r_call_inner(leak(f), argc, argv.as_mut_ptr());
    // A `throw` inside the callee leaves the pending-throw flag set; the
    // generated `spid3r_threw` guard after the operator handles it, exactly
    // as it does for a direct call.
    Ok(to_owned(r))
}

/// Call a compiled function with `args` (the receiver first for methods).
/// Arity is range-checked BEFORE entering generated code: the prologue loads
/// `argv[i]` for every declared parameter, so a short call reads past the
/// array (audit S-3). The wording matches the VM's `call_value_sync`.
fn call_cfn_method(cfn: &CompiledFn, method: &str, args: &[Value]) -> Result<Value, String> {
    let argc = args.len();
    if argc < cfn.min_arity || argc > cfn.arity {
        return Err(format!(
            "{} expects {} argument(s), got {}",
            method, cfn.arity, argc
        ));
    }
    let argv: Vec<*mut Value> = args.iter().map(|a| leak(a.clone())).collect();
    if !enter_call() {
        return Err("stack overflow: call depth limit reached".to_string());
    }
    let r = unsafe { (cfn.code)(cfn.env, argc, argv.as_ptr()) };
    leave_call();
    Ok(to_owned(r))
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

#[no_mangle]
pub extern "C" fn spid3r_binop(l: *const Value, r: *const Value, op: i64) -> *mut Value {
    let op = match binop_from_u8(op as u8) {
        Some(o) => o,
        None => abort("native runtime: unknown binop"),
    };
    let l_ref = deref(l);
    let r_ref = deref(r);
    if let (Value::Int(a), Value::Int(b)) = (l_ref, r_ref) {
        let a = *a;
        let b = *b;
        return match op {
            BinOp::Add => match a.checked_add(b) {
                Some(v) => leak(Value::Int(v)),
                None => rt_throw(&format!("integer overflow: {} + {}", a, b)),
            },
            BinOp::Sub => match a.checked_sub(b) {
                Some(v) => leak(Value::Int(v)),
                None => rt_throw(&format!("integer overflow: {} - {}", a, b)),
            },
            BinOp::Mul => match a.checked_mul(b) {
                Some(v) => leak(Value::Int(v)),
                None => rt_throw(&format!("integer overflow: {} * {}", a, b)),
            },
            BinOp::Div => {
                if b == 0 {
                    rt_throw("division by zero")
                } else {
                    match a.checked_div(b) {
                        Some(v) => leak(Value::Int(v)),
                        None => rt_throw(&format!("integer overflow: {} / {}", a, b)),
                    }
                }
            }
            BinOp::Mod => {
                if b == 0 {
                    rt_throw("modulo by zero")
                } else {
                    leak(Value::Int(a.wrapping_rem(b)))
                }
            }
            BinOp::Eq => spid3r_value_bool((a == b) as u8),
            BinOp::Ne => spid3r_value_bool((a != b) as u8),
            BinOp::Lt => spid3r_value_bool((a < b) as u8),
            BinOp::Le => spid3r_value_bool((a <= b) as u8),
            BinOp::Gt => spid3r_value_bool((a > b) as u8),
            BinOp::Ge => spid3r_value_bool((a >= b) as u8),
        };
    }
    if let (Value::Float(a), Value::Float(b)) = (l_ref, r_ref) {
        let a = *a;
        let b = *b;
        return match op {
            BinOp::Add => leak(Value::Float(a + b)),
            BinOp::Sub => leak(Value::Float(a - b)),
            BinOp::Mul => leak(Value::Float(a * b)),
            BinOp::Div => {
                if b == 0.0 {
                    rt_throw("division by zero")
                } else {
                    leak(Value::Float(a / b))
                }
            }
            BinOp::Mod => {
                if b == 0.0 {
                    rt_throw("modulo by zero")
                } else {
                    leak(Value::Float(a % b))
                }
            }
            BinOp::Eq => spid3r_value_bool((a == b) as u8),
            BinOp::Ne => spid3r_value_bool((a != b) as u8),
            BinOp::Lt => spid3r_value_bool((a < b) as u8),
            BinOp::Le => spid3r_value_bool((a <= b) as u8),
            BinOp::Gt => spid3r_value_bool((a > b) as u8),
            BinOp::Ge => spid3r_value_bool((a >= b) as u8),
        };
    }
    let l = to_owned(l);
    let r = to_owned(r);
    // Operator overloading first: a value carrying `__add__`/`__sub__`/
    // `__mul__`/`__div__` (or a reflected form on the right operand) handles
    // the operator. Without this, the stdlib numeric types (complex,
    // rational, decimal, bigint) and user classes with dunders raised a type
    // error under AOT while the same program ran on tree/VM (audit S-21).
    if let Some((fwd, rev)) = overload_dunders(op) {
        let over = native_try_operator(&l, &r, fwd).or_else(|| native_try_operator(&r, &l, rev));
        if let Some(res) = over {
            return match res {
                Ok(v) => leak(v),
                Err(msg) => rt_throw(&msg),
            };
        }
    }
    match vm_eval_binary(op, l, r) {
        Ok(v) => leak(v),
        Err(msg) => rt_throw(&msg),
    }
}

#[no_mangle]
pub extern "C" fn spid3r_unop(v: *const Value, op: i64) -> *mut Value {
    let op = match unop_from_u8(op as u8) {
        Some(o) => o,
        None => abort("native runtime: unknown unop"),
    };
    let v = to_owned(v);
    // Unary overloading (`__neg__`), the same dispatch as the VM's
    // `try_unary_operator`.
    if op == UnOp::Neg {
        if let Some(res) = native_try_unary_operator(&v, "__neg__") {
            return match res {
                Ok(v) => leak(v),
                Err(msg) => rt_throw(&msg),
            };
        }
    }
    match vm_eval_unary(op, v) {
        Ok(v) => leak(v),
        Err(msg) => rt_throw(&msg),
    }
}

#[no_mangle]
pub extern "C" fn spid3r_truthy(v: *const Value) -> u8 {
    crate::value::truthy(deref(v)) as u8
}

#[no_mangle]
pub extern "C" fn spid3r_eq(l: *const Value, r: *const Value) -> u8 {
    crate::value::values_equal(deref(l), deref(r)) as u8
}

/// Display a value the way `print` does. Returns a malloc'd C string that the
/// caller frees with `spid3r_cstr_free`.
#[no_mangle]
pub extern "C" fn spid3r_display(v: *const Value) -> *mut c_char {
    let s = crate::value::display_value(deref(v));
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

#[no_mangle]
pub extern "C" fn spid3r_global_get(name: *const c_char) -> *mut Value {
    let key = cstr(name);
    let g = globals().lock().unwrap();
    match g.get(&key) {
        Some(v) => leak(v.clone()),
        None => leak(Value::Null),
    }
}

#[no_mangle]
pub extern "C" fn spid3r_global_set(name: *const c_char, v: *mut Value) {
    let key = cstr(name);
    let val = to_owned(v);
    globals().lock().unwrap().insert(key, val);
}

/// Define a global if it does not exist yet (used for top-level `let`).
#[no_mangle]
pub extern "C" fn spid3r_global_def(name: *const c_char, v: *mut Value) {
    let key = cstr(name);
    let val = to_owned(v);
    globals().lock().unwrap().entry(key).or_insert(val);
}

#[no_mangle]
pub extern "C" fn spid3r_global_exists(name: *const c_char) -> u8 {
    ffi_guard(
        "global_exists",
        |_| 0,
        || {
            let key = cstr(name);
            globals().lock().unwrap().contains_key(&key) as u8
        },
    )
}

// ----------------------------------------------------------------------------
// extern "C" trampolines — calls
// ----------------------------------------------------------------------------

unsafe fn args_vec(argc: usize, argv: *const *mut Value) -> Vec<Value> {
    let mut v = Vec::with_capacity(argc);
    for i in 0..argc {
        let p = *argv.add(i);
        v.push(to_owned(p));
    }
    v
}

/// Call any callable value: compiled function, native stdlib function, or
/// compiled class (construct an instance and run its constructor).
#[no_mangle]
pub extern "C" fn spid3r_call(f: *mut Value, argc: usize, argv: *const *mut Value) -> *mut Value {
    ffi_value("call", || spid3r_call_inner(f, argc, argv))
}

/// Cancellation point after a call returns (audit S-23). A loop body that
/// blocks — the canonical `while true { sleep_ms(1) }` worker, or a socket
/// read with a timeout — burns wall-clock time the loop's iteration-count
/// poll cannot see: at `CANCEL_POLL_INTERVAL` (2048) iterations, cancelling a
/// task that sleeps 1ms per iteration took ~2s (measured 3.5s under load),
/// and a `sleep_ms(100)` loop would take minutes. Re-checking once the call
/// returns bounds the latency to a single call, matching the tree engine's
/// per-statement check. An error raised by the callee wins: cancellation is
/// sticky, so the next call re-raises it. A no-op outside spawned tasks.
fn cancel_guard(r: *mut Value) -> *mut Value {
    if THROWN.with(|t| t.borrow().is_none()) && crate::sched::current_task_cancelled() {
        return rt_throw("task cancelled");
    }
    r
}

fn spid3r_call_inner(f: *mut Value, argc: usize, argv: *const *mut Value) -> *mut Value {
    cancel_guard(spid3r_call_inner_dispatch(f, argc, argv))
}

fn spid3r_call_inner_dispatch(f: *mut Value, argc: usize, argv: *const *mut Value) -> *mut Value {
    let callee = to_owned(f);
    match callee {
        Value::Cfn(c) => {
            // Arity-check BEFORE entering generated code. The prologue loads
            // `argv[offset + i]` for every declared parameter, so a short
            // call reads past the argv array and the garbage is later
            // dereferenced as a `*mut Value` (observed: SIGSEGV). The
            // semantic checker only validates statically-known callees, so
            // dynamic calls (`fs[0](1)`) reach here unchecked. `min_arity`
            // covers trailing constant defaults, which the prologue fills.
            if argc < c.min_arity || argc > c.arity {
                return rt_throw(&format!(
                    "{} expects {} argument(s), got {}",
                    c.name.as_deref().unwrap_or("<fn>"),
                    c.arity,
                    argc
                ));
            }
            if !enter_call() {
                return rt_throw("stack overflow: call depth limit reached");
            }
            let r = unsafe { (c.code)(c.env, argc, argv) };
            leave_call();
            r
        }
        Value::Native(n) => {
            let args = unsafe { args_vec(argc, argv) };
            let ctx = NativeCtx {
                sched: Scheduler::shared(),
            };
            match (n.func)(&ctx, args) {
                Ok(v) => leak(v),
                Err(msg) => rt_throw(&msg),
            }
        }
        Value::NativeClosure(n) => {
            let args = unsafe { args_vec(argc, argv) };
            let ctx = NativeCtx {
                sched: Scheduler::shared(),
            };
            match (n.func)(&ctx, args) {
                Ok(v) => leak(v),
                Err(msg) => rt_throw(&msg),
            }
        }
        Value::CClass(c) => construct_cinstance(c, unsafe { args_vec(argc, argv) }),
        // Catchable like the tree/VM call dispatch (F-9): calling a
        // non-callable value is a script-level runtime error there.
        other => rt_throw(&format!(
            "{} is not callable",
            crate::value::type_name(&other)
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
    CALL_DEPTH.with(|d| {
        let n = d.get() + 1;
        if n > crate::vm::MAX_CALL_DEPTH {
            return false;
        }
        d.set(n);
        true
    })
}

fn leave_call() {
    CALL_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
}

/// Build a `Value::Cfn` for a compiled function (called from closures).
#[no_mangle]
pub extern "C" fn spid3r_make_fn(
    name: *const c_char,
    arity: usize,
    min_arity: usize,
    code: *const c_void,
    env: *mut c_void,
) -> *mut Value {
    ffi_guard(
        "make_fn",
        |_| std::ptr::null_mut(),
        || make_fn_inner(name, arity, min_arity, code, env),
    )
}

fn make_fn_inner(
    name: *const c_char,
    arity: usize,
    min_arity: usize,
    code: *const c_void,
    env: *mut c_void,
) -> *mut Value {
    let code: unsafe extern "C" fn(*mut c_void, usize, *const *mut Value) -> *mut Value =
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
    leak(Value::Cfn(std::sync::Arc::new(CompiledFn {
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
    name: *const c_char,
    argc: usize,
    argv: *const *mut Value,
) -> *mut Value {
    let name = cstr(name);
    let args = unsafe { args_vec(argc, argv) };
    match collections::construct(&name, args) {
        Ok(v) => leak(v),
        Err(msg) => rt_throw(&msg),
    }
}

/// `obj.method(args)` — class methods for compiled instances, everything else
/// delegates to the shared collections layer (strings, lists, maps, ...).
#[no_mangle]
pub extern "C" fn spid3r_method(
    obj: *mut Value,
    name: *const c_char,
    argc: usize,
    argv: *const *mut Value,
) -> *mut Value {
    ffi_value("method", || {
        cancel_guard(spid3r_method_inner(obj, name, argc, argv))
    })
}

fn spid3r_method_inner(
    obj: *mut Value,
    name: *const c_char,
    argc: usize,
    argv: *const *mut Value,
) -> *mut Value {
    let object = to_owned(obj);
    let name = cstr(name);
    let args = unsafe { args_vec(argc, argv) };
    if let Value::CInstance(inst) = &object {
        let class = inst.lock().unwrap().class.clone();
        if let Some((_, cfn)) = find_method_cclass(&class, &name) {
            // Same wording as the VM/tree runtime dispatch (PAR-04): the
            // message counts the *declared* parameters and the *passed*
            // arguments, neither of which includes `self`.
            if argc + 1 < cfn.min_arity || argc + 1 > cfn.arity {
                let want = if cfn.min_arity == cfn.arity {
                    format!("{} argument(s)", cfn.arity - 1)
                } else {
                    format!("{} to {} argument(s)", cfn.min_arity - 1, cfn.arity - 1)
                };
                return rt_throw(&format!("method {} expects {}, got {}", name, want, argc));
            }
            let env = cfn.env;
            let code = cfn.code;
            let mut call_args: Vec<*mut Value> = Vec::with_capacity(argc + 1);
            call_args.push(leak(object.clone()));
            for a in &args {
                call_args.push(leak(a.clone()));
            }
            if !enter_call() {
                return rt_throw("stack overflow: call depth limit reached");
            }
            let r = unsafe { (code)(env, argc + 1, call_args.as_ptr()) };
            leave_call();
            return r;
        }
        // Catchable like the tree/VM dispatch (`fail`/`Flow::Error`), not an
        // internal abort: a call through an untyped value can reach this with
        // any method name (found by tests/engine_matrix.rs).
        return rt_throw(&format!("class {} has no method '{}'", class.name, name));
    }
    match collections::call_method_host(&mut NativeCallbackHost, &object, &name, args) {
        Ok(v) => leak(v),
        Err(msg) => match crate::sched::take_error_thrown() {
            // `task.join()` (and callback hosts) report a String error but
            // stash the user-thrown value; rethrow the value so an enclosing
            // `try` rebinds it (audit A-9).
            Some(v) => rt_throw_value(v),
            None => rt_throw(&msg),
        },
    }
}

/// `super(args)` / `super.m(args)` inside a compiled method or constructor
/// (PAR-03). The receiver is the current compiled instance; the target class
/// is resolved by name in the globals — the same resolution the tree
/// interpreter performs — and every error message matches tree/vm verbatim.
/// `method_name == NULL` calls the parent constructor on the instance being
/// initialized; otherwise it runs the parent method with the same receiver.
#[no_mangle]
pub extern "C" fn spid3r_super_call(
    recv: *mut Value,
    parent_name: *const c_char,
    method_name: *const c_char,
    argc: usize,
    argv: *const *mut Value,
) -> *mut Value {
    ffi_value("super", || {
        spid3r_super_call_inner(recv, parent_name, method_name, argc, argv)
    })
}

fn spid3r_super_call_inner(
    recv: *mut Value,
    parent_name: *const c_char,
    method_name: *const c_char,
    argc: usize,
    argv: *const *mut Value,
) -> *mut Value {
    let object = to_owned(recv);
    match object {
        Value::CInstance(_) => {}
        _ => {
            return rt_throw("super is only available inside a class method");
        }
    }
    let parent = cstr(parent_name);
    if parent.is_empty() {
        return rt_throw("super is not available: this class has no parent class");
    }
    let class = {
        let g = globals().lock().unwrap();
        match g.get(&parent) {
            Some(Value::CClass(c)) => c.clone(),
            Some(_) => return rt_throw(&format!("'{}' is not a class", parent)),
            None => return rt_throw(&format!("unknown class '{}'", parent)),
        }
    };
    let args = unsafe { args_vec(argc, argv) };
    if method_name.is_null() {
        // Parent constructor on the SAME instance (it already exists — it is
        // being initialized), walking the ancestor chain if the immediate
        // parent inherited its constructor.
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
                return rt_throw(&format!("class '{}' has no constructor", class.name));
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
            return rt_throw(&format!(
                "constructor {}::new expects {}, got {}",
                ctor_class_name, want, argc
            ));
        }
        let mut call_args: Vec<*mut Value> = Vec::with_capacity(argc + 1);
        call_args.push(leak(object.clone()));
        for a in &args {
            call_args.push(leak(a.clone()));
        }
        let env = ctor.env;
        let code = ctor.code;
        if !enter_call() {
            return rt_throw("stack overflow: call depth limit reached");
        }
        unsafe { (code)(env, argc + 1, call_args.as_ptr()) };
        leave_call();
        return leak(Value::Null);
    }
    // Parent method on the same receiver (the tree engine's `call_class_method`
    // against the parent class; `find_method_cclass` walks the same chain).
    let m = cstr(method_name);
    let (_, f) = match find_method_cclass(&class, &m) {
        Some(x) => x,
        None => {
            return rt_throw(&format!("class {} has no method '{}'", class.name, m));
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
        return rt_throw(&format!("method {} expects {}, got {}", m, want, argc));
    }
    let mut call_args: Vec<*mut Value> = Vec::with_capacity(argc + 1);
    call_args.push(leak(object.clone()));
    for a in &args {
        call_args.push(leak(a.clone()));
    }
    let env = f.env;
    let code = f.code;
    if !enter_call() {
        return rt_throw("stack overflow: call depth limit reached");
    }
    let r = unsafe { (code)(env, argc + 1, call_args.as_ptr()) };
    leave_call();
    r
}

/// Lets builtin collections call Spid3r functions back (higher-order methods
/// such as `list.map(f)`) inside natively compiled programs. Compiled closures
/// are plain `extern "C"` functions, so the host calls the machine code
/// directly — the same pattern as `spid3r_method`'s instance dispatch.
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
                let mut argv: Vec<*mut Value> = Vec::with_capacity(args.len());
                for a in &args {
                    argv.push(leak(a.clone()));
                }
                let code = cfn.code;
                let env = cfn.env;
                if !enter_call() {
                    return Err("stack overflow: call depth limit reached".to_string());
                }
                let r = unsafe { (code)(env, args.len(), argv.as_ptr()) };
                leave_call();
                if spid3r_threw() != 0 {
                    let thrown = peek_thrown().unwrap_or(std::ptr::null_mut());
                    let msg = if !thrown.is_null() {
                        crate::value::display_value(deref(thrown))
                    } else {
                        "throw in callback".to_string()
                    };
                    return Err(msg);
                }
                if r.is_null() {
                    return Ok(Value::Null);
                }
                Ok(to_owned(r))
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
                    let thrown = peek_thrown().unwrap_or(std::ptr::null_mut());
                    let msg = if !thrown.is_null() {
                        crate::value::display_value(deref(thrown))
                    } else {
                        "throw in constructor".to_string()
                    };
                    return Err(msg);
                }
                if r.is_null() {
                    return Ok(Value::Null);
                }
                Ok(to_owned(r))
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

#[no_mangle]
pub extern "C" fn spid3r_index_get(obj: *const Value, idx: *const Value) -> *mut Value {
    let object = deref(obj);
    let idx = deref(idx);
    match collections::index_get(object, idx) {
        Ok(v) => leak(v),
        Err(msg) => rt_throw(&msg),
    }
}

#[no_mangle]
pub extern "C" fn spid3r_index_set(obj: *const Value, idx: *const Value, v: *mut Value) {
    let object = deref(obj);
    let idx = deref(idx);
    let val = to_owned(v);
    if let Err(msg) = collections::index_set(object, idx, val) {
        rt_throw(&msg);
    }
}

#[no_mangle]
pub extern "C" fn spid3r_field_get(obj: *const Value, name: *const c_char) -> *mut Value {
    let object = deref(obj);
    let name = cstr(name);
    // Every failure here is a script-level runtime error on the tree/VM
    // engines (interpreter `field_get` raises a catchable `Flow::Error`),
    // so each arm throws through the catchable channel instead of `abort`
    // (F-9: `m?.missing` / `m.missing` on a map without the key used to
    // kill the process with exit 70 even inside `try`/`catch`).
    match object {
        Value::CInstance(inst) => {
            let inst = inst.lock().unwrap();
            match inst.fields.get(&name) {
                Some(v) => leak(v.clone()),
                None => rt_throw(&format!(
                    "instance of {} has no field '{}'",
                    inst.class.name, name
                )),
            }
        }
        Value::VInstance(inst) => {
            let inst = inst.lock().unwrap();
            match inst.fields.get(&name) {
                Some(v) => leak(v.clone()),
                None => rt_throw(&format!(
                    "instance of {} has no field '{}'",
                    inst.class.name, name
                )),
            }
        }
        // `map.field` is key lookup, the same as the interpreter and the VM.
        // Without this arm a program using it ran on tree and VM but aborted
        // here with "cannot read field 'k' of map" (found by
        // tests/engine_matrix.rs).
        Value::Map(m) => {
            let key = crate::value::MapKey::Str(name.clone().into());
            let guard = m.lock().unwrap();
            match guard.find(&key) {
                Some(i) => leak(guard.entries[i].1.clone()),
                None => rt_throw(&format!("missing key '{}'", name)),
            }
        }
        other => rt_throw(&crate::value::no_field_error(other, &name)),
    }
}

#[no_mangle]
pub extern "C" fn spid3r_field_set(obj: *const Value, name: *const c_char, v: *mut Value) {
    let object = deref(obj);
    let name = cstr(name);
    let val = to_owned(v);
    // Catchable like the interpreter's `field_set` (`Flow::Error`), not an
    // abort (F-9).
    match object {
        Value::CInstance(inst) => {
            let mut inst = inst.lock().unwrap();
            if let Some(slot) = inst.fields.get_mut(&name) {
                *slot = val;
            } else {
                rt_throw(&format!(
                    "instance of {} has no field '{}'",
                    inst.class.name, name
                ));
            }
        }
        other => {
            rt_throw(&crate::value::no_field_error(other, &name));
        }
    }
}

/// `obj?.field` — the null-safe dot's read (F-4): like [`spid3r_field_get`],
/// except a null receiver and a missing map key return null instead of
/// throwing. Instance/other failures delegate to `spid3r_field_get`, so a
/// missing instance field stays a real (catchable) error, exactly like the
/// tree interpreter and the VM's `FieldGetSoft`.
#[no_mangle]
pub extern "C" fn spid3r_field_get_soft(obj: *const Value, name: *const c_char) -> *mut Value {
    let object = deref(obj);
    match object {
        Value::Null => leak(Value::Null),
        Value::Map(m) => {
            let name = cstr(name);
            let key = crate::value::MapKey::Str(name.into());
            let guard = m.lock().unwrap();
            match guard.find(&key) {
                Some(i) => leak(guard.entries[i].1.clone()),
                None => leak(Value::Null),
            }
        }
        _ => spid3r_field_get(obj, name),
    }
}

#[no_mangle]
pub extern "C" fn spid3r_len(v: *const Value) -> i64 {
    let v = deref(v);
    match collections::len_of(v) {
        Ok(n) => n as i64,
        Err(msg) => {
            rt_throw(&msg);
            0
        }
    }
}

// ----------------------------------------------------------------------------
// extern "C" trampolines — for-in iteration (snapshot semantics, like the VM)
// ----------------------------------------------------------------------------

struct ForInState {
    items: Vec<Value>,
    next: usize,
}

#[no_mangle]
pub extern "C" fn spid3r_forin_start(obj: *const Value) -> *mut c_void {
    let object = deref(obj);
    match collections::iter_values(object) {
        Ok(items) => Box::into_raw(Box::new(ForInState { items, next: 0 })) as *mut c_void,
        Err(msg) => {
            rt_throw(&msg);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn spid3r_forin_next(iter: *mut c_void) -> *mut Value {
    if iter.is_null() {
        return std::ptr::null_mut();
    }
    let st = unsafe { &mut *(iter as *mut ForInState) };
    if st.next >= st.items.len() {
        return std::ptr::null_mut();
    }
    let v = st.items[st.next].clone();
    st.next += 1;
    leak(v)
}

#[no_mangle]
pub extern "C" fn spid3r_forin_end(iter: *mut c_void) {
    if !iter.is_null() {
        unsafe { drop(Box::from_raw(iter as *mut ForInState)) };
    }
}

// ----------------------------------------------------------------------------
// extern "C" trampolines — closures / upvalue cells
// ----------------------------------------------------------------------------

/// A heap slot holding a `*mut Value`; captured variables live in one of these
/// so closures share the exact same box the defining frame reads/writes.
///
/// The field is atomic (audit A-12): `spawn` puts a closure — and therefore its
/// captured cells — on a worker thread, so two tasks can read and write the same
/// cell concurrently. A plain `*mut Value` made that a data race (UB, torn
/// pointer reads); load/store are `SeqCst` so a reader never observes a
/// half-written pointer. Generated code never touches the field directly (it
/// calls `spid3r_cell_get`/`spid3r_cell_set`), so the layout stays opaque.
#[repr(C)]
pub struct UpvalueSlot {
    pub value: std::sync::atomic::AtomicPtr<Value>,
}

#[no_mangle]
pub extern "C" fn spid3r_cell_new(initial: *mut Value) -> *mut UpvalueSlot {
    let v = if initial.is_null() {
        leak(Value::Null)
    } else {
        initial
    };
    Box::into_raw(Box::new(UpvalueSlot {
        value: std::sync::atomic::AtomicPtr::new(v),
    }))
}

#[no_mangle]
pub extern "C" fn spid3r_cell_get(cell: *mut UpvalueSlot) -> *mut Value {
    if cell.is_null() {
        abort("native runtime: null upvalue cell");
    }
    unsafe { (*cell).value.load(std::sync::atomic::Ordering::SeqCst) }
}

#[no_mangle]
pub extern "C" fn spid3r_cell_set(cell: *mut UpvalueSlot, v: *mut Value) {
    if cell.is_null() {
        abort("native runtime: null upvalue cell");
    }
    unsafe { (*cell).value.store(v, std::sync::atomic::Ordering::SeqCst) };
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
    name: *const c_char,
    parent_name: *const c_char,
) -> *mut Value {
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
    });
    leak(Value::CClass(class))
}

/// Mutable access to a class during registration. The class Value was just
/// created by generated code and is not yet visible anywhere else, so its
/// `Arc` is uniquely owned and we can borrow the payload mutably.
fn class_mut<'a>(class: *const Value) -> &'a mut crate::value::CompiledClass {
    // The class box was just created by generated code and is not yet shared,
    // so a mutable borrow through the raw pointer is sound here.
    let class: &mut Value = unsafe { &mut *(class as *mut Value) };
    if let Value::CClass(c) = class {
        match std::sync::Arc::get_mut(c) {
            Some(c) => c,
            None => abort("native runtime: class registration raced another reference"),
        }
    } else {
        abort("native runtime: class registration with a non-class value")
    }
}

#[no_mangle]
pub extern "C" fn spid3r_class_add_field(class: *const Value, name: *const c_char, vis: u8) {
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
    class: *const Value,
    name: *const c_char,
    arity: usize,
    min_arity: usize,
    code: *const c_void,
) {
    let name = cstr(name);
    let code: unsafe extern "C" fn(*mut c_void, usize, *const *mut Value) -> *mut Value =
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
    class: *const Value,
    arity: usize,
    min_arity: usize,
    code: *const c_void,
) {
    let code: unsafe extern "C" fn(*mut c_void, usize, *const *mut Value) -> *mut Value =
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

/// Construct an instance of a compiled class: initialize fields along the
/// inheritance chain (root first), then run the nearest constructor (if any).
pub fn construct_cinstance(
    class: std::sync::Arc<crate::value::CompiledClass>,
    args: Vec<Value>,
) -> *mut Value {
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
    let obj_ptr = leak(obj.clone());
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
            // Catchable like the tree/VM constructor dispatch (F-9): the same
            // message the interpreter raises, routed through the throw channel.
            return rt_throw(&format!(
                "new {} expects {}, got {}",
                class.name,
                want,
                args.len()
            ));
        }
        let mut call_args: Vec<*mut Value> = Vec::with_capacity(args.len() + 1);
        call_args.push(obj_ptr);
        for a in &args {
            call_args.push(leak(a.clone()));
        }
        if !enter_call() {
            return rt_throw("stack overflow: call depth limit reached");
        }
        unsafe {
            (ctor.code)(ctor.env, args.len() + 1, call_args.as_ptr());
        }
        leave_call();
    }
    obj_ptr
}

// ----------------------------------------------------------------------------
// extern "C" trampolines — printing (replicating the stdlib natives exactly)
// ----------------------------------------------------------------------------

fn join_display(args: &[Value]) -> String {
    args.iter()
        .map(crate::value::display_value)
        .collect::<Vec<_>>()
        .join(" ")
}

#[no_mangle]
pub extern "C" fn spid3r_print(argc: usize, argv: *const *mut Value) {
    let args = unsafe { args_vec(argc, argv) };
    print!("{}", join_display(&args));
}

#[no_mangle]
pub extern "C" fn spid3r_println(argc: usize, argv: *const *mut Value) {
    let args = unsafe { args_vec(argc, argv) };
    println!("{}", join_display(&args));
}

#[no_mangle]
pub extern "C" fn spid3r_eprintln(argc: usize, argv: *const *mut Value) {
    let args = unsafe { args_vec(argc, argv) };
    eprintln!("{}", join_display(&args));
}

// ----------------------------------------------------------------------------
// process startup
// ----------------------------------------------------------------------------

/// Start a natively compiled Spid3r program: install the stdlib, run the
/// generated entry (class registration + top-level statements), then call
/// `main` and use its return value as the process exit code (matching
/// `run --engine vm`).
///
/// `entry` is the generated entry function, passed in by the emitted `main`
/// so the staticlib never references program-specific symbols.
#[no_mangle]
pub extern "C" fn spid3r_rt_start(entry: extern "C" fn() -> *mut Value) -> i32 {
    rt_init();
    ffi_guard("rt_start", |_| 70, || run_program(entry))
}

/// Run the compiled entry (class registration + top-level statements) and then
/// `main`, returning the process exit code. Top-level statements that throw
/// outside a `try` route to the entry's uncaught block (which exits); an
/// uncaught throw from `main` leaves the pending flag set, which is reported
/// here like the tree/VM top level before returning a non-zero exit code.
fn run_program(entry: extern "C" fn() -> *mut Value) -> i32 {
    // Count this thread as executing Spid3r code (see `sched::gc_quiescent`).
    let _active = crate::sched::enter_active();
    entry();
    let main = {
        let g = globals().lock().unwrap();
        g.get("main").cloned()
    };
    let code = match main {
        Some(Value::Cfn(f)) => {
            let argv: [*mut Value; 0] = [];
            let ret = unsafe { (f.code)(f.env, 0, argv.as_ptr()) };
            if spid3r_threw() != 0 {
                spid3r_uncaught();
            }
            match to_owned(ret) {
                Value::Int(i) => i as i32,
                _ => 0,
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
    // Join-on-exit (audit A-3): match the tree/VM engines — wait for tasks
    // `main` left running so their output is not lost when the process exits.
    // (`join_tasks_at_exit` prints its own warning when the deadline expires.)
    let _ = crate::sched::join_tasks_at_exit();
    // Deterministic teardown, like the CLI: stop the pool and join the workers
    // instead of letting process exit kill them mid-write.
    crate::sched::shutdown();
    // This is a C-style `main`: Rust's stdout flush-on-exit never runs, so
    // flush explicitly before handing the exit code back.
    let _ = std::io::stdout().flush();
    code
}

// ----------------------------------------------------------------------------
// concurrency — spawn / await  (native AOT)
//
// The tree/VM engines run tasks on the shared, process-wide worker pool in
// [`crate::sched::Scheduler`]; the native backend reuses that exact pool. A
// `spawn` queues a job that invokes the compiled closure on a worker thread;
// `await` joins a task (or receives from a channel). Values are `Arc<Mutex>`
// and the globals table is a shared mutex, so crossing the thread boundary is
// the same thread-safety the other engines rely on.
// ----------------------------------------------------------------------------

/// A compiled closure ready to run on a worker thread. Its `env` is a raw
/// pointer to a process-lifetime upvalue-cell array (allocated by
/// `spid3r_env_alloc`); the cells hold thread-safe `Arc<Mutex>` values, so
/// sharing the pointer across threads is sound — we only assert `Send`.
#[derive(Clone, Copy)]
struct SendCfn {
    code: unsafe extern "C" fn(*mut c_void, usize, *const *mut Value) -> *mut Value,
    env: *mut c_void,
}
unsafe impl Send for SendCfn {}

/// `spawn f` — run a compiled callable on the shared worker pool and return a
/// `Value::Task` handle (mirroring the tree/VM `spawn`). An uncaught `throw`
/// inside the task becomes the task's failure; `await` later surfaces it as
/// "task failed: <message>". A non-callable argument throws.
#[no_mangle]
pub extern "C" fn spid3r_spawn(f: *mut Value) -> *mut Value {
    match to_owned(f) {
        Value::Cfn(c) => {
            let cfn = SendCfn {
                code: c.code,
                env: c.env,
            };
            let sched = Scheduler::shared();
            let handle = sched.spawn_job(Box::new(move || {
                // Bind the whole `SendCfn` into a local so edition-2021 disjoint
                // capture takes the entire (Send) struct, not the raw `env` ptr.
                let cfn = cfn;
                let argv: [*mut Value; 0] = [];
                let r = unsafe { (cfn.code)(cfn.env, 0usize, argv.as_ptr()) };
                if spid3r_threw() != 0 {
                    let thrown = spid3r_take_thrown();
                    if thrown.is_null() {
                        crate::sched::TaskOutcome {
                            result: Err("task threw".to_string()),
                            thrown: None,
                        }
                    } else {
                        // Carry the thrown user value across the task
                        // boundary so `await` rethrows it structurally
                        // (audit A-9).
                        let v = to_owned(deref(thrown));
                        crate::sched::TaskOutcome {
                            thrown: Some(v.clone()),
                            result: Err(crate::value::display_value(&v)),
                        }
                    }
                } else {
                    crate::sched::TaskOutcome {
                        result: Ok(to_owned(r)),
                        thrown: None,
                    }
                }
            }));
            leak(Value::Task(handle))
        }
        other => rt_throw(&format!(
            "spawn expects a function, found {}",
            crate::value::type_name(&other)
        )),
    }
}

/// `await v` — join a task (blocking) or receive from a channel; anything else
/// is a catchable error. Matches the tree/VM `await` exactly.
#[no_mangle]
pub extern "C" fn spid3r_await(v: *mut Value) -> *mut Value {
    match to_owned(v) {
        Value::Task(t) => match t.join() {
            Ok(val) => leak(val),
            Err(msg) => match t.thrown_value() {
                // Rethrow the task's thrown user value structurally so `try`
                // rebinds the original value, not a string (audit A-9).
                Some(v) => rt_throw_value(v),
                None => rt_throw(&format!("task failed: {}", msg)),
            },
        },
        Value::Channel(c) => leak(c.recv()),
        other => rt_throw(&format!(
            "await expects a task or a channel, found {}",
            crate::value::type_name(&other)
        )),
    }
}

/// Runtime failure from generated code (type errors, missing fields, ...).
#[no_mangle]
pub extern "C" fn spid3r_abort(msg: *const c_char) -> ! {
    let msg = cstr(msg);
    abort(&msg)
}

/// Version string embedded in native binaries (`spid3r_version()`).
#[no_mangle]
pub extern "C" fn spid3r_runtime_version() -> *mut c_char {
    match CString::new(crate::VERSION) {
        Ok(c) => c.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

// ----------------------------------------------------------------------------
// unit tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(p: *const Value) -> Value {
        to_owned(p)
    }

    #[test]
    fn value_makers_roundtrip() {
        assert!(matches!(owned(spid3r_value_int(42)), Value::Int(42)));
        assert!(matches!(owned(spid3r_value_bool(1)), Value::Bool(true)));
        assert!(matches!(owned(spid3r_value_bool(0)), Value::Bool(false)));
        assert!(matches!(owned(spid3r_value_null()), Value::Null));
        if let Value::Float(f) = owned(spid3r_value_float(2.5)) {
            assert!((f - 2.5).abs() < f64::EPSILON);
        } else {
            panic!("expected a float");
        }
    }

    #[test]
    fn str_is_nul_terminated_safe() {
        let s = CString::new("héllo").unwrap();
        let v = spid3r_value_str(s.as_ptr() as *const u8, "héllo".len());
        match owned(v) {
            Value::Str(t) => assert_eq!(&*t, "héllo"),
            other => panic!("expected a string, got {:?}", other),
        }
    }

    #[test]
    fn binop_uses_vm_semantics() {
        let a = spid3r_value_int(7);
        let b = spid3r_value_int(3);
        let sum = spid3r_binop(a, b, 0); // Add
        let quo = spid3r_binop(a, b, 3); // Div
        let lt = spid3r_binop(a, b, 7); // Lt
        assert!(matches!(owned(sum), Value::Int(10)));
        assert!(matches!(owned(quo), Value::Int(2))); // int division like the VM
        assert!(matches!(owned(lt), Value::Bool(false))); // 7 < 3 is false
    }

    #[test]
    fn binop_string_concat() {
        let a = spid3r_value_str(c"ab".as_ptr() as *const u8, 2);
        let b = spid3r_value_str(c"cd".as_ptr() as *const u8, 2);
        let r = spid3r_binop(a, b, 0);
        match owned(r) {
            Value::Str(s) => assert_eq!(&*s, "abcd"),
            other => panic!("expected a string, got {:?}", other),
        }
    }

    #[test]
    fn truthy_matches_vm_rules() {
        assert_eq!(spid3r_truthy(spid3r_value_int(0)), 0);
        assert_eq!(spid3r_truthy(spid3r_value_int(1)), 1);
        assert_eq!(spid3r_truthy(spid3r_value_null()), 0);
        assert_eq!(spid3r_truthy(spid3r_value_bool(1)), 1);
    }

    #[test]
    fn globals_set_get_def_exists() {
        rt_init(); // idempotent
        let name = CString::new("__rt_test_g").unwrap();
        let v = spid3r_value_int(9);
        spid3r_global_set(name.as_ptr(), v);
        let got = spid3r_global_get(name.as_ptr());
        assert!(matches!(owned(got), Value::Int(9)));
        assert_ne!(spid3r_global_exists(name.as_ptr()), 0);
        let other = CString::new("__rt_test_absent").unwrap();
        assert_eq!(spid3r_global_exists(other.as_ptr()), 0);
    }

    #[test]
    fn forin_iterates_a_list() {
        let items = [
            spid3r_value_int(1),
            spid3r_value_int(2),
            spid3r_value_int(3),
        ];
        let list = spid3r_construct(CString::new("list").unwrap().as_ptr(), 3, items.as_ptr());
        let it = spid3r_forin_start(list);
        let mut seen = Vec::new();
        loop {
            let next = spid3r_forin_next(it);
            if next.is_null() {
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
        let cell = spid3r_cell_new(spid3r_value_int(1));
        spid3r_cell_set(cell, spid3r_value_int(2));
        assert!(matches!(owned(spid3r_cell_get(cell)), Value::Int(2)));
    }

    #[test]
    fn method_dispatch_on_list_len() {
        let name = CString::new("len").unwrap();
        let items = [spid3r_value_int(1), spid3r_value_int(2)];
        let list = spid3r_construct(CString::new("list").unwrap().as_ptr(), 2, items.as_ptr());
        let r = spid3r_method(list, name.as_ptr(), 0, std::ptr::null());
        assert!(matches!(owned(r), Value::Int(2)));
    }

    #[test]
    fn class_registration_and_construction() {
        unsafe {
            rt_init();
            // register a tiny class whose ctor stores argv[1] into field "x"
            let cls = spid3r_class_register(
                CString::new("__RtPoint").unwrap().as_ptr(),
                std::ptr::null(),
            );
            spid3r_class_add_field(cls, CString::new("x").unwrap().as_ptr(), 1);
            set_probe_ctor(cls);
            spid3r_global_set(CString::new("__RtPoint").unwrap().as_ptr(), cls);

            let arg = spid3r_value_int(77);
            let inst = construct_cinstance(
                match &owned(cls) {
                    Value::CClass(c) => c.clone(),
                    other => panic!("expected a class, got {:?}", other),
                },
                vec![owned(arg)],
            );
            let x = spid3r_field_get(inst, CString::new("x").unwrap().as_ptr());
            assert!(matches!(owned(x), Value::Int(77)));
        }
    }

    /// Attach a plain Rust constructor with the compiled ABI to `cls`:
    /// self at argv[0], store argv[1] into field "x" of the receiver.
    unsafe fn set_probe_ctor(cls: *mut Value) {
        extern "C" fn ctor(_env: *mut c_void, argc: u64, argv: *const *mut Value) -> *mut Value {
            assert_eq!(argc, 2);
            unsafe {
                let recv = *argv;
                let arg = *argv.add(1);
                let field = CString::new("x").unwrap();
                spid3r_field_set(recv, field.as_ptr(), arg);
                spid3r_value_null()
            }
        }
        let code: extern "C" fn(*mut c_void, u64, *const *mut Value) -> *mut Value = ctor;
        spid3r_class_set_ctor(cls, 2, 2, code as *const c_void);
    }
}
