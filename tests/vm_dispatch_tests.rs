//! VM method-dispatch tests: the `Invoke` inline cache must be correct for
//! polymorphic call sites, inherited methods, and closures created in a loop
//! (whose chunks are created and dropped repeatedly, exercising the cache's
//! chunk pinning). Runs the real binary so this covers the shipped path.

use std::process::Command;

fn run_vm(tag: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("spid3r_vmd_{}_{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let p = dir.join("prog.matrix");
    std::fs::write(&p, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_spid3r"))
        .args(["run", "--engine", "vm", p.to_str().unwrap()])
        .output()
        .expect("run");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "vm run failed for {}:\n{}\n{}",
        tag,
        stdout,
        stderr
    );
    stdout
}

#[test]
fn polymorphic_call_site_dispatches_to_the_right_class() {
    // One call site, two classes: the cache must not stick to the first one.
    let src = "\
class A { fn new() { } public fn tag(self) -> string { return \"A\"; } }
class B { fn new() { } public fn tag(self) -> string { return \"B\"; } }
fn describe(o) { return o.tag(); }
fn main() {
    let a = new A();
    let b = new B();
    println(describe(a));
    println(describe(b));
    println(describe(a));
    println(describe(b));
    return 0;
}
";
    let out = run_vm("poly", src);
    assert_eq!(out, "A\nB\nA\nB\n", "polymorphic dispatch: {}", out);
}

#[test]
fn inherited_methods_resolve_through_the_parent_chain() {
    let src = "\
class Base {
    public let n;
    fn new(n) { self.n = n; }
    public fn twice(self) -> int { return self.n * 2; }
}
class Child extends Base {
    fn new(n) { self.n = n; }
}
fn call_twice(o) { return o.twice(); }
fn main() {
    let b = new Base(3);
    let c = new Child(10);
    println(call_twice(b));
    println(call_twice(c));
    return 0;
}
";
    let out = run_vm("inherit", src);
    assert_eq!(out, "6\n20\n", "inherited dispatch: {}", out);
}

#[test]
fn closures_created_in_a_loop_keep_dispatching_correctly() {
    // Each iteration creates a fresh closure (fresh chunk). The cache keys on
    // chunk identity, so this exercises cache entry churn and the chunk pin
    // that keeps addresses from being reused.
    let src = "\
class Counter {
    public let n;
    fn new() { self.n = 0; }
    public fn bump(self) -> int { self.n = self.n + 1; return self.n; }
}
fn main() {
    let c = new Counter();
    let total = 0;
    let i = 0;
    while i < 50 {
        let f = fn() { return c.bump(); };
        total = total + f();
        i = i + 1;
    }
    println(total);
    println(c.n);
    return 0;
}
";
    let out = run_vm("closures", src);
    assert_eq!(out, "1275\n50\n", "closure churn: {}", out);
}

#[test]
fn two_methods_on_one_class_at_different_sites_are_cached_separately() {
    let src = "\
class Box {
    public let v;
    fn new(v) { self.v = v; }
    public fn get(self) -> int { return self.v; }
    public fn double(self) -> int { return self.v * 2; }
}
fn main() {
    let b = new Box(7);
    let i = 0;
    while i < 3 {
        println(b.get());
        println(b.double());
        i = i + 1;
    }
    return 0;
}
";
    let out = run_vm("sites", src);
    assert_eq!(out, "7\n14\n7\n14\n7\n14\n", "per-site caching: {}", out);
}

#[test]
fn vm_dispatch_matches_the_tree_interpreter() {
    // The reference engine has no inline cache; outputs must agree.
    let src = "\
class Animal {
    public let name;
    fn new(n) { self.name = n; }
    public fn speak(self) -> string { return \"...\"; }
}
class Dog extends Animal {
    public let name;
    fn new(n) { self.name = n; }
    public fn speak(self) -> string { return \"woof\"; }
}
fn main() {
    let xs = list(new Animal(\"a\"), new Dog(\"d\"));
    let i = 0;
    while i < 2 {
        let o = xs.get(i);
        println(o.speak());
        i = i + 1;
    }
    return 0;
}
";
    let dir = std::env::temp_dir().join(format!("spid3r_vmd_par_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let p = dir.join("prog.matrix");
    std::fs::write(&p, src).expect("write");
    let out = |engine: &str| {
        let o = Command::new(env!("CARGO_BIN_EXE_spid3r"))
            .args(["run", "--engine", engine, p.to_str().unwrap()])
            .output()
            .expect("run");
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).to_string(),
        )
    };
    let tree = out("tree");
    let vm = out("vm");
    assert_eq!(tree, vm, "tree vs vm dispatch differ");
    assert!(vm.1.contains("woof"), "expected dispatch output: {}", vm.1);
    let _ = std::fs::remove_dir_all(&dir);
}
