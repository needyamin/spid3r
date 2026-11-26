//! Numeric literal and loop-syntax extensions (2026-10-21 backlog).
//!
//! Two DX gaps closed after the audit found them by probing:
//! * Radix-prefixed integer literals — `0xFF`, `0o777`, `0b1010` (either
//!   prefix case) — and `_` digit separators in any digit run
//!   (`1_000`, `0xDEAD_BEEF`, `1_0.5_0`). Underscores must separate digits;
//!   leading, trailing, doubled, or punctuation-adjacent separators are
//!   clean lexer errors. A prefix without a digit (`0x`) stays the decimal
//!   `0` followed by an identifier, matching the documented `1elif` rule.
//! * The parenthesized single-variable `for` form — `for (x) in list` — so
//!   it is symmetric with the two-variable `for (k, v) in map`.
//!
//! Every case runs on all three engines.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run_src(engine: &str, name: &str, src: &str) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!("spid3r_literal_{}.matrix", name));
    std::fs::write(&path, src).map_err(|e| format!("write {}: {}", path.display(), e))?;
    let o = Command::new(bin())
        .args(["run", "--engine", engine, path.to_str().unwrap()])
        .output()
        .map_err(|e| format!("spawn failed: {}", e))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).to_string())
    }
}

fn assert_all_engines(name: &str, src: &str, expected: &str) {
    for engine in ["tree", "vm"] {
        let out = run_src(engine, name, src)
            .unwrap_or_else(|e| panic!("[{}] {} run failed:\n{}", name, engine, e));
        assert_eq!(out, expected, "[{}] {} output", name, engine);
    }
    let path = std::env::temp_dir().join(format!("spid3r_literal_{}.matrix", name));
    let tc = std::env::temp_dir().join(format!("spid3r_literal_{}.matrixc", name));
    let comp = Command::new(bin())
        .args([
            "compile",
            path.to_str().unwrap(),
            "-o",
            tc.to_str().unwrap(),
        ])
        .output()
        .expect("compile");
    assert!(
        comp.status.success(),
        "[{}] compile failed: {}",
        name,
        String::from_utf8_lossy(&comp.stderr)
    );
    let exec = Command::new(bin())
        .args(["exec", tc.to_str().unwrap()])
        .output()
        .expect("exec");
    assert_eq!(
        String::from_utf8_lossy(&exec.stdout).to_string(),
        expected,
        "[{}] exec output",
        name
    );
}

fn assert_lexer_error(name: &str, src: &str, needle: &str) {
    for engine in ["tree", "vm"] {
        let err = match run_src(engine, name, src) {
            Ok(out) => panic!("[{}] {} unexpectedly succeeded:\n{}", name, engine, out),
            Err(e) => e,
        };
        assert!(
            err.contains("error[lexer]") && err.contains(needle),
            "[{}] {} expected a lexer error containing {:?}, got:\n{}",
            name,
            engine,
            needle,
            err
        );
    }
}

#[test]
fn radix_prefixed_integers() {
    assert_all_engines(
        "radix",
        r#"fn main() {
    println(0xFF);
    println(0x10);
    println(0X1F);
    println(0b1010);
    println(0o777);
    println(0xDEAD_BEEF);
    println(0xFF + 1);
    return 0;
}
"#,
        "255\n16\n31\n10\n511\n3735928559\n256\n",
    );
}

#[test]
fn digit_separators_in_decimal_and_radix_literals() {
    assert_all_engines(
        "separators",
        r#"fn main() {
    println(1_000);
    println(0xDEAD_BEEF);
    println(1_0.5_0);
    println(1_000_000);
    println(0b1_0);
    return 0;
}
"#,
        "1000\n3735928559\n10.5\n1000000\n2\n",
    );
}

#[test]
fn malformed_separators_are_lexer_errors() {
    assert_lexer_error(
        "sep_double",
        "fn main() { let v = 1__0; return 0; }\n",
        "underscores must separate digits",
    );
    assert_lexer_error(
        "sep_trailing",
        "fn main() { let v = 1_; return 0; }\n",
        "underscores must separate digits",
    );
    assert_lexer_error(
        "sep_before_dot",
        "fn main() { let v = 1_.5; return 0; }\n",
        "underscores must separate digits",
    );
}

#[test]
fn radix_overflow_is_a_clean_lexer_error() {
    assert_lexer_error(
        "radix_overflow",
        "fn main() { let v = 0xFFFFFFFFFFFFFFFFF; return 0; }\n",
        "out of i64 range",
    );
}

#[test]
fn prefix_without_digits_stays_zero_plus_ident() {
    // `0x` is the decimal `0` followed by the identifier `x` — the documented
    // `1elif` rule — so `0x` alone is a parse error, not a value.
    for engine in ["tree", "vm"] {
        let err = match run_src(
            engine,
            "prefix_bare",
            "fn main() { let v = 0x; return 0; }\n",
        ) {
            Ok(out) => panic!("[prefix_bare] {} unexpectedly succeeded: {}", engine, out),
            Err(e) => e,
        };
        assert!(
            err.contains("error[parser]"),
            "[prefix_bare] {} expected a parser error, got:\n{}",
            engine,
            err
        );
    }
}

#[test]
fn parenthesized_single_variable_for_in() {
    assert_all_engines(
        "for_paren",
        r#"fn main() {
    let total = 0;
    for (x) in [1, 2, 3] { total += x; }
    println(total);
    for (i) in ["a", "b"] { println(i); }
    for (k, v) in map(1, "one") { println(k); }
    for x in [7] { println(x); }
    return 0;
}
"#,
        "6\na\nb\n1\n7\n",
    );
}
