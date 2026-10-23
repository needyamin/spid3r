//! The Spid3r bytecode virtual machine.
//!
//! A register-free stack machine: each `CallFrame` owns a `slots` vector that
//! doubles as its operand stack and its locals (clox style). Calls push a new
//! frame and `continue` the main loop; `OpReturn` pops the frame. Closures are
//! `Arc<VmFunc>` carrying `UpvalueCell`s so they remain `Send` and correct
//! across spawned tasks. All value operations (construct, method dispatch,
//! indexing, length, iteration, equality, display) are delegated to the shared
//! `value::collections` layer, keeping the VM semantically identical to the
//! tree-walk interpreter.

pub mod bytecode;
mod compiler;
mod coverage;
mod debug;
mod opt;

pub use debug::{DebugState, StepMode, DEBUG_QUIT};
pub use opt::{optimize_script, OptStats};

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::ast::{BinOp, Program, UnOp};
use crate::diagnostics::{Diagnostic, Phase, Span};
use crate::modules::ModuleCache;
use crate::parser;
use crate::sched::Scheduler;
use crate::semantic;
use crate::value::collections;
use crate::value::{
    new_ref, truthy, type_name, values_equal, NativeCtx, NativeFn, UpvalueCell, Value, VmClass,
    VmFunc, VmInstanceData,
};
use crate::vm::bytecode::*;
pub use crate::vm::compiler::{compile_program, compile_program_unoptimized};

/// Modules embedded by the compiler (keyed by import literal) so `exec` can run a
/// `.matrixc` without re-reading source: the compiled script function plus the
/// list of its exported public names.
type EmbeddedModules = Arc<Mutex<HashMap<String, (Arc<VmFunc>, Vec<String>)>>>;

/// Maximum Spid3r call depth before the VM reports a stack overflow.
pub const MAX_CALL_DEPTH: usize = 1_000;

/// A runtime error. `thrown` carries the user value for `throw`; `trace` is
/// the runtime call stack captured when the error surfaces (see
/// `capture_trace`).
#[derive(Clone, Debug)]
struct VmError {
    thrown: Option<Value>,
    message: String,
    span: Span,
    trace: Vec<crate::diagnostics::TraceFrame>,
}

impl VmError {
    fn to_diag(&self) -> Diagnostic {
        Diagnostic {
            // A `VmError` is only ever raised while *executing* (compile-time
            // failures return a `Diagnostic` directly), so it is a runtime error
            // and must read `error[runtime]` — the same as the tree interpreter.
            // `Phase::Vm` stays reserved for malformed bytecode (validator /
            // deserialize). Before this, the same program reported a different
            // phase depending on `--engine`; tests/engine_matrix.rs caught it.
            phase: Phase::Runtime,
            severity: crate::diagnostics::Severity::Error,
            lint: crate::diagnostics::Lint::None,
            message: self.message.clone(),
            span: Some(self.span.clone()),
            thrown: self.thrown.clone(),
            trace: self.trace.clone(),
            extras: None,
        }
    }

    /// The string a spawned task's error carries across the task boundary:
    /// the user-thrown value when there is one (so `join`/`await` rethrow
    /// surfaces `boom`, not a generic "uncaught throw"), else the message.
    fn task_error_string(&self) -> String {
        match &self.thrown {
            Some(v) => crate::value::display_value(v),
            None => self.message.clone(),
        }
    }
}

/// Where a `try` handler currently is in its lifecycle.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HandlerPhase {
    Try,
    Catch,
    Finally,
}

struct Handler {
    has_catch: bool,
    catch_slot: u8,
    catch_target: usize,
    finally_target: usize,
    stack_len: usize,
    frame_depth: usize,
    phase: HandlerPhase,
    pending_return: Option<Value>,
}

/// One function activation.
struct CallFrame {
    func: Arc<VmFunc>,
    /// The globals this frame resolves names against: the defining module's
    /// map, not the caller's (see `VmFunc::globals`).
    globals: Arc<Mutex<HashMap<String, Value>>>,
    /// Display name for stack traces. `None` means "use `func.name`". Method
    /// frames carry the qualified `Class::method` form and constructor frames
    /// `Class::new`, so a trace reads the same on the tree engine and the VM.
    display: Option<String>,
    ip: usize,
    /// Byte offset of the instruction currently executing (or waiting on a
    /// call) in this frame, recorded before `ip` advances past its operands.
    inst_ip: usize,
    slots: Vec<Value>,
    /// Captured locals: slot index -> shared cell. Created lazily on first
    /// capture so reads/writes of that local stay in sync after the frame
    /// returns and across tasks.
    captured: Vec<Option<UpvalueCell>>,
    /// Per-activation cells for this function's OWN captured locals (indices
    /// `[0..func.own_upvalues]`). Fresh on every call so two activations of
    /// the same closure never share captured state (recursion, repeated
    /// calls); the enclosing-captured cells live on the `VmFunc` instance in
    /// `func.upvalues[own..]` and are shared intentionally.
    own_cells: Vec<UpvalueCell>,
    /// Mutexes locked in this frame (released on return, like `finally`).
    /// Each token unlocks its mutex when dropped, so a panic unwinding
    /// through the VM cannot leave a user mutex locked.
    guards: Vec<crate::sched::MutexGuardToken>,
    /// When `Some`, `OpReturn` from this frame pushes this value instead of
    /// the function's own return (used by class constructors).
    ctor_result: Option<Value>,
}

/// The virtual machine.
pub struct Vm {
    globals: Arc<Mutex<HashMap<String, Value>>>,
    /// Names bound as `public const` by imported modules: the VM-level
    /// counterpart of the tree's per-environment const sets. `SetGlobal`
    /// refuses to rebind these names. Source-compiled programs never emit
    /// such an assignment (the semantic pass rejects it at compile time),
    /// so the runtime check only fires for crafted bytecode.
    global_consts: Arc<Mutex<HashSet<String>>>,
    /// The constants **declared by this script** (top-level `const`). A
    /// module VM gets its own set, so the module can report exactly which of
    /// its exports are `public const` (the importer freezes those in the
    /// namespace).
    consts: Arc<Mutex<HashSet<String>>>,
    sched: Arc<Scheduler>,
    modules: Arc<Mutex<ModuleCache>>,
    file: Arc<str>,
    module_dirs: Vec<PathBuf>,
    /// Modules embedded by the compiler, keyed by the import literal, so that
    /// `exec` can run a `.matrixc` without re-reading any `.matrix` source.
    embedded: EmbeddedModules,
    frames: Vec<CallFrame>,
    handlers: Vec<Handler>,
    error: Option<VmError>,
    reraise: Option<VmError>,
    depth: usize,
    /// When non-zero, `run` returns as soon as the frame stack shrinks back to
    /// this depth (used by synchronous callback calls: `list.map(f)` pushes a
    /// frame for `f`, runs to this depth, and takes its return value).
    stop: usize,
    /// Active debugger session (`spid3r debug`); `None` in normal runs.
    debug: Option<DebugState>,
    /// Line coverage (`spid3r run --coverage`); `None` in normal runs, so the
    /// hot loop pays one `Option` check per instruction when disabled.
    coverage: Option<coverage::Coverage>,
    /// Opcode profiler (`spid3r run --prof`); `None` in normal runs, so the
    /// hot loop pays a single well-predicted `Option` check per instruction.
    prof: Option<Profiler>,
    /// Monomorphic inline cache for `Invoke` on instances: keyed by
    /// (chunk address, instruction offset) → (class identity, resolved
    /// method). The class check makes it safe when a site sees several
    /// classes; `clear()` keeps the table bounded.
    invoke_cache: HashMap<(usize, usize), InvokeCache>,
    /// Recycled `CallFrame::slots` buffers to avoid malloc/free on every call.
    slot_pool: Vec<Vec<Value>>,
    /// Lazily-flushed growable string buffer for consecutive `s = s + rhs`
    /// local appends: `(frame_index, slot_index, owned_buffer)`.
    pending_str: Option<(usize, usize, String)>,
}

/// One inline-cache entry: the class the method was resolved on (by `Arc`
/// identity, so redefining a class invalidates the entry) and the method.
///
/// `_chunk` is a lifetime pin: it keeps the cached site's chunk alive for as
/// long as the entry exists, so the `(chunk address, ip)` key can never be
/// confused by a freed chunk whose address was reused by a different one.
struct InvokeCache {
    _chunk: Arc<Chunk>,
    class: usize,
    func: Arc<VmFunc>,
}

// ------------------------------------------------------------------ profiler

/// Opcode execution counts for `spid3r run --prof`. Indexed by the raw opcode
/// byte (the full byte range — `OpCode::from_u8` already rejected anything
/// else before the counter sees it), so counting is one array increment.
struct Profiler {
    counts: [u64; 256],
    instructions: u64,
}

impl Default for Profiler {
    fn default() -> Self {
        Profiler {
            counts: [0u64; 256],
            instructions: 0,
        }
    }
}

impl Profiler {
    fn count(&mut self, opcode_byte: u8) {
        self.counts[opcode_byte as usize] += 1;
        self.instructions += 1;
    }

    fn report(&self) -> String {
        let mut rows: Vec<(u64, &'static str)> = (0u8..=u8::MAX)
            .filter_map(|b| {
                let count = self.counts[b as usize];
                if count == 0 {
                    return None;
                }
                OpCode::from_u8(b).map(|op| (count, op.name()))
            })
            .collect();
        rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        let mut out = String::from("==== vm profile ====\n");
        out.push_str(&format!("instructions: {}\n", self.instructions));
        out.push_str("  count  %   opcode\n");
        for (count, name) in &rows {
            out.push_str(&format!(
                "{:>8}  {:>4.1}%  {}\n",
                count,
                100.0 * *count as f64 / self.instructions as f64,
                name
            ));
        }
        out
    }
}

impl Vm {
    /// Create a VM with its own globals (natives + `Object`) and module cache.
    pub fn new(file: Arc<str>, module_dirs: Vec<PathBuf>) -> Self {
        let mut vm = Vm {
            globals: Arc::new(Mutex::new(HashMap::new())),
            global_consts: Arc::new(Mutex::new(HashSet::new())),
            consts: Arc::new(Mutex::new(HashSet::new())),
            sched: Scheduler::shared(),
            modules: Arc::new(Mutex::new(ModuleCache::new())),
            file,
            module_dirs,
            embedded: Arc::new(Mutex::new(HashMap::new())),
            frames: Vec::with_capacity(16),
            handlers: Vec::new(),
            error: None,
            reraise: None,
            depth: 0,
            stop: 0,
            debug: None,
            coverage: None,
            prof: None,
            invoke_cache: HashMap::new(),
            slot_pool: Vec::with_capacity(16),
            pending_str: None,
        };
        vm.install_stdlib();
        vm
    }

    /// A VM that shares globals/imported-const names/scheduler/module cache
    /// with another (used for spawned tasks and imported modules).
    #[allow(clippy::too_many_arguments)] // the shared handles a child VM needs
    fn with_shared(
        globals: Arc<Mutex<HashMap<String, Value>>>,
        global_consts: Arc<Mutex<HashSet<String>>>,
        consts: Arc<Mutex<HashSet<String>>>,
        sched: Arc<Scheduler>,
        modules: Arc<Mutex<ModuleCache>>,
        embedded: EmbeddedModules,
        file: Arc<str>,
        module_dirs: Vec<PathBuf>,
    ) -> Self {
        Vm {
            globals,
            global_consts,
            consts,
            sched,
            modules,
            embedded,
            file,
            module_dirs,
            frames: Vec::with_capacity(8),
            handlers: Vec::new(),
            error: None,
            reraise: None,
            depth: 0,
            stop: 0,
            debug: None,
            coverage: None,
            prof: None,
            invoke_cache: HashMap::new(),
            slot_pool: Vec::with_capacity(8),
            pending_str: None,
        }
    }

    fn install_stdlib(&mut self) {
        for (name, func) in crate::stdlib::native_defs() {
            let native = NativeFn { name, func };
            self.globals
                .lock()
                .unwrap()
                .insert(name.to_string(), Value::Native(Arc::new(native)));
        }
        let object = Arc::new(VmClass {
            name: "Object".to_string(),
            parent: None,
            fields: Vec::new(),
            methods: HashMap::new(),
            ctor: None,
        });
        self.globals
            .lock()
            .unwrap()
            .insert("Object".to_string(), Value::VClass(object));
    }

    // --------------------------------------------------------- top-level run

    /// Run a program file: top-level statements, then `main`. Returns the
    /// process exit code.
    pub fn run_program(&mut self, program: &Program) -> Result<i32, Diagnostic> {
        let (script, _public) = compile_program(program, self.file.as_ref(), &self.module_dirs)?;
        self.run_compiled(Arc::new(script))
    }

    /// Run a script function that has already been compiled (e.g. loaded from a
    /// `.matrixc` file by `exec`). Mirrors the tail of `run_program`.
    pub fn run_compiled(&mut self, script: Arc<VmFunc>) -> Result<i32, Diagnostic> {
        // Count this thread as executing Spid3r code: the GC gate allows a
        // collection only when the caller is the only active thread (audit A-4).
        let _active = crate::sched::enter_active();
        // Populate the embedded-module table so `import` inside the program can
        // resolve from the self-contained `.matrixc` without re-reading source.
        {
            let mut emb = self.embedded.lock().unwrap();
            for m in &script.chunk.modules {
                emb.insert(m.key.clone(), (m.func.clone(), m.public.clone()));
            }
        }
        validate_chunk(&script.chunk)?;
        if let Some(c) = self.coverage.as_mut() {
            c.instrument(&script.chunk);
        }
        let result = (|| {
            self.call_vfunc_frame(script, Vec::new(), None)
                .map_err(|e| e.to_diag())?;
            let _ = self.run().map_err(|e| e.to_diag())?;
            let main = self.globals.lock().unwrap().get("main").cloned();
            match main {
                Some(Value::VFunc(f)) => {
                    self.call_vfunc_frame(f, Vec::new(), None)
                        .map_err(|e| e.to_diag())?;
                    let ret = self.run().map_err(|e| e.to_diag())?;
                    match ret {
                        Value::Int(i) => Ok(i as i32),
                        _ => Ok(0),
                    }
                }
                Some(_) => Err(Diagnostic::new(
                    Phase::Semantic,
                    "main must be a function: fn main() { ... }",
                )),
                None => Err(Diagnostic::new(
                    Phase::Semantic,
                    "this program has no fn main; add one to run it as a program",
                )),
            }
        })();
        // Join-on-exit (audit A-3): the VM runs on the caller thread, so wait
        // for tasks `main` left running before reporting the exit code.
        crate::sched::join_tasks_at_exit();
        result
    }

    /// Run a module: top-level statements only, then collect its public names.
    fn run_module(&mut self, program: &Program) -> Result<Vec<(String, Value, bool)>, Diagnostic> {
        let (script, public) = compile_program(program, self.file.as_ref(), &self.module_dirs)?;
        // Mirror `run_compiled`: expose any embedded modules to the import path.
        {
            let mut emb = self.embedded.lock().unwrap();
            for m in &script.chunk.modules {
                emb.insert(m.key.clone(), (m.func.clone(), m.public.clone()));
            }
        }
        validate_chunk(&script.chunk)?;
        self.call_vfunc_frame(Arc::new(script), Vec::new(), None)
            .map_err(|e| e.to_diag())?;
        let _ = self.run().map_err(|e| e.to_diag())?;
        // Const-ness of public bindings, from the module AST (mirrors the
        // tree runner's export flags).
        let consts: HashSet<String> = program
            .stmts
            .iter()
            .filter_map(|s| match s {
                crate::ast::Stmt::Const { name, vis, .. } if *vis == crate::ast::Vis::Public => {
                    Some(name.clone())
                }
                _ => None,
            })
            .collect();
        let g = self.globals.lock().unwrap();
        let mut exports = Vec::new();
        for name in public {
            if let Some(v) = g.get(&name) {
                let c = consts.contains(&name);
                exports.push((name, v.clone(), c));
            }
        }
        Ok(exports)
    }

    /// Run a single function to completion (used by spawned tasks).
    fn run_function(&mut self, fd: Arc<VmFunc>, args: Vec<Value>) -> Result<Value, VmError> {
        self.call_vfunc_frame(fd, args, None)?;
        self.run()
    }

    /// Call a Spid3r function value synchronously and return its result. Used
    /// by higher-order collection methods (`list.map(f)` and friends): the
    /// callback runs as a frame in THIS vm (sharing globals, scheduler and
    /// upvalue cells) and `run` returns as soon as the frame stack shrinks
    /// back to the current depth. Handler/depth state is deliberately NOT
    /// reset, so a `try` around the call still catches a callback's `throw`.
    /// Operator overloading: a map (the stdlib numeric types) or a class
    /// instance that carries a callable `__add__`/`__sub__`/`__mul__`/
    /// `__div__` handles the operator. `None` means "no overload": the caller
    /// reports its usual type error.
    fn try_operator(
        &mut self,
        l: &Value,
        r: &Value,
        method: &str,
    ) -> Option<Result<Value, String>> {
        let f = match l {
            Value::Map(_) => {
                crate::value::collections::index_get(l, &Value::Str(method.into())).ok()?
            }
            Value::VInstance(inst) => {
                let cls = inst.lock().unwrap().class.clone();
                Value::VFunc(find_method_vclass(&cls, method)?)
            }
            _ => return None,
        };
        if !matches!(
            f,
            Value::VFunc(_) | Value::Native(_) | Value::NativeClosure(_)
        ) {
            return None;
        }
        Some(
            self.call_value_sync(&f, vec![l.clone(), r.clone()])
                .map_err(|e| e.message),
        )
    }

    /// Unary operator dispatch (`__neg__`): same lookup as `try_operator`,
    /// but the method is called with the receiver alone.
    fn try_unary_operator(&mut self, v: &Value, method: &str) -> Option<Result<Value, String>> {
        let f = match v {
            Value::Map(_) => {
                crate::value::collections::index_get(v, &Value::Str(method.into())).ok()?
            }
            Value::VInstance(inst) => {
                let cls = inst.lock().unwrap().class.clone();
                Value::VFunc(find_method_vclass(&cls, method)?)
            }
            _ => return None,
        };
        if !matches!(
            f,
            Value::VFunc(_) | Value::Native(_) | Value::NativeClosure(_)
        ) {
            return None;
        }
        Some(
            self.call_value_sync(&f, vec![v.clone()])
                .map_err(|e| e.message),
        )
    }

    fn call_value_sync(&mut self, f: &Value, args: Vec<Value>) -> Result<Value, VmError> {
        match f {
            Value::VFunc(fd) => {
                let saved = self.stop;
                self.stop = self.frames.len();
                let r = self
                    .call_vfunc_frame(fd.clone(), args, None)
                    .and_then(|_| self.run_loop());
                self.stop = saved;
                r
            }
            Value::Native(n) => {
                let ctx = crate::value::NativeCtx {
                    sched: self.sched.clone(),
                };
                (n.func)(&ctx, args).map_err(|m| VmError {
                    thrown: None,
                    message: m,
                    span: self.cur_span(),
                    trace: Vec::new(),
                })
            }
            Value::NativeClosure(n) => {
                let ctx = crate::value::NativeCtx {
                    sched: self.sched.clone(),
                };
                (n.func)(&ctx, args).map_err(|m| VmError {
                    thrown: None,
                    message: m,
                    span: self.cur_span(),
                    trace: Vec::new(),
                })
            }
            other => Err(VmError {
                thrown: None,
                message: format!("{} is not callable", type_name(other)),
                span: self.cur_span(),
                trace: Vec::new(),
            }),
        }
    }

    // ----------------------------------------------------------- the loop

    /// Start a fresh top-level run: clear error state and execute until the
    /// frame stack is empty (or until `stop` is reached, for nested sync calls).
    fn run(&mut self) -> Result<Value, VmError> {
        if self.stop == 0 {
            self.handlers.clear();
            self.error = None;
            self.reraise = None;
            self.depth = 0;
        }
        self.run_loop()
    }

    /// Enable opcode profiling for this VM (`spid3r run --prof`). Counts every
    /// executed instruction by opcode; the report is retrieved with
    /// [`Self::profiling_report`] after the run.
    pub fn enable_profiling(&mut self) {
        self.prof = Some(Profiler::default());
    }

    /// Collect line coverage for the next run.
    pub fn enable_coverage(&mut self) {
        self.coverage = Some(coverage::Coverage::default());
    }

    /// Credit the instruction the innermost frame is about to execute.
    fn coverage_hit(&mut self) {
        let Some(f) = self.frames.last() else {
            return;
        };
        let span = f.func.chunk.span_at(f.ip);
        if let Some(c) = self.coverage.as_mut() {
            c.record(&span.file, span.line);
        }
    }

    /// The coverage collected so far, if any.
    pub fn coverage(&self) -> Option<&coverage::Coverage> {
        self.coverage.as_ref()
    }

    /// The profiling report: total instructions and per-opcode counts, hot
    /// opcodes first. Empty (and stderr-silent) unless profiling was enabled.
    pub fn profiling_report(&self) -> String {
        match &self.prof {
            Some(p) => p.report(),
            None => String::new(),
        }
    }

    fn run_loop(&mut self) -> Result<Value, VmError> {
        // Cancellation poll: a spawned task checks the flag every
        // CANCEL_POLL_INTERVAL instructions, so `task_cancel` stops a runaway
        // task without waiting for it to reach a call boundary.
        const CANCEL_POLL_INTERVAL: u32 = 2048;
        let mut cancel_poll: u32 = 0;
        loop {
            cancel_poll = cancel_poll.wrapping_add(1);
            if cancel_poll >= CANCEL_POLL_INTERVAL {
                cancel_poll = 0;
                if crate::sched::current_task_cancelled() {
                    self.fail("task cancelled");
                    continue;
                }
                // Cycle collection at the VM's periodic poll point — the VM's
                // analogue of the interpreter's statement boundary (the trigger
                // docs/GC.md's design specifies for both engines). Without it a
                // VM program that allocates cyclic garbage in a loop leaks every
                // cell until process exit (the tree engine collects them; the
                // VM never did). Safe here: an instruction boundary holds every
                // live value as a strong `Value` reference (frame slots, Rust
                // locals), so each is an external root; `maybe_collect` itself
                // no-ops unless the allocation threshold is reached and this is
                // the only thread executing Spid3r code.
                crate::value::gc::maybe_collect();
            }
            if let Some(mut err) = self.error.take() {
                self.flush_pending_str();
                // Capture the runtime call stack while the frames are still
                // alive: this is the single funnel every VM error passes
                // through, so attaching the trace here covers all of them.
                if err.trace.is_empty() {
                    err.trace = self.capture_trace();
                }
                self.unwind(err)?;
                if self.error.is_some() {
                    continue;
                }
            }
            let nf = self.frames.len() - 1;
            let ip = self.frames[nf].ip;
            self.frames[nf].inst_ip = ip;
            // Debugger: may pause here (breakpoints, stepping). It runs before
            // the instruction so the paused location is what is about to run.
            if self.debug.is_some() {
                self.flush_pending_str();
                self.debug_hook()?;
            }
            if self.coverage.is_some() {
                self.coverage_hit();
            }
            let ip = self.frames[nf].ip;
            self.frames[nf].inst_ip = ip;
            // Bounds-checked fetch (audit A-8): `validate_chunk` rejects chunks
            // that can fall off the end, but a corrupt `.matrixc` (or a future
            // opcode whose length changes) must still fail as a clean VM error
            // instead of panicking the host on `code[ip]`.
            let b = match self.frames[nf].func.chunk.code.get(ip) {
                Some(&b) => b,
                None => {
                    let len = self.frames[nf].func.chunk.code.len();
                    self.fail(format!(
                        "instruction pointer {} is outside the code section ({} bytes)",
                        ip, len
                    ));
                    continue;
                }
            };
            let op: OpCode = match OpCode::from_u8(b) {
                Some(o) => o,
                None => {
                    self.fail(format!("invalid opcode 0x{:02x} at offset {}", b, ip));
                    continue;
                }
            };
            if let Some(p) = self.prof.as_mut() {
                p.count(b);
            }
            match op {
                OpCode::Constant => {
                    let idx = self.code_u16(nf, ip + 1) as usize;
                    let v = self.const_at(nf, idx);
                    self.frames[nf].ip = ip + 3;
                    self.frames[nf].slots.push(v);
                }
                OpCode::Null => {
                    self.frames[nf].ip = ip + 1;
                    self.frames[nf].slots.push(Value::Null);
                }
                OpCode::True => {
                    self.frames[nf].ip = ip + 1;
                    self.frames[nf].slots.push(Value::Bool(true));
                }
                OpCode::False => {
                    self.frames[nf].ip = ip + 1;
                    self.frames[nf].slots.push(Value::Bool(false));
                }
                OpCode::None => {
                    self.frames[nf].ip = ip + 1;
                    self.frames[nf].slots.push(Value::Option(None));
                }
                OpCode::Pop => {
                    self.frames[nf].ip = ip + 1;
                    let _ = self.pop_top();
                }
                OpCode::GetLocal => {
                    let slot = self.code_u8(nf, ip + 1) as usize;
                    if self.debug.is_none() && self.frames[nf].captured.is_empty() {
                        let code = &self.frames[nf].func.chunk.code;
                        if let Some(&next_op) = code.get(ip + 2) {
                            if next_op == OpCode::GetLocal as u8 {
                                let slot_b = code[ip + 3] as usize;
                                if let Some(&op2) = code.get(ip + 4) {
                                    if op2 == OpCode::Less as u8
                                        && code.get(ip + 5) == Some(&(OpCode::JumpIfFalse as u8))
                                    {
                                        if let (Some(&Value::Int(a)), Some(&Value::Int(b))) = (
                                            self.frames[nf].slots.get(slot),
                                            self.frames[nf].slots.get(slot_b),
                                        ) {
                                            if a < b {
                                                self.frames[nf].ip = ip + 8;
                                            } else {
                                                let off = i16::from_le_bytes([
                                                    code[ip + 6],
                                                    code[ip + 7],
                                                ]);
                                                self.frames[nf].ip =
                                                    ((ip + 8) as i64 + off as i64) as usize;
                                            }
                                            continue;
                                        }
                                    } else if op2 == OpCode::Add as u8
                                        && code.get(ip + 5) == Some(&(OpCode::SetLocal as u8))
                                        && code.get(ip + 7) == Some(&(OpCode::Pop as u8))
                                    {
                                        let dst = code[ip + 6] as usize;
                                        if let (Some(&Value::Int(a)), Some(&Value::Int(b))) = (
                                            self.frames[nf].slots.get(slot),
                                            self.frames[nf].slots.get(slot_b),
                                        ) {
                                            if dst < self.frames[nf].slots.len() {
                                                if let Some(v) = a.checked_add(b) {
                                                    self.frames[nf].slots[dst] = Value::Int(v);
                                                    self.frames[nf].ip = ip + 8;
                                                    continue;
                                                }
                                            }
                                        }
                                    }
                                }
                            } else if next_op == OpCode::Constant as u8 && ip + 5 < code.len() {
                                let cidx =
                                    u16::from_le_bytes([code[ip + 3], code[ip + 4]]) as usize;
                                let op2 = code[ip + 5];
                                if op2 == OpCode::Add as u8
                                    && code.get(ip + 6) == Some(&(OpCode::SetLocal as u8))
                                    && code.get(ip + 8) == Some(&(OpCode::Pop as u8))
                                {
                                    let dst = code[ip + 7] as usize;
                                    if let (Some(&Value::Int(a)), Some(&Value::Int(b))) = (
                                        self.frames[nf].slots.get(slot),
                                        self.frames[nf].func.chunk.constants.get(cidx),
                                    ) {
                                        if dst < self.frames[nf].slots.len() {
                                            if let Some(v) = a.checked_add(b) {
                                                self.frames[nf].slots[dst] = Value::Int(v);
                                                self.frames[nf].ip = ip + 9;
                                                continue;
                                            }
                                        }
                                    } else if dst == slot {
                                        if let Some(Value::Str(rhs)) =
                                            self.frames[nf].func.chunk.constants.get(cidx).cloned()
                                        {
                                            if let Some((pf, ps, ref mut buf)) = self.pending_str {
                                                if pf == nf && ps == slot {
                                                    buf.push_str(&rhs);
                                                    self.frames[nf].ip = ip + 9;
                                                    continue;
                                                }
                                            }
                                            self.flush_pending_str();
                                            if let Some(Value::Str(lhs)) =
                                                self.frames[nf].slots.get(slot)
                                            {
                                                let mut buf = String::with_capacity(
                                                    (lhs.len() + rhs.len() + 64).max(128),
                                                );
                                                buf.push_str(lhs);
                                                buf.push_str(&rhs);
                                                self.pending_str = Some((nf, slot, buf));
                                                self.frames[nf].ip = ip + 9;
                                                continue;
                                            }
                                        }
                                    }
                                } else if let (Some(&Value::Int(a)), Some(&Value::Int(b))) = (
                                    self.frames[nf].slots.get(slot),
                                    self.frames[nf].func.chunk.constants.get(cidx),
                                ) {
                                    if op2 == OpCode::Mul as u8 {
                                        self.frames[nf].ip = ip + 6;
                                        if let Some(v) = a.checked_mul(b) {
                                            self.frames[nf].slots.push(Value::Int(v));
                                            continue;
                                        }
                                        continue;
                                    } else if op2 == OpCode::Div as u8 && b != 0 {
                                        self.frames[nf].ip = ip + 6;
                                        self.frames[nf].slots.push(Value::Int(a.wrapping_div(b)));
                                        continue;
                                    } else if op2 == OpCode::Mod as u8 && b != 0 {
                                        self.frames[nf].ip = ip + 6;
                                        self.frames[nf].slots.push(Value::Int(a.wrapping_rem(b)));
                                        continue;
                                    } else if op2 == OpCode::Sub as u8 {
                                        if let Some(v) = a.checked_sub(b) {
                                            self.frames[nf].ip = ip + 6;
                                            self.frames[nf].slots.push(Value::Int(v));
                                            continue;
                                        }
                                    } else if op2 == OpCode::Add as u8 {
                                        if let Some(v) = a.checked_add(b) {
                                            self.frames[nf].ip = ip + 6;
                                            self.frames[nf].slots.push(Value::Int(v));
                                            continue;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if self.pending_str.is_some() {
                        self.flush_pending_str();
                    }
                    self.frames[nf].ip = ip + 2;
                    let v = {
                        let f = &self.frames[nf];
                        if let Some(Some(c)) = f.captured.get(slot) {
                            c.lock().unwrap().clone()
                        } else {
                            f.slots.get(slot).cloned().unwrap_or(Value::Null)
                        }
                    };
                    self.frames[nf].slots.push(v);
                }
                OpCode::SetLocal => {
                    let slot = self.code_u8(nf, ip + 1) as usize;
                    if self
                        .pending_str
                        .as_ref()
                        .is_some_and(|&(pf, ps, _)| pf == nf && ps == slot)
                    {
                        self.pending_str = None;
                    }
                    self.frames[nf].ip = ip + 2;
                    // Peek (do not pop) the assigned value: in the clox shared
                    // stack model an assignment leaves its value on top so the
                    // enclosing expression/statement balances it with `OpPop`.
                    let v = self.frames[nf].slots.last().cloned().unwrap_or(Value::Null);
                    {
                        let f = &mut self.frames[nf];
                        if let Some(Some(c)) = f.captured.get(slot) {
                            *c.lock().unwrap() = v.clone();
                        }
                        if slot < f.slots.len() {
                            f.slots[slot] = v;
                        } else {
                            f.slots.resize(slot + 1, Value::Null);
                            f.slots[slot] = v;
                        }
                    }
                }
                OpCode::GetGlobal => {
                    let idx = self.code_u16(nf, ip + 1) as usize;
                    self.frames[nf].ip = ip + 3;
                    let v = match self.frames[nf].func.chunk.constants.get(idx) {
                        Some(Value::Str(name)) => self.frames[nf]
                            .globals
                            .lock()
                            .unwrap()
                            .get(name.as_ref())
                            .cloned()
                            .unwrap_or(Value::Null),
                        _ => {
                            self.fail("global name is not a string");
                            continue;
                        }
                    };
                    self.frames[nf].slots.push(v);
                }
                OpCode::SetGlobal => {
                    let idx = self.code_u16(nf, ip + 1) as usize;
                    self.frames[nf].ip = ip + 3;
                    let name = match self.const_at(nf, idx) {
                        Value::Str(s) => s,
                        _ => {
                            self.fail("global name is not a string");
                            continue;
                        }
                    };
                    let v = self.frames[nf].slots.last().cloned().unwrap_or(Value::Null);
                    // Imported `public const` names are immutable: the tree
                    // rejects the rebind in `environment::assign` with this
                    // same message. Declarations (`DefineGlobal`) stay
                    // unchecked — the tree's `environment::define` does not
                    // consult const sets either.
                    if self.global_consts.lock().unwrap().contains(name.as_ref()) {
                        self.fail(format!("cannot assign to const '{}'", name));
                        continue;
                    }
                    let mut g = self.frames[nf].globals.lock().unwrap();
                    match g.get_mut(name.as_ref()) {
                        Some(slot) => *slot = v,
                        None => {
                            g.insert(name.to_string(), v);
                        }
                    }
                }
                OpCode::DefineGlobal => {
                    let idx = self.code_u16(nf, ip + 1) as usize;
                    self.frames[nf].ip = ip + 3;
                    let name = match self.const_at(nf, idx) {
                        Value::Str(s) => s,
                        _ => {
                            self.fail("global name is not a string");
                            continue;
                        }
                    };
                    let v = self.pop_top();
                    self.frames[nf]
                        .globals
                        .lock()
                        .unwrap()
                        .insert(name.to_string(), v);
                }
                OpCode::DefineConst => {
                    let idx = self.code_u16(nf, ip + 1) as usize;
                    self.frames[nf].ip = ip + 3;
                    let name = match self.const_at(nf, idx) {
                        Value::Str(s) => s,
                        _ => {
                            self.fail("global name is not a string");
                            continue;
                        }
                    };
                    let v = self.pop_top();
                    // Record the name as a constant of this script, so a
                    // rebind is rejected and an importing module sees the
                    // export as a `public const`.
                    self.consts.lock().unwrap().insert(name.to_string());
                    self.frames[nf]
                        .globals
                        .lock()
                        .unwrap()
                        .insert(name.to_string(), v);
                }
                OpCode::SuperCtor | OpCode::SuperMethod => {
                    let is_ctor = matches!(op, OpCode::SuperCtor);
                    let (pidx, midx, argc, next_ip) = if is_ctor {
                        let pidx = self.code_u16(nf, ip + 1) as usize;
                        let argc = self.code_u8(nf, ip + 3) as usize;
                        (pidx, 0usize, argc, ip + 4)
                    } else {
                        let pidx = self.code_u16(nf, ip + 1) as usize;
                        let midx = self.code_u16(nf, ip + 3) as usize;
                        let argc = self.code_u8(nf, ip + 5) as usize;
                        (pidx, midx, argc, ip + 6)
                    };
                    self.frames[nf].ip = next_ip;
                    let parent = match self.const_at(nf, pidx) {
                        Value::Str(s) => s.to_string(),
                        _ => {
                            self.fail("super: parent class name is not a string");
                            continue;
                        }
                    };
                    let method = if is_ctor {
                        None
                    } else {
                        match self.const_at(nf, midx) {
                            Value::Str(s) => Some(s.to_string()),
                            _ => {
                                self.fail("super: method name is not a string");
                                continue;
                            }
                        }
                    };
                    // The receiver is the executing frame's `self` (slot 0).
                    let recv = self.frames[nf]
                        .slots
                        .first()
                        .cloned()
                        .unwrap_or(Value::Null);
                    let args = self.pop_n(argc);
                    // The parent class is a global.
                    let class = {
                        let g = self.globals.lock().unwrap();
                        match g.get(&parent) {
                            Some(Value::VClass(c)) => Some(c.clone()),
                            _ => None,
                        }
                    };
                    let Some(class) = class else {
                        self.fail(format!("super: unknown class '{}'", parent));
                        continue;
                    };
                    let target = match &method {
                        Some(m) => match find_method_vclass(&class, m) {
                            Some(fd) => fd,
                            None => {
                                self.fail(format!("class {} has no method '{}'", class.name, m));
                                continue;
                            }
                        },
                        None => match &class.ctor {
                            Some(c) => c.clone(),
                            None => {
                                self.fail(format!("class {} has no constructor", class.name));
                                continue;
                            }
                        },
                    };
                    let want = match &method {
                        // A method takes `self` explicitly; a constructor gets
                        // a synthetic one.
                        Some(_) => argc + 1,
                        None => argc + 1,
                    };
                    if want < target.min_arity || want > target.arity {
                        let want_msg = if target.min_arity == target.arity {
                            format!("{} argument(s)", target.arity - 1)
                        } else {
                            format!(
                                "{} to {} argument(s)",
                                target.min_arity - 1,
                                target.arity - 1
                            )
                        };
                        self.fail(format!(
                            "{} expects {}, got {}",
                            target.name.as_deref().unwrap_or("fn"),
                            want_msg,
                            argc
                        ));
                        continue;
                    }
                    let mut call_args = self.alloc_slots(want);
                    call_args.push(recv);
                    call_args.extend(args);
                    let display = match &method {
                        Some(m) => Some(format!("{}::{}", class.name, m)),
                        None => Some(format!("{}::new", class.name)),
                    };
                    if let Err(e) = self.call_vfunc_frame(target, call_args, display) {
                        self.error = Some(e);
                        continue;
                    }
                }
                OpCode::GetUpvalue => {
                    let slot = self.code_u8(nf, ip + 1) as usize;
                    self.frames[nf].ip = ip + 2;
                    let v = {
                        let f = &self.frames[nf];
                        match f.func.upvalues.get(slot).cloned() {
                            Some(c) => c.lock().unwrap().clone(),
                            None => Value::Null,
                        }
                    };
                    self.frames[nf].slots.push(v);
                }
                OpCode::SetUpvalue => {
                    let slot = self.code_u8(nf, ip + 1) as usize;
                    self.frames[nf].ip = ip + 2;
                    // Peek (do not pop), matching `SetLocal`.
                    let v = self.frames[nf].slots.last().cloned().unwrap_or(Value::Null);
                    let f = &mut self.frames[nf];
                    if let Some(c) = f.func.upvalues.get(slot).cloned() {
                        *c.lock().unwrap() = v;
                    }
                }
                OpCode::Closure => {
                    self.flush_pending_str();
                    let idx = self.code_u16(nf, ip + 1) as usize;
                    self.frames[nf].ip = ip + 3;
                    let fproto = self.func_proto_at(nf, idx);
                    let closure = self.make_closure(fproto, nf);
                    self.frames[nf].slots.push(Value::VFunc(closure));
                }
                OpCode::Class => {
                    self.flush_pending_str();
                    let idx = self.code_u16(nf, ip + 1) as usize;
                    self.frames[nf].ip = ip + 3;
                    let cproto = self.class_proto_at(nf, idx);
                    match self.build_class(cproto, nf) {
                        Ok(vclass) => self.frames[nf].slots.push(Value::VClass(vclass)),
                        Err(e) => {
                            self.error = Some(e);
                            continue;
                        }
                    }
                }
                OpCode::Add => {
                    let r = self.pop_top();
                    let l = self.pop_top();
                    let res = match (&l, &r) {
                        (Value::Int(a), Value::Int(b)) => {
                            crate::value::checked_int("+", *a, *b).map(Value::Int)
                        }
                        _ => match self
                            .try_operator(&l, &r, "__add__")
                            .or_else(|| self.try_operator(&r, &l, "__radd__"))
                        {
                            Some(over) => over,
                            None => vm_eval_binary(BinOp::Add, l.clone(), r.clone()),
                        },
                    };
                    match res {
                        Ok(v) => {
                            if self.debug.is_none() && self.frames[nf].captured.is_empty() {
                                let code = &self.frames[nf].func.chunk.code;
                                if code.get(ip + 1) == Some(&(OpCode::SetLocal as u8))
                                    && code.get(ip + 3) == Some(&(OpCode::Pop as u8))
                                {
                                    let dst = code[ip + 2] as usize;
                                    if dst < self.frames[nf].slots.len() {
                                        self.frames[nf].slots[dst] = v;
                                        self.frames[nf].ip = ip + 4;
                                        continue;
                                    }
                                }
                            }
                            self.frames[nf].ip = ip + 1;
                            self.frames[nf].slots.push(v);
                        }
                        Err(m) => {
                            self.frames[nf].ip = ip + 1;
                            self.fail(m);
                        }
                    }
                }
                OpCode::Sub => {
                    self.frames[nf].ip = ip + 1;
                    let r = self.pop_top();
                    if let (Some(Value::Int(a)), Value::Int(b)) =
                        (self.frames[nf].slots.last_mut(), &r)
                    {
                        if let Some(v) = a.checked_sub(*b) {
                            *a = v;
                            continue;
                        }
                    }
                    let l = self.pop_top();
                    match match self
                        .try_operator(&l, &r, "__sub__")
                        .or_else(|| self.try_operator(&r, &l, "__rsub__"))
                    {
                        Some(over) => over,
                        None => vm_eval_binary(BinOp::Sub, l.clone(), r.clone()),
                    } {
                        Ok(v) => self.frames[nf].slots.push(v),
                        Err(m) => self.fail(m),
                    }
                }
                OpCode::Mul => {
                    self.frames[nf].ip = ip + 1;
                    let r = self.pop_top();
                    if let (Some(Value::Int(a)), Value::Int(b)) =
                        (self.frames[nf].slots.last_mut(), &r)
                    {
                        if let Some(v) = a.checked_mul(*b) {
                            *a = v;
                            continue;
                        }
                    }
                    let l = self.pop_top();
                    match match self
                        .try_operator(&l, &r, "__mul__")
                        .or_else(|| self.try_operator(&r, &l, "__rmul__"))
                    {
                        Some(over) => over,
                        None => vm_eval_binary(BinOp::Mul, l.clone(), r.clone()),
                    } {
                        Ok(v) => self.frames[nf].slots.push(v),
                        Err(m) => self.fail(m),
                    }
                }
                OpCode::Div => {
                    self.frames[nf].ip = ip + 1;
                    let r = self.pop_top();
                    if let (Some(Value::Int(a)), Value::Int(b)) =
                        (self.frames[nf].slots.last_mut(), &r)
                    {
                        if *b != 0 {
                            *a = a.wrapping_div(*b);
                            continue;
                        }
                    }
                    let l = self.pop_top();
                    match match self
                        .try_operator(&l, &r, "__div__")
                        .or_else(|| self.try_operator(&r, &l, "__rdiv__"))
                    {
                        Some(over) => over,
                        None => vm_eval_binary(BinOp::Div, l.clone(), r.clone()),
                    } {
                        Ok(v) => self.frames[nf].slots.push(v),
                        Err(m) => self.fail(m),
                    }
                }
                OpCode::Mod => {
                    self.frames[nf].ip = ip + 1;
                    let r = self.pop_top();
                    if let (Some(Value::Int(a)), Value::Int(b)) =
                        (self.frames[nf].slots.last_mut(), &r)
                    {
                        if *b != 0 {
                            *a = a.wrapping_rem(*b);
                            continue;
                        }
                    }
                    let l = self.pop_top();
                    match vm_eval_binary(BinOp::Mod, l, r) {
                        Ok(v) => self.frames[nf].slots.push(v),
                        Err(m) => self.fail(m),
                    }
                }
                OpCode::Neg => {
                    self.frames[nf].ip = ip + 1;
                    let v = self.pop_top();
                    let res = match self.try_unary_operator(&v, "__neg__") {
                        Some(over) => over,
                        None => vm_eval_unary(UnOp::Neg, v),
                    };
                    match res {
                        Ok(x) => self.frames[nf].slots.push(x),
                        Err(m) => self.fail(m),
                    }
                }
                OpCode::Not => {
                    self.frames[nf].ip = ip + 1;
                    let v = self.pop_top();
                    match vm_eval_unary(UnOp::Not, v) {
                        Ok(x) => self.frames[nf].slots.push(x),
                        Err(m) => self.fail(m),
                    }
                }
                OpCode::Equal => {
                    self.frames[nf].ip = ip + 1;
                    let r = self.pop_top();
                    let l = self.pop_top();
                    self.frames[nf]
                        .slots
                        .push(Value::Bool(values_equal(&l, &r)));
                }
                OpCode::NotEqual => {
                    self.frames[nf].ip = ip + 1;
                    let r = self.pop_top();
                    let l = self.pop_top();
                    self.frames[nf]
                        .slots
                        .push(Value::Bool(!values_equal(&l, &r)));
                }
                OpCode::Less => {
                    self.frames[nf].ip = ip + 1;
                    let r = self.pop_top();
                    if let (Some(Value::Int(a)), Value::Int(b)) = (self.frames[nf].slots.last(), &r)
                    {
                        let res = *a < *b;
                        *self.frames[nf].slots.last_mut().unwrap() = Value::Bool(res);
                        continue;
                    }
                    let l = self.pop_top();
                    match vm_eval_binary(BinOp::Lt, l, r) {
                        Ok(v) => self.frames[nf].slots.push(v),
                        Err(m) => self.fail(m),
                    }
                }
                OpCode::LessEq => {
                    self.frames[nf].ip = ip + 1;
                    let r = self.pop_top();
                    let l = self.pop_top();
                    match vm_eval_binary(BinOp::Le, l, r) {
                        Ok(v) => self.frames[nf].slots.push(v),
                        Err(m) => self.fail(m),
                    }
                }
                OpCode::Greater => {
                    self.frames[nf].ip = ip + 1;
                    let r = self.pop_top();
                    let l = self.pop_top();
                    match vm_eval_binary(BinOp::Gt, l, r) {
                        Ok(v) => self.frames[nf].slots.push(v),
                        Err(m) => self.fail(m),
                    }
                }
                OpCode::GreaterEq => {
                    self.frames[nf].ip = ip + 1;
                    let r = self.pop_top();
                    let l = self.pop_top();
                    match vm_eval_binary(BinOp::Ge, l, r) {
                        Ok(v) => self.frames[nf].slots.push(v),
                        Err(m) => self.fail(m),
                    }
                }
                OpCode::Jump => {
                    let off = self.code_i16(nf, ip + 1);
                    let target = (ip as i64 + 3 + off as i64) as usize;
                    if target >= self.frames[nf].func.chunk.code.len() {
                        self.fail("invalid jump target");
                        continue;
                    }
                    self.frames[nf].ip = target;
                }
                OpCode::JumpIfFalse => {
                    let off = self.code_i16(nf, ip + 1);
                    let v = self.pop_top();
                    if !truthy(&v) {
                        let target = (ip as i64 + 3 + off as i64) as usize;
                        if target >= self.frames[nf].func.chunk.code.len() {
                            self.fail("invalid jump target");
                            continue;
                        }
                        self.frames[nf].ip = target;
                    } else {
                        self.frames[nf].ip = ip + 3;
                    }
                }
                OpCode::JumpIfTrue => {
                    let off = self.code_i16(nf, ip + 1);
                    let v = self.pop_top();
                    if truthy(&v) {
                        let target = (ip as i64 + 3 + off as i64) as usize;
                        if target >= self.frames[nf].func.chunk.code.len() {
                            self.fail("invalid jump target");
                            continue;
                        }
                        self.frames[nf].ip = target;
                    } else {
                        self.frames[nf].ip = ip + 3;
                    }
                }
                OpCode::Loop => {
                    let off = self.code_i16(nf, ip + 1);
                    let target = (ip as i64 + 3 + off as i64) as usize;
                    if target >= self.frames[nf].func.chunk.code.len() {
                        self.fail("invalid jump target");
                        continue;
                    }
                    self.frames[nf].ip = target;
                }
                OpCode::Call => {
                    let argc = self.code_u8(nf, ip + 1) as usize;
                    self.frames[nf].ip = ip + 2;
                    let slen = self.frames[nf].slots.len();
                    if argc == 1 && slen >= 2 {
                        if let Value::Native(n) = &self.frames[nf].slots[slen - 2] {
                            if n.name == "string" {
                                let arg = self.frames[nf].slots.pop().unwrap();
                                self.frames[nf].slots.pop();
                                let s: Arc<str> = match arg {
                                    Value::Int(i) => i.to_string().into(),
                                    Value::Str(s) => s,
                                    other => crate::value::display_value(&other).into(),
                                };
                                self.frames[nf].slots.push(Value::Str(s));
                                continue;
                            } else if n.name == "len" {
                                let arg = self.frames[nf].slots.pop().unwrap();
                                self.frames[nf].slots.pop();
                                match collections::len_of(&arg) {
                                    Ok(len) => self.frames[nf].slots.push(Value::Int(len as i64)),
                                    Err(msg) => self.fail(msg),
                                }
                                continue;
                            }
                        }
                    }
                    let args = self.pop_n(argc);
                    let callee = self.pop_top();
                    match callee {
                        Value::VFunc(fd) => {
                            if let Err(e) = self.call_vfunc_frame(fd, args, None) {
                                self.error = Some(e);
                                continue;
                            }
                        }
                        Value::Native(n) => {
                            let ctx = NativeCtx {
                                sched: self.sched.clone(),
                            };
                            match (n.func)(&ctx, args) {
                                Ok(v) => self.frames[nf].slots.push(v),
                                Err(msg) => self.fail(msg),
                            }
                        }
                        Value::NativeClosure(n) => {
                            let ctx = NativeCtx {
                                sched: self.sched.clone(),
                            };
                            match (n.func)(&ctx, args) {
                                Ok(v) => self.frames[nf].slots.push(v),
                                Err(msg) => self.fail(msg),
                            }
                        }
                        Value::VClass(c) => {
                            if let Err(e) = self.new_instance_vm(c, args) {
                                self.error = Some(e);
                                continue;
                            }
                        }
                        Value::Class(cd) => {
                            let vc = tree_class_to_vclass(&cd);
                            if let Err(e) = self.new_instance_vm(vc, args) {
                                self.error = Some(e);
                                continue;
                            }
                        }
                        other => self.fail(format!("{} is not callable", type_name(&other))),
                    }
                }
                OpCode::Invoke => {
                    let idx = self.code_u16(nf, ip + 1) as usize;
                    let argc = self.code_u8(nf, ip + 3) as usize;
                    self.frames[nf].ip = ip + 4;
                    let name = match self.const_at(nf, idx) {
                        Value::Str(s) => s,
                        _ => {
                            self.fail("method name is not a string");
                            continue;
                        }
                    };
                    let slen = self.frames[nf].slots.len();
                    if slen > argc {
                        let recv_idx = slen - 1 - argc;
                        match &self.frames[nf].slots[recv_idx] {
                            Value::List(d) | Value::VecDyn(d) => {
                                let mname = name.as_ref();
                                if mname == "push" && argc == 1 {
                                    let val = self.frames[nf].slots.pop().unwrap();
                                    let d = match self.frames[nf].slots.pop().unwrap() {
                                        Value::List(d) | Value::VecDyn(d) => d,
                                        _ => unreachable!(),
                                    };
                                    d.lock().unwrap().items.push(val);
                                    self.frames[nf].slots.push(Value::Null);
                                    continue;
                                } else if mname == "pop" && argc == 0 {
                                    let popped = d.lock().unwrap().items.pop();
                                    if let Some(v) = popped {
                                        self.frames[nf].slots[recv_idx] = v;
                                        continue;
                                    }
                                } else if mname == "len" && argc == 0 {
                                    let n = d.lock().unwrap().items.len() as i64;
                                    self.frames[nf].slots[recv_idx] = Value::Int(n);
                                    continue;
                                }
                            }
                            Value::MutexVal(m) => {
                                // Detach from the frame borrow: the lock paths
                                // below mutate the frame.
                                let m = m.clone();
                                // `mu.lock()` must be recorded on the frame so
                                // the lock is released when the frame returns —
                                // and when a panic unwinds through the VM. The
                                // tree engine has the same discipline in its
                                // method dispatch.
                                let mname = name.as_ref();
                                if mname == "lock" && argc == 0 {
                                    let v = m.lock();
                                    self.frames[nf]
                                        .guards
                                        .push(crate::sched::MutexGuardToken::new(m.clone()));
                                    self.frames[nf].slots[recv_idx] = v;
                                    continue;
                                }
                                if mname == "unlock" && argc == 0 {
                                    m.unlock();
                                    for g in self.frames[nf].guards.iter_mut() {
                                        if let Some(held) = g.mutex() {
                                            if std::sync::Arc::ptr_eq(held, &m) {
                                                g.disarm();
                                            }
                                        }
                                    }
                                    self.frames[nf].slots[recv_idx] = Value::Null;
                                    continue;
                                }
                            }
                            Value::Map(m) => {
                                let mname = name.as_ref();
                                // A **module namespace** is a read-only view of
                                // a module's exports: an exported name wins
                                // over the map's own method (`mu.get()` is the
                                // module's `get`, not `map.get`), and the
                                // namespace cannot be mutated. Both rules live
                                // in the shared dispatcher, so the VM and the
                                // tree engine cannot diverge.
                                if m.lock().unwrap().namespace {
                                    let args = self.pop_n(argc);
                                    let obj = self.pop_top();
                                    match self.host_callback_method(&obj, &name, args) {
                                        Ok(v) => self.frames[nf].slots.push(v),
                                        Err(e) => self.error = Some(e),
                                    }
                                    continue;
                                }
                                if mname == "set" && argc == 2 {
                                    let val = self.frames[nf].slots.pop().unwrap();
                                    let key = self.frames[nf].slots.pop().unwrap();
                                    if let Ok(mk) = crate::value::as_map_key(&key) {
                                        if let Value::Map(m) = &self.frames[nf].slots[recv_idx] {
                                            m.lock().unwrap().set_key(mk, val);
                                        }
                                        self.frames[nf].slots[recv_idx] = Value::Null;
                                        continue;
                                    } else {
                                        self.frames[nf].slots.push(key);
                                        self.frames[nf].slots.push(val);
                                    }
                                } else if mname == "get" && argc == 1 {
                                    let key = self.frames[nf].slots.pop().unwrap();
                                    if let Ok(mk) = crate::value::as_map_key(&key) {
                                        let found = if let Value::Map(m) =
                                            &self.frames[nf].slots[recv_idx]
                                        {
                                            let guard = m.lock().unwrap();
                                            guard.find(&mk).map(|i| guard.entries[i].1.clone())
                                        } else {
                                            None
                                        };
                                        if let Some(v) = found {
                                            self.frames[nf].slots[recv_idx] = v;
                                            continue;
                                        } else {
                                            self.frames[nf].slots.push(key);
                                        }
                                    } else {
                                        self.frames[nf].slots.push(key);
                                    }
                                } else if mname == "len" && argc == 0 {
                                    let n = m.lock().unwrap().entries.len() as i64;
                                    self.frames[nf].slots[recv_idx] = Value::Int(n);
                                    continue;
                                } else if mname == "has" && argc == 1 {
                                    let key = self.frames[nf].slots.pop().unwrap();
                                    if let Ok(mk) = crate::value::as_map_key(&key) {
                                        let b = if let Value::Map(m) =
                                            &self.frames[nf].slots[recv_idx]
                                        {
                                            m.lock().unwrap().find(&mk).is_some()
                                        } else {
                                            false
                                        };
                                        self.frames[nf].slots[recv_idx] = Value::Bool(b);
                                        continue;
                                    } else {
                                        self.frames[nf].slots.push(key);
                                    }
                                } else if !collections::seq::is_builtin_map_method(mname) {
                                    let key = crate::value::MapKey::Str(name.clone());
                                    let found =
                                        if let Value::Map(m) = &self.frames[nf].slots[recv_idx] {
                                            let guard = m.lock().unwrap();
                                            guard.find(&key).map(|i| guard.entries[i].1.clone())
                                        } else {
                                            None
                                        };
                                    match found {
                                        Some(callee) => {
                                            let mut call_args = self.alloc_slots(argc);
                                            call_args.extend(
                                                self.frames[nf].slots.drain((recv_idx + 1)..),
                                            );
                                            self.frames[nf].slots.pop();
                                            match callee {
                                                Value::VFunc(fd) => {
                                                    if let Err(e) =
                                                        self.call_vfunc_frame(fd, call_args, None)
                                                    {
                                                        self.error = Some(e);
                                                        continue;
                                                    }
                                                }
                                                Value::Native(n) => {
                                                    let ctx = NativeCtx {
                                                        sched: self.sched.clone(),
                                                    };
                                                    match (n.func)(&ctx, call_args) {
                                                        Ok(v) => self.frames[nf].slots.push(v),
                                                        Err(msg) => self.fail(msg),
                                                    }
                                                }
                                                Value::NativeClosure(n) => {
                                                    let ctx = NativeCtx {
                                                        sched: self.sched.clone(),
                                                    };
                                                    match (n.func)(&ctx, call_args) {
                                                        Ok(v) => self.frames[nf].slots.push(v),
                                                        Err(msg) => self.fail(msg),
                                                    }
                                                }
                                                Value::VClass(c) => {
                                                    if let Err(e) =
                                                        self.new_instance_vm(c, call_args)
                                                    {
                                                        self.error = Some(e);
                                                        continue;
                                                    }
                                                }
                                                Value::Class(cd) => {
                                                    let vc = tree_class_to_vclass(&cd);
                                                    if let Err(e) =
                                                        self.new_instance_vm(vc, call_args)
                                                    {
                                                        self.error = Some(e);
                                                        continue;
                                                    }
                                                }
                                                other => {
                                                    self.fail(format!(
                                                        "'{}' is not callable (found {})",
                                                        name,
                                                        type_name(&other)
                                                    ));
                                                }
                                            }
                                            continue;
                                        }
                                        None => {
                                            self.frames[nf].slots.truncate(recv_idx);
                                            self.fail(format!(
                                                "map has no method or function '{}'",
                                                name
                                            ));
                                            continue;
                                        }
                                    }
                                }
                            }
                            Value::Str(s) if name.as_ref() == "len" && argc == 0 => {
                                let n = s.chars().count() as i64;
                                self.frames[nf].slots[recv_idx] = Value::Int(n);
                                continue;
                            }
                            Value::VInstance(inst) => {
                                let class = inst.lock().unwrap().class.clone();
                                let chunk_ptr = Arc::as_ptr(&self.frames[nf].func.chunk) as usize;
                                let key = (chunk_ptr, ip);
                                let class_id = Arc::as_ptr(&class) as usize;
                                let hit = match self.invoke_cache.get(&key) {
                                    Some(c) if c.class == class_id => Some(c.func.clone()),
                                    _ => None,
                                };
                                let resolved = match hit {
                                    Some(fd) => Some(fd),
                                    None => match find_method_vclass(&class, &name) {
                                        Some(fd) => {
                                            if self.invoke_cache.len() > 4096 {
                                                self.invoke_cache.clear();
                                            }
                                            let chunk = self.frames[nf].func.chunk.clone();
                                            self.invoke_cache.insert(
                                                key,
                                                InvokeCache {
                                                    _chunk: chunk,
                                                    class: class_id,
                                                    func: fd.clone(),
                                                },
                                            );
                                            Some(fd)
                                        }
                                        None => None,
                                    },
                                };
                                match resolved {
                                    Some(fd) => {
                                        if argc + 1 < fd.min_arity || argc + 1 > fd.arity {
                                            self.frames[nf].slots.truncate(recv_idx);
                                            let want = if fd.min_arity == fd.arity {
                                                format!("{} argument(s)", fd.arity - 1)
                                            } else {
                                                format!(
                                                    "{} to {} argument(s)",
                                                    fd.min_arity - 1,
                                                    fd.arity - 1
                                                )
                                            };
                                            self.fail(format!(
                                                "method {} expects {}, got {}",
                                                name, want, argc
                                            ));
                                            continue;
                                        }
                                        let mut call_args = self.alloc_slots(argc + 1);
                                        call_args.extend(self.frames[nf].slots.drain(recv_idx..));
                                        // Stack traces name the frame
                                        // `Class::method`, like the tree engine.
                                        let display = Some(format!("{}::{}", class.name, name));
                                        if let Err(e) =
                                            self.call_vfunc_frame(fd, call_args, display)
                                        {
                                            self.error = Some(e);
                                            continue;
                                        }
                                    }
                                    None => {
                                        self.frames[nf].slots.truncate(recv_idx);
                                        self.fail(format!(
                                            "class {} has no method '{}'",
                                            class.name, name
                                        ));
                                    }
                                }
                                continue;
                            }
                            _ => {}
                        }
                    }
                    let args = self.pop_n(argc);
                    let obj = self.pop_top();
                    if collections::is_callback_method(&name) {
                        match &obj {
                            Value::List(d) | Value::VecDyn(d) => {
                                let items = d.lock().unwrap().items.clone();
                                match self.list_callback_vm(&obj, &items, &name, args) {
                                    Ok(v) => self.frames[nf].slots.push(v),
                                    Err(e) => self.error = Some(e),
                                }
                                continue;
                            }
                            // `option.map(f)` / `result.and_then(f)` take a
                            // callback too. Without this arm the VM fell back to
                            // the no-callbacks dispatcher and reported
                            // "higher-order methods … not supported in this
                            // context" while the tree engine ran it fine.
                            Value::Option(_) | Value::Result(_) => {
                                match self.host_callback_method(&obj, &name, args) {
                                    Ok(v) => self.frames[nf].slots.push(v),
                                    Err(e) => self.error = Some(e),
                                }
                                continue;
                            }
                            _ => {}
                        }
                    }
                    match collections::call_method(&obj, &name, args) {
                        Ok(v) => self.frames[nf].slots.push(v),
                        Err(msg) => {
                            // Callbacks and `task.join()` stash the thrown
                            // user value behind their String error; rebind it
                            // here so `catch` sees the original value
                            // (audit A-9).
                            let mut err = VmError {
                                thrown: None,
                                message: msg,
                                span: self.cur_span(),
                                trace: Vec::new(),
                            };
                            if let Some(v) = crate::sched::take_error_thrown() {
                                err.thrown = Some(v);
                            }
                            self.error = Some(err);
                        }
                    }
                }
                OpCode::IndexGet => {
                    self.frames[nf].ip = ip + 1;
                    let idx = self.pop_top();
                    let obj = self.pop_top();
                    match collections::index_get(&obj, &idx) {
                        Ok(v) => self.frames[nf].slots.push(v),
                        Err(msg) => self.fail(msg),
                    }
                }
                OpCode::IndexSet => {
                    self.frames[nf].ip = ip + 1;
                    let v = self.pop_top();
                    let idx = self.pop_top();
                    let obj = self.pop_top();
                    match collections::index_set(&obj, &idx, v.clone()) {
                        // Leave the assigned value on the stack, matching
                        // `SetLocal`/`SetGlobal` (see the FieldSet note).
                        Ok(()) => self.frames[nf].slots.push(v),
                        Err(msg) => self.fail(msg),
                    }
                }
                OpCode::FieldGet => {
                    let idx = self.code_u16(nf, ip + 1) as usize;
                    self.frames[nf].ip = ip + 3;
                    let obj = self.pop_top();
                    let name = match self.frames[nf].func.chunk.constants.get(idx) {
                        Some(Value::Str(s)) => s.clone(),
                        _ => {
                            self.fail("field name is not a string");
                            continue;
                        }
                    };
                    match obj {
                        Value::VInstance(inst) => {
                            let ib = inst.lock().unwrap();
                            match ib.fields.get(name.as_ref()) {
                                Some(v) => {
                                    let val = v.clone();
                                    drop(ib);
                                    self.frames[nf].slots.push(val);
                                }
                                None => {
                                    let err = format!(
                                        "instance of {} has no field '{}'",
                                        ib.class.name, name
                                    );
                                    drop(ib);
                                    self.fail(err);
                                }
                            }
                        }
                        Value::Map(m) => {
                            let mb = m.lock().unwrap();
                            let mk = crate::value::MapKey::Str(name.clone());
                            match mb.find(&mk) {
                                Some(i) => {
                                    let val = mb.entries[i].1.clone();
                                    drop(mb);
                                    self.frames[nf].slots.push(val);
                                }
                                None => {
                                    drop(mb);
                                    self.fail(format!("missing key '{}'", name));
                                }
                            }
                        }
                        other => self.fail(crate::value::no_field_error(&other, name.as_ref())),
                    }
                }
                OpCode::FieldSet => {
                    let idx = self.code_u16(nf, ip + 1) as usize;
                    self.frames[nf].ip = ip + 3;
                    let v = self.pop_top();
                    let obj = self.pop_top();
                    // Read the field name up front: the diagnostic for a
                    // non-instance target needs it too.
                    let field_name = match self.frames[nf].func.chunk.constants.get(idx) {
                        Some(Value::Str(s)) => s.clone(),
                        _ => {
                            self.fail("field name is not a string");
                            continue;
                        }
                    };
                    match obj {
                        Value::VInstance(inst) => {
                            let name = field_name;
                            let mut ib = inst.lock().unwrap();
                            match ib.fields.get_mut(name.as_ref()) {
                                Some(slot) => {
                                    *slot = v.clone();
                                    drop(ib);
                                    self.frames[nf].slots.push(v);
                                }
                                None => {
                                    let err = format!(
                                        "instance of {} has no field '{}'",
                                        ib.class.name, name
                                    );
                                    drop(ib);
                                    self.fail(err);
                                }
                            }
                        }
                        other => {
                            self.fail(crate::value::no_field_error(&other, field_name.as_ref()))
                        }
                    }
                }
                OpCode::Construct => {
                    let idx = self.code_u16(nf, ip + 1) as usize;
                    let argc = self.code_u8(nf, ip + 3) as usize;
                    self.frames[nf].ip = ip + 4;
                    let name = match self.const_at(nf, idx) {
                        Value::Str(s) => s,
                        _ => {
                            self.fail("constructor name is not a string");
                            continue;
                        }
                    };
                    let args = self.pop_n(argc);
                    match collections::construct(&name, args) {
                        Ok(v) => self.frames[nf].slots.push(v),
                        Err(msg) => self.fail(msg),
                    }
                }
                OpCode::Return => {
                    let ret = self.pop_top();
                    if self.error.is_some() {
                        // pop_top underflowed on a malformed (crafted)
                        // chunk: route through the normal error funnel at
                        // the top of the run loop instead of falling
                        // through to `return Ok`, which would swallow the
                        // failure.
                        continue;
                    }
                    let nf = self.frames.len() - 1;
                    if self
                        .pending_str
                        .as_ref()
                        .is_some_and(|&(pf, _, _)| pf == nf)
                    {
                        self.pending_str = None;
                    }
                    // If returning from inside a `finally` block, pop that handler
                    // and cancel any pending re-raise from its `try`/`catch`.
                    while self.handlers.last().is_some_and(|h| {
                        h.frame_depth == self.frames.len() && h.phase == HandlerPhase::Finally
                    }) {
                        self.handlers.pop();
                        self.reraise = None;
                    }
                    // If still inside a `try` or `catch` in this frame, run its
                    // `finally` block first and defer the return until `EndTry`.
                    if let Some(h) = self
                        .handlers
                        .last_mut()
                        .filter(|h| h.frame_depth == self.frames.len())
                    {
                        h.phase = HandlerPhase::Finally;
                        h.pending_return = Some(ret);
                        let stack_len = h.stack_len;
                        let finally_target = h.finally_target;
                        self.frames[nf].slots.truncate(stack_len);
                        self.frames[nf].ip = finally_target;
                        continue;
                    }
                    let ctor = self.frames[nf].ctor_result.take();
                    let ret_or_ctor = ctor.unwrap_or(ret);
                    if !self.frames[nf].guards.is_empty() {
                        // Dropping the tokens unlocks the mutexes (and wakes
                        // any waiter).
                        let _ = std::mem::take(&mut self.frames[nf].guards);
                    }
                    if let Some(popped_frame) = self.frames.pop() {
                        self.free_slots(popped_frame.slots);
                    }
                    self.depth = self.depth.saturating_sub(1);
                    if self.frames.len() == self.stop {
                        return Ok(ret_or_ctor);
                    }
                    self.frames.last_mut().unwrap().slots.push(ret_or_ctor);
                }
                OpCode::Try => {
                    let has_catch = self.code_u8(nf, ip + 1) != 0;
                    let catch_slot = self.code_u8(nf, ip + 2);
                    let catch_off = self.code_i16(nf, ip + 3);
                    let finally_off = self.code_i16(nf, ip + 5);
                    let operand_end = ip + 7;
                    self.frames[nf].ip = operand_end;
                    let catch_target = (operand_end as i64 + catch_off as i64) as usize;
                    let finally_target = (operand_end as i64 + finally_off as i64) as usize;
                    self.handlers.push(Handler {
                        has_catch,
                        catch_slot,
                        catch_target,
                        finally_target,
                        stack_len: self.frames[nf].slots.len(),
                        frame_depth: self.frames.len(),
                        phase: HandlerPhase::Try,
                        pending_return: None,
                    });
                }
                OpCode::EnterFinally => {
                    self.frames[nf].ip = ip + 1;
                    if let Some(h) = self.handlers.last_mut() {
                        h.phase = HandlerPhase::Finally;
                    }
                }
                OpCode::Dup => {
                    self.frames[nf].ip = ip + 1;
                    let v = self.pop_top();
                    self.frames[nf].slots.push(v.clone());
                    self.frames[nf].slots.push(v);
                }
                OpCode::Dup2 => {
                    self.frames[nf].ip = ip + 1;
                    let b = self.pop_top();
                    let a = self.pop_top();
                    self.frames[nf].slots.push(a.clone());
                    self.frames[nf].slots.push(b.clone());
                    self.frames[nf].slots.push(a);
                    self.frames[nf].slots.push(b);
                }
                OpCode::CaptureSync => {
                    self.flush_pending_str();
                    let slot = self.code_u8(nf, ip + 1) as usize;
                    let idx = self.code_u8(nf, ip + 2) as usize;
                    self.frames[nf].ip = ip + 3;
                    if self.frames[nf]
                        .captured
                        .get(slot)
                        .and_then(|c| c.as_ref())
                        .is_some()
                    {
                        continue;
                    }
                    let cell = crate::value::new_untracked_ref(
                        self.frames[nf]
                            .slots
                            .get(slot)
                            .cloned()
                            .unwrap_or(Value::Null),
                    );
                    if idx < self.frames[nf].own_cells.len() {
                        self.frames[nf].own_cells[idx] = cell.clone();
                    }
                    let cap = &mut self.frames[nf].captured;
                    if slot >= cap.len() {
                        cap.resize(slot + 1, None);
                    }
                    cap[slot] = Some(cell);
                }
                OpCode::Uncapture => {
                    let slot = self.code_u8(nf, ip + 1) as usize;
                    self.frames[nf].ip = ip + 2;
                    if let Some(entry) = self.frames[nf].captured.get_mut(slot) {
                        *entry = None;
                    }
                }
                OpCode::EndTry => {
                    self.frames[nf].ip = ip + 1;
                    let popped = self.handlers.pop();
                    if let Some(re) = self.reraise.take() {
                        self.error = Some(re);
                    } else if let Some(ret) = popped.and_then(|h| h.pending_return) {
                        if let Some(outer) = self
                            .handlers
                            .last_mut()
                            .filter(|h| h.frame_depth == self.frames.len())
                        {
                            outer.phase = HandlerPhase::Finally;
                            outer.pending_return = Some(ret);
                            let stack_len = outer.stack_len;
                            let finally_target = outer.finally_target;
                            self.frames[nf].slots.truncate(stack_len);
                            self.frames[nf].ip = finally_target;
                            continue;
                        }
                        let ctor = self.frames[nf].ctor_result.take();
                        let ret_or_ctor = ctor.unwrap_or(ret);
                        if !self.frames[nf].guards.is_empty() {
                            // Dropping the tokens unlocks the mutexes.
                            let _ = std::mem::take(&mut self.frames[nf].guards);
                        }
                        if let Some(popped_frame) = self.frames.pop() {
                            self.free_slots(popped_frame.slots);
                        }
                        self.depth = self.depth.saturating_sub(1);
                        if self.frames.len() == self.stop {
                            return Ok(ret_or_ctor);
                        }
                        self.frames.last_mut().unwrap().slots.push(ret_or_ctor);
                    }
                }
                OpCode::Throw => {
                    self.frames[nf].ip = ip + 1;
                    let v = self.pop_top();
                    self.error = Some(VmError {
                        thrown: Some(v),
                        message: "uncaught throw".into(),
                        span: self.cur_span(),
                        trace: Vec::new(),
                    });
                }
                OpCode::Await => {
                    self.frames[nf].ip = ip + 1;
                    let v = self.pop_top();
                    match v {
                        Value::Task(t) => match t.join() {
                            Ok(val) => self.frames[nf].slots.push(val),
                            Err(msg) => {
                                // A task that threw a user value rethrows it
                                // structurally so `catch` rebinds the original
                                // value, not a string (audit A-9).
                                let mut err = VmError {
                                    thrown: None,
                                    message: format!("task failed: {}", msg),
                                    span: self.cur_span(),
                                    trace: Vec::new(),
                                };
                                if let Some(v) = t.thrown_value() {
                                    err.thrown = Some(v);
                                }
                                self.error = Some(err);
                            }
                        },
                        Value::Channel(c) => {
                            let val = c.recv();
                            self.frames[nf].slots.push(val);
                        }
                        other => self.fail(format!(
                            "await expects a task or a channel, found {}",
                            type_name(&other)
                        )),
                    }
                }
                OpCode::Spawn => {
                    self.frames[nf].ip = ip + 1;
                    let callable = self.pop_top();
                    let fd = match callable {
                        Value::VFunc(f) => f,
                        other => {
                            self.fail(format!(
                                "spawn expects a function, found {}",
                                type_name(&other)
                            ));
                            continue;
                        }
                    };
                    let globals = self.globals.clone();
                    let sched = self.sched.clone();
                    let modules = self.modules.clone();
                    let embedded = self.embedded.clone();
                    let file = self.file.clone();
                    let module_dirs = self.module_dirs.clone();
                    let global_consts = self.global_consts.clone();
                    let consts = self.consts.clone();
                    let handle = self.sched.spawn_job(Box::new(move || {
                        let mut vm = Vm::with_shared(
                            globals,
                            global_consts,
                            consts,
                            sched,
                            modules,
                            embedded,
                            file,
                            module_dirs,
                        );
                        // Carry the thrown value across the task boundary so
                        // `join`/`await` rethrow is structural: `catch` on the
                        // joining side rebinds the original value (audit A-9).
                        match vm.run_function(fd, Vec::new()) {
                            Ok(v) => crate::sched::TaskOutcome {
                                result: Ok(v),
                                thrown: None,
                            },
                            Err(e) => crate::sched::TaskOutcome {
                                thrown: e.thrown.clone(),
                                result: Err(e.task_error_string()),
                            },
                        }
                    }));
                    self.frames[nf].slots.push(Value::Task(handle));
                }
                OpCode::Import => {
                    let idx = self.code_u16(nf, ip + 1) as usize;
                    self.frames[nf].ip = ip + 3;
                    let path = match self.const_at(nf, idx) {
                        Value::Str(s) => s,
                        _ => {
                            self.fail("import path is not a string");
                            continue;
                        }
                    };
                    match self.import_module(&path) {
                        Ok(exports) => {
                            // Record which exports are `public const` before
                            // binding them: global assignment consults this
                            // set (mirrors the tree's `define_const`).
                            {
                                let mut consts = self.global_consts.lock().unwrap();
                                for (n, _v, is_const) in &exports {
                                    if *is_const {
                                        consts.insert(n.clone());
                                    }
                                }
                            }
                            let frame_globals = self.frames[nf].globals.clone();
                            let mut g = frame_globals.lock().unwrap();
                            for (n, v, _is_const) in exports {
                                g.insert(n, v);
                            }
                            self.frames[nf].slots.push(Value::Null);
                        }
                        Err(e) => {
                            self.error = Some(e);
                            continue;
                        }
                    }
                }
                OpCode::ImportAs => {
                    let path_idx = self.code_u16(nf, ip + 1) as usize;
                    let alias_idx = self.code_u16(nf, ip + 3) as usize;
                    self.frames[nf].ip = ip + 5;
                    let path = match self.const_at(nf, path_idx) {
                        Value::Str(s) => s,
                        _ => {
                            self.fail("import path is not a string");
                            continue;
                        }
                    };
                    let alias = match self.const_at(nf, alias_idx) {
                        Value::Str(s) => s,
                        _ => {
                            self.fail("import alias is not a string");
                            continue;
                        }
                    };
                    match self.import_module(&path) {
                        Ok(exports) => {
                            let mut entries = Vec::new();
                            let mut frozen = std::collections::HashSet::new();
                            for (name, value, is_const) in exports {
                                let key = crate::value::MapKey::Str(name.into());
                                if is_const {
                                    // A `public const` export stays immutable
                                    // through the namespace as well.
                                    frozen.insert(key.clone());
                                }
                                entries.push((key, value));
                            }
                            // The alias is a **module namespace**: a read-only
                            // view of the module's exports (see the tree
                            // engine's import path).
                            let map_val = Value::Map(new_ref(collections::MapData::namespace(
                                entries, frozen,
                            )));
                            {
                                let mut consts = self.global_consts.lock().unwrap();
                                consts.insert(alias.to_string());
                            }
                            let frame_globals = self.frames[nf].globals.clone();
                            let mut g = frame_globals.lock().unwrap();
                            g.insert(alias.to_string(), map_val);
                            self.frames[nf].slots.push(Value::Null);
                        }
                        Err(e) => {
                            self.error = Some(e);
                            continue;
                        }
                    }
                }
                OpCode::Truthy => {
                    self.frames[nf].ip = ip + 1;
                    let v = self.pop_top();
                    self.frames[nf].slots.push(Value::Bool(truthy(&v)));
                }
                OpCode::ForEachInit => {
                    self.frames[nf].ip = ip + 1;
                    let v = self.pop_top();
                    match collections::iter_values(&v) {
                        Ok(items) => self.frames[nf].slots.push(Value::Tuple(new_ref(items))),
                        Err(msg) => self.fail(msg),
                    }
                }
                OpCode::Len => {
                    self.frames[nf].ip = ip + 1;
                    let v = self.pop_top();
                    match collections::len_of(&v) {
                        Ok(n) => self.frames[nf].slots.push(Value::Int(n as i64)),
                        Err(msg) => self.fail(msg),
                    }
                }
            }
        }
    }

    // --------------------------------------------------------- frame helpers

    /// Snapshot the call stack for an error trace, most recent frame first.
    /// Each frame contributes its function name and the span of the
    /// instruction it is currently at (`inst_ip`).
    fn capture_trace(&self) -> Vec<crate::diagnostics::TraceFrame> {
        let mut out = Vec::with_capacity(self.frames.len());
        for f in self.frames.iter().rev() {
            // `span_at` maps a byte offset to the span of the instruction that
            // contains it (inst_spans is indexed by instruction, not by byte).
            let span = f.func.chunk.span_at(f.inst_ip);
            let name = f.display.clone().or_else(|| f.func.name.clone());
            if let Some(last) = out.last() {
                let last: &crate::diagnostics::TraceFrame = last;
                if last.func == name && last.span.line == span.line && last.span.file == span.file {
                    continue;
                }
            }
            out.push(crate::diagnostics::TraceFrame { func: name, span });
        }
        out
    }

    #[inline]
    fn alloc_slots(&mut self, cap: usize) -> Vec<Value> {
        match self.slot_pool.pop() {
            Some(mut v) => {
                v.clear();
                v.reserve(cap);
                v
            }
            None => Vec::with_capacity(cap.max(8)),
        }
    }

    #[inline]
    fn free_slots(&mut self, mut v: Vec<Value>) {
        if self.slot_pool.len() < 64 && v.capacity() <= 256 {
            v.clear();
            self.slot_pool.push(v);
        }
    }

    #[inline]
    fn flush_pending_str(&mut self) {
        if let Some((fidx, slot, buf)) = self.pending_str.take() {
            if let Some(f) = self.frames.get_mut(fidx) {
                let val = Value::Str(buf.into());
                if let Some(Some(c)) = f.captured.get(slot) {
                    *c.lock().unwrap() = val.clone();
                }
                if slot < f.slots.len() {
                    f.slots[slot] = val;
                }
            }
        }
    }

    fn pop_top(&mut self) -> Value {
        let nf = self.frames.len() - 1;
        match self.frames[nf].slots.pop() {
            Some(v) => v,
            None => {
                self.fail("stack underflow: not enough operands on the stack");
                Value::Null
            }
        }
    }

    fn pop_n(&mut self, n: usize) -> Vec<Value> {
        let nf = self.frames.len() - 1;
        let len = self.frames[nf].slots.len();
        if len < n {
            self.fail("stack underflow: not enough operands on the stack");
            return vec![Value::Null; n];
        }
        let mut v = self.alloc_slots(n + 1);
        v.extend(self.frames[nf].slots.drain((len - n)..));
        v
    }

    fn call_vfunc_frame(
        &mut self,
        fd: Arc<VmFunc>,
        mut args: Vec<Value>,
        display: Option<String>,
    ) -> Result<(), VmError> {
        if args.len() < fd.min_arity || args.len() > fd.arity {
            let want = if fd.min_arity == fd.arity {
                format!("{} argument(s)", fd.arity)
            } else {
                format!("{} to {} argument(s)", fd.min_arity, fd.arity)
            };
            return Err(VmError {
                thrown: None,
                message: format!(
                    "{} expects {}, got {}",
                    fd.name.as_deref().unwrap_or("fn"),
                    want,
                    args.len()
                ),
                span: self.cur_span(),
                trace: Vec::new(),
            });
        }
        // Fill omitted trailing parameters from the baked constant defaults.
        // Every caller routes through here, so short calls never reach the
        // frame with uninitialized slots.
        let missing = fd.arity - args.len();
        if missing > 0 {
            let start = fd.defaults.len() - missing;
            for v in &fd.defaults[start..] {
                args.push(v.clone());
            }
        }
        if self.depth >= MAX_CALL_DEPTH {
            return Err(VmError {
                thrown: None,
                message: "stack overflow: call depth limit reached".into(),
                span: self.cur_span(),
                trace: Vec::new(),
            });
        }
        self.depth += 1;
        let own_cells = if fd.own_upvalues == 0 {
            Vec::new()
        } else {
            (0..fd.own_upvalues as usize)
                .map(|_| crate::value::new_untracked_ref(Value::Null))
                .collect()
        };
        let frame_globals = fd.globals.clone().unwrap_or_else(|| self.globals.clone());
        self.frames.push(CallFrame {
            func: fd,
            globals: frame_globals,
            display,
            ip: 0,
            inst_ip: 0,
            slots: args,
            captured: Vec::new(),
            own_cells,
            guards: Vec::new(),
            ctor_result: None,
        });
        Ok(())
    }

    /// Build a runtime `VmFunc` (with upvalue cells) from a prototype. Each of
    /// the prototype's upvalue slots is an *index into the enclosing frame's own
    /// upvalue list* (clox style): the cell lives in `parent.func.upvalues`, so a
    /// nested closure shares the exact cell the defining frame uses. This keeps
    /// closures correct across arbitrary nesting and after the defining frame
    /// has returned (the `Arc` keeps the cell alive).
    fn make_closure(&mut self, fproto: std::sync::Arc<VmFuncProto>, nf: usize) -> Arc<VmFunc> {
        let mut upvalues: Vec<UpvalueCell> = Vec::with_capacity(fproto.upvalues.len());
        // Bit 7 == 0 (`0x00..=0x7f`) addresses the parent frame's per-activation
        // cell for its own captured local (`own_cells[slot]`); bit 7 == 1
        // (`0x80..=0xff`) addresses the parent function's enclosing upvalues
        // (`func.upvalues[slot]`).
        for &eslot in &fproto.upvalues {
            let is_enclosing = (eslot & 0x80) != 0;
            let slot = (eslot & 0x7f) as usize;
            let cell = if !is_enclosing {
                match self.frames[nf].own_cells.get(slot) {
                    Some(c) => c.clone(),
                    None => {
                        let val = self
                            .frames
                            .get(nf)
                            .and_then(|f| f.slots.get(slot).cloned())
                            .unwrap_or(Value::Null);
                        crate::value::new_untracked_ref(val)
                    }
                }
            } else {
                match self.frames[nf].func.upvalues.get(slot) {
                    Some(c) => c.clone(),
                    None => {
                        let val = self
                            .frames
                            .get(nf)
                            .and_then(|f| f.slots.get(slot).cloned())
                            .unwrap_or(Value::Null);
                        crate::value::new_untracked_ref(val)
                    }
                }
            };
            upvalues.push(cell);
        }
        // Capture the globals of the frame doing the defining, not the VM's
        // field: a module's function creating a closure must give that
        // closure the module's scope (the VM field holds the *entry*
        // program's map once a module function runs under another VM).
        let defining_globals = self
            .frames
            .last()
            .map(|f| f.globals.clone())
            .unwrap_or_else(|| self.globals.clone());
        Arc::new(VmFunc {
            globals: Some(defining_globals),
            name: fproto.name.clone(),
            arity: fproto.arity,
            min_arity: fproto.min_arity,
            defaults: fproto.defaults.clone(),
            is_async: fproto.is_async,
            // Cheap Arc clone: every closure from the same prototype shares
            // one immutable bytecode chunk.
            chunk: fproto.chunk.clone(),
            upvalues,
            own_upvalues: fproto.own_upvalues,
            debug_locals: fproto.debug_locals.clone(),
        })
    }

    /// Build a `VmClass` from its prototype, resolving the parent by name from
    /// globals and lifting method prototypes into `VmFunc` values.
    fn build_class(&mut self, cproto: VmClassProto, nf: usize) -> Result<Arc<VmClass>, VmError> {
        let parent = match &cproto.parent {
            Some(p) => {
                let g = self.globals.lock().unwrap();
                match g.get(p) {
                    Some(Value::VClass(c)) => Some(c.clone()),
                    Some(Value::Class(cd)) => Some(tree_class_to_vclass(cd)),
                    _ => {
                        return Err(VmError {
                            thrown: None,
                            message: format!("unknown class '{}'", p),
                            span: self.cur_span(),
                            trace: Vec::new(),
                        })
                    }
                }
            }
            None => None,
        };
        let mut methods: HashMap<String, (crate::ast::Vis, Arc<VmFunc>)> = HashMap::new();
        let mut ctor = None;
        for (name, vis, fidx, is_ctor) in &cproto.methods {
            let fproto = self.func_proto_at(nf, *fidx as usize);
            let fd = self.make_closure(fproto, nf);
            methods.insert(name.clone(), (*vis, fd.clone()));
            if *is_ctor {
                ctor = Some(fd);
            }
        }
        Ok(Arc::new(VmClass {
            name: cproto.name,
            parent,
            fields: cproto.fields,
            methods,
            ctor,
        }))
    }

    /// Create an instance and run its constructor (if any). The constructed
    /// instance is placed on the caller's stack by `OpReturn` of the ctor
    /// frame.
    fn new_instance_vm(&mut self, class: Arc<VmClass>, args: Vec<Value>) -> Result<(), VmError> {
        let mut fields: HashMap<String, Value> = HashMap::new();
        let mut cur = Some(class.clone());
        let mut chain: Vec<Arc<VmClass>> = Vec::new();
        while let Some(c) = cur {
            cur = c.parent.clone();
            chain.push(c);
        }
        for c in chain.iter().rev() {
            for (fname, _) in &c.fields {
                fields.insert(fname.clone(), Value::Null);
            }
        }
        let instance = VmInstanceData {
            class: class.clone(),
            fields,
        };
        let obj = Value::VInstance(new_ref(instance));
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
        match ctor {
            Some(ctor) => {
                if args.len() + 1 < ctor.min_arity || args.len() + 1 > ctor.arity {
                    let want = if ctor.min_arity == ctor.arity {
                        format!("{} argument(s)", ctor.arity - 1)
                    } else {
                        format!("{} to {} argument(s)", ctor.min_arity - 1, ctor.arity - 1)
                    };
                    return Err(VmError {
                        thrown: None,
                        message: format!("new {} expects {}, got {}", class.name, want, args.len()),
                        span: self.cur_span(),
                        trace: Vec::new(),
                    });
                }
                let mut call_args = self.alloc_slots(args.len() + 1);
                call_args.push(obj.clone());
                call_args.extend(args);
                // Constructor frames read `Class::new` in stack traces.
                let display = Some(format!("{}::new", class.name));
                self.call_vfunc_frame(ctor, call_args, display)?;
                let nf = self.frames.len() - 1;
                self.frames[nf].ctor_result = Some(obj);
            }
            None => {
                if !args.is_empty() {
                    return Err(VmError {
                        thrown: None,
                        message: format!(
                            "new {} expects 0 argument(s), got {}",
                            class.name,
                            args.len()
                        ),
                        span: self.cur_span(),
                        trace: Vec::new(),
                    });
                }
                let nf = self.frames.len() - 1;
                self.frames[nf].slots.push(obj);
            }
        }
        Ok(())
    }

    // ------------------------------------------------------- exception unwind

    fn unwind(&mut self, err: VmError) -> Result<(), VmError> {
        self.flush_pending_str();
        while let Some(mut h) = self.handlers.pop() {
            // `stop` bounds this synchronous run: frames at index >= `stop` and
            // handlers with `frame_depth > stop` belong to it, everything below
            // belongs to the OUTER run that called `call_value_sync` (for
            // example the `try` around a `list.sort_by(cmp)` when `cmp`
            // throws). Consuming an outer handler here would catch the error in
            // a frame whose nested frames are already gone, leaving the frame
            // stack below `stop` — and the next `Return` then popped past the
            // boundary and panicked on an empty stack (Invariant #3).
            if h.frame_depth <= self.stop {
                self.handlers.push(h);
                // Tear this run's frames down to the boundary so the outer run
                // resumes on a clean stack.
                while self.frames.len() > self.stop {
                    let nf = self.frames.len() - 1;
                    if !self.frames[nf].guards.is_empty() {
                        // Dropping the tokens unlocks the mutexes (and wakes
                        // any waiter).
                        let _ = std::mem::take(&mut self.frames[nf].guards);
                    }
                    if let Some(popped_frame) = self.frames.pop() {
                        self.free_slots(popped_frame.slots);
                    }
                    self.depth = self.depth.saturating_sub(1);
                }
                self.error = Some(err.clone());
                return Err(err);
            }
            while self.frames.len() > h.frame_depth {
                let nf = self.frames.len() - 1;
                if !self.frames[nf].guards.is_empty() {
                    // Dropping the tokens unlocks the mutexes.
                    let _ = std::mem::take(&mut self.frames[nf].guards);
                }
                if let Some(popped_frame) = self.frames.pop() {
                    self.free_slots(popped_frame.slots);
                }
                self.depth = self.depth.saturating_sub(1);
            }
            {
                let top = self.frames.last_mut().unwrap();
                top.slots.truncate(h.stack_len);
            }
            let thrown = err
                .thrown
                .clone()
                .unwrap_or_else(|| Value::Str(err.message.clone().into()));
            match h.phase {
                HandlerPhase::Try => {
                    if h.has_catch {
                        let catch_slot = h.catch_slot;
                        let catch_target = h.catch_target;
                        h.phase = HandlerPhase::Catch;
                        self.handlers.push(h);
                        let top = self.frames.last_mut().unwrap();
                        if (catch_slot as usize) >= top.slots.len() {
                            top.slots.resize(catch_slot as usize + 1, Value::Null);
                        }
                        top.slots[catch_slot as usize] = thrown;
                        top.ip = catch_target;
                        return Ok(());
                    } else {
                        let finally_target = h.finally_target;
                        h.phase = HandlerPhase::Finally;
                        self.handlers.push(h);
                        let top = self.frames.last_mut().unwrap();
                        top.ip = finally_target;
                        self.reraise = Some(VmError {
                            thrown: err.thrown.clone(),
                            message: err.message.clone(),
                            span: err.span.clone(),
                            trace: Vec::new(),
                        });
                        return Ok(());
                    }
                }
                HandlerPhase::Catch => {
                    let finally_target = h.finally_target;
                    h.phase = HandlerPhase::Finally;
                    self.handlers.push(h);
                    let top = self.frames.last_mut().unwrap();
                    top.ip = finally_target;
                    self.reraise = Some(VmError {
                        thrown: err.thrown.clone(),
                        message: err.message.clone(),
                        span: err.span.clone(),
                        trace: Vec::new(),
                    });
                    return Ok(());
                }
                HandlerPhase::Finally => {
                    self.reraise = Some(VmError {
                        thrown: err.thrown.clone(),
                        message: err.message.clone(),
                        span: err.span.clone(),
                        trace: Vec::new(),
                    });
                    continue;
                }
            }
        }
        self.error = Some(err.clone());
        Err(err)
    }

    // ------------------------------------------------------------- modules

    fn resolve_module_path(&self, path: &str) -> Result<PathBuf, VmError> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        let base = PathBuf::from(self.file.as_ref());
        if let Some(dir) = base.parent() {
            candidates.push(dir.join(path));
            candidates.push(dir.join(format!("{}.matrix", path)));
        }
        for d in &self.module_dirs {
            candidates.push(d.join(path));
            candidates.push(d.join(format!("{}.matrix", path)));
        }
        for c in candidates {
            if c.is_file() {
                // Canonicalize (matching the tree interpreter's module cache)
                // so the same file imported under different spellings is one
                // module instance, not two. Strip Windows' `\\?\` device
                // prefix for local drives so diagnostics stay readable.
                let canon = std::fs::canonicalize(&c).unwrap_or(c);
                let s = canon.to_string_lossy().to_string();
                if let Some(rest) = s.strip_prefix(r"\\?\") {
                    if rest.len() >= 2 && rest.as_bytes()[1] == b':' {
                        return Ok(PathBuf::from(rest));
                    }
                }
                return Ok(canon);
            }
        }
        Err(VmError {
            thrown: None,
            message: format!("module '{}' not found", path),
            span: self.cur_span(),
            trace: Vec::new(),
        })
    }

    fn import_module(&mut self, path: &str) -> Result<Vec<(String, Value, bool)>, VmError> {
        // 1. Self-contained path: the module was embedded into the `.matrixc`
        //    at compile time. Run its script in a child VM that shares our
        //    globals and the (flat, transitive) embedded table, so even nested
        //    imports resolve without any `.matrix` source on disk.
        {
            let emb = self.embedded.lock().unwrap();
            if let Some((func, public)) = emb.get(path) {
                let func = func.clone();
                let public = public.clone();
                drop(emb);
                // The embedded table is keyed by the resolved (canonical)
                // module path — the `Import` op now carries that same string —
                // so one module has exactly one entry and one execution. The
                // module cache consult/record below mirrors the source path.
                // When no source exists on disk (pure `.matrixc` exec), fall
                // back to a key derived from the import operand itself, which
                // is already spelling-independent.
                let resolved = self
                    .resolve_module_path(path)
                    .ok()
                    .unwrap_or_else(|| PathBuf::from(format!("<embedded:{}>", path)));
                // Claim the path atomically (cache check + in-progress): two
                // tasks importing the same new module load it exactly once.
                let claim = match crate::modules::claim_load(&self.modules, &resolved, path) {
                    crate::modules::Claim::AlreadyLoaded(exports) => {
                        return Ok(exports);
                    }
                    crate::modules::Claim::Cyclic(message) => {
                        return Err(VmError {
                            thrown: None,
                            message,
                            span: self.cur_span(),
                            trace: Vec::new(),
                        });
                    }
                    crate::modules::Claim::Acquired(claim) => claim,
                };
                // A module runs against its OWN globals map: sharing the
                // importer's map let one module's `mul` overwrite another's
                // (a same-named function in two modules resolved to whichever
                // loaded last). Functions created while the module runs
                // capture this map, so the module's internals stay isolated
                // and only its exports cross the boundary.
                let globals: Arc<Mutex<HashMap<String, Value>>> =
                    Arc::new(Mutex::new(HashMap::new()));
                let sched = self.sched.clone();
                let modules = self.modules.clone();
                let embedded = self.embedded.clone();
                let file = self.file.clone();
                let module_dirs = self.module_dirs.clone();
                let global_consts = self.global_consts.clone();
                let mut mvm = Vm::with_shared(
                    globals,
                    global_consts,
                    // A module VM keeps its own constant set: it reports
                    // exactly which of its exports are `public const`.
                    Arc::new(Mutex::new(HashSet::new())),
                    sched,
                    modules,
                    embedded,
                    file,
                    module_dirs,
                );
                // Seed the builtins into the module's own globals (see the
                // source path's note).
                mvm.install_stdlib();
                let out = mvm.run_module_from_func(func, public);
                match &out {
                    Ok(exports) => claim.complete(exports.clone()),
                    // The claim drops here: the path is released and any
                    // waiting task retries (and reproduces) the failure.
                    Err(_) => drop(claim),
                }
                return out;
            }
        }
        // 2. Fallback: resolve from source on disk (REPL, an older `.matrixc`
        //    that did not embed this import, or a transitive import that was not
        //    embedded for some reason).
        let resolved = self.resolve_module_path(path)?;
        // Claim the path atomically (see the embedded path above): one load
        // per module, concurrent importers wait, same-thread re-entry is the
        // circular-import error.
        let claim = match crate::modules::claim_load(&self.modules, &resolved, path) {
            crate::modules::Claim::AlreadyLoaded(exports) => {
                return Ok(exports);
            }
            crate::modules::Claim::Cyclic(message) => {
                return Err(VmError {
                    thrown: None,
                    message,
                    span: self.cur_span(),
                    trace: Vec::new(),
                });
            }
            crate::modules::Claim::Acquired(claim) => claim,
        };
        let src = crate::modules::read_module_source(&resolved).map_err(|e| VmError {
            thrown: None,
            message: format!("cannot import '{}': {}", path, e),
            span: self.cur_span(),
            trace: Vec::new(),
        })?;
        let file: Arc<str> = Arc::from(resolved.to_string_lossy().to_string());
        let program = parser::parse_file(&src, file.clone()).map_err(|d| VmError {
            thrown: None,
            message: d.message,
            span: d.span.unwrap_or_else(|| self.cur_span()),
            trace: Vec::new(),
        })?;
        semantic::check_file(&program, false, &resolved.to_string_lossy()).map_err(|d| {
            VmError {
                thrown: None,
                message: d.message,
                span: d.span.unwrap_or_else(|| self.cur_span()),
                trace: Vec::new(),
            }
        })?;
        let mut module_dirs = self.module_dirs.clone();
        if let Some(dir) = resolved.parent() {
            module_dirs.push(dir.to_path_buf());
        }
        let mut mvm = Vm::with_shared(
            // Own globals map: see the embedded path's isolation note.
            Arc::new(Mutex::new(HashMap::new())),
            self.global_consts.clone(),
            // A module VM keeps its own constant set (see the embedded path).
            Arc::new(Mutex::new(HashSet::new())),
            self.sched.clone(),
            self.modules.clone(),
            self.embedded.clone(),
            file,
            module_dirs,
        );
        // A module's globals start empty; the builtins must be seeded or the
        // module cannot even call `len` (they used to arrive via the shared
        // importer map).
        mvm.install_stdlib();
        let exports = mvm.run_module(&program).map_err(|d| VmError {
            thrown: d.thrown,
            message: d.message,
            span: d.span.unwrap_or_else(|| self.cur_span()),
            trace: Vec::new(),
        });
        match &exports {
            Ok(ex) => claim.complete(ex.clone()),
            // The claim drops here: release the path and wake any waiter.
            Err(_) => drop(claim),
        }
        exports
    }

    /// Run an already-compiled module script function and collect its exported
    /// public names. Shares this VM's globals, so the caller sees the imported
    /// definitions. Used for embedded modules (self-contained `.matrixc`).
    /// Const-ness is unknowable here (no AST in bytecode); the semantic pass
    /// rejected any assignment to an imported const at compile time.
    fn run_module_from_func(
        &mut self,
        func: Arc<VmFunc>,
        public: Vec<String>,
    ) -> Result<Vec<(String, Value, bool)>, VmError> {
        if let Err(e) = validate_chunk(&func.chunk) {
            return Err(VmError {
                thrown: None,
                message: e.message,
                span: self.cur_span(),
                trace: Vec::new(),
            });
        }
        self.call_vfunc_frame(func, Vec::new(), None)?;
        let _ = self.run()?;
        let g = self.globals.lock().unwrap();
        // Const-ness comes from the module's own constant set (populated by
        // `DefineConst` while the module ran), so an embedded module reports
        // its `public const` exports exactly like a source module does.
        let consts = self.consts.lock().unwrap().clone();
        let mut exports = Vec::new();
        for name in &public {
            if let Some(v) = g.get(name) {
                exports.push((name.clone(), v.clone(), consts.contains(name)));
            }
        }
        Ok(exports)
    }

    /// Higher-order list methods for the VM: iterate the snapshot and call the
    /// callback synchronously via `call_value_sync`. The iteration logic itself
    /// lives in `collections` (shared with the tree interpreter); this wrapper
    /// supplies the engine side of the `CallbackHost` trait.
    /// Run an option/result combinator whose argument is a script function.
    fn host_callback_method(
        &mut self,
        receiver: &Value,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Value, VmError> {
        struct VmHost<'a> {
            vm: &'a mut Vm,
        }
        impl crate::value::collections::CallbackHost for VmHost<'_> {
            fn call_callback(&mut self, f: &Value, args: Vec<Value>) -> Result<Value, String> {
                self.vm.call_value_sync(f, args).map_err(|e| {
                    crate::sched::stash_error_thrown(e.thrown.clone());
                    match &e.thrown {
                        Some(v) => crate::value::display_value(v),
                        None => e.message,
                    }
                })
            }
        }
        let mut host = VmHost { vm: self };
        crate::value::collections::call_method_host(&mut host, receiver, name, args).map_err(
            |msg| {
                let mut err = VmError {
                    thrown: None,
                    message: msg,
                    span: self.cur_span(),
                    trace: Vec::new(),
                };
                if let Some(v) = crate::sched::take_error_thrown() {
                    err.thrown = Some(v);
                }
                err
            },
        )
    }

    fn list_callback_vm(
        &mut self,
        receiver: &Value,
        items: &[Value],
        name: &str,
        args: Vec<Value>,
    ) -> Result<Value, VmError> {
        struct VmHost<'a> {
            vm: &'a mut Vm,
        }
        impl crate::value::collections::CallbackHost for VmHost<'_> {
            fn call_callback(&mut self, f: &Value, args: Vec<Value>) -> Result<Value, String> {
                // Same rule as the tree engine and as the task boundary:
                // surface the user's thrown value rather than the generic
                // "uncaught throw" message. The String-only error channel
                // loses the value itself, so stash it for the CallMethod seam
                // to rebind in `catch` (audit A-9).
                self.vm.call_value_sync(f, args).map_err(|e| {
                    crate::sched::stash_error_thrown(e.thrown.clone());
                    match &e.thrown {
                        Some(v) => crate::value::display_value(v),
                        None => e.message,
                    }
                })
            }
        }
        let mut host = VmHost { vm: self };
        crate::value::collections::list_callback_method(
            &mut host,
            Some(receiver),
            items,
            name,
            &args,
        )
        .map_err(|msg| {
            // Rebind a thrown user value stashed by the callback host so
            // `catch` sees the original value, not a string (audit A-9).
            let mut err = VmError {
                thrown: None,
                message: msg,
                span: self.cur_span(),
                trace: Vec::new(),
            };
            if let Some(v) = crate::sched::take_error_thrown() {
                err.thrown = Some(v);
            }
            err
        })
    }

    // ------------------------------------------------------------- code read

    fn code_u8(&self, nf: usize, p: usize) -> u8 {
        *self.frames[nf].func.chunk.code.get(p).unwrap_or(&0)
    }

    fn code_u16(&self, nf: usize, p: usize) -> u16 {
        let c = &self.frames[nf].func.chunk.code;
        let mut a = [0u8; 2];
        if p + 1 < c.len() {
            a[0] = c[p];
            a[1] = c[p + 1];
        }
        u16::from_le_bytes(a)
    }

    fn code_i16(&self, nf: usize, p: usize) -> i16 {
        self.code_u16(nf, p) as i16
    }

    fn const_at(&self, nf: usize, idx: usize) -> Value {
        self.frames[nf]
            .func
            .chunk
            .constants
            .get(idx)
            .cloned()
            .unwrap_or(Value::Null)
    }

    fn func_proto_at(&self, nf: usize, idx: usize) -> std::sync::Arc<VmFuncProto> {
        self.frames[nf]
            .func
            .chunk
            .functions
            .get(idx)
            .cloned()
            .unwrap_or_else(|| {
                std::sync::Arc::new(VmFuncProto {
                    name: None,
                    arity: 0,
                    min_arity: 0,
                    defaults: Vec::new(),
                    is_async: false,
                    upvalues: Vec::new(),
                    own_upvalues: 0,
                    chunk: std::sync::Arc::new(Chunk::new()),
                    debug_locals: Vec::new(),
                })
            })
    }

    fn class_proto_at(&self, nf: usize, idx: usize) -> VmClassProto {
        self.frames[nf]
            .func
            .chunk
            .classes
            .get(idx)
            .cloned()
            .unwrap_or_else(|| VmClassProto {
                name: String::new(),
                parent: None,
                fields: Vec::new(),
                methods: Vec::new(),
            })
    }

    fn cur_span(&self) -> Span {
        match self.frames.last() {
            Some(frame) => frame.func.chunk.span_at(frame.inst_ip),
            None => Span::new(self.file.clone(), 0, 0, 0),
        }
    }

    fn fail(&mut self, msg: impl Into<String>) {
        self.error = Some(VmError {
            thrown: None,
            message: msg.into(),
            span: self.cur_span(),
            trace: Vec::new(),
        });
    }
}

// --------------------------------------------------------- free functions

fn find_method_vclass(class: &Arc<VmClass>, name: &str) -> Option<Arc<VmFunc>> {
    let mut cur = Some(class.clone());
    while let Some(c) = cur {
        if let Some((_, fd)) = c.methods.get(name) {
            return Some(fd.clone());
        }
        cur = c.parent.clone();
    }
    None
}

/// Convert a tree-walk `ClassData` to a `VmClass`. Used only for `Object`
/// (which has no methods), so method/ctor shells are intentionally empty.
fn tree_class_to_vclass(cd: &crate::value::ClassData) -> Arc<VmClass> {
    Arc::new(VmClass {
        name: cd.name.clone(),
        parent: cd.parent.as_ref().map(|p| tree_class_to_vclass(p)),
        fields: cd.fields.clone(),
        methods: HashMap::new(),
        ctor: None,
    })
}

/// Binary operator semantics, identical to the tree-walk interpreter.
/// (Also used by the native backend's `spid3r_binop` trampoline.)
pub(crate) fn vm_eval_binary(op: BinOp, l: Value, r: Value) -> Result<Value, String> {
    use Value::*;
    match op {
        BinOp::Add => match (&l, &r) {
            (Int(a), Int(b)) => Ok(Int(crate::value::checked_int("+", *a, *b)?)),
            (Int(a), Float(b)) => Ok(Float(*a as f64 + b)),
            (Float(a), Int(b)) => Ok(Float(a + *b as f64)),
            (Float(a), Float(b)) => Ok(Float(a + b)),
            (Str(a), Str(b)) => {
                let mut s = String::with_capacity(a.len() + b.len());
                s.push_str(a);
                s.push_str(b);
                Ok(Str(s.into()))
            }
            _ => Err(format!(
                "cannot add {} and {}",
                type_name(&l),
                type_name(&r)
            )),
        },
        BinOp::Sub => match (&l, &r) {
            (Int(a), Int(b)) => Ok(Int(crate::value::checked_int("-", *a, *b)?)),
            (Int(a), Float(b)) => Ok(Float(*a as f64 - b)),
            (Float(a), Int(b)) => Ok(Float(a - *b as f64)),
            (Float(a), Float(b)) => Ok(Float(a - b)),
            _ => Err(format!(
                "cannot subtract {} from {}",
                type_name(&r),
                type_name(&l)
            )),
        },
        BinOp::Mul => match (&l, &r) {
            (Int(a), Int(b)) => Ok(Int(crate::value::checked_int("*", *a, *b)?)),
            (Int(a), Float(b)) => Ok(Float(*a as f64 * b)),
            (Float(a), Int(b)) => Ok(Float(a * *b as f64)),
            (Float(a), Float(b)) => Ok(Float(a * b)),
            _ => Err(format!(
                "cannot multiply {} and {}",
                type_name(&l),
                type_name(&r)
            )),
        },
        BinOp::Div => match (&l, &r) {
            (Int(_), Int(b)) if *b == 0 => Err("division by zero".into()),
            (Int(a), Int(b)) => Ok(Int(a.wrapping_div(*b))),
            (Int(a), Float(b)) => Ok(Float(*a as f64 / b)),
            (Float(a), Int(b)) => Ok(Float(a / *b as f64)),
            (Float(a), Float(b)) if *b == 0.0 => Err("division by zero".into()),
            (Float(a), Float(b)) => Ok(Float(a / b)),
            _ => Err(format!(
                "cannot divide {} by {}",
                type_name(&l),
                type_name(&r)
            )),
        },
        BinOp::Mod => match (&l, &r) {
            (Int(_), Int(b)) if *b == 0 => Err("modulo by zero".into()),
            (Int(a), Int(b)) => Ok(Int(a.wrapping_rem(*b))),
            (Int(a), Float(b)) => Ok(Float(*a as f64 % b)),
            (Float(a), Int(b)) => Ok(Float(a % *b as f64)),
            (Float(a), Float(b)) if *b == 0.0 => Err("modulo by zero".into()),
            (Float(a), Float(b)) => Ok(Float(a % b)),
            _ => Err(format!(
                "cannot take {} mod {}",
                type_name(&l),
                type_name(&r)
            )),
        },
        BinOp::Eq => Ok(Bool(values_equal(&l, &r))),
        BinOp::Ne => Ok(Bool(!values_equal(&l, &r))),
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            let ord = match (&l, &r) {
                (Int(a), Int(b)) => Some(a.cmp(b)),
                (Int(a), Float(b)) => (*a as f64).partial_cmp(b),
                (Float(a), Int(b)) => a.partial_cmp(&(*b as f64)),
                (Float(a), Float(b)) => a.partial_cmp(b),
                (Str(a), Str(b)) => Some(a.cmp(b)),
                _ => None,
            };
            match ord {
                Some(o) => {
                    let res = match op {
                        BinOp::Lt => o.is_lt(),
                        BinOp::Le => o.is_le(),
                        BinOp::Gt => o.is_gt(),
                        _ => o.is_ge(),
                    };
                    Ok(Bool(res))
                }
                None => Err(format!(
                    "cannot compare {} and {}",
                    type_name(&l),
                    type_name(&r)
                )),
            }
        }
    }
}

/// Unary operator semantics, identical to the tree-walk interpreter.
/// (Also used by the native backend's `spid3r_unop` trampoline.)
pub(crate) fn vm_eval_unary(op: UnOp, v: Value) -> Result<Value, String> {
    match op {
        UnOp::Not => Ok(Value::Bool(!truthy(&v))),
        UnOp::Neg => match v {
            Value::Int(i) => Ok(Value::Int(crate::value::checked_neg(i)?)),
            Value::Float(f) => Ok(Value::Float(-f)),
            other => Err(format!("cannot negate {}", type_name(&other))),
        },
    }
}
