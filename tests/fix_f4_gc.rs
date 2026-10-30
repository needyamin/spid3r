//! F-4 regression: building a deeply nested list must not stall the runtime.
//!
//! Before the fix, two shared cost centers in `src/value/gc.rs` were quadratic
//! in the number of live tracked cells: the statement-boundary auto-collector
//! re-scanned the whole graph every fixed 1024 allocations, and `track()`
//! pruned the whole registry on EVERY allocation once it held >8192 entries.
//! A ~100k-deep build already blew past 60 s on both engines (audit probes
//! p07/p07b, F-4 in docs/AUDIT_REPORT.md). The fix amortizes
//! both (exponential backoff on the collect threshold, watermark doubling on
//! the registry prune), so the build is linear again. This test pins a
//! 400k-deep build completing in under 10 s per engine with the auto-GC on.
//!
//! Helper pattern from tests/audit_regressions.rs: the suite must never hang,
//! so the child is spawned and polled with `try_wait` against a deadline and
//! killed on timeout.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

/// Spawn `spid3r run --engine <engine> <path>` with `SPID3R_GC=1` (auto-GC on;
/// explicit so an ambient `SPID3R_GC=0` cannot mask a regression) and poll
/// with `try_wait` until it exits or `limit` elapses. Killed at the deadline.
fn run_gc_on(engine: &str, path: &Path, limit: Duration) -> Result<String, String> {
    let mut child = Command::new(bin())
        .args(["run", "--engine", engine])
        .arg(path)
        .env("SPID3R_GC", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn failed: {}", e))?;
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("wait failed: {}", e))?
        {
            let mut out = String::new();
            if let Some(mut s) = child.stdout.take() {
                let _ = s.read_to_string(&mut out);
            }
            if !status.success() {
                let mut err = String::new();
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut err);
                }
                return Err(format!("exit {:?}: {}", status.code(), err));
            }
            return Ok(out);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "did not finish within {:?} — the F-4 stall is back",
                limit
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

const BUILD_400K: &str = r#"
fn main() {
    let l = list(1);
    let i = 0;
    while i < 400000 {
        let n = list();
        n.push(l);
        l = n;
        i = i + 1;
    }
    println("built");
    return 0;
}
"#;

#[test]
fn deep_nested_list_build_completes_with_gc_on() {
    let path = std::env::temp_dir().join(format!("spid3r_f4_build_{}.matrix", std::process::id()));
    std::fs::write(&path, BUILD_400K).expect("write probe");
    for engine in ["tree", "vm"] {
        let started = Instant::now();
        let out = run_gc_on(engine, &path, Duration::from_secs(10))
            .unwrap_or_else(|e| panic!("[{}] 400k deep build failed: {}", engine, e));
        assert!(
            out.contains("built"),
            "[{}] expected \"built\" on stdout, got: {}",
            engine,
            out
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "[{}] took {:?} — GC cost is not amortized",
            engine,
            started.elapsed()
        );
    }
    let _ = std::fs::remove_file(&path);
}
