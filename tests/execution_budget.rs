//! Execution budget (2026-10-29): a per-run step allowance that bounds
//! untrusted code. Both engines enforce it, exhaustion is a **fatal** error
//! that `try`/`catch` cannot intercept, and a run without a budget is
//! unaffected.
//!
//! Every test carries a wall-clock deadline: a budget that fails to stop a
//! runaway loop would otherwise hang the suite instead of failing it.

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::modules;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::vm::Vm;
use std::sync::Arc;
use std::time::{Duration, Instant};

const MSG: &str = "execution budget exhausted";

fn run_tree(src: &str, budget: Option<u64>) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("budget.matrix"))?;
    semantic::check_file(&program, true, "budget.matrix")?;
    let mut interp = Interp::new(Arc::from("budget.matrix"), modules::default_module_dirs());
    interp.set_budget(budget.map(spid3r_lang::budget::Budget::new));
    interp.run_program(&program)
}

fn run_vm(src: &str, budget: Option<u64>) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("budget.matrix"))?;
    semantic::check_file(&program, true, "budget.matrix")?;
    let dirs = modules::default_module_dirs();
    let mut vm = Vm::new(Arc::from("budget.matrix"), dirs);
    vm.set_budget(budget.map(spid3r_lang::budget::Budget::new));
    vm.run_program(&program)
}

/// Run both engines, require both to fail with the budget message, and bound
/// the wall-clock cost so a regression fails instead of hanging.
fn both_fail_with_budget(src: &str, budget: u64) {
    let started = Instant::now();
    let tree = run_tree(src, Some(budget));
    let vm = run_vm(src, Some(budget));
    for (engine, result) in [("tree", tree), ("vm", vm)] {
        match result {
            Ok(code) => panic!(
                "{} engine: expected budget failure, got exit {}",
                engine, code
            ),
            Err(d) => assert!(
                d.message.contains(MSG),
                "{} engine: expected '{}', got '{}'",
                engine,
                MSG,
                d.message
            ),
        }
    }
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "budget enforcement took {:?}; the stop is not prompt",
        started.elapsed()
    );
}

#[test]
fn budget_stops_runaway_loop_on_both_engines() {
    both_fail_with_budget(
        "fn main() { let i = 0; while true { i = i + 1; } return 0; }",
        5_000,
    );
}

#[test]
fn budget_stops_empty_body_loop() {
    // An empty body never executes a statement, so the per-iteration charge
    // is what must stop this loop.
    both_fail_with_budget("fn main() { while true { } return 0; }", 3_000);
}

#[test]
fn budget_error_is_not_catchable() {
    // If the fatal error were catchable, the program would print and return
    // 0; instead the run must fail with the budget message on both engines.
    both_fail_with_budget(
        r#"fn main() {
            try { while true { } } catch (e) { println("swallowed"); }
            return 0;
        }"#,
        3_000,
    );
}

#[test]
fn budget_also_applies_inside_functions_and_callbacks() {
    both_fail_with_budget(
        r#"fn spin(n) { let i = 0; while true { i = i + 1; } return i; }
        fn main() { let l = list(1, 2, 3); l.map(fn(x) { return spin(x); }); return 0; }"#,
        5_000,
    );
}

#[test]
fn budget_covers_spawned_tasks() {
    // The task draws from the same allowance: join must surface the stop.
    both_fail_with_budget(
        r#"fn main() {
            let t = spawn fn() { while true { } };
            t.join();
            return 0;
        }"#,
        3_000,
    );
}

#[test]
fn generous_budget_does_not_change_normal_programs() {
    let src = r#"fn main() {
        let total = 0;
        let i = 0;
        while i < 1000 { total = total + i; i = i + 1; }
        assert_eq(total, 499500);
        return 0;
    }"#;
    assert_eq!(run_tree(src, Some(1_000_000)).unwrap(), 0);
    assert_eq!(run_vm(src, Some(1_000_000)).unwrap(), 0);
    // And no budget at all (the default) behaves identically.
    assert_eq!(run_tree(src, None).unwrap(), 0);
    assert_eq!(run_vm(src, None).unwrap(), 0);
}

#[test]
fn embed_budget_api_stops_untrusted_code() {
    use spid3r_lang::embed::Engine;

    let engine = Engine::new().budget(10_000);
    let err = engine
        .run("spin", "fn main() { while true { } return 0; }")
        .expect_err("the budget must stop the loop");
    assert!(
        err.diagnostic.message.contains(MSG),
        "unexpected message: {}",
        err.diagnostic.message
    );

    // A normal program under a generous budget still succeeds with its output.
    let engine = Engine::new().budget(5_000_000);
    let out = engine
        .run(
            "ok",
            r#"fn main() { let s = 0; let i = 0; while i < 10 { s = s + i; i = i + 1; } println(s); return 0; }"#,
        )
        .expect("script runs");
    assert_eq!(out.exit_code, 0);
    assert_eq!(out.output, "45\n");
}

#[test]
fn zero_budget_stops_even_the_first_statement() {
    use spid3r_lang::embed::Engine;
    let engine = Engine::new().budget(0);
    let err = engine
        .run("zero", "fn main() { println(\"hi\"); return 0; }")
        .expect_err("a zero budget must stop immediately");
    assert!(err.diagnostic.message.contains(MSG));
    assert_eq!(err.output, "", "nothing should run under a zero budget");
}
