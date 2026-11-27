//! The `stdlib/*.matrix` sources compiled into the binary.
//!
//! Two consumers:
//!
//! * a browser has no filesystem, so `import "json"` cannot be served from disk
//!   the way it is on desktop;
//! * a **single-file app** that ships `.matrix` source (`--embed app.matrix`)
//!   runs from a synthetic path with no `stdlib/` beside it, and the runtime
//!   `--bundle-runtime` extracts lives alone in the user cache. Without this
//!   table `import "semver"` failed with "module not found" on exactly the
//!   clean machine the single-file build exists for.
//!
//! A third case falls out of the second: the installer ships no `stdlib/`
//! either, so on an installed machine this table *is* the standard library.
//! All three backends (tree walker, VM, AOT) fall back to it, and all three
//! prefer a file when one exists, so a project-local `stdlib/` still wins.
//!
//! `build.rs` emits the DEFLATE-compressed table; the cost is ~85 KB in
//! every binary, inflated lazily once on first lookup.

include!(concat!(env!("OUT_DIR"), "/embedded_stdlib.rs"));

static UNPACKED: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn unpacked() -> &'static str {
    UNPACKED
        .get_or_init(|| {
            let bytes = crate::stdlib::compress::inflate_raw(EMBEDDED_STDLIB_DEFLATE)
                .expect("embedded stdlib deflate payload is valid");
            String::from_utf8(bytes).expect("embedded stdlib payload is valid UTF-8")
        })
        .as_str()
}

/// Source of the stdlib module called `name`, if one is embedded.
///
/// `name` is the import string, with or without the `.matrix` or `.mt` extension.
pub fn source(name: &str) -> Option<&'static str> {
    let trimmed = name.trim();
    let key = trimmed
        .strip_suffix(".matrix")
        .or_else(|| trimmed.strip_suffix(".mt"))
        .unwrap_or(trimmed);
    EMBEDDED_STDLIB_INDEX
        .iter()
        .find(|(module, _, _)| *module == key)
        .map(|&(_, start, end)| &unpacked()[start..end])
}

/// Every embedded module name, sorted (used by `spid3r --list-stdlib` in the
/// browser and by tests).
pub fn names() -> Vec<&'static str> {
    EMBEDDED_STDLIB_INDEX
        .iter()
        .map(|(name, _, _)| *name)
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn table_is_not_empty_and_lookup_works() {
        assert!(!super::names().is_empty());
        // Pick a module that exists in `stdlib/`; the table is generated from
        // that directory, so a rename here means the generator broke.
        let src = super::source("semver").expect("semver must be embedded");
        assert!(src.contains("public fn"));
        assert_eq!(
            super::source("semver.matrix").map(str::len),
            Some(src.len())
        );
        assert!(super::source("definitely-not-a-module").is_none());
    }

    /// Every entry must be resolvable by name and carry real source, and when
    /// running inside the source checkout it must match `stdlib/<name>.matrix`
    /// byte-for-byte after DEFLATE round-trip.
    #[test]
    fn every_entry_has_source() {
        let names = super::names();
        assert!(names.len() > 5, "only {} modules embedded", names.len());
        let has_disk_stdlib = std::path::Path::new("stdlib").is_dir();
        for name in names {
            let src = super::source(name).expect("lookup by own name");
            assert!(!src.trim().is_empty(), "{} is empty", name);
            if has_disk_stdlib {
                let disk_path = format!("stdlib/{}.matrix", name);
                if std::path::Path::new(&disk_path).is_file() {
                    let disk_src = std::fs::read_to_string(&disk_path).expect("read disk stdlib");
                    assert_eq!(src, disk_src, "embedded stdlib mismatch for {}", name);
                }
            }
        }
    }
}
