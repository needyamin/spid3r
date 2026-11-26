//! Deterministic `spider.lock` reading and properly escaped writing.
//!
//! `read_lock` stays tolerant of unknown future keys; `write_lock_string` escapes
//! every value via [`super::manifest::toml_escape_string`] so paths, URLs, or
//! package metadata can never spoof extra TOML keys or blocks (audit S4).

use std::collections::BTreeMap;
use std::path::Path;

use super::manifest::toml_escape_string;
use super::semver::Version;

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
    /// Canonical `manifest.sha256` digest (manifest + `src/` tree), if verified.
    pub manifest_sha256: Option<String>,
    /// Platform triple when vendored from a native binary wheel (e.g. `windows-x86_64`).
    pub target: Option<String>,
    /// SHA-256 hash of `vendor/<pkg>/_native/<target>/` binary artifacts, if any.
    pub native_sha256: Option<String>,
    /// Snapshot of declared `[ffi]` library specs for offline audit & build verification.
    pub ffi_libs: Vec<String>,
    /// The workspace member that pulled this dependency in, if any.
    pub used_by: Option<String>,
}

/// Parse `spider.lock`.
///
/// Deliberately tolerant: an unknown key is ignored, and a block without a
/// `name` is skipped so forward/backward compatibility holds.
pub fn read_lock(root: &Path) -> Result<Vec<LockEntry>, String> {
    let path = root.join("spider.lock");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
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
                manifest_sha256: None,
                target: None,
                native_sha256: None,
                ffi_libs: Vec::new(),
                used_by: None,
            });
            continue;
        }
        if line.starts_with('[') {
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
        let value_trimmed = value.trim();
        let parsed_str = super::manifest::parse_string(value_trimmed)
            .unwrap_or_else(|| value_trimmed.trim_matches('"').to_string());
        match key.trim() {
            "name" => entry.name = parsed_str,
            "version" => entry.version = parsed_str,
            "source" => entry.source = parsed_str,
            "used-by" => entry.used_by = Some(parsed_str),
            "sha256" => entry.sha256 = Some(parsed_str),
            "manifest-sha256" => entry.manifest_sha256 = Some(parsed_str),
            "target" => {
                if !parsed_str.is_empty() {
                    entry.target = Some(parsed_str);
                }
            }
            "native-sha256" => {
                if !parsed_str.is_empty() {
                    entry.native_sha256 = Some(parsed_str);
                }
            }
            "ffi-libs" => {
                entry.ffi_libs = parsed_str
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string)
                    .collect();
            }
            "files" => entry.files = value_trimmed.parse().ok(),
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

/// Serialize `spider.lock` safely with proper TOML string escaping.
pub fn write_lock_string(header: &str, entries: &[LockEntry]) -> String {
    let mut out = String::from(header);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    for e in entries {
        out.push_str("\n[[package]]\n");
        out.push_str(&format!("name = \"{}\"\n", toml_escape_string(&e.name)));
        out.push_str(&format!(
            "version = \"{}\"\n",
            toml_escape_string(&e.version)
        ));
        out.push_str(&format!("source = \"{}\"\n", toml_escape_string(&e.source)));
        if let Some(ref used_by) = e.used_by {
            if !used_by.is_empty() {
                out.push_str(&format!("used-by = \"{}\"\n", toml_escape_string(used_by)));
            }
        }
        if let Some(files) = e.files {
            out.push_str(&format!("files = {}\n", files));
        }
        if let Some(ref sha) = e.sha256 {
            out.push_str(&format!("sha256 = \"{}\"\n", toml_escape_string(sha)));
        }
        if let Some(ref msha) = e.manifest_sha256 {
            out.push_str(&format!(
                "manifest-sha256 = \"{}\"\n",
                toml_escape_string(msha)
            ));
        }
        if let Some(ref tgt) = e.target {
            if !tgt.is_empty() {
                out.push_str(&format!("target = \"{}\"\n", toml_escape_string(tgt)));
            }
        }
        if let Some(ref nsha) = e.native_sha256 {
            if !nsha.is_empty() {
                out.push_str(&format!(
                    "native-sha256 = \"{}\"\n",
                    toml_escape_string(nsha)
                ));
            }
        }
        if !e.ffi_libs.is_empty() {
            let joined = e.ffi_libs.join(",");
            out.push_str(&format!("ffi-libs = \"{}\"\n", toml_escape_string(&joined)));
        }
    }
    out
}

/// Map `package name -> locked source` for fast git/registry lock lookups.
pub fn locked_source_map(entries: &[LockEntry]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for e in entries {
        if !e.name.is_empty() && !e.source.is_empty() {
            map.insert(e.name.clone(), e.source.clone());
        }
    }
    map
}

/// Extract a locked 40-hex Git commit for `name` when `source` matches `url`.
pub fn locked_git_commit(
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

/// Extract the locked SemVer `Version` of a registry dependency (fixes B2).
pub fn locked_registry_version(entries: &[LockEntry], name: &str) -> Option<Version> {
    let entry = entries.iter().find(|e| e.name == name)?;
    if !entry.source.starts_with("registry+") {
        return None;
    }
    Version::parse(&entry.version).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_read_roundtrips_escaped_fields() {
        let entry = LockEntry {
            name: "win_gui".to_string(),
            version: "0.1.0".to_string(),
            source: "path+C:\\deps\\win\"gui".to_string(),
            files: Some(3),
            sha256: Some("abc123".to_string()),
            manifest_sha256: Some("def456".to_string()),
            target: Some("windows-x86_64".to_string()),
            native_sha256: Some("fed987".to_string()),
            ffi_libs: vec!["user32".to_string(), "gdi32".to_string()],
            used_by: Some("app".to_string()),
        };
        let raw = write_lock_string("# header\n", std::slice::from_ref(&entry));
        assert!(
            !raw.contains("path+C:\\deps\\win\"gui\n"),
            "unescaped:\n{raw}"
        );

        let dir = std::env::temp_dir().join(format!("spid3r_lock_esc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("spider.lock"), &raw).unwrap();
        let back = read_lock(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(back, vec![entry]);
    }
}
