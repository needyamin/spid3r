//! Standard library: console, files, time, env, process, conversions, and
//! networking (`net`: TCP sockets + HTTP framing). Every call that touches
//! the operating system is explicit; there is no implicit shell.

pub mod argon2;
pub mod bcrypt;
pub mod compress;
pub mod csv;
pub mod datetime;
pub mod db;
pub mod digest;
pub mod entropy;
pub mod kdf;
pub mod media;
pub mod multipart;
pub mod mysql;
pub mod net;
pub mod postgres;
pub mod redis;
pub mod regex;
pub mod serialize;
pub mod shutdown;
pub mod signal;
pub mod tz;
pub mod unicode;
mod unicode_tables;

use std::cell::Cell;
use std::sync::Arc;

use crate::environment::{self, Env};
use crate::pkg::{sha256_bytes, sha256_hex};
use crate::sched::Scheduler;
use crate::value::collections;
use crate::value::{
    display_value, new_ref, truthy, values_equal, MapKey, NativeCtx, NativeFn, Value,
};

/// Signature of a native builtin.
pub type NativeDef = (
    &'static str,
    fn(&NativeCtx, Vec<Value>) -> Result<Value, String>,
);

/// All builtin natives. Shared by the tree-walk interpreter (which installs
/// into an `Env`) and the bytecode VM (which installs into a flat global map).
pub fn native_defs() -> Vec<NativeDef> {
    let mut defs: Vec<NativeDef> = vec![
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
        ("write_file_atomic", n_write_file_atomic),
        ("file_exists", n_file_exists),
        ("file_delete", n_file_delete),
        ("remove_file", n_file_delete),
        ("rename", n_rename),
        ("copy_file", n_copy_file),
        ("list_dir", n_list_dir),
        ("is_dir", n_is_dir),
        ("is_file", n_is_file),
        ("file_size", n_file_size),
        ("file_mtime", n_file_mtime),
        ("cwd", n_cwd),
        ("chdir", n_chdir),
        ("exe_path", n_exe_path),
        ("env_set", n_env_set),
        ("path_join", n_path_join),
        ("basename", n_basename),
        ("dirname", n_dirname),
        ("path_ext", n_path_ext),
        ("is_ok", n_is_ok),
        ("is_err", n_is_err),
        ("dir_create", n_dir_create),
        ("dir_remove", n_dir_remove),
        ("sleep_ms", n_sleep_ms),
        ("now", n_now),
        ("env", n_env),
        ("args", n_args),
        ("gui_args", n_args),
        ("spawn_process", n_spawn_process),
        ("run_payload", n_run_payload),
        ("run_payload_strict", n_run_payload_strict),
        ("exec_capture", n_exec_capture),
        ("yield", n_yield),
        ("task_count", n_task_count),
        ("len", n_len),
        ("abs", n_abs),
        ("sqrt", n_sqrt),
        ("pow", n_pow),
        ("min", n_min),
        ("max", n_max),
        ("clamp", n_clamp),
        ("floor", n_floor),
        ("ceil", n_ceil),
        ("round", n_round),
        ("trunc", n_trunc),
        ("sign", n_sign),
        ("exp", n_exp),
        ("ln", n_ln),
        ("log10", n_log10),
        ("log2", n_log2),
        ("sin", n_sin),
        ("cos", n_cos),
        ("tan", n_tan),
        ("asin", n_asin),
        ("acos", n_acos),
        ("atan", n_atan),
        ("is_nan", n_is_nan),
        ("is_inf", n_is_inf),
        ("atan2", n_atan2),
        ("hypot", n_hypot),
        ("float_or", n_float_or),
        ("nan", n_nan),
        ("inf", n_inf),
        ("random_float", n_random_float),
        ("random_normal", n_random_normal),
        ("sha256", n_sha256),
        ("hmac_sha256", n_hmac_sha256),
        ("pbkdf2_sha256", n_pbkdf2_sha256),
        ("secure_compare", n_secure_compare),
        ("chr", n_chr),
        ("ord", n_ord),
        ("base64_encode", n_base64_encode),
        ("base64_decode", n_base64_decode),
        ("base64_url_encode", n_base64_url_encode),
        ("base64_url_decode", n_base64_url_decode),
        ("hex_encode", n_hex_encode),
        ("hex_decode", n_hex_decode),
        ("base32_encode", n_base32_encode),
        ("base32_decode", n_base32_decode),
        ("int", n_int),
        ("float", n_float),
        ("string", n_string),
        ("bool", n_bool),
        ("bit_and", n_bit_and),
        ("bit_or", n_bit_or),
        ("bit_xor", n_bit_xor),
        ("shl", n_shl),
        ("shr", n_shr),
        ("random_int", n_random_int),
        ("json_parse", n_json_parse),
        ("json_write", n_json_write),
        ("type_of", n_type_of),
        ("is_failure", n_is_failure),
        ("gc_collect", n_gc_collect),
        ("gc_live_cells", n_gc_live_cells),
        ("read_bytes", n_read_bytes),
        ("bytes_concat", n_bytes_concat),
        ("write_bytes", n_write_bytes),
        ("save_npy", n_save_npy),
        ("load_npy", n_load_npy),
        ("read_csv", n_read_csv),
        ("parse_csv", n_parse_csv),
        ("write_csv", n_write_csv),
        ("regex_match", n_regex_match),
        ("regex_find", n_regex_find),
        ("regex_find_all", n_regex_find_all),
        ("regex_replace", n_regex_replace),
        ("unix_time", n_unix_time),
        ("now_ms", n_now_ms),
        ("elapsed_ms", n_now_ms),
        ("unix_ms", n_unix_ms),
        ("datetime_parts", n_datetime_parts),
        ("datetime_from_parts", n_datetime_from_parts),
        ("format_datetime", n_format_datetime),
    ];
    defs.extend(net::native_defs());
    defs.extend(db::native_defs());
    defs.extend(compress::native_defs());
    defs.extend(signal::native_defs());
    defs.extend(kdf::native_defs());
    defs.extend(serialize::native_defs());
    defs.extend(argon2::native_defs());
    defs.extend(bcrypt::native_defs());
    defs.extend(media::native_defs());
    defs.extend(multipart::native_defs());
    defs.extend(shutdown::native_defs());
    defs.extend(entropy::native_defs());
    defs.extend(redis::native_defs());
    defs.extend(postgres::native_defs());
    defs.extend(mysql::native_defs());
    defs.extend(unicode::native_defs());
    defs.extend(tz::native_defs());
    defs
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

fn want_int(v: &Value, who: &str) -> Result<i64, String> {
    match v {
        Value::Int(i) => Ok(*i),
        other => Err(format!(
            "{} expects an int, found {}",
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

thread_local! {
    static CAPTURE_OUTPUT: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Begin capturing stdout/stderr into an in-memory buffer (used by WASM and embedded callers).
pub fn start_capture() {
    CAPTURE_OUTPUT.with(|c| {
        *c.borrow_mut() = Some(String::new());
    });
}

/// Take the captured output string and reset the buffer.
pub fn take_capture() -> String {
    CAPTURE_OUTPUT.with(|c| c.borrow_mut().take().unwrap_or_default())
}

fn emit_output(s: &str) {
    let captured = CAPTURE_OUTPUT.with(|c| {
        if let Some(buf) = c.borrow_mut().as_mut() {
            buf.push_str(s);
            true
        } else {
            false
        }
    });
    if !captured {
        print!("{}", s);
    }
}

/// Same capture semantics as [`emit_output`], but the uncaptured destination is
/// standard error: `eprint` is documented as "print to standard error", and an
/// embedding host reads the captured buffer for both streams anyway.
fn emit_err(s: &str) {
    let captured = CAPTURE_OUTPUT.with(|c| {
        if let Some(buf) = c.borrow_mut().as_mut() {
            buf.push_str(s);
            true
        } else {
            false
        }
    });
    if !captured {
        eprint!("{}", s);
    }
}

fn n_print(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    emit_output(&join_display(&args));
    Ok(Value::Null)
}

fn n_println(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let mut s = join_display(&args);
    s.push('\n');
    emit_output(&s);
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
    emit_err(&join_display(&args));
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

/// `rename(from, to)` — atomically move/rename a file or directory, replacing
/// an existing target. POSIX `rename(2)` and Windows
/// `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` both give replace-on-rename, so
/// the durable "write to temp, then swap" pattern is one call. A missing
/// source is an `err`, never a throw.
fn n_rename(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!("rename expects 2 arguments, got {}", args.len()));
    }
    let from = want_str(&args[0], "rename")?;
    let to = want_str(&args[1], "rename")?;
    match std::fs::rename(&from, &to) {
        Ok(()) => collections::construct("ok", vec![Value::Null]),
        Err(e) => collections::construct(
            "err",
            vec![Value::Str(
                format!("rename '{}' -> '{}': {}", from, to, e).into(),
            )],
        ),
    }
}

/// `copy_file(from, to)` — copy a file, replacing an existing target. Same
/// `result` contract as the rest of the filesystem family.
fn n_copy_file(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!("copy_file expects 2 arguments, got {}", args.len()));
    }
    let from = want_str(&args[0], "copy_file")?;
    let to = want_str(&args[1], "copy_file")?;
    match std::fs::copy(&from, &to) {
        Ok(_) => collections::construct("ok", vec![Value::Null]),
        Err(e) => collections::construct(
            "err",
            vec![Value::Str(
                format!("copy_file '{}' -> '{}': {}", from, to, e).into(),
            )],
        ),
    }
}

/// `write_file_atomic(path, text)` — durable save: write to a temp file in
/// the same directory, then `rename` it over `path`. A crash or power loss
/// mid-write leaves the ORIGINAL content intact instead of a truncated or
/// empty file (plain `write_file` truncates in place).
fn n_write_file_atomic(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!(
            "write_file_atomic expects 2 arguments, got {}",
            args.len()
        ));
    }
    let path = want_str(&args[0], "write_file_atomic")?;
    let contents = want_str(&args[1], "write_file_atomic")?;
    let tmp = format!("{}.tmp", path);
    let fail = |what: String, e: std::io::Error| {
        collections::construct(
            "err",
            vec![Value::Str(format!("{} '{}': {}", what, path, e).into())],
        )
    };
    if let Err(e) = std::fs::write(&tmp, contents.as_bytes()) {
        return fail("write_file_atomic (temp write)".into(), e);
    }
    match std::fs::rename(&tmp, &path) {
        Ok(()) => collections::construct("ok", vec![Value::Null]),
        Err(e) => {
            // Best-effort cleanup of the temp file; the original error wins.
            let _ = std::fs::remove_file(&tmp);
            fail("write_file_atomic (swap)".into(), e)
        }
    }
}

// ------------------------------------------------------- directory & stat

/// `list_dir(path)` — names of a directory's entries as an `ok(list)`:
/// sorted lexicographically (deterministic across platforms and runs),
/// excluding `.` and `..`. A missing path, or one that is not a directory,
/// is an `err(msg)` — the filesystem family never throws.
fn n_list_dir(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let path = want_str(&one("list_dir", &args)?, "list_dir")?;
    let entries = match std::fs::read_dir(&path) {
        Ok(it) => it,
        Err(e) => {
            return collections::construct(
                "err",
                vec![Value::Str(format!("list_dir '{}': {}", path, e).into())],
            )
        }
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        match entry {
            Ok(e) => names.push(e.file_name().to_string_lossy().into_owned()),
            Err(e) => {
                return collections::construct(
                    "err",
                    vec![Value::Str(format!("list_dir '{}': {}", path, e).into())],
                )
            }
        }
    }
    names.sort();
    let items: Vec<Value> = names.into_iter().map(|n| Value::Str(n.into())).collect();
    collections::construct(
        "ok",
        vec![Value::List(crate::value::new_ref(
            crate::value::collections::seq::VecData { items },
        ))],
    )
}

/// `is_dir(path)` — `true` when the path exists and is a directory
/// (symlinks are followed). Every other outcome — missing path, a regular
/// file, a permission error — is `false`, never a throw: the same contract
/// as `file_exists`.
fn n_is_dir(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let path = want_str(&one("is_dir", &args)?, "is_dir")?;
    Ok(Value::Bool(std::path::Path::new(&path).is_dir()))
}

/// `is_file(path)` — `true` when the path exists and is a regular file
/// (symlinks are followed). Same false-not-throw contract as `is_dir`.
fn n_is_file(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let path = want_str(&one("is_file", &args)?, "is_file")?;
    Ok(Value::Bool(std::path::Path::new(&path).is_file()))
}

/// `file_size(path)` — size in bytes as `ok(int)`; `err(msg)` when the path
/// is missing or is a directory.
fn n_file_size(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let path = want_str(&one("file_size", &args)?, "file_size")?;
    match std::fs::metadata(&path) {
        Ok(md) => {
            if md.is_dir() {
                return collections::construct(
                    "err",
                    vec![Value::Str(
                        format!("file_size '{}': is a directory", path).into(),
                    )],
                );
            }
            let size = md.len();
            // u64 -> i64: a >8 EiB file cannot exist on any supported
            // filesystem; fail closed instead of wrapping if one ever does.
            match i64::try_from(size) {
                Ok(n) => collections::construct("ok", vec![Value::Int(n)]),
                Err(_) => collections::construct(
                    "err",
                    vec![Value::Str(
                        format!("file_size '{}': size {} exceeds i64", path, size).into(),
                    )],
                ),
            }
        }
        Err(e) => collections::construct(
            "err",
            vec![Value::Str(format!("file_size '{}': {}", path, e).into())],
        ),
    }
}

/// `file_mtime(path)` — last modification time as `ok(float)` Unix seconds
/// (fractional where the platform provides sub-second precision);
/// `err(msg)` on a missing path. Pairs with `unix_time()`.
fn n_file_mtime(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let path = want_str(&one("file_mtime", &args)?, "file_mtime")?;
    match std::fs::metadata(&path).map(|md| md.modified()) {
        Ok(Ok(t)) => {
            let secs = t
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                // A pre-epoch timestamp (clock set before 1970) reads as a
                // negative number of seconds instead of an error.
                .unwrap_or_else(|e| -(e.duration().as_secs_f64()));
            collections::construct("ok", vec![Value::Float(secs)])
        }
        Ok(Err(e)) => collections::construct(
            "err",
            vec![Value::Str(format!("file_mtime '{}': {}", path, e).into())],
        ),
        Err(e) => collections::construct(
            "err",
            vec![Value::Str(format!("file_mtime '{}': {}", path, e).into())],
        ),
    }
}

// ------------------------------------------------------ process context

/// `cwd()` — the process working directory as `ok(string)` / `err(msg)`.
///
/// Process-global: it is shared with spawned tasks and any other engine in
/// the same process (same rationale as `args()`). Script-facing file
/// natives resolve relative paths against it.
fn n_cwd(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(format!("cwd expects 0 arguments, got {}", args.len()));
    }
    match std::env::current_dir() {
        Ok(d) => collections::construct(
            "ok",
            vec![Value::Str(d.to_string_lossy().into_owned().into())],
        ),
        Err(e) => collections::construct("err", vec![Value::Str(format!("cwd: {}", e).into())]),
    }
}

/// `exe_path()` — absolute path of the running program, as `ok(string)` /
/// `err(msg)`.
///
/// A launcher needs this because relative paths resolve against the *working
/// directory*, which is not the program's own folder when it is started from a
/// shortcut, a scheduled task, a service, or `Start-Process`. Combine it with
/// `dirname()` to find the files shipped beside the executable.
fn n_exe_path(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(format!("exe_path expects 0 arguments, got {}", args.len()));
    }
    match std::env::current_exe() {
        Ok(p) => collections::construct(
            "ok",
            vec![Value::Str(p.to_string_lossy().into_owned().into())],
        ),
        Err(e) => {
            collections::construct("err", vec![Value::Str(format!("exe_path: {}", e).into())])
        }
    }
}

/// `chdir(path)` — change the process working directory. `ok(null)` on
/// success, `err(msg)` when the path is missing or is not a directory.
///
/// Process-global and NOT synchronized with concurrently running tasks:
/// library embedders running several engines in one process should treat
/// `chdir` as a startup-time call (the same discipline as `set_script_args`).
fn n_chdir(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let path = want_str(&one("chdir", &args)?, "chdir")?;
    match std::env::set_current_dir(&path) {
        Ok(()) => collections::construct("ok", vec![Value::Null]),
        Err(e) => collections::construct(
            "err",
            vec![Value::Str(format!("chdir '{}': {}", path, e).into())],
        ),
    }
}

/// `env_set(name, value)` — set a process environment variable, visible to
/// `env()` and to processes spawned afterwards. `ok(null)` on success.
///
/// Process-global (like `chdir`); host-embedders that must not touch the
/// host environment should deny the Environment capability, which stubs
/// this native along with `env` and `spawn_process`.
fn n_env_set(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!("env_set expects 2 arguments, got {}", args.len()));
    }
    let name = want_str(&args[0], "env_set")?;
    let value = want_str(&args[1], "env_set")?;
    // A name containing '=' or a NUL cannot be represented in the
    // environment block; refuse it cleanly instead of corrupting the block.
    if name.is_empty() || name.contains('=') || name.contains('\0') || value.contains('\0') {
        return collections::construct(
            "err",
            vec![Value::Str(
                "env_set: name must be non-empty without '=' or NUL; value without NUL".into(),
            )],
        );
    }
    std::env::set_var(&name, &value);
    collections::construct("ok", vec![Value::Null])
}

// ------------------------------------------------------------ path lexing

/// Both `/` and `\` are treated as component separators by the lexical path
/// helpers, on every platform: scripts written on one OS keep behaving the
/// same on the others (Windows accepts `/` in its file APIs, so a
/// `/`-normalized path stays directly usable).
fn is_sep(c: char) -> bool {
    c == '/' || c == '\\'
}

/// Split a path into components: the leading root (`/`, `\`, `C:`, `\\`...)
/// plus one entry per separator-delimited piece, with separators and empty
/// pieces dropped. `["a//b"]` and `["a", "b"]` therefore agree.
fn split_components(path: &str) -> (Option<String>, Vec<String>) {
    let chars: Vec<char> = path.chars().collect();
    if chars.is_empty() {
        return (None, Vec::new());
    }
    let mut idx = 0usize;
    let mut root: Option<String> = None;
    // UNC (`\\server\share`): keep the whole prefix as the root. The two
    // leading separators must be followed by a real component somewhere —
    // an all-separator path (`///`) is a plain root, not a UNC prefix.
    if chars.len() >= 2
        && is_sep(chars[0])
        && is_sep(chars[1])
        && chars[2..].iter().any(|&c| !is_sep(c))
    {
        let mut j = 2;
        let mut pieces = 0;
        while j < chars.len() {
            if is_sep(chars[j]) {
                pieces += 1;
                if pieces == 2 {
                    break;
                }
            }
            j += 1;
        }
        root = Some(chars[..j].iter().collect());
        idx = j;
    } else if is_sep(chars[0]) {
        root = Some(chars[0].to_string());
        idx = 1;
    } else if chars.len() >= 2 && chars[1] == ':' && chars[0].is_ascii_alphabetic() {
        // Windows drive (`C:` optionally followed by a separator).
        if chars.len() >= 3 && is_sep(chars[2]) {
            root = Some(chars[..3].iter().collect());
            idx = 3;
        } else {
            root = Some(chars[..2].iter().collect());
            idx = 2;
        }
    }
    let mut parts = Vec::new();
    let mut cur = String::new();
    for &c in &chars[idx..] {
        if is_sep(c) {
            if !cur.is_empty() {
                parts.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    (root, parts)
}

/// `path_join(a, b, ...)` — join path components with `/`. An absolute
/// later component resets the join (`path_join("a", "/b")` = `"/b"`), empty
/// components are skipped, and a trailing separator is preserved only when
/// the last non-empty component had one (`path_join("a/", "b")` = `"a/b"`).
fn n_path_join(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.is_empty() {
        return Err("path_join expects at least 1 argument, got 0".to_string());
    }
    let mut parts: Vec<String> = Vec::with_capacity(args.len());
    for a in &args {
        parts.push(want_str(a, "path_join")?.to_string());
    }
    let trailing_sep = parts
        .iter()
        .rev()
        .find(|p| !p.is_empty())
        .map(|p| p.chars().last().map(is_sep).unwrap_or(false))
        .unwrap_or(false);
    let mut out = String::new();
    let mut had_any = false;
    for p in &parts {
        if p.is_empty() {
            continue;
        }
        let (root, comps) = split_components(p);
        if let Some(r) = root {
            // An absolute component discards everything before it.
            out.clear();
            out.push_str(&r.replace('\\', "/"));
            had_any = true;
        }
        for c in comps {
            if had_any && !out.ends_with('/') {
                out.push('/');
            }
            out.push_str(&c);
            had_any = true;
        }
    }
    if trailing_sep && !out.is_empty() && !out.ends_with('/') {
        out.push('/');
    }
    Ok(Value::Str(out.into()))
}

/// `basename(path)` — the final component (`"/a/b/c"` -> `"c"`, `"a/b/"`
/// -> `"b"`); `"."` for a path that is only separators or empty.
fn n_basename(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let path = want_str(&one("basename", &args)?, "basename")?;
    let trimmed: &str = path.trim_end_matches(is_sep);
    if trimmed.is_empty() {
        // Only separators (or empty): the root itself on POSIX (`/`), `.`
        // for the empty path — `basename("")` is `.` by the same
        // convention `dirname("x")` uses.
        let root = split_components(&path).0;
        return Ok(Value::Str(match root {
            Some(r) => r.replace('\\', "/").into(),
            None => ".".into(),
        }));
    }
    match trimmed.rfind(is_sep) {
        Some(i) => Ok(Value::Str(trimmed[i + 1..].into())),
        None => Ok(Value::Str(trimmed.into())),
    }
}

/// `dirname(path)` — everything before the final component
/// (`"/a/b/c"` -> `"/a/b"`, `"c"` -> `"."`, `"/c"` -> `"/"`, `"a/b/"` ->
/// `"a"`); separators collapse to `/`.
fn n_dirname(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let path = want_str(&one("dirname", &args)?, "dirname")?;
    let trimmed: &str = path.trim_end_matches(is_sep);
    if trimmed.is_empty() {
        // Only separators (or empty): the parent of a root is the root
        // itself (POSIX `dirname /` = `/`); an empty path has no parent.
        let root = split_components(&path).0;
        return Ok(Value::Str(match root {
            Some(r) => r.replace('\\', "/").into(),
            None => ".".into(),
        }));
    }
    let (root, comps) = split_components(trimmed);
    let root_str = root.as_deref().map(|r| r.replace('\\', "/"));
    if comps.is_empty() {
        // Only a root (`/`, `C:`) or an empty path.
        return Ok(Value::Str(match root_str {
            Some(r) => r.into(),
            None => ".".into(),
        }));
    }
    if comps.len() == 1 && root_str.is_none() {
        return Ok(Value::Str(".".into()));
    }
    let parent = comps[..comps.len() - 1].join("/");
    let out = match root_str {
        // A root that already ends in a separator (`/`, `C:/`) must not
        // grow a second one before the parent path.
        Some(r) if r.ends_with('/') => format!("{}{}", r, parent),
        // A single component under a root (`/c`, `//srv/share/f`): the
        // parent is the root itself, without a dangling separator.
        Some(r) if parent.is_empty() => r,
        Some(r) => format!("{}/{}", r, parent),
        None => parent,
    };
    Ok(Value::Str(out.into()))
}

/// `path_ext(path)` — the extension after the final dot of the last
/// component, without the dot (`"a.tar.gz"` -> `"gz"`); empty when there is
/// none, and a leading dot does not count (`".gitignore"` -> `""`).
fn n_path_ext(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let path = want_str(&one("path_ext", &args)?, "path_ext")?;
    // Operate on the basename only: dots in directories are not extensions.
    let base: String = {
        let trimmed = path.trim_end_matches(is_sep);
        match trimmed.rfind(is_sep) {
            Some(i) => trimmed[i + 1..].to_string(),
            None => trimmed.to_string(),
        }
    };
    // A leading dot is part of the name, not an extension marker.
    match base.rfind('.') {
        Some(i) if i > 0 => Ok(Value::Str(base[i + 1..].into())),
        _ => Ok(Value::Str("".into())),
    }
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
    // process's first monotonic call instead. This reads the SAME process-wide
    // origin as `now_ms()`/`elapsed_ms()` (`sched::now_ms`), so the builtins
    // are directly comparable — each used to initialise its own start instant,
    // which made `now()` disagree with `now_ms()` at the same moment.
    // `now()` is NOT a clock: use `unix_time()`/`unix_ms()` for the date.
    Ok(Value::Int(crate::sched::now_ms()))
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

/// Extract a list/vec of strings. Process argument vectors must be strings:
/// a number in the list would otherwise be silently stringified differently
/// by each engine.
fn argv_list(v: &Value, who: &str) -> Result<Vec<String>, String> {
    match v {
        Value::List(d) | Value::VecDyn(d) => {
            let d = d.lock().unwrap();
            let mut out = Vec::with_capacity(d.items.len());
            for item in &d.items {
                out.push(want_str(item, who)?);
            }
            Ok(out)
        }
        other => Err(format!(
            "{} expects a list of arguments, found {}",
            who,
            crate::value::type_name(other)
        )),
    }
}

fn n_spawn_process(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 && args.len() != 3 {
        return Err(format!(
            "spawn_process expects 2 or 3 arguments, got {}",
            args.len()
        ));
    }
    let cmd = want_str(&args[0], "spawn_process")?;
    let argv = argv_list(&args[1], "spawn_process")?;
    // Optional third argument: map("hidden", true) starts a console program
    // without a console window. That is what lets a GUI-subsystem launcher
    // start a console toolchain (`spid3r gui`) with no terminal flashing up.
    let mut hidden = false;
    if args.len() == 3 {
        if let Value::Map(opts) = &args[2] {
            let key = crate::value::as_map_key(&Value::Str("hidden".into()))?;
            let found = {
                let m = opts.lock().unwrap();
                m.find(&key).map(|i| m.entries[i].1.clone())
            };
            if let Some(v) = found {
                match v {
                    Value::Bool(b) => hidden = b,
                    Value::Null => {}
                    other => {
                        return Err(format!(
                            "spawn_process: 'hidden' must be a bool, found {}",
                            crate::value::type_name(&other)
                        ));
                    }
                }
            }
        } else if !matches!(&args[2], Value::Null) {
            return Err(format!(
                "spawn_process: options must be a map or null, found {}",
                crate::value::type_name(&args[2])
            ));
        }
    }
    // Documented contract (docs/STANDARD_LIBRARY.md notes): process
    // operations that can fail return a `result` instead of throwing.
    let mut command = std::process::Command::new(&cmd);
    command.args(&argv);
    apply_spawn_flags(&mut command, hidden);
    match command.status() {
        Ok(status) => {
            collections::construct("ok", vec![Value::Int(status.code().unwrap_or(-1) as i64)])
        }
        Err(e) => collections::construct(
            "err",
            vec![Value::Str(format!("spawn_process '{}': {}", cmd, e).into())],
        ),
    }
}

/// Start the child without a console window when asked.
///
/// A console-subsystem child of a GUI process gets a *new* console allocated by
/// Windows, so a windowless launcher would still flash a terminal. Only the
/// creation flags can prevent that; there is no equivalent on Unix, where a
/// process simply inherits the parent's terminal (or none).
fn apply_spawn_flags(command: &mut std::process::Command, hidden: bool) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        if hidden {
            command.creation_flags(CREATE_NO_WINDOW);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (command, hidden);
    }
}

/// Payload entry that `spid3r build --bundle-runtime` fills with a copy of the
/// toolchain executable. Shared with the writer so the two cannot drift.
const RUNTIME_ENTRY: &str = crate::native::payload::RUNTIME_ENTRY;

/// Cache key for a runtime: the SHA-256 of its bytes, lowercase hex.
///
/// The key names the extraction directory *and* is the integrity check the
/// next launch re-computes over the file it finds there, so it has to be a
/// real hash: FNV-1a is invertible, which lets a forged executable of the same
/// length pass the check and be run. SHA-256 costs one pass over ~10 MB on a
/// cold launch and nothing on the warm path beyond re-verifying the cached
/// file.
fn runtime_key(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

/// File name of the extracted runtime.
///
/// Derived from [`RUNTIME_ENTRY`] so the launcher's reader and the file it
/// writes into the cache cannot drift apart.
fn runtime_file_name() -> &'static str {
    RUNTIME_ENTRY.rsplit('/').next().unwrap_or(RUNTIME_ENTRY)
}

/// Root of the per-user runtime cache (`.../runtime`).
///
/// `SPID3R_CACHE_DIR` overrides the location, which is what the tests use and
/// what a locked-down machine (no `LOCALAPPDATA`) can set; the fallback is the
/// system temp directory rather than an error, because a runnable app that
/// leaves a cache behind beats an app that refuses to start.
fn runtime_cache_root() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("SPID3R_CACHE_DIR") {
        if !dir.is_empty() {
            return std::path::PathBuf::from(dir).join("runtime");
        }
    }
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from)
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache"))
            })
    };
    match base {
        Some(base) => base
            .join(if cfg!(windows) { "Spid3r" } else { "spid3r" })
            .join("runtime"),
        None => std::env::temp_dir().join("spid3r-runtime"),
    }
}

/// What the cache currently holds for the runtime we carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheState {
    /// Present, readable, and byte-identical to the runtime we carry.
    Intact,
    /// Nothing at that path — a first launch, or a cache that was cleared.
    Missing,
    /// Present but not readable (an antivirus scanner holding the file, a
    /// sharing violation). Not evidence of tampering, so the file must not be
    /// deleted.
    Unreadable,
    /// Readable but not the runtime we carry: truncated, or a same-length
    /// impostor planted by another process. Replace it.
    Mismatch,
}

/// Classify the file at `exe` against the runtime bytes we would extract.
///
/// The directory name is the content hash, so a same-size file is not enough:
/// anything that can write into the user's cache (another process running as
/// the same user, a leftover from an interrupted write, a tampered copy) would
/// otherwise be executed as the runtime. Hashing the candidate closes that.
///
/// A read error is reported separately from a hash mismatch: treating "I could
/// not open it" as "it is wrong" makes a transient antivirus lock destroy a
/// perfectly good runtime.
fn cached_runtime_state(exe: &std::path::Path, data: &[u8], key: &str) -> CacheState {
    match std::fs::read(exe) {
        Ok(bytes) => {
            if bytes.len() == data.len() && sha256_hex(&bytes) == key {
                CacheState::Intact
            } else {
                CacheState::Mismatch
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => CacheState::Missing,
        Err(_) => CacheState::Unreadable,
    }
}

/// Distinguishes temporary files written by concurrent extractions in one
/// process (two payloads carrying identical bytes).
static EXTRACT_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Write the runtime bytes to `tmp`, executable on Unix.
fn write_tmp_runtime(tmp: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(tmp, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Extract a copy into a directory of its own, for when the cache entry exists
/// but cannot be read.
///
/// There are two wrong answers here and this is the third. Running the
/// unreadable file trusts it without checking anything — and "unreadable" is
/// something an attacker with write access to the cache can arrange (deny
/// read, allow execute), which would bypass the hash completely. Deleting it
/// turns a scanner's temporary lock into a failed launch.
///
/// So the unreadable entry is left untouched and the bytes we actually carry
/// are unpacked beside it, into a directory whose name no other process can
/// guess. Whatever runs is still the runtime out of our own payload.
fn extract_alongside(
    root: &std::path::Path,
    key: &str,
    unreadable: &std::path::Path,
    data: &[u8],
) -> Result<(std::path::PathBuf, bool), String> {
    let dir = root.join(format!(
        "{}-{}-{}",
        key,
        std::process::id(),
        EXTRACT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let exe = dir.join(runtime_file_name());
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("run_payload: cannot create '{}': {}", dir.display(), e))?;
    let tmp = dir.join(format!(".spid3r-{}.tmp", std::process::id()));
    if let Err(e) = write_tmp_runtime(&tmp, data) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "run_payload: cannot write '{}': {}",
            tmp.display(),
            e
        ));
    }
    if let Err(e) = std::fs::rename(&tmp, &exe) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "run_payload: cannot install '{}': {}",
            exe.display(),
            e
        ));
    }
    crate::logging::record(
        "run_payload",
        &format!(
            "cannot verify '{}'; extracted a fresh copy to '{}'",
            unreadable.display(),
            exe.display()
        ),
    );
    prune_runtime_cache(root, key);
    Ok((exe, true))
}

/// Extract an embedded runtime to the per-user cache, returning its path and
/// whether *this* call is what put it there.
///
/// Concurrent launches are the normal case (a user double-clicks twice, or the
/// app is started from a shortcut and a file association at once), so the write
/// is atomic: the bytes go to a unique temporary file in the same directory and
/// are then renamed into place. A second process either sees a valid file and
/// reuses it, or loses the rename race and verifies the winner's copy — it
/// never reads or executes a half-written executable.
fn cache_runtime(data: &[u8]) -> Result<(std::path::PathBuf, bool), String> {
    let key = runtime_key(data);
    let root = runtime_cache_root();
    let dir = root.join(&key);
    let exe = dir.join(runtime_file_name());

    match cached_runtime_state(&exe, data, &key) {
        CacheState::Intact => return Ok((exe, false)),
        // Not "wrong", but not verifiable either: never run it as it is.
        CacheState::Unreadable => return extract_alongside(&root, &key, &exe, data),
        CacheState::Missing | CacheState::Mismatch => {}
    }
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("run_payload: cannot create '{}': {}", dir.display(), e))?;

    let tmp = dir.join(format!(
        ".spid3r-{}-{}.tmp",
        std::process::id(),
        EXTRACT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    if let Err(e) = write_tmp_runtime(&tmp, data) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "run_payload: cannot write '{}': {}",
            tmp.display(),
            e
        ));
    }

    match std::fs::rename(&tmp, &exe) {
        Ok(()) => {}
        Err(first) => {
            // The rename replaces an existing file on Windows too
            // (`MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`); what it cannot
            // do is replace an image that is currently running, which fails
            // with a sharing violation. So look again at what is on disk
            // before deciding what to do with our temporary file.
            match cached_runtime_state(&exe, data, &key) {
                // A concurrent launch won the race: keep theirs, drop ours.
                CacheState::Intact => {
                    let _ = std::fs::remove_file(&tmp);
                }
                // Same rule as above: an unverifiable file is not run.
                CacheState::Unreadable => {
                    let _ = std::fs::remove_file(&tmp);
                    return extract_alongside(&root, &key, &exe, data);
                }
                // Not ours (tampered or truncated): replace it, and refuse to
                // run anything if that is impossible.
                CacheState::Missing | CacheState::Mismatch => {
                    let _ = std::fs::remove_file(&exe);
                    if let Err(e) = std::fs::rename(&tmp, &exe) {
                        let _ = std::fs::remove_file(&tmp);
                        return Err(format!(
                            "run_payload: cannot install '{}': {} (rename failed: {})",
                            exe.display(),
                            e,
                            first
                        ));
                    }
                }
            }
        }
    }

    prune_runtime_cache(&root, &key);
    Ok((exe, true))
}

/// Drop runtimes left by earlier builds: keep the current one plus the two most
/// recent others (a previously launched app may still be running from one).
///
/// Best effort — a directory in use cannot be removed on Windows, and that is
/// fine: the next launch tries again.
fn prune_runtime_cache(root: &std::path::Path, keep: &str) {
    const KEEP_EXTRA: usize = 2;
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut others: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == keep {
            // Stale temporary files from a crashed extraction. Only old ones:
            // another launch may be writing its own temporary file right now,
            // and deleting that would fail its install.
            if let Ok(files) = std::fs::read_dir(&path) {
                for f in files.flatten() {
                    let p = f.path();
                    let stale_name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().ends_with(".tmp"))
                        .unwrap_or(false);
                    let old = f
                        .metadata()
                        .and_then(|m| m.modified())
                        .map(|t| t.elapsed().map(|age| age.as_secs() > 3600).unwrap_or(false))
                        .unwrap_or(false);
                    if stale_name && old {
                        let _ = std::fs::remove_file(&p);
                    }
                }
            }
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        others.push((modified, path));
    }
    if others.len() <= KEEP_EXTRA {
        return;
    }
    // Newest first: keep the two most recent, drop the rest.
    others.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, path) in others.into_iter().skip(KEEP_EXTRA) {
        let _ = std::fs::remove_dir_all(&path);
    }
}

/// The installed toolchain: `%SPID3R_EXE%`, then the per-user install, then
/// whatever `spid3r` resolves to on `PATH`.
fn installed_runtime() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("SPID3R_EXE") {
        let p = std::path::PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let p = std::path::PathBuf::from(local)
                .join("Programs")
                .join("Spid3r")
                .join("spid3r.exe");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    which_on_path("spid3r")
}

/// Look up `name` on `PATH`, returning the first match.
///
/// `src/native/link.rs` has its own copy for locating the linker, but that
/// module only exists with the `native` feature while stdlib compiles in every
/// build — so the lookup has to live here too.
fn which_on_path(name: &str) -> Option<std::path::PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".to_string())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        for ext in &exts {
            let candidate = dir.join(format!("{}{}", name, ext));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Which runtime `exe` should start, and whether this call extracted it.
///
/// Split out of [`n_run_payload`] because this is the security-relevant
/// decision and it must never start anything: the only `Command::status()` in
/// the launcher sits *after* a successful return from here, so a corrupt
/// payload cannot reach a child process at all.
///
/// Three outcomes, and the difference between the first two is the whole
/// point:
///
/// - `Ok(None)` from the payload reader — the executable carries no payload.
///   That is the documented loose/dev case, and falling back to an installed
///   toolchain is correct.
/// - `Err(..)` — a payload *is* there and cannot be parsed (truncated trailer,
///   corrupted body). Falling back would hand execution to whatever
///   `spid3r.exe` is on `%SPID3R_EXE%`, in `%LOCALAPPDATA%\Programs\Spid3r` or
///   on `PATH`, so truncating the trailer of an app would redirect it to an
///   attacker-controlled binary. Hard error instead.
/// - a runtime entry — extract it to the cache and use it.
fn resolve_runtime(
    exe: &std::path::Path,
    strict: bool,
) -> Result<(std::path::PathBuf, bool), String> {
    // Only the runtime entry is copied: the rest of the payload (assets,
    // bytecode) is the GUI side's business and can be several megabytes.
    let embedded = match crate::native::payload::read_filtered(exe, |name| name == RUNTIME_ENTRY) {
        Ok(entries) => entries.and_then(|e| e.into_iter().next().map(|(_, data)| data)),
        Err(e) => {
            let msg = format!(
                "run_payload: the embedded payload of '{}' is corrupt ({}); rebuild the app \
                 with `spid3r build` — refusing to fall back to an installed runtime",
                exe.display(),
                e
            );
            crate::logging::record("run_payload", &msg);
            return Err(msg);
        }
    };
    match embedded {
        Some(data) => cache_runtime(&data).inspect_err(|e| {
            crate::logging::record("run_payload", e);
        }),
        None => {
            if strict {
                let msg = "run_payload: this app was built to carry its own runtime, but its \
                           payload is missing or truncated; rebuild it with `spid3r build`"
                    .to_string();
                crate::logging::record("run_payload", &msg);
                return Err(msg);
            }
            let runtime = installed_runtime().ok_or_else(|| {
                let msg = "run_payload: no GUI runtime found — this app carries none of its own. \
             Install Spid3r (on PATH or in %LOCALAPPDATA%\\Programs\\Spid3r), or rebuild \
             the app with `spid3r build --bundle-runtime` to embed one."
                    .to_string();
                crate::logging::record("run_payload", &msg);
                msg
            })?;
            Ok((runtime, false))
        }
    }
}

/// `run_payload()` — hand off from an AOT-built launcher to the real GUI
/// runtime, returning the child's exit code.
///
/// The AOT backend does not link the WebView bridge, so a built app cannot open
/// its own window. This native replaces the per-app launcher code that used to
/// search for an installed `spid3r`: it first uses the runtime carried inside
/// the executable itself (see `spid3r build --bundle-runtime`), extracting it to
/// the per-user cache, and only then falls back to an installed toolchain.
///
/// The working directory is set to the executable's own folder before spawning,
/// because the app's data files sit beside it and relative paths would
/// otherwise resolve against whatever directory the shell started us in.
fn n_run_payload(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    run_payload_impl(args, false)
}

/// `run_payload_strict()` — [`n_run_payload`] for a build that is *supposed* to
/// carry its own runtime.
///
/// `payload::body_range` returns `Ok(None)` when the trailer magic is absent, so
/// deleting the last 24 bytes of a packaged exe makes the payload vanish
/// cleanly. The lenient `run_payload` then falls back to `%SPID3R_EXE%`, the
/// per-user install or `PATH` — and a same-user process can strip those bytes to
/// redirect the launch to a binary of its choosing. The generated launcher stub
/// (`spid3r build --bytecode`) calls this variant instead, where a missing
/// payload is fatal. A hand-written `--embed`-only launcher keeps the lenient
/// form, because falling back to an installed Spid3r is legitimate for it.
fn n_run_payload_strict(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    run_payload_impl(args, true)
}

/// Shared body of `run_payload` / `run_payload_strict`.
fn run_payload_impl(args: Vec<Value>, strict: bool) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(format!(
            "run_payload expects 0 arguments, got {}",
            args.len()
        ));
    }
    let self_exe = std::env::current_exe().map_err(|e| {
        let msg = format!("run_payload: cannot locate this executable: {}", e);
        crate::logging::record("run_payload", &msg);
        msg
    })?;

    let (runtime, freshly_extracted) = resolve_runtime(&self_exe, strict)?;

    // Steady-state launches are silent: only a launch that actually unpacked
    // a runtime (first run after a rebuild, or a repaired cache) is news.
    if freshly_extracted {
        crate::logging::record(
            "run_payload",
            &format!(
                "launch {} (extracted runtime {} from the payload)",
                self_exe.display(),
                runtime.display()
            ),
        );
    }
    if std::env::var_os("SPID3R_RUN_PAYLOAD_DEBUG").is_some() {
        eprintln!(
            "run_payload: {} --payload {}",
            runtime.display(),
            self_exe.display()
        );
    }

    if let Some(dir) = self_exe.parent() {
        std::env::set_current_dir(dir).map_err(|e| {
            let msg = format!("run_payload: cannot enter '{}': {}", dir.display(), e);
            crate::logging::record("run_payload", &msg);
            msg
        })?;
    }

    // Hidden: a launcher built with `--subsystem windows` owns no console, and
    // a console child would make Windows allocate a fresh console window.
    let mut command = std::process::Command::new(&runtime);
    command.arg("gui").arg("--payload").arg(&self_exe);
    apply_spawn_flags(&mut command, true);
    let status = command.status().map_err(|e| {
        let msg = format!("run_payload: cannot start '{}': {}", runtime.display(), e);
        crate::logging::record("run_payload", &msg);
        msg
    })?;
    if !status.success() {
        crate::logging::record(
            "run_payload",
            &format!(
                "runtime exited with {} for '{}'",
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "no exit code".to_string()),
                self_exe.display()
            ),
        );
    }
    Ok(Value::Int(status.code().unwrap_or(-1) as i64))
}

/// Hard cap on the bytes read from a child's stdout or stderr. A program that
/// talks to a noisy (or hostile) child must not be able to exhaust memory, so
/// the streams are truncated at this bound rather than read to EOF.
const EXEC_CAPTURE_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Run a child process with all three pipes connected and collect its output.
///
/// The child is spawned with piped stdin/stdout/stderr. Writing stdin on this
/// thread while reading stdout here would deadlock the moment the child filled
/// a pipe buffer, so stdin is consumed and both output pipes are drained on
/// worker threads before the child is reaped.
fn run_capture(
    cmd: &str,
    argv: &[String],
    stdin_text: Option<&str>,
) -> Result<(i64, String, String), String> {
    let mut child = match std::process::Command::new(cmd)
        .args(argv)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Err(format!("exec_capture '{}': {}", cmd, e)),
    };

    // Dropping the handle closes the child's stdin, which is how a program
    // like `cat` learns the input is finished.
    let stdin_writer = match (child.stdin.take(), stdin_text) {
        (Some(mut stdin), Some(text)) => {
            let text = text.to_string();
            Some(std::thread::spawn(move || {
                use std::io::Write;
                let _ = stdin.write_all(text.as_bytes());
                let _ = stdin.flush();
            }))
        }
        (Some(_), None) => None,
        (None, Some(_)) => None,
        (None, None) => None,
    };

    let stdout_reader = child.stdout.take().map(|out| {
        std::thread::spawn(move || read_pipe_capped(out).map_err(|e| format!("stdout: {}", e)))
    });
    let stderr_reader = child.stderr.take().map(|out| {
        std::thread::spawn(move || read_pipe_capped(out).map_err(|e| format!("stderr: {}", e)))
    });

    let stdout = join_reader(stdout_reader);
    let stderr = join_reader(stderr_reader);
    if let Some(h) = stdin_writer {
        let _ = h.join();
    }
    // Reap the child no matter what: skipping wait() leaves a zombie, and a
    // read error must not leak one either.
    let waited = child.wait();
    let stdout = stdout?;
    let stderr = stderr?;
    match waited {
        Ok(status) => Ok((status.code().unwrap_or(-1) as i64, stdout, stderr)),
        Err(e) => Err(format!("exec_capture '{}': waiting failed: {}", cmd, e)),
    }
}

/// Join a reader thread, turning a panic inside it into a clean error string.
fn join_reader(
    h: Option<std::thread::JoinHandle<Result<String, String>>>,
) -> Result<String, String> {
    match h {
        Some(h) => match h.join() {
            Ok(r) => r,
            Err(_) => Err("exec_capture: child output reader failed".to_string()),
        },
        None => Ok(String::new()),
    }
}

/// Read a child pipe up to `EXEC_CAPTURE_MAX_BYTES` and decode it as UTF-8.
/// Truncation is silent (documented); non-UTF-8 output is an error, because
/// lossy decoding would hand the program different bytes than the child wrote.
fn read_pipe_capped<R: std::io::Read>(r: R) -> Result<String, String> {
    use std::io::Read;
    let mut buf = Vec::new();
    let mut limited = r.take(EXEC_CAPTURE_MAX_BYTES);
    limited
        .read_to_end(&mut buf)
        .map_err(|e| format!("exec_capture: reading child output failed: {}", e))?;
    String::from_utf8(buf).map_err(|_| "exec_capture: child output is not valid UTF-8".to_string())
}

/// `exec_capture(cmd, args, stdin_text) -> result` — run a process and capture
/// its output. `stdin_text` is written to the child's stdin before waiting
/// (`null` closes stdin immediately). On success the payload is a map with
/// `"code"` (int exit status), `"stdout"` and `"stderr"` (strings, each capped
/// at 64 MiB). A process that cannot be spawned — or that writes output which
/// is not valid UTF-8 — yields `err(msg)` instead of throwing.
fn n_exec_capture(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(format!(
            "exec_capture expects 3 arguments, got {}",
            args.len()
        ));
    }
    let cmd = want_str(&args[0], "exec_capture")?;
    let argv = argv_list(&args[1], "exec_capture")?;
    let stdin_text = match &args[2] {
        Value::Null => None,
        Value::Str(s) => Some(s.to_string()),
        other => {
            return Err(format!(
                "exec_capture expects a string or null for stdin, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    // Documented contract: a failed spawn is a `result`, not a throw, so
    // scripts can branch on it without a try block.
    match run_capture(&cmd, &argv, stdin_text.as_deref()) {
        Ok((code, stdout, stderr)) => collections::construct(
            "ok",
            vec![collections::construct(
                "map",
                vec![
                    Value::Str("code".into()),
                    Value::Int(code),
                    Value::Str("stdout".into()),
                    Value::Str(stdout.into()),
                    Value::Str("stderr".into()),
                    Value::Str(stderr.into()),
                ],
            )?],
        ),
        Err(msg) => collections::construct("err", vec![Value::Str(msg.into())]),
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
        Value::Int(i) => i
            .checked_abs()
            .map(Value::Int)
            .ok_or_else(|| format!("integer overflow: abs({})", i)),
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

// ------------------------------------------------- scalar math & utility

/// One-argument float builtin. Each of the 17 scalar functions is a thin
/// wrapper so the builtin's own name appears in arity/type diagnostics; a
/// macro keeps the wrappers from being copy-paste boilerplate.
macro_rules! math1 {
    ($fname:ident, $builtin:literal, $body:expr) => {
        fn $fname(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
            let x = want_num(&one($builtin, &args)?, $builtin)?;
            Ok($body(x))
        }
    };
}

math1!(n_floor, "floor", |x: f64| Value::Float(x.floor()));
math1!(n_ceil, "ceil", |x: f64| Value::Float(x.ceil()));
math1!(n_round, "round", |x: f64| Value::Float(x.round()));
math1!(n_trunc, "trunc", |x: f64| Value::Float(x.trunc()));
math1!(n_sign, "sign", |x: f64| Value::Float(if x.is_nan() {
    f64::NAN
} else if x > 0.0 {
    1.0
} else if x < 0.0 {
    -1.0
} else {
    x // keeps -0.0 negative and 0.0 positive
}));
math1!(n_exp, "exp", |x: f64| Value::Float(x.exp()));
math1!(n_ln, "ln", |x: f64| Value::Float(x.ln()));
math1!(n_log10, "log10", |x: f64| Value::Float(x.log10()));
math1!(n_log2, "log2", |x: f64| Value::Float(x.log2()));
math1!(n_sin, "sin", |x: f64| Value::Float(x.sin()));
math1!(n_cos, "cos", |x: f64| Value::Float(x.cos()));
math1!(n_tan, "tan", |x: f64| Value::Float(x.tan()));
math1!(n_asin, "asin", |x: f64| Value::Float(x.asin()));
math1!(n_acos, "acos", |x: f64| Value::Float(x.acos()));
math1!(n_atan, "atan", |x: f64| Value::Float(x.atan()));
math1!(n_is_nan, "is_nan", |x: f64| Value::Bool(x.is_nan()));
math1!(n_is_inf, "is_inf", |x: f64| Value::Bool(x.is_infinite()));

fn n_atan2(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!("atan2 expects 2 arguments, got {}", args.len()));
    }
    // Note the argument order: atan2(y, x) is the conventional (and C-library)
    // order, so `atan2(a, b)` is the angle of the vector (a, b).
    let y = want_num(&args[0], "atan2")?;
    let x = want_num(&args[1], "atan2")?;
    Ok(Value::Float(y.atan2(x)))
}

fn n_hypot(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!("hypot expects 2 arguments, got {}", args.len()));
    }
    let x = want_num(&args[0], "hypot")?;
    let y = want_num(&args[1], "hypot")?;
    // `hypot` rather than `sqrt(x*x + y*y)`: the naive form overflows for
    // large-but-representable operands (1e200 each).
    Ok(Value::Float(x.hypot(y)))
}

/// `float_or(str, default)` — parse a string as a float, falling back to
/// `default` when the text is not a number. Numbers pass through unchanged,
/// so a value already in numeric form needs no guard at the call site.
fn n_float_or(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!("float_or expects 2 arguments, got {}", args.len()));
    }
    let fallback = want_num(&args[1], "float_or")?;
    match &args[0] {
        Value::Str(s) => Ok(Value::Float(s.trim().parse::<f64>().unwrap_or(fallback))),
        Value::Int(i) => Ok(Value::Float(*i as f64)),
        Value::Float(f) => Ok(Value::Float(*f)),
        other => Err(format!(
            "float_or expects a string or number, found {}",
            crate::value::type_name(other)
        )),
    }
}

/// `clamp(x, lo, hi)` — `x` limited to the closed range `[lo, hi]`.
fn n_clamp(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(format!("clamp expects 3 arguments, got {}", args.len()));
    }
    let x = want_num(&args[0], "clamp")?;
    let lo = want_num(&args[1], "clamp")?;
    let hi = want_num(&args[2], "clamp")?;
    // `f64::clamp` panics when lo > hi or a bound is NaN, so the range is
    // validated here and reported as an ordinary (catchable) error.
    // `partial_cmp` answers `None` for a NaN bound, which must be rejected
    // too — otherwise `clamp` would panic the host on user input.
    if matches!(
        lo.partial_cmp(&hi),
        None | Some(std::cmp::Ordering::Greater)
    ) {
        return Err(format!(
            "clamp: low bound {} is not below or equal to the high bound {}",
            lo, hi
        ));
    }
    Ok(Value::Float(x.clamp(lo, hi)))
}

fn n_nan(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    no_args("nan", &args)?;
    Ok(Value::Float(f64::NAN))
}

fn n_inf(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    no_args("inf", &args)?;
    Ok(Value::Float(f64::INFINITY))
}

/// Shared arity guard for the zero-argument constants/generators.
fn no_args(name: &str, args: &[Value]) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(format!("{} expects 0 arguments, got {}", name, args.len()))
    }
}

/// Uniform f64 in `[0.0, 1.0)`. 53 of the xorshift output's bits become the
/// mantissa: using all 64 would round the top half of the range up to exactly
/// 1.0, which is outside the promised interval.
fn random_unit() -> f64 {
    (next_random() >> 11) as f64 * RANDOM_UNIT_SCALE
}

/// 2^-53, the scale that maps a 53-bit integer onto `[0.0, 1.0)`.
const RANDOM_UNIT_SCALE: f64 = 1.0 / 9_007_199_254_740_992.0;

fn n_random_float(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.is_empty() {
        Ok(Value::Float(random_unit()))
    } else if args.len() == 2 {
        let min = match &args[0] {
            Value::Float(f) => *f,
            Value::Int(i) => *i as f64,
            _ => return Err("random_float expects numbers for min and max".to_string()),
        };
        let max = match &args[1] {
            Value::Float(f) => *f,
            Value::Int(i) => *i as f64,
            _ => return Err("random_float expects numbers for min and max".to_string()),
        };
        if min >= max {
            return Err("random_float: min must be less than max".to_string());
        }
        let u = random_unit();
        Ok(Value::Float(min + u * (max - min)))
    } else {
        Err(format!(
            "random_float expects 0 or 2 arguments, got {}",
            args.len()
        ))
    }
}

/// `random_normal()` — one sample from the standard normal distribution,
/// via the Box-Muller transform of two uniform draws. `1.0 - u` keeps the
/// logarithm's argument in `(0, 1]`, so the transform can never emit an
/// infinity even in the (2^-53-probability) zero case.
fn n_random_normal(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    no_args("random_normal", &args)?;
    let u = random_unit();
    let v = random_unit();
    let r = (-2.0 * (1.0 - u).ln()).sqrt();
    let theta = 2.0 * std::f64::consts::PI * v;
    Ok(Value::Float(r * theta.cos()))
}

// ------------------------------------------------------------------ crypto

/// `sha256(s)` — FIPS 180-4 SHA-256 of the UTF-8 bytes of `s`, lowercase hex.
/// Same implementation the package layer uses to verify dependencies, so
/// program-computed and tool-computed digests agree bit for bit.
/// Accepts a `bytes` value too, so binary data (a decompressed file, a PNG)
/// can be hashed without a lossy trip through a string.
fn n_sha256(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let data = want_b64_input(&one("sha256", &args)?, "sha256")?;
    Ok(Value::Str(
        crate::pkg::hex_encode(&sha256_bytes(&data)).into(),
    ))
}

/// `hmac_sha256(key, msg)` — RFC 2104 HMAC-SHA256 of the UTF-8 bytes of
/// `msg` under the UTF-8 bytes of `key`, lowercase hex. Keys longer than the
/// 64-byte block are pre-hashed, as the construction requires. Both arguments
/// may be strings or `bytes`.
fn n_hmac_sha256(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!(
            "hmac_sha256 expects 2 arguments, got {}",
            args.len()
        ));
    }
    let key = want_b64_input(&args[0], "hmac_sha256")?;
    let msg = want_b64_input(&args[1], "hmac_sha256")?;
    Ok(Value::Str(
        crate::pkg::hex_encode(&hmac_sha256_bytes(&key, &msg)).into(),
    ))
}

/// HMAC-SHA256 (RFC 2104) over a 64-byte block.
pub(crate) fn hmac_sha256_bytes(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&sha256_bytes(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Vec::with_capacity(BLOCK + msg.len());
    inner.extend_from_slice(&ipad);
    inner.extend_from_slice(msg);
    let inner_hash = sha256_bytes(&inner);
    let mut outer = Vec::with_capacity(BLOCK + 32);
    outer.extend_from_slice(&opad);
    outer.extend_from_slice(&inner_hash);
    sha256_bytes(&outer)
}

// ------------------------------------------------------ pbkdf2 / base64

/// `pbkdf2_sha256(password, salt, iterations)` — PBKDF2-HMAC-SHA256 (RFC
/// 8018) with a 32-byte derived key, returned as lowercase hex. The one
/// primitive user-password storage needs on top of `hmac_sha256`: iterated
/// HMAC slows brute-force attempts by `iterations`. The salt may be a string
/// or a bytes value; iterations are clamped to 1..=10_000_000 (a DoS guard —
/// the native would otherwise spin a host process indefinitely).
fn n_pbkdf2_sha256(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(format!(
            "pbkdf2_sha256 expects 3 arguments (password, salt, iterations), got {}",
            args.len()
        ));
    }
    let password = want_str(&args[0], "pbkdf2_sha256")?;
    let salt = want_salt(&args[1], "pbkdf2_sha256")?;
    let iters = match &args[2] {
        Value::Int(i) if (1..=10_000_000).contains(i) => *i as u32,
        Value::Int(i) => {
            return Err(format!(
                "pbkdf2_sha256: iterations must be between 1 and 10000000, got {}",
                i
            ));
        }
        other => {
            return Err(format!(
                "pbkdf2_sha256 expects an int iteration count, found {}",
                crate::value::type_name(other)
            ));
        }
    };
    Ok(Value::Str(
        crate::pkg::hex_encode(&pbkdf2_sha256_bytes(password.as_bytes(), &salt, iters)).into(),
    ))
}

fn want_salt(v: &Value, who: &str) -> Result<Vec<u8>, String> {
    match v {
        Value::Str(s) => Ok(s.as_bytes().to_vec()),
        Value::Bytes(b) => Ok(b.lock().unwrap().clone()),
        other => Err(format!(
            "{}: salt must be a string or bytes, found {}",
            who,
            crate::value::type_name(other)
        )),
    }
}

/// PBKDF2 (RFC 8018) with HMAC-SHA256 as the PRF and dkLen = 32 — a single
/// block (block index 1), so no dkLen splitting logic is needed.
fn pbkdf2_sha256_bytes(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut block = Vec::with_capacity(salt.len() + 4);
    block.extend_from_slice(salt);
    block.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac_sha256_bytes(password, &block);
    let mut out = u;
    for _ in 1..iterations {
        u = hmac_sha256_bytes(password, &u);
        for (o, b) in out.iter_mut().zip(u.iter()) {
            *o ^= *b;
        }
    }
    out
}

/// `chr(code)` — the one-character string for a Unicode code point.
/// Surrogates (U+D800..U+DFFF) and values above U+10FFFF are rejected.
fn n_chr(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let code = want_int(&one("chr", &args)?, "chr")?;
    if !(0..=0x10FFFF).contains(&code) {
        return Err(format!("chr: code point out of range: {}", code));
    }
    if (0xD800..=0xDFFF).contains(&code) {
        return Err(format!("chr: {} is a surrogate, not a character", code));
    }
    let c = char::from_u32(code as u32)
        .ok_or_else(|| format!("chr: {} is not a valid code point", code))?;
    Ok(Value::Str(c.to_string().into()))
}

/// `ord(s)` — the code point of a one-character string.
fn n_ord(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let s = want_str(&one("ord", &args)?, "ord")?;
    let mut chars = s.chars();
    let first = chars
        .next()
        .ok_or_else(|| "ord: expected a one-character string, got an empty one".to_string())?;
    if chars.next().is_some() {
        return Err(format!(
            "ord: expected a one-character string, got {} characters",
            s.chars().count()
        ));
    }
    Ok(Value::Int(first as i64))
}

/// `secure_compare(a, b)` — timing-safe equality for two strings or two byte
/// values. `==` on digests short-circuits at the first differing byte, which
/// leaks how much of a guess was right; this touches every byte of the longer
/// input and folds the length difference into the same accumulator.
fn n_secure_compare(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!(
            "secure_compare expects 2 arguments, got {}",
            args.len()
        ));
    }
    let a = want_b64_input(&args[0], "secure_compare")?;
    let b = want_b64_input(&args[1], "secure_compare")?;
    let mut diff = (a.len() ^ b.len()) as u64;
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= (x ^ y) as u64;
    }
    Ok(Value::Bool(diff == 0))
}

/// Base64 (RFC 4648). The standard alphabet (+/) with padding, and the
/// URL-safe alphabet (-_) unpadded — the pair the web framework's JWT
/// helpers need. Accepts strings or bytes; `decode` returns bytes.
pub(crate) const B64_STD: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const B64_URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn want_b64_input(v: &Value, who: &str) -> Result<Vec<u8>, String> {
    match v {
        Value::Str(s) => Ok(s.as_bytes().to_vec()),
        Value::Bytes(b) => Ok(b.lock().unwrap().clone()),
        other => Err(format!(
            "{} expects a string or bytes, found {}",
            who,
            crate::value::type_name(other)
        )),
    }
}

pub(crate) fn base64_encode_bytes(data: &[u8], alphabet: &[u8; 64], pad: bool) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(alphabet[(n >> 18) as usize & 63] as char);
        out.push(alphabet[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(alphabet[(n >> 6) as usize & 63] as char);
        } else if pad {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(alphabet[n as usize & 63] as char);
        } else if pad {
            out.push('=');
        }
    }
    out
}

pub(crate) fn base64_decode_str(
    text: &str,
    alphabet: &[u8; 64],
    who: &str,
) -> Result<Vec<u8>, String> {
    // Padding ('=') is accepted only as a 0/1/2-character suffix; its count
    // is implied by the data length, so it is stripped and not validated
    // beyond position (the RFC's canonical encoders never emit it for the
    // URL-safe alphabet anyway).
    let core = text.trim_end_matches('=');
    if text[..core.len()].contains('=') {
        return Err(format!("{}: misplaced '=' padding", who));
    }
    if core.len() % 4 == 1 {
        return Err(format!(
            "{}: invalid base64 length {} (a quantum cannot encode a byte)",
            who,
            core.len()
        ));
    }
    let mut out = Vec::with_capacity(core.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for ch in core.bytes() {
        let v = match alphabet.iter().position(|&a| a == ch) {
            Some(i) => i as u32,
            None => {
                return Err(format!(
                    "{}: invalid base64 character '{}'",
                    who, ch as char
                ));
            }
        };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

fn n_base64_encode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!(
            "base64_encode expects 1 argument, got {}",
            args.len()
        ));
    }
    let data = want_b64_input(&args[0], "base64_encode")?;
    Ok(Value::Str(base64_encode_bytes(&data, B64_STD, true).into()))
}

fn n_base64_url_encode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!(
            "base64_url_encode expects 1 argument, got {}",
            args.len()
        ));
    }
    let data = want_b64_input(&args[0], "base64_url_encode")?;
    Ok(Value::Str(
        base64_encode_bytes(&data, B64_URL, false).into(),
    ))
}

fn n_base64_decode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!(
            "base64_decode expects 1 argument, got {}",
            args.len()
        ));
    }
    let text = want_str(&args[0], "base64_decode")?;
    let data = base64_decode_str(&text, B64_STD, "base64_decode")?;
    Ok(Value::Bytes(new_ref(data)))
}

fn n_base64_url_decode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!(
            "base64_url_decode expects 1 argument, got {}",
            args.len()
        ));
    }
    let text = want_str(&args[0], "base64_url_decode")?;
    let data = base64_decode_str(&text, B64_URL, "base64_url_decode")?;
    Ok(Value::Bytes(new_ref(data)))
}

// ------------------------------------------------------------------ hex/base32

const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";
const B32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// `hex_encode(data)` — lowercase hex, two digits per byte. Accepts a string
/// (its UTF-8 bytes) or a bytes value.
fn n_hex_encode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("hex_encode expects 1 argument, got {}", args.len()));
    }
    let data = want_b64_input(&args[0], "hex_encode")?;
    let mut out = String::with_capacity(data.len() * 2);
    for b in &data {
        out.push(HEX_LOWER[(b >> 4) as usize] as char);
        out.push(HEX_LOWER[(b & 0x0f) as usize] as char);
    }
    Ok(Value::Str(out.into()))
}

/// `hex_decode(text)` — bytes. Rejects odd lengths and non-hex characters.
fn n_hex_decode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("hex_decode expects 1 argument, got {}", args.len()));
    }
    let text = want_str(&args[0], "hex_decode")?;
    let bytes = text.as_bytes();
    if bytes.len() % 2 != 0 {
        return Err(format!("hex_decode: odd length ({})", bytes.len()));
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_val(bytes[i])
            .ok_or_else(|| format!("hex_decode: '{}' is not a hex digit", bytes[i] as char))?;
        let lo = hex_val(bytes[i + 1])
            .ok_or_else(|| format!("hex_decode: '{}' is not a hex digit", bytes[i + 1] as char))?;
        out.push(hi * 16 + lo);
        i += 2;
    }
    Ok(Value::Bytes(new_ref(out)))
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `base32_encode(data)` — RFC 4648 standard alphabet, padded. Accepts a
/// string or bytes.
fn n_base32_encode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!(
            "base32_encode expects 1 argument, got {}",
            args.len()
        ));
    }
    let data = want_b64_input(&args[0], "base32_encode")?;
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    for chunk in data.chunks(5) {
        // Pack up to 5 bytes into a 40-bit buffer, then emit eight 5-bit
        // groups, padding the tail per the RFC 4648 table.
        let mut buf = [0u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let bits: u64 = (buf[0] as u64) << 32
            | (buf[1] as u64) << 24
            | (buf[2] as u64) << 16
            | (buf[3] as u64) << 8
            | (buf[4] as u64);
        let keep = match chunk.len() {
            1 => 2,
            2 => 4,
            3 => 5,
            4 => 7,
            _ => 8,
        };
        for k in 0..8 {
            if k < keep {
                let v = ((bits >> (35 - 5 * k)) & 0x1f) as usize;
                out.push(B32_ALPHABET[v] as char);
            } else {
                out.push('=');
            }
        }
    }
    Ok(Value::Str(out.into()))
}

/// `base32_decode(text)` — bytes. Rejects bad lengths and non-alphabet
/// characters (padding optional but must be trailing).
fn n_base32_decode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!(
            "base32_decode expects 1 argument, got {}",
            args.len()
        ));
    }
    let text = want_str(&args[0], "base32_decode")?;
    let raw = text.as_bytes();
    if raw.is_empty() {
        return Ok(Value::Bytes(new_ref(Vec::new())));
    }
    if raw.len() % 8 != 0 {
        return Err(format!(
            "base32_decode: length {} is not a multiple of 8",
            raw.len()
        ));
    }
    let mut out = Vec::with_capacity(raw.len() / 8 * 5);
    let mut bits: u64 = 0;
    let mut nbits = 0u32;
    for &b in raw {
        if b == b'=' {
            break; // padding: stop consuming
        }
        let v = b32_val(b)
            .ok_or_else(|| format!("base32_decode: '{}' is not a base32 character", b as char))?;
        bits = (bits << 5) | v as u64;
        nbits += 5;
        if nbits >= 8 {
            nbits -= 8;
            out.push((bits >> nbits) as u8);
        }
    }
    Ok(Value::Bytes(new_ref(out)))
}

fn b32_val(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'2'..=b'7' => Some(b - b'2' + 26),
        _ => None,
    }
}

// ------------------------------------------------------------- conversions

fn n_int(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let v = one("int", &args)?;
    match v {
        Value::Int(i) => Ok(Value::Int(i)),
        Value::Float(f) => {
            if f.is_nan() {
                return Err("int(): cannot convert NaN to integer".into());
            }
            if !f.is_finite() || f >= 9223372036854775808.0 || f < -9223372036854775808.0 {
                return Err(format!("int(): float {} is out of integer range", f));
            }
            Ok(Value::Int(f.trunc() as i64))
        }
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

/// The integer operand of a bitwise native.
fn bit_int(v: &Value, who: &str) -> Result<i64, String> {
    match v {
        Value::Int(i) => Ok(*i),
        other => Err(format!(
            "{} expects int operands, found {}",
            who,
            crate::value::type_name(other)
        )),
    }
}

fn bit_binop(name: &str, args: &[Value], f: impl Fn(i64, i64) -> i64) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!("{} expects 2 arguments, got {}", name, args.len()));
    }
    let a = bit_int(&args[0], name)?;
    let b = bit_int(&args[1], name)?;
    Ok(Value::Int(f(a, b)))
}

// `a & b` and friends are desugared to these by the parser, so the semantics
// live in one place instead of in each engine.
fn n_bit_and(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    bit_binop("bit_and", &args, |a, b| a & b)
}

fn n_bit_or(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    bit_binop("bit_or", &args, |a, b| a | b)
}

fn n_bit_xor(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    bit_binop("bit_xor", &args, |a, b| a ^ b)
}

/// `a << b`. A shift of 0..=63 is defined; anything else is an error rather
/// than Rust's panic (debug) / wrap (release) on an out-of-range shift.
fn n_shl(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!("shl expects 2 arguments, got {}", args.len()));
    }
    let a = bit_int(&args[0], "shl")?;
    let b = bit_int(&args[1], "shl")?;
    let shift = u32::try_from(b)
        .ok()
        .filter(|s| *s < 64)
        .ok_or_else(|| format!("shl: shift amount {} is out of range (0..63)", b))?;
    Ok(Value::Int(a.wrapping_shl(shift)))
}

/// `a >> b`, arithmetic (sign-preserving) shift.
fn n_shr(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!("shr expects 2 arguments, got {}", args.len()));
    }
    let a = bit_int(&args[0], "shr")?;
    let b = bit_int(&args[1], "shr")?;
    let shift = u32::try_from(b)
        .ok()
        .filter(|s| *s < 64)
        .ok_or_else(|| format!("shr: shift amount {} is out of range (0..63)", b))?;
    Ok(Value::Int(a.wrapping_shr(shift)))
}

fn n_bool(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let v = one("bool", &args)?;
    Ok(Value::Bool(crate::value::truthy(&v)))
}

/// `is_failure(v)` — is this a `none` or an `err`?
///
/// This is the predicate the `?` operator lowers to: `x?` becomes
/// `if (is_failure(v)) { return v; } v.unwrap()`. It is a normal builtin rather
/// than a hidden helper so the check is usable directly.
fn n_is_failure(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let v = one("is_failure", &args)?;
    match v {
        Value::Option(o) => Ok(Value::Bool(o.is_none())),
        Value::Result(r) => Ok(Value::Bool(r.is_err())),
        other => Err(format!(
            "is_failure expects an option or a result, found {}",
            crate::value::type_name(&other)
        )),
    }
}

fn n_type_of(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let v = one("type_of", &args)?;
    Ok(Value::Str(crate::value::type_name(&v).into()))
}

/// `gc_collect()` — run one cycle-collection pass; returns the cells swept.
fn n_gc_collect(_ctx: &NativeCtx, _args: Vec<Value>) -> Result<Value, String> {
    // A mark-sweep reads each cell's strong count and then walks its children;
    // another thread that is *running* can move a value between those steps and
    // make a live cell look like garbage (audit A-4). Collect only when the
    // caller is the only thread executing Spid3r code: tasks parked in
    // recv/join/mutex/sleep cannot mutate anything, so they do not block it.
    if !crate::sched::gc_quiescent() {
        return Err(format!(
            "gc_collect: {} other thread(s) are running; join or finish them before collecting",
            crate::sched::active_threads().saturating_sub(1)
        ));
    }
    Ok(Value::Int(crate::value::gc::collect() as i64))
}

/// `gc_live_cells()` — number of currently-live tracked container cells.
fn n_gc_live_cells(_ctx: &NativeCtx, _args: Vec<Value>) -> Result<Value, String> {
    Ok(Value::Int(crate::value::gc::live_cells() as i64))
}

// ------------------------------------------------------------------ random

thread_local! {
    /// 0 means "unseeded": the first use on this thread derives a seed from the
    /// clock, a stack address and the thread id. That mix is enough to make the
    /// stream differ across runs and threads, and it is **not** OS entropy - the
    /// output function is invertible, so a few observed values reveal the rest of
    /// the stream. This generator is for sampling, shuffling and jitter only;
    /// anything an attacker can guess at (tokens, session ids, UUIDs) must use
    /// `random_bytes` (src/stdlib/entropy.rs), which reads the OS CSPRNG.
    static RNG: Cell<u64> = const { Cell::new(0) };
}

pub(crate) fn next_random() -> u64 {
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

/// Text → JSON `Value`, for host code that speaks JSON over a wire (the LSP
/// server) rather than through a `json_parse` builtin call. Same parser, same
/// errors, same depth guard — one implementation, not two.
pub(crate) fn json_parse_text(s: &str) -> Result<Value, String> {
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

struct JsonParser {
    chars: Vec<char>,
    pos: usize,
    depth: usize,
}

/// Render the tail of an "unexpected …" JSON diagnostic (F-8). A `None`
/// peek is end-of-input: the old `{:?}` Debug formatting leaked Rust's
/// `Option::None` into the message ("json_parse: unexpected character
/// None"), so the EOF case gets its own wording instead.
fn unexpected_json(peeked: Option<char>) -> String {
    match peeked {
        Some(c) => format!("unexpected character '{}'", c),
        None => "unexpected end of input".to_string(),
    }
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
            other => Err(format!("json_parse: {}", unexpected_json(other))),
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
                                    char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00))
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
                other => return Err(format!("json_parse: {} in array", unexpected_json(other))),
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
            let mk = crate::value::MapKey::Str(key.into());
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
                other => return Err(format!("json_parse: {} in object", unexpected_json(other))),
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

// ------------------------------------------------------------------ binary bytes & npy

/// `file_delete(path)` -> `ok(true)` when removed, `ok(false)` when it did not
/// exist, `err(msg)` on any other failure.
///
/// A file-backed store has to be able to revoke: truncating instead of
/// deleting leaves junk behind forever, and there is no shell to fall back on.
fn n_file_delete(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let path = want_str(&one("file_delete", &args)?, "file_delete")?;
    match std::fs::remove_file(&path) {
        Ok(()) => collections::construct("ok", vec![Value::Bool(true)]),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            collections::construct("ok", vec![Value::Bool(false)])
        }
        Err(e) => collections::construct(
            "err",
            vec![Value::Str(
                format!("cannot delete '{}': {}", path, e).into(),
            )],
        ),
    }
}

/// `bytes_concat([b1, b2, ...])` -> `ok(bytes)`.
///
/// The one operation a byte-oriented protocol needs and the language cannot
/// express: `+` is not defined on `bytes`, and building a large payload by
/// pushing one int at a time is O(n) Value allocations.
fn n_bytes_concat(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let v = one("bytes_concat", &args)?;
    let items = match &v {
        Value::List(l) | Value::VecDyn(l) => l.lock().unwrap().items.clone(),
        other => vec![other.clone()],
    };
    let mut out: Vec<u8> = Vec::new();
    for item in items {
        match item {
            Value::Bytes(b) => out.extend_from_slice(&b.lock().unwrap()),
            Value::Str(s) => out.extend_from_slice(s.as_bytes()),
            other => {
                return Err(format!(
                    "bytes_concat expects bytes or strings, found {}",
                    crate::value::type_name(&other)
                ))
            }
        }
    }
    Ok(Value::Bytes(new_ref(out)))
}

/// `dir_create(path)` -> `ok(true)` when created, `ok(false)` when it already
/// existed, `err(msg)` otherwise. Creates parents.
fn n_dir_create(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let path = want_str(&one("dir_create", &args)?, "dir_create")?;
    if std::path::Path::new(&path).is_dir() {
        return collections::construct("ok", vec![Value::Bool(false)]);
    }
    match std::fs::create_dir_all(&path) {
        Ok(()) => collections::construct("ok", vec![Value::Bool(true)]),
        Err(e) => collections::construct(
            "err",
            vec![Value::Str(
                format!("cannot create '{}': {}", path, e).into(),
            )],
        ),
    }
}

/// `dir_remove(path)` -> `ok(true)` when removed, `ok(false)` when absent.
/// Recursive: a test fixture that leaves a directory behind is a bug.
fn n_dir_remove(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let path = want_str(&one("dir_remove", &args)?, "dir_remove")?;
    if !std::path::Path::new(&path).exists() {
        return collections::construct("ok", vec![Value::Bool(false)]);
    }
    match std::fs::remove_dir_all(&path) {
        Ok(()) => collections::construct("ok", vec![Value::Bool(true)]),
        Err(e) => collections::construct(
            "err",
            vec![Value::Str(
                format!("cannot remove '{}': {}", path, e).into(),
            )],
        ),
    }
}

fn n_read_bytes(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let path = want_str(&one("read_bytes", &args)?, "read_bytes")?;
    match std::fs::read(&path) {
        Ok(data) => Ok(Value::Result(Ok(Box::new(Value::Bytes(new_ref(data)))))),
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(
            format!("cannot read '{}': {}", path, e).into(),
        ))))),
    }
}

fn n_write_bytes(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!(
            "write_bytes expects 2 arguments (path, bytes), got {}",
            args.len()
        ));
    }
    let path = want_str(&args[0], "write_bytes")?;
    match &args[1] {
        Value::Bytes(b) => {
            let data = b.lock().unwrap().clone();
            match std::fs::write(&path, data) {
                Ok(()) => Ok(Value::Result(Ok(Box::new(Value::Null)))),
                Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(
                    format!("cannot write to '{}': {}", path, e).into(),
                ))))),
            }
        }
        other => Err(format!(
            "write_bytes expects a bytes value, found {}",
            crate::value::type_name(other)
        )),
    }
}

fn n_save_npy(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!(
            "save_npy expects 2 arguments (path, matrix_or_vector), got {}",
            args.len()
        ));
    }
    let path = want_str(&args[0], "save_npy")?;
    match &args[1] {
        Value::Vector(v) => {
            let v = v.lock().unwrap();
            match collections::misc::save_npy_file(&path, &[v.data.len()], &v.data) {
                Ok(()) => Ok(Value::Result(Ok(Box::new(Value::Null)))),
                Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(e.into()))))),
            }
        }
        Value::Matrix(m) => {
            let m = m.lock().unwrap();
            match collections::misc::save_npy_file(&path, &[m.rows, m.cols], &m.data) {
                Ok(()) => Ok(Value::Result(Ok(Box::new(Value::Null)))),
                Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(e.into()))))),
            }
        }
        other => Err(format!(
            "save_npy expects a matrix or vector, found {}",
            crate::value::type_name(other)
        )),
    }
}

fn n_load_npy(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let path = want_str(&one("load_npy", &args)?, "load_npy")?;
    match collections::misc::load_npy_data(&path) {
        Ok(collections::misc::NpyLoaded::Vector(data)) => Ok(Value::Result(Ok(Box::new(
            Value::Vector(new_ref(collections::misc::VectorData { data })),
        )))),
        Ok(collections::misc::NpyLoaded::Matrix { rows, cols, data }) => {
            Ok(Value::Result(Ok(Box::new(Value::Matrix(new_ref(
                collections::misc::MatrixData { rows, cols, data },
            ))))))
        }
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(e.into()))))),
    }
}

// ------------------------------------------------------------------ csv

fn n_parse_csv(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(format!(
            "parse_csv expects 1 or 2 arguments, got {}",
            args.len()
        ));
    }
    let text = want_str(&args[0], "parse_csv")?;
    let mut delimiter = ',';
    let mut has_header = true;
    if args.len() == 2 && !matches!(args[1], Value::Null) {
        match &args[1] {
            Value::Map(m) => {
                let m = m.lock().unwrap();
                if let Some(i) = m.find(&MapKey::Str("delimiter".into())) {
                    if let Value::Str(s) = &m.entries[i].1 {
                        if let Some(ch) = s.chars().next() {
                            delimiter = ch;
                        }
                    }
                }
                if let Some(i) = m.find(&MapKey::Str("has_header".into())) {
                    if let Value::Bool(b) = &m.entries[i].1 {
                        has_header = *b;
                    }
                }
            }
            other => {
                return Err(format!(
                    "parse_csv options must be a map or null, found {}",
                    crate::value::type_name(other)
                ));
            }
        }
    }
    match csv::parse_csv_text(&text, delimiter, has_header) {
        Ok(v) => Ok(v),
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(e.into()))))),
    }
}

fn n_read_csv(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(format!(
            "read_csv expects 1 or 2 arguments, got {}",
            args.len()
        ));
    }
    let path = want_str(&args[0], "read_csv")?;
    let text = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            return Ok(Value::Result(Err(Box::new(Value::Str(
                format!("cannot read '{}': {}", path, e).into(),
            )))));
        }
    };
    let mut parse_args = vec![Value::Str(text.into())];
    if args.len() == 2 {
        parse_args.push(args[1].clone());
    }
    n_parse_csv(_ctx, parse_args)
}

fn n_write_csv(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(format!(
            "write_csv expects 3 arguments (path, headers, rows), got {}",
            args.len()
        ));
    }
    let path = want_str(&args[0], "write_csv")?;
    let headers = match &args[1] {
        Value::List(l) | Value::VecDyn(l) => l.lock().unwrap().items.clone(),
        Value::Tuple(t) => t.lock().unwrap().clone(),
        other => {
            return Err(format!(
                "write_csv headers expects a list, found {}",
                crate::value::type_name(other)
            ));
        }
    };
    let rows = match &args[2] {
        Value::List(l) | Value::VecDyn(l) => l.lock().unwrap().items.clone(),
        Value::Tuple(t) => t.lock().unwrap().clone(),
        other => {
            return Err(format!(
                "write_csv rows expects a list, found {}",
                crate::value::type_name(other)
            ));
        }
    };
    csv::write_csv_file(&path, &headers, &rows)
}

// ------------------------------------------------------------------ regex

fn n_regex_match(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!(
            "regex_match expects 2 arguments, got {}",
            args.len()
        ));
    }
    let pat = want_str(&args[0], "regex_match")?;
    let text = want_str(&args[1], "regex_match")?;
    let m = regex::regex_match(&pat, &text)?;
    Ok(Value::Bool(m))
}

fn n_regex_find(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!(
            "regex_find expects 2 arguments, got {}",
            args.len()
        ));
    }
    let pat = want_str(&args[0], "regex_find")?;
    let text = want_str(&args[1], "regex_find")?;
    let found = regex::regex_find(&pat, &text)?;
    Ok(Value::Option(found.map(|s| Box::new(Value::Str(s.into())))))
}

fn n_regex_find_all(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!(
            "regex_find_all expects 2 arguments, got {}",
            args.len()
        ));
    }
    let pat = want_str(&args[0], "regex_find_all")?;
    let text = want_str(&args[1], "regex_find_all")?;
    let items = regex::regex_find_all(&pat, &text)?;
    Ok(collections::make_list(
        items.into_iter().map(|s| Value::Str(s.into())).collect(),
    ))
}

fn n_regex_replace(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 3 {
        return Err(format!(
            "regex_replace expects 3 arguments, got {}",
            args.len()
        ));
    }
    let pat = want_str(&args[0], "regex_replace")?;
    let text = want_str(&args[1], "regex_replace")?;
    let rep = want_str(&args[2], "regex_replace")?;
    let res = regex::regex_replace(&pat, &text, &rep)?;
    Ok(Value::Str(res.into()))
}

// ------------------------------------------------------------------ datetime

/// `now_ms()` - a monotonic millisecond clock, for measuring elapsed time.
fn n_now_ms(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(format!("now_ms expects 0 arguments, got {}", args.len()));
    }
    Ok(Value::Int(crate::sched::now_ms()))
}

fn n_unix_time(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(format!("unix_time takes 0 arguments, got {}", args.len()));
    }
    Ok(Value::Int(datetime::unix_time()))
}

fn n_unix_ms(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(format!("unix_ms takes 0 arguments, got {}", args.len()));
    }
    Ok(Value::Int(datetime::unix_ms()))
}

fn n_datetime_parts(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let sec = want_int(&one("datetime_parts", &args)?, "datetime_parts")?;
    let (year, month, day, hour, minute, second, weekday) = datetime::datetime_parts(sec);
    let entries = vec![
        (MapKey::Str("year".into()), Value::Int(year)),
        (MapKey::Str("month".into()), Value::Int(month as i64)),
        (MapKey::Str("day".into()), Value::Int(day as i64)),
        (MapKey::Str("hour".into()), Value::Int(hour)),
        (MapKey::Str("minute".into()), Value::Int(minute)),
        (MapKey::Str("second".into()), Value::Int(second)),
        (MapKey::Str("weekday".into()), Value::Int(weekday)),
    ];
    Ok(Value::Map(new_ref(collections::MapData::from_entries(
        entries,
    ))))
}

fn n_datetime_from_parts(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 6 {
        return Err(format!(
            "datetime_from_parts expects 6 arguments (year, month, day, hour, minute, second), got {}",
            args.len()
        ));
    }
    let y = want_int(&args[0], "datetime_from_parts")?;
    let m = want_int(&args[1], "datetime_from_parts")?;
    let d = want_int(&args[2], "datetime_from_parts")?;
    let h = want_int(&args[3], "datetime_from_parts")?;
    let min = want_int(&args[4], "datetime_from_parts")?;
    let s = want_int(&args[5], "datetime_from_parts")?;
    let ts = datetime::datetime_from_parts(y, m, d, h, min, s)?;
    Ok(Value::Int(ts))
}

fn n_format_datetime(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 2 {
        return Err(format!(
            "format_datetime expects 2 arguments, got {}",
            args.len()
        ));
    }
    let sec = want_int(&args[0], "format_datetime")?;
    let fmt = want_str(&args[1], "format_datetime")?;
    Ok(Value::Str(datetime::format_datetime(sec, &fmt).into()))
}

fn n_is_ok(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let v = one("is_ok", &args)?;
    match v {
        Value::Result(r) => Ok(Value::Bool(r.is_ok())),
        _ => Ok(Value::Bool(false)),
    }
}

fn n_is_err(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let v = one("is_err", &args)?;
    match v {
        Value::Result(r) => Ok(Value::Bool(r.is_err())),
        _ => Ok(Value::Bool(false)),
    }
}

#[cfg(test)]
mod runtime_cache_tests {
    use super::*;

    /// `SPID3R_CACHE_DIR` is process-global, so the cases below must not run
    /// concurrently with each other.
    static SUITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "spid3r-runtime-cache-{}-{}",
            name,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("SPID3R_CACHE_DIR", &dir);
        dir
    }

    fn cleanup(dir: &std::path::Path) {
        std::env::remove_var("SPID3R_CACHE_DIR");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn extraction_lands_under_the_content_hash() {
        let _serial = SUITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = scratch("layout");
        let data = b"fake-runtime-bytes".to_vec();
        let (exe, freshly_extracted) = cache_runtime(&data).expect("extract");
        assert!(freshly_extracted, "a first extraction writes the file");
        assert!(exe.is_file());
        assert_eq!(
            exe.parent().unwrap().file_name().unwrap().to_string_lossy(),
            sha256_hex(&data),
            "the directory is keyed by the SHA-256 of the content"
        );
        assert_eq!(sha256_hex(&data).len(), 64);
        assert_eq!(std::fs::read(&exe).unwrap(), data);
        // A second call finds the file it just wrote and does not rewrite it.
        let (again, rewritten) = cache_runtime(&data).expect("reuse");
        assert_eq!(again, exe);
        assert!(!rewritten, "a warm cache is not a fresh extraction");
        cleanup(&root);
    }

    /// Two launches of the same app at the same moment are ordinary, and both
    /// must come out with one intact runtime — not a half-written executable.
    #[test]
    fn concurrent_extractions_produce_one_intact_copy() {
        let _serial = SUITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = scratch("concurrent");
        let data: Vec<u8> = (0..512 * 1024).map(|i| (i % 251) as u8).collect();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let data = data.clone();
            handles.push(std::thread::spawn(move || {
                cache_runtime(&data).map(|(path, _)| path)
            }));
        }
        let mut paths = Vec::new();
        for h in handles {
            paths.push(h.join().expect("no panic").expect("every launch succeeds"));
        }
        assert!(
            paths.windows(2).all(|w| w[0] == w[1]),
            "all launches must agree on one path: {:?}",
            paths
        );
        assert_eq!(std::fs::read(&paths[0]).unwrap(), data);
        // No temporary files may be left behind.
        let leftovers: Vec<_> = std::fs::read_dir(paths[0].parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{:?}", leftovers);
        cleanup(&root);
    }

    /// The directory name is a content hash, so a same-size file with different
    /// bytes is not the runtime we asked for. Executing it would run whatever
    /// another local process planted in the cache.
    #[test]
    fn a_forged_cached_runtime_is_replaced() {
        let _serial = SUITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = scratch("forged");
        let data = b"the-real-runtime".to_vec();
        let (exe, _) = cache_runtime(&data).expect("extract");

        // Same length, different bytes: a length check alone would accept it,
        // and so would any invertible hash of the content.
        let forged = vec![b'X'; data.len()];
        std::fs::write(&exe, &forged).expect("overwrite with same size");
        assert_eq!(std::fs::read(&exe).unwrap(), forged);
        assert_eq!(
            cached_runtime_state(&exe, &data, &runtime_key(&data)),
            CacheState::Mismatch,
            "a same-size impostor must not read as intact"
        );

        let (exe, replaced) = cache_runtime(&data).expect("re-extract");
        assert!(replaced, "the impostor is replaced, not reused");
        assert_eq!(std::fs::read(&exe).unwrap(), data, "forged copy replaced");
        assert_eq!(
            cached_runtime_state(&exe, &data, &runtime_key(&data)),
            CacheState::Intact
        );
        cleanup(&root);
    }

    /// A file that cannot be read is not a file that is wrong — but it is also
    /// not a file that may be run. "Make it unreadable" is something an
    /// attacker with cache write access can arrange (deny read, allow
    /// execute), so the unverifiable copy is skipped, not trusted; the bytes we
    /// carry are unpacked beside it instead, which also keeps a scanner's
    /// temporary lock from costing the launch.
    #[test]
    fn an_unreadable_cached_runtime_is_never_executed() {
        let _serial = SUITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = scratch("unreadable");
        let data = b"the-real-runtime".to_vec();
        let key = runtime_key(&data);
        let dir = root.join("runtime").join(&key);
        std::fs::create_dir_all(&dir).unwrap();
        // A directory at the executable's path cannot be read as a file, which
        // is the portable stand-in for a sharing violation.
        let unreadable = dir.join(runtime_file_name());
        std::fs::create_dir(&unreadable).unwrap();
        std::fs::write(unreadable.join("held"), b"x").unwrap();
        assert_eq!(
            cached_runtime_state(&unreadable, &data, &key),
            CacheState::Unreadable
        );

        let (path, written) =
            cache_runtime(&data).expect("a locked entry must not cost the launch");
        assert_ne!(path, unreadable, "the unverifiable file must not be run");
        assert!(written, "a fresh copy is extracted instead");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            data,
            "what runs is the runtime we carry"
        );
        assert!(unreadable.is_dir(), "the locked entry is left alone");
        assert_eq!(
            path.parent().unwrap().parent().unwrap(),
            root.join("runtime"),
            "the fresh copy still lives in the cache, so pruning can reclaim it"
        );
        cleanup(&root);
    }

    /// A missing cache is the ordinary first-launch case.
    #[test]
    fn a_missing_runtime_is_reported_as_missing() {
        let _serial = SUITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = scratch("missing");
        let data = b"the-real-runtime".to_vec();
        let exe = root
            .join("runtime")
            .join(runtime_key(&data))
            .join(runtime_file_name());
        assert_eq!(
            cached_runtime_state(&exe, &data, &runtime_key(&data)),
            CacheState::Missing
        );
        cleanup(&root);
    }

    /// Every rebuild produces a new hash directory; without pruning the cache
    /// grows by a full runtime (~10 MB) per build, forever.
    #[test]
    fn old_runtimes_are_pruned_but_recent_ones_survive() {
        let _serial = SUITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = scratch("prune");
        let runtime_root = root.join("runtime");
        std::fs::create_dir_all(&runtime_root).unwrap();
        // Five older builds, distinguishable only by mtime.
        for i in 0..5 {
            let dir = runtime_root.join(format!("old{i:016x}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(runtime_file_name()), b"old").unwrap();
            let when =
                std::time::SystemTime::now() - std::time::Duration::from_secs(3600 * (10 - i));
            let _ = filetime_set(&dir, when);
        }
        let (exe, _) = cache_runtime(b"current-runtime").expect("extract");
        let current = exe
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();

        let mut names: Vec<String> = std::fs::read_dir(&runtime_root)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        assert!(names.contains(&current), "the current runtime must stay");
        assert_eq!(
            names.len(),
            3,
            "current + two most recent others: {:?}",
            names
        );
        cleanup(&root);
    }

    /// Best-effort mtime adjustment for the pruning test (no external crates).
    fn filetime_set(dir: &std::path::Path, _when: std::time::SystemTime) -> std::io::Result<()> {
        // Directory mtimes cannot be set portably without a dependency; the
        // pruning order falls back to creation order, which is enough here:
        // touch the file so `modified()` is populated at all.
        let f = dir.join(runtime_file_name());
        if f.is_file() {
            let bytes = std::fs::read(&f)?;
            std::fs::write(&f, bytes)?;
        }
        Ok(())
    }
}

/// `resolve_runtime` decides which binary the launcher will start. It must
/// never start one itself: that is what keeps a corrupt payload from becoming
/// "run whatever `spid3r.exe` this machine happens to have".
#[cfg(test)]
mod launch_resolution_tests {
    use super::*;

    /// `SPID3R_EXE` is process-global, so these cases must not overlap.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spid3r-launch-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// An exe whose trailer says "payload here" but whose body cannot be
    /// parsed: exactly what a truncated or bit-rotted app looks like.
    fn write_corrupt_payload(exe: &std::path::Path) {
        let mut bytes = vec![0u8; 64];
        bytes.extend_from_slice(b"SPID3R-PAYLOAD01");
        bytes.extend_from_slice(&8u64.to_le_bytes());
        std::fs::write(exe, &bytes).unwrap();
    }

    /// The regression test for the redirect: with a perfectly usable installed
    /// runtime available, a corrupt payload must still be a hard error. If the
    /// old `.ok().flatten()` fallback came back, this would return
    /// `Ok(<sentinel>)` and the launcher would spawn it.
    #[test]
    fn a_corrupt_payload_does_not_fall_back_to_an_installed_runtime() {
        let _serial = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("corrupt");
        let sentinel = dir.join("installed").join("spid3r.exe");
        std::fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
        std::fs::write(&sentinel, b"a perfectly good runtime").unwrap();

        let previous = std::env::var_os("SPID3R_EXE");
        std::env::set_var("SPID3R_EXE", &sentinel);

        let corrupt = dir.join("corrupt.exe");
        write_corrupt_payload(&corrupt);
        let err = resolve_runtime(&corrupt, false).expect_err("a corrupt payload must not resolve");
        assert!(err.contains("corrupt"), "{}", err);
        assert!(err.contains("refusing to fall back"), "{}", err);
        assert!(
            !err.contains(&sentinel.display().to_string()),
            "the installed runtime must not even be considered: {}",
            err
        );

        // The same environment, minus the payload: the fallback is still the
        // documented behaviour for an app that carries nothing.
        let plain = dir.join("plain.exe");
        std::fs::write(&plain, vec![0u8; 512]).unwrap();
        let (resolved, extracted) = resolve_runtime(&plain, false).expect("no payload, no problem");
        assert_eq!(resolved, sentinel, "a payload-free app uses the install");
        assert!(!extracted);

        // ... but the strict form — what the generated launcher stub calls —
        // refuses, because that build is supposed to carry a payload. Stripping
        // the 24-byte trailer is exactly this case.
        let err = resolve_runtime(&plain, true)
            .expect_err("a strict launcher must not fall back to an installed runtime");
        assert!(err.contains("missing or truncated"), "{}", err);
        assert!(
            !err.contains(&sentinel.display().to_string()),
            "the installed runtime must not even be considered: {}",
            err
        );

        match previous {
            Some(v) => std::env::set_var("SPID3R_EXE", v),
            None => std::env::remove_var("SPID3R_EXE"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
