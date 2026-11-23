//! Cooperative shutdown for long-running servers.
//!
//! A production server must stop accepting, finish what is in flight, and only
//! then exit. The pieces a script needs for that are tiny: a flag it can poll
//! and a way to learn that the operator asked to stop. Both live here.
//!
//! The flag is set by a signal handler (SIGINT, plus SIGTERM on Unix) and can
//! also be set directly with `request_shutdown()` - which is what an admin
//! endpoint or a test uses, since a script cannot install its own handler.
//!
//! **The handler only stores to an `AtomicBool`.** That is deliberate: a
//! signal handler runs on an arbitrary thread, possibly while the allocator or
//! a lock is held, so calling back into the interpreter from it would be a
//! use-after-free waiting to happen. Polling keeps the dangerous code to one
//! atomic store, which is async-signal-safe.
//!
//! A second signal aborts the process. Without that, a server whose drain is
//! stuck (a handler in an infinite loop) would be unkillable by Ctrl+C, which
//! is a worse failure than an ungraceful exit.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::NativeDef;
use crate::value::{type_name, NativeCtx, Value};

/// Set once, never cleared except by `reset_shutdown()` (tests).
static REQUESTED: AtomicBool = AtomicBool::new(false);
static SIG_INT: AtomicBool = AtomicBool::new(false);
static SIG_TERM: AtomicBool = AtomicBool::new(false);
static SIG_HUP: AtomicBool = AtomicBool::new(false);
static SIG_USR1: AtomicBool = AtomicBool::new(false);
static SIG_USR2: AtomicBool = AtomicBool::new(false);
/// Guards against installing the handler twice.
static INSTALLED: AtomicBool = AtomicBool::new(false);
/// Cooperative wakeup callback registered by active event loops (such as the
/// desktop GUI window) so `request_shutdown()` exits without waiting on idle I/O.
/// Signal handlers never touch this lock — only `request()` does.
static SHUTDOWN_HOOK: Mutex<Option<Arc<dyn Fn() + Send + Sync>>> = Mutex::new(None);

#[cfg(unix)]
const SIGHUP: i32 = 1;
#[cfg(any(unix, windows))]
const SIGINT: i32 = 2;
#[cfg(unix)]
const SIGTERM: i32 = 15;
#[cfg(target_os = "macos")]
const SIGUSR1: i32 = 30;
#[cfg(all(unix, not(target_os = "macos")))]
const SIGUSR1: i32 = 10;
#[cfg(target_os = "macos")]
const SIGUSR2: i32 = 31;
#[cfg(all(unix, not(target_os = "macos")))]
const SIGUSR2: i32 = 12;

// wasm32-unknown-unknown has no signals and no libc to link against: the
// declaration itself would become an unresolved import and fail the wasm build.
#[cfg(any(unix, windows))]
extern "C" {
    /// The C runtime's `signal()`. Declared here rather than pulling in a
    /// crate: the standard library does not expose it and this is the only
    /// FFI the runtime needs.
    fn signal(signum: i32, handler: usize) -> usize;
}

#[cfg(any(unix, windows))]
extern "C" fn on_signal(sig: i32) {
    #[cfg(unix)]
    {
        if sig == SIGHUP {
            SIG_HUP.store(true, Ordering::SeqCst);
            return;
        }
        if sig == SIGUSR1 {
            SIG_USR1.store(true, Ordering::SeqCst);
            return;
        }
        if sig == SIGUSR2 {
            SIG_USR2.store(true, Ordering::SeqCst);
            return;
        }
        if sig == SIGTERM {
            SIG_TERM.store(true, Ordering::SeqCst);
        }
    }
    if sig == SIGINT {
        SIG_INT.store(true, Ordering::SeqCst);
    }
    if REQUESTED.swap(true, Ordering::SeqCst) {
        // Second termination signal: the drain is not finishing. Stop it.
        std::process::abort();
    }
}

#[cfg(windows)]
extern "system" fn console_ctrl_handler(ctrl_type: u32) -> i32 {
    match ctrl_type {
        // CTRL_C_EVENT (0)
        0 => {
            SIG_INT.store(true, Ordering::SeqCst);
            if REQUESTED.swap(true, Ordering::SeqCst) {
                std::process::abort();
            }
            1
        }
        // CTRL_BREAK_EVENT (1), CTRL_CLOSE_EVENT (2), CTRL_SHUTDOWN_EVENT (6)
        1 | 2 | 6 => {
            SIG_TERM.store(true, Ordering::SeqCst);
            if REQUESTED.swap(true, Ordering::SeqCst) {
                std::process::abort();
            }
            1
        }
        _ => 0,
    }
}

pub(crate) fn install() {
    // Inverted so that the wasm build (where the body below is compiled out)
    // does not end in a bare `return` that clippy flags as needless.
    if !INSTALLED.swap(true, Ordering::SeqCst) {
        #[cfg(any(unix, windows))]
        {
            // SAFETY: the handler stores to an atomic and, on the second
            // termination signal, calls abort(); both are async-signal-safe.
            let handler = on_signal as extern "C" fn(i32);
            unsafe {
                signal(SIGINT, handler as usize);
                #[cfg(unix)]
                {
                    signal(SIGHUP, handler as usize);
                    signal(SIGTERM, handler as usize);
                    signal(SIGUSR1, handler as usize);
                    signal(SIGUSR2, handler as usize);
                }
            }
        }
        #[cfg(windows)]
        {
            extern "system" {
                fn SetConsoleCtrlHandler(
                    handler: Option<extern "system" fn(u32) -> i32>,
                    add: i32,
                ) -> i32;
            }
            // SAFETY: `console_ctrl_handler` only writes lock-free `AtomicBool`s.
            unsafe {
                SetConsoleCtrlHandler(Some(console_ctrl_handler), 1);
            }
        }
        // Everywhere else (wasm) there is nothing to install: the flag is
        // still settable with `request_shutdown()`, which is what an embedding
        // host or a browser calls instead of sending a signal.
    }
}

pub fn native_defs() -> Vec<NativeDef> {
    vec![
        ("shutdown_requested", n_requested),
        ("request_shutdown", n_request),
        ("reset_shutdown", n_reset),
        ("signal_requested", n_signal_requested),
    ]
}

fn flag_for_name(raw: &str) -> Result<&'static AtomicBool, String> {
    let upper = raw.trim().to_ascii_uppercase();
    let norm = upper.strip_prefix("SIG").unwrap_or(&upper);
    match norm {
        "INT" => Ok(&SIG_INT),
        "TERM" => Ok(&SIG_TERM),
        "HUP" => Ok(&SIG_HUP),
        "USR1" => Ok(&SIG_USR1),
        "USR2" => Ok(&SIG_USR2),
        _ => Err(format!(
            "signal_requested: unknown signal '{}' (expected SIGINT, SIGTERM, SIGHUP, SIGUSR1, or SIGUSR2)",
            raw
        )),
    }
}

/// `signal_requested(name, clear?)` -> bool. Installs signal handlers on first
/// call and checks whether `name` (`"SIGINT"`, `"SIGTERM"`, `"SIGHUP"`,
/// `"SIGUSR1"`, `"SIGUSR2"`, with or without `"SIG"` prefix) has fired since
/// the last check. By default (`clear = true`) it atomically clears the flag
/// via `swap(false)` so reload/rotation loops handle each signal once; pass
/// `false` as the 2nd argument to peek without clearing.
fn n_signal_requested(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(format!(
            "signal_requested expects 1 or 2 arguments, got {}",
            args.len()
        ));
    }
    let name: &str = match &args[0] {
        Value::Str(s) => s,
        other => {
            return Err(format!(
                "signal_requested expects a string signal name, found {}",
                type_name(other)
            ))
        }
    };
    let clear = match args.get(1) {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => *b,
        Some(other) => {
            return Err(format!(
                "signal_requested expects a bool clear flag, found {}",
                type_name(other)
            ))
        }
    };
    install();
    let flag = flag_for_name(name)?;
    let fired = if clear {
        flag.swap(false, Ordering::SeqCst)
    } else {
        flag.load(Ordering::SeqCst)
    };
    Ok(Value::Bool(fired))
}

pub fn is_requested() -> bool {
    REQUESTED.load(Ordering::SeqCst)
}

/// Register or clear a callback invoked when `request()` marks shutdown.
pub fn set_hook(hook: Option<Arc<dyn Fn() + Send + Sync>>) {
    let mut guard = SHUTDOWN_HOOK.lock().unwrap_or_else(|e| e.into_inner());
    *guard = hook;
}

/// Request cooperative shutdown from non-signal code and wake any registered
/// shutdown hook (for example, the GUI event loop).
pub fn request() {
    REQUESTED.store(true, Ordering::SeqCst);
    let hook = {
        let guard = SHUTDOWN_HOOK.lock().unwrap_or_else(|e| e.into_inner());
        guard.clone()
    };
    if let Some(h) = hook {
        h();
    }
}

/// `shutdown_requested()` -> bool. Installs the signal handler on first call,
/// so a program that never serves anything is unaffected.
fn n_requested(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(format!(
            "shutdown_requested expects 0 arguments, got {}",
            args.len()
        ));
    }
    install();
    Ok(Value::Bool(REQUESTED.load(Ordering::SeqCst)))
}

/// `request_shutdown()` -> null. Sets the flag for the next poll.
fn n_request(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(format!(
            "request_shutdown expects 0 arguments, got {}",
            args.len()
        ));
    }
    install();
    request();
    Ok(Value::Null)
}

/// `reset_shutdown()` -> null. Clears the flag (tests, and a server that wants
/// to keep running after an operator-initiated drain).
fn n_reset(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if !args.is_empty() {
        return Err(format!(
            "reset_shutdown expects 0 arguments, got {}",
            args.len()
        ));
    }
    REQUESTED.store(false, Ordering::SeqCst);
    SIG_INT.store(false, Ordering::SeqCst);
    SIG_TERM.store(false, Ordering::SeqCst);
    SIG_HUP.store(false, Ordering::SeqCst);
    SIG_USR1.store(false, Ordering::SeqCst);
    SIG_USR2.store(false, Ordering::SeqCst);
    Ok(Value::Null)
}
