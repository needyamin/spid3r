//! `super` support (2026-10-21 audit, F-1 — the audit's highest-severity
//! language gap).
//!
//! Semantics (both engines, byte-identical):
//! * `super(args)` inside a constructor runs the parent class's constructor
//!   on the **same** instance — the instance already exists, it is being
//!   initialized.
//! * `super.m(args)` calls the parent class's implementation of `m` with the
//!   current instance as the receiver. The lookup starts at the *defining*
//!   class's parent, so a subclass that does not override the method still
//!   reaches the grandparent implementation instead of recursing.
//!
//! Errors are semantic and identical on both engines: `super` outside a
//! class, in a parentless class, against a missing method, or with the wrong
//! argument count.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run_src(engine: &str, name: &str, src: &str) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!("spid3r_super_{}.matrix", name));
    std::fs::write(&path, src).map_err(|e| format!("write {}: {}", path.display(), e))?;
    let o = Command::new(bin())
        .args(["run", "--engine", engine, path.to_str().unwrap()])
        .output()
        .map_err(|e| format!("spawn failed: {}", e))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).to_string())
    }
}

fn assert_all_engines(name: &str, src: &str, expected: &str) {
    for engine in ["tree", "vm"] {
        let out = run_src(engine, name, src)
            .unwrap_or_else(|e| panic!("[{}] {} run failed:\n{}", name, engine, e));
        assert_eq!(out, expected, "[{}] {} output", name, engine);
    }
    // The compiled artifact must behave identically too.
    let path = std::env::temp_dir().join(format!("spid3r_super_{}.matrix", name));
    let tc = std::env::temp_dir().join(format!("spid3r_super_{}.matrixc", name));
    let comp = Command::new(bin())
        .args(["compile", path.to_str().unwrap(), "-o", tc.to_str().unwrap()])
        .output()
        .expect("compile");
    assert!(
        comp.status.success(),
        "[{}] compile failed: {}",
        name,
        String::from_utf8_lossy(&comp.stderr)
    );
    let exec = Command::new(bin())
        .args(["exec", tc.to_str().unwrap()])
        .output()
        .expect("exec");
    assert!(
        exec.status.success(),
        "[{}] exec failed: {}",
        name,
        String::from_utf8_lossy(&exec.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&exec.stdout).to_string(),
        expected,
        "[{}] exec output",
        name
    );
}

fn assert_semantic_error(name: &str, src: &str, needle: &str) {
    for engine in ["tree", "vm"] {
        let err = match run_src(engine, name, src) {
            Ok(out) => panic!("[{}] {} unexpectedly succeeded:\n{}", name, engine, out),
            Err(e) => e,
        };
        assert!(
            err.contains(needle) && err.contains("error[semantic]"),
            "[{}] {} expected a semantic error containing {:?}, got:\n{}",
            name,
            engine,
            needle,
            err
        );
    }
}

const HIERARCHY: &str = r#"
class Animal {
    public let name;
    public let sound;
    public fn new(name, sound) { self.name = name; self.sound = sound; }
    public fn speak(self) { return self.name + " says " + self.sound; }
}
class Dog extends Animal {
    public let tricks;
    public fn new(name) { super(name, "woof"); self.tricks = 0; }
    public fn speak(self) { return super.speak() + " (" + string(self.tricks) + " tricks)"; }
}
"#;

#[test]
fn super_constructor_initializes_the_same_instance() {
    assert_all_engines(
        "super_ctor",
        &format!(
            "{}\nfn main() {{\n    let d = new Dog(\"rex\");\n    println(d.name);\n    println(d.sound);\n    println(d.tricks);\n    return 0;\n}}\n",
            HIERARCHY
        ),
        "rex\nwoof\n0\n",
    );
}

#[test]
fn super_method_delegates_to_the_parent_implementation() {
    assert_all_engines(
        "super_method",
        &format!(
            "{}\nfn main() {{\n    let d = new Dog(\"rex\");\n    println(d.speak());\n    return 0;\n}}\n",
            HIERARCHY
        ),
        "rex says woof (0 tricks)\n",
    );
}

#[test]
fn super_chain_reaches_the_grandparent_through_an_unoverridden_method() {
    // `Puppy` does not override `speak`, so `Dog`'s body runs with a Puppy
    // receiver; its `super.speak()` must still find `Animal`'s method — not
    // recurse into `Dog`'s.
    assert_all_engines(
        "super_grandparent",
        &format!(
            "{}\nclass Puppy extends Dog {{\n    public fn new(name) {{ super(name); self.tricks = 1; }}\n}}\nfn main() {{\n    let p = new Puppy(\"bit\");\n    println(p.speak());\n    println(p.tricks);\n    return 0;\n}}\n",
            HIERARCHY
        ),
        "bit says woof (1 tricks)\n1\n",
    );
}

#[test]
fn super_outside_a_class_is_rejected() {
    assert_semantic_error(
        "super_outside",
        "fn main() {\n    super();\n    return 0;\n}\n",
        "super is only allowed inside a class method",
    );
}

#[test]
fn super_in_a_parentless_class_is_rejected() {
    assert_semantic_error(
        "super_parentless",
        "class A {\n    public fn new() { }\n    public fn hi(self) { return super.hi(); }\n}\nfn main() { return 0; }\n",
        "class 'A' has no parent class: super is not available here",
    );
}

#[test]
fn super_missing_method_is_rejected() {
    assert_semantic_error(
        "super_missing_method",
        "class A { public fn new() { } }\nclass B extends A {\n    public fn hi(self) { return super.missing(); }\n}\nfn main() { return 0; }\n",
        "class 'A' has no method 'missing'",
    );
}

#[test]
fn super_wrong_argument_count_is_rejected() {
    assert_semantic_error(
        "super_arity",
        "class A { public fn new(n) { } }\nclass B extends A {\n    public fn new() { super(); }\n}\nfn main() { return 0; }\n",
        "constructor A::new expects 1 argument(s), got 0",
    );
}

#[test]
fn super_method_arity_is_checked() {
    assert_semantic_error(
        "super_method_arity",
        "class A { public fn new() { } public fn f(self, x) { return x; } }\nclass B extends A {\n    public fn g(self) { return super.f(); }\n}\nfn main() { return 0; }\n",
        "method A::f expects 1 argument(s), got 0",
    );
}
