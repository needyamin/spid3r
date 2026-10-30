//! Stage 3: enums (sum types) and `match`.
//!
//! An enum declaration lowers to one constructor function per variant — a
//! `map` with a `__tag` key — and a `match` lowers to a linear chain of guarded
//! `if`s. That is why construction is an ordinary call (so it works across
//! `import`) and why no backend needed a new opcode.
//!
//! These tests pin the surface semantics: patterns, guards, bindings,
//! exhaustiveness, and the unmatched-value error.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run(src: &str, tag: &str, engine: &str) -> (i32, String, String) {
    let dir = std::env::temp_dir().join("spid3r_stage3");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{}.matrix", tag));
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

fn both(tag: &str, src: &str, expected: &str) {
    for engine in ["tree", "vm"] {
        let (code, out, err) = run(src, tag, engine);
        assert_eq!(code, 0, "[{} / {}] failed: {}", tag, engine, err);
        assert_eq!(out.trim_end(), expected, "[{} / {}] stdout", tag, engine);
    }
}

const SHAPES: &str = "enum Shape {\n    Empty,\n    Circle(radius),\n    Rect(w, h),\n}\n";

#[test]
fn a_variant_constructs_a_tagged_value_and_match_reads_it() {
    let src = format!(
        "{}fn area(s) {{\n    return match (s) {{\n        Circle(r) => 3 * r * r,\n        Rect(w, h) => w * h,\n        Empty() => 0,\n    }};\n}}\nfn main() {{\n    println(area(Circle(2)));\n    println(area(Rect(3, 4)));\n    println(area(Empty()));\n    return 0;\n}}\n",
        SHAPES
    );
    both("match_basic", &src, "12\n12\n0");
}

/// Regression: a bare PascalCase identifier in a pattern is a *payload-less
/// enum variant*, not a catch-all bind. Before the fix `Red`/`Green`/`Blue`
/// were parsed as bindings, which match *every* value, so the first arm's body
/// (`"red"`) was returned for all three inputs instead of the matched variant.
#[test]
fn bare_variant_pattern_is_not_a_catch_all_bind() {
    both(
        "match_bare_variant",
        "enum Color { Red, Green, Blue }\nfn pick(c) {\n    return match (c) {\n        Red => \"red\",\n        Green => \"green\",\n        Blue => \"blue\",\n    };\n}\nfn main() {\n    println(pick(Red()));\n    println(pick(Green()));\n    println(pick(Blue()));\n    return 0;\n}\n",
        "red\ngreen\nblue",
    );
}

/// Arms are tried top to bottom, and a failed *guard* falls through to the next
/// arm rather than failing the match.
#[test]
fn a_failed_guard_falls_through_to_the_next_arm() {
    let src = format!(
        "{}fn describe(s) {{\n    return match (s) {{\n        Circle(r) if (r > 10) => \"big\",\n        Circle(r) => \"small\",\n        Rect(w, h) if (w == h) => \"square\",\n        Rect(w, h) => \"rect\",\n        Empty() => \"empty\",\n    }};\n}}\nfn main() {{\n    println(describe(Circle(20)));\n    println(describe(Circle(3)));\n    println(describe(Rect(4, 4)));\n    println(describe(Rect(4, 5)));\n    println(describe(Empty()));\n    return 0;\n}}\n",
        SHAPES
    );
    both("match_guards", &src, "big\nsmall\nsquare\nrect\nempty");
}

#[test]
fn literals_bindings_and_wildcards_all_match() {
    both(
        "match_literals",
        "fn classify(n) {\n    return match (n) {\n        0 => \"zero\",\n        1 => \"one\",\n        x => \"many ({x})\",\n    };\n}\nfn main() {\n    println(classify(0));\n    println(classify(1));\n    println(classify(7));\n    let s = match (\"b\") {\n        \"a\" => 1,\n        \"b\" => 2,\n        _ => 3,\n    };\n    println(s);\n    return 0;\n}\n",
        "zero\none\nmany (7)\n2",
    );
}

/// A variant pattern's sub-pattern can itself be a variant pattern.
#[test]
fn patterns_nest() {
    both(
        "match_nested",
        "enum Point { At(x, y) }\nenum Shape { Dot(p), Line(p) }\nfn main() {\n    let s = Dot(At(3, 4));\n    let r = match (s) {\n        Dot(At(x, y)) => x * 10 + y,\n        Line(p) => 0,\n    };\n    println(r);\n    return 0;\n}\n",
        "34",
    );
}

/// A binding introduced by a pattern is scoped to that arm only.
#[test]
fn pattern_bindings_do_not_leak_between_arms() {
    both(
        "match_scope",
        "fn f(n) {\n    let out = \"\";\n    match (n) {\n        1 => { let x = 10; out = \"one:{x}\"; },\n        _ => { let x = 20; out = \"other:{x}\"; },\n    }\n    return out;\n}\nfn main() {\n    println(f(1));\n    println(f(2));\n    return 0;\n}\n",
        "one:10\nother:20",
    );
}

/// `match` is a statement too, and needs no trailing `;`.
#[test]
fn match_works_in_statement_position() {
    both(
        "match_stmt",
        "fn main() {\n    let v = 2;\n    match (v) {\n        1 => println(\"one\"),\n        _ => println(\"other\"),\n    }\n    return 0;\n}\n",
        "other",
    );
}

/// Every variant must be covered when the arms name a single enum; the checker
/// says which one is missing.
#[test]
fn a_non_exhaustive_match_is_a_semantic_error() {
    let (code, _, err) = run(
        "enum Color { Red, Green, Blue }\nfn name(c) {\n    return match (c) {\n        Red() => \"red\",\n        Green() => \"green\",\n    };\n}\nfn main() {\n    println(name(Red()));\n    return 0;\n}\n",
        "match_exhaustive",
        "tree",
    );
    assert_ne!(code, 0, "a missing variant must fail the check");
    assert!(
        err.contains("not exhaustive") && err.contains("Blue"),
        "the error should name the missing variant: {}",
        err
    );
}

/// When the checker cannot tell which enum is being matched (literals only), an
/// unmatched value is a clean runtime error rather than a silent null.
#[test]
fn an_unmatched_value_is_a_runtime_error() {
    for engine in ["tree", "vm"] {
        let (code, _, err) = run(
            "fn main() {\n    let r = match (9) {\n        1 => \"one\",\n        2 => \"two\",\n    };\n    println(r);\n    return 0;\n}\n",
            "match_unmatched",
            engine,
        );
        assert_eq!(code, 1, "[{}] should exit 1", engine);
        assert!(
            err.contains("no match arm matched"),
            "[{}] unexpected error: {}",
            engine,
            err
        );
    }
}

/// The scrutinee is evaluated exactly once, even with side effects.
#[test]
fn the_scrutinee_is_evaluated_once() {
    both(
        "match_once",
        "fn make() {\n    println(\"make\");\n    return 1;\n}\nfn main() {\n    let r = match (make()) {\n        1 => \"one\",\n        _ => \"other\",\n    };\n    println(r);\n    return 0;\n}\n",
        "make\none",
    );
}

/// An enum's variants are exported, so a constructor is usable from an importing
/// file — construction is an ordinary function call.
#[test]
fn enum_constructors_work_across_imports() {
    let dir = std::env::temp_dir().join("spid3r_stage3_import");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join("shapes.matrix"),
        "public enum Shape {\n    Empty,\n    Circle(radius),\n}\npublic fn area(s) {\n    return match (s) {\n        Circle(r) => 3 * r * r,\n        Empty() => 0,\n    };\n}\n",
    )
    .expect("write module");
    std::fs::write(
        dir.join("main.matrix"),
        "import \"shapes\";\nfn main() {\n    println(area(Circle(2)));\n    println(area(Empty()));\n    return 0;\n}\n",
    )
    .expect("write main");
    for engine in ["tree", "vm"] {
        let o = Command::new(bin())
            .args([
                "run",
                "--engine",
                engine,
                dir.join("main.matrix").to_str().unwrap(),
            ])
            .current_dir(&dir)
            .output()
            .expect("spawn");
        assert_eq!(
            String::from_utf8_lossy(&o.stdout).trim_end(),
            "12\n0",
            "[import / {}] stdout (stderr: {})",
            engine,
            String::from_utf8_lossy(&o.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `match` in the *importing* file, on a variant declared in another file.
///
/// The lowering pass never sees that declaration, so it cannot know the field
/// names — the constructor stores them in `__fields` and the pattern binds by
/// position. Without that, the arm body failed with `unknown name`.
#[test]
fn a_match_on_an_imported_enum_binds_its_payload() {
    let dir = std::env::temp_dir().join("spid3r_stage3_xmod");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(
        dir.join("shapes.matrix"),
        "public enum Shape {\n    Empty,\n    Circle(radius),\n}\n",
    )
    .expect("write module");
    std::fs::write(
        dir.join("main.matrix"),
        "import \"shapes\";\nfn main() {\n    let s = Circle(7);\n    let r = match (s) {\n        Circle(x) => x,\n        Empty() => 0,\n    };\n    println(r);\n    return 0;\n}\n",
    )
    .expect("write main");
    for engine in ["tree", "vm"] {
        let o = Command::new(bin())
            .args([
                "run",
                "--engine",
                engine,
                dir.join("main.matrix").to_str().unwrap(),
            ])
            .current_dir(&dir)
            .output()
            .expect("spawn");
        assert_eq!(
            String::from_utf8_lossy(&o.stdout).trim_end(),
            "7",
            "[xmod / {}] stdout (stderr: {})",
            engine,
            String::from_utf8_lossy(&o.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
