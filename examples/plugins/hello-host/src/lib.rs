//! A sample Spid3r **host plugin**.
//!
//! It provides two functions to scripts:
//!
//! | function | permission | what it does |
//! | --- | --- | --- |
//! | `hello_greet(name)` | none (pure) | returns `"Hello, <name>!"` |
//! | `hello_env(name)` | `environment` | reads a process environment variable |
//!
//! The second one exists to make the permission model visible: a host can call
//! `Engine::deny(Capability::Environment)` and the script gets a *catchable*
//! `capability denied` error instead of the value.
//!
//! # What this is, and is not
//!
//! This is trusted code. It is compiled into the host binary, runs in the same
//! address space, and could read the environment whether or not it declared the
//! capability. The declaration is what lets a host *stop a script* from calling
//! it — it does not constrain the plugin itself. There is no sandbox here; the
//! only sandboxable plugin kind in Spid3r is a WASM plugin, and that is not
//! implemented yet.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use spid3r_lang::embed::Capability;
use spid3r_lang::plugin::{FunctionDecl, PluginFnDef, PluginHost, PluginKind, PluginManifest};
use spid3r_lang::value::{NativeCtx, Value};

/// The manifest is embedded at compile time: a plugin should not depend on a
/// file being next to it at run time, and a mismatch between the shipped
/// `plugin.toml` and the code would be a bug worth failing on.
fn manifest() -> &'static PluginManifest {
    static M: OnceLock<PluginManifest> = OnceLock::new();
    M.get_or_init(|| {
        PluginManifest::parse(include_str!("../plugin.toml"))
            .expect("the embedded plugin.toml must be valid")
    })
}

/// `hello_greet(name) -> string`. Pure: no capability, no I/O.
pub fn greet(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    match args.first() {
        Some(Value::Str(s)) => Ok(Value::Str(Arc::from(format!("Hello, {}!", s)))),
        _ => Err("hello_greet(name) expects a string".to_string()),
    }
}

/// `hello_env(name) -> string | null`. Needs the `environment` capability.
///
/// Returning `null` for a missing variable (rather than an error) keeps the
/// script's `try`/`catch` free for real failures.
pub fn env(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let key = match args.first() {
        Some(Value::Str(s)) => s.to_string(),
        _ => return Err("hello_env(name) expects a string".to_string()),
    };
    match std::env::var(&key) {
        Ok(v) => Ok(Value::Str(Arc::from(v))),
        Err(_) => Ok(Value::Null),
    }
}

/// The plugin itself.
pub struct HelloPlugin {
    started: Arc<AtomicBool>,
}

impl HelloPlugin {
    pub fn new() -> Self {
        HelloPlugin {
            started: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Default for HelloPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginHost for HelloPlugin {
    fn manifest(&self) -> &PluginManifest {
        manifest()
    }

    fn functions(&self) -> Vec<PluginFnDef> {
        vec![
            PluginFnDef {
                name: "hello_greet",
                arity: 1,
                permission: None,
                func: greet,
            },
            PluginFnDef {
                name: "hello_env",
                arity: 1,
                // Declared in plugin.toml under `capabilities`, and on the
                // `[[function]]` block. Registration fails if either is missing.
                permission: Some(Capability::Environment),
                func: env,
            },
        ]
    }

    fn start(&mut self) -> Result<(), String> {
        self.started.store(true, Ordering::SeqCst);
        eprintln!("[hello] started");
        Ok(())
    }

    fn stop(&mut self) -> Result<(), String> {
        // Must tolerate being called twice; `shutdown_all` guards on state, but
        // a host may call `stop` directly as well.
        if self.started.swap(false, Ordering::SeqCst) {
            eprintln!("[hello] stopped");
        }
        Ok(())
    }
}

/// The manifest this plugin declares, for a host that wants to inspect it
/// before registering.
pub fn declared_functions() -> Vec<FunctionDecl> {
    manifest().functions.clone()
}

/// The kind, so a host can assert it never loaded a WASM plugin through the
/// host-plugin path.
pub fn kind() -> PluginKind {
    manifest().kind
}
