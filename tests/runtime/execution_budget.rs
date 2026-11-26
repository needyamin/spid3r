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

#[cfg(feature = "native")]
#[test]
fn r12_3_native_enforce_budget_stops_runaway_and_default_unaffected() {
    let exe = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("spid3r{}", std::env::consts::EXE_SUFFIX));
    let dir = std::env::temp_dir().join(format!("spid3r_native_bud_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let src_file = dir.join("spin.matrix");
    std::fs::write(
        &src_file,
        "fn main() { let i = 0; while i < 100000 { i = i + 1; } println(i); return 0; }\n",
    )
    .expect("write");

    let bud_exe = dir.join(format!("spin_bud{}", std::env::consts::EXE_SUFFIX));
    let b1 = std::process::Command::new(&exe)
        .args([
            "build",
            "--enforce-budget",
            "5",
            src_file.to_str().unwrap(),
            "-o",
            bud_exe.to_str().unwrap(),
        ])
        .output()
        .expect("build --enforce-budget");
    assert_eq!(
        b1.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&b1.stderr)
    );
    let r1 = std::process::Command::new(&bud_exe).output().expect("run");
    assert_ne!(r1.status.code(), Some(0), "budget must abort runaway loop");
    assert!(
        String::from_utf8_lossy(&r1.stderr).contains(MSG),
        "stderr: {}",
        String::from_utf8_lossy(&r1.stderr)
    );

    // Off-by-default finishes cleanly.
    let def_exe = dir.join(format!("spin_def{}", std::env::consts::EXE_SUFFIX));
    let b2 = std::process::Command::new(&exe)
        .args([
            "build",
            src_file.to_str().unwrap(),
            "-o",
            def_exe.to_str().unwrap(),
        ])
        .output()
        .expect("build default");
    assert_eq!(b2.status.code(), Some(0));
    let r2 = std::process::Command::new(&def_exe).output().expect("run");
    assert_eq!(r2.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&r2.stdout).trim_end(), "100000");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn r12_4_deep_ref_chain_drops_iteratively_and_import_cycle_is_reported() {
    use spid3r_lang::embed::Engine;
    let src = r#"
fn main() {
    let cur = ref(0);
    let i = 1;
    while i < 20000 {
        cur = ref(cur);
        i = i + 1;
    }
    println("built");
    return 0;
}
"#;
    let out = Engine::new().run("deep_ref", src).expect("deep ref runs");
    assert_eq!(out.output, "built\n");

    let dir = std::env::temp_dir().join(format!("spid3r_cycle_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let a = dir.join("a.matrix");
    let b = dir.join("b.matrix");
    std::fs::write(&a, "import \"b\";\nfn main() { return 0; }\n").expect("write a");
    std::fs::write(&b, "import \"a\";\npublic fn f() { return 1; }\n").expect("write b");
    let prog = parser::parse_file(
        &std::fs::read_to_string(&a).unwrap(),
        Arc::from(a.to_string_lossy().to_string()),
    )
    .expect("parse");
    let err = semantic::check_file(&prog, true, a.to_str().unwrap())
        .expect_err("import cycle must be diagnosed");
    assert!(
        err.message.contains("circular import"),
        "expected circular import error, got: {}",
        err.message
    );
    let _ = std::fs::remove_dir_all(&dir);
}
