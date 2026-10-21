//! Three-engine conformance matrix (roadmap Stage 1).
//!
//! Every statement and expression form in `src/ast/mod.rs` has a case here, and
//! each case is run on the tree interpreter, the bytecode VM and (for a
//! representative subset) the LLVM AOT backend. Three things are asserted:
//!
//! 1. **stdout matches the expected text** — so two engines cannot agree on the
//!    wrong answer. This is what makes the matrix a conformance test rather than
//!    a differential test.
//! 2. **stderr and the exit code match** — the engines must not only agree on
//!    output but on *failure*: the same phase (`error[runtime]`, not
//!    `error[vm]`) and the same message. The existing differential harness
//!    (`spid3r test`) compares stdout only, which is why a phase mismatch
//!    survived until this file existed.
//! 3. **the AOT subset matches too** — a construct that compiles but computes a
//!    different answer is worse than one that is rejected.
//!
//! This is also the gate for `src/lower`: a lowering rule that changes meaning
//! fails here, on every engine, instead of in a user's program.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

#[cfg(feature = "native")]
fn native_available() -> bool {
    bin()
        .parent()
        .map(|d| d.join("LLVM-C.dll"))
        .unwrap_or_default()
        .is_file()
}

struct Case {
    name: &'static str,
    src: &'static str,
    /// Exact expected stdout (compared after trimming the trailing newline).
    out: &'static str,
    code: i32,
    /// First stderr line, for the cases that must fail.
    err: Option<&'static str>,
    /// Run through the AOT backend as well.
    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    aot: bool,
}

/// Run `src` on `engine` and return `(exit_code, stdout, first_stderr_line)`.
fn run_engine(engine: &str, name: &str, src: &str) -> (i32, String, String) {
    // A private directory: the AOT test runs in parallel and must not share
    // file names with this one.
    let dir = std::env::temp_dir().join("spid3r_matrix_treem");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{}.matrix", name));
    std::fs::write(&path, src).expect("write case");
    let o = Command::new(bin())
        .args(["run", "--engine", engine, path.to_str().unwrap()])
        .output()
        .expect("spawn spid3r");
    let _ = std::fs::remove_file(&path);
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    let first = err.lines().next().unwrap_or("").trim().to_string();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        first,
    )
}

/// Run the AOT backend and return `(exit_code, stdout, first_stderr_line)`.
#[cfg(feature = "native")]
fn run_aot(name: &str, src: &str) -> Option<(i32, String, String)> {
    let dir = std::env::temp_dir().join("spid3r_matrix_aot");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let src_path = dir.join(format!("{}.matrix", name));
    std::fs::write(&src_path, src).expect("write case");
    let exe = dir.join(format!("{}{}", name, std::env::consts::EXE_SUFFIX));
    let build = Command::new(bin())
        .args([
            "build",
            src_path.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("spawn spid3r build");
    let _ = std::fs::remove_file(&src_path);
    if !build.status.success() {
        let msg = String::from_utf8_lossy(&build.stderr).to_string();
        panic!("[{} / aot] build failed: {}", name, msg);
    }
    let o = Command::new(&exe).output().expect("run aot binary");
    let _ = std::fs::remove_file(&exe);
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    Some((
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        err.lines().next().unwrap_or("").trim().to_string(),
    ))
}

const CASES: &[Case] = &[
    // ---------------------------------------------------------------- literals
    Case {
        name: "literals",
        src: "fn main() {\n    println(42);\n    println(1.5);\n    println(\"hi\");\n    println(true);\n    println(false);\n    println(null);\n    return 0;\n}\n",
        out: "42\n1.5\nhi\ntrue\nfalse\nnull",
        code: 0,
        err: None,
        aot: true,
    },
    // ---------------------------------------------------------------- operators
    Case {
        name: "arith",
        src: "fn main() {\n    println(2 + 3 * 4);\n    println((2 + 3) * 4);\n    println(7 / 2);\n    println(7 % 2);\n    println(-5 + 2);\n    println(2.5 * 2);\n    return 0;\n}\n",
        out: "14\n20\n3\n1\n-3\n5.0",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "string_ops",
        src: "fn main() {\n    println(\"ab\" + \"cd\");\n    println(\"abc\" == \"abc\");\n    println(\"abc\" != \"abd\");\n    println(\"abc\".len());\n    println(\"abc\".upper());\n    println(\"abc\".slice(1, 3));\n    return 0;\n}\n",
        out: "abcd\ntrue\ntrue\n3\nABC\nbc",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "logical_shortcircuit",
        src: "fn boom() {\n    println(\"boom\");\n    return true;\n}\nfn main() {\n    println(false and boom());\n    println(true or boom());\n    println(true and boom());\n    return 0;\n}\n",
        // `boom` must not run for the first two: short-circuit, not eager eval.
        out: "false\ntrue\nboom\ntrue",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "compound_assign",
        src: "fn main() {\n    let x = 10;\n    x += 5;\n    println(x);\n    x -= 3;\n    println(x);\n    x *= 2;\n    println(x);\n    x /= 4;\n    println(x);\n    return 0;\n}\n",
        out: "15\n12\n24\n6",
        code: 0,
        err: None,
        aot: true,
    },
    // ----------------------------------------------------------------- bindings
    Case {
        name: "let_const_annot",
        src: "fn main() {\n    let a = 1;\n    let b: int = 2;\n    const C = 3;\n    println(a + b + C);\n    let s: string = \"x\";\n    println(s);\n    return 0;\n}\n",
        out: "6\nx",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "global_let",
        src: "let g = 10;\nfn main() {\n    println(g);\n    return 0;\n}\n",
        out: "10",
        code: 0,
        err: None,
        aot: false,
    },
    // ---------------------------------------------------------------- branching
    Case {
        name: "if_elif_else",
        src: "fn grade(n) {\n    if (n > 90) {\n        return \"A\";\n    } else {\n        if (n > 80) {\n            return \"B\";\n        } else {\n            return \"C\";\n        }\n    }\n}\nfn main() {\n    println(grade(95));\n    println(grade(85));\n    println(grade(10));\n    return 0;\n}\n",
        out: "A\nB\nC",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "nested_blocks",
        src: "fn main() {\n    let x = 1;\n    {\n        let x = 2;\n        println(x);\n    }\n    println(x);\n    return 0;\n}\n",
        // A nested block introduces a scope; the outer binding is untouched.
        out: "2\n1",
        code: 0,
        err: None,
        aot: true,
    },
    // ------------------------------------------------------------------- loops
    Case {
        name: "while_break_continue",
        src: "fn main() {\n    let i = 0;\n    while (i < 10) {\n        i = i + 1;\n        if (i == 3) { continue; }\n        if (i == 6) { break; }\n        println(i);\n    }\n    return 0;\n}\n",
        out: "1\n2\n4\n5",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "for_in_list",
        src: "fn main() {\n    let total = 0;\n    for x in [1, 2, 3] {\n        total = total + x;\n    }\n    println(total);\n    for ch in \"abc\" {\n        println(ch);\n    }\n    return 0;\n}\n",
        out: "6\na\nb\nc",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "for_in_map",
        src: "fn main() {\n    let m = map();\n    m[\"a\"] = 1;\n    m[\"b\"] = 2;\n    let n = 0;\n    for k in m {\n        n = n + 1;\n        println(k);\n    }\n    println(n);\n    return 0;\n}\n",
        // Insertion order, and iteration yields the KEYS.
        out: "a\nb\n2",
        code: 0,
        err: None,
        aot: false,
    },
    Case {
        name: "for_c",
        src: "fn main() {\n    let total = 0;\n    for let i = 0; i < 4; i = i + 1 {\n        total = total + i;\n    }\n    println(total);\n    return 0;\n}\n",
        out: "6",
        code: 0,
        err: None,
        aot: true,
    },
    // --------------------------------------------------------------- functions
    Case {
        name: "fn_recursion",
        src: "fn fib(n) {\n    if (n < 2) { return n; }\n    return fib(n - 1) + fib(n - 2);\n}\nfn main() {\n    println(fib(10));\n    return 0;\n}\n",
        out: "55",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "closures",
        src: "fn counter() {\n    let n = 0;\n    return fn() {\n        n = n + 1;\n        return n;\n    };\n}\nfn main() {\n    let c = counter();\n    println(c());\n    println(c());\n    let d = counter();\n    println(d());\n    return 0;\n}\n",
        // Two closures capture *separate* cells; the first keeps its own count.
        out: "1\n2\n1",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "higher_order",
        src: "fn main() {\n    let xs = [1, 2, 3, 4];\n    println(xs.map(fn(x) { return x * 2; }));\n    println(xs.filter(fn(x) { return x > 2; }));\n    println(xs.reduce(0, fn(a, b) { return a + b; }));\n    return 0;\n}\n",
        out: "[2, 4, 6, 8]\n[3, 4]\n10",
        code: 0,
        err: None,
        aot: true,
    },
    // ----------------------------------------------------------------- classes
    Case {
        name: "class_basic",
        src: "class Point {\n    public let x;\n    public let y;\n    fn new(x, y) {\n        self.x = x;\n        self.y = y;\n    }\n    public fn sum(self) {\n        return self.x + self.y;\n    }\n}\nfn main() {\n    let p = new Point(2, 3);\n    println(p.x);\n    println(p.sum());\n    return 0;\n}\n",
        out: "2\n5",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "class_inherit",
        src: "class Animal {\n    public let name;\n    fn new(name) { self.name = name; }\n    public fn noise(self) { return \"...\"; }\n    public fn describe(self) { return self.name + \" says \" + self.noise(); }\n}\nclass Dog extends Animal {\n    fn new(name) { self.name = name; }\n    public fn noise(self) { return \"woof\"; }\n}\nfn main() {\n    let d = new Dog(\"rex\");\n    println(d.describe());\n    return 0;\n}\n",
        // Virtual dispatch: `describe` runs on Animal but calls Dog.noise.
        out: "rex says woof",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "interface",
        src: "interface Named {\n    fn name(self) -> string;\n}\nclass Person implements Named {\n    public let n;\n    fn new(n) { self.n = n; }\n    public fn name(self) { return self.n; }\n}\nfn main() {\n    let p = new Person(\"ada\");\n    println(p.name());\n    return 0;\n}\n",
        out: "ada",
        code: 0,
        err: None,
        aot: false,
    },
    Case {
        name: "field_assign",
        src: "class Box {\n    public let v;\n    fn new() { self.v = 0; }\n}\nfn main() {\n    let b = new Box();\n    b.v = 5;\n    println(b.v);\n    return 0;\n}\n",
        out: "5",
        code: 0,
        err: None,
        aot: true,
    },
    // ------------------------------------------------------- indexing & access
    Case {
        name: "index_and_assign",
        src: "fn main() {\n    let xs = [1, 2, 3];\n    println(xs[0]);\n    xs[0] = 9;\n    println(xs[0]);\n    println(xs.len());\n    let m = map();\n    m[\"k\"] = 7;\n    println(m[\"k\"]);\n    println(\"abc\"[1]);\n    return 0;\n}\n",
        out: "1\n9\n3\n7\nb",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "collection_literals",
        src: "fn main() {\n    println([1, 2]);\n    println([]);\n    let t = tuple(1, \"a\");\n    println(t.len());\n    let s = set(1, 2, 2);\n    println(s.len());\n    return 0;\n}\n",
        out: "[1, 2]\n[]\n2\n2",
        code: 0,
        err: None,
        aot: false,
    },
    Case {
        name: "string_methods",
        src: "fn main() {\n    println(\"a,b,c\".split(\",\"));\n    println(\" x \".trim());\n    println(\"abc\".index_of(\"b\"));\n    println(\"abc\".starts_with(\"ab\"));\n    println(\"ab\".repeat(2));\n    return 0;\n}\n",
        out: "[a, b, c]\nx\n1\ntrue\nabab",
        code: 0,
        err: None,
        aot: false,
    },
    Case {
        name: "null_handling",
        src: "fn main() {\n    let x = null;\n    println(x == null);\n    println(x != null);\n    println(x);\n    return 0;\n}\n",
        out: "true\nfalse\nnull",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "float_format",
        src: "fn main() {\n    println(4.0);\n    println(1.25);\n    println(0.1 + 0.2);\n    return 0;\n}\n",
        out: "4.0\n1.25\n0.30000000000000004",
        code: 0,
        err: None,
        aot: true,
    },
    // ------------------------------------------------------------- exceptions
    Case {
        name: "try_catch",
        src: "fn main() {\n    try {\n        throw \"bad\";\n    } catch (e) {\n        println(\"caught \" + e);\n    }\n    return 0;\n}\n",
        out: "caught bad",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "try_finally",
        src: "fn f() {\n    try {\n        return 1;\n    } finally {\n        println(\"finally\");\n    }\n}\nfn main() {\n    println(f());\n    return 0;\n}\n",
        // `finally` runs on the return path, before the caller sees the value.
        out: "finally\n1",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "try_nested_rethrow",
        src: "fn main() {\n    try {\n        try {\n            throw \"inner\";\n        } catch (e) {\n            throw \"outer:\" + e;\n        }\n    } catch (e2) {\n        println(e2);\n    }\n    return 0;\n}\n",
        out: "outer:inner",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "throw_value",
        src: "fn main() {\n    try {\n        let m = map();\n        m[\"code\"] = 7;\n        throw m;\n    } catch (e) {\n        println(type_of(e));\n        println(e[\"code\"]);\n    }\n    return 0;\n}\n",
        // A thrown value crosses the catch boundary as the value, not a string.
        out: "map\n7",
        code: 0,
        err: None,
        aot: true,
    },
    // ------------------------------------------------------------ concurrency
    Case {
        name: "spawn_join",
        src: "fn main() {\n    let t = spawn fn() {\n        return 5;\n    };\n    println(t.join());\n    return 0;\n}\n",
        out: "5",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "async_await",
        src: "async fn run() {\n    let t = spawn fn() {\n        return 7;\n    };\n    println(await t);\n    return 0;\n}\nfn main() {\n    run();\n    return 0;\n}\n",
        out: "7",
        code: 0,
        err: None,
        aot: false,
    },
    // ------------------------------------------------------------ error paths
    Case {
        name: "err_shadowing",
        src: "fn main() {\n    let x = 1;\n    let x = x + 1;\n    println(x);\n    return 0;\n}\n",
        out: "",
        code: 1,
        err: Some("error[semantic]: 'x' is already declared in this scope"),
        aot: false,
    },
    Case {
        name: "err_await_outside_async",
        src: "fn main() {\n    let t = spawn fn() { return 1; };\n    println(await t);\n    return 0;\n}\n",
        out: "",
        code: 1,
        err: Some("error[semantic]: await is only allowed inside an async function"),
        aot: false,
    },
    Case {
        name: "err_index_oob",
        src: "fn main() {\n    let xs = [1];\n    println(xs[5]);\n    return 0;\n}\n",
        out: "",
        code: 1,
        // Was `error[vm]` on the VM and `error[runtime]` on the tree engine.
        err: Some("error[runtime]: index 5 out of bounds for length 1"),
        aot: false,
    },
    Case {
        name: "err_div_zero",
        src: "fn main() {\n    println(1 / 0);\n    return 0;\n}\n",
        out: "",
        code: 1,
        err: Some("error[runtime]: division by zero"),
        aot: false,
    },
];

#[test]
fn every_construct_behaves_identically_on_tree_and_vm() {
    let mut failures = Vec::new();
    for case in CASES {
        for engine in ["tree", "vm"] {
            let got = run_engine(engine, case.name, case.src);
            // Collect instead of panicking so one report lists every divergence.
            let expected = (case.code, case.out.trim_end().to_string());
            if got.0 != expected.0 || got.1.trim_end() != expected.1 {
                failures.push(format!(
                    "[{} / {}] exit {} stdout {:?} (expected exit {} stdout {:?})",
                    case.name, engine, got.0, got.1, case.code, case.out
                ));
                continue;
            }
            match case.err {
                Some(e) if got.2 != e => failures.push(format!(
                    "[{} / {}] error {:?} (expected {:?})",
                    case.name, engine, got.2, e
                )),
                None if !got.2.is_empty() => failures.push(format!(
                    "[{} / {}] unexpected stderr {:?}",
                    case.name, engine, got.2
                )),
                _ => {}
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} conformance failure(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The AOT subset: one case per construct family the native backend claims to
/// support. A construct that compiles but computes a different answer is worse
/// than one that is rejected, so these compare against the same expectations.
#[cfg(feature = "native")]
#[test]
fn the_aot_subset_matches_the_same_expectations() {
    if !native_available() {
        eprintln!("skip: LLVM-C.dll not found next to {}", bin().display());
        return;
    }
    let mut failures = Vec::new();
    for case in CASES.iter().filter(|c| c.aot) {
        let (code, out, first_err) = run_aot(case.name, case.src).expect("aot");
        if code != case.code || out.trim_end() != case.out.trim_end() {
            failures.push(format!(
                "[{} / aot] exit {} stdout {:?} (expected exit {} stdout {:?})",
                case.name, code, out, case.code, case.out
            ));
            continue;
        }
        if let Some(e) = case.err {
            if first_err != e {
                failures.push(format!(
                    "[{} / aot] error {:?} (expected {:?})",
                    case.name, first_err, e
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} AOT conformance failure(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// `import` needs real files, so it is covered here rather than in the table.
/// Both the interpreter and the VM must resolve the module and see the same
/// values, including a `public const` staying immutable.
#[test]
fn imports_behave_identically_on_tree_and_vm() {
    let dir = std::env::temp_dir().join("spid3r_matrix_import");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join("util.matrix"),
        "public const LIMIT = 3;\npublic fn double(n) {\n    return n * 2;\n}\nfn hidden() {\n    return 0;\n}\n",
    )
    .expect("write module");
    let main_src = "import \"util\";\nfn main() {\n    println(double(4));\n    println(LIMIT);\n    return 0;\n}\n";
    let main_path = dir.join("main.matrix");
    std::fs::write(&main_path, main_src).expect("write main");

    for engine in ["tree", "vm"] {
        let o = Command::new(bin())
            .args(["run", "--engine", engine, main_path.to_str().unwrap()])
            .current_dir(&dir)
            .output()
            .expect("spawn");
        let out = String::from_utf8_lossy(&o.stdout).to_string();
        assert_eq!(
            out.trim_end(),
            "8\n3",
            "[import / {}] stdout (stderr: {})",
            engine,
            String::from_utf8_lossy(&o.stderr)
        );
    }

    // A private module binding must not leak into the importer.
    let leak = "import \"util\";\nfn main() {\n    println(hidden());\n    return 0;\n}\n";
    let leak_path = dir.join("leak.matrix");
    std::fs::write(&leak_path, leak).expect("write leak");
    let o = Command::new(bin())
        .args(["run", "--engine", "tree", leak_path.to_str().unwrap()])
        .current_dir(&dir)
        .output()
        .expect("spawn");
    assert_ne!(
        o.status.code().unwrap_or(-1),
        0,
        "a private import must fail"
    );
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(
        err.contains("hidden"),
        "the error should name the private binding: {}",
        err
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `src/lower` is the identity today, so the lowered tree must be structurally
/// equal to the parsed one. When a desugaring rule lands, this test is the place
/// that says *what* it changed — and `--dump-lowered` shows it.
#[test]
fn lowering_is_the_identity_today() {
    let src = "fn main() {\n    let xs = [1, 2, 3];\n    for x in xs {\n        println(x);\n    }\n    try {\n        throw \"e\";\n    } catch (e) {\n        println(e);\n    }\n    return 0;\n}\n";
    let program =
        spid3r_lang::parser::parse_file(src, std::sync::Arc::from("lower.matrix")).expect("parse");
    let lowered = spid3r_lang::lower::lower(&program);
    assert_eq!(
        format!("{:?}", program),
        format!("{:?}", lowered.as_ref()),
        "the lowering pass changed the tree; update this test and the Stage-2 notes"
    );
    // And it must be idempotent, because a backend can be handed an already
    // lowered program (the VM compiles module programs through the same entry).
    let twice = spid3r_lang::lower::lower(lowered.as_ref());
    assert_eq!(
        format!("{:?}", lowered.as_ref()),
        format!("{:?}", twice.as_ref()),
        "lower() is not idempotent"
    );
}
