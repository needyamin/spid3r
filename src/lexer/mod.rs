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
        } else {
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
            b'=' => self.two(b'=', TokenKind::EqEq, TokenKind::Assign),
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
            b'<' => self.two(b'=', TokenKind::Le, TokenKind::Lt),
            b'>' => self.two(b'=', TokenKind::Ge, TokenKind::Gt),
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
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.bump();
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') && matches!(self.peek_at(1), Some(c) if c.is_ascii_digit()) {
            is_float = true;
            self.bump();
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.bump();
            }
        }
        let text = std::str::from_utf8(&self.src[begin..self.pos]).unwrap();
        let span = Span::new(self.file.clone(), line, col, (self.pos - begin) as u32);
        let kind = if is_float {
            let v: f64 = text.parse().map_err(|_| {
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
            match text.parse::<i64>() {
                Ok(v) => TokenKind::Int(v),
                Err(_) => {
                    return Err(err_at(
                        crate::diagnostics::Phase::Lexer,
                        span,
                        format!("integer '{}' is out of i64 range", text),
                    ))
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

    fn lex_string(&mut self, line: u32, col: u32, start_pos: usize) -> SpidResult<Token> {
        self.bump(); // opening quote
        let mut s = String::new();
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
                    return Ok(Token {
                        kind: TokenKind::Str(s),
                        span: Span::new(self.file.clone(), line, col, len),
                    });
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
