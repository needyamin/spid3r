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
    if cfg!(windows) {
        bin()
            .parent()
            .map(|d| d.join("LLVM-C.dll"))
            .unwrap_or_default()
            .is_file()
    } else {
        std::process::Command::new("cc")
            .arg("--version")
            .output()
            .is_ok()
            || std::process::Command::new("clang")
                .arg("--version")
                .output()
                .is_ok()
    }
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
    // ------------------------------------------- logical negation over `or`
    // `not (A or B)` used to answer the wrong value on the VM when `A` was
    // falsy and the right operand actually ran (docs/VM_NOTES.md). Fixed, but
    // no example exercises it, so it is pinned here on all three engines.
    Case {
        name: "not_over_or",
        src: "fn f(s) {\n    let t = s.trim();\n    if not (t == \"x\" or t == \"- a\") {\n        return \"no\";\n    }\n    return \"yes\";\n}\nfn main() {\n    println(f(\"x\"));\n    println(f(\"y\"));\n    return 0;\n}\n",
        out: "yes\nno",
        code: 0,
        err: None,
        aot: true,
    },
    // The original `not_over_or` never evaluates the right operand to TRUE.
    // That hole let the `[Not][JumpIfFalse]` peephole delete a `Not` that was
    // still the `or` merge target, so the B-path jump landed on the then
    // branch. `--no-opt` was always correct; the optimizer was not.
    Case {
        name: "not_over_or_b_true",
        src: "fn pick(s) {\n    if not (s == \"x\" or s == \"y\") {\n        return \"taken\";\n    }\n    return \"else\";\n}\nfn main() {\n    println(pick(\"x\"));\n    println(pick(\"y\"));\n    println(pick(\"z\"));\n    println(not (false or true));\n    if (not (false or true)) { println(\"IF_TAKEN\"); } else { println(\"IF_ELSE\"); }\n    return 0;\n}\n",
        out: "else\nelse\ntaken\nfalse\nIF_ELSE",
        code: 0,
        err: None,
        aot: true,
    },
    // Phase 3: the hybrid int compare (inline compare over runtime-checked
    // boxed leaves) originally passed the SLOT ADDRESS instead of the boxed
    // pointer to `spid3r_is_int`/`spid3r_int_of`, so the helpers read the
    // slot's own bytes as a `Value` — an ASLR-dependent is-Int verdict plus
    // adjacent-stack-garbage payload. Loops whose bounds came from boxed
    // `len()` results then ran a garbage number of iterations ~10% of runs
    // (failure indices were the bound values themselves). This case is
    // deliberately bound-sensitive: any over- or under-run changes the
    // cell/count lines or raises the out-of-bounds error.
    Case {
        name: "hybrid_loop_bounds",
        src: "fn fill(x) {\n    let r = x.len();\n    if r == 0 { return matrix(0, 0); }\n    let c = x[0].len();\n    let m = matrix(r, c);\n    for let i = 0; i < r; i = i + 1 {\n        for let j = 0; j < c; j = j + 1 {\n            m.set(i, j, float(x[i][j]));\n        }\n    }\n    return m;\n}\nfn main() {\n    let x = [[0.5, 1.2], [1.5, 0.3], [2.2, 2.8], [3.1, 1.9], [0.8, 2.1], [2.9, 0.5]];\n    let m = fill(x);\n    println(\"rows=\" + string(m.rows()) + \" cols=\" + string(m.cols()));\n    println(\"cell=\" + string(m.get(5, 1)));\n    let n = x.len();\n    let count = 0;\n    for let i = 0; i < n; i = i + 1 {\n        count += 1;\n    }\n    println(\"count=\" + string(count));\n    return 0;\n}\n",
        out: "rows=6 cols=2\ncell=0.5\ncount=6",
        code: 0,
        err: None,
        aot: true,
    },
    // ---------------------------------------------------------------- literals
    Case {
        name: "literals",
        src: "fn main() {\n    println(42);\n    println(1.5);\n    println(\"hi\");\n    println(true);\n    println(false);\n    println(null);\n    return 0;\n}\n",
        out: "42\n1.5\nhi\ntrue\nfalse\nnull",
        code: 0,
        err: None,
        aot: true,
    },
    // ------------------------------------------------------------- polish (2)
    Case {
        name: "else_if",
        src: "fn grade(n) {\n    if (n > 90) {\n        return \"A\";\n    } else if (n > 80) {\n        return \"B\";\n    } else if (n > 70) {\n        return \"C\";\n    } else {\n        return \"F\";\n    }\n}\nfn main() {\n    println(grade(95));\n    println(grade(85));\n    println(grade(75));\n    println(grade(10));\n    return 0;\n}\n",
        out: "A\nB\nC\nF",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "negative_index",
        src: "fn main() {\n    let xs = [10, 20, 30];\n    println(xs[-1]);\n    println(xs[-3]);\n    println(\"abc\"[-1]);\n    xs[-1] = 99;\n    println(xs);\n    return 0;\n}\n",
        out: "30\n10\nc\n[10, 20, 99]",
        code: 0,
        err: None,
        aot: true,
    },
    // ---------------------------------------------------------- ergonomics (3)
    Case {
        name: "destructure",
        src: "fn pair() {\n    return [10, 20];\n}\nfn main() {\n    let (a, b) = pair();\n    println(a + b);\n    let (x, y, z) = [1, 2, 3];\n    println(x * 100 + y * 10 + z);\n    return 0;\n}\n",
        out: "30\n123",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "for_kv",
        src: "fn main() {\n    let m = map();\n    m[\"one\"] = 1;\n    m[\"two\"] = 2;\n    let total = 0;\n    for (k, v) in m {\n        println(\"{k}={v}\");\n        total = total + v;\n    }\n    println(total);\n    return 0;\n}\n",
        out: "one=1\ntwo=2\n3",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "spawn_named",
        src: "fn work() {\n    return 41;\n}\nfn main() {\n    let t = spawn work;\n    println(t.join());\n    return 0;\n}\n",
        out: "41",
        code: 0,
        err: None,
        aot: true,
    },
    // ------------------------------------------------------------- errors (4)
    Case {
        name: "option_result_annotations",
        src: "fn main() {\n    let a: option<int> = some(5);\n    let b: option<int> = none();\n    let c: result<int> = ok(1);\n    let d: result<int> = err(\"e\");\n    println(a);\n    println(b);\n    println(c);\n    println(d);\n    return 0;\n}\n",
        out: "some(5)\nnone\nok(1)\nerr(e)",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "propagate",
        src: "fn half(n) {\n    if (n % 2 != 0) {\n        return err(\"odd\");\n    }\n    return ok(n / 2);\n}\nfn quarter(n) {\n    let h = half(n)?;\n    return ok(h / 2);\n}\nfn main() {\n    println(quarter(8));\n    println(quarter(3));\n    return 0;\n}\n",
        out: "ok(2)\nerr(odd)",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "error_combinators",
        src: "fn main() {\n    println(some(2).map(fn(x) { return x * 10; }));\n    println(none().ok_or(\"m\"));\n    println(err(\"e\").map_err(fn(e) { return \"E-\" + e; }));\n    println(ok(3).and_then(fn(x) { return ok(x * 2); }));\n    return 0;\n}\n",
        out: "some(20)\nerr(m)\nerr(E-e)\nok(6)",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "err_element_type",
        src: "fn main() {\n    let xs: list<int> = [\"a\"];\n    return 0;\n}\n",
        out: "",
        code: 1,
        err: Some("error[semantic]: type mismatch in 'xs': expected list<int>, got list<string>"),
        aot: false,
    },
    // I-2: an uncaught RUNTIME error prints the bare message on every engine —
    // `error[runtime]: division by zero` — not the user-throw shape. The AOT
    // trampolines used to raise through the user-throw entry point, which
    // cleared the runtime-error flag, so the binary printed
    // `uncaught throw (thrown: division by zero)`.
    Case {
        name: "uncaught_runtime_error",
        src: "fn main() {\n    let x = 1 / 0;\n    println(x);\n    return 0;\n}\n",
        out: "",
        code: 1,
        err: Some("error[runtime]: division by zero"),
        aot: true,
    },
    // I-2 parity control: an uncaught USER throw keeps the
    // `uncaught throw (thrown: <value>)` shape on all three engines.
    Case {
        name: "uncaught_user_throw",
        src: "fn main() {\n    throw \"boom\";\n    return 0;\n}\n",
        out: "",
        code: 1,
        err: Some("error[runtime]: uncaught throw (thrown: boom)"),
        aot: true,
    },
    // -------------------------------------------------------------- enums (3)
    Case {
        name: "enum_match",
        src: "enum Shape {\n    Empty,\n    Circle(radius),\n    Rect(w, h),\n}\nfn area(s) {\n    return match (s) {\n        Circle(r) => 3 * r * r,\n        Rect(w, h) => w * h,\n        Empty() => 0,\n    };\n}\nfn main() {\n    println(area(Circle(2)));\n    println(area(Rect(3, 4)));\n    println(area(Empty()));\n    return 0;\n}\n",
        out: "12\n12\n0",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "match_guards",
        src: "enum Shape {\n    Empty,\n    Circle(radius),\n    Rect(w, h),\n}\nfn describe(s) {\n    return match (s) {\n        Circle(r) if (r > 10) => \"big\",\n        Circle(r) => \"small\",\n        Rect(w, h) if (w == h) => \"square\",\n        Rect(w, h) => \"rect\",\n        Empty() => \"empty\",\n    };\n}\nfn main() {\n    println(describe(Circle(20)));\n    println(describe(Circle(3)));\n    println(describe(Rect(4, 4)));\n    println(describe(Rect(4, 5)));\n    println(describe(Empty()));\n    return 0;\n}\n",
        // A failed guard falls through to the next arm.
        out: "big\nsmall\nsquare\nrect\nempty",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "match_literals",
        src: "fn classify(n) {\n    return match (n) {\n        0 => \"zero\",\n        1 => \"one\",\n        x => \"many ({x})\",\n    };\n}\nfn main() {\n    println(classify(0));\n    println(classify(1));\n    println(classify(7));\n    return 0;\n}\n",
        out: "zero\none\nmany (7)",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "err_match_not_exhaustive",
        src: "enum Color { Red, Green, Blue }\nfn name(c) {\n    return match (c) {\n        Red() => \"red\",\n        Green() => \"green\",\n    };\n}\nfn main() {\n    println(name(Red()));\n    return 0;\n}\n",
        out: "",
        code: 1,
        err: Some("error[semantic]: this match is not exhaustive for enum 'Color': missing 'Blue' (add an arm or `_`)"),
        aot: false,
    },
    // -------------------------------------------------------------- syntax (3)
    Case {
        name: "if_expression",
        src: "fn grade(n) {\n    return if (n > 90) { \"A\" } else { \"B\" };\n}\nfn main() {\n    println(grade(95));\n    println(grade(10));\n    println(if (false) { 1 });\n    let x = if (1 < 2) { 10 } else { 20 };\n    println(x);\n    return 0;\n}\n",
        // A missing `else` yields null; branches need no trailing `;`.
        out: "A\nB\nnull\n10",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "null_coalesce",
        src: "fn expensive() {\n    println(\"expensive\");\n    return 99;\n}\nfn main() {\n    println(null ?? \"d\");\n    println(\"v\" ?? \"d\");\n    println(null ?? expensive());\n    println(\"v\" ?? expensive());\n    println(null ?? null ?? \"last\");\n    return 0;\n}\n",
        // The right side must NOT be evaluated when the left is non-null.
        out: "d\nv\nexpensive\n99\nv\nlast",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "opt_field",
        src: "fn main() {\n    let m = map();\n    m[\"k\"] = 7;\n    println(m?.k);\n    let z = null;\n    println(z?.k);\n    println(z?.k ?? \"fallback\");\n    return 0;\n}\n",
        out: "7\nnull\nfallback",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "hoist_preserves_order",
        src: "fn side(n) {\n    println(\"s{n}\");\n    return n;\n}\nfn main() {\n    println(side(1) + (if (true) { side(2) } else { 0 }));\n    println(side(9), (if (true) { side(8) } else { 0 }));\n    println(false and (if (true) { side(7) } else { 0 }));\n    return 0;\n}\n",
        // Earlier operands still evaluate first, and `and` still short-circuits.
        out: "s1\ns2\n3\ns9\ns8\n9 8\nfalse",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "err_if_branch_has_no_value",
        src: "fn main() {\n    let x = if (true) { let y = 1; } else { 2 };\n    return 0;\n}\n",
        out: "",
        code: 1,
        err: Some("error[semantic]: an if-expression branch must end with an expression (or with return/throw)"),
        aot: false,
    },
    // -------------------------------------------------------------- syntax (2)
    Case {
        name: "string_interp",
        src: "class User {\n    public let name;\n    public let age;\n    fn new(name, age) {\n        self.name = name;\n        self.age = age;\n    }\n}\nfn main() {\n    let name = \"ada\";\n    let n = 36;\n    let u = new User(\"bob\", 7);\n    println(\"hi {name}\");\n    println(\"hi {name}, {n} years\");\n    println(\"user: {u.name} ({u.age})\");\n    println(\"{name}{name}\");\n    println(\"{{ user }} template stays literal\");\n    println(\"{not a hole}\");\n    return 0;\n}\n",
        // A hole is `{ident(.ident)*}` with no spaces; everything else is text,
        // so the stdlib template syntax `"{{ user }}"` is untouched.
        out: "hi ada\nhi ada, 36 years\nuser: bob (7)\nadaada\n{{ user }} template stays literal\n{not a hole}",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "bitwise",
        src: "fn main() {\n    println(12 & 10);\n    println(12 | 10);\n    println(12 ^ 10);\n    println(1 << 4);\n    println(256 >> 3);\n    println(-8 >> 1);\n    println((1 | 2) & 3);\n    println(1 << 2 + 1);\n    println((6 & 3) == 2);\n    println(1 | 2 ^ 3 & 4);\n    return 0;\n}\n",
        // C precedence: `<<` binds tighter than `+`? No — `1 << 2 + 1` is
        // `1 << 3`. `==` binds tighter than `&`, `&` tighter than `^` than `|`.
        out: "8\n14\n6\n16\n32\n-4\n3\n8\ntrue\n3",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "err_bitwise_bool",
        src: "fn main() {\n    println(6 & 3 == 2);\n    return 0;\n}\n",
        out: "",
        code: 1,
        // `==` binds tighter than `&`, so this is `6 & (3 == 2)` — a bool
        // operand, rejected by the one implementation.
        err: Some("error[runtime]: bit_and expects int operands, found bool"),
        aot: false,
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
    // Operator overloading: a class instance carries the dunder as a method
    // (audit S-21 — the native backend had no overload path at all), a map
    // carries it as a value (the stdlib numeric types), and the reflected
    // forms fire when the left operand has no overload of its own.
    Case {
        name: "operator_overload_instance",
        src: "class Vec2 {\n    public let x;\n    public let y;\n    fn new(x, y) {\n        self.x = x;\n        self.y = y;\n    }\n    public fn __add__(self, o) {\n        return Vec2(self.x + o.x, self.y + o.y);\n    }\n    public fn __sub__(self, o) {\n        return Vec2(self.x - o.x, self.y - o.y);\n    }\n    public fn __mul__(self, k) {\n        return Vec2(self.x * k, self.y * k);\n    }\n    public fn __div__(self, k) {\n        return Vec2(self.x / k, self.y / k);\n    }\n    public fn __neg__(self) {\n        return Vec2(-self.x, -self.y);\n    }\n}\nfn main() {\n    let a = Vec2(1, 2);\n    let b = Vec2(3, 4);\n    let c = a + b;\n    println(c.x);\n    println(c.y);\n    let d = b - a;\n    println(d.x);\n    println(d.y);\n    let e = a * 3;\n    println(e.x);\n    println(e.y);\n    let f = b / 2;\n    println(f.x);\n    println(f.y);\n    let g = -a;\n    println(g.x);\n    println(g.y);\n    return 0;\n}\n",
        out: "4\n6\n2\n2\n3\n6\n1\n2\n-1\n-2",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "operator_overload_reflected",
        src: "class Num {\n    public let v;\n    fn new(v) {\n        self.v = v;\n    }\n    public fn __radd__(self, o) {\n        return Num(o + self.v);\n    }\n}\nfn main() {\n    let c = 10 + Num(5);\n    println(c.v);\n    return 0;\n}\n",
        out: "15",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "operator_overload_map",
        src: "fn mk(v) {\n    let m = map();\n    m[\"v\"] = v;\n    m[\"__add__\"] = fn(a, b) {\n        return mk(a[\"v\"] + b[\"v\"]);\n    };\n    return m;\n}\nfn main() {\n    let c = mk(1) + mk(2);\n    println(c[\"v\"]);\n    return 0;\n}\n",
        out: "3",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "operator_overload_missing",
        src: "class P {\n    public let v;\n    fn new(v) {\n        self.v = v;\n    }\n}\nfn main() {\n    let a = P(1);\n    let b = P(2);\n    println(a + b);\n    return 0;\n}\n",
        out: "",
        code: 1,
        // An instance without the dunder is still a type error — the overload
        // path must not swallow it.
        err: Some("error[runtime]: cannot add instance and instance"),
        aot: false,
    },
    // Cancellation latency (audit S-23). A loop body that blocks — the
    // canonical `while true { sleep_ms(..) }` worker — burns wall-clock time
    // the loop's iteration-count poll cannot see: the native backend polled
    // every 2048 iterations, so cancelling a 5ms-per-iteration sleeper took
    // ~10s (a `sleep_ms(100)` loop would take minutes) while tree and VM
    // stopped it within one sleep. `join` waits for the task to actually
    // die, so the elapsed time is the observable.
    Case {
        name: "cancel_latency_is_bounded",
        src: "fn main() {\n    let worker = spawn fn() {\n        while true {\n            sleep_ms(5);\n        }\n        return 0;\n    };\n    sleep_ms(50);\n    let t0 = now_ms();\n    worker.cancel();\n    try {\n        worker.join();\n        println(\"joined\");\n    } catch (e) {\n        println(\"threw: \" + string(e));\n    }\n    if (now_ms() - t0 < 3000) {\n        println(\"fast: true\");\n    } else {\n        println(\"fast: false\");\n    }\n    return 0;\n}\n",
        out: "threw: task cancelled\nfast: true",
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
        // Higher-order collection callbacks (PAR-02) — including a closure
        // that CAPTURES an enclosing local (`k`), which crosses the callback
        // host boundary inside compiled code.
        src: "fn scale(k) {\n    let xs = [1, 2, 3];\n    return xs.map(fn(n) { return n * k; });\n}\nfn main() {\n    let xs = [1, 2, 3, 4];\n    println(xs.map(fn(x) { return x * 2; }));\n    println(xs.filter(fn(x) { return x > 2; }));\n    println(xs.reduce(0, fn(a, b) { return a + b; }));\n    println(scale(3));\n    println(scale(10));\n    return 0;\n}\n",
        out: "[2, 4, 6, 8]\n[3, 4]\n10\n[3, 6, 9]\n[10, 20, 30]",
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
        name: "class_super",
        src: "class Animal {\n    public let name;\n    public let sound;\n    fn new(name, sound) { self.name = name; self.sound = sound; }\n    public fn speak(self) { return self.name + \" says \" + self.sound; }\n}\nclass Dog extends Animal {\n    public let tricks;\n    fn new(name) { super(name, \"woof\"); self.tricks = 0; }\n    public fn speak(self) { return super.speak() + \"!\"; }\n}\nfn main() {\n    let d = new Dog(\"rex\");\n    println(d.speak());\n    println(d.tricks);\n    return 0;\n}\n",
        // `super(args)` runs the parent constructor on the same instance;
        // `super.m(args)` delegates to the parent implementation.
        out: "rex says woof!\n0",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "method_arity_dynamic",
        // A call through an untyped parameter bypasses the checker's static
        // arity check, so the runtime message is observable — and it must be
        // catchable with the SAME text on every engine. Caught (not uncaught)
        // because the AOT binary's uncaught-throw format is a documented
        // diagnostic gap; the caught path pins the message itself.
        src: "class Greeter {\n    fn new() {}\n    public fn greet(self, who) { return \"hi \" + who; }\n}\nfn shout(g) {\n    try {\n        return g.greet(\"a\", \"b\");\n    } catch (e) {\n        return e;\n    }\n}\nfn main() {\n    println(shout(new Greeter()));\n    return 0;\n}\n",
        out: "method greet expects 1 argument(s), got 2",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "tuple_literal",
        // PAR-05: `(a, b, c)` is a tuple literal (desugared to the `tuple(...)`
        // constructor, like every other collection literal). `(e)` stays a
        // parenthesized expression and `(e,)` is a one-element tuple.
        src: "fn main() {\n    let t = (1, \"two\", 3.5);\n    println(t);\n    println(len(t));\n    println(t[1]);\n    let e = ();\n    println(e);\n    let one = (7,);\n    println(one);\n    let g = (5);\n    println(g);\n    let nested = ((1, 2), (3,));\n    println(nested);\n    return 0;\n}\n",
        out: "(1, two, 3.5)\n3\ntwo\n()\n(7)\n5\n((1, 2), (3))",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "as_cast",
        // PAR-06: `expr as type` desugars to the int/float/string/bool builtin
        // (one implementation, every engine). Contextual keyword — `as` binds
        // tighter than `*` (Rust-style), so `1 + 2 as string` casts only the 2.
        src: "fn main() {\n    let x = 42;\n    println(x as string + \"!\");\n    println(\"7\" as int + 1);\n    println(3 as float);\n    println(1 as bool);\n    println(\"3.5\" as float * 2.0);\n    println((1 + 2) as string);\n    println(5 as int);\n    return 0;\n}\n",
        out: "42!\n8\n3.0\ntrue\n7.0\n3\n5",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "default_params",
        // Constant defaults are trailing-only (parser-enforced) and fill at
        // the callee: functions, methods, and constructors alike.
        src: "fn greet(name, greeting = \"Hello\", punct = \"!\") {\n    return greeting + \", \" + name + punct;\n}\nclass Counter {\n    public let n;\n    fn new(start = 0) { self.n = start; }\n    public fn add(self, by = 1) { self.n = self.n + by; }\n}\nfn main() {\n    println(greet(\"ada\"));\n    println(greet(\"bob\", \"Yo\"));\n    println(greet(\"cid\", \"Hey\", \"?\"));\n    let c = new Counter();\n    c.add();\n    c.add(5);\n    println(c.n);\n    println(greet(\"x\", \"Hi\", \"!\"));\n    return 0;\n}\n",
        out: "Hello, ada!\nYo, bob!\nHey, cid?\n6\nHi, x!",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "named_args",
        // Named arguments on direct function calls and constructors (v1:
        // method calls reject them with a clear diagnostic). `lower`
        // reorders them into positional form against the callee's declared
        // parameters; unbound trailing parameters fall back to defaults.
        src: "fn connect(host, port = 80, secure = false) {\n    return host + \":\" + string(port) + \" ssl=\" + string(secure);\n}\nclass Point {\n    public let x;\n    public let y;\n    fn new(x = 0, y = 0) { self.x = x; self.y = y; }\n}\nfn main() {\n    println(connect(\"example.com\"));\n    println(connect(\"a.io\", port: 8080));\n    println(connect(host: \"b.io\", secure: true));\n    let p = new Point(y: 5);\n    println(p.x);\n    println(p.y);\n    let q = new Point(x: 1, y: 2);\n    println(q.x);\n    println(q.y);\n    return 0;\n}\n",
        out: "example.com:80 ssl=false\na.io:8080 ssl=false\nb.io:80 ssl=true\n0\n5\n1\n2",
        code: 0,
        err: None,
        aot: true,
    },
    Case {
        name: "named_args_errors",
        // Compile-time rejections: unknown name, duplicate, positional after
        // named, and (v1) method calls.
        src: "fn f(a, b) { return a; }\nfn main() {\n    f(b: 1, nope: 2);\n    return 0;\n}\n",
        out: "",
        code: 1,
        err: Some("error[semantic]: unknown parameter 'nope' in the call to f"),
        aot: false,
    },
    Case {
        name: "named_args_positional_after_named",
        src: "fn f(a, b) { return a; }\nfn main() {\n    f(b: 1, 2);\n    return 0;\n}\n",
        out: "",
        code: 1,
        err: Some("error[semantic]: a positional argument cannot follow a named one"),
        aot: false,
    },
    Case {
        name: "named_args_method_rejected",
        src: "class C {\n    fn m(self, a) { return a; }\n}\nfn main() {\n    let c = new C();\n    c.m(a: 1);\n    return 0;\n}\n",
        out: "",
        code: 1,
        err: Some("error[semantic]: named arguments on method calls are not supported yet (call m as a function, or pass arguments positionally)"),
        aot: false,
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
    // ------------------------------------------------- Phase 0 HLC golden values
    // Inline RESULT-only sources for the three Heavy-Load Checker compute
    // workloads vendored byte-verbatim in benchmarks/compare/{prime,json,map}.matrix.
    // Each is that verbatim source with EXACTLY THREE timing lines removed:
    //   let t0 = now_ms();
    //   let us = (now_ms() - t0) * 1000;
    //   println("TIME_US " + string(us));
    // Constants, loop bodies and the RESULT expression are byte-identical;
    // `python tools/phase0_fixtures.py --verify` proves the delta mechanically
    // against manifest.json's SHA-256 digests, and asserts the absolute pins on
    // tree / vm / exec for the verbatim sources.
    //
    // These are CONFORMANCE fixtures ONLY - they are NEVER timed. The verbatim
    // sources are the only ones the Phase 0 harness runs, because they emit a
    // non-deterministic TIME_US line that cmd_test's byte-exact stdout contract
    // cannot accept. `aot: false` because the local binary has no native
    // backend, so an AOT claim could not be verified here.
    // Pinned checks: 5133 121013308 / 249868800 9163 / 44432313 100000.
    Case {
        name: "phase0_hlc_prime",
        src: r#"fn is_prime(n: int) -> bool {
    if n < 2 { return false; }
    let d = 2;
    while d * d <= n { if n % d == 0 { return false; } d += 1; }
    return true;
}
fn main() {
    let i = 2;
    let count = 0;
    let checksum = 0;
    while i < 50000 {
        if is_prime(i) { count += 1; checksum += i; }
        i += 1;
    }
    println("RESULT " + string(count) + " " + string(checksum));
    return 0;
}
"#,
        out: "RESULT 5133 121013308",
        code: 0,
        err: None,
        aot: false,
    },
    Case {
        name: "phase0_hlc_json",
        src: r#"fn build_rows(n: int) {
    let rows = list();
    let i = 0;
    while i < n {
        rows.push(map("id", i, "sku", "SKU-" + string(i), "qty", (i % 7) + 1, "price", (i * 37) % 1000));
        i += 1;
    }
    return rows;
}
fn main() {
    let rows = build_rows(200);
    let total = 0;
    let last_len = 0;
    let r = 0;
    while r < 600 {
        let s = json_write(rows);
        let back = json_parse(s);
        last_len = s.len();
        let i = 0;
        while i < back.len() {
            let row = back[i];
            total = total + row["price"] * row["qty"] + row["id"];
            i += 1;
        }
        r += 1;
    }
    println("RESULT " + string(total) + " " + string(last_len));
    return 0;
}
"#,
        out: "RESULT 249868800 9163",
        code: 0,
        err: None,
        aot: false,
    },
    Case {
        name: "phase0_hlc_map",
        src: r#"fn build_table(n: int) {
    let t = map();
    let i = 0;
    while i < n {
        t["k" + string(i)] = (i * 37 + 11) % 997;
        i += 1;
    }
    return t;
}
fn main() {
    let t = build_table(64);
    let acc = 0;
    let r = 0;
    while r < 100000 {
        let key = "k" + string(r % 64);
        acc = acc + t[key];
        r += 1;
    }
    println("RESULT " + string(acc) + " 100000");
    return 0;
}
"#,
        out: "RESULT 44432313 100000",
        code: 0,
        err: None,
        aot: false,
    },
    // ------------------------------------------------------------------
    // Phase 1 conformance controls.
    //
    // `src/vm/mod.rs` peeks at two opcode sequences at RUNTIME and fuses them
    // (GetLocal;GetLocal;Mul;GetLocal;LessEq;JumpIfFalse and
    //  GetLocal;GetLocal;Mod;Constant;Equal;JumpIfFalse). Those fast paths are
    // only legal because they FALL THROUGH to generic dispatch whenever a
    // checked operation fails, an operand is not an Int, or a jump target is
    // out of range. `src/stdlib/mod.rs` also gained a shared small-int string
    // cache and allocation-reduced JSON writer/parser. These cases pin the
    // tree oracle's answers at exactly the boundaries a naive fast path would
    // get wrong, so a future "optimisation" that starts swallowing overflow,
    // modulo-by-zero, float comparisons or cross-type equality fails here on
    // BOTH engines rather than silently changing script behaviour.
    // ------------------------------------------------------------------
    Case {
        name: "phase1_fused_mul_overflow_falls_through",
        src: r#"fn f(d: int, n: int) {
    while d * d <= n { println("IN"); return; }
    println("OK");
}
fn main() { f(3037000500, 10); return 0; }
"#,
        out: "",
        code: 1,
        // checked_mul returns None, so generic Mul must still raise the
        // pre-existing vm_eval_binary overflow diagnostic — not a fused
        // wrapped product, and not a silent false comparison.
        err: Some("error[runtime]: integer overflow: 3037000500 * 3037000500"),
        aot: false,
    },
    Case {
        name: "phase1_fused_mod_zero_falls_through",
        src: r#"fn f(n: int, d: int) {
    if n % d == 0 { println("ZERO"); } else { println("NO"); }
}
fn main() { f(10, 0); return 0; }
"#,
        out: "",
        code: 1,
        // checked_rem returns None on a zero divisor; the generic Mod handler
        // owns the error text and the span.
        err: Some("error[runtime]: modulo by zero"),
        aot: false,
    },
    Case {
        name: "phase1_fused_less_eq_float_falls_through",
        src: r#"fn main() {
    let d = 1.5;
    let n = 3.0;
    let hits = 0;
    while d * d <= n { hits = hits + 1; d = d + 1.0; }
    println(string(hits));
    return 0;
}
"#,
        // Float operands must bypass the Int-only fused path entirely.
        out: "1",
        code: 0,
        err: None,
        aot: false,
    },
    Case {
        name: "phase1_fused_mod_paths_and_fallthroughs",
        src: r#"fn mod_true(n: int, d: int) {
    if n % d == 0 { println("DIV"); } else { println("NOT"); }
}
fn mod_str_const(n: int, d: int) {
    if n % d == "0" { println("STRMATCH"); } else { println("NOSTR"); }
}
fn mul_float_rhs(n: float) {
    let d = 2;
    let hits = 0;
    while d * d <= n { hits += 1; d += 1; }
    println(string(hits));
    return 0;
}
fn minmod() {
    let a = -9223372036854775807 - 1;
    let b = -1;
    println(string(a % b));
    return 0;
}
fn main() {
    mod_true(10, 5);
    mod_true(10, 3);
    mod_str_const(10, 5);
    mul_float_rhs(10.0);
    minmod();
    return 0;
}
"#,
        // Covers the fused true/false branches, a non-Int constant (so
        // values_equal's cross-type rules still apply), a Float RHS in the
        // Mul pattern, and i64::MIN % -1 where checked_rem returns None and
        // the generic wrapping_rem result (0) must survive.
        out: "DIV\nNOT\nNOSTR\n2\n0",
        code: 0,
        err: None,
        aot: false,
    },
    Case {
        name: "phase1_json_writer_parser_boundaries",
        src: r#"fn main() {
    let m = map(null, 1, true, 2, 3, "x", "q\"u", 4);
    println(json_write(m));
    println(string(0));
    println(string(1023));
    println(string(1024));
    println(string(-1));
    println(string(9223372036854775807));
    println(json_parse("9223372036854775807"));
    println(json_parse("9223372036854775808"));
    println(json_parse("-9223372036854775808"));
    println(json_parse("-0"));
    println(json_parse("0012"));
    println(json_parse("1e3"));
    return 0;
}
"#,
        // write_json_key must emit identical key spellings for null/bool/int
        // and identical escaping for string keys. int_to_string_value must be
        // byte-identical across the cached 0..=1023 range and outside it.
        // integer_token must hand i64 overflow, i64::MIN and exponent shapes
        // back to the original text/f64 fallback unchanged.
        out: concat!(
            "{\"null\":1,\"true\":2,\"3\":\"x\",\"q\\\"u\":4}\n",
            "0\n1023\n1024\n-1\n9223372036854775807\n",
            "9223372036854775807\n9223372036854776000\n",
            "-9223372036854775808\n0\n12\n1000.0",
        ),
        code: 0,
        err: None,
        aot: false,
    },
    Case {
        name: "phase1_fused_prefix_string_paths",
        // Both shapes of fused superinstruction C:
        //   Constant p; GetGlobal g; GetLocal s; Constant k; Mod; Call 1; Add
        //   Constant p; GetGlobal g; GetLocal s; Call 1; Add
        src: r#"fn keys(r: int) {
    let a = "k" + string(r % 64);
    let b = "v" + string(r);
    println(a);
    println(b);
}
fn big() {
    let n = 1234567890123;
    println("n" + string(n));
}
fn floats(x: float) {
    println("f" + string(x));
}
fn minmod() {
    let a = -9223372036854775807 - 1;
    println("m" + string(a % -1));
}
fn main() {
    keys(0);
    keys(63);
    keys(64);
    keys(-1);
    big();
    floats(1.5);
    minmod();
    return 0;
}
"#,
        // Covers cached (0..=1023) and uncached small-int spellings, negative
        // remainders, an int outside the cache, a Float local (must fall
        // through to the generic `string` native), and i64::MIN % -1 where
        // checked_rem is None so the generic wrapping_rem result survives.
        out: concat!(
            "k0\nv0\n",
            "k63\nv63\n",
            "k0\nv64\n",
            "k-1\nv-1\n",
            "n1234567890123\n",
            "f1.5\n",
            "m0",
        ),
        code: 0,
        err: None,
        aot: false,
    },
    Case {
        name: "phase1_fused_prefix_string_rebind",
        // The global is resolved on EVERY hit: a user-defined `string` must
        // win, not a stale inlined builtin.
        src: r#"fn string(x) { return "OVERRIDE"; }
fn main() {
    let r = 7;
    println("k" + string(r % 64));
    println("v" + string(r));
    return 0;
}
"#,
        out: "kOVERRIDE\nvOVERRIDE",
        code: 0,
        err: None,
        aot: false,
    },
    Case {
        name: "phase1_fused_prefix_string_mod_zero",
        // checked_rem returns None for a zero divisor, so the fused path must
        // fall through and raise the identical generic runtime error.
        src: r#"fn main() {
    let r = 10;
    println("k" + string(r % 0));
    return 0;
}
"#,
        out: "",
        code: 1,
        err: Some("error[runtime]: modulo by zero"),
        aot: false,
    },
    Case {
        name: "phase1_fused_prefix_string_loop_key",
        // The exact `benchmarks/compare/map.matrix` hot expression, in a
        // 1000-iteration loop, so both the fused key construction and the
        // subsequent map lookup are exercised on tree and VM.
        src: r#"fn main() {
    let t = map();
    let i = 0;
    while i < 64 {
        t["k" + string(i)] = i * 3;
        i += 1;
    }
    let acc = 0;
    let r = 0;
    while r < 1000 {
        let key = "k" + string(r % 64);
        acc = acc + t[key];
        r += 1;
    }
    println("RESULT " + string(acc));
    return 0;
}
"#,
        out: "RESULT 93060",
        code: 0,
        err: None,
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

/// An import spec that escapes the search roots is rejected **identically** by
/// every backend.
///
/// This is a contract row, not a style check: the tree walker validated the
/// spec (`src/modules::check_import_path`) and the VM/AOT resolvers did not, so
/// `import "../escape"` loaded a file outside the module tree on two engines
/// and failed on the third — "compiles but computes differently", the failure
/// mode `docs/ENGINE_CONTRACT.md` ranks worst. Resolution now goes through
/// `src/modules/resolve.rs` for all of them, and this row keeps it that way.
#[test]
fn an_escaping_import_spec_is_rejected_identically_on_tree_and_vm() {
    // Pid-suffixed like every other suite: a fixed name lets two concurrent
    // test processes delete each other's fixture mid-run.
    let dir = std::env::temp_dir().join(format!(
        "spid3r_matrix_import_escape_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    // The escape target exists, so a resolver that skipped validation would
    // load it instead of erroring.
    std::fs::write(
        dir.join("escape.matrix"),
        "public fn f() {\n    return 1;\n}\n",
    )
    .expect("write module");
    let src = "import \"../escape\";\nfn main() {\n    println(f());\n    return 0;\n}\n";
    let path = dir.join("main.matrix");
    std::fs::write(&path, src).expect("write main");

    let mut seen: Option<(i32, String)> = None;
    for engine in ["tree", "vm"] {
        let o = Command::new(bin())
            .args(["run", "--engine", engine, path.to_str().unwrap()])
            .current_dir(&dir)
            .output()
            .expect("spawn");
        let err = String::from_utf8_lossy(&o.stderr).to_string();
        let first = err.lines().next().unwrap_or("").trim().to_string();
        let code = o.status.code().unwrap_or(-1);
        assert_eq!(code, 1, "[{engine}] expected a semantic error: {err}");
        assert!(
            first.contains("invalid module path"),
            "[{engine}] expected the spec diagnostic: {first}"
        );
        match &seen {
            None => seen = Some((code, first)),
            Some((c0, m0)) => {
                assert_eq!(&code, c0, "[{engine}] exit code differs from tree");
                assert_eq!(&first, m0, "[{engine}] stderr differs from tree");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A program with none of the surface-only nodes is passed through **by
/// reference**: `lower` returns `Cow::Borrowed`, so nothing is copied and no
/// existing behaviour can change. This is the property that makes the pass safe
/// to insert in front of every backend.
#[test]
fn lowering_is_a_no_op_for_programs_without_the_new_syntax() {
    let src = "fn main() {\n    let xs = [1, 2, 3];\n    for x in xs {\n        println(x);\n    }\n    try {\n        throw \"e\";\n    } catch (e) {\n        println(e);\n    }\n    return 0;\n}\n";
    let program =
        spid3r_lang::parser::parse_file(src, std::sync::Arc::from("lower.matrix")).expect("parse");
    let lowered = spid3r_lang::lower::lower(&program);
    assert!(
        matches!(lowered, std::borrow::Cow::Borrowed(_)),
        "a program without if-expressions, ?. or ?? must not be copied"
    );
    assert_eq!(
        format!("{:?}", program),
        format!("{:?}", lowered.as_ref()),
        "the lowering pass changed a program it had nothing to rewrite in"
    );
}

/// The surface nodes must be gone before a backend sees the tree, and the
/// pass must be idempotent (the VM compiles module programs through it too).
/// (`OptField` is a first-class op since F-4 — `?.` survives lowering by
/// design now, so only `Coalesce` is checked here; the if-expression cannot
/// be checked by name because lowering *produces* `Stmt::If`, which prints
/// the same `If {` — its removal is covered by the matrix cases that
/// execute it.)
#[test]
fn lowering_removes_the_surface_nodes_and_is_idempotent() {
    let src = "fn main() {\n    let a = if (true) { 1 } else { 2 };\n    let b = null ?? a;\n    let m = map();\n    println(m?.k);\n    println(b);\n    return 0;\n}\n";
    let program =
        spid3r_lang::parser::parse_file(src, std::sync::Arc::from("lower2.matrix")).expect("parse");
    let lowered = spid3r_lang::lower::lower(&program);
    let text = format!("{:?}", lowered.as_ref());
    // `Coalesce` has a unique name; the if-expression cannot be checked by
    // name because lowering *produces* `Stmt::If`, which prints the same
    // `If {` — its removal is covered by the matrix cases that execute it.
    assert!(
        !text.contains("Coalesce"),
        "`Coalesce` survived lowering://n{}",
        text
    );
    assert!(
        text.contains("__spid3r_t"),
        "no temporary was introduced, so nothing was lowered://n{}",
        text
    );
    let twice = spid3r_lang::lower::lower(lowered.as_ref());
    assert_eq!(
        format!("{:?}", lowered.as_ref()),
        format!("{:?}", twice.as_ref()),
        "lower() is not idempotent"
    );
}

/// Full-output variant of `run_engine`: returns (exit, stdout, full stderr).
/// Trace comparisons need every stderr line, not just the first.
fn run_engine_full(engine: &str, name: &str, src: &str) -> (i32, String, String) {
    let dir = std::env::temp_dir().join("spid3r_matrix_treem");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{}.matrix", name));
    std::fs::write(&path, src).expect("write case");
    let o = Command::new(bin())
        .args(["run", "--engine", engine, path.to_str().unwrap()])
        .output()
        .expect("spawn spid3r");
    let _ = std::fs::remove_file(&path);
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

/// Deep self-recursion must produce an identical stack-overflow diagnostic on
/// both engines — same message, same span, same exit code, and (the part that
/// used to diverge) the same stack trace: the tree interpreter now collapses
/// consecutive frames naming the same function at the same file:line, exactly
/// like the VM's `capture_trace`.
#[test]
fn stack_overflow_traces_match_on_tree_and_vm() {
    let src = "fn r(n) {\n    if n <= 0 { return 0; }\n    return r(n - 1) + 1;\n}\nfn main() {\n    println(r(5000));\n    return 0;\n}\n";
    let tree = run_engine_full("tree", "stack_overflow_trace", src);
    let vm = run_engine_full("vm", "stack_overflow_trace", src);
    assert_eq!(tree.0, vm.0, "exit codes differ");
    assert_eq!(tree.1, vm.1, "stdout differs");
    assert_eq!(tree.2, vm.2, "stderr (incl. stack trace) differs");
    assert!(tree.2.contains("stack overflow: call depth limit reached"));
    // Both engines must have collapsed the recursion to one frame + main.
    assert_eq!(
        tree.2.matches("at r (").count(),
        1,
        "tree trace not collapsed"
    );
    assert_eq!(vm.2.matches("at r (").count(), 1, "vm trace not collapsed");
}

/// Assignment evaluation order: the tree interpreter (the oracle) evaluates
/// the RHS before the target's object/index expressions, and the VM must agree
/// when either side has observable side effects. Covers index and field
/// targets, plain and compound assignment, and call targets on both sides.
#[test]
fn assignment_evaluation_order_matches_on_tree_and_vm() {
    let src = concat!(
        "class C { let f; fn init(self) { self.f = 0; } }\n",
        "let g = null;\n",
        "fn get_c() { println(\"target\"); return g; }\n",
        "fn log(s) { println(s); return s; }\n",
        "fn key() { println(\"key\"); return \"a\"; }\n",
        "fn main() {\n",
        "  g = C();\n",
        // Field target, plain and compound: RHS first, then the receiver call.
        "  get_c().f = log(\"rhs\");\n",
        "  get_c().f += log(\"rhs2\");\n",
        // Index target (string key), plain and compound.
        "  let m = {\"a\": 1};\n",
        "  m[key()] = log(\"rv\");\n",
        "  m[key()] += log(\"rv2\");\n",
        "  println(m[\"a\"]);\n",
        "  println(g.f);\n",
        "  return 0;\n",
        "}\n"
    );
    let expected = "rhs\ntarget\nrhs2\ntarget\nrv\nkey\nrv2\nkey\nrvrv2\nrhsrhs2";
    for engine in ["tree", "vm"] {
        let got = run_engine(engine, "assign_order", src);
        assert_eq!(
            (got.0, got.1.trim_end()),
            (0, expected),
            "[{} / assign_order] evaluation order diverged",
            engine
        );
        assert!(
            got.2.is_empty(),
            "[{} / assign_order] stderr {:?}",
            engine,
            got.2
        );
    }
}

/// Phase 1: the VM's fused superinstructions are runtime pattern peeks, so
/// they MUST be disabled under `--prof` and `--coverage`. Otherwise the
/// profiler would under-count `mul` / `mod` / `less_eq` / `equal` /
/// `jump_if_false` and coverage would silently miss the lines those opcodes
/// belong to. This test pins both the semantic output AND the observation of
/// every original opcode, and also checks `--no-opt` (where the same runtime
/// patterns are reachable) still agrees with the default VM.
#[test]
fn phase1_fused_paths_preserve_instrumentation() {
    let src = concat!(
        "fn is_prime(n: int) {\n",
        "    let d = 2;\n",
        "    while d * d <= n {\n",
        "        if n % d == 0 { return false; }\n",
        "        d += 1;\n",
        "    }\n",
        "    return true;\n",
        "}\n",
        "fn main() {\n",
        "    let i = 2;\n",
        "    let c = 0;\n",
        "    while i < 200 {\n",
        "        if is_prime(i) { c += 1; }\n",
        "        i += 1;\n",
        "    }\n",
        "    println(\"COUNT \" + string(c));\n",
        "    return 0;\n",
        "}\n",
    );
    let dir = std::env::temp_dir().join("spid3r_matrix_phase1_flags");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("fused_prof.matrix");
    std::fs::write(&path, src).expect("write case");

    let run = |engine: &str, extra: &[&str]| {
        let o = Command::new(bin())
            .arg("run")
            .arg("--engine")
            .arg(engine)
            .args(extra)
            .arg(path.to_str().unwrap())
            .output()
            .expect("spawn spid3r");
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).to_string(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        )
    };

    let expected = "COUNT 46";
    for (label, engine, extra) in [
        ("tree", "tree", &[][..]),
        ("vm", "vm", &[][..]),
        ("vm --no-opt", "vm", &["--no-opt"][..]),
        ("vm --prof", "vm", &["--prof"][..]),
        ("vm --coverage", "vm", &["--coverage"][..]),
        ("vm --prof --no-opt", "vm", &["--prof", "--no-opt"][..]),
    ] {
        let (code, out, err) = run(engine, extra);
        assert_eq!(code, 0, "[{}] exit {} stderr {}", label, code, err);
        assert_eq!(out.trim_end(), expected, "[{}] stdout", label);
    }

    // Under --prof the original opcodes must still be observed: a fused path
    // that ran anyway would erase these lines from the profile.
    let (_, _, prof) = run("vm", &["--prof"]);
    assert!(prof.contains("==== vm profile ===="), "no profile: {}", prof);
    assert!(prof.contains("instructions:"), "no instruction count: {}", prof);
    for opcode in ["mul", "mod", "less_eq", "equal", "jump_if_false"] {
        assert!(
            prof.lines().any(|l| l.trim_end().ends_with(opcode)),
            "--prof lost the {:?} opcode: {}",
            opcode,
            prof
        );
    }

    // Under --coverage the fused patterns must not skip line hits.
    let (_, _, cov) = run("vm", &["--coverage"]);
    assert!(cov.contains("(total)"), "no coverage report: {}", cov);
    assert!(cov.contains("100.0%"), "fused path lost coverage: {}", cov);

    let _ = std::fs::remove_file(&path);
}

/// Phase 1 fused superinstruction C skips seven opcodes per hit
/// (`constant`, `get_global`, `get_local`, `constant`, `mod`, `call`, `add`).
/// It MUST therefore be disabled under `--prof` and `--coverage`, or the
/// profiler would under-count and coverage would silently lose line hits.
///
/// Phase 5: EVERY runtime fusion obeys that same gate. The
/// `GetLocal a; GetLocal b; Less; JumpIfFalse` and
/// `GetLocal a; GetLocal b; Add; SetLocal; Pop` fusions are gated on
/// `--prof` / `--coverage` too (they used to run under instrumentation,
/// which made the profiler under-count the original opcode sequence by
/// ~23%), and `--no-opt` additionally turns the Phase 1 peeks off
/// (`Vm::set_opt_enabled(false)`). Instrumentation therefore observes EVERY
/// original opcode in both modes, which is what the two totals below pin.
#[test]
fn phase1_fused_prefix_string_preserves_instrumentation() {
    let src = concat!(
        "fn main() {\n",
        "    let t = map();\n",
        "    let i = 0;\n",
        "    while i < 64 {\n",
        "        t[\"k\" + string(i)] = i * 3;\n",
        "        i += 1;\n",
        "    }\n",
        "    let acc = 0;\n",
        "    let r = 0;\n",
        "    while r < 1000 {\n",
        "        let key = \"k\" + string(r % 64);\n",
        "        acc = acc + t[key];\n",
        "        r += 1;\n",
        "    }\n",
        "    println(\"RESULT \" + string(acc));\n",
        "    return 0;\n",
        "}\n",
    );
    let dir = std::env::temp_dir().join("spid3r_matrix_phase1_fusedc");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("fusedc_prof.matrix");
    std::fs::write(&path, src).expect("write case");

    let run = |engine: &str, extra: &[&str]| {
        let o = Command::new(bin())
            .arg("run")
            .arg("--engine")
            .arg(engine)
            .args(extra)
            .arg(path.to_str().unwrap())
            .output()
            .expect("spawn spid3r");
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).to_string(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        )
    };

    let expected = "RESULT 93060";
    for (label, engine, extra) in [
        ("tree", "tree", &[][..]),
        ("vm", "vm", &[][..]),
        ("vm --no-opt", "vm", &["--no-opt"][..]),
        ("vm --prof", "vm", &["--prof"][..]),
        ("vm --coverage", "vm", &["--coverage"][..]),
        ("vm --prof --no-opt", "vm", &["--prof", "--no-opt"][..]),
    ] {
        let (code, out, err) = run(engine, extra);
        assert_eq!(code, 0, "[{}] exit {} stderr {}", label, code, err);
        assert_eq!(out.trim_end(), expected, "[{}] stdout", label);
    }

    let instructions = |prof: &str| -> i64 {
        prof.lines()
            .find_map(|l| l.trim().strip_prefix("instructions: "))
            .and_then(|n| n.trim().parse::<i64>().ok())
            .unwrap_or(-1)
    };
    let (_, _, prof) = run("vm", &["--prof"]);
    assert!(prof.contains("==== vm profile ===="), "no profile: {}", prof);
    // Phase 1: with every original opcode observed the unoptimized run is
    // 22306 instructions; a RUNTIME fused path running under --prof would
    // collapse this by thousands of instructions, which this assertion is
    // here to catch.
    //
    // Phase 2: the optimizer now ROTATES the two `while` back-edges on the
    // default path (see src/vm/opt.rs::rotate_loops). Each rotation removes
    // exactly one unconditional `Jump` dispatch per loop iteration, so the
    // optimized --prof total is 22306 - (64 + 1000) = 21242. This is a real
    // COMPILE-TIME reduction in executed work, not a hidden opcode: the
    // profile below still shows the condition opcodes (`less` 1066 times)
    // and the two top-of-loop `jump_if_false` checks, plus 1064 bottom
    // `jump_if_true` checks. --prof --no-opt must remain 22306 because
    // rotation lives only in compile_program().
    assert_eq!(instructions(&prof), 21242, "--prof lost opcodes: {}", prof);
    let (_, _, prof_no_opt) = run("vm", &["--prof", "--no-opt"]);
    assert_eq!(
        instructions(&prof_no_opt),
        22306,
        "--prof --no-opt lost opcodes: {}",
        prof_no_opt
    );
    assert_eq!(
        instructions(&prof_no_opt) - instructions(&prof),
        1064,
        "rotation must remove exactly one Jump per loop iteration"
    );
    for opcode in [
        "constant",
        "get_global",
        "get_local",
        "call",
        "add",
        "index_get",
    ] {
        assert!(
            prof.lines().any(|l| l.trim_end().ends_with(opcode)),
            "--prof lost the {:?} opcode: {}",
            opcode,
            prof
        );
    }

    let (_, _, cov) = run("vm", &["--coverage"]);
    assert!(cov.contains("(total)"), "no coverage report: {}", cov);
    assert!(cov.contains("100.0%"), "fused path lost coverage: {}", cov);

    let _ = std::fs::remove_file(&path);
}
