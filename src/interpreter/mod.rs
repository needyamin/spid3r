//! The reference runner: a tree-walk interpreter over the checked AST.
//! It stays the oracle for the bytecode virtual machine.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::ast::*;
use crate::diagnostics::{Diagnostic, Phase, Span};
use crate::environment::{self, Env};
use crate::sched::Scheduler;
use crate::value::collections;
use crate::value::{FunctionData, InstanceData, Value};

/// Maximum Spid3r call depth before the interpreter reports a stack overflow.
/// Deep recursion is a Spid3r error, not a Rust panic. The guard is checked on
/// every call, and the program runs on a thread with a large stack, so this
/// limit - not the OS stack size - is what stops runaway recursion.
pub const MAX_CALL_DEPTH: usize = 1_000;

#[cfg(not(target_arch = "wasm32"))]
// The program thread's stack. `MAX_CALL_DEPTH` (1000) is a documented
// language guarantee, and an interpreter frame costs tens of KB (the large
// `Diagnostic` sits in every `Result` on the call path), so 1000 levels need
// ~60 MB. 64 MiB left no margin: the depth-guard test passed locally but
// aborted with a stack overflow on macOS CI (different frame layout). 256 MiB
// is a virtual reservation, committed only as used.
const RUN_STACK_BYTES: usize = 256 * 1024 * 1024;

/// Stack size for REPL evaluation threads (same rationale as RUN_STACK_BYTES).
pub const REPL_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Result of executing a statement: a value, a control-flow signal, or an error.
pub enum Flow {
    Value(Value),
    Return(Value),
    Break,
    Continue,
    Error(Diagnostic),
    /// An error no `try`/`catch` may intercept — currently only execution
    /// budget exhaustion. It propagates exactly like `Error` except that the
    /// `try` statement does not match it.
    Fatal(Diagnostic),
}

/// Public sweep handle for other crates in this workspace (`modules`) that
/// run interpreter entry points; see [`GuardSweep`].
pub struct GuardSweepPublic<'a> {
    interp: &'a Interp,
    mark: usize,
}

impl<'a> GuardSweepPublic<'a> {
    pub fn new(interp: &'a Interp) -> Self {
        GuardSweepPublic {
            interp,
            mark: interp.guard_mark(),
        }
    }
}

impl Drop for GuardSweepPublic<'_> {
    fn drop(&mut self) {
        self.interp.release_guards(self.mark);
    }
}

/// Releases every mutex this interpreter still holds when the entry-point
/// stack frame goes away.
///
/// A panic is never caught *inside* the engine — it unwinds out of the whole
/// run — so a sweep at each entry point (program run, spawned task, module
/// load) covers every user frame. On the normal path the sweep is a no-op:
/// the call boundaries already released their guards, so nothing is left
/// above the entry mark. The sweep lives at entry points, not per block, so
/// deep recursion does not pay extra stack per frame.
struct GuardSweep<'a> {
    interp: &'a Interp,
    mark: usize,
}

impl Drop for GuardSweep<'_> {
    fn drop(&mut self) {
        self.interp.release_guards(self.mark);
    }
}

#[derive(Clone)]
enum TraceCallee {
    Func(Option<String>),
    Method(Arc<crate::value::ClassData>, Arc<FunctionData>),
    Ctor(Arc<crate::value::ClassData>),
}

impl TraceCallee {
    fn to_name(&self) -> Option<String> {
        match self {
            TraceCallee::Func(n) => n.clone(),
            TraceCallee::Method(c, fd) => Some(format!(
                "{}::{}",
                c.name,
                fd.proto.name.as_deref().unwrap_or("")
            )),
            TraceCallee::Ctor(c) => Some(format!("{}::new", c.name)),
        }
    }
}

pub struct Interp {
    pub globals: Env,
    pub sched: Arc<Scheduler>,
    pub modules: Arc<Mutex<crate::modules::ModuleCache>>,
    pub file: Arc<str>,
    pub module_dirs: Vec<PathBuf>,
    /// Mutex guards held by the current scope; released at block exit. Each
    /// entry unlocks its mutex when dropped, so a panic unwinding through the
    /// interpreter cannot leave a user mutex locked.
    pub guards: Mutex<Vec<crate::sched::MutexGuardToken>>,
    guards_len: AtomicUsize,
    pub is_module: bool,
    pub public_names: Mutex<HashSet<String>>,
    /// Active call frames for error traces: (callee, call site).
    call_stack: Mutex<Vec<(TraceCallee, Span)>>,
    depth: AtomicUsize,
    /// Execution budget (`Engine::budget` / `spid3r run --budget`). `None`
    /// costs one `Option` check per charge site. Spawned tasks inherit a
    /// clone, so the whole run shares one allowance.
    budget: Option<crate::budget::Budget>,
}

/// Lets builtin collections call Spid3r functions back (higher-order methods
/// such as `list.map(f)`, `filter`, `reduce`). The tree interpreter calls the
/// function value directly and re-enters `call_function`.
struct InterpCallbackHost<'a> {
    interp: &'a Interp,
    span: Span,
}

impl crate::value::collections::CallbackHost for InterpCallbackHost<'_> {
    fn call_callback(&mut self, f: &Value, args: Vec<Value>) -> Result<Value, String> {
        match self.interp.call_value(f.clone(), args, &self.span) {
            Ok(v) => Ok(v),
            Err(Flow::Error(d)) => {
                // This seam only carries a String, so stash the user-thrown
                // value for the engine's method-call seam to rebind in
                // `catch` (audit A-9) — otherwise `l.map(f)` where `f` throws
                // an object degrades the payload to its display string.
                crate::sched::stash_error_thrown(d.thrown.clone());
                Err(match &d.thrown {
                    Some(v) => crate::value::display_value(v),
                    None => d.message.clone(),
                })
            }
            // A fatal error (budget exhaustion) is not an ordinary callback
            // failure: the collection method propagates the message as its
            // error, which the enclosing statement turns back into an error —
            // and the next budget charge (already exhausted) is fatal again.
            Err(Flow::Fatal(d)) => Err(d.message.clone()),
            Err(Flow::Return(v)) => Ok(v),
            Err(_) => Err("invalid control flow in a callback".to_string()),
        }
    }
}

impl Interp {
    pub fn new(file: Arc<str>, module_dirs: Vec<PathBuf>) -> Self {
        let interp = Interp {
            globals: environment::new_global(),
            sched: Scheduler::shared(),
            modules: Arc::new(Mutex::new(crate::modules::ModuleCache::new())),
            file,
            module_dirs,
            guards: Mutex::new(Vec::new()),
            guards_len: AtomicUsize::new(0),
            is_module: false,
            public_names: Mutex::new(HashSet::new()),
            call_stack: Mutex::new(Vec::new()),
            depth: AtomicUsize::new(0),
            budget: None,
        };
        crate::stdlib::install(&interp.globals, interp.sched.clone());
        interp.install_builtin_classes();
        interp
    }

    /// The global environment this interpreter installed its builtins into.
    ///
    /// The debugger uses it to evaluate `print`/`watch` expressions: it binds
    /// the paused frame's locals as globals and evaluates there, so a watch can
    /// use anything the program can (`len(x)`, `json_write(v)`, a closure it
    /// closed over, ...) without a second evaluator existing.
    pub(crate) fn globals(&self) -> &Env {
        &self.globals
    }

    /// A module-side interpreter: its own globals, module mode, sharing the
    /// scheduler and the module cache with the importer.
    pub fn module_interp(
        file: Arc<str>,
        module_dirs: Vec<PathBuf>,
        sched: Arc<Scheduler>,
        modules: Arc<Mutex<crate::modules::ModuleCache>>,
        _importer_globals: Env,
        budget: Option<crate::budget::Budget>,
    ) -> Self {
        let interp = Interp {
            globals: environment::new_global(),
            sched,
            modules,
            file,
            module_dirs,
            guards: Mutex::new(Vec::new()),
            guards_len: AtomicUsize::new(0),
            is_module: true,
            public_names: Mutex::new(HashSet::new()),
            call_stack: Mutex::new(Vec::new()),
            depth: AtomicUsize::new(0),
            budget,
        };
        crate::stdlib::install(&interp.globals, interp.sched.clone());
        interp.install_builtin_classes();
        interp
    }

    /// A task-side interpreter sharing globals, scheduler, and module cache.
    pub fn shared(
        globals: Env,
        sched: Arc<Scheduler>,
        modules: Arc<Mutex<crate::modules::ModuleCache>>,
        file: Arc<str>,
        module_dirs: Vec<PathBuf>,
    ) -> Self {
        Interp {
            globals,
            sched,
            modules,
            file,
            module_dirs,
            guards: Mutex::new(Vec::new()),
            guards_len: AtomicUsize::new(0),
            is_module: false,
            public_names: Mutex::new(HashSet::new()),
            call_stack: Mutex::new(Vec::new()),
            depth: AtomicUsize::new(0),
            budget: None,
        }
    }

    fn install_builtin_classes(&self) {
        // The root class. `extends Object` works; classes without a parent
        // simply have none.
        let object = crate::value::new_class(
            "Object".to_string(),
            None,
            Vec::new(),
            Vec::new(),
            HashMap::new(),
            None,
        );
        environment::define(&self.globals, "Object", Value::Class(object));
    }

    /// Run a whole program file: top-level statements first, then `main`.
    /// Returns the process exit code.
    ///
    /// Execution happens on a dedicated large-stack thread so that runaway
    /// recursion is stopped by MAX_CALL_DEPTH with a clean diagnostic instead
    /// of a Rust stack overflow.
    pub fn run_program(&self, program: &Program) -> Result<i32, Diagnostic> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let result = std::thread::scope(|s| {
                let handle = std::thread::Builder::new()
                    .stack_size(RUN_STACK_BYTES)
                    .spawn_scoped(s, || self.run_program_inner(program))
                    .expect("failed to start the spid3r program thread");
                match handle.join() {
                    Ok(result) => result,
                    Err(_) => Err(Diagnostic::new(
                        Phase::Runtime,
                        "the program thread panicked; this is a bug in spid3r",
                    )),
                }
            });
            // Join-on-exit (audit A-3): wait for tasks `main` left running so
            // their output is not lost to process teardown — also on the error
            // path, where a failing program may still have live tasks.
            crate::sched::join_tasks_at_exit();
            result
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.run_program_inner(program)
        }
    }

    /// Run program inline on the current thread without spawning a new OS thread.
    /// Used by WebAssembly and embedded/in-memory callers.
    pub fn run_program_inline(&self, program: &Program) -> Result<i32, Diagnostic> {
        let result = self.run_program_inner(program);
        // Same join-on-exit contract as `run_program` (audit A-3), so an
        // embedding host does not lose output written by spawned tasks.
        #[cfg(not(target_arch = "wasm32"))]
        crate::sched::join_tasks_at_exit();
        result
    }

    /// Run a module body: every top-level statement, and **no `fn main`
    /// required**.
    ///
    /// [`run_program`] is the *program* entry: it executes the top level and
    /// then calls `main`. A module has no entry point — the definitions are the
    /// point — so a loader needs this instead. A Spid3r-source plugin is exactly
    /// that case: its `public fn`s must be bound without inventing a `main`.
    pub fn run_module(&self, program: &Program) -> Result<(), Diagnostic> {
        let _guard_sweep = GuardSweep {
            interp: self,
            mark: self.guard_mark(),
        };
        let lowered = crate::lower::lower(program);
        let _active = crate::sched::enter_active();
        let env = self.globals.clone();
        for stmt in &lowered.as_ref().stmts {
            match self.exec_stmt(stmt, &env) {
                Flow::Value(_) => {}
                Flow::Error(d) | Flow::Fatal(d) => return Err(d),
                other => return Err(self.invalid_flow(&other, stmt.span())),
            }
        }
        Ok(())
    }

    fn run_program_inner(&self, program: &Program) -> Result<i32, Diagnostic> {
        // `extern fn` resolves at link time, and the tree interpreter links
        // nothing. Refuse before running a single statement rather than
        // defining a function whose body is empty — a silent `null` return is
        // the one outcome worse than an error.
        if let Some(name) = program.stmts.iter().find_map(|s| match s {
            Stmt::Fn(f) if f.is_extern => Some(f.name.clone().unwrap_or_default()),
            _ => None,
        }) {
            return Err(Diagnostic::new(
                Phase::Usage,
                format!(
                    "extern fn '{name}' needs a native executable: the tree interpreter cannot call C. \
                     Build with `spidr build`."
                ),
            ));
        }
        // On a panic (which unwinds out of the whole run), release every user
        // mutex this interpreter still holds; see `GuardSweep`.
        let _guard_sweep = GuardSweep {
            interp: self,
            mark: self.guard_mark(),
        };
        // Every backend executes the *lowered* AST, never the parser's output
        // directly (see `crate::lower`). The pass is the identity today, so this
        // borrows rather than clones.
        let lowered = crate::lower::lower(program);
        let program = lowered.as_ref();
        // Count this thread as executing Spid3r code: the GC gate allows a
        // collection only when the caller is the only active thread (audit A-4).
        let _active = crate::sched::enter_active();
        let env = self.globals.clone();
        for stmt in &program.stmts {
            match self.exec_stmt(stmt, &env) {
                Flow::Value(_) => {}
                Flow::Error(d) => return Err(d),
                Flow::Fatal(d) => return Err(d),
                other => return Err(self.invalid_flow(&other, stmt.span())),
            }
        }
        let main = environment::get(&self.globals, "main");
        let main = match main {
            Some(Value::Function(fd)) => fd,
            Some(_) => {
                return Err(Diagnostic::new(
                    Phase::Semantic,
                    "main must be a function: fn main() { ... }",
                ))
            }
            None => {
                return Err(Diagnostic::new(
                    Phase::Semantic,
                    "this program has no fn main; add one to run it as a program",
                ))
            }
        };
        let span = main.proto.span.clone();
        let v = match self.call_function(&main, Vec::new(), &span) {
            Ok(v) => v,
            Err(Flow::Error(d)) => return Err(d),
            Err(Flow::Fatal(d)) => return Err(d),
            Err(_) => {
                return Err(Diagnostic::new(
                    Phase::Runtime,
                    "main ended with invalid control flow",
                ))
            }
        };
        Ok(match v {
            Value::Int(i) => i as i32,
            _ => 0,
        })
    }

    fn invalid_flow(&self, f: &Flow, span: &Span) -> Diagnostic {
        let what = match f {
            Flow::Return(_) => "return outside a function",
            Flow::Break => "break outside a loop",
            Flow::Continue => "continue outside a loop",
            _ => "invalid control flow",
        };
        Diagnostic::at(Phase::Runtime, span.clone(), what)
    }

    // ------------------------------------------------------------- errors

    /// Build a runtime error. Cold path: kept out of line so the (large)
    /// `Diagnostic` locals do not land in the frame of every caller — the
    /// interpreter recurses once per call level, and inflating `eval`'s frame
    /// multiplies directly into the recursion stack budget.
    #[inline(never)]
    pub fn rt(&self, span: &Span, msg: impl Into<String>) -> Flow {
        Flow::Error(
            Diagnostic::at(Phase::Runtime, span.clone(), msg).with_trace(self.trace_frames(span)),
        )
    }

    /// Build a fatal runtime error: same shape as [`Interp::rt`], but no
    /// `try`/`catch` may intercept it. Used for execution-budget exhaustion.
    #[inline(never)]
    pub fn fatal(&self, span: &Span, msg: impl Into<String>) -> Flow {
        Flow::Fatal(
            Diagnostic::at(Phase::Runtime, span.clone(), msg).with_trace(self.trace_frames(span)),
        )
    }

    /// Charge one budget unit; returns the fatal flow when the budget is
    /// exhausted. `None` (the default) costs one `Option` check.
    #[inline]
    fn charge_budget(&self, span: &Span) -> Option<Flow> {
        match &self.budget {
            Some(b) if !b.charge(1) => Some(self.fatal(span, "execution budget exhausted")),
            _ => None,
        }
    }

    /// Install an execution budget (`Engine::budget`, `spid3r run --budget`).
    /// `None` (the default) means unlimited.
    pub fn set_budget(&mut self, budget: Option<crate::budget::Budget>) {
        self.budget = budget;
    }

    /// Share this interpreter's budget (clone) with a child interpreter —
    /// spawned tasks and module initialization draw one allowance.
    pub(crate) fn budget_handle(&self) -> Option<crate::budget::Budget> {
        self.budget.clone()
    }

    // ------------------------------------------------------------ statements

    #[inline]
    fn guard_mark(&self) -> usize {
        self.guards_len.load(Ordering::Relaxed)
    }

    #[inline]
    fn release_guards(&self, mark: usize) {
        if self.guards_len.load(Ordering::Relaxed) <= mark {
            return;
        }
        let mut guards = self.guards.lock().unwrap();
        while guards.len() > mark {
            // Dropping the token unlocks the mutex (and wakes a waiter).
            guards.pop();
        }
        self.guards_len.store(guards.len(), Ordering::Relaxed);
    }

    #[inline]
    fn block_needs_env(stmts: &[Stmt]) -> bool {
        stmts.iter().any(|s| {
            matches!(
                s,
                Stmt::Let { .. }
                    | Stmt::Const { .. }
                    | Stmt::Fn(_)
                    | Stmt::Class(_)
                    | Stmt::Import { .. }
            )
        })
    }

    pub fn exec_block(&self, stmts: &[Stmt], env: &Env) -> Flow {
        for s in stmts {
            // Execution budget: one unit per statement.
            if let Some(flow) = self.charge_budget(s.span()) {
                return flow;
            }
            // Cycle collection at a statement boundary (no-op unless SPID3R_GC).
            crate::value::gc::maybe_collect();
            // Cancellation point: a cancelled task stops at the next
            // statement boundary.
            if crate::sched::current_task_cancelled() {
                return self.rt(s.span(), "task cancelled");
            }
            match self.exec_stmt(s, env) {
                Flow::Value(_) => continue,
                other => return other,
            }
        }
        Flow::Value(Value::Null)
    }

    pub fn exec_stmt(&self, s: &Stmt, env: &Env) -> Flow {
        match s {
            // `crate::lower` rewrites `yield` into the generator form; seeing
            // one here is a pipeline bug.
            Stmt::Yield { span, .. } => self.rt(
                span,
                "internal error: a `yield` reached the interpreter without lowering",
            ),
            // `crate::lower` turns an enum into constructor functions; a backend
            // must never see the declaration itself.
            Stmt::LetTuple { span, .. } => Flow::Error(crate::diagnostics::Diagnostic::at(
                crate::diagnostics::Phase::Runtime,
                span.clone(),
                "internal error: a destructuring `let` reached the interpreter without \
                 lowering (this is a bug in spid3r)",
            )),
            Stmt::Enum(e) => Flow::Error(crate::diagnostics::Diagnostic::at(
                crate::diagnostics::Phase::Runtime,
                e.span.clone(),
                "internal error: an enum declaration reached the interpreter without \
                 lowering (this is a bug in spid3r)",
            )),
            Stmt::Import { path, alias, span } => match crate::modules::load(self, path, span) {
                Ok(names) => {
                    if let Some(alias_name) = alias {
                        let mut entries = Vec::new();
                        let mut frozen = std::collections::HashSet::new();
                        for (name, value, is_const) in names {
                            let key = crate::value::MapKey::Str(name.into());
                            if is_const {
                                // A `public const` export stays immutable
                                // through the namespace as well.
                                frozen.insert(key.clone());
                            }
                            entries.push((key, value));
                        }
                        // The alias is a **module namespace**: a read-only view
                        // of the module's exports. `mu.get()` calls the
                        // module's `get`, and the namespace cannot be mutated.
                        let map_val = Value::Map(crate::value::new_ref(
                            crate::value::collections::MapData::namespace(entries, frozen),
                        ));
                        environment::define_const(env, alias_name, map_val);
                    } else {
                        for (name, value, is_const) in names {
                            if is_const {
                                environment::define_const(env, &name, value);
                            } else {
                                environment::define(env, &name, value);
                            }
                        }
                    }
                    Flow::Value(Value::Null)
                }
                Err(d) => {
                    // A module whose initialization exhausted the shared
                    // budget must surface as fatal, not as a catchable
                    // import error.
                    if let Some(b) = &self.budget {
                        if b.exhausted() {
                            return self.fatal(s.span(), "execution budget exhausted");
                        }
                    }
                    Flow::Error(d)
                }
            },
            Stmt::Fn(proto) => {
                if let Some(name) = &proto.name {
                    let f = Value::Function(crate::value::new_function(proto.clone(), env.clone()));
                    environment::define(env, name, f);
                    if self.is_module && proto.vis == Vis::Public {
                        self.public_names.lock().unwrap().insert(name.clone());
                    }
                }
                Flow::Value(Value::Null)
            }
            Stmt::Class(decl) => {
                let class = self.build_class(decl, env);
                environment::define(env, &decl.name, Value::Class(class));
                if self.is_module && decl.vis == Vis::Public {
                    self.public_names.lock().unwrap().insert(decl.name.clone());
                }
                Flow::Value(Value::Null)
            }
            Stmt::Interface(decl) => {
                if self.is_module && decl.vis == Vis::Public {
                    self.public_names.lock().unwrap().insert(decl.name.clone());
                }
                Flow::Value(Value::Null)
            }
            Stmt::Let {
                name, value, vis, ..
            } => {
                let v = match value {
                    Some(e) => match self.eval(e, env) {
                        Ok(v) => v,
                        Err(f) => return f,
                    },
                    None => Value::Null,
                };
                environment::define(env, name, v);
                if self.is_module && *vis == Vis::Public {
                    self.public_names.lock().unwrap().insert(name.clone());
                }
                Flow::Value(Value::Null)
            }
            Stmt::Const {
                name, value, vis, ..
            } => {
                let v = match self.eval(value, env) {
                    Ok(v) => v,
                    Err(f) => return f,
                };
                environment::define_const(env, name, v);
                if self.is_module && *vis == Vis::Public {
                    self.public_names.lock().unwrap().insert(name.clone());
                }
                Flow::Value(Value::Null)
            }
            Stmt::If {
                cond,
                then,
                elifs,
                els,
                ..
            } => {
                let c = match self.eval(cond, env) {
                    Ok(v) => v,
                    Err(f) => return f,
                };
                if crate::value::truthy(&c) {
                    let mark = self.guard_mark();
                    let r = if Self::block_needs_env(&then.stmts) {
                        let benv = environment::child(env);
                        self.exec_block(&then.stmts, &benv)
                    } else {
                        self.exec_block(&then.stmts, env)
                    };
                    self.release_guards(mark);
                    return r;
                }
                for (ec, eb) in elifs {
                    let c = match self.eval(ec, env) {
                        Ok(v) => v,
                        Err(f) => return f,
                    };
                    if crate::value::truthy(&c) {
                        let mark = self.guard_mark();
                        let r = if Self::block_needs_env(&eb.stmts) {
                            let benv = environment::child(env);
                            self.exec_block(&eb.stmts, &benv)
                        } else {
                            self.exec_block(&eb.stmts, env)
                        };
                        self.release_guards(mark);
                        return r;
                    }
                }
                if let Some(eb) = els {
                    let mark = self.guard_mark();
                    let r = if Self::block_needs_env(&eb.stmts) {
                        let benv = environment::child(env);
                        self.exec_block(&eb.stmts, &benv)
                    } else {
                        self.exec_block(&eb.stmts, env)
                    };
                    self.release_guards(mark);
                    return r;
                }
                Flow::Value(Value::Null)
            }
            Stmt::While { cond, body, .. } => {
                let needs_env = Self::block_needs_env(&body.stmts);
                loop {
                    // Budget: one unit per iteration, so an empty-body loop
                    // still consumes allowance (the body's statements charge
                    // on top of this).
                    if let Some(flow) = self.charge_budget(cond.span()) {
                        return flow;
                    }
                    let c = match self.eval(cond, env) {
                        Ok(v) => v,
                        Err(f) => return f,
                    };
                    if !crate::value::truthy(&c) {
                        return Flow::Value(Value::Null);
                    }
                    let mark = self.guard_mark();
                    let res = if needs_env {
                        let benv = environment::child(env);
                        self.exec_block(&body.stmts, &benv)
                    } else {
                        self.exec_block(&body.stmts, env)
                    };
                    match res {
                        Flow::Value(_) => {
                            self.release_guards(mark);
                        }
                        Flow::Break => {
                            self.release_guards(mark);
                            return Flow::Value(Value::Null);
                        }
                        Flow::Continue => {
                            self.release_guards(mark);
                        }
                        other => {
                            self.release_guards(mark);
                            return other;
                        }
                    }
                }
            }
            Stmt::ForIn {
                var, iter, body, ..
            } => {
                let items = match self.eval(iter, env) {
                    Ok(v) => match collections::iter_values(&v) {
                        Ok(items) => items,
                        Err(msg) => return self.rt(iter.span(), msg),
                    },
                    Err(f) => return f,
                };
                for item in items {
                    // Budget: one unit per iteration (an empty body still
                    // consumes allowance).
                    if let Some(flow) = self.charge_budget(iter.span()) {
                        return flow;
                    }
                    let ienv = environment::child(env);
                    environment::define(&ienv, var, item);
                    let mark = self.guard_mark();
                    match self.exec_block(&body.stmts, &ienv) {
                        Flow::Value(_) => {
                            self.release_guards(mark);
                        }
                        Flow::Break => {
                            self.release_guards(mark);
                            return Flow::Value(Value::Null);
                        }
                        Flow::Continue => {
                            self.release_guards(mark);
                        }
                        other => {
                            self.release_guards(mark);
                            return other;
                        }
                    }
                }
                Flow::Value(Value::Null)
            }
            Stmt::ForC {
                init,
                cond,
                post,
                body,
                ..
            } => {
                let lenv = environment::child(env);
                match self.exec_stmt(init, &lenv) {
                    Flow::Value(_) => {}
                    other => return other,
                }
                let needs_env = Self::block_needs_env(&body.stmts);
                loop {
                    let c = match self.eval(cond, &lenv) {
                        Ok(v) => v,
                        Err(f) => return f,
                    };
                    if !crate::value::truthy(&c) {
                        return Flow::Value(Value::Null);
                    }
                    let mark = self.guard_mark();
                    let body_flow = if needs_env {
                        let benv = environment::child(&lenv);
                        self.exec_block(&body.stmts, &benv)
                    } else {
                        self.exec_block(&body.stmts, &lenv)
                    };
                    self.release_guards(mark);
                    match body_flow {
                        Flow::Value(_) => {}
                        Flow::Break => return Flow::Value(Value::Null),
                        Flow::Continue => {}
                        other => return other,
                    }
                    match self.eval(post, &lenv) {
                        Ok(_) => {}
                        Err(f) => return f,
                    }
                }
            }
            Stmt::Return { value, span: _ } => {
                let v = match value {
                    Some(e) => match self.eval(e, env) {
                        Ok(v) => v,
                        Err(f) => return f,
                    },
                    None => Value::Null,
                };
                Flow::Return(v)
            }
            Stmt::Break { .. } => Flow::Break,
            Stmt::Continue { .. } => Flow::Continue,
            Stmt::Block(b) => {
                let mark = self.guard_mark();
                let r = if Self::block_needs_env(&b.stmts) {
                    let benv = environment::child(env);
                    self.exec_block(&b.stmts, &benv)
                } else {
                    self.exec_block(&b.stmts, env)
                };
                self.release_guards(mark);
                r
            }
            Stmt::Expr(e) => match self.eval(e, env) {
                Ok(v) => Flow::Value(v),
                Err(f) => f,
            },
            Stmt::Spawn { callable, span } => {
                let f = match self.eval(callable, env) {
                    Ok(v) => v,
                    Err(flow) => return flow,
                };
                let fd = match f {
                    Value::Function(fd) => fd,
                    other => {
                        return self.rt(
                            span,
                            format!(
                                "spawn expects a function, found {}",
                                crate::value::type_name(&other)
                            ),
                        )
                    }
                };
                let globals = self.globals.clone();
                let sched = self.sched.clone();
                let modules = self.modules.clone();
                let file = self.file.clone();
                let module_dirs = self.module_dirs.clone();
                let budget = self.budget.clone();
                let span2 = span.clone();
                let handle = self.sched.spawn_job(Box::new(move || {
                    let mut interp = Interp::shared(globals, sched, modules, file, module_dirs);
                    // The task draws from the same allowance as its spawner.
                    interp.set_budget(budget);
                    // A panic in the task releases the task's mutexes (see
                    // `GuardSweep`).
                    let _guard_sweep = GuardSweep {
                        interp: &interp,
                        mark: 0,
                    };
                    // Surface the user-thrown value across the task boundary
                    // so `join`/`await` rethrow is structural: `catch` on the
                    // joining side rebinds the original value, not its display
                    // string (audit A-9).
                    match interp.call_function(&fd, Vec::new(), &span2) {
                        Ok(v) => crate::sched::TaskOutcome {
                            result: Ok(v),
                            thrown: None,
                        },
                        Err(Flow::Error(d)) => crate::sched::TaskOutcome {
                            thrown: d.thrown.clone(),
                            result: Err(match &d.thrown {
                                Some(v) => crate::value::display_value(v),
                                None => d.message.clone(),
                            }),
                        },
                        // A fatal error stops the task with its message; the
                        // shared budget stays exhausted, so the task's joiner
                        // fails at its next charge site too.
                        Err(Flow::Fatal(d)) => crate::sched::TaskOutcome {
                            result: Err(d.message.clone()),
                            thrown: None,
                        },
                        Err(_) => crate::sched::TaskOutcome {
                            result: Err("invalid control flow in task".to_string()),
                            thrown: None,
                        },
                    }
                }));
                Flow::Value(Value::Task(handle))
            }
            Stmt::Try {
                body,
                catch,
                finally,
                ..
            } => {
                let benv = environment::child(env);
                let mark = self.guard_mark();
                let body_flow = self.exec_block(&body.stmts, &benv);
                self.release_guards(mark);
                let mut result = match body_flow {
                    Flow::Error(d) => match catch {
                        Some(cc) => {
                            let cenv = environment::child(env);
                            let bound = match d.thrown {
                                Some(v) => v,
                                None => Value::Str(d.message.into()),
                            };
                            environment::define(&cenv, &cc.name, bound);
                            let cmark = self.guard_mark();
                            let r = self.exec_block(&cc.body.stmts, &cenv);
                            self.release_guards(cmark);
                            r
                        }
                        None => Flow::Error(d),
                    },
                    other => other,
                };
                if let Some(fb) = finally {
                    let fenv = environment::child(env);
                    let fmark = self.guard_mark();
                    let fr = self.exec_block(&fb.stmts, &fenv);
                    self.release_guards(fmark);
                    if !matches!(fr, Flow::Value(_)) {
                        result = fr;
                    }
                }
                result
            }
            Stmt::Throw { value, span } => {
                let v = match self.eval(value, env) {
                    Ok(v) => v,
                    Err(f) => return f,
                };
                let mut d = Diagnostic::at(Phase::Runtime, span.clone(), "uncaught throw")
                    .with_trace(self.trace_frames(span));
                d.thrown = Some(v);
                Flow::Error(d)
            }
        }
    }

    // ----------------------------------------------------------- expressions

    pub fn eval(&self, e: &Expr, env: &Env) -> Result<Value, Flow> {
        match e {
            // Surface-only nodes: `crate::lower` rewrites these into a temp plus
            // statements before any backend runs. Reaching one here means the
            // lowering pass was skipped, which is an internal bug, not a program
            // error — report it instead of aborting the host.
            Expr::NamedArg { span, .. } => Err(self.rt(
                span,
                "internal error: a named argument reached the interpreter without lowering",
            )),
            Expr::If { span, .. }
            | Expr::Coalesce { span, .. }
            | Expr::Match { span, .. }
            | Expr::Propagate(_, span) => Err(self.rt(
                span,
                "internal error: a surface-only expression reached the interpreter \
                     without lowering (this is a bug in spid3r)",
            )),
            // `obj?.field` — the null-safe dot (F-4): a null receiver AND a
            // missing map key both read as null; every other failure (an
            // instance without the field, a non-fieldable type) stays the
            // same catchable error plain `.` raises.
            Expr::OptField { object, name, span } => {
                let obj = self.eval(object, env)?;
                match &obj {
                    Value::Null => Ok(Value::Null),
                    Value::Map(_) => match self.field_get(&obj, name, span) {
                        Ok(v) => Ok(v),
                        Err(_) => Ok(Value::Null),
                    },
                    _ => self.field_get(&obj, name, span),
                }
            }
            Expr::Int(i, _) => Ok(Value::Int(*i)),
            Expr::Float(f, _) => Ok(Value::Float(*f)),
            Expr::Bool(b, _) => Ok(Value::Bool(*b)),
            Expr::Null(_) => Ok(Value::Null),
            Expr::Str(s, _) => Ok(Value::Str(s.clone().into())),
            Expr::Ident(name, span) => {
                if name == "none" {
                    return Ok(Value::Option(None));
                }
                match environment::get(env, name) {
                    Some(v) => Ok(v),
                    None => Err(self.rt(span, format!("unknown name '{}'", name))),
                }
            }
            Expr::List(items, span) => {
                let mut vals = Vec::with_capacity(items.len());
                for it in items {
                    vals.push(self.eval(it, env)?);
                }
                collections::construct("list", vals).map_err(|msg| self.rt(span, msg))
            }
            Expr::Fn(proto) => Ok(Value::Function(crate::value::new_function(
                (**proto).clone(),
                env.clone(),
            ))),
            Expr::Unary { op, expr, span } => {
                let v = self.eval(expr, env)?;
                match op {
                    UnOp::Not => Ok(Value::Bool(!crate::value::truthy(&v))),
                    UnOp::Neg => match v {
                        Value::Int(i) => Ok(Value::Int(
                            crate::value::checked_neg(i).map_err(|e| self.rt(span, e))?,
                        )),
                        Value::Float(f) => Ok(Value::Float(-f)),
                        other => match self.unary_operator_call(&other, "__neg__", span) {
                            Some(res) => res,
                            None => Err(self.rt(
                                span,
                                format!("cannot negate {}", crate::value::type_name(&other)),
                            )),
                        },
                    },
                }
            }
            Expr::Binary { op, l, r, span } => {
                let lv = self.eval(l, env)?;
                let rv = self.eval(r, env)?;
                self.eval_binary(*op, lv, rv, span)
            }
            Expr::Logical { op, l, r, .. } => {
                let lv = self.eval(l, env)?;
                match op {
                    LogOp::And => {
                        if !crate::value::truthy(&lv) {
                            return Ok(Value::Bool(false));
                        }
                        let rv = self.eval(r, env)?;
                        Ok(Value::Bool(crate::value::truthy(&rv)))
                    }
                    LogOp::Or => {
                        if crate::value::truthy(&lv) {
                            return Ok(Value::Bool(true));
                        }
                        let rv = self.eval(r, env)?;
                        Ok(Value::Bool(crate::value::truthy(&rv)))
                    }
                }
            }
            Expr::Assign {
                target,
                op,
                value,
                span,
            } => {
                let v = self.eval(value, env)?;
                match &**target {
                    Expr::Ident(name, _) => {
                        let new_v = if *op == AssignOp::Assign {
                            v
                        } else {
                            let old = match environment::get(env, name) {
                                Some(v) => v,
                                None => {
                                    return Err(self.rt(span, format!("unknown name '{}'", name)))
                                }
                            };
                            self.compound(*op, old, v, span)?
                        };
                        if let Err(e) = environment::assign(env, name, new_v.clone()) {
                            return Err(self.rt(
                                span,
                                match e {
                                    environment::AssignError::Const(n) => {
                                        format!("cannot assign to const '{}'", n)
                                    }
                                    environment::AssignError::Unbound(n) => {
                                        format!("unknown name '{}'", n)
                                    }
                                },
                            ));
                        }
                        Ok(new_v)
                    }
                    Expr::Index { object, index, .. } => {
                        let obj = self.eval(object, env)?;
                        let idx = self.eval(index, env)?;
                        let new_v = if *op == AssignOp::Assign {
                            v
                        } else {
                            let old = self.index_get(&obj, &idx, span)?;
                            self.compound(*op, old, v, span)?
                        };
                        self.index_set(&obj, &idx, new_v.clone(), span)?;
                        Ok(new_v)
                    }
                    Expr::Field { object, name, .. } => {
                        let obj = self.eval(object, env)?;
                        let new_v = if *op == AssignOp::Assign {
                            v
                        } else {
                            let old = self.field_get(&obj, name, span)?;
                            self.compound(*op, old, v, span)?
                        };
                        self.field_set(&obj, name, new_v.clone(), span)?;
                        Ok(new_v)
                    }
                    _ => Err(self.rt(span, "invalid assignment target")),
                }
            }
            Expr::Call { callee, args, span } => {
                if let Expr::Ident(name, _) = &**callee {
                    if collections::is_constructor(name) {
                        let mut vals = Vec::with_capacity(args.len());
                        for a in args {
                            vals.push(self.eval(a, env)?);
                        }
                        return collections::construct(name, vals)
                            .map_err(|msg| self.rt(span, msg));
                    }
                }
                let f = self.eval(callee, env)?;
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval(a, env)?);
                }
                self.call_value(f, vals, span)
            }
            Expr::Method {
                object,
                name,
                args,
                span,
            } => {
                let obj = self.eval(object, env)?;
                if let Value::MutexVal(m) = &obj {
                    if name == "lock" {
                        if !args.is_empty() {
                            return Err(self.rt(span, "lock takes no arguments"));
                        }
                        // A self-deadlock (this context already holds the
                        // mutex) is a catchable runtime error, not a hang.
                        let v = m.lock().map_err(|e| self.rt(span, e))?;
                        let mut guards = self.guards.lock().unwrap();
                        guards.push(crate::sched::MutexGuardToken::new(m.clone()));
                        self.guards_len.store(guards.len(), Ordering::Relaxed);
                        return Ok(v);
                    }
                    if name == "unlock" {
                        if !args.is_empty() {
                            return Err(self.rt(span, "unlock takes no arguments"));
                        }
                        m.unlock();
                        // The mutex was released explicitly: disarm this
                        // scope's token so the block-exit release does not
                        // unlock it a second time (which could free a lock
                        // another task now legitimately holds).
                        let mut guards = self.guards.lock().unwrap();
                        for g in guards.iter_mut() {
                            if let Some(held) = g.mutex() {
                                if std::sync::Arc::ptr_eq(held, m) {
                                    g.disarm();
                                }
                            }
                        }
                        return Ok(Value::Null);
                    }
                }
                if let Value::Instance(inst) = &obj {
                    let mut vals = Vec::with_capacity(args.len());
                    for a in args {
                        vals.push(self.eval(a, env)?);
                    }
                    return self.call_instance_method(inst, name, vals, span);
                }
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval(a, env)?);
                }
                let mut host = InterpCallbackHost {
                    interp: self,
                    span: span.clone(),
                };
                match collections::call_method_host(&mut host, &obj, name, vals) {
                    Ok(v) => Ok(v),
                    Err(msg) => {
                        // Callbacks and `task.join()` stash the user-thrown
                        // value behind their String error; rebind it here so
                        // `catch` sees the original value (audit A-9).
                        let mut d = Diagnostic::at(Phase::Runtime, span.clone(), msg)
                            .with_trace(self.trace_frames(span));
                        if let Some(v) = crate::sched::take_error_thrown() {
                            d.thrown = Some(v);
                        }
                        Err(Flow::Error(d))
                    }
                }
            }
            Expr::Index {
                object,
                index,
                span,
            } => {
                let obj = self.eval(object, env)?;
                let idx = self.eval(index, env)?;
                self.index_get(&obj, &idx, span)
            }
            Expr::Field { object, name, span } => {
                let obj = self.eval(object, env)?;
                self.field_get(&obj, name, span)
            }
            // A bare `super` is not a value (the checker rejects it); the
            // lowering turns every legal use into a `SuperCall`.
            // `super(...)` / `super.m(...)`: the parent class's constructor or
            // method, called on the current instance. The work lives in a
            // `#[inline(never)]` helper: inlining it into `eval` would grow
            // every expression frame, and the interpreter recurses once per
            // call level — the recursion stack budget is per-frame cost.
            Expr::SuperCall {
                parent,
                method,
                args,
                span,
            } => self.eval_super_call(parent, method, args, env, span),
            // A bare `super` is not a value (the checker rejects it); the
            // lowering turns every legal use into a `SuperCall`.
            Expr::Super { span } => Err(self.rt(
                span,
                "super must be called: write super(...) or super.method(...)",
            )),
            Expr::New { class, args, span } => {
                let c = match environment::get(env, class) {
                    Some(Value::Class(c)) => c,
                    Some(_) => return Err(self.rt(span, format!("'{}' is not a class", class))),
                    None => return Err(self.rt(span, format!("unknown class '{}'", class))),
                };
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval(a, env)?);
                }
                self.new_instance(&c, vals, span)
            }
            Expr::Spawn { callable, span } => {
                let f = self.eval(callable, env)?;
                let fd = match f {
                    Value::Function(fd) => fd,
                    other => {
                        return Err(self.rt(
                            span,
                            format!(
                                "spawn expects a function, found {}",
                                crate::value::type_name(&other)
                            ),
                        ))
                    }
                };
                let globals = self.globals.clone();
                let sched = self.sched.clone();
                let modules = self.modules.clone();
                let file = self.file.clone();
                let module_dirs = self.module_dirs.clone();
                let budget = self.budget.clone();
                let span2 = span.clone();
                let handle = self.sched.spawn_job(Box::new(move || {
                    let mut interp = Interp::shared(globals, sched, modules, file, module_dirs);
                    // The task draws from the same allowance as its spawner.
                    interp.set_budget(budget);
                    // A panic in the task releases the task's mutexes (see
                    // `GuardSweep`).
                    let _guard_sweep = GuardSweep {
                        interp: &interp,
                        mark: 0,
                    };
                    // Surface the user-thrown value across the task boundary
                    // so `join`/`await` rethrow is structural: `catch` on the
                    // joining side rebinds the original value, not its display
                    // string (audit A-9).
                    match interp.call_function(&fd, Vec::new(), &span2) {
                        Ok(v) => crate::sched::TaskOutcome {
                            result: Ok(v),
                            thrown: None,
                        },
                        Err(Flow::Error(d)) => crate::sched::TaskOutcome {
                            thrown: d.thrown.clone(),
                            result: Err(match &d.thrown {
                                Some(v) => crate::value::display_value(v),
                                None => d.message.clone(),
                            }),
                        },
                        // A fatal error stops the task with its message; the
                        // shared budget stays exhausted, so the task's joiner
                        // fails at its next charge site too.
                        Err(Flow::Fatal(d)) => crate::sched::TaskOutcome {
                            result: Err(d.message.clone()),
                            thrown: None,
                        },
                        Err(_) => crate::sched::TaskOutcome {
                            result: Err("invalid control flow in task".to_string()),
                            thrown: None,
                        },
                    }
                }));
                Ok(Value::Task(handle))
            }
            Expr::Await(inner, span) => {
                let v = self.eval(inner, env)?;
                match v {
                    Value::Task(t) => match t.join() {
                        Ok(v) => Ok(v),
                        Err(msg) => {
                            // A task that threw a user value rethrows it
                            // structurally so `catch` rebinds the original
                            // value, not a string (audit A-9).
                            let mut d = Diagnostic::at(
                                Phase::Runtime,
                                span.clone(),
                                format!("task failed: {}", msg),
                            )
                            .with_trace(self.trace_frames(span));
                            if let Some(v) = t.thrown_value() {
                                d.thrown = Some(v);
                            }
                            Err(Flow::Error(d))
                        }
                    },
                    Value::Channel(c) => Ok(c.recv()),
                    other => Err(self.rt(
                        span,
                        format!(
                            "await expects a task or a channel, found {}",
                            crate::value::type_name(&other)
                        ),
                    )),
                }
            }
        }
    }

    fn compound(&self, op: AssignOp, old: Value, new: Value, span: &Span) -> Result<Value, Flow> {
        let bin = match op {
            AssignOp::Assign => return Ok(new),
            AssignOp::AddAssign => BinOp::Add,
            AssignOp::SubAssign => BinOp::Sub,
            AssignOp::MulAssign => BinOp::Mul,
            AssignOp::DivAssign => BinOp::Div,
            AssignOp::ModAssign => BinOp::Mod,
        };
        self.eval_binary(bin, old, new, span)
    }

    pub fn eval_binary(&self, op: BinOp, l: Value, r: Value, span: &Span) -> Result<Value, Flow> {
        use Value::*;
        match op {
            BinOp::Add => match (&l, &r) {
                (Int(a), Int(b)) => Ok(Int(
                    crate::value::checked_int("+", *a, *b).map_err(|e| self.rt(span, e))?
                )),
                (Int(a), Float(b)) => Ok(Float(*a as f64 + b)),
                (Float(a), Int(b)) => Ok(Float(a + *b as f64)),
                (Float(a), Float(b)) => Ok(Float(a + b)),
                (Str(a), Str(b)) => {
                    let mut s = String::with_capacity(a.len() + b.len());
                    s.push_str(a);
                    s.push_str(b);
                    Ok(Str(s.into()))
                }
                _ => match self
                    .operator_call(&l, &r, "__add__", span)
                    .or_else(|| self.operator_call(&r, &l, "__radd__", span))
                {
                    Some(res) => res,
                    None => Err(self.rt(
                        span,
                        format!(
                            "cannot add {} and {}",
                            crate::value::type_name(&l),
                            crate::value::type_name(&r)
                        ),
                    )),
                },
            },
            BinOp::Sub => match (&l, &r) {
                (Int(a), Int(b)) => Ok(Int(
                    crate::value::checked_int("-", *a, *b).map_err(|e| self.rt(span, e))?
                )),
                (Int(a), Float(b)) => Ok(Float(*a as f64 - b)),
                (Float(a), Int(b)) => Ok(Float(a - *b as f64)),
                (Float(a), Float(b)) => Ok(Float(a - b)),
                _ => match self
                    .operator_call(&l, &r, "__sub__", span)
                    .or_else(|| self.operator_call(&r, &l, "__rsub__", span))
                {
                    Some(res) => res,
                    None => Err(self.rt(
                        span,
                        format!(
                            "cannot subtract {} from {}",
                            crate::value::type_name(&r),
                            crate::value::type_name(&l)
                        ),
                    )),
                },
            },
            BinOp::Mul => match (&l, &r) {
                (Int(a), Int(b)) => Ok(Int(
                    crate::value::checked_int("*", *a, *b).map_err(|e| self.rt(span, e))?
                )),
                (Int(a), Float(b)) => Ok(Float(*a as f64 * b)),
                (Float(a), Int(b)) => Ok(Float(a * *b as f64)),
                (Float(a), Float(b)) => Ok(Float(a * b)),
                _ => match self
                    .operator_call(&l, &r, "__mul__", span)
                    .or_else(|| self.operator_call(&r, &l, "__rmul__", span))
                {
                    Some(res) => res,
                    None => Err(self.rt(
                        span,
                        format!(
                            "cannot multiply {} and {}",
                            crate::value::type_name(&l),
                            crate::value::type_name(&r)
                        ),
                    )),
                },
            },
            BinOp::Div => match (&l, &r) {
                (Int(_), Int(b)) if *b == 0 => Err(self.rt(span, "division by zero")),
                (Int(a), Int(b)) => match a.checked_div(*b) {
                    Some(v) => Ok(Int(v)),
                    None => Err(self.rt(span, format!("integer overflow: {} / {}", a, b))),
                },
                (Int(a), Float(b)) => Ok(Float(*a as f64 / b)),
                (Float(a), Int(b)) => Ok(Float(a / *b as f64)),
                (Float(a), Float(b)) if *b == 0.0 => Err(self.rt(span, "division by zero")),
                (Float(a), Float(b)) => Ok(Float(a / b)),
                _ => match self
                    .operator_call(&l, &r, "__div__", span)
                    .or_else(|| self.operator_call(&r, &l, "__rdiv__", span))
                {
                    Some(res) => res,
                    None => Err(self.rt(
                        span,
                        format!(
                            "cannot divide {} by {}",
                            crate::value::type_name(&l),
                            crate::value::type_name(&r)
                        ),
                    )),
                },
            },
            BinOp::Mod => match (&l, &r) {
                (Int(_), Int(b)) if *b == 0 => Err(self.rt(span, "modulo by zero")),
                (Int(a), Int(b)) => Ok(Int(a.wrapping_rem(*b))),
                (Int(a), Float(b)) => Ok(Float(*a as f64 % b)),
                (Float(a), Int(b)) => Ok(Float(a % *b as f64)),
                (Float(a), Float(b)) if *b == 0.0 => Err(self.rt(span, "modulo by zero")),
                (Float(a), Float(b)) => Ok(Float(a % b)),
                _ => match self
                    .operator_call(&l, &r, "__mod__", span)
                    .or_else(|| self.operator_call(&r, &l, "__rmod__", span))
                {
                    Some(res) => res,
                    None => Err(self.rt(
                        span,
                        format!(
                            "cannot take {} mod {}",
                            crate::value::type_name(&l),
                            crate::value::type_name(&r)
                        ),
                    )),
                },
            },
            BinOp::Eq => Ok(Bool(crate::value::values_equal(&l, &r))),
            BinOp::Ne => Ok(Bool(!crate::value::values_equal(&l, &r))),
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
                    None => Err(self.rt(
                        span,
                        format!(
                            "cannot compare {} and {}",
                            crate::value::type_name(&l),
                            crate::value::type_name(&r)
                        ),
                    )),
                }
            }
        }
    }

    fn index_get(&self, obj: &Value, idx: &Value, span: &Span) -> Result<Value, Flow> {
        collections::index_get(obj, idx).map_err(|msg| self.rt(span, msg))
    }

    fn index_set(&self, obj: &Value, idx: &Value, v: Value, span: &Span) -> Result<(), Flow> {
        collections::index_set(obj, idx, v).map_err(|msg| self.rt(span, msg))
    }

    fn field_get(&self, obj: &Value, name: &str, span: &Span) -> Result<Value, Flow> {
        match obj {
            Value::Instance(inst) => {
                let ib = inst.lock().unwrap();
                match ib.fields.get(name) {
                    Some(v) => Ok(v.clone()),
                    None => Err(self.rt(
                        span,
                        format!("instance of {} has no field '{}'", ib.class.name, name),
                    )),
                }
            }
            Value::Map(m) => {
                let key = crate::value::MapKey::Str(name.into());
                let guard = m.lock().unwrap();
                match guard.find(&key) {
                    Some(i) => Ok(guard.entries[i].1.clone()),
                    None => Err(self.rt(span, format!("missing key '{}'", name))),
                }
            }
            other => Err(self.rt(span, crate::value::no_field_error(other, name))),
        }
    }

    fn field_set(&self, obj: &Value, name: &str, v: Value, span: &Span) -> Result<(), Flow> {
        match obj {
            Value::Instance(inst) => {
                let mut ib = inst.lock().unwrap();
                match ib.fields.get_mut(name) {
                    Some(slot) => {
                        *slot = v;
                        Ok(())
                    }
                    None => Err(self.rt(
                        span,
                        format!("instance of {} has no field '{}'", ib.class.name, name),
                    )),
                }
            }
            other => Err(self.rt(span, crate::value::no_field_error(other, name))),
        }
    }

    // --------------------------------------------------------------- calling

    /// Operator overloading: a value that carries a callable `__add__`
    /// (or `__sub__`/`__mul__`/`__div__`) field handles the operator.
    /// Maps (the stdlib numeric types) and class instances both qualify.
    /// `None` means "no overload": the caller reports its usual type error.
    fn operator_call(
        &self,
        l: &Value,
        r: &Value,
        method: &str,
        span: &Span,
    ) -> Option<Result<Value, Flow>> {
        match l {
            // A map carries the dunder as a value.
            Value::Map(_) => {
                let f = self.field_get(l, method, span).ok()?;
                if !matches!(
                    f,
                    Value::Function(_)
                        | Value::Native(_)
                        | Value::NativeClosure(_)
                        | Value::Class(_)
                ) {
                    return None;
                }
                Some(self.call_value(f, vec![l.clone(), r.clone()], span))
            }
            // A class instance carries it as a *method*, and `field_get` only
            // sees declared fields - so operator overloading on a class never
            // dispatched (the documented "a map or a class instance" was only
            // half true). Resolve the method through the class chain and call
            // it on the instance, exactly like `obj.op(x)`.
            Value::Instance(inst) => {
                let has = {
                    let ib = inst.lock().unwrap();
                    Self::find_method(&ib.class, method).is_some()
                };
                if !has {
                    return None;
                }
                Some(self.call_instance_method(inst, method, vec![r.clone()], span))
            }
            _ => None,
        }
    }

    /// Unary counterpart of [`Self::operator_call`]: `__neg__` receives just
    /// the receiver, not a right-hand operand.
    fn unary_operator_call(
        &self,
        v: &Value,
        method: &str,
        span: &Span,
    ) -> Option<Result<Value, Flow>> {
        match v {
            // A map carries the dunder as a value.
            Value::Map(_) => {
                let f = self.field_get(v, method, span).ok()?;
                if !matches!(
                    f,
                    Value::Function(_)
                        | Value::Native(_)
                        | Value::NativeClosure(_)
                        | Value::Class(_)
                ) {
                    return None;
                }
                Some(self.call_value(f, vec![v.clone()], span))
            }
            // A class instance carries it as a *method*, and `field_get` only
            // sees declared fields — the same defect the binary path above
            // documents. Resolve through the class chain and call it on the
            // instance, exactly like `obj.__neg__()` and the VM's
            // `try_unary_operator`.
            Value::Instance(inst) => {
                let has = {
                    let ib = inst.lock().unwrap();
                    Self::find_method(&ib.class, method).is_some()
                };
                if !has {
                    return None;
                }
                Some(self.call_instance_method(inst, method, Vec::new(), span))
            }
            _ => None,
        }
    }

    pub fn call_value(&self, f: Value, args: Vec<Value>, span: &Span) -> Result<Value, Flow> {
        match f {
            Value::Function(fd) => self.call_function(&fd, args, span),
            Value::Native(n) => {
                let ctx = crate::value::NativeCtx {
                    sched: self.sched.clone(),
                };
                (n.func)(&ctx, args).map_err(|msg| self.rt(span, msg))
            }
            Value::NativeClosure(n) => {
                let ctx = crate::value::NativeCtx {
                    sched: self.sched.clone(),
                };
                (n.func)(&ctx, args).map_err(|msg| self.rt(span, msg))
            }
            Value::Class(cd) => self.new_instance(&cd, args, span),
            other => Err(self.rt(
                span,
                format!("{} is not callable", crate::value::type_name(&other)),
            )),
        }
    }

    pub fn call_function(
        &self,
        fd: &FunctionData,
        args: Vec<Value>,
        span: &Span,
    ) -> Result<Value, Flow> {
        let depth = self.depth.fetch_add(1, Ordering::Relaxed);
        if depth >= MAX_CALL_DEPTH {
            self.depth.fetch_sub(1, Ordering::Relaxed);
            return Err(self.rt(span, "stack overflow: call depth limit reached"));
        }
        // Track the frame for error traces: (callee, call site).
        self.call_stack
            .lock()
            .unwrap()
            .push((TraceCallee::Func(fd.proto.name.clone()), span.clone()));
        let result = self.call_function_inner(fd, args, span);
        self.call_stack.lock().unwrap().pop();
        self.depth.fetch_sub(1, Ordering::Relaxed);
        result
    }

    /// Build an error trace from the current call stack, most recent frame
    /// first: the innermost function is shown at the error position, and every
    /// outer frame at the call site where it invoked the frame below it.
    fn trace_frames(&self, err_span: &Span) -> Vec<crate::diagnostics::TraceFrame> {
        let stack = self.call_stack.lock().unwrap();
        let mut out: Vec<crate::diagnostics::TraceFrame> = Vec::new();
        if let Some((callee, _)) = stack.last() {
            out.push(crate::diagnostics::TraceFrame {
                func: callee.to_name(),
                span: err_span.clone(),
            });
        }
        for k in (0..stack.len()).rev() {
            // The frame below sits at the call site of the frame above.
            if let Some((_, call_site)) = stack.get(k + 1) {
                out.push(crate::diagnostics::TraceFrame {
                    func: stack[k].0.to_name(),
                    span: call_site.clone(),
                });
            }
        }
        out
    }

    fn call_function_inner(
        &self,
        fd: &FunctionData,
        args: Vec<Value>,
        span: &Span,
    ) -> Result<Value, Flow> {
        if args.len() < fd.proto.min_arity() || args.len() > fd.proto.params.len() {
            let (min, max) = (fd.proto.min_arity(), fd.proto.params.len());
            let want = if min == max {
                format!("{} argument(s)", max)
            } else {
                format!("{} to {} argument(s)", min, max)
            };
            return Err(self.rt(
                span,
                format!(
                    "{} expects {}, got {}",
                    fd.proto.name.as_deref().unwrap_or("fn"),
                    want,
                    args.len()
                ),
            ));
        }
        let argc = args.len();
        let mut vars = HashMap::with_capacity(fd.proto.params.len());
        for (p, v) in fd.proto.params.iter().zip(args) {
            vars.insert(p.name.clone(), v);
        }
        // Fill omitted trailing parameters from their constant defaults. The
        // checker guarantees defaults are literals, so evaluation cannot touch
        // the environment.
        for p in fd.proto.params.iter().skip(argc) {
            if let Some(d) = &p.default {
                vars.insert(p.name.clone(), self.eval(d, &self.globals)?);
            }
        }
        let fenv = environment::child_with_vars(&fd.env, vars);
        let mark = self.guard_mark();
        let flow = self.exec_block(&fd.proto.body.stmts, &fenv);
        self.release_guards(mark);
        match flow {
            Flow::Value(v) => Ok(v),
            Flow::Return(v) => Ok(v),
            Flow::Break => Err(self.rt(span, "break outside a loop")),
            Flow::Continue => Err(self.rt(span, "continue outside a loop")),
            Flow::Error(d) => Err(Flow::Error(d)),
            Flow::Fatal(d) => Err(Flow::Fatal(d)),
        }
    }

    fn build_class(&self, decl: &ClassDecl, env: &Env) -> Arc<crate::value::ClassData> {
        let parent = decl.parent.as_ref().and_then(|p| {
            environment::get(env, p).and_then(|v| match v {
                Value::Class(c) => Some(c),
                _ => None,
            })
        });
        let mut methods = HashMap::new();
        for m in &decl.methods {
            if let Some(name) = &m.name {
                let fd = crate::value::new_function(m.clone(), env.clone());
                methods.insert(name.clone(), (m.vis, fd));
            }
        }
        let ctor = methods.get("new").map(|(_, f)| f.clone());
        crate::value::new_class(
            decl.name.clone(),
            parent,
            decl.interfaces.clone(),
            decl.fields
                .iter()
                .map(|f| (f.name.clone(), f.vis))
                .collect(),
            methods,
            ctor,
        )
    }

    /// Walk the class chain looking for a method.
    fn find_method(class: &Arc<crate::value::ClassData>, name: &str) -> Option<Arc<FunctionData>> {
        if let Some((_, fd)) = class.methods.get(name) {
            return Some(fd.clone());
        }
        match &class.parent {
            Some(p) => Self::find_method(p, name),
            None => None,
        }
    }

    pub fn new_instance(
        &self,
        class: &Arc<crate::value::ClassData>,
        args: Vec<Value>,
        span: &Span,
    ) -> Result<Value, Flow> {
        let mut fields = HashMap::new();
        // fields from the whole chain, defaulting to null
        let mut cur: Option<Arc<crate::value::ClassData>> = Some(class.clone());
        let mut chain: Vec<Arc<crate::value::ClassData>> = Vec::new();
        while let Some(c) = cur {
            cur = c.parent.clone();
            chain.push(c);
        }
        for c in chain.iter().rev() {
            for (fname, _) in &c.fields {
                fields.insert(fname.clone(), Value::Null);
            }
        }
        let instance = crate::value::new_ref(InstanceData {
            class: class.clone(),
            fields,
        });
        let obj = Value::Instance(instance.clone());
        // run the constructor (child first, then up the chain)
        let ctor = {
            let mut cur: Option<Arc<crate::value::ClassData>> = Some(class.clone());
            let mut found = None;
            while let Some(c) = cur {
                if let Some(fd) = &c.ctor {
                    found = Some(fd.clone());
                    break;
                }
                cur = c.parent.clone();
            }
            found
        };
        if let Some(ctor) = ctor {
            // Constructor parameters do not include self; self is bound here.
            if args.len() < ctor.proto.min_arity() || args.len() > ctor.proto.params.len() {
                let (min, max) = (ctor.proto.min_arity(), ctor.proto.params.len());
                let want = if min == max {
                    format!("{} argument(s)", max)
                } else {
                    format!("{} to {} argument(s)", min, max)
                };
                return Err(self.rt(
                    span,
                    format!("new {} expects {}, got {}", class.name, want, args.len()),
                ));
            }
            let depth = self.depth.fetch_add(1, Ordering::Relaxed);
            if depth >= MAX_CALL_DEPTH {
                self.depth.fetch_sub(1, Ordering::Relaxed);
                return Err(self.rt(span, "stack overflow: call depth limit reached"));
            }
            self.call_stack
                .lock()
                .unwrap()
                .push((TraceCallee::Ctor(class.clone()), span.clone()));
            let mut vars = HashMap::with_capacity(ctor.proto.params.len() + 1);
            vars.insert("self".to_string(), obj.clone());
            let argc = args.len();
            for (p, v) in ctor.proto.params.iter().zip(args) {
                vars.insert(p.name.clone(), v);
            }
            for p in ctor.proto.params.iter().skip(argc) {
                if let Some(d) = &p.default {
                    vars.insert(p.name.clone(), self.eval(d, &self.globals)?);
                }
            }
            let fenv = environment::child_with_vars(&ctor.env, vars);
            let mark = self.guard_mark();
            let flow = self.exec_block(&ctor.proto.body.stmts, &fenv);
            self.release_guards(mark);
            self.call_stack.lock().unwrap().pop();
            self.depth.fetch_sub(1, Ordering::Relaxed);
            match flow {
                Flow::Value(_) | Flow::Return(_) => {}
                Flow::Break => return Err(self.rt(span, "break outside a loop")),
                Flow::Continue => return Err(self.rt(span, "continue outside a loop")),
                Flow::Error(d) => return Err(Flow::Error(d)),
                Flow::Fatal(d) => return Err(Flow::Fatal(d)),
            }
        } else if !args.is_empty() {
            return Err(self.rt(
                span,
                format!(
                    "new {} expects 0 argument(s), got {}",
                    class.name,
                    args.len()
                ),
            ));
        }
        Ok(obj)
    }

    fn call_instance_method(
        &self,
        inst: &crate::value::Ref<InstanceData>,
        name: &str,
        args: Vec<Value>,
        span: &Span,
    ) -> Result<Value, Flow> {
        let class = { inst.lock().unwrap().class.clone() };
        self.call_class_method(&class, inst, name, args, span)
    }

    /// Evaluate a resolved `super` call. Kept out of line (see the match arm).
    #[inline(never)]
    fn eval_super_call(
        &self,
        parent: &str,
        method: &Option<String>,
        args: &[Expr],
        env: &Env,
        span: &Span,
    ) -> Result<Value, Flow> {
        let mut vals = Vec::with_capacity(args.len());
        for a in args {
            vals.push(self.eval(a, env)?);
        }
        self.call_super(parent, method.as_deref(), vals, env, span)
    }

    /// `super(...)` / `super.m(...)`.
    ///
    /// The receiver is the current instance (`self`); the target is the
    /// parent class resolved by the lowering. A constructor call runs the
    /// parent's constructor on the *same* instance (the instance already
    /// exists — it is being initialized), and a method call runs the parent's
    /// implementation with the same receiver.
    #[inline(never)]
    fn call_super(
        &self,
        parent: &str,
        method: Option<&str>,
        args: Vec<Value>,
        env: &environment::Env,
        span: &Span,
    ) -> Result<Value, Flow> {
        let recv = environment::get(env, "self");
        let inst = match recv {
            Some(Value::Instance(inst)) => inst,
            _ => return Err(self.rt(span, "super is only available inside a class method")),
        };
        if parent.is_empty() {
            return Err(self.rt(
                span,
                "super is not available: this class has no parent class",
            ));
        }
        // Resolve the parent in the *calling* scope first: a class defined in
        // an imported module lives in that module's globals, not in the
        // script's (which only receives the module's `public` names). Falling
        // back to `self.globals` keeps the script case working unchanged.
        let found =
            environment::get(env, parent).or_else(|| environment::get(&self.globals, parent));
        let class = match found {
            Some(Value::Class(c)) => c,
            Some(_) => return Err(self.rt(span, format!("'{}' is not a class", parent))),
            None => return Err(self.rt(span, format!("unknown class '{}'", parent))),
        };
        match method {
            Some(m) => self.call_class_method(&class, &inst, m, args, span),
            None => self.call_parent_constructor(&class, &inst, args, span),
        }
    }

    /// Run the parent class's constructor on the instance being initialized.
    fn call_parent_constructor(
        &self,
        class: &Arc<crate::value::ClassData>,
        inst: &crate::value::Ref<InstanceData>,
        args: Vec<Value>,
        span: &Span,
    ) -> Result<Value, Flow> {
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
            None => return Err(self.rt(span, format!("class '{}' has no constructor", class.name))),
        };
        // Constructor parameters do not include self; self is bound here.
        if args.len() < ctor.proto.min_arity() || args.len() > ctor.proto.params.len() {
            let (min, max) = (ctor.proto.min_arity(), ctor.proto.params.len());
            let want = if min == max {
                format!("{} argument(s)", max)
            } else {
                format!("{} to {} argument(s)", min, max)
            };
            return Err(self.rt(
                span,
                format!(
                    "constructor {}::new expects {}, got {}",
                    ctor_class_name,
                    want,
                    args.len()
                ),
            ));
        }
        let depth = self.depth.fetch_add(1, Ordering::Relaxed);
        if depth >= MAX_CALL_DEPTH {
            self.depth.fetch_sub(1, Ordering::Relaxed);
            return Err(self.rt(span, "stack overflow: call depth limit reached"));
        }
        self.call_stack
            .lock()
            .unwrap()
            .push((TraceCallee::Ctor(class.clone()), span.clone()));
        let mut vars = HashMap::with_capacity(ctor.proto.params.len() + 1);
        vars.insert("self".to_string(), Value::Instance(inst.clone()));
        let argc = args.len();
        for (p, v) in ctor.proto.params.iter().zip(args) {
            vars.insert(p.name.clone(), v);
        }
        for p in ctor.proto.params.iter().skip(argc) {
            if let Some(d) = &p.default {
                vars.insert(p.name.clone(), self.eval(d, &self.globals)?);
            }
        }
        let fenv = environment::child_with_vars(&ctor.env, vars);
        let mark = self.guard_mark();
        let flow = self.exec_block(&ctor.proto.body.stmts, &fenv);
        self.release_guards(mark);
        self.call_stack.lock().unwrap().pop();
        self.depth.fetch_sub(1, Ordering::Relaxed);
        match flow {
            Flow::Value(_) | Flow::Return(_) => Ok(Value::Null),
            Flow::Break => Err(self.rt(span, "break outside a loop")),
            Flow::Continue => Err(self.rt(span, "continue outside a loop")),
            Flow::Error(d) => Err(Flow::Error(d)),
            Flow::Fatal(d) => Err(Flow::Fatal(d)),
        }
    }

    /// Call a method of `class` (searching its inheritance chain) on `inst`.
    fn call_class_method(
        &self,
        class: &Arc<crate::value::ClassData>,
        inst: &crate::value::Ref<InstanceData>,
        name: &str,
        args: Vec<Value>,
        span: &Span,
    ) -> Result<Value, Flow> {
        let fd = match Self::find_method(class, name) {
            Some(fd) => fd,
            None => {
                return Err(self.rt(
                    span,
                    format!("class {} has no method '{}'", class.name, name),
                ))
            }
        };
        if args.len() + 1 < fd.proto.min_arity() || args.len() + 1 > fd.proto.params.len() {
            let (min, max) = (fd.proto.min_arity(), fd.proto.params.len());
            let want = if min == max {
                format!("{} argument(s)", max - 1)
            } else {
                format!("{} to {} argument(s)", min - 1, max - 1)
            };
            return Err(self.rt(
                span,
                format!("method {} expects {}, got {}", name, want, args.len()),
            ));
        }
        let depth = self.depth.fetch_add(1, Ordering::Relaxed);
        if depth >= MAX_CALL_DEPTH {
            self.depth.fetch_sub(1, Ordering::Relaxed);
            return Err(self.rt(span, "stack overflow: call depth limit reached"));
        }
        self.call_stack
            .lock()
            .unwrap()
            .push((TraceCallee::Method(class.clone(), fd.clone()), span.clone()));
        let obj = Value::Instance(inst.clone());
        let mut vars = HashMap::with_capacity(fd.proto.params.len());
        vars.insert("self".to_string(), obj);
        let argc = args.len();
        for (p, v) in fd.proto.params.iter().skip(1).zip(args) {
            vars.insert(p.name.clone(), v);
        }
        for p in fd.proto.params.iter().skip(1 + argc) {
            if let Some(d) = &p.default {
                vars.insert(p.name.clone(), self.eval(d, &self.globals)?);
            }
        }
        let fenv = environment::child_with_vars(&fd.env, vars);
        let mark = self.guard_mark();
        let flow = self.exec_block(&fd.proto.body.stmts, &fenv);
        self.release_guards(mark);
        self.call_stack.lock().unwrap().pop();
        self.depth.fetch_sub(1, Ordering::Relaxed);
        match flow {
            Flow::Value(v) | Flow::Return(v) => Ok(v),
            Flow::Break => Err(self.rt(span, "break outside a loop")),
            Flow::Continue => Err(self.rt(span, "continue outside a loop")),
            Flow::Error(d) => Err(Flow::Error(d)),
            Flow::Fatal(d) => Err(Flow::Fatal(d)),
        }
    }
}
