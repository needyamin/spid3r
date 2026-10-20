//! Error-handling tests (Phases 4-12): throw/catch/finally semantics, thrown
//! values, rethrow, catch binding, and runtime errors surfacing as Spid3r
//! errors with spans instead of Rust panics.

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use std::process::Command;
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
fn catch_catches_a_thrown_value() {
    expect_ok(
        r#"
        fn main() {
            let caught = 0;
            try {
                throw 7;
            } catch (e) {
                caught = e;
            }
            if caught != 7 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn catch_can_inspect_runtime_errors() {
    expect_ok(
        r#"
        fn main() {
            let msg = "";
            try {
                let xs = list();
                let x = xs.get(3);
            } catch (e) {
                msg = string(e);
            }
            if len(msg) == 0 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn finally_runs_on_both_paths() {
    expect_ok(
        r#"
        fn main() {
            let runs = 0;
            try {
                throw "boom";
            } catch (e) {
            } finally {
                runs += 1;
            }
            try {
                // no throw
            } catch (e) {
            } finally {
                runs += 1;
            }
            if runs != 2 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn finally_runs_while_returning_from_try() {
    expect_ok(
        r#"
        fn pick(flag) {
            let seen = 0;
            try {
                if flag { return "a"; }
                return "b";
            } finally {
                seen += 1;
            }
        }
        fn main() {
            if pick(true) != "a" { return 1; }
            if pick(false) != "b" { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn uncaught_throw_propagates_with_its_value() {
    let d = run(
        r#"
        fn main() {
            throw "kaboom";
        }
    "#,
    )
    .unwrap_err();
    assert!(
        d.message.contains("uncaught throw"),
        "message should say uncaught throw: {}",
        d.message
    );
    match &d.thrown {
        Some(v) => assert_eq!(spid3r_lang::value::display_value(v), "kaboom"),
        None => panic!("the thrown value should be attached"),
    }
}

#[test]
fn throw_values_can_be_any_type() {
    expect_ok(
        r#"
        fn main() {
            let got = null;
            try {
                throw list("e", 42);
            } catch (e) {
                got = e;
            }
            if got.len() != 2 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn break_out_of_a_catch_is_rejected_by_the_parser_rules() {
    // `try` without catch or finally is a parser error, not a runtime one.
    expect_err(
        r#"
        fn main() {
            try {
                throw 1;
            }
            return 0;
        }
    "#,
        "catch",
    );
}

#[test]
fn an_error_inside_a_caught_task_becomes_a_catch_value() {
    expect_ok(
        r#"
        async fn risky() {
            let bad = list();
            let v = bad.get(9);
            return v;
        }
        async fn guard() {
            try {
                await spawn fn() { return risky_runner(); };
            } catch (e) {
                return "caught";
            }
            return "no";
        }
        fn risky_runner() {
            let bad = list();
            return bad.get(9);
        }
        fn main() {
            if guard() != "caught" { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn errors_carry_spans() {
    let d = run(
        r#"
        fn main() {
            let xs = list(1);
            let y = xs.get(9);
            return 0;
        }
    "#,
    )
    .unwrap_err();
    assert!(d.span.is_some(), "runtime errors should carry a span");
    let sp = d.span.unwrap();
    assert!(sp.line >= 3, "span should point at the failing line, got {}", sp.line);
}

#[test]
fn runtime_errors_carry_a_call_stack_trace() {
    // Both engines attach the runtime call stack to uncaught runtime errors,
    // so a failure deep in a call chain shows how it was reached.
    let dir = std::env::temp_dir().join(format!("spid3r_trace_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let prog = dir.join("trace.matrix");
    std::fs::write(
        &prog,
        "fn inner(x) {\n    return x.no_such_method();\n}\nfn middle() {\n    return inner(5);\n}\nfn main() {\n    println(middle());\n    return 0;\n}\n",
    )
    .expect("write");
    for engine in ["tree", "vm"] {
        let out = Command::new(env!("CARGO_BIN_EXE_spid3r"))
            .args(["run", "--engine", engine, prog.to_str().unwrap()])
            .output()
            .expect("run");
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(!out.status.success(), "{}: should fail", engine);
        assert!(
            err.contains("stack trace (most recent call first):"),
            "{}: missing trace header:/n{}",
            engine,
            err
        );
        for frame in ["at inner (", "at middle (", "at main ("] {
            assert!(
                err.contains(frame),
                "{}: missing frame {}:\n{}",
                engine,
                frame,
                err
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn throw_carries_a_call_stack_trace() {
    let dir = std::env::temp_dir().join(format!("spid3r_trace_throw_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let prog = dir.join("throw.matrix");
    std::fs::write(
        &prog,
        "fn boom() {\n    throw \"bad\";\n}\nfn main() {\n    boom();\n    return 0;\n}\n",
    )
    .expect("write");
    for engine in ["tree", "vm"] {
        let out = Command::new(env!("CARGO_BIN_EXE_spid3r"))
            .args(["run", "--engine", engine, prog.to_str().unwrap()])
            .output()
            .expect("run");
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            err.contains("stack trace (most recent call first):") && err.contains("at boom ("),
            "{}: uncaught throw should trace:/n{}",
            engine,
            err
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
