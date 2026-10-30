//! Packaging surface of `spid3r build`.
//!
//! Three things are covered here, all of them ways a program can ship as a
//! desktop application:
//!
//! 1. `--subsystem windows` must produce a binary Windows will start *without*
//!    a terminal window. That is a property of the PE header, so it is asserted
//!    by reading the header rather than by eyeballing a screen.
//! 2. `--allow-gui` must let a launcher-style program (one that mentions GUI
//!    natives but only calls them under `spid3r gui`) build, while the default
//!    stays a hard error so a program cannot silently compile into a binary
//!    whose GUI calls are null.
//! 3. `spawn_process(cmd, args, map("hidden", true))` must keep working and must
//!    reject malformed options rather than guessing.

#![cfg(feature = "native")]

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

/// A fresh scratch directory per test, removed by the caller.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r_pkg_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn write_program(dir: &Path, name: &str, src: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, src).expect("write program");
    path
}

/// `spid3r build app.matrix -o app.exe <extra…>`; returns (success, output).
fn build(dir: &Path, extra: &[&str]) -> (bool, String) {
    let mut cmd = Command::new(bin());
    cmd.current_dir(dir)
        .arg("build")
        .arg(dir.join("app.matrix"))
        .arg("-o")
        .arg(dir.join("app.exe"));
    for a in extra {
        cmd.arg(a);
    }
    let out = cmd.output().expect("run spid3r build");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

const PLAIN: &str = "fn main() {\n    return 0;\n}\n";

/// A launcher-shaped program: GUI natives exist only inside functions, which is
/// how the desktop apps in `examples/gui` are written.
#[cfg(feature = "gui")]
const LAUNCHER: &str = "\
fn gui_linked() {
    return type_of(gui_title) == \"function\";
}

fn main() {
    if not gui_linked() {
        return 0;
    }
    gui_title(\"unused in an AOT build\");
    return 0;
}
";

#[cfg(windows)]
fn pe_subsystem(path: &Path) -> u16 {
    let bytes = std::fs::read(path).expect("read exe");
    assert_eq!(&bytes[0..2], b"MZ", "not a PE file");
    let pe = u32::from_le_bytes(bytes[0x3C..0x40].try_into().unwrap()) as usize;
    assert_eq!(
        &bytes[pe..pe + 4],
        b"PE\0\0",
        "missing PE signature at {:#x}",
        pe
    );
    u16::from_le_bytes(bytes[pe + 24 + 68..pe + 24 + 70].try_into().unwrap())
}

#[cfg(windows)]
#[test]
fn subsystem_flag_controls_the_pe_header() {
    let dir = scratch("subsystem");
    write_program(&dir, "app.matrix", PLAIN);

    let (ok, log) = build(&dir, &[]);
    assert!(ok, "default build failed: {log}");
    assert_eq!(
        pe_subsystem(&dir.join("app.exe")),
        3,
        "the default must stay a console binary (backwards compatibility)"
    );

    let (ok, log) = build(&dir, &["--subsystem", "windows"]);
    assert!(ok, "windows-subsystem build failed: {log}");
    assert_eq!(
        pe_subsystem(&dir.join("app.exe")),
        2,
        "--subsystem windows must mark the PE as a GUI binary"
    );

    // The entry point is still `main`, so the binary must run.
    let status = Command::new(dir.join("app.exe"))
        .status()
        .expect("run windows-subsystem exe");
    assert_eq!(status.code(), Some(0), "windows-subsystem exe must exit 0");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn subsystem_flag_rejects_unknown_kinds() {
    let dir = scratch("subsystem_bad");
    write_program(&dir, "app.matrix", PLAIN);
    let (ok, log) = build(&dir, &["--subsystem", "tty"]);
    assert!(!ok, "an unknown subsystem must be a usage error");
    assert!(
        log.contains("--subsystem must be 'console' or 'windows'"),
        "unexpected message: {log}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Needs a toolchain built with `gui`, because that is what makes `gui_title`
/// a known name in the first place; without it the semantic check rejects the
/// program before `build` ever sees it.
#[cfg(feature = "gui")]
#[test]
fn gui_natives_need_an_explicit_opt_in() {
    let dir = scratch("allow_gui");
    write_program(&dir, "app.matrix", LAUNCHER);

    let (ok, log) = build(&dir, &[]);
    assert!(
        !ok,
        "a program with GUI natives must not build silently: {log}"
    );
    assert!(
        log.contains("does not link the WebView bridge"),
        "unexpected message: {log}"
    );
    assert!(
        log.contains("--allow-gui"),
        "the error must point at the escape hatch: {log}"
    );

    let (ok, log) = build(&dir, &["--allow-gui"]);
    assert!(ok, "--allow-gui must build a launcher: {log}");
    assert!(
        log.contains("resolve to null"),
        "an accepted build must warn that the natives are null: {log}"
    );

    let status = Command::new(dir.join("app.exe"))
        .status()
        .expect("run launcher");
    assert_eq!(
        status.code(),
        Some(0),
        "the launcher path must run without touching the GUI natives"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn exe_path_names_the_running_binary() {
    // A launcher resolves its shipped files relative to its own executable, not
    // to the working directory (which differs when started from a shortcut).
    let dir = scratch("exe_path");
    write_program(
        &dir,
        "app.matrix",
        "fn main() {\n    println(exe_path().unwrap());\n    return 0;\n}\n",
    );
    let (ok, log) = build(&dir, &[]);
    assert!(ok, "build failed: {log}");

    let exe = dir.join("app.exe");
    let out = Command::new(&exe)
        .current_dir(std::env::temp_dir())
        .output()
        .expect("run built exe");
    assert!(
        out.status.success(),
        "run failed: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let printed = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let expected = std::fs::canonicalize(&exe).expect("canonical exe");
    let actual = std::fs::canonicalize(&printed).expect("printed path must exist");
    assert_eq!(
        actual, expected,
        "exe_path must name the running binary, got {printed}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spawn_process_accepts_a_hidden_option() {
    let dir = scratch("spawn_hidden");
    // `hidden` only changes window creation on Windows; the call itself must
    // work everywhere, and a bad option must be reported, not ignored.
    let (program, argv) = if cfg!(windows) {
        ("cmd", r#"["/c", "exit", "0"]"#)
    } else {
        ("sh", r#"["-c", "exit 0"]"#)
    };
    let src = format!(
        "\
fn main() {{
    let a = spawn_process(\"{program}\", {argv}, map(\"hidden\", true)).unwrap();
    if a == 0 {{ println(\"hidden-ok\"); }}
    let b = spawn_process(\"{program}\", {argv}).unwrap();
    if b == 0 {{ println(\"plain-ok\"); }}
    try {{
        spawn_process(\"{program}\", {argv}, map(\"hidden\", \"yes\")).unwrap();
        println(\"accepted-bad-option\");
    }} catch (err) {{
        println(\"rejected-bad-option\");
    }}
    return 0;
}}
"
    );
    write_program(&dir, "app.matrix", &src);

    let out = Command::new(bin())
        .current_dir(&dir)
        .args(["run", "app.matrix"])
        .output()
        .expect("run program");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "run failed: {stdout}");
    assert!(
        stdout.contains("hidden-ok"),
        "hidden spawn must work: {stdout}"
    );
    assert!(
        stdout.contains("plain-ok"),
        "2-arg form must still work: {stdout}"
    );
    assert!(
        stdout.contains("rejected-bad-option"),
        "a non-bool 'hidden' must be rejected: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
