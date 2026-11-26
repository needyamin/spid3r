//! R9.4 (--freestanding + --entry) & R9.5 (--target + --sysroot + --emit-obj + .sha256).

#![cfg(feature = "native")]

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    for key in ["CARGO_BIN_EXE_spid3r", "CARGO_BIN_EXE_spidr"] {
        if let Ok(p) = std::env::var(key) {
            let p = PathBuf::from(p);
            if p.is_file() {
                return p;
            }
        }
    }
    let exe = std::env::current_exe().expect("test executable path");
    let profile_dir = exe
        .parent()
        .and_then(|deps| deps.parent())
        .expect("profile directory");
    for stem in ["spid3r", "spidr"] {
        let cand = profile_dir.join(format!("{}{}", stem, std::env::consts::EXE_SUFFIX));
        if cand.is_file() {
            return cand;
        }
    }
    panic!("spid3r binary not found next to {}", profile_dir.display());
}

#[test]
fn r9_4_freestanding_example_builds_and_runs_on_host() {
    let dir = std::env::temp_dir().join(format!("spid3r_fs_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let exe = dir.join(format!("hello_fs{}", std::env::consts::EXE_SUFFIX));
    let build = Command::new(bin())
        .args([
            "build",
            "--freestanding",
            "examples/freestanding/hello.matrix",
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("spawn spid3r build");
    assert_eq!(
        build.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&exe).output().expect("run freestanding");
    assert_eq!(run.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim_end(),
        "freestanding:42"
    );

    // Custom --entry + --emit-obj produces an object + .sha256 sidecar.
    let obj = dir.join("custom_entry.o");
    let emit = Command::new(bin())
        .args([
            "build",
            "--freestanding",
            "--entry",
            "kernel_main",
            "--emit-obj",
            "examples/freestanding/hello.matrix",
            "-o",
            obj.to_str().unwrap(),
        ])
        .output()
        .expect("spawn spid3r build --emit-obj");
    assert_eq!(
        emit.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&emit.stderr)
    );
    assert!(obj.is_file());
    let sidecar = dir.join("custom_entry.o.sha256");
    assert!(sidecar.is_file(), "missing .sha256 sidecar");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn r9_5_cross_target_emit_and_sysroot_validation() {
    let dir = std::env::temp_dir().join(format!("spid3r_cross_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");

    // Linking for non-host target without --sysroot fails closed cleanly (exit 2).
    let no_sr = Command::new(bin())
        .args([
            "build",
            "--target",
            "aarch64-unknown-linux-gnu",
            "examples/freestanding/hello.matrix",
            "-o",
            dir.join("out_arm").to_str().unwrap(),
        ])
        .output()
        .expect("spawn spid3r");
    assert_eq!(no_sr.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&no_sr.stderr).contains("--sysroot"),
        "stderr: {}",
        String::from_utf8_lossy(&no_sr.stderr)
    );

    // Cross-target object emission for a non-host OS/ABI + SEC-B .sha256.
    let (cross_triple, expected_magic): (&str, &[u8]) = if cfg!(windows) {
        ("x86_64-unknown-linux-gnu", b"\x7fELF")
    } else if cfg!(target_os = "macos") {
        ("aarch64-unknown-linux-gnu", b"\x7fELF")
    } else {
        ("x86_64-apple-darwin", b"\xcf\xfa\xed\xfe")
    };
    let cross_obj = dir.join("hello_cross.o");
    let cross = Command::new(bin())
        .args([
            "build",
            "--target",
            cross_triple,
            "--emit-obj",
            "examples/freestanding/hello.matrix",
            "-o",
            cross_obj.to_str().unwrap(),
        ])
        .output()
        .expect("spawn spid3r cross emit-obj");
    assert_eq!(
        cross.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&cross.stderr)
    );
    let bytes = std::fs::read(&cross_obj).expect("read cross obj");
    assert_eq!(
        &bytes[0..expected_magic.len()],
        expected_magic,
        "expected {cross_triple} magic"
    );
    let sha_text = std::fs::read_to_string(dir.join("hello_cross.o.sha256")).expect("sha256");
    let expected_hex = spid3r_lang::pkg::sha256_hex(&bytes);
    assert!(
        sha_text.starts_with(&expected_hex),
        "sha256 mismatch: {sha_text} vs {expected_hex}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
