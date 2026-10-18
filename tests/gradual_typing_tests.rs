//! Gradual typing (PH-4): `spid3r check` enforces `: Type` annotations at
//! compile time only. Unannotated names stay dynamically typed (`Any`), so
//! existing programs are unaffected.

use std::process::Command;

fn check(src: &str, tag: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!("spid3r_ty_{}_{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let p = dir.join("prog.matrix");
    std::fs::write(&p, src).expect("write");
    let o = Command::new(env!("CARGO_BIN_EXE_spid3r"))
        .args(["check", p.to_str().unwrap()])
        .output()
        .expect("spawn");
    let _ = std::fs::remove_dir_all(&dir);
    (
        o.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

#[test]
fn valid_annotations_pass() {
    let src = "\
fn main() {
    let x: int = 5;
    let s: string = \"hi\";
    let f: float = 1.5;
    let b: bool = true;
    let xs: list = list(1, 2, 3);
    println(x);
    return 0;
}
";
    let (code, out) = check(src, "valid");
    assert_eq!(code, 0, "unexpected failure: {}", out);
    assert!(out.contains("ok"), "{}", out);
}

#[test]
fn let_mismatch_is_rejected() {
    let src = "\
fn main() {
    let x: int = \"not an int\";
    return 0;
}
";
    let (code, out) = check(src, "letmis");
    assert_ne!(code, 0);
    assert!(out.contains("type mismatch"), "{}", out);
    assert!(out.contains("expected int"), "{}", out);
}

#[test]
fn const_mismatch_is_rejected() {
    let src = "\
const x: int = \"nope\";
";
    let (code, out) = check(src, "constmis");
    assert_ne!(code, 0);
    assert!(out.contains("type mismatch"), "{}", out);
}

#[test]
fn argument_mismatch_is_rejected() {
    let src = "\
fn f(x: int) { }
f(\"string\");
";
    let (code, out) = check(src, "argmis");
    assert_ne!(code, 0);
    assert!(out.contains("argument 1"), "{}", out);
    assert!(out.contains("expects int"), "{}", out);
}

#[test]
fn return_mismatch_is_rejected() {
    let src = "\
fn f() -> int {
    return \"string\";
}
";
    let (code, out) = check(src, "retmis");
    assert_ne!(code, 0);
    assert!(out.contains("return type mismatch"), "{}", out);
    assert!(out.contains("expected int"), "{}", out);
}

#[test]
fn reassignment_mismatch_is_rejected() {
    let src = "\
fn g() {
    let x: int = 5;
    x = \"now a string\";
}
";
    let (code, out) = check(src, "reassign");
    assert_ne!(code, 0);
    assert!(out.contains("type mismatch assigning to 'x'"), "{}", out);
}

#[test]
fn field_assignment_mismatch_is_rejected() {
    let src = "\
class C {
    let v: int;
    fn set(self) {
        self.v = \"string\";
    }
}
";
    let (code, out) = check(src, "fieldmis");
    assert_ne!(code, 0);
    assert!(out.contains("type mismatch assigning to field 'v'"), "{}", out);
}

#[test]
fn annotated_field_read_has_type() {
    // A correctly typed field assignment must pass.
    let src = "\
class C {
    let v: int;
    fn set(self) {
        self.v = 7;
    }
}
fn g() {
    let c: C = new C();
    c.v = 42;
}
";
    let (code, out) = check(src, "fieldok");
    assert_eq!(code, 0, "unexpected failure: {}", out);
}

#[test]
fn subtype_assignment_is_allowed() {
    let src = "\
class Animal { }
class Dog extends Animal { }
fn g() {
    let a: Animal = new Dog();
    let d: Dog = new Animal();
}
";
    let (ok, out_ok) = check(
        "class Animal { }\nclass Dog extends Animal { }\nfn g() { let a: Animal = new Dog(); }\n",
        "subok",
    );
    assert_eq!(ok, 0, "subtype assignment wrongly rejected: {}", out_ok);

    let (bad, out_bad) = check(src, "subbad");
    assert_ne!(bad, 0, "subtype-violating assignment must be rejected");
    assert!(out_bad.contains("type mismatch"), "{}", out_bad);
}

#[test]
fn unannotated_stays_dynamic() {
    // An unannotated binding is `Any`, so assigning it to an annotated target
    // must not be flagged (gradual typing: dynamic values are accepted).
    let src = "\
fn g() {
    let x = 5;
    let y: int = x;
    let z: any = \"anything\";
    let w: any = 3.5;
}
";
    let (code, out) = check(src, "dynamic");
    assert_eq!(code, 0, "dynamic value wrongly rejected: {}", out);
}

#[test]
fn numeric_widening_int_to_float() {
    let src = "\
fn g() {
    let f: float = 5;
}
";
    let (code, out) = check(src, "widen");
    assert_eq!(code, 0, "int->float widening wrongly rejected: {}", out);
}

#[test]
fn unknown_type_name_is_rejected() {
    let src = "\
fn g() {
    let x: NotAType = 5;
}
";
    let (code, out) = check(src, "unknownty");
    assert_ne!(code, 0);
    assert!(out.contains("unknown type"), "{}", out);
}

#[test]
fn method_argument_mismatch_is_rejected() {
    let src = "\
class C {
    public fn m(self, x: int) { }
}
fn g() {
    let c: C = new C();
    c.m(\"string\");
}
";
    let (code, out) = check(src, "metharg");
    assert_ne!(code, 0);
    assert!(out.contains("argument 1 of method 'm'"), "{}", out);
    assert!(out.contains("expects int"), "{}", out);
}
