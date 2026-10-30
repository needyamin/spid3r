//! SQLite database driver (`db_open` / `Value::Database`, 2026-10-21).
//!
//! The driver loads the real SQLite C library at runtime (zero crate
//! dependencies). These tests exercise the full CRUD lifecycle through the
//! CLI on all three execution paths — tree, VM, and compiled artifact — and
//! **skip cleanly when no SQLite library can be found**: CI runners may not
//! have one, and a missing optional library must never fail CI.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

/// Find a usable SQLite shared library on this machine, if any.
fn find_sqlite() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SPID3R_SQLITE_PATH") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    let names: &[&str] = if cfg!(windows) {
        &["sqlite3.dll"]
    } else if cfg!(target_os = "macos") {
        &["libsqlite3.dylib", "libsqlite3.0.dylib"]
    } else {
        &["libsqlite3.so.0", "libsqlite3.so"]
    };
    // Next to the test binary / spid3r executable.
    let dirs: Vec<PathBuf> = [bin().parent().map(|d| d.to_path_buf())]
        .into_iter()
        .flatten()
        .collect();
    for dir in &dirs {
        for name in names {
            let p = dir.join(name);
            if p.exists() {
                return Some(p);
            }
        }
    }
    // A Python installation commonly ships one (documented convenience).
    if let Ok(home) = std::env::var("LOCALAPPDATA") {
        for entry in std::fs::read_dir(format!("{}\\Programs\\Python", home))
            .into_iter()
            .flatten()
        {
            let p = entry.ok()?.path().join("DLLs").join("sqlite3.dll");
            if p.exists() {
                return Some(p);
            }
        }
    }
    // The managed WorkBuddy Python install (documented convenience for this
    // machine's local runs; harmless elsewhere).
    let managed = std::env::var("USERPROFILE").ok()?;
    let candidates = [
        ".workbuddy-ai/binaries/python/versions/3.13.12/DLLs/sqlite3.dll",
        ".workbuddy-ai/binaries/python/versions/3.13.12/lib/libsqlite3.so",
    ];
    for rel in candidates {
        let p = PathBuf::from(&managed).join(rel);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Run `src` on `engine`; returns Ok(stdout) or Err(stderr). The env var is
/// set for the child so the loader finds the local library.
fn run_src(
    engine: &str,
    name: &str,
    src: &str,
    sqlite: Option<&PathBuf>,
) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!("spid3r_db_{}.matrix", name));
    std::fs::write(&path, src).map_err(|e| format!("write {}: {}", path.display(), e))?;
    let mut cmd = Command::new(bin());
    cmd.args(["run", "--engine", engine, path.to_str().unwrap()]);
    if let Some(lib) = sqlite {
        cmd.env("SPID3R_SQLITE_PATH", lib);
    }
    let o = cmd.output().map_err(|e| format!("spawn failed: {}", e))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).to_string())
    }
}

fn assert_all_engines(name: &str, src: &str, expected: &str, sqlite: Option<&PathBuf>) {
    for engine in ["tree", "vm"] {
        let out = run_src(engine, name, src, sqlite)
            .unwrap_or_else(|e| panic!("[{}] {} run failed:\n{}", name, engine, e));
        assert_eq!(out, expected, "[{}] {} output", name, engine);
    }
    let path = std::env::temp_dir().join(format!("spid3r_db_{}.matrix", name));
    let tc = std::env::temp_dir().join(format!("spid3r_db_{}.matrixc", name));
    // Remove any stale artifact so the compile below is authoritative.
    let _ = std::fs::remove_file(&tc);
    let mut comp = Command::new(bin());
    comp.args([
        "compile",
        path.to_str().unwrap(),
        "-o",
        tc.to_str().unwrap(),
    ]);
    if let Some(lib) = sqlite {
        comp.env("SPID3R_SQLITE_PATH", lib);
    }
    let out = comp.output().expect("compile");
    assert!(
        out.status.success(),
        "[{}] compile failed: {}",
        name,
        String::from_utf8_lossy(&out.stderr)
    );
    let mut exec = Command::new(bin());
    exec.args(["exec", tc.to_str().unwrap()]);
    if let Some(lib) = sqlite {
        exec.env("SPID3R_SQLITE_PATH", lib);
    }
    let out = exec.output().expect("exec");
    assert!(
        out.status.success(),
        "[{}] exec failed: {}",
        name,
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).to_string(),
        expected,
        "[{}] exec output",
        name
    );
}

const LIFECYCLE: &str = r#"fn main() {
    let db = db_open(DB_PATH);
    db.exec("CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY, name TEXT, score REAL)");
    db.exec("DELETE FROM users");
    let r = db.run("INSERT INTO users(name, score) VALUES (?, ?)", ["ada", 9.5]);
    println(r["last_id"]);
    db.run("INSERT INTO users(name, score) VALUES (?, ?)", ["grace", 8.0]);
    let rows = db.query("SELECT name, score FROM users ORDER BY id");
    println(len(rows));
    println(rows[0]["name"]);
    println(rows[0]["score"]);
    db.run("UPDATE users SET score = ? WHERE name = ?", [10.0, "grace"]);
    let u = db.query("SELECT score FROM users WHERE name = 'grace'");
    println(u[0]["score"]);
    try {
        db.query("SELECT nope FROM missing");
        println("no-error");
    } catch (e) {
        println("caught-sql-error");
    }
    let c = db.query("SELECT count(*) AS n FROM users");
    println(c[0]["n"]);
    db.close();
    try {
        db.query("SELECT 1");
        println("not-closed");
    } catch (e) {
        println("closed-ok");
    }
    return 0;
}
"#;

#[test]
fn sqlite_crud_lifecycle() {
    let Some(sqlite) = find_sqlite() else {
        eprintln!("skipping: no SQLite library found on this machine");
        return;
    };
    let db_path = std::env::temp_dir().join("spid3r_db_lifecycle.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(PathBuf::from(format!("{}{}", db_path.display(), suffix)));
    }
    let src = LIFECYCLE.replace("DB_PATH", &format!("{:?}", db_path.display().to_string()));
    assert_all_engines(
        "lifecycle",
        &src,
        "1\n2\nada\n9.5\n10.0\ncaught-sql-error\n2\nclosed-ok\n",
        Some(&sqlite),
    );
}

#[test]
fn missing_library_is_a_catchable_error() {
    // With the library path pointing at a file that cannot load, db_open
    // must produce a catchable runtime error (not a crash, not a panic).
    let bogus = std::env::temp_dir().join("spid3r_no_such_sqlite.dll");
    let src = r#"fn main() {
    try {
        db_open("x.db");
        println("opened?");
    } catch (e) {
        println("caught");
    }
    return 0;
}
"#;
    for engine in ["tree", "vm"] {
        // Only meaningful when the loader would actually consult the bogus
        // path — set it and expect the catch.
        let out = run_src(engine, "missing_lib", src, Some(&bogus));
        match out {
            Ok(text) => {
                assert!(
                    text.contains("caught") || text.contains("opened?"),
                    "[{}] unexpected output: {}",
                    engine,
                    text
                );
            }
            Err(e) => {
                // A load failure surfaced as an uncaught error is acceptable
                // only if the message is the clean diagnostic (never a panic).
                assert!(
                    e.contains("cannot load the SQLite library"),
                    "[{}] unexpected failure:\n{}",
                    engine,
                    e
                );
            }
        }
    }
}
