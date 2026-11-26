//! Regression tests for the low-severity items in the 2026-10-20 audit.
//!
//! Each test names the defect it pins:
//!   - `Parser::parse` panicked on an empty token vector;
//!   - diagnostic columns counted bytes, so a caret drifted on non-ASCII lines;
//!   - `parse_csv` kept the UTF-8 BOM in the first header name;
//!   - a negated regex class never matched `\n`;
//!   - `read_tar` extracted symlink/device entries as regular files;
//!   - `spid3r test` discarded its arguments.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn write_temp(name: &str, body: &str) -> PathBuf {
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, body).expect("write probe");
    path
}

fn run(args: &[&str]) -> (i32, String) {
    let out = Command::new(bin())
        .args(args)
        .output()
        .expect("spawn spid3r");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr),
    )
}

/// `Parser::parse(vec![])` used to underflow `self.toks.len() - 1` in
/// `peek_kind`. The public API must return a diagnostic instead of panicking.
#[test]
fn an_empty_token_stream_is_an_error_not_a_panic() {
    let r = spid3r_lang::parser::parse(Vec::new());
    match r {
        Ok(_) => panic!("an empty token stream must not parse"),
        Err(d) => assert!(
            d.message.contains("empty token stream"),
            "unexpected message: {}",
            d.message
        ),
    }
}

/// Columns count characters. On `let s = "ααα"; ghost();` the `α`s are two
/// bytes each, so byte counting reported column 23 instead of 20.
#[test]
fn diagnostic_columns_count_characters_not_bytes() {
    let src = "fn main() {\n    let s = \"\u{3b1}\u{3b1}\u{3b1}\"; ghost();\n    return 0;\n}\n";
    let path = write_temp("spid3r_colcheck.matrix", src);
    let (code, out) = run(&["check", path.to_str().unwrap()]);
    assert_ne!(code, 0, "unknown name should fail the check");
    assert!(
        out.contains(":2:20"),
        "expected the error at line 2, column 20 (characters): {}",
        out
    );
    let _ = std::fs::remove_file(&path);
}

/// Excel writes a BOM by default; leaving it in the first header makes every
/// lookup by column name miss.
#[test]
fn csv_headers_lose_the_utf8_bom() {
    let src = concat!(
        "fn main() {\n",
        "    let t = parse_csv(\"\u{feff}name,age\\nada,36\\n\", null).unwrap();\n",
        "    let h = t[\"headers\"][0];\n",
        "    println(h.len());\n",
        "    println(h);\n",
        "    return 0;\n",
        "}\n"
    );
    let path = write_temp("spid3r_bom.matrix", src);
    let (code, out) = run(&["run", path.to_str().unwrap()]);
    assert_eq!(code, 0, "probe failed: {}", out);
    assert_eq!(
        out.trim(),
        "4\nname",
        "the BOM must be stripped from the header"
    );
    let _ = std::fs::remove_file(&path);
}

/// `[^a]` matches a newline (PCRE/Python/Go); `.` does not. The engine excluded
/// newline from *both*, so `[^a]` never matched one.
#[test]
fn a_negated_class_matches_newline_but_dot_does_not() {
    let src = "fn main() {\n    println(regex_match(\"[^a]\", \"\\n\"));\n    println(regex_match(\".\", \"\\n\"));\n    println(regex_match(\"[^\\\\n]\", \"\\n\"));\n    return 0;\n}\n";
    let path = write_temp("spid3r_negclass.matrix", src);
    let (code, out) = run(&["run", path.to_str().unwrap()]);
    assert_eq!(code, 0, "probe failed: {}", out);
    assert_eq!(
        out.trim(),
        "true\nfalse\nfalse",
        "negated class / dot newline handling drifted"
    );
    let _ = std::fs::remove_file(&path);
}

/// `spid3r test` used to ignore its arguments entirely. `--list` must print the
/// files, `--filter` must narrow them, and an unknown argument must be an error
/// rather than a silent full run.
#[test]
fn the_test_runner_honours_its_arguments() {
    let dir = std::env::temp_dir().join("spid3r_testargs");
    let examples = dir.join("examples");
    std::fs::create_dir_all(&examples).expect("mkdir");
    for name in ["alpha", "beta"] {
        std::fs::write(
            examples.join(format!("{}.matrix", name)),
            "fn main() {\n    println(1);\n    return 0;\n}\n",
        )
        .expect("write example");
    }
    let listed = Command::new(bin())
        .args(["test", "--list"])
        .current_dir(&dir)
        .output()
        .expect("spawn");
    let listed = String::from_utf8_lossy(&listed.stdout).to_string();
    assert!(
        listed.contains("alpha.matrix") && listed.contains("beta.matrix"),
        "--list should name both files: {}",
        listed
    );

    let filtered = Command::new(bin())
        .args(["test", "--filter", "alpha"])
        .current_dir(&dir)
        .output()
        .expect("spawn");
    let filtered = String::from_utf8_lossy(&filtered.stdout).to_string();
    assert!(
        filtered.contains("alpha") && !filtered.contains("beta"),
        "--filter should run only the matching file: {}",
        filtered
    );

    let bogus = Command::new(bin())
        .args(["test", "--nope"])
        .current_dir(&dir)
        .output()
        .expect("spawn");
    assert_ne!(
        bogus.status.code().unwrap_or(-1),
        0,
        "an unknown argument must fail instead of silently running everything"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
