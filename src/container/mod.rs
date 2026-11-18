//! Spid3r OCI container runtime (`R11.3`, `R11.5`–`R11.9`).
//!
//! Zero external dependencies. Uses `src/oci/` for image/layer pull & verify
//! and `src/native/sys.rs` (Linux + `--features container`, `DP-7`) for kernel
//! namespace/mount/seccomp/cgroup isolation.

pub mod daemon;
pub mod net;
pub mod runtime;
pub mod snapshot;

use std::path::PathBuf;

/// Root state directory for Spid3r containers (`SPID3R_CONTAINER_DIR` or `~/.spid3r/containers`).
pub fn default_container_dir() -> PathBuf {
    if let Ok(p) = std::env::var("SPID3R_CONTAINER_DIR") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    home.join(".spid3r").join("containers")
}
