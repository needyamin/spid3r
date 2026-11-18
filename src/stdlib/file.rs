//! Opaque integer file-handle natives (`file_open`, `file_read`,
//! `file_read_line`, `file_write`, `file_seek`, `file_tell`, `file_flush`,
//! `file_fsync`, `file_lock`, `file_unlock`, `file_close`).
//!
//! Mirrors the `Sock` handle table in `src/stdlib/net.rs` (Phase R7.1):
//! monotonic `i64` handle IDs (never reused), a cap on simultaneously open
//! handles, and `Result` (`ok(...)` / `err(...)`) returns for all I/O errors.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use super::{want_int, want_str, NativeDef};
use crate::value::{collections, new_ref, type_name, NativeCtx, Value};

const MAX_OPEN_FILES: usize = 1024;
const MAX_READ_BYTES: usize = 64 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

struct FileHandle {
    file: Mutex<File>,
    #[allow(dead_code)]
    path: String,
}

static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);

fn table() -> &'static Mutex<HashMap<i64, Arc<FileHandle>>> {
    static T: OnceLock<Mutex<HashMap<i64, Arc<FileHandle>>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lookup(id: i64, who: &str) -> Result<Arc<FileHandle>, String> {
    table().lock().unwrap().get(&id).cloned().ok_or_else(|| {
        format!(
            "{}: handle {} is not open (already closed or invalid)",
            who, id
        )
    })
}

fn ok(v: Value) -> Result<Value, String> {
    collections::construct("ok", vec![v])
}

fn err(msg: impl Into<String>) -> Result<Value, String> {
    collections::construct("err", vec![Value::Str(msg.into().into())])
}

fn arity(who: &str, args: &[Value], n: usize) -> Result<(), String> {
    if args.len() != n {
        Err(format!(
            "{} expects {} argument(s), got {}",
            who,
            n,
            args.len()
        ))
    } else {
        Ok(())
    }
}

pub fn native_defs() -> Vec<NativeDef> {
    vec![
        ("file_open", n_file_open),
        ("file_read", n_file_read),
        ("file_read_line", n_file_read_line),
        ("file_write", n_file_write),
        ("file_seek", n_file_seek),
        ("file_tell", n_file_tell),
        ("file_flush", n_file_flush),
        ("file_fsync", n_file_fsync),
        ("file_lock", n_file_lock),
        ("file_unlock", n_file_unlock),
        ("file_close", n_file_close),
    ]
}

/// `file_open(path, mode?)` -> `ok(handle)` / `err(msg)`.
/// Supported modes: `"r"`, `"w"`, `"a"`, `"r+"`, `"w+"`, `"a+"` (and `"b"` variants).
pub(crate) fn n_file_open(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "file_open";
    if !(1..=2).contains(&args.len()) {
        return Err(format!(
            "{} expects 1 or 2 arguments, got {}",
            WHO,
            args.len()
        ));
    }
    let path = want_str(&args[0], WHO)?;
    let mode = match args.get(1) {
        None | Some(Value::Null) => "r".to_string(),
        Some(v) => want_str(v, WHO)?,
    };
    let mut opts = OpenOptions::new();
    match mode.as_str() {
        "r" | "rb" => {
            opts.read(true);
        }
        "w" | "wb" => {
            opts.write(true).create(true).truncate(true);
        }
        "a" | "ab" => {
            opts.append(true).create(true);
        }
        "r+" | "rb+" | "r+b" => {
            opts.read(true).write(true);
        }
        "w+" | "wb+" | "w+b" => {
            opts.read(true).write(true).create(true).truncate(true);
        }
        "a+" | "ab+" | "a+b" => {
            opts.read(true).append(true).create(true);
        }
        other => {
            return err(format!(
                "{}: unsupported mode {:?} (expected r, w, a, r+, w+, a+)",
                WHO, other
            ));
        }
    }
    {
        let t = table().lock().unwrap();
        if t.len() >= MAX_OPEN_FILES {
            return err(format!(
                "{}: too many open file handles (limit {})",
                WHO, MAX_OPEN_FILES
            ));
        }
    }
    match opts.open(&path) {
        Ok(file) => {
            let mut t = table().lock().unwrap();
            if t.len() >= MAX_OPEN_FILES {
                return err(format!(
                    "{}: too many open file handles (limit {})",
                    WHO, MAX_OPEN_FILES
                ));
            }
            let id = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
            t.insert(
                id,
                Arc::new(FileHandle {
                    file: Mutex::new(file),
                    path,
                }),
            );
            ok(Value::Int(id))
        }
        Err(e) => err(format!("{} '{}': {}", WHO, path, e)),
    }
}

/// `file_read(id, max_bytes)` -> `ok(bytes)` / `err(msg)`.
fn n_file_read(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "file_read";
    arity(WHO, &args, 2)?;
    let id = want_int(&args[0], WHO)?;
    let max_n = want_int(&args[1], WHO)?;
    if max_n < 0 || (max_n as usize) > MAX_READ_BYTES {
        return err(format!(
            "{}: max_bytes must be between 0 and {}, got {}",
            WHO, MAX_READ_BYTES, max_n
        ));
    }
    let h = match lookup(id, WHO) {
        Ok(h) => h,
        Err(m) => return err(m),
    };
    let mut f = h.file.lock().unwrap();
    let mut buf = vec![0u8; max_n as usize];
    match f.read(&mut buf) {
        Ok(n) => {
            buf.truncate(n);
            ok(Value::Bytes(new_ref(buf)))
        }
        Err(e) => err(format!("{}: {}", WHO, e)),
    }
}

/// `file_read_line(id)` -> `ok(string_or_null)` / `err(msg)`.
/// Reads up to `\n` (stripping trailing `\n` / `\r\n`) without over-buffering
/// past the newline so `file_tell` / `file_seek` stay exact. Returns `ok(null)`
/// at EOF.
fn n_file_read_line(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "file_read_line";
    arity(WHO, &args, 1)?;
    let id = want_int(&args[0], WHO)?;
    let h = match lookup(id, WHO) {
        Ok(h) => h,
        Err(m) => return err(m),
    };
    let mut f = h.file.lock().unwrap();
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match f.read(&mut byte) {
            Ok(0) => {
                if buf.is_empty() {
                    return ok(Value::Null);
                }
                break;
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                if buf.len() >= MAX_LINE_BYTES {
                    return err(format!(
                        "{}: line exceeds {} byte limit",
                        WHO, MAX_LINE_BYTES
                    ));
                }
                buf.push(byte[0]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return err(format!("{}: {}", WHO, e)),
        }
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    let s = String::from_utf8_lossy(&buf).into_owned();
    ok(Value::Str(s.into()))
}

/// `file_write(id, text_or_bytes)` -> `ok(bytes_written)` / `err(msg)`.
fn n_file_write(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "file_write";
    arity(WHO, &args, 2)?;
    let id = want_int(&args[0], WHO)?;
    let data: Vec<u8> = match &args[1] {
        Value::Str(s) => s.as_bytes().to_vec(),
        Value::Bytes(b) => b.lock().unwrap().clone(),
        other => {
            return Err(format!(
                "{} expects a string or bytes, found {}",
                WHO,
                type_name(other)
            ))
        }
    };
    let h = match lookup(id, WHO) {
        Ok(h) => h,
        Err(m) => return err(m),
    };
    let mut f = h.file.lock().unwrap();
    match f.write_all(&data) {
        Ok(()) => ok(Value::Int(data.len() as i64)),
        Err(e) => err(format!("{}: {}", WHO, e)),
    }
}

/// `file_seek(id, pos, whence?)` -> `ok(new_pos)` / `err(msg)`.
/// `whence`: `"start"` / `0` (default), `"cur"` / `1`, or `"end"` / `2`.
fn n_file_seek(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "file_seek";
    if !(2..=3).contains(&args.len()) {
        return Err(format!(
            "{} expects 2 or 3 arguments, got {}",
            WHO,
            args.len()
        ));
    }
    let id = want_int(&args[0], WHO)?;
    let pos = want_int(&args[1], WHO)?;
    let target = match args.get(2) {
        None | Some(Value::Null) | Some(Value::Int(0)) => {
            if pos < 0 {
                return err(format!("{}: negative seek offset {} from start", WHO, pos));
            }
            SeekFrom::Start(pos as u64)
        }
        Some(Value::Int(1)) => SeekFrom::Current(pos),
        Some(Value::Int(2)) => SeekFrom::End(pos),
        Some(Value::Str(s)) => match s.as_ref() {
            "start" | "set" | "SEEK_SET" => {
                if pos < 0 {
                    return err(format!("{}: negative seek offset {} from start", WHO, pos));
                }
                SeekFrom::Start(pos as u64)
            }
            "cur" | "current" | "SEEK_CUR" => SeekFrom::Current(pos),
            "end" | "SEEK_END" => SeekFrom::End(pos),
            other => {
                return err(format!(
                    "{}: unknown whence {:?} (expected start, cur, or end)",
                    WHO, other
                ))
            }
        },
        Some(other) => {
            return Err(format!(
                "{} expects a string or int whence, found {}",
                WHO,
                type_name(other)
            ))
        }
    };
    let h = match lookup(id, WHO) {
        Ok(h) => h,
        Err(m) => return err(m),
    };
    let mut f = h.file.lock().unwrap();
    match f.seek(target) {
        Ok(new_pos) => ok(Value::Int(new_pos as i64)),
        Err(e) => err(format!("{}: {}", WHO, e)),
    }
}

/// `file_tell(id)` -> `ok(pos)` / `err(msg)`.
fn n_file_tell(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "file_tell";
    arity(WHO, &args, 1)?;
    let id = want_int(&args[0], WHO)?;
    let h = match lookup(id, WHO) {
        Ok(h) => h,
        Err(m) => return err(m),
    };
    let mut f = h.file.lock().unwrap();
    match f.stream_position() {
        Ok(pos) => ok(Value::Int(pos as i64)),
        Err(e) => err(format!("{}: {}", WHO, e)),
    }
}

/// `file_flush(id)` -> `ok(true)` / `err(msg)`.
fn n_file_flush(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "file_flush";
    arity(WHO, &args, 1)?;
    let id = want_int(&args[0], WHO)?;
    let h = match lookup(id, WHO) {
        Ok(h) => h,
        Err(m) => return err(m),
    };
    let mut f = h.file.lock().unwrap();
    match f.flush() {
        Ok(()) => ok(Value::Bool(true)),
        Err(e) => err(format!("{}: {}", WHO, e)),
    }
}

/// `file_fsync(id)` -> `ok(true)` / `err(msg)` (R7.1b: `FlushFileBuffers` on
/// Windows, `fsync` on Unix).
fn n_file_fsync(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "file_fsync";
    arity(WHO, &args, 1)?;
    let id = want_int(&args[0], WHO)?;
    let h = match lookup(id, WHO) {
        Ok(h) => h,
        Err(m) => return err(m),
    };
    let f = h.file.lock().unwrap();
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        #[link(name = "kernel32")]
        extern "system" {
            fn FlushFileBuffers(hfile: *mut std::ffi::c_void) -> i32;
        }
        // SAFETY: `f` is an open `std::fs::File`, so `as_raw_handle()` is a valid OS handle.
        let rc = unsafe { FlushFileBuffers(f.as_raw_handle()) };
        if rc != 0 {
            ok(Value::Bool(true))
        } else {
            err(format!("{}: {}", WHO, std::io::Error::last_os_error()))
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        extern "C" {
            fn fsync(fd: i32) -> i32;
        }
        // SAFETY: `f` is an open `std::fs::File`, so `as_raw_fd()` is a valid fd.
        let rc = unsafe { fsync(f.as_raw_fd()) };
        if rc == 0 {
            ok(Value::Bool(true))
        } else {
            err(format!("{}: {}", WHO, std::io::Error::last_os_error()))
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        match f.sync_all() {
            Ok(()) => ok(Value::Bool(true)),
            Err(e) => err(format!("{}: {}", WHO, e)),
        }
    }
}

#[cfg(windows)]
mod win_lock {
    #[repr(C)]
    pub struct Overlapped {
        pub internal: usize,
        pub internal_high: usize,
        pub offset: u32,
        pub offset_high: u32,
        pub hevent: *mut std::ffi::c_void,
    }

    pub const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x0000_0002;

    #[link(name = "kernel32")]
    extern "system" {
        pub fn LockFileEx(
            hfile: *mut std::ffi::c_void,
            dwflags: u32,
            dwreserved: u32,
            nnumberofbytestolocklow: u32,
            nnumberofbytestolockhigh: u32,
            lpoverlapped: *mut Overlapped,
        ) -> i32;
        pub fn UnlockFileEx(
            hfile: *mut std::ffi::c_void,
            dwreserved: u32,
            nnumberofbytestounlocklow: u32,
            nnumberofbytestounlockhigh: u32,
            lpoverlapped: *mut Overlapped,
        ) -> i32;
    }
}

/// `file_lock(id, exclusive?)` -> `ok(true)` / `err(msg)` (R7.1b: `LockFileEx`
/// on Windows, `flock` on Unix).
fn n_file_lock(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "file_lock";
    if !(1..=2).contains(&args.len()) {
        return Err(format!(
            "{} expects 1 or 2 arguments, got {}",
            WHO,
            args.len()
        ));
    }
    let id = want_int(&args[0], WHO)?;
    let exclusive = match args.get(1) {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => *b,
        Some(other) => {
            return Err(format!(
                "{} expects a boolean exclusive flag, found {}",
                WHO,
                type_name(other)
            ))
        }
    };
    let h = match lookup(id, WHO) {
        Ok(h) => h,
        Err(m) => return err(m),
    };
    let f = h.file.lock().unwrap();
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        let mut ov = win_lock::Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            hevent: std::ptr::null_mut(),
        };
        let flags = if exclusive {
            win_lock::LOCKFILE_EXCLUSIVE_LOCK
        } else {
            0
        };
        // SAFETY: `f` is an open file handle and `ov` is a valid zeroed OVERLAPPED struct.
        let rc = unsafe {
            win_lock::LockFileEx(f.as_raw_handle(), flags, 0, u32::MAX, u32::MAX, &mut ov)
        };
        if rc != 0 {
            ok(Value::Bool(true))
        } else {
            err(format!("{}: {}", WHO, std::io::Error::last_os_error()))
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        const LOCK_SH: i32 = 1;
        const LOCK_EX: i32 = 2;
        extern "C" {
            fn flock(fd: i32, operation: i32) -> i32;
        }
        let op = if exclusive { LOCK_EX } else { LOCK_SH };
        // SAFETY: `f` is an open file descriptor.
        let rc = unsafe { flock(f.as_raw_fd(), op) };
        if rc == 0 {
            ok(Value::Bool(true))
        } else {
            err(format!("{}: {}", WHO, std::io::Error::last_os_error()))
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        drop(f);
        let _ = exclusive;
        err(format!("{}: not supported on this platform", WHO))
    }
}

/// `file_unlock(id)` -> `ok(true)` / `err(msg)`.
fn n_file_unlock(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "file_unlock";
    arity(WHO, &args, 1)?;
    let id = want_int(&args[0], WHO)?;
    let h = match lookup(id, WHO) {
        Ok(h) => h,
        Err(m) => return err(m),
    };
    let f = h.file.lock().unwrap();
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        let mut ov = win_lock::Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            hevent: std::ptr::null_mut(),
        };
        // SAFETY: `f` is an open file handle and `ov` is a valid zeroed OVERLAPPED struct.
        let rc =
            unsafe { win_lock::UnlockFileEx(f.as_raw_handle(), 0, u32::MAX, u32::MAX, &mut ov) };
        if rc != 0 {
            ok(Value::Bool(true))
        } else {
            err(format!("{}: {}", WHO, std::io::Error::last_os_error()))
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        const LOCK_UN: i32 = 8;
        extern "C" {
            fn flock(fd: i32, operation: i32) -> i32;
        }
        // SAFETY: `f` is an open file descriptor.
        let rc = unsafe { flock(f.as_raw_fd(), LOCK_UN) };
        if rc == 0 {
            ok(Value::Bool(true))
        } else {
            err(format!("{}: {}", WHO, std::io::Error::last_os_error()))
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        drop(f);
        err(format!("{}: not supported on this platform", WHO))
    }
}

/// `file_close(id)` -> `ok(true)` / `err(msg)`.
fn n_file_close(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "file_close";
    arity(WHO, &args, 1)?;
    let id = want_int(&args[0], WHO)?;
    let removed = table().lock().unwrap().remove(&id);
    match removed {
        Some(_) => ok(Value::Bool(true)),
        None => err(format!(
            "{}: handle {} is not open (already closed or invalid)",
            WHO, id
        )),
    }
}
