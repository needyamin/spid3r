//! Audit A-17: deep container chains must not abort the process on teardown.
//!
//! A singly-owned chain (`s = stack(); s.push(s)`, repeated 200k times) is
//! walked by the default recursive `Drop` on the host stack. Before the fix,
//! `stack`/`queue`/`deque`/`bag` chains exited `0xC00000FD`
//! ("has overflowed its stack") *after* printing their result, so the program
//! looked correct right up to the moment the process died.
//!
//! Every probe runs in a child process on purpose: a regression here aborts the
//! process, which would take the whole test binary (and every later test) down
//! with it.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

/// Run `src` as a program and return `(exit_code, stdout, stderr)`.
fn run(src: &str, tag: &str) -> (i32, String, String) {
    let path = std::env::temp_dir().join(format!("spid3r_deepdrop_{}.matrix", tag));
    std::fs::write(&path, src).expect("write probe");
    let out = Command::new(bin())
        .args(["run", path.to_str().unwrap()])
        .output()
        .expect("spawn spid3r");
    let _ = std::fs::remove_file(&path);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// One chain of `kind` nested inside itself, 200 000 deep.
fn chain(kind: &str) -> String {
    format!(
        "fn main() {{\n    let d = 0;\n    let s = {k}();\n    \
         while (d < 200000) {{ let n = {k}(); n.push(s); s = n; d = d + 1; }}\n    \
         println(\"built\");\n    return 0;\n}}\n",
        k = kind
    )
}

#[test]
fn deep_container_chains_tear_down_without_overflowing_the_stack() {
    // The four that the audit reproduced, plus `set`/`list`/`bag`, which share
    // the same teardown path now.
    for kind in ["stack", "queue", "deque", "list", "set", "bag"] {
        let (code, out, err) = run(&chain(kind), kind);
        assert_eq!(
            code, 0,
            "a 200k-deep `{}` chain must exit 0, got {} ({}{})",
            kind, code, out, err
        );
        assert_eq!(out.trim(), "built", "`{}` chain did not finish", kind);
        assert!(
            !err.contains("overflow"),
            "`{}` chain overflowed the stack: {}",
            kind,
            err
        );
    }
}

/// A map chain (`m["k"] = prev`) takes a different teardown branch (entries
/// rather than `items`).
#[test]
fn a_deep_map_chain_tears_down() {
    let src = "fn main() {\n    let d = 0;\n    let m = map();\n    \
               while (d < 200000) { let n = map(); n[\"k\"] = m; m = n; d = d + 1; }\n    \
               println(\"built\");\n    return 0;\n}\n";
    let (code, out, err) = run(src, "map");
    assert_eq!(code, 0, "deep map chain failed: {}{}", out, err);
}

/// Instances are the most idiomatic deep structure in a dynamic language
/// (`class Node { next }`), and they tear down through the field map.
#[test]
fn a_deep_instance_chain_tears_down() {
    let src = "class Node {\n    public let next;\n    fn new() { self.next = null; }\n}\n\
               fn main() {\n    let d = 0;\n    let cur = new Node();\n    \
               while (d < 200000) {\n        let n = new Node();\n        n.next = cur;\n        \
               cur = n;\n        d = d + 1;\n    }\n    println(\"built\");\n    return 0;\n}\n";
    let (code, out, err) = run(src, "inst");
    assert_eq!(code, 0, "deep instance chain failed: {}{}", out, err);
}

/// A *mixed* chain is the case the three previous per-type guards each missed:
/// `list` only knew `list`, `map` only knew `map`+`list`, `set` only `set`+`list`.
#[test]
fn a_mixed_container_chain_tears_down() {
    let src = r#"fn wrap(kind, cur) {
    if (kind == 0) {
        let n = stack();
        n.push(cur);
        return n;
    } else {
        if (kind == 1) {
            let n = map();
            n["k"] = cur;
            return n;
        } else {
            if (kind == 2) {
                let n = deque();
                n.push(cur);
                return n;
            } else {
                if (kind == 3) {
                    let n = set();
                    n.add(cur);
                    return n;
                } else {
                    let n = bag();
                    n.add(cur);
                    return n;
                }
            }
        }
    }
}

fn main() {
    let d = 0;
    let cur = list();
    while (d < 200000) {
        cur = wrap(d % 5, cur);
        d = d + 1;
    }
    println("built");
    return 0;
}
"#;
    let (code, out, err) = run(src, "mixed");
    assert_eq!(code, 0, "mixed chain failed: {}{}", out, err);
}

/// Teardown must not corrupt values that are still shared: an aliased child is
/// left untouched when the container that held it is dropped.
#[test]
fn shared_children_survive_a_teardown() {
    let src = "fn main() {\n    let keep = list();\n    keep.push(1);\n    let s = stack();\n    \
               s.push(keep);\n    println(keep.len());\n    keep.push(2);\n    \
               println(keep.len());\n    return 0;\n}\n";
    let (code, out, err) = run(src, "shared");
    assert_eq!(code, 0, "shared-child program failed: {}{}", out, err);
    assert_eq!(out.trim(), "1\n2", "aliased list was disturbed");
}
