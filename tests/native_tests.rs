//! Native-backend differential tests — the fourth engine. Every example the
//! native backend supports is compiled ahead-of-time (`spid3r build`) and its
//! stdout must be byte-for-byte identical to the tree-walk interpreter.
//!
//! The native backend supports most of the language and runs it through the
//! differential: functions/closures, classes, `try`/`catch`/`finally`/`throw`,
//! `import` (flattened at build time), and `spawn`/`async`/`await` (tasks run on
//! the shared worker pool in `sched`). The only rejected construct is a class
//! declared inside a function (clean "not supported" diagnostic), never silently
//! miscompiled.
//!
//! Requires `cargo test --features native` plus a local LLVM 18 install
//! (see docs/BUILDING.md). Without the feature this file compiles to nothing.

#![cfg(feature = "native")]

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

/// Run the spid3r binary and return stdout, or an error string (stderr) on
/// non-zero exit / IO failure.
fn out(args: &[&str]) -> Result<String, String> {
    let o = Command::new(bin())
        .args(args)
        .output()
        .map_err(|e| format!("spawn failed: {}", e))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).to_string())
    }
}

/// Run a compiled executable directly and return its stdout, or stderr on
/// non-zero exit.
fn run_exe(path: &str) -> Result<String, String> {
    let o = Command::new(path)
        .output()
        .map_err(|e| format!("spawn failed: {}", e))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).to_string())
    }
}

/// The native backend links `LLVM-C.dll` beside the compiler (see build.rs);
/// if it was not copied there, skip the whole module instead of failing.
fn native_available() -> bool {
    let dll = bin().parent().map(|d| d.join("LLVM-C.dll")).unwrap_or_default();
    dll.is_file()
}

#[test]
fn native_matches_tree_on_supported_examples() {
    if !native_available() {
        eprintln!(
            "skip: LLVM-C.dll not found next to {} (build with SPID3R_LLVM_BIN_DIR set)",
            bin().display()
        );
        return;
    }
    let manifest = env!("CARGO_MANIFEST_DIR");
    let root = PathBuf::from(manifest).join("examples");
    assert!(root.is_dir(), "examples/ must exist at the crate root");

    let mut entries: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("read examples/")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "matrix").unwrap_or(false))
        .collect();
    entries.sort();

    let tmp = std::env::temp_dir();
    let mut checked = 0usize;
    let mut skipped = 0usize;

    for p in &entries {
        let path = p.to_str().unwrap();
        let src = std::fs::read_to_string(p).expect("read example");
        if !src.contains("fn main") {
            // library module: only the front end has to accept it
            out(&["check", path])
                .unwrap_or_else(|e| panic!("library {} must pass check:\n{}", path, e));
            continue;
        }

        let tree = out(&["run", "--engine", "tree", path])
            .unwrap_or_else(|e| panic!("tree run of {} failed:\n{}", path, e));

        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let exe = tmp.join(if cfg!(windows) {
            format!("spid3r_nt_{}.exe", stem)
        } else {
            format!("spid3r_nt_{}", stem)
        });
        let exe = exe.to_str().unwrap().to_string();

        match out(&["build", path, "-o", &exe]) {
            Ok(_) => {}
            Err(e) => {
                // documented v1 gaps must be rejected, never miscompiled
                if e.contains("not supported by the native backend yet") {
                    skipped += 1;
                    eprintln!("skip {}: {}", stem, e.lines().next().unwrap_or(""));
                    continue;
                }
                panic!("native build of {} failed unexpectedly:\n{}", path, e);
            }
        }

        let native = run_exe(&exe)
            .unwrap_or_else(|e| panic!("native run of {} failed:\n{}", path, e));
        assert_eq!(tree, native, "tree vs native stdout differ for {}", path);
        checked += 1;
        let _ = std::fs::remove_file(&exe);
    }

    assert!(
        checked >= 10,
        "expected at least 10 examples to run natively, only {} did",
        checked
    );
    eprintln!("native differential: {} passed, {} skipped (documented gaps)", checked, skipped);
}

#[test]
fn unsupported_features_are_rejected_with_clear_diagnostics() {
    if !native_available() {
        eprintln!("skip: LLVM-C.dll not found");
        return;
    }
    // (name, source, acceptable error substrings — the native backend's hard
    // diagnostic, plus the front end's own resolution error for `import`).
    // `spawn`/`async`/`await` are now supported and run through the differential.
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "import",
            "import \"x\"; fn main() { return 0; }",
            &["not supported by the native backend yet", "module"],
        ),
        (
            "class-in-fn",
            "fn main() { class A { fn new() {} } let a = new A(); return 0; }",
            &["not supported by the native backend yet", "unexpected keyword 'class'"],
        ),
    ];
    for (name, src, accepted) in cases {
        let dir = std::env::temp_dir();
        let src_path = dir.join(format!("spid3r_bad_{}.matrix", name));
        std::fs::write(&src_path, src).expect("write scratch");
        let src_path = src_path.to_str().unwrap().to_string();
        let exe = dir.join(format!("spid3r_bad_{}.exe", name));
        let exe = exe.to_str().unwrap().to_string();
        let r = out(&["build", &src_path, "-o", &exe]);
        match r {
            Ok(_) => panic!("{} must be rejected by the native backend", name),
            Err(e) => {
                assert!(
                    accepted.iter().any(|a| e.contains(a)),
                    "{} rejected with a non-diagnostic message:\n{}",
                    name,
                    e
                );
            }
        }
        let _ = std::fs::remove_file(&src_path);
        let _ = std::fs::remove_file(&exe);
    }
}
