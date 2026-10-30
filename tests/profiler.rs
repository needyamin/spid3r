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
    let (code, _, stderr) = run("prof_tree", &["--prof"], LOOP);
    assert_eq!(code, 2, "usage errors exit 2: {}", stderr);
    assert!(
        stderr.contains("--prof is a VM-engine feature"),
        "stderr: {}",
        stderr
    );
}
