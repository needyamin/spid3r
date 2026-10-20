//! Build script for the Spid3r compiler.
//!
//! Two jobs:
//!
//! 1. **Windows installer** (`cargo build --release` on Windows): writes
//!    `Spid3r-Setup-<version>.exe` to the repository root with NSIS. See
//!    [`windows_installer`]. Set `SPID3R_NO_INSTALLER=1` to skip it.
//! 2. **LLVM linking** for the `native` (AOT backend) feature, below.
//!
//! `inkwell`/`llvm-sys` are built with `no-llvm-linking` because llvm-sys does
//! not support dynamic LLVM linking on Windows and requires `llvm-config`
//! elsewhere; instead we link the LLVM C API library ourselves:
//!
//! - Windows: `LLVM-C.lib` (import library for `LLVM-C.dll`, which the official
//!   LLVM installer ships) found via `SPID3R_LLVM_LIB_DIR`, else
//!   `LLVM_SYS_181_PREFIX`, else the default install path.
//! - Linux/macOS: `llvm-config --libdir` + `-lLLVM-N`, else Homebrew prefixes.
//!
//! On Windows we also copy `LLVM-C.dll` next to the built binaries so
//! `cargo run` / `cargo test` work without touching PATH.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// WebView2 bootstrapper acquisition: opt-in, checksum-verified, fail-closed
/// (audit A-11). Kept in its own file so `tests/artifact_hardening.rs` can
/// exercise the exact logic the build runs.
#[path = "build_support/webview2.rs"]
mod webview2;

fn main() {
    // Files this script `#[path]`-includes are build inputs too: without these
    // directives a change to the hash implementation or the WebView2 policy
    // would not rerun the build script.
    println!("cargo:rerun-if-changed=build_support/webview2.rs");
    println!("cargo:rerun-if-changed=src/pkg/sha256.rs");
    if env::var_os("CARGO_FEATURE_NATIVE").is_some() {
        native_llvm();
    }
    windows_installer();
}

/// Build `Spid3r-Setup-<version>.exe` for `cargo build --release` on Windows.
///
/// A build script runs before its own crate is compiled, so the installer's
/// `spid3r.exe` comes from a nested release build into `<target>/installer`
/// (same features, guarded against recursion by `SPID3R_INSTALLER_NESTED`).
/// NSIS is taken from `packaging/nsis/nsis/nsis-3.10/` or `PATH`; without it
/// the step is skipped with a warning.
fn windows_installer() {
    println!("cargo:rerun-if-env-changed=SPID3R_NO_INSTALLER");
    if env::var("PROFILE").as_deref() != Ok("release")
        || env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || env::var_os("SPID3R_NO_INSTALLER").is_some()
        || env::var_os("SPID3R_INSTALLER_NESTED").is_some()
    {
        return;
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    for input in [
        "src",
        "build.rs",
        "Cargo.toml",
        "Cargo.lock",
        "README.md",
        "docs",
        "examples",
        "stdlib",
        "demo_apps",
        "packaging/nsis/spid3r.nsi",
        "packaging/nsis/LICENSE.txt",
        "packaging/nsis/assets",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(input).display());
    }

    let nsis_dir = root.join("packaging").join("nsis");
    let makensis = Some(nsis_dir.join("nsis").join("nsis-3.10").join("makensis.exe"))
        .filter(|p| p.is_file())
        .or_else(|| find_on_path("makensis.exe"));
    let Some(makensis) = makensis else {
        println!(
            "cargo:warning=installer skipped: makensis.exe not found \
             (put NSIS 3.x in packaging/nsis/nsis/nsis-3.10/ or on PATH)"
        );
        return;
    };

    // <target>/<profile>/build/<pkg>-<hash>/out -> <target>
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap_or_default());
    let Some(target_root) = out_dir.ancestors().nth(4) else {
        println!("cargo:warning=installer skipped: unexpected OUT_DIR layout");
        return;
    };
    let nested_dir = target_root.join("installer");
    let triple = env::var("TARGET").unwrap_or_default();
    let gui = env::var_os("CARGO_FEATURE_GUI").is_some();
    let native = env::var_os("CARGO_FEATURE_NATIVE").is_some();
    let mut features = Vec::new();
    if gui {
        features.push("gui");
    }
    if native {
        features.push("native");
    }
    // `spid3r build` needs LLVM-C.dll to start, plus LLVM-C.lib and the
    // runtime staticlib to link programs; all three ship next to spid3r.exe.
    let mut native_files = Vec::new();
    if native {
        match (windows_llvm_dll(), windows_llvm_lib_dir()) {
            (Some(dll), Some(lib_dir)) => {
                native_files.push(("LLVM_DLL", dll));
                native_files.push(("LLVM_LIB", lib_dir.join("LLVM-C.lib")));
            }
            _ => {
                println!(
                    "cargo:warning=installer skipped: the native backend needs LLVM-C.dll \
                     and LLVM-C.lib (install LLVM 18.1+; see docs/BUILDING.md)"
                );
                return;
            }
        }
    }

    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut build = Command::new(cargo);
    build
        .current_dir(&root)
        .args([
            "build",
            "--release",
            "--bin",
            "spid3r",
            "--lib",
            "--no-default-features",
        ])
        .arg("--target")
        .arg(&triple)
        .arg("--target-dir")
        .arg(&nested_dir)
        .env("SPID3R_INSTALLER_NESTED", "1");
    if !features.is_empty() {
        build.arg("--features").arg(features.join(","));
    }
    match build.status() {
        Ok(s) if s.success() => {}
        Ok(s) => {
            println!("cargo:warning=installer skipped: nested release build failed ({s})");
            return;
        }
        Err(e) => {
            println!("cargo:warning=installer skipped: cannot run cargo: {e}");
            return;
        }
    }
    let release_dir = nested_dir.join(&triple).join("release");
    let exe = release_dir.join("spid3r.exe");
    if native {
        native_files.push(("RUNTIME_LIB", release_dir.join("spid3r_lang.lib")));
    }
    let webview2 = if gui {
        webview2_bootstrapper(&nsis_dir)
    } else {
        None
    };

    let version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let mut nsis = Command::new(&makensis);
    nsis.current_dir(&nsis_dir)
        .arg("/V2")
        .arg(format!("/DAPPVERSION={version}"))
        .arg(format!("/DWITH_GUI={}", if gui { 1 } else { 0 }))
        .arg(format!("/DWITH_NATIVE={}", if native { 1 } else { 0 }))
        .arg(format!("/DSPID3R_EXE={}", exe.display()));
    for (name, path) in &native_files {
        nsis.arg(format!("/D{name}={}", path.display()));
    }
    if let Some(setup) = &webview2 {
        nsis.arg(format!("/DWEBVIEW2_SETUP={}", setup.display()));
    }
    let output = nsis.arg("spid3r.nsi").output();
    match output {
        Ok(o) if o.status.success() => println!(
            "cargo:warning=installer written: {}",
            root.join(format!("Spid3r-Setup-{version}.exe")).display()
        ),
        Ok(o) => {
            let log = String::from_utf8_lossy(&o.stdout).to_string()
                + &String::from_utf8_lossy(&o.stderr);
            for line in log.lines().filter(|l| !l.trim().is_empty()) {
                println!("cargo:warning=makensis: {line}");
            }
            println!(
                "cargo:warning=installer skipped: makensis failed ({})",
                o.status
            );
        }
        Err(e) => println!("cargo:warning=installer skipped: cannot run makensis: {e}"),
    }
}

/// Microsoft's Evergreen WebView2 bootstrapper, bundled only when the operator
/// opts in and the download verifies (see `build_support/webview2.rs`). A plain
/// release build performs **no** network access; a partial or tampered download
/// is discarded instead of being cached as usable (audit A-11).
fn webview2_bootstrapper(nsis_dir: &std::path::Path) -> Option<PathBuf> {
    println!("cargo:rerun-if-env-changed=SPID3R_FETCH_WEBVIEW2");
    println!("cargo:rerun-if-env-changed=SPID3R_WEBVIEW2_SHA256");
    webview2::ensure(nsis_dir)
}

fn find_on_path(exe: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|dir| dir.join(exe))
        .find(|p| p.is_file())
}

fn native_llvm() {
    println!("cargo:rerun-if-env-changed=SPID3R_LLVM_LIB_DIR");
    println!("cargo:rerun-if-env-changed=LLVM_SYS_181_PREFIX");
    println!("cargo:rerun-if-env-changed=SPID3R_LLVM_BIN_DIR");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if target_os == "windows" {
        windows_llvm();
    } else {
        unix_llvm();
    }

    // Drop LLVM-C.dll next to the produced binaries (Windows) so `cargo run`,
    // `cargo test` (whose test executables live in deps/), and the installer
    // work without LLVM's bin directory on PATH.
    if target_os == "windows" {
        if let Some(dll) = windows_llvm_dll() {
            let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap_or_default());
            // <target>/<profile>/build/<pkg>-<hash>/out -> <target>/<profile>
            if let Some(profile) = out_dir.ancestors().nth(3) {
                for dir in [profile.to_path_buf(), profile.join("deps")] {
                    let _ = fs::create_dir_all(&dir);
                    let _ = fs::copy(&dll, dir.join("LLVM-C.dll"));
                }
            }
        } else {
            println!(
                "cargo:warning=LLVM-C.dll not found; spid3r.exe will need LLVM's bin \
                 directory on PATH (or set SPID3R_LLVM_BIN_DIR). See docs/BUILDING.md."
            );
        }
    }
}

/// Directory holding `LLVM-C.lib` (Windows).
fn windows_llvm_lib_dir() -> Option<PathBuf> {
    [
        env::var_os("SPID3R_LLVM_LIB_DIR").map(PathBuf::from),
        env::var_os("LLVM_SYS_181_PREFIX").map(|p| PathBuf::from(p).join("lib")),
        Some(PathBuf::from(r"C:\Program Files\LLVM\lib")),
        Some(PathBuf::from(r"C:\Program Files (x86)\LLVM\lib")),
    ]
    .into_iter()
    .flatten()
    .find(|d| d.join("LLVM-C.lib").is_file())
}

/// `LLVM-C.dll` (Windows), which a native-enabled spid3r.exe loads at startup.
fn windows_llvm_dll() -> Option<PathBuf> {
    [
        env::var_os("SPID3R_LLVM_BIN_DIR").map(PathBuf::from),
        env::var_os("LLVM_SYS_181_PREFIX").map(|p| PathBuf::from(p).join("bin")),
        Some(PathBuf::from(r"C:\Program Files\LLVM\bin")),
        Some(PathBuf::from(r"C:\Program Files (x86)\LLVM\bin")),
    ]
    .into_iter()
    .flatten()
    .map(|d| d.join("LLVM-C.dll"))
    .find(|p| p.is_file())
}

fn windows_llvm() {
    match windows_llvm_lib_dir() {
        Some(dir) => {
            let dir = dir.display().to_string();
            println!("cargo:rustc-link-search=native={dir}");
            println!("cargo:rustc-link-lib=dylib=LLVM-C");
        }
        None => {
            println!(
                "cargo:warning=LLVM 18 not found for the `native` backend. \
                 Install LLVM 18.1+ (https://github.com/llvm/llvm-project/releases) \
                 or set SPID3R_LLVM_LIB_DIR. See docs/BUILDING.md."
            );
        }
    }
}

fn unix_llvm() {
    // Prefer llvm-config when available.
    for name in ["llvm-config", "llvm-config-18"] {
        if let Ok(out) = Command::new(name).arg("--libdir").output() {
            if out.status.success() {
                let libdir = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !libdir.is_empty() {
                    println!("cargo:rustc-link-search=native={libdir}");
                    // Debian/Ubuntu ship the single libLLVM-18 dylib; Homebrew
                    // uses libLLVM. Try both by emitting the versioned name.
                    let prefix = env::var_os("LLVM_SYS_181_PREFIX")
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if prefix.contains("homebrew") || prefix.contains("Cellar") {
                        println!("cargo:rustc-link-lib=dylib=LLVM");
                    } else {
                        println!("cargo:rustc-link-lib=dylib=LLVM-18");
                        println!("cargo:rustc-link-lib=dylib=LLVM");
                    }
                    return;
                }
            }
        }
    }

    // Homebrew fallback.
    for base in [
        "/opt/homebrew/opt/llvm@18/lib",
        "/usr/local/opt/llvm@18/lib",
    ] {
        if PathBuf::from(base).is_dir() {
            println!("cargo:rustc-link-search=native={base}");
            println!("cargo:rustc-link-lib=dylib=LLVM");
            return;
        }
    }

    println!(
        "cargo:warning=LLVM 18 not found for the `native` backend. \
         Install llvm-18 development packages and llvm-config, or set \
         LLVM_SYS_181_PREFIX. See docs/BUILDING.md."
    );
}
