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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::types::BasicType;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{FunctionValue, PointerValue};
use inkwell::AddressSpace;
use inkwell::FloatPredicate;
use inkwell::IntPredicate;

use crate::ast::{
    AssignOp, BinOp, Block, CatchClause, ClassDecl, Expr, FnProto, LogOp, Param, Program, Stmt,
    TypeExpr, UnOp,
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
    /// Stack slot holding an unboxed 64-bit integer.
    LocalInt(PointerValue<'ctx>),
    /// AOT-4: stack slot holding an unboxed 64-bit IEEE float.
    LocalFloat(PointerValue<'ctx>),
    /// R10.3: stack slot holding a raw `Vec<i64>` handle (`list<int>`).
    UListI64(PointerValue<'ctx>),
    /// R10.3: stack slot holding a raw `Vec<f64>` handle (`list<float>`).
    UListF64(PointerValue<'ctx>),
    /// Phase 1: loop-hoisted contiguous read-only `i64` slice view.
    RowViewI64 {
        data: PointerValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
        orig_slot: PointerValue<'ctx>,
        is_ulist: bool,
    },
    /// Phase 1: loop-hoisted contiguous read-only `f64` slice view.
    RowViewF64 {
        data: PointerValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
        orig_slot: PointerValue<'ctx>,
        is_ulist: bool,
    },
    /// Stack slot holding an upvalue-cell pointer.
    Cell(PointerValue<'ctx>),
    /// Index of the captured cell inside this function's environment.
    Upvalue(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UnboxedElem {
    I64,
    F64,
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
    /// `loops.len()` when this `try` was entered. A `break`/`continue` only
    /// crosses this `finally` when its target loop encloses the `try`
    /// (`loops.len() <= loop_depth`); a loop nested *inside* the try body is
    /// exited directly without running `finally`.
    loop_depth: usize,
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

/// Extract the floating-point result of a call.
fn call_float<'a>(r: inkwell::values::CallSiteValue<'a>) -> inkwell::values::FloatValue<'a> {
    match r.try_as_basic_value() {
        inkwell::values::ValueKind::Basic(b) => b.into_float_value(),
        _ => panic!("spid3r: runtime trampoline did not return a float"),
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
        // Lowering rewrites generators; the yielded value is still an
        // expression, so walk it like any other statement value.
        Stmt::Yield { value, .. } => collect_outer_uses_expr(value, declared, out),
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
        // A named argument never survives `lower`; walk its value anyway so a
        // capture inside one is never missed.
        Expr::NamedArg { value, .. } => {
            collect_outer_uses_expr(value, declared, out);
        }
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
// top-level scalar constant folding (W4.3)
// ---------------------------------------------------------------------------

fn top_const_scalar(e: &Expr) -> Option<Expr> {
    match e {
        Expr::Int(v, sp) => Some(Expr::Int(*v, sp.clone())),
        Expr::Float(v, sp) => Some(Expr::Float(*v, sp.clone())),
        Expr::Str(s, sp) => Some(Expr::Str(s.clone(), sp.clone())),
        Expr::Bool(b, sp) => Some(Expr::Bool(*b, sp.clone())),
        Expr::Null(sp) => Some(Expr::Null(sp.clone())),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
            span,
        } => match &**expr {
            Expr::Int(v, isp) if *v != i64::MIN => Some(Expr::Unary {
                op: UnOp::Neg,
                expr: Box::new(Expr::Int(*v, isp.clone())),
                span: span.clone(),
            }),
            Expr::Float(v, fsp) => Some(Expr::Unary {
                op: UnOp::Neg,
                expr: Box::new(Expr::Float(*v, fsp.clone())),
                span: span.clone(),
            }),
            _ => None,
        },
        _ => None,
    }
}

fn top_const_int(e: &Expr) -> Option<i64> {
    match e {
        Expr::Int(v, _) => Some(*v),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
            ..
        } => match &**expr {
            Expr::Int(v, _) => v.checked_neg(),
            _ => None,
        },
        _ => None,
    }
}

fn is_pure_top_expr(
    e: &Expr,
    shadowed: &HashSet<String>,
    known_consts: &HashMap<String, Expr>,
) -> bool {
    if top_const_scalar(e).is_some() {
        return true;
    }
    match e {
        Expr::Ident(n, _) => known_consts.contains_key(n),
        Expr::List(items, _) => items
            .iter()
            .all(|it| is_pure_top_expr(it, shadowed, known_consts)),
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(fn_name, _) = &**callee {
                if !shadowed.contains(fn_name) {
                    return match fn_name.as_str() {
                        "map" => args
                            .iter()
                            .all(|a| is_pure_top_expr(a, shadowed, known_consts)),
                        "bytes" => args.len() == 1 && matches!(args[0], Expr::Int(n, _) if n >= 0),
                        "float" => {
                            args.len() == 1
                                && matches!(&args[0], Expr::Str(s, _) if s == "inf" || s == "-inf" || s == "nan")
                        }
                        _ => false,
                    };
                }
            }
            false
        }
        _ => false,
    }
}

fn collect_top_const_maps(program: &Program) -> (HashMap<String, Expr>, HashMap<String, i64>) {
    let mut decl_counts: HashMap<String, usize> = HashMap::new();
    let mut declared: HashSet<String> = HashSet::new();
    for s in &program.stmts {
        match s {
            Stmt::Let { name, .. } | Stmt::Const { name, .. } => {
                *decl_counts.entry(name.clone()).or_insert(0) += 1;
                declared.insert(name.clone());
            }
            Stmt::Fn(p) => {
                if let Some(n) = &p.name {
                    *decl_counts.entry(n.clone()).or_insert(0) += 1;
                    declared.insert(n.clone());
                }
            }
            Stmt::Class(c) => {
                *decl_counts.entry(c.name.clone()).or_insert(0) += 1;
                declared.insert(c.name.clone());
            }
            Stmt::Interface(i) => {
                *decl_counts.entry(i.name.clone()).or_insert(0) += 1;
                declared.insert(i.name.clone());
            }
            Stmt::Enum(e) => {
                *decl_counts.entry(e.name.clone()).or_insert(0) += 1;
                declared.insert(e.name.clone());
            }
            Stmt::LetTuple { names, .. } => {
                for n in names {
                    *decl_counts.entry(n.clone()).or_insert(0) += 1;
                    declared.insert(n.clone());
                }
            }
            _ => {}
        }
    }

    let mut consts: HashMap<String, Expr> = HashMap::new();
    let mut ints: HashMap<String, i64> = HashMap::new();
    for s in &program.stmts {
        match s {
            Stmt::Fn(_) | Stmt::Class(_) | Stmt::Interface(_) => {}
            Stmt::Let { value, .. } => {
                if let Some(v) = value {
                    if !is_pure_top_expr(v, &declared, &consts) {
                        break;
                    }
                }
            }
            Stmt::Const { name, value, .. } => {
                if let Some(scalar) = top_const_scalar(value) {
                    if decl_counts.get(name).copied() == Some(1) {
                        if let Some(iv) = top_const_int(&scalar) {
                            ints.insert(name.clone(), iv);
                        }
                        consts.insert(name.clone(), scalar);
                    }
                } else if !is_pure_top_expr(value, &declared, &consts) {
                    break;
                }
            }
            _ => break,
        }
    }
    (consts, ints)
}

fn stmt_mentions_assign_target(s: &Stmt, target_name: &str) -> bool {
    fn expr_assigns(e: &Expr, target_name: &str) -> bool {
        match e {
            Expr::Assign { target, value, .. } => {
                if matches!(&**target, Expr::Ident(n, _) if n == target_name) {
                    return true;
                }
                expr_assigns(target, target_name) || expr_assigns(value, target_name)
            }
            Expr::Unary { expr, .. } | Expr::Await(expr, _) => expr_assigns(expr, target_name),
            Expr::Binary { l, r, .. } | Expr::Logical { l, r, .. } => {
                expr_assigns(l, target_name) || expr_assigns(r, target_name)
            }
            Expr::Call { callee, args, .. } => {
                expr_assigns(callee, target_name)
                    || args.iter().any(|a| expr_assigns(a, target_name))
            }
            Expr::Method { object, args, .. } => {
                expr_assigns(object, target_name)
                    || args.iter().any(|a| expr_assigns(a, target_name))
            }
            Expr::Index { object, index, .. } => {
                expr_assigns(object, target_name) || expr_assigns(index, target_name)
            }
            Expr::Field { object, .. } | Expr::OptField { object, .. } => {
                expr_assigns(object, target_name)
            }
            Expr::List(items, _) => items.iter().any(|a| expr_assigns(a, target_name)),
            Expr::New { args, .. } | Expr::SuperCall { args, .. } => {
                args.iter().any(|a| expr_assigns(a, target_name))
            }
            Expr::Fn(p) => p
                .body
                .stmts
                .iter()
                .any(|st| stmt_mentions_assign_target(st, target_name)),
            _ => false,
        }
    }
    match s {
        Stmt::Expr(e) | Stmt::Return { value: Some(e), .. } | Stmt::Throw { value: e, .. } => {
            expr_assigns(e, target_name)
        }
        Stmt::Let { value: Some(e), .. } | Stmt::Const { value: e, .. } => {
            expr_assigns(e, target_name)
        }
        Stmt::Fn(p) => p
            .body
            .stmts
            .iter()
            .any(|st| stmt_mentions_assign_target(st, target_name)),
        Stmt::Class(c) => c.methods.iter().any(|m| {
            m.body
                .stmts
                .iter()
                .any(|st| stmt_mentions_assign_target(st, target_name))
        }),
        Stmt::If {
            cond,
            then,
            elifs,
            els,
            ..
        } => {
            expr_assigns(cond, target_name)
                || then
                    .stmts
                    .iter()
                    .any(|st| stmt_mentions_assign_target(st, target_name))
                || elifs.iter().any(|(c, b)| {
                    expr_assigns(c, target_name)
                        || b.stmts
                            .iter()
                            .any(|st| stmt_mentions_assign_target(st, target_name))
                })
                || els.as_ref().is_some_and(|b| {
                    b.stmts
                        .iter()
                        .any(|st| stmt_mentions_assign_target(st, target_name))
                })
        }
        Stmt::While { cond, body, .. } => {
            expr_assigns(cond, target_name)
                || body
                    .stmts
                    .iter()
                    .any(|st| stmt_mentions_assign_target(st, target_name))
        }
        Stmt::ForIn { iter, body, .. } => {
            expr_assigns(iter, target_name)
                || body
                    .stmts
                    .iter()
                    .any(|st| stmt_mentions_assign_target(st, target_name))
        }
        Stmt::ForC {
            init,
            cond,
            post,
            body,
            ..
        } => {
            stmt_mentions_assign_target(init, target_name)
                || expr_assigns(cond, target_name)
                || expr_assigns(post, target_name)
                || body
                    .stmts
                    .iter()
                    .any(|st| stmt_mentions_assign_target(st, target_name))
        }
        Stmt::Block(b) => b
            .stmts
            .iter()
            .any(|st| stmt_mentions_assign_target(st, target_name)),
        Stmt::Try {
            body,
            catch,
            finally,
            ..
        } => {
            body.stmts
                .iter()
                .any(|st| stmt_mentions_assign_target(st, target_name))
                || catch.as_ref().is_some_and(|c| {
                    c.body
                        .stmts
                        .iter()
                        .any(|st| stmt_mentions_assign_target(st, target_name))
                })
                || finally.as_ref().is_some_and(|f| {
                    f.stmts
                        .iter()
                        .any(|st| stmt_mentions_assign_target(st, target_name))
                })
        }
        _ => false,
    }
}

fn is_scalar_type_expr(te: &crate::ast::TypeExpr) -> Option<&'static str> {
    if !te.args.is_empty() || te.nullable || !te.union.is_empty() {
        return None;
    }
    match te.name.as_str() {
        "int" | "usize" => Some("int"),
        "float" => Some("float"),
        "bool" => Some("bool"),
        _ => None,
    }
}

/// AOT-5: collect top-level typed non-generic functions that are declared once,
/// never reassigned, have no default parameters, and accept/return at least one
/// unboxed scalar (`int`, `float`, `bool`).
fn collect_raw_fns(program: &Program) -> HashMap<String, FnProto> {
    let mut top_counts: HashMap<String, usize> = HashMap::new();
    for s in &program.stmts {
        match s {
            Stmt::Fn(f) => {
                if let Some(ref name) = f.name {
                    *top_counts.entry(name.clone()).or_insert(0) += 1;
                }
            }
            Stmt::Let { name, .. } | Stmt::Const { name, .. } => {
                *top_counts.entry(name.clone()).or_insert(0) += 1;
            }
            Stmt::Class(c) => {
                *top_counts.entry(c.name.clone()).or_insert(0) += 1;
            }
            _ => {}
        }
    }
    let mut out = HashMap::new();
    for s in &program.stmts {
        let Stmt::Fn(f) = s else { continue };
        let Some(ref name) = f.name else { continue };
        if name == "main"
            || !f.type_params.is_empty()
            || f.is_extern
            || f.is_async
            || f.params.iter().any(|p| p.default.is_some())
            || top_counts.get(name).copied().unwrap_or(0) != 1
        {
            continue;
        }
        let has_scalar_param = f
            .params
            .iter()
            .any(|p| p.ty.as_ref().and_then(is_scalar_type_expr).is_some());
        let has_scalar_ret = f.ret.as_ref().and_then(is_scalar_type_expr).is_some();
        if !has_scalar_param && !has_scalar_ret {
            continue;
        }
        if program
            .stmts
            .iter()
            .any(|st| stmt_mentions_assign_target(st, name))
        {
            continue;
        }
        out.insert(name.clone(), f.clone());
    }
    out
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
    /// locals of the current function that live in unboxed i64 stack slots
    int_locals: Option<HashSet<String>>,
    /// AOT-4: locals of the current function that live in unboxed f64 stack slots
    float_locals: Option<HashSet<String>>,
    /// true when compiling `--freestanding` (skips `spid3r_reg_*` calls)
    freestanding: bool,
    /// true while compiling the module entry (top level = globals)
    is_entry: bool,
    strings: HashMap<String, PointerValue<'ctx>>,
    tramps: HashMap<String, FunctionValue<'ctx>>,
    /// `extern fn` declarations, by Spidr name. These are C symbols the
    /// linker resolves, not runtime trampolines.
    externs: HashMap<String, FunctionValue<'ctx>>,
    /// The declared boundary types behind [`Ir::externs`], recorded by
    /// `declare_externs` so marshalling can tell `string` from `ptr`.
    extern_sigs: HashMap<String, ExternSig>,
    tmp: usize,
    fn_counter: usize,
    /// Phase 3: set while a hybrid compare's SLOW arm recompiles the original
    /// expression, so the re-dispatch cannot match the hybrid path again (the
    /// slow arm must emit the fully generic lowering — re-entering the hybrid
    /// path would recurse forever).
    hybrid_off: bool,
    /// W4.3: top-level unambiguous scalar `const` values safe to inline at use sites.
    top_consts: HashMap<String, Expr>,
    top_ints: HashMap<String, i64>,
    /// R3.4: runtime class-id global for the enclosing class's methods/ctor.
    current_class_id_gv: Option<inkwell::values::GlobalValue<'ctx>>,
    /// R3.5: per-module `{ i64 gen, ptr val }` cache slots for globals/builtins.
    global_caches: HashMap<String, inkwell::values::GlobalValue<'ctx>>,
    /// R9.1: register-class POD struct definitions.
    pod_structs: HashMap<String, PodStruct>,
    /// R9.1: target triple for ABI-specific struct-by-value lowering.
    target_triple: Option<String>,
    /// R9.1: variable C-ABI type hints for varargs.
    var_c_hints: HashMap<String, &'static str>,
    /// R9.2: unique callback slot/thunk counter.
    cb_counter: usize,
    /// DP-5: allow `dl_open` / `dl_sym` / `dl_close` / `dl_call` (gated by `--allow-dlopen`).
    allow_dlopen: bool,
    /// R12.3: optional instruction-step budget limit enforced in native loops.
    enforce_budget: Option<i64>,
    /// R12.3: global step counter when `enforce_budget.is_some()`.
    budget_counter_gv: Option<inkwell::values::GlobalValue<'ctx>>,
    /// R10.3: unboxed `list<int>` / `list<float>` locals in the current function.
    unboxed_lists: Option<HashMap<String, UnboxedElem>>,
    /// R10.3: generic top-level functions available for monomorphization.
    generic_fns: HashMap<String, FnProto>,
    /// R10.3: already-emitted monomorphized specializations.
    mono_fns: HashMap<String, MonoFnSig<'ctx>>,
    /// R10.3: unboxed i64 return slot when compiling an `-> int` monomorphized fn.
    mono_ret_int_slot: Option<PointerValue<'ctx>>,
    /// AOT-4/5: unboxed f64 return slot when compiling an `-> float` specialized fn.
    mono_ret_float_slot: Option<PointerValue<'ctx>>,
    /// AOT-5: unboxed i8 return slot when compiling an `-> bool` specialized fn.
    mono_ret_bool_slot: Option<PointerValue<'ctx>>,
    /// AOT-5: i8 threw flag slot when compiling a `{ T, i1 }` direct-call `spid3r_fn_raw__*`.
    mono_ret_threw_slot: Option<PointerValue<'ctx>>,
    /// AOT-5: top-level typed non-generic functions eligible for direct raw calls.
    raw_fns: HashMap<String, FnProto>,
    /// AOT-5: already-emitted `spid3r_fn_raw__*` specializations.
    raw_compiled: HashMap<String, MonoFnSig<'ctx>>,
    /// DP-1 / RT-2: placeholder alloca for the current function's `[M x { i64, i64 }]`
    /// `ValueV2` frame slots array (`None` in pure unboxed raw functions).
    frame_base_placeholder: Option<PointerValue<'ctx>>,
    /// DP-1 / RT-2: number of `ValueV2` slots allocated in the current function frame.
    frame_slot_count: u32,
    /// Phase 1: BCE-proven loop induction variable per hoisted `RowView*` variable name.
    row_bce_vars: HashMap<String, String>,
    /// Phase 1: variables in the current function whose type annotation or float
    /// literal initializer/assignment hints float representation in hoisted loops.
    float_hint_vars: HashSet<String>,
    /// Phase 2: names declared or assigned anywhere in the program (so built-in
    /// helpers like `string` are only fast-lowered when never shadowed/rebound).
    shadowed_builtins: HashSet<String>,
    /// Phase 2: single-declaration locals known to hold string values.
    str_locals: Option<HashSet<String>>,
    /// Phase 2: set by self-guarded or non-throwing `compile_expr_inner` arms so
    /// `compile_expr` skips emitting a duplicate `spid3r_threw()` poll.
    expr_guarded: bool,
}

type MonoFnSig<'ctx> = (FunctionValue<'ctx>, Option<String>, Vec<Option<String>>);

/// Compilation options passed from the CLI / build driver into IR generation.
#[derive(Clone, Debug, Default)]
pub struct IrOptions {
    pub target_triple: Option<String>,
    pub allow_dlopen: bool,
    pub enforce_budget: Option<i64>,
    pub freestanding: bool,
    pub entry_symbol: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PodFieldTy {
    Int,
    Float,
    Bool,
    Usize,
    Ptr,
}

#[derive(Clone)]
struct PodStruct {
    name: String,
    fields: Vec<(String, PodFieldTy)>,
}

/// Declared boundary types of one `extern fn` (SH-7). Marshalling dispatches
/// on these names, not on the LLVM type alone: `string` and `ptr` both lower
/// to an LLVM pointer, but a `string` argument is copied into a C buffer and
/// freed after the call, while a `ptr` argument travels through untouched.
#[derive(Clone)]
struct ExternSig {
    params: Vec<String>,
    ret: Option<String>,
    is_variadic: bool,
    win64_sret: Option<String>,
}

/// Signature of compiled Spid3r functions (DP-1 Option A: 16-byte ValueV2 out-param).
type FnTy<'ctx> = unsafe extern "C" fn(
    *mut crate::value::ValueV2,
    *mut core::ffi::c_void,
    usize,
    *const *const crate::value::ValueV2,
);

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
            int_locals: None,
            float_locals: None,
            hybrid_off: false,
            top_consts: HashMap::new(),
            top_ints: HashMap::new(),
            current_class_id_gv: None,
            global_caches: HashMap::new(),
            pod_structs: HashMap::new(),
            target_triple: None,
            var_c_hints: HashMap::new(),
            cb_counter: 0,
            allow_dlopen: false,
            enforce_budget: None,
            budget_counter_gv: None,
            unboxed_lists: None,
            generic_fns: HashMap::new(),
            mono_fns: HashMap::new(),
            mono_ret_int_slot: None,
            mono_ret_float_slot: None,
            mono_ret_bool_slot: None,
            mono_ret_threw_slot: None,
            raw_fns: HashMap::new(),
            raw_compiled: HashMap::new(),
            frame_base_placeholder: None,
            frame_slot_count: 0,
            row_bce_vars: HashMap::new(),
            float_hint_vars: HashSet::new(),
            shadowed_builtins: HashSet::new(),
            str_locals: None,
            expr_guarded: false,
            freestanding: false,
            is_entry: false,
            strings: HashMap::new(),
            tramps: HashMap::new(),
            externs: HashMap::new(),
            extern_sigs: HashMap::new(),
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

    /// LLVM type of `ValueV2`: `{ i64, i64 }` (16-byte register pair, DP-1 Option A).
    fn val_v2_ty(&self) -> inkwell::types::StructType<'ctx> {
        self.ctx
            .struct_type(&[self.i64_ty().into(), self.i64_ty().into()], false)
    }

    /// Begin a `ValueV2` GC-rooted frame in the current function's entry block.
    fn begin_v2_frame(&mut self) {
        let placeholder = self
            .builder
            .build_alloca(self.val_v2_ty(), "v2frame.ph")
            .unwrap();
        self.frame_base_placeholder = Some(placeholder);
        self.frame_slot_count = 0;
    }

    /// Finalize the `ValueV2` frame: replace the placeholder alloca with the
    /// exact `[M x { i64, i64 }]` array, zero-init it (`TAG_NULL = 0`), and
    /// register it with `spid3r_frame_enter`.
    fn finish_v2_frame(&mut self, func: FunctionValue<'ctx>) {
        let Some(placeholder) = self.frame_base_placeholder.take() else {
            return;
        };
        let count = self.frame_slot_count.max(1);
        self.frame_slot_count = 0;
        let entry = func
            .get_first_basic_block()
            .expect("function must have entry block");
        let cur = self.builder.get_insert_block();
        let arr_ty = self.val_v2_ty().array_type(count);
        let ph_inst = placeholder.as_instruction().unwrap();

        // Place the real array alloca at the very top of the entry block.
        match entry.get_first_instruction() {
            Some(first) => self.builder.position_before(&first),
            None => self.builder.position_at_end(entry),
        }
        let real_arr = self.builder.build_alloca(arr_ty, "v2frame").unwrap();

        // Zero-init and register frame roots right before `placeholder` (before
        // any GEP into `placeholder` executes).
        self.builder.position_before(&ph_inst);
        self.builder
            .build_memset(real_arr, 8, self.i8c(0), self.i64c((count as i64) * 16))
            .unwrap();
        let f_enter = self.tramp(
            "spid3r_frame_enter",
            &[self.ptr_ty().into(), self.i64_ty().into()],
            None,
        );
        self.builder
            .build_call(
                f_enter,
                &[real_arr.into(), self.i64c(count as i64).into()],
                "",
            )
            .unwrap();

        // Replace every GEP use of `placeholder` with `real_arr` and erase it.
        ph_inst.replace_all_uses_with(&real_arr.as_instruction().unwrap());
        ph_inst.erase_from_basic_block();

        if let Some(bb) = cur {
            self.builder.position_at_end(bb);
        }
    }

    /// Finalize a `ValueV2` frame in a monomorphized/raw helper only if any
    /// `ValueV2` slots were allocated, keeping pure scalar functions zero-overhead.
    fn finish_v2_frame_if_used(&mut self, func: FunctionValue<'ctx>, ret_bb: BasicBlock<'ctx>) {
        if self.frame_slot_count == 0 {
            if let Some(ph) = self.frame_base_placeholder.take() {
                if let Some(inst) = ph.as_instruction() {
                    inst.erase_from_basic_block();
                }
            }
            return;
        }
        self.finish_v2_frame(func);
        let cur = self.builder.get_insert_block();
        if let Some(term) = ret_bb.get_terminator() {
            self.builder.position_before(&term);
            let f_exit = self.tramp("spid3r_frame_exit", &[], None);
            self.builder.build_call(f_exit, &[], "").unwrap();
        }
        if let Some(bb) = cur {
            self.builder.position_at_end(bb);
        }
    }

    /// Allocate one 16-byte `ValueV2` slot in the current function's rooted
    /// frame array (or a standalone entry alloca in unboxed raw functions).
    fn alloc_v2_slot(&mut self) -> PointerValue<'ctx> {
        if let Some(base) = self.frame_base_placeholder {
            let idx = self.frame_slot_count;
            self.frame_slot_count += 1;
            let v2_ty = self.val_v2_ty();
            // SAFETY: `finish_v2_frame` sizes `base` to `[frame_slot_count x { i64, i64 }]`.
            unsafe {
                self.builder
                    .build_in_bounds_gep(v2_ty, base, &[self.i64c(idx as i64)], "v2slot")
                    .unwrap()
            }
        } else {
            self.alloca_entry_ty(self.val_v2_ty(), "v2tmp")
        }
    }

    /// Store an inline scalar `ValueV2 { tag, payload }` into `slot` (zero heap alloc).
    fn store_v2_inline(
        &mut self,
        slot: PointerValue<'ctx>,
        tag: u64,
        payload: inkwell::values::IntValue<'ctx>,
    ) {
        let v2_ty = self.val_v2_ty();
        let tag_ptr = unsafe {
            self.builder
                .build_in_bounds_gep(v2_ty, slot, &[self.i64c(0), self.i32c(0)], "v2.t")
                .unwrap()
        };
        self.builder
            .build_store(tag_ptr, self.i64c(tag as i64))
            .unwrap();
        let pay_ptr = unsafe {
            self.builder
                .build_in_bounds_gep(v2_ty, slot, &[self.i64c(0), self.i32c(1)], "v2.p")
                .unwrap()
        };
        self.builder.build_store(pay_ptr, payload).unwrap();
    }

    /// Copy a 16-byte `ValueV2` by value from `src` to `dst`.
    fn copy_v2(&mut self, dst: PointerValue<'ctx>, src: PointerValue<'ctx>) {
        let v = self
            .builder
            .build_load(self.val_v2_ty(), src, "v2.cp")
            .unwrap();
        self.builder.build_store(dst, v).unwrap();
    }

    /// Allocate a new `UpvalueSlot` initialized from a `ValueV2` pointer.
    fn cell_new(&mut self, v: PointerValue<'ctx>) -> PointerValue<'ctx> {
        let f = self.tramp(
            "spid3r_cell_new",
            &[self.ptr_ty().into()],
            Some(self.ptr_ty().into()),
        );
        call_ptr(self.emit_call(f, &[v.into()], "cell"))
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
        if matches!(name, "spid3r_is_int" | "spid3r_int_of") {
            self.add_fn_enum_attr(f, "nounwind", 0);
            self.add_fn_enum_attr(f, "willreturn", 0);
            // Both helpers only read the Value box and never mutate, lock, or throw.
            // LLVM MemoryEffects encoding: Ref (0b01) across all 4 locations = 0x55 -> `memory(read)`.
            if !self.add_fn_enum_attr(f, "memory", 0x55) {
                self.add_fn_enum_attr(f, "readonly", 0);
            }
        }
        if matches!(
            name,
            "spid3r_i64_add"
                | "spid3r_i64_sub"
                | "spid3r_i64_mul"
                | "spid3r_i64_div"
                | "spid3r_i64_mod"
                | "spid3r_i64_neg"
                | "spid3r_cancel_check"
                | "spid3r_budget_exhausted"
                | "spid3r_uncaught"
                | "spid3r_abort"
        ) {
            self.add_fn_enum_attr(f, "cold", 0);
        }
        self.tramps.insert(name.to_string(), f);
        f
    }

    fn expect_i1(
        &mut self,
        val: inkwell::values::IntValue<'ctx>,
        expected: bool,
    ) -> inkwell::values::IntValue<'ctx> {
        let f = if let Some(f) = self.tramps.get("llvm.expect.i1") {
            *f
        } else {
            let b = self.ctx.bool_type();
            let ft = b.fn_type(&[b.into(), b.into()], false);
            let f = self.module.add_function("llvm.expect.i1", ft, None);
            self.tramps.insert("llvm.expect.i1".to_string(), f);
            f
        };
        let exp = self.ctx.bool_type().const_int(u64::from(expected), false);
        let name = self.tmp_name("exp");
        let call = self
            .builder
            .build_call(f, &[val.into(), exp.into()], &name)
            .unwrap();
        call_int(call)
    }

    fn add_fn_enum_attr(&self, f: FunctionValue<'ctx>, kind: &str, val: u64) -> bool {
        let id = inkwell::attributes::Attribute::get_named_enum_kind_id(kind);
        if id != 0 {
            let attr = self.ctx.create_enum_attribute(id, val);
            f.add_attribute(inkwell::attributes::AttributeLoc::Function, attr);
            true
        } else {
            false
        }
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
        let out = self.alloc_v2_slot();
        let f = self.tramp(name, &[self.ptr_ty().into()], None);
        self.builder.build_call(f, &[out.into()], "").unwrap();
        out
    }

    fn call1p(&mut self, name: &str, a: PointerValue<'ctx>) -> PointerValue<'ctx> {
        if name == "spid3r_cell_new" {
            return self.cell_new(a);
        }
        let out = self.alloc_v2_slot();
        let f = self.tramp(name, &[self.ptr_ty().into(), self.ptr_ty().into()], None);
        self.builder
            .build_call(f, &[out.into(), a.into()], "")
            .unwrap();
        out
    }

    fn call1p_i64(&mut self, name: &str, a: PointerValue<'ctx>, n: i64) -> PointerValue<'ctx> {
        let out = self.alloc_v2_slot();
        let f = self.tramp(
            name,
            &[
                self.ptr_ty().into(),
                self.ptr_ty().into(),
                self.i64_ty().into(),
            ],
            None,
        );
        let n = self.i64c(n);
        self.builder
            .build_call(f, &[out.into(), a.into(), n.into()], "")
            .unwrap();
        out
    }

    fn call2p(
        &mut self,
        name: &str,
        a: PointerValue<'ctx>,
        b: PointerValue<'ctx>,
    ) -> PointerValue<'ctx> {
        let out = self.alloc_v2_slot();
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
            .build_call(f, &[out.into(), a.into(), b.into()], "")
            .unwrap();
        out
    }

    fn call3p(
        &mut self,
        name: &str,
        a: PointerValue<'ctx>,
        b: PointerValue<'ctx>,
        c: PointerValue<'ctx>,
    ) -> PointerValue<'ctx> {
        let out = self.alloc_v2_slot();
        let f = self.tramp(
            name,
            &[
                self.ptr_ty().into(),
                self.ptr_ty().into(),
                self.ptr_ty().into(),
                self.ptr_ty().into(),
            ],
            None,
        );
        self.builder
            .build_call(f, &[out.into(), a.into(), b.into(), c.into()], "")
            .unwrap();
        out
    }

    fn call_binop(
        &mut self,
        a: PointerValue<'ctx>,
        b: PointerValue<'ctx>,
        op: u8,
    ) -> PointerValue<'ctx> {
        let out = self.alloc_v2_slot();
        let f = self.tramp(
            "spid3r_binop",
            &[
                self.ptr_ty().into(),
                self.ptr_ty().into(),
                self.ptr_ty().into(),
                self.i64_ty().into(),
            ],
            None,
        );
        let op = self.i64c(op as i64);
        self.builder
            .build_call(f, &[out.into(), a.into(), b.into(), op.into()], "")
            .unwrap();
        out
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

    fn call1p_to_i64(
        &mut self,
        name: &str,
        a: PointerValue<'ctx>,
    ) -> inkwell::values::IntValue<'ctx> {
        let f = self.tramp(name, &[self.ptr_ty().into()], Some(self.i64_ty().into()));
        let r = self.emit_call(f, &[a.into()], "call");
        call_int(r)
    }

    // -------------------------------------------------------- value makers

    fn v_int_from_i64(&mut self, v: inkwell::values::IntValue<'ctx>) -> PointerValue<'ctx> {
        let out = self.alloc_v2_slot();
        self.store_v2_inline(out, crate::value::ValueV2::TAG_INT, v);
        out
    }

    fn v_int(&mut self, v: i64) -> PointerValue<'ctx> {
        let v_c = self.i64c(v);
        self.v_int_from_i64(v_c)
    }

    fn v_float_from_f64(&mut self, v: inkwell::values::FloatValue<'ctx>) -> PointerValue<'ctx> {
        let out = self.alloc_v2_slot();
        let bits = self
            .builder
            .build_bit_cast(v, self.i64_ty(), "fbits")
            .unwrap()
            .into_int_value();
        self.store_v2_inline(out, crate::value::ValueV2::TAG_FLOAT, bits);
        out
    }

    fn v_float(&mut self, v: f64) -> PointerValue<'ctx> {
        let v_c = self.ctx.f64_type().const_float(v);
        self.v_float_from_f64(v_c)
    }

    fn v_bool_from_i1(&mut self, c: inkwell::values::IntValue<'ctx>) -> PointerValue<'ctx> {
        let out = self.alloc_v2_slot();
        let c_i64 = self
            .builder
            .build_int_z_extend(c, self.i64_ty(), "b64")
            .unwrap();
        self.store_v2_inline(out, crate::value::ValueV2::TAG_BOOL, c_i64);
        out
    }

    fn v_bool(&mut self, v: bool) -> PointerValue<'ctx> {
        let v_c = self.i1(v);
        self.v_bool_from_i1(v_c)
    }

    fn v_null(&mut self) -> PointerValue<'ctx> {
        let out = self.alloc_v2_slot();
        self.store_v2_inline(out, crate::value::ValueV2::TAG_NULL, self.i64c(0));
        out
    }

    fn v_str(&mut self, s: &str) -> PointerValue<'ctx> {
        let out = self.alloc_v2_slot();
        let b = s.as_bytes();
        if b.len() <= 6 || (b.len() == 7 && b[6] < 128) {
            let v2 = crate::value::ValueV2::from_sso_bytes(b);
            self.store_v2_inline(out, v2.tag, self.i64c(0));
            return out;
        }
        let (gptr, len) = self.str_global(s);
        let f = self.tramp(
            "spid3r_value_str",
            &[
                self.ptr_ty().into(),
                self.ptr_ty().into(),
                self.i64_ty().into(),
            ],
            None,
        );
        let len = self.i64c(len as i64);
        self.builder
            .build_call(f, &[out.into(), gptr.into(), len.into()], "")
            .unwrap();
        out
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

    /// Initialize an entry-block slot to zero EXACTLY ONCE. The store must
    /// go in the entry block (before its terminator): a store in the loop
    /// body would reset the counter on every iteration, so the poll would
    /// never fire.
    fn init_entry_slot(
        &mut self,
        slot: PointerValue<'ctx>,
        value: inkwell::values::IntValue<'ctx>,
    ) {
        let f = self.current_fn();
        let entry = f
            .get_first_basic_block()
            .expect("every function starts with an entry block");
        let cur = self.cur_block();
        match entry.get_last_instruction() {
            Some(term) => self.builder.position_before(&term),
            None => self.builder.position_at_end(entry),
        }
        self.builder.build_store(slot, value).unwrap();
        if let Some(b) = cur {
            self.builder.position_at_end(b);
        }
    }

    /// Emit the cancellation poll used inside loop bodies. A per-loop
    /// counter triggers a runtime check every CANCEL_POLL_INTERVAL
    /// iterations, so a cancelled native task stops promptly without paying
    /// a runtime call on every iteration.
    fn emit_cancel_poll(&mut self, counter: PointerValue<'ctx>) {
        const CANCEL_POLL_INTERVAL: i64 = 2048;
        let one = self.i64c(1);
        let cur = self
            .builder
            .build_load(self.i64_ty(), counter, "cancel.n")
            .unwrap()
            .into_int_value();
        let next = self.builder.build_int_add(cur, one, "cancel.n1").unwrap();
        self.builder.build_store(counter, next).unwrap();
        let limit = self.i64c(CANCEL_POLL_INTERVAL);
        let hit_raw = self
            .builder
            .build_int_compare(IntPredicate::UGE, next, limit, "cancel.hit")
            .unwrap();
        let hit = self.expect_i1(hit_raw, false);
        let cont = self.new_block("cancel.cont");
        let check = self.new_block("cancel.check");
        self.builder
            .build_conditional_branch(hit, check, cont)
            .unwrap();
        self.builder.position_at_end(check);
        self.builder.build_store(counter, self.i64c(0)).unwrap();
        let f = self.tramp("spid3r_cancel_check", &[], Some(self.i8_ty().into()));
        let _r = self.emit_call(f, &[], "cancel.chk");
        // The check raises "task cancelled" through the normal throw path;
        // throw_guard routes it to the handler (or out of the function).
        self.throw_guard();
        self.builder.build_unconditional_branch(cont).unwrap();
        self.builder.position_at_end(cont);
        if let Some(bgv) = self.budget_counter_gv {
            let bp = bgv.as_pointer_value();
            let rem = self
                .builder
                .build_load(self.i64_ty(), bp, "bud.rem")
                .unwrap()
                .into_int_value();
            let rem1 = self
                .builder
                .build_int_sub(rem, self.i64c(1), "bud.dec")
                .unwrap();
            self.builder.build_store(bp, rem1).unwrap();
            let exhausted_raw = self
                .builder
                .build_int_compare(
                    IntPredicate::SLE,
                    rem1,
                    self.i64_ty().const_zero(),
                    "bud.hit",
                )
                .unwrap();
            let exhausted = self.expect_i1(exhausted_raw, false);
            let bud_fail = self.new_block("bud.fail");
            let bud_ok = self.new_block("bud.ok");
            self.builder
                .build_conditional_branch(exhausted, bud_fail, bud_ok)
                .unwrap();
            self.builder.position_at_end(bud_fail);
            let (msg, _) = self.str_global("execution budget exhausted");
            let abort_f = self.tramp("spid3r_abort", &[self.ptr_ty().into()], None);
            self.emit_call(abort_f, &[msg.into()], "bud.abort");
            self.builder.build_unreachable().unwrap();
            self.builder.position_at_end(bud_ok);
        }
    }

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
        self.call1p("spid3r_cell_get", slot)
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
        // The yielded expression may itself contain a closure literal.
        Stmt::Yield { value, .. } => nested_fns_expr(value, out),
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
        Expr::NamedArg { value, .. } => {
            nested_fns_expr(value, out);
        }
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

fn is_pure_int_expr(
    e: &Expr,
    int_vars: &HashSet<String>,
    ulists: &HashMap<String, UnboxedElem>,
) -> bool {
    static EMPTY_STRS: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
    let empty = EMPTY_STRS.get_or_init(HashSet::new);
    is_pure_int_expr_with_strs(e, int_vars, ulists, empty)
}

fn is_pure_int_expr_with_strs(
    e: &Expr,
    int_vars: &HashSet<String>,
    ulists: &HashMap<String, UnboxedElem>,
    str_vars: &HashSet<String>,
) -> bool {
    match e {
        Expr::Int(..) => true,
        Expr::Ident(name, _) => int_vars.contains(name),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
            ..
        } => is_pure_int_expr_with_strs(expr, int_vars, ulists, str_vars),
        Expr::Binary {
            op: BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod,
            l,
            r,
            ..
        } => {
            is_pure_int_expr_with_strs(l, int_vars, ulists, str_vars)
                && is_pure_int_expr_with_strs(r, int_vars, ulists, str_vars)
        }
        Expr::Index { object, index, .. } => {
            matches!(&**object, Expr::Ident(n, _) if ulists.get(n) == Some(&UnboxedElem::I64))
                && is_pure_int_expr_with_strs(index, int_vars, ulists, str_vars)
        }
        Expr::Method {
            object, name, args, ..
        } if args.is_empty() => {
            if let Expr::Ident(n, _) = &**object {
                match ulists.get(n) {
                    Some(UnboxedElem::I64) => name == "len" || name == "pop",
                    Some(UnboxedElem::F64) => name == "len",
                    None => name == "len" && str_vars.contains(n),
                }
            } else {
                false
            }
        }
        _ => false,
    }
}

struct IntAnalysisInit<'a> {
    name: String,
    expr: &'a Expr,
    scoped_ints: HashSet<String>,
    scoped_floats: HashSet<String>,
}

struct IntAnalysisAssign<'a> {
    name: String,
    op: AssignOp,
    expr: &'a Expr,
    scoped_ints: HashSet<String>,
    scoped_floats: HashSet<String>,
}

#[derive(Default)]
struct IntAnalysisAcc<'a> {
    decl_counts: HashMap<String, usize>,
    candidates: HashSet<String>,
    invalid: HashSet<String>,
    inits: Vec<IntAnalysisInit<'a>>,
    assignments: Vec<IntAnalysisAssign<'a>>,
    scoped_ints: HashSet<String>,
    scoped_floats: HashSet<String>,
}

fn collect_int_analysis_block<'a>(
    stmts: &'a [Stmt],
    boxed: &HashSet<String>,
    ulists: &HashMap<String, UnboxedElem>,
    acc: &mut IntAnalysisAcc<'a>,
) {
    let saved_ints = acc.scoped_ints.clone();
    let saved_floats = acc.scoped_floats.clone();
    for s in stmts {
        collect_int_analysis_stmt(s, boxed, ulists, acc);
    }
    acc.scoped_ints = saved_ints;
    acc.scoped_floats = saved_floats;
}

fn collect_int_analysis_stmt<'a>(
    s: &'a Stmt,
    boxed: &HashSet<String>,
    ulists: &HashMap<String, UnboxedElem>,
    acc: &mut IntAnalysisAcc<'a>,
) {
    match s {
        Stmt::Let { name, value, .. } => {
            *acc.decl_counts.entry(name.clone()).or_insert(0) += 1;
            if boxed.contains(name) {
                acc.invalid.insert(name.clone());
            } else if let Some(v) = value {
                acc.candidates.insert(name.clone());
                acc.inits.push(IntAnalysisInit {
                    name: name.clone(),
                    expr: v,
                    scoped_ints: acc.scoped_ints.clone(),
                    scoped_floats: acc.scoped_floats.clone(),
                });
                collect_int_analysis_expr(v, acc);
            } else {
                acc.invalid.insert(name.clone());
            }
            acc.scoped_ints.remove(name);
            acc.scoped_floats.remove(name);
        }
        Stmt::Const { name, value, .. } => {
            *acc.decl_counts.entry(name.clone()).or_insert(0) += 1;
            if boxed.contains(name) {
                acc.invalid.insert(name.clone());
            } else {
                acc.candidates.insert(name.clone());
                acc.inits.push(IntAnalysisInit {
                    name: name.clone(),
                    expr: value,
                    scoped_ints: acc.scoped_ints.clone(),
                    scoped_floats: acc.scoped_floats.clone(),
                });
                collect_int_analysis_expr(value, acc);
            }
            acc.scoped_ints.remove(name);
            acc.scoped_floats.remove(name);
        }
        Stmt::LetTuple { names, value, .. } => {
            collect_int_analysis_expr(value, acc);
            for n in names {
                acc.invalid.insert(n.clone());
                acc.scoped_ints.remove(n);
                acc.scoped_floats.remove(n);
            }
        }
        Stmt::If {
            cond,
            then,
            elifs,
            els,
            ..
        } => {
            collect_int_analysis_expr(cond, acc);
            collect_int_analysis_block(&then.stmts, boxed, ulists, acc);
            for (c, b) in elifs {
                collect_int_analysis_expr(c, acc);
                collect_int_analysis_block(&b.stmts, boxed, ulists, acc);
            }
            if let Some(b) = els {
                collect_int_analysis_block(&b.stmts, boxed, ulists, acc);
            }
        }
        Stmt::While { cond, body, .. } => {
            collect_int_analysis_expr(cond, acc);
            collect_int_analysis_block(&body.stmts, boxed, ulists, acc);
        }
        Stmt::ForIn {
            var,
            var2,
            iter,
            body,
            ..
        } => {
            acc.invalid.insert(var.clone());
            if let Some(v2) = var2 {
                acc.invalid.insert(v2.clone());
            }
            collect_int_analysis_expr(iter, acc);
            let unbox_elem = if var2.is_none()
                && !boxed.contains(var)
                && !stmt_mentions_assign_target(&Stmt::Block(body.clone()), var)
            {
                if let Expr::Ident(list_name, _) = iter {
                    if !ulist_mutated_in_stmts(&body.stmts, list_name) {
                        ulists.get(list_name).copied()
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            let saved_ints = acc.scoped_ints.clone();
            let saved_floats = acc.scoped_floats.clone();
            acc.scoped_ints.remove(var);
            acc.scoped_floats.remove(var);
            if let Some(v2) = var2 {
                acc.scoped_ints.remove(v2);
                acc.scoped_floats.remove(v2);
            }
            match unbox_elem {
                Some(UnboxedElem::I64) => {
                    acc.scoped_ints.insert(var.clone());
                }
                Some(UnboxedElem::F64) => {
                    acc.scoped_floats.insert(var.clone());
                }
                None => {}
            }
            collect_int_analysis_block(&body.stmts, boxed, ulists, acc);
            acc.scoped_ints = saved_ints;
            acc.scoped_floats = saved_floats;
        }
        Stmt::ForC {
            init,
            cond,
            post,
            body,
            ..
        } => {
            let saved_ints = acc.scoped_ints.clone();
            let saved_floats = acc.scoped_floats.clone();
            collect_int_analysis_stmt(init, boxed, ulists, acc);
            collect_int_analysis_expr(cond, acc);
            collect_int_analysis_expr(post, acc);
            collect_int_analysis_block(&body.stmts, boxed, ulists, acc);
            acc.scoped_ints = saved_ints;
            acc.scoped_floats = saved_floats;
        }
        Stmt::Return { value: Some(v), .. } => {
            collect_int_analysis_expr(v, acc);
        }
        Stmt::Return { value: None, .. } => {}
        Stmt::Block(b) => {
            collect_int_analysis_block(&b.stmts, boxed, ulists, acc);
        }
        Stmt::Expr(e) => {
            collect_int_analysis_expr(e, acc);
        }
        Stmt::Spawn { callable, .. } => {
            collect_int_analysis_expr(callable, acc);
        }
        Stmt::Try {
            body,
            catch,
            finally,
            ..
        } => {
            collect_int_analysis_block(&body.stmts, boxed, ulists, acc);
            if let Some(c) = catch {
                acc.invalid.insert(c.name.clone());
                let saved_ints = acc.scoped_ints.clone();
                let saved_floats = acc.scoped_floats.clone();
                acc.scoped_ints.remove(&c.name);
                acc.scoped_floats.remove(&c.name);
                collect_int_analysis_block(&c.body.stmts, boxed, ulists, acc);
                acc.scoped_ints = saved_ints;
                acc.scoped_floats = saved_floats;
            }
            if let Some(f) = finally {
                collect_int_analysis_block(&f.stmts, boxed, ulists, acc);
            }
        }
        Stmt::Throw { value, .. } => {
            collect_int_analysis_expr(value, acc);
        }
        Stmt::Yield { value, .. } => {
            collect_int_analysis_expr(value, acc);
        }
        _ => {}
    }
}

fn collect_int_analysis_expr<'a>(e: &'a Expr, acc: &mut IntAnalysisAcc<'a>) {
    match e {
        Expr::Assign {
            target, op, value, ..
        } => {
            if let Expr::Ident(name, _) = target.as_ref() {
                acc.assignments.push(IntAnalysisAssign {
                    name: name.clone(),
                    op: *op,
                    expr: value.as_ref(),
                    scoped_ints: acc.scoped_ints.clone(),
                    scoped_floats: acc.scoped_floats.clone(),
                });
            } else {
                let mut cur = target.as_ref();
                while let Expr::Field { object, .. }
                | Expr::Index { object, .. }
                | Expr::OptField { object, .. } = cur
                {
                    cur = object.as_ref();
                }
                if let Expr::Ident(name, _) = cur {
                    acc.invalid.insert(name.clone());
                }
                collect_int_analysis_expr(target, acc);
            }
            collect_int_analysis_expr(value, acc);
        }
        Expr::Unary { expr, .. } => collect_int_analysis_expr(expr, acc),
        Expr::Binary { l, r, .. } | Expr::Logical { l, r, .. } | Expr::Coalesce { l, r, .. } => {
            collect_int_analysis_expr(l, acc);
            collect_int_analysis_expr(r, acc);
        }
        Expr::Call { callee, args, .. } => {
            collect_int_analysis_expr(callee, acc);
            for a in args {
                collect_int_analysis_expr(a, acc);
            }
        }
        Expr::Method { object, args, .. } => {
            collect_int_analysis_expr(object, acc);
            for a in args {
                collect_int_analysis_expr(a, acc);
            }
        }
        Expr::Index { object, index, .. } => {
            collect_int_analysis_expr(object, acc);
            collect_int_analysis_expr(index, acc);
        }
        Expr::Field { object, .. } | Expr::OptField { object, .. } => {
            collect_int_analysis_expr(object, acc);
        }
        Expr::List(items, _) => {
            for i in items {
                collect_int_analysis_expr(i, acc);
            }
        }
        Expr::New { args, .. } => {
            for a in args {
                collect_int_analysis_expr(a, acc);
            }
        }
        Expr::SuperCall { args, .. } => {
            for a in args {
                collect_int_analysis_expr(a, acc);
            }
        }
        Expr::NamedArg { value, .. }
        | Expr::Propagate(value, _)
        | Expr::Await(value, _)
        | Expr::Spawn {
            callable: value, ..
        } => {
            collect_int_analysis_expr(value, acc);
        }
        _ => {}
    }
}

fn is_pure_str_expr(e: &Expr, str_vars: &HashSet<String>, shadowed: &HashSet<String>) -> bool {
    match e {
        Expr::Str(..) => true,
        Expr::Ident(name, _) => str_vars.contains(name),
        Expr::Call {
            callee,
            type_args,
            args,
            ..
        } if type_args.is_empty() && args.len() == 1 => matches!(
            callee.as_ref(),
            Expr::Ident(fn_name, _)
                if (fn_name == "string" || fn_name == "json_write")
                    && !shadowed.contains(fn_name)
        ),
        Expr::Binary {
            op: BinOp::Add, l, ..
        } => is_pure_str_expr(l, str_vars, shadowed),
        _ => false,
    }
}

fn analyze_str_locals(
    proto: &FnProto,
    boxed: &HashSet<String>,
    shadowed: &HashSet<String>,
) -> HashSet<String> {
    let empty_ulists = HashMap::new();
    let mut acc = IntAnalysisAcc::default();
    for p in &proto.params {
        acc.invalid.insert(p.name.clone());
    }
    for b in boxed {
        acc.invalid.insert(b.clone());
    }
    collect_int_analysis_block(&proto.body.stmts, boxed, &empty_ulists, &mut acc);
    for (name, count) in acc.decl_counts {
        if count > 1 {
            acc.invalid.insert(name);
        }
    }
    let mut str_vars: HashSet<String> = acc.candidates.difference(&acc.invalid).cloned().collect();
    loop {
        let mut to_remove = Vec::new();
        for name in &str_vars {
            let mut ok = true;
            for init in &acc.inits {
                if &init.name == name && !is_pure_str_expr(init.expr, &str_vars, shadowed) {
                    ok = false;
                    break;
                }
            }
            if ok {
                for assign in &acc.assignments {
                    if &assign.name == name
                        && (!matches!(assign.op, AssignOp::Assign | AssignOp::AddAssign)
                            || !is_pure_str_expr(assign.expr, &str_vars, shadowed))
                    {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                to_remove.push(name.clone());
            }
        }
        if to_remove.is_empty() {
            break;
        }
        for r in to_remove {
            str_vars.remove(&r);
        }
    }
    str_vars
}

fn analyze_int_locals_with_params(
    proto: &FnProto,
    boxed: &HashSet<String>,
    int_params: &HashSet<String>,
    ulists: &HashMap<String, UnboxedElem>,
    str_vars: &HashSet<String>,
) -> HashSet<String> {
    let mut acc = IntAnalysisAcc::default();

    for p in &proto.params {
        if int_params.contains(&p.name) && !boxed.contains(&p.name) {
            *acc.decl_counts.entry(p.name.clone()).or_insert(0) += 1;
            acc.candidates.insert(p.name.clone());
        } else {
            acc.invalid.insert(p.name.clone());
        }
    }
    for b in boxed {
        acc.invalid.insert(b.clone());
    }

    collect_int_analysis_block(&proto.body.stmts, boxed, ulists, &mut acc);

    for (name, count) in acc.decl_counts {
        if count > 1 {
            acc.invalid.insert(name);
        }
    }

    let mut int_vars: HashSet<String> = acc.candidates.difference(&acc.invalid).cloned().collect();

    loop {
        let mut to_remove = Vec::new();
        for name in &int_vars {
            let mut ok = true;
            for init in &acc.inits {
                if &init.name == name {
                    let pure = if init.scoped_ints.is_empty() {
                        is_pure_int_expr_with_strs(init.expr, &int_vars, ulists, str_vars)
                    } else {
                        let mut eff = int_vars.clone();
                        eff.extend(init.scoped_ints.iter().cloned());
                        is_pure_int_expr_with_strs(init.expr, &eff, ulists, str_vars)
                    };
                    if !pure {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                for assign in &acc.assignments {
                    if &assign.name == name {
                        let pure = if assign.scoped_ints.is_empty() {
                            is_pure_int_expr_with_strs(assign.expr, &int_vars, ulists, str_vars)
                        } else {
                            let mut eff = int_vars.clone();
                            eff.extend(assign.scoped_ints.iter().cloned());
                            is_pure_int_expr_with_strs(assign.expr, &eff, ulists, str_vars)
                        };
                        if !pure {
                            ok = false;
                            break;
                        }
                    }
                }
            }
            if !ok {
                to_remove.push(name.clone());
            }
        }
        if to_remove.is_empty() {
            break;
        }
        for r in to_remove {
            int_vars.remove(&r);
        }
    }

    int_vars
}

fn is_pure_float_expr(
    e: &Expr,
    float_vars: &HashSet<String>,
    int_vars: &HashSet<String>,
    ulists: &HashMap<String, UnboxedElem>,
) -> bool {
    match e {
        Expr::Float(..) => true,
        Expr::Ident(name, _) => float_vars.contains(name),
        Expr::Unary {
            op: UnOp::Neg,
            expr,
            ..
        } => is_pure_float_expr(expr, float_vars, int_vars, ulists),
        Expr::Binary {
            op: BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div,
            l,
            r,
            ..
        } => {
            let lf = is_pure_float_expr(l, float_vars, int_vars, ulists);
            let rf = is_pure_float_expr(r, float_vars, int_vars, ulists);
            let li = is_pure_int_expr(l, int_vars, ulists);
            let ri = is_pure_int_expr(r, int_vars, ulists);
            (lf && (rf || ri)) || (li && rf)
        }
        Expr::Index { object, index, .. } => {
            matches!(&**object, Expr::Ident(n, _) if ulists.get(n) == Some(&UnboxedElem::F64))
                && is_pure_int_expr(index, int_vars, ulists)
        }
        Expr::Method {
            object, name, args, ..
        } if args.is_empty() && name == "pop" => {
            matches!(&**object, Expr::Ident(n, _) if ulists.get(n) == Some(&UnboxedElem::F64))
        }
        _ => false,
    }
}

fn analyze_float_locals(
    proto: &FnProto,
    boxed: &HashSet<String>,
    int_locals: &HashSet<String>,
    float_params: &HashSet<String>,
    ulists: &HashMap<String, UnboxedElem>,
) -> HashSet<String> {
    let mut acc = IntAnalysisAcc::default();

    for p in &proto.params {
        if float_params.contains(&p.name)
            && !boxed.contains(&p.name)
            && !int_locals.contains(&p.name)
        {
            *acc.decl_counts.entry(p.name.clone()).or_insert(0) += 1;
            acc.candidates.insert(p.name.clone());
        } else {
            acc.invalid.insert(p.name.clone());
        }
    }
    for b in boxed {
        acc.invalid.insert(b.clone());
    }
    for iv in int_locals {
        acc.invalid.insert(iv.clone());
    }

    collect_int_analysis_block(&proto.body.stmts, boxed, ulists, &mut acc);

    for (name, count) in acc.decl_counts {
        if count > 1 {
            acc.invalid.insert(name);
        }
    }

    let mut float_vars: HashSet<String> =
        acc.candidates.difference(&acc.invalid).cloned().collect();

    loop {
        let mut to_remove = Vec::new();
        for name in &float_vars {
            let mut ok = true;
            for init in &acc.inits {
                if &init.name == name {
                    let pure = if init.scoped_floats.is_empty() && init.scoped_ints.is_empty() {
                        is_pure_float_expr(init.expr, &float_vars, int_locals, ulists)
                    } else {
                        let mut eff_f = float_vars.clone();
                        eff_f.extend(init.scoped_floats.iter().cloned());
                        let mut eff_i = int_locals.clone();
                        eff_i.extend(init.scoped_ints.iter().cloned());
                        is_pure_float_expr(init.expr, &eff_f, &eff_i, ulists)
                    };
                    if !pure {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                for assign in &acc.assignments {
                    if &assign.name == name {
                        if assign.op == AssignOp::ModAssign {
                            ok = false;
                            break;
                        }
                        let pure =
                            if assign.scoped_floats.is_empty() && assign.scoped_ints.is_empty() {
                                is_pure_float_expr(assign.expr, &float_vars, int_locals, ulists)
                            } else {
                                let mut eff_f = float_vars.clone();
                                eff_f.extend(assign.scoped_floats.iter().cloned());
                                let mut eff_i = int_locals.clone();
                                eff_i.extend(assign.scoped_ints.iter().cloned());
                                is_pure_float_expr(assign.expr, &eff_f, &eff_i, ulists)
                            };
                        if !pure {
                            ok = false;
                            break;
                        }
                    }
                }
            }
            if !ok {
                to_remove.push(name.clone());
            }
        }
        if to_remove.is_empty() {
            break;
        }
        for r in to_remove {
            float_vars.remove(&r);
        }
    }

    float_vars
}

fn ulist_mutated_in_stmts(stmts: &[Stmt], name: &str) -> bool {
    fn expr_mutates(e: &Expr, name: &str) -> bool {
        match e {
            Expr::Method {
                object,
                name: mname,
                args,
                ..
            } => {
                if matches!(&**object, Expr::Ident(v, _) if v == name)
                    && matches!(mname.as_str(), "push" | "pop")
                {
                    return true;
                }
                expr_mutates(object, name) || args.iter().any(|a| expr_mutates(a, name))
            }
            Expr::Assign { target, value, .. } => {
                if let Expr::Index { object, .. } = target.as_ref() {
                    if matches!(&**object, Expr::Ident(v, _) if v == name) {
                        return true;
                    }
                }
                expr_mutates(target, name) || expr_mutates(value, name)
            }
            Expr::Unary { expr, .. } | Expr::Await(expr, _) => expr_mutates(expr, name),
            Expr::Binary { l, r, .. } | Expr::Logical { l, r, .. } => {
                expr_mutates(l, name) || expr_mutates(r, name)
            }
            Expr::Call { callee, args, .. } => {
                expr_mutates(callee, name) || args.iter().any(|a| expr_mutates(a, name))
            }
            Expr::Index { object, index, .. } => {
                expr_mutates(object, name) || expr_mutates(index, name)
            }
            Expr::List(items, _) => items.iter().any(|a| expr_mutates(a, name)),
            _ => false,
        }
    }
    stmts.iter().any(|s| match s {
        Stmt::Expr(e) | Stmt::Return { value: Some(e), .. } | Stmt::Throw { value: e, .. } => {
            expr_mutates(e, name)
        }
        Stmt::Let { value: Some(e), .. } | Stmt::Const { value: e, .. } => expr_mutates(e, name),
        Stmt::If {
            cond,
            then,
            elifs,
            els,
            ..
        } => {
            expr_mutates(cond, name)
                || ulist_mutated_in_stmts(&then.stmts, name)
                || elifs
                    .iter()
                    .any(|(c, b)| expr_mutates(c, name) || ulist_mutated_in_stmts(&b.stmts, name))
                || els
                    .as_ref()
                    .is_some_and(|b| ulist_mutated_in_stmts(&b.stmts, name))
        }
        Stmt::While { cond, body, .. } => {
            expr_mutates(cond, name) || ulist_mutated_in_stmts(&body.stmts, name)
        }
        Stmt::ForIn { iter, body, .. } => {
            expr_mutates(iter, name) || ulist_mutated_in_stmts(&body.stmts, name)
        }
        Stmt::ForC {
            init,
            cond,
            post,
            body,
            ..
        } => {
            ulist_mutated_in_stmts(std::slice::from_ref(init), name)
                || expr_mutates(cond, name)
                || expr_mutates(post, name)
                || ulist_mutated_in_stmts(&body.stmts, name)
        }
        Stmt::Block(b) => ulist_mutated_in_stmts(&b.stmts, name),
        Stmt::Try {
            body,
            catch,
            finally,
            ..
        } => {
            ulist_mutated_in_stmts(&body.stmts, name)
                || catch
                    .as_ref()
                    .is_some_and(|c| ulist_mutated_in_stmts(&c.body.stmts, name))
                || finally
                    .as_ref()
                    .is_some_and(|f| ulist_mutated_in_stmts(&f.stmts, name))
        }
        _ => false,
    })
}

/// R10.3: collect local `let xs: list<int> = ...` / `list<float>` variables
/// (plus unannotated homogeneous `int` literal lists whose mutations stay `int`)
/// that are never reassigned, never captured, and never escape as whole-list
/// values (only `.push`, `.len()`, `.pop()`, `xs[i]`, `xs[i] = v`, or `for x in xs`).
fn analyze_unboxed_lists(
    proto: &FnProto,
    boxed: &HashSet<String>,
    int_locals: &HashSet<String>,
) -> HashMap<String, UnboxedElem> {
    let mut cands: HashMap<String, UnboxedElem> = HashMap::new();
    let mut shadowed: HashSet<String> = HashSet::new();
    let mut inferred_ints: HashSet<String> = HashSet::new();
    let mut non_escaping_idents: HashSet<usize> = HashSet::new();

    fn record_cands(
        stmts: &[Stmt],
        boxed: &HashSet<String>,
        int_locals: &HashSet<String>,
        cands: &mut HashMap<String, UnboxedElem>,
        shadowed: &mut HashSet<String>,
        inferred_ints: &mut HashSet<String>,
    ) {
        for s in stmts {
            match s {
                Stmt::Let {
                    name,
                    ty: Some(te),
                    value,
                    ..
                } => {
                    if cands.contains_key(name) {
                        shadowed.insert(name.clone());
                        continue;
                    }
                    if !boxed.contains(name)
                        && te.name.eq_ignore_ascii_case("list")
                        && te.args.len() == 1
                        && !te.nullable
                        && te.union.is_empty()
                    {
                        let elem_te = &te.args[0];
                        if elem_te.args.is_empty() && !elem_te.nullable && elem_te.union.is_empty()
                        {
                            let elem = if elem_te.name == "int" {
                                Some(UnboxedElem::I64)
                            } else if elem_te.name == "float" {
                                Some(UnboxedElem::F64)
                            } else {
                                None
                            };
                            let init_ok = match value {
                                None => true,
                                Some(Expr::List(..)) => true,
                                Some(Expr::Call { callee, args, .. })
                                    if matches!(&**callee, Expr::Ident(n, _) if n == "list")
                                        && args.is_empty() =>
                                {
                                    true
                                }
                                _ => false,
                            };
                            if let (Some(el), true) = (elem, init_ok) {
                                cands.insert(name.clone(), el);
                            }
                        }
                    }
                }
                Stmt::Let {
                    name,
                    ty: None,
                    value: Some(Expr::List(items, _)),
                    ..
                } => {
                    if cands.contains_key(name) {
                        shadowed.insert(name.clone());
                        continue;
                    }
                    if !boxed.contains(name)
                        && !items.is_empty()
                        && items
                            .iter()
                            .all(|it| is_pure_int_expr(it, int_locals, &HashMap::new()))
                    {
                        cands.insert(name.clone(), UnboxedElem::I64);
                        inferred_ints.insert(name.clone());
                    }
                }
                Stmt::Let { name, .. } | Stmt::Const { name, .. } => {
                    if cands.contains_key(name) {
                        shadowed.insert(name.clone());
                    }
                }
                Stmt::If {
                    then, elifs, els, ..
                } => {
                    record_cands(
                        &then.stmts,
                        boxed,
                        int_locals,
                        cands,
                        shadowed,
                        inferred_ints,
                    );
                    for (_, b) in elifs {
                        record_cands(&b.stmts, boxed, int_locals, cands, shadowed, inferred_ints);
                    }
                    if let Some(b) = els {
                        record_cands(&b.stmts, boxed, int_locals, cands, shadowed, inferred_ints);
                    }
                }
                Stmt::While { body, .. } => {
                    record_cands(
                        &body.stmts,
                        boxed,
                        int_locals,
                        cands,
                        shadowed,
                        inferred_ints,
                    );
                }
                Stmt::Block(b) => {
                    record_cands(&b.stmts, boxed, int_locals, cands, shadowed, inferred_ints);
                }
                Stmt::ForC { init, body, .. } => {
                    record_cands(
                        std::slice::from_ref(init),
                        boxed,
                        int_locals,
                        cands,
                        shadowed,
                        inferred_ints,
                    );
                    record_cands(
                        &body.stmts,
                        boxed,
                        int_locals,
                        cands,
                        shadowed,
                        inferred_ints,
                    );
                }
                Stmt::ForIn { var, body, .. } => {
                    if cands.contains_key(var) {
                        shadowed.insert(var.clone());
                    }
                    record_cands(
                        &body.stmts,
                        boxed,
                        int_locals,
                        cands,
                        shadowed,
                        inferred_ints,
                    );
                }
                Stmt::Try {
                    body,
                    catch,
                    finally,
                    ..
                } => {
                    record_cands(
                        &body.stmts,
                        boxed,
                        int_locals,
                        cands,
                        shadowed,
                        inferred_ints,
                    );
                    if let Some(c) = catch {
                        if cands.contains_key(&c.name) {
                            shadowed.insert(c.name.clone());
                        }
                        record_cands(
                            &c.body.stmts,
                            boxed,
                            int_locals,
                            cands,
                            shadowed,
                            inferred_ints,
                        );
                    }
                    if let Some(f) = finally {
                        record_cands(&f.stmts, boxed, int_locals, cands, shadowed, inferred_ints);
                    }
                }
                _ => {}
            }
        }
    }

    fn scan_expr(
        e: &Expr,
        cands: &HashMap<String, UnboxedElem>,
        inferred_ints: &HashSet<String>,
        int_locals: &HashSet<String>,
        escaped: &mut HashSet<String>,
        allowed_id_ptr: &mut HashSet<usize>,
    ) {
        match e {
            Expr::Ident(name, _) => {
                let ptr = e as *const Expr as usize;
                if cands.contains_key(name) && !allowed_id_ptr.contains(&ptr) {
                    escaped.insert(name.clone());
                }
            }
            Expr::Unary { expr, .. } | Expr::Await(expr, _) => {
                scan_expr(
                    expr,
                    cands,
                    inferred_ints,
                    int_locals,
                    escaped,
                    allowed_id_ptr,
                );
            }
            Expr::Binary { l, r, .. } | Expr::Logical { l, r, .. } => {
                scan_expr(l, cands, inferred_ints, int_locals, escaped, allowed_id_ptr);
                scan_expr(r, cands, inferred_ints, int_locals, escaped, allowed_id_ptr);
            }
            Expr::Index { object, index, .. } => {
                if let Expr::Ident(name, _) = &**object {
                    if cands.contains_key(name) {
                        allowed_id_ptr.insert(&**object as *const Expr as usize);
                    }
                }
                scan_expr(
                    object,
                    cands,
                    inferred_ints,
                    int_locals,
                    escaped,
                    allowed_id_ptr,
                );
                scan_expr(
                    index,
                    cands,
                    inferred_ints,
                    int_locals,
                    escaped,
                    allowed_id_ptr,
                );
            }
            Expr::Method {
                object, name, args, ..
            } => {
                if let Expr::Ident(vname, _) = &**object {
                    if cands.contains_key(vname)
                        && ((name == "push"
                            && args.len() == 1
                            && (!inferred_ints.contains(vname)
                                || is_pure_int_expr(&args[0], int_locals, &HashMap::new())))
                            || ((name == "len" || name == "pop") && args.is_empty()))
                    {
                        allowed_id_ptr.insert(&**object as *const Expr as usize);
                    }
                }
                scan_expr(
                    object,
                    cands,
                    inferred_ints,
                    int_locals,
                    escaped,
                    allowed_id_ptr,
                );
                for a in args {
                    scan_expr(a, cands, inferred_ints, int_locals, escaped, allowed_id_ptr);
                }
            }
            Expr::Assign {
                target, op, value, ..
            } => {
                if let Expr::Index { object, index, .. } = target.as_ref() {
                    if *op == AssignOp::Assign {
                        if let Expr::Ident(vname, _) = &**object {
                            if cands.contains_key(vname)
                                && (!inferred_ints.contains(vname)
                                    || is_pure_int_expr(value, int_locals, &HashMap::new()))
                            {
                                allowed_id_ptr.insert(&**object as *const Expr as usize);
                            }
                        }
                    }
                    scan_expr(
                        object,
                        cands,
                        inferred_ints,
                        int_locals,
                        escaped,
                        allowed_id_ptr,
                    );
                    scan_expr(
                        index,
                        cands,
                        inferred_ints,
                        int_locals,
                        escaped,
                        allowed_id_ptr,
                    );
                } else {
                    scan_expr(
                        target,
                        cands,
                        inferred_ints,
                        int_locals,
                        escaped,
                        allowed_id_ptr,
                    );
                }
                scan_expr(
                    value,
                    cands,
                    inferred_ints,
                    int_locals,
                    escaped,
                    allowed_id_ptr,
                );
            }
            Expr::Call { callee, args, .. } => {
                scan_expr(
                    callee,
                    cands,
                    inferred_ints,
                    int_locals,
                    escaped,
                    allowed_id_ptr,
                );
                for a in args {
                    scan_expr(a, cands, inferred_ints, int_locals, escaped, allowed_id_ptr);
                }
            }
            Expr::New { args, .. } | Expr::SuperCall { args, .. } | Expr::List(args, _) => {
                for a in args {
                    scan_expr(a, cands, inferred_ints, int_locals, escaped, allowed_id_ptr);
                }
            }
            Expr::Field { object, .. } | Expr::OptField { object, .. } => {
                scan_expr(
                    object,
                    cands,
                    inferred_ints,
                    int_locals,
                    escaped,
                    allowed_id_ptr,
                );
            }
            _ => {}
        }
    }

    fn scan_stmts(
        stmts: &[Stmt],
        cands: &HashMap<String, UnboxedElem>,
        inferred_ints: &HashSet<String>,
        int_locals: &HashSet<String>,
        escaped: &mut HashSet<String>,
        allowed_id_ptr: &mut HashSet<usize>,
    ) {
        for s in stmts {
            match s {
                Stmt::Let { value: Some(v), .. } | Stmt::Const { value: v, .. } => {
                    scan_expr(v, cands, inferred_ints, int_locals, escaped, allowed_id_ptr);
                }
                Stmt::Expr(e)
                | Stmt::Return { value: Some(e), .. }
                | Stmt::Throw { value: e, .. } => {
                    scan_expr(e, cands, inferred_ints, int_locals, escaped, allowed_id_ptr);
                }
                Stmt::If {
                    cond,
                    then,
                    elifs,
                    els,
                    ..
                } => {
                    scan_expr(
                        cond,
                        cands,
                        inferred_ints,
                        int_locals,
                        escaped,
                        allowed_id_ptr,
                    );
                    scan_stmts(
                        &then.stmts,
                        cands,
                        inferred_ints,
                        int_locals,
                        escaped,
                        allowed_id_ptr,
                    );
                    for (c, b) in elifs {
                        scan_expr(c, cands, inferred_ints, int_locals, escaped, allowed_id_ptr);
                        scan_stmts(
                            &b.stmts,
                            cands,
                            inferred_ints,
                            int_locals,
                            escaped,
                            allowed_id_ptr,
                        );
                    }
                    if let Some(b) = els {
                        scan_stmts(
                            &b.stmts,
                            cands,
                            inferred_ints,
                            int_locals,
                            escaped,
                            allowed_id_ptr,
                        );
                    }
                }
                Stmt::While { cond, body, .. } => {
                    scan_expr(
                        cond,
                        cands,
                        inferred_ints,
                        int_locals,
                        escaped,
                        allowed_id_ptr,
                    );
                    scan_stmts(
                        &body.stmts,
                        cands,
                        inferred_ints,
                        int_locals,
                        escaped,
                        allowed_id_ptr,
                    );
                }
                Stmt::ForC {
                    init,
                    cond,
                    post,
                    body,
                    ..
                } => {
                    scan_stmts(
                        std::slice::from_ref(init),
                        cands,
                        inferred_ints,
                        int_locals,
                        escaped,
                        allowed_id_ptr,
                    );
                    scan_expr(
                        cond,
                        cands,
                        inferred_ints,
                        int_locals,
                        escaped,
                        allowed_id_ptr,
                    );
                    scan_expr(
                        post,
                        cands,
                        inferred_ints,
                        int_locals,
                        escaped,
                        allowed_id_ptr,
                    );
                    scan_stmts(
                        &body.stmts,
                        cands,
                        inferred_ints,
                        int_locals,
                        escaped,
                        allowed_id_ptr,
                    );
                }
                Stmt::ForIn {
                    var2: None,
                    iter,
                    body,
                    ..
                } => {
                    if let Expr::Ident(name, _) = iter {
                        if cands.contains_key(name) && !ulist_mutated_in_stmts(&body.stmts, name) {
                            allowed_id_ptr.insert(iter as *const Expr as usize);
                        }
                    }
                    scan_expr(
                        iter,
                        cands,
                        inferred_ints,
                        int_locals,
                        escaped,
                        allowed_id_ptr,
                    );
                    scan_stmts(
                        &body.stmts,
                        cands,
                        inferred_ints,
                        int_locals,
                        escaped,
                        allowed_id_ptr,
                    );
                }
                Stmt::ForIn { iter, body, .. } => {
                    scan_expr(
                        iter,
                        cands,
                        inferred_ints,
                        int_locals,
                        escaped,
                        allowed_id_ptr,
                    );
                    scan_stmts(
                        &body.stmts,
                        cands,
                        inferred_ints,
                        int_locals,
                        escaped,
                        allowed_id_ptr,
                    );
                }
                Stmt::Block(b) => scan_stmts(
                    &b.stmts,
                    cands,
                    inferred_ints,
                    int_locals,
                    escaped,
                    allowed_id_ptr,
                ),
                Stmt::Try {
                    body,
                    catch,
                    finally,
                    ..
                } => {
                    scan_stmts(
                        &body.stmts,
                        cands,
                        inferred_ints,
                        int_locals,
                        escaped,
                        allowed_id_ptr,
                    );
                    if let Some(c) = catch {
                        scan_stmts(
                            &c.body.stmts,
                            cands,
                            inferred_ints,
                            int_locals,
                            escaped,
                            allowed_id_ptr,
                        );
                    }
                    if let Some(f) = finally {
                        scan_stmts(
                            &f.stmts,
                            cands,
                            inferred_ints,
                            int_locals,
                            escaped,
                            allowed_id_ptr,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    record_cands(
        &proto.body.stmts,
        boxed,
        int_locals,
        &mut cands,
        &mut shadowed,
        &mut inferred_ints,
    );
    for s in shadowed {
        cands.remove(&s);
    }
    if cands.is_empty() {
        return cands;
    }
    let mut escaped = HashSet::new();
    scan_stmts(
        &proto.body.stmts,
        &cands,
        &inferred_ints,
        int_locals,
        &mut escaped,
        &mut non_escaping_idents,
    );
    for e in escaped {
        cands.remove(&e);
    }
    cands
}

fn expr_mentions_ident(e: &Expr, target: &str) -> bool {
    match e {
        Expr::Ident(n, _) => n == target,
        Expr::Unary { expr, .. }
        | Expr::Await(expr, _)
        | Expr::Propagate(expr, _)
        | Expr::NamedArg { value: expr, .. }
        | Expr::Spawn { callable: expr, .. } => expr_mentions_ident(expr, target),
        Expr::Binary { l, r, .. } | Expr::Logical { l, r, .. } | Expr::Coalesce { l, r, .. } => {
            expr_mentions_ident(l, target) || expr_mentions_ident(r, target)
        }
        Expr::Assign {
            target: t, value, ..
        } => expr_mentions_ident(t, target) || expr_mentions_ident(value, target),
        Expr::Call { callee, args, .. } => {
            expr_mentions_ident(callee, target)
                || args.iter().any(|a| expr_mentions_ident(a, target))
        }
        Expr::Method { object, args, .. } => {
            expr_mentions_ident(object, target)
                || args.iter().any(|a| expr_mentions_ident(a, target))
        }
        Expr::Index { object, index, .. } => {
            expr_mentions_ident(object, target) || expr_mentions_ident(index, target)
        }
        Expr::Field { object, .. } | Expr::OptField { object, .. } => {
            expr_mentions_ident(object, target)
        }
        Expr::List(items, _) => items.iter().any(|i| expr_mentions_ident(i, target)),
        Expr::New { args, .. } | Expr::SuperCall { args, .. } => {
            args.iter().any(|a| expr_mentions_ident(a, target))
        }
        Expr::Fn(p) => {
            p.params.iter().any(|x| {
                x.name == target
                    || x.default
                        .as_ref()
                        .is_some_and(|d| expr_mentions_ident(d, target))
            }) || p
                .body
                .stmts
                .iter()
                .any(|st| stmt_mentions_ident(st, target))
        }
        Expr::If {
            cond, then, els, ..
        } => {
            expr_mentions_ident(cond, target)
                || then.stmts.iter().any(|st| stmt_mentions_ident(st, target))
                || els
                    .as_ref()
                    .is_some_and(|b| b.stmts.iter().any(|st| stmt_mentions_ident(st, target)))
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            expr_mentions_ident(scrutinee, target)
                || arms.iter().any(|arm| {
                    arm.guard
                        .as_ref()
                        .is_some_and(|g| expr_mentions_ident(g, target))
                        || arm
                            .body
                            .stmts
                            .iter()
                            .any(|st| stmt_mentions_ident(st, target))
                })
        }
        Expr::Int(..)
        | Expr::Float(..)
        | Expr::Str(..)
        | Expr::Bool(..)
        | Expr::Null(..)
        | Expr::Super { .. } => false,
    }
}

fn stmt_mentions_ident(s: &Stmt, target: &str) -> bool {
    match s {
        Stmt::Let { name, value, .. } => {
            name == target
                || value
                    .as_ref()
                    .is_some_and(|v| expr_mentions_ident(v, target))
        }
        Stmt::Const { name, value, .. } => name == target || expr_mentions_ident(value, target),
        Stmt::LetTuple { names, value, .. } => {
            names.iter().any(|n| n == target) || expr_mentions_ident(value, target)
        }
        Stmt::Fn(p) => {
            p.name.as_deref() == Some(target)
                || p.params.iter().any(|x| {
                    x.name == target
                        || x.default
                            .as_ref()
                            .is_some_and(|d| expr_mentions_ident(d, target))
                })
                || p.body
                    .stmts
                    .iter()
                    .any(|st| stmt_mentions_ident(st, target))
        }
        Stmt::Class(c) => {
            c.name == target
                || c.methods.iter().any(|m| {
                    m.name.as_deref() == Some(target)
                        || m.params.iter().any(|x| {
                            x.name == target
                                || x.default
                                    .as_ref()
                                    .is_some_and(|d| expr_mentions_ident(d, target))
                        })
                        || m.body
                            .stmts
                            .iter()
                            .any(|st| stmt_mentions_ident(st, target))
                })
        }
        Stmt::If {
            cond,
            then,
            elifs,
            els,
            ..
        } => {
            expr_mentions_ident(cond, target)
                || then.stmts.iter().any(|st| stmt_mentions_ident(st, target))
                || elifs.iter().any(|(c, b)| {
                    expr_mentions_ident(c, target)
                        || b.stmts.iter().any(|st| stmt_mentions_ident(st, target))
                })
                || els
                    .as_ref()
                    .is_some_and(|b| b.stmts.iter().any(|st| stmt_mentions_ident(st, target)))
        }
        Stmt::While { cond, body, .. } => {
            expr_mentions_ident(cond, target)
                || body.stmts.iter().any(|st| stmt_mentions_ident(st, target))
        }
        Stmt::ForIn {
            var,
            var2,
            iter,
            body,
            ..
        } => {
            var == target
                || var2.as_deref() == Some(target)
                || expr_mentions_ident(iter, target)
                || body.stmts.iter().any(|st| stmt_mentions_ident(st, target))
        }
        Stmt::ForC {
            init,
            cond,
            post,
            body,
            ..
        } => {
            stmt_mentions_ident(init, target)
                || expr_mentions_ident(cond, target)
                || expr_mentions_ident(post, target)
                || body.stmts.iter().any(|st| stmt_mentions_ident(st, target))
        }
        Stmt::Return { value, .. } => value
            .as_ref()
            .is_some_and(|v| expr_mentions_ident(v, target)),
        Stmt::Block(b) => b.stmts.iter().any(|st| stmt_mentions_ident(st, target)),
        Stmt::Expr(e) | Stmt::Throw { value: e, .. } => expr_mentions_ident(e, target),
        Stmt::Spawn { callable, .. } => expr_mentions_ident(callable, target),
        Stmt::Yield { value, .. } => expr_mentions_ident(value, target),
        Stmt::Try {
            body,
            catch,
            finally,
            ..
        } => {
            body.stmts.iter().any(|st| stmt_mentions_ident(st, target))
                || catch.as_ref().is_some_and(|c| {
                    c.name == target
                        || c.body
                            .stmts
                            .iter()
                            .any(|st| stmt_mentions_ident(st, target))
                })
                || finally
                    .as_ref()
                    .is_some_and(|f| f.stmts.iter().any(|st| stmt_mentions_ident(st, target)))
        }
        Stmt::Import { .. }
        | Stmt::Interface(..)
        | Stmt::Enum(..)
        | Stmt::Break { .. }
        | Stmt::Continue { .. } => false,
    }
}

fn stmt_binds_ident(s: &Stmt, target: &str) -> bool {
    match s {
        Stmt::Let { name, .. } | Stmt::Const { name, .. } => name == target,
        Stmt::LetTuple { names, .. } => names.iter().any(|n| n == target),
        Stmt::If {
            then, elifs, els, ..
        } => {
            then.stmts.iter().any(|st| stmt_binds_ident(st, target))
                || elifs
                    .iter()
                    .any(|(_, b)| b.stmts.iter().any(|st| stmt_binds_ident(st, target)))
                || els
                    .as_ref()
                    .is_some_and(|b| b.stmts.iter().any(|st| stmt_binds_ident(st, target)))
        }
        Stmt::While { body, .. } => body.stmts.iter().any(|st| stmt_binds_ident(st, target)),
        Stmt::ForIn {
            var, var2, body, ..
        } => {
            var == target
                || var2.as_deref() == Some(target)
                || body.stmts.iter().any(|st| stmt_binds_ident(st, target))
        }
        Stmt::ForC { init, body, .. } => {
            stmt_binds_ident(init, target)
                || body.stmts.iter().any(|st| stmt_binds_ident(st, target))
        }
        Stmt::Block(b) => b.stmts.iter().any(|st| stmt_binds_ident(st, target)),
        Stmt::Try {
            body,
            catch,
            finally,
            ..
        } => {
            body.stmts.iter().any(|st| stmt_binds_ident(st, target))
                || catch.as_ref().is_some_and(|c| {
                    c.name == target || c.body.stmts.iter().any(|st| stmt_binds_ident(st, target))
                })
                || finally
                    .as_ref()
                    .is_some_and(|f| f.stmts.iter().any(|st| stmt_binds_ident(st, target)))
        }
        _ => false,
    }
}

/// Filter `ulists` to entries declared directly at the top level of `stmts`
/// with no prior statement or initializer mentioning the binding and no later
/// statement re-binding it, so fixed-point scalar analysis can safely assume
/// any later reference in `stmts` sees the unboxed list slot.
fn filter_top_level_ulists(
    stmts: &[Stmt],
    ulists: &HashMap<String, UnboxedElem>,
) -> HashMap<String, UnboxedElem> {
    let mut safe = HashMap::new();
    for (name, &elem) in ulists {
        let Some((pos, value)) = stmts.iter().enumerate().find_map(|(i, s)| match s {
            Stmt::Let { name: n, value, .. } if n == name => Some((i, value)),
            _ => None,
        }) else {
            continue;
        };
        if value.as_ref().is_some_and(|v| expr_mentions_ident(v, name)) {
            continue;
        }
        if stmts[..pos].iter().any(|s| stmt_mentions_ident(s, name)) {
            continue;
        }
        if stmts[pos + 1..].iter().any(|s| stmt_binds_ident(s, name)) {
            continue;
        }
        safe.insert(name.clone(), elem);
    }
    safe
}

fn type_expr_mentions_float(ty: &TypeExpr) -> bool {
    ty.name == "float"
        || ty.args.iter().any(type_expr_mentions_float)
        || ty.union.iter().any(type_expr_mentions_float)
}

fn expr_has_float_literal(e: &Expr) -> bool {
    match e {
        Expr::Float(..) => true,
        Expr::Int(..)
        | Expr::Bool(..)
        | Expr::Str(..)
        | Expr::Null(..)
        | Expr::Ident(..)
        | Expr::Super { .. } => false,
        Expr::Fn(_) => false,
        Expr::Unary { expr, .. } | Expr::Await(expr, _) | Expr::Propagate(expr, _) => {
            expr_has_float_literal(expr)
        }
        Expr::Binary { l, r, .. } | Expr::Logical { l, r, .. } | Expr::Coalesce { l, r, .. } => {
            expr_has_float_literal(l) || expr_has_float_literal(r)
        }
        Expr::Assign { target, value, .. } => {
            expr_has_float_literal(target) || expr_has_float_literal(value)
        }
        Expr::Index { object, index, .. } => {
            expr_has_float_literal(object) || expr_has_float_literal(index)
        }
        Expr::Field { object, .. } | Expr::OptField { object, .. } => {
            expr_has_float_literal(object)
        }
        Expr::NamedArg { value, .. }
        | Expr::Spawn {
            callable: value, ..
        } => expr_has_float_literal(value),
        Expr::List(items, _) => items.iter().any(expr_has_float_literal),
        Expr::Call { callee, args, .. } => {
            expr_has_float_literal(callee) || args.iter().any(expr_has_float_literal)
        }
        Expr::Method { object, args, .. } => {
            expr_has_float_literal(object) || args.iter().any(expr_has_float_literal)
        }
        Expr::New { args, .. } | Expr::SuperCall { args, .. } => {
            args.iter().any(expr_has_float_literal)
        }
        Expr::If {
            cond, then, els, ..
        } => {
            expr_has_float_literal(cond)
                || stmts_have_float_literal(&then.stmts)
                || els
                    .as_ref()
                    .is_some_and(|b| stmts_have_float_literal(&b.stmts))
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            expr_has_float_literal(scrutinee)
                || arms.iter().any(|arm| {
                    arm.guard.as_ref().is_some_and(expr_has_float_literal)
                        || stmts_have_float_literal(&arm.body.stmts)
                })
        }
    }
}

fn stmts_have_float_literal(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| match s {
        Stmt::Let { value: Some(e), .. }
        | Stmt::Const { value: e, .. }
        | Stmt::LetTuple { value: e, .. }
        | Stmt::Expr(e)
        | Stmt::Return { value: Some(e), .. }
        | Stmt::Throw { value: e, .. }
        | Stmt::Spawn { callable: e, .. } => expr_has_float_literal(e),
        Stmt::Yield { value, .. } => expr_has_float_literal(value),
        Stmt::If {
            cond,
            then,
            elifs,
            els,
            ..
        } => {
            expr_has_float_literal(cond)
                || stmts_have_float_literal(&then.stmts)
                || elifs
                    .iter()
                    .any(|(c, b)| expr_has_float_literal(c) || stmts_have_float_literal(&b.stmts))
                || els
                    .as_ref()
                    .is_some_and(|b| stmts_have_float_literal(&b.stmts))
        }
        Stmt::While { cond, body, .. }
        | Stmt::ForIn {
            iter: cond, body, ..
        } => expr_has_float_literal(cond) || stmts_have_float_literal(&body.stmts),
        Stmt::ForC {
            init,
            cond,
            post,
            body,
            ..
        } => {
            stmts_have_float_literal(std::slice::from_ref(init.as_ref()))
                || expr_has_float_literal(cond)
                || expr_has_float_literal(post)
                || stmts_have_float_literal(&body.stmts)
        }
        Stmt::Block(b) => stmts_have_float_literal(&b.stmts),
        Stmt::Try {
            body,
            catch,
            finally,
            ..
        } => {
            stmts_have_float_literal(&body.stmts)
                || catch
                    .as_ref()
                    .is_some_and(|c| stmts_have_float_literal(&c.body.stmts))
                || finally
                    .as_ref()
                    .is_some_and(|f| stmts_have_float_literal(&f.stmts))
        }
        Stmt::Fn(_)
        | Stmt::Class(_)
        | Stmt::Interface(_)
        | Stmt::Enum(_)
        | Stmt::Import { .. }
        | Stmt::Let { value: None, .. }
        | Stmt::Return { value: None, .. }
        | Stmt::Break { .. }
        | Stmt::Continue { .. } => false,
    })
}

fn collect_float_hints_expr(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Fn(_) => {}
        Expr::Assign { target, value, .. } => {
            if let Expr::Ident(name, _) = target.as_ref() {
                if expr_has_float_literal(value) {
                    out.insert(name.clone());
                }
            }
            collect_float_hints_expr(target, out);
            collect_float_hints_expr(value, out);
        }
        Expr::Unary { expr, .. } | Expr::Await(expr, _) | Expr::Propagate(expr, _) => {
            collect_float_hints_expr(expr, out);
        }
        Expr::Binary { l, r, .. }
        | Expr::Logical { l, r, .. }
        | Expr::Coalesce { l, r, .. }
        | Expr::Index {
            object: l,
            index: r,
            ..
        } => {
            collect_float_hints_expr(l, out);
            collect_float_hints_expr(r, out);
        }
        Expr::Field { object, .. } | Expr::OptField { object, .. } => {
            collect_float_hints_expr(object, out);
        }
        Expr::NamedArg { value, .. }
        | Expr::Spawn {
            callable: value, ..
        } => {
            collect_float_hints_expr(value, out);
        }
        Expr::List(items, _) => {
            for it in items {
                collect_float_hints_expr(it, out);
            }
        }
        Expr::Call { callee, args, .. } => {
            collect_float_hints_expr(callee, out);
            for a in args {
                collect_float_hints_expr(a, out);
            }
        }
        Expr::Method { object, args, .. } => {
            collect_float_hints_expr(object, out);
            for a in args {
                collect_float_hints_expr(a, out);
            }
        }
        Expr::New { args, .. } | Expr::SuperCall { args, .. } => {
            for a in args {
                collect_float_hints_expr(a, out);
            }
        }
        Expr::If {
            cond, then, els, ..
        } => {
            collect_float_hints_expr(cond, out);
            collect_float_hints_stmts(&then.stmts, out);
            if let Some(b) = els {
                collect_float_hints_stmts(&b.stmts, out);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_float_hints_expr(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_float_hints_expr(g, out);
                }
                collect_float_hints_stmts(&arm.body.stmts, out);
            }
        }
        Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Str(..)
        | Expr::Null(..)
        | Expr::Ident(..)
        | Expr::Super { .. } => {}
    }
}

fn collect_float_hints_stmts(stmts: &[Stmt], out: &mut HashSet<String>) {
    for s in stmts {
        match s {
            Stmt::Fn(_)
            | Stmt::Class(_)
            | Stmt::Interface(_)
            | Stmt::Enum(_)
            | Stmt::Import { .. } => {}
            Stmt::Let {
                name, ty, value, ..
            } => {
                if ty.as_ref().is_some_and(type_expr_mentions_float)
                    || value.as_ref().is_some_and(expr_has_float_literal)
                {
                    out.insert(name.clone());
                }
                if let Some(v) = value {
                    collect_float_hints_expr(v, out);
                }
            }
            Stmt::Const {
                name, ty, value, ..
            } => {
                if ty.as_ref().is_some_and(type_expr_mentions_float)
                    || expr_has_float_literal(value)
                {
                    out.insert(name.clone());
                }
                collect_float_hints_expr(value, out);
            }
            Stmt::LetTuple { value, .. }
            | Stmt::Expr(value)
            | Stmt::Throw { value, .. }
            | Stmt::Spawn {
                callable: value, ..
            } => {
                collect_float_hints_expr(value, out);
            }
            Stmt::Yield { value, .. } => collect_float_hints_expr(value, out),
            Stmt::Return { value: Some(v), .. } => collect_float_hints_expr(v, out),
            Stmt::If {
                cond,
                then,
                elifs,
                els,
                ..
            } => {
                collect_float_hints_expr(cond, out);
                collect_float_hints_stmts(&then.stmts, out);
                for (c, b) in elifs {
                    collect_float_hints_expr(c, out);
                    collect_float_hints_stmts(&b.stmts, out);
                }
                if let Some(b) = els {
                    collect_float_hints_stmts(&b.stmts, out);
                }
            }
            Stmt::While { cond, body, .. } => {
                collect_float_hints_expr(cond, out);
                collect_float_hints_stmts(&body.stmts, out);
            }
            Stmt::ForIn { iter, body, .. } => {
                collect_float_hints_expr(iter, out);
                collect_float_hints_stmts(&body.stmts, out);
            }
            Stmt::ForC {
                init,
                cond,
                post,
                body,
                ..
            } => {
                collect_float_hints_stmts(std::slice::from_ref(init.as_ref()), out);
                collect_float_hints_expr(cond, out);
                collect_float_hints_expr(post, out);
                collect_float_hints_stmts(&body.stmts, out);
            }
            Stmt::Block(b) => collect_float_hints_stmts(&b.stmts, out),
            Stmt::Try {
                body,
                catch,
                finally,
                ..
            } => {
                collect_float_hints_stmts(&body.stmts, out);
                if let Some(c) = catch {
                    collect_float_hints_stmts(&c.body.stmts, out);
                }
                if let Some(f) = finally {
                    collect_float_hints_stmts(&f.stmts, out);
                }
            }
            Stmt::Return { value: None, .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
        }
    }
}

fn collect_float_hint_vars(params: &[Param], stmts: &[Stmt]) -> HashSet<String> {
    let mut out = HashSet::new();
    for p in params {
        if p.ty.as_ref().is_some_and(type_expr_mentions_float) {
            out.insert(p.name.clone());
        }
    }
    collect_float_hints_stmts(stmts, &mut out);
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PromotedKind {
    Int,
    Float,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SimKind {
    Int,
    Float,
    Bool,
}

struct BcePlan {
    idx_var: String,
    bound_expr: Expr,
    inclusive: bool,
    bce_rows: BTreeSet<String>,
}

struct HoistedLoopPlan {
    rows: BTreeMap<String, UnboxedElem>,
    promoted: BTreeMap<String, (PromotedKind, bool)>,
    bce: Option<BcePlan>,
}

fn collect_shadowed_builtins_expr(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Assign { target, value, .. } => {
            if let Expr::Ident(n, _) = target.as_ref() {
                out.insert(n.clone());
            } else {
                collect_shadowed_builtins_expr(target, out);
            }
            collect_shadowed_builtins_expr(value, out);
        }
        Expr::Unary { expr, .. } | Expr::Await(expr, _) | Expr::Propagate(expr, _) => {
            collect_shadowed_builtins_expr(expr, out);
        }
        Expr::Binary { l, r, .. }
        | Expr::Logical { l, r, .. }
        | Expr::Coalesce { l, r, .. }
        | Expr::Index {
            object: l,
            index: r,
            ..
        } => {
            collect_shadowed_builtins_expr(l, out);
            collect_shadowed_builtins_expr(r, out);
        }
        Expr::Field { object, .. } | Expr::OptField { object, .. } => {
            collect_shadowed_builtins_expr(object, out);
        }
        Expr::NamedArg { value, .. }
        | Expr::Spawn {
            callable: value, ..
        } => {
            collect_shadowed_builtins_expr(value, out);
        }
        Expr::List(items, _) => {
            for it in items {
                collect_shadowed_builtins_expr(it, out);
            }
        }
        Expr::Call { callee, args, .. } => {
            collect_shadowed_builtins_expr(callee, out);
            for a in args {
                collect_shadowed_builtins_expr(a, out);
            }
        }
        Expr::Method { object, args, .. } => {
            collect_shadowed_builtins_expr(object, out);
            for a in args {
                collect_shadowed_builtins_expr(a, out);
            }
        }
        Expr::New { args, .. } | Expr::SuperCall { args, .. } => {
            for a in args {
                collect_shadowed_builtins_expr(a, out);
            }
        }
        Expr::If {
            cond, then, els, ..
        } => {
            collect_shadowed_builtins_expr(cond, out);
            collect_shadowed_builtins_stmts(&then.stmts, out);
            if let Some(b) = els {
                collect_shadowed_builtins_stmts(&b.stmts, out);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_shadowed_builtins_expr(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_shadowed_builtins_expr(g, out);
                }
                collect_shadowed_builtins_stmts(&arm.body.stmts, out);
            }
        }
        Expr::Fn(p) => {
            if let Some(n) = &p.name {
                out.insert(n.clone());
            }
            collect_shadowed_builtins_stmts(&p.body.stmts, out);
        }
        Expr::Int(..)
        | Expr::Float(..)
        | Expr::Bool(..)
        | Expr::Str(..)
        | Expr::Null(..)
        | Expr::Ident(..)
        | Expr::Super { .. } => {}
    }
}

fn collect_shadowed_builtins_stmts(stmts: &[Stmt], out: &mut HashSet<String>) {
    for s in stmts {
        match s {
            Stmt::Fn(p) => {
                if let Some(n) = &p.name {
                    out.insert(n.clone());
                }
                collect_shadowed_builtins_stmts(&p.body.stmts, out);
            }
            Stmt::Class(c) => {
                out.insert(c.name.clone());
                for m in &c.methods {
                    collect_shadowed_builtins_stmts(&m.body.stmts, out);
                }
            }
            Stmt::Interface(i) => {
                out.insert(i.name.clone());
                for m in &i.default_methods {
                    collect_shadowed_builtins_stmts(&m.body.stmts, out);
                }
            }
            Stmt::Enum(e) => {
                out.insert(e.name.clone());
            }
            Stmt::Import { alias: Some(a), .. } => {
                out.insert(a.clone());
            }
            Stmt::Let { name, value, .. } => {
                out.insert(name.clone());
                if let Some(v) = value {
                    collect_shadowed_builtins_expr(v, out);
                }
            }
            Stmt::Const { name, value, .. } => {
                out.insert(name.clone());
                collect_shadowed_builtins_expr(value, out);
            }
            Stmt::LetTuple { names, value, .. } => {
                for n in names {
                    out.insert(n.clone());
                }
                collect_shadowed_builtins_expr(value, out);
            }
            Stmt::Expr(e)
            | Stmt::Throw { value: e, .. }
            | Stmt::Spawn { callable: e, .. }
            | Stmt::Return { value: Some(e), .. } => collect_shadowed_builtins_expr(e, out),
            Stmt::Yield { value, .. } => collect_shadowed_builtins_expr(value, out),
            Stmt::If {
                cond,
                then,
                elifs,
                els,
                ..
            } => {
                collect_shadowed_builtins_expr(cond, out);
                collect_shadowed_builtins_stmts(&then.stmts, out);
                for (c, b) in elifs {
                    collect_shadowed_builtins_expr(c, out);
                    collect_shadowed_builtins_stmts(&b.stmts, out);
                }
                if let Some(b) = els {
                    collect_shadowed_builtins_stmts(&b.stmts, out);
                }
            }
            Stmt::While { cond, body, .. } => {
                collect_shadowed_builtins_expr(cond, out);
                collect_shadowed_builtins_stmts(&body.stmts, out);
            }
            Stmt::ForIn { iter, body, .. } => {
                collect_shadowed_builtins_expr(iter, out);
                collect_shadowed_builtins_stmts(&body.stmts, out);
            }
            Stmt::ForC {
                init,
                cond,
                post,
                body,
                ..
            } => {
                collect_shadowed_builtins_stmts(std::slice::from_ref(init.as_ref()), out);
                collect_shadowed_builtins_expr(cond, out);
                collect_shadowed_builtins_expr(post, out);
                collect_shadowed_builtins_stmts(&body.stmts, out);
            }
            Stmt::Block(b) => collect_shadowed_builtins_stmts(&b.stmts, out),
            Stmt::Try {
                body,
                catch,
                finally,
                ..
            } => {
                collect_shadowed_builtins_stmts(&body.stmts, out);
                if let Some(c) = catch {
                    collect_shadowed_builtins_stmts(&c.body.stmts, out);
                }
                if let Some(f) = finally {
                    collect_shadowed_builtins_stmts(&f.stmts, out);
                }
            }
            Stmt::Import { alias: None, .. }
            | Stmt::Return { value: None, .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. } => {}
        }
    }
}

fn collect_shadowed_builtins(program: &Program) -> HashSet<String> {
    let mut out = HashSet::new();
    collect_shadowed_builtins_stmts(&program.stmts, &mut out);
    out
}

fn analyze_fn_locals(
    proto: &FnProto,
    boxed: &HashSet<String>,
    int_params: &HashSet<String>,
    float_params: &HashSet<String>,
    shadowed_globals: &HashSet<String>,
) -> (
    HashSet<String>,
    HashSet<String>,
    HashMap<String, UnboxedElem>,
    HashSet<String>,
) {
    let mut shadowed_here = shadowed_globals.clone();
    for p in &proto.params {
        shadowed_here.insert(p.name.clone());
    }
    collect_declared(&proto.body.stmts, &mut shadowed_here);
    let str_locals = analyze_str_locals(proto, boxed, &shadowed_here);
    let empty_ulists = HashMap::new();
    let int_0 =
        analyze_int_locals_with_params(proto, boxed, int_params, &empty_ulists, &str_locals);
    let ulists = analyze_unboxed_lists(proto, boxed, &int_0);
    let safe_ulists = filter_top_level_ulists(&proto.body.stmts, &ulists);
    let int_locals =
        analyze_int_locals_with_params(proto, boxed, int_params, &safe_ulists, &str_locals);
    let float_locals = analyze_float_locals(proto, boxed, &int_locals, float_params, &safe_ulists);
    (int_locals, float_locals, ulists, str_locals)
}

// ---------------------------------------------------------------------------
// statement + expression compilation
// ---------------------------------------------------------------------------

impl<'ctx> Ir<'ctx> {
    fn is_int_expr(&self, e: &Expr) -> bool {
        match e {
            Expr::Int(..) => true,
            Expr::Ident(name, _) => match self.lookup(name) {
                Some(VarLoc::LocalInt(_)) => true,
                Some(_) => false,
                None => self.top_ints.contains_key(name),
            },
            Expr::Unary {
                op: UnOp::Neg,
                expr,
                ..
            } => self.is_int_expr(expr),
            Expr::Binary { op, l, r, .. } => {
                matches!(
                    op,
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
                ) && self.is_int_expr(l)
                    && self.is_int_expr(r)
            }
            Expr::Index { object, index, .. } => {
                if let Expr::Ident(name, _) = &**object {
                    if matches!(
                        self.lookup(name),
                        Some(VarLoc::UListI64(_)) | Some(VarLoc::RowViewI64 { .. })
                    ) && self.is_int_expr(index)
                    {
                        return true;
                    }
                }
                false
            }
            Expr::Method {
                object, name, args, ..
            } => {
                if let Expr::Ident(vname, _) = &**object {
                    if args.is_empty() {
                        match self.lookup(vname) {
                            Some(VarLoc::UListI64(_)) if name == "len" || name == "pop" => {
                                return true;
                            }
                            Some(VarLoc::UListF64(_))
                            | Some(VarLoc::RowViewI64 { .. })
                            | Some(VarLoc::RowViewF64 { .. })
                                if name == "len" =>
                            {
                                return true;
                            }
                            _ if name == "len"
                                && self.str_locals.as_ref().is_some_and(|s| s.contains(vname)) =>
                            {
                                return true;
                            }
                            _ => {}
                        }
                    }
                }
                false
            }
            Expr::Call {
                callee,
                type_args,
                args,
                ..
            } if !type_args.is_empty() => {
                if let Expr::Ident(name, _) = &**callee {
                    if let Some(proto) = self.generic_fns.get(name) {
                        if proto.type_params.len() == type_args.len()
                            && proto.params.len() == args.len()
                            && !proto.is_async
                        {
                            return Self::subst_scalar_type(
                                proto.ret.as_ref(),
                                &proto.type_params,
                                type_args,
                            ) == Some("int");
                        }
                    }
                }
                false
            }
            Expr::Call {
                callee,
                type_args,
                args,
                ..
            } if type_args.is_empty() => {
                if let Expr::Ident(name, _) = &**callee {
                    return self.can_direct_raw_call(name, args) == Some("int");
                }
                false
            }
            _ => false,
        }
    }

    fn is_int_compare<'a>(&self, e: &'a Expr) -> Option<(BinOp, &'a Expr, &'a Expr)> {
        if let Expr::Binary {
            op: op @ (BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne),
            l,
            r,
            ..
        } = e
        {
            if self.is_int_expr(l) && self.is_int_expr(r) {
                return Some((*op, l.as_ref(), r.as_ref()));
            }
        }
        None
    }

    fn is_bool_expr(&self, e: &Expr) -> bool {
        match e {
            Expr::Bool(..) => true,
            _ if self.is_int_compare(e).is_some() => true,
            _ if self.is_float_compare(e).is_some() => true,
            Expr::Unary {
                op: UnOp::Not,
                expr,
                ..
            } => self.is_bool_expr(expr),
            Expr::Call {
                callee: box_callee,
                type_args,
                args,
                ..
            } => {
                if let Expr::Ident(name, _) = &**box_callee {
                    if type_args.is_empty() {
                        return self.can_direct_raw_call(name, args) == Some("bool");
                    }
                }
                false
            }
            _ => false,
        }
    }

    fn is_float_expr(&self, e: &Expr) -> bool {
        match e {
            Expr::Float(..) => true,
            Expr::Ident(name, _) => matches!(self.lookup(name), Some(VarLoc::LocalFloat(_))),
            Expr::Unary {
                op: UnOp::Neg,
                expr,
                ..
            } => self.is_float_expr(expr),
            Expr::Binary {
                op: BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div,
                l,
                r,
                ..
            } => {
                let lf = self.is_float_expr(l);
                let rf = self.is_float_expr(r);
                let li = self.is_int_expr(l);
                let ri = self.is_int_expr(r);
                (lf && (rf || ri)) || (li && rf)
            }
            Expr::Index { object, index, .. } => {
                if let Expr::Ident(name, _) = &**object {
                    if matches!(
                        self.lookup(name),
                        Some(VarLoc::UListF64(_)) | Some(VarLoc::RowViewF64 { .. })
                    ) && self.is_int_expr(index)
                    {
                        return true;
                    }
                }
                false
            }
            Expr::Method {
                object, name, args, ..
            } if args.is_empty() && name == "pop" => {
                if let Expr::Ident(vname, _) = &**object {
                    return matches!(self.lookup(vname), Some(VarLoc::UListF64(_)));
                }
                false
            }
            Expr::Call {
                callee,
                type_args,
                args,
                ..
            } if type_args.is_empty() => {
                if let Expr::Ident(name, _) = &**callee {
                    return self.can_direct_raw_call(name, args) == Some("float");
                }
                false
            }
            _ => false,
        }
    }

    fn is_float_compare<'a>(&self, e: &'a Expr) -> Option<(BinOp, &'a Expr, &'a Expr)> {
        if let Expr::Binary {
            op: op @ (BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne),
            l,
            r,
            ..
        } = e
        {
            let lf = self.is_float_expr(l);
            let rf = self.is_float_expr(r);
            if (lf && (rf || self.is_int_expr(r))) || (self.is_int_expr(l) && rf) {
                return Some((*op, l.as_ref(), r.as_ref()));
            }
        }
        None
    }

    fn can_direct_raw_call(&self, name: &str, args: &[Expr]) -> Option<&'static str> {
        if self.lookup(name).is_some() {
            return None;
        }
        let proto = self.raw_fns.get(name)?;
        if proto.params.len() != args.len() {
            return None;
        }
        for (p, a) in proto.params.iter().zip(args.iter()) {
            match p.ty.as_ref().and_then(is_scalar_type_expr) {
                Some("int") if !self.is_int_expr(a) => return None,
                Some("float") if !self.is_float_expr(a) && !self.is_int_expr(a) => return None,
                Some("bool") if !self.is_bool_expr(a) => return None,
                _ => {}
            }
        }
        proto
            .ret
            .as_ref()
            .and_then(is_scalar_type_expr)
            .or(Some("any"))
    }

    fn ulist_hdr_ty(&self) -> inkwell::types::StructType<'ctx> {
        self.ctx
            .struct_type(&[self.ptr_ty().into(), self.i64_ty().into()], false)
    }

    fn emit_ulist_len(&mut self, ul_ptr: PointerValue<'ctx>) -> inkwell::values::IntValue<'ctx> {
        let hdr_ty = self.ulist_hdr_ty();
        let len_ptr = self
            .builder
            .build_struct_gep(hdr_ty, ul_ptr, 1, "ul.lenp")
            .unwrap();
        self.builder
            .build_load(self.i64_ty(), len_ptr, "ul.len")
            .unwrap()
            .into_int_value()
    }

    fn emit_ulist_i64_get_inline(
        &mut self,
        ul_ptr: PointerValue<'ctx>,
        idx: inkwell::values::IntValue<'ctx>,
    ) -> IrResult<inkwell::values::IntValue<'ctx>> {
        let len = self.emit_ulist_len(ul_ptr);
        let inb_raw = self
            .builder
            .build_int_compare(IntPredicate::ULT, idx, len, "ul.inb")
            .unwrap();
        let inb = self.expect_i1(inb_raw, true);
        let fast_bb = self.new_block("ul.fast");
        let slow_bb = self.new_block("ul.slow");
        let cont_bb = self.new_block("ul.cont");
        self.builder
            .build_conditional_branch(inb, fast_bb, slow_bb)
            .unwrap();

        self.builder.position_at_end(fast_bb);
        let hdr_ty = self.ulist_hdr_ty();
        let data_slot = self
            .builder
            .build_struct_gep(hdr_ty, ul_ptr, 0, "ul.datap")
            .unwrap();
        let data = self
            .builder
            .build_load(self.ptr_ty(), data_slot, "ul.data")
            .unwrap()
            .into_pointer_value();
        let elem_ptr = unsafe {
            self.builder
                .build_in_bounds_gep(self.i64_ty(), data, &[idx], "ul.ep")
                .unwrap()
        };
        let fast_v = self
            .builder
            .build_load(self.i64_ty(), elem_ptr, "ul.ev")
            .unwrap()
            .into_int_value();
        let fast_from = self.cur_block().unwrap();
        self.br_to(cont_bb);

        self.builder.position_at_end(slow_bb);
        let f = self.tramp(
            "spid3r_ulist_i64_get",
            &[self.ptr_ty().into(), self.i64_ty().into()],
            Some(self.i64_ty().into()),
        );
        let slow_v = call_int(self.emit_call(f, &[ul_ptr.into(), idx.into()], "ul.sget"));
        self.throw_guard();
        let slow_from = self.cur_block().unwrap();
        self.br_to(cont_bb);

        self.builder.position_at_end(cont_bb);
        let phi = self.builder.build_phi(self.i64_ty(), "ul.phi").unwrap();
        phi.add_incoming(&[(&fast_v, fast_from), (&slow_v, slow_from)]);
        Ok(phi.as_basic_value().into_int_value())
    }

    fn emit_ulist_i64_set_inline(
        &mut self,
        ul_ptr: PointerValue<'ctx>,
        idx: inkwell::values::IntValue<'ctx>,
        val: inkwell::values::IntValue<'ctx>,
    ) {
        let len = self.emit_ulist_len(ul_ptr);
        let inb_raw = self
            .builder
            .build_int_compare(IntPredicate::ULT, idx, len, "ul.inb")
            .unwrap();
        let inb = self.expect_i1(inb_raw, true);
        let fast_bb = self.new_block("ul.fast");
        let slow_bb = self.new_block("ul.slow");
        let cont_bb = self.new_block("ul.cont");
        self.builder
            .build_conditional_branch(inb, fast_bb, slow_bb)
            .unwrap();

        self.builder.position_at_end(fast_bb);
        let hdr_ty = self.ulist_hdr_ty();
        let data_slot = self
            .builder
            .build_struct_gep(hdr_ty, ul_ptr, 0, "ul.datap")
            .unwrap();
        let data = self
            .builder
            .build_load(self.ptr_ty(), data_slot, "ul.data")
            .unwrap()
            .into_pointer_value();
        let elem_ptr = unsafe {
            self.builder
                .build_in_bounds_gep(self.i64_ty(), data, &[idx], "ul.ep")
                .unwrap()
        };
        self.builder.build_store(elem_ptr, val).unwrap();
        self.br_to(cont_bb);

        self.builder.position_at_end(slow_bb);
        let f = self.tramp(
            "spid3r_ulist_i64_set",
            &[
                self.ptr_ty().into(),
                self.i64_ty().into(),
                self.i64_ty().into(),
            ],
            None,
        );
        self.emit_call(f, &[ul_ptr.into(), idx.into(), val.into()], "");
        self.throw_guard();
        self.br_to(cont_bb);

        self.builder.position_at_end(cont_bb);
    }

    fn emit_ulist_f64_get_inline(
        &mut self,
        ul_ptr: PointerValue<'ctx>,
        idx: inkwell::values::IntValue<'ctx>,
    ) -> IrResult<inkwell::values::FloatValue<'ctx>> {
        let f64_t = self.ctx.f64_type();
        let len = self.emit_ulist_len(ul_ptr);
        let inb_raw = self
            .builder
            .build_int_compare(IntPredicate::ULT, idx, len, "ul.inb")
            .unwrap();
        let inb = self.expect_i1(inb_raw, true);
        let fast_bb = self.new_block("ul.fast");
        let slow_bb = self.new_block("ul.slow");
        let cont_bb = self.new_block("ul.cont");
        self.builder
            .build_conditional_branch(inb, fast_bb, slow_bb)
            .unwrap();

        self.builder.position_at_end(fast_bb);
        let hdr_ty = self.ulist_hdr_ty();
        let data_slot = self
            .builder
            .build_struct_gep(hdr_ty, ul_ptr, 0, "ul.datap")
            .unwrap();
        let data = self
            .builder
            .build_load(self.ptr_ty(), data_slot, "ul.data")
            .unwrap()
            .into_pointer_value();
        let elem_ptr = unsafe {
            self.builder
                .build_in_bounds_gep(f64_t, data, &[idx], "ul.ep")
                .unwrap()
        };
        let fast_v = self
            .builder
            .build_load(f64_t, elem_ptr, "ul.ev")
            .unwrap()
            .into_float_value();
        let fast_from = self.cur_block().unwrap();
        self.br_to(cont_bb);

        self.builder.position_at_end(slow_bb);
        let f = self.tramp(
            "spid3r_ulist_f64_get",
            &[self.ptr_ty().into(), self.i64_ty().into()],
            Some(f64_t.into()),
        );
        let call_r = self.emit_call(f, &[ul_ptr.into(), idx.into()], "ul.sget");
        let slow_v = match call_r.try_as_basic_value() {
            inkwell::values::ValueKind::Basic(v) => v.into_float_value(),
            _ => unreachable!(),
        };
        self.throw_guard();
        let slow_from = self.cur_block().unwrap();
        self.br_to(cont_bb);

        self.builder.position_at_end(cont_bb);
        let phi = self.builder.build_phi(f64_t, "ul.phi").unwrap();
        phi.add_incoming(&[(&fast_v, fast_from), (&slow_v, slow_from)]);
        Ok(phi.as_basic_value().into_float_value())
    }

    fn emit_ulist_f64_set_inline(
        &mut self,
        ul_ptr: PointerValue<'ctx>,
        idx: inkwell::values::IntValue<'ctx>,
        val: inkwell::values::FloatValue<'ctx>,
    ) {
        let f64_t = self.ctx.f64_type();
        let len = self.emit_ulist_len(ul_ptr);
        let inb_raw = self
            .builder
            .build_int_compare(IntPredicate::ULT, idx, len, "ul.inb")
            .unwrap();
        let inb = self.expect_i1(inb_raw, true);
        let fast_bb = self.new_block("ul.fast");
        let slow_bb = self.new_block("ul.slow");
        let cont_bb = self.new_block("ul.cont");
        self.builder
            .build_conditional_branch(inb, fast_bb, slow_bb)
            .unwrap();

        self.builder.position_at_end(fast_bb);
        let hdr_ty = self.ulist_hdr_ty();
        let data_slot = self
            .builder
            .build_struct_gep(hdr_ty, ul_ptr, 0, "ul.datap")
            .unwrap();
        let data = self
            .builder
            .build_load(self.ptr_ty(), data_slot, "ul.data")
            .unwrap()
            .into_pointer_value();
        let elem_ptr = unsafe {
            self.builder
                .build_in_bounds_gep(f64_t, data, &[idx], "ul.ep")
                .unwrap()
        };
        self.builder.build_store(elem_ptr, val).unwrap();
        self.br_to(cont_bb);

        self.builder.position_at_end(slow_bb);
        let f = self.tramp(
            "spid3r_ulist_f64_set",
            &[self.ptr_ty().into(), self.i64_ty().into(), f64_t.into()],
            None,
        );
        self.emit_call(f, &[ul_ptr.into(), idx.into(), val.into()], "");
        self.throw_guard();
        self.br_to(cont_bb);

        self.builder.position_at_end(cont_bb);
    }

    fn is_row_bce_hit(&self, list_name: &str, index_expr: &Expr) -> bool {
        matches!(
            index_expr,
            Expr::Ident(id, _) if self.row_bce_vars.get(list_name) == Some(id)
        )
    }

    /// Phase 1: emit inline slice read for a hoisted `RowViewI64`, eliding the
    /// per-iteration bounds check when pre-loop BCE proved `0 <= idx < len`.
    fn emit_row_view_i64_get(
        &mut self,
        bce_hit: bool,
        data: PointerValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
        orig_slot: PointerValue<'ctx>,
        is_ulist: bool,
        idx: inkwell::values::IntValue<'ctx>,
    ) -> IrResult<inkwell::values::IntValue<'ctx>> {
        if bce_hit {
            // SAFETY: pre-loop BCE check proved `0 <= idx < bound <= len`.
            let elem_ptr = unsafe {
                self.builder
                    .build_in_bounds_gep(self.i64_ty(), data, &[idx], "rv.ep")
                    .unwrap()
            };
            return Ok(self
                .builder
                .build_load(self.i64_ty(), elem_ptr, "rv.ev")
                .unwrap()
                .into_int_value());
        }

        let inb_raw = self
            .builder
            .build_int_compare(IntPredicate::ULT, idx, len, "rv.inb")
            .unwrap();
        let inb = self.expect_i1(inb_raw, true);
        let fast_bb = self.new_block("rv.fast");
        let slow_bb = self.new_block("rv.slow");
        let cont_bb = self.new_block("rv.cont");
        self.builder
            .build_conditional_branch(inb, fast_bb, slow_bb)
            .unwrap();

        self.builder.position_at_end(fast_bb);
        // SAFETY: `ULT(idx, len)` above guarantees `0 <= idx < len`.
        let elem_ptr = unsafe {
            self.builder
                .build_in_bounds_gep(self.i64_ty(), data, &[idx], "rv.ep")
                .unwrap()
        };
        let fast_v = self
            .builder
            .build_load(self.i64_ty(), elem_ptr, "rv.ev")
            .unwrap()
            .into_int_value();
        let fast_from = self.cur_block().unwrap();
        self.br_to(cont_bb);

        self.builder.position_at_end(slow_bb);
        let slow_v = if is_ulist {
            let ul_ptr = self
                .builder
                .build_load(self.ptr_ty(), orig_slot, "rv.ul")
                .unwrap()
                .into_pointer_value();
            let f = self.tramp(
                "spid3r_ulist_i64_get",
                &[self.ptr_ty().into(), self.i64_ty().into()],
                Some(self.i64_ty().into()),
            );
            let sv = call_int(self.emit_call(f, &[ul_ptr.into(), idx.into()], "rv.sget"));
            self.throw_guard();
            sv
        } else {
            let idx_v2 = self.v_int_from_i64(idx);
            let bv = self.call2p("spid3r_index_get", orig_slot, idx_v2);
            self.throw_guard();
            self.call1p_to_i64("spid3r_to_i64", bv)
        };
        let slow_from = self.cur_block().unwrap();
        self.br_to(cont_bb);

        self.builder.position_at_end(cont_bb);
        let phi = self.builder.build_phi(self.i64_ty(), "rv.phi").unwrap();
        phi.add_incoming(&[(&fast_v, fast_from), (&slow_v, slow_from)]);
        Ok(phi.as_basic_value().into_int_value())
    }

    /// Phase 1: emit inline slice read for a hoisted `RowViewF64`, eliding the
    /// per-iteration bounds check when pre-loop BCE proved `0 <= idx < len`.
    fn emit_row_view_f64_get(
        &mut self,
        bce_hit: bool,
        data: PointerValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
        orig_slot: PointerValue<'ctx>,
        is_ulist: bool,
        idx: inkwell::values::IntValue<'ctx>,
    ) -> IrResult<inkwell::values::FloatValue<'ctx>> {
        let f64_t = self.ctx.f64_type();
        if bce_hit {
            // SAFETY: pre-loop BCE check proved `0 <= idx < bound <= len`.
            let elem_ptr = unsafe {
                self.builder
                    .build_in_bounds_gep(f64_t, data, &[idx], "rv.ep")
                    .unwrap()
            };
            return Ok(self
                .builder
                .build_load(f64_t, elem_ptr, "rv.ev")
                .unwrap()
                .into_float_value());
        }

        let inb_raw = self
            .builder
            .build_int_compare(IntPredicate::ULT, idx, len, "rv.inb")
            .unwrap();
        let inb = self.expect_i1(inb_raw, true);
        let fast_bb = self.new_block("rv.fast");
        let slow_bb = self.new_block("rv.slow");
        let cont_bb = self.new_block("rv.cont");
        self.builder
            .build_conditional_branch(inb, fast_bb, slow_bb)
            .unwrap();

        self.builder.position_at_end(fast_bb);
        // SAFETY: `ULT(idx, len)` above guarantees `0 <= idx < len`.
        let elem_ptr = unsafe {
            self.builder
                .build_in_bounds_gep(f64_t, data, &[idx], "rv.ep")
                .unwrap()
        };
        let fast_v = self
            .builder
            .build_load(f64_t, elem_ptr, "rv.ev")
            .unwrap()
            .into_float_value();
        let fast_from = self.cur_block().unwrap();
        self.br_to(cont_bb);

        self.builder.position_at_end(slow_bb);
        let slow_v = if is_ulist {
            let ul_ptr = self
                .builder
                .build_load(self.ptr_ty(), orig_slot, "rv.ul")
                .unwrap()
                .into_pointer_value();
            let f = self.tramp(
                "spid3r_ulist_f64_get",
                &[self.ptr_ty().into(), self.i64_ty().into()],
                Some(f64_t.into()),
            );
            let call_r = self.emit_call(f, &[ul_ptr.into(), idx.into()], "rv.sget");
            let sv = match call_r.try_as_basic_value() {
                inkwell::values::ValueKind::Basic(v) => v.into_float_value(),
                _ => unreachable!(),
            };
            self.throw_guard();
            sv
        } else {
            let idx_v2 = self.v_int_from_i64(idx);
            let bv = self.call2p("spid3r_index_get", orig_slot, idx_v2);
            self.throw_guard();
            let tof = self.tramp("spid3r_to_f64", &[self.ptr_ty().into()], Some(f64_t.into()));
            let call_r = self
                .builder
                .build_call(tof, &[bv.into()], "rv.f64")
                .unwrap();
            match call_r.try_as_basic_value() {
                inkwell::values::ValueKind::Basic(v) => v.into_float_value(),
                _ => unreachable!(),
            }
        };
        let slow_from = self.cur_block().unwrap();
        self.br_to(cont_bb);

        self.builder.position_at_end(cont_bb);
        let phi = self.builder.build_phi(f64_t, "rv.phi").unwrap();
        phi.add_incoming(&[(&fast_v, fast_from), (&slow_v, slow_from)]);
        Ok(phi.as_basic_value().into_float_value())
    }

    /// Phase 1: simulate expression type within a candidate hoisted `while` loop
    /// using the candidate `rows` and `promoted` maps. Returns `None` if an
    /// expression falls back to dynamic `ValueV2` operations or uses `%` on floats.
    fn sim_expr_kind(
        &self,
        e: &Expr,
        rows: &BTreeMap<String, UnboxedElem>,
        promoted: &BTreeMap<String, (PromotedKind, bool)>,
        loop_declared: &HashSet<String>,
    ) -> Option<SimKind> {
        match e {
            Expr::Int(..) => Some(SimKind::Int),
            Expr::Float(..) => Some(SimKind::Float),
            Expr::Bool(..) => Some(SimKind::Bool),
            Expr::Ident(name, _) => {
                if rows.contains_key(name) {
                    return None;
                }
                if loop_declared.contains(name) {
                    if self.int_locals.as_ref().is_some_and(|s| s.contains(name)) {
                        return Some(SimKind::Int);
                    }
                    if self.float_locals.as_ref().is_some_and(|s| s.contains(name)) {
                        return Some(SimKind::Float);
                    }
                    return None;
                }
                if let Some((pk, _)) = promoted.get(name) {
                    return Some(match pk {
                        PromotedKind::Int => SimKind::Int,
                        PromotedKind::Float => SimKind::Float,
                    });
                }
                match self.lookup(name) {
                    Some(VarLoc::LocalInt(_)) => Some(SimKind::Int),
                    Some(VarLoc::LocalFloat(_)) => Some(SimKind::Float),
                    Some(_) => None,
                    None => self.top_ints.contains_key(name).then_some(SimKind::Int),
                }
            }
            Expr::Unary {
                op: UnOp::Neg,
                expr,
                ..
            } => match self.sim_expr_kind(expr, rows, promoted, loop_declared) {
                Some(SimKind::Int) => Some(SimKind::Int),
                Some(SimKind::Float) => Some(SimKind::Float),
                _ => None,
            },
            Expr::Unary {
                op: UnOp::Not,
                expr,
                ..
            } => matches!(
                self.sim_expr_kind(expr, rows, promoted, loop_declared),
                Some(SimKind::Bool)
            )
            .then_some(SimKind::Bool),
            Expr::Binary { op, l, r, .. } => {
                let lk = self.sim_expr_kind(l, rows, promoted, loop_declared)?;
                let rk = self.sim_expr_kind(r, rows, promoted, loop_declared)?;
                match op {
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => match (lk, rk) {
                        (SimKind::Int, SimKind::Int) => Some(SimKind::Int),
                        (SimKind::Float, SimKind::Float | SimKind::Int)
                        | (SimKind::Int, SimKind::Float) => Some(SimKind::Float),
                        _ => None,
                    },
                    BinOp::Mod => match (lk, rk) {
                        (SimKind::Int, SimKind::Int) => Some(SimKind::Int),
                        _ => None,
                    },
                    BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne => {
                        match (lk, rk) {
                            (SimKind::Int | SimKind::Float, SimKind::Int | SimKind::Float) => {
                                Some(SimKind::Bool)
                            }
                            _ => None,
                        }
                    }
                }
            }
            Expr::Index { object, index, .. } => {
                let Expr::Ident(name, _) = object.as_ref() else {
                    return None;
                };
                if self.sim_expr_kind(index, rows, promoted, loop_declared) != Some(SimKind::Int) {
                    return None;
                }
                if !loop_declared.contains(name) {
                    if let Some(&elem) = rows.get(name) {
                        return Some(match elem {
                            UnboxedElem::I64 => SimKind::Int,
                            UnboxedElem::F64 => SimKind::Float,
                        });
                    }
                    match self.lookup(name) {
                        Some(VarLoc::UListI64(_)) | Some(VarLoc::RowViewI64 { .. }) => {
                            return Some(SimKind::Int);
                        }
                        Some(VarLoc::UListF64(_)) | Some(VarLoc::RowViewF64 { .. }) => {
                            return Some(SimKind::Float);
                        }
                        _ => {}
                    }
                }
                None
            }
            Expr::Method {
                object, name, args, ..
            } if name == "len" && args.is_empty() => {
                let Expr::Ident(vname, _) = object.as_ref() else {
                    return None;
                };
                if !loop_declared.contains(vname)
                    && (rows.contains_key(vname)
                        || matches!(
                            self.lookup(vname),
                            Some(VarLoc::UListI64(_))
                                | Some(VarLoc::UListF64(_))
                                | Some(VarLoc::RowViewI64 { .. })
                                | Some(VarLoc::RowViewF64 { .. })
                        ))
                {
                    return Some(SimKind::Int);
                }
                None
            }
            _ => None,
        }
    }

    /// Phase 1: analyze a `while` loop for hoisted 2D row views, `ValueV2` scalar
    /// promotion, and loop-invariant bounds-check elimination (BCE).
    fn analyze_hoisted_while_loop(&self, cond: &Expr, body: &[Stmt]) -> Option<HoistedLoopPlan> {
        if body.is_empty() {
            return None;
        }
        struct LoopWalk<'a, 'ctx> {
            loop_declared: HashSet<String>,
            assigned_vars: HashSet<String>,
            nested_assigned: HashSet<String>,
            indexed_objs: HashSet<String>,
            standalone_uses: HashSet<String>,
            all_idents: HashSet<String>,
            index_vars: HashSet<String>,
            mod_vars: HashSet<String>,
            has_continue: bool,
            uses_mod: bool,
            has_float_signal: bool,
            index_pairs: Vec<(String, &'a Expr)>,
            assigns: Vec<(String, AssignOp, &'a Expr)>,
            typed_decls: Vec<(String, Option<&'a Expr>)>,
            all_exprs: Vec<&'a Expr>,
            float_hint_vars: &'a HashSet<String>,
            ir: &'a Ir<'ctx>,
        }

        fn collect_ident_list(e: &Expr, out: &mut HashSet<String>) {
            match e {
                Expr::Ident(n, _) => {
                    out.insert(n.clone());
                }
                Expr::Unary { expr, .. } => collect_ident_list(expr, out),
                Expr::Binary { l, r, .. } | Expr::Logical { l, r, .. } => {
                    collect_ident_list(l, out);
                    collect_ident_list(r, out);
                }
                Expr::Index { object, index, .. } => {
                    collect_ident_list(object, out);
                    collect_ident_list(index, out);
                }
                Expr::Method { object, args, .. } => {
                    collect_ident_list(object, out);
                    for a in args {
                        collect_ident_list(a, out);
                    }
                }
                _ => {}
            }
        }

        fn walk_expr<'a, 'ctx>(
            e: &'a Expr,
            top_assign: bool,
            in_index: bool,
            in_mod: bool,
            w: &mut LoopWalk<'a, 'ctx>,
        ) -> bool {
            w.all_exprs.push(e);
            match e {
                Expr::Int(..) | Expr::Bool(..) => true,
                Expr::Float(..) => {
                    w.has_float_signal = true;
                    true
                }
                Expr::Ident(name, _) => {
                    w.all_idents.insert(name.clone());
                    w.standalone_uses.insert(name.clone());
                    if in_index {
                        w.index_vars.insert(name.clone());
                    }
                    if in_mod {
                        w.mod_vars.insert(name.clone());
                    }
                    if w.float_hint_vars.contains(name)
                        || matches!(
                            w.ir.lookup(name),
                            Some(VarLoc::LocalFloat(_))
                                | Some(VarLoc::UListF64(_))
                                | Some(VarLoc::RowViewF64 { .. })
                        )
                    {
                        w.has_float_signal = true;
                    }
                    true
                }
                Expr::Unary { expr, .. } => walk_expr(expr, false, in_index, in_mod, w),
                Expr::Binary { op, l, r, .. } => {
                    let is_m = *op == BinOp::Mod;
                    if is_m {
                        w.uses_mod = true;
                    }
                    walk_expr(l, false, in_index, in_mod || is_m, w)
                        && walk_expr(r, false, in_index, in_mod || is_m, w)
                }
                Expr::Logical { l, r, .. } => {
                    walk_expr(l, false, in_index, in_mod, w)
                        && walk_expr(r, false, in_index, in_mod, w)
                }
                Expr::Assign {
                    target, op, value, ..
                } => {
                    let Expr::Ident(tname, _) = target.as_ref() else {
                        return false;
                    };
                    w.all_idents.insert(tname.clone());
                    w.assigned_vars.insert(tname.clone());
                    if !top_assign {
                        w.nested_assigned.insert(tname.clone());
                    }
                    let is_m = *op == AssignOp::ModAssign;
                    if is_m {
                        w.uses_mod = true;
                        w.mod_vars.insert(tname.clone());
                    }
                    if w.float_hint_vars.contains(tname)
                        || matches!(
                            w.ir.lookup(tname),
                            Some(VarLoc::LocalFloat(_)) | Some(VarLoc::UListF64(_))
                        )
                    {
                        w.has_float_signal = true;
                    }
                    w.assigns.push((tname.clone(), *op, value.as_ref()));
                    walk_expr(value, false, in_index, in_mod || is_m, w)
                }
                Expr::Index { object, index, .. } => {
                    let Expr::Ident(obj_name, _) = object.as_ref() else {
                        return false;
                    };
                    w.all_idents.insert(obj_name.clone());
                    w.indexed_objs.insert(obj_name.clone());
                    if matches!(
                        w.ir.lookup(obj_name),
                        Some(VarLoc::UListF64(_)) | Some(VarLoc::RowViewF64 { .. })
                    ) {
                        w.has_float_signal = true;
                    }
                    w.index_pairs.push((obj_name.clone(), index.as_ref()));
                    walk_expr(index, false, true, in_mod, w)
                }
                Expr::Method {
                    object, name, args, ..
                } if name == "len" && args.is_empty() => {
                    let Expr::Ident(obj_name, _) = object.as_ref() else {
                        return false;
                    };
                    w.all_idents.insert(obj_name.clone());
                    if matches!(
                        w.ir.lookup(obj_name),
                        Some(VarLoc::UListF64(_)) | Some(VarLoc::RowViewF64 { .. })
                    ) {
                        w.has_float_signal = true;
                    }
                    true
                }
                _ => false,
            }
        }

        fn walk_stmt<'a, 'ctx>(s: &'a Stmt, w: &mut LoopWalk<'a, 'ctx>) -> bool {
            match s {
                Stmt::Let { name, value, .. } => {
                    w.loop_declared.insert(name.clone());
                    w.typed_decls.push((name.clone(), value.as_ref()));
                    value
                        .as_ref()
                        .is_none_or(|e| walk_expr(e, false, false, false, w))
                }
                Stmt::Const { name, value, .. } => {
                    w.loop_declared.insert(name.clone());
                    w.typed_decls.push((name.clone(), Some(value)));
                    walk_expr(value, false, false, false, w)
                }
                Stmt::If {
                    cond,
                    then,
                    elifs,
                    els,
                    ..
                } => {
                    if !walk_expr(cond, false, false, false, w) || !walk_block(&then.stmts, w) {
                        return false;
                    }
                    for (c, b) in elifs {
                        if !walk_expr(c, false, false, false, w) || !walk_block(&b.stmts, w) {
                            return false;
                        }
                    }
                    els.as_ref().is_none_or(|b| walk_block(&b.stmts, w))
                }
                Stmt::Block(b) => walk_block(&b.stmts, w),
                Stmt::Expr(e) => walk_expr(e, true, false, false, w),
                Stmt::Break { .. } => true,
                Stmt::Continue { .. } => {
                    w.has_continue = true;
                    true
                }
                _ => false,
            }
        }

        fn walk_block<'a, 'ctx>(stmts: &'a [Stmt], w: &mut LoopWalk<'a, 'ctx>) -> bool {
            stmts.iter().all(|st| walk_stmt(st, w))
        }

        let mut w = LoopWalk {
            loop_declared: HashSet::new(),
            assigned_vars: HashSet::new(),
            nested_assigned: HashSet::new(),
            indexed_objs: HashSet::new(),
            standalone_uses: HashSet::new(),
            all_idents: HashSet::new(),
            index_vars: HashSet::new(),
            mod_vars: HashSet::new(),
            has_continue: false,
            uses_mod: false,
            has_float_signal: false,
            index_pairs: Vec::new(),
            assigns: Vec::new(),
            typed_decls: Vec::new(),
            all_exprs: Vec::new(),
            float_hint_vars: &self.float_hint_vars,
            ir: self,
        };

        if !walk_expr(cond, false, false, false, &mut w) || !walk_block(body, &mut w) {
            return None;
        }

        let mut rows = BTreeMap::new();
        let mut had_local_row = false;
        for name in &w.indexed_objs {
            if w.loop_declared.contains(name)
                || w.assigned_vars.contains(name)
                || w.standalone_uses.contains(name)
            {
                continue;
            }
            match self.lookup(name) {
                Some(VarLoc::Local(_)) => {
                    let elem = if w.has_float_signal && !w.uses_mod {
                        UnboxedElem::F64
                    } else {
                        UnboxedElem::I64
                    };
                    rows.insert(name.clone(), elem);
                    had_local_row = true;
                }
                Some(VarLoc::UListI64(_)) => {
                    rows.insert(name.clone(), UnboxedElem::I64);
                }
                Some(VarLoc::UListF64(_)) => {
                    rows.insert(name.clone(), UnboxedElem::F64);
                }
                _ => {}
            }
        }
        if rows.is_empty() {
            return None;
        }

        let mut promoted: BTreeMap<String, (PromotedKind, bool)> = BTreeMap::new();
        for name in &w.all_idents {
            if rows.contains_key(name)
                || w.loop_declared.contains(name)
                || w.nested_assigned.contains(name)
            {
                continue;
            }
            if matches!(self.lookup(name), Some(VarLoc::Local(_))) {
                let kind = if self.float_hint_vars.contains(name) {
                    PromotedKind::Float
                } else {
                    PromotedKind::Int
                };
                let written = w.assigned_vars.contains(name);
                promoted.insert(name.clone(), (kind, written));
            }
        }

        loop {
            let mut changed = false;
            let mut drop_promoted: Option<String> = None;
            let mut upgrade_float: Option<String> = None;
            for (tname, op, rhs) in &w.assigns {
                if let Some(&(kind, _)) = promoted.get(tname) {
                    let rk = self.sim_expr_kind(rhs, &rows, &promoted, &w.loop_declared);
                    match kind {
                        PromotedKind::Int => match rk {
                            Some(SimKind::Int) => {}
                            Some(SimKind::Float)
                                if *op != AssignOp::ModAssign
                                    && !w.index_vars.contains(tname)
                                    && !w.mod_vars.contains(tname) =>
                            {
                                upgrade_float = Some(tname.clone());
                                break;
                            }
                            _ => {
                                drop_promoted = Some(tname.clone());
                                break;
                            }
                        },
                        PromotedKind::Float => {
                            if *op == AssignOp::ModAssign
                                || !matches!(rk, Some(SimKind::Float | SimKind::Int))
                            {
                                drop_promoted = Some(tname.clone());
                                break;
                            }
                        }
                    }
                } else if !w.loop_declared.contains(tname) {
                    match self.lookup(tname) {
                        Some(VarLoc::LocalInt(_)) => {
                            if self.sim_expr_kind(rhs, &rows, &promoted, &w.loop_declared)
                                != Some(SimKind::Int)
                            {
                                return None;
                            }
                        }
                        Some(VarLoc::LocalFloat(_))
                            if (*op == AssignOp::ModAssign
                                || !matches!(
                                    self.sim_expr_kind(rhs, &rows, &promoted, &w.loop_declared),
                                    Some(SimKind::Float | SimKind::Int)
                                )) =>
                        {
                            return None;
                        }
                        _ => {}
                    }
                }
            }
            if let Some(u) = upgrade_float {
                if let Some(entry) = promoted.get_mut(&u) {
                    entry.0 = PromotedKind::Float;
                    changed = true;
                }
            } else if let Some(d) = drop_promoted {
                promoted.remove(&d);
                changed = true;
            }
            if changed {
                continue;
            }

            let mut drop_row: Option<String> = None;
            for (rname, idx_expr) in &w.index_pairs {
                if rows.contains_key(rname)
                    && self.sim_expr_kind(idx_expr, &rows, &promoted, &w.loop_declared)
                        != Some(SimKind::Int)
                {
                    drop_row = Some(rname.clone());
                    break;
                }
            }
            if let Some(r) = drop_row {
                rows.remove(&r);
                continue;
            }
            break;
        }

        if rows.is_empty() {
            return None;
        }

        // Validate loop-local bindings whose slot type was chosen by function-wide
        // analysis (`int_locals` / `float_locals`) as well as float `%` safety.
        for (dname, val_opt) in &w.typed_decls {
            let is_int = self.int_locals.as_ref().is_some_and(|s| s.contains(dname));
            let is_float = !is_int
                && self
                    .float_locals
                    .as_ref()
                    .is_some_and(|s| s.contains(dname));
            if let Some(v) = val_opt {
                if is_int
                    && self.sim_expr_kind(v, &rows, &promoted, &w.loop_declared)
                        != Some(SimKind::Int)
                {
                    return None;
                }
                if is_float
                    && !matches!(
                        self.sim_expr_kind(v, &rows, &promoted, &w.loop_declared),
                        Some(SimKind::Float | SimKind::Int)
                    )
                {
                    return None;
                }
            }
        }
        for (tname, op, rhs) in &w.assigns {
            if w.loop_declared.contains(tname) {
                let is_int = self.int_locals.as_ref().is_some_and(|s| s.contains(tname));
                let is_float = !is_int
                    && self
                        .float_locals
                        .as_ref()
                        .is_some_and(|s| s.contains(tname));
                if is_int
                    && self.sim_expr_kind(rhs, &rows, &promoted, &w.loop_declared)
                        != Some(SimKind::Int)
                {
                    return None;
                }
                if is_float
                    && (*op == AssignOp::ModAssign
                        || !matches!(
                            self.sim_expr_kind(rhs, &rows, &promoted, &w.loop_declared),
                            Some(SimKind::Float | SimKind::Int)
                        ))
                {
                    return None;
                }
            }
        }
        for e in &w.all_exprs {
            if let Expr::Binary {
                op: BinOp::Mod,
                l,
                r,
                ..
            } = e
            {
                let lk = self.sim_expr_kind(l, &rows, &promoted, &w.loop_declared);
                let rk = self.sim_expr_kind(r, &rows, &promoted, &w.loop_declared);
                if matches!(lk, Some(SimKind::Float)) || matches!(rk, Some(SimKind::Float)) {
                    return None;
                }
            }
        }

        // Analyze pre-loop BCE.
        let bce = if !w.has_continue {
            if let Expr::Binary {
                op: cmp_op @ (BinOp::Lt | BinOp::Le),
                l,
                r,
                ..
            } = cond
            {
                if let Expr::Ident(idx_var, _) = l.as_ref() {
                    let idx_ok = !w.loop_declared.contains(idx_var)
                        && (matches!(self.lookup(idx_var), Some(VarLoc::LocalInt(_)))
                            || matches!(promoted.get(idx_var), Some((PromotedKind::Int, _))));
                    let bound_ok = self.sim_expr_kind(r, &rows, &promoted, &w.loop_declared)
                        == Some(SimKind::Int);
                    let mut bound_idents = HashSet::new();
                    collect_ident_list(r, &mut bound_idents);
                    let bound_inv = bound_idents
                        .iter()
                        .all(|id| !w.assigned_vars.contains(id) && !w.loop_declared.contains(id));

                    let is_unit_inc = matches!(
                        body.last(),
                        Some(Stmt::Expr(Expr::Assign { target, op, value, .. }))
                        if matches!(target.as_ref(), Expr::Ident(tn, _) if tn == idx_var)
                            && match (op, value.as_ref()) {
                                (AssignOp::AddAssign, Expr::Int(1, _)) => true,
                                (AssignOp::Assign, Expr::Binary { op: BinOp::Add, l, r, .. }) => {
                                    (matches!(l.as_ref(), Expr::Ident(a, _) if a == idx_var)
                                        && matches!(r.as_ref(), Expr::Int(1, _)))
                                        || (matches!(l.as_ref(), Expr::Int(1, _))
                                            && matches!(r.as_ref(), Expr::Ident(b, _) if b == idx_var))
                                }
                                _ => false,
                            }
                    );

                    if idx_ok && bound_ok && bound_inv && is_unit_inc {
                        let mut early_check = LoopWalk {
                            loop_declared: HashSet::new(),
                            assigned_vars: HashSet::new(),
                            nested_assigned: HashSet::new(),
                            indexed_objs: HashSet::new(),
                            standalone_uses: HashSet::new(),
                            all_idents: HashSet::new(),
                            index_vars: HashSet::new(),
                            mod_vars: HashSet::new(),
                            has_continue: false,
                            uses_mod: false,
                            has_float_signal: false,
                            index_pairs: Vec::new(),
                            assigns: Vec::new(),
                            typed_decls: Vec::new(),
                            all_exprs: Vec::new(),
                            float_hint_vars: &self.float_hint_vars,
                            ir: self,
                        };
                        let ok_prefix = walk_expr(cond, false, false, false, &mut early_check)
                            && walk_block(&body[..body.len() - 1], &mut early_check);
                        if ok_prefix && !early_check.assigned_vars.contains(idx_var) {
                            let mut bce_rows = BTreeSet::new();
                            for rname in rows.keys() {
                                let mut hits = 0usize;
                                let mut all_match = true;
                                for (obj_n, idx_e) in &early_check.index_pairs {
                                    if obj_n == rname {
                                        if matches!(idx_e, Expr::Ident(id, _) if id == idx_var) {
                                            hits += 1;
                                        } else {
                                            all_match = false;
                                            break;
                                        }
                                    }
                                }
                                if all_match && hits > 0 {
                                    bce_rows.insert(rname.clone());
                                }
                            }
                            if !bce_rows.is_empty() {
                                Some(BcePlan {
                                    idx_var: idx_var.clone(),
                                    bound_expr: r.as_ref().clone(),
                                    inclusive: *cmp_op == BinOp::Le,
                                    bce_rows,
                                })
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        if !had_local_row && bce.is_none() {
            return None;
        }

        Some(HoistedLoopPlan {
            rows,
            promoted,
            bce,
        })
    }

    fn emit_while_loop_plain(&mut self, cond: &Expr, body: &Block) -> IrResult<()> {
        let cond_bb = self.new_block("while.cond");
        let body_bb = self.new_block("while.body");
        let end_bb = self.new_block("while.end");
        self.br_to(cond_bb);
        self.builder.position_at_end(cond_bb);
        self.emit_cond_branch(cond, body_bb, end_bb)?;
        self.builder.position_at_end(body_bb);
        // Cancellation point: the counter slot is allocated in the
        // entry block, so it is live for the whole loop.
        let counter = self.alloca_entry_ty(self.i64_ty(), "cancel.counter");
        self.init_entry_slot(counter, self.i64c(0));
        self.emit_cancel_poll(counter);
        self.loops.push(LoopFrame {
            break_bb: end_bb,
            continue_bb: cond_bb,
        });
        let res = self.compile_block(&body.stmts);
        self.loops.pop();
        res?;
        self.br_to(cond_bb);
        self.builder.position_at_end(end_bb);
        Ok(())
    }

    /// Phase 1: emit multiversioned `while` loop with hoisted `RowView*` slice
    /// views, promoted `ValueV2` scalar accumulators (written back on both
    /// normal/break and exception exits), and pre-loop BCE with chunked cancel
    /// polling so LLVM can vectorize the inner loop cleanly.
    fn emit_hoisted_while_loop(
        &mut self,
        cond: &Expr,
        body: &Block,
        plan: HoistedLoopPlan,
    ) -> IrResult<()> {
        let f64_t = self.ctx.f64_type();
        let v2_ty = self.val_v2_ty();
        let hdr_ty = self.ulist_hdr_ty();
        let lv_ty = self.ctx.struct_type(
            &[
                self.ptr_ty().into(),
                self.i64_ty().into(),
                self.i64_ty().into(),
            ],
            false,
        );

        let fast_prep_bb = self.new_block("whl.prep");
        let fast_run_bb = self.new_block("whl.fast");
        let fast_end_bb = self.new_block("whl.fast.end");
        let fast_exc_bb = self.new_block("whl.fast.exc");
        let slow_loop_bb = self.new_block("whl.slow");
        let after_bb = self.new_block("whl.after");

        self.br_to(fast_prep_bb);
        self.builder.position_at_end(fast_prep_bb);

        let mut acquired_rows: Vec<(String, VarLoc<'ctx>)> = Vec::with_capacity(plan.rows.len());
        for (name, elem) in &plan.rows {
            match self.lookup(name) {
                Some(VarLoc::Local(orig_slot)) => {
                    let lv_slot = self.alloca_entry_ty(lv_ty, "lv");
                    let fn_name = match elem {
                        UnboxedElem::I64 => "spid3r_list_view_i64",
                        UnboxedElem::F64 => "spid3r_list_view_f64",
                    };
                    let f = self.tramp(
                        fn_name,
                        &[self.ptr_ty().into(), self.ptr_ty().into()],
                        Some(self.i8_ty().into()),
                    );
                    let ok_i8 =
                        call_int(self.emit_call(f, &[orig_slot.into(), lv_slot.into()], "lv.ok"));
                    let ok_raw = self
                        .builder
                        .build_int_compare(IntPredicate::NE, ok_i8, self.i8c(0), "lv.hit")
                        .unwrap();
                    let ok = self.expect_i1(ok_raw, true);
                    let next_bb = self.new_block("whl.prep.ok");
                    self.builder
                        .build_conditional_branch(ok, next_bb, slow_loop_bb)
                        .unwrap();
                    self.builder.position_at_end(next_bb);

                    let data_p = self
                        .builder
                        .build_struct_gep(lv_ty, lv_slot, 0, "lv.datap")
                        .unwrap();
                    let data = self
                        .builder
                        .build_load(self.ptr_ty(), data_p, "lv.data")
                        .unwrap()
                        .into_pointer_value();
                    let len_p = self
                        .builder
                        .build_struct_gep(lv_ty, lv_slot, 1, "lv.lenp")
                        .unwrap();
                    let len = self
                        .builder
                        .build_load(self.i64_ty(), len_p, "lv.len")
                        .unwrap()
                        .into_int_value();
                    let loc = match elem {
                        UnboxedElem::I64 => VarLoc::RowViewI64 {
                            data,
                            len,
                            orig_slot,
                            is_ulist: false,
                        },
                        UnboxedElem::F64 => VarLoc::RowViewF64 {
                            data,
                            len,
                            orig_slot,
                            is_ulist: false,
                        },
                    };
                    acquired_rows.push((name.clone(), loc));
                }
                Some(VarLoc::UListI64(orig_slot)) => {
                    let ul_ptr = self
                        .builder
                        .build_load(self.ptr_ty(), orig_slot, "rv.ul")
                        .unwrap()
                        .into_pointer_value();
                    let data_p = self
                        .builder
                        .build_struct_gep(hdr_ty, ul_ptr, 0, "ul.datap")
                        .unwrap();
                    let data = self
                        .builder
                        .build_load(self.ptr_ty(), data_p, "ul.data")
                        .unwrap()
                        .into_pointer_value();
                    let len = self.emit_ulist_len(ul_ptr);
                    acquired_rows.push((
                        name.clone(),
                        VarLoc::RowViewI64 {
                            data,
                            len,
                            orig_slot,
                            is_ulist: true,
                        },
                    ));
                }
                Some(VarLoc::UListF64(orig_slot)) => {
                    let ul_ptr = self
                        .builder
                        .build_load(self.ptr_ty(), orig_slot, "rv.ul")
                        .unwrap()
                        .into_pointer_value();
                    let data_p = self
                        .builder
                        .build_struct_gep(hdr_ty, ul_ptr, 0, "ul.datap")
                        .unwrap();
                    let data = self
                        .builder
                        .build_load(self.ptr_ty(), data_p, "ul.data")
                        .unwrap()
                        .into_pointer_value();
                    let len = self.emit_ulist_len(ul_ptr);
                    acquired_rows.push((
                        name.clone(),
                        VarLoc::RowViewF64 {
                            data,
                            len,
                            orig_slot,
                            is_ulist: true,
                        },
                    ));
                }
                _ => unreachable!("analyze_hoisted_while_loop checked VarLoc"),
            }
        }

        // Guard and unbox every promoted `ValueV2` scalar before entering the loop.
        struct PromotedSlot<'ctx> {
            name: String,
            kind: PromotedKind,
            written: bool,
            orig_slot: PointerValue<'ctx>,
            fast_slot: PointerValue<'ctx>,
        }
        let mut promoted_slots: Vec<PromotedSlot<'ctx>> = Vec::with_capacity(plan.promoted.len());
        for (name, (kind, written)) in &plan.promoted {
            let Some(VarLoc::Local(orig_slot)) = self.lookup(name) else {
                unreachable!("analyze_hoisted_while_loop checked Local for promoted scalar");
            };
            let tag_p = unsafe {
                self.builder
                    .build_in_bounds_gep(v2_ty, orig_slot, &[self.i64c(0), self.i32c(0)], "pm.tp")
                    .unwrap()
            };
            let tag = self
                .builder
                .build_load(self.i64_ty(), tag_p, "pm.tag")
                .unwrap()
                .into_int_value();
            let expected_tag = match kind {
                PromotedKind::Int => crate::value::ValueV2::TAG_INT,
                PromotedKind::Float => crate::value::ValueV2::TAG_FLOAT,
            };
            let tag_ok_raw = self
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    tag,
                    self.i64c(expected_tag as i64),
                    "pm.tok",
                )
                .unwrap();
            let tag_ok = self.expect_i1(tag_ok_raw, true);
            let next_bb = self.new_block("whl.pm.ok");
            self.builder
                .build_conditional_branch(tag_ok, next_bb, slow_loop_bb)
                .unwrap();
            self.builder.position_at_end(next_bb);

            let pay_p = unsafe {
                self.builder
                    .build_in_bounds_gep(v2_ty, orig_slot, &[self.i64c(0), self.i32c(1)], "pm.pp")
                    .unwrap()
            };
            let pay = self
                .builder
                .build_load(self.i64_ty(), pay_p, "pm.pay")
                .unwrap()
                .into_int_value();
            let fast_slot = match kind {
                PromotedKind::Int => {
                    let s = self.alloca_entry_ty(self.i64_ty(), "pm.i");
                    self.builder.build_store(s, pay).unwrap();
                    s
                }
                PromotedKind::Float => {
                    let s = self.alloca_entry_ty(f64_t, "pm.f");
                    let fv = self
                        .builder
                        .build_bit_cast(pay, f64_t, "pm.fbits")
                        .unwrap()
                        .into_float_value();
                    self.builder.build_store(s, fv).unwrap();
                    s
                }
            };
            promoted_slots.push(PromotedSlot {
                name: name.clone(),
                kind: *kind,
                written: *written,
                orig_slot,
                fast_slot,
            });
        }

        // Bind the hoisted row views and promoted scalar slots into a fast scope.
        self.push_scope();
        for (name, loc) in &acquired_rows {
            self.declare(name, *loc);
        }
        for ps in &promoted_slots {
            let loc = match ps.kind {
                PromotedKind::Int => VarLoc::LocalInt(ps.fast_slot),
                PromotedKind::Float => VarLoc::LocalFloat(ps.fast_slot),
            };
            self.declare(&ps.name, loc);
        }

        // Pre-loop BCE guards (evaluated before pushing `fast_exc_bb` so any
        // error in `bound_expr` unwinds without clobbering promoted scalars).
        let bce_exec = if let Some(ref bce) = plan.bce {
            let Some(VarLoc::LocalInt(idx_slot)) = self.lookup(&bce.idx_var) else {
                unreachable!("BCE idx_var must be LocalInt");
            };
            let idx0 = self
                .builder
                .build_load(self.i64_ty(), idx_slot, "bce.idx0")
                .unwrap()
                .into_int_value();
            let non_neg_raw = self
                .builder
                .build_int_compare(IntPredicate::SGE, idx0, self.i64c(0), "bce.ge0")
                .unwrap();
            let non_neg = self.expect_i1(non_neg_raw, true);
            let nn_bb = self.new_block("bce.nn");
            self.builder
                .build_conditional_branch(non_neg, nn_bb, slow_loop_bb)
                .unwrap();
            self.builder.position_at_end(nn_bb);

            let bound_val = match self.compile_expr_i64(&bce.bound_expr) {
                Ok(v) => v,
                Err(e) => {
                    self.pop_scope();
                    return Err(e);
                }
            };
            for rname in &bce.bce_rows {
                let row_len = match self.lookup(rname) {
                    Some(VarLoc::RowViewI64 { len, .. }) | Some(VarLoc::RowViewF64 { len, .. }) => {
                        len
                    }
                    _ => unreachable!("BCE row must be RowView*"),
                };
                let pred = if bce.inclusive {
                    IntPredicate::SLT
                } else {
                    IntPredicate::SLE
                };
                let inb_raw = self
                    .builder
                    .build_int_compare(pred, bound_val, row_len, "bce.bnd")
                    .unwrap();
                let inb = self.expect_i1(inb_raw, true);
                let ok_bb = self.new_block("bce.ok");
                self.builder
                    .build_conditional_branch(inb, ok_bb, slow_loop_bb)
                    .unwrap();
                self.builder.position_at_end(ok_bb);
            }
            let limit_val = if bce.inclusive {
                // SAFETY/no-overflow: `SLT(bound_val, row_len <= i64::MAX)` above
                // proves `bound_val < i64::MAX`, so `bound_val + 1 <= i64::MAX`.
                self.builder
                    .build_int_add(bound_val, self.i64c(1), "bce.lim")
                    .unwrap()
            } else {
                bound_val
            };
            Some((idx_slot, limit_val, bce))
        } else {
            None
        };

        self.br_to(fast_run_bb);
        self.builder.position_at_end(fast_run_bb);

        let outer_exc = self.exc.last().copied();
        if outer_exc.is_some() {
            self.exc.push(fast_exc_bb);
        }
        let mut new_bce = HashMap::new();
        if let Some((_, _, bce)) = &bce_exec {
            for rname in &bce.bce_rows {
                new_bce.insert(rname.clone(), bce.idx_var.clone());
            }
        }
        let prev_bce = std::mem::replace(&mut self.row_bce_vars, new_bce);

        let body_res: IrResult<()> = if let Some((idx_slot, limit_val, bce)) = &bce_exec {
            let _ = bce;
            if self.budget_counter_gv.is_none() {
                const CANCEL_POLL_INTERVAL: i64 = 2048;
                let counter = self.alloca_entry_ty(self.i64_ty(), "cancel.counter");
                self.init_entry_slot(counter, self.i64c(0));

                let outer_cond_bb = self.new_block("bce.outer.cond");
                let outer_body_bb = self.new_block("bce.outer.body");
                let cancel_chk_bb = self.new_block("bce.cancel.chk");
                let inner_cond_bb = self.new_block("bce.inner.cond");
                let inner_body_bb = self.new_block("bce.inner.body");

                self.br_to(outer_cond_bb);
                self.builder.position_at_end(outer_cond_bb);
                let idx_o = self
                    .builder
                    .build_load(self.i64_ty(), *idx_slot, "bce.io")
                    .unwrap()
                    .into_int_value();
                let more = self
                    .builder
                    .build_int_compare(IntPredicate::SLT, idx_o, *limit_val, "bce.more")
                    .unwrap();
                self.builder
                    .build_conditional_branch(more, outer_body_bb, fast_end_bb)
                    .unwrap();

                self.builder.position_at_end(outer_body_bb);
                let rem = self
                    .builder
                    .build_int_sub(*limit_val, idx_o, "bce.rem")
                    .unwrap();
                let max_chunk = self.i64c(CANCEL_POLL_INTERVAL);
                let gt_chunk = self
                    .builder
                    .build_int_compare(IntPredicate::SGT, rem, max_chunk, "bce.gt")
                    .unwrap();
                let step = self
                    .builder
                    .build_select(gt_chunk, max_chunk, rem, "bce.step")
                    .unwrap()
                    .into_int_value();
                let chunk_end = self.builder.build_int_add(idx_o, step, "bce.cend").unwrap();

                let cur_c = self
                    .builder
                    .build_load(self.i64_ty(), counter, "cancel.n")
                    .unwrap()
                    .into_int_value();
                let next_c = self
                    .builder
                    .build_int_add(cur_c, step, "cancel.ns")
                    .unwrap();
                self.builder.build_store(counter, next_c).unwrap();
                let hit_raw = self
                    .builder
                    .build_int_compare(IntPredicate::UGE, next_c, max_chunk, "cancel.hit")
                    .unwrap();
                let hit = self.expect_i1(hit_raw, false);
                self.builder
                    .build_conditional_branch(hit, cancel_chk_bb, inner_cond_bb)
                    .unwrap();

                self.builder.position_at_end(cancel_chk_bb);
                self.builder.build_store(counter, self.i64c(0)).unwrap();
                let f_chk = self.tramp("spid3r_cancel_check", &[], Some(self.i8_ty().into()));
                let _r = self.emit_call(f_chk, &[], "cancel.chk");
                self.throw_guard();
                self.br_to(inner_cond_bb);

                self.builder.position_at_end(inner_cond_bb);
                let idx_i = self
                    .builder
                    .build_load(self.i64_ty(), *idx_slot, "bce.ii")
                    .unwrap()
                    .into_int_value();
                let in_chunk = self
                    .builder
                    .build_int_compare(IntPredicate::SLT, idx_i, chunk_end, "bce.incnk")
                    .unwrap();
                self.builder
                    .build_conditional_branch(in_chunk, inner_body_bb, outer_cond_bb)
                    .unwrap();

                self.builder.position_at_end(inner_body_bb);
                self.loops.push(LoopFrame {
                    break_bb: fast_end_bb,
                    continue_bb: inner_cond_bb,
                });
                let r = self.compile_block(&body.stmts[..body.stmts.len() - 1]);
                self.loops.pop();
                if r.is_ok() && !self.sealed() {
                    let cur_idx = self
                        .builder
                        .build_load(self.i64_ty(), *idx_slot, "bce.idx")
                        .unwrap()
                        .into_int_value();
                    let next_idx = self
                        .builder
                        .build_int_add(cur_idx, self.i64c(1), "bce.inc")
                        .unwrap();
                    self.builder.build_store(*idx_slot, next_idx).unwrap();
                    self.br_to(inner_cond_bb);
                }
                r
            } else {
                let fast_cond_bb = self.new_block("whl.fast.cond");
                let fast_body_bb = self.new_block("whl.fast.body");
                self.br_to(fast_cond_bb);
                self.builder.position_at_end(fast_cond_bb);
                let idx_v = self
                    .builder
                    .build_load(self.i64_ty(), *idx_slot, "bce.iv")
                    .unwrap()
                    .into_int_value();
                let go = self
                    .builder
                    .build_int_compare(IntPredicate::SLT, idx_v, *limit_val, "bce.go")
                    .unwrap();
                self.builder
                    .build_conditional_branch(go, fast_body_bb, fast_end_bb)
                    .unwrap();
                self.builder.position_at_end(fast_body_bb);
                let counter = self.alloca_entry_ty(self.i64_ty(), "cancel.counter");
                self.init_entry_slot(counter, self.i64c(0));
                self.emit_cancel_poll(counter);
                self.loops.push(LoopFrame {
                    break_bb: fast_end_bb,
                    continue_bb: fast_cond_bb,
                });
                let r = self.compile_block(&body.stmts[..body.stmts.len() - 1]);
                self.loops.pop();
                if r.is_ok() && !self.sealed() {
                    let cur_idx = self
                        .builder
                        .build_load(self.i64_ty(), *idx_slot, "bce.idx")
                        .unwrap()
                        .into_int_value();
                    let next_idx = self
                        .builder
                        .build_int_add(cur_idx, self.i64c(1), "bce.inc")
                        .unwrap();
                    self.builder.build_store(*idx_slot, next_idx).unwrap();
                    self.br_to(fast_cond_bb);
                }
                r
            }
        } else {
            let fast_cond_bb = self.new_block("whl.fast.cond");
            let fast_body_bb = self.new_block("whl.fast.body");
            self.br_to(fast_cond_bb);
            self.builder.position_at_end(fast_cond_bb);
            let cond_r = self.emit_cond_branch(cond, fast_body_bb, fast_end_bb);
            if cond_r.is_ok() {
                self.builder.position_at_end(fast_body_bb);
                let counter = self.alloca_entry_ty(self.i64_ty(), "cancel.counter");
                self.init_entry_slot(counter, self.i64c(0));
                self.emit_cancel_poll(counter);
                self.loops.push(LoopFrame {
                    break_bb: fast_end_bb,
                    continue_bb: fast_cond_bb,
                });
                let r = self.compile_block(&body.stmts);
                self.loops.pop();
                if r.is_ok() {
                    self.br_to(fast_cond_bb);
                }
                r
            } else {
                cond_r
            }
        };

        self.row_bce_vars = prev_bce;
        if outer_exc.is_some() {
            self.exc.pop();
        }
        self.pop_scope();
        body_res?;

        // Normal / break exit: write back updated promoted scalars to `ValueV2`.
        self.builder.position_at_end(fast_end_bb);
        for ps in &promoted_slots {
            if !ps.written {
                continue;
            }
            match ps.kind {
                PromotedKind::Int => {
                    let v = self
                        .builder
                        .build_load(self.i64_ty(), ps.fast_slot, "wb.i")
                        .unwrap()
                        .into_int_value();
                    self.store_v2_inline(ps.orig_slot, crate::value::ValueV2::TAG_INT, v);
                }
                PromotedKind::Float => {
                    let fv = self
                        .builder
                        .build_load(f64_t, ps.fast_slot, "wb.f")
                        .unwrap()
                        .into_float_value();
                    let bits = self
                        .builder
                        .build_bit_cast(fv, self.i64_ty(), "wb.fb")
                        .unwrap()
                        .into_int_value();
                    self.store_v2_inline(ps.orig_slot, crate::value::ValueV2::TAG_FLOAT, bits);
                }
            }
        }
        self.br_to(after_bb);

        // Exception exit: write back updated promoted scalars before propagating.
        self.builder.position_at_end(fast_exc_bb);
        for ps in &promoted_slots {
            if !ps.written {
                continue;
            }
            match ps.kind {
                PromotedKind::Int => {
                    let v = self
                        .builder
                        .build_load(self.i64_ty(), ps.fast_slot, "ewb.i")
                        .unwrap()
                        .into_int_value();
                    self.store_v2_inline(ps.orig_slot, crate::value::ValueV2::TAG_INT, v);
                }
                PromotedKind::Float => {
                    let fv = self
                        .builder
                        .build_load(f64_t, ps.fast_slot, "ewb.f")
                        .unwrap()
                        .into_float_value();
                    let bits = self
                        .builder
                        .build_bit_cast(fv, self.i64_ty(), "ewb.fb")
                        .unwrap()
                        .into_int_value();
                    self.store_v2_inline(ps.orig_slot, crate::value::ValueV2::TAG_FLOAT, bits);
                }
            }
        }
        if let Some(outer) = outer_exc {
            self.br_to(outer);
        } else {
            self.builder.build_unreachable().unwrap();
        }

        // Slow fallback path (guards failed).
        self.builder.position_at_end(slow_loop_bb);
        self.emit_while_loop_plain(cond, body)?;
        self.br_to(after_bb);

        self.builder.position_at_end(after_bb);
        Ok(())
    }

    fn emit_f64_binop(
        &mut self,
        lhs: inkwell::values::FloatValue<'ctx>,
        rhs: inkwell::values::FloatValue<'ctx>,
        op: BinOp,
    ) -> IrResult<inkwell::values::FloatValue<'ctx>> {
        match op {
            BinOp::Add => {
                let n = self.tmp_name("fadd");
                Ok(self.builder.build_float_add(lhs, rhs, &n).unwrap())
            }
            BinOp::Sub => {
                let n = self.tmp_name("fsub");
                Ok(self.builder.build_float_sub(lhs, rhs, &n).unwrap())
            }
            BinOp::Mul => {
                let n = self.tmp_name("fmul");
                Ok(self.builder.build_float_mul(lhs, rhs, &n).unwrap())
            }
            BinOp::Div => {
                let f64_t = self.ctx.f64_type();
                let is_zero_raw = self
                    .builder
                    .build_float_compare(FloatPredicate::OEQ, rhs, f64_t.const_float(0.0), "fdivz")
                    .unwrap();
                let is_zero = self.expect_i1(is_zero_raw, false);
                let fast_bb = self.new_block("fdiv.fast");
                let slow_bb = self.new_block("fdiv.slow");
                let cont_bb = self.new_block("fdiv.cont");
                self.builder
                    .build_conditional_branch(is_zero, slow_bb, fast_bb)
                    .unwrap();

                self.builder.position_at_end(fast_bb);
                let fnm = self.tmp_name("fdiv");
                let fast = self.builder.build_float_div(lhs, rhs, &fnm).unwrap();
                let fast_from = self.cur_block().unwrap();
                self.br_to(cont_bb);

                self.builder.position_at_end(slow_bb);
                let bl = self.v_float_from_f64(lhs);
                let br = self.v_float_from_f64(rhs);
                let bv = self.call_binop(bl, br, binop_code(BinOp::Div));
                self.throw_guard();
                let tof = self.tramp("spid3r_to_f64", &[self.ptr_ty().into()], Some(f64_t.into()));
                let call_r = self.builder.build_call(tof, &[bv.into()], "f64").unwrap();
                let slow = match call_r.try_as_basic_value() {
                    inkwell::values::ValueKind::Basic(v) => v.into_float_value(),
                    _ => unreachable!(),
                };
                let slow_from = self.cur_block().unwrap();
                self.br_to(cont_bb);

                self.builder.position_at_end(cont_bb);
                let phi = self.builder.build_phi(f64_t, "fdiv.phi").unwrap();
                phi.add_incoming(&[(&fast, fast_from), (&slow, slow_from)]);
                Ok(phi.as_basic_value().into_float_value())
            }
            _ => unreachable!(),
        }
    }

    fn compile_expr_f64(&mut self, e: &Expr) -> IrResult<inkwell::values::FloatValue<'ctx>> {
        let f64_t = self.ctx.f64_type();
        match e {
            Expr::Float(v, _) => Ok(f64_t.const_float(*v)),
            Expr::Int(v, _) => Ok(f64_t.const_float(*v as f64)),
            Expr::Ident(name, _) => match self.lookup(name) {
                Some(VarLoc::LocalFloat(slot)) => {
                    let lname = self.tmp_name("fv");
                    Ok(self
                        .builder
                        .build_load(f64_t, slot, &lname)
                        .unwrap()
                        .into_float_value())
                }
                Some(VarLoc::LocalInt(slot)) => {
                    let lname = self.tmp_name("iv");
                    let iv = self
                        .builder
                        .build_load(self.i64_ty(), slot, &lname)
                        .unwrap()
                        .into_int_value();
                    Ok(self
                        .builder
                        .build_signed_int_to_float(iv, f64_t, "i2f")
                        .unwrap())
                }
                None => {
                    if let Some(&v) = self.top_ints.get(name) {
                        Ok(f64_t.const_float(v as f64))
                    } else {
                        panic!("compile_expr_f64 called on non-float ident {}", name)
                    }
                }
                _ => panic!("compile_expr_f64 called on non-float ident {}", name),
            },
            Expr::Unary {
                op: UnOp::Neg,
                expr,
                ..
            } => {
                if self.is_int_expr(e) {
                    let iv = self.compile_expr_i64(e)?;
                    return Ok(self
                        .builder
                        .build_signed_int_to_float(iv, f64_t, "i2f")
                        .unwrap());
                }
                let inner = self.compile_expr_f64(expr)?;
                let n = self.tmp_name("fneg");
                Ok(self.builder.build_float_neg(inner, &n).unwrap())
            }
            Expr::Binary { op, l, r, .. } => {
                if self.is_int_expr(e) {
                    let iv = self.compile_expr_i64(e)?;
                    return Ok(self
                        .builder
                        .build_signed_int_to_float(iv, f64_t, "i2f")
                        .unwrap());
                }
                let lhs = if self.is_float_expr(l) {
                    self.compile_expr_f64(l)?
                } else {
                    let iv = self.compile_expr_i64(l)?;
                    self.builder
                        .build_signed_int_to_float(iv, f64_t, "i2f")
                        .unwrap()
                };
                let rhs = if self.is_float_expr(r) {
                    self.compile_expr_f64(r)?
                } else {
                    let iv = self.compile_expr_i64(r)?;
                    self.builder
                        .build_signed_int_to_float(iv, f64_t, "i2f")
                        .unwrap()
                };
                self.emit_f64_binop(lhs, rhs, *op)
            }
            Expr::Index { object, index, .. } => {
                if let Expr::Ident(name, _) = &**object {
                    match self.lookup(name) {
                        Some(VarLoc::UListF64(slot)) => {
                            let ptr = self
                                .builder
                                .build_load(self.ptr_ty(), slot, "ul")
                                .unwrap()
                                .into_pointer_value();
                            let idx = self.compile_expr_i64(index)?;
                            return self.emit_ulist_f64_get_inline(ptr, idx);
                        }
                        Some(VarLoc::RowViewF64 {
                            data,
                            len,
                            orig_slot,
                            is_ulist,
                        }) => {
                            let idx = self.compile_expr_i64(index)?;
                            let bce_hit = self.is_row_bce_hit(name, index);
                            return self.emit_row_view_f64_get(
                                bce_hit, data, len, orig_slot, is_ulist, idx,
                            );
                        }
                        _ => {}
                    }
                }
                if self.is_int_expr(e) {
                    let iv = self.compile_expr_i64(e)?;
                    return Ok(self
                        .builder
                        .build_signed_int_to_float(iv, f64_t, "i2f")
                        .unwrap());
                }
                panic!("compile_expr_f64 called on non-float index")
            }
            Expr::Method { object, name, .. } if name == "pop" => {
                if let Expr::Ident(vname, _) = &**object {
                    if let Some(VarLoc::UListF64(slot)) = self.lookup(vname) {
                        let ptr = self
                            .builder
                            .build_load(self.ptr_ty(), slot, "ul")
                            .unwrap()
                            .into_pointer_value();
                        let f = self.tramp(
                            "spid3r_ulist_f64_pop",
                            &[self.ptr_ty().into()],
                            Some(f64_t.into()),
                        );
                        let call_r = self.emit_call(f, &[ptr.into()], "ul.pop");
                        let fv = match call_r.try_as_basic_value() {
                            inkwell::values::ValueKind::Basic(v) => v.into_float_value(),
                            _ => unreachable!(),
                        };
                        self.throw_guard();
                        return Ok(fv);
                    }
                }
                if self.is_int_expr(e) {
                    let iv = self.compile_expr_i64(e)?;
                    return Ok(self
                        .builder
                        .build_signed_int_to_float(iv, f64_t, "i2f")
                        .unwrap());
                }
                panic!("compile_expr_f64 called on non-float method")
            }
            Expr::Method { .. } if self.is_int_expr(e) => {
                let iv = self.compile_expr_i64(e)?;
                Ok(self
                    .builder
                    .build_signed_int_to_float(iv, f64_t, "i2f")
                    .unwrap())
            }
            Expr::Call {
                callee,
                type_args,
                args,
                ..
            } => {
                if let Expr::Ident(name, _) = &**callee {
                    if self.is_int_expr(e) {
                        let iv = self.compile_expr_i64(e)?;
                        return Ok(self
                            .builder
                            .build_signed_int_to_float(iv, f64_t, "i2f")
                            .unwrap());
                    }
                    if type_args.is_empty() {
                        if let Some(val) = self.try_compile_raw_call(name, args)? {
                            return Ok(val.into_float_value());
                        }
                    }
                }
                panic!("compile_expr_f64 called on non-float call")
            }
            _ => panic!("compile_expr_f64 called on non-float expr"),
        }
    }

    /// Declare an LLVM overflow-checking arithmetic intrinsic
    /// (`llvm.sadd.with.overflow.i64` and friends). Declared by hand so no
    /// inkwell `Intrinsic` lookup API is needed; LLVM links the definition
    /// automatically because the name is reserved.
    fn overflow_intrinsic(&mut self, name: &str) -> inkwell::values::FunctionValue<'ctx> {
        if let Some(f) = self.tramps.get(name) {
            return *f;
        }
        let pair = self
            .ctx
            .struct_type(&[self.i64_ty().into(), self.ctx.bool_type().into()], false);
        let ft = pair.fn_type(&[self.i64_ty().into(), self.i64_ty().into()], false);
        let f = self.module.add_function(name, ft, None);
        self.tramps.insert(name.to_string(), f);
        f
    }

    // ------------------------------------------------------------------
    // Phase 3: hybrid int expressions
    //
    // `is_int_expr` only accepts leaves backed by an UNBOXED `LocalInt` slot,
    // and `analyze_int_locals` deliberately excludes function parameters, so
    // a hot loop like `while d * d <= n` (with boxed `n`) previously compiled
    // its whole condition through `spid3r_binop` — an extern call plus a heap
    // box per operation. A hybrid expression has the same shape but allows
    // plain boxed `Local` slots as leaves: at run time each leaf is checked
    // with `spid3r_is_int` and extracted with `spid3r_int_of` (both pure), so
    // the arithmetic runs inline on registers. Any leaf that is not an `Int`
    // at run time falls back to the fully generic path, which recompiles the
    // ORIGINAL expression — float promotion, operator overloads and error
    // messages therefore stay byte-identical to the tree and VM engines.
    // Parameters are never unboxed at entry (a dynamic call can pass a
    // non-int to an annotated parameter — the semantic checker only rejects
    // statically-resolvable call sites), so the check stays at the use site.
    // ------------------------------------------------------------------

    /// Like [`Self::is_int_expr`], but a plain boxed `Local` slot also
    /// qualifies as an int leaf. Captured (`Cell`/`Upvalue`) bindings are
    /// excluded: their extraction is a runtime call, not a slot read.
    fn is_hybrid_int_expr(&self, e: &Expr) -> bool {
        match e {
            Expr::Int(..) => true,
            Expr::Ident(name, _) => match self.lookup(name) {
                Some(VarLoc::Local(_)) | Some(VarLoc::LocalInt(_)) => true,
                Some(_) => false,
                None => self.top_ints.contains_key(name),
            },
            Expr::Unary {
                op: UnOp::Neg,
                expr,
                ..
            } => self.is_hybrid_int_expr(expr),
            Expr::Binary { op, l, r, .. } => {
                matches!(
                    op,
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
                ) && self.is_hybrid_int_expr(l)
                    && self.is_hybrid_int_expr(r)
            }
            _ => false,
        }
    }

    /// The boxed leaves (in first-use order, deduplicated) of a hybrid int
    /// expression. Empty when every leaf is already unboxed.
    fn collect_hybrid_leaves<'a>(&self, e: &'a Expr, out: &mut Vec<&'a str>) {
        match e {
            Expr::Int(..) => {}
            Expr::Ident(name, _) => {
                if matches!(self.lookup(name), Some(VarLoc::Local(_)))
                    && !out.contains(&name.as_str())
                {
                    out.push(name.as_str());
                }
            }
            Expr::Unary { expr, .. } => self.collect_hybrid_leaves(expr, out),
            Expr::Binary { l, r, .. } => {
                self.collect_hybrid_leaves(l, out);
                self.collect_hybrid_leaves(r, out);
            }
            _ => {}
        }
    }

    fn is_hybrid_int_compare<'a>(&self, e: &'a Expr) -> Option<(BinOp, &'a Expr, &'a Expr)> {
        if self.hybrid_off {
            // A hybrid slow arm is recompiling this expression generically.
            return None;
        }
        if let Expr::Binary {
            op: op @ (BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne),
            l,
            r,
            ..
        } = e
        {
            if self.is_hybrid_int_expr(l) && self.is_hybrid_int_expr(r) {
                return Some((*op, l.as_ref(), r.as_ref()));
            }
        }
        None
    }

    /// Compile one hybrid operand to an i64 register. Boxed leaves were
    /// extracted by the caller into `vals` after the runtime type check.
    fn hybrid_i64(
        &mut self,
        e: &Expr,
        vals: &mut HashMap<&str, inkwell::values::IntValue<'ctx>>,
    ) -> IrResult<inkwell::values::IntValue<'ctx>> {
        match e {
            Expr::Int(v, _) => Ok(self.i64c(*v)),
            Expr::Ident(name, _) => match self.lookup(name) {
                Some(VarLoc::LocalInt(slot)) => {
                    let lname = self.tmp_name("iv");
                    Ok(self
                        .builder
                        .build_load(self.i64_ty(), slot, &lname)
                        .unwrap()
                        .into_int_value())
                }
                Some(VarLoc::Local(_)) => vals
                    .get(name.as_str())
                    .copied()
                    .ok_or_else(|| self.err("internal: hybrid leaf was not extracted")),
                None => self
                    .top_ints
                    .get(name)
                    .copied()
                    .map(|v| self.i64c(v))
                    .ok_or_else(|| self.err("internal: non-local ident in hybrid arm")),
                _ => Err(self.err("internal: non-local ident in hybrid arm")),
            },
            Expr::Unary {
                op: UnOp::Neg,
                expr,
                ..
            } => {
                let a = self.hybrid_i64(expr, vals)?;
                self.emit_i64_neg(a)
            }
            Expr::Binary { op, l, r, .. } => {
                let a = self.hybrid_i64(l, vals)?;
                let b = self.hybrid_i64(r, vals)?;
                self.emit_i64_binop(a, b, *op)
            }
            _ => Err(self.err("internal: non-int expr in hybrid arm")),
        }
    }

    /// Emit the shared prologue of a hybrid compare: a check chain over the
    /// boxed leaves. On success control enters `fast_bb`; any non-`Int` leaf
    /// branches to `slow_bb`. Returns the extracted i64 leaves.
    fn emit_hybrid_leaf_checks(
        &mut self,
        leaves: &[&str],
        fast_bb: BasicBlock<'ctx>,
        slow_bb: BasicBlock<'ctx>,
    ) -> IrResult<Vec<inkwell::values::IntValue<'ctx>>> {
        let mut slots = Vec::with_capacity(leaves.len());
        for name in leaves {
            match self.lookup(name) {
                Some(VarLoc::Local(slot)) => slots.push(slot),
                _ => return Err(self.err("internal: hybrid leaf is not a plain local")),
            }
        }
        let mut vals = Vec::with_capacity(slots.len());
        for (i, slot) in slots.iter().enumerate() {
            // THE BOX POINTER, not the slot address: `slot` is an `alloca ptr`
            // holding a `*mut Value`; the helpers expect the VALUE itself.
            // Passing the slot address made `deref()` reinterpret the slot's
            // own bytes (a heap pointer + adjacent stack garbage) as a `Value`
            // — an ASLR-dependent is-Int verdict feeding an out-of-bounds
            // payload read, i.e. the non-deterministic loop-bound corruption
            // fixed in this session. Every other boxed-local read site loads
            // the pointer first; so does this one.
            // Under DP-1 ValueV2, `slot` IS the `*const ValueV2` 16-byte slot.
            let box_ptr = *slot;
            let f = self.tramp(
                "spid3r_is_int",
                &[self.ptr_ty().into()],
                Some(self.i8_ty().into()),
            );
            let r = self.emit_call(f, &[box_ptr.into()], "isint");
            let ok_i8 = call_int(r);
            // Pure extraction, emitted BEFORE the branch: `spid3r_int_of`
            // never throws (it yields 0 for non-Int, a value the slow arm
            // supersedes), so nothing here can cross a terminator.
            let g = self.tramp(
                "spid3r_int_of",
                &[self.ptr_ty().into()],
                Some(self.i64_ty().into()),
            );
            let rv = self.emit_call(g, &[box_ptr.into()], "intof");
            vals.push(call_int(rv));
            let ok_name = self.tmp_name("isint.ok");
            let ok = self
                .builder
                .build_int_compare(IntPredicate::EQ, ok_i8, self.i8c(1), &ok_name)
                .unwrap();
            let on_ok = if i + 1 == slots.len() {
                fast_bb
            } else {
                self.new_block("hyb.chk")
            };
            self.builder
                .build_conditional_branch(ok, on_ok, slow_bb)
                .unwrap();
            if i + 1 < slots.len() {
                self.builder.position_at_end(on_ok);
            }
        }
        if slots.is_empty() {
            self.br_to(fast_bb);
        }
        Ok(vals)
    }

    /// CONDITION version: both arms end in a direct branch to `then_bb` /
    /// `else_bb`, so no value crosses the merge. The slow arm recompiles the
    /// ORIGINAL compare expression through [`Self::compile_expr`] — the same
    /// emission the generic path makes — so its semantics are exact.
    fn emit_hybrid_int_compare_cond(
        &mut self,
        op: BinOp,
        l: &Expr,
        r: &Expr,
        whole: &Expr,
        then_bb: BasicBlock<'ctx>,
        else_bb: BasicBlock<'ctx>,
    ) -> IrResult<()> {
        let predicate = match op {
            BinOp::Lt => IntPredicate::SLT,
            BinOp::Le => IntPredicate::SLE,
            BinOp::Gt => IntPredicate::SGT,
            BinOp::Ge => IntPredicate::SGE,
            BinOp::Eq => IntPredicate::EQ,
            BinOp::Ne => IntPredicate::NE,
            _ => return Err(self.err("internal: hybrid compare on non-compare op")),
        };
        let mut leaf_names: Vec<&str> = Vec::new();
        self.collect_hybrid_leaves(l, &mut leaf_names);
        self.collect_hybrid_leaves(r, &mut leaf_names);
        let fast_bb = self.new_block("hyb.fast");
        let slow_bb = self.new_block("hyb.slow");
        let vals = self.emit_hybrid_leaf_checks(&leaf_names, fast_bb, slow_bb)?;
        // Fast arm: pure register compare over the extracted leaves.
        self.builder.position_at_end(fast_bb);
        let mut by_name: HashMap<&str, inkwell::values::IntValue<'ctx>> = HashMap::new();
        for (name, v) in leaf_names.iter().zip(vals.iter()) {
            by_name.insert(*name, *v);
        }
        let lhs = self.hybrid_i64(l, &mut by_name)?;
        let rhs = self.hybrid_i64(r, &mut by_name)?;
        let cmp_name = self.tmp_name("hyb.cmp");
        let c = self
            .builder
            .build_int_compare(predicate, lhs, rhs, &cmp_name)
            .unwrap();
        self.builder
            .build_conditional_branch(c, then_bb, else_bb)
            .unwrap();
        // Slow arm: the generic path, verbatim. `hybrid_off` stops the
        // re-dispatch from matching this same compare again.
        self.builder.position_at_end(slow_bb);
        let saved = self.hybrid_off;
        self.hybrid_off = true;
        let slow_result = self.compile_expr(whole);
        self.hybrid_off = saved;
        let v = slow_result?;
        let c = self.cond_i1(v);
        self.builder
            .build_conditional_branch(c, then_bb, else_bb)
            .unwrap();
        Ok(())
    }

    /// VALUE version: both arms produce a boxed value (the fast arm uses the
    /// shared `true`/`false` singletons, so still no allocation) merged by a
    /// phi. The slow arm recompiles the original expression generically.
    fn emit_hybrid_int_compare_value(
        &mut self,
        op: BinOp,
        l: &Expr,
        r: &Expr,
        whole: &Expr,
    ) -> IrResult<PointerValue<'ctx>> {
        let predicate = match op {
            BinOp::Lt => IntPredicate::SLT,
            BinOp::Le => IntPredicate::SLE,
            BinOp::Gt => IntPredicate::SGT,
            BinOp::Ge => IntPredicate::SGE,
            BinOp::Eq => IntPredicate::EQ,
            BinOp::Ne => IntPredicate::NE,
            _ => return Err(self.err("internal: hybrid compare on non-compare op")),
        };
        let mut leaf_names: Vec<&str> = Vec::new();
        self.collect_hybrid_leaves(l, &mut leaf_names);
        self.collect_hybrid_leaves(r, &mut leaf_names);
        let fast_bb = self.new_block("hyb.fast");
        let slow_bb = self.new_block("hyb.slow");
        let merge_bb = self.new_block("hyb.merge");
        let vals = self.emit_hybrid_leaf_checks(&leaf_names, fast_bb, slow_bb)?;
        self.builder.position_at_end(fast_bb);
        let mut by_name: HashMap<&str, inkwell::values::IntValue<'ctx>> = HashMap::new();
        for (name, v) in leaf_names.iter().zip(vals.iter()) {
            by_name.insert(*name, *v);
        }
        let lhs = self.hybrid_i64(l, &mut by_name)?;
        let rhs = self.hybrid_i64(r, &mut by_name)?;
        let cmp_name = self.tmp_name("hyb.cmp");
        let c = self
            .builder
            .build_int_compare(predicate, lhs, rhs, &cmp_name)
            .unwrap();
        let fast_v = self.v_bool_from_i1(c);
        // `hybrid_i64` may have routed through an inline-binop merge block,
        // so capture wherever the builder actually sits (same for the slow
        // edge below).
        let fast_def_bb = self.cur_block();
        self.br_to(merge_bb);
        self.builder.position_at_end(slow_bb);
        let saved = self.hybrid_off;
        self.hybrid_off = true;
        let slow_result = self.compile_expr(whole);
        self.hybrid_off = saved;
        let slow_v = slow_result?;
        // `compile_expr` ends with a pending-throw guard that may have
        // continued into a fresh block — the phi's predecessor is wherever
        // the builder actually sits now, not `slow_bb`.
        let slow_def_bb = self.cur_block();
        self.br_to(merge_bb);
        self.builder.position_at_end(merge_bb);
        let merged = self.builder.build_phi(self.ptr_ty(), "hyb.phi").unwrap();
        merged.add_incoming(&[
            (&fast_v, fast_def_bb.unwrap()),
            (&slow_v, slow_def_bb.unwrap()),
        ]);
        Ok(merged.as_basic_value().into_pointer_value())
    }

    /// The pre-Phase-3 binop path: one opaque `extern "C"` call plus the
    /// pending-throw guard. Kept verbatim as the SLOW path of the inline
    /// lowering below, so every error message, the pending-throw routing and
    /// the exit code stay byte-identical when the fast path is not taken.
    fn emit_i64_binop_slow(
        &mut self,
        lhs: inkwell::values::IntValue<'ctx>,
        rhs: inkwell::values::IntValue<'ctx>,
        tramp_name: &str,
    ) -> IrResult<inkwell::values::IntValue<'ctx>> {
        let f = self.tramp(
            tramp_name,
            &[self.i64_ty().into(), self.i64_ty().into()],
            Some(self.i64_ty().into()),
        );
        let r = self.emit_call(f, &[lhs.into(), rhs.into()], "i64op");
        self.throw_guard();
        Ok(call_int(r))
    }

    /// Inline i64 negation with the exact `spid3r_i64_neg` semantics: only
    /// `i64::MIN` overflows, and the trampoline raises that error.
    fn emit_i64_neg(
        &mut self,
        a: inkwell::values::IntValue<'ctx>,
    ) -> IrResult<inkwell::values::IntValue<'ctx>> {
        let is_min_raw = self
            .builder
            .build_int_compare(IntPredicate::EQ, a, self.i64c(i64::MIN), "negchk")
            .unwrap();
        let is_min = self.expect_i1(is_min_raw, false);
        let fast_bb = self.new_block("neg.fast");
        let slow_bb = self.new_block("neg.slow");
        let cont_bb = self.new_block("neg.cont");
        self.builder
            .build_conditional_branch(is_min, slow_bb, fast_bb)
            .unwrap();
        self.builder.position_at_end(fast_bb);
        let zero = self.i64c(0);
        let fast = self.builder.build_int_sub(zero, a, "neg.res").unwrap();
        self.br_to(cont_bb);
        self.builder.position_at_end(slow_bb);
        let f = self.tramp(
            "spid3r_i64_neg",
            &[self.i64_ty().into()],
            Some(self.i64_ty().into()),
        );
        let r = self.emit_call(f, &[a.into()], "i64neg");
        self.throw_guard();
        let slow = call_int(r);
        let slow_def_bb = self.cur_block();
        self.br_to(cont_bb);
        self.builder.position_at_end(cont_bb);
        let merged = self.builder.build_phi(self.i64_ty(), "neg.phi").unwrap();
        merged.add_incoming(&[(&fast, fast_bb), (&slow, slow_def_bb.unwrap())]);
        Ok(merged.as_basic_value().into_int_value())
    }

    /// Lower an i64 binary operation. Phase 3 replaces the old opaque
    /// `extern "C"` trampoline with inline register arithmetic:
    ///
    /// - Add/Sub/Mul use LLVM's overflow intrinsics; the overflow edge calls
    ///   the SAME runtime trampoline as before, so `integer overflow: a + b`
    ///   (with the real operands), the pending-throw routing and the exit
    ///   code are unchanged. The fast edge is pure register code — no ABI
    ///   crossing, no heap box, no pending-throw check.
    /// - Div guards division by zero and `i64::MIN / -1` (both raise through
    ///   the trampoline) and lowers everything else to `sdiv`.
    /// - Mod guards division by zero (trampoline, exact
    ///   `modulo by zero` message) and divisor `-1` (Rust's `wrapping_rem`
    ///   yields 0, where LLVM `srem` would be undefined) and lowers the rest
    ///   to `srem`.
    ///
    /// Phase 1 diagnosed the trampoline-per-binop shape as the main reason
    /// `native --release` was *slower* than the VM; this is the P3 repair.
    fn emit_i64_binop(
        &mut self,
        lhs: inkwell::values::IntValue<'ctx>,
        rhs: inkwell::values::IntValue<'ctx>,
        op: BinOp,
    ) -> IrResult<inkwell::values::IntValue<'ctx>> {
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                let (intrinsic, tramp_name) = match op {
                    BinOp::Add => ("llvm.sadd.with.overflow.i64", "spid3r_i64_add"),
                    BinOp::Sub => ("llvm.ssub.with.overflow.i64", "spid3r_i64_sub"),
                    BinOp::Mul => ("llvm.smul.with.overflow.i64", "spid3r_i64_mul"),
                    _ => unreachable!(),
                };
                let ovf_f = self.overflow_intrinsic(intrinsic);
                let call_name = self.tmp_name("ovf");
                let call = self
                    .builder
                    .build_call(ovf_f, &[lhs.into(), rhs.into()], &call_name)
                    .unwrap();
                let pair = match call.try_as_basic_value() {
                    inkwell::values::ValueKind::Basic(b) => b.into_struct_value(),
                    _ => panic!("spid3r: overflow intrinsic did not return a struct"),
                };
                let fast = self
                    .builder
                    .build_extract_value(pair, 0, "ovf.res")
                    .unwrap()
                    .into_int_value();
                let overflowed_raw = self
                    .builder
                    .build_extract_value(pair, 1, "ovf.bit")
                    .unwrap()
                    .into_int_value();
                let overflowed = self.expect_i1(overflowed_raw, false);
                let entry_bb = self.cur_block();
                let slow_bb = self.new_block("i64op.slow");
                let cont_bb = self.new_block("i64op.cont");
                self.builder
                    .build_conditional_branch(overflowed, slow_bb, cont_bb)
                    .unwrap();
                self.builder.position_at_end(slow_bb);
                let slow = self.emit_i64_binop_slow(lhs, rhs, tramp_name)?;
                let slow_def_bb = self.cur_block();
                self.br_to(cont_bb);
                self.builder.position_at_end(cont_bb);
                let merged = self.builder.build_phi(self.i64_ty(), "i64op.phi").unwrap();
                merged.add_incoming(&[(&fast, entry_bb.unwrap()), (&slow, slow_def_bb.unwrap())]);
                Ok(merged.as_basic_value().into_int_value())
            }
            BinOp::Div => {
                // `b == 0` raises `division by zero`; `i64::MIN / -1` is the
                // only `checked_div` overflow. Both go through the trampoline
                // for the exact message; the fast edge is a plain `sdiv` in
                // its own block so the undefined `MIN / -1` machine op is
                // never reached on the slow edge.
                let is_zero = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, rhs, self.i64c(0), "divz")
                    .unwrap();
                let lhs_is_min = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, lhs, self.i64c(i64::MIN), "divmin")
                    .unwrap();
                let rhs_is_m1 = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, rhs, self.i64c(-1), "divm1")
                    .unwrap();
                let both_min_m1 = self
                    .builder
                    .build_and(lhs_is_min, rhs_is_m1, "divovf")
                    .unwrap();
                let bad_raw = self
                    .builder
                    .build_or(is_zero, both_min_m1, "divslow")
                    .unwrap();
                let bad = self.expect_i1(bad_raw, false);
                let fast_bb = self.new_block("div.fast");
                let slow_bb = self.new_block("div.slow");
                let cont_bb = self.new_block("div.cont");
                self.builder
                    .build_conditional_branch(bad, slow_bb, fast_bb)
                    .unwrap();
                self.builder.position_at_end(fast_bb);
                let div_name = self.tmp_name("div");
                let fast = self
                    .builder
                    .build_int_signed_div(lhs, rhs, &div_name)
                    .unwrap();
                self.br_to(cont_bb);
                self.builder.position_at_end(slow_bb);
                let slow = self.emit_i64_binop_slow(lhs, rhs, "spid3r_i64_div")?;
                let slow_def_bb = self.cur_block();
                self.br_to(cont_bb);
                self.builder.position_at_end(cont_bb);
                let merged = self.builder.build_phi(self.i64_ty(), "div.phi").unwrap();
                merged.add_incoming(&[(&fast, fast_bb), (&slow, slow_def_bb.unwrap())]);
                Ok(merged.as_basic_value().into_int_value())
            }
            BinOp::Mod => {
                // `b == 0` raises `modulo by zero`. Rust's `wrapping_rem`
                // yields 0 for `MIN % -1` where LLVM `srem` would be
                // undefined, so divisor `-1` is short-circuited to 0 — the
                // same value the runtime trampoline returns (load-bearing
                // three-engine parity, see plan.html §9).
                //
                // Every edge lands in a FRESH merge block: the slow edge must
                // never fall into `mod.chk` again (that would both re-run the
                // `-1` check and, when no handler is armed to route the
                // pending throw, reach `srem` with a zero divisor — UB).
                let is_zero_raw = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, rhs, self.i64c(0), "modz")
                    .unwrap();
                let is_zero = self.expect_i1(is_zero_raw, false);
                let slow_bb = self.new_block("mod.slow");
                let check_bb = self.new_block("mod.chk");
                let zero_bb = self.new_block("mod.zero");
                let srem_bb = self.new_block("mod.srem");
                let merge_bb = self.new_block("mod.merge");
                self.builder
                    .build_conditional_branch(is_zero, slow_bb, check_bb)
                    .unwrap();
                self.builder.position_at_end(slow_bb);
                let slow = self.emit_i64_binop_slow(lhs, rhs, "spid3r_i64_mod")?;
                let slow_def_bb = self.cur_block();
                self.br_to(merge_bb);
                self.builder.position_at_end(check_bb);
                let is_m1 = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, rhs, self.i64c(-1), "modm1")
                    .unwrap();
                self.builder
                    .build_conditional_branch(is_m1, zero_bb, srem_bb)
                    .unwrap();
                self.builder.position_at_end(zero_bb);
                let zero = self.i64c(0);
                self.br_to(merge_bb);
                self.builder.position_at_end(srem_bb);
                let rem_name = self.tmp_name("mod");
                let rem = self
                    .builder
                    .build_int_signed_rem(lhs, rhs, &rem_name)
                    .unwrap();
                self.br_to(merge_bb);
                self.builder.position_at_end(merge_bb);
                let merged = self.builder.build_phi(self.i64_ty(), "mod.phi").unwrap();
                merged.add_incoming(&[
                    (&slow, slow_def_bb.unwrap()),
                    (&zero, zero_bb),
                    (&rem, srem_bb),
                ]);
                Ok(merged.as_basic_value().into_int_value())
            }
            _ => {
                let tramp_name = match op {
                    BinOp::Add => "spid3r_i64_add",
                    BinOp::Sub => "spid3r_i64_sub",
                    BinOp::Mul => "spid3r_i64_mul",
                    BinOp::Div => "spid3r_i64_div",
                    BinOp::Mod => "spid3r_i64_mod",
                    _ => unreachable!(),
                };
                self.emit_i64_binop_slow(lhs, rhs, tramp_name)
            }
        }
    }

    fn compile_expr_i64(&mut self, e: &Expr) -> IrResult<inkwell::values::IntValue<'ctx>> {
        match e {
            Expr::Int(v, _) => Ok(self.i64c(*v)),
            Expr::Ident(name, _) => match self.lookup(name) {
                Some(VarLoc::LocalInt(slot)) => {
                    let lname = self.tmp_name("iv");
                    let v = self
                        .builder
                        .build_load(self.i64_ty(), slot, &lname)
                        .unwrap()
                        .into_int_value();
                    Ok(v)
                }
                None => {
                    if let Some(&v) = self.top_ints.get(name) {
                        Ok(self.i64c(v))
                    } else {
                        panic!("compile_expr_i64 called on non-int ident {}", name)
                    }
                }
                _ => panic!("compile_expr_i64 called on non-int ident {}", name),
            },
            Expr::Unary {
                op: UnOp::Neg,
                expr,
                ..
            } => {
                let inner = self.compile_expr_i64(expr)?;
                // Phase 3: inline negation; only `i64::MIN` takes the
                // trampoline, so the hot path stays in registers.
                self.emit_i64_neg(inner)
            }
            Expr::Binary { op, l, r, .. } => {
                let a = self.compile_expr_i64(l)?;
                let b = self.compile_expr_i64(r)?;
                self.emit_i64_binop(a, b, *op)
            }
            Expr::Index { object, index, .. } => {
                if let Expr::Ident(name, _) = &**object {
                    match self.lookup(name) {
                        Some(VarLoc::UListI64(slot)) => {
                            let ptr = self
                                .builder
                                .build_load(self.ptr_ty(), slot, "ul")
                                .unwrap()
                                .into_pointer_value();
                            let idx = self.compile_expr_i64(index)?;
                            return self.emit_ulist_i64_get_inline(ptr, idx);
                        }
                        Some(VarLoc::RowViewI64 {
                            data,
                            len,
                            orig_slot,
                            is_ulist,
                        }) => {
                            let idx = self.compile_expr_i64(index)?;
                            let bce_hit = self.is_row_bce_hit(name, index);
                            return self.emit_row_view_i64_get(
                                bce_hit, data, len, orig_slot, is_ulist, idx,
                            );
                        }
                        _ => {}
                    }
                }
                panic!("compile_expr_i64 called on non-int index")
            }
            Expr::Method { object, name, .. } => {
                if let Expr::Ident(vname, _) = &**object {
                    match (self.lookup(vname), name.as_str()) {
                        (Some(VarLoc::UListI64(slot)), "len")
                        | (Some(VarLoc::UListF64(slot)), "len") => {
                            let ptr = self
                                .builder
                                .build_load(self.ptr_ty(), slot, "ul")
                                .unwrap()
                                .into_pointer_value();
                            return Ok(self.emit_ulist_len(ptr));
                        }
                        (Some(VarLoc::RowViewI64 { len, .. }), "len")
                        | (Some(VarLoc::RowViewF64 { len, .. }), "len") => {
                            return Ok(len);
                        }
                        (Some(VarLoc::UListI64(slot)), "pop") => {
                            let ptr = self
                                .builder
                                .build_load(self.ptr_ty(), slot, "ul")
                                .unwrap()
                                .into_pointer_value();
                            let f = self.tramp(
                                "spid3r_ulist_i64_pop",
                                &[self.ptr_ty().into()],
                                Some(self.i64_ty().into()),
                            );
                            let out = call_int(self.emit_call(f, &[ptr.into()], "ul.pop"));
                            self.throw_guard();
                            return Ok(out);
                        }
                        _ if name == "len"
                            && self.str_locals.as_ref().is_some_and(|s| s.contains(vname)) =>
                        {
                            let obj = self.compile_ident(vname)?;
                            return self.emit_v2_len_inline_i64(obj);
                        }
                        _ => {}
                    }
                }
                panic!("compile_expr_i64 called on non-int method")
            }
            Expr::Call {
                callee,
                type_args,
                args,
                ..
            } => {
                if let Expr::Ident(name, _) = &**callee {
                    if type_args.is_empty() {
                        if let Some(val) = self.try_compile_raw_call(name, args)? {
                            return Ok(val.into_int_value());
                        }
                    } else if let Some(val) = self.try_compile_mono_call(name, type_args, args)? {
                        return Ok(val.into_int_value());
                    }
                }
                panic!("compile_expr_i64 called on non-int call")
            }
            _ => panic!("compile_expr_i64 called on non-int expr"),
        }
    }

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
            // Lowering rewrites generators; a `yield` here is a pipeline bug.
            Stmt::Yield { span, .. } => Err(format!(
                "internal error at {}:{}: a `yield` reached the native backend \
                 without lowering (this is a bug in spid3r)",
                span.file, span.line
            )),
            // `crate::lower` expands these before compilation.
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
                let in_entry_root = self.is_entry && self.scopes.len() == 1;
                let ulist_elem = if in_entry_root {
                    None
                } else {
                    self.unboxed_lists
                        .as_ref()
                        .and_then(|m| m.get(name).copied())
                };
                if let Some(elem) = ulist_elem {
                    let new_name = match elem {
                        UnboxedElem::I64 => "spid3r_ulist_i64_new",
                        UnboxedElem::F64 => "spid3r_ulist_f64_new",
                    };
                    let nf = self.tramp(new_name, &[], Some(self.ptr_ty().into()));
                    let ptr = call_ptr(self.builder.build_call(nf, &[], "ul.new").unwrap());
                    if let Some(Expr::List(items, _)) = value {
                        match elem {
                            UnboxedElem::I64 => {
                                let pf = self.tramp(
                                    "spid3r_ulist_i64_push",
                                    &[self.ptr_ty().into(), self.i64_ty().into()],
                                    None,
                                );
                                for it in items {
                                    let iv = if self.is_int_expr(it) {
                                        self.compile_expr_i64(it)?
                                    } else {
                                        let bv = self.compile_expr(it)?;
                                        self.call1p_to_i64("spid3r_to_i64", bv)
                                    };
                                    self.builder
                                        .build_call(pf, &[ptr.into(), iv.into()], "")
                                        .unwrap();
                                }
                            }
                            UnboxedElem::F64 => {
                                let f64_t = self.ctx.f64_type();
                                let pf = self.tramp(
                                    "spid3r_ulist_f64_push",
                                    &[self.ptr_ty().into(), f64_t.into()],
                                    None,
                                );
                                let tof = self.tramp(
                                    "spid3r_to_f64",
                                    &[self.ptr_ty().into()],
                                    Some(f64_t.into()),
                                );
                                for it in items {
                                    let fv = if self.is_float_expr(it) || self.is_int_expr(it) {
                                        self.compile_expr_f64(it)?
                                    } else {
                                        let bv = self.compile_expr(it)?;
                                        let call_r = self
                                            .builder
                                            .build_call(tof, &[bv.into()], "f64")
                                            .unwrap();
                                        match call_r.try_as_basic_value() {
                                            inkwell::values::ValueKind::Basic(v) => {
                                                v.into_float_value()
                                            }
                                            _ => unreachable!(),
                                        }
                                    };
                                    self.builder
                                        .build_call(pf, &[ptr.into(), fv.into()], "")
                                        .unwrap();
                                }
                            }
                        }
                    }
                    let slot = self.alloca_entry_ty(self.ptr_ty(), "ul");
                    self.builder.build_store(slot, ptr).unwrap();
                    let loc = match elem {
                        UnboxedElem::I64 => VarLoc::UListI64(slot),
                        UnboxedElem::F64 => VarLoc::UListF64(slot),
                    };
                    self.declare(name, loc);
                    return Ok(());
                }
                let is_int = !in_entry_root
                    && self
                        .int_locals
                        .as_ref()
                        .map(|s| s.contains(name))
                        .unwrap_or(false);
                let is_float = !in_entry_root
                    && !is_int
                    && self
                        .float_locals
                        .as_ref()
                        .map(|s| s.contains(name))
                        .unwrap_or(false);
                if is_int {
                    let iv = match value {
                        Some(e) => self.compile_expr_i64(e)?,
                        None => self.i64c(0),
                    };
                    let slot = self.alloca_entry_ty(self.i64_ty(), "li");
                    self.builder.build_store(slot, iv).unwrap();
                    self.declare(name, VarLoc::LocalInt(slot));
                    Ok(())
                } else if is_float {
                    let fv = match value {
                        Some(e) => self.compile_expr_f64(e)?,
                        None => self.ctx.f64_type().const_float(0.0),
                    };
                    let slot = self.alloca_entry_ty(self.ctx.f64_type(), "lf");
                    self.builder.build_store(slot, fv).unwrap();
                    self.declare(name, VarLoc::LocalFloat(slot));
                    Ok(())
                } else {
                    let v = match value {
                        Some(e) => self.compile_expr(e)?,
                        None => self.v_null(),
                    };
                    self.bind_declaration(name, v)?;
                    Ok(())
                }
            }
            Stmt::Const { name, value, .. } => {
                let in_entry_root = self.is_entry && self.scopes.len() == 1;
                let is_int = !in_entry_root
                    && self
                        .int_locals
                        .as_ref()
                        .map(|s| s.contains(name))
                        .unwrap_or(false);
                let is_float = !in_entry_root
                    && !is_int
                    && self
                        .float_locals
                        .as_ref()
                        .map(|s| s.contains(name))
                        .unwrap_or(false);
                if is_int {
                    let iv = self.compile_expr_i64(value)?;
                    let slot = self.alloca_entry_ty(self.i64_ty(), "ci");
                    self.builder.build_store(slot, iv).unwrap();
                    self.declare(name, VarLoc::LocalInt(slot));
                    Ok(())
                } else if is_float {
                    let fv = self.compile_expr_f64(value)?;
                    let slot = self.alloca_entry_ty(self.ctx.f64_type(), "cf");
                    self.builder.build_store(slot, fv).unwrap();
                    self.declare(name, VarLoc::LocalFloat(slot));
                    Ok(())
                } else {
                    let v = self.compile_expr(value)?;
                    self.bind_declaration(name, v)?;
                    Ok(())
                }
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
                if let Some(plan) = self.analyze_hoisted_while_loop(cond, &body.stmts) {
                    self.emit_hoisted_while_loop(cond, body, plan)
                } else {
                    self.emit_while_loop_plain(cond, body)
                }
            }
            Stmt::ForIn {
                var,
                var2,
                iter,
                body,
                ..
            } => {
                if var2.is_none() {
                    if let Expr::Ident(list_name, _) = iter {
                        if let Some(
                            ul_loc @ (VarLoc::UListI64(ul_slot) | VarLoc::UListF64(ul_slot)),
                        ) = self.lookup(list_name)
                        {
                            if !ulist_mutated_in_stmts(&body.stmts, list_name) {
                                let ul_ptr = self
                                    .builder
                                    .build_load(self.ptr_ty(), ul_slot, "fi.ul")
                                    .unwrap()
                                    .into_pointer_value();
                                let len = self.emit_ulist_len(ul_ptr);
                                let idx_slot = self.alloca_entry_ty(self.i64_ty(), "fi.idx");
                                self.builder.build_store(idx_slot, self.i64c(0)).unwrap();

                                let head_bb = self.new_block("forin.head");
                                let body_bb = self.new_block("forin.body");
                                let step_bb = self.new_block("forin.step");
                                let end_bb = self.new_block("forin.end");
                                self.br_to(head_bb);

                                self.builder.position_at_end(head_bb);
                                let idx = self
                                    .builder
                                    .build_load(self.i64_ty(), idx_slot, "fi.i")
                                    .unwrap()
                                    .into_int_value();
                                let has_more = self
                                    .builder
                                    .build_int_compare(IntPredicate::SLT, idx, len, "fi.more")
                                    .unwrap();
                                self.builder
                                    .build_conditional_branch(has_more, body_bb, end_bb)
                                    .unwrap();

                                self.builder.position_at_end(body_bb);
                                let counter = self.alloca_entry_ty(self.i64_ty(), "cancel.counter");
                                self.init_entry_slot(counter, self.i64c(0));
                                self.emit_cancel_poll(counter);

                                let forin_boxed = self
                                    .boxed
                                    .as_ref()
                                    .map(|b| b.contains(var))
                                    .unwrap_or(false);
                                let var_assigned =
                                    stmt_mentions_assign_target(&Stmt::Block(body.clone()), var);
                                self.push_scope();
                                match ul_loc {
                                    VarLoc::UListI64(_) => {
                                        let iv = self.emit_ulist_i64_get_inline(ul_ptr, idx)?;
                                        if !forin_boxed && !var_assigned {
                                            let vslot =
                                                self.alloca_entry_ty(self.i64_ty(), "fi.vi");
                                            self.builder.build_store(vslot, iv).unwrap();
                                            self.declare(var, VarLoc::LocalInt(vslot));
                                        } else {
                                            let bv = self.v_int_from_i64(iv);
                                            if forin_boxed {
                                                let cell = self.call1p("spid3r_cell_new", bv);
                                                let cslot = self.alloca_entry("forin.cell");
                                                self.builder.build_store(cslot, cell).unwrap();
                                                self.declare(var, VarLoc::Cell(cslot));
                                            } else {
                                                let lslot = self.temp_slot(bv);
                                                self.declare(var, VarLoc::Local(lslot));
                                            }
                                        }
                                    }
                                    VarLoc::UListF64(_) => {
                                        let fv = self.emit_ulist_f64_get_inline(ul_ptr, idx)?;
                                        if !forin_boxed && !var_assigned {
                                            let vslot =
                                                self.alloca_entry_ty(self.ctx.f64_type(), "fi.vf");
                                            self.builder.build_store(vslot, fv).unwrap();
                                            self.declare(var, VarLoc::LocalFloat(vslot));
                                        } else {
                                            let bv = self.v_float_from_f64(fv);
                                            if forin_boxed {
                                                let cell = self.call1p("spid3r_cell_new", bv);
                                                let cslot = self.alloca_entry("forin.cell");
                                                self.builder.build_store(cslot, cell).unwrap();
                                                self.declare(var, VarLoc::Cell(cslot));
                                            } else {
                                                let lslot = self.temp_slot(bv);
                                                self.declare(var, VarLoc::Local(lslot));
                                            }
                                        }
                                    }
                                    _ => unreachable!(),
                                }
                                self.loops.push(LoopFrame {
                                    break_bb: end_bb,
                                    continue_bb: step_bb,
                                });
                                self.compile_block(&body.stmts)?;
                                self.loops.pop();
                                self.pop_scope();
                                self.br_to(step_bb);

                                self.builder.position_at_end(step_bb);
                                let cur_i = self
                                    .builder
                                    .build_load(self.i64_ty(), idx_slot, "fi.ci")
                                    .unwrap()
                                    .into_int_value();
                                let next_i = self
                                    .builder
                                    .build_int_add(cur_i, self.i64c(1), "fi.ni")
                                    .unwrap();
                                self.builder.build_store(idx_slot, next_i).unwrap();
                                self.br_to(head_bb);

                                self.builder.position_at_end(end_bb);
                                return Ok(());
                            }
                        }
                    }
                }
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
                let next = self.alloc_v2_slot();
                let f = self.tramp(
                    "spid3r_forin_next",
                    &[self.ptr_ty().into(), self.ptr_ty().into()],
                    Some(self.i8_ty().into()),
                );
                let has_next = call_int(self.emit_call(f, &[next.into(), it.into()], "forin.has"));
                let is_end = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, has_next, self.i8c(0), "forin.done")
                    .unwrap();
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
                let counter = self.alloca_entry_ty(self.i64_ty(), "cancel.counter");
                self.init_entry_slot(counter, self.i64c(0));
                self.emit_cancel_poll(counter);
                self.loops.push(LoopFrame {
                    break_bb: end_bb,
                    continue_bb: post_bb,
                });
                self.compile_block(&body.stmts)?;
                self.loops.pop();
                self.br_to(post_bb);
                self.builder.position_at_end(post_bb);
                if let Expr::Assign {
                    target, op, value, ..
                } = post
                {
                    if let Expr::Ident(name, _) = target.as_ref() {
                        if let Some(VarLoc::LocalInt(slot)) = self.lookup(name) {
                            if *op == AssignOp::Assign {
                                let iv = self.compile_expr_i64(value)?;
                                self.builder.build_store(slot, iv).unwrap();
                            } else {
                                let bin = match op {
                                    AssignOp::AddAssign => BinOp::Add,
                                    AssignOp::SubAssign => BinOp::Sub,
                                    AssignOp::MulAssign => BinOp::Mul,
                                    AssignOp::DivAssign => BinOp::Div,
                                    AssignOp::ModAssign => BinOp::Mod,
                                    AssignOp::Assign => unreachable!(),
                                };
                                let cur = self
                                    .builder
                                    .build_load(self.i64_ty(), slot, "cur")
                                    .unwrap()
                                    .into_int_value();
                                let rhs = self.compile_expr_i64(value)?;
                                let res = self.emit_i64_binop(cur, rhs, bin)?;
                                self.builder.build_store(slot, res).unwrap();
                            }
                            self.br_to(cond_bb);
                            self.builder.position_at_end(end_bb);
                            return Ok(());
                        } else if let Some(VarLoc::LocalFloat(slot)) = self.lookup(name) {
                            let res = if *op == AssignOp::Assign {
                                self.compile_expr_f64(value)?
                            } else {
                                let bin = match op {
                                    AssignOp::AddAssign => BinOp::Add,
                                    AssignOp::SubAssign => BinOp::Sub,
                                    AssignOp::MulAssign => BinOp::Mul,
                                    AssignOp::DivAssign => BinOp::Div,
                                    _ => unreachable!(),
                                };
                                let cur = self
                                    .builder
                                    .build_load(self.ctx.f64_type(), slot, "fcur")
                                    .unwrap()
                                    .into_float_value();
                                let rhs = self.compile_expr_f64(value)?;
                                self.emit_f64_binop(cur, rhs, bin)?
                            };
                            self.builder.build_store(slot, res).unwrap();
                            self.br_to(cond_bb);
                            self.builder.position_at_end(end_bb);
                            return Ok(());
                        }
                    }
                }
                self.compile_expr(post)?;
                self.br_to(cond_bb);
                self.builder.position_at_end(end_bb);
                Ok(())
            }
            Stmt::Return { value, .. } => {
                if !self.finally_stack.is_empty() {
                    let v = match value {
                        Some(e) => self.compile_expr(e)?,
                        None => self.v_null(),
                    };
                    // Route through `finally` so it runs before the function
                    // returns (mirrors the tree/VM `finally` semantics).
                    self.route_to_finally(1, Some(v));
                    return Ok(());
                }
                if let Some(ts) = self.mono_ret_threw_slot {
                    self.builder.build_store(ts, self.i8c(0)).unwrap();
                }
                if let Some(slot) = self.mono_ret_int_slot {
                    let iv = match value {
                        Some(e) if self.is_int_expr(e) => self.compile_expr_i64(e)?,
                        Some(e) => {
                            let bv = self.compile_expr(e)?;
                            self.call1p_to_i64("spid3r_to_i64", bv)
                        }
                        None => self.i64c(0),
                    };
                    self.builder.build_store(slot, iv).unwrap();
                    self.br_to(self.ret_bb.expect("ret_bb"));
                    return Ok(());
                }
                if let Some(slot) = self.mono_ret_float_slot {
                    let f64_t = self.ctx.f64_type();
                    let fv = match value {
                        Some(e) if self.is_float_expr(e) || self.is_int_expr(e) => {
                            self.compile_expr_f64(e)?
                        }
                        Some(e) => {
                            let bv = self.compile_expr(e)?;
                            let tof = self.tramp(
                                "spid3r_to_f64",
                                &[self.ptr_ty().into()],
                                Some(f64_t.into()),
                            );
                            let call_r =
                                self.builder.build_call(tof, &[bv.into()], "ret.f").unwrap();
                            match call_r.try_as_basic_value() {
                                inkwell::values::ValueKind::Basic(v) => v.into_float_value(),
                                _ => unreachable!(),
                            }
                        }
                        None => f64_t.const_float(0.0),
                    };
                    self.builder.build_store(slot, fv).unwrap();
                    self.br_to(self.ret_bb.expect("ret_bb"));
                    return Ok(());
                }
                if let Some(slot) = self.mono_ret_bool_slot {
                    let bi8 = match value {
                        Some(Expr::Bool(b, _)) => self.i8c(u8::from(*b)),
                        Some(e) => {
                            let c = self.compile_cond_to_i1(e)?;
                            self.builder
                                .build_int_z_extend(c, self.i8_ty(), "bi8")
                                .unwrap()
                        }
                        None => self.i8c(0),
                    };
                    self.builder.build_store(slot, bi8).unwrap();
                    self.br_to(self.ret_bb.expect("ret_bb"));
                    return Ok(());
                }
                let v = match value {
                    Some(e) => self.compile_expr(e)?,
                    None => self.v_null(),
                };
                self.store_return(v);
                Ok(())
            }
            Stmt::Break { .. } => {
                if self.loops.is_empty() {
                    return Err(self.err("'break' outside of a loop"));
                }
                self.emit_loop_exit(true);
                Ok(())
            }
            Stmt::Continue { .. } => {
                if self.loops.is_empty() {
                    return Err(self.err("'continue' outside of a loop"));
                }
                self.emit_loop_exit(false);
                Ok(())
            }
            Stmt::Block(b) => self.compile_block(&b.stmts),
            Stmt::Expr(e) => {
                if let Expr::Assign {
                    target, op, value, ..
                } = e
                {
                    if let Expr::Ident(name, _) = target.as_ref() {
                        if let Some(VarLoc::LocalInt(slot)) = self.lookup(name) {
                            if *op == AssignOp::Assign {
                                let iv = self.compile_expr_i64(value)?;
                                self.builder.build_store(slot, iv).unwrap();
                                return Ok(());
                            } else {
                                let bin = match op {
                                    AssignOp::AddAssign => BinOp::Add,
                                    AssignOp::SubAssign => BinOp::Sub,
                                    AssignOp::MulAssign => BinOp::Mul,
                                    AssignOp::DivAssign => BinOp::Div,
                                    AssignOp::ModAssign => BinOp::Mod,
                                    AssignOp::Assign => unreachable!(),
                                };
                                let cur = self
                                    .builder
                                    .build_load(self.i64_ty(), slot, "cur")
                                    .unwrap()
                                    .into_int_value();
                                let rhs = self.compile_expr_i64(value)?;
                                let res = self.emit_i64_binop(cur, rhs, bin)?;
                                self.builder.build_store(slot, res).unwrap();
                                return Ok(());
                            }
                        } else if let Some(VarLoc::LocalFloat(slot)) = self.lookup(name) {
                            let res = if *op == AssignOp::Assign {
                                self.compile_expr_f64(value)?
                            } else {
                                let bin = match op {
                                    AssignOp::AddAssign => BinOp::Add,
                                    AssignOp::SubAssign => BinOp::Sub,
                                    AssignOp::MulAssign => BinOp::Mul,
                                    AssignOp::DivAssign => BinOp::Div,
                                    _ => unreachable!(),
                                };
                                let cur = self
                                    .builder
                                    .build_load(self.ctx.f64_type(), slot, "fcur")
                                    .unwrap()
                                    .into_float_value();
                                let rhs = self.compile_expr_f64(value)?;
                                self.emit_f64_binop(cur, rhs, bin)?
                            };
                            self.builder.build_store(slot, res).unwrap();
                            return Ok(());
                        }
                    }
                }
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
            let pr = self.alloc_v2_slot();
            let pt = self.alloc_v2_slot();
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
                loop_depth: self.loops.len(),
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
                    loop_depth: self.loops.len(),
                });
                self.exc.push(catch_fh);
                self.push_scope();
                let slot = self.alloc_v2_slot();
                self.copy_v2(slot, thrown);
                self.declare(&cc.name, VarLoc::Local(slot));
                self.compile_block(&cc.body.stmts)?;
                self.pop_scope();
                self.exc.pop();
                self.finally_stack.pop();
                self.br_to(finally_entry.unwrap()); // catch handled it (pending 0)
                                                    // the catch itself threw: re-throw after finally
                self.builder.position_at_end(catch_fh);
                let nt = self.take_thrown();
                self.copy_v2(pend_thrown.unwrap(), nt);
                self.builder
                    .build_store(pend_exit.unwrap(), self.i32c(4))
                    .unwrap();
                self.br_to(finally_entry.unwrap());
            } else {
                self.push_scope();
                let slot = self.alloc_v2_slot();
                self.copy_v2(slot, thrown);
                self.declare(&cc.name, VarLoc::Local(slot));
                self.compile_block(&cc.body.stmts)?;
                self.pop_scope();
                self.br_to(after_bb);
            }
        } else if has_finally {
            // no catch: run `finally`, then re-throw the pending value.
            let thrown = self.take_thrown();
            self.copy_v2(pend_thrown.unwrap(), thrown);
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

            // return -> store the pending value and return from the function,
            // first running any enclosing `finally` blocks (this try's frame
            // is already popped, so `finally_stack` holds only outer ones).
            self.builder.position_at_end(bb_ret);
            let v = pend_ret.unwrap();
            if self.finally_stack.is_empty() {
                self.store_return(v);
            } else {
                self.route_to_finally(1, Some(v));
            }

            // break / continue -> the loop enclosing this try, chaining through
            // any outer `finally` that the jump also crosses. These arms are
            // only reachable when such a loop exists (a `break` targeting a
            // loop inside the body never routes here), so `unreachable` is a
            // dead arm when there is none.
            self.builder.position_at_end(bb_brk);
            if self.loops.is_empty() {
                self.builder.build_unreachable().unwrap();
            } else {
                self.emit_loop_exit(true);
            }

            self.builder.position_at_end(bb_cont);
            if self.loops.is_empty() {
                self.builder.build_unreachable().unwrap();
            } else {
                self.emit_loop_exit(false);
            }

            // re-throw the pending value to the enclosing handler
            self.builder.position_at_end(bb_re);
            let v = pend_thrown.unwrap();
            self.throw_value(v); // (re)set the throw flag
            self.br_to(outer_handler); // route to the enclosing handler
        }

        self.builder.position_at_end(after_bb);
        Ok(())
    }

    /// Emit a `break` (`is_break`) or `continue` targeting the innermost loop.
    /// If the innermost active `try`/`finally` lies between here and that loop
    /// (the loop was entered before the try), the jump is routed through the
    /// `finally` dispatch, which chains outward; otherwise it branches
    /// directly. Callers guarantee `self.loops` is non-empty.
    fn emit_loop_exit(&mut self, is_break: bool) {
        let crosses_finally = self
            .finally_stack
            .last()
            .is_some_and(|ctx| self.loops.len() <= ctx.loop_depth);
        if crosses_finally {
            self.route_to_finally(if is_break { 2 } else { 3 }, None);
            return;
        }
        let frame = self.loops.last().expect("loop exit with no active loop");
        let bb = if is_break {
            frame.break_bb
        } else {
            frame.continue_bb
        };
        self.br_to(bb);
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
            self.copy_v2(slot, v);
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
        let is_raw = self
            .builder
            .build_int_compare(IntPredicate::NE, b, self.i8c(0), &cname)
            .unwrap();
        let is = self.expect_i1(is_raw, false);
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

    /// Branch to the active exception handler if an unboxed raw call returned
    /// `threw == true`, without calling `@spid3r_threw()`.
    fn throw_guard_i1(&mut self, threw: inkwell::values::IntValue<'ctx>) {
        let Some(handler) = self.exc.last().copied() else {
            return;
        };
        let is = self.expect_i1(threw, false);
        let cont = self.new_block("raw.ok");
        if let Some(cur) = self.cur_block() {
            if cur.get_terminator().is_none() {
                self.builder
                    .build_conditional_branch(is, handler, cont)
                    .unwrap();
                self.builder.position_at_end(cont);
            }
        }
    }

    /// Branch to the active exception handler if an `extern "C"` call returned
    /// a non-zero `i8` throw flag, avoiding a separate `@spid3r_threw()` FFI call.
    fn throw_guard_i8_call(&mut self, call_r: inkwell::values::CallSiteValue<'ctx>) {
        self.expr_guarded = true;
        if self.exc.is_empty() {
            return;
        }
        let b = call_int(call_r);
        let cname = self.tmp_name("exc");
        let is_raw = self
            .builder
            .build_int_compare(IntPredicate::NE, b, self.i8c(0), &cname)
            .unwrap();
        self.throw_guard_i1(is_raw);
    }

    /// Fetch (and clear) the pending thrown value from the runtime.
    fn take_thrown(&mut self) -> PointerValue<'ctx> {
        self.call0p("spid3r_take_thrown")
    }

    /// A temporary stack slot holding `v` (for loop variables etc.).
    fn temp_slot(&mut self, v: PointerValue<'ctx>) -> PointerValue<'ctx> {
        let slot = self.alloc_v2_slot();
        self.copy_v2(slot, v);
        slot
    }

    fn store_return(&mut self, v: PointerValue<'ctx>) {
        if let Some(ts) = self.mono_ret_threw_slot {
            self.builder.build_store(ts, self.i8c(0)).unwrap();
        }
        if let Some(rs) = self.mono_ret_int_slot {
            let iv = self.call1p_to_i64("spid3r_to_i64", v);
            self.builder.build_store(rs, iv).unwrap();
        } else if let Some(rs) = self.mono_ret_float_slot {
            let f64_t = self.ctx.f64_type();
            let tof = self.tramp("spid3r_to_f64", &[self.ptr_ty().into()], Some(f64_t.into()));
            let call_r = self.builder.build_call(tof, &[v.into()], "ret.f").unwrap();
            let fv = match call_r.try_as_basic_value() {
                inkwell::values::ValueKind::Basic(fv) => fv.into_float_value(),
                _ => unreachable!(),
            };
            self.builder.build_store(rs, fv).unwrap();
        } else if let Some(rs) = self.mono_ret_bool_slot {
            let tf = self.tramp(
                "spid3r_truthy",
                &[self.ptr_ty().into()],
                Some(self.i8_ty().into()),
            );
            let b = call_int(self.emit_call(tf, &[v.into()], "ret.b"));
            let bi1 = self
                .builder
                .build_int_compare(IntPredicate::NE, b, self.i8c(0), "ret.bi1")
                .unwrap();
            let bi8 = self
                .builder
                .build_int_z_extend(bi1, self.i8_ty(), "ret.bi8")
                .unwrap();
            self.builder.build_store(rs, bi8).unwrap();
        } else {
            let slot = *self.ret_slots.last().expect("return outside of a function");
            self.copy_v2(slot, v);
        }
        let bb = self.ret_bb.expect("return outside of a function");
        self.br_to(bb);
    }

    fn compile_cond_to_i1(&mut self, cond: &Expr) -> IrResult<inkwell::values::IntValue<'ctx>> {
        if let Expr::Unary {
            op: UnOp::Not,
            expr,
            ..
        } = cond
        {
            if self.is_bool_expr(expr) {
                let inner = self.compile_cond_to_i1(expr)?;
                return Ok(self.builder.build_not(inner, "notc").unwrap());
            }
        }
        if let Some((op, l, r)) = self.is_int_compare(cond) {
            let predicate = match op {
                BinOp::Lt => IntPredicate::SLT,
                BinOp::Le => IntPredicate::SLE,
                BinOp::Gt => IntPredicate::SGT,
                BinOp::Ge => IntPredicate::SGE,
                BinOp::Eq => IntPredicate::EQ,
                BinOp::Ne => IntPredicate::NE,
                _ => unreachable!(),
            };
            let lhs = self.compile_expr_i64(l)?;
            let rhs = self.compile_expr_i64(r)?;
            return Ok(self
                .builder
                .build_int_compare(predicate, lhs, rhs, "cmp")
                .unwrap());
        }
        if let Some((op, l, r)) = self.is_float_compare(cond) {
            let predicate = match op {
                BinOp::Lt => FloatPredicate::OLT,
                BinOp::Le => FloatPredicate::OLE,
                BinOp::Gt => FloatPredicate::OGT,
                BinOp::Ge => FloatPredicate::OGE,
                BinOp::Eq => FloatPredicate::OEQ,
                BinOp::Ne => FloatPredicate::ONE,
                _ => unreachable!(),
            };
            let lhs = self.compile_expr_f64(l)?;
            let rhs = self.compile_expr_f64(r)?;
            return Ok(self
                .builder
                .build_float_compare(predicate, lhs, rhs, "fcmp")
                .unwrap());
        }
        if let Expr::Call {
            callee,
            type_args,
            args,
            ..
        } = cond
        {
            if type_args.is_empty() {
                if let Expr::Ident(name, _) = &**callee {
                    if self.can_direct_raw_call(name, args) == Some("bool") {
                        if let Some(val) = self.try_compile_raw_call(name, args)? {
                            let b8 = val.into_int_value();
                            return Ok(self
                                .builder
                                .build_int_compare(IntPredicate::NE, b8, self.i8c(0), "rawb")
                                .unwrap());
                        }
                    }
                }
            }
        }
        let v = self.compile_expr(cond)?;
        Ok(self.cond_i1(v))
    }

    fn emit_cond_branch(
        &mut self,
        cond: &Expr,
        then_bb: BasicBlock<'ctx>,
        else_bb: BasicBlock<'ctx>,
    ) -> IrResult<()> {
        if self.is_int_compare(cond).is_none() && self.is_float_compare(cond).is_none() {
            if let Some((cmp_op, left, right)) = self.is_hybrid_int_compare(cond) {
                return self
                    .emit_hybrid_int_compare_cond(cmp_op, left, right, cond, then_bb, else_bb);
            }
        }
        let c = self.compile_cond_to_i1(cond)?;
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
            let slot = self.alloc_v2_slot();
            self.copy_v2(slot, v);
            self.declare(name, VarLoc::Local(slot));
        }
        Ok(())
    }
}

impl<'ctx> Ir<'ctx> {
    #[inline]
    fn expr_can_throw(e: &Expr) -> bool {
        !matches!(
            e,
            Expr::Int(..)
                | Expr::Float(..)
                | Expr::Bool(..)
                | Expr::Null(..)
                | Expr::Str(..)
                | Expr::Ident(..)
        )
    }

    fn is_unshadowed_builtin(&self, name: &str) -> bool {
        self.lookup(name).is_none() && !self.shadowed_builtins.contains(name)
    }

    fn target_is_little_endian(&self) -> bool {
        let Some(ref triple) = self.target_triple else {
            return cfg!(target_endian = "little");
        };
        let arch = triple.split('-').next().unwrap_or("");
        !matches!(
            arch,
            "sparc" | "sparcv9" | "mips" | "mips64" | "powerpc" | "powerpc64" | "s390x" | "m68k"
        )
    }

    fn emit_v2_len_probe(
        &mut self,
        obj: PointerValue<'ctx>,
        want_i64: bool,
    ) -> IrResult<(PointerValue<'ctx>, Option<inkwell::values::IntValue<'ctx>>)> {
        if !self.target_is_little_endian() {
            let out = self.call1p("spid3r_list_len", obj);
            self.throw_guard();
            if want_i64 {
                return Ok((out, Some(self.call1p_to_i64("spid3r_to_i64", out))));
            }
            self.expr_guarded = true;
            return Ok((out, None));
        }

        let v2_ty = self.val_v2_ty();
        let out = self.alloc_v2_slot();
        let tag_p = unsafe {
            self.builder
                .build_in_bounds_gep(v2_ty, obj, &[self.i64c(0), self.i32c(0)], "len.tp")
                .unwrap()
        };
        let tag = self
            .builder
            .build_load(self.i64_ty(), tag_p, "len.tag")
            .unwrap()
            .into_int_value();
        let pay_p = unsafe {
            self.builder
                .build_in_bounds_gep(v2_ty, obj, &[self.i64c(0), self.i32c(1)], "len.pp")
                .unwrap()
        };
        let pay = self
            .builder
            .build_load(self.i64_ty(), pay_p, "len.pay")
            .unwrap()
            .into_int_value();

        let is_ra_raw = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                self.i64c(crate::value::ValueV2::TAG_ROPE_ASCII as i64),
                "len.ra",
            )
            .unwrap();
        let ra_bb = self.new_block("len.ra");
        let chk_sso_bb = self.new_block("len.chksso");
        let sso_bb = self.new_block("len.sso");
        let slow_bb = self.new_block("len.slow");
        let cont_bb = self.new_block("len.cont");

        self.builder
            .build_conditional_branch(is_ra_raw, ra_bb, chk_sso_bb)
            .unwrap();

        self.builder.position_at_end(ra_bb);
        let ra_len = self
            .builder
            .build_and(pay, self.i64c(0xFFFF_FFFF), "len.rav")
            .unwrap();
        if !want_i64 {
            self.store_v2_inline(out, crate::value::ValueV2::TAG_INT, ra_len);
        }
        let ra_from = self.cur_block().unwrap();
        self.br_to(cont_bb);

        self.builder.position_at_end(chk_sso_bb);
        let lo = self
            .builder
            .build_and(tag, self.i64c(0xFF), "len.lo")
            .unwrap();
        let sso_len = self
            .builder
            .build_int_sub(
                lo,
                self.i64c(crate::value::ValueV2::TAG_SSO_BASE as i64),
                "len.ssov",
            )
            .unwrap();
        let in_range = self
            .builder
            .build_int_compare(IntPredicate::ULE, sso_len, self.i64c(15), "len.in15")
            .unwrap();
        let t_hi = self
            .builder
            .build_and(tag, self.i64c(0x8080_8080_8080_8000u64 as i64), "len.thi")
            .unwrap();
        let p_hi = self
            .builder
            .build_and(pay, self.i64c(0x8080_8080_8080_8080u64 as i64), "len.phi")
            .unwrap();
        let hi_or = self.builder.build_or(t_hi, p_hi, "len.hior").unwrap();
        let is_ascii = self
            .builder
            .build_int_compare(IntPredicate::EQ, hi_or, self.i64c(0), "len.asc")
            .unwrap();
        let sso_ok = self
            .builder
            .build_and(in_range, is_ascii, "len.ssook")
            .unwrap();
        self.builder
            .build_conditional_branch(sso_ok, sso_bb, slow_bb)
            .unwrap();

        self.builder.position_at_end(sso_bb);
        if !want_i64 {
            self.store_v2_inline(out, crate::value::ValueV2::TAG_INT, sso_len);
        }
        let sso_from = self.cur_block().unwrap();
        self.br_to(cont_bb);

        self.builder.position_at_end(slow_bb);
        let f_len = self.tramp(
            "spid3r_list_len",
            &[self.ptr_ty().into(), self.ptr_ty().into()],
            None,
        );
        self.builder
            .build_call(f_len, &[out.into(), obj.into()], "")
            .unwrap();
        self.throw_guard();
        let slow_i64 = if want_i64 {
            Some(self.call1p_to_i64("spid3r_to_i64", out))
        } else {
            None
        };
        let slow_from = self.cur_block().unwrap();
        self.br_to(cont_bb);

        self.builder.position_at_end(cont_bb);
        if let Some(sv) = slow_i64 {
            let phi = self.builder.build_phi(self.i64_ty(), "len.phi").unwrap();
            phi.add_incoming(&[(&ra_len, ra_from), (&sso_len, sso_from), (&sv, slow_from)]);
            Ok((out, Some(phi.as_basic_value().into_int_value())))
        } else {
            self.expr_guarded = true;
            Ok((out, None))
        }
    }

    fn emit_v2_len_inline_i64(
        &mut self,
        obj: PointerValue<'ctx>,
    ) -> IrResult<inkwell::values::IntValue<'ctx>> {
        let (_, iv) = self.emit_v2_len_probe(obj, true)?;
        Ok(iv.expect("emit_v2_len_probe(true) returns i64"))
    }

    fn emit_v2_len_inline(&mut self, obj: PointerValue<'ctx>) -> IrResult<PointerValue<'ctx>> {
        let (out, _) = self.emit_v2_len_probe(obj, false)?;
        Ok(out)
    }

    /// Phase 2: inline scalar `ValueV2` fast path for int `Add`/`Sub`/`Mul` and
    /// comparisons; falls back on cold path to `spid3r_binop` + `throw_guard`.
    fn emit_v2_binop_inline(
        &mut self,
        a: PointerValue<'ctx>,
        b: PointerValue<'ctx>,
        op: BinOp,
    ) -> IrResult<PointerValue<'ctx>> {
        let v2_ty = self.val_v2_ty();
        let out = self.alloc_v2_slot();

        let lt_p = unsafe {
            self.builder
                .build_in_bounds_gep(v2_ty, a, &[self.i64c(0), self.i32c(0)], "v2b.ltp")
                .unwrap()
        };
        let rt_p = unsafe {
            self.builder
                .build_in_bounds_gep(v2_ty, b, &[self.i64c(0), self.i32c(0)], "v2b.rtp")
                .unwrap()
        };
        let l_tag = self
            .builder
            .build_load(self.i64_ty(), lt_p, "v2b.lt")
            .unwrap()
            .into_int_value();
        let r_tag = self
            .builder
            .build_load(self.i64_ty(), rt_p, "v2b.rt")
            .unwrap()
            .into_int_value();
        let int_tag = self.i64c(crate::value::ValueV2::TAG_INT as i64);
        let l_is_int = self
            .builder
            .build_int_compare(IntPredicate::EQ, l_tag, int_tag, "v2b.li")
            .unwrap();
        let r_is_int = self
            .builder
            .build_int_compare(IntPredicate::EQ, r_tag, int_tag, "v2b.ri")
            .unwrap();
        let both_raw = self
            .builder
            .build_and(l_is_int, r_is_int, "v2b.bi")
            .unwrap();
        let both = self.expect_i1(both_raw, true);

        let fast_bb = self.new_block("v2b.fast");
        let slow_bb = self.new_block("v2b.slow");
        let cont_bb = self.new_block("v2b.cont");
        self.builder
            .build_conditional_branch(both, fast_bb, slow_bb)
            .unwrap();

        self.builder.position_at_end(fast_bb);
        let lp_p = unsafe {
            self.builder
                .build_in_bounds_gep(v2_ty, a, &[self.i64c(0), self.i32c(1)], "v2b.lpp")
                .unwrap()
        };
        let rp_p = unsafe {
            self.builder
                .build_in_bounds_gep(v2_ty, b, &[self.i64c(0), self.i32c(1)], "v2b.rpp")
                .unwrap()
        };
        let lhs = self
            .builder
            .build_load(self.i64_ty(), lp_p, "v2b.lp")
            .unwrap()
            .into_int_value();
        let rhs = self
            .builder
            .build_load(self.i64_ty(), rp_p, "v2b.rp")
            .unwrap()
            .into_int_value();
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                let intr = match op {
                    BinOp::Add => "llvm.sadd.with.overflow.i64",
                    BinOp::Sub => "llvm.ssub.with.overflow.i64",
                    BinOp::Mul => "llvm.smul.with.overflow.i64",
                    _ => unreachable!(),
                };
                let ovf_f = self.overflow_intrinsic(intr);
                let cname = self.tmp_name("v2ovf");
                let call = self
                    .builder
                    .build_call(ovf_f, &[lhs.into(), rhs.into()], &cname)
                    .unwrap();
                let pair = match call.try_as_basic_value() {
                    inkwell::values::ValueKind::Basic(v) => v.into_struct_value(),
                    _ => panic!("spid3r: overflow intrinsic did not return a struct"),
                };
                let res = self
                    .builder
                    .build_extract_value(pair, 0, "v2b.res")
                    .unwrap()
                    .into_int_value();
                let ovf_raw = self
                    .builder
                    .build_extract_value(pair, 1, "v2b.ovf")
                    .unwrap()
                    .into_int_value();
                let ovf = self.expect_i1(ovf_raw, false);
                let no_ovf_bb = self.new_block("v2b.ok");
                self.builder
                    .build_conditional_branch(ovf, slow_bb, no_ovf_bb)
                    .unwrap();
                self.builder.position_at_end(no_ovf_bb);
                self.store_v2_inline(out, crate::value::ValueV2::TAG_INT, res);
                self.br_to(cont_bb);
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let pred = match op {
                    BinOp::Eq => IntPredicate::EQ,
                    BinOp::Ne => IntPredicate::NE,
                    BinOp::Lt => IntPredicate::SLT,
                    BinOp::Le => IntPredicate::SLE,
                    BinOp::Gt => IntPredicate::SGT,
                    BinOp::Ge => IntPredicate::SGE,
                    _ => unreachable!(),
                };
                let cmp_i1 = self
                    .builder
                    .build_int_compare(pred, lhs, rhs, "v2b.cmp")
                    .unwrap();
                let cmp_i64 = self
                    .builder
                    .build_int_z_extend(cmp_i1, self.i64_ty(), "v2b.b64")
                    .unwrap();
                self.store_v2_inline(out, crate::value::ValueV2::TAG_BOOL, cmp_i64);
                self.br_to(cont_bb);
            }
            _ => unreachable!("emit_v2_binop_inline called on unsupported BinOp"),
        }

        self.builder.position_at_end(slow_bb);
        let f_slow = self.tramp(
            "spid3r_binop",
            &[
                self.ptr_ty().into(),
                self.ptr_ty().into(),
                self.ptr_ty().into(),
                self.i64_ty().into(),
            ],
            None,
        );
        self.builder
            .build_call(
                f_slow,
                &[
                    out.into(),
                    a.into(),
                    b.into(),
                    self.i64c(binop_code(op) as i64).into(),
                ],
                "",
            )
            .unwrap();
        self.throw_guard();
        self.br_to(cont_bb);

        self.builder.position_at_end(cont_bb);
        self.expr_guarded = true;
        Ok(out)
    }

    fn is_unshadowed_str_int_call<'a>(&self, e: &'a Expr) -> Option<&'a Expr> {
        if let Expr::Call {
            callee,
            type_args,
            args,
            ..
        } = e
        {
            if type_args.is_empty()
                && args.len() == 1
                && matches!(callee.as_ref(), Expr::Ident(n, _) if n == "string" && self.is_unshadowed_builtin("string"))
                && self.is_int_expr(&args[0])
            {
                return Some(&args[0]);
            }
        }
        None
    }

    fn is_guaranteed_str_chain_expr(&self, e: &Expr) -> bool {
        match e {
            Expr::Str(..) => true,
            Expr::Ident(n, _) => self.str_locals.as_ref().is_some_and(|s| s.contains(n)),
            Expr::Call {
                callee,
                type_args,
                args,
                ..
            } if type_args.is_empty() && args.len() == 1 => matches!(
                callee.as_ref(),
                Expr::Ident(fn_name, _)
                    if (fn_name == "string" || fn_name == "json_write")
                        && self.is_unshadowed_builtin(fn_name)
            ),
            Expr::Binary {
                op: BinOp::Add,
                l,
                r,
                ..
            } => self.is_guaranteed_str_chain_expr(l) && self.is_guaranteed_str_chain_expr(r),
            _ => false,
        }
    }

    fn expr_contains_generic_call(e: &Expr) -> bool {
        match e {
            Expr::Call {
                type_args,
                callee,
                args,
                ..
            } => {
                !type_args.is_empty()
                    || Self::expr_contains_generic_call(callee)
                    || args.iter().any(Self::expr_contains_generic_call)
            }
            Expr::Unary { expr, .. } => Self::expr_contains_generic_call(expr),
            Expr::Binary { l, r, .. } => {
                Self::expr_contains_generic_call(l) || Self::expr_contains_generic_call(r)
            }
            Expr::Index { object, index, .. } => {
                Self::expr_contains_generic_call(object) || Self::expr_contains_generic_call(index)
            }
            _ => false,
        }
    }

    fn try_compile_str_add_chain(&mut self, e: &Expr) -> IrResult<Option<PointerValue<'ctx>>> {
        fn collect_left_parts<'a>(ir: &Ir<'_>, node: &'a Expr, out: &mut Vec<&'a Expr>) {
            if let Expr::Binary {
                op: BinOp::Add,
                l,
                r,
                ..
            } = node
            {
                if ir.is_guaranteed_str_chain_expr(l) {
                    collect_left_parts(ir, l, out);
                    out.push(r.as_ref());
                    return;
                }
            }
            out.push(node);
        }

        let Expr::Binary {
            op: BinOp::Add,
            l,
            r,
            ..
        } = e
        else {
            return Ok(None);
        };
        if !self.is_guaranteed_str_chain_expr(l) {
            return Ok(None);
        }
        let mut parts: Vec<&Expr> = Vec::new();
        collect_left_parts(self, l, &mut parts);
        parts.push(r.as_ref());

        // Fast 2-part specialization: `"prefix" + string(int_expr)`.
        if parts.len() == 2 {
            if let Expr::Str(lit, _) = parts[0] {
                if let Some(arg) = self.is_unshadowed_str_int_call(parts[1]) {
                    let iv = self.compile_expr_i64(arg)?;
                    let (gp, glen) = self.str_global(lit);
                    let out = self.alloc_v2_slot();
                    let f = self.tramp(
                        "spid3r_str_lit_i64",
                        &[
                            self.ptr_ty().into(),
                            self.ptr_ty().into(),
                            self.i64_ty().into(),
                            self.i64_ty().into(),
                        ],
                        None,
                    );
                    self.builder
                        .build_call(
                            f,
                            &[
                                out.into(),
                                gp.into(),
                                self.i64c(glen as i64).into(),
                                iv.into(),
                            ],
                            "",
                        )
                        .unwrap();
                    if !Self::expr_contains_generic_call(arg) {
                        self.expr_guarded = true;
                    }
                    return Ok(Some(out));
                }
            }
        }

        enum CompiledPart<'ctx> {
            Lit(PointerValue<'ctx>, usize),
            Int(inkwell::values::IntValue<'ctx>, bool),
            Val(PointerValue<'ctx>),
        }
        let mut compiled = Vec::with_capacity(parts.len());
        let mut has_val = false;
        let mut has_generic_call = false;
        for p in &parts {
            match p {
                Expr::Str(lit, _) => {
                    let (gp, glen) = self.str_global(lit);
                    compiled.push(CompiledPart::Lit(gp, glen));
                }
                _ if self.is_unshadowed_str_int_call(p).is_some() => {
                    let arg = self.is_unshadowed_str_int_call(p).unwrap();
                    let iv = self.compile_expr_i64(arg)?;
                    let gc = Self::expr_contains_generic_call(arg);
                    has_generic_call |= gc;
                    compiled.push(CompiledPart::Int(iv, gc));
                }
                other => {
                    has_val = true;
                    let pv = self.compile_expr(other)?;
                    compiled.push(CompiledPart::Val(pv));
                }
            }
        }

        let part_ty = self.ctx.struct_type(
            &[
                self.i64_ty().into(),
                self.i64_ty().into(),
                self.i64_ty().into(),
            ],
            false,
        );
        let arr_ty = part_ty.array_type(compiled.len() as u32);
        let arr_slot = self.alloca_entry_ty(arr_ty, "str.parts");
        for (idx, cp) in compiled.into_iter().enumerate() {
            let elem_ptr = unsafe {
                self.builder
                    .build_in_bounds_gep(part_ty, arr_slot, &[self.i64c(idx as i64)], "str.p")
                    .unwrap()
            };
            let (k_val, a_val, b_val) = match cp {
                CompiledPart::Lit(gp, glen) => {
                    let ai = self
                        .builder
                        .build_ptr_to_int(gp, self.i64_ty(), "sp.p")
                        .unwrap();
                    (self.i64c(0), ai, self.i64c(glen as i64))
                }
                CompiledPart::Int(iv, _) => (self.i64c(1), iv, self.i64c(0)),
                CompiledPart::Val(pv) => {
                    let ai = self
                        .builder
                        .build_ptr_to_int(pv, self.i64_ty(), "sp.v")
                        .unwrap();
                    (self.i64c(2), ai, self.i64c(0))
                }
            };
            let f0 = self
                .builder
                .build_struct_gep(part_ty, elem_ptr, 0, "sp.k")
                .unwrap();
            let f1 = self
                .builder
                .build_struct_gep(part_ty, elem_ptr, 1, "sp.a")
                .unwrap();
            let f2 = self
                .builder
                .build_struct_gep(part_ty, elem_ptr, 2, "sp.b")
                .unwrap();
            self.builder.build_store(f0, k_val).unwrap();
            self.builder.build_store(f1, a_val).unwrap();
            self.builder.build_store(f2, b_val).unwrap();
        }
        let out = self.alloc_v2_slot();
        let f_build = self.tramp(
            "spid3r_str_build",
            &[
                self.ptr_ty().into(),
                self.ptr_ty().into(),
                self.i64_ty().into(),
            ],
            None,
        );
        self.builder
            .build_call(
                f_build,
                &[
                    out.into(),
                    arr_slot.into(),
                    self.i64c(parts.len() as i64).into(),
                ],
                "",
            )
            .unwrap();
        if !has_val && !has_generic_call {
            self.expr_guarded = true;
        }
        Ok(Some(out))
    }

    fn compile_expr(&mut self, e: &Expr) -> IrResult<PointerValue<'ctx>> {
        let prev = std::mem::replace(&mut self.expr_guarded, false);
        let v_res = self.compile_expr_inner(e);
        let handled = std::mem::replace(&mut self.expr_guarded, prev);
        let v = v_res?;
        // Any operation that threw set the pending-throw flag; route to the
        // current handler instead of using this (placeholder) value further.
        if !handled && Self::expr_can_throw(e) {
            self.throw_guard();
        }
        Ok(v)
    }

    fn compile_expr_inner(&mut self, e: &Expr) -> IrResult<PointerValue<'ctx>> {
        match e {
            // Named arguments are reordered away by `crate::lower`; one
            // surviving here is a pipeline bug, not a user error.
            Expr::NamedArg { span, .. } => Err(format!(
                "internal error at {}:{}: a named argument reached the native \
                 backend without lowering (this is a bug in spid3r)",
                span.file, span.line
            )),
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
                let out = self.alloc_v2_slot();
                let f = self.tramp(
                    "spid3r_super_call",
                    &[
                        self.ptr_ty().into(),
                        self.ptr_ty().into(),
                        self.ptr_ty().into(),
                        self.ptr_ty().into(),
                        self.i64_ty().into(),
                        self.ptr_ty().into(),
                    ],
                    None,
                );
                let n = self.i64c(args.len() as i64);
                self.builder
                    .build_call(
                        f,
                        &[
                            out.into(),
                            recv.into(),
                            pp.into(),
                            mp.into(),
                            n.into(),
                            arr.into(),
                        ],
                        "",
                    )
                    .unwrap();
                Ok(out)
            }
            // Surface-only nodes are rewritten by `crate::lower` before codegen.
            // (`?.` is a first-class op since F-4 — see the OptField arm above.)
            Expr::If { span, .. }
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
                let out = self.alloc_v2_slot();
                let f = self.tramp(
                    "spid3r_construct",
                    &[
                        self.ptr_ty().into(),
                        self.ptr_ty().into(),
                        self.i64_ty().into(),
                        self.ptr_ty().into(),
                    ],
                    None,
                );
                let n = self.i64c(items.len() as i64);
                self.builder
                    .build_call(f, &[out.into(), np.into(), n.into(), arr.into()], "")
                    .unwrap();
                Ok(out)
            }
            Expr::Fn(p) => self.compile_closure(p),
            Expr::Unary { op, expr, .. } => {
                if *op == UnOp::Neg && self.is_int_expr(e) {
                    let iv = self.compile_expr_i64(e)?;
                    return Ok(self.v_int_from_i64(iv));
                }
                if *op == UnOp::Neg && self.is_float_expr(e) {
                    let fv = self.compile_expr_f64(e)?;
                    return Ok(self.v_float_from_f64(fv));
                }
                let v = self.compile_expr(expr)?;
                Ok(self.call1p_i64("spid3r_unop", v, unop_code(*op) as i64))
            }
            Expr::Binary { op, l, r, .. } => {
                if let Some((cmp_op, left, right)) = self.is_int_compare(e) {
                    let predicate = match cmp_op {
                        BinOp::Lt => IntPredicate::SLT,
                        BinOp::Le => IntPredicate::SLE,
                        BinOp::Gt => IntPredicate::SGT,
                        BinOp::Ge => IntPredicate::SGE,
                        BinOp::Eq => IntPredicate::EQ,
                        BinOp::Ne => IntPredicate::NE,
                        _ => unreachable!(),
                    };
                    let lhs = self.compile_expr_i64(left)?;
                    let rhs = self.compile_expr_i64(right)?;
                    let c = self
                        .builder
                        .build_int_compare(predicate, lhs, rhs, "cmp")
                        .unwrap();
                    if !Self::expr_contains_generic_call(left)
                        && !Self::expr_contains_generic_call(right)
                    {
                        self.expr_guarded = true;
                    }
                    return Ok(self.v_bool_from_i1(c));
                }
                if let Some((cmp_op, left, right)) = self.is_float_compare(e) {
                    let predicate = match cmp_op {
                        BinOp::Lt => FloatPredicate::OLT,
                        BinOp::Le => FloatPredicate::OLE,
                        BinOp::Gt => FloatPredicate::OGT,
                        BinOp::Ge => FloatPredicate::OGE,
                        BinOp::Eq => FloatPredicate::OEQ,
                        BinOp::Ne => FloatPredicate::ONE,
                        _ => unreachable!(),
                    };
                    let lhs = self.compile_expr_f64(left)?;
                    let rhs = self.compile_expr_f64(right)?;
                    let c = self
                        .builder
                        .build_float_compare(predicate, lhs, rhs, "fcmp")
                        .unwrap();
                    if !Self::expr_contains_generic_call(left)
                        && !Self::expr_contains_generic_call(right)
                    {
                        self.expr_guarded = true;
                    }
                    return Ok(self.v_bool_from_i1(c));
                }
                // Phase 3: hybrid int compare over boxed leaves — see
                // `is_hybrid_int_expr`. Unreachable for the fully-unboxed
                // case (handled above). Re-enabled after the missing
                // boxed-pointer load in `emit_hybrid_leaf_checks` was fixed
                // (covered by `hybrid_int_loop_bounds_survive_aot`).
                if let Some((cmp_op, left, right)) = self.is_hybrid_int_compare(e) {
                    return self.emit_hybrid_int_compare_value(cmp_op, left, right, e);
                }
                if self.is_int_expr(e) {
                    let iv = self.compile_expr_i64(e)?;
                    return Ok(self.v_int_from_i64(iv));
                }
                if self.is_float_expr(e) {
                    let fv = self.compile_expr_f64(e)?;
                    return Ok(self.v_float_from_f64(fv));
                }
                if *op == BinOp::Add {
                    if let Some(sv) = self.try_compile_str_add_chain(e)? {
                        return Ok(sv);
                    }
                }
                let a = self.compile_expr(l)?;
                let b = self.compile_expr(r)?;
                if matches!(
                    op,
                    BinOp::Add
                        | BinOp::Sub
                        | BinOp::Mul
                        | BinOp::Eq
                        | BinOp::Ne
                        | BinOp::Lt
                        | BinOp::Le
                        | BinOp::Gt
                        | BinOp::Ge
                ) {
                    return self.emit_v2_binop_inline(a, b, *op);
                }
                Ok(self.call_binop(a, b, binop_code(*op)))
            }
            Expr::Logical { op, l, r, .. } => {
                // Short-circuit and coerce to bool, matching the Tree and VM engines.
                let lhs = self.compile_expr(l)?;
                let c = self.cond_i1(lhs);
                let lhs_i8 = self.i8c(u8::from(matches!(op, LogOp::Or)));
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
                let f_truthy = self.tramp(
                    "spid3r_truthy",
                    &[self.ptr_ty().into()],
                    Some(self.i8_ty().into()),
                );
                let rhs_i8 = call_int(self.emit_call(f_truthy, &[rhs.into()], "rt"));
                let rhs_from = self.cur_block().unwrap();
                self.br_to(end_bb);
                self.builder.position_at_end(end_bb);
                let pname = self.tmp_name("logic");
                let phi = self.builder.build_phi(self.i8_ty(), &pname).unwrap();
                phi.add_incoming(&[(&lhs_i8, lhs_from), (&rhs_i8, rhs_from)]);
                let phi_i1 = self
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        phi.as_basic_value().into_int_value(),
                        self.i8c(0),
                        "lb1",
                    )
                    .unwrap();
                Ok(self.v_bool_from_i1(phi_i1))
            }
            Expr::Assign {
                target, op, value, ..
            } => self.compile_assign(target, *op, value),
            Expr::Call {
                callee,
                type_args,
                args,
                ..
            } => self.compile_call_with_types(callee, type_args, args),
            Expr::Method {
                object, name, args, ..
            } => {
                if let Expr::Ident(vname, _) = &**object {
                    match (self.lookup(vname), name.as_str()) {
                        (Some(VarLoc::UListI64(slot)), "push") if args.len() == 1 => {
                            let ptr = self
                                .builder
                                .build_load(self.ptr_ty(), slot, "ul")
                                .unwrap()
                                .into_pointer_value();
                            let iv = if self.is_int_expr(&args[0]) {
                                self.compile_expr_i64(&args[0])?
                            } else {
                                let bv = self.compile_expr(&args[0])?;
                                self.call1p_to_i64("spid3r_to_i64", bv)
                            };
                            let f = self.tramp(
                                "spid3r_ulist_i64_push",
                                &[self.ptr_ty().into(), self.i64_ty().into()],
                                None,
                            );
                            self.builder
                                .build_call(f, &[ptr.into(), iv.into()], "")
                                .unwrap();
                            return Ok(self.v_null());
                        }
                        (Some(VarLoc::UListI64(slot)), "len")
                        | (Some(VarLoc::UListF64(slot)), "len")
                            if args.is_empty() =>
                        {
                            let ptr = self
                                .builder
                                .build_load(self.ptr_ty(), slot, "ul")
                                .unwrap()
                                .into_pointer_value();
                            let iv = self.emit_ulist_len(ptr);
                            return Ok(self.v_int_from_i64(iv));
                        }
                        (
                            Some(VarLoc::RowViewI64 { len, .. })
                            | Some(VarLoc::RowViewF64 { len, .. }),
                            "len",
                        ) if args.is_empty() => {
                            return Ok(self.v_int_from_i64(len));
                        }
                        (Some(VarLoc::UListI64(slot)), "pop") if args.is_empty() => {
                            let ptr = self
                                .builder
                                .build_load(self.ptr_ty(), slot, "ul")
                                .unwrap()
                                .into_pointer_value();
                            let f = self.tramp(
                                "spid3r_ulist_i64_pop",
                                &[self.ptr_ty().into()],
                                Some(self.i64_ty().into()),
                            );
                            let iv = call_int(self.emit_call(f, &[ptr.into()], "ul.pop"));
                            self.throw_guard();
                            return Ok(self.v_int_from_i64(iv));
                        }
                        (Some(VarLoc::UListF64(slot)), "push") if args.len() == 1 => {
                            let f64_t = self.ctx.f64_type();
                            let ptr = self
                                .builder
                                .build_load(self.ptr_ty(), slot, "ul")
                                .unwrap()
                                .into_pointer_value();
                            let fv = if self.is_float_expr(&args[0]) || self.is_int_expr(&args[0]) {
                                self.compile_expr_f64(&args[0])?
                            } else {
                                let bv = self.compile_expr(&args[0])?;
                                let tof = self.tramp(
                                    "spid3r_to_f64",
                                    &[self.ptr_ty().into()],
                                    Some(f64_t.into()),
                                );
                                let call_r =
                                    self.builder.build_call(tof, &[bv.into()], "f64").unwrap();
                                match call_r.try_as_basic_value() {
                                    inkwell::values::ValueKind::Basic(v) => v.into_float_value(),
                                    _ => unreachable!(),
                                }
                            };
                            let f = self.tramp(
                                "spid3r_ulist_f64_push",
                                &[self.ptr_ty().into(), f64_t.into()],
                                None,
                            );
                            self.builder
                                .build_call(f, &[ptr.into(), fv.into()], "")
                                .unwrap();
                            return Ok(self.v_null());
                        }
                        (Some(VarLoc::UListF64(slot)), "pop") if args.is_empty() => {
                            let f64_t = self.ctx.f64_type();
                            let ptr = self
                                .builder
                                .build_load(self.ptr_ty(), slot, "ul")
                                .unwrap()
                                .into_pointer_value();
                            let f = self.tramp(
                                "spid3r_ulist_f64_pop",
                                &[self.ptr_ty().into()],
                                Some(f64_t.into()),
                            );
                            let call_r = self.emit_call(f, &[ptr.into()], "ul.pop");
                            let fv = match call_r.try_as_basic_value() {
                                inkwell::values::ValueKind::Basic(v) => v.into_float_value(),
                                _ => unreachable!(),
                            };
                            return Ok(self.v_float_from_f64(fv));
                        }
                        _ => {}
                    }
                }
                let obj = self.compile_expr(object)?;
                let mut argv: Vec<PointerValue<'ctx>> = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.compile_expr(a)?);
                }
                let is_self = matches!(object.as_ref(), Expr::Ident(id, _) if id == "self");
                if !is_self {
                    // Track 3 RT-5 / Phase 2: inline `.len()` on ASCII ropes and
                    // SSO strings with `spid3r_list_len` fallback, plus `.push(x)`.
                    if name == "len" && argv.is_empty() {
                        return self.emit_v2_len_inline(obj);
                    }
                    if name == "push" && argv.len() == 1 {
                        return Ok(self.call2p("spid3r_list_push", obj, argv[0]));
                    }
                }
                let (np, _) = self.str_global(name);
                let arr = self.argv_array(&argv);
                let n = self.i64c(args.len() as i64);
                if is_self {
                    if let Some(cid_gv) = self.current_class_id_gv {
                        let cid = self
                            .builder
                            .build_load(self.i64_ty(), cid_gv.as_pointer_value(), "cid")
                            .unwrap()
                            .into_int_value();
                        let slot_name = self.tmp_name("mslot");
                        let slot_gv = self.module.add_global(self.i64_ty(), None, &slot_name);
                        slot_gv.set_initializer(&self.i64_ty().const_int((-2i64) as u64, true));
                        slot_gv.set_linkage(inkwell::module::Linkage::Internal);
                        let slot_ptr = slot_gv.as_pointer_value();
                        let cur_slot = self
                            .builder
                            .build_load(self.i64_ty(), slot_ptr, "slot")
                            .unwrap()
                            .into_int_value();
                        let uninit = self
                            .builder
                            .build_int_compare(
                                IntPredicate::EQ,
                                cur_slot,
                                self.i64_ty().const_int((-2i64) as u64, true),
                                "slot.uninit",
                            )
                            .unwrap();
                        let f_cur = self.current_fn();
                        let cur_bb = self.builder.get_insert_block().unwrap();
                        let resolve_bb = self.ctx.append_basic_block(f_cur, "slot.resolve");
                        let call_bb = self.ctx.append_basic_block(f_cur, "slot.call");
                        self.builder
                            .build_conditional_branch(uninit, resolve_bb, call_bb)
                            .unwrap();

                        self.builder.position_at_end(resolve_bb);
                        let f_slot_of = self.tramp(
                            "spid3r_class_slot_of",
                            &[self.i64_ty().into(), self.ptr_ty().into()],
                            Some(self.i64_ty().into()),
                        );
                        let resolved =
                            call_int(self.emit_call(f_slot_of, &[cid.into(), np.into()], "rslot"));
                        self.builder.build_store(slot_ptr, resolved).unwrap();
                        self.builder.build_unconditional_branch(call_bb).unwrap();

                        self.builder.position_at_end(call_bb);
                        let phi = self.builder.build_phi(self.i64_ty(), "slot.val").unwrap();
                        phi.add_incoming(&[(&cur_slot, cur_bb), (&resolved, resolve_bb)]);
                        let out = self.alloc_v2_slot();
                        let f_mslot = self.tramp(
                            "spid3r_method_slot",
                            &[
                                self.ptr_ty().into(),
                                self.ptr_ty().into(),
                                self.i64_ty().into(),
                                self.i64_ty().into(),
                                self.ptr_ty().into(),
                                self.i64_ty().into(),
                                self.ptr_ty().into(),
                            ],
                            None,
                        );
                        self.builder
                            .build_call(
                                f_mslot,
                                &[
                                    out.into(),
                                    obj.into(),
                                    cid.into(),
                                    phi.as_basic_value().into(),
                                    np.into(),
                                    n.into(),
                                    arr.into(),
                                ],
                                "",
                            )
                            .unwrap();
                        return Ok(out);
                    }
                }
                let out = self.alloc_v2_slot();
                let f = self.tramp(
                    "spid3r_method",
                    &[
                        self.ptr_ty().into(),
                        self.ptr_ty().into(),
                        self.ptr_ty().into(),
                        self.i64_ty().into(),
                        self.ptr_ty().into(),
                    ],
                    None,
                );
                self.builder
                    .build_call(
                        f,
                        &[out.into(), obj.into(), np.into(), n.into(), arr.into()],
                        "",
                    )
                    .unwrap();
                Ok(out)
            }
            Expr::Index { object, index, .. } => {
                if let Expr::Ident(name, _) = &**object {
                    match self.lookup(name) {
                        Some(VarLoc::UListI64(slot)) => {
                            let ptr = self
                                .builder
                                .build_load(self.ptr_ty(), slot, "ul")
                                .unwrap()
                                .into_pointer_value();
                            let idx = if self.is_int_expr(index) {
                                self.compile_expr_i64(index)?
                            } else {
                                let bv = self.compile_expr(index)?;
                                self.call1p_to_i64("spid3r_to_i64", bv)
                            };
                            let iv = self.emit_ulist_i64_get_inline(ptr, idx)?;
                            return Ok(self.v_int_from_i64(iv));
                        }
                        Some(VarLoc::UListF64(slot)) => {
                            let ptr = self
                                .builder
                                .build_load(self.ptr_ty(), slot, "ul")
                                .unwrap()
                                .into_pointer_value();
                            let idx = if self.is_int_expr(index) {
                                self.compile_expr_i64(index)?
                            } else {
                                let bv = self.compile_expr(index)?;
                                self.call1p_to_i64("spid3r_to_i64", bv)
                            };
                            let fv = self.emit_ulist_f64_get_inline(ptr, idx)?;
                            return Ok(self.v_float_from_f64(fv));
                        }
                        Some(VarLoc::RowViewI64 {
                            data,
                            len,
                            orig_slot,
                            is_ulist,
                        }) => {
                            let idx = if self.is_int_expr(index) {
                                self.compile_expr_i64(index)?
                            } else {
                                let bv = self.compile_expr(index)?;
                                self.call1p_to_i64("spid3r_to_i64", bv)
                            };
                            let bce_hit = self.is_row_bce_hit(name, index);
                            let iv = self.emit_row_view_i64_get(
                                bce_hit, data, len, orig_slot, is_ulist, idx,
                            )?;
                            return Ok(self.v_int_from_i64(iv));
                        }
                        Some(VarLoc::RowViewF64 {
                            data,
                            len,
                            orig_slot,
                            is_ulist,
                        }) => {
                            let idx = if self.is_int_expr(index) {
                                self.compile_expr_i64(index)?
                            } else {
                                let bv = self.compile_expr(index)?;
                                self.call1p_to_i64("spid3r_to_i64", bv)
                            };
                            let bce_hit = self.is_row_bce_hit(name, index);
                            let fv = self.emit_row_view_f64_get(
                                bce_hit, data, len, orig_slot, is_ulist, idx,
                            )?;
                            return Ok(self.v_float_from_f64(fv));
                        }
                        _ => {}
                    }
                }
                let obj = self.compile_expr(object)?;
                // Track 3 RT-5 / Phase 2: compile-time pre-hashed string-literal
                // map lookup trampoline (`spid3r_map_get_str_h`).
                if let Expr::Str(lit, _) = &**index {
                    let out = self.alloc_v2_slot();
                    let (kp, klen) = self.str_global(lit);
                    let hash = crate::value::collections::fx_hash_str(lit);
                    let f = self.tramp(
                        "spid3r_map_get_str_h",
                        &[
                            self.ptr_ty().into(),
                            self.ptr_ty().into(),
                            self.ptr_ty().into(),
                            self.i64_ty().into(),
                            self.i64_ty().into(),
                        ],
                        Some(self.i8_ty().into()),
                    );
                    let call_r = self
                        .builder
                        .build_call(
                            f,
                            &[
                                out.into(),
                                obj.into(),
                                kp.into(),
                                self.i64c(klen as i64).into(),
                                self.i64c(hash as i64).into(),
                            ],
                            "mget.threw",
                        )
                        .unwrap();
                    self.throw_guard_i8_call(call_r);
                    return Ok(out);
                }
                let idx = self.compile_expr(index)?;
                let out = self.alloc_v2_slot();
                let f = self.tramp(
                    "spid3r_index_get_chk",
                    &[
                        self.ptr_ty().into(),
                        self.ptr_ty().into(),
                        self.ptr_ty().into(),
                    ],
                    Some(self.i8_ty().into()),
                );
                let call_r = self
                    .builder
                    .build_call(f, &[out.into(), obj.into(), idx.into()], "idx.threw")
                    .unwrap();
                self.throw_guard_i8_call(call_r);
                Ok(out)
            }
            Expr::Field { object, name, .. } => {
                let obj = self.compile_expr(object)?;
                let (np, _) = self.str_global(name);
                Ok(self.call2p("spid3r_field_get", obj, np))
            }
            // `obj?.field` — the null-safe dot (F-4): a null receiver and a
            // missing map key read as null; the trampoline throws for every
            // other failure, and the throw_guard after this expression routes
            // it to the handler exactly like a plain field read.
            Expr::OptField { object, name, .. } => {
                let obj = self.compile_expr(object)?;
                let (np, _) = self.str_global(name);
                // if obj == null { null } else { field_get_soft(obj, name) }
                // — three blocks, phi-merged (same shape as the logical ops).
                let is_null = self.builder.build_is_null(obj, "opt.isnull").unwrap();
                let null_bb = self.new_block("optnull");
                let get_bb = self.new_block("optget");
                let cont_bb = self.new_block("optcont");
                self.builder
                    .build_conditional_branch(is_null, null_bb, get_bb)
                    .unwrap();
                self.builder.position_at_end(null_bb);
                let n = self.v_null();
                let null_from = self.cur_block().unwrap();
                self.br_to(cont_bb);
                self.builder.position_at_end(get_bb);
                let r = self.call2p("spid3r_field_get_soft", obj, np);
                let r_from = self.cur_block().unwrap();
                self.br_to(cont_bb);
                self.builder.position_at_end(cont_bb);
                let pname = self.tmp_name("opt");
                let phi = self.builder.build_phi(self.ptr_ty(), &pname).unwrap();
                phi.add_incoming(&[(&n, null_from), (&r, r_from)]);
                Ok(phi.as_basic_value().into_pointer_value())
            }
            Expr::New {
                class, args, span, ..
            } => {
                // Resolve the class name like any identifier: locals first,
                // then globals. A class value can sit in a local (`let C =
                // Pt; new C(9)`, or a class pulled out of a module namespace:
                // `let C = mod.C; new C(9)`), and the old global-only lookup
                // returned null for those — the tree/VM engines call through
                // the value. `new SomeClass(...)` with the class at global
                // scope takes the same global-read path this way.
                let cls = self.compile_expr(&Expr::Ident(class.clone(), span.clone()))?;
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
                let tmp = self.alloc_v2_slot();
                self.copy_v2(tmp, slot);
                Ok(tmp)
            }
            Some(VarLoc::LocalInt(slot)) => {
                let lname = self.tmp_name("iv");
                let iv = self
                    .builder
                    .build_load(self.i64_ty(), slot, &lname)
                    .unwrap()
                    .into_int_value();
                Ok(self.v_int_from_i64(iv))
            }
            Some(VarLoc::LocalFloat(slot)) => {
                let lname = self.tmp_name("fv");
                let fv = self
                    .builder
                    .build_load(self.ctx.f64_type(), slot, &lname)
                    .unwrap()
                    .into_float_value();
                Ok(self.v_float_from_f64(fv))
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
            Some(VarLoc::UListI64(_))
            | Some(VarLoc::UListF64(_))
            | Some(VarLoc::RowViewI64 { .. })
            | Some(VarLoc::RowViewF64 { .. }) => {
                Err(self.err("internal: unboxed list identifier escaped"))
            }
            None => {
                if let Some(c) = self.top_consts.get(name).cloned() {
                    self.const_expr_value(&c)
                } else {
                    Ok(self.global_value(name))
                }
            }
        }
    }

    /// Read a name from the runtime global table via the generation-guarded
    /// per-module cache slot (`{ i64 gen, ptr val }`).
    fn global_value(&mut self, name: &str) -> PointerValue<'ctx> {
        let slot_gv = if let Some(&gv) = self.global_caches.get(name) {
            gv
        } else {
            let slot_ty = self
                .ctx
                .struct_type(&[self.i64_ty().into(), self.ptr_ty().into()], false);
            let gv_name = self.tmp_name("gcache");
            let gv = self.module.add_global(slot_ty, None, &gv_name);
            let init = slot_ty.const_named_struct(&[
                self.i64_ty().const_zero().into(),
                self.ptr_ty().const_null().into(),
            ]);
            gv.set_initializer(&init);
            gv.set_linkage(inkwell::module::Linkage::Internal);
            self.global_caches.insert(name.to_string(), gv);
            gv
        };
        let (np, _) = self.str_global(name);
        self.call2p("spid3r_global_cached", np, slot_gv.as_pointer_value())
    }

    /// Call an arbitrary value handle with `argc` arguments from a raw array.
    fn build_call_value(
        &mut self,
        callee: PointerValue<'ctx>,
        argc: usize,
        argv: PointerValue<'ctx>,
    ) -> IrResult<PointerValue<'ctx>> {
        let out = self.alloc_v2_slot();
        let f = self.tramp(
            "spid3r_call",
            &[
                self.ptr_ty().into(),
                self.ptr_ty().into(),
                self.i64_ty().into(),
                self.ptr_ty().into(),
            ],
            None,
        );
        let n = self.i64c(argc as i64);
        self.builder
            .build_call(f, &[out.into(), callee.into(), n.into(), argv.into()], "")
            .unwrap();
        Ok(out)
    }

    // ------------------------------------------------------------- FFI

    fn target_is_windows_msvc(&self) -> bool {
        if let Some(ref t) = self.target_triple {
            return t.contains("windows-msvc") || t.contains("win32");
        }
        cfg!(all(target_os = "windows", target_env = "msvc"))
    }

    fn pod_field_llvm_ty(&self, ty: PodFieldTy) -> BasicTypeEnum<'ctx> {
        match ty {
            PodFieldTy::Int | PodFieldTy::Usize => self.i64_ty().into(),
            PodFieldTy::Float => self.ctx.f64_type().into(),
            PodFieldTy::Bool => self.i8_ty().into(),
            PodFieldTy::Ptr => self.ptr_ty().into(),
        }
    }

    fn pod_llvm_struct(&self, pod: &PodStruct) -> inkwell::types::StructType<'ctx> {
        let field_tys: Vec<BasicTypeEnum<'ctx>> = pod
            .fields
            .iter()
            .map(|(_, ft)| self.pod_field_llvm_ty(*ft))
            .collect();
        self.ctx.struct_type(&field_tys, false)
    }

    fn c_type_name(&self, ty_name: &str, who: &str) -> IrResult<BasicTypeEnum<'ctx>> {
        match ty_name {
            "int" => Ok(self.i64_ty().into()),
            "float" => Ok(self.ctx.f64_type().into()),
            "bool" => Ok(self.i8_ty().into()),
            "string" => Ok(self.ptr_ty().into()),
            "ptr" => Ok(self.ptr_ty().into()),
            "usize" => Ok(self.i64_ty().into()),
            other => {
                if let Some(pod) = self.pod_structs.get(other) {
                    return Ok(self.pod_llvm_struct(pod).into());
                }
                Err(format!(
                    "extern fn '{who}': cannot pass '{other}' to C (supported: int, float, bool, string, ptr, usize, or 1–2 scalar-field POD class)"
                ))
            }
        }
    }

    fn c_type(&self, t: &crate::ast::TypeExpr, who: &str) -> IrResult<BasicTypeEnum<'ctx>> {
        self.c_type_name(&t.name, who)
    }

    fn collect_pod_structs(&mut self, program: &Program) {
        self.pod_structs.clear();
        for stmt in &program.stmts {
            let Stmt::Class(c) = stmt else { continue };
            if c.parent.is_some() || c.fields.is_empty() || c.fields.len() > 2 {
                continue;
            }
            let mut fields = Vec::with_capacity(c.fields.len());
            let mut valid = true;
            for f in &c.fields {
                let Some(ref te) = f.ty else {
                    valid = false;
                    break;
                };
                if te.nullable || !te.union.is_empty() {
                    valid = false;
                    break;
                }
                let pft = match te.name.as_str() {
                    "int" => PodFieldTy::Int,
                    "float" => PodFieldTy::Float,
                    "bool" => PodFieldTy::Bool,
                    "usize" => PodFieldTy::Usize,
                    "ptr" => PodFieldTy::Ptr,
                    _ => {
                        valid = false;
                        break;
                    }
                };
                fields.push((f.name.clone(), pft));
            }
            if valid {
                self.pod_structs.insert(
                    c.name.clone(),
                    PodStruct {
                        name: c.name.clone(),
                        fields,
                    },
                );
            }
        }
    }

    /// Declare every `extern fn` in the program as an external symbol.
    pub fn declare_externs(&mut self, program: &Program) -> IrResult<()> {
        self.collect_pod_structs(program);
        let win64 = self.target_is_windows_msvc();
        for stmt in &program.stmts {
            let Stmt::Fn(f) = stmt else { continue };
            if !f.is_extern {
                continue;
            }
            let name = f.name.clone().unwrap_or_default();
            if name.is_empty() {
                return Err("extern fn needs a name".to_string());
            }
            let mut params: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> = Vec::new();
            let mut param_types: Vec<String> = Vec::new();
            let mut win64_sret: Option<String> = None;

            if let Some(te) = f.ret.as_ref() {
                if win64 {
                    if let Some(pod) = self.pod_structs.get(&te.name) {
                        if pod.fields.len() == 2 {
                            params.push(self.ptr_ty().into());
                            win64_sret = Some(pod.name.clone());
                        }
                    }
                }
            }

            for p in &f.params {
                let te = p.ty.as_ref().ok_or_else(|| {
                    format!("extern fn '{name}': every parameter needs a type (int, float, bool, string, ptr, usize)")
                })?;
                if let Some(pod) = self.pod_structs.get(&te.name) {
                    if win64 {
                        if pod.fields.len() == 1 {
                            params.push(self.i64_ty().into());
                        } else {
                            params.push(self.ptr_ty().into());
                        }
                    } else {
                        params.push(self.pod_llvm_struct(pod).into());
                    }
                } else {
                    params.push(self.c_type(te, &name)?.into());
                }
                param_types.push(te.name.clone());
            }
            let ret_type = f.ret.as_ref().map(|te| te.name.clone());
            let ft = match f.ret.as_ref() {
                Some(_te) if win64_sret.is_some() => {
                    self.ctx.void_type().fn_type(&params, f.is_variadic)
                }
                Some(te) if win64 && self.pod_structs.contains_key(&te.name) => {
                    self.i64_ty().fn_type(&params, f.is_variadic)
                }
                Some(te) => self.c_type(te, &name)?.fn_type(&params, f.is_variadic),
                None => self.ctx.void_type().fn_type(&params, f.is_variadic),
            };
            let fv = self.module.add_function(&name, ft, Some(Linkage::External));
            self.extern_sigs.insert(
                name.clone(),
                ExternSig {
                    params: param_types,
                    ret: ret_type,
                    is_variadic: f.is_variadic,
                    win64_sret,
                },
            );
            self.externs.insert(name, fv);
        }
        Ok(())
    }

    fn marshal_c_scalar(
        &mut self,
        v: PointerValue<'ctx>,
        ty_name: &str,
        who: &str,
        free_after: &mut Vec<PointerValue<'ctx>>,
    ) -> IrResult<inkwell::values::BasicMetadataValueEnum<'ctx>> {
        match ty_name {
            "int" | "usize" => {
                let f = self.tramp(
                    "spid3r_to_i64",
                    &[self.ptr_ty().into()],
                    Some(self.i64_ty().into()),
                );
                Ok(call_int(self.emit_call(f, &[v.into()], "ffi.i")).into())
            }
            "bool" => {
                let f = self.tramp(
                    "spid3r_to_i64",
                    &[self.ptr_ty().into()],
                    Some(self.i64_ty().into()),
                );
                let i = call_int(self.emit_call(f, &[v.into()], "ffi.b"));
                let b = self
                    .builder
                    .build_int_compare(IntPredicate::NE, i, self.i64_ty().const_zero(), "ffi.nz")
                    .unwrap();
                Ok(self
                    .builder
                    .build_int_z_extend(b, self.i8_ty(), "ffi.b8")
                    .unwrap()
                    .into())
            }
            "float" => {
                let f = self.tramp(
                    "spid3r_to_f64",
                    &[self.ptr_ty().into()],
                    Some(self.ctx.f64_type().into()),
                );
                Ok(call_float(self.emit_call(f, &[v.into()], "ffi.f")).into())
            }
            "ptr" => {
                let f = self.tramp(
                    "spid3r_to_ptr",
                    &[self.ptr_ty().into()],
                    Some(self.ptr_ty().into()),
                );
                Ok(call_ptr(self.emit_call(f, &[v.into()], "ffi.p")).into())
            }
            "string" => {
                let f = self.tramp(
                    "spid3r_cstr",
                    &[self.ptr_ty().into()],
                    Some(self.ptr_ty().into()),
                );
                let p = call_ptr(self.emit_call(f, &[v.into()], "ffi.s"));
                free_after.push(p);
                Ok(p.into())
            }
            other => Err(format!("extern fn '{who}': unsupported C type '{other}'")),
        }
    }

    fn box_c_scalar(
        &mut self,
        val: inkwell::values::BasicValueEnum<'ctx>,
        ty_name: &str,
        who: &str,
    ) -> IrResult<PointerValue<'ctx>> {
        match ty_name {
            "int" | "usize" => Ok(self.v_int_from_i64(val.into_int_value())),
            "bool" => {
                let c = self
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        val.into_int_value(),
                        self.i8c(0),
                        "ffi.b1",
                    )
                    .unwrap();
                Ok(self.v_bool_from_i1(c))
            }
            "float" => Ok(self.v_float_from_f64(val.into_float_value())),
            "ptr" => {
                let out = self.alloc_v2_slot();
                let f = self.tramp(
                    "spid3r_value_from_ptr",
                    &[self.ptr_ty().into(), self.ptr_ty().into()],
                    None,
                );
                self.builder
                    .build_call(f, &[out.into(), val.into()], "")
                    .unwrap();
                Ok(out)
            }
            "string" => {
                let out = self.alloc_v2_slot();
                let f = self.tramp(
                    "spid3r_value_from_cstr",
                    &[self.ptr_ty().into(), self.ptr_ty().into()],
                    None,
                );
                self.builder
                    .build_call(f, &[out.into(), val.into()], "")
                    .unwrap();
                Ok(out)
            }
            other => Err(format!(
                "extern fn '{who}': unsupported C return type '{other}'"
            )),
        }
    }

    fn pod_field_ty_name(ft: PodFieldTy) -> &'static str {
        match ft {
            PodFieldTy::Int => "int",
            PodFieldTy::Float => "float",
            PodFieldTy::Bool => "bool",
            PodFieldTy::Usize => "usize",
            PodFieldTy::Ptr => "ptr",
        }
    }

    fn marshal_pod_struct(
        &mut self,
        v: PointerValue<'ctx>,
        pod: &PodStruct,
        who: &str,
        win64: bool,
        free_after: &mut Vec<PointerValue<'ctx>>,
    ) -> IrResult<inkwell::values::BasicMetadataValueEnum<'ctx>> {
        let st_ty = self.pod_llvm_struct(pod);
        let mut field_vals = Vec::with_capacity(pod.fields.len());
        for (fname, fty) in &pod.fields {
            let (np, _) = self.str_global(fname);
            let fv = self.call2p("spid3r_field_get", v, np);
            let cval = self.marshal_c_scalar(fv, Self::pod_field_ty_name(*fty), who, free_after)?;
            let bv: inkwell::values::BasicValueEnum<'ctx> = match cval {
                inkwell::values::BasicMetadataValueEnum::IntValue(x) => x.into(),
                inkwell::values::BasicMetadataValueEnum::FloatValue(x) => x.into(),
                inkwell::values::BasicMetadataValueEnum::PointerValue(x) => x.into(),
                _ => unreachable!(),
            };
            field_vals.push(bv);
        }
        if win64 && pod.fields.len() == 1 {
            let bv = field_vals[0];
            let i64v = match bv {
                inkwell::values::BasicValueEnum::IntValue(iv) => {
                    if iv.get_type().get_bit_width() < 64 {
                        self.builder
                            .build_int_z_extend(iv, self.i64_ty(), "pod.zext")
                            .unwrap()
                    } else {
                        iv
                    }
                }
                inkwell::values::BasicValueEnum::FloatValue(fv) => self
                    .builder
                    .build_bit_cast(fv, self.i64_ty(), "pod.f2i")
                    .unwrap()
                    .into_int_value(),
                inkwell::values::BasicValueEnum::PointerValue(pv) => self
                    .builder
                    .build_ptr_to_int(pv, self.i64_ty(), "pod.p2i")
                    .unwrap(),
                _ => unreachable!(),
            };
            return Ok(i64v.into());
        }
        let mut agg = st_ty.get_undef();
        for (idx, fv) in field_vals.iter().enumerate() {
            agg = self
                .builder
                .build_insert_value(agg, *fv, idx as u32, "pod.ins")
                .unwrap()
                .into_struct_value();
        }
        if win64 {
            let slot = self.builder.build_alloca(st_ty, "pod.arg").unwrap();
            self.builder.build_store(slot, agg).unwrap();
            return Ok(slot.into());
        }
        Ok(agg.into())
    }

    fn box_pod_struct(
        &mut self,
        pod: &PodStruct,
        field_cvals: &[inkwell::values::BasicValueEnum<'ctx>],
        who: &str,
    ) -> IrResult<PointerValue<'ctx>> {
        let mut boxed_vals = Vec::with_capacity(pod.fields.len());
        let mut name_ptrs = Vec::with_capacity(pod.fields.len());
        for ((fname, fty), cval) in pod.fields.iter().zip(field_cvals.iter()) {
            boxed_vals.push(self.box_c_scalar(*cval, Self::pod_field_ty_name(*fty), who)?);
            let (np, _) = self.str_global(fname);
            name_ptrs.push(np);
        }
        let names_arr = self.argv_array(&name_ptrs);
        let vals_arr = self.argv_array(&boxed_vals);
        let (cn_ptr, _) = self.str_global(&pod.name);
        let out = self.alloc_v2_slot();
        let f = self.tramp(
            "spid3r_struct_box",
            &[
                self.ptr_ty().into(),
                self.ptr_ty().into(),
                self.i64_ty().into(),
                self.ptr_ty().into(),
                self.ptr_ty().into(),
            ],
            None,
        );
        let cnt = self.i64c(pod.fields.len() as i64);
        self.builder
            .build_call(
                f,
                &[
                    out.into(),
                    cn_ptr.into(),
                    cnt.into(),
                    names_arr.into(),
                    vals_arr.into(),
                ],
                "",
            )
            .unwrap();
        Ok(out)
    }

    fn infer_vararg_c_type(&self, arg: &Expr) -> &'static str {
        match arg {
            Expr::Float(_, _) => "float",
            Expr::Str(_, _) => "string",
            Expr::Ident(name, _) => self.var_c_hints.get(name).copied().unwrap_or("int"),
            Expr::Binary { l, r, op, .. } => {
                if *op == crate::ast::BinOp::Add
                    && (self.infer_vararg_c_type(l) == "string"
                        || self.infer_vararg_c_type(r) == "string")
                {
                    "string"
                } else if self.infer_vararg_c_type(l) == "float"
                    || self.infer_vararg_c_type(r) == "float"
                {
                    "float"
                } else {
                    "int"
                }
            }
            _ => "int",
        }
    }

    fn compile_extern_call(
        &mut self,
        name: &str,
        fv: FunctionValue<'ctx>,
        args: &[Expr],
    ) -> IrResult<PointerValue<'ctx>> {
        let sig = self
            .extern_sigs
            .get(name)
            .cloned()
            .ok_or_else(|| format!("missing signature for extern fn '{name}'"))?;
        let fixed_len = sig.params.len();
        if (!sig.is_variadic && args.len() != fixed_len)
            || (sig.is_variadic && args.len() < fixed_len)
        {
            return Err(format!(
                "extern fn '{name}' takes {}{} argument(s), got {}",
                if sig.is_variadic { "at least " } else { "" },
                fixed_len,
                args.len()
            ));
        }

        let win64 = self.target_is_windows_msvc();
        let mut raw: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> =
            Vec::with_capacity(args.len() + 1);
        let mut sret_alloca: Option<(PointerValue<'ctx>, PodStruct)> = None;
        if let Some(ref sret_name) = sig.win64_sret {
            let pod = self
                .pod_structs
                .get(sret_name)
                .cloned()
                .ok_or_else(|| format!("extern fn '{name}': missing sret pod '{sret_name}'"))?;
            let st_ty = self.pod_llvm_struct(&pod);
            let slot = self.builder.build_alloca(st_ty, "ffi.sret").unwrap();
            raw.push(slot.into());
            sret_alloca = Some((slot, pod));
        }

        let mut free_after: Vec<PointerValue<'ctx>> = Vec::new();
        for (i, arg) in args.iter().enumerate() {
            let v = self.compile_expr(arg)?;
            let declared = sig
                .params
                .get(i)
                .map(|s| s.as_str())
                .unwrap_or_else(|| self.infer_vararg_c_type(arg));
            let vararg_ty = if i >= fixed_len && declared == "bool" {
                "int"
            } else {
                declared
            };
            let meta = if let Some(pod) = self.pod_structs.get(vararg_ty).cloned() {
                self.marshal_pod_struct(v, &pod, name, win64, &mut free_after)?
            } else {
                self.marshal_c_scalar(v, vararg_ty, name, &mut free_after)?
            };
            raw.push(meta);
        }

        let call = self.emit_call(fv, &raw, "ffi.call");

        if !free_after.is_empty() {
            let free = self.tramp("spid3r_cstr_free", &[self.ptr_ty().into()], None);
            for p in free_after {
                self.emit_call(free, &[p.into()], "ffi.free");
            }
        }
        self.throw_guard();

        if let Some((slot, pod)) = sret_alloca {
            let st_ty = self.pod_llvm_struct(&pod);
            let mut cvals = Vec::with_capacity(pod.fields.len());
            for (idx, (_, fty)) in pod.fields.iter().enumerate() {
                let gep = self
                    .builder
                    .build_struct_gep(st_ty, slot, idx as u32, "sret.f")
                    .unwrap();
                let loaded = self
                    .builder
                    .build_load(self.pod_field_llvm_ty(*fty), gep, "sret.v")
                    .unwrap();
                cvals.push(loaded);
            }
            return self.box_pod_struct(&pod, &cvals, name);
        }

        match sig.ret.as_deref() {
            None => Ok(self.v_null()),
            Some(ret_name) => {
                if let Some(pod) = self.pod_structs.get(ret_name).cloned() {
                    if win64 && pod.fields.len() == 1 {
                        let i64v = call_int(call);
                        let (_, fty) = pod.fields[0];
                        let cval: inkwell::values::BasicValueEnum<'ctx> = match fty {
                            PodFieldTy::Int | PodFieldTy::Usize => i64v.into(),
                            PodFieldTy::Float => self
                                .builder
                                .build_bit_cast(i64v, self.ctx.f64_type(), "pod.i2f")
                                .unwrap(),
                            PodFieldTy::Bool => self
                                .builder
                                .build_int_truncate(i64v, self.i8_ty(), "pod.i2b")
                                .unwrap()
                                .into(),
                            PodFieldTy::Ptr => self
                                .builder
                                .build_int_to_ptr(i64v, self.ptr_ty(), "pod.i2p")
                                .unwrap()
                                .into(),
                        };
                        return self.box_pod_struct(&pod, &[cval], name);
                    }
                    let agg = call.try_as_basic_value().unwrap_basic().into_struct_value();
                    let mut cvals = Vec::with_capacity(pod.fields.len());
                    for idx in 0..pod.fields.len() {
                        cvals.push(
                            self.builder
                                .build_extract_value(agg, idx as u32, "pod.ext")
                                .unwrap(),
                        );
                    }
                    return self.box_pod_struct(&pod, &cvals, name);
                }
                let bv = call.try_as_basic_value().unwrap_basic();
                self.box_c_scalar(bv, ret_name, name)
            }
        }
    }

    fn parse_c_fn_sig(&self, sig: &str, who: &str) -> IrResult<(Vec<String>, Option<String>)> {
        let s = sig.trim();
        let rest = s
            .strip_prefix("fn")
            .ok_or_else(|| {
                format!("{who}: signature must look like \"fn(int, int) -> int\", got '{sig}'")
            })?
            .trim();
        let lp = rest
            .find('(')
            .ok_or_else(|| format!("{who}: missing '(' in '{sig}'"))?;
        let rp = rest
            .rfind(')')
            .ok_or_else(|| format!("{who}: missing ')' in '{sig}'"))?;
        let params_str = rest[lp + 1..rp].trim();
        let mut params = Vec::new();
        if !params_str.is_empty() {
            for p in params_str.split(',') {
                let p = p.trim();
                let ty = match p {
                    "int" | "float" | "bool" | "string" | "ptr" | "usize" => p.to_string(),
                    other => {
                        return Err(format!(
                            "{who}: unsupported C type '{other}' in '{sig}' (supported: int, float, bool, string, ptr, usize)"
                        ))
                    }
                };
                params.push(ty);
            }
        }
        let after = rest[rp + 1..].trim();
        let ret = if after.is_empty() {
            None
        } else if let Some(r) = after.strip_prefix("->") {
            let r = r.trim();
            if r == "void" {
                None
            } else if matches!(r, "int" | "float" | "bool" | "string" | "ptr" | "usize") {
                Some(r.to_string())
            } else {
                return Err(format!("{who}: unsupported C return type '{r}' in '{sig}'"));
            }
        } else {
            return Err(format!("{who}: invalid return clause '{after}' in '{sig}'"));
        };
        Ok((params, ret))
    }

    fn compile_ffi_callback(&mut self, args: &[Expr]) -> IrResult<PointerValue<'ctx>> {
        if args.len() != 2 {
            return Err(format!(
                "ffi_callback expects 2 arguments (fn, sig_str), got {}",
                args.len()
            ));
        }
        let sig_str = match &args[1] {
            Expr::Str(s, _) => s.clone(),
            _ => {
                return Err(
                    "ffi_callback: second argument must be a string literal signature like \"fn(int, int) -> int\""
                        .to_string(),
                )
            }
        };
        let (param_tys, ret_ty) = self.parse_c_fn_sig(&sig_str, "ffi_callback")?;
        let closure_v = self.compile_expr(&args[0])?;

        let cb_id = self.cb_counter;
        self.cb_counter += 1;

        let v2_ty = self.val_v2_ty();
        let slot_gv = self
            .module
            .add_global(v2_ty, None, &format!("__spid3r_cb_slot_{cb_id}"));
        slot_gv.set_initializer(&v2_ty.const_zero());
        slot_gv.set_linkage(Linkage::Internal);
        self.copy_v2(slot_gv.as_pointer_value(), closure_v);

        let mut llvm_params: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> =
            Vec::with_capacity(param_tys.len());
        for pt in &param_tys {
            llvm_params.push(self.c_type_name(pt, "ffi_callback")?.into());
        }
        let fn_ty = match ret_ty.as_deref() {
            Some(rt) => self
                .c_type_name(rt, "ffi_callback")?
                .fn_type(&llvm_params, false),
            None => self.ctx.void_type().fn_type(&llvm_params, false),
        };
        let thunk_fn = self.module.add_function(
            &format!("__spid3r_cb_thunk_{cb_id}"),
            fn_ty,
            Some(Linkage::Internal),
        );

        let saved_bb = self.builder.get_insert_block();
        let entry_bb = self.ctx.append_basic_block(thunk_fn, "entry");
        self.builder.position_at_end(entry_bb);

        let saved_exc = std::mem::take(&mut self.exc);
        let saved_ret_bb = self.ret_bb.take();
        let saved_ret_slots = std::mem::take(&mut self.ret_slots);
        let saved_frame_ph = self.frame_base_placeholder.take();
        let saved_frame_cnt = std::mem::replace(&mut self.frame_slot_count, 0);

        let clos = slot_gv.as_pointer_value();

        let mut boxed_args = Vec::with_capacity(param_tys.len());
        for (idx, pt) in param_tys.iter().enumerate() {
            let pval = thunk_fn.get_nth_param(idx as u32).unwrap();
            boxed_args.push(self.box_c_scalar(pval, pt, "ffi_callback")?);
        }
        let arr = self.argv_array(&boxed_args);
        let res_ptr = self.alloc_v2_slot();
        let call_tramp = self.tramp(
            "spid3r_call",
            &[
                self.ptr_ty().into(),
                self.ptr_ty().into(),
                self.i64_ty().into(),
                self.ptr_ty().into(),
            ],
            None,
        );
        let argc = self.i64c(param_tys.len() as i64);
        self.builder
            .build_call(
                call_tramp,
                &[res_ptr.into(), clos.into(), argc.into(), arr.into()],
                "",
            )
            .unwrap();

        let threw_f = self.tramp("spid3r_threw", &[], Some(self.i8_ty().into()));
        let threw = call_int(self.builder.build_call(threw_f, &[], "cb.threw").unwrap());
        let is_threw = self
            .builder
            .build_int_compare(
                IntPredicate::NE,
                threw,
                self.i8_ty().const_zero(),
                "cb.is_threw",
            )
            .unwrap();
        let err_bb = self.ctx.append_basic_block(thunk_fn, "cb.err");
        let ok_bb = self.ctx.append_basic_block(thunk_fn, "cb.ok");
        self.builder
            .build_conditional_branch(is_threw, err_bb, ok_bb)
            .unwrap();

        self.builder.position_at_end(err_bb);
        match ret_ty.as_deref() {
            None => {
                self.builder.build_return(None).unwrap();
            }
            Some("int" | "usize") => {
                let z = self.i64_ty().const_zero();
                self.builder.build_return(Some(&z)).unwrap();
            }
            Some("bool") => {
                let z = self.i8_ty().const_zero();
                self.builder.build_return(Some(&z)).unwrap();
            }
            Some("float") => {
                let z = self.ctx.f64_type().const_zero();
                self.builder.build_return(Some(&z)).unwrap();
            }
            Some("ptr" | "string") => {
                let z = self.ptr_ty().const_null();
                self.builder.build_return(Some(&z)).unwrap();
            }
            Some(_) => unreachable!(),
        }

        self.builder.position_at_end(ok_bb);
        if let Some(rt) = ret_ty.as_deref() {
            let mut dummy_free = Vec::new();
            let c_ret = self.marshal_c_scalar(res_ptr, rt, "ffi_callback", &mut dummy_free)?;
            let bv: inkwell::values::BasicValueEnum<'ctx> = match c_ret {
                inkwell::values::BasicMetadataValueEnum::IntValue(x) => x.into(),
                inkwell::values::BasicMetadataValueEnum::FloatValue(x) => x.into(),
                inkwell::values::BasicMetadataValueEnum::PointerValue(x) => x.into(),
                _ => unreachable!(),
            };
            self.builder.build_return(Some(&bv)).unwrap();
        } else {
            self.builder.build_return(None).unwrap();
        }

        self.frame_slot_count = saved_frame_cnt;
        self.frame_base_placeholder = saved_frame_ph;
        self.exc = saved_exc;
        self.ret_bb = saved_ret_bb;
        self.ret_slots = saved_ret_slots;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        let out = self.alloc_v2_slot();
        let box_ptr = self.tramp(
            "spid3r_value_from_ptr",
            &[self.ptr_ty().into(), self.ptr_ty().into()],
            None,
        );
        let fn_ptr = thunk_fn.as_global_value().as_pointer_value();
        self.builder
            .build_call(box_ptr, &[out.into(), fn_ptr.into()], "")
            .unwrap();
        Ok(out)
    }

    fn compile_dl_call(&mut self, args: &[Expr]) -> IrResult<PointerValue<'ctx>> {
        if !self.allow_dlopen {
            return Err(
                "'dl_call' requires --allow-dlopen (runtime FFI is disabled by default)"
                    .to_string(),
            );
        }
        if args.len() < 2 {
            return Err(format!(
                "dl_call expects at least 2 arguments (fn_ptr, sig_str, ...), got {}",
                args.len()
            ));
        }
        let sig_str = match &args[1] {
            Expr::Str(s, _) => s.clone(),
            _ => {
                return Err(
                    "dl_call: second argument must be a string literal signature like \"fn(int, int) -> int\""
                        .to_string(),
                )
            }
        };
        let (param_tys, ret_ty) = self.parse_c_fn_sig(&sig_str, "dl_call")?;
        let call_args = &args[2..];
        if call_args.len() != param_tys.len() {
            return Err(format!(
                "dl_call '{sig_str}' takes {} argument(s), got {}",
                param_tys.len(),
                call_args.len()
            ));
        }

        let fp_val = self.compile_expr(&args[0])?;
        let to_ptr = self.tramp(
            "spid3r_to_ptr",
            &[self.ptr_ty().into()],
            Some(self.ptr_ty().into()),
        );
        let fp = call_ptr(self.emit_call(to_ptr, &[fp_val.into()], "dl.fp"));

        let mut llvm_params: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> =
            Vec::with_capacity(param_tys.len());
        let mut raw_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> =
            Vec::with_capacity(call_args.len());
        let mut free_after = Vec::new();
        for (arg, pt) in call_args.iter().zip(param_tys.iter()) {
            llvm_params.push(self.c_type_name(pt, "dl_call")?.into());
            let v = self.compile_expr(arg)?;
            raw_args.push(self.marshal_c_scalar(v, pt, "dl_call", &mut free_after)?);
        }
        let fn_ty = match ret_ty.as_deref() {
            Some(rt) => self
                .c_type_name(rt, "dl_call")?
                .fn_type(&llvm_params, false),
            None => self.ctx.void_type().fn_type(&llvm_params, false),
        };
        let call = self
            .builder
            .build_indirect_call(fn_ty, fp, &raw_args, "dl.call")
            .unwrap();
        if !free_after.is_empty() {
            let free = self.tramp("spid3r_cstr_free", &[self.ptr_ty().into()], None);
            for p in free_after {
                self.emit_call(free, &[p.into()], "dl.free");
            }
        }
        self.throw_guard();

        match ret_ty.as_deref() {
            None => Ok(self.v_null()),
            Some(rt) => {
                let bv = call.try_as_basic_value().unwrap_basic();
                self.box_c_scalar(bv, rt, "dl_call")
            }
        }
    }

    fn subst_scalar_type(
        te: Option<&crate::ast::TypeExpr>,
        tparams: &[String],
        targs: &[crate::ast::TypeExpr],
    ) -> Option<&'static str> {
        let te = te?;
        if !te.union.is_empty() || te.nullable {
            return None;
        }
        let resolved_name = if let Some(pos) = tparams.iter().position(|p| p == &te.name) {
            let ta = &targs[pos];
            if !ta.union.is_empty() || ta.nullable || !ta.args.is_empty() {
                return None;
            }
            ta.name.as_str()
        } else {
            if !te.args.is_empty() {
                return None;
            }
            te.name.as_str()
        };
        match resolved_name {
            "int" | "usize" => Some("int"),
            "float" => Some("float"),
            "bool" => Some("bool"),
            _ => None,
        }
    }

    fn mangle_type_args(targs: &[crate::ast::TypeExpr]) -> String {
        targs
            .iter()
            .map(|t| {
                if t.args.is_empty() {
                    t.name.clone()
                } else {
                    let sub: Vec<String> = t.args.iter().map(|a| a.name.clone()).collect();
                    format!("{}_{}", t.name, sub.join("_"))
                }
            })
            .collect::<Vec<_>>()
            .join("_")
    }

    fn try_compile_mono_call(
        &mut self,
        name: &str,
        type_args: &[crate::ast::TypeExpr],
        args: &[Expr],
    ) -> IrResult<Option<inkwell::values::BasicValueEnum<'ctx>>> {
        if type_args.is_empty() {
            return Ok(None);
        }
        let Some(proto) = self.generic_fns.get(name).cloned() else {
            return Ok(None);
        };
        if proto.type_params.len() != type_args.len()
            || proto.params.len() != args.len()
            || proto.is_async
        {
            return Ok(None);
        }
        let mangled = format!(
            "spid3r_mono_{}__{}",
            name,
            Self::mangle_type_args(type_args)
        );
        let (fv, ret_sc, param_scs) = if let Some(cached) = self.mono_fns.get(&mangled).cloned() {
            cached
        } else {
            if self.mono_fns.len() >= 64 {
                return Ok(None);
            }
            let ret_sc = Self::subst_scalar_type(proto.ret.as_ref(), &proto.type_params, type_args)
                .map(|s| s.to_string());
            let param_scs: Vec<Option<String>> = proto
                .params
                .iter()
                .map(|p| {
                    Self::subst_scalar_type(p.ty.as_ref(), &proto.type_params, type_args)
                        .map(|s| s.to_string())
                })
                .collect();
            let f64_t = self.ctx.f64_type();
            let llvm_params: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> = param_scs
                .iter()
                .map(|sc| match sc.as_deref() {
                    Some("int") => self.i64_ty().into(),
                    Some("float") => f64_t.into(),
                    Some("bool") => self.ctx.bool_type().into(),
                    _ => self.ptr_ty().into(),
                })
                .collect();
            let fty = match ret_sc.as_deref() {
                Some("int") => self.i64_ty().fn_type(&llvm_params, false),
                Some("float") => f64_t.fn_type(&llvm_params, false),
                Some("bool") => self.ctx.bool_type().fn_type(&llvm_params, false),
                _ => self.val_v2_ty().fn_type(&llvm_params, false),
            };
            let fv = self.module.add_function(&mangled, fty, None);
            self.mono_fns
                .insert(mangled.clone(), (fv, ret_sc.clone(), param_scs.clone()));

            let caller_bb = self.builder.get_insert_block();
            let entry_bb = self.ctx.append_basic_block(fv, "entry");
            let ret_bb = self.ctx.append_basic_block(fv, "ret");
            let propagate_bb = self.ctx.append_basic_block(fv, "propagate");
            self.builder.position_at_end(entry_bb);

            let saved_frame_ph = self.frame_base_placeholder.take();
            let saved_frame_cnt = std::mem::replace(&mut self.frame_slot_count, 0);
            self.begin_v2_frame();

            let ret_int_slot = if ret_sc.as_deref() == Some("int") {
                Some(self.builder.build_alloca(self.i64_ty(), "mret.i").unwrap())
            } else {
                None
            };
            let ret_val_slot = self.builder.build_alloca(self.val_v2_ty(), "mret").unwrap();
            {
                self.builder.position_at_end(propagate_bb);
                if let Some(rs) = ret_int_slot {
                    self.builder.build_store(rs, self.i64c(0)).unwrap();
                }
                self.store_v2_inline(ret_val_slot, crate::value::ValueV2::TAG_NULL, self.i64c(0));
                self.builder.build_unconditional_branch(ret_bb).unwrap();
                self.builder.position_at_end(entry_bb);
            }

            let boxed_set = boxing_set(&proto);
            let mut int_params = HashSet::new();
            let mut float_params = HashSet::new();
            for (i, p) in proto.params.iter().enumerate() {
                if param_scs[i].as_deref() == Some("int") {
                    int_params.insert(p.name.clone());
                } else if param_scs[i].as_deref() == Some("float") {
                    float_params.insert(p.name.clone());
                }
            }
            let (int_locals, float_locals, ulists, str_locals) = analyze_fn_locals(
                &proto,
                &boxed_set,
                &int_params,
                &float_params,
                &self.shadowed_builtins,
            );
            let saved = (
                self.env_param.take(),
                self.ret_bb.replace(ret_bb),
                std::mem::take(&mut self.ret_slots),
                std::mem::take(&mut self.caps),
                self.boxed.replace(boxed_set.clone()),
                std::mem::take(&mut self.scopes),
                std::mem::take(&mut self.loops),
                std::mem::take(&mut self.exc),
                std::mem::take(&mut self.finally_stack),
                self.is_entry,
                self.int_locals.replace(int_locals.clone()),
                self.float_locals.replace(float_locals.clone()),
                self.unboxed_lists.replace(ulists),
                std::mem::replace(&mut self.mono_ret_int_slot, ret_int_slot),
                self.mono_ret_float_slot.take(),
                self.mono_ret_bool_slot.take(),
                self.mono_ret_threw_slot.take(),
                std::mem::replace(
                    &mut self.float_hint_vars,
                    collect_float_hint_vars(&proto.params, &proto.body.stmts),
                ),
                self.str_locals.replace(str_locals),
            );
            self.ret_slots.push(ret_val_slot);
            self.exc = vec![propagate_bb];
            self.is_entry = false;
            self.push_scope();
            for (i, p) in proto.params.iter().enumerate() {
                let raw = fv.get_nth_param(i as u32).unwrap();
                if int_locals.contains(&p.name) {
                    let slot = self.builder.build_alloca(self.i64_ty(), "mp.i").unwrap();
                    self.builder
                        .build_store(slot, raw.into_int_value())
                        .unwrap();
                    self.declare(&p.name, VarLoc::LocalInt(slot));
                } else if float_locals.contains(&p.name) {
                    let slot = self.builder.build_alloca(f64_t, "mp.f").unwrap();
                    self.builder
                        .build_store(slot, raw.into_float_value())
                        .unwrap();
                    self.declare(&p.name, VarLoc::LocalFloat(slot));
                } else {
                    let pv = match param_scs[i].as_deref() {
                        Some("int") => self.v_int_from_i64(raw.into_int_value()),
                        Some("float") => self.v_float_from_f64(raw.into_float_value()),
                        Some("bool") => self.v_bool_from_i1(raw.into_int_value()),
                        _ => raw.into_pointer_value(),
                    };
                    if boxed_set.contains(&p.name) {
                        let cell = self.call1p("spid3r_cell_new", pv);
                        let slot = self.builder.build_alloca(self.ptr_ty(), "mpc").unwrap();
                        self.builder.build_store(slot, cell).unwrap();
                        self.declare(&p.name, VarLoc::Cell(slot));
                    } else {
                        let slot = self.alloc_v2_slot();
                        self.copy_v2(slot, pv);
                        self.declare(&p.name, VarLoc::Local(slot));
                    }
                }
            }
            self.compile_block(&proto.body.stmts)?;
            self.pop_scope();
            self.ensure_open();
            if let Some(rs) = ret_int_slot {
                self.builder.build_store(rs, self.i64c(0)).unwrap();
            }
            let null = self.v_null();
            self.copy_v2(ret_val_slot, null);
            self.builder.build_unconditional_branch(ret_bb).unwrap();
            self.builder.position_at_end(ret_bb);
            match ret_sc.as_deref() {
                Some("int") => {
                    let iv = self
                        .builder
                        .build_load(self.i64_ty(), ret_int_slot.unwrap(), "mret.iv")
                        .unwrap()
                        .into_int_value();
                    self.builder.build_return(Some(&iv)).unwrap();
                }
                Some("float") => {
                    let tof =
                        self.tramp("spid3r_to_f64", &[self.ptr_ty().into()], Some(f64_t.into()));
                    let call_r = self
                        .builder
                        .build_call(tof, &[ret_val_slot.into()], "mret.f")
                        .unwrap();
                    let fval = match call_r.try_as_basic_value() {
                        inkwell::values::ValueKind::Basic(v) => v,
                        _ => unreachable!(),
                    };
                    self.builder.build_return(Some(&fval)).unwrap();
                }
                Some("bool") => {
                    let tf = self.tramp(
                        "spid3r_truthy",
                        &[self.ptr_ty().into()],
                        Some(self.i8_ty().into()),
                    );
                    let b8 = call_int(
                        self.builder
                            .build_call(tf, &[ret_val_slot.into()], "b")
                            .unwrap(),
                    );
                    let b = self
                        .builder
                        .build_int_compare(IntPredicate::NE, b8, self.i8c(0), "b1")
                        .unwrap();
                    self.builder.build_return(Some(&b)).unwrap();
                }
                _ => {
                    let rv = self
                        .builder
                        .build_load(self.val_v2_ty(), ret_val_slot, "mret.v")
                        .unwrap();
                    self.builder.build_return(Some(&rv)).unwrap();
                }
            }
            self.finish_v2_frame_if_used(fv, ret_bb);
            self.frame_slot_count = saved_frame_cnt;
            self.frame_base_placeholder = saved_frame_ph;

            self.str_locals = saved.18;
            self.float_hint_vars = saved.17;
            self.mono_ret_threw_slot = saved.16;
            self.mono_ret_bool_slot = saved.15;
            self.mono_ret_float_slot = saved.14;
            self.mono_ret_int_slot = saved.13;
            self.unboxed_lists = saved.12;
            self.float_locals = saved.11;
            self.int_locals = saved.10;
            self.is_entry = saved.9;
            self.finally_stack = saved.8;
            self.exc = saved.7;
            self.loops = saved.6;
            self.scopes = saved.5;
            self.boxed = saved.4;
            self.caps = saved.3;
            self.ret_slots = saved.2;
            self.ret_bb = saved.1;
            self.env_param = saved.0;
            if let Some(bb) = caller_bb {
                self.builder.position_at_end(bb);
            }
            (fv, ret_sc, param_scs)
        };

        let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> =
            Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            match param_scs[i].as_deref() {
                Some("int") => {
                    let iv = if self.is_int_expr(a) {
                        self.compile_expr_i64(a)?
                    } else {
                        let bv = self.compile_expr(a)?;
                        self.call1p_to_i64("spid3r_to_i64", bv)
                    };
                    call_args.push(iv.into());
                }
                Some("float") => {
                    let f64_t = self.ctx.f64_type();
                    let fv = if self.is_float_expr(a) || self.is_int_expr(a) {
                        self.compile_expr_f64(a)?
                    } else {
                        let bv = self.compile_expr(a)?;
                        let tof = self.tramp(
                            "spid3r_to_f64",
                            &[self.ptr_ty().into()],
                            Some(f64_t.into()),
                        );
                        let call_r = self.builder.build_call(tof, &[bv.into()], "farg").unwrap();
                        match call_r.try_as_basic_value() {
                            inkwell::values::ValueKind::Basic(v) => v.into_float_value(),
                            _ => unreachable!(),
                        }
                    };
                    call_args.push(fv.into());
                }
                Some("bool") => {
                    let bv = self.compile_expr(a)?;
                    let tf = self.tramp(
                        "spid3r_truthy",
                        &[self.ptr_ty().into()],
                        Some(self.ctx.bool_type().into()),
                    );
                    let b = call_int(self.builder.build_call(tf, &[bv.into()], "b").unwrap());
                    call_args.push(b.into());
                }
                _ => {
                    call_args.push(self.compile_expr(a)?.into());
                }
            }
        }
        let call = self.emit_call(fv, &call_args, "mono");
        let _ = ret_sc;
        Ok(match call.try_as_basic_value() {
            inkwell::values::ValueKind::Basic(inkwell::values::BasicValueEnum::StructValue(sv)) => {
                let out = self.alloc_v2_slot();
                self.builder.build_store(out, sv).unwrap();
                Some(out.into())
            }
            inkwell::values::ValueKind::Basic(v) => Some(v),
            _ => None,
        })
    }

    /// AOT-5: compile/call `spid3r_fn_raw__<name>` for statically-typed direct
    /// calls with unboxed scalar parameters/returns and zero-FFI `{ val, i1 }`
    /// exception propagation.
    fn try_compile_raw_call(
        &mut self,
        name: &str,
        args: &[Expr],
    ) -> IrResult<Option<inkwell::values::BasicValueEnum<'ctx>>> {
        if self.can_direct_raw_call(name, args).is_none() {
            return Ok(None);
        }
        let Some(proto) = self.raw_fns.get(name).cloned() else {
            return Ok(None);
        };
        let f64_t = self.ctx.f64_type();
        let (fv, ret_sc, param_scs) = if let Some(cached) = self.raw_compiled.get(name).cloned() {
            cached
        } else {
            if self.raw_compiled.len() >= 128 {
                return Ok(None);
            }
            let ret_sc = proto
                .ret
                .as_ref()
                .and_then(is_scalar_type_expr)
                .map(|s| s.to_string());
            let param_scs: Vec<Option<String>> = proto
                .params
                .iter()
                .map(|p| {
                    p.ty.as_ref()
                        .and_then(is_scalar_type_expr)
                        .map(|s| s.to_string())
                })
                .collect();
            let base_ret_ty: BasicTypeEnum<'ctx> = match ret_sc.as_deref() {
                Some("int") => self.i64_ty().into(),
                Some("float") => f64_t.into(),
                Some("bool") => self.i8_ty().into(),
                _ => self.val_v2_ty().into(),
            };
            let ret_struct_ty = self
                .ctx
                .struct_type(&[base_ret_ty, self.ctx.bool_type().into()], false);
            let llvm_params: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> = param_scs
                .iter()
                .map(|sc| match sc.as_deref() {
                    Some("int") => self.i64_ty().into(),
                    Some("float") => f64_t.into(),
                    Some("bool") => self.i8_ty().into(),
                    _ => self.ptr_ty().into(),
                })
                .collect();
            let fty = ret_struct_ty.fn_type(&llvm_params, false);
            let raw_name = format!("spid3r_fn_raw__{}", name);
            let fv = self
                .module
                .add_function(&raw_name, fty, Some(Linkage::Internal));
            self.add_fn_enum_attr(fv, "nounwind", 0);
            self.add_fn_enum_attr(fv, "willreturn", 0);
            self.add_fn_enum_attr(fv, "inlinehint", 0);
            self.raw_compiled
                .insert(name.to_string(), (fv, ret_sc.clone(), param_scs.clone()));

            let caller_bb = self.builder.get_insert_block();
            let entry_bb = self.ctx.append_basic_block(fv, "entry");
            let ret_bb = self.ctx.append_basic_block(fv, "ret");
            let propagate_bb = self.ctx.append_basic_block(fv, "propagate");
            self.builder.position_at_end(entry_bb);

            let ret_int_slot = if ret_sc.as_deref() == Some("int") {
                Some(self.builder.build_alloca(self.i64_ty(), "rret.i").unwrap())
            } else {
                None
            };
            let ret_float_slot = if ret_sc.as_deref() == Some("float") {
                Some(self.builder.build_alloca(f64_t, "rret.f").unwrap())
            } else {
                None
            };
            let ret_bool_slot = if ret_sc.as_deref() == Some("bool") {
                Some(self.builder.build_alloca(self.i8_ty(), "rret.b").unwrap())
            } else {
                None
            };
            let saved_frame_ph = self.frame_base_placeholder.take();
            let saved_frame_cnt = std::mem::replace(&mut self.frame_slot_count, 0);
            self.begin_v2_frame();
            let ret_val_slot = self
                .builder
                .build_alloca(self.val_v2_ty(), "rret.v")
                .unwrap();
            let ret_threw_slot = self.builder.build_alloca(self.i8_ty(), "rret.ex").unwrap();
            self.builder
                .build_store(ret_threw_slot, self.i8c(0))
                .unwrap();

            {
                self.builder.position_at_end(propagate_bb);
                if let Some(rs) = ret_int_slot {
                    self.builder.build_store(rs, self.i64c(0)).unwrap();
                }
                if let Some(rs) = ret_float_slot {
                    self.builder
                        .build_store(rs, f64_t.const_float(0.0))
                        .unwrap();
                }
                if let Some(rs) = ret_bool_slot {
                    self.builder.build_store(rs, self.i8c(0)).unwrap();
                }
                self.store_v2_inline(ret_val_slot, crate::value::ValueV2::TAG_NULL, self.i64c(0));
                self.builder
                    .build_store(ret_threw_slot, self.i8c(1))
                    .unwrap();
                self.builder.build_unconditional_branch(ret_bb).unwrap();
                self.builder.position_at_end(entry_bb);
            }

            let boxed_set = boxing_set(&proto);
            let mut int_params = HashSet::new();
            let mut float_params = HashSet::new();
            for (i, p) in proto.params.iter().enumerate() {
                if param_scs[i].as_deref() == Some("int") {
                    int_params.insert(p.name.clone());
                } else if param_scs[i].as_deref() == Some("float") {
                    float_params.insert(p.name.clone());
                }
            }
            let (int_locals, float_locals, ulists, str_locals) = analyze_fn_locals(
                &proto,
                &boxed_set,
                &int_params,
                &float_params,
                &self.shadowed_builtins,
            );
            let saved = (
                self.env_param.take(),
                self.ret_bb.replace(ret_bb),
                std::mem::take(&mut self.ret_slots),
                std::mem::take(&mut self.caps),
                self.boxed.replace(boxed_set.clone()),
                std::mem::take(&mut self.scopes),
                std::mem::take(&mut self.loops),
                std::mem::take(&mut self.exc),
                std::mem::take(&mut self.finally_stack),
                self.is_entry,
                self.int_locals.replace(int_locals.clone()),
                self.float_locals.replace(float_locals.clone()),
                self.unboxed_lists.replace(ulists),
                std::mem::replace(&mut self.mono_ret_int_slot, ret_int_slot),
                std::mem::replace(&mut self.mono_ret_float_slot, ret_float_slot),
                std::mem::replace(&mut self.mono_ret_bool_slot, ret_bool_slot),
                self.mono_ret_threw_slot.replace(ret_threw_slot),
                std::mem::replace(
                    &mut self.float_hint_vars,
                    collect_float_hint_vars(&proto.params, &proto.body.stmts),
                ),
                self.str_locals.replace(str_locals),
            );
            self.ret_slots.push(ret_val_slot);
            self.exc = vec![propagate_bb];
            self.is_entry = false;
            self.push_scope();
            for (i, p) in proto.params.iter().enumerate() {
                let raw = fv.get_nth_param(i as u32).unwrap();
                if int_locals.contains(&p.name) {
                    let slot = self.builder.build_alloca(self.i64_ty(), "rp.i").unwrap();
                    self.builder
                        .build_store(slot, raw.into_int_value())
                        .unwrap();
                    self.declare(&p.name, VarLoc::LocalInt(slot));
                } else if float_locals.contains(&p.name) {
                    let slot = self.builder.build_alloca(f64_t, "rp.f").unwrap();
                    self.builder
                        .build_store(slot, raw.into_float_value())
                        .unwrap();
                    self.declare(&p.name, VarLoc::LocalFloat(slot));
                } else {
                    let pv = match param_scs[i].as_deref() {
                        Some("int") => self.v_int_from_i64(raw.into_int_value()),
                        Some("float") => self.v_float_from_f64(raw.into_float_value()),
                        Some("bool") => {
                            let c = self
                                .builder
                                .build_int_compare(
                                    IntPredicate::NE,
                                    raw.into_int_value(),
                                    self.i8c(0),
                                    "pb1",
                                )
                                .unwrap();
                            self.v_bool_from_i1(c)
                        }
                        _ => raw.into_pointer_value(),
                    };
                    if boxed_set.contains(&p.name) {
                        let cell = self.call1p("spid3r_cell_new", pv);
                        let slot = self.builder.build_alloca(self.ptr_ty(), "rpc").unwrap();
                        self.builder.build_store(slot, cell).unwrap();
                        self.declare(&p.name, VarLoc::Cell(slot));
                    } else {
                        let slot = self.alloc_v2_slot();
                        self.copy_v2(slot, pv);
                        self.declare(&p.name, VarLoc::Local(slot));
                    }
                }
            }
            self.compile_block(&proto.body.stmts)?;
            self.pop_scope();
            self.ensure_open();
            self.builder
                .build_store(ret_threw_slot, self.i8c(0))
                .unwrap();
            if let Some(rs) = ret_int_slot {
                self.builder.build_store(rs, self.i64c(0)).unwrap();
            }
            if let Some(rs) = ret_float_slot {
                self.builder
                    .build_store(rs, f64_t.const_float(0.0))
                    .unwrap();
            }
            if let Some(rs) = ret_bool_slot {
                self.builder.build_store(rs, self.i8c(0)).unwrap();
            }
            let null = self.v_null();
            self.copy_v2(ret_val_slot, null);
            self.builder.build_unconditional_branch(ret_bb).unwrap();

            self.builder.position_at_end(ret_bb);
            let out_val: inkwell::values::BasicValueEnum<'ctx> = match ret_sc.as_deref() {
                Some("int") => self
                    .builder
                    .build_load(self.i64_ty(), ret_int_slot.unwrap(), "rret.iv")
                    .unwrap(),
                Some("float") => self
                    .builder
                    .build_load(f64_t, ret_float_slot.unwrap(), "rret.fv")
                    .unwrap(),
                Some("bool") => self
                    .builder
                    .build_load(self.i8_ty(), ret_bool_slot.unwrap(), "rret.bv")
                    .unwrap(),
                _ => self
                    .builder
                    .build_load(self.val_v2_ty(), ret_val_slot, "rret.pv")
                    .unwrap(),
            };
            let threw_i8 = self
                .builder
                .build_load(self.i8_ty(), ret_threw_slot, "rret.ex8")
                .unwrap()
                .into_int_value();
            let threw_i1 = self
                .builder
                .build_int_compare(IntPredicate::NE, threw_i8, self.i8c(0), "rret.ex1")
                .unwrap();
            let agg0 = ret_struct_ty.const_zero();
            let agg1 = self
                .builder
                .build_insert_value(agg0, out_val, 0, "ragg.v")
                .unwrap()
                .into_struct_value();
            let agg2 = self
                .builder
                .build_insert_value(agg1, threw_i1, 1, "ragg.ex")
                .unwrap()
                .into_struct_value();
            self.builder.build_return(Some(&agg2)).unwrap();

            self.finish_v2_frame_if_used(fv, ret_bb);
            self.frame_slot_count = saved_frame_cnt;
            self.frame_base_placeholder = saved_frame_ph;

            self.str_locals = saved.18;
            self.float_hint_vars = saved.17;
            self.mono_ret_threw_slot = saved.16;
            self.mono_ret_bool_slot = saved.15;
            self.mono_ret_float_slot = saved.14;
            self.mono_ret_int_slot = saved.13;
            self.unboxed_lists = saved.12;
            self.float_locals = saved.11;
            self.int_locals = saved.10;
            self.is_entry = saved.9;
            self.finally_stack = saved.8;
            self.exc = saved.7;
            self.loops = saved.6;
            self.scopes = saved.5;
            self.boxed = saved.4;
            self.caps = saved.3;
            self.ret_slots = saved.2;
            self.ret_bb = saved.1;
            self.env_param = saved.0;
            if let Some(bb) = caller_bb {
                self.builder.position_at_end(bb);
            }
            (fv, ret_sc, param_scs)
        };

        let mut call_args: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> =
            Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            match param_scs[i].as_deref() {
                Some("int") => {
                    let iv = self.compile_expr_i64(a)?;
                    call_args.push(iv.into());
                }
                Some("float") => {
                    let fv = self.compile_expr_f64(a)?;
                    call_args.push(fv.into());
                }
                Some("bool") => {
                    let b1 = self.compile_cond_to_i1(a)?;
                    let b8 = self
                        .builder
                        .build_int_z_extend(b1, self.i8_ty(), "ba")
                        .unwrap();
                    call_args.push(b8.into());
                }
                _ => {
                    call_args.push(self.compile_expr(a)?.into());
                }
            }
        }
        let call = self.emit_call(fv, &call_args, "raw");
        let pair = match call.try_as_basic_value() {
            inkwell::values::ValueKind::Basic(v) => v.into_struct_value(),
            _ => unreachable!(),
        };
        let out_v = self.builder.build_extract_value(pair, 0, "raw.v").unwrap();
        let threw_i1 = self
            .builder
            .build_extract_value(pair, 1, "raw.ex")
            .unwrap()
            .into_int_value();
        self.throw_guard_i1(threw_i1);
        if ret_sc.is_none() {
            let out = self.alloc_v2_slot();
            self.builder.build_store(out, out_v).unwrap();
            return Ok(Some(out.into()));
        }
        Ok(Some(out_v))
    }

    fn compile_call_with_types(
        &mut self,
        callee: &Expr,
        type_args: &[crate::ast::TypeExpr],
        args: &[Expr],
    ) -> IrResult<PointerValue<'ctx>> {
        if !type_args.is_empty() {
            if let Expr::Ident(name, _) = callee {
                if let Some(raw_ret) = self.try_compile_mono_call(name, type_args, args)? {
                    let ret_sc = self.generic_fns.get(name).and_then(|p| {
                        Self::subst_scalar_type(p.ret.as_ref(), &p.type_params, type_args)
                    });
                    return Ok(match ret_sc {
                        Some("int") => self.v_int_from_i64(raw_ret.into_int_value()),
                        Some("float") => self.v_float_from_f64(raw_ret.into_float_value()),
                        Some("bool") => self.v_bool_from_i1(raw_ret.into_int_value()),
                        _ => raw_ret.into_pointer_value(),
                    });
                }
            }
        } else if let Expr::Ident(name, _) = callee {
            if let Some(ret_sc) = self.can_direct_raw_call(name, args) {
                if let Some(raw_ret) = self.try_compile_raw_call(name, args)? {
                    return Ok(match ret_sc {
                        "int" => self.v_int_from_i64(raw_ret.into_int_value()),
                        "float" => self.v_float_from_f64(raw_ret.into_float_value()),
                        "bool" => {
                            let c = self
                                .builder
                                .build_int_compare(
                                    IntPredicate::NE,
                                    raw_ret.into_int_value(),
                                    self.i8c(0),
                                    "rb1",
                                )
                                .unwrap();
                            self.v_bool_from_i1(c)
                        }
                        _ => raw_ret.into_pointer_value(),
                    });
                }
            }
        }
        self.compile_call(callee, args)
    }

    fn compile_call(&mut self, callee: &Expr, args: &[Expr]) -> IrResult<PointerValue<'ctx>> {
        // `extern fn` — a direct C call, marshalled explicitly.
        if let Expr::Ident(name, _) = callee {
            if let Some(fv) = self.externs.get(name).copied() {
                return self.compile_extern_call(name, fv, args);
            }
            match name.as_str() {
                "string"
                    if args.len() == 1
                        && self.is_unshadowed_builtin("string")
                        && self.is_int_expr(&args[0]) =>
                {
                    let iv = self.compile_expr_i64(&args[0])?;
                    let out = self.alloc_v2_slot();
                    let f = self.tramp(
                        "spid3r_str_from_i64",
                        &[self.ptr_ty().into(), self.i64_ty().into()],
                        None,
                    );
                    self.builder
                        .build_call(f, &[out.into(), iv.into()], "")
                        .unwrap();
                    if !Self::expr_contains_generic_call(&args[0]) {
                        self.expr_guarded = true;
                    }
                    return Ok(out);
                }
                "ffi_callback" => return self.compile_ffi_callback(args),
                "dl_call" => return self.compile_dl_call(args),
                "dl_open" => {
                    if !self.allow_dlopen {
                        return Err(
                            "'dl_open' requires --allow-dlopen (runtime FFI is disabled by default)"
                                .to_string(),
                        );
                    }
                    if args.len() != 1 {
                        return Err(format!("dl_open expects 1 argument, got {}", args.len()));
                    }
                    let v = self.compile_expr(&args[0])?;
                    return Ok(self.call1p("spid3r_dl_open", v));
                }
                "dl_sym" => {
                    if !self.allow_dlopen {
                        return Err(
                            "'dl_sym' requires --allow-dlopen (runtime FFI is disabled by default)"
                                .to_string(),
                        );
                    }
                    if args.len() != 2 {
                        return Err(format!("dl_sym expects 2 arguments, got {}", args.len()));
                    }
                    let h = self.compile_expr(&args[0])?;
                    let s = self.compile_expr(&args[1])?;
                    return Ok(self.call2p("spid3r_dl_sym", h, s));
                }
                "dl_close" => {
                    if !self.allow_dlopen {
                        return Err(
                            "'dl_close' requires --allow-dlopen (runtime FFI is disabled by default)"
                                .to_string(),
                        );
                    }
                    if args.len() != 1 {
                        return Err(format!("dl_close expects 1 argument, got {}", args.len()));
                    }
                    let h = self.compile_expr(&args[0])?;
                    return Ok(self.call1p("spid3r_dl_close", h));
                }
                _ => {}
            }
        }
        // builtin collection constructors: list(...), map(...), ...
        if let Expr::Ident(name, _) = callee {
            if collections::is_constructor(name) {
                let (np, _) = self.str_global(name);
                let mut argv: Vec<PointerValue<'ctx>> = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.compile_expr(a)?);
                }
                let arr = self.argv_array(&argv);
                let out = self.alloc_v2_slot();
                let f = self.tramp(
                    "spid3r_construct",
                    &[
                        self.ptr_ty().into(),
                        self.ptr_ty().into(),
                        self.i64_ty().into(),
                        self.ptr_ty().into(),
                    ],
                    None,
                );
                let n = self.i64c(args.len() as i64);
                self.builder
                    .build_call(f, &[out.into(), np.into(), n.into(), arr.into()], "")
                    .unwrap();
                return Ok(out);
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
        if let Expr::Ident(name, _) = target {
            if let Some(VarLoc::LocalInt(slot)) = self.lookup(name) {
                let iv = if op == AssignOp::Assign {
                    self.compile_expr_i64(value)?
                } else {
                    let bin = match op {
                        AssignOp::AddAssign => BinOp::Add,
                        AssignOp::SubAssign => BinOp::Sub,
                        AssignOp::MulAssign => BinOp::Mul,
                        AssignOp::DivAssign => BinOp::Div,
                        AssignOp::ModAssign => BinOp::Mod,
                        AssignOp::Assign => unreachable!(),
                    };
                    let cur = self
                        .builder
                        .build_load(self.i64_ty(), slot, "cur")
                        .unwrap()
                        .into_int_value();
                    let rhs = self.compile_expr_i64(value)?;
                    self.emit_i64_binop(cur, rhs, bin)?
                };
                self.builder.build_store(slot, iv).unwrap();
                return Ok(self.v_int_from_i64(iv));
            } else if let Some(VarLoc::LocalFloat(slot)) = self.lookup(name) {
                let fv = if op == AssignOp::Assign {
                    self.compile_expr_f64(value)?
                } else {
                    let bin = match op {
                        AssignOp::AddAssign => BinOp::Add,
                        AssignOp::SubAssign => BinOp::Sub,
                        AssignOp::MulAssign => BinOp::Mul,
                        AssignOp::DivAssign => BinOp::Div,
                        _ => unreachable!(),
                    };
                    let cur = self
                        .builder
                        .build_load(self.ctx.f64_type(), slot, "fcur")
                        .unwrap()
                        .into_float_value();
                    let rhs = self.compile_expr_f64(value)?;
                    self.emit_f64_binop(cur, rhs, bin)?
                };
                self.builder.build_store(slot, fv).unwrap();
                return Ok(self.v_float_from_f64(fv));
            }
        }
        if let Expr::Index { object, index, .. } = target {
            if op == AssignOp::Assign {
                if let Expr::Ident(name, _) = &**object {
                    match self.lookup(name) {
                        Some(VarLoc::UListI64(slot)) => {
                            let ptr = self
                                .builder
                                .build_load(self.ptr_ty(), slot, "ul")
                                .unwrap()
                                .into_pointer_value();
                            let idx = if self.is_int_expr(index) {
                                self.compile_expr_i64(index)?
                            } else {
                                let bv = self.compile_expr(index)?;
                                self.call1p_to_i64("spid3r_to_i64", bv)
                            };
                            let iv = if self.is_int_expr(value) {
                                self.compile_expr_i64(value)?
                            } else {
                                let bv = self.compile_expr(value)?;
                                self.call1p_to_i64("spid3r_to_i64", bv)
                            };
                            self.emit_ulist_i64_set_inline(ptr, idx, iv);
                            return Ok(self.v_int_from_i64(iv));
                        }
                        Some(VarLoc::UListF64(slot)) => {
                            let f64_t = self.ctx.f64_type();
                            let ptr = self
                                .builder
                                .build_load(self.ptr_ty(), slot, "ul")
                                .unwrap()
                                .into_pointer_value();
                            let idx = if self.is_int_expr(index) {
                                self.compile_expr_i64(index)?
                            } else {
                                let bv = self.compile_expr(index)?;
                                self.call1p_to_i64("spid3r_to_i64", bv)
                            };
                            let fv = if self.is_float_expr(value) || self.is_int_expr(value) {
                                self.compile_expr_f64(value)?
                            } else {
                                let bv = self.compile_expr(value)?;
                                let tof = self.tramp(
                                    "spid3r_to_f64",
                                    &[self.ptr_ty().into()],
                                    Some(f64_t.into()),
                                );
                                let call_r =
                                    self.builder.build_call(tof, &[bv.into()], "f64").unwrap();
                                match call_r.try_as_basic_value() {
                                    inkwell::values::ValueKind::Basic(v) => v.into_float_value(),
                                    _ => unreachable!(),
                                }
                            };
                            self.emit_ulist_f64_set_inline(ptr, idx, fv);
                            return Ok(self.v_float_from_f64(fv));
                        }
                        _ => {}
                    }
                }
            }
        }
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
                    self.copy_v2(slot, v);
                    Ok(v)
                }
                Some(VarLoc::LocalInt(slot)) => {
                    let iv = self.call1p_to_i64("spid3r_to_i64", v);
                    self.builder.build_store(slot, iv).unwrap();
                    Ok(v)
                }
                Some(VarLoc::LocalFloat(slot)) => {
                    let f64_t = self.ctx.f64_type();
                    let tof =
                        self.tramp("spid3r_to_f64", &[self.ptr_ty().into()], Some(f64_t.into()));
                    let call_r = self.builder.build_call(tof, &[v.into()], "f64").unwrap();
                    let fv = match call_r.try_as_basic_value() {
                        inkwell::values::ValueKind::Basic(bv) => bv.into_float_value(),
                        _ => unreachable!(),
                    };
                    self.builder.build_store(slot, fv).unwrap();
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
                Some(VarLoc::UListI64(_))
                | Some(VarLoc::UListF64(_))
                | Some(VarLoc::RowViewI64 { .. })
                | Some(VarLoc::RowViewF64 { .. }) => {
                    Err(self.err("internal: cannot reassign unboxed list"))
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
        let fty = self.ctx.void_type().fn_type(
            &[
                ptr_ty.into(),
                ptr_ty.into(),
                self.i64_ty().into(),
                ptr_ty.into(),
            ],
            false,
        );
        self.fn_counter += 1;
        let llvm_name = format!("spid3r_fn.{}.{}", self.fn_counter, name_hint);
        let f = self.module.add_function(&llvm_name, fty, None);
        self.add_fn_enum_attr(f, "nounwind", 0);
        self.add_fn_enum_attr(f, "willreturn", 0);
        let saved_bb = self.builder.get_insert_block();
        let entry_bb = self.ctx.append_basic_block(f, "entry");
        self.builder.position_at_end(entry_bb);

        let saved_frame_ph = self.frame_base_placeholder.take();
        let saved_frame_cnt = std::mem::replace(&mut self.frame_slot_count, 0);
        self.begin_v2_frame();

        // out + environment + argc + argv
        let out_ptr = f.get_nth_param(0).unwrap().into_pointer_value();
        let env = f.get_nth_param(1).unwrap().into_pointer_value();
        let argc_v = f.get_nth_param(2).unwrap().into_int_value();
        let argv = f.get_nth_param(3).unwrap().into_pointer_value();
        let ret_slot = self.alloc_v2_slot();
        let ret_bb = self.ctx.append_basic_block(f, "ret");
        {
            self.builder.position_at_end(ret_bb);
            self.copy_v2(out_ptr, ret_slot);
            let f_exit = self.tramp("spid3r_frame_exit", &[], None);
            self.builder.build_call(f_exit, &[], "").unwrap();
            self.builder.build_return(None).unwrap();
            self.builder.position_at_end(entry_bb);
        }
        let propagate_bb = self.ctx.append_basic_block(f, "propagate");
        {
            self.builder.position_at_end(propagate_bb);
            self.store_v2_inline(ret_slot, crate::value::ValueV2::TAG_NULL, self.i64c(0));
            self.builder.build_unconditional_branch(ret_bb).unwrap();
            self.builder.position_at_end(entry_bb);
        }
        let boxed_set = boxing_set(proto);
        let (int_locals, float_locals, unboxed_lists, str_locals) = analyze_fn_locals(
            proto,
            &boxed_set,
            &HashSet::new(),
            &HashSet::new(),
            &self.shadowed_builtins,
        );
        let saved = (
            self.env_param.replace(env),
            self.ret_bb.replace(ret_bb),
            std::mem::take(&mut self.ret_slots),
            std::mem::replace(&mut self.caps, caps),
            self.boxed.replace(boxed_set),
            std::mem::take(&mut self.scopes),
            std::mem::take(&mut self.loops),
            std::mem::take(&mut self.exc),
            std::mem::take(&mut self.finally_stack),
            self.is_entry,
            self.int_locals.replace(int_locals),
            self.float_locals.replace(float_locals),
            self.unboxed_lists.replace(unboxed_lists),
            self.mono_ret_int_slot.take(),
            self.mono_ret_float_slot.take(),
            self.mono_ret_bool_slot.take(),
            self.mono_ret_threw_slot.take(),
            std::mem::replace(
                &mut self.float_hint_vars,
                collect_float_hint_vars(&proto.params, &proto.body.stmts),
            ),
            self.str_locals.replace(str_locals),
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
            let slot = self.alloc_v2_slot();
            self.copy_v2(slot, recv);
            self.declare("self", VarLoc::Local(slot));
        }
        // bind parameters (argv[self_offset..]); captured params get a cell.
        let boxed = self.boxed.clone().unwrap_or_default();
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
                let slot = self.alloc_v2_slot();
                self.copy_v2(slot, a);
                self.declare(&p.name, VarLoc::Local(slot));
            }
        }
        self.compile_block(&proto.body.stmts)?;
        self.pop_scope();
        // implicit return null (in a fresh block if the body ended sealed)
        self.ensure_open();
        let null = self.v_null();
        self.copy_v2(ret_slot, null);
        self.builder.build_unconditional_branch(ret_bb).unwrap();
        self.builder.position_at_end(ret_bb);

        self.finish_v2_frame(f);
        self.frame_slot_count = saved_frame_cnt;
        self.frame_base_placeholder = saved_frame_ph;

        // restore
        self.str_locals = saved.18;
        self.float_hint_vars = saved.17;
        self.mono_ret_threw_slot = saved.16;
        self.mono_ret_bool_slot = saved.15;
        self.mono_ret_float_slot = saved.14;
        self.mono_ret_int_slot = saved.13;
        self.unboxed_lists = saved.12;
        self.float_locals = saved.11;
        self.int_locals = saved.10;
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
                Some(
                    loc @ (VarLoc::Cell(_)
                    | VarLoc::Local(_)
                    | VarLoc::LocalInt(_)
                    | VarLoc::LocalFloat(_)
                    | VarLoc::Upvalue(_)),
                ) => {
                    caps.push(name.clone());
                    env_sources.push(loc);
                }
                None
                | Some(VarLoc::UListI64(_))
                | Some(VarLoc::UListF64(_))
                | Some(VarLoc::RowViewI64 { .. })
                | Some(VarLoc::RowViewF64 { .. }) => {} // global or uncaptured unboxed list
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
                    VarLoc::Local(s) => self.call1p("spid3r_cell_new", *s),
                    VarLoc::LocalInt(slot) => {
                        let iv = self
                            .builder
                            .build_load(self.i64_ty(), *slot, "capiv")
                            .unwrap()
                            .into_int_value();
                        let v = self.v_int_from_i64(iv);
                        self.call1p("spid3r_cell_new", v)
                    }
                    VarLoc::LocalFloat(slot) => {
                        let fv = self
                            .builder
                            .build_load(self.ctx.f64_type(), *slot, "capfv")
                            .unwrap()
                            .into_float_value();
                        let v = self.v_float_from_f64(fv);
                        self.call1p("spid3r_cell_new", v)
                    }
                    VarLoc::Upvalue(j) => {
                        let env = self.cur_env();
                        self.call_env_slot(env, *j)
                    }
                    VarLoc::UListI64(_)
                    | VarLoc::UListF64(_)
                    | VarLoc::RowViewI64 { .. }
                    | VarLoc::RowViewF64 { .. } => unreachable!(),
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
        let out = self.alloc_v2_slot();
        let f = self.tramp(
            "spid3r_make_fn",
            &[
                self.ptr_ty().into(),
                self.ptr_ty().into(),
                self.i64_ty().into(),
                self.i64_ty().into(),
                self.ptr_ty().into(),
                self.ptr_ty().into(),
            ],
            None,
        );
        let n = self.i64c(arity);
        let mn = self.i64c(min_arity);
        self.builder
            .build_call(
                f,
                &[
                    out.into(),
                    name_ptr.into(),
                    n.into(),
                    mn.into(),
                    code_ptr.into(),
                    env.into(),
                ],
                "",
            )
            .unwrap();
        out
    }

    /// Build the module entry: register classes, run top-level statements.
    pub fn build_entry(&mut self, program: &Program) -> IrResult<PointerValue<'ctx>> {
        // Declare every `extern fn` before any body is emitted, so a call site
        // can resolve the symbol and a bad parameter type fails immediately.
        self.declare_externs(program)?;
        let ptr_ty = self.ptr_ty();
        let fty = self.ctx.void_type().fn_type(&[ptr_ty.into()], false);
        let entry = self.module.add_function("spid3r_entry", fty, None);
        self.add_fn_enum_attr(entry, "nounwind", 0);
        self.add_fn_enum_attr(entry, "willreturn", 0);
        let bb = self.ctx.append_basic_block(entry, "entry");
        self.builder.position_at_end(bb);
        let out_ptr = entry.get_nth_param(0).unwrap().into_pointer_value();
        let saved_frame_ph = self.frame_base_placeholder.take();
        let saved_frame_cnt = std::mem::replace(&mut self.frame_slot_count, 0);
        self.begin_v2_frame();
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
            self.int_locals.take(),
            self.float_locals.take(),
            std::mem::take(&mut self.top_consts),
            std::mem::take(&mut self.top_ints),
            self.unboxed_lists.take(),
            std::mem::take(&mut self.generic_fns),
            std::mem::take(&mut self.raw_fns),
            std::mem::take(&mut self.raw_compiled),
            self.mono_ret_int_slot.take(),
            self.mono_ret_float_slot.take(),
            self.mono_ret_bool_slot.take(),
            self.mono_ret_threw_slot.take(),
            std::mem::replace(
                &mut self.float_hint_vars,
                collect_float_hint_vars(&[], &program.stmts),
            ),
            std::mem::replace(
                &mut self.shadowed_builtins,
                collect_shadowed_builtins(program),
            ),
            self.str_locals.take(),
        );
        let (tc, ti) = collect_top_const_maps(program);
        self.top_consts = tc;
        self.top_ints = ti;
        self.raw_fns = collect_raw_fns(program);
        for s in &program.stmts {
            if let Stmt::Fn(f) = s {
                if !f.type_params.is_empty() && !f.is_extern && !f.is_async {
                    if let Some(ref name) = f.name {
                        self.generic_fns.insert(name.clone(), f.clone());
                    }
                }
            }
        }
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

        if !self.freestanding {
            let core_reg = self.tramp("spid3r_reg_core", &[], None);
            self.builder.build_call(core_reg, &[], "").unwrap();
            for group in collect_used_stdlib_groups(program) {
                let sym = format!("spid3r_reg_{}", group);
                let g_reg = self.tramp(&sym, &[], None);
                self.builder.build_call(g_reg, &[], "").unwrap();
            }
        }

        // register classes first (parents before children, as declared)
        for s in &program.stmts {
            if let Stmt::Class(c) = s {
                self.register_class(c)?;
            } else if let Stmt::Const {
                name,
                value: Expr::Ident(target, _),
                ..
            }
            | Stmt::Let {
                name,
                value: Some(Expr::Ident(target, _)),
                ..
            } = s
            {
                if program
                    .stmts
                    .iter()
                    .any(|st| matches!(st, Stmt::Class(c) if &c.name == target))
                {
                    let (np, _) = self.str_global(name);
                    let cls = self.global_value(target);
                    let f = self.tramp(
                        "spid3r_global_set",
                        &[self.ptr_ty().into(), self.ptr_ty().into()],
                        None,
                    );
                    self.builder
                        .build_call(f, &[np.into(), cls.into()], "")
                        .unwrap();
                }
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
        self.store_v2_inline(out_ptr, crate::value::ValueV2::TAG_NULL, self.i64c(0));
        let f_exit = self.tramp("spid3r_frame_exit", &[], None);
        self.builder.build_call(f_exit, &[], "").unwrap();
        self.builder.build_return(None).unwrap();

        self.finish_v2_frame(entry);
        self.frame_slot_count = saved_frame_cnt;
        self.frame_base_placeholder = saved_frame_ph;

        self.str_locals = saved.24;
        self.shadowed_builtins = saved.23;
        self.float_hint_vars = saved.22;
        self.mono_ret_threw_slot = saved.21;
        self.mono_ret_bool_slot = saved.20;
        self.mono_ret_float_slot = saved.19;
        self.mono_ret_int_slot = saved.18;
        self.raw_compiled = saved.17;
        self.raw_fns = saved.16;
        self.generic_fns = saved.15;
        self.unboxed_lists = saved.14;
        self.top_ints = saved.13;
        self.top_consts = saved.12;
        self.float_locals = saved.11;
        self.int_locals = saved.10;
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
        let display_name = c
            .name
            .strip_prefix("__mod_")
            .and_then(|s| s.split_once('_').map(|(_, rest)| rest))
            .or_else(|| {
                c.name
                    .strip_prefix("__sel_")
                    .and_then(|s| s.split_once('_').map(|(_, rest)| rest))
            })
            .unwrap_or(&c.name);
        let (display_ptr, _) = self.str_global(display_name);
        let (name_ptr, _) = self.str_global(&c.name);
        let parent_ptr = match &c.parent {
            Some(p) => {
                let (pp, _) = self.str_global(p);
                pp
            }
            None => self.null_ptr(),
        };
        let cls = self.call2p("spid3r_class_register", display_ptr, parent_ptr);
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
        let cid_name = self.tmp_name("class_id");
        let cid_gv = self.module.add_global(self.i64_ty(), None, &cid_name);
        cid_gv.set_initializer(&self.i64_ty().const_zero());
        cid_gv.set_linkage(inkwell::module::Linkage::Internal);
        let prev_class_id_gv = self.current_class_id_gv.replace(cid_gv);
        for m in &c.methods {
            let is_ctor = m.name.as_deref() == Some("new");
            let arity = if is_ctor {
                m.params.len() + 1 // synthetic self at argv[0]
            } else {
                m.params.len()
            };
            let min_arity = if is_ctor {
                m.min_arity() + 1 // synthetic self at argv[0]
            } else {
                m.min_arity()
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
                            self.i64c(min_arity as i64).into(),
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
        self.current_class_id_gv = prev_class_id_gv;
        // Mint class_id + MRO-flatten vtable right before publishing globally.
        let pub_fn = self.tramp(
            "spid3r_class_publish",
            &[self.ptr_ty().into()],
            Some(self.i64_ty().into()),
        );
        let cid_val = call_int(self.emit_call(pub_fn, &[cls.into()], "pub_cid"));
        self.builder
            .build_store(cid_gv.as_pointer_value(), cid_val)
            .unwrap();
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
            "spid3r_rt_start_slim",
            &[self.ptr_ty().into()],
            Some(i32_ty.into()),
        );
        let r = self.builder.build_call(f, &[entry.into()], "code").unwrap();
        let code = call_int(r);
        self.builder.build_return(Some(&code)).unwrap();
    }
}

fn collect_used_stdlib_groups(program: &Program) -> Vec<&'static str> {
    fn walk_pattern(p: &crate::ast::Pattern, out: &mut HashSet<&'static str>) {
        match p {
            crate::ast::Pattern::Wild | crate::ast::Pattern::Bind(_) => {}
            crate::ast::Pattern::Literal(e) => walk_expr(e, out),
            crate::ast::Pattern::Variant { args, .. } => {
                for a in args {
                    walk_pattern(a, out);
                }
            }
        }
    }
    fn walk_fn(f: &FnProto, out: &mut HashSet<&'static str>) {
        for p in &f.params {
            if let Some(d) = &p.default {
                walk_expr(d, out);
            }
        }
        walk_block(&f.body, out);
    }
    fn walk_block(b: &Block, out: &mut HashSet<&'static str>) {
        for s in &b.stmts {
            walk_stmt(s, out);
        }
    }
    fn walk_expr(e: &Expr, out: &mut HashSet<&'static str>) {
        match e {
            Expr::Ident(name, _) => {
                if let Some(g) = crate::stdlib::stdlib_group_for_name(name) {
                    out.insert(g);
                }
            }
            Expr::List(items, _) => {
                for it in items {
                    walk_expr(it, out);
                }
            }
            Expr::Fn(f) => walk_fn(f, out),
            Expr::Unary { expr, .. }
            | Expr::Await(expr, _)
            | Expr::Spawn { callable: expr, .. }
            | Expr::Propagate(expr, _)
            | Expr::NamedArg { value: expr, .. }
            | Expr::Field { object: expr, .. }
            | Expr::OptField { object: expr, .. } => walk_expr(expr, out),
            Expr::Binary { l, r, .. }
            | Expr::Logical { l, r, .. }
            | Expr::Index {
                object: l,
                index: r,
                ..
            }
            | Expr::Assign {
                target: l,
                value: r,
                ..
            }
            | Expr::Coalesce { l, r, .. } => {
                walk_expr(l, out);
                walk_expr(r, out);
            }
            Expr::Call { callee, args, .. } => {
                walk_expr(callee, out);
                for a in args {
                    walk_expr(a, out);
                }
            }
            Expr::Method { object, args, .. } => {
                walk_expr(object, out);
                for a in args {
                    walk_expr(a, out);
                }
            }
            Expr::New { args, .. } | Expr::SuperCall { args, .. } => {
                for a in args {
                    walk_expr(a, out);
                }
            }
            Expr::If {
                cond, then, els, ..
            } => {
                walk_expr(cond, out);
                walk_block(then, out);
                if let Some(eb) = els {
                    walk_block(eb, out);
                }
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                walk_expr(scrutinee, out);
                for arm in arms {
                    walk_pattern(&arm.pattern, out);
                    if let Some(g) = &arm.guard {
                        walk_expr(g, out);
                    }
                    walk_block(&arm.body, out);
                }
            }
            Expr::Int(_, _)
            | Expr::Float(_, _)
            | Expr::Str(_, _)
            | Expr::Bool(_, _)
            | Expr::Null(_)
            | Expr::Super { .. } => {}
        }
    }
    fn walk_stmt(s: &Stmt, out: &mut HashSet<&'static str>) {
        match s {
            Stmt::Import { .. } | Stmt::Enum(_) | Stmt::Break { .. } | Stmt::Continue { .. } => {}
            Stmt::Fn(f) => walk_fn(f, out),
            Stmt::Class(c) => {
                for m in &c.methods {
                    walk_fn(m, out);
                }
            }
            Stmt::Interface(i) => {
                for (_, params, _, _) in &i.methods {
                    for p in params {
                        if let Some(d) = &p.default {
                            walk_expr(d, out);
                        }
                    }
                }
                for m in &i.default_methods {
                    walk_fn(m, out);
                }
            }
            Stmt::Let { value: Some(e), .. }
            | Stmt::Const { value: e, .. }
            | Stmt::LetTuple { value: e, .. }
            | Stmt::Expr(e)
            | Stmt::Spawn { callable: e, .. }
            | Stmt::Throw { value: e, .. } => walk_expr(e, out),
            Stmt::Let { value: None, .. } | Stmt::Return { value: None, .. } => {}
            Stmt::Return { value: Some(e), .. } => walk_expr(e, out),
            Stmt::Yield { value, .. } => walk_expr(value, out),
            Stmt::If {
                cond,
                then,
                elifs,
                els,
                ..
            } => {
                walk_expr(cond, out);
                walk_block(then, out);
                for (c, b) in elifs {
                    walk_expr(c, out);
                    walk_block(b, out);
                }
                if let Some(eb) = els {
                    walk_block(eb, out);
                }
            }
            Stmt::While { cond, body, .. } => {
                walk_expr(cond, out);
                walk_block(body, out);
            }
            Stmt::ForIn { iter, body, .. } => {
                walk_expr(iter, out);
                walk_block(body, out);
            }
            Stmt::ForC {
                init,
                cond,
                post,
                body,
                ..
            } => {
                walk_stmt(init, out);
                walk_expr(cond, out);
                walk_expr(post, out);
                walk_block(body, out);
            }
            Stmt::Block(b) => walk_block(b, out),
            Stmt::Try {
                body,
                catch,
                finally,
                ..
            } => {
                walk_block(body, out);
                if let Some(c) = catch {
                    walk_block(&c.body, out);
                }
                if let Some(f) = finally {
                    walk_block(f, out);
                }
            }
        }
    }

    const ALL_GROUPS: [&str; 10] = [
        "json", "fs", "net", "db", "media", "tz", "unicode", "crypto", "data", "info",
    ];
    let mut used = HashSet::new();
    for s in &program.stmts {
        walk_stmt(s, &mut used);
    }
    ALL_GROUPS
        .into_iter()
        .filter(|g| used.contains(g))
        .collect()
}

/// Compile a checked program to an LLVM module.
pub fn build_module(
    program: &Program,
    module_name: &str,
) -> IrResult<inkwell::module::Module<'static>> {
    build_module_with_opts(program, module_name, &IrOptions::default())
}

/// Compile a checked program to an LLVM module with explicit AOT build options.
pub fn build_module_with_opts(
    program: &Program,
    module_name: &str,
    opts: &IrOptions,
) -> IrResult<inkwell::module::Module<'static>> {
    // Same seam as the interpreters: the AOT backend lowers before codegen.
    let lowered = crate::lower::lower(program);
    let program = lowered.as_ref();
    let ctx = Box::leak(Box::new(Context::create()));
    let mut ir = Ir::new(ctx, module_name);
    ir.target_triple = opts.target_triple.clone();
    ir.allow_dlopen = opts.allow_dlopen;
    ir.enforce_budget = opts.enforce_budget;
    ir.freestanding = opts.freestanding;
    if let Some(limit) = opts.enforce_budget {
        let gv = ir
            .module
            .add_global(ir.i64_ty(), None, "__spid3r_budget_steps");
        gv.set_initializer(&ir.i64_ty().const_int(limit.max(0) as u64, false));
        gv.set_linkage(Linkage::Internal);
        ir.budget_counter_gv = Some(gv);
    }
    let entry = ir.build_entry(program)?;
    if opts.freestanding {
        let entry_name = opts.entry_symbol.as_deref().unwrap_or("main");
        let i32_ty = ir.ctx.i32_type();
        let fty = i32_ty.fn_type(&[], false);
        let entry_fn = ir.module.add_function(entry_name, fty, None);
        let bb = ir.ctx.append_basic_block(entry_fn, "entry");
        ir.builder.position_at_end(bb);
        let f = ir.tramp(
            "spid3r_rt_start_freestanding",
            &[ir.ptr_ty().into()],
            Some(i32_ty.into()),
        );
        let r = ir
            .builder
            .build_call(f, &[entry.into()], "fs.code")
            .unwrap();
        let code = call_int(r);
        ir.builder.build_return(Some(&code)).unwrap();
    } else {
        ir.build_cmain(entry);
    }
    if let Err(e) = ir.module().verify() {
        return Err(format!("internal error: generated LLVM IR is invalid: {e}"));
    }
    Ok(ir.module)
}
