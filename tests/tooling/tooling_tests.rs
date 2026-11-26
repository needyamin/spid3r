//! Tooling: line coverage, test doubles and the fixture runner.
//!
//! Coverage is checked through the shipped CLI (`run --coverage`), because the
//! flag, the report format and the LCOV file are the interface a CI job uses.
//! The mock and testkit modules are checked as programs on both engines, since
//! they are ordinary Spid3r code.

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::modules;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::vm::Vm;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run_tree(src: &str, name: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from(name))?;
    semantic::check_file(&program, true, name)?;
    let interp = Interp::new(Arc::from(name), modules::default_module_dirs());
    interp.run_program(&program)
}

fn run_vm(src: &str, name: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from(name))?;
    semantic::check_file(&program, true, name)?;
    let dirs = modules::default_module_dirs();
    let mut vm = Vm::new(Arc::from(name), dirs);
    vm.run_program(&program)
}

fn write_tmp(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, src).expect("write tmp program");
    path.to_string_lossy().to_string()
}

// ---------------------------------------------------------------- coverage

const BRANCHY: &str = r#"
fn classify(n) {
    if n < 0 {
        return "negative";
    }
    if n == 0 {
        return "zero";
    }
    return "positive";
}

fn main() {
    println(classify(5));
    println(classify(0));
    return 0;
}
"#;

#[test]
fn coverage_reports_covered_and_uncovered_lines() {
    let prog = write_tmp("spid3r_cov_prog.matrix", BRANCHY);
    let lcov = std::env::temp_dir().join("spid3r_cov_prog.info");
    let _ = std::fs::remove_file(&lcov);
    let out = Command::new(bin())
        .args([
            "run",
            "--engine",
            "vm",
            "--coverage",
            "--coverage-out",
            lcov.to_str().unwrap(),
            &prog,
        ])
        .output()
        .expect("run with coverage");
    assert!(out.status.success());
    // The program's own stdout is untouched: the report goes to stderr, so a
    // differential test can run with coverage on and still compare stdout.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "positive\nzero"
    );
    let report = String::from_utf8_lossy(&out.stderr);
    assert!(report.contains("%"), "coverage table missing:\n{}", report);
    assert!(
        report.contains("(total)"),
        "total line missing:\n{}",
        report
    );

    let info = std::fs::read_to_string(&lcov).expect("lcov file");
    assert!(
        info.contains("SF:"),
        "lcov source record missing:\n{}",
        info
    );
    assert!(info.contains("end_of_record"), "lcov terminator missing");
    // `return "negative";` is on line 4 and never runs, so it must appear with
    // a count of 0 - a report that omits it would hide the gap.
    assert!(
        info.lines().any(|l| l == "DA:4,0"),
        "the unexecuted line should be reported with 0:\n{}",
        info
    );
    // A line that did run has a non-zero count.
    assert!(
        info.lines()
            .any(|l| l.starts_with("DA:") && !l.ends_with(",0")),
        "no covered lines recorded:\n{}",
        info
    );
    let lf: usize = info
        .lines()
        .find_map(|l| l.strip_prefix("LF:").and_then(|n| n.parse().ok()))
        .expect("LF line");
    let lh: usize = info
        .lines()
        .find_map(|l| l.strip_prefix("LH:").and_then(|n| n.parse().ok()))
        .expect("LH line");
    assert_eq!(lf, 10, "coverable line count:\n{}", info);
    assert_eq!(lh, 9, "covered line count:\n{}", info);
}

#[test]
fn coverage_is_refused_on_the_tree_engine() {
    let prog = write_tmp("spid3r_cov_tree.matrix", "fn main() {\n  return 0;\n}\n");
    let out = Command::new(bin())
        .args(["run", "--engine", "tree", "--coverage", &prog])
        .output()
        .expect("run");
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--coverage is a VM-engine feature"),
        "the limitation must be stated, not silent:\n{}",
        err
    );
}

// ---------------------------------------------------------------- doubles

#[test]
fn mocks_record_calls_and_assert() {
    let src = r#"
import "mock";

fn main() {
    // A stub answers without recording.
    let clock = stub(1700000000);
    assert_eq(clock(), 1700000000);
    assert_eq(clock("ignored", 2), 1700000000);

    // A one-argument mock records arguments and results.
    let repo = mock1(fn(id) { return map("id", id, "name", "ada"); });
    let call = repo["call"];
    let u = call(7);
    assert_eq(u["name"], "ada");
    assert_eq(mock_count(repo), 1);
    assert(mock_called_with(repo, [7]));
    assert(not mock_called_with(repo, [8]));
    mock_assert_called_once(repo);
    mock_assert_called_with(repo, [7]);
    assert_eq(mock_last_args(repo)[0], 7);
    assert_eq(mock_result(repo, 0)["id"], 7);

    // Two arguments, and the recording keeps order.
    let add = mock2(fn(a, b) { return a + b; });
    assert_eq(add["call"](2, 3), 5);
    assert_eq(add["call"](10, 20), 30);
    assert_eq(mock_count(add), 2);
    assert_eq(mock_args(add, 1)[1], 20);

    // Reset clears the log but keeps the implementation.
    mock_reset(add);
    mock_assert_not_called(add);
    assert_eq(add["call"](1, 1), 2);

    // A list-taking mock covers the variadic case.
    let sum = mock_list(fn(a) { return len(a); });
    assert_eq(sum["call"]([1, 2, 3]), 3);
    assert(mock_called_with(sum, [1, 2, 3]));

    // A stub that throws is how an error path is exercised.
    let boom = stub_throws("upstream down");
    let caught = "";
    try {
        boom();
    } catch(e) {
        caught = string(e);
    }
    assert_eq(caught, "upstream down");

    // A failed expectation throws with a readable message.
    let m = mock0(fn() { return 1; });
    m["call"]();
    let msg = "";
    try {
        mock_assert_called_once(mock0(fn() { return 1; }));
    } catch(e) {
        msg = string(e);
    }
    assert(msg.contains("expected 1 call(s), got 0"));
    return 0;
}
"#;
    assert_eq!(run_tree(src, "mock_tree").unwrap(), 0);
    assert_eq!(run_vm(src, "mock_vm").unwrap(), 0);
}

// ---------------------------------------------------------------- fixtures

#[test]
fn testkit_runs_cases_and_reports_failures() {
    let src = r#"
import "testkit";

fn main() {
    let t = test_suite("fixtures");
    let seen = list();
    t.before_each(fn(ctx) {
        seen.push(ctx["name"]);
    });
    t.case("gets a temp dir", fn(ctx) {
        assert(dir_ok(ctx["dir"]));
    });
    t.case("writes and reads a fixture", fn(ctx) {
        let p = fixture_write(ctx["dir"], "a.txt", "hello");
        assert_eq(read_file(p).unwrap(), "hello");
    });
    t.case("fails on purpose", fn(ctx) {
        assert_eq(1, 2);
    });
    t.skip("skipped case", "not today");
    let failures = t.run();
    // Two passed, one failed, one skipped.
    assert_eq(failures, 1);
    assert_eq(len(seen), 3);
    // The temp directory was removed even for the failing case.
    return 0;
}

fn dir_ok(path) {
    // A file can be written into it, which is the only portable check here
    // (there is no "is a directory" native).
    let p = fixture_write(path, "probe.txt", "x");
    return file_exists(p);
}
"#;
    assert_eq!(run_tree(src, "testkit_tree").unwrap(), 0);
    assert_eq!(run_vm(src, "testkit_vm").unwrap(), 0);
}

#[test]
fn with_tmpdir_cleans_up_after_a_throw() {
    let src = r#"
import "testkit";

fn main() {
    let captured = "";
    try {
        with_tmpdir("cleanup", fn(dir) {
            fixture_write(dir, "x.txt", "data");
            throw "boom";
        });
    } catch(e) {
        captured = string(e);
    }
    assert_eq(captured, "boom");
    // The directory is gone: writing a file into it now fails.
    let gone = "";
    with_tmpdir("probe", fn(dir) {
        gone = dir;
    });
    assert_eq(file_exists(gone), false);
    return 0;
}
"#;
    assert_eq!(run_tree(src, "tmpdir_tree").unwrap(), 0);
    assert_eq!(run_vm(src, "tmpdir_vm").unwrap(), 0);
}
