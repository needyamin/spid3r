//! Multi-key Ed25519 trust verification and atomic staged vendoring into `vendor/`
//! (audit S2, S5, S7).

use std::path::Path;

use super::ed25519::verify as ed25519_verify;
use super::hash::{
    collect_regular_files, manifest_and_tree_digest, manifest_and_wheel_digest, native_tree_hash,
    tree_hash, vendor_src_hash,
};
use super::manifest::{
    host_target_triple, valid_pkg_name, validate_native_rel_path, DepSource, Manifest,
};
use super::sha256::hex_decode;

/// Outcome of verified atomic vendoring into `vendor/<name>/`.
#[derive(Debug, Clone)]
pub struct VendoredPkg {
    pub files: usize,
    pub sha256: String,
    pub manifest_sha256: Option<String>,
    pub target: Option<String>,
    pub native_sha256: Option<String>,
    pub ffi_libs: Vec<String>,
}

/// Parse `packages.trust_pk` / `SPID3R_TRUST_PK` supporting one or more
/// comma-separated 64-hex Ed25519 public keys for zero-downtime key rotation
/// (audit S7).
pub fn configured_trust_pks() -> Result<Vec<[u8; 32]>, String> {
    let raw = crate::config::get("packages.trust_pk")
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::var("SPID3R_TRUST_PK")
                .ok()
                .filter(|v| !v.trim().is_empty())
        });
    let Some(raw_text) = raw else {
        return Ok(Vec::new());
    };
    let mut keys = Vec::new();
    for part in raw_text.split(',') {
        let pk_hex = part.trim();
        if pk_hex.is_empty() {
            continue;
        }
        let pk_bytes = hex_decode(pk_hex)
            .ok_or_else(|| format!("invalid packages.trust_pk hex '{}'", pk_hex))?;
        let pk_arr: [u8; 32] = pk_bytes
            .try_into()
            .map_err(|_| format!("packages.trust_pk '{}' must decode to 32 bytes", pk_hex))?;
        keys.push(pk_arr);
    }
    Ok(keys)
}

/// Verify an ASCII hex-encoded Ed25519 signature `sig_hex` over `msg` against
/// at least one key in `pks`.
pub fn verify_ed25519_against_any(
    pks: &[[u8; 32]],
    msg: &[u8],
    sig_hex: &str,
    label: &str,
) -> Result<(), String> {
    let sig_bytes =
        hex_decode(sig_hex.trim()).ok_or_else(|| format!("{}: invalid hex signature", label))?;
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| format!("{}: signature must decode to 64 bytes", label))?;
    if pks.iter().any(|pk| ed25519_verify(pk, msg, &sig_arr)) {
        return Ok(());
    }
    Err(format!("{}: Ed25519 signature verification failed", label))
}

/// Copy `dep_dir/src`, `spider.toml` snapshot, and verified `native/<target>/`
/// atomically into `root/vendor/<name>` using staging and backup directories
/// so interrupted installs never corrupt `vendor/` (audit S5).
pub fn vendor_dep_full(root: &Path, name: &str, dep_dir: &Path) -> Result<VendoredPkg, String> {
    valid_pkg_name(name)?;
    let src_dir = dep_dir.join("src");
    let src_meta = std::fs::symlink_metadata(&src_dir).map_err(|_| {
        format!(
            "dependency '{}' has no src/ directory ({})",
            name,
            dep_dir.display()
        )
    })?;
    if src_meta.file_type().is_symlink() || !src_meta.is_dir() {
        return Err(format!(
            "dependency '{}' has invalid src/ directory ({})",
            name,
            dep_dir.display()
        ));
    }

    let vendor_root = root.join("vendor");
    std::fs::create_dir_all(&vendor_root)
        .map_err(|e| format!("cannot create {}: {}", vendor_root.display(), e))?;
    let dest = vendor_root.join(name);
    if !dest.starts_with(&vendor_root) || dest == vendor_root {
        return Err(format!(
            "refusing to vendor outside vendor/: {}",
            dest.display()
        ));
    }

    let host_triple = host_target_triple();
    let native_src_dir = dep_dir.join("native").join(host_triple);
    let has_native_dir = match std::fs::symlink_metadata(&native_src_dir) {
        Ok(m) if m.file_type().is_symlink() => {
            return Err(format!(
                "dependency '{}': refusing symlinked native directory {}",
                name,
                native_src_dir.display()
            ));
        }
        Ok(m) if m.is_dir() => true,
        Ok(_) => {
            return Err(format!(
                "dependency '{}': non-directory at {}",
                name,
                native_src_dir.display()
            ));
        }
        Err(_) => false,
    };

    let mut ffi_libs = Vec::new();
    let manifest_src = dep_dir.join("spider.toml");
    let has_manifest = match std::fs::symlink_metadata(&manifest_src) {
        Ok(m) if m.file_type().is_symlink() => {
            return Err(format!(
                "dependency '{}': refusing symlinked spider.toml",
                name
            ));
        }
        Ok(m) if m.is_file() => {
            if let Ok(Some(m_parsed)) = Manifest::load(dep_dir) {
                ffi_libs.extend(m_parsed.ffi.libs.iter().cloned());
                ffi_libs.extend(m_parsed.ffi.bundled.iter().cloned());
                ffi_libs.extend(m_parsed.ffi.runtime_libs.iter().cloned());
            }
            true
        }
        _ => false,
    };

    let pid = std::process::id();
    let stage = vendor_root.join(format!(".tmp_{}_{}", name, pid));
    let backup = vendor_root.join(format!(".old_{}_{}", name, pid));
    if stage.exists() {
        let _ = std::fs::remove_dir_all(&stage);
    }
    if backup.exists() {
        let _ = std::fs::remove_dir_all(&backup);
    }

    let files = collect_regular_files(&src_dir)?;
    let prep = (|| -> Result<(usize, String, Option<String>, Option<String>), String> {
        std::fs::create_dir_all(&stage)
            .map_err(|e| format!("cannot create {}: {}", stage.display(), e))?;
        for f in &files {
            let rel = f.strip_prefix(&src_dir).unwrap_or(f);
            let target = stage.join(rel);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create {}: {}", parent.display(), e))?;
            }
            std::fs::copy(f, &target).map_err(|e| {
                format!("cannot copy {} -> {}: {}", f.display(), target.display(), e)
            })?;
        }
        if has_manifest {
            std::fs::copy(&manifest_src, stage.join(".spid3r-manifest.toml"))
                .map_err(|e| format!("cannot snapshot manifest for '{}': {}", name, e))?;
        }
        let (target_opt, native_sha_opt) = if has_native_dir {
            let native_files = collect_regular_files(&native_src_dir)?;
            let native_dest = stage.join("_native").join(host_triple);
            std::fs::create_dir_all(&native_dest)
                .map_err(|e| format!("cannot create {}: {}", native_dest.display(), e))?;
            for nf in &native_files {
                let rel = nf.strip_prefix(&native_src_dir).unwrap_or(nf);
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                validate_native_rel_path(&rel_str)?;
                let target = native_dest.join(rel);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("cannot create {}: {}", parent.display(), e))?;
                }
                std::fs::copy(nf, &target).map_err(|e| {
                    format!(
                        "cannot copy {} -> {}: {}",
                        nf.display(),
                        target.display(),
                        e
                    )
                })?;
            }
            let n_hash = native_tree_hash(&native_dest)?;
            (Some(host_triple.to_string()), Some(n_hash))
        } else {
            (None, None)
        };
        let (count, hash) = vendor_src_hash(&stage)?;
        Ok((count, hash, target_opt, native_sha_opt))
    })();

    let (count, hash, target_opt, native_sha_opt) = match prep {
        Ok(res) => res,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&stage);
            return Err(e);
        }
    };

    let had_dest = dest.exists();
    if had_dest {
        if let Err(e) = std::fs::rename(&dest, &backup) {
            let _ = std::fs::remove_dir_all(&stage);
            return Err(format!("cannot stage backup of {}: {}", dest.display(), e));
        }
    }
    if let Err(e) = std::fs::rename(&stage, &dest) {
        if had_dest && backup.exists() {
            let _ = std::fs::rename(&backup, &dest);
        }
        let _ = std::fs::remove_dir_all(&stage);
        return Err(format!("cannot commit {}: {}", dest.display(), e));
    }
    if backup.exists() {
        let _ = std::fs::remove_dir_all(&backup);
    }
    Ok(VendoredPkg {
        files: count,
        sha256: hash,
        manifest_sha256: None,
        target: target_opt,
        native_sha256: native_sha_opt,
        ffi_libs,
    })
}

/// Backward-compatible wrapper returning `(file_count, sha256)`.
pub fn vendor_dep(root: &Path, name: &str, dep_dir: &Path) -> Result<(usize, String), String> {
    let v = vendor_dep_full(root, name, dep_dir)?;
    Ok((v.files, v.sha256))
}

/// Vendor `dep_dir` cleanly with mandatory SHA-256 + Ed25519 + optional
/// `manifest.sha256` checks for registry dependencies before committing to `vendor/`.
pub fn vendor_dep_checked(
    root: &Path,
    name: &str,
    dep_dir: &Path,
    source: &DepSource,
) -> Result<VendoredPkg, String> {
    let mut manifest_sha_out: Option<String> = None;
    let expected: Option<String> = if matches!(source, DepSource::Registry { .. }) {
        let path = dep_dir.join("sha256.txt");
        let raw_sha_bytes = std::fs::read(&path).map_err(|e| {
            format!(
                "registry dependency '{}': no readable sha256.txt at {} ({}) — \
                 refusing to install unverified code; publish with \
                 `spkg publish --to-dir` to generate one",
                name,
                path.display(),
                e
            )
        })?;
        let text = String::from_utf8_lossy(&raw_sha_bytes);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(format!(
                "registry dependency '{}': sha256.txt is empty — refusing to \
                 install unverified code",
                name
            ));
        }

        let actual = tree_hash(dep_dir)?;
        if actual != trimmed {
            return Err(format!(
                "registry dependency '{}': checksum mismatch before install \
                 (sha256.txt declares {}, source tree hashes to {})",
                name, trimmed, actual
            ));
        }

        let pks = configured_trust_pks()?;
        let sig_path = dep_dir.join("sha256.txt.sig");
        if !pks.is_empty() {
            let sig_text = std::fs::read_to_string(&sig_path).map_err(|e| {
                format!(
                    "registry dependency '{}': missing signature {} required by packages.trust_pk ({})",
                    name,
                    sig_path.display(),
                    e
                )
            })?;
            verify_ed25519_against_any(
                &pks,
                &raw_sha_bytes,
                &sig_text,
                &format!("registry dependency '{}' ({})", name, sig_path.display()),
            )?;
        } else if !sig_path.is_file() {
            eprintln!(
                "warning: registry dependency '{}' is unsigned (no sha256.txt.sig)",
                name
            );
        }

        // Reject symlinked metadata files before reading them.
        for meta_name in ["sha256.txt", "manifest.sha256", "spider.toml"] {
            let p = dep_dir.join(meta_name);
            if let Ok(m) = std::fs::symlink_metadata(&p) {
                if m.file_type().is_symlink() {
                    return Err(format!(
                        "registry dependency '{}': refusing symlinked {}",
                        name,
                        p.display()
                    ));
                }
            }
        }

        // Verify full manifest + src/ digest when present, and REQUIRE both
        // manifest.sha256 and manifest.sha256.sig when packages.trust_pk is configured (audit C2).
        let msha_path = dep_dir.join("manifest.sha256");
        let raw_msha_bytes = match std::fs::read(&msha_path) {
            Ok(b) => Some(b),
            Err(e) if !pks.is_empty() => {
                return Err(format!(
                    "registry dependency '{}': missing {} required by packages.trust_pk ({})",
                    name,
                    msha_path.display(),
                    e
                ));
            }
            Err(_) => None,
        };
        if let Some(raw_msha_bytes) = raw_msha_bytes {
            let msha_declared = String::from_utf8_lossy(&raw_msha_bytes).trim().to_string();
            let manifest_bytes = std::fs::read(dep_dir.join("spider.toml")).map_err(|e| {
                format!(
                    "registry dependency '{}': cannot read spider.toml: {}",
                    name, e
                )
            })?;
            let computed = manifest_and_tree_digest(&manifest_bytes, trimmed);
            if computed != msha_declared {
                return Err(format!(
                    "registry dependency '{}': manifest.sha256 mismatch (declares {}, computed {})",
                    name, msha_declared, computed
                ));
            }
            if !pks.is_empty() {
                let msig_path = dep_dir.join("manifest.sha256.sig");
                let msig_text = std::fs::read_to_string(&msig_path).map_err(|e| {
                    format!(
                        "registry dependency '{}': missing {} required by packages.trust_pk ({})",
                        name,
                        msig_path.display(),
                        e
                    )
                })?;
                verify_ed25519_against_any(
                    &pks,
                    &raw_msha_bytes,
                    &msig_text,
                    &format!("registry dependency '{}' ({})", name, msig_path.display()),
                )?;
            }
            manifest_sha_out = Some(msha_declared);
        }

        // Verify optional wheel digest (`sha256-<target>.txt` + `.sig`,
        // `manifest-<target>.sha256` + `.sig`) when native artifacts or wheel metadata exist.
        let triple = host_target_triple();
        let wheel_sha_path = dep_dir.join(format!("sha256-{}.txt", triple));
        let native_dir = dep_dir.join("native").join(triple);
        if wheel_sha_path.is_file() || native_dir.is_dir() {
            let raw_wsha = std::fs::read(&wheel_sha_path).map_err(|e| {
                format!(
                    "registry wheel '{}@{}': missing {} ({})",
                    name,
                    triple,
                    wheel_sha_path.display(),
                    e
                )
            })?;
            let wsha_declared = String::from_utf8_lossy(&raw_wsha).trim().to_string();
            if wsha_declared.is_empty() {
                return Err(format!(
                    "registry wheel '{}@{}': sha256-{}.txt is empty",
                    name, triple, triple
                ));
            }
            if !native_dir.is_dir() {
                return Err(format!(
                    "registry wheel '{}@{}': sha256-{}.txt present without native/{}/",
                    name, triple, triple, triple
                ));
            }
            let actual_native = native_tree_hash(&native_dir)?;
            if actual_native != wsha_declared {
                return Err(format!(
                    "registry wheel '{}@{}': native checksum mismatch (declares {}, hashes to {})",
                    name, triple, wsha_declared, actual_native
                ));
            }
            let wsig_path = dep_dir.join(format!("sha256-{}.txt.sig", triple));
            if !pks.is_empty() {
                let wsig = std::fs::read_to_string(&wsig_path).map_err(|e| {
                    format!(
                        "registry wheel '{}@{}': missing {} required by packages.trust_pk ({})",
                        name,
                        triple,
                        wsig_path.display(),
                        e
                    )
                })?;
                verify_ed25519_against_any(
                    &pks,
                    &raw_wsha,
                    &wsig,
                    &format!(
                        "registry wheel '{}@{}' ({})",
                        name,
                        triple,
                        wsig_path.display()
                    ),
                )?;
            }
            let wmsha_path = dep_dir.join(format!("manifest-{}.sha256", triple));
            let raw_wmsha_opt = match std::fs::read(&wmsha_path) {
                Ok(b) => Some(b),
                Err(e) if !pks.is_empty() => {
                    return Err(format!(
                        "registry wheel '{}@{}': missing {} required by packages.trust_pk ({})",
                        name,
                        triple,
                        wmsha_path.display(),
                        e
                    ));
                }
                Err(_) => None,
            };
            if let Some(raw_wmsha) = raw_wmsha_opt {
                let declared_wmsha = String::from_utf8_lossy(&raw_wmsha).trim().to_string();
                let manifest_bytes = std::fs::read(dep_dir.join("spider.toml")).map_err(|e| {
                    format!(
                        "registry wheel '{}@{}': cannot read spider.toml: {}",
                        name, triple, e
                    )
                })?;
                let computed =
                    manifest_and_wheel_digest(&manifest_bytes, trimmed, triple, &wsha_declared);
                if computed != declared_wmsha {
                    return Err(format!(
                        "registry wheel '{}@{}': manifest-{}.sha256 mismatch (declares {}, computed {})",
                        name, triple, triple, declared_wmsha, computed
                    ));
                }
                if !pks.is_empty() {
                    let wmsig_path = dep_dir.join(format!("manifest-{}.sha256.sig", triple));
                    let wmsig = std::fs::read_to_string(&wmsig_path).map_err(|e| {
                        format!(
                            "registry wheel '{}@{}': missing {} required by packages.trust_pk ({})",
                            name,
                            triple,
                            wmsig_path.display(),
                            e
                        )
                    })?;
                    verify_ed25519_against_any(
                        &pks,
                        &raw_wmsha,
                        &wmsig,
                        &format!(
                            "registry wheel '{}@{}' ({})",
                            name,
                            triple,
                            wmsig_path.display()
                        ),
                    )?;
                }
            }
        }

        Some(trimmed.to_string())
    } else {
        None
    };

    let mut vendored = vendor_dep_full(root, name, dep_dir)?;
    vendored.manifest_sha256 = manifest_sha_out;

    if let Some(expected) = &expected {
        if vendored.sha256 != *expected {
            let _ = std::fs::remove_dir_all(root.join("vendor").join(name));
            return Err(format!(
                "registry dependency '{}': checksum mismatch after vendoring \
                 (sha256.txt declares {}, vendored tree hashes to {})",
                name, expected, vendored.sha256
            ));
        }
    }
    Ok(vendored)
}
