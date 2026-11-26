//! `spider.toml` manifest model (v1 + v2 ecosystem metadata), quote-aware
//! parser, TOML string escaper, and host compatibility validator.

use std::collections::BTreeMap;
use std::path::Path;

use super::semver::{Constraint, Version};

/// Where a dependency comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum DepSource {
    /// `{ path = "../mathlib" }` or `mathlib = "../mathlib"`.
    Path(String),
    /// `{ git = "https://…", rev = "main" }`.
    Git { url: String, rev: Option<String> },
    /// `{ version = "^1.2" }` — resolved from a package registry.
    Registry {
        version: String,
        registry: Option<String>,
    },
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

/// One declared dependency: its name, source, and optional version constraint.
#[derive(Debug, Clone, PartialEq)]
pub struct Dependency {
    pub name: String,
    pub source: DepSource,
    pub version: Option<String>,
}

/// Foreign-function interface linking configuration declared in `[ffi]`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FfiConfig {
    /// System/linker library names (e.g. `"user32"`, `"m"`, `"openblas"`).
    /// Strictly validated to prevent linker flag injection.
    pub libs: Vec<String>,
    /// Bundled static/import library relative paths under `native/<target>/`.
    pub bundled: Vec<String>,
    /// Companion shared library relative paths under `native/<target>/`
    /// staged alongside the built executable.
    pub runtime_libs: Vec<String>,
}

/// A parsed `spider.toml`.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    /// `None` for a workspace-only root (`[workspace]` without `[package]`).
    pub package: Option<PackageInfo>,
    /// Workspace members, when this manifest declares a workspace.
    pub members: Vec<String>,
    /// Dependencies in declaration order (empty for a workspace-only root).
    pub deps: Vec<Dependency>,
    /// Development-only dependencies (`[dev-dependencies]`, installed with `--dev`).
    pub dev_deps: Vec<Dependency>,
    /// Optional feature-group dependencies (`[optional-dependencies.<extra>]`,
    /// installed with `--extra <extra>`).
    pub optional_deps: BTreeMap<String, Vec<Dependency>>,
    /// Executable binary entries (`[bin]`, mapping command name → `.matrix` path).
    pub bins: BTreeMap<String, String>,
    /// Native FFI linking metadata (`[ffi]`: `libs`, `bundled`, `runtime_libs`).
    pub ffi: FfiConfig,
    /// Legacy alias kept synchronized with `ffi.libs`.
    pub ffi_libs: Vec<String>,
}

/// The `[package]` section (v1 required/core fields + optional v2 ecosystem metadata).
#[derive(Debug, Clone, PartialEq)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    /// Entry file relative to the project root.
    pub entry: String,
    pub description: Option<String>,
    pub license: Option<String>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub spid3r: Option<String>,
    pub authors: Vec<String>,
    pub keywords: Vec<String>,
    pub categories: Vec<String>,
    pub targets: Vec<String>,
}

/// Canonical `<os>-<arch>` triple for target compatibility checks.
pub fn host_target_triple() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "windows-x86_64",
        ("windows", "aarch64") => "windows-aarch64",
        ("windows", "x86") => "windows-x86",
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("macos", "x86_64") => "macos-x86_64",
        ("macos", "aarch64") => "macos-aarch64",
        _ => std::env::consts::OS,
    }
}

impl Manifest {
    /// The `[package]` entry path (defaults to `"src/main.matrix"`).
    pub fn entry(&self) -> &str {
        self.package
            .as_ref()
            .map(|p| p.entry.as_str())
            .unwrap_or("src/main.matrix")
    }

    /// The `[package]` name (empty for a workspace-only root).
    pub fn name(&self) -> &str {
        self.package.as_ref().map(|p| p.name.as_str()).unwrap_or("")
    }

    /// The `[package]` version (empty for a workspace-only root).
    pub fn version(&self) -> &str {
        self.package
            .as_ref()
            .map(|p| p.version.as_str())
            .unwrap_or("")
    }

    /// Verify optional `[package]` `spid3r` toolchain version constraint and
    /// `targets` platform compatibility against the current host.
    pub fn check_host_compat(&self, dep_name: &str) -> Result<(), String> {
        let Some(pkg) = &self.package else {
            return Ok(());
        };
        if let Some(ref req) = pkg.spid3r {
            let c = Constraint::parse(req).map_err(|e| {
                format!(
                    "dependency '{}': invalid package.spid3r '{}': {}",
                    dep_name, req, e
                )
            })?;
            if let Ok(cur) = Version::parse(crate::VERSION) {
                if !c.matches(&cur) {
                    return Err(format!(
                        "dependency '{}' requires Spid3r {}, but current toolchain is {}",
                        dep_name,
                        req,
                        crate::VERSION
                    ));
                }
            }
        }
        if !pkg.targets.is_empty() {
            let triple = host_target_triple();
            let os = std::env::consts::OS;
            let matched = pkg.targets.iter().any(|t| {
                let t = t.trim().to_ascii_lowercase();
                t == "*" || t == "any" || t == triple || t == os
            });
            if !matched {
                return Err(format!(
                    "dependency '{}' only supports targets [{}], which does not match host '{}'",
                    dep_name,
                    pkg.targets.join(", "),
                    triple
                ));
            }
        }
        Ok(())
    }

    /// Load `dir/spider.toml` if one exists.
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

/// Escape an arbitrary string for safe inclusion inside `"..."` in TOML.
/// Control bytes other than `\n`, `\r`, `\t` are dropped so injected bytes
/// cannot forge lines or keys (audit S4 / B8).
pub fn toml_escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\x7f' => {}
            c => out.push(c),
        }
    }
    out
}

/// A package/dependency name becomes a path component under `vendor/`,
/// `.spid3r/git/` and `dist/`. Strict allowlist guards against traversal and
/// Windows reserved device names.
pub fn valid_pkg_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        && !name.starts_with('.')
        && !name.ends_with('.')
        && !is_windows_reserved_stem(name);
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid package name '{}' (letters, digits, '_', '-', '.'; no path separators or reserved names)",
            name
        ))
    }
}

/// Validate a system/linker library name for `[ffi] libs`.
///
/// Allows only safe bare library identifiers (`[A-Za-z0-9_.-]+`, never starting
/// with `-`, `/`, `\`, `@`, or `.`) so untrusted manifests cannot inject linker
/// flags (`/FORCE`, `-Wl,...`, `@resp`) into `cmd_build`.
pub fn validate_ffi_lib_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    let ok = !trimmed.is_empty()
        && trimmed.len() <= 128
        && !trimmed.starts_with(['-', '/', '\\', '@', '.'])
        && !trimmed.contains("..")
        && trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "ffi: invalid library name '{}' (bare library identifier only; flags and paths rejected)",
            name
        ))
    }
}

/// Validate a relative path inside `native/<target>/` for `[ffi] bundled` or
/// `runtime_libs`, blocking traversal and disallowing non-native payloads.
pub fn validate_native_rel_path(rel: &str) -> Result<(), String> {
    let clean = super::tar::validate_archive_path(rel)?;
    let fname = clean
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let allowed = [
        ".lib", ".a", ".dll", ".so", ".dylib", ".h", ".def", ".o", ".obj",
    ]
    .iter()
    .any(|ext| fname.ends_with(ext))
        || fname.rsplit_once(".so.").is_some_and(|(stem, ver)| {
            !stem.is_empty()
                && !ver.is_empty()
                && ver.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        });
    if !allowed {
        return Err(format!(
            "ffi: unsupported native artifact extension in '{}' (allowed: .lib, .a, .dll, .so, .dylib, .h, .def, .o, .obj)",
            rel
        ));
    }
    Ok(())
}

/// Validate a `[bin]` command name and relative `.matrix` / `.mt` entry path.
pub fn validate_bin_entry(name: &str, rel_path: &str) -> Result<(), String> {
    valid_pkg_name(name)?;
    super::tar::validate_archive_path(rel_path)?;
    let lower = rel_path.to_ascii_lowercase();
    if !lower.ends_with(".matrix") && !lower.ends_with(".mt") {
        return Err(format!(
            "bin '{}': entry '{}' must end with .matrix or .mt",
            name, rel_path
        ));
    }
    Ok(())
}

pub(crate) fn is_windows_reserved_stem(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component);
    matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// Parse the manifest subset: `[section]` headers, `key = "string"` values,
/// arrays `["a", "b"]`, inline tables (`{ path = "..." }`), `#` comments.
pub fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut section = String::new();
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut entry: Option<String> = None;
    let mut description: Option<String> = None;
    let mut license: Option<String> = None;
    let mut homepage: Option<String> = None;
    let mut repository: Option<String> = None;
    let mut spid3r_req: Option<String> = None;
    let mut authors: Vec<String> = Vec::new();
    let mut keywords: Vec<String> = Vec::new();
    let mut categories: Vec<String> = Vec::new();
    let mut targets: Vec<String> = Vec::new();
    let mut deps: Vec<Dependency> = Vec::new();
    let mut dev_deps: Vec<Dependency> = Vec::new();
    let mut optional_deps: BTreeMap<String, Vec<Dependency>> = BTreeMap::new();
    let mut bins: BTreeMap<String, String> = BTreeMap::new();
    let mut members: Vec<String> = Vec::new();
    let mut ffi = FfiConfig::default();

    let parse_dep_entry = |key: String, value: &str, where_: &str| -> Result<Dependency, String> {
        let (source, dep_ver) = if value.starts_with('{') {
            let table = parse_inline_table(value)
                .ok_or_else(|| format!("{}: dependency '{}' has a malformed table", where_, key))?;
            let dep_ver = table.get("version").cloned();
            if let Some(v) = &dep_ver {
                Constraint::parse(v)
                    .map_err(|e| format!("{}: dependency '{}': {}", where_, key, e))?;
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
                (None, None) => match &dep_ver {
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
            (source, dep_ver)
        } else {
            let source = DepSource::Path(parse_string(value).ok_or_else(|| {
                format!("{}: dependency '{}' expects a path string", where_, key)
            })?);
            (source, None)
        };
        Ok(Dependency {
            name: key,
            source,
            version: dep_ver,
        })
    };

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
            if let Some(extra) = section.strip_prefix("optional-dependencies.") {
                valid_pkg_name(extra.trim()).map_err(|e| {
                    format!("{}: invalid optional-dependencies group: {}", where_, e)
                })?;
                optional_deps.entry(extra.trim().to_string()).or_default();
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("{}: expected `key = value`", where_));
        };
        let key = key.trim().to_string();
        let value = value.trim();
        match section.as_str() {
            "package" => match key.as_str() {
                "authors" | "keywords" | "categories" | "targets" => {
                    let list = parse_string_array(value)
                        .ok_or_else(|| format!("{}: package.{} expects [\"...\"]", where_, key))?;
                    match key.as_str() {
                        "authors" => authors = list,
                        "keywords" => keywords = list,
                        "categories" => categories = list,
                        "targets" => targets = list,
                        _ => unreachable!(),
                    }
                }
                _ => {
                    let v = parse_string(value).ok_or_else(|| {
                        format!("{}: package.{} expects a quoted string", where_, key)
                    })?;
                    match key.as_str() {
                        "name" => name = Some(v),
                        "version" => version = Some(v),
                        "entry" => entry = Some(v),
                        "description" => description = Some(v),
                        "license" => license = Some(v),
                        "homepage" => homepage = Some(v),
                        "repository" => repository = Some(v),
                        "spid3r" => {
                            Constraint::parse(&v).map_err(|e| {
                                format!("{}: package.spid3r invalid constraint: {}", where_, e)
                            })?;
                            spid3r_req = Some(v);
                        }
                        other => {
                            return Err(format!("{}: unknown package key '{}'", where_, other))
                        }
                    }
                }
            },
            "dependencies" => {
                let dep = parse_dep_entry(key, value, &where_)?;
                if deps.iter().any(|d| d.name == dep.name) {
                    return Err(format!("{}: duplicate dependency '{}'", where_, dep.name));
                }
                deps.push(dep);
            }
            "dev-dependencies" => {
                let dep = parse_dep_entry(key, value, &where_)?;
                if dev_deps.iter().any(|d| d.name == dep.name) {
                    return Err(format!(
                        "{}: duplicate dev-dependency '{}'",
                        where_, dep.name
                    ));
                }
                dev_deps.push(dep);
            }
            "bin" => {
                let rel = parse_string(value).ok_or_else(|| {
                    format!("{}: bin.{} expects a quoted entry path string", where_, key)
                })?;
                validate_bin_entry(&key, &rel).map_err(|e| format!("{}: {}", where_, e))?;
                if bins.insert(key.clone(), rel).is_some() {
                    return Err(format!("{}: duplicate [bin] entry '{}'", where_, key));
                }
            }
            "workspace" => match key.as_str() {
                "members" => {
                    members = parse_string_array(value).ok_or_else(|| {
                        format!("{}: workspace.members expects [\"a\", \"b\"]", where_)
                    })?;
                }
                other => return Err(format!("{}: unknown workspace key '{}'", where_, other)),
            },
            "ffi" => match key.as_str() {
                "libs" => {
                    let list = parse_string_array(value)
                        .ok_or_else(|| format!("{}: ffi.libs expects [\"libfoo\", ...]", where_))?;
                    for lib in &list {
                        validate_ffi_lib_name(lib).map_err(|e| format!("{}: {}", where_, e))?;
                    }
                    ffi.libs = list;
                }
                "bundled" => {
                    let list = parse_string_array(value).ok_or_else(|| {
                        format!("{}: ffi.bundled expects [\"foo.lib\", ...]", where_)
                    })?;
                    for p in &list {
                        validate_native_rel_path(p).map_err(|e| format!("{}: {}", where_, e))?;
                    }
                    ffi.bundled = list;
                }
                "runtime_libs" => {
                    let list = parse_string_array(value).ok_or_else(|| {
                        format!("{}: ffi.runtime_libs expects [\"foo.dll\", ...]", where_)
                    })?;
                    for p in &list {
                        validate_native_rel_path(p).map_err(|e| format!("{}: {}", where_, e))?;
                    }
                    ffi.runtime_libs = list;
                }
                other => return Err(format!("{}: unknown ffi key '{}'", where_, other)),
            },
            s if s.starts_with("optional-dependencies.") => {
                let extra = s.trim_start_matches("optional-dependencies.").trim();
                let dep = parse_dep_entry(key, value, &where_)?;
                let group = optional_deps.entry(extra.to_string()).or_default();
                if group.iter().any(|d| d.name == dep.name) {
                    return Err(format!(
                        "{}: duplicate dependency '{}' in [optional-dependencies.{}]",
                        where_, dep.name, extra
                    ));
                }
                group.push(dep);
            }
            "" => return Err(format!("{}: value outside of a section", where_)),
            other => return Err(format!("{}: unknown section '{}'", where_, other)),
        }
    }

    let package = match name {
        Some(name) => Some(PackageInfo {
            name,
            version: version.unwrap_or_else(|| "0.0.0".to_string()),
            entry: entry.unwrap_or_else(|| "src/main.matrix".to_string()),
            description,
            license,
            homepage,
            repository,
            spid3r: spid3r_req,
            authors,
            keywords,
            categories,
            targets,
        }),
        None if !members.is_empty() => None,
        None => return Err("[package] name is required".to_string()),
    };
    let ffi_libs = ffi.libs.clone();
    Ok(Manifest {
        package,
        members,
        deps,
        dev_deps,
        optional_deps,
        bins,
        ffi,
        ffi_libs,
    })
}

/// Split `inner` on `sep` only outside `"..."` quoted strings (fixes B5).
fn split_outside_quotes(inner: &str, sep: char) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let mut in_str = false;
    let mut escaped = false;
    let mut start = 0usize;
    for (idx, ch) in inner.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_str {
            match ch {
                '\\' => escaped = true,
                '"' => in_str = false,
                _ => {}
            }
        } else {
            match ch {
                '"' => in_str = true,
                c if c == sep => {
                    parts.push(&inner[start..idx]);
                    start = idx + c.len_utf8();
                }
                _ => {}
            }
        }
    }
    if in_str {
        return None;
    }
    parts.push(&inner[start..]);
    Some(parts)
}

/// `["a", "b"]` → `Vec<String>`.
pub(crate) fn parse_string_array(v: &str) -> Option<Vec<String>> {
    let inner = v.trim().strip_prefix('[')?.strip_suffix(']')?;
    let mut out = Vec::new();
    for part in split_outside_quotes(inner, ',')? {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        out.push(parse_string(part)?);
    }
    Some(out)
}

pub(crate) fn strip_comment(line: &str) -> &str {
    let mut in_str = false;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_str => escaped = true,
            '"' => in_str = !in_str,
            '#' if !in_str => return &line[..i],
            _ => {}
        }
    }
    line
}

/// A double-quoted string value, with `\"`, `\\`, `\n`, `\r`, and `\t` escapes.
pub(crate) fn parse_string(v: &str) -> Option<String> {
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
                'r' => out.push('\r'),
                't' => out.push('\t'),
                _ => return None,
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// `{ a = "x", b = "y" }` → map of string values (quote-aware for commas inside strings).
fn parse_inline_table(v: &str) -> Option<BTreeMap<String, String>> {
    let inner = v.trim().strip_prefix('{')?.strip_suffix('}')?;
    let mut out = BTreeMap::new();
    for part in split_outside_quotes(inner, ',')? {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (k, val) = part.split_once('=')?;
        out.insert(k.trim().to_string(), parse_string(val)?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_table_allows_compound_version_commas() {
        let m = parse_manifest(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nml = { version = \">=1.0.0, <2.0.0\" }\n",
        )
        .expect("compound version inside inline table parses");
        assert_eq!(
            m.deps[0].source,
            DepSource::Registry {
                version: ">=1.0.0, <2.0.0".to_string(),
                registry: None
            }
        );
    }

    #[test]
    fn v2_ecosystem_metadata_and_ffi_section_parse() {
        let m = parse_manifest(
            "[package]\nname = \"winview\"\nversion = \"0.2.0\"\nentry = \"src/lib.matrix\"\ndescription = \"Native GUI\"\nlicense = \"MIT\"\nspid3r = \">=1.0.0\"\ncategories = [\"gui\", \"windows\"]\ntargets = [\"windows-x86_64\", \"windows-aarch64\"]\n\n[ffi]\nlibs = [\"user32.dll\", \"gdi32.dll\"]\n",
        )
        .expect("v2 manifest parses");
        let pkg = m.package.unwrap();
        assert_eq!(pkg.categories, vec!["gui", "windows"]);
        assert_eq!(pkg.targets, vec!["windows-x86_64", "windows-aarch64"]);
        assert_eq!(m.ffi_libs, vec!["user32.dll", "gdi32.dll"]);
    }

    #[test]
    fn parses_dev_optional_bin_and_extended_ffi_and_rejects_injection() {
        let m = parse_manifest(
            "[package]\nname = \"numspid3r\"\nversion = \"1.0.0\"\n\n\
             [dependencies]\ncore = { version = \"^1.0\" }\n\n\
             [dev-dependencies]\nunit = \"../unit\"\n\n\
             [optional-dependencies.gpu]\ncuda = { version = \"^2.0\" }\n\n\
             [bin]\nnumcli = \"src/cli.matrix\"\n\n\
             [ffi]\nlibs = [\"m\"]\nbundled = [\"openblas.lib\"]\nruntime_libs = [\"openblas.dll\"]\n",
        )
        .expect("extended manifest parses");
        assert_eq!(m.dev_deps.len(), 1);
        assert_eq!(m.dev_deps[0].name, "unit");
        assert_eq!(m.optional_deps["gpu"].len(), 1);
        assert_eq!(m.optional_deps["gpu"][0].name, "cuda");
        assert_eq!(m.bins["numcli"], "src/cli.matrix");
        assert_eq!(m.ffi.libs, vec!["m"]);
        assert_eq!(m.ffi.bundled, vec!["openblas.lib"]);
        assert_eq!(m.ffi.runtime_libs, vec!["openblas.dll"]);

        assert!(
            parse_manifest("[package]\nname = \"evil\"\n[ffi]\nlibs = [\"-Wl,--evil\"]\n").is_err()
        );
        assert!(
            parse_manifest("[package]\nname = \"evil\"\n[ffi]\nlibs = [\"@resp.txt\"]\n").is_err()
        );
        assert!(
            parse_manifest("[package]\nname = \"evil\"\n[ffi]\nbundled = [\"../evil.lib\"]\n")
                .is_err()
        );
        assert!(parse_manifest(
            "[package]\nname = \"evil\"\n[ffi]\nruntime_libs = [\"evil.exe\"]\n"
        )
        .is_err());
        assert!(
            parse_manifest("[package]\nname = \"evil\"\n[bin]\nbad = \"src/main.exe\"\n").is_err()
        );
    }
}
