//! Section 3.2 (Numbers & data types): checked integer arithmetic plus the
//! bigint / decimal / complex libraries. Everything runs on BOTH engines.

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::modules;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::vm::Vm;
use std::sync::Arc;

fn run_tree(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("numbers.matrix"))?;
    semantic::check_file(&program, true, "numbers.matrix")?;
    let interp = Interp::new(Arc::from("numbers.matrix"), modules::default_module_dirs());
    interp.run_program(&program)
}

fn run_vm(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("numbers.matrix"))?;
    semantic::check_file(&program, true, "numbers.matrix")?;
    let dirs = modules::default_module_dirs();
    let mut vm = Vm::new(Arc::from("numbers.matrix"), dirs);
    vm.run_program(&program)
}

fn both(src: &str) {
    assert_eq!(run_tree(src).unwrap(), 0, "tree engine");
    assert_eq!(run_vm(src).unwrap(), 0, "vm engine");
}

#[test]
fn integer_overflow_raises_on_both_engines() {
    let src = r#"
fn main() {
    let big = 9223372036854775807;
    let failed = false;
    try {
        let x = big + 1;
    } catch (e) {
        failed = true;
    }
    assert_eq(failed, true);
    return 0;
}
"#;
    both(src);
}

const BIGINT: &str = r#"
import "bigint" as bi;

fn main() {
    let two = bi.from_int(2);
    assert_eq(bi.to_string(bi.pow(two, 64)), "18446744073709551616");
    assert_eq(bi.to_string(bi.pow(two, 128)), "340282366920938463463374607431768211456");
    let a = bi.from_string("123456789012345678901234567890");
    let b = bi.from_string("987654321098765432109876543210");
    assert_eq(
        bi.to_string(bi.mul(a, b)),
        "121932631137021795226185032733622923332237463801111263526900"
    );
    let qr = bi.divmod(bi.mul(a, b), b);
    assert_eq(bi.to_string(qr[0]), "123456789012345678901234567890");
    assert_eq(bi.to_string(qr[1]), "0");
    assert_eq(bi.to_string(bi.add(bi.from_int(-5), bi.from_int(3))), "-2");
    assert_eq(bi.cmp(a, b), -1);
    assert_eq(bi.cmp(a, a), 0);
    let f = bi.from_int(1);
    let i = 2;
    while i <= 25 {
        f = bi.mul(f, bi.from_int(i));
        i = i + 1;
    }
    assert_eq(bi.to_string(f), "15511210043330985984000000");
    return 0;
}
"#;

#[test]
fn bigint_is_arbitrary_precision_on_both_engines() {
    both(BIGINT);
}

/// Mixed operands and normalization of `neg`/`abs`.
///
/// `n * 2` used to reach `mul_magnitude` with a plain int and die inside the
/// library (`int does not support indexing`, pointing at bigint.matrix) —
/// the operator methods assumed both sides were bigints. `neg`/`abs` returned
/// a bare map without the operator methods, so `bi.neg(n) + 1` had no
/// `__add__` to dispatch to. Both are regressions, not new features.
const BIGINT_MIXED: &str = r#"
import "bigint" as bi;

fn main() {
    let n = 100n;
    assert_eq(bi.to_string(n * 2), "200");
    assert_eq(bi.to_string(n + 1), "101");
    assert_eq(bi.to_string(n - 1), "99");
    assert_eq(bi.to_string(n / 3), "33");
    assert_eq(bi.to_string(n + "41"), "141");
    assert_eq(bi.cmp(n, 100), 0);
    assert_eq(bi.to_string(bi.neg(n) + 1), "-99");
    assert_eq(bi.to_string(bi.abs(bi.from_int(-7))), "7");
    assert_eq(bi.to_string(bi.divmod(100n, 7)[0]), "14");
    assert_eq(bi.to_string(bi.divmod(100n, 7)[1]), "2");
    // There is no `__neg__` dispatch, so a negative value must be built.
    assert_eq(bi.to_string(bi.from_int(-7)), "-7");
    assert_eq(bi.to_string(0n - 7n), "-7");
    return 0;
}
"#;

#[test]
fn bigint_mixed_operands_on_both_engines() {
    both(BIGINT_MIXED);
}

const DECIMAL: &str = r#"
import "decimal" as dec;

fn main() {
    // The headline: 0.10 + 0.20 is exactly 0.30, not 0.30000000000000004.
    let sum = dec.add(dec.from_string("0.10"), dec.from_string("0.20"));
    assert_eq(dec.to_string(sum), "0.30");
    assert_eq(dec.to_string(dec.mul(dec.from_string("1.5"), dec.from_string("2.5"))), "3.75");
    assert_eq(dec.to_string(dec.div(dec.from_string("1"), dec.from_string("3"), 6)), "0.333333");
    assert_eq(dec.to_string(dec.round(dec.from_string("2.5"), 0)), "3");
    assert_eq(dec.to_string(dec.round(dec.from_string("2.4"), 0)), "2");
    assert_eq(dec.to_string(dec.round(dec.from_string("2.675"), 2)), "2.68");
    assert_eq(dec.cmp(dec.from_string("0.1"), dec.from_string("0.100")), 0);
    assert_eq(dec.to_string(dec.sub(dec.from_string("100"), dec.from_string("0.01"))), "99.99");
    return 0;
}
"#;

#[test]
fn decimal_is_exact_on_both_engines() {
    both(DECIMAL);
}

const COMPLEX: &str = r#"
import "complex" as cx;

fn main() {
    let z = cx.of(3, 4);
    assert_eq(cx.to_string(cx.mul(z, z)), "-7.0+24.0i");
    assert_eq(cx.abs(z), 5.0);
    assert_eq(cx.to_string(cx.div(cx.of(1, 2), cx.of(1, -2))), "-0.6+0.8i");
    assert_eq(cx.to_string(cx.conj(z)), "3.0-4.0i");
    assert_eq(cx.to_string(cx.add(cx.of(1, 1), cx.of(2, -3))), "3.0-2.0i");
    assert_eq(cx.arg(cx.of(0, 1)), 1.5707963267948966);
    return 0;
}
"#;

#[test]
fn complex_arithmetic_on_both_engines() {
    both(COMPLEX);
}

const MODULE_ISOLATION: &str = r#"
import "decimal" as dec;
import "complex" as cx;

fn main() {
    // decimal and complex both export `add`/`mul`/`div`; before module-scope
    // isolation the VM resolved one module's internal call to the other's
    // function (whichever loaded last) and the program failed with a
    // "missing key" error.
    assert_eq(dec.to_string(dec.add(dec.from_string("1.5"), dec.from_string("2.5"))), "4.0");
    assert_eq(cx.to_string(cx.mul(cx.of(1, 1), cx.of(1, 1))), "0.0+2.0i");
    return 0;
}
"#;

#[test]
fn two_modules_with_the_same_export_names_stay_isolated() {
    both(MODULE_ISOLATION);
}

const OPERATORS: &str = r#"
import "decimal" as dec;
import "bigint" as bi;
import "complex" as cx;

fn main() {
    // Operator overloading: the values carry __add__/__sub__/__mul__/__div__,
    // so the ordinary operators work on them.
    let a = dec.from_string("0.10");
    let b = dec.from_string("0.20");
    assert_eq(dec.to_string(a + b), "0.30");
    assert_eq(dec.to_string(dec.from_string("1.00") - a), "0.90");
    assert_eq(dec.to_string(dec.from_string("1.5") * dec.from_string("2.5")), "3.75");
    assert_eq(dec.to_string(dec.from_string("1") / dec.from_string("4")), "0.250000");

    let x = bi.from_int(6);
    let y = bi.from_int(7);
    assert_eq(bi.to_string(x * y), "42");
    assert_eq(bi.to_string(x + y), "13");
    assert_eq(bi.to_string(y / x), "1");

    let z1 = cx.of(1, 2);
    let z2 = cx.of(3, -1);
    assert_eq(cx.to_string(z1 + z2), "4.0+1.0i");
    assert_eq(cx.to_string(z1 * z2), "5.0+5.0i");

    // Values display as their numeric form (a precomputed __display__),
    // not as the internal map.
    assert_eq(string(a), "0.10");
    assert_eq(string(cx.of(1, 2)), "1.0+2.0i");
    return 0;
}
"#;

#[test]
fn operator_overloading_on_both_engines() {
    both(OPERATORS);
}

const LITERALS_AND_RATIONALS: &str = r#"
import "bigint" as bi;
import "rational" as q;

fn main() {
    // Big-integer literals: the digits do not fit an i64.
    let big = 123456789012345678901234567890n;
    assert_eq(string(big), "123456789012345678901234567890");
    assert_eq(bi.to_string(big), "123456789012345678901234567890");
    let two = 2n;
    assert_eq(string(two * two), "4");
    // A plain int literal is still an int.
    assert_eq(string(42 + 1), "43");

    // Exact rationals (Python's fractions.Fraction role).
    let a = q.of(1, 3);
    let b = q.of(1, 6);
    assert_eq(q.to_string(a + b), "1/2");
    assert_eq(q.to_string(a - b), "1/6");
    assert_eq(q.to_string(a * q.of(3, 2)), "1/2");
    assert_eq(q.to_string(a / b), "2");
    assert_eq(q.to_decimal(a, 6), "0.333333");
    assert_eq(q.to_string(q.from_string("1.25")), "5/4");
    assert_eq(q.to_string(q.of(4, 2)), "2");
    assert_eq(q.cmp(a, b), 1);
    return 0;
}
"#;

#[test]
fn bigint_literals_and_rationals_on_both_engines() {
    both(LITERALS_AND_RATIONALS);
}

const INTERP: &str = r#"
fn main() {
    let xs = [1, 2, 3];
    let name = "ada";
    // Rich holes: any expression, not just a name or a field path.
    assert_eq("len={len(xs)}", "len=3");
    assert_eq("sum={xs[0] + xs[1] + xs[2]}", "sum=6");
    assert_eq("upper={name.upper()}", "upper=ADA");
    assert_eq("cond={xs[0] < xs[2]}", "cond=true");
    assert_eq("nested={string(len(xs) * 2)}", "nested=6");
    // The old forms and template text are unchanged.
    assert_eq("plain {name} and {xs[0]}", "plain ada and 1");
    assert_eq("template: {{ user }} and { a }", "template: {{ user }} and { a }");
    return 0;
}
"#;

#[test]
fn rich_interpolation_on_both_engines() {
    both(INTERP);
}
