//! Spid3r-source plugins (`kind = "matrix"`): the kind that lets a plugin be
//! written in the language itself.
//!
//! These tests are **not** feature-gated: a matrix plugin needs no `wasmi`, so
//! the zero-dependency build must support it. That is a design requirement, not
//! an accident — `cargo build --no-default-features` still loads these.

use std::path::{Path, PathBuf};
use std::process::Command;

use spid3r_lang::plugin;

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

fn gate() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r_matrix_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).expect("mkdir parent");
    }
    std::fs::write(&p, body).expect("write");
}

fn run(args: &[&str], dir: &Path, plugins: &Path) -> (i32, String, String) {
    let o = Command::new(bin())
        .args(args)
        .current_dir(dir)
        .env("SPID3R_PLUGINS", plugins)
        .output()
        .expect("spawn spidr");
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

/// A plugin directory with the given manifest body and plugin source.
fn make_plugin(root: &Path, name: &str, manifest_body: &str, source: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("mkdir plugin");
    std::fs::write(dir.join("plugin.toml"), manifest_body).expect("write manifest");
    std::fs::write(dir.join(format!("{}.matrix", name)), source).expect("write source");
    dir
}

const MANIFEST: &str = "\
[plugin]
name = \"calc\"
version = \"0.1.0\"
abi = \"spid3r-plugin-abi-1\"
kind = \"matrix\"
entry = \"calc.matrix\"

[[function]]
name = \"double\"
arity = 1

[[function]]
name = \"label\"
arity = 1
";

const SOURCE: &str = "\
public fn double(n) {
    return n * 2;
}

public fn label(s) {
    return \"<\" + s + \">\";
}
";

// --------------------------------------------------------------- end to end

/// The whole point: a program calls functions from a plugin written in Spid3r,
/// on both script engines, with no build step and no Rust.
#[test]
fn a_program_calls_a_plugin_written_in_spid3r() {
    let root = scratch("e2e");
    make_plugin(&root, "calc", MANIFEST, SOURCE);
    let app = root.join("app");
    std::fs::create_dir_all(&app).unwrap();
    write(
        &app,
        "app.matrix",
        "fn main() {\n    println(double(21));\n    println(label(\"x\"));\n    return 0;\n}\n",
    );

    let mut outputs = Vec::new();
    for engine in ["tree", "vm"] {
        let (code, out, err) = run(&["run", "--engine", engine, "app.matrix"], &app, &root);
        assert_eq!(code, 0, "[{engine}] stderr: {err}");
        outputs.push(out);
    }
    assert_eq!(outputs[0], outputs[1], "tree and vm disagree");
    assert_eq!(outputs[0].trim_end(), "42\n<x>");
}

/// A plugin is a module: it must load **without** a `fn main`.
#[test]
fn a_plugin_needs_no_fn_main() {
    let root = scratch("nomain");
    make_plugin(&root, "calc", MANIFEST, SOURCE);
    let app = root.join("app");
    std::fs::create_dir_all(&app).unwrap();
    write(&app, "app.matrix", "fn main() {\n    return 0;\n}\n");
    let (code, _, err) = run(&["run", "app.matrix"], &app, &root);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        !err.contains("no fn main"),
        "a plugin must not be treated as a program: {err}"
    );
}

/// A throw inside a plugin reaches the caller's `catch` with its message
/// intact, not as the generic "uncaught throw".
#[test]
fn an_error_from_a_plugin_is_catchable() {
    let root = scratch("throw");
    make_plugin(
        &root,
        "calc",
        "[plugin]\nname = \"calc\"\nversion = \"0.1.0\"\nabi = \"spid3r-plugin-abi-1\"\nkind = \"matrix\"\nentry = \"calc.matrix\"\n\n[[function]]\nname = \"boom\"\narity = 0\n",
        "public fn boom() {\n    throw \"kaboom\";\n}\n",
    );
    let app = root.join("app");
    std::fs::create_dir_all(&app).unwrap();
    write(
        &app,
        "app.matrix",
        "fn main() {\n    try {\n        boom();\n    } catch (e) {\n        println(\"caught: \" + e);\n    }\n    return 0;\n}\n",
    );
    let (code, out, err) = run(&["run", "app.matrix"], &app, &root);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim_end(), "caught: kaboom");
}

/// A plugin sees its own globals, not the caller's: it cannot read or clobber
/// the calling program's variables.
#[test]
fn a_plugin_does_not_share_the_callers_globals() {
    let root = scratch("isolated");
    make_plugin(
        &root,
        "calc",
        "[plugin]\nname = \"calc\"\nversion = \"0.1.0\"\nabi = \"spid3r-plugin-abi-1\"\nkind = \"matrix\"\nentry = \"calc.matrix\"\n\n[[function]]\nname = \"peek\"\narity = 0\n",
        // `secret` only exists in the caller; this must not resolve to it.
        "public fn peek() {\n    return \"plugin-only\";\n}\n",
    );
    let app = root.join("app");
    std::fs::create_dir_all(&app).unwrap();
    write(
        &app,
        "app.matrix",
        "let secret = \"caller\";\nfn main() {\n    println(peek());\n    println(secret);\n    return 0;\n}\n",
    );
    let (code, out, err) = run(&["run", "app.matrix"], &app, &root);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim_end(), "plugin-only\ncaller");
}

// ------------------------------------------------------------------ loaders

/// A declared function the file does not define is a load error naming both.
#[test]
fn a_declared_function_missing_from_the_file_is_reported() {
    let _g = gate();
    let root = scratch("missing_fn");
    make_plugin(
        &root,
        "calc",
        "[plugin]\nname = \"calc\"\nversion = \"0.1.0\"\nabi = \"spid3r-plugin-abi-1\"\nkind = \"matrix\"\nentry = \"calc.matrix\"\n\n[[function]]\nname = \"nope\"\narity = 0\n",
        "public fn other() {\n    return 1;\n}\n",
    );
    let err = plugin::register_matrix(&root.join("calc")).unwrap_err();
    assert!(err.contains("nope"), "{err}");
    assert!(err.contains("does not define"), "{err}");
}

/// An arity that disagrees with the file is refused, so the manifest cannot
/// describe a call the language would reject.
#[test]
fn an_arity_mismatch_is_refused() {
    let _g = gate();
    let root = scratch("arity");
    make_plugin(
        &root,
        "calc",
        "[plugin]\nname = \"calc\"\nversion = \"0.1.0\"\nabi = \"spid3r-plugin-abi-1\"\nkind = \"matrix\"\nentry = \"calc.matrix\"\n\n[[function]]\nname = \"double\"\narity = 3\n",
        "public fn double(n) {\n    return n * 2;\n}\n",
    );
    let err = plugin::register_matrix(&root.join("calc")).unwrap_err();
    assert!(err.contains("parameter"), "{err}");
}

/// A missing entry file is reported against the plugin, naming the file.
#[test]
fn a_missing_entry_file_is_reported() {
    let _g = gate();
    let root = scratch("no_entry");
    let dir = root.join("calc");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "[plugin]\nname = \"calc\"\nversion = \"0.1.0\"\nabi = \"spid3r-plugin-abi-1\"\nkind = \"matrix\"\nentry = \"calc.matrix\"\n",
    )
    .unwrap();
    let err = plugin::register_matrix(&dir).unwrap_err();
    assert!(err.contains("calc.matrix"), "{err}");
}

/// The host path must refuse a matrix plugin: it has its own loader.
#[test]
fn the_host_loader_refuses_a_matrix_plugin() {
    let _g = gate();
    let root = scratch("hostpath");
    make_plugin(&root, "calc", MANIFEST, SOURCE);
    // Build a PluginHost whose manifest claims kind = "matrix".
    struct Fake {
        manifest: plugin::PluginManifest,
    }
    impl plugin::PluginHost for Fake {
        fn manifest(&self) -> &plugin::PluginManifest {
            &self.manifest
        }
        fn functions(&self) -> Vec<plugin::PluginFnDef> {
            Vec::new()
        }
    }
    let m = plugin::PluginManifest::parse(MANIFEST).expect("parse");
    let err = plugin::register(Box::new(Fake { manifest: m })).unwrap_err();
    assert!(err.contains("register_matrix"), "{err}");
}

/// `kind = "matrix"` is accepted by the parser and reported by `as_str`.
#[test]
fn the_matrix_kind_is_part_of_the_manifest_vocabulary() {
    let m = plugin::PluginManifest::parse(MANIFEST).expect("parse");
    assert_eq!(m.kind, plugin::PluginKind::Matrix);
    assert_eq!(m.kind.as_str(), "matrix");
    assert_eq!(m.functions.len(), 2);
}
