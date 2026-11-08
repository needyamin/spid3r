//! Functional tests for the `spid3r run` resource-limit flags, the
//! `spid3r exec --sandbox` gate, and the `spid3r disasm` command.
//!
//! Exit codes: 0 success, 1 program error (compile or runtime), 2 usage
//! error, 4 invalid bytecode.

use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_spid3r")
}

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin()).args(args).output().expect("spawn spid3r");
    let o = (out.status.code().unwrap_or(-1), out);
    let (code, out) = o;
    (
        code,
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// A unique scratch directory per test: the tests run in parallel and must
/// not race on shared files.
fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r_resource_limits_{}", tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn write_in(dir: &std::path::Path, name: &str, src: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, src).expect("write");
    p
}

// ------------------------------------------------------------- --max-steps

const RUNAWAY: &str = r#"fn main() {
    let i = 0;
    while true {
        i = i + 1;
    }
    return 0;
}
"#;

/// `--max-steps` is a documented alias of `--budget`: the same cap, either
/// spelling, on both engines.
#[test]
fn max_steps_is_an_alias_of_budget_on_both_engines() {
    let dir = scratch_dir("alias");
    let p = write_in(&dir, "runaway.matrix", RUNAWAY);
    let path = p.to_str().unwrap().to_string();
    for engine in ["tree", "vm"] {
        for flag in ["--budget", "--max-steps"] {
            let mut args = vec!["run", flag, "500"];
            if engine == "vm" {
                args.push("--engine");
                args.push("vm");
            }
            args.push(&path);
            let (code, _out, err) = run(&args);
            assert_eq!(
                code, 1,
                "[{} {}] the budget is a fatal program error: {err}",
                engine, flag
            );
            assert!(
                err.contains("execution budget exhausted"),
                "[{} {}] expected the budget diagnostic: {err}",
                engine,
                flag
            );
        }
    }
}

#[test]
fn a_small_program_runs_under_a_generous_cap() {
    let dir = scratch_dir("generous");
    let p = write_in(
        &dir,
        "ten_iterations.matrix",
        r#"fn main() {
    let i = 0;
    while i < 10 {
        i = i + 1;
    }
    println(i);
    return 0;
}
"#,
    );
    let path = p.to_str().unwrap().to_string();
    for flag in ["--budget", "--max-steps"] {
        let (code, out, err) = run(&["run", "--engine", "vm", flag, "100000", &path]);
        assert_eq!(code, 0, "[{flag}] {err}");
        assert!(out.contains("10"), "[{flag}] stdout: {out}");
    }
}

#[test]
fn both_step_spellings_are_a_usage_error() {
    let dir = scratch_dir("both_flags");
    let p = write_in(&dir, "ok.matrix", "fn main() {\n    return 0;\n}\n");
    let path = p.to_str().unwrap().to_string();
    let (code, _, err) = run(&["run", "--max-steps", "5", "--budget", "9", &path]);
    assert_eq!(code, 2, "{err}");
    let (code, _, err) = run(&["run", "--max-steps=5", "--budget=9", &path]);
    assert_eq!(code, 2, "{err}");
}

#[test]
fn max_steps_rejects_bad_values_as_usage_errors() {
    let dir = scratch_dir("bad_values");
    let p = write_in(&dir, "ok.matrix", "fn main() {\n    return 0;\n}\n");
    let path = p.to_str().unwrap().to_string();
    let (code, _, err) = run(&["run", "--max-steps", "abc", &path]);
    assert_eq!(code, 2, "{err}");
    let (code, _, err) = run(&["run", "--max-steps", &path]);
    assert_eq!(code, 2, "{err}");
}

// -------------------------------------------------------- --max-call-depth

#[test]
fn max_call_depth_names_the_limit_on_both_engines() {
    let dir = scratch_dir("deep");
    let p = write_in(
        &dir,
        "deep.matrix",
        r#"fn f(n) {
    if n <= 0 { return 0; }
    return f(n - 1);
}
fn main() {
    return f(100);
}
"#,
    );
    let path = p.to_str().unwrap().to_string();
    for engine in ["tree", "vm"] {
        let mut args = vec!["run", "--max-call-depth", "10"];
        if engine == "vm" {
            args.push("--engine");
            args.push("vm");
        }
        args.push(&path);
        let (code, _out, err) = run(&args);
        assert_eq!(
            code, 1,
            "[{}] depth overflow is a program error: {err}",
            engine
        );
        assert!(
            err.contains("stack overflow: call depth limit reached (limit 10)"),
            "[{}] the diagnostic names the limit: {err}",
            engine
        );
    }
}

#[test]
fn default_call_depth_still_reports_the_untouched_message() {
    let dir = scratch_dir("infinite_recursion");
    let p = write_in(
        &dir,
        "infinite_recursion.matrix",
        "fn f() {\n    return f();\n}\nfn main() {\n    return f();\n}\n",
    );
    let path = p.to_str().unwrap().to_string();
    for engine in ["tree", "vm"] {
        let mut args = vec!["run"];
        if engine == "vm" {
            args.push("--engine");
            args.push("vm");
        }
        args.push(&path);
        let (code, _, err) = run(&args);
        assert_eq!(code, 1, "[{engine}] {err}");
        assert!(
            err.contains("stack overflow: call depth limit reached"),
            "[{engine}] {err}"
        );
    }
}

#[test]
fn max_call_depth_rejects_zero_as_a_usage_error() {
    let dir = scratch_dir("depth_zero");
    let p = write_in(&dir, "ok2.matrix", "fn main() {\n    return 0;\n}\n");
    let path = p.to_str().unwrap().to_string();
    let (code, _, err) = run(&["run", "--max-call-depth", "0", &path]);
    assert_eq!(code, 2, "{err}");
}

// --------------------------------------------------------------- --sandbox

#[test]
fn sandbox_blocks_filesystem_on_both_engines_and_stays_catchable() {
    let dir = scratch_dir("read_attempt");
    let p = write_in(
        &dir,
        "read_attempt.matrix",
        r#"fn main() {
    try {
        let s = read_file("Cargo.toml");
        println("leaked");
        return 1;
    } catch e {
        println("caught: " + string(e));
        return 0;
    }
}
"#,
    );
    let path = p.to_str().unwrap().to_string();
    for engine in ["tree", "vm"] {
        let mut args = vec!["run", "--sandbox"];
        if engine == "vm" {
            args.push("--engine");
            args.push("vm");
        }
        args.push(&path);
        let (code, out, err) = run(&args);
        assert_eq!(code, 0, "[{engine}] the denial is catchable: {err}");
        assert!(
            out.contains("caught: sandboxed: filesystem access denied"),
            "[{engine}] stdout: {out}"
        );
    }
}

#[test]
fn sandbox_blocks_network_and_process_access() {
    let dir = scratch_dir("net_attempt");
    let p = write_in(
        &dir,
        "net_attempt.matrix",
        r#"fn main() {
    try {
        tcp_connect("127.0.0.1", 9);
        return 1;
    } catch e {
        println("caught: " + string(e));
        return 0;
    }
}
"#,
    );
    let path = p.to_str().unwrap().to_string();
    let (code, out, err) = run(&["run", "--engine", "vm", "--sandbox", &path]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("caught: sandboxed: network access denied"),
        "stdout: {out}"
    );

    let dir = scratch_dir("proc_attempt");
    let p = write_in(
        &dir,
        "proc_attempt.matrix",
        r#"fn main() {
    try {
        spawn_process("cmd", ["/c", "echo hi"]);
        return 1;
    } catch e {
        println("caught: " + string(e));
        return 0;
    }
}
"#,
    );
    let path = p.to_str().unwrap().to_string();
    let (code, out, err) = run(&["run", "--engine", "vm", "--sandbox", &path]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("caught: sandboxed: process access denied"),
        "stdout: {out}"
    );
}

#[test]
fn sandbox_reaches_into_imported_modules() {
    // The module body installs its own stdlib; the sandbox must gate there
    // too, or an import would re-arm the filesystem.
    let dir = scratch_dir("module");
    std::fs::write(
        dir.join("helper.matrix"),
        "public fn go() {\n    return read_file(\"Cargo.toml\");\n}\n",
    )
    .expect("write module");
    let p = write_in(
        &dir,
        "moduser.matrix",
        r#"import "helper" as h;
fn main() {
    try {
        let s = h.go();
        println("leaked");
        return 1;
    } catch e {
        println("caught: " + string(e));
        return 0;
    }
}
"#,
    );
    let path = p.to_str().unwrap().to_string();
    // Run in the scratch dir so `import "helper"` resolves.
    let out = Command::new(bin())
        .args(["run", "--engine", "vm", "--sandbox", &path])
        .current_dir(&dir)
        .output()
        .expect("spawn spid3r");
    assert_eq!(out.status.code().unwrap_or(-1), 0);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("caught: sandboxed: filesystem access denied"),
        "stdout: {stdout}"
    );
}

#[test]
fn sandbox_applies_to_exec_too() {
    // exec is the untrusted-bytecode distribution path: the same gate must
    // arm there, and the denial must be catchable by the bytecode program.
    let dir = scratch_dir("exec_sandbox");
    let src = write_in(
        &dir,
        "reader.matrix",
        r#"fn main() {
    try {
        let s = read_file("Cargo.toml");
        println("leaked");
        return 1;
    } catch e {
        println("caught: " + string(e));
        return 0;
    }
}
"#,
    );
    let outc = dir.join("reader.matrixc");
    let outc_str = outc.to_str().unwrap().to_string();
    let (code, _, err) = run(&["compile", src.to_str().unwrap(), "-o", &outc_str]);
    assert_eq!(code, 0, "{err}");

    // Without --sandbox the native actually runs: the read succeeds (cargo
    // test's cwd is the package root, so Cargo.toml exists) and the program
    // reports the leak with its own exit code.
    let (code, out, _) = run(&["exec", &outc_str]);
    assert_eq!(code, 1, "the program's own exit code is not the CLI's");
    assert!(out.contains("leaked"), "unsandboxed exec: {out}");
    assert!(!out.contains("sandboxed"), "unsandboxed exec: {out}");

    // With --sandbox the native is denied before it can touch the disk, and
    // the denial is catchable by the bytecode program.
    let (code, out, err) = run(&["exec", "--sandbox", &outc_str]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("caught: sandboxed: filesystem access denied"),
        "stdout: {out}"
    );
}

#[test]
fn sandbox_blocks_environment_access_on_run() {
    let dir = scratch_dir("env_run");
    let p = write_in(
        &dir,
        "env_attempt.matrix",
        r#"fn main() {
    try {
        let v = env("PATH");
        println("leaked env");
        return 1;
    } catch e {
        println("caught: " + string(e));
        return 0;
    }
}
"#,
    );
    let path = p.to_str().unwrap().to_string();
    let (code, out, err) = run(&["run", "--engine", "vm", "--sandbox", &path]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("caught: sandboxed: environment access denied"),
        "stdout: {out}"
    );

    let dir = scratch_dir("env_set_run");
    let p = write_in(
        &dir,
        "env_set_attempt.matrix",
        r#"fn main() {
    try {
        env_set("SPID3R_SANDBOX_PROBE", "1");
        println("mutated env");
        return 1;
    } catch e {
        println("caught: " + string(e));
        return 0;
    }
}
"#,
    );
    let path = p.to_str().unwrap().to_string();
    let (code, out, err) = run(&["run", "--engine", "vm", "--sandbox", &path]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("caught: sandboxed: environment access denied"),
        "stdout: {out}"
    );
}

#[test]
fn sandbox_blocks_environment_access_on_exec() {
    let dir = scratch_dir("env_exec");
    let src = write_in(
        &dir,
        "env_reader.matrix",
        r#"fn main() {
    try {
        let v = env("PATH");
        println("leaked env");
        return 1;
    } catch e {
        println("caught: " + string(e));
        return 0;
    }
}
"#,
    );
    let outc = dir.join("env_reader.matrixc");
    let outc_str = outc.to_str().unwrap().to_string();
    let (code, _, err) = run(&["compile", src.to_str().unwrap(), "-o", &outc_str]);
    assert_eq!(code, 0, "{err}");

    // Without --sandbox the environment is readable (the program reports it
    // with its own exit code).
    let (code, out, _) = run(&["exec", &outc_str]);
    assert_eq!(code, 1, "the program's own exit code is not the CLI's");
    assert!(out.contains("leaked env"), "unsandboxed exec: {out}");

    // With --sandbox the read is denied before it reaches the host.
    let (code, out, err) = run(&["exec", "--sandbox", &outc_str]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("caught: sandboxed: environment access denied"),
        "stdout: {out}"
    );
}

// -------------------------------------------------------------- disasm

#[test]
fn disasm_prints_a_readable_disassembly() {
    let dir = scratch_dir("disasm_ok");
    let p = write_in(&dir, "tiny.matrix", "fn main() {\n    return 0;\n}\n");
    let path = p.to_str().unwrap().to_string();
    let out_path = dir.join("tiny.matrixc");
    let outc = out_path.to_str().unwrap().to_string();
    let (code, _, err) = run(&["compile", &path, "-o", &outc]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = run(&["disasm", &outc]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("CONSTANT") || out.contains("Return"),
        "the disassembly lists opcodes: {out}"
    );
}

#[test]
fn disasm_exits_4_on_a_corrupt_bytecode_file() {
    let dir = scratch_dir("disasm_corrupt");
    let p = dir.join("corrupt.matrixc");
    std::fs::write(&p, b"this is not bytecode").expect("write");
    let path = p.to_str().unwrap().to_string();
    let (code, _, err) = run(&["disasm", &path]);
    assert_eq!(code, 4, "{err}");
    assert!(err.contains("error[vm]"), "{err}");
}

#[test]
fn disasm_usage_errors_exit_2() {
    let (code, _, _) = run(&["disasm"]);
    assert_eq!(code, 2);
    let (code, _, _) = run(&["disasm", "missing-file.matrixc"]);
    assert_eq!(code, 2);
}

#[test]
fn exec_also_exits_4_on_a_corrupt_bytecode_file() {
    let dir = scratch_dir("exec_corrupt");
    let p = dir.join("corrupt2.matrixc");
    std::fs::write(&p, b"truncated").expect("write");
    let path = p.to_str().unwrap().to_string();
    let (code, _, _) = run(&["exec", &path]);
    assert_eq!(code, 4);
}

// ------------------------------------------------- exit-code contract

#[test]
fn exit_code_contract() {
    let dir = scratch_dir("contract");

    // 0 — success.
    let p = write_in(&dir, "contract_ok.matrix", "fn main() {\n    return 0;\n}\n");
    let (code, _, _) = run(&["run", p.to_str().unwrap()]);
    assert_eq!(code, 0);

    // 1 — program error (compile failure or runtime failure).
    let p = write_in(
        &dir,
        "contract_broken.matrix",
        "fn main() {\n    let x = ;\n    return 0;\n}\n",
    );
    let (code, _, _) = run(&["run", p.to_str().unwrap()]);
    assert_eq!(code, 1);

    let p = write_in(&dir, "contract_throw.matrix", "fn main() {\n    throw \"boom\";\n}\n");
    let (code, _, _) = run(&["run", p.to_str().unwrap()]);
    assert_eq!(code, 1);

    // 2 — usage error.
    let (code, _, _) = run(&["run", "--bogus-flag"]);
    assert_eq!(code, 2);

    // 4 — invalid bytecode.
    let p = dir.join("contract_bad.matrixc");
    std::fs::write(&p, b"junk").expect("write");
    let (code, _, _) = run(&["disasm", p.to_str().unwrap()]);
    assert_eq!(code, 4);
}
