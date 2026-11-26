//! `stdlib/assert.matrix` — the pure-Spid3r test-assertion framework.
//!
//! Run on BOTH engines: the module is plain Spid3r, so it must behave
//! identically everywhere. `done()` returns the failure count and `main`
//! returns it as the process exit code — 0 = suite passed.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run(engine: &str, name: &str, src: &str) -> (i32, String, String) {
    let dir = std::env::temp_dir().join("spid3r_assert");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{}.matrix", name));
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

const ALL_PASS: &str = "\
import \"assert\" as a;

fn main() {
    a.eq(2 + 2, 4, \"math\");
    a.eq([1, 2], [1, 2], \"lists are deep\");
    a.ne(1, 2, \"differ\");
    a.is_true(1 < 2, \"order\");
    a.near(0.1 + 0.2, 0.3, 0.0000001, \"floats\");
    a.contains(\"hello world\", \"world\", \"substring\");
    a.throws(fn() { throw \"boom\"; }, \"throws\");
    a.does_not_throw(fn() { let x = 1; }, \"no throw\");
    a.is_ok(read_file(\"Cargo.toml\"), \"ok result\");
    a.is_err(read_file(\"Z:/no/such/file.matrix\"), \"err result\");
    return a.done();
}
";

#[test]
fn passing_suite_exits_zero_on_both_engines() {
    for engine in ["vm", "tree"] {
        let (code, stdout, stderr) = run(engine, "assert_pass", ALL_PASS);
        assert_eq!(code, 0, "[{}] stderr: {}", engine, stderr);
        assert!(
            stdout.contains("10 passed, 0 failed"),
            "[{}] stdout: {}",
            engine,
            stdout
        );
    }
}

#[test]
fn failing_suite_exits_with_the_failure_count() {
    let src = "\
import \"assert\" as a;

fn main() {
    a.eq(1, 2, \"one is two\");
    a.is_true(false, \"false is true\");
    a.eq(\"same\", \"same\", \"this one passes\");
    return a.done();
}
";
    for engine in ["vm", "tree"] {
        let (code, stdout, stderr) = run(engine, "assert_fail", src);
        assert_eq!(code, 2, "[{}] exit code = failure count", engine);
        assert!(stdout.contains("FAIL one is two"));
        assert!(stdout.contains("FAIL false is true"));
        assert!(!stdout.contains("FAIL this one passes"));
        assert!(stdout.contains("1 passed, 2 failed"));
        assert!(stderr.is_empty(), "[{}] stderr: {}", engine, stderr);
    }
}

#[test]
fn non_result_value_fails_instead_of_erroring() {
    // A mistyped check (plain int where a result is expected) must read as a
    // test failure, not crash the suite.
    let src = "\
import \"assert\" as a;

fn main() {
    a.is_ok(42, \"an int is not a result\");
    return a.done();
}
";
    for engine in ["vm", "tree"] {
        let (code, stdout, stderr) = run(engine, "assert_type", src);
        assert_eq!(code, 1, "[{}]", engine);
        assert!(stdout.contains("FAIL an int is not a result"));
        assert!(stderr.is_empty(), "[{}] stderr: {}", engine, stderr);
    }
}
