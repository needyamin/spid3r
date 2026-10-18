//! REPL: reads lines, evaluates them, keeps successful bindings, survives a
//! bad line, and supports multiline input until braces balance.

use std::io::Write;
use std::sync::Arc;

use crate::interpreter::{Flow, Interp};
use crate::lexer::Lexer;
use crate::modules;
use crate::parser;
use crate::semantic;
use crate::value::display_value;

pub fn run() {
    println!("spid3r repl - :quit to leave, :help for commands");
    let interp = Interp::new(Arc::from("<repl>"), modules::default_module_dirs());
    let env = interp.globals.clone();
    let mut buffer = String::new();
    // Names bound by earlier lines, so a later line can use them.
    let mut known: Vec<String> = Vec::new();
    loop {
        let prompt = if buffer.is_empty() { "> " } else { ". " };
        print!("{}", prompt);
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) => {
                println!();
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
        let trimmed = line.trim();
        if buffer.is_empty() {
            match trimmed {
                ":quit" | ":q" => break,
                ":help" | ":h" => {
                    println!(":quit leave   :help this text   :reset clear bindings");
                    continue;
                }
                ":reset" => {
                    known.clear();
                    println!("bindings cleared");
                    continue;
                }
                "" => continue,
                _ => {}
            }
        }
        buffer.push_str(&line);
        if !balanced(&buffer) {
            continue;
        }
        let source = std::mem::take(&mut buffer);
        let is_expr = is_bare_expr(&source);
        match eval_line(&interp, &env, &source, &mut known) {
            Ok(Some(v)) => {
                if is_expr {
                    println!("{}", display_value(&v));
                }
            }
            Ok(None) => {}
            Err(d) => {
                eprintln!("{}", d);
            }
        }
    }
}

/// True when braces, brackets, and parens balance outside strings and comments.
fn balanced(src: &str) -> bool {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if in_line_comment {
            if c == '\n' {
                in_line_comment = false;
            }
        } else if in_block_comment {
            if c == '*' && next == Some('/') {
                in_block_comment = false;
                i += 1;
            }
        } else if in_string {
            if c == '\\' {
                i += 1;
            } else if c == '"' {
                in_string = false;
            }
        } else {
            match c {
                '/' if next == Some('/') => {
                    in_line_comment = true;
                    i += 1;
                }
                '/' if next == Some('*') => {
                    in_block_comment = true;
                    i += 1;
                }
                '"' => in_string = true,
                '{' | '[' | '(' => depth += 1,
                '}' | ']' | ')' => depth -= 1,
                _ => {}
            }
        }
        i += 1;
    }
    depth <= 0 && !in_string && !in_block_comment
}

/// A REPL line is a "bare expression" when it is a single expression statement,
/// so its value gets printed.
fn is_bare_expr(src: &str) -> bool {
    let trimmed = src.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Starts with a statement keyword: not a bare expression.
    const STARTERS: &[&str] = &[
        "let ",
        "const ",
        "if ",
        "while ",
        "for ",
        "fn ",
        "class ",
        "interface ",
        "return ",
        "break",
        "continue",
        "import ",
        "try ",
        "throw ",
        "spawn ",
        "public ",
        "private ",
        "async ",
    ];
    if STARTERS.iter().any(|s| trimmed.starts_with(s)) {
        return false;
    }
    // A bare expression is echoed whether or not it ends in `;`; the trailing
    // semicolon is optional noise in the REPL.
    true
}

fn eval_line(
    interp: &Interp,
    env: &crate::environment::Env,
    src: &str,
    known: &mut Vec<String>,
) -> Result<Option<crate::value::Value>, crate::diagnostics::Diagnostic> {
    let file: Arc<str> = Arc::from("<repl>");
    let toks = Lexer::new(src, file.clone()).tokenize()?;
    let program = parser::parse(toks)?;
    semantic::check_repl(&program, known)?;
    // Evaluate on a large-stack thread, same as `run`: deep recursion must
    // hit the interpreter's depth guard, not the OS stack.
    let result = std::thread::scope(|s| {
        let handle = std::thread::Builder::new()
            .stack_size(crate::interpreter::REPL_STACK_BYTES)
            .spawn_scoped(s, || {
                let mut last = None;
                for stmt in &program.stmts {
                    match interp.exec_stmt(stmt, env) {
                        Flow::Value(v) => last = Some(v),
                        Flow::Return(v) => last = Some(v),
                        Flow::Error(d) => return Err(d),
                        _ => {}
                    }
                }
                Ok(last)
            })
            .expect("failed to start the repl evaluation thread");
        handle.join().unwrap_or_else(|_| {
            Err(crate::diagnostics::Diagnostic::new(
                crate::diagnostics::Phase::Runtime,
                "the repl thread panicked; this is a bug in spid3r",
            ))
        })
    });
    if result.is_ok() {
        // remember what this line bound, so the next line can use it
        for n in semantic::declared_names(&program) {
            if !known.contains(&n) {
                known.push(n);
            }
        }
    }
    result
}
