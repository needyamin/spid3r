//! GC observability contract pin (F-3 regression test).
//!
//! `gc_live_cells()` and `gc_collect()`'s swept count are engine-internal
//! memory observability (the native backend is an arena — values live for the
//! process — so the same workload reports tree 380 / vm 159 / native 300462
//! cells by design). They are exempt from byte-parity via ENGINE_CONTRACT
//! exemption #4; what MUST be identical on every engine is the portable
//! subset:
//!
//!   - both builtins exist and return `int`,
//!   - `gc_collect()` completes safely (quiescent gate, no error),
//!   - live data (a kept cycle) always survives a collection.
//!
//! The probe below prints only those portable facts, so its stdout must be
//! byte-identical on tree, VM, and native — including the 20 000-cycle churn
//! that makes the raw counts diverge.

#![cfg(feature = "native")]

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

/// Prints ONLY portable facts about the GC builtins. The raw cell counts are
/// deliberately never printed (they are engine-internal — F-3).
const PROBE: &str = r#"class Cycler {
    public let link;
    public let val;
    fn new(v) { self.val = v; self.link = null; }
}

fn main() {
    // a kept two-node cycle: must survive every collection on every engine
    let kept1 = Cycler(4242);
    let kept2 = Cycler(4243);
    kept1.link = kept2;
    kept2.link = kept1;

    // churn abandoned cycles: the cell counts diverge here by design
    let k = 0;
    while (k < 20000) {
        let x = Cycler(k);
        let y = Cycler(k + 1);
        x.link = y;
        y.link = x;
        k = k + 1;
    }

    let swept = gc_collect();
    let cells = gc_live_cells();

    println("swept type: {type_of(swept)}");
    println("swept non-negative: {string(swept >= 0)}");
    println("cells type: {type_of(cells)}");
    println("cells positive: {string(cells > 0)}");
    println("kept intact after collect: {string(kept1.link.val == 4243 and kept1.link.link.val == 4242)}");
    return 0;
}
"#;

const EXPECTED: &str = concat!(
    "swept type: int\n",
    "swept non-negative: true\n",
    "cells type: int\n",
    "cells positive: true\n",
    "kept intact after collect: true\n",
);

#[test]
fn gc_builtins_portable_facts_hold_on_all_engines() {
    let dir = std::env::temp_dir();
    let src_path = dir.join("spid3r_f3_gc_portability.matrix");
    std::fs::write(&src_path, PROBE).expect("write scratch probe");
    let src = src_path.to_str().unwrap().to_string();

    // tree engine
    let (code, tree, err) = run(Command::new(bin()).args(["run", "--engine", "tree", &src]));
    assert_eq!(code, 0, "tree exited {code}:\n{err}");
    assert_eq!(tree, EXPECTED, "tree portable facts drifted");

    // vm engine: byte-identical
    let (code, vm, err) = run(Command::new(bin()).args(["run", "--engine", "vm", &src]));
    assert_eq!(code, 0, "vm exited {code}:\n{err}");
    assert_eq!(vm, tree, "vm stdout differs from tree");

    // native engine: the F-3 pin. The raw numbers diverge by design (arena
    // model), but the portable facts must hold and the output must be
    // byte-identical.
    if !native_available() {
        eprintln!("skip native arm: LLVM-C.dll not found");
        return;
    }
    let exe = dir.join(if cfg!(windows) {
        "spid3r_f3_gc_portability.exe"
    } else {
        "spid3r_f3_gc_portability"
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
