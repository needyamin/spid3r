//! Phase 2 lexer tests. Covers every keyword, identifiers vs keywords, numbers,
//! strings and escapes, comments, bad tokens, spans, and keyword-in-string.

use spid3r_lang::diagnostics::{Phase, Span};
use spid3r_lang::lexer::token::{Keyword, TokenKind};
use spid3r_lang::lexer::Lexer;
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

fn lex(src: &str) -> Vec<spid3r_lang::lexer::token::Token> {
    let file: Arc<str> = Arc::from("t.matrix");
    Lexer::new(src, file).tokenize().unwrap()
}

fn lex_err(src: &str) -> spid3r_lang::diagnostics::Diagnostic {
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
fn scientific_notation_floats_are_single_tokens() {
    // `1.5e3` must be ONE Float(1500.0), not Int(1) + Dot + Float(.5) + Ident("e3").
    let ks = kinds("1.5e3");
    assert_eq!(ks.len(), 2); // value + Eof
    assert_eq!(ks[0], TokenKind::Float(1500.0));

    assert_eq!(kinds("1e-5")[0], TokenKind::Float(0.00001));
    assert_eq!(kinds("2E+4")[0], TokenKind::Float(20000.0));
    assert_eq!(kinds("0.5e1")[0], TokenKind::Float(5.0));
    assert_eq!(kinds("5e0")[0], TokenKind::Float(5.0));
    assert_eq!(kinds("1.0E2")[0], TokenKind::Float(100.0));
    assert_eq!(kinds("1E2")[0], TokenKind::Float(100.0));
    // A float with an exponent is still a float, never an integer token.
    assert!(matches!(kinds("9e0")[0], TokenKind::Float(_)));
    // The exponent marker needs a digit after it: `1e` stays Int(1) + Ident("e").
    assert_eq!(kinds("1e")[0], TokenKind::Int(1));
    assert_eq!(kinds("1e")[1], TokenKind::Ident("e".into()));
    // ... and `1emit` stays Int(1) + Ident("emit") (a decimal point is not
    // required, and the marker must not swallow the following identifier).
    let ks = kinds("1emit");
    assert_eq!(ks[0], TokenKind::Int(1));
    assert_eq!(ks[1], TokenKind::Ident("emit".into()));
    // `5e` followed by a digit is still taken as an exponent.
    assert_eq!(kinds("5e1")[0], TokenKind::Float(50.0));
    // Plain decimals and integers are unchanged (a value that is not an
    // approximation of a well-known constant, to keep Clippy quiet).
    assert_eq!(kinds("42")[0], TokenKind::Int(42));
    assert_eq!(kinds("1.25")[0], TokenKind::Float(1.25));
    assert_eq!(kinds("0.5")[0], TokenKind::Float(0.5));
    // Parsing through the value is what matters: `1.5e3` must equal 1500,
    // not the concatenation of two numbers.
    assert_eq!(kinds("1.5e3")[0], TokenKind::Float(1500.0));
    assert_eq!(kinds("2E+4")[0], TokenKind::Float(20000.0));
    // A bare exponent sign with no digits is not an exponent.
    let ks = kinds("1e+x");
    assert_eq!(ks[0], TokenKind::Int(1));
    assert_eq!(ks[1], TokenKind::Ident("e".into()));
    assert_eq!(ks[2], TokenKind::Plus);
    // Underflow to zero is representable and stays a float.
    assert_eq!(kinds("1e-400")[0], TokenKind::Float(0.0));
}

#[test]
fn scientific_notation_with_a_sign_after_the_marker() {
    assert_eq!(kinds("1.5e+3")[0], TokenKind::Float(1500.0));
    assert_eq!(kinds("1.5e-3")[0], TokenKind::Float(0.0015));
    // A sign with no exponent digits is not an exponent: `1e+x` is Int(1),
    // Ident("e"), Plus, Ident("x").
    let ks = kinds("1e+x");
    assert_eq!(ks[0], TokenKind::Int(1));
    assert_eq!(ks[1], TokenKind::Ident("e".into()));
    assert_eq!(ks[2], TokenKind::Plus);
}

#[test]
fn scientific_notation_overflow_is_a_lexer_error() {
    // `1e400` overflows f64: the lexer must reject it with a diagnostic naming
    // the literal rather than emitting an infinity or panicking.
    let e = lex_err("1e400");
    assert_eq!(e.phase, Phase::Lexer);
    assert!(e.message.contains("out of f64 range"), "{}", e.message);
    assert!(e.message.contains("1e400"), "{}", e.message);
    // Underflow to zero is representable and stays a float.
    assert_eq!(kinds("1e-400")[0], TokenKind::Float(0.0));
}

#[test]
fn scientific_notation_spans_cover_the_whole_literal() {
    // The diagnostic span must cover the entire literal, not just its first
    // character, or a caret would point at the wrong place.
    let toks = lex("x = 1.5e-3;");
    assert_eq!(toks[0].kind, TokenKind::Ident("x".into()));
    let f = &toks[2];
    assert_eq!(f.kind, TokenKind::Float(0.0015));
    // "1.5e-3" is six characters; the span must cover all of them.
    assert_eq!(
        f.span.len, 6,
        "span should cover 1.5e-3, got {}",
        f.span.len
    );
}

#[test]
fn string_escapes() {
    let ks = kinds(r#""a\nb\tc\\d\"e""#);
    assert_eq!(ks[0], TokenKind::Str("a\nb\tc\\d\"e".to_string()));
}

#[test]
fn unicode_escapes() {
    // F-7: `\u{...}` (Rust-style, 1-6 hex digits) and `\uXXXX` (JSON-style,
    // surrogate pairs combine) — both used to be "unknown escape '\u'".
    let ks = kinds(r#""\u{1F600}\u{e9}\u0041""#);
    assert_eq!(ks[0], TokenKind::Str("\u{1F600}\u{00e9}A".to_string()));
    // A surrogate pair combines into one astral character.
    let ks = kinds(r#""\uD83D\uDE00""#);
    assert_eq!(ks[0], TokenKind::Str("\u{1F600}".to_string()));
    // The maximum scalar value, and a one-digit braced form.
    let ks = kinds(r#""\u{10FFFF}\u{7e}""#);
    assert_eq!(ks[0], TokenKind::Str("\u{10FFFF}~".to_string()));
    // Mixed with ordinary text and the pre-existing escapes.
    let ks = kinds(r#""A\u{42}C\ntail""#);
    assert_eq!(ks[0], TokenKind::Str("ABC\ntail".to_string()));
}

#[test]
fn unicode_escape_errors_are_clean() {
    for (src, want) in [
        (r#""\u{}""#, "needs at least one hex digit"),
        (r#""\u{110000}""#, "not a Unicode scalar value"),
        (r#""\u{D800}""#, "not a Unicode scalar value"),
        (r#""\u{1234567}""#, "at most 6 hex digits"),
        (r#""\u12""#, "needs 4 hex digits"),
        (r#""\uD83D""#, "lone high surrogate"),
        (r#""\uDC00""#, "lone low surrogate"),
        (r#""\uD83D\u0041""#, "bad surrogate pair"),
    ] {
        let e = lex_err(src);
        assert_eq!(e.phase, Phase::Lexer, "{}: {:?}", src, e);
        assert!(e.message.contains(want), "{} -> {}", src, e.message);
    }
}

#[test]
fn unterminated_string_and_bad_escape_and_bad_char() {
    assert_eq!(lex_err("\"abc").phase, Phase::Lexer);
    let e = lex_err(r#""\q""#);
    assert_eq!(e.phase, Phase::Lexer);
    assert!(e.message.contains("unknown escape"), "{}", e.message);
    // `@` is the decorator marker now; `~` is still an unknown character.
    let e = lex_err("~");
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

// ---------------------------------------------------------- interpolation
// holes vs. string boundaries

/// Extract the `Str` payloads from a token stream, in order.
fn strs(src: &str) -> Vec<String> {
    kinds(src)
        .into_iter()
        .filter_map(|k| match k {
            TokenKind::Str(s) => Some(s),
            _ => None,
        })
        .collect()
}

/// The hole scan used to treat the literal's own closing quote as opening a
/// quoted region, then hunted for a `}` past the end of the literal - across
/// the newline, the `+` and the next literal's opening quote. Concatenation
/// was silently destroyed (the raw source text survived inside the string).
#[test]
fn unmatched_brace_does_not_swallow_the_string_boundary() {
    // One literal, brace never closes inside it.
    assert_eq!(strs("\"a{b\""), vec!["a{b".to_string()]);
    // The classic two-literal concatenation.
    assert_eq!(
        strs("\"a{b\"\n+ \"c}d\""),
        vec!["a{b".to_string(), "c}d".to_string()]
    );
    // CSS-shaped text: braces balanced across literals, not within one.
    assert_eq!(
        strs("\".calc{padding:18px;\"\n+ \"box-shadow:0 2px}\""),
        vec![
            ".calc{padding:18px;".to_string(),
            "box-shadow:0 2px}".to_string()
        ]
    );
    // A quote inside the hole text, still unbalanced within the literal.
    assert_eq!(
        strs("\"v: {a + \"\n+ \"x}\""),
        vec!["v: {a + ".to_string(), "x}".to_string()]
    );
    // Same shape on one line: the capture is `b" + "c`, never an expression.
    assert_eq!(
        strs("\"a{b\" + \"c}d\""),
        vec!["a{b".to_string(), "c}d".to_string()]
    );
}

/// Holes that already worked must keep working: quotes inside brackets,
/// balanced quotes inside the literal, plain paths and calls.
#[test]
fn working_hole_forms_still_lex() {
    let pieces = |src: &str| match kinds(src).into_iter().next().unwrap() {
        TokenKind::InterpStr(p) => p,
        other => panic!("expected InterpStr, got {:?}", other),
    };
    use spid3r_lang::lexer::token::InterpPiece;

    // Quote inside a call: the hole closes inside the literal.
    assert_eq!(
        pieces("\"{f(\"x\")}\""),
        vec![InterpPiece::Hole("f(\"x\")".to_string())]
    );
    // Quote at bracket depth 0, balanced inside the literal.
    assert_eq!(
        pieces("\"{a + \"x\"}\""),
        vec![InterpPiece::Hole("a + \"x\"".to_string())]
    );
    // A brace inside the hole's own quoted string.
    assert_eq!(
        pieces("\"{f(\"}\")}\""),
        vec![InterpPiece::Hole("f(\"}\")".to_string())]
    );
    // Plain path and call holes.
    assert_eq!(
        pieces("\"n={len(xs)}\""),
        vec![
            InterpPiece::Lit("n=".to_string()),
            InterpPiece::Hole("len(xs)".to_string()),
        ]
    );
    // Template text is untouched.
    assert_eq!(strs("\"{{ user }}\""), vec!["{{ user }}".to_string()]);
    // A hole whose text is not an expression stays literal text, unchanged.
    assert_eq!(strs("\"{not a hole}\""), vec!["{not a hole}".to_string()]);
}

/// End-to-end through the parser: the concatenated result must be one string.
#[test]
fn concat_across_braces_end_to_end() {
    use spid3r_lang::parser;
    let src = "fn main() {\n    println(\"a{b\"\n    + \"c}d\");\n    return 0;\n}\n";
    let program = parser::parse_file(src, Arc::from("t.matrix")).unwrap();
    // The hole scan must not have escaped into the `+`: exactly one string
    // token per literal, so the program parses as one println of one add.
    let dumped = format!("{:?}", program);
    assert!(dumped.contains("a{b"), "first literal lost: {}", dumped);
    assert!(dumped.contains("c}d"), "second literal lost: {}", dumped);
}
