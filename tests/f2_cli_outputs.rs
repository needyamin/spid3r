//! CLI output artifacts (F-2 regression pin).
//!
//! Two `compile`/`build` output gaps:
//!
//!   (a) `compile -o out` used to write an extensionless bytecode blob that
//!       `exec` and every doc referred to as `.matrixc` — the resolved path
//!       now gains the canonical `.matrixc` extension when the user gave
//!       none (an explicit extension is respected), and the resolved path is
//!       what gets printed.
//!   (b) `spid3r build` of a program with `import "x" as alias;` is rejected
//!       with a clean `error[usage]` diagnostic and exit code 2 — the
//!       contract shape for an unsupported construct (never silently
//!       mis-compiled, never a wrong-phase exit code). The same program runs
//!       on `--engine vm`, which implements module namespaces.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
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

#[test]
fn compile_o_without_extension_gains_matrixc() {
    let dir = std::env::temp_dir();
    let src = dir.join("spid3r_f2_src.matrix");
    std::fs::write(
        &src,
        "fn main() {\n    println(\"hi\");\n    return 0;\n}\n",
    )
    .expect("write scratch");
    let src_s = src.to_str().unwrap().to_string();

    // Extensionless -o: the artifact is <out>.matrixc and the printed path
    // is the resolved one.
    let out = dir.join("spid3r_f2_out");
    let _ = std::fs::remove_file(dir.join("spid3r_f2_out.matrixc"));
    let out_s = out.to_str().unwrap().to_string();
    let (code, stdout, err) = run(Command::new(bin()).args(["compile", &src_s, "-o", &out_s]));
    assert_eq!(code, 0, "compile failed:\n{err}");
    let resolved = dir.join("spid3r_f2_out.matrixc");
    assert!(
        resolved.is_file(),
        "expected {} to exist (the canonical extension must be appended)",
        resolved.display()
    );
    assert!(
        !out.is_file(),
        "the extensionless blob must not be written anymore"
    );
    assert!(
        stdout.contains(&resolved.to_string_lossy().to_string())
            || stdout.contains("spid3r_f2_out.matrixc"),
        "the resolved path must be printed, got: {stdout}"
    );

    // exec runs the artifact that compile named.
    let (code, stdout, err) = run(Command::new(bin()).arg("exec").arg(&resolved));
    assert_eq!(code, 0, "exec failed:\n{err}");
    assert_eq!(stdout, "hi\n");

    // An explicit extension is respected verbatim.
    let out2 = dir.join("spid3r_f2_keep.matrixc");
    let _ = std::fs::remove_file(&out2);
    let (code, _, err) = run(Command::new(bin())
        .args(["compile", &src_s, "-o"])
        .arg(&out2));
    assert_eq!(code, 0, "compile failed:\n{err}");
    assert!(out2.is_file(), "explicit .matrixc must be used verbatim");

    let _ = std::fs::remove_file(&resolved);
    let _ = std::fs::remove_file(&out2);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn native_rejects_aliased_import_as_usage_error() {
    let dir = std::env::temp_dir();
    let src = dir.join("spid3r_f2_alias.matrix");
    std::fs::write(
        &src,
        "import \"bigint\" as bi;\nfn main() {\n    println(\"x\");\n    return 0;\n}\n",
    )
    .expect("write scratch");
    let src_s = src.to_str().unwrap().to_string();

    // The same program runs on the VM (which implements namespaces).
    let (code, stdout, err) = run(Command::new(bin()).args(["run", "--engine", "vm", &src_s]));
    assert_eq!(code, 0, "vm run failed:\n{err}");
    assert_eq!(stdout, "x\n");

    // Native: clean rejection, error[usage], exit 2 (the contract shape for
    // an unsupported construct) — not error[semantic]/exit 1 (F-2 fix).
    if !bin()
        .parent()
        .map(|d| d.join("LLVM-C.dll"))
        .unwrap_or_default()
        .is_file()
    {
        eprintln!("skip native arm: LLVM-C.dll not found");
        let _ = std::fs::remove_file(&src);
        return;
    }
    let exe = dir.join("spid3r_f2_alias.exe");
    let exe_s = exe.to_str().unwrap().to_string();
    let (code, _, stderr) = run(Command::new(bin()).args(["build", &src_s, "-o", &exe_s]));
    assert_eq!(code, 2, "expected exit 2 (usage), got {code}:\n{stderr}");
    assert!(
        stderr.contains("error[usage]"),
        "expected an error[usage] diagnostic, got: {stderr}"
    );
    assert!(
        stderr.contains("is not supported by the native backend yet"),
        "expected the documented gap message, got: {stderr}"
    );
    let _ = std::fs::remove_file(&src);
}
