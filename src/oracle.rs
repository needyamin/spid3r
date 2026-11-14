//! Stage-0 oracle dump surface (docs/SELFHOST.md SH-5).
//!
//! Deterministic JSON dumps of the stage-0 front end's internal state — the
//! token stream and the checked-AST-shape parse tree — consumed by the
//! self-host parity harness (Phases 1–2 of `full_plan.html`) and CI. This is
//! **bootstrap tooling**: it exists so the Spid3r-written lexer and parser can
//! be diffed against the Rust oracle, and it must never change user-visible
//! behavior. Exit codes stay on the standard 0/1/2/4 contract.
//!
//! Both dumps are pure functions of the source text: no environment, no
//! hashing, no pointer identity, fields in fixed order — two runs on the same
//! input produce byte-identical output (`spid3r tokens --json` /
//! `spid3r parse --dump-ast-json`).

use crate::ast::{
    AssignOp, BinOp, Block, CatchClause, ClassDecl, Expr, FieldDecl, FnProto, LogOp, MatchArm,
    Param, Pattern, Program, Stmt, TypeExpr, UnOp, VariantDecl, Vis,
};
use crate::diagnostics::Span;
use crate::lexer::token::{InterpPiece, Token, TokenKind};

// ------------------------------------------------------------------ shared

/// JSON string escaping: quotes, backslashes and control characters. UTF-8
/// beyond ASCII passes through unescaped, so the dump is byte-stable.
fn jstr(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `"line":L,"col":C,"len":N` — the span fields, in this order, in every node.
/// `file` is omitted: a dump is single-file, and the top level names it once.
fn span_fields(s: &Span, out: &mut String) {
    out.push_str(&format!("\"line\":{},\"col\":{},\"len\":{}", s.line, s.col, s.len));
}

fn vis_str(v: Vis) -> &'static str {
    match v {
        Vis::Public => "public",
        Vis::Private => "private",
    }
}

// ------------------------------------------------------------------- tokens

/// The token stream as JSON: `{"file":…,"count":…,"tokens":[…]}`. Payloads are
/// the lexer's model values (a `Str` carries the decoded string, a `Float` its
/// f64), not the source spelling — the payload is what every backend consumes,
/// so the payload is the parity contract.
pub fn tokens_to_json(file: &str, toks: &[Token]) -> String {
    let mut out = String::with_capacity(64 + toks.len() * 48);
    out.push_str("{\"file\":");
    out.push_str(&jstr(file));
    out.push_str(",\"count\":");
    out.push_str(&toks.len().to_string());
    out.push_str(",\"tokens\":[");
    for (i, t) in toks.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        token_json(t, &mut out);
    }
    out.push_str("]}\n");
    out
}

fn token_json(t: &Token, out: &mut String) {
    out.push_str("{\"kind\":");
    match &t.kind {
        TokenKind::Int(n) => {
            out.push_str(&format!("\"Int\",\"value\":{}", n));
        }
        TokenKind::BigIntLit(digits) => {
            out.push_str(&format!("\"BigIntLit\",\"value\":{}", jstr(digits)));
        }
        TokenKind::Float(x) => {
            out.push_str(&format!("\"Float\",\"value\":{}", x));
        }
        TokenKind::Str(s) => {
            out.push_str(&format!("\"Str\",\"value\":{}", jstr(s)));
        }
        TokenKind::InterpStr(pieces) => {
            out.push_str("\"InterpStr\",\"pieces\":[");
            for (i, p) in pieces.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                match p {
                    InterpPiece::Lit(text) => out.push_str(&format!("{{\"lit\":{}}}", jstr(text))),
                    InterpPiece::Path(path) => {
                        out.push_str("{\"path\":[");
                        for (i, seg) in path.iter().enumerate() {
                            if i > 0 {
                                out.push(',');
                            }
                            out.push_str(&jstr(seg));
                        }
                        out.push_str("]}");
                    }
                    InterpPiece::Hole(text) => {
                        out.push_str(&format!("{{\"hole\":{}}}", jstr(text)))
                    }
                }
            }
            out.push(']');
        }
        TokenKind::Ident(name) => {
            out.push_str(&format!("\"Ident\",\"value\":{}", jstr(name)));
        }
        TokenKind::Kw(k) => {
            out.push_str(&format!("\"Kw\",\"value\":{}", jstr(k.as_str())));
        }
        TokenKind::Plus => out.push_str("\"Plus\""),
        TokenKind::Minus => out.push_str("\"Minus\""),
        TokenKind::Star => out.push_str("\"Star\""),
        TokenKind::Slash => out.push_str("\"Slash\""),
        TokenKind::Percent => out.push_str("\"Percent\""),
        TokenKind::EqEq => out.push_str("\"EqEq\""),
        TokenKind::BangEq => out.push_str("\"BangEq\""),
        TokenKind::Lt => out.push_str("\"Lt\""),
        TokenKind::Le => out.push_str("\"Le\""),
        TokenKind::Gt => out.push_str("\"Gt\""),
        TokenKind::Ge => out.push_str("\"Ge\""),
        TokenKind::Assign => out.push_str("\"Assign\""),
        TokenKind::At => out.push_str("\"At\""),
        TokenKind::PlusEq => out.push_str("\"PlusEq\""),
        TokenKind::MinusEq => out.push_str("\"MinusEq\""),
        TokenKind::StarEq => out.push_str("\"StarEq\""),
        TokenKind::SlashEq => out.push_str("\"SlashEq\""),
        TokenKind::PercentEq => out.push_str("\"PercentEq\""),
        TokenKind::LParen => out.push_str("\"LParen\""),
        TokenKind::RParen => out.push_str("\"RParen\""),
        TokenKind::LBrace => out.push_str("\"LBrace\""),
        TokenKind::RBrace => out.push_str("\"RBrace\""),
        TokenKind::LBracket => out.push_str("\"LBracket\""),
        TokenKind::RBracket => out.push_str("\"RBracket\""),
        TokenKind::Comma => out.push_str("\"Comma\""),
        TokenKind::Colon => out.push_str("\"Colon\""),
        TokenKind::Semi => out.push_str("\"Semi\""),
        TokenKind::Dot => out.push_str("\"Dot\""),
        TokenKind::Arrow => out.push_str("\"Arrow\""),
        TokenKind::Amp => out.push_str("\"Amp\""),
        TokenKind::AmpAmp => out.push_str("\"AmpAmp\""),
        TokenKind::Pipe => out.push_str("\"Pipe\""),
        TokenKind::PipePipe => out.push_str("\"PipePipe\""),
        TokenKind::Caret => out.push_str("\"Caret\""),
        TokenKind::Shl => out.push_str("\"Shl\""),
        TokenKind::Shr => out.push_str("\"Shr\""),
        TokenKind::Question => out.push_str("\"Question\""),
        TokenKind::QuestionDot => out.push_str("\"QuestionDot\""),
        TokenKind::QuestionQuestion => out.push_str("\"QuestionQuestion\""),
        TokenKind::FatArrow => out.push_str("\"FatArrow\""),
        TokenKind::IntMinMagnitude => out.push_str("\"IntMinMagnitude\""),
        TokenKind::Eof => out.push_str("\"Eof\""),
    }
    out.push(',');
    span_fields(&t.span, out);
    out.push('}');
}

// ----------------------------------------------------------------- the AST

/// The parse tree as JSON: `{"file":…,"stmts":[…]}`. Every node is
/// `{"node":"<Variant>",…fields in declaration order…,"span":{line,col,len}}`.
/// This shape is the Phase-2 parity contract (roadmap 2.1/2.2): the
/// Spid3r-written parser must reproduce it byte-for-byte on the frozen corpus.
pub fn program_to_json(file: &str, prog: &Program) -> String {
    let mut out = String::with_capacity(4096);
    out.push_str("{\"file\":");
    out.push_str(&jstr(file));
    out.push_str(",\"stmts\":[");
    for (i, s) in prog.stmts.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        stmt_json(s, &mut out);
    }
    out.push_str("]}\n");
    out
}

fn stmt_json(s: &Stmt, out: &mut String) {
    out.push_str("{\"node\":");
    match s {
        Stmt::Import { path, alias, span } => {
            out.push_str(&format!(
                "\"Import\",\"path\":{},\"alias\":{}",
                jstr(path),
                opt_str_json(alias.as_deref())
            ));
            span_out(span, out);
        }
        Stmt::Fn(f) => {
            out.push_str("\"Fn\",\"fn\":");
            fn_proto_json(f, out);
            span_out(&f.span, out);
        }
        Stmt::Class(c) => {
            out.push_str("\"Class\",");
            class_json(c, out);
            span_out(&c.span, out);
        }
        Stmt::Interface(i) => {
            out.push_str(&format!("\"Interface\",\"vis\":\"{}\",\"name\":{}", vis_str(i.vis), jstr(&i.name)));
            out.push_str(",\"methods\":[");
            for (n, (name, params, ret, span)) in i.methods.iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                out.push_str(&format!(
                    "{{\"name\":{},\"params\":",
                    jstr(name)
                ));
                params_json(params, out);
                out.push_str(",\"ret\":");
                opt_type_json(ret.as_ref(), out);
                out.push_str(&format!(
                    ",\"line\":{},\"col\":{},\"len\":{}}}",
                    span.line, span.col, span.len
                ));
            }
            out.push_str("],\"default_methods\":[");
            for (n, f) in i.default_methods.iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                fn_proto_json(f, out);
            }
            out.push(']');
            span_out(&i.span, out);
        }
        Stmt::Enum(e) => {
            out.push_str(&format!("\"Enum\",\"vis\":\"{}\",\"name\":{},\"variants\":[", vis_str(e.vis), jstr(&e.name)));
            for (n, v) in e.variants.iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                variant_json(v, out);
            }
            out.push(']');
            span_out(&e.span, out);
        }
        Stmt::Let { vis, name, ty, value, span } => {
            out.push_str(&format!(
                "\"Let\",\"vis\":\"{}\",\"name\":{},\"ty\":",
                vis_str(*vis),
                jstr(name)
            ));
            opt_type_json(ty.as_ref(), out);
            out.push_str(",\"value\":");
            opt_expr_json(value.as_ref(), out);
            span_out(span, out);
        }
        Stmt::Const { vis, name, ty, value, span } => {
            out.push_str(&format!(
                "\"Const\",\"vis\":\"{}\",\"name\":{},\"ty\":",
                vis_str(*vis),
                jstr(name)
            ));
            opt_type_json(ty.as_ref(), out);
            out.push_str(",\"value\":");
            expr_json(value, out);
            span_out(span, out);
        }
        Stmt::LetTuple { vis, names, value, span } => {
            out.push_str(&format!("\"LetTuple\",\"vis\":\"{}\",\"names\":[", vis_str(*vis)));
            for (n, name) in names.iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                out.push_str(&jstr(name));
            }
            out.push_str("],\"value\":");
            expr_json(value, out);
            span_out(span, out);
        }
        Stmt::If { cond, then, elifs, els, span } => {
            out.push_str("\"If\",\"cond\":");
            expr_json(cond, out);
            out.push_str(",\"then\":");
            block_json(then, out);
            out.push_str(",\"elifs\":[");
            for (n, (c, b)) in elifs.iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                out.push_str("{\"cond\":");
                expr_json(c, out);
                out.push_str(",\"body\":");
                block_json(b, out);
                out.push('}');
            }
            out.push_str("],\"els\":");
            opt_block_json(els.as_ref(), out);
            span_out(span, out);
        }
        Stmt::While { cond, body, span } => {
            out.push_str("\"While\",\"cond\":");
            expr_json(cond, out);
            out.push_str(",\"body\":");
            block_json(body, out);
            span_out(span, out);
        }
        Stmt::ForIn { var, var2, iter, body, span } => {
            out.push_str(&format!(
                "\"ForIn\",\"var\":{},\"var2\":{},\"iter\":",
                jstr(var),
                opt_str_json(var2.as_deref())
            ));
            expr_json(iter, out);
            out.push_str(",\"body\":");
            block_json(body, out);
            span_out(span, out);
        }
        Stmt::ForC { init, cond, post, body, span } => {
            out.push_str("\"ForC\",\"init\":");
            stmt_json(init, out);
            out.push_str(",\"cond\":");
            expr_json(cond, out);
            out.push_str(",\"post\":");
            expr_json(post, out);
            out.push_str(",\"body\":");
            block_json(body, out);
            span_out(span, out);
        }
        Stmt::Return { value, span } => {
            out.push_str("\"Return\",\"value\":");
            opt_expr_json(value.as_ref(), out);
            span_out(span, out);
        }
        Stmt::Break { span } => {
            out.push_str("\"Break\"");
            span_out(span, out);
        }
        Stmt::Continue { span } => {
            out.push_str("\"Continue\"");
            span_out(span, out);
        }
        Stmt::Block(b) => {
            out.push_str("\"Block\",\"block\":");
            block_json(b, out);
            span_out(&b.span, out);
        }
        Stmt::Expr(e) => {
            out.push_str("\"Expr\",\"expr\":");
            expr_json(e, out);
            span_out(e.span(), out);
        }
        Stmt::Spawn { callable, span } => {
            out.push_str("\"Spawn\",\"callable\":");
            expr_json(callable, out);
            span_out(span, out);
        }
        Stmt::Yield { value, span } => {
            out.push_str("\"Yield\",\"value\":");
            expr_json(value, out);
            span_out(span, out);
        }
        Stmt::Try { body, catch, finally, span } => {
            out.push_str("\"Try\",\"body\":");
            block_json(body, out);
            out.push_str(",\"catch\":");
            match catch {
                None => out.push_str("null"),
                Some(CatchClause { name, body, span }) => {
                    out.push_str(&format!("{{\"name\":{},\"body\":", jstr(name)));
                    block_json(body, out);
                    out.push(',');
                    span_fields(span, out);
                    out.push('}');
                }
            }
            out.push_str(",\"finally\":");
            opt_block_json(finally.as_ref(), out);
            span_out(span, out);
        }
        Stmt::Throw { value, span } => {
            out.push_str("\"Throw\",\"value\":");
            expr_json(value, out);
            span_out(span, out);
        }
    }
    out.push('}');
}

fn variant_json(v: &VariantDecl, out: &mut String) {
    out.push_str(&format!("{{\"node\":\"Variant\",\"name\":{},\"fields\":[", jstr(&v.name)));
    for (n, f) in v.fields.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        out.push_str(&jstr(f));
    }
    out.push(']');
    span_out(&v.span, out);
    out.push('}');
}

fn class_json(c: &ClassDecl, out: &mut String) {
    out.push_str(&format!(
        "\"vis\":\"{}\",\"name\":{},\"is_abstract\":{},\"abstracts\":[",
        vis_str(c.vis),
        jstr(&c.name),
        c.is_abstract
    ));
    str_list(&c.abstracts, out);
    out.push_str("],\"parent\":");
    out.push_str(&opt_str_json(c.parent.as_deref()));
    out.push_str(",\"interfaces\":[");
    str_list(&c.interfaces, out);
    out.push_str("],\"type_params\":[");
    str_list(&c.type_params, out);
    out.push_str("],\"fields\":[");
    for (n, f) in c.fields.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        field_json(f, out);
    }
    out.push_str("],\"methods\":[");
    for (n, m) in c.methods.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        fn_proto_json(m, out);
    }
    out.push(']');
}

fn field_json(f: &FieldDecl, out: &mut String) {
    out.push_str(&format!(
        "{{\"node\":\"Field\",\"vis\":\"{}\",\"name\":{},\"ty\":",
        vis_str(f.vis),
        jstr(&f.name)
    ));
    opt_type_json(f.ty.as_ref(), out);
    out.push(',');
    span_fields(&f.span, out);
    out.push('}');
}

fn fn_proto_json(f: &FnProto, out: &mut String) {
    out.push_str(&format!(
        "{{\"node\":\"FnProto\",\"vis\":\"{}\",\"is_async\":{},\"is_extern\":{},\"name\":",
        vis_str(f.vis),
        f.is_async,
        f.is_extern
    ));
    out.push_str(&opt_str_json(f.name.as_deref()));
    out.push_str(",\"params\":");
    params_json(&f.params, out);
    out.push_str(",\"ret\":");
    opt_type_json(f.ret.as_ref(), out);
    out.push_str(",\"type_params\":[");
    str_list(&f.type_params, out);
    out.push_str("],\"body\":");
    block_json(&f.body, out);
    out.push(',');
    span_fields(&f.span, out);
    out.push('}');
}

fn params_json(params: &[Param], out: &mut String) {
    out.push('[');
    for (n, p) in params.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"node\":\"Param\",\"name\":{},\"ty\":",
            jstr(&p.name)
        ));
        opt_type_json(p.ty.as_ref(), out);
        out.push_str(",\"default\":");
        opt_expr_json(p.default.as_ref(), out);
        out.push(',');
        span_fields(&p.span, out);
        out.push('}');
    }
    out.push(']');
}

fn type_json(t: &TypeExpr, out: &mut String) {
    out.push_str(&format!("{{\"node\":\"Type\",\"name\":{},\"arg\":", jstr(&t.name)));
    match t.arg.as_ref() {
        None => out.push_str("null"),
        Some(a) => type_json(a, out),
    }
    out.push_str(&format!(",\"nullable\":{},\"union\":[", t.nullable));
    for (n, u) in t.union.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        type_json(u, out);
    }
    out.push(']');
    span_out(&t.span, out);
    out.push('}');
}

fn pattern_json(p: &Pattern, out: &mut String) {
    out.push_str("{\"node\":");
    match p {
        Pattern::Wild => out.push_str("\"Wild\""),
        Pattern::Bind(name) => out.push_str(&format!("\"Bind\",\"name\":{}", jstr(name))),
        Pattern::Literal(e) => {
            out.push_str("\"Literal\",\"expr\":");
            expr_json(e, out);
            out.push(',');
            span_fields(e.span(), out);
        }
        Pattern::Variant { name, args, span } => {
            out.push_str(&format!("\"Variant\",\"name\":{},\"args\":[", jstr(name)));
            for (n, a) in args.iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                pattern_json(a, out);
            }
            out.push(']');
            span_out(span, out);
        }
    }
    out.push('}');
}

fn match_arm_json(a: &MatchArm, out: &mut String) {
    out.push_str("{\"node\":\"MatchArm\",\"pattern\":");
    pattern_json(&a.pattern, out);
    out.push_str(",\"guard\":");
    opt_expr_json(a.guard.as_ref(), out);
    out.push_str(",\"body\":");
    block_json(&a.body, out);
    out.push(',');
    span_fields(&a.span, out);
    out.push('}');
}

fn block_json(b: &Block, out: &mut String) {
    out.push_str("{\"node\":\"Block\",\"stmts\":[");
    for (n, s) in b.stmts.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        stmt_json(s, out);
    }
    out.push(']');
    span_out(&b.span, out);
    out.push('}');
}

fn expr_json(e: &Expr, out: &mut String) {
    out.push_str("{\"node\":");
    match e {
        Expr::Int(v, s) => {
            out.push_str(&format!("\"Int\",\"value\":{}", v));
            span_out(s, out);
        }
        Expr::Float(v, s) => {
            out.push_str(&format!("\"Float\",\"value\":{}", v));
            span_out(s, out);
        }
        Expr::Str(v, s) => {
            out.push_str(&format!("\"Str\",\"value\":{}", jstr(v)));
            span_out(s, out);
        }
        Expr::Bool(v, s) => {
            out.push_str(&format!("\"Bool\",\"value\":{}", v));
            span_out(s, out);
        }
        Expr::Null(s) => {
            out.push_str("\"Null\"");
            span_out(s, out);
        }
        Expr::Ident(name, s) => {
            out.push_str(&format!("\"Ident\",\"name\":{}", jstr(name)));
            span_out(s, out);
        }
        Expr::List(items, s) => {
            out.push_str("\"List\",\"items\":[");
            expr_list(items, out);
            out.push(']');
            span_out(s, out);
        }
        Expr::Fn(f) => {
            out.push_str("\"Fn\",\"fn\":");
            fn_proto_json(f, out);
            span_out(&f.span, out);
        }
        Expr::Unary { op, expr, span } => {
            out.push_str(&format!("\"Unary\",\"op\":\"{}\",\"expr\":", unop_str(*op)));
            expr_json(expr, out);
            span_out(span, out);
        }
        Expr::Binary { op, l, r, span } => {
            out.push_str(&format!("\"Binary\",\"op\":\"{}\",\"l\":", binop_str(*op)));
            expr_json(l, out);
            out.push_str(",\"r\":");
            expr_json(r, out);
            span_out(span, out);
        }
        Expr::Logical { op, l, r, span } => {
            out.push_str(&format!(
                "\"Logical\",\"op\":\"{}\",\"l\":",
                match op {
                    LogOp::And => "And",
                    LogOp::Or => "Or",
                }
            ));
            expr_json(l, out);
            out.push_str(",\"r\":");
            expr_json(r, out);
            span_out(span, out);
        }
        Expr::Assign { target, op, value, span } => {
            out.push_str(&format!(
                "\"Assign\",\"op\":\"{}\",\"target\":",
                assignop_str(*op)
            ));
            expr_json(target, out);
            out.push_str(",\"value\":");
            expr_json(value, out);
            span_out(span, out);
        }
        Expr::Call { callee, args, span } => {
            out.push_str("\"Call\",\"callee\":");
            expr_json(callee, out);
            out.push_str(",\"args\":[");
            expr_list(args, out);
            out.push(']');
            span_out(span, out);
        }
        Expr::Method { object, name, args, span } => {
            out.push_str(&format!("\"Method\",\"name\":{},\"object\":", jstr(name)));
            expr_json(object, out);
            out.push_str(",\"args\":[");
            expr_list(args, out);
            out.push(']');
            span_out(span, out);
        }
        Expr::Index { object, index, span } => {
            out.push_str("\"Index\",\"object\":");
            expr_json(object, out);
            out.push_str(",\"index\":");
            expr_json(index, out);
            span_out(span, out);
        }
        Expr::Field { object, name, span } => {
            out.push_str(&format!("\"Field\",\"name\":{},\"object\":", jstr(name)));
            expr_json(object, out);
            span_out(span, out);
        }
        Expr::New { class, args, span } => {
            out.push_str(&format!("\"New\",\"class\":{},\"args\":[", jstr(class)));
            expr_list(args, out);
            out.push(']');
            span_out(span, out);
        }
        Expr::Await(inner, s) => {
            out.push_str("\"Await\",\"expr\":");
            expr_json(inner, out);
            span_out(s, out);
        }
        Expr::NamedArg { name, value, span } => {
            out.push_str(&format!("\"NamedArg\",\"name\":{},\"value\":", jstr(name)));
            expr_json(value, out);
            span_out(span, out);
        }
        Expr::Spawn { callable, span } => {
            out.push_str("\"Spawn\",\"callable\":");
            expr_json(callable, out);
            span_out(span, out);
        }
        Expr::Super { span } => {
            out.push_str("\"Super\"");
            span_out(span, out);
        }
        Expr::SuperCall { parent, method, args, span } => {
            out.push_str(&format!(
                "\"SuperCall\",\"parent\":{},\"method\":{},\"args\":[",
                jstr(parent),
                opt_str_json(method.as_deref())
            ));
            expr_list(args, out);
            out.push(']');
            span_out(span, out);
        }
        Expr::If { cond, then, els, span } => {
            out.push_str("\"If\",\"cond\":");
            expr_json(cond, out);
            out.push_str(",\"then\":");
            block_json(then, out);
            out.push_str(",\"els\":");
            opt_block_json(els.as_ref(), out);
            span_out(span, out);
        }
        Expr::OptField { object, name, span } => {
            out.push_str(&format!("\"OptField\",\"name\":{},\"object\":", jstr(name)));
            expr_json(object, out);
            span_out(span, out);
        }
        Expr::Coalesce { l, r, span } => {
            out.push_str("\"Coalesce\",\"l\":");
            expr_json(l, out);
            out.push_str(",\"r\":");
            expr_json(r, out);
            span_out(span, out);
        }
        Expr::Propagate(inner, s) => {
            out.push_str("\"Propagate\",\"expr\":");
            expr_json(inner, out);
            span_out(s, out);
        }
        Expr::Match { scrutinee, arms, span } => {
            out.push_str("\"Match\",\"scrutinee\":");
            expr_json(scrutinee, out);
            out.push_str(",\"arms\":[");
            for (n, a) in arms.iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                match_arm_json(a, out);
            }
            out.push(']');
            span_out(span, out);
        }
    }
    out.push('}');
}

fn unop_str(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "Neg",
        UnOp::Not => "Not",
    }
}

fn binop_str(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "Add",
        BinOp::Sub => "Sub",
        BinOp::Mul => "Mul",
        BinOp::Div => "Div",
        BinOp::Mod => "Mod",
        BinOp::Eq => "Eq",
        BinOp::Ne => "Ne",
        BinOp::Lt => "Lt",
        BinOp::Le => "Le",
        BinOp::Gt => "Gt",
        BinOp::Ge => "Ge",
    }
}

fn assignop_str(op: AssignOp) -> &'static str {
    match op {
        AssignOp::Assign => "Assign",
        AssignOp::AddAssign => "AddAssign",
        AssignOp::SubAssign => "SubAssign",
        AssignOp::MulAssign => "MulAssign",
        AssignOp::DivAssign => "DivAssign",
        AssignOp::ModAssign => "ModAssign",
    }
}

// ------------------------------------------------------------- small helpers

fn expr_list(items: &[Expr], out: &mut String) {
    for (n, e) in items.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        expr_json(e, out);
    }
}

fn str_list(items: &[String], out: &mut String) {
    for (n, s) in items.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        out.push_str(&jstr(s));
    }
}

fn opt_str_json(v: Option<&str>) -> String {
    match v {
        Some(s) => jstr(s),
        None => "null".to_string(),
    }
}

fn opt_expr_json(e: Option<&Expr>, out: &mut String) {
    match e {
        None => out.push_str("null"),
        Some(e) => expr_json(e, out),
    }
}

fn opt_type_json(t: Option<&TypeExpr>, out: &mut String) {
    match t {
        None => out.push_str("null"),
        Some(t) => type_json(t, out),
    }
}

fn opt_block_json(b: Option<&Block>, out: &mut String) {
    match b {
        None => out.push_str("null"),
        Some(b) => block_json(b, out),
    }
}

/// Append `,"line":L,"col":C,"len":N` and leave the caller to close the node.
fn span_out(s: &Span, out: &mut String) {
    out.push(',');
    span_fields(s, out);
}
