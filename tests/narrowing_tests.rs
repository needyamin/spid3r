//! Phase R6.3 (flow-sensitive nullability narrowing) and R6.4 (scrutinee-driven
//! exhaustiveness) tests.

use spid3r_lang::parser;
use spid3r_lang::semantic;
use std::sync::Arc;

fn check_ok(src: &str) {
    let prog = parser::parse_file(src, Arc::from("<test>")).expect("parse ok");
    if let Err(d) = semantic::check_file(&prog, true, "<test>") {
        panic!("expected check_ok, got error: {}\nsrc:\n{}", d.message, src);
    }
}

fn check_err(src: &str, needle: &str) {
    let prog = parser::parse_file(src, Arc::from("<test>")).expect("parse ok");
    match semantic::check_file(&prog, true, "<test>") {
        Ok(()) => panic!("expected semantic error containing {:?}, got Ok", needle),
        Err(d) => assert!(
            d.message.contains(needle),
            "expected error containing {:?}, got: {}",
            needle,
            d.message
        ),
    }
}

#[test]
fn if_and_early_return_null_guards_narrow_nullable_types() {
    // 1. `if x != null` narrows `x: int?` to `int` inside the taken branch.
    check_ok(
        r#"
fn need_int(n: int) -> int { return n + 1; }
fn test_branch(x: int?) -> int {
    if (x != null) {
        return need_int(x);
    } else {
        return 0;
    }
}
fn main() { return test_branch(5); }
"#,
    );

    // 2. `if not is_null(x)` narrows `x: string?` to `string`.
    check_ok(
        r#"
fn is_null(v) -> bool { return v == null; }
fn greet(s: string) -> string { return "hi " + s; }
fn test_is_null(s: string?) -> string {
    if (not is_null(s)) {
        return greet(s);
    }
    return "anon";
}
fn main() { println(test_is_null("ada")); return 0; }
"#,
    );

    // 3. Early return on `if x == null { return ...; }` narrows `x` in the rest of the block.
    check_ok(
        r#"
fn need_int(n: int) -> int { return n * 2; }
fn after_guard(x: int?) -> int {
    if (x == null) {
        return -1;
    }
    return need_int(x);
}
fn main() { return after_guard(10); }
"#,
    );

    // Without narrowing, passing `x: int?` directly to `need_int(n: int)` is rejected.
    check_err(
        r#"
fn need_int(n: int) -> int { return n; }
fn bad(x: int?) -> int {
    return need_int(x);
}
fn main() { return bad(1); }
"#,
        "expects int, got int?",
    );
}

#[test]
fn propagate_and_opt_field_and_while_and_catch_narrowing() {
    // Site 2 (`?` propagate) + Site 3 (`?.` optional field) + Site 5 (`while x != null`).
    check_ok(
        r#"
class Boxed {
    public let count: int;
    fn new(c: int) { self.count = c; }
}
fn unwrap_opt(x: option<int>) -> int {
    let v: int = x?;
    return v;
}
fn read_opt_field(b: Boxed?) -> int? {
    return b?.count;
}
fn read_narrowed_opt_field(b: Boxed?) -> int {
    if (b != null) {
        return b?.count;
    }
    return 0;
}
fn loop_narrow(x: int?) -> int {
    let acc: int = 0;
    while (x != null) {
        let step: int = x;
        acc = acc + step;
        x = null;
    }
    return acc;
}
fn main() {
    return loop_narrow(3);
}
"#,
    );

    // Site 4 (`catch` binder carries thrown error payload type).
    check_err(
        r#"
fn main() {
    try {
        throw "string-error";
    } catch (e) {
        let bad: int = e;
    }
    return 0;
}
"#,
        "type mismatch in 'bad': expected int, got string",
    );
}

#[test]
fn scrutinee_driven_exhaustiveness_catches_missing_variants() {
    // R6.4: scrutinee annotated `: Color` with a literal arm (previously ignored
    // by the arm-only heuristic!) is now caught as non-exhaustive.
    check_err(
        r#"
enum Color { Red, Green, Blue }
fn code_of(c: Color) -> int {
    return match (c) {
        0 => { 0 }
    };
}
fn main() { return 0; }
"#,
        "this match is not exhaustive for enum 'Color': missing 'Red', 'Green', 'Blue'",
    );

    // Missing one variant on a scrutinee-typed variable is caught.
    check_err(
        r#"
enum Color { Red, Green, Blue }
fn main() {
    let c = Red();
    let v = match (c) {
        Red => { 1 }
        Green => { 2 }
    };
    return v;
}
"#,
        "this match is not exhaustive for enum 'Color': missing 'Blue'",
    );

    // Wildcard `_` still satisfies exhaustiveness for an enum scrutinee.
    check_ok(
        r#"
enum Color { Red, Green, Blue }
fn code_of(c: Color) -> int {
    return match (c) {
        Red => { 1 }
        _ => { 0 }
    };
}
fn main() { return code_of(Red()); }
"#,
    );
}
