//! Symlink-safe recursive file collection and constant-memory streaming SHA-256
//! hashing for package source trees and release verification.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::sha256::Sha256Hasher;

/// Hard ceiling on regular files inside a single package tree (defends against
/// accidental/malicious inode exhaustion).
pub const MAX_PACKAGE_FILES: usize = 10_000;

const STREAM_BUF_BYTES: usize = 64 * 1024;

/// Walk `dir` recursively without following symlinks, collecting regular files
/// in deterministic sorted order. Refuses symlinks and special filesystem entries
/// (audit S1).
pub fn collect_regular_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("cannot inspect {}: {}", dir.display(), e))?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "refusing symlink '{}' as package directory",
            dir.display()
        ));
    }
    if !meta.is_dir() {
        return Err(format!("'{}' is not a directory", dir.display()));
    }
    let mut files = Vec::new();
    collect_walk(dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {}", dir.display(), e))?;
    for entry in entries {
        let p = entry
            .map_err(|e| format!("cannot read {}: {}", dir.display(), e))?
            .path();
        let meta = std::fs::symlink_metadata(&p)
            .map_err(|e| format!("cannot inspect {}: {}", p.display(), e))?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            return Err(format!(
                "refusing symlink '{}' inside package tree",
                p.display()
            ));
        }
        if ft.is_dir() {
            collect_walk(&p, out)?;
        } else if ft.is_file() {
            if out.len() >= MAX_PACKAGE_FILES {
                return Err(format!(
                    "package tree exceeds maximum of {} files",
                    MAX_PACKAGE_FILES
                ));
            }
            out.push(p);
        } else {
            return Err(format!(
                "refusing non-regular file '{}' inside package tree",
                p.display()
            ));
        }
    }
    Ok(())
}

/// Stream-hash every file under `base_dir` using exact relative POSIX path +
/// NUL + raw file bytes + NUL, matching the historical `sha256_bytes` layout at
/// constant memory (`O(1)` RAM).
pub fn hash_files(base_dir: &Path, files: &[PathBuf]) -> Result<String, String> {
    let mut hasher = Sha256Hasher::new();
    let mut buf = [0u8; STREAM_BUF_BYTES];
    for f in files {
        let rel = f.strip_prefix(base_dir).unwrap_or(f);
        hasher.update(rel.to_string_lossy().replace('\\', "/").as_bytes());
        hasher.update(&[0u8]);
        let mut file =
            std::fs::File::open(f).map_err(|e| format!("cannot read {}: {}", f.display(), e))?;
        loop {
            let n = file
                .read(&mut buf)
                .map_err(|e| format!("cannot read {}: {}", f.display(), e))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        hasher.update(&[0u8]);
    }
    Ok(hasher.finalize_hex())
}

/// Deterministic SHA-256 over a package's `src/` tree (`sha256.txt`).
pub fn tree_hash(root: &Path) -> Result<String, String> {
    let src_dir = root.join("src");
    let meta = std::fs::symlink_metadata(&src_dir)
        .map_err(|_| format!("no src/ directory in {}", root.display()))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(format!("no safe src/ directory in {}", root.display()));
    }
    content_hash(&src_dir)
}

/// Deterministic symlink-safe streaming SHA-256 over all regular files under `dir`.
pub fn content_hash(dir: &Path) -> Result<String, String> {
    let files = collect_regular_files(dir)?;
    hash_files(dir, &files)
}

/// Collect regular files under `dir` recursively without following symlinks,
/// skipping any top-level entry whose name matches `skip_top_names` (used to
/// separate vendored `_native/` wheels and manifest snapshots from pure `src/` hashes).
pub fn collect_regular_files_excluding(
    dir: &Path,
    skip_top_names: &[&str],
) -> Result<Vec<PathBuf>, String> {
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("cannot inspect {}: {}", dir.display(), e))?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "refusing symlink '{}' as package directory",
            dir.display()
        ));
    }
    if !meta.is_dir() {
        return Err(format!("'{}' is not a directory", dir.display()));
    }
    let mut files = Vec::new();
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {}", dir.display(), e))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {}: {}", dir.display(), e))?;
        if let Some(name) = entry.file_name().to_str() {
            if skip_top_names.contains(&name) {
                continue;
            }
        }
        let p = entry.path();
        let meta = std::fs::symlink_metadata(&p)
            .map_err(|e| format!("cannot inspect {}: {}", p.display(), e))?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            return Err(format!(
                "refusing symlink '{}' inside package tree",
                p.display()
            ));
        }
        if ft.is_dir() {
            collect_walk(&p, &mut files)?;
        } else if ft.is_file() {
            if files.len() >= MAX_PACKAGE_FILES {
                return Err(format!(
                    "package tree exceeds maximum of {} files",
                    MAX_PACKAGE_FILES
                ));
            }
            files.push(p);
        } else {
            return Err(format!(
                "refusing non-regular file '{}' inside package tree",
                p.display()
            ));
        }
    }
    files.sort();
    Ok(files)
}

/// Reserved internal filenames/directories stored inside `vendor/<pkg>/`
/// that must not alter the package's pure `src/` checksum.
pub const VENDOR_RESERVED_TOP_ENTRIES: &[&str] = &["_native", ".spid3r-manifest.toml"];

/// Hash a vendored package's source files (`vendor/<pkg>/`) excluding
/// `_native/` and `.spid3r-manifest.toml`. Returns `(file_count, sha256)`.
pub fn vendor_src_hash(vendor_pkg_dir: &Path) -> Result<(usize, String), String> {
    let files = collect_regular_files_excluding(vendor_pkg_dir, VENDOR_RESERVED_TOP_ENTRIES)?;
    let hash = hash_files(vendor_pkg_dir, &files)?;
    Ok((files.len(), hash))
}

/// Deterministic SHA-256 over a native target directory (`native/<target>/`).
pub fn native_tree_hash(native_target_dir: &Path) -> Result<String, String> {
    content_hash(native_target_dir)
}

/// Canonical release digest binding `spider.toml` bytes together with the verified
/// `src/` tree SHA-256 (`manifest.sha256` / `manifest.sha256.sig`).
pub fn manifest_and_tree_digest(manifest_bytes: &[u8], tree_sha256: &str) -> String {
    let mut hasher = Sha256Hasher::new();
    hasher.update(b"spid3r-release-v1\0");
    hasher.update(manifest_bytes);
    hasher.update(&[0u8]);
    hasher.update(tree_sha256.trim().as_bytes());
    hasher.update(&[0u8]);
    hasher.finalize_hex()
}

/// Canonical per-target wheel digest binding `spider.toml`, `src/` hash, target
/// triple, and `native/<target>/` hash (`manifest-<target>.sha256`).
pub fn manifest_and_wheel_digest(
    manifest_bytes: &[u8],
    src_sha256: &str,
    target: &str,
    native_sha256: &str,
) -> String {
    let mut hasher = Sha256Hasher::new();
    hasher.update(b"spid3r-wheel-v1\0");
    hasher.update(manifest_bytes);
    hasher.update(&[0u8]);
    hasher.update(src_sha256.trim().as_bytes());
    hasher.update(&[0u8]);
    hasher.update(target.trim().as_bytes());
    hasher.update(&[0u8]);
    hasher.update(native_sha256.trim().as_bytes());
    hasher.update(&[0u8]);
    hasher.finalize_hex()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::sha256::sha256_hex;

    #[test]
    fn streaming_hasher_matches_oneshot_across_block_boundaries() {
        for len in [0usize, 1, 55, 56, 63, 64, 65, 127, 128, 129, 100_000] {
            let data: Vec<u8> = (0..len).map(|i| ((i * 31 + 7) % 251) as u8).collect();
            let expected = sha256_hex(&data);
            for chunk_size in [1usize, 7, 64, 1024] {
                let mut h = Sha256Hasher::new();
                for chunk in data.chunks(chunk_size) {
                    h.update(chunk);
                }
                assert_eq!(
                    h.finalize_hex(),
                    expected,
                    "len={len} chunk_size={chunk_size}"
                );
            }
        }
    }

    #[test]
    fn collect_regular_files_rejects_symlinks() {
        let dir = std::env::temp_dir().join(format!("spid3r_hash_sym_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("real.matrix");
        std::fs::write(&target, "x\n").unwrap();
        let link = dir.join("evil.matrix");

        #[cfg(unix)]
        let sym_res = std::os::unix::fs::symlink(&target, &link);
        #[cfg(windows)]
        let sym_res = std::os::windows::fs::symlink_file(&target, &link);

        if sym_res.is_ok() {
            let err = collect_regular_files(&dir).unwrap_err();
            assert!(err.contains("symlink"), "expected symlink error, got {err}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
