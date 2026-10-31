//! `spidr plugin abi | list | check` — the tooling side of ABI v1.
//!
//! The point of these commands is that the contract is *inspectable*: you can
//! read the version, see what the search path would load, and validate a
//! directory without registering it. `check` must run the loader's own
//! validation, so a passing check and a failing load cannot disagree.
//!
//! Not feature-gated: a `matrix` plugin needs no `wasmi`, so this works in the
//! zero-dependency build.

use std::path::{Path, PathBuf};
use std::process::Command;

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

fn run(args: &[&str], cwd: &Path, plugins: Option<&Path>) -> (i32, String, String) {
    let mut cmd = Command::new(bin());
    cmd.args(args).current_dir(cwd);
    if let Some(p) = plugins {
        cmd.env("SPID3R_PLUGINS", p);
    } else {
        // An empty search path, so the machine's own plugins cannot leak in.
        cmd.env("SPID3R_PLUGINS", cwd.join("__none__"));
    }
    let o = cmd.output().expect("spawn spidr");
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r_abi_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples/plugins")
        .join(name)
}

/// The version string is readable, and it is the one the loaders compare
/// against.
#[test]
fn plugin_abi_prints_the_contract_version() {
    let dir = scratch("abi");
    let (code, out, err) = run(&["plugin", "abi"], &dir, None);
    assert_eq!(code, 0, "stderr: {err}");
    assert_eq!(out.trim(), "spid3r-plugin-abi-1");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A conforming Spid3r-source plugin passes, and the report says what it found.
#[test]
fn check_accepts_a_conforming_matrix_plugin() {
    let dir = scratch("ok");
    let (code, out, err) = run(
        &["plugin", "check", sample("hello-matrix").to_str().unwrap()],
        &dir,
        None,
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("conforms to spid3r-plugin-abi-1"), "{out}");
    assert!(out.contains("kind matrix"), "{out}");
    assert!(out.contains("public fn"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A wrong ABI string is the headline failure, and the message names both
/// sides so the author knows what to change.
#[test]
fn check_rejects_a_wrong_abi() {
    let dir = scratch("badabi");
    let plugin = dir.join("wrong");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(
        plugin.join("plugin.toml"),
        "[plugin]\nname = \"wrong\"\nversion = \"0.1.0\"\nabi = \"spid3r-plugin-abi-9\"\nkind = \"matrix\"\nentry = \"wrong.matrix\"\n\n[[function]]\nname = \"f\"\narity = 0\n",
    )
    .unwrap();
    std::fs::write(
        plugin.join("wrong.matrix"),
        "public fn f() {\n    return 1;\n}\n",
    )
    .unwrap();

    let (code, _, err) = run(&["plugin", "check", plugin.to_str().unwrap()], &dir, None);
    assert_eq!(code, 1, "an invalid plugin must exit 1");
    assert!(err.contains("spid3r-plugin-abi-9"), "{err}");
    assert!(err.contains("spid3r-plugin-abi-1"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A host plugin cannot be validated from disk. `check` must say that rather
/// than passing it — a silent pass would be a lie about what was verified.
#[test]
fn check_refuses_a_host_plugin_instead_of_passing_it() {
    let dir = scratch("host");
    let (code, _, err) = run(
        &["plugin", "check", sample("hello-host").to_str().unwrap()],
        &dir,
        None,
    );
    assert_eq!(code, 1);
    assert!(err.contains("cannot be validated from disk"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A declared function the file does not define is caught by `check`, so the
/// author sees it before shipping rather than at load.
#[test]
fn check_catches_a_declared_function_that_does_not_exist() {
    let dir = scratch("missing");
    let plugin = dir.join("p");
    std::fs::create_dir_all(&plugin).unwrap();
    std::fs::write(
        plugin.join("plugin.toml"),
        "[plugin]\nname = \"p\"\nversion = \"0.1.0\"\nabi = \"spid3r-plugin-abi-1\"\nkind = \"matrix\"\nentry = \"p.matrix\"\n\n[[function]]\nname = \"absent\"\narity = 0\n",
    )
    .unwrap();
    std::fs::write(
        plugin.join("p.matrix"),
        "public fn present() {\n    return 1;\n}\n",
    )
    .unwrap();

    let (code, _, err) = run(&["plugin", "check", plugin.to_str().unwrap()], &dir, None);
    assert_eq!(code, 1);
    assert!(err.contains("absent"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `list` reports what the search path would load, with each plugin's kind.
#[test]
fn list_reports_the_search_path() {
    let dir = scratch("list");
    let plugins = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/plugins");
    let (code, out, err) = run(&["plugin", "list"], &dir, Some(&plugins));
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("hello-matrix (matrix)"), "{out}");
    assert!(out.contains("hello-wasm (wasm)"), "{out}");
    // A host plugin is listed too: `list` reports what is *there*, and the
    // loader is what refuses it.
    assert!(out.contains("hello (host)"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// With nothing on the search path, `list` says so and names where it looked —
/// the most common "why did my plugin not load" question.
#[test]
fn list_explains_an_empty_search_path() {
    let dir = scratch("empty");
    let (code, out, _) = run(&["plugin", "list"], &dir, None);
    assert_eq!(code, 0);
    assert!(out.contains("no plugins found"), "{out}");
    assert!(out.contains("looked in"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A missing operand is a usage error (exit 2), not a crash or a silent pass.
#[test]
fn check_without_a_directory_is_a_usage_error() {
    let dir = scratch("usage");
    let (code, _, err) = run(&["plugin", "check"], &dir, None);
    assert_eq!(code, 2);
    assert!(err.contains("needs a directory"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
