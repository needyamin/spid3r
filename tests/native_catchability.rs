//! Native-backend catchability matrix (F-9 regression pin).
//!
//! Two native runtime error paths used to bypass `try`/`catch` and abort the
//! process with exit 70 while tree/VM caught them and exited 0:
//!
//!   (a) `m?.nope` / `m.nope` on a map without the key
//!       (`spid3r_field_get`'s map arm aborted),
//!   (b) unbounded recursion hitting the call-depth limit
//!       (`enter_call` aborted).
//!
//! Both now route through the same pending-throw channel the generated
//! `spid3r_threw` guards already use for catchable errors. This test mirrors
//! `examples/complex_app/probes4.matrix` on all three engines: stdout must be
//! byte-identical and every engine must exit 0 with every error caught.
//!
//! NOTE (F-4 fix, 2026-10-25): `m?.missing` no longer throws on any engine —
//! the null-safe dot reads a missing map key as null (see
//! `tests/f4_opt_field.rs`) — so path 1 pins that instead and the catchable
//! missing-key error is exercised by the plain-dot path 2.

#![cfg(feature = "native")]

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

/// The LLVM-C.dll gate shared by the other native suites.
fn native_available() -> bool {
    bin()
        .parent()
        .map(|d| d.join("LLVM-C.dll"))
        .unwrap_or_default()
        .is_file()
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

/// The catchability matrix. Every `try` block must be caught: the process
/// prints the "caught" line and reaches "end reached" with exit 0 on every
/// engine.
const PROBE: &str = r#"fn boom(n) {
    return boom(n + 1);
}

fn down(n) {
    if n <= 0 {
        return 0;
    }
    return down(n - 1) + 1;
}

class Box {
    let x;
    fn new(v) { self.x = v; }
}

fn main() {
    // 1. safe-dot on a missing map key reads as null now (F-4), it does not
    // throw on any engine
    try {
        let m = map("a", 1);
        let v = m?.nope;
        println("1 safe-dot null: " + string(v == null));
    } catch(e) {
        println("1 caught (wrong after F-4): " + e);
    }

    // plain dot on a missing map key
    try {
        let m2 = map("a", 1);
        let w = m2.nope;
        println("2: no throw?");
    } catch(e) {
        println("2 caught: " + e);
    }

    // (b) call-depth overflow is a catchable runtime error, not an abort
    try {
        boom(0);
        println("3: no throw?");
    } catch(e) {
        println("3 caught: " + e);
    }

    // the depth counter unwound: normal recursion works after the catch
    println("4 depth ok: {down(8)}");

    // calling a non-callable value
    try {
        let f = 42;
        f(1);
        println("5: no throw?");
    } catch(e) {
        println("5 caught: " + e);
    }

    // missing instance field, read and write (dynamic receivers so the
    // semantic checker cannot reject them statically)
    try {
        let b = new Box(3);
        let h = map("obj", b);
        let q = h["obj"].missing;
        println("6r: no throw?");
    } catch(e) {
        println("6r caught: " + e);
    }
    try {
        let b2 = new Box(3);
        let h2 = map("obj", b2);
        h2["obj"].missing = 5;
        println("6w: no throw?");
    } catch(e) {
        println("6w caught: " + e);
    }

    println("end reached");
    return 0;
}
"#;

const EXPECTED: &str = concat!(
    "1 safe-dot null: true\n",
    "2 caught: missing key 'nope'\n",
    "3 caught: stack overflow: call depth limit reached\n",
    "4 depth ok: 8\n",
    "5 caught: int is not callable\n",
    "6r caught: instance of Box has no field 'missing'\n",
    "6w caught: instance of Box has no field 'missing'\n",
    "end reached\n",
);

#[test]
fn runtime_error_paths_are_catchable_on_all_engines() {
    let dir = std::env::temp_dir();
    let src_path = dir.join("spid3r_f9_catchability.matrix");
    std::fs::write(&src_path, PROBE).expect("write scratch probe");
    let src = src_path.to_str().unwrap().to_string();

    // tree engine: exit 0, every path caught
    let (code, tree, err) = run(Command::new(bin()).args(["run", "--engine", "tree", &src]));
    assert_eq!(code, 0, "tree exited {code}:\n{err}");
    assert_eq!(tree, EXPECTED, "tree stdout drifted");

    // vm engine: byte-identical stdout, exit 0
    let (code, vm, err) = run(Command::new(bin()).args(["run", "--engine", "vm", &src]));
    assert_eq!(code, 0, "vm exited {code}:\n{err}");
    assert_eq!(vm, tree, "vm stdout differs from tree");

    // native engine: the actual F-9 pin. Before the fix this aborted with
    // exit 70 after the fourth path; now every path is caught, exit 0,
    // stdout byte-identical.
    if !native_available() {
        eprintln!("skip native arm: LLVM-C.dll not found");
        return;
    }
    let exe = dir.join(if cfg!(windows) {
        "spid3r_f9_catchability.exe"
    } else {
        "spid3r_f9_catchability"
    });
    let exe_s = exe.to_str().unwrap().to_string();
    let (bcode, _, berr) = run(Command::new(bin()).args(["build", &src, "-o", &exe_s]));
    assert_eq!(bcode, 0, "native build failed:\n{berr}");
    let (code, native, err) = run(&mut Command::new(&exe));
    assert_eq!(
        code, 0,
        "native exited {code} (F-9 regression: an error path escaped try/catch):\n{err}"
    );
    assert_eq!(native, tree, "native stdout differs from tree");
    let _ = std::fs::remove_file(&exe);
    let _ = std::fs::remove_file(&src_path);
}
