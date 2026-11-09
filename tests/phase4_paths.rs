//! Phase-4 `spid3r.ini` path keys, end-to-end through the real binary:
//!
//! * `paths.cache_dir` — cache home for the extracted bundle runtime
//!   (canonical spelling of the legacy `SPID3R_CACHE_DIR`);
//! * `paths.state_dir` — per-user state home: agent store, audit log, pid
//!   file (canonical spelling of the legacy `SPID3R_STATE_DIR`);
//! * `paths.sqlite_path` — first candidate for the SQLite shared library
//!   (canonical spelling of the legacy `SPID3R_SQLITE_PATH`);
//! * `cache.bytecode_dir` — bytecode cache for `spid3r run`: `.matrixc`
//!   containers keyed by the source's SHA-256 (the OPcache analog).
//!
//! Precedence per key: process env (legacy spelling AND the generic
//! `SPID3R_PATHS_*` spelling) > INI > platform default. Empty = platform
//! default. All four are frozen: a remote peer that could relocate the
//! host's write targets could poison data or plant a library, so the
//! schema-derived frozen pin in `src/config/schema.rs` covers them (unit
//! level) and the local `config set` surface keeps working (here).
//!
//! Every case runs in its own process (Command::new) so the SPID3R_INI file
//! and env overrides are loaded by that process's config loader. Exit
//! codes: 0 success, 1 program error, 2 usage error.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_spid3r")
}

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r_phase4_{}", tag));
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
/// into the path assertions).
fn run_in(
    cwd: &Path,
    env: &[(&str, &str)],
    remove: &[&str],
    args: &[&str],
) -> (i32, String, String) {
    let mut cmd = Command::new(bin());
    cmd.args(args).current_dir(cwd);
    // Remove first, then set: an explicitly passed variable must survive
    // its own appearance in `remove` (which only exists to strip inherited
    // machine-level SPID3R_* values).
    for k in remove {
        cmd.env_remove(k);
    }
    for (k, v) in env {
        cmd.env(k, v);
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

/// Forward slashes are used deliberately: backslashes inside Spid3r string
/// literals are escape sequences, and INI values go through the loader's
/// `\n`/`\t` unescaping as well. Every Windows path API the runtime touches
/// accepts forward slashes.
fn fwd(p: &Path) -> String {
    p.to_str().unwrap().replace('\\', "/")
}

/// The platform default state home, computed the same way `config::state_dir`
/// does. Only used to assert the *absence* of relocation artifacts there, so
/// a mirror drift degrades into a weaker test, never a false failure.
fn default_state_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA")
            .map(|b| PathBuf::from(&b).join("Spid3r").join("state"))
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .map(|b| PathBuf::from(&b).join("Spid3r").join("state"))
            });
    }
    if let Some(x) = std::env::var_os("XDG_STATE_HOME") {
        if !x.is_empty() {
            return Some(PathBuf::from(x).join("spid3r"));
        }
    }
    std::env::var_os("HOME").map(|h| {
        PathBuf::from(&h)
            .join(".local")
            .join("state")
            .join("spid3r")
    })
}

/// The value of an `spid3r info` row by label (text render: rows are
/// `  <label padded>  <value>`).
fn info_row(out: &str, label: &str) -> String {
    for line in out.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix(label) {
            let v = rest.trim_start().trim_start_matches('=').trim();
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    String::new()
}

const PATH_ENV_VARS: &[&str] = &[
    "SPID3R_CACHE_DIR",
    "SPID3R_STATE_DIR",
    "SPID3R_SQLITE_PATH",
    "SPID3R_PATHS_CACHE_DIR",
    "SPID3R_PATHS_STATE_DIR",
    "SPID3R_PATHS_SQLITE_PATH",
];

// ------------------------------------------------------------- precedence --

/// Per key: INI set -> INI location used; env (legacy spelling) set -> env
/// location used; BOTH set -> env wins; generic `SPID3R_PATHS_*` spelling
/// beats the INI too. Observable through the `spid3r info` rows that the
/// consumers actually resolve through (`State directory`, `Runtime cache`,
/// `SQLite library`).
#[test]
fn precedence_matrix_for_path_keys() {
    let dir = scratch_dir("precedence");
    let ini_cache = dir.join("ini_cache");
    let ini_state = dir.join("ini_state");
    let ini_sqlite = dir.join("libsqlite3.so");
    let env_cache = dir.join("env_cache");
    let env_state = dir.join("env_state");
    let env_sqlite = dir.join("env_sqlite.so");
    let ini = write_in(
        &dir,
        "spid3r.ini",
        &format!(
            "[paths]\ncache_dir = {}\nstate_dir = {}\nsqlite_path = {}\n",
            fwd(&ini_cache),
            fwd(&ini_state),
            fwd(&ini_sqlite)
        ),
    );

    // INI only: all three rows land on the INI paths.
    let (code, out, err) = run_with(&[("SPID3R_INI", ini.as_str())], PATH_ENV_VARS, &["info"]);
    assert_eq!(code, 0, "err={}", err);
    assert_eq!(info_row(&out, "State directory"), fwd(&ini_state));
    assert_eq!(
        info_row(&out, "Runtime cache"),
        PathBuf::from(fwd(&ini_cache))
            .join("runtime")
            .display()
            .to_string()
    );
    assert_eq!(info_row(&out, "SQLite library"), fwd(&ini_sqlite));

    // Legacy env beats the INI, per key, independently.
    let (code, out, err) = run_with(
        &[
            ("SPID3R_INI", ini.as_str()),
            ("SPID3R_STATE_DIR", fwd(&env_state).as_str()),
        ],
        PATH_ENV_VARS,
        &["info"],
    );
    assert_eq!(code, 0, "err={}", err);
    assert_eq!(info_row(&out, "State directory"), fwd(&env_state));
    assert_eq!(
        info_row(&out, "Runtime cache"),
        PathBuf::from(fwd(&ini_cache))
            .join("runtime")
            .display()
            .to_string()
    );

    let (code, out, err) = run_with(
        &[
            ("SPID3R_INI", ini.as_str()),
            ("SPID3R_CACHE_DIR", fwd(&env_cache).as_str()),
        ],
        PATH_ENV_VARS,
        &["info"],
    );
    assert_eq!(code, 0, "err={}", err);
    assert_eq!(info_row(&out, "State directory"), fwd(&ini_state));
    assert_eq!(
        info_row(&out, "Runtime cache"),
        PathBuf::from(fwd(&env_cache))
            .join("runtime")
            .display()
            .to_string()
    );

    let (code, out, err) = run_with(
        &[
            ("SPID3R_INI", ini.as_str()),
            ("SPID3R_SQLITE_PATH", fwd(&env_sqlite).as_str()),
        ],
        PATH_ENV_VARS,
        &["info"],
    );
    assert_eq!(code, 0, "err={}", err);
    assert_eq!(info_row(&out, "SQLite library"), fwd(&env_sqlite));

    // The generic SPID3R_PATHS_* spelling beats the INI as well.
    let (code, out, err) = run_with(
        &[
            ("SPID3R_INI", ini.as_str()),
            ("SPID3R_PATHS_STATE_DIR", fwd(&env_state).as_str()),
        ],
        PATH_ENV_VARS,
        &["info"],
    );
    assert_eq!(code, 0, "err={}", err);
    assert_eq!(info_row(&out, "State directory"), fwd(&env_state));
}

/// Unset everywhere: the rows show the platform defaults (mirrored in
/// `default_state_dir`), never a stray configured path.
#[test]
fn unset_keys_fall_back_to_platform_defaults() {
    let dir = scratch_dir("defaults");
    let empty_ini = write_in(&dir, "empty.ini", "");
    let (code, out, err) = run_with(
        &[("SPID3R_INI", empty_ini.as_str())],
        PATH_ENV_VARS,
        &["info"],
    );
    assert_eq!(code, 0, "err={}", err);
    let state = info_row(&out, "State directory");
    assert!(
        !state.contains("phase4"),
        "no configured path may leak into the default state row: {}",
        state
    );
    if let Some(default) = default_state_dir() {
        assert_eq!(
            state,
            default.display().to_string(),
            "state row must match the platform default"
        );
    }
    // Runtime cache row: the platform default appends `runtime` under the
    // cache home; an override appends it under the override. Both end in
    // `runtime` — assert the row is the suffix behavior, not a leak.
    let cache = info_row(&out, "Runtime cache");
    assert!(cache.ends_with("runtime"), "cache row: {}", cache);
    assert!(
        !cache.contains("phase4"),
        "cache row must be the default: {}",
        cache
    );
    assert_eq!(info_row(&out, "SQLite library"), "(default search)");
}

/// Alias equivalence: the legacy env spelling and the canonical ini key
/// produce identical resolution; `config get` sees the env value through
/// the alias too.
#[test]
fn legacy_env_and_ini_alias_are_equivalent() {
    let dir = scratch_dir("alias");
    let empty_ini = write_in(&dir, "empty.ini", "");
    let state = dir.join("state_via_env");
    let ini = write_in(
        &dir,
        "spid3r.ini",
        &format!("[paths]\nstate_dir = {}\n", fwd(&state)),
    );

    // env-only and ini-only must resolve to the same row.
    let (_, out_env, err) = run_with(
        &[
            ("SPID3R_INI", empty_ini.as_str()),
            ("SPID3R_STATE_DIR", fwd(&state).as_str()),
        ],
        PATH_ENV_VARS,
        &["info"],
    );
    assert_eq!(
        info_row(&out_env, "State directory"),
        fwd(&state),
        "err={}",
        err
    );
    let (_, out_ini, err) = run_with(&[("SPID3R_INI", ini.as_str())], PATH_ENV_VARS, &["info"]);
    assert_eq!(
        info_row(&out_ini, "State directory"),
        fwd(&state),
        "err={}",
        err
    );
    let row_env = info_row(&out_env, "State directory");
    let row_ini = info_row(&out_ini, "State directory");
    assert_eq!(row_env, row_ini);

    // The alias flows into the config layer: `config get` shows the value
    // whether it came from env or ini.
    let (code, out, err) = run_with(
        &[
            ("SPID3R_INI", empty_ini.as_str()),
            ("SPID3R_STATE_DIR", fwd(&state).as_str()),
        ],
        PATH_ENV_VARS,
        &["config", "get", "paths.state_dir"],
    );
    assert_eq!(code, 0, "err={}", err);
    assert_eq!(out.trim(), fwd(&state));
}

// ------------------------------------------------------------- relocation --

/// One ini relocates all the path keys; a state artifact (`agent pair`
/// writes devices.json) lands under the INI root and nowhere else.
#[test]
fn relocation_puts_artifacts_under_the_ini_roots() {
    let dir = scratch_dir("relocate");
    let state = dir.join("state");
    let ini = write_in(
        &dir,
        "spid3r.ini",
        &format!(
            "[paths]\nstate_dir = {}\ncache_dir = {}\n[cache]\nbytecode_dir = {}\n",
            fwd(&state),
            fwd(&dir.join("cache")),
            fwd(&dir.join("bytecode")),
        ),
    );
    let (code, out, err) = run_in(
        &dir,
        &[("SPID3R_INI", ini.as_str())],
        PATH_ENV_VARS,
        &["agent", "pair", "--role", "viewer", "--name", "reloc"],
    );
    assert_eq!(code, 0, "out={} err={}", out, err);

    // The artifact is under the INI root, and it carries the fresh pairing
    // code that was printed.
    let devices = state.join("devices.json");
    assert!(
        devices.is_file(),
        "devices.json must be under the INI state root"
    );
    let text = std::fs::read_to_string(&devices).expect("read devices.json");
    let code_line = out
        .lines()
        .find(|l| l.contains("pairing code"))
        .expect("pairing code printed");
    let pair_code = code_line
        .rsplit(' ')
        .next()
        .expect("code token")
        .trim()
        .to_string();
    assert!(
        text.contains(&pair_code),
        "fresh pairing code must be in the relocated store"
    );

    // Nothing landed in the platform default (its devices.json, if any,
    // cannot contain a code that was minted for the relocated store).
    if let Some(default) = default_state_dir() {
        if let Ok(t) = std::fs::read_to_string(default.join("devices.json")) {
            assert!(
                !t.contains(&pair_code),
                "the pairing must not leak into the platform default state home"
            );
        }
    }
}

// ----------------------------------------------------------------- frozen --

/// The local `config set` surface still accepts the frozen path keys
/// (frozen blocks the REMOTE agent config.set, which is pinned by the
/// schema unit test); the value round-trips through config.get.
#[test]
fn local_config_set_accepts_path_keys() {
    let dir = scratch_dir("frozen_local");
    // Pin to a real isolated ini so this test exercises the file layer,
    // independent of any machine-level config candidates.
    let target = write_in(&dir, "spid3r.ini", "");
    let state = dir.join("state");
    let (code, out, err) = run_with(
        &[
            ("SPID3R_INI", target.as_str()),
            ("SPID3R_CONFIG_DIR", dir.to_str().unwrap()),
            ("SPID3R_STATE_DIR", fwd(&state).as_str()),
        ],
        PATH_ENV_VARS,
        &["config", "set", "paths.state_dir", &fwd(&state)],
    );
    assert_eq!(code, 0, "out={} err={}", out, err);
    let (code, out, err) = run_with(
        &[
            ("SPID3R_INI", target.as_str()),
            ("SPID3R_CONFIG_DIR", dir.to_str().unwrap()),
        ],
        PATH_ENV_VARS,
        &["config", "get", "paths.state_dir"],
    );
    assert_eq!(code, 0, "err={}", err);
    assert_eq!(out.trim(), fwd(&state));
}

// --------------------------------------------------------------- surfaces --

/// `spid3r info` and the config surfaces carry the new keys.
#[test]
fn surfaces_show_the_new_keys() {
    let dir = scratch_dir("surfaces");
    let empty_ini = write_in(&dir, "empty.ini", "");
    let (code, out, err) = run_with(
        &[("SPID3R_INI", empty_ini.as_str())],
        PATH_ENV_VARS,
        &["info"],
    );
    assert_eq!(code, 0, "err={}", err);
    for key in [
        "paths.cache_dir",
        "paths.state_dir",
        "paths.sqlite_path",
        "cache.bytecode_dir",
    ] {
        assert!(out.contains(key), "info must show {}: {}", key, out);
    }

    // config init template gains [paths] and [cache].
    let target = dir.join("out").join("spid3r.ini");
    let (code, out, err) = run_with(
        &[("SPID3R_INI", target.to_str().unwrap())],
        &[],
        &["config", "init"],
    );
    assert_eq!(code, 0, "out={} err={}", out, err);
    let text = std::fs::read_to_string(&target).expect("template written");
    assert!(text.contains("[paths]"), "template: {}", text);
    assert!(text.contains("[cache]"), "template: {}", text);
    assert!(text.contains("# cache_dir"), "template: {}", text);
    assert!(text.contains("# state_dir"), "template: {}", text);
    assert!(text.contains("# sqlite_path"), "template: {}", text);
    assert!(text.contains("# bytecode_dir"), "template: {}", text);

    // config validate accepts valid (nonexistent) paths: directories are
    // created on demand, so there is no existence requirement.
    let ini = write_in(
        &dir,
        "valid.ini",
        &format!(
            "[paths]\nstate_dir = {}\n[cache]\nbytecode_dir = {}\n",
            fwd(&dir.join("not_yet_created")),
            fwd(&dir.join("bc_not_yet")),
        ),
    );
    let (code, out, err) = run_with(
        &[("SPID3R_INI", ini.as_str())],
        PATH_ENV_VARS,
        &["config", "validate"],
    );
    assert_eq!(code, 0, "out={} err={}", out, err);

    // A path entry that points at an existing FILE can never work: validate
    // notes it (not an error at load, a note for the operator).
    let plain_file = write_in(&dir, "plain.txt", "not a directory");
    let ini = write_in(
        &dir,
        "file.ini",
        &format!("[paths]\nstate_dir = {}\n", plain_file),
    );
    let (code, out, err) = run_with(
        &[("SPID3R_INI", ini.as_str())],
        PATH_ENV_VARS,
        &["config", "validate"],
    );
    assert_ne!(code, 0, "a file where a directory belongs must be flagged");
    assert!(
        out.contains("is a file, not a directory") || err.contains("is a file, not a directory"),
        "out={} err={}",
        out,
        err
    );
}

// ---------------------------------------------------------- bytecode cache --

const BC_SCRIPT: &str = r#"fn main() {
    println("bytecode-cache-v1");
    return 0;
}
"#;

/// Write-back, hit, and the differential contract: a cached run (which
/// skipped the front end and the optimizer) prints exactly what a
/// compile-fresh run prints, and the stored container runs identically
/// under `spid3r exec`.
#[test]
fn bytecode_cache_writeback_hit_and_differential() {
    let dir = scratch_dir("bc_basic");
    let bc = dir.join("bc");
    let ini = write_in(
        &dir,
        "spid3r.ini",
        &format!("[cache]\nbytecode_dir = {}\n", fwd(&bc)),
    );
    let script = write_in(&dir, "hello.matrix", BC_SCRIPT);
    let env = [("SPID3R_INI", ini.as_str())];

    // Miss: compiles and writes back one container keyed by the source hash.
    let (code, out1, err) = run_with(&env, PATH_ENV_VARS, &["run", "--engine", "vm", &script]);
    assert_eq!(code, 0, "err={}", err);
    assert!(out1.contains("bytecode-cache-v1"), "out={}", out1);
    let mut entries: Vec<String> = std::fs::read_dir(&bc)
        .expect("cache dir created")
        .map(|e| e.unwrap().file_name().to_str().unwrap().to_string())
        .collect();
    assert_eq!(entries.len(), 1, "one container after the first run");
    let name = entries.remove(0);
    assert_eq!(
        name.len(),
        64 + ".matrixc".len(),
        "key is the source SHA-256: {}",
        name
    );
    let hash = name.strip_suffix(".matrixc").expect("container suffix");
    assert!(
        hash.chars().all(|c| c.is_ascii_hexdigit()),
        "key must be hex: {}",
        name
    );
    let container = bc.join(&name);

    // Hit: the second run must produce identical output without any
    // observable cache change.
    let (code, out2, err) = run_with(&env, PATH_ENV_VARS, &["run", "--engine", "vm", &script]);
    assert_eq!(code, 0, "err={}", err);
    assert_eq!(out1, out2, "cached run must be output-identical");
    assert_eq!(std::fs::read_dir(&bc).unwrap().count(), 1);

    // Differential contract: the stored container, run standalone via
    // `exec` (no source in the loop at all), prints the same output.
    let container_str = container.to_str().unwrap().to_string();
    let (code, out3, err) = run_with(&env, PATH_ENV_VARS, &["exec", &container_str]);
    assert_eq!(code, 0, "err={}", err);
    assert_eq!(out1, out3, "exec of the cached container must match");

    // Staleness is impossible by construction: change the source, the key
    // changes, a second container appears and the new output is served.
    let script2 = write_in(&dir, "hello.matrix", &BC_SCRIPT.replace("v1", "v2"));
    let (code, out4, err) = run_with(&env, PATH_ENV_VARS, &["run", "--engine", "vm", &script2]);
    assert_eq!(code, 0, "err={}", err);
    assert!(out4.contains("bytecode-cache-v2"), "out={}", out4);
    assert_eq!(
        std::fs::read_dir(&bc).unwrap().count(),
        2,
        "one container per source hash"
    );
    let _ = script2;
}

/// A corrupted container is never trusted: the v3 checksum fails, the run
/// falls back to a fresh compile (same output), and the write-back replaces
/// the corrupt entry.
#[test]
fn bytecode_cache_corruption_falls_back_to_compile() {
    let dir = scratch_dir("bc_corrupt");
    let bc = dir.join("bc");
    let ini = write_in(
        &dir,
        "spid3r.ini",
        &format!("[cache]\nbytecode_dir = {}\n", fwd(&bc)),
    );
    let script = write_in(&dir, "hello.matrix", BC_SCRIPT);
    let env = [("SPID3R_INI", ini.as_str())];

    let (code, out1, _) = run_with(&env, PATH_ENV_VARS, &["run", "--engine", "vm", &script]);
    assert_eq!(code, 0);
    let container = std::fs::read_dir(&bc)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();

    // Flip bytes in the payload (not the magic): the container checksum
    // must reject it.
    let mut bytes = std::fs::read(&container).expect("read container");
    assert!(bytes.len() > 40);
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&container, &bytes).expect("corrupt container");

    let (code, out2, err) = run_with(&env, PATH_ENV_VARS, &["run", "--engine", "vm", &script]);
    assert_eq!(code, 0, "a corrupt cache entry must recompile, err={}", err);
    assert_eq!(out1, out2, "recompiled output identical to the first run");

    // Write-back replaced the corrupt bytes with a good container.
    let good = std::fs::read(&container).expect("read restored container");
    assert_ne!(good, bytes, "the corrupt entry must have been rewritten");
}

/// The tree engine has no bytecode: it must not touch the bytecode cache,
/// and an unset `cache.bytecode_dir` must cache nothing at all.
#[test]
fn bytecode_cache_only_applies_to_the_vm_engine() {
    let dir = scratch_dir("bc_engine");
    let bc = dir.join("bc");
    let ini = write_in(
        &dir,
        "spid3r.ini",
        &format!("[cache]\nbytecode_dir = {}\n", fwd(&bc)),
    );
    let script = write_in(&dir, "hello.matrix", BC_SCRIPT);
    let env = [("SPID3R_INI", ini.as_str())];

    let (code, _, err) = run_with(&env, PATH_ENV_VARS, &["run", "--engine", "tree", &script]);
    assert_eq!(code, 0, "err={}", err);
    assert!(
        !bc.exists(),
        "the tree engine must not populate the bytecode cache"
    );

    // Empty key = disabled: no cache directory is created.
    let ini_empty = write_in(&dir, "empty.ini", "[cache]\nbytecode_dir =\n");
    let env_empty = [("SPID3R_INI", ini_empty.as_str())];
    let (code, _, err) = run_with(
        &env_empty,
        PATH_ENV_VARS,
        &["run", "--engine", "vm", &script],
    );
    assert_eq!(code, 0, "err={}", err);
    assert!(
        !bc.exists(),
        "an empty cache.bytecode_dir disables the cache"
    );
}
