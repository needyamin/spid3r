//! The phase-3 security policy: `security.basedir`, `security.disable_natives`
//! and `modules.disable`, enforced at the [`crate::stdlib::native_defs`] funnel.
//!
//! All three lists are read **once**, the first time the policy is needed
//! (engine install, module resolution, native dispatch), from the process's
//! resolved configuration. Baking them into a process-global prevents a
//! script from widening its own permissions mid-run: `config_set` can still
//! mutate the live config snapshot, but the baked policy — the thing the
//! guards below actually compare against — never re-reads it. This mirrors
//! the sandbox arming model (`arm_sandbox`), and it is why all three keys
//! are classified `StartupOnly` and `frozen`.
//!
//! # basedir (`security.basedir`)
//!
//! An open_basedir analog: when non-empty, every native that takes a
//! script-controlled path may only touch paths inside the listed
//! directories. The enforcement guarantees, and their honest limits, are
//! documented in `docs/SECURITY.md`.
//!
//! Comparison rules (see [`resolve_for_compare`]):
//!
//! 1. **component-wise prefix match**, never a raw string prefix — basedir
//!    `C:\foo\bar` does not match `C:\foo\barbaz.txt`;
//! 2. **Windows case-insensitive** component comparison;
//! 3. symlinks/junctions are resolved with `fs::canonicalize` before the
//!    comparison, so a link inside basedir pointing outside is outside;
//! 4. a path that cannot be resolved is **rejected** (deny by default);
//! 5. Windows verbatim (`\\?\`) and UNC (`\\?\UNC\`) spellings normalize the
//!    same way on both sides of the comparison because both go through the
//!    same resolver;
//! 6. relative script paths resolve against the process cwd; a relative
//!    basedir entry is resolved against the cwd at bake time.
//!
//! An empty list means no restriction. A basedir entry that does not exist
//! on disk matches nothing — entries are baked from existing directories
//! only — so a typo in the only entry denies every path rather than
//! silently re-scoping the allowlist.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use crate::value::{NativeCtx, Value};

/// The baked process-wide policy.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// Canonical basedir entries. `None` is an entry that could not be
    /// resolved and therefore matches nothing (deny by default).
    basedir: Vec<Option<PathBuf>>,
    /// Natives replaced by the configuration stub.
    pub(crate) disabled_natives: Vec<String>,
    /// Module names (as written by the import) that fail to resolve.
    disabled_modules: Vec<String>,
    /// Sticky indicator: if this process baked policy while an explicit pin
    /// was broken, a later reload cannot safely widen the cached policy.
    pub(crate) pin_failed_at_initialization: bool,
}

static POLICY: OnceLock<Policy> = OnceLock::new();

/// The baked policy, initialized on first use from the resolved config.
pub fn policy() -> &'static Policy {
    POLICY.get_or_init(Policy::from_config)
}

/// Refuse program execution while an explicit config pin is broken. If the
/// process already baked a policy under a broken pin, require a restart even
/// after config reload: the OnceLock policy cannot be safely refreshed.
pub fn execution_refusal() -> Option<String> {
    let cfg = crate::config::snapshot();
    if crate::config::pin_failed(&cfg) {
        return Some(crate::config::pin_problems(&cfg).join("; "));
    }
    if POLICY
        .get()
        .is_some_and(|policy| policy.pin_failed_at_initialization)
    {
        return Some(
            "runtime policy was initialized while SPID3R_INI was broken; restart Spid3r after repairing the pin".into(),
        );
    }
    None
}

impl Policy {
    fn from_config() -> Self {
        let cfg = crate::config::snapshot();
        let basedir = split_list(cfg.get("security.basedir").unwrap_or(""))
            .into_iter()
            .map(|entry| resolve_entry_strict(&entry))
            .collect();
        let disabled_natives = split_list(cfg.get("security.disable_natives").unwrap_or(""));
        let disabled_modules = split_list(cfg.get("modules.disable").unwrap_or(""));
        let pin_failed_at_initialization = crate::config::pin_failed(&cfg);
        Policy {
            basedir,
            disabled_natives,
            disabled_modules,
            pin_failed_at_initialization,
        }
    }
}

/// Split a list value on ';' and ',', trimming empties.
fn split_list(raw: &str) -> Vec<String> {
    raw.split([',', ';'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// Resolve a path for basedir comparison: canonicalize the deepest existing
/// ancestor, then re-append the remaining components (which keep their input
/// spelling and case). Returns `None` when even the filesystem root cannot
/// be canonicalized — deny by default.
///
/// Relative paths resolve against the process cwd first.
fn resolve_for_compare(path: &str) -> Option<PathBuf> {
    if path.trim().is_empty() {
        return None;
    }
    let p = Path::new(path);
    let abs: PathBuf = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(p)
    };
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut cur: &Path = abs.as_path();
    loop {
        match std::fs::canonicalize(cur) {
            Ok(real) => {
                let mut resolved = real;
                for part in missing.iter().rev() {
                    resolved.push(part);
                }
                return Some(resolved);
            }
            Err(_) => {
                let last = cur.file_name()?;
                let parent = cur.parent()?;
                missing.push(last.to_os_string());
                cur = parent;
            }
        }
    }
}

/// Resolve a basedir ENTRY — strictly. Unlike a script target (which may be
/// a file that does not exist yet, so the ancestor fallback applies), an
/// entry must exist on disk at bake time: an entry that does not exist is
/// kept as an unresolvable placeholder that matches nothing, so a typo in
/// the only entry denies every path instead of silently re-scoping the
/// allowlist to a directory the script could create (`dir_create` under a
/// typo'd hypothetical prefix would otherwise hand the script a writable
/// directory outside the operator's intent). `config validate` notes such
/// entries.
fn resolve_entry_strict(path: &str) -> Option<PathBuf> {
    if path.trim().is_empty() {
        return None;
    }
    let p = Path::new(path);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(p),
            Err(_) => return None,
        }
    };
    std::fs::canonicalize(&abs).ok()
}

/// Case-sensitive on POSIX, case-insensitive on Windows: does `target` lie
/// inside `entry`? Both paths must already be in resolved (canonical) form.
fn inside(entry: &Path, target: &Path) -> bool {
    let mut e = entry.components();
    let mut t = target.components();
    loop {
        match (e.next(), t.next()) {
            // The entry's components are exhausted: everything left of the
            // target hangs under it.
            (None, _) => return true,
            // The target ran out first: it is a sibling or a parent.
            (Some(_), None) => return false,
            (Some(ec), Some(tc)) => {
                if !component_eq(&ec, &tc) {
                    return false;
                }
            }
        }
    }
}

fn component_eq(a: &Component, b: &Component) -> bool {
    match (a, b) {
        (Component::Prefix(pa), Component::Prefix(pb)) => prefix_eq(pa, pb),
        _ => {
            if fs_is_case_insensitive() {
                a.as_os_str().to_string_lossy().to_lowercase()
                    == b.as_os_str().to_string_lossy().to_lowercase()
            } else {
                a.as_os_str() == b.as_os_str()
            }
        }
    }
}

#[cfg(windows)]
fn prefix_eq(a: &std::path::PrefixComponent, b: &std::path::PrefixComponent) -> bool {
    use std::path::Prefix;
    match (a.kind(), b.kind()) {
        (Prefix::Disk(x), Prefix::Disk(y)) => x.eq_ignore_ascii_case(&y),
        (Prefix::VerbatimDisk(x), Prefix::VerbatimDisk(y)) => x.eq_ignore_ascii_case(&y),
        (a, b) => a == b,
    }
}

#[cfg(not(windows))]
fn prefix_eq(a: &std::path::PrefixComponent, b: &std::path::PrefixComponent) -> bool {
    a == b
}

/// Whether the policy restricts paths at all (an empty basedir means no
/// restriction — the default).
pub fn basedir_active(pol: &Policy) -> bool {
    !pol.basedir.is_empty()
}

/// Reject `path` when the basedir allowlist does not contain it. The error
/// mirrors the sandbox denial wording and is catchable by the script.
pub fn check_path(path: &str) -> Result<(), String> {
    let pol = policy();
    if !basedir_active(pol) {
        return Ok(());
    }
    match resolve_for_compare(path) {
        Some(resolved) => {
            for entry in pol.basedir.iter().flatten() {
                if inside(entry, &resolved) {
                    return Ok(());
                }
            }
            Err(format!("path outside basedir: {}", path))
        }
        // Deny by default: a path we cannot resolve could be anything.
        None => Err(format!("path outside basedir: {}", path)),
    }
}

/// The denial raised for a native disabled via `security.disable_natives`.
fn disabled_native_stub(_ctx: &NativeCtx, _args: Vec<Value>) -> Result<Value, String> {
    Err("disabled by configuration".to_string())
}

pub(crate) fn pin_failure_stub(_ctx: &NativeCtx, _args: Vec<Value>) -> Result<Value, String> {
    Err("runtime policy initialized while SPID3R_INI was broken; restart Spid3r".to_string())
}

/// The stub function installed for a config-disabled native.
pub fn disabled_stub_fn() -> crate::vm::NativeImpl {
    disabled_native_stub
}

/// Why a module import must fail, or `None` when the name is allowed.
///
/// On Windows the comparison is **case-insensitive**, like NTFS itself: the
/// module files backing `import "MATHX"` and `import "mathx"` are the same
/// file, so a case-sensitive deny list would be a one-keystroke bypass.
/// On POSIX the comparison is case-sensitive — file names there really do
/// differ by case. Both spellings of a module (with and without the
/// `.matrix` suffix the resolver treats as equivalent) are matched.
pub fn module_disabled(spec: &str) -> Option<String> {
    let pol = policy();
    if pol.disabled_modules.is_empty() {
        return None;
    }
    disabled_by_module_names(&pol.disabled_modules, spec)
}

/// The match itself, factored out so the unit test exercises exactly the
/// code the resolver gate runs.
fn disabled_by_module_names(names: &[String], spec: &str) -> Option<String> {
    let bare = strip_matrix_suffix(spec);
    for name in names {
        let name_bare = strip_matrix_suffix(name);
        if name_matches(name, spec) || name_matches(name_bare, bare) {
            return Some(format!("module disabled by configuration: '{}'", spec));
        }
    }
    None
}

/// Strip the `.matrix` suffix the resolver treats as equivalent. On Windows
/// the suffix compares case-insensitively (the file-name rules): the file
/// `json.Matrix` is the same file as `json.matrix` there.
fn strip_matrix_suffix(s: &str) -> &str {
    if let Some(body) = s.strip_suffix(".matrix") {
        return body;
    }
    if fs_is_case_insensitive() && s.len() >= 7 && s.is_char_boundary(s.len() - 7) {
        let (body, ext) = s.split_at(s.len() - 7);
        if ext.eq_ignore_ascii_case(".matrix") {
            return body;
        }
    }
    s
}

/// Whether the platform's default file system resolves file names
/// case-insensitively. Windows (NTFS) always; macOS (APFS default) also —
/// a case-sensitive APFS volume is possible but rare, and a security gate
/// must follow the default posture (over-deny beats bypass). Linux ext4
/// defaults to case-sensitive.
fn fs_is_case_insensitive() -> bool {
    cfg!(windows) || cfg!(target_os = "macos")
}

/// One module-name comparison, with the platform's file-name case rules.
fn name_matches(a: &str, b: &str) -> bool {
    if fs_is_case_insensitive() {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// Whether `assert()`/`assert_eq()` check their condition. Read live: the
/// flag can only make assertions *looser*, which is benign either way, so
/// unlike the deny lists it is not baked.
pub fn assert_active() -> bool {
    crate::config::get("modules.assert_active")
        .map(|v| crate::config::schema::parse_bool(&v))
        .unwrap_or(true)
}

/// Extract a string path argument, or `None` (the inner native then reports
/// its own type error).
fn path_arg(args: &[Value], idx: usize) -> Option<String> {
    match args.get(idx) {
        Some(Value::Str(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// Generate a wrapper that checks every listed argument position against the
/// basedir allowlist before delegating to the real native.
macro_rules! basedir_guard {
    ($name:ident, $inner:expr, [$($idx:literal),*]) => {
        fn $name(ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
            $(
                if let Some(p) = path_arg(&args, $idx) {
                    check_path(&p)?;
                }
            )*
            $inner(ctx, args)
        }
    };
}

// The guarded wrappers. One wrapper per native signature shape; argument
// indices are the path positions.
basedir_guard!(g_read_file, super::n_read_file, [0]);
basedir_guard!(g_write_file, super::n_write_file, [0]);
basedir_guard!(g_write_file_atomic, super::n_write_file_atomic, [0]);
basedir_guard!(g_file_exists, super::n_file_exists, [0]);
basedir_guard!(g_file_delete, super::n_file_delete, [0]);
basedir_guard!(g_rename, super::n_rename, [0, 1]);
basedir_guard!(g_copy_file, super::n_copy_file, [0, 1]);
basedir_guard!(g_list_dir, super::n_list_dir, [0]);
basedir_guard!(g_is_dir, super::n_is_dir, [0]);
basedir_guard!(g_is_file, super::n_is_file, [0]);
basedir_guard!(g_file_size, super::n_file_size, [0]);
basedir_guard!(g_file_mtime, super::n_file_mtime, [0]);
basedir_guard!(g_chdir, super::n_chdir, [0]);
basedir_guard!(g_dir_create, super::n_dir_create, [0]);
basedir_guard!(g_dir_remove, super::n_dir_remove, [0]);
basedir_guard!(g_read_bytes, super::n_read_bytes, [0]);
basedir_guard!(g_write_bytes, super::n_write_bytes, [0]);
basedir_guard!(g_save_npy, super::n_save_npy, [0]);
basedir_guard!(g_load_npy, super::n_load_npy, [0]);
basedir_guard!(g_read_csv, super::n_read_csv, [0]);
basedir_guard!(g_write_csv, super::n_write_csv, [0]);
basedir_guard!(g_db_open, super::db::n_db_open, [0]);
basedir_guard!(g_tcp_recv_to_file, super::net::n_tcp_recv_to_file, [1]);
basedir_guard!(g_tcp_send_file, super::net::n_tcp_send_file, [1]);

/// The guarded replacement for a path-taking native, or `None` when the
/// native takes no script-controlled path (pure computation, network handles,
/// or no path argument).
///
/// The map is pinned by tests below against BOTH sides of the contract:
/// every `Capability::FileSystem` native that takes a path must appear here,
/// and every name here must be a real native.
pub fn guarded(name: &str) -> Option<crate::vm::NativeImpl> {
    match name {
        "read_file" => Some(g_read_file),
        "write_file" => Some(g_write_file),
        "write_file_atomic" => Some(g_write_file_atomic),
        "file_exists" => Some(g_file_exists),
        "file_delete" | "remove_file" => Some(g_file_delete),
        "rename" => Some(g_rename),
        "copy_file" => Some(g_copy_file),
        "list_dir" => Some(g_list_dir),
        "is_dir" => Some(g_is_dir),
        "is_file" => Some(g_is_file),
        "file_size" => Some(g_file_size),
        "file_mtime" => Some(g_file_mtime),
        "chdir" => Some(g_chdir),
        "dir_create" => Some(g_dir_create),
        "dir_remove" => Some(g_dir_remove),
        "read_bytes" => Some(g_read_bytes),
        "write_bytes" => Some(g_write_bytes),
        "save_npy" => Some(g_save_npy),
        "load_npy" => Some(g_load_npy),
        "read_csv" => Some(g_read_csv),
        "write_csv" => Some(g_write_csv),
        // Beyond the Capability::FileSystem list: the SQLite database file
        // handle, and the two TCP natives that stream to/from a script path.
        "db_open" => Some(g_db_open),
        "tcp_recv_to_file" => Some(g_tcp_recv_to_file),
        "tcp_send_file" => Some(g_tcp_send_file),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure-logic basedir checks that do not depend on the baked policy:
    /// the resolver and the component-wise comparison.
    fn allowed(entries: &[&str], target: &str) -> bool {
        let pol = Policy {
            // Entries resolve STRICTLY, exactly like Policy::from_config.
            basedir: entries.iter().map(|e| resolve_entry_strict(e)).collect(),
            disabled_natives: Vec::new(),
            disabled_modules: Vec::new(),
            pin_failed_at_initialization: false,
        };
        match resolve_for_compare(target) {
            Some(resolved) => pol
                .basedir
                .iter()
                .flatten()
                .any(|entry| inside(entry, &resolved)),
            None => false,
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("spid3r_fs_policy_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn component_prefix_not_string_prefix() {
        // basedir C:\foo\bar must not match C:\foo\barbaz.txt: the next
        // component after `foo` is `bar` vs `barbaz.txt`.
        let dir = scratch("prefix");
        std::fs::create_dir_all(dir.join("foo").join("bar")).expect("mkdir");
        std::fs::write(dir.join("foo").join("barbaz.txt"), b"x").expect("write");
        let base = dir.join("foo").join("bar");
        assert!(allowed(
            &[base.to_str().unwrap()],
            dir.join("foo").join("bar").join("in.txt").to_str().unwrap()
        ));
        assert!(
            !allowed(
                &[base.to_str().unwrap()],
                dir.join("foo").join("barbaz.txt").to_str().unwrap()
            ),
            "a string-prefix match would let this through"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nonexistent_target_resolves_through_its_existing_ancestor() {
        // The target file does not exist yet (a write), so canonicalize
        // fails; the nearest existing ancestor carries the comparison and
        // the missing components are appended.
        let dir = scratch("ancestor");
        std::fs::create_dir_all(dir.join("inside")).expect("mkdir");
        let inside = dir.join("inside");
        assert!(allowed(
            &[inside.to_str().unwrap()],
            inside.join("new").join("file.txt").to_str().unwrap()
        ));
        let outside = dir.join("outside");
        assert!(!allowed(
            &[inside.to_str().unwrap()],
            outside.join("file.txt").to_str().unwrap()
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn symlink_inside_pointing_outside_is_outside() {
        let dir = scratch("link");
        std::fs::create_dir_all(dir.join("bd")).expect("mkdir bd");
        std::fs::create_dir_all(dir.join("outside")).expect("mkdir outside");
        std::fs::write(dir.join("outside").join("secret.txt"), b"x").expect("write");
        let link = dir.join("bd").join("link");
        #[cfg(windows)]
        let made = {
            // A junction needs no privilege; a real symlink does unless
            // developer mode is on. `mklink /J` is the portable choice.
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(dir.join("outside"))
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(dir.join("outside"), &link).is_ok();
        if !made {
            eprintln!("skipping junction/symlink assertion: could not create the link");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert!(
            !allowed(
                &[dir.join("bd").to_str().unwrap()],
                link.join("secret.txt").to_str().unwrap()
            ),
            "canonicalize must resolve the link before comparing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_paths_resolve_against_cwd() {
        // No cwd mutation (process-global, races other tests): cargo test
        // runs at the workspace root, where Cargo.toml exists, so a relative
        // path must resolve to the same absolute path as the cwd spelling.
        let resolved = resolve_for_compare("Cargo.toml").expect("resolves");
        assert!(resolved.is_absolute());
        let cwd = std::env::current_dir().expect("cwd");
        let explicit = resolve_for_compare(cwd.join("Cargo.toml").to_str().unwrap()).unwrap();
        assert_eq!(resolved, explicit);
    }

    #[test]
    fn nonexistent_entry_matches_nothing() {
        // Entries are baked STRICTLY (must exist): an entry whose directory
        // was never created resolves to nothing, so every path is denied —
        // including a path "inside" the entry, whose hypothetical prefix
        // must not be handed to the script (it could dir_create it). The
        // ancestor fallback applies to script TARGETS only.
        let dir = scratch("unresolvable");
        std::fs::write(dir.join("anything.txt"), b"x").expect("write");
        assert!(!allowed(
            &[dir.join("ghost").join("deeper").to_str().unwrap()],
            dir.join("anything.txt").to_str().unwrap()
        ));
        // Even a target under the ghost entry itself is denied: the entry
        // matches nothing, so nothing can be inside it.
        assert!(!allowed(
            &[dir.join("ghost").join("deeper").to_str().unwrap()],
            dir.join("ghost")
                .join("deeper")
                .join("f.txt")
                .to_str()
                .unwrap()
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_list_is_no_restriction() {
        let pol = Policy::default();
        assert!(!basedir_active(&pol));
    }

    /// Both sides of the guard contract, pinned:
    /// (1) every `Capability::FileSystem` native that takes a path argument
    ///     is guarded (exe_path takes none — it *returns* a path);
    /// (2) the extra path-taking natives beyond that list (the SQLite file
    ///     handle and the two TCP file-streaming natives) are guarded;
    /// (3) every guarded name is a real builtin native.
    /// Missing one entry here is a security hole; this test is the
    /// exhaustiveness proof.
    #[test]
    fn the_guard_map_is_exhaustive() {
        for name in crate::embed::Capability::FileSystem.natives() {
            if *name == "exe_path" {
                assert!(guarded(name).is_none(), "exe_path takes no path argument");
                continue;
            }
            assert!(guarded(name).is_some(), "{} must be guarded", name);
        }
        for name in ["db_open", "tcp_recv_to_file", "tcp_send_file"] {
            assert!(guarded(name).is_some(), "{} must be guarded", name);
        }
        let builtins: std::collections::HashSet<&str> = crate::stdlib::builtin_defs()
            .iter()
            .map(|(n, _)| *n)
            .collect();
        for name in [
            "read_file",
            "write_file",
            "write_file_atomic",
            "file_exists",
            "file_delete",
            "remove_file",
            "rename",
            "copy_file",
            "list_dir",
            "is_dir",
            "is_file",
            "file_size",
            "file_mtime",
            "chdir",
            "dir_create",
            "dir_remove",
            "read_bytes",
            "write_bytes",
            "save_npy",
            "load_npy",
            "read_csv",
            "write_csv",
            "db_open",
            "tcp_recv_to_file",
            "tcp_send_file",
        ] {
            assert!(
                builtins.contains(name),
                "guarded name '{}' is not a builtin native",
                name
            );
        }
    }

    #[test]
    fn module_disabled_matches_name_as_written() {
        // Direct policy construction: resolve_module's gate calls this with
        // the import spelling. Case rules follow the platform's file-name
        // rules: NTFS is case-insensitive, so `import "MATHX"` resolves the
        // same file as "mathx" there and must be denied too; POSIX
        // file names really differ by case, so it stays exact.
        let pol = Policy {
            basedir: Vec::new(),
            disabled_natives: Vec::new(),
            disabled_modules: vec!["json".to_string()],
            pin_failed_at_initialization: false,
        };
        assert!(module_disabled_in(&pol, "json").is_some());
        assert!(module_disabled_in(&pol, "json.matrix").is_some());
        assert!(module_disabled_in(&pol, "jsonx").is_none());
        assert!(module_disabled_in(&pol, "mathx").is_none());
        if fs_is_case_insensitive() {
            assert!(
                module_disabled_in(&pol, "JSON").is_some(),
                "on a case-insensitive-FS platform (Windows/macOS) a case variant must be denied"
            );
            assert!(
                module_disabled_in(&pol, "Json.Matrix").is_some(),
                "the .matrix suffix form must follow the same case rules"
            );
        } else {
            assert!(
                module_disabled_in(&pol, "JSON").is_none(),
                "on a case-sensitive-FS platform (Linux) the comparison stays exact"
            );
        }
    }

    fn module_disabled_in(pol: &Policy, spec: &str) -> Option<String> {
        super::disabled_by_module_names(&pol.disabled_modules, spec)
    }
}
