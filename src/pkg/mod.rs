//! `spkg` / `spid3r` project, manifest, lockfile, tar, crypto, and
//! host-portable registry package subsystem.
//!
//! Organized into clean single-responsibility submodules:
//! - [`sha256`], [`sha512`], [`ed25519`], [`hash`]: zero-dependency crypto + symlink-safe streaming tree hashes
//! - [`semver`], [`manifest`], [`lockfile`]: SemVer constraints, `spider.toml` v1/v2, escaped `spider.lock`
//! - [`tar`], [`git`], [`vendor`]: bounded ustar archives, hardened Git transport, atomic vendoring
//! - [`registry`], [`resolver`], [`ops`]: host-portable mirrors/migrations, 2-phase graph solver, CLI ops

use std::path::Path;

// `sha256.rs` stays at this exact path because `build.rs` #[path]-includes it.
#[path = "sha256.rs"]
pub mod sha256;
pub use sha256::{hex_decode, hex_encode, sha256_bytes, sha256_hex, Sha256Hasher};

mod sha512;
pub use sha512::{sha512_bytes, sha512_hex};

pub mod audit;
pub mod ed25519;
pub use ed25519::{keypair_from_seed, sign as ed25519_sign, verify as ed25519_verify};

pub mod ffi_link;
pub mod git;
pub mod hash;
pub mod lockfile;
pub mod manifest;
pub mod ops;
pub mod registry;
pub mod resolver;
pub mod semver;
pub mod tar;
pub mod tool;
pub mod vendor;

pub use git::validate_git_url;
pub use hash::{content_hash, manifest_and_tree_digest, tree_hash};
pub use manifest::{
    parse_manifest, toml_escape_string, valid_pkg_name, DepSource, Dependency, FfiConfig, Manifest,
    PackageInfo,
};
pub use resolver::{
    install, install_opts, install_with, latest_registry_version, InstallOptions, InstallReport,
};
pub use semver::{Constraint, Version};
pub use tar::{pack, pack_entries, pack_for_target, pack_workspace, read_tar, unpack, write_tar};

/// Create `spider.toml`, `src/main.matrix`, and `.gitignore` in `dir` (if missing).
pub fn init(dir: &Path, name: &str) -> Result<Vec<String>, String> {
    let mut created = Vec::new();
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {}", dir.display(), e))?;
    let manifest = dir.join("spider.toml");
    if !manifest.exists() {
        let text = format!(
            "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\n",
            toml_escape_string(name)
        );
        std::fs::write(&manifest, text)
            .map_err(|e| format!("cannot write {}: {}", manifest.display(), e))?;
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
        assert!(
            parse_manifest("[package]\nversion = \"1\"\n").is_err(),
            "name required"
        );
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
