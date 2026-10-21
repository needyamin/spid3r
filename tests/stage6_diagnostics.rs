//! Stage 6: diagnostics that are actionable — a source snippet with a caret,
//! `= help:` lines, and a machine-readable form (`check --json`).
//!
//! Before this, every error was `error[phase]: message` plus a `--> file:line:col`
//! line: no source, no caret, nothing a reader could act on without opening the
//! file at that position.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn write(tag: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("spid3r_stage6");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{}.matrix", tag));
    std::fs::write(&path, body).expect("write case");
    path
}

/// Character index of `needle` in `hay` (the renderer counts characters, while
/// `str::find` returns a byte offset).
fn char_index_of(hay: &str, needle: &str) -> usize {
    let byte = hay.find(needle).expect("needle");
    hay[..byte].chars().count()
}

fn check(path: &PathBuf, extra: &[&str]) -> (i32, String, String) {
    let mut args = vec!["check"];
    args.extend_from_slice(extra);
    args.push(path.to_str().unwrap());
    let o = Command::new(bin()).args(&args).output().expect("spawn");
    let _ = std::fs::remove_file(path);
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

/// The error carries the offending source line and a caret under the span.
#[test]
fn an_error_shows_the_source_line_and_a_caret() {
    let path = write(
        "snippet",
        "fn main() {\n    let xs: list<int> = [\"a\"];\n    return 0;\n}\n",
    );
    let (code, _, err) = check(&path, &[]);
    assert_ne!(code, 0);
    assert!(
        err.contains("2 |     let xs: list<int> = [\"a\"];"),
        "the source line should be shown:\n{}",
        err
    );
    // The caret sits under `["a"]` (5 characters).
    assert!(
        err.contains("^^^^^"),
        "a caret should mark the span:\n{}",
        err
    );
    // The caret line is aligned with the code line: same prefix width.
    let code_line = err
        .lines()
        .find(|l| l.contains("let xs: list<int>"))
        .expect("code line");
    let caret_line = err.lines().find(|l| l.contains("^^^^^")).expect("caret");
    assert_eq!(
        char_index_of(code_line, "[\"a\"]"),
        caret_line.find('^').unwrap(),
        "caret must start where the span starts://n{}",
        err
    );
}

/// Columns count characters, so a caret still lands correctly on a line with
/// non-ASCII text before the error.
#[test]
fn the_caret_aligns_on_a_line_with_non_ascii_text() {
    let path = write(
        "unicode",
        "fn main() {\n    let s = \"\u{3b1}\u{3b1}\u{3b1}\"; ghost();\n    return 0;\n}\n",
    );
    let (code, _, err) = check(&path, &[]);
    assert_ne!(code, 0);
    // The message line also contains "ghost", so select the snippet by its
    // gutter ("2 | ") instead.
    let code_line = err
        .lines()
        .find(|l| l.starts_with("2 |") && l.contains("ghost"))
        .expect("code line");
    let caret_line = err.lines().find(|l| l.contains('^')).expect("caret");
    assert_eq!(
        char_index_of(code_line, "ghost"),
        caret_line.find('^').unwrap(),
        "the caret must align past the α characters:\n{}",
        err
    );
}

/// A type mismatch explains what to do about it.
#[test]
fn a_type_mismatch_carries_a_help_line() {
    let path = write(
        "help",
        "fn main() {\n    let n: int = \"s\";\n    return 0;\n}\n",
    );
    let (code, _, err) = check(&path, &[]);
    assert_ne!(code, 0);
    assert!(
        err.contains("= help:") && err.contains("any"),
        "a mismatch should suggest a fix:\n{}",
        err
    );
}

/// `--json` emits exactly one object with the location fields an editor needs.
#[test]
fn json_mode_emits_one_object_with_a_location() {
    let path = write(
        "json",
        "fn main() {\n    let xs: list<int> = [\"a\"];\n    return 0;\n}\n",
    );
    let (code, out, err) = check(&path, &["--json"]);
    assert_ne!(code, 0);
    assert!(
        err.is_empty(),
        "json mode should not also print the human form"
    );
    let line = out.trim();
    assert!(
        line.starts_with('{') && line.ends_with('}'),
        "not json: {}",
        line
    );
    assert_eq!(
        line.matches('{').count(),
        line.matches('}').count(),
        "braces should balance: {}",
        line
    );
    for field in [
        "\"phase\":\"semantic\"",
        "\"message\":\"type mismatch in 'xs'",
        "\"line\":2",
        "\"col\":25",
        "\"len\":5",
        "\"help\":\"annotations are checked",
    ] {
        assert!(line.contains(field), "missing {} in {}", field, line);
    }
}

/// A runtime error gets a snippet too — it is where a stack trace matters most.
#[test]
fn a_runtime_error_shows_its_source_line() {
    let dir = std::env::temp_dir().join("spid3r_stage6");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("runtime.matrix");
    std::fs::write(
        &path,
        "fn main() {\n    let xs = [1];\n    println(xs[5]);\n    return 0;\n}\n",
    )
    .expect("write");
    let o = Command::new(bin())
        .args(["run", path.to_str().unwrap()])
        .output()
        .expect("spawn");
    let _ = std::fs::remove_file(&path);
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(
        err.contains("3 |     println(xs[5]);") && err.contains("^^^^^"),
        "a runtime error should show the line and caret:\n{}",
        err
    );
    assert!(
        err.contains("stack trace"),
        "the trace should still follow:\n{}",
        err
    );
}

/// A diagnostic with no readable file (a synthetic span) falls back to the
/// plain form instead of inventing a snippet.
#[test]
fn a_synthetic_span_falls_back_to_the_plain_form() {
    use spid3r_lang::diagnostics::{Diagnostic, Phase, Span};
    let d = Diagnostic::at(
        Phase::Semantic,
        Span::new(std::sync::Arc::from("<memory>"), 1, 1, 1),
        "synthetic",
    );
    let rendered = d.render();
    assert!(rendered.starts_with("error[semantic]: synthetic"));
    assert!(rendered.contains("--> <memory>:1:1"));
    assert!(!rendered.contains('^'), "no snippet for a fake file");
}
