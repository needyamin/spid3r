//! WebAssembly & In-Browser execution runtime for Spid3r.
//!
//! Provides zero-dependency, C-ABI and Rust APIs to check, evaluate, and
//! format Spid3r programs in browser environments, Node.js, Deno, or Workers.

use crate::fmt;
use crate::interpreter::Interp;
use crate::modules;
use crate::parser;
use crate::semantic;
use crate::stdlib;
use crate::value::display_value;
use crate::vm::Vm;
use crate::{LANGUAGE_VERSION, VERSION};
use std::sync::Arc;

/// Escape a string for JSON output without external crates.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0C' => out.push_str("\\f"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Version information as a JSON string.
pub fn spid3r_version_json() -> String {
    format!(
        "{{\"version\":\"{}\",\"language_version\":\"{}\"}}",
        json_escape(VERSION),
        json_escape(LANGUAGE_VERSION)
    )
}

/// Lex, parse, and semantic check source code without executing.
pub fn spid3r_check_json(src: &str) -> String {
    let file_id: Arc<str> = Arc::from("<browser>");
    let program = match parser::parse_file(src, file_id.clone()) {
        Ok(p) => p,
        Err(d) => {
            let line = d.span.as_ref().map(|s| s.line).unwrap_or(0);
            let col = d.span.as_ref().map(|s| s.col).unwrap_or(0);
            return format!(
                "{{\"ok\":false,\"phase\":\"{:?}\",\"error\":\"{}\",\"line\":{},\"col\":{}}}",
                d.phase,
                json_escape(&d.message),
                line,
                col
            );
        }
    };

    let has_main = program.stmts.iter().any(|s| match s {
        crate::ast::Stmt::Fn(f) => f.name.as_deref() == Some("main"),
        _ => false,
    });

    if let Err(d) = semantic::check_file(&program, has_main, "<browser>") {
        let line = d.span.as_ref().map(|s| s.line).unwrap_or(0);
        let col = d.span.as_ref().map(|s| s.col).unwrap_or(0);
        return format!(
            "{{\"ok\":false,\"phase\":\"{:?}\",\"error\":\"{}\",\"line\":{},\"col\":{}}}",
            d.phase,
            json_escape(&d.message),
            line,
            col
        );
    }

    "{\"ok\":true}".to_string()
}

/// Run a program on the tree interpreter or bytecode VM and capture output.
pub fn spid3r_eval_json(src: &str, engine: &str) -> String {
    stdlib::start_capture();
    let file_id: Arc<str> = Arc::from("<browser>");

    let full_src = if !src.contains("fn main") {
        format!("{}\nfn main() {{}}\n", src)
    } else {
        src.to_string()
    };

    let program = match parser::parse_file(&full_src, file_id.clone()) {
        Ok(p) => p,
        Err(d) => {
            let stdout = stdlib::take_capture();
            let line = d.span.as_ref().map(|s| s.line).unwrap_or(0);
            let col = d.span.as_ref().map(|s| s.col).unwrap_or(0);
            return format!(
                "{{\"ok\":false,\"phase\":\"{:?}\",\"error\":\"{}\",\"line\":{},\"col\":{},\"stdout\":\"{}\"}}",
                d.phase,
                json_escape(&d.message),
                line,
                col,
                json_escape(&stdout)
            );
        }
    };

    let has_main = program.stmts.iter().any(|s| match s {
        crate::ast::Stmt::Fn(f) => f.name.as_deref() == Some("main"),
        _ => false,
    });

    if let Err(d) = semantic::check_file(&program, has_main, "<browser>") {
        let stdout = stdlib::take_capture();
        let line = d.span.as_ref().map(|s| s.line).unwrap_or(0);
        let col = d.span.as_ref().map(|s| s.col).unwrap_or(0);
        return format!(
            "{{\"ok\":false,\"phase\":\"{:?}\",\"error\":\"{}\",\"line\":{},\"col\":{},\"stdout\":\"{}\"}}",
            d.phase,
            json_escape(&d.message),
            line,
            col,
            json_escape(&stdout)
        );
    }

    let dirs = modules::default_module_dirs();
    let result = if engine == "vm" {
        let mut vm = Vm::new(file_id, dirs);
        vm.run_program(&program)
    } else {
        let interp = Interp::new(file_id, dirs);
        interp.run_program_inline(&program)
    };

    let stdout = stdlib::take_capture();
    match result {
        Ok(code) => {
            format!(
                "{{\"ok\":true,\"exit_code\":{},\"stdout\":\"{}\"}}",
                code,
                json_escape(&stdout)
            )
        }
        Err(d) => {
            let line = d.span.as_ref().map(|s| s.line).unwrap_or(0);
            let col = d.span.as_ref().map(|s| s.col).unwrap_or(0);
            let thrown_str = d.thrown.as_ref().map(display_value).unwrap_or_default();
            format!(
                "{{\"ok\":false,\"phase\":\"{:?}\",\"error\":\"{}\",\"thrown\":\"{}\",\"line\":{},\"col\":{},\"stdout\":\"{}\"}}",
                d.phase,
                json_escape(&d.message),
                json_escape(&thrown_str),
                line,
                col,
                json_escape(&stdout)
            )
        }
    }
}

/// Format source code canonically.
pub fn spid3r_format_json(src: &str) -> String {
    let file_id: Arc<str> = Arc::from("<browser>");
    match fmt::format_source(src, &file_id) {
        Ok(formatted) => {
            format!("{{\"ok\":true,\"code\":\"{}\"}}", json_escape(&formatted))
        }
        Err(d) => {
            format!("{{\"ok\":false,\"error\":\"{}\"}}", json_escape(&d.message))
        }
    }
}

// ---------------------------------------------------------------- C-ABI API
// Zero-dependency C-ABI interface for WebAssembly:
// All returned buffers are prepended with a 4-byte little-endian length.

fn alloc_payload(json: String) -> *mut u8 {
    let bytes = json.into_bytes();
    let len = bytes.len() as u32;
    let mut buf = Vec::with_capacity(4 + bytes.len());
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(&bytes);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

#[no_mangle]
pub extern "C" fn spid3r_wasm_alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(len);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Free a heap-allocated buffer previously allocated by `spid3r_wasm_alloc` or returned by a WebAssembly entrypoint.
///
/// # Safety
/// `ptr` must point to a buffer allocated by the same allocator with length `len`,
/// and must not be used after this call.
#[no_mangle]
pub unsafe extern "C" fn spid3r_wasm_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len > 0 {
        let _ = Vec::from_raw_parts(ptr, len, len);
    }
}

/// Evaluate Spid3r source code and return a pointer to JSON results.
///
/// # Safety
/// `src_ptr` must point to `src_len` readable bytes and `eng_ptr` to `eng_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn spid3r_wasm_eval(
    src_ptr: *const u8,
    src_len: usize,
    eng_ptr: *const u8,
    eng_len: usize,
) -> *mut u8 {
    let src = if src_ptr.is_null() || src_len == 0 {
        ""
    } else {
        std::str::from_utf8(std::slice::from_raw_parts(src_ptr, src_len)).unwrap_or("")
    };
    let eng = if eng_ptr.is_null() || eng_len == 0 {
        "tree"
    } else {
        std::str::from_utf8(std::slice::from_raw_parts(eng_ptr, eng_len)).unwrap_or("tree")
    };
    alloc_payload(spid3r_eval_json(src, eng))
}

/// Check Spid3r source code for syntax and semantic errors.
///
/// # Safety
/// `src_ptr` must point to `src_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn spid3r_wasm_check(src_ptr: *const u8, src_len: usize) -> *mut u8 {
    let src = if src_ptr.is_null() || src_len == 0 {
        ""
    } else {
        std::str::from_utf8(std::slice::from_raw_parts(src_ptr, src_len)).unwrap_or("")
    };
    alloc_payload(spid3r_check_json(src))
}

/// Format Spid3r source code canonically.
///
/// # Safety
/// `src_ptr` must point to `src_len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn spid3r_wasm_format(src_ptr: *const u8, src_len: usize) -> *mut u8 {
    let src = if src_ptr.is_null() || src_len == 0 {
        ""
    } else {
        std::str::from_utf8(std::slice::from_raw_parts(src_ptr, src_len)).unwrap_or("")
    };
    alloc_payload(spid3r_format_json(src))
}

#[no_mangle]
pub extern "C" fn spid3r_wasm_version() -> *mut u8 {
    alloc_payload(spid3r_version_json())
}
