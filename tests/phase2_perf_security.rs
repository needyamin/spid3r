//! Phase 2 (performance program) security regression: a native disabled by
//! `security.disable_natives` must be denied identically by every VM-family
//! execution path (tree-walk, VM, VM `--no-opt`, and `exec` of a compiled
//! container).
//!
//! The VM used to inline `Call len` / `Call string` by checking `n.name`, which
//! bypassed the stub that `stdlib::native_defs()` installs from the resolved
//! config. Tree-walk raised `disabled by configuration` (exit 1) while the VM
//! printed the builtin's result (exit 0) — a policy bypass AND a break of the
//! tree/VM differential contract. The fix gates the inline on the installed
//! function pointer instead of the name.
//!
//! Scope note: the AOT `native` backend is NOT exercised here (the security
//! officer's PoC covered tree / vm / vm --no-opt); native lowering consumes
//! the AST rather than optimizer bytecode, so `tools/native_diff.sh` remains
//! the gate for that path.
//!
//! Every case runs in a fresh child process with an explicit `SPID3R_INI`, so
//! the host's `%APPDATA%` config cannot change the verdict.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_spid3r")
}

fn scratch(tag: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "spid3r_phase2_perf_{}_{}_{}",
        tag,
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create scratch directory");
    dir
}

fn run_in(dir: &Path, ini: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(dir)
        .env("SPID3R_INI", ini)
        .env("SPID3R_CONFIG_DIR", dir.join("config"))
        .env("SPID3R_STATE_DIR", dir.join("state"))
        .output()
        .expect("spawn spid3r")
}

fn write_file(path: &Path, text: &str) {
    std::fs::write(path, text).expect("write file");
}

/// All four variants must agree on exit code and on the presence of the
/// diagnostic. stdout must be identical too (no engine may print a value).
fn assert_uniform(out: &[(&str, Output)], want_code: i32, want_stdout: &str, want_err: &str) {
    for (label, o) in out {
        assert_eq!(
            o.status.code(),
            Some(want_code),
            "{label}: rc differs; stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        let stdout = String::from_utf8_lossy(&o.stdout);
        assert_eq!(
            stdout.trim(),
            want_stdout,
            "{label}: stdout differs; stderr={:?}",
            String::from_utf8_lossy(&o.stderr)
        );
        let stderr = String::from_utf8_lossy(&o.stderr);
        assert!(
            stderr.contains(want_err),
            "{label}: missing '{want_err}' in stderr: {stderr}"
        );
    }
}

/// Run tree / vm / vm --no-opt / exec on `src`, returning labelled outputs.
fn all_engines(dir: &Path, ini: &Path, src: &Path) -> Vec<(&'static str, Output)> {
    let mut out = Vec::new();
    for (label, args) in [
        ("tree", vec!["run", "--engine", "tree"]),
        ("vm", vec!["run", "--engine", "vm"]),
        ("vm --no-opt", vec!["run", "--engine", "vm", "--no-opt"]),
    ] {
        let mut a = args;
        a.push(src.to_str().unwrap());
        out.push((label, run_in(dir, ini, &a)));
    }
    let mc = dir.join("probe.matrixc");
    let compiled = run_in(
        dir,
        ini,
        &["compile", src.to_str().unwrap(), "-o", mc.to_str().unwrap()],
    );
    assert_eq!(
        compiled.status.code(),
        Some(0),
        "compile failed: {:?}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    out.push(("exec", run_in(dir, ini, &["exec", mc.to_str().unwrap()])));
    out
}

#[test]
fn disabled_len_is_denied_by_every_engine() {
    let dir = scratch("disabled-len");
    let ini = dir.join("spid3r.ini");
    let src = dir.join("probe.matrix");
    write_file(&ini, "[security]\ndisable_natives = len\n");
    write_file(
        &src,
        "fn main() {\n    println(len(\"abc\"));\n    return 0;\n}\n",
    );
    let out = all_engines(&dir, &ini, &src);
    assert_uniform(&out, 1, "", "disabled by configuration");
}

#[test]
fn disabled_string_is_denied_by_every_engine() {
    let dir = scratch("disabled-string");
    let ini = dir.join("spid3r.ini");
    let src = dir.join("probe.matrix");
    write_file(&ini, "[security]\ndisable_natives = string\n");
    write_file(
        &src,
        "fn main() {\n    println(string(7));\n    return 0;\n}\n",
    );
    let out = all_engines(&dir, &ini, &src);
    assert_uniform(&out, 1, "", "disabled by configuration");
}

#[test]
fn disabled_string_is_denied_inside_the_fused_prefix_pattern() {
    // Phase 1 fused superinstruction C (`"k" + string(r % 64)`) is a SECOND
    // inlining site, reached from the `Constant` opcode rather than `Call`.
    // It used a name-only check on the live global, so a disabled `string`
    // could still be executed as `int_to_string_value` by the VM while tree
    // raised the policy error. This case would print `86` with rc=0 on the
    // pre-fix binary on the VM engines.
    let dir = scratch("disabled-string-fused");
    let ini = dir.join("spid3r.ini");
    let src = dir.join("probe.matrix");
    write_file(&ini, "[security]\ndisable_natives = string\n");
    write_file(
        &src,
        concat!(
            "fn main() {\n",
            "    let r = 0;\n",
            "    let acc = 0;\n",
            "    while r < 32 {\n",
            "        let key = \"k\" + string(r % 64);\n",
            "        acc = acc + key.len();\n",
            "        r += 1;\n",
            "    }\n",
            "    println(acc);\n",
            "    return 0;\n",
            "}\n",
        ),
    );
    let out = all_engines(&dir, &ini, &src);
    assert_uniform(&out, 1, "", "disabled by configuration");
}

#[test]
fn enabled_len_and_string_still_inline_identically() {
    // Control: with no policy override the inline fast path must still work,
    // and all four engines must print the same value with exit 0.
    let dir = scratch("enabled-control");
    let ini = dir.join("spid3r.ini");
    let src = dir.join("probe.matrix");
    write_file(&ini, "# no policy overrides\n");
    write_file(
        &src,
        "fn main() {\n    println(len(\"abc\"));\n    println(string(7));\n    return 0;\n}\n",
    );
    let out = all_engines(&dir, &ini, &src);
    for (label, o) in &out {
        assert_eq!(
            o.status.code(),
            Some(0),
            "{label}: rc differs; stderr={:?}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&o.stdout).trim(),
            "3\n7",
            "{label}: stdout differs"
        );
    }
}
