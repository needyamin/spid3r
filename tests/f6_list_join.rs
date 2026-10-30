//! list.join auto-stringify (F-6 regression pin).
//!
//! `list.join(sep)` used to refuse any non-string element:
//! `[1, 2, 3].join(",")` died with `error[runtime]: join needs strings,
//! found int`, forcing every program that prints an int list through a
//! `to_strs` shim. Since the F-6 fix each element is auto-stringified with
//! the canonical display form — the exact text `print`, `string()` and
//! interpolation produce — so `[1, 2].join("-")` is `"1-2"`. All-string
//! lists are byte-for-byte what they always were, and the separator is
//! still type-checked.
//!
//! The join runs with the list's lock released (audit A-7 pattern): an
//! element may be (or reach) the receiver and the shared display function
//! takes container locks of its own, so a self-referential list must not
//! deadlock. The probe pins that case too.
//!
//! One shared implementation behind `collections::call_method`, so stdout
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

const PROBE: &str = r#"fn check(n, got, want) {
    if got == want {
        println(n + " ok");
    } else {
        println(n + " FAIL got=[" + got + "] want=[" + want + "]");
    }
}

fn main() {
    // the F-6 repro: int lists join directly
    check("1 int join", [1, 2, 3].join(","), "1,2,3");
    check("2 dash join", [1, 2, 3].join("-"), "1-2-3");
    // mixed scalars use the canonical display (same text as string(x))
    check("3 mixed join", [1, 1.5, true, null, "x"].join("|"), "1|1.5|true|null|x");
    // all-string lists are unchanged
    check("4 string join", ["a", "b", "c"].join("+"), "a+b+c");
    // empty separator / empty list
    check("5 empty sep", [1, 2].join(""), "12");
    check("6 empty list", [].join(","), "");
    // container elements stringify to their display form
    check("7 nested join", [[1, 2], [3]].join(";"), "[1, 2];[3]");
    check("8 map elem join", [map("k", 1), 2].join(","), "map{k: 1},2");
    // float formatting matches print/string (5.0 stays "5.0")
    check("9 float join", [5.0, 0.5].join(","), "5.0,0.5");
    // join output is exactly string(x) per element (no second dialect)
    check("10 string consistency", [1, true].join(","), string(1) + "," + string(true));
    // self-referential list: no deadlock, cycle prints as [...]
    let sr = [1];
    sr.push(sr);
    check("11 selfref join", sr.join(","), "1,[1, [...]]");
    // the separator itself is still type-checked (clean catchable error)
    try {
        [1].join(2);
        println("12 no throw?");
    } catch(e) {
        println("12 caught: " + e);
    }
    // the split()->join() round trip (stdlib idiom) is unchanged
    check("13 split join", "a,b".split(",").join("|"), "a|b");
    // vec shares the list implementation
    check("14 vec join", vec(1, 2).join(","), "1,2");
    // single element
    check("15 single join", [42].join(","), "42");
    println("== f6 verified ==");
    return 0;
}
"#;

const EXPECTED: &str = concat!(
    "1 int join ok\n",
    "2 dash join ok\n",
    "3 mixed join ok\n",
    "4 string join ok\n",
    "5 empty sep ok\n",
    "6 empty list ok\n",
    "7 nested join ok\n",
    "8 map elem join ok\n",
    "9 float join ok\n",
    "10 string consistency ok\n",
    "11 selfref join ok\n",
    "12 caught: list.join expects a string, found int\n",
    "13 split join ok\n",
    "14 vec join ok\n",
    "15 single join ok\n",
    "== f6 verified ==\n",
);

#[test]
fn list_join_auto_stringifies_elements() {
    let dir = std::env::temp_dir();
    let src_path = dir.join("spid3r_f6_join.matrix");
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

    // native engine: same shared collections dispatcher, pinned end-to-end.
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
        "spid3r_f6_join.exe"
    } else {
        "spid3r_f6_join"
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
