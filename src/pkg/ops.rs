//! SPM operations: verify, tree, add, remove, publish-to-directory, local
//! search.
//!
//! Everything here works on the same on-disk formats `install` produces — the
//! `spider.toml` manifest, `spider.lock` and `vendor/` — and none of it uses a
//! third-party crate. `install` itself lives in the parent module; this file is
//! the read/inspect/edit half of the package manager.
//!
//! Two rules the whole file obeys:
//!
//! * **A manifest edit is textual.** `add` and `remove` insert or delete one
//!   line inside `[dependencies]` instead of re-serialising the document, so a
//!   user's comments, ordering and formatting survive. A canonicalising
//!   rewrite would be easier and would quietly destroy hand-maintained files.
//! * **Nothing here executes package code.** No build scripts, no hooks. That
//!   is a deliberate security property of this package manager (see
//!   `docs/ECOSYSTEM.md`), not an omission.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{content_hash, valid_pkg_name, Manifest};

/// One `[[package]]` block of `spider.lock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockEntry {
    pub name: String,
    pub version: String,
    pub source: String,
    /// `files = N`, when the writer recorded it.
    pub files: Option<usize>,
    /// The content hash of the dependency's `src/` tree.
    pub sha256: Option<String>,
    /// The workspace member that pulled this dependency in, if any.
    pub used_by: Option<String>,
}

/// Parse `spider.lock`.
///
/// Deliberately tolerant: an unknown key is ignored, and a block without a
/// `name` is skipped. The file is generated, but a lock written by an older
/// `spidr` must still load (that is the whole point of a lock file), so a new
/// key must never make an old lock unreadable — and vice versa.
pub fn read_lock(root: &Path) -> Result<Vec<LockEntry>, String> {
    let path = root.join("spider.lock");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<LockEntry> = Vec::new();
    let mut cur: Option<LockEntry> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("[[package]]") {
            if let Some(e) = cur.take() {
                if !e.name.is_empty() {
                    out.push(e);
                }
            }
            cur = Some(LockEntry {
                name: String::new(),
                version: String::new(),
                source: String::new(),
                files: None,
                sha256: None,
                used_by: None,
            });
            continue;
        }
        if line.starts_with('[') {
            // A new section of any other kind ends the current block.
            if let Some(e) = cur.take() {
                if !e.name.is_empty() {
                    out.push(e);
                }
            }
            continue;
        }
        let Some(entry) = cur.as_mut() else { continue };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_string();
        match key.trim() {
            "name" => entry.name = value,
            "version" => entry.version = value,
            "source" => entry.source = value,
            "used-by" => entry.used_by = Some(value),
            "sha256" => entry.sha256 = Some(value),
            "files" => entry.files = value.parse().ok(),
            _ => {}
        }
    }
    if let Some(e) = cur {
        if !e.name.is_empty() {
            out.push(e);
        }
    }
    Ok(out)
}

/// One problem found by [`verify`].
#[derive(Debug, Clone)]
pub struct VerifyIssue {
    /// The package the issue is about.
    pub name: String,
    /// `missing`, `hash`, `files` or `unlocked`.
    pub kind: &'static str,
    /// A human-readable explanation naming both values.
    pub detail: String,
}

/// The outcome of `spidr verify`.
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

/// Re-hash every vendored dependency against the `sha256` in `spider.lock`.
///
/// This is the integrity check the trust model promises: install verifies a
/// registry package's `sha256.txt`, and `verify` re-checks the vendored tree
/// afterwards, so a hand-edited file under `vendor/` is detected instead of
/// silently compiled into the program.
pub fn verify(root: &Path) -> Result<VerifyReport, String> {
    let entries = read_lock(root)?;
    let mut report = VerifyReport::default();
    let vendor = root.join("vendor");

    for entry in &entries {
        valid_pkg_name(&entry.name)?;
        let dir = vendor.join(&entry.name);
        let Some(expected) = entry.sha256.as_deref() else {
            // A git/path dependency records no hash; nothing to re-check.
            continue;
        };
        report.checked += 1;
        if !dir.is_dir() {
            report.issues.push(VerifyIssue {
                name: entry.name.clone(),
                kind: "missing",
                detail: format!(
                    "vendored at vendor/{} but the directory is missing — run `spidr install`",
                    entry.name
                ),
            });
            continue;
        }
        let actual = content_hash(&dir)?;
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
            let mut found: Vec<PathBuf> = Vec::new();
            collect(&dir, &mut found);
            if found.len() != files {
                report.issues.push(VerifyIssue {
                    name: entry.name.clone(),
                    kind: "files",
                    detail: format!(
                        "vendor/{} has {} file(s), spider.lock records {}",
                        entry.name,
                        found.len(),
                        files
                    ),
                });
            }
        }
    }

    // A vendored directory with no lock entry is a package nobody declared:
    // report it rather than trusting it.
    if vendor.is_dir() {
        let mut names: Vec<String> = std::fs::read_dir(&vendor)
            .map_err(|e| format!("cannot read vendor/: {}", e))?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
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

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.filter_map(|e| e.ok()) {
        let p = entry.path();
        if p.is_dir() {
            collect(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// Render `spidr tree`: the declared dependency tree.
///
/// The lock is flat (one block per package, no edge list), so the tree is
/// rebuilt from the manifests: each dependency's own `spider.toml` is read when
/// it is a path dependency, and a locked version is shown when one exists.
/// `used-by` is used for the workspace case, where several members share one
/// vendor directory.
pub fn dep_tree(root: &Path) -> Result<String, String> {
    let manifest =
        Manifest::load(root)?.ok_or_else(|| format!("no spider.toml in {}", root.display()))?;
    let locked = read_lock(root)?;
    let mut out = String::new();
    out.push_str(&format!("{} {}\n", manifest.name(), manifest.version()));

    let mut visiting: Vec<String> = vec![manifest.name().to_string()];
    render_deps(root, &manifest, &locked, "", &mut visiting, &mut out)?;
    if manifest.deps.is_empty() {
        out.push_str("└─ (no dependencies)\n");
    }
    Ok(out)
}

fn render_deps(
    root: &Path,
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

        // Recurse into a path dependency's own manifest, guarding against a
        // dependency cycle in the manifests themselves.
        if visiting.contains(&dep.name) {
            out.push_str(&format!(
                "{}{}  (cycle: {})\n",
                prefix,
                if last { "   " } else { "│  " },
                dep.name
            ));
            continue;
        }
        let super::DepSource::Path(p) = &dep.source else {
            continue;
        };
        let dep_root = root.join(p);
        let Ok(Some(child)) = Manifest::load(&dep_root) else {
            continue;
        };
        visiting.push(dep.name.clone());
        let next = format!("{}{}", prefix, if last { "   " } else { "│  " });
        render_deps(&dep_root, &child, locked, &next, visiting, out)?;
        visiting.pop();
    }
    Ok(())
}

/// `spidr add <name> [--path P | --git URL [--rev R] | --version V]`.
///
/// The value written is exactly what the manifest parser accepts, so `add` and
/// a hand-written line are indistinguishable afterwards.
pub fn add_dependency(
    root: &Path,
    name: &str,
    value: &str,
    version: Option<&str>,
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
                v
            );
        } else {
            // A bare path string carries no constraint; the table form is the
            // only place a version fits.
            return Err(
                "a version constraint needs the table form, e.g. { path = \"../x\", version = \"^1.2\" }"
                    .to_string(),
            );
        }
    }

    let updated = insert_into_dependencies(&text, name, &entry)?;
    if updated == text {
        return Ok(false); // already declared verbatim
    }
    std::fs::write(&path, updated)
        .map_err(|e| format!("cannot write {}: {}", path.display(), e))?;
    Ok(true)
}

/// Insert `line` into `[dependencies]`, creating the section if needed.
/// Returns the new document; unchanged if `name` is already declared.
fn insert_into_dependencies(text: &str, name: &str, line: &str) -> Result<String, String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut section: Option<(usize, usize)> = None; // (header idx, end idx)
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
    if let Some((_, end)) = section {
        // Recompute the section end: the next section header after the start.
        let start = section.unwrap().0;
        let mut stop = lines.len();
        for (i, raw) in lines.iter().enumerate().skip(start + 1) {
            if raw.trim().starts_with('[') {
                stop = i;
                break;
            }
        }
        let _ = end;
        for raw in lines.iter().take(stop).skip(start + 1) {
            let t = raw.trim();
            if let Some((k, _)) = t.split_once('=') {
                if k.trim() == name {
                    return Ok(text.to_string());
                }
            }
        }
        let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        out.insert(stop, line.to_string());
        return Ok(out.join("\n") + "\n");
    }
    // No [dependencies] section yet: append one.
    let mut out = text.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("\n[dependencies]\n");
    out.push_str(line);
    out.push('\n');
    Ok(out)
}

/// `spidr remove <name>`: delete the declaration from `[dependencies]`.
/// Returns whether a line was removed. `vendor/<name>` is left for the next
/// `install` to prune.
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
        if section == "dependencies" && !removed {
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

/// `spidr publish --to-dir <dir>`: write `<dir>/<name>/<version>/` in the
/// layout the local registry resolver reads, plus a `sha256.txt`.
///
/// No network, no signing: TLS + checksum is the v1 trust model, and this is
/// the local half of it.
pub fn publish_to_dir(root: &Path, dir: &Path) -> Result<(PathBuf, String), String> {
    let manifest =
        Manifest::load(root)?.ok_or_else(|| format!("no spider.toml in {}", root.display()))?;
    valid_pkg_name(manifest.name())?;
    let version = manifest.version().to_string();
    if version.is_empty() {
        return Err("the package has no version".to_string());
    }
    let dest = dir.join(manifest.name()).join(&version);
    if dest.exists() {
        return Err(format!(
            "{} already exists — bump the version instead of overwriting a published release",
            dest.display()
        ));
    }
    std::fs::create_dir_all(&dest)
        .map_err(|e| format!("cannot create {}: {}", dest.display(), e))?;

    // The registry layout is a package tree: spider.toml + src/.
    std::fs::copy(root.join("spider.toml"), dest.join("spider.toml"))
        .map_err(|e| format!("cannot copy spider.toml: {}", e))?;
    let src = root.join("src");
    if !src.is_dir() {
        return Err(format!("no src/ directory in {}", root.display()));
    }
    copy_tree(&src, &dest.join("src"))?;

    let hash = content_hash(&dest.join("src"))?;
    std::fs::write(dest.join("sha256.txt"), format!("{}\n", hash))
        .map_err(|e| format!("cannot write sha256.txt: {}", e))?;
    Ok((dest, hash))
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("cannot create {}: {}", to.display(), e))?;
    for entry in std::fs::read_dir(from)
        .map_err(|e| format!("cannot read {}: {}", from.display(), e))?
        .filter_map(|e| e.ok())
    {
        let p = entry.path();
        let target = to.join(entry.file_name());
        if p.is_dir() {
            copy_tree(&p, &target)?;
        } else {
            std::fs::copy(&p, &target)
                .map_err(|e| format!("cannot copy {}: {}", p.display(), e))?;
        }
    }
    Ok(())
}

/// `spidr search <query> --local`: match package names in the local registry
/// (`SPID3R_REGISTRY`) and in `vendor/`.
pub fn search_local(root: &Path, query: &str) -> Result<Vec<(String, String)>, String> {
    let needle = query.to_lowercase();
    let mut found: BTreeMap<String, String> = BTreeMap::new();

    if let Ok(reg) = std::env::var("SPID3R_REGISTRY") {
        let reg = PathBuf::from(reg);
        if let Ok(rd) = std::fs::read_dir(&reg) {
            for entry in rd.filter_map(|e| e.ok()) {
                if !entry.path().is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.to_lowercase().contains(&needle) {
                    continue;
                }
                let mut versions: Vec<String> = std::fs::read_dir(entry.path())
                    .map(|rd| {
                        rd.filter_map(|e| e.ok())
                            .filter(|e| e.path().is_dir())
                            .map(|e| e.file_name().to_string_lossy().to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                versions.sort();
                found.insert(name, format!("registry ({})", versions.join(", ")));
            }
        }
    }

    let vendor = root.join("vendor");
    if let Ok(rd) = std::fs::read_dir(&vendor) {
        for entry in rd.filter_map(|e| e.ok()) {
            if !entry.path().is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.to_lowercase().contains(&needle) {
                found.entry(name).or_insert_with(|| "vendored".to_string());
            }
        }
    }

    Ok(found.into_iter().collect())
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

        // Adding again must not duplicate the line.
        assert!(!add_dependency(&dir, "mathlib", "\"../mathlib\"", None).unwrap());
        let again = std::fs::read_to_string(dir.join("spider.toml")).unwrap();
        assert_eq!(text, again);

        // The result must parse as a manifest.
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
        assert_eq!(clean.checked, 1);

        std::fs::write(dir.join("vendor/mathlib/util.matrix"), "return 2;\n").unwrap();
        let dirty = verify(&dir).unwrap();
        assert!(!dirty.ok());
        assert_eq!(dirty.issues[0].kind, "hash");
        assert!(
            dirty.issues[0].detail.contains("expected"),
            "{:?}",
            dirty.issues
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
