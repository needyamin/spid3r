//! Phase 3 parser tests: precedence, nesting, every statement form, classes,
//! interfaces, spawn, and malformed input.

use spid3r_lang::ast::*;
use spid3r_lang::parser::parse_file;
use std::sync::Arc;

fn parse(src: &str) -> Program {
    parse_file(src, Arc::from("t.matrix")).expect("parse failed")
}

fn parse_err(src: &str) -> String {
    parse_file(src, Arc::from("t.matrix"))
        .unwrap_err()
        .to_string()
}

#[test]
fn precedence_matches_the_frozen_grammar() {
    // 1 + 2 * 3 must parse as 1 + (2 * 3), and the tree shape proves it.
    let p = parse("let x = 1 + 2 * 3;");
    match &p.stmts[0] {
        Stmt::Let {
            value: Some(Expr::Binary {
                op: BinOp::Add, r, ..
            }),
            ..
        } => {
            assert!(matches!(**r, Expr::Binary { op: BinOp::Mul, .. }));
        }
        other => panic!("bad shape: {:?}", other),
    }
    // not binds tighter than and, and tighter than or
    let p = parse("let x = not a and b or c;");
    match &p.stmts[0] {
        Stmt::Let {
            value: Some(Expr::Logical {
                op: LogOp::Or, l, ..
            }),
            ..
        } => {
            assert!(matches!(**l, Expr::Logical { op: LogOp::And, .. }));
        }
        other => panic!("bad shape: {:?}", other),
    }
    // comparison is tighter than and
    let p = parse("let x = a < b and c;");
    match &p.stmts[0] {
        Stmt::Let {
            value: Some(Expr::Logical {
                op: LogOp::And, l, ..
            }),
            ..
        } => {
            assert!(matches!(**l, Expr::Binary { op: BinOp::Lt, .. }));
        }
        other => panic!("bad shape: {:?}", other),
    }
}

#[test]
fn assignment_is_right_associative_and_lowest() {
    let p = parse("a = b = 1;");
    match &p.stmts[0] {
        Stmt::Expr(Expr::Assign { value, .. }) => {
            assert!(matches!(**value, Expr::Assign { .. }));
        }
        other => panic!("bad shape: {:?}", other),
    }
    // `a == b` is not an assignment
    let p = parse("a == b;");
    assert!(matches!(
        &p.stmts[0],
        Stmt::Expr(Expr::Binary { op: BinOp::Eq, .. })
    ));
}

#[test]
fn unary_and_calls_and_index_and_methods() {
    let p = parse("let x = -f(1)[2].get(3);");
    match &p.stmts[0] {
        Stmt::Let { value: Some(e), .. } => match e {
            Expr::Unary {
                op: UnOp::Neg,
                expr,
                ..
            } => match &**expr {
                Expr::Method { name, args, .. } => {
                    assert_eq!(name, "get");
                    assert_eq!(args.len(), 1);
                }
                other => panic!("expected method: {:?}", other),
            },
            other => panic!("bad shape: {:?}", other),
        },
        other => panic!("bad shape: {:?}", other),
    }
}

#[test]
fn every_statement_form_parses() {
    let src = r#"
        import "math_util";
        let a = 1;
        let b: int;
        const c = 2;
        if a > 1 { let z = 1; } elif a == 1 { let z = 2; } else { let z = 3; }
        while a < 10 { a = a + 1; }
        for x in [1, 2, 3] { print(x); }
        for let i = 0; i < 3; i = i + 1 { print(i); }
        fn f(a, b) { return a + b; }
        class K extends Object implements Named { private let x; fn new(x) { self.x = x; } }
        interface Named { fn name(self) -> string; }
        spawn fn() { print("t"); };
        try { throw "x"; } catch (e) { print(e); } finally { print("f"); }
        break_outside: while true { break; continue; }
        return_ok: fn g() { return 1; }
        { let scoped = 1; }
    "#;
    // fix: break/continue must be inside a loop; rewrite the two broken lines
    let src = src.replace(
        "break_outside: while true { break; continue; }",
        "while true { break; continue; }",
    );
    let src = src.replace("return_ok: fn g() { return 1; }", "fn g() { return 1; }");
    let p = parse(&src);
    let forms: Vec<&str> = p
        .stmts
        .iter()
        .map(|s| match s {
            Stmt::Import { .. } => "import",
            Stmt::Fn(_) => "fn",
            Stmt::Class(_) => "class",
            Stmt::Interface(_) => "interface",
            Stmt::Let { .. } => "let",
            Stmt::Const { .. } => "const",
            Stmt::If { .. } => "if",
            Stmt::While { .. } => "while",
            Stmt::ForIn { .. } => "for-in",
            Stmt::ForC { .. } => "for-c",
            Stmt::Return { .. } => "return",
            Stmt::Break { .. } => "break",
            Stmt::Continue { .. } => "continue",
            Stmt::Block(_) => "block",
            Stmt::Expr(_) => "expr",
            Stmt::Spawn { .. } => "spawn",
            Stmt::Try { .. } => "try",
            Stmt::Throw { .. } => "throw",
        })
        .collect();
    for f in [
        "import",
        "let",
        "const",
        "if",
        "while",
        "for-in",
        "for-c",
        "fn",
        "class",
        "interface",
        "spawn",
        "try",
        "block",
    ] {
        assert!(forms.contains(&f), "missing {} in {:?}", f, forms);
    }
    // nested forms: throw lives in the try body, return/break/continue in theirs
    let p2 = parse(&src);
    let mut found_throw = false;
    let mut found_ret = false;
    let mut found_break = false;
    let mut found_continue = false;
    fn walk(stmts: &[Stmt], t: &mut bool, r: &mut bool, b: &mut bool, c: &mut bool) {
        for s in stmts {
            match s {
                Stmt::Throw { .. } => *t = true,
                Stmt::Return { .. } => *r = true,
                Stmt::Break { .. } => *b = true,
                Stmt::Continue { .. } => *c = true,
                Stmt::Try {
                    body,
                    catch,
                    finally,
                    ..
                } => {
                    walk(&body.stmts, t, r, b, c);
                    if let Some(cc) = catch {
                        walk(&cc.body.stmts, t, r, b, c);
                    }
                    if let Some(fb) = finally {
                        walk(&fb.stmts, t, r, b, c);
                    }
                }
                Stmt::While { body, .. } => walk(&body.stmts, t, r, b, c),
                Stmt::Fn(f) => walk(&f.body.stmts, t, r, b, c),
                _ => {}
            }
        }
    }
    walk(
        &p2.stmts,
        &mut found_throw,
        &mut found_ret,
        &mut found_break,
        &mut found_continue,
    );
    assert!(found_throw && found_ret && found_break && found_continue);
}

#[test]
fn async_fn_and_await_parse() {
    let p = parse("async fn f() { await g(); }");
    match &p.stmts[0] {
        Stmt::Fn(f) => {
            assert!(f.is_async);
            assert_eq!(f.name.as_deref(), Some("f"));
        }
        other => panic!("{:?}", other),
    }
    let p = parse("fn f() { let t = await g(); }");
    match &p.stmts[0] {
        Stmt::Fn(f) => match &f.body.stmts[0] {
            Stmt::Let {
                value: Some(Expr::Await(_, _)),
                ..
            } => {}
            other => panic!("{:?}", other),
        },
        other => panic!("{:?}", other),
    }
}

#[test]
fn class_with_interface_and_visibility() {
    let p = parse(
        r#"
        class User extends Object implements Named, Aged {
            public let id;
            private let secret;
            fn new(id, secret) { self.id = id; self.secret = secret; }
            public fn name(self) -> string { return "u"; }
            private fn hidden(self) -> int { return 1; }
        }
        "#,
    );
    match &p.stmts[0] {
        Stmt::Class(c) => {
            assert_eq!(c.name, "User");
            assert_eq!(c.parent.as_deref(), Some("Object"));
            assert_eq!(c.interfaces, vec!["Named".to_string(), "Aged".to_string()]);
            assert_eq!(c.fields.len(), 2);
            assert_eq!(c.fields[0].vis, Vis::Public);
            assert_eq!(c.fields[1].vis, Vis::Private);
            assert_eq!(c.methods.len(), 3);
            assert!(c.methods.iter().any(|m| m.name.as_deref() == Some("new")));
        }
        other => panic!("{:?}", other),
    }
}

#[test]
fn new_expression_and_fn_literal() {
    let p = parse("let u = new User(1, 2);");
    match &p.stmts[0] {
        Stmt::Let {
            value: Some(Expr::New { class, args, .. }),
            ..
        } => {
            assert_eq!(class, "User");
            assert_eq!(args.len(), 2);
        }
        other => panic!("{:?}", other),
    }
    let p = parse("let f = fn(a) { return a; };");
    match &p.stmts[0] {
        Stmt::Let {
            value: Some(Expr::Fn(f)),
            ..
        } => {
            assert!(f.name.is_none());
            assert_eq!(f.params.len(), 1);
        }
        other => panic!("{:?}", other),
    }
}

#[test]
fn malformed_input_errors() {
    assert!(parse_err("let x = ;").contains("parser"));
    assert!(parse_err("let x 1;").contains("parser"));
    assert!(parse_err("if x { let y = 1;").contains("parser"));
    assert!(parse_err("fn () {}").contains("parser"));
    assert!(parse_err("import math_util;").contains("parser"));
    assert!(parse_err("try { let x = 1; }").contains("catch"));
}

#[test]
fn string_with_keywords_and_escapes() {
    let p = parse(r#"let s = "class fn let \" \n \\";"#);
    match &p.stmts[0] {
        Stmt::Let {
            value: Some(Expr::Str(s, _)),
            ..
        } => {
            assert_eq!(s, "class fn let \" \n \\");
        }
        other => panic!("{:?}", other),
    }
}

#[test]
fn comments_are_not_syntax() {
    let p = parse("let x = 1; // comment\n/* block\n comment */ let y = 2;");
    assert_eq!(p.stmts.len(), 2);
}

#[test]
fn index_assignment_target() {
    let p = parse("a[0] = 5; b[1] += 2;");
    match &p.stmts[0] {
        Stmt::Expr(Expr::Assign {
            target,
            op: AssignOp::Assign,
            ..
        }) => {
            assert!(matches!(**target, Expr::Index { .. }));
        }
        other => panic!("{:?}", other),
    }
    assert!(matches!(
        &p.stmts[1],
        Stmt::Expr(Expr::Assign {
            op: AssignOp::AddAssign,
            ..
        })
    ));
}

#[test]
fn empty_program_and_main() {
    let p = parse("");
    assert!(p.stmts.is_empty());
    let p = parse("fn main() { return 0; }");
    match &p.stmts[0] {
        Stmt::Fn(f) => assert_eq!(f.name.as_deref(), Some("main")),
        other => panic!("{:?}", other),
    }
}

#[test]
fn labeled_variable_is_not_a_thing() {
    // `while` used as if it were a keyword in a bad spot
    assert!(parse_err("let while = 1;").contains("parser"));
}
