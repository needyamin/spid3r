//! Regression tests for the deadlock-hardening fixes (audit round 3).
//!
//! Both fixes were found by round-3 probes (`audit/probes_r3/`):
//!
//! * **Mutex self-deadlock.** `mu.lock()` on a mutex the calling context
//!   already held parked forever with no diagnostic (the probe hung until the
//!   60 s watchdog timeout). `MutexData` now records the owner
//!   (`current_owner_id`) and returns a catchable
//!   `mutex: deadlock - this task already holds the lock` error instead.
//! * **recv with no possible sender.** `ch.recv()` on a channel no task will
//!   ever send to blocked the process forever, silently. Waits that only
//!   another task can end (`recv`/`mutex.lock`/`join`) now register as
//!   *untimed parks*; when nothing is running, queued, or live and a thread
//!   is parked that way, the scheduler watchdog aborts with a fatal
//!   diagnostic (exit 1) after ~3 s. `SPID3R_DEADLOCK_ABORT=0` downgrades to a
//!   one-shot warning. Socket waits and `sleep_ms` are timed parks and never
//!   trigger the detector (pinned by `long_sleep_is_not_a_deadlock`).

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

/// Write `src` to a temp file and run it on `engine`; return (exit code,
/// stdout + stderr).
fn run_engine(engine: &str, name: &str, src: &str) -> (i32, String) {
    let path = std::env::temp_dir().join(format!("spid3r_deadlock_{}.matrix", name));
    std::fs::write(&path, src).expect("write probe");
    let out = Command::new(bin())
        .args(["run", "--engine", engine, path.to_str().unwrap()])
        .output()
        .expect("spawn spid3r");
    let _ = std::fs::remove_file(&path);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr),
    )
}

/// Run on both engines and assert the same outcome.
fn both_engines(name: &str, src: &str, check: impl Fn(&str, i32, &str)) {
    for engine in ["tree", "vm"] {
        let (code, out) = run_engine(engine, &format!("{}_{}", name, engine), src);
        check(engine, code, &out);
    }
}

#[test]
fn mutex_self_deadlock_is_a_clean_error_not_a_hang() {
    let src = r#"
fn main() {
    let m = mutex(1);
    let g = m.lock();
    println("first lock ok");
    let h = m.lock();
    println("second lock ok (BAD)");
    return 0;
}
"#;
    both_engines("self_deadlock", src, |engine, code, out| {
        assert_eq!(code, 1, "[{}] expected exit 1, output: {}", engine, out);
        assert!(
            out.contains("mutex: deadlock"),
            "[{}] expected the deadlock diagnostic, output: {}",
            engine,
            out
        );
        assert!(
            !out.contains("second lock ok (BAD)"),
            "[{}] the second lock must not succeed, output: {}",
            engine,
            out
        );
    });
}

#[test]
fn mutex_self_deadlock_is_catchable_and_the_mutex_stays_usable() {
    let src = r#"
fn main() {
    let m = mutex(7);
    let g = m.lock();
    let caught = "not caught";
    try {
        let h = m.lock();
        caught = "second lock succeeded (BAD)";
    } catch(e) {
        caught = string(e);
    }
    println("caught: " + caught);
    m.unlock();
    let g2 = m.lock();
    println("relocked, value " + string(g2));
    m.unlock();
    println("done");
    return 0;
}
"#;
    both_engines("catchable", src, |engine, code, out| {
        assert_eq!(code, 0, "[{}] expected exit 0, output: {}", engine, out);
        assert!(
            out.contains("caught: mutex: deadlock"),
            "[{}] the deadlock error must be catchable, output: {}",
            engine,
            out
        );
        assert!(
            out.contains("relocked, value 7") && out.contains("done"),
            "[{}] the mutex must stay usable, output: {}",
            engine,
            out
        );
    });
}

#[test]
fn mutex_sequential_relock_still_works() {
    // Sanity: lock / unlock / lock / unlock in one scope is legal and the
    // second lock must see the value.
    let src = r#"
fn main() {
    let m = mutex(0);
    let a = m.lock();
    m.unlock();
    let b = m.lock();
    m.unlock();
    println("both locks returned " + string(a) + " and " + string(b));
    return 0;
}
"#;
    both_engines("relock", src, |engine, code, out| {
        assert_eq!(code, 0, "[{}] expected exit 0, output: {}", engine, out);
        assert!(
            out.contains("both locks returned 0 and 0"),
            "[{}] output: {}",
            engine,
            out
        );
    });
}

#[test]
fn cross_task_mutex_lock_still_works() {
    // The owner check must not break the legitimate pattern: one task holds
    // the lock, another waits, the holder releases, the waiter proceeds.
    let src = r#"
fn main() {
    let m = mutex(1);
    let g = m.lock();
    let t = spawn fn() {
        let h = m.lock();
        return h;
    };
    sleep_ms(50);
    m.unlock();
    println("task got " + string(t.join()));
    println("done");
    return 0;
}
"#;
    both_engines("cross_task", src, |engine, code, out| {
        assert_eq!(code, 0, "[{}] expected exit 0, output: {}", engine, out);
        assert!(
            out.contains("task got 1") && out.contains("done"),
            "[{}] output: {}",
            engine,
            out
        );
    });
}

#[test]
fn recv_with_no_sender_aborts_with_a_diagnostic() {
    // No spawn at all: the park itself must start the deadlock watchdog.
    let src = r#"
fn main() {
    let ch = channel();
    println("about to recv");
    let v = ch.recv();
    println("got " + string(v));
    return 0;
}
"#;
    let started = Instant::now();
    both_engines("recv_no_sender", src, |engine, code, out| {
        assert_eq!(code, 1, "[{}] expected exit 1, output: {}", engine, out);
        assert!(
            out.contains("deadlock"),
            "[{}] expected the deadlock diagnostic, output: {}",
            engine,
            out
        );
        assert!(
            !out.contains("got "),
            "[{}] recv must not return, output: {}",
            engine,
            out
        );
    });
    // The detector must fire promptly (~3 s), not after a long hang.
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "deadlock detection took too long: {:?}",
        started.elapsed()
    );
}

#[test]
fn long_sleep_is_not_a_deadlock() {
    // False-positive guard: sleeps are timed parks. Main sleeps 6 s and a
    // spawned task sleeps 3 s before joining — neither may trip the detector.
    let src = r#"
fn main() {
    sleep_ms(6000);
    let t = spawn fn() {
        sleep_ms(3000);
        return 42;
    };
    println("task returned " + string(t.join()));
    println("done");
    return 0;
}
"#;
    both_engines("long_sleep", src, |engine, code, out| {
        assert_eq!(code, 0, "[{}] expected exit 0, output: {}", engine, out);
        assert!(
            out.contains("task returned 42") && out.contains("done"),
            "[{}] output: {}",
            engine,
            out
        );
        assert!(
            !out.contains("deadlock"),
            "[{}] a sleeping program is not deadlocked, output: {}",
            engine,
            out
        );
    });
}

#[test]
fn deadlock_abort_can_be_downgraded_to_a_warning() {
    // SPID3R_DEADLOCK_ABORT=0: warn once, keep waiting (the harness kills the
    // child after the warning). Pins the embedding-host escape hatch.
    let path = std::env::temp_dir().join("spid3r_deadlock_downgrade.matrix");
    std::fs::write(
        &path,
        r#"
fn main() {
    let ch = channel();
    let v = ch.recv();
    return 0;
}
"#,
    )
    .expect("write probe");
    let mut child = Command::new(bin())
        .args(["run", path.to_str().unwrap()])
        .env("SPID3R_DEADLOCK_ABORT", "0")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn spid3r");
    // The warning fires at ~3 s; give it room, then kill the (still waiting)
    // child and count the warnings.
    std::thread::sleep(Duration::from_secs(6));
    let _ = child.kill();
    let out = child.wait_with_output().expect("wait for child");
    let _ = std::fs::remove_file(&path);
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    let warnings = text.matches("deadlock").count();
    assert!(
        warnings >= 1,
        "expected at least one deadlock warning, stderr: {}",
        text
    );
    assert_eq!(
        warnings, 1,
        "the warning must be one-shot, stderr: {}",
        text
    );
}
