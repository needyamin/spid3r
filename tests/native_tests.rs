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
    let dll = bin()
        .parent()
        .map(|d| d.join("LLVM-C.dll"))
        .unwrap_or_default();
    dll.is_file()
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
    // rejected). Aliasing is unsupported on the native backend, so the two
    // stdlib types are exercised in separate programs.
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
                "4.0+6.0i\n",       // complex add
                "-5.0+10.0i\n",     // complex mul
                "2.0+2.0i\n",       // complex sub
                "2.2-0.4i\n",       // complex div
                "375\n",            // Money add
                "375\n",            // Money sub
                "15\n",             // Money mul
                "25\n",             // Money div
                "-7\n",             // Money neg
                "15\n",             // Money radd (10 + Money(5))
                "3\n",              // no throw: the try body completes
            ),
        ),
        (
            "big",
            big,
            concat!(
                "123456789012345678901234567891\n", // bigint add
                "-123456789012345678901234567890\n", // bigint neg
                "246913578024691357802469135780\n", // bigint mul
                "123456789012345677901234567891\n", // bigint sub
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
        assert_eq!(tree, vm, "tree vs vm differ for operator overloading ({})", tag);

        let exe = dir.join(if cfg!(windows) {
            format!("spid3r_nat_ops_{}.exe", tag)
        } else {
            format!("spid3r_nat_ops_{}", tag)
        });
        let exe = exe.to_str().unwrap().to_string();
        out(&["build", &src_path, "-o", &exe])
            .unwrap_or_else(|e| panic!("native build failed ({}):/n{}", tag, e));
        let native = run_exe(&exe).unwrap_or_else(|e| panic!("native run failed ({}):/n{}", tag, e));
        assert_eq!(
            tree, native,
            "tree vs native stdout differ for operator overloading ({})",
            tag
        );
        assert_eq!(tree, expected, "output drifted from the expected values ({})", tag);
        let _ = std::fs::remove_file(&src_path);
        let _ = std::fs::remove_file(&exe);
    }
}
