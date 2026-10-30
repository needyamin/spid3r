//! `\u` escapes in string literals (F-7 regression pin).
//!
//! Source code could not spell a non-ASCII escape: `"\u{1F600}"` died with
//! `error[lexer]: unknown escape '\u'`, even though the JSON layer accepted
//! `\uXXXX` (with surrogate pairs) — an asymmetry that forced raw UTF-8 in
//! source. Since the F-7 fix the lexer's escape scan accepts both forms:
//!
//! * `\u{XXXXXX}` — Rust-style: 1–6 hex digits naming any Unicode scalar
//!   value (`\u{1F600}` is 😀, `\u{7e}` is `~`).
//! * `\uXXXX` — JSON-style: exactly 4 hex digits; a high surrogate must be
//!   followed by a `\uXXXX` low surrogate and the pair combines into one
//!   astral character — the same rule the JSON reader applies.
//!
//! Malformed escapes are clean lexer diagnostics at the backslash
//! (`\u{}`, `\u{110000}`, `\u{D800}`, `\u{1234567}`, `\u12`, `\uD83D`,
//! `\uDC00`, bad pairs). The escapes resolve at lex time, so every engine
//! sees the same decoded string: the probe's stdout must be byte-identical
//! on tree, VM, and native.

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

fn main() {
    // braced form, astral and BMP
    check("1 braced astral", "\u{1F600}", chr(128512));
    check("2 braced BMP", "\u{e9}", chr(233));
    // JSON-style \uXXXX, including surrogate pairing
    check("3 4-digit form", "\u0041\u00e9", "A" + chr(233));
    check("4 surrogate pair", "\uD83D\uDE00", chr(128512));
    // the maximum scalar value
    check("5 max scalar", "\u{10FFFF}", chr(1114111));
    // embedded in ordinary text; len counts characters, not bytes
    check("6 mixed text", "A\u{42}C", "ABC");
    check("7 len semantics", "x\u{1F600}y".len(), 3);
    // the two spellings agree, and both agree with the JSON layer
    check("8 braced==4digit", "\u{1F600}", "\uD83D\uDE00");
    check("9 source==json", "\uD83D\uDE00", json_parse("\"\\uD83D\\uDE00\""));
    // ord round trip
    check("10 ord round trip", ord("\u{1F600}"), 128512);
    // the pre-existing escapes are untouched
    check("11 old escapes", "\n\t\\\"", chr(10) + chr(9) + chr(92) + chr(34));
    // escapes inside interpolation holes (the hole capture is re-lexed)
    println("12 hole: " + "A\u{42}C");
    println("== f7 verified ==");
    return 0;
}
"#;

const EXPECTED: &str = concat!(
    "1 braced astral ok\n",
    "2 braced BMP ok\n",
    "3 4-digit form ok\n",
    "4 surrogate pair ok\n",
    "5 max scalar ok\n",
    "6 mixed text ok\n",
    "7 len semantics ok\n",
    "8 braced==4digit ok\n",
    "9 source==json ok\n",
    "10 ord round trip ok\n",
    "11 old escapes ok\n",
    "12 hole: ABC\n",
    "== f7 verified ==\n",
);

/// Malformed escapes: (source snippet, expected message fragment).
const BAD_ESCAPES: &[(&str, &str)] = &[
    ("\"\\u{}\"", "needs at least one hex digit"),
    ("\"\\u{110000}\"", "not a Unicode scalar value"),
    ("\"\\u{D800}\"", "not a Unicode scalar value"),
    ("\"\\u{1234567}\"", "at most 6 hex digits"),
    ("\"\\u12\"", "needs 4 hex digits"),
    ("\"\\uD83D\"", "lone high surrogate"),
    ("\"\\uDC00\"", "lone low surrogate"),
    ("\"\\uD83D\\u0041\"", "bad surrogate pair"),
];

#[test]
fn unicode_escapes_decode_identically_on_all_engines() {
    let dir = std::env::temp_dir();
    let src_path = dir.join("spid3r_f7_unicode.matrix");
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

    // native engine: the escapes are resolved by the shared lexer, so the
    // compiled binary must print the same bytes.
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
        "spid3r_f7_unicode.exe"
    } else {
        "spid3r_f7_unicode"
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

#[test]
fn malformed_unicode_escapes_are_clean_lexer_errors() {
    let dir = std::env::temp_dir();
    for (i, (esc, want)) in BAD_ESCAPES.iter().enumerate() {
        let src_path = dir.join(format!("spid3r_f7_bad_{i}.matrix"));
        std::fs::write(&src_path, format!("fn main() {{ let s = {esc}; }}\n"))
            .expect("write scratch probe");
        let src = src_path.to_str().unwrap().to_string();
        let (code, _, err) = run(Command::new(bin()).args(["run", &src]));
        assert_ne!(code, 0, "{esc} must not compile");
        assert!(
            err.contains("error[lexer]"),
            "{esc}: expected a lexer error, got:\n{err}"
        );
        assert!(
            err.contains(want),
            "{esc}: expected message fragment {want:?}, got:\n{err}"
        );
        let _ = std::fs::remove_file(&src_path);
    }
}
