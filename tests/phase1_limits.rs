//! Phase-1 `spid3r.ini` limit keys, end-to-end through the real binary:
//!
//! * `runtime.wall_clock_limit_ms` — fatal on expiry, both engines;
//! * `runtime.memory_limit_mb` — catchable OOM, both engines;
//! * unlimited by default;
//! * `errors.display` / `debug.log_stdout` alias and precedence;
//! * `errors.log_file` dedicated error log;
//! * `runtime.timezone` default zone for the datetime natives.
//!
//! Every case runs in its own process (Command::new) so the env-var overrides
//! and the SPID3R_INI file are loaded by that process's config loader.
//!
//! Exit codes: 0 success, 1 program error, 2 usage error.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_spid3r")
}

/// A unique scratch directory per test: the tests run in parallel and must
/// not race on shared files.
fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r_phase1_{}", tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn write_in(dir: &std::path::Path, name: &str, src: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, src).expect("write");
    p.to_str().unwrap().to_string()
}

/// Run `spid3r` with `env` set for the child process. Pin a real, minimal
/// config file unless the caller supplies its own SPID3R_INI, so user-level
/// machine configuration can never affect these tests.
fn run_with(env: &[(&str, &str)], args: &[&str]) -> (i32, String, String) {
    static NEXT_PIN: AtomicUsize = AtomicUsize::new(0);
    let owns_pin = !env.iter().any(|(key, _)| *key == "SPID3R_INI");
    let pin_dir = if owns_pin {
        let tag = format!("run_{}", NEXT_PIN.fetch_add(1, Ordering::Relaxed));
        Some(scratch_dir(&tag))
    } else {
        None
    };
    if let Some(dir) = &pin_dir {
        std::fs::write(dir.join("spid3r.ini"), "; isolated test configuration\n")
            .expect("write isolated ini");
    }

    let mut cmd = Command::new(bin());
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    if let Some(dir) = &pin_dir {
        cmd.env("SPID3R_INI", dir.join("spid3r.ini"));
    }
    let out = cmd.output().expect("spawn spid3r");
    if let Some(dir) = &pin_dir {
        let _ = std::fs::remove_dir_all(dir);
    }
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn engine_args(engine: &str, args: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    if engine == "vm" {
        out.push("--engine".to_string());
        out.push("vm".to_string());
    }
    out
}

fn run_with_engine(engine: &str, env: &[(&str, &str)], args: &[&str]) -> (i32, String, String) {
    let argv: Vec<String> = engine_args(engine, args);
    let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    run_with(env, &refs)
}

// ------------------------------------------------- runtime.wall_clock_limit_ms

const SLEEPER: &str = r#"fn main() {
    while true {
        sleep_ms(50);
    }
    return 0;
}
"#;

/// (a) The wall clock is a fatal, uncatchable error on BOTH engines, via the
/// generic env override — and stdout stays identical across engines (the
/// differential contract).
#[test]
fn wall_clock_limit_is_fatal_on_both_engines() {
    let dir = scratch_dir("wall_env");
    let path = write_in(&dir, "sleeper.matrix", SLEEPER);
    for engine in ["tree", "vm"] {
        let (code, _out, err) = run_with_engine(
            engine,
            &[("SPID3R_RUNTIME_WALL_CLOCK_LIMIT_MS", "500")],
            &["run", &path],
        );
        assert_eq!(
            code, 1,
            "[{engine}] the wall clock is a fatal program error: {err}"
        );
        assert!(
            err.contains("wall-clock limit exceeded (500 ms)"),
            "[{engine}] expected the wall-clock diagnostic: {err}"
        );
        assert!(!err.contains("catch"), "[{engine}] {err}");
    }
}

/// (a) The same fatal via an ini file pointed at with SPID3R_INI.
#[test]
fn wall_clock_limit_comes_from_the_ini_file() {
    let dir = scratch_dir("wall_ini");
    let path = write_in(&dir, "sleeper.matrix", SLEEPER);
    let ini = write_in(&dir, "spid3r.ini", "[runtime]\nwall_clock_limit_ms = 400\n");
    let ini_ref = ini.as_str();
    for engine in ["tree", "vm"] {
        let (code, _out, err) =
            run_with_engine(engine, &[("SPID3R_INI", ini_ref)], &["run", &path]);
        assert_eq!(code, 1, "[{engine}] {err}");
        assert!(
            err.contains("wall-clock limit exceeded (400 ms)"),
            "[{engine}] {err}"
        );
    }
}

/// (a) The fatal bypasses `try`/`catch`, like budget exhaustion.
#[test]
fn wall_clock_limit_cannot_be_caught() {
    let dir = scratch_dir("wall_catch");
    let path = write_in(
        &dir,
        "sleeper_catch.matrix",
        r#"fn main() {
    try {
        while true {
            sleep_ms(50);
        }
    } catch e {
        println("swallowed");
        return 0;
    }
    return 1;
}
"#,
    );
    for engine in ["tree", "vm"] {
        let (code, out, err) = run_with_engine(
            engine,
            &[("SPID3R_RUNTIME_WALL_CLOCK_LIMIT_MS", "500")],
            &["run", &path],
        );
        assert_eq!(code, 1, "[{engine}] {err}");
        assert!(!out.contains("swallowed"), "[{engine}] {out}");
        assert!(
            err.contains("wall-clock limit exceeded"),
            "[{engine}] {err}"
        );
    }
}

/// (c) With the limit unset (0 = default), the same loops run to completion.
#[test]
fn default_limits_are_unlimited() {
    let dir = scratch_dir("unlimited");
    let path = write_in(
        &dir,
        "bounded.matrix",
        r#"fn main() {
    let l = [];
    let i = 0;
    while i < 20000 {
        l.push("payload");
        i = i + 1;
    }
    let j = 0;
    while j < 3 {
        sleep_ms(20);
        j = j + 1;
    }
    println("done");
    return 0;
}
"#,
    );
    for engine in ["tree", "vm"] {
        let (code, out, err) = run_with_engine(engine, &[], &["run", &path]);
        assert_eq!(code, 0, "[{engine}] {err}");
        assert!(out.contains("done"), "[{engine}] stdout: {out}");
    }
}

// -------------------------------------------------- runtime.memory_limit_mb

/// (b) A growing list trips the catchable OOM on BOTH engines, and the
/// program recovers (drops the container, continues, exits 0).
#[test]
fn memory_limit_is_catchable_on_both_engines() {
    let dir = scratch_dir("mem_catch");
    let payload = "A".repeat(192);
    let src = format!(
        r#"fn main() {{
    let l = [];
    try {{
        while true {{
            l.push("{payload}");
        }}
    }} catch e {{
        println("caught: " + string(e));
        l = null;
        return 0;
    }}
    return 1;
}}
"#
    );
    let path = write_in(&dir, "grower.matrix", &src);
    for engine in ["tree", "vm"] {
        let (code, out, err) = run_with_engine(
            engine,
            &[("SPID3R_RUNTIME_MEMORY_LIMIT_MB", "64")],
            &["run", &path],
        );
        assert_eq!(
            code, 0,
            "[{engine}] the OOM is catchable and the program continues: {err}"
        );
        assert!(
            out.contains("caught: out of memory: script exceeded memory_limit_mb (64)"),
            "[{engine}] stdout: {out}"
        );
    }
}

/// (b) The default (0) leaves the same growth loop unbounded.
#[test]
fn default_memory_limit_is_unlimited() {
    let dir = scratch_dir("mem_unlimited");
    let src = r#"fn main() {
    let l = [];
    let i = 0;
    while i < 10000 {
        l.push("payload");
        i = i + 1;
    }
    println("done");
    return 0;
}
"#;
    let path = write_in(&dir, "bounded_grower.matrix", src);
    for engine in ["tree", "vm"] {
        let (code, out, err) = run_with_engine(engine, &[], &["run", &path]);
        assert_eq!(code, 0, "[{engine}] {err}");
        assert!(out.contains("done"), "[{engine}] stdout: {out}");
    }
}

// ------------------------------------------- errors.display / debug.log_stdout

/// The log line `spid3r gui <missing>` writes (record_error -> "cli: …").
/// Mirrored to stderr exactly when the alias resolution says so.
#[test]
fn errors_display_mirrors_log_lines_and_alias_resolves() {
    let dir = scratch_dir("display");
    let log_file = dir.join("shared.log").to_string_lossy().to_string();
    let missing = dir.join("nope.matrix").to_string_lossy().to_string();

    // errors.display = true (canonical spelling): mirrored.
    let (code, _out, err) = run_with(
        &[
            ("SPID3R_ERRORS_DISPLAY", "true"),
            ("SPID3R_LOG_FILE", &log_file),
        ],
        &["gui", &missing],
    );
    assert_eq!(code, 2, "a usage error for a missing file");
    assert!(
        err.contains(" cli: "),
        "expected the mirrored log line: {err}"
    );

    // debug.log_stdout = true (alias): mirrored too.
    let (code, _out, err) = run_with(
        &[
            ("SPID3R_DEBUG_LOG_STDOUT", "true"),
            ("SPID3R_LOG_FILE", &log_file),
        ],
        &["gui", &missing],
    );
    assert_eq!(code, 2);
    assert!(err.contains(" cli: "), "alias must still work: {err}");

    // Both set: errors.display wins (explicit false beats the alias true).
    let (code, _out, err) = run_with(
        &[
            ("SPID3R_ERRORS_DISPLAY", "false"),
            ("SPID3R_DEBUG_LOG_STDOUT", "true"),
            ("SPID3R_LOG_FILE", &log_file),
        ],
        &["gui", &missing],
    );
    assert_eq!(code, 2);
    assert!(
        !err.contains(" cli: "),
        "explicit errors.display=false must win over the alias: {err}"
    );

    // Neither set: no mirror.
    let (code, _out, err) = run_with(&[("SPID3R_LOG_FILE", &log_file)], &["gui", &missing]);
    assert_eq!(code, 2);
    assert!(!err.contains(" cli: "), "default is no mirror: {err}");
}

/// errors.log_file: error-level diagnostics land in the dedicated file (and
/// keep flowing to the shared log).
#[test]
fn errors_log_file_receives_error_diagnostics() {
    let dir = scratch_dir("errlog");
    let shared = dir.join("shared.log");
    let errors = dir.join("errors.log");
    let missing = dir.join("nope.matrix").to_string_lossy().to_string();
    let (code, _out, err) = run_with(
        &[
            ("SPID3R_LOG_FILE", shared.to_str().unwrap()),
            ("SPID3R_ERRORS_LOG_FILE", errors.to_str().unwrap()),
            // Keep the mirror off so stderr stays clean for the assertion.
            ("SPID3R_ERRORS_DISPLAY", "false"),
        ],
        &["gui", &missing],
    );
    assert_eq!(code, 2, "{err}");
    let err_text = std::fs::read_to_string(&errors).unwrap_or_default();
    assert!(err_text.contains(" cli: "), "error log: {err_text}");
    let shared_text = std::fs::read_to_string(&shared).unwrap_or_default();
    assert!(
        shared_text.contains(" cli: "),
        "shared log keeps it too: {shared_text}"
    );
}

// ------------------------------------------------------------ runtime.timezone

/// (e) The configured default timezone localizes the datetime natives on
/// both engines: a fixed offset and an IANA name.
#[test]
fn timezone_default_applies_to_datetime_natives() {
    let dir = scratch_dir("tz");
    let src = r#"fn main() {
    let p = datetime_parts(0);
    println(string(p.hour));
    println(format_datetime(0, "%z %Z"));
    // 2025-01-15T12:00:00Z: 13:00 local in Europe/Berlin (CET, +01:00).
    let q = datetime_parts(1736942400);
    println(string(q.hour) + " " + string(q.day));
    return 0;
}
"#;
    let path = write_in(&dir, "tzcheck.matrix", src);
    for (env, hour0, zone_line, hour) in [
        ("+06:00", "6", "+0600 +06:00", "18 15"),
        ("Europe/Berlin", "1", "+0100 CET", "13 15"),
    ] {
        for engine in ["tree", "vm"] {
            let (code, out, err) =
                run_with_engine(engine, &[("SPID3R_RUNTIME_TIMEZONE", env)], &["run", &path]);
            assert_eq!(code, 0, "[{engine} {env}] {err}");
            let lines: Vec<&str> = out.lines().collect();
            assert_eq!(
                lines.first().copied(),
                Some(hour0),
                "[{engine} {env}] datetime_parts(0).hour: {out}"
            );
            assert_eq!(
                lines.get(1).copied(),
                Some(zone_line),
                "[{engine} {env}] format_datetime %z %Z: {out}"
            );
            assert_eq!(
                lines.get(2).copied(),
                Some(hour),
                "[{engine} {env}] Berlin local parts: {out}"
            );
        }
    }
}

/// A rejected timezone value falls back to UTC with a note (never aborts),
/// and the note is surfaced once at run start through the shared diagnostic
/// log — not only by `spid3r config validate`.
#[test]
fn unknown_timezone_is_ignored_with_a_logged_note() {
    let dir = scratch_dir("tz_bad");
    let path = write_in(
        &dir,
        "tzcheck2.matrix",
        "fn main() {\n    let p = datetime_parts(0);\n    println(string(p.hour));\n    return 0;\n}\n",
    );
    let log_file = dir.join("shared.log");
    let log_ref = log_file.to_string_lossy().to_string();
    for engine in ["tree", "vm"] {
        let (code, out, err) = run_with_engine(
            engine,
            &[
                ("SPID3R_RUNTIME_TIMEZONE", "Mars/Olympus"),
                ("SPID3R_LOG_FILE", log_ref.as_str()),
            ],
            &["run", &path],
        );
        assert_eq!(code, 0, "[{engine}] {err}");
        assert!(out.contains("0"), "[{engine}] UTC fallback: {out}");
        let text = std::fs::read_to_string(&log_file).unwrap_or_default();
        assert!(
            text.contains("config: SPID3R_RUNTIME_TIMEZONE:"),
            "[{engine}] the invalid value must be noted in the shared log: {text}"
        );
    }
}

// ------------------------------------------------------- differential contract

/// Tree and VM agree on stdout for the limited runs (the wall-clock fatal
/// prints nothing before it stops; the caught OOM prints the same line).
#[test]
fn tree_and_vm_produce_identical_stdout() {
    let dir = scratch_dir("differential");

    let sleeper = write_in(&dir, "diff_sleeper.matrix", SLEEPER);
    let mem_catch = {
        let payload = "A".repeat(192);
        let src = format!(
            r#"fn main() {{
    let l = [];
    try {{
        while true {{
            l.push("{payload}");
        }}
    }} catch e {{
        println("caught");
        l = null;
        return 0;
    }}
    return 1;
}}
"#
        );
        write_in(&dir, "diff_grower.matrix", &src)
    };

    for (env, path) in [
        (
            vec![("SPID3R_RUNTIME_WALL_CLOCK_LIMIT_MS", "500")],
            sleeper.as_str(),
        ),
        (
            vec![("SPID3R_RUNTIME_MEMORY_LIMIT_MB", "64")],
            mem_catch.as_str(),
        ),
    ] {
        let env_refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, *v)).collect();
        let mut outs = Vec::new();
        let mut codes = Vec::new();
        for engine in ["tree", "vm"] {
            let (code, out, err) = run_with_engine(engine, &env_refs, &["run", path]);
            codes.push(code);
            outs.push(out);
            assert!(!err.contains("panicked"), "[{engine}] {err}");
        }
        assert_eq!(codes[0], codes[1], "exit codes must match for {path}");
        assert_eq!(outs[0], outs[1], "stdout must match for {path}");
    }
}
