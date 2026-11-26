//! Regression tests for the 2026-10-26 bug report (bug.html from the study
//! planner session): B1-B4 language/stdlib/bundler defects and C-01..C-03
//! CLI papercuts. Every test mirrors an acceptance assertion from the report.

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run(cmd: &mut Command) -> (i32, String, String) {
    let o = cmd.output().expect("spawn failed");
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r_bugfix_{}", name));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------- B1

/// `now()`, `now_ms()`, and `elapsed_ms()` must share one process-wide
/// monotonic origin, and `now()` called 100 ms after the first monotonic call
/// must report those 100 ms instead of restarting from 0.
#[test]
fn b1_monotonic_clocks_share_one_origin() {
    let dir = scratch("b1_clocks");
    let src = dir.join("clocks.matrix");
    std::fs::write(
        &src,
        r#"fn main() {
    let a = now();
    sleep_ms(120);
    let b = now();
    assert(b - a >= 100 and b - a < 5000);
    let c = now_ms();
    assert(c - a >= 100);
    // Two back-to-back reads of the shared origin: on a throttled CI runner
    // even consecutive statements can take a while, so the bound is loose.
    assert((now_ms() - now()) <= 50);
    assert((elapsed_ms() - now()) <= 50);
    println("clocks-ok");
    return 0;
}
"#,
    )
    .unwrap();
    let (code, stdout, stderr) = run(Command::new(bin()).arg("run").arg(&src));
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("clocks-ok"), "stdout: {stdout}");
}

/// The wall clock is untouched: `unix_time()` is a real epoch timestamp.
#[test]
fn b1_wall_clock_still_epoch() {
    let dir = scratch("b1_unix");
    let src = dir.join("unix.matrix");
    std::fs::write(
        &src,
        r#"fn main() {
    assert(unix_time() > 1700000000);
    assert(datetime_parts(unix_time())["year"] >= 2024);
    println("unix-ok");
    return 0;
}
"#,
    )
    .unwrap();
    let (code, stdout, stderr) = run(Command::new(bin()).arg("run").arg(&src));
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("unix-ok"), "stdout: {stdout}");
}

// ---------------------------------------------------------------- B2

/// `rename`, `copy_file`, and `write_file_atomic` exist and follow the
/// result contract from the report's acceptance block.
#[test]
fn b2_rename_copy_atomic() {
    let dir = scratch("b2_files");
    let src = dir.join("files.matrix");
    std::fs::write(
        &src,
        r#"fn main() {
    write_file("t.tmp", "x").unwrap();
    rename("t.tmp", "t.txt").unwrap();
    assert(file_exists("t.tmp") == false);
    assert(file_exists("t.txt") == true);

    // replace an existing target
    write_file("t2.tmp", "new").unwrap();
    rename("t2.tmp", "t.txt").unwrap();
    assert(read_file("t.txt").unwrap() == "new");

    // failure is an err, not a throw
    let r = rename("missing_file.tmp", "x.tmp");
    assert(type_of(r) == "result");
    assert(is_err(r));

    copy_file("t.txt", "t_copy.txt").unwrap();
    assert(read_file("t_copy.txt").unwrap() == "new");
    let c = copy_file("missing_file.tmp", "y.tmp");
    assert(is_err(c));

    write_file_atomic("atomic.txt", "saved").unwrap();
    assert(read_file("atomic.txt").unwrap() == "saved");
    println("files-ok");
    return 0;
}
"#,
    )
    .unwrap();
    let (code, stdout, stderr) = run(Command::new(bin()).arg("run").arg(&src).current_dir(&dir));
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("files-ok"), "stdout: {stdout}");
}

// ---------------------------------------------------------------- B3

/// A `gui_on` handler that reads neither parameter must not produce
/// unused-parameter warnings under `spid3r check`.
///
/// `gui_on` is a GUI native: without the `gui` feature the name is unknown
/// and the check legitimately fails, so these tests only exist when the
/// feature is on (the CI core job runs `cargo test --no-default-features`).
#[cfg(feature = "gui")]
#[test]
fn b3_gui_on_handler_lint_clean() {
    let dir = scratch("b3_gui");
    let src = dir.join("app.matrix");
    std::fs::write(
        &src,
        r#"fn main() {
    gui_on("ready", fn(event, payload) {
        return null;
    });
    return 0;
}
"#,
    )
    .unwrap();
    let (code, stdout, stderr) = run(Command::new(bin()).arg("check").arg(&src));
    assert_eq!(code, 0, "stdout: {stdout} stderr: {stderr}");
    assert!(
        !stderr.contains("unused parameter"),
        "lint fired on gui_on handler: {stderr}"
    );
}

/// The `_`-prefix escape hatch keeps working (report acceptance test).
#[cfg(feature = "gui")]
#[test]
fn b3_gui_on_underscore_prefix_still_clean() {
    let dir = scratch("b3_gui2");
    let src = dir.join("app.matrix");
    std::fs::write(
        &src,
        r#"fn main() {
    gui_on("ready", fn(_event, _payload) {
        return null;
    });
    return 0;
}
"#,
    )
    .unwrap();
    let (code, _stdout, stderr) = run(Command::new(bin()).arg("check").arg(&src));
    assert_eq!(code, 0, "stderr: {stderr}");
}

/// The lint itself survives elsewhere: a plain closure still warns.
#[test]
fn b3_plain_closure_still_warns() {
    let dir = scratch("b3_plain");
    let src = dir.join("app.matrix");
    std::fs::write(
        &src,
        "fn main() {\n    let f = fn(a, b) {\n        return 1;\n    };\n    let _g = f;\n    return 0;\n}\n",
    )
    .unwrap();
    let (code, _stdout, stderr) = run(Command::new(bin()).arg("check").arg(&src));
    assert_eq!(code, 0);
    assert!(
        stderr.contains("unused parameter 'a'"),
        "expected the plain-closure lint to stay: {stderr}"
    );
}

// ---------------------------------------------------------------- B4

/// `--self-contained` embeds the runtime executable in a desktop bundle and
/// points the launcher at the local copy.
#[test]
fn b4_self_contained_bundle_embeds_runtime() {
    let dir = scratch("b4_bundle");
    let src = dir.join("app.matrix");
    std::fs::write(
        &src,
        "fn main() {\n    println(\"hi\");\n    return 0;\n}\n",
    )
    .unwrap();
    let out = dir.join("bundle_out");
    let (code, _stdout, stderr) = run(Command::new(bin()).args([
        "bundle",
        src.to_str().unwrap(),
        "--target",
        "desktop",
        "-o",
        out.to_str().unwrap(),
        "--self-contained",
    ]));
    assert_eq!(code, 0, "stderr: {stderr}");
    if cfg!(target_os = "windows") {
        assert!(out.join("spid3r.exe").is_file(), "runtime not embedded");
    } else {
        assert!(out.join("spid3r").is_file(), "runtime not embedded");
    }
    // The bundler writes run.bat on Windows and run.sh elsewhere (B4 fix),
    // and the launcher must point at the embedded local copy.
    let local_prefix = if cfg!(target_os = "windows") {
        ".\\spid3r"
    } else {
        "./spid3r"
    };
    let launcher = if cfg!(target_os = "windows") {
        std::fs::read_to_string(out.join("run.bat")).unwrap()
    } else {
        std::fs::read_to_string(out.join("run.sh")).unwrap()
    };
    assert!(
        launcher.contains(local_prefix) && launcher.contains("spid3r exec"),
        "launcher must target the embedded runtime: {launcher}"
    );
}

/// Default bundles stay light: no runtime copied without the flag.
#[test]
fn b4_default_bundle_stays_light() {
    let dir = scratch("b4_light");
    let src = dir.join("app.matrix");
    std::fs::write(
        &src,
        "fn main() {\n    println(\"hi\");\n    return 0;\n}\n",
    )
    .unwrap();
    let out = dir.join("bundle_out");
    let (code, _stdout, stderr) = run(Command::new(bin()).args([
        "bundle",
        src.to_str().unwrap(),
        "--target",
        "desktop",
        "-o",
        out.to_str().unwrap(),
    ]));
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        !out.join("spid3r.exe").exists() && !out.join("spid3r").exists(),
        "runtime must not be embedded without --self-contained"
    );
}

// ---------------------------------------------------------------- C-01

/// `check --json` emits machine-readable stdout only: nothing at all for a
/// clean file, one JSON object per diagnostic otherwise.
#[test]
fn c01_check_json_is_parseable() {
    let dir = scratch("c01_json");

    let clean = dir.join("clean.matrix");
    std::fs::write(
        &clean,
        "fn main() {\n    println(\"ok\");\n    return 0;\n}\n",
    )
    .unwrap();
    let (code, stdout, _stderr) =
        run(Command::new(bin()).args(["check", clean.to_str().unwrap(), "--json"]));
    assert_eq!(code, 0);
    assert_eq!(
        stdout.trim(),
        "",
        "clean file must print no summary under --json: {stdout}"
    );

    let warn = dir.join("warn.matrix");
    std::fs::write(
        &warn,
        "fn main() {\n    let unused = 1;\n    return 0;\n}\n",
    )
    .unwrap();
    let (code, stdout, _stderr) =
        run(Command::new(bin()).args(["check", warn.to_str().unwrap(), "--json"]));
    assert_eq!(code, 0);
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        assert!(line.trim_start().starts_with('{'), "not JSON: {line}");
    }

    let err = dir.join("err.matrix");
    std::fs::write(
        &err,
        "fn main() {\n    let x: int = \"str\";\n    return 0;\n}\n",
    )
    .unwrap();
    let (code, stdout, _stderr) =
        run(Command::new(bin()).args(["check", err.to_str().unwrap(), "--json"]));
    assert_ne!(code, 0);
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        assert!(line.trim_start().starts_with('{'), "not JSON: {line}");
    }
}

// ---------------------------------------------------------------- C-02

/// C-02: the terminator stays required (the language requires it), but the
/// diagnostic names the real problem against the typed line — and a
/// null-valued statement echoes nothing.
#[test]
fn c02_repl_semicolon_and_null_echo() {
    let mut child = Command::new(bin())
        .arg("repl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn repl");
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"let a = 2 + 3\nlet a = 2 + 3;\nprintln(\"a = \" + string(a));\nprintln(\"side effect\")\n2 + 3\n:quit\n")
        .unwrap();
    let o = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&o.stdout).to_string();
    let stderr = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(
        stderr.contains("missing ';' at end of statement"),
        "the missing-terminator diagnostic must name the real problem: {stderr}"
    );
    assert!(
        !stderr.contains("found end of file"),
        "the end-of-file phrasing must not leak into the repl: {stderr}"
    );
    assert!(
        stdout.contains("a = 5"),
        "the retried binding must survive: {stdout}"
    );
    assert!(stdout.contains('5'), "bare expr echo: {stdout}");
    assert!(
        !stdout.lines().any(|l| l.trim() == "null"),
        "null echo suppressed (println's return value): {stdout}"
    );
}

// ---------------------------------------------------------------- C-03

/// A single-file wasm bundle written to an extensionless `-o` path gains the
/// `.html` extension, so the OS opens it in a browser.
#[test]
fn c03_single_file_bundle_gets_html_extension() {
    let dir = scratch("c03_ext");
    let src = dir.join("app.matrix");
    std::fs::write(
        &src,
        "fn main() {\n    println(\"hi\");\n    return 0;\n}\n",
    )
    .unwrap();
    let (code, _stdout, stderr) = run(Command::new(bin()).args([
        "bundle",
        src.to_str().unwrap(),
        "--target",
        "wasm",
        "--single-file",
        "-o",
        dir.join("out1").to_str().unwrap(),
    ]));
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(dir.join("out1.html").is_file(), "expected out1.html");
}

// ================================================================ Round 2 (bug2.html)
// ---------------------------------------------------------------- B-1

#[test]
fn round2_b1_target_specific_bundle_flags_rejected() {
    let dir = scratch("r2_b1_bundle_flags");
    let src = dir.join("app.matrix");
    std::fs::write(&src, "fn main() {\n    return 0;\n}\n").unwrap();

    // 1. --single-file with desktop target must be rejected with exit code 2
    let (code, _stdout, stderr) = run(Command::new(bin()).args([
        "bundle",
        src.to_str().unwrap(),
        "--target",
        "desktop",
        "--single-file",
        "-o",
        dir.join("d1").to_str().unwrap(),
    ]));
    assert_eq!(code, 2, "expected exit 2, got {code}; stderr: {stderr}");
    assert!(
        stderr.contains("--single-file applies only to --target wasm"),
        "stderr was: {stderr}"
    );

    // 2. --self-contained with wasm target must be rejected with exit code 2
    let (code, _stdout, stderr) = run(Command::new(bin()).args([
        "bundle",
        src.to_str().unwrap(),
        "--target",
        "wasm",
        "--self-contained",
        "-o",
        dir.join("w1").to_str().unwrap(),
    ]));
    assert_eq!(code, 2, "expected exit 2, got {code}; stderr: {stderr}");
    assert!(
        stderr.contains("--self-contained applies only to --target desktop"),
        "stderr was: {stderr}"
    );
}

// ---------------------------------------------------------------- B-2

#[test]
fn round2_b2_fmt_rejects_unparseable_file_and_leaves_file_untouched() {
    let dir = scratch("r2_b2_fmt");
    let src = dir.join("broken.matrix");
    let broken_content = "fn main() {\n    let x = ;\n    return 0;\n}\n";
    std::fs::write(&src, broken_content).unwrap();

    let (code, stdout, stderr) = run(Command::new(bin()).args(["fmt", src.to_str().unwrap()]));
    assert_eq!(
        code, 1,
        "fmt on unparseable file must exit 1; stdout: {stdout}, stderr: {stderr}"
    );
    assert!(
        stderr.contains("unexpected ';'") || stderr.contains("error[parser]"),
        "expected parser error on stderr, got: {stderr}"
    );

    let content_after = std::fs::read_to_string(&src).unwrap();
    assert_eq!(
        content_after, broken_content,
        "fmt must not rewrite or touch an unparseable file"
    );
}

// ---------------------------------------------------------------- B-3

#[test]
fn round2_b3_release_flag_build() {
    let (code, stdout, stderr) = run(Command::new(bin()).args(["build", "--help"]));
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("--release"), "help missing --release");

    #[cfg(feature = "native")]
    {
        let dir = scratch("r2_b3_build");
        let src = dir.join("bench.matrix");
        std::fs::write(
            &src,
            "fn main() {\n    println(\"ok\");\n    return 0;\n}\n",
        )
        .unwrap();
        let out_debug = dir.join(if cfg!(windows) { "bd.exe" } else { "bd" });
        let out_release = dir.join(if cfg!(windows) { "br.exe" } else { "br" });

        let (code_d, stdout_d, stderr_d) = run(Command::new(bin()).args([
            "build",
            src.to_str().unwrap(),
            "-o",
            out_debug.to_str().unwrap(),
        ]));
        let (code_r, stdout_r, _stderr_r) = run(Command::new(bin()).args([
            "build",
            src.to_str().unwrap(),
            "--release",
            "-o",
            out_release.to_str().unwrap(),
        ]));
        if code_d == 0 && code_r == 0 {
            assert!(
                stdout_d.contains("debug"),
                "debug build stdout was: {stdout_d}"
            );
            assert!(
                stdout_r.contains("release"),
                "release build stdout was: {stdout_r}"
            );
            let bytes_d = std::fs::read(&out_debug).unwrap();
            let bytes_r = std::fs::read(&out_release).unwrap();
            assert_ne!(
                bytes_d, bytes_r,
                "debug and release binaries should differ in size/content"
            );
        } else {
            assert!(
                stderr_d.contains("no MSVC linker")
                    || stderr_d.contains("cannot locate the LLVM shared library")
                    || stderr_d.contains("no Windows SDK found")
                    || stderr_d.contains("no 'cc' linker found")
                    || stderr_d.contains("no 'clang' linker found")
                    || stderr_d.contains("cannot find the Spid3r runtime library"),
                "unexpected failure: {stderr_d}"
            );
        }
    }
}

// ---------------------------------------------------------------- B-4

#[test]
fn round2_b4_lsp_document_symbol_function_range_and_selection_range() {
    use std::io::{BufRead, BufReader, Read, Write};
    let mut child = Command::new(bin())
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn lsp");
    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let send = |w: &mut std::process::ChildStdin, body: &str| {
        write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        w.flush().unwrap();
    };

    // Read on a thread so the waits below are bounded. A server that stops
    // answering must fail this test in seconds instead of blocking the suite
    // until the CI job's timeout (which reports no failing test at all).
    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
    std::thread::spawn(move || loop {
        let frame = (|| -> Result<String, String> {
            let mut len = 0usize;
            loop {
                let mut line = String::new();
                let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
                if n == 0 {
                    return Err("server closed stdout".to_string());
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    break;
                }
                if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
                    len = rest
                        .trim()
                        .parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?;
                }
            }
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf).map_err(|e| e.to_string())?;
            String::from_utf8(buf).map_err(|e| e.to_string())
        })();
        let stop = frame.is_err();
        if tx.send(frame).is_err() || stop {
            return;
        }
    });

    let read_response = |rx: &std::sync::mpsc::Receiver<Result<String, String>>| -> String {
        match rx.recv_timeout(std::time::Duration::from_secs(20)) {
            Ok(Ok(body)) => body,
            Ok(Err(e)) => panic!("lsp read failed: {e}"),
            Err(e) => panic!("lsp sent no response within 20s: {e}"),
        }
    };

    // 1. initialize
    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#,
    );
    let _init_resp = read_response(&rx);

    // 2. didOpen
    let uri = "file:///test.matrix";
    let src = "let x = 1;\n\nfn main() {\n    let y = 2;\n    return 0;\n}\n";
    let open_msg = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{}","languageId":"spid3r","version":1,"text":{:?}}}}}}}"#,
        uri, src
    );
    send(&mut stdin, &open_msg);
    // didOpen triggers publishDiagnostics notification
    let _diag_notif = read_response(&rx);

    // 3. documentSymbol
    let doc_sym_req = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"textDocument/documentSymbol","params":{{"textDocument":{{"uri":"{}"}}}}}}"#,
        uri
    );
    send(&mut stdin, &doc_sym_req);
    let sym_resp = read_response(&rx);

    // Assert main symbol range and selectionRange
    assert!(sym_resp.contains(r#""name":"main""#));
    assert!(sym_resp.contains(r#""kind":12"#));
    // range: 2:0 -> 5:1 (the whole function)
    assert!(
        sym_resp.contains(
            r#""range":{"start":{"line":2,"character":0},"end":{"line":5,"character":1}}"#
        ),
        "symbol response was: {sym_resp}"
    );
    // selectionRange: 2:3 -> 2:7 (the identifier main)
    assert!(
        sym_resp.contains(
            r#""selectionRange":{"start":{"line":2,"character":3},"end":{"line":2,"character":7}}"#
        ),
        "symbol response was: {sym_resp}"
    );

    // Assert top-level let x symbol range does NOT swallow fn main
    assert!(sym_resp.contains(r#""name":"x""#));
    assert!(
        sym_resp.contains(
            r#""range":{"start":{"line":0,"character":0},"end":{"line":0,"character":10}}"#
        ),
        "symbol response for x range was: {sym_resp}"
    );
    assert!(
        sym_resp.contains(
            r#""selectionRange":{"start":{"line":0,"character":4},"end":{"line":0,"character":5}}"#
        ),
        "symbol response for x selectionRange was: {sym_resp}"
    );

    let _ = child.kill();
    let _ = child.wait();
}

// ---------------------------------------------------------------- B-5

#[test]
fn round2_b5_bundle_help_text_no_standalone_default() {
    let (code, stdout, stderr) = run(Command::new(bin()).args(["bundle", "--help"]));
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(
        stdout.contains("Package into a distribution (add --self-contained to embed the runtime)"),
        "help text missing updated description: {stdout}"
    );
    assert!(
        !stdout.contains("Package into a standalone distribution"),
        "help text must not call the default standalone: {stdout}"
    );
}
