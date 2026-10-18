//! `const` enforcement and module-system tests.
//!
//! Const: the semantic pass rejects assignment to a `const` binding (local or
//! imported). The tree runner additionally enforces it at run time via the
//! environment's const set (defense for dynamically imported bindings).
//! Modules: documented search order, caching, and import-cycle detection.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

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

fn write_tmp(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, src).expect("write tmp file");
    path.to_str().unwrap().to_string()
}

#[test]
fn assignment_to_const_is_rejected() {
    let cases: &[(&str, &str)] = &[
        (
            "local",
            "fn main() {\n    const x = 5;\n    x = 6;\n    return 0;\n}\n",
        ),
        (
            "compound",
            "fn main() {\n    const x = 5;\n    x += 1;\n    return 0;\n}\n",
        ),
        (
            "top_level",
            "const g = 1;\nfn main() {\n    g = 2;\n    return 0;\n}\n",
        ),
    ];
    for (name, src) in cases {
        let p = write_tmp(&format!("spid3r_const_bad_{}.matrix", name), src);
        for engine in ["tree", "vm"] {
            let err = out(&["run", "--engine", engine, &p]).unwrap_err();
            assert!(
                err.contains("cannot assign to const"),
                "{} ({}): expected a const error, got: {}",
                name,
                engine,
                err
            );
        }
    }
}

#[test]
fn const_binding_is_usable() {
    let p = write_tmp(
        "spid3r_const_ok.matrix",
        "const limit = 10;\nfn main() {\n    let total = 0;\n    for let i = 0; i < limit; i = i + 1 {\n        total = total + i;\n    }\n    println(total);\n    return 0;\n}\n",
    );
    let tree = out(&["run", "--engine", "tree", &p]).expect("tree run");
    let vm = out(&["run", "--engine", "vm", &p]).expect("vm run");
    assert_eq!(tree, vm);
    assert!(tree.contains("45"), "expected 45, got: {}", tree);
}

#[test]
fn imported_const_assignment_is_rejected_at_check_time() {
    let dir = std::env::temp_dir().join(format!("spid3r_modconst_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let module = dir.join("version.module.matrix");
    std::fs::write(&module, "public const VERSION = 7;\npublic fn ver() { return VERSION; }\n")
        .expect("write module");
    let user = dir.join("user.matrix");
    std::fs::write(
        &user,
        "import \"version.module\";\nfn main() {\n    println(ver());\n    VERSION = 8;\n    return 0;\n}\n",
    )
    .expect("write user");
    // Assigning to the imported const must fail the check...
    let err = out(&["check", user.to_str().unwrap()]).unwrap_err();
    assert!(
        err.contains("cannot assign to const 'VERSION'"),
        "expected a const error for the imported binding, got: "
    );
    // ...and the legal use must still run identically on both engines.
    let ok = dir.join("ok.matrix");
    std::fs::write(
        &ok,
        "import \"version.module\";\nfn main() {\n    println(ver());\n    return 0;\n}\n",
    )
    .expect("write ok");
    let tree = out(&["run", "--engine", "tree", ok.to_str().unwrap()]).expect("tree run");
    let vm = out(&["run", "--engine", "vm", ok.to_str().unwrap()]).expect("vm run");
    assert_eq!(tree, vm);
    assert!(tree.contains("7"), "expected 7, got: {}", tree);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_cycle_is_detected_with_a_chain() {
    let dir = std::env::temp_dir().join(format!("spid3r_modcycle_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let a = dir.join("cyc_a.matrix");
    let b = dir.join("cyc_b.matrix");
    std::fs::write(&a, "import \"cyc_b\";\npublic fn fa() { return 1; }\n").expect("write a");
    std::fs::write(&b, "import \"cyc_a\";\npublic fn fb() { return 2; }\n").expect("write b");
    let main = dir.join("cyc_main.matrix");
    std::fs::write(&main, "import \"cyc_a\";\nfn main() { return 0; }\n").expect("write main");
    let err = out(&["run", "--engine", "tree", main.to_str().unwrap()]).unwrap_err();
    assert!(
        err.contains("circular import"),
        "expected a circular-import error, got: "
    );
    let err_vm = out(&["run", "--engine", "vm", main.to_str().unwrap()]).unwrap_err();
    assert!(
        err_vm.contains("circular import"),
        "vm: expected a circular-import error, got: "
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spid3r_path_is_searched_for_imports() {
    let root = std::env::temp_dir().join(format!("spid3r_modpath_{}", std::process::id()));
    let libdir = root.join("libs");
    std::fs::create_dir_all(&libdir).expect("mkdir libs");
    std::fs::write(libdir.join("far.matrix"), "public fn far() { return 42; }\n")
        .expect("write module");
    let main = root.join("main.matrix");
    std::fs::write(&main, "import \"far\";\nfn main() {\n    println(far());\n    return 0;\n}\n")
        .expect("write main");
    // Without SPID3R_PATH the import cannot resolve next to the importer...
    assert!(out(&["run", "--engine", "tree", main.to_str().unwrap()]).is_err());
    // ...with it, both engines resolve and agree.
    let mut cmd_tree = Command::new(bin());
    cmd_tree
        .args(["run", "--engine", "tree", main.to_str().unwrap()])
        .env("SPID3R_PATH", libdir.to_str().unwrap());
    let t = String::from_utf8_lossy(&cmd_tree.output().expect("run tree").stdout).to_string();
    let mut cmd_vm = Command::new(bin());
    cmd_vm
        .args(["run", "--engine", "vm", main.to_str().unwrap()])
        .env("SPID3R_PATH", libdir.to_str().unwrap());
    let v = String::from_utf8_lossy(&cmd_vm.output().expect("run vm").stdout).to_string();
    assert_eq!(t, v);
    assert!(t.contains("42"), "expected 42, got: {}", t);
    let _ = std::fs::remove_dir_all(&root);
}
