use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

fn spid3r_bin() -> &'static str {
    env!("CARGO_BIN_EXE_spid3r")
}

fn scratch(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "spid3r_doc_test_{}_{}_{}",
        label,
        std::process::id(),
        nanos
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_file(path: &Path, content: &str) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

fn run_spid3r(cwd: &Path, args: &[&str]) -> Output {
    Command::new(spid3r_bin())
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("spid3r runs")
}

#[test]
fn doc_json_extracts_module_and_symbols_and_filters_private() {
    let dir = scratch("json");
    let src_path = dir.join("math.matrix");
    write_file(
        &src_path,
        "\
/// Math utility module.
/// Handles arithmetic and shapes.

/// Adds two integers together.
/// Returns the sum.
public fn add(a: int, b: int) -> int {
    return a + b;
}

/// Internal helper skipped by default.
fn secret_helper() -> int {
    return 42;
}

/// A mutable step counter.
public class Counter {
    public let value: int;
    let hidden: int;

    /// Advance the counter.
    public fn inc(step: int) -> int {
        return step;
    }

    fn reset() {
        return;
    }
}

/// Types that can format themselves.
public interface Printable {
    fn format() -> str;
}

/// Geometric shape variants.
public enum Shape {
    Empty,
    Circle(radius)
}

/// Upper bound constant.
public const MAX: int = 100;
",
    );

    let out = run_spid3r(&dir, &["doc", "--json", src_path.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "spid3r doc --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout
            .contains("\"module_doc\": \"Math utility module.\\nHandles arithmetic and shapes.\""),
        "{stdout}"
    );
    assert!(stdout.contains("\"name\": \"add\""), "{stdout}");
    assert!(
        stdout.contains("public fn add(a: int, b: int) -> int"),
        "{stdout}"
    );
    assert!(
        stdout.contains("\"doc\": \"Adds two integers together.\\nReturns the sum.\""),
        "{stdout}"
    );
    assert!(stdout.contains("\"name\": \"Counter\""), "{stdout}");
    assert!(stdout.contains("\"name\": \"inc\""), "{stdout}");
    assert!(stdout.contains("\"name\": \"Printable\""), "{stdout}");
    assert!(stdout.contains("\"name\": \"Shape\""), "{stdout}");
    assert!(stdout.contains("\"name\": \"Circle\""), "{stdout}");
    assert!(stdout.contains("\"name\": \"MAX\""), "{stdout}");
    assert!(
        !stdout.contains("secret_helper"),
        "private top-level fn should be excluded without --private:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"name\": \"reset\"") && !stdout.contains("\"name\": \"hidden\""),
        "private class members should be excluded when public members exist:\n{stdout}"
    );

    let out_priv = run_spid3r(
        &dir,
        &["doc", "--json", "--private", src_path.to_str().unwrap()],
    );
    assert!(out_priv.status.success());
    let priv_stdout = String::from_utf8_lossy(&out_priv.stdout);
    assert!(
        priv_stdout.contains("\"name\": \"secret_helper\""),
        "{priv_stdout}"
    );
    assert!(priv_stdout.contains("\"name\": \"reset\""), "{priv_stdout}");
    assert!(
        priv_stdout.contains("\"name\": \"hidden\""),
        "{priv_stdout}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn doc_html_writes_output_file_and_escapes_html() {
    let dir = scratch("html");
    let src_path = dir.join("lib.matrix");
    let html_path = dir.join("out/api.html");
    write_file(
        &src_path,
        "\
/// Compute `a < b & c` safely.
/// Avoid raw <script>alert(1)</script> injection.
public fn check(a: int, b: int) -> bool {
    return a < b;
}
",
    );

    let out = run_spid3r(
        &dir,
        &[
            "doc",
            src_path.to_str().unwrap(),
            "-o",
            html_path.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "spid3r doc -o failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let html = std::fs::read_to_string(&html_path).expect("html written");
    assert!(html.starts_with("<!DOCTYPE html>"), "{html}");
    assert!(
        html.contains("public fn check(a: int, b: int) -&gt; bool"),
        "{html}"
    );
    assert!(
        html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "{html}"
    );
    assert!(
        !html.contains("<script>alert(1)</script>"),
        "unescaped script tag found"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn doc_directory_walk_and_error_handling() {
    let dir = scratch("dir_walk");
    let src_dir = dir.join("src");
    let out_dir = dir.join("site/");
    write_file(
        &src_dir.join("a.matrix"),
        "/// Alpha.\npublic fn alpha() { return 1; }\n",
    );
    write_file(
        &src_dir.join("b.matrix"),
        "/// Beta.\npublic fn beta() { return 2; }\n",
    );

    let help = run_spid3r(&dir, &["doc", "--help"]);
    assert!(help.status.success());

    let missing = run_spid3r(&dir, &["doc", dir.join("nope.matrix").to_str().unwrap()]);
    assert!(!missing.status.success());

    let dir_out = run_spid3r(
        &dir,
        &[
            "doc",
            src_dir.to_str().unwrap(),
            "--out",
            out_dir.to_str().unwrap(),
        ],
    );
    assert!(
        dir_out.status.success(),
        "directory doc generation failed: {}",
        String::from_utf8_lossy(&dir_out.stderr)
    );
    let index_html = std::fs::read_to_string(dir.join("site/index.html")).unwrap();
    assert!(index_html.contains("alpha") && index_html.contains("beta"));

    let _ = std::fs::remove_dir_all(&dir);
}
