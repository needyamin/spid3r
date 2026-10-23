//! Operating-system entropy: `random_bytes(n)`.
//!
//! This exists because `random_int` is **not** a CSPRNG. It is xorshift128
//! seeded from the clock, a stack address and the thread id - perfectly good
//! for sampling, shuffling and jitter, and completely unsuitable for a session
//! token or a password reset link: the state is 128 bits, the output function
//! is invertible, and observing a few outputs reveals the rest of the stream.
//!
//! A session store that used it would be shipping guessable session IDs, which
//! is a vulnerability rather than a limitation. So the one primitive that needs
//! real entropy gets a real source:
//!
//!   * Unix: `/dev/urandom` (the kernel CSPRNG).
//!   * Windows: `RtlGenRandom` (`SystemFunction036` in advapi32), the same
//!     source `rand`'s `OsRng` uses there.
//!
//! There is no fallback. On a platform with neither, this returns an error -
//! silently degrading to a predictable generator is the failure mode this
//! module exists to prevent.

use super::NativeDef;
use crate::value::collections;
use crate::value::{new_ref, type_name, NativeCtx, Value};

/// Refuse absurd requests: the caller wants a token, not a data blob.
const MAX_BYTES: i64 = 1024 * 1024;

pub fn native_defs() -> Vec<NativeDef> {
    vec![("random_bytes", n_random_bytes)]
}

#[cfg(unix)]
fn fill(buf: &mut [u8]) -> Result<(), String> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom")
        .map_err(|e| format!("random_bytes: cannot open /dev/urandom: {}", e))?;
    f.read_exact(buf)
        .map_err(|e| format!("random_bytes: /dev/urandom read failed: {}", e))
}

#[cfg(windows)]
#[link(name = "advapi32")]
extern "system" {
    /// `RtlGenRandom`. Returns non-zero on success.
    fn SystemFunction036(buf: *mut u8, len: u32) -> u8;
}

#[cfg(windows)]
fn fill(buf: &mut [u8]) -> Result<(), String> {
    if buf.is_empty() {
        return Ok(());
    }
    // SAFETY: `buf` is a valid, uniquely borrowed slice of exactly `len` bytes.
    let ok = unsafe { SystemFunction036(buf.as_mut_ptr(), buf.len() as u32) };
    if ok == 0 {
        Err(format!(
            "random_bytes: RtlGenRandom failed: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn fill(_buf: &mut [u8]) -> Result<(), String> {
    Err("random_bytes: no OS entropy source is available on this platform".to_string())
}

/// `random_bytes(n)` -> `ok(bytes)` with `n` bytes from the OS CSPRNG.
fn n_random_bytes(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "random_bytes";
    if args.len() != 1 {
        return Err(format!("{} expects 1 argument, got {}", WHO, args.len()));
    }
    let n = match &args[0] {
        Value::Int(i) => *i,
        other => {
            return Err(format!(
                "{} expects an int, found {}",
                WHO,
                type_name(other)
            ))
        }
    };
    if !(0..=MAX_BYTES).contains(&n) {
        return Err(format!("{}: n must be between 0 and {}", WHO, MAX_BYTES));
    }
    let mut buf = vec![0u8; n as usize];
    match fill(&mut buf) {
        Ok(()) => collections::construct("ok", vec![Value::Bytes(new_ref(buf))]),
        Err(e) => collections::construct("err", vec![Value::Str(e.into())]),
    }
}
