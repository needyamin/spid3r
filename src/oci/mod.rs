//! OCI Image Spec v1, Registry v2 client, and layer pipeline (`R11.1`–`R11.2`).
//!
//! Zero external dependencies: reuses `crate::stdlib::json_parse_text`,
//! `crate::stdlib::net::fetch_bytes`, `crate::stdlib::compress::gunzip_bytes`,
//! `crate::pkg::{sha256_hex, read_tar, ed25519}`.

pub mod client;
pub mod layers;
pub mod manifest;

use std::path::PathBuf;

/// Root directory for Spid3r's local OCI image store (`SPID3R_OCI_DIR` or `~/.spid3r/oci`).
pub fn default_oci_store() -> PathBuf {
    if let Ok(p) = std::env::var("SPID3R_OCI_DIR") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    home.join(".spid3r").join("oci")
}
