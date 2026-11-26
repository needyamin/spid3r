//! Phase-3 `spid3r.ini` security keys, end-to-end through the real binary:
//!
//! * `security.basedir` — open_basedir analog: inside allowed, outside and
//!   prefix-confusion and symlink-escape denied, nonexistent targets
//!   resolved through their existing ancestor, empty list = no restriction;
//! * `security.disable_natives` — per-native denial, config_get reflects the
//!   list, unknown names noted and ignored, composes with `--sandbox`;
//! * `modules.disable` — import of a disabled module fails on both engines
//!   with the same message;
//! * `modules.assert_active` — false makes assert() a no-op returning true;
//! * `packages.registry` / `packages.allow_insecure_git` — ini vs the
//!   `SPID3R_*` env aliases behave identically;
//! * the surfaces (`info`, `config init`, `config validate`) carry the new
//!   keys.
//!
//! The schema-derived frozen list (agent `config.set`) is pinned by unit
//! tests in `src/config/schema.rs` and `src/agent/mod.rs`; pairing a real
//! agent in CI for it would be overkill.
//!
//! Every case runs in its own process (Command::new) so the SPID3R_INI file
//! and env overrides are loaded by that process's config loader. Exit
//! codes: 0 success, 1 program error, 2 usage error.

use std::path::Path;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_spid3r")
}

fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r_phase3_{}", tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn write_in(dir: &Path, name: &str, src: &str) -> String {
    let p = dir.join(name);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&p, src).expect("write");
    p.to_str().unwrap().to_string()
}

/// Run `spid3r` from `cwd` with `env` set and `remove` dropped from the
/// inherited environment (so machine-level SPID3R_* variables cannot leak
/// into the package-manager assertions).
fn run_in(
    cwd: &Path,
    env: &[(&str, &str)],
    remove: &[&str],
    args: &[&str],
) -> (i32, String, String) {
    let mut cmd = Command::new(bin());
    cmd.args(args).current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    for k in remove {
        cmd.env_remove(k);
    }
    let out = cmd.output().expect("spawn spid3r");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// Run from the repo-independent scratch dir (for `run`/`info`/`config`
/// commands that take an explicit file path).
fn run_with(env: &[(&str, &str)], remove: &[&str], args: &[&str]) -> (i32, String, String) {
    let cwd = std::env::temp_dir();
    run_in(&cwd, env, remove, args)
}

fn run_with_engine(engine: &str, env: &[(&str, &str)], args: &[&str]) -> (i32, String, String) {
    let mut argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    if engine == "vm" {
        argv.push("--engine".to_string());
        argv.push("vm".to_string());
    }
    let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    run_with(env, &[], &refs)
}

// ------------------------------------------------------------------ basedir

const OUTSIDE_CATCH: &str = r#"fn main() {
    try { read_file("$TARGET$"); println("BAD"); } catch e { println("caught: " + e); }
    return 0;
}
"#;

/// Substitute `$NAME$` placeholders with absolute paths. Forward slashes
/// are used deliberately: backslashes inside Spid3r string literals are
/// escape sequences (`\U` is not a path separator), and INI values go
/// through the loader's `\n`/`\t` unescaping as well. Every Windows path
/// API the runtime touches accepts forward slashes.
fn fwd(p: &Path) -> String {
    p.to_str().unwrap().replace('\\', "/")
}

fn subst(template: &str, pairs: &[(&str, &Path)]) -> String {
    let mut out = template.to_string();
    for (placeholder, path) in pairs {
        out = out.replace(placeholder, &fwd(path));
    }
    out
}

/// Inside the basedir everything works: write, read back, and a stat on a
/// path that does not exist yet (the ancestor-resolution path of the guard).
#[test]
fn basedir_allows_paths_inside() {
    let dir = scratch_dir("bd_inside");
    let ini = write_in(&dir, "spid3r.ini", "");
    let bd = dir.join("bd");
    std::fs::create_dir_all(&bd).expect("mkdir bd");
    let script = write_in(
        &dir,
        "s.matrix",
        &subst(
            r#"fn main() {
    write_file("$BD$/in.txt", "hello");
    if (read_file("$BD$/in.txt").unwrap() != "hello") { return 2; }
    if (file_exists("$BD$/ghost.txt")) { return 3; }
    return 0;
}
"#,
            &[("$BD$", &bd)],
        ),
    );
    let env = [
        ("SPID3R_INI", ini.as_str()),
        ("SPID3R_SECURITY_BASEDIR", bd.to_str().unwrap()),
    ];
    for engine in ["tree", "vm"] {
        let (code, out, err) = run_with_engine(engine, &env, &["run", &script]);
        assert_eq!(code, 0, "[{engine}] out={} err={}", out, err);
    }
}

/// A path outside the basedir is denied with a catchable error, on both
/// engines, via ini and via the generic env override.
#[test]
fn basedir_denies_paths_outside() {
    let dir = scratch_dir("bd_outside");
    let outside = dir.join("outside");
    std::fs::create_dir_all(&outside).expect("mkdir outside");
    std::fs::write(outside.join("x.txt"), b"secret").expect("write");
    let ini = write_in(
        &dir,
        "spid3r.ini",
        &format!("[security]\nbasedir = {}", fwd(&dir.join("bd"))),
    );
    let script = write_in(
        &dir,
        "s.matrix",
        &subst(OUTSIDE_CATCH, &[("$TARGET$", &outside.join("x.txt"))]),
    );
    for engine in ["tree", "vm"] {
        let (code, out, err) =
            run_with_engine(engine, &[("SPID3R_INI", ini.as_str())], &["run", &script]);
        assert_eq!(code, 0, "[{engine}] err={}", err);
        assert!(!out.contains("BAD"), "[{engine}] the read must be denied");
        assert!(out.contains("path outside basedir"), "[{engine}] {}", out);
    }

    // The generic env override arms the same policy without an ini.
    let (code, out, _) = run_with_engine(
        "tree",
        &[("SPID3R_SECURITY_BASEDIR", dir.join("bd").to_str().unwrap())],
        &["run", &script],
    );
    assert_eq!(code, 0);
    assert!(out.contains("path outside basedir"), "{}", out);
}

/// Component-wise prefix matching: basedir `...\foo\bar` must not match
/// `...\foo\barbaz.txt`. A raw string-prefix comparison would.
#[test]
fn basedir_prefix_match_is_component_wise() {
    let dir = scratch_dir("bd_prefix");
    let foo = dir.join("foo");
    std::fs::create_dir_all(foo.join("bar")).expect("mkdir");
    std::fs::write(foo.join("barbaz.txt"), b"x").expect("write");
    let ini = write_in(
        &dir,
        "spid3r.ini",
        &format!("[security]\nbasedir = {}", fwd(&foo.join("bar"))),
    );
    let script = write_in(
        &dir,
        "s.matrix",
        &subst(OUTSIDE_CATCH, &[("$TARGET$", &foo.join("barbaz.txt"))]),
    );
    let (code, out, err) = run_with(&[("SPID3R_INI", ini.as_str())], &[], &["run", &script]);
    assert_eq!(code, 0, "err={}", err);
    assert!(
        out.contains("path outside basedir"),
        "a string-prefix match would allow barbaz.txt: {}",
        out
    );
}

/// A junction/symlink inside basedir pointing outside is OUTSIDE: the guard
/// canonicalizes before comparing. Windows junctions (`mklink /J`) need no
/// privilege; if the link cannot be created the case is still covered by the
/// unit test in `src/stdlib/fs_policy.rs`.
#[test]
fn basedir_resolves_links_before_comparing() {
    let dir = scratch_dir("bd_link");
    let bd = dir.join("bd");
    let outside = dir.join("outside");
    std::fs::create_dir_all(&bd).expect("mkdir");
    std::fs::create_dir_all(&outside).expect("mkdir");
    std::fs::write(outside.join("secret.txt"), b"x").expect("write");
    let link = bd.join("link");
    #[cfg(windows)]
    let made = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&outside)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    #[cfg(not(windows))]
    let made = std::os::unix::fs::symlink(&outside, &link).is_ok();
    if !made {
        eprintln!("skipping: could not create the junction/symlink");
        return;
    }
    let ini = write_in(
        &dir,
        "spid3r.ini",
        &format!("[security]\nbasedir = {}", fwd(&bd)),
    );
    let script = write_in(
        &dir,
        "s.matrix",
        &subst(OUTSIDE_CATCH, &[("$TARGET$", &link.join("secret.txt"))]),
    );
    let (code, out, err) = run_with(&[("SPID3R_INI", ini.as_str())], &[], &["run", &script]);
    assert_eq!(code, 0, "err={}", err);
    assert!(
        out.contains("path outside basedir"),
        "the link escapes the allowlist: {}",
        out
    );
}

/// A nonexistent path OUTSIDE the basedir (nothing but the scratch root
/// exists) is still denied: resolution walks up to the nearest existing
/// ancestor and appends the missing components before comparing. The same
/// ghost INSIDE the allowlist resolves fine.
#[test]
fn basedir_denies_nonexistent_paths_outside() {
    let dir = scratch_dir("bd_ghost");
    let bd = dir.join("bd");
    std::fs::create_dir_all(&bd).expect("mkdir");
    let ini = write_in(
        &dir,
        "spid3r.ini",
        &format!("[security]\nbasedir = {}", fwd(&bd)),
    );
    let script = write_in(
        &dir,
        "s.matrix",
        &subst(
            r#"fn main() {
    try { file_exists("$GHOST$/deep/file.txt"); println("BAD"); } catch e { println("caught: " + e); }
    if (file_exists("$BD$/ghost.txt")) { return 3; }
    return 0;
}
"#,
            &[("$GHOST$", &dir.join("ghost")), ("$BD$", &bd)],
        ),
    );
    let (code, out, err) = run_with(&[("SPID3R_INI", ini.as_str())], &[], &["run", &script]);
    assert_eq!(code, 0, "err={}", err);
    assert!(!out.contains("BAD"), "{}", out);
    assert!(out.contains("path outside basedir"), "{}", out);
}

/// A verbatim `\\?\C:\...` spelling of a target outside the basedir is
/// denied exactly like the plain spelling: both sides of the comparison go
/// through the same resolver, so verbatim and non-verbatim forms normalize
/// identically (officer PoC, pinned end-to-end). The verbatim path is passed
/// as a script ARGUMENT (not a string literal): CLI args bypass the lexer's
/// escape processing, which would reject the raw backslashes.
#[test]
fn basedir_denies_verbatim_paths_outside() {
    let dir = scratch_dir("bd_verbatim");
    let bd = dir.join("bd");
    let outside = dir.join("outside");
    std::fs::create_dir_all(&bd).expect("mkdir bd");
    std::fs::create_dir_all(&outside).expect("mkdir outside");
    std::fs::write(outside.join("x.txt"), b"secret").expect("write");
    let ini = write_in(
        &dir,
        "spid3r.ini",
        &format!("[security]\nbasedir = {}", fwd(&bd)),
    );
    // The verbatim spelling of the outside file, derived from the real-case
    // canonical form (\\?\C:\... on Windows, already-verbatim kept as-is).
    let real = std::fs::canonicalize(&outside).expect("canonicalize outside");
    let real_str = real.to_str().unwrap();
    let verbatim_dir = if let Some(rest) = real_str.strip_prefix(r"\\?\") {
        format!(r"\\?\{}", rest)
    } else {
        format!(r"\\?\{}", real_str)
    };
    let verbatim_target = if cfg!(windows) {
        format!("{}\\x.txt", verbatim_dir)
    } else {
        format!("{}/x.txt", verbatim_dir)
    };
    let script = write_in(
        &dir,
        "s.matrix",
        r#"fn main() {
    let target = args()[0];
    try { read_file(target); println("BAD"); } catch e { println("caught: " + e); }
    return 0;
}
"#,
    );
    let argv = [&script, &verbatim_target];
    let (code, out, err) = run_with(
        &[("SPID3R_INI", ini.as_str())],
        &[],
        &["run", argv[0], argv[1]],
    );
    assert_eq!(code, 0, "err={}", err);
    assert!(!out.contains("BAD"), "{}", out);
    assert!(
        out.contains("path outside basedir"),
        "the verbatim spelling must resolve identically: {}",
        out
    );
}

/// A basedir entry that does not exist on disk matches NOTHING — even a
/// path that would be "inside" it (which cannot exist either, since its
/// root is missing) is denied. This pins the deny-by-default fail-safe for
/// a typo'd only-entry (officer PoC; `config validate` notes the typo).
#[test]
fn basedir_ghost_entry_denies_even_inside_paths() {
    let dir = scratch_dir("bd_ghost_entry");
    let ghost = dir.join("ghost"); // never created
    let ini = write_in(
        &dir,
        "spid3r.ini",
        &format!("[security]\nbasedir = {}", fwd(&ghost)),
    );
    let script = write_in(
        &dir,
        "s.matrix",
        &subst(
            r#"fn main() {
    try { file_exists("$GHOST$/inside/file.txt"); println("BAD"); } catch e { println("caught: " + e); }
    try { write_file("$GHOST$/inside/file.txt", "x"); println("BAD"); } catch e { println("caught2: " + e); }
    // Sanity: an unrelated real path outside the (unusable) allowlist is
    // denied for the same reason.
    try { read_file("$REAL$"); println("BAD"); } catch e { println("caught3: " + e); }
    return 0;
}
"#,
            &[("$GHOST$", &ghost), ("$REAL$", &dir.join("s2.matrix"))],
        ),
    );
    let s2 = dir.join("s2.matrix");
    std::fs::write(&s2, b"fn main() { return 0; }\n").expect("write s2");
    let (code, out, err) = run_with(&[("SPID3R_INI", ini.as_str())], &[], &["run", &script]);
    assert_eq!(code, 0, "err={}", err);
    assert!(!out.contains("BAD"), "everything must be denied: {}", out);
    assert!(out.contains("caught"), "{}", out);
    assert!(out.contains("caught2"), "{}", out);
    assert!(out.contains("caught3"), "{}", out);
    // And the typo is visible to the operator through config validate.
    let (_, _, v_err) = run_with(
        &[("SPID3R_INI", ini.as_str())],
        &[],
        &["config", "validate"],
    );
    assert!(
        v_err.contains("does not exist") && v_err.contains("all paths will be denied"),
        "validate must flag the ghost entry: {}",
        v_err
    );
}

/// Empty (default) basedir = no restriction: the pre-phase-3 behavior.
#[test]
fn basedir_empty_is_no_restriction() {
    let dir = scratch_dir("bd_empty");
    let ini = write_in(&dir, "spid3r.ini", "");
    let outside = dir.join("outside");
    std::fs::create_dir_all(&outside).expect("mkdir");
    let script = write_in(
        &dir,
        "s.matrix",
        &subst(
            r#"fn main() {
    write_file("$OUT$/y.txt", "ok");
    if (read_file("$OUT$/y.txt").unwrap() != "ok") { return 2; }
    return 0;
}
"#,
            &[("$OUT$", &outside)],
        ),
    );
    let (code, _out, err) = run_with(&[("SPID3R_INI", ini.as_str())], &[], &["run", &script]);
    assert_eq!(code, 0, "err={}", err);
}

// ---------------------------------------------------------- disable_natives

/// `security.disable_natives = read_file` denies exactly that native with a
/// catchable `disabled by configuration` error; other natives keep working;
/// `config_get` reflects the (normalized) list.
#[test]
fn disable_natives_denies_named_natives_only() {
    let dir = scratch_dir("dn_basic");
    let ini = write_in(
        &dir,
        "spid3r.ini",
        "[security]\ndisable_natives = read_file\n",
    );
    let w = dir.join("w");
    std::fs::create_dir_all(&w).expect("mkdir");
    let script = write_in(
        &dir,
        "s.matrix",
        &subst(
            r#"fn main() {
    try { read_file("$T$/in.txt"); println("BAD"); } catch e { println("rf: " + e); }
    write_file("$W$/x.txt", "written");
    println(config_get("security.disable_natives"));
    return 0;
}
"#,
            &[("$T$", &w), ("$W$", &w)],
        ),
    );
    for engine in ["tree", "vm"] {
        let (code, out, err) =
            run_with_engine(engine, &[("SPID3R_INI", ini.as_str())], &["run", &script]);
        assert_eq!(code, 0, "[{engine}] err={}", err);
        assert!(!out.contains("BAD"), "[{engine}] {}", out);
        assert!(
            out.contains("disabled by configuration"),
            "[{engine}] {}",
            out
        );
        // The policy list is visible to the script that is its subject (it
        // can probe the natives anyway); only the agent's REMOTE surfaces
        // redact it (see the runtime_info policy_keys test).
        assert!(
            out.contains("read_file"),
            "[{engine}] config_get must reflect the list: {}",
            out
        );
    }
    assert_eq!(
        std::fs::read_to_string(w.join("x.txt")).unwrap(),
        "written",
        "write_file must still work"
    );
}

/// Unknown names are noted and ignored: the valid names keep working.
#[test]
fn disable_natives_ignores_unknown_names_with_a_note() {
    let dir = scratch_dir("dn_unknown");
    let ini = write_in(
        &dir,
        "spid3r.ini",
        "[security]\ndisable_natives = read_file, no_such_native\n",
    );
    let script = write_in(
        &dir,
        "s.matrix",
        r#"fn main() {
    try { read_file("nope.txt"); println("BAD"); } catch e { println("rf: " + e); }
    return 0;
}
"#,
    );
    let (code, out, err) = run_with(&[("SPID3R_INI", ini.as_str())], &[], &["run", &script]);
    assert_eq!(code, 0, "err={}", err);
    assert!(out.contains("disabled by configuration"), "{}", out);
    // The note surfaces through `config validate` (the loader notes list).
    let (_, _, v_err) = run_with(
        &[("SPID3R_INI", ini.as_str())],
        &[],
        &["config", "validate"],
    );
    assert!(
        v_err.contains("unknown native 'no_such_native'"),
        "validate must note the unknown name: {}",
        v_err
    );
}

/// disable_natives composes with --sandbox: when both deny a native the
/// call is still denied (either denial text is a denial).
#[test]
fn disable_natives_composes_with_sandbox() {
    let dir = scratch_dir("dn_sandbox");
    let ini = write_in(
        &dir,
        "spid3r.ini",
        "[security]\ndisable_natives = write_file\n",
    );
    let script = write_in(
        &dir,
        "s.matrix",
        r#"fn main() {
    try { write_file("x.txt", "y"); println("BAD"); } catch e { println("caught: " + e); }
    return 0;
}
"#,
    );
    let (code, out, err) = run_with(
        &[("SPID3R_INI", ini.as_str())],
        &[],
        &["run", "--sandbox", &script],
    );
    assert_eq!(code, 0, "err={}", err);
    assert!(!out.contains("BAD"), "{}", out);
    assert!(out.contains("caught"), "{}", out);
    assert!(out.contains("denied"), "{}", out);
}

// ------------------------------------------------------------ modules.disable

/// `modules.disable = json` makes `import "json"` fail with the same clear
/// error on both engines, while other imports keep working.
#[test]
fn modules_disable_blocks_import_on_both_engines() {
    let dir = scratch_dir("md_json");
    let ini = write_in(&dir, "spid3r.ini", "[modules]\ndisable = json\n");
    let script = write_in(
        &dir,
        "s.matrix",
        "import \"json\";\nfn main() { println(\"BAD\"); return 0; }\n",
    );
    let mut messages: Vec<String> = Vec::new();
    for engine in ["tree", "vm"] {
        let (code, out, err) =
            run_with_engine(engine, &[("SPID3R_INI", ini.as_str())], &["run", &script]);
        assert_ne!(code, 0, "[{engine}] the import must fail");
        assert!(!out.contains("BAD"), "[{engine}] {}", out);
        let text = format!("{}{}", out, err);
        assert!(
            text.contains("module disabled by configuration: 'json'"),
            "[{engine}] {}{}",
            out,
            err
        );
        messages.push(text);
    }
    assert_eq!(
        messages[0], messages[1],
        "both engines must report the identical diagnostic"
    );
}

/// Other modules import fine; the list reaches config_get.
#[test]
fn modules_disable_leaves_other_modules_alone() {
    let dir = scratch_dir("md_other");
    let ini = write_in(&dir, "spid3r.ini", "[modules]\ndisable = json\n");
    let script = write_in(
        &dir,
        "s.matrix",
        "import \"mathx\";\nfn main() { println(config_get(\"modules.disable\")); return 0; }\n",
    );
    let (code, out, err) = run_with(&[("SPID3R_INI", ini.as_str())], &[], &["run", &script]);
    assert_eq!(code, 0, "err={}", err);
    assert!(
        out.contains("json"),
        "config_get reflects the list: {}",
        out
    );
}

/// A case-variant import must never load a disabled module, on any platform.
/// The gate follows the platform's default file-name case rules: on Windows
/// (NTFS) and macOS (APFS default) file systems resolve case-insensitively,
/// so `import "MATHX"` resolves the same module file as "mathx" and the deny
/// gate must fold case the same way (it fires before resolution, even when
/// no module file exists). On Linux (ext4, case-sensitive) the gate compares
/// exactly, so the gate does not fire — but the file system cannot resolve
/// "MATHX" either, so the case-variant import still fails at resolution.
/// Either way there is no bypass.
#[test]
fn modules_disable_case_variant_follows_platform_case_rules() {
    let ci_fs = cfg!(windows) || cfg!(target_os = "macos");
    let dir = scratch_dir("md_case");
    let ini = write_in(&dir, "spid3r.ini", "[modules]\ndisable = mathx\n");
    let script = write_in(
        &dir,
        "s.matrix",
        "import \"MATHX\";\nfn main() { println(\"BAD\"); return 0; }\n",
    );
    let mut messages: Vec<String> = Vec::new();
    for engine in ["tree", "vm"] {
        let (code, out, err) =
            run_with_engine(engine, &[("SPID3R_INI", ini.as_str())], &["run", &script]);
        assert_ne!(
            code, 0,
            "[{engine}] the case-variant import must fail on every platform"
        );
        assert!(!out.contains("BAD"), "[{engine}] {}", out);
        let text = format!("{}{}", out, err);
        if ci_fs {
            assert!(
                text.contains("module disabled by configuration: 'MATHX'"),
                "[{engine}] {}{}",
                out,
                err
            );
            messages.push(text);
        } else {
            // Case-sensitive FS (Linux): the gate is exact, so the failure
            // must come from resolution ("module not found"), not the gate.
            assert!(
                !text.contains("module disabled by configuration"),
                "[{engine}] the failure must come from resolution here: {}{}",
                out,
                err
            );
        }
    }
    if ci_fs {
        assert_eq!(
            messages[0], messages[1],
            "both engines must report the identical diagnostic"
        );
    }
}

// -------------------------------------------------------- modules.assert_active

/// With `modules.assert_active = false`, assert() is a no-op returning true;
/// with the default (true) it still throws.
#[test]
fn assert_active_gates_assert() {
    let dir = scratch_dir("aa");
    let script = write_in(
        &dir,
        "s.matrix",
        r#"fn main() {
    let r = assert(1 == 2);
    println(r);
    return 0;
}
"#,
    );
    // Default: the assertion fires.
    let empty_ini = write_in(&dir, "empty.ini", "");
    let (code, _, err) = run_with(
        &[("SPID3R_INI", empty_ini.as_str())],
        &[],
        &["run", &script],
    );
    assert_ne!(code, 0, "the assertion must fire by default: {}", err);
    assert!(err.contains("assertion failed"), "{}", err);

    // Inactive: no-op returning true.
    let ini = write_in(&dir, "spid3r.ini", "[modules]\nassert_active = false\n");
    let (code, out, err) = run_with(&[("SPID3R_INI", ini.as_str())], &[], &["run", &script]);
    assert_eq!(code, 0, "err={}", err);
    assert!(out.contains("true"), "{}", out);
}

// ------------------------------------------------------------------ packages

/// A registry dependency resolves from `packages.registry` exactly like it
/// does from the `SPID3R_REGISTRY` env alias, and the not-set error names
/// the alias either way. No network: the registry is an empty directory.
#[test]
fn packages_registry_ini_matches_env_alias() {
    let dir = scratch_dir("pkg_registry");
    let reg = dir.join("reg");
    std::fs::create_dir_all(&reg).expect("mkdir reg");
    let app = dir.join("app");
    std::fs::create_dir_all(app.join("src")).expect("mkdir app");
    write_in(
        &app,
        "spider.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nmathx = { version = \"^1.0\" }\n",
    );
    write_in(&app, "src/main.matrix", "fn main() { return 0; }\n");

    // Env alias, empty registry: the package is not there.
    let (code, _, err) = run_in(
        &app,
        &[("SPID3R_REGISTRY", reg.to_str().unwrap())],
        &[],
        &["install"],
    );
    assert_ne!(code, 0, "install must fail against an empty registry");
    assert!(
        err.contains("registry: no package 'mathx'"),
        "env alias error: {}",
        err
    );

    // ini key, both env spellings removed: the same message.
    let ini = write_in(
        &dir,
        "spid3r.ini",
        &format!("[packages]\nregistry = {}", fwd(&reg)),
    );
    let (code, _, err2) = run_in(
        &app,
        &[("SPID3R_INI", ini.as_str())],
        &["SPID3R_REGISTRY", "SPID3R_PACKAGES_REGISTRY"],
        &["install"],
    );
    assert_ne!(code, 0);
    assert!(
        err2.contains("registry: no package 'mathx'"),
        "ini error: {}",
        err2
    );

    // Neither set: the documented not-set error (which still names the
    // alias, the documented spelling).
    let empty_ini = write_in(&dir, "empty.ini", "");
    let (code, _, err3) = run_in(
        &app,
        &[("SPID3R_INI", empty_ini.as_str())],
        &["SPID3R_REGISTRY", "SPID3R_PACKAGES_REGISTRY"],
        &["install"],
    );
    assert_ne!(code, 0);
    assert!(
        err3.contains("SPID3R_REGISTRY is not set"),
        "not-set error: {}",
        err3
    );
}

/// `packages.allow_insecure_git = false` (default) refuses an http:// git
/// dependency with the same message the env-alias path always produced; the
/// opt-in (ini OR env alias) removes the refusal. The URL points at a
/// closed local port so no network is involved either way.
#[test]
fn packages_allow_insecure_git_gates_http_git_dependencies() {
    let dir = scratch_dir("pkg_insecure");
    let app = dir.join("app");
    std::fs::create_dir_all(app.join("src")).expect("mkdir app");
    write_in(
        &app,
        "spider.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nevil = { git = \"http://127.0.0.1:1/evil.git\" }\n",
    );
    write_in(&app, "src/main.matrix", "fn main() { return 0; }\n");

    // Default: refused, message unchanged from the env-var era.
    let (code, _, err) = run_in(
        &app,
        &[],
        &[
            "SPID3R_ALLOW_INSECURE_GIT",
            "SPID3R_PACKAGES_ALLOW_INSECURE_GIT",
        ],
        &["install"],
    );
    assert_ne!(code, 0);
    assert!(
        err.contains("plaintext transport has no integrity protection")
            && err.contains("SPID3R_ALLOW_INSECURE_GIT"),
        "refusal: {}",
        err
    );

    // ini opt-in: the refusal is gone (the clone fails for transport
    // reasons instead, at a closed port).
    let ini = write_in(
        &dir,
        "spid3r.ini",
        "[packages]\nallow_insecure_git = true\n",
    );
    let (code, _, err2) = run_in(&app, &[("SPID3R_INI", ini.as_str())], &[], &["install"]);
    assert_ne!(code, 0, "the clone must still fail (closed port)");
    assert!(
        !err2.contains("plaintext transport has no integrity protection"),
        "the ini opt-in must lift the refusal: {}",
        err2
    );

    // The generic env override keeps working.
    let empty_ini = write_in(&dir, "empty.ini", "");
    let (code, _, err3) = run_in(
        &app,
        &[
            ("SPID3R_INI", empty_ini.as_str()),
            ("SPID3R_PACKAGES_ALLOW_INSECURE_GIT", "true"),
        ],
        &[],
        &["install"],
    );
    assert_ne!(code, 0);
    assert!(
        !err3.contains("plaintext transport has no integrity protection"),
        "the env override must still lift the refusal: {}",
        err3
    );
}

// ----------------------------------------------------------------- surfaces

/// `spid3r info` carries all seven new keys.
#[test]
fn info_shows_the_new_keys() {
    let dir = scratch_dir("info");
    let empty_ini = write_in(&dir, "empty.ini", "");
    let (code, out, err) = run_with(&[("SPID3R_INI", empty_ini.as_str())], &[], &["info"]);
    assert_eq!(code, 0, "err={}", err);
    for key in [
        "security.basedir",
        "security.disable_natives",
        "modules.disable",
        "modules.assert_active",
        "packages.registry",
        "packages.allow_insecure_git",
        "packages.allow_local_git",
    ] {
        assert!(out.contains(key), "info must show {}: {}", key, out);
    }
}

/// `config init` writes the commented template with the [packages] section
/// and the new security/modules keys.
#[test]
fn config_init_template_carries_the_new_keys() {
    let dir = scratch_dir("init");
    let target = dir.join("out").join("spid3r.ini");
    let target_str = target.to_str().unwrap().to_string();
    let (code, out, err) = run_with(
        &[("SPID3R_INI", target_str.as_str())],
        &[],
        &["config", "init"],
    );
    assert_eq!(code, 0, "out={} err={}", out, err);
    let text = std::fs::read_to_string(&target).expect("template written");
    assert!(text.contains("[packages]"), "template: {}", text);
    assert!(text.contains("# basedir"), "template: {}", text);
    assert!(text.contains("# disable_natives"), "template: {}", text);
    assert!(text.contains("# disable ="), "template: {}", text);
    assert!(text.contains("assert_active = true"), "template: {}", text);
    assert!(
        text.contains("allow_insecure_git = false") && text.contains("allow_local_git = false"),
        "template: {}",
        text
    );
}

/// `config validate` rejects malformed values with per-key messages.
#[test]
fn config_validate_reports_per_key_problems() {
    let dir = scratch_dir("validate");
    let ini = write_in(
        &dir,
        "spid3r.ini",
        "[packages]\nallow_insecure_git = banana\n[modules]\nassert_active = maybe\n",
    );
    let (code, _, err) = run_with(
        &[("SPID3R_INI", ini.as_str())],
        &[],
        &["config", "validate"],
    );
    assert_ne!(code, 0);
    assert!(
        err.contains("packages.allow_insecure_git") && err.contains("expected a boolean"),
        "{}",
        err
    );
    assert!(
        err.contains("modules.assert_active") && err.contains("expected a boolean"),
        "{}",
        err
    );
}

/// `config validate` notes a security.basedir entry that does not exist on
/// disk (F-6): the entry matches nothing, so every path is denied — correct
/// fail-safe behavior, but an operator typo should be a note, not a support
/// ticket.
#[test]
fn config_validate_notes_missing_basedir_entries() {
    let dir = scratch_dir("validate_basedir");
    let ghost = dir.join("ghost");
    // Quoted: a bare ';' starts an INI comment, so list values with ';'
    // separators must be quoted in the file (',' needs no quotes).
    let ini = write_in(
        &dir,
        "spid3r.ini",
        &format!("[security]\nbasedir = \"{};{}\"\n", fwd(&dir), fwd(&ghost)),
    );
    let (code, _, err) = run_with(
        &[("SPID3R_INI", ini.as_str())],
        &[],
        &["config", "validate"],
    );
    assert_ne!(code, 0, "a missing basedir entry must be reported");
    assert!(
        err.contains("security.basedir") && err.contains("does not exist"),
        "{}",
        err
    );
    assert!(err.contains("all paths will be denied"), "{}", err);
    assert!(err.contains(&fwd(&ghost)), "{}", err);

    // The existing entry is not flagged, and with no other problems the
    // whole validate comes back clean.
    let ok_ini = write_in(
        &dir,
        "ok.ini",
        &format!("[security]\nbasedir = {}\n", fwd(&dir)),
    );
    let (code, out, err) = run_with(
        &[("SPID3R_INI", ok_ini.as_str())],
        &[],
        &["config", "validate"],
    );
    assert_eq!(code, 0, "an existing basedir must validate clean: {}", err);
    assert!(out.contains("ok"), "{}", out);
}

// -------------------------------------------------------------- differential

/// The phase-3 denials behave identically under both engines: same catchable
/// outcome, same message text. `disable_natives` takes precedence over the
/// basedir guard for the same native (the stub is installed over the guarded
/// wrapper), so the shared text is the configuration denial.
#[test]
fn denials_are_identical_across_engines() {
    let dir = scratch_dir("diff");
    let outside = dir.join("outside");
    std::fs::create_dir_all(&outside).expect("mkdir");
    let ini = write_in(
        &dir,
        "spid3r.ini",
        &format!(
            "[security]\nbasedir = {}\ndisable_natives = read_file\n[modules]\ndisable = json\n",
            fwd(&dir.join("bd"))
        ),
    );
    let script = write_in(
        &dir,
        "s.matrix",
        &subst(
            r#"fn main() {
    try { read_file("$OUT$/x.txt"); println("BAD"); } catch e { println("caught: " + e); }
    return 0;
}
"#,
            &[("$OUT$", &outside)],
        ),
    );
    let mut outputs: Vec<(i32, String)> = Vec::new();
    for engine in ["tree", "vm"] {
        let (code, out, err) =
            run_with_engine(engine, &[("SPID3R_INI", ini.as_str())], &["run", &script]);
        assert_eq!(code, 0, "[{engine}] err={}", err);
        assert!(!out.contains("BAD"), "[{engine}] {}", out);
        assert!(
            out.contains("disabled by configuration"),
            "[{engine}] {}",
            out
        );
        outputs.push((code, out));
    }
    assert_eq!(outputs[0], outputs[1], "engines must agree byte for byte");
}
