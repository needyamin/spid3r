//! The **one** module resolver, shared by every backend.
//!
//! Before this file existed, four resolvers had drifted apart: the tree
//! interpreter's, the bytecode compiler's, the VM's run-time fallback, and the
//! AOT flattener's (plus the semantic pass's scan). They agreed on the happy
//! path and disagreed everywhere else — only some of them validated the
//! import spec with [`check_import_path`], only some canonicalized the result,
//! and one had lost the embedded-stdlib fallback entirely. That is precisely
//! the "compiles but computes differently" class of bug
//! `docs/ENGINE_CONTRACT.md` calls the worst possible failure.
//!
//! There is now exactly one search order, one validation rule, and one error
//! message. The order is documented and must not change (a project-local
//! `stdlib/` or `vendor/` copy overrides the built-in one):
//!
//! 1. the importing file's directory
//! 2. `SPID3R_PATH`
//! 3. `./stdlib`
//! 4. the `stdlib/` shipped beside (or above) the executable
//! 5. `vendor/` from the nearest enclosing package
//! 6. the embedded stdlib table (last resort — disk always wins)
//!
//! Each candidate is tried both as given and with a `.matrix` suffix.

use std::path::PathBuf;

use crate::diagnostics::{Diagnostic, Phase};

use super::{canonical_key, check_import_path, embedded_path};

/// Where a resolved module came from. Reported by `spidr modules` so the
/// precedence above is inspectable instead of guessable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleOrigin {
    /// A `.matrix` file found beside the importer or on the search path.
    Disk,
    /// A file under a `vendor/` directory populated by `spidr install`.
    /// Checksummed against `spider.lock`; `spidr verify` re-checks it.
    Vendor,
    /// The embedded stdlib table. The path is the synthetic
    /// `<stdlib>/name.matrix` marker, not a real file — read it through
    /// [`super::read_module_source`].
    Embedded,
    /// A module contributed by a plugin. No resolver produces this yet; it is
    /// part of the enum so `spidr modules` can report plugin-provided modules
    /// without a second, parallel enum when the plugin API lands.
    Plugin,
}

impl ModuleOrigin {
    /// The stable label used by `spidr modules` and by tests.
    pub fn as_str(self) -> &'static str {
        match self {
            ModuleOrigin::Disk => "disk",
            ModuleOrigin::Vendor => "vendor",
            ModuleOrigin::Embedded => "embedded",
            ModuleOrigin::Plugin => "plugin",
        }
    }
}

/// A module resolved to something a backend can read.
#[derive(Clone, Debug)]
pub struct ResolvedModule {
    /// For [`ModuleOrigin::Embedded`] this is a marker path, not a file.
    pub path: PathBuf,
    pub origin: ModuleOrigin,
}

impl ResolvedModule {
    /// Whether [`Self::path`] is the synthetic embedded marker.
    pub fn is_embedded(&self) -> bool {
        self.origin == ModuleOrigin::Embedded
    }
}

/// The default search roots, each tagged with the trust tier it implies.
///
/// `SPID3R_PATH` entries are split on `;` (and each part is additionally split
/// with the platform path separator) because the variable has always been
/// documented that way; a `;`-separated list is the portable spelling that
/// works identically on Windows and POSIX.
pub fn search_roots() -> Vec<(PathBuf, ModuleOrigin)> {
    let mut roots: Vec<(PathBuf, ModuleOrigin)> = Vec::new();
    if let Ok(path) = std::env::var("SPID3R_PATH") {
        for part in path.split(';') {
            for d in std::env::split_paths(part.trim()) {
                let s = d.to_string_lossy();
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    roots.push((PathBuf::from(trimmed), ModuleOrigin::Disk));
                }
            }
        }
    }
    roots.push((PathBuf::from("stdlib"), ModuleOrigin::Disk));
    // The stdlib shipped beside the executable (installer / release tarball
    // layout) or in an ancestor directory (development layout, e.g.
    // target/release/../../stdlib), so `import "http"` — and the implicit
    // "bigint" module behind `123n` — works from any working directory.
    if let Ok(exe) = std::env::current_exe() {
        let mut cur = exe.parent();
        while let Some(dir) = cur {
            let shipped = dir.join("stdlib");
            if shipped.is_dir() {
                roots.push((shipped, ModuleOrigin::Disk));
                break;
            }
            cur = dir.parent();
        }
    }
    // Project dependencies vendored by `spid3r install`. Inside a workspace
    // member the shared `vendor/` lives at the workspace root, so walk up.
    if let Some(vendor) = super::find_vendor_dir() {
        roots.push((vendor, ModuleOrigin::Vendor));
    }
    roots
}

/// Resolve an import spec to a module.
///
/// `importer` is the *file* containing the `import` (not a directory); its
/// parent is searched first. `extra_dirs` are host-supplied roots appended
/// after the defaults — the embedding API's `add_module_dir`, and the module's
/// own directory when a module imports further modules.
///
/// The spec is validated by [`check_import_path`] **before** any filesystem
/// access, so an absolute path, a drive/UNC prefix, or a `..` component is
/// rejected with a semantic diagnostic on every engine instead of silently
/// escaping the search roots on some of them.
pub fn resolve_module(
    importer: &str,
    spec: &str,
    extra_dirs: &[PathBuf],
) -> Result<ResolvedModule, Diagnostic> {
    check_import_path(spec)?;

    let mut roots: Vec<(PathBuf, ModuleOrigin)> = Vec::with_capacity(extra_dirs.len() + 8);
    let base = PathBuf::from(importer);
    if let Some(dir) = base.parent() {
        roots.push((dir.to_path_buf(), ModuleOrigin::Disk));
    }
    roots.extend(search_roots());
    roots.extend(extra_dirs.iter().cloned().map(|d| (d, ModuleOrigin::Disk)));

    for (dir, origin) in roots {
        // As written first, then with the `.matrix` suffix: `import "util"`
        // and `import "util.matrix"` both work, and an extensionless file
        // named `util` wins over `util.matrix` beside it.
        for candidate in [dir.join(spec), dir.join(format!("{}.matrix", spec))] {
            if candidate.is_file() {
                return Ok(ResolvedModule {
                    // Canonicalize so the module cache, cycle detection and the
                    // VM's embedded table key on the real file rather than the
                    // import spelling: `import "lib/util"` and
                    // `import "./lib/util"` must be ONE module instance.
                    path: canonical_key(&candidate),
                    origin,
                });
            }
        }
    }
    if let Some(path) = embedded_path(spec) {
        return Ok(ResolvedModule {
            path,
            origin: ModuleOrigin::Embedded,
        });
    }
    Err(Diagnostic::new(
        Phase::Semantic,
        format!(
            "module '{}' not found (looked next to the importing file and on SPID3R_PATH)",
            spec
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_absolute_and_parent_specs_before_touching_the_disk() {
        for spec in ["/etc/passwd", "C:/Windows/win.ini", "../escape", "a/../b"] {
            let e = resolve_module("main.matrix", spec, &[]).unwrap_err();
            assert_eq!(e.phase, Phase::Semantic, "spec {spec}");
            assert!(
                e.message.contains("invalid module path"),
                "spec {spec}: {}",
                e.message
            );
        }
    }

    #[test]
    fn rejects_specs_that_look_like_the_embedded_marker() {
        // `<stdlib>/name.matrix` is the synthetic path the embedded table is
        // keyed on. A spec must never be able to spell it, or a file read
        // could be confused with a compiled-in module.
        for spec in ["<stdlib>/semver", "<stdlib>/semver.matrix"] {
            let e = resolve_module("main.matrix", spec, &[]).unwrap_err();
            assert_eq!(e.phase, Phase::Semantic, "spec {spec}");
            assert!(
                e.message.contains("invalid module path"),
                "spec {spec}: {}",
                e.message
            );
        }
    }

    #[test]
    fn the_embedded_table_covers_the_shipped_stdlib() {
        // The fallback is real: `semver` is in the compiled-in table, so an
        // install with no `stdlib/` on disk can still resolve it. Whether
        // resolution actually lands on the table depends on the environment
        // (a shipped or project-local `stdlib/` is found first), which is the
        // precedence the next test pins.
        assert!(
            super::embedded_path("semver").is_some(),
            "semver must be in the embedded stdlib table"
        );
        let r = resolve_module("main.matrix", "semver", &[]).expect("semver resolves");
        assert!(
            matches!(r.origin, ModuleOrigin::Disk | ModuleOrigin::Embedded),
            "unexpected origin {:?}",
            r.origin
        );
    }

    #[test]
    fn the_importers_directory_wins_over_every_search_root() {
        // The first root is the importing file's own directory. A copy there
        // must shadow the same name on `extra_dirs` — and, on any normal
        // checkout, the shipped `stdlib/` behind it. The end-to-end version of
        // this rule (a project-local `stdlib/` beating the shipped copy) is in
        // `tests/module_resolution.rs`, where the working directory is the
        // fixture and the assertion does not depend on where `cargo test` runs.
        let dir = std::env::temp_dir().join(format!("spid3r_resolve_prec_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("stdlib")).expect("mkdir");
        let beside = dir.join("shadowed.matrix");
        std::fs::write(&beside, "public fn marker() { return 1; }\n").expect("write beside");
        std::fs::write(
            dir.join("stdlib").join("shadowed.matrix"),
            "public fn marker() { return 2; }\n",
        )
        .expect("write shadowed");

        let r = resolve_module(
            dir.join("main.matrix").to_str().unwrap(),
            "shadowed",
            &[dir.join("stdlib")],
        )
        .expect("resolve");
        assert_eq!(r.origin, ModuleOrigin::Disk);
        assert_eq!(
            r.path,
            canonical_key(&beside),
            "the importer's directory must be searched first"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_extensionless_file_wins_over_the_matrix_suffix() {
        // Each root is tried as written before the `.matrix` form, so a file
        // literally named `util` shadows `util.matrix` beside it. The order is
        // documented; this pins it so a future "tidy-up" cannot swap it.
        let dir = std::env::temp_dir().join(format!("spid3r_resolve_ext_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        let bare = dir.join("util");
        let suffixed = dir.join("util.matrix");
        std::fs::write(&bare, "public fn f() { return 1; }\n").expect("write bare");
        std::fs::write(&suffixed, "public fn f() { return 2; }\n").expect("write suffixed");

        let r = resolve_module(
            dir.join("main.matrix").to_str().unwrap(),
            "util",
            std::slice::from_ref(&dir),
        )
        .expect("resolve");
        assert_eq!(r.path, canonical_key(&bare), "the bare file must win");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn origin_labels_are_stable() {
        // `spidr modules` prints these; they are part of the CLI contract.
        assert_eq!(ModuleOrigin::Disk.as_str(), "disk");
        assert_eq!(ModuleOrigin::Vendor.as_str(), "vendor");
        assert_eq!(ModuleOrigin::Embedded.as_str(), "embedded");
        assert_eq!(ModuleOrigin::Plugin.as_str(), "plugin");
    }

    #[test]
    fn missing_module_reports_the_search_contract() {
        let e = resolve_module("main.matrix", "definitely-not-a-module", &[]).unwrap_err();
        assert_eq!(e.phase, Phase::Semantic);
        assert!(e.message.contains("not found"), "{}", e.message);
    }
}
