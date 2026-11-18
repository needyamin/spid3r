//! OCI layer pipeline: SHA-256 verification before extraction, memory-capped
//! gzip inflate, hardened ustar extraction (traversal refusal), `.wh.` whiteout
//! support, and race-safe atomic rename into the content-addressed layer cache (`R11.2`).

use std::path::{Component, Path, PathBuf};

use super::manifest::{validate_digest, verify_digest_bytes};
use crate::pkg::read_tar;
use crate::stdlib::compress::gunzip_bytes;

/// Hard cap on uncompressed layer tar size in memory (64 MiB).
pub const MAX_LAYER_TAR_BYTES: usize = 64 * 1024 * 1024;

/// Store a raw blob under `<store_dir>/blobs/sha256/<hex>` after verifying its digest.
pub fn store_blob_verified(
    store_dir: &Path,
    expected_digest: &str,
    bytes: &[u8],
) -> Result<PathBuf, String> {
    let hex = verify_digest_bytes(expected_digest, bytes)?;
    let blobs_dir = store_dir.join("blobs").join("sha256");
    std::fs::create_dir_all(&blobs_dir)
        .map_err(|e| format!("oci: mkdir {}: {e}", blobs_dir.display()))?;
    let dest = blobs_dir.join(&hex);
    if dest.is_file() {
        return Ok(dest);
    }
    let tmp = blobs_dir.join(format!(
        "{hex}.tmp.{}.{}",
        std::process::id(),
        unique_nonce()
    ));
    std::fs::write(&tmp, bytes).map_err(|e| format!("oci: write blob: {e}"))?;
    if !dest.exists() {
        let _ = std::fs::rename(&tmp, &dest);
    }
    let _ = std::fs::remove_file(&tmp);
    Ok(dest)
}

/// Verify `blob_bytes` against `expected_digest`, gunzip (if gzipped), validate
/// all tar paths, and atomically unpack into `<store_dir>/layers/<hex>/`.
///
/// Corrupted digests or path traversal entries fail closed before anything is
/// committed to the layer cache.
pub fn cache_and_unpack_layer(
    store_dir: &Path,
    expected_digest: &str,
    blob_bytes: &[u8],
) -> Result<PathBuf, String> {
    let hex = validate_digest(expected_digest)?;
    let layer_dir = store_dir.join("layers").join(&hex);
    if layer_dir.is_dir() {
        // Still verify the incoming blob digest before trusting it.
        verify_digest_bytes(expected_digest, blob_bytes)?;
        return Ok(layer_dir);
    }

    store_blob_verified(store_dir, expected_digest, blob_bytes)?;

    if blob_bytes.len() > MAX_LAYER_TAR_BYTES {
        return Err(format!(
            "error[corrupt]: OCI layer '{}' exceeds {} byte buffer cap",
            expected_digest, MAX_LAYER_TAR_BYTES
        ));
    }

    let tar_bytes = if blob_bytes.len() >= 2 && blob_bytes[0] == 0x1f && blob_bytes[1] == 0x8b {
        gunzip_bytes(blob_bytes).map_err(|e| {
            format!("error[corrupt]: gzip inflate failed for {expected_digest}: {e}")
        })?
    } else {
        blob_bytes.to_vec()
    };
    if tar_bytes.len() > MAX_LAYER_TAR_BYTES {
        return Err(format!(
            "error[corrupt]: unpacked OCI layer '{}' exceeds {} byte cap",
            expected_digest, MAX_LAYER_TAR_BYTES
        ));
    }

    let entries = read_tar(&tar_bytes)
        .map_err(|e| format!("error[corrupt]: invalid layer tar {expected_digest}: {e}"))?;

    let parent = store_dir.join("layers");
    std::fs::create_dir_all(&parent)
        .map_err(|e| format!("oci: mkdir {}: {e}", parent.display()))?;
    let tmp_dir = parent.join(format!(
        "{hex}.tmp.{}.{}",
        std::process::id(),
        unique_nonce()
    ));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("oci: mkdir {}: {e}", tmp_dir.display()))?;

    let mut whiteouts: Vec<String> = Vec::new();
    for (raw_path, data) in &entries {
        let rel = sanitize_tar_rel_path(raw_path).inspect_err(|_| {
            let _ = std::fs::remove_dir_all(&tmp_dir);
        })?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let file_name = rel.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        if file_name.starts_with(".wh.") {
            whiteouts.push(rel.to_string_lossy().replace('\\', "/"));
            continue;
        }
        let dest = tmp_dir.join(&rel);
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("oci: mkdir {}: {e}", p.display()))?;
        }
        std::fs::write(&dest, data).map_err(|e| format!("oci: write {}: {e}", dest.display()))?;
    }

    if !whiteouts.is_empty() {
        let wh_file = tmp_dir.join(".spid3r_whiteouts");
        std::fs::write(&wh_file, whiteouts.join("\n"))
            .map_err(|e| format!("oci: write whiteouts: {e}"))?;
    }

    if !layer_dir.exists() {
        if let Err(e) = std::fs::rename(&tmp_dir, &layer_dir) {
            if !layer_dir.is_dir() {
                let _ = std::fs::remove_dir_all(&tmp_dir);
                return Err(format!("oci: commit layer {}: {e}", layer_dir.display()));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp_dir);
    Ok(layer_dir)
}

/// Apply a layer's extracted files + `.wh.` whiteouts onto `rootfs_dir` (copy-up
/// assembly mode, `R11.2`/`R11.3`).
pub fn apply_layer_to_rootfs(layer_dir: &Path, rootfs_dir: &Path) -> Result<(), String> {
    let wh_file = layer_dir.join(".spid3r_whiteouts");
    if wh_file.is_file() {
        let txt =
            std::fs::read_to_string(&wh_file).map_err(|e| format!("oci: read whiteouts: {e}"))?;
        for line in txt.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let rel = sanitize_tar_rel_path(line)?;
            let parent_rel = rel.parent().unwrap_or_else(|| Path::new(""));
            let fname = rel.file_name().and_then(|s| s.to_str()).unwrap_or_default();
            if fname == ".wh..wh..opq" {
                let target_dir = rootfs_dir.join(parent_rel);
                if target_dir.is_dir() {
                    let _ = std::fs::remove_dir_all(&target_dir);
                    std::fs::create_dir_all(&target_dir)
                        .map_err(|e| format!("oci: recreate opaque dir: {e}"))?;
                }
            } else if let Some(victim) = fname.strip_prefix(".wh.") {
                let target = rootfs_dir.join(parent_rel).join(victim);
                if target.is_dir() {
                    let _ = std::fs::remove_dir_all(&target);
                } else if target.exists() {
                    let _ = std::fs::remove_file(&target);
                }
            }
        }
    }
    copy_dir_filtered(layer_dir, rootfs_dir)
}

fn copy_dir_filtered(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("oci: mkdir {}: {e}", dst.display()))?;
    for entry in
        std::fs::read_dir(src).map_err(|e| format!("oci: readdir {}: {e}", src.display()))?
    {
        let entry = entry.map_err(|e| format!("oci: dir entry: {e}"))?;
        let name = entry.file_name();
        if name == ".spid3r_whiteouts" {
            continue;
        }
        let sp = entry.path();
        let dp = dst.join(&name);
        if sp.is_dir() {
            copy_dir_filtered(&sp, &dp)?;
        } else {
            if let Some(p) = dp.parent() {
                std::fs::create_dir_all(p)
                    .map_err(|e| format!("oci: mkdir {}: {e}", p.display()))?;
            }
            std::fs::copy(&sp, &dp)
                .map_err(|e| format!("oci: copy {} -> {}: {e}", sp.display(), dp.display()))?;
        }
    }
    Ok(())
}

/// Sanitize a tar entry path, refusing absolute paths, drive prefixes, and `..`.
pub fn sanitize_tar_rel_path(raw: &str) -> Result<PathBuf, String> {
    let normalized = raw.replace('\\', "/");
    if normalized.starts_with('/') || normalized.contains(':') {
        return Err(format!(
            "error[corrupt]: tar path traversal/absolute path refused: '{}'",
            raw
        ));
    }
    let mut out = PathBuf::new();
    for comp in Path::new(&normalized).components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(seg) => out.push(seg),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "error[corrupt]: tar path traversal refused: '{}'",
                    raw
                ));
            }
        }
    }
    Ok(out)
}

fn unique_nonce() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}
