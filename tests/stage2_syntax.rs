//! Stage-2 syntax: string interpolation and bitwise operators.
//!
//! Both are desugared in the parser into constructs the engines already handle
//! (a `+` chain of `string(...)` calls, and calls to stdlib natives), so there
//! is one implementation rather than three. These tests pin the surface syntax
//! and the compatibility rules; `tests/engine_matrix.rs` covers engine parity.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run(src: &str, tag: &str, engine: &str) -> (i32, String, String) {
    let dir = std::env::temp_dir().join("spid3r_stage2_syntax");
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

/// Both engines, same expectation.
fn both(tag: &str, src: &str, expected: &str) {
    for engine in ["tree", "vm"] {
        let (code, out, err) = run(src, tag, engine);
        assert_eq!(code, 0, "[{} / {}] failed: {}", tag, engine, err);
        assert_eq!(out.trim_end(), expected, "[{} / {}] stdout", tag, engine);
    }
}

// ------------------------------------------------------------ interpolation

#[test]
fn a_hole_interpolates_a_variable_and_a_field_path() {
    both(
        "interp_basic",
        "class U {\n    public let name;\n    fn new(n) { self.name = n; }\n}\nfn main() {\n    let who = \"ada\";\n    let u = new U(\"bob\");\n    println(\"hi {who}\");\n    println(\"hi {u.name}\");\n    return 0;\n}\n",
        "hi ada\nhi bob",
    );
}

#[test]
fn a_hole_converts_non_strings_without_an_explicit_call() {
    // The desugaring wraps every hole in `string(...)`, which is idempotent on
    // strings and formats ints/floats/bools.
    both(
        "interp_types",
        "fn main() {\n    let n = 36;\n    let f = 1.5;\n    let b = true;\n    println(\"{n} {f} {b}\");\n    return 0;\n}\n",
        "36 1.5 true",
    );
}

#[test]
fn several_holes_and_adjacent_holes_concatenate_in_order() {
    both(
        "interp_multi",
        "fn main() {\n    let a = 1;\n    let b = 2;\n    println(\"{a}-{b}-{a}\");\n    println(\"{a}{b}\");\n    return 0;\n}\n",
        "1-2-1\n12",
    );
}

/// The stdlib's own template engine writes `"{{ user }}"` at run time. A hole
/// requires an identifier immediately after `{`, so braces followed by `{` or a
/// space stay literal — that is what makes interpolation additive.
#[test]
fn template_braces_stay_literal() {
    both(
        "interp_template",
        "fn main() {\n    println(\"{{ user }}\");\n    println(\"{{{ key }}}\");\n    println(\"{not a hole}\");\n    println(\"{}\");\n    println(\"a}b{\");\n    return 0;\n}\n",
        "{{ user }}\n{{{ key }}}\n{not a hole}\n{}\na}b{",
    );
}

#[test]
fn an_unknown_name_in_a_hole_is_a_semantic_error() {
    let (code, _, err) = run(
        "fn main() {\n    println(\"x {ghost} y\");\n    return 0;\n}\n",
        "interp_unknown",
        "tree",
    );
    assert_ne!(code, 0, "an unknown hole name must fail the check");
    assert!(
        err.contains("unknown name 'ghost'"),
        "the error should name the missing binding: {}",
        err
    );
}

// ---------------------------------------------------------------- bitwise

#[test]
fn bitwise_operators_compute_the_expected_values() {
    both(
        "bit_values",
        "fn main() {\n    println(12 & 10);\n    println(12 | 10);\n    println(12 ^ 10);\n    println(1 << 4);\n    println(256 >> 3);\n    println(-8 >> 1);\n    return 0;\n}\n",
        // -8 >> 1 is an arithmetic shift: -4, not a large positive number.
        "8\n14\n6\n16\n32\n-4",
    );
}

#[test]
fn bitwise_precedence_follows_c() {
    both(
        "bit_prec",
        "fn main() {\n    println(1 << 2 + 1);\n    println((6 & 3) == 2);\n    println(1 | 2 ^ 3 & 4);\n    println(1 | 2 & 3);\n    return 0;\n}\n",
        // `1 << (2+1)`; `(6&3)==2`; `1 | (2 ^ (3&4))`; `1 | (2&3)`.
        "8\ntrue\n3\n3",
    );
}

/// `==` binds tighter than `&`, so `6 & 3 == 2` is `6 & (3 == 2)` — a bool
/// operand, which the single implementation rejects. This is the C reading, and
/// the error is a clean runtime diagnostic rather than a panic.
#[test]
fn a_non_int_operand_is_a_clean_error() {
    for engine in ["tree", "vm"] {
        let (code, _, err) = run(
            "fn main() {\n    println(6 & 3 == 2);\n    return 0;\n}\n",
            "bit_bool",
            engine,
        );
        assert_eq!(code, 1, "[{}] should exit 1", engine);
        assert!(
            err.contains("bit_and expects int operands, found bool"),
            "[{}] unexpected error: {}",
            engine,
            err
        );
    }
}

/// An out-of-range shift is an error, not a panic (debug) or a silent wrap
/// (release) — Rust's `<<` does both depending on the profile.
#[test]
fn an_out_of_range_shift_is_an_error() {
    both(
        "bit_shift_ok",
        "fn main() {\n    println(1 << 63);\n    return 0;\n}\n",
        "-9223372036854775808",
    );
    for expr in ["1 << 64", "1 >> -1"] {
        let src = format!("fn main() {{\n    println({});\n    return 0;\n}}\n", expr);
        let (code, _, err) = run(&src, "bit_shift_bad", "tree");
        assert_eq!(code, 1, "`{}` should be a clean error", expr);
        assert!(
            err.contains("out of range"),
            "`{}` should report the range: {}",
            expr,
            err
        );
    }
}

/// The desugaring targets are ordinary natives, so they are also callable
/// directly — which is how the VM and AOT backends run them unchanged.
#[test]
fn the_desugar_targets_are_plain_natives() {
    both(
        "bit_natives",
        "fn main() {\n    println(bit_and(12, 10));\n    println(bit_or(12, 10));\n    println(bit_xor(12, 10));\n    println(shl(1, 4));\n    println(shr(256, 3));\n    return 0;\n}\n",
        "8\n14\n6\n16\n32",
    );
}
