//! Bounded POSIX ustar archive reader/writer, strict cross-platform path
//! sanitizer (audit S3), and `pack` / `unpack` operations.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use super::hash::{collect_regular_files, tree_hash};
use super::manifest::Manifest;
use super::sha256::sha256_hex;

/// Maximum entries accepted in a single tarball (defends against inode/count DoS).
pub const MAX_TAR_ENTRIES: usize = 10_000;
/// Maximum total unpacked payload size across all entries (128 MiB).
pub const MAX_TAR_TOTAL_BYTES: usize = 128 * 1024 * 1024;
/// Maximum encoded path length per archive entry.
pub const MAX_TAR_PATH_LEN: usize = 512;

/// Validate a tar entry name strictly before writing to disk (audit S3).
/// Allows legitimate filenames containing two dots (`foo..bar.matrix`) while
/// rejecting path traversal (`..` components), Windows drive/UNC/ADS (`:`),
/// backslashes, control bytes, trailing dots/spaces, and Windows reserved device names.
pub fn validate_archive_path(name: &str) -> Result<PathBuf, String> {
    if name.is_empty() || name.len() > MAX_TAR_PATH_LEN {
        return Err(format!("tar: invalid entry path length '{}'", name));
    }
    if name.starts_with('/')
        || name.starts_with('\\')
        || name.contains(':')
        || name.contains('\\')
        || name.chars().any(|c| (c as u32) < 0x20 || c == '\x7f')
    {
        return Err(format!("tar: refusing unsafe path '{}'", name));
    }

    let mut clean = PathBuf::new();
    for comp in Path::new(name).components() {
        match comp {
            Component::Normal(seg) => {
                let s = seg
                    .to_str()
                    .ok_or_else(|| format!("tar: non-UTF8 path segment in '{}'", name))?;
                if s.is_empty()
                    || s.ends_with('.')
                    || s.ends_with(' ')
                    || super::manifest::is_windows_reserved_stem(s)
                {
                    return Err(format!("tar: refusing unsafe/reserved path '{}'", name));
                }
                clean.push(s);
            }
            _ => return Err(format!("tar: refusing unsafe path '{}'", name)),
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(format!("tar: refusing empty path '{}'", name));
    }
    Ok(clean)
}

/// Find a `/` split `(prefix, name)` where `prefix.len() <= 155` and `name.len() <= 100`
/// per POSIX.1-1988 ustar.
fn split_ustar_path(path: &str) -> Option<(&str, &str)> {
    let bytes = path.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'/' {
            let prefix = &path[..i];
            let name = &path[i + 1..];
            if !prefix.is_empty() && prefix.len() <= 155 && !name.is_empty() && name.len() <= 100 {
                return Some((prefix, name));
            }
        }
    }
    None
}

fn write_tar_block(out: &mut Vec<u8>, name: &[u8], prefix: &[u8], typeflag: u8, data: &[u8]) {
    let mut header = [0u8; 512];
    let name_len = name.len().min(100);
    header[..name_len].copy_from_slice(&name[..name_len]);
    if !prefix.is_empty() {
        let p_len = prefix.len().min(155);
        header[345..345 + p_len].copy_from_slice(&prefix[..p_len]);
    }
    put_octal(&mut header[100..], 0o644, 8);
    put_octal(&mut header[108..], 0, 8);
    put_octal(&mut header[116..], 0, 8);
    put_octal(&mut header[124..], data.len() as u64, 12);
    put_octal(&mut header[136..], 0, 12);
    header[156] = typeflag;
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    for b in &mut header[148..156] {
        *b = b' ';
    }
    let sum: u64 = header.iter().map(|b| *b as u64).sum();
    put_octal(&mut header[148..], sum, 8);
    header[155] = b' ';
    out.extend_from_slice(&header);
    out.extend_from_slice(data);
    let pad = (512 - data.len() % 512) % 512;
    out.extend(std::iter::repeat_n(0u8, pad));
}

/// Write a deterministic uncompressed **ustar** archive.
pub fn write_tar(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (path, data) in entries {
        let name_bytes = path.as_bytes();
        if name_bytes.len() <= 100 {
            write_tar_block(&mut out, name_bytes, &[], b'0', data);
        } else if let Some((prefix, name)) = split_ustar_path(path) {
            write_tar_block(&mut out, name.as_bytes(), prefix.as_bytes(), b'0', data);
        } else {
            let mut long_name = Vec::with_capacity(name_bytes.len() + 1);
            long_name.extend_from_slice(name_bytes);
            long_name.push(0);
            write_tar_block(&mut out, b"././@LongLink", &[], b'L', &long_name);
            write_tar_block(&mut out, &name_bytes[..100], &[], b'0', data);
        }
    }
    out.extend(std::iter::repeat_n(0u8, 1024));
    out
}

fn put_octal(field: &mut [u8], value: u64, width: usize) {
    let digits = format!("{:0width$o}", value, width = width - 1);
    let bytes = digits.as_bytes();
    let n = bytes.len().min(width - 1);
    field[..n].copy_from_slice(&bytes[..n]);
    field[n] = 0;
}

fn parse_octal(field: &[u8]) -> Option<u64> {
    let text: String = field
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| *b as char)
        .collect();
    let text = text.trim().trim_end_matches('\0');
    if text.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(text, 8).ok()
}

/// Parse a bounded ustar archive written by [`write_tar`], validating header
/// checksums, entry count, total payload size, and non-symlink entry types.
pub fn read_tar(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut total_bytes = 0usize;
    let mut pending_long_name: Option<String> = None;
    while pos + 512 <= data.len() {
        let header = &data[pos..pos + 512];
        if header.iter().all(|b| *b == 0) {
            break;
        }
        let expected_cksum = parse_octal(&header[148..156])
            .ok_or_else(|| "tar: malformed header checksum".to_string())?;
        let actual_cksum: u64 = header
            .iter()
            .enumerate()
            .map(|(i, &b)| {
                if (148..156).contains(&i) {
                    b' ' as u64
                } else {
                    b as u64
                }
            })
            .sum();
        if expected_cksum != actual_cksum {
            return Err(format!(
                "tar: header checksum mismatch (expected {:o}, got {:o})",
                expected_cksum, actual_cksum
            ));
        }

        let name_end = header[..100].iter().position(|b| *b == 0).unwrap_or(100);
        let raw_name = String::from_utf8_lossy(&header[..name_end]).to_string();
        let prefix_end = header[345..500].iter().position(|b| *b == 0).unwrap_or(155);
        let prefix = String::from_utf8_lossy(&header[345..345 + prefix_end]).to_string();

        let size_u64 = parse_octal(&header[124..136])
            .ok_or_else(|| format!("tar: malformed size field for '{}'", raw_name))?;
        let size = usize::try_from(size_u64)
            .map_err(|_| format!("tar: entry '{}' exceeds platform size limits", raw_name))?;
        total_bytes = total_bytes
            .checked_add(size)
            .ok_or_else(|| "tar: total unpacked byte size overflow".to_string())?;
        if total_bytes > MAX_TAR_TOTAL_BYTES {
            return Err(format!(
                "tar: unpacked archive exceeds maximum of {} bytes",
                MAX_TAR_TOTAL_BYTES
            ));
        }

        let typeflag = header[156];
        pos += 512;
        let end = pos
            .checked_add(size)
            .ok_or_else(|| format!("tar: entry '{}' size overflow", raw_name))?;
        if end > data.len() {
            return Err(format!("tar: entry '{}' is truncated", raw_name));
        }
        let payload = &data[pos..end];
        pos = end + (512 - size % 512) % 512;

        if typeflag == b'L' {
            let len = payload
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(payload.len());
            if len > MAX_TAR_PATH_LEN {
                return Err(format!(
                    "tar: @LongLink entry exceeds {} byte path limit",
                    MAX_TAR_PATH_LEN
                ));
            }
            pending_long_name = Some(String::from_utf8_lossy(&payload[..len]).to_string());
            continue;
        }

        match typeflag {
            b'0' | 0 => {}
            b'5' | b'x' | b'g' => {
                pending_long_name = None;
                continue;
            }
            other => {
                return Err(format!(
                    "tar: entry '{}' has unsupported type '{}' (only regular files and \
                     directories are accepted)",
                    raw_name,
                    if other.is_ascii_graphic() {
                        (other as char).to_string()
                    } else {
                        format!("0x{:02x}", other)
                    }
                ))
            }
        }

        if out.len() >= MAX_TAR_ENTRIES {
            return Err(format!(
                "tar: archive exceeds maximum of {} entries",
                MAX_TAR_ENTRIES
            ));
        }

        let full_name = if let Some(long) = pending_long_name.take() {
            long
        } else if !prefix.is_empty() {
            format!("{}/{}", prefix, raw_name)
        } else {
            raw_name
        };
        out.push((full_name, payload.to_vec()));
    }
    Ok(out)
}

/// Safely extract raw tar bytes into `dest`, validating every path component,
/// Windows reserved stems, bounds, and case-insensitive duplicates (audit S3).
pub fn extract_tar_checked(data: &[u8], dest: &Path) -> Result<Vec<String>, String> {
    let entries = read_tar(data)?;
    let mut seen_lower: HashSet<String> = HashSet::with_capacity(entries.len());
    let mut extracted = Vec::with_capacity(entries.len());
    for (name, bytes) in entries {
        let rel = validate_archive_path(&name)?;
        let fold = name.to_ascii_lowercase();
        if !seen_lower.insert(fold) {
            return Err(format!(
                "tar: refusing duplicate/case-colliding entry '{}'",
                name
            ));
        }
        let target = dest.join(&rel);
        if !target.starts_with(dest) {
            return Err(format!("tar: refusing unsafe path '{}'", name));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {}", parent.display(), e))?;
        }
        std::fs::write(&target, &bytes)
            .map_err(|e| format!("cannot write {}: {}", target.display(), e))?;
        extracted.push(name);
    }
    Ok(extracted)
}

/// Collect files `pack` archives: `spider.toml`, everything under `src/`, and
/// optionally `native/<target>/` for platform wheels (symlink-safe, sorted, `/` separators).
pub fn pack_entries_for_target(
    root: &Path,
    target: Option<&str>,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let _manifest = Manifest::load(root)?.ok_or_else(|| {
        format!(
            "no spider.toml in {} — run `spid3r init` first",
            root.display()
        )
    })?;
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let manifest_path = root.join("spider.toml");
    let mmeta = std::fs::symlink_metadata(&manifest_path)
        .map_err(|e| format!("cannot read spider.toml: {}", e))?;
    if mmeta.file_type().is_symlink() || !mmeta.is_file() {
        return Err("refusing non-regular spider.toml".to_string());
    }
    let manifest_bytes =
        std::fs::read(&manifest_path).map_err(|e| format!("cannot read spider.toml: {}", e))?;
    entries.push(("spider.toml".to_string(), manifest_bytes));

    let src = root.join("src");
    if src.exists() {
        let files = collect_regular_files(&src)?;
        for f in files {
            let rel = f.strip_prefix(root).unwrap_or(&f).to_path_buf();
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let bytes =
                std::fs::read(&f).map_err(|e| format!("cannot read {}: {}", f.display(), e))?;
            entries.push((rel_str, bytes));
        }
    }

    if let Some(triple) = target {
        super::manifest::valid_pkg_name(triple)?;
        let native_dir = root.join("native").join(triple);
        let nmeta = std::fs::symlink_metadata(&native_dir).map_err(|_| {
            format!(
                "wheel target '{}' has no directory at {}",
                triple,
                native_dir.display()
            )
        })?;
        if nmeta.file_type().is_symlink() || !nmeta.is_dir() {
            return Err(format!(
                "refusing symlinked or non-directory native target '{}'",
                native_dir.display()
            ));
        }
        let nfiles = collect_regular_files(&native_dir)?;
        for f in nfiles {
            let rel_under_native = f.strip_prefix(&native_dir).unwrap_or(&f);
            let rel_native_str = rel_under_native.to_string_lossy().replace('\\', "/");
            super::manifest::validate_native_rel_path(&rel_native_str)?;
            let rel_str = format!("native/{}/{}", triple, rel_native_str);
            let bytes =
                std::fs::read(&f).map_err(|e| format!("cannot read {}: {}", f.display(), e))?;
            entries.push((rel_str, bytes));
        }
    }
    Ok(entries)
}

/// Collect files `pack` archives: `spider.toml` + everything under `src/`
/// (symlink-safe, sorted, `/` separators).
pub fn pack_entries(root: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    pack_entries_for_target(root, None)
}

/// `spkg pack [--target TRIPLE]`: write `dist/<name>-<version>[-<target>].tar`
/// and return `(path, file_count, sha256_of_archive)`.
pub fn pack_for_target(
    root: &Path,
    out_override: Option<&Path>,
    target: Option<&str>,
) -> Result<(PathBuf, usize, String), String> {
    let entries = pack_entries_for_target(root, target)?;
    let manifest = Manifest::load(root)?.ok_or("no spider.toml")?;
    let data = write_tar(&entries);
    let hash = sha256_hex(&data);
    if let Ok(tree) = tree_hash(root) {
        println!("sha256(tree): {}", tree);
    }
    let out_path = match out_override {
        Some(p) => p.to_path_buf(),
        None => {
            let dir = root.join("dist");
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
            let fname = match target {
                Some(t) => format!("{}-{}-{}.tar", manifest.name(), manifest.version(), t),
                None => format!("{}-{}.tar", manifest.name(), manifest.version()),
            };
            dir.join(fname)
        }
    };
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.is_dir() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {}", parent.display(), e))?;
        }
    }
    std::fs::write(&out_path, &data)
        .map_err(|e| format!("cannot write {}: {}", out_path.display(), e))?;
    Ok((out_path, entries.len(), hash))
}

/// `spkg pack`: write `dist/<name>-<version>.tar` and return
/// `(path, file_count, sha256_of_archive)`.
pub fn pack(root: &Path, out_override: Option<&Path>) -> Result<(PathBuf, usize, String), String> {
    pack_for_target(root, out_override, None)
}

/// `spkg pack` at a workspace root: pack every member into `<root>/dist/`.
pub fn pack_workspace(
    root: &Path,
    manifest: &Manifest,
) -> Result<Vec<(PathBuf, usize, String)>, String> {
    let dist = root.join("dist");
    std::fs::create_dir_all(&dist)
        .map_err(|e| format!("cannot create {}: {}", dist.display(), e))?;
    let mut out = Vec::new();
    for member in &manifest.members {
        let member_dir = root
            .join(member)
            .canonicalize()
            .map_err(|e| format!("workspace member '{}': cannot resolve: {}", member, e))?;
        let member_manifest = Manifest::load(&member_dir)?
            .ok_or_else(|| format!("workspace member '{}' has no spider.toml", member))?;
        let entries = pack_entries(&member_dir)?;
        let data = write_tar(&entries);
        let hash = sha256_hex(&data);
        let out_path = dist.join(format!(
            "{}-{}.tar",
            member_manifest.name(),
            member_manifest.version()
        ));
        std::fs::write(&out_path, &data)
            .map_err(|e| format!("cannot write {}: {}", out_path.display(), e))?;
        out.push((out_path, entries.len(), hash));
    }
    Ok(out)
}

/// `spkg unpack <archive> [dir]`: extract safely without clobbering `dest`.
pub fn unpack(archive: &Path, dest: &Path) -> Result<Vec<String>, String> {
    if dest.exists() {
        return Err(format!(
            "refusing to unpack: {} already exists",
            dest.display()
        ));
    }
    let data =
        std::fs::read(archive).map_err(|e| format!("cannot read {}: {}", archive.display(), e))?;
    std::fs::create_dir_all(dest)
        .map_err(|e| format!("cannot create {}: {}", dest.display(), e))?;
    match extract_tar_checked(&data, dest) {
        Ok(list) => Ok(list),
        Err(e) => {
            let _ = std::fs::remove_dir_all(dest);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_validation_blocks_reserved_and_allows_double_dot_filenames() {
        assert!(validate_archive_path("src/foo..bar.matrix").is_ok());
        assert!(validate_archive_path("src/../evil.matrix").is_err());
        assert!(validate_archive_path("../evil.matrix").is_err());
        assert!(validate_archive_path("src/CON.matrix").is_err());
        assert!(validate_archive_path("src/nul").is_err());
        assert!(validate_archive_path("src/file.").is_err());
        assert!(validate_archive_path("C:evil.matrix").is_err());
    }

    #[test]
    fn extract_rejects_case_collisions() {
        let tar = write_tar(&[
            ("src/Lib.matrix".to_string(), b"a".to_vec()),
            ("src/lib.matrix".to_string(), b"b".to_vec()),
        ]);
        let dir = std::env::temp_dir().join(format!("spid3r_tar_case_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let err = extract_tar_checked(&tar, &dir).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(err.contains("case-colliding"), "err={err}");
    }
}
