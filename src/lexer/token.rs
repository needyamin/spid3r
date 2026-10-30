//! Token kinds. Keywords are distinct from identifiers at the token level.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keyword {
    Fn,
    Let,
    Const,
    If,
    Elif,
    Else,
    While,
    For,
    In,
    Break,
    Continue,
    Return,
    True,
    False,
    Null,
    And,
    Or,
    Not,
    Import,
    As,
    Try,
    Catch,
    Finally,
    Throw,
    Class,
    /// `abstract class C` / `abstract fn m();`
    Abstract,
    /// `yield expr;` inside a generator function.
    Yield,
    Interface,
    New,
    SelfKw,
    Public,
    Private,
    Spawn,
    Async,
    Await,
    Extends,
    Implements,
    Match,
    Enum,
    Super,
}

impl Keyword {
    /// The source spelling of the keyword.
    pub fn as_str(self) -> &'static str {
        match self {
            Keyword::Fn => "fn",
            Keyword::Let => "let",
            Keyword::Const => "const",
            Keyword::If => "if",
            Keyword::Elif => "elif",
            Keyword::Else => "else",
            Keyword::While => "while",
            Keyword::For => "for",
            Keyword::In => "in",
            Keyword::Break => "break",
            Keyword::Continue => "continue",
            Keyword::Return => "return",
            Keyword::True => "true",
            Keyword::False => "false",
            Keyword::Null => "null",
            Keyword::And => "and",
            Keyword::Or => "or",
            Keyword::Not => "not",
            Keyword::Import => "import",
            Keyword::As => "as",
            Keyword::Try => "try",
            Keyword::Catch => "catch",
            Keyword::Finally => "finally",
            Keyword::Throw => "throw",
            Keyword::Class => "class",
            Keyword::Abstract => "abstract",
            Keyword::Yield => "yield",
            Keyword::Interface => "interface",
            Keyword::New => "new",
            Keyword::SelfKw => "self",
            Keyword::Public => "public",
            Keyword::Private => "private",
            Keyword::Spawn => "spawn",
            Keyword::Async => "async",
            Keyword::Await => "await",
            Keyword::Extends => "extends",
            Keyword::Implements => "implements",
            Keyword::Match => "match",
            Keyword::Enum => "enum",
            Keyword::Super => "super",
        }
    }

    pub fn from_ident(s: &str) -> Option<Keyword> {
        Some(match s {
            "fn" => Keyword::Fn,
            "let" => Keyword::Let,
            "const" => Keyword::Const,
            "if" => Keyword::If,
            "elif" => Keyword::Elif,
            "else" => Keyword::Else,
            "while" => Keyword::While,
            "for" => Keyword::For,
            "in" => Keyword::In,
            "break" => Keyword::Break,
            "continue" => Keyword::Continue,
            "return" => Keyword::Return,
            "true" => Keyword::True,
            "false" => Keyword::False,
            "null" => Keyword::Null,
            "and" => Keyword::And,
            "or" => Keyword::Or,
            "not" => Keyword::Not,
            "import" => Keyword::Import,
            "as" => Keyword::As,
            "try" => Keyword::Try,
            "catch" => Keyword::Catch,
            "finally" => Keyword::Finally,
            "throw" => Keyword::Throw,
            "class" => Keyword::Class,
            "abstract" => Keyword::Abstract,
            "yield" => Keyword::Yield,
            "interface" => Keyword::Interface,
            "new" => Keyword::New,
            "self" => Keyword::SelfKw,
            "public" => Keyword::Public,
            "private" => Keyword::Private,
            "spawn" => Keyword::Spawn,
            "async" => Keyword::Async,
            "await" => Keyword::Await,
            "extends" => Keyword::Extends,
            "implements" => Keyword::Implements,
            "match" => Keyword::Match,
            "enum" => Keyword::Enum,
            "super" => Keyword::Super,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    Int(i64),
    /// A big-integer literal (`123456789012345678901234567890n`): the digits,
    /// already validated as decimal. The parser desugars it into
    /// `__bigint.from_string("<digits>")` and injects the import.
    BigIntLit(String),
    Float(f64),
    Str(String),
    /// A string literal containing `{name}` interpolation holes.
    ///
    /// Only `{` + identifier path + `}` with **no whitespace** is a hole, which
    /// keeps existing text untouched: the stdlib template engine writes
    /// `"{{ user }}"`, and that stays a literal because `{` is followed by `{`
    /// or a space. There is no `{{` escape for the same reason.
    InterpStr(Vec<InterpPiece>),
    Ident(String),
    Kw(Keyword),

    Plus,
    Minus,
    Star,
    Slash,
    Percent,

    EqEq,
    BangEq,
    Lt,
    Le,
    Gt,
    Ge,

    Assign,
    /// `@` - the decorator marker (`@memo fn f() {}`).
    At,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,

    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,

    Comma,
    Colon,
    Semi,
    Dot,
    Arrow,
    /// `&` — bitwise and (desugared to `bit_and(a, b)`).
    Amp,
    /// `&&` — logical and.
    AmpAmp,
    /// `|` — bitwise or (desugared to `bit_or(a, b)`).
    Pipe,
    /// `||` — logical or.
    PipePipe,
    /// `^` — bitwise xor (desugared to `bit_xor(a, b)`).
    Caret,
    /// `<<` — shift left (desugared to `shl(a, b)`).
    Shl,
    /// `>>` — shift right (desugared to `shr(a, b)`).
    Shr,
    /// `?` on its own (reserved; `?.` and `??` are the usable forms).
    Question,
    /// `?.` — null-safe field access.
    QuestionDot,
    /// `??` — null coalescing.
    QuestionQuestion,
    /// `=>` — separates a match pattern from its body.
    FatArrow,
    /// Exact magnitude 9223372036854775808 (parsed as i64::MIN only after unary minus).
    IntMinMagnitude,

    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: crate::diagnostics::Span,
}

/// One piece of an interpolated string literal.
#[derive(Clone, Debug, PartialEq)]
pub enum InterpPiece {
    /// Literal text between holes.
    Lit(String),
    /// `{a.b.c}`: the first segment is a variable, the rest are field accesses.
    Path(Vec<String>),
    /// `{expr}`: the hole's source text, compiled as an expression by the
    /// parser (so `"{len(xs)}"` and `"{a + 1}"` work). The text is captured
    /// only when it is NOT a plain path, which keeps the simple form cheap.
    Hole(String),
}
