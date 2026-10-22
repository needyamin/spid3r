//! The `stdlib/*.matrix` sources compiled into the binary.
//!
//! A browser has no filesystem, so `import "json"` cannot be served from disk
//! the way it is on desktop. `build.rs` emits the table; the module loader
//! falls back to it only when normal resolution fails, so a project-local
//! `stdlib/` still wins everywhere it exists.
//!
//! Only compiled for wasm32 (`src/lib.rs`): desktop builds keep the sources on
//! disk and stay lean.

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
        // `json` is one of the oldest modules; if this fails the generator
        // stopped finding stdlib/*.matrix.
        let src = super::source("json").expect("json must be embedded");
        assert!(src.contains("public fn"));
        assert_eq!(super::source("json.matrix").map(str::len), Some(src.len()));
        assert!(super::source("definitely-not-a-module").is_none());
    }
}
