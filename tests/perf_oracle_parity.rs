//! Oracle parity for the VM's runtime fusions.
//!
//! The VM fuses a handful of opcode shapes at runtime (`GetLocal a; GetLocal b;
//! <op>; SetLocal d; Pop`, `GetLocal a; Constant k; <op>; SetLocal d; Pop`, and
//! the `Less` / `Mul` / `Mod` condition peeks). Those paths skip the generic
//! handlers, so they must reproduce them *exactly* — including the cases where
//! the generic handler raises an error instead of producing a value.
//!
//! The oracle is `--no-opt`, which turns the compile-time optimiser and the
//! runtime peeks off together, leaving plain bytecode dispatch. Every case here
//! is run three ways (default VM, `--no-opt` VM, tree) and must agree on stdout,
//! stderr and exit code byte for byte. A fused arm that quietly wraps where the
//! generic path errors — or that skips a fall-through — fails here rather than
//! in a user's program.
//!
//! Deliberately absent: any assertion on timing. This file is about meaning.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

struct Outcome {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(engine: &str, extra: &[&str], path: &Path) -> Outcome {
    let o = Command::new(bin())
        .arg("run")
        .arg("--engine")
        .arg(engine)
        .args(extra)
        .arg(path.to_str().unwrap())
        .output()
        .expect("spawn spid3r");
    Outcome {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).to_string(),
        stderr: String::from_utf8_lossy(&o.stderr).to_string(),
    }
}

fn scratch(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("spid3r_perf_oracle_parity");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{name}.matrix"));
    std::fs::write(&path, src).expect("write case");
    path
}

/// Run one program on the fused path, the unfused path and the tree engine, and
/// require all three to agree on everything observable.
fn assert_parity(name: &str, src: &str) -> Outcome {
    let path = scratch(name, src);
    let fused = run("vm", &[], &path);
    let plain = run("vm", &["--no-opt"], &path);
    assert_eq!(
        fused.code, plain.code,
        "[{name}] exit code: fused {} vs --no-opt {}",
        fused.code, plain.code
    );
    assert_eq!(fused.stdout, plain.stdout, "[{name}] stdout vs --no-opt");
    assert_eq!(fused.stderr, plain.stderr, "[{name}] stderr vs --no-opt");

    let tree = run("tree", &[], &path);
    assert_eq!(fused.code, tree.code, "[{name}] exit code: vm vs tree");
    assert_eq!(fused.stdout, tree.stdout, "[{name}] stdout: vm vs tree");
    fused
}

fn assert_runtime_error(name: &str, src: &str, needle: &str) {
    let out = assert_parity(name, src);
    assert_ne!(out.code, 0, "[{name}] expected a non-zero exit");
    assert!(
        out.stderr.contains("error[runtime]"),
        "[{name}] expected a runtime error, got: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains(needle),
        "[{name}] expected {needle:?} in: {}",
        out.stderr
    );
}

// ---------------------------------------------------------------------------
// Happy paths: the fused arms must produce the value the generic handlers do.
// ---------------------------------------------------------------------------

/// `GetLocal a; GetLocal b; <op>; SetLocal a; Pop` for all five operators.
#[test]
fn local_op_local_arithmetic_matches_the_oracle() {
    let out = assert_parity(
        "local_local_ops",
        concat!(
            "fn main() {\n",
            "    let a = 100;\n",
            "    let b = 7;\n",
            "    a = a - b;\n",
            "    println(a);\n",
            "    a = a * b;\n",
            "    println(a);\n",
            "    a = a / b;\n",
            "    println(a);\n",
            "    a = a % b;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
    );
    assert_eq!(out.code, 0);
    assert_eq!(out.stdout.replace("\r\n", "\n"), "93\n651\n93\n2\n");
}

/// `GetLocal a; Constant k; <op>; SetLocal a; Pop` for all five operators.
#[test]
fn local_op_const_arithmetic_matches_the_oracle() {
    let out = assert_parity(
        "local_const_ops",
        concat!(
            "fn main() {\n",
            "    let a = 100;\n",
            "    a = a - 7;\n",
            "    println(a);\n",
            "    a = a * 7;\n",
            "    println(a);\n",
            "    a = a / 7;\n",
            "    println(a);\n",
            "    a = a % 7;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
    );
    assert_eq!(out.code, 0);
    assert_eq!(out.stdout.replace("\r\n", "\n"), "93\n651\n93\n2\n");
}

/// The string append arm lives in the same fused block as the integer ops; a
/// generalisation that broke it would show up here.
#[test]
fn string_append_still_uses_the_in_place_buffer() {
    let out = assert_parity(
        "string_append",
        concat!(
            "fn main() {\n",
            "    let s = \"\";\n",
            "    let i = 0;\n",
            "    while i < 200 {\n",
            "        s = s + \"ab\";\n",
            "        i = i + 1;\n",
            "    }\n",
            "    println(s.len());\n",
            "    return 0;\n",
            "}\n",
        ),
    );
    assert_eq!(out.code, 0);
    assert_eq!(out.stdout.replace("\r\n", "\n"), "400\n");
}

// ---------------------------------------------------------------------------
// Fall-through paths: checked_* returning None must reach the generic handler,
// which raises the exact same error. This is the property a fused arm is most
// likely to break, by wrapping where the interpreter errors.
// ---------------------------------------------------------------------------

#[test]
fn add_overflow_falls_through_to_the_generic_error() {
    assert_runtime_error(
        "add_overflow",
        concat!(
            "fn main() {\n",
            "    let a = 9223372036854775807;\n",
            "    let b = 1;\n",
            "    a = a + b;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
        "integer overflow",
    );
}

#[test]
fn sub_overflow_falls_through_to_the_generic_error() {
    assert_runtime_error(
        "sub_overflow",
        concat!(
            "fn main() {\n",
            "    let a = 0 - 9223372036854775807 - 1;\n",
            "    a = a - 1;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
        "integer overflow",
    );
}

#[test]
fn mul_overflow_falls_through_to_the_generic_error() {
    assert_runtime_error(
        "mul_overflow",
        concat!(
            "fn main() {\n",
            "    let a = 9223372036854775807;\n",
            "    a = a * 2;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
        "integer overflow",
    );
}

/// Same overflows, but with BOTH operands in locals, which selects the
/// `GetLocal; GetLocal; <op>; SetLocal; Pop` arm rather than the
/// `GetLocal; Constant; <op>; SetLocal; Pop` one. Without these cases a fused
/// arm that swapped `checked_sub` for `wrapping_sub` passed the whole file.
#[test]
fn overflow_through_the_local_operand_arm_also_falls_through() {
    assert_runtime_error(
        "sub_overflow_local_operand",
        concat!(
            "fn main() {\n",
            "    let a = 0 - 9223372036854775807 - 1;\n",
            "    let b = 1;\n",
            "    a = a - b;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
        "integer overflow",
    );
    assert_runtime_error(
        "mul_overflow_local_operand",
        concat!(
            "fn main() {\n",
            "    let a = 9223372036854775807;\n",
            "    let b = 2;\n",
            "    a = a * b;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
        "integer overflow",
    );
}

#[test]
fn division_by_zero_falls_through_to_the_generic_error() {
    assert_runtime_error(
        "div_by_zero_local",
        concat!(
            "fn main() {\n",
            "    let z = 0;\n",
            "    let a = 10;\n",
            "    a = a / z;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
        "division by zero",
    );
    assert_runtime_error(
        "div_by_zero_const",
        concat!(
            "fn main() {\n",
            "    let a = 10;\n",
            "    a = a / 0;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
        "division by zero",
    );
}

#[test]
fn modulo_by_zero_falls_through_to_the_generic_error() {
    assert_runtime_error(
        "mod_by_zero_local",
        concat!(
            "fn main() {\n",
            "    let z = 0;\n",
            "    let a = 10;\n",
            "    a = a % z;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
        "modulo by zero",
    );
    assert_runtime_error(
        "mod_by_zero_const",
        concat!(
            "fn main() {\n",
            "    let a = 10;\n",
            "    a = a % 0;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
        "modulo by zero",
    );
}

/// `i64::MIN / -1` overflows; `i64::MIN % -1` is defined and is 0. The two must
/// not be treated the same: `%` uses `wrapping_rem`, `/` uses `checked_div`.
#[test]
fn i64_min_div_neg_one_overflows_like_the_generic_path() {
    assert_runtime_error(
        "min_div_neg_one",
        concat!(
            "fn main() {\n",
            "    let m = 0 - 9223372036854775807 - 1;\n",
            "    let d = 0 - 1;\n",
            "    let r = 0;\n",
            "    r = m / d;\n",
            "    println(r);\n",
            "    return 0;\n",
            "}\n",
        ),
        "integer overflow",
    );
}

#[test]
fn i64_min_mod_neg_one_wraps_like_the_generic_path() {
    let out = assert_parity(
        "min_mod_neg_one",
        concat!(
            "fn main() {\n",
            "    let m = 0 - 9223372036854775807 - 1;\n",
            "    let d = 0 - 1;\n",
            "    let r = 7;\n",
            "    r = m % d;\n",
            "    println(r);\n",
            "    return 0;\n",
            "}\n",
        ),
    );
    assert_eq!(out.code, 0);
    assert_eq!(out.stdout.replace("\r\n", "\n"), "0\n");
}

// ---------------------------------------------------------------------------
// Observers: the fusions are gated off under --prof / --coverage / the
// debugger, so the same program must produce the same answer under them.
// ---------------------------------------------------------------------------

/// A captured upvalue disables every fusion in the frame (`captured` must be
/// empty for the peek to run), because a slot backed by a shared cell cannot
/// be read straight out of `slots`. The answer must not change either way.
#[test]
fn captured_upvalue_disables_fusion_without_changing_the_answer() {
    let out = assert_parity(
        "captured_upvalue",
        concat!(
            "fn main() {\n",
            "    let n = 500;\n",
            "    let total = 0;\n",
            "    let i = 0;\n",
            "    while i < n {\n",
            "        let add = fn(x) { return x + i; };\n",
            "        total = total + add(i);\n",
            "        i = i + 1;\n",
            "    }\n",
            "    println(total);\n",
            "    return 0;\n",
            "}\n",
        ),
    );
    assert_eq!(out.code, 0);
    // sum over i in [0, 500) of (i + i) = 2 * (499 * 500 / 2) = 249500
    assert_eq!(out.stdout.replace("\r\n", "\n"), "249500\n");
}

#[test]
fn instrumentation_flags_do_not_change_the_answer() {
    let src = concat!(
        "fn main() {\n",
        "    let a = 100;\n",
        "    let b = 7;\n",
        "    a = a - b;\n",
        "    a = a * b;\n",
        "    a = a / b;\n",
        "    a = a % b;\n",
        "    println(a);\n",
        "    return 0;\n",
        "}\n",
    );
    let path = scratch("instrumented_ops", src);
    let base = run("vm", &[], &path);
    for extra in [&["--prof"][..], &["--coverage"][..], &["--no-opt"][..]] {
        let other = run("vm", extra, &path);
        assert_eq!(base.code, other.code, "exit code with {extra:?}");
        assert_eq!(base.stdout, other.stdout, "stdout with {extra:?}");
    }
}
