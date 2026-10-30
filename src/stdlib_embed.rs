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
//! The module loader falls back to it only when normal resolution fails, so a
//! project-local `stdlib/` still wins everywhere it exists. `build.rs` emits the
//! table; the cost is the size of `stdlib/` (~420 KB) in every binary.

include!(concat!(env!("OUT_DIR"), "/embedded_stdlib.rs"));

/// Source of the stdlib module called `name`, if one is embedded.
///
/// `name` is the import string, with or without the `.matrix` extension.
pub fn source(name: &str) -> Option<&'static str> {
    let key = name.trim().trim_end_matches(".matrix");
    EMBEDDED_STDLIB
        .iter()
        .find(|(module, _)| *module == key)
        .map(|(_, src)| *src)
}

/// Every embedded module name, sorted (used by `spid3r --list-stdlib` in the
/// browser and by tests).
pub fn names() -> Vec<&'static str> {
    EMBEDDED_STDLIB.iter().map(|(name, _)| *name).collect()
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

    /// Every entry must be resolvable by name and carry real source. A module
    /// that was deleted from `stdlib/` but left in the table (or the reverse)
    /// would make `import` behave differently on wasm and on desktop.
    #[test]
    fn every_entry_has_source() {
        let names = super::names();
        assert!(names.len() > 5, "only {} modules embedded", names.len());
        for name in names {
            let src = super::source(name).expect("lookup by own name");
            assert!(!src.trim().is_empty(), "{} is empty", name);
        }
    }
}
