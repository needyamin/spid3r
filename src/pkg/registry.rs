//! Remote HTTP(S) registry client for `spid3r install` (`R8.2`).
//!
//! When `packages.registry` / `SPID3R_REGISTRY` starts with `http://` or `https://`,
//! this client fetches `<base>/<name>/index.txt` plus each version's `sha256.txt`,
//! optional `sha256.txt.sig`, and `package.tar`, verifies paths and tree SHA-256
//! before caching under `.spid3r/registry/<hash>/<name>/<version>/` in the exact
//! directory layout [`super::resolve_registry`] reads.

use std::path::{Path, PathBuf};

use super::{read_tar, sha256_hex, tree_hash, valid_pkg_name, Version};

/// True when `spec` points at an HTTP or HTTPS package registry.
pub fn is_remote_url(spec: &str) -> bool {
    let lower = spec.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Sync all published versions of `name` from a remote HTTP(S) registry into a
/// local cache directory formatted identically to a filesystem registry root,
/// returning the cache root path (`<project>/.spid3r/registry/<registry_id>`).
pub fn sync_remote_package(
    project_root: &Path,
    registry_url: &str,
    sub_registry: Option<&str>,
    name: &str,
    update: bool,
) -> Result<PathBuf, String> {
    valid_pkg_name(name)?;
    let mut base = registry_url.trim().trim_end_matches('/').to_string();
    if let Some(sub) = sub_registry {
        valid_pkg_name(sub)?;
        base = format!("{}/{}", base, sub);
    }

    let reg_id = &sha256_hex(base.as_bytes())[..16];
    let cache_root = project_root.join(".spid3r").join("registry").join(reg_id);

    let index_url = format!("{}/{}/index.txt", base, name);
    let (status, _, index_bytes) = crate::stdlib::net::fetch_bytes("GET", &index_url, &[], &[])
        .map_err(|e| {
            format!(
                "registry: cannot reach remote registry '{}' ({}): {}",
                base, index_url, e
            )
        })?;
    if status == 404 {
        return Err(format!(
            "registry: no package '{}' (looked in {})",
            name, base
        ));
    }
    if status != 200 {
        return Err(format!(
            "registry: failed to fetch {} (HTTP {})",
            index_url, status
        ));
    }

    let index_text = String::from_utf8_lossy(&index_bytes);
    let mut versions: Vec<String> = Vec::new();
    for line in index_text.lines() {
        let ver = line.trim();
        if ver.is_empty() || ver.starts_with('#') {
            continue;
        }
        Version::parse(ver).map_err(|e| {
            format!(
                "registry: invalid version '{}' in {}: {}",
                ver, index_url, e
            )
        })?;
        versions.push(ver.to_string());
    }
    if versions.is_empty() {
        return Err(format!(
            "registry: no versions listed for '{}' at {}",
            name, index_url
        ));
    }

    let pkg_cache_dir = cache_root.join(name);
    std::fs::create_dir_all(&pkg_cache_dir)
        .map_err(|e| format!("cannot create {}: {}", pkg_cache_dir.display(), e))?;

    let pub_cache_path = cache_root.join("registry.pub");
    if update || !pub_cache_path.is_file() {
        let pub_url = format!("{}/registry.pub", base);
        if let Ok((200, _, pub_bytes)) = crate::stdlib::net::fetch_bytes("GET", &pub_url, &[], &[])
        {
            let _ = std::fs::write(&pub_cache_path, &pub_bytes);
        }
    }

    for ver in &versions {
        let ver_dir = pkg_cache_dir.join(ver);
        if !update && ver_dir.join("sha256.txt").is_file() && ver_dir.join("spider.toml").is_file()
        {
            continue;
        }
        fetch_and_verify_version(&pkg_cache_dir, &base, name, ver, &ver_dir)?;
    }

    Ok(cache_root)
}

fn fetch_and_verify_version(
    pkg_cache_dir: &Path,
    base_url: &str,
    name: &str,
    ver: &str,
    ver_dir: &Path,
) -> Result<(), String> {
    let sha_url = format!("{}/{}/{}/sha256.txt", base_url, name, ver);
    let (sha_status, _, sha_bytes) = crate::stdlib::net::fetch_bytes("GET", &sha_url, &[], &[])
        .map_err(|e| {
            format!(
                "registry dependency '{}': failed to fetch {}: {}",
                name, sha_url, e
            )
        })?;
    if sha_status != 200 {
        return Err(format!(
            "registry dependency '{}': missing sha256.txt at {} (HTTP {})",
            name, sha_url, sha_status
        ));
    }
    let sha_text = String::from_utf8_lossy(&sha_bytes).trim().to_string();
    if sha_text.is_empty() {
        return Err(format!(
            "registry dependency '{}': sha256.txt is empty at {} — refusing to install unverified code",
            name, sha_url
        ));
    }

    // Optional detached Ed25519 signature over exact ASCII sha256.txt bytes.
    let sig_url = format!("{}/{}/{}/sha256.txt.sig", base_url, name, ver);
    let sig_bytes_opt = match crate::stdlib::net::fetch_bytes("GET", &sig_url, &[], &[]) {
        Ok((200, _, bytes)) => Some(bytes),
        _ => None,
    };

    let tar_url = format!("{}/{}/{}/package.tar", base_url, name, ver);
    let (mut tar_status, _, mut tar_bytes) =
        crate::stdlib::net::fetch_bytes("GET", &tar_url, &[], &[]).map_err(|e| {
            format!(
                "registry dependency '{}': failed to fetch {}: {}",
                name, tar_url, e
            )
        })?;
    if tar_status == 404 {
        let alt_url = format!("{}/{}/{}-{}.tar", base_url, name, name, ver);
        let (st, _, bytes) =
            crate::stdlib::net::fetch_bytes("GET", &alt_url, &[], &[]).map_err(|e| {
                format!(
                    "registry dependency '{}': failed to fetch {}: {}",
                    name, alt_url, e
                )
            })?;
        tar_status = st;
        tar_bytes = bytes;
    }
    if tar_status != 200 {
        return Err(format!(
            "registry dependency '{}': failed to download tarball for {} (HTTP {})",
            name, ver, tar_status
        ));
    }

    let stage_dir = pkg_cache_dir.join(format!(".stage_{}_{}", ver, std::process::id()));
    if stage_dir.exists() {
        let _ = std::fs::remove_dir_all(&stage_dir);
    }
    std::fs::create_dir_all(&stage_dir)
        .map_err(|e| format!("cannot create {}: {}", stage_dir.display(), e))?;

    let res = (|| -> Result<(), String> {
        extract_tar_checked(&tar_bytes, &stage_dir)?;
        let actual = tree_hash(&stage_dir)?;
        if actual != sha_text {
            return Err(format!(
                "registry dependency '{}': checksum mismatch for {}@{} \
                 (sha256.txt declares {}, archive tree hashes to {})",
                name, name, ver, sha_text, actual
            ));
        }
        std::fs::write(stage_dir.join("sha256.txt"), &sha_bytes)
            .map_err(|e| format!("cannot write cached sha256.txt: {}", e))?;
        if let Some(sig_bytes) = &sig_bytes_opt {
            std::fs::write(stage_dir.join("sha256.txt.sig"), sig_bytes)
                .map_err(|e| format!("cannot write cached sha256.txt.sig: {}", e))?;
        }
        if ver_dir.exists() {
            std::fs::remove_dir_all(ver_dir)
                .map_err(|e| format!("cannot clear {}: {}", ver_dir.display(), e))?;
        }
        std::fs::rename(&stage_dir, ver_dir).map_err(|e| {
            format!(
                "cannot move {} to {}: {}",
                stage_dir.display(),
                ver_dir.display(),
                e
            )
        })?;
        Ok(())
    })();

    if res.is_err() {
        let _ = std::fs::remove_dir_all(&stage_dir);
    }
    res
}

/// Extract a tarball into `dest` using `read_tar` plus `unpack`'s path-traversal
/// guard (`..`, `:`, leading `/` or `\`, and `target.starts_with(dest)`).
fn extract_tar_checked(data: &[u8], dest: &Path) -> Result<(), String> {
    let entries = read_tar(data)?;
    for (name, bytes) in entries {
        if name.contains("..")
            || name.contains(':')
            || name.starts_with('/')
            || name.starts_with('\\')
        {
            return Err(format!("tar: refusing unsafe path '{}'", name));
        }
        let target = dest.join(&name);
        if !target.starts_with(dest) {
            return Err(format!("tar: refusing unsafe path '{}'", name));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {}", parent.display(), e))?;
        }
        std::fs::write(&target, &bytes)
            .map_err(|e| format!("cannot write {}: {}", target.display(), e))?;
    }
    Ok(())
}
