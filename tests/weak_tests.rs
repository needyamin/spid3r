//! Weak references (`weak` / `deref`) and reference self-equality.
//!
//! Weak references are the documented escape hatch for the reference-cycle
//! leak: values are reference-counted with no cycle collector, so back-edges
//! (e.g. `node.prev`) should hold a `weak` instead of a strong reference.
//! Also covers the self-equality fast path: `xs == xs` used to deadlock by
//! locking the same non-reentrant `Mutex` twice.

use std::process::Command;
use std::time::Duration;

fn run(engine: &str, src: &str, tag: &str) -> (i32, String, String) {
    let dir = std::env::temp_dir().join(format!("spid3r_weak_{}_{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let p = dir.join("prog.matrix");
    std::fs::write(&p, src).expect("write");
    let o = Command::new(env!("CARGO_BIN_EXE_spid3r"))
        .args(["run", "--engine", engine, p.to_str().unwrap()])
        .output()
        .expect("spawn");
    let _ = std::fs::remove_dir_all(&dir);
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

fn run_both(src: &str, tag: &str) -> (i32, String) {
    let (c1, o1, e1) = run("tree", src, tag);
    let (c2, o2, e2) = run("vm", src, tag);
    assert_eq!(
        c1, c2,
        "{}: exit codes differ (tree={}, vm={})",
        tag, e1, e2
    );
    assert_eq!(
        o1, o2,
        "{}: stdout differs\n--- tree ---\n{}\n--- vm ---\n{}",
        tag, o1, o2
    );
    (c1, o1)
}

/// A program that would hang if the self-equality deadlock regressed. Guarded
/// by a wall-clock limit because a hang in the child would otherwise wedge the
/// test suite rather than fail it.
fn run_with_timeout(engine: &str, src: &str, tag: &str, limit: Duration) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("spid3r_weak_t_{}_{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let p = dir.join("prog.matrix");
    std::fs::write(&p, src).expect("write");
    let started = std::time::Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_spid3r"))
        .args(["run", "--engine", engine, p.to_str().unwrap()])
        .output();
    let _ = std::fs::remove_dir_all(&dir);
    let elapsed = started.elapsed();
    let o = out.expect("spawn");
    assert!(
        elapsed < limit,
        "{}: took {:?} — the self-equality deadlock is back",
        tag,
        elapsed
    );
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
    )
}

#[test]
fn self_equality_is_true_and_fast_for_reference_values() {
    let src = "\
fn main() {
    let xs = list(1, 2, 3);
    println(xs == xs);
    let m = map(\"a\", 1);
    println(m == m);
    let s = set(1, 2);
    println(s == s);
    let t = tuple(1, 2);
    println(t == t);
    return 0;
}
";
    for engine in ["tree", "vm"] {
        let (code, out) = run_with_timeout(engine, src, "selfeq", Duration::from_secs(10));
        assert_eq!(code, 0);
        assert_eq!(out, "true\ntrue\ntrue\ntrue\n", "{}: {}", engine, out);
    }
}

#[test]
fn different_lists_are_still_compared_by_content() {
    let src = "\
fn main() {
    let a = list(1, 2, 3);
    let b = list(1, 2, 3);
    println(a == b);
    println(a == list(1, 2));
    return 0;
}
";
    let (code, out) = run_both(src, "content");
    assert_eq!(code, 0);
    assert_eq!(out, "true\nfalse\n", "{}", out);
}

#[test]
fn weak_round_trip_and_dead_weak() {
    let src = "\
fn main() {
    let xs = list(1, 2, 3);
    let w = weak(xs);
    println(deref(w).get(1));
    println(deref(w) == xs);
    let dead = weak(list(9));
    println(deref(dead));
    return 0;
}
";
    let (code, out) = run_both(src, "roundtrip");
    assert_eq!(code, 0);
    assert_eq!(out, "2\ntrue\nnull\n", "{}", out);
}

#[test]
fn weak_references_are_stored_in_containers() {
    // The cycle-mitigation pattern: a container holds a weak back-edge.
    let src = "\
fn main() {
    let xs = list(1, 2);
    xs.push(weak(xs));
    let back = xs.get(2);
    println(deref(back) == xs);
    println(deref(back).get(0));
    return 0;
}
";
    let (code, out) = run_both(src, "incontainer");
    assert_eq!(code, 0);
    assert_eq!(out, "true\n1\n", "{}", out);
}

#[test]
fn weak_errors_are_clear() {
    let src = "\
fn main() {
    let w = weak(42);
    return 0;
}
";
    let (c1, _, e1) = run("tree", src, "err");
    let (c2, _, e2) = run("vm", src, "err");
    assert_ne!(c1, 0);
    assert_ne!(c2, 0);
    for e in [e1, e2] {
        assert!(e.contains("cannot make a weak reference"), "error: {}", e);
    }
    // deref on a non-weak value is also an error.
    let src2 = "fn main() {\n    println(deref(list(1)));\n    return 0;\n}\n";
    let (c1, _, e1) = run("tree", src2, "err2");
    assert_ne!(c1, 0);
    assert!(
        e1.contains("deref expects a weak reference"),
        "error: {}",
        e1
    );
}

#[test]
fn weak_display_and_type_name() {
    let src = "\
fn main() {
    let xs = list(1);
    let w = weak(xs);
    println(string(w));
    println(type_of(w));
    return 0;
}
";
    let (c1, o1, e1) = run("tree", src, "display");
    assert_eq!(c1, 0, "{}{}", o1, e1);
    assert!(o1.contains("weak"), "output: {}", o1);
}
