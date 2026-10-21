//! Module-namespace semantics (2026-10-21 audit, F-5 and the const-mutation
//! defect it uncovered).
//!
//! An import alias (`import "m" as mu`) is a **namespace**: a read-only view
//! of the module's exports. Two rules hold on both engines and in the
//! checker:
//!
//! 1. A `public const` export cannot be rebound through the namespace
//!    (`mu["VERSION"] = 1`, `mu.set("VERSION", 1)`) — the same rule that
//!    protects a local `const`.
//! 2. An exported name wins over a built-in map method: `mu.get()` calls the
//!    module's `get`, not `map.get` (which would demand a key argument).
//!
//! Both rules are checked at compile time when the key is a literal, and at
//! run time (the namespace is frozen) for a computed key.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

/// Write a helper module next to the temp dir and return its file name.
fn write_module(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("spid3r_ns_{}.matrix", name));
    std::fs::write(&path, src).expect("write module");
    path.file_name().unwrap().to_string_lossy().into_owned()
}

fn run(engine: &str, name: &str, src: &str) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!("spid3r_ns_{}.matrix", name));
    std::fs::write(&path, src).expect("write");
    let o = Command::new(bin())
        .args(["run", "--engine", engine, path.to_str().unwrap()])
        .output()
        .expect("run");
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).to_string())
    }
}

/// The program must fail identically on both engines, with `needle` in the
/// message.
fn assert_both_fail(name: &str, src: &str, needle: &str) {
    for engine in ["tree", "vm"] {
        let err = match run(engine, name, src) {
            Ok(out) => panic!("[{}] {} unexpectedly succeeded:\n{}", name, engine, out),
            Err(e) => e,
        };
        assert!(
            err.contains(needle),
            "[{}] {} message did not contain {:?}:\n{}",
            name,
            engine,
            needle,
            err
        );
    }
}

#[test]
fn public_const_export_cannot_be_rebound_through_the_namespace() {
    let m = write_module(
        "consts",
        "public const VERSION = 7;\npublic let counter = 0;\npublic fn get() { return VERSION; }\n",
    );
    // Index assignment with a literal key is rejected by the checker.
    assert_both_fail(
        "ns_const_index",
        &format!(
            "import \"{}\" as mu;\nfn main() {{\n    mu[\"VERSION\"] = 9;\n    return 0;\n}}\n",
            m
        ),
        "cannot assign to constant 'VERSION'",
    );
    // The `set` method form is rejected too.
    assert_both_fail(
        "ns_const_set",
        &format!(
            "import \"{}\" as mu;\nfn main() {{\n    mu.set(\"VERSION\", 9);\n    return 0;\n}}\n",
            m
        ),
        "cannot assign to constant 'VERSION'",
    );
}

#[test]
fn public_const_export_is_frozen_at_run_time_for_computed_keys() {
    // A computed key cannot be checked statically, so the runtime refuses it:
    // the namespace is frozen.
    let m = write_module("consts_rt", "public const VERSION = 7;\n");
    for engine in ["tree", "vm"] {
        let src = format!(
            "import \"{}\" as mu;\nfn main() {{\n    let k = \"VERSION\";\n    try {{ mu[k] = 9; println(\"assigned\"); }} catch(e) {{ println(\"frozen\"); }}\n    println(mu.VERSION);\n    return 0;\n}}\n",
            m
        );
        let out = run(engine, "ns_const_runtime", &src)
            .unwrap_or_else(|e| panic!("[{}] {} run failed:\n{}", "ns_const_runtime", engine, e));
        assert_eq!(
            out, "frozen\n7\n",
            "[{}] {} namespace was mutable",
            "ns_const_runtime", engine
        );
    }
}

#[test]
fn exported_names_shadow_built_in_map_methods() {
    // `get`, `len` and `set` are map methods; a module that exports functions
    // with those names must still be callable through the namespace.
    let m = write_module(
        "shadow",
        "public fn get() { return \"exported-get\"; }\npublic fn len() { return 42; }\n",
    );
    let src = format!(
        "import \"{}\" as mu;\nfn main() {{\n    println(mu.get());\n    println(mu.len());\n    return 0;\n}}\n",
        m
    );
    for engine in ["tree", "vm"] {
        let out = run(engine, "ns_shadow", &src)
            .unwrap_or_else(|e| panic!("[ns_shadow] {} run failed:\n{}", engine, e));
        assert_eq!(
            out, "exported-get\n42\n",
            "[ns_shadow] {} disagreed",
            engine
        );
    }
}

#[test]
fn namespace_is_read_only_for_mutable_exports_too() {
    // Even a non-constant export cannot be rebound through the namespace: it
    // is a view of the module, not a copy the importer owns.
    let m = write_module("readonly", "public let counter = 0;\n");
    for engine in ["tree", "vm"] {
        let src = format!(
            "import \"{}\" as mu;\nfn main() {{\n    try {{ mu[\"counter\"] = 1; println(\"assigned\"); }} catch(e) {{ println(\"readonly\"); }}\n    return 0;\n}}\n",
            m
        );
        let out = run(engine, "ns_readonly", &src)
            .unwrap_or_else(|e| panic!("[ns_readonly] {} run failed:\n{}", engine, e));
        assert_eq!(
            out, "readonly\n",
            "[ns_readonly] {} let a namespace be mutated",
            engine
        );
    }
}

#[test]
fn a_plain_map_is_still_mutable() {
    // The read-only rule applies to namespaces only — an ordinary map keeps
    // its documented mutability.
    for engine in ["tree", "vm"] {
        let out = run(
            engine,
            "ns_plain_map",
            "fn main() {\n    let m = map();\n    m.set(\"a\", 1);\n    m[\"b\"] = 2;\n    println(m.get(\"a\"));\n    println(m[\"b\"]);\n    return 0;\n}\n",
        )
        .unwrap_or_else(|e| panic!("[ns_plain_map] {} run failed:\n{}", engine, e));
        assert_eq!(out, "1\n2\n", "[ns_plain_map] {} disagreed", engine);
    }
}

#[test]
fn imported_const_binding_is_immutable_in_the_importer() {
    // The global import form (`import "m"`, no alias) binds the constant
    // directly: it must not be rebindable.
    let m = write_module("consts_global", "public const VERSION = 7;\n");
    assert_both_fail(
        "ns_const_global",
        &format!(
            "import \"{}\";\nfn main() {{\n    VERSION = 9;\n    return 0;\n}}\n",
            m
        ),
        "cannot assign to const 'VERSION'",
    );
}
