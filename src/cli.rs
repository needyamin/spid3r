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
    run [--engine tree|vm] [--prof] [--coverage] [--no-opt] [--budget N]
        <file.matrix>                      Compile and run; --prof (vm) prints
                                           per-opcode execution counts;
                                           --coverage records line coverage;
                                           --no-opt disables bytecode optimizations;
                                           --budget caps the work for untrusted code
    check <file.matrix> [--json]           Lex, parse, semantic-check; no run.
      [--deny-warnings] [--allow LINT]     Lints: unused-variable, unused-parameter,
                                           unused-import, unreachable-code
    compile <file.matrix> [-o out] [--dump] [--no-opt]
                                           Write bytecode (.matrixc); --dump prints it;
                                           --no-opt disables bytecode optimizations
    build <file.matrix> [-o out] [--release] [--icon app.ico]
           [--subsystem console|windows] [--allow-gui] [--embed path]
           [--bytecode app.matrixc] [--bundle-runtime]
                                           Compile to a native machine-code executable;
                                           --bytecode ships compiled bytecode instead of
                                           source, --bundle-runtime makes it run without
                                           an installed Spid3r
    gui <file.matrix> [--html page.html] [--engine tree|vm] [--dry-run] [args...]
    gui --payload <app.exe>                Run a program in a GUI webview window
                                           (--payload runs one embedded by `build
                                           --embed`, straight from the exe; .matrixc
                                           payloads run on the VM). Arguments after the
                                           file reach the program's args()
    bundle <file> [--target wasm|desktop|android] [--html page.html] [-o out]
                                           Package into a distribution (add
                                           --self-contained to embed the runtime,
                                           desktop target only)
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
    modules <file.matrix> [--tree]         List every module this file imports,
                                           with the origin of each: disk, vendor,
                                           embedded or plugin. --tree prints the
                                           dependency graph instead
    install [--update]                     Vendor path/git dependencies into
                                           vendor/, write spider.lock (commit
                                           pinned, SHA-256 per dep); --update
                                           re-resolves instead of honoring the
                                           locked commits
    dist [-o out] [--no-opt]               Compile the project entry (spider.toml)
                                           into target/<package>.matrixc
    add <name> --path P | --git URL [--rev R] | --version V
                                           Declare a dependency in spider.toml and
                                           vendor it (rolled back if install fails)
    remove <name>                          Drop a dependency and its vendor/ copy
    tree                                   Print the dependency tree
    verify                                 Re-hash vendor/ against spider.lock;
                                           exit 1 on drift, naming the package
    search <query> [--local]               Find packages in the local registry and
                                           vendor/ (no network in v1)
    publish --to-dir <dir>                 Write <dir>/<name>/<version>/ with a
                                           sha256.txt, ready for a static registry
    test [--filter SUBSTR] [--list]        Run every examples/*.matrix file (or
                                           a project's tests/*.matrix) through the
                                           three-engine differential
    bench [--engine tree|vm] [--runs N] [--compare] [--gate] [--no-opt] [files...]
                                           Time benchmarks/*.matrix (best of N);
                                           shipped suite lives in benchmarks/
    fmt [--check] [files...]               Canonical formatting (in place);
                                           --check exits 1 if any file differs
    lsp                                    Language Server over stdio
                                           (JSON-RPC: diagnostics, hover,
                                           go-to-definition, symbols,
                                           completion, formatting)
    version                                Print version information
    help                                   Print this help

EXTERNAL COMMANDS:
    spidr-<name> [args...]                 Any executable named `spidr-<name>`
                                           on PATH runs with the remaining
                                           arguments; its exit code becomes
                                           spid3r's. Names may not start with
                                           `-` or contain a path separator.
                                           On Windows only .exe/.com are
                                           dispatched (never .bat/.cmd).

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
    let mut rest: Vec<String> = it.collect();
    // Plugins are discovered from disk once per process, before any engine
    // installs its natives. Only for commands that can run or check a program:
    // `spidr version` must not read a plugin directory.
    if matches!(
        cmd.as_str(),
        "run" | "check" | "compile" | "exec" | "test" | "bench" | "debug" | "gui" | "repl"
    ) {
        load_plugins();
    }
    // `--json` makes the error printer emit one JSON object instead of the
    // human form. It is a *diagnostic* flag, so it is consumed here rather than
    // by a subcommand, and only for commands whose output is diagnostics.
    let json = matches!(cmd.as_str(), "check" | "compile") && rest.iter().any(|a| a == "--json");
    if json {
        rest.retain(|a| a != "--json");
    }
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
        .spawn(move || dispatch(cmd_for_thread, rest_clone, json));
    match spawned {
        Ok(handle) => match handle.join() {
            Ok(outcome) => {
                // Deterministic teardown (audit A-3): every program has already
                // drained its tasks; stop the pool and join the worker threads
                // instead of letting process exit kill them mid-write.
                crate::sched::shutdown();
                outcome_code(outcome, json, &cmd)
            }
            Err(panic) => std::panic::resume_unwind(panic),
        },
        // Thread creation failed (extremely unlikely): run inline as before.
        Err(_) => {
            let log_cmd = cmd.clone();
            let outcome = dispatch(cmd, rest, json);
            crate::sched::shutdown();
            outcome_code(outcome, json, &log_cmd)
        }
    }
}

/// Discover and register plugins from the search path.
///
/// Runs once per process. A plugin that fails to load is a **warning**, not a
/// fatal error: one broken plugin in a shared directory must not stop the
/// toolchain from running a program. `SPID3R_PLUGINS` is the way to pin an
/// exact set when that is not what you want.
fn load_plugins() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Discovery is unconditional: a Spid3r-source plugin needs no wasmi, so
        // a `--no-default-features` build can still use one.
        for e in crate::plugin::load_all() {
            eprintln!("warning: plugin: {}", e);
        }
    });
}

/// Whether an unknown subcommand may be handed to `spidr-<name>`.
///
/// This is a **security boundary**, not a convenience check. The executable
/// path is built by concatenation (`spidr-` + name), so a name carrying a path
/// separator, a drive/UNC prefix, or a `..` component would let
/// `spidr ../../bin/sh` run an arbitrary program. Only a plain token is
/// accepted, it may not begin with `-` (that is an option, and dispatching
/// `spidr --bogus` to a file named `spidr---bogus` would be a trap), and it
/// may not begin with `.` or contain `..`.
fn is_dispatchable_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    if name.starts_with('-') || name.starts_with('.') || name.contains("..") {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Locate `spidr-<name>` on `PATH`.
///
/// On Windows the `PATHEXT` candidates are tried, but **only `.EXE` and
/// `.COM`**. Batch shims (`.BAT`/`.CMD`) are deliberately excluded: `cmd.exe`
/// re-parses the command line it is handed, so a single argument containing
/// `&` or `|` becomes a second command. A tool such as
/// `spidr lint "$PR_FILE"` would then execute whatever a pull request put in
/// the filename. That is a command injection in the toolchain, and no amount
/// of argument quoting makes batch dispatch safe — so a Windows tool must ship
/// a real executable. (Git for Windows refuses to dispatch to batch files for
/// the same reason.)
///
/// On other platforms the bare name must be a file; the OS decides whether it
/// is executable.
///
/// `PATH` entries are split with [`std::env::split_paths`], so the platform
/// separator is honoured. Empty entries are skipped; a *relative* entry such as
/// `.` is honoured as given, exactly as the shell does — putting `.` on `PATH`
/// is the user's decision, and silently ignoring it would make `spidr` behave
/// differently from every other command they run.
fn find_external_tool(name: &str) -> Option<PathBuf> {
    let stem = format!("spidr-{}", name);
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        // `PATHEXT` minus the batch entries; see the doc comment above.
        let from_env: Vec<String> = std::env::var("PATHEXT")
            .map(|v| {
                v.split(';')
                    .map(|s| s.trim().to_ascii_uppercase())
                    .filter(|s| s == ".EXE" || s == ".COM")
                    .collect()
            })
            .unwrap_or_default();
        if from_env.is_empty() {
            vec![".COM".to_string(), ".EXE".to_string()]
        } else {
            from_env
        }
    } else {
        Vec::new()
    };
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let direct = dir.join(&stem);
        if direct.is_file() {
            return Some(direct);
        }
        for ext in &exts {
            let cand = dir.join(format!("{}{}", stem, ext));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Run `spidr-<name>` with the remaining arguments, returning its exit code.
///
/// `None` means "not an external tool" — the caller then reports the ordinary
/// unknown-command usage error. The child inherits stdin/stdout/stderr, so it
/// behaves exactly as if the user had invoked it directly, and its exit code
/// becomes ours (a death by signal maps to the generic user-error code, since
/// Spid3r's own contract only defines 0/1/2).
fn dispatch_external(name: &str, rest: &[String]) -> Option<i32> {
    if !is_dispatchable_name(name) {
        return None;
    }
    let exe = find_external_tool(name)?;
    match std::process::Command::new(&exe).args(rest).status() {
        Ok(status) => Some(status.code().unwrap_or(EXIT_USER_ERROR)),
        Err(e) => {
            eprintln!("error[usage]: cannot run '{}': {}", exe.display(), e);
            Some(EXIT_USAGE)
        }
    }
}

fn dispatch(cmd: String, rest: Vec<String>, json: bool) -> Result<i32, Diagnostic> {
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        if let Some(help_text) = subcommand_help(&cmd) {
            print!("{}", help_text);
            return Ok(EXIT_OK);
        }
    }

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
        "check" => cmd_check_with(&rest, json),
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
        "lsp" => Ok(crate::lsp::cmd_lsp()),
        "init" => cmd_init(&rest),
        "modules" => cmd_modules(&rest),
        "verify" => cmd_verify(&rest),
        "tree" => cmd_tree(&rest),
        "add" => cmd_add(&rest),
        "remove" => cmd_remove(&rest),
        "search" => cmd_search(&rest),
        "publish" => cmd_publish(&rest),
        "install" => cmd_install(&rest),
        "dist" => cmd_project_build(&rest),
        "pack" => cmd_pack(&rest),
        "unpack" => cmd_unpack(&rest),
        "debug" => cmd_debug(&rest),
        other => {
            // `spidr-<name>` extension tools. Checked before the usage error so
            // an installed external command is indistinguishable from a
            // built-in one; `--help` above already gave the tool its chance.
            if let Some(code) = dispatch_external(other, &rest) {
                return Ok(code);
            }
            eprintln!("error[usage]: unknown command '{}'\n{}", other, HELP);
            return Ok(EXIT_USAGE);
        }
    };
    outcome
}

fn outcome_code(outcome: Result<i32, Diagnostic>, json: bool, cmd: &str) -> i32 {
    match outcome {
        Ok(code) => code,
        Err(d) => {
            // `gui` is the one command whose output can be invisible: a packaged
            // app is started by a windows-subsystem launcher and the runtime it
            // spawns has no console, so stderr goes nowhere. Every other
            // command runs in a terminal that already shows the diagnostic —
            // logging those too would fill the user's log with ordinary
            // `check` failures.
            if cmd == "gui" {
                crate::logging::record(
                    "cli",
                    &format!("{}: {}", d.phase.as_str(), one_line(&d.message)),
                );
            }
            if json {
                // One object per line: `spid3r check --json file.matrix` is
                // consumable by an editor or a CI annotation step.
                println!("{}", d.to_json());
            } else {
                // `render` adds the source snippet and the help/note lines.
                eprintln!("{}", d.render());
            }
            if d.phase == Phase::Usage {
                EXIT_USAGE
            } else {
                EXIT_USER_ERROR
            }
        }
    }
}

/// Collapse a multi-line diagnostic so one log entry stays one line.
fn one_line(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut last_space = false;
    for c in message.chars() {
        let c = if c.is_control() { ' ' } else { c };
        if c == ' ' {
            if last_space {
                continue;
            }
            last_space = true;
        } else {
            last_space = false;
        }
        out.push(c);
    }
    out.trim().to_string()
}

/// Per-subcommand `--help` text. This is the **only** copy: `dispatch` answers
/// `--help` here, and `native::link::cmd_build` prints the same string when it
/// is called directly, so the two cannot drift apart.
pub(crate) fn subcommand_help(cmd: &str) -> Option<&'static str> {
    match cmd {
        "run" => Some("\
spid3r run - Compile and run a program

USAGE:
    spid3r run [options] <file.matrix> [args...]

OPTIONS:
    --engine <tree|vm>     Execution engine: tree-walk or bytecode VM (default: tree)
    --prof                 Print per-opcode VM execution counts (requires --engine vm)
    --no-opt               Disable bytecode optimizer (VM engine)
    --coverage             Enable code coverage tracking
    --coverage-out <file>  Write coverage report to file
    --budget <n>           Stop after roughly N steps with the fatal error
                           'execution budget exhausted' (for untrusted code;
                           not enforced by the AOT backend)
    -h, --help             Print help information
"),
        "check" => Some("\
spid3r check - Lex, parse, and semantic-check without running

USAGE:
    spid3r check <file.matrix> [options]

OPTIONS:
    --json                 Emit diagnostics as JSON objects
    --deny-warnings        Treat warnings as errors
    --allow <lint>         Silence a specific lint (unused-variable, unused-parameter, unused-import, unreachable-code)
    -h, --help             Print help information
"),
        "compile" => Some("\
spid3r compile - Compile to bytecode (.matrixc)

USAGE:
    spid3r compile <file.matrix> [options]

OPTIONS:
    -o <path>              Output bytecode file path
    --dump                 Disassemble and print bytecode
    --no-opt               Disable bytecode optimizer
    -h, --help             Print help information
"),
        "build" => Some("\
spid3r build - Compile to a native machine-code executable

USAGE:
    spid3r build <file.matrix> [options]

OPTIONS:
    -o <path>              Output executable path (an extensionless path gains the
                           platform suffix: `-o app` writes `app.exe`)
    --target <triple>      Target triple; must be the host's (cross-linking is not
                           supported — build on the target OS)
    --release              Compile with optimizations
    --icon <path>          Application icon (.ico); a missing or unreadable file is
                           an error, not a silently skipped step
    --subsystem <kind>     Windows subsystem: 'console' (default) or 'windows'
                           ('windows' removes the terminal window; Windows only)
    --link-lib <name|path> Link a C library so `extern fn` declarations resolve
                           (repeatable). A bare name means -l<name> on Unix and
                           <name>.lib under MSVC; a path is passed as given
    --allow-gui            Build even though GUI natives are not linked in AOT
                           builds (they resolve to null at runtime — use this for
                           a launcher that hands off to `spid3r gui`)
    --embed <path>         Embed a file or directory into the executable, keeping
                           its path relative to the source file (repeatable;
                           relative paths resolve beside the source). Then
                           `spid3r gui --payload <exe>` runs it from inside the
                           binary, so the app ships as one file
    --bytecode <file.matrixc>
                           Ship a compiled program instead of its source. The
                           launcher is compiled from a three-line stub, so no
                           application code, symbol names or string literals
                           reach the native image; the payload carries the
                           bytecode and `gui --payload` runs it on the VM. The
                           file is validated here, not at first launch
    --bundle-runtime       Also embed the running spid3r executable, so the
                           built app starts its own runtime copy and runs on a
                           machine with no Spid3r installed (implies a payload;
                           requires a program in it)
    --product-name <name>  Application product name metadata
    --version <version>    Application version string metadata
    -h, --help             Print help information
"),
        "gui" => Some("\
spid3r gui - Run a program in a GUI webview window

USAGE:
    spid3r gui <file.matrix> [options] [args...]
    spid3r gui <file.matrixc> [options] [args...]
    spid3r gui --payload <app.exe> [options] [args...]

OPTIONS:
    --html <path>          Load an existing HTML page into the webview window.
                           With --payload: a payload entry name, or a file on disk
    --payload <exe>        Run the program embedded in <exe> by `spid3r build
                           --embed`, serving its assets from memory
    --engine <tree|vm>     Engine to run the program on. `vm` is required for
                           .matrixc bytecode and runs the same engine as
                           `spid3r exec`
    --persist              Keep the webview profile on disk. `--payload` is
                           ephemeral by default so embedded assets are never
                           cached; this restores localStorage persistence
    --dry-run              Replay the GUI bridge and print queued JavaScript without opening a window
    -h, --help             Print help information

ARGS:
    Arguments after the program file (and after --payload) are passed to the
    program's args(); flags are not.
"),
        "bundle" => Some("\
spid3r bundle - Package into a distribution (add --self-contained to embed the runtime)

USAGE:
    spid3r bundle <file.matrix> [options]

OPTIONS:
    --target <target>      Target platform: wasm, desktop, android (default: wasm)
    --html <path>          HTML template or page to package
    -o <path>              Output directory or file path
    --single-file          Emit self-contained single-file HTML bundle (wasm target)
    --self-contained       Embed the spid3r runtime in the bundle and point the
                           launcher at it (desktop target); runs with no global install
    -h, --help             Print help information
"),
        "exec" => Some("\
spid3r exec - Run bytecode on the virtual machine

USAGE:
    spid3r exec <file.matrixc>

OPTIONS:
    -h, --help             Print help information
"),
        "repl" => Some("\
spid3r repl - Interactive read-eval-print loop

USAGE:
    spid3r repl

OPTIONS:
    -h, --help             Print help information
"),
        "test" => Some("\
spid3r test - Run differential tests

USAGE:
    spid3r test [options]

OPTIONS:
    --filter <substr>      Run tests matching substring
    --list                 List tests without executing
    -h, --help             Print help information
"),
        "bench" => Some("\
spid3r bench - Run benchmarks

USAGE:
    spid3r bench [options] [files...]

OPTIONS:
    --engine <tree|vm>     Benchmark tree or vm engine
    --runs <n>             Number of iterations (best of N)
    --compare              Compare tree vs vm
    --gate                 Fail if performance gates regress
    -h, --help             Print help information
"),
        "fmt" => Some("\
spid3r fmt - Canonical formatting

USAGE:
    spid3r fmt [options] [files...]

OPTIONS:
    --check                Exit with code 1 if any file differs without modifying files
    -h, --help             Print help information
"),
        "lsp" => Some("\
spid3r lsp - Language Server Protocol over stdio

USAGE:
    spid3r lsp

OPTIONS:
    -h, --help             Print help information
"),
        "init" => Some("\
spid3r init - Initialize a new Spid3r project

USAGE:
    spid3r init [dir]

OPTIONS:
    -h, --help             Print help information
"),
        "modules" => Some("\
spid3r modules - List the modules a program imports

USAGE:
    spidr modules <file.matrix> [--tree]

OPTIONS:
    --tree                 Print the dependency graph instead of a flat list
    -h, --help             Print help information

Each line is `<origin><TAB><path>`. The origin is where the resolver found the
module: `disk` (beside the importer or on the search path), `vendor` (from
vendor/), `embedded` (the stdlib table compiled into the binary) or `plugin`.
A module imported by two others is listed once and marked `(*)` in the tree.
"),
        "install" => Some("\
spid3r install - Install and vendor dependencies

USAGE:
    spid3r install [options]

OPTIONS:
    --update               Re-resolve and update dependencies
    -h, --help             Print help information
"),
        "dist" => Some("\
spid3r dist - Compile project into target package

USAGE:
    spid3r dist [options]

OPTIONS:
    -o <path>              Output artifact path
    --no-opt               Disable bytecode optimization
    -h, --help             Print help information
"),
        "pack" => Some("\
spid3r pack - Archive project

USAGE:
    spid3r pack [-o out.tar]

OPTIONS:
    -o <path>              Output archive path
    -h, --help             Print help information
"),
        "unpack" => Some("\
spid3r unpack - Extract a packed project

USAGE:
    spid3r unpack <archive.tar> [dir]

OPTIONS:
    -h, --help             Print help information
"),
        "debug" => Some("\
spid3r debug - Line-based debugger on the VM

USAGE:
    spid3r debug <file.matrix> [options]

OPTIONS:
    --break <lines>        Comma-separated breakpoints (e.g. 10,25)
    --commands <file>      Read debugger commands from file
    -h, --help             Print help information
"),
        _ => None,
    }
}

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
    let warnings = semantic::check_file_with_warnings(&program, true, path)?;
    for w in &warnings {
        if w.message.contains("explicit call to 'main'") {
            eprintln!("{}", w.render());
        }
    }
    Ok((program, file))
}

/// Load and run a program on the reference runner (tree-walk).
fn run_tree(path: &str, budget: Option<u64>) -> Result<i32, Diagnostic> {
    let (program, file) = front_end(path)?;
    let dirs = modules::default_module_dirs();
    let mut interp = Interp::new(file, dirs);
    interp.set_budget(budget.map(crate::budget::Budget::new));
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
    let mut prof = false;
    let mut coverage = false;
    let mut coverage_out: Option<String> = None;
    let mut budget: Option<u64> = None;
    let mut script_args: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-opt" => no_opt = true,
            "--prof" => prof = true,
            "--coverage" => coverage = true,
            "--budget" => {
                i += 1;
                match args.get(i) {
                    Some(v) => match v.parse::<u64>() {
                        Ok(n) => budget = Some(n),
                        Err(_) => {
                            return Err(usage(&format!(
                                "--budget expects a non-negative step count, got '{}'",
                                v
                            )))
                        }
                    },
                    None => return Err(usage("--budget needs a step count")),
                }
            }
            "--coverage-out" => {
                i += 1;
                match args.get(i) {
                    Some(p) => coverage_out = Some(p.clone()),
                    None => return Err(usage("--coverage-out needs a file path")),
                }
                coverage = true;
            }
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
        "tree" => {
            if prof {
                return Err(usage(
                    "--prof is a VM-engine feature; rerun with --engine vm",
                ));
            }
            if coverage {
                return Err(usage(
                    "--coverage is a VM-engine feature; rerun with --engine vm",
                ));
            }
            run_tree(&file, budget)
        }
        "vm" => {
            let (program, file_name) = front_end(&file)?;
            let dirs = modules::default_module_dirs();
            let (result, report, cov) = if no_opt {
                let (script, _public) =
                    crate::vm::compile_program_unoptimized(&program, &file, &dirs)?;
                let mut vm = Vm::new(file_name, dirs);
                vm.set_budget(budget.map(crate::budget::Budget::new));
                if prof {
                    vm.enable_profiling();
                }
                if coverage {
                    vm.enable_coverage();
                }
                let r = vm.run_compiled(Arc::new(script));
                let report = vm.profiling_report();
                let cov = vm
                    .coverage()
                    .map(|c| (c.report(), c.lcov()))
                    .unwrap_or_default();
                (r, report, cov)
            } else {
                let mut vm = Vm::new(file_name, dirs);
                vm.set_budget(budget.map(crate::budget::Budget::new));
                if prof {
                    vm.enable_profiling();
                }
                if coverage {
                    vm.enable_coverage();
                }
                let r = vm.run_program(&program);
                let report = vm.profiling_report();
                let cov = vm
                    .coverage()
                    .map(|c| (c.report(), c.lcov()))
                    .unwrap_or_default();
                (r, report, cov)
            };
            if prof {
                eprint!("{}", report);
            }
            if coverage {
                // The table goes to stderr so a program's stdout stays exactly
                // what it would be without the flag (differential tests rely
                // on that).
                eprint!("{}", cov.0);
                if let Some(path) = &coverage_out {
                    if let Err(e) = std::fs::write(path, &cov.1) {
                        eprintln!("cannot write coverage to '{}': {}", path, e);
                    }
                }
            }
            result
        }
        _ => unreachable!(),
    }
}

fn cmd_check(args: &[String]) -> Result<i32, Diagnostic> {
    cmd_check_with(args, false)
}

fn cmd_check_with(args: &[String], json: bool) -> Result<i32, Diagnostic> {
    // `--dump-lowered` prints what the backends actually receive. With an
    // identity lowering pass that is the same tree, but it is the only way to
    // see a desugaring rule's output without reading engine code.
    let mut dump = false;
    let mut deny_warnings = false;
    let mut allow: Vec<String> = Vec::new();
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dump-lowered" => dump = true,
            "--deny-warnings" => deny_warnings = true,
            "--allow" => {
                let name = args
                    .get(i + 1)
                    .ok_or_else(|| usage("--allow needs a lint name (e.g. unused-variable)"))?;
                allow.push(name.clone());
                i += 1;
            }
            other if other.starts_with("--") => {
                return Err(usage(&format!(
                    "check: unknown flag '{}' (supported: --json, --dump-lowered, \
                     --deny-warnings, --allow LINT)",
                    other
                )))
            }
            other => positional.push(other.to_string()),
        }
        i += 1;
    }
    let file = single_file(&positional, "check")?;
    let (src, file_name) = read_source(&file)?;
    let program = parser::parse_file(&src, file_name)?;
    // a file without fn main is a library module: checked without the main rule
    let has_main = program.stmts.iter().any(|s| match s {
        crate::ast::Stmt::Fn(f) => f.name.as_deref() == Some("main"),
        _ => false,
    });
    // `check` is the only command that surfaces lint warnings; every other
    // entry point keeps using the validity-only API.
    let warnings = semantic::check_file_with_warnings(&program, has_main, &file)?;
    let mut reported = 0usize;
    for w in &warnings {
        if allow.iter().any(|a| a == w.lint.as_str()) {
            continue;
        }
        reported += 1;
        if json {
            println!("{}", w.to_json());
        } else {
            eprintln!("{}", w.render());
        }
    }
    if dump {
        let lowered = crate::lower::lower(&program);
        println!("{:#?}", lowered.as_ref());
    }
    if deny_warnings && reported > 0 {
        return Err(Diagnostic::new(
            Phase::Semantic,
            format!(
                "{} warning(s) treated as errors because --deny-warnings was given",
                reported
            ),
        ));
    }
    // C-01: under `--json` stdout must be machine-readable and nothing else —
    // the human `file: ok` summary line made a clean file unparseable and
    // mixed text into an otherwise-JSONL stream. The summary stays on the
    // default (human) output only.
    if !json {
        println!("{}: ok", file);
    }
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
        let mut self_contained = false;

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
                "--self-contained" => {
                    self_contained = true;
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
        if single_file && target != crate::bundle::BundleTarget::Wasm {
            return Err(crate::diagnostics::Diagnostic::new(
                crate::diagnostics::Phase::Usage,
                "--single-file applies only to --target wasm",
            ));
        }
        if self_contained && target != crate::bundle::BundleTarget::Desktop {
            return Err(crate::diagnostics::Diagnostic::new(
                crate::diagnostics::Phase::Usage,
                "--self-contained applies only to --target desktop",
            ));
        }
        let opts = crate::bundle::BundleOptions {
            source_file,
            target,
            html_file,
            output_path: out,
            single_file,
            self_contained,
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
        Some(o) => {
            // An extensionless `-o` path used to write an unidentifiable
            // bytecode blob that `exec` and every doc referred to as
            // `.matrixc` (F-2). Respect an explicit extension; add the
            // canonical one when the user gave none.
            if Path::new(o).extension().is_none() {
                format!("{}.matrixc", o)
            } else {
                o.to_string()
            }
        }
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

/// Options for `spid3r test`.
struct TestOpts {
    /// Only run files whose name contains this substring.
    filter: Option<String>,
    /// Print the files that would run, without running them.
    list: bool,
}

fn parse_test_args(args: &[String]) -> Result<TestOpts, Diagnostic> {
    let mut opts = TestOpts {
        filter: None,
        list: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--filter" | "-f" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| usage("--filter needs a substring to match"))?;
                opts.filter = Some(v.clone());
                i += 2;
            }
            "--list" => {
                opts.list = true;
                i += 1;
            }
            other => {
                return Err(usage(&format!(
                    "test: unknown argument '{}' (supported: --filter SUBSTR, --list)",
                    other
                )))
            }
        }
    }
    Ok(opts)
}

/// Does `path` pass the `--filter` substring?
fn test_selected(opts: &TestOpts, path: &Path) -> bool {
    match &opts.filter {
        None => true,
        Some(needle) => path
            .file_stem()
            .map(|s| s.to_string_lossy().contains(needle.as_str()))
            .unwrap_or(false),
    }
}

fn cmd_test(args: &[String]) -> Result<i32, Diagnostic> {
    // Audit: the argument vector used to be discarded (`_args`), so
    // `spid3r test whatever` silently ran everything.
    let opts = parse_test_args(args)?;
    let root = PathBuf::from("examples");
    if !root.is_dir() {
        if Path::new("spider.toml").is_file() {
            let manifest = crate::pkg::Manifest::load(Path::new("."))
                .map_err(|e| usage(&e))?
                .ok_or_else(|| usage("cannot read spider.toml"))?;
            let (total_ok, total_failed) = if manifest.members.is_empty() {
                project_test_in(Path::new("."), &opts)?
            } else {
                // Workspace: test every member in its own directory.
                let mut ok = 0;
                let mut failed = 0;
                for member in &manifest.members {
                    println!("== member {} ==", member);
                    let (o, f) = project_test_in(Path::new(member), &opts)?;
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
    files.retain(|p| test_selected(&opts, p));
    if opts.list {
        for f in &files {
            println!("{}", f.display());
        }
        return Ok(EXIT_OK);
    }
    let mut ok = 0;
    let mut failed = 0;
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let path = f.to_string_lossy().to_string();
        // Program vs library: decide by the AST, not a fragile string search.
        let is_program = has_main_fn(&path);
        let result: Result<(), String> = if is_program {
            // Differential test: all three engines must produce identical stdout.
            // `to_str_lossy` (not `to_str().unwrap()`): a file whose name is not
            // valid UTF-8 is still a legal test file on POSIX, and `has_main_fn`
            // already read it through the lossy path — panicking here would be
            // inconsistent with that decision.
            let arg = f.to_string_lossy().to_string();
            let tree = run_capture(&bin, &["run", "--engine", "tree", &arg]);
            let vm = run_capture(&bin, &["run", "--engine", "vm", &arg]);
            let tc = std::env::temp_dir()
                .join(format!("spid3r_test_{}.matrixc", name.replace('.', "_")));
            let tc = tc.to_string_lossy().to_string();
            let compiled = run_capture(&bin, &["compile", &arg, "-o", &tc]);
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
/// Render a tree-vs-vm output divergence for `spid3r test`.
///
/// Kept as a function rather than an inline `format!` so the layout is
/// assertable: the message once opened with a literal `//n`, which welded the
/// heading onto the first line of tree's output and made the diff unreadable.
fn differ_report(tree: &str, vm: &str) -> String {
    format!(
        "outputs differ:\n--- tree ---\n{}\n--- vm ---\n{}",
        tree, vm
    )
}

fn project_test_in(dir: &Path, opts: &TestOpts) -> Result<(usize, usize), Diagnostic> {
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
    files.retain(|p| test_selected(opts, p));
    if opts.list {
        for f in &files {
            println!("{}", f.display());
        }
        return Ok((0, 0));
    }
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
                (Ok(t), Ok(v)) => Err(differ_report(&t, &v)),
                (Err(tree), Err(vm)) => Err(format!(
                    "both engines failed:\n--- tree ---\n{}\n--- vm ---\n{}",
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
    if commands.is_empty() && !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        use std::io::Read;
        let mut text = String::new();
        if std::io::stdin().read_to_string(&mut text).is_ok() {
            let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
            commands = text
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .collect();
        }
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
    match vm.debug_bootstrap() {
        Ok(true) => {}
        Ok(false) => return Ok(0),
        Err(_) if vm.debug_quit() => {
            println!("debugger: quit after {} breakpoint hit(s)", vm.debug_hits());
            return Ok(3);
        }
        Err(diag) => return Err(diag),
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
/// The project root: the working directory, which must hold a `spider.toml`.
fn project_root() -> Result<PathBuf, Diagnostic> {
    let root = std::env::current_dir().map_err(|e| usage(&format!("cannot read cwd: {}", e)))?;
    if !root.join("spider.toml").is_file() {
        return Err(usage(
            "no spider.toml in this directory — run `spidr init` first",
        ));
    }
    Ok(root)
}

/// `spidr verify`: re-hash every vendored dependency against `spider.lock`.
///
/// Exit 1 on any drift, with the offending package named — this is the command
/// a CI job runs after `install` to prove the tree it compiled is the tree the
/// lock file describes.
fn cmd_verify(args: &[String]) -> Result<i32, Diagnostic> {
    if let Some(a) = args.first() {
        return Err(usage(&format!("unknown argument '{}'", a)));
    }
    let root = project_root()?;
    let report = crate::pkg::ops::verify(&root).map_err(|e| usage(&e))?;
    if report.ok() {
        println!(
            "ok: {} vendored package(s) match spider.lock",
            report.checked
        );
        return Ok(EXIT_OK);
    }
    for issue in &report.issues {
        eprintln!("error[verify]: {}: {}", issue.name, issue.detail);
    }
    eprintln!(
        "{} problem(s) across {} checked package(s)",
        report.issues.len(),
        report.checked
    );
    Ok(EXIT_USER_ERROR)
}

/// `spidr tree`: the declared dependency tree.
fn cmd_tree(args: &[String]) -> Result<i32, Diagnostic> {
    if let Some(a) = args.first() {
        return Err(usage(&format!("unknown argument '{}'", a)));
    }
    let root = project_root()?;
    print!(
        "{}",
        crate::pkg::ops::dep_tree(&root).map_err(|e| usage(&e))?
    );
    Ok(EXIT_OK)
}

/// `spidr add <name> (--path P | --git URL [--rev R] | --version V)`.
///
/// The manifest is edited first, then `install` runs. If install fails the
/// manifest edit is rolled back, so a typo cannot leave the project in a state
/// that does not resolve.
fn cmd_add(args: &[String]) -> Result<i32, Diagnostic> {
    let mut name: Option<String> = None;
    let mut path: Option<String> = None;
    let mut git: Option<String> = None;
    let mut rev: Option<String> = None;
    let mut version: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let need = |i: usize| -> Result<String, Diagnostic> {
            args.get(i + 1)
                .cloned()
                .ok_or_else(|| usage(&format!("{} needs a value", args[i])))
        };
        match args[i].as_str() {
            "--path" => {
                path = Some(need(i)?);
                i += 1;
            }
            "--git" => {
                git = Some(need(i)?);
                i += 1;
            }
            "--rev" => {
                rev = Some(need(i)?);
                i += 1;
            }
            "--version" => {
                version = Some(need(i)?);
                i += 1;
            }
            a if a.starts_with('-') => return Err(usage(&format!("unknown argument '{}'", a))),
            a => {
                if name.is_some() {
                    return Err(usage("add takes exactly one package name"));
                }
                name = Some(a.to_string());
            }
        }
        i += 1;
    }
    let name = name.ok_or_else(|| usage("add needs a package name"))?;
    if path.is_some() && git.is_some() {
        return Err(usage("--path and --git are mutually exclusive"));
    }
    let value = match (&path, &git, &version) {
        (Some(p), None, _) => format!("{{ path = \"{}\" }}", p),
        (None, Some(url), _) => match &rev {
            Some(r) => format!("{{ git = \"{}\", rev = \"{}\" }}", url, r),
            None => format!("{{ git = \"{}\" }}", url),
        },
        (None, None, Some(v)) => format!("{{ version = \"{}\" }}", v),
        (None, None, None) => {
            return Err(usage(
                "add needs a source: --path P, --git URL, or --version V",
            ))
        }
        // Rejected above; listed so the match stays exhaustive if that guard
        // ever moves.
        (Some(_), Some(_), _) => return Err(usage("--path and --git are mutually exclusive")),
    };
    // A `--version` alongside --path/--git is a constraint; on its own it is
    // the source, already folded into `value` above.
    let constraint = match (&path, &git) {
        (Some(_), _) | (_, Some(_)) => version.as_deref(),
        _ => None,
    };

    let root = project_root()?;
    // Snapshot the manifest so a failed install can restore the exact bytes,
    // not a re-serialised approximation of them.
    let manifest_path = root.join("spider.toml");
    let before = std::fs::read(&manifest_path)
        .map_err(|e| usage(&format!("cannot read spider.toml: {}", e)))?;
    let changed =
        crate::pkg::ops::add_dependency(&root, &name, &value, constraint).map_err(|e| usage(&e))?;
    if !changed {
        println!("{} is already declared in spider.toml", name);
        return Ok(EXIT_OK);
    }
    match crate::pkg::install(&root) {
        Ok(report) => {
            println!("added {}", name);
            println!(
                "vendored {} package(s), {} unchanged",
                report.vendored, report.unchanged
            );
            Ok(EXIT_OK)
        }
        Err(e) => {
            // Roll back to the exact previous document: a dependency that does
            // not resolve must not stay in the manifest, and a partial edit
            // (an empty `[dependencies]` header) is still a change.
            let _ = std::fs::write(&manifest_path, &before);
            Err(usage(&format!(
                "cannot add '{}': {}\n(the manifest was left unchanged)",
                name, e
            )))
        }
    }
}

/// `spidr remove <name>`: drop the declaration and its vendored copy.
fn cmd_remove(args: &[String]) -> Result<i32, Diagnostic> {
    let name = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| usage("remove needs a package name"))?
        .clone();
    if args.iter().filter(|a| !a.starts_with('-')).count() > 1 {
        return Err(usage("remove takes exactly one package name"));
    }
    let root = project_root()?;
    let removed = crate::pkg::ops::remove_dependency(&root, &name).map_err(|e| usage(&e))?;
    if !removed {
        eprintln!("warning: '{}' is not declared in spider.toml", name);
        return Ok(EXIT_USER_ERROR);
    }
    // Prune the vendored copy so `verify` does not report it as unlocked.
    crate::pkg::valid_pkg_name(&name).map_err(|e| usage(&e))?;
    let dir = root.join("vendor").join(&name);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| usage(&format!("cannot remove {}: {}", dir.display(), e)))?;
    }
    // Rewrite `spider.lock` without the removed package. Without this, the lock
    // still names a vendored directory that no longer exists and `verify`
    // reports it missing — a remove that leaves the project unverifiable.
    if let Err(e) = crate::pkg::install(&root) {
        eprintln!("warning: could not rewrite spider.lock: {}", e);
    }
    println!("removed {}", name);
    Ok(EXIT_OK)
}

/// `spidr search <query> [--local]`.
///
/// v1 is local-only: the local registry (`SPID3R_REGISTRY`) and `vendor/`. A
/// remote index is P8 in `plan.txt`; saying "no results" for a network search
/// would be a lie, so `--local` is accepted and the remote case says so.
fn cmd_search(args: &[String]) -> Result<i32, Diagnostic> {
    let local = args.iter().any(|a| a == "--local");
    let query = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .ok_or_else(|| usage("search needs a query"))?
        .clone();
    if !local {
        eprintln!(
            "note: only the local registry is searched in this version (see docs/REGISTRY.md)"
        );
    }
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let found = crate::pkg::ops::search_local(&root, &query).map_err(|e| usage(&e))?;
    if found.is_empty() {
        println!("no packages matching '{}'", query);
        return Ok(EXIT_OK);
    }
    for (name, origin) in found {
        println!("{}\t{}", name, origin);
    }
    Ok(EXIT_OK)
}

/// `spidr publish --to-dir <dir>`: write a registry-shaped package tree.
fn cmd_publish(args: &[String]) -> Result<i32, Diagnostic> {
    let mut to_dir: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--to-dir" => {
                to_dir = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| usage("--to-dir needs a path"))?,
                );
                i += 1;
            }
            a => return Err(usage(&format!("unknown argument '{}'", a))),
        }
        i += 1;
    }
    let to_dir =
        to_dir.ok_or_else(|| usage("publish needs a destination: spidr publish --to-dir <dir>"))?;
    let root = project_root()?;
    let (dest, hash) =
        crate::pkg::ops::publish_to_dir(&root, &PathBuf::from(&to_dir)).map_err(|e| usage(&e))?;
    println!("published to {}", dest.display());
    println!("sha256: {}", hash);
    Ok(EXIT_OK)
}

/// `spidr modules <file> [--tree]`: make the resolver's decisions inspectable.
///
/// Prints every module reachable from `file` with the origin the resolver
/// assigned it. Nothing is executed and nothing is type-checked — this is the
/// command you reach for when a build fails and you need to know which file an
/// `import` actually named.
fn cmd_modules(args: &[String]) -> Result<i32, Diagnostic> {
    let mut tree = false;
    let mut file: Option<String> = None;
    for a in args {
        match a.as_str() {
            "--tree" => tree = true,
            other if other.starts_with('-') => {
                return Err(usage(&format!("unknown argument '{}'", other)))
            }
            other => {
                if file.is_some() {
                    return Err(usage("modules takes exactly one file"));
                }
                file = Some(other.to_string());
            }
        }
    }
    let file = file.ok_or_else(|| usage("modules needs a file: spidr modules <file.matrix>"))?;
    let path = PathBuf::from(&file);
    if !path.is_file() {
        return Err(usage(&format!("'{}' is not a file", file)));
    }

    // No extra dirs: `resolve_module` already searches the documented roots,
    // including `SPID3R_PATH`, `./stdlib`, the shipped stdlib and `vendor/`.
    let graph = crate::modules::graph::build(&path, &[])?;

    if tree {
        print!("{}", crate::modules::graph::render_tree(&graph));
        return Ok(EXIT_OK);
    }
    println!("{}", crate::modules::graph::render_flat(&graph));
    let counts = graph.count_by_origin();
    let summary: Vec<String> = counts
        .iter()
        .map(|(origin, n)| format!("{} {}", n, origin.as_str()))
        .collect();
    println!(
        "{} module(s) ({}); {} import edge(s)",
        graph.nodes.len(),
        summary.join(", "),
        graph.nodes.iter().map(|n| n.imports.len()).sum::<usize>()
    );
    Ok(EXIT_OK)
}

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
/// `.matrix` file under examples/ and benchmarks/ (the latter when present —
/// the shipped set lives in benchmarks/).
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
                eprintln!("{}", d.render());
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

/// Perf-gate floors (CI-01): minimum acceptable iters/sec per benchmark, used
/// by `spid3r bench --gate` (the CI perf job). Calibrated at roughly 1/3 of
/// the WORST single-run rate observed on reference hardware (best-of-5
/// release numbers are several times higher still) — they exist to catch
/// CATASTROPHIC regressions (a 3x+ slowdown) on shared CI runners, not small
/// drift; docs/ROADMAP.md's table is the record for real tuning. A benchmark file
/// added to BENCH_ITERS without a floor simply is not gated.
const BENCH_FLOORS: &[(&str, u64)] = &[
    ("arith", 6_000_000),
    ("strings", 140_000),
    ("list", 800_000),
    ("map", 160_000),
    ("methods", 500_000),
    ("closures", 170_000),
    ("json", 8_000),
    ("tasks", 6_000),
];

fn bench_floor(stem: &str) -> Option<u64> {
    BENCH_FLOORS
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
    let mut gate = false;
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
            "--gate" => gate = true,
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
                "no benchmarks/ directory found, or pass benchmark files explicitly",
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
    let mut rates: Vec<(String, Option<f64>)> = Vec::new();
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
        let rate_val = bench_iters(&stem).map(|iters| iters as f64 / secs);
        rates.push((stem.clone(), rate_val));
        let (time_s, rate) = match rate_val {
            Some(r) => (format!("{:.3?}", time), format!("{:.0}", r)),
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
    if gate {
        // CI-01: fail the run when any benchmark falls below its floor.
        println!();
        println!("perf gate (floors catch catastrophic regressions on shared runners):");
        let mut gate_failures = 0;
        for (stem, rate) in &rates {
            match (*rate, bench_floor(stem)) {
                (Some(r), Some(floor)) if r >= floor as f64 => {
                    println!("{:<12} PASS  ({:.0} >= {} iters/sec)", stem, r, floor);
                }
                (Some(r), Some(floor)) => {
                    println!(
                        "{:<12} FAIL  ({:.0} < {} iters/sec — investigate before merging)",
                        stem, r, floor
                    );
                    gate_failures += 1;
                }
                (None, Some(_)) => {
                    println!(
                        "{:<12} FAIL  (no iters/sec — add the file to BENCH_ITERS)",
                        stem
                    );
                    gate_failures += 1;
                }
                (_, None) => {}
            }
        }
        if gate_failures > 0 {
            println!("{} perf gate violation(s)", gate_failures);
            return Ok(EXIT_USER_ERROR);
        }
        println!("perf gate passed");
    }
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::{differ_report, is_dispatchable_name};

    /// The dispatch filter is a security boundary, so it is pinned directly:
    /// deleting any clause of `is_dispatchable_name` must fail here.
    #[test]
    fn only_plain_tokens_are_dispatchable() {
        for ok in ["fmt-ext", "lint2", "a_b", "tool.v2", "x"] {
            assert!(is_dispatchable_name(ok), "{ok:?} should be dispatchable");
        }
        for bad in [
            "",        // empty
            "--bogus", // an option, not a subcommand
            "-v",      // short option
            ".hidden", // a hidden file, not a command
            "..",      // traversal
            "../echo", // traversal with a target
            "a/b",     // separator (POSIX)
            "a\\b",    // separator (Windows)
            "C:/x",    // drive prefix
            "a b",     // whitespace would split the argv entry
            "a&b",     // cmd metacharacter
            "a|b",     // cmd metacharacter
            "a$b",     // shell metacharacter
        ] {
            assert!(!is_dispatchable_name(bad), "{bad:?} must be rejected");
        }
        // The length cap keeps the concatenated executable name sane.
        assert!(is_dispatchable_name(&"a".repeat(64)));
        assert!(!is_dispatchable_name(&"a".repeat(65)));
    }

    #[test]
    fn differ_report_is_readable() {
        // The differential message once opened with a literal `//n` instead of
        // a newline, which welded "--- tree ---" onto the first line of tree's
        // output and made the diff unreadable. Assert the layout directly.
        let msg = differ_report("a\nb", "a\nc");
        let lines: Vec<&str> = msg.lines().collect();
        assert_eq!(
            lines,
            vec![
                "outputs differ:",
                "--- tree ---",
                "a",
                "b",
                "--- vm ---",
                "a",
                "c"
            ],
            "unexpected layout: {:?}",
            msg
        );
        assert!(
            !msg.contains("//n"),
            "must not contain a literal //n: {}",
            msg
        );

        // Captured output ends in a newline, so the separator keeps a blank
        // line between the last output line and the next heading.
        let msg = differ_report("a\n", "b\n");
        assert!(
            msg.contains("a\n\n--- vm ---"),
            "no blank separator: {:?}",
            msg
        );
    }
}
