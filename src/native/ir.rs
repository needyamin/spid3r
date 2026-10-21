//! AST -> LLVM IR for the Spid3r native backend.
//!
//! Every Spid3r value in generated code is an opaque `ptr` (`*mut Value`).
//! User control flow (functions, loops, branches) is real machine code; every
//! operation that needs runtime semantics (arithmetic type rules, calls,
//! collections, methods, fields) is a call into the `extern "C"` trampolines
//! of [`crate::native::rt`], which delegate to the exact same code the
//! bytecode VM uses.
//!
//! Function ABI: `ptr @spid3r_fn.N(ptr %env, i64 %argc, ptr %argv) -> ptr`
//! - `env`   — closure environment (array of captured upvalue cells) or null,
//! - `argc`  — argument count (for diagnostics; arity is checked by callers),
//! - `argv`  — array of `*mut Value` argument handles (self at argv[0] for
//!   methods and constructors, matching the VM).
//!
//! Variable model (mirrors the bytecode VM):
//! - plain locals live in a stack slot holding a value handle,
//! - locals captured by a nested closure live in an upvalue cell
//!   (`*mut UpvalueSlot`) created at the declaration, so the closure and the
//!   defining frame share the exact same box,
//! - globals (top-level `let`/`const`/`fn`/`class`, natives) live in the
//!   runtime's global table and are re-read on every use.

use std::collections::{HashMap, HashSet};

use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::types::BasicType;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{FunctionValue, PointerValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use crate::ast::{
    AssignOp, BinOp, Block, CatchClause, ClassDecl, Expr, FnProto, LogOp, Program, Stmt, UnOp,
};
use crate::value::collections;

pub type IrResult<T> = Result<T, String>;

// ---------------------------------------------------------------------------
// scope model
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum VarLoc<'ctx> {
    /// Stack slot holding a value handle.
    Local(PointerValue<'ctx>),
    /// Stack slot holding an upvalue-cell pointer.
    Cell(PointerValue<'ctx>),
    /// Index of the captured cell inside this function's environment.
    Upvalue(usize),
}

struct Scope<'ctx> {
    vars: HashMap<String, VarLoc<'ctx>>,
}

struct LoopFrame<'ctx> {
    break_bb: BasicBlock<'ctx>,
    continue_bb: BasicBlock<'ctx>,
}

/// State for an enclosing `try`/`finally` region. When a `return`, `break`,
/// `continue`, or re-throw is compiled inside such a region, it is routed to
/// `finally_entry` (which runs the `finally` block and then dispatches on
/// `pending_exit`) instead of performing the exit directly — so `finally` runs
/// on every exit path, matching the tree/VM semantics.
#[derive(Clone)]
struct FinallyCtx<'ctx> {
    /// Block that runs the `finally` block, then dispatches on `pending_exit`.
    finally_entry: BasicBlock<'ctx>,
    /// Status code: 0 = normal fall-through, 1 = return, 2 = break,
    /// 3 = continue, 4 = re-throw `pending_thrown`.
    pending_exit: PointerValue<'ctx>,
    /// Return value, valid when `pending_exit == 1`.
    pending_ret: PointerValue<'ctx>,
    /// Thrown value, valid when `pending_exit == 4`.
    pending_thrown: PointerValue<'ctx>,
}

/// BinOp discriminant, matching `rt::binop_from_u8`.
fn binop_code(op: BinOp) -> u8 {
    match op {
        BinOp::Add => 0,
        BinOp::Sub => 1,
        BinOp::Mul => 2,
        BinOp::Div => 3,
        BinOp::Mod => 4,
        BinOp::Eq => 5,
        BinOp::Ne => 6,
        BinOp::Lt => 7,
        BinOp::Le => 8,
        BinOp::Gt => 9,
        BinOp::Ge => 10,
    }
}

fn unop_code(op: UnOp) -> u8 {
    match op {
        UnOp::Not => 0,
        UnOp::Neg => 1,
    }
}

/// Extract the pointer result of a call whose declared return type is `ptr`
/// (inkwell 0.10 idiom for unwrapping a `CallSiteValue`).
fn call_ptr<'a>(r: inkwell::values::CallSiteValue<'a>) -> PointerValue<'a> {
    match r.try_as_basic_value() {
        inkwell::values::ValueKind::Basic(b) => b.into_pointer_value(),
        _ => panic!("spid3r: runtime trampoline did not return a pointer"),
    }
}

/// Extract the integer (i1/i8/i64) result of a call.
fn call_int<'a>(r: inkwell::values::CallSiteValue<'a>) -> inkwell::values::IntValue<'a> {
    match r.try_as_basic_value() {
        inkwell::values::ValueKind::Basic(b) => b.into_int_value(),
        _ => panic!("spid3r: runtime trampoline did not return an integer"),
    }
}

// ---------------------------------------------------------------------------
// capture analysis
// ---------------------------------------------------------------------------

fn collect_outer_uses(
    stmts: &[Stmt],
    declared: &mut Vec<HashSet<String>>,
    out: &mut HashSet<String>,
) {
    for s in stmts {
        collect_outer_uses_stmt(s, declared, out);
    }
}

fn declare_name(declared: &mut [HashSet<String>], name: &str) {
    if let Some(top) = declared.last_mut() {
        top.insert(name.to_string());
    }
}

fn collect_outer_uses_stmt(
    s: &Stmt,
    declared: &mut Vec<HashSet<String>>,
    out: &mut HashSet<String>,
) {
    match s {
        // Removed by lowering; nothing to walk.
        Stmt::Enum(_) => {}
        Stmt::LetTuple { value, .. } => collect_outer_uses_expr(value, declared, out),
        Stmt::Let { name, value, .. } => {
            if let Some(v) = value {
                collect_outer_uses_expr(v, declared, out);
            }
            declare_name(declared, name);
        }
        Stmt::Const { name, value, .. } => {
            collect_outer_uses_expr(value, declared, out);
            declare_name(declared, name);
        }
        Stmt::Fn(p) => {
            if let Some(n) = &p.name {
                declare_name(declared, n);
            }
            collect_outer_uses_fn(p, declared, out);
        }
        Stmt::Class(c) => {
            for f in &c.fields {
                declare_name(declared, &f.name);
            }
            for m in &c.methods {
                collect_outer_uses_fn(m, declared, out);
            }
        }
        Stmt::Interface(_) | Stmt::Import { .. } => {}
        Stmt::If {
            cond,
            then,
            elifs,
            els,
            ..
        } => {
            collect_outer_uses_expr(cond, declared, out);
            collect_outer_uses(&then.stmts, declared, out);
            for (c, b) in elifs {
                collect_outer_uses_expr(c, declared, out);
                collect_outer_uses(&b.stmts, declared, out);
            }
            if let Some(b) = els {
                collect_outer_uses(&b.stmts, declared, out);
            }
        }
        Stmt::While { cond, body, .. } => {
            collect_outer_uses_expr(cond, declared, out);
            collect_outer_uses(&body.stmts, declared, out);
        }
        Stmt::ForIn {
            var, iter, body, ..
        } => {
            collect_outer_uses_expr(iter, declared, out);
            declare_name(declared, var);
            collect_outer_uses(&body.stmts, declared, out);
        }
        Stmt::ForC {
            init,
            cond,
            post,
            body,
            ..
        } => {
            collect_outer_uses_stmt(init, declared, out);
            collect_outer_uses_expr(cond, declared, out);
            collect_outer_uses_expr(post, declared, out);
            collect_outer_uses(&body.stmts, declared, out);
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                collect_outer_uses_expr(v, declared, out);
            }
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::Block(b) => collect_outer_uses(&b.stmts, declared, out),
        Stmt::Expr(e) => collect_outer_uses_expr(e, declared, out),
        Stmt::Spawn { callable, .. } => collect_outer_uses_expr(callable, declared, out),
        Stmt::Try {
            body,
            catch,
            finally,
            ..
        } => {
            collect_outer_uses(&body.stmts, declared, out);
            if let Some(c) = catch {
                declared.push(HashSet::from([c.name.clone()]));
                collect_outer_uses(&c.body.stmts, declared, out);
                declared.pop();
            }
            if let Some(f) = finally {
                collect_outer_uses(&f.stmts, declared, out);
            }
        }
        Stmt::Throw { value, .. } => collect_outer_uses_expr(value, declared, out),
    }
}

fn collect_outer_uses_expr(
    e: &Expr,
    declared: &mut Vec<HashSet<String>>,
    out: &mut HashSet<String>,
) {
    match e {
        // `super` itself references no names; its arguments might — and the
        // receiver is read implicitly, so a nested closure whose body uses
        // `super` must capture `self` like any other outer local (PAR-03).
        Expr::Super { .. } => {
            let local = declared.iter().any(|f| f.contains("self"));
            if !local {
                out.insert("self".to_string());
            }
        }
        Expr::SuperCall { args, .. } => {
            let local = declared.iter().any(|f| f.contains("self"));
            if !local {
                out.insert("self".to_string());
            }
            for a in args {
                collect_outer_uses_expr(a, declared, out);
            }
        }
        // Surface-only nodes (removed by lowering) — walk them anyway so a
        // capture inside one is never missed.
        Expr::If {
            cond, then, els, ..
        } => {
            collect_outer_uses_expr(cond, declared, out);
            collect_outer_uses(&then.stmts, declared, out);
            if let Some(b) = els {
                collect_outer_uses(&b.stmts, declared, out);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_outer_uses_expr(scrutinee, declared, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_outer_uses_expr(g, declared, out);
                }
                collect_outer_uses(&arm.body.stmts, declared, out);
            }
        }
        Expr::Propagate(inner, _) => collect_outer_uses_expr(inner, declared, out),
        Expr::OptField { object, .. } => collect_outer_uses_expr(object, declared, out),
        Expr::Coalesce { l, r, .. } => {
            collect_outer_uses_expr(l, declared, out);
            collect_outer_uses_expr(r, declared, out);
        }
        Expr::Ident(name, _) => {
            let local = declared.iter().any(|f| f.contains(name));
            if !local {
                out.insert(name.clone());
            }
        }
        Expr::Int(..) | Expr::Float(..) | Expr::Str(..) | Expr::Bool(..) | Expr::Null(_) => {}
        Expr::List(items, _) => {
            for i in items {
                collect_outer_uses_expr(i, declared, out);
            }
        }
        Expr::Fn(p) => collect_outer_uses_fn(p, declared, out),
        Expr::Unary { expr, .. } => collect_outer_uses_expr(expr, declared, out),
        Expr::Binary { l, r, .. } => {
            collect_outer_uses_expr(l, declared, out);
            collect_outer_uses_expr(r, declared, out);
        }
        Expr::Logical { l, r, .. } => {
            collect_outer_uses_expr(l, declared, out);
            collect_outer_uses_expr(r, declared, out);
        }
        Expr::Assign { target, value, .. } => {
            collect_outer_uses_expr(target, declared, out);
            collect_outer_uses_expr(value, declared, out);
        }
        Expr::Call { callee, args, .. } => {
            collect_outer_uses_expr(callee, declared, out);
            for a in args {
                collect_outer_uses_expr(a, declared, out);
            }
        }
        Expr::Method { object, args, .. } => {
            collect_outer_uses_expr(object, declared, out);
            for a in args {
                collect_outer_uses_expr(a, declared, out);
            }
        }
        Expr::Index { object, index, .. } => {
            collect_outer_uses_expr(object, declared, out);
            collect_outer_uses_expr(index, declared, out);
        }
        Expr::Field { object, .. } => collect_outer_uses_expr(object, declared, out),
        Expr::New { args, .. } => {
            for a in args {
                collect_outer_uses_expr(a, declared, out);
            }
        }
        Expr::Await(inner, _)
        | Expr::Spawn {
            callable: inner, ..
        } => collect_outer_uses_expr(inner, declared, out),
    }
}

fn collect_outer_uses_fn(
    p: &FnProto,
    declared: &mut Vec<HashSet<String>>,
    out: &mut HashSet<String>,
) {
    let frame = p.params.iter().map(|x| x.name.clone()).collect();
    declared.push(frame);
    collect_outer_uses(&p.body.stmts, declared, out);
    declared.pop();
}

/// Names declared anywhere inside `stmts` (any nesting depth), so we can tell
/// a local apart from an outer/global name.
fn collect_declared(stmts: &[Stmt], out: &mut HashSet<String>) {
    for s in stmts {
        collect_declared_stmt(s, out);
    }
}

fn collect_declared_stmt(s: &Stmt, out: &mut HashSet<String>) {
    match s {
        Stmt::Let { name, value, .. } => {
            if let Some(v) = value {
                collect_declared_expr(v, out);
            }
            out.insert(name.clone());
        }
        Stmt::Const { name, value, .. } => {
            collect_declared_expr(value, out);
            out.insert(name.clone());
        }
        Stmt::Fn(p) => {
            if let Some(n) = &p.name {
                out.insert(n.clone());
            }
            collect_declared(&p.body.stmts, out);
        }
        Stmt::Class(c) => {
            for f in &c.fields {
                out.insert(f.name.clone());
            }
            for m in &c.methods {
                collect_declared(&m.body.stmts, out);
            }
        }
        Stmt::If {
            then, elifs, els, ..
        } => {
            collect_declared(&then.stmts, out);
            for (_, b) in elifs {
                collect_declared(&b.stmts, out);
            }
            if let Some(b) = els {
                collect_declared(&b.stmts, out);
            }
        }
        Stmt::While { body, .. } => collect_declared(&body.stmts, out),
        Stmt::ForIn { var, body, .. } => {
            out.insert(var.clone());
            collect_declared(&body.stmts, out);
        }
        Stmt::ForC { init, body, .. } => {
            collect_declared_stmt(init, out);
            collect_declared(&body.stmts, out);
        }
        Stmt::Block(b) => collect_declared(&b.stmts, out),
        Stmt::Try {
            body,
            catch,
            finally,
            ..
        } => {
            collect_declared(&body.stmts, out);
            if let Some(c) = catch {
                out.insert(c.name.clone());
                collect_declared(&c.body.stmts, out);
            }
            if let Some(f) = finally {
                collect_declared(&f.stmts, out);
            }
        }
        _ => {}
    }
}

fn collect_declared_expr(e: &Expr, out: &mut HashSet<String>) {
    if let Expr::Fn(p) = e {
        if let Some(n) = &p.name {
            out.insert(n.clone());
        }
        collect_declared(&p.body.stmts, out);
    }
}

/// The names a function body captures from enclosing scopes, in a stable
/// order: the outer uses minus everything declared inside the function.
fn capture_list(p: &FnProto) -> Vec<String> {
    let mut uses: HashSet<String> = HashSet::new();
    let mut declared: Vec<HashSet<String>> =
        vec![p.params.iter().map(|x| x.name.clone()).collect()];
    collect_outer_uses(&p.body.stmts, &mut declared, &mut uses);
    let mut inner: HashSet<String> = HashSet::new();
    collect_declared(&p.body.stmts, &mut inner);
    let mut v: Vec<String> = uses.difference(&inner).cloned().collect();
    v.sort();
    v
}

// ---------------------------------------------------------------------------
// code generator
// ---------------------------------------------------------------------------

pub struct Ir<'ctx> {
    ctx: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    scopes: Vec<Scope<'ctx>>,
    loops: Vec<LoopFrame<'ctx>>,
    /// Stack of active exception-handler blocks. The top is where a pending
    /// `throw` in the current region routes to (a `try`'s handler, or — at the
    /// bottom — the function's propagate / entry's uncaught block).
    exc: Vec<BasicBlock<'ctx>>,
    /// Stack of active `try`/`finally` regions (for routing exits through
    /// `finally`); empty when not inside a `try` with a `finally` block.
    finally_stack: Vec<FinallyCtx<'ctx>>,
    ret_bb: Option<BasicBlock<'ctx>>,
    /// per-nesting-level return-value slots of the function being compiled
    ret_slots: Vec<PointerValue<'ctx>>,
    env_param: Option<PointerValue<'ctx>>,
    /// capture names of the function currently being compiled
    caps: Vec<String>,
    /// locals of the current function that live in upvalue cells
    boxed: Option<HashSet<String>>,
    /// true while compiling the module entry (top level = globals)
    is_entry: bool,
    strings: HashMap<String, PointerValue<'ctx>>,
    tramps: HashMap<String, FunctionValue<'ctx>>,
    tmp: usize,
    fn_counter: usize,
}

/// Signature of compiled Spid3r functions.
type FnTy<'ctx> = unsafe extern "C" fn(
    *mut core::ffi::c_void,
    u64,
    *const *mut crate::value::Value,
) -> *mut crate::value::Value;

impl<'ctx> Ir<'ctx> {
    pub fn new(ctx: &'ctx Context, module_name: &str) -> Self {
        Ir {
            ctx,
            module: ctx.create_module(module_name),
            builder: ctx.create_builder(),
            scopes: Vec::new(),
            loops: Vec::new(),
            exc: Vec::new(),
            finally_stack: Vec::new(),
            ret_bb: None,
            ret_slots: Vec::new(),
            env_param: None,
            caps: Vec::new(),
            boxed: None,
            is_entry: false,
            strings: HashMap::new(),
            tramps: HashMap::new(),
            tmp: 0,
            fn_counter: 0,
        }
    }

    pub fn module(&self) -> &Module<'ctx> {
        &self.module
    }

    // ------------------------------------------------------------- helpers

    fn err(&self, msg: impl Into<String>) -> String {
        msg.into()
    }

    fn tmp_name(&mut self, hint: &str) -> String {
        self.tmp += 1;
        format!("{}.{}", hint, self.tmp)
    }

    fn ptr_ty(&self) -> inkwell::types::PointerType<'ctx> {
        self.ctx.ptr_type(AddressSpace::default())
    }

    fn i8_ty(&self) -> inkwell::types::IntType<'ctx> {
        self.ctx.i8_type()
    }

    fn i64_ty(&self) -> inkwell::types::IntType<'ctx> {
        self.ctx.i64_type()
    }

    fn i1(&self, b: bool) -> inkwell::values::IntValue<'ctx> {
        self.ctx.bool_type().const_int(b as u64, false)
    }

    fn i64c(&self, v: i64) -> inkwell::values::IntValue<'ctx> {
        self.i64_ty().const_int(v as u64, false)
    }

    fn i8c(&self, v: u8) -> inkwell::values::IntValue<'ctx> {
        self.i8_ty().const_int(v as u64, false)
    }

    fn i32_ty(&self) -> inkwell::types::IntType<'ctx> {
        self.ctx.i32_type()
    }

    fn i32c(&self, v: i32) -> inkwell::values::IntValue<'ctx> {
        self.i32_ty().const_int(v as u64, false)
    }

    fn null_ptr(&self) -> PointerValue<'ctx> {
        self.ptr_ty().const_null()
    }

    /// Allocate a stack slot in the function's ENTRY block.
    ///
    /// Allocas emitted inside a loop body are executed again on every
    /// iteration and only reclaimed when the function returns, so a
    /// long-running loop would grow the stack forever until it overflows.
    /// Placing every slot in the entry block makes loops reuse one slot.
    fn alloca_entry(&mut self, name: &str) -> PointerValue<'ctx> {
        self.alloca_entry_ty(self.ptr_ty(), name)
    }

    fn alloca_entry_ty<T: inkwell::types::BasicType<'ctx>>(
        &mut self,
        ty: T,
        name: &str,
    ) -> PointerValue<'ctx> {
        let f = self.current_fn();
        let entry = f
            .get_first_basic_block()
            .expect("every function starts with an entry block");
        let cur = self.cur_block();
        // insert at the very start of the entry block (before everything,
        // including earlier slots — an alloca's position only affects frame
        // layout, and every use is dominated by it)
        match entry.get_first_instruction() {
            Some(first) => self.builder.position_before(&first),
            None => self.builder.position_at_end(entry),
        }
        let slot = self.builder.build_alloca(ty, name).unwrap();
        // restore the caller's insertion point
        match cur {
            Some(b) if b == entry => self.builder.position_at_end(entry),
            Some(b) => self.builder.position_at_end(b),
            None => {}
        }
        slot
    }

    /// Materialize compiled arguments as an LLVM `[N x ptr]` stack array and
    /// return a pointer to element 0 (a null pointer when `argv` is empty).
    fn argv_array(&mut self, argv: &[PointerValue<'ctx>]) -> PointerValue<'ctx> {
        if argv.is_empty() {
            return self.null_ptr();
        }
        let arr_ty = self.ptr_ty().array_type(argv.len() as u32);
        let slot = self.alloca_entry_ty(arr_ty, "argv");
        for (i, a) in argv.iter().enumerate() {
            let elem = unsafe {
                self.builder
                    .build_in_bounds_gep(
                        arr_ty,
                        slot,
                        &[self.i64c(0), self.i64c(i as i64)],
                        "argv.elem",
                    )
                    .unwrap()
            };
            self.builder.build_store(elem, *a).unwrap();
        }
        slot
    }

    // -------------------------------------------------------- trampolines

    /// Declare (or fetch) an extern trampoline from the runtime.
    fn tramp(
        &mut self,
        name: &str,
        params: &[BasicTypeEnum<'ctx>],
        ret: Option<BasicTypeEnum<'ctx>>,
    ) -> FunctionValue<'ctx> {
        if let Some(f) = self.tramps.get(name) {
            return *f;
        }
        let metas: Vec<inkwell::types::BasicMetadataTypeEnum> =
            params.iter().map(|t| (*t).into()).collect();
        let ft = match ret {
            Some(r) => r.fn_type(&metas, false),
            None => self.ctx.void_type().fn_type(&metas, false),
        };
        let f = self.module.add_function(name, ft, None);
        self.tramps.insert(name.to_string(), f);
        f
    }

    /// Emit a call with a fresh temporary name (borrow-friendly wrapper around
    /// `Builder::build_call`).
    fn emit_call(
        &mut self,
        f: FunctionValue<'ctx>,
        args: &[inkwell::values::BasicMetadataValueEnum<'ctx>],
        hint: &str,
    ) -> inkwell::values::CallSiteValue<'ctx> {
        let name = self.tmp_name(hint);
        self.builder.build_call(f, args, &name).unwrap()
    }

    fn call0p(&mut self, name: &str) -> PointerValue<'ctx> {
        let f = self.tramp(name, &[], Some(self.ptr_ty().into()));
        let r = self.emit_call(f, &[], "call");
        call_ptr(r)
    }

    fn call1p(&mut self, name: &str, a: PointerValue<'ctx>) -> PointerValue<'ctx> {
        let f = self.tramp(name, &[self.ptr_ty().into()], Some(self.ptr_ty().into()));
        let r = self.emit_call(f, &[a.into()], "call");
        call_ptr(r)
    }

    fn call1p_i64(&mut self, name: &str, a: PointerValue<'ctx>, n: i64) -> PointerValue<'ctx> {
        let f = self.tramp(
            name,
            &[self.ptr_ty().into(), self.i64_ty().into()],
            Some(self.ptr_ty().into()),
        );
        let n = self.i64c(n);
        let r = self.emit_call(f, &[a.into(), n.into()], "call");
        call_ptr(r)
    }

    fn call2p(
        &mut self,
        name: &str,
        a: PointerValue<'ctx>,
        b: PointerValue<'ctx>,
    ) -> PointerValue<'ctx> {
        let f = self.tramp(
            name,
            &[self.ptr_ty().into(), self.ptr_ty().into()],
            Some(self.ptr_ty().into()),
        );
        let r = self.emit_call(f, &[a.into(), b.into()], "call");
        call_ptr(r)
    }

    fn call3p(
        &mut self,
        name: &str,
        a: PointerValue<'ctx>,
        b: PointerValue<'ctx>,
        c: PointerValue<'ctx>,
    ) -> PointerValue<'ctx> {
        let f = self.tramp(
            name,
            &[
                self.ptr_ty().into(),
                self.ptr_ty().into(),
                self.ptr_ty().into(),
            ],
            Some(self.ptr_ty().into()),
        );
        let r = self.emit_call(f, &[a.into(), b.into(), c.into()], "call");
        call_ptr(r)
    }

    fn call_binop(
        &mut self,
        a: PointerValue<'ctx>,
        b: PointerValue<'ctx>,
        op: u8,
    ) -> PointerValue<'ctx> {
        let f = self.tramp(
            "spid3r_binop",
            &[
                self.ptr_ty().into(),
                self.ptr_ty().into(),
                self.i64_ty().into(),
            ],
            Some(self.ptr_ty().into()),
        );
        let op = self.i64c(op as i64);
        let r = self.emit_call(f, &[a.into(), b.into(), op.into()], "bin");
        call_ptr(r)
    }

    fn call_void1p(&mut self, name: &str, a: PointerValue<'ctx>) {
        let f = self.tramp(name, &[self.ptr_ty().into()], None);
        self.builder.build_call(f, &[a.into()], "").unwrap();
    }

    fn call_void2p(&mut self, name: &str, a: PointerValue<'ctx>, b: PointerValue<'ctx>) {
        let f = self.tramp(name, &[self.ptr_ty().into(), self.ptr_ty().into()], None);
        self.builder
            .build_call(f, &[a.into(), b.into()], "")
            .unwrap();
    }

    fn call_void3p(
        &mut self,
        name: &str,
        a: PointerValue<'ctx>,
        b: PointerValue<'ctx>,
        c: PointerValue<'ctx>,
    ) {
        let f = self.tramp(
            name,
            &[
                self.ptr_ty().into(),
                self.ptr_ty().into(),
                self.ptr_ty().into(),
            ],
            None,
        );
        self.builder
            .build_call(f, &[a.into(), b.into(), c.into()], "")
            .unwrap();
    }

    // -------------------------------------------------------- value makers

    fn v_int(&mut self, v: i64) -> PointerValue<'ctx> {
        let f = self.tramp(
            "spid3r_value_int",
            &[self.i64_ty().into()],
            Some(self.ptr_ty().into()),
        );
        let v = self.i64c(v);
        let r = self.emit_call(f, &[v.into()], "i");
        call_ptr(r)
    }

    fn v_float(&mut self, v: f64) -> PointerValue<'ctx> {
        let f = self.tramp(
            "spid3r_value_float",
            &[self.ctx.f64_type().into()],
            Some(self.ptr_ty().into()),
        );
        let v = self.ctx.f64_type().const_float(v);
        let r = self.emit_call(f, &[v.into()], "f");
        call_ptr(r)
    }

    fn v_bool(&mut self, v: bool) -> PointerValue<'ctx> {
        let f = self.tramp(
            "spid3r_value_bool",
            &[self.i8_ty().into()],
            Some(self.ptr_ty().into()),
        );
        let v = self.i8c(v as u8);
        let r = self.emit_call(f, &[v.into()], "b");
        call_ptr(r)
    }

    fn v_null(&mut self) -> PointerValue<'ctx> {
        self.call0p("spid3r_value_null")
    }

    fn v_str(&mut self, s: &str) -> PointerValue<'ctx> {
        let (gptr, len) = self.str_global(s);
        let f = self.tramp(
            "spid3r_value_str",
            &[self.ptr_ty().into(), self.i64_ty().into()],
            Some(self.ptr_ty().into()),
        );
        let len = self.i64c(len as i64);
        let r = self.emit_call(f, &[gptr.into(), len.into()], "s");
        call_ptr(r)
    }

    /// Add (or reuse) a string constant in the module; returns a pointer to its
    /// bytes and its length (the constant is NUL-terminated so the runtime can
    /// read it as a C string as well).
    fn str_global(&mut self, s: &str) -> (PointerValue<'ctx>, usize) {
        if let Some(p) = self.strings.get(s) {
            return (*p, s.len());
        }
        let bytes = s.as_bytes();
        let arr = self.ctx.const_string(bytes, true);
        let name = format!(".str.{}", self.strings.len());
        let g = self
            .module
            .add_global(arr.get_type(), Some(AddressSpace::default()), &name);
        g.set_initializer(&arr);
        g.set_constant(true);
        g.set_linkage(Linkage::Private);
        g.set_unnamed_addr(true);
        let p = g.as_pointer_value();
        self.strings.insert(s.to_string(), p);
        (p, bytes.len())
    }

    // -------------------------------------------------------- scopes

    fn push_scope(&mut self) {
        self.scopes.push(Scope {
            vars: HashMap::new(),
        });
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn declare(&mut self, name: &str, loc: VarLoc<'ctx>) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.vars.insert(name.to_string(), loc);
        }
    }

    fn lookup(&self, name: &str) -> Option<VarLoc<'ctx>> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.vars.get(name) {
                return Some(*v);
            }
        }
        None
    }

    // -------------------------------------------------------- control flow

    fn new_block(&self, name: &str) -> BasicBlock<'ctx> {
        let f = self.current_fn();
        self.ctx.append_basic_block(f, name)
    }

    fn current_fn(&self) -> FunctionValue<'ctx> {
        self.builder
            .get_insert_block()
            .unwrap()
            .get_parent()
            .unwrap()
    }

    fn cur_block(&self) -> Option<BasicBlock<'ctx>> {
        self.builder.get_insert_block()
    }

    fn br_to(&self, bb: BasicBlock<'ctx>) {
        if let Some(b) = self.cur_block() {
            if b.get_terminator().is_none() {
                self.builder.build_unconditional_branch(bb).unwrap();
            }
        }
    }

    /// True when the current basic block already has a terminator (so nothing
    /// more may be emitted into it).
    fn sealed(&self) -> bool {
        match self.cur_block() {
            Some(b) => b.get_terminator().is_some(),
            None => true,
        }
    }

    /// Ensure the builder sits in an open block: after `return`/`break`/
    /// `continue` the current block is sealed, so continue emission in a fresh
    /// (dead) block. LLVM prunes the unreachable code later.
    fn ensure_open(&mut self) {
        if self.sealed() {
            let bb = self.new_block("dead");
            self.builder.position_at_end(bb);
        }
    }

    /// Convert a value handle to an i1 condition.
    fn cond_i1(&mut self, v: PointerValue<'ctx>) -> inkwell::values::IntValue<'ctx> {
        let f = self.tramp(
            "spid3r_truthy",
            &[self.ptr_ty().into()],
            Some(self.i8_ty().into()),
        );
        let r = self.emit_call(f, &[v.into()], "t");
        let b = call_int(r);
        let zero = self.i8c(0);
        let cname = self.tmp_name("c");
        self.builder
            .build_int_compare(IntPredicate::NE, b, zero, &cname)
            .unwrap()
    }

    /// Load the current function's environment pointer.
    fn cur_env(&self) -> PointerValue<'ctx> {
        self.env_param.unwrap()
    }

    /// Read a captured upvalue cell: env slot -> cell -> value.
    fn read_upvalue(&mut self, index: usize) -> PointerValue<'ctx> {
        let env = self.cur_env();
        let slot = self.call_env_slot(env, index);
        let f = self.tramp(
            "spid3r_cell_get",
            &[self.ptr_ty().into()],
            Some(self.ptr_ty().into()),
        );
        let r = self.emit_call(f, &[slot.into()], "uv");
        call_ptr(r)
    }

    fn call_env_slot(&mut self, env: PointerValue<'ctx>, index: usize) -> PointerValue<'ctx> {
        let f = self.tramp(
            "spid3r_env_slot",
            &[self.ptr_ty().into(), self.i64_ty().into()],
            Some(self.ptr_ty().into()),
        );
        let n = self.i64c(index as i64);
        let r = self.emit_call(f, &[env.into(), n.into()], "slot");
        call_ptr(r)
    }
}

// ---------------------------------------------------------------------------
// nested-function walk (for the boxing analysis)
// ---------------------------------------------------------------------------

fn nested_fns_block<'a>(stmts: &'a [Stmt], out: &mut Vec<&'a FnProto>) {
    for s in stmts {
        nested_fns_stmt(s, out);
    }
}

fn nested_fns_stmt<'a>(s: &'a Stmt, out: &mut Vec<&'a FnProto>) {
    match s {
        Stmt::Fn(p) => {
            out.push(p);
            nested_fns_block(&p.body.stmts, out);
        }
        Stmt::Let { value, .. } => {
            if let Some(v) = value {
                nested_fns_expr(v, out);
            }
        }
        Stmt::Const { value, .. } => nested_fns_expr(value, out),
        Stmt::Class(c) => {
            for m in &c.methods {
                out.push(m);
                nested_fns_block(&m.body.stmts, out);
            }
        }
        Stmt::If {
            cond,
            then,
            elifs,
            els,
            ..
        } => {
            nested_fns_expr(cond, out);
            nested_fns_block(&then.stmts, out);
            for (c, b) in elifs {
                nested_fns_expr(c, out);
                nested_fns_block(&b.stmts, out);
            }
            if let Some(b) = els {
                nested_fns_block(&b.stmts, out);
            }
        }
        Stmt::While { cond, body, .. } => {
            nested_fns_expr(cond, out);
            nested_fns_block(&body.stmts, out);
        }
        Stmt::ForIn { iter, body, .. } => {
            nested_fns_expr(iter, out);
            nested_fns_block(&body.stmts, out);
        }
        Stmt::ForC {
            init,
            cond,
            post,
            body,
            ..
        } => {
            nested_fns_stmt(init, out);
            nested_fns_expr(cond, out);
            nested_fns_expr(post, out);
            nested_fns_block(&body.stmts, out);
        }
        Stmt::Return { value, .. } => {
            if let Some(v) = value {
                nested_fns_expr(v, out);
            }
        }
        Stmt::Block(b) => nested_fns_block(&b.stmts, out),
        Stmt::Expr(e) => nested_fns_expr(e, out),
        Stmt::Spawn { callable, .. } => nested_fns_expr(callable, out),
        Stmt::Try {
            body,
            catch,
            finally,
            ..
        } => {
            nested_fns_block(&body.stmts, out);
            if let Some(c) = catch {
                nested_fns_block(&c.body.stmts, out);
            }
            if let Some(f) = finally {
                nested_fns_block(&f.stmts, out);
            }
        }
        Stmt::Throw { value, .. } => nested_fns_expr(value, out),
        Stmt::Interface(_) | Stmt::Import { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::Enum(_) => {}
        Stmt::LetTuple { value, .. } => nested_fns_expr(value, out),
    }
}

fn nested_fns_expr<'a>(e: &'a Expr, out: &mut Vec<&'a FnProto>) {
    match e {
        Expr::Super { .. } => {}
        Expr::SuperCall { args, .. } => {
            for a in args {
                nested_fns_expr(a, out);
            }
        }
        Expr::If {
            cond, then, els, ..
        } => {
            nested_fns_expr(cond, out);
            nested_fns_block(&then.stmts, out);
            if let Some(b) = els {
                nested_fns_block(&b.stmts, out);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            nested_fns_expr(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    nested_fns_expr(g, out);
                }
                nested_fns_block(&arm.body.stmts, out);
            }
        }
        Expr::Propagate(inner, _) => nested_fns_expr(inner, out),
        Expr::OptField { object, .. } => nested_fns_expr(object, out),
        Expr::Coalesce { l, r, .. } => {
            nested_fns_expr(l, out);
            nested_fns_expr(r, out);
        }
        Expr::Fn(p) => {
            out.push(p);
            nested_fns_block(&p.body.stmts, out);
        }
        Expr::List(items, _) => {
            for i in items {
                nested_fns_expr(i, out);
            }
        }
        Expr::Unary { expr, .. } => nested_fns_expr(expr, out),
        Expr::Binary { l, r, .. } | Expr::Logical { l, r, .. } => {
            nested_fns_expr(l, out);
            nested_fns_expr(r, out);
        }
        Expr::Assign { target, value, .. } => {
            nested_fns_expr(target, out);
            nested_fns_expr(value, out);
        }
        Expr::Call { callee, args, .. } => {
            nested_fns_expr(callee, out);
            for a in args {
                nested_fns_expr(a, out);
            }
        }
        Expr::Method { object, args, .. } => {
            nested_fns_expr(object, out);
            for a in args {
                nested_fns_expr(a, out);
            }
        }
        Expr::Index { object, index, .. } => {
            nested_fns_expr(object, out);
            nested_fns_expr(index, out);
        }
        Expr::Field { object, .. } => nested_fns_expr(object, out),
        Expr::New { args, .. } => {
            for a in args {
                nested_fns_expr(a, out);
            }
        }
        Expr::Await(inner, _)
        | Expr::Spawn {
            callable: inner, ..
        } => nested_fns_expr(inner, out),
        Expr::Int(..)
        | Expr::Float(..)
        | Expr::Str(..)
        | Expr::Bool(..)
        | Expr::Null(_)
        | Expr::Ident(..) => {}
    }
}

/// Locals of `p` (params + declared) that are referenced from any nested
/// function at any depth: exactly the variables that must live in upvalue
/// cells instead of plain stack slots.
fn boxing_set(p: &FnProto) -> HashSet<String> {
    let mut locals: HashSet<String> = p.params.iter().map(|x| x.name.clone()).collect();
    collect_declared(&p.body.stmts, &mut locals);
    let mut nested: Vec<&FnProto> = Vec::new();
    nested_fns_block(&p.body.stmts, &mut nested);
    let mut needed: HashSet<String> = HashSet::new();
    for g in nested {
        for name in capture_list(g) {
            if locals.contains(&name) {
                needed.insert(name);
            }
        }
    }
    needed
}

// ---------------------------------------------------------------------------
// statement + expression compilation
// ---------------------------------------------------------------------------

impl<'ctx> Ir<'ctx> {
    fn unsupported(&self, what: &str) -> String {
        self.err(format!(
            "'{}' is not supported by the native backend yet; run it with --engine vm",
            what
        ))
    }

    fn compile_block(&mut self, stmts: &[Stmt]) -> IrResult<()> {
        self.push_scope();
        for s in stmts {
            self.ensure_open();
            self.compile_stmt(s)?;
        }
        self.pop_scope();
        Ok(())
    }

    fn compile_stmt(&mut self, s: &Stmt) -> IrResult<()> {
        match s {
            // `crate::lower` turns an enum into constructor functions.
            Stmt::LetTuple { span, .. } => Err(format!(
                "internal error at {}:{}: a destructuring `let` reached the native backend \
                 without lowering (this is a bug in spid3r)",
                span.file, span.line
            )),
            Stmt::Enum(e) => Err(format!(
                "internal error at {}:{}: an enum declaration reached the native backend \
                 without lowering (this is a bug in spid3r)",
                e.span.file, e.span.line
            )),
            Stmt::Let { name, value, .. } => {
                let v = match value {
                    Some(e) => self.compile_expr(e)?,
                    None => self.v_null(),
                };
                self.bind_declaration(name, v)?;
                Ok(())
            }
            Stmt::Const { name, value, .. } => {
                let v = self.compile_expr(value)?;
                self.bind_declaration(name, v)?;
                Ok(())
            }
            Stmt::Fn(p) => {
                // A named nested function is a local binding holding a closure.
                let v = self.compile_closure(p)?;
                if let Some(n) = &p.name {
                    self.bind_declaration(n, v)?;
                }
                Ok(())
            }
            Stmt::Class(_) | Stmt::Interface(_) => {
                // Defensive dead code (PAR-01 finding, 2026-10-22): the parser
                // rejects class/interface declarations inside any function
                // body on every engine (`unexpected keyword 'class'`), so a
                // Stmt::Class can never reach this arm. Kept as a guard in
                // case the grammar ever relaxes.
                Err(self.unsupported("class declaration inside a function"))
            }
            Stmt::Import { .. } => Err(self.unsupported("import (module resolution)")),
            Stmt::If {
                cond,
                then,
                elifs,
                els,
                ..
            } => {
                let then_bb = self.new_block("if.then");
                let else_bb = self.new_block("if.else");
                let end_bb = self.new_block("if.end");
                self.emit_cond_branch(cond, then_bb, else_bb)?;
                self.builder.position_at_end(then_bb);
                self.compile_block(&then.stmts)?;
                self.br_to(end_bb);

                self.builder.position_at_end(else_bb);
                for (c, b) in elifs {
                    let e_then = self.new_block("elif.then");
                    let e_else = self.new_block("elif.else");
                    self.emit_cond_branch(c, e_then, e_else)?;
                    self.builder.position_at_end(e_then);
                    self.compile_block(&b.stmts)?;
                    self.br_to(end_bb);
                    self.builder.position_at_end(e_else);
                }
                if let Some(b) = els {
                    self.compile_block(&b.stmts)?;
                }
                // `br_to` is a no-op in a sealed block, so this is safe even
                // when every arm returned/broke/continued.
                self.br_to(end_bb);
                self.builder.position_at_end(end_bb);
                Ok(())
            }
            Stmt::While { cond, body, .. } => {
                let cond_bb = self.new_block("while.cond");
                let body_bb = self.new_block("while.body");
                let end_bb = self.new_block("while.end");
                self.br_to(cond_bb);
                self.builder.position_at_end(cond_bb);
                self.emit_cond_branch(cond, body_bb, end_bb)?;
                self.builder.position_at_end(body_bb);
                self.loops.push(LoopFrame {
                    break_bb: end_bb,
                    continue_bb: cond_bb,
                });
                self.compile_block(&body.stmts)?;
                self.loops.pop();
                self.br_to(cond_bb);
                self.builder.position_at_end(end_bb);
                Ok(())
            }
            Stmt::ForIn {
                var, iter, body, ..
            } => {
                let obj = self.compile_expr(iter)?;
                // iterator state lives in a slot across the loop body
                let iter_slot = self.alloca_entry("forin.iter");
                let f = self.tramp(
                    "spid3r_forin_start",
                    &[self.ptr_ty().into()],
                    Some(self.ptr_ty().into()),
                );
                let it = self
                    .builder
                    .build_call(f, &[obj.into()], "forin.start")
                    .unwrap();
                let it = call_ptr(it);
                // `spid3r_forin_start` throws on a non-iterable object.
                self.throw_guard();
                self.builder.build_store(iter_slot, it).unwrap();

                let head_bb = self.new_block("forin.head");
                let body_bb = self.new_block("forin.body");
                let end_bb = self.new_block("forin.end");
                self.br_to(head_bb);

                self.builder.position_at_end(head_bb);
                let it = self
                    .builder
                    .build_load(self.ptr_ty(), iter_slot, "forin.it")
                    .unwrap()
                    .into_pointer_value();
                let f = self.tramp(
                    "spid3r_forin_next",
                    &[self.ptr_ty().into()],
                    Some(self.ptr_ty().into()),
                );
                let next = self
                    .builder
                    .build_call(f, &[it.into()], "forin.next")
                    .unwrap();
                let next = call_ptr(next);
                let is_end = self.builder.build_is_null(next, "forin.done").unwrap();
                self.builder
                    .build_conditional_branch(is_end, end_bb, body_bb)
                    .unwrap();

                self.builder.position_at_end(body_bb);
                self.push_scope();
                let forin_boxed = self
                    .boxed
                    .as_ref()
                    .map(|b| b.contains(var))
                    .unwrap_or(false);
                if forin_boxed {
                    let cell = self.call1p("spid3r_cell_new", next);
                    let slot = self.alloca_entry("forin.cell");
                    self.builder.build_store(slot, cell).unwrap();
                    self.declare(var, VarLoc::Cell(slot));
                } else {
                    let slot = self.temp_slot(next);
                    self.declare(var, VarLoc::Local(slot));
                }
                self.loops.push(LoopFrame {
                    break_bb: end_bb,
                    continue_bb: head_bb,
                });
                self.compile_block(&body.stmts)?;
                self.loops.pop();
                self.pop_scope();
                self.br_to(head_bb);

                self.builder.position_at_end(end_bb);
                let it = self
                    .builder
                    .build_load(self.ptr_ty(), iter_slot, "forin.it2")
                    .unwrap()
                    .into_pointer_value();
                self.call_void1p("spid3r_forin_end", it);
                Ok(())
            }
            Stmt::ForC {
                init,
                cond,
                post,
                body,
                ..
            } => {
                self.compile_stmt(init)?;
                let cond_bb = self.new_block("forc.cond");
                let body_bb = self.new_block("forc.body");
                let post_bb = self.new_block("forc.post");
                let end_bb = self.new_block("forc.end");
                self.br_to(cond_bb);
                self.builder.position_at_end(cond_bb);
                self.emit_cond_branch(cond, body_bb, end_bb)?;
                self.builder.position_at_end(body_bb);
                self.loops.push(LoopFrame {
                    break_bb: end_bb,
                    continue_bb: post_bb,
                });
                self.compile_block(&body.stmts)?;
                self.loops.pop();
                self.br_to(post_bb);
                self.builder.position_at_end(post_bb);
                self.compile_expr(post)?;
                self.br_to(cond_bb);
                self.builder.position_at_end(end_bb);
                Ok(())
            }
            Stmt::Return { value, .. } => {
                let v = match value {
                    Some(e) => self.compile_expr(e)?,
                    None => self.v_null(),
                };
                if !self.finally_stack.is_empty() {
                    // Route through `finally` so it runs before the function
                    // returns (mirrors the tree/VM `finally` semantics).
                    self.route_to_finally(1, Some(v));
                } else {
                    self.store_return(v);
                }
                Ok(())
            }
            Stmt::Break { .. } => {
                if !self.finally_stack.is_empty() {
                    self.route_to_finally(2, None);
                    return Ok(());
                }
                let bb = self
                    .loops
                    .last()
                    .map(|f| f.break_bb)
                    .ok_or_else(|| self.err("'break' outside of a loop"))?;
                self.br_to(bb);
                Ok(())
            }
            Stmt::Continue { .. } => {
                if !self.finally_stack.is_empty() {
                    self.route_to_finally(3, None);
                    return Ok(());
                }
                let bb = self
                    .loops
                    .last()
                    .map(|f| f.continue_bb)
                    .ok_or_else(|| self.err("'continue' outside of a loop"))?;
                self.br_to(bb);
                Ok(())
            }
            Stmt::Block(b) => self.compile_block(&b.stmts),
            Stmt::Expr(e) => {
                self.compile_expr(e)?;
                Ok(())
            }
            Stmt::Spawn { callable, .. } => {
                let f = self.compile_expr(callable)?;
                self.call1p("spid3r_spawn", f);
                // `spid3r_spawn` sets the throw flag on a non-callable argument.
                self.throw_guard();
                Ok(())
            }
            Stmt::Try {
                body,
                catch,
                finally,
                ..
            } => self.compile_try(body, catch.as_ref(), finally.as_ref()),
            Stmt::Throw { value, .. } => {
                let v = self.compile_expr(value)?;
                self.throw_value(v);
                // Route to the enclosing handler (or propagate up when there is
                // no enclosing `try`); the rest of the region is abandoned.
                self.throw_guard();
                Ok(())
            }
        }
    }

    /// Compile `try { body } catch (name) { catch } finally { finally }`.
    ///
    /// `finally` runs on every exit (normal completion, `return`, `break`,
    /// `continue`, a caught exception, and a re-thrown exception), matching the
    /// tree/VM engines. Control flow:
    ///
    /// - The try body is compiled inline with `handler_bb` pushed at the top of
    ///   `exc`, so any operation that throws (or an inline `throw`) branches
    ///   straight to it via the `spid3r_threw` guard — skipping the rest of the
    ///   body exactly like unwinding would, but as compiled control flow.
    /// - `handler_bb` consumes the thrown value; a `catch` (if present) runs with
    ///   the value bound to the catch name, otherwise the exception stays
    ///   pending. `return`/`break`/`continue`/throws inside the body or catch
    ///   route through the shared `finally` dispatch ([`Self::route_to_finally`]).
    /// - The `finally` block runs on every path; afterwards the dispatch
    ///   performs the still-pending exit (return / break / continue / re-throw to
    ///   the enclosing handler) or falls through past the `try`.
    fn compile_try(
        &mut self,
        body: &Block,
        catch: Option<&CatchClause>,
        finally: Option<&Block>,
    ) -> IrResult<()> {
        let has_finally = finally.is_some();
        let has_catch = catch.is_some();
        // The handler a re-throw from *this* try should reach: the enclosing
        // try's handler, or the current function's propagate / uncaught frame.
        let outer_handler = *self
            .exc
            .last()
            .expect("compile_try with no active handler frame");

        let after_bb = self.new_block("try.after");

        // finally bookkeeping (only allocated when a `finally` block exists).
        let (finally_entry, dispatch_bb, pend_exit, pend_ret, pend_thrown) = if has_finally {
            let fe = self.new_block("try.finally");
            let db = self.new_block("try.dispatch");
            let pe = self.alloca_entry_ty(self.i32_ty(), "pend_exit");
            self.builder.build_store(pe, self.i32c(0)).unwrap();
            let pr = self.alloca_entry("pend_ret");
            let pt = self.alloca_entry("pend_thrown");
            (Some(fe), Some(db), Some(pe), Some(pr), Some(pt))
        } else {
            (None, None, None, None, None)
        };

        let body_bb = self.new_block("try.body");
        let handler_bb = self.new_block("try.handler");
        self.br_to(body_bb);

        // ---- try body (throwing ops route to handler_bb) ----
        self.builder.position_at_end(body_bb);
        if has_finally {
            self.finally_stack.push(FinallyCtx {
                finally_entry: finally_entry.unwrap(),
                pending_exit: pend_exit.unwrap(),
                pending_ret: pend_ret.unwrap(),
                pending_thrown: pend_thrown.unwrap(),
            });
        }
        self.exc.push(handler_bb);
        self.push_scope();
        self.compile_block(&body.stmts)?;
        self.pop_scope();
        self.exc.pop();
        // normal completion of the body (the throw flag is clear here)
        if has_finally {
            self.finally_stack.pop();
            self.br_to(finally_entry.unwrap()); // pending_exit stays 0 (normal)
        } else {
            self.br_to(after_bb);
        }

        // ---- exception handler (throw flag set by a guarded op) ----
        self.builder.position_at_end(handler_bb);
        if has_catch {
            let cc = catch.unwrap();
            let thrown = self.take_thrown(); // consume the caught value
            if has_finally {
                // during catch, a throw / return / break / continue routes
                // through THIS finally (then propagates outward).
                let catch_fh = self.new_block("try.catchfh");
                self.finally_stack.push(FinallyCtx {
                    finally_entry: finally_entry.unwrap(),
                    pending_exit: pend_exit.unwrap(),
                    pending_ret: pend_ret.unwrap(),
                    pending_thrown: pend_thrown.unwrap(),
                });
                self.exc.push(catch_fh);
                self.push_scope();
                let slot = self.alloca_entry("catchvar");
                self.builder.build_store(slot, thrown).unwrap();
                self.declare(&cc.name, VarLoc::Local(slot));
                self.compile_block(&cc.body.stmts)?;
                self.pop_scope();
                self.exc.pop();
                self.finally_stack.pop();
                self.br_to(finally_entry.unwrap()); // catch handled it (pending 0)
                                                    // the catch itself threw: re-throw after finally
                self.builder.position_at_end(catch_fh);
                let nt = self.take_thrown();
                self.builder.build_store(pend_thrown.unwrap(), nt).unwrap();
                self.builder
                    .build_store(pend_exit.unwrap(), self.i32c(4))
                    .unwrap();
                self.br_to(finally_entry.unwrap());
            } else {
                self.push_scope();
                let slot = self.alloca_entry("catchvar");
                self.builder.build_store(slot, thrown).unwrap();
                self.declare(&cc.name, VarLoc::Local(slot));
                self.compile_block(&cc.body.stmts)?;
                self.pop_scope();
                self.br_to(after_bb);
            }
        } else if has_finally {
            // no catch: run `finally`, then re-throw the pending value.
            let thrown = self.take_thrown();
            self.builder
                .build_store(pend_thrown.unwrap(), thrown)
                .unwrap();
            self.builder
                .build_store(pend_exit.unwrap(), self.i32c(4))
                .unwrap();
            self.br_to(finally_entry.unwrap());
        } else {
            // no catch, no finally: propagate to the enclosing handler. The
            // throw flag still holds the value; that handler will take it.
            self.br_to(outer_handler);
        }

        // ---- finally block + dispatch on pending_exit ----
        if has_finally {
            self.builder.position_at_end(finally_entry.unwrap());
            self.push_scope();
            if let Some(fb) = finally {
                self.compile_block(&fb.stmts)?;
            }
            self.pop_scope();
            self.br_to(dispatch_bb.unwrap());

            self.builder.position_at_end(dispatch_bb.unwrap());
            let pe = self
                .builder
                .build_load(self.i32_ty(), pend_exit.unwrap(), "pe")
                .unwrap()
                .into_int_value();
            let bb_ret = self.new_block("try.ret");
            let bb_brk = self.new_block("try.brk");
            let bb_cont = self.new_block("try.cont");
            let bb_re = self.new_block("try.rethrow");
            let cases = [
                (self.i32c(1), bb_ret),
                (self.i32c(2), bb_brk),
                (self.i32c(3), bb_cont),
                (self.i32c(4), bb_re),
            ];
            self.builder.build_switch(pe, after_bb, &cases).unwrap();

            // return -> store the pending value and return from the function
            self.builder.position_at_end(bb_ret);
            let v = self
                .builder
                .build_load(self.ptr_ty(), pend_ret.unwrap(), "rv")
                .unwrap()
                .into_pointer_value();
            self.store_return(v);

            // break -> enclosing loop's break target. A `break`/`continue` that
            // routes here is only reachable when an enclosing loop exists (the
            // semantic pass rejects them otherwise), so `unreachable` is a dead
            // arm when there is none.
            self.builder.position_at_end(bb_brk);
            match self.loops.last() {
                Some(f) => self.br_to(f.break_bb),
                None => {
                    self.builder.build_unreachable().unwrap();
                }
            }

            // continue -> enclosing loop's continue target
            self.builder.position_at_end(bb_cont);
            match self.loops.last() {
                Some(f) => self.br_to(f.continue_bb),
                None => {
                    self.builder.build_unreachable().unwrap();
                }
            }

            // re-throw the pending value to the enclosing handler
            self.builder.position_at_end(bb_re);
            let v = self
                .builder
                .build_load(self.ptr_ty(), pend_thrown.unwrap(), "rv")
                .unwrap()
                .into_pointer_value();
            self.throw_value(v); // (re)set the throw flag
            self.br_to(outer_handler); // route to the enclosing handler
        }

        self.builder.position_at_end(after_bb);
        Ok(())
    }

    /// Route a `return`/`break`/`continue`/re-throw through the active `finally`
    /// dispatch (when inside a `try` with a `finally` block). `exit` is the
    /// `pending_exit` code (1=return, 2=break, 3=continue, 4=rethrow).
    fn route_to_finally(&mut self, exit: i32, value: Option<PointerValue<'ctx>>) {
        let ctx = self
            .finally_stack
            .last()
            .expect("finally routing without an active try/finally")
            .clone();
        if let Some(v) = value {
            let slot = if exit == 1 {
                ctx.pending_ret
            } else {
                ctx.pending_thrown
            };
            self.builder.build_store(slot, v).unwrap();
        }
        self.builder
            .build_store(ctx.pending_exit, self.i32c(exit))
            .unwrap();
        self.br_to(ctx.finally_entry);
    }

    /// Emit a call to `spid3r_throw`, which records the value in the pending
    /// throw flag. It returns normally; the following `spid3r_threw` guard (or
    /// the statement terminator) routes control to the current handler.
    fn throw_value(&mut self, v: PointerValue<'ctx>) {
        let fv = self.tramp("spid3r_throw", &[self.ptr_ty().into()], None);
        self.builder.build_call(fv, &[v.into()], "").unwrap();
    }

    /// After an operation that may have set the pending throw flag, branch to
    /// the current handler block. With no enclosing `try`, the bottom of `exc`
    /// is the function's propagate block (an early `return`), so a throw flows
    /// up to the nearest enclosing `try` — or, at top level, to the uncaught
    /// handler. The value produced by the operation is only used on the
    /// continuation, so a throw correctly abandons the rest of the region.
    fn throw_guard(&mut self) {
        let Some(handler) = self.exc.last().copied() else {
            return;
        };
        let f = self.tramp("spid3r_threw", &[], Some(self.i8_ty().into()));
        let r = self.emit_call(f, &[], "threw");
        let b = call_int(r); // i8
        let cname = self.tmp_name("exc");
        let is = self
            .builder
            .build_int_compare(IntPredicate::NE, b, self.i8c(0), &cname)
            .unwrap();
        let cont = self.new_block("ok");
        if let Some(cur) = self.cur_block() {
            if cur.get_terminator().is_none() {
                self.builder
                    .build_conditional_branch(is, handler, cont)
                    .unwrap();
                self.builder.position_at_end(cont);
            }
        }
    }

    /// Fetch (and clear) the pending thrown value from the runtime.
    fn take_thrown(&mut self) -> PointerValue<'ctx> {
        self.call0p("spid3r_take_thrown")
    }

    /// A temporary stack slot holding `v` (for loop variables etc.).
    fn temp_slot(&mut self, v: PointerValue<'ctx>) -> PointerValue<'ctx> {
        let slot = self.alloca_entry("tmp");
        self.builder.build_store(slot, v).unwrap();
        slot
    }

    fn store_return(&mut self, v: PointerValue<'ctx>) {
        // The current function must have a return slot.
        let slot = *self.ret_slots.last().expect("return outside of a function");
        self.builder.build_store(slot, v).unwrap();
        let bb = self.ret_bb.expect("return outside of a function");
        self.br_to(bb);
    }

    fn emit_cond_branch(
        &mut self,
        cond: &Expr,
        then_bb: BasicBlock<'ctx>,
        else_bb: BasicBlock<'ctx>,
    ) -> IrResult<()> {
        let v = self.compile_expr(cond)?;
        let c = self.cond_i1(v);
        self.builder
            .build_conditional_branch(c, then_bb, else_bb)
            .unwrap();
        Ok(())
    }

    /// Bind a `let`/`const` in the current scope (global at entry top level,
    /// local otherwise; captured locals get a cell).
    fn bind_declaration(&mut self, name: &str, v: PointerValue<'ctx>) -> IrResult<()> {
        let in_entry_root = self.is_entry && self.scopes.len() == 1;
        if in_entry_root {
            let (np, _) = self.str_global(name);
            let f = self.tramp(
                "spid3r_global_set",
                &[self.ptr_ty().into(), self.ptr_ty().into()],
                None,
            );
            self.builder
                .build_call(f, &[np.into(), v.into()], "")
                .unwrap();
            return Ok(());
        }
        let boxed = self
            .boxed
            .as_ref()
            .map(|b| b.contains(name))
            .unwrap_or(false);
        if boxed {
            let cell = self.call1p("spid3r_cell_new", v);
            let slot = self.alloca_entry("cell");
            self.builder.build_store(slot, cell).unwrap();
            self.declare(name, VarLoc::Cell(slot));
        } else {
            let slot = self.alloca_entry("l");
            self.builder.build_store(slot, v).unwrap();
            self.declare(name, VarLoc::Local(slot));
        }
        Ok(())
    }
}

impl<'ctx> Ir<'ctx> {
    fn compile_expr(&mut self, e: &Expr) -> IrResult<PointerValue<'ctx>> {
        let v = self.compile_expr_inner(e)?;
        // Any operation that threw set the pending-throw flag; route to the
        // current handler instead of using this (placeholder) value further.
        self.throw_guard();
        Ok(v)
    }

    fn compile_expr_inner(&mut self, e: &Expr) -> IrResult<PointerValue<'ctx>> {
        match e {
            // `super` is resolved to a `SuperCall` by `crate::lower`; the
            // checker rejects a bare value-position `super` before codegen,
            // so reaching this arm is a pipeline bug, not a user error.
            Expr::Super { span } => Err(format!(
                "internal error at {}:{}: a bare `super` reached the native backend \
                 (the checker rejects it first; this is a bug in spid3r)",
                span.file, span.line
            )),
            Expr::SuperCall {
                parent,
                method,
                args,
                ..
            } => {
                // The receiver is the executing frame's `self` (argv[0] of
                // every compiled method/constructor); the parent class and
                // method names travel as constants and the runtime trampoline
                // `spid3r_super_call` resolves the parent in the globals —
                // the same resolution tree/vm perform.
                let recv = self.compile_ident("self")?;
                let mut argv: Vec<PointerValue<'ctx>> = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.compile_expr(a)?);
                }
                let arr = self.argv_array(&argv);
                let (pp, _) = self.str_global(parent);
                let mp = match method {
                    Some(m) => self.str_global(m).0,
                    None => self.null_ptr(),
                };
                let f = self.tramp(
                    "spid3r_super_call",
                    &[
                        self.ptr_ty().into(),
                        self.ptr_ty().into(),
                        self.ptr_ty().into(),
                        self.i64_ty().into(),
                        self.ptr_ty().into(),
                    ],
                    Some(self.ptr_ty().into()),
                );
                let n = self.i64c(args.len() as i64);
                let sname = self.tmp_name("sup");
                let r = self
                    .builder
                    .build_call(
                        f,
                        &[recv.into(), pp.into(), mp.into(), n.into(), arr.into()],
                        &sname,
                    )
                    .unwrap();
                Ok(call_ptr(r))
            }
            // Surface-only nodes are rewritten by `crate::lower` before codegen.
            Expr::If { span, .. }
            | Expr::OptField { span, .. }
            | Expr::Coalesce { span, .. }
            | Expr::Match { span, .. }
            | Expr::Propagate(_, span) => Err(format!(
                "internal error at {}:{}: a surface-only expression reached the native \
                     backend without lowering (this is a bug in spid3r)",
                span.file, span.line
            )),
            Expr::Int(v, _) => Ok(self.v_int(*v)),
            Expr::Float(v, _) => Ok(self.v_float(*v)),
            Expr::Bool(v, _) => Ok(self.v_bool(*v)),
            Expr::Null(_) => Ok(self.v_null()),
            Expr::Str(s, _) => Ok(self.v_str(s)),
            Expr::Ident(name, _) => self.compile_ident(name),
            Expr::List(items, _) => {
                let mut argv: Vec<PointerValue<'ctx>> = Vec::with_capacity(items.len());
                for i in items {
                    argv.push(self.compile_expr(i)?);
                }
                let arr = self.argv_array(&argv);
                let (np, _) = self.str_global("list");
                let f = self.tramp(
                    "spid3r_construct",
                    &[
                        self.ptr_ty().into(),
                        self.i64_ty().into(),
                        self.ptr_ty().into(),
                    ],
                    Some(self.ptr_ty().into()),
                );
                let n = self.i64c(items.len() as i64);
                let cname = self.tmp_name("ctor");
                let r = self
                    .builder
                    .build_call(f, &[np.into(), n.into(), arr.into()], &cname)
                    .unwrap();
                Ok(call_ptr(r))
            }
            Expr::Fn(p) => self.compile_closure(p),
            Expr::Unary { op, expr, .. } => {
                let v = self.compile_expr(expr)?;
                Ok(self.call1p_i64("spid3r_unop", v, unop_code(*op) as i64))
            }
            Expr::Binary { op, l, r, .. } => {
                let a = self.compile_expr(l)?;
                let b = self.compile_expr(r)?;
                Ok(self.call_binop(a, b, binop_code(*op)))
            }
            Expr::Logical { op, l, r, .. } => {
                // short-circuit, like the VM
                let lhs = self.compile_expr(l)?;
                let c = self.cond_i1(lhs);
                let lhs_from = self.cur_block().unwrap();
                let rhs_bb = self.new_block("logic.rhs");
                let end_bb = self.new_block("logic.end");
                match op {
                    LogOp::And => {
                        self.builder
                            .build_conditional_branch(c, rhs_bb, end_bb)
                            .unwrap();
                    }
                    LogOp::Or => {
                        self.builder
                            .build_conditional_branch(c, end_bb, rhs_bb)
                            .unwrap();
                    }
                }
                self.builder.position_at_end(rhs_bb);
                let rhs = self.compile_expr(r)?;
                let rhs_from = self.cur_block().unwrap();
                self.br_to(end_bb);
                self.builder.position_at_end(end_bb);
                let pname = self.tmp_name("logic");
                let phi = self.builder.build_phi(self.ptr_ty(), &pname).unwrap();
                phi.add_incoming(&[(&lhs, lhs_from), (&rhs, rhs_from)]);
                Ok(phi.as_basic_value().into_pointer_value())
            }
            Expr::Assign {
                target, op, value, ..
            } => self.compile_assign(target, *op, value),
            Expr::Call { callee, args, .. } => self.compile_call(callee, args),
            Expr::Method {
                object, name, args, ..
            } => {
                let obj = self.compile_expr(object)?;
                let mut argv: Vec<PointerValue<'ctx>> = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.compile_expr(a)?);
                }
                let (np, _) = self.str_global(name);
                let arr = self.argv_array(&argv);
                let f = self.tramp(
                    "spid3r_method",
                    &[
                        self.ptr_ty().into(),
                        self.ptr_ty().into(),
                        self.i64_ty().into(),
                        self.ptr_ty().into(),
                    ],
                    Some(self.ptr_ty().into()),
                );
                let n = self.i64c(args.len() as i64);
                let mname = self.tmp_name("m");
                let r = self
                    .builder
                    .build_call(f, &[obj.into(), np.into(), n.into(), arr.into()], &mname)
                    .unwrap();
                Ok(call_ptr(r))
            }
            Expr::Index { object, index, .. } => {
                let obj = self.compile_expr(object)?;
                let idx = self.compile_expr(index)?;
                Ok(self.call2p("spid3r_index_get", obj, idx))
            }
            Expr::Field { object, name, .. } => {
                let obj = self.compile_expr(object)?;
                let (np, _) = self.str_global(name);
                Ok(self.call2p("spid3r_field_get", obj, np))
            }
            Expr::New { class, args, .. } => {
                let cls = self.global_value(class);
                let mut argv: Vec<PointerValue<'ctx>> = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.compile_expr(a)?);
                }
                let arr = self.argv_array(&argv);
                let r = self.build_call_value(cls, args.len(), arr)?;
                Ok(r)
            }
            Expr::Spawn { callable, .. } => {
                let f = self.compile_expr(callable)?;
                Ok(self.call1p("spid3r_spawn", f))
            }
            Expr::Await(inner, _) => {
                let inner_v = self.compile_expr(inner)?;
                Ok(self.call1p("spid3r_await", inner_v))
            }
        }
    }

    /// Build the constant handle for a checker-validated default expression.
    /// The semantic checker guarantees the shape; the error arms are defensive.
    fn const_expr_value(&mut self, e: &Expr) -> IrResult<PointerValue<'ctx>> {
        match e {
            Expr::Int(v, _) => Ok(self.v_int(*v)),
            Expr::Float(v, _) => Ok(self.v_float(*v)),
            Expr::Str(s, _) => Ok(self.v_str(s)),
            Expr::Bool(b, _) => Ok(self.v_bool(*b)),
            Expr::Null(_) => Ok(self.v_null()),
            Expr::Unary {
                op: crate::ast::UnOp::Neg,
                expr,
                ..
            } => match &**expr {
                Expr::Int(v, _) => Ok(self.v_int(-v)),
                Expr::Float(v, _) => Ok(self.v_float(-v)),
                _ => Err(self.unsupported("non-constant parameter default")),
            },
            _ => Err(self.unsupported("non-constant parameter default")),
        }
    }

    fn compile_ident(&mut self, name: &str) -> IrResult<PointerValue<'ctx>> {
        match self.lookup(name) {
            Some(VarLoc::Local(slot)) => {
                let lname = self.tmp_name("v");
                let v = self
                    .builder
                    .build_load(self.ptr_ty(), slot, &lname)
                    .unwrap();
                Ok(v.into_pointer_value())
            }
            Some(VarLoc::Cell(slot)) => {
                let lname = self.tmp_name("c");
                let cell = self
                    .builder
                    .build_load(self.ptr_ty(), slot, &lname)
                    .unwrap()
                    .into_pointer_value();
                Ok(self.call1p("spid3r_cell_get", cell))
            }
            Some(VarLoc::Upvalue(i)) => Ok(self.read_upvalue(i)),
            None => Ok(self.global_value(name)),
        }
    }

    /// Read a name from the runtime global table.
    fn global_value(&mut self, name: &str) -> PointerValue<'ctx> {
        let (np, _) = self.str_global(name);
        self.call1p("spid3r_global_get", np)
    }

    /// Call an arbitrary value handle with `argc` arguments from a raw array.
    fn build_call_value(
        &mut self,
        callee: PointerValue<'ctx>,
        argc: usize,
        argv: PointerValue<'ctx>,
    ) -> IrResult<PointerValue<'ctx>> {
        let f = self.tramp(
            "spid3r_call",
            &[
                self.ptr_ty().into(),
                self.i64_ty().into(),
                self.ptr_ty().into(),
            ],
            Some(self.ptr_ty().into()),
        );
        let n = self.i64c(argc as i64);
        let cname = self.tmp_name("call");
        let r = self
            .builder
            .build_call(f, &[callee.into(), n.into(), argv.into()], &cname)
            .unwrap();
        Ok(call_ptr(r))
    }

    fn compile_call(&mut self, callee: &Expr, args: &[Expr]) -> IrResult<PointerValue<'ctx>> {
        // builtin collection constructors: list(...), map(...), ...
        if let Expr::Ident(name, _) = callee {
            if collections::is_constructor(name) {
                let (np, _) = self.str_global(name);
                let mut argv: Vec<PointerValue<'ctx>> = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.compile_expr(a)?);
                }
                let arr = self.argv_array(&argv);
                let f = self.tramp(
                    "spid3r_construct",
                    &[
                        self.ptr_ty().into(),
                        self.i64_ty().into(),
                        self.ptr_ty().into(),
                    ],
                    Some(self.ptr_ty().into()),
                );
                let n = self.i64c(args.len() as i64);
                let cname = self.tmp_name("ctor");
                let r = self
                    .builder
                    .build_call(f, &[np.into(), n.into(), arr.into()], &cname)
                    .unwrap();
                return Ok(call_ptr(r));
            }
        }
        let callee_v = self.compile_expr(callee)?;
        let mut argv: Vec<PointerValue<'ctx>> = Vec::with_capacity(args.len());
        for a in args {
            argv.push(self.compile_expr(a)?);
        }
        let arr = self.argv_array(&argv);
        let r = self.build_call_value(callee_v, args.len(), arr)?;
        Ok(r)
    }

    fn compile_assign(
        &mut self,
        target: &Expr,
        op: AssignOp,
        value: &Expr,
    ) -> IrResult<PointerValue<'ctx>> {
        // compound assignment: x += e  ==>  x = x + e
        if op != AssignOp::Assign {
            let bin = match op {
                AssignOp::AddAssign => BinOp::Add,
                AssignOp::SubAssign => BinOp::Sub,
                AssignOp::MulAssign => BinOp::Mul,
                AssignOp::DivAssign => BinOp::Div,
                AssignOp::ModAssign => BinOp::Mod,
                AssignOp::Assign => unreachable!(),
            };
            let cur = self.compile_expr(target)?;
            let rhs = self.compile_expr(value)?;
            let result = self.call_binop(cur, rhs, binop_code(bin));
            return self.store_assign(target, result);
        }
        let v = self.compile_expr(value)?;
        self.store_assign(target, v)
    }

    fn store_assign(
        &mut self,
        target: &Expr,
        v: PointerValue<'ctx>,
    ) -> IrResult<PointerValue<'ctx>> {
        match target {
            Expr::Ident(name, _) => match self.lookup(name) {
                Some(VarLoc::Local(slot)) => {
                    self.builder.build_store(slot, v).unwrap();
                    Ok(v)
                }
                Some(VarLoc::Cell(slot)) => {
                    let cell = self
                        .builder
                        .build_load(self.ptr_ty(), slot, "c")
                        .unwrap()
                        .into_pointer_value();
                    let f = self.tramp(
                        "spid3r_cell_set",
                        &[self.ptr_ty().into(), self.ptr_ty().into()],
                        None,
                    );
                    self.builder
                        .build_call(f, &[cell.into(), v.into()], "")
                        .unwrap();
                    Ok(v)
                }
                Some(VarLoc::Upvalue(i)) => {
                    let env = self.cur_env();
                    let slot = self.call_env_slot(env, i);
                    let f = self.tramp(
                        "spid3r_cell_set",
                        &[self.ptr_ty().into(), self.ptr_ty().into()],
                        None,
                    );
                    self.builder
                        .build_call(f, &[slot.into(), v.into()], "")
                        .unwrap();
                    Ok(v)
                }
                None => {
                    let (np, _) = self.str_global(name);
                    let f = self.tramp(
                        "spid3r_global_set",
                        &[self.ptr_ty().into(), self.ptr_ty().into()],
                        None,
                    );
                    self.builder
                        .build_call(f, &[np.into(), v.into()], "")
                        .unwrap();
                    Ok(v)
                }
            },
            Expr::Field { object, name, .. } => {
                let obj = self.compile_expr(object)?;
                let (np, _) = self.str_global(name);
                let f = self.tramp(
                    "spid3r_field_set",
                    &[
                        self.ptr_ty().into(),
                        self.ptr_ty().into(),
                        self.ptr_ty().into(),
                    ],
                    None,
                );
                self.builder
                    .build_call(f, &[obj.into(), np.into(), v.into()], "")
                    .unwrap();
                Ok(v)
            }
            Expr::Index { object, index, .. } => {
                let obj = self.compile_expr(object)?;
                let idx = self.compile_expr(index)?;
                let f = self.tramp(
                    "spid3r_index_set",
                    &[
                        self.ptr_ty().into(),
                        self.ptr_ty().into(),
                        self.ptr_ty().into(),
                    ],
                    None,
                );
                self.builder
                    .build_call(f, &[obj.into(), idx.into(), v.into()], "")
                    .unwrap();
                Ok(v)
            }
            _ => Err(self.err("invalid assignment target")),
        }
    }
}

// ---------------------------------------------------------------------------
// functions, closures, entry
// ---------------------------------------------------------------------------

impl<'ctx> Ir<'ctx> {
    /// Compile a function body. `caps` is the ordered list of captured names;
    /// the emitted function reads them from `%env`.
    ///
    /// `self_offset` is 1 for class constructors (`new`), whose parameters
    /// start at argv[1] because the receiver is implicitly at argv[0] (the
    /// same layout the VM uses), and 0 for plain functions and methods (whose
    /// `self` is an explicit parameter).
    fn compile_function(
        &mut self,
        proto: &FnProto,
        caps: Vec<String>,
        name_hint: &str,
        self_offset: usize,
    ) -> IrResult<FunctionValue<'ctx>> {
        let ptr_ty = self.ptr_ty();
        let fty = ptr_ty.fn_type(&[ptr_ty.into(), self.i64_ty().into(), ptr_ty.into()], false);
        self.fn_counter += 1;
        let llvm_name = format!("spid3r_fn.{}.{}", self.fn_counter, name_hint);
        let f = self.module.add_function(&llvm_name, fty, None);
        // Remember where emission was happening (this function is emitted as
        // a side effect; the caller keeps building in its own block).
        let saved_bb = self.builder.get_insert_block();
        let entry_bb = self.ctx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry_bb);

        // environment + return slot + capture declarations
        let env = f.get_nth_param(0).unwrap().into_pointer_value();
        let argv = f.get_nth_param(2).unwrap().into_pointer_value();
        let ret_slot = self.builder.build_alloca(ptr_ty, "ret").unwrap();
        let ret_bb = self.ctx.append_basic_block(f, "ret");
        {
            self.builder.position_at_end(ret_bb);
            let v = self.builder.build_load(ptr_ty, ret_slot, "rv").unwrap();
            self.builder.build_return(Some(&v)).unwrap();
            self.builder.position_at_end(entry_bb);
        }
        // Bottom exception-handler frame: a throw in this function with no
        // enclosing `try` returns to the caller with the pending flag set, so
        // it propagates up to the nearest enclosing `try` (an early return).
        let propagate_bb = self.ctx.append_basic_block(f, "propagate");
        {
            self.builder.position_at_end(propagate_bb);
            let null = ptr_ty.const_null();
            self.builder.build_store(ret_slot, null).unwrap();
            self.builder.build_unconditional_branch(ret_bb).unwrap();
            self.builder.position_at_end(entry_bb);
        }
        let saved = (
            self.env_param.replace(env),
            self.ret_bb.replace(ret_bb),
            std::mem::take(&mut self.ret_slots),
            std::mem::replace(&mut self.caps, caps),
            self.boxed.replace(boxing_set(proto)),
            std::mem::take(&mut self.scopes),
            std::mem::take(&mut self.loops),
            std::mem::take(&mut self.exc),
            std::mem::take(&mut self.finally_stack),
            self.is_entry,
        );
        self.ret_slots.push(ret_slot);
        self.exc = vec![propagate_bb];

        self.push_scope();
        // declare the captures as upvalue slots
        let caps_now = self.caps.clone();
        for (i, name) in caps_now.iter().enumerate() {
            self.declare(name, VarLoc::Upvalue(i));
        }
        // constructors receive the receiver implicitly at argv[0]
        if self_offset == 1 {
            let gep = unsafe {
                self.builder
                    .build_in_bounds_gep(self.ptr_ty(), argv, &[self.i64c(0)], "self")
                    .unwrap()
            };
            let recv = self
                .builder
                .build_load(ptr_ty, gep, "selfv")
                .unwrap()
                .into_pointer_value();
            let slot = self.builder.build_alloca(ptr_ty, "selfslot").unwrap();
            self.builder.build_store(slot, recv).unwrap();
            self.declare("self", VarLoc::Local(slot));
        }
        // bind parameters (argv[self_offset..]); captured params get a cell.
        // A parameter with a constant default is bound conditionally: the
        // argument exists only when argc exceeds its slot, otherwise the
        // baked constant is used (defaults are trailing-only, so omitted
        // arguments are always a suffix).
        let boxed = self.boxed.clone().unwrap_or_default();
        let argc_v = f.get_nth_param(1).unwrap().into_int_value();
        for (i, p) in proto.params.iter().enumerate() {
            let a = if let Some(d) = &p.default {
                let has = self
                    .builder
                    .build_int_compare(
                        inkwell::IntPredicate::SGT,
                        argc_v,
                        self.i64c((self_offset + i) as i64),
                        "has_arg",
                    )
                    .unwrap();
                let then_bb = self.ctx.append_basic_block(f, "arg_then");
                let else_bb = self.ctx.append_basic_block(f, "arg_else");
                let merge_bb = self.ctx.append_basic_block(f, "arg_merge");
                self.builder
                    .build_conditional_branch(has, then_bb, else_bb)
                    .unwrap();
                self.builder.position_at_end(then_bb);
                let gep = unsafe {
                    self.builder
                        .build_in_bounds_gep(
                            self.ptr_ty(),
                            argv,
                            &[self.i64c((self_offset + i) as i64)],
                            "arg",
                        )
                        .unwrap()
                };
                let from_argv = self.builder.build_load(ptr_ty, gep, "argv").unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();
                self.builder.position_at_end(else_bb);
                let from_default = self.const_expr_value(d)?;
                self.builder.build_unconditional_branch(merge_bb).unwrap();
                self.builder.position_at_end(merge_bb);
                let phi = self.builder.build_phi(ptr_ty, "arg").unwrap();
                phi.add_incoming(&[(&from_argv, then_bb), (&from_default, else_bb)]);
                phi.as_basic_value().into_pointer_value()
            } else {
                let gep = unsafe {
                    self.builder
                        .build_in_bounds_gep(
                            self.ptr_ty(),
                            argv,
                            &[self.i64c((self_offset + i) as i64)],
                            "arg",
                        )
                        .unwrap()
                };
                self.builder
                    .build_load(ptr_ty, gep, "argv")
                    .unwrap()
                    .into_pointer_value()
            };
            if boxed.contains(&p.name) {
                let cell = self.call1p("spid3r_cell_new", a);
                let slot = self.builder.build_alloca(ptr_ty, "pcell").unwrap();
                self.builder.build_store(slot, cell).unwrap();
                self.declare(&p.name, VarLoc::Cell(slot));
            } else {
                let slot = self.builder.build_alloca(ptr_ty, "p").unwrap();
                self.builder.build_store(slot, a).unwrap();
                self.declare(&p.name, VarLoc::Local(slot));
            }
        }
        self.compile_block(&proto.body.stmts)?;
        self.pop_scope();
        // implicit return null (in a fresh block if the body ended sealed)
        self.ensure_open();
        let null = self.v_null();
        self.builder.build_store(ret_slot, null).unwrap();
        self.builder.build_unconditional_branch(ret_bb).unwrap();
        self.builder.position_at_end(ret_bb);

        // restore
        self.is_entry = saved.9;
        self.finally_stack = saved.8;
        self.exc = saved.7;
        self.scopes = saved.5;
        self.loops = saved.6;
        self.boxed = saved.4;
        self.caps = saved.3;
        self.ret_slots = saved.2;
        self.ret_bb = saved.1;
        self.env_param = saved.0;
        // Restore the caller's emission position.
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        Ok(f)
    }

    /// Compile a function literal: emit the function and build the closure
    /// value, wiring captured cells into a fresh environment.
    fn compile_closure(&mut self, proto: &FnProto) -> IrResult<PointerValue<'ctx>> {
        let all_uses = capture_list(proto);
        // Resolve the captures against the current scope chain: locals and
        // upvalues of the enclosing function are captured; anything else is a
        // global and must NOT be captured. The filtered order is what both the
        // environment layout and the callee's upvalue indices use.
        let mut caps: Vec<String> = Vec::new();
        let mut env_sources: Vec<VarLoc<'ctx>> = Vec::new();
        for name in &all_uses {
            match self.lookup(name) {
                Some(loc @ (VarLoc::Cell(_) | VarLoc::Local(_) | VarLoc::Upvalue(_))) => {
                    caps.push(name.clone());
                    env_sources.push(loc);
                }
                None => {} // global: read via the global table
            }
        }
        let name_hint = proto.name.clone().unwrap_or_else(|| "anon".to_string());
        let f = self.compile_function(proto, caps.clone(), &name_hint, 0)?;

        let arity = proto.params.len();
        let (name_ptr, _) = self.str_global(proto.name.as_deref().unwrap_or(""));
        let code_ptr = f.as_global_value().as_pointer_value();
        let env = if env_sources.is_empty() {
            self.null_ptr()
        } else {
            let cnt = env_sources.len();
            let env = self.call_env_alloc(cnt);
            for (i, src) in env_sources.iter().enumerate() {
                let cell = match src {
                    VarLoc::Cell(slot) => self
                        .builder
                        .build_load(self.ptr_ty(), *slot, "cap")
                        .unwrap()
                        .into_pointer_value(),
                    VarLoc::Local(_) => {
                        // A non-boxed local can only end up here if the boxing
                        // analysis missed it; box it defensively.
                        let v = self
                            .builder
                            .build_load(
                                self.ptr_ty(),
                                match src {
                                    VarLoc::Local(s) => *s,
                                    _ => unreachable!(),
                                },
                                "capv",
                            )
                            .unwrap()
                            .into_pointer_value();
                        self.call1p("spid3r_cell_new", v)
                    }
                    VarLoc::Upvalue(j) => {
                        let env = self.cur_env();
                        self.call_env_slot(env, *j)
                    }
                };
                self.call_env_set_slot(env, i, cell);
            }
            env
        };
        let r = self.build_make_fn(
            name_ptr,
            arity as i64,
            proto.min_arity() as i64,
            code_ptr,
            env,
        );
        Ok(r)
    }

    fn call_env_alloc(&mut self, count: usize) -> PointerValue<'ctx> {
        let f = self.tramp(
            "spid3r_env_alloc",
            &[self.i64_ty().into()],
            Some(self.ptr_ty().into()),
        );
        let n = self.i64c(count as i64);
        let ename = self.tmp_name("env");
        let r = self.builder.build_call(f, &[n.into()], &ename).unwrap();
        call_ptr(r)
    }

    fn call_env_set_slot(
        &mut self,
        env: PointerValue<'ctx>,
        index: usize,
        cell: PointerValue<'ctx>,
    ) {
        let f = self.tramp(
            "spid3r_env_set_slot",
            &[
                self.ptr_ty().into(),
                self.i64_ty().into(),
                self.ptr_ty().into(),
            ],
            None,
        );
        self.builder
            .build_call(
                f,
                &[env.into(), self.i64c(index as i64).into(), cell.into()],
                "",
            )
            .unwrap();
    }

    fn build_make_fn(
        &mut self,
        name_ptr: PointerValue<'ctx>,
        arity: i64,
        min_arity: i64,
        code_ptr: PointerValue<'ctx>,
        env: PointerValue<'ctx>,
    ) -> PointerValue<'ctx> {
        let f = self.tramp(
            "spid3r_make_fn",
            &[
                self.ptr_ty().into(),
                self.i64_ty().into(),
                self.i64_ty().into(),
                self.ptr_ty().into(),
                self.ptr_ty().into(),
            ],
            Some(self.ptr_ty().into()),
        );
        let n = self.i64c(arity);
        let mn = self.i64c(min_arity);
        let fname = self.tmp_name("fn");
        let r = self
            .builder
            .build_call(
                f,
                &[
                    name_ptr.into(),
                    n.into(),
                    mn.into(),
                    code_ptr.into(),
                    env.into(),
                ],
                &fname,
            )
            .unwrap();
        call_ptr(r)
    }

    /// Build the module entry: register classes, run top-level statements.
    pub fn build_entry(&mut self, program: &Program) -> IrResult<PointerValue<'ctx>> {
        let ptr_ty = self.ptr_ty();
        let fty = ptr_ty.fn_type(&[], false);
        let entry = self.module.add_function("spid3r_entry", fty, None);
        let bb = self.ctx.append_basic_block(entry, "entry");
        self.builder.position_at_end(bb);
        let saved = (
            self.env_param.take(),
            self.ret_bb.take(),
            std::mem::take(&mut self.ret_slots),
            std::mem::take(&mut self.caps),
            self.boxed.take(),
            std::mem::take(&mut self.scopes),
            std::mem::take(&mut self.loops),
            std::mem::take(&mut self.exc),
            std::mem::take(&mut self.finally_stack),
            self.is_entry,
        );
        self.is_entry = true;
        // Bottom exception-handler frame for top-level statements: a `throw`
        // outside any `try` is an uncaught program error.
        let uncaught_bb = self.ctx.append_basic_block(entry, "uncaught");
        {
            self.builder.position_at_end(uncaught_bb);
            let f = self.tramp("spid3r_uncaught", &[], None);
            self.builder.build_call(f, &[], "").unwrap();
            self.builder.build_unreachable().unwrap();
            self.builder.position_at_end(bb);
        }
        self.exc = vec![uncaught_bb];
        self.push_scope();

        // register classes first (parents before children, as declared)
        for s in &program.stmts {
            if let Stmt::Class(c) = s {
                self.register_class(c)?;
            }
        }
        for s in &program.stmts {
            // classes/interfaces were registered by the pass above
            if matches!(s, Stmt::Class(_) | Stmt::Interface(_)) {
                continue;
            }
            self.ensure_open();
            self.compile_stmt(s)?;
        }
        self.pop_scope();

        self.ensure_open();
        let null = self.v_null();
        self.builder.build_return(Some(&null)).unwrap();

        self.is_entry = saved.9;
        self.finally_stack = saved.8;
        self.exc = saved.7;
        self.scopes = saved.5;
        self.loops = saved.6;
        self.boxed = saved.4;
        self.caps = saved.3;
        self.ret_slots = saved.2;
        self.ret_bb = saved.1;
        self.env_param = saved.0;
        Ok(entry.as_global_value().as_pointer_value())
    }

    fn register_class(&mut self, c: &ClassDecl) -> IrResult<()> {
        let (name_ptr, _) = self.str_global(&c.name);
        let parent_ptr = match &c.parent {
            Some(p) => {
                let (pp, _) = self.str_global(p);
                pp
            }
            None => self.null_ptr(),
        };
        let cls = self.call2p("spid3r_class_register", name_ptr, parent_ptr);
        // NOTE: the class becomes a global only after all fields/methods/ctor
        // are registered — the runtime mutates the class through its unique
        // Arc while no other reference exists.
        for field in &c.fields {
            let (fp, _) = self.str_global(&field.name);
            let vis = if field.vis == crate::ast::Vis::Public {
                1u8
            } else {
                0u8
            };
            let f = self.tramp(
                "spid3r_class_add_field",
                &[
                    self.ptr_ty().into(),
                    self.ptr_ty().into(),
                    self.i8_ty().into(),
                ],
                None,
            );
            self.builder
                .build_call(f, &[cls.into(), fp.into(), self.i8c(vis).into()], "")
                .unwrap();
        }
        for m in &c.methods {
            let is_ctor = m.name.as_deref() == Some("new");
            let arity = if is_ctor {
                m.params.len() + 1 // synthetic self at argv[0]
            } else {
                m.params.len()
            };
            // constructors carry an implicit self at argv[0], so their
            // parameters start one slot later (matching the VM's layout)
            let self_offset = if is_ctor { 1 } else { 0 };
            let f = self.compile_function(
                m,
                Vec::new(),
                &format!("{}.{}", c.name, m.name.as_deref().unwrap_or("m")),
                self_offset,
            )?;
            let code_ptr = f.as_global_value().as_pointer_value();
            if is_ctor {
                let ft = self.tramp(
                    "spid3r_class_set_ctor",
                    &[
                        self.ptr_ty().into(),
                        self.i64_ty().into(),
                        self.i64_ty().into(),
                        self.ptr_ty().into(),
                    ],
                    None,
                );
                self.builder
                    .build_call(
                        ft,
                        &[
                            cls.into(),
                            self.i64c(arity as i64).into(),
                            self.i64c(m.min_arity() as i64).into(),
                            code_ptr.into(),
                        ],
                        "",
                    )
                    .unwrap();
            } else {
                let (mp, _) = self.str_global(m.name.as_deref().unwrap_or("m"));
                let ft = self.tramp(
                    "spid3r_class_add_method",
                    &[
                        self.ptr_ty().into(),
                        self.ptr_ty().into(),
                        self.i64_ty().into(),
                        self.i64_ty().into(),
                        self.ptr_ty().into(),
                    ],
                    None,
                );
                self.builder
                    .build_call(
                        ft,
                        &[
                            cls.into(),
                            mp.into(),
                            self.i64c(arity as i64).into(),
                            self.i64c(m.min_arity() as i64).into(),
                            code_ptr.into(),
                        ],
                        "",
                    )
                    .unwrap();
            }
        }
        // now publish the fully built class under its name
        let f = self.tramp(
            "spid3r_global_set",
            &[self.ptr_ty().into(), self.ptr_ty().into()],
            None,
        );
        self.builder
            .build_call(f, &[name_ptr.into(), cls.into()], "")
            .unwrap();
        Ok(())
    }

    /// Build the C `main`: hand the entry to the runtime, which installs the
    /// stdlib, runs the top-level code, then calls `main`.
    pub fn build_cmain(&mut self, entry: PointerValue<'ctx>) {
        let i32_ty = self.ctx.i32_type();
        let fty = i32_ty.fn_type(&[], false);
        let main = self.module.add_function("main", fty, None);
        let bb = self.ctx.append_basic_block(main, "entry");
        self.builder.position_at_end(bb);
        let f = self.tramp(
            "spid3r_rt_start",
            &[self.ptr_ty().into()],
            Some(i32_ty.into()),
        );
        let r = self.builder.build_call(f, &[entry.into()], "code").unwrap();
        let code = call_int(r);
        self.builder.build_return(Some(&code)).unwrap();
    }
}

/// Compile a checked program to an LLVM module.
pub fn build_module(
    program: &Program,
    module_name: &str,
) -> IrResult<inkwell::module::Module<'static>> {
    // Same seam as the interpreters: the AOT backend lowers before codegen.
    let lowered = crate::lower::lower(program);
    let program = lowered.as_ref();
    let ctx = Box::leak(Box::new(Context::create()));
    let mut ir = Ir::new(ctx, module_name);
    // top-level functions are globals holding closure values
    // (compiled as part of the entry statements, like the VM does)
    let entry = ir.build_entry(program)?;
    ir.build_cmain(entry);
    if let Err(e) = ir.module().verify() {
        return Err(format!("internal error: generated LLVM IR is invalid: {e}"));
    }
    Ok(ir.module)
}
