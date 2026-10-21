//! Recursive-descent parser with Pratt-style expression precedence.

use crate::ast::*;
use crate::diagnostics::{err_at, Phase, Span, SpidResult};
use crate::lexer::token::{Keyword, Token, TokenKind};
use std::sync::Arc;

/// Maximum expression/statement nesting.
///
/// Each level costs roughly one stack frame per precedence function (~13), so
/// the cap has to stay well inside a 2 MB thread stack — the default for a
/// spawned thread, and the size the test suite runs on. At 96 the parser sat on
/// the edge (adding one precedence level overflowed it), so 64 leaves room.
const MAX_PARSE_DEPTH: usize = 64;

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
    depth: usize,
    /// Depth of `if`-expression branch blocks. Inside one, a trailing
    /// expression may omit its `;`, because it is the branch's value:
    /// `let x = if (c) { 1 } else { 2 };`
    value_block_depth: usize,
}

pub fn parse(toks: Vec<Token>) -> SpidResult<Program> {
    // The lexer always appends EOF, so an empty stream can only come from a
    // direct API caller — but `kind()`/`span()` index `toks[0]` unconditionally,
    // so an empty vector used to panic here (audit, low severity).
    if toks.is_empty() {
        return Err(crate::diagnostics::Diagnostic::new(
            Phase::Parser,
            "cannot parse an empty token stream (the lexer always emits EOF)",
        ));
    }
    let mut p = Parser {
        toks,
        pos: 0,
        depth: 0,
        value_block_depth: 0,
    };
    p.parse_program()
}

impl Parser {
    // ---- token helpers -------------------------------------------------

    fn kind(&self) -> &TokenKind {
        &self.toks[self.pos].kind
    }

    fn peek_kind(&self, n: usize) -> &TokenKind {
        // `saturating_sub`: an empty stream would otherwise underflow here even
        // if a caller slipped past `parse`'s guard.
        let last = self.toks.len().saturating_sub(1);
        let i = (self.pos + n).min(last);
        &self.toks[i].kind
    }

    fn span(&self) -> Span {
        self.toks[self.pos].span.clone()
    }

    fn prev_span(&self) -> Span {
        self.toks[self.pos.saturating_sub(1)].span.clone()
    }

    fn at_kw(&self, kw: Keyword) -> bool {
        matches!(self.kind(), TokenKind::Kw(k) if *k == kw)
    }

    fn eat_kw(&mut self, kw: Keyword) -> bool {
        if self.at_kw(kw) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn at(&self, k: &TokenKind) -> bool {
        self.kind() == k
    }

    fn eat(&mut self, k: &TokenKind) -> bool {
        if self.at(k) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, k: &TokenKind, what: &str) -> SpidResult<()> {
        if self.eat(k) {
            Ok(())
        } else {
            Err(err_at(
                Phase::Parser,
                self.span(),
                format!("expected {}, found {}", what, describe(self.kind())),
            ))
        }
    }

    fn expect_ident(&mut self) -> SpidResult<(String, Span)> {
        match self.kind().clone() {
            TokenKind::Ident(s) => {
                let sp = self.span();
                self.pos += 1;
                Ok((s, sp))
            }
            TokenKind::Kw(Keyword::SelfKw) => {
                let sp = self.span();
                self.pos += 1;
                Ok(("self".to_string(), sp))
            }
            _ => Err(err_at(
                Phase::Parser,
                self.span(),
                format!("expected an identifier, found {}", describe(self.kind())),
            )),
        }
    }

    fn expect_kw(&mut self, kw: Keyword) -> SpidResult<Span> {
        if self.at_kw(kw) {
            let sp = self.span();
            self.pos += 1;
            Ok(sp)
        } else {
            Err(err_at(
                Phase::Parser,
                self.span(),
                format!(
                    "expected '{}', found {}",
                    kw.as_str(),
                    describe(self.kind())
                ),
            ))
        }
    }

    fn err_here(&self, msg: impl Into<String>) -> crate::diagnostics::Diagnostic {
        err_at(Phase::Parser, self.span(), msg)
    }

    // ---- program -------------------------------------------------------

    fn parse_program(&mut self) -> SpidResult<Program> {
        let mut stmts = Vec::new();
        let mut seen_non_import = false;
        while !self.at(&TokenKind::Eof) {
            if self.at_kw(Keyword::Import) {
                if seen_non_import {
                    return Err(self.err_here("import must appear before other declarations"));
                }
                stmts.push(self.parse_import()?);
            } else {
                seen_non_import = true;
                stmts.push(self.parse_declaration()?);
            }
        }
        Ok(Program { stmts })
    }

    fn parse_import(&mut self) -> SpidResult<Stmt> {
        let sp = self.expect_kw(Keyword::Import)?;
        let path = match self.kind().clone() {
            TokenKind::Str(s) => {
                self.pos += 1;
                s
            }
            _ => {
                return Err(self
                    .err_here("import expects a quoted module path, e.g. import \"math_util\";"))
            }
        };
        let alias = if self.eat_kw(Keyword::As) {
            let (name, _) = self.expect_ident()?;
            Some(name)
        } else {
            None
        };
        self.expect(&TokenKind::Semi, "';' after import")?;
        Ok(Stmt::Import {
            path,
            alias,
            span: sp,
        })
    }

    fn parse_declaration(&mut self) -> SpidResult<Stmt> {
        // A module-level `public` / `private` may precede any declaration:
        // fn, class, interface, const, or let. `private` is the default.
        let mut vis = Vis::Private;
        let mut had_vis = false;
        if self.eat_kw(Keyword::Public) {
            vis = Vis::Public;
            had_vis = true;
        } else if self.eat_kw(Keyword::Private) {
            had_vis = true;
        }
        match self.kind() {
            TokenKind::Kw(Keyword::Fn) | TokenKind::Kw(Keyword::Async) => Ok(Stmt::Fn(
                self.parse_fn(false, if had_vis { Some(vis) } else { None })?,
            )),
            TokenKind::Kw(Keyword::Class) => Ok(Stmt::Class(self.parse_class(vis)?)),
            TokenKind::Kw(Keyword::Interface) => Ok(Stmt::Interface(self.parse_interface(vis)?)),
            TokenKind::Kw(Keyword::Enum) => Ok(Stmt::Enum(self.parse_enum(vis)?)),
            TokenKind::Kw(Keyword::Let) | TokenKind::Kw(Keyword::Const) => {
                let s = self.parse_statement()?;
                // attach the visibility the declaration carried
                Ok(match s {
                    Stmt::Let {
                        name,
                        ty,
                        value,
                        span,
                        ..
                    } => Stmt::Let {
                        vis,
                        name,
                        ty,
                        value,
                        span,
                    },
                    Stmt::Const {
                        name,
                        ty,
                        value,
                        span,
                        ..
                    } => Stmt::Const {
                        vis,
                        name,
                        ty,
                        value,
                        span,
                    },
                    other => {
                        if had_vis {
                            return Err(self.err_here(
                                "public/private must precede a fn, class, interface, const, or let",
                            ));
                        }
                        other
                    }
                })
            }
            _ => {
                if had_vis {
                    return Err(self.err_here(
                        "public/private must precede a fn, class, interface, const, or let",
                    ));
                }
                self.parse_statement()
            }
        }
    }

    // ---- functions -----------------------------------------------------

    /// `allow_anon` permits the nameless `fn(...) {...}` literal form.
    /// `pre_vis` is the visibility already read by a caller (module-level
    /// declarations); when None the keyword is read here (class bodies).
    fn parse_fn(&mut self, allow_anon: bool, pre_vis: Option<Vis>) -> SpidResult<FnProto> {
        let start = self.span();
        let mut vis = pre_vis.unwrap_or(Vis::Private);
        if pre_vis.is_none() {
            if self.eat_kw(Keyword::Public) {
                vis = Vis::Public;
            } else if self.eat_kw(Keyword::Private) {
                vis = Vis::Private;
            }
        }
        let is_async = self.eat_kw(Keyword::Async);
        self.expect_kw(Keyword::Fn)?;
        let name = match self.kind().clone() {
            TokenKind::Ident(s) => {
                self.pos += 1;
                Some(s)
            }
            TokenKind::Kw(Keyword::New) => {
                // inside classes the constructor is `fn new(...)`
                self.pos += 1;
                Some("new".to_string())
            }
            _ if allow_anon && matches!(self.kind(), TokenKind::LParen) => None,
            _ => {
                return Err(self.err_here(format!(
                    "expected a function name, found {}",
                    describe(self.kind())
                )))
            }
        };
        if name.is_none() && !allow_anon {
            return Err(self.err_here("a function declaration needs a name"));
        }
        let params = self.parse_params()?;
        let ret = if self.eat(&TokenKind::Arrow) {
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = self.parse_block()?;
        let span = join(start, body.span.clone());
        Ok(FnProto {
            vis,
            is_async,
            name,
            params,
            ret,
            body,
            span,
        })
    }

    fn parse_params(&mut self) -> SpidResult<Vec<Param>> {
        self.expect(&TokenKind::LParen, "'('")?;
        let mut params = Vec::new();
        if !self.at(&TokenKind::RParen) {
            loop {
                let sp = self.span();
                let (name, _) = self.expect_ident()?;
                let ty = if self.eat(&TokenKind::Colon) {
                    Some(self.parse_type()?)
                } else {
                    None
                };
                params.push(Param { name, ty, span: sp });
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen, "')'")?;
        Ok(params)
    }

    fn parse_type(&mut self) -> SpidResult<TypeExpr> {
        if self.depth >= MAX_PARSE_DEPTH {
            return Err(err_at(
                Phase::Parser,
                self.span(),
                format!("nesting deeper than {} levels", MAX_PARSE_DEPTH),
            ));
        }
        self.depth += 1;
        let res = (|| {
            let (name, span) = self.expect_ident()?;
            let arg = if self.eat(&TokenKind::Lt) {
                let t = self.parse_type()?;
                self.expect(&TokenKind::Gt, "'>'")?;
                Some(Box::new(t))
            } else {
                None
            };
            Ok(TypeExpr { name, arg, span })
        })();
        self.depth -= 1;
        res
    }

    // ---- classes and interfaces ----------------------------------------

    fn parse_class(&mut self, vis: Vis) -> SpidResult<ClassDecl> {
        let start = self.expect_kw(Keyword::Class)?;
        let (name, _) = self.expect_ident()?;
        let parent = if self.eat_kw(Keyword::Extends) {
            Some(self.expect_ident()?.0)
        } else {
            None
        };
        let mut interfaces = Vec::new();
        if self.eat_kw(Keyword::Implements) {
            interfaces.push(self.expect_ident()?.0);
            while self.eat(&TokenKind::Comma) {
                interfaces.push(self.expect_ident()?.0);
            }
        }
        self.expect(&TokenKind::LBrace, "'{' to open the class body")?;
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        while !self.at(&TokenKind::RBrace) {
            if self.at(&TokenKind::Eof) {
                return Err(self.err_here("unterminated class body"));
            }
            let vis = if self.eat_kw(Keyword::Public) {
                Vis::Public
            } else {
                // `private` is the default; the keyword is optional and consumed
                self.eat_kw(Keyword::Private);
                Vis::Private
            };
            if self.eat_kw(Keyword::Let) {
                let (fname, fspan) = self.expect_ident()?;
                let ty = if self.eat(&TokenKind::Colon) {
                    Some(self.parse_type()?)
                } else {
                    None
                };
                self.expect(&TokenKind::Semi, "';' after field declaration")?;
                fields.push(FieldDecl {
                    name: fname,
                    vis,
                    ty,
                    span: fspan,
                });
            } else if self.at_kw(Keyword::Fn) || self.at_kw(Keyword::Async) {
                let mut m = self.parse_fn(false, None)?;
                // parse_fn consumed its own (absent) modifier; the class-level
                // visibility wins.
                m.vis = vis;
                methods.push(m);
            } else if self.at_kw(Keyword::New) {
                // constructor shorthand: `new(params) { ... }`
                let sp = self.expect_kw(Keyword::New)?;
                let params = self.parse_params()?;
                let body = self.parse_block()?;
                let body_span = body.span.clone();
                methods.push(FnProto {
                    vis,
                    is_async: false,
                    name: Some("new".to_string()),
                    params,
                    ret: None,
                    body,
                    span: join(sp, body_span),
                });
            } else {
                return Err(self.err_here(format!(
                    "expected a field or method in class body, found {}",
                    describe(self.kind())
                )));
            }
        }
        let end = self.expect_kw_rbrace()?;
        Ok(ClassDecl {
            vis,
            name,
            parent,
            interfaces,
            fields,
            methods,
            span: join(start, end),
        })
    }

    fn expect_kw_rbrace(&mut self) -> SpidResult<Span> {
        if self.eat(&TokenKind::RBrace) {
            Ok(self.prev_span())
        } else {
            Err(self.err_here("expected '}'"))
        }
    }

    /// `enum Name { A, B(x), C(x, y) }`
    fn parse_enum(&mut self, vis: Vis) -> SpidResult<EnumDecl> {
        let start = self.expect_kw(Keyword::Enum)?;
        let (name, _) = self.expect_ident()?;
        self.expect(&TokenKind::LBrace, "'{' to open the enum body")?;
        let mut variants = Vec::new();
        while !self.at(&TokenKind::RBrace) {
            if self.at(&TokenKind::Eof) {
                return Err(self.err_here("unterminated enum body"));
            }
            let (vname, vspan) = self.expect_ident()?;
            let mut fields = Vec::new();
            if self.eat(&TokenKind::LParen) {
                while !self.at(&TokenKind::RParen) {
                    let (fname, _) = self.expect_ident()?;
                    fields.push(fname);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen, "')' after the variant's fields")?;
            }
            variants.push(VariantDecl {
                name: vname,
                fields,
                span: vspan,
            });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        let end = self.expect_kw_rbrace()?;
        Ok(EnumDecl {
            vis,
            name,
            variants,
            span: join(start, end),
        })
    }

    fn parse_interface(&mut self, vis: Vis) -> SpidResult<InterfaceDecl> {
        let start = self.expect_kw(Keyword::Interface)?;
        let (name, _) = self.expect_ident()?;
        self.expect(&TokenKind::LBrace, "'{' to open the interface body")?;
        let mut methods = Vec::new();
        while !self.at(&TokenKind::RBrace) {
            if self.at(&TokenKind::Eof) {
                return Err(self.err_here("unterminated interface body"));
            }
            self.expect_kw(Keyword::Fn)?;
            let (mname, mspan) = self.expect_ident()?;
            let params = self.parse_params()?;
            let ret = if self.eat(&TokenKind::Arrow) {
                Some(self.parse_type()?)
            } else {
                None
            };
            self.expect(&TokenKind::Semi, "';' after interface method")?;
            methods.push((mname, params, ret, mspan));
        }
        let end = self.expect_kw_rbrace()?;
        Ok(InterfaceDecl {
            vis,
            name,
            methods,
            span: join(start, end),
        })
    }

    // ---- statements ----------------------------------------------------

    fn parse_block(&mut self) -> SpidResult<Block> {
        if self.depth >= MAX_PARSE_DEPTH {
            return Err(err_at(
                Phase::Parser,
                self.span(),
                format!("nesting deeper than {} levels", MAX_PARSE_DEPTH),
            ));
        }
        self.depth += 1;
        let res = (|| {
            let start = self
                .expect(&TokenKind::LBrace, "'{'")
                .map(|()| self.prev_span())?;
            let mut stmts = Vec::new();
            while !self.at(&TokenKind::RBrace) {
                if self.at(&TokenKind::Eof) {
                    return Err(err_at(
                        Phase::Parser,
                        start,
                        "unterminated block; expected '}'",
                    ));
                }
                stmts.push(self.parse_statement()?);
            }
            let end = self.expect_kw_rbrace()?;
            Ok(Block {
                stmts,
                span: join(start, end),
            })
        })();
        self.depth -= 1;
        res
    }

    /// A block in value position: like `parse_block`, but the trailing
    /// expression may omit its `;`.
    fn parse_value_block(&mut self) -> SpidResult<Block> {
        self.value_block_depth += 1;
        let res = self.parse_block();
        self.value_block_depth -= 1;
        res
    }

    fn parse_statement(&mut self) -> SpidResult<Stmt> {
        match self.kind() {
            TokenKind::Kw(Keyword::Let) => self.parse_let(true),
            TokenKind::Kw(Keyword::Const) => self.parse_const(),
            TokenKind::Kw(Keyword::Fn) if matches!(self.peek_kind(1), TokenKind::Ident(_)) => {
                Ok(Stmt::Fn(self.parse_fn(false, Some(Vis::Private))?))
            }
            TokenKind::Kw(Keyword::Async)
                if matches!(self.peek_kind(1), TokenKind::Kw(Keyword::Fn))
                    && matches!(self.peek_kind(2), TokenKind::Ident(_)) =>
            {
                Ok(Stmt::Fn(self.parse_fn(false, Some(Vis::Private))?))
            }
            TokenKind::Kw(Keyword::If) => self.parse_if(),
            TokenKind::Kw(Keyword::While) => self.parse_while(),
            TokenKind::Kw(Keyword::For) => self.parse_for(),
            TokenKind::Kw(Keyword::Return) => {
                let sp = self.span();
                self.pos += 1;
                let value = if self.at(&TokenKind::Semi) {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                self.expect(&TokenKind::Semi, "';' after return")?;
                Ok(Stmt::Return { value, span: sp })
            }
            TokenKind::Kw(Keyword::Break) => {
                let sp = self.span();
                self.pos += 1;
                self.expect(&TokenKind::Semi, "';' after break")?;
                Ok(Stmt::Break { span: sp })
            }
            TokenKind::Kw(Keyword::Continue) => {
                let sp = self.span();
                self.pos += 1;
                self.expect(&TokenKind::Semi, "';' after continue")?;
                Ok(Stmt::Continue { span: sp })
            }
            TokenKind::Kw(Keyword::Try) => self.parse_try(),
            TokenKind::Kw(Keyword::Throw) => {
                let sp = self.span();
                self.pos += 1;
                let value = self.parse_expr()?;
                self.expect(&TokenKind::Semi, "';' after throw")?;
                Ok(Stmt::Throw { value, span: sp })
            }
            TokenKind::Kw(Keyword::Spawn) => {
                let sp = self.span();
                self.pos += 1;
                let callable = self.parse_expr()?;
                self.expect(&TokenKind::Semi, "';' after spawn")?;
                if matches!(callable, Expr::Fn(_)) {
                    Ok(Stmt::Spawn { callable, span: sp })
                } else {
                    Err(self.err_here("spawn expects a function: spawn fn() { ... };"))
                }
            }
            // A `match` in statement position: like `if`, the trailing `;` is
            // optional because the arm bodies are already delimited.
            TokenKind::Kw(Keyword::Match) => {
                let e = self.parse_match_expr()?;
                let _ = self.eat(&TokenKind::Semi);
                Ok(Stmt::Expr(e))
            }
            TokenKind::LBrace => Ok(Stmt::Block(self.parse_block()?)),
            _ => {
                let e = self.parse_expr()?;
                // The last expression of an `if`-expression branch is its
                // value, so the `;` is optional there.
                if !(self.value_block_depth > 0 && self.at(&TokenKind::RBrace)) {
                    self.expect(&TokenKind::Semi, "';' after expression")?;
                }
                Ok(Stmt::Expr(e))
            }
        }
    }

    fn parse_let(&mut self, with_semi: bool) -> SpidResult<Stmt> {
        self.parse_let_inner(with_semi, true)
    }

    fn parse_let_inner(&mut self, with_semi: bool, eat_let: bool) -> SpidResult<Stmt> {
        let sp = if eat_let {
            self.expect_kw(Keyword::Let)?
        } else {
            self.prev_span()
        };
        let (name, _) = self.expect_ident()?;
        let ty = if self.eat(&TokenKind::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        let value = if self.eat(&TokenKind::Assign) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        if with_semi {
            self.expect(&TokenKind::Semi, "';' after let")?;
        }
        Ok(Stmt::Let {
            vis: Vis::Private,
            name,
            ty,
            value,
            span: sp,
        })
    }

    fn parse_const(&mut self) -> SpidResult<Stmt> {
        let sp = self.expect_kw(Keyword::Const)?;
        let (name, _) = self.expect_ident()?;
        let ty = if self.eat(&TokenKind::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(&TokenKind::Assign, "'=' in const declaration")?;
        let value = self.parse_expr()?;
        self.expect(&TokenKind::Semi, "';' after const")?;
        Ok(Stmt::Const {
            vis: Vis::Private,
            name,
            ty,
            value,
            span: sp,
        })
    }

    /// `if` in value position: `let x = if (c) { a } else { b };`.
    ///
    /// `else if` chains are represented by nesting the second `if` inside a
    /// one-statement block, so `lower` only ever sees a single `els` block.
    fn parse_if_expr(&mut self) -> SpidResult<Expr> {
        let sp = self.expect_kw(Keyword::If)?;
        let cond = self.parse_expr()?;
        let then = self.parse_value_block()?;
        let mut els = None;
        if self.eat_kw(Keyword::Else) {
            if self.at_kw(Keyword::If) {
                let nested = self.parse_if_expr()?;
                let span = nested.span().clone();
                els = Some(Block {
                    stmts: vec![Stmt::Expr(nested)],
                    span,
                });
            } else {
                els = Some(self.parse_value_block()?);
            }
        }
        let end_span = els
            .as_ref()
            .map(|b| b.span.clone())
            .unwrap_or_else(|| then.span.clone());
        let span = join(sp, end_span);
        Ok(Expr::If {
            cond: Box::new(cond),
            then,
            els,
            span,
        })
    }

    /// `match (value) { Pattern => body, … }` — the same parser serves the
    /// statement and the value form.
    fn parse_match_expr(&mut self) -> SpidResult<Expr> {
        let start = self.expect_kw(Keyword::Match)?;
        self.expect(&TokenKind::LParen, "'(' after match")?;
        let scrutinee = self.parse_expr()?;
        self.expect(&TokenKind::RParen, "')' after the matched value")?;
        self.expect(&TokenKind::LBrace, "'{' to open the match arms")?;
        let mut arms = Vec::new();
        while !self.at(&TokenKind::RBrace) {
            if self.at(&TokenKind::Eof) {
                return Err(self.err_here("unterminated match body"));
            }
            let arm_start = self.span();
            let pattern = self.parse_pattern()?;
            let guard = if self.eat_kw(Keyword::If) {
                Some(self.parse_expr()?)
            } else {
                None
            };
            self.expect(&TokenKind::FatArrow, "'=>' after the pattern")?;
            // `=> expr` and `=> { statements; value }` are both accepted; the
            // bare form is wrapped in a one-statement block.
            let body = if self.at(&TokenKind::LBrace) {
                self.parse_value_block()?
            } else {
                let e = self.parse_expr()?;
                let span = e.span().clone();
                Block {
                    stmts: vec![Stmt::Expr(e)],
                    span,
                }
            };
            let span = join(arm_start, body.span.clone());
            arms.push(MatchArm {
                pattern,
                guard,
                body,
                span,
            });
            // Arms may be separated by a comma, or just follow each other.
            let _ = self.eat(&TokenKind::Comma);
        }
        let end = self.expect_kw_rbrace()?;
        Ok(Expr::Match {
            scrutinee: Box::new(scrutinee),
            arms,
            span: join(start, end),
        })
    }

    fn parse_pattern(&mut self) -> SpidResult<Pattern> {
        match self.kind().clone() {
            TokenKind::Ident(name) => {
                let sp = self.span();
                self.pos += 1;
                // `Name(...)` is a variant pattern; a bare identifier binds.
                if self.at(&TokenKind::LParen) {
                    self.pos += 1;
                    let mut args = Vec::new();
                    while !self.at(&TokenKind::RParen) {
                        args.push(self.parse_pattern()?);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(&TokenKind::RParen, "')' after the variant's fields")?;
                    return Ok(Pattern::Variant {
                        name,
                        args,
                        span: sp,
                    });
                }
                if name == "_" {
                    Ok(Pattern::Wild)
                } else {
                    Ok(Pattern::Bind(name))
                }
            }
            TokenKind::Int(_)
            | TokenKind::Float(_)
            | TokenKind::Str(_)
            | TokenKind::InterpStr(_)
            | TokenKind::Kw(Keyword::True)
            | TokenKind::Kw(Keyword::False)
            | TokenKind::Kw(Keyword::Null) => {
                let e = self.parse_primary()?;
                Ok(Pattern::Literal(e))
            }
            _ => Err(self
                .err_here("expected a pattern: `_`, a name to bind, a literal, or `Variant(...)`")),
        }
    }

    fn parse_if(&mut self) -> SpidResult<Stmt> {
        let sp = self.expect_kw(Keyword::If)?;
        let cond = self.parse_expr()?;
        let then = self.parse_block()?;
        let mut elifs = Vec::new();
        let mut els = None;
        loop {
            if self.eat_kw(Keyword::Elif) {
                let c = self.parse_expr()?;
                let b = self.parse_block()?;
                elifs.push((c, b));
            } else if self.eat_kw(Keyword::Else) {
                els = Some(self.parse_block()?);
                break;
            } else {
                break;
            }
        }
        let end_span = els
            .as_ref()
            .map(|b| b.span.clone())
            .or_else(|| elifs.last().map(|(_, b)| b.span.clone()))
            .unwrap_or_else(|| then.span.clone());
        let span = join(sp, end_span);
        Ok(Stmt::If {
            cond,
            then,
            elifs,
            els,
            span,
        })
    }

    fn parse_while(&mut self) -> SpidResult<Stmt> {
        let sp = self.expect_kw(Keyword::While)?;
        let cond = self.parse_expr()?;
        let body = self.parse_block()?;
        let span = join(sp, body.span.clone());
        Ok(Stmt::While { cond, body, span })
    }

    fn parse_for(&mut self) -> SpidResult<Stmt> {
        let sp = self.expect_kw(Keyword::For)?;
        if matches!(self.peek_kind(1), TokenKind::Kw(Keyword::In)) {
            let (var, _) = self.expect_ident()?;
            self.expect_kw(Keyword::In)?;
            let iter = self.parse_expr()?;
            let body = self.parse_block()?;
            let span = join(sp, body.span.clone());
            return Ok(Stmt::ForIn {
                var,
                iter,
                body,
                span,
            });
        }
        self.expect_kw(Keyword::Let)?;
        let init = self.parse_let_inner(false, false)?;
        self.expect(&TokenKind::Semi, "';' after for-init")?;
        let cond = self.parse_expr()?;
        self.expect(&TokenKind::Semi, "';' after for-condition")?;
        let post = self.parse_expr()?;
        let body = self.parse_block()?;
        let span = join(sp, body.span.clone());
        Ok(Stmt::ForC {
            init: Box::new(init),
            cond,
            post,
            body,
            span,
        })
    }

    fn parse_try(&mut self) -> SpidResult<Stmt> {
        let sp = self.expect_kw(Keyword::Try)?;
        let body = self.parse_block()?;
        let catch = if self.eat_kw(Keyword::Catch) {
            let catch_sp = self.prev_span();
            self.expect(&TokenKind::LParen, "'('")?;
            let (name, _) = self.expect_ident()?;
            self.expect(&TokenKind::RParen, "')'")?;
            let cbody = self.parse_block()?;
            let cspan = join(catch_sp, cbody.span.clone());
            Some(CatchClause {
                name,
                body: cbody,
                span: cspan,
            })
        } else {
            None
        };
        let finally = if self.eat_kw(Keyword::Finally) {
            Some(self.parse_block()?)
        } else {
            None
        };
        if catch.is_none() && finally.is_none() {
            return Err(err_at(
                Phase::Parser,
                sp,
                "try needs a catch or a finally block",
            ));
        }
        let end_span = finally
            .as_ref()
            .map(|f| f.span.clone())
            .or_else(|| catch.as_ref().map(|c| c.span.clone()))
            .unwrap_or_else(|| body.span.clone());
        let span = join(sp, end_span);
        Ok(Stmt::Try {
            body,
            catch,
            finally,
            span,
        })
    }

    // ---- expressions ---------------------------------------------------

    pub fn parse_expr(&mut self) -> SpidResult<Expr> {
        if self.depth >= MAX_PARSE_DEPTH {
            return Err(err_at(
                Phase::Parser,
                self.span(),
                format!("nesting deeper than {} levels", MAX_PARSE_DEPTH),
            ));
        }
        self.depth += 1;
        let res = self.parse_assign();
        self.depth -= 1;
        res
    }

    fn parse_assign(&mut self) -> SpidResult<Expr> {
        let left = self.parse_coalesce_inline()?;
        let op = match self.kind() {
            TokenKind::Assign => Some(AssignOp::Assign),
            TokenKind::PlusEq => Some(AssignOp::AddAssign),
            TokenKind::MinusEq => Some(AssignOp::SubAssign),
            TokenKind::StarEq => Some(AssignOp::MulAssign),
            TokenKind::SlashEq => Some(AssignOp::DivAssign),
            TokenKind::PercentEq => Some(AssignOp::ModAssign),
            _ => None,
        };
        if let Some(op) = op {
            let sp = left.span().clone();
            let target_is_ok = matches!(
                &left,
                Expr::Ident(_, _) | Expr::Index { .. } | Expr::Field { .. }
            );
            if !target_is_ok {
                return Err(err_at(
                    Phase::Parser,
                    sp,
                    "invalid assignment target; expected a variable, field, or index",
                ));
            }
            self.pos += 1;
            if self.depth >= MAX_PARSE_DEPTH {
                return Err(err_at(
                    Phase::Parser,
                    self.span(),
                    format!("nesting deeper than {} levels", MAX_PARSE_DEPTH),
                ));
            }
            self.depth += 1;
            let value_res = self.parse_assign();
            self.depth -= 1;
            let value = value_res?;
            let span = join(sp, value.span().clone());
            return Ok(Expr::Assign {
                target: Box::new(left),
                op,
                value: Box::new(value),
                span,
            });
        }
        Ok(left)
    }

    /// `a ?? b` — null coalescing, binding looser than `or` (as in C#) and
    /// tighter than assignment. Left-associative.
    ///
    /// Folded into `parse_assign` rather than given its own function: every
    /// level of nesting costs one stack frame per precedence level, and at the
    /// depth cap the parser was already close to a 2 MB thread's limit.
    fn parse_coalesce_inline(&mut self) -> SpidResult<Expr> {
        let mut left = self.parse_or()?;
        while matches!(self.kind(), TokenKind::QuestionQuestion) {
            self.pos += 1;
            let right = self.parse_or()?;
            let span = join(left.span().clone(), right.span().clone());
            left = Expr::Coalesce {
                l: Box::new(left),
                r: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    fn parse_or(&mut self) -> SpidResult<Expr> {
        let mut left = self.parse_and()?;
        while self.at_kw(Keyword::Or) {
            self.pos += 1;
            let right = self.parse_and()?;
            let span = join(left.span().clone(), right.span().clone());
            left = Expr::Logical {
                op: LogOp::Or,
                l: Box::new(left),
                r: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> SpidResult<Expr> {
        let mut left = self.parse_bit_or()?;
        while self.at_kw(Keyword::And) {
            self.pos += 1;
            let right = self.parse_bit_or()?;
            let span = join(left.span().clone(), right.span().clone());
            left = Expr::Logical {
                op: LogOp::And,
                l: Box::new(left),
                r: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    // Bitwise operators follow C precedence: `|` < `^` < `&` < comparison <
    // `<<`/`>>` < additive. Each level is a stdlib call, not a new opcode, so
    // there is exactly one implementation for all three engines.

    fn parse_bit_or(&mut self) -> SpidResult<Expr> {
        let mut left = self.parse_bit_xor()?;
        while matches!(self.kind(), TokenKind::Pipe) {
            self.pos += 1;
            let right = self.parse_bit_xor()?;
            left = bit_call("bit_or", left, right);
        }
        Ok(left)
    }

    fn parse_bit_xor(&mut self) -> SpidResult<Expr> {
        let mut left = self.parse_bit_and()?;
        while matches!(self.kind(), TokenKind::Caret) {
            self.pos += 1;
            let right = self.parse_bit_and()?;
            left = bit_call("bit_xor", left, right);
        }
        Ok(left)
    }

    fn parse_bit_and(&mut self) -> SpidResult<Expr> {
        let mut left = self.parse_cmp()?;
        while matches!(self.kind(), TokenKind::Amp) {
            self.pos += 1;
            let right = self.parse_cmp()?;
            left = bit_call("bit_and", left, right);
        }
        Ok(left)
    }

    fn parse_shift(&mut self) -> SpidResult<Expr> {
        let mut left = self.parse_add()?;
        loop {
            let name = match self.kind() {
                TokenKind::Shl => "shl",
                TokenKind::Shr => "shr",
                _ => break,
            };
            self.pos += 1;
            let right = self.parse_add()?;
            left = bit_call(name, left, right);
        }
        Ok(left)
    }

    fn parse_cmp(&mut self) -> SpidResult<Expr> {
        let mut left = self.parse_shift()?;
        loop {
            let op = match self.kind() {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::BangEq => BinOp::Ne,
                TokenKind::Lt => BinOp::Lt,
                TokenKind::Le => BinOp::Le,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::Ge => BinOp::Ge,
                _ => break,
            };
            self.pos += 1;
            let right = self.parse_shift()?;
            let span = join(left.span().clone(), right.span().clone());
            left = Expr::Binary {
                op,
                l: Box::new(left),
                r: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    fn parse_add(&mut self) -> SpidResult<Expr> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.kind() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.pos += 1;
            let right = self.parse_mul()?;
            let span = join(left.span().clone(), right.span().clone());
            left = Expr::Binary {
                op,
                l: Box::new(left),
                r: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> SpidResult<Expr> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.kind() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Percent => BinOp::Mod,
                _ => break,
            };
            self.pos += 1;
            let right = self.parse_unary()?;
            let span = join(left.span().clone(), right.span().clone());
            left = Expr::Binary {
                op,
                l: Box::new(left),
                r: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> SpidResult<Expr> {
        if self.depth >= MAX_PARSE_DEPTH {
            return Err(err_at(
                Phase::Parser,
                self.span(),
                format!("nesting deeper than {} levels", MAX_PARSE_DEPTH),
            ));
        }
        self.depth += 1;
        let res = (|| {
            let sp = self.span();
            if self.at_kw(Keyword::Await) {
                self.pos += 1;
                let e = self.parse_unary()?;
                let span = join(sp, e.span().clone());
                return Ok(Expr::Await(Box::new(e), span));
            }
            if self.at_kw(Keyword::Spawn) {
                self.pos += 1;
                let callable = self.parse_unary()?;
                let span = join(sp, callable.span().clone());
                if !matches!(callable, Expr::Fn(_)) {
                    return Err(err_at(Phase::Parser, span, "spawn expects a function"));
                }
                return Ok(Expr::Spawn {
                    callable: Box::new(callable),
                    span,
                });
            }
            if self.at_kw(Keyword::Not) {
                self.pos += 1;
                let e = self.parse_unary()?;
                let span = join(sp, e.span().clone());
                return Ok(Expr::Unary {
                    op: UnOp::Not,
                    expr: Box::new(e),
                    span,
                });
            }
            if self.at(&TokenKind::Minus) {
                self.pos += 1;
                let e = self.parse_unary()?;
                let span = join(sp, e.span().clone());
                return Ok(Expr::Unary {
                    op: UnOp::Neg,
                    expr: Box::new(e),
                    span,
                });
            }
            self.parse_postfix()
        })();
        self.depth -= 1;
        res
    }

    fn parse_postfix(&mut self) -> SpidResult<Expr> {
        let mut e = self.parse_primary()?;
        loop {
            match self.kind() {
                TokenKind::LParen => {
                    let args = self.parse_args()?;
                    let span = join(e.span().clone(), self.prev_span());
                    e = Expr::Call {
                        callee: Box::new(e),
                        args,
                        span,
                    };
                }
                TokenKind::LBracket => {
                    self.pos += 1;
                    let idx = self.parse_expr()?;
                    let end = self
                        .expect(&TokenKind::RBracket, "']'")
                        .map(|()| self.prev_span())?;
                    let span = join(e.span().clone(), end);
                    e = Expr::Index {
                        object: Box::new(e),
                        index: Box::new(idx),
                        span,
                    };
                }
                TokenKind::Dot => {
                    self.pos += 1;
                    let (name, nspan) = self.expect_ident()?;
                    if self.at(&TokenKind::LParen) {
                        let args = self.parse_args()?;
                        let span = join(e.span().clone(), self.prev_span());
                        e = Expr::Method {
                            object: Box::new(e),
                            name,
                            args,
                            span,
                        };
                    } else {
                        let span = join(e.span().clone(), nspan);
                        e = Expr::Field {
                            object: Box::new(e),
                            name,
                            span,
                        };
                    }
                }
                // `obj?.field` — null-safe field access. A method call through
                // `?.` is deliberately not accepted yet: it needs the argument
                // list to be skipped when the receiver is null, which is a
                // different lowering.
                TokenKind::QuestionDot => {
                    self.pos += 1;
                    let (name, nspan) = self.expect_ident()?;
                    if self.at(&TokenKind::LParen) {
                        return Err(self.err_here(
                            "`?.` takes a field, not a call; write `(obj?.f)(...)` \
                             or check for null first",
                        ));
                    }
                    let span = join(e.span().clone(), nspan);
                    e = Expr::OptField {
                        object: Box::new(e),
                        name,
                        span,
                    };
                }
                _ => return Ok(e),
            }
        }
    }

    fn parse_args(&mut self) -> SpidResult<Vec<Expr>> {
        self.expect(&TokenKind::LParen, "'('")?;
        let mut args = Vec::new();
        if !self.at(&TokenKind::RParen) {
            loop {
                args.push(self.parse_expr()?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen, "')'")?;
        Ok(args)
    }

    fn parse_primary(&mut self) -> SpidResult<Expr> {
        let sp = self.span();
        let kind = self.kind().clone();
        match kind {
            TokenKind::Int(v) => {
                self.pos += 1;
                Ok(Expr::Int(v, sp))
            }
            TokenKind::Float(v) => {
                self.pos += 1;
                Ok(Expr::Float(v, sp))
            }
            TokenKind::Str(s) => {
                self.pos += 1;
                Ok(Expr::Str(s, sp))
            }
            TokenKind::InterpStr(pieces) => {
                self.pos += 1;
                Ok(interp_to_expr(&pieces, sp))
            }
            // `if` as an expression: only reachable in value position, because
            // `parse_statement` consumes a leading `if` as a statement.
            TokenKind::Kw(Keyword::If) => self.parse_if_expr(),
            TokenKind::Kw(Keyword::Match) => self.parse_match_expr(),
            TokenKind::Kw(Keyword::True) => {
                self.pos += 1;
                Ok(Expr::Bool(true, sp))
            }
            TokenKind::Kw(Keyword::False) => {
                self.pos += 1;
                Ok(Expr::Bool(false, sp))
            }
            TokenKind::Kw(Keyword::Null) => {
                self.pos += 1;
                Ok(Expr::Null(sp))
            }
            TokenKind::Kw(Keyword::SelfKw) => {
                self.pos += 1;
                Ok(Expr::Ident("self".to_string(), sp))
            }
            TokenKind::Ident(name) => {
                self.pos += 1;
                Ok(Expr::Ident(name, sp))
            }
            TokenKind::Kw(Keyword::New) => {
                self.pos += 1;
                let (class, _) = self.expect_ident()?;
                let args = self.parse_args()?;
                let span = join(sp, self.prev_span());
                Ok(Expr::New { class, args, span })
            }
            TokenKind::Kw(Keyword::Fn) | TokenKind::Kw(Keyword::Async) => {
                let proto = self.parse_fn(true, None)?;
                Ok(Expr::Fn(Box::new(proto)))
            }
            TokenKind::LBracket => {
                self.pos += 1;
                let mut items = Vec::new();
                if !self.at(&TokenKind::RBracket) {
                    loop {
                        items.push(self.parse_expr()?);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                }
                let end = self
                    .expect(&TokenKind::RBracket, "']'")
                    .map(|()| self.prev_span())?;
                let span = join(sp, end);
                Ok(Expr::List(items, span))
            }
            TokenKind::LParen => {
                self.pos += 1;
                let e = self.parse_expr()?;
                self.expect(&TokenKind::RParen, "')'")?;
                Ok(e)
            }
            _ => Err(self.err_here(format!("unexpected {}", describe(&kind)))),
        }
    }
}

fn describe(k: &TokenKind) -> String {
    match k {
        TokenKind::Eof => "end of file".to_string(),
        TokenKind::Kw(kw) => format!("keyword '{}'", kw.as_str()),
        TokenKind::Ident(s) => format!("'{}'", s),
        TokenKind::Int(v) => format!("integer {}", v),
        TokenKind::Float(v) => format!("number {}", v),
        TokenKind::Str(s) => format!("string \"{}\"", s),
        TokenKind::Assign => "'='".to_string(),
        TokenKind::Semi => "';'".to_string(),
        TokenKind::RParen => "')'".to_string(),
        TokenKind::RBrace => "'}'".to_string(),
        TokenKind::RBracket => "']'".to_string(),
        TokenKind::LParen => "'('".to_string(),
        TokenKind::LBrace => "'{'".to_string(),
        other => format!("{:?}", other),
    }
}

/// Desugar a bitwise operator into a call of the stdlib native.
///
/// Keeping these as calls rather than new opcodes means one implementation
/// serves the interpreter, the VM and the AOT backend, and the type/range
/// errors come from one place.
fn bit_call(name: &str, l: Expr, r: Expr) -> Expr {
    let span = join(l.span().clone(), r.span().clone());
    Expr::Call {
        callee: Box::new(Expr::Ident(name.to_string(), span.clone())),
        args: vec![l, r],
        span,
    }
}

/// Desugar an interpolated literal into a concatenation.
///
/// `"hi {name} and {user.age}"` becomes
/// `"hi " + string(name) + " and " + string(user.age)`.
///
/// Doing it here rather than in `crate::lower` is deliberate: the rewrite needs
/// no scope information, and expanding it into ordinary `Ident`/`Field` nodes
/// means the semantic checker reports `unknown name 'name'` and the type of a
/// mistyped hole for free — no new AST node, no engine work.
///
/// Every hole is wrapped in `string(...)` so an `int` or `bool` interpolates
/// without an explicit conversion (`string` is idempotent on strings).
fn interp_to_expr(pieces: &[crate::lexer::token::InterpPiece], sp: Span) -> Expr {
    use crate::lexer::token::InterpPiece;
    let mut parts: Vec<Expr> = Vec::new();
    for piece in pieces {
        match piece {
            InterpPiece::Lit(text) => {
                if !text.is_empty() {
                    parts.push(Expr::Str(text.clone(), sp.clone()));
                }
            }
            InterpPiece::Path(segments) => {
                let mut path = Expr::Ident(segments[0].clone(), sp.clone());
                for seg in &segments[1..] {
                    path = Expr::Field {
                        object: Box::new(path),
                        name: seg.clone(),
                        span: sp.clone(),
                    };
                }
                parts.push(Expr::Call {
                    callee: Box::new(Expr::Ident("string".to_string(), sp.clone())),
                    args: vec![path],
                    span: sp.clone(),
                });
            }
        }
    }
    // An interpolated literal always has at least one hole, but stay total.
    if parts.is_empty() {
        return Expr::Str(String::new(), sp);
    }
    let mut iter = parts.into_iter();
    let mut acc = iter.next().expect("non-empty");
    for next in iter {
        acc = Expr::Binary {
            op: BinOp::Add,
            l: Box::new(acc),
            r: Box::new(next),
            span: sp.clone(),
        };
    }
    acc
}

fn join(a: Span, b: Span) -> Span {
    // Span a composite node from a's start to b's end. b's length is its own
    // token width, so b's end column is `b.col + b.len`; the composite length is
    // the distance from a's start to that end. For multi-line spans only the
    // start position is meaningful, so we keep a's own length.
    if b.line >= a.line {
        let end_col = b.col.saturating_add(b.len);
        let len = end_col.saturating_sub(a.col).max(1);
        Span::new(a.file, a.line, a.col, len)
    } else {
        Span::new(a.file, a.line, a.col, a.len.max(1))
    }
}

pub fn parse_file(src: &str, file: Arc<str>) -> SpidResult<Program> {
    let toks = crate::lexer::Lexer::new(src, file).tokenize()?;
    parse(toks)
}
