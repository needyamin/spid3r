//! A sample Spid3r **WASM plugin**.
//!
//! It is the sandboxed counterpart to `hello-host`: the host interprets this
//! module, and the module can do nothing except read the bytes it is given and
//! write bytes back. It cannot open a file, spawn a process, read an
//! environment variable or reach the network — the host links no such imports,
//! so the capability is not revoked, it never existed.
//!
//! # The contract
//!
//! | export | signature | purpose |
//! | --- | --- | --- |
//! | `memory` | — | linear memory (exported automatically for a cdylib) |
//! | `alloc` | `(i32) -> i32` | reserve bytes for the request |
//! | `dealloc` | `(i32, i32) -> ()` | release them |
//! | `hello_greet` | `(i32, i32) -> i64` | `(ptr, len)` in, `(ptr << 32) \| len` out |
//! | `hello_sum` | `(i32, i32) -> i64` | same |
//!
//! The wire format is documented in `src/plugin/abi.rs` of the toolchain:
//! `u8 argc` then tagged values, and a single tagged value back.
//!
//! Build it with:
//!
//! ```console
//! $ cargo build --target wasm32-unknown-unknown --release
//! $ cp target/wasm32-unknown-unknown/release/spid3r_plugin_hello_wasm.wasm plugin.wasm
//! ```

use std::mem;

const TAG_NULL: u8 = 0;
const TAG_INT: u8 = 1;
const TAG_FLOAT: u8 = 2;
const TAG_BOOL: u8 = 3;
const TAG_STRING: u8 = 4;
const TAG_LIST: u8 = 5;
const TAG_ERROR: u8 = 255;

/// Responses are copied here so the guest never hands the host a dangling
/// pointer and never leaks an allocation. One buffer is enough: a call cannot
/// nest.
const OUT_CAP: usize = 1 << 16;
static mut OUT: [u8; OUT_CAP] = [0; OUT_CAP];

/// Reserve `len` bytes and return the pointer. The host writes the request
/// there and calls `dealloc` afterwards.
#[no_mangle]
pub extern "C" fn alloc(len: u32) -> u32 {
    let mut buf: Vec<u8> = Vec::with_capacity(len as usize);
    buf.resize(len as usize, 0);
    let ptr = buf.as_mut_ptr() as u32;
    mem::forget(buf);
    ptr
}

#[no_mangle]
pub extern "C" fn dealloc(ptr: u32, len: u32) {
    // Reconstructing the `Vec` is what frees it; `with_capacity(len)` above
    // means capacity == len, so this is the same allocation.
    unsafe {
        drop(Vec::from_raw_parts(ptr as *mut u8, len as usize, len as usize));
    }
}

/// Copy `bytes` into the static response buffer and return `(ptr << 32) | len`.
fn respond(bytes: &[u8]) -> i64 {
    let n = bytes.len().min(OUT_CAP);
    unsafe {
        let out = &mut *core::ptr::addr_of_mut!(OUT);
        out[..n].copy_from_slice(&bytes[..n]);
        (((out.as_ptr() as u32) as i64) << 32) | (n as i64)
    }
}

fn respond_error(msg: &str) -> i64 {
    let mut out = vec![TAG_ERROR];
    out.extend_from_slice(&(msg.len() as u32).to_le_bytes());
    out.extend_from_slice(msg.as_bytes());
    respond(&out)
}

fn respond_string(s: &str) -> i64 {
    let mut out = vec![TAG_STRING];
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    respond(&out)
}

fn respond_int(i: i64) -> i64 {
    let mut out = vec![TAG_INT];
    out.extend_from_slice(&i.to_le_bytes());
    respond(&out)
}

/// A cursor over the request buffer.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, pos: 0 }
    }

    fn u8(&mut self) -> Result<u8, String> {
        let b = *self.bytes.get(self.pos).ok_or("truncated request")?;
        self.pos += 1;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.bytes.len())
            .ok_or("truncated request")?;
        let s = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    /// Read one tagged value, returning only what this sample understands.
    fn value(&mut self) -> Result<Arg, String> {
        match self.u8()? {
            TAG_NULL => Ok(Arg::Null),
            TAG_INT => {
                let raw = self.take(8)?;
                Ok(Arg::Int(i64::from_le_bytes(raw.try_into().unwrap())))
            }
            TAG_FLOAT => {
                let raw = self.take(8)?;
                Ok(Arg::Float(f64::from_le_bytes(raw.try_into().unwrap())))
            }
            TAG_BOOL => Ok(Arg::Bool(self.u8()? != 0)),
            TAG_STRING => {
                let raw = self.take(4)?;
                let len = u32::from_le_bytes(raw.try_into().unwrap()) as usize;
                let bytes = self.take(len)?;
                Ok(Arg::Str(String::from_utf8_lossy(bytes).to_string()))
            }
            TAG_LIST => {
                let raw = self.take(4)?;
                let count = u32::from_le_bytes(raw.try_into().unwrap()) as usize;
                let mut items = Vec::with_capacity(count.min(1024));
                for _ in 0..count {
                    items.push(self.value()?);
                }
                Ok(Arg::List(items))
            }
            other => Err(format!("unknown tag {other}")),
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)] // the float/bool/list arms are part of the ABI even if this
                    // sample only uses int and string
enum Arg {
    Null,
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    List(Vec<Arg>),
}

fn args(ptr: u32, len: u32) -> Result<Vec<Arg>, String> {
    // SAFETY: the host just wrote `len` bytes at `ptr` via `alloc`, and holds
    // the only reference for the duration of the call.
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let mut r = Reader::new(bytes);
    let count = r.u8()? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(r.value()?);
    }
    Ok(out)
}

fn as_str(a: &Arg) -> Result<&str, String> {
    match a {
        Arg::Str(s) => Ok(s),
        other => Err(format!("expected a string, got {:?}", other)),
    }
}

fn as_int(a: &Arg) -> Result<i64, String> {
    match a {
        Arg::Int(i) => Ok(*i),
        other => Err(format!("expected an int, got {:?}", other)),
    }
}

/// `hello_greet(name) -> string`
#[no_mangle]
pub extern "C" fn hello_greet(ptr: u32, len: u32) -> i64 {
    let out = args(ptr, len).and_then(|a| {
        let name = as_str(a.first().ok_or("hello_greet needs a name")?)?;
        Ok(format!("Hello, {name}! (from wasm)"))
    });
    match out {
        Ok(s) => respond_string(&s),
        Err(e) => respond_error(&e),
    }
}

/// `hello_sum(a, b) -> int`
#[no_mangle]
pub extern "C" fn hello_sum(ptr: u32, len: u32) -> i64 {
    let out = args(ptr, len).and_then(|a| {
        if a.len() != 2 {
            return Err("hello_sum takes two arguments".to_string());
        }
        Ok(as_int(&a[0])? + as_int(&a[1])?)
    });
    match out {
        Ok(i) => respond_int(i),
        Err(e) => respond_error(&e),
    }
}
