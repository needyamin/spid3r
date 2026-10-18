//! `spid3r fmt` — canonical source formatting.
//!
//! Token-stream reprinter: the source is lexed (comments and token byte
//! ranges preserved) and re-emitted with canonical spacing and indentation.
//! Literals and identifiers are copied from the original source slices, so
//! `7.0` stays `7.0` and string escapes are untouched. Two guarantees are
//! enforced for every formatted file:
//!
//! 1. **Token preservation** — the formatted output must lex to the exact
//!    same token sequence as the input (spans ignored). Any mismatch is a
//!    bug and aborts instead of writing a semantically different file.
//! 2. **Idempotence** — formatting already-formatted text is a no-op.
//!
//! Style: 4-space indent, one statement per line, `} else {` / `} catch {` /
//! `} finally {` joins, blank lines collapsed to one, line comments kept at
//! (or moved to) the current indent, trailing same-line comments attached
//! with two spaces.
//!
//! Line wrapping (`MAX_WIDTH`): a parenthesised group whose flat rendering
//! would overflow the width is broken one item per line — but only when the
//! group itself is substantial, so a short call at the end of a long line is
//! left alone. A group whose arguments contain a block (a closure body such as
//! `xs.map(fn(x) { ... })`) is *transparent*: the braces own the indentation,
//! the call stays compact, and it closes as `}));`. A group that already spans
//! several source lines stays multiline, which is what makes wrapping
//! idempotent — the layout decision is read back from the source.
//!
//! Trailing commas are deliberately NOT added to multiline calls: that would
//! insert a token and break the token-preservation guarantee above.

use std::collections::HashMap;
use std::sync::Arc;

use crate::diagnostics::{Diagnostic, Phase};
use crate::lexer::token::{Keyword, Token, TokenKind};
use crate::lexer::Lexer;

const INDENT: &str = "    ";

/// Line width beyond which a parenthesised group is broken one item per line.
const MAX_WIDTH: usize = 100;

/// One entry of the merged source-ordered stream: a token (with its index
/// into the token vec for raw-slice lookup) or a comment.
enum Item<'x> {
    Tok(&'x Token, usize),
    Com(&'x crate::lexer::Comment),
}

fn tok_kind<'x>(it: &'x Item<'x>) -> Option<&'x TokenKind> {
    match it {
        Item::Tok(t, _) => Some(&t.kind),
        Item::Com(_) => None,
    }
}

/// Format `src`. Errors on a lexer failure; returns the formatted text
/// (already-formatted input → identical output).
pub fn format_source(src: &str, file: &Arc<str>) -> Result<String, Diagnostic> {
    let normalized;
    let src = if src.contains('\r') {
        normalized = src.replace("\r\n", "\n").replace('\r', "\n");
        normalized.as_str()
    } else {
        src
    };
    let lexed = Lexer::new(src, file.clone())
        .tokenize_with_comments()
        .map_err(|mut d| {
            d.message = format!("fmt: {}", d.message);
            d
        })?;
    let tokens = lexed.tokens;
    let ranges = lexed.ranges;
    let mut comments = lexed.comments;
    comments.sort_by_key(|c| (c.line, c.col));

    let mut stream: Vec<Item> = Vec::with_capacity(tokens.len() + comments.len());
    for (i, t) in tokens.iter().enumerate() {
        if t.kind == TokenKind::Eof {
            continue;
        }
        stream.push(Item::Tok(t, i));
    }
    for c in &comments {
        stream.push(Item::Com(c));
    }
    stream.sort_by_key(|it| match it {
        Item::Tok(t, _) => (t.span.line, t.span.col),
        Item::Com(c) => (c.line, c.col),
    });

    // --- line wrapping: pair up parentheses and measure flat widths.
    // A group is wrapped only when the source has it on one line, it contains
    // no comments, and its flat rendering would push the line past
    // MAX_WIDTH. Once wrapped it has source line breaks, so re-formatting
    // keeps it wrapped (idempotence does not depend on the measurement).
    let mut matching: HashMap<usize, usize> = HashMap::new();
    {
        let mut stack: Vec<usize> = Vec::new();
        for (i, t) in tokens.iter().enumerate() {
            match t.kind {
                TokenKind::LParen | TokenKind::LBracket => stack.push(i),
                TokenKind::RParen | TokenKind::RBracket => {
                    if let Some(open) = stack.pop() {
                        matching.insert(open, i);
                    }
                }
                _ => {}
            }
        }
    }

    let mut f = Formatter {
        src,
        ranges: &ranges,
        tokens: &tokens,
        matching: &matching,
        comments: &comments,
        out: String::with_capacity(src.len() + 64),
        depth: 0,
        paren: 0,
        brace: 0,
        for_header: Vec::new(),
        wrap_stack: Vec::new(),
        pending_break: false,
        line_open: false,
        after_unary: false,
        last_src_line: 0,
        cur_out_src_line: 0,
        first: true,
    };

    for idx in 0..stream.len() {
        // Lookahead for the `} else` / `} elif` / `} catch` / `} finally` join.
        let join_after_brace = matches!(
            (&stream[idx], stream.get(idx + 1).and_then(tok_kind)),
            (Item::Tok(t, _), Some(nt))
                if t.kind == TokenKind::RBrace
                    && matches!(
                        nt,
                        TokenKind::Kw(
                            Keyword::Else
                                | Keyword::Elif
                                | Keyword::Catch
                                | Keyword::Finally
                        )
                    )
        );

        match &stream[idx] {
            Item::Com(c) => {
                let text = c.text.trim_end();
                if f.line_open && c.line == f.cur_out_src_line {
                    // Trailing comment: attach to the line just written; a
                    // pending break from `;`/`{` stays pending.
                    f.out.push_str("  ");
                    f.out.push_str(text);
                } else {
                    // Own-line comment. Break only if the current line still
                    // has open content or a break is pending — a comment that
                    // follows another comment's own line continues directly.
                    if f.line_open || f.pending_break {
                        f.out.push('\n');
                    }
                    if !f.first && c.line > f.last_src_line + 1 && f.paren == 0 {
                        f.out.push('\n'); // preserve one blank line
                    }
                    f.indent();
                    f.out.push_str(text);
                    f.out.push('\n');
                    f.line_open = false;
                    f.pending_break = false;
                }
                f.last_src_line = f.last_src_line.max(c.line);
                f.first = false;
            }
            Item::Tok(tok, ti) => {
                let tline = tok.span.line;
                let prev = prev_tok_kind(&stream, idx);

                // Break before the token when the source had a line break,
                // a break is pending, or the token is `}` (always its own
                // line start unless it joins else/elif/catch/finally).
                // A `;` hugging a `}` (`fn() { ... };`) never breaks.
                let forced = tok.kind == TokenKind::RBrace && !join_after_brace;
                let semi_joins_brace =
                    tok.kind == TokenKind::Semi && prev == Some(&TokenKind::RBrace);
                let mut must_break = forced || (f.pending_break && !semi_joins_brace);
                // Source line breaks are preserved inside parentheses as well
                // (this is what keeps a wrapped argument list wrapped);
                // blank lines are still only allowed outside parens.
                let transparent_level = f
                    .wrap_stack
                    .last()
                    .map(|g| g.transparent && g.paren_level == f.paren && g.brace_at_open == f.brace)
                    .unwrap_or(false);
                if !must_break && !transparent_level && !f.first && tline > f.last_src_line {
                    must_break = true;
                }
                // In a transparent (closure-argument) group the call closes on
                // the same line as the body's `}`: `}))`.
                if transparent_level && tok.kind == TokenKind::RParen {
                    must_break = false;
                    f.pending_break = false;
                }
                let blank = must_break
                    && f.paren == 0
                    && !f.first
                    && tline > f.last_src_line + 1
                    && tok.kind != TokenKind::RBrace
                    && prev != Some(&TokenKind::LBrace)
                    && !matches!(
                        tok.kind,
                        TokenKind::Kw(
                            Keyword::Else
                                | Keyword::Elif
                                | Keyword::Catch
                                | Keyword::Finally
                        )
                    );
                if must_break {
                    if blank {
                        f.out.push('\n');
                    }
                    if f.line_open {
                        f.out.push('\n');
                    }
                    f.line_open = false;
                    f.pending_break = false;
                }

                // Separating space (when staying on the same output line).
                let spaced = match prev {
                    Some(p) => needs_space(p, &tok.kind),
                    None => false,
                };
                if f.line_open && spaced && f.cur_out_src_line == tline && !f.after_unary {
                    f.out.push(' ');
                }
                f.after_unary = false;

                match &tok.kind {
                    TokenKind::LBrace => {
                        f.put("{");
                        if f.for_header.last() == Some(&true) {
                            f.for_header.pop();
                        }
                        f.depth += 1;
                        f.brace += 1;
                        f.pending_break = true;
                    }
                    TokenKind::RBrace => {
                        f.depth = f.depth.saturating_sub(1);
                        f.brace = f.brace.saturating_sub(1);
                        f.put("}");
                        if !join_after_brace {
                            f.pending_break = true;
                        }
                    }
                    TokenKind::LParen => {
                        let (multiline, force_commas, transparent) = f.group_mode(*ti);
                        f.put("(");
                        f.paren += 1;
                        f.wrap_stack.push(WrapGroup {
                            multiline,
                            force_commas,
                            transparent,
                            brace_at_open: f.brace,
                            base_depth: f.depth,
                            paren_level: f.paren,
                        });
                        if multiline {
                            f.depth += 1;
                            if force_commas {
                                // First argument goes on its own line.
                                f.out.push('\n');
                                f.line_open = false;
                            }
                        }
                    }
                    TokenKind::RParen => {
                        f.paren = f.paren.saturating_sub(1);
                        if let Some(g) = f.wrap_stack.pop() {
                            if g.multiline {
                                f.depth = g.base_depth;
                                if g.force_commas && f.line_open {
                                    f.out.push('\n');
                                    f.line_open = false;
                                }
                            }
                        }
                        f.put(")");
                    }
                    TokenKind::LBracket => f.put("["),
                    TokenKind::RBracket => f.put("]"),
                    TokenKind::Comma => {
                        f.put(",");
                        // Inside a wrapped group, one item per line.
                        if let Some(g) = f.wrap_stack.last() {
                            if g.force_commas && f.paren == g.paren_level {
                                f.out.push('\n');
                                f.line_open = false;
                            }
                        }
                    }
                    TokenKind::Colon => f.put(":"),
                    TokenKind::Semi => {
                        f.put(";");
                        if !(f.for_header.last() == Some(&true) && f.paren == 0) && f.paren == 0 {
                            f.pending_break = true;
                        }
                    }
                    TokenKind::Dot => f.put("."),
                    TokenKind::Arrow => f.put("->"),
                    TokenKind::Kw(Keyword::For) => {
                        f.for_header.push(true);
                        f.put("for");
                    }
                    TokenKind::Kw(kw) => f.put(kw.as_str()),
                    TokenKind::Minus | TokenKind::Plus => {
                        let unary = !matches!(prev, Some(p) if ends_value(p));
                        f.put(if tok.kind == TokenKind::Minus { "-" } else { "+" });
                        f.after_unary = unary;
                    }
                    _ => {
                        // Literals and identifiers: raw source slice.
                        let (a, b) = f.ranges[*ti];
                        let raw = String::from_utf8_lossy(&f.src.as_bytes()[a..b]).into_owned();
                        f.put(&raw);
                    }
                }

                f.cur_out_src_line = tline;
                f.last_src_line = tline;
                f.first = false;
            }
        }
    }

    let mut out = f.out;
    while out.ends_with('\n') {
        out.pop();
    }
    if out.ends_with(INDENT) {
        // A break was pending at EOF with nothing after it; drop the indent.
        while out.ends_with(' ') {
            out.pop();
        }
        while out.ends_with(INDENT.chars().next().unwrap()) {
            out.pop();
        }
    }
    out.push('\n');

    // Safety net: the formatted text must lex to the same token stream.
    let relex = Lexer::new(&out, file.clone()).tokenize().map_err(|mut d| {
        d.message = format!("fmt: internal: output does not lex: {}", d.message);
        d
    })?;
    let original: Vec<TokenKind> = tokens.iter().map(|t| t.kind.clone()).collect();
    let reformatted: Vec<TokenKind> = relex.iter().map(|t| t.kind.clone()).collect();
    if original != reformatted {
        return Err(Diagnostic::new(
            Phase::Parser,
            "fmt: internal error: formatted output changed the token stream; file left untouched",
        ));
    }
    Ok(out)
}

/// A parenthesised group on the wrap stack.
struct WrapGroup {
    multiline: bool,
    /// Break after each top-level comma (one argument per line). False for
    /// groups that contain comments, whose layout follows the source.
    force_commas: bool,
    /// A group whose arguments contain a block (closure body): the braces own
    /// the layout, so line breaks at the group's own paren level are artifacts
    /// and are dropped (the call is kept on one line up to the `{`).
    transparent: bool,
    /// Brace nesting at the time the group opened, to tell the group's own
    /// level apart from the inside of a closure body.
    brace_at_open: usize,
    /// Indentation level of the line that opened the group.
    base_depth: usize,
    /// `paren` value just inside the group, so only commas belonging to this
    /// group (not to a nested one) trigger a break.
    paren_level: usize,
}

struct Formatter<'a> {
    src: &'a str,
    ranges: &'a [(usize, usize)],
    tokens: &'a [crate::lexer::token::Token],
    matching: &'a HashMap<usize, usize>,
    comments: &'a [crate::lexer::Comment],
    out: String,
    depth: usize,
    paren: usize,
    /// Current brace nesting (used to distinguish a group's own level from the
    /// inside of a closure body).
    brace: usize,
    /// Open parenthesised groups, innermost last (line wrapping).
    wrap_stack: Vec<WrapGroup>,
    /// One entry per open `for`: its header is active until the body `{`.
    for_header: Vec<bool>,
    /// A `;`/`{`/`}` ended the logical line; break before the next item.
    pending_break: bool,
    /// The current output line has content (indent already written).
    line_open: bool,
    /// A unary `-`/`+` was just emitted: no space before its operand.
    after_unary: bool,
    /// Source line of the last written item (token or comment).
    last_src_line: u32,
    /// Source line whose content is on the current output line.
    cur_out_src_line: u32,
    first: bool,
}

impl Formatter<'_> {
    /// Current output column (0-based) of the line being written.
    fn col(&self) -> usize {
        match self.out.rfind('\n') {
            Some(i) => self.out.len() - i - 1,
            None => self.out.len(),
        }
    }

    /// How the group opened at token `open` is laid out:
    /// `(multiline, force_comma_breaks)`.
    ///
    /// A group that already spans several source lines stays multiline (this
    /// is what makes wrapping idempotent — the layout decision is read back
    /// from the source). A single-line group becomes multiline only when its
    /// own flat width is substantial and it would overflow `MAX_WIDTH`.
    /// Groups containing comments follow the source layout and never get
    /// comma-forced breaks.
    fn group_mode(&self, open: usize) -> (bool, bool, bool) {
        let Some(&close) = self.matching.get(&open) else {
            return (false, false, false);
        };
        if close <= open + 1 {
            return (false, false, false); // `()` — nothing to lay out
        }
        let (l0, l1) = (self.tokens[open].span.line, self.tokens[close].span.line);
        let src_multiline = l0 != l1;
        // A group containing a block (a closure body such as
        // `xs.map(fn(x) { ... })`) keeps its source layout: the braces own the
        // indentation there, so "one argument per line" would only add noise.
        if self.tokens[open + 1..close]
            .iter()
            .any(|t| matches!(t.kind, TokenKind::LBrace))
        {
            // The braces own the indentation: keep the call compact and drop
            // line breaks at the group's own level.
            return (false, false, true);
        }
        if self.comments.iter().any(|c| c.line > l0 && c.line < l1) {
            return (src_multiline, false, false);
        }
        if src_multiline {
            return (true, true, false);
        }
        let flat = self.flat_width(open, close);
        // A group is only worth breaking if it is itself substantial: a short
        // call at the end of a long line stays flat (breaking it would just
        // spread three tiny arguments over three lines).
        let wrap = flat >= MAX_WIDTH / 2 && self.col() + flat > MAX_WIDTH;
        (wrap, wrap, false)
    }

    /// Approximate width of tokens `(open, close)` rendered on one line:
    /// each token's text plus one separating space.
    fn flat_width(&self, open: usize, close: usize) -> usize {
        let mut w = 0usize;
        for i in open + 1..close {
            w += token_text_len(&self.tokens[i], self.ranges[i]) + 1;
        }
        w
    }

    fn indent(&mut self) {
        for _ in 0..self.depth {
            self.out.push_str(INDENT);
        }
    }

    /// Write text on the current line, starting it (with indent) if needed.
    fn put(&mut self, s: &str) {
        if !self.line_open {
            self.indent();
            self.line_open = true;
        }
        self.out.push_str(s);
    }
}

fn prev_tok_kind<'x, 'y>(stream: &'x [Item<'y>], idx: usize) -> Option<&'x TokenKind>
where
    'y: 'x,
{
    let mut i = idx;
    while i > 0 {
        i -= 1;
        if let Item::Tok(t, _) = &stream[i] {
            return Some(&t.kind);
        }
    }
    None
}

/// True when the value-like token `prev` can end an expression, which makes a
/// following `-`/`+` binary (spaced) rather than a sign (tight).
fn ends_value(prev: &TokenKind) -> bool {
    matches!(
        prev,
        TokenKind::Int(_)
            | TokenKind::Float(_)
            | TokenKind::Str(_)
            | TokenKind::Ident(_)
            | TokenKind::RParen
            | TokenKind::RBracket
            | TokenKind::Kw(Keyword::True | Keyword::False | Keyword::Null | Keyword::SelfKw)
    )
}

/// Rendered length of one token, for width measurement.
fn token_text_len(t: &Token, range: (usize, usize)) -> usize {
    match &t.kind {
        TokenKind::Int(_) | TokenKind::Float(_) | TokenKind::Str(_) | TokenKind::Ident(_) => {
            range.1 - range.0
        }
        TokenKind::Kw(kw) => kw.as_str().len(),
        TokenKind::EqEq
        | TokenKind::BangEq
        | TokenKind::Le
        | TokenKind::Ge
        | TokenKind::PlusEq
        | TokenKind::MinusEq
        | TokenKind::StarEq
        | TokenKind::SlashEq
        | TokenKind::PercentEq
        | TokenKind::Arrow => 2,
        TokenKind::Eof => 0,
        _ => 1,
    }
}

/// The single spacing rule: does a space go between `prev` and `next` when
/// both land on the same output line? (A unary `-`/`+` additionally suppresses
/// the space before its own operand via `after_unary`.)
fn needs_space(prev: &TokenKind, next: &TokenKind) -> bool {
    match next {
        // Tight openers and closers.
        TokenKind::LParen
        | TokenKind::LBracket
        | TokenKind::Comma
        | TokenKind::Semi
        | TokenKind::Colon
        | TokenKind::Dot
        | TokenKind::RParen
        | TokenKind::RBracket => false,
        // Sign vs binary plus/minus: spaced after a value or a keyword
        // (`a + 1`, `return -x`), tight after separators and operators
        // (`(-1`, `x = -1`, `== -1`).
        TokenKind::Minus | TokenKind::Plus => {
            ends_value(prev) || matches!(prev, TokenKind::Kw(_))
        }
        _ => match prev {
            // Nothing after these on the same line is spaced (`(x`, `[x`).
            TokenKind::LParen | TokenKind::LBracket | TokenKind::Dot => false,
            _ => true,
        },
    }
}
