//! Scheduler tests (Phases 4-12): spawn on the real OS thread pool, task
//! join, channels that block without busy-spinning, mutex scopes, and the
//! await rules the spec fixes (await on a task or a channel, async only).

use spid3r::diagnostics::Diagnostic;
use spid3r::interpreter::Interp;
use spid3r::parser;
use spid3r::semantic;
use std::sync::Arc;

fn run(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("t.matrix"))?;
    semantic::check_file(&program, true, "t.matrix")?;
    let interp = Interp::new(Arc::from("t.matrix"), vec![]);
    interp.run_program(&program)
}

fn expect_ok(src: &str) {
    run(src).unwrap_or_else(|d| panic!("expected success, got: {}", d));
}

fn expect_err(src: &str, needle: &str) {
    match run(src) {
        Err(d) => assert!(
            d.message.contains(needle),
            "error {:?} does not mention {}",
            d.message,
            needle
        ),
        Ok(code) => panic!("expected failure mentioning {}, exited with {}", needle, code),
    }
}

#[test]
fn spawn_returns_a_task_that_can_be_joined() {
    expect_ok(
        r#"
        async fn join(t) {
            return await t;
        }
        fn main() {
            let t = spawn fn() { return 21 * 2; };
            if join(t) != 42 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn many_tasks_all_finish() {
    expect_ok(
        r#"
        async fn join3(a, b, c) {
            return (await a) + (await b) + (await c);
        }
        fn main() {
            let t1 = spawn fn() { return 1; };
            let t2 = spawn fn() { return 2; };
            let t3 = spawn fn() { return 3; };
            if join3(t1, t2, t3) != 6 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn channels_pass_values_between_tasks() {
    expect_ok(
        r#"
        async fn wait_for(ch) {
            return await ch;
        }
        fn main() {
            let ch = channel();
            spawn fn() {
                sleep_ms(1);
                ch.send("ping");
            };
            if wait_for(ch) != "ping" { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn channel_len_and_try_recv_reflect_the_queue() {
    expect_ok(
        r#"
        fn main() {
            let ch = channel();
            ch.send(1);
            ch.send(2);
            if ch.len() != 2 { return 1; }
            if ch.try_recv() != 1 { return 1; }
            if ch.try_recv() != 2 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn mutex_guards_a_shared_cell() {
    expect_ok(
        r#"
        fn main() {
            let m = mutex(0);
            m.lock();
            let v = m.unlock();
            if v != 0 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn await_rejects_non_task_non_channel() {
    expect_err(
        r#"
        async fn bad() {
            return await 5;
        }
        fn main() {
            bad();
            return 0;
        }
    "#,
        "await expects",
    );
}

#[test]
fn await_outside_async_is_a_semantic_error() {
    expect_err(
        r#"
        fn main() {
            let t = spawn fn() { return 1; };
            return await t;
        }
    "#,
        "only allowed inside an async function",
    );
}

#[test]
fn task_count_reports_the_pool_heartbeat() {
    expect_ok(
        r#"
        fn main() {
            if task_count() < 0 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn spawn_process_reports_a_missing_binary_as_an_error() {
    // 2026-10 audit: spawn_process now follows the documented result contract
    // (docs/STANDARD_LIBRARY.md notes: "File and process operations that can
    // fail return a result (ok/err) rather than throwing"), so a missing
    // binary yields `err(msg)` instead of a thrown runtime error.
    expect_ok(
        r#"
        fn main() {
            let r = spawn_process("nonexistent-spid3r-binary-xyz", list());
            if not r.is_err() { return 1; }
            let msg = r.unwrap_err();
            if not msg.contains("nonexistent-spid3r-binary-xyz") { return 1; }
            return 0;
        }
    "#,
    );
}
