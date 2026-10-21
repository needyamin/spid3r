//! Abstract syntax tree. Every expression carries its own span so runtime
//! errors can point at source.

use crate::diagnostics::Span;

#[derive(Clone, Debug)]
pub struct Program {
    pub stmts: Vec<Stmt>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vis {
    Public,
    Private,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub ty: Option<TypeExpr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct TypeExpr {
    pub name: String,
    pub arg: Option<Box<TypeExpr>>,
    pub span: Span,
}

/// A function: named declaration, method, constructor (`name == Some("new")`),
/// or anonymous literal (`name == None`).
#[derive(Clone, Debug)]
pub struct FnProto {
    pub vis: Vis,
    pub is_async: bool,
    pub name: Option<String>,
    pub params: Vec<Param>,
    pub ret: Option<TypeExpr>,
    pub body: Block,
    pub span: Span,
}

/// A variant of an [`EnumDecl`]: a name plus its payload field names.
#[derive(Clone, Debug)]
pub struct VariantDecl {
    pub name: String,
    /// Empty for a nullary variant (`Empty()`).
    pub fields: Vec<String>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct EnumDecl {
    /// `public` marks the enum's constructors as module exports.
    pub vis: Vis,
    pub name: String,
    pub variants: Vec<VariantDecl>,
    pub span: Span,
}

/// One arm's pattern.
#[derive(Clone, Debug)]
pub enum Pattern {
    /// `_`
    Wild,
    /// `x` — matches anything and binds it.
    Bind(String),
    /// A literal: `1`, `"s"`, `true`, `null`.
    Literal(Expr),
    /// `Circle(r)` / `Empty()` — matches the tag, then the sub-patterns.
    Variant {
        name: String,
        args: Vec<Pattern>,
        span: Span,
    },
}

impl Pattern {
    pub fn span(&self) -> Span {
        match self {
            Pattern::Wild => Span::new(std::sync::Arc::from("<match>"), 0, 0, 0),
            Pattern::Bind(_) => Span::new(std::sync::Arc::from("<match>"), 0, 0, 0),
            Pattern::Literal(e) => e.span().clone(),
            Pattern::Variant { span, .. } => span.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MatchArm {
    pub pattern: Pattern,
    /// `if (cond)` — a guard; a failed guard falls through to the next arm.
    pub guard: Option<Expr>,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FieldDecl {
    pub name: String,
    pub vis: Vis,
    pub ty: Option<TypeExpr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ClassDecl {
    /// `public` marks the class as a module export; `private` is the default.
    pub vis: Vis,
    pub name: String,
    pub parent: Option<String>,
    pub interfaces: Vec<String>,
    pub fields: Vec<FieldDecl>,
    pub methods: Vec<FnProto>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct InterfaceDecl {
    /// `public` marks the interface as a module export; `private` is the default.
    pub vis: Vis,
    pub name: String,
    /// Method signatures: name, params, return type. No bodies.
    pub methods: Vec<(String, Vec<Param>, Option<TypeExpr>, Span)>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct CatchClause {
    pub name: String,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Import {
        path: String,
        alias: Option<String>,
        span: Span,
    },
    Fn(FnProto),
    Class(ClassDecl),
    Interface(InterfaceDecl),
    /// `enum Shape { Empty, Circle(radius), Rect(w, h) }`
    ///
    /// A sum type: the variants are the constructors. `crate::lower` turns the
    /// declaration into one constructor function per variant (a map with a
    /// `__tag` key), so construction is an ordinary call and needs no engine
    /// support.
    Enum(EnumDecl),
    Let {
        /// `public` marks the binding as a module export.
        vis: Vis,
        name: String,
        ty: Option<TypeExpr>,
        value: Option<Expr>,
        span: Span,
    },
    Const {
        /// `public` marks the binding as a module export.
        vis: Vis,
        name: String,
        ty: Option<TypeExpr>,
        value: Expr,
        span: Span,
    },
    If {
        cond: Expr,
        then: Block,
        elifs: Vec<(Expr, Block)>,
        els: Option<Block>,
        span: Span,
    },
    While {
        cond: Expr,
        body: Block,
        span: Span,
    },
    ForIn {
        var: String,
        iter: Expr,
        body: Block,
        span: Span,
    },
    ForC {
        init: Box<Stmt>,
        cond: Expr,
        post: Expr,
        body: Block,
        span: Span,
    },
    Return {
        value: Option<Expr>,
        span: Span,
    },
    Break {
        span: Span,
    },
    Continue {
        span: Span,
    },
    Block(Block),
    Expr(Expr),
    Spawn {
        callable: Expr,
        span: Span,
    },
    Try {
        body: Block,
        catch: Option<CatchClause>,
        finally: Option<Block>,
        span: Span,
    },
    Throw {
        value: Expr,
        span: Span,
    },
}

impl Stmt {
    pub fn span(&self) -> &Span {
        match self {
            Stmt::Import { span, .. }
            | Stmt::Let { span, .. }
            | Stmt::Const { span, .. }
            | Stmt::If { span, .. }
            | Stmt::While { span, .. }
            | Stmt::ForIn { span, .. }
            | Stmt::ForC { span, .. }
            | Stmt::Return { span, .. }
            | Stmt::Break { span }
            | Stmt::Continue { span }
            | Stmt::Spawn { span, .. }
            | Stmt::Try { span, .. }
            | Stmt::Throw { span, .. } => span,
            Stmt::Fn(f) => &f.span,
            Stmt::Class(c) => &c.span,
            Stmt::Interface(i) => &i.span,
            Stmt::Enum(e) => &e.span,
            Stmt::Block(b) => &b.span,
            Stmt::Expr(e) => e.span(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Expr {
    Int(i64, Span),
    Float(f64, Span),
    Str(String, Span),
    Bool(bool, Span),
    Null(Span),
    Ident(String, Span),
    List(Vec<Expr>, Span),
    Fn(Box<FnProto>),
    Unary {
        op: UnOp,
        expr: Box<Expr>,
        span: Span,
    },
    Binary {
        op: BinOp,
        l: Box<Expr>,
        r: Box<Expr>,
        span: Span,
    },
    Logical {
        op: LogOp,
        l: Box<Expr>,
        r: Box<Expr>,
        span: Span,
    },
    Assign {
        target: Box<Expr>,
        op: AssignOp,
        value: Box<Expr>,
        span: Span,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        span: Span,
    },
    Method {
        object: Box<Expr>,
        name: String,
        args: Vec<Expr>,
        span: Span,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
        span: Span,
    },
    Field {
        object: Box<Expr>,
        name: String,
        span: Span,
    },
    New {
        class: String,
        args: Vec<Expr>,
        span: Span,
    },
    Await(Box<Expr>, Span),
    Spawn {
        callable: Box<Expr>,
        span: Span,
    },

    // ------------------------------------------------------- surface-only nodes
    //
    // These three cannot be executed by any backend: `crate::lower` rewrites
    // them into a temp plus statements before a backend sees them. They exist as
    // AST nodes only so the checker can report real errors (an unknown field, a
    // branch with no value) against the source the user wrote.
    /// `if (c) { a } else { b }` in value position. Both branches must end with
    /// an expression; the missing `else` yields `null`.
    If {
        cond: Box<Expr>,
        then: Block,
        els: Option<Block>,
        span: Span,
    },
    /// `obj?.field` — `null` when `obj` is `null`, otherwise `obj.field`.
    OptField {
        object: Box<Expr>,
        name: String,
        span: Span,
    },
    /// `a ?? b` — `b` is evaluated only when `a` is `null`.
    Coalesce {
        l: Box<Expr>,
        r: Box<Expr>,
        span: Span,
    },
    /// `expr?` — propagate a failure. If the value is `none`/`err`, return it
    /// from the enclosing function; otherwise yield the payload.
    Propagate(Box<Expr>, Span),
    /// `match (value) { Pattern => body, … }` in value position. Arms are tried
    /// top to bottom; the first whose pattern (and guard) succeeds wins.
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
        span: Span,
    },
}

impl Expr {
    pub fn span(&self) -> &Span {
        match self {
            Expr::Int(_, s)
            | Expr::Float(_, s)
            | Expr::Str(_, s)
            | Expr::Bool(_, s)
            | Expr::Null(s)
            | Expr::Ident(_, s)
            | Expr::List(_, s)
            | Expr::Await(_, s) => s,
            Expr::Unary { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Logical { span, .. }
            | Expr::Assign { span, .. }
            | Expr::Call { span, .. }
            | Expr::Method { span, .. }
            | Expr::Index { span, .. }
            | Expr::Field { span, .. }
            | Expr::New { span, .. }
            | Expr::Spawn { span, .. }
            | Expr::If { span, .. }
            | Expr::OptField { span, .. }
            | Expr::Coalesce { span, .. }
            | Expr::Match { span, .. } => span,
            Expr::Propagate(_, span) => span,
            Expr::Fn(f) => &f.span,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogOp {
    And,
    Or,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    ModAssign,
}
