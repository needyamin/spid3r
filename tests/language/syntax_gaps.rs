//! Two gaps a developer hits immediately, found by sweeping common idioms.
//!
//! 1. `else if` was a parse error — the language only accepted `elif`, which is
//!    a gratuitous difference from every C-family language people arrive from.
//! 2. `xs[-1]` was an error, and its message said "found int" for `-1`.
//!
//! Both are now supported; the `[]` operator takes Python-style negative
//! indices (`-1` is the last element), while method arguments such as
//! `remove`/`insert` keep their non-negative rule.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run(src: &str, tag: &str, engine: &str) -> (i32, String, String) {
    let dir = std::env::temp_dir().join("spid3r_syntax_gaps");
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

// ---------------------------------------------------------------- else if

#[test]
fn else_if_chains_parse() {
    both(
        "else_if",
        "fn grade(n) {\n    if (n > 90) {\n        return \"A\";\n    } else if (n > 80) {\n        return \"B\";\n    } else {\n        return \"C\";\n    }\n}\nfn main() {\n    println(grade(95));\n    println(grade(85));\n    println(grade(10));\n    return 0;\n}\n",
        "A\nB\nC",
    );
}

/// `elif` and `else if` are the same thing and may be mixed.
#[test]
fn elif_and_else_if_can_be_mixed() {
    both(
        "else_if_mixed",
        "fn f(n) {\n    if (n == 1) { return \"one\"; }\n    elif (n == 2) { return \"two\"; }\n    else if (n == 3) { return \"three\"; }\n    else { return \"many\"; }\n}\nfn main() {\n    println(f(1));\n    println(f(2));\n    println(f(3));\n    println(f(9));\n    return 0;\n}\n",
        "one\ntwo\nthree\nmany",
    );
}

/// `else if` also works in value position.
#[test]
fn else_if_works_in_a_value_position_if() {
    both(
        "else_if_value",
        "fn f(n) {\n    return if (n > 10) { \"big\" } else if (n > 0) { \"small\" } else { \"none\" };\n}\nfn main() {\n    println(f(50));\n    println(f(5));\n    println(f(-5));\n    return 0;\n}\n",
        "big\nsmall\nnone",
    );
}

// ------------------------------------------------------------ negative index

#[test]
fn a_negative_index_reads_from_the_end() {
    both(
        "neg_index",
        "fn main() {\n    let xs = [10, 20, 30];\n    println(xs[-1]);\n    println(xs[-3]);\n    println(\"abc\"[-1]);\n    let t = tuple(1, 2, 3);\n    println(t[-1]);\n    return 0;\n}\n",
        "30\n10\nc\n3",
    );
}

#[test]
fn a_negative_index_can_be_assigned() {
    both(
        "neg_index_set",
        "fn main() {\n    let xs = [1, 2, 3];\n    xs[-1] = 99;\n    xs[-3] = 11;\n    println(xs);\n    return 0;\n}\n",
        "[11, 2, 99]",
    );
}

/// An out-of-range negative index reports the number the user wrote, not the
/// wrapped one.
#[test]
fn an_out_of_range_negative_index_names_the_original_index() {
    for engine in ["tree", "vm"] {
        let (code, _, err) = run(
            "fn main() {\n    let xs = [1];\n    println(xs[-5]);\n    return 0;\n}\n",
            "neg_index_bad",
            engine,
        );
        assert_eq!(code, 1, "[{}] should exit 1", engine);
        assert!(
            err.contains("index -5 out of bounds for length 1"),
            "[{}] unexpected error: {}",
            engine,
            err
        );
    }
}

/// A non-integer index says what it found, and a negative one no longer claims
/// to have "found int".
#[test]
fn index_errors_name_the_actual_operand() {
    let (_, _, err) = run(
        "fn main() {\n    let xs = [1];\n    println(xs[\"a\"]);\n    return 0;\n}\n",
        "index_str",
        "tree",
    );
    assert!(
        err.contains("expects an integer index, found string"),
        "{}",
        err
    );

    let (_, _, err) = run(
        "fn main() {\n    let xs = [1];\n    println(xs.remove(-1));\n    return 0;\n}\n",
        "index_method",
        "tree",
    );
    // `remove` takes a VALUE, not an index, so -1 is simply not in the list.
    assert!(err.contains("not in list"), "{}", err);
}
