//! GUI layer (Phase 6 of the plan): run a Spid3r program inside a desktop
//! window with an embedded webview.
//!
//! The model is deliberately small:
//!
//! 1. `spid3r gui prog.matrix` runs the program on the tree interpreter
//!    **first**. The program builds the UI: it registers event handlers with
//!    `gui_on` and queues initial JavaScript with `gui_eval` / `gui_set_html`.
//! 2. A `tao` window opens with a `wry` webview showing a page (built-in by
//!    default, or `--html page.html`).
//! 3. When the page is up it receives a `ready` event; the program's queued
//!    JavaScript is then evaluated in order.
//! 4. The page talks back through `window.spid3r.emit(event, payload)`. The
//!    window's event loop dispatches each event to the registered handler as
//!    `handler(event, payload)` and evaluates any JavaScript it queues.
//!
//! `--dry-run` performs the whole flow without a window: queued JavaScript is
//! printed (`js: …`) instead of evaluated. That is what the integration tests
//! assert against — a real window cannot be driven headlessly.
//!
//! Two engines can drive a GUI program. The tree interpreter runs `.matrix`
//! source and is the most feature-complete engine (`spawn`, `import` and
//! friends behave exactly as they do under `run`). The VM runs compiled
//! `.matrixc` bytecode, which is what `build --bytecode` ships so the
//! executable carries no source. Select it with `--engine vm`, or implicitly by
//! passing a `.matrixc` file or embedding one in a payload.

pub mod bridge;
pub mod window;
pub use window::PageSource;
pub use window::DEFAULT_PAGE;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use crate::ast::Program;
use crate::diagnostics::{Diagnostic, Phase};
use crate::environment::{self, Env};
use crate::interpreter::Interp;
use crate::modules;
use crate::parser;
use crate::semantic;
use crate::value::{NativeCtx, NativeFn, Value};

fn usage(msg: impl Into<String>) -> Diagnostic {
    Diagnostic::new(Phase::Usage, format!("usage error: {}", msg.into()))
}

fn gui_error(msg: impl Into<String>) -> Diagnostic {
    Diagnostic::new(Phase::Runtime, msg.into())
}

/// `spid3r gui file.matrix [--html page.html] [--dry-run]`
pub fn cmd_gui(args: &[String]) -> Result<i32, Diagnostic> {
    let mut file: Option<String> = None;
    let mut html: Option<String> = None;
    let mut payload: Option<String> = None;
    let mut dry_run = false;
    let mut persist = false;
    let mut engine_vm = false;
    // Everything after the program file: what the program sees in `args()`.
    // Flags and their values are consumed below and never leak into it — a GUI
    // program used to see `["--html", "page.html"]` as its own arguments.
    let mut script_args: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                // One source of truth: `cli::subcommand_help` owns the text and
                // `dispatch` prints it too, so a direct call to this function
                // cannot drift from `spid3r gui --help`.
                if let Some(help) = crate::cli::subcommand_help("gui") {
                    println!("{}", help);
                }
                return Ok(0);
            }
            "--engine" => {
                i += 1;
                match args.get(i).map(|s| s.as_str()) {
                    Some("vm") => engine_vm = true,
                    Some("tree") => engine_vm = false,
                    Some(other) => {
                        return Err(usage(format!(
                            "--engine expects 'tree' or 'vm', got '{}'",
                            other
                        )))
                    }
                    None => return Err(usage("--engine needs 'tree' or 'vm'")),
                }
            }
            "--html" => {
                i += 1;
                html = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| usage("--html needs a path"))?,
                );
            }
            "--dry-run" => dry_run = true,
            "--persist" => persist = true,
            "--payload" => {
                i += 1;
                payload = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| usage("--payload needs an executable path"))?,
                );
            }
            other if other.starts_with('-') => {
                return Err(usage(format!("unknown option '{}'", other)));
            }
            other => {
                if payload.is_some() || file.is_some() {
                    // Positional arguments after the program belong to the
                    // program, exactly as they do for `spid3r run`. With
                    // `--payload` there is no file slot to fill at all: the
                    // payload already carries the program, so the first
                    // positional is already a program argument.
                    script_args.push(other.to_string());
                } else {
                    file = Some(other.to_string());
                }
            }
        }
        i += 1;
    }
    if payload.is_some() && file.is_some() {
        return Err(usage(
            "--payload and a source file are mutually exclusive: the payload \
             already carries the program",
        ));
    }
    let loaded = match (&payload, &file) {
        (Some(exe), _) => load_packaged(exe, html.as_deref())?,
        (None, Some(f)) => {
            let path = Path::new(f);
            if !path.is_file() {
                return Err(usage(format!("'{}' is not a file", f)));
            }
            let page = match html.as_deref() {
                Some(h) => PageSource::File(h.to_string()),
                None => PageSource::Builtin,
            };
            let file_id: Arc<str> = Arc::from(f.as_str());
            let display = f.clone();
            if path.extension().and_then(|x| x.to_str()) == Some("matrixc") {
                let bytes = std::fs::read(path)
                    .map_err(|e| usage(format!("cannot read '{}': {}", f, e)))?;
                Loaded {
                    source: None,
                    bytecode: Some(bytes),
                    file_id,
                    display,
                    page,
                }
            } else {
                let src = std::fs::read_to_string(path)
                    .map_err(|e| usage(format!("cannot read '{}': {}", f, e)))?;
                Loaded {
                    source: Some(src),
                    bytecode: None,
                    file_id,
                    display,
                    page,
                }
            }
        }
        (None, None) => return Err(usage("gui needs a source file: spid3r gui file.matrix")),
    };

    crate::stdlib::set_script_args(script_args);

    bridge::reset();
    let dirs = modules::default_module_dirs();

    // Bytecode wins when both are present: it is what the build embedded on
    // purpose, and running it is the entire point of shipping `.matrixc`.
    let (runtime, code) = if let Some(bytes) = loaded.bytecode {
        let func = crate::vm::bytecode::deserialize_vmfunc(&bytes).map_err(|e| {
            gui_error(format!(
                "cannot load bytecode from '{}': {}",
                loaded.display, e
            ))
        })?;
        let mut vm = crate::vm::Vm::new(loaded.file_id.clone(), dirs);
        install_vm(&mut vm);
        // Phase 1: the program builds the UI. Handlers are registered and the
        // initial JavaScript is queued; nothing touches the window yet.
        let code = vm.run_compiled(Arc::new(func))?;
        (Runtime::Vm(Rc::new(RefCell::new(vm))), code)
    } else {
        let src = loaded
            .source
            .ok_or_else(|| gui_error("no program to run"))?;
        let program: Program = parser::parse_file(&src, loaded.file_id.clone())?;
        let warnings = semantic::check_file_with_warnings(&program, true, &loaded.display)?;
        for w in &warnings {
            if w.message.contains("explicit call to 'main'") {
                eprintln!("{}", w.render());
            }
        }
        if engine_vm {
            let mut vm = crate::vm::Vm::new(loaded.file_id.clone(), dirs);
            install_vm(&mut vm);
            let code = vm.run_program(&program)?;
            (Runtime::Vm(Rc::new(RefCell::new(vm))), code)
        } else {
            let interp = Arc::new(Interp::new(loaded.file_id.clone(), dirs));
            install(&interp.globals);
            let code = interp.run_program(&program)?;
            (Runtime::Tree(interp), code)
        }
    };

    // Phase 2: show it. `--dry-run` prints the queued JavaScript instead of
    // opening a window (what the test suite uses).
    if dry_run {
        window::dry_run(&runtime);
    } else {
        // A packaged app keeps its assets in memory; letting the webview cache
        // them to disk would put the UI source back on the filesystem.
        let ephemeral = payload.is_some() && !persist;
        let app_id = app_identity(payload.as_deref().or(file.as_deref()));
        window::open(runtime, loaded.page, ephemeral, &app_id).map_err(gui_error)?;
    }
    Ok(code)
}

/// Short, stable id for the app this process is running, used to key its
/// WebView2 profile.
///
/// Two packaged apps must not share a profile: under `--persist` the webview
/// keeps `localStorage`, and one origin for every app would let app A read
/// what app B stored. The identity is therefore the executable that carries
/// the program (`--payload`) or the program file itself, canonicalised so
/// `./App.exe` and `App.exe` land on the same profile. The hash keeps the
/// folder name short and free of the path's illegal-for-a-filename characters.
fn app_identity(target: Option<&str>) -> String {
    let raw = target.unwrap_or("spid3r");
    let resolved = std::fs::canonicalize(raw)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| raw.to_string());
    let hex = crate::pkg::sha256_hex(resolved.as_bytes());
    hex[..16].to_string()
}

/// What `gui` found to run: source text, bytecode, or neither, plus the page.
struct Loaded {
    source: Option<String>,
    bytecode: Option<Vec<u8>>,
    file_id: Arc<str>,
    display: String,
    page: PageSource,
}

/// Resolve the program and page for `gui --payload <exe>`.
///
/// Prefers the payload embedded by `spid3r build --embed`. Falls back to the
/// folder layout beside the executable (a `.matrix` plus `ui/index.html`), so
/// a launcher built without `--embed` still runs while it is developed.
fn load_packaged(exe: &str, html: Option<&str>) -> Result<Loaded, Diagnostic> {
    let exe_path = Path::new(exe);
    if !exe_path.is_file() {
        return Err(usage(format!("--payload '{}': no such file", exe)));
    }

    // The runtime copy in a `--bundle-runtime` payload is ~10 MB that the GUI
    // never serves; parse it but do not copy it into the page's file map.
    let entries = crate::native::payload::read_filtered(exe_path, |name| {
        name != crate::native::payload::RUNTIME_ENTRY
    })
    .map_err(gui_error)?;
    if let Some(entries) = entries {
        let mut files = std::collections::HashMap::new();
        for (name, data) in entries {
            files.insert(name, data);
        }
        // Bytecode first: a build that ships `.matrixc` ships no source at all,
        // and one that ships both should run the compiled form.
        let bytecode_name = pick(&files, "main.matrixc", ".matrixc");
        let source_name = pick(&files, "main.matrix", ".matrix");
        if bytecode_name.is_none() && source_name.is_none() {
            return Err(gui_error(format!(
                "'{}' carries an embedded payload but no .matrixc or .matrix program in it",
                exe
            )));
        }
        // Read the program before `files` moves into the page source.
        let bytecode = bytecode_name.as_ref().map(|n| files[n].clone());
        let source = match (&bytecode, &source_name) {
            // A payload that has bytecode does not need its source, and a build
            // that embeds only bytecode has none to read.
            (Some(_), _) => None,
            (None, Some(n)) => Some(
                std::str::from_utf8(&files[n])
                    .map_err(|_| gui_error(format!("embedded program '{}' is not valid UTF-8", n)))?
                    .to_string(),
            ),
            (None, None) => None,
        };
        // `--html` names an entry inside the payload. A file on disk is NOT
        // accepted here: the payload is what the app ships, and a page picked
        // up from wherever the process happens to run is not that app's UI —
        // it is a page the runtime cannot account for.
        let page = match html {
            Some(h) if files.contains_key(h) => PageSource::Memory {
                entry: h.to_string(),
                files: Arc::new(files),
            },
            Some(h) => {
                return Err(gui_error(format!(
                    "--html '{}' is not an entry in '{}'; the payload has: {}",
                    h,
                    exe,
                    html_entries(&files)
                )))
            }
            None => {
                let entry = pick(&files, "ui/index.html", ".html").ok_or_else(|| {
                    gui_error(format!(
                        "'{}' carries an embedded payload but no HTML page in it",
                        exe
                    ))
                })?;
                PageSource::Memory {
                    entry,
                    files: Arc::new(files),
                }
            }
        };
        let program_name = bytecode_name.or(source_name).unwrap_or_default();
        let display = format!("{}/{}", exe, program_name);
        let file_id: Arc<str> = Arc::from(display.as_str());
        return Ok(Loaded {
            source,
            bytecode,
            file_id,
            display,
            page,
        });
    }

    // No payload: run the loose files beside the executable. Bytecode wins for
    // the same reason as above.
    let base = exe_path.parent().unwrap_or(Path::new("."));
    let loose = |ext: &str| -> Option<std::path::PathBuf> {
        std::fs::read_dir(base)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().and_then(|x| x.to_str()) == Some(ext))
    };
    let bytecode_path = loose("matrixc");
    let matrix = loose("matrix");
    let chosen = bytecode_path
        .clone()
        .or_else(|| matrix.clone())
        .ok_or_else(|| {
            gui_error(format!(
                "'{}' has no embedded payload, and no .matrix or .matrixc program sits beside it",
                exe
            ))
        })?;
    let display = chosen.display().to_string();
    let bytecode = match &bytecode_path {
        Some(p) => Some(
            std::fs::read(p)
                .map_err(|e| gui_error(format!("cannot read '{}': {}", p.display(), e)))?,
        ),
        None => None,
    };
    let source = match (&bytecode, &matrix) {
        (Some(_), _) => None,
        (None, Some(p)) => Some(
            std::fs::read_to_string(p)
                .map_err(|e| gui_error(format!("cannot read '{}': {}", p.display(), e)))?,
        ),
        (None, None) => None,
    };
    let file_id: Arc<str> = Arc::from(display.as_str());
    let page = match html {
        Some(h) => PageSource::File(h.to_string()),
        None => {
            let page = base.join("ui").join("index.html");
            match page.is_file() {
                true => PageSource::File(page.display().to_string()),
                false => PageSource::Builtin,
            }
        }
    };
    Ok(Loaded {
        source,
        bytecode,
        file_id,
        display,
        page,
    })
}

/// Pick `preferred` if present, else the alphabetically first key ending in
/// `suffix`.
fn pick(
    files: &std::collections::HashMap<String, Vec<u8>>,
    preferred: &str,
    suffix: &str,
) -> Option<String> {
    if files.contains_key(preferred) {
        return Some(preferred.to_string());
    }
    files.keys().filter(|k| k.ends_with(suffix)).min().cloned()
}

/// The HTML entries in a payload, for the `--html` "not found" message.
fn html_entries(files: &std::collections::HashMap<String, Vec<u8>>) -> String {
    let mut names: Vec<&str> = files
        .keys()
        .map(|k| k.as_str())
        .filter(|k| k.ends_with(".html") || k.ends_with(".htm"))
        .collect();
    names.sort();
    if names.is_empty() {
        "no .html entries".to_string()
    } else {
        names.join(", ")
    }
}

// ---------------------------------------------------------------------------
// language natives
// ---------------------------------------------------------------------------

type GuiNative = fn(&NativeCtx, Vec<Value>) -> Result<Value, String>;

/// The GUI natives, installed on top of the standard library.
const GUI_NATIVES: &[(&str, GuiNative)] = &[
    ("gui_eval", n_eval),
    ("gui_set_html", n_set_html),
    ("gui_on", n_on),
    ("gui_emit", n_emit),
    ("gui_title", n_title),
];

/// Install the GUI natives into a program's global table.
pub fn install(globals: &Env) {
    for (name, func) in GUI_NATIVES {
        let native = NativeFn { name, func: *func };
        environment::define(globals, name, Value::Native(Arc::new(native)));
    }
}

/// The same table, for the bytecode VM.
///
/// A program shipped as `.matrixc` runs on the VM, which keeps its globals in a
/// `HashMap` rather than an `Env`. Without this the compiled program would find
/// every `gui_*` name undefined — and, worse, `type_of(gui_title)` would be
/// null, so a launcher-shaped `main()` would take its `run_payload()` branch
/// and try to start a second copy of the app.
pub fn install_vm(vm: &mut crate::vm::Vm) {
    vm.define_natives(GUI_NATIVES);
}

/// The engine a GUI program runs on.
///
/// The window layer needs exactly one thing from it — "call this handler with
/// (event, payload)" — so the two engines meet here and nowhere else.
///
/// The VM lives behind `Rc<RefCell<_>>` rather than `Arc<Mutex<_>>`: the window
/// event loop runs on the thread that called `open`, and the VM is neither
/// `Send` nor `Sync`, so sharing it across threads would be a lie.
#[derive(Clone)]
pub enum Runtime {
    /// Source, run by the tree-walking interpreter.
    Tree(Arc<Interp>),
    /// Serialized bytecode, run by the VM.
    Vm(Rc<RefCell<crate::vm::Vm>>),
}

fn arg_count(name: &str, args: &[Value], want: usize) -> Result<(), String> {
    if args.len() == want {
        Ok(())
    } else {
        Err(format!(
            "{} expects {} argument(s), got {}",
            name,
            want,
            args.len()
        ))
    }
}

fn want_str(name: &str, v: &Value) -> Result<String, String> {
    match v {
        Value::Str(s) => Ok(s.to_string()),
        other => Err(format!(
            "{} expects a string, found {}",
            name,
            crate::value::type_name(other)
        )),
    }
}

fn is_callable(v: &Value) -> bool {
    matches!(
        v,
        // Tree-interpreter callables.
        Value::Function(_)
            | Value::Native(_)
            | Value::NativeClosure(_)
            | Value::Class(_)
            // VM callables: a compiled program passes `VFunc`/`VClass` where a
            // tree program passes `Function`/`Class`, so both spellings have to
            // be accepted or `gui_on` rejects every handler under `--engine vm`.
            | Value::VFunc(_)
            | Value::VClass(_)
    )
}

/// `gui_eval(js)` — evaluate `js` in the page, in order.
fn n_eval(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arg_count("gui_eval", &args, 1)?;
    let js = want_str("gui_eval", &args[0])?;
    bridge::push_js(&js);
    Ok(Value::Null)
}

/// `gui_set_html(html)` — set the app root's content in the page.
fn n_set_html(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arg_count("gui_set_html", &args, 1)?;
    let html = want_str("gui_set_html", &args[0])?;
    bridge::push_js(&format!(
        "document.getElementById('spid3r-root').innerHTML = {};",
        bridge::json_quote(&html)
    ));
    Ok(Value::Null)
}

/// `gui_on(event, handler)` — call `handler(event, payload)` for every
/// `event` the page emits. Registering an event twice replaces the handler.
fn n_on(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arg_count("gui_on", &args, 2)?;
    let event = want_str("gui_on", &args[0])?;
    if !is_callable(&args[1]) {
        return Err(format!(
            "gui_on expects a function as the handler, found {}",
            crate::value::type_name(&args[1])
        ));
    }
    bridge::set_handler(&event, args[1].clone());
    Ok(Value::Null)
}

/// `gui_emit(event, payload)` — queue an event as if the page emitted it.
/// Useful for program-driven UI updates and for testing handlers.
fn n_emit(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arg_count("gui_emit", &args, 2)?;
    let event = want_str("gui_emit", &args[0])?;
    let payload = want_str("gui_emit", &args[1])?;
    bridge::push_event(&event, &payload);
    Ok(Value::Null)
}

/// `gui_title(text)` — title of the window (read when the window opens).
fn n_title(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arg_count("gui_title", &args, 1)?;
    let title = want_str("gui_title", &args[0])?;
    bridge::set_title(&title);
    Ok(Value::Null)
}
