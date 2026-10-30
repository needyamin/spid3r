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

/// Give an extensionless `-o` path the platform's executable suffix.
///
/// `spid3r build app.matrix -o app` used to write a file called `app`, which
/// Windows will not run (no association, no `.exe`). An explicit extension is
/// used verbatim, matching `spid3r compile -o`.
fn normalize_exe_path(out: String, triple: &str) -> String {
    if Path::new(&out).extension().is_some() {
        return out;
    }
    if triple.contains("windows") || cfg!(windows) {
        format!("{}.exe", out)
    } else {
        out
    }
}

fn has_gui_natives(program: &Program) -> bool {
    const GUI_NATIVES: &[&str] = &[
        "gui_eval",
        "gui_set_html",
        "gui_on",
        "gui_emit",
        "gui_title",
    ];
    fn check_expr(e: &crate::ast::Expr) -> bool {
        match e {
            crate::ast::Expr::Ident(name, _) => GUI_NATIVES.contains(&name.as_str()),
            crate::ast::Expr::Call { callee, args, .. } => {
                check_expr(callee) || args.iter().any(check_expr)
            }
            crate::ast::Expr::Method { object, args, .. } => {
                check_expr(object) || args.iter().any(check_expr)
            }
            crate::ast::Expr::Unary { expr, .. } => check_expr(expr),
            crate::ast::Expr::Binary { l, r, .. } | crate::ast::Expr::Logical { l, r, .. } => {
                check_expr(l) || check_expr(r)
            }
            crate::ast::Expr::Assign { target, value, .. } => {
                check_expr(target) || check_expr(value)
            }
            crate::ast::Expr::List(items, _) => items.iter().any(check_expr),
            crate::ast::Expr::Index { object, index, .. } => {
                check_expr(object) || check_expr(index)
            }
            crate::ast::Expr::Field { object, .. } => check_expr(object),
            crate::ast::Expr::OptField { object, .. } => check_expr(object),
            crate::ast::Expr::New { args, .. } => args.iter().any(check_expr),
            crate::ast::Expr::Await(expr, _) => check_expr(expr),
            crate::ast::Expr::Spawn { callable, .. } => check_expr(callable),
            crate::ast::Expr::Fn(proto) => check_block(&proto.body),
            crate::ast::Expr::NamedArg { value, .. } => check_expr(value),
            crate::ast::Expr::Propagate(expr, _) => check_expr(expr),
            crate::ast::Expr::SuperCall { args, .. } => args.iter().any(check_expr),
            crate::ast::Expr::If {
                cond, then, els, ..
            } => {
                check_expr(cond)
                    || check_block(then)
                    || els.as_ref().map(check_block).unwrap_or(false)
            }
            crate::ast::Expr::Match {
                scrutinee, arms, ..
            } => {
                check_expr(scrutinee)
                    || arms.iter().any(|arm| {
                        arm.guard.as_ref().map(check_expr).unwrap_or(false)
                            || check_block(&arm.body)
                    })
            }
            crate::ast::Expr::Coalesce { l, r, .. } => check_expr(l) || check_expr(r),
            _ => false,
        }
    }
    fn check_block(b: &crate::ast::Block) -> bool {
        b.stmts.iter().any(check_stmt)
    }
    fn check_stmt(s: &crate::ast::Stmt) -> bool {
        match s {
            crate::ast::Stmt::Expr(e) => check_expr(e),
            crate::ast::Stmt::Let { value: Some(e), .. }
            | crate::ast::Stmt::Const { value: e, .. } => check_expr(e),
            crate::ast::Stmt::LetTuple { value, .. } => check_expr(value),
            crate::ast::Stmt::If {
                cond,
                then,
                elifs,
                els,
                ..
            } => {
                check_expr(cond)
                    || check_block(then)
                    || elifs.iter().any(|(c, b)| check_expr(c) || check_block(b))
                    || els.as_ref().map(check_block).unwrap_or(false)
            }
            crate::ast::Stmt::While { cond, body, .. } => check_expr(cond) || check_block(body),
            crate::ast::Stmt::ForIn { iter, body, .. } => check_expr(iter) || check_block(body),
            crate::ast::Stmt::ForC {
                init,
                cond,
                post,
                body,
                ..
            } => check_stmt(init) || check_expr(cond) || check_expr(post) || check_block(body),
            crate::ast::Stmt::Return { value: Some(e), .. } => check_expr(e),
            crate::ast::Stmt::Throw { value, .. } => check_expr(value),
            crate::ast::Stmt::Fn(f) => check_block(&f.body),
            crate::ast::Stmt::Class(c) => c.methods.iter().any(|m| check_block(&m.body)),
            crate::ast::Stmt::Block(b) => check_block(b),
            crate::ast::Stmt::Spawn { callable, .. } => check_expr(callable),
            crate::ast::Stmt::Yield { value, .. } => check_expr(value),
            crate::ast::Stmt::Try {
                body,
                catch,
                finally,
                ..
            } => {
                check_block(body)
                    || catch
                        .as_ref()
                        .map(|c| check_block(&c.body))
                        .unwrap_or(false)
                    || finally.as_ref().map(check_block).unwrap_or(false)
            }
            _ => false,
        }
    }
    program.stmts.iter().any(check_stmt)
}

/// Windows PE subsystem of the produced executable.
///
/// A `console` binary owns a terminal window; a `windows` binary does not, which
/// is what a GUI application (or a launcher for one) needs.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Subsystem {
    #[default]
    Console,
    Windows,
}

#[derive(Debug, Default, Clone)]
pub struct BuildMeta {
    pub icon: Option<PathBuf>,
    pub product_name: Option<String>,
    pub version: Option<String>,
    pub subsystem: Subsystem,
    /// Build even though GUI natives are not linked (launcher builds). The
    /// natives resolve to null at runtime; every guard below honours this.
    pub allow_gui: bool,
    /// Append the running `spid3r` executable to the payload, so the launcher
    /// can start its own runtime copy on a machine without Spid3r installed.
    pub bundle_runtime: bool,
}

/// The program compiled when `--bytecode` is given.
///
/// Deliberately trivial: it must not mention anything from the application, or
/// the application's symbols and literals end up in the native image again.
/// `run_payload_strict()` finds the runtime, anchors the working directory and
/// hands the payload over — and, unlike `run_payload()`, treats a *missing*
/// payload as fatal. Stripping the 24-byte trailer makes `body_range` report
/// "no payload", which the lenient form resolves by launching an installed
/// `spid3r.exe` from `%SPID3R_EXE%`, the per-user install or `PATH`; a
/// same-user process can strip those bytes to redirect the launch.
const LAUNCHER_STUB: &str = "\
fn main() {
    return run_payload_strict();
}
";

/// A private, per-build scratch directory under the system temp directory.
///
/// Intermediates (`.o`, `.manifest`, `.rc`, `.res`) used to be written to
/// `%TEMP%\spid3r_<stem>_<pid>.*`, which is predictable: a same-user process
/// could pre-create a hardlink at that path and have the compiler overwrite an
/// unrelated file, or hand `rc.exe`/`link.exe` a doctored resource. A fresh
/// directory with an unpredictable suffix, removed by `Drop` on both the success
/// and the failure path, closes that.
struct TempBuildDir(PathBuf);

impl TempBuildDir {
    fn new() -> Result<Self, Diagnostic> {
        let base = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let mut last: Option<std::io::Error> = None;
        for attempt in 0..8u32 {
            let name = if attempt == 0 {
                format!("spid3r-build-{}-{}", pid, nanos)
            } else {
                format!("spid3r-build-{}-{}-{}", pid, nanos, attempt)
            };
            let dir = base.join(name);
            // `create_dir`, not `create_dir_all`: an existing path is an error,
            // so a planted directory is never silently reused.
            match std::fs::create_dir(&dir) {
                Ok(()) => return Ok(TempBuildDir(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    last = Some(e);
                    continue;
                }
                Err(e) => {
                    return Err(Diagnostic::new(
                        Phase::Build,
                        format!(
                            "cannot create a scratch directory in '{}': {}",
                            base.display(),
                            e
                        ),
                    ))
                }
            }
        }
        Err(Diagnostic::new(
            Phase::Build,
            format!(
                "cannot create a private scratch directory in '{}': {}",
                base.display(),
                last.map(|e| e.to_string()).unwrap_or_default()
            ),
        ))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempBuildDir {
    fn drop(&mut self) {
        // Best effort: a leftover directory is harmless, a panic is not.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The `spid3r build` command.
pub fn cmd_build(args: &[String]) -> Result<i32, Diagnostic> {
    let mut file: Option<String> = None;
    let mut out: Option<String> = None;
    let mut target: Option<String> = None;
    let mut release = false;
    let mut icon: Option<PathBuf> = None;
    let mut product_name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut subsystem = Subsystem::Console;
    let mut allow_gui = false;
    let mut bundle_runtime = false;
    let mut bytecode: Option<PathBuf> = None;
    let mut embed: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                // One source of truth: `spid3r <cmd> --help` is answered by
                // `cli::subcommand_help`, and a direct call to this function
                // prints the same text instead of a second, drifting copy.
                if let Some(help) = crate::cli::subcommand_help("build") {
                    println!("{}", help);
                }
                return Ok(0);
            }
            "-o" => {
                i += 1;
                out = Some(args.get(i).cloned().ok_or_else(|| err("-o needs a path"))?);
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
            "--icon" => {
                i += 1;
                let p = args
                    .get(i)
                    .cloned()
                    .ok_or_else(|| err("--icon needs a path"))?;
                icon = Some(PathBuf::from(p));
            }
            "--product-name" => {
                i += 1;
                product_name = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| err("--product-name needs a name"))?,
                );
            }
            "--version" => {
                i += 1;
                version = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| err("--version needs a version string"))?,
                );
            }
            "--subsystem" => {
                i += 1;
                let kind = args
                    .get(i)
                    .cloned()
                    .ok_or_else(|| err("--subsystem needs a kind: console or windows"))?;
                subsystem = match kind.as_str() {
                    "console" => Subsystem::Console,
                    "windows" | "gui" => Subsystem::Windows,
                    other => {
                        return Err(err(format!(
                            "--subsystem must be 'console' or 'windows', got '{}'",
                            other
                        )));
                    }
                };
            }
            "--allow-gui" => allow_gui = true,
            "--bundle-runtime" => bundle_runtime = true,
            "--bytecode" => {
                i += 1;
                bytecode = Some(PathBuf::from(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| err("--bytecode needs a .matrixc file"))?,
                ));
            }
            "--embed" => {
                i += 1;
                embed.push(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| err("--embed needs a file or directory"))?,
                );
            }
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
    // Before anything else that could reach the AOT backend: on a machine
    // without a loadable LLVM-C.dll the process died with 0xC06D007E
    // (STATUS_DELAY_LOAD_FAILED) the moment the delay-loaded import was first
    // touched — no `error[...]`, no output file, no clue. Say what is missing.
    if cfg!(windows) && find_llvm_dll().is_none() {
        let mut diag = Diagnostic::new(
            Phase::Build,
            "LLVM-C.dll not found (needed to compile with the native backend); \
             install LLVM 18.1+ or set SPID3R_LLVM_BIN_DIR — see docs/BUILDING.md",
        );
        if let Some(note) = llvm_dll_elsewhere() {
            diag = diag.with_note(note);
        }
        return Err(diag);
    }
    // Pin the genuine DLL by absolute path. The delay-load import resolves by
    // bare name, and the loader searches the application directory and the
    // current directory before `PATH` — so without this, a `LLVM-C.dll` dropped
    // beside `spid3r.exe` or into the project being built runs its `DllMain`
    // inside the toolchain. Must happen before any LLVM call.
    #[cfg(windows)]
    preload_llvm_c();

    let file = file.ok_or_else(|| err("build needs a source file: spid3r build file.matrix"))?;

    // front end: read, parse, semantic-check (programs must have a main)
    let path = Path::new(&file);
    if !path.is_file() {
        return Err(err(format!("'{}' is not a file", file)));
    }
    // An icon that cannot be read used to be skipped silently, so a typo in
    // `--icon app.ico` produced an executable with the default icon and no
    // indication anything went wrong.
    if let Some(ref ico) = icon {
        if !ico.is_file() {
            return Err(err(format!("--icon '{}' is not a file", ico.display())));
        }
        if ico.metadata().map(|m| m.len() == 0).unwrap_or(true) {
            return Err(err(format!("--icon '{}' is empty", ico.display())));
        }
    }
    // `--bytecode` compiles a stub launcher instead of the application.
    //
    // This is what keeps the app out of the native image: without it the whole
    // program is AOT-compiled into the exe (every function body, every string
    // literal, and a symbol name per function) even though a packaged run only
    // ever executes `main` → `run_payload`. The stub is three lines and calls
    // nothing but the runtime.
    let mut bytecode_bytes: Option<Vec<u8>> = None;
    let (src, file_id): (String, std::sync::Arc<str>) = match &bytecode {
        Some(bc) => {
            if !bc.is_file() {
                return Err(err(format!("--bytecode '{}' is not a file", bc.display())));
            }
            let bytes = std::fs::read(bc).map_err(|e| {
                Diagnostic::new(
                    Phase::Build,
                    format!("--bytecode: cannot read '{}': {}", bc.display(), e),
                )
            })?;
            // Validate here, not at first launch: a truncated or unrelated file
            // used to build fine and then fail inside the GUI on the target
            // machine, where the error is invisible for a windows-subsystem exe.
            crate::vm::bytecode::deserialize_vmfunc(&bytes).map_err(|e| {
                Diagnostic::new(
                    Phase::Build,
                    format!(
                        "--bytecode '{}' is not valid Spid3r bytecode: {} \
                         (recompile it with `spid3r compile`)",
                        bc.display(),
                        e
                    ),
                )
            })?;
            bytecode_bytes = Some(bytes);
            (
                LAUNCHER_STUB.to_string(),
                std::sync::Arc::from("<launcher>"),
            )
        }
        None => (
            std::fs::read_to_string(path)
                .map_err(|e| err(format!("cannot read '{}': {}", file, e)))?,
            std::sync::Arc::from(file.as_str()),
        ),
    };
    let program: Program = crate::parser::parse_file(&src, file_id.clone())?;
    crate::semantic::check_file(&program, true, &file)?;

    let mut gui_natives = has_gui_natives(&program);
    if gui_natives && !allow_gui {
        return Err(Diagnostic::new(
            Phase::Build,
            "program uses GUI natives; the AOT backend does not link the WebView bridge — run it with `spid3r gui`, or pass --allow-gui to build a launcher that hands off to it",
        ));
    }

    // Flatten `import` into one self-contained program so the AOT backend
    // compiles a single module (it has no runtime import window).
    let program = crate::native::modules::flatten_imports(&program, &file)?;

    gui_natives = gui_natives || has_gui_natives(&program);
    if gui_natives && !allow_gui {
        return Err(Diagnostic::new(
            Phase::Build,
            "program uses GUI natives; the AOT backend does not link the WebView bridge — run it with `spid3r gui`, or pass --allow-gui to build a launcher that hands off to it",
        ));
    }
    if gui_natives {
        eprintln!(
            "warning: GUI natives are not linked in AOT builds; they resolve to null, \
             so this binary only works as a launcher for `spid3r gui`"
        );
    }

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("program")
        .to_string();
    let triple = target.unwrap_or_else(default_triple);
    // The linker invocation is the host's (MSVC on Windows, cc elsewhere) and
    // pulls the host runtime library, so a foreign triple only produced a
    // confusing linker error after a successful compile.
    let host = default_triple();
    if triple != host {
        return Err(err(format!(
            "--target '{}' is not supported: `spid3r build` links for the host \
             ('{}') only. Build on the target OS instead (see docs/BUILDING.md).",
            triple, host
        )));
    }
    // `-o app` used to write an extensionless file that Windows will not
    // launch; `compile` already appends its canonical extension.
    let out = normalize_exe_path(out.unwrap_or_else(|| default_exe(&stem)), &triple);
    let out_path = PathBuf::from(&out);

    let meta = BuildMeta {
        icon,
        product_name,
        version,
        subsystem,
        allow_gui,
        bundle_runtime,
    };

    // Collect and validate the payload *before* compiling and linking: a bad
    // `--embed`/`--bytecode` combination used to be detected after the linker
    // had already written the executable, so the build failed but left a
    // half-finished artifact behind (and wasted a full AOT compile).
    let mut entries = if embed.is_empty() {
        Vec::new()
    } else {
        collect_embed(&embed, path)?
    };
    // `--bytecode` embeds the compiled program under its own file name; the
    // payload reader prefers a `.matrixc` entry over a `.matrix` one.
    if let Some(bc) = &bytecode {
        let name = bc
            .file_name()
            .map(|n| n.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|| "app.matrixc".to_string());
        // Already read and validated above; reuse the bytes instead of a
        // second read that could pick up a different file.
        let data = bytecode_bytes.take().ok_or_else(|| {
            Diagnostic::new(Phase::Build, "--bytecode: program bytes are missing")
        })?;
        if entries.iter().any(|(n, _)| n == &name) {
            return Err(Diagnostic::new(
                Phase::Build,
                format!(
                    "--bytecode '{}' collides with an --embed entry of the same name",
                    name
                ),
            ));
        }
        println!("bytecode program: {} ({} bytes)", name, data.len());
        entries.push((name, data));
    }
    // Two `--embed` specs can expand to the same entry name (`--embed ui
    // --embed ui/index.html`): the payload reader folds entries into a map, so
    // which copy won used to depend on iteration order. Reject it here, while
    // the user can still tell which flag to drop.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (name, _) in &entries {
        if !seen.insert(name.as_str()) {
            return Err(Diagnostic::new(
                Phase::Build,
                format!(
                    "--embed produced two entries named '{}'; the payload cannot hold both \
                     (check for a repeated or overlapping --embed path)",
                    name
                ),
            ));
        }
    }
    // A payload without a program is dead weight: `run_payload()` refuses it at
    // launch ("no .matrixc or .matrix program"), and for a windows-subsystem exe
    // that refusal is invisible. This used to be checked for `--bundle-runtime`
    // only, so `build launcher.matrix --embed ui` exited 0 and produced a binary
    // that could never start.
    let has_program = entries
        .iter()
        .any(|(n, _)| n.ends_with(".matrix") || n.ends_with(".matrixc"));
    if !has_program && (bundle_runtime || !entries.is_empty()) {
        return Err(Diagnostic::new(
            Phase::Build,
            "the payload would contain no program: add `--bytecode app.matrixc` or \
             `--embed app.matrix` (a payload without a program cannot be launched)",
        ));
    }
    // `--bundle-runtime` ships the toolchain itself: the launcher hands off to
    // a runtime extracted from this payload, so the target machine needs no
    // Spid3r install. The entry name is what `run_payload()` looks for.
    let mut runtime_len: Option<usize> = None;
    if bundle_runtime {
        let self_exe = std::env::current_exe().map_err(|e| {
            Diagnostic::new(
                Phase::Build,
                format!("--bundle-runtime: cannot locate the running spid3r: {}", e),
            )
        })?;
        let data = std::fs::read(&self_exe).map_err(|e| {
            Diagnostic::new(
                Phase::Build,
                format!(
                    "--bundle-runtime: cannot read '{}': {}",
                    self_exe.display(),
                    e
                ),
            )
        })?;
        runtime_len = Some(data.len());
        entries.push((crate::native::payload::RUNTIME_ENTRY.to_string(), data));
    }

    // Every intermediate lives in one private directory for this build, and
    // `TempBuildDir`'s `Drop` removes it on both the success and the failure
    // path. Predictable names in the shared temp directory
    // (`spid3r_{stem}_{pid}.o`) let a same-user process pre-create a hardlink
    // there (`mklink /H`) and have the compiler clobber an arbitrary file — or
    // feed a doctored `.res` to the linker.
    let build_dir = TempBuildDir::new()?;
    let obj_path = build_dir.path().join(format!("spid3r_{}.o", stem));
    let report = build_native(
        &program,
        &triple,
        release,
        &obj_path,
        &out_path,
        &meta,
        build_dir.path(),
    )?;
    if !entries.is_empty() {
        let total: usize = entries.iter().map(|(_, d)| d.len()).sum();
        crate::native::payload::append(&out_path, &entries).map_err(|e| {
            Diagnostic::new(
                Phase::Build,
                format!("cannot embed files into '{}': {}", out_path.display(), e),
            )
        })?;
        println!("embedded {} file(s), {} bytes", entries.len(), total);
        if let Some(len) = runtime_len {
            println!(
                "bundled runtime: {} bytes (app runs without an installed spid3r)",
                len
            );
        }
    }
    println!("{}", report);
    Ok(0)
}

/// Ceilings for one `--embed` walk.
///
/// `Path::is_dir()` follows reparse points and `mklink /J` needs no privileges,
/// so `--embed ui` over a tree containing a junction used to either sweep
/// unrelated files (a junction to `C:\Users`) or recurse until the stack or the
/// disk ran out (a junction cycle). The limits below turn both into a
/// diagnostic, and the depth cap also bounds a cycle the symlink check misses.
const MAX_EMBED_DEPTH: usize = 32;
const MAX_EMBED_ENTRIES: usize = 10_000;
const MAX_EMBED_BYTES: u64 = 512 * 1024 * 1024;

/// Running totals for one `--embed` walk.
#[derive(Default)]
struct EmbedBudget {
    entries: usize,
    bytes: u64,
}

impl EmbedBudget {
    fn limit_error(what: &str, limit: impl std::fmt::Display) -> Diagnostic {
        Diagnostic::new(
            Phase::Usage,
            format!(
                "--embed: {} limit exceeded ({}); narrow the --embed paths",
                what, limit
            ),
        )
    }
}

/// Expand `--embed` specs into `(path, contents)` pairs.
///
/// Paths are kept relative to the source file's directory, so a bundle
/// reproduces the folder layout the app expects: `--embed ui` becomes
/// `ui/index.html`, `ui/app.js`, and so on. Dot-entries are skipped, which
/// keeps editor and tooling directories out of the binary.
fn collect_embed(specs: &[String], source: &Path) -> Result<Vec<(String, Vec<u8>)>, Diagnostic> {
    let base = source.parent().unwrap_or(Path::new("."));
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let mut budget = EmbedBudget::default();
    for spec in specs {
        // Relative specs resolve against the source file's directory, the same
        // base the entry names use — so the build does not depend on the
        // caller's working directory.
        let p = match Path::new(spec).is_absolute() {
            true => PathBuf::from(spec),
            false => base.join(spec),
        };
        let p = p.as_path();
        if p.is_dir() {
            collect_dir(p, base, &mut out, 0, &mut budget)?;
        } else if p.is_file() {
            push_entry(p, base, &mut out, &mut budget)?;
        } else {
            return Err(Diagnostic::new(
                Phase::Usage,
                format!(
                    "--embed '{}': no such file or directory (looked in '{}')",
                    spec,
                    if base.as_os_str().is_empty() {
                        ".".to_string()
                    } else {
                        base.display().to_string()
                    }
                ),
            ));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn collect_dir(
    dir: &Path,
    base: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
    depth: usize,
    budget: &mut EmbedBudget,
) -> Result<(), Diagnostic> {
    if depth >= MAX_EMBED_DEPTH {
        return Err(EmbedBudget::limit_error(
            &format!("recursion depth (at '{}')", dir.display()),
            MAX_EMBED_DEPTH,
        ));
    }
    let mut children: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| {
            Diagnostic::new(
                Phase::Usage,
                format!("cannot read '{}': {}", dir.display(), e),
            )
        })?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| !n.starts_with('.'))
                .unwrap_or(false)
        })
        .collect();
    children.sort();
    for child in children {
        // `symlink_metadata` does not follow the link, unlike `is_dir()` /
        // `is_file()`: a junction must be skipped, not walked.
        let md = match std::fs::symlink_metadata(&child) {
            Ok(md) => md,
            Err(e) => {
                eprintln!(
                    "warning: --embed: cannot inspect '{}': {} (skipped)",
                    child.display(),
                    e
                );
                continue;
            }
        };
        let ft = md.file_type();
        if ft.is_symlink() {
            eprintln!(
                "warning: --embed: '{}' is a symbolic link or junction; not followed (skipped)",
                child.display()
            );
            continue;
        }
        if ft.is_dir() {
            collect_dir(&child, base, out, depth + 1, budget)?;
        } else if ft.is_file() {
            push_entry(&child, base, out, budget)?;
        }
    }
    Ok(())
}

fn push_entry(
    file: &Path,
    base: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
    budget: &mut EmbedBudget,
) -> Result<(), Diagnostic> {
    let name = match file.strip_prefix(base) {
        Ok(p) => p.to_string_lossy().replace('\\', "/"),
        Err(_) => {
            // A path outside the source tree (`--embed C:\Users\me\.ssh\id_rsa`)
            // used to be embedded under its bare file name without a word. Keep
            // the fallback, but say so.
            let base_name = file
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("file"))
                .to_string_lossy()
                .replace('\\', "/");
            eprintln!(
                "warning: --embed: '{}' is outside '{}'; embedding it as '{}'",
                file.display(),
                base.display(),
                base_name
            );
            base_name
        }
    };
    if budget.entries + 1 > MAX_EMBED_ENTRIES {
        return Err(EmbedBudget::limit_error("file count", MAX_EMBED_ENTRIES));
    }
    // Check the size before reading: a single multi-gigabyte file must not be
    // pulled into memory only to be rejected afterwards.
    let size = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
    if budget.bytes.saturating_add(size) > MAX_EMBED_BYTES {
        return Err(EmbedBudget::limit_error(
            "total size (MiB)",
            MAX_EMBED_BYTES / (1024 * 1024),
        ));
    }
    let data = std::fs::read(file).map_err(|e| {
        Diagnostic::new(
            Phase::Usage,
            format!("cannot read '{}': {}", file.display(), e),
        )
    })?;
    budget.entries += 1;
    budget.bytes = budget.bytes.saturating_add(data.len() as u64);
    out.push((name, data));
    Ok(())
}

/// Compile + link, returning a short human-readable report.
///
/// `tmp` is a private scratch directory owned by the caller (see
/// [`TempBuildDir`]); every intermediate goes there.
pub fn build_native(
    program: &Program,
    triple: &str,
    opt: bool,
    obj_path: &Path,
    out_path: &Path,
    meta: &BuildMeta,
    tmp: &Path,
) -> Result<String, Diagnostic> {
    if has_gui_natives(program) && !meta.allow_gui {
        return Err(Diagnostic::new(
            Phase::Build,
            "program uses GUI natives; the AOT backend does not link the WebView bridge — run it with `spid3r gui`, or pass --allow-gui to build a launcher that hands off to it",
        ));
    }
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
    link_executable(obj_path, out_path, triple, meta, opt, tmp)?;
    Ok(format!(
        "built {} ({} {} bit, {})",
        out_path.display(),
        triple,
        if triple.contains("i686") || triple.contains("i586") {
            32
        } else {
            64
        },
        if opt { "release" } else { "debug" }
    ))
}

/// The host triple, matching LLVM's notion.
///
/// Prefers the triple the toolchain was built for, which `build.rs` passes
/// through as `SPID3R_TARGET`. The table below is only a fallback: it assumes
/// MSVC on Windows and glibc on Linux, so it misreports a `*-windows-gnu` or
/// `*-linux-musl` host and `--target <host triple>` was then rejected as
/// foreign by [`cmd_build`].
pub fn default_triple() -> String {
    if let Some(target) = option_env!("SPID3R_TARGET") {
        if !target.is_empty() {
            return target.to_string();
        }
    }
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
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_path_buf))
        {
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
    if !cfg!(windows) {
        for cmd in ["llvm-config-18", "llvm-config", "llvm-config-19"] {
            if let Ok(out) = Command::new(cmd).arg("--libdir").output() {
                if out.status.success() {
                    let dir =
                        PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
                    if dir.is_dir() {
                        return Some(dir);
                    }
                }
            }
        }
    }
    let candidates: &[&str] = if cfg!(windows) {
        &[
            r"C:\Program Files\LLVM\lib",
            r"C:\Program Files (x86)\LLVM\lib",
        ]
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
    candidates
        .iter()
        .map(Path::new)
        .find(|p| p.is_dir())
        .map(|p| p.to_path_buf())
}

/// Locate `LLVM-C.dll` for the pre-flight check **and** for the preload.
///
/// Two jobs, and the order below serves the second:
///
/// * Diagnose a missing DLL before the delay-load stub aborts the process with
///   `0xC06D007E` (no `error[...]`, no output file).
/// * Pick the file [`preload_llvm_c`] loads by absolute path. The delay-load
///   import resolves `LLVM-C.dll` by **bare name** through the unhardened
///   loader search, where the application directory and the *current
///   directory* both win over `PATH` — so a `LLVM-C.dll` dropped beside
///   `spid3r.exe`, or in a project directory a developer just cloned, would run
///   its `DllMain` inside the toolchain. Loading the genuine file first makes
///   the later `LoadLibrary("LLVM-C.dll")` return that module.
///
/// Order: explicit overrides → standard LLVM install locations → `PATH`
/// **excluding** the current directory and the executable's own directory →
/// the executable's directory last (the installer and `build.rs` put the DLL
/// there, and that directory is already trusted for the running binary).
fn find_llvm_dll() -> Option<PathBuf> {
    const NAME: &str = "LLVM-C.dll";

    // 1. Explicit overrides.
    if let Ok(dir) = std::env::var("SPID3R_LLVM_BIN_DIR") {
        let dll = Path::new(&dir).join(NAME);
        if dll.is_file() {
            prepend_to_path(Path::new(&dir));
            return Some(dll);
        }
    }
    if let Ok(prefix) = std::env::var("LLVM_SYS_181_PREFIX") {
        let dll = Path::new(&prefix).join("bin").join(NAME);
        if dll.is_file() {
            return Some(dll);
        }
    }
    // 2. The standard install locations, whether or not `bin` is on `PATH`.
    for base in [
        r"C:\Program Files\LLVM\bin",
        r"C:\Program Files (x86)\LLVM\bin",
    ] {
        let dll = Path::new(base).join(NAME);
        if dll.is_file() {
            return Some(dll);
        }
    }
    // 3. `PATH`, minus the two directories the loader searches before it.
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf));
    let cwd = std::env::current_dir().ok();
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            if cwd.as_deref() == Some(dir.as_path()) || exe_dir.as_deref() == Some(dir.as_path()) {
                continue;
            }
            let dll = dir.join(NAME);
            if dll.is_file() {
                return Some(dll);
            }
        }
    }
    // 4. Beside the executable: the installer layout.
    exe_dir.map(|d| d.join(NAME)).filter(|p| p.is_file())
}

/// Load `LLVM-C.dll` by absolute path before the delay-load import resolves it
/// by name.
///
/// Windows matches loaded modules by base name, so the delay-load helper's
/// later `LoadLibrary("LLVM-C.dll")` returns this module instead of searching
/// the application/current directory first. A failure here is not fatal: the
/// import still resolves the old way, and the caller has already established
/// that some copy exists.
#[cfg(windows)]
fn preload_llvm_c() {
    let Some(dll) = find_llvm_dll() else {
        return;
    };
    // SAFETY: `load_library_ex_w` builds a NUL-terminated UTF-16 buffer from a
    // valid path. The returned handle is intentionally never freed: dropping it
    // would unload the module while generated code can still call into it.
    if !unsafe { win::load_library_ex_w(&dll) } {
        crate::logging::record("build", &format!("could not preload '{}'", dll.display()));
    }
}

/// The one Win32 call needed to pin `LLVM-C.dll`.
#[cfg(windows)]
mod win {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryExW(
            file_name: *const u16,
            file: *mut core::ffi::c_void,
            flags: u32,
        ) -> *mut core::ffi::c_void;
    }

    /// `LoadLibraryExW(path, NULL, LOAD_WITH_ALTERED_SEARCH_PATH)`.
    ///
    /// The altered-search flag makes the DLL's own dependencies resolve beside
    /// it rather than against the process search order.
    ///
    /// # Safety
    /// Caller must keep the returned handle alive for as long as the module is
    /// needed (this call site never frees it).
    pub unsafe fn load_library_ex_w(path: &Path) -> bool {
        const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x0000_0008;
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);
        let handle = LoadLibraryExW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            LOAD_WITH_ALTERED_SEARCH_PATH,
        );
        !handle.is_null()
    }
}

/// Put `dir` first on `PATH` for the rest of the process.
///
/// A delay-loaded import resolves through the loader's normal search, so this is
/// what makes `SPID3R_LLVM_BIN_DIR` mean anything at run time. Child processes
/// (`link.exe`, `rc.exe`) inherit the extra entry, which is harmless.
fn prepend_to_path(dir: &Path) {
    let mut paths = vec![dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&existing));
    }
    if let Ok(joined) = std::env::join_paths(paths) {
        std::env::set_var("PATH", joined);
    }
}

/// Where else a copy of `LLVM-C.dll` sits, for the "not found" note.
///
/// Reporting this is the difference between "install LLVM" (wrong — it *is*
/// installed) and "LLVM is installed, but its `bin` directory is not on `PATH`".
fn llvm_dll_elsewhere() -> Option<String> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("LLVM_SYS_181_PREFIX") {
        dirs.push(Path::new(&p).join("bin"));
    }
    dirs.push(PathBuf::from(r"C:\Program Files\LLVM\bin"));
    dirs.push(PathBuf::from(r"C:\Program Files (x86)\LLVM\bin"));
    dirs.iter()
        .map(|d| d.join("LLVM-C.dll"))
        .find(|p| p.is_file())
        .map(|p| {
            format!(
                "'{}' exists but is not on PATH; add that directory to PATH \
                 (or set SPID3R_LLVM_BIN_DIR to it)",
                p.display()
            )
        })
}

/// Find `libspid3r.a` / `spid3r.lib` via environment overrides, sysroot layout,
/// or Cargo target directories (see [`super::find_staticlib`]).
fn find_staticlib() -> Result<PathBuf, Diagnostic> {
    super::find_staticlib()
}

/// Link the emitted object + runtime staticlib into the final executable.
fn link_executable(
    obj: &Path,
    out: &Path,
    triple: &str,
    meta: &BuildMeta,
    opt: bool,
    tmp: &Path,
) -> Result<(), Diagnostic> {
    let lib = find_staticlib()?;
    if cfg!(windows) {
        link_windows(obj, out, &lib, meta, opt, tmp)
    } else if cfg!(target_os = "macos") {
        link_plain("clang", obj, out, &lib, triple, &[], opt)
    } else {
        let cc = if which_program("clang").is_some() {
            "clang"
        } else {
            "cc"
        };
        link_plain(cc, obj, out, &lib, triple, &[], opt)
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
                let vc = e
                    .path()
                    .join("BuildTools")
                    .join("VC")
                    .join("Tools")
                    .join("MSVC");
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

/// Escape a string for use inside a `.rc` `VALUE` line.
///
/// `--product-name` and `--version` are user input pasted into an RC file;
/// a quote or a newline there ended the string and started a new directive,
/// which either broke `rc.exe` (no icon, no version info, no warning) or
/// injected resources.
fn rc_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn compile_windows_resources(
    out: &Path,
    meta: &BuildMeta,
    sdk_latest: &Path,
    tmp: &Path,
) -> Result<Option<PathBuf>, String> {
    let icon_requested = meta.icon.is_some();
    let ver = sdk_latest
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or_default();
    let mut rc_cand = sdk_latest
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("bin").join(ver).join("x64").join("rc.exe"))
        .unwrap_or_default();
    if !rc_cand.is_file() {
        match which_program("rc") {
            Some(p) => rc_cand = p,
            None if icon_requested => {
                return Err(
                    "rc.exe (the Windows SDK resource compiler) was not found, so --icon cannot \
                     be embedded; install the Windows 10/11 SDK (see docs/BUILDING.md)"
                        .to_string(),
                )
            }
            None => return Ok(None),
        }
    }

    let stem = out.file_stem().and_then(|s| s.to_str()).unwrap_or("app");

    // `tmp` is the per-build scratch directory: the names below only have to be
    // unique within it, not across processes.
    let manifest_path = tmp.join(format!("spid3r_manifest_{}.manifest", stem));
    let manifest_content = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2, PerMonitor</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
      <activeCodePage xmlns="http://schemas.microsoft.com/SMI/2019/WindowsSettings">UTF-8</activeCodePage>
    </windowsSettings>
  </application>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
      <supportedOS Id="{1f676c76-80e1-4239-95bb-83d0f6d0da78}"/>
      <supportedOS Id="{4a2f28e3-53b9-4441-ba9c-d69d4a4a6e38}"/>
      <supportedOS Id="{35138b9a-5d96-4fbd-8e2d-a2440225f93a}"/>
    </application>
  </compatibility>
</assembly>"#;
    let _ = std::fs::write(&manifest_path, manifest_content);

    let rc_path = tmp.join(format!("spid3r_res_{}.rc", stem));
    let res_path = tmp.join(format!("spid3r_res_{}.res", stem));

    let manifest_escaped = manifest_path.to_string_lossy().replace('\\', "/");
    let mut rc_content = format!("1 24 \"{}\"\n", manifest_escaped);

    let icon_path = meta.icon.clone().or_else(|| {
        let cand = PathBuf::from("packaging/nsis/assets/spid3r.ico");
        if cand.is_file() {
            Some(cand)
        } else {
            None
        }
    });

    if let Some(ico) = icon_path {
        let abs_ico = std::fs::canonicalize(&ico).unwrap_or(ico);
        rc_content.push_str(&format!(
            "1 ICON \"{}\"\n",
            abs_ico.to_string_lossy().replace('\\', "/")
        ));
    }

    let prod_name = rc_escape(meta.product_name.as_deref().unwrap_or(stem));
    let ver_str = rc_escape(meta.version.as_deref().unwrap_or("1.0.0"));
    let parts: Vec<u16> = meta
        .version
        .as_deref()
        .unwrap_or("1.0.0")
        .split('.')
        .filter_map(|p| p.trim().parse::<u16>().ok())
        .collect();
    let v0 = parts.first().copied().unwrap_or(1);
    let v1 = parts.get(1).copied().unwrap_or(0);
    let v2 = parts.get(2).copied().unwrap_or(0);
    let v3 = parts.get(3).copied().unwrap_or(0);

    rc_content.push_str(&format!(
        "1 VERSIONINFO\nFILEVERSION {v0},{v1},{v2},{v3}\nPRODUCTVERSION {v0},{v1},{v2},{v3}\nFILEFLAGSMASK 0x3fL\nFILEFLAGS 0x0L\nFILEOS 0x40004L\nFILETYPE 0x1L\nFILESUBTYPE 0x0L\nBEGIN\n    BLOCK \"StringFileInfo\"\n    BEGIN\n        BLOCK \"040904b0\"\n        BEGIN\n            VALUE \"FileDescription\", \"{prod_name}\"\n            VALUE \"FileVersion\", \"{v0}.{v1}.{v2}.{v3}\"\n            VALUE \"InternalName\", \"{stem}\"\n            VALUE \"OriginalFilename\", \"{stem}.exe\"\n            VALUE \"ProductName\", \"{prod_name}\"\n            VALUE \"ProductVersion\", \"{ver_str}\"\n        END\n    END\n    BLOCK \"VarFileInfo\"\n    BEGIN\n        VALUE \"Translation\", 0x409, 1200\n    END\nEND\n"
    ));

    if let Err(e) = std::fs::write(&rc_path, &rc_content) {
        let _ = std::fs::remove_file(&manifest_path);
        return Err(format!("cannot write '{}': {}", rc_path.display(), e));
    }

    let output = std::process::Command::new(&rc_cand)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_path)
        .arg(&rc_path)
        .output();

    let _ = std::fs::remove_file(&manifest_path);
    let _ = std::fs::remove_file(&rc_path);

    match output {
        Ok(o) if o.status.success() && res_path.is_file() => Ok(Some(res_path)),
        Ok(o) if icon_requested => {
            let log = String::from_utf8_lossy(&o.stdout).to_string()
                + &String::from_utf8_lossy(&o.stderr);
            let detail = log.trim();
            Err(if detail.is_empty() {
                format!("rc.exe failed ({})", o.status)
            } else {
                format!("rc.exe rejected the resources: {}", detail)
            })
        }
        Ok(_) => Ok(None),
        Err(e) if icon_requested => Err(format!("cannot run '{}': {}", rc_cand.display(), e)),
        Err(_) => Ok(None),
    }
}

fn link_windows(
    obj: &Path,
    out: &Path,
    lib: &Path,
    meta: &BuildMeta,
    opt: bool,
    tmp: &Path,
) -> Result<(), Diagnostic> {
    let mut link_exe: Option<PathBuf> = None;
    let mut libs_dir: Option<PathBuf> = None;
    for root in vs_roots() {
        let cand = root
            .join("bin")
            .join("HostX64")
            .join("x64")
            .join("link.exe");
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
        match meta.subsystem {
            Subsystem::Console => "-subsystem:console".to_string(),
            Subsystem::Windows => "-subsystem:windows".to_string(),
        },
        "-nodefaultlib".to_string(),
        obj.display().to_string(),
        lib.display().to_string(),
    ];
    // The generated entry point is `main`. Under the Windows subsystem the
    // linker would otherwise look for WinMain/WinMainCRTStartup, so pin the CRT
    // startup that calls main and exits with its return value.
    if meta.subsystem == Subsystem::Windows {
        args.push("-entry:mainCRTStartup".to_string());
    }
    if let Some(dir) = &libs_dir {
        args.push(format!("-libpath:{}", dir.display()));
    }
    args.push(format!(
        "-libpath:{}",
        sdk_latest.join("ucrt").join("x64").display()
    ));
    args.push(format!(
        "-libpath:{}",
        sdk_latest.join("um").join("x64").display()
    ));
    // The staticlib also contains the compiler's inkwell bindings. They are
    // reachable from `spid3r build`, never from an application, but a
    // non-optimised link keeps whatever the linker pulls in — so without
    // delay-loading, a `build` without `--release` produced an exe that
    // imported LLVM-C.dll and died with STATUS_DLL_NOT_FOUND (0xC0000135) on
    // the very machine `--bundle-runtime` exists for. Delay-loading moves the
    // requirement to the first actual LLVM call, which an app never makes.
    // Optional: `LLVM-C.lib` may resolve through the `LIB` environment variable
    // (a VS developer prompt) instead, and a `--release` build strips every
    // LLVM reference with `-opt:ref`, so the import library is never consulted.
    // Requiring a directory here broke both cases for no reason — pass the name
    // regardless and let the linker decide.
    let llvm_lib_dir = find_llvm_lib_dir();
    if let Some(dir) = &llvm_lib_dir {
        args.push(format!("-libpath:{}", dir.display()));
    }
    args.push("LLVM-C.lib".to_string());
    args.push("/DELAYLOAD:LLVM-C.dll".to_string());
    args.push("delayimp.lib".to_string());
    // With `-opt:ref` the LLVM references are stripped entirely, and the
    // linker then warns that the delay-load import has nothing to hook
    // (LNK4199). That is the desired outcome, not a problem.
    args.push("-ignore:4199".to_string());
    if opt {
        args.push("-opt:ref,icf".to_string());
    } else {
        args.push("-opt:noref,noicf".to_string());
    }
    // headroom above MAX_CALL_DEPTH frames (default 1MB is tight in debug
    // builds where every value lives in its own stack slot)
    args.push("-stack:4194304".to_string());
    // Everything above is CRT-independent; the two candidate CRT sets are
    // appended per attempt below, and the rest of the argument list is identical
    // between them.
    let mut tail_args: Vec<String> = Vec::new();
    for l in [
        "kernel32.lib",
        "ntdll.lib",
        "userenv.lib",
        "ws2_32.lib",
        "dbghelp.lib",
        "advapi32.lib",
    ] {
        tail_args.push(l.to_string());
    }
    let res_file = compile_windows_resources(out, meta, sdk_latest, tmp).map_err(|e| {
        Diagnostic::new(
            Phase::Build,
            format!(
                "cannot embed Windows resources into '{}': {}",
                out.display(),
                e
            ),
        )
    })?;
    if let Some(ref r) = res_file {
        tail_args.push(r.display().to_string());
    }

    // The import libraries Rust's std needs. Which CRT variant they must come
    // from follows the *runtime staticlib's* build: `.cargo/config.toml` links
    // the static CRT on Windows, so the staticlib references `memcpy`,
    // `__CxxFrameHandler3` and friends from `libcmt`/`libvcruntime`, not their
    // `__imp_` DLL-import counterparts. Passing `vcruntime.lib` there instead
    // would re-introduce a VCRUNTIME140.dll import in every built app — the
    // very dependency static linking removes.
    //
    // `cfg!(target_feature)` describes *this* binary, which is not the same
    // thing: `find_staticlib()` resolves the library independently
    // (`SPID3R_RUNTIME_LIB`, or the newest `deps/spid3r_lang-*.lib`), so a
    // stale or overridden staticlib mismatches the CRT set and the link fails
    // with LNK2019 `__imp_*` / LNK2005 that point nowhere. Rather than guess,
    // try the preferred set and fall back to the other one.
    const STATIC_CRT: &[&str] = &["libcmt.lib", "libvcruntime.lib", "libucrt.lib"];
    const DYNAMIC_CRT: &[&str] = &["msvcrt.lib", "vcruntime.lib", "ucrt.lib"];
    let (preferred, alternate) = if cfg!(target_feature = "crt-static") {
        (STATIC_CRT, DYNAMIC_CRT)
    } else {
        (DYNAMIC_CRT, STATIC_CRT)
    };

    let attempt = |crt: &[&str]| -> Result<(), Diagnostic> {
        let mut args = args.clone();
        for l in crt {
            args.push((*l).to_string());
        }
        args.extend(tail_args.iter().cloned());
        run_linker(&link_exe, &args)
    };

    let outcome = match attempt(preferred) {
        Ok(()) => Ok(()),
        Err(first) if is_crt_mismatch(&first.message) => {
            // Report the *first* failure if the retry fails too: it names the
            // symbols the program actually needs, while the second attempt's
            // error is an artefact of guessing wrong.
            attempt(alternate).map_err(|_| first)
        }
        Err(e) => Err(e),
    };
    if let Some(ref r) = res_file {
        let _ = std::fs::remove_file(r);
    }
    outcome.map_err(|e| llvm_link_hint(e, llvm_lib_dir.is_some()))
}

/// Does this linker output look like a CRT / import-library mismatch?
///
/// `LNK2019` (unresolved symbol, typically `__imp_*`), `LNK2005` (duplicate
/// symbol), `LNK4098` (conflicting CRT) and a bare `__imp_` mention all mean the
/// import libraries do not match the staticlib's CRT.
fn is_crt_mismatch(message: &str) -> bool {
    ["LNK2019", "LNK2005", "LNK4098", "__imp_"]
        .iter()
        .any(|needle| message.contains(needle))
}

/// Replace a bare "cannot open LLVM-C.lib" failure with an actionable one.
///
/// Only when the library directory was unknown: then the linker had to find the
/// import library on its own (through `LIB`), and it did not. When the directory
/// *was* known the failure means something else, and rewriting it would hide the
/// real cause.
fn llvm_link_hint(e: Diagnostic, llvm_dir_known: bool) -> Diagnostic {
    if llvm_dir_known {
        return e;
    }
    if !(e.message.contains("LLVM-C.lib") || e.message.contains("LNK1181")) {
        return e;
    }
    Diagnostic::new(
        Phase::Build,
        "cannot locate LLVM-C.lib, which the runtime static library links against; \
         install LLVM 18.1+ or set SPID3R_LLVM_LIB_DIR (see docs/BUILDING.md)",
    )
    .with_note(e.message)
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
    opt: bool,
) -> Result<(), Diagnostic> {
    let exe = which_program(cc).ok_or_else(|| {
        err(format!(
            "no '{}' linker found; install a C toolchain (see docs/BUILDING.md)",
            cc
        ))
    })?;
    let mut args: Vec<String> = vec![obj.display().to_string()];
    if !cfg!(windows) && !cfg!(target_os = "macos") {
        args.push("-Wl,--start-group".to_string());
        args.push(lib.display().to_string());
        args.push("-Wl,--end-group".to_string());
        args.push("-lstdc++".to_string());
    } else {
        args.push(lib.display().to_string());
    }
    args.extend([
        "-o".to_string(),
        out.display().to_string(),
        "-lpthread".to_string(),
        "-ldl".to_string(),
        "-lm".to_string(),
    ]);
    if opt {
        if cfg!(target_os = "macos") {
            args.push("-Wl,-dead_strip".to_string());
        } else {
            args.push("-Wl,--gc-sections".to_string());
            args.push("-Wl,--strip-all".to_string());
        }
    } else {
        if cfg!(target_os = "macos") {
            args.push("-Wl,-dead_strip".to_string());
        } else if !cfg!(windows) {
            args.push("-Wl,--gc-sections".to_string());
        }
    }
    // Query llvm-config for any additional system libs and ldflags if present.
    if !cfg!(windows) {
        for cmd in ["llvm-config-18", "llvm-config", "llvm-config-19"] {
            if let Ok(out) = Command::new(cmd)
                .args(["--ldflags", "--system-libs"])
                .output()
            {
                if out.status.success() {
                    let out_text = String::from_utf8_lossy(&out.stdout);
                    for word in out_text.split_whitespace() {
                        if !args.iter().any(|a| a == word) {
                            args.push(word.to_string());
                        }
                    }
                    break;
                }
            }
        }
    }
    // the staticlib also contains the compiler's inkwell bindings; the LLVM
    // shared library resolves them (dead-stripped away in the output).
    if let Some(dir) = find_llvm_lib_dir() {
        let flag = format!("-L{}", dir.display());
        if !args.contains(&flag) {
            args.push(flag);
        }
        if !cfg!(windows) && !cfg!(target_os = "macos") {
            let rpath = format!("-Wl,-rpath,{}", dir.display());
            if !args.contains(&rpath) {
                args.push(rpath);
            }
        }
    }
    let mut llvm_candidates: Vec<Vec<String>> = Vec::new();
    if !cfg!(windows) {
        for cmd in ["llvm-config-18", "llvm-config", "llvm-config-19"] {
            if let Ok(out) = Command::new(cmd).arg("--libs").output() {
                if out.status.success() {
                    let out_text = String::from_utf8_lossy(&out.stdout);
                    let flags: Vec<String> =
                        out_text.split_whitespace().map(|s| s.to_string()).collect();
                    if !flags.is_empty() {
                        llvm_candidates.push(flags);
                    }
                    break;
                }
            }
        }
    }
    let fallback_names: &[&str] = if cfg!(target_os = "macos") {
        &["-lLLVM"]
    } else {
        &["-lLLVM-18", "-lLLVM-19", "-lLLVM"]
    };
    for name in fallback_names {
        llvm_candidates.push(vec![name.to_string()]);
    }
    for e in extra {
        args.push(e.to_string());
    }
    let mut last_err: Option<Diagnostic> = None;
    for lgroup in llvm_candidates {
        let mut attempt = args.clone();
        for lname in lgroup {
            attempt.push(lname);
        }
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

/// Look up a program on `PATH` (Windows also tries the `.exe` suffix).
///
/// `pub(crate)` because `run_payload()` in the standard library resolves the
/// installed runtime the same way instead of duplicating the search.
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
    let out = Command::new(exe)
        .args(args)
        .output()
        .map_err(|e| err(format!("cannot run the linker '{}': {}", exe.display(), e)))?;
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
