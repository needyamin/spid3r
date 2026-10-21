//! AST lowering: the single seam between surface syntax and the backends.
//!
//! Every backend — the tree interpreter, the bytecode VM and the LLVM AOT
//! backend — consumes the AST produced here, never the parser's output
//! directly. This is what lets a *syntax* feature be written once as a
//! desugaring instead of three times as an engine feature.
//!
//! # What this pass rewrites
//!
//! Three surface nodes need a **conditional expression**, which the language does
//! not have at the engine level:
//!
//! | Surface | Lowered to |
//! |---|---|
//! | `if (c) { a } else { b }` | `let __t = null; if (c) { __t = a } else { __t = b }` |
//! | `obj?.field` | `let __o = obj; let __t = null; if (__o != null) { __t = __o.field }` |
//! | `a ?? b` | `let __t = a; if (__t == null) { __t = b }` |
//!
//! # The hoisting rule
//!
//! A rewrite produces statements that must run where the expression was. Those
//! statements are hoisted to the enclosing statement, which is only sound if
//! evaluation order is preserved. Consider
//!
//! ```spidr
//! g(h(), if (c) { a } else { b })
//! ```
//!
//! Hoisting the `if` before the call would evaluate `c`/`a`/`b` *before* `h()`,
//! changing the order of side effects. So whenever any child of a composite
//! expression needs a prelude, **every** child is spilled into a temporary in
//! source order:
//!
//! ```spidr
//! let __t0 = h();
//! let __t1 = null; if (c) { __t1 = a } else { __t1 = b }
//! g(__t0, __t1)
//! ```
//!
//! This is ANF, but only where it is needed: a statement containing none of the
//! three nodes is copied through untouched, and `lower` returns
//! `Cow::Borrowed` for a whole program that contains none of them, so existing
//! code pays nothing.
//!
//! # Control flow that must not become eager
//!
//! `and`/`or` short-circuit and return the *operand*, not a bool, so they are
//! never spilled naively. If either side needs a prelude they become:
//!
//! ```spidr
//! let __t = <left>; if (bool(__t)) { __t = <right> }   // and
//! let __t = <left>; if (not bool(__t)) { __t = <right> } // or
//! ```
//!
//! A `while` condition with a prelude becomes `while (true) { <prelude>;
//! if (not cond) { break } ... }`, so the condition is still evaluated once per
//! iteration.
//!
//! # Contract
//!
//! * **Idempotent** — a backend may be handed an already-lowered program (the VM
//!   compiles module programs through the same entry point), and lowered output
//!   contains none of the three nodes.
//! * **Span-preserving** — synthesized statements carry the span of the
//!   construct they replace, so diagnostics and stack traces still point at the
//!   source the user wrote.
//! * **Behaviour-preserving** — `tests/engine_matrix.rs` runs every construct on
//!   every engine against the expected output.

use std::borrow::Cow;

use crate::ast::{AssignOp, BinOp, Block, ClassDecl, Expr, FnProto, Program, Stmt, UnOp, Vis};
use crate::diagnostics::Span;

/// Lower a parsed program into the form the backends execute.
pub fn lower(program: &Program) -> Cow<'_, Program> {
    // The fast path: a program with no surface-only node is passed through by
    // reference, so nothing is copied and nothing can change.
    if !program_needs_lowering(program) {
        return Cow::Borrowed(program);
    }
    let mut l = Lower {
        next_temp: 0,
        current_parent: None,
        variants: std::collections::HashMap::new(),
    };
    Cow::Owned(l.program(program))
}

/// Lower an owned program, cloning only if a rule fires.
pub fn lower_owned(program: Program) -> Program {
    match lower(&program) {
        Cow::Borrowed(_) => program,
        Cow::Owned(rewritten) => rewritten,
    }
}

// --------------------------------------------------------------------- scan

/// Does this program contain a node that only the lowering pass understands?
fn program_needs_lowering(p: &Program) -> bool {
    p.stmts.iter().any(stmt_needs_lowering)
}

fn stmt_needs_lowering(s: &Stmt) -> bool {
    match s {
        Stmt::Import { .. } | Stmt::Interface(_) | Stmt::Break { .. } | Stmt::Continue { .. } => {
            false
        }
        // An enum always needs lowering: its declaration becomes constructor
        // functions, and a match always needs lowering into an arm chain.
        Stmt::Enum(_) => true,
        Stmt::Fn(f) => block_needs_lowering(&f.body),
        Stmt::Class(c) => c.methods.iter().any(|m| block_needs_lowering(&m.body)),
        Stmt::Let { value, .. } => value.as_ref().map(expr_needs_lowering).unwrap_or(false),
        Stmt::Const { value, .. } => expr_needs_lowering(value),
        // Both always need lowering: they expand into ordinary statements.
        Stmt::LetTuple { .. } => true,
        Stmt::If {
            cond,
            then,
            elifs,
            els,
            ..
        } => {
            expr_needs_lowering(cond)
                || block_needs_lowering(then)
                || elifs
                    .iter()
                    .any(|(c, b)| expr_needs_lowering(c) || block_needs_lowering(b))
                || els.as_ref().map(block_needs_lowering).unwrap_or(false)
        }
        Stmt::While { cond, body, .. } => expr_needs_lowering(cond) || block_needs_lowering(body),
        // `for (k, v)` must be rewritten even when nothing else needs lowering:
        // the engines only know the one-variable form, and the value binding is
        // synthesized here.
        Stmt::ForIn {
            var2, iter, body, ..
        } => var2.is_some() || expr_needs_lowering(iter) || block_needs_lowering(body),
        Stmt::ForC {
            init,
            cond,
            post,
            body,
            ..
        } => {
            stmt_needs_lowering(init)
                || expr_needs_lowering(cond)
                || expr_needs_lowering(post)
                || block_needs_lowering(body)
        }
        Stmt::Return { value, .. } => value.as_ref().map(expr_needs_lowering).unwrap_or(false),
        Stmt::Block(b) => block_needs_lowering(b),
        Stmt::Expr(e) => expr_needs_lowering(e),
        Stmt::Spawn { callable, .. } => expr_needs_lowering(callable),
        Stmt::Try {
            body,
            catch,
            finally,
            ..
        } => {
            block_needs_lowering(body)
                || catch
                    .as_ref()
                    .map(|c| block_needs_lowering(&c.body))
                    .unwrap_or(false)
                || finally.as_ref().map(block_needs_lowering).unwrap_or(false)
        }
        Stmt::Throw { value, .. } => expr_needs_lowering(value),
    }
}

fn block_needs_lowering(b: &Block) -> bool {
    b.stmts.iter().any(stmt_needs_lowering)
}

fn expr_needs_lowering(e: &Expr) -> bool {
    match e {
        Expr::If { .. }
        | Expr::OptField { .. }
        | Expr::Coalesce { .. }
        | Expr::Match { .. }
        | Expr::Propagate(..) => true,
        // `super` is rewritten into a resolved `SuperCall`.
        Expr::Super { .. } => true,
        Expr::Int(..)
        | Expr::Float(..)
        | Expr::Str(..)
        | Expr::Bool(..)
        | Expr::Null(..)
        | Expr::Ident(..) => false,
        // An already-resolved `SuperCall` needs no further lowering.
        Expr::SuperCall { .. } => false,
        Expr::List(items, _) => items.iter().any(expr_needs_lowering),
        Expr::Fn(f) => block_needs_lowering(&f.body),
        Expr::Unary { expr, .. } => expr_needs_lowering(expr),
        Expr::Binary { l, r, .. } | Expr::Logical { l, r, .. } => {
            expr_needs_lowering(l) || expr_needs_lowering(r)
        }
        Expr::Assign { target, value, .. } => {
            expr_needs_lowering(target) || expr_needs_lowering(value)
        }
        Expr::Call { callee, args, .. } => {
            expr_needs_lowering(callee) || args.iter().any(expr_needs_lowering)
        }
        Expr::Method { object, args, .. } => {
            expr_needs_lowering(object) || args.iter().any(expr_needs_lowering)
        }
        Expr::Index { object, index, .. } => {
            expr_needs_lowering(object) || expr_needs_lowering(index)
        }
        Expr::Field { object, .. } => expr_needs_lowering(object),
        Expr::New { args, .. } => args.iter().any(expr_needs_lowering),
        Expr::Await(inner, _) => expr_needs_lowering(inner),
        Expr::Spawn { callable, .. } => expr_needs_lowering(callable),
    }
}

// ------------------------------------------------------------------- lowering

struct Lower {
    next_temp: usize,
    /// The parent class of the class whose body is being lowered. `super`
    /// resolves against it: `super.m(...)` calls the parent's implementation
    /// of `m`, `super(...)` calls the parent's constructor.
    current_parent: Option<String>,
    /// Variant name -> payload field names, collected from the program's own
    /// `enum` declarations. A match needs the field names to bind them.
    variants: std::collections::HashMap<String, Vec<String>>,
}

impl Lower {
    fn fresh(&mut self) -> String {
        let n = self.next_temp;
        self.next_temp += 1;
        format!("__spid3r_t{n}")
    }

    fn program(&mut self, p: &Program) -> Program {
        let mut stmts = Vec::new();
        for s in &p.stmts {
            stmts.extend(self.stmt(s));
        }
        Program { stmts }
    }

    /// Lower one statement into one or more statements (a prelude plus itself).
    fn stmt(&mut self, s: &Stmt) -> Vec<Stmt> {
        match s {
            Stmt::Fn(f) => {
                let mut out = vec![Stmt::Fn(self.fn_proto(f))];
                out.retain(|_| true);
                out
            }
            Stmt::Class(c) => vec![Stmt::Class(self.class_decl(c))],
            Stmt::Enum(e) => self.enum_decl(e),
            Stmt::Interface(_)
            | Stmt::Import { .. }
            | Stmt::Break { .. }
            | Stmt::Continue { .. } => {
                vec![s.clone()]
            }
            Stmt::Let {
                vis,
                name,
                ty,
                value,
                span,
            } => match value {
                Some(v) => {
                    let (value, pre) = self.expr(v);
                    let mut out = pre;
                    out.push(Stmt::Let {
                        vis: *vis,
                        name: name.clone(),
                        ty: ty.clone(),
                        value: Some(value),
                        span: span.clone(),
                    });
                    out
                }
                None => vec![s.clone()],
            },
            // `let (a, b) = e;` -> `let __d = e; let a = __d[0]; let b = __d[1];`
            Stmt::LetTuple {
                vis,
                names,
                value,
                span,
            } => {
                let (value, mut pre) = self.expr(value);
                let d = self.fresh();
                pre.push(let_stmt(&d, value, span.clone()));
                for (i, n) in names.iter().enumerate() {
                    pre.push(Stmt::Let {
                        vis: *vis,
                        name: n.clone(),
                        ty: None,
                        value: Some(Expr::Index {
                            object: Box::new(Expr::Ident(d.clone(), span.clone())),
                            index: Box::new(Expr::Int(i as i64, span.clone())),
                            span: span.clone(),
                        }),
                        span: span.clone(),
                    });
                }
                pre
            }
            Stmt::Const {
                vis,
                name,
                ty,
                value,
                span,
            } => {
                let (value, pre) = self.expr(value);
                let mut out = pre;
                out.push(Stmt::Const {
                    vis: *vis,
                    name: name.clone(),
                    ty: ty.clone(),
                    value,
                    span: span.clone(),
                });
                out
            }
            Stmt::If {
                cond,
                then,
                elifs,
                els,
                span,
            } => self.if_stmt(cond, then, elifs, els.as_ref(), span),
            Stmt::While { cond, body, span } => self.while_stmt(cond, body, span),
            Stmt::ForIn {
                var,
                var2,
                iter,
                body,
                span,
            } => {
                let (iter, mut pre) = self.expr(iter);
                let mut body = self.block(body);
                // `for (k, v) in m` -> `for k in m { let v = m[k]; … }`. The
                // iterable expression is evaluated once into a temp so the
                // lookup does not repeat it.
                if let Some(v2) = var2 {
                    let src = self.fresh();
                    pre.push(let_stmt(&src, iter.clone(), span.clone()));
                    body.stmts.insert(
                        0,
                        Stmt::Let {
                            vis: Vis::Private,
                            name: v2.clone(),
                            ty: None,
                            value: Some(Expr::Index {
                                object: Box::new(Expr::Ident(src.clone(), span.clone())),
                                index: Box::new(Expr::Ident(var.clone(), span.clone())),
                                span: span.clone(),
                            }),
                            span: span.clone(),
                        },
                    );
                    pre.push(Stmt::ForIn {
                        var: var.clone(),
                        var2: None,
                        iter: Expr::Ident(src, span.clone()),
                        body,
                        span: span.clone(),
                    });
                    return pre;
                }
                pre.push(Stmt::ForIn {
                    var: var.clone(),
                    var2: None,
                    iter,
                    body,
                    span: span.clone(),
                });
                pre
            }
            Stmt::ForC {
                init,
                cond,
                post,
                body,
                span,
            } => self.for_c_stmt(init, cond, post, body, span),
            Stmt::Return { value, span } => match value {
                Some(v) => {
                    let (value, mut pre) = self.expr(v);
                    pre.push(Stmt::Return {
                        value: Some(value),
                        span: span.clone(),
                    });
                    pre
                }
                None => vec![s.clone()],
            },
            Stmt::Block(b) => vec![Stmt::Block(self.block(b))],
            Stmt::Expr(e) => {
                let (e, mut pre) = self.expr(e);
                pre.push(Stmt::Expr(e));
                pre
            }
            Stmt::Spawn { callable, span } => {
                let (callable, mut pre) = self.expr(callable);
                pre.push(Stmt::Spawn {
                    callable,
                    span: span.clone(),
                });
                pre
            }
            Stmt::Try {
                body,
                catch,
                finally,
                span,
            } => vec![Stmt::Try {
                body: self.block(body),
                catch: catch.as_ref().map(|c| crate::ast::CatchClause {
                    name: c.name.clone(),
                    body: self.block(&c.body),
                    span: c.span.clone(),
                }),
                finally: finally.as_ref().map(|b| self.block(b)),
                span: span.clone(),
            }],
            Stmt::Throw { value, span } => {
                let (value, mut pre) = self.expr(value);
                pre.push(Stmt::Throw {
                    value,
                    span: span.clone(),
                });
                pre
            }
        }
    }

    fn block(&mut self, b: &Block) -> Block {
        let mut stmts = Vec::new();
        for s in &b.stmts {
            stmts.extend(self.stmt(s));
        }
        Block {
            stmts,
            span: b.span.clone(),
        }
    }

    fn fn_proto(&mut self, f: &FnProto) -> FnProto {
        FnProto {
            body: self.block(&f.body),
            ..f.clone()
        }
    }

    fn class_decl(&mut self, c: &ClassDecl) -> ClassDecl {
        // A method body may use `super`, which resolves against this class's
        // parent. The parent name is known here (the `extends` clause), so the
        // lowering can resolve `super` without the checker rewriting the AST.
        let outer_parent = self.current_parent.clone();
        self.current_parent = c.parent.clone();
        let methods = c.methods.iter().map(|m| self.fn_proto(m)).collect();
        self.current_parent = outer_parent;
        ClassDecl { methods, ..c.clone() }
    }

    /// `if` as a statement. Elif arms whose condition needs a prelude are
    /// rewritten as nested `else { <prelude>; if ... }`, because a prelude
    /// cannot sit between two arms.
    fn if_stmt(
        &mut self,
        cond: &Expr,
        then: &Block,
        elifs: &[(Expr, Block)],
        els: Option<&Block>,
        span: &Span,
    ) -> Vec<Stmt> {
        let (cond, mut pre) = self.expr(cond);
        let then = self.block(then);
        let mut lowered_elifs: Vec<(Expr, Block)> = Vec::new();
        let mut tail_els: Option<Block> = els.map(|b| self.block(b));

        for (c, b) in elifs {
            let (lc, lpre) = self.expr(c);
            let lb = self.block(b);
            if lpre.is_empty() {
                lowered_elifs.push((lc, lb));
            } else {
                // Everything after this arm nests inside the else block.
                let mut inner: Vec<Stmt> = lpre;
                inner.push(Stmt::If {
                    cond: lc,
                    then: lb,
                    elifs: Vec::new(),
                    els: tail_els.take(),
                    span: span.clone(),
                });
                tail_els = Some(Block {
                    stmts: inner,
                    span: span.clone(),
                });
            }
        }

        pre.push(Stmt::If {
            cond,
            then,
            elifs: lowered_elifs,
            els: tail_els,
            span: span.clone(),
        });
        pre
    }

    /// A `while` whose condition needs a prelude becomes
    /// `while (true) { <prelude>; if (not cond) { break } <body> }`, so the
    /// condition still runs once per iteration.
    fn while_stmt(&mut self, cond: &Expr, body: &Block, span: &Span) -> Vec<Stmt> {
        let (cond, pre) = self.expr(cond);
        if pre.is_empty() {
            return vec![Stmt::While {
                cond,
                body: self.block(body),
                span: span.clone(),
            }];
        }
        let mut stmts = pre;
        stmts.push(Stmt::If {
            cond: Expr::Unary {
                op: UnOp::Not,
                expr: Box::new(cond),
                span: span.clone(),
            },
            then: Block {
                stmts: vec![Stmt::Break { span: span.clone() }],
                span: span.clone(),
            },
            elifs: Vec::new(),
            els: None,
            span: span.clone(),
        });
        stmts.extend(self.block(body).stmts);
        vec![Stmt::While {
            cond: Expr::Bool(true, span.clone()),
            body: Block {
                stmts,
                span: body.span.clone(),
            },
            span: span.clone(),
        }]
    }

    /// `for let i = …; cond; post { body }`. With preludes in the condition or
    /// the post expression this becomes the equivalent `while (true)` loop, so
    /// each part still runs at the right moment.
    fn for_c_stmt(
        &mut self,
        init: &Stmt,
        cond: &Expr,
        post: &Expr,
        body: &Block,
        span: &Span,
    ) -> Vec<Stmt> {
        let init_stmts = self.stmt(init);
        let (cond, cond_pre) = self.expr(cond);
        let (post, post_pre) = self.expr(post);
        let lowered_body = self.block(body);

        if cond_pre.is_empty() && post_pre.is_empty() {
            let mut out = init_stmts;
            out.push(Stmt::ForC {
                init: Box::new(Stmt::Block(Block {
                    stmts: Vec::new(),
                    span: span.clone(),
                })),
                cond,
                post,
                body: lowered_body,
                span: span.clone(),
            });
            return out;
        }

        let mut loop_body: Vec<Stmt> = cond_pre;
        loop_body.push(Stmt::If {
            cond: Expr::Unary {
                op: UnOp::Not,
                expr: Box::new(cond),
                span: span.clone(),
            },
            then: Block {
                stmts: vec![Stmt::Break { span: span.clone() }],
                span: span.clone(),
            },
            elifs: Vec::new(),
            els: None,
            span: span.clone(),
        });
        loop_body.extend(lowered_body.stmts);
        loop_body.extend(post_pre);
        loop_body.push(Stmt::Expr(post));

        let mut out = init_stmts;
        out.push(Stmt::While {
            cond: Expr::Bool(true, span.clone()),
            body: Block {
                stmts: loop_body,
                span: body.span.clone(),
            },
            span: span.clone(),
        });
        out
    }

    /// `enum Name { A, B(x) }` → one constructor function per variant.
    ///
    /// A variant is a `map` with a `__tag` key plus one key per payload field,
    /// and the constructor is an ordinary **public** function. That choice is
    /// what makes the feature free for the engines and for the module system:
    /// `Circle(1)` is a normal call, so it works across `import` with no
    /// name-resolution step, and `match` only reads a map key.
    fn enum_decl(&mut self, e: &crate::ast::EnumDecl) -> Vec<Stmt> {
        let mut out = Vec::new();
        for v in &e.variants {
            self.variants.insert(v.name.clone(), v.fields.clone());
            let span = v.span.clone();
            let mut body: Vec<Stmt> = Vec::new();
            // let __m = map();
            body.push(Stmt::Let {
                vis: Vis::Private,
                name: "__m".to_string(),
                ty: None,
                value: Some(Expr::Call {
                    callee: Box::new(Expr::Ident("map".to_string(), span.clone())),
                    args: Vec::new(),
                    span: span.clone(),
                }),
                span: span.clone(),
            });
            // __m["__tag"] = "Variant";
            body.push(set_index(
                "__m",
                "__tag",
                Expr::Str(v.name.clone(), span.clone()),
                &span,
            ));
            // __m["__fields"] = ["a", "b"] — the field order travels with the
            // value, so a `match` in *another file* (which never sees this
            // declaration) can still bind payload fields by position.
            body.push(set_index(
                "__m",
                "__fields",
                Expr::List(
                    v.fields
                        .iter()
                        .map(|f| Expr::Str(f.clone(), span.clone()))
                        .collect(),
                    span.clone(),
                ),
                &span,
            ));
            for f in &v.fields {
                body.push(set_index(
                    "__m",
                    f,
                    Expr::Ident(f.clone(), span.clone()),
                    &span,
                ));
            }
            body.push(Stmt::Return {
                value: Some(Expr::Ident("__m".to_string(), span.clone())),
                span: span.clone(),
            });
            out.push(Stmt::Fn(FnProto {
                vis: e.vis,
                is_async: false,
                name: Some(v.name.clone()),
                params: v
                    .fields
                    .iter()
                    .map(|f| crate::ast::Param {
                        name: f.clone(),
                        ty: None,
                        span: span.clone(),
                    })
                    .collect(),
                ret: None,
                body: Block {
                    stmts: body,
                    span: span.clone(),
                },
                span: span.clone(),
            }));
        }
        out
    }

    /// `match (v) { P => body, … }` → a linear chain of guarded `if`s.
    ///
    /// ```text
    /// let __s = <scrutinee>;      // evaluated once
    /// let __t = null;             // the result
    /// let __d = false;            // "an arm already ran"
    /// if (not __d and <tag test>) { <binds>; if (<guard>) { <body>; __d = true } }
    /// …
    /// if (not __d) { throw "no match arm matched …" }
    /// ```
    ///
    /// The `__d` flag keeps the chain linear: a failed *guard* falls through to
    /// the next arm without duplicating the remaining arms into every branch.
    fn match_expr(
        &mut self,
        scrutinee: &Expr,
        arms: &[crate::ast::MatchArm],
        span: &Span,
    ) -> (Expr, Vec<Stmt>) {
        let (scrutinee, mut pre) = self.expr(scrutinee);
        let s_span = scrutinee.span().clone();
        let scrut = self.fresh();
        pre.push(let_null(&scrut, scrutinee, s_span.clone()));

        let result = self.fresh();
        pre.push(let_stmt(&result, Expr::Null(span.clone()), span.clone()));
        let done = self.fresh();
        pre.push(let_stmt(
            &done,
            Expr::Bool(false, span.clone()),
            span.clone(),
        ));

        for arm in arms {
            let base = Expr::Ident(scrut.clone(), s_span.clone());
            let (test, binds) = self.pattern_test(&arm.pattern, &base, span);
            let mut inner = binds;
            let body = self.value_block(&arm.body, &result);
            let mut guarded: Vec<Stmt> = body.stmts;
            guarded.push(set_local(&done, Expr::Bool(true, span.clone()), span));
            match &arm.guard {
                Some(g) => {
                    let (g, gpre) = self.expr(g);
                    inner.extend(gpre);
                    inner.push(Stmt::If {
                        cond: g,
                        then: Block {
                            stmts: guarded,
                            span: span.clone(),
                        },
                        elifs: Vec::new(),
                        els: None,
                        span: span.clone(),
                    });
                }
                None => inner.extend(guarded),
            }
            // `not __d and <test>`: once an arm has run, later arms are skipped.
            let cond = Expr::Logical {
                op: crate::ast::LogOp::And,
                l: Box::new(Expr::Unary {
                    op: UnOp::Not,
                    expr: Box::new(Expr::Ident(done.clone(), span.clone())),
                    span: span.clone(),
                }),
                r: Box::new(test),
                span: span.clone(),
            };
            pre.push(Stmt::If {
                cond,
                then: Block {
                    stmts: inner,
                    span: span.clone(),
                },
                elifs: Vec::new(),
                els: None,
                span: span.clone(),
            });
        }

        pre.push(Stmt::If {
            cond: Expr::Unary {
                op: UnOp::Not,
                expr: Box::new(Expr::Ident(done.clone(), span.clone())),
                span: span.clone(),
            },
            then: Block {
                stmts: vec![Stmt::Throw {
                    value: Expr::Str(
                        "no match arm matched the value (the match is not exhaustive)".to_string(),
                        span.clone(),
                    ),
                    span: span.clone(),
                }],
                span: span.clone(),
            },
            elifs: Vec::new(),
            els: None,
            span: span.clone(),
        });

        (Expr::Ident(result, span.clone()), pre)
    }

    /// `expr?` — return the failure from the enclosing function, otherwise
    /// yield the payload.
    ///
    /// ```text
    /// let __v = <expr>;
    /// if (is_failure(__v)) { return __v; }
    /// __v.unwrap()
    /// ```
    ///
    /// Returning the container itself (not the bare error) is what makes `?`
    /// compose: an `option` propagates `none`, a `result` propagates `err(e)`.
    fn propagate(&mut self, inner: &Expr, span: &Span) -> (Expr, Vec<Stmt>) {
        let (value, mut pre) = self.expr(inner);
        let vspan = value.span().clone();
        let v = self.fresh();
        pre.push(let_null(&v, value, vspan.clone()));
        pre.push(Stmt::If {
            cond: Expr::Call {
                callee: Box::new(Expr::Ident("is_failure".to_string(), span.clone())),
                args: vec![Expr::Ident(v.clone(), vspan.clone())],
                span: span.clone(),
            },
            then: Block {
                stmts: vec![Stmt::Return {
                    value: Some(Expr::Ident(v.clone(), vspan.clone())),
                    span: span.clone(),
                }],
                span: span.clone(),
            },
            elifs: Vec::new(),
            els: None,
            span: span.clone(),
        });
        let unwrapped = Expr::Method {
            object: Box::new(Expr::Ident(v, vspan.clone())),
            name: "unwrap".to_string(),
            args: Vec::new(),
            span: span.clone(),
        };
        (unwrapped, pre)
    }

    /// The condition a pattern imposes plus the bindings it introduces, tested
    /// against `base` (a temp holding the value).
    fn pattern_test(
        &mut self,
        p: &crate::ast::Pattern,
        base: &Expr,
        span: &Span,
    ) -> (Expr, Vec<Stmt>) {
        use crate::ast::Pattern;
        match p {
            Pattern::Wild => (Expr::Bool(true, span.clone()), Vec::new()),
            Pattern::Bind(name) => (
                Expr::Bool(true, span.clone()),
                vec![let_stmt(name, base.clone(), span.clone())],
            ),
            Pattern::Literal(lit) => (
                Expr::Binary {
                    op: BinOp::Eq,
                    l: Box::new(base.clone()),
                    r: Box::new(lit.clone()),
                    span: span.clone(),
                },
                Vec::new(),
            ),
            Pattern::Variant {
                name,
                args,
                span: vspan,
            } => {
                // `base["__tag"] == "Name"`
                let tag = Expr::Index {
                    object: Box::new(base.clone()),
                    index: Box::new(Expr::Str("__tag".to_string(), vspan.clone())),
                    span: vspan.clone(),
                };
                let mut cond = Expr::Binary {
                    op: BinOp::Eq,
                    l: Box::new(tag),
                    r: Box::new(Expr::Str(name.clone(), vspan.clone())),
                    span: vspan.clone(),
                };
                let fields = self.variants.get(name).cloned();
                let mut binds = Vec::new();
                // When the enum is declared in another file, its field *names*
                // are unknown here — but the constructor stored them in
                // `__fields`, so read the order at run time and index by
                // position. Without this, a cross-module match bound nothing and
                // the arm body failed with `unknown name`.
                let dynamic_fields = if fields.is_none() && !args.is_empty() {
                    let fl = self.fresh();
                    binds.push(let_null(
                        &fl,
                        Expr::Index {
                            object: Box::new(base.clone()),
                            index: Box::new(Expr::Str("__fields".to_string(), vspan.clone())),
                            span: vspan.clone(),
                        },
                        vspan.clone(),
                    ));
                    Some(fl)
                } else {
                    None
                };
                for (i, arg) in args.iter().enumerate() {
                    let key = match (&fields, &dynamic_fields) {
                        (Some(f), _) => match f.get(i) {
                            Some(name) => Expr::Str(name.clone(), vspan.clone()),
                            None => continue,
                        },
                        (None, Some(fl)) => Expr::Index {
                            object: Box::new(Expr::Ident(fl.clone(), vspan.clone())),
                            index: Box::new(Expr::Int(i as i64, vspan.clone())),
                            span: vspan.clone(),
                        },
                        (None, None) => continue,
                    };
                    let sub = Expr::Index {
                        object: Box::new(base.clone()),
                        index: Box::new(key),
                        span: vspan.clone(),
                    };
                    let (sub_cond, mut sub_binds) = self.pattern_test(arg, &sub, span);
                    binds.append(&mut sub_binds);
                    cond = Expr::Logical {
                        op: crate::ast::LogOp::And,
                        l: Box::new(cond),
                        r: Box::new(sub_cond),
                        span: vspan.clone(),
                    };
                }
                (cond, binds)
            }
        }
    }

    /// Lower an expression, returning it plus the statements that must run
    /// before it is evaluated.
    fn expr(&mut self, e: &Expr) -> (Expr, Vec<Stmt>) {
        match e {
            Expr::If {
                cond,
                then,
                els,
                span,
            } => self.if_expr(cond, then, els.as_ref(), span),
            Expr::OptField { object, name, span } => self.opt_field(object, name, span),
            Expr::Coalesce { l, r, span } => self.coalesce(l, r, span),
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => self.match_expr(scrutinee, arms, span),
            Expr::Propagate(inner, span) => self.propagate(inner, span),
            // A bare `super` is not a value; the checker rejects it. It is
            // passed through so the backend can report it if the checker was
            // bypassed.
            Expr::Super { span } => (Expr::Super { span: span.clone() }, Vec::new()),
            // An already-resolved `SuperCall` passes through unchanged.
            Expr::SuperCall { .. } => (e.clone(), Vec::new()),

            Expr::Int(..)
            | Expr::Float(..)
            | Expr::Str(..)
            | Expr::Bool(..)
            | Expr::Null(..)
            | Expr::Ident(..) => (e.clone(), Vec::new()),

            Expr::List(items, span) => {
                let (items, pre) = self.seq_list(items);
                (Expr::List(items, span.clone()), pre)
            }
            Expr::Fn(f) => (Expr::Fn(Box::new(self.fn_proto(f))), Vec::new()),
            Expr::Unary { op, expr, span } => {
                let (expr, pre) = self.expr(expr);
                (
                    Expr::Unary {
                        op: *op,
                        expr: Box::new(expr),
                        span: span.clone(),
                    },
                    pre,
                )
            }
            Expr::Binary { op, l, r, span } => {
                let (v, pre) = self.seq(&[l, r]);
                let mut it = v.into_iter();
                (
                    Expr::Binary {
                        op: *op,
                        l: Box::new(it.next().expect("two operands")),
                        r: Box::new(it.next().expect("two operands")),
                        span: span.clone(),
                    },
                    pre,
                )
            }
            // Short-circuiting: never spill both sides. `and`/`or` return the
            // operand value, so the temporary carries the result.
            Expr::Logical { op, l, r, span } => self.logical(*op, l, r, span),
            Expr::Assign {
                target,
                op,
                value,
                span,
            } => self.assign(target, *op, value, span),
            Expr::Call { callee, args, span } => {
                // `super(args)` — the parent class's constructor.
                if let Expr::Super { .. } = &**callee {
                    let (args, pre) = self.seq_list(args);
                    return (
                        Expr::SuperCall {
                            parent: self.current_parent.clone().unwrap_or_default(),
                            method: None,
                            args,
                            span: span.clone(),
                        },
                        pre,
                    );
                }
                let mut parts: Vec<&Expr> = vec![callee];
                parts.extend(args.iter());
                let (v, pre) = self.seq(&parts);
                let mut it = v.into_iter();
                let callee = it.next().expect("callee");
                (
                    Expr::Call {
                        callee: Box::new(callee),
                        args: it.collect(),
                        span: span.clone(),
                    },
                    pre,
                )
            }
            Expr::Method {
                object,
                name,
                args,
                span,
            } => {
                // `super.m(args)` — the parent class's implementation of `m`.
                if let Expr::Super { .. } = &**object {
                    let (args, pre) = self.seq_list(args);
                    return (
                        Expr::SuperCall {
                            parent: self.current_parent.clone().unwrap_or_default(),
                            method: Some(name.clone()),
                            args,
                            span: span.clone(),
                        },
                        pre,
                    );
                }
                let mut parts: Vec<&Expr> = vec![object];
                parts.extend(args.iter());
                let (v, pre) = self.seq(&parts);
                let mut it = v.into_iter();
                let object = it.next().expect("object");
                (
                    Expr::Method {
                        object: Box::new(object),
                        name: name.clone(),
                        args: it.collect(),
                        span: span.clone(),
                    },
                    pre,
                )
            }
            Expr::Index {
                object,
                index,
                span,
            } => {
                let (v, pre) = self.seq(&[object, index]);
                let mut it = v.into_iter();
                (
                    Expr::Index {
                        object: Box::new(it.next().expect("object")),
                        index: Box::new(it.next().expect("index")),
                        span: span.clone(),
                    },
                    pre,
                )
            }
            Expr::Field { object, name, span } => {
                let (object, pre) = self.expr(object);
                (
                    Expr::Field {
                        object: Box::new(object),
                        name: name.clone(),
                        span: span.clone(),
                    },
                    pre,
                )
            }
            Expr::New { class, args, span } => {
                let (args, pre) = self.seq_list(args);
                (
                    Expr::New {
                        class: class.clone(),
                        args,
                        span: span.clone(),
                    },
                    pre,
                )
            }
            Expr::Await(inner, span) => {
                let (inner, pre) = self.expr(inner);
                (Expr::Await(Box::new(inner), span.clone()), pre)
            }
            Expr::Spawn { callable, span } => {
                let (callable, pre) = self.expr(callable);
                (
                    Expr::Spawn {
                        callable: Box::new(callable),
                        span: span.clone(),
                    },
                    pre,
                )
            }
        }
    }

    /// Lower a list of expressions that are evaluated in source order.
    ///
    /// If none of them needs a prelude, they are returned as-is (the common
    /// case, and the reason ordinary code is untouched). If any does, every one
    /// is spilled into a temporary in order, which preserves the exact sequence
    /// of side effects.
    fn seq_list(&mut self, exprs: &[Expr]) -> (Vec<Expr>, Vec<Stmt>) {
        let refs: Vec<&Expr> = exprs.iter().collect();
        self.seq(&refs)
    }

    fn seq(&mut self, exprs: &[&Expr]) -> (Vec<Expr>, Vec<Stmt>) {
        let mut lowered: Vec<(Expr, Vec<Stmt>)> = Vec::with_capacity(exprs.len());
        for e in exprs {
            lowered.push(self.expr(e));
        }
        if lowered.iter().all(|(_, p)| p.is_empty()) {
            return (lowered.into_iter().map(|(e, _)| e).collect(), Vec::new());
        }
        let mut pre = Vec::new();
        let mut out = Vec::new();
        for (e, p) in lowered {
            pre.extend(p);
            let span = e.span().clone();
            let t = self.fresh();
            pre.push(let_null(&t, e, span.clone()));
            out.push(Expr::Ident(t, span));
        }
        (out, pre)
    }

    /// `if (c) { a } else { b }` → a temporary assigned in both branches.
    fn if_expr(
        &mut self,
        cond: &Expr,
        then: &Block,
        els: Option<&Block>,
        span: &Span,
    ) -> (Expr, Vec<Stmt>) {
        let (cond, mut pre) = self.expr(cond);
        let t = self.fresh();
        pre.push(let_stmt(&t, Expr::Null(span.clone()), span.clone()));
        let then = self.value_block(then, &t);
        let els = els.map(|b| self.value_block(b, &t));
        pre.push(Stmt::If {
            cond,
            then,
            elifs: Vec::new(),
            els,
            span: span.clone(),
        });
        (Expr::Ident(t, span.clone()), pre)
    }

    /// A block whose trailing expression becomes the value of `temp`.
    /// A block that ends in `return`/`throw` needs no assignment.
    fn value_block(&mut self, b: &Block, temp: &str) -> Block {
        let mut stmts = Vec::new();
        let last = b.stmts.len().saturating_sub(1);
        for (i, s) in b.stmts.iter().enumerate() {
            if i == last {
                if let Stmt::Expr(e) = s {
                    let (value, pre) = self.expr(e);
                    let span = value.span().clone();
                    stmts.extend(pre);
                    stmts.push(Stmt::Expr(Expr::Assign {
                        target: Box::new(Expr::Ident(temp.to_string(), span.clone())),
                        op: AssignOp::Assign,
                        value: Box::new(value),
                        span,
                    }));
                    continue;
                }
            }
            stmts.extend(self.stmt(s));
        }
        Block {
            stmts,
            span: b.span.clone(),
        }
    }

    /// `obj?.field` — the receiver is evaluated once, and the field is read only
    /// when it is not null.
    fn opt_field(&mut self, object: &Expr, name: &str, span: &Span) -> (Expr, Vec<Stmt>) {
        let (object, mut pre) = self.expr(object);
        let obj_span = object.span().clone();
        let obj = self.fresh();
        pre.push(let_null(&obj, object, obj_span.clone()));
        let t = self.fresh();
        pre.push(let_stmt(&t, Expr::Null(span.clone()), span.clone()));
        pre.push(Stmt::If {
            cond: Expr::Binary {
                op: BinOp::Ne,
                l: Box::new(Expr::Ident(obj.clone(), obj_span.clone())),
                r: Box::new(Expr::Null(obj_span.clone())),
                span: obj_span.clone(),
            },
            then: Block {
                stmts: vec![Stmt::Expr(Expr::Assign {
                    target: Box::new(Expr::Ident(t.clone(), span.clone())),
                    op: AssignOp::Assign,
                    value: Box::new(Expr::Field {
                        object: Box::new(Expr::Ident(obj, obj_span.clone())),
                        name: name.to_string(),
                        span: span.clone(),
                    }),
                    span: span.clone(),
                })],
                span: span.clone(),
            },
            elifs: Vec::new(),
            els: None,
            span: span.clone(),
        });
        (Expr::Ident(t, span.clone()), pre)
    }

    /// `a ?? b` — `b` is evaluated only when `a` is null.
    fn coalesce(&mut self, l: &Expr, r: &Expr, span: &Span) -> (Expr, Vec<Stmt>) {
        let (left, mut pre) = self.expr(l);
        let t = self.fresh();
        let lspan = left.span().clone();
        pre.push(let_null(&t, left, lspan.clone()));
        let (right, right_pre) = self.expr(r);
        let mut then_stmts = right_pre;
        then_stmts.push(Stmt::Expr(Expr::Assign {
            target: Box::new(Expr::Ident(t.clone(), span.clone())),
            op: AssignOp::Assign,
            value: Box::new(right),
            span: span.clone(),
        }));
        pre.push(Stmt::If {
            cond: Expr::Binary {
                op: BinOp::Eq,
                l: Box::new(Expr::Ident(t.clone(), lspan.clone())),
                r: Box::new(Expr::Null(lspan.clone())),
                span: lspan.clone(),
            },
            then: Block {
                stmts: then_stmts,
                span: span.clone(),
            },
            elifs: Vec::new(),
            els: None,
            span: span.clone(),
        });
        (Expr::Ident(t, span.clone()), pre)
    }

    /// `and`/`or` return one of their operands, and evaluate the right one only
    /// when needed.
    fn logical(
        &mut self,
        op: crate::ast::LogOp,
        l: &Expr,
        r: &Expr,
        span: &Span,
    ) -> (Expr, Vec<Stmt>) {
        let (left, left_pre) = self.expr(l);
        let (right, right_pre) = self.expr(r);
        if left_pre.is_empty() && right_pre.is_empty() {
            return (
                Expr::Logical {
                    op,
                    l: Box::new(left),
                    r: Box::new(right),
                    span: span.clone(),
                },
                Vec::new(),
            );
        }
        let t = self.fresh();
        let mut pre = left_pre;
        let lspan = left.span().clone();
        pre.push(let_null(&t, left, lspan.clone()));
        // `and` takes the right operand when the left is truthy; `or` when it is
        // falsy. `bool(...)` is the language's truthiness conversion.
        let truthy = Expr::Call {
            callee: Box::new(Expr::Ident("bool".to_string(), lspan.clone())),
            args: vec![Expr::Ident(t.clone(), lspan.clone())],
            span: lspan.clone(),
        };
        let cond = match op {
            crate::ast::LogOp::And => truthy,
            crate::ast::LogOp::Or => Expr::Unary {
                op: UnOp::Not,
                expr: Box::new(truthy),
                span: lspan.clone(),
            },
        };
        let mut then_stmts = right_pre;
        then_stmts.push(Stmt::Expr(Expr::Assign {
            target: Box::new(Expr::Ident(t.clone(), span.clone())),
            op: AssignOp::Assign,
            value: Box::new(right),
            span: span.clone(),
        }));
        pre.push(Stmt::If {
            cond,
            then: Block {
                stmts: then_stmts,
                span: span.clone(),
            },
            elifs: Vec::new(),
            els: None,
            span: span.clone(),
        });
        (Expr::Ident(t, span.clone()), pre)
    }

    /// Assignment evaluates its target's parts before the value, so both are
    /// lowered with the shared spill rule.
    fn assign(
        &mut self,
        target: &Expr,
        op: AssignOp,
        value: &Expr,
        span: &Span,
    ) -> (Expr, Vec<Stmt>) {
        let (value_l, value_pre) = self.expr(value);
        if value_pre.is_empty() {
            let (target_l, target_pre) = self.expr(target);
            return (
                Expr::Assign {
                    target: Box::new(target_l),
                    op,
                    value: Box::new(value_l),
                    span: span.clone(),
                },
                target_pre,
            );
        }
        // The value needs statements: lower the target's parts first, spilling
        // them so the target is still computed before the value.
        let (target_l, target_pre) = self.expr(target);
        let mut pre = target_pre;
        pre.extend(value_pre);
        (
            Expr::Assign {
                target: Box::new(target_l),
                op,
                value: Box::new(value_l),
                span: span.clone(),
            },
            pre,
        )
    }
}

/// `name["key"] = value;`
fn set_index(name: &str, key: &str, value: Expr, span: &Span) -> Stmt {
    Stmt::Expr(Expr::Assign {
        target: Box::new(Expr::Index {
            object: Box::new(Expr::Ident(name.to_string(), span.clone())),
            index: Box::new(Expr::Str(key.to_string(), span.clone())),
            span: span.clone(),
        }),
        op: AssignOp::Assign,
        value: Box::new(value),
        span: span.clone(),
    })
}

/// `name = value;` for an existing binding.
fn set_local(name: &str, value: Expr, span: &Span) -> Stmt {
    Stmt::Expr(Expr::Assign {
        target: Box::new(Expr::Ident(name.to_string(), span.clone())),
        op: AssignOp::Assign,
        value: Box::new(value),
        span: span.clone(),
    })
}

fn let_stmt(name: &str, value: Expr, span: Span) -> Stmt {
    Stmt::Let {
        vis: Vis::Private,
        name: name.to_string(),
        ty: None,
        value: Some(value),
        span,
    }
}

/// `let name = value;` — the same as [`let_stmt`]; kept as a named alias so the
/// call sites that mean "spill this expression" read as such.
fn let_null(name: &str, value: Expr, span: Span) -> Stmt {
    let_stmt(name, value, span)
}
