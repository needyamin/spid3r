//! Stage 4: the error model — `option`/`result` typing, `?` propagation, and
//! the combinators.
//!
//! The headline defect fixed here: `let x: option<int> = some(5)` used to be a
//! type error ("expected option, got some"), because `some` typed as its own
//! collection name. Every annotated `option`/`result` binding was unusable.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run(src: &str, tag: &str, engine: &str) -> (i32, String, String) {
    let dir = std::env::temp_dir().join("spid3r_stage4");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{}.matrix", tag));
    std::fs::write(&path, src).expect("write case");
    let o = Command::new(bin())
        .args(["run", "--engine", engine, path.to_str().unwrap()])
        .output()
        .expect("spawn spid3r");
    let _ = std::fs::remove_file(&path);
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

fn both(tag: &str, src: &str, expected: &str) {
    for engine in ["tree", "vm"] {
        let (code, out, err) = run(src, tag, engine);
        assert_eq!(code, 0, "[{} / {}] failed: {}", tag, engine, err);
        assert_eq!(out.trim_end(), expected, "[{} / {}] stdout", tag, engine);
    }
}

// ------------------------------------------------------------ option/result

#[test]
fn an_option_annotation_accepts_some_and_none() {
    both(
        "opt_annot",
        "fn main() {\n    let a: option<int> = some(5);\n    let b: option<int> = none();\n    println(a);\n    println(b);\n    println(a.is_some());\n    println(b.is_none());\n    return 0;\n}\n",
        "some(5)\nnone\ntrue\ntrue",
    );
}

#[test]
fn a_result_annotation_accepts_ok_and_err() {
    both(
        "res_annot",
        "fn main() {\n    let a: result<int> = ok(5);\n    let b: result<int> = err(\"bad\");\n    println(a);\n    println(b);\n    println(a.is_ok());\n    println(b.is_err());\n    return 0;\n}\n",
        "ok(5)\nerr(bad)\ntrue\ntrue",
    );
}

/// Element types are checked now, so an annotation actually says something.
#[test]
fn a_collection_annotation_checks_its_element_type() {
    let (code, _, err) = run(
        "fn main() {\n    let xs: list<int> = [\"a\"];\n    return 0;\n}\n",
        "elem_type",
        "tree",
    );
    assert_ne!(code, 0, "a wrong element type must be rejected");
    assert!(
        err.contains("list<int>") && err.contains("list<string>"),
        "the message should name both element types: {}",
        err
    );
    // The matching case, and an empty literal (unknown element), still pass.
    both(
        "elem_type_ok",
        "fn main() {\n    let xs: list<int> = [1, 2];\n    let empty: list<int> = [];\n    println(xs.len() + empty.len());\n    return 0;\n}\n",
        "2",
    );
}

// --------------------------------------------------------------- `?` operator

#[test]
fn question_propagates_an_err() {
    both(
        "prop_err",
        "fn half(n) {\n    if (n % 2 != 0) {\n        return err(\"odd\");\n    }\n    return ok(n / 2);\n}\nfn quarter(n) {\n    let h = half(n)?;\n    return ok(h / 2);\n}\nfn main() {\n    println(quarter(8));\n    println(quarter(3));\n    return 0;\n}\n",
        "ok(2)\nerr(odd)",
    );
}

#[test]
fn question_propagates_a_none() {
    both(
        "prop_none",
        "fn first(xs) {\n    if (xs.len() == 0) {\n        return none();\n    }\n    return some(xs[0]);\n}\nfn head_plus(xs) {\n    let v = first(xs)?;\n    return some(v + 1);\n}\nfn main() {\n    println(head_plus([5]));\n    println(head_plus([]));\n    return 0;\n}\n",
        "some(6)\nnone",
    );
}

/// `?` returns from the *enclosing function*, so a caller sees the failure and
/// nothing after the `?` runs.
#[test]
fn question_returns_from_the_enclosing_function() {
    both(
        "prop_return",
        "fn f(r) {\n    let v = r?;\n    println(\"not reached\");\n    return ok(v);\n}\nfn main() {\n    println(f(ok(1)));\n    println(f(err(\"no\")));\n    return 0;\n}\n",
        // `not reached` prints once (the ok call), never for the err call.
        "not reached\nok(1)\nerr(no)",
    );
}

#[test]
fn question_on_a_non_option_is_a_clean_error() {
    for engine in ["tree", "vm"] {
        let (code, _, err) = run(
            "fn main() {\n    let x = 5?;\n    return 0;\n}\n",
            "prop_bad",
            engine,
        );
        assert_eq!(code, 1, "[{}] should exit 1", engine);
        assert!(
            err.contains("is_failure expects an option or a result"),
            "[{}] unexpected error: {}",
            engine,
            err
        );
    }
}

// --------------------------------------------------------------- combinators

#[test]
fn option_combinators_chain() {
    both(
        "opt_combo",
        "fn main() {\n    println(some(2).map(fn(x) { return x * 10; }));\n    println(none().map(fn(x) { return x * 10; }));\n    println(some(4).and_then(fn(x) { return some(x + 1); }));\n    println(none().and_then(fn(x) { return some(x + 1); }));\n    println(none().ok_or(\"missing\"));\n    println(some(7).ok_or(\"missing\"));\n    println(none().unwrap_or_else(fn() { return 0; }));\n    println(some(some(9)).flatten());\n    return 0;\n}\n",
        "some(20)\nnone\nsome(5)\nnone\nerr(missing)\nok(7)\n0\nsome(9)",
    );
}

#[test]
fn result_combinators_chain() {
    both(
        "res_combo",
        "fn main() {\n    println(ok(3).map(fn(x) { return x + 1; }));\n    println(err(\"e\").map(fn(x) { return x + 1; }));\n    println(err(\"e\").map_err(fn(e) { return \"E:\" + e; }));\n    println(ok(3).map_err(fn(e) { return \"E:\" + e; }));\n    println(ok(3).and_then(fn(x) { return ok(x * 2); }));\n    println(err(\"x\").unwrap_or_else(fn(e) { return \"recovered from \" + e; }));\n    println(ok(ok(5)).flatten());\n    return 0;\n}\n",
        "ok(4)\nerr(e)\nerr(E:e)\nok(3)\nok(6)\nrecovered from x\nok(5)",
    );
}

/// A combinator's callback runs once, and only on the payload side.
#[test]
fn a_combinator_does_not_call_back_on_the_failure_side() {
    both(
        "combo_lazy",
        "fn side(n) {\n    println(\"side{n}\");\n    return n;\n}\nfn main() {\n    println(none().map(fn(x) { return side(x); }));\n    println(some(1).map(fn(x) { return side(x); }));\n    return 0;\n}\n",
        "none\nside1\nsome(1)",
    );
}

/// The predicate `?` lowers to is a normal builtin.
#[test]
fn is_failure_reports_the_failure_side() {
    both(
        "is_failure",
        "fn main() {\n    println(is_failure(none()));\n    println(is_failure(some(1)));\n    println(is_failure(err(\"e\")));\n    println(is_failure(ok(1)));\n    return 0;\n}\n",
        "true\nfalse\ntrue\nfalse",
    );
}
