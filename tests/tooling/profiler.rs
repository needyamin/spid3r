//! `spid3r run --prof` — the VM opcode profiler.
//!
//! The profile goes to stderr (stdout stays byte-identical to a normal run),
//! so a program's output is unaffected while the report travels alongside it.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

const LOOP: &str = "fn main() {\n    let s = 0;\n    let i = 0;\n    while i < 1000 {\n        s = s + i;\n        i = i + 1;\n    }\n    println(s);\n    return 0;\n}\n";

fn run(name: &str, args: &[&str], src: &str) -> (i32, String, String) {
    let dir = std::env::temp_dir().join("spid3r_prof");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{}.matrix", name));
    std::fs::write(&path, src).expect("write case");
    let mut a = vec!["run"];
    a.extend_from_slice(args);
    a.push(path.to_str().unwrap());
    let o = Command::new(bin())
        .args(&a)
        .env_remove("SPID3R_MAX_WORKERS")
        .output()
        .expect("spawn spid3r");
    let _ = std::fs::remove_file(&path);
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

#[test]
fn prof_reports_opcode_counts_on_stderr() {
    let (code, stdout, stderr) = run("prof_on", &["--engine", "vm", "--prof"], LOOP);
    assert_eq!(code, 0, "stderr: {}", stderr);
    // Program output is untouched.
    assert_eq!(stdout.trim(), "499500");
    assert!(
        stderr.contains("==== vm profile ===="),
        "stderr: {}",
        stderr
    );
    assert!(stderr.contains("instructions:"), "stderr: {}", stderr);
    // A 1000-iteration loop: the loop-condition local reads dominate.
    assert!(stderr.contains("get_local"), "stderr: {}", stderr);
    // Parse "instructions: N" and verify the sum of opcode counts matches it
    // exactly — the report must account for every executed instruction.
    let total: u64 = stderr
        .lines()
        .find_map(|l| l.strip_prefix("instructions: ")?.trim().parse().ok())
        .expect("an instructions line");
    let counted: u64 = stderr
        .lines()
        .filter_map(|l| {
            let f = l.split_whitespace().next()?;
            f.parse::<u64>().ok()
        })
        .sum();
    assert_eq!(counted, total, "opcode counts must sum to the total");
    assert!(total > 3000, "a 1000-iteration loop executes thousands");
}

#[test]
fn without_prof_stderr_stays_clean() {
    let (code, stdout, stderr) = run("prof_off", &["--engine", "vm"], LOOP);
    assert_eq!(code, 0);
    assert_eq!(stdout.trim(), "499500");
    assert!(
        !stderr.contains("vm profile"),
        "no report without --prof: {}",
        stderr
    );
}

#[test]
fn prof_with_the_tree_engine_is_a_usage_error() {
    // Explicit `--engine tree`: the VM is the default engine (D4.1), so an
    // unqualified `--prof` runs on the VM and is no longer a usage error.
    let (code, _, stderr) = run("prof_tree", &["--engine", "tree", "--prof"], LOOP);
    assert_eq!(code, 2, "usage errors exit 2: {}", stderr);
    assert!(
        stderr.contains("--prof is a VM-engine feature"),
        "stderr: {}",
        stderr
    );
}

#[test]
fn prof_attributes_per_function_self_instructions_summing_to_total() {
    let src = "\
fn leaf(x) {
    return x * 2 + 1;
}
fn work(n) {
    let s = 0;
    let i = 0;
    while i < n {
        s = s + leaf(i);
        i = i + 1;
    }
    return s;
}
fn main() {
    println(work(25));
    return 0;
}
";
    let (code, stdout, stderr) = run("prof_funcs", &["--engine", "vm", "--prof"], src);
    assert_eq!(code, 0, "stderr: {}", stderr);
    assert_eq!(stdout.trim(), "625");
    assert!(
        stderr.contains("==== function profile (self-time) ===="),
        "missing function profile section:\n{}",
        stderr
    );
    assert!(stderr.contains("fn leaf"), "stderr:\n{}", stderr);
    assert!(stderr.contains("fn work"), "stderr:\n{}", stderr);

    let total: u64 = stderr
        .lines()
        .find_map(|l| l.strip_prefix("instructions: ")?.trim().parse().ok())
        .expect("instructions total");

    let op_sum: u64 = stderr
        .lines()
        .filter_map(|l| l.split_whitespace().next()?.parse::<u64>().ok())
        .sum();
    assert_eq!(op_sum, total, "opcode counts must sum to total");

    let mut fn_self_sum: u64 = 0;
    let mut leaf_calls: Option<u64> = None;
    let mut work_calls: Option<u64> = None;
    for line in stderr.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("fn ") {
            let fn_name = rest.split_whitespace().next().unwrap_or("");
            if let Some(after_calls) = rest.split("calls:").nth(1) {
                let calls: u64 = after_calls
                    .split_whitespace()
                    .next()
                    .and_then(|t| t.parse().ok())
                    .unwrap_or(0);
                if fn_name == "leaf" {
                    leaf_calls = Some(calls);
                } else if fn_name == "work" {
                    work_calls = Some(calls);
                }
            }
            if let Some(after_self) = rest.split("self_insts:").nth(1) {
                let si: u64 = after_self
                    .split_whitespace()
                    .next()
                    .and_then(|t| t.parse().ok())
                    .expect("parse self_insts");
                fn_self_sum += si;
            }
        }
    }
    assert_eq!(leaf_calls, Some(25), "stderr:\n{}", stderr);
    assert_eq!(work_calls, Some(1), "stderr:\n{}", stderr);
    assert_eq!(
        fn_self_sum, total,
        "sum of per-function self_insts ({}) must equal total instructions ({})",
        fn_self_sum, total
    );
}

#[test]
fn repl_non_tty_piped_session_evaluates_resets_and_quits() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new(env!("CARGO_BIN_EXE_spid3r"))
        .arg("repl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn repl");
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin
            .write_all(b"let x = 10;\nx + 5;\n:reset\n:quit\n")
            .unwrap();
    }
    let out = child.wait_with_output().expect("wait repl");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "repl failed: stderr={stderr}");
    assert!(stdout.contains("spid3r repl"), "{stdout}");
    assert!(stdout.contains("15"), "{stdout}");
    assert!(stdout.contains("bindings cleared"), "{stdout}");
}
