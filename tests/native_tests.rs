//! Native-backend differential tests — the fourth engine. Every example the
//! native backend supports is compiled ahead-of-time (`spid3r build`) and its
//! stdout must be byte-for-byte identical to the tree-walk interpreter.
//!
//! The native backend supports most of the language and runs it through the
//! differential: functions/closures, classes, `try`/`catch`/`finally`/`throw`,
//! `import` (flattened at build time), and `spawn`/`async`/`await` (tasks run on
//! the shared worker pool in `sched`). The only rejected construct is a class
//! declared inside a function (clean "not supported" diagnostic), never silently
//! miscompiled.
//!
//! Requires `cargo test --features native` plus a local LLVM 18 install
//! (see docs/BUILDING.md). Without the feature this file compiles to nothing.

#![cfg(feature = "native")]

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

/// Run the spid3r binary and return stdout, or an error string (stderr) on
/// non-zero exit / IO failure.
fn out(args: &[&str]) -> Result<String, String> {
    let o = Command::new(bin())
        .args(args)
        .output()
        .map_err(|e| format!("spawn failed: {}", e))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).to_string())
    }
}

/// Run a compiled executable directly and return its stdout, or stderr on
/// non-zero exit.
fn run_exe(path: &str) -> Result<String, String> {
    let o = Command::new(path)
        .output()
        .map_err(|e| format!("spawn failed: {}", e))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).to_string())
    }
}

/// The native backend links `LLVM-C.dll` beside the compiler (see build.rs);
/// if it was not copied there, skip the whole module instead of failing.
fn native_available() -> bool {
    if cfg!(windows) {
        let dll = bin()
            .parent()
            .map(|d| d.join("LLVM-C.dll"))
            .unwrap_or_default();
        dll.is_file()
    } else {
        Command::new("cc").arg("--version").output().is_ok()
            || Command::new("clang").arg("--version").output().is_ok()
    }
}

#[test]
fn native_matches_tree_on_supported_examples() {
    if !native_available() {
        eprintln!(
            "skip: LLVM-C.dll not found next to {} (build with SPID3R_LLVM_BIN_DIR set)",
            bin().display()
        );
        return;
    }
    let manifest = env!("CARGO_MANIFEST_DIR");
    let root = PathBuf::from(manifest).join("examples");
    assert!(root.is_dir(), "examples/ must exist at the crate root");

    let mut entries: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("read examples/")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "matrix").unwrap_or(false))
        .collect();
    entries.sort();

    let tmp = std::env::temp_dir();
    let mut checked = 0usize;
    let mut skipped = 0usize;

    for p in &entries {
        let path = p.to_str().unwrap();
        let src = std::fs::read_to_string(p).expect("read example");
        if !src.contains("fn main") {
            // library module: only the front end has to accept it
            out(&["check", path])
                .unwrap_or_else(|e| panic!("library {} must pass check:\n{}", path, e));
            continue;
        }

        let tree = out(&["run", "--engine", "tree", path])
            .unwrap_or_else(|e| panic!("tree run of {} failed:\n{}", path, e));

        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let exe = tmp.join(if cfg!(windows) {
            format!("spid3r_nt_{}.exe", stem)
        } else {
            format!("spid3r_nt_{}", stem)
        });
        let exe = exe.to_str().unwrap().to_string();

        match out(&["build", path, "-o", &exe]) {
            Ok(_) => {}
            Err(e) => {
                // documented v1 gaps must be rejected, never miscompiled
                if e.contains("not supported by the native backend yet") {
                    skipped += 1;
                    eprintln!("skip {}: {}", stem, e.lines().next().unwrap_or(""));
                    continue;
                }
                panic!("native build of {} failed unexpectedly:\n{}", path, e);
            }
        }

        let native =
            run_exe(&exe).unwrap_or_else(|e| panic!("native run of {} failed:\n{}", path, e));
        assert_eq!(tree, native, "tree vs native stdout differ for {}", path);
        checked += 1;
        let _ = std::fs::remove_file(&exe);
    }

    assert!(
        checked >= 10,
        "expected at least 10 examples to run natively, only {} did",
        checked
    );
    eprintln!(
        "native differential: {} passed, {} skipped (documented gaps)",
        checked, skipped
    );
}

#[test]
fn native_matches_tree_on_new_stdlib_builtins() {
    // The 2026-10 stdlib expansion: the AOT runtime installs the same
    // `native_defs()` table as the interpreters, so a builtin added to the
    // stdlib must answer identically from generated machine code.
    if !native_available() {
        eprintln!("skip: LLVM-C.dll not found");
        return;
    }
    let dir = std::env::temp_dir();
    let src_path = dir.join("spid3r_nat_stdlib.matrix");
    let src = r#"fn main() {
    println(floor(3.7) + ceil(3.2) + trunc(-3.9) + sign(-4.0));
    println(round(2.5) + ln(1.0) + log2(8.0) + log10(1000.0) + exp(0.0));
    println(sin(0.0) + cos(0.0) + tan(0.0) + asin(1.0) + acos(1.0) + atan(1.0));
    println(atan2(1.0, 1.0) + hypot(3.0, 4.0));
    println(clamp(15.0, 0.0, 10.0) + float_or("2.25", 1.5) + float_or("nope", 1.5));
    // Booleans are not addable in Spid3r, so the predicates go through int().
    println(int(is_nan(nan())) + int(is_inf(inf())));
    println(1.5e3 + 1e-5 * 100000.0 + 2E+4 / 4.0);
    println(sha256("abc"));
    println(sha256(""));
    println(hmac_sha256("key", "The quick brown fox jumps over the lazy dog"));
    return 0;
}
"#;
    std::fs::write(&src_path, src).expect("write scratch");
    let src_path = src_path.to_str().unwrap().to_string();

    let tree = out(&["run", "--engine", "tree", &src_path])
        .unwrap_or_else(|e| panic!("tree run failed:\n{}", e));

    let exe = dir.join(if cfg!(windows) {
        "spid3r_nat_stdlib.exe"
    } else {
        "spid3r_nat_stdlib"
    });
    let exe = exe.to_str().unwrap().to_string();
    out(&["build", &src_path, "-o", &exe])
        .unwrap_or_else(|e| panic!("native build failed:\n{}", e));
    let native = run_exe(&exe).unwrap_or_else(|e| panic!("native run failed:\n{}", e));
    assert_eq!(
        tree, native,
        "tree vs native stdout differ for the expanded stdlib"
    );

    // The expected values, independently of any engine.
    assert_eq!(
        tree,
        concat!(
            "3.0\n",
            "10.0\n",
            "3.356194490192345\n",
            "5.785398163397448\n",
            "13.75\n",
            "2\n",
            "6501.0\n",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\n",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n",
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8\n",
        ),
        "tree output drifted from the expected values",
    );
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&exe);
}

#[test]
fn native_matches_tree_on_sorting_and_formatting() {
    // The 2026-10 sorting/formatting round on the AOT backend: the runtime
    // installs the same `native_defs()` table and runs callbacks through
    // `NativeCallbackHost`, so a stable sort and a fixed-precision format must
    // answer identically from generated machine code.
    if !native_available() {
        eprintln!("skip: LLVM-C.dll not found");
        return;
    }
    let dir = std::env::temp_dir();
    let src_path = dir.join("spid3r_nat_sort.matrix");
    let src = r#"fn main() {
    let xs = list(30, 10, 20, 10);
    xs.sort_by(fn(a, b) { return a < b; });
    println(xs);
    println(list(3, 1, 2).argsort());
    println(list(3, 1, 2).sorted_by(fn(a, b) { return b - a; }));
    println("{:.2}".format(88.6875));
    println("{:.0}".format(2.5));
    println(hmac_sha256("k", "m"));
    return 0;
}
"#;
    std::fs::write(&src_path, src).expect("write scratch");
    let src_path = src_path.to_str().unwrap().to_string();

    let tree = out(&["run", "--engine", "tree", &src_path])
        .unwrap_or_else(|e| panic!("tree run failed://n{}", e));

    let exe = dir.join(if cfg!(windows) {
        "spid3r_nat_sort.exe"
    } else {
        "spid3r_nat_sort"
    });
    let exe = exe.to_str().unwrap().to_string();
    out(&["build", &src_path, "-o", &exe])
        .unwrap_or_else(|e| panic!("native build failed://n{}", e));
    let native = run_exe(&exe).unwrap_or_else(|e| panic!("native run failed://n{}", e));
    assert_eq!(
        tree, native,
        "tree vs native stdout differ for sorting/formatting"
    );

    assert_eq!(
        tree,
        concat!(
            "[10, 10, 20, 30]\n",
            "[1, 2, 0]\n",
            "[3, 2, 1]\n",
            "88.69\n",
            "2\n",
            "b60090e3052297aeb5a080889ce2fc4bca957e756faeb4df7d31800ca1e771ec\n",
        ),
        "tree output drifted from the expected values",
    );
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&exe);
}

#[test]
fn native_loop_exits_and_nested_finally_match_tree() {
    // Regression (2026-10-25 follow-up audit): two native-only control-flow
    // miscompiles around `try`/`finally`.
    //  1. A `break`/`continue` targeting a loop *inside* a `try` body that has
    //     a `finally` was routed through the `finally` dispatch, which then
    //     jumped to the loop *enclosing* the try - or hit `unreachable` when
    //     there was none, silently exiting the process with status 0.
    //  2. With nested `try`/`finally`, a `return`/`break`/`continue` ran only
    //     the innermost `finally` and skipped every outer one.
    // Tree and VM were already correct; native must now match byte-for-byte.
    if !native_available() {
        eprintln!("skip: LLVM-C.dll not found");
        return;
    }
    let dir = std::env::temp_dir();
    let src_path = dir.join("spid3r_nat_finally_loops.matrix");
    let src = r#"fn inner_break() {
    let n = 0;
    try {
        while (true) { n += 1; if (n > 2) { break; } }
    } finally { n += 100; }
    return n;
}
fn inner_continue() {
    let n = 0;
    let odd = 0;
    try {
        while (n < 5) { n += 1; if (n % 2 == 0) { continue; } odd += 1; }
    } finally { n += 100; }
    return string(n) + "/" + string(odd);
}
fn pending_concat_break() {
    let s = "L";
    try {
        while (true) { s = s + "z"; if (s.len() > 4) { break; } }
        s = s + "!";
    } finally { s = s + "F"; }
    return s;
}
fn nested_return() {
    try {
        try { return "ret"; } finally { println("inner-finally"); }
    } finally { println("outer-finally"); }
    return "unreached";
}
fn nested_break() {
    let n = 0;
    while (true) {
        try {
            try { n += 1; break; } finally { println("b-inner"); }
        } finally { println("b-outer"); }
    }
    return n;
}
fn nested_continue() {
    let n = 0;
    while (n < 2) {
        n += 1;
        try {
            try { continue; } finally { println("c-inner" + string(n)); }
        } finally { println("c-outer" + string(n)); }
    }
    return n;
}
fn outer_loop_inner_loop() {
    let out = "";
    let i = 0;
    while (i < 3) {
        i += 1;
        try {
            let j = 0;
            while (true) { j += 1; if (j > i) { break; } }
            if (i == 2) { continue; }
            out = out + string(j);
        } finally { out = out + "."; }
    }
    return out;
}
fn main() {
    println(inner_break());
    println(inner_continue());
    println(pending_concat_break());
    println(nested_return());
    println(nested_break());
    println(nested_continue());
    println(outer_loop_inner_loop());
    println("end");
    return 0;
}
"#;
    std::fs::write(&src_path, src).expect("write scratch");
    let src_path = src_path.to_str().unwrap().to_string();

    let tree = out(&["run", "--engine", "tree", &src_path])
        .unwrap_or_else(|e| panic!("tree run failed:\n{}", e));
    let vm = out(&["run", "--engine", "vm", &src_path])
        .unwrap_or_else(|e| panic!("vm run failed:\n{}", e));
    let expected = concat!(
        "103\n",
        "105/3\n",
        "Lzzzz!F\n",
        "inner-finally\n",
        "outer-finally\n",
        "ret\n",
        "b-inner\n",
        "b-outer\n",
        "1\n",
        "c-inner1\n",
        "c-outer1\n",
        "c-inner2\n",
        "c-outer2\n",
        "2\n",
        "2..4.\n",
        "end\n",
    );
    assert_eq!(
        tree, expected,
        "tree output drifted from the expected values"
    );
    assert_eq!(vm, expected, "vm output differs from expected");

    let exe = dir.join(if cfg!(windows) {
        "spid3r_nat_finally_loops.exe"
    } else {
        "spid3r_nat_finally_loops"
    });
    let exe = exe.to_str().unwrap().to_string();
    out(&["build", &src_path, "-o", &exe])
        .unwrap_or_else(|e| panic!("native build failed:\n{}", e));
    let native = run_exe(&exe).unwrap_or_else(|e| panic!("native run failed:\n{}", e));
    assert_eq!(native, expected, "native output differs from tree/vm");
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&exe);
}

#[test]
fn unsupported_features_are_rejected_with_clear_diagnostics() {
    if !native_available() {
        eprintln!("skip: LLVM-C.dll not found");
        return;
    }
    // (name, source, acceptable error substrings — the native backend's hard
    // diagnostic, plus the front end's own resolution error for `import`).
    // `spawn`/`async`/`await` are now supported and run through the differential.
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "import",
            "import \"x\"; fn main() { return 0; }",
            &["not supported by the native backend yet", "module"],
        ),
        (
            "class-in-fn",
            "fn main() { class A { fn new() {} } let a = new A(); return 0; }",
            &[
                "not supported by the native backend yet",
                "unexpected keyword 'class'",
            ],
        ),
    ];
    for (name, src, accepted) in cases {
        let dir = std::env::temp_dir();
        let src_path = dir.join(format!("spid3r_bad_{}.matrix", name));
        std::fs::write(&src_path, src).expect("write scratch");
        let src_path = src_path.to_str().unwrap().to_string();
        let exe = dir.join(format!("spid3r_bad_{}.exe", name));
        let exe = exe.to_str().unwrap().to_string();
        let r = out(&["build", &src_path, "-o", &exe]);
        match r {
            Ok(_) => panic!("{} must be rejected by the native backend", name),
            Err(e) => {
                assert!(
                    accepted.iter().any(|a| e.contains(a)),
                    "{} rejected with a non-diagnostic message:\n{}",
                    name,
                    e
                );
            }
        }
        let _ = std::fs::remove_file(&src_path);
        let _ = std::fs::remove_file(&exe);
    }
}

#[test]
fn native_matches_tree_on_structured_task_errors() {
    // Audit A-9 on the AOT backend: a task that throws a structured value must
    // hand the value (not its display string) to `catch`, both through `await`
    // and through the `task.join()` method. `spid3r_await` rethrows the stored
    // value; `spid3r_method` must do the same instead of throwing the String
    // error the shared collections layer reports.
    if !native_available() {
        eprintln!("skip: LLVM-C.dll not found");
        return;
    }
    let dir = std::env::temp_dir();
    let src_path = dir.join("spid3r_nat_task_errors.matrix");
    let src = r#"async fn joiner(t) {
    return await t;
}
fn main() {
    let a = spawn fn() {
        let o = map();
        o.set("code", 7);
        throw o;
    };
    try {
        let v = joiner(a);
        println("no-throw");
    } catch(e) {
        println(type_of(e));
        println(e["code"]);
    }
    let b = spawn fn() {
        let o = map();
        o.set("code", 9);
        throw o;
    };
    sleep_ms(50);
    try {
        let v = b.join();
        println("no-throw");
    } catch(e) {
        println(type_of(e));
        println(e["code"]);
    }
    return 0;
}
"#;
    std::fs::write(&src_path, src).expect("write scratch");
    let src_path = src_path.to_str().unwrap().to_string();

    let tree = out(&["run", "--engine", "tree", &src_path])
        .unwrap_or_else(|e| panic!("tree run failed:/n{}", e));

    let exe = dir.join(if cfg!(windows) {
        "spid3r_nat_task_errors.exe"
    } else {
        "spid3r_nat_task_errors"
    });
    let exe = exe.to_str().unwrap().to_string();
    out(&["build", &src_path, "-o", &exe])
        .unwrap_or_else(|e| panic!("native build failed:/n{}", e));
    let native = run_exe(&exe).unwrap_or_else(|e| panic!("native run failed:/n{}", e));
    assert_eq!(
        tree, native,
        "tree vs native stdout differ for structured task errors"
    );
    assert_eq!(
        tree,
        concat!("map\n", "7\n", "map\n", "9\n"),
        "structured thrown values did not survive the task boundary",
    );
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&exe);
}

#[test]
fn native_matches_tree_on_captured_state_and_spawned_tasks() {
    // Audit A-12: upvalue cells are now atomic and environment arrays carry a
    // length header, so closures and spawned tasks that share captured state
    // must still behave exactly like the tree engine.
    if !native_available() {
        eprintln!("skip: LLVM-C.dll not found");
        return;
    }
    let dir = std::env::temp_dir();
    let src_path = dir.join("spid3r_nat_captures.matrix");
    let src = r#"fn counter() {
    let n = 0;
    return fn() {
        n = n + 1;
        return n;
    };
}

async fn wait(t) {
    return await t;
}

fn main() {
    let a = counter();
    println(a());
    println(a());
    let b = counter();
    println(b());
    let hits = 0;
    let ts = list();
    let i = 0;
    while (i < 8) {
        ts.push(spawn fn() {
            hits = hits + 1;
        });
        i = i + 1;
    }
    let j = 0;
    while (j < 8) {
        wait(ts[j]);
        j = j + 1;
    }
    println(hits >= 1);
    println(hits <= 8);
    return 0;
}
"#;
    std::fs::write(&src_path, src).expect("write scratch");
    let src_path = src_path.to_str().unwrap().to_string();

    let tree = out(&["run", "--engine", "tree", &src_path])
        .unwrap_or_else(|e| panic!("tree run failed:/n{}", e));

    let exe = dir.join(if cfg!(windows) {
        "spid3r_nat_captures.exe"
    } else {
        "spid3r_nat_captures"
    });
    let exe = exe.to_str().unwrap().to_string();
    out(&["build", &src_path, "-o", &exe])
        .unwrap_or_else(|e| panic!("native build failed:/n{}", e));
    let native = run_exe(&exe).unwrap_or_else(|e| panic!("native run failed:/n{}", e));
    assert_eq!(
        tree, native,
        "tree vs native stdout differ for captured state / spawned tasks"
    );
    assert!(
        tree.starts_with("1\n2\n1\n"),
        "closure counter output drifted: {}",
        tree
    );
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&exe);
}

/// Operator overloading under AOT (audit S-21). The native `spid3r_binop`
/// trampoline used to delegate straight to `vm_eval_binary`, which has no
/// overload path: `Money + Money` (class-instance dunder) and
/// `complex + complex` (map-carried dunder, the route every stdlib numeric
/// type uses) both raised "cannot add instance and instance" /
/// "cannot add map and map" in compiled code while the same program ran on
/// tree and VM. This pins the full overload set — binary, reflected and
/// unary — against the tree engine on real stdlib types and a user class.
#[test]
fn native_operator_overloading_matches_tree() {
    if !native_available() {
        eprintln!("skip: LLVM-C.dll not found next to {}", bin().display());
        return;
    }
    let dir = std::env::temp_dir();

    // `complex` and `bigint` both export `to_string`, so a single program
    // importing both unaliased is a semantic import collision (correctly
    // rejected). Aliased imports of both in one program would work, but the
    // two stdlib types are exercised in separate programs here so each set of
    // expected values stays self-contained.
    let cx = r#"import "complex";

class Money {
    public let cents;
    fn new(cents) {
        self.cents = cents;
    }
    public fn __add__(self, other) {
        return Money(self.cents + other.cents);
    }
    public fn __sub__(self, other) {
        return Money(self.cents - other.cents);
    }
    public fn __mul__(self, k) {
        return Money(self.cents * k);
    }
    public fn __div__(self, k) {
        return Money(self.cents / k);
    }
    public fn __neg__(self) {
        return Money(-self.cents);
    }
    public fn __radd__(self, o) {
        return Money(o + self.cents);
    }
}

fn main() {
    // map-carried dunders: the stdlib numeric types
    let a = of(1, 2);
    let b = of(3, 4);
    println(to_string(a + b));
    println(to_string(a * b));
    println(to_string(b - a));
    println(to_string(b / a));
    // class-instance dunders: every operator, plus the reflected form
    let m = new Money(250) + new Money(125);
    println(m.cents);
    println((new Money(500) - new Money(125)).cents);
    println((new Money(5) * 3).cents);
    println((new Money(100) / 4).cents);
    println((-new Money(7)).cents);
    println((10 + new Money(5)).cents);
    // a throw inside an overloaded operator stays catchable
    try {
        let bad = new Money(1) + new Money(2);
        println(bad.cents);
    } catch (e) {
        println("caught: " + string(e));
    }
    return 0;
}
"#;
    let big = r#"import "bigint";

fn main() {
    let b = from_string("123456789012345678901234567890");
    println(to_string(b + from_string("1")));
    println(to_string(-b));
    println(to_string(b * from_string("2")));
    println(to_string(b - from_string("999999999999")));
    return 0;
}
"#;

    // (tag, source, expected stdout)
    let cases: [(&str, &str, &str); 2] = [
        (
            "cx",
            cx,
            concat!(
                "4.0+6.0i\n",   // complex add
                "-5.0+10.0i\n", // complex mul
                "2.0+2.0i\n",   // complex sub
                "2.2-0.4i\n",   // complex div
                "375\n",        // Money add
                "375\n",        // Money sub
                "15\n",         // Money mul
                "25\n",         // Money div
                "-7\n",         // Money neg
                "15\n",         // Money radd (10 + Money(5))
                "3\n",          // no throw: the try body completes
            ),
        ),
        (
            "big",
            big,
            concat!(
                "123456789012345678901234567891\n",  // bigint add
                "-123456789012345678901234567890\n", // bigint neg
                "246913578024691357802469135780\n",  // bigint mul
                "123456789012345677901234567891\n",  // bigint sub
            ),
        ),
    ];

    for (tag, src, expected) in cases {
        let src_path = dir.join(format!("spid3r_nat_ops_{}.matrix", tag));
        std::fs::write(&src_path, src).expect("write scratch");
        let src_path = src_path.to_str().unwrap().to_string();

        let tree = out(&["run", "--engine", "tree", &src_path])
            .unwrap_or_else(|e| panic!("tree run failed ({}):/n{}", tag, e));
        let vm = out(&["run", "--engine", "vm", &src_path])
            .unwrap_or_else(|e| panic!("vm run failed ({}):/n{}", tag, e));
        assert_eq!(
            tree, vm,
            "tree vs vm differ for operator overloading ({})",
            tag
        );

        let exe = dir.join(if cfg!(windows) {
            format!("spid3r_nat_ops_{}.exe", tag)
        } else {
            format!("spid3r_nat_ops_{}", tag)
        });
        let exe = exe.to_str().unwrap().to_string();
        out(&["build", &src_path, "-o", &exe])
            .unwrap_or_else(|e| panic!("native build failed ({}):/n{}", tag, e));
        let native =
            run_exe(&exe).unwrap_or_else(|e| panic!("native run failed ({}):/n{}", tag, e));
        assert_eq!(
            tree, native,
            "tree vs native stdout differ for operator overloading ({})",
            tag
        );
        assert_eq!(
            tree, expected,
            "output drifted from the expected values ({})",
            tag
        );
        let _ = std::fs::remove_file(&src_path);
        let _ = std::fs::remove_file(&exe);
    }
}

#[test]
fn native_audit_2026_regressions_match_tree() {
    if !native_available() {
        eprintln!("skip: LLVM-C.dll not found next to {}", bin().display());
        return;
    }
    let dir = std::env::temp_dir().join("spid3r_nat_audit_2026");
    std::fs::create_dir_all(&dir).expect("mkdir");

    // Case 1: SPID3R-2026-005 (logical bool coercion), SPID3R-2026-006 (BigInt literals 123n),
    // SPID3R-2026-007 (default parameters on constructors, super(), super.method(), callbacks),
    // SPID3R-2026-003 (named args on enum variants, inherited ctors, super),
    // SPID3R-2026-008 (serialize vec & overflow), SPID3R-2026-009 (abstract fn ->),
    // SPID3R-2026-010 (keyword interpolation), SPID3R-2026-013 (abs(i64::MIN)).
    let main_case = r#"enum Shape {
    Rect(w, h)
}

abstract class AbstractBase {
    abstract fn tag(self) -> string;
}

class Parent extends AbstractBase {
    public let x;
    public let y;
    fn new(x = 10, y = 20) {
        self.x = x;
        self.y = y;
    }
    public fn tag(self) -> string {
        return "P({self.x},{self.y})";
    }
    public fn compute(self, a, b = 5) {
        return self.x + self.y + a + b;
    }
}

class Child extends Parent {
    fn new(x = 1, y = 2) {
        super(x);
        self.y = y;
    }
    public fn compute(self, a, b = 5) {
        return super.compute(a) + b;
    }
}

class InheritedCtor extends Parent {
    public fn sum(self) {
        return self.x + self.y;
    }
}

class GrandChild extends InheritedCtor {
    fn new(a = 3, b = 4) {
        super(y: b, x: a);
    }
}

fn main() {
    // SPID3R-2026-005: Logical `and`/`or` coerce to bool
    let l1 = 1 and 2;
    let l2 = 0 or "fallback";
    let l3 = "hi" and 0;
    let l4 = 0 and "never";
    println(type_of(l1) + ":" + string(l1));
    println(type_of(l2) + ":" + string(l2));
    println(type_of(l3) + ":" + string(l3));
    println(type_of(l4) + ":" + string(l4));

    // SPID3R-2026-006: BigInt literal `123n` works natively AND does not clobber stdlib `abs`
    let big = 123456789012345678901234567890n + 1n;
    println(big);
    println(abs(-42));

    // SPID3R-2026-007 & 003: Default params & named args on ctors, super(), super.method(), callbacks
    let p0 = new Parent();
    let p1 = new Parent(100);
    let c0 = new Child();
    let c1 = new Child(y: 50, x: 30);
    let ic = new InheritedCtor(y: 8, x: 7);
    let gc = new GrandChild(b: 19, a: 11);
    println(p0.tag() + " " + p1.tag() + " " + c0.tag() + " " + c1.tag());
    println(c0.compute(3));
    println(ic.sum() + gc.sum());

    let mapped = [1, 2, 3].map(fn(v, bonus = 10) { return v + bonus; });
    println(mapped);

    let r = Rect(h: 6, w: 7);
    let area = match (r) {
        Rect(w, h) => w * h,
        _ => 0
    };
    println(area);

    // SPID3R-2026-010: "{true}", "{false}", "{null}"
    println("{true}:{false}:{null}");

    // SPID3R-2026-008 & 013: vec serialization and abs(i64::MIN) overflow
    println(msgpack_decode(msgpack_encode(vec(4, 5))).unwrap());
    let min_i = -9223372036854775807 - 1;
    try {
        println(abs(min_i));
    } catch (e) {
        println("abs_overflow:" + string(e.contains("integer overflow")));
    }
    return 0;
}
"#;

    // Case 2: SPID3R-2026-006 `yield` generators in Native AOT
    let yield_case = r#"import "gen";

fn counter(n) {
    let i = 0;
    while i < n {
        yield i * 10;
        i = i + 1;
    }
}

fn main() {
    println(collect(counter(4)));
    return 0;
}
"#;

    for (tag, src) in [("audit_main", main_case), ("audit_yield", yield_case)] {
        let src_path = dir.join(format!("spid3r_nat_{}.matrix", tag));
        std::fs::write(&src_path, src).expect("write scratch");
        let src_path_s = src_path.to_str().unwrap().to_string();

        let tree = out(&["run", "--engine", "tree", &src_path_s])
            .unwrap_or_else(|e| panic!("tree run failed ({}):\n{}", tag, e));
        let vm = out(&["run", "--engine", "vm", &src_path_s])
            .unwrap_or_else(|e| panic!("vm run failed ({}):\n{}", tag, e));
        assert_eq!(tree, vm, "tree vs vm stdout differ ({})", tag);

        let exe = dir.join(if cfg!(windows) {
            format!("spid3r_nat_{}.exe", tag)
        } else {
            format!("spid3r_nat_{}", tag)
        });
        let exe_s = exe.to_str().unwrap().to_string();
        out(&["build", &src_path_s, "-o", &exe_s])
            .unwrap_or_else(|e| panic!("native build failed ({}):\n{}", tag, e));
        let native =
            run_exe(&exe_s).unwrap_or_else(|e| panic!("native run failed ({}):\n{}", tag, e));
        assert_eq!(tree, native, "tree vs native stdout differ ({})", tag);

        let _ = std::fs::remove_file(&src_path);
        let _ = std::fs::remove_file(&exe);
    }
}

#[test]
fn native_aliased_import_deep_parity() {
    if !native_available() {
        eprintln!("skip native test: LLVM-C.dll not found");
        return;
    }
    let dir = std::env::temp_dir();
    let mod_path = dir.join("spid3r_nat_aliased_submod.matrix");
    let main_path = dir.join("spid3r_nat_aliased_submod_main.matrix");

    let sub_src = r#"
public let base_val = 100;
public let opt_obj = map("greeting", "hello from submod");

public class Adder {
    public let x: int;
    fn new(x: int = 5) {
        self.x = x;
    }
    public fn add(self, y: int) -> int {
        return self.x + y;
    }
}

public fn compute(a, b) {
    let ad = new Adder(a);
    let greeting = opt_obj?.greeting;
    println(greeting);
    return ad.add(b) + base_val;
}
"#;

    let main_src = r#"
import "spid3r_nat_aliased_submod" as sub;

fn main() {
    let res = sub.compute(10, 25);
    println("computed:" + string(res));
    let a = sub.Adder(40);
    println("adder:" + string(a.add(2)));
    return 0;
}
"#;

    std::fs::write(&mod_path, sub_src).expect("write sub");
    std::fs::write(&main_path, main_src).expect("write main");

    let main_path_s = main_path.to_str().unwrap().to_string();

    let tree = out(&["run", "--engine", "tree", &main_path_s])
        .unwrap_or_else(|e| panic!("tree run failed:\n{}", e));
    let vm = out(&["run", "--engine", "vm", &main_path_s])
        .unwrap_or_else(|e| panic!("vm run failed:\n{}", e));
    assert_eq!(tree, vm, "tree vs vm stdout differ");

    let exe = dir.join("spid3r_nat_alias_deep.exe");
    let exe_s = exe.to_str().unwrap().to_string();
    out(&["build", &main_path_s, "-o", &exe_s])
        .unwrap_or_else(|e| panic!("native build failed:\n{}", e));
    let native = run_exe(&exe_s).unwrap_or_else(|e| panic!("native run failed:\n{}", e));
    assert_eq!(tree, native, "tree vs native stdout differ");

    let _ = std::fs::remove_file(&mod_path);
    let _ = std::fs::remove_file(&main_path);
    let _ = std::fs::remove_file(&exe);
}

/// A namespace import of a module that itself plain-imports another module
/// must not re-export the transitive module's names.
///
/// The flattener used to collect the namespace's export set (and mangle
/// definitions) from the FLATTENED statement list, where transitively
/// inlined plain-import declarations sit — so `mid.LIB_TAG` answered on
/// native while the tree/VM correctly raised "missing key 'LIB_TAG'"
/// (`mid_mod` merely imports `mod_lib`; it does not re-export it).
#[test]
fn native_namespace_does_not_leak_transitive_imports() {
    if !native_available() {
        eprintln!("skip native test: LLVM-C.dll not found");
        return;
    }
    let dir = std::env::temp_dir();
    let lib_path = dir.join("spid3r_nat_ns_lib.matrix");
    let mid_path = dir.join("spid3r_nat_ns_mid.matrix");
    let main_path = dir.join("spid3r_nat_ns_trans.matrix");

    std::fs::write(
        &lib_path,
        "public const LIB_TAG = \"lib-v1\";\n\npublic fn lib_add(a, b) {\n    return a + b;\n}\n",
    )
    .expect("write lib");
    std::fs::write(
        &mid_path,
        "import \"spid3r_nat_ns_lib\";\n\npublic fn combined(x) {\n    return lib_add(x, 10);\n}\n",
    )
    .expect("write mid");
    std::fs::write(
        &main_path,
        "import \"spid3r_nat_ns_mid\" as mid;\n\nfn main() {\n    println(mid.combined(4));\n    println(mid.LIB_TAG);\n    return 0;\n}\n",
    )
    .expect("write main");

    let main_path_s = main_path.to_str().unwrap().to_string();

    // The reference: `combined` resolves, the transitive name does not.
    let tree_run = out(&["run", "--engine", "tree", &main_path_s]);
    match &tree_run {
        Ok(_) => panic!("tree must fail on mid.LIB_TAG (not re-exported)"),
        Err(e) => assert!(
            e.contains("missing key 'LIB_TAG'"),
            "tree error drifted: {}",
            e
        ),
    }
    let tree = tree_run.unwrap_err();
    assert!(
        tree.starts_with("error[runtime]"),
        "tree stderr drifted: {}",
        tree
    );

    let exe = dir.join("spid3r_nat_ns_trans.exe");
    let exe_s = exe.to_str().unwrap().to_string();
    out(&["build", &main_path_s, "-o", &exe_s])
        .unwrap_or_else(|e| panic!("native build failed:\n{}", e));
    let native = run_exe(&exe_s).unwrap_or_else(|e| e);
    assert!(
        native.contains("missing key 'LIB_TAG'"),
        "native must fail the same way, got: {}",
        native
    );

    // The working half (`mid.combined`) through the same namespace, with no
    // error: compile a main that only uses what the namespace really exports.
    std::fs::write(
        &main_path,
        "import \"spid3r_nat_ns_mid\" as mid;\n\nfn main() {\n    println(mid.combined(4));\n    return 0;\n}\n",
    )
    .expect("rewrite main");
    let tree_ok = out(&["run", "--engine", "tree", &main_path_s])
        .unwrap_or_else(|e| panic!("tree run failed:\n{}", e));
    assert_eq!(tree_ok.trim(), "14");
    out(&["build", &main_path_s, "-o", &exe_s])
        .unwrap_or_else(|e| panic!("native build failed:\n{}", e));
    let native_ok = run_exe(&exe_s).unwrap_or_else(|e| panic!("native run failed:\n{}", e));
    assert_eq!(tree_ok, native_ok, "tree vs native stdout differ");

    let _ = std::fs::remove_file(&lib_path);
    let _ = std::fs::remove_file(&mid_path);
    let _ = std::fs::remove_file(&main_path);
    let _ = std::fs::remove_file(&exe);
}

/// `new X(...)` where X is a local holding a class value — an alias (`let C =
/// Pt; new C(9)`) or a class pulled out of a module namespace
/// (`let C = mod.Cls; new C(5)`) — must construct like the tree/VM engines.
///
/// Expr::New's codegen resolved the class name through the GLOBAL table only,
/// so a local class value read as null: "null is not callable" on native
/// where tree/VM constructed normally. (`mod.Cls(5)` — calling the member —
/// worked, which is why the aliased-import parity test above never caught
/// the `new` path.)
#[test]
fn native_first_class_class_construction_matches_tree() {
    if !native_available() {
        eprintln!("skip native test: LLVM-C.dll not found");
        return;
    }
    let dir = std::env::temp_dir();
    let mod_path = dir.join("spid3r_nat_ns_cls.matrix");
    let main_path = dir.join("spid3r_nat_ns_cls_main.matrix");

    std::fs::write(
        &mod_path,
        "public class Counter {\n    public let n;\n    fn new(start) {\n        self.n = start;\n    }\n    public fn bump(self) -> int {\n        self.n = self.n + 1;\n        return self.n;\n    }\n}\n",
    )
    .expect("write mod");
    std::fs::write(
        &main_path,
        "import \"spid3r_nat_ns_cls\" as mc;\n\nclass Local {\n    public let x;\n    fn new(x) { self.x = x; }\n    public fn get(self) -> int { return self.x; }\n}\n\nfn main() {\n    let C = Local;\n    let p = new C(9);\n    println(p.get());\n    let K = mc.Counter;\n    let c = new K(5);\n    println(c.bump());\n    println(c.bump());\n    println(new Local(3).get());\n    return 0;\n}\n",
    )
    .expect("write main");

    let main_path_s = main_path.to_str().unwrap().to_string();

    let tree = out(&["run", "--engine", "tree", &main_path_s])
        .unwrap_or_else(|e| panic!("tree run failed:\n{}", e));
    let vm = out(&["run", "--engine", "vm", &main_path_s])
        .unwrap_or_else(|e| panic!("vm run failed:\n{}", e));
    assert_eq!(tree, vm, "tree vs vm stdout differ");

    let exe = dir.join("spid3r_nat_ns_cls.exe");
    let exe_s = exe.to_str().unwrap().to_string();
    out(&["build", &main_path_s, "-o", &exe_s])
        .unwrap_or_else(|e| panic!("native build failed:\n{}", e));
    let native = run_exe(&exe_s).unwrap_or_else(|e| panic!("native run failed:\n{}", e));
    assert_eq!(tree, native, "tree vs native stdout differ");

    let _ = std::fs::remove_file(&mod_path);
    let _ = std::fs::remove_file(&main_path);
    let _ = std::fs::remove_file(&exe);
}

#[test]
fn native_spawned_task_globals_cache_invalidates_across_tasks() {
    if !native_available() {
        eprintln!("skip native test: LLVM-C.dll not found");
        return;
    }
    let dir = std::env::temp_dir();
    let src_path = dir.join("spid3r_nat_gcache.matrix");
    let exe_path = dir.join("spid3r_nat_gcache.exe");
    let src = "\
let g = 10;
fn read_g() -> int { return g; }
fn main() {
    println(read_g());
    let t = spawn fn() {
        g = 42;
    };
    t.join();
    println(read_g());
    return 0;
}
";
    std::fs::write(&src_path, src).expect("write src");
    let src_s = src_path.to_str().unwrap().to_string();
    let exe_s = exe_path.to_str().unwrap().to_string();
    let tree = out(&["run", "--engine", "tree", &src_s]).expect("tree run");
    out(&["build", &src_s, "-o", &exe_s]).expect("native build");
    let native = run_exe(&exe_s).expect("native run");
    assert_eq!(tree, "10\n42\n");
    assert_eq!(tree, native);
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&exe_path);
}

#[test]
fn native_aot_slab_alloc_accounting_exposed_in_spid3rinfo() {
    if !native_available() {
        eprintln!("skip native test: LLVM-C.dll not found");
        return;
    }
    let dir = std::env::temp_dir();
    let src_path = dir.join("spid3r_nat_r40_alloc.matrix");
    let exe_path = dir.join("spid3r_nat_r40_alloc.exe");
    let src = "\
fn make_work(n) {
    let xs = list();
    let i = 0;
    while (i < n) {
        xs.push(\"item-\" + string(i));
        i = i + 1;
    }
    return xs.len();
}
fn main() {
    let n = make_work(25);
    let info = spid3rinfo();
    let mem = info[\"AOT Memory\"];
    println(n);
    println(int(mem[\"Slab capacity\"]) == 4096);
    println(int(mem[\"Slabs allocated\"]) == 0);
    println(int(mem[\"Completed call allocs\"]) > 0);
    println(int(mem[\"Max call allocs\"]) > 0);
    return 0;
}
";
    std::fs::write(&src_path, src).expect("write src");
    let src_s = src_path.to_str().unwrap().to_string();
    let exe_s = exe_path.to_str().unwrap().to_string();
    out(&["build", &src_s, "-o", &exe_s]).expect("native build");
    let native = run_exe(&exe_s).expect("native run");
    assert_eq!(native, "25\ntrue\ntrue\ntrue\ntrue\n");
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&exe_path);
}

fn assert_native_matches_tree(stem: &str, src: &str) -> String {
    let dir = std::env::temp_dir();
    let src_path = dir.join(format!("spid3r_{}.matrix", stem));
    let exe_path = dir.join(if cfg!(windows) {
        format!("spid3r_{}.exe", stem)
    } else {
        format!("spid3r_{}", stem)
    });
    std::fs::write(&src_path, src).expect("write src");
    let src_s = src_path.to_str().unwrap().to_string();
    let exe_s = exe_path.to_str().unwrap().to_string();
    let tree = out(&["run", "--engine", "tree", &src_s]).expect("tree run");
    out(&["build", &src_s, "-o", &exe_s]).expect("native build");
    let native = run_exe(&exe_s).expect("native run");
    assert_eq!(tree, native, "tree vs native mismatch for {}", stem);
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&exe_path);
    native
}

#[test]
fn phase1_2d_int_matmul_matches_tree() {
    if !native_available() {
        eprintln!("skip native test: LLVM-C.dll not found");
        return;
    }
    let src = r#"
fn build_a(n: int) {
    let a = list();
    let i = 0;
    while i < n {
        let row = list();
        let j = 0;
        while j < n {
            row.push((i * 7 + j * 13) % 19 - 9);
            j += 1;
        }
        a.push(row);
        i += 1;
    }
    return a;
}
fn build_bt(n: int) {
    let bt = list();
    let k = 0;
    while k < n {
        let row = list();
        let j = 0;
        while j < n {
            row.push((k * 11 + j * 3) % 17 - 8);
            j += 1;
        }
        bt.push(row);
        k += 1;
    }
    return bt;
}
fn small_rect() {
    let m1 = [[1, 2, 3], [4, 5, 6]];
    let m2t = [[7, 9, 11], [8, 10, 12]];
    let total = 0;
    let i = 0;
    while i < 2 {
        let r = m1[i];
        let k = 0;
        while k < 2 {
            let c = m2t[k];
            let s = 0;
            let j = 0;
            while j < 3 {
                s = s + r[j] * c[j];
                j += 1;
            }
            println(s);
            total = total + s;
            k += 1;
        }
        i += 1;
    }
    return total;
}
fn main() {
    println("RECT " + string(small_rect()));
    let n = 128;
    let a = build_a(n);
    let bt = build_bt(n);
    let total = 0;
    let diag = 0;
    let i = 0;
    while i < n {
        let ai = a[i];
        let k = 0;
        while k < n {
            let btk = bt[k];
            let s = 0;
            let j = 0;
            while j < n {
                s = s + ai[j] * btk[j];
                j += 1;
            }
            total = total + s;
            if i == k { diag = diag + s; }
            k += 1;
        }
        i += 1;
    }
    println("RESULT " + string(total) + " " + string(diag));
    return 0;
}
"#;
    let out_s = assert_native_matches_tree("p1_int_matmul", src);
    assert!(
        out_s.contains("RESULT -1985 -40\n"),
        "unexpected matmul output:\n{}",
        out_s
    );
}

#[test]
fn phase1_2d_float_dot_and_matmul_matches_tree() {
    if !native_available() {
        eprintln!("skip native test: LLVM-C.dll not found");
        return;
    }
    let src = r#"
fn float_matmul(n: int) {
    let a = list();
    let bt = list();
    let i = 0;
    while i < n {
        let r1 = list();
        let r2 = list();
        let j = 0;
        while j < n {
            r1.push(float(i * 3 + j + 1) * 0.25);
            r2.push(float(i + j * 5 - 2) * 0.5);
            j += 1;
        }
        a.push(r1);
        bt.push(r2);
        i += 1;
    }
    let total = 0.0;
    let ii = 0;
    while ii < n {
        let ai = a[ii];
        let k = 0;
        while k < n {
            let btk = bt[k];
            let s = 0.0;
            let j = 0;
            while j < n {
                s = s + ai[j] * btk[j];
                j += 1;
            }
            total = total + s;
            k += 1;
        }
        ii += 1;
    }
    return total;
}
fn int_accum_on_float_row(row, n: int) {
    let s = 0;
    let j = 0;
    while j < n {
        s = s + row[j];
        j += 1;
    }
    return s;
}
fn main() {
    println(float_matmul(4));
    let r = [1.5, 2.25, 3.75];
    println(int_accum_on_float_row(r, 3));
    println(int_accum_on_float_row(r, 0));
    return 0;
}
"#;
    assert_native_matches_tree("p1_float_matmul", src);
}

#[test]
fn phase1_bce_edge_cases_negative_oob_empty() {
    if !native_available() {
        eprintln!("skip native test: LLVM-C.dll not found");
        return;
    }
    let src = r#"
fn main() {
    let row = [10, 20, 30, 40, 50];
    let s1 = 0;
    let j1 = -2;
    try {
        while j1 < 2 {
            s1 = s1 + row[j1];
            j1 += 1;
        }
        println("neg-ok " + string(s1));
    } catch (e) {
        println("neg-err " + string(e) + " s=" + string(s1) + " j=" + string(j1));
    }

    let s2 = 0;
    let j2 = 0;
    try {
        while j2 < 8 {
            s2 = s2 + row[j2];
            j2 += 1;
        }
        println("oob-ok");
    } catch (e) {
        println("oob-err " + string(e) + " s=" + string(s2) + " j=" + string(j2));
    }

    let empty = [];
    let s3 = 0;
    let j3 = 0;
    while j3 < 0 {
        s3 = s3 + empty[j3];
        j3 += 1;
    }
    println("empty0 " + string(s3));

    try {
        let j4 = 0;
        while j4 < 2 {
            s3 = s3 + empty[j4];
            j4 += 1;
        }
    } catch (e) {
        println("empty2-err " + string(e));
    }

    try {
        let j5 = 0;
        while j5 <= 0 {
            s3 = s3 + empty[j5];
            j5 += 1;
        }
    } catch (e) {
        println("empty-le-err " + string(e));
    }

    let s6 = 0;
    let j6 = 0;
    while j6 <= row.len() - 1 {
        s6 = s6 + row[j6];
        j6 += 1;
    }
    println("le-sum " + string(s6) + " j=" + string(j6));

    let j7 = 0;
    let s7 = 123;
    while j7 < -3 {
        s7 = s7 + row[j7];
        j7 += 1;
    }
    println("neg-bound " + string(s7));
    return 0;
}
"#;
    assert_native_matches_tree("p1_bce_edges", src);
}

#[test]
fn phase1_mixed_list_and_non_list_fallback() {
    if !native_available() {
        eprintln!("skip native test: LLVM-C.dll not found");
        return;
    }
    let src = r#"
fn sum_rows(rows) {
    let i = 0;
    while i < rows.len() {
        let r = rows[i];
        let s = 0;
        let j = 0;
        try {
            while j < r.len() {
                s = s + r[j];
                j += 1;
            }
            println("row " + string(i) + "=" + string(s));
        } catch (e) {
            println("row " + string(i) + " err s=" + string(s) + " j=" + string(j));
        }
        i += 1;
    }
}
fn main() {
    sum_rows([[1, 2, 3], [4, "x", 6], [10, 20]]);
    let m = map(0, 10, 1, 20, 2, 30);
    let s = 0;
    let j = 0;
    while j < 3 {
        s = s + m[j];
        j += 1;
    }
    println("map=" + string(s));
    return 0;
}
"#;
    assert_native_matches_tree("p1_mixed_fallback", src);
}

#[test]
fn phase1_aliasing_and_invalidation_safety() {
    if !native_available() {
        eprintln!("skip native test: LLVM-C.dll not found");
        return;
    }
    let src = r#"
fn dot2(a, b, n: int) {
    let s = 0;
    let j = 0;
    while j < n {
        s = s + a[j] * b[j];
        j += 1;
    }
    return s;
}
fn sum_all(r) {
    let s = 0;
    let j = 0;
    while j < r.len() {
        s = s + r[j];
        j += 1;
    }
    return s;
}
fn main() {
    let mat = [[1, 2, 3, 4]];
    let ai = mat[0];
    let bi = mat[0];
    println("alias-dot=" + string(dot2(ai, bi, 4)));

    ai.push(5);
    println("after-push=" + string(sum_all(bi)) + " dot=" + string(dot2(ai, bi, 5)));

    bi.set(1, 200);
    println("after-set=" + string(sum_all(ai)));

    ai.pop();
    println("after-pop=" + string(sum_all(bi)));

    bi.reverse();
    println("after-rev=" + string(ai[0]) + "," + string(sum_all(ai)));

    ai.clear();
    println("after-clear-len=" + string(bi.len()) + " sum=" + string(sum_all(bi)));
    return 0;
}
"#;
    assert_native_matches_tree("p1_alias_inval", src);
}

#[test]
fn phase1_overflow_and_break_writeback_inside_try() {
    if !native_available() {
        eprintln!("skip native test: LLVM-C.dll not found");
        return;
    }
    let src = r#"
fn main() {
    let ai = [1, 2, 1, 9223372036854775800];
    let btk = [10, 20, 30, 2];
    let s = 0;
    let j = 0;
    let n = 4;
    try {
        while j < n {
            s = s + ai[j] * btk[j];
            j += 1;
        }
    } catch (e) {
        println("CAUGHT " + string(e));
    }
    println("POST_OVF s=" + string(s) + " j=" + string(j));

    let sb = 0;
    let jb = 0;
    while jb < 5 {
        if jb == 3 {
            break;
        }
        sb = sb + ai[jb] * 5;
        jb += 1;
    }
    println("BREAK s=" + string(sb) + " j=" + string(jb));

    let sc = 0.0;
    let jc = 0;
    let fl = [1.5, 2.5, 4.0, 8.0];
    while jc < 4 {
        jc += 1;
        if jc == 2 {
            continue;
        }
        sc = sc + fl[jc - 1];
    }
    println("CONT s=" + string(sc) + " j=" + string(jc));

    let sf = 1.0;
    let jf = 0;
    let f2 = [2.0, 0.0, 4.0];
    try {
        while jf < 3 {
            sf = sf + 10.0 / f2[jf];
            jf += 1;
        }
    } catch (e) {
        println("FDIV_ERR " + string(e));
    }
    println("POST_FDIV s=" + string(sf) + " j=" + string(jf));
    return 0;
}
"#;
    assert_native_matches_tree("p1_ovf_break", src);
}

#[test]
fn phase1_multithreaded_fallback_safety() {
    if !native_available() {
        eprintln!("skip native test: LLVM-C.dll not found");
        return;
    }
    let src = r#"
fn main() {
    let t = spawn fn() {
        return 42;
    };
    println("spawned=" + string(t.join()));
    let a = [[1, 2, 3], [4, 5, 6]];
    let bt = [[7, 8, 9], [10, 11, 12]];
    let total = 0;
    let i = 0;
    while i < 2 {
        let ai = a[i];
        let k = 0;
        while k < 2 {
            let btk = bt[k];
            let s = 0;
            let j = 0;
            while j < 3 {
                s = s + ai[j] * btk[j];
                j += 1;
            }
            total = total + s;
            k += 1;
        }
        i += 1;
    }
    println("mt-total=" + string(total));
    return 0;
}
"#;
    assert_native_matches_tree("p1_mt_fallback", src);
}

#[test]
fn phase2_string_builders_sso_rope_ring_and_unicode_len_match_tree() {
    if !native_available() {
        eprintln!("skip native test: LLVM-C.dll not found");
        return;
    }
    let src = r#"
fn shadow_param(string) {
    return "param:" + string;
}

fn shadow_local() {
    let string = fn(n) { return "S"; };
    return "local:" + string(9);
}

fn main() {
    let s0 = "";
    let s1 = "abc";
    let s2 = "hello_phase2_longer_than_sso";
    let u1 = "café";
    let u2 = "héllo 🌍 world café!";
    println(s0.len(), s1.len(), s2.len(), u1.len(), u2.len());

    let kept = [];
    let i = 0;
    while i < 14 {
        let r = "prefix_longer_than_fifteen:" + string(i);
        kept.push(r);
        println(r.len());
        i += 1;
    }
    for item in kept {
        println(item);
    }

    try {
        println("ok" + 42 + string(1 / 0));
    } catch (e) {
        println("err=" + string(e));
    }

    let obj = {
        "tag": "obj",
        "__radd__": fn(self_m, o) {
            return o + ":" + self_m["tag"];
        }
    };
    println("x" + obj);
    println(shadow_param("abc"));
    println(shadow_local());
    return 0;
}
"#;
    assert_native_matches_tree("p2_str_builders", src);
}

#[test]
fn phase2_inline_v2_binop_overflow_and_float_fallback_match_tree() {
    if !native_available() {
        eprintln!("skip native test: LLVM-C.dll not found");
        return;
    }
    let src = r#"
fn box_pair(a, b) {
    let xs = [a, b];
    return xs;
}

fn main() {
    let vals = [
        9223372036854775807,
        1,
        -9223372036854775808,
        2,
        1.5,
        2.25,
        "ab",
        "cd",
        10,
        3
    ];
    let max_i = vals[0];
    let one = vals[1];
    let min_i = vals[2];
    let two = vals[3];
    let f1 = vals[4];
    let f2 = vals[5];
    let s1 = vals[6];
    let s2 = vals[7];
    let ten = vals[8];
    let three = vals[9];

    println(ten + three, ten - three, ten * three, ten / three, ten % three);
    println(ten == ten, ten != three, three < ten, three <= three, ten > three, ten >= ten);
    println(one + f1, f2 + two, 1 == f1, 1.5 == f1, one < f1, f2 >= two);
    println(s1 + s2);

    try { println(max_i + one); } catch (e) { println("add_ovf=" + string(e)); }
    try { println(min_i - one); } catch (e) { println("sub_ovf=" + string(e)); }
    try { println(max_i * two); } catch (e) { println("mul_ovf=" + string(e)); }
    try { println(ten / (one - one)); } catch (e) { println("div0=" + string(e)); }
    try { println(min_i / (0 - one)); } catch (e) { println("min_div=" + string(e)); }
    return 0;
}
"#;
    assert_native_matches_tree("p2_v2_binop", src);
}

#[test]
fn phase2_prehashed_map_and_json_roundtrip_match_tree() {
    if !native_available() {
        eprintln!("skip native test: LLVM-C.dll not found");
        return;
    }
    let src = r#"
fn main() {
    let small = {"alpha": 10, "beta": 20, "gamma": "café"};
    println(small["alpha"], small["beta"], small["gamma"]);
    try {
        println(small["absent"]);
    } catch (e) {
        println("miss=" + string(e));
    }

    let big = {};
    let i = 0;
    while i < 24 {
        big["k" + string(i)] = i * 3 + 1;
        i += 1;
    }
    println(big["k0"], big["k7"], big["k17"], big["k23"]);
    let dk = "k" + string(12);
    println(big[dk]);

    let raw = "{\"s\":\"a\\\"b\\\\c\\n\\t\\u00e9\",\"uni\":\"héllo\",\"pos\":42,\"neg\":-17,\"max\":9223372036854775807,\"min\":-9223372036854775808,\"big\":9999999999999999999,\"f1\":1e5,\"f2\":-2.5e-3,\"arr\":[],\"obj\":{}}";
    let parsed = json_parse(raw);
    println(parsed["s"], parsed["uni"], parsed["pos"], parsed["neg"]);
    println(parsed["max"], parsed["min"]);
    println(json_write(parsed["arr"]), json_write(parsed["obj"]));
    let rt = json_parse(json_write(parsed));
    println(rt["pos"] == parsed["pos"], rt["s"] == parsed["s"], rt["f1"] == parsed["f1"]);

    let cyc = [];
    cyc.push(cyc);
    try {
        println(json_write(cyc));
    } catch (e) {
        println("cyc=" + string(e));
    }
    return 0;
}
"#;
    assert_native_matches_tree("p2_map_json", src);
}

#[test]
fn phase2_fast_mutex_transition_on_spawn_matches_tree() {
    if !native_available() {
        eprintln!("skip native test: LLVM-C.dll not found");
        return;
    }
    let src = r#"
fn main() {
    let shared = {"init": 100};
    let items = [1, 2, 3, 4];
    shared["pre"] = 55;
    items.push(5);

    let t1 = spawn fn() {
        shared["w1"] = items[0] + items[4];
        return shared["init"];
    };
    let t2 = spawn fn() {
        shared["w2"] = shared["pre"] + items[2];
        return 2;
    };
    let t3 = spawn fn() {
        shared["w3"] = items[1] * 10;
        return 3;
    };
    let r1 = t1.join();
    let r2 = t2.join();
    let r3 = t3.join();

    println(r1, r2, r3, shared["init"], shared["pre"], shared["w1"], shared["w2"], shared["w3"]);
    return 0;
}
"#;
    assert_native_matches_tree("p2_fast_mutex_spawn", src);
}

#[test]
fn phase2_hoisted_map_loop_mutation_fallback_and_str_key_var_match_tree() {
    if !native_available() {
        eprintln!("skip native test: LLVM-C.dll not found");
        return;
    }
    let src = r#"
fn sum_map_loop(m, n: int) {
    let i = 0;
    let s = 0;
    while i < n {
        s = s + m["k" + string(i % 4)];
        i = i + 1;
    }
    return s;
}

fn mutate_long_key(m) {
    m["this_key_is_over_15_bytes"] = 999;
}

fn mutate_str_val(m) {
    m["k0"] = "not_an_int";
}

fn sum_with_str_local(m) -> int {
    let k = "beta";
    let i = 0;
    let s = 0;
    while i < 5 {
        s = s + m[k];
        i = i + 1;
    }
    return s;
}

fn sum_until_missing(m) -> int {
    let i = 0;
    let s = 0;
    try {
        while i < 5 {
            s = s + m["k" + string(i)];
            i = i + 1;
        }
    } catch (e) {
        return s * 10 + i;
    }
    return s;
}

fn main() {
    let m = {"k0": 10, "k1": 20, "k2": 30, "k3": 40};
    println(sum_map_loop(m, 8));
    mutate_long_key(m);
    println(sum_map_loop(m, 8));

    let m2 = {"alpha": 3, "beta": 7};
    println(sum_with_str_local(m2));

    let m3 = {"k0": 5, "k1": 6, "k2": 7};
    println(sum_until_missing(m3));

    mutate_str_val(m);
    try {
        println(sum_map_loop(m, 4));
    } catch (e) {
        println("caught_type=" + string(e));
    }
    return 0;
}
"#;
    assert_native_matches_tree("p2_hoisted_map_fallback", src);
}

#[test]
fn phase2_hoisted_loops_reject_shadowed_string_and_dunder_mutation() {
    if !native_available() {
        eprintln!("skip native test: LLVM-C.dll not found");
        return;
    }
    let src = r#"
let MAP_HOLDER = [];
let ROW_HOLDER = [];
let LIST_HOLDER = [];

fn string(x: int) -> string {
    let m = MAP_HOLDER[0];
    m["a"] = x * 100;
    m["b"] = x * 100;
    m["c"] = x * 100;
    m["d"] = x * 100;
    return "d";
}

fn test_shadowed_string_in_map_loop() -> int {
    let m = {"a": 1, "b": 1, "c": 1, "d": 1};
    MAP_HOLDER.clear();
    MAP_HOLDER.push(m);
    let i = 1;
    let s = 0;
    while i <= 3 {
        s = s + m[string(i)];
        i = i + 1;
    }
    return s;
}

fn test_dunder_in_map_loop() -> int {
    let m = {"a": 1, "b": 1, "c": 1, "d": 1};
    MAP_HOLDER.clear();
    MAP_HOLDER.push(m);
    let evil = {
        "__neg__": fn(self) {
            let target = MAP_HOLDER[0];
            target["d"] = 200;
            return 0;
        }
    };
    let i = 0;
    let s = 0;
    while i < 3 {
        let d = -evil;
        s = s + m["d"] + d;
        i = i + 1;
    }
    return s;
}

let GLOBAL_ROW_EVIL = {
    "__neg__": fn(self) {
        let row = ROW_HOLDER[0];
        row.clear();
        let k = 0;
        while k < 16 {
            row.push(200);
            k = k + 1;
        }
        return 0;
    }
};

fn test_dunder_in_row_loop() -> int {
    let grid = [[1, 2, 3, 4, 5, 6, 7, 8]];
    let row = grid[0];
    ROW_HOLDER.clear();
    ROW_HOLDER.push(row);
    let i = 0;
    let s = 0;
    while i < 3 {
        let d = -GLOBAL_ROW_EVIL;
        s = s + row[5] + d;
        i = i + 1;
    }
    return s;
}

fn test_dunder_in_readonly_len_loop() -> int {
    let xs = [[10, 20, 30]];
    LIST_HOLDER.clear();
    LIST_HOLDER.push(xs);
    let evil = {
        "__neg__": fn(self) {
            let target = LIST_HOLDER[0];
            target.clear();
            let k = 0;
            while k < 16 {
                target.push([50, 60, 70]);
                k = k + 1;
            }
            return 0;
        }
    };
    let i = 0;
    let s = 0;
    while i < xs.len() {
        let row = xs[i];
        let d = -evil;
        s = s + row[0] + d;
        i = i + 1;
    }
    return s;
}

fn main() {
    println(test_shadowed_string_in_map_loop());
    println(test_dunder_in_map_loop());
    println(test_dunder_in_row_loop());
    println(test_dunder_in_readonly_len_loop());
    return 0;
}
"#;
    assert_native_matches_tree("p2_hoisted_dunder_safety", src);
}
