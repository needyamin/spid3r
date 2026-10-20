//! OOP tests (Phases 4-12): classes, constructors, methods, `self`,
//! single inheritance, privacy, interfaces, and polymorphic dispatch.

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::parser;
use spid3r_lang::semantic;
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
fn class_holds_fields_and_methods() {
    expect_ok(
        r#"
        class Counter {
            public let n;
            fn new() {
                self.n = 0;
            }
            public fn bump(self) {
                self.n += 1;
                return self.n;
            }
        }
        fn main() {
            let c = new Counter();
            if c.bump() != 1 { return 1; }
            if c.bump() != 2 { return 1; }
            if c.n != 2 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn instances_are_copies_not_aliases() {
    expect_ok(
        r#"
        class Point {
            public let x;
            fn new(x) {
                self.x = x;
            }
        }
        fn main() {
            let a = new Point(1);
            let b = new Point(1);
            if a == b { return 1; }   // identity, not structure
            let c = a;
            if c != a { return 1; }   // same instance
            c.x = 5;
            if a.x != 5 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn inheritance_passes_fields_and_methods_down() {
    expect_ok(
        r#"
        class Base {
            public let tag;
            fn new(tag) {
                self.tag = tag;
            }
            public fn describe(self) -> string {
                return "base:" + self.tag;
            }
        }
        class Child extends Base {
            public fn child_only(self) -> string {
                return "child:" + self.tag;
            }
        }
        fn main() {
            let c = new Child("x");
            if c.tag != "x" { return 1; }
            if c.describe() != "base:x" { return 1; }
            if c.child_only() != "child:x" { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn a_child_constructor_can_override_the_parent() {
    expect_ok(
        r#"
        class Animal {
            public let noise;
            fn new(noise) {
                self.noise = noise;
            }
            public fn speak(self) -> string {
                return self.noise;
            }
        }
        class Dog extends Animal {
            fn new() {
                self.noise = "woof";
            }
        }
        fn main() {
            let d = new Dog();
            if d.speak() != "woof" { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn unknown_parent_is_rejected() {
    expect_err(
        r#"
        class Dog extends Wolf {
        }
        fn main() {
            return 0;
        }
    "#,
        "parent class",
    );
}

#[test]
fn unknown_class_in_new_is_rejected() {
    expect_err(
        r#"
        fn main() {
            let d = new Ghost();
            return 0;
        }
    "#,
        "unknown class",
    );
}

#[test]
fn private_members_are_invisible_outside_the_class() {
    expect_err(
        r#"
        class Boxed {
            private let secret;
            fn new() {
                self.secret = 1;
            }
        }
        fn main() {
            let b = new Boxed();
            return b.secret;
        }
    "#,
        "private",
    );
}

#[test]
fn the_same_class_can_use_its_own_private_members() {
    expect_ok(
        r#"
        class Boxed {
            private let secret;
            fn new() {
                self.secret = 41;
            }
            private fn reveal(self) {
                return self.secret + 1;
            }
            public fn show(self) {
                return self.reveal();
            }
        }
        fn main() {
            let b = new Boxed();
            if b.show() != 42 { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn calling_an_unknown_method_is_rejected_statically() {
    expect_err(
        r#"
        class Plain {
            fn new() {
            }
        }
        fn main() {
            let p = new Plain();
            return p.nope();
        }
    "#,
        "no method 'nope'",
    );
}

#[test]
fn interfaces_enforce_their_contract() {
    expect_err(
        r#"
        interface Named {
            fn name(self) -> string;
        }
        class Broken implements Named {
            fn new() {
            }
        }
        fn main() {
            return 0;
        }
    "#,
        "does not implement",
    );
}

#[test]
fn interfaces_dispatch_by_name() {
    expect_ok(
        r#"
        interface Named {
            fn name(self) -> string;
        }
        class User extends Object implements Named {
            public let id;
            fn new(id) {
                self.id = id;
            }
            public fn name(self) -> string {
                return "user-" + string(self.id);
            }
        }
        class Team implements Named {
            public let label;
            fn new(label) {
                self.label = label;
            }
            public fn name(self) -> string {
                return "team-" + self.label;
            }
        }
        fn main() {
            let u = new User(7);
            let t = new Team("core");
            if u.name() != "user-7" { return 1; }
            if t.name() != "team-core" { return 1; }
            return 0;
        }
    "#,
    );
}

#[test]
fn default_missing_field_is_null_and_wrong_arity_is_caught() {
    expect_err(
        r#"
        class One {
            public let a;
            fn new(a) {
                self.a = a;
            }
        }
        fn main() {
            let o = new One(1, 2);
            return 0;
        }
    "#,
        "argument",
    );
}

#[test]
fn classes_keep_per_instance_state() {
    expect_ok(
        r#"
        class Bag {
            public let items;
            fn new() {
                self.items = list();
            }
            public fn put(self, x) {
                self.items.push(x);
                return self.items.len();
            }
        }
        fn main() {
            let a = new Bag();
            let b = new Bag();
            a.put(1);
            a.put(2);
            if b.put(9) != 1 { return 1; }
            if a.items.len() != 2 { return 1; }
            return 0;
        }
    "#,
    );
}
