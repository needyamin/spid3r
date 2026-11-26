//! Trailing-comma parity (F-5 regression pin).
//!
//! Trailing commas used to be accepted in `match` arms and tuple literals but
//! rejected everywhere else: `add(1, 2,)`, `let xs = [1, 2, 3,];` and
//! `let (a, b,) = pair;` all failed with `error[parser]: unexpected ')'` /
//! `']'` — exactly in the multi-line constructors where a trailing comma is
//! natural. Since the F-5 fix the comma loops for call arguments (including
//! `map(...)`/`tuple(...)` constructors and named arguments), list literals,
//! and `let`-destructuring all follow the match-arm policy: a trailing comma
//! before the closer is accepted and ignored.
//!
//! Pure parser change with no engine-specific behavior, so the probe's stdout
//! must be byte-identical on tree, VM, and native.

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

const PROBE: &str = r#"fn add(a, b) {
    return a + b;
}

fn main() {
    // trailing comma in call args (the F-5 repro)
    println(add(1, 2,));
    // multi-line map constructor, the natural shape
    let m = map(
        "a", 1,
        "b", 2,
    );
    println(m.b);
    // trailing comma in a list literal
    let xs = [1, 2, 3,];
    println(xs.len());
    // trailing comma in destructuring
    let (p, q,) = [10, 20];
    println(p + q);
    // tuple literal (already worked before the fix)
    let t = (7, 8,);
    println(t[1]);
    // single-element list with a trailing comma
    let one = [9,];
    println(one[0]);
    // no-trailing-comma calls are untouched
    println(add(5, 6));
    println("== done ==");
    return 0;
}
"#;

const EXPECTED: &str = concat!(
    "3\n",
    "2\n",
    "3\n",
    "30\n",
    "8\n",
    "9\n",
    "11\n",
    "== done ==\n",
);

#[test]
fn trailing_commas_are_accepted_like_match_arms() {
    let dir = std::env::temp_dir();
    let src_path = dir.join("spid3r_f5_trailing.matrix");
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

    // native engine: the compiled path parses with the same parser, but pin
    // it end-to-end anyway.
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
        "spid3r_f5_trailing.exe"
    } else {
        "spid3r_f5_trailing"
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
