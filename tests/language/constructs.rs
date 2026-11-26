//! Section 3.3 constructs: decorators, abstract classes, erased generics, and
//! compiler-level `yield`. Every case runs on BOTH engines.

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::modules;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::vm::Vm;
use std::sync::Arc;

fn run_tree(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("constructs.matrix"))?;
    semantic::check_file(&program, true, "constructs.matrix")?;
    let interp = Interp::new(
        Arc::from("constructs.matrix"),
        modules::default_module_dirs(),
    );
    interp.run_program(&program)
}

fn run_vm(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("constructs.matrix"))?;
    semantic::check_file(&program, true, "constructs.matrix")?;
    let dirs = modules::default_module_dirs();
    let mut vm = Vm::new(Arc::from("constructs.matrix"), dirs);
    vm.run_program(&program)
}

fn both(src: &str) {
    assert_eq!(run_tree(src).unwrap(), 0, "tree engine");
    assert_eq!(run_vm(src).unwrap(), 0, "vm engine");
}

const DECORATORS: &str = r#"
fn memo(f) {
    let cache = map();
    return fn(x) {
        if cache.has(x) { return cache[x]; }
        let v = f(x);
        cache[x] = v;
        return v;
    };
}

fn trace(f) {
    return fn(x) { return f(x) + 100; };
}

@memo
fn square(n) { return n * n; }

@trace
@memo
fn cube(n) { return n * n * n; }

fn main() {
    assert_eq(square(5), 25);
    assert_eq(square(5), 25);
    // Bottom-up: trace(memo(cube)) -> memo result plus 100.
    assert_eq(cube(2), 108);
    return 0;
}
"#;

#[test]
fn decorators_on_both_engines() {
    both(DECORATORS);
}

const ABSTRACT: &str = r#"
abstract class Shape {
    abstract fn area(self);
    public fn describe(self) {
        return "area " + string(self.area());
    }
}

class Square extends Shape {
    public let side;
    fn new(side) { self.side = side; }
    public fn area(self) { return self.side * self.side; }
}

fn main() {
    let s = new Square(4);
    assert_eq(s.describe(), "area 16");
    return 0;
}
"#;

#[test]
fn abstract_classes_on_both_engines() {
    both(ABSTRACT);
}

#[test]
fn abstract_class_cannot_be_instantiated() {
    let src = r#"
abstract class Shape {
    abstract fn area(self);
}
fn main() {
    let s = new Shape();
    return 0;
}
"#;
    let err = run_tree(src).unwrap_err().message;
    assert!(
        err.contains("cannot instantiate the abstract class"),
        "{}",
        err
    );
    let err = run_vm(src).unwrap_err().message;
    assert!(
        err.contains("cannot instantiate the abstract class"),
        "{}",
        err
    );
}

#[test]
fn subclass_must_implement_abstract_methods() {
    let src = r#"
abstract class Shape {
    abstract fn area(self);
}
class Bad extends Shape {
    public fn nothing(self) { return 1; }
}
fn main() {
    let b = new Bad();
    return 0;
}
"#;
    let err = run_tree(src).unwrap_err().message;
    assert!(
        err.contains("must implement the abstract method 'area'"),
        "{}",
        err
    );
    let err = run_vm(src).unwrap_err().message;
    assert!(
        err.contains("must implement the abstract method 'area'"),
        "{}",
        err
    );
}

const GENERICS: &str = r#"
class Box<T> {
    public let value;
    fn new(value) { self.value = value; }
    public fn get(self) { return self.value; }
}

fn first<T>(xs) { return xs[0]; }

fn identity<T>(x) { return x; }

// An erased-generic return annotation was once rejected with "unknown type 'T'":
// the parameter name is captured by the parser but the checker must put it in
// scope for the body and the annotation to resolve. (Regression guard.)
fn head<T>(xs) -> T { return xs[0]; }

fn main() {
    let b = new Box(7);
    assert_eq(b.get(), 7);
    assert_eq(first([3, 4, 5]), 3);
    assert_eq(identity("x"), "x");
    assert_eq(head([9, 8, 7]), 9);
    return 0;
}
"#;

#[test]
fn erased_generics_on_both_engines() {
    both(GENERICS);
}

const YIELD: &str = r#"
import "gen" as g;

fn count(n) {
    let i = 0;
    while i < n {
        yield i;
        i = i + 1;
    }
}

fn fib() {
    let a = 0;
    let b = 1;
    while true {
        yield a;
        let t = a + b;
        a = b;
        b = t;
    }
}

fn main() {
    assert_eq(g.collect(count(4)), [0, 1, 2, 3]);
    // An infinite generator, consumed lazily.
    assert_eq(g.take(fib(), 6), [0, 1, 1, 2, 3, 5]);
    let it = count(2);
    assert_eq(g.next(it), 0);
    assert_eq(g.next(it), 1);
    assert_eq(g.next(it), null);
    return 0;
}
"#;

#[test]
fn yield_generators_on_both_engines() {
    both(YIELD);
}
