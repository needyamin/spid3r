//! Diagnostics: spans, phases, and the single error type every phase returns.

use std::fmt;
use std::sync::Arc;

/// A location in a source file. `len` is measured in bytes from `line:col`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub file: Arc<str>,
    pub line: u32,
    pub col: u32,
    pub len: u32,
}

impl Span {
    pub fn new(file: Arc<str>, line: u32, col: u32, len: u32) -> Self {
        Span {
            file,
            line,
            col,
            len,
        }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.col)
    }
}

/// Which compiler phase produced a diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Lexer,
    Parser,
    Semantic,
    Runtime,
    Vm,
    /// Bad command-line usage: exits with code 2, not 1.
    Usage,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Lexer => "lexer",
            Phase::Parser => "parser",
            Phase::Semantic => "semantic",
            Phase::Runtime => "runtime",
            Phase::Vm => "vm",
            Phase::Usage => "usage",
        }
    }
}

/// One frame of a runtime call stack, used for error traces.
#[derive(Clone, Debug)]
pub struct TraceFrame {
    /// The function's name; `None` for the top-level script.
    pub func: Option<String>,
    /// Where execution was inside that function when the error was raised.
    pub span: Span,
}

impl fmt::Display for TraceFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.func {
            Some(name) => write!(f, "{} ({})", name, self.span),
            None => write!(f, "<script> ({})", self.span),
        }
    }
}

/// A Spid3r error. Ordinary user errors are values of this type; the Rust host
/// never panics because of user code.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub phase: Phase,
    pub message: String,
    pub span: Option<Span>,
    /// The value a user `throw`ed, when the error came from user code.
    pub thrown: Option<crate::value::Value>,
    /// Runtime call stack at the point the error was raised, most recent
    /// frame first (empty for compile-time diagnostics).
    pub trace: Vec<TraceFrame>,
}

impl Diagnostic {
    pub fn new(phase: Phase, message: impl Into<String>) -> Self {
        Diagnostic {
            phase,
            message: message.into(),
            span: None,
            thrown: None,
            trace: Vec::new(),
        }
    }

    pub fn at(phase: Phase, span: Span, message: impl Into<String>) -> Self {
        Diagnostic {
            phase,
            message: message.into(),
            span: Some(span),
            thrown: None,
            trace: Vec::new(),
        }
    }

    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    /// Attach a runtime call stack (most recent frame first).
    pub fn with_trace(mut self, trace: Vec<TraceFrame>) -> Self {
        self.trace = trace;
        self
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error[{}]: {}", self.phase.as_str(), self.message)?;
        if let Some(v) = &self.thrown {
            write!(f, " (thrown: {})", crate::value::display_value(v))?;
        }
        if let Some(span) = &self.span {
            write!(f, "\n  --> {}", span)?;
        }
        if !self.trace.is_empty() {
            write!(f, "\n  stack trace (most recent call first):")?;
            for frame in &self.trace {
                write!(f, "\n    at {}", frame)?;
            }
        }
        Ok(())
    }
}

pub type SpidResult<T> = Result<T, Diagnostic>;

/// Shorthand constructors used all over the crate.
pub fn err(phase: Phase, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(phase, message)
}

pub fn err_at(phase: Phase, span: Span, message: impl Into<String>) -> Diagnostic {
    Diagnostic::at(phase, span, message)
}
