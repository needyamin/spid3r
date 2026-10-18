//! Phase 2 lexer tests. Covers every keyword, identifiers vs keywords, numbers,
//! strings and escapes, comments, bad tokens, spans, and keyword-in-string.

use spid3r::diagnostics::{Phase, Span};
use spid3r::lexer::token::{Keyword, TokenKind};
use spid3r::lexer::Lexer;
use std::sync::Arc;

fn kinds(src: &str) -> Vec<TokenKind> {
    let file: Arc<str> = Arc::from("t.matrix");
    Lexer::new(src, file)
        .tokenize()
        .unwrap()
        .into_iter()
        .map(|t| t.kind)
        .collect()
}

fn lex(src: &str) -> Vec<spid3r::lexer::token::Token> {
    let file: Arc<str> = Arc::from("t.matrix");
    Lexer::new(src, file).tokenize().unwrap()
}

fn lex_err(src: &str) -> spid3r::diagnostics::Diagnostic {
    let file: Arc<str> = Arc::from("t.matrix");
    Lexer::new(src, file).tokenize().unwrap_err()
}

#[test]
fn every_keyword_lexes_as_keyword() {
    let words = [
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
    ];
    for w in words {
        let ks = kinds(w);
        assert_eq!(ks.len(), 2, "{} produced {:?}", w, ks);
        match &ks[0] {
            TokenKind::Kw(kw) => assert_eq!(kw.as_str(), w),
            other => panic!("{} lexed as {:?}, not a keyword", w, other),
        }
    }
}

#[test]
fn keyword_inside_string_is_text() {
    let ks = kinds("\"class fn let\"");
    assert_eq!(ks[0], TokenKind::Str("class fn let".to_string()));
    assert_eq!(ks[1], TokenKind::Eof);
}

#[test]
fn identifiers_and_numbers() {
    let ks = kinds("foo _bar baz1 42 1.5 0");
    assert_eq!(
        ks,
        vec![
            TokenKind::Ident("foo".into()),
            TokenKind::Ident("_bar".into()),
            TokenKind::Ident("baz1".into()),
            TokenKind::Int(42),
            TokenKind::Float(1.5),
            TokenKind::Int(0),
            TokenKind::Eof,
        ]
    );
}

#[test]
fn number_then_dot_is_int_then_dot() {
    let ks = kinds("42.foo");
    assert_eq!(ks[0], TokenKind::Int(42));
    assert_eq!(ks[1], TokenKind::Dot);
    assert_eq!(ks[2], TokenKind::Ident("foo".into()));
}

#[test]
fn string_escapes() {
    let ks = kinds(r#""a\nb\tc\\d\"e""#);
    assert_eq!(ks[0], TokenKind::Str("a\nb\tc\\d\"e".to_string()));
}

#[test]
fn unterminated_string_and_bad_escape_and_bad_char() {
    assert_eq!(lex_err("\"abc").phase, Phase::Lexer);
    let e = lex_err(r#""\q""#);
    assert_eq!(e.phase, Phase::Lexer);
    assert!(e.message.contains("unknown escape"), "{}", e.message);
    let e = lex_err("@");
    assert_eq!(e.phase, Phase::Lexer);
}

#[test]
fn comments_are_skipped() {
    let ks = kinds("let x = 1; // trailing\n/* block */ let y = 2;");
    assert_eq!(ks[0], TokenKind::Kw(Keyword::Let));
    assert!(ks
        .iter()
        .all(|k| !matches!(k, TokenKind::Ident(s) if s.contains("block"))));
    assert_eq!(
        ks.iter()
            .filter(|k| **k == TokenKind::Kw(Keyword::Let))
            .count(),
        2
    );
}

#[test]
fn unterminated_block_comment() {
    assert_eq!(lex_err("/* nope").phase, Phase::Lexer);
}

#[test]
fn operators_and_delimiters() {
    let ks = kinds("+ - * / % == != < <= > >= = += -= *= /= %= ( ) { } [ ] , : ; . ->");
    let ops: Vec<TokenKind> = ks.into_iter().filter(|k| *k != TokenKind::Eof).collect();
    assert_eq!(
        ops,
        vec![
            TokenKind::Plus,
            TokenKind::Minus,
            TokenKind::Star,
            TokenKind::Slash,
            TokenKind::Percent,
            TokenKind::EqEq,
            TokenKind::BangEq,
            TokenKind::Lt,
            TokenKind::Le,
            TokenKind::Gt,
            TokenKind::Ge,
            TokenKind::Assign,
            TokenKind::PlusEq,
            TokenKind::MinusEq,
            TokenKind::StarEq,
            TokenKind::SlashEq,
            TokenKind::PercentEq,
            TokenKind::LParen,
            TokenKind::RParen,
            TokenKind::LBrace,
            TokenKind::RBrace,
            TokenKind::LBracket,
            TokenKind::RBracket,
            TokenKind::Comma,
            TokenKind::Colon,
            TokenKind::Semi,
            TokenKind::Dot,
            TokenKind::Arrow,
        ]
    );
}

#[test]
fn spans_carry_line_and_column() {
    let toks = lex("let x = 1;\nlet yy = 22;");
    assert_eq!(toks[0].span, Span::new(Arc::from("t.matrix"), 1, 1, 3));
    assert_eq!(toks[1].span, Span::new(Arc::from("t.matrix"), 1, 5, 1));
    // second line starts fresh
    let semi = toks.iter().find(|t| t.kind == TokenKind::Semi).unwrap();
    assert_eq!(semi.span.line, 1);
    let second = toks.iter().filter(|t| t.span.line == 2).count();
    assert!(second >= 5, "second line tokens: {}", second);
}

#[test]
fn lone_bang_is_error() {
    assert!(lex_err("!").message.contains('!'));
}

#[test]
fn big_int_rejected() {
    let e = lex_err("99999999999999999999");
    assert!(e.message.contains("i64"));
}
