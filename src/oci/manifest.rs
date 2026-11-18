//! OCI Image Spec v1 manifests, manifest lists (multi-arch), image config,
//! digest validation, and Ed25519 manifest signature policy (`R11.1`, `R11.8`).

use crate::pkg::sha256_hex;
use crate::stdlib::json_parse_text;
use crate::value::collections::MapKey;
use crate::value::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OciPlatform {
    pub os: String,
    pub architecture: String,
    pub variant: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OciDescriptor {
    pub media_type: String,
    pub digest: String,
    pub size: u64,
    pub platform: Option<OciPlatform>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OciManifest {
    pub schema_version: i64,
    pub media_type: String,
    pub config: OciDescriptor,
    pub layers: Vec<OciDescriptor>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OciIndex {
    pub schema_version: i64,
    pub media_type: String,
    pub manifests: Vec<OciDescriptor>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestOrIndex {
    Manifest(OciManifest),
    Index(OciIndex),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OciImageConfig {
    pub os: String,
    pub architecture: String,
    pub env: Vec<String>,
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
    pub working_dir: String,
    pub user: String,
    pub diff_ids: Vec<String>,
}

/// Image signature verification policy (`R11.8`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignaturePolicy {
    AllowUnsigned,
    RequireSigned,
}

/// Map Rust's `std::env::consts::ARCH` to OCI architecture names.
pub fn host_oci_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "386",
        "arm" => "arm",
        "riscv64" => "riscv64",
        other => other,
    }
}

/// Map Rust's `std::env::consts::OS` to OCI OS names.
pub fn host_oci_os() -> &'static str {
    match std::env::consts::OS {
        "linux" => "linux",
        "windows" => "windows",
        "macos" => "darwin",
        other => other,
    }
}

/// Validate an OCI `sha256:<64 hex>` digest and return its lowercase 64-char hex part.
pub fn validate_digest(digest: &str) -> Result<String, String> {
    let hex = digest.strip_prefix("sha256:").ok_or_else(|| {
        format!(
            "error[corrupt]: unsupported or malformed OCI digest '{}'",
            digest
        )
    })?;
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "error[corrupt]: invalid sha256 digest length/hex '{}'",
            digest
        ));
    }
    Ok(hex.to_ascii_lowercase())
}

/// Verify raw bytes against an expected `sha256:<hex>` digest (fail closed).
pub fn verify_digest_bytes(expected: &str, bytes: &[u8]) -> Result<String, String> {
    let expected_hex = validate_digest(expected)?;
    let actual_hex = sha256_hex(bytes);
    if actual_hex != expected_hex {
        return Err(format!(
            "error[corrupt]: sha256 digest mismatch (expected {}, got sha256:{})",
            expected, actual_hex
        ));
    }
    Ok(actual_hex)
}

pub fn map_get_str(v: &Value, key: &str) -> Option<Value> {
    let Value::Map(m) = v else {
        return None;
    };
    let guard = m.lock().unwrap_or_else(|e| e.into_inner());
    let target = MapKey::Str(key.into());
    guard
        .entries
        .iter()
        .find(|(k, _)| k == &target)
        .map(|(_, val)| val.clone())
}

fn map_get(v: &Value, key: &str) -> Option<Value> {
    map_get_str(v, key)
}

fn get_str(v: &Value, key: &str) -> Option<String> {
    match map_get(v, key) {
        Some(Value::Str(s)) => Some(s.to_string()),
        _ => None,
    }
}

fn get_i64(v: &Value, key: &str) -> Option<i64> {
    match map_get(v, key) {
        Some(Value::Int(i)) => Some(i),
        _ => None,
    }
}

fn get_str_list(v: &Value, key: &str) -> Vec<String> {
    let Some(Value::List(l)) = map_get(v, key) else {
        return Vec::new();
    };
    let guard = l.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .items
        .iter()
        .filter_map(|it| match it {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        })
        .collect()
}

fn parse_descriptor(v: &Value) -> Result<OciDescriptor, String> {
    let media_type = get_str(v, "mediaType").unwrap_or_default();
    let digest = get_str(v, "digest")
        .ok_or_else(|| "error[corrupt]: OCI descriptor missing 'digest'".to_string())?;
    let _ = validate_digest(&digest)?;
    let size_i = get_i64(v, "size").unwrap_or(0);
    if size_i < 0 {
        return Err("error[corrupt]: OCI descriptor has negative size".to_string());
    }
    let platform = map_get(v, "platform").and_then(|p| {
        let os = get_str(&p, "os")?;
        let architecture = get_str(&p, "architecture")?;
        let variant = get_str(&p, "variant");
        Some(OciPlatform {
            os,
            architecture,
            variant,
        })
    });
    Ok(OciDescriptor {
        media_type,
        digest,
        size: size_i as u64,
        platform,
    })
}

/// Parse either an OCI Image Manifest or an OCI Image Index / Docker Manifest List.
pub fn parse_manifest_or_index(bytes: &[u8]) -> Result<ManifestOrIndex, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "error[corrupt]: OCI manifest is not valid UTF-8".to_string())?;
    let root = json_parse_text(text)
        .map_err(|e| format!("error[corrupt]: malformed OCI manifest JSON: {e}"))?;
    let schema_version = get_i64(&root, "schemaVersion").unwrap_or(2);
    let media_type = get_str(&root, "mediaType").unwrap_or_default();

    if let Some(Value::List(ms)) = map_get(&root, "manifests") {
        let items = ms.lock().unwrap_or_else(|e| e.into_inner()).items.clone();
        let mut manifests = Vec::with_capacity(items.len());
        for item in &items {
            manifests.push(parse_descriptor(item)?);
        }
        return Ok(ManifestOrIndex::Index(OciIndex {
            schema_version,
            media_type,
            manifests,
        }));
    }

    let cfg_val = map_get(&root, "config")
        .ok_or_else(|| "error[corrupt]: OCI manifest missing 'config'".to_string())?;
    let config = parse_descriptor(&cfg_val)?;

    let layers_val = map_get(&root, "layers")
        .ok_or_else(|| "error[corrupt]: OCI manifest missing 'layers'".to_string())?;
    let Value::List(ls) = layers_val else {
        return Err("error[corrupt]: OCI manifest 'layers' is not a list".to_string());
    };
    let items = ls.lock().unwrap_or_else(|e| e.into_inner()).items.clone();
    let mut layers = Vec::with_capacity(items.len());
    for item in &items {
        layers.push(parse_descriptor(item)?);
    }
    Ok(ManifestOrIndex::Manifest(OciManifest {
        schema_version,
        media_type,
        config,
        layers,
    }))
}

impl OciIndex {
    /// Select the manifest descriptor matching `target_os` and `target_arch`.
    pub fn select_for(&self, target_os: &str, target_arch: &str) -> Result<&OciDescriptor, String> {
        self.manifests
            .iter()
            .find(|d| {
                d.platform
                    .as_ref()
                    .map(|p| p.os == target_os && p.architecture == target_arch)
                    .unwrap_or(false)
            })
            .ok_or_else(|| {
                format!(
                    "oci: no manifest in index matches platform {}/{}",
                    target_os, target_arch
                )
            })
    }
}

/// Parse an OCI image config JSON blob.
pub fn parse_image_config(bytes: &[u8]) -> Result<OciImageConfig, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "error[corrupt]: OCI image config is not valid UTF-8".to_string())?;
    let root = json_parse_text(text)
        .map_err(|e| format!("error[corrupt]: malformed OCI config JSON: {e}"))?;
    let os = get_str(&root, "os").unwrap_or_else(|| "linux".to_string());
    let architecture = get_str(&root, "architecture").unwrap_or_else(|| "amd64".to_string());
    let cfg = map_get(&root, "config");
    let (env, entrypoint, cmd, working_dir, user) = match cfg {
        Some(ref c) => (
            get_str_list(c, "Env"),
            get_str_list(c, "Entrypoint"),
            get_str_list(c, "Cmd"),
            get_str(c, "WorkingDir").unwrap_or_else(|| "/".to_string()),
            get_str(c, "User").unwrap_or_default(),
        ),
        None => (
            Vec::new(),
            Vec::new(),
            Vec::new(),
            "/".to_string(),
            String::new(),
        ),
    };
    let diff_ids = map_get(&root, "rootfs")
        .map(|rf| get_str_list(&rf, "diff_ids"))
        .unwrap_or_default();
    Ok(OciImageConfig {
        os,
        architecture,
        env,
        entrypoint,
        cmd,
        working_dir,
        user,
        diff_ids,
    })
}

/// Verify an optional Ed25519 signature over raw manifest bytes (`R11.8`).
///
/// `sig_hex`: 128-char hex Ed25519 signature (`None` if unsigned).
/// `pubkey_hex`: 64-char hex Ed25519 public key.
pub fn verify_manifest_signature(
    manifest_bytes: &[u8],
    sig_hex: Option<&str>,
    pubkey_hex: Option<&str>,
    policy: SignaturePolicy,
) -> Result<(), String> {
    let Some(sig) = sig_hex else {
        return match policy {
            SignaturePolicy::AllowUnsigned => Ok(()),
            SignaturePolicy::RequireSigned => {
                Err("error[corrupt]: unsigned OCI image rejected by signature policy".to_string())
            }
        };
    };
    let pk_hex = pubkey_hex.ok_or_else(|| {
        "error[corrupt]: manifest signature present but no trusted public key configured"
            .to_string()
    })?;
    let pk = decode_hex_array::<32>(pk_hex, "public key")?;
    let sig_bytes = decode_hex_array::<64>(sig, "signature")?;
    if !crate::pkg::ed25519::verify(&pk, manifest_bytes, &sig_bytes) {
        return Err(
            "error[corrupt]: OCI manifest Ed25519 signature verification failed".to_string(),
        );
    }
    Ok(())
}

fn decode_hex_array<const N: usize>(hex: &str, label: &str) -> Result<[u8; N], String> {
    let hex = hex.trim();
    if hex.len() != N * 2 {
        return Err(format!(
            "error[corrupt]: expected {} hex chars for {label}, got {}",
            N * 2,
            hex.len()
        ));
    }
    let mut out = [0u8; N];
    for i in 0..N {
        out[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| format!("error[corrupt]: invalid hex in {label}"))?;
    }
    Ok(out)
}
