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

#[test]
fn parser_recovering_reports_multiple_errors_in_check() {
    let path = write(
        "multi_err",
        "fn main() {\n    let a = ;\n    let b = ;\n    return 0;\n}\n",
    );
    let (code, _out, err) = check(&path, &[]);
    assert_eq!(code, 1);
    assert!(
        err.contains("2 |     let a = ;"),
        "expected first error on line 2, got:\n{}",
        err
    );
    assert!(
        err.contains("3 |     let b = ;"),
        "expected recovered second error on line 3, got:\n{}",
        err
    );

    // JSON mode emits one JSON object per recovered error with code & end_col.
    let path_json = write(
        "multi_err_json",
        "fn main() {\n    let a = ;\n    let b = ;\n    return 0;\n}\n",
    );
    let (jcode, jout, jerr) = check(&path_json, &["--json"]);
    assert_eq!(jcode, 1);
    assert!(jerr.is_empty());
    let lines: Vec<&str> = jout.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "expected 2 JSON diagnostic lines, got: {jout}"
    );
    assert!(lines[0].contains("\"code\":\"E1027\""));
    assert!(lines[0].contains("\"end_line\":2"));
    assert!(lines[1].contains("\"line\":3"));
}

#[test]
fn error_codes_end_spans_and_ansi_colors_work() {
    use spid3r_lang::diagnostics::{Diagnostic, Phase, Span};
    use spid3r_lang::lexer::Lexer;

    let toks = Lexer::new("let alpha = 42;\n", std::sync::Arc::from("<test>"))
        .tokenize()
        .expect("lex");
    // `alpha` starts at line 1, col 5, len 5 -> end_line 1, end_col 10.
    assert_eq!(toks[1].span.line, 1);
    assert_eq!(toks[1].span.col, 5);
    assert_eq!(toks[1].end_line(), 1);
    assert_eq!(toks[1].end_col(), 10);

    let d = Diagnostic::at(
        Phase::Parser,
        Span::new(std::sync::Arc::from("<test>"), 1, 5, 5),
        "unexpected token",
    )
    .with_code("E1027");
    let coded = d.render_with(Some("let alpha = 42;\n"));
    assert!(
        coded.starts_with("error[E1027][parser]: unexpected token"),
        "unexpected coded header: {}",
        coded
    );
    let colored = d.render_ansi(Some("let alpha = 42;\n"), true);
    assert!(colored.contains("\x1b[1;31merror[E1027][parser]\x1b[0m: unexpected token"));
    assert!(colored.contains("\x1b[1;34m-->\x1b[0m <test>:1:5"));
    let plain = d.render_ansi(Some("let alpha = 42;\n"), false);
    assert!(!plain.contains("\x1b["));
}
