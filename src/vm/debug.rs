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
//! | `locals` | print the current frame's slot values, by name |
//! | `print <expr>` / `p` | evaluate an expression in the paused frame |
//! | `watch <expr>` | add an expression shown at every pause |
//! | `unwatch <n>` / `unwatch *` | remove one watch, or all |
//! | `repl` | evaluate expressions in the frame until an empty line |
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
    /// Expressions re-evaluated at every pause.
    pub watches: Vec<String>,
    /// Set when the user quits.
    pub quit: bool,
}

impl DebugState {
    pub fn new(source: Vec<String>, breakpoints: HashSet<u32>, commands: Vec<String>) -> Self {
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
            watches: Vec::new(),
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

    /// Run pre-execution commands (e.g. from a script file, piped stdin, or initial prompt)
    /// until a resume command ('continue', 'step', 'next', 'finish', or 'quit') is issued.
    pub fn debug_bootstrap(&mut self) -> Result<bool, crate::diagnostics::Diagnostic> {
        let needs_prompt = match self.debug.as_ref() {
            Some(state) => state.breakpoints.is_empty(),
            None => false,
        };
        if !needs_prompt {
            return Ok(true);
        }
        loop {
            let cmd = match self.debug.as_mut().and_then(|s| s.next_command()) {
                Some(c) => c,
                None => {
                    if let Some(s) = self.debug.as_mut() {
                        s.mode = StepMode::Run;
                    }
                    return Ok(true);
                }
            };
            match self.debug_exec(&cmd).map_err(|e| e.to_diag())? {
                true => return Ok(true),
                false => continue,
            }
        }
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
        let watches = match self.debug.as_ref() {
            Some(d) if !d.watches.is_empty() => d.watches.clone(),
            _ => Vec::new(),
        };
        for (i, expr) in watches.iter().enumerate() {
            match self.debug_eval(expr) {
                Ok(v) => println!("  watch[{}] {} = {}", i, expr, display_short(&v)),
                Err(e) => println!("  watch[{}] {} = <{}>", i, expr, e),
            }
        }
    }

    /// The locals that are live at the current instruction, by name.
    ///
    /// The compiler records each local's slot and the byte range it lives in;
    /// a name that has gone out of scope is simply absent, which is what makes
    /// `print x` after the block fail instead of showing a stale slot.
    fn debug_live_locals(&self) -> Vec<(String, Value)> {
        let Some(f) = self.frames.last() else {
            return Vec::new();
        };
        let ip = f.ip;
        f.func
            .debug_locals
            .iter()
            .filter(|d| ip >= d.from && ip < d.to)
            .filter_map(|d| {
                f.slots
                    .get(d.slot as usize)
                    .map(|v| (d.name.clone(), v.clone()))
            })
            .collect()
    }

    /// Evaluate an expression against the paused frame.
    ///
    /// The tree interpreter does the work: its globals are populated with the
    /// live locals, so the expression sees exactly what the frame sees (plus
    /// the builtins). That is the "REPL in frame" - there is no second
    /// evaluator to keep in sync with the language, and no bytecode to splice
    /// into the running chunk.
    fn debug_eval(&self, src: &str) -> Result<Value, String> {
        let expr = match crate::parser::parse_expression(src, std::sync::Arc::from("<debug>")) {
            Ok(e) => e,
            Err(d) => return Err(d.message),
        };
        let interp = crate::interpreter::Interp::new(
            std::sync::Arc::from("<debug>"),
            self.module_dirs.clone(),
        );
        let env = interp.globals();
        for (name, value) in self.debug_live_locals() {
            crate::environment::define(env, &name, value);
        }
        match interp.eval(&expr, env) {
            Ok(v) => Ok(v),
            Err(crate::interpreter::Flow::Error(d)) => Err(d.message),
            Err(_) => Err("evaluation left the expression".to_string()),
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
            "p" | "print" | "eval" => {
                if arg.is_empty() {
                    println!("usage: print <expression>");
                } else {
                    match self.debug_eval(arg) {
                        Ok(v) => println!("{}", crate::value::display_value(&v)),
                        Err(e) => println!("error: {}", e),
                    }
                }
                Ok(false)
            }
            "watch" => {
                if arg.is_empty() {
                    if let Some(s) = self.debug.as_ref() {
                        if s.watches.is_empty() {
                            println!("no watches");
                        }
                        for (i, w) in s.watches.iter().enumerate() {
                            match self.debug_eval(w) {
                                Ok(v) => println!("  [{}] {} = {}", i, w, display_short(&v)),
                                Err(e) => println!("  [{}] {} = <{}>", i, w, e),
                            }
                        }
                    }
                } else {
                    // Validate now: a watch that never parses would print the
                    // same error at every pause.
                    if let Err(e) = self.debug_eval(arg) {
                        println!("watch not added: {}", e);
                        return Ok(false);
                    }
                    if let Some(s) = self.debug.as_mut() {
                        s.watches.push(arg.to_string());
                        println!("watch[{}] = {}", s.watches.len() - 1, arg);
                    }
                }
                Ok(false)
            }
            "unwatch" => {
                let n = match arg {
                    "*" | "" => {
                        if let Some(s) = self.debug.as_mut() {
                            s.watches.clear();
                        }
                        println!("all watches removed");
                        return Ok(false);
                    }
                    other => other.parse::<usize>().ok(),
                };
                match n {
                    Some(i) => {
                        let removed = self
                            .debug
                            .as_mut()
                            .filter(|s| i < s.watches.len())
                            .map(|s| s.watches.remove(i));
                        match removed {
                            Some(w) => println!("watch[{}] removed ({})", i, w),
                            None => println!("no watch[{}]", i),
                        }
                    }
                    None => println!("usage: unwatch <n> | unwatch *"),
                }
                Ok(false)
            }
            "repl" => {
                println!("evaluating in the paused frame; an empty line returns to the debugger");
                loop {
                    let line = match self.debug.as_mut().and_then(|s| s.next_command()) {
                        Some(l) => l,
                        None => return Ok(false),
                    };
                    let text = line.trim();
                    if text.is_empty() || text == ".done" || text == "done" {
                        return Ok(false);
                    }
                    // A debugger command typed at the REPL leaves the REPL and
                    // runs as a command: a scripted session cannot send a blank
                    // line (`--commands` strips them), and typing `c` there and
                    // getting "unknown name 'c'" is a trap.
                    if is_debugger_command(text.split_whitespace().next().unwrap_or("")) {
                        return self.debug_exec(text);
                    }
                    match self.debug_eval(text) {
                        Ok(v) => println!("{}", crate::value::display_value(&v)),
                        Err(e) => println!("error: {}", e),
                    }
                }
            }
            "bt" | "backtrace" | "where" => {
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
                    fatal: false,
                })
            }
            "h" | "help" | "?" => {
                println!(
                    "commands: continue(c) step(s) next(n) finish(f) break(b) <line> \
                     delete(d) <line> bt locals list print(p) <expr> watch <expr> \
                     unwatch <n|*> repl quit(q)"
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
        // Slots are: parameters, then named locals, then the operand stack.
        // The header used to say "0..N are parameters/locals" (which reads as
        // if every local were in that range) and every slot past the arity was
        // tagged "stack" — including a named `let` binding.
        let params = if arity == 0 {
            "no parameters".to_string()
        } else {
            format!("0..{} are parameters", arity - 1)
        };
        println!("{} slots ({}):", name, params);
        // Names when the compiler kept them (a source build), slot indices for
        // a `.matrixc` artifact, which carries no debug info.
        let names: std::collections::HashMap<u8, &str> = frame
            .func
            .debug_locals
            .iter()
            .map(|d| (d.slot, d.name.as_str()))
            .collect();
        for (i, v) in frame.slots.iter().enumerate() {
            let tag = if i < arity {
                "param"
            } else if names.contains_key(&(i as u8)) {
                "local"
            } else {
                "stack"
            };
            match names.get(&(i as u8)) {
                Some(n) => println!("  {} = {} ({})", n, display_short(v), tag),
                None => println!("  [{}] ({}) {}", i, tag, display_short(v)),
            }
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

/// Is this word a debugger command (as opposed to an expression to evaluate)?
fn is_debugger_command(word: &str) -> bool {
    matches!(
        word,
        "c" | "cont"
            | "continue"
            | "run"
            | "r"
            | "s"
            | "step"
            | "stepi"
            | "n"
            | "next"
            | "f"
            | "finish"
            | "b"
            | "break"
            | "d"
            | "delete"
            | "bt"
            | "backtrace"
            | "where"
            | "locals"
            | "list"
            | "l"
            | "p"
            | "print"
            | "eval"
            | "watch"
            | "unwatch"
            | "repl"
            | "q"
            | "quit"
            | "exit"
            | "h"
            | "help"
            | "?"
    )
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
