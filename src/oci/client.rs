//! OCI Registry v2 HTTP(S) client (`R11.1`).
//!
//! Supports manifest/index fetch (resolving multi-arch manifest lists by host
//! platform), config + layer blob downloads, SHA-256 verification, and anonymous
//! `WWW-Authenticate: Bearer realm="...",service="...",scope="..."` token exchange.

use std::path::{Path, PathBuf};

use super::layers::{cache_and_unpack_layer, store_blob_verified};
use super::manifest::{
    host_oci_arch, host_oci_os, parse_image_config, parse_manifest_or_index, verify_digest_bytes,
    verify_manifest_signature, ManifestOrIndex, OciImageConfig, OciManifest, SignaturePolicy,
};
use crate::stdlib::json_parse_text;
use crate::value::Value;

/// Parsed OCI image reference (`[registry/]repository[:tag|@digest]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageRef {
    pub registry: String,
    pub repository: String,
    pub reference: String,
    pub scheme: String,
}

#[derive(Clone, Debug)]
pub struct PulledImage {
    pub reference: String,
    pub manifest_digest: String,
    pub manifest: OciManifest,
    pub config: OciImageConfig,
    pub layer_dirs: Vec<PathBuf>,
}

impl ImageRef {
    /// Parse `"http://127.0.0.1:5000/repo/app:v1"` or `"registry/repo:tag"` or `"repo:tag"`.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let s = spec.trim();
        if s.is_empty() {
            return Err("oci: empty image reference".to_string());
        }
        let (scheme, rest) = if let Some(r) = s.strip_prefix("http://") {
            ("http".to_string(), r)
        } else if let Some(r) = s.strip_prefix("https://") {
            ("https".to_string(), r)
        } else {
            ("https".to_string(), s)
        };
        let (registry, path_part) = match rest.find('/') {
            Some(i) => {
                let first = &rest[..i];
                if first.contains('.') || first.contains(':') || first == "localhost" {
                    (first.to_string(), &rest[i + 1..])
                } else {
                    ("registry-1.docker.io".to_string(), rest)
                }
            }
            None => {
                return Err(format!(
                    "oci: image reference '{}' must include a repository path",
                    spec
                ))
            }
        };
        let (repository, reference) = if let Some(at) = path_part.find('@') {
            (path_part[..at].to_string(), path_part[at + 1..].to_string())
        } else if let Some(colon) = path_part.rfind(':') {
            (
                path_part[..colon].to_string(),
                path_part[colon + 1..].to_string(),
            )
        } else {
            (path_part.to_string(), "latest".to_string())
        };
        if repository.is_empty() || reference.is_empty() {
            return Err(format!("oci: malformed image reference '{}'", spec));
        }
        Ok(ImageRef {
            registry,
            repository,
            reference,
            scheme,
        })
    }

    pub fn base_url(&self) -> String {
        format!("{}://{}", self.scheme, self.registry)
    }
}

fn fetch_with_bearer(
    url: &str,
    accept: Option<&str>,
    cached_token: &mut Option<String>,
) -> Result<crate::stdlib::net::HttpBytesResponse, String> {
    let mut headers = Vec::new();
    if let Some(a) = accept {
        headers.push(("Accept".to_string(), a.to_string()));
    }
    if let Some(tok) = cached_token.as_ref() {
        headers.push(("Authorization".to_string(), format!("Bearer {}", tok)));
    }
    let (status, resp_headers, body) = crate::stdlib::net::fetch_bytes("GET", url, &headers, &[])?;
    if status == 401 {
        if let Some(www) = resp_headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("www-authenticate"))
            .map(|(_, v)| v.as_str())
        {
            if let Some(token) = exchange_anonymous_bearer(www)? {
                *cached_token = Some(token.clone());
                let mut retry_headers = Vec::new();
                if let Some(a) = accept {
                    retry_headers.push(("Accept".to_string(), a.to_string()));
                }
                retry_headers.push(("Authorization".to_string(), format!("Bearer {}", token)));
                return crate::stdlib::net::fetch_bytes("GET", url, &retry_headers, &[]);
            }
        }
    }
    Ok((status, resp_headers, body))
}

fn exchange_anonymous_bearer(www_auth: &str) -> Result<Option<String>, String> {
    let trimmed = www_auth.trim();
    let Some(rest) = trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))
    else {
        return Ok(None);
    };
    let mut realm = None;
    let mut service = None;
    let mut scope = None;
    for part in rest.split(',') {
        let p = part.trim();
        if let Some((k, v)) = p.split_once('=') {
            let val = v.trim().trim_matches('"').to_string();
            match k.trim() {
                "realm" => realm = Some(val),
                "service" => service = Some(val),
                "scope" => scope = Some(val),
                _ => {}
            }
        }
    }
    let Some(realm_url) = realm else {
        return Ok(None);
    };
    let mut q = Vec::new();
    if let Some(s) = service {
        q.push(format!("service={s}"));
    }
    if let Some(sc) = scope {
        q.push(format!("scope={sc}"));
    }
    let token_url = if q.is_empty() {
        realm_url
    } else if realm_url.contains('?') {
        format!("{realm_url}&{}", q.join("&"))
    } else {
        format!("{realm_url}?{}", q.join("&"))
    };
    let (status, _, body) = crate::stdlib::net::fetch_bytes("GET", &token_url, &[], &[])?;
    if status != 200 {
        return Err(format!("oci: token endpoint returned HTTP {}", status));
    }
    let txt = std::str::from_utf8(&body)
        .map_err(|_| "oci: token endpoint returned non-UTF-8".to_string())?;
    let v = json_parse_text(txt)?;
    for key in ["token", "access_token"] {
        if let Some(Value::Str(s)) = super::manifest::map_get_str(&v, key) {
            return Ok(Some(s.to_string()));
        }
    }
    Ok(None)
}

/// Pull an OCI image from a Registry v2 endpoint into `store_dir`, verifying
/// every manifest/config/layer SHA-256 digest before extracting layers.
pub fn pull_image(
    spec: &str,
    store_dir: &Path,
    policy: SignaturePolicy,
    pubkey_hex: Option<&str>,
) -> Result<PulledImage, String> {
    let img = ImageRef::parse(spec)?;
    let mut token = None;
    let accept = "application/vnd.oci.image.manifest.v1+json, application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.docker.distribution.manifest.list.v2+json";
    let manifest_url = format!(
        "{}/v2/{}/manifests/{}",
        img.base_url(),
        img.repository,
        img.reference
    );
    let (status, resp_headers, mut manifest_bytes) =
        fetch_with_bearer(&manifest_url, Some(accept), &mut token)?;
    if status != 200 {
        return Err(format!(
            "oci: failed to fetch manifest '{}' (HTTP {})",
            manifest_url, status
        ));
    }
    if img.reference.starts_with("sha256:") {
        verify_digest_bytes(&img.reference, &manifest_bytes)?;
    }

    let mut sig_hex = resp_headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("x-spid3r-signature"))
        .map(|(_, v)| v.clone());

    let mut manifest = match parse_manifest_or_index(&manifest_bytes)? {
        ManifestOrIndex::Manifest(m) => m,
        ManifestOrIndex::Index(idx) => {
            let desc = idx.select_for(host_oci_os(), host_oci_arch())?;
            let sub_url = format!(
                "{}/v2/{}/manifests/{}",
                img.base_url(),
                img.repository,
                desc.digest
            );
            let (st, sh, sub_bytes) = fetch_with_bearer(&sub_url, Some(accept), &mut token)?;
            if st != 200 {
                return Err(format!(
                    "oci: failed to fetch platform manifest '{}' (HTTP {})",
                    sub_url, st
                ));
            }
            verify_digest_bytes(&desc.digest, &sub_bytes)?;
            manifest_bytes = sub_bytes;
            if let Some(s) = sh
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("x-spid3r-signature"))
                .map(|(_, v)| v.clone())
            {
                sig_hex = Some(s);
            }
            match parse_manifest_or_index(&manifest_bytes)? {
                ManifestOrIndex::Manifest(m) => m,
                ManifestOrIndex::Index(_) => {
                    return Err("error[corrupt]: nested OCI manifest index is not supported".into())
                }
            }
        }
    };

    verify_manifest_signature(&manifest_bytes, sig_hex.as_deref(), pubkey_hex, policy)?;

    let manifest_digest = format!("sha256:{}", crate::pkg::sha256_hex(&manifest_bytes));
    let _ = &mut manifest;

    // Fetch & verify config blob
    let cfg_url = format!(
        "{}/v2/{}/blobs/{}",
        img.base_url(),
        img.repository,
        manifest.config.digest
    );
    let (cst, _, cfg_bytes) = fetch_with_bearer(&cfg_url, None, &mut token)?;
    if cst != 200 {
        return Err(format!(
            "oci: failed to fetch config blob '{}' (HTTP {})",
            cfg_url, cst
        ));
    }
    verify_digest_bytes(&manifest.config.digest, &cfg_bytes)?;
    store_blob_verified(store_dir, &manifest.config.digest, &cfg_bytes)?;
    let config = parse_image_config(&cfg_bytes)?;

    // Fetch, verify, cache & unpack each layer
    let mut layer_dirs = Vec::with_capacity(manifest.layers.len());
    for layer in &manifest.layers {
        let blob_url = format!(
            "{}/v2/{}/blobs/{}",
            img.base_url(),
            img.repository,
            layer.digest
        );
        let (lst, _, blob_bytes) = fetch_with_bearer(&blob_url, None, &mut token)?;
        if lst != 200 {
            return Err(format!(
                "oci: failed to fetch layer blob '{}' (HTTP {})",
                blob_url, lst
            ));
        }
        let dir = cache_and_unpack_layer(store_dir, &layer.digest, &blob_bytes)?;
        layer_dirs.push(dir);
    }

    // Persist image metadata under store_dir/images/<safe_ref>.json
    save_pulled_metadata(store_dir, spec, &manifest_digest, &manifest, &layer_dirs)?;

    Ok(PulledImage {
        reference: spec.to_string(),
        manifest_digest,
        manifest,
        config,
        layer_dirs,
    })
}

fn save_pulled_metadata(
    store_dir: &Path,
    spec: &str,
    manifest_digest: &str,
    manifest: &OciManifest,
    layer_dirs: &[PathBuf],
) -> Result<(), String> {
    let images_dir = store_dir.join("images");
    std::fs::create_dir_all(&images_dir)
        .map_err(|e| format!("oci: mkdir {}: {e}", images_dir.display()))?;
    let id = &crate::pkg::sha256_hex(spec.as_bytes())[..16];
    let out = images_dir.join(format!("{id}.meta"));
    let mut text = format!(
        "reference={spec}\ndigest={manifest_digest}\nconfig={}\n",
        manifest.config.digest
    );
    for (l, d) in manifest.layers.iter().zip(layer_dirs.iter()) {
        text.push_str(&format!("layer={}|{}\n", l.digest, d.display()));
    }
    let tmp = images_dir.join(format!("{id}.meta.tmp.{}", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| format!("oci: write meta: {e}"))?;
    std::fs::rename(&tmp, &out).map_err(|e| format!("oci: rename meta: {e}"))?;
    Ok(())
}
