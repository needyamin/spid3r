//! Filesystem/path completeness pack (2026-10-29): `list_dir`, the stat
//! family (`is_dir`, `is_file`, `file_size`, `file_mtime`), process context
//! (`cwd`, `chdir`, `env_set`), and the lexical path helpers (`path_join`,
//! `basename`, `dirname`, `path_ext`).
//!
//! Every program runs on BOTH engines (tree + VM) and must agree — the same
//! parity contract as `tests/io_additions.rs`. Paths are embedded with `/`
//! separators: the natives accept them on Windows too, which is also what
//! the helpers normalize to.

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::modules;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::vm::Vm;
use std::sync::Arc;

fn run_tree(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("fs_pack.matrix"))?;
    semantic::check_file(&program, true, "fs_pack.matrix")?;
    let interp = Interp::new(Arc::from("fs_pack.matrix"), modules::default_module_dirs());
    interp.run_program(&program)
}

fn run_vm(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("fs_pack.matrix"))?;
    semantic::check_file(&program, true, "fs_pack.matrix")?;
    let dirs = modules::default_module_dirs();
    let mut vm = Vm::new(Arc::from("fs_pack.matrix"), dirs);
    vm.run_program(&program)
}

fn both(src: &str) -> i32 {
    let tree = run_tree(src).unwrap_or_else(|d| panic!("tree engine failed: {}", d.message));
    let vm = run_vm(src).unwrap_or_else(|d| panic!("vm engine failed: {}", d.message));
    assert_eq!(tree, vm, "engines disagree on exit code");
    tree
}

/// Embed the path with `/` separators so a Windows `\` never becomes a
/// string escape inside the generated Spid3r source.
fn slashed(p: &std::path::Path) -> String {
    p.display().to_string().replace('\\', "/")
}

fn temp_root(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("spid3r_fspack_{}_{}", tag, std::process::id()))
}

#[test]
fn list_dir_is_sorted_and_reports_errors() {
    let dir = temp_root("list");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("b.txt"), "hello").unwrap();
    std::fs::write(dir.join("a.txt"), "xy").unwrap();

    let src = format!(
        r#"fn main() {{
            let r = list_dir("{d}");
            assert_eq(is_ok(r), true);
            let names = r.unwrap();
            assert_eq(len(names), 3);
            assert_eq(names[0], "a.txt");
            assert_eq(names[1], "b.txt");
            assert_eq(names[2], "sub");
            // A missing path and a file path are errors, never throws.
            assert_eq(is_err(list_dir("{d}/nope")), true);
            assert_eq(is_err(list_dir("{d}/a.txt")), true);
            return 0;
        }}"#,
        d = slashed(&dir)
    );
    assert_eq!(both(&src), 0);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn stat_family_reports_type_size_and_mtime() {
    let dir = temp_root("stat");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("a.txt"), "xy").unwrap();
    std::fs::write(dir.join("b.txt"), "hello").unwrap();

    let src = format!(
        r#"fn main() {{
            assert_eq(is_dir("{d}"), true);
            assert_eq(is_dir("{d}/a.txt"), false);
            assert_eq(is_dir("{d}/nope"), false);
            assert_eq(is_file("{d}/a.txt"), true);
            assert_eq(is_file("{d}/sub"), false);
            assert_eq(is_file("{d}/nope"), false);
            assert_eq(file_size("{d}/a.txt").unwrap(), 2);
            assert_eq(file_size("{d}/b.txt").unwrap(), 5);
            // A directory has no file size; a missing path is an error too.
            assert_eq(is_err(file_size("{d}/sub")), true);
            assert_eq(is_err(file_size("{d}/nope")), true);
            let mt = file_mtime("{d}/a.txt");
            assert_eq(is_ok(mt), true);
            assert_eq(mt.unwrap() > 0.0, true);
            assert_eq(is_err(file_mtime("{d}/nope")), true);
            return 0;
        }}"#,
        d = slashed(&dir)
    );
    assert_eq!(both(&src), 0);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn path_helpers_are_pure_and_cross_platform() {
    // No filesystem access at all: the helpers are lexical, and the expected
    // strings are identical on every OS (that is the point of treating both
    // separators uniformly).
    let src = r#"fn main() {
        assert_eq(path_join("a", "b", "c.txt"), "a/b/c.txt");
        assert_eq(path_join("a/", "b"), "a/b");
        assert_eq(path_join("base", "/abs"), "/abs");
        assert_eq(path_join("", ""), "");
        assert_eq(path_join("a", "", "b"), "a/b");
        assert_eq(path_join("C:\\x", "y"), "C:/x/y");
        assert_eq(basename("/usr/local/bin/spid3r"), "spid3r");
        assert_eq(basename("a/b/"), "b");
        assert_eq(basename("/"), "/");
        assert_eq(basename(""), ".");
        assert_eq(basename("plain.txt"), "plain.txt");
        assert_eq(dirname("/usr/local/bin/spid3r"), "/usr/local/bin");
        assert_eq(dirname("c"), ".");
        assert_eq(dirname("/c"), "/");
        assert_eq(dirname("/"), "/");
        assert_eq(dirname("a/b/"), "a");
        assert_eq(dirname("C:\\dir\\file.txt"), "C:/dir");
        assert_eq(path_ext("archive.tar.gz"), "gz");
        assert_eq(path_ext("noext"), "");
        assert_eq(path_ext(".gitignore"), "");
        assert_eq(path_ext("/a.b/c"), "");
        assert_eq(path_ext("a.txt"), "txt");
        return 0;
    }"#;
    assert_eq!(both(src), 0);
}

#[test]
fn cwd_chdir_roundtrip() {
    let dir = temp_root("cwd");
    std::fs::create_dir_all(&dir).unwrap();
    let tag = format!("spid3r_fspack_cwd_{}", std::process::id());

    // chdir is process-global, so this test owns the CWD exclusively while it
    // runs and restores it before returning. The absolute paths used by the
    // other tests in this file stay correct regardless.
    let src = format!(
        r#"fn main() {{
            let before = cwd();
            assert_eq(is_ok(before), true);
            let r = chdir("{d}");
            assert_eq(is_ok(r), true);
            let after = cwd().unwrap();
            assert_eq(basename(after), "{tag}");
            // Restore the original directory; the round trip must succeed.
            let back = chdir(before.unwrap());
            assert_eq(is_ok(back), true);
            assert_eq(cwd().unwrap(), before.unwrap());
            // chdir to a missing directory is a catchable error result.
            assert_eq(is_err(chdir("{d}/nope")), true);
            return 0;
        }}"#,
        d = slashed(&dir),
        tag = tag
    );
    assert_eq!(both(&src), 0);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn env_set_is_visible_to_env() {
    let src = r#"fn main() {
        let r = env_set("SPID3R_FSPACK_VAR", "hello42");
        assert_eq(is_ok(r), true);
        assert_eq(string(env("SPID3R_FSPACK_VAR")), "some(hello42)");
        // Invalid names are refused as results, not thrown.
        assert_eq(is_err(env_set("A=B", "x")), true);
        assert_eq(is_err(env_set("", "x")), true);
        return 0;
    }"#;
    assert_eq!(both(src), 0);
}

#[test]
fn environment_capability_denies_env_set() {
    use spid3r_lang::embed::{Capability, Engine};
    let engine = Engine::new().deny(Capability::Environment);
    let out = engine
        .run(
            "deny_env",
            r#"fn main() {
                try { env_set("SPID3R_CAP_TEST", "1"); return 1; } catch e { println("denied"); }
                return 0;
            }"#,
        )
        .expect("script runs");
    assert_eq!(out.exit_code, 0);
    assert_eq!(out.output, "denied\n");
}

#[test]
fn filesystem_capability_denies_new_fs_natives() {
    use spid3r_lang::embed::{Capability, Engine};
    let engine = Engine::new().deny(Capability::FileSystem);
    let out = engine
        .run(
            "deny_fs",
            r#"fn main() {
                try { list_dir("."); return 1; } catch e { println("no list"); }
                try { file_size("Cargo.toml"); return 2; } catch e { println("no size"); }
                return 0;
            }"#,
        )
        .expect("script runs");
    assert_eq!(out.exit_code, 0);
    assert_eq!(out.output, "no list\nno size\n");
}
