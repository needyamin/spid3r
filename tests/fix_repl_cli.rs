//! Regression tests for the REPL/CLI fix trio (2026-10-19 audit):
//!
//! * F7 — REPL `:reset` must rebuild the whole per-session state (environment
//!   including its const set), not just the known-name list.
//! * F11 — REPL bare expressions may omit the trailing `;`; declarations and
//!   genuinely broken input must still error exactly as before.
//! * F13 — `spid3r fmt` expands directory arguments to their `*.matrix` files
//!   (sorted by name, non-recursive); a directory without .matrix files is a
//!   clean usage error; file arguments behave exactly as before.
//!
//! Every CLI invocation goes through one helper that pipes stdin, drains
//! stdout/stderr on reader threads, and polls `try_wait` with a 15 s deadline:
//! a hung subprocess is killed and fails the test instead of hanging the suite.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

/// Run `spid3r` with `args`, feed `stdin_text` through a pipe, and return
/// (exit code, stdout, stderr). Kills the child after 15 s.
fn run_with_stdin(args: &[&str], stdin_text: &str) -> (i32, String, String) {
    let mut child = Command::new(bin())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn spid3r");

    // Write stdin from its own thread: the child may exit before consuming it
    // (usage errors do), and a full pipe would otherwise block this test.
    let mut stdin = child.stdin.take().expect("child stdin");
    let input = stdin_text.to_string();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
        let _ = stdin.flush();
        // Dropping stdin closes the pipe, which ends the REPL's read loop.
    });

    let mut stdout_pipe = child.stdout.take().expect("child stdout");
    let mut stderr_pipe = child.stderr.take().expect("child stderr");
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + Duration::from_secs(15);
    let code = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break status.code().unwrap_or(-1),
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("spid3r {args:?} did not exit within 15s (possible hang)");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };
    let _ = writer.join();
    let stdout = String::from_utf8_lossy(&out_reader.join().unwrap_or_default()).to_string();
    let stderr = String::from_utf8_lossy(&err_reader.join().unwrap_or_default()).to_string();
    (code, stdout, stderr)
}

/// A throwaway directory, removed on drop (also on assertion failure).
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let path =
            std::env::temp_dir().join(format!("spid3r_fixcli_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        TempDir { path }
    }

    /// Write `text` to `rel` inside the directory (parents created) and return
    /// its path as a string.
    fn write(&self, rel: &str, text: &str) -> String {
        let p = self.path.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&p, text).expect("write temp file");
        p.to_string_lossy().to_string()
    }

    fn path_str(&self) -> String {
        self.path.to_string_lossy().to_string()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Canonical formatter output for a minimal program (verified: `fmt --check`
/// reports `unchanged` for exactly this text).
const FORMATTED: &str = "fn main() {\n    return 0;\n}\n";
/// The same program before formatting: `fmt --check` says `would reformat`,
/// `fmt` rewrites it to exactly `FORMATTED`.
const UNFORMATTED: &str = "fn main() {\nreturn 0;\n}\n";

// ---------------------------------------------------------------- F7: :reset

#[test]
fn reset_clears_const_sets_so_the_name_is_redefinable() {
    // Was: :reset only cleared the name list, so the runtime const set
    // survived and `let c = 2;` / `c = 3;` failed after the reset.
    let (code, out, err) = run_with_stdin(
        &["repl"],
        "const c = 1;\n:reset\nlet c = 2;\nc = 3;\nc\n:quit\n",
    );
    assert_eq!(code, 0, "repl should exit cleanly; stderr: {}", err);
    assert!(
        err.is_empty(),
        "no error expected after :reset (const set must be cleared too): {}",
        err
    );
    assert!(
        !out.contains("cannot assign to const"),
        "const violation after :reset; stdout:\n{}",
        out
    );
    // The final `c` echo is the last thing printed before the `:quit` prompt.
    assert!(
        out.trim_end().ends_with("3\n>"),
        "expected the final echo to be 3; stdout:\n{}",
        out
    );
}

#[test]
fn help_and_quit_are_unchanged() {
    let (code, out, err) = run_with_stdin(&["repl"], ":help\n:quit\n");
    assert_eq!(code, 0, "stderr: {}", err);
    assert!(err.is_empty(), "stderr: {}", err);
    assert!(out.contains(":quit leave"), "help text missing:\n{}", out);
    assert!(
        out.contains(":reset clear bindings"),
        "help text missing:\n{}",
        out
    );
}

// ------------------------------------------------------- F11: optional `;`

#[test]
fn bare_expression_without_semicolon_echoes() {
    let (code, out, err) = run_with_stdin(&["repl"], "2 + 2\n:quit\n");
    assert_eq!(code, 0, "stderr: {}", err);
    assert!(err.is_empty(), "missing `;` must not error: {}", err);
    assert!(
        out.trim_end().ends_with("4\n>"),
        "expected 4 to be echoed; stdout:\n{}",
        out
    );
}

#[test]
fn bare_expression_with_semicolon_still_echoes() {
    let (code, out, err) = run_with_stdin(&["repl"], "2 + 2;\n:quit\n");
    assert_eq!(code, 0, "stderr: {}", err);
    assert!(err.is_empty(), "stderr: {}", err);
    assert!(
        out.trim_end().ends_with("4\n>"),
        "expected 4 to be echoed; stdout:\n{}",
        out
    );
}

#[test]
fn multiline_bare_expression_without_final_semicolon_echoes() {
    // The buffer balances across lines; only the final `;` is missing.
    let (code, out, err) = run_with_stdin(&["repl"], "let a = 20;\n( a +\n2 )\n:quit\n");
    assert_eq!(code, 0, "stderr: {}", err);
    assert!(err.is_empty(), "stderr: {}", err);
    assert!(
        out.trim_end().ends_with("22\n>"),
        "expected 22 to be echoed; stdout:\n{}",
        out
    );
}

#[test]
fn declaration_without_semicolon_still_errors() {
    // Declarations are statements: their `;` stays required.
    let (code, out, err) = run_with_stdin(&["repl"], "let x = 5\n:quit\n");
    assert_eq!(code, 0, "the repl itself must survive; stderr: {}", err);
    assert!(
        err.contains("expected ';'"),
        "declaration without `;` must still be a parse error; stderr:\n{}",
        err
    );
    assert!(
        !out.contains("5\n"),
        "nothing should be echoed for the failed line; stdout:\n{}",
        out
    );
}

#[test]
fn genuinely_broken_expression_still_errors() {
    // `2 +` fails for a reason other than the missing `;`: the original
    // diagnostic must stand (the `;`-appended retry also fails).
    let (code, _out, err) = run_with_stdin(&["repl"], "2 +\n:quit\n");
    assert_eq!(code, 0, "the repl itself must survive; stderr: {}", err);
    assert!(
        err.contains("error["),
        "broken input must still report a diagnostic; stderr:\n{}",
        err
    );
    assert!(err.contains("unexpected end of file"), "stderr:\n{}", err);
}

// ------------------------------------------------------ F13: fmt directories

#[test]
fn fmt_check_on_directory_reports_unchanged_sorted_and_non_recursive() {
    let d = TempDir::new("dir_ok");
    d.write("b.matrix", FORMATTED);
    d.write("a.matrix", FORMATTED);
    // Must be ignored: not a .matrix file, and a nested directory.
    d.write("notes.txt", "not spid3r");
    d.write("nested/c.matrix", FORMATTED);

    let (code, out, err) = run_with_stdin(&["fmt", "--check", &d.path_str()], "");
    assert_eq!(code, 0, "stderr: {}", err);
    assert_eq!(
        out.matches("unchanged").count(),
        2,
        "exactly the two top-level .matrix files; stdout:\n{}",
        out
    );
    let ai = out.find("a.matrix").expect("a.matrix reported");
    let bi = out.find("b.matrix").expect("b.matrix reported");
    assert!(ai < bi, "files must be sorted by name; stdout:\n{}", out);
    assert!(!out.contains("c.matrix"), "must be non-recursive:\n{}", out);
    assert!(!out.contains("notes.txt"), "only .matrix files:\n{}", out);
    assert!(err.is_empty(), "stderr: {}", err);
}

#[test]
fn fmt_check_on_directory_without_matrix_files_is_a_clean_usage_error() {
    let d = TempDir::new("dir_empty");
    d.write("readme.txt", "nothing to format");
    let (code, out, err) = run_with_stdin(&["fmt", "--check", &d.path_str()], "");
    assert_eq!(code, 2, "usage errors exit 2; stdout:\n{}", out);
    assert!(
        err.contains("error[usage]"),
        "clean usage diagnostic expected; stderr:\n{}",
        err
    );
    assert!(
        err.contains("no .matrix files found in directory"),
        "stderr:\n{}",
        err
    );
    assert!(
        !err.contains("os error"),
        "must not leak an OS error for an empty directory; stderr:\n{}",
        err
    );
}

#[test]
fn fmt_check_directory_mixed_with_file_argument() {
    let d = TempDir::new("dir_mixed");
    d.write("a.matrix", FORMATTED);
    // The explicit file lives outside the directory's expansion: the dir arg
    // contributes a.matrix only (expansion is non-recursive), the explicit
    // arg contributes the nested file.
    let solo = d.write("sub/solo.matrix", FORMATTED);
    let (code, out, err) = run_with_stdin(&["fmt", "--check", &d.path_str(), &solo], "");
    assert_eq!(code, 0, "stderr: {}", err);
    assert_eq!(
        out.matches("unchanged").count(),
        2,
        "directory file + explicit file; stdout:\n{}",
        out
    );
    assert!(out.contains("a.matrix"), "stdout:\n{}", out);
    assert!(out.contains("solo.matrix"), "stdout:\n{}", out);
    assert!(err.is_empty(), "stderr: {}", err);
}

#[test]
fn fmt_on_single_file_still_behaves_exactly_as_before() {
    let d = TempDir::new("single_file");
    let ok = d.write("ok.matrix", FORMATTED);
    let ugly = d.write("ugly.matrix", UNFORMATTED);

    // Already-formatted file: unchanged, exit 0.
    let (code, out, err) = run_with_stdin(&["fmt", "--check", &ok], "");
    assert_eq!(code, 0, "stderr: {}", err);
    assert!(out.contains("unchanged"), "stdout:\n{}", out);

    // Unformatted file with --check: would reformat, exit 1.
    let (code, out, err) = run_with_stdin(&["fmt", "--check", &ugly], "");
    assert_eq!(code, 1, "stderr: {}", err);
    assert!(out.contains("would reformat"), "stdout:\n{}", out);

    // Without --check: rewritten in place, exit 0, canonical content.
    let (code, out, err) = run_with_stdin(&["fmt", &ugly], "");
    assert_eq!(code, 0, "stderr: {}", err);
    assert!(out.contains("formatted"), "stdout:\n{}", out);
    let after = std::fs::read_to_string(&ugly).expect("read rewritten file");
    assert_eq!(after, FORMATTED, "rewrite must produce canonical text");

    // And the rewritten file now checks clean.
    let (code, out, err) = run_with_stdin(&["fmt", "--check", &ugly], "");
    assert_eq!(code, 0, "stderr: {}", err);
    assert!(out.contains("unchanged"), "stdout:\n{}", out);
}
