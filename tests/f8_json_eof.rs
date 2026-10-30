//! `json_parse` end-of-input diagnostic (F-8 regression pin).
//!
//! Truncated JSON used to leak Rust internals: `json_parse("{\"broken\": ")`
//! reported `json_parse: unexpected character None` — `None` is
//! `Option::None`'s Debug formatting, because the reader formatted its
//! `Option<char>` peek with `{:?}` at every "unexpected …" site (value,
//! array, object). Since the F-8 fix the EOF case has its own wording,
//! `unexpected end of input`, while a real bad character is still named
//! (`unexpected character '@'`) and every other diagnostic is unchanged.
//!
//! One shared parser behind the `json_parse` builtin (and the LSP's
//! `json_parse_text`), so stdout must be byte-identical on tree, VM, and
//! native.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn native_available() -> bool {
    if cfg!(windows) {
        bin()
            .parent()
            .map(|d| d.join("LLVM-C.dll"))
            .unwrap_or_default()
            .is_file()
    } else {
        Command::new("cc").arg("--version").output().is_ok()
            || Command::new("clang").arg("--version").output().is_ok()
    }
}

/// Run a command, returning (exit_code, stdout, stderr).
fn run(cmd: &mut Command) -> (i32, String, String) {
    let o = cmd.output().expect("spawn failed");
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

const PROBE: &str = r#"fn check(n, got, want) {
    if got == want {
        println(n + " ok");
    } else {
        println(n + " FAIL got=[" + got + "] want=[" + want + "]");
    }
}

fn caught(src) {
    try {
        json_parse(src);
        return "NO-THROW";
    } catch(e) {
        return e;
    }
}

fn main() {
    // the F-8 repro: truncated object after the colon
    check("1 truncated object", caught("{\"broken\": "), "json_parse: unexpected end of input");
    // truncated arrays (the "similar phrasing" case from the finding)
    check("2 truncated array", caught("[\"a\", 100000"), "json_parse: unexpected end of input in array");
    check("3 truncated array elem", caught("[1, "), "json_parse: unexpected end of input");
    // truncated object value and empty input
    check("4 truncated obj value", caught("{\"k\":"), "json_parse: unexpected end of input");
    check("5 empty input", caught(""), "json_parse: unexpected end of input");
    // the leaked Debug text must be gone everywhere
    let e = caught("{\"broken\": ");
    check("6 no None leak", e.contains("None"), false);
    // a real bad character is still named, in every position
    check("7 bad char value", caught("@"), "json_parse: unexpected character '@'");
    check("8 bad char array", caught("[1 2]"), "json_parse: unexpected character '2' in array");
    check("9 bad char object", caught("{\"a\": @}"), "json_parse: unexpected character '@'");
    // unrelated diagnostics are untouched
    check("10 bad literal", caught("tru"), "json_parse: expected 'true'");
    check("11 bad number", caught("1."), "json_parse: bad number");
    // valid inputs still parse
    check("12 valid array", json_parse("[1, 2]").len(), 2);
    check("13 valid object", json_parse("{\"a\": 1}").a, 1);
    println("== f8 verified ==");
    return 0;
}
"#;

const EXPECTED: &str = concat!(
    "1 truncated object ok\n",
    "2 truncated array ok\n",
    "3 truncated array elem ok\n",
    "4 truncated obj value ok\n",
    "5 empty input ok\n",
    "6 no None leak ok\n",
    "7 bad char value ok\n",
    "8 bad char array ok\n",
    "9 bad char object ok\n",
    "10 bad literal ok\n",
    "11 bad number ok\n",
    "12 valid array ok\n",
    "13 valid object ok\n",
    "== f8 verified ==\n",
);

#[test]
fn json_parse_eof_diagnostic_says_end_of_input() {
    let dir = std::env::temp_dir();
    let src_path = dir.join("spid3r_f8_json_eof.matrix");
    std::fs::write(&src_path, PROBE).expect("write scratch probe");
    let src = src_path.to_str().unwrap().to_string();

    // tree engine
    let (code, tree, err) = run(Command::new(bin()).args(["run", "--engine", "tree", &src]));
    assert_eq!(code, 0, "tree exited {code}:\n{err}");
    assert_eq!(tree, EXPECTED, "tree output drifted");

    // vm engine: byte-identical
    let (code, vm, err) = run(Command::new(bin()).args(["run", "--engine", "vm", &src]));
    assert_eq!(code, 0, "vm exited {code}:\n{err}");
    assert_eq!(vm, tree, "vm stdout differs from tree");

    // native engine: the builtin is the same shared Rust parser.
    // The AOT arm only exists when the native backend is compiled in. The
    // default build still runs the tree/vm assertions above, which is the
    // part of this pin that guards language semantics (audit F-09).
    if !cfg!(feature = "native") {
        eprintln!("skip native arm: built without the native backend");
        let _ = std::fs::remove_file(&src_path);
        return;
    }
    if !native_available() {
        eprintln!("skip native arm: LLVM-C.dll not found");
        let _ = std::fs::remove_file(&src_path);
        return;
    }
    let exe = dir.join(if cfg!(windows) {
        "spid3r_f8_json_eof.exe"
    } else {
        "spid3r_f8_json_eof"
    });
    let exe_s = exe.to_str().unwrap().to_string();
    let (bcode, _, berr) = run(Command::new(bin()).args(["build", &src, "-o", &exe_s]));
    assert_eq!(bcode, 0, "native build failed:\n{berr}");
    let (code, native, err) = run(&mut Command::new(&exe));
    assert_eq!(code, 0, "native exited {code}:\n{err}");
    assert_eq!(native, tree, "native stdout differs from tree");
    let _ = std::fs::remove_file(&exe);
    let _ = std::fs::remove_file(&src_path);
}
