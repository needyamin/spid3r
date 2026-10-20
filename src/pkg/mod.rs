//! `spid3r init` / `spid3r install` — the project and dependency layer.
//!
//! A project is a directory with a `spider.toml` manifest:
//!
//! ```toml
//! [package]
//! name = "myapp"
//! version = "0.1.0"
//! entry = "src/main.matrix"
//!
//! [dependencies]
//! mathlib = { path = "../mathlib" }
//! util = "../util"            # shorthand for a path dependency
//! strlib = { version = "^1.2" }   # a registry dependency (see docs/REGISTRY.md)
//! ```
//!
//! `spid3r install` resolves every dependency (path, git, or registry), copies
//! its `src/` tree into `vendor/<name>/`, and writes `spider.lock` — a
//! deterministic record of each dependency's version and a SHA-256 over its
//! vendored files. Re-running `install` is idempotent; a tampered or stale
//! `vendor/` tree is re-synced.
//!
//! A **registry** dependency (`{ version = "…" }`) is resolved against a local
//! package registry directory named by `SPID3R_REGISTRY` — `<name>/<version>/`
//! package trees — with no network access. See `docs/REGISTRY.md`.
//!
//! The vendored tree is what `import` searches: `vendor` is part of the default
//! module directories, so `import "mathlib/lib"` resolves to
//! `vendor/mathlib/lib.matrix`.
//!
//! No third-party crates are used: the TOML subset and SHA-256 below are
//! self-contained (the project is zero-dependency by design).

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

/// Where a dependency comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum DepSource {
    /// `{ path = "../mathlib" }` or `mathlib = "../mathlib"`.
    Path(String),
    /// `{ git = "https://…", rev = "main" }` — `rev` is a branch, tag, or
    /// commit; omitted means the remote's default branch.
    Git { url: String, rev: Option<String> },
    /// `{ version = "^1.2" }` — resolved from a local package registry (the
    /// directory named by `SPID3R_REGISTRY`, or a named sub-registry via
    /// `registry = "name"`). No network: the registry is a directory of
    /// `<name>/<version>/` package trees. See `docs/REGISTRY.md`.
    Registry { version: String, registry: Option<String> },
}

impl DepSource {
    /// Human-readable origin for messages.
    pub fn describe(&self) -> String {
        match self {
            DepSource::Path(p) => p.clone(),
            DepSource::Git { url, rev } => match rev {
                Some(r) => format!("{}#{}", url, r),
                None => url.clone(),
            },
            DepSource::Registry { version, registry } => match registry {
                Some(r) => format!("registry:{}@{}", r, version),
                None => format!("registry@{}", version),
            },
        }
    }
}

/// A version constraint declared on a dependency (`version = "^1.2"`).
#[derive(Debug, Clone, PartialEq)]
pub enum Constraint {
    /// `^x.y.z`: same major, at least the given minor/patch.
    Caret(Version),
    /// `>=x.y.z`.
    AtLeast(Version),
    /// `=x.y.z`: exactly this version.
    Exact(Version),
}

impl Constraint {
    /// Parse `^1.2.3`, `>=1.2`, `=1.2.3`, or a bare `1.2.3` (exact).
    pub fn parse(text: &str) -> Result<Constraint, String> {
        let text = text.trim();
        if let Some(rest) = text.strip_prefix('^') {
            return Ok(Constraint::Caret(Version::parse(rest)?));
        }
        if let Some(rest) = text.strip_prefix(">=") {
            return Ok(Constraint::AtLeast(Version::parse(rest)?));
        }
        let (op, rest) = match text.strip_prefix('=') {
            Some(rest) => ("=", rest),
            None => ("=", text),
        };
        let v = Version::parse(rest)?;
        debug_assert_eq!(op, "=");
        Ok(Constraint::Exact(v))
    }

    /// Does `v` satisfy this constraint?
    pub fn matches(&self, v: &Version) -> bool {
        match self {
            Constraint::Exact(want) => v == want,
            Constraint::AtLeast(want) => v >= want,
            Constraint::Caret(want) => {
                v.major == want.major
                    && (v.minor, v.patch) >= (want.minor, want.patch)
            }
        }
    }
}

/// A `major.minor.patch` version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// Parse `1.2.3` or `1.2` (= 1.2.0).
    pub fn parse(text: &str) -> Result<Version, String> {
        let parts: Vec<&str> = text.trim().split('.').collect();
        if parts.is_empty() || parts.len() > 3 {
            return Err(format!("malformed version '{}'", text));
        }
        let mut nums = [0u64; 3];
        for (i, p) in parts.iter().enumerate() {
            nums[i] = p
                .parse()
                .map_err(|_| format!("malformed version '{}' (component '{}')", text, p))?;
        }
        Ok(Version {
            major: nums[0],
            minor: nums[1],
            patch: nums[2],
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// One declared dependency: its name, source, and optional version constraint.
#[derive(Debug, Clone, PartialEq)]
pub struct Dependency {
    pub name: String,
    pub source: DepSource,
    pub version: Option<String>,
}

/// A parsed `spider.toml`.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    /// `None` for a workspace-only root (a manifest with `[workspace]` but no
    /// `[package]`).
    pub package: Option<PackageInfo>,
    /// Workspace members, when this manifest declares a workspace.
    pub members: Vec<String>,
    /// Dependencies in declaration order (empty for a workspace-only root).
    pub deps: Vec<Dependency>,
}

/// The `[package]` section.
#[derive(Debug, Clone, PartialEq)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    /// Entry file relative to the project root.
    pub entry: String,
}

impl Manifest {
    /// The `[package]` entry path (empty for a workspace-only root).
    pub fn entry(&self) -> &str {
        self.package
            .as_ref()
            .map(|p| p.entry.as_str())
            .unwrap_or("src/main.matrix")
    }

    /// The `[package]` name (empty for a workspace-only root).
    pub fn name(&self) -> &str {
        self.package
            .as_ref()
            .map(|p| p.name.as_str())
            .unwrap_or("")
    }

    /// The `[package]` version (empty for a workspace-only root).
    pub fn version(&self) -> &str {
        self.package
            .as_ref()
            .map(|p| p.version.as_str())
            .unwrap_or("")
    }

    /// The manifest for `dir` if one exists.
    pub fn load(dir: &Path) -> Result<Option<Manifest>, String> {
        let path = dir.join("spider.toml");
        if !path.is_file() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
        let m = parse_manifest(&text).map_err(|e| format!("{}: {}", path.display(), e))?;
        Ok(Some(m))
    }
}

/// Parse the manifest subset: `[section]` headers, `key = "string"` values,
/// inline tables (`{ path = "..." }`), `#` comments, blank lines.
pub fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let mut section = String::new();
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut entry: Option<String> = None;
    let mut deps: Vec<Dependency> = Vec::new();
    let mut members: Vec<String> = Vec::new();

    for (lineno, raw) in text.lines().enumerate() {
        let line = strip_comment(raw).trim().to_string();
        if line.is_empty() {
            continue;
        }
        let where_ = format!("line {}", lineno + 1);
        if let Some(rest) = line.strip_prefix('[') {
            let Some(sect) = rest.strip_suffix(']') else {
                return Err(format!("{}: unterminated section header", where_));
            };
            section = sect.trim().to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("{}: expected `key = value`", where_));
        };
        let key = key.trim().to_string();
        let value = value.trim();
        match section.as_str() {
            "package" => {
                let v = parse_string(value).ok_or_else(|| {
                    format!("{}: package.{} expects a quoted string", where_, key)
                })?;
                match key.as_str() {
                    "name" => name = Some(v),
                    "version" => version = Some(v),
                    "entry" => entry = Some(v),
                    other => return Err(format!("{}: unknown package key '{}'", where_, other)),
                }
            }
            "dependencies" => {
                let (source, version) = if value.starts_with('{') {
                    let table = parse_inline_table(value).ok_or_else(|| {
                        format!("{}: dependency '{}' has a malformed table", where_, key)
                    })?;
                    let version = table.get("version").cloned();
                    if let Some(v) = &version {
                        Constraint::parse(v).map_err(|e| {
                            format!("{}: dependency '{}': {}", where_, key, e)
                        })?;
                    }
                    let source = match (table.get("path"), table.get("git")) {
                        (Some(p), None) => DepSource::Path(p.clone()),
                        (None, Some(url)) => DepSource::Git {
                            url: url.clone(),
                            rev: table.get("rev").cloned(),
                        },
                        (Some(_), Some(_)) => {
                            return Err(format!(
                                "{}: dependency '{}' cannot have both `path` and `git`",
                                where_, key
                            ))
                        }
                        (None, None) => match &version {
                            // A version-only table is a registry dependency.
                            Some(v) => DepSource::Registry {
                                version: v.clone(),
                                registry: table.get("registry").cloned(),
                            },
                            None => {
                                return Err(format!(
                                    "{}: dependency '{}' needs `path`, `git`, or `version`",
                                    where_, key
                                ))
                            }
                        },
                    };
                    (source, version)
                } else {
                    let source = DepSource::Path(parse_string(value).ok_or_else(|| {
                        format!("{}: dependency '{}' expects a path string", where_, key)
                    })?);
                    // The shorthand form carries no constraint.
                    (source, None)
                };
                if deps.iter().any(|d| d.name == key) {
                    return Err(format!("{}: duplicate dependency '{}'", where_, key));
                }
                deps.push(Dependency {
                    name: key,
                    source,
                    version,
                });
            }
            "workspace" => match key.as_str() {
                "members" => {
                    members = parse_string_array(value).ok_or_else(|| {
                        format!("{}: workspace.members expects [\"a\", \"b\"]", where_)
                    })?;
                }
                other => {
                    return Err(format!("{}: unknown workspace key '{}'", where_, other))
                }
            },
            "" => return Err(format!("{}: value outside of a section", where_)),
            other => return Err(format!("{}: unknown section '{}'", where_, other)),
        }
    }

    let package = match name {
        Some(name) => Some(PackageInfo {
            name,
            version: version.unwrap_or_else(|| "0.0.0".to_string()),
            entry: entry.unwrap_or_else(|| "src/main.matrix".to_string()),
        }),
        None if !members.is_empty() => None, // workspace-only root
        None => return Err("[package] name is required".to_string()),
    };
    Ok(Manifest {
        package,
        members,
        deps,
    })
}

/// `["a", "b"]` → Vec<String>.
fn parse_string_array(v: &str) -> Option<Vec<String>> {
    let inner = v.trim().strip_prefix('[')?.strip_suffix(']')?;
    let mut out = Vec::new();
    for part in inner.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        out.push(parse_string(part)?);
    }
    Some(out)
}

fn strip_comment(line: &str) -> &str {
    // A `#` outside a quoted string starts a comment.
    let mut in_str = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '#' if !in_str => return &line[..i],
            _ => {}
        }
    }
    line
}

/// A double-quoted string value, with `\"` and `\\` escapes.
fn parse_string(v: &str) -> Option<String> {
    let v = v.trim();
    let inner = v.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                _ => return None,
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// `{ a = "x", b = "y" }` → map of string values.
fn parse_inline_table(v: &str) -> Option<BTreeMap<String, String>> {
    let inner = v.trim().strip_prefix('{')?.strip_suffix('}')?;
    let mut out = BTreeMap::new();
    for part in inner.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (k, val) = part.split_once('=')?;
        out.insert(k.trim().to_string(), parse_string(val)?);
    }
    Some(out)
}

// ---------------------------------------------------------------- tar (ustar)

/// Find a `/` split `(prefix, name)` where `prefix.len() <= 155` and `name.len() <= 100`
/// per POSIX.1-1988 ustar.
fn split_ustar_path(path: &str) -> Option<(&str, &str)> {
    let bytes = path.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'/' {
            let prefix = &path[..i];
            let name = &path[i + 1..];
            if !prefix.is_empty()
                && prefix.len() <= 155
                && !name.is_empty()
                && name.len() <= 100
            {
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
    put_octal(&mut header[100..], 0o644, 8); // mode
    put_octal(&mut header[108..], 0, 8); // uid
    put_octal(&mut header[116..], 0, 8); // gid
    put_octal(&mut header[124..], data.len() as u64, 12); // size
    put_octal(&mut header[136..], 0, 12); // mtime (deterministic archives)
    header[156] = typeflag;
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    // Checksum: sum of all header bytes with the checksum field as spaces.
    for b in &mut header[148..156] {
        *b = b' ';
    }
    let sum: u64 = header.iter().map(|b| *b as u64).sum();
    put_octal(&mut header[148..], sum, 8);
    header[155] = b' '; // checksum is space-terminated
    out.extend_from_slice(&header);
    out.extend_from_slice(data);
    let pad = (512 - data.len() % 512) % 512;
    out.extend(std::iter::repeat_n(0u8, pad));
}

/// Write an uncompressed **ustar** archive: one entry per `(path, contents)`,
/// plus the two zero blocks that end a tar stream. Paths up to 100 bytes fit in
/// `name`; paths up to 255 bytes with a directory separator use the POSIX ustar
/// `prefix` field (`header[345..500]`); even longer paths emit a GNU tar
/// `././@LongLink` (`typeflag = 'L'`) metadata block so names are never truncated.
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
    out.extend(std::iter::repeat_n(0u8, 1024)); // end-of-archive marker
    out
}

fn put_octal(field: &mut [u8], value: u64, width: usize) {
    let digits = format!("{:0width$o}", value, width = width - 1);
    let bytes = digits.as_bytes();
    let n = bytes.len().min(width - 1);
    field[..n].copy_from_slice(&bytes[..n]);
    field[n] = 0;
}

/// Parse a tar written by [`write_tar`] (verifies header checksums and supports
/// both POSIX ustar `prefix` and GNU `@LongLink` long filenames).
pub fn read_tar(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut pending_long_name: Option<String> = None;
    while pos + 512 <= data.len() {
        let header = &data[pos..pos + 512];
        if header.iter().all(|b| *b == 0) {
            break; // end-of-archive
        }
        // Verify ustar header checksum (bytes 148..156 treated as ASCII spaces).
        let expected_cksum = parse_octal(&header[148..156])
            .ok_or_else(|| "tar: malformed header checksum".to_string())?;
        let actual_cksum: u64 = header
            .iter()
            .enumerate()
            .map(|(i, &b)| if (148..156).contains(&i) { b' ' as u64 } else { b as u64 })
            .sum();
        if expected_cksum != actual_cksum {
            return Err(format!(
                "tar: header checksum mismatch (expected {:o}, got {:o})",
                expected_cksum, actual_cksum
            ));
        }

        let name_end = header[..100].iter().position(|b| *b == 0).unwrap_or(100);
        let raw_name = String::from_utf8_lossy(&header[..name_end]).to_string();
        let prefix_end = header[345..500]
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(155);
        let prefix = String::from_utf8_lossy(&header[345..345 + prefix_end]).to_string();

        let size = parse_octal(&header[124..136])
            .ok_or_else(|| format!("tar: malformed size field for '{}'", raw_name))?;
        let typeflag = header[156];
        pos += 512;
        let end = pos
            .checked_add(size as usize)
            .ok_or_else(|| format!("tar: entry '{}' size overflow", raw_name))?;
        if end > data.len() {
            return Err(format!("tar: entry '{}' is truncated", raw_name));
        }
        let payload = &data[pos..end];
        pos = end + (512 - size as usize % 512) % 512;

        if typeflag == b'L' {
            let len = payload.iter().position(|&b| b == 0).unwrap_or(payload.len());
            pending_long_name = Some(String::from_utf8_lossy(&payload[..len]).to_string());
            continue;
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

// ---------------------------------------------------------------- pack/unpack

/// Collect the files `pack` archives: `spider.toml` plus everything under
/// `src/`, sorted, with `/` separators regardless of platform.
pub fn pack_entries(root: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    let manifest = Manifest::load(root)?.ok_or_else(|| {
        format!("no spider.toml in {} — run `spid3r init` first", root.display())
    })?;
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let manifest_bytes = std::fs::read(root.join("spider.toml"))
        .map_err(|e| format!("cannot read spider.toml: {}", e))?;
    entries.push(("spider.toml".to_string(), manifest_bytes));
    let src = root.join("src");
    if src.is_dir() {
        let mut files = Vec::new();
        collect_all_files(&src, &mut files)?;
        files.sort();
        for f in files {
            let rel = f.strip_prefix(root).unwrap_or(&f).to_path_buf();
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let bytes = std::fs::read(&f)
                .map_err(|e| format!("cannot read {}: {}", f.display(), e))?;
            entries.push((rel_str, bytes));
        }
    }
    let _ = manifest;
    Ok(entries)
}

fn collect_all_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read {}: {}", dir.display(), e))?;
    for e in entries {
        let p = e
            .map_err(|e| format!("cannot read {}: {}", dir.display(), e))?
            .path();
        if p.is_dir() {
            collect_all_files(&p, out)?;
        } else {
            out.push(p);
        }
    }
    Ok(())
}

/// Every file under `dir`, recursively (any extension).
///
/// Vendoring and hashing must cover the *whole* `src/` tree, not just
/// `*.matrix`: a dependency can ship data files or binary assets that the
/// program reads, and anything that lands in `vendor/` must be covered by the
/// lockfile hash (audit A-13: the old filter hashed only `.matrix` files, so a
/// non-`.matrix` payload was neither copied nor verified).
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {}", dir.display(), e))?;
    for e in entries {
        let p = e.map_err(|e| format!("cannot read {}: {}", dir.display(), e))?.path();
        if p.is_dir() {
            collect_files(&p, out)?;
        } else {
            out.push(p);
        }
    }
    Ok(())
}

/// Hash a file list the way [`vendor_dep`] does: for each file in sorted order,
/// `relative/path` (bytes, `/` separators) `0x00` raw contents `0x00`, hashed as
/// one byte stream.
///
/// Raw bytes, not a lossy `String`: the old construction pushed
/// `String::from_utf8_lossy(contents)`, so two different binary payloads (and
/// any non-UTF-8 source) hashed identically, and the lockfile could not detect
/// a changed asset (audit A-13).
fn hash_files(src_dir: &Path, files: &[PathBuf]) -> Result<String, String> {
    let mut input: Vec<u8> = Vec::new();
    for f in files {
        let rel = f.strip_prefix(src_dir).unwrap_or(f);
        input.extend_from_slice(rel.to_string_lossy().replace('\\', "/").as_bytes());
        input.push(0);
        let bytes = std::fs::read(f).map_err(|e| format!("cannot read {}: {}", f.display(), e))?;
        input.extend_from_slice(&bytes);
        input.push(0);
    }
    Ok(sha256_hex(&input))
}

/// SHA-256 over a package's `src/` tree using the exact construction the
/// installer hashes vendored trees with (`path\0contents\0` per file, paths
/// normalized to `/`, raw bytes). This is the hash `sha256.txt` must contain.
pub fn tree_hash(root: &Path) -> Result<String, String> {
    let src_dir = root.join("src");
    if !src_dir.is_dir() {
        return Err(format!("no src/ directory in {}", root.display()));
    }
    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(&src_dir, &mut files)?;
    files.sort();
    hash_files(&src_dir, &files)
}

/// `spid3r pack`: write `dist/<name>-<version>.tar` and return
/// (path, file count, sha256 of the archive).
pub fn pack(root: &Path, out_override: Option<&Path>) -> Result<(PathBuf, usize, String), String> {
    let entries = pack_entries(root)?;
    let manifest = Manifest::load(root)?.ok_or("no spider.toml")?;
    let data = write_tar(&entries);
    let hash = sha256_hex(&data);
    // The registry's sha256.txt is checked against the VENDORED TREE hash,
    // not the archive hash. Print both so the documented workflow (paste the
    // printed hash into dist/sha256.txt) actually works.
    if let Ok(tree) = tree_hash(root) {
        println!("sha256(tree): {}", tree);
    }
    let out_path = match out_override {
        Some(p) => p.to_path_buf(),
        None => {
            let dir = root.join("dist");
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
            dir.join(format!("{}-{}.tar", manifest.name(), manifest.version()))
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

/// `spid3r pack` at a **workspace root**: pack every member into the root's
/// `dist/` as `<member-name>-<version>.tar`. Returns (path, files, sha256) per
/// member.
pub fn pack_workspace(root: &Path, manifest: &Manifest) -> Result<Vec<(PathBuf, usize, String)>, String> {
    let dist = root.join("dist");
    std::fs::create_dir_all(&dist)
        .map_err(|e| format!("cannot create {}: {}", dist.display(), e))?;
    let mut out = Vec::new();
    for member in &manifest.members {
        let member_dir = root
            .join(member)
            .canonicalize()
            .map_err(|e| format!("workspace member '{}': cannot resolve: {}", member, e))?;
        let member_manifest = Manifest::load(&member_dir)?.ok_or_else(|| {
            format!("workspace member '{}' has no spider.toml", member)
        })?;
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

/// `spid3r unpack <archive> [dir]`: extract into `dir` (refuses to clobber an
/// existing directory). Returns the extracted paths.
pub fn unpack(archive: &Path, dest: &Path) -> Result<Vec<String>, String> {
    if dest.exists() {
        return Err(format!(
            "refusing to unpack: {} already exists",
            dest.display()
        ));
    }
    let data = std::fs::read(archive)
        .map_err(|e| format!("cannot read {}: {}", archive.display(), e))?;
    let entries = read_tar(&data)?;
    let mut extracted = Vec::new();
    for (name, bytes) in entries {
        // Path traversal guard: reject anything that escapes the destination.
        // `:` is rejected outright: on Windows it introduces a drive prefix or
        // an alternate data stream, both of which make `Path::join` discard
        // `dest` (arbitrary-write primitive from a hostile archive).
        if name.contains("..")
            || name.contains(':')
            || name.starts_with('/')
            || name.starts_with('\\')
        {
            return Err(format!("tar: refusing unsafe path '{}'", name));
        }
        let target = dest.join(&name);
        // Defense in depth: the final path must still live under `dest`.
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

// ------------------------------------------------------------------ sha256
//
// Implementation lives in `sha256.rs` because `build.rs` includes the same file
// (a build script cannot depend on the crate it builds).

#[path = "sha256.rs"]
mod sha256;
pub use sha256::{hex_encode, sha256_bytes, sha256_hex};

// ---------------------------------------------------------------- install

/// What `install` did, for the CLI summary.
#[derive(Debug, Default)]
pub struct InstallReport {
    pub deps: Vec<String>,
    pub vendored: usize,
    pub unchanged: usize,
}

/// A package/dependency name becomes a path component under `vendor/`,
/// `.spid3r/git/` and `dist/`. Anything outside this allowlist is rejected so
/// a hostile manifest cannot escape those directories (a `..` key used to make
/// `install` delete the project root via `remove_dir_all`).
pub fn valid_pkg_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        && !name.starts_with('.');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid package name '{}' (letters, digits, '_', '-', '.'; no path separators)",
            name
        ))
    }
}

/// Copy a dependency's sources into `vendor/<name>/` and return
/// (files copied, content hash) — the hash covers the vendored tree.
fn vendor_dep(root: &Path, name: &str, dep_dir: &Path) -> Result<(usize, String), String> {
    valid_pkg_name(name)?;
    let src_dir = dep_dir.join("src");
    if !src_dir.is_dir() {
        return Err(format!(
            "dependency '{}' has no src/ directory ({})",
            name,
            dep_dir.display()
        ));
    }
    let dest = root.join("vendor").join(name);
    // Defense in depth: the delete below must never leave `vendor/`.
    let vendor_root = root.join("vendor");
    if !dest.starts_with(&vendor_root) || dest == vendor_root {
        return Err(format!("refusing to vendor outside vendor/: {}", dest.display()));
    }
    // Start clean so removed files do not linger.
    if dest.exists() {
        std::fs::remove_dir_all(&dest)
            .map_err(|e| format!("cannot clear {}: {}", dest.display(), e))?;
    }
    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(&src_dir, &mut files)?;
    files.sort();

    for f in &files {
        let rel = f.strip_prefix(&src_dir).unwrap_or(f).to_path_buf();
        let target = dest.join(&rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {}", parent.display(), e))?;
        }
        let bytes = std::fs::read(f).map_err(|e| format!("cannot read {}: {}", f.display(), e))?;
        std::fs::write(&target, &bytes)
            .map_err(|e| format!("cannot write {}: {}", target.display(), e))?;
    }
    // Deterministic hash input: relative path + NUL + raw contents + NUL.
    let hash = hash_files(&src_dir, &files)?;
    Ok((files.len(), hash))
}

/// Vendor a dependency and, for a registry package that ships a `sha256.txt`
/// (the expected content hash), verify the vendored tree against it.
fn vendor_dep_checked(
    root: &Path,
    name: &str,
    dep_dir: &Path,
    source: &DepSource,
) -> Result<(usize, String), String> {
    let (files, hash) = vendor_dep(root, name, dep_dir)?;
    if matches!(source, DepSource::Registry { .. }) {
        if let Ok(expected) = std::fs::read_to_string(dep_dir.join("sha256.txt")) {
            let expected = expected.trim();
            if !expected.is_empty() && expected != hash {
                return Err(format!(
                    "registry dependency '{}': checksum mismatch (package declares {}, \
                     vendored tree hashes to {})",
                    name, expected, hash
                ));
            }
        }
    }
    Ok((files, hash))
}

/// Resolve a dependency's source to its on-disk directory, returning
/// `(dep_dir, origin)` where `origin` is the string recorded in `spider.lock`.
/// `base_dir` is what `path`/`git` deps resolve relative to (the project root,
/// or a workspace member's directory). `locked` is the previous lockfile and
/// `update` drops its git pins (re-resolve the upstream tip).
fn resolve_source(
    root: &Path,
    base_dir: &Path,
    name: &str,
    source: &DepSource,
    locked: &BTreeMap<String, String>,
    update: bool,
) -> Result<(PathBuf, String), String> {
    match source {
        DepSource::Path(rel) => {
            let dir = base_dir
                .join(rel)
                .canonicalize()
                .map_err(|e| format!("dependency '{}': cannot resolve '{}': {}", name, rel, e))?;
            Ok((dir, format!("path+{}", rel)))
        }
        DepSource::Git { url, rev } => {
            let pin = if update {
                None
            } else {
                locked_commit(locked, name, url)
            };
            let (dir, commit) = fetch_git(root, name, url, rev.as_deref(), pin.as_deref())?;
            Ok((dir, format!("git+{}#{}", url, commit)))
        }
        DepSource::Registry { version, registry } => {
            let dir = resolve_registry(name, version, registry.as_deref())?;
            let resolved = dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| version.clone());
            Ok((dir, format!("registry+{}", resolved)))
        }
    }
}

/// The registry base directory: `$SPID3R_REGISTRY`, or a named sub-registry
/// `$SPID3R_REGISTRY/<name>`. `None` when the env var is unset.
fn registry_root(registry: Option<&str>) -> Option<PathBuf> {
    let base = std::env::var_os("SPID3R_REGISTRY").map(PathBuf::from)?;
    Some(match registry {
        Some(name) => base.join(name),
        None => base,
    })
}

/// Resolve `name` at a version satisfying `version` (a constraint like `^1.2`)
/// inside the registry, picking the highest matching `<version>/` directory.
fn resolve_registry(name: &str, version: &str, registry: Option<&str>) -> Result<PathBuf, String> {
    let constraint =
        Constraint::parse(version).map_err(|e| format!("dependency '{}': {}", name, e))?;
    let root = registry_root(registry).ok_or_else(|| {
        format!(
            "dependency '{}' is a registry dependency, but SPID3R_REGISTRY is not set \
             (point it at a package registry directory; see docs/REGISTRY.md)",
            name
        )
    })?;
    let pkg_dir = root.join(name);
    if !pkg_dir.is_dir() {
        return Err(format!(
            "registry: no package '{}' (looked in {})",
            name,
            pkg_dir.display()
        ));
    }
    let entries = std::fs::read_dir(&pkg_dir)
        .map_err(|e| format!("registry: cannot read {}: {}", pkg_dir.display(), e))?;
    let mut best: Option<(Version, PathBuf)> = None;
    for entry in entries {
        let path = entry
            .map_err(|e| format!("registry: cannot read {}: {}", pkg_dir.display(), e))?
            .path();
        if !path.is_dir() {
            continue;
        }
        let Some(dirname) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(v) = Version::parse(dirname) else {
            continue;
        };
        if constraint.matches(&v) && best.as_ref().is_none_or(|(bv, _)| v > *bv) {
            best = Some((v, path));
        }
    }
    match best {
        Some((_, dir)) => Ok(dir),
        None => Err(format!(
            "registry: no version of '{}' matches {} (looked in {})",
            name,
            version,
            pkg_dir.display()
        )),
    }
}

/// Allowed git transports for a manifest-controlled URL (audit A-5).
///
/// `git clone` treats some URLs as *commands*: `ext::<cmd>` runs `<cmd>` through
/// the shell (the CVE-2022-25912 class), and a leading `-` can smuggle options
/// such as `--upload-pack=<cmd>`. A dependency URL is attacker-controlled input
/// (it arrives with a cloned repository), so only real network transports are
/// accepted by default:
///
/// * `https://` and `ssh://` — authenticated, integrity-protected
/// * scp-like `user@host:path` — the usual git-over-ssh form
///
/// Opt-in escapes, for local development only:
/// * `SPID3R_ALLOW_LOCAL_GIT_DEPS=1` — bare paths and `file://` URLs
/// * `SPID3R_ALLOW_INSECURE_GIT=1` — `http://` and `git://` (no integrity)
///
/// `ext::` is rejected **unconditionally**, including under those variables:
/// no legitimate package URL runs a shell command, and the opt-in would have to
/// be set by the very developer who is about to clone the hostile manifest.
pub fn validate_git_url(name: &str, url: &str) -> Result<(), String> {
    let reject = |why: &str| {
        Err(format!(
            "git dependency '{}': {} ('{}'). Allowed: https:// and ssh:// (scp-like \
             user@host:path); local paths need SPID3R_ALLOW_LOCAL_GIT_DEPS=1, plain \
             http/git need SPID3R_ALLOW_INSECURE_GIT=1",
            name, why, url
        ))
    };
    if url.is_empty() {
        return reject("empty url");
    }
    if url.starts_with('-') {
        return reject("url may not start with '-'");
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return reject("url contains whitespace or control characters");
    }
    let lower = url.to_ascii_lowercase();
    // git's "external command" transport. Only a *leading* `ext::` is a
    // transport prefix — `https://host/ext::x` is an ordinary https URL, and
    // IPv6 literals (`https://[::1]/repo`) legitimately contain `::`.
    if lower.starts_with("ext::") {
        return Err(format!(
            "git dependency '{}': refusing the ext:: transport, which runs an arbitrary \
             command during install ('{}')",
            name, url
        ));
    }
    if lower.starts_with("https://") {
        return Ok(());
    }
    if lower.starts_with("ssh://") {
        // A host beginning with '-' would be read by OpenSSH as an option
        // (CVE-2017-1000117 class), so it is not a host at all.
        let authority = url.get(6..).unwrap_or("").split('/').next().unwrap_or("");
        let host = authority.rsplit('@').next().unwrap_or(authority);
        if host.starts_with('-') || host.is_empty() {
            return reject("ssh host is missing or starts with '-'");
        }
        return Ok(());
    }
    if lower.starts_with("http://") || lower.starts_with("git://") {
        return if env_flag("SPID3R_ALLOW_INSECURE_GIT") {
            Ok(())
        } else {
            reject("plaintext transport has no integrity protection")
        };
    }
    if lower.starts_with("file://") {
        return if env_flag("SPID3R_ALLOW_LOCAL_GIT_DEPS") {
            Ok(())
        } else {
            reject("file:// reads the local filesystem")
        };
    }
    if lower.contains("://") {
        return reject("unsupported url scheme");
    }
    // Windows drive path (C:\repo or C:/repo) is a local path, not scp syntax.
    let bytes = url.as_bytes();
    let is_drive = bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic();
    let is_scp_like = !is_drive
        && url
            .split_once(':')
            .map(|(host, path)| {
                !host.is_empty()
                    && !host.contains('/')
                    && !host.contains('\\')
                    && !path.is_empty()
                    // `user@-host:path` would hand OpenSSH an option.
                    && !host.rsplit('@').next().unwrap_or(host).starts_with('-')
            })
            .unwrap_or(false);
    if is_scp_like {
        return Ok(());
    }
    // Anything left is a bare local path (absolute, relative, or drive letter).
    if env_flag("SPID3R_ALLOW_LOCAL_GIT_DEPS") {
        Ok(())
    } else {
        reject("local path")
    }
}

/// Is the named opt-in environment variable set to an affirmative value?
fn env_flag(key: &str) -> bool {
    matches!(
        std::env::var(key)
            .map(|v| v.trim().to_ascii_lowercase())
            .ok()
            .as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

/// The commit a previous install recorded for `name`, when the manifest still
/// points at the same URL. `spider.lock` is the reproducibility contract: a
/// re-install must reproduce the locked tree instead of tracking a moving
/// branch or tag (audit A-13).
fn locked_commit(
    locked: &BTreeMap<String, String>,
    name: &str,
    url: &str,
) -> Option<String> {
    let source = locked.get(name)?;
    let rest = source.strip_prefix("git+")?;
    let (locked_url, commit) = rest.rsplit_once('#')?;
    if locked_url != url {
        return None;
    }
    let commit = commit.trim();
    let ok = commit.len() == 40 && commit.chars().all(|c| c.is_ascii_hexdigit());
    ok.then(|| commit.to_ascii_lowercase())
}

/// Parse the `[[package]]` blocks of `spider.lock` into `name -> source`.
fn read_locked_sources(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(root.join("spider.lock")) else {
        return out;
    };
    let mut name: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            name = None;
            continue;
        }
        if let Some(rest) = line.strip_prefix("name = ") {
            name = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("source = ") {
            if let Some(n) = name.take() {
                out.insert(n, rest.trim().trim_matches('"').to_string());
            }
        }
    }
    out
}

/// Clone (or update) a git dependency into `.spid3r/git/<name>` and return
/// (checkout dir, full commit hash).
///
/// The URL must pass [`validate_git_url`]. When `locked` holds a commit, that
/// exact commit is checked out (and verified) so a re-install reproduces the
/// tree recorded in `spider.lock`; `--update` drops the pin and re-resolves.
fn fetch_git(
    root: &Path,
    name: &str,
    url: &str,
    rev: Option<&str>,
    locked: Option<&str>,
) -> Result<(PathBuf, String), String> {
    valid_pkg_name(name)?;
    validate_git_url(name, url)?;
    if let Some(r) = rev {
        let safe = !r.is_empty()
            && !r.starts_with('-')
            && r.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
        if !safe {
            return Err(format!(
                "git dependency '{}': unsafe revision '{}'",
                name, r
            ));
        }
    }
    let dir = root.join(".spid3r").join("git").join(name);
    if dir.join(".git").is_dir() {
        run_git(&["-C", &path_str(&dir), "fetch", "--all", "--tags", "--quiet"])?;
    } else {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {}", parent.display(), e))?;
        }
        run_git(&["clone", "--quiet", "--", url, &path_str(&dir)])?;
    }
    if let Some(commit) = locked {
        run_git(&["-C", &path_str(&dir), "checkout", "--quiet", commit]).map_err(|e| {
            format!(
                "git dependency '{}': the commit {} recorded in spider.lock is not \
                 available ({}). If upstream rewrote its history, run \
                 `spid3r install --update` to re-resolve it.",
                name, commit, e
            )
        })?;
    } else if let Some(rev) = rev {
        run_git(&["-C", &path_str(&dir), "checkout", "--quiet", rev])?;
    } else {
        // Default-branch tip. A previous pinned install left HEAD detached at
        // the locked commit, so a bare `pull` would stay there: move to the
        // fetched remote tip explicitly (`install --update` and first installs
        // both take this path).
        let target = remote_default_branch(&dir)
            .map(|b| format!("origin/{}", b))
            .unwrap_or_else(|| "origin/HEAD".to_string());
        if run_git(&["-C", &path_str(&dir), "checkout", "--quiet", "--detach", &target]).is_err() {
            // Very old/odd clone: fall back to a fast-forward pull.
            let _ = run_git(&["-C", &path_str(&dir), "pull", "--quiet", "--ff-only"]);
        }
    }
    let out = run_git(&["-C", &path_str(&dir), "rev-parse", "HEAD"])?;
    let head = out.trim().to_ascii_lowercase();
    if let Some(commit) = locked {
        if head != commit {
            return Err(format!(
                "git dependency '{}': checked out {} but spider.lock pins {}; refusing to \
                 vendor a different tree (run `spid3r install --update` to re-resolve)",
                name, head, commit
            ));
        }
    }
    Ok((dir, head))
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

/// The remote's default branch name (`main`, `master`, ...), from
/// `refs/remotes/origin/HEAD`.
fn remote_default_branch(dir: &Path) -> Option<String> {
    let out = run_git(&[
        "-C",
        &path_str(dir),
        "symbolic-ref",
        "--short",
        "refs/remotes/origin/HEAD",
    ])
    .ok()?;
    let name = out.trim().strip_prefix("origin/")?.trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// Run `git <args>`; on failure include git's stderr in the message.
fn run_git(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git ({}): is git installed?", e))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!(
            "git {} failed: {}",
            args.iter().filter(|a| !a.starts_with('-')).cloned().collect::<Vec<_>>().join(" "),
            if err.is_empty() { "unknown error".to_string() } else { err }
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Install every workspace member's dependencies into ONE shared `vendor/` at
/// the workspace root, with one lockfile. Dependencies resolve relative to the
/// member that declares them. Two members vendoring the same dependency name
/// from different sources is an error.
fn install_workspace(
    root: &Path,
    manifest: &Manifest,
    update: bool,
) -> Result<InstallReport, String> {
    let locked = read_locked_sources(root);
    let mut report = InstallReport::default();
    let mut lock = String::from("# generated by spid3r install; do not edit\n");
    lock.push_str("# workspace\n");
    let mut seen: BTreeMap<String, (String, String)> = BTreeMap::new(); // name -> (origin, version)

    struct WsQueueItem {
        dep: Dependency,
        base_dir: PathBuf,
        active_chain: Vec<String>,
        member: String,
    }

    for member in &manifest.members {
        let member_dir = root
            .join(member)
            .canonicalize()
            .map_err(|e| format!("workspace member '{}': cannot resolve: {}", member, e))?;
        let member_manifest = Manifest::load(&member_dir)?.ok_or_else(|| {
            format!(
                "workspace member '{}' ({}) has no spider.toml",
                member,
                member_dir.display()
            )
        })?;
        lock.push_str(&format!(
            "# member {} ({})\n",
            member,
            member_manifest.version()
        ));

        let mut queue: VecDeque<WsQueueItem> = VecDeque::new();
        for dep in member_manifest.deps {
            queue.push_back(WsQueueItem {
                dep,
                base_dir: member_dir.clone(),
                active_chain: vec![member.clone()],
                member: member.clone(),
            });
        }

        while let Some(item) = queue.pop_front() {
            let name = &item.dep.name;
            if item.active_chain.contains(name) {
                return Err(format!(
                    "circular dependency detected: {} -> {}",
                    item.active_chain.join(" -> "),
                    name
                ));
            }

            let (dep_dir, origin) =
                resolve_source(root, &item.base_dir, name, &item.dep.source, &locked, update)?;

            if let Some((prev_origin, prev_version)) = seen.get(name) {
                if prev_origin != &origin {
                    return Err(format!(
                        "workspace: dependency '{}' is declared twice with different sources ({} and {})",
                        name, prev_origin, origin
                    ));
                }
                if let Some(constraint_text) = item.dep.version.as_deref() {
                    let constraint = Constraint::parse(constraint_text)
                        .map_err(|e| format!("dependency '{}': {}", name, e))?;
                    let actual = Version::parse(prev_version)
                        .map_err(|e| format!("dependency '{}': {}", name, e))?;
                    if !constraint.matches(&actual) {
                        return Err(format!(
                            "dependency '{}' requires version {}, but {} provides {}",
                            name,
                            constraint_text,
                            name,
                            prev_version
                        ));
                    }
                }
                continue;
            }

            let dep_manifest = Manifest::load(&dep_dir)?.ok_or_else(|| {
                format!(
                    "dependency '{}' ({}) has no spider.toml",
                    name,
                    dep_dir.display()
                )
            })?;

            if let Some(constraint_text) = item.dep.version.as_deref() {
                let constraint = Constraint::parse(constraint_text)
                    .map_err(|e| format!("dependency '{}': {}", name, e))?;
                let actual = Version::parse(dep_manifest.version())
                    .map_err(|e| format!("dependency '{}': {}", name, e))?;
                if !constraint.matches(&actual) {
                    return Err(format!(
                        "dependency '{}' requires version {}, but {} provides {}",
                        name,
                        constraint_text,
                        dep_manifest.name(),
                        dep_manifest.version()
                    ));
                }
            }

            let (files, hash) = vendor_dep_checked(root, name, &dep_dir, &item.dep.source)?;
            lock.push_str(&format!(
                "\n[[package]]\nname = \"{}\"\nversion = \"{}\"\nsource = \"{}\"\nused-by = \"{}\"\nfiles = {}\nsha256 = \"{}\"\n",
                name, dep_manifest.version(), origin, item.member, files, hash
            ));
            report.deps.push(name.clone());
            report.vendored += files;
            seen.insert(name.clone(), (origin, dep_manifest.version().to_string()));

            let mut next_chain = item.active_chain.clone();
            next_chain.push(name.clone());
            for child_dep in dep_manifest.deps {
                queue.push_back(WsQueueItem {
                    dep: child_dep,
                    base_dir: dep_dir.clone(),
                    active_chain: next_chain.clone(),
                    member: item.member.clone(),
                });
            }
        }
    }

    let lock_path = root.join("spider.lock");
    let old = std::fs::read_to_string(&lock_path).unwrap_or_default();
    if old != lock {
        std::fs::write(&lock_path, lock)
            .map_err(|e| format!("cannot write {}: {}", lock_path.display(), e))?;
    }
    Ok(report)
}

/// Resolve and vendor every dependency, then write `spider.lock`.
pub fn install(root: &Path) -> Result<InstallReport, String> {
    install_with(root, false)
}

/// `spid3r install`: resolve and vendor every dependency, then write
/// `spider.lock`. `update` ignores the commits recorded there and re-resolves
/// git dependencies to their current `rev`/default-branch tip.
pub fn install_with(root: &Path, update: bool) -> Result<InstallReport, String> {
    let Some(manifest) = Manifest::load(root)? else {
        return Err(format!(
            "no spider.toml in {} — run `spid3r init` first",
            root.display()
        ));
    };
    if !manifest.members.is_empty() {
        return install_workspace(root, &manifest, update);
    }
    let locked = read_locked_sources(root);
    let mut report = InstallReport::default();
    let mut lock = String::from("# generated by spid3r install; do not edit\n");
    lock.push_str(&format!("# package {} {}\n", manifest.name(), manifest.version()));

    struct QueueItem {
        dep: Dependency,
        base_dir: PathBuf,
        active_chain: Vec<String>,
    }

    let mut queue: VecDeque<QueueItem> = VecDeque::new();
    let root_pkg_name = manifest.name().to_string();
    for dep in manifest.deps {
        queue.push_back(QueueItem {
            dep,
            base_dir: root.to_path_buf(),
            active_chain: vec![root_pkg_name.clone()],
        });
    }

    let mut seen: BTreeMap<String, (String, String)> = BTreeMap::new();

    while let Some(item) = queue.pop_front() {
        let name = &item.dep.name;
        if item.active_chain.contains(name) {
            return Err(format!(
                "circular dependency detected: {} -> {}",
                item.active_chain.join(" -> "),
                name
            ));
        }

        let (dep_dir, origin) =
            resolve_source(root, &item.base_dir, name, &item.dep.source, &locked, update)?;

        if let Some((prev_origin, prev_version)) = seen.get(name) {
            if prev_origin != &origin {
                return Err(format!(
                    "dependency '{}' is declared twice with different sources ({} and {})",
                    name, prev_origin, origin
                ));
            }
            if let Some(constraint_text) = item.dep.version.as_deref() {
                let constraint = Constraint::parse(constraint_text)
                    .map_err(|e| format!("dependency '{}': {}", name, e))?;
                let actual = Version::parse(prev_version)
                    .map_err(|e| format!("dependency '{}': {}", name, e))?;
                if !constraint.matches(&actual) {
                    return Err(format!(
                        "dependency '{}' requires version {}, but {} provides {}",
                        name,
                        constraint_text,
                        name,
                        prev_version
                    ));
                }
            }
            continue;
        }

        let dep_manifest = Manifest::load(&dep_dir)?.ok_or_else(|| {
            format!(
                "dependency '{}' ({}) has no spider.toml",
                name,
                dep_dir.display()
            )
        })?;

        if let Some(constraint_text) = item.dep.version.as_deref() {
            let constraint = Constraint::parse(constraint_text)
                .map_err(|e| format!("dependency '{}': {}", name, e))?;
            let actual = Version::parse(dep_manifest.version())
                .map_err(|e| format!("dependency '{}': {}", name, e))?;
            if !constraint.matches(&actual) {
                return Err(format!(
                    "dependency '{}' requires version {}, but {} provides {}",
                    name,
                    constraint_text,
                    dep_manifest.name(),
                    dep_manifest.version()
                ));
            }
        }

        let (files, hash) = vendor_dep_checked(root, name, &dep_dir, &item.dep.source)?;
        lock.push_str(&format!(
            "\n[[package]]\nname = \"{}\"\nversion = \"{}\"\nsource = \"{}\"\nfiles = {}\nsha256 = \"{}\"\n",
            name, dep_manifest.version(), origin, files, hash
        ));
        report.deps.push(name.clone());
        report.vendored += files;
        seen.insert(name.clone(), (origin, dep_manifest.version().to_string()));

        let mut next_chain = item.active_chain.clone();
        next_chain.push(name.clone());
        for child_dep in dep_manifest.deps {
            queue.push_back(QueueItem {
                dep: child_dep,
                base_dir: dep_dir.clone(),
                active_chain: next_chain.clone(),
            });
        }
    }

    let lock_path = root.join("spider.lock");
    let old = std::fs::read_to_string(&lock_path).unwrap_or_default();
    if old == lock {
        report.unchanged += 1;
    } else {
        std::fs::write(&lock_path, lock)
            .map_err(|e| format!("cannot write {}: {}", lock_path.display(), e))?;
    }
    Ok(report)
}

/// Create `spider.toml` and `src/main.matrix` in `dir` (if missing).
pub fn init(dir: &Path, name: &str) -> Result<Vec<String>, String> {
    let mut created = Vec::new();
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
    let manifest = dir.join("spider.toml");
    if !manifest.exists() {
        let text = format!(
            "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\n",
            name
        );
        std::fs::write(&manifest, text).map_err(|e| format!("cannot write {}: {}", manifest.display(), e))?;
        created.push("spider.toml".to_string());
    }
    let src = dir.join("src");
    std::fs::create_dir_all(&src).map_err(|e| format!("cannot create {}: {}", src.display(), e))?;
    let main = src.join("main.matrix");
    if !main.exists() {
        std::fs::write(
            &main,
            "// entry point\nfn main() {\n    println(\"hello from Spid3r\");\n    return 0;\n}\n",
        )
        .map_err(|e| format!("cannot write {}: {}", main.display(), e))?;
        created.push("src/main.matrix".to_string());
    }
    // Build artifacts and fetched dependencies are reproducible from
    // spider.lock; they should not be committed.
    let gitignore = dir.join(".gitignore");
    let wanted = "vendor/\ntarget/\n.spid3r/\n";
    if !gitignore.exists() {
        std::fs::write(&gitignore, wanted)
            .map_err(|e| format!("cannot write {}: {}", gitignore.display(), e))?;
        created.push(".gitignore".to_string());
    } else {
        let text = std::fs::read_to_string(&gitignore)
            .map_err(|e| format!("cannot read {}: {}", gitignore.display(), e))?;
        let missing: Vec<&str> = wanted
            .lines()
            .filter(|l| !text.lines().any(|x| x.trim() == *l))
            .collect();
        if !missing.is_empty() {
            let mut text = text;
            if !text.ends_with('\n') && !text.is_empty() {
                text.push('\n');
            }
            for l in missing {
                text.push_str(l);
                text.push('\n');
            }
            std::fs::write(&gitignore, text)
                .map_err(|e| format!("cannot write {}: {}", gitignore.display(), e))?;
        }
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn sha256_bytes_matches_the_hex_form() {
        // The stdlib exposes the raw digest; the two entry points must agree
        // byte for byte, or `spid3r sha256(...)` and the toolchain's own
        // manifest hashes would diverge.
        let digest = sha256_bytes(b"abc");
        assert_eq!(digest.len(), 32);
        assert_eq!(
            hex_encode(&digest),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex_encode(&sha256_bytes(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // Multi-block message: padding and the length field must be right.
        assert_eq!(
            hex_encode(&sha256_bytes(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // Exactly one full block (63 bytes of data forces a second, padding-only
        // block) — a classic off-by-one in the padding loop.
        let exactly_63 = [b'a'; 63];
        let exactly_64 = [b'a'; 64];
        assert_eq!(
            hex_encode(&sha256_bytes(&exactly_63)),
            "7d3e74a05d7db15bce4ad9ec0658ea98e3f06eeecf16b4c6fff2da457ddc2f34"
        );
        assert_eq!(
            hex_encode(&sha256_bytes(&exactly_64)),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    #[test]
    fn hex_encode_is_lowercase_and_fixed_width() {
        assert_eq!(hex_encode(&[0x00, 0x0f, 0xff]), "000fff");
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn parses_a_manifest() {
        let m = parse_manifest(
            "[package]\nname = \"app\"\nversion = \"1.2.3\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nmathlib = { path = \"../mathlib\" }\nutil = \"../util\"  # trailing comment\n",
        )
        .expect("parse");
        assert_eq!(m.name(), "app");
        assert_eq!(m.version(), "1.2.3");
        assert_eq!(m.entry(), "src/main.matrix");
        assert_eq!(
            m.deps,
            vec![
                Dependency {
                    name: "mathlib".to_string(),
                    source: DepSource::Path("../mathlib".to_string()),
                    version: None,
                },
                Dependency {
                    name: "util".to_string(),
                    source: DepSource::Path("../util".to_string()),
                    version: None,
                },
            ]
        );
    }

    #[test]
    fn rejects_bad_manifests() {
        assert!(parse_manifest("[package]\nname = \"x\"\n").is_ok());
        assert!(parse_manifest("[package]\nversion = \"1\"\n").is_err(), "name required");
        assert!(parse_manifest("[dependencies]\nlib = 1\n").is_err());
        assert!(parse_manifest("[other]\nname = \"x\"\n").is_err());
    }

    #[test]
    fn comment_hash_inside_a_string_is_not_a_comment() {
        let m = parse_manifest("[package]\nname = \"a#b\"\n").expect("parse");
        assert_eq!(m.name(), "a#b");
    }

    #[test]
    fn version_only_dependency_is_a_registry_dep() {
        let m = parse_manifest(
            "[package]\nname = \"app\"\n\n[dependencies]\nmathlib = { version = \"^1.2\" }\n\
             internal = { version = \"1.0\", registry = \"internal\" }\n",
        )
        .expect("parse");
        assert_eq!(
            m.deps[0].source,
            DepSource::Registry {
                version: "^1.2".to_string(),
                registry: None
            }
        );
        assert_eq!(
            m.deps[1].source,
            DepSource::Registry {
                version: "1.0".to_string(),
                registry: Some("internal".to_string())
            }
        );
    }

    #[test]
    fn dependency_table_needs_a_source() {
        assert!(parse_manifest("[package]\nname = \"x\"\n\n[dependencies]\nlib = { }\n").is_err());
    }
}
