//! Enterprise supply-chain auditing (`spkg audit`) and SBOM / freeze export
//! (`spkg freeze [--sbom]`). Zero external crate dependencies.

use std::collections::BTreeSet;
use std::path::Path;

use super::lockfile::{read_lock, LockEntry};
use super::manifest::{parse_manifest, Manifest};
use super::ops::verify;
use super::registry::{
    fetch_remote_advisories, fetch_remote_yanked, is_remote_url, registry_endpoints,
};
use super::semver::{Constraint, Version};
use super::vendor::configured_trust_pks;

/// One finding emitted by [`audit_project`].
#[derive(Debug, Clone)]
pub struct AuditFinding {
    /// `"critical"`, `"high"`, `"medium"`, or `"info"`.
    pub severity: &'static str,
    pub package: String,
    pub kind: &'static str,
    pub detail: String,
}

#[derive(Debug, Default)]
pub struct AuditReport {
    pub checked: usize,
    pub findings: Vec<AuditFinding>,
}

impl AuditReport {
    pub fn has_failures(&self) -> bool {
        self.findings
            .iter()
            .any(|f| matches!(f.severity, "critical" | "high" | "medium"))
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

pub fn render_audit_json(report: &AuditReport) -> String {
    let mut out = String::from("{\n  \"checked\": ");
    out.push_str(&report.checked.to_string());
    out.push_str(",\n  \"ok\": ");
    out.push_str(if report.has_failures() {
        "false"
    } else {
        "true"
    });
    out.push_str(",\n  \"findings\": [");
    for (i, f) in report.findings.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("\n    {");
        out.push_str(&format!(
            "\"severity\": \"{}\", \"package\": \"{}\", \"kind\": \"{}\", \"detail\": \"{}\"",
            json_escape(f.severity),
            json_escape(&f.package),
            json_escape(f.kind),
            json_escape(&f.detail),
        ));
        out.push('}');
    }
    if !report.findings.is_empty() {
        out.push('\n');
        out.push_str("  ");
    }
    out.push_str("]\n}\n");
    out
}

/// Audit `vendor/`, `spider.lock`, signature trust settings, unpinned/insecure
/// Git sources, T5 `[ffi]` native surfaces, yanked versions, and advisories.
pub fn audit_project(root: &Path, offline: bool) -> Result<AuditReport, String> {
    let mut report = AuditReport::default();

    // 1. Cryptographic tree & native binary verification against spider.lock.
    let vreport = verify(root)?;
    report.checked = vreport.checked;
    for issue in vreport.issues {
        let sev = match issue.kind {
            "hash" | "native-hash" | "unverified" => "critical",
            "missing" | "native-missing" | "files" | "unlocked" => "high",
            _ => "high",
        };
        report.findings.push(AuditFinding {
            severity: sev,
            package: issue.name,
            kind: issue.kind,
            detail: issue.detail,
        });
    }

    let entries = read_lock(root)?;
    let pks = configured_trust_pks()?;
    let mut registry_checked: BTreeSet<String> = BTreeSet::new();
    let endpoints = registry_endpoints();
    let remote_eps: Vec<String> = endpoints
        .iter()
        .filter(|e| is_remote_url(e))
        .cloned()
        .collect();

    for entry in &entries {
        if entry.source.starts_with("registry+") {
            if entry.manifest_sha256.is_none() {
                report.findings.push(AuditFinding {
                    severity: "medium",
                    package: entry.name.clone(),
                    kind: "no-manifest-hash",
                    detail: "registry package lacks manifest.sha256 binding spider.toml and src/"
                        .to_string(),
                });
            }
            if pks.is_empty() && registry_checked.insert(entry.name.clone()) {
                report.findings.push(AuditFinding {
                    severity: "info",
                    package: entry.name.clone(),
                    kind: "trust-pk-unset",
                    detail: "packages.trust_pk is unset; Ed25519 signature enforcement is off"
                        .to_string(),
                });
            }
        } else if entry.source.starts_with("git+") {
            if entry.source.starts_with("git+http://") || entry.source.starts_with("git+git://") {
                report.findings.push(AuditFinding {
                    severity: "high",
                    package: entry.name.clone(),
                    kind: "insecure-git",
                    detail: format!(
                        "plaintext git dependency '{}' lacks transport integrity",
                        entry.source
                    ),
                });
            }
            let has_commit = entry
                .source
                .rsplit_once('#')
                .is_some_and(|(_, c)| c.len() == 40 && c.chars().all(|ch| ch.is_ascii_hexdigit()));
            if !has_commit {
                report.findings.push(AuditFinding {
                    severity: "high",
                    package: entry.name.clone(),
                    kind: "unpinned-git",
                    detail: format!(
                        "git dependency '{}' is not pinned to a 40-hex commit",
                        entry.source
                    ),
                });
            }
        }

        if !entry.ffi_libs.is_empty() || entry.native_sha256.is_some() {
            report.findings.push(AuditFinding {
                severity: "info",
                package: entry.name.clone(),
                kind: "ffi-surface",
                detail: format!(
                    "declares native FFI surface [{}]{}",
                    entry.ffi_libs.join(", "),
                    entry
                        .target
                        .as_deref()
                        .map(|t| format!(" (wheel {})", t))
                        .unwrap_or_default()
                ),
            });
        }
    }

    if !offline && !remote_eps.is_empty() {
        for entry in &entries {
            if !entry.source.starts_with("registry+") {
                continue;
            }
            let Ok(locked_v) = Version::parse(&entry.version) else {
                continue;
            };
            for ep in &remote_eps {
                if let Ok(yanked) = fetch_remote_yanked(ep, None, &entry.name) {
                    if yanked.contains(&locked_v) {
                        report.findings.push(AuditFinding {
                            severity: "medium",
                            package: entry.name.clone(),
                            kind: "yanked",
                            detail: format!(
                                "{}@{} has been yanked upstream",
                                entry.name, entry.version
                            ),
                        });
                        break;
                    }
                }
            }
        }

        if let Ok(advisories) = fetch_remote_advisories(&remote_eps) {
            for adv in advisories {
                let Ok(constraint) = Constraint::parse(&adv.constraint) else {
                    continue;
                };
                for entry in &entries {
                    if entry.name != adv.package {
                        continue;
                    }
                    if let Ok(v) = Version::parse(&entry.version) {
                        if constraint.matches(&v) {
                            let sev = match adv.severity.as_str() {
                                "critical" => "critical",
                                "high" => "high",
                                _ => "medium",
                            };
                            report.findings.push(AuditFinding {
                                severity: sev,
                                package: entry.name.clone(),
                                kind: "advisory",
                                detail: format!(
                                    "{} ({}): {} [{}]",
                                    adv.id, adv.constraint, adv.summary, entry.version
                                ),
                            });
                        }
                    }
                }
            }
        }
    }

    Ok(report)
}

fn entry_license(root: &Path, entry: &LockEntry) -> Option<String> {
    let path = root
        .join("vendor")
        .join(&entry.name)
        .join(".spid3r-manifest.toml");
    let text = std::fs::read_to_string(path).ok()?;
    parse_manifest(&text).ok()?.package.and_then(|p| p.license)
}

/// Export pinned dependencies as `pip freeze` lines or CycloneDX 1.5 JSON SBOM.
pub fn freeze_project(root: &Path, sbom_json: bool) -> Result<String, String> {
    let manifest =
        Manifest::load(root)?.ok_or_else(|| format!("no spider.toml in {}", root.display()))?;
    let mut entries = read_lock(root)?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    if !sbom_json {
        let mut out = String::new();
        for e in &entries {
            out.push_str(&format!("{}=={}\n", e.name, e.version));
        }
        return Ok(out);
    }

    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"bomFormat\": \"CycloneDX\",\n");
    out.push_str("  \"specVersion\": \"1.5\",\n");
    out.push_str("  \"version\": 1,\n");
    out.push_str("  \"metadata\": {\n");
    out.push_str("    \"component\": {\n");
    out.push_str("      \"type\": \"application\",\n");
    out.push_str(&format!(
        "      \"name\": \"{}\",\n",
        json_escape(manifest.name())
    ));
    out.push_str(&format!(
        "      \"version\": \"{}\"\n",
        json_escape(manifest.version())
    ));
    out.push_str("    }\n");
    out.push_str("  },\n");
    out.push_str("  \"components\": [");
    for (idx, e) in entries.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str("\n    {\n");
        out.push_str("      \"type\": \"library\",\n");
        out.push_str(&format!("      \"name\": \"{}\",\n", json_escape(&e.name)));
        out.push_str(&format!(
            "      \"version\": \"{}\",\n",
            json_escape(&e.version)
        ));
        out.push_str(&format!(
            "      \"purl\": \"pkg:spid3r/{}@{}\"",
            json_escape(&e.name),
            json_escape(&e.version)
        ));
        if let Some(lic) = entry_license(root, e) {
            out.push_str(",\n      \"licenses\": [{\"license\": {\"id\": \"");
            out.push_str(&json_escape(&lic));
            out.push_str("\"}}]");
        }
        let mut hashes = Vec::new();
        if let Some(ref s) = e.sha256 {
            hashes.push(("spid3r:src-sha256", s.as_str()));
        }
        if let Some(ref m) = e.manifest_sha256 {
            hashes.push(("spid3r:manifest-sha256", m.as_str()));
        }
        if let Some(ref n) = e.native_sha256 {
            hashes.push(("spid3r:native-sha256", n.as_str()));
        }
        if !hashes.is_empty() {
            out.push_str(",\n      \"hashes\": [");
            for (hi, (label, digest)) in hashes.iter().enumerate() {
                if hi > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format!(
                    "{{\"alg\": \"SHA-256\", \"content\": \"{}\", \"note\": \"{}\"}}",
                    json_escape(digest),
                    json_escape(label)
                ));
            }
            out.push(']');
        }
        let mut props: Vec<(String, String)> = vec![("spid3r:source".into(), e.source.clone())];
        if let Some(ref tgt) = e.target {
            props.push(("spid3r:target".into(), tgt.clone()));
        }
        if !e.ffi_libs.is_empty() {
            props.push(("spid3r:ffi-libs".into(), e.ffi_libs.join(",")));
        }
        out.push_str(",\n      \"properties\": [");
        for (pi, (k, v)) in props.iter().enumerate() {
            if pi > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!(
                "{{\"name\": \"{}\", \"value\": \"{}\"}}",
                json_escape(k),
                json_escape(v)
            ));
        }
        out.push_str("]\n    }");
    }
    if !entries.is_empty() {
        out.push_str("\n  ");
    }
    out.push_str("]\n}\n");
    Ok(out)
}
