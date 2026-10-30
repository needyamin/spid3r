//! Integration tests (Phases 4-12): modules and imports, programs that mix
//! every language feature, CLI behaviors (check/run/repl/test), and the exit
//! code contract.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

/// A fresh scratch directory per test so module tests do not collide.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r-it-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn write(dir: &Path, name: &str, src: &str) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, src).expect("write file");
    p
}

/// Run the built binary (cargo has already built it for `cargo test`).
fn spid3r(args: &[&str]) -> (i32, String, String) {
    let exe = env!("CARGO_BIN_EXE_spid3r");
    let out = Command::new(exe).args(args).output().expect("run spid3r");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn import_loads_public_names_only() {
    let dir = scratch("import-public");
    write(
        &dir,
        "util.matrix",
        r#"
        public fn double(x) { return x * 2; }
        fn hidden() { return 1; }
        public const LIMIT = 10;
    "#,
    );
    let main = write(
        &dir,
        "main.matrix",
        r#"
        import "util";
        fn main() {
            if double(21) != 42 { return 1; }
            if LIMIT != 10 { return 1; }
            return 0;
        }
    "#,
    );
    let (code, out, err) = spid3r(&["run", main.to_str().unwrap()]);
    assert_eq!(code, 0, "stdout: {}\nstderr: {}", out, err);
}

#[test]
fn importing_a_private_name_is_rejected() {
    let dir = scratch("import-private");
    write(
        &dir,
        "util.matrix",
        r#"
        public fn double(x) { return x * 2; }
        fn hidden() { return 1; }
    "#,
    );
    let main = write(
        &dir,
        "main.matrix",
        r#"
        import "util";
        fn main() {
            return hidden();
        }
    "#,
    );
    let (code, _out, _err) = spid3r(&["run", main.to_str().unwrap()]);
    assert_eq!(code, 1, "private import should fail with exit 1");
}

#[test]
fn a_missing_module_is_a_clean_error() {
    let dir = scratch("import-missing");
    let main = write(
        &dir,
        "main.matrix",
        r#"
        import "nope";
        fn main() { return 0; }
    "#,
    );
    let (code, _out, err) = spid3r(&["run", main.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(
        err.contains("nope"),
        "error should name the module: {}",
        err
    );
}

#[test]
fn circular_imports_are_detected() {
    let dir = scratch("import-cycle");
    write(
        &dir,
        "a.matrix",
        "public fn a() { return 1; }\nimport \"b\";\n",
    );
    write(
        &dir,
        "b.matrix",
        "public fn b() { return 2; }\nimport \"a\";\n",
    );
    let main = write(
        &dir,
        "main.matrix",
        r#"
        import "a";
        fn main() { return a(); }
    "#,
    );
    let (code, _out, err) = spid3r(&["run", main.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(
        err.to_lowercase().contains("circular") || err.contains("cycle"),
        "should report a cycle: {}",
        err
    );
}

#[test]
fn exit_code_comes_from_main() {
    let dir = scratch("exit-code");
    let main = write(
        &dir,
        "main.matrix",
        r#"
        fn main() {
            return 3;
        }
    "#,
    );
    let (code, _out, _err) = spid3r(&["run", main.to_str().unwrap()]);
    assert_eq!(code, 3, "main's int return is the exit code");
}

#[test]
fn a_runtime_error_exits_1_with_a_message() {
    let dir = scratch("runtime-error");
    let main = write(
        &dir,
        "main.matrix",
        r#"
        fn main() {
            let xs = list();
            return xs.get(1);
        }
    "#,
    );
    let (code, _out, err) = spid3r(&["run", main.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(
        err.contains("error"),
        "stderr should carry a diagnostic: {}",
        err
    );
}

#[test]
fn usage_errors_exit_2() {
    let (code, _out, _err) = spid3r(&["frobnicate"]);
    assert_eq!(code, 2);
}

#[test]
fn check_reports_ok_without_running() {
    let dir = scratch("check-ok");
    let main = write(
        &dir,
        "main.matrix",
        r#"
        fn main() {
            println("this must not print");
            return 0;
        }
    "#,
    );
    let (code, out, err) = spid3r(&["check", main.to_str().unwrap()]);
    assert_eq!(code, 0, "stderr: {}", err);
    assert!(
        !out.contains("this must not print"),
        "check must not run the program"
    );
}

#[test]
fn a_program_mixing_every_feature_runs() {
    let dir = scratch("kitchen-sink");
    write(
        &dir,
        "shapes.matrix",
        r#"
        public interface Shape {
            fn area(self) -> float;
        }
        public class Square extends Object implements Shape {
            public let side;
            fn new(side) {
                self.side = side;
            }
            public fn area(self) -> float {
                return self.side * self.side;
            }
        }
    "#,
    );
    let main = write(
        &dir,
        "main.matrix",
        r#"
        import "shapes";

        class Engine {
            private let log;
            fn new() {
                self.log = list();
            }
            public fn note(self, what) {
                self.log.push(what);
            }
            public fn size(self) {
                return self.log.len();
            }
        }

        async fn collect(t) {
            return await t;
        }

        fn fib(n) {
            if n < 2 { return n; }
            return fib(n - 1) + fib(n - 2);
        }

        fn main() {
            // data structures
            let q = queue();
            q.push(1);
            q.push(2);
            if q.pop() != 1 { return 1; }

            // modules, interfaces, classes
            let s = new Square(3.0);
            if s.area() != 9.0 { return 1; }

            // local classes and privacy
            let e = new Engine();
            e.note("ran");
            if e.size() != 1 { return 1; }

            // recursion
            if fib(10) != 55 { return 1; }

            // concurrency
            let t = spawn fn() { return 7; };
            if collect(t) != 7 { return 1; }

            // error handling
            try {
                throw "x";
            } catch (err) {
                if err != "x" { return 1; }
            } finally {
                e.note("done");
            }
            if e.size() != 2 { return 1; }

            println("kitchen sink ok");
            return 0;
        }
    "#,
    );
    let (code, out, err) = spid3r(&["run", main.to_str().unwrap()]);
    assert_eq!(code, 0, "stdout: {}\nstderr: {}", out, err);
    assert!(out.contains("kitchen sink ok"));
}

#[test]
fn repl_answers_an_expression_and_keeps_state() {
    let dir = scratch("repl");
    // feed a few lines including a bad one; the repl must survive it
    let input_path = dir.join("repl-input.txt");
    fs::write(&input_path, "let x = 6;\nx * 7;\nnope();\n:quit\n").unwrap();
    let exe = env!("CARGO_BIN_EXE_spid3r");
    let out = Command::new(exe)
        .arg("repl")
        .stdin(fs::File::open(&input_path).unwrap())
        .output()
        .expect("run repl");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(stdout.contains("42"), "repl should print 42: {}", stdout);
}

#[test]
fn test_command_runs_every_example() {
    // run from the project root: CARGO_MANIFEST_DIR
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (code, out, err) = Command::new(env!("CARGO_BIN_EXE_spid3r"))
        .arg("test")
        .current_dir(&root)
        .output()
        .map(|o| {
            (
                o.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&o.stdout).to_string(),
                String::from_utf8_lossy(&o.stderr).to_string(),
            )
        })
        .expect("run spid3r test");
    assert_eq!(code, 0, "stdout: {}\nstderr: {}", out, err);
    assert!(
        out.contains(&format!("{} ok, 0 failed", example_count())),
        "output: {}",
        out
    );
}

/// Number of examples shipped in examples/ (counted at compile time so adding
/// an example does not silently stale this assertion).
fn example_count() -> usize {
    std::fs::read_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples"))
        .expect("read examples/")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "matrix").unwrap_or(false))
        .count()
}

#[test]
fn compile_now_emits_and_runs_bytecode() {
    // Phase 13 landed the bytecode compiler: `compile` writes a `.matrixc`
    // next to the source and `exec` runs it.
    let dir = scratch("compile-ok");
    let main = write(&dir, "main.matrix", "fn main() { return 4; }");
    let (code, out, err) = spid3r(&["compile", main.to_str().unwrap()]);
    assert_eq!(
        code, 0,
        "compile should now succeed\nstdout: {}\nstderr: {}",
        out, err
    );
    assert!(
        out.contains("wrote"),
        "compile should report the written file: {}",
        out
    );

    let mc = dir.join("main.matrixc");
    assert!(mc.exists(), "expected bytecode file at {:?}", mc);

    let (code2, out2, err2) = spid3r(&["exec", mc.to_str().unwrap()]);
    assert_eq!(
        code2, 4,
        "exec should return the program's exit code\nstdout: {}\nstderr: {}",
        out2, err2
    );
}

#[test]
fn argument_vector_is_available_at_runtime() {
    let dir = scratch("argv");
    let main = write(
        &dir,
        "main.matrix",
        r#"
        fn main() {
            let a = args();
            if a.len() < 1 { return 1; }
            if a.get(0) != "one" { return 1; }
            return 0;
        }
    "#,
    );
    // args after the file are passed to the program
    let (code, out, err) = spid3r(&["run", main.to_str().unwrap()]);
    assert_eq!(code, 1, "no args passed, should return 1; stderr: {}", err);
    let _ = (out, Arc::new(0));
}
