//! `spidr-<name>` external subcommand dispatch.
//!
//! An unknown subcommand is looked up on `PATH` as `spidr-<name>` and executed
//! with the remaining arguments; its exit code becomes spid3r's. The lookup is
//! also a security boundary: the executable path is built by concatenation, so
//! a name carrying a path separator or starting with `-` must never be
//! dispatched, and batch shims must never be dispatched on Windows (cmd.exe
//! re-parses the command line). All of it is pinned here.
//!
//! The shim is a copy of the `spidr` binary itself, which makes the tests
//! portable and keeps the assertions behavioural: `spidr echo version` must
//! print the version, `spidr echo --bogus` must exit 2, and so on.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Path to the `spidr` binary under test.
///
/// Resolved at run time rather than through `env!("CARGO_BIN_EXE_...")`: the
/// macro is only defined when cargo builds the binary alongside this exact
/// test target, which is not guaranteed for a freshly added target. Cargo
/// always writes the binary into the profile directory that contains the test
/// executable's `deps/`, so that is the reliable location.
fn bin() -> PathBuf {
    for key in ["CARGO_BIN_EXE_spid3r", "CARGO_BIN_EXE_spidr"] {
        if let Ok(p) = std::env::var(key) {
            let p = PathBuf::from(p);
            if p.is_file() {
                return p;
            }
        }
    }
    let exe = std::env::current_exe().expect("test executable path");
    let profile_dir = exe
        .parent()
        .and_then(|deps| deps.parent())
        .expect("profile directory");
    for stem in ["spid3r", "spidr"] {
        let cand = profile_dir.join(format!("{}{}", stem, std::env::consts::EXE_SUFFIX));
        if cand.is_file() {
            return cand;
        }
    }
    panic!("spid3r binary not found next to {}", profile_dir.display());
}

/// A private shim directory containing an executable `spidr-echo` — a copy of
/// the `spid3r` binary, so invoking it runs a real Spid3r command.
fn shim_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spidr_dispatch_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir shim");
    let shim = dir.join(format!("spidr-echo{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(bin(), &shim).expect("copy shim");
    dir
}

/// A directory with a space in its name holding a program that prints `RAN`.
fn program_in_a_spaced_dir(tag: &str) -> (PathBuf, String) {
    let dir = std::env::temp_dir().join(format!(
        "spidr dispatch spaced {} {}",
        tag,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let prog = dir.join("prog.matrix");
    std::fs::write(
        &prog,
        "fn main() {\n    println(\"RAN\");\n    return 0;\n}\n",
    )
    .expect("write");
    (dir, prog.to_string_lossy().to_string())
}

/// `PATH` with `dir` prepended.
fn path_with(dir: &Path) -> std::ffi::OsString {
    let mut parts = vec![dir.to_path_buf()];
    if let Some(existing) = std::env::var_os("PATH") {
        parts.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(parts).expect("join PATH")
}

fn run(args: &[&str], path: &Path) -> (i32, String, String) {
    let o = Command::new(bin())
        .args(args)
        .env("PATH", path_with(path))
        .output()
        .expect("spawn spidr");
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

/// The whole point: an unknown subcommand that names an installed tool runs
/// it, and the tool's exit code becomes spidr's. 0, 1 and 2 all keep their
/// documented meaning.
#[test]
fn an_external_tool_is_dispatched_and_its_exit_code_propagates() {
    let dir = shim_dir("found");

    // 0 — the shim runs `spidr version`.
    let (code, out, err) = run(&["echo", "version"], &dir);
    assert_eq!(code, 0, "success not propagated (stderr: {err})");
    assert!(
        out.contains("1.0.0"),
        "the tool did not run with its argument: {out:?}"
    );

    // 2 — the shim runs `spidr --bogus`, a usage error.
    let (code, _, err) = run(&["echo", "--bogus"], &dir);
    assert_eq!(code, 2, "usage error not propagated: {err}");

    // 1 — the shim runs a program that throws, an uncaught user error.
    let bad = dir.join("bad.matrix");
    std::fs::write(&bad, "fn main() {\n    throw \"boom\";\n}\n").expect("write bad");
    let (code, _, err) = run(&["echo", "run", bad.to_str().unwrap()], &dir);
    assert_eq!(code, 1, "user error not propagated: {err}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Arguments are forwarded verbatim — including a path containing spaces,
/// which would otherwise be split and reported as "file not found".
#[test]
fn arguments_reach_the_tool_intact() {
    let dir = shim_dir("args");
    let (prog_dir, prog) = program_in_a_spaced_dir("args");
    let (code, out, err) = run(&["echo", "run", &prog], &dir);
    assert_eq!(code, 0, "spaced path was not forwarded intact: {err}");
    assert!(out.contains("RAN"), "stdout: {out:?}");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&prog_dir);
}

/// A `-`-prefixed token is an option, not a subcommand. Dispatching it would
/// let `spidr --bogus` execute a file named `spidr---bogus`, which is a trap.
///
/// The decoy makes this test **load-bearing**: if the `-` clause of
/// `is_dispatchable_name` were removed, `spidr --bogus version` would run the
/// decoy (a copy of `spidr`) with `version`, print the version and exit 0.
/// Without the decoy the test would pass either way, because a missing tool
/// produces the same usage error as a rejected name.
#[test]
fn option_like_tokens_are_never_dispatched() {
    let dir = shim_dir("options");
    let decoy = dir.join(format!("spidr---bogus{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(bin(), &decoy).expect("copy decoy");

    let (code, out, err) = run(&["--bogus", "version"], &dir);
    assert_eq!(
        code, 2,
        "expected a usage error, got {code} (stdout: {out:?})"
    );
    assert!(
        err.contains("unknown command"),
        "expected the unknown-command error: {err}"
    );
    assert!(
        !out.contains("1.0.0"),
        "the decoy ran: an option-like token was dispatched"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Path separators (and drive/UNC forms) must be rejected before any lookup:
/// `spidr ../../bin/sh` would otherwise run an arbitrary binary.
///
/// The `spidr-a/b` decoy makes this load-bearing: without the separator
/// clause, `spidr a/b version` would resolve to `<shim>/spidr-a/b` and run it.
#[test]
fn names_with_path_separators_are_rejected() {
    let dir = shim_dir("separators");
    let nested = dir.join("spidr-a");
    std::fs::create_dir_all(&nested).expect("mkdir nested");
    for name in ["b", "b.exe"] {
        std::fs::copy(bin(), nested.join(name)).expect("copy decoy");
    }

    for name in ["../echo", "a/b", "..\\echo", "C:/x", "..", "."] {
        let (code, _, err) = run(&[name], &dir);
        assert_eq!(code, 2, "name {name:?} must be a usage error");
        assert!(
            err.contains("unknown command"),
            "name {name:?} must not be dispatched: {err}"
        );
    }
    // The decoy itself must be reachable only if the guard is gone.
    let (code, out, _) = run(&["a/b", "version"], &dir);
    assert_eq!(code, 2, "a separator name was dispatched (stdout: {out:?})");
    assert!(
        !out.contains("1.0.0"),
        "the decoy ran: separator not rejected"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// With no matching tool the ordinary usage error is unchanged — the dispatch
/// attempt must be invisible.
#[test]
fn an_unknown_command_without_a_tool_is_still_a_usage_error() {
    let dir = shim_dir("absent");
    let (code, _, err) = run(&["definitely-not-installed-xyz"], &dir);
    assert_eq!(code, 2);
    assert!(err.contains("unknown command"));
    assert!(err.contains("definitely-not-installed-xyz"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A built-in command always wins: `spidr version` must not look for
/// `spidr-version` even when one is on PATH.
#[test]
fn built_in_commands_are_not_shadowed_by_external_tools() {
    let dir = shim_dir("shadow");
    // A `spidr-version` shim. If it were dispatched, `spidr version` would run
    // a bare `spidr` (no subcommand), which prints help to stderr and exits 2.
    let shim = dir.join(format!("spidr-version{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(bin(), &shim).expect("copy shim");
    let (code, out, _) = run(&["version"], &dir);
    assert_eq!(code, 0, "the built-in must not be shadowed");
    assert!(out.contains("1.0.0"), "version output: {out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Windows batch shims are deliberately **not** dispatched: `cmd.exe`
/// re-parses the command line, so one argument containing `&` runs a second
/// command. This pins the exclusion — a `.bat` on PATH must be ignored even
/// though it is "found", and the marker it would write must never appear.
#[cfg(windows)]
#[test]
fn batch_shims_are_not_dispatched() {
    let dir = shim_dir("batch");
    let marker = dir.join("pwned.txt");
    std::fs::write(
        dir.join("spidr-batchy.bat"),
        format!(
            "@echo off\r\n> \"{}\" echo pwned\r\nexit /b 0\r\n",
            marker.display()
        ),
    )
    .expect("write bat");

    let (code, _, err) = run(&["batchy", "a&b"], &dir);
    assert_eq!(code, 2, "a batch shim must not be dispatched: {err}");
    assert!(err.contains("unknown command"), "{err}");
    assert!(
        !marker.exists(),
        "the batch shim ran — command injection is live"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
