//! Build script for the Spid3r compiler.
//!
//! Only active when the `native` (LLVM AOT backend) feature is enabled.
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

fn main() {
    if env::var_os("CARGO_FEATURE_NATIVE").is_none() {
        return;
    }
    println!("cargo:rerun-if-env-changed=SPID3R_LLVM_LIB_DIR");
    println!("cargo:rerun-if-env-changed=LLVM_SYS_181_PREFIX");
    println!("cargo:rerun-if-env-changed=SPID3R_LLVM_BIN_DIR");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if target_os == "windows" {
        windows_llvm();
    } else {
        unix_llvm();
    }

    // Best effort: drop LLVM-C.dll next to the produced binaries (Windows).
    if target_os == "windows" {
        if let Some(bin_dir) = env::var_os("SPID3R_LLVM_BIN_DIR") {
            let dll = PathBuf::from(&bin_dir).join("LLVM-C.dll");
            if dll.is_file() {
                let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap_or_default());
                // target/<profile>/
                if let Some(profile) = out_dir.ancestors().nth(3) {
                    let dst = profile.join("LLVM-C.dll");
                    let _ = fs::copy(&dll, &dst);
                }
            }
        }
    }
}

fn windows_llvm() {
    let candidates: Vec<PathBuf> = [
        env::var_os("SPID3R_LLVM_LIB_DIR").map(PathBuf::from),
        env::var_os("LLVM_SYS_181_PREFIX").map(|p| PathBuf::from(p).join("lib")),
        Some(PathBuf::from(r"C:\Program Files\LLVM\lib")),
        Some(PathBuf::from(r"C:\Program Files (x86)\LLVM\lib")),
    ]
    .into_iter()
    .flatten()
    .collect();

    let lib_dir = candidates
        .iter()
        .find(|d| d.join("LLVM-C.lib").is_file())
        .cloned();

    match lib_dir {
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
    for base in ["/opt/homebrew/opt/llvm@18/lib", "/usr/local/opt/llvm@18/lib"] {
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
