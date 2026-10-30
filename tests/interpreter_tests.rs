//! Interpreter tests (Phase 4-12): values, arithmetic rules, control flow,
//! functions and closures, plus the standard library builtins.

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use std::sync::Arc;

/// Run a program that must contain `fn main`; returns main's exit code.
fn run(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("t.matrix"))?;
    semantic::check_file(&program, true, "t.matrix")?;
    let interp = Interp::new(Arc::from("t.matrix"), vec![]);
    interp.run_program(&program)
}

/// Capture stdout of a program via print/println redirection is not available;
/// instead assert observable results through main's exit code.
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
        Ok(code) => panic!("expected failure, exited with {}", code),
    }
}

#[test]
fn hello_world_runs() {
    expect_ok(
        r#"
        fn main() {
            println("hello, spid3r");
            return 0;
        }
    "#,
    );
}

#[test]
fn arithmetic_promotes_ints_to_floats() {
    expect_ok(
        r#"
        fn main() {
            let a = 3 + 0.5;
            if a != 3.5 { return 1; }
            let b = 10 / 4;
            if b != 2.5 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn integer_overflow_is_an_error_not_a_silent_wrap() {
    // The audit flagged silent wraparound as data corruption: `max * 2` used
    // to become -2. It is a runtime error now (Python raises; PHP converts to
    // float - nobody silently corrupts).
    expect_err(
        r#"
        fn main() {
            let big = 9223372036854775807;
            let wrapped = big * 2;
            return 0;
        }
    "#,
        "integer overflow",
    );
    expect_err(
        r#"
        fn main() {
            let big = 9223372036854775807;
            let wrapped = big + 1;
            return 0;
        }
    "#,
        "integer overflow",
    );
}

#[test]
fn division_by_zero_throws() {
    expect_err(
        r#"
        fn main() {
            let x = 1 / 0;
            return 0;
        }
    "#,
        "division by zero",
    );
}

#[test]
fn number_plus_string_is_an_error() {
    expect_err(
        r#"
        fn main() {
            let x = "n=" + 5;
            return 0;
        }
    "#,
        "cannot add",
    );
}

#[test]
fn string_concat_works() {
    expect_ok(
        r#"
        fn main() {
            let s = "spid" + "3r";
            if s != "spid3r" { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn truthiness_rules() {
    expect_ok(
        r#"
        fn main() {
            if (0 or "" or null or false) { return 1; }
            if (not (1 and "x")) { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn while_and_for_c_style_loops() {
    expect_ok(
        r#"
        fn main() {
            let i = 0;
            while i < 5 {
                i += 1;
            }
            if i != 5 { return 1; }
            let sum = 0;
            for let j = 0; j < 10; j += 1 {
                sum += j;
            }
            if sum != 45 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn for_in_iterates_a_list() {
    expect_ok(
        r#"
        fn main() {
            let xs = list(1, 2, 3);
            let sum = 0;
            for x in xs {
                sum += x;
            }
            if sum != 6 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn break_and_continue_work() {
    expect_ok(
        r#"
        fn main() {
            let n = 0;
            for let i = 0; i < 100; i += 1 {
                if i == 3 { continue; }
                if i == 7 { break; }
                n += i;
            }
            // skips 3, stops before 7: 0+1+2+4+5+6
            if n != 18 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn functions_recursion_and_early_return() {
    expect_ok(
        r#"
        fn fact(n) {
            if n <= 1 { return 1; }
            return n * fact(n - 1);
        }
        fn main() {
            if fact(5) != 120 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn closures_capture_defining_scope() {
    expect_ok(
        r#"
        fn make_adder(n) {
            return fn(x) { return x + n; };
        }
        fn main() {
            let add10 = make_adder(10);
            if add10(5) != 15 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn builtin_conversions_and_math() {
    expect_ok(
        r#"
        fn main() {
            if int("42") != 42 { return 1; }
            if float(1) != 1.0 { return 1; }
            if abs(-7) != 7 { return 1; }
            if sqrt(16.0) != 4.0 { return 1; }
            if pow(2, 10) != 1024 { return 1; }
            if min(3, 9) != 3 { return 1; }
            if max(3, 9) != 9 { return 1; }
            if string(7) != "7" { return 1; }
            if bool(0) != false { return 1; }
            if type_of(1) != "int" { return 1; }
            if type_of(list()) != "list" { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn len_works_on_collections_and_strings() {
    expect_ok(
        r#"
        fn main() {
            if len("abcd") != 4 { return 1; }
            if len(list(1, 2, 3)) != 3 { return 1; }
            if len(map("a", 1, "b", 2)) != 2 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn const_cannot_be_reassigned() {
    expect_err(
        r#"
        const k = 1;
        fn main() {
            k = 2;
            return 0;
        }
    "#,
        "const",
    );
}

#[test]
fn main_is_required_for_programs() {
    expect_err(
        r#"
        fn helper() { return 1; }
    "#,
        "fn main",
    );
}

#[test]
fn unknown_name_is_rejected() {
    expect_err(
        r#"
        fn main() {
            return missing_name();
        }
    "#,
        "unknown name",
    );
}

#[test]
fn recursive_depth_guard_stops_infinite_recursion() {
    expect_err(
        r#"
        fn boom(n) {
            return boom(n + 1) + 1;
        }
        fn main() {
            return boom(0);
        }
    "#,
        "depth",
    );
}
