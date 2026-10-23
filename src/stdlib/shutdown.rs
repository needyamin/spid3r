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

use super::NativeDef;
use crate::value::{NativeCtx, Value};

/// Set once, never cleared except by `reset_shutdown()` (tests).
static REQUESTED: AtomicBool = AtomicBool::new(false);
/// Guards against installing the handler twice.
static INSTALLED: AtomicBool = AtomicBool::new(false);

const SIGINT: i32 = 2;
#[cfg(unix)]
const SIGTERM: i32 = 15;

extern "C" {
    /// The C runtime's `signal()`. Declared here rather than pulling in a
    /// crate: the standard library does not expose it and this is the only
    /// FFI the runtime needs.
    fn signal(signum: i32, handler: usize) -> usize;
}

extern "C" fn on_signal(_sig: i32) {
    if REQUESTED.swap(true, Ordering::SeqCst) {
        // Second signal: the drain is not finishing. Stop it.
        std::process::abort();
    }
}

fn install() {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    // SAFETY: the handler stores to an atomic and, on the second signal, calls
    // abort(); both are async-signal-safe.
    let handler = on_signal as extern "C" fn(i32);
    unsafe {
        signal(SIGINT, handler as usize);
        #[cfg(unix)]
        signal(SIGTERM, handler as usize);
    }
}

pub fn native_defs() -> Vec<NativeDef> {
    vec![
        ("shutdown_requested", n_requested),
        ("request_shutdown", n_request),
        ("reset_shutdown", n_reset),
    ]
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
    REQUESTED.store(true, Ordering::SeqCst);
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
    Ok(Value::Null)
}
