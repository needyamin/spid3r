//! Diagnostics: spans, phases, and the single error type every phase returns.

use std::fmt;
use std::sync::Arc;

/// A location in a source file. `col` and `len` are measured in **characters**
/// (not bytes), so a caret lands on the right glyph on a line containing
/// non-ASCII text.
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
    /// Native AOT compilation and link phase.
    Build,
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
            Phase::Build => "build",
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

/// How serious a diagnostic is. A warning never fails a build unless
/// `--deny-warnings` is passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Severity {
    #[default]
    Error,
    Warning,
}

/// Which lint produced a warning, so `--allow <name>` can silence one without
/// silencing the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Lint {
    #[default]
    None,
    UnusedVariable,
    UnusedParameter,
    UnusedImport,
    UnreachableCode,
}

impl Lint {
    /// The name used on the command line (`--allow unused-variable`).
    pub fn as_str(self) -> &'static str {
        match self {
            Lint::None => "",
            Lint::UnusedVariable => "unused-variable",
            Lint::UnusedParameter => "unused-parameter",
            Lint::UnusedImport => "unused-import",
            Lint::UnreachableCode => "unreachable-code",
        }
    }
}

/// A Spid3r diagnostic: an error, or a warning from a lint. Ordinary user errors
/// are values of this type; the Rust host never panics because of user code.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub phase: Phase,
    /// `Error` unless a lint produced this. It fits in the padding after
    /// `phase`, so the struct does not grow — which matters because it sits in
    /// every `Result` (see `extras`).
    pub severity: Severity,
    /// Which lint produced this, if any.
    pub lint: Lint,
    pub message: String,
    pub span: Option<Span>,
    /// The value a user `throw`ed, when the error came from user code.
    pub thrown: Option<crate::value::Value>,
    /// Runtime call stack at the point the error was raised, most recent
    /// frame first (empty for compile-time diagnostics).
    pub trace: Vec<TraceFrame>,
    /// Help and notes, boxed.
    ///
    /// `Diagnostic` sits in every `Result`, including the recursive bytecode
    /// serializer/deserializer, where its size is multiplied by the nesting
    /// depth. Two inline fields (48 bytes) pushed a 200-level case over a 2 MB
    /// test-thread stack; one boxed pointer does not.
    pub extras: Option<Box<DiagnosticExtras>>,
}

/// The optional, rarely-present parts of a [`Diagnostic`].
#[derive(Clone, Debug, Default)]
pub struct DiagnosticExtras {
    /// An actionable suggestion, rendered as `= help: …`.
    pub help: Option<String>,
    /// Extra context, rendered as `= note: …`.
    pub notes: Vec<String>,
}

impl Diagnostic {
    pub fn new(phase: Phase, message: impl Into<String>) -> Self {
        Diagnostic {
            phase,
            severity: Severity::Error,
            lint: Lint::None,
            message: message.into(),
            span: None,
            thrown: None,
            trace: Vec::new(),
            extras: None,
        }
    }

    pub fn at(phase: Phase, span: Span, message: impl Into<String>) -> Self {
        Diagnostic {
            phase,
            severity: Severity::Error,
            lint: Lint::None,
            message: message.into(),
            span: Some(span),
            thrown: None,
            trace: Vec::new(),
            extras: None,
        }
    }

    /// A lint warning: never fails the build unless the caller says so.
    pub fn warning(lint: Lint, span: Span, message: impl Into<String>) -> Self {
        Diagnostic {
            phase: Phase::Semantic,
            severity: Severity::Warning,
            lint,
            message: message.into(),
            span: Some(span),
            thrown: None,
            trace: Vec::new(),
            extras: None,
        }
    }

    pub fn is_warning(&self) -> bool {
        self.severity == Severity::Warning
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

    fn extras_mut(&mut self) -> &mut DiagnosticExtras {
        self.extras
            .get_or_insert_with(|| Box::new(DiagnosticExtras::default()))
    }

    /// Attach an actionable suggestion (`= help: …`).
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.extras_mut().help = Some(help.into());
        self
    }

    /// Attach extra context (`= note: …`).
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.extras_mut().notes.push(note.into());
        self
    }

    /// The `= help:` line, if any.
    pub fn help(&self) -> Option<&str> {
        self.extras.as_ref().and_then(|e| e.help.as_deref())
    }

    /// The `= note:` lines.
    pub fn notes(&self) -> &[String] {
        match &self.extras {
            Some(e) => &e.notes,
            None => &[],
        }
    }

    /// Render the diagnostic the way the CLI prints it: message, `-->` line,
    /// a source snippet with a caret, then any help/notes, then the trace.
    ///
    /// The snippet needs the source text. It is read from the span's file when
    /// that file can still be read (the normal case for `spid3r check`/`run`);
    /// if it cannot — an in-memory source, a deleted file, a `<test>` span — the
    /// output falls back to the plain form instead of guessing.
    pub fn render(&self) -> String {
        self.render_with(None)
    }

    /// Like [`Diagnostic::render`], with the source supplied by the caller
    /// (used by the embedding API and the WASM bridge, which have no file).
    pub fn render_with(&self, source: Option<&str>) -> String {
        let mut out = String::new();
        let kind = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        out.push_str(&format!(
            "{}[{}]: {}",
            kind,
            self.phase.as_str(),
            self.message
        ));
        if let Some(v) = &self.thrown {
            out.push_str(&format!(" (thrown: {})", crate::value::display_value(v)));
        }
        let Some(span) = &self.span else {
            self.push_footer(&mut out);
            return out;
        };
        out.push_str(&format!("\n  --> {}", span));

        let owned;
        let text = match source {
            Some(s) => Some(s),
            None => {
                owned = std::fs::read_to_string(&*span.file).ok();
                owned.as_deref()
            }
        };
        if let Some(text) = text {
            if let Some(snippet) = snippet_for(text, span) {
                out.push_str(&snippet);
            }
        }
        self.push_footer(&mut out);
        out
    }

    fn push_footer(&self, out: &mut String) {
        for n in self.notes() {
            out.push_str(&format!("\n  = note: {}", n));
        }
        if let Some(h) = self.help() {
            out.push_str(&format!("\n  = help: {}", h));
        }
        if !self.trace.is_empty() {
            out.push_str("\n  stack trace (most recent call first):");
            for frame in &self.trace {
                out.push_str(&format!("\n    at {}", frame));
            }
        }
    }

    /// One-line-per-field JSON, for editors and CI (`spid3r check --json`).
    pub fn to_json(&self) -> String {
        let span = match &self.span {
            Some(s) => format!(
                "\"file\":{},\"line\":{},\"col\":{},\"len\":{}",
                json_str(&s.file),
                s.line,
                s.col,
                s.len
            ),
            None => "\"file\":null,\"line\":null,\"col\":null,\"len\":null".to_string(),
        };
        let notes: Vec<String> = self.notes().iter().map(|n| json_str(n)).collect();
        let trace: Vec<String> = self
            .trace
            .iter()
            .map(|f| {
                format!(
                    "{{\"func\":{},\"line\":{},\"col\":{}}}",
                    match &f.func {
                        Some(n) => json_str(n),
                        None => "null".to_string(),
                    },
                    f.span.line,
                    f.span.col
                )
            })
            .collect();
        format!(
            "{{\"severity\":{},\"phase\":{},\"lint\":{},\"message\":{},{},\"help\":{},\"notes\":[{}],\"trace\":[{}]}}",
            json_str(match self.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
            }),
            json_str(self.phase.as_str()),
            match self.lint {
                Lint::None => "null".to_string(),
                l => json_str(l.as_str()),
            },
            json_str(&self.message),
            span,
            match self.help() {
                Some(h) => json_str(h),
                None => "null".to_string(),
            },
            notes.join(","),
            trace.join(",")
        )
    }
}

/// The caret block for one line of source, or `None` when the span points
/// outside the text (a synthetic span, a stale file).
fn snippet_for(text: &str, span: &Span) -> Option<String> {
    let line_no = span.line as usize;
    if line_no == 0 {
        return None;
    }
    let line = text.lines().nth(line_no - 1)?;
    // Tabs are expanded so the caret lines up with what a terminal shows.
    let shown: String = line.replace('\t', "    ");
    let col = span.col.max(1) as usize;
    // Display width of the first `col - 1` characters.
    let mut offset = 0usize;
    for (i, ch) in line.chars().enumerate() {
        if i + 1 >= col {
            break;
        }
        offset += if ch == '\t' { 4 } else { 1 };
    }
    let remaining = shown.chars().count().saturating_sub(offset);
    let caret = (span.len.max(1) as usize).min(remaining.max(1));
    let gutter = format!("{}", line_no);
    let pad = " ".repeat(gutter.len());
    Some(format!(
        "\n{pad} |\n{gutter} | {shown}\n{pad} | {}{}",
        " ".repeat(offset),
        "^".repeat(caret)
    ))
}

/// Minimal JSON string escaping (the diagnostic layer has no dependency on the
/// stdlib's JSON writer, which is for script values).
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
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
