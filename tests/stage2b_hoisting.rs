//! Stage-2b: `if` as an expression, `?.` and `??` — the surface features that
//! need statement hoisting.
//!
//! All three are rewritten by `src/lower` into a temporary plus statements, so
//! no backend knows they exist. The interesting properties are therefore
//! *semantic*: short-circuit, evaluation order, and the value of a branch.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run(src: &str, tag: &str, engine: &str) -> (i32, String, String) {
    let dir = std::env::temp_dir().join("spid3r_stage2b");
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

// ------------------------------------------------------------ if-expression

#[test]
fn an_if_expression_yields_its_branch_value() {
    both(
        "ifexpr_value",
        "fn sign(n) {\n    return if (n < 0) { \"neg\" } else { \"nonneg\" };\n}\nfn main() {\n    println(sign(-1));\n    println(sign(0));\n    let x = if (1 < 2) { 10 } else { 20 };\n    println(x);\n    return 0;\n}\n",
        "neg\nnonneg\n10",
    );
}

#[test]
fn a_missing_else_yields_null() {
    both(
        "ifexpr_no_else",
        "fn main() {\n    let x = if (false) { 1 };\n    println(x);\n    println(x == null);\n    return 0;\n}\n",
        "null\ntrue",
    );
}

/// The branch's value is its last expression, and earlier statements in the
/// branch still run.
#[test]
fn a_branch_may_do_work_before_its_value() {
    both(
        "ifexpr_work",
        "fn main() {\n    let x = if (true) {\n        let y = 2;\n        y * 3\n    } else {\n        0\n    };\n    println(x);\n    return 0;\n}\n",
        "6",
    );
}

#[test]
fn else_if_chains_nest() {
    both(
        "ifexpr_chain",
        "fn f(n) {\n    return if (n > 2) { \"big\" } else if (n > 0) { \"small\" } else { \"none\" };\n}\nfn main() {\n    println(f(5));\n    println(f(1));\n    println(f(-1));\n    return 0;\n}\n",
        "big\nsmall\nnone",
    );
}

/// `return` inside a branch leaves the enclosing function, not the branch.
#[test]
fn return_inside_a_branch_returns_from_the_function() {
    both(
        "ifexpr_return",
        "fn f(n) {\n    let x = if (n > 0) { return 99; } else { 1 };\n    return x + 1000;\n}\nfn main() {\n    println(f(1));\n    println(f(-1));\n    return 0;\n}\n",
        "99\n1001",
    );
}

#[test]
fn a_branch_without_a_value_is_a_semantic_error() {
    let (code, _, err) = run(
        "fn main() {\n    let x = if (true) { let y = 1; } else { 2 };\n    return 0;\n}\n",
        "ifexpr_novalue",
        "tree",
    );
    assert_ne!(code, 0);
    assert!(
        err.contains("must end with an expression"),
        "unexpected error: {}",
        err
    );
}

// ------------------------------------------------------------------ coalesce

#[test]
fn coalesce_falls_back_only_on_null() {
    both(
        "coalesce_basic",
        "fn main() {\n    println(null ?? \"d\");\n    println(\"v\" ?? \"d\");\n    println(0 ?? 9);\n    println(false ?? true);\n    println(null ?? null ?? \"last\");\n    return 0;\n}\n",
        // `0` and `false` are not null, so they win.
        "d\nv\n0\nfalse\nlast",
    );
}

/// The whole point of `??` over a function call: the right side must not run
/// when the left is present.
#[test]
fn coalesce_does_not_evaluate_the_fallback_when_the_left_is_present() {
    both(
        "coalesce_lazy",
        "fn boom() {\n    println(\"boom\");\n    return 1;\n}\nfn main() {\n    println(\"v\" ?? boom());\n    println(null ?? boom());\n    return 0;\n}\n",
        "v\nboom\n1",
    );
}

// -------------------------------------------------------------- opt. chaining

#[test]
fn optional_field_returns_null_for_a_null_receiver() {
    both(
        "opt_basic",
        "fn main() {\n    let m = map();\n    m[\"k\"] = 7;\n    println(m?.k);\n    let z = null;\n    println(z?.k);\n    return 0;\n}\n",
        "7\nnull",
    );
}

/// The receiver is evaluated exactly once, even when it has side effects.
#[test]
fn optional_field_evaluates_its_receiver_once() {
    both(
        "opt_once",
        "fn make() {\n    println(\"make\");\n    let m = map();\n    m[\"k\"] = 1;\n    return m;\n}\nfn main() {\n    println(make()?.k);\n    return 0;\n}\n",
        "make\n1",
    );
}

#[test]
fn optional_field_chains_with_coalesce() {
    both(
        "opt_coalesce",
        "class Box {\n    public let v;\n    fn new(v) { self.v = v; }\n}\nfn main() {\n    let b = new Box(null);\n    println(b?.v ?? \"empty\");\n    let n = null;\n    println(n?.v ?? \"empty\");\n    return 0;\n}\n",
        "empty\nempty",
    );
}

// ------------------------------------------------------- evaluation order

/// Hoisting must not reorder side effects: an operand written before the
/// hoisting expression still runs first.
#[test]
fn hoisting_preserves_left_to_right_evaluation() {
    both(
        "hoist_order",
        "fn side(n) {\n    println(\"s{n}\");\n    return n;\n}\nfn main() {\n    println(side(1) + (if (true) { side(2) } else { 0 }));\n    println(side(9), (if (true) { side(8) } else { 0 }));\n    return 0;\n}\n",
        "s1\ns2\n3\ns9\ns8\n9 8",
    );
}

/// `and`/`or` still short-circuit when one side needs a prelude.
#[test]
fn logical_operators_still_short_circuit_when_hoisting() {
    both(
        "hoist_logic",
        "fn side(n) {\n    println(\"s{n}\");\n    return n;\n}\nfn main() {\n    println(false and (if (true) { side(1) } else { 0 }));\n    println(true or (if (true) { side(2) } else { 0 }));\n    println(true and (if (true) { side(3) } else { 0 }));\n    return 0;\n}\n",
        "false\ntrue\ns3\n3",
    );
}

/// A `while` condition is re-evaluated on every iteration, so its prelude has to
/// move inside the loop body.
#[test]
fn a_loop_condition_with_a_prelude_runs_each_iteration() {
    both(
        "hoist_loop",
        "fn main() {\n    let m = map();\n    m[\"n\"] = 0;\n    while (m?.n < 3) {\n        m[\"n\"] = m[\"n\"] + 1;\n        println(m[\"n\"]);\n    }\n    return 0;\n}\n",
        "1\n2\n3",
    );
}
