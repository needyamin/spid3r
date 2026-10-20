//! Panic-hardening regression tests (2026-10-20 audit, Stage 1).
//!
//! Each test pins a fix for a defect where ordinary script input could abort
//! the host process instead of producing a clean Spid3r error. Every program
//! runs on BOTH engines (tree reference + bytecode VM) and both must produce
//! identical stdout — an abort on either engine fails the test loudly.
//!
//! Covered here:
//! * `bytes.from_hex` slicing mid-UTF-8-character (host panic before validation)
//! * regex pattern-parser recursion and NFA epsilon-closure recursion (stack
//!   overflow abort on deeply nested / huge alternation patterns)
//! * `sort`/`sorted`/`argsort` comparator that was not a total order over NaN
//!   and huge int/float mixes (Rust's sort panics on such input)

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

/// Write `src` to a temp `.matrix` file and run it on `engine`.
fn run_src(engine: &str, name: &str, src: &str) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!("spid3r_panic_hardening_{}.matrix", name));
    std::fs::write(&path, src).map_err(|e| format!("write {}: {}", path.display(), e))?;
    let o = Command::new(bin())
        .args(["run", "--engine", engine, path.to_str().unwrap()])
        .output()
        .map_err(|e| format!("spawn failed: {}", e))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(format!(
            "{} exit {}: {}",
            engine,
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stderr)
        ))
    }
}

/// Assert a program produces `expected` identically on tree and vm.
fn assert_both_engines(name: &str, src: &str, expected: &str) {
    let tree =
        run_src("tree", name, src).unwrap_or_else(|e| panic!("[{}] tree run failed:\n{}", name, e));
    let vm =
        run_src("vm", name, src).unwrap_or_else(|e| panic!("[{}] vm run failed:\n{}", name, e));
    assert_eq!(tree, expected, "[{}] tree output", name);
    assert_eq!(vm, expected, "[{}] vm output (diverged from tree)", name);
}

#[test]
fn from_hex_rejects_non_ascii_without_panicking() {
    // Regression: `bytes.from_hex` sliced `&s[i..i+2]` by byte offsets, so an
    // input like "a⚡" (even byte length, multi-byte char astride the slice
    // boundary) panicked the host before validation ran. It must be a clean,
    // catchable runtime error on both engines, while valid hex still works.
    assert_both_engines(
        "from_hex_utf8",
        r#"fn main() {
    try {
        bytes().from_hex("a⚡");
        println("BAD");
    } catch (e) {
        println("caught utf8");
    }
    try {
        bytes().from_hex("GG");
        println("BAD");
    } catch (e) {
        println("caught invalid");
    }
    println(bytes().from_hex("616263") == bytes("abc"));
    return 0;
}
"#,
        "caught utf8\ncaught invalid\ntrue\n",
    );
}

#[test]
fn regex_survives_deep_nesting_and_huge_alternation() {
    // Regression: the pattern parser recursed once per '(' without a cap and
    // the NFA epsilon-closure (add_state) recursed once per Split state, so a
    // deeply nested pattern or a ~10^5-branch alternation could abort the host
    // with a stack overflow. Nesting is now capped at 512 with a clean error,
    // and the closure is iterative (no depth limit). A 200k-branch alternation
    // must compile and match correctly.
    assert_both_engines(
        "regex_depth",
        r#"fn main() {
    let deep = "";
    let i = 0;
    while i < 600 { deep = deep + "("; i = i + 1; }
    let j = 0;
    while j < 600 { deep = deep + ")"; j = j + 1; }
    try {
        regex_match(deep, "a");
        println("BAD");
    } catch (e) {
        println("deep caught");
    }
    let parts = list();
    let k = 0;
    while k < 200000 { parts.push("a|"); k = k + 1; }
    let alt = parts.join("") + "a";
    println(regex_match(alt, "z"));
    println(regex_match(alt, "a"));
    return 0;
}
"#,
        "deep caught\nfalse\ntrue\n",
    );
}

#[test]
fn sort_and_argsort_are_total_over_nan_and_huge_ints() {
    // Regression: the sort comparator folded NaN (and f64-rounding collisions
    // between huge ints and neighboring floats) into `Equal`, which is not a
    // total order — Rust's sort panics on such input. NaN now sorts last,
    // Int↔Float comparisons are exact, and argsort() agrees with sort().
    assert_both_engines(
        "sort_total",
        r#"fn main() {
    let a = list(3.0, nan(), 1.0, nan(), 2.0);
    a.sort();
    println(a[0] == 1.0);
    println(a[1] == 2.0);
    println(a[2] == 3.0);
    println(is_nan(a[3]));
    println(is_nan(a[4]));
    let b = list(2.5, nan(), -1.0);
    let s = b.sorted();
    println(s[0] == -1.0);
    println(s[1] == 2.5);
    println(is_nan(s[2]));
    let c = list(1152921504606846977, 1152921504606846976, 1.152921504606847e18);
    c.sort();
    println(c[0] == c[1]);
    println(type_of(c[0]) == "int");
    println(type_of(c[1]) == "float");
    println(type_of(c[2]) == "int");
    let d = list(3, nan(), 1);
    let e = d.clone();
    e.sort();
    let by_perm = list();
    for i in d.argsort() { by_perm.push(d[i]); }
    // NaN never compares equal, not even to itself, so list-level `==` cannot
    // be used here; assert the two permutations element-wise instead.
    println(by_perm[0] == 1);
    println(by_perm[1] == 3);
    println(is_nan(by_perm[2]));
    println(e[0] == 1);
    println(e[1] == 3);
    println(is_nan(e[2]));
    return 0;
}
"#,
        concat!(
            "true\n", "true\n", "true\n", "true\n", "true\n", "true\n", "true\n", "true\n",
            "true\n", "true\n", "true\n", "true\n", "true\n", "true\n", "true\n", "true\n",
            "true\n", "true\n",
        ),
    );
}
