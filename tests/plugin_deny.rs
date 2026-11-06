//! Call-time plugin permission enforcement (audit findings S-1/S-2).
//!
//! `Engine::deny` must hold for plugin functions no matter when the plugin was
//! registered relative to the deny call, and no matter how the plugin is
//! backed (host `fn` pointers, WASM closures, matrix closures). The registry
//! and the deny set are process-global, so every test here takes the gate and
//! uses unique plugin/function names.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use spid3r_lang::embed::{Capability, Engine};
use spid3r_lang::plugin::{self, PluginFnDef};
use spid3r_lang::value::{NativeCtx, Value};

/// The registry and deny set are process-global; serialize the tests.
fn gate() -> std::sync::MutexGuard<'static, ()> {
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r_deny_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// The sample WASM plugin shipped in the repo.
fn wasm_sample_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/plugins/hello-wasm")
}

/// A script that calls `fn_name("x")` inside try/catch and reports either the
/// call result or the caught error, so the assertions below can tell the two
/// apart from `output` alone.
fn probe_src(fn_name: &str) -> String {
    format!(
        "fn main() {{\n    try {{\n        let r = {fn}(\"x\");\n        println(\"CALLED:\" + r);\n    }} catch e {{\n        println(\"DENIED:\" + e);\n    }}\n    return 0;\n}}\n",
        fn = fn_name
    )
}

// ------------------------------------------------------------------ matrix

/// A matrix plugin whose manifest declares `permission = "filesystem"` for its
/// function. Under `deny_all()` the call must be refused BEFORE the plugin's
/// Spid3r code runs — the whole point of the S-1 fix.
///
/// Returns the *search root* (a directory containing the plugin directory),
/// which is what `plugin::load_all_in` takes.
fn make_matrix_plugin(tag: &str, name: &str, fn_name: &str) -> PathBuf {
    let root = scratch(tag);
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!(
            "[plugin]\nname = \"{name}\"\nversion = \"0.1.0\"\nabi = \"spid3r-plugin-abi-1\"\nkind = \"matrix\"\nentry = \"probe.matrix\"\ncapabilities = [\"filesystem\"]\n\n[[function]]\nname = \"{fn_name}\"\narity = 1\npermission = \"filesystem\"\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("probe.matrix"),
        format!(
            "public fn {fn}(p) {{\n    return read_file(p);\n}}\n",
            fn = fn_name
        ),
    )
    .unwrap();
    root
}

#[test]
fn a_matrix_plugin_function_is_blocked_under_deny_all() {
    let _g = gate();
    plugin::reset_denied();
    let dir = make_matrix_plugin("matrix_denied", "deny-matrix-probe", "matrix_probe_read");
    let errs = plugin::load_all_in(&[dir.clone()]);
    assert!(errs.is_empty(), "plugin must load: {errs:?}");

    let engine = Engine::new().deny_all();
    let out = engine
        .run("matrix_deny", &probe_src("matrix_probe_read"))
        .expect("the script itself must run");
    assert!(
        out.output.contains("DENIED:capability denied: 'matrix_probe_read'"),
        "the plugin call must be refused with a denied error: {:?}",
        out.output
    );
    assert!(
        !out.output.contains("CALLED:"),
        "the plugin function must not execute: {:?}",
        out.output
    );
    plugin::shutdown_all();
    plugin::reset_denied();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Positive control: the same plugin, no denial — the function runs.
#[test]
fn an_undeclared_plugin_function_still_runs_without_a_deny() {
    let _g = gate();
    plugin::reset_denied();
    let root = std::env::temp_dir().join(format!(
        "spid3r_deny_matrix_ok_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("deny-matrix-ok");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "[plugin]\nname = \"deny-matrix-ok\"\nversion = \"0.1.0\"\nabi = \"spid3r-plugin-abi-1\"\nkind = \"matrix\"\nentry = \"probe.matrix\"\n\n[[function]]\nname = \"matrix_probe_pure\"\narity = 1\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("probe.matrix"),
        "public fn matrix_probe_pure(p) {\n    return \"pure:\" + p;\n}\n",
    )
    .unwrap();
    let errs = plugin::load_all_in(&[root.clone()]);
    assert!(errs.is_empty(), "plugin must load: {errs:?}");

    // A pure (permission-free) plugin function is not revocable by capability
    // denial: deny_all() must not break it.
    let engine = Engine::new().deny_all();
    let out = engine
        .run("matrix_ok", &probe_src("matrix_probe_pure"))
        .expect("the script must run");
    assert!(
        out.output.contains("CALLED:pure:x"),
        "a permission-free plugin function must still run: {:?}",
        out.output
    );
    plugin::shutdown_all();
    plugin::reset_denied();
    let _ = std::fs::remove_dir_all(&root);
}

// -------------------------------------------------------------------- wasm

/// A WASM plugin whose manifest declares `permission = "network"`: the
/// declaration must actually be enforced when the capability is denied (S-1,
/// WASM half). The sample module's `hello_greet` is re-declared with a
/// `network` permission, which the manifest allows and the call-time guard
/// then holds the closure to.
#[cfg(feature = "plugins-wasm")]
#[test]
fn a_wasm_plugin_permission_declaration_is_enforced_under_deny() {
    let _g = gate();
    plugin::reset_denied();
    let root = scratch("wasm_denied");
    let dir = root.join("deny-wasm-probe");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(wasm_sample_dir().join("plugin.wasm"), dir.join("plugin.wasm"))
        .expect("copy module");
    std::fs::write(
        dir.join("plugin.toml"),
        "[plugin]\nname = \"deny-wasm-probe\"\nversion = \"0.1.0\"\nabi = \"spid3r-plugin-abi-1\"\nkind = \"wasm\"\nentry = \"plugin.wasm\"\ncapabilities = [\"network\"]\n\n[[function]]\nname = \"hello_greet\"\narity = 1\npermission = \"network\"\n",
    )
    .unwrap();
    let errs = plugin::load_all_in(&[root]);
    assert!(errs.is_empty(), "plugin must load: {errs:?}");

    let engine = Engine::new().deny(Capability::Network);
    let out = engine
        .run("wasm_deny", &probe_src("hello_greet"))
        .expect("the script itself must run");
    assert!(
        out.output.contains("DENIED:capability denied: 'hello_greet'"),
        "the wasm function's declared permission must be enforced: {:?}",
        out.output
    );
    assert!(
        !out.output.contains("CALLED:"),
        "the wasm function must not execute: {:?}",
        out.output
    );
    plugin::shutdown_all();
    plugin::reset_denied();
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------- registration order

/// A host plugin registered AFTER `Engine::deny` is still blocked (S-2): the
/// engines overwrite denied host-plugin functions when they install globals.
#[test]
fn a_host_plugin_registered_after_deny_is_blocked() {
    let _g = gate();
    plugin::reset_denied();

    // Build (and deny with) the engine FIRST, register the plugin SECOND.
    let engine = Engine::new().deny_all();
    plugin::register(Box::new(OrderProbe {
        fname: "late_probe_read",
    }))
    .expect("register after deny must succeed (the gate is at call time)");

    let out = engine
        .run("late_host", &probe_src("late_probe_read"))
        .expect("the script itself must run");
    assert!(
        out.output.contains("DENIED:capability denied: 'late_probe_read'"),
        "a plugin registered after deny() must be blocked: {:?}",
        out.output
    );
    assert!(
        !out.output.contains("CALLED:"),
        "the late plugin function must not execute: {:?}",
        out.output
    );
    plugin::shutdown_all();
    plugin::reset_denied();
}

/// Host plugin probe: one filesystem-permission function that returns a
/// marker, so "executed" and "blocked" are distinguishable.
struct OrderProbe {
    fname: &'static str,
}

impl plugin::PluginHost for OrderProbe {
    fn manifest(&self) -> &plugin::PluginManifest {
        use std::sync::OnceLock;
        static MANIFEST: OnceLock<plugin::PluginManifest> = OnceLock::new();
        MANIFEST.get_or_init(|| plugin::PluginManifest {
            name: "late-host-probe".to_string(),
            version: "0.1.0".to_string(),
            abi: plugin::ABI_V1.to_string(),
            kind: plugin::PluginKind::Host,
            capabilities: vec![Capability::FileSystem],
            entry: String::new(),
            functions: vec![plugin::FunctionDecl {
                name: "late_probe_read".to_string(),
                arity: 1,
                permission: Some(Capability::FileSystem),
            }],
        })
    }

    fn functions(&self) -> Vec<PluginFnDef> {
        vec![PluginFnDef {
            name: self.fname,
            arity: 1,
            permission: Some(Capability::FileSystem),
            func: |_ctx: &NativeCtx, _args: Vec<Value>| {
                Ok(Value::Str("HOST_EXECUTED".into()))
            },
        }]
    }
}

// ------------------------------------------------------- wrapper guard unit

/// The closure wrapper itself: a captured closure that was created while the
/// capability was allowed must still refuse once it is denied (this is the
/// "handed out before deny, invoked after deny" hole).
#[test]
fn a_guarded_closure_refuses_once_the_capability_is_denied() {
    let _g = gate();
    plugin::reset_denied();

    let dir = make_matrix_plugin("guard_unit", "guard-matrix-probe", "guard_probe_read");
    let errs = plugin::load_all_in(&[dir.clone()]);
    assert!(errs.is_empty(), "plugin must load: {errs:?}");
    let closure = plugin::registered_closures()
        .into_iter()
        .find(|(n, _)| n == "guard_probe_read")
        .map(|(_, f)| f)
        .expect("the guarded closure exists in the registry");

    let ctx = NativeCtx {
        sched: spid3r_lang::sched::Scheduler::shared(),
    };
    // Allowed before the deny: the call goes through (read_file of a missing
    // path returns `ok(err(...))` per the language convention — the point is
    // that it is NOT capability-blocked).
    let before = closure(&ctx, vec![Value::Str("x".into())]);
    if let Err(e) = &before {
        assert!(
            !e.contains("capability denied"),
            "pre-deny call must not be capability-blocked: {e}"
        );
        panic!("pre-deny call failed in an unexpected way: {e}");
    }

    plugin::deny_capability(Capability::FileSystem);
    let after = closure(&ctx, vec![Value::Str("x".into())]).unwrap_err();
    assert!(
        after.contains("capability denied: 'guard_probe_read'"),
        "post-deny call must be capability-blocked: {after}"
    );

    plugin::shutdown_all();
    plugin::reset_denied();
    let _ = std::fs::remove_dir_all(&dir);
}
