//! Stage 2c: the deferred ergonomics — destructuring `let`, `for (k, v)`, and
//! `spawn` of any callable.
//!
//! All three are lowering/parser work: no backend gained a new capability.
//! Destructuring and the two-variable loop expand into ordinary statements;
//! `spawn` only lost a parser restriction that the runtime never needed.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run(src: &str, tag: &str, engine: &str) -> (i32, String, String) {
    let dir = std::env::temp_dir().join("spid3r_stage2c");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{}.matrix", tag));
    std::fs::write(&path, src).expect("write case");
    let o = Command::new(bin())
        .args(["run", "--engine", engine, path.to_str().unwrap()])
        .output()
        .expect("spawn spid3r");
    let _ = std::fs::remove_file(&path);
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

fn both(tag: &str, src: &str, expected: &str) {
    for engine in ["tree", "vm"] {
        let (code, out, err) = run(src, tag, engine);
        assert_eq!(code, 0, "[{} / {}] failed: {}", tag, engine, err);
        assert_eq!(out.trim_end(), expected, "[{} / {}] stdout", tag, engine);
    }
}

// ------------------------------------------------------------- destructuring

#[test]
fn a_let_binds_several_elements_at_once() {
    both(
        "destructure",
        "fn pair() {\n    return [10, 20];\n}\nfn main() {\n    let (a, b) = pair();\n    println(a + b);\n    let (x, y, z) = [1, 2, 3];\n    println(x * 100 + y * 10 + z);\n    return 0;\n}\n",
        "30\n123",
    );
}

/// The right-hand side is evaluated exactly once, even though several names
/// are bound from it.
#[test]
fn destructuring_evaluates_its_value_once() {
    both(
        "destructure_once",
        "fn make() {\n    println(\"make\");\n    return [1, 2];\n}\nfn main() {\n    let (a, b) = make();\n    println(a + b);\n    return 0;\n}\n",
        "make\n3",
    );
}

#[test]
fn destructured_names_are_ordinary_bindings() {
    both(
        "destructure_use",
        "fn main() {\n    let (a, b) = [1, 2];\n    a = a + 10;\n    println(a);\n    println(b);\n    let (c, d) = [a, b];\n    println(c + d);\n    return 0;\n}\n",
        "11\n2\n13",
    );
}

// ------------------------------------------------------------- for (k, v)

#[test]
fn a_two_variable_for_iterates_map_entries() {
    both(
        "for_kv",
        "fn main() {\n    let m = map();\n    m[\"one\"] = 1;\n    m[\"two\"] = 2;\n    let total = 0;\n    for (k, v) in m {\n        println(\"{k}={v}\");\n        total = total + v;\n    }\n    println(total);\n    return 0;\n}\n",
        // Insertion order, and the value is the map's, not the key.
        "one=1\ntwo=2\n3",
    );
}

/// The one-variable form is unchanged (it still yields keys for a map and
/// elements for a list).
#[test]
fn the_single_variable_loop_is_unaffected() {
    both(
        "for_single",
        "fn main() {\n    let m = map();\n    m[\"a\"] = 1;\n    for k in m {\n        println(k);\n    }\n    for v in [7, 8] {\n        println(v);\n    }\n    return 0;\n}\n",
        "a\n7\n8",
    );
}

// --------------------------------------------------------------- spawn

#[test]
fn spawn_accepts_a_named_function() {
    both(
        "spawn_named",
        "fn work() {\n    return 41;\n}\nfn main() {\n    let t = spawn work;\n    println(t.join());\n    return 0;\n}\n",
        "41",
    );
}

#[test]
fn spawn_accepts_a_closure_held_in_a_variable() {
    both(
        "spawn_closure",
        "fn make(n) {\n    return fn() {\n        return n + 1;\n    };\n}\nfn main() {\n    let c = make(6);\n    let t = spawn c;\n    println(t.join());\n    return 0;\n}\n",
        "7",
    );
}

/// A non-callable is a clear runtime error, not a panic or a silent no-op.
#[test]
fn spawning_a_non_callable_is_a_clean_error() {
    for engine in ["tree", "vm"] {
        let (code, _, err) = run(
            "fn main() {\n    let t = spawn 5;\n    return 0;\n}\n",
            "spawn_bad",
            engine,
        );
        assert_eq!(code, 1, "[{}] should exit 1", engine);
        assert!(
            err.contains("spawn expects a function"),
            "[{}] unexpected error: {}",
            engine,
            err
        );
    }
}
