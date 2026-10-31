//! Spid3r-source plugins: write a plugin **in Spid3r**.
//!
//! This is the plugin kind that makes the ecosystem belong to the language. A
//! `kind = "matrix"` plugin is a `.matrix` file whose `public fn`s become
//! script-callable functions — no Rust, no WebAssembly, no build step beyond
//! the file existing.
//!
//! ```toml
//! [plugin]
//! name = "greetings"
//! kind = "matrix"
//! entry = "greetings.matrix"
//!
//! [[function]]
//! name = "greet"
//! arity = 1
//! ```
//!
//! ```spid3r
//! public fn greet(name) {
//!     return "Hello, " + name + "!";
//! }
//! ```
//!
//! # Trust
//!
//! A matrix plugin is **script code**, so it has exactly the authority a script
//! has: the same builtins, the same filesystem access, the same everything. It
//! is tier T1 in `docs/SECURITY.md` — the same trust as the project's own
//! source. That is the honest consequence of "your own language": code written
//! in it is not sandboxed by virtue of being a plugin.
//!
//! Use `kind = "wasm"` for anything you did not write. Use `kind = "host"` when
//! the plugin needs to be Rust.
//!
//! # Isolation
//!
//! Each plugin gets its own interpreter and its own globals, so a plugin cannot
//! read or clobber the calling program's variables. It can still call the
//! standard library — that is the point.

use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::diagnostics::Span;
use crate::interpreter::{Flow, Interp};
use crate::value::{NativeClosureFn, Value};

use super::manifest::{PluginKind, PluginManifest, ABI_V1};

/// A loaded matrix plugin: its manifest and one closure per declared function.
pub struct LoadedMatrix {
    pub manifest: PluginManifest,
    pub closures: Vec<(String, Arc<NativeClosureFn>)>,
    /// Kept alive for the process; the closures hold clones of the same handle.
    _interp: Arc<Mutex<Interp>>,
}

impl std::fmt::Debug for LoadedMatrix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedMatrix")
            .field("name", &self.manifest.name)
            .field("functions", &self.closures.len())
            .finish_non_exhaustive()
    }
}

/// Load `plugin.toml` + its `.matrix` entry from `dir`.
pub fn load(dir: &Path) -> Result<LoadedMatrix, String> {
    let manifest = PluginManifest::load(dir)?;
    if manifest.kind != PluginKind::Matrix {
        return Err(format!(
            "plugin '{}' is kind = \"{}\"; the Spid3r-source loader only accepts \"matrix\"",
            manifest.name,
            manifest.kind.as_str()
        ));
    }
    if !manifest.abi_matches() {
        return Err(format!(
            "plugin '{}' targets ABI '{}' but this build implements '{}'",
            manifest.name, manifest.abi, ABI_V1
        ));
    }

    // `entry` defaults to `<name>.matrix`, so the common case needs no key.
    let entry = if manifest.entry.is_empty() {
        format!("{}.matrix", manifest.name)
    } else {
        manifest.entry.clone()
    };
    let path = dir.join(&entry);
    let src = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;

    let file: Arc<str> = Arc::from(path.to_string_lossy().to_string());
    let program = crate::parser::parse_file(&src, file.clone())
        .map_err(|d| format!("plugin '{}': {}", manifest.name, d.message))?;
    // `is_main = false`: a plugin is a module, so it needs no `fn main`.
    crate::semantic::check_file(&program, false, &file)
        .map_err(|d| format!("plugin '{}': {}", manifest.name, d.message))?;
    let lowered = crate::lower::lower(&program);

    // The plugin's own directory is first on its search path, so it can
    // `import` a sibling file without knowing where it was installed.
    let mut dirs: Vec<std::path::PathBuf> = vec![dir.to_path_buf()];
    dirs.extend(crate::modules::default_module_dirs());
    // `Interp` is not `Clone`, and a closure needs shared ownership: one
    // interpreter per plugin, behind a mutex. Calls from a script are already
    // serialized by the evaluator, so this is a cheap guard, not a bottleneck.
    let interp = Arc::new(Mutex::new(Interp::new(file.clone(), dirs)));

    // Run the top level: this binds the `fn`s. Nothing else executes unless the
    // plugin author wrote top-level statements, which are their choice.
    interp
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .run_module(&lowered)
        .map_err(|d| {
            format!(
                "plugin '{}' failed while loading: {}",
                manifest.name, d.message
            )
        })?;

    let mut closures: Vec<(String, Arc<NativeClosureFn>)> = Vec::new();
    for decl in &manifest.functions {
        let guard = interp.lock().unwrap_or_else(|e| e.into_inner());
        let value = crate::environment::get(guard.globals(), &decl.name).ok_or_else(|| {
            format!(
                "plugin '{}': manifest declares '{}' but the file does not define it",
                manifest.name, decl.name
            )
        })?;
        let Value::Function(fd) = value else {
            return Err(format!(
                "plugin '{}': '{}' is not a function (it is a {})",
                manifest.name,
                decl.name,
                crate::value::type_name(&value)
            ));
        };
        if let Some(expected) = arity_of(&fd) {
            if expected != decl.arity {
                return Err(format!(
                    "plugin '{}' function '{}' takes {} parameter(s) but plugin.toml declares {}",
                    manifest.name, decl.name, expected, decl.arity
                ));
            }
        }
        closures.push((decl.name.clone(), make_closure(interp.clone(), fd)));
    }

    Ok(LoadedMatrix {
        manifest,
        closures,
        _interp: interp,
    })
}

/// Wrap a Spid3r function as a host-callable closure.
fn make_closure(
    interp: Arc<Mutex<Interp>>,
    fd: Arc<crate::value::FunctionData>,
) -> Arc<NativeClosureFn> {
    Arc::new(move |_ctx, args: Vec<Value>| {
        let span = Span::new(Arc::from("<plugin>"), 0, 0, 0);
        let guard = interp.lock().unwrap_or_else(|e| e.into_inner());
        match guard.call_function(&fd, args, &span) {
            Ok(v) => Ok(v),
            Err(flow) => Err(flow_message(flow)),
        }
    })
}

/// Turn a control-flow escape into the string a script's `try`/`catch` sees.
fn flow_message(flow: Flow) -> String {
    match flow {
        // A `throw "msg"` carries the value in `thrown`; the message alone
        // would reach the caller as the useless "uncaught throw".
        Flow::Error(d) | Flow::Fatal(d) => match d.thrown {
            Some(Value::Str(s)) => s.to_string(),
            Some(v) => format!("{} (thrown {})", d.message, crate::value::type_name(&v)),
            None => d.message,
        },
        Flow::Value(v) | Flow::Return(v) => {
            format!(
                "plugin function returned early: {}",
                crate::value::type_name(&v)
            )
        }
        Flow::Break => "break outside a loop in a plugin function".to_string(),
        Flow::Continue => "continue outside a loop in a plugin function".to_string(),
    }
}

/// Declared parameter count, when the prototype records it.
fn arity_of(fd: &crate::value::FunctionData) -> Option<usize> {
    Some(fd.proto.params.len())
}
