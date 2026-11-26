//! `stdlib/gen.matrix` — generators over the scheduler, tested on BOTH engines.
//!
//! The library is plain Spid3r, so it must behave identically everywhere;
//! the interesting invariants are lazy consumption (an infinite producer is
//! bounded by `take`), composability (map/filter chains), and clean
//! cancellation (no parked tasks left behind — the exit drain would warn).

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run(engine: &str, name: &str, src: &str) -> (i32, String, String) {
    let dir = std::env::temp_dir().join("spid3r_gen");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{}.matrix", name));
    std::fs::write(&path, src).expect("write case");
    let o = Command::new(bin())
        .args(["run", "--engine", engine, path.to_str().unwrap()])
        .env("SPID3R_TASK_DRAIN_MS", "300")
        .output()
        .expect("spawn spid3r");
    let _ = std::fs::remove_file(&path);
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

const FULL: &str = "\
import \"gen\" as g;

fn main() {
    println(g.collect(g.range_gen(5)));
    let pipe = g.map(g.filter(g.range_gen(10), fn(x) { return x % 2 == 0; }),
                     fn(x) { return x * x; });
    println(g.collect(pipe));
    println(g.take(g.range_gen(1000), 3));
    let inf = g.gen(fn(emit) {
        let i = 0;
        while true {
            emit(i);
            i = i + 1;
        }
    });
    println(g.take(inf, 4));
    let abandoned = g.range_gen(1000000);
    println(g.next(abandoned));
    g.close(abandoned);
    let words = g.gen(fn(emit) {
        emit(\"a\");
        emit(\"b\");
        emit(\"c\");
    });
    println(g.next(words));
    println(g.collect(words));
    println(g.collect(g.range_gen(3, 6)));
    return 0;
}
";

const EXPECTED: &str =
    "[0, 1, 2, 3, 4]\n[0, 4, 16, 36, 64]\n[0, 1, 2]\n[0, 1, 2, 3]\n0\na\n[b, c]\n[3, 4, 5]\n";

#[test]
fn generators_work_on_both_engines() {
    for engine in ["vm", "tree"] {
        let (code, stdout, stderr) = run(engine, "gen_full", FULL);
        assert_eq!(code, 0, "[{}] stderr: {}", engine, stderr);
        assert_eq!(stdout, EXPECTED, "[{}]", engine);
    }
}

#[test]
fn close_leaves_no_parked_tasks() {
    // The exit drain warns when tasks are still running: a cancelled
    // generator must unwind its body (throw-based cancel), not skip an
    // emission and keep looping.
    let src = "\
import \"gen\" as g;

fn main() {
    let inf = g.gen(fn(emit) {
        let i = 0;
        while true {
            emit(i);
            i = i + 1;
        }
    });
    println(g.take(inf, 3));
    let also = g.range_gen(1000000);
    println(g.next(also));
    g.close(also);
    println(g.collect(g.range_gen(3)));
    return 0;
}
";
    for engine in ["vm", "tree"] {
        let (code, stdout, stderr) = run(engine, "gen_close", src);
        assert_eq!(code, 0, "[{}] stderr: {}", engine, stderr);
        assert!(stdout.contains("[0, 1, 2]"));
        assert!(
            stdout.contains("[0, 1, 2]"),
            "[{}] stdout: {}",
            engine,
            stdout
        );
        assert!(
            !stderr.contains("still running"),
            "[{}] leaked a parked task: {}",
            engine,
            stderr
        );
    }
}
