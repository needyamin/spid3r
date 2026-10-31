//! SPM: manifest/lock compatibility, the add → verify → publish cycle, and
//! integrity.
//!
//! The plan's P3 exit criteria are the shape of this file: a full
//! add/remove/verify cycle in a temp dir with **no network**, a v1 manifest
//! that still installs, and a tampered vendored file that makes `spidr verify`
//! exit 1.

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

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r_pkg_{}_{}", tag, std::process::id()));
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

fn run_in(dir: &Path, args: &[&str]) -> (i32, String, String) {
    run_env(dir, args, &[])
}

fn run_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(bin());
    cmd.args(args).current_dir(dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let o = cmd.output().expect("spawn spidr");
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

/// A package with a manifest and a `src/lib.matrix`.
fn make_package(root: &Path, name: &str, version: &str, body: &str) {
    write(
        root,
        "spider.toml",
        &format!(
            "[package]\nname = \"{}\"\nversion = \"{}\"\n",
            name, version
        ),
    );
    write(root, "src/lib.matrix", body);
}

// ----------------------------------------------------------------- manifest

/// A v1 manifest — no `description`, no `license`, nothing but the three
/// original keys — must keep parsing. This is the R4 backward-compatibility
/// gate: existing projects must not need an edit to keep building.
#[test]
fn a_v1_manifest_still_parses_and_installs() {
    let dir = scratch("v1");
    make_package(
        &dir.join("mathlib"),
        "mathlib",
        "0.1.0",
        "public fn add(a, b) {\n    return a + b;\n}\n",
    );
    write(
        &dir,
        "app/spider.toml",
        "[package]\nname = \"app\"\nversion = \"1.0.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nmathlib = { path = \"../mathlib\" }\n",
    );
    write(
        &dir,
        "app/src/main.matrix",
        "import \"mathlib/lib\";\nfn main() {\n    println(add(2, 3));\n    return 0;\n}\n",
    );

    let app = dir.join("app");
    let (code, out, err) = run_in(&app, &["install"]);
    assert_eq!(code, 0, "v1 manifest failed to install: {err}");
    assert!(!out.is_empty(), "install printed nothing");

    // And the program still runs against the vendored copy.
    let (code, out, err) = run_in(&app, &["run", "src/main.matrix"]);
    assert_eq!(code, 0, "run failed: {err}");
    assert_eq!(out.trim_end(), "5");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `spidr verify` on a clean tree succeeds and counts what it checked.
#[test]
fn verify_reports_a_clean_tree() {
    let dir = scratch("verify_ok");
    make_package(
        &dir.join("mathlib"),
        "mathlib",
        "0.1.0",
        "public fn f() {\n    return 1;\n}\n",
    );
    write(
        &dir,
        "app/spider.toml",
        "[package]\nname = \"app\"\nversion = \"1.0.0\"\n\n[dependencies]\nmathlib = { path = \"../mathlib\" }\n",
    );
    write(
        &dir,
        "app/src/main.matrix",
        "fn main() {\n    return 0;\n}\n",
    );
    let app = dir.join("app");
    assert_eq!(run_in(&app, &["install"]).0, 0);

    let (code, out, err) = run_in(&app, &["verify"]);
    assert_eq!(code, 0, "verify failed on a clean tree: {err}");
    assert!(out.contains("1 vendored package"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The integrity gate**: editing one vendored byte must make `verify` exit 1
/// and name the package and both hashes.
#[test]
fn verify_fails_with_exit_1_when_a_vendored_file_is_tampered_with() {
    let dir = scratch("verify_bad");
    make_package(
        &dir.join("mathlib"),
        "mathlib",
        "0.1.0",
        "public fn f() {\n    return 1;\n}\n",
    );
    write(
        &dir,
        "app/spider.toml",
        "[package]\nname = \"app\"\nversion = \"1.0.0\"\n\n[dependencies]\nmathlib = { path = \"../mathlib\" }\n",
    );
    write(
        &dir,
        "app/src/main.matrix",
        "fn main() {\n    return 0;\n}\n",
    );
    let app = dir.join("app");
    assert_eq!(run_in(&app, &["install"]).0, 0);

    // The vendored copy mirrors the dependency's src/.
    write(
        &app,
        "vendor/mathlib/lib.matrix",
        "public fn f() {\n    return 999;\n}\n",
    );

    let (code, _, err) = run_in(&app, &["verify"]);
    assert_eq!(code, 1, "tampering must fail verify: {err}");
    assert!(err.contains("mathlib"), "the package must be named: {err}");
    assert!(err.contains("expected"), "both hashes must be shown: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

// -------------------------------------------------------------- add/remove

/// `add` declares the dependency, vendors it, and is idempotent.
#[test]
fn add_declares_vendors_and_is_idempotent() {
    let dir = scratch("add");
    make_package(
        &dir.join("mathlib"),
        "mathlib",
        "0.1.0",
        "public fn f() {\n    return 1;\n}\n",
    );
    write(
        &dir,
        "app/spider.toml",
        "[package]\nname = \"app\"\nversion = \"1.0.0\"\n",
    );
    write(
        &dir,
        "app/src/main.matrix",
        "fn main() {\n    return 0;\n}\n",
    );
    let app = dir.join("app");

    let (code, out, err) = run_in(&app, &["add", "mathlib", "--path", "../mathlib"]);
    assert_eq!(code, 0, "add failed: {err}");
    assert!(out.contains("added mathlib"), "{out}");

    let manifest = std::fs::read_to_string(app.join("spider.toml")).unwrap();
    assert!(manifest.contains("[dependencies]"), "{manifest}");
    assert!(
        manifest.contains("mathlib = { path = \"../mathlib\" }"),
        "{manifest}"
    );
    assert!(
        app.join("vendor/mathlib/lib.matrix").is_file(),
        "not vendored"
    );

    // Second add: no duplicate line, and it says so.
    let (code, out, _) = run_in(&app, &["add", "mathlib", "--path", "../mathlib"]);
    assert_eq!(code, 0);
    assert!(out.contains("already declared"), "{out}");
    let again = std::fs::read_to_string(app.join("spider.toml")).unwrap();
    assert_eq!(manifest, again, "the manifest changed on a repeat add");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A dependency that does not resolve must be rolled back out of the manifest,
/// not left behind to break every later command.
#[test]
fn a_failed_add_leaves_the_manifest_unchanged() {
    let dir = scratch("add_fail");
    write(
        &dir,
        "app/spider.toml",
        "[package]\nname = \"app\"\nversion = \"1.0.0\"\n",
    );
    write(
        &dir,
        "app/src/main.matrix",
        "fn main() {\n    return 0;\n}\n",
    );
    let app = dir.join("app");
    let before = std::fs::read_to_string(app.join("spider.toml")).unwrap();

    let (code, _, err) = run_in(&app, &["add", "ghost", "--path", "../nowhere"]);
    assert_ne!(code, 0, "adding a missing path must fail");
    assert!(
        err.contains("ghost"),
        "the error must name the package: {err}"
    );
    let after = std::fs::read_to_string(app.join("spider.toml")).unwrap();
    assert_eq!(before, after, "the manifest was not rolled back:\n{after}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `remove` drops the declaration and the vendored copy, so a later `verify`
/// does not report an unlocked package.
#[test]
fn remove_drops_the_declaration_and_the_vendored_copy() {
    let dir = scratch("remove");
    make_package(
        &dir.join("mathlib"),
        "mathlib",
        "0.1.0",
        "public fn f() {\n    return 1;\n}\n",
    );
    write(
        &dir,
        "app/spider.toml",
        "[package]\nname = \"app\"\nversion = \"1.0.0\"\n",
    );
    write(
        &dir,
        "app/src/main.matrix",
        "fn main() {\n    return 0;\n}\n",
    );
    let app = dir.join("app");
    assert_eq!(
        run_in(&app, &["add", "mathlib", "--path", "../mathlib"]).0,
        0
    );

    let (code, out, err) = run_in(&app, &["remove", "mathlib"]);
    assert_eq!(code, 0, "remove failed: {err}");
    assert!(out.contains("removed mathlib"), "{out}");
    let manifest = std::fs::read_to_string(app.join("spider.toml")).unwrap();
    assert!(!manifest.contains("mathlib"), "{manifest}");
    assert!(!app.join("vendor/mathlib").exists(), "vendor copy survived");

    let (code, _, _) = run_in(&app, &["verify"]);
    assert_eq!(code, 0, "verify must be clean after a remove");
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------- tree

/// `spidr tree` shows the declared dependency with its locked version and
/// origin.
#[test]
fn tree_prints_the_declared_dependencies() {
    let dir = scratch("tree");
    make_package(
        &dir.join("mathlib"),
        "mathlib",
        "0.1.0",
        "public fn f() {\n    return 1;\n}\n",
    );
    write(
        &dir,
        "app/spider.toml",
        "[package]\nname = \"app\"\nversion = \"1.0.0\"\n",
    );
    write(
        &dir,
        "app/src/main.matrix",
        "fn main() {\n    return 0;\n}\n",
    );
    let app = dir.join("app");
    assert_eq!(
        run_in(&app, &["add", "mathlib", "--path", "../mathlib"]).0,
        0
    );

    let (code, out, err) = run_in(&app, &["tree"]);
    assert_eq!(code, 0, "tree failed: {err}");
    assert!(out.starts_with("app 1.0.0"), "{out}");
    assert!(out.contains("mathlib 0.1.0"), "{out}");
    assert!(out.contains("../mathlib"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

// -------------------------------------------------------- publish → install

/// The plan's round trip: publish a package into a local registry directory,
/// then install it **by version** from that registry in a second project.
#[test]
fn publish_then_install_by_version_from_a_local_registry() {
    let dir = scratch("publish");
    make_package(
        &dir.join("mathlib"),
        "mathlib",
        "0.1.0",
        "public fn add(a, b) {\n    return a + b;\n}\n",
    );
    let lib = dir.join("mathlib");
    let registry = dir.join("registry");

    let (code, _, err) = run_in(&lib, &["publish", "--to-dir", registry.to_str().unwrap()]);
    assert_eq!(code, 0, "publish failed: {err}");
    assert!(
        registry.join("mathlib/0.1.0/spider.toml").is_file(),
        "registry layout is <dir>/<name>/<version>/"
    );
    assert!(
        registry.join("mathlib/0.1.0/sha256.txt").is_file(),
        "publish must write sha256.txt"
    );

    // A consumer resolves it by version, with no network.
    write(
        &dir,
        "app/spider.toml",
        "[package]\nname = \"app\"\nversion = \"1.0.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nmathlib = { version = \"^0.1.0\" }\n",
    );
    write(
        &dir,
        "app/src/main.matrix",
        "import \"mathlib/lib\";\nfn main() {\n    println(add(4, 5));\n    return 0;\n}\n",
    );
    let app = dir.join("app");
    let (code, _, err) = run_env(
        &app,
        &["install"],
        &[("SPID3R_REGISTRY", registry.to_str().unwrap())],
    );
    assert_eq!(code, 0, "install from the local registry failed: {err}");

    let (code, out, err) = run_env(
        &app,
        &["run", "src/main.matrix"],
        &[("SPID3R_REGISTRY", registry.to_str().unwrap())],
    );
    assert_eq!(code, 0, "run failed: {err}");
    assert_eq!(out.trim_end(), "9");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Publishing the same version twice is refused: a registry entry is
/// immutable, and silently overwriting one would make every existing
/// `spider.lock` a lie.
#[test]
fn publishing_an_existing_version_is_refused() {
    let dir = scratch("publish_twice");
    make_package(
        &dir.join("mathlib"),
        "mathlib",
        "0.1.0",
        "public fn f() {\n    return 1;\n}\n",
    );
    let lib = dir.join("mathlib");
    let registry = dir.join("registry");
    assert_eq!(
        run_in(&lib, &["publish", "--to-dir", registry.to_str().unwrap()]).0,
        0
    );
    let (code, _, err) = run_in(&lib, &["publish", "--to-dir", registry.to_str().unwrap()]);
    assert_ne!(code, 0, "the second publish must be refused");
    assert!(err.contains("already exists"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
