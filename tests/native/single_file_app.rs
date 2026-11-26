//! End-to-end tests for the single-file `.exe` pipeline.
//!
//! The pipeline is: `spid3r compile` → `.matrixc`, then `spid3r build --embed`
//! / `--bytecode` / `--bundle-runtime` appends a payload to a native
//! executable, and `spid3r gui --payload` runs that payload from memory.
//!
//! Everything here drives the real binary. A GUI window cannot be opened
//! headlessly, so the payload execution tests use `--dry-run`, which runs the
//! program, replays the bridge and prints the JavaScript the program queued —
//! the same code path minus `window::open`.
//!
//! # Feature gate
//!
//! The whole file requires **both** `native` and `gui`, so it compiles to an
//! empty test binary on the other CI legs:
//!
//! * without `native` there is no `spid3r build`, which every fixture needs;
//! * without `gui` the checker rejects the fixture's `gui_title` / `gui_set_html`
//!   calls ("unknown name"), because the GUI natives are only registered when
//!   that feature is on.
//!
//! Gating per test was not enough: a `--features native` build has `build` but
//! no GUI natives, so the fixture failed at *compile* time before any test
//! could decide to skip.
#![cfg(all(feature = "native", feature = "gui"))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spid3r-single-file-{}-{}",
        name,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Run the toolchain from `cwd` with a clean, deterministic environment.
fn run_in(cwd: &Path, args: &[&str]) -> Run {
    let out = Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .env_remove("SPID3R_PATH")
        .env_remove("SPID3R_RUN_PAYLOAD_DEBUG")
        .output()
        .expect("spawn spid3r");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    }
}

fn run(args: &[&str]) -> Run {
    run_in(&std::env::temp_dir(), args)
}

/// Is this toolchain able to produce native executables at all?
fn native_backend_available() -> bool {
    // A build without the `native` feature answers `build --help` with a usage
    // error instead of the help text.
    let help = run(&["build", "--help"]);
    help.code == 0 && help.stdout.contains("--bundle-runtime")
}

/// A native build needs LLVM; report whether this machine can do one.
///
/// Only a *missing toolchain* is a reason to skip. Every other failure is a
/// real regression, so it must fail the test: returning `false` for any
/// non-zero exit turned every test in this file into a no-op that still
/// reported success.
fn can_link_native(dir: &Path) -> bool {
    if !native_backend_available() {
        eprintln!("skipping: this build has no native backend");
        return false;
    }
    let probe = dir.join("probe.matrix");
    fs::write(&probe, "fn main() { return 0; }\n").expect("write probe");
    let out = run(&[
        "build",
        probe.to_str().unwrap(),
        "-o",
        dir.join("probe.exe").to_str().unwrap(),
    ]);
    if out.code == 0 {
        return true;
    }
    if ["LLVM", "linker", "MSVC"]
        .iter()
        .any(|needle| out.stderr.contains(needle))
    {
        eprintln!(
            "skipping: native backend unavailable here (exit {})\n{}",
            out.code, out.stderr
        );
        return false;
    }
    panic!(
        "`spid3r build` failed for a reason that is not a missing toolchain \
         (exit {}):\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.code, out.stdout, out.stderr
    );
}

// --------------------------------------------------------------- fixtures

const APP_SRC: &str = r#"fn main() {
    gui_title("Pipeline Test");
    gui_set_html("<h1 id='t'>hello</h1>");
    gui_on("ping", fn (event, payload) {
        println("ping: " + payload);
        gui_eval("document.getElementById('t').textContent = 'pong'");
        return null;
    });
    gui_emit("ping", "from-main");
    return 0;
}
"#;

const LAUNCHER_SRC: &str = "fn main() {\n    return run_payload();\n}\n";

const INDEX_HTML: &str = "<!doctype html><html><body><div id=\"spid3r-root\"></div></body></html>";

/// A GUI app plus its `ui/` assets and a compiled `.matrixc`, laid out the way
/// `docs/GUI.md` describes.
struct App {
    dir: PathBuf,
    source: PathBuf,
    bytecode: PathBuf,
}

fn make_app(name: &str) -> Option<App> {
    let dir = scratch(name);
    if !can_link_native(&dir) {
        return None;
    }
    fs::create_dir_all(dir.join("ui")).expect("ui dir");
    let source = dir.join("app.matrix");
    fs::write(&source, APP_SRC).expect("write app");
    fs::write(dir.join("ui/index.html"), INDEX_HTML).expect("write index");
    fs::write(dir.join("ui/app.js"), "window.spid3r.emit('ping','page');").expect("write js");
    let bytecode = dir.join("app.matrixc");
    let out = run(&[
        "compile",
        source.to_str().unwrap(),
        "-o",
        bytecode.to_str().unwrap(),
    ]);
    assert_eq!(out.code, 0, "compile failed: {}", out.stderr);
    Some(App {
        dir,
        source,
        bytecode,
    })
}

// ------------------------------------------------------------ PE payloads

/// Minimal PE import-table reader (test-side, no dependencies).
///
/// Windows-only: its single caller is `#[cfg(windows)]`, and leaving it
/// unconditional makes it dead code on the other CI legs, where clippy runs
/// with `-D warnings`.
#[cfg(windows)]
fn pe_imports(bytes: &[u8]) -> Vec<String> {
    fn u16_at(d: &[u8], o: usize) -> usize {
        u16::from_le_bytes([d[o], d[o + 1]]) as usize
    }
    fn u32_at(d: &[u8], o: usize) -> usize {
        u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]) as usize
    }
    if bytes.len() < 0x40 || &bytes[..2] != b"MZ" {
        return Vec::new();
    }
    let pe = u32_at(bytes, 0x3c);
    if pe + 24 > bytes.len() || &bytes[pe..pe + 4] != b"PE\0\0" {
        return Vec::new();
    }
    let nsec = u16_at(bytes, pe + 6);
    let opt = pe + 24;
    let magic = u16_at(bytes, opt);
    let dd = opt + if magic == 0x20b { 112 } else { 96 };
    let imp_rva = u32_at(bytes, dd + 8);
    let sec_off = opt + u16_at(bytes, pe + 20);
    let mut secs = Vec::new();
    for i in 0..nsec {
        let o = sec_off + i * 40;
        if o + 40 > bytes.len() {
            return Vec::new();
        }
        secs.push((
            u32_at(bytes, o + 12),
            u32_at(bytes, o + 8),
            u32_at(bytes, o + 20),
            u32_at(bytes, o + 16),
        ));
    }
    let rva_to_off = |rva: usize| -> Option<usize> {
        for &(va, vsz, raw, rsz) in &secs {
            if rva >= va && rva < va + vsz.max(rsz) {
                return Some(raw + (rva - va));
            }
        }
        None
    };
    let Some(mut off) = rva_to_off(imp_rva) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    loop {
        if off + 20 > bytes.len() || bytes[off..off + 20].iter().all(|b| *b == 0) {
            break;
        }
        let name_rva = u32_at(bytes, off + 12);
        match rva_to_off(name_rva) {
            Some(n) => {
                let end = bytes[n..].iter().position(|b| *b == 0).unwrap_or(0) + n;
                names.push(String::from_utf8_lossy(&bytes[n..end]).to_string());
            }
            None => break,
        }
        off += 20;
    }
    names
}

/// Read the payload entry names straight out of the file, independently of the
/// production reader, so a format regression is visible here.
fn payload_entry_names(exe: &Path) -> Option<Vec<String>> {
    const MAGIC: &[u8; 16] = b"SPID3R-PAYLOAD01";
    let bytes = fs::read(exe).ok()?;
    if bytes.len() < 24 {
        return None;
    }
    let trailer = &bytes[bytes.len() - 24..];
    if &trailer[..16] != MAGIC {
        return None;
    }
    let body_len = u64::from_le_bytes(trailer[16..24].try_into().ok()?) as usize;
    let start = bytes.len().checked_sub(24 + body_len)?;
    let mut body = bytes[start..bytes.len() - 24].to_vec();
    for (i, b) in body.iter_mut().enumerate() {
        *b ^= b"sp1d3r-embed-key"[i % 16] ^ ((i >> 8) as u8);
    }
    let mut names = Vec::new();
    let mut at = 0usize;
    while at + 4 <= body.len() {
        let n = u32::from_le_bytes(body[at..at + 4].try_into().ok()?) as usize;
        at += 4;
        if at + n + 8 > body.len() {
            return None;
        }
        names.push(String::from_utf8_lossy(&body[at..at + n]).to_string());
        at += n;
        let len = u64::from_le_bytes(body[at..at + 8].try_into().ok()?) as usize;
        at += 8 + len;
    }
    Some(names)
}

// ------------------------------------------------------------------ tests

/// `build --bytecode --embed ui` must produce one file that carries the
/// compiled program and the UI, and must not carry the program's source or
/// string literals in the native image.
#[test]
fn bytecode_build_carries_program_and_assets_but_not_source() {
    let Some(app) = make_app("bytecode-payload") else {
        return;
    };
    let exe = app.dir.join("App.exe");
    let out = run(&[
        "build",
        app.source.to_str().unwrap(),
        "-o",
        exe.to_str().unwrap(),
        "--subsystem",
        "windows",
        "--bytecode",
        app.bytecode.to_str().unwrap(),
        "--embed",
        "ui",
    ]);
    assert_eq!(out.code, 0, "build failed: {}", out.stderr);

    let names = payload_entry_names(&exe).expect("payload trailer");
    assert!(names.contains(&"app.matrixc".to_string()), "{:?}", names);
    assert!(names.contains(&"ui/index.html".to_string()), "{:?}", names);
    assert!(names.contains(&"ui/app.js".to_string()), "{:?}", names);

    // The source never reaches the image: the payload is obfuscated and the
    // launcher is compiled from a stub, so neither the app's text nor its
    // string literals appear verbatim. (`gui_set_html` is not a valid probe:
    // the native registry in the runtime staticlib names every GUI builtin.)
    let bytes = fs::read(&exe).expect("read exe");
    let contains = |needle: &str| bytes.windows(needle.len()).any(|w| w == needle.as_bytes());
    assert!(!contains("from-main"), "app literal leaked into the image");
    assert!(
        !contains("<h1 id='t'>hello</h1>"),
        "app source leaked into the image"
    );
    let _ = fs::remove_dir_all(&app.dir);
}

/// The payload is what `gui --payload` runs. `--dry-run` exercises the whole
/// load → compile/VM → bridge path without opening a window.
#[test]
fn gui_payload_runs_embedded_bytecode() {
    let Some(app) = make_app("payload-run") else {
        return;
    };
    let exe = app.dir.join("App.exe");
    let built = run(&[
        "build",
        app.source.to_str().unwrap(),
        "-o",
        exe.to_str().unwrap(),
        "--bytecode",
        app.bytecode.to_str().unwrap(),
        "--embed",
        "ui",
    ]);
    assert_eq!(built.code, 0, "build failed: {}", built.stderr);

    let out = run(&["gui", "--payload", exe.to_str().unwrap(), "--dry-run"]);
    assert_eq!(out.code, 0, "dry-run failed: {}", out.stderr);
    assert!(out.stdout.contains("ping: from-main"), "{}", out.stdout);
    assert!(
        out.stdout
            .contains("document.getElementById('t').textContent = 'pong'"),
        "{}",
        out.stdout
    );
    let _ = fs::remove_dir_all(&app.dir);
}

/// A single-file app that ships **source** must resolve stdlib imports on a
/// machine that has no `stdlib/` directory — the extracted runtime lives alone
/// in the user cache there. This failed with "module 'semver' not found"
/// before the stdlib table was compiled into every build.
#[test]
fn source_payload_resolves_stdlib_without_a_stdlib_directory() {
    let Some(app) = make_app("stdlib-clean") else {
        return;
    };
    fs::write(
        &app.source,
        "import \"semver\";\n\nfn main() {\n    let v = parse(\"1.2.3\");\n    gui_set_html(\"<b>\" + string(v[\"major\"]) + \"</b>\");\n    return 0;\n}\n",
    )
    .expect("write app");
    fs::write(app.dir.join("launcher.matrix"), LAUNCHER_SRC).expect("write launcher");
    let exe = app.dir.join("Src.exe");
    let built = run(&[
        "build",
        app.dir.join("launcher.matrix").to_str().unwrap(),
        "-o",
        exe.to_str().unwrap(),
        "--embed",
        "app.matrix",
        "--embed",
        "ui",
    ]);
    assert_eq!(built.code, 0, "build failed: {}", built.stderr);

    // A toolchain copy with nothing beside it: no stdlib/, no repo ancestors.
    let isolated = scratch("stdlib-clean-runtime");
    let runtime = isolated.join(if cfg!(windows) {
        "spid3r.exe"
    } else {
        "spid3r"
    });
    fs::copy(bin(), &runtime).expect("copy toolchain");
    let out = run_in(
        &isolated,
        &["gui", "--payload", exe.to_str().unwrap(), "--dry-run"],
    );
    assert_eq!(out.code, 0, "clean-machine run failed: {}", out.stderr);
    assert!(out.stdout.contains("<b>1</b>"), "{}", out.stdout);
    let _ = fs::remove_dir_all(&app.dir);
    let _ = fs::remove_dir_all(&isolated);
}

/// A truncated payload must be reported, not crash the runtime.
#[test]
fn a_truncated_payload_is_a_clean_error() {
    let Some(app) = make_app("truncated") else {
        return;
    };
    let exe = app.dir.join("App.exe");
    let built = run(&[
        "build",
        app.source.to_str().unwrap(),
        "-o",
        exe.to_str().unwrap(),
        "--bytecode",
        app.bytecode.to_str().unwrap(),
        "--embed",
        "ui",
    ]);
    assert_eq!(built.code, 0, "build failed: {}", built.stderr);

    // Cut into the payload body but keep the trailer intact, so the length
    // fields no longer agree with the file.
    let mut bytes = fs::read(&exe).expect("read");
    let cut = bytes.len() - 24 - 8;
    bytes.drain(cut..bytes.len() - 24);
    let broken = app.dir.join("Broken.exe");
    fs::write(&broken, &bytes).expect("write broken");

    let out = run(&["gui", "--payload", broken.to_str().unwrap(), "--dry-run"]);
    assert_ne!(out.code, 0, "a corrupt payload must fail");
    assert!(
        out.stderr.contains("payload") || out.stderr.contains("truncated"),
        "stderr: {}",
        out.stderr
    );
    assert!(!out.stderr.contains("panicked"), "{}", out.stderr);
    let _ = fs::remove_dir_all(&app.dir);
}

/// The app that ships must not depend on build-machine DLLs: `LLVM-C.dll` is
/// delay-loaded (only `spid3r build` uses it) and the C runtime is static.
///
/// Both link configurations are checked. `--release` alone proves nothing:
/// `-opt:ref` dead-stripped the LLVM import even before it was delay-loaded, so
/// a release-only test passed with or without the fix. The **debug** build is
/// the one that used to import `LLVM-C.dll` and die with
/// `0xC0000135 STATUS_DLL_NOT_FOUND` on a machine without LLVM.
#[test]
#[cfg(windows)]
fn built_app_imports_only_system_dlls() {
    let Some(app) = make_app("imports") else {
        return;
    };
    for release in [true, false] {
        let exe = app.dir.join(match release {
            true => "App-release.exe",
            false => "App-debug.exe",
        });
        let mut args = vec![
            "build",
            app.source.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ];
        if release {
            args.push("--release");
        }
        args.extend([
            "--bytecode",
            app.bytecode.to_str().unwrap(),
            "--embed",
            "ui",
        ]);
        let built = run(&args);
        assert_eq!(
            built.code, 0,
            "build failed (release: {:?}): {}",
            release, built.stderr
        );

        let imports = pe_imports(&fs::read(&exe).expect("read exe"));
        assert!(
            !imports.is_empty(),
            "could not read the import table (release: {:?})",
            release
        );
        for forbidden in ["LLVM-C.dll", "VCRUNTIME140.dll", "VCRUNTIME140_1.dll"] {
            assert!(
                !imports.iter().any(|d| d.eq_ignore_ascii_case(forbidden)),
                "'{}' must not be imported (release: {:?}; imports: {:?})",
                forbidden,
                release,
                imports
            );
        }
    }
    let _ = fs::remove_dir_all(&app.dir);
}

/// Build-time validation: mistakes that used to surface at first launch (where
/// a windows-subsystem app cannot show them) are rejected by `build`.
#[test]
fn build_rejects_payloads_that_cannot_work() {
    let Some(app) = make_app("validation") else {
        return;
    };
    let out_exe = app.dir.join("Out.exe");
    let src = app.source.to_str().unwrap();
    let out = out_exe.to_str().unwrap();

    let dup = run(&[
        "build",
        src,
        "-o",
        out,
        "--allow-gui",
        "--embed",
        "ui",
        "--embed",
        "ui",
    ]);
    assert_ne!(dup.code, 0);
    assert!(dup.stderr.contains("two entries"), "{}", dup.stderr);

    let no_program = run(&[
        "build",
        src,
        "-o",
        out,
        "--allow-gui",
        "--embed",
        "ui",
        "--bundle-runtime",
    ]);
    assert_ne!(no_program.code, 0);
    assert!(
        no_program.stderr.contains("no program"),
        "{}",
        no_program.stderr
    );

    let junk = app.dir.join("junk.matrixc");
    fs::write(&junk, b"this is not bytecode").expect("write junk");
    let bad_bc = run(&[
        "build",
        src,
        "-o",
        out,
        "--allow-gui",
        "--bytecode",
        junk.to_str().unwrap(),
    ]);
    assert_ne!(bad_bc.code, 0);
    assert!(
        bad_bc.stderr.contains("not valid Spid3r bytecode"),
        "{}",
        bad_bc.stderr
    );

    let missing_icon = run(&[
        "build",
        src,
        "-o",
        out,
        "--allow-gui",
        "--icon",
        app.dir.join("nope.ico").to_str().unwrap(),
    ]);
    assert_ne!(missing_icon.code, 0);
    assert!(
        missing_icon.stderr.contains("not a file"),
        "{}",
        missing_icon.stderr
    );
    let _ = fs::remove_dir_all(&app.dir);
}

/// `-o app` gains the platform's executable suffix instead of writing a file
/// Windows will not run.
#[test]
fn extensionless_output_gets_the_exe_suffix() {
    let Some(app) = make_app("extension") else {
        return;
    };
    let stem = app.dir.join("runme");
    let out = run(&[
        "build",
        app.source.to_str().unwrap(),
        "-o",
        stem.to_str().unwrap(),
        "--allow-gui",
    ]);
    assert_eq!(out.code, 0, "build failed: {}", out.stderr);
    let expected = if cfg!(windows) {
        app.dir.join("runme.exe")
    } else {
        app.dir.join("runme")
    };
    assert!(expected.is_file(), "expected {}", expected.display());
    let _ = fs::remove_dir_all(&app.dir);
}

/// `gui --payload` and a source file are mutually exclusive; the old behaviour
/// silently ignored the file.
///
#[test]
fn payload_and_source_file_are_rejected_together() {
    let dir = scratch("payload-conflict");
    let src = dir.join("app.matrix");
    fs::write(&src, "fn main() { return 0; }\n").expect("write");
    let out = run(&[
        "gui",
        src.to_str().unwrap(),
        "--payload",
        src.to_str().unwrap(),
        "--dry-run",
    ]);
    assert_ne!(out.code, 0);
    assert!(out.stderr.contains("mutually exclusive"), "{}", out.stderr);
    let _ = fs::remove_dir_all(&dir);
}

/// `--html` against a payload names an entry inside it. An unknown name used to
/// be used as the page anyway, which served 404 for every request.
#[test]
fn payload_html_must_exist() {
    let Some(app) = make_app("html-entry") else {
        return;
    };
    let exe = app.dir.join("App.exe");
    let built = run(&[
        "build",
        app.source.to_str().unwrap(),
        "-o",
        exe.to_str().unwrap(),
        "--bytecode",
        app.bytecode.to_str().unwrap(),
        "--embed",
        "ui",
    ]);
    assert_eq!(built.code, 0, "build failed: {}", built.stderr);

    let bad = run(&[
        "gui",
        "--payload",
        exe.to_str().unwrap(),
        "--html",
        "does-not-exist.html",
        "--dry-run",
    ]);
    assert_ne!(bad.code, 0);
    assert!(bad.stderr.contains("is not an entry in"), "{}", bad.stderr);
    assert!(
        bad.stderr.contains("ui/index.html"),
        "the message should list what the payload has: {}",
        bad.stderr
    );

    // A page that exists on disk is not an entry either: the payload is the
    // app, and a page picked up from the working directory is not part of it.
    let on_disk = app.dir.join("side.html");
    fs::write(&on_disk, "<html></html>").expect("write side page");
    let disk = run(&[
        "gui",
        "--payload",
        exe.to_str().unwrap(),
        "--html",
        on_disk.to_str().unwrap(),
        "--dry-run",
    ]);
    assert_ne!(
        disk.code, 0,
        "a file on disk must not stand in for a payload entry"
    );
    assert!(
        disk.stderr.contains("is not an entry in"),
        "{}",
        disk.stderr
    );

    // A real entry works.
    let ok = run(&[
        "gui",
        "--payload",
        exe.to_str().unwrap(),
        "--html",
        "ui/index.html",
        "--dry-run",
    ]);
    assert_eq!(ok.code, 0, "{}", ok.stderr);
    let _ = fs::remove_dir_all(&app.dir);
}

/// Script arguments belong to the program, not to the CLI: `--html` and its
/// value used to arrive in `args()`.
///
#[test]
fn gui_flags_do_not_leak_into_script_args() {
    let dir = scratch("script-args");
    let src = dir.join("args.matrix");
    fs::write(
        &src,
        "fn main() {\n    println(\"args: \" + string(args()));\n    return 0;\n}\n",
    )
    .expect("write");
    fs::write(dir.join("page.html"), "<html></html>").expect("write page");

    let out = run(&[
        "gui",
        src.to_str().unwrap(),
        "--html",
        "page.html",
        "--dry-run",
        "alpha",
        "beta",
    ]);
    assert_eq!(out.code, 0, "gui failed: {}", out.stderr);
    let combined = format!("{}{}", out.stdout, out.stderr);
    // Positive assertion: both positionals must actually arrive. Asserting
    // only the absence of flags also passes when `args()` is empty.
    assert!(
        combined.contains("alpha") && combined.contains("beta"),
        "positional arguments must reach args(): {}",
        combined
    );
    assert!(
        !combined.contains("--html") && !combined.contains("page.html"),
        "flags leaked into args(): {}",
        combined
    );
    let _ = fs::remove_dir_all(&dir);
}

/// `gui --payload App.exe alpha beta`: positionals after `--payload` are the
/// program's arguments. They used to be taken as the *source file*, so the
/// mutual-exclusion check rejected the command outright — `--payload App.exe
/// alpha` could not be expressed at all, even though the help text documents
/// it.
#[test]
fn payload_positionals_reach_the_program() {
    let Some(app) = make_app("payload-args") else {
        return;
    };
    fs::write(
        &app.source,
        "fn main() {\n    let a = args();\n    println(\"n=\" + string(len(a)));\n    \
         if len(a) > 0 { println(\"0=\" + a[0]); }\n    if len(a) > 1 { println(\"1=\" + a[1]); }\n    \
         return 0;\n}\n",
    )
    .expect("write app");
    let exe = app.dir.join("App.exe");
    let built = run(&[
        "build",
        app.source.to_str().unwrap(),
        "-o",
        exe.to_str().unwrap(),
        "--embed",
        "app.matrix",
        "--embed",
        "ui",
    ]);
    assert_eq!(built.code, 0, "build failed: {}", built.stderr);

    let out = run(&[
        "gui",
        "--payload",
        exe.to_str().unwrap(),
        "--dry-run",
        "alpha",
        "beta",
    ]);
    assert_eq!(
        out.code, 0,
        "gui --payload with arguments must succeed: {}",
        out.stderr
    );
    assert!(out.stdout.contains("n=2"), "args() was: {}", out.stdout);
    assert!(out.stdout.contains("0=alpha"), "args() was: {}", out.stdout);
    assert!(out.stdout.contains("1=beta"), "args() was: {}", out.stdout);
    let _ = fs::remove_dir_all(&app.dir);
}
