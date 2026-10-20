//! Command line front end for the `spid3r` binary.
//!
//! Exit codes: 0 success, 1 user-program error, 2 usage error.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::ast::Stmt;
use crate::diagnostics::{Diagnostic, Phase};
use crate::interpreter::Interp;
use crate::modules;
use crate::parser;
use crate::semantic;
use crate::vm::bytecode::{deserialize_vmfunc, disassemble, serialize_vmfunc};
use crate::vm::Vm;
use crate::{LANGUAGE_VERSION, VERSION};

pub const EXIT_OK: i32 = 0;
pub const EXIT_USER_ERROR: i32 = 1;
pub const EXIT_USAGE: i32 = 2;

const HELP: &str = "\
spid3r - the Spid3r language toolchain

USAGE:
    spid3r <command> [options]

COMMANDS:
    run [--engine tree|vm] <file.matrix>   Compile and run a program
    check <file.matrix>                    Lex, parse, semantic-check; no run
    compile <file.matrix> [-o out] [--dump]
                                           Write bytecode (.matrixc); --dump prints it
    build <file.matrix> [-o out] [--target triple] [--release]
                                           Compile to a native machine-code executable
    gui <file.matrix> [--html page.html] [--dry-run]
                                           Run a program in a GUI webview window
    bundle <file> [--target wasm|desktop|android] [--html page.html] [-o out]
                                           Package into a standalone distribution
                                           (WebAssembly, Desktop, or Android)
    exec <file.matrixc>                    Run bytecode on the virtual machine
    repl                                   Interactive read-eval-print loop
    debug <file.matrix> [--break L[,L…]] [--commands FILE]
                                           Line-based debugger on the VM:
                                           breakpoints, step/next/finish,
                                           backtrace, locals, source listing
    pack [-o out.tar]                      Archive spider.toml + src/ into
                                           dist/<name>-<version>.tar
    unpack <archive> [dir]                 Extract a packed project
    init [dir]                             Create spider.toml + src/main.matrix
    install [--update]                     Vendor path/git dependencies into
                                           vendor/, write spider.lock (commit
                                           pinned, SHA-256 per dep); --update
                                           re-resolves instead of honoring the
                                           locked commits
    dist [-o out] [--no-opt]               Compile the project entry (spider.toml)
                                           into target/<package>.matrixc
    test                                   Run every examples/*.matrix file
    bench [--engine tree|vm] [--runs N] [--compare] [files...]
                                           Time benchmarks/*.matrix (best of N)
    fmt [--check] [files...]               Canonical formatting (in place);
                                           --check exits 1 if any file differs
    version                                Print version information
    help                                   Print this help

EXIT CODES:
    0 success, 1 user-program error, 2 usage error
";

pub fn version_text() -> String {
    format!(
        "spid3r {} (language {})\nRust host, no runtime dependencies.",
        VERSION, LANGUAGE_VERSION
    )
}

pub fn run(args: Vec<String>) -> i32 {
    // Default `args()` to empty for every subcommand; `run` replaces this with
    // the real script arguments. Without this, programs executed via `exec`,
    // `test`, or `dist` would see the interpreter's own argv.
    crate::stdlib::set_script_args(Vec::new());
    let mut it = args.into_iter().skip(1);
    let cmd = match it.next() {
        Some(c) => c,
        None => {
            eprint!("{}", HELP);
            return EXIT_USAGE;
        }
    };
    let rest: Vec<String> = it.collect();
    // The parser caps expression nesting at MAX_PARSE_DEPTH, but the semantic
    // checker, the tree interpreter, and the VM compiler all recurse per AST
    // level with frames far larger than the parser's; on a 1 MiB default stack
    // (Windows main thread, debug builds) deep-but-valid input aborted the
    // host with STATUS_STACK_OVERFLOW and no diagnostic. Run every subcommand
    // on a dedicated big-stack worker thread so the worst case is a clean
    // error, and join it so exit codes and panics behave exactly as before.
    let cmd_for_thread = cmd.clone();
    let rest_clone = rest.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("spid3r-{}", cmd_for_thread))
        .stack_size(512 * 1024 * 1024)
        .spawn(move || dispatch(cmd_for_thread, rest_clone));
    match spawned {
        Ok(handle) => match handle.join() {
            Ok(outcome) => {
                // Deterministic teardown (audit A-3): every program has already
                // drained its tasks; stop the pool and join the worker threads
                // instead of letting process exit kill them mid-write.
                crate::sched::shutdown();
                outcome_code(outcome)
            }
            Err(panic) => std::panic::resume_unwind(panic),
        },
        // Thread creation failed (extremely unlikely): run inline as before.
        Err(_) => {
            let outcome = dispatch(cmd, rest);
            crate::sched::shutdown();
            outcome_code(outcome)
        }
    }
}

fn dispatch(cmd: String, rest: Vec<String>) -> Result<i32, Diagnostic> {
    let outcome = match cmd.as_str() {
        "version" | "--version" | "-V" => {
            println!("{}", version_text());
            return Ok(EXIT_OK);
        }
        "help" | "--help" | "-h" => {
            print!("{}", HELP);
            return Ok(EXIT_OK);
        }
        "run" => cmd_run(&rest),
        "check" => cmd_check(&rest),
        "compile" => cmd_compile(&rest),
        "build" => cmd_build(&rest),
        "gui" => cmd_gui(&rest),
        "bundle" => cmd_bundle(&rest),
        "exec" => cmd_exec(&rest),
        "repl" => {
            crate::repl::run();
            Ok(EXIT_OK)
        }
        "test" => cmd_test(&rest),
        "bench" => cmd_bench(&rest),
        "fmt" => cmd_fmt(&rest),
        "init" => cmd_init(&rest),
        "install" => cmd_install(&rest),
        "dist" => cmd_project_build(&rest),
        "pack" => cmd_pack(&rest),
        "unpack" => cmd_unpack(&rest),
        "debug" => cmd_debug(&rest),
        other => {
            eprintln!("error[usage]: unknown command '{}'\n{}", other, HELP);
            return Ok(EXIT_USAGE);
        }
    };
    outcome
}

fn outcome_code(outcome: Result<i32, Diagnostic>) -> i32 {
    match outcome {
        Ok(code) => code,
        Err(d) => {
            eprintln!("{}", d);
            if d.phase == Phase::Usage {
                EXIT_USAGE
            } else {
                EXIT_USER_ERROR
            }
        }
    }
}

// ------------------------------------------------------------------ helpers

fn usage(msg: &str) -> Diagnostic {
    Diagnostic::new(Phase::Usage, format!("usage error: {}", msg))
}

fn read_source(path: &str) -> Result<(String, Arc<str>), Diagnostic> {
    let p = Path::new(path);
    if !p.is_file() {
        return Err(usage(&format!("'{}' is not a file", path)));
    }
    let src =
        std::fs::read_to_string(p).map_err(|e| usage(&format!("cannot read '{}': {}", path, e)))?;
    let file: Arc<str> = Arc::from(path);
    Ok((src, file))
}

fn front_end(path: &str) -> Result<(crate::ast::Program, Arc<str>), Diagnostic> {
    let (src, file) = read_source(path)?;
    let program = parser::parse_file(&src, file.clone())?;
    semantic::check_file(&program, true, path)?;
    Ok((program, file))
}

/// Load and run a program on the reference runner (tree-walk).
fn run_tree(path: &str) -> Result<i32, Diagnostic> {
    let (program, file) = front_end(path)?;
    let dirs = modules::default_module_dirs();
    let interp = Interp::new(file, dirs);
    interp.run_program(&program)
}

/// `spid3r run` has no webview, so a program that calls the `gui_*`
/// natives would die later with an engine-specific error (`unknown name` on
/// the tree engine, `null is not callable` on the VM). Detect it up front from
/// the token stream (comments and string literals are ignored) and point the
/// user at `spid3r gui` instead.
fn reject_gui_program(path: &str) -> Result<(), Diagnostic> {
    const GUI_FUNCTIONS: &[&str] = &[
        "gui_eval",
        "gui_set_html",
        "gui_on",
        "gui_emit",
        "gui_title",
    ];
    let (src, file) = read_source(path)?;
    let Ok(tokens) = crate::lexer::Lexer::new(&src, file).tokenize() else {
        return Ok(()); // lex errors are reported by the normal front end
    };
    for t in &tokens {
        if let crate::lexer::token::TokenKind::Ident(name) = &t.kind {
            if GUI_FUNCTIONS.contains(&name.as_str()) {
                return Err(usage(&format!(
                    "'{}' uses the GUI function `{}` and needs a window; run it with `spid3r gui {}` instead of `spid3r run`",
                    path, name, path
                )));
            }
        }
    }
    Ok(())
}

// ----------------------------------------------------------------- commands

fn cmd_run(args: &[String]) -> Result<i32, Diagnostic> {
    let mut engine = "tree";
    let mut file: Option<String> = None;
    let mut no_opt = false;
    let mut script_args: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-opt" => no_opt = true,
            "--engine" => {
                i += 1;
                match args.get(i) {
                    Some(e) if e == "tree" || e == "vm" => engine = e,
                    Some(e) => return Err(usage(&format!("unknown engine '{}'", e))),
                    None => return Err(usage("--engine needs a value")),
                }
            }
            a if a.starts_with("--") => return Err(usage(&format!("unknown option '{}'", a))),
            a => {
                if file.is_none() {
                    file = Some(a.to_string());
                } else {
                    // Positionals after the script path are the program's own
                    // arguments (installed for `args()` below).
                    script_args.push(a.to_string());
                }
            }
        }
        i += 1;
    }
    // Inside a project, `spid3r run` with no file runs the manifest entry.
    let file = match file {
        Some(f) => f,
        None => {
            let manifest = crate::pkg::Manifest::load(Path::new("."))
                .map_err(|e| usage(&e))?
                .ok_or_else(|| usage("run needs a .matrix file"))?;
            if !Path::new(manifest.entry()).is_file() {
                return Err(usage(&format!(
                    "entry '{}' does not exist",
                    manifest.entry()
                )));
            }
            manifest.entry().to_string()
        }
    };
    reject_gui_program(&file)?;
    // `args()` must see the program's arguments, not the interpreter's argv.
    crate::stdlib::set_script_args(script_args);
    match engine {
        "tree" => run_tree(&file),
        "vm" => {
            let (program, file_name) = front_end(&file)?;
            let dirs = modules::default_module_dirs();
            if no_opt {
                let (script, _public) =
                    crate::vm::compile_program_unoptimized(&program, &file, &dirs)?;
                let mut vm = Vm::new(file_name, dirs);
                vm.run_compiled(Arc::new(script))
            } else {
                let mut vm = Vm::new(file_name, dirs);
                vm.run_program(&program)
            }
        }
        _ => unreachable!(),
    }
}

fn cmd_check(args: &[String]) -> Result<i32, Diagnostic> {
    let file = single_file(args, "check")?;
    let (src, file_name) = read_source(&file)?;
    let program = parser::parse_file(&src, file_name)?;
    // a file without fn main is a library module: checked without the main rule
    let has_main = program.stmts.iter().any(|s| match s {
        crate::ast::Stmt::Fn(f) => f.name.as_deref() == Some("main"),
        _ => false,
    });
    semantic::check_file(&program, has_main, &file)?;
    println!("{}: ok", file);
    Ok(EXIT_OK)
}

// ------------------------------------------------------------------ build

/// `spid3r build file.matrix [-o out] [--target triple] [--release]`
/// Compile to a native machine-code executable with the LLVM backend.
fn cmd_build(args: &[String]) -> Result<i32, Diagnostic> {
    #[cfg(not(feature = "native"))]
    {
        let _ = args;
        Err(usage(
            "the native backend is not enabled in this build; install LLVM 18.1+ \
             (see docs/BUILDING.md) and rebuild the toolchain with --features native",
        ))
    }
    #[cfg(feature = "native")]
    {
        crate::native::link::cmd_build(args)
    }
}

/// `spid3r gui file.matrix [--html page.html] [--dry-run]`
/// Run a program in a desktop window with an embedded webview.
fn cmd_gui(args: &[String]) -> Result<i32, Diagnostic> {
    #[cfg(not(feature = "gui"))]
    {
        let _ = args;
        Err(usage(
            "the GUI backend is not enabled in this build; rebuild the toolchain \
             with --features gui",
        ))
    }
    #[cfg(feature = "gui")]
    {
        crate::gui::cmd_gui(args)
    }
}

/// `spid3r bundle file.matrix [--target wasm|desktop|android] [--html page.html] [-o out] [--single-file]`
/// Package a Spid3r application into a standalone cross-platform bundle.
fn cmd_bundle(args: &[String]) -> Result<i32, Diagnostic> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = args;
        Err(usage("bundler is not supported inside WebAssembly runtime"))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut file: Option<PathBuf> = None;
        let mut target = crate::bundle::BundleTarget::Wasm;
        let mut html_file: Option<PathBuf> = None;
        let mut out: Option<PathBuf> = None;
        let mut single_file = false;

        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--target" => {
                    i += 1;
                    let t_str = args.get(i).ok_or_else(|| {
                        usage("--target needs a value (wasm, desktop, or android)")
                    })?;
                    target = crate::bundle::BundleTarget::parse(t_str).map_err(|e| usage(&e))?;
                }
                "--html" => {
                    i += 1;
                    html_file = Some(PathBuf::from(
                        args.get(i).ok_or_else(|| usage("--html needs a path"))?,
                    ));
                }
                "-o" | "--out" => {
                    i += 1;
                    out = Some(PathBuf::from(
                        args.get(i).ok_or_else(|| usage("-o needs a path"))?,
                    ));
                }
                "--single-file" => {
                    single_file = true;
                }
                a if a.starts_with("--") => return Err(usage(&format!("unknown option '{}'", a))),
                other => {
                    if file.is_some() {
                        return Err(usage(&format!("unexpected extra argument '{}'", other)));
                    }
                    file = Some(PathBuf::from(other));
                }
            }
            i += 1;
        }

        let source_file = file.ok_or_else(|| {
            usage("bundle needs a source file: spid3r bundle app.matrix [--target wasm|desktop|android]")
        })?;
        let opts = crate::bundle::BundleOptions {
            source_file,
            target,
            html_file,
            output_path: out,
            single_file,
        };

        let result_path = crate::bundle::bundle(&opts)?;
        println!("Bundle generated successfully: {}", result_path.display());
        Ok(EXIT_OK)
    }
}

fn cmd_compile(args: &[String]) -> Result<i32, Diagnostic> {
    let mut file: Option<String> = None;
    let mut out: Option<String> = None;
    let mut dump = false;
    let mut no_opt = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                i += 1;
                out = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| usage("-o needs a path"))?,
                );
            }
            "--dump" => dump = true,
            "--no-opt" => no_opt = true,
            a if a.starts_with("--") => return Err(usage(&format!("unknown option '{}'", a))),
            a => {
                if file.is_some() {
                    return Err(usage("compile takes exactly one file"));
                }
                file = Some(a.to_string());
            }
        }
        i += 1;
    }
    let file = file.ok_or_else(|| usage("compile needs a .matrix file"))?;
    compile_file(
        &file,
        out.as_deref(),
        CompileOptions {
            dump,
            no_opt,
            entry: false,
        },
    )
}

/// What `compile`/`build` share: front end -> bytecode -> serialized `.matrixc`.
struct CompileOptions {
    /// `compile --dump`: print the disassembly.
    dump: bool,
    /// `--no-opt`: skip the bytecode optimizer.
    no_opt: bool,
    /// `build`: the file comes from the project manifest; the output defaults
    /// to `target/<package>.matrixc` instead of next to the source.
    entry: bool,
}

fn compile_file(file: &str, out: Option<&str>, opts: CompileOptions) -> Result<i32, Diagnostic> {
    let (src, file_name) = read_source(file)?;
    let program = parser::parse_file(&src, file_name)?;
    // A file without `fn main` is a library module: check it without the
    // main-required rule, then still emit its (empty-top-level) bytecode.
    let has_main = program
        .stmts
        .iter()
        .any(|s| matches!(s, Stmt::Fn(f) if f.name.as_deref() == Some("main")));
    semantic::check_file(&program, has_main, file)?;
    let dirs = modules::default_module_dirs();
    let (script, _public) = if opts.no_opt {
        crate::vm::compile_program_unoptimized(&program, file, &dirs)?
    } else {
        crate::vm::compile_program(&program, file, &dirs)?
    };
    if opts.dump {
        println!("{}", disassemble(&script.chunk, "<script>"));
    }
    let bytes = serialize_vmfunc(&script);
    let out_path = match out {
        Some(o) => o.to_string(),
        None => {
            if opts.entry {
                // Project build: target/<package>.matrixc.
                let manifest = crate::pkg::Manifest::load(Path::new("."))
                    .map_err(|e| usage(&e))?
                    .ok_or_else(|| usage("no spider.toml in this directory"))?;
                std::fs::create_dir_all("target")
                    .map_err(|e| usage(&format!("cannot create target/: {}", e)))?;
                Path::new("target")
                    .join(format!("{}.matrixc", manifest.name()))
                    .to_string_lossy()
                    .to_string()
            } else {
                default_matrixc(file)
            }
        }
    };
    if let Some(parent) = Path::new(&out_path).parent() {
        if !parent.as_os_str().is_empty() && !parent.is_dir() {
            std::fs::create_dir_all(parent)
                .map_err(|e| usage(&format!("cannot create '{}': {}", parent.display(), e)))?;
        }
    }
    std::fs::write(&out_path, bytes)
        .map_err(|e| usage(&format!("cannot write '{}': {}", out_path, e)))?;
    println!("wrote {}", out_path);
    Ok(EXIT_OK)
}

fn cmd_exec(args: &[String]) -> Result<i32, Diagnostic> {
    let file = single_file(args, "exec")?;
    let bytes =
        std::fs::read(&file).map_err(|e| usage(&format!("cannot read '{}': {}", file, e)))?;
    let script = deserialize_vmfunc(&bytes)?;
    let file_arc: Arc<str> = Arc::from(file.as_str());
    let dirs = modules::default_module_dirs();
    let mut vm = Vm::new(file_arc, dirs);
    vm.run_compiled(Arc::new(script))
}

/// Default `.matrixc` output path: replace a trailing `.matrix` with
/// `.matrixc`, otherwise append `.matrixc`.
fn default_matrixc(file: &str) -> String {
    if let Some(stripped) = file.strip_suffix(".matrix") {
        format!("{}.matrixc", stripped)
    } else {
        format!("{}.matrixc", file)
    }
}

fn single_file(args: &[String], cmd: &str) -> Result<String, Diagnostic> {
    let files: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if files.len() != 1 {
        return Err(usage(&format!("{} takes exactly one file", cmd)));
    }
    Ok(files[0].clone())
}

fn cmd_test(_args: &[String]) -> Result<i32, Diagnostic> {
    let root = PathBuf::from("examples");
    if !root.is_dir() {
        if Path::new("spider.toml").is_file() {
            let manifest = crate::pkg::Manifest::load(Path::new("."))
                .map_err(|e| usage(&e))?
                .ok_or_else(|| usage("cannot read spider.toml"))?;
            let (total_ok, total_failed) = if manifest.members.is_empty() {
                project_test_in(Path::new("."))?
            } else {
                // Workspace: test every member in its own directory.
                let mut ok = 0;
                let mut failed = 0;
                for member in &manifest.members {
                    println!("== member {} ==", member);
                    let (o, f) = project_test_in(Path::new(member))?;
                    println!("member {}: {} ok, {} failed", member, o, f);
                    ok += o;
                    failed += f;
                }
                (ok, failed)
            };
            println!("{} ok, {} failed", total_ok, total_failed);
            if total_failed == 0 {
                return Ok(EXIT_OK);
            }
            return Ok(EXIT_USER_ERROR);
        }
        return Err(Diagnostic::new(
            Phase::Parser,
            "no examples/ directory here; run spid3r test from the project root",
        ));
    }
    let bin = std::env::current_exe().map_err(|e| {
        Diagnostic::new(
            Phase::Usage,
            format!("cannot locate the spid3r binary: {}", e),
        )
    })?;
    let mut files: Vec<PathBuf> = std::fs::read_dir(&root)
        .map_err(|e| usage(&format!("cannot read examples/: {}", e)))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "matrix").unwrap_or(false))
        .collect();
    files.sort();
    let mut ok = 0;
    let mut failed = 0;
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let path = f.to_string_lossy().to_string();
        // Program vs library: decide by the AST, not a fragile string search.
        let is_program = has_main_fn(&path);
        let result: Result<(), String> = if is_program {
            // Differential test: all three engines must produce identical stdout.
            let tree = run_capture(
                &bin,
                &["run", "--engine", "tree", f.as_os_str().to_str().unwrap()],
            );
            let vm = run_capture(
                &bin,
                &["run", "--engine", "vm", f.as_os_str().to_str().unwrap()],
            );
            let tc = std::env::temp_dir()
                .join(format!("spid3r_test_{}.matrixc", name.replace('.', "_")));
            let tc = tc.to_string_lossy().to_string();
            let compiled = run_capture(
                &bin,
                &["compile", f.as_os_str().to_str().unwrap(), "-o", &tc],
            );
            let exec_out = match &compiled {
                Ok(_) => run_capture(&bin, &["exec", &tc]),
                Err(e) => Err(e.clone()),
            };
            match (tree, vm, exec_out) {
                (Ok(t), Ok(v), Ok(x)) if t == v && v == x => Ok(()),
                (Ok(t), Ok(v), Ok(x)) => Err(format!(
                    "outputs differ:\n--- tree ---\n{}\n--- vm ---\n{}\n--- exec ---\n{}",
                    t, v, x
                )),
                other => Err(format!(
                    "an engine failed: tree_ok={}, vm_ok={}, exec_ok={}",
                    other.0.is_ok(),
                    other.1.is_ok(),
                    other.2.is_ok()
                )),
            }
        } else {
            // Library module: it must at least pass the semantic check.
            cmd_check(&[path]).map(|_| ()).map_err(|d| d.message)
        };
        match result {
            Ok(()) => {
                println!(
                    "ok   {} {}",
                    name,
                    if is_program {
                        "(tree==vm==exec)"
                    } else {
                        "(library: checked)"
                    }
                );
                ok += 1;
            }
            Err(d) => {
                println!("FAIL {}: {}", name, d);
                failed += 1;
            }
        }
    }
    println!("{} ok, {} failed", ok, failed);
    if failed == 0 {
        Ok(EXIT_OK)
    } else {
        Ok(EXIT_USER_ERROR)
    }
}

/// `spid3r test` inside a project: run every `tests/*.matrix` as a
/// differential (tree == vm, both exit 0); files without `fn main` are
/// libraries and only get the semantic check. Prints the per-test lines and
/// returns (ok, failed) for the caller's summary.
fn project_test_in(dir: &Path) -> Result<(usize, usize), Diagnostic> {
    let tests = dir.join("tests");
    if !tests.is_dir() {
        println!("no tests/ directory in this project — nothing to run");
        return Ok((0, 0));
    }
    let bin = std::env::current_exe().map_err(|e| {
        Diagnostic::new(
            Phase::Usage,
            format!("cannot locate the spid3r binary: {}", e),
        )
    })?;
    let mut files: Vec<PathBuf> = std::fs::read_dir(&tests)
        .map_err(|e| usage(&format!("cannot read tests/: {}", e)))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "matrix").unwrap_or(false))
        .collect();
    files.sort();
    let mut ok = 0;
    let mut failed = 0;
    for f in &files {
        // Absolute paths: the subprocess runs in `dir`, so a relative path
        // would resolve against the wrong base (workspace members are nested).
        let abs = f.canonicalize().unwrap_or_else(|_| f.clone());
        let path = abs.to_string_lossy().to_string();
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let is_program = has_main_fn(&path);
        let result: Result<(), String> = if is_program {
            let tree = run_capture_in(&bin, &["run", "--engine", "tree", &path], Some(dir));
            let vm = run_capture_in(&bin, &["run", "--engine", "vm", &path], Some(dir));
            match (tree, vm) {
                (Ok(t), Ok(v)) if t == v => Ok(()),
                (Ok(t), Ok(v)) => Err(format!(
                    "outputs differ://n--- tree ---\n{}\n--- vm ---\n{}",
                    t, v
                )),
                (Err(tree), Err(vm)) => Err(format!(
                    "both engines failed://n--- tree ---\n{}\n--- vm ---\n{}",
                    tree, vm
                )),
                other => Err(format!(
                    "an engine failed: tree_ok={}, vm_ok={}",
                    other.0.is_ok(),
                    other.1.is_ok()
                )),
            }
        } else {
            // Library: the semantic check runs in the member directory.
            let saved =
                std::env::current_dir().map_err(|e| usage(&format!("cannot read cwd: {}", e)))?;
            std::env::set_current_dir(dir)
                .map_err(|e| usage(&format!("cannot enter {}: {}", dir.display(), e)))?;
            let r = cmd_check(&[path]).map(|_| ()).map_err(|d| d.message);
            let _ = std::env::set_current_dir(saved);
            r
        };
        match result {
            Ok(()) => {
                println!(
                    "ok   tests/{} {}",
                    name,
                    if is_program {
                        "(tree==vm)"
                    } else {
                        "(library: checked)"
                    }
                );
                ok += 1;
            }
            Err(d) => {
                println!("FAIL tests/{}: {}", name, d);
                failed += 1;
            }
        }
    }
    Ok((ok, failed))
}

/// True when the file's AST defines `fn main`, i.e. it is a runnable program
/// rather than a library module. Uses the parsed AST (not a string search) so a
/// literal `fn main` embedded in a comment or string cannot misclassify it.
fn has_main_fn(path: &str) -> bool {
    match read_source(path) {
        Ok((src, file)) => match parser::parse_file(&src, file) {
            Ok(program) => program
                .stmts
                .iter()
                .any(|s| matches!(s, Stmt::Fn(f) if f.name.as_deref() == Some("main"))),
            Err(_) => false,
        },
        Err(_) => false,
    }
}

// ------------------------------------------------------------------- debug

/// `spid3r debug <file.matrix> [--break LINE[,LINE...]] [--commands FILE]`
///
/// A line-based debugger over the bytecode VM: set breakpoints, step
/// instruction by instruction or line by line, inspect the call stack and the
/// current frame's slots, and list source around the pause point. With
/// `--commands FILE` (or piped stdin) the session is fully scriptable, which is
/// how the debugger is tested; when the command source is exhausted the
/// program runs to completion.
///
/// Exit codes: the program's own exit code, or 3 when the user quits the
/// debugger with `quit`.
fn cmd_debug(args: &[String]) -> Result<i32, Diagnostic> {
    let mut file: Option<String> = None;
    let mut breakpoints: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut commands: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--break" | "-b" => {
                i += 1;
                let spec = args
                    .get(i)
                    .ok_or_else(|| usage("--break needs a line number"))?;
                for part in spec.split(',') {
                    let l = part.trim().parse::<u32>().map_err(|_| {
                        usage(&format!("--break expects line numbers, got '{}'", part))
                    })?;
                    breakpoints.insert(l);
                }
            }
            "--commands" | "-c" => {
                i += 1;
                let path = args
                    .get(i)
                    .ok_or_else(|| usage("--commands needs a file"))?;
                let text = std::fs::read_to_string(path)
                    .map_err(|e| usage(&format!("cannot read commands '{}': {}", path, e)))?;
                // Strip a UTF-8 BOM so the first scripted command is not seen
                // as "\u{feff}bt" (the .matrix lexer strips it the same way).
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
                commands = text
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .collect();
            }
            a if a.starts_with("--") => return Err(usage(&format!("unknown option '{}'", a))),
            a => {
                if file.is_some() {
                    return Err(usage("debug takes exactly one file"));
                }
                file = Some(a.to_string());
            }
        }
        i += 1;
    }
    let file = file.ok_or_else(|| usage("debug needs a .matrix file"))?;
    let (src, file_name) = read_source(&file)?;
    let source: Vec<String> = src.lines().map(|l| l.to_string()).collect();
    let program = parser::parse_file(&src, file_name.clone())?;
    semantic::check_file(&program, true, &file)?;

    let dirs = modules::default_module_dirs();
    let mut vm = Vm::new(file_name.clone(), dirs);
    let state = crate::vm::DebugState::new(source, breakpoints, commands);
    let interactive = state.interactive;
    vm.enable_debug(state);
    if interactive {
        println!(
            "spid3r debug — {} (VM engine). Type 'help' for commands, 'quit' to exit.",
            file
        );
    }
    let result = vm.run_program(&program);
    if vm.debug_quit() {
        println!("debugger: quit after {} breakpoint hit(s)", vm.debug_hits());
        return Ok(3);
    }
    result
}

/// `spid3r fmt [--check] [files...]`
///
/// `spid3r pack [-o out.tar]` — archive the project (spider.toml + src/) into
/// a deterministic uncompressed tar for distribution.
fn cmd_pack(args: &[String]) -> Result<i32, Diagnostic> {
    let mut out: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                i += 1;
                out = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| usage("-o needs a path"))?,
                );
            }
            a => return Err(usage(&format!("unknown argument '{}'", a))),
        }
        i += 1;
    }
    let root = std::env::current_dir().map_err(|e| usage(&format!("cannot read cwd: {}", e)))?;
    let manifest = crate::pkg::Manifest::load(&root)
        .map_err(|e| usage(&e))?
        .ok_or_else(|| usage("no spider.toml in this directory — run `spid3r init` first"))?;
    if manifest.members.is_empty() {
        let (path, files, hash) =
            crate::pkg::pack(&root, out.as_deref().map(PathBuf::from).as_deref())
                .map_err(|e| usage(&e))?;
        println!("packed {} file(s) into {}", files, path.display());
        println!("sha256: {}", hash);
        return Ok(EXIT_OK);
    }
    // Workspace root: pack every member into the root's dist/.
    if out.is_some() {
        return Err(usage("-o is not supported for a workspace pack"));
    }
    let archives = crate::pkg::pack_workspace(&root, &manifest).map_err(|e| usage(&e))?;
    for (path, files, hash) in &archives {
        println!("packed {} file(s) into {}", files, path.display());
        println!("sha256: {}", hash);
    }
    println!("{} archive(s)", archives.len());
    Ok(EXIT_OK)
}

/// `spid3r unpack <archive> [dir]` — extract a packed project.
fn cmd_unpack(args: &[String]) -> Result<i32, Diagnostic> {
    if args.is_empty() || args.len() > 2 {
        return Err(usage("unpack takes an archive and an optional directory"));
    }
    let archive = PathBuf::from(&args[0]);
    let dest = match args.get(1) {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(
            archive
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "unpacked".to_string()),
        ),
    };
    let extracted = crate::pkg::unpack(&archive, &dest).map_err(|e| usage(&e))?;
    println!(
        "unpacked {} file(s) into {}",
        extracted.len(),
        dest.display()
    );
    for f in &extracted {
        println!("  {}", f);
    }
    Ok(EXIT_OK)
}

/// `spid3r init [dir]` — scaffold a project (never overwrites existing files).
fn cmd_init(args: &[String]) -> Result<i32, Diagnostic> {
    let mut dir: Option<String> = None;
    for a in args {
        if a.starts_with("--") {
            return Err(usage(&format!("unknown option '{}'", a)));
        }
        if dir.is_some() {
            return Err(usage("init takes at most one directory"));
        }
        dir = Some(a.clone());
    }
    let dir = dir.unwrap_or_else(|| ".".to_string());
    let path = PathBuf::from(&dir);
    // Derive the name from the literal argument: canonicalize() fails on a
    // not-yet-existing directory, which used to force every fresh `init` to
    // the fallback name "app".
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "app".to_string());
    let created = crate::pkg::init(&path, &name).map_err(|e| usage(&e))?;
    if created.is_empty() {
        println!("{}: already initialised", path.display());
    } else {
        for f in created {
            println!("created {}", path.join(f).display());
        }
    }
    Ok(EXIT_OK)
}

/// `spid3r dist [-o out.matrixc] [--no-opt]` — compile the project entry
/// (from spider.toml) into a self-contained `.matrixc`.
fn cmd_project_build(args: &[String]) -> Result<i32, Diagnostic> {
    let mut out: Option<String> = None;
    let mut no_opt = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                i += 1;
                out = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| usage("-o needs a path"))?,
                );
            }
            "--no-opt" => no_opt = true,
            a => return Err(usage(&format!("unknown argument '{}'", a))),
        }
        i += 1;
    }
    let root = std::env::current_dir().map_err(|e| usage(&format!("cannot read cwd: {}", e)))?;
    let manifest = crate::pkg::Manifest::load(&root)
        .map_err(|e| usage(&e))?
        .ok_or_else(|| usage("no spider.toml in this directory — run `spid3r init` first"))?;
    let entry = root.join(manifest.entry());
    if !entry.is_file() {
        return Err(usage(&format!(
            "entry '{}' does not exist",
            manifest.entry()
        )));
    }
    // Fail early with a helpful message rather than "unknown import".
    if !manifest.deps.is_empty() && crate::modules::find_vendor_dir().is_none() {
        return Err(usage(
            "dependencies are declared but not installed — run `spid3r install` first",
        ));
    }
    compile_file(
        &entry.to_string_lossy(),
        out.as_deref(),
        CompileOptions {
            dump: false,
            no_opt,
            entry: true,
        },
    )
}

/// `spid3r install` — resolve `[dependencies]` into `vendor/` and lock them.
fn cmd_install(args: &[String]) -> Result<i32, Diagnostic> {
    let mut update = false;
    for a in args {
        match a.as_str() {
            "--update" | "-u" => update = true,
            other => return Err(usage(&format!("install: unknown argument '{}'", other))),
        }
    }
    let root = std::env::current_dir().map_err(|e| usage(&format!("cannot read cwd: {}", e)))?;
    let report = crate::pkg::install_with(&root, update).map_err(|e| usage(&e))?;
    if report.deps.is_empty() {
        println!("no dependencies in spider.toml");
    } else {
        println!(
            "installed {} dependency(ies), {} file(s) vendored{}",
            report.deps.len(),
            report.vendored,
            if report.unchanged > 0 {
                ", spider.lock unchanged"
            } else {
                ""
            }
        );
        for d in &report.deps {
            println!("  {}", d);
        }
    }
    Ok(EXIT_OK)
}

/// Rewrite files in canonical form. Without `--check`, files are formatted in
/// place; with `--check`, nothing is written and the command exits 1 when any
/// file is not yet formatted (CI mode). With no files given, formats every
/// `.matrix` file under examples/ and benchmarks/.
fn cmd_fmt(args: &[String]) -> Result<i32, Diagnostic> {
    let mut check = false;
    let mut files: Vec<String> = Vec::new();
    for a in args {
        match a.as_str() {
            "--check" => check = true,
            s if s.starts_with("--") => return Err(usage(&format!("unknown option '{}'", s))),
            s => files.push(s.to_string()),
        }
    }
    if files.is_empty() {
        for dir in ["examples", "benchmarks"] {
            let d = PathBuf::from(dir);
            if d.is_dir() {
                let mut found: Vec<String> = std::fs::read_dir(&d)
                    .map_err(|e| usage(&format!("cannot read {}/: {}", dir, e)))?
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().map(|x| x == "matrix").unwrap_or(false))
                    .map(|p| p.to_string_lossy().to_string())
                    .collect();
                found.sort();
                files.extend(found);
            }
        }
    }
    if files.is_empty() {
        return Err(usage("fmt needs at least one file"));
    }

    // Expand directory arguments in place: each directory contributes its
    // `*.matrix` files (sorted by name, non-recursive) and then behaves
    // exactly like multiple file arguments. A directory without any .matrix
    // file is a clean usage error instead of a failed directory read.
    let mut expanded: Vec<String> = Vec::new();
    for arg in &files {
        let path = Path::new(arg);
        if path.is_dir() {
            let mut found: Vec<String> = std::fs::read_dir(path)
                .map_err(|e| usage(&format!("cannot read '{}': {}", arg, e)))?
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|p| p.is_file() && p.extension().map(|x| x == "matrix").unwrap_or(false))
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            found.sort();
            if found.is_empty() {
                return Err(usage(&format!(
                    "no .matrix files found in directory '{}'",
                    arg
                )));
            }
            expanded.extend(found);
        } else {
            expanded.push(arg.clone());
        }
    }
    let files = expanded;

    let mut changed = 0usize;
    let mut errors = 0usize;
    for f in &files {
        let src = std::fs::read_to_string(f)
            .map_err(|e| usage(&format!("cannot read '{}': {}", f, e)))?;
        let file: Arc<str> = Arc::from(f.as_str());
        let formatted = match crate::fmt::format_source(&src, &file) {
            Ok(t) => t,
            Err(d) => {
                println!("error: {}: {}", f, d.message);
                errors += 1;
                continue;
            }
        };
        let src_lf = if src.contains('\r') {
            src.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            src.clone()
        };
        if formatted == src_lf {
            println!("unchanged {}", f);
            continue;
        }
        changed += 1;
        if check {
            println!("would reformat {}", f);
        } else {
            std::fs::write(f, &formatted)
                .map_err(|e| usage(&format!("cannot write '{}': {}", f, e)))?;
            println!("formatted {}", f);
        }
    }
    if errors > 0 {
        return Ok(EXIT_USER_ERROR);
    }
    if check && changed > 0 {
        return Ok(EXIT_USER_ERROR);
    }
    Ok(EXIT_OK)
}

/// Run the spid3r binary as a subprocess and capture its stdout. Returns an error
/// string on non-zero exit or IO failure (stderr is included in the message).
fn run_capture(bin: &Path, args: &[&str]) -> Result<String, String> {
    run_capture_in(bin, args, None)
}

/// `run_capture` with an optional working directory (workspace tests run in
/// their member's directory so module resolution and relative paths work).
fn run_capture_in(bin: &Path, args: &[&str], cwd: Option<&Path>) -> Result<String, String> {
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("failed to execute {:?}: {}", bin, e))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).to_string())
    }
}

// ------------------------------------------------------------------ bench

/// Outer-iteration counts for the shipped benchmarks, so `spid3r bench` can
/// report iters/sec. Each iteration runs the file's full loop body once; the
/// per-iteration work is documented in the benchmark file itself.
const BENCH_ITERS: &[(&str, u64)] = &[
    ("arith", 2_000_000),
    ("strings", 30_000),
    ("list", 200_000),
    ("map", 40_000),
    ("methods", 200_000),
    ("closures", 40_000),
    ("json", 2_000),
    ("tasks", 2_000),
];

fn bench_iters(stem: &str) -> Option<u64> {
    BENCH_ITERS
        .iter()
        .find(|(n, _)| *n == stem)
        .map(|(_, c)| *c)
}

/// `spid3r bench [--engine tree|vm] [--runs N] [--compare] [files...]`
///
/// Times each benchmark file by running it in a fresh subprocess (best of N
/// runs), checks the printed checksum is stable across runs, and with
/// `--compare` also verifies tree and vm produce identical output - a
/// performance harness that doubles as a differential test.
fn cmd_bench(args: &[String]) -> Result<i32, Diagnostic> {
    let mut engine = "vm";
    let mut runs: usize = 3;
    let mut compare = false;
    let mut no_opt = false;
    let mut files: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--engine" => {
                i += 1;
                match args.get(i) {
                    Some(e) if e == "tree" || e == "vm" => engine = e,
                    Some(e) => return Err(usage(&format!("unknown engine '{}'", e))),
                    None => return Err(usage("--engine needs a value")),
                }
            }
            "--runs" => {
                i += 1;
                let n = args
                    .get(i)
                    .and_then(|v| v.parse::<usize>().ok())
                    .ok_or_else(|| usage("--runs needs a number"))?;
                if n == 0 {
                    return Err(usage("--runs must be at least 1"));
                }
                runs = n;
            }
            "--compare" => compare = true,
            "--no-opt" => no_opt = true,
            a if a.starts_with("--") => return Err(usage(&format!("unknown option '{}'", a))),
            a => files.push(a.to_string()),
        }
        i += 1;
    }
    if files.is_empty() {
        let dir = PathBuf::from("benchmarks");
        if !dir.is_dir() {
            return Err(Diagnostic::new(
                Phase::Usage,
                "no benchmarks/ directory here; run spid3r bench from the project root \
                 or pass benchmark files explicitly",
            ));
        }
        let mut found: Vec<String> = std::fs::read_dir(&dir)
            .map_err(|e| usage(&format!("cannot read benchmarks/: {}", e)))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "matrix").unwrap_or(false))
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        found.sort();
        files = found;
    }
    if files.is_empty() {
        return Err(usage("no benchmark files found"));
    }

    let bin = std::env::current_exe().map_err(|e| {
        Diagnostic::new(
            Phase::Usage,
            format!("cannot locate the spid3r binary: {}", e),
        )
    })?;

    println!(
        "spid3r bench - engine {}, best of {} run{}{}",
        engine,
        runs,
        if runs == 1 { "" } else { "s" },
        if compare { ", tree vs vm compared" } else { "" }
    );
    println!(
        "{:<12} {:>12} {:>14} {:>10}",
        "benchmark", "iters", "best time", "iters/sec"
    );
    let mut failures = 0;
    for f in &files {
        let stem = Path::new(f)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| f.clone());
        let run = |eng: &str| -> Result<(std::time::Duration, String), String> {
            let mut best: Option<std::time::Duration> = None;
            let mut last_out = String::new();
            for _ in 0..runs {
                let t0 = std::time::Instant::now();
                let mut args = vec!["run", "--engine", eng];
                if no_opt {
                    args.push("--no-opt");
                }
                args.push(f);
                let out = run_capture(&bin, &args)?;
                let dt = t0.elapsed();
                if last_out.is_empty() {
                    last_out = out;
                } else if last_out != out {
                    return Err("output differs between runs (unstable benchmark)".to_string());
                }
                best = Some(match best {
                    Some(b) if b <= dt => b,
                    _ => dt,
                });
            }
            Ok((best.unwrap(), last_out))
        };
        let (time, out) = match run(engine) {
            Ok(x) => x,
            Err(e) => {
                println!("{:<12} FAILED: {}", stem, e.trim());
                failures += 1;
                continue;
            }
        };
        if compare {
            let other = if engine == "vm" { "tree" } else { "vm" };
            match run(other) {
                Ok((_, out2)) if out2 == out => {}
                Ok((_, out2)) => {
                    println!(
                        "{:<12} FAILED: {} and {} outputs differ ({} vs {})",
                        stem,
                        engine,
                        other,
                        out.trim(),
                        out2.trim()
                    );
                    failures += 1;
                    continue;
                }
                Err(e) => {
                    println!("{:<12} FAILED on {}: {}", stem, other, e.trim());
                    failures += 1;
                    continue;
                }
            }
        }
        let secs = time.as_secs_f64();
        let (time_s, rate) = match bench_iters(&stem) {
            Some(iters) => (
                format!("{:.3?}", time),
                format!("{:.0}", iters as f64 / secs),
            ),
            None => (format!("{:.3?}", time), "-".to_string()),
        };
        println!(
            "{:<12} {:>12} {:>14} {:>10}",
            stem,
            bench_iters(&stem)
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".into()),
            time_s,
            rate
        );
    }
    if failures > 0 {
        println!("{} benchmark(s) failed", failures);
        return Ok(EXIT_USER_ERROR);
    }
    Ok(EXIT_OK)
}
