//! Final steps of the native backend: emit an object file for the target
//! triple, then link it against `libspid3r.a` (the runtime staticlib) into a
//! native executable — the same way rustc links a program.
//!
//! Toolchain resolution is per host OS and needs no Rust dependency, only
//! external linkers:
//! - Windows: MSVC `link.exe` (BuildTools) with the Windows SDK CRT import
//!   libraries; falls back to `clang`/`gcc` (MinGW) and `zig cc`.
//! - macOS: `clang`/`cc`.
//! - Linux: `cc`/`clang`.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::ast::Program;
use crate::diagnostics::{Diagnostic, Phase};

fn err(msg: impl Into<String>) -> Diagnostic {
    Diagnostic::new(Phase::Usage, msg.into())
}

/// Default output name for a program (`hello.matrix` -> `hello` / `hello.exe`).
pub fn default_exe(stem: &str) -> String {
    if cfg!(windows) {
        format!("{}.exe", stem)
    } else {
        stem.to_string()
    }
}

/// The `spid3r build` command.
pub fn cmd_build(args: &[String]) -> Result<i32, Diagnostic> {
    let mut file: Option<String> = None;
    let mut out: Option<String> = None;
    let mut target: Option<String> = None;
    let mut release = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                i += 1;
                out = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| err("-o needs a path"))?,
                );
            }
            "--target" => {
                i += 1;
                target = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| err("--target needs a triple"))?,
                );
            }
            "--release" => release = true,
            other if other.starts_with('-') => {
                return Err(err(format!("unknown option '{}'", other)));
            }
            other => {
                if file.is_some() {
                    return Err(err("build takes exactly one source file"));
                }
                file = Some(other.to_string());
            }
        }
        i += 1;
    }
    let file = file.ok_or_else(|| err("build needs a source file: spid3r build file.matrix"))?;

    // front end: read, parse, semantic-check (programs must have a main)
    let path = Path::new(&file);
    if !path.is_file() {
        return Err(err(format!("'{}' is not a file", file)));
    }
    let src = std::fs::read_to_string(path)
        .map_err(|e| err(format!("cannot read '{}': {}", file, e)))?;
    let file_id: std::sync::Arc<str> = std::sync::Arc::from(file.as_str());
    let program: Program = crate::parser::parse_file(&src, file_id.clone())?;
    crate::semantic::check_file(&program, true, &file)?;
    // Flatten `import` into one self-contained program so the AOT backend
    // compiles a single module (it has no runtime import window).
    let program = crate::native::modules::flatten_imports(&program, &file)?;

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("program")
        .to_string();
    let out = out.unwrap_or_else(|| default_exe(&stem));
    let out_path = PathBuf::from(&out);

    let triple = target.unwrap_or_else(default_triple);
    let obj_dir = std::env::temp_dir();
    let obj_path = obj_dir.join(format!("spid3r_{}_{}.o", stem, std::process::id()));
    let report = build_native(&program, &triple, release, &obj_path, &out_path)?;
    let _ = std::fs::remove_file(&obj_path);
    println!("{}", report);
    Ok(0)
}

/// Compile + link, returning a short human-readable report.
pub fn build_native(
    program: &Program,
    triple: &str,
    opt: bool,
    obj_path: &Path,
    out_path: &Path,
) -> Result<String, Diagnostic> {
    // 1. LLVM IR
    let module = crate::native::ir::build_module(program, "spid3r_program")
        .map_err(|e| Diagnostic::new(Phase::Usage, e))?;
    if std::env::var_os("SPID3R_DUMP_IR").is_some() {
        eprintln!("{}", module.to_string());
    }
    // 2. object file
    crate::native::llvm::emit_object(&module, triple, opt, obj_path)
        .map_err(|e| Diagnostic::new(Phase::Usage, e))?;
    // 3. link against the runtime staticlib
    link_executable(obj_path, out_path, triple)?;
    Ok(format!(
        "built {} ({} {} bit, {})",
        out_path.display(),
        triple,
        if triple.contains("i686") || triple.contains("i586") { 32 } else { 64 },
        if opt { "release" } else { "debug" }
    ))
}

/// The host triple, matching LLVM's notion.
pub fn default_triple() -> String {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "x86_64-pc-windows-msvc".to_string(),
        ("windows", "x86") => "i686-pc-windows-msvc".to_string(),
        ("windows", "aarch64") => "aarch64-pc-windows-msvc".to_string(),
        ("macos", "x86_64") => "x86_64-apple-darwin".to_string(),
        ("macos", "aarch64") => "aarch64-apple-darwin".to_string(),
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu".to_string(),
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu".to_string(),
        ("linux", "x86") => "i686-unknown-linux-gnu".to_string(),
        (os, arch) => format!("{}-unknown-{}", arch, os),
    }
}

/// Locate the directory holding the LLVM-C import library / shared library,
/// mirroring `build.rs`'s search order. The installer ships `LLVM-C.lib` next
/// to `spid3r.exe`, so that directory is tried right after the overrides.
fn find_llvm_lib_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SPID3R_LLVM_LIB_DIR") {
        let p = PathBuf::from(p);
        if p.is_dir() {
            return Some(p);
        }
    }
    if cfg!(windows) {
        if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
            if dir.join("LLVM-C.lib").is_file() {
                return Some(dir);
            }
        }
    }
    if let Ok(p) = std::env::var("LLVM_SYS_181_PREFIX") {
        let dir = Path::new(&p).join("lib");
        if dir.is_dir() {
            return Some(dir);
        }
    }
    let candidates: &[&str] = if cfg!(windows) {
        &[r"C:\Program Files\LLVM\lib", r"C:\Program Files (x86)\LLVM\lib"]
    } else if cfg!(target_os = "macos") {
        &["/usr/local/lib", "/opt/homebrew/lib", "/usr/lib"]
    } else {
        &[
            "/usr/lib/llvm-18/lib",
            "/usr/lib/llvm-19/lib",
            "/usr/local/lib",
            "/usr/lib/x86_64-linux-gnu",
            "/usr/lib",
        ]
    };
    candidates.iter().map(Path::new).find(|p| p.is_dir()).map(|p| p.to_path_buf())
}

/// Find `libspid3r.a` (the staticlib produced by `cargo build`) next to the
/// current executable or in the target directory.
fn find_staticlib() -> Result<PathBuf, Diagnostic> {
    let name = if cfg!(windows) { "spid3r.lib" } else { "libspid3r.a" };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        // target/<profile>/spid3r[.exe] -> target/<profile>/<name>
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(name));
            // target/<profile>/deps/<name>
            candidates.push(dir.join("deps").join(name));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        for profile in ["release", "debug"] {
            candidates.push(cwd.join("target").join(profile).join(name));
            candidates.push(cwd.join("target").join(profile).join("deps").join(name));
        }
    }
    for c in candidates {
        if c.is_file() {
            return Ok(c);
        }
    }
    Err(err(format!(
        "cannot find the Spid3r runtime library '{}'; build the toolchain first \
         (cargo build --features native)",
        name
    )))
}

/// Link the emitted object + runtime staticlib into the final executable.
fn link_executable(obj: &Path, out: &Path, triple: &str) -> Result<(), Diagnostic> {
    let lib = find_staticlib()?;
    if cfg!(windows) {
        link_windows(obj, out, &lib)
    } else if cfg!(target_os = "macos") {
        link_plain("clang", obj, out, &lib, triple, &[])
    } else {
        link_plain("cc", obj, out, &lib, triple, &[])
    }
}

// ---------------------------------------------------------------------------
// Windows: MSVC link.exe with the Windows SDK + MSVC CRT import libraries
// ---------------------------------------------------------------------------

fn vs_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(p) = std::env::var("VCINSTALLDIR") {
        roots.push(PathBuf::from(p));
    }
    for base in [
        r"C:\Program Files\Microsoft Visual Studio",
        r"C:\Program Files (x86)\Microsoft Visual Studio",
    ] {
        let p = PathBuf::from(base);
        if let Ok(entries) = std::fs::read_dir(&p) {
            for e in entries.flatten() {
                let vc = e.path().join("BuildTools").join("VC").join("Tools").join("MSVC");
                if let Ok(versions) = std::fs::read_dir(&vc) {
                    let mut dirs: Vec<PathBuf> = versions.flatten().map(|v| v.path()).collect();
                    dirs.sort();
                    if let Some(latest) = dirs.last() {
                        roots.push(latest.clone());
                    }
                }
            }
        }
    }
    roots
}

fn winsdk_lib_root() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let base = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\Lib");
    if let Ok(entries) = std::fs::read_dir(&base) {
        let mut versions: Vec<PathBuf> = entries.flatten().map(|v| v.path()).collect();
        versions.sort();
        if let Some(latest) = versions.last() {
            roots.push(latest.clone());
        }
    }
    roots
}

fn link_windows(obj: &Path, out: &Path, lib: &Path) -> Result<(), Diagnostic> {
    let mut link_exe: Option<PathBuf> = None;
    let mut libs_dir: Option<PathBuf> = None;
    for root in vs_roots() {
        let cand = root.join("bin").join("HostX64").join("x64").join("link.exe");
        if cand.is_file() {
            link_exe = Some(cand);
            libs_dir = Some(root.join("lib").join("x64"));
            break;
        }
    }
    if link_exe.is_none() {
        // fall back to a link.exe on PATH that is not Git's coreutils shim
        if let Some(p) = which_program("link") {
            link_exe = Some(p);
        }
    }
    let link_exe = link_exe.ok_or_else(|| {
        err(
            "no MSVC linker (link.exe) found; install Visual Studio Build Tools \
             (see docs/BUILDING.md)",
        )
    })?;

    let sdk = winsdk_lib_root();
    if sdk.is_empty() {
        return Err(err(
            "no Windows SDK found; install the Windows 10/11 SDK (see docs/BUILDING.md)",
        ));
    }
    let sdk_latest = &sdk[0];

    let mut args: Vec<String> = vec![
        "-nologo".to_string(),
        format!("-out:{}", out.display()),
        "-subsystem:console".to_string(),
        "-nodefaultlib".to_string(),
        obj.display().to_string(),
        lib.display().to_string(),
    ];
    if let Some(dir) = &libs_dir {
        args.push(format!("-libpath:{}", dir.display()));
    }
    args.push(format!("-libpath:{}", sdk_latest.join("ucrt").join("x64").display()));
    args.push(format!("-libpath:{}", sdk_latest.join("um").join("x64").display()));
    // the staticlib also contains the compiler's inkwell bindings; resolve
    // their LLVM-C references and then strip everything the program does not
    // use, so the shipped executable has no LLVM-C.dll dependency.
    if let Some(dir) = find_llvm_lib_dir() {
        args.push(format!("-libpath:{}", dir.display()));
    }
    args.push("LLVM-C.lib".to_string());
    args.push("-opt:ref".to_string());
    // headroom above MAX_CALL_DEPTH frames (default 1MB is tight in debug
    // builds where every value lives in its own stack slot)
    args.push("-stack:4194304".to_string());
    // the import libraries Rust's std needs (same set rustc passes)
    for l in [
        "msvcrt.lib",
        "vcruntime.lib",
        "ucrt.lib",
        "kernel32.lib",
        "ntdll.lib",
        "userenv.lib",
        "ws2_32.lib",
        "dbghelp.lib",
        "advapi32.lib",
    ] {
        args.push(l.to_string());
    }
    run_linker(&link_exe, &args)
}

// ---------------------------------------------------------------------------
// macOS / Linux: the system C driver
// ---------------------------------------------------------------------------

fn link_plain(
    cc: &str,
    obj: &Path,
    out: &Path,
    lib: &Path,
    _triple: &str,
    extra: &[&str],
) -> Result<(), Diagnostic> {
    let exe = which_program(cc).ok_or_else(|| {
        err(format!(
            "no '{}' linker found; install a C toolchain (see docs/BUILDING.md)",
            cc
        ))
    })?;
    let mut args: Vec<String> = vec![
        obj.display().to_string(),
        lib.display().to_string(),
        "-o".to_string(),
        out.display().to_string(),
        "-lpthread".to_string(),
        "-ldl".to_string(),
        "-lm".to_string(),
    ];
    // the staticlib also contains the compiler's inkwell bindings; the LLVM
    // shared library resolves them (dead-stripped away in the output).
    if let Some(dir) = find_llvm_lib_dir() {
        args.push(format!("-L{}", dir.display()));
    }
    let llvm_lib_names: &[&str] = if cfg!(target_os = "macos") {
        &["-lLLVM"]
    } else {
        &["-lLLVM-18", "-lLLVM-19", "-lLLVM"]
    };
    for e in extra {
        args.push(e.to_string());
    }
    let mut last_err: Option<Diagnostic> = None;
    for lname in llvm_lib_names {
        let mut attempt = args.clone();
        attempt.push(lname.to_string());
        match run_linker(&exe, &attempt) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = Some(e),
        }
    }
    // no LLVM library found under any known name: report the first attempt's
    // failure (or a targeted message if none ran)
    Err(last_err.unwrap_or_else(|| {
        err("cannot locate the LLVM shared library (libLLVM-18); set \
             SPID3R_LLVM_LIB_DIR to its directory")
    }))
}

fn which_program(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        vec![".exe".to_string(), "".to_string()]
    } else {
        vec!["".to_string()]
    };
    for dir in std::env::split_paths(&paths) {
        for ext in &exts {
            let cand = dir.join(format!("{}{}", name, ext));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

fn run_linker(exe: &Path, args: &[String]) -> Result<(), Diagnostic> {
    let out = Command::new(exe).args(args).output().map_err(|e| {
        err(format!(
            "cannot run the linker '{}': {}",
            exe.display(),
            e
        ))
    })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        return Err(err(format!(
            "linking failed ({}):\n{}{}",
            exe.display(),
            stdout.trim(),
            if stderr.trim().is_empty() {
                String::new()
            } else {
                format!("\n{}", stderr.trim())
            }
        )));
    }
    Ok(())
}
