//! Plugin API v1: registration, the manifest contract, the permission gate, and
//! the fact that a plugin function is reachable from a script.
//!
//! The registry is **process-global**, so these tests share one plugin table.
//! Each test registers a uniquely named plugin and asserts only about its own
//! functions; ordering between tests must not matter.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use spid3r_lang::embed::{Capability, Engine};
use spid3r_lang::plugin::{
    list, register, registered, registered_functions, shutdown_all, FunctionDecl, PluginFnDef,
    PluginHost, PluginKind, PluginManifest, ABI_V1,
};
use spid3r_lang::value::{NativeCtx, Value};

/// The plugin registry is process-global, so `shutdown_all()` in one test would
/// stop plugins registered by another running in parallel. Serialize the whole
/// file: correctness here depends on exclusive use of one shared table.
fn gate() -> MutexGuard<'static, ()> {
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// A plugin with one pure function and one that declares `environment`.
struct Sample {
    manifest: PluginManifest,
    stops: AtomicUsize,
}

impl Sample {
    fn new(name: &'static str) -> Self {
        let (echo, env) = (format!("{name}_echo"), format!("{name}_env"));
        Sample {
            manifest: PluginManifest {
                name: name.to_string(),
                version: "0.1.0".to_string(),
                abi: ABI_V1.to_string(),
                kind: PluginKind::Host,
                capabilities: vec![Capability::Environment],
                entry: String::new(),
                functions: vec![
                    FunctionDecl {
                        name: echo,
                        arity: 1,
                        permission: None,
                    },
                    FunctionDecl {
                        name: env,
                        arity: 1,
                        permission: Some(Capability::Environment),
                    },
                ],
            },
            stops: AtomicUsize::new(0),
        }
    }
}

fn echo(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    match args.first() {
        Some(Value::Str(s)) => Ok(Value::Str(s.clone())),
        _ => Err("echo(x) expects a string".to_string()),
    }
}

fn env(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    match args.first() {
        Some(Value::Str(_)) => Ok(Value::Str(std::sync::Arc::from("secret"))),
        _ => Err("env(x) expects a string".to_string()),
    }
}

impl PluginHost for Sample {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }
    fn functions(&self) -> Vec<PluginFnDef> {
        // The names are leaked once so they can be `&'static str`, which is what
        // `NativeDef` requires. A real plugin has them as literals.
        let echo_name: &'static str =
            Box::leak(self.manifest.functions[0].name.clone().into_boxed_str());
        let env_name: &'static str =
            Box::leak(self.manifest.functions[1].name.clone().into_boxed_str());
        vec![
            PluginFnDef {
                name: echo_name,
                arity: 1,
                permission: None,
                func: echo,
            },
            PluginFnDef {
                name: env_name,
                arity: 1,
                permission: Some(Capability::Environment),
                func: env,
            },
        ]
    }
    fn stop(&mut self) -> Result<(), String> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// The merge point: a started plugin's function is in `native_defs()`, which is
/// the list every engine installs from.
#[test]
fn a_started_plugins_functions_reach_the_native_table() {
    let _g = gate();
    register(Box::new(Sample::new("t_merge"))).expect("register");
    let defs = spid3r_lang::stdlib::native_defs();
    assert!(
        defs.iter().any(|(n, _)| *n == "t_merge_echo"),
        "the plugin function must be in the table all three engines read"
    );
    assert!(
        spid3r_lang::stdlib::builtin_defs()
            .iter()
            .all(|(n, _)| *n != "t_merge_echo"),
        "builtin_defs() must stay plugin-free, or registration would recurse"
    );
}

/// End to end: a script calls the plugin function and its result is asserted
/// inside the program, so no output capture is needed.
#[test]
fn a_script_can_call_a_plugin_function() {
    let _g = gate();
    register(Box::new(Sample::new("t_call"))).expect("register");
    let src = "fn main() {\n    assert_eq(t_call_echo(\"hi\"), \"hi\");\n    return 0;\n}\n";
    let out = Engine::new()
        .run("plugin_call.matrix", src)
        .expect("the script must run");
    assert_eq!(out.exit_code, 0, "output: {}", out.output);
}

/// The permission gate: `deny(Environment)` must reach a plugin function whose
/// declared permission is `environment`, and the script must be able to catch
/// it rather than crash.
#[test]
fn deny_reaches_a_plugin_function_and_is_catchable() {
    let _g = gate();
    register(Box::new(Sample::new("t_deny"))).expect("register");

    // Without deny, it returns the value.
    let ok = "fn main() {\n    assert_eq(t_deny_env(\"X\"), \"secret\");\n    return 0;\n}\n";
    assert_eq!(
        Engine::new().run("ok.matrix", ok).expect("runs").exit_code,
        0
    );

    // With deny, the call is a catchable error.
    let denied = "fn main() {\n    try {\n        t_deny_env(\"X\");\n        println(\"NOT DENIED\");\n    } catch (e) {\n        println(e);\n    }\n    return 0;\n}\n";
    let out = Engine::new()
        .deny(Capability::Environment)
        .run("denied.matrix", denied)
        .expect("a denied call is catchable, not fatal");
    assert_eq!(out.exit_code, 0);
    assert!(
        out.output.contains("capability denied"),
        "expected a capability error, got: {:?}",
        out.output
    );
    assert!(
        !out.output.contains("NOT DENIED"),
        "the plugin function ran despite deny: {:?}",
        out.output
    );
}

/// A permission the manifest never announced is a registration error, so a
/// plugin cannot quietly acquire authority it did not declare.
#[test]
fn an_undeclared_capability_blocks_registration() {
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
                name: "t_greedy_read",
                arity: 0,
                permission: Some(Capability::FileSystem),
                func: |_c, _a| Ok(Value::Null),
            }]
        }
    }
    let manifest = PluginManifest {
        name: "t_greedy".to_string(),
        version: "0.1.0".to_string(),
        abi: ABI_V1.to_string(),
        kind: PluginKind::Host,
        capabilities: Vec::new(), // deliberately does not declare filesystem
        entry: String::new(),
        functions: vec![FunctionDecl {
            name: "t_greedy_read".to_string(),
            arity: 0,
            permission: Some(Capability::FileSystem),
        }],
    };
    let err = register(Box::new(Greedy { manifest })).unwrap_err();
    assert!(err.contains("does not declare"), "{err}");
    assert!(
        !registered().iter().any(|(n, _)| *n == "t_greedy_read"),
        "a rejected plugin must offer nothing"
    );
}

/// A wrong ABI string is rejected before any plugin code runs.
#[test]
fn a_wrong_abi_is_rejected_before_start() {
    let _g = gate();
    struct WrongAbi {
        manifest: PluginManifest,
    }
    impl PluginHost for WrongAbi {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }
        fn functions(&self) -> Vec<PluginFnDef> {
            Vec::new()
        }
        fn start(&mut self) -> Result<(), String> {
            panic!("start must not run for a rejected ABI");
        }
    }
    let manifest = PluginManifest {
        name: "t_wrong_abi".to_string(),
        version: "0.1.0".to_string(),
        abi: "spid3r-plugin-abi-999".to_string(),
        kind: PluginKind::Host,
        capabilities: Vec::new(),
        entry: String::new(),
        functions: Vec::new(),
    };
    let err = register(Box::new(WrongAbi { manifest })).unwrap_err();
    assert!(err.contains("ABI"), "{err}");
}

/// Stopping is idempotent, and a stopped plugin stops offering functions.
#[test]
fn shutdown_is_idempotent_and_removes_the_functions() {
    let _g = gate();
    register(Box::new(Sample::new("t_stop"))).expect("register");
    assert!(registered_functions()
        .iter()
        .any(|f| f.name == "t_stop_echo"));

    shutdown_all();
    assert!(
        !registered_functions()
            .iter()
            .any(|f| f.name == "t_stop_echo"),
        "a stopped plugin must not offer functions"
    );
    // Second call is a no-op rather than a panic or a double-stop error.
    assert!(shutdown_all().is_empty());
}

/// A WASM plugin must not be loaded through the host path: it would get the
/// host's full authority while its manifest claims a sandbox. There is no WASM
/// loader, so registration must refuse by name rather than fall back to host.
#[test]
fn a_wasm_plugin_is_refused_rather_than_loaded_as_host() {
    let _g = gate();
    struct Wasm {
        manifest: PluginManifest,
    }
    impl PluginHost for Wasm {
        fn manifest(&self) -> &PluginManifest {
            &self.manifest
        }
        fn functions(&self) -> Vec<PluginFnDef> {
            Vec::new()
        }
    }
    let manifest = PluginManifest {
        name: "t_wasm".to_string(),
        version: "0.1.0".to_string(),
        abi: ABI_V1.to_string(),
        kind: PluginKind::Wasm,
        capabilities: Vec::new(),
        entry: String::new(),
        functions: Vec::new(),
    };
    let err = register(Box::new(Wasm { manifest })).unwrap_err();
    assert!(err.contains("WASM"), "{err}");
    assert!(!list().iter().any(|p| p.name == "t_wasm"));
}
