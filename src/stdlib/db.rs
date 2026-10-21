//! SQLite database driver (audit backlog item: persistence for CMS/ERP).
//!
//! `db_open(path)` loads the real SQLite C library at runtime with the OS
//! loader — **zero crate dependencies, nothing downloaded at build time** —
//! and returns a `Value::Database` handle with parameterized methods:
//!
//! ```text
//! let db = db_open("app.db")
//! db.exec("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
//! db.run("INSERT INTO users(name) VALUES (?)", ["ada"])
//! let rows = db.query("SELECT * FROM users WHERE id = ?", [1])
//! db.close()
//! ```
//!
//! Search order for the shared library: `SPID3R_SQLITE_PATH`, then the
//! running executable's directory, then the OS default search paths. A failed
//! load produces a catchable error that names every location tried.
//!
//! Thread safety: every access to the connection is serialized behind the
//! handle's `Mutex`, and the connection is opened with SQLITE_OPEN_FULLMUTEX,
//! so the raw pointer inside `DbData` is sound to move across threads.

#[cfg(not(target_arch = "wasm32"))]
use crate::value::new_ref;
use crate::value::{NativeCtx, Value};
#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashMap;
use std::ffi::c_void;
#[cfg(not(target_arch = "wasm32"))]
use std::ffi::CString;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------- loading

/// The loaded SQLite C API, as function pointers resolved once at load time.
#[cfg(not(target_arch = "wasm32"))]
pub struct SqliteApi {
    pub initialize: unsafe extern "C" fn() -> i32,
    pub open_v2: unsafe extern "C" fn(*const u8, *mut *mut c_void, i32, *const u8) -> i32,
    pub close_v2: unsafe extern "C" fn(*mut c_void) -> i32,
    pub prepare_v2:
        unsafe extern "C" fn(*mut c_void, *const u8, i32, *mut *mut c_void, *mut *const u8) -> i32,
    pub step: unsafe extern "C" fn(*mut c_void) -> i32,
    pub finalize: unsafe extern "C" fn(*mut c_void) -> i32,
    pub bind_int64: unsafe extern "C" fn(*mut c_void, i32, i64) -> i32,
    pub bind_double: unsafe extern "C" fn(*mut c_void, i32, f64) -> i32,
    pub bind_text: unsafe extern "C" fn(*mut c_void, i32, *const u8, i32, isize) -> i32,
    pub bind_blob: unsafe extern "C" fn(*mut c_void, i32, *const u8, i32, isize) -> i32,
    pub bind_null: unsafe extern "C" fn(*mut c_void, i32) -> i32,
    pub column_count: unsafe extern "C" fn(*mut c_void) -> i32,
    pub column_type: unsafe extern "C" fn(*mut c_void, i32) -> i32,
    pub column_int64: unsafe extern "C" fn(*mut c_void, i32) -> i64,
    pub column_double: unsafe extern "C" fn(*mut c_void, i32) -> f64,
    pub column_text: unsafe extern "C" fn(*mut c_void, i32) -> *const u8,
    pub column_blob: unsafe extern "C" fn(*mut c_void, i32) -> *const u8,
    pub column_bytes: unsafe extern "C" fn(*mut c_void, i32) -> i32,
    pub column_name: unsafe extern "C" fn(*mut c_void, i32) -> *const u8,
    pub errmsg: unsafe extern "C" fn(*mut c_void) -> *const u8,
    pub changes: unsafe extern "C" fn(*mut c_void) -> i32,
    pub last_insert_rowid: unsafe extern "C" fn(*mut c_void) -> i64,
}

#[cfg(not(target_arch = "wasm32"))]
#[cfg(not(target_arch = "wasm32"))]
const SQLITE_OK: i32 = 0;
#[cfg(not(target_arch = "wasm32"))]
const SQLITE_ROW: i32 = 100;
#[cfg(not(target_arch = "wasm32"))]
const SQLITE_DONE: i32 = 101;
/// SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_FULLMUTEX.
#[cfg(not(target_arch = "wasm32"))]
const OPEN_FLAGS: i32 = 0x2 | 0x4 | 0x1_0000;
/// SQLITE_TRANSIENT: SQLite copies the bound buffer.
#[cfg(not(target_arch = "wasm32"))]
const TRANSIENT: isize = -1;

#[cfg(not(target_arch = "wasm32"))]
const COL_INT: i32 = 1;
#[cfg(not(target_arch = "wasm32"))]
const COL_FLOAT: i32 = 2;
#[cfg(not(target_arch = "wasm32"))]
const COL_TEXT: i32 = 3;
#[cfg(not(target_arch = "wasm32"))]
const COL_BLOB: i32 = 5;

#[cfg(all(not(target_arch = "wasm32"), windows))]
mod loader {
    use super::*;
    pub type Handle = *mut c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryW(lpfilename: *const u16) -> Handle;
        fn GetProcAddress(hmodule: Handle, lpprocname: *const u8) -> Handle;
    }

    pub fn library_names() -> &'static [&'static str] {
        &["sqlite3.dll"]
    }

    pub fn open(path: &std::path::Path) -> Result<Handle, String> {
        let wide: Vec<u16> = path
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let h = unsafe { LoadLibraryW(wide.as_ptr()) };
        if h.is_null() {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            Ok(h)
        }
    }

    pub fn symbol(h: Handle, name: &str) -> Option<*mut c_void> {
        let c = match CString::new(name) {
            Ok(c) => c,
            Err(_) => return None,
        };
        let p = unsafe { GetProcAddress(h, c.as_ptr() as *const u8) };
        if p.is_null() {
            None
        } else {
            Some(p)
        }
    }
}

#[cfg(all(not(target_arch = "wasm32"), unix))]
mod loader {
    use super::*;
    pub type Handle = *mut c_void;

    #[cfg_attr(target_os = "linux", link(name = "dl"))]
    extern "C" {
        fn dlopen(filename: *const u8, flag: i32) -> Handle;
        fn dlsym(handle: Handle, symbol: *const u8) -> Handle;
    }

    pub fn library_names() -> &'static [&'static str] {
        if cfg!(target_os = "macos") {
            &["libsqlite3.dylib", "libsqlite3.0.dylib"]
        } else {
            &["libsqlite3.so.0", "libsqlite3.so"]
        }
    }

    pub fn open(path: &std::path::Path) -> Result<Handle, String> {
        let c = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| "library path contains a NUL byte".to_string())?;
        let h = unsafe { dlopen(c.as_ptr() as *const u8, 2) }; // RTLD_NOW
        if h.is_null() {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            Ok(h)
        }
    }

    pub fn symbol(h: Handle, name: &str) -> Option<*mut c_void> {
        let c = CString::new(name).ok()?;
        let p = unsafe { dlsym(h, c.as_ptr() as *const u8) };
        if p.is_null() {
            None
        } else {
            Some(p)
        }
    }
}

/// Candidate locations for the SQLite shared library, in order.
#[cfg(not(target_arch = "wasm32"))]
fn candidate_paths() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = std::env::var("SPID3R_SQLITE_PATH") {
        if !p.trim().is_empty() {
            out.push(std::path::PathBuf::from(p));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in loader::library_names() {
                out.push(dir.join(name));
            }
        }
    }
    for name in loader::library_names() {
        out.push(std::path::PathBuf::from(name));
    }
    out
}

#[cfg(not(target_arch = "wasm32"))]
fn load_sqlite() -> Result<&'static SqliteApi, String> {
    static API: OnceLock<Result<SqliteApi, String>> = OnceLock::new();
    let result = API.get_or_init(|| {
        let mut tried: Vec<String> = Vec::new();
        let mut handle: Option<loader::Handle> = None;
        for path in candidate_paths() {
            if !path.exists() {
                tried.push(format!("{} (not found)", path.display()));
                continue;
            }
            match loader::open(&path) {
                Ok(h) => {
                    handle = Some(h);
                    break;
                }
                Err(e) => tried.push(format!("{} (load failed: {})", path.display(), e)),
            }
        }
        let handle = match handle {
            Some(h) => h,
            None => {
                return Err(format!(
                    "cannot load the SQLite library (sqlite3). Tried: {}. \
                     Put sqlite3.dll/libsqlite3.so next to the spid3r executable, \
                     or set SPID3R_SQLITE_PATH to its full path.",
                    if tried.is_empty() {
                        "no candidates".to_string()
                    } else {
                        tried.join("; ")
                    }
                ))
            }
        };
        /// Resolve one symbol and transmute it to the caller-inferred type.
        fn sym_as<T>(handle: loader::Handle, name: &str) -> Result<T, String>
        where
            T: Copy,
        {
            let p = loader::symbol(handle, name)
                .ok_or_else(|| format!("SQLite library is missing symbol {}", name))?;
            Ok(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&p) })
        }
        Ok(SqliteApi {
            initialize: sym_as(handle, "sqlite3_initialize")?,
            open_v2: sym_as(handle, "sqlite3_open_v2")?,
            close_v2: sym_as(handle, "sqlite3_close_v2")?,
            prepare_v2: sym_as(handle, "sqlite3_prepare_v2")?,
            step: sym_as(handle, "sqlite3_step")?,
            finalize: sym_as(handle, "sqlite3_finalize")?,
            bind_int64: sym_as(handle, "sqlite3_bind_int64")?,
            bind_double: sym_as(handle, "sqlite3_bind_double")?,
            bind_text: sym_as(handle, "sqlite3_bind_text")?,
            bind_blob: sym_as(handle, "sqlite3_bind_blob")?,
            bind_null: sym_as(handle, "sqlite3_bind_null")?,
            column_count: sym_as(handle, "sqlite3_column_count")?,
            column_type: sym_as(handle, "sqlite3_column_type")?,
            column_int64: sym_as(handle, "sqlite3_column_int64")?,
            column_double: sym_as(handle, "sqlite3_column_double")?,
            column_text: sym_as(handle, "sqlite3_column_text")?,
            column_blob: sym_as(handle, "sqlite3_column_blob")?,
            column_bytes: sym_as(handle, "sqlite3_column_bytes")?,
            column_name: sym_as(handle, "sqlite3_column_name")?,
            errmsg: sym_as(handle, "sqlite3_errmsg")?,
            changes: sym_as(handle, "sqlite3_changes")?,
            last_insert_rowid: sym_as(handle, "sqlite3_last_insert_rowid")?,
        })
    });
    let api = result.as_ref().map_err(|e| e.clone())?;
    // Explicit global initialization before the first open. sqlite3_open
    // would do this lazily, but at least one shipped SQLite build (3.53.1,
    // as shipped with CPython on Windows) faults when the lazy path runs in
    // a host process — an explicit call is documented as always safe and
    // idempotent.
    let rc = unsafe { (api.initialize)() };
    if rc != SQLITE_OK {
        return Err(format!("sqlite3_initialize failed (rc={})", rc));
    }
    Ok(api)
}

// ------------------------------------------------------------- the handle

/// An open SQLite connection. `Drop` closes it; every method locks.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub struct DbData {
    pub path: String,
    db: *mut c_void,
    closed: bool,
}

// SAFETY: the connection is opened with SQLITE_OPEN_FULLMUTEX and all access
// is serialized behind the handle's `Arc<Mutex<DbData>>`.
unsafe impl Send for DbData {}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for DbData {
    fn drop(&mut self) {
        if !self.closed && !self.db.is_null() {
            // The API is always loaded when a handle exists; on the
            // impossible failure path there is nothing to close.
            if let Ok(api) = load_sqlite() {
                unsafe { (api.close_v2)(self.db) };
            }
        }
        self.db = std::ptr::null_mut();
    }
}

// ------------------------------------------------------------- statements

/// One SQL statement batch's execution result.
#[cfg(not(target_arch = "wasm32"))]
struct StepOutcome {
    changes: i64,
    last_id: i64,
}

#[cfg(not(target_arch = "wasm32"))]
fn errmsg_of(api: &SqliteApi, db: *mut c_void) -> String {
    let p = unsafe { (api.errmsg)(db) };
    if p.is_null() {
        "unknown SQLite error".to_string()
    } else {
        String::from_utf8_lossy(unsafe { std::ffi::CStr::from_ptr(p as *const i8) }.to_bytes())
            .into_owned()
    }
}

/// Prepare, bind, and run every statement in `sql` (a `?`-tail loop, so
/// multi-statement DDL works without `sqlite3_exec`). `on_row` fires per
/// result row (used by `query`).
#[cfg(not(target_arch = "wasm32"))]
fn run_sql(
    db: *mut c_void,
    sql: &str,
    params: &[Value],
    on_row: &mut dyn FnMut(&SqliteApi, *mut c_void),
) -> Result<StepOutcome, String> {
    let api = load_sqlite()?;
    let sql_c = CString::new(sql.as_bytes()).map_err(|_| "SQL contains a NUL byte".to_string())?;
    let mut tail: *const u8 = sql_c.as_ptr() as *const u8;
    let end = unsafe { tail.add(sql.len()) };
    let mut total = StepOutcome {
        changes: 0,
        last_id: 0,
    };
    while tail < end {
        let mut stmt: *mut c_void = std::ptr::null_mut();
        let mut next: *const u8 = std::ptr::null();
        let rc = unsafe {
            (api.prepare_v2)(
                db,
                tail,
                (end as usize - tail as usize) as i32,
                &mut stmt,
                &mut next,
            )
        };
        if rc != SQLITE_OK {
            return Err(errmsg_of(api, db));
        }
        if stmt.is_null() {
            break; // only whitespace/comment remained
        }
        // Bind (1-based indices).
        for (i, p) in params.iter().enumerate() {
            let idx = (i + 1) as i32;
            let rc = match p {
                Value::Null => unsafe { (api.bind_null)(stmt, idx) },
                Value::Int(v) => unsafe { (api.bind_int64)(stmt, idx, *v) },
                Value::Float(v) => unsafe { (api.bind_double)(stmt, idx, *v) },
                Value::Str(s) => unsafe {
                    (api.bind_text)(stmt, idx, s.as_ptr(), s.len() as i32, TRANSIENT)
                },
                Value::Bytes(b) => {
                    let data = b.lock().unwrap().clone();
                    unsafe {
                        (api.bind_blob)(stmt, idx, data.as_ptr(), data.len() as i32, TRANSIENT)
                    }
                }
                other => {
                    unsafe { (api.finalize)(stmt) };
                    return Err(format!(
                        "cannot bind a {} value as a SQL parameter (use null/int/float/string/bytes)",
                        crate::value::type_name(other)
                    ));
                }
            };
            if rc != SQLITE_OK {
                let msg = errmsg_of(api, db);
                unsafe { (api.finalize)(stmt) };
                return Err(msg);
            }
        }
        loop {
            let rc = unsafe { (api.step)(stmt) };
            if rc == SQLITE_ROW {
                on_row(api, stmt);
            } else if rc == SQLITE_DONE {
                break;
            } else {
                let msg = errmsg_of(api, db);
                unsafe { (api.finalize)(stmt) };
                return Err(msg);
            }
        }
        unsafe { (api.finalize)(stmt) };
        total.changes += unsafe { (api.changes)(db) } as i64;
        total.last_id = unsafe { (api.last_insert_rowid)(db) };
        tail = next;
        if next.is_null() {
            break;
        }
    }
    Ok(total)
}

#[cfg(not(target_arch = "wasm32"))]
fn row_value(api: &SqliteApi, stmt: *mut c_void, col: i32) -> Value {
    match unsafe { (api.column_type)(stmt, col) } {
        COL_INT => Value::Int(unsafe { (api.column_int64)(stmt, col) }),
        COL_FLOAT => Value::Float(unsafe { (api.column_double)(stmt, col) }),
        COL_TEXT => {
            let len = unsafe { (api.column_bytes)(stmt, col) };
            let p = unsafe { (api.column_text)(stmt, col) };
            if p.is_null() || len <= 0 {
                return Value::Str(String::new().into());
            }
            let bytes = unsafe { std::slice::from_raw_parts(p, len as usize) };
            Value::Str(String::from_utf8_lossy(bytes).into_owned().into())
        }
        COL_BLOB => {
            let len = unsafe { (api.column_bytes)(stmt, col) };
            let p = unsafe { (api.column_blob)(stmt, col) };
            if p.is_null() || len <= 0 {
                return Value::Bytes(new_ref(Vec::new()));
            }
            let bytes = unsafe { std::slice::from_raw_parts(p, len as usize) }.to_vec();
            Value::Bytes(new_ref(bytes))
        }
        _ => Value::Null,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn column_names(api: &SqliteApi, stmt: *mut c_void) -> Vec<String> {
    let n = unsafe { (api.column_count)(stmt) };
    (0..n)
        .map(|c| {
            let p = unsafe { (api.column_name)(stmt, c) };
            if p.is_null() {
                format!("col{}", c)
            } else {
                String::from_utf8_lossy(
                    unsafe { std::ffi::CStr::from_ptr(p as *const i8) }.to_bytes(),
                )
                .into_owned()
            }
        })
        .collect()
}

// ---------------------------------------------------------------- methods

/// The `Value::Database` method table: `query`, `run`, `exec`, `close`.
#[cfg(not(target_arch = "wasm32"))]
pub fn database_method(
    d: &Arc<Mutex<DbData>>,
    name: &str,
    args: &[Value],
) -> Result<Value, String> {
    match name {
        "query" | "run" | "exec" => {
            if args.is_empty() || args.len() > 2 {
                return Err(format!("{} takes (sql) or (sql, params)", name));
            }
            let sql = match &args[0] {
                Value::Str(s) => s.clone(),
                other => {
                    return Err(format!(
                        "{} expects a SQL string, found {}",
                        name,
                        crate::value::type_name(other)
                    ))
                }
            };
            let params: Vec<Value> = match args.get(1) {
                None => Vec::new(),
                Some(Value::List(l)) => l.lock().unwrap().items.clone(),
                Some(Value::Tuple(t)) => t.lock().unwrap().clone(),
                Some(other) => {
                    return Err(format!(
                        "{} params must be a list, found {}",
                        name,
                        crate::value::type_name(other)
                    ))
                }
            };
            let guard = d.lock().unwrap();
            if guard.closed {
                return Err("database is closed".to_string());
            }
            let db = guard.db;
            let mut rows: Vec<Value> = Vec::new();
            let outcome = run_sql(db, &sql, &params, &mut |api, stmt| {
                let names = column_names(api, stmt);
                let n = unsafe { (api.column_count)(stmt) };
                let mut m: HashMap<String, Value> = HashMap::with_capacity(n as usize);
                for c in 0..n {
                    m.insert(names[c as usize].clone(), row_value(api, stmt, c));
                }
                rows.push(Value::Map(new_ref(
                    crate::value::collections::MapData::from_entries(
                        m.into_iter()
                            .map(|(k, v)| (crate::value::MapKey::Str(k.into()), v))
                            .collect(),
                    ),
                )));
            })?;
            if name == "query" {
                Ok(Value::List(new_ref(
                    crate::value::collections::seq::VecData { items: rows },
                )))
            } else {
                Ok(Value::Map(new_ref(
                    crate::value::collections::MapData::from_entries(vec![
                        (
                            crate::value::MapKey::Str("changes".into()),
                            Value::Int(outcome.changes),
                        ),
                        (
                            crate::value::MapKey::Str("last_id".into()),
                            Value::Int(outcome.last_id),
                        ),
                    ]),
                )))
            }
        }
        "close" => {
            let mut guard = d.lock().unwrap();
            if !guard.closed {
                unsafe { (load_sqlite()?.close_v2)(guard.db) };
                guard.db = std::ptr::null_mut();
                guard.closed = true;
            }
            Ok(Value::Null)
        }
        other => Err(format!("database has no method '{}'", other)),
    }
}

// ------------------------------------------------------------- db_open

fn n_db_open(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = args;
        Err("db_open is not supported on WebAssembly (no OS file system)".to_string())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if args.len() != 1 {
            return Err(format!(
                "db_open expects 1 argument (path), got {}",
                args.len()
            ));
        }
        let path = match &args[0] {
            Value::Str(s) => s.clone(),
            other => {
                return Err(format!(
                    "db_open expects a path string, found {}",
                    crate::value::type_name(other)
                ))
            }
        };
        let api = load_sqlite()?;
        let cpath = CString::new(path.as_bytes())
            .map_err(|_| "database path contains a NUL byte".to_string())?;
        let mut db: *mut c_void = std::ptr::null_mut();
        let rc = unsafe {
            (api.open_v2)(
                cpath.as_ptr() as *const u8,
                &mut db,
                OPEN_FLAGS,
                std::ptr::null(),
            )
        };
        if rc != SQLITE_OK {
            let msg = if db.is_null() {
                format!("cannot open database '{}' (rc={})", path, rc)
            } else {
                let m = errmsg_of(api, db);
                unsafe { (api.close_v2)(db) };
                m
            };
            return Err(msg);
        }
        // Sensible defaults for application workloads.
        let _ = run_sql(db, "PRAGMA journal_mode=WAL;", &[], &mut |_, _| {});
        let _ = run_sql(db, "PRAGMA foreign_keys=ON;", &[], &mut |_, _| {});
        Ok(Value::Database(Arc::new(Mutex::new(DbData {
            path: path.to_string(),
            db,
            closed: false,
        }))))
    }
}

/// On wasm the driver is absent: every method is a clean error.
#[cfg(target_arch = "wasm32")]
pub fn database_method(
    _d: &Arc<Mutex<DbData>>,
    name: &str,
    _args: &[Value],
) -> Result<Value, String> {
    let _ = name;
    Err("databases are not available on WebAssembly".to_string())
}

/// The `db_open` builtin (registered like every other native).
pub fn native_defs() -> Vec<crate::stdlib::NativeDef> {
    vec![("db_open", n_db_open)]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The loader resolves symbols and a connection opens/closes cleanly.
    /// Skips (with a notice) when no SQLite library is installed — CI runners
    /// may not have one; the test must never fail because of that.
    #[test]
    fn open_and_close_direct() {
        let api = match load_sqlite() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("skipping: {}", e);
                return;
            }
        };
        let cpath = CString::new("target/db_direct_test.db").unwrap();
        let mut db: *mut c_void = std::ptr::null_mut();
        let rc = unsafe {
            (api.open_v2)(
                cpath.as_ptr() as *const u8,
                &mut db,
                OPEN_FLAGS,
                std::ptr::null(),
            )
        };
        assert_eq!(rc, SQLITE_OK, "open_v2 failed: {}", errmsg_of(api, db));
        // Round-trip a statement through the shared run_sql path.
        let outcome = run_sql(
            db,
            "CREATE TABLE IF NOT EXISTS t (x INT); INSERT INTO t VALUES (42);",
            &[],
            &mut |_, _| {},
        )
        .expect("statement failed");
        assert_eq!(outcome.changes, 1);
        let mut seen: Vec<i64> = Vec::new();
        run_sql(
            db,
            "SELECT x FROM t",
            &[],
            &mut |api, stmt| match row_value(api, stmt, 0) {
                Value::Int(v) => seen.push(v),
                other => panic!("expected an int, got {:?}", crate::value::type_name(&other)),
            },
        )
        .expect("select failed");
        assert_eq!(seen, vec![42]);
        unsafe { (api.close_v2)(db) };
    }
}
