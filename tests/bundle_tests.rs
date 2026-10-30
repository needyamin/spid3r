//! Tests for `spid3r bundle` (WebAssembly, Desktop, Android targets).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spid3r-bundle-test-{}-{}",
        name,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn spid3r(args: &[&str]) -> (i32, String, String) {
    let exe = env!("CARGO_BIN_EXE_spid3r");
    let out = Command::new(exe).args(args).output().expect("run spid3r");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn bundle_wasm_directory() {
    let dir = scratch("wasm-dir");
    let app = dir.join("app.matrix");
    fs::write(&app, "fn main() { println(\"bundled wasm\"); return 0; }").unwrap();

    let out_dir = dir.join("dist_web");
    let (code, stdout, stderr) = spid3r(&[
        "bundle",
        app.to_str().unwrap(),
        "--target",
        "wasm",
        "-o",
        out_dir.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "bundle stderr: {}", stderr);
    assert!(stdout.contains("Bundle generated successfully"));

    assert!(out_dir.join("index.html").is_file());
    assert!(out_dir.join("spid3r.js").is_file());
    assert!(out_dir.join("spid3r.wasm").is_file());
    assert!(out_dir.join("app.matrix").is_file());
}

#[test]
fn bundle_wasm_single_file_html() {
    let dir = scratch("wasm-single");
    let app = dir.join("app.matrix");
    fs::write(
        &app,
        "fn main() { println(\"single file app\"); return 0; }",
    )
    .unwrap();

    let out_html = dir.join("app.html");
    let (code, stdout, stderr) = spid3r(&[
        "bundle",
        app.to_str().unwrap(),
        "--target",
        "wasm",
        "--single-file",
        "-o",
        out_html.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "bundle stderr: {}", stderr);
    assert!(stdout.contains("Bundle generated successfully"));

    assert!(out_html.is_file());
    let html_content = fs::read_to_string(&out_html).unwrap();
    assert!(html_content.contains("Spid3r Standalone Web Bundle"));
    assert!(html_content.contains("single file app"));
    assert!(html_content.contains("Spid3r.init("));
}

#[test]
fn bundle_desktop_bytecode() {
    let dir = scratch("desktop-bundle");
    let app = dir.join("desktop_app.matrix");
    fs::write(&app, "fn main() { println(\"desktop bundle\"); return 0; }").unwrap();

    let out_dir = dir.join("dist_desktop");
    let (code, stdout, stderr) = spid3r(&[
        "bundle",
        app.to_str().unwrap(),
        "--target",
        "desktop",
        "-o",
        out_dir.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "bundle stderr: {}", stderr);
    assert!(stdout.contains("Bundle generated successfully"));

    let matrixc = out_dir.join("desktop_app.matrixc");
    assert!(matrixc.is_file());

    // Execute the bundled .matrixc directly with spid3r exec
    let (exec_code, exec_out, exec_err) = spid3r(&["exec", matrixc.to_str().unwrap()]);
    assert_eq!(exec_code, 0, "exec err: {}", exec_err);
    assert!(exec_out.contains("desktop bundle"));
}

#[test]
fn bundle_android_project() {
    let dir = scratch("android-bundle");
    let app = dir.join("mobile_app.matrix");
    fs::write(&app, "fn main() { println(\"mobile android\"); return 0; }").unwrap();

    let out_dir = dir.join("dist_android");
    let (code, stdout, stderr) = spid3r(&[
        "bundle",
        app.to_str().unwrap(),
        "--target",
        "android",
        "-o",
        out_dir.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "bundle stderr: {}", stderr);
    assert!(stdout.contains("Bundle generated successfully"));

    assert!(out_dir
        .join("app")
        .join("src")
        .join("main")
        .join("AndroidManifest.xml")
        .is_file());
    assert!(out_dir
        .join("app")
        .join("src")
        .join("main")
        .join("assets")
        .join("spid3r.wasm")
        .is_file());
    assert!(out_dir
        .join("app")
        .join("src")
        .join("main")
        .join("assets")
        .join("spid3r.js")
        .is_file());
    assert!(out_dir
        .join("app")
        .join("src")
        .join("main")
        .join("assets")
        .join("index.html")
        .is_file());
    assert!(out_dir
        .join("app")
        .join("src")
        .join("main")
        .join("assets")
        .join("app.matrix")
        .is_file());
    assert!(out_dir.join("app").join("build.gradle.kts").is_file());

    let manifest = fs::read_to_string(
        out_dir
            .join("app")
            .join("src")
            .join("main")
            .join("AndroidManifest.xml"),
    )
    .unwrap();
    let gradle = fs::read_to_string(out_dir.join("app").join("build.gradle.kts")).unwrap();
    let kt = fs::read_to_string(
        out_dir
            .join("app")
            .join("src")
            .join("main")
            .join("java")
            .join("com")
            .join("spid3r")
            .join("mobileapp")
            .join("MainActivity.kt"),
    )
    .unwrap();
    assert!(manifest.contains("package=\"com.spid3r.mobileapp\""));
    assert!(gradle.contains("namespace = \"com.spid3r.mobileapp\""));
    assert!(gradle.contains("applicationId = \"com.spid3r.mobileapp\""));
    assert!(kt.contains("package com.spid3r.mobileapp"));
}

#[test]
fn bundle_android_package_id_sanitization_for_digit_and_uppercase_stem() {
    let dir = scratch("android-digit-stem");
    let app = dir.join("2048-Game.matrix");
    fs::write(&app, "fn main() { println(\"2048\"); return 0; }").unwrap();

    let out_dir = dir.join("dist_2048");
    let (code, _, stderr) = spid3r(&[
        "bundle",
        app.to_str().unwrap(),
        "--target",
        "android",
        "-o",
        out_dir.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "bundle stderr: {}", stderr);

    let manifest = fs::read_to_string(
        out_dir
            .join("app")
            .join("src")
            .join("main")
            .join("AndroidManifest.xml"),
    )
    .unwrap();
    let gradle = fs::read_to_string(out_dir.join("app").join("build.gradle.kts")).unwrap();
    let kt = fs::read_to_string(
        out_dir
            .join("app")
            .join("src")
            .join("main")
            .join("java")
            .join("com")
            .join("spid3r")
            .join("app2048game")
            .join("MainActivity.kt"),
    )
    .unwrap();
    assert!(manifest.contains("package=\"com.spid3r.app2048game\""));
    assert!(gradle.contains("namespace = \"com.spid3r.app2048game\""));
    assert!(gradle.contains("applicationId = \"com.spid3r.app2048game\""));
    assert!(kt.contains("package com.spid3r.app2048game"));
}

/// The documented invocation is `-o dist/app.html`, i.e. a path whose parent
/// directory does not exist yet. The directory targets create their output
/// directory; the single-file branch used to write straight to the path and
/// fail with "cannot find the path specified" on a fresh checkout.
#[test]
fn bundle_wasm_single_file_creates_a_missing_output_directory() {
    let dir = scratch("wasm-single-nested");
    let app = dir.join("app.matrix");
    fs::write(&app, "fn main() { println(\"nested\"); return 0; }").unwrap();

    let out_html = dir.join("dist").join("web").join("app.html");
    assert!(!out_html.parent().unwrap().exists(), "precondition");

    let (code, _stdout, stderr) = spid3r(&[
        "bundle",
        app.to_str().unwrap(),
        "--target",
        "wasm",
        "--single-file",
        "-o",
        out_html.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "bundle stderr: {}", stderr);
    assert!(out_html.is_file(), "the bundle must be written");
    let html = fs::read_to_string(&out_html).unwrap();
    assert!(html.contains("Spid3r Standalone Web Bundle"));
    let _ = fs::remove_dir_all(&dir);
}

/// A GUI bundle's launcher must not `pause`: the console only exists to show
/// startup errors while the window runs, and holding it open for a keypress
/// after the window closes leaves a stray console behind. A console app keeps
/// the pause so a double-clicked script does not vanish with its output.
#[cfg(windows)]
#[test]
fn only_a_console_bundle_pauses_the_launcher() {
    let dir = scratch("bundle-pause");
    let app = dir.join("app.matrix");
    fs::write(&app, "fn main() { return 0; }\n").unwrap();
    fs::write(dir.join("page.html"), "<html></html>").unwrap();

    let (code, _, stderr) = spid3r(&[
        "bundle",
        app.to_str().unwrap(),
        "--target",
        "desktop",
        "-o",
        dir.join("console").to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{}", stderr);
    let bat = fs::read_to_string(dir.join("console/run.bat")).unwrap();
    assert!(bat.contains("spid3r exec"), "{}", bat);
    assert!(
        bat.contains("pause"),
        "console launcher should pause: {}",
        bat
    );

    let (code, _, stderr) = spid3r(&[
        "bundle",
        app.to_str().unwrap(),
        "--target",
        "desktop",
        "--html",
        "page.html",
        "-o",
        dir.join("gui").to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{}", stderr);
    let bat = fs::read_to_string(dir.join("gui/run.bat")).unwrap();
    assert!(bat.contains("spid3r gui"), "{}", bat);
    assert!(
        !bat.contains("pause"),
        "a GUI launcher must not pause: {}",
        bat
    );
    let _ = fs::remove_dir_all(&dir);
}
