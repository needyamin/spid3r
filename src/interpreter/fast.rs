//! Track 5 (`TW-1` / `TW-2`): Lexical slot-resolved local frame execution and
//! generation-guarded global/builtin caching for the tree-walk interpreter.
//!
//! Keeps the tree-walk oracle 100% semantics-preserving and simple to audit:
//! - Only pure-local functions (whose enclosing environment is a root global
//!   scope and whose body contains no closures, classes, imports, `const`,
//!   `try`/`throw`, `spawn`, or `super`) use the slot-resolved path.
//! - Everything else executes on the classic `Arc<Mutex<EnvironmentData>>`
//!   path untouched.
//! - Statement-boundary budget, wall-clock, memory-limit, GC, cancellation,
//!   mutex-guard, and call-stack trace semantics match `exec_block`/`eval` 1:1.

use std::cell::RefCell;
use std::sync::{Arc, Weak};

use crate::ast::*;
use crate::diagnostics::{Diagnostic, Phase, Span};
use crate::environment::{self, Env};
use crate::value::collections::{self, FxHashMap};
use crate::value::{FunctionData, Value};

use super::{Flow, Interp, InterpCallbackHost};

pub(super) struct ResolvedFn {
    pub(super) num_slots: usize,
    pub(super) defaults: Vec<Option<RExpr>>,
    pub(super) body: Vec<RStmt>,
}

pub(super) enum RStmt {
    Let {
        slot: u16,
        value: Option<RExpr>,
        span: Span,
    },
    If {
        cond: RExpr,
        then: Vec<RStmt>,
        elifs: Vec<(RExpr, Vec<RStmt>)>,
        els: Option<Vec<RStmt>>,
        span: Span,
    },
    While {
        cond: RExpr,
        body: Vec<RStmt>,
        span: Span,
    },
    ForIn {
        slot: u16,
        iter: RExpr,
        body: Vec<RStmt>,
        span: Span,
    },
    ForC {
        init: Box<RStmt>,
        cond: RExpr,
        post: RExpr,
        body: Vec<RStmt>,
        span: Span,
    },
    Return {
        value: Option<RExpr>,
        span: Span,
    },
    Break(Span),
    Continue(Span),
    Block(Vec<RStmt>, Span),
    Expr(RExpr, Span),
}

impl RStmt {
    #[inline]
    fn span(&self) -> &Span {
        match self {
            RStmt::Let { span, .. }
            | RStmt::If { span, .. }
            | RStmt::While { span, .. }
            | RStmt::ForIn { span, .. }
            | RStmt::ForC { span, .. }
            | RStmt::Return { span, .. }
            | RStmt::Break(span)
            | RStmt::Continue(span)
            | RStmt::Block(_, span)
            | RStmt::Expr(_, span) => span,
        }
    }
}

pub(super) enum RExpr {
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    NoneVal,
    Str(Arc<str>),
    Local(u16),
    Global(Arc<str>, Span),
    List(Vec<RExpr>, Span),
    Unary {
        op: UnOp,
        expr: Box<RExpr>,
        span: Span,
    },
    Binary {
        op: BinOp,
        l: Box<RExpr>,
        r: Box<RExpr>,
        span: Span,
    },
    Logical {
        op: LogOp,
        l: Box<RExpr>,
        r: Box<RExpr>,
    },
    AssignLocal {
        slot: u16,
        op: AssignOp,
        value: Box<RExpr>,
        span: Span,
    },
    AssignGlobal {
        name: Arc<str>,
        op: AssignOp,
        value: Box<RExpr>,
        span: Span,
    },
    AssignIndex {
        object: Box<RExpr>,
        index: Box<RExpr>,
        op: AssignOp,
        value: Box<RExpr>,
        span: Span,
    },
    AssignField {
        object: Box<RExpr>,
        name: Arc<str>,
        op: AssignOp,
        value: Box<RExpr>,
        span: Span,
    },
    Construct {
        name: Arc<str>,
        args: Vec<RExpr>,
        span: Span,
    },
    Call {
        callee: Box<RExpr>,
        args: Vec<RExpr>,
        span: Span,
    },
    Method {
        object: Box<RExpr>,
        name: Arc<str>,
        args: Vec<RExpr>,
        span: Span,
    },
    Index {
        object: Box<RExpr>,
        index: Box<RExpr>,
        span: Span,
    },
    Field {
        object: Box<RExpr>,
        name: Arc<str>,
        span: Span,
    },
    OptField {
        object: Box<RExpr>,
        name: Arc<str>,
        span: Span,
    },
    New {
        class_slot: Option<u16>,
        class_name: Arc<str>,
        args: Vec<RExpr>,
        span: Span,
    },
    Await(Box<RExpr>, Span),
}

// ------------------------------------------------------------ thread-local caches

const GLOBAL_CACHE_SLOTS: usize = 64;

#[derive(Clone)]
struct GlobalCacheEntry {
    env_ptr: usize,
    name_ptr: usize,
    gen: u64,
    val: Value,
}

type ResolvedCacheEntry = (Weak<FunctionData>, Option<Arc<ResolvedFn>>);

thread_local! {
    static RESOLVED_CACHE: RefCell<FxHashMap<usize, ResolvedCacheEntry>> =
        RefCell::new(FxHashMap::default());
    static SLOT_POOL: RefCell<Vec<Vec<Value>>> = const { RefCell::new(Vec::new()) };
    static GLOBAL_CACHE: RefCell<Vec<Option<GlobalCacheEntry>>> =
        RefCell::new(vec![None; GLOBAL_CACHE_SLOTS]);
}

#[inline]
fn alloc_local_slots(n: usize) -> Vec<Value> {
    SLOT_POOL.with(|pool| {
        let mut v = pool
            .borrow_mut()
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(n.max(8)));
        v.clear();
        v.resize(n, Value::Null);
        v
    })
}

#[inline]
fn free_local_slots(mut v: Vec<Value>) {
    v.clear();
    if v.capacity() <= 256 {
        SLOT_POOL.with(|pool| {
            let mut p = pool.borrow_mut();
            if p.len() < 64 {
                p.push(v);
            }
        });
    }
}

#[inline]
fn cached_global_get(env: &Env, name: &Arc<str>) -> Option<Value> {
    let env_ptr = Arc::as_ptr(env) as usize;
    let name_ptr = name.as_ptr() as usize;
    let gen = environment::global_gen();
    let idx = ((env_ptr >> 4) ^ (name_ptr >> 3)) & (GLOBAL_CACHE_SLOTS - 1);
    let hit = GLOBAL_CACHE.with(|cache| {
        let c = cache.borrow();
        if let Some(e) = &c[idx] {
            if e.env_ptr == env_ptr && e.name_ptr == name_ptr && e.gen == gen {
                return Some(e.val.clone());
            }
        }
        None
    });
    if hit.is_some() {
        return hit;
    }
    let resolved = environment::get(env, name.as_ref())?;
    GLOBAL_CACHE.with(|cache| {
        cache.borrow_mut()[idx] = Some(GlobalCacheEntry {
            env_ptr,
            name_ptr,
            gen,
            val: resolved.clone(),
        });
    });
    Some(resolved)
}

/// Look up or build the slot-resolved representation for `fd_arc`.
/// Returns `None` if `fd_arc` uses features requiring heap `EnvironmentData`
/// frames (closures, classes, try/catch, spawn, etc.).
pub(super) fn get_or_resolve(fd_arc: &Arc<FunctionData>) -> Option<Arc<ResolvedFn>> {
    if !crate::value::lock_ref(&fd_arc.env).is_global {
        return None;
    }
    let key = Arc::as_ptr(fd_arc) as usize;
    let cached = RESOLVED_CACHE.with(|cache| {
        let map = cache.borrow();
        if let Some((weak, entry)) = map.get(&key) {
            if weak.upgrade().is_some_and(|a| Arc::ptr_eq(&a, fd_arc)) {
                return Some(entry.clone());
            }
        }
        None
    });
    if let Some(entry) = cached {
        return entry;
    }
    let resolved = resolve_fn(&fd_arc.proto).map(Arc::new);
    RESOLVED_CACHE.with(|cache| {
        let mut map = cache.borrow_mut();
        if map.len() > 2048 {
            map.clear();
        }
        map.insert(key, (Arc::downgrade(fd_arc), resolved.clone()));
    });
    resolved
}

// ------------------------------------------------------------ lexical resolver

struct Resolver {
    scopes: Vec<Vec<(String, u16)>>,
    next_slot: u16,
}

impl Resolver {
    fn new() -> Self {
        Self {
            scopes: vec![Vec::new()],
            next_slot: 0,
        }
    }

    fn declare(&mut self, name: &str) -> Option<u16> {
        let slot = self.next_slot;
        self.next_slot = self.next_slot.checked_add(1)?;
        self.scopes.last_mut()?.push((name.to_string(), slot));
        Some(slot)
    }

    fn lookup(&self, name: &str) -> Option<u16> {
        for scope in self.scopes.iter().rev() {
            for (n, slot) in scope.iter().rev() {
                if n == name {
                    return Some(*slot);
                }
            }
        }
        None
    }

    fn resolve_block(&mut self, stmts: &[Stmt], push_scope: bool) -> Option<Vec<RStmt>> {
        if push_scope {
            self.scopes.push(Vec::new());
        }
        let mut out = Vec::with_capacity(stmts.len());
        for s in stmts {
            out.push(self.resolve_stmt(s)?);
        }
        if push_scope {
            self.scopes.pop();
        }
        Some(out)
    }

    fn resolve_stmt(&mut self, s: &Stmt) -> Option<RStmt> {
        match s {
            Stmt::Let {
                name,
                value,
                vis,
                span,
                ..
            } => {
                if *vis == Vis::Public {
                    return None;
                }
                let rval = match value {
                    Some(e) => Some(self.resolve_expr(e)?),
                    None => None,
                };
                let slot = self.declare(name)?;
                Some(RStmt::Let {
                    slot,
                    value: rval,
                    span: span.clone(),
                })
            }
            Stmt::If {
                cond,
                then,
                elifs,
                els,
                span,
            } => {
                let rcond = self.resolve_expr(cond)?;
                let rthen = self.resolve_block(&then.stmts, true)?;
                let mut relifs = Vec::with_capacity(elifs.len());
                for (ec, eb) in elifs {
                    relifs.push((self.resolve_expr(ec)?, self.resolve_block(&eb.stmts, true)?));
                }
                let rels = match els {
                    Some(eb) => Some(self.resolve_block(&eb.stmts, true)?),
                    None => None,
                };
                Some(RStmt::If {
                    cond: rcond,
                    then: rthen,
                    elifs: relifs,
                    els: rels,
                    span: span.clone(),
                })
            }
            Stmt::While { cond, body, span } => Some(RStmt::While {
                cond: self.resolve_expr(cond)?,
                body: self.resolve_block(&body.stmts, true)?,
                span: span.clone(),
            }),
            Stmt::ForIn {
                var,
                var2: None,
                iter,
                body,
                span,
            } => {
                let riter = self.resolve_expr(iter)?;
                self.scopes.push(Vec::new());
                let slot = self.declare(var)?;
                let rbody = self.resolve_block(&body.stmts, false)?;
                self.scopes.pop();
                Some(RStmt::ForIn {
                    slot,
                    iter: riter,
                    body: rbody,
                    span: span.clone(),
                })
            }
            Stmt::ForC {
                init,
                cond,
                post,
                body,
                span,
            } => {
                self.scopes.push(Vec::new());
                let rinit = Box::new(self.resolve_stmt(init)?);
                let rcond = self.resolve_expr(cond)?;
                let rpost = self.resolve_expr(post)?;
                let rbody = self.resolve_block(&body.stmts, true)?;
                self.scopes.pop();
                Some(RStmt::ForC {
                    init: rinit,
                    cond: rcond,
                    post: rpost,
                    body: rbody,
                    span: span.clone(),
                })
            }
            Stmt::Return { value, span } => {
                let rval = match value {
                    Some(e) => Some(self.resolve_expr(e)?),
                    None => None,
                };
                Some(RStmt::Return {
                    value: rval,
                    span: span.clone(),
                })
            }
            Stmt::Break { span } => Some(RStmt::Break(span.clone())),
            Stmt::Continue { span } => Some(RStmt::Continue(span.clone())),
            Stmt::Block(b) => Some(RStmt::Block(
                self.resolve_block(&b.stmts, true)?,
                b.span.clone(),
            )),
            Stmt::Expr(e) => Some(RStmt::Expr(self.resolve_expr(e)?, e.span().clone())),
            _ => None,
        }
    }

    fn resolve_expr(&mut self, e: &Expr) -> Option<RExpr> {
        match e {
            Expr::Int(i, _) => Some(RExpr::Int(*i)),
            Expr::Float(f, _) => Some(RExpr::Float(*f)),
            Expr::Bool(b, _) => Some(RExpr::Bool(*b)),
            Expr::Null(_) => Some(RExpr::Null),
            Expr::Str(s, _) => Some(RExpr::Str(Arc::from(s.as_str()))),
            Expr::Ident(name, span) => {
                if name == "none" {
                    Some(RExpr::NoneVal)
                } else if let Some(slot) = self.lookup(name) {
                    Some(RExpr::Local(slot))
                } else {
                    Some(RExpr::Global(Arc::from(name.as_str()), span.clone()))
                }
            }
            Expr::List(items, span) => {
                let mut out = Vec::with_capacity(items.len());
                for it in items {
                    out.push(self.resolve_expr(it)?);
                }
                Some(RExpr::List(out, span.clone()))
            }
            Expr::Unary { op, expr, span } => Some(RExpr::Unary {
                op: *op,
                expr: Box::new(self.resolve_expr(expr)?),
                span: span.clone(),
            }),
            Expr::Binary { op, l, r, span } => Some(RExpr::Binary {
                op: *op,
                l: Box::new(self.resolve_expr(l)?),
                r: Box::new(self.resolve_expr(r)?),
                span: span.clone(),
            }),
            Expr::Logical { op, l, r, .. } => Some(RExpr::Logical {
                op: *op,
                l: Box::new(self.resolve_expr(l)?),
                r: Box::new(self.resolve_expr(r)?),
            }),
            Expr::Assign {
                target,
                op,
                value,
                span,
            } => {
                let rval = Box::new(self.resolve_expr(value)?);
                match &**target {
                    Expr::Ident(name, _) => {
                        if let Some(slot) = self.lookup(name) {
                            Some(RExpr::AssignLocal {
                                slot,
                                op: *op,
                                value: rval,
                                span: span.clone(),
                            })
                        } else {
                            Some(RExpr::AssignGlobal {
                                name: Arc::from(name.as_str()),
                                op: *op,
                                value: rval,
                                span: span.clone(),
                            })
                        }
                    }
                    Expr::Index { object, index, .. } => Some(RExpr::AssignIndex {
                        object: Box::new(self.resolve_expr(object)?),
                        index: Box::new(self.resolve_expr(index)?),
                        op: *op,
                        value: rval,
                        span: span.clone(),
                    }),
                    Expr::Field { object, name, .. } => Some(RExpr::AssignField {
                        object: Box::new(self.resolve_expr(object)?),
                        name: Arc::from(name.as_str()),
                        op: *op,
                        value: rval,
                        span: span.clone(),
                    }),
                    _ => None,
                }
            }
            Expr::Call {
                callee, args, span, ..
            } => {
                let mut rargs = Vec::with_capacity(args.len());
                for a in args {
                    rargs.push(self.resolve_expr(a)?);
                }
                if let Expr::Ident(name, _) = &**callee {
                    if collections::is_constructor(name) {
                        return Some(RExpr::Construct {
                            name: Arc::from(name.as_str()),
                            args: rargs,
                            span: span.clone(),
                        });
                    }
                }
                Some(RExpr::Call {
                    callee: Box::new(self.resolve_expr(callee)?),
                    args: rargs,
                    span: span.clone(),
                })
            }
            Expr::Method {
                object,
                name,
                args,
                span,
            } => {
                let mut rargs = Vec::with_capacity(args.len());
                for a in args {
                    rargs.push(self.resolve_expr(a)?);
                }
                Some(RExpr::Method {
                    object: Box::new(self.resolve_expr(object)?),
                    name: Arc::from(name.as_str()),
                    args: rargs,
                    span: span.clone(),
                })
            }
            Expr::Index {
                object,
                index,
                span,
            } => Some(RExpr::Index {
                object: Box::new(self.resolve_expr(object)?),
                index: Box::new(self.resolve_expr(index)?),
                span: span.clone(),
            }),
            Expr::Field { object, name, span } => Some(RExpr::Field {
                object: Box::new(self.resolve_expr(object)?),
                name: Arc::from(name.as_str()),
                span: span.clone(),
            }),
            Expr::OptField { object, name, span } => Some(RExpr::OptField {
                object: Box::new(self.resolve_expr(object)?),
                name: Arc::from(name.as_str()),
                span: span.clone(),
            }),
            Expr::New {
                class, args, span, ..
            } => {
                let mut rargs = Vec::with_capacity(args.len());
                for a in args {
                    rargs.push(self.resolve_expr(a)?);
                }
                let class_slot = self.lookup(class);
                Some(RExpr::New {
                    class_slot,
                    class_name: Arc::from(class.as_str()),
                    args: rargs,
                    span: span.clone(),
                })
            }
            Expr::Await(inner, span) => Some(RExpr::Await(
                Box::new(self.resolve_expr(inner)?),
                span.clone(),
            )),
            _ => None,
        }
    }
}

fn resolve_fn(proto: &FnProto) -> Option<ResolvedFn> {
    let mut r = Resolver::new();
    let mut defaults = Vec::with_capacity(proto.params.len());
    for p in &proto.params {
        r.declare(&p.name)?;
        defaults.push(match &p.default {
            Some(d) => Some(r.resolve_expr(d)?),
            None => None,
        });
    }
    let body = r.resolve_block(&proto.body.stmts, false)?;
    Some(ResolvedFn {
        num_slots: r.next_slot as usize,
        defaults,
        body,
    })
}

// ------------------------------------------------------------ slot executor

impl Interp {
    pub(super) fn exec_resolved_fn(
        &self,
        fd: &FunctionData,
        rfn: &ResolvedFn,
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
        let mut slots = alloc_local_slots(rfn.num_slots);
        let argc = args.len();
        for (i, v) in args.into_iter().enumerate() {
            slots[i] = v;
        }
        for (i, def) in rfn.defaults.iter().enumerate().skip(argc) {
            if let Some(d) = def {
                match self.eval_r(d, &mut slots, &self.globals) {
                    Ok(v) => slots[i] = v,
                    Err(e) => {
                        free_local_slots(slots);
                        return Err(e);
                    }
                }
            }
        }
        let mark = self.guard_mark();
        let mut poll = 0u32;
        let flow = self.exec_r_block(&rfn.body, &mut slots, &fd.env, &mut poll);
        self.release_guards(mark);
        free_local_slots(slots);
        match flow {
            Flow::Value(v) => Ok(v),
            Flow::Return(v) => Ok(v),
            Flow::Break => Err(self.rt(span, "break outside a loop")),
            Flow::Continue => Err(self.rt(span, "continue outside a loop")),
            Flow::Error(d) => Err(Flow::Error(d)),
            Flow::Fatal(d) => Err(Flow::Fatal(d)),
        }
    }

    pub(super) fn exec_resolved_fn_1(
        &self,
        fd: &FunctionData,
        rfn: &ResolvedFn,
        arg0: Value,
        span: &Span,
    ) -> Result<Value, Flow> {
        if 1 < fd.proto.min_arity() || fd.proto.params.is_empty() {
            return self.exec_resolved_fn(fd, rfn, vec![arg0], span);
        }
        let mut slots = alloc_local_slots(rfn.num_slots);
        slots[0] = arg0;
        for (i, def) in rfn.defaults.iter().enumerate().skip(1) {
            if let Some(d) = def {
                match self.eval_r(d, &mut slots, &self.globals) {
                    Ok(v) => slots[i] = v,
                    Err(e) => {
                        free_local_slots(slots);
                        return Err(e);
                    }
                }
            }
        }
        let mark = self.guard_mark();
        let mut poll = 0u32;
        let flow = self.exec_r_block(&rfn.body, &mut slots, &fd.env, &mut poll);
        self.release_guards(mark);
        free_local_slots(slots);
        match flow {
            Flow::Value(v) => Ok(v),
            Flow::Return(v) => Ok(v),
            Flow::Break => Err(self.rt(span, "break outside a loop")),
            Flow::Continue => Err(self.rt(span, "continue outside a loop")),
            Flow::Error(d) => Err(Flow::Error(d)),
            Flow::Fatal(d) => Err(Flow::Fatal(d)),
        }
    }

    #[inline(always)]
    fn eval_r_int(e: &RExpr, slots: &[Value]) -> Option<i64> {
        match e {
            RExpr::Local(s) => match slots[*s as usize] {
                Value::Int(i) => Some(i),
                _ => None,
            },
            RExpr::Int(i) => Some(*i),
            RExpr::Binary { op, l, r, .. } => {
                let a = Self::eval_r_int(l, slots)?;
                let b = Self::eval_r_int(r, slots)?;
                match op {
                    BinOp::Add => a.checked_add(b),
                    BinOp::Sub => a.checked_sub(b),
                    BinOp::Mul => a.checked_mul(b),
                    BinOp::Div if b != 0 => a.checked_div(b),
                    BinOp::Mod if b != 0 => Some(a.wrapping_rem(b)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    #[inline(always)]
    fn eval_r_atom(&self, e: &RExpr, slots: &mut [Value], env: &Env) -> Result<Value, Flow> {
        match e {
            RExpr::Local(slot) => Ok(slots[*slot as usize].clone()),
            RExpr::Int(i) => Ok(Value::Int(*i)),
            RExpr::Bool(b) => Ok(Value::Bool(*b)),
            RExpr::Str(s) => Ok(Value::Str(s.clone())),
            RExpr::Null => Ok(Value::Null),
            _ => self.eval_r(e, slots, env),
        }
    }

    #[inline(always)]
    fn eval_r_cond(&self, e: &RExpr, slots: &mut [Value], env: &Env) -> Result<bool, Flow> {
        if let RExpr::Binary { op, l, r, span } = e {
            if let (Some(a), Some(b)) = (Self::eval_r_int(l, slots), Self::eval_r_int(r, slots)) {
                match op {
                    BinOp::Lt => return Ok(a < b),
                    BinOp::Le => return Ok(a <= b),
                    BinOp::Gt => return Ok(a > b),
                    BinOp::Ge => return Ok(a >= b),
                    BinOp::Eq => return Ok(a == b),
                    BinOp::Ne => return Ok(a != b),
                    _ => {}
                }
            }
            let lv = self.eval_r_atom(l, slots, env)?;
            let rv = self.eval_r_atom(r, slots, env)?;
            if let (Value::Int(a), Value::Int(b)) = (&lv, &rv) {
                match op {
                    BinOp::Lt => return Ok(*a < *b),
                    BinOp::Le => return Ok(*a <= *b),
                    BinOp::Gt => return Ok(*a > *b),
                    BinOp::Ge => return Ok(*a >= *b),
                    BinOp::Eq => return Ok(*a == *b),
                    BinOp::Ne => return Ok(*a != *b),
                    _ => {}
                }
            }
            let v = self.eval_binary(*op, lv, rv, span)?;
            return Ok(crate::value::truthy(&v));
        }
        let v = self.eval_r_atom(e, slots, env)?;
        Ok(crate::value::truthy(&v))
    }

    fn exec_r_block(
        &self,
        stmts: &[RStmt],
        slots: &mut [Value],
        env: &Env,
        poll: &mut u32,
    ) -> Flow {
        for s in stmts {
            if self.budget.is_some() {
                if let Some(flow) = self.charge_budget(s.span()) {
                    return flow;
                }
            }
            if self.wall_clock.is_some() || self.mem_limit.is_some() {
                if let Some(flow) = self.check_limits(s.span()) {
                    return flow;
                }
            }
            *poll = poll.wrapping_add(1);
            if (*poll & 63) == 0 {
                crate::value::gc::maybe_collect();
                if crate::sched::current_task_cancelled() {
                    return self.rt(s.span(), "task cancelled");
                }
            }
            match self.exec_r_stmt(s, slots, env, poll) {
                Flow::Value(_) => continue,
                other => return other,
            }
        }
        Flow::Value(Value::Null)
    }

    fn exec_r_stmt(&self, s: &RStmt, slots: &mut [Value], env: &Env, poll: &mut u32) -> Flow {
        match s {
            RStmt::Let { slot, value, .. } => {
                let v = match value {
                    Some(e) => match self.eval_r_atom(e, slots, env) {
                        Ok(v) => v,
                        Err(f) => return f,
                    },
                    None => Value::Null,
                };
                slots[*slot as usize] = v;
                Flow::Value(Value::Null)
            }
            RStmt::If {
                cond,
                then,
                elifs,
                els,
                ..
            } => {
                let c = match self.eval_r_cond(cond, slots, env) {
                    Ok(b) => b,
                    Err(f) => return f,
                };
                if c {
                    let mark = self.guard_mark();
                    let r = self.exec_r_block(then, slots, env, poll);
                    self.release_guards(mark);
                    return r;
                }
                for (ec, eb) in elifs {
                    let c = match self.eval_r_cond(ec, slots, env) {
                        Ok(b) => b,
                        Err(f) => return f,
                    };
                    if c {
                        let mark = self.guard_mark();
                        let r = self.exec_r_block(eb, slots, env, poll);
                        self.release_guards(mark);
                        return r;
                    }
                }
                if let Some(eb) = els {
                    let mark = self.guard_mark();
                    let r = self.exec_r_block(eb, slots, env, poll);
                    self.release_guards(mark);
                    return r;
                }
                Flow::Value(Value::Null)
            }
            RStmt::While { cond, body, span } => loop {
                if self.budget.is_some() {
                    if let Some(flow) = self.charge_budget(span) {
                        return flow;
                    }
                }
                if self.wall_clock.is_some() || self.mem_limit.is_some() {
                    if let Some(flow) = self.check_limits(span) {
                        return flow;
                    }
                }
                let c = match self.eval_r_cond(cond, slots, env) {
                    Ok(b) => b,
                    Err(f) => return f,
                };
                if !c {
                    return Flow::Value(Value::Null);
                }
                let mark = self.guard_mark();
                let res = self.exec_r_block(body, slots, env, poll);
                self.release_guards(mark);
                match res {
                    Flow::Value(_) | Flow::Continue => {}
                    Flow::Break => return Flow::Value(Value::Null),
                    other => return other,
                }
            },
            RStmt::ForIn {
                slot,
                iter,
                body,
                span,
            } => {
                let items = match self.eval_r(iter, slots, env) {
                    Ok(v) => match collections::iter_values(&v) {
                        Ok(items) => items,
                        Err(msg) => return self.rt(span, msg),
                    },
                    Err(f) => return f,
                };
                for item in items {
                    if self.budget.is_some() {
                        if let Some(flow) = self.charge_budget(span) {
                            return flow;
                        }
                    }
                    if self.wall_clock.is_some() || self.mem_limit.is_some() {
                        if let Some(flow) = self.check_limits(span) {
                            return flow;
                        }
                    }
                    slots[*slot as usize] = item;
                    let mark = self.guard_mark();
                    let res = self.exec_r_block(body, slots, env, poll);
                    self.release_guards(mark);
                    match res {
                        Flow::Value(_) | Flow::Continue => {}
                        Flow::Break => return Flow::Value(Value::Null),
                        other => return other,
                    }
                }
                Flow::Value(Value::Null)
            }
            RStmt::ForC {
                init,
                cond,
                post,
                body,
                span,
            } => {
                match self.exec_r_stmt(init, slots, env, poll) {
                    Flow::Value(_) => {}
                    other => return other,
                }
                loop {
                    let c = match self.eval_r_cond(cond, slots, env) {
                        Ok(b) => b,
                        Err(f) => return f,
                    };
                    if !c {
                        return Flow::Value(Value::Null);
                    }
                    if self.wall_clock.is_some() || self.mem_limit.is_some() {
                        if let Some(flow) = self.check_limits(span) {
                            return flow;
                        }
                    }
                    let mark = self.guard_mark();
                    let body_flow = self.exec_r_block(body, slots, env, poll);
                    self.release_guards(mark);
                    match body_flow {
                        Flow::Value(_) | Flow::Continue => {}
                        Flow::Break => return Flow::Value(Value::Null),
                        other => return other,
                    }
                    if let Err(f) = self.eval_r(post, slots, env) {
                        return f;
                    }
                }
            }
            RStmt::Return { value, .. } => {
                let v = match value {
                    Some(e) => match self.eval_r_atom(e, slots, env) {
                        Ok(v) => v,
                        Err(f) => return f,
                    },
                    None => Value::Null,
                };
                Flow::Return(v)
            }
            RStmt::Break(_) => Flow::Break,
            RStmt::Continue(_) => Flow::Continue,
            RStmt::Block(stmts, _) => {
                let mark = self.guard_mark();
                let r = self.exec_r_block(stmts, slots, env, poll);
                self.release_guards(mark);
                r
            }
            RStmt::Expr(e, _) => {
                // Fast statement-level local assignment (`x = expr;`): avoids
                // cloning the assigned value just to discard it.
                if let RExpr::AssignLocal {
                    slot,
                    op: AssignOp::Assign,
                    value,
                    ..
                } = e
                {
                    if let Some(iv) = Self::eval_r_int(value, slots) {
                        slots[*slot as usize] = Value::Int(iv);
                        return Flow::Value(Value::Null);
                    }
                    let v = match self.eval_r_atom(value, slots, env) {
                        Ok(v) => v,
                        Err(f) => return f,
                    };
                    slots[*slot as usize] = v;
                    return Flow::Value(Value::Null);
                }
                match self.eval_r(e, slots, env) {
                    Ok(v) => Flow::Value(v),
                    Err(f) => f,
                }
            }
        }
    }

    fn eval_r(&self, e: &RExpr, slots: &mut [Value], env: &Env) -> Result<Value, Flow> {
        match e {
            RExpr::Int(i) => Ok(Value::Int(*i)),
            RExpr::Float(f) => Ok(Value::Float(*f)),
            RExpr::Bool(b) => Ok(Value::Bool(*b)),
            RExpr::Null => Ok(Value::Null),
            RExpr::NoneVal => Ok(Value::Option(None)),
            RExpr::Str(s) => Ok(Value::Str(s.clone())),
            RExpr::Local(slot) => Ok(slots[*slot as usize].clone()),
            RExpr::Global(name, span) => match cached_global_get(env, name) {
                Some(v) => Ok(v),
                None => Err(self.rt(span, format!("unknown name '{}'", name))),
            },
            RExpr::List(items, span) => {
                let mut vals = Vec::with_capacity(items.len());
                for it in items {
                    vals.push(self.eval_r(it, slots, env)?);
                }
                collections::construct("list", vals).map_err(|msg| self.rt(span, msg))
            }
            RExpr::Unary { op, expr, span } => {
                let v = self.eval_r(expr, slots, env)?;
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
            RExpr::Binary { op, l, r, span } => {
                let lv = self.eval_r_atom(l, slots, env)?;
                let rv = self.eval_r_atom(r, slots, env)?;
                if let (Value::Int(a), Value::Int(b)) = (&lv, &rv) {
                    match op {
                        BinOp::Add => {
                            return match a.checked_add(*b) {
                                Some(v) => Ok(Value::Int(v)),
                                None => {
                                    Err(self.rt(span, format!("integer overflow: {} + {}", a, b)))
                                }
                            };
                        }
                        BinOp::Sub => {
                            return match a.checked_sub(*b) {
                                Some(v) => Ok(Value::Int(v)),
                                None => {
                                    Err(self.rt(span, format!("integer overflow: {} - {}", a, b)))
                                }
                            };
                        }
                        BinOp::Mul => {
                            return match a.checked_mul(*b) {
                                Some(v) => Ok(Value::Int(v)),
                                None => {
                                    Err(self.rt(span, format!("integer overflow: {} * {}", a, b)))
                                }
                            };
                        }
                        BinOp::Div if *b != 0 => {
                            return match a.checked_div(*b) {
                                Some(v) => Ok(Value::Int(v)),
                                None => {
                                    Err(self.rt(span, format!("integer overflow: {} / {}", a, b)))
                                }
                            };
                        }
                        BinOp::Mod if *b != 0 => {
                            return Ok(Value::Int(a.wrapping_rem(*b)));
                        }
                        BinOp::Lt => return Ok(Value::Bool(*a < *b)),
                        BinOp::Le => return Ok(Value::Bool(*a <= *b)),
                        BinOp::Gt => return Ok(Value::Bool(*a > *b)),
                        BinOp::Ge => return Ok(Value::Bool(*a >= *b)),
                        BinOp::Eq => return Ok(Value::Bool(*a == *b)),
                        BinOp::Ne => return Ok(Value::Bool(*a != *b)),
                        _ => {}
                    }
                }
                self.eval_binary(*op, lv, rv, span)
            }
            RExpr::Logical { op, l, r } => {
                let lv = self.eval_r(l, slots, env)?;
                match op {
                    LogOp::And => {
                        if !crate::value::truthy(&lv) {
                            return Ok(Value::Bool(false));
                        }
                        let rv = self.eval_r(r, slots, env)?;
                        Ok(Value::Bool(crate::value::truthy(&rv)))
                    }
                    LogOp::Or => {
                        if crate::value::truthy(&lv) {
                            return Ok(Value::Bool(true));
                        }
                        let rv = self.eval_r(r, slots, env)?;
                        Ok(Value::Bool(crate::value::truthy(&rv)))
                    }
                }
            }
            RExpr::AssignLocal {
                slot,
                op,
                value,
                span,
            } => {
                let v = self.eval_r(value, slots, env)?;
                let idx = *slot as usize;
                let new_v = if *op == AssignOp::Assign {
                    v
                } else {
                    let old = slots[idx].clone();
                    self.compound(*op, old, v, span)?
                };
                slots[idx] = new_v.clone();
                Ok(new_v)
            }
            RExpr::AssignGlobal {
                name,
                op,
                value,
                span,
            } => {
                let v = self.eval_r(value, slots, env)?;
                let new_v = if *op == AssignOp::Assign {
                    v
                } else {
                    let old = match cached_global_get(env, name) {
                        Some(v) => v,
                        None => return Err(self.rt(span, format!("unknown name '{}'", name))),
                    };
                    self.compound(*op, old, v, span)?
                };
                if let Err(e) = environment::assign(env, name.as_ref(), new_v.clone()) {
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
            RExpr::AssignIndex {
                object,
                index,
                op,
                value,
                span,
            } => {
                let v = self.eval_r(value, slots, env)?;
                let obj = self.eval_r(object, slots, env)?;
                let idx = self.eval_r(index, slots, env)?;
                let new_v = if *op == AssignOp::Assign {
                    v
                } else {
                    let old = self.index_get(&obj, &idx, span)?;
                    self.compound(*op, old, v, span)?
                };
                self.index_set(&obj, &idx, new_v.clone(), span)?;
                Ok(new_v)
            }
            RExpr::AssignField {
                object,
                name,
                op,
                value,
                span,
            } => {
                let v = self.eval_r(value, slots, env)?;
                let obj = self.eval_r(object, slots, env)?;
                let new_v = if *op == AssignOp::Assign {
                    v
                } else {
                    let old = self.field_get(&obj, name.as_ref(), span)?;
                    self.compound(*op, old, v, span)?
                };
                self.field_set(&obj, name.as_ref(), new_v.clone(), span)?;
                Ok(new_v)
            }
            RExpr::Construct { name, args, span } => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval_r(a, slots, env)?);
                }
                collections::construct(name.as_ref(), vals).map_err(|msg| self.rt(span, msg))
            }
            RExpr::Call { callee, args, span } => {
                let f = self.eval_r(callee, slots, env)?;
                if args.len() == 1 {
                    match &f {
                        Value::Function(fd) => {
                            let arg = self.eval_r_atom(&args[0], slots, env)?;
                            return self.call_function_arc_1(fd, arg, span);
                        }
                        Value::Native(n) => {
                            if std::ptr::fn_addr_eq(n.func, crate::stdlib::inline_string_impl()) {
                                let arg = self.eval_r_atom(&args[0], slots, env)?;
                                let s = match arg {
                                    Value::Int(i) => crate::stdlib::int_to_string_value(i),
                                    Value::Str(s) => s,
                                    other => crate::value::display_value(&other).into(),
                                };
                                if self.wall_clock.is_some() || self.mem_limit.is_some() {
                                    if let Some(flow) = self.check_limits(span) {
                                        return Err(flow);
                                    }
                                }
                                return Ok(Value::Str(s));
                            } else if std::ptr::fn_addr_eq(n.func, crate::stdlib::inline_len_impl())
                            {
                                let arg = self.eval_r_atom(&args[0], slots, env)?;
                                let len =
                                    collections::len_of(&arg).map_err(|m| self.rt(span, m))?;
                                if self.wall_clock.is_some() || self.mem_limit.is_some() {
                                    if let Some(flow) = self.check_limits(span) {
                                        return Err(flow);
                                    }
                                }
                                return Ok(Value::Int(len as i64));
                            }
                        }
                        _ => {}
                    }
                }
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval_r_atom(a, slots, env)?);
                }
                self.call_value(f, vals, span)
            }
            RExpr::Method {
                object,
                name,
                args,
                span,
            } => {
                let obj = self.eval_r(object, slots, env)?;
                if let Value::MutexVal(m) = &obj {
                    if name.as_ref() == "lock" {
                        if !args.is_empty() {
                            return Err(self.rt(span, "lock takes no arguments"));
                        }
                        let v = m.lock().map_err(|e| self.rt(span, e))?;
                        let mut guards = self.guards.lock().unwrap();
                        guards.push(crate::sched::MutexGuardToken::new(m.clone()));
                        self.guards_len
                            .store(guards.len(), std::sync::atomic::Ordering::Relaxed);
                        return Ok(v);
                    }
                    if name.as_ref() == "unlock" {
                        if !args.is_empty() {
                            return Err(self.rt(span, "unlock takes no arguments"));
                        }
                        m.unlock();
                        let mut guards = self.guards.lock().unwrap();
                        for g in guards.iter_mut() {
                            if let Some(held) = g.mutex() {
                                if Arc::ptr_eq(held, m) {
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
                        vals.push(self.eval_r(a, slots, env)?);
                    }
                    return self.call_instance_method(inst, name.as_ref(), vals, span);
                }
                if let Value::Class(class) = &obj {
                    let mut vals = Vec::with_capacity(args.len());
                    for a in args {
                        vals.push(self.eval_r(a, slots, env)?);
                    }
                    return self.call_class_method(class, obj.clone(), name.as_ref(), vals, span);
                }
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval_r(a, slots, env)?);
                }
                let mut host = InterpCallbackHost {
                    interp: self,
                    span: span.clone(),
                };
                match collections::call_method_host(&mut host, &obj, name.as_ref(), vals) {
                    Ok(v) => Ok(v),
                    Err(msg) => {
                        let mut d = Diagnostic::at(Phase::Runtime, span.clone(), msg)
                            .with_trace(self.trace_frames(span));
                        if let Some(v) = crate::sched::take_error_thrown() {
                            d.thrown = Some(v);
                        }
                        Err(Flow::Error(Box::new(d)))
                    }
                }
            }
            RExpr::Index {
                object,
                index,
                span,
            } => {
                let obj = self.eval_r(object, slots, env)?;
                let idx = self.eval_r(index, slots, env)?;
                match (&obj, &idx) {
                    (Value::Map(m), Value::Str(k)) => {
                        let guard = crate::value::lock_ref(m);
                        match guard.find_str(k.as_ref()) {
                            Some(i) => Ok(guard.entries[i].1.clone()),
                            None => Err(self.rt(span, format!("missing key {}", k))),
                        }
                    }
                    (Value::List(d) | Value::VecDyn(d), Value::Int(n)) => {
                        let guard = crate::value::lock_ref(d);
                        let len = guard.items.len();
                        let resolved = if *n < 0 { len as i64 + *n } else { *n };
                        if resolved >= 0 && (resolved as usize) < len {
                            Ok(guard.items[resolved as usize].clone())
                        } else {
                            Err(self.rt(
                                span,
                                format!("index {} out of bounds for length {}", n, len),
                            ))
                        }
                    }
                    _ => self.index_get(&obj, &idx, span),
                }
            }
            RExpr::Field { object, name, span } => {
                let obj = self.eval_r(object, slots, env)?;
                self.field_get(&obj, name.as_ref(), span)
            }
            RExpr::OptField { object, name, span } => {
                let obj = self.eval_r(object, slots, env)?;
                match &obj {
                    Value::Null => Ok(Value::Null),
                    Value::Map(_) => match self.field_get(&obj, name.as_ref(), span) {
                        Ok(v) => Ok(v),
                        Err(_) => Ok(Value::Null),
                    },
                    _ => self.field_get(&obj, name.as_ref(), span),
                }
            }
            RExpr::New {
                class_slot,
                class_name,
                args,
                span,
            } => {
                let val_opt = match class_slot {
                    Some(slot) => Some(slots[*slot as usize].clone()),
                    None => cached_global_get(env, class_name),
                };
                let c = match val_opt {
                    Some(Value::Class(c)) => c,
                    Some(_) => {
                        return Err(self.rt(span, format!("'{}' is not a class", class_name)))
                    }
                    None => return Err(self.rt(span, format!("unknown class '{}'", class_name))),
                };
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval_r_atom(a, slots, env)?);
                }
                self.new_instance(&c, vals, span)
            }
            RExpr::Await(inner, span) => {
                let v = self.eval_r(inner, slots, env)?;
                match v {
                    Value::Task(t) => match t.join() {
                        Ok(v) => Ok(v),
                        Err(msg) => {
                            let mut d = Diagnostic::at(
                                Phase::Runtime,
                                span.clone(),
                                format!("task failed: {}", msg),
                            )
                            .with_trace(self.trace_frames(span));
                            if let Some(v) = t.thrown_value() {
                                d.thrown = Some(v);
                            }
                            Err(Flow::Error(Box::new(d)))
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
}
