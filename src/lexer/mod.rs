//! The Spid3r lexer. Every token carries file, line, and column.

use crate::diagnostics::{err_at, Span, SpidResult};
use crate::lexer::token::{Keyword, Token, TokenKind};

pub mod token;

pub struct Lexer<'a> {
    src: &'a [u8],
    file: std::sync::Arc<str>,
    pos: usize,
    line: u32,
    col: u32,
    /// Comments in source order (filled by `tokenize_with_comments`).
    comments: Vec<Comment>,
    /// Byte range `(start, end)` of each token, parallel to the token vec.
    ranges: Vec<(usize, usize)>,
    /// Byte offset where the most recent token started (set by `next_token`).
    last_tok_start: usize,
}

/// A comment skipped by the lexer, kept for the formatter. `text` includes
/// the `//` or `/* ... */` markers.
#[derive(Clone, Debug)]
pub struct Comment {
    pub line: u32,
    pub col: u32,
    pub text: String,
}

/// Everything `spid3r fmt` needs beyond the token stream: comments in source
/// order and each token's byte range in the source.
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub comments: Vec<Comment>,
    /// Byte range `(start, end)` per token, parallel to `tokens`.
    pub ranges: Vec<(usize, usize)>,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str, file: std::sync::Arc<str>) -> Self {
        let bytes = src.as_bytes();
        let start = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            3
        } else {
            0
        };
        Lexer {
            src: bytes,
            file,
            pos: start,
            line: 1,
            col: 1,
            comments: Vec::new(),
            ranges: Vec::new(),
            last_tok_start: start,
        }
    }

    pub fn tokenize(self) -> SpidResult<Vec<Token>> {
        Ok(self.tokenize_with_comments()?.tokens)
    }

    /// Tokens plus everything the formatter needs that the parser ignores:
    /// comments in source order and each token's byte range in the source.
    pub fn tokenize_with_comments(mut self) -> SpidResult<Lexed> {
        let mut out = Vec::new();
        loop {
            let tok = self.next_token()?;
            let is_eof = tok.kind == TokenKind::Eof;
            self.ranges.push((self.last_tok_start, self.pos));
            out.push(tok);
            if is_eof {
                return Ok(Lexed {
                    tokens: out,
                    comments: std::mem::take(&mut self.comments),
                    ranges: std::mem::take(&mut self.ranges),
                });
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<u8> {
        self.src.get(self.pos + n).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.pos += 1;
        if c == b'\n' {
            self.line += 1;
            self.col = 1;
        } else if (c & 0xC0) != 0x80 {
            // Columns count CHARACTERS, not bytes: a UTF-8 continuation byte
            // (10xxxxxx) is part of the previous character, so it must not
            // advance the column. Counting bytes put every caret on a
            // non-ASCII line too far right.
            self.col += 1;
        }
        Some(c)
    }

    fn skip_trivia(&mut self) -> SpidResult<()> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => {
                    self.bump();
                }
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    let (line, col) = (self.line, self.col);
                    let begin = self.pos;
                    while let Some(c) = self.peek() {
                        if c == b'\n' {
                            break;
                        }
                        self.bump();
                    }
                    let raw = String::from_utf8_lossy(&self.src[begin..self.pos]);
                    let text = raw.trim_end_matches('\r').to_string();
                    self.comments.push(Comment { line, col, text });
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    let (line, col) = (self.line, self.col);
                    let start = Span::new(self.file.clone(), self.line, self.col, 2);
                    let begin = self.pos;
                    self.bump();
                    self.bump();
                    let mut closed = false;
                    while let Some(c) = self.bump() {
                        if c == b'*' && self.peek() == Some(b'/') {
                            self.bump();
                            closed = true;
                            break;
                        }
                    }
                    if !closed {
                        return Err(err_at(
                            crate::diagnostics::Phase::Lexer,
                            start,
                            "unterminated block comment",
                        ));
                    }
                    let text = String::from_utf8_lossy(&self.src[begin..self.pos]).into_owned();
                    self.comments.push(Comment { line, col, text });
                }
                _ => return Ok(()),
            }
        }
    }

    /// Lex one token. All tokens start and end on one line except strings with
    /// escapes (span points at the opening quote, which is what diagnostics use).
    fn next_token(&mut self) -> SpidResult<Token> {
        self.skip_trivia()?;
        let line = self.line;
        let col = self.col;
        let start_pos = self.pos;
        self.last_tok_start = start_pos;
        let file = self.file.clone();
        let mk_span = |len: u32| Span::new(file.clone(), line, col, len);
        let lex_err = |msg: String| {
            err_at(
                crate::diagnostics::Phase::Lexer,
                Span::new(file.clone(), line, col, 1),
                msg,
            )
        };

        let c = match self.peek() {
            Some(c) => c,
            None => {
                return Ok(Token {
                    kind: TokenKind::Eof,
                    span: Span::new(self.file.clone(), line, col, 0),
                })
            }
        };

        if c.is_ascii_digit() {
            return self.lex_number(line, col);
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let s = self.lex_ident();
            let kind = match Keyword::from_ident(&s) {
                Some(kw) => TokenKind::Kw(kw),
                None => TokenKind::Ident(s),
            };
            let len = (self.pos - start_pos) as u32;
            return Ok(Token {
                kind,
                span: mk_span(len),
            });
        }
        if c == b'"' {
            return self.lex_string(line, col, start_pos);
        }

        self.bump();
        let kind = match c {
            b'+' => self.two(b'=', TokenKind::PlusEq, TokenKind::Plus),
            b'-' => {
                if self.peek() == Some(b'>') {
                    self.bump();
                    TokenKind::Arrow
                } else {
                    self.two(b'=', TokenKind::MinusEq, TokenKind::Minus)
                }
            }
            b'*' => self.two(b'=', TokenKind::StarEq, TokenKind::Star),
            b'/' => self.two(b'=', TokenKind::SlashEq, TokenKind::Slash),
            b'%' => self.two(b'=', TokenKind::PercentEq, TokenKind::Percent),
            b'=' => {
                // `=>` before `=`/`==`: a match arm separator.
                if self.peek() == Some(b'>') {
                    self.bump();
                    TokenKind::FatArrow
                } else {
                    self.two(b'=', TokenKind::EqEq, TokenKind::Assign)
                }
            }
            b'!' => {
                if self.peek() == Some(b'=') {
                    self.bump();
                    TokenKind::BangEq
                } else {
                    return Err(lex_err(
                        "unexpected '!'; did you mean '!=' or 'not'?".into(),
                    ));
                }
            }
            // `<<` / `>>` before the single-character forms, so `1 << 2` is a
            // shift rather than two comparisons.
            b'<' => {
                if self.peek() == Some(b'<') {
                    self.bump();
                    TokenKind::Shl
                } else {
                    self.two(b'=', TokenKind::Le, TokenKind::Lt)
                }
            }
            b'>' => {
                if self.peek() == Some(b'>') {
                    self.bump();
                    TokenKind::Shr
                } else {
                    self.two(b'=', TokenKind::Ge, TokenKind::Gt)
                }
            }
            b'?' => {
                if self.peek() == Some(b'.') {
                    self.bump();
                    TokenKind::QuestionDot
                } else if self.peek() == Some(b'?') {
                    self.bump();
                    TokenKind::QuestionQuestion
                } else {
                    TokenKind::Question
                }
            }
            b'&' => TokenKind::Amp,
            b'|' => TokenKind::Pipe,
            b'^' => TokenKind::Caret,
            b'(' => TokenKind::LParen,
            b')' => TokenKind::RParen,
            b'{' => TokenKind::LBrace,
            b'}' => TokenKind::RBrace,
            b'[' => TokenKind::LBracket,
            b']' => TokenKind::RBracket,
            b',' => TokenKind::Comma,
            b':' => TokenKind::Colon,
            b';' => TokenKind::Semi,
            b'.' => TokenKind::Dot,
            _ => {
                if let Ok(rest) = std::str::from_utf8(&self.src[start_pos..]) {
                    if let Some(ch) = rest.chars().next() {
                        for _ in 1..ch.len_utf8() {
                            self.bump();
                        }
                        return Err(lex_err(format!("unexpected character '{}'", ch)));
                    }
                }
                return Err(lex_err(format!("unexpected byte 0x{:02X}", c)));
            }
        };
        let len = (self.pos - start_pos) as u32;
        Ok(Token {
            kind,
            span: mk_span(len),
        })
    }

    fn two(&mut self, second: u8, if_match: TokenKind, else_: TokenKind) -> TokenKind {
        if self.peek() == Some(second) {
            self.bump();
            if_match
        } else {
            else_
        }
    }

    fn lex_number(&mut self, line: u32, col: u32) -> SpidResult<Token> {
        let begin = self.pos;
        // Radix prefixes: `0x`/`0o`/`0b` (either case) followed by at least
        // one digit of that radix. Underscores may separate digits. A prefix
        // without a digit is not a radix literal: `0x` stays the decimal `0`
        // followed by the identifier `x`, matching the documented `1elif`
        // behaviour.
        if self.peek() == Some(b'0') {
            let radix = match self.peek_at(1) {
                Some(b'x') | Some(b'X') => Some(16u32),
                Some(b'o') | Some(b'O') => Some(8u32),
                Some(b'b') | Some(b'B') => Some(2u32),
                _ => None,
            };
            if let Some(radix) = radix {
                let starts = matches!(self.peek_at(2), Some(c) if (c as char).is_digit(radix));
                if starts {
                    self.bump(); // the `0`
                    self.bump(); // the prefix letter
                    let digits_begin = self.pos;
                    while matches!(self.peek(), Some(c) if (c as char).is_digit(radix) || c == b'_')
                    {
                        self.bump();
                    }
                    let raw = std::str::from_utf8(&self.src[digits_begin..self.pos]).unwrap();
                    let full = std::str::from_utf8(&self.src[begin..self.pos]).unwrap();
                    let len = self.src[begin..self.pos]
                        .iter()
                        .filter(|b| (*b & 0xC0) != 0x80)
                        .count() as u32;
                    let span = Span::new(self.file.clone(), line, col, len);
                    let kind = parse_radix_int(raw, radix, full, &span)?;
                    return Ok(Token { kind, span });
                }
            }
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == b'_') {
            self.bump();
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') && matches!(self.peek_at(1), Some(c) if c.is_ascii_digit()) {
            is_float = true;
            self.bump();
            while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == b'_') {
                self.bump();
            }
        }
        // Exponent: `e`/`E`, an optional `+`/`-`, then at least one digit.
        // The lookahead matters twice over: `1elif` must stay the int `1`
        // followed by the ident `elif`, and a bare `1e` (nothing numeric
        // after the marker) is not a valid float literal either.
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            let mut after_marker = self.pos + 1;
            if matches!(self.src.get(after_marker), Some(b'+') | Some(b'-')) {
                after_marker += 1;
            }
            if matches!(self.src.get(after_marker), Some(c) if c.is_ascii_digit()) {
                is_float = true;
                self.bump(); // the e/E
                if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                    self.bump();
                }
                while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == b'_') {
                    self.bump();
                }
            }
        }
        let text = std::str::from_utf8(&self.src[begin..self.pos]).unwrap();
        if text.contains('_') {
            if let Some(why) = separator_error(text) {
                return Err(err_at(
                    crate::diagnostics::Phase::Lexer,
                    Span::new(self.file.clone(), line, col, text.chars().count() as u32),
                    format!("invalid numeric literal '{}': {}", text, why),
                ));
            }
        }
        // `len` counts characters for the same reason `col` does: a span must
        // stay meaningful on a line with non-ASCII text.
        let len = self.src[begin..self.pos]
            .iter()
            .filter(|b| (*b & 0xC0) != 0x80)
            .count() as u32;
        let span = Span::new(self.file.clone(), line, col, len);
        let stripped: String = if text.contains('_') {
            text.chars().filter(|c| *c != '_').collect()
        } else {
            text.to_string()
        };
        let kind = if is_float {
            let v: f64 = stripped.parse().map_err(|_| {
                err_at(
                    crate::diagnostics::Phase::Lexer,
                    span.clone(),
                    format!("bad float '{}'", text),
                )
            })?;
            if !v.is_finite() {
                return Err(err_at(
                    crate::diagnostics::Phase::Lexer,
                    span,
                    format!("float '{}' is out of f64 range", text),
                ));
            }
            TokenKind::Float(v)
        } else {
            // A trailing `n` marks a big-integer literal BEFORE the i64 range
            // check: the whole point is that the digits do not fit an i64.
            // `n` must not be the start of an identifier (`1next`).
            let is_suffix = self.peek() == Some(b'n')
                && !matches!(self.peek_at(1), Some(c)
                    if (c as char).is_ascii_alphanumeric() || c == b'_');
            if is_suffix {
                self.bump();
                let digits: String = stripped.replace('_', "");
                TokenKind::BigIntLit(digits)
            } else {
                match stripped.parse::<i64>() {
                    Ok(v) => TokenKind::Int(v),
                    Err(_) => {
                        return Err(err_at(
                            crate::diagnostics::Phase::Lexer,
                            span,
                            format!(
                                "integer '{}' is out of i64 range (add an `n` suffix for a \
                                 big integer)",
                                text
                            ),
                        ))
                    }
                }
            }
        };
        Ok(Token { kind, span })
    }

    fn lex_ident(&mut self) -> String {
        let begin = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_') {
            self.bump();
        }
        String::from_utf8(self.src[begin..self.pos].to_vec()).unwrap()
    }

    /// Try to read `ident(.ident)*}` starting at the character after `{`.
    /// Rewinds and returns `None` if the text is not exactly that shape, so the
    /// caller can emit a literal `{`.
    /// Capture `{...}` as an expression hole: the text between the braces,
    /// tracked through nested braces and quoted strings. Returns `None` when
    /// the content is empty, spans a newline, or never closes - the caller
    /// then treats the `{` as literal text.
    fn try_interp_expr(&mut self) -> Option<String> {
        // The caller has already consumed the `{`; capture starts here.
        let (pos, line, col) = (self.pos, self.line, self.col);
        let start = self.pos;
        let mut depth = 0usize;
        let mut quote: Option<u8> = None;
        while let Some(c) = self.peek() {
            match quote {
                Some(q) => {
                    if c == b'\\' {
                        self.bump();
                    } else if c == q {
                        quote = None;
                    }
                }
                None => match c {
                    b'"' | b'\'' => quote = Some(c),
                    b'{' => depth += 1,
                    b'}' => {
                        if depth == 0 {
                            let raw =
                                String::from_utf8_lossy(&self.src[start..self.pos]).into_owned();
                            self.bump(); // the `}`
                            if raw.trim().is_empty() {
                                self.pos = pos;
                                self.line = line;
                                self.col = col;
                                return None;
                            }
                            return Some(raw);
                        }
                        depth -= 1;
                    }
                    b'\n' => break,
                    _ => {}
                },
            }
            self.bump();
        }
        self.pos = pos;
        self.line = line;
        self.col = col;
        None
    }

    fn try_interp_path(&mut self) -> Option<Vec<String>> {
        let (pos, line, col) = (self.pos, self.line, self.col);
        let mut segments = Vec::new();
        loop {
            let start = self.pos;
            match self.peek() {
                Some(c) if c == b'_' || c.is_ascii_alphabetic() => {}
                _ => break,
            }
            while matches!(self.peek(), Some(c) if c == b'_' || c.is_ascii_alphanumeric()) {
                self.bump();
            }
            segments.push(String::from_utf8_lossy(&self.src[start..self.pos]).into_owned());
            if self.peek() == Some(b'.') {
                self.bump();
                continue;
            }
            break;
        }
        let ok = !segments.is_empty() && self.peek() == Some(b'}');
        if !ok || self.pos == pos {
            // Not a hole: restore and let the caller treat `{` as text.
            self.pos = pos;
            self.line = line;
            self.col = col;
            return None;
        }
        self.bump(); // the closing `}`
        Some(segments)
    }

    fn lex_string(&mut self, line: u32, col: u32, start_pos: usize) -> SpidResult<Token> {
        self.bump(); // opening quote
        let mut s = String::new();
        // Interpolation holes seen so far; empty means a plain string literal.
        let mut pieces: Vec<crate::lexer::token::InterpPiece> = Vec::new();
        loop {
            let c = match self.peek() {
                Some(c) => c,
                None => {
                    return Err(err_at(
                        crate::diagnostics::Phase::Lexer,
                        Span::new(self.file.clone(), line, col, 1),
                        "unterminated string literal",
                    ))
                }
            };
            let (bs_line, bs_col) = (self.line, self.col);
            self.bump();
            match c {
                b'"' => {
                    let len = (self.pos - start_pos) as u32;
                    let kind = if pieces.is_empty() {
                        TokenKind::Str(s)
                    } else {
                        if !s.is_empty() {
                            pieces.push(crate::lexer::token::InterpPiece::Lit(s));
                        }
                        TokenKind::InterpStr(pieces)
                    };
                    return Ok(Token {
                        kind,
                        span: Span::new(self.file.clone(), line, col, len),
                    });
                }
                // `{name}` is an interpolation hole. Anything else starting with
                // `{` is literal text, which is what keeps the template syntax
                // `"{{ user }}"` (used by stdlib/web.matrix) untouched: `{` is
                // followed by `{` or a space there, and a hole requires an
                // identifier immediately after the brace.
                b'{' => {
                    if let Some(path) = self.try_interp_path() {
                        if !s.is_empty() {
                            pieces.push(crate::lexer::token::InterpPiece::Lit(std::mem::take(
                                &mut s,
                            )));
                        }
                        pieces.push(crate::lexer::token::InterpPiece::Path(path));
                    } else if matches!(self.peek(), Some(c)
                        if c.is_ascii_alphabetic()
                            || c == b'_'
                            || c.is_ascii_digit()
                            || c == b'('
                            || c == b'['
                            || c == b'-')
                    {
                        // `{expr}`: a richer hole. `{{` and `{ ` stay literal
                        // so template text (`"{{ user }}"`) is untouched.
                        match self.try_interp_expr() {
                            Some(raw) => {
                                if !s.is_empty() {
                                    pieces.push(crate::lexer::token::InterpPiece::Lit(
                                        std::mem::take(&mut s),
                                    ));
                                }
                                pieces.push(crate::lexer::token::InterpPiece::Hole(raw));
                            }
                            None => s.push('{'),
                        }
                    } else {
                        s.push('{');
                    }
                }
                b'\\' => {
                    let e = match self.peek() {
                        Some(e) => e,
                        None => {
                            return Err(err_at(
                                crate::diagnostics::Phase::Lexer,
                                Span::new(self.file.clone(), line, col, 1),
                                "unterminated string literal",
                            ))
                        }
                    };
                    self.bump();
                    match e {
                        b'n' => s.push('\n'),
                        b'r' => s.push('\r'),
                        b't' => s.push('\t'),
                        b'\\' => s.push('\\'),
                        b'"' => s.push('"'),
                        _ => {
                            return Err(err_at(
                                crate::diagnostics::Phase::Lexer,
                                Span::new(self.file.clone(), bs_line, bs_col, 2),
                                format!("unknown escape '\\{}'", e as char),
                            ))
                        }
                    }
                }
                _ => {
                    let mut bytes = vec![c];
                    while matches!(self.peek(), Some(b) if b >= 0x80) {
                        bytes.push(self.bump().unwrap());
                    }
                    match std::str::from_utf8(&bytes) {
                        Ok(t) => s.push_str(t),
                        Err(_) => {
                            return Err(err_at(
                                crate::diagnostics::Phase::Lexer,
                                Span::new(self.file.clone(), line, col, 1),
                                "invalid UTF-8 in string literal",
                            ))
                        }
                    }
                }
            }
        }
    }
}

/// Why a numeric literal's `_` separators are malformed, if they are: every
/// `_` must sit between two digits.
fn separator_error(text: &str) -> Option<&'static str> {
    let bytes = text.as_bytes();
    let is_d = |b: u8| b.is_ascii_digit();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'_' {
            continue;
        }
        let before = i > 0 && is_d(bytes[i - 1]);
        let after = i + 1 < bytes.len() && is_d(bytes[i + 1]);
        if !before || !after {
            return Some("underscores must separate digits");
        }
    }
    None
}

/// Parse the digits of a radix-prefixed integer literal (`0x1_0`, `0b1010`,
/// `0o777`). Underscores may separate digits but may not lead, trail, or
/// repeat. Returns the token kind, or a clean lexer error.
fn parse_radix_int(digits: &str, radix: u32, full: &str, span: &Span) -> SpidResult<TokenKind> {
    let invalid = |why: &str| {
        Err(err_at(
            crate::diagnostics::Phase::Lexer,
            span.clone(),
            format!("invalid integer literal '{}': {}", full, why),
        ))
    };
    if digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return invalid("underscores must separate digits");
    }
    let text: String = digits.chars().filter(|c| *c != '_').collect();
    match i64::from_str_radix(&text, radix) {
        Ok(v) => Ok(TokenKind::Int(v)),
        Err(_) => invalid("value is out of i64 range"),
    }
}
