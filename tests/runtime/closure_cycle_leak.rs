//! Audit A-13 — closure/environment cycles ARE collected (2026-10-21).
//!
//! This file used to *pin the leak*: environments and closures were not
//! tracked cells, so `env -> closure -> env` was unreachable and never freed
//! (one leaked cell per closure created in a loop; see `docs/GC.md` for the
//! original measurement). Environments, `FunctionData`, and `ClassData` are
//! now registered cells (`environment::child*`, `value::new_function`,
//! `value::new_class`), and the collector gained cell-level edges
//! (`GcTrace::gc_trace_cells`) so both directions of the cycle are visible.
//!
//! These tests assert **reclamation**: after a forced `gc_collect()`, a loop
//! that creates closure cycles returns to its baseline live-cell count, and —
//! the safety half of the invariant — closures keep working across
//! collections (a premature sweep would show up as a wrong answer or a
//! missing binding, not a leak).

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run(src: &str, tag: &str) -> (i32, String) {
    let path = std::env::temp_dir().join(format!("spid3r_closure_leak_{}.matrix", tag));
    std::fs::write(&path, src).expect("write probe");
    let out = Command::new(bin())
        .args(["run", path.to_str().unwrap()])
        .output()
        .expect("spawn spid3r");
    let _ = std::fs::remove_file(&path);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr),
    )
}

fn counts(out: &str) -> Vec<usize> {
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| l.parse().expect("live cell count"))
        .collect()
}

/// The canonical A-13 repro: `make()` binds a closure in the very environment
/// it captures. After a forced collection the cycle cells must be gone.
#[test]
fn a_closure_cycle_is_collected() {
    let src = r#"fn make() {
    let data = list();
    data.push(1);
    let f = fn() { return data; };
    return f;
}
fn main() {
    gc_collect();
    let before = gc_live_cells();
    let i = 0;
    while (i < 2000) {
        let f = make();
        i = i + 1;
    }
    gc_collect();
    let after = gc_live_cells();
    println(before);
    println(after);
    return 0;
}
"#;
    let (code, out) = run(src, "cycle");
    assert_eq!(code, 0, "probe failed: {}", out);
    let lines = counts(&out);
    assert_eq!(lines.len(), 2, "expected two counts, got: {}", out);
    assert!(
        lines[1] <= lines[0] + 4,
        "closure cycles were not reclaimed: before={} after={} (delta {})",
        lines[0],
        lines[1],
        lines[1] as isize - lines[0] as isize
    );
}

/// A *nested named function* has the same shape: the declaration binds the
/// name in the environment the closure captures.
#[test]
fn a_nested_recursive_function_cycle_is_collected() {
    let src = r#"fn make() {
    let payload = list();
    payload.push(1);
    fn step(n) {
        if (n <= 0) { return payload.len(); }
        return step(n - 1);
    }
    return step(2);
}
fn main() {
    gc_collect();
    let before = gc_live_cells();
    let i = 0;
    while (i < 2000) {
        let n = make();
        i = i + 1;
    }
    gc_collect();
    let after = gc_live_cells();
    println(before);
    println(after);
    return 0;
}
"#;
    let (code, out) = run(src, "nested");
    assert_eq!(code, 0, "probe failed: {}", out);
    let lines = counts(&out);
    assert_eq!(lines.len(), 2, "expected two counts, got: {}", out);
    assert!(
        lines[1] <= lines[0] + 4,
        "nested-function cycles were not reclaimed: before={} after={}",
        lines[0],
        lines[1]
    );
}

/// The safety half: a closure captured across collections keeps working, and
/// the acyclic case (no cycle to collect) neither leaks nor over-collects.
#[test]
fn closures_keep_working_across_collections() {
    let src = r#"fn make(shared) {
    let f = fn() { return shared.len(); };
    return f();
}
fn main() {
    let shared = list();
    shared.push(1);
    gc_collect();
    let before = gc_live_cells();
    let i = 0;
    while (i < 2000) {
        let n = make(shared);
        if (n != 1) { println("WRONG"); return 1; }
        i = i + 1;
    }
    gc_collect();
    let after = gc_live_cells();
    println(before);
    println(after);
    println(shared.len());
    return 0;
}
"#;
    let (code, out) = run(src, "acyclic");
    assert_eq!(code, 0, "probe failed: {}", out);
    let lines = counts(&out);
    assert_eq!(lines.len(), 3, "expected three lines, got: {}", out);
    // The shared list survives every collection and the closures stay correct.
    assert_eq!(lines[2], 1, "the shared list was lost");
    assert!(
        lines[1] <= lines[0] + 4,
        "the acyclic case leaked: before={} after={}",
        lines[0],
        lines[1]
    );
}

/// A captured container survives the collections that run while the closure
/// is still reachable, and the closure still reads the *mutated* content —
/// the strongest signal that no live cell was swept.
#[test]
fn a_captured_container_survives_concurrent_collections() {
    let src = r#"fn main() {
    let data = list();
    let f = fn() { return data.len(); };
    let i = 0;
    while (i < 5000) {
        data.push(i);
        gc_collect();
        i = i + 1;
    }
    println(f());
    return 0;
}
"#;
    let (code, out) = run(src, "survive");
    assert_eq!(code, 0, "probe failed: {}", out);
    assert_eq!(
        out.trim(),
        "5000",
        "a captured container was corrupted or swept while live"
    );
}
