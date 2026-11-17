//! R2.3 event-loop core tests: socket readiness (`on_readable`, `on_writable`,
//! `event_loop_unwatch`), timers (`on_timer`), task cancellation during poll,
//! and GC-quiescence interplay (idle loop must not freeze `gc_collect()` or
//! trigger the deadlock watchdog). Both tree and VM engines must match.

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::modules;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::vm::Vm;
use std::sync::{Arc, Mutex};

static TEST_LOCK: Mutex<()> = Mutex::new(());

fn run_tree(src: &str) -> Result<i32, Diagnostic> {
    let _g = TEST_LOCK.lock().unwrap();
    let program = parser::parse_file(src, Arc::from("event_loop_test.matrix"))?;
    semantic::check_file(&program, true, "event_loop_test.matrix")?;
    let interp = Interp::new(
        Arc::from("event_loop_test.matrix"),
        modules::default_module_dirs(),
    );
    interp.run_program(&program)
}

fn run_vm(src: &str) -> Result<i32, Diagnostic> {
    let _g = TEST_LOCK.lock().unwrap();
    let program = parser::parse_file(src, Arc::from("event_loop_test.matrix"))?;
    semantic::check_file(&program, true, "event_loop_test.matrix")?;
    let dirs = modules::default_module_dirs();
    let mut vm = Vm::new(Arc::from("event_loop_test.matrix"), dirs);
    vm.run_program(&program)
}

const EVENT_LOOP_READINESS_AND_TIMERS: &str = r#"
fn main() {
    let listener = tcp_listen("127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(listener).unwrap();
    let ch = channel(8);

    on_readable(listener, fn(l) {
        let conn = tcp_accept(l).unwrap();
        on_readable(conn, fn(c) {
            let msg = tcp_read(c, 64).unwrap();
            if msg == "" {
                event_loop_unwatch(c);
                tcp_close(c);
                return;
            }
            tcp_write(c, msg);
            ch.send("echoed:" + msg);
            event_loop_unwatch(c);
            tcp_close(c);
        });
    });

    let t = after_ms(20);
    on_timer(t, fn(tid) {
        ch.send("timer");
    });

    // Safety stop timer in case something stalls.
    let watchdog = after_ms(3000);
    on_timer(watchdog, fn(tid) {
        event_loop_stop();
    });

    let runner = spawn fn() {
        event_loop_run();
    };

    let client = tcp_connect(addr).unwrap();
    tcp_write(client, "hello");
    let reply = tcp_read(client, 64).unwrap();
    assert_eq(reply, "hello");

    let seen_echo = false;
    let seen_timer = false;
    let i = 0;
    while i < 2 {
        let ev = ch.recv();
        if ev == "echoed:hello" { seen_echo = true; }
        if ev == "timer" { seen_timer = true; }
        i += 1;
    }
    assert_eq(seen_echo, true);
    assert_eq(seen_timer, true);

    timer_cancel(watchdog);
    event_loop_unwatch(listener);
    event_loop_stop();
    runner.join();
    tcp_close(client);
    tcp_close(listener);
    return 0;
}
"#;

#[test]
fn event_loop_readiness_and_timers_on_both_engines() {
    assert_eq!(run_tree(EVENT_LOOP_READINESS_AND_TIMERS).unwrap(), 0);
    assert_eq!(run_vm(EVENT_LOOP_READINESS_AND_TIMERS).unwrap(), 0);
}

const EVENT_LOOP_GC_AND_CANCEL: &str = r#"
fn main() {
    // 1. GC-quiescence interplay: while event_loop_run() is parked idle,
    //    `gc_collect()` must succeed without freezing or firing watchdog.
    let runner = spawn fn() {
        event_loop_run();
    };
    sleep_ms(40);
    let collected = gc_collect();
    assert_eq(collected >= 0, true);

    // 2. Cancel-during-poll: cancelling the task running event_loop_run()
    //    unblocks it promptly.
    assert_eq(runner.cancel(), true);
    try { runner.join(); } catch (e) { }
    event_loop_stop();
    return 0;
}
"#;

#[test]
fn event_loop_gc_quiescence_and_cancel_on_both_engines() {
    assert_eq!(run_tree(EVENT_LOOP_GC_AND_CANCEL).unwrap(), 0);
    assert_eq!(run_vm(EVENT_LOOP_GC_AND_CANCEL).unwrap(), 0);
}
