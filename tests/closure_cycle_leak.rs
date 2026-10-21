//! Audit A-13: closure/environment cycles are NOT collected — this pins the
//! current, measured behaviour so a future fix is visible.
//!
//! The collector tracks value containers and instances; a closure's environment
//! (`Env`) and `FunctionData` are not cells, so `env -> closure -> env` is
//! unreachable and never freed. See `docs/GC.md` ("Closures and environments
//! are NOT collected") for the measurement and for what a real fix requires.
//!
//! When environments become tracked, the numbers below drop to zero and these
//! assertions fail — that failure is the signal to delete this file and assert
//! reclamation instead.

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

/// Every iteration of `make()` leaks the list the closure captured, because the
/// closure is bound in the very environment it captures.
#[test]
fn a_closure_cycle_leaks_one_cell_per_iteration() {
    let src = r#"fn make() {
    let data = list();
    data.push(1);
    let f = fn() { return data; };
    return f;
}
fn main() {
    println(gc_live_cells());
    let i = 0;
    while (i < 2000) {
        let f = make();
        i = i + 1;
    }
    println(gc_live_cells());
    return 0;
}
"#;
    let (code, out) = run(src, "loop");
    assert_eq!(code, 0, "probe failed: {}", out);
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(
        lines,
        ["0", "2000"],
        "known A-13 leak changed shape — update docs/GC.md and this test"
    );
}

/// A *nested named function* has the same shape: the declaration binds the name
/// in the environment the closure captures, so recursion keeps it alive.
#[test]
fn a_nested_recursive_function_leaks_the_same_way() {
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
    let i = 0;
    while (i < 2000) {
        let n = make();
        i = i + 1;
    }
    println(gc_live_cells());
    return 0;
}
"#;
    let (code, out) = run(src, "nested");
    assert_eq!(code, 0, "probe failed: {}", out);
    assert_eq!(
        out.trim(),
        "2000",
        "known A-13 leak changed shape — update docs/GC.md and this test"
    );
}

/// The documented workaround: with the cycle broken there is nothing to collect,
/// so the count stays at zero. This also guards against a future fix that
/// over-collects (which would show up here as a wrong result, not a leak).
#[test]
fn a_closure_outside_a_cycle_is_reclaimed() {
    let src = r#"fn make(shared) {
    let f = fn() { return shared.len(); };
    return f();
}
fn main() {
    let shared = list();
    shared.push(1);
    let i = 0;
    while (i < 2000) {
        let n = make(shared);
        i = i + 1;
    }
    println(gc_live_cells());
    return 0;
}
"#;
    let (code, out) = run(src, "acyclic");
    assert_eq!(code, 0, "probe failed: {}", out);
    assert_eq!(
        out.trim(),
        "1",
        "the shared list must survive and nothing may leak"
    );
}
