//! WebView2 bootstrapper acquisition for the Windows installer (audit A-11).
//!
//! Policy:
//! * **No network access by default.** A plain `cargo build --release` never
//!   downloads anything; the fetch is opt-in via `SPID3R_FETCH_WEBVIEW2=1`.
//! * **Verify before use.** The download is written to a `.part` file, then its
//!   size and SHA-256 are checked against the pinned values (override the hash
//!   with `SPID3R_WEBVIEW2_SHA256` when Microsoft rotates the bootstrapper).
//!   Only a verified file is renamed into place.
//! * **Fail closed.** A partial, truncated, or tampered download is deleted and
//!   the installer is built without the bootstrapper — never with a file that
//!   merely has a non-zero length (the pre-fix behavior).
//!
//! This file is `#[path]`-included by `build.rs` and by
//! `tests/artifact_hardening.rs`, so the decision logic under test is exactly
//! the logic the build runs.

#[path = "../src/pkg/sha256.rs"]
mod sha256;

use std::path::{Path, PathBuf};
use std::process::Command;

/// Microsoft's Evergreen WebView2 bootstrapper (redistributable, ~1.8 MB).
pub const URL: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";

/// SHA-256 of the bootstrapper this tree was last verified against. Microsoft
/// rotates the fwlink target, so a mismatch is reported with instructions to
/// set `SPID3R_WEBVIEW2_SHA256` to the hash of the file the operator verified.
pub const PINNED_SHA256: &str = "ca4c9016c70e2184c5e231662e0e98abf4826647180ef3faab32fcb1ca2ec15e";

/// Size of that pinned file. Anything shorter is a partial download.
pub const PINNED_BYTES: u64 = 1_888_472;

/// Is the download opt-in enabled? `SPID3R_FETCH_WEBVIEW2=1` (or `true`/`yes`).
// The test harness drives the pure seams below instead of reading the process
// environment (tests run in parallel), so the env wrappers are unused there.
#[allow(dead_code)]
pub fn fetch_enabled() -> bool {
    enabled_from(std::env::var("SPID3R_FETCH_WEBVIEW2").ok().as_deref())
}

pub fn enabled_from(value: Option<&str>) -> bool {
    matches!(
        value.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

/// Expected SHA-256: the operator's override, else the pinned value.
#[allow(dead_code)]
pub fn expected_sha256() -> Option<String> {
    expected_from(std::env::var("SPID3R_WEBVIEW2_SHA256").ok().as_deref())
}

pub fn expected_from(override_value: Option<&str>) -> Option<String> {
    override_value
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| v.len() == 64 && v.chars().all(|c| c.is_ascii_hexdigit()))
        .or_else(|| {
            if PINNED_SHA256.is_empty() {
                None
            } else {
                Some(PINNED_SHA256.to_string())
            }
        })
}

/// Verify a downloaded file: at least `min_bytes` long and matching `expected`
/// (lowercase hex). Returns the actual hash on success so callers can report it.
pub fn verify_file(path: &Path, expected: &str, min_bytes: u64) -> Result<String, String> {
    let meta =
        std::fs::metadata(path).map_err(|e| format!("cannot stat {}: {}", path.display(), e))?;
    let len = meta.len();
    if len < min_bytes {
        return Err(format!(
            "{} is {} bytes, shorter than the expected {} (partial download)",
            path.display(),
            len,
            min_bytes
        ));
    }
    let bytes =
        std::fs::read(path).map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    let actual = sha256::sha256_hex(&bytes);
    if actual != expected.trim().to_ascii_lowercase() {
        return Err(format!(
            "checksum mismatch for {}: expected {}, got {}",
            path.display(),
            expected,
            actual
        ));
    }
    Ok(actual)
}

/// Ensure a verified bootstrapper exists in `redist/`, downloading it only when
/// `fetch` is true. Returns `None` (and leaves no file behind) when the fetch is
/// disabled or verification fails — the installer is then built without it.
pub fn ensure_with(
    nsis_dir: &Path,
    fetch: bool,
    expected: Option<&str>,
    min_bytes: u64,
) -> Option<PathBuf> {
    let dest = nsis_dir
        .join("redist")
        .join("MicrosoftEdgeWebview2Setup.exe");
    if dest.is_file() {
        match expected {
            Some(hash) => match verify_file(&dest, hash, min_bytes) {
                Ok(_) => return Some(dest),
                Err(e) => {
                    // A cached file that no longer verifies is not usable. Do
                    // not delete it silently if fetching is disabled: the
                    // operator may have pinned a different hash deliberately.
                    // However, a truncated partial download (< min_bytes) is never deliberate.
                    println!("cargo:warning=WebView2 bootstrapper rejected: {e}");
                    if fetch
                        || std::fs::metadata(&dest)
                            .map(|m| min_bytes > 0 && m.len() < min_bytes)
                            .unwrap_or(false)
                    {
                        let _ = std::fs::remove_file(&dest);
                    }
                    if !fetch {
                        return None;
                    }
                }
            },
            None => {
                println!(
                    "cargo:warning=WebView2 bootstrapper not verified: no SHA-256 available \
                     (set SPID3R_WEBVIEW2_SHA256)"
                );
                return None;
            }
        }
    }
    if !fetch {
        println!(
            "cargo:warning=WebView2 bootstrapper not bundled: the download is opt-in \
             (set SPID3R_FETCH_WEBVIEW2=1; the installer will build without it)"
        );
        return None;
    }
    let Some(hash) = expected else {
        println!(
            "cargo:warning=WebView2 bootstrapper not fetched: no SHA-256 to verify against \
             (set SPID3R_WEBVIEW2_SHA256)"
        );
        return None;
    };
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Download beside the destination, verify, then move into place. A killed
    // or truncated download therefore never becomes the cached artifact.
    let part = dest.with_extension("part");
    let _ = std::fs::remove_file(&part);
    let curl_ok = match Command::new("curl.exe")
        .args(["-fsSL", "--retry", "2", "--max-time", "120", "-o"])
        .arg(&part)
        .arg(URL)
        .status()
    {
        Ok(s) => s.success(),
        Err(_) => false,
    };
    if !curl_ok {
        let ps_cmd = format!(
            "[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; (New-Object System.Net.WebClient).DownloadFile('{}', '{}')",
            URL,
            part.to_string_lossy().replace('\'', "''")
        );
        let ps_ok = match Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &ps_cmd,
            ])
            .status()
        {
            Ok(s) => s.success(),
            Err(_) => false,
        };
        if !ps_ok {
            println!(
                "cargo:warning=WebView2 download failed via curl and powershell; not bundling it"
            );
            let _ = std::fs::remove_file(&part);
            return None;
        }
    }
    match verify_file(&part, hash, min_bytes) {
        Ok(actual) => {
            if std::fs::rename(&part, &dest).is_err() {
                let _ = std::fs::remove_file(&part);
                println!("cargo:warning=cannot move the verified WebView2 download into place");
                return None;
            }
            println!("cargo:warning=WebView2 bootstrapper fetched and verified ({actual})");
            Some(dest)
        }
        Err(e) => {
            println!("cargo:warning=WebView2 download rejected: {e}");
            let _ = std::fs::remove_file(&part);
            None
        }
    }
}

/// `ensure_with` using the environment policy.
#[allow(dead_code)]
pub fn ensure(nsis_dir: &Path) -> Option<PathBuf> {
    ensure_with(
        nsis_dir,
        fetch_enabled(),
        expected_sha256().as_deref(),
        PINNED_BYTES,
    )
}
