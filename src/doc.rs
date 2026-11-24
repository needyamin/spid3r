//! `spid3r doc` — documentation generator for `.matrix` sources (`R8.5`).
//!
//! Walks `.matrix` files or directories, extracts comments immediately above
//! declarations (sharing `doc_above` / `strip_comment` with `crate::lsp`),
//! and renders either machine-readable JSON (`--json`) or a self-contained
//! single-file HTML reference.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::ast::{self, Program, Stmt, Vis};
use crate::diagnostics::{Diagnostic, Phase, Span};
use crate::lexer::{Comment, Lexer};
use crate::parser;

/// Extract a contiguous comment run immediately above `span`, markers stripped.
///
/// Shared between `spid3r doc` and `spid3r lsp` so hover tooltips and generated
/// documentation always agree on which comments attach to a declaration.
pub fn doc_above(span: &Span, comments: &[Comment]) -> Option<String> {
    if span.line < 2 {
        return None;
    }
    let mut rows: Vec<(u32, String)> = comments
        .iter()
        .filter(|c| c.line < span.line)
        .map(|c| (c.line, strip_comment(&c.text)))
        .collect();
    rows.sort_by_key(|(l, _)| *l);
    let mut collected: Vec<String> = Vec::new();
    let mut prev = span.line as i64 - 1;
    for (line, text) in rows.into_iter().rev() {
        if i64::from(line) == prev {
            collected.push(text);
            prev -= 1;
        } else {
            break;
        }
    }
    collected.reverse();
    if collected.is_empty() {
        None
    } else {
        Some(collected.join("\n"))
    }
}

/// Strip leading `///`, `//`, `/* ... */`, or `*` comment delimiters.
pub fn strip_comment(t: &str) -> String {
    let t = t.trim();
    if let Some(rest) = t.strip_prefix("///") {
        return rest.trim().to_string();
    }
    if let Some(rest) = t.strip_prefix("//") {
        return rest.trim().to_string();
    }
    if let Some(rest) = t.strip_prefix("/*") {
        let inner = rest.strip_suffix("*/").unwrap_or(rest);
        return inner.trim().to_string();
    }
    if let Some(rest) = t.strip_prefix('*') {
        return rest.trim().to_string();
    }
    t.to_string()
}

/// Format a type annotation expression for display in signatures.
pub fn type_str(ty: &Option<ast::TypeExpr>) -> String {
    match ty {
        Some(t) => format_type_expr(t),
        None => String::new(),
    }
}

fn format_type_expr(t: &ast::TypeExpr) -> String {
    if !t.union.is_empty() {
        let parts: Vec<String> = t.union.iter().map(format_type_expr).collect();
        let joined = parts.join(" | ");
        return if t.nullable {
            format!("({})?", joined)
        } else {
            joined
        };
    }
    let mut s = t.name.clone();
    if !t.args.is_empty() {
        s.push('<');
        let rendered: Vec<String> = t.args.iter().map(format_type_expr).collect();
        s.push_str(&rendered.join(", "));
        s.push('>');
    }
    if t.nullable {
        s.push('?');
    }
    s
}

/// Format a function or method signature (without leading visibility).
pub fn fn_signature(f: &ast::FnProto) -> String {
    let params: Vec<String> = f
        .params
        .iter()
        .map(|p| {
            let t = type_str(&p.ty);
            if t.is_empty() {
                p.name.clone()
            } else {
                format!("{}: {}", p.name, t)
            }
        })
        .collect();
    let ret = f
        .ret
        .as_ref()
        .map(|r| format!(" -> {}", format_type_expr(r)))
        .unwrap_or_default();
    let tparams = if f.type_params.is_empty() {
        String::new()
    } else {
        format!("<{}>", f.type_params.join(", "))
    };
    format!(
        "{}{}({}){}",
        f.name.as_deref().unwrap_or("<anonymous>"),
        tparams,
        params.join(", "),
        ret
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolDoc {
    pub kind: &'static str,
    pub name: String,
    pub vis: &'static str,
    pub signature: String,
    pub doc: Option<String>,
    pub failure_mode: Option<&'static str>,
    pub line: u32,
    pub children: Vec<SymbolDoc>,
}

fn scan_return_expr(
    e: &ast::Expr,
    has_throw: &mut bool,
    has_result: &mut bool,
    has_option: &mut bool,
) {
    match e {
        ast::Expr::Call { callee, .. } => {
            if let ast::Expr::Ident(name, _) = &**callee {
                match name.as_str() {
                    "ok" | "err" => *has_result = true,
                    "some" => *has_option = true,
                    _ => {}
                }
            }
        }
        ast::Expr::Ident(name, _) if name == "none" => *has_option = true,
        ast::Expr::If { then, els, .. } => {
            scan_stmts_failure(&then.stmts, has_throw, has_result, has_option);
            if let Some(Stmt::Expr(tail)) = then.stmts.last() {
                scan_return_expr(tail, has_throw, has_result, has_option);
            }
            if let Some(b) = els {
                scan_stmts_failure(&b.stmts, has_throw, has_result, has_option);
                if let Some(Stmt::Expr(tail)) = b.stmts.last() {
                    scan_return_expr(tail, has_throw, has_result, has_option);
                }
            }
        }
        ast::Expr::Match { arms, .. } => {
            for arm in arms {
                scan_stmts_failure(&arm.body.stmts, has_throw, has_result, has_option);
                if let Some(Stmt::Expr(tail)) = arm.body.stmts.last() {
                    scan_return_expr(tail, has_throw, has_result, has_option);
                }
            }
        }
        _ => {}
    }
}

fn scan_expr_blocks(
    e: &ast::Expr,
    has_throw: &mut bool,
    has_result: &mut bool,
    has_option: &mut bool,
) {
    match e {
        ast::Expr::If {
            cond, then, els, ..
        } => {
            scan_expr_blocks(cond, has_throw, has_result, has_option);
            scan_stmts_failure(&then.stmts, has_throw, has_result, has_option);
            if let Some(b) = els {
                scan_stmts_failure(&b.stmts, has_throw, has_result, has_option);
            }
        }
        ast::Expr::Match {
            scrutinee, arms, ..
        } => {
            scan_expr_blocks(scrutinee, has_throw, has_result, has_option);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    scan_expr_blocks(g, has_throw, has_result, has_option);
                }
                scan_stmts_failure(&arm.body.stmts, has_throw, has_result, has_option);
            }
        }
        ast::Expr::Call { callee, args, .. } => {
            scan_expr_blocks(callee, has_throw, has_result, has_option);
            for a in args {
                scan_expr_blocks(a, has_throw, has_result, has_option);
            }
        }
        ast::Expr::Binary { l, r, .. }
        | ast::Expr::Logical { l, r, .. }
        | ast::Expr::Coalesce { l, r, .. } => {
            scan_expr_blocks(l, has_throw, has_result, has_option);
            scan_expr_blocks(r, has_throw, has_result, has_option);
        }
        ast::Expr::Unary { expr, .. } | ast::Expr::Propagate(expr, _) => {
            scan_expr_blocks(expr, has_throw, has_result, has_option);
        }
        ast::Expr::Assign { target, value, .. } => {
            scan_expr_blocks(target, has_throw, has_result, has_option);
            scan_expr_blocks(value, has_throw, has_result, has_option);
        }
        _ => {}
    }
}

fn scan_stmts_failure(
    stmts: &[Stmt],
    has_throw: &mut bool,
    has_result: &mut bool,
    has_option: &mut bool,
) {
    for stmt in stmts {
        match stmt {
            Stmt::Throw { .. } => *has_throw = true,
            Stmt::Return { value: Some(e), .. } => {
                scan_expr_blocks(e, has_throw, has_result, has_option);
                scan_return_expr(e, has_throw, has_result, has_option);
            }
            Stmt::Let { value: Some(e), .. } | Stmt::Const { value: e, .. } | Stmt::Expr(e) => {
                scan_expr_blocks(e, has_throw, has_result, has_option);
            }
            Stmt::If {
                cond,
                then,
                elifs,
                els,
                ..
            } => {
                scan_expr_blocks(cond, has_throw, has_result, has_option);
                scan_stmts_failure(&then.stmts, has_throw, has_result, has_option);
                for (c, b) in elifs {
                    scan_expr_blocks(c, has_throw, has_result, has_option);
                    scan_stmts_failure(&b.stmts, has_throw, has_result, has_option);
                }
                if let Some(b) = els {
                    scan_stmts_failure(&b.stmts, has_throw, has_result, has_option);
                }
            }
            Stmt::While { body, .. } | Stmt::ForC { body, .. } | Stmt::ForIn { body, .. } => {
                scan_stmts_failure(&body.stmts, has_throw, has_result, has_option);
            }
            Stmt::Try {
                body,
                catch,
                finally,
                ..
            } => {
                // Only unguarded throws outside a catching try block escape;
                // still scan catch and finally blocks for rethrows / returns.
                if catch.is_none() {
                    scan_stmts_failure(&body.stmts, has_throw, has_result, has_option);
                } else {
                    let mut ignored_throw = false;
                    scan_stmts_failure(&body.stmts, &mut ignored_throw, has_result, has_option);
                }
                if let Some(c) = catch {
                    scan_stmts_failure(&c.body.stmts, has_throw, has_result, has_option);
                }
                if let Some(b) = finally {
                    scan_stmts_failure(&b.stmts, has_throw, has_result, has_option);
                }
            }
            Stmt::Block(b) => scan_stmts_failure(&b.stmts, has_throw, has_result, has_option),
            _ => {}
        }
    }
}

fn infer_fn_failure_mode(f: &ast::FnProto) -> &'static str {
    let mut has_throw = false;
    let mut has_result = false;
    let mut has_option = false;
    if let Some(ret) = &f.ret {
        match ret.name.as_str() {
            "result" | "Result" => has_result = true,
            "option" | "Option" => has_option = true,
            _ => {}
        }
    }
    scan_stmts_failure(
        &f.body.stmts,
        &mut has_throw,
        &mut has_result,
        &mut has_option,
    );
    match (has_result, has_option, has_throw) {
        (true, _, true) => "result|throws",
        (true, _, false) => "result",
        (false, true, true) => "option|throws",
        (false, true, false) => "option",
        (false, false, true) => "throws",
        (false, false, false) => "infallible",
    }
}

fn infer_ret_failure_mode(ret: &Option<ast::TypeExpr>) -> &'static str {
    if let Some(r) = ret {
        match r.name.as_str() {
            "result" | "Result" => return "result",
            "option" | "Option" => return "option",
            _ => {}
        }
    }
    "unspecified"
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleDoc {
    pub file: String,
    pub module_doc: Option<String>,
    pub symbols: Vec<SymbolDoc>,
}

fn vis_str(v: Vis) -> &'static str {
    match v {
        Vis::Public => "public",
        Vis::Private => "private",
    }
}

fn vis_prefix(v: Vis) -> &'static str {
    match v {
        Vis::Public => "public ",
        Vis::Private => "",
    }
}

fn opt_colon_type(ty: &Option<ast::TypeExpr>) -> String {
    let ts = type_str(ty);
    if ts.is_empty() {
        String::new()
    } else {
        format!(": {}", ts)
    }
}

fn stmt_is_public(s: &Stmt) -> bool {
    match s {
        Stmt::Fn(f) => f.vis == Vis::Public,
        Stmt::Class(c) => c.vis == Vis::Public,
        Stmt::Interface(i) => i.vis == Vis::Public,
        Stmt::Enum(e) => e.vis == Vis::Public,
        Stmt::Let { vis, .. } | Stmt::Const { vis, .. } | Stmt::LetTuple { vis, .. } => {
            *vis == Vis::Public
        }
        _ => false,
    }
}

fn stmt_line(s: &Stmt) -> Option<u32> {
    let sp = s.span();
    (sp.line > 0).then_some(sp.line)
}

/// Optional top-of-file module doc comment when comments begin on line 1 and
/// are not immediately adjacent to the first statement's line.
fn extract_module_doc(comments: &[Comment], program: &Program) -> Option<String> {
    let mut sorted: Vec<&Comment> = comments.iter().collect();
    sorted.sort_by_key(|c| c.line);
    let mut lines: Vec<String> = Vec::new();
    let mut expected_line: u32 = 1;
    let mut last_line: u32 = 0;
    for c in sorted {
        if c.line == expected_line {
            lines.push(strip_comment(&c.text));
            last_line = c.line;
            expected_line += 1;
        } else if c.line > expected_line {
            break;
        }
    }
    if lines.is_empty() {
        return None;
    }
    if let Some(first_stmt_line) = program.stmts.iter().filter_map(stmt_line).min() {
        if last_line + 1 >= first_stmt_line {
            return None;
        }
    }
    Some(lines.join("\n"))
}

/// Parse and extract documentation items from a single `.matrix` source string.
pub fn extract_module_docs(
    display_path: &str,
    src: &str,
    include_private: bool,
) -> Result<ModuleDoc, Diagnostic> {
    let lexed = Lexer::new(src, Arc::from(display_path)).tokenize_with_comments()?;
    let program = parser::parse(lexed.tokens)?;
    let module_doc = extract_module_doc(&lexed.comments, &program);
    let has_public_top = program.stmts.iter().any(stmt_is_public);
    let allow_private_top = include_private || !has_public_top;

    let mut symbols = Vec::new();
    for stmt in &program.stmts {
        match stmt {
            Stmt::Fn(f) => {
                let Some(name) = &f.name else { continue };
                if !allow_private_top && f.vis != Vis::Public {
                    continue;
                }
                let sig = format!(
                    "{}{}{}fn {}",
                    vis_prefix(f.vis),
                    if f.is_async { "async " } else { "" },
                    if f.is_extern { "extern " } else { "" },
                    fn_signature(f)
                );
                symbols.push(SymbolDoc {
                    kind: "fn",
                    name: name.clone(),
                    vis: vis_str(f.vis),
                    signature: sig,
                    doc: doc_above(&f.span, &lexed.comments),
                    failure_mode: Some(infer_fn_failure_mode(f)),
                    line: f.span.line,
                    children: Vec::new(),
                });
            }
            Stmt::Class(c) => {
                if !allow_private_top && c.vis != Vis::Public {
                    continue;
                }
                let tparams = if c.type_params.is_empty() {
                    String::new()
                } else {
                    format!("<{}>", c.type_params.join(", "))
                };
                let mut bases = String::new();
                if let Some(p) = &c.parent {
                    bases.push_str(&format!(" extends {}", p));
                }
                if !c.interfaces.is_empty() {
                    bases.push_str(&format!(" implements {}", c.interfaces.join(", ")));
                }
                let sig = format!(
                    "{}{}class {}{}{}",
                    vis_prefix(c.vis),
                    if c.is_abstract { "abstract " } else { "" },
                    c.name,
                    tparams,
                    bases
                );

                let has_public_member = c.fields.iter().any(|f| f.vis == Vis::Public)
                    || c.methods.iter().any(|m| m.vis == Vis::Public);
                let allow_private_member = include_private || !has_public_member;
                let mut children = Vec::new();
                for field in &c.fields {
                    if !allow_private_member && field.vis != Vis::Public {
                        continue;
                    }
                    children.push(SymbolDoc {
                        kind: "field",
                        name: field.name.clone(),
                        vis: vis_str(field.vis),
                        signature: format!(
                            "{}{}{}",
                            vis_prefix(field.vis),
                            field.name,
                            opt_colon_type(&field.ty)
                        ),
                        doc: doc_above(&field.span, &lexed.comments),
                        failure_mode: None,
                        line: field.span.line,
                        children: Vec::new(),
                    });
                }
                for m in &c.methods {
                    let Some(m_name) = &m.name else { continue };
                    if !allow_private_member && m.vis != Vis::Public {
                        continue;
                    }
                    children.push(SymbolDoc {
                        kind: "method",
                        name: m_name.clone(),
                        vis: vis_str(m.vis),
                        signature: format!(
                            "{}{}fn {}",
                            vis_prefix(m.vis),
                            if m.is_async { "async " } else { "" },
                            fn_signature(m)
                        ),
                        doc: doc_above(&m.span, &lexed.comments),
                        failure_mode: Some(infer_fn_failure_mode(m)),
                        line: m.span.line,
                        children: Vec::new(),
                    });
                }
                symbols.push(SymbolDoc {
                    kind: "class",
                    name: c.name.clone(),
                    vis: vis_str(c.vis),
                    signature: sig,
                    doc: doc_above(&c.span, &lexed.comments),
                    failure_mode: None,
                    line: c.span.line,
                    children,
                });
            }
            Stmt::Interface(i) => {
                if !allow_private_top && i.vis != Vis::Public {
                    continue;
                }
                let mut children = Vec::new();
                for (m_name, params, ret, m_span) in &i.methods {
                    let param_str: Vec<String> = params
                        .iter()
                        .map(|p| {
                            let t = type_str(&p.ty);
                            if t.is_empty() {
                                p.name.clone()
                            } else {
                                format!("{}: {}", p.name, t)
                            }
                        })
                        .collect();
                    let ret_str = ret
                        .as_ref()
                        .map(|r| format!(" -> {}", format_type_expr(r)))
                        .unwrap_or_default();
                    children.push(SymbolDoc {
                        kind: "method",
                        name: m_name.clone(),
                        vis: "public",
                        signature: format!("fn {}({}){}", m_name, param_str.join(", "), ret_str),
                        doc: doc_above(m_span, &lexed.comments),
                        failure_mode: Some(infer_ret_failure_mode(ret)),
                        line: m_span.line,
                        children: Vec::new(),
                    });
                }
                for m in &i.default_methods {
                    let Some(m_name) = &m.name else { continue };
                    children.push(SymbolDoc {
                        kind: "method",
                        name: m_name.clone(),
                        vis: vis_str(m.vis),
                        signature: format!("fn {}", fn_signature(m)),
                        doc: doc_above(&m.span, &lexed.comments),
                        failure_mode: Some(infer_fn_failure_mode(m)),
                        line: m.span.line,
                        children: Vec::new(),
                    });
                }
                symbols.push(SymbolDoc {
                    kind: "interface",
                    name: i.name.clone(),
                    vis: vis_str(i.vis),
                    signature: format!("{}interface {}", vis_prefix(i.vis), i.name),
                    doc: doc_above(&i.span, &lexed.comments),
                    failure_mode: None,
                    line: i.span.line,
                    children,
                });
            }
            Stmt::Enum(e) => {
                if !allow_private_top && e.vis != Vis::Public {
                    continue;
                }
                let children = e
                    .variants
                    .iter()
                    .map(|v| SymbolDoc {
                        kind: "variant",
                        name: v.name.clone(),
                        vis: vis_str(e.vis),
                        signature: if v.fields.is_empty() {
                            v.name.clone()
                        } else {
                            format!("{}({})", v.name, v.fields.join(", "))
                        },
                        doc: doc_above(&v.span, &lexed.comments),
                        failure_mode: None,
                        line: v.span.line,
                        children: Vec::new(),
                    })
                    .collect();
                symbols.push(SymbolDoc {
                    kind: "enum",
                    name: e.name.clone(),
                    vis: vis_str(e.vis),
                    signature: format!("{}enum {}", vis_prefix(e.vis), e.name),
                    doc: doc_above(&e.span, &lexed.comments),
                    failure_mode: None,
                    line: e.span.line,
                    children,
                });
            }
            Stmt::Const {
                vis,
                name,
                ty,
                span,
                ..
            } => {
                if !allow_private_top && *vis != Vis::Public {
                    continue;
                }
                symbols.push(SymbolDoc {
                    kind: "const",
                    name: name.clone(),
                    vis: vis_str(*vis),
                    signature: format!("{}const {}{}", vis_prefix(*vis), name, opt_colon_type(ty)),
                    doc: doc_above(span, &lexed.comments),
                    failure_mode: None,
                    line: span.line,
                    children: Vec::new(),
                });
            }
            Stmt::Let {
                vis,
                name,
                ty,
                span,
                ..
            } => {
                if !allow_private_top && *vis != Vis::Public {
                    continue;
                }
                symbols.push(SymbolDoc {
                    kind: "let",
                    name: name.clone(),
                    vis: vis_str(*vis),
                    signature: format!("{}let {}{}", vis_prefix(*vis), name, opt_colon_type(ty)),
                    doc: doc_above(span, &lexed.comments),
                    failure_mode: None,
                    line: span.line,
                    children: Vec::new(),
                });
            }
            _ => {}
        }
    }

    Ok(ModuleDoc {
        file: display_path.to_string(),
        module_doc,
        symbols,
    })
}

/// Extract documentation from disk paths (`.matrix` files or directories).
pub fn extract_docs_for_paths(
    paths: &[PathBuf],
    include_private: bool,
) -> Result<Vec<ModuleDoc>, Diagnostic> {
    let files = collect_matrix_files(paths)?;
    let mut modules = Vec::with_capacity(files.len());
    for file in files {
        let display = file.to_string_lossy().replace('\\', "/");
        let src = std::fs::read_to_string(&file).map_err(|e| {
            Diagnostic::new(
                Phase::Usage,
                format!("cannot read '{}': {}", file.display(), e),
            )
        })?;
        modules.push(extract_module_docs(&display, &src, include_private)?);
    }
    Ok(modules)
}

fn collect_matrix_files(inputs: &[PathBuf]) -> Result<Vec<PathBuf>, Diagnostic> {
    let mut out = Vec::new();
    for p in inputs {
        if p.is_file() {
            out.push(p.clone());
        } else if p.is_dir() {
            walk_matrix_dir(p, &mut out)?;
        } else {
            return Err(Diagnostic::new(
                Phase::Usage,
                format!("path '{}' does not exist", p.display()),
            ));
        }
    }
    out.sort();
    out.dedup();
    if out.is_empty() {
        return Err(Diagnostic::new(
            Phase::Usage,
            "no .matrix source files found to document",
        ));
    }
    Ok(out)
}

fn walk_matrix_dir(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Diagnostic> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        Diagnostic::new(
            Phase::Usage,
            format!("cannot read directory '{}': {}", dir.display(), e),
        )
    })?;
    let mut items: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        items.push(entry.path());
    }
    items.sort();
    for path in items {
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if matches!(name, "target" | "vendor" | ".spid3r" | ".git") {
            continue;
        }
        if path.is_dir() {
            walk_matrix_dir(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("matrix") {
            out.push(path);
        }
    }
    Ok(())
}

fn json_escape(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_opt_json_str(opt: Option<&str>, out: &mut String) {
    match opt {
        Some(s) => json_escape(s, out),
        None => out.push_str("null"),
    }
}

fn write_symbol_json(sym: &SymbolDoc, out: &mut String, indent: usize) {
    let pad = " ".repeat(indent);
    let inner = " ".repeat(indent + 2);
    let _ = writeln!(out, "{}{{", pad);
    let _ = write!(out, "{}\"kind\": ", inner);
    json_escape(sym.kind, out);
    out.push_str(",\n");
    let _ = write!(out, "{}\"name\": ", inner);
    json_escape(&sym.name, out);
    out.push_str(",\n");
    let _ = write!(out, "{}\"vis\": ", inner);
    json_escape(sym.vis, out);
    out.push_str(",\n");
    let _ = write!(out, "{}\"signature\": ", inner);
    json_escape(&sym.signature, out);
    out.push_str(",\n");
    let _ = write!(out, "{}\"doc\": ", inner);
    write_opt_json_str(sym.doc.as_deref(), out);
    out.push_str(",\n");
    let _ = write!(out, "{}\"failure_mode\": ", inner);
    write_opt_json_str(sym.failure_mode, out);
    out.push_str(",\n");
    let _ = writeln!(out, "{}\"line\": {},", inner, sym.line);
    let _ = write!(out, "{}\"children\": [", inner);
    if sym.children.is_empty() {
        out.push_str("]\n");
    } else {
        out.push('\n');
        for (idx, child) in sym.children.iter().enumerate() {
            write_symbol_json(child, out, indent + 4);
            if idx + 1 < sym.children.len() {
                out.push(',');
            }
            out.push('\n');
        }
        let _ = writeln!(out, "{}]", inner);
    }
    let _ = write!(out, "{}}}", pad);
}

/// Render extracted documentation modules as deterministic formatted JSON.
pub fn render_json(modules: &[ModuleDoc]) -> String {
    let mut out = String::from("{\n  \"modules\": [\n");
    for (m_idx, m) in modules.iter().enumerate() {
        out.push_str("    {\n      \"file\": ");
        json_escape(&m.file, &mut out);
        out.push_str(",\n      \"module_doc\": ");
        write_opt_json_str(m.module_doc.as_deref(), &mut out);
        out.push_str(",\n      \"symbols\": [");
        if m.symbols.is_empty() {
            out.push_str("]\n");
        } else {
            out.push('\n');
            for (s_idx, sym) in m.symbols.iter().enumerate() {
                write_symbol_json(sym, &mut out, 8);
                if s_idx + 1 < m.symbols.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str("      ]\n");
        }
        out.push_str("    }");
        if m_idx + 1 < modules.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn render_doc_paragraphs(doc: Option<&str>, out: &mut String) {
    if let Some(text) = doc {
        if !text.trim().is_empty() {
            out.push_str("<div class=\"doc\">");
            for para in text.split("\n\n") {
                let p = html_escape(para.trim()).replace('\n', "<br>");
                let _ = write!(out, "<p>{}</p>", p);
            }
            out.push_str("</div>");
        }
    }
}

/// Render a self-contained single-file HTML reference for the documented modules.
pub fn render_html(modules: &[ModuleDoc]) -> String {
    let mut out = String::from(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n\
<title>Spid3r API Documentation</title>\n<style>\n\
:root{--bg:#0b0f14;--panel:#121821;--border:#1f2a37;--text:#e5e7eb;--muted:#9ca3af;--accent:#38bdf8;--code:#0f172a;}\n\
*{box-sizing:border-box;}body{margin:0;background:var(--bg);color:var(--text);font-family:system-ui,-apple-system,Segoe UI,Roboto,sans-serif;display:flex;min-height:100vh;}\n\
nav{width:270px;background:var(--panel);border-right:1px solid var(--border);padding:1rem;position:sticky;top:0;height:100vh;overflow:auto;flex-shrink:0;}\n\
nav h1{font-size:1rem;margin:0 0 .75rem 0;color:var(--accent);}\n\
nav input{width:100%;padding:.45rem .6rem;margin-bottom:.75rem;background:var(--bg);color:var(--text);border:1px solid var(--border);border-radius:6px;}\n\
nav a{display:block;color:var(--text);text-decoration:none;padding:.22rem .35rem;border-radius:4px;font-size:.86rem;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;}\n\
nav a:hover{background:#1b2432;color:var(--accent);}\n\
nav .mod-link{font-weight:600;margin-top:.6rem;color:var(--accent);}\n\
nav .sym-link{padding-left:1rem;color:var(--muted);}\n\
main{flex:1;padding:2rem 2.5rem;max-width:980px;}\n\
section.module{margin-bottom:2.5rem;}\n\
h2.mod-title{border-bottom:1px solid var(--border);padding-bottom:.4rem;color:var(--accent);font-size:1.35rem;}\n\
article.symbol{background:var(--panel);border:1px solid var(--border);border-radius:8px;padding:1rem 1.15rem;margin:.9rem 0;}\n\
.sym-head{display:flex;align-items:center;gap:.55rem;flex-wrap:wrap;}\n\
.badge{font-size:.72rem;text-transform:uppercase;letter-spacing:.04em;padding:.15rem .45rem;border-radius:4px;background:#1e293b;color:var(--accent);}\n\
pre.sig{margin:.4rem 0 0 0;padding:.5rem .75rem;background:var(--code);border-radius:6px;overflow:auto;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.9rem;}\n\
.doc{margin-top:.6rem;line-height:1.5;color:#d1d5db;font-size:.93rem;}.doc p{margin:.35rem 0;}\n\
.children{margin-top:.75rem;padding-left:1rem;border-left:2px solid var(--border);}\n\
.child{margin:.55rem 0;}\n\
.hidden{display:none!important;}\n\
</style>\n</head>\n<body>\n<nav>\n<h1>Spid3r Docs</h1>\n\
<input id=\"doc-search\" type=\"search\" placeholder=\"Filter symbols...\" oninput=\"filterDocs(this.value)\">\n",
    );

    for (m_idx, m) in modules.iter().enumerate() {
        let mod_id = format!("mod-{}", m_idx);
        let _ = writeln!(
            out,
            "<a class=\"mod-link\" href=\"#{}\">{}</a>",
            mod_id,
            html_escape(&m.file)
        );
        for (s_idx, sym) in m.symbols.iter().enumerate() {
            let sym_id = format!("sym-{}-{}", m_idx, s_idx);
            let _ = writeln!(
                out,
                "<a class=\"sym-link\" data-name=\"{}\" href=\"#{}\">{} {}</a>",
                html_escape(&sym.name.to_lowercase()),
                sym_id,
                html_escape(sym.kind),
                html_escape(&sym.name)
            );
        }
    }

    out.push_str("</nav>\n<main>\n");
    for (m_idx, m) in modules.iter().enumerate() {
        let mod_id = format!("mod-{}", m_idx);
        let _ = writeln!(
            out,
            "<section class=\"module\" id=\"{}\">\n<h2 class=\"mod-title\">{}</h2>",
            mod_id,
            html_escape(&m.file)
        );
        render_doc_paragraphs(m.module_doc.as_deref(), &mut out);
        for (s_idx, sym) in m.symbols.iter().enumerate() {
            let sym_id = format!("sym-{}-{}", m_idx, s_idx);
            let _ = writeln!(
                out,
                "<article class=\"symbol\" id=\"{}\" data-name=\"{}\">\n\
<div class=\"sym-head\"><span class=\"badge\">{}</span><strong>{}</strong></div>\n\
<pre class=\"sig\"><code>{}</code></pre>",
                sym_id,
                html_escape(&sym.name.to_lowercase()),
                html_escape(sym.kind),
                html_escape(&sym.name),
                html_escape(&sym.signature)
            );
            render_doc_paragraphs(sym.doc.as_deref(), &mut out);
            if !sym.children.is_empty() {
                out.push_str("<div class=\"children\">\n");
                for child in &sym.children {
                    let _ = writeln!(
                        out,
                        "<div class=\"child\"><span class=\"badge\">{}</span> <code>{}</code>",
                        html_escape(child.kind),
                        html_escape(&child.signature)
                    );
                    render_doc_paragraphs(child.doc.as_deref(), &mut out);
                    out.push_str("</div>\n");
                }
                out.push_str("</div>\n");
            }
            out.push_str("</article>\n");
        }
        out.push_str("</section>\n");
    }

    out.push_str(
        "</main>\n<script>\nfunction filterDocs(q){\n\
q=(q||'').trim().toLowerCase();\n\
document.querySelectorAll('article.symbol, a.sym-link').forEach(function(el){\n\
var name=el.getAttribute('data-name')||'';\n\
el.classList.toggle('hidden', q!=='' && name.indexOf(q)===-1);\n\
});\n}\n</script>\n</body>\n</html>\n",
    );
    out
}

/// CLI entrypoint for `spid3r doc`.
pub fn cmd_doc(args: &[String]) -> Result<i32, Diagnostic> {
    let mut json = false;
    let mut include_private = false;
    let mut out_path: Option<PathBuf> = None;
    let mut inputs: Vec<PathBuf> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--json" => json = true,
            "--private" => include_private = true,
            "-o" | "--out" => {
                i += 1;
                if i >= args.len() {
                    return Err(Diagnostic::new(
                        Phase::Usage,
                        format!("usage error: {} requires an output path", a),
                    ));
                }
                out_path = Some(PathBuf::from(&args[i]));
            }
            s if s.starts_with("--out=") => {
                out_path = Some(PathBuf::from(&s["--out=".len()..]));
            }
            s if s.starts_with('-') => {
                return Err(Diagnostic::new(
                    Phase::Usage,
                    format!("usage error: unknown `doc` option '{}'", s),
                ));
            }
            path => inputs.push(PathBuf::from(path)),
        }
        i += 1;
    }

    if inputs.is_empty() {
        let src_dir = PathBuf::from("src");
        if src_dir.is_dir() {
            inputs.push(src_dir);
        } else {
            inputs.push(PathBuf::from("."));
        }
    }

    let modules = extract_docs_for_paths(&inputs, include_private)?;
    let rendered = if json {
        render_json(&modules)
    } else {
        render_html(&modules)
    };

    if let Some(target) = out_path {
        let dest = if target.is_dir()
            || target
                .to_str()
                .is_some_and(|s| s.ends_with('/') || s.ends_with('\\'))
        {
            target.join(if json { "docs.json" } else { "index.html" })
        } else {
            target
        };
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    Diagnostic::new(
                        Phase::Usage,
                        format!("cannot create directory '{}': {}", parent.display(), e),
                    )
                })?;
            }
        }
        std::fs::write(&dest, rendered.as_bytes()).map_err(|e| {
            Diagnostic::new(
                Phase::Usage,
                format!("cannot write '{}': {}", dest.display(), e),
            )
        })?;
    } else {
        print!("{}", rendered);
    }

    Ok(0)
}
