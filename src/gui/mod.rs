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
//! The GUI engine always uses the tree interpreter (the most feature-complete
//! engine); `spawn`, `import` and friends behave exactly as they do under
//! `run`.

pub mod bridge;
pub mod window;

use std::path::Path;
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
    let mut dry_run = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--html" => {
                i += 1;
                html = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| usage("--html needs a path"))?,
                );
            }
            "--dry-run" => dry_run = true,
            other if other.starts_with('-') => {
                return Err(usage(format!("unknown option '{}'", other)));
            }
            other => {
                if file.is_some() {
                    return Err(usage("gui takes exactly one source file"));
                }
                file = Some(other.to_string());
            }
        }
        i += 1;
    }
    let file = file.ok_or_else(|| usage("gui needs a source file: spid3r gui file.matrix"))?;
    let path = Path::new(&file);
    if !path.is_file() {
        return Err(usage(format!("'{}' is not a file", file)));
    }
    let src = std::fs::read_to_string(path)
        .map_err(|e| usage(format!("cannot read '{}': {}", file, e)))?;
    let file_id: Arc<str> = Arc::from(file.as_str());
    let program: Program = parser::parse_file(&src, file_id.clone())?;
    semantic::check_file(&program, true, &file)?;

    bridge::reset();
    let dirs = modules::default_module_dirs();
    let interp = Arc::new(Interp::new(file_id, dirs));
    install(&interp.globals);

    // Phase 1: the program builds the UI. Handlers are registered and the
    // initial JavaScript is queued; nothing touches the window yet.
    let code = interp.run_program(&program)?;

    // Phase 2: show it. `--dry-run` prints the queued JavaScript instead of
    // opening a window (what the test suite uses).
    if dry_run {
        window::dry_run(&interp);
    } else {
        window::open(&interp, html.as_deref()).map_err(gui_error)?;
    }
    Ok(code)
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
        Value::Function(_) | Value::Native(_) | Value::Class(_)
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
