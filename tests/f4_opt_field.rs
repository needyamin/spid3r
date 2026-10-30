//! Null-safe dot semantics (F-4 regression pin).
//!
//! `obj?.field` used to guard only a null receiver: a missing map key still
//! threw `missing key 'k'` on every engine, so the documented
//! `m?.k ?? fallback` idiom never protected map reads. Since the F-4 fix
//! `?.` is a first-class operation on every engine (`Expr::OptField` survives
//! lowering):
//!
//!   - a null receiver AND a missing map key read as `null` (then `??`
//!     applies naturally),
//!   - everything else matches plain `.`: an instance without the field and
//!     a non-fieldable type still raise the same catchable errors,
//!   - `m.get(k)` keeps throwing.
//!
//! The probe prints only engine-independent facts, so stdout must be
//! byte-identical on tree, VM, and native. The VM side additionally exercises
//! the additive `FieldGetSoft` opcode (docs/BYTECODE_STABILITY.md: new
//! opcodes need no format-version bump).

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

const PROBE: &str = r#"class Box {
    public let v;
    fn new(v) { self.v = v; }
}

fn main() {
    let m = map("a", 1);
    println("1 exists: {m?.a}");
    println("2 missing: {m?.missing}");
    println("3 idiom: {m?.missing ?? -1}");
    let z = null;
    println("4 null recv: {z?.k}");
    println("5 null idiom: " + string(z?.k ?? "fb"));
    let n = map("a", map("b", 42));
    println("6 chain ok: {n?.a?.b}");
    println("7 chain miss mid: {n?.x?.b}");
    println("8 chain miss leaf: {n?.a?.x}");

    println("9 get still throws:");
    try {
        m.get("missing");
        println("   no throw?");
    } catch(e) {
        println("   caught: {e}");
    }

    println("10 dot still throws:");
    try {
        let w = m.missing;
        println("   no throw?");
    } catch(e) {
        println("   caught: {e}");
    }

    println("11 instance miss still throws:");
    try {
        let b = new Box(3);
        let h = map("o", b);
        let q = h["o"]?.nope;
        println("   no throw? {q}");
    } catch(e) {
        println("   caught: {e}");
    }

    println("12 non-fieldable still throws:");
    try {
        let q = (5)?.x;
        println("   no throw? {q}");
    } catch(e) {
        println("   caught: {e}");
    }

    println("== done ==");
    return 0;
}
"#;

const EXPECTED: &str = concat!(
    "1 exists: 1\n",
    "2 missing: null\n",
    "3 idiom: -1\n",
    "4 null recv: null\n",
    "5 null idiom: fb\n",
    "6 chain ok: 42\n",
    "7 chain miss mid: null\n",
    "8 chain miss leaf: null\n",
    "9 get still throws:\n",
    "   caught: missing key missing\n",
    "10 dot still throws:\n",
    "   caught: missing key 'missing'\n",
    "11 instance miss still throws:\n",
    "   caught: instance of Box has no field 'nope'\n",
    "12 non-fieldable still throws:\n",
    "   caught: int has no field 'x'\n",
    "== done ==\n",
);

#[test]
fn opt_field_semantics_hold_on_all_engines() {
    let dir = std::env::temp_dir();
    let src_path = dir.join("spid3r_f4_opt_field.matrix");
    std::fs::write(&src_path, PROBE).expect("write scratch probe");
    let src = src_path.to_str().unwrap().to_string();

    // tree engine
    let (code, tree, err) = run(Command::new(bin()).args(["run", "--engine", "tree", &src]));
    assert_eq!(code, 0, "tree exited {code}:\n{err}");
    assert_eq!(tree, EXPECTED, "tree semantics drifted");

    // vm engine: byte-identical (exercises the FieldGetSoft opcode)
    let (code, vm, err) = run(Command::new(bin()).args(["run", "--engine", "vm", &src]));
    assert_eq!(code, 0, "vm exited {code}:\n{err}");
    assert_eq!(vm, tree, "vm stdout differs from tree");

    // native engine: byte-identical (exercises spid3r_field_get_soft)
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
        "spid3r_f4_opt_field.exe"
    } else {
        "spid3r_f4_opt_field"
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
