//! SPM operations: `verify`, `tree`, `add`, `remove`, `publish --to-dir`,
//! and `search`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::hash::{
    content_hash, manifest_and_tree_digest, manifest_and_wheel_digest, native_tree_hash,
    vendor_src_hash,
};
pub use super::lockfile::{read_lock, LockEntry};
use super::manifest::{
    parse_manifest, toml_escape_string, valid_pkg_name, validate_native_rel_path, DepSource,
    Manifest, PackageInfo,
};
use super::registry;
use super::semver::Version;

/// One problem found by [`verify`].
#[derive(Debug, Clone)]
pub struct VerifyIssue {
    /// The package the issue is about.
    pub name: String,
    /// `missing`, `hash`, `files`, `unverified`, or `unlocked`.
    pub kind: &'static str,
    /// Human-readable explanation.
    pub detail: String,
}

/// Outcome of `spkg verify`.
#[derive(Debug, Clone, Default)]
pub struct VerifyReport {
    pub checked: usize,
    pub issues: Vec<VerifyIssue>,
}

impl VerifyReport {
    pub fn ok(&self) -> bool {
        self.issues.is_empty()
    }
}

/// Re-hash every vendored dependency against `spider.lock` (symlink-safe).
pub fn verify(root: &Path) -> Result<VerifyReport, String> {
    let entries = read_lock(root)?;
    let mut report = VerifyReport::default();
    let vendor = root.join("vendor");

    for entry in &entries {
        valid_pkg_name(&entry.name)?;
        let dir = vendor.join(&entry.name);
        let Some(expected) = entry.sha256.as_deref() else {
            if entry.source.starts_with("registry+") {
                report.issues.push(VerifyIssue {
                    name: entry.name.clone(),
                    kind: "unverified",
                    detail: "a registry dependency must record a sha256 in \
                             spider.lock — run `spkg install` to re-vendor it"
                        .to_string(),
                });
            }
            continue;
        };
        report.checked += 1;
        let dmeta = match std::fs::symlink_metadata(&dir) {
            Ok(m) => m,
            Err(_) => {
                report.issues.push(VerifyIssue {
                    name: entry.name.clone(),
                    kind: "missing",
                    detail: format!(
                        "vendored at vendor/{} but the directory is missing — run `spkg install`",
                        entry.name
                    ),
                });
                continue;
            }
        };
        if dmeta.file_type().is_symlink() || !dmeta.is_dir() {
            report.issues.push(VerifyIssue {
                name: entry.name.clone(),
                kind: "hash",
                detail: format!(
                    "vendor/{} is a symlink or non-directory — refusing to trust unverified files",
                    entry.name
                ),
            });
            continue;
        }
        let (found_count, actual) = vendor_src_hash(&dir)?;
        if actual != expected {
            report.issues.push(VerifyIssue {
                name: entry.name.clone(),
                kind: "hash",
                detail: format!(
                    "vendor/{} does not match spider.lock\n  expected {}\n  actual   {}",
                    entry.name, expected, actual
                ),
            });
            continue;
        }
        if let Some(files) = entry.files {
            if found_count != files {
                report.issues.push(VerifyIssue {
                    name: entry.name.clone(),
                    kind: "files",
                    detail: format!(
                        "vendor/{} has {} file(s), spider.lock records {}",
                        entry.name, found_count, files
                    ),
                });
            }
        }
        if let Some(expected_native) = entry.native_sha256.as_deref() {
            let tgt = entry
                .target
                .as_deref()
                .unwrap_or_else(|| super::manifest::host_target_triple());
            let native_dir = dir.join("_native").join(tgt);
            if !native_dir.is_dir() {
                report.issues.push(VerifyIssue {
                    name: entry.name.clone(),
                    kind: "native-missing",
                    detail: format!(
                        "vendor/{} is missing native artifact directory _native/{}",
                        entry.name, tgt
                    ),
                });
            } else {
                let actual_native = native_tree_hash(&native_dir)?;
                if actual_native != expected_native {
                    report.issues.push(VerifyIssue {
                        name: entry.name.clone(),
                        kind: "native-hash",
                        detail: format!(
                            "vendor/{}/_native/{} does not match spider.lock\n  expected {}\n  actual   {}",
                            entry.name, tgt, expected_native, actual_native
                        ),
                    });
                }
            }
        }
        if let Some(expected_msha) = entry.manifest_sha256.as_deref() {
            let snap_manifest = dir.join(".spid3r-manifest.toml");
            match std::fs::symlink_metadata(&snap_manifest) {
                Ok(m) if m.file_type().is_symlink() => {
                    report.issues.push(VerifyIssue {
                        name: entry.name.clone(),
                        kind: "manifest-hash",
                        detail: format!("vendor/{}/.spid3r-manifest.toml is a symlink", entry.name),
                    });
                }
                Ok(m) if m.is_file() => {
                    let raw_manifest = std::fs::read(&snap_manifest)
                        .map_err(|e| format!("cannot read {}: {}", snap_manifest.display(), e))?;
                    let computed = manifest_and_tree_digest(&raw_manifest, &actual);
                    if computed != expected_msha {
                        report.issues.push(VerifyIssue {
                            name: entry.name.clone(),
                            kind: "manifest-hash",
                            detail: format!(
                                "vendor/{} manifest.sha256 mismatch (expected {}, computed {})",
                                entry.name, expected_msha, computed
                            ),
                        });
                    }
                }
                _ => {
                    report.issues.push(VerifyIssue {
                        name: entry.name.clone(),
                        kind: "manifest-missing",
                        detail: format!(
                            "vendor/{} is missing .spid3r-manifest.toml required by spider.lock",
                            entry.name
                        ),
                    });
                }
            }
        }
    }

    if vendor.is_dir() {
        let mut names: Vec<String> = std::fs::read_dir(&vendor)
            .map_err(|e| format!("cannot read vendor/: {}", e))?
            .filter_map(|e| e.ok())
            .filter(|e| {
                let p = e.path();
                p.is_dir()
                    && !e.file_name().to_string_lossy().starts_with(".tmp_")
                    && !e.file_name().to_string_lossy().starts_with(".old_")
            })
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        for name in names {
            if !entries.iter().any(|e| e.name == name) {
                report.issues.push(VerifyIssue {
                    name,
                    kind: "unlocked",
                    detail: "present in vendor/ but absent from spider.lock".to_string(),
                });
            }
        }
    }

    Ok(report)
}

/// Render `spkg tree`: full dependency tree across path, git, registry, and
/// workspace members (fixes B6).
pub fn dep_tree(root: &Path) -> Result<String, String> {
    let manifest =
        Manifest::load(root)?.ok_or_else(|| format!("no spider.toml in {}", root.display()))?;
    let locked = read_lock(root)?;
    let mut out = String::new();

    if !manifest.members.is_empty() && manifest.package.is_none() {
        out.push_str("workspace\n");
        let mcount = manifest.members.len();
        for (idx, member) in manifest.members.iter().enumerate() {
            let last_m = idx + 1 == mcount;
            let branch = if last_m { "└─ " } else { "├─ " };
            let mdir = root.join(member);
            if let Ok(Some(m_manifest)) = Manifest::load(&mdir) {
                out.push_str(&format!(
                    "{}{} {} [{}]\n",
                    branch,
                    m_manifest.name(),
                    m_manifest.version(),
                    member
                ));
                let next_prefix = if last_m { "   " } else { "│  " };
                let mut visiting = vec![m_manifest.name().to_string()];
                render_deps(
                    root,
                    &mdir,
                    &m_manifest,
                    &locked,
                    next_prefix,
                    &mut visiting,
                    &mut out,
                )?;
            } else {
                out.push_str(&format!("{}{} (missing spider.toml)\n", branch, member));
            }
        }
        return Ok(out);
    }

    out.push_str(&format!("{} {}\n", manifest.name(), manifest.version()));
    let mut visiting: Vec<String> = vec![manifest.name().to_string()];
    render_deps(root, root, &manifest, &locked, "", &mut visiting, &mut out)?;
    if manifest.deps.is_empty() {
        out.push_str("└─ (no dependencies)\n");
    }
    Ok(out)
}

fn find_dep_manifest(
    project_root: &Path,
    caller_dir: &Path,
    name: &str,
    source: &DepSource,
    lock: Option<&LockEntry>,
) -> Option<(PathBuf, Manifest)> {
    match source {
        DepSource::Path(p) => {
            let dep_root = caller_dir.join(p);
            Manifest::load(&dep_root)
                .ok()
                .flatten()
                .map(|m| (dep_root, m))
        }
        DepSource::Git { .. } => {
            let dep_root = project_root.join(".spid3r").join("git").join(name);
            Manifest::load(&dep_root)
                .ok()
                .flatten()
                .map(|m| (dep_root, m))
        }
        DepSource::Registry { registry, .. } => {
            let ver = &lock?.version;
            for ep in registry::registry_endpoints() {
                if registry::is_remote_url(&ep) {
                    if let Ok(base) = registry::build_registry_base(&ep, registry.as_deref()) {
                        let cache_dir = registry::registry_cache_root(project_root, &base)
                            .join(name)
                            .join(ver);
                        if let Ok(Some(m)) = Manifest::load(&cache_dir) {
                            return Some((cache_dir, m));
                        }
                    }
                } else {
                    let base = match registry {
                        Some(sub) => PathBuf::from(&ep).join(sub),
                        None => PathBuf::from(&ep),
                    };
                    let pkg_dir = base.join(name).join(ver);
                    if let Ok(Some(m)) = Manifest::load(&pkg_dir) {
                        return Some((pkg_dir, m));
                    }
                }
            }
            None
        }
    }
}

fn render_deps(
    project_root: &Path,
    caller_dir: &Path,
    manifest: &Manifest,
    locked: &[LockEntry],
    prefix: &str,
    visiting: &mut Vec<String>,
    out: &mut String,
) -> Result<(), String> {
    let n = manifest.deps.len();
    for (i, dep) in manifest.deps.iter().enumerate() {
        let last = i + 1 == n;
        let branch = if last { "└─ " } else { "├─ " };
        let lock = locked.iter().find(|e| e.name == dep.name);
        let version = lock
            .map(|e| e.version.clone())
            .unwrap_or_else(|| "-".into());
        let constraint = dep
            .version
            .as_deref()
            .map(|c| format!(" ({})", c))
            .unwrap_or_default();
        out.push_str(&format!(
            "{}{}{} {} [{}]{}{}\n",
            prefix,
            branch,
            dep.name,
            version,
            dep.source.describe(),
            constraint,
            if lock.is_some() { "" } else { "  unlocked" }
        ));

        if visiting.contains(&dep.name) {
            out.push_str(&format!(
                "{}{}  (cycle: {})\n",
                prefix,
                if last { "   " } else { "│  " },
                dep.name
            ));
            continue;
        }
        let Some((dep_root, child)) =
            find_dep_manifest(project_root, caller_dir, &dep.name, &dep.source, lock)
        else {
            continue;
        };
        visiting.push(dep.name.clone());
        let next = format!("{}{}", prefix, if last { "   " } else { "│  " });
        render_deps(
            project_root,
            &dep_root,
            &child,
            locked,
            &next,
            visiting,
            out,
        )?;
        visiting.pop();
    }
    Ok(())
}

/// Declare a dependency textually in `spider.toml` `[dependencies]` while
/// preserving existing comments and ordering.
pub fn add_dependency(
    root: &Path,
    name: &str,
    value: &str,
    version: Option<&str>,
) -> Result<bool, String> {
    upsert_dependency(root, name, value, version, false)
}

/// Declare or update a dependency in `spider.toml` `[dependencies]`.
/// When `replace_existing` is true and `[dependencies]` already declares `name`
/// with a different spec, replaces that line cleanly.
pub fn upsert_dependency(
    root: &Path,
    name: &str,
    value: &str,
    version: Option<&str>,
    replace_existing: bool,
) -> Result<bool, String> {
    valid_pkg_name(name)?;
    let path = root.join("spider.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;

    let mut entry = format!("{} = {}", name, value);
    if let Some(v) = version {
        if value.starts_with('{') {
            entry = format!(
                "{} = {{ {}, version = \"{}\" }}",
                name,
                value.trim_start_matches('{').trim_end_matches('}').trim(),
                toml_escape_string(v)
            );
        } else {
            return Err(
                "a version constraint needs the table form, e.g. { path = \"../x\", version = \"^1.2\" }"
                    .to_string(),
            );
        }
    }

    let updated = insert_into_dependencies(&text, name, &entry, replace_existing)?;
    if updated == text {
        return Ok(false);
    }
    std::fs::write(&path, updated)
        .map_err(|e| format!("cannot write {}: {}", path.display(), e))?;
    Ok(true)
}

fn insert_into_dependencies(
    text: &str,
    name: &str,
    line: &str,
    replace_existing: bool,
) -> Result<String, String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut section: Option<(usize, usize)> = None;
    for (i, raw) in lines.iter().enumerate() {
        let t = raw.trim();
        if let Some(rest) = t.strip_prefix('[') {
            if let Some(sect) = rest.strip_suffix(']') {
                if section.is_some() {
                    break;
                }
                if sect.trim() == "dependencies" {
                    section = Some((i, lines.len()));
                }
            }
        }
    }
    if let Some((start, _end)) = section {
        let mut stop = lines.len();
        for (i, raw) in lines.iter().enumerate().skip(start + 1) {
            if raw.trim().starts_with('[') {
                stop = i;
                break;
            }
        }
        for (idx, raw) in lines.iter().enumerate().take(stop).skip(start + 1) {
            let t = raw.trim();
            if let Some((k, _)) = t.split_once('=') {
                if k.trim() == name {
                    if replace_existing && t != line.trim() {
                        let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
                        out[idx] = line.to_string();
                        return Ok(out.join("\n") + "\n");
                    }
                    return Ok(text.to_string());
                }
            }
        }
        let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        out.insert(stop, line.to_string());
        return Ok(out.join("\n") + "\n");
    }
    let mut out = text.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("\n[dependencies]\n");
    out.push_str(line);
    out.push('\n');
    Ok(out)
}

/// Remove declaration for `name` from `spider.toml` across `[dependencies]`,
/// `[dev-dependencies]`, and `[optional-dependencies.*]`.
pub fn remove_dependency(root: &Path, name: &str) -> Result<bool, String> {
    let path = root.join("spider.toml");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    let mut section = String::new();
    let mut removed = false;
    let mut out: Vec<String> = Vec::new();
    for raw in text.lines() {
        let t = raw.trim();
        if let Some(rest) = t.strip_prefix('[') {
            if let Some(sect) = rest.strip_suffix(']') {
                section = sect.trim().to_string();
                out.push(raw.to_string());
                continue;
            }
        }
        let in_dep_section = section == "dependencies"
            || section == "dev-dependencies"
            || section.starts_with("optional-dependencies.");
        if in_dep_section {
            if let Some((k, _)) = t.split_once('=') {
                if k.trim() == name {
                    removed = true;
                    continue;
                }
            }
        }
        out.push(raw.to_string());
    }
    if !removed {
        return Ok(false);
    }
    std::fs::write(&path, out.join("\n") + "\n")
        .map_err(|e| format!("cannot write {}: {}", path.display(), e))?;
    Ok(true)
}

/// `spkg publish --to-dir <dir>`: write immutable versioned release directory,
/// `sha256.txt`, `sha256.txt.sig`, `manifest.sha256`, `manifest.sha256.sig`,
/// `package.tar`, `<pkg>/index.txt` (+ `.sig`), `registry.pub`, `registry.toml`
/// (+ `.sig`), and `catalog.txt` (+ `.sig`).
pub fn publish_to_dir(root: &Path, dir: &Path) -> Result<(PathBuf, String), String> {
    publish_to_dir_with_target(root, dir, None)
}

/// Publish a universal release (`target = None`) or a signed per-target binary
/// wheel (`target = Some("windows-x86_64")`) into `dir`.
pub fn publish_to_dir_with_target(
    root: &Path,
    dir: &Path,
    target: Option<&str>,
) -> Result<(PathBuf, String), String> {
    if let Some(t) = target {
        valid_pkg_name(t)?;
    }
    let manifest =
        Manifest::load(root)?.ok_or_else(|| format!("no spider.toml in {}", root.display()))?;
    valid_pkg_name(manifest.name())?;
    let version = manifest.version().to_string();
    if version.is_empty() {
        return Err("the package has no version".to_string());
    }
    Version::parse(&version)
        .map_err(|e| format!("invalid package version '{}': {}", version, e))?;
    let pkg_dir = dir.join(manifest.name());
    let dest = pkg_dir.join(&version);
    let adding_wheel_to_existing = target.is_some() && dest.exists();
    if dest.exists() && target.is_none() {
        return Err(format!(
            "{} already exists — bump the version instead of overwriting a published release",
            dest.display()
        ));
    }
    if let Some(t) = target {
        let wheel_tar = dest.join(format!("package-{}.tar", t));
        if wheel_tar.exists() {
            return Err(format!(
                "{} already exists — bump the version instead of overwriting a published wheel",
                wheel_tar.display()
            ));
        }
        let native_src = root.join("native").join(t);
        let nmeta = std::fs::symlink_metadata(&native_src).map_err(|_| {
            format!(
                "publish --target {}: missing directory '{}'",
                t,
                native_src.display()
            )
        })?;
        if nmeta.file_type().is_symlink() || !nmeta.is_dir() {
            return Err(format!(
                "publish --target {}: invalid/symlinked native directory '{}'",
                t,
                native_src.display()
            ));
        }
        for nf in super::hash::collect_regular_files(&native_src)? {
            let rel = nf.strip_prefix(&native_src).unwrap_or(&nf);
            validate_native_rel_path(&rel.to_string_lossy().replace('\\', "/"))?;
        }
    }
    if !adding_wheel_to_existing {
        std::fs::create_dir_all(&dest)
            .map_err(|e| format!("cannot create {}: {}", dest.display(), e))?;
    }

    let publish_res = (|| -> Result<String, String> {
        let src_manifest_path = root.join("spider.toml");
        let manifest_bytes = std::fs::read(&src_manifest_path)
            .map_err(|e| format!("cannot read spider.toml: {}", e))?;

        let (pk, sk) = load_or_create_signing_keypair(dir)?;
        let pk_hex = super::sha256::hex_encode(&pk);
        std::fs::write(dir.join("registry.pub"), format!("{}\n", pk_hex))
            .map_err(|e| format!("cannot write registry.pub: {}", e))?;

        let hash = if adding_wheel_to_existing {
            let existing_manifest = std::fs::read(dest.join("spider.toml"))
                .map_err(|e| format!("existing release lacks spider.toml: {}", e))?;
            if existing_manifest != manifest_bytes {
                return Err(format!(
                    "cannot add wheel to existing {}@{}: local spider.toml differs from published release",
                    manifest.name(),
                    version
                ));
            }
            let existing_sha = std::fs::read_to_string(dest.join("sha256.txt"))
                .map_err(|e| format!("existing release lacks sha256.txt: {}", e))?
                .trim()
                .to_string();
            let local_hash = super::hash::tree_hash(root)?;
            if local_hash != existing_sha {
                return Err(format!(
                    "cannot add wheel to existing {}@{}: local src/ hash ({}) differs from published release ({})",
                    manifest.name(),
                    version,
                    local_hash,
                    existing_sha
                ));
            }
            existing_sha
        } else {
            std::fs::write(dest.join("spider.toml"), &manifest_bytes)
                .map_err(|e| format!("cannot write spider.toml: {}", e))?;

            let src = root.join("src");
            let src_meta = std::fs::symlink_metadata(&src)
                .map_err(|_| format!("no src/ directory in {}", root.display()))?;
            if src_meta.file_type().is_symlink() || !src_meta.is_dir() {
                return Err(format!(
                    "invalid or symlinked src/ directory in {}",
                    root.display()
                ));
            }
            copy_tree_no_symlinks(&src, &dest.join("src"))?;

            let hash = content_hash(&dest.join("src"))?;
            let sha_txt = format!("{}\n", hash);
            std::fs::write(dest.join("sha256.txt"), sha_txt.as_bytes())
                .map_err(|e| format!("cannot write sha256.txt: {}", e))?;

            let manifest_digest = manifest_and_tree_digest(&manifest_bytes, &hash);
            let msha_txt = format!("{}\n", manifest_digest);
            std::fs::write(dest.join("manifest.sha256"), msha_txt.as_bytes())
                .map_err(|e| format!("cannot write manifest.sha256: {}", e))?;

            let entries = super::tar::pack_entries(root)?;
            let tar_bytes = super::tar::write_tar(&entries);
            std::fs::write(dest.join("package.tar"), &tar_bytes)
                .map_err(|e| format!("cannot write package.tar: {}", e))?;

            let sig = super::ed25519::sign(&sk, sha_txt.as_bytes())?;
            std::fs::write(
                dest.join("sha256.txt.sig"),
                format!("{}\n", super::sha256::hex_encode(&sig)),
            )
            .map_err(|e| format!("cannot write sha256.txt.sig: {}", e))?;

            let msig = super::ed25519::sign(&sk, msha_txt.as_bytes())?;
            std::fs::write(
                dest.join("manifest.sha256.sig"),
                format!("{}\n", super::sha256::hex_encode(&msig)),
            )
            .map_err(|e| format!("cannot write manifest.sha256.sig: {}", e))?;
            hash
        };

        if let Some(t) = target {
            let native_src = root.join("native").join(t);
            let native_dst = dest.join("native").join(t);
            copy_tree_no_symlinks(&native_src, &native_dst)?;
            let n_hash = native_tree_hash(&native_dst)?;
            let n_sha_txt = format!("{}\n", n_hash);
            std::fs::write(dest.join(format!("sha256-{}.txt", t)), n_sha_txt.as_bytes())
                .map_err(|e| format!("cannot write wheel sha256: {}", e))?;
            let n_sig = super::ed25519::sign(&sk, n_sha_txt.as_bytes())?;
            std::fs::write(
                dest.join(format!("sha256-{}.txt.sig", t)),
                format!("{}\n", super::sha256::hex_encode(&n_sig)),
            )
            .map_err(|e| format!("cannot write wheel sig: {}", e))?;

            let w_msha = manifest_and_wheel_digest(&manifest_bytes, &hash, t, &n_hash);
            let w_msha_txt = format!("{}\n", w_msha);
            std::fs::write(
                dest.join(format!("manifest-{}.sha256", t)),
                w_msha_txt.as_bytes(),
            )
            .map_err(|e| format!("cannot write wheel manifest digest: {}", e))?;
            let w_msig = super::ed25519::sign(&sk, w_msha_txt.as_bytes())?;
            std::fs::write(
                dest.join(format!("manifest-{}.sha256.sig", t)),
                format!("{}\n", super::sha256::hex_encode(&w_msig)),
            )
            .map_err(|e| format!("cannot write wheel manifest sig: {}", e))?;

            let w_entries = super::tar::pack_entries_for_target(root, Some(t))?;
            let w_tar = super::tar::write_tar(&w_entries);
            std::fs::write(dest.join(format!("package-{}.tar", t)), &w_tar)
                .map_err(|e| format!("cannot write wheel tar: {}", e))?;
        }

        let mut versions: Vec<Version> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&pkg_dir) {
            for entry in rd.filter_map(|e| e.ok()) {
                if !entry.path().is_dir() {
                    continue;
                }
                if let Some(name) = entry.file_name().to_str() {
                    if let Ok(v) = Version::parse(name) {
                        versions.push(v);
                    }
                }
            }
        }
        versions.sort();
        let index_body: String = versions
            .iter()
            .map(|v| format!("{}.{}.{}\n", v.major, v.minor, v.patch))
            .collect();
        std::fs::write(pkg_dir.join("index.txt"), index_body.as_bytes())
            .map_err(|e| format!("cannot write index.txt: {}", e))?;
        let index_sig = super::ed25519::sign(&sk, index_body.as_bytes())?;
        std::fs::write(
            pkg_dir.join("index.txt.sig"),
            format!("{}\n", super::sha256::hex_encode(&index_sig)),
        )
        .map_err(|e| format!("cannot write index.txt.sig: {}", e))?;

        let desc_path = dir.join("registry.toml");
        if !desc_path.exists() {
            let desc_body = format!(
                "# Host-portable Spid3r Package Registry descriptor\n\
                 name = \"Spid3r Package Registry\"\n\
                 api_version = \"1\"\n\
                 public_key = \"{}\"\n\
                 moved_to = \"\"\n\
                 mirrors = []\n",
                pk_hex
            );
            std::fs::write(&desc_path, desc_body)
                .map_err(|e| format!("cannot write registry.toml: {}", e))?;
        } else {
            let existing_desc = std::fs::read_to_string(&desc_path)
                .map_err(|e| format!("cannot read registry.toml: {}", e))?;
            let parsed_desc = registry::parse_registry_descriptor(&existing_desc);
            if parsed_desc.public_key.as_deref() != Some(pk_hex.as_str()) {
                let mut replaced = false;
                let mut lines: Vec<String> = existing_desc
                    .lines()
                    .map(|raw| {
                        if let Some((k, _)) = raw.split_once('=') {
                            if k.trim() == "public_key" {
                                replaced = true;
                                return format!("public_key = \"{}\"", pk_hex);
                            }
                        }
                        raw.to_string()
                    })
                    .collect();
                if !replaced {
                    lines.push(format!("public_key = \"{}\"", pk_hex));
                }
                std::fs::write(&desc_path, lines.join("\n") + "\n")
                    .map_err(|e| format!("cannot update registry.toml: {}", e))?;
            }
        }
        let desc_bytes = std::fs::read(&desc_path)
            .map_err(|e| format!("cannot read registry.toml for signing: {}", e))?;
        let dsig = super::ed25519::sign(&sk, &desc_bytes)?;
        std::fs::write(
            dir.join("registry.toml.sig"),
            format!("{}\n", super::sha256::hex_encode(&dsig)),
        )
        .map_err(|e| format!("cannot write registry.toml.sig: {}", e))?;

        write_registry_catalog(dir)?;
        let cat_bytes = std::fs::read(dir.join("catalog.txt"))
            .map_err(|e| format!("cannot read catalog.txt for signing: {}", e))?;
        let csig = super::ed25519::sign(&sk, &cat_bytes)?;
        std::fs::write(
            dir.join("catalog.txt.sig"),
            format!("{}\n", super::sha256::hex_encode(&csig)),
        )
        .map_err(|e| format!("cannot write catalog.txt.sig: {}", e))?;
        Ok(hash)
    })();

    match publish_res {
        Ok(hash) => Ok((dest, hash)),
        Err(e) => {
            if !adding_wheel_to_existing {
                let _ = std::fs::remove_dir_all(&dest);
            }
            Err(e)
        }
    }
}

fn sanitize_catalog_cell(s: &str) -> String {
    s.replace(['\t', '\r', '\n'], " ").trim().to_string()
}

/// Regenerate `<dir>/catalog.txt` summarizing every published package so remote
/// `spkg search` and static web portals stay in sync.
pub fn write_registry_catalog(dir: &Path) -> Result<(), String> {
    let mut packages: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.filter_map(|e| e.ok()) {
            if !entry.path().is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(String::from) else {
                continue;
            };
            if valid_pkg_name(&name).is_err() {
                continue;
            }
            packages.push(name);
        }
    }
    packages.sort();

    let mut out = String::from(
        "# Spid3r Package Registry Catalog\n# name\tlatest\tversions\tdescription\tcategories\ttargets\tlicense\n",
    );
    for pkg_name in packages {
        let pkg_dir = dir.join(&pkg_name);
        let mut versions: Vec<Version> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&pkg_dir) {
            for entry in rd.filter_map(|e| e.ok()) {
                if !entry.path().is_dir() {
                    continue;
                }
                if let Some(ver_name) = entry.file_name().to_str() {
                    if let Ok(v) = Version::parse(ver_name) {
                        versions.push(v);
                    }
                }
            }
        }
        if versions.is_empty() {
            continue;
        }
        versions.sort();
        let latest = versions.last().unwrap().to_string();
        let ver_csv: Vec<String> = versions.iter().map(ToString::to_string).collect();
        let ver_dir = pkg_dir.join(&latest);
        let mut wheel_targets: Vec<String> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&ver_dir) {
            for entry in rd.filter_map(|e| e.ok()) {
                if let Some(fname) = entry.file_name().to_str() {
                    if let Some(rest) = fname
                        .strip_prefix("package-")
                        .and_then(|s| s.strip_suffix(".tar"))
                    {
                        if !rest.is_empty() {
                            wheel_targets.push(rest.to_string());
                        }
                    }
                }
            }
        }
        let pkg_info: Option<PackageInfo> = Manifest::load(&ver_dir)
            .ok()
            .flatten()
            .and_then(|m| m.package);
        let (desc, cats, targets, license) = match pkg_info {
            Some(p) => {
                let mut tgt_set: std::collections::BTreeSet<String> =
                    p.targets.iter().map(|t| sanitize_catalog_cell(t)).collect();
                for wt in wheel_targets {
                    tgt_set.insert(sanitize_catalog_cell(&wt));
                }
                (
                    p.description
                        .map(|s| sanitize_catalog_cell(&s))
                        .unwrap_or_default(),
                    p.categories
                        .iter()
                        .map(|c| sanitize_catalog_cell(c))
                        .collect::<Vec<_>>()
                        .join(","),
                    tgt_set.into_iter().collect::<Vec<_>>().join(","),
                    p.license
                        .map(|s| sanitize_catalog_cell(&s))
                        .unwrap_or_default(),
                )
            }
            None => (
                String::new(),
                String::new(),
                wheel_targets.join(","),
                String::new(),
            ),
        };
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            pkg_name,
            latest,
            ver_csv.join(","),
            desc,
            cats,
            targets,
            license
        ));
    }

    std::fs::write(dir.join("catalog.txt"), out)
        .map_err(|e| format!("cannot write catalog.txt: {}", e))?;
    Ok(())
}

fn load_or_create_signing_keypair(dir: &Path) -> Result<([u8; 32], [u8; 64]), String> {
    let parse_seed_hex = |raw: &str, label: &str| -> Result<[u8; 32], String> {
        let trimmed = raw.trim();
        let bytes = super::sha256::hex_decode(trimmed)
            .ok_or_else(|| format!("{}: invalid hex signing key", label))?;
        if bytes.len() != 32 && bytes.len() != 64 {
            return Err(format!(
                "{}: signing key must be 32-byte seed (64 hex chars) or 64-byte secret (128 hex chars), got {} bytes",
                label,
                bytes.len()
            ));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes[..32]);
        Ok(seed)
    };

    for var in ["SPID3R_SIGN_KEY", "SPID3R_PACKAGES_SIGN_KEY"] {
        if let Ok(val) = std::env::var(var) {
            if !val.trim().is_empty() {
                let seed = parse_seed_hex(&val, var)?;
                return Ok(super::ed25519::keypair_from_seed(&seed));
            }
        }
    }

    let key_path = dir.join("registry.key");
    let seed = if key_path.is_file() {
        let text = std::fs::read_to_string(&key_path)
            .map_err(|e| format!("cannot read {}: {}", key_path.display(), e))?;
        parse_seed_hex(&text, &key_path.display().to_string())?
    } else {
        let mut seed = [0u8; 32];
        crate::stdlib::entropy::fill_random(&mut seed)?;
        std::fs::write(&key_path, format!("{}\n", super::sha256::hex_encode(&seed)))
            .map_err(|e| format!("cannot write {}: {}", key_path.display(), e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&key_path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(&key_path, perms);
            }
        }
        eprintln!(
            "note: generated signing key at {}; keep private keys outside git/cloud folders and prefer SPID3R_SIGN_KEY",
            key_path.display()
        );
        seed
    };
    Ok(super::ed25519::keypair_from_seed(&seed))
}

fn copy_tree_no_symlinks(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("cannot create {}: {}", to.display(), e))?;
    for entry in std::fs::read_dir(from)
        .map_err(|e| format!("cannot read {}: {}", from.display(), e))?
        .filter_map(|e| e.ok())
    {
        let p = entry.path();
        let meta = std::fs::symlink_metadata(&p)
            .map_err(|e| format!("cannot inspect {}: {}", p.display(), e))?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "refusing symlink '{}' in published package",
                p.display()
            ));
        }
        let target = to.join(entry.file_name());
        if meta.is_dir() {
            copy_tree_no_symlinks(&p, &target)?;
        } else if meta.is_file() {
            std::fs::copy(&p, &target)
                .map_err(|e| format!("cannot copy {}: {}", p.display(), e))?;
        } else {
            return Err(format!(
                "refusing non-regular file '{}' in published package",
                p.display()
            ));
        }
    }
    Ok(())
}

/// Search configured registries (`packages.registry` mirrors or local dir) and
/// `vendor/`, optionally filtering remote results by `--category`.
pub fn search_with_category(
    root: &Path,
    query: &str,
    category: Option<&str>,
    local_only: bool,
) -> Result<Vec<(String, String)>, String> {
    let trimmed = query.trim();
    let needle = if trimmed == "*" {
        String::new()
    } else {
        trimmed.to_lowercase()
    };
    let cat_filter = category
        .map(|c| c.trim().to_ascii_lowercase())
        .filter(|c| !c.is_empty());
    let mut found: BTreeMap<String, String> = BTreeMap::new();

    let endpoints = registry::registry_endpoints();
    let remote_eps: Vec<String> = endpoints
        .iter()
        .filter(|ep| registry::is_remote_url(ep))
        .cloned()
        .collect();
    if !local_only && !remote_eps.is_empty() {
        for (name, origin) in registry::search_remote_across_mirrors(&remote_eps, query, category)?
        {
            found.insert(name, origin);
        }
    }

    for ep in endpoints.iter().filter(|ep| !registry::is_remote_url(ep)) {
        let reg = PathBuf::from(ep);
        if let Ok(rd) = std::fs::read_dir(&reg) {
            for entry in rd.filter_map(|e| e.ok()) {
                if !entry.path().is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if valid_pkg_name(&name).is_err() {
                    continue;
                }
                let mut versions: Vec<Version> = std::fs::read_dir(entry.path())
                    .map(|rd| {
                        rd.filter_map(|e| e.ok())
                            .filter(|e| e.path().is_dir())
                            .filter_map(|e| Version::parse(e.file_name().to_str()?).ok())
                            .collect()
                    })
                    .unwrap_or_default();
                versions.sort();
                if versions.is_empty() {
                    continue;
                }
                let latest = versions.last().unwrap().to_string();
                let pkg_info = Manifest::load(&entry.path().join(&latest))
                    .ok()
                    .flatten()
                    .and_then(|m| m.package);
                if let Some(ref cat) = cat_filter {
                    let ok = pkg_info
                        .as_ref()
                        .is_some_and(|p| p.categories.iter().any(|c| c.eq_ignore_ascii_case(cat)));
                    if !ok {
                        continue;
                    }
                }
                let desc = pkg_info
                    .as_ref()
                    .and_then(|p| p.description.as_deref())
                    .unwrap_or("");
                if !needle.is_empty()
                    && !name.to_lowercase().contains(&needle)
                    && !desc.to_lowercase().contains(&needle)
                {
                    continue;
                }
                let ver_list: Vec<String> = versions.iter().map(ToString::to_string).collect();
                found.insert(name, format!("registry ({})", ver_list.join(", ")));
            }
        }
    }

    if cat_filter.is_none() {
        let vendor = root.join("vendor");
        if let Ok(rd) = std::fs::read_dir(&vendor) {
            for entry in rd.filter_map(|e| e.ok()) {
                if !entry.path().is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with(".tmp_") || name.starts_with(".old_") {
                    continue;
                }
                if needle.is_empty() || name.to_lowercase().contains(&needle) {
                    found.entry(name).or_insert_with(|| "vendored".to_string());
                }
            }
        }
    }

    Ok(found.into_iter().collect())
}

/// `spkg search [query] [--local]`.
pub fn search(root: &Path, query: &str, local_only: bool) -> Result<Vec<(String, String)>, String> {
    search_with_category(root, query, None, local_only)
}

/// Backward-compatible alias searching local registry directory + `vendor/`.
pub fn search_local(root: &Path, query: &str) -> Result<Vec<(String, String)>, String> {
    search(root, query, true)
}

#[derive(Debug, Clone)]
pub struct InstalledPkgInfo {
    pub name: String,
    pub version: String,
    pub source: String,
    pub target: Option<String>,
    pub vendored: bool,
    pub verified: bool,
    pub ffi_libs: Vec<String>,
}

/// List locked/vendored packages with their wheel target and checksum status (`spkg list`).
pub fn list_installed(root: &Path) -> Result<Vec<InstalledPkgInfo>, String> {
    let entries = read_lock(root)?;
    let vendor_root = root.join("vendor");
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        valid_pkg_name(&e.name)?;
        let dir = vendor_root.join(&e.name);
        let vendored = std::fs::symlink_metadata(&dir)
            .is_ok_and(|m| !m.file_type().is_symlink() && m.is_dir());
        let verified = if !vendored {
            false
        } else if let Some(ref sha) = e.sha256 {
            let src_res = vendor_src_hash(&dir);
            let (src_ok, actual_src) = match src_res {
                Ok((_, h)) => (h == *sha, Some(h)),
                Err(_) => (false, None),
            };
            let manifest_ok = match (e.manifest_sha256.as_deref(), actual_src.as_deref()) {
                (Some(exp_msha), Some(asrc)) => {
                    let snap = dir.join(".spid3r-manifest.toml");
                    std::fs::symlink_metadata(&snap)
                        .is_ok_and(|m| !m.file_type().is_symlink() && m.is_file())
                        && std::fs::read(&snap)
                            .is_ok_and(|raw| manifest_and_tree_digest(&raw, asrc) == exp_msha)
                }
                (Some(_), None) => false,
                (None, _) => true,
            };
            let native_ok = match e.native_sha256.as_deref() {
                Some(nsha) => {
                    let tgt = e
                        .target
                        .as_deref()
                        .unwrap_or_else(|| super::manifest::host_target_triple());
                    let nd = dir.join("_native").join(tgt);
                    nd.is_dir() && native_tree_hash(&nd).is_ok_and(|h| h == nsha)
                }
                None => true,
            };
            src_ok && manifest_ok && native_ok
        } else {
            !e.source.starts_with("registry+")
        };
        out.push(InstalledPkgInfo {
            name: e.name,
            version: e.version,
            source: e.source,
            target: e.target,
            vendored,
            verified,
            ffi_libs: e.ffi_libs,
        });
    }
    Ok(out)
}

/// Inspect metadata and hashes for a single locked/vendored dependency (`spkg show <name>`).
pub fn show_package(root: &Path, name: &str) -> Result<String, String> {
    valid_pkg_name(name)?;
    let entries = read_lock(root)?;
    let entry = entries
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| format!("package '{}' is not installed in spider.lock", name))?;
    let vendor_dir = root.join("vendor").join(name);
    let snap = vendor_dir.join(".spid3r-manifest.toml");
    let manifest_opt = if snap.is_file() {
        std::fs::read_to_string(&snap)
            .ok()
            .and_then(|raw| parse_manifest(&raw).ok())
    } else {
        None
    };

    let mut out = String::new();
    out.push_str(&format!("Name: {}\n", entry.name));
    out.push_str(&format!("Version: {}\n", entry.version));
    out.push_str(&format!("Source: {}\n", entry.source));
    if let Some(ref tgt) = entry.target {
        out.push_str(&format!("Wheel-Target: {}\n", tgt));
    }
    if let Some(ref m) = manifest_opt {
        if let Some(ref p) = m.package {
            if let Some(ref d) = p.description {
                out.push_str(&format!("Summary: {}\n", d));
            }
            if let Some(ref l) = p.license {
                out.push_str(&format!("License: {}\n", l));
            }
            if !p.authors.is_empty() {
                out.push_str(&format!("Authors: {}\n", p.authors.join(", ")));
            }
            if let Some(ref hp) = p.homepage {
                out.push_str(&format!("Homepage: {}\n", hp));
            }
        }
        let dep_names: Vec<String> = m.deps.iter().map(|d| d.name.clone()).collect();
        out.push_str(&format!(
            "Dependencies: {}\n",
            if dep_names.is_empty() {
                "(none)".to_string()
            } else {
                dep_names.join(", ")
            }
        ));
        if !m.bins.is_empty() {
            let bin_names: Vec<String> = m.bins.keys().cloned().collect();
            out.push_str(&format!("Binaries: {}\n", bin_names.join(", ")));
        }
    }
    if !entry.ffi_libs.is_empty() {
        out.push_str(&format!("FFI-Libs: {}\n", entry.ffi_libs.join(", ")));
    }
    if let Some(ref sha) = entry.sha256 {
        out.push_str(&format!("SHA-256: {}\n", sha));
    }
    if let Some(ref msha) = entry.manifest_sha256 {
        out.push_str(&format!("Manifest-SHA-256: {}\n", msha));
    }
    if let Some(ref nsha) = entry.native_sha256 {
        out.push_str(&format!("Native-SHA-256: {}\n", nsha));
    }
    out.push_str(&format!("Location: {}\n", vendor_dir.display()));
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct OutdatedEntry {
    pub name: String,
    pub current: String,
    pub latest: String,
}

/// Compare locked registry dependencies against latest non-yanked versions (`spkg outdated`).
pub fn outdated_packages(root: &Path, offline: bool) -> Result<Vec<OutdatedEntry>, String> {
    let entries = read_lock(root)?;
    let endpoints = registry::registry_endpoints();
    let mut out = Vec::new();
    for e in entries {
        if !e.source.starts_with("registry+") {
            continue;
        }
        let Ok(cur) = Version::parse(&e.version) else {
            continue;
        };
        let latest = if offline {
            match registry::fetch_cached_versions(root, &endpoints, None, &e.name) {
                Ok((_, vers)) => vers.into_iter().max(),
                Err(_) => None,
            }
        } else {
            let mut best: Option<Version> = None;
            for ep in &endpoints {
                if registry::is_remote_url(ep) {
                    if let Ok((eff, mut vers)) = registry::fetch_versions_across_mirrors(
                        std::slice::from_ref(ep),
                        None,
                        &e.name,
                    ) {
                        if let Ok(yanked) = registry::fetch_remote_yanked(&eff, None, &e.name) {
                            vers.retain(|v| !yanked.contains(v));
                        }
                        if let Some(top) = vers.into_iter().max() {
                            if best.as_ref().is_none_or(|b| top > *b) {
                                best = Some(top);
                            }
                            break;
                        }
                    }
                } else if let Ok(v) = super::resolver::latest_registry_version(&e.name, None) {
                    best = Some(v);
                    break;
                }
            }
            best
        };
        if let Some(top) = latest {
            if top > cur {
                out.push(OutdatedEntry {
                    name: e.name,
                    current: cur.to_string(),
                    latest: top.to_string(),
                });
            }
        }
    }
    Ok(out)
}

/// Return the active global package cache root (`paths.cache_dir` / `SPID3R_CACHE_DIR`).
pub fn cache_dir_path() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    registry::registry_cache_root(&cwd, "__default__")
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cwd.join(".spid3r").join("registry"))
}

fn dir_size_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.filter_map(|e| e.ok()) {
            let p = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                total = total.saturating_add(dir_size_bytes(&p));
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

/// List cached registry buckets and their byte sizes (`spkg cache list`).
pub fn cache_list() -> Result<Vec<(String, u64)>, String> {
    let root = cache_dir_path();
    let mut items = Vec::new();
    if !root.is_dir() {
        return Ok(items);
    }
    for entry in std::fs::read_dir(&root)
        .map_err(|e| format!("cannot read cache '{}': {}", root.display(), e))?
        .filter_map(|e| e.ok())
    {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "__default__" {
            continue;
        }
        items.push((name, dir_size_bytes(&p)));
    }
    items.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(items)
}

/// Remove cached registry entries (`spkg cache prune`), returning `(dirs_removed, bytes_freed)`.
pub fn cache_prune() -> Result<(usize, u64), String> {
    let root = cache_dir_path();
    if !root.is_dir() {
        return Ok((0, 0));
    }
    let mut removed = 0usize;
    let mut freed = 0u64;
    for entry in std::fs::read_dir(&root)
        .map_err(|e| format!("cannot read cache '{}': {}", root.display(), e))?
        .filter_map(|e| e.ok())
    {
        let p = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            continue;
        }
        let bytes = dir_size_bytes(&p);
        std::fs::remove_dir_all(&p).map_err(|e| format!("cannot remove {}: {}", p.display(), e))?;
        removed += 1;
        freed = freed.saturating_add(bytes);
    }
    Ok((removed, freed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spid3r_ops_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn add_creates_the_section_and_is_idempotent() {
        let dir = scratch("add");
        std::fs::write(
            dir.join("spider.toml"),
            "[package]\nname = \"app\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();

        assert!(add_dependency(&dir, "mathlib", "\"../mathlib\"", None).unwrap());
        let text = std::fs::read_to_string(dir.join("spider.toml")).unwrap();
        assert!(text.contains("[dependencies]"), "{text}");
        assert!(text.contains("mathlib = \"../mathlib\""), "{text}");

        assert!(!add_dependency(&dir, "mathlib", "\"../mathlib\"", None).unwrap());
        let again = std::fs::read_to_string(dir.join("spider.toml")).unwrap();
        assert_eq!(text, again);

        let m = super::super::parse_manifest(&again).expect("manifest parses");
        assert_eq!(m.deps.len(), 1);
        assert_eq!(m.deps[0].name, "mathlib");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_keeps_existing_comments_and_order() {
        let dir = scratch("add_comments");
        std::fs::write(
            dir.join("spider.toml"),
            "[package]\nname = \"app\"\nversion = \"1.0.0\"\n\n# pinned on purpose\ndescription = \"x\"\n",
        )
        .unwrap();
        add_dependency(&dir, "util", "{ path = \"../util\" }", None).unwrap();
        let text = std::fs::read_to_string(dir.join("spider.toml")).unwrap();
        assert!(
            text.contains("# pinned on purpose"),
            "comment lost:\n{text}"
        );
        assert!(text.contains("description = \"x\""), "order lost:\n{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_deletes_only_the_named_dependency() {
        let dir = scratch("remove");
        std::fs::write(
            dir.join("spider.toml"),
            "[package]\nname = \"app\"\nversion = \"1.0.0\"\n\n[dependencies]\nkeep = \"../keep\"\ngone = \"../gone\"\n",
        )
        .unwrap();
        assert!(remove_dependency(&dir, "gone").unwrap());
        let text = std::fs::read_to_string(dir.join("spider.toml")).unwrap();
        assert!(text.contains("keep"), "{text}");
        assert!(!text.contains("gone ="), "{text}");
        assert!(!remove_dependency(&dir, "gone").unwrap(), "already removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_lock_from_an_older_writer_still_parses() {
        let dir = scratch("lock");
        std::fs::write(
            dir.join("spider.lock"),
            "# generated by spid3r install; do not edit\n# package app 1.0.0\n\n[[package]]\nname = \"mathlib\"\nversion = \"0.1.0\"\nsource = \"path:../mathlib\"\nfiles = 2\nsha256 = \"abc\"\nfuture-key = \"ignored\"\n",
        )
        .unwrap();
        let entries = read_lock(&dir).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "mathlib");
        assert_eq!(entries[0].files, Some(2));
        assert_eq!(entries[0].sha256.as_deref(), Some("abc"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_detects_a_tampered_vendored_file() {
        let dir = scratch("verify");
        std::fs::create_dir_all(dir.join("vendor/mathlib")).unwrap();
        std::fs::write(dir.join("vendor/mathlib/util.matrix"), "return 1;\n").unwrap();
        let good = content_hash(&dir.join("vendor/mathlib")).unwrap();
        std::fs::write(
            dir.join("spider.lock"),
            format!(
                "[[package]]\nname = \"mathlib\"\nversion = \"0.1.0\"\nsource = \"path:x\"\nfiles = 1\nsha256 = \"{}\"\n",
                good
            ),
        )
        .unwrap();

        let clean = verify(&dir).unwrap();
        assert!(clean.ok(), "clean tree reported {:?}", clean.issues);

        std::fs::write(dir.join("vendor/mathlib/util.matrix"), "return 99;\n").unwrap();
        let dirty = verify(&dir).unwrap();
        assert!(!dirty.ok());
        assert_eq!(dirty.issues[0].kind, "hash");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
