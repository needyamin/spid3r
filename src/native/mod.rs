//! Native backend: compile Spid3r programs to real machine code.
//!
//! Two halves:
//! - [`rt`] — the runtime: `extern "C"` trampolines + `spid3r_rt_start`.
//!   These live inside the `staticlib` we produce (`libspid3r.a` / `spid3r.lib`)
//!   and are what AOT-compiled binaries call into. Zero dependencies.
//! - `ir` / `link` (behind the `native` feature) — LLVM IR generation via
//!   inkwell, object emission, and final linking of the executable.

#![allow(dead_code)] // trampolines are used by *generated* code, not always by this crate

pub mod rt;

#[cfg(feature = "native")]
pub mod ir;

#[cfg(feature = "native")]
pub mod link;

#[cfg(feature = "native")]
pub mod modules;

#[cfg(feature = "native")]
pub mod llvm;

use crate::diagnostics::{Diagnostic, Phase};
use std::path::{Path, PathBuf};

/// Platform-specific filename of the Spid3r native runtime static library.
///
/// The name follows the **lib target** (`spid3r_lang`), not the package or the
/// CLI binary: the lib target was renamed so its `cdylib` artifact no longer
/// collides with the binary's `spid3r.pdb` on MSVC (audit F2). Installs created
/// before the rename still ship `spid3r.lib`, so the search below also tries the
/// legacy name.
pub const fn runtime_lib_name() -> &'static str {
    if cfg!(windows) {
        "spid3r_lang.lib"
    } else {
        "libspid3r_lang.a"
    }
}

/// Filename the runtime static library had before the lib rename (2026-10-21).
pub const fn legacy_runtime_lib_name() -> &'static str {
    if cfg!(windows) {
        "spid3r.lib"
    } else {
        "libspid3r.a"
    }
}

/// Build the ordered candidate paths searched for the native runtime library
/// (`spid3r_lang.lib` on Windows, `libspid3r_lang.a` on Unix).
///
/// Search order:
/// 1. `SPID3R_RUNTIME_LIB` (direct file path, or directory containing the library)
/// 2. `SPID3R_LIB_DIR` and `MATRIX_RLIB_DIR` (directory overrides)
/// 3. Relative to the running executable (`<exe_dir>`, `<exe_dir>/deps`,
///    `<exe_dir>/lib`, `<exe_dir>/..`, `<exe_dir>/../lib`, `<exe_dir>/../lib/spid3r`)
/// 4. Relative to the working directory (`target/release` and `target/debug`)
///
/// Every location is probed for the current name first and the pre-rename name
/// second, so a toolchain installed before the rename keeps working.
pub fn staticlib_candidates(
    runtime_lib_env: Option<&Path>,
    lib_dir_env: Option<&Path>,
    matrix_rlib_env: Option<&Path>,
    current_exe: Option<&Path>,
    current_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let legacy = legacy_runtime_lib_name();
    let names = [runtime_lib_name(), legacy];

    if let Some(p) = runtime_lib_env {
        if !p.as_os_str().is_empty() {
            candidates.push(p.to_path_buf());
            for name in names {
                candidates.push(p.join(name));
            }
        }
    }
    let mut dirs: Vec<&Path> = Vec::new();
    for dir in [lib_dir_env, matrix_rlib_env].into_iter().flatten() {
        if !dir.as_os_str().is_empty() {
            dirs.push(dir);
        }
    }
    let mut exe_dirs: Vec<PathBuf> = Vec::new();
    if let Some(exe) = current_exe {
        if let Some(dir) = exe.parent() {
            exe_dirs.push(dir.to_path_buf());
            if let Some(parent) = dir.parent() {
                exe_dirs.push(parent.to_path_buf());
            }
        }
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(cwd) = current_dir {
        for profile in ["release", "debug"] {
            roots.push(cwd.join("target").join(profile));
        }
    }
    for dir in dirs {
        for name in names {
            candidates.push(dir.join(name));
            candidates.push(dir.join("deps").join(name));
            candidates.push(dir.join("lib").join(name));
        }
    }
    for dir in &exe_dirs {
        for name in names {
            candidates.push(dir.join(name));
            candidates.push(dir.join("deps").join(name));
            candidates.push(dir.join("lib").join(name));
            candidates.push(dir.join("lib").join("spid3r").join(name));
        }
    }
    for dir in &roots {
        for name in names {
            candidates.push(dir.join(name));
            candidates.push(dir.join("deps").join(name));
        }
    }
    candidates
}

/// Locate the Spid3r runtime static library (`spid3r.lib` / `libspid3r.a`) for
/// native AOT linking.
pub fn find_staticlib() -> Result<PathBuf, Diagnostic> {
    let rt_env = std::env::var_os("SPID3R_RUNTIME_LIB").map(PathBuf::from);
    let lib_env = std::env::var_os("SPID3R_LIB_DIR").map(PathBuf::from);
    let matrix_env = std::env::var_os("MATRIX_RLIB_DIR").map(PathBuf::from);
    let exe = std::env::current_exe().ok();
    let cwd = std::env::current_dir().ok();

    let candidates = staticlib_candidates(
        rt_env.as_deref(),
        lib_env.as_deref(),
        matrix_env.as_deref(),
        exe.as_deref(),
        cwd.as_deref(),
    );
    for c in candidates {
        if c.is_file() {
            return Ok(c);
        }
    }
    let name = runtime_lib_name();
    Err(Diagnostic::new(
        Phase::Usage,
        format!(
            "cannot find the Spid3r runtime library '{}'; set SPID3R_LIB_DIR / \
             MATRIX_RLIB_DIR or build the toolchain first (cargo build --features native)",
            name
        ),
    ))
}
