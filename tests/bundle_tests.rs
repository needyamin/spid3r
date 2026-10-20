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
}
