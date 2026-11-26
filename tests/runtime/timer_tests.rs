//! R2.2 timer tests: ordering, cancellation, intervals, monotonic edge, and
//! capability-free sandbox availability. Both tree and VM engines must match.

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::embed::{Capability, Engine};
use spid3r_lang::interpreter::Interp;
use spid3r_lang::modules;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::vm::Vm;
use std::sync::{Arc, Mutex};

// `timer_check()` pops from the process-wide timer heap, so serialize tests
// in this binary to prevent concurrent tests from consuming each other's ticks.
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn run_tree(src: &str) -> Result<i32, Diagnostic> {
    let _g = TEST_LOCK.lock().unwrap();
    let program = parser::parse_file(src, Arc::from("timer_test.matrix"))?;
    semantic::check_file(&program, true, "timer_test.matrix")?;
    let interp = Interp::new(
        Arc::from("timer_test.matrix"),
        modules::default_module_dirs(),
    );
    interp.run_program(&program)
}

fn run_vm(src: &str) -> Result<i32, Diagnostic> {
    let _g = TEST_LOCK.lock().unwrap();
    let program = parser::parse_file(src, Arc::from("timer_test.matrix"))?;
    semantic::check_file(&program, true, "timer_test.matrix")?;
    let dirs = modules::default_module_dirs();
    let mut vm = Vm::new(Arc::from("timer_test.matrix"), dirs);
    vm.run_program(&program)
}

const TIMER_ORDER_AND_CANCEL: &str = r#"
fn main() {
    // Schedule out of deadline order: t_late (80ms) before t_early (20ms),
    // plus t_cancel (30ms) which is cancelled before firing.
    let t_late = after_ms(80);
    let t_cancel = after_ms(30);
    let t_early = after_ms(20);
    assert_eq(timer_pending(t_late), true);
    assert_eq(timer_pending(t_cancel), true);
    assert_eq(timer_pending(t_early), true);

    assert_eq(timer_cancel(t_cancel), true);
    assert_eq(timer_cancel(t_cancel), false);
    assert_eq(timer_pending(t_cancel), false);

    // Wait long enough for both active timers to expire, then poll once.
    sleep_ms(110);
    let fired = timer_check();
    // Filter to just our handles in case other tests ran in the same process.
    let ours = list();
    for id in fired {
        if id == t_early || id == t_late || id == t_cancel {
            ours.push(id);
        }
    }
    assert_eq(len(ours), 2);
    assert_eq(ours[0], t_early);
    assert_eq(ours[1], t_late);
    assert_eq(timer_pending(t_early), false);
    assert_eq(timer_pending(t_late), false);
    return 0;
}
"#;

#[test]
fn timer_ordering_and_cancel_on_both_engines() {
    assert_eq!(run_tree(TIMER_ORDER_AND_CANCEL).unwrap(), 0);
    assert_eq!(run_vm(TIMER_ORDER_AND_CANCEL).unwrap(), 0);
}

const TIMER_INTERVAL_AND_ZERO_EDGE: &str = r#"
fn main() {
    // Monotonic edge: after_ms(0) is immediately due on timer_check().
    let t0 = after_ms(0);
    assert_eq(timer_pending(t0), true);
    let f0 = timer_check();
    let seen0 = false;
    for id in f0 {
        if id == t0 { seen0 = true; }
    }
    assert_eq(seen0, true);
    assert_eq(timer_pending(t0), false);

    // Interval timer fires repeatedly until cancelled.
    let iv = every_ms(25);
    assert_eq(timer_pending(iv), true);
    sleep_ms(40);
    let f1 = timer_check();
    let c1 = 0;
    for id in f1 { if id == iv { c1 += 1; } }
    assert_eq(c1, 1);
    assert_eq(timer_pending(iv), true);

    sleep_ms(40);
    let f2 = timer_check();
    let c2 = 0;
    for id in f2 { if id == iv { c2 += 1; } }
    assert_eq(c2, 1);
    assert_eq(timer_cancel(iv), true);
    assert_eq(timer_pending(iv), false);

    // Invalid delays raise catchable runtime errors.
    let neg_err = false;
    try { after_ms(-1); } catch (e) { neg_err = true; }
    assert_eq(neg_err, true);

    let zero_iv_err = false;
    try { every_ms(0); } catch (e) { zero_iv_err = true; }
    assert_eq(zero_iv_err, true);
    return 0;
}
"#;

#[test]
fn timer_interval_zero_edge_and_errors_on_both_engines() {
    assert_eq!(run_tree(TIMER_INTERVAL_AND_ZERO_EDGE).unwrap(), 0);
    assert_eq!(run_vm(TIMER_INTERVAL_AND_ZERO_EDGE).unwrap(), 0);
}

#[test]
fn timers_are_capability_free_under_sandbox() {
    let _g = TEST_LOCK.lock().unwrap();
    let engine = Engine::new()
        .deny(Capability::FileSystem)
        .deny(Capability::Network)
        .deny(Capability::Process)
        .deny(Capability::Environment);
    let out = engine
        .run(
            "timer_sandbox.matrix",
            r#"
            fn main() {
                let t = after_ms(0);
                assert_eq(timer_pending(t), true);
                let fired = timer_check();
                let seen = false;
                for id in fired { if id == t { seen = true; } }
                assert_eq(seen, true);
                return 0;
            }
            "#,
        )
        .expect("timers must be allowed when all capabilities are denied");
    assert_eq!(out.exit_code, 0);
}
