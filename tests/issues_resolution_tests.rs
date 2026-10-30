use std::path::PathBuf;
use std::process::Command;

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

// The core CI job runs `cargo test --no-default-features`; tests that need
// the AOT backend (`spid3r build` via LLVM) or the GUI feature (`gui_*`
// natives, the gui subcommand) are gated so the suite compiles and passes
// under every feature combination.
#[cfg(all(feature = "native", feature = "gui"))]
#[test]
fn test_b1_spid3r_build_rejects_gui_natives() {
    let dir = std::env::temp_dir();
    let src = dir.join("test_b1_gui.matrix");
    std::fs::write(
        &src,
        "fn main() {\n    gui_title(\"Test\");\n    return 0;\n}\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run(Command::new(bin()).args(["build", src.to_str().unwrap()]));
    assert_ne!(
        code, 0,
        "expected build to fail for gui natives, stdout: {stdout}"
    );
    assert!(
        stderr.contains("program uses GUI natives; the AOT backend does not link the WebView bridge — run it with `spid3r gui`"),
        "stderr was: {stderr}"
    );
    let _ = std::fs::remove_file(src);
}

#[cfg(feature = "gui")]
#[test]
fn test_b2_desktop_bundle_gui() {
    let dir = std::env::temp_dir().join("test_b2_bundle");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let src = dir.join("app.matrix");
    let ui_dir = dir.join("ui");
    std::fs::create_dir_all(&ui_dir).unwrap();
    let html_file = ui_dir.join("index.html");
    std::fs::write(&html_file, "<h1>Hello GUI</h1>").unwrap();
    std::fs::write(
        &src,
        "fn main() {\n    gui_title(\"App\");\n    return 0;\n}\n",
    )
    .unwrap();

    let out_dir = dir.join("dist");
    let (code, stdout, stderr) = run(Command::new(bin()).args([
        "bundle",
        src.to_str().unwrap(),
        "--target",
        "desktop",
        "--html",
        html_file.to_str().unwrap(),
        "-o",
        out_dir.to_str().unwrap(),
    ]));
    assert_eq!(code, 0, "bundle failed: {stderr}\n{stdout}");

    // Should contain source matrix file
    assert!(
        out_dir.join("app.matrix").is_file(),
        "source .matrix not copied"
    );
    // Should preserve ui/ asset structure
    assert!(
        out_dir.join("ui").join("index.html").is_file() || out_dir.join("index.html").is_file(),
        "html asset missing"
    );

    #[cfg(target_os = "windows")]
    {
        let launcher = out_dir.join("run.bat");
        assert!(launcher.is_file(), "run.bat not generated");
        let content = std::fs::read_to_string(&launcher).unwrap();
        assert!(
            content.contains("spid3r gui") && content.contains("app.matrix"),
            "launcher missing spid3r gui: {content}"
        );
        assert!(
            content.contains("--html"),
            "launcher missing --html: {content}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "native")]
#[test]
fn test_b3_pe_resource_embedding() {
    #[cfg(target_os = "windows")]
    {
        // 1. Check spid3r.exe itself has PE version info
        let spid3r_bin = bin();
        let out = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "[System.Diagnostics.FileVersionInfo]::GetVersionInfo('{}').ProductName",
                    spid3r_bin.display()
                ),
            ])
            .output()
            .expect("inspect spid3r.exe");
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(name, "Spid3r", "spid3r.exe missing ProductName metadata");

        // 2. Build custom binary with --icon, --product-name, --version
        let dir = std::env::temp_dir().join("test_b3_res");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let src = dir.join("main.matrix");
        std::fs::write(
            &src,
            "fn main() {\n    println(\"custom-app-ran\");\n    return 0;\n}\n",
        )
        .unwrap();
        let out_exe = dir.join("custom_app.exe");

        let (code, stdout, stderr) = run(Command::new(bin()).args([
            "build",
            src.to_str().unwrap(),
            "--icon",
            "packaging/nsis/assets/spid3r.ico",
            "--product-name",
            "CustomProduct",
            "--version",
            "3.4.5.6",
            "-o",
            out_exe.to_str().unwrap(),
        ]));
        assert_eq!(
            code, 0,
            "spid3r build with metadata failed: {stderr}\n{stdout}"
        );
        assert!(out_exe.is_file(), "output executable was not produced");

        let out = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "[System.Diagnostics.FileVersionInfo]::GetVersionInfo('{}') | Select-Object -ExpandProperty ProductName",
                    out_exe.display()
                ),
            ])
            .output()
            .expect("inspect custom_app.exe");
        let prod = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(prod, "CustomProduct", "built exe has wrong ProductName");

        let (run_code, run_stdout, run_stderr) = run(&mut Command::new(&out_exe));
        assert_eq!(run_code, 0, "running built exe failed: {run_stderr}");
        assert!(
            run_stdout.contains("custom-app-ran"),
            "stdout was: {run_stdout}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn test_e1_explicit_main_warning() {
    let dir = std::env::temp_dir();
    let src = dir.join("test_e1_main.matrix");
    std::fs::write(
        &src,
        "fn main() {\n    println(\"hi\");\n    return 0;\n}\nmain();\n",
    )
    .unwrap();

    let (code, _stdout, stderr) = run(Command::new(bin()).args(["run", src.to_str().unwrap()]));
    assert_eq!(code, 0, "run failed: {stderr}");
    assert!(
        stderr
            .contains("explicit call to 'main': main() is invoked automatically; remove this call"),
        "expected unreachable code lint on main(), got: {stderr}"
    );
    let _ = std::fs::remove_file(src);
}

#[test]
fn test_e2_ternary_operator_error() {
    let dir = std::env::temp_dir();
    let src = dir.join("test_e2_ternary.matrix");
    std::fs::write(
        &src,
        "fn main() {\n    let x = true ? 1 : 2;\n    return 0;\n}\n",
    )
    .unwrap();

    let (code, _stdout, stderr) = run(Command::new(bin()).args(["run", src.to_str().unwrap()]));
    assert_ne!(code, 0, "expected ternary to fail");
    assert!(
        stderr.contains("the conditional operator is not supported; use an if statement"),
        "stderr was: {stderr}"
    );
    let _ = std::fs::remove_file(src);
}

#[test]
fn test_e3_catch_without_parens() {
    let dir = std::env::temp_dir();
    let src = dir.join("test_e3_catch.matrix");
    std::fs::write(
        &src,
        "fn main() {\n    try {\n        throw \"expected-error\";\n    } catch err {\n        println(\"caught: \" + err);\n    }\n    return 0;\n}\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run(Command::new(bin()).args(["run", src.to_str().unwrap()]));
    assert_eq!(code, 0, "run failed: {stderr}");
    assert!(
        stdout.contains("caught: expected-error"),
        "stdout was: {stdout}"
    );
    let _ = std::fs::remove_file(src);
}

#[test]
fn test_e4_module_unexported_symbol_diagnostic() {
    let dir = std::env::temp_dir().join("test_e4_unexported");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let m_file = dir.join("helper.matrix");
    std::fs::write(&m_file, "fn secret_calc() {\n    return 42;\n}\n").unwrap();

    let app_file = dir.join("main.matrix");
    std::fs::write(
        &app_file,
        "import \"./helper\";\nfn main() {\n    println(secret_calc());\n    return 0;\n}\n",
    )
    .unwrap();

    let (code, _stdout, stderr) =
        run(Command::new(bin()).args(["run", app_file.to_str().unwrap()]));
    assert_ne!(code, 0, "expected unexported access to fail");
    assert!(
        stderr.contains("secret_calc is declared in")
            && stderr.contains("helper.matrix")
            && stderr.contains("not exported — mark it `public"),
        "stderr was: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_e5_subcommand_help() {
    for cmd in ["run", "build", "gui", "bundle", "test", "compile"] {
        let (code, stdout, stderr) = run(Command::new(bin()).args([cmd, "--help"]));
        assert_eq!(code, 0, "command '{cmd} --help' failed: {stderr}");
        assert!(
            stdout.to_lowercase().contains("usage:") && stdout.contains("spid3r"),
            "help output missing Usage: {stdout}"
        );

        let (code_short, stdout_short, stderr_short) = run(Command::new(bin()).args([cmd, "-h"]));
        assert_eq!(code_short, 0, "command '{cmd} -h' failed: {stderr_short}");
        assert!(
            stdout_short.to_lowercase().contains("usage:") && stdout_short.contains("spid3r"),
            "help output missing Usage: {stdout_short}"
        );
    }
}

#[test]
fn test_d2_result_len_error() {
    let dir = std::env::temp_dir();
    let src = dir.join("test_d2_len.matrix");
    std::fs::write(
        &src,
        "fn main() {\n    let res = ok(\"test\");\n    let n = len(res);\n    return 0;\n}\n",
    )
    .unwrap();

    let (code, _stdout, stderr) = run(Command::new(bin()).args(["run", src.to_str().unwrap()]));
    assert_ne!(code, 0, "expected len(result) to fail");
    assert!(
        stderr.contains("value is a result; call .unwrap() or handle the error"),
        "stderr was: {stderr}"
    );
    let _ = std::fs::remove_file(src);
}

#[test]
fn test_d3_remove_file() {
    let dir = std::env::temp_dir();
    let test_file = dir.join("test_d3_remove.txt");
    std::fs::write(&test_file, "delete me").unwrap();
    let path_str = test_file.to_str().unwrap().replace('\\', "/");

    let src = dir.join("test_d3_script.matrix");
    std::fs::write(
        &src,
        format!(
            "fn main() {{\n    let res = remove_file(\"{path_str}\");\n    println(\"ok: \" + string(is_ok(res)));\n    return 0;\n}}\n"
        ),
    )
    .unwrap();

    let (code, stdout, stderr) = run(Command::new(bin()).args(["run", src.to_str().unwrap()]));
    assert_eq!(code, 0, "run failed: {stderr}");
    assert!(stdout.contains("ok: true"), "stdout was: {stdout}");
    assert!(!test_file.exists(), "file was not removed");

    let _ = std::fs::remove_file(src);
}

#[test]
fn test_d4_is_ok_is_err() {
    let dir = std::env::temp_dir();
    let src = dir.join("test_d4_builtins.matrix");
    std::fs::write(
        &src,
        r#"
fn main() {
    let o = ok(42);
    let e = err("fail");
    println("o ok: " + string(is_ok(o)));
    println("o err: " + string(is_err(o)));
    println("e ok: " + string(is_ok(e)));
    println("e err: " + string(is_err(e)));
    return 0;
}
"#,
    )
    .unwrap();

    let (code, stdout, stderr) = run(Command::new(bin()).args(["run", src.to_str().unwrap()]));
    assert_eq!(code, 0, "run failed: {stderr}");
    assert!(stdout.contains("o ok: true"), "stdout was: {stdout}");
    assert!(stdout.contains("o err: false"), "stdout was: {stdout}");
    assert!(stdout.contains("e ok: false"), "stdout was: {stdout}");
    assert!(stdout.contains("e err: true"), "stdout was: {stdout}");

    let _ = std::fs::remove_file(src);
}

#[cfg(feature = "gui")]
#[test]
fn test_p1_gui_args() {
    let dir = std::env::temp_dir();
    let src = dir.join("test_p1_gui_args.matrix");
    std::fs::write(
        &src,
        r#"
fn main() {
    let a = args();
    println("gui args: " + string(a));
    return 0;
}
"#,
    )
    .unwrap();

    let (code, stdout, stderr) = run(Command::new(bin()).args([
        "gui",
        src.to_str().unwrap(),
        "--html",
        "index.html",
        "--dry-run",
        "alpha",
        "beta",
    ]));
    assert_eq!(code, 0, "gui run failed: {stderr}");
    // `args()` carries the program's own arguments — the positionals after the
    // source file — not the CLI's. `--html`/`index.html` used to be appended
    // because the flag list was filtered by value instead of parsed, so every
    // GUI program saw the toolchain's flags as its arguments.
    assert!(
        stdout.contains("alpha") && stdout.contains("beta"),
        "positional arguments must reach the program: {stdout}"
    );
    assert!(
        !stdout.contains("--html") && !stdout.contains("index.html"),
        "CLI flags leaked into args(): {stdout}"
    );

    let _ = std::fs::remove_file(src);
}

#[cfg(feature = "gui")]
#[test]
fn test_p3_default_page_html_hint() {
    assert!(
        spid3r_lang::gui::DEFAULT_PAGE.contains("spid3r gui app.matrix --html page.html"),
        "DEFAULT_PAGE missing --html guidance"
    );
}

#[test]
fn test_e2_ternary_lookahead_no_false_positive_with_named_arg() {
    let dir = std::env::temp_dir();
    let src = dir.join("test_e2_named_arg_propagate.matrix");
    std::fs::write(
        &src,
        r#"
fn get_ok() {
    return ok(42);
}

fn helper(a, x = 0) {
    return a + x;
}

fn main() {
    let res = helper(get_ok()?, x: 100);
    println("val: " + string(res));
    return 0;
}
"#,
    )
    .unwrap();

    let (code, stdout, stderr) = run(Command::new(bin()).args(["run", src.to_str().unwrap()]));
    assert_eq!(code, 0, "run failed: {stderr}");
    assert!(stdout.contains("val: 142"), "stdout was: {stdout}");
    let _ = std::fs::remove_file(src);
}

#[cfg(all(feature = "native", feature = "gui"))]
#[test]
fn test_b1_nested_gui_native_in_closure_rejected() {
    let dir = std::env::temp_dir();
    let src = dir.join("test_b1_closure_gui.matrix");
    std::fs::write(
        &src,
        "fn main() {\n    let f = fn() { gui_title(\"Nested\"); };\n    return 0;\n}\n",
    )
    .unwrap();

    let (code, stdout, stderr) = run(Command::new(bin()).args(["build", src.to_str().unwrap()]));
    assert_ne!(
        code, 0,
        "expected build to reject nested GUI native, stdout: {stdout}"
    );
    assert!(
        stderr.contains("program uses GUI natives; the AOT backend does not link the WebView bridge — run it with `spid3r gui`"),
        "stderr was: {stderr}"
    );
    let _ = std::fs::remove_file(src);
}

#[test]
fn test_elapsed_ms_and_gui_args_aliases() {
    let dir = std::env::temp_dir();
    let src = dir.join("test_aliases.matrix");
    std::fs::write(
        &src,
        r#"
fn main() {
    let t = elapsed_ms();
    println("t is int: " + string(type_of(t) == "int"));
    let a = gui_args();
    println("a is list: " + string(type_of(a) == "list"));
    return 0;
}
"#,
    )
    .unwrap();

    let (code, stdout, stderr) = run(Command::new(bin()).args(["run", src.to_str().unwrap()]));
    assert_eq!(code, 0, "run failed: {stderr}");
    assert!(stdout.contains("t is int: true"), "stdout was: {stdout}");
    assert!(stdout.contains("a is list: true"), "stdout was: {stdout}");
    let _ = std::fs::remove_file(src);
}
