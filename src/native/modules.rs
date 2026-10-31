//! AOT module flattening: resolve `import "name";` and inline the module's
//! top-level declarations into one self-contained `Program` before codegen.
//!
//! The bytecode compiler embeds imported modules into `.matrixc` (see
//! [`crate::vm::compiler`]); the AOT backend instead merges them at the AST
//! level, so [`crate::native::ir::build_module`] sees a single module with
//! every dependency present (transitively) and needs no runtime import
//! window. Visibility is already enforced by the semantic pass, which runs
//! before this flattening in the `build` flow, so a program that reaches here
//! only imports public names.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::ast::{Block, CatchClause, Expr, FnProto, MatchArm, Pattern, Program, Stmt, Vis};
use crate::diagnostics::{Diagnostic, Phase, Span};

/// Recursively inline the declarations of every imported module into a single
/// program. `Stmt::Import` statements are expanded; all other statements pass
/// through unchanged. Each module is inlined at most once (dedup by canonical
/// path); a cycle is reported like the bytecode path does.
pub fn flatten_imports(program: &Program, importer_file: &str) -> Result<Program, Diagnostic> {
    let mut out: Vec<Stmt> = Vec::new();
    let mut done: HashSet<PathBuf> = HashSet::new();
    let mut done_aliased: HashSet<(PathBuf, String)> = HashSet::new();
    let mut in_progress: HashSet<PathBuf> = HashSet::new();
    let importer_path = PathBuf::from(importer_file);
    if importer_path.is_file() {
        in_progress.insert(crate::modules::canonical_key(&importer_path));
    }
    flatten_stmts(
        &program.stmts,
        importer_file,
        &mut out,
        &mut done,
        &mut done_aliased,
        &mut in_progress,
    )?;
    Ok(Program { stmts: out })
}

fn flatten_stmts(
    stmts: &[Stmt],
    importer_file: &str,
    out: &mut Vec<Stmt>,
    done: &mut HashSet<PathBuf>,
    done_aliased: &mut HashSet<(PathBuf, String)>,
    in_progress: &mut HashSet<PathBuf>,
) -> Result<(), Diagnostic> {
    for s in stmts {
        if let Stmt::Import { path, alias, span } = s {
            if let Some(alias_name) = alias {
                let resolved = resolve_module(importer_file, path)?;
                let canon = crate::modules::canonical_key(&resolved);
                let key = (canon.clone(), alias_name.clone());
                if done_aliased.contains(&key) {
                    continue;
                }
                if in_progress.contains(&canon) {
                    return Err(Diagnostic::new(
                        Phase::Semantic,
                        format!("circular import of '{}'", path),
                    ));
                }
                // `read_module_source`, not `fs::read_to_string`: an embedded
                // module's path is the synthetic `<stdlib>/name.matrix` marker,
                // which only this reader understands.
                let src = crate::modules::read_module_source(&resolved).map_err(|e| {
                    Diagnostic::new(Phase::Semantic, format!("cannot import '{}': {}", path, e))
                })?;
                let resolved_str = resolved.to_string_lossy().to_string();
                let file_id: Arc<str> = Arc::from(resolved_str.as_str());
                let module: Program = crate::parser::parse_file(&src, file_id)?;
                crate::semantic::check_file(&module, false, &resolved_str)?;
                let lowered = crate::lower::lower(&module);
                let mut sub_stmts = Vec::new();
                in_progress.insert(canon.clone());
                flatten_stmts(
                    &lowered.stmts,
                    &resolved_str,
                    &mut sub_stmts,
                    done,
                    done_aliased,
                    in_progress,
                )?;
                in_progress.remove(&canon);
                done_aliased.insert(key);
                mangle_aliased_module(&sub_stmts, alias_name, span, out);
                continue;
            }
            let resolved = resolve_module(importer_file, path)?;
            let canon = crate::modules::canonical_key(&resolved);
            if done.contains(&canon) {
                continue; // already inlined by another importer
            }
            if in_progress.contains(&canon) {
                return Err(Diagnostic::new(
                    Phase::Semantic,
                    format!("circular import of '{}'", path),
                ));
            }
            let src = crate::modules::read_module_source(&resolved).map_err(|e| {
                Diagnostic::new(Phase::Semantic, format!("cannot import '{}': {}", path, e))
            })?;
            let resolved_str = resolved.to_string_lossy().to_string();
            let file_id: Arc<str> = Arc::from(resolved_str.as_str());
            let module: Program = crate::parser::parse_file(&src, file_id)?;
            crate::semantic::check_file(&module, false, &resolved_str)?;
            // Same seam as the other backends: an imported module is lowered
            // before its statements are flattened into the caller.
            let lowered = crate::lower::lower(&module);
            in_progress.insert(canon.clone());
            flatten_stmts(
                &lowered.stmts,
                &resolved_str,
                out,
                done,
                done_aliased,
                in_progress,
            )?;
            in_progress.remove(&canon);
            done.insert(canon);
        } else {
            out.push(s.clone());
        }
    }
    Ok(())
}

fn mangle_aliased_module(stmts: &[Stmt], alias: &str, span: &Span, out: &mut Vec<Stmt>) {
    let prefix = format!("__mod_{}_", alias);
    let mut top_names: HashSet<String> = HashSet::new();
    let mut exports: Vec<String> = Vec::new();
    for s in stmts {
        match s {
            Stmt::Fn(p) => {
                if let Some(name) = &p.name {
                    top_names.insert(name.clone());
                    if p.vis == Vis::Public {
                        exports.push(name.clone());
                    }
                }
            }
            Stmt::Class(c) => {
                top_names.insert(c.name.clone());
                if c.vis == Vis::Public {
                    exports.push(c.name.clone());
                }
            }
            Stmt::Let { vis, name, .. } | Stmt::Const { vis, name, .. } => {
                top_names.insert(name.clone());
                if *vis == Vis::Public {
                    exports.push(name.clone());
                }
            }
            _ => {}
        }
    }

    let mut scopes: Vec<HashSet<String>> = Vec::new();
    for s in stmts {
        out.push(rewrite_top_stmt(s, &prefix, &top_names, &mut scopes));
    }

    let mut map_args: Vec<Expr> = Vec::with_capacity(exports.len() * 2);
    for exp in exports {
        map_args.push(Expr::Str(exp.clone(), span.clone()));
        map_args.push(Expr::Ident(format!("{}{}", prefix, exp), span.clone()));
    }
    out.push(Stmt::Const {
        vis: Vis::Private,
        name: alias.to_string(),
        ty: None,
        value: Expr::Call {
            callee: Box::new(Expr::Ident("module_namespace".to_string(), span.clone())),
            args: map_args,
            span: span.clone(),
        },
        span: span.clone(),
    });
}

fn is_shadowed(name: &str, scopes: &[HashSet<String>]) -> bool {
    scopes.iter().any(|s| s.contains(name))
}

fn rewrite_top_stmt(
    s: &Stmt,
    prefix: &str,
    top_names: &HashSet<String>,
    scopes: &mut Vec<HashSet<String>>,
) -> Stmt {
    match s {
        Stmt::Fn(p) => {
            let mut p2 = rewrite_fn_proto(p, prefix, top_names, scopes);
            if let Some(name) = &p.name {
                p2.name = Some(format!("{}{}", prefix, name));
            }
            Stmt::Fn(p2)
        }
        Stmt::Let {
            vis,
            name,
            ty,
            value,
            span,
        } => Stmt::Let {
            vis: *vis,
            name: format!("{}{}", prefix, name),
            ty: ty.clone(),
            value: value
                .as_ref()
                .map(|v| rewrite_expr(v, prefix, top_names, scopes)),
            span: span.clone(),
        },
        Stmt::Const {
            vis,
            name,
            ty,
            value,
            span,
        } => Stmt::Const {
            vis: *vis,
            name: format!("{}{}", prefix, name),
            ty: ty.clone(),
            value: rewrite_expr(value, prefix, top_names, scopes),
            span: span.clone(),
        },
        Stmt::Class(c) => {
            let mut c2 = c.clone();
            c2.name = format!("{}{}", prefix, c.name);
            if let Some(parent) = &c.parent {
                if top_names.contains(parent) {
                    c2.parent = Some(format!("{}{}", prefix, parent));
                }
            }
            c2.methods = c
                .methods
                .iter()
                .map(|m| rewrite_fn_proto(m, prefix, top_names, scopes))
                .collect();
            Stmt::Class(c2)
        }
        other => rewrite_stmt_in_scope(other, prefix, top_names, scopes),
    }
}

fn rewrite_fn_proto(
    p: &FnProto,
    prefix: &str,
    top_names: &HashSet<String>,
    scopes: &mut Vec<HashSet<String>>,
) -> FnProto {
    let mut param_scope = HashSet::new();
    let params = p
        .params
        .iter()
        .map(|param| {
            let default = param
                .default
                .as_ref()
                .map(|d| rewrite_expr(d, prefix, top_names, scopes));
            param_scope.insert(param.name.clone());
            crate::ast::Param {
                default,
                ..param.clone()
            }
        })
        .collect();
    scopes.push(param_scope);
    let body = rewrite_block(&p.body, prefix, top_names, scopes);
    scopes.pop();
    FnProto {
        params,
        body,
        ..p.clone()
    }
}

fn rewrite_block(
    b: &Block,
    prefix: &str,
    top_names: &HashSet<String>,
    scopes: &mut Vec<HashSet<String>>,
) -> Block {
    scopes.push(HashSet::new());
    let mut stmts = Vec::with_capacity(b.stmts.len());
    for s in &b.stmts {
        stmts.push(rewrite_stmt_in_scope(s, prefix, top_names, scopes));
    }
    scopes.pop();
    Block {
        stmts,
        span: b.span.clone(),
    }
}

fn rewrite_stmt_in_scope(
    s: &Stmt,
    prefix: &str,
    top_names: &HashSet<String>,
    scopes: &mut Vec<HashSet<String>>,
) -> Stmt {
    match s {
        Stmt::Fn(p) => {
            if let Some(name) = &p.name {
                if let Some(cur) = scopes.last_mut() {
                    cur.insert(name.clone());
                }
            }
            Stmt::Fn(rewrite_fn_proto(p, prefix, top_names, scopes))
        }
        Stmt::Let {
            vis,
            name,
            ty,
            value,
            span,
        } => {
            let value = value
                .as_ref()
                .map(|v| rewrite_expr(v, prefix, top_names, scopes));
            if let Some(cur) = scopes.last_mut() {
                cur.insert(name.clone());
            }
            Stmt::Let {
                vis: *vis,
                name: name.clone(),
                ty: ty.clone(),
                value,
                span: span.clone(),
            }
        }
        Stmt::Const {
            vis,
            name,
            ty,
            value,
            span,
        } => {
            let value = rewrite_expr(value, prefix, top_names, scopes);
            if let Some(cur) = scopes.last_mut() {
                cur.insert(name.clone());
            }
            Stmt::Const {
                vis: *vis,
                name: name.clone(),
                ty: ty.clone(),
                value,
                span: span.clone(),
            }
        }
        Stmt::If {
            cond,
            then,
            elifs,
            els,
            span,
        } => Stmt::If {
            cond: rewrite_expr(cond, prefix, top_names, scopes),
            then: rewrite_block(then, prefix, top_names, scopes),
            elifs: elifs
                .iter()
                .map(|(c, b)| {
                    (
                        rewrite_expr(c, prefix, top_names, scopes),
                        rewrite_block(b, prefix, top_names, scopes),
                    )
                })
                .collect(),
            els: els
                .as_ref()
                .map(|b| rewrite_block(b, prefix, top_names, scopes)),
            span: span.clone(),
        },
        Stmt::While { cond, body, span } => Stmt::While {
            cond: rewrite_expr(cond, prefix, top_names, scopes),
            body: rewrite_block(body, prefix, top_names, scopes),
            span: span.clone(),
        },
        Stmt::ForIn {
            var,
            var2,
            iter,
            body,
            span,
        } => {
            let iter = rewrite_expr(iter, prefix, top_names, scopes);
            let mut loop_scope = HashSet::new();
            loop_scope.insert(var.clone());
            if let Some(v2) = var2 {
                loop_scope.insert(v2.clone());
            }
            scopes.push(loop_scope);
            let body = rewrite_block(body, prefix, top_names, scopes);
            scopes.pop();
            Stmt::ForIn {
                var: var.clone(),
                var2: var2.clone(),
                iter,
                body,
                span: span.clone(),
            }
        }
        Stmt::ForC {
            init,
            cond,
            post,
            body,
            span,
        } => {
            scopes.push(HashSet::new());
            let init = Box::new(rewrite_stmt_in_scope(init, prefix, top_names, scopes));
            let cond = rewrite_expr(cond, prefix, top_names, scopes);
            let post = rewrite_expr(post, prefix, top_names, scopes);
            let body = rewrite_block(body, prefix, top_names, scopes);
            scopes.pop();
            Stmt::ForC {
                init,
                cond,
                post,
                body,
                span: span.clone(),
            }
        }
        Stmt::Return { value, span } => Stmt::Return {
            value: value
                .as_ref()
                .map(|v| rewrite_expr(v, prefix, top_names, scopes)),
            span: span.clone(),
        },
        Stmt::Block(b) => Stmt::Block(rewrite_block(b, prefix, top_names, scopes)),
        Stmt::Expr(e) => Stmt::Expr(rewrite_expr(e, prefix, top_names, scopes)),
        Stmt::Spawn { callable, span } => Stmt::Spawn {
            callable: rewrite_expr(callable, prefix, top_names, scopes),
            span: span.clone(),
        },
        Stmt::Try {
            body,
            catch,
            finally,
            span,
        } => {
            let body = rewrite_block(body, prefix, top_names, scopes);
            let catch = catch.as_ref().map(|c| {
                let mut catch_scope = HashSet::new();
                catch_scope.insert(c.name.clone());
                scopes.push(catch_scope);
                let cbody = rewrite_block(&c.body, prefix, top_names, scopes);
                scopes.pop();
                CatchClause {
                    name: c.name.clone(),
                    body: cbody,
                    span: c.span.clone(),
                }
            });
            let finally = finally
                .as_ref()
                .map(|f| rewrite_block(f, prefix, top_names, scopes));
            Stmt::Try {
                body,
                catch,
                finally,
                span: span.clone(),
            }
        }
        Stmt::Throw { value, span } => Stmt::Throw {
            value: rewrite_expr(value, prefix, top_names, scopes),
            span: span.clone(),
        },
        other => other.clone(),
    }
}

fn rewrite_expr(
    e: &Expr,
    prefix: &str,
    top_names: &HashSet<String>,
    scopes: &mut Vec<HashSet<String>>,
) -> Expr {
    match e {
        Expr::Ident(name, sp) => {
            if !is_shadowed(name, scopes) && top_names.contains(name) {
                Expr::Ident(format!("{}{}", prefix, name), sp.clone())
            } else {
                e.clone()
            }
        }
        Expr::List(items, sp) => Expr::List(
            items
                .iter()
                .map(|it| rewrite_expr(it, prefix, top_names, scopes))
                .collect(),
            sp.clone(),
        ),
        Expr::Fn(p) => Expr::Fn(Box::new(rewrite_fn_proto(p, prefix, top_names, scopes))),
        Expr::Unary { op, expr, span } => Expr::Unary {
            op: *op,
            expr: Box::new(rewrite_expr(expr, prefix, top_names, scopes)),
            span: span.clone(),
        },
        Expr::Binary { op, l, r, span } => Expr::Binary {
            op: *op,
            l: Box::new(rewrite_expr(l, prefix, top_names, scopes)),
            r: Box::new(rewrite_expr(r, prefix, top_names, scopes)),
            span: span.clone(),
        },
        Expr::Logical { op, l, r, span } => Expr::Logical {
            op: *op,
            l: Box::new(rewrite_expr(l, prefix, top_names, scopes)),
            r: Box::new(rewrite_expr(r, prefix, top_names, scopes)),
            span: span.clone(),
        },
        Expr::Assign {
            target,
            op,
            value,
            span,
        } => Expr::Assign {
            target: Box::new(rewrite_expr(target, prefix, top_names, scopes)),
            op: *op,
            value: Box::new(rewrite_expr(value, prefix, top_names, scopes)),
            span: span.clone(),
        },
        Expr::Call { callee, args, span } => {
            let callee = match callee.as_ref() {
                Expr::Ident(name, _) if crate::value::collections::is_constructor(name) => {
                    callee.clone()
                }
                other => Box::new(rewrite_expr(other, prefix, top_names, scopes)),
            };
            Expr::Call {
                callee,
                args: args
                    .iter()
                    .map(|a| rewrite_expr(a, prefix, top_names, scopes))
                    .collect(),
                span: span.clone(),
            }
        }
        Expr::Method {
            object,
            name,
            args,
            span,
        } => Expr::Method {
            object: Box::new(rewrite_expr(object, prefix, top_names, scopes)),
            name: name.clone(),
            args: args
                .iter()
                .map(|a| rewrite_expr(a, prefix, top_names, scopes))
                .collect(),
            span: span.clone(),
        },
        Expr::Index {
            object,
            index,
            span,
        } => Expr::Index {
            object: Box::new(rewrite_expr(object, prefix, top_names, scopes)),
            index: Box::new(rewrite_expr(index, prefix, top_names, scopes)),
            span: span.clone(),
        },
        Expr::Field { object, name, span } => Expr::Field {
            object: Box::new(rewrite_expr(object, prefix, top_names, scopes)),
            name: name.clone(),
            span: span.clone(),
        },
        Expr::New { class, args, span } => {
            let class = if !is_shadowed(class, scopes) && top_names.contains(class) {
                format!("{}{}", prefix, class)
            } else {
                class.clone()
            };
            Expr::New {
                class,
                args: args
                    .iter()
                    .map(|a| rewrite_expr(a, prefix, top_names, scopes))
                    .collect(),
                span: span.clone(),
            }
        }
        Expr::Await(inner, sp) => Expr::Await(
            Box::new(rewrite_expr(inner, prefix, top_names, scopes)),
            sp.clone(),
        ),
        Expr::Spawn { callable, span } => Expr::Spawn {
            callable: Box::new(rewrite_expr(callable, prefix, top_names, scopes)),
            span: span.clone(),
        },
        Expr::SuperCall {
            parent,
            method,
            args,
            span,
        } => {
            let parent = if top_names.contains(parent) {
                format!("{}{}", prefix, parent)
            } else {
                parent.clone()
            };
            Expr::SuperCall {
                parent,
                method: method.clone(),
                args: args
                    .iter()
                    .map(|a| rewrite_expr(a, prefix, top_names, scopes))
                    .collect(),
                span: span.clone(),
            }
        }
        Expr::OptField { object, name, span } => Expr::OptField {
            object: Box::new(rewrite_expr(object, prefix, top_names, scopes)),
            name: name.clone(),
            span: span.clone(),
        },
        Expr::Coalesce { l, r, span } => Expr::Coalesce {
            l: Box::new(rewrite_expr(l, prefix, top_names, scopes)),
            r: Box::new(rewrite_expr(r, prefix, top_names, scopes)),
            span: span.clone(),
        },
        Expr::Propagate(inner, span) => Expr::Propagate(
            Box::new(rewrite_expr(inner, prefix, top_names, scopes)),
            span.clone(),
        ),
        Expr::NamedArg { name, value, span } => Expr::NamedArg {
            name: name.clone(),
            value: Box::new(rewrite_expr(value, prefix, top_names, scopes)),
            span: span.clone(),
        },
        Expr::If {
            cond,
            then,
            els,
            span,
        } => Expr::If {
            cond: Box::new(rewrite_expr(cond, prefix, top_names, scopes)),
            then: rewrite_block(then, prefix, top_names, scopes),
            els: els
                .as_ref()
                .map(|b| rewrite_block(b, prefix, top_names, scopes)),
            span: span.clone(),
        },
        Expr::Match {
            scrutinee,
            arms,
            span,
        } => Expr::Match {
            scrutinee: Box::new(rewrite_expr(scrutinee, prefix, top_names, scopes)),
            arms: arms
                .iter()
                .map(|arm| {
                    let mut arm_scope = HashSet::new();
                    collect_pattern_bindings(&arm.pattern, &mut arm_scope);
                    scopes.push(arm_scope);
                    let guard = arm
                        .guard
                        .as_ref()
                        .map(|g| rewrite_expr(g, prefix, top_names, scopes));
                    let body = rewrite_block(&arm.body, prefix, top_names, scopes);
                    scopes.pop();
                    MatchArm {
                        pattern: arm.pattern.clone(),
                        guard,
                        body,
                        span: arm.span.clone(),
                    }
                })
                .collect(),
            span: span.clone(),
        },
        other => other.clone(),
    }
}

fn collect_pattern_bindings(p: &Pattern, out: &mut HashSet<String>) {
    match p {
        Pattern::Bind(name) => {
            out.insert(name.clone());
        }
        Pattern::Variant { args, .. } => {
            for a in args {
                collect_pattern_bindings(a, out);
            }
        }
        Pattern::Wild | Pattern::Literal(_) => {}
    }
}

/// Resolve an import path relative to the importing file's directory first,
/// then [`crate::modules::default_module_dirs`] (`SPID3R_PATH`, `stdlib/`,
/// `vendor/`) — mirroring [`crate::vm::compiler`]'s resolver so both the
/// bytecode and AOT paths agree on module lookup.
///
/// The final [`crate::modules::embedded_path`] fallback is what makes a
/// `stdlib/` directory optional: without it `spid3r build` reported
/// `module 'semver' not found` on an install that had no `stdlib/` on disk,
/// while `spid3r run` in the same directory succeeded off the embedded table.
/// Disk still wins, so a project-local `stdlib/` or `vendor/` copy keeps
/// overriding the built-in one, exactly as on the other two backends.
fn resolve_module(importer_file: &str, path: &str) -> Result<PathBuf, Diagnostic> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let base = PathBuf::from(importer_file);
    if let Some(dir) = base.parent() {
        candidates.push(dir.join(path));
        candidates.push(dir.join(format!("{}.matrix", path)));
    }
    for d in crate::modules::default_module_dirs() {
        candidates.push(d.join(path));
        candidates.push(d.join(format!("{}.matrix", path)));
    }
    for c in candidates {
        if c.is_file() {
            return Ok(c);
        }
    }
    if let Some(p) = crate::modules::embedded_path(path) {
        return Ok(p);
    }
    Err(Diagnostic::new(
        Phase::Semantic,
        format!(
            "module '{}' not found (looked next to the importing file and on SPID3R_PATH)",
            path
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_imports_passthrough() {
        let program =
            crate::parser::parse_file("fn main() { return 0; }", std::sync::Arc::from("m"))
                .expect("parse");
        let flat = flatten_imports(&program, "m").expect("flatten");
        assert_eq!(flat.stmts.len(), program.stmts.len());
    }

    #[test]
    fn missing_module_errors() {
        let program = crate::parser::parse_file(
            "import \"nope\";\nfn main() { return 0; }",
            std::sync::Arc::from("m"),
        )
        .expect("parse");
        let e = flatten_imports(&program, "m").unwrap_err();
        assert!(e.message.contains("not found"), "got: {}", e.message);
    }
}
