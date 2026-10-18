//! `spid3r debug` — a line-based debugger over the bytecode VM.
//!
//! The VM carries a source span for every instruction, so the debugger can
//! report exact locations, set breakpoints by line, and step at instruction
//! (or statement-line) granularity. It lives as a child module of `vm` so it
//! can read the frame stack directly.
//!
//! Commands (typed at the `(spid3r)` prompt, or fed from `--commands FILE`
//! for scripted/CI use):
//!
//! | command | effect |
//! |---|---|
//! | `continue` / `c` | run until the next breakpoint (or exit) |
//! | `step` / `s` | execute one instruction |
//! | `next` / `n` | run to the next source line in this frame (step over) |
//! | `finish` / `f` | run until the current frame returns |
//! | `break <line>` / `b` | add a breakpoint |
//! | `delete <line>` / `d` | remove a breakpoint |
//! | `bt` / `backtrace` / `where` | print the call stack |
//! | `locals` | print the current frame's slot values |
//! | `list` / `l` | show the source around the current line |
//! | `quit` / `q` | abort the program (exit code 3) |
//! | `help` / `h` | command summary |
//!
//! The debugger is VM-only: the tree interpreter has no instruction stream to
//! step through (use `run --engine vm`, which is also what `debug` uses).

use std::collections::HashSet;
use std::io::Write;

use crate::diagnostics::Span;
use crate::value::Value;

use super::{Vm, VmError};

/// Message used to unwind the VM when the user quits the debugger; the CLI
/// turns it into exit code 3 without printing an error.
pub const DEBUG_QUIT: &str = "debugger: program aborted by the user";

/// How the debugger should decide when to pause next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepMode {
    /// Only breakpoints pause execution.
    Run,
    /// Pause before every instruction.
    Step,
    /// Pause at the next line in a frame at or above `depth` (step over).
    Over { depth: usize },
    /// Pause when the frame stack drops below `depth` (step out).
    Finish { depth: usize },
}

/// Debugger session state, owned by the `Vm` while a debug run is active.
pub struct DebugState {
    pub breakpoints: HashSet<u32>,
    mode: StepMode,
    /// Scripted commands (empty = read from stdin, interactive).
    commands: Vec<String>,
    next_cmd: usize,
    pub interactive: bool,
    /// Last source line seen, so a breakpoint fires once per line.
    last_line: u32,
    /// Source text, for `list`.
    source: Vec<String>,
    /// Breakpoint hits, for the summary printed at the end.
    pub hits: usize,
    /// Set when the user quits.
    pub quit: bool,
}

impl DebugState {
    pub fn new(
        source: Vec<String>,
        breakpoints: HashSet<u32>,
        commands: Vec<String>,
    ) -> Self {
        let interactive = commands.is_empty();
        DebugState {
            breakpoints,
            mode: StepMode::Run,
            commands,
            next_cmd: 0,
            interactive,
            last_line: 0,
            source,
            hits: 0,
            quit: false,
        }
    }

    /// Next command: from the script queue, or stdin. `None` = EOF (treat as
    /// `continue` so a closed stdin never hangs a scripted run).
    fn next_command(&mut self) -> Option<String> {
        if self.interactive {
            let mut buf = String::new();
            print!("(spid3r) ");
            let _ = std::io::stdout().flush();
            match std::io::stdin().read_line(&mut buf) {
                Ok(0) => None,
                Ok(_) => Some(buf),
                Err(_) => None,
            }
        } else if self.next_cmd < self.commands.len() {
            let c = self.commands[self.next_cmd].clone();
            self.next_cmd += 1;
            // Echo scripted commands so transcripts read like a session.
            println!("(spid3r) {}", c.trim_end());
            Some(c)
        } else {
            None
        }
    }
}

impl Vm {
    /// Enable debugger support for the next run.
    pub fn enable_debug(&mut self, state: DebugState) {
        self.debug = Some(state);
    }

    /// True when the user quit the debugger (the CLI maps this to exit code 3).
    pub fn debug_quit(&self) -> bool {
        self.debug.as_ref().map(|d| d.quit).unwrap_or(false)
    }

    /// Breakpoint hits so far, for the end-of-run summary.
    pub fn debug_hits(&self) -> usize {
        self.debug.as_ref().map(|d| d.hits).unwrap_or(0)
    }

    /// Called before every instruction while a debug session is active.
    /// Decides whether to pause and, if so, runs the command loop.
    pub(super) fn debug_hook(&mut self) -> Result<(), VmError> {
        let Some(state) = self.debug.as_ref() else {
            return Ok(());
        };
        let (line, depth) = match self.debug_location() {
            Some(l) => l,
            None => return Ok(()),
        };
        let mode = state.mode;
        let breakpoint = state.breakpoints.contains(&line) && line != state.last_line;
        let pause = match mode {
            StepMode::Step => true,
            StepMode::Run => breakpoint,
            StepMode::Over { depth: d } => depth <= d && line != state.last_line,
            StepMode::Finish { depth: d } => depth < d,
        };
        if let Some(state) = self.debug.as_mut() {
            state.last_line = line;
        }
        if !pause {
            return Ok(());
        }
        let reason = match mode {
            StepMode::Run => {
                if let Some(s) = self.debug.as_mut() {
                    s.hits += 1;
                }
                "breakpoint"
            }
            StepMode::Step => "step",
            StepMode::Over { .. } => "next",
            StepMode::Finish { .. } => "finish",
        };
        self.debug_pause(reason)
    }

    /// The current source line and frame depth, if a span is available.
    fn debug_location(&self) -> Option<(u32, usize)> {
        let span = self.debug_span()?;
        Some((span.line, self.frames.len()))
    }

    /// Span of the instruction the innermost frame is about to execute.
    fn debug_span(&self) -> Option<Span> {
        let f = self.frames.last()?;
        Some(f.func.chunk.span_at(f.ip))
    }

    /// Print the pause location and run commands until execution resumes.
    fn debug_pause(&mut self, reason: &str) -> Result<(), VmError> {
        self.debug_report(reason);
        loop {
            let cmd = match self.debug.as_mut().and_then(|s| s.next_command()) {
                Some(c) => c,
                None => {
                    // EOF (or the script ran out of commands): continue.
                    if let Some(s) = self.debug.as_mut() {
                        s.mode = StepMode::Run;
                    }
                    return Ok(());
                }
            };
            match self.debug_exec(&cmd)? {
                true => return Ok(()), // resumed
                false => continue,     // still paused
            }
        }
    }

    /// Print the pause banner: location, function, and the source line.
    fn debug_report(&self, reason: &str) {
        let span = self.debug_span();
        let name = self
            .frames
            .last()
            .and_then(|f| f.func.name.clone())
            .unwrap_or_else(|| "<script>".to_string());
        match span {
            Some(span) => {
                println!(
                    "\n{} at {}:{}:{} in {}()",
                    reason, span.file, span.line, span.col, name
                );
                if let Some(src) = self.debug.as_ref().and_then(|d| d.source_line(span.line)) {
                    println!("  {} | {}", span.line, src);
                }
            }
            None => println!("\n{} at {} (no source location)", reason, name),
        }
    }

    /// Execute one debugger command. Returns `true` when execution should
    /// resume, `false` to stay paused at the prompt.
    fn debug_exec(&mut self, raw: &str) -> Result<bool, VmError> {
        let line = raw.trim();
        if line.is_empty() {
            // Bare enter repeats the last resume action: continue.
            return self.debug_resume(StepMode::Run);
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next().unwrap_or("").trim();
        match cmd {
            "c" | "cont" | "continue" | "run" | "r" => self.debug_resume(StepMode::Run),
            "s" | "step" | "stepi" => self.debug_resume(StepMode::Step),
            "n" | "next" => {
                let depth = self.frames.len();
                self.debug_resume(StepMode::Over { depth })
            }
            "f" | "finish" => {
                let depth = self.frames.len();
                self.debug_resume(StepMode::Finish { depth })
            }
            "b" | "break" => {
                match arg.parse::<u32>() {
                    Ok(l) if l > 0 => {
                        if let Some(s) = self.debug.as_mut() {
                            s.breakpoints.insert(l);
                        }
                        println!("breakpoint set at line {}", l);
                    }
                    _ => println!("usage: break <line>"),
                }
                Ok(false)
            }
            "d" | "delete" => {
                match arg.parse::<u32>() {
                    Ok(l) => {
                        if let Some(s) = self.debug.as_mut() {
                            s.breakpoints.remove(&l);
                        }
                        println!("breakpoint at line {} removed", l);
                    }
                    _ => println!("usage: delete <line>"),
                }
                Ok(false)
            }
            "bt" | "backtrace" | "where" | "w" => {
                self.debug_print_backtrace();
                Ok(false)
            }
            "locals" | "l" if cmd == "locals" => {
                self.debug_print_locals();
                Ok(false)
            }
            "list" | "l" => {
                self.debug_print_source();
                Ok(false)
            }
            "q" | "quit" | "exit" => {
                if let Some(s) = self.debug.as_mut() {
                    s.quit = true;
                }
                Err(VmError {
                    thrown: None,
                    message: DEBUG_QUIT.to_string(),
                    span: self.cur_span(),
                    trace: Vec::new(),
                })
            }
            "h" | "help" | "?" => {
                println!(
                    "commands: continue(c) step(s) next(n) finish(f) break(b) <line> \
                     delete(d) <line> bt locals list quit(q)"
                );
                Ok(false)
            }
            other => {
                println!("unknown command '{}' (try help)", other);
                Ok(false)
            }
        }
    }

    fn debug_resume(&mut self, mode: StepMode) -> Result<bool, VmError> {
        if let Some(s) = self.debug.as_mut() {
            s.mode = mode;
        }
        Ok(true)
    }

    /// Print the runtime call stack, most recent frame first.
    fn debug_print_backtrace(&self) {
        let trace = self.capture_trace();
        if trace.is_empty() {
            println!("no frames");
            return;
        }
        println!("stack trace (most recent call first):");
        for f in trace {
            println!("    at {}", f);
        }
    }

    /// Print the innermost frame's slot values (slot 0.. are its parameters
    /// and locals, followed by the operand stack).
    fn debug_print_locals(&self) {
        let Some(frame) = self.frames.last() else {
            println!("no frame");
            return;
        };
        let name = frame
            .func
            .name
            .clone()
            .unwrap_or_else(|| "<script>".to_string());
        let arity = frame.func.arity;
        if frame.slots.is_empty() {
            println!("{}: no slots", name);
            return;
        }
        println!("{} slots (0..{} are parameters/locals):", name, arity.saturating_sub(1));
        for (i, v) in frame.slots.iter().enumerate() {
            let tag = if i < arity { "param/local" } else { "stack" };
            println!("  [{}] ({}) {}", i, tag, display_short(v));
        }
    }

    /// Show a window of source around the current line.
    fn debug_print_source(&self) {
        let Some(span) = self.debug_span() else {
            println!("no source location");
            return;
        };
        let Some(state) = self.debug.as_ref() else {
            return;
        };
        let cur = span.line as usize;
        let lo = cur.saturating_sub(3).max(1);
        let hi = (cur + 2).min(state.source.len());
        for n in lo..=hi {
            let marker = if n == cur { "->" } else { "  " };
            let text = state.source.get(n - 1).map(|s| s.as_str()).unwrap_or("");
            println!("{} {:4} | {}", marker, n, text);
        }
    }
}

impl DebugState {
    fn source_line(&self, line: u32) -> Option<String> {
        self.source.get(line as usize - 1).cloned()
    }
}

/// Compact single-line rendering of a value for the debugger.
fn display_short(v: &Value) -> String {
    let s = crate::value::display_value(v);
    if s.chars().count() > 60 {
        let cut: String = s.chars().take(57).collect();
        format!("{}...", cut)
    } else {
        s
    }
}
