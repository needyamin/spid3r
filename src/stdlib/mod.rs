//! Standard library: console, files, time, env, process, and conversions.
//! Every call that touches the operating system is explicit; there is no
//! implicit shell and no network in v1.

use std::cell::Cell;
use std::sync::Arc;

use crate::environment::{self, Env};
use crate::sched::Scheduler;
use crate::value::collections;
use crate::value::{display_value, truthy, values_equal, NativeCtx, NativeFn, Value};

/// Signature of a native builtin.
pub type NativeDef = (
    &'static str,
    fn(&NativeCtx, Vec<Value>) -> Result<Value, String>,
);

/// All builtin natives. Shared by the tree-walk interpreter (which installs
/// into an `Env`) and the bytecode VM (which installs into a flat global map).
pub fn native_defs() -> Vec<NativeDef> {
    vec![
        ("print", n_print),
        ("println", n_println),
        ("assert", n_assert),
        ("assert_eq", n_assert_eq),
        ("weak", n_weak),
        ("deref", n_deref),
        ("eprint", n_eprint),
        ("read_line", n_read_line),
        ("read_file", n_read_file),
        ("write_file", n_write_file),
        ("file_exists", n_file_exists),
        ("sleep_ms", n_sleep_ms),
        ("now", n_now),
        ("env", n_env),
        ("args", n_args),
        ("spawn_process", n_spawn_process),
        ("yield", n_yield),
        ("task_count", n_task_count),
        ("len", n_len),
        ("abs", n_abs),
        ("sqrt", n_sqrt),
        ("pow", n_pow),
        ("min", n_min),
        ("max", n_max),
        ("int", n_int),
        ("float", n_float),
        ("string", n_string),
        ("bool", n_bool),
        ("random_int", n_random_int),
        ("json_parse", n_json_parse),
        ("json_write", n_json_write),
        ("type_of", n_type_of),
        ("gc_collect", n_gc_collect),
        ("gc_live_cells", n_gc_live_cells),
    ]
}

pub fn install(globals: &Env, sched: Arc<Scheduler>) {
    for (name, func) in native_defs() {
        let native = NativeFn { name, func };
        environment::define(globals, name, Value::Native(Arc::new(native)));
    }
    let _ = sched;
}

fn one(name: &str, args: &[Value]) -> Result<Value, String> {
    if args.len() == 1 {
        Ok(args[0].clone())
    } else {
        Err(format!("{} expects 1 argument, got {}", name, args.len()))
    }
}

fn want_num(v: &Value, who: &str) -> Result<f64, String> {
    match v {
        Value::Int(i) => Ok(*i as f64),
        Value::Float(f) => Ok(*f),
        other => Err(format!(
            "{} expects a number, found {}",
            who,
            crate::value::type_name(other)
        )),
    }
}

fn want_str(v: &Value, who: &str) -> Result<String, String> {
    match v {
        Value::Str(s) => Ok(s.to_string()),
        other => Err(format!(
            "{} expects a string, found {}",
            who,
            crate::value::type_name(other)
        )),
    }
}

fn join_display(args: &[Value]) -> String {
    args.iter()
        .map(crate::value::display_value)
        .collect::<Vec<_>>()
        .join(" ")
}

// --------------------------------------------------------------- console

fn n_print(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    print!("{}", join_display(&args));
    Ok(Value::Null)
}

fn n_println(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    println!("{}", join_display(&args));
    Ok(Value::Null)
}

/// `weak(x)` — a weak reference to a container/instance. Weak references do
/// not keep their target alive, so they are the tool for back-edges that would
/// otherwise form an uncollectable reference cycle (e.g. `node.prev`).
fn n_weak(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("weak expects 1 argument, got {}", args.len()));
    }
    crate::value::make_weak(&args[0])
}

/// `deref(w)` — the value a weak reference points at, or `null` when the
/// target has been freed. Errors on a non-weak argument.
fn n_deref(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("deref expects 1 argument, got {}", args.len()));
    }
    match crate::value::weak_upgrade(&args[0])? {
        Some(v) => Ok(v),
        None => Ok(Value::Null),
    }
}

/// `assert(cond, message?)` — throw when `cond` is falsy. The backbone of
/// project tests (`spid3r test` runs every `tests/*.matrix` and requires
/// exit 0).
fn n_assert(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    match args.len() {
        1 | 2 => {}
        n => return Err(format!("assert expects 1 or 2 arguments, got {}", n)),
    }
    if truthy(&args[0]) {
        return Ok(Value::Null);
    }
    match args.get(1) {
        Some(msg) => Err(format!("assertion failed: {}", display_value(msg))),
        None => Err("assertion failed".to_string()),
    }
}

/// `assert_eq(actual, expected, message?)` — throw when the values differ.
/// The message shows both sides, so a failing test explains itself.
fn n_assert_eq(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    match args.len() {
        2 | 3 => {}
        n => return Err(format!("assert_eq expects 2 or 3 arguments, got {}", n)),
    }
    if values_equal(&args[0], &args[1]) {
        return Ok(Value::Null);
    }
    let mut msg = format!(
        "assertion failed: {} != {}",
        display_value(&args[0]),
        display_value(&args[1])
    );
    if let Some(extra) = args.get(2) {
        msg.push_str(&format!(" — {}", display_value(extra)));
    }
    Err(msg)
}

fn n_eprint(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    // `eprint` mirrors `print`: no trailing newline (docs table lists no
    // `eprintln`; previously this appended one, breaking the symmetry).
    eprint!("{}", join_display(&args));
    Ok(Value::Null)
}

fn n_read_line(_ctx: &NativeCtx, _args: Vec<Value>) -> Result<Value, String> {
    use std::io::BufRead;
    let stdin = std::io::stdin();
    let mut line = String::new();
    match stdin.lock().read_line(&mut line) {
        Ok(0) => Ok(Value::Null),
        Ok(_) => {
            while line.ends_with('\n') || line.ends_with('\r') {
                line.pop();
            }
            Ok(Value::Str(line.into()))
        }
        Err(e) => Err(format!("read_line failed: {}", e)),
    }
}

// ------------------------------------------------------------------ files

fn n_read_file(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let path = want_str(&one("read_file", &args)?, "read_file")?;
    // Documented contract (docs/STANDARD_LIBRARY.md): file operations return
    // a `result` (`ok(text)` / `err(msg)`) instead of throwing, so callers
    // can branch on `is_ok`/`is_err` without a `try` block.
    match std::fs::read_to_string(&path) {
        Ok(s) => collections::construct("ok", vec![Value::Str(s.into())]),
        Err(e) => collections::construct(
            "err",
            vec![Value::Str(format!("read_file '{}': {}", path, e).into())],
        ),
    }
}

fn n_write_file(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!(
            "write_file expects 2 arguments, got {}",
            args.len()
        ));
    }
    let path = want_str(&args[0], "write_file")?;
    let contents = want_str(&args[1], "write_file")?;
    // Documented contract: returns `ok(null)` / `err(msg)` rather than
    // throwing or returning a bare null.
    match std::fs::write(&path, contents) {
        Ok(()) => collections::construct("ok", vec![Value::Null]),
        Err(e) => collections::construct(
            "err",
            vec![Value::Str(format!("write_file '{}': {}", path, e).into())],
        ),
    }
}

fn n_file_exists(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let path = want_str(&one("file_exists", &args)?, "file_exists")?;
    // Docs promise "true if the path exists": directories count too, not
    // only regular files.
    Ok(Value::Bool(std::path::Path::new(&path).exists()))
}

// ------------------------------------------------------------------- time

fn n_sleep_ms(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let ms = match one("sleep_ms", &args)? {
        Value::Int(i) => i,
        other => {
            return Err(format!(
                "sleep_ms expects an int, found {}",
                crate::value::type_name(&other)
            ))
        }
    };
    crate::sched::sleep_ms(ms);
    Ok(Value::Null)
}

fn n_now(_ctx: &NativeCtx, _args: Vec<Value>) -> Result<Value, String> {
    // Docs promise a MONOTONIC timestamp; wall-clock time can jump backwards
    // under NTP or a manual clock change. Report milliseconds since the
    // process's first `now()` call instead.
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(std::time::Instant::now);
    Ok(Value::Int(start.elapsed().as_millis() as i64))
}

// -------------------------------------------------------------------- env

fn n_env(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let name = want_str(&one("env", &args)?, "env")?;
    match std::env::var(&name) {
        Ok(v) => Ok(Value::Option(Some(Box::new(Value::Str(v.into()))))),
        Err(_) => Ok(Value::Option(None)),
    }
}

/// Script argument vector for `args()`: the CLI installs everything after
/// the script path, so a program sees its own arguments, not the
/// interpreter's argv. Every subcommand installs a default (empty) vector at
/// dispatch time; `run` replaces it with the real script arguments. Library/
/// REPL use without a CLI keeps the empty default.
static SCRIPT_ARGS: std::sync::RwLock<Vec<String>> = std::sync::RwLock::new(Vec::new());

pub fn set_script_args(args: Vec<String>) {
    *SCRIPT_ARGS.write().unwrap() = args;
}

fn n_args(_ctx: &NativeCtx, _args: Vec<Value>) -> Result<Value, String> {
    let items: Vec<Value> = SCRIPT_ARGS
        .read()
        .unwrap()
        .iter()
        .map(|s| Value::Str(s.clone().into()))
        .collect();
    Ok(collections::make_list(items))
}

// ---------------------------------------------------------------- process

fn n_spawn_process(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!(
            "spawn_process expects 2 arguments, got {}",
            args.len()
        ));
    }
    let cmd = want_str(&args[0], "spawn_process")?;
    let argv = match &args[1] {
        Value::List(d) | Value::VecDyn(d) => {
            let d = d.lock().unwrap();
            let mut out = Vec::new();
            for v in &d.items {
                out.push(want_str(v, "spawn_process")?);
            }
            out
        }
        other => {
            return Err(format!(
                "spawn_process expects a list of arguments, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    // Documented contract (docs/STANDARD_LIBRARY.md notes): process
    // operations that can fail return a `result` instead of throwing.
    match std::process::Command::new(&cmd).args(&argv).status() {
        Ok(status) => collections::construct(
            "ok",
            vec![Value::Int(status.code().unwrap_or(-1) as i64)],
        ),
        Err(e) => collections::construct(
            "err",
            vec![Value::Str(format!("spawn_process '{}': {}", cmd, e).into())],
        ),
    }
}

// ------------------------------------------------------------------ tasks

fn n_yield(_ctx: &NativeCtx, _args: Vec<Value>) -> Result<Value, String> {
    std::thread::yield_now();
    Ok(Value::Null)
}

fn n_task_count(ctx: &NativeCtx, _args: Vec<Value>) -> Result<Value, String> {
    Ok(Value::Int(ctx.sched.live_tasks() as i64))
}

// ----------------------------------------------------------------- numbers

fn n_len(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let v = one("len", &args)?;
    collections::len_of(&v).map(|n| Value::Int(n as i64))
}

fn n_abs(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let v = one("abs", &args)?;
    match v {
        Value::Int(i) => Ok(Value::Int(i.wrapping_abs())),
        Value::Float(f) => Ok(Value::Float(f.abs())),
        other => Err(format!(
            "abs expects a number, found {}",
            crate::value::type_name(&other)
        )),
    }
}

fn n_sqrt(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let v = one("sqrt", &args)?;
    let n = want_num(&v, "sqrt")?;
    Ok(Value::Float(n.sqrt()))
}

fn n_pow(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!("pow expects 2 arguments, got {}", args.len()));
    }
    let a = want_num(&args[0], "pow")?;
    let b = want_num(&args[1], "pow")?;
    Ok(Value::Float(a.powf(b)))
}

fn n_min(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() < 2 {
        return Err("min expects at least 2 arguments".to_string());
    }
    let mut best = args[0].clone();
    for v in &args[1..] {
        let better = match (&best, v) {
            (Value::Int(a), Value::Int(b)) => *b < *a,
            (Value::Int(a), Value::Float(b)) => *b < *a as f64,
            (Value::Float(a), Value::Int(b)) => (*b as f64) < *a,
            (Value::Float(a), Value::Float(b)) => *b < *a,
            _ => return Err("min expects numbers".to_string()),
        };
        if better {
            best = v.clone();
        }
    }
    Ok(best)
}

fn n_max(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() < 2 {
        return Err("max expects at least 2 arguments".to_string());
    }
    let mut best = args[0].clone();
    for v in &args[1..] {
        let better = match (&best, v) {
            (Value::Int(a), Value::Int(b)) => *b > *a,
            (Value::Int(a), Value::Float(b)) => *b > *a as f64,
            (Value::Float(a), Value::Int(b)) => (*b as f64) > *a,
            (Value::Float(a), Value::Float(b)) => *b > *a,
            _ => return Err("max expects numbers".to_string()),
        };
        if better {
            best = v.clone();
        }
    }
    Ok(best)
}

// ------------------------------------------------------------- conversions

fn n_int(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let v = one("int", &args)?;
    match v {
        Value::Int(i) => Ok(Value::Int(i)),
        Value::Float(f) => Ok(Value::Int(f.trunc() as i64)),
        Value::Bool(b) => Ok(Value::Int(if b { 1 } else { 0 })),
        Value::Str(s) => s
            .trim()
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| format!("int('{}'): not an integer", s)),
        other => Err(format!(
            "int expects a number, bool, or string, found {}",
            crate::value::type_name(&other)
        )),
    }
}

fn n_float(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let v = one("float", &args)?;
    match v {
        Value::Int(i) => Ok(Value::Float(i as f64)),
        Value::Float(f) => Ok(Value::Float(f)),
        Value::Bool(b) => Ok(Value::Float(if b { 1.0 } else { 0.0 })),
        Value::Str(s) => s
            .trim()
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| format!("float('{}'): not a number", s)),
        other => Err(format!(
            "float expects a number, bool, or string, found {}",
            crate::value::type_name(&other)
        )),
    }
}

fn n_string(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let v = one("string", &args)?;
    Ok(Value::Str(crate::value::display_value(&v).into()))
}

fn n_bool(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let v = one("bool", &args)?;
    Ok(Value::Bool(crate::value::truthy(&v)))
}

fn n_type_of(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let v = one("type_of", &args)?;
    Ok(Value::Str(crate::value::type_name(&v).into()))
}

/// `gc_collect()` — run one cycle-collection pass; returns the cells swept.
fn n_gc_collect(_ctx: &NativeCtx, _args: Vec<Value>) -> Result<Value, String> {
    Ok(Value::Int(crate::value::gc::collect() as i64))
}

/// `gc_live_cells()` — number of currently-live tracked container cells.
fn n_gc_live_cells(_ctx: &NativeCtx, _args: Vec<Value>) -> Result<Value, String> {
    Ok(Value::Int(crate::value::gc::live_cells() as i64))
}

// ------------------------------------------------------------------ random

thread_local! {
    /// 0 means "unseeded": the first use on this thread derives a seed from
    /// real entropy (time x stack address x thread id). The previous
    /// compile-time constant made every run of every program emit the same
    /// sequence, and every worker thread correlated with every other.
    static RNG: Cell<u64> = const { Cell::new(0) };
}

fn next_random() -> u64 {
    RNG.with(|r| {
        let mut x = r.get();
        if x == 0 {
            let stack_addr = &x as *const _ as u64;
            let tid_hash = format!("{:?}", std::thread::current().id())
                .bytes()
                .fold(0x2545F4914F6CDD1Du64, |acc, b| {
                    acc ^ (b as u64).wrapping_mul(0x100000001B3)
                });
            x = (crate::sched::now_ms() as u64)
                ^ stack_addr.wrapping_mul(0x9E3779B97F4A7C15)
                ^ tid_hash;
            if x == 0 {
                x = 0x9E3779B97F4A7C15;
            }
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        r.set(x);
        x
    })
}

fn n_random_int(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!(
            "random_int expects 2 arguments, got {}",
            args.len()
        ));
    }
    let lo = match args[0] {
        Value::Int(i) => i,
        ref other => {
            return Err(format!(
                "random_int expects ints, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    let hi = match args[1] {
        Value::Int(i) => i,
        ref other => {
            return Err(format!(
                "random_int expects ints, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    if hi < lo {
        return Err("random_int: high bound is below the low bound".to_string());
    }
    // Span math in u128: the previous `(hi - lo + 1) as u64` overflowed and
    // panicked on wide ranges (e.g. the full i64 span).
    let span: u128 = ((hi as i128) - (lo as i128) + 1) as u128;
    let r: u128 = if span <= u128::from(u64::MAX) {
        u128::from(next_random()) % span
    } else {
        // Full i64 range: the raw 64-bit value is uniform over it.
        u128::from(next_random())
    };
    Ok(Value::Int(((lo as i128) + (r as i128)) as i64))
}

// -------------------------------------------------------------------- JSON

fn n_json_parse(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let s = want_str(&one("json_parse", &args)?, "json_parse")?;
    let mut parser = JsonParser {
        chars: s.chars().collect(),
        pos: 0,
        depth: 0,
    };
    let v = parser.parse_value()?;
    parser.skip_ws();
    if parser.pos != parser.chars.len() {
        return Err("json_parse: trailing characters after the value".to_string());
    }
    Ok(v)
}

fn n_json_write(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let v = one("json_write", &args)?;
    let mut out = String::new();
    write_json(&v, &mut out, 0)?;
    Ok(Value::Str(out.into()))
}

struct JsonParser {
    chars: Vec<char>,
    pos: usize,
    depth: usize,
}

impl JsonParser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    fn skip_ws(&mut self) {
        while matches!(
            self.peek(),
            Some(' ') | Some('\t') | Some('\n') | Some('\r')
        ) {
            self.pos += 1;
        }
    }
    fn parse_value(&mut self) -> Result<Value, String> {
        // Depth guard: deeply nested input must fail with a diagnostic, not
        // overflow the host stack (the parser recurses per level).
        self.depth += 1;
        if self.depth > JSON_MAX_DEPTH {
            return Err(format!(
                "json_parse: nesting deeper than {} levels",
                JSON_MAX_DEPTH
            ));
        }
        let r = self.parse_value_inner();
        self.depth -= 1;
        r
    }

    fn parse_value_inner(&mut self) -> Result<Value, String> {
        self.skip_ws();
        match self.peek() {
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
            Some('"') => Ok(Value::Str(self.parse_string()?.into())),
            Some('t') => self.expect_word("true", Value::Bool(true)),
            Some('f') => self.expect_word("false", Value::Bool(false)),
            Some('n') => self.expect_word("null", Value::Null),
            Some(c) if c == '-' || c.is_ascii_digit() => self.parse_number(),
            other => Err(format!("json_parse: unexpected character {:?}", other)),
        }
    }
    fn expect_word(&mut self, word: &str, v: Value) -> Result<Value, String> {
        for expected in word.chars() {
            match self.peek() {
                Some(c) if c == expected => self.pos += 1,
                _ => return Err(format!("json_parse: expected '{}'", word)),
            }
        }
        Ok(v)
    }
    fn parse_number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.pos += 1;
        }
        let mut is_float = false;
        let mut has_exp = false;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.pos += 1;
            } else if c == '.' && !is_float && !has_exp {
                is_float = true;
                self.pos += 1;
                if !matches!(self.peek(), Some(d) if d.is_ascii_digit()) {
                    return Err("json_parse: bad number".to_string());
                }
            } else if (c == 'e' || c == 'E') && !has_exp && self.pos > start {
                has_exp = true;
                self.pos += 1;
                if matches!(self.peek(), Some('+') | Some('-')) {
                    self.pos += 1;
                }
                if !matches!(self.peek(), Some(d) if d.is_ascii_digit()) {
                    return Err("json_parse: bad number".to_string());
                }
            } else {
                break;
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        if is_float || has_exp {
            text.parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .map(Value::Float)
                .ok_or_else(|| "json_parse: bad number".to_string())
        } else {
            match text.parse::<i64>() {
                Ok(i) => Ok(Value::Int(i)),
                // Integer literals beyond i64 (e.g. `1e30` written as digits,
                // or values this library itself wrote as floats) fall back to
                // f64 so `json_parse(json_write(v))` round-trips.
                Err(_) => text
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .map(Value::Float)
                    .ok_or_else(|| "json_parse: bad number".to_string()),
            }
        }
    }
    fn parse_string(&mut self) -> Result<String, String> {
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Err("json_parse: unterminated string".to_string()),
                Some('"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some('\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some('n') => {
                            out.push('\n');
                            self.pos += 1;
                        }
                        Some('t') => {
                            out.push('\t');
                            self.pos += 1;
                        }
                        Some('r') => {
                            out.push('\r');
                            self.pos += 1;
                        }
                        Some('"') => {
                            out.push('"');
                            self.pos += 1;
                        }
                        Some('\\') => {
                            out.push('\\');
                            self.pos += 1;
                        }
                        Some('/') => {
                            out.push('/');
                            self.pos += 1;
                        }
                        Some('u') => {
                            // \uXXXX, combining surrogate pairs (standard JSON).
                            self.pos += 1;
                            let hi = self.parse_hex4()?;
                            let ch = if (0xD800..=0xDBFF).contains(&hi) {
                                if self.peek() == Some('\\') {
                                    self.pos += 1;
                                    if self.peek() != Some('u') {
                                        return Err("json_parse: bad surrogate pair".to_string());
                                    }
                                    self.pos += 1;
                                    let lo = self.parse_hex4()?;
                                    if !(0xDC00..=0xDFFF).contains(&lo) {
                                        return Err("json_parse: bad surrogate pair".to_string());
                                    }
                                    char::from_u32(
                                        0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00),
                                    )
                                } else {
                                    return Err("json_parse: lone high surrogate".to_string());
                                }
                            } else if (0xDC00..=0xDFFF).contains(&hi) {
                                return Err("json_parse: lone low surrogate".to_string());
                            } else {
                                char::from_u32(hi)
                            };
                            match ch {
                                Some(c) => out.push(c),
                                None => return Err("json_parse: bad \\u escape".to_string()),
                            }
                        }
                        _ => return Err("json_parse: bad escape".to_string()),
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }
    fn parse_hex4(&mut self) -> Result<u32, String> {
        let mut v: u32 = 0;
        for _ in 0..4 {
            match self.peek().and_then(|c| c.to_digit(16)) {
                Some(d) => {
                    v = v * 16 + d;
                    self.pos += 1;
                }
                None => return Err("json_parse: bad \\u escape".to_string()),
            }
        }
        Ok(v)
    }

    fn parse_array(&mut self) -> Result<Value, String> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(']') {
            self.pos += 1;
            return Ok(collections::make_list(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some(']') => {
                    self.pos += 1;
                    return Ok(collections::make_list(items));
                }
                other => return Err(format!("json_parse: unexpected {:?} in array", other)),
            }
        }
    }
    fn parse_object(&mut self) -> Result<Value, String> {
        self.pos += 1;
        let mut entries: Vec<(crate::value::MapKey, Value)> = Vec::new();
        self.skip_ws();
        if self.peek() == Some('}') {
            self.pos += 1;
            return Ok(Value::Map(crate::value::new_ref(
                crate::value::collections::seq::MapData::from_entries(entries),
            )));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            if self.peek() != Some(':') {
                return Err("json_parse: expected ':'".to_string());
            }
            self.pos += 1;
            let value = self.parse_value()?;
            let mk = crate::value::MapKey::Str(key);
            if let Some(pos) = entries.iter().position(|(k, _)| k == &mk) {
                entries[pos].1 = value;
            } else {
                entries.push((mk, value));
            }
            self.skip_ws();
            match self.peek() {
                Some(',') => self.pos += 1,
                Some('}') => {
                    self.pos += 1;
                    return Ok(Value::Map(crate::value::new_ref(
                        crate::value::collections::seq::MapData::from_entries(entries),
                    )));
                }
                other => return Err(format!("json_parse: unexpected {:?} in object", other)),
            }
        }
    }
}

/// Maximum nesting depth accepted by `json_parse` and `json_write`; deeper
/// input is a cycle or pathological nesting and gets a clean error instead
/// of a stack overflow / deadlock.
const JSON_MAX_DEPTH: usize = 512;

/// JSON string body escaper, shared by string values and map keys (keys were
/// previously written raw, so a key containing `"` produced invalid JSON).
fn write_json_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_json(v: &Value, out: &mut String, depth: usize) -> Result<(), String> {
    if depth > JSON_MAX_DEPTH {
        return Err(format!(
            "json_write: nesting deeper than {} levels (a self-referential container?)",
            JSON_MAX_DEPTH
        ));
    }
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Float(f) => {
            // `{:?}` is the shortest round-tripping form and keeps a decimal
            // point for integral values (2.0 stays a float on reparse). Rust's
            // Display emitted `NaN`/`inf`, which are not JSON.
            if f.is_finite() {
                out.push_str(&format!("{:?}", f));
            } else {
                return Err(format!(
                    "json_write: float {} cannot be written as JSON (non-finite)",
                    f
                ));
            }
        }
        Value::Str(s) => write_json_str(s, out),
        Value::List(d) | Value::VecDyn(d) => {
            // Snapshot under the lock and recurse without it: recursing while
            // holding the Mutex deadlocks on self-referential containers
            // (non-reentrant lock re-acquired on the same thread).
            let items = d.lock().unwrap().items.clone();
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(item, out, depth + 1)?;
            }
            out.push(']');
        }
        Value::Map(d) => {
            let entries = d.lock().unwrap().entries.clone();
            out.push('{');
            for (i, (k, val)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_str(&k.display(), out);
                out.push(':');
                write_json(val, out, depth + 1)?;
            }
            out.push('}');
        }
        Value::Option(o) => match o {
            Some(v) => write_json(v, out, depth + 1)?,
            None => out.push_str("null"),
        },
        other => {
            return Err(format!(
                "json_write: {} cannot be written as JSON",
                crate::value::type_name(other)
            ))
        }
    }
    Ok(())
}
