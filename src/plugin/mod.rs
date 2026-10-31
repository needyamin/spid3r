//! Plugin API v1: host plugins, lifecycle, and the single registration point.
//!
//! # Why this is not a dynamic ABI
//!
//! A `NativeDef` is `(&'static str, fn(&NativeCtx, Vec<Value>) -> Result<…>)`.
//! `Value` is a reference-counted enum with no stable representation, so it
//! cannot cross a C ABI boundary. Loading arbitrary native code at run time
//! would therefore need either a serialisation hop or a handle table, and
//! `dlopen` on top of that. v1 does neither: a **host plugin is compiled into
//! its host**, exactly like a Cargo build script's build-dependency.
//!
//! That is a real limitation, stated plainly: the `spidr` binary itself
//! registers no plugins, so `spidr run` cannot call a plugin function. A host
//! that embeds `spid3r_lang` calls [`register`] at startup and its scripts can.
//!
//! # Trust
//!
//! A host plugin is **trusted code**. It runs in the same address space with
//! the same authority as the toolchain. A declared `permission` is enforced by
//! [`crate::embed::Engine::deny`], which is a policy filter over names — it
//! stops a *script* from calling the function, and it cannot revoke authority
//! the plugin already has. Never describe this as a sandbox.
//!
//! # Lifecycle
//!
//! `Loaded → Started → Stopped`, with `Failed` for a plugin whose `start`
//! returned an error. `start` runs before the first call (registration happens
//! at host startup), `stop` runs at [`shutdown_all`]. Stopping twice is safe,
//! and a plugin that fails to start is never registered, so it cannot poison
//! the registry for the next one.

pub mod abi;
pub mod manifest;
pub mod matrix;
#[cfg(feature = "plugins-wasm")]
pub mod wasm;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::embed::Capability;
use crate::value::{NativeClosureFn, NativeCtx, Value};

pub use manifest::{capability_name, FunctionDecl, PluginKind, PluginManifest, ABI_V1};

/// Where a plugin is in its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginState {
    /// Registered, `start` not yet attempted.
    Loaded,
    /// `start` succeeded; its functions are visible to scripts.
    Started,
    /// `stop` ran. Functions are no longer offered.
    Stopped,
    /// `start` failed. Never offered, never fatal to the host.
    Failed(String),
}

impl PluginState {
    pub fn as_str(&self) -> &'static str {
        match self {
            PluginState::Loaded => "loaded",
            PluginState::Started => "started",
            PluginState::Stopped => "stopped",
            PluginState::Failed(_) => "failed",
        }
    }
}

/// One function a plugin exposes to scripts.
///
/// `name` is `&'static str` because that is what [`crate::stdlib::NativeDef`]
/// requires; a plugin's function names are compile-time constants, which is
/// also what keeps them out of the marshalling problem entirely.
#[derive(Clone, Copy)]
pub struct PluginFnDef {
    pub name: &'static str,
    /// Declared arity, checked against the manifest at registration.
    pub arity: usize,
    /// The capability a script must be allowed to call this function.
    /// `None` means pure computation.
    pub permission: Option<Capability>,
    pub func: fn(&NativeCtx, Vec<Value>) -> Result<Value, String>,
}

impl std::fmt::Debug for PluginFnDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginFnDef")
            .field("name", &self.name)
            .field("arity", &self.arity)
            .field("permission", &self.permission)
            .finish()
    }
}

/// What a host implements to provide functions to scripts.
pub trait PluginHost: Send {
    /// The parsed manifest. Its `abi` must equal [`ABI_V1`].
    fn manifest(&self) -> &PluginManifest;

    /// The functions this plugin offers. Called once, at registration.
    fn functions(&self) -> Vec<PluginFnDef>;

    /// Run before the first call. Default: nothing to do.
    fn start(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// Run at [`shutdown_all`]. Must tolerate being called twice.
    fn stop(&mut self) -> Result<(), String> {
        Ok(())
    }
}

struct Entry {
    /// `None` for a WASM plugin, which has no Rust lifecycle object.
    host: Option<Box<dyn PluginHost>>,
    manifest: PluginManifest,
    /// Natives exposed as bare `fn` pointers (host plugins).
    functions: Vec<PluginFnDef>,
    /// Natives exposed as closures (WASM plugins). A guest instance is captured
    /// state, so a `fn` pointer cannot represent it.
    closures: Vec<(String, Arc<NativeClosureFn>)>,
    state: PluginState,
}

static REGISTRY: OnceLock<Mutex<Vec<Entry>>> = OnceLock::new();

fn registry() -> &'static Mutex<Vec<Entry>> {
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

fn lock() -> std::sync::MutexGuard<'static, Vec<Entry>> {
    // A panicking plugin `start` must not poison the registry for everyone
    // else in the process.
    registry().lock().unwrap_or_else(|e| e.into_inner())
}

/// Register a plugin. Called by a host at startup, before it creates an engine.
///
/// Rejected before any plugin code runs:
/// * an `abi` that is not exactly [`ABI_V1`];
/// * a function whose `permission` the manifest does not declare;
/// * a duplicate plugin name, a duplicate function name, or a function that
///   would shadow a builtin.
///
/// A `start` failure is **not** a registration failure for the host: the plugin
/// is recorded as [`PluginState::Failed`] and its functions are not offered, so
/// one broken plugin cannot take the process down.
pub fn register(host: Box<dyn PluginHost>) -> Result<(), String> {
    let manifest = host.manifest().clone();

    if !manifest.abi_matches() {
        return Err(format!(
            "plugin '{}' targets ABI '{}' but this build implements '{}'",
            manifest.name,
            if manifest.abi.is_empty() {
                "(none)"
            } else {
                &manifest.abi
            },
            ABI_V1
        ));
    }

    // Neither a WASM nor a Spid3r-source plugin travels the host path: each has
    // its own loader, and the error says which one.
    if manifest.kind == PluginKind::Matrix {
        return Err(format!(
            "plugin '{}' is a Spid3r-source plugin; load it with plugin::register_matrix() or plugin::load_all()",
            manifest.name
        ));
    }
    if manifest.kind == PluginKind::Wasm {
        return Err(if cfg!(feature = "plugins-wasm") {
            format!(
                "plugin '{}' is a WASM plugin; load it with plugin::register_wasm() or plugin::load_all()",
                manifest.name
            )
        } else {
            format!(
                "plugin '{}' is a WASM plugin, but this build was compiled without the `plugins-wasm` feature",
                manifest.name
            )
        });
    }

    let functions = host.functions();

    // A declared function may only use a capability the manifest announced.
    for f in &functions {
        if let Some(cap) = f.permission {
            if !manifest.capabilities.contains(&cap) {
                return Err(format!(
                    "plugin '{}' function '{}' needs capability '{}', which plugin.toml does not declare",
                    manifest.name,
                    f.name,
                    capability_name(cap)
                ));
            }
        }
    }

    // The manifest is the contract: a function offered at run time that the
    // manifest never declared is a mismatch, not an extension.
    for f in &functions {
        match manifest.functions.iter().find(|d| d.name == f.name) {
            None => {
                return Err(format!(
                    "plugin '{}' offers '{}', which plugin.toml does not declare",
                    manifest.name, f.name
                ))
            }
            Some(decl) if decl.arity != f.arity => {
                return Err(format!(
                    "plugin '{}' function '{}' has arity {} but plugin.toml declares {}",
                    manifest.name, f.name, f.arity, decl.arity
                ))
            }
            Some(_) => {}
        }
    }

    {
        let reg = lock();
        if reg.iter().any(|e| e.manifest.name == manifest.name) {
            return Err(format!("plugin '{}' is already registered", manifest.name));
        }
        let builtins: Vec<&'static str> = crate::stdlib::builtin_defs()
            .iter()
            .map(|(n, _)| *n)
            .collect();
        for f in &functions {
            if builtins.contains(&f.name) {
                return Err(format!(
                    "plugin '{}' function '{}' would shadow a builtin",
                    manifest.name, f.name
                ));
            }
            if reg
                .iter()
                .filter(|e| e.state == PluginState::Started)
                .flat_map(|e| e.functions.iter())
                .any(|g| g.name == f.name)
                || reg
                    .iter()
                    .filter(|e| e.state == PluginState::Started)
                    .flat_map(|e| e.closures.iter())
                    .any(|(n, _)| n == f.name)
            {
                return Err(format!(
                    "plugin '{}' function '{}' is already provided by another plugin",
                    manifest.name, f.name
                ));
            }
        }
    }

    let idx = {
        let mut reg = lock();
        reg.push(Entry {
            host: Some(host),
            manifest,
            functions,
            closures: Vec::new(),
            state: PluginState::Loaded,
        });
        reg.len() - 1
    };

    // `start` runs with the lock released: a plugin is allowed to do real work
    // here (open a socket pool, read its config) and must not block the
    // registry while it does.
    let started = {
        let mut reg = lock();
        match reg[idx].host.as_mut() {
            Some(host) => host.start(),
            None => Ok(()),
        }
    };
    let mut reg = lock();
    match started {
        Ok(()) => {
            reg[idx].state = PluginState::Started;
            Ok(())
        }
        Err(e) => {
            // Recorded, not propagated: the host keeps running, this plugin's
            // functions are simply never offered.
            reg[idx].state = PluginState::Failed(e.clone());
            Err(format!(
                "plugin '{}' failed to start: {}",
                reg[idx].manifest.name, e
            ))
        }
    }
}

/// Every function offered by a started plugin, as native definitions.
///
/// This is the single merge point: [`crate::stdlib::native_defs`] appends the
/// result, and all three engines install from that list, so a plugin function
/// needs no per-engine code.
pub fn registered() -> Vec<crate::stdlib::NativeDef> {
    let reg = lock();
    reg.iter()
        .filter(|e| e.state == PluginState::Started)
        .flat_map(|e| e.functions.iter())
        .map(|f| (f.name, f.func))
        .collect()
}

/// Names of every function a started plugin offers — used by
/// [`crate::embed::Engine::deny`] to stub them by capability.
pub fn registered_functions() -> Vec<PluginFnDef> {
    let reg = lock();
    reg.iter()
        .filter(|e| e.state == PluginState::Started)
        .flat_map(|e| e.functions.iter().copied())
        .collect()
}

/// Every function offered by a started **WASM** plugin, as closures.
///
/// A guest instance is captured state, so these cannot be `NativeDef`s (which
/// are bare `fn` pointers). The tree interpreter and the VM install them as
/// `Value::NativeClosure`, which both already know how to call.
pub fn registered_closures() -> Vec<(String, Arc<NativeClosureFn>)> {
    let reg = lock();
    reg.iter()
        .filter(|e| e.state == PluginState::Started)
        .flat_map(|e| e.closures.iter().cloned())
        .collect()
}

/// Load and register the WASM plugin in `dir` (a directory holding
/// `plugin.toml`).
///
/// Instantiation is the plugin's "start": a module that fails to compile or
/// instantiate never reaches the registry, so a broken plugin cannot shadow a
/// working one.
#[cfg(feature = "plugins-wasm")]
pub fn register_wasm(dir: &Path) -> Result<(), String> {
    let loaded = wasm::load(dir)?;
    let manifest = loaded.manifest.clone();

    {
        let reg = lock();
        if reg.iter().any(|e| e.manifest.name == manifest.name) {
            return Err(format!("plugin '{}' is already registered", manifest.name));
        }
        let builtins: Vec<&'static str> = crate::stdlib::builtin_defs()
            .iter()
            .map(|(n, _)| *n)
            .collect();
        for decl in &manifest.functions {
            if builtins.contains(&decl.name.as_str()) {
                return Err(format!(
                    "plugin '{}' function '{}' would shadow a builtin",
                    manifest.name, decl.name
                ));
            }
            let taken = reg
                .iter()
                .filter(|e| e.state == PluginState::Started)
                .any(|e| {
                    e.functions.iter().any(|f| f.name == decl.name)
                        || e.closures.iter().any(|(n, _)| *n == decl.name)
                });
            if taken {
                return Err(format!(
                    "plugin '{}' function '{}' is already provided by another plugin",
                    manifest.name, decl.name
                ));
            }
        }
    }

    let mut reg = lock();
    reg.push(Entry {
        host: None,
        manifest,
        functions: Vec::new(),
        closures: loaded.closures,
        // Instantiation already succeeded, which is what "started" means here.
        state: PluginState::Started,
    });
    Ok(())
}

/// Load and register the Spid3r-source plugin in `dir`.
///
/// This is the loader that makes the ecosystem belong to the language: the
/// plugin is a `.matrix` file, and its `public fn`s become callable from any
/// script. It runs with script authority (tier T1), so it is trusted code —
/// `kind = "wasm"` is the sandboxed alternative.
pub fn register_matrix(dir: &Path) -> Result<(), String> {
    let loaded = matrix::load(dir)?;
    let manifest = loaded.manifest.clone();

    {
        let reg = lock();
        if reg.iter().any(|e| e.manifest.name == manifest.name) {
            return Err(format!("plugin '{}' is already registered", manifest.name));
        }
        let builtins: Vec<&'static str> = crate::stdlib::builtin_defs()
            .iter()
            .map(|(n, _)| *n)
            .collect();
        for decl in &manifest.functions {
            if builtins.contains(&decl.name.as_str()) {
                return Err(format!(
                    "plugin '{}' function '{}' would shadow a builtin",
                    manifest.name, decl.name
                ));
            }
            let taken = reg
                .iter()
                .filter(|e| e.state == PluginState::Started)
                .any(|e| {
                    e.functions.iter().any(|f| f.name == decl.name)
                        || e.closures.iter().any(|(n, _)| *n == decl.name)
                });
            if taken {
                return Err(format!(
                    "plugin '{}' function '{}' is already provided by another plugin",
                    manifest.name, decl.name
                ));
            }
        }
    }

    let mut reg = lock();
    reg.push(Entry {
        host: None,
        manifest,
        functions: Vec::new(),
        closures: loaded.closures,
        // The file parsed, checked and ran: that is this kind's "start".
        state: PluginState::Started,
    });
    Ok(())
}

/// Where plugins are looked for, in order.
///
/// `SPID3R_PLUGINS` (a `;`-separated list) first, so a project or CI job can
/// pin an exact set; then `./plugins`, the directory beside the executable, and
/// finally the per-user directory. A plugin is identified by the directory that
/// holds its `plugin.toml`.
pub fn default_plugin_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(list) = std::env::var("SPID3R_PLUGINS") {
        for part in list.split(';') {
            let part = part.trim();
            if !part.is_empty() {
                dirs.push(PathBuf::from(part));
            }
        }
    }
    dirs.push(PathBuf::from("plugins"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.join("plugins"));
        }
    }
    if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        dirs.push(PathBuf::from(home).join(".spidr").join("plugins"));
    }
    dirs
}

/// Load every plugin found on the default search path.
///
/// Returns the errors rather than failing: one unloadable plugin must not stop
/// the toolchain from running a program. A caller that wants to be strict can
/// check the returned list.
pub fn load_all() -> Vec<String> {
    load_all_in(&default_plugin_dirs())
}

/// [`load_all`] against an explicit list of plugin directories.
pub fn load_all_in(dirs: &[PathBuf]) -> Vec<String> {
    let mut errors = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue; // a missing search root is normal
        };
        let mut found: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.join("plugin.toml").is_file())
            .collect();
        // Deterministic load order, so a name clash reports the same plugin.
        found.sort();
        for plugin_dir in found {
            let Ok(text) = std::fs::read_to_string(plugin_dir.join("plugin.toml")) else {
                continue;
            };
            let Ok(m) = PluginManifest::parse(&text) else {
                errors.push(format!(
                    "{}: plugin.toml does not parse",
                    plugin_dir.display()
                ));
                continue;
            };
            let result = match m.kind {
                PluginKind::Wasm => register_wasm_if_enabled(&plugin_dir, &m.name),
                PluginKind::Matrix => register_matrix(&plugin_dir),
                // A host plugin cannot be loaded from disk: it is Rust code
                // compiled into its host. Say so instead of failing obscurely.
                PluginKind::Host => Err(format!(
                    "plugin '{}': kind = \"host\" must be compiled into its host and registered with plugin::register(); it cannot be loaded from disk",
                    m.name
                )),
            };
            if let Err(e) = result {
                errors.push(format!("{}: {}", plugin_dir.display(), e));
            }
        }
    }
    errors
}

/// Function names declared by plugin manifests **on the search path**, without
/// loading or registering anything.
///
/// `build` and `dist` deliberately do not register plugins — a standalone
/// native executable cannot carry one — so this is how they can still explain an
/// unknown name that a plugin would have provided.
pub fn declared_on_disk() -> Vec<String> {
    let mut names = Vec::new();
    for dir in default_plugin_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let manifest_path = entry.path().join("plugin.toml");
            if !manifest_path.is_file() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&manifest_path) else {
                continue;
            };
            if let Ok(m) = PluginManifest::parse(&text) {
                names.extend(m.functions.into_iter().map(|f| f.name));
            }
        }
    }
    names
}

/// Load a WASM plugin when the feature is compiled in; explain when it is not.
#[cfg(feature = "plugins-wasm")]
fn register_wasm_if_enabled(dir: &Path, _name: &str) -> Result<(), String> {
    register_wasm(dir)
}

#[cfg(not(feature = "plugins-wasm"))]
fn register_wasm_if_enabled(_dir: &Path, name: &str) -> Result<(), String> {
    Err(format!(
        "plugin '{}' is a WASM plugin, but this build was compiled without the `plugins-wasm` feature",
        name
    ))
}

/// A snapshot of one registered plugin, for `plugin list`-style output.
#[derive(Debug, Clone)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub kind: PluginKind,
    pub state: PluginState,
    pub functions: Vec<String>,
    pub capabilities: Vec<&'static str>,
}

/// Every registered plugin, in registration order.
pub fn list() -> Vec<PluginInfo> {
    let reg = lock();
    reg.iter()
        .map(|e| PluginInfo {
            name: e.manifest.name.clone(),
            version: e.manifest.version.clone(),
            kind: e.manifest.kind,
            state: e.state.clone(),
            functions: e
                .functions
                .iter()
                .map(|f| f.name.to_string())
                .chain(e.closures.iter().map(|(n, _)| n.clone()))
                .collect(),
            capabilities: e
                .manifest
                .capabilities
                .iter()
                .copied()
                .map(capability_name)
                .collect(),
        })
        .collect()
}

/// Stop every started plugin. Safe to call twice: a stopped plugin is skipped.
///
/// Returns the errors rather than failing fast — one plugin's `stop` throwing
/// must not leave the others running.
pub fn shutdown_all() -> Vec<String> {
    let mut errors = Vec::new();
    let mut reg = lock();
    for entry in reg.iter_mut() {
        if entry.state == PluginState::Started {
            let stopped = match entry.host.as_mut() {
                Some(host) => host.stop(),
                None => Ok(()),
            };
            match stopped {
                Ok(()) => entry.state = PluginState::Stopped,
                Err(e) => {
                    errors.push(format!("plugin '{}': {}", entry.manifest.name, e));
                    entry.state = PluginState::Stopped;
                }
            }
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// The registry is process-global and `shutdown_all` is process-wide, so
    /// these tests must not run concurrently with each other.
    fn gate() -> MutexGuard<'static, ()> {
        static GATE: OnceLock<Mutex<()>> = OnceLock::new();
        GATE.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    static START_CALLS: AtomicUsize = AtomicUsize::new(0);

    /// A probe plugin. Each instance gets its **own** function name: two
    /// started plugins offering the same name is a registration error by
    /// design, so sharing one name across tests would be testing the wrong
    /// thing.
    struct Probe {
        manifest: PluginManifest,
        fname: &'static str,
        fail_start: bool,
    }

    impl PluginHost for Probe {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }
        fn functions(&self) -> Vec<PluginFnDef> {
            vec![PluginFnDef {
                name: self.fname,
                arity: 0,
                permission: None,
                func: |_ctx, _args| Ok(Value::Int(1)),
            }]
        }
        fn start(&mut self) -> Result<(), String> {
            START_CALLS.fetch_add(1, Ordering::SeqCst);
            if self.fail_start {
                return Err("boom".to_string());
            }
            Ok(())
        }
    }

    fn manifest(name: &str, abi: &str, fname: &str) -> PluginManifest {
        PluginManifest {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            abi: abi.to_string(),
            kind: PluginKind::Host,
            capabilities: Vec::new(),
            entry: String::new(),
            functions: vec![FunctionDecl {
                name: fname.to_string(),
                arity: 0,
                permission: None,
            }],
        }
    }

    #[test]
    fn an_unknown_abi_is_rejected_before_start_runs() {
        let _g = gate();
        let before = START_CALLS.load(Ordering::SeqCst);
        let err = register(Box::new(Probe {
            manifest: manifest("abi-test", "spid3r-plugin-abi-99", "abi_ping"),
            fname: "abi_ping",
            fail_start: false,
        }))
        .unwrap_err();
        assert!(err.contains("ABI"), "{err}");
        assert_eq!(
            START_CALLS.load(Ordering::SeqCst),
            before,
            "start must not run for a rejected ABI"
        );
        assert!(!list().iter().any(|p| p.name == "abi-test"));
    }

    #[test]
    fn a_failing_start_is_recorded_and_offers_no_functions() {
        let _g = gate();
        let err = register(Box::new(Probe {
            manifest: manifest("fails-to-start", ABI_V1, "fails_ping"),
            fname: "fails_ping",
            fail_start: true,
        }))
        .unwrap_err();
        assert!(err.contains("failed to start"), "{err}");
        let info = list()
            .into_iter()
            .find(|p| p.name == "fails-to-start")
            .expect("recorded");
        assert!(matches!(info.state, PluginState::Failed(_)));
        assert!(
            !registered().iter().any(|(n, _)| *n == "fails_ping"),
            "a failed plugin must not offer functions"
        );
    }

    #[test]
    fn a_started_plugin_offers_its_functions_and_stops_safely_twice() {
        let _g = gate();
        register(Box::new(Probe {
            manifest: manifest("good", ABI_V1, "good_ping"),
            fname: "good_ping",
            fail_start: false,
        }))
        .expect("register");
        assert!(registered().iter().any(|(n, _)| *n == "good_ping"));

        shutdown_all();
        assert!(
            !registered().iter().any(|(n, _)| *n == "good_ping"),
            "a stopped plugin must not offer functions"
        );
        // Idempotent: nothing started, nothing to do, no panic.
        assert!(shutdown_all().is_empty());
    }

    #[test]
    fn a_duplicate_plugin_name_is_rejected() {
        let _g = gate();
        register(Box::new(Probe {
            manifest: manifest("dup", ABI_V1, "dup_ping"),
            fname: "dup_ping",
            fail_start: false,
        }))
        .expect("first");
        let err = register(Box::new(Probe {
            manifest: manifest("dup", ABI_V1, "dup_ping_2"),
            fname: "dup_ping_2",
            fail_start: false,
        }))
        .unwrap_err();
        assert!(err.contains("already registered"), "{err}");
    }

    /// Two different plugins may not offer the same function name: the second
    /// would silently shadow the first.
    #[test]
    fn two_plugins_cannot_offer_the_same_function() {
        let _g = gate();
        register(Box::new(Probe {
            manifest: manifest("first-provider", ABI_V1, "shared_fn"),
            fname: "shared_fn",
            fail_start: false,
        }))
        .expect("first");
        let err = register(Box::new(Probe {
            manifest: manifest("second-provider", ABI_V1, "shared_fn"),
            fname: "shared_fn",
            fail_start: false,
        }))
        .unwrap_err();
        assert!(err.contains("already provided"), "{err}");
    }

    #[test]
    fn a_function_the_manifest_never_declared_is_rejected() {
        let _g = gate();
        let mut m = manifest("undeclared", ABI_V1, "declared_fn");
        m.functions.clear();
        let err = register(Box::new(Probe {
            manifest: m,
            fname: "declared_fn",
            fail_start: false,
        }))
        .unwrap_err();
        assert!(err.contains("does not declare"), "{err}");
    }

    #[test]
    fn a_permission_the_manifest_never_announced_is_rejected() {
        let _g = gate();
        struct Greedy {
            manifest: PluginManifest,
        }
        impl PluginHost for Greedy {
            fn manifest(&self) -> &PluginManifest {
                &self.manifest
            }
            fn functions(&self) -> Vec<PluginFnDef> {
                vec![PluginFnDef {
                    name: "greedy_read",
                    arity: 0,
                    permission: Some(Capability::FileSystem),
                    func: |_c, _a| Ok(Value::Null),
                }]
            }
        }
        let mut m = manifest("greedy", ABI_V1, "greedy_read");
        m.functions = vec![FunctionDecl {
            name: "greedy_read".to_string(),
            arity: 0,
            permission: Some(Capability::FileSystem),
        }];
        let err = register(Box::new(Greedy { manifest: m })).unwrap_err();
        assert!(err.contains("does not declare"), "{err}");
    }

    #[test]
    fn an_arity_mismatch_is_rejected() {
        let _g = gate();
        let mut m = manifest("arity", ABI_V1, "arity_fn");
        m.functions[0].arity = 3;
        let err = register(Box::new(Probe {
            manifest: m,
            fname: "arity_fn",
            fail_start: false,
        }))
        .unwrap_err();
        assert!(err.contains("arity"), "{err}");
    }
}
