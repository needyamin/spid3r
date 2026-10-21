//! Language-surface regression corpus (2026-10-21 external audit).
//!
//! Every case runs on **all three engines** — the tree-walking interpreter
//! (the reference), the bytecode VM, and the compiled `.matrixc` artifact —
//! and all three must produce the *same expected stdout*. Asserting the
//! expected text (not just "the engines agree") matters: a program that fails
//! identically on every engine would otherwise pass an equality-only
//! differential.
//!
//! The corpus covers the newest language surface, which is where engine
//! divergence historically hid: string interpolation, bitwise operators,
//! null-coalescing, optional chaining, destructuring, `for (k, v)`, match and
//! enums, the option/result error model, `elif` chains, negative indexing,
//! `try`/`catch`/`finally` control flow, tasks and mutexes, C-style `for`,
//! closures, float literals, JSON round-trips, class privacy, module privacy,
//! and the string/list method families.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

/// Run `src` on `engine`; returns stdout, or the error text when the program
/// fails.
fn run_src(engine: &str, name: &str, src: &str) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!("spid3r_surface_{}.matrix", name));
    std::fs::write(&path, src).map_err(|e| format!("write {}: {}", path.display(), e))?;
    let o = Command::new(bin())
        .args(["run", "--engine", engine, path.to_str().unwrap()])
        .output()
        .map_err(|e| format!("spawn failed: {}", e))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(format!(
            "{} exit {}: {}",
            engine,
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stderr)
        ))
    }
}

/// Assert a program produces `expected` on the tree engine, the VM, and the
/// compiled artifact.
fn assert_all_engines(name: &str, src: &str, expected: &str) {
    let tree =
        run_src("tree", name, src).unwrap_or_else(|e| panic!("[{}] tree run failed:\n{}", name, e));
    let vm =
        run_src("vm", name, src).unwrap_or_else(|e| panic!("[{}] vm run failed:\n{}", name, e));
    assert_eq!(tree, expected, "[{}] tree output", name);
    assert_eq!(vm, expected, "[{}] vm output (diverged from tree)", name);

    let path = std::env::temp_dir().join(format!("spid3r_surface_{}.matrix", name));
    let tc = std::env::temp_dir().join(format!("spid3r_surface_{}.matrixc", name));
    let comp = Command::new(bin())
        .args([
            "compile",
            path.to_str().unwrap(),
            "-o",
            tc.to_str().unwrap(),
        ])
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
    let exec_out = String::from_utf8_lossy(&exec.stdout).to_string();
    assert_eq!(exec_out, expected, "[{}] exec output (diverged)", name);
}

// ------------------------------------------------------------ interpolation

#[test]
fn interpolation_holes_and_literal_braces() {
    // A hole is `{ident}` or `{obj.field}` with no whitespace; anything else
    // that starts with `{` stays literal text.
    assert_all_engines(
        "interp",
        r#"fn main() {
    let x = 5;
    let obj = map();
    obj.set("k", 42);
    println("{x}");
    println("v={x}!");
    println("{x}{x}");
    println("{obj.k}");
    println("{{ user }}");
    println("{}");
    return 0;
}
"#,
        "5\nv=5!\n55\n42\n{{ user }}\n{}\n",
    );
}

#[test]
fn interpolation_hole_is_an_ordinary_expression() {
    // The hole desugars to a string concatenation, so the checker sees it.
    assert_all_engines(
        "interp_expr",
        r#"fn main() {
    let n = 3;
    let xs = [1, 2, 3];
    println("n={n} xs={xs}");
    return 0;
}
"#,
        "n=3 xs=[1, 2, 3]\n",
    );
}

// ----------------------------------------------------------------- bitwise

#[test]
fn bitwise_precedence_and_shift_bounds() {
    // C precedence: `|` < `^` < `&` < comparison < shift < additive.
    assert_all_engines(
        "bitwise",
        r#"fn main() {
    println(1 << 2 + 3);
    println(4 | 3 ^ 5 & 1);
    println(16 >> 1 == 8);
    println(1 ^ 3);
    try { println(1 << 64); } catch(e) { println("shift-too-big"); }
    try { println(1 << -1); } catch(e) { println("shift-negative"); }
    return 0;
}
"#,
        "32\n6\ntrue\n2\nshift-too-big\nshift-negative\n",
    );
}

// -------------------------------------------------------------- coalescing

#[test]
fn null_coalescing_is_null_only() {
    // `??` is null-coalescing (PHP semantics): `none` is an option value, not
    // null, so it is returned unchanged.
    assert_all_engines(
        "coalesce",
        r#"fn main() {
    println(null ?? 5);
    println(none ?? 5);
    println(1 ?? 5);
    let n = null;
    println(n ?? "fallback");
    return 0;
}
"#,
        "5\nnone\n1\nfallback\n",
    );
}

#[test]
fn optional_chaining_on_null_is_safe() {
    assert_all_engines(
        "optchain",
        r#"fn main() {
    let obj = map();
    obj.set("k", 7);
    let maybe = null;
    println(maybe?.k ?? -1);
    println(obj.k);
    return 0;
}
"#,
        "-1\n7\n",
    );
}

// ----------------------------------------------------------- destructuring

#[test]
fn destructuring_let_and_closure_capture() {
    assert_all_engines(
        "destructure",
        r#"fn pair() { return [1, 2]; }
fn main() {
    let (a, b) = pair();
    println(a + b);
    let (c, d) = [10, 20, 30];
    println(c);
    println(d);
    let (e, f) = [7, 8];
    let closure = fn() { return e + f; };
    println(closure());
    try { let (g, h) = [1]; println(g); } catch(e2) { println("too-few"); }
    return 0;
}
"#,
        "3\n10\n20\n15\ntoo-few\n",
    );
}

// ------------------------------------------------------------- for-loops

#[test]
fn for_in_over_list_and_map() {
    assert_all_engines(
        "for_in",
        r#"fn main() {
    let total = 0;
    for i in [1, 2, 3] { total += i; }
    println(total);
    let m1 = map();
    m1.set("a", 1);
    m1.set("b", 2);
    let sum = 0;
    for (k, v) in m1 { sum += v; }
    println(sum);
    for (k, v) in m1 { println(k + "=" + string(v)); }
    return 0;
}
"#,
        "6\n3\na=1\nb=2\n",
    );
}

#[test]
fn c_style_for_with_continue_and_break() {
    assert_all_engines(
        "c_for",
        r#"fn main() {
    let s = 0;
    for let i = 0; i < 5; i += 1 {
        if (i == 2) { continue; }
        if (i == 4) { break; }
        s += i;
    }
    println(s);
    return 0;
}
"#,
        "4\n",
    );
}

// ------------------------------------------------------------------ match

#[test]
fn match_guards_fall_through_and_exhaustiveness() {
    assert_all_engines(
        "match",
        r#"enum Shape { Circle(r), Rect(w, h), Point }
fn area(s) {
    return match (s) {
        Circle(r) => 3 * r * r,
        Rect(w, h) if w == h => "square " + string(w),
        Rect(w, h) => w * h,
        Point => 0,
    };
}
fn main() {
    println(area(Circle(2)));
    println(area(Rect(3, 3)));
    println(area(Rect(3, 4)));
    println(area(Point()));
    let x = 5;
    println(match (x) { 5 => "five", _ => "other" });
    try { println(match (x) { 1 => "one" }); } catch(e) { println("no-match"); }
    return 0;
}
"#,
        "12\nsquare 3\n12\n0\nfive\nno-match\n",
    );
}

// ------------------------------------------------------- option / result

#[test]
fn option_result_combinators_and_propagation() {
    assert_all_engines(
        "option",
        r#"fn find(x) { if (x > 0) { return some(x); } return none; }
fn main() {
    println(find(5).unwrap());
    println(find(-1).is_none());
    println(find(3).map(fn(v) { return v * 2; }).unwrap());
    println(find(-1).unwrap_or(0));
    println(ok(1).and_then(fn(v) { return ok(v + 1); }).unwrap());
    println(err("boom").is_err());
    fn inner() { let v = find(-1)?; return v; }
    println(string(inner()));
    return 0;
}
"#,
        "5\ntrue\n6\n0\n2\ntrue\nnone\n",
    );
}

// --------------------------------------------------------------- elif/else

#[test]
fn elif_chains_static_and_dynamic() {
    // Regression: the VM optimizer used to mis-fold static-condition elif
    // chains (see tests/audit_regressions.rs::opt_static_elif_*).
    assert_all_engines(
        "elif",
        r#"fn grade(n) {
    if (n >= 90) { return "A"; } else if (n >= 80) { return "B"; } elif (n >= 70) { return "C"; } else { return "F"; }
}
fn main() {
    println(grade(95));
    println(grade(85));
    println(grade(75));
    println(grade(3));
    if (false) { println("no"); } elif (true) { println("yes"); }
    if (1 > 2) { println("a"); } elif (1 < 2) { println("b"); } else { println("c"); }
    return 0;
}
"#,
        "A\nB\nC\nF\nyes\nb\n",
    );
}

// ------------------------------------------------------- negative indexing

#[test]
fn negative_indexing_on_list_string_and_tuple() {
    assert_all_engines(
        "negindex",
        r#"fn main() {
    let xs = [1, 2, 3];
    println(xs[-1]);
    println(xs[-3]);
    xs[-1] = 9;
    println(xs[2]);
    println("abc"[-1]);
    let t = tuple(1, 2, 3);
    println(t[-2]);
    try { println(xs[-4]); } catch(e) { println("out-of-range"); }
    return 0;
}
"#,
        "3\n1\n9\nc\n2\nout-of-range\n",
    );
}

// --------------------------------------------------------- try / finally

#[test]
fn finally_runs_on_return_break_and_continue() {
    assert_all_engines(
        "tryflow",
        r#"fn f() {
    try {
        return 1;
    } finally {
        println("fin");
    }
}
fn main() {
    println(f());
    for i in [1, 2, 3] {
        try {
            if (i == 2) { continue; }
            if (i == 3) { break; }
            println(i);
        } finally {
            println("F" + string(i));
        }
    }
    try { throw "x"; } catch(e) { println("C:" + e); } finally { println("FF"); }
    return 0;
}
"#,
        "fin\n1\n1\nF1\nF2\nF3\nC:x\nFF\n",
    );
}

// ------------------------------------------------------- tasks and mutexes

#[test]
fn task_throw_fidelity_and_mutex_accumulation() {
    assert_all_engines(
        "tasks",
        r#"fn main() {
    let t = spawn fn() {
        throw "taskboom";
        return 0;
    };
    try { t.join(); } catch(e) { println("caught:" + e); }
    let t2 = spawn fn() { return 41; };
    println(t2.join() + 1);
    let mu = mutex(0);
    let acc = 0;
    let ts = [];
    for i in [1, 2, 3, 4] {
        ts.push(spawn fn() {
            mu.lock();
            acc += i;
            mu.unlock();
            return 0;
        });
    }
    for t3 in ts { t3.join(); }
    println(acc);
    return 0;
}
"#,
        "caught:taskboom\n42\n10\n",
    );
}

// -------------------------------------------------------------- closures

#[test]
fn closure_capture_per_activation_and_per_iteration() {
    assert_all_engines(
        "closures",
        r#"fn make() {
    let n = 0;
    return fn() { n += 1; return n; };
}
fn main() {
    let c1 = make();
    let c2 = make();
    println(c1());
    println(c1());
    println(c2());
    let fs = [];
    for i in [0, 1, 2] { fs.push(fn() { return i; }); }
    println(fs[0]());
    println(fs[1]());
    println(fs[2]());
    return 0;
}
"#,
        "1\n2\n1\n0\n1\n2\n",
    );
}

// ----------------------------------------------------------------- floats

#[test]
fn float_literals_promotion_and_division() {
    assert_all_engines(
        "floats",
        r#"fn main() {
    println(1.5e3);
    println(1e-5);
    println(2E+4);
    println(7 / 2);
    println(7.0 / 2);
    println(7 % 3);
    println(-7 % 3);
    println(int("42") + 1);
    println(float("2.5") * 2);
    return 0;
}
"#,
        "1500.0\n0.00001\n20000.0\n3\n3.5\n1\n-1\n43\n5.0\n",
    );
}

// ------------------------------------------------------------------- JSON

#[test]
fn json_round_trip_with_unicode_and_escapes() {
    assert_all_engines(
        "json",
        r#"fn main() {
    let m1 = map();
    m1.set("k", [1, 2.5, "s", true, none]);
    m1.set("u", "é中");
    m1.set("q", "quote\"back\\slash\n");
    let s = json_write(m1);
    let back = json_parse(s);
    println(back.get("u"));
    println(back.get("q"));
    println(json_write(json_parse("[1e3, 0.1]")));
    try { json_parse("{bad"); println("no-throw"); } catch(e) { println("throw"); }
    return 0;
}
"#,
        "é中\nquote\"back\\slash\n\n[1000.0,0.1]\nthrow\n",
    );
}

// ---------------------------------------------------------------- classes

#[test]
fn class_inheritance_fields_and_methods() {
    assert_all_engines(
        "classes",
        r#"class Animal {
    private let secret;
    public let name;
    public fn new(n) { self.name = n; self.secret = 1; }
    public fn speak(self) { return self.name + " speaks"; }
    public fn get_secret(self) { return self.secret; }
}
class Dog extends Animal {
    public fn new(n) { self.name = n; self.secret = 2; }
}
fn main() {
    let d = new Dog("rex");
    println(d.speak());
    println(d.get_secret());
    println(d.name);
    let a = new Animal("cat");
    println(a.speak());
    return 0;
}
"#,
        "rex speaks\n2\nrex\ncat speaks\n",
    );
}

// ---------------------------------------------------------------- modules

#[test]
fn module_public_and_private_names() {
    // The helper module is written next to the script (imports resolve next
    // to the importing file).
    let dir = std::env::temp_dir();
    let mod_path = dir.join("spid3r_surface_mod_helper.matrix");
    std::fs::write(
        &mod_path,
        "public const VERSION = 7;\npublic fn add(a, b) { return a + b; }\nlet hidden = 99;\n",
    )
    .expect("write module");
    assert_all_engines(
        "modules",
        r#"import "spid3r_surface_mod_helper" as mu;
fn main() {
    println(mu.add(2, 3));
    println(mu.VERSION);
    try { println(mu.hidden); } catch(e) { println("private"); }
    return 0;
}
"#,
        "5\n7\nprivate\n",
    );
}

// ------------------------------------------------------ strings and lists

#[test]
fn string_method_family() {
    assert_all_engines(
        "strings",
        r#"fn main() {
    println("hello".slice(1, 3));
    println("hello".upper());
    println("  x  ".trim());
    println(len("a,b,c".split(",")));
    println("aaa".replace("a", "b"));
    println("abc".contains("b"));
    println("abc".starts_with("ab"));
    println("abc".ends_with("c"));
    println("x".repeat(3));
    println(string(42) + string(1.5) + string(true) + string(none));
    println(len("héllo"));
    println("héllo"[1]);
    println("abc".index_of("c"));
    try { println("abc".slice(2, 1)); } catch(e) { println("bad-slice"); }
    return 0;
}
"#,
        "el\nHELLO\nx\n3\nbbb\ntrue\ntrue\ntrue\nxxx\n421.5truenone\n5\né\n2\nbad-slice\n",
    );
}

#[test]
fn list_method_family() {
    assert_all_engines(
        "lists",
        r#"fn main() {
    let xs = [3, 1, 2];
    xs.sort(); println(xs);
    xs.reverse(); println(xs);
    println(xs.map(fn(v) { return v * 2; }));
    println(xs.filter(fn(v) { return v > 2; }));
    println(xs.reduce(0, fn(acc, v) { return acc + v; }));
    println(xs.index_of(1));
    xs.insert(1, 99); println(xs);
    xs.remove(99); println(xs);
    let st = stack(); st.push(1); st.push(2); println(st.pop()); println(st.peek());
    let q = queue(); q.push("a"); q.push("b"); println(q.pop());
    return 0;
}
"#,
        "[1, 2, 3]\n[3, 2, 1]\n[6, 4, 2]\n[3]\n6\n2\n[3, 99, 2, 1]\n[3, 2, 1]\n2\n1\na\n",
    );
}

// -------------------------------------------------------------- formatter

#[test]
fn formatter_is_idempotent_and_preserves_semantics() {
    // `spid3r fmt` must not change what the program does: run the original,
    // format it, run it again, and compare.
    let src = r#"fn main() {
    let x = 5;
    let obj = map();
    obj.set("k", 42);
    println("{x} {obj.k}");
    for i in [1, 2] { println(i); }
    return 0;
}
"#;
    let name = "fmt_roundtrip";
    let path = std::env::temp_dir().join(format!("spid3r_surface_{}.matrix", name));
    std::fs::write(&path, src).expect("write");

    let first = run_src("tree", name, src).expect("first run");
    let f = Command::new(bin())
        .args(["fmt", path.to_str().unwrap()])
        .output()
        .expect("fmt");
    assert!(f.status.success(), "fmt failed: {}", String::from_utf8_lossy(&f.stderr));
    let formatted = std::fs::read_to_string(&path).expect("read back");
    let second = run_src("tree", name, &formatted).expect("second run");
    assert_eq!(first, second, "formatter changed program behaviour");

    // Formatting an already-formatted file is a no-op.
    let check = Command::new(bin())
        .args(["fmt", "--check", path.to_str().unwrap()])
        .output()
        .expect("fmt --check");
    assert!(
        check.status.success(),
        "fmt is not idempotent: {}",
        String::from_utf8_lossy(&check.stdout)
    );
}

// ------------------------------------------------------- stack-trace parity

/// Assert the two engines produce the same stack-trace frame names (and the
/// same trace text) for a failing program.
fn assert_same_trace(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("spid3r_surface_{}.matrix", name));
    std::fs::write(&path, src).expect("write");
    let run = |engine: &str| {
        let o = Command::new(bin())
            .args(["run", "--engine", engine, path.to_str().unwrap()])
            .output()
            .expect("run");
        assert!(
            !o.status.success(),
            "[{}] {} unexpectedly succeeded",
            name,
            engine
        );
        String::from_utf8_lossy(&o.stderr).to_string()
    };
    let tree = run("tree");
    let vm = run("vm");
    assert_eq!(tree, vm, "[{}] stack traces diverged between engines", name);
    tree
}

#[test]
fn method_stack_frames_are_qualified_like_the_tree_engine() {
    // The VM used to print a bare `speak` where the tree engine printed
    // `Animal::speak`.
    let trace = assert_same_trace(
        "trace_methods",
        r#"class Animal {
    public let name;
    public fn new(n) { self.name = n; }
    public fn speak(self) { return self.boom(); }
    public fn boom(self) { throw "deep"; }
}
fn main() {
    let a = new Animal("x");
    a.speak();
    return 0;
}
"#,
    );
    assert!(
        trace.contains("Animal::boom") && trace.contains("Animal::speak"),
        "expected qualified method frames, got:/n{}",
        trace
    );
}

#[test]
fn constructor_stack_frames_are_qualified_like_the_tree_engine() {
    let trace = assert_same_trace(
        "trace_ctor",
        r#"class Animal {
    public let name;
    public fn new(n) { self.name = n; throw "ctor-boom"; }
    public fn speak(self) { return self.name; }
}
fn main() {
    let a = new Animal("x");
    a.speak();
    return 0;
}
"#,
    );
    assert!(
        trace.contains("Animal::new"),
        "expected a qualified constructor frame, got:/n{}",
        trace
    );
}
