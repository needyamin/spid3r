//! Embedding API v0 — run Spid3r scripts from a Rust host application.
//!
//! Spid3r's core is a zero-dependency Rust library; this module is the
//! supported surface for embedding it as a scripting layer. The contract is
//! deliberately small and semver-tracked:
//!
//! * [`Engine`] — builder for one scripting configuration: host functions
//!   (stateful Rust closures callable from Spid3r), host globals, extra
//!   module search directories, and script arguments.
//! * [`Engine::run`] — parse + check + execute in-memory source on a
//!   dedicated big-stack thread, returning the program's exit code and the
//!   captured stdout/stderr text.
//! * [`conv`] — lossless conversions between [`Value`] and Rust types.
//!
//! # Example (the whole host-side integration)
//!
//! ```
//! use spid3r_lang::embed::{conv, Engine};
//!
//! let engine = Engine::new()
//!     .register("shout", |args| {
//!         let s = conv::as_str(args.first().unwrap_or(&spid3r_lang::Value::Null)).unwrap_or("");
//!         Ok(conv::from_string(s.to_uppercase()))
//!     })
//!     .set_global("version", conv::from_string("1.0"));
//!
//! let out = engine
//!     .run("demo", r#"fn main() { println(shout("hi") + " " + version); return 0; }"#)
//!     .expect("script failed");
//! assert_eq!(out.exit_code, 0);
//! assert_eq!(out.output, "HI 1.0\n");
//! ```
//!
//! # Semantics and limits (v0)
//!
//! * The **tree-walk interpreter** runs embedded scripts (the reference
//!   engine). The bytecode VM stays a toolchain/CLI concern for now.
//! * Host names are **dynamically typed** (`Any`): the checker knows they
//!   exist, so scripts may call/read them, but their types are unchecked —
//!   the same treatment the standard library's natives get. A script may
//!   shadow a host name with `let` (the host binding is overwritten).
//! * Host functions must be `Fn` (re-entrant); use interior mutability
//!   (`Arc<Atomic*>`/`Arc<Mutex<_>>`) for mutable state. Errors returned as
//!   `Err(String)` surface as normal catchable Spid3r runtime errors.
//! * Output capture is **per-thread**: `print`/`println`/`eprint` from the
//!   script's main thread are captured; output from spawned tasks still goes
//!   to the process stdout. Each run executes on its own worker thread, so
//!   concurrent engines do not see each other's output.
//! * Script arguments (`args()`) are process-global, matching the CLI; set
//!   them once for the process rather than per concurrent engine.
//! * Modules resolve from the script's directory (the `name` passed to
//!   [`Engine::run`]), then the standard search path; `add_module_dir`
//!   appends extra directories.

use std::path::PathBuf;
use std::sync::Arc;

use crate::diagnostics::{Diagnostic, Phase};
use crate::value::{NativeClosureData, NativeClosureFn, Value};

/// A failed embedded run: the diagnostic plus whatever the script printed
/// before failing (empty for lex/parse/check failures, which happen before
/// execution starts). The diagnostic is boxed to keep `Result<_, ScriptError>`
/// small on the hot path.
#[derive(Debug)]
pub struct ScriptError {
    pub diagnostic: Box<Diagnostic>,
    /// Output captured before the failure (stdout + the `eprint` stream).
    pub output: String,
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.diagnostic)
    }
}

impl std::error::Error for ScriptError {}

/// The outcome of a successful run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    /// The value `main` returned (the process-style exit code: 0 = success).
    pub exit_code: i32,
    /// Everything the script's main thread wrote via `print`/`println`/`eprint`.
    pub output: String,
}

/// A group of host capabilities an embedded script can be denied.
///
/// Spid3r has no sandbox by default: a script runs with the authority of the
/// process that hosts it. [`Engine::deny`] is the coarse switch that makes
/// embedding a lower-trust script practical — it replaces the covered natives
/// with stubs that return a catchable `capability denied` error.
///
/// This is a **policy filter, not a sandbox**. It only covers the built-in
/// natives listed in [`Capability::natives`]; a host function you registered
/// yourself keeps whatever authority you gave it, and no filter can take that
/// back. See `docs/SECURITY.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Reading, writing, renaming and deleting files and directories.
    FileSystem,
    /// Starting child processes.
    Process,
    /// TCP, UDP, WebSocket and HTTP (client and server sides).
    Network,
    /// Reading and writing process environment variables.
    Environment,
}

impl Capability {
    /// The native builtins this capability covers.
    pub fn natives(self) -> &'static [&'static str] {
        match self {
            Capability::FileSystem => &[
                "read_file",
                "write_file",
                "write_file_atomic",
                "file_exists",
                "file_delete",
                "remove_file",
                "rename",
                "copy_file",
                "list_dir",
                "is_dir",
                "is_file",
                "file_size",
                "file_mtime",
                "chdir",
                "exe_path",
                "dir_create",
                "dir_remove",
                "read_bytes",
                "write_bytes",
                "save_npy",
                "load_npy",
                "read_csv",
                "write_csv",
            ],
            Capability::Process => &["spawn_process", "run_payload", "exec_capture"],
            Capability::Network => &[
                "tcp_listen",
                "tcp_accept",
                "tcp_connect",
                "tcp_read",
                "tcp_write",
                "tcp_close",
                "tcp_local_addr",
                "tcp_peer_addr",
                "tcp_set_timeout",
                "tcp_poll",
                "tcp_read_exact",
                "tcp_write_bytes",
                "tcp_recv_to_file",
                "tcp_send_file",
                "ws_connect",
                "ws_accept",
                "ws_send",
                "ws_recv",
                "ws_close",
                "udp_bind",
                "udp_join_multicast",
                "udp_leave_multicast",
                "udp_set_multicast_ttl",
                "udp_set_broadcast",
                "udp_local_addr",
                "udp_send_to",
                "udp_recv_from",
                "udp_close",
                "http_read_request",
                "http_write_response",
                "http_fetch",
                "__http_fetch",
            ],
            Capability::Environment => &["env", "env_set"],
        }
    }
}

/// Builder-style scripting engine. Cheap to clone; prefer creating one per
/// host configuration rather than per call.
#[derive(Clone, Default)]
pub struct Engine {
    module_dirs: Vec<PathBuf>,
    host_natives: Vec<NativeClosureData>,
    globals: Vec<(String, Value)>,
    script_args: Vec<String>,
    budget: Option<u64>,
}

impl Engine {
    /// A new engine with the standard module search path
    /// (`modules::default_module_dirs()`: importing file's directory,
    /// `SPID3R_PATH`, `vendor/` walk-up, `./stdlib`).
    pub fn new() -> Self {
        Engine {
            module_dirs: crate::modules::default_module_dirs(),
            host_natives: Vec::new(),
            globals: Vec::new(),
            script_args: Vec::new(),
            budget: None,
        }
    }

    /// Append an extra module search directory (searched after the defaults,
    /// in insertion order).
    pub fn add_module_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.module_dirs.push(dir.into());
        self
    }

    /// Register a host function callable from Spid3r code.
    ///
    /// The closure receives the call arguments and returns a value or an
    /// error string. Errors surface as ordinary catchable Spid3r runtime
    /// errors. Registering a name that already exists (a stdlib native or a
    /// previous registration) overrides it.
    ///
    /// ```
    /// use spid3r_lang::embed::{conv, Engine};
    /// use std::sync::atomic::{AtomicI64, Ordering};
    /// use std::sync::Arc;
    ///
    /// let calls = Arc::new(AtomicI64::new(0));
    /// let c = calls.clone();
    /// let engine = Engine::new().register("tick", move |_args| {
    ///     Ok(conv::from_i64(c.fetch_add(1, Ordering::SeqCst)))
    /// });
    /// let out = engine.run("t", "fn main() { tick(); tick(); println(tick()); return 0; }").unwrap();
    /// assert_eq!(out.output, "2\n");
    /// ```
    pub fn register<F>(mut self, name: &str, f: F) -> Self
    where
        F: Fn(&[Value]) -> Result<Value, String> + Send + Sync + 'static,
    {
        let func: Arc<NativeClosureFn> = Arc::new(move |_ctx, args| f(&args));
        self.host_natives.push(NativeClosureData {
            name: name.to_string(),
            func,
        });
        self
    }

    /// Bind a global value that the script can read. Host globals are dynamic
    /// (`Any`) and may be shadowed by a script-level `let`.
    pub fn set_global(mut self, name: &str, value: Value) -> Self {
        self.globals.push((name.to_string(), value));
        self
    }

    /// Deny a capability: every native it covers is replaced by a stub that
    /// returns a catchable `capability denied: '…' is not available to this
    /// script` error. Chain the calls to deny several.
    ///
    /// This is a policy filter over the built-in natives, not a sandbox — see
    /// [`Capability`] and `docs/SECURITY.md`.
    ///
    /// ```
    /// use spid3r_lang::embed::{Capability, Engine};
    ///
    /// let engine = Engine::new().deny(Capability::FileSystem);
    /// let out = engine
    ///     .run("no_fs", r#"fn main() {
    ///         try { read_file("Cargo.toml"); return 1; } catch e { println("denied"); }
    ///         return 0;
    ///     }"#)
    ///     .expect("script runs");
    /// assert_eq!(out.exit_code, 0);
    /// assert_eq!(out.output, "denied\n");
    /// ```
    pub fn deny(mut self, capability: Capability) -> Self {
        for &name in capability.natives() {
            self = self.register(name, move |_args| {
                Err(format!(
                    "capability denied: '{name}' is not available to this script"
                ))
            });
        }
        self
    }

    /// Deny filesystem, process, network and environment access.
    pub fn deny_all(self) -> Self {
        self.deny(Capability::FileSystem)
            .deny(Capability::Process)
            .deny(Capability::Network)
            .deny(Capability::Environment)
    }

    /// Cap the work one [`Engine::run`] may do: after roughly `steps` units
    /// (statements in the tree engine, instruction-poll batches in the VM)
    /// the run fails with the **fatal** error `execution budget exhausted`.
    /// A fatal error cannot be caught by the script's `try`/`catch`, so a
    /// runaway loop cannot swallow the stop and keep running.
    ///
    /// The allowance is per `run` call and shared by the tasks that run
    /// spawns. `None` (the default) means unlimited. The native AOT backend
    /// does not enforce it — embedders run the tree engine, which this
    /// applies to.
    ///
    /// ```
    /// use spid3r_lang::embed::Engine;
    ///
    /// let engine = Engine::new().budget(10_000);
    /// let err = engine
    ///     .run("spin", "fn main() { while true { } return 0; }")
    ///     .expect_err("the budget must stop the loop");
    /// assert!(err.diagnostic.message.contains("execution budget exhausted"));
    /// ```
    pub fn budget(mut self, steps: u64) -> Self {
        self.budget = Some(steps);
        self
    }

    /// Set `args()` for the script. Process-global (shared with the CLI and
    /// other engines in the same process); prefer setting it once.
    pub fn set_script_args(mut self, args: Vec<String>) -> Self {
        self.script_args = args;
        self
    }

    /// Parse, check, and run `src`. The script's `fn main` is the entry
    /// point; its return value is the exit code. Output written by the main
    /// thread is captured into [`RunResult::output`].
    ///
    /// Lex/parse/check failures return a [`ScriptError`] with the phase
    /// diagnostic and empty output; runtime failures include the output
    /// produced before the failure.
    pub fn run(&self, name: &str, src: &str) -> Result<RunResult, ScriptError> {
        // Front end (shared with the CLI): lex + parse + check. Host names
        // are declared as externals so the checker accepts them as dynamic.
        let file: Arc<str> = Arc::from(name);
        let program = crate::parser::parse_file(src, file.clone()).map_err(|d| ScriptError {
            diagnostic: Box::new(d),
            output: String::new(),
        })?;
        let externals = crate::semantic::Externals {
            names: self
                .host_natives
                .iter()
                .map(|n| n.name.clone())
                .chain(self.globals.iter().map(|(n, _)| n.clone()))
                .collect(),
            module_dirs: self.module_dirs.clone(),
        };
        crate::semantic::check_file_with(&program, true, name, &externals).map_err(|d| {
            ScriptError {
                diagnostic: Box::new(d),
                output: String::new(),
            }
        })?;

        // Execute on a dedicated big-stack worker: the interpreter recurses
        // per nesting level, and the OS main-thread stack is too small for
        // deep-but-valid programs (same rationale as the CLI's dispatch).
        let module_dirs = self.module_dirs.clone();
        let natives = self.host_natives.clone();
        let globals = self.globals.clone();
        let script_args = self.script_args.clone();
        let budget = self.budget.map(crate::budget::Budget::new);
        let run_name = name.to_string();
        let spawned = std::thread::Builder::new()
            .name(format!("spid3r-embed-{}", name))
            .stack_size(512 * 1024 * 1024)
            .spawn(move || {
                crate::stdlib::start_capture();
                crate::stdlib::set_script_args(script_args);
                let mut interp = crate::interpreter::Interp::new(file, module_dirs);
                interp.set_budget(budget);
                for n in &natives {
                    crate::environment::define(
                        &interp.globals,
                        &n.name,
                        Value::NativeClosure(Arc::new(n.clone())),
                    );
                }
                for (g, v) in &globals {
                    crate::environment::define(&interp.globals, g, v.clone());
                }
                // run_program_inline (not run_program): the program must run on
                // THIS thread so the thread-local output capture sees it. This
                // worker already provides the big stack run_program would.
                let code = interp.run_program_inline(&program);
                let output = crate::stdlib::take_capture();
                (code, output)
            })
            .map_err(|e| ScriptError {
                diagnostic: Box::new(Diagnostic::new(
                    Phase::Usage,
                    format!("cannot start embed worker for '{}': {}", run_name, e),
                )),
                output: String::new(),
            })?;
        match spawned.join() {
            Ok((Ok(exit_code), output)) => Ok(RunResult { exit_code, output }),
            Ok((Err(diagnostic), output)) => Err(ScriptError {
                diagnostic: Box::new(diagnostic),
                output,
            }),
            // A host panic inside the worker (a bug in a host function, not
            // in user code) must not be silently swallowed.
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }
}

/// Lossless, allocation-free-where-possible conversions between [`Value`] and
/// Rust types. Every accessor returns `Option`/`Result` rather than guessing.
pub mod conv {
    use crate::value::Value;

    /// `int` → `i64`.
    pub fn as_i64(v: &Value) -> Option<i64> {
        match v {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// `float` → `f64`; `int` widens (mirroring the language's `Int → Float`).
    pub fn as_f64(v: &Value) -> Option<f64> {
        match v {
            Value::Float(f) => Some(*f),
            Value::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    /// `bool` → `bool`.
    pub fn as_bool(v: &Value) -> Option<bool> {
        match v {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// `string` → `&str`.
    pub fn as_str(v: &Value) -> Option<&str> {
        match v {
            Value::Str(s) => Some(s.as_ref()),
            _ => None,
        }
    }

    /// `list`/`vec` → a snapshot of its elements. Any other type is `None`.
    pub fn as_list(v: &Value) -> Option<Vec<Value>> {
        match v {
            Value::List(d) | Value::VecDyn(d) => Some(d.lock().unwrap().items.clone()),
            _ => None,
        }
    }

    /// The value as its display form (what `println` would print).
    pub fn display(v: &Value) -> String {
        crate::value::display_value(v)
    }

    pub fn from_i64(i: i64) -> Value {
        Value::Int(i)
    }

    pub fn from_f64(f: f64) -> Value {
        Value::Float(f)
    }

    pub fn from_bool(b: bool) -> Value {
        Value::Bool(b)
    }

    /// Build a Spid3r `string` from any string-like Rust value.
    pub fn from_string(s: impl Into<std::sync::Arc<str>>) -> Value {
        Value::Str(s.into())
    }

    /// Build a Spid3r `list` from Rust values.
    pub fn from_list(items: Vec<Value>) -> Value {
        Value::List(crate::value::new_ref(crate::value::collections::VecData {
            items,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Script args are process-global (matching the CLI); unit tests run in
    /// parallel threads and would race each other's engine workers. Serializing
    /// this suite removes the race without changing the public contract.
    static SUITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn host_function_with_captured_state_is_callable() {
        let _suite = SUITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use std::sync::atomic::{AtomicI64, Ordering};
        let calls = Arc::new(AtomicI64::new(0));
        let c = calls.clone();
        let engine = Engine::new().register("tick", move |_args| {
            Ok(conv::from_i64(c.fetch_add(1, Ordering::SeqCst)))
        });
        let out = engine
            .run(
                "t",
                "fn main() { tick(); tick(); println(tick()); return 0; }",
            )
            .expect("run");
        assert_eq!(out.exit_code, 0);
        assert_eq!(out.output, "2\n");
    }

    #[test]
    fn host_error_is_catchable_in_script() {
        let _suite = SUITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine = Engine::new().register("explode", |_args| Err("kaboom".to_string()));
        let out = engine
            .run(
                "t",
                "fn main() { try { explode(); } catch(e) { println(\"caught: \" + e); } return 0; }",
            )
            .expect("run");
        assert!(out.output.contains("caught: kaboom"), "{}", out.output);
    }

    #[test]
    fn host_function_works_as_a_collection_callback() {
        let _suite = SUITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine = Engine::new().register("double", |args| {
            let n = conv::as_i64(args.first().unwrap_or(&Value::Null)).unwrap_or(0);
            Ok(conv::from_i64(n * 2))
        });
        let out = engine
            .run(
                "t",
                "fn main() { let xs = list(1, 2, 3); println(xs.map(double)); return 0; }",
            )
            .expect("run");
        assert!(out.output.contains("2, 4, 6"), "{}", out.output);
    }

    #[test]
    fn globals_and_script_args_reach_the_script() {
        let _suite = SUITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine = Engine::new()
            .set_global("answer", conv::from_i64(42))
            .set_script_args(vec!["alpha".to_string(), "beta".to_string()]);
        let out = engine
            .run(
                "t",
                "fn main() { println(answer); println(args()); return 0; }",
            )
            .expect("run");
        assert!(out.output.contains("42"), "{}", out.output);
        assert!(out.output.contains("alpha"), "{}", out.output);
    }

    #[test]
    fn type_of_sees_host_functions_as_functions() {
        let _suite = SUITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine = Engine::new().register("noop", |_args| Ok(Value::Null));
        let out = engine
            .run("t", "fn main() { println(type_of(noop)); return 0; }")
            .expect("run");
        assert!(out.output.contains("function"), "{}", out.output);
    }

    #[test]
    fn runtime_error_carries_partial_output() {
        let _suite = SUITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine = Engine::new();
        let err = engine
            .run(
                "t",
                "fn main() { println(\"before\"); let z = 1 / 0; return 0; }",
            )
            .unwrap_err();
        assert!(err.output.contains("before"), "{}", err.output);
    }

    #[test]
    fn parse_errors_are_reported_before_execution() {
        let _suite = SUITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine = Engine::new();
        let err = engine.run("t", "fn main( { return 0; }").unwrap_err();
        assert_eq!(err.diagnostic.phase, Phase::Parser);
        assert!(err.output.is_empty());
    }

    #[test]
    fn unknown_names_are_still_rejected() {
        let _suite = SUITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Externals are known; genuinely unknown names must still fail.
        let engine = Engine::new();
        let err = engine
            .run("t", "fn main() { return not_registered(); }")
            .unwrap_err();
        assert!(
            err.diagnostic.message.contains("unknown name"),
            "{}",
            err.diagnostic.message
        );
    }
    #[test]
    fn denied_capabilities_are_catchable_and_scoped() {
        let _suite = SUITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let engine = Engine::new().deny_all();
        let src = r#"fn main() {
    try { read_file("Cargo.toml"); println("fs: BAD"); } catch e { println("fs: denied"); }
    try { write_file("/tmp/x", "y"); println("write: BAD"); } catch e { println("write: denied"); }
    try { spawn_process("echo hi"); println("proc: BAD"); } catch e { println("proc: denied"); }
    try { tcp_connect("127.0.0.1:1"); println("net: BAD"); } catch e { println("net: denied"); }
    try { env("PATH"); println("env: BAD"); } catch e { println("env: denied"); }
    // Non-denied natives keep working.
    println(sha256("abc").slice(0, 8));
    return 0;
}"#;
        let out = engine.run("denied", src).expect("script runs");
        assert_eq!(out.exit_code, 0);
        assert_eq!(
            out.output,
            "fs: denied\nwrite: denied\nproc: denied\nnet: denied\nenv: denied\nba7816bf\n"
        );

        // The same script with no filter reaches the filesystem.
        let open = Engine::new();
        let probe = r#"fn main() {
    try { read_file("Cargo.toml"); println("fs: allowed"); } catch e { println("fs: denied"); }
    return 0;
}"#;
        let out = open.run("allowed", probe).expect("script runs");
        assert_eq!(
            out.output, "fs: allowed\n",
            "the filter must be what denies it"
        );
    }
}
