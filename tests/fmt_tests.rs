//! `spid3r fmt` tests: formatting must preserve the token stream exactly and
//! must be idempotent, for every shipped example/benchmark and for hand-written
//! edge cases.

use std::path::PathBuf;
use std::sync::Arc;

use spid3r::fmt::format_source;
use spid3r::lexer::Lexer;

fn tokens_of(src: &str) -> Vec<spid3r::lexer::token::TokenKind> {
    let file: Arc<str> = Arc::from("<fmt-test>");
    Lexer::new(src, file)
        .tokenize()
        .expect("lex")
        .iter()
        .map(|t| t.kind.clone())
        .collect()
}

fn check_roundtrip(name: &str, src: &str) {
    let file: Arc<str> = Arc::from("<fmt-test>");
    let formatted = format_source(src, &file)
        .unwrap_or_else(|e| panic!("{}: fmt failed: {}", name, e.message));
    assert_eq!(
        tokens_of(src),
        tokens_of(&formatted),
        "{}: fmt changed the token stream\n--- formatted ---\n{}",
        name,
        formatted
    );
    let twice = format_source(&formatted, &file)
        .unwrap_or_else(|e| panic!("{}: second fmt failed: {}", name, e.message));
    assert_eq!(
        formatted, twice,
        "{}: fmt is not idempotent\n--- once ---\n{}\n--- twice ---\n{}",
        name, formatted, twice
    );
}

#[test]
fn formats_every_example_and_benchmark() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in ["examples", "benchmarks"] {
        let d = root.join(dir);
        if !d.is_dir() {
            continue;
        }
        for e in std::fs::read_dir(&d).expect("read dir") {
            let p = e.expect("entry").path();
            if p.extension().map(|x| x == "matrix").unwrap_or(false) {
                files.push(p);
            }
        }
    }
    assert!(files.len() >= 20, "expected the shipped corpus, got {}", files.len());
    for p in files {
        let src = std::fs::read_to_string(&p).expect("read");
        check_roundtrip(&p.to_string_lossy(), &src);
    }
}

#[test]
fn formats_a_messy_program_canonically() {
    let src = "fn main() {\nlet xs=list(1,2,3);\nif xs.len()>0 { println( xs ); }\n\
               let f=fn(x){return -x+1;};\ntry { throw \"e\"; } catch (e) { println(e); }\n\
               // kept comment\nreturn 0;\n}\n";
    let file: Arc<str> = Arc::from("<fmt-test>");
    let out = format_source(src, &file).expect("fmt");
    let expected = "\
fn main() {
    let xs = list(1, 2, 3);
    if xs.len() > 0 {
        println(xs);
    }
    let f = fn(x) {
        return -x + 1;
    };
    try {
        throw \"e\";
    } catch(e) {
        println(e);
    }
    // kept comment
    return 0;
}
";
    assert_eq!(out, expected);
}

#[test]
fn cli_fmt_check_exits_nonzero_on_unformatted_input() {
    use std::process::Command;
    let dir = std::env::temp_dir().join(format!("spid3r_fmt_check_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let messy = dir.join("messy.matrix");
    std::fs::write(&messy, "fn main() {\nlet x=1;\nprintln(x);\nreturn 0;\n}\n").expect("write");
    // --check reports and exits non-zero without touching the file.
    let out = Command::new(env!("CARGO_BIN_EXE_spid3r"))
        .args(["fmt", "--check", messy.to_str().unwrap()])
        .output()
        .expect("run fmt --check");
    assert!(!out.status.success(), "--check should fail on unformatted input");
    assert_eq!(std::fs::read_to_string(&messy).unwrap(), "fn main() {\nlet x=1;\nprintln(x);\nreturn 0;\n}\n");
    // Plain fmt rewrites; a second --check then passes.
    let out = Command::new(env!("CARGO_BIN_EXE_spid3r"))
        .args(["fmt", messy.to_str().unwrap()])
        .output()
        .expect("run fmt");
    assert!(out.status.success(), "fmt should succeed");
    let out = Command::new(env!("CARGO_BIN_EXE_spid3r"))
        .args(["fmt", "--check", messy.to_str().unwrap()])
        .output()
        .expect("run fmt --check");
    assert!(out.status.success(), "--check should pass after fmt");
    let formatted = std::fs::read_to_string(&messy).unwrap();
    assert!(formatted.contains("    let x = 1;"), "canonical form: {}", formatted);
    let _ = std::fs::remove_dir_all(&dir);
}


#[test]
fn keywords_operators_and_for_headers_are_canonical() {
    let src = "fn main(){for let k=0;k<3;k=k+1{println(k*2);}}\n";
    let file: Arc<str> = Arc::from("<fmt-test>");
    let out = format_source(src, &file).expect("fmt");
    assert!(
        out.contains("for let k = 0; k < 3; k = k + 1 {"),
        "for header should be spaced: {}",
        out
    );
    assert!(out.contains("println(k * 2);"), "binary * should be spaced: {}", out);
    check_roundtrip("for-header", src);
}

#[test]
fn strings_and_float_literals_are_untouched() {
    let src = "fn main() {\n    let a = \"x\\n\\t\\\"y\\\"\";\n    let b = 7.0;\n    let c = 0.5;\n    println(a + string(b + c));\n    return 0;\n}\n";
    let file: Arc<str> = Arc::from("<fmt-test>");
    let out = format_source(src, &file).expect("fmt");
    assert!(out.contains("7.0"), "float literal must keep its form: {}", out);
    assert!(out.contains("0.5"), "float literal must keep its form: {}", out);
    assert!(out.contains("\\n\\t"), "string escapes must be preserved: {}", out);
    check_roundtrip("literals", src);
}

#[test]
fn long_argument_lists_are_wrapped_one_per_line() {
    let src = "fn main() {\n    let config = map(\"name\", \"spid3r\", \"version\", \"0.1.0\", \"author\", \"someone\", \"license\", \"MIT\", \"extra\", 1);\n    return 0;\n}\n";
    let file: Arc<str> = Arc::from("<fmt-test>");
    let out = format_source(src, &file).expect("fmt");
    assert!(
        out.contains("map(\n        \"name\",\n        \"spid3r\","),
        "expected one argument per line:/n{}",
        out
    );
    assert!(out.contains("\n    );"), "closing paren on its own line:/n{}", out);
    check_roundtrip("wrapped-call", src);
}

#[test]
fn nested_long_calls_wrap_independently() {
    let src = "fn main() {\n    println(concat_all(\"aaaaaaaaaa\", \"bbbbbbbbbb\", \"cccccccccc\", \"dddddddddd\", \"eeeeeeeeee\", \"ffffffffff\"));\n    return 0;\n}\n";
    let file: Arc<str> = Arc::from("<fmt-test>");
    let out = format_source(src, &file).expect("fmt");
    // Inner call broken one-per-line, indented under `println(`.
    assert!(
        out.contains("            \"aaaaaaaaaa\",\n            \"bbbbbbbbbb\","),
        "inner call should be wrapped and indented:/n{}",
        out
    );
    check_roundtrip("nested-wrap", src);
}

#[test]
fn short_calls_stay_flat_even_on_long_lines() {
    // The line is over the width limit, but no group is substantial enough to
    // break, so the formatter leaves it alone rather than exploding one small
    // call across three lines.
    let src = "fn main() {\n    let result = compute(1111111, 2222222, 3333333) + compute(4444444, 5555555, 6666666) + compute(7, 8, 9);\n    return 0;\n}\n";
    let file: Arc<str> = Arc::from("<fmt-test>");
    let out = format_source(src, &file).expect("fmt");
    assert!(
        out.contains("compute(7, 8, 9);"),
        "short call must stay flat:/n{}",
        out
    );
    check_roundtrip("flat-long-line", src);
}

#[test]
fn closure_arguments_keep_the_call_compact() {
    // A block inside the arguments owns the indentation: the call stays on one
    // line up to the `{`, and closes on the body's `}` line (`}));`).
    let src = "fn main() {\n    println(xs.map(fn(x) {\n        return x * x;\n    }));\n    return 0;\n}\n";
    let file: Arc<str> = Arc::from("<fmt-test>");
    let out = format_source(src, &file).expect("fmt");
    assert!(
        out.contains("println(xs.map(fn(x) {"),
        "call should stay compact:/n{}",
        out
    );
    assert!(out.contains("    }));"), "closing should join the brace:/n{}", out);
    check_roundtrip("closure-arg", src);
}

#[test]
fn cli_fmt_check_accepts_crlf_line_endings() {
    use std::process::Command;
    let dir = std::env::temp_dir().join(format!("spid3r_fmt_crlf_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let crlf_file = dir.join("canonical_crlf.matrix");
    // Exact canonical formatting, but with Windows CRLF (\r\n) line endings as produced
    // by `git checkout` with `core.autocrlf=true` on Windows CI runners.
    let canonical_crlf = "// header comment\r\nfn main() {\r\n    let x = 1;\r\n    println(x);\r\n    return 0;\r\n}\r\n";
    std::fs::write(&crlf_file, canonical_crlf).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_spid3r"))
        .args(["fmt", "--check", crlf_file.to_str().unwrap()])
        .output()
        .expect("run fmt --check on CRLF file");
    assert!(
        out.status.success(),
        "--check should succeed on canonically formatted CRLF files, stdout: {}, stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

