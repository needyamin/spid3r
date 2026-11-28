//! Host-portable remote HTTP(S) registry client with lazy single-version
//! downloads (fixes B1), pre-extract Ed25519 signature verification (fixes S2),
//! `registry.toml` host migration + mirror failover (fixes S7), shared global
//! caching, and extended `catalog.txt` search.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::hash::{
    manifest_and_tree_digest, manifest_and_wheel_digest, native_tree_hash, tree_hash,
};
use super::manifest::{
    host_target_triple, parse_string, parse_string_array, strip_comment, valid_pkg_name, Manifest,
};
use super::semver::Version;
use super::sha256::sha256_hex;
use super::tar::extract_tar_checked;
use super::vendor::{configured_trust_pks_for_endpoint, verify_ed25519_against_any};

const MAX_REDIRECTS: usize = 5;
const MAX_MIGRATION_HOPS: usize = 2;
const MAX_METADATA_BYTES: usize = 4 * 1024 * 1024;

type FetchResponse = (u16, Vec<(String, String)>, Vec<u8>);

/// Security advisory entry parsed from `<registry>/advisories.txt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advisory {
    pub package: String,
    pub constraint: String,
    pub severity: String,
    pub id: String,
    pub summary: String,
}

/// Parsed optional `<base>/registry.toml` host portability descriptor.
#[derive(Debug, Clone, Default)]
pub struct RegistryDescriptor {
    pub name: String,
    pub url: String,
    pub moved_to: Option<String>,
    pub mirrors: Vec<String>,
    pub public_key: Option<String>,
}

/// Parsed optional `<base>/<pkg>/<ver>/release.toml` thin-index release descriptor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReleaseDescriptor {
    pub name: String,
    pub version: String,
    pub tarball_url: String,
    pub tarball_sha256: String,
    pub source_repo: String,
    pub source_tag: String,
}

/// True when `spec` points at an HTTP or HTTPS package registry.
pub fn is_remote_url(spec: &str) -> bool {
    let lower = spec.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Read configured `packages.registry` setting, treating `"none"`, `"off"`, or
/// `"disabled"` as disabled and empty values as the compiled default registry.
pub(crate) fn registry_base_setting() -> Option<String> {
    let val = match crate::config::get("packages.registry") {
        Some(v) => v,
        None => return Some(crate::config::schema::DEFAULT_PACKAGE_REGISTRY.to_string()),
    };
    let trimmed = val.trim();
    if trimmed.eq_ignore_ascii_case("none")
        || trimmed.eq_ignore_ascii_case("off")
        || trimmed.eq_ignore_ascii_case("disabled")
    {
        return None;
    }
    if trimmed.is_empty() {
        return Some(crate::config::schema::DEFAULT_PACKAGE_REGISTRY.to_string());
    }
    Some(trimmed.to_string())
}

/// Return all configured registry endpoints (comma-separated primary + mirrors).
pub fn registry_endpoints() -> Vec<String> {
    let Some(raw) = registry_base_setting() else {
        return Vec::new();
    };
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// Shared user-level package cache directory (`paths.cache_dir` /
/// `SPID3R_CACHE_DIR` / platform cache home), falling back safely to
/// `<project_root>/.spid3r/registry/<reg_id>` when unwritable.
pub fn registry_cache_root(project_root: &Path, base_url: &str) -> PathBuf {
    let reg_id = &sha256_hex(base_url.as_bytes())[..16];
    let global_base = std::env::var_os("SPID3R_CACHE_DIR")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            crate::config::get("paths.cache_dir")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        })
        .or_else(default_platform_cache_dir);

    if let Some(base) = global_base {
        let candidate = base.join("registry").join(reg_id);
        if std::fs::create_dir_all(&candidate).is_ok() {
            return candidate;
        }
    }
    project_root.join(".spid3r").join("registry").join(reg_id)
}

fn default_platform_cache_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .map(|b| b.join("Spid3r").join("cache"));
    }
    if let Some(x) = std::env::var_os("XDG_CACHE_HOME") {
        if !x.is_empty() {
            return Some(PathBuf::from(x).join("spid3r"));
        }
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join(".cache").join("spid3r"))
}

fn configured_registry_token() -> Option<String> {
    std::env::var("SPID3R_REGISTRY_TOKEN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn url_origin(url: &str) -> Option<String> {
    let idx = url.find("://")?;
    let scheme = url[..idx].to_ascii_lowercase();
    let rest = &url[idx + 3..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let mut authority = rest[..end].to_ascii_lowercase();
    if authority.is_empty() {
        return None;
    }
    if scheme == "https" {
        if let Some(stripped) = authority.strip_suffix(":443") {
            authority = stripped.to_string();
        }
    } else if scheme == "http" {
        if let Some(stripped) = authority.strip_suffix(":80") {
            authority = stripped.to_string();
        }
    }
    Some(format!("{}://{}", scheme, authority))
}

fn fetch_registry_metadata_bytes(url: &str) -> Result<FetchResponse, String> {
    let resp = fetch_registry_bytes(url)?;
    if resp.2.len() > MAX_METADATA_BYTES {
        return Err(format!(
            "registry: metadata response from '{}' exceeds {} byte cap ({} bytes)",
            url,
            MAX_METADATA_BYTES,
            resp.2.len()
        ));
    }
    Ok(resp)
}

/// Fetch raw HTTP(S) bytes from a registry URL, sending `User-Agent: spkg/...`
/// and optional `Authorization: Bearer <SPID3R_REGISTRY_TOKEN>` (HTTPS only,
/// stripped automatically on any cross-origin redirect), following up to 5 HTTP
/// redirects (`301 | 302 | 303 | 307 | 308`).
pub fn fetch_registry_bytes(url: &str) -> Result<FetchResponse, String> {
    let initial_origin = url_origin(url);
    let token = configured_registry_token();
    let mut current = url.to_string();
    for hop in 0..=MAX_REDIRECTS {
        let mut headers = vec![("User-Agent".to_string(), format!("spkg/{}", crate::VERSION))];
        if let Some(ref tok) = token {
            let cur_lower = current.to_ascii_lowercase();
            if cur_lower.starts_with("https://") && url_origin(&current) == initial_origin {
                headers.push(("Authorization".to_string(), format!("Bearer {}", tok)));
            }
        }
        let resp = crate::stdlib::net::fetch_bytes("GET", &current, &headers, &[])?;
        let status = resp.0;
        if !matches!(status, 301 | 302 | 303 | 307 | 308) {
            return Ok(resp);
        }
        if hop == MAX_REDIRECTS {
            return Err(format!("registry: too many redirects fetching '{}'", url));
        }
        let location = resp
            .1
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("location"))
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                format!(
                    "registry: HTTP {} redirect without Location header for '{}'",
                    status, current
                )
            })?;
        let next = resolve_redirect_url(&current, &location)?;
        if current.to_ascii_lowercase().starts_with("https://")
            && next.to_ascii_lowercase().starts_with("http://")
        {
            return Err(format!(
                "registry: refusing insecure HTTPS -> HTTP redirect from '{}' to '{}'",
                current, next
            ));
        }
        current = next;
    }
    Err(format!("registry: too many redirects fetching '{}'", url))
}

fn verify_optional_metadata_sig(url: &str, body: &[u8], label: &str) -> Result<(), String> {
    let pks = configured_trust_pks_for_endpoint(Some(url))?;
    if pks.is_empty() {
        return Ok(());
    }
    let sig_url = format!("{}.sig", url);
    let (status, _, sig_bytes) = fetch_registry_metadata_bytes(&sig_url).map_err(|e| {
        format!(
            "{}: failed to fetch required signature {} ({})",
            label, sig_url, e
        )
    })?;
    if status != 200 {
        return Err(format!(
            "{}: missing required signature {} (HTTP {}, enforced by packages.trust_pk)",
            label, sig_url, status
        ));
    }
    let sig_str = String::from_utf8_lossy(&sig_bytes);
    verify_ed25519_against_any(&pks, body, &sig_str, &format!("{} ({})", label, sig_url))?;
    Ok(())
}

/// Record effective `moved_to` target under the original endpoint's cache root
/// so `--offline` lookups still find migrated cached packages.
pub fn record_migration_cache(project_root: &Path, initial_ep: &str, effective_ep: &str) {
    let orig = initial_ep.trim().trim_end_matches('/');
    let eff = effective_ep.trim().trim_end_matches('/');
    if orig.is_empty() || eff.is_empty() || orig == eff {
        return;
    }
    let cache_root = registry_cache_root(project_root, orig);
    let _ = std::fs::write(cache_root.join(".migrated_to"), format!("{}\n", eff));
}

fn resolve_redirect_url(current: &str, location: &str) -> Result<String, String> {
    let loc_lower = location.to_ascii_lowercase();
    if loc_lower.starts_with("http://") || loc_lower.starts_with("https://") {
        return Ok(location.to_string());
    }
    if loc_lower.contains("://") || loc_lower.starts_with("//") {
        return Err(format!(
            "registry: refusing unsupported redirect target '{}'",
            location
        ));
    }
    let scheme_end = current
        .find("://")
        .ok_or_else(|| format!("registry: malformed URL '{}'", current))?;
    let rest_start = scheme_end + 3;
    let origin_end = current[rest_start..]
        .find('/')
        .map(|i| rest_start + i)
        .unwrap_or(current.len());
    let origin = &current[..origin_end];
    if location.starts_with('/') {
        return Ok(format!("{}{}", origin, location));
    }
    let dir_end = current[origin_end..]
        .rfind('/')
        .map(|i| origin_end + i + 1)
        .unwrap_or(origin_end);
    let base_dir = &current[..dir_end];
    if base_dir.ends_with('/') {
        Ok(format!("{}{}", base_dir, location))
    } else {
        Ok(format!("{}/{}", base_dir, location))
    }
}

pub(crate) fn build_registry_base(
    registry_url: &str,
    sub_registry: Option<&str>,
) -> Result<String, String> {
    let mut base = registry_url.trim().trim_end_matches('/').to_string();
    if let Some(sub) = sub_registry {
        valid_pkg_name(sub)?;
        base = format!("{}/{}", base, sub);
    }
    Ok(base)
}

/// Parse `<base>/<pkg>/<ver>/release.toml` thin-index release descriptor safely.
pub fn parse_release_descriptor(text: &str) -> ReleaseDescriptor {
    let mut desc = ReleaseDescriptor::default();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    for raw in text.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() || line.starts_with('[') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim();
        let val = v.trim();
        match key {
            "name" => {
                if let Some(s) = parse_string(val) {
                    desc.name = s.trim().to_string();
                }
            }
            "version" => {
                if let Some(s) = parse_string(val) {
                    desc.version = s.trim().to_string();
                }
            }
            "tarball_url" => {
                if let Some(s) = parse_string(val) {
                    desc.tarball_url = s.trim().to_string();
                }
            }
            "tarball_sha256" => {
                if let Some(s) = parse_string(val) {
                    desc.tarball_sha256 = s.trim().to_string();
                }
            }
            "source_repo" => {
                if let Some(s) = parse_string(val) {
                    desc.source_repo = s.trim().to_string();
                }
            }
            "source_tag" => {
                if let Some(s) = parse_string(val) {
                    desc.source_tag = s.trim().to_string();
                }
            }
            _ => {}
        }
    }
    desc
}

/// Parse `<base>/registry.toml` host descriptor safely.
pub fn parse_registry_descriptor(text: &str) -> RegistryDescriptor {
    let mut desc = RegistryDescriptor::default();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    for raw in text.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() || line.starts_with('[') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim();
        let val = v.trim();
        match key {
            "name" => {
                if let Some(s) = parse_string(val) {
                    desc.name = s;
                }
            }
            "url" => {
                if let Some(s) = parse_string(val) {
                    desc.url = s;
                }
            }
            "moved_to" => {
                if let Some(s) = parse_string(val) {
                    let t = s.trim().to_string();
                    if !t.is_empty() {
                        desc.moved_to = Some(t);
                    }
                }
            }
            "public_key" => {
                if let Some(s) = parse_string(val) {
                    let t = s.trim().to_string();
                    if !t.is_empty() {
                        desc.public_key = Some(t);
                    }
                }
            }
            "mirrors" => {
                if let Some(arr) = parse_string_array(val) {
                    desc.mirrors = arr;
                }
            }
            _ => {}
        }
    }
    desc
}

/// Resolve effective registry base URL (honoring `registry.toml` `moved_to`
/// migrations safely and refusing `https://` -> `http://` downgrades) plus
/// any fallback mirrors advertised by `registry.toml`.
pub fn resolve_migrated_base(initial_url: &str) -> Result<(String, Vec<String>), String> {
    let mut current = initial_url.trim().trim_end_matches('/').to_string();
    let mut extra_mirrors: Vec<String> = Vec::new();
    for _hop in 0..MAX_MIGRATION_HOPS {
        let desc_url = format!("{}/registry.toml", current);
        let (status, _, bytes) = match fetch_registry_metadata_bytes(&desc_url) {
            Ok(resp) => resp,
            Err(_) => break,
        };
        if status == 404 {
            break;
        }
        if status != 200 {
            return Err(format!(
                "registry '{}': unexpected HTTP {} fetching {}",
                current, status, desc_url
            ));
        }
        verify_optional_metadata_sig(
            &desc_url,
            &bytes,
            &format!("registry descriptor '{}'", desc_url),
        )?;
        let desc = parse_registry_descriptor(&String::from_utf8_lossy(&bytes));
        let cur_is_https = current.to_ascii_lowercase().starts_with("https://");
        for m in desc.mirrors {
            let trimmed = m.trim().trim_end_matches('/').to_string();
            if is_remote_url(&trimmed) {
                if cur_is_https && trimmed.to_ascii_lowercase().starts_with("http://") {
                    return Err(format!(
                        "registry '{}': refusing insecure http:// mirror '{}' advertised by HTTPS registry.toml",
                        current, trimmed
                    ));
                }
                if !extra_mirrors.contains(&trimmed) {
                    extra_mirrors.push(trimmed);
                }
            }
        }
        let Some(next_raw) = desc.moved_to else {
            break;
        };
        let next = next_raw.trim().trim_end_matches('/').to_string();
        if !is_remote_url(&next) {
            return Err(format!(
                "registry '{}': invalid moved_to target '{}' (must be http(s)://)",
                current, next
            ));
        }
        if cur_is_https && next.to_ascii_lowercase().starts_with("http://") {
            return Err(format!(
                "registry '{}': refusing insecure HTTPS -> HTTP migration moved_to '{}'",
                current, next
            ));
        }
        if next == current {
            break;
        }
        eprintln!("note: registry '{}' moved to '{}'", current, next);
        current = next;
    }
    Ok((current, extra_mirrors))
}

/// Expand `endpoints` (comma-separated configured mirrors + `registry.toml`
/// migrations/mirrors) and run `op(candidate_url)` across candidates on
/// network/5xx errors.
pub fn fetch_versions_across_mirrors(
    endpoints: &[String],
    sub_registry: Option<&str>,
    name: &str,
) -> Result<(String, Vec<Version>), String> {
    valid_pkg_name(name)?;
    let mut last_err = None;
    for ep in endpoints {
        if !is_remote_url(ep) {
            continue;
        }
        let (effective_root, advertised_mirrors) = resolve_migrated_base(ep)?;
        let mut try_urls = vec![effective_root];
        for m in advertised_mirrors {
            if !try_urls.contains(&m) {
                try_urls.push(m);
            }
        }
        for url in try_urls {
            match fetch_remote_versions(&url, sub_registry, name) {
                Ok(vers) => return Ok((url, vers)),
                Err(e) => {
                    // Immediate propagation when the endpoint explicitly returned HTTP 404.
                    if e.contains("no package '") && endpoints.len() == 1 {
                        return Err(e);
                    }
                    last_err = Some(e);
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| format!("registry: no package '{}' found", name)))
}

/// Fetch ONLY `<base>/<name>/index.txt` from `registry_url` and return sorted `Version`s.
pub fn fetch_remote_versions(
    registry_url: &str,
    sub_registry: Option<&str>,
    name: &str,
) -> Result<Vec<Version>, String> {
    valid_pkg_name(name)?;
    let base = build_registry_base(registry_url, sub_registry)?;
    let index_url = format!("{}/{}/index.txt", base, name);
    let (status, _, index_bytes) = fetch_registry_metadata_bytes(&index_url).map_err(|e| {
        format!(
            "registry: cannot reach remote registry '{}' ({}): {}",
            base, index_url, e
        )
    })?;
    if status == 404 {
        return Err(format!(
            "registry: no package '{}' (looked in {})",
            name, base
        ));
    }
    if status != 200 {
        return Err(format!(
            "registry: failed to fetch {} (HTTP {})",
            index_url, status
        ));
    }

    let index_text = String::from_utf8_lossy(&index_bytes);
    verify_optional_metadata_sig(
        &index_url,
        &index_bytes,
        &format!("registry index '{}' ({})", name, index_url),
    )?;
    let mut versions: Vec<Version> = Vec::new();
    for line in index_text.lines() {
        let ver = line.trim();
        if ver.is_empty() || ver.starts_with('#') {
            continue;
        }
        let parsed = Version::parse(ver).map_err(|e| {
            format!(
                "registry: invalid version '{}' in {}: {}",
                ver, index_url, e
            )
        })?;
        versions.push(parsed);
    }
    if versions.is_empty() {
        return Err(format!(
            "registry: no versions listed for '{}' at {}",
            name, index_url
        ));
    }
    versions.sort();
    Ok(versions)
}

/// Fetch optional `<base>/<name>/yanked.txt` returning the set of yanked versions.
pub fn fetch_remote_yanked(
    registry_url: &str,
    sub_registry: Option<&str>,
    name: &str,
) -> Result<BTreeSet<Version>, String> {
    valid_pkg_name(name)?;
    let base = build_registry_base(registry_url, sub_registry)?;
    let url = format!("{}/{}/yanked.txt", base, name);
    let (status, _, bytes) = fetch_registry_metadata_bytes(&url)
        .map_err(|e| format!("yanked list '{}' ({}): {}", name, url, e))?;
    if status == 404 {
        return Ok(BTreeSet::new());
    }
    if status != 200 {
        return Err(format!("yanked list '{}' ({}): HTTP {}", name, url, status));
    }
    verify_optional_metadata_sig(&url, &bytes, &format!("yanked list '{}' ({})", name, url))?;
    let mut yanked = BTreeSet::new();
    for line in String::from_utf8_lossy(&bytes).lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Ok(v) = Version::parse(t) {
            yanked.insert(v);
        }
    }
    Ok(yanked)
}

/// Parse `<base>/advisories.txt` TSV lines (`pkg\tconstraint\tseverity\tid\tsummary`).
pub fn parse_advisories(text: &str) -> Vec<Advisory> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 5 {
            continue;
        }
        out.push(Advisory {
            package: cols[0].trim().to_string(),
            constraint: cols[1].trim().to_string(),
            severity: cols[2].trim().to_ascii_lowercase(),
            id: cols[3].trim().to_string(),
            summary: cols[4].trim().to_string(),
        });
    }
    out
}

/// Fetch optional `<registry>/advisories.txt` across configured remote endpoints.
pub fn fetch_remote_advisories(endpoints: &[String]) -> Result<Vec<Advisory>, String> {
    let mut all = Vec::new();
    for ep in endpoints {
        if !is_remote_url(ep) {
            continue;
        }
        let (effective, _) = resolve_migrated_base(ep)?;
        let base = build_registry_base(&effective, None)?;
        let url = format!("{}/advisories.txt", base);
        let (status, _, bytes) = fetch_registry_metadata_bytes(&url)
            .map_err(|e| format!("advisories ({}): {}", url, e))?;
        if status == 404 {
            continue;
        }
        if status != 200 {
            return Err(format!("advisories ({}): HTTP {}", url, status));
        }
        verify_optional_metadata_sig(&url, &bytes, &format!("advisories ({})", url))?;
        all.extend(parse_advisories(&String::from_utf8_lossy(&bytes)));
    }
    Ok(all)
}

/// Read cached versions strictly from disk (used by `spkg install --offline`).
pub fn fetch_cached_versions(
    project_root: &Path,
    endpoints: &[String],
    sub_registry: Option<&str>,
    name: &str,
) -> Result<(PathBuf, Vec<Version>), String> {
    valid_pkg_name(name)?;
    let mut candidate_dirs: Vec<PathBuf> = Vec::new();
    for ep in endpoints {
        if is_remote_url(ep) {
            let base = build_registry_base(ep, sub_registry)?;
            candidate_dirs.push(registry_cache_root(project_root, &base).join(name));
            let orig_root = registry_cache_root(project_root, ep.trim().trim_end_matches('/'));
            if let Ok(migrated) = std::fs::read_to_string(orig_root.join(".migrated_to")) {
                let mig = migrated.trim();
                if !mig.is_empty() && is_remote_url(mig) {
                    if let Ok(mig_base) = build_registry_base(mig, sub_registry) {
                        candidate_dirs
                            .push(registry_cache_root(project_root, &mig_base).join(name));
                    }
                }
            }
        } else {
            let base = match sub_registry {
                Some(sub) => {
                    valid_pkg_name(sub)?;
                    PathBuf::from(ep).join(sub)
                }
                None => PathBuf::from(ep),
            };
            candidate_dirs.push(base.join(name));
        }
    }
    for cache_pkg_dir in candidate_dirs {
        if !cache_pkg_dir.is_dir() {
            continue;
        }
        let mut versions = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&cache_pkg_dir) {
            for entry in rd.filter_map(|e| e.ok()) {
                let p = entry.path();
                if !p.is_dir() || !p.join("spider.toml").is_file() {
                    continue;
                }
                if let Some(d) = entry.file_name().to_str() {
                    if let Ok(v) = Version::parse(d) {
                        versions.push(v);
                    }
                }
            }
        }
        if !versions.is_empty() {
            versions.sort();
            return Ok((cache_pkg_dir, versions));
        }
    }
    Err(format!(
        "offline mode: package '{}' is not present in local cache or registry directory",
        name
    ))
}

/// Search `<base>/catalog.txt` across a remote endpoint, optionally filtering by category.
pub fn search_remote_catalog(
    registry_url: &str,
    query: &str,
    category: Option<&str>,
) -> Result<Vec<(String, String)>, String> {
    let (effective, _) = resolve_migrated_base(registry_url)?;
    let base = build_registry_base(&effective, None)?;
    let catalog_url = format!("{}/catalog.txt", base);
    let (status, _, bytes) = fetch_registry_metadata_bytes(&catalog_url).map_err(|e| {
        format!(
            "registry: cannot reach remote catalog '{}' ({}): {}",
            base, catalog_url, e
        )
    })?;
    if status == 404 {
        return Ok(Vec::new());
    }
    if status != 200 {
        return Err(format!(
            "registry: failed to fetch {} (HTTP {})",
            catalog_url, status
        ));
    }
    verify_optional_metadata_sig(
        &catalog_url,
        &bytes,
        &format!("registry catalog '{}'", catalog_url),
    )?;

    let trimmed_q = query.trim();
    let needle = if trimmed_q == "*" {
        String::new()
    } else {
        trimmed_q.to_ascii_lowercase()
    };
    let cat_filter = category
        .map(|c| c.trim().to_ascii_lowercase())
        .filter(|c| !c.is_empty());

    let text = String::from_utf8_lossy(&bytes);
    let mut hits = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 2 {
            continue;
        }
        let name = cols[0].trim();
        if name.is_empty() {
            continue;
        }
        let latest = cols[1].trim();
        let versions = if cols.len() >= 3 && !cols[2].trim().is_empty() {
            cols[2].trim().replace(',', ", ")
        } else {
            latest.to_string()
        };
        let desc = cols.get(3).map(|s| s.trim()).unwrap_or("");
        let cats = cols.get(4).map(|s| s.trim()).unwrap_or("");

        if let Some(ref cat) = cat_filter {
            let matches_cat = cats
                .split(',')
                .any(|c| c.trim().to_ascii_lowercase() == *cat);
            if !matches_cat {
                continue;
            }
        }
        if !needle.is_empty()
            && !name.to_ascii_lowercase().contains(&needle)
            && !desc.to_ascii_lowercase().contains(&needle)
            && !cats.to_ascii_lowercase().contains(&needle)
        {
            continue;
        }
        let mut origin = format!("registry ({})", versions);
        if !cats.is_empty() {
            origin.push_str(&format!(" [{}]", cats.replace(',', ", ")));
        }
        if !desc.is_empty() {
            origin.push_str(&format!(" — {}", desc));
        }
        hits.push((name.to_string(), origin));
    }
    Ok(hits)
}

/// Search across configured remote registry mirrors and merge matches.
pub fn search_remote_across_mirrors(
    endpoints: &[String],
    query: &str,
    category: Option<&str>,
) -> Result<Vec<(String, String)>, String> {
    let mut last_err = None;
    for ep in endpoints {
        if !is_remote_url(ep) {
            continue;
        }
        match search_remote_catalog(ep, query, category) {
            Ok(hits) => return Ok(hits),
            Err(e) => last_err = Some(e),
        }
    }
    if let Some(e) = last_err {
        return Err(e);
    }
    Ok(Vec::new())
}

fn cached_version_usable(ver_dir: &Path, require_sigs: bool) -> bool {
    if !ver_dir.is_dir() {
        return false;
    }
    let sha_p = ver_dir.join("sha256.txt");
    let toml_p = ver_dir.join("spider.toml");
    if !sha_p.is_file() || !toml_p.is_file() {
        return false;
    }
    let Ok(sha_txt) = std::fs::read_to_string(&sha_p) else {
        return false;
    };
    let trimmed = sha_txt.trim();
    if trimmed.is_empty() || tree_hash(ver_dir).ok().as_deref() != Some(trimmed) {
        return false;
    }
    if require_sigs {
        if !ver_dir.join("sha256.txt.sig").is_file()
            || !ver_dir.join("manifest.sha256").is_file()
            || !ver_dir.join("manifest.sha256.sig").is_file()
        {
            return false;
        }
        let triple = host_target_triple();
        let wsha = ver_dir.join(format!("sha256-{}.txt", triple));
        if wsha.is_file()
            && (!ver_dir.join(format!("sha256-{}.txt.sig", triple)).is_file()
                || !ver_dir
                    .join(format!("manifest-{}.sha256", triple))
                    .is_file()
                || !ver_dir
                    .join(format!("manifest-{}.sha256.sig", triple))
                    .is_file())
        {
            return false;
        }
    }
    true
}

/// Lazily fetch, verify (Ed25519 BEFORE extracting tarball!), and cache a single
/// resolved version `<name>/<ver>/` (fixes B1 & S2). Returns the cached `<name>/<ver>/` directory.
pub fn fetch_remote_version(
    project_root: &Path,
    registry_url: &str,
    sub_registry: Option<&str>,
    name: &str,
    ver: &str,
    update: bool,
) -> Result<PathBuf, String> {
    valid_pkg_name(name)?;
    Version::parse(ver)?;
    let base = build_registry_base(registry_url, sub_registry)?;
    let cache_root = registry_cache_root(project_root, &base);
    let pkg_cache_dir = cache_root.join(name);
    std::fs::create_dir_all(&pkg_cache_dir)
        .map_err(|e| format!("cannot create {}: {}", pkg_cache_dir.display(), e))?;

    let pub_cache_path = cache_root.join("registry.pub");
    if update || !pub_cache_path.is_file() {
        let pub_url = format!("{}/registry.pub", base);
        if let Ok((200, _, pub_bytes)) = fetch_registry_metadata_bytes(&pub_url) {
            let _ = std::fs::write(&pub_cache_path, &pub_bytes);
        }
    }

    let ver_dir = pkg_cache_dir.join(ver);
    let require_sigs = !configured_trust_pks_for_endpoint(Some(&base))?.is_empty();
    if !update && cached_version_usable(&ver_dir, require_sigs) {
        if !require_sigs {
            eprintln!(
                "warning[pkg]: installing '{}@{}' from local registry cache without Ed25519 signature verification (packages.trust_pk is unset); set packages.trust_pk to pin trusted registry keys",
                name, ver
            );
        }
        return Ok(ver_dir);
    }

    fetch_and_verify_version(&pkg_cache_dir, &base, name, ver, &ver_dir)?;
    Ok(ver_dir)
}

/// Compatibility helper wrapping [`fetch_remote_versions`] + [`fetch_remote_version`].
pub fn sync_remote_package(
    project_root: &Path,
    registry_url: &str,
    sub_registry: Option<&str>,
    name: &str,
    update: bool,
) -> Result<PathBuf, String> {
    let base = build_registry_base(registry_url, sub_registry)?;
    let versions = fetch_remote_versions(registry_url, sub_registry, name)?;
    for v in &versions {
        let _ = fetch_remote_version(
            project_root,
            registry_url,
            sub_registry,
            name,
            &v.to_string(),
            update,
        )?;
    }
    Ok(registry_cache_root(project_root, &base))
}

fn fetch_and_verify_version(
    pkg_cache_dir: &Path,
    base_url: &str,
    name: &str,
    ver: &str,
    ver_dir: &Path,
) -> Result<(), String> {
    let sha_url = format!("{}/{}/{}/sha256.txt", base_url, name, ver);
    let (sha_status, _, sha_bytes) = fetch_registry_bytes(&sha_url).map_err(|e| {
        format!(
            "registry dependency '{}': failed to fetch {}: {}",
            name, sha_url, e
        )
    })?;
    if sha_status != 200 {
        return Err(format!(
            "registry dependency '{}': missing sha256.txt at {} (HTTP {})",
            name, sha_url, sha_status
        ));
    }
    let sha_text = String::from_utf8_lossy(&sha_bytes).trim().to_string();
    if sha_text.is_empty() {
        return Err(format!(
            "registry dependency '{}': sha256.txt is empty at {} — refusing to install unverified code",
            name, sha_url
        ));
    }

    // Fetch detached Ed25519 signature over exact ASCII sha256.txt bytes AND
    // verify BEFORE downloading or extracting package.tar (audit S2).
    let sig_url = format!("{}/{}/{}/sha256.txt.sig", base_url, name, ver);
    let sig_bytes_opt = match fetch_registry_metadata_bytes(&sig_url) {
        Ok((200, _, bytes)) => Some(bytes),
        _ => None,
    };

    let pks = configured_trust_pks_for_endpoint(Some(base_url))?;
    if !pks.is_empty() {
        let sig_bytes = sig_bytes_opt.as_ref().ok_or_else(|| {
            format!(
                "registry dependency '{}': missing {} required by packages.trust_pk",
                name, sig_url
            )
        })?;
        let sig_str = String::from_utf8_lossy(sig_bytes);
        verify_ed25519_against_any(
            &pks,
            &sha_bytes,
            &sig_str,
            &format!("registry dependency '{}' ({})", name, sig_url),
        )?;
    }

    // manifest.sha256 + manifest.sha256.sig binding spider.toml + src/ (required when trust_pk is set).
    let msha_url = format!("{}/{}/{}/manifest.sha256", base_url, name, ver);
    let msha_bytes_opt = match fetch_registry_metadata_bytes(&msha_url) {
        Ok((200, _, bytes)) => Some(bytes),
        _ => None,
    };
    let msig_url = format!("{}/{}/{}/manifest.sha256.sig", base_url, name, ver);
    let msig_bytes_opt = if msha_bytes_opt.is_some() {
        match fetch_registry_metadata_bytes(&msig_url) {
            Ok((200, _, bytes)) => Some(bytes),
            _ => None,
        }
    } else {
        None
    };

    if !pks.is_empty() {
        let msha_bytes = msha_bytes_opt.as_ref().ok_or_else(|| {
            format!(
                "registry dependency '{}': missing {} required by packages.trust_pk",
                name, msha_url
            )
        })?;
        let msig_bytes = msig_bytes_opt.as_ref().ok_or_else(|| {
            format!(
                "registry dependency '{}': missing {} required by packages.trust_pk",
                name, msig_url
            )
        })?;
        verify_ed25519_against_any(
            &pks,
            msha_bytes,
            &String::from_utf8_lossy(msig_bytes),
            &format!("registry dependency '{}' ({})", name, msig_url),
        )?;
    }

    struct WheelBundle {
        wsha_bytes: Vec<u8>,
        wsha_text: String,
        wsig_opt: Option<Vec<u8>>,
        wmsha_pair: Option<(Vec<u8>, Option<Vec<u8>>)>,
    }

    // Prefer a prebuilt host wheel (`sha256-<target>.txt` + `package-<target>.tar`)
    // when published, seamlessly falling back to universal `package.tar`.
    let triple = host_target_triple();
    let wsha_url = format!("{}/{}/{}/sha256-{}.txt", base_url, name, ver, triple);
    let wheel_probe = fetch_registry_metadata_bytes(&wsha_url);
    let mut wheel_bundle: Option<WheelBundle> = None;
    let (tar_bytes, is_wheel) = if let Ok((200, _, wsha_bytes)) = wheel_probe {
        let wsha_text = String::from_utf8_lossy(&wsha_bytes).trim().to_string();
        if wsha_text.is_empty() {
            return Err(format!(
                "registry wheel '{}@{}': sha256-{}.txt is empty",
                name, triple, triple
            ));
        }
        let wsig_url = format!("{}/{}/{}/sha256-{}.txt.sig", base_url, name, ver, triple);
        let wsig_bytes_opt = match fetch_registry_metadata_bytes(&wsig_url) {
            Ok((200, _, b)) => Some(b),
            _ => None,
        };
        if !pks.is_empty() {
            let wsig = wsig_bytes_opt.as_ref().ok_or_else(|| {
                format!(
                    "registry wheel '{}@{}': missing {} required by packages.trust_pk",
                    name, triple, wsig_url
                )
            })?;
            verify_ed25519_against_any(
                &pks,
                &wsha_bytes,
                &String::from_utf8_lossy(wsig),
                &format!("registry wheel '{}@{}' ({})", name, triple, wsig_url),
            )?;
        }
        let wmsha_url = format!("{}/{}/{}/manifest-{}.sha256", base_url, name, ver, triple);
        let wmsig_url = format!(
            "{}/{}/{}/manifest-{}.sha256.sig",
            base_url, name, ver, triple
        );
        let wmsha_pair = match fetch_registry_metadata_bytes(&wmsha_url) {
            Ok((200, _, wmsha_bytes)) => {
                let wmsig = match fetch_registry_metadata_bytes(&wmsig_url) {
                    Ok((200, _, b)) => Some(b),
                    _ => None,
                };
                if !pks.is_empty() {
                    let sig_b = wmsig.as_ref().ok_or_else(|| {
                        format!(
                            "registry wheel '{}@{}': missing {} required by packages.trust_pk",
                            name, triple, wmsig_url
                        )
                    })?;
                    verify_ed25519_against_any(
                        &pks,
                        &wmsha_bytes,
                        &String::from_utf8_lossy(sig_b),
                        &format!("registry wheel '{}@{}' ({})", name, triple, wmsig_url),
                    )?;
                }
                Some((wmsha_bytes, wmsig))
            }
            _ => {
                if !pks.is_empty() {
                    return Err(format!(
                        "registry wheel '{}@{}': missing {} required by packages.trust_pk",
                        name, triple, wmsha_url
                    ));
                }
                None
            }
        };
        let wtar_url = format!("{}/{}/{}/package-{}.tar", base_url, name, ver, triple);
        let (wtar_status, _, wtar_bytes) = fetch_registry_bytes(&wtar_url).map_err(|e| {
            format!(
                "registry wheel '{}@{}': failed to fetch {}: {}",
                name, triple, wtar_url, e
            )
        })?;
        if wtar_status != 200 {
            return Err(format!(
                "registry wheel '{}@{}': failed to download {} (HTTP {})",
                name, triple, wtar_url, wtar_status
            ));
        }
        wheel_bundle = Some(WheelBundle {
            wsha_bytes,
            wsha_text,
            wsig_opt: wsig_bytes_opt,
            wmsha_pair,
        });
        (wtar_bytes, true)
    } else {
        // Thin-index release support: if `<base>/<name>/<ver>/release.toml` exists,
        // verify its Ed25519 signature (when trust_pk is configured), download the
        // external tarball_url (e.g. GitHub Release or R2/CDN), and verify tarball_sha256.
        // Otherwise seamlessly fall back to legacy in-repo `package.tar`.
        let release_url = format!("{}/{}/{}/release.toml", base_url, name, ver);
        if let Ok((200, _, rel_bytes)) = fetch_registry_metadata_bytes(&release_url) {
            verify_optional_metadata_sig(
                &release_url,
                &rel_bytes,
                &format!("registry release descriptor '{}@{}'", name, ver),
            )?;
            let rel = parse_release_descriptor(&String::from_utf8_lossy(&rel_bytes));
            if !rel.name.is_empty() && rel.name != name {
                return Err(format!(
                    "registry dependency '{}': release.toml declares name '{}' instead of '{}'",
                    name, rel.name, name
                ));
            }
            if !rel.version.is_empty() && rel.version != ver {
                return Err(format!(
                    "registry dependency '{}': release.toml declares version '{}' instead of '{}'",
                    name, rel.version, ver
                ));
            }
            if !is_remote_url(&rel.tarball_url) {
                return Err(format!(
                    "registry dependency '{}': release.toml has invalid tarball_url '{}'",
                    name, rel.tarball_url
                ));
            }
            if base_url.to_ascii_lowercase().starts_with("https://")
                && rel.tarball_url.to_ascii_lowercase().starts_with("http://")
            {
                return Err(format!(
                    "registry dependency '{}': refusing insecure http:// tarball_url '{}' from HTTPS registry",
                    name, rel.tarball_url
                ));
            }
            let (tar_status, _, tar_bytes) =
                fetch_registry_bytes(&rel.tarball_url).map_err(|e| {
                    format!(
                        "registry dependency '{}': failed to fetch {}: {}",
                        name, rel.tarball_url, e
                    )
                })?;
            if tar_status != 200 {
                return Err(format!(
                    "registry dependency '{}': failed to download tarball from {} (HTTP {})",
                    name, rel.tarball_url, tar_status
                ));
            }
            if !rel.tarball_sha256.is_empty() {
                let actual_tar_sha = sha256_hex(&tar_bytes);
                if !actual_tar_sha.eq_ignore_ascii_case(&rel.tarball_sha256) {
                    return Err(format!(
                        "registry dependency '{}': tarball_sha256 mismatch for {}@{} (release.toml declares {}, downloaded {})",
                        name, name, ver, rel.tarball_sha256, actual_tar_sha
                    ));
                }
            }
            (tar_bytes, false)
        } else {
            let tar_url = format!("{}/{}/{}/package.tar", base_url, name, ver);
            let (mut tar_status, _, mut tar_bytes) =
                fetch_registry_bytes(&tar_url).map_err(|e| {
                    format!(
                        "registry dependency '{}': failed to fetch {}: {}",
                        name, tar_url, e
                    )
                })?;
            if tar_status == 404 {
                let alt_url = format!("{}/{}/{}-{}.tar", base_url, name, name, ver);
                let (st, _, bytes) = fetch_registry_bytes(&alt_url).map_err(|e| {
                    format!(
                        "registry dependency '{}': failed to fetch {}: {}",
                        name, alt_url, e
                    )
                })?;
                tar_status = st;
                tar_bytes = bytes;
            }
            if tar_status != 200 {
                return Err(format!(
                    "registry dependency '{}': failed to download tarball for {} (HTTP {})",
                    name, ver, tar_status
                ));
            }
            (tar_bytes, false)
        }
    };

    let stage_dir = pkg_cache_dir.join(format!(".stage_{}_{}", ver, std::process::id()));
    if stage_dir.exists() {
        let _ = std::fs::remove_dir_all(&stage_dir);
    }
    std::fs::create_dir_all(&stage_dir)
        .map_err(|e| format!("cannot create {}: {}", stage_dir.display(), e))?;

    let res = (|| -> Result<(), String> {
        extract_tar_checked(&tar_bytes, &stage_dir)?;
        let actual = tree_hash(&stage_dir)?;
        if actual != sha_text {
            return Err(format!(
                "registry dependency '{}': checksum mismatch for {}@{} \
                 (sha256.txt declares {}, archive tree hashes to {})",
                name, name, ver, sha_text, actual
            ));
        }

        // Verify spider.toml declares the exact requested package name and version.
        let extracted_manifest = Manifest::load(&stage_dir)?.ok_or_else(|| {
            format!(
                "registry dependency '{}': tarball for {}@{} has no spider.toml",
                name, name, ver
            )
        })?;
        if extracted_manifest.name() != name {
            return Err(format!(
                "registry dependency '{}': tarball spider.toml declares name '{}' instead of '{}'",
                name,
                extracted_manifest.name(),
                name
            ));
        }
        let want_v = Version::parse(ver)?;
        let got_v = Version::parse(extracted_manifest.version())?;
        if got_v != want_v {
            return Err(format!(
                "registry dependency '{}': tarball spider.toml declares version '{}' instead of '{}'",
                name,
                extracted_manifest.version(),
                ver
            ));
        }

        if let Some(msha_bytes) = &msha_bytes_opt {
            let declared_msha = String::from_utf8_lossy(msha_bytes).trim().to_string();
            let raw_manifest = std::fs::read(stage_dir.join("spider.toml"))
                .map_err(|e| format!("cannot read staged spider.toml: {}", e))?;
            let computed = manifest_and_tree_digest(&raw_manifest, &sha_text);
            if computed != declared_msha {
                return Err(format!(
                    "registry dependency '{}': manifest.sha256 mismatch for {}@{} (declares {}, computed {})",
                    name, name, ver, declared_msha, computed
                ));
            }
            std::fs::write(stage_dir.join("manifest.sha256"), msha_bytes)
                .map_err(|e| format!("cannot write cached manifest.sha256: {}", e))?;
        }
        if let Some(msig_bytes) = &msig_bytes_opt {
            std::fs::write(stage_dir.join("manifest.sha256.sig"), msig_bytes)
                .map_err(|e| format!("cannot write cached manifest.sha256.sig: {}", e))?;
        }

        if is_wheel {
            if let Some(wb) = &wheel_bundle {
                let native_dir = stage_dir.join("native").join(triple);
                if !native_dir.is_dir() {
                    return Err(format!(
                        "registry wheel '{}@{}': archive missing native/{}/",
                        name, triple, triple
                    ));
                }
                let actual_native = native_tree_hash(&native_dir)?;
                if actual_native != wb.wsha_text {
                    return Err(format!(
                        "registry wheel '{}@{}': native checksum mismatch (declares {}, hashes to {})",
                        name, triple, wb.wsha_text, actual_native
                    ));
                }
                std::fs::write(
                    stage_dir.join(format!("sha256-{}.txt", triple)),
                    &wb.wsha_bytes,
                )
                .map_err(|e| format!("cannot write cached wheel sha256: {}", e))?;
                if let Some(wsig) = &wb.wsig_opt {
                    std::fs::write(stage_dir.join(format!("sha256-{}.txt.sig", triple)), wsig)
                        .map_err(|e| format!("cannot write cached wheel sig: {}", e))?;
                }
                if let Some((wmsha_bytes, wmsig_opt)) = &wb.wmsha_pair {
                    let declared_wmsha = String::from_utf8_lossy(wmsha_bytes).trim().to_string();
                    let raw_manifest = std::fs::read(stage_dir.join("spider.toml"))
                        .map_err(|e| format!("cannot read staged spider.toml: {}", e))?;
                    let computed =
                        manifest_and_wheel_digest(&raw_manifest, &sha_text, triple, &wb.wsha_text);
                    if computed != declared_wmsha {
                        return Err(format!(
                            "registry wheel '{}@{}': manifest-{}.sha256 mismatch (declares {}, computed {})",
                            name, triple, triple, declared_wmsha, computed
                        ));
                    }
                    std::fs::write(
                        stage_dir.join(format!("manifest-{}.sha256", triple)),
                        wmsha_bytes,
                    )
                    .map_err(|e| format!("cannot write cached manifest-wheel digest: {}", e))?;
                    if let Some(wmsig) = wmsig_opt {
                        std::fs::write(
                            stage_dir.join(format!("manifest-{}.sha256.sig", triple)),
                            wmsig,
                        )
                        .map_err(|e| format!("cannot write cached manifest-wheel sig: {}", e))?;
                    }
                }
            }
        }

        std::fs::write(stage_dir.join("sha256.txt"), &sha_bytes)
            .map_err(|e| format!("cannot write cached sha256.txt: {}", e))?;
        if let Some(sig_bytes) = &sig_bytes_opt {
            std::fs::write(stage_dir.join("sha256.txt.sig"), sig_bytes)
                .map_err(|e| format!("cannot write cached sha256.txt.sig: {}", e))?;
        }
        if ver_dir.exists() {
            std::fs::remove_dir_all(ver_dir)
                .map_err(|e| format!("cannot clear {}: {}", ver_dir.display(), e))?;
        }
        if let Err(e) = std::fs::rename(&stage_dir, ver_dir) {
            return Err(format!(
                "cannot move {} to {}: {}",
                stage_dir.display(),
                ver_dir.display(),
                e
            ));
        }
        Ok(())
    })();

    if res.is_err() {
        let _ = std::fs::remove_dir_all(&stage_dir);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn parse_descriptor_and_redirect_downgrade_guard() {
        let desc = parse_registry_descriptor(
            "name = \"Mirror Reg\"\nurl = \"https://old.example\"\nmoved_to = \"https://new.example\"\nmirrors = [\"https://m1.example\", \"https://m2.example\"]\npublic_key = \"abcd\"\n",
        );
        assert_eq!(desc.name, "Mirror Reg");
        assert_eq!(desc.moved_to.as_deref(), Some("https://new.example"));
        assert_eq!(
            desc.mirrors,
            vec!["https://m1.example", "https://m2.example"]
        );
        assert_eq!(desc.public_key.as_deref(), Some("abcd"));

        let err = resolve_redirect_url("https://reg.example/a/b", "http://evil.example/c");
        // resolve_redirect_url normalizes, while fetch_registry_bytes enforces https -> http block
        assert!(err.is_ok());

        // Localhost HTTP `moved_to` migration via registry.toml.
        let listener_dest = TcpListener::bind("127.0.0.1:0").unwrap();
        let dest_port = listener_dest.local_addr().unwrap().port();
        let dest_handle = std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener_dest.accept() {
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf);
                let body = b"name = \"New Host\"\nmoved_to = \"\"\n";
                let hdr = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(hdr.as_bytes());
                let _ = s.write_all(body);
            }
        });

        let listener_old = TcpListener::bind("127.0.0.1:0").unwrap();
        let old_port = listener_old.local_addr().unwrap().port();
        let old_handle = std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener_old.accept() {
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf);
                let body = format!("moved_to = \"http://127.0.0.1:{}\"\n", dest_port);
                let hdr = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(hdr.as_bytes());
                let _ = s.write_all(body.as_bytes());
            }
        });

        let (resolved, _) =
            resolve_migrated_base(&format!("http://127.0.0.1:{}", old_port)).unwrap();
        assert_eq!(resolved, format!("http://127.0.0.1:{}", dest_port));
        let _ = old_handle.join();
        let _ = dest_handle.join();

        assert_eq!(
            url_origin("https://Pkg.Example.com:443/a/b").as_deref(),
            Some("https://pkg.example.com")
        );
        assert_eq!(
            url_origin("http://Pkg.Example.com:80/a/b").as_deref(),
            Some("http://pkg.example.com")
        );
        assert_eq!(
            url_origin("https://Pkg.Example.com:8443/a/b").as_deref(),
            Some("https://pkg.example.com:8443")
        );
        let advs = parse_advisories("# comment\nmathlib\t<1.2.0\thigh\tSPK-2026-001\tBuffer bug\n");
        assert_eq!(advs.len(), 1);
        assert_eq!(advs[0].package, "mathlib");
        assert_eq!(advs[0].severity, "high");
        assert_eq!(advs[0].id, "SPK-2026-001");

        let rel = parse_release_descriptor(
            "name = \"demo\"\nversion = \"0.1.0\"\ntarball_url = \"https://github.com/spid3r-spkg/spid3r-spkg.github.io/releases/download/demo-0.1.0/package.tar\"\ntarball_sha256 = \"deadbeef\"\nsource_repo = \"https://github.com/needyamin/demo\"\nsource_tag = \"v0.1.0\"\n",
        );
        assert_eq!(rel.name, "demo");
        assert_eq!(rel.version, "0.1.0");
        assert_eq!(
            rel.tarball_url,
            "https://github.com/spid3r-spkg/spid3r-spkg.github.io/releases/download/demo-0.1.0/package.tar"
        );
        assert_eq!(rel.tarball_sha256, "deadbeef");
        assert_eq!(rel.source_repo, "https://github.com/needyamin/demo");
        assert_eq!(rel.source_tag, "v0.1.0");
    }
}
