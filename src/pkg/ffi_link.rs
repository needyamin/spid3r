//! Verified automatic FFI library collection for `spid3r build` / `yamc`.
//!
//! Walks up from the build target to find an enclosing `spider.toml` /
//! `spider.lock` + `vendor/`, re-verifies every dependency with `[ffi]` or
//! `_native/` artifacts against `spider.lock` hashes BEFORE passing anything to
//! the linker, rejects linker flag/path injection, and collects runtime DLLs/SOs
//! to copy beside the compiled executable.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::hash::{manifest_and_tree_digest, native_tree_hash, vendor_src_hash};
use super::lockfile::{read_lock, LockEntry};
use super::manifest::{
    host_target_triple, parse_manifest, valid_pkg_name, validate_ffi_lib_name,
    validate_native_rel_path, FfiConfig, Manifest,
};

#[derive(Debug, Default, Clone)]
pub struct ResolvedFfiLinks {
    /// Arguments passed through `set_link_libs(...)` to the native linker.
    pub link_libs: Vec<String>,
    /// Shared libraries `(source_abs_path, file_name)` to stage next to output binary.
    pub runtime_copies: Vec<(PathBuf, String)>,
    /// Human-readable audit lines naming each linked dependency/root.
    pub sources: Vec<String>,
}

fn find_project_root(source_file: &Path) -> Option<PathBuf> {
    let abs = if source_file.is_absolute() {
        source_file.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(source_file)
    };
    let mut dir = abs.parent()?.to_path_buf();
    loop {
        if dir.join("spider.toml").is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn normalize_system_lib(raw: &str) -> Result<String, String> {
    validate_ffi_lib_name(raw)?;
    let trimmed = raw.trim();
    if cfg!(windows) {
        if let Some(stem) = trimmed
            .strip_suffix(".dll")
            .or_else(|| trimmed.strip_suffix(".DLL"))
        {
            if !stem.is_empty() {
                return Ok(stem.to_string());
            }
        }
    }
    Ok(trimmed.to_string())
}

fn push_unique(vec: &mut Vec<String>, seen: &mut BTreeSet<String>, val: String) {
    if seen.insert(val.clone()) {
        vec.push(val);
    }
}

fn collect_ffi_from_base(
    owner: &str,
    ffi: &FfiConfig,
    native_root: &Path,
    out: &mut ResolvedFfiLinks,
    seen_libs: &mut BTreeSet<String>,
    seen_rt: &mut BTreeSet<String>,
) -> Result<(), String> {
    let mut attached = Vec::new();
    for raw in &ffi.libs {
        let lib = normalize_system_lib(raw)?;
        attached.push(lib.clone());
        push_unique(&mut out.link_libs, seen_libs, lib);
    }
    for rel in &ffi.bundled {
        validate_native_rel_path(rel)?;
        let full = native_root.join(rel);
        let meta = std::fs::symlink_metadata(&full).map_err(|_| {
            format!(
                "{}: bundled FFI library 'native/{}/{}' is missing ({})",
                owner,
                host_target_triple(),
                rel,
                full.display()
            )
        })?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(format!(
                "{}: refusing symlink or non-file bundled library '{}'",
                owner,
                full.display()
            ));
        }
        attached.push(rel.clone());
        push_unique(
            &mut out.link_libs,
            seen_libs,
            full.to_string_lossy().to_string(),
        );
    }
    for rel in &ffi.runtime_libs {
        validate_native_rel_path(rel)?;
        let full = native_root.join(rel);
        let meta = std::fs::symlink_metadata(&full).map_err(|_| {
            format!(
                "{}: runtime FFI library 'native/{}/{}' is missing ({})",
                owner,
                host_target_triple(),
                rel,
                full.display()
            )
        })?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(format!(
                "{}: refusing symlink or non-file runtime library '{}'",
                owner,
                full.display()
            ));
        }
        let fname = full
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("{}: invalid runtime library filename '{}'", owner, rel))?
            .to_string();
        attached.push(rel.clone());
        if seen_rt.insert(fname.clone()) {
            out.runtime_copies.push((full, fname));
        }
    }
    if !attached.is_empty() {
        out.sources
            .push(format!("{} ({})", owner, attached.join(", ")));
    }
    Ok(())
}

fn collect_declared_ffi_list(ffi: &FfiConfig) -> Vec<String> {
    let mut out = Vec::with_capacity(ffi.libs.len() + ffi.bundled.len() + ffi.runtime_libs.len());
    out.extend(ffi.libs.iter().cloned());
    out.extend(ffi.bundled.iter().cloned());
    out.extend(ffi.runtime_libs.iter().cloned());
    out
}

fn verify_vendor_ffi_entry(
    vendor_dir: &Path,
    entry: &LockEntry,
    triple: &str,
    raw_manifest_bytes: &[u8],
    dep_ffi: &FfiConfig,
) -> Result<(), String> {
    let vmeta = std::fs::symlink_metadata(vendor_dir).map_err(|_| {
        format!(
            "vendor/{} is missing (declared FFI in spider.lock) — run `spkg install`",
            entry.name
        )
    })?;
    if vmeta.file_type().is_symlink() || !vmeta.is_dir() {
        return Err(format!(
            "refusing auto-FFI link for symlinked or non-directory 'vendor/{}'",
            entry.name
        ));
    }
    let (_count, actual_sha) = vendor_src_hash(vendor_dir)?;
    if let Some(expected_sha) = entry.sha256.as_deref() {
        if actual_sha != expected_sha {
            return Err(format!(
                "refusing auto-FFI link for tampered package 'vendor/{}': \
                 src sha256 mismatch (expected {}, got {})",
                entry.name, expected_sha, actual_sha
            ));
        }
    } else if entry.source.starts_with("registry+") {
        return Err(format!(
            "refusing auto-FFI link for unverified registry package 'vendor/{}' \
             (missing sha256 in spider.lock)",
            entry.name
        ));
    }

    if let Some(expected_msha) = entry.manifest_sha256.as_deref() {
        let computed_msha = manifest_and_tree_digest(raw_manifest_bytes, &actual_sha);
        if computed_msha != expected_msha {
            return Err(format!(
                "refusing auto-FFI link for tampered manifest in 'vendor/{}': \
                 manifest-sha256 mismatch (expected {}, got {})",
                entry.name, expected_msha, computed_msha
            ));
        }
    }

    let declared_ffi = collect_declared_ffi_list(dep_ffi);
    if declared_ffi != entry.ffi_libs {
        return Err(format!(
            "refusing auto-FFI link for tampered manifest in 'vendor/{}': \
             .spid3r-manifest.toml [ffi] ({:?}) does not match spider.lock ffi-libs ({:?}) — run `spkg install`",
            entry.name, declared_ffi, entry.ffi_libs
        ));
    }

    let native_dir = vendor_dir.join("_native").join(triple);
    if native_dir.exists() {
        let meta = std::fs::symlink_metadata(&native_dir).map_err(|e| {
            format!(
                "cannot inspect native dir for 'vendor/{}': {}",
                entry.name, e
            )
        })?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(format!(
                "refusing symlinked _native directory in 'vendor/{}'",
                entry.name
            ));
        }
        let expected_native = entry.native_sha256.as_deref().ok_or_else(|| {
            format!(
                "refusing auto-FFI link for 'vendor/{}': _native/{}/ has no native-sha256 pin in spider.lock — run `spkg install`",
                entry.name, triple
            )
        })?;
        let actual_native = native_tree_hash(&native_dir)?;
        if actual_native != expected_native {
            return Err(format!(
                "refusing auto-FFI link for tampered native binaries in 'vendor/{}': \
                 native-sha256 mismatch (expected {}, got {})",
                entry.name, expected_native, actual_native
            ));
        }
    }
    Ok(())
}

/// Collect and cryptographically verify all `[ffi]` libraries declared by the
/// project's root `spider.toml` and its locked `vendor/*` dependencies.
pub fn collect_project_ffi_links(source_file: &Path) -> Result<ResolvedFfiLinks, String> {
    let Some(root) = find_project_root(source_file) else {
        return Ok(ResolvedFfiLinks::default());
    };
    let Some(root_manifest) = Manifest::load(&root)? else {
        return Ok(ResolvedFfiLinks::default());
    };
    let triple = host_target_triple();
    let mut out = ResolvedFfiLinks::default();
    let mut seen_libs = BTreeSet::new();
    let mut seen_rt = BTreeSet::new();

    if !root_manifest.ffi.libs.is_empty()
        || !root_manifest.ffi.bundled.is_empty()
        || !root_manifest.ffi.runtime_libs.is_empty()
    {
        collect_ffi_from_base(
            root_manifest.name(),
            &root_manifest.ffi,
            &root.join("native").join(triple),
            &mut out,
            &mut seen_libs,
            &mut seen_rt,
        )?;
    }

    let lock_entries = read_lock(&root)?;
    let vendor_root = root.join("vendor");
    for entry in &lock_entries {
        valid_pkg_name(&entry.name)?;
        let vendor_dir = vendor_root.join(&entry.name);
        if let Ok(vmeta) = std::fs::symlink_metadata(&vendor_dir) {
            if vmeta.file_type().is_symlink() {
                return Err(format!(
                    "refusing symlinked dependency directory 'vendor/{}'",
                    entry.name
                ));
            }
        }
        let snap_manifest = vendor_dir.join(".spid3r-manifest.toml");
        match std::fs::symlink_metadata(&snap_manifest) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(format!(
                    "refusing symlinked manifest snapshot in 'vendor/{}'",
                    entry.name
                ));
            }
            Ok(m) if m.is_file() => {}
            _ => {
                if !entry.ffi_libs.is_empty() {
                    return Err(format!(
                        "vendor/{} is missing .spid3r-manifest.toml (spider.lock declares ffi-libs) — run `spkg install`",
                        entry.name
                    ));
                }
                continue;
            }
        }
        let raw_manifest_bytes = std::fs::read(&snap_manifest).map_err(|e| {
            format!(
                "cannot read vendored manifest '{}': {}",
                snap_manifest.display(),
                e
            )
        })?;
        let raw_manifest = String::from_utf8(raw_manifest_bytes.clone()).map_err(|e| {
            format!(
                "vendored manifest '{}' is not valid UTF-8: {}",
                snap_manifest.display(),
                e
            )
        })?;
        let dep_manifest = parse_manifest(&raw_manifest).map_err(|e| {
            format!(
                "invalid vendored manifest '{}': {}",
                snap_manifest.display(),
                e
            )
        })?;
        let has_ffi = !dep_manifest.ffi.libs.is_empty()
            || !dep_manifest.ffi.bundled.is_empty()
            || !dep_manifest.ffi.runtime_libs.is_empty();
        if !has_ffi && entry.ffi_libs.is_empty() {
            continue;
        }
        verify_vendor_ffi_entry(
            &vendor_dir,
            entry,
            triple,
            &raw_manifest_bytes,
            &dep_manifest.ffi,
        )?;
        collect_ffi_from_base(
            &format!("vendor/{}", entry.name),
            &dep_manifest.ffi,
            &vendor_dir.join("_native").join(triple),
            &mut out,
            &mut seen_libs,
            &mut seen_rt,
        )?;
    }

    Ok(out)
}
