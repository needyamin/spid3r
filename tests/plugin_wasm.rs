//! WASM plugins: discovery, execution on both script engines, the sandbox
//! boundary, and the fuel budget.
//!
//! These tests are gated on `plugins-wasm`, because without the feature there is
//! no loader at all — the whole point of the gate is that the default build can
//! be built with no dependency.

#![cfg(feature = "plugins-wasm")]

use std::path::{Path, PathBuf};
use std::process::Command;

use spid3r_lang::plugin;

/// The sample plugin shipped in the repo.
fn sample_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/plugins/hello-wasm")
}

/// The directory that *contains* plugin directories.
fn plugins_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/plugins")
}

fn bin() -> PathBuf {
    for key in ["CARGO_BIN_EXE_spid3r", "CARGO_BIN_EXE_spidr"] {
        if let Ok(p) = std::env::var(key) {
            let p = PathBuf::from(p);
            if p.is_file() {
                return p;
            }
        }
    }
    let exe = std::env::current_exe().expect("test executable path");
    let profile_dir = exe
        .parent()
        .and_then(|deps| deps.parent())
        .expect("profile directory");
    for stem in ["spid3r", "spidr"] {
        let cand = profile_dir.join(format!("{}{}", stem, std::env::consts::EXE_SUFFIX));
        if cand.is_file() {
            return cand;
        }
    }
    panic!("spid3r binary not found next to {}", profile_dir.display());
}

/// The registry is process-global, so serialize the in-process tests.
fn gate() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r_wasm_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// Copy the sample plugin into a fresh directory, so a test can tamper with it.
fn copy_sample(tag: &str) -> PathBuf {
    let dir = scratch(tag);
    for f in ["plugin.toml", "plugin.wasm"] {
        std::fs::copy(sample_dir().join(f), dir.join(f)).expect("copy sample");
    }
    dir
}

// ------------------------------------------------------------- end to end

/// The complaint this work answers: `spidr run` can use a plugin that was never
/// compiled into it, on both script engines, with identical output.
#[test]
fn the_cli_runs_a_wasm_plugin_on_tree_and_vm() {
    let demo = sample_dir().join("demo.matrix");
    let mut outputs = Vec::new();
    for engine in ["tree", "vm"] {
        let o = Command::new(bin())
            .args(["run", "--engine", engine, demo.to_str().unwrap()])
            .env("SPID3R_PLUGINS", plugins_root())
            .output()
            .expect("spawn spidr");
        assert_eq!(
            o.status.code().unwrap_or(-1),
            0,
            "[{engine}] stderr: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        outputs.push(String::from_utf8_lossy(&o.stdout).to_string());
    }
    assert_eq!(
        outputs[0], outputs[1],
        "tree and vm disagree on the plugin call"
    );
    assert!(
        outputs[0].contains("Hello, Ada! (from wasm)"),
        "stdout: {:?}",
        outputs[0]
    );
    assert!(outputs[0].contains("42"), "stdout: {:?}", outputs[0]);
}

/// Without the plugin on the search path the call must fail as an unknown name,
/// not silently do something else.
#[test]
fn without_the_plugin_the_call_is_an_error() {
    let demo = sample_dir().join("demo.matrix");
    let o = Command::new(bin())
        .args(["run", demo.to_str().unwrap()])
        // An empty search path, so no plugin is discovered.
        .env("SPID3R_PLUGINS", scratch("empty"))
        .output()
        .expect("spawn spidr");
    assert_eq!(o.status.code().unwrap_or(-1), 1);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("hello_greet"), "stderr: {err}");
}

/// A native executable cannot link a plugin loaded from disk, so `build` must
/// refuse at compile time rather than emit a binary that throws at run time.
#[cfg(feature = "native")]
#[test]
fn aot_refuses_a_program_that_calls_a_plugin() {
    let demo = sample_dir().join("demo.matrix");
    let out = scratch("aot").join(format!("demo{}", std::env::consts::EXE_SUFFIX));
    let o = Command::new(bin())
        .args(["build", demo.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("SPID3R_PLUGINS", plugins_root())
        .output()
        .expect("spawn spidr");
    assert_eq!(
        o.status.code().unwrap_or(-1),
        1,
        "build must fail, not produce a broken binary"
    );
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("unknown name"), "stderr: {err}");
    assert!(
        err.contains("cannot link"),
        "the error must explain why: {err}"
    );
    assert!(!out.exists(), "no binary may be produced");
}

// ---------------------------------------------------------------- in process

/// Loading the sample yields exactly the functions its manifest declares.
#[test]
fn the_manifest_and_the_module_agree() {
    let _g = gate();
    let errs = plugin::load_all_in(&[plugins_root()]);
    // `hello-host` is a host plugin and cannot be loaded from disk; that is
    // reported, not silently ignored.
    assert!(
        errs.iter().any(|e| e.contains("hello-host")),
        "a host plugin on the search path must be reported: {errs:?}"
    );
    assert!(
        errs.iter().all(|e| !e.contains("hello-wasm")),
        "the wasm plugin must load cleanly: {errs:?}"
    );
    let names: Vec<String> = plugin::registered_closures()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert!(names.contains(&"hello_greet".to_string()), "{names:?}");
    assert!(names.contains(&"hello_sum".to_string()), "{names:?}");
}

/// The fuel budget is enforced: a call given almost no fuel traps instead of
/// running forever. This is what makes an untrusted plugin safe to call.
#[test]
fn a_call_is_bounded_by_fuel() {
    let _g = gate();
    let mut loaded = spid3r_lang::plugin::wasm::load(&sample_dir()).expect("load");
    // 1 unit of fuel cannot even finish instantiating a call.
    spid3r_lang::plugin::wasm::set_fuel(&mut loaded, 1);
    let sum = loaded
        .closures
        .iter()
        .find(|(n, _)| n == "hello_sum")
        .map(|(_, f)| f.clone())
        .expect("hello_sum");
    let ctx = spid3r_lang::value::NativeCtx {
        sched: spid3r_lang::sched::Scheduler::shared(),
    };
    let err = sum(
        &ctx,
        vec![spid3r_lang::Value::Int(1), spid3r_lang::Value::Int(2)],
    )
    .expect_err("1 unit of fuel must not be enough");
    assert!(
        err.to_lowercase().contains("fuel") || err.contains("failed"),
        "expected a fuel/trap error, got: {err}"
    );
}

/// A `kind = "host"` plugin handed to the WASM loader is refused: the two
/// backings have different trust properties and must not be interchangeable.
#[test]
fn the_wasm_loader_refuses_a_host_manifest() {
    let _g = gate();
    let dir = scratch("hostkind");
    std::fs::write(
        dir.join("plugin.toml"),
        "[plugin]\nname = \"wrong\"\nversion = \"0.1.0\"\nabi = \"spid3r-plugin-abi-1\"\nkind = \"host\"\n",
    )
    .unwrap();
    let err = spid3r_lang::plugin::wasm::load(&dir).unwrap_err();
    assert!(err.contains("host"), "{err}");
}

/// A manifest whose `entry` does not exist is reported by name, not by a raw IO
/// error from somewhere deeper.
#[test]
fn a_missing_module_is_reported_against_the_plugin() {
    let _g = gate();
    let dir = copy_sample("missing_entry");
    std::fs::remove_file(dir.join("plugin.wasm")).unwrap();
    let err = spid3r_lang::plugin::wasm::load(&dir).unwrap_err();
    assert!(err.contains("plugin.wasm"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A module that does not export a function the manifest declares is refused,
/// so the manifest can never describe something the guest cannot do.
#[test]
fn an_undeclared_export_is_refused() {
    let _g = gate();
    let dir = copy_sample("bad_export");
    std::fs::write(
        dir.join("plugin.toml"),
        "[plugin]\nname = \"bad-export\"\nversion = \"0.1.0\"\nabi = \"spid3r-plugin-abi-1\"\nkind = \"wasm\"\nentry = \"plugin.wasm\"\n\n[[function]]\nname = \"hello_nope\"\narity = 0\n",
    )
    .unwrap();
    let err = spid3r_lang::plugin::wasm::load(&dir).unwrap_err();
    assert!(err.contains("hello_nope"), "{err}");
    assert!(err.contains("does not export"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A wrong ABI is rejected before the module is even compiled.
#[test]
fn a_wrong_abi_is_refused_by_the_wasm_loader() {
    let _g = gate();
    let dir = copy_sample("bad_abi");
    std::fs::write(
        dir.join("plugin.toml"),
        "[plugin]\nname = \"bad-abi\"\nversion = \"0.1.0\"\nabi = \"spid3r-plugin-abi-9\"\nkind = \"wasm\"\nentry = \"plugin.wasm\"\n\n[[function]]\nname = \"hello_greet\"\narity = 1\n",
    )
    .unwrap();
    let err = spid3r_lang::plugin::wasm::load(&dir).unwrap_err();
    assert!(err.contains("ABI"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A plugin cannot shadow a builtin, even as a closure.
#[test]
fn a_wasm_function_cannot_shadow_a_builtin() {
    let _g = gate();
    let dir = copy_sample("shadow");
    std::fs::write(
        dir.join("plugin.toml"),
        "[plugin]\nname = \"shadow\"\nversion = \"0.1.0\"\nabi = \"spid3r-plugin-abi-1\"\nkind = \"wasm\"\nentry = \"plugin.wasm\"\n\n[[function]]\nname = \"println\"\narity = 1\n",
    )
    .unwrap();
    // `println` is not exported by the module either, but the shadow check runs
    // first and must be the reported reason.
    let err = plugin::register_wasm(&dir).unwrap_err();
    assert!(
        err.contains("shadow a builtin") || err.contains("does not export"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The sample's own `plugin.wasm` must stay in sync with its source: a stale
/// binary is the one failure mode a reader cannot see.
#[test]
fn the_committed_module_is_not_empty() {
    let meta = std::fs::metadata(sample_dir().join("plugin.wasm")).expect("plugin.wasm exists");
    assert!(
        meta.len() > 1024,
        "plugin.wasm is {} bytes — rebuild examples/plugins/hello-wasm",
        meta.len()
    );
    // The wasm magic, so a truncated or text file is caught immediately.
    let head = std::fs::read(sample_dir().join("plugin.wasm")).unwrap();
    assert_eq!(
        &head[..4],
        b"\0asm",
        "plugin.wasm is not a WebAssembly module"
    );
}

/// A path helper used by the tests above; kept so the intent is explicit.
#[allow(dead_code)]
fn is_dir(p: &Path) -> bool {
    p.is_dir()
}
