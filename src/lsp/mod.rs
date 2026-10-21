//! `spid3r lsp` — a Language Server Protocol server over stdio.
//!
//! JSON-RPC 2.0 with `Content-Length` framing, implementing the subset an
//! editor needs to work on a dynamic scripting language:
//!
//! - `textDocument/publishDiagnostics` on open/change — the *same* lexer,
//!   parser, semantic checker and lint pass `spid3r check` uses, so the editor
//!   and the CLI can never disagree about a problem.
//! - `textDocument/hover`, `textDocument/definition` — symbol navigation from
//!   an index built out of the AST (top-level declarations plus each function
//!   body's locals), with doc comments recovered from the lexer's comment
//!   side-channel.
//! - `textDocument/documentSymbol` — the outline tree.
//! - `textDocument/completion` — file symbols and keywords.
//! - `textDocument/formatting` — the same formatter as `spid3r fmt`.
//!
//! Wire positions are `(line, character)` **character** offsets, 0-based,
//! which is what Spid3r's diagnostics already use. See `docs/LSP.md`.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::sync::Arc;

use crate::ast::{self, Block, Expr, Program, Stmt};
use crate::diagnostics::{Diagnostic, Severity, Span};
use crate::lexer::token::{Token, TokenKind};
use crate::lexer::{Comment, Lexer};
use crate::parser;
use crate::semantic;
use crate::value::collections::MapKey;
use crate::value::Value;

// ---------------------------------------------------------------- JSON layer

/// A JSON value.
///
/// The stdlib already owns a JSON reader and writer (`json_parse` /
/// `json_write`, both exercised by tests), so the LSP parses inbound messages
/// into that `Value` and converts once into this lock-free shape. Outbound
/// messages are built directly as `Json` and serialized here, which avoids
/// going through the runtime's `Arc<Mutex<..>>` collections at all.
#[derive(Clone, Debug)]
enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(o) => o.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }
    fn as_i(&self) -> Option<i64> {
        match self {
            Json::Int(i) => Some(*i),
            _ => None,
        }
    }
    fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }
    fn arr(&self) -> Option<&Vec<Json>> {
        match self {
            Json::Arr(a) => Some(a),
            _ => None,
        }
    }
    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Int(i) => out.push_str(&i.to_string()),
            Json::Str(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\t' => out.push_str("\\t"),
                        '\r' => out.push_str("\\r"),
                        c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            Json::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Json::Obj(o) => {
                out.push('{');
                for (i, (k, v)) in o.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    Json::Str(k.clone()).write(out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// Convert the runtime `Value` produced by [`crate::stdlib::json_parse_text`]
/// into the lock-free [`Json`] shape.
fn from_value(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(i) => Json::Int(*i),
        Value::Float(f) => Json::Int(*f as i64),
        Value::Str(s) => Json::Str(s.to_string()),
        Value::List(d) => {
            let items = &d.lock().unwrap().items;
            Json::Arr(items.iter().map(from_value).collect())
        }
        Value::Map(d) => {
            let entries = &d.lock().unwrap().entries;
            let mut obj = Vec::new();
            for (k, val) in entries {
                let key = match k {
                    MapKey::Null => "null".to_string(),
                    MapKey::Bool(b) => b.to_string(),
                    MapKey::Int(i) => i.to_string(),
                    MapKey::Str(s) => s.to_string(),
                };
                obj.push((key, from_value(val)));
            }
            Json::Obj(obj)
        }
        _ => Json::Null,
    }
}

// --------------------------------------------------- positions, spans, extents

/// A document position: 0-based `(line, character)`, matching Spid3r's
/// character-based diagnostic columns.
type Pos = (u32, u32);

/// A rectangular region `(start_line, start_char, end_line, end_char)`, 0-based.
/// Used for scope regions, where a name is visible.
type Extent = (u32, u32, u32, u32);

fn cmp_pos(a: Pos, b: Pos) -> std::cmp::Ordering {
    a.0.cmp(&b.0).then(a.1.cmp(&b.1))
}

fn contains(e: Extent, p: Pos) -> bool {
    cmp_pos((e.0, e.1), p) != std::cmp::Ordering::Greater
        && cmp_pos(p, (e.2, e.3)) != std::cmp::Ordering::Greater
}

fn extent_size(e: Extent) -> u64 {
    let lines = e.2.saturating_sub(e.0);
    let chars = if lines == 0 {
        e.3.saturating_sub(e.1)
    } else {
        0
    };
    u64::from(lines) * 1_000_000 + u64::from(chars)
}

/// A Spid3r `Span` (1-based line/col, `len` in characters, single line) as an
/// LSP `Range`.
fn span_range(s: &Span) -> Json {
    let line = s.line.saturating_sub(1);
    let col = s.col.saturating_sub(1);
    Json::Obj(vec![
        (
            "start".into(),
            Json::Obj(vec![
                ("line".into(), Json::Int(i64::from(line))),
                ("character".into(), Json::Int(i64::from(col))),
            ]),
        ),
        (
            "end".into(),
            Json::Obj(vec![
                ("line".into(), Json::Int(i64::from(line))),
                ("character".into(), Json::Int(i64::from(col + s.len))),
            ]),
        ),
    ])
}

fn span_extent(s: &Span) -> Extent {
    let line = s.line.saturating_sub(1);
    let col = s.col.saturating_sub(1);
    (line, col, line, col + s.len)
}

/// The whole-document extent, for declarations that are visible everywhere.
fn whole_extent(text: &str) -> Extent {
    let lines: Vec<&str> = text.split('\n').collect();
    let last_line = lines.len().saturating_sub(1) as u32;
    let last_char = lines.last().map(|l| l.chars().count()).unwrap_or(0) as u32;
    (0, 0, last_line, last_char)
}

/// A full-document `Range`, for formatting edits.
fn full_range(text: &str) -> Json {
    let e = whole_extent(text);
    Json::Obj(vec![
        (
            "start".into(),
            Json::Obj(vec![
                ("line".into(), Json::Int(0)),
                ("character".into(), Json::Int(0)),
            ]),
        ),
        (
            "end".into(),
            Json::Obj(vec![
                ("line".into(), Json::Int(i64::from(e.2))),
                ("character".into(), Json::Int(i64::from(e.3))),
            ]),
        ),
    ])
}

/// Byte offset of a document position, for locating the token under the cursor.
fn byte_offset(text: &str, pos: Pos) -> usize {
    let mut off = 0usize;
    for (i, line) in text.split('\n').enumerate() {
        if i as u32 == pos.0 {
            let skip: usize = line
                .chars()
                .take(pos.1 as usize)
                .map(|c| c.len_utf8())
                .sum();
            return off + skip;
        }
        off += line.len() + 1; // + 1 for the '\n' that `split` removed
    }
    text.len()
}

// ------------------------------------------------------------- symbol index

/// What a declaration is, mapped onto LSP `SymbolKind` numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SymKind {
    Function,
    Method,
    Class,
    Interface,
    Enum,
    Variant,
    Field,
    Let,
    Const,
    Parameter,
}

fn kind_num(k: SymKind) -> i64 {
    match k {
        SymKind::Function => 12,
        SymKind::Method => 6,
        SymKind::Class => 5,
        SymKind::Interface => 11,
        SymKind::Enum => 10,
        SymKind::Variant => 22,
        SymKind::Field => 8,
        SymKind::Let => 13,
        SymKind::Const => 14,
        SymKind::Parameter => 13,
    }
}

fn kind_word(k: SymKind) -> &'static str {
    match k {
        SymKind::Function => "function",
        SymKind::Method => "method",
        SymKind::Class => "class",
        SymKind::Interface => "interface",
        SymKind::Enum => "enum",
        SymKind::Variant => "variant",
        SymKind::Field => "field",
        SymKind::Let => "variable",
        SymKind::Const => "constant",
        SymKind::Parameter => "parameter",
    }
}

/// One name and where it is declared. `extent` is the region over which the
/// name resolves — a function's parameter is only visible in its body, a
/// top-level declaration everywhere in the file.
struct Decl {
    name: String,
    kind: SymKind,
    def_span: Span,
    extent: Extent,
    detail: String,
    doc: Option<String>,
}

/// A comment run immediately above a declaration, markers stripped.
fn doc_above(span: &Span, comments: &[Comment]) -> Option<String> {
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

fn strip_comment(t: &str) -> String {
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

fn type_str(ty: &Option<ast::TypeExpr>) -> String {
    match ty {
        Some(t) => {
            let mut s = t.name.clone();
            if let Some(arg) = &t.arg {
                s.push('<');
                s.push_str(&type_str(&Some((**arg).clone())));
                s.push('>');
            }
            s
        }
        None => String::new(),
    }
}

fn fn_signature(f: &ast::FnProto) -> String {
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
        .map(|r| format!(" -> {}", r.name))
        .unwrap_or_default();
    format!(
        "{}({}){}",
        f.name.as_deref().unwrap_or("<anonymous>"),
        params.join(", "),
        ret
    )
}

/// Grow `[start, end]` to cover a span.
fn grow(start: &mut Pos, end: &mut Pos, sp: &Span) {
    let ss = (sp.line.saturating_sub(1), sp.col.saturating_sub(1));
    let ee = (sp.line.saturating_sub(1), sp.col.saturating_sub(1) + sp.len);
    if cmp_pos(ss, *start) == std::cmp::Ordering::Less {
        *start = ss;
    }
    if cmp_pos(ee, *end) == std::cmp::Ordering::Greater {
        *end = ee;
    }
}

/// The textual region a block covers: the union of its statements, nested ones
/// included. A declaration's *visibility* extent is the whole enclosing body,
/// so a local declared inside an `if` is still offered as a hover target in the
/// rest of the function — over-permissive in one direction, never silent in
/// the other.
fn block_extent(b: &Block) -> Extent {
    let mut start: Pos = (u32::MAX, u32::MAX);
    let mut end: Pos = (0, 0);
    for s in &b.stmts {
        grow(&mut start, &mut end, s.span());
        stmt_extent(s, &mut start, &mut end);
    }
    if start.0 == u32::MAX {
        span_extent(&b.span)
    } else {
        (start.0, start.1, end.0, end.1)
    }
}

fn stmt_extent(s: &Stmt, start: &mut Pos, end: &mut Pos) {
    match s {
        Stmt::If {
            cond,
            then,
            elifs,
            els,
            span,
        } => {
            grow(start, end, span);
            expr_extent(cond, start, end);
            grow(start, end, &then.span);
            for (c, b) in elifs {
                expr_extent(c, start, end);
                grow(start, end, &b.span);
            }
            if let Some(b) = els {
                grow(start, end, &b.span);
            }
        }
        Stmt::While { cond, body, span } => {
            grow(start, end, span);
            expr_extent(cond, start, end);
            grow(start, end, &body.span);
        }
        Stmt::ForIn {
            iter, body, span, ..
        } => {
            grow(start, end, span);
            expr_extent(iter, start, end);
            grow(start, end, &body.span);
        }
        Stmt::ForC {
            cond,
            post,
            body,
            span,
            ..
        } => {
            grow(start, end, span);
            expr_extent(cond, start, end);
            expr_extent(post, start, end);
            grow(start, end, &body.span);
        }
        Stmt::Block(b) => grow(start, end, &b.span),
        Stmt::Try {
            body,
            catch,
            finally,
            span,
        } => {
            grow(start, end, span);
            grow(start, end, &body.span);
            if let Some(c) = catch {
                grow(start, end, &c.span);
            }
            if let Some(b) = finally {
                grow(start, end, &b.span);
            }
        }
        Stmt::Spawn { callable, span } => {
            grow(start, end, span);
            expr_extent(callable, start, end);
        }
        Stmt::Throw { value, span } => {
            grow(start, end, span);
            expr_extent(value, start, end);
        }
        Stmt::Expr(e) => expr_extent(e, start, end),
        // `Return`'s own span stops at the keyword, so the value has to be
        // grown separately or the body's extent ends at `return`.
        Stmt::Return { value: Some(v), .. } => expr_extent(v, start, end),
        _ => {}
    }
}

/// Grow over an expression.
///
/// Every `Expr` span covers its entire source text (arguments, operands,
/// branches included), so growing over it is enough — there is no need to walk
/// into the children to keep a region complete. A nested `fn` literal is a
/// scope of its own and is registered separately in [`collect_locals_stmt`].
fn expr_extent(e: &Expr, start: &mut Pos, end: &mut Pos) {
    grow(start, end, e.span());
}

/// Collect the locals of a statement (and the blocks it nests) into `decls`,
/// each visible over `extent`.
fn collect_locals_stmt(s: &Stmt, extent: Extent, decls: &mut Vec<Decl>) {
    let add = |name: &str, kind: SymKind, span: &Span, ty: String, decls: &mut Vec<Decl>| {
        decls.push(Decl {
            name: name.to_string(),
            kind,
            def_span: span.clone(),
            extent,
            detail: ty,
            doc: None,
        });
    };
    match s {
        Stmt::Let { name, ty, span, .. } => add(name, SymKind::Let, span, type_str(ty), decls),
        Stmt::Const { name, ty, span, .. } => add(name, SymKind::Const, span, type_str(ty), decls),
        Stmt::LetTuple { names, span, .. } => {
            for n in names {
                add(n, SymKind::Let, span, String::new(), decls);
            }
        }
        Stmt::ForIn {
            var,
            var2,
            span,
            body,
            ..
        } => {
            add(var, SymKind::Let, span, String::new(), decls);
            if let Some(v) = var2 {
                add(v, SymKind::Let, span, String::new(), decls);
            }
            // Narrow to the loop body: a loop variable is not visible outside
            // it, and a same-named binding inside must shadow (rename needs
            // that distinction).
            let be = block_extent(body);
            for s in &body.stmts {
                collect_locals_stmt(s, be, decls);
            }
        }
        Stmt::ForC { init, body, .. } => {
            collect_locals_stmt(init, extent, decls);
            let be = block_extent(body);
            for s in &body.stmts {
                collect_locals_stmt(s, be, decls);
            }
        }
        Stmt::If {
            then, elifs, els, ..
        } => {
            for b in [then].into_iter().chain(elifs.iter().map(|(_, b)| b)) {
                let be = block_extent(b);
                for s in &b.stmts {
                    collect_locals_stmt(s, be, decls);
                }
            }
            if let Some(b) = els {
                let be = block_extent(b);
                for s in &b.stmts {
                    collect_locals_stmt(s, be, decls);
                }
            }
        }
        Stmt::While { body, .. } | Stmt::Block(body) => {
            let be = block_extent(body);
            for s in &body.stmts {
                collect_locals_stmt(s, be, decls);
            }
        }
        Stmt::Try {
            body,
            catch,
            finally,
            ..
        } => {
            let be = block_extent(body);
            for s in &body.stmts {
                collect_locals_stmt(s, be, decls);
            }
            if let Some(c) = catch {
                let ce = block_extent(&c.body);
                for s in &c.body.stmts {
                    collect_locals_stmt(s, ce, decls);
                }
            }
            if let Some(b) = finally {
                let fe = block_extent(b);
                for s in &b.stmts {
                    collect_locals_stmt(s, fe, decls);
                }
            }
        }
        _ => {}
    }
}

fn collect_locals(b: &Block, extent: Extent, decls: &mut Vec<Decl>) {
    for s in &b.stmts {
        collect_locals_stmt(s, extent, decls);
    }
}

/// Build the flat declaration index used for hover, definition and completion.
///
/// Top-level declarations resolve over the whole file; a function's parameters
/// and locals resolve over its body, so an inner `let x` shadows an outer one
/// by being the smallest extent that contains the cursor.
fn build_index(program: &Program, comments: &[Comment], file_extent: Extent) -> Vec<Decl> {
    let mut decls: Vec<Decl> = Vec::new();
    for s in &program.stmts {
        match s {
            Stmt::Fn(f) => {
                let be = block_extent(&f.body);
                if let Some(name) = &f.name {
                    decls.push(Decl {
                        name: name.clone(),
                        kind: SymKind::Function,
                        def_span: f.span.clone(),
                        extent: file_extent,
                        detail: fn_signature(f),
                        doc: doc_above(&f.span, comments),
                    });
                }
                for p in &f.params {
                    decls.push(Decl {
                        name: p.name.clone(),
                        kind: SymKind::Parameter,
                        def_span: p.span.clone(),
                        extent: be,
                        detail: type_str(&p.ty),
                        doc: None,
                    });
                }
                collect_locals(&f.body, be, &mut decls);
            }
            Stmt::Class(c) => {
                decls.push(Decl {
                    name: c.name.clone(),
                    kind: SymKind::Class,
                    def_span: c.span.clone(),
                    extent: file_extent,
                    detail: String::new(),
                    doc: doc_above(&c.span, comments),
                });
                for m in &c.methods {
                    let be = block_extent(&m.body);
                    if let Some(name) = &m.name {
                        decls.push(Decl {
                            name: name.clone(),
                            kind: SymKind::Method,
                            def_span: m.span.clone(),
                            extent: file_extent,
                            detail: fn_signature(m),
                            doc: doc_above(&m.span, comments),
                        });
                    }
                    for p in &m.params {
                        decls.push(Decl {
                            name: p.name.clone(),
                            kind: SymKind::Parameter,
                            def_span: p.span.clone(),
                            extent: be,
                            detail: type_str(&p.ty),
                            doc: None,
                        });
                    }
                    collect_locals(&m.body, be, &mut decls);
                }
            }
            Stmt::Interface(i) => {
                decls.push(Decl {
                    name: i.name.clone(),
                    kind: SymKind::Interface,
                    def_span: i.span.clone(),
                    extent: file_extent,
                    detail: String::new(),
                    doc: doc_above(&i.span, comments),
                });
            }
            Stmt::Enum(e) => {
                decls.push(Decl {
                    name: e.name.clone(),
                    kind: SymKind::Enum,
                    def_span: e.span.clone(),
                    extent: file_extent,
                    detail: String::new(),
                    doc: doc_above(&e.span, comments),
                });
                for v in &e.variants {
                    decls.push(Decl {
                        name: v.name.clone(),
                        kind: SymKind::Variant,
                        def_span: v.span.clone(),
                        extent: file_extent,
                        detail: v.fields.join(", "),
                        doc: None,
                    });
                }
            }
            Stmt::Let { name, ty, span, .. } => decls.push(Decl {
                name: name.clone(),
                kind: SymKind::Let,
                def_span: span.clone(),
                extent: file_extent,
                detail: type_str(ty),
                doc: None,
            }),
            Stmt::Const { name, ty, span, .. } => decls.push(Decl {
                name: name.clone(),
                kind: SymKind::Const,
                def_span: span.clone(),
                extent: file_extent,
                detail: type_str(ty),
                doc: None,
            }),
            Stmt::LetTuple { names, span, .. } => {
                for n in names {
                    decls.push(Decl {
                        name: n.clone(),
                        kind: SymKind::Let,
                        def_span: span.clone(),
                        extent: file_extent,
                        detail: String::new(),
                        doc: None,
                    });
                }
            }
            _ => {}
        }
    }
    decls
}

// ---------------------------------------------------------------- documents

/// Everything the feature handlers need about one open file, computed once per
/// edit so hover/definition/symbols/completion never re-parse.
struct Doc {
    text: String,
    program: Program,
    diags: Vec<Diagnostic>,
    decls: Vec<Decl>,
    tokens: Vec<Token>,
    ranges: Vec<(usize, usize)>,
}

fn analyze(text: &str) -> Doc {
    let mut comments: Vec<Comment> = Vec::new();
    let mut tokens: Vec<Token> = Vec::new();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let program: Program;
    let diags: Vec<Diagnostic>;
    let file = Arc::from("<lsp>");
    match Lexer::new(text, file).tokenize_with_comments() {
        Err(d) => {
            program = Program { stmts: vec![] };
            diags = vec![d];
        }
        Ok(lexed) => {
            comments = lexed.comments.clone();
            tokens = lexed.tokens.clone();
            ranges = lexed.ranges.clone();
            match parser::parse(lexed.tokens) {
                Err(d) => {
                    program = Program { stmts: vec![] };
                    diags = vec![d];
                }
                Ok(p) => {
                    let has_main = p.stmts.iter().any(|s| match s {
                        Stmt::Fn(f) => f.name.as_deref() == Some("main"),
                        _ => false,
                    });
                    // Errors and warnings, exactly like `spid3r check`: on
                    // error the checker stops at the first one, and that one
                    // is what the editor must show (dropping it would leave
                    // the file looking clean).
                    diags = match semantic::check_file_with_warnings(&p, has_main, "<lsp>") {
                        Ok(warnings) => warnings,
                        Err(first_error) => vec![first_error],
                    };
                    program = p;
                }
            }
        }
    }
    let file_extent = whole_extent(text);
    let decls = build_index(&program, &comments, file_extent);
    Doc {
        text: text.to_string(),
        program,
        diags,
        decls,
        tokens,
        ranges,
    }
}

/// The identifier token under `pos`, as `(name, span)`.
fn ident_at(doc: &Doc, pos: Pos) -> Option<(String, Span)> {
    let off = byte_offset(&doc.text, pos);
    for (tok, (s, e)) in doc.tokens.iter().zip(doc.ranges.iter()) {
        if off < *s {
            break;
        }
        // A cursor parked just past the last character of a word is the
        // common "hover the end" case, so the end is inclusive.
        if off <= *e {
            if let TokenKind::Ident(name) = &tok.kind {
                return Some((name.clone(), tok.span.clone()));
            }
        }
    }
    None
}

/// The declaration a name resolves to at `pos`: the innermost extent that
/// contains the position, which is what makes a local shadow a global.
fn resolve<'a>(doc: &'a Doc, name: &str, pos: Pos) -> Option<&'a Decl> {
    let mut best: Option<&Decl> = None;
    let mut best_size = u64::MAX;
    for d in &doc.decls {
        if d.name == name && contains(d.extent, pos) {
            let size = extent_size(d.extent);
            if size < best_size {
                best = Some(d);
                best_size = size;
            }
        }
    }
    best
}

// ------------------------------------------------------------------ handlers

fn uri_of(params: &Json) -> String {
    params
        .get("textDocument")
        .and_then(|t| t.get("uri"))
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string()
}

fn position_of(params: &Json) -> Pos {
    params
        .get("position")
        .map(|p| {
            let line = p.get("line").and_then(|l| l.as_i()).unwrap_or(0);
            let ch = p.get("character").and_then(|c| c.as_i()).unwrap_or(0);
            (line.max(0) as u32, ch.max(0) as u32)
        })
        .unwrap_or((0, 0))
}

fn hover_result(docs: &HashMap<String, Doc>, params: &Json) -> Json {
    let uri = uri_of(params);
    let doc = match docs.get(&uri) {
        Some(d) => d,
        None => return Json::Null,
    };
    let pos = position_of(params);
    let (name, span) = match ident_at(doc, pos) {
        Some(x) => x,
        None => return Json::Null,
    };
    let decl = match resolve(doc, &name, pos) {
        Some(d) => d,
        None => return Json::Null,
    };
    // `detail` already carries a full signature for callables — it starts with
    // the declaration's own name — so the kind word is all that is missing. For
    // bindings it is only the type, so the name has to be added here, or an
    // untyped binding renders as a bare keyword ("parameter " and nothing else).
    let head = if decl.detail.starts_with(name.as_str()) {
        format!("{} {}", kind_word(decl.kind), decl.detail)
    } else if decl.detail.is_empty() {
        format!("{} {}", kind_word(decl.kind), name)
    } else {
        format!("{} {}: {}", kind_word(decl.kind), name, decl.detail)
    };
    let mut value = format!("```spid3r\n{}\n```", head);
    if let Some(d) = &decl.doc {
        value.push_str("\n\n");
        value.push_str(d);
    }
    Json::Obj(vec![
        (
            "contents".into(),
            Json::Obj(vec![
                ("kind".into(), Json::Str("markdown".into())),
                ("value".into(), Json::Str(value)),
            ]),
        ),
        ("range".into(), span_range(&span)),
    ])
}

fn definition_result(docs: &HashMap<String, Doc>, params: &Json) -> Json {
    let uri = uri_of(params);
    let doc = match docs.get(&uri) {
        Some(d) => d,
        None => return Json::Null,
    };
    let pos = position_of(params);
    let (name, _) = match ident_at(doc, pos) {
        Some(x) => x,
        None => return Json::Null,
    };
    let decl = match resolve(doc, &name, pos) {
        Some(d) => d,
        None => return Json::Null,
    };
    Json::Arr(vec![Json::Obj(vec![
        ("uri".into(), Json::Str(uri)),
        ("range".into(), span_range(&decl.def_span)),
    ])])
}

/// The character position for a byte offset — the inverse of `byte_offset`.
/// Document-scale inputs (editor files), so the single pass is fine.
fn pos_at_byte(text: &str, off: usize) -> Pos {
    let mut line = 0u32;
    let mut col = 0u32;
    for (i, c) in text.char_indices() {
        if i >= off {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Every occurrence of `name` in the document that resolves to `target`,
/// flagged with whether it is the declaration's own name token. Resolution
/// goes through the same `resolve` the hover/definition handlers use, so
/// shadowing is honored: a same-named local inside a tighter extent that
/// resolves elsewhere is NOT a reference. File-scoped by design (see LSP.md).
///
/// The declaration's name token is the FIRST same-named identifier at or after
/// the declaration's start: for `fn helper(a) { … }` that is `helper` (params
/// and body follow it), for `let v = expr` the binding name precedes the
/// initializer — so the rule survives recursion and self-referencing
/// initializers alike.
fn occurrences_of(doc: &Doc, target: &Decl, name: &str) -> Vec<(Span, bool)> {
    let decl_start = byte_offset(
        &doc.text,
        (
            target.def_span.line.saturating_sub(1),
            target.def_span.col.saturating_sub(1),
        ),
    );
    let mut out = Vec::new();
    let mut decl_seen = false;
    for (tok, (s, _e)) in doc.tokens.iter().zip(doc.ranges.iter()) {
        if let TokenKind::Ident(n) = &tok.kind {
            if n != name || *s < decl_start {
                continue;
            }
            let pos = pos_at_byte(&doc.text, *s);
            if let Some(d) = resolve(doc, name, pos) {
                if d.def_span.line == target.def_span.line && d.def_span.col == target.def_span.col
                {
                    let is_decl = !decl_seen;
                    decl_seen = true;
                    out.push((tok.span.clone(), is_decl));
                }
            }
        }
    }
    out
}

fn references_result(docs: &HashMap<String, Doc>, params: &Json) -> Json {
    let uri = uri_of(params);
    let doc = match docs.get(&uri) {
        Some(d) => d,
        None => return Json::Null,
    };
    let pos = position_of(params);
    let (name, _) = match ident_at(doc, pos) {
        Some(x) => x,
        None => return Json::Null,
    };
    let decl = match resolve(doc, &name, pos) {
        Some(d) => d,
        None => return Json::Null,
    };
    let include_decl = params
        .get("context")
        .and_then(|c| c.get("includeDeclaration"))
        .and_then(|b| b.as_bool())
        .unwrap_or(true);
    let mut locs = Vec::new();
    for (span, is_decl) in occurrences_of(doc, decl, &name) {
        if is_decl && !include_decl {
            continue;
        }
        locs.push(Json::Obj(vec![
            ("uri".into(), Json::Str(uri.clone())),
            ("range".into(), span_range(&span)),
        ]));
    }
    Json::Arr(locs)
}

fn rename_result(docs: &HashMap<String, Doc>, params: &Json) -> Json {
    let uri = uri_of(params);
    let doc = match docs.get(&uri) {
        Some(d) => d,
        None => return Json::Null,
    };
    let pos = position_of(params);
    let (name, _) = match ident_at(doc, pos) {
        Some(x) => x,
        None => return Json::Null,
    };
    let decl = match resolve(doc, &name, pos) {
        Some(d) => d,
        None => return Json::Null,
    };
    let new_name = match params.get("newName").and_then(|n| n.as_str()) {
        Some(n) => n,
        None => return Json::Null,
    };
    // The replacement must lex as one identifier, or the edit would corrupt
    // the program the moment the client applies it.
    let valid = !new_name.is_empty()
        && new_name.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !new_name
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(true);
    if !valid {
        return Json::Obj(vec![(
            "failureReason".into(),
            Json::Str(format!("'{}' is not a valid identifier", new_name)),
        )]);
    }
    let edits: Vec<Json> = occurrences_of(doc, decl, &name)
        .into_iter()
        .map(|(span, _is_decl)| {
            Json::Obj(vec![
                ("range".into(), span_range(&span)),
                ("newText".into(), Json::Str(new_name.to_string())),
            ])
        })
        .collect();
    Json::Obj(vec![(
        "changes".into(),
        Json::Obj(vec![(uri, Json::Arr(edits))]),
    )])
}

fn symbol(name: &str, kind: SymKind, span: &Span, children: Vec<Json>) -> Json {
    let mut obj = vec![
        ("name".into(), Json::Str(name.to_string())),
        ("kind".into(), Json::Int(kind_num(kind))),
        ("range".into(), span_range(span)),
        ("selectionRange".into(), span_range(span)),
    ];
    if !children.is_empty() {
        obj.push(("children".into(), Json::Arr(children)));
    }
    Json::Obj(obj)
}

fn document_symbol_result(docs: &HashMap<String, Doc>, params: &Json) -> Json {
    let uri = uri_of(params);
    let doc = match docs.get(&uri) {
        Some(d) => d,
        None => return Json::Arr(vec![]),
    };
    let mut out: Vec<Json> = Vec::new();
    for s in &doc.program.stmts {
        match s {
            Stmt::Fn(f) => {
                if let Some(name) = &f.name {
                    out.push(symbol(name, SymKind::Function, &f.span, vec![]));
                }
            }
            Stmt::Class(c) => {
                let mut children = Vec::new();
                for m in &c.methods {
                    if let Some(n) = &m.name {
                        children.push(symbol(n, SymKind::Method, &m.span, vec![]));
                    }
                }
                for f in &c.fields {
                    children.push(symbol(&f.name, SymKind::Field, &f.span, vec![]));
                }
                out.push(symbol(&c.name, SymKind::Class, &c.span, children));
            }
            Stmt::Interface(i) => {
                let children = i
                    .methods
                    .iter()
                    .map(|(n, _, _, sp)| symbol(n, SymKind::Method, sp, vec![]))
                    .collect();
                out.push(symbol(&i.name, SymKind::Interface, &i.span, children));
            }
            Stmt::Enum(e) => {
                let children = e
                    .variants
                    .iter()
                    .map(|v| symbol(&v.name, SymKind::Variant, &v.span, vec![]))
                    .collect();
                out.push(symbol(&e.name, SymKind::Enum, &e.span, children));
            }
            Stmt::Let { name, span, .. } => out.push(symbol(name, SymKind::Let, span, vec![])),
            Stmt::Const { name, span, .. } => out.push(symbol(name, SymKind::Const, span, vec![])),
            Stmt::LetTuple { names, span, .. } => {
                for n in names {
                    out.push(symbol(n, SymKind::Let, span, vec![]));
                }
            }
            _ => {}
        }
    }
    Json::Arr(out)
}

const KEYWORDS: &[&str] = &[
    "fn",
    "let",
    "const",
    "if",
    "elif",
    "else",
    "while",
    "for",
    "in",
    "break",
    "continue",
    "return",
    "true",
    "false",
    "null",
    "and",
    "or",
    "not",
    "import",
    "as",
    "try",
    "catch",
    "finally",
    "throw",
    "class",
    "interface",
    "new",
    "self",
    "public",
    "private",
    "spawn",
    "async",
    "await",
    "extends",
    "implements",
    "match",
    "enum",
];

fn completion_result(docs: &HashMap<String, Doc>, params: &Json) -> Json {
    let uri = uri_of(params);
    let doc = docs.get(&uri);
    let mut seen: HashSet<String> = HashSet::new();
    let mut items: Vec<Json> = Vec::new();
    if let Some(doc) = doc {
        for d in &doc.decls {
            if seen.insert(d.name.clone()) {
                items.push(Json::Obj(vec![
                    ("label".into(), Json::Str(d.name.clone())),
                    ("kind".into(), Json::Int(kind_num(d.kind))),
                    ("detail".into(), Json::Str(d.detail.clone())),
                ]));
            }
        }
    }
    for kw in KEYWORDS {
        if seen.insert((*kw).to_string()) {
            items.push(Json::Obj(vec![
                ("label".into(), Json::Str((*kw).to_string())),
                ("kind".into(), Json::Int(17)), // Keyword
                ("detail".into(), Json::Str("keyword".into())),
            ]));
        }
    }
    let _ = params;
    Json::Arr(items)
}

fn formatting_result(docs: &HashMap<String, Doc>, params: &Json) -> Json {
    let uri = uri_of(params);
    let doc = match docs.get(&uri) {
        Some(d) => d,
        None => return Json::Arr(vec![]),
    };
    let _ = params;
    let file = Arc::from(uri.as_str());
    match crate::fmt::format_source(&doc.text, &file) {
        Ok(formatted) if formatted != doc.text => Json::Arr(vec![Json::Obj(vec![
            ("range".into(), full_range(&doc.text)),
            ("newText".into(), Json::Str(formatted)),
        ])]),
        _ => Json::Arr(vec![]),
    }
}

// -------------------------------------------------------------- wire protocol

/// Read one framed message. Returns `None` on EOF or a framing error, which
/// ends the session.
fn read_message(r: &mut impl BufRead) -> Option<String> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = match r.read_line(&mut line) {
            Ok(n) => n,
            Err(_) => return None,
        };
        if n == 0 {
            return None; // EOF
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break; // end of the header block
        }
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            if let Ok(n) = rest.trim().parse::<usize>() {
                content_length = Some(n);
            }
        }
    }
    let len = content_length?;
    let mut buf = vec![0u8; len];
    if r.read_exact(&mut buf).is_err() {
        return None;
    }
    String::from_utf8(buf).ok()
}

fn write_message(out: &mut impl Write, body: &Json) {
    let mut s = String::new();
    body.write(&mut s);
    let _ = write!(out, "Content-Length: {}\r\n\r\n", s.len());
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
}

fn send_result(out: &mut impl Write, id: &Json, result: Json) {
    let body = Json::Obj(vec![
        ("jsonrpc".into(), Json::Str("2.0".into())),
        ("id".into(), id.clone()),
        ("result".into(), result),
    ]);
    write_message(out, &body);
}

fn send_error(out: &mut impl Write, id: &Json, code: i64, message: &str) {
    let body = Json::Obj(vec![
        ("jsonrpc".into(), Json::Str("2.0".into())),
        ("id".into(), id.clone()),
        (
            "error".into(),
            Json::Obj(vec![
                ("code".into(), Json::Int(code)),
                ("message".into(), Json::Str(message.to_string())),
            ]),
        ),
    ]);
    write_message(out, &body);
}

fn send_notification(out: &mut impl Write, method: &str, params: Json) {
    let body = Json::Obj(vec![
        ("jsonrpc".into(), Json::Str("2.0".into())),
        ("method".into(), Json::Str(method.to_string())),
        ("params".into(), params),
    ]);
    write_message(out, &body);
}

fn publish_diagnostics(out: &mut impl Write, uri: &str, doc: &Doc) {
    let diags: Vec<Json> = doc
        .diags
        .iter()
        .map(|d| {
            let severity = match d.severity {
                Severity::Error => 1,
                Severity::Warning => 2,
            };
            let mut message = d.message.clone();
            if let Some(h) = d.help() {
                message.push_str("\n\n");
                message.push_str(h);
            }
            let range = match &d.span {
                Some(s) => span_range(s),
                None => span_range(&Span::new(Arc::from(uri), 1, 1, 0)),
            };
            Json::Obj(vec![
                ("range".into(), range),
                ("severity".into(), Json::Int(severity)),
                ("source".into(), Json::Str("spid3r".into())),
                ("message".into(), Json::Str(message)),
            ])
        })
        .collect();
    send_notification(
        out,
        "textDocument/publishDiagnostics",
        Json::Obj(vec![
            ("uri".into(), Json::Str(uri.to_string())),
            ("diagnostics".into(), Json::Arr(diags)),
        ]),
    );
}

fn capabilities() -> Json {
    Json::Obj(vec![
        (
            "capabilities".into(),
            Json::Obj(vec![
                ("textDocumentSync".into(), Json::Int(1)), // 1 = Full
                ("hoverProvider".into(), Json::Bool(true)),
                ("definitionProvider".into(), Json::Bool(true)),
                ("referencesProvider".into(), Json::Bool(true)),
                ("renameProvider".into(), Json::Bool(true)),
                ("documentSymbolProvider".into(), Json::Bool(true)),
                (
                    "completionProvider".into(),
                    Json::Obj(vec![
                        (
                            "triggerCharacters".into(),
                            Json::Arr(vec![Json::Str(".".into())]),
                        ),
                        ("resolveProvider".into(), Json::Bool(false)),
                    ]),
                ),
                ("documentFormattingProvider".into(), Json::Bool(true)),
                (
                    "publishDiagnosticsProvider".into(),
                    Json::Obj(vec![("relatedInformation".into(), Json::Bool(false))]),
                ),
            ]),
        ),
        (
            "serverInfo".into(),
            Json::Obj(vec![
                ("name".into(), Json::Str("spid3r".into())),
                (
                    "version".into(),
                    Json::Str(crate::LANGUAGE_VERSION.to_string()),
                ),
            ]),
        ),
    ])
}

// ---------------------------------------------------------------------- loop

/// Run the server until `exit`. Returns the process exit code: 0 when the
/// client said `shutdown` first, 1 otherwise (LSP §exit).
pub fn cmd_lsp() -> i32 {
    let stdin = std::io::stdin();
    let mut reader = std::io::BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut docs: HashMap<String, Doc> = HashMap::new();
    let mut shutdown_requested = false;

    while let Some(raw) = read_message(&mut reader) {
        let value = match crate::stdlib::json_parse_text(&raw) {
            Ok(v) => from_value(&v),
            Err(_) => continue, // not JSON: not ours to answer
        };
        let method = match value.get("method").and_then(|m| m.as_str()) {
            Some(m) => m.to_string(),
            None => continue, // a response, not a request
        };
        let id = value.get("id").cloned().unwrap_or(Json::Null);
        let params = value.get("params").cloned().unwrap_or(Json::Null);
        let is_request = value.get("id").is_some();

        match method.as_str() {
            "initialize" => send_result(&mut out, &id, capabilities()),
            "initialized" => {}
            "shutdown" => {
                shutdown_requested = true;
                send_result(&mut out, &id, Json::Null);
            }
            "exit" => return if shutdown_requested { 0 } else { 1 },
            "$/cancelRequest" | "$/setTrace" => {}
            "textDocument/didOpen" => {
                let uri = uri_of(&params);
                let text = params
                    .get("textDocument")
                    .and_then(|t| t.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let doc = analyze(&text);
                publish_diagnostics(&mut out, &uri, &doc);
                docs.insert(uri, doc);
            }
            "textDocument/didChange" => {
                let uri = uri_of(&params);
                let text = params
                    .get("contentChanges")
                    .and_then(|c| c.arr())
                    .and_then(|a| a.first())
                    .and_then(|c| c.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let doc = analyze(&text);
                publish_diagnostics(&mut out, &uri, &doc);
                docs.insert(uri, doc);
            }
            "textDocument/didClose" => {
                let uri = uri_of(&params);
                docs.remove(&uri);
                send_notification(
                    &mut out,
                    "textDocument/publishDiagnostics",
                    Json::Obj(vec![
                        ("uri".into(), Json::Str(uri)),
                        ("diagnostics".into(), Json::Arr(vec![])),
                    ]),
                );
            }
            "textDocument/hover" => {
                if is_request {
                    send_result(&mut out, &id, hover_result(&docs, &params));
                }
            }
            "textDocument/definition" => {
                if is_request {
                    send_result(&mut out, &id, definition_result(&docs, &params));
                }
            }
            "textDocument/references" => {
                if is_request {
                    send_result(&mut out, &id, references_result(&docs, &params));
                }
            }
            "textDocument/rename" => {
                if is_request {
                    send_result(&mut out, &id, rename_result(&docs, &params));
                }
            }
            "textDocument/documentSymbol" => {
                if is_request {
                    send_result(&mut out, &id, document_symbol_result(&docs, &params));
                }
            }
            "textDocument/completion" => {
                if is_request {
                    send_result(&mut out, &id, completion_result(&docs, &params));
                }
            }
            "textDocument/formatting" => {
                if is_request {
                    send_result(&mut out, &id, formatting_result(&docs, &params));
                }
            }
            _ => {
                if is_request {
                    send_error(&mut out, &id, -32601, "method not supported");
                }
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_FUNCS: &str = "fn helper(a) {\n  return a;\n}\nfn main() {\n  helper(1);\n}\n";

    #[test]
    fn a_parameter_is_declared_in_its_function_scope() {
        let doc = analyze(TWO_FUNCS);
        let (name, _) = ident_at(&doc, (1, 9)).expect("the `a` under the cursor");
        assert_eq!(name, "a");
        let d = resolve(&doc, &name, (1, 9)).expect("resolves to the parameter");
        assert_eq!(d.kind, SymKind::Parameter);
        assert_eq!(d.name, "a");
    }

    #[test]
    fn a_top_level_name_resolves_to_its_declaration() {
        let doc = analyze(TWO_FUNCS);
        let (name, _) = ident_at(&doc, (4, 2)).expect("the call under the cursor");
        assert_eq!(name, "helper");
        let d = resolve(&doc, &name, (4, 2)).expect("resolves to the function");
        assert_eq!(d.kind, SymKind::Function);
        assert_eq!(d.detail, "helper(a)");
    }

    #[test]
    fn a_local_shadows_an_outer_name() {
        // `v` is declared at top level and again inside `shadow`, with
        // different annotations, so the resolved one is identifiable.
        let doc =
            analyze("let v: string = \"a\";\nfn shadow() {\n  let v: int = 2;\n  return v;\n}\n");
        let inside = resolve(&doc, "v", (3, 9)).expect("the inner v");
        assert_eq!(inside.detail, "int");
        let outside = resolve(&doc, "v", (0, 4)).expect("the outer v");
        assert_eq!(outside.detail, "string");
    }

    #[test]
    fn a_doc_comment_is_attached_to_the_declaration_below_it() {
        let doc = analyze("/// Doubles its argument.\nfn double(x) {\n  return x * 2;\n}\n");
        let d = doc
            .decls
            .iter()
            .find(|d| d.name == "double")
            .expect("a decl for double");
        assert_eq!(d.doc.as_deref(), Some("Doubles its argument."));
    }

    #[test]
    fn a_type_error_becomes_a_diagnostic() {
        let doc = analyze("fn main() {\n  let x: int = \"hello\";\n}\n");
        assert_eq!(
            doc.diags.len(),
            1,
            "{:?}",
            doc.diags
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(doc.diags[0].severity, Severity::Error);
    }

    #[test]
    fn a_syntax_error_becomes_a_diagnostic() {
        let doc = analyze("fn main() {\n  let x = ;\n}\n");
        assert_eq!(doc.diags.len(), 1);
        assert!(!doc.diags[0].message.is_empty());
    }
}
