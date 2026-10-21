//! Stage-2 concurrency regressions (assessment/AUDIT_2026-10-20.md §6).
//!
//! Every test here runs a real `spid3r run` child process under a watchdog and
//! asserts on its exit code and output. A child that does not finish inside the
//! deadline is killed and the test fails with `timed out (possible deadlock)` —
//! so a reintroduced mutex-re-entry hang, a starved worker pool, or a lost
//! task output fails the suite instead of hanging CI forever.
//!
//! Coverage map:
//!   A-7  self-referential container methods must not re-lock the receiver
//!   A-3  grow-on-demand workers (blocked tasks > pool) + join on program exit
//!   A-4  `gc_collect()` refuses to sweep while tasks are live
//!   A-9  thrown values cross spawn/await/join/callback seams structurally
//!
//! Both engines are exercised: the tree interpreter and the bytecode VM share
//! `value::collections`, so a value-layer deadlock hangs both, but the
//! task/callback seams are engine-specific.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

impl Outcome {
    fn stdout_lines(&self) -> Vec<&str> {
        self.stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect()
    }
}

fn write_program(src: &str) -> std::path::PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("spid3r_stage2_{}_{}.matrix", std::process::id(), n));
    std::fs::write(&path, src).expect("write program");
    path
}

fn run_with(src: &str, engine: &str, timeout: Duration, envs: &[(&str, &str)]) -> Outcome {
    let path = write_program(src);
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spid3r"));
    cmd.args(["run", "--engine", engine, path.to_str().unwrap()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn spid3r");

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                timed_out = true;
                break;
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }

    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    let status = child.wait().expect("wait");
    let _ = std::fs::remove_file(&path);
    Outcome {
        code: status.code(),
        stdout,
        stderr,
        timed_out,
    }
}

fn run(src: &str, engine: &str) -> Outcome {
    run_with(src, engine, DEFAULT_TIMEOUT, &[])
}

/// Run `src` on both engines and require a clean exit with exactly `expected`
/// stdout lines (trimmed, blank lines dropped).
fn assert_both_engines(case: &str, src: &str, expected: &[&str]) {
    for engine in ["tree", "vm"] {
        let out = run(src, engine);
        if out.timed_out {
            panic!(
                "{} [{}]: timed out after {:?} (possible deadlock)\nstderr: {}",
                case, engine, DEFAULT_TIMEOUT, out.stderr
            );
        }
        assert_eq!(
            out.code,
            Some(0),
            "{} [{}]: non-zero exit\nstdout: {}\nstderr: {}",
            case,
            engine,
            out.stdout,
            out.stderr
        );
        assert_eq!(
            out.stdout_lines(),
            expected,
            "{} [{}]: unexpected output\nfull stdout: {}\nstderr: {}",
            case,
            engine,
            out.stdout,
            out.stderr
        );
    }
}

// --------------------------------------------------------------------- A-7

/// Self-referential container methods: the receiver's own mutex must never be
/// re-locked while comparing/deep-cloning its items. Before the fix these hung
/// forever (no error, no timeout) on both engines.
#[test]
fn self_referential_container_methods_terminate() {
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "list.slice",
            r#"
fn main() {
    let l = list();
    l.push(l);
    println(l.slice(0, 1).len());
    return 0;
}
"#,
            &["1"],
        ),
        (
            "list.contains/index_of/remove",
            r#"
fn main() {
    let l = list();
    l.push(l);
    println(l.contains(l));
    println(l.index_of(l));
    l.remove(l);
    println(l.len());
    return 0;
}
"#,
            &["true", "0", "0"],
        ),
        (
            "stack.to_list",
            r#"
fn main() {
    let s = stack();
    s.push(s);
    println(s.to_list().len());
    println(s.contains(s));
    return 0;
}
"#,
            &["1", "true"],
        ),
        (
            "queue.to_list",
            r#"
fn main() {
    let q = queue();
    q.push(q);
    println(q.to_list().len());
    println(q.contains(q));
    return 0;
}
"#,
            &["1", "true"],
        ),
        (
            "deque.to_list",
            r#"
fn main() {
    let d = deque();
    d.push(d);
    println(d.to_list().len());
    println(d.contains(d));
    return 0;
}
"#,
            &["1", "true"],
        ),
        (
            "ring_buffer.contains",
            r#"
fn main() {
    let r = ring_buffer(4);
    r.push(r);
    println(r.contains(r));
    return 0;
}
"#,
            &["true"],
        ),
        (
            "set.to_list/contains/remove",
            r#"
fn main() {
    let s = set();
    s.add(s);
    println(s.to_list().len());
    println(s.contains(s));
    s.remove(s);
    println(s.len());
    return 0;
}
"#,
            &["1", "true", "0"],
        ),
        (
            "bag.count_of/contains/remove",
            r#"
fn main() {
    let b = bag();
    b.add(b);
    println(b.count_of(b));
    println(b.contains(b));
    b.remove(b);
    println(b.count_of(b));
    return 0;
}
"#,
            &["1", "true", "0"],
        ),
        (
            "graph.nodes (deep clone)",
            r#"
fn main() {
    let g = graph("directed");
    g.add_node(g);
    println(g.nodes().len());
    println(g.has_node(g));
    return 0;
}
"#,
            &["1", "true"],
        ),
        (
            "graph.clone (deep clone)",
            // A-14: the clone is cyclic — its node is the clone itself, not the
            // original graph. `h.has_node(g)` used to be true precisely because
            // the clone aliased `g`.
            r#"
fn main() {
    let g = graph("directed");
    g.add_node(g);
    let h = g.clone();
    println(h.len());
    println(h.has_node(h));
    println(h.has_node(g));
    return 0;
}
"#,
            &["1", "true", "false"],
        ),
        (
            "graph traversal",
            r#"
fn main() {
    let g = graph("directed");
    g.add_node(g);
    g.add_edge(g, g, 2.0);
    println(g.bfs(g).len());
    println(g.dfs(g).len());
    println(g.neighbors(g).len());
    println(g.path_exists(g, g));
    println(g.find_path(g, g).is_some());
    println(g.edge_count());
    return 0;
}
"#,
            &["1", "1", "1", "true", "true", "1"],
        ),
        (
            "binary_tree traversals",
            r#"
fn main() {
    let t = binary_tree();
    t.insert(t);
    println(t.inorder().len());
    println(t.preorder().len());
    println(t.postorder().len());
    println(t.level_order().len());
    return 0;
}
"#,
            &["1", "1", "1", "1"],
        ),
        (
            "linked_list.to_list/contains",
            r#"
fn main() {
    let l = linked_list();
    l.push_back(l);
    println(l.to_list().len());
    println(l.contains(l));
    return 0;
}
"#,
            &["1", "true"],
        ),
        (
            "map.values/items",
            r#"
fn main() {
    let m = map();
    m.set("k", m);
    println(m.values().len());
    println(m.items().len());
    return 0;
}
"#,
            &["1", "1"],
        ),
        (
            "list.append self",
            r#"
fn main() {
    let l = list();
    l.push(1);
    l.append(l);
    println(l.len());
    return 0;
}
"#,
            &["2"],
        ),
        (
            "set.union self",
            r#"
fn main() {
    let s = set();
    s.add(1);
    println(s.union(s).len());
    return 0;
}
"#,
            &["1"],
        ),
    ];

    for (name, src, expected) in cases {
        assert_both_engines(name, src, expected);
    }
}

// --------------------------------------------------------------------- A-3

/// More concurrently blocked tasks than pool workers: the queued work must
/// still run. Before grow-on-demand this deadlocked permanently (every worker
/// parked in `recv`, the spawned task never scheduled).
#[test]
fn grow_on_demand_runs_work_queued_behind_blocked_tasks() {
    let pool = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    // Enough blocked tasks to occupy every base worker, plus a margin so the
    // last few jobs are queued behind parked ones.
    let blocked = pool + 6;
    let src = format!(
        r#"
async fn joiner(t) {{
    return await t;
}}

fn main() {{
    let ch = channel();
    let n = {blocked};
    let i = 0;
    while (i < n) {{
        spawn fn() {{
            ch.recv();
        }};
        i = i + 1;
    }}
    sleep_ms(300);
    let t = spawn fn() {{
        return 42;
    }};
    println(joiner(t));
    let j = 0;
    while (j < n) {{
        ch.send(0);
        j = j + 1;
    }}
    return 0;
}}
"#
    );
    assert_both_engines("grow-on-demand", &src, &["42"]);
}

/// A task that outlives `main` must finish before the process exits, so its
/// output is not lost to teardown. Before the fix the process exited while the
/// task was still sleeping and "task-output" never appeared.
#[test]
fn program_exit_waits_for_running_tasks() {
    let src = r#"
fn main() {
    spawn fn() {
        sleep_ms(300);
        println("task-output");
    };
    println("main-output");
    return 0;
}
"#;
    for engine in ["tree", "vm"] {
        let out = run_with(
            src,
            engine,
            DEFAULT_TIMEOUT,
            &[("SPID3R_TASK_DRAIN_MS", "5000")],
        );
        assert!(!out.timed_out, "[{}] exit drain timed out", engine);
        assert_eq!(out.code, Some(0), "[{}] stderr: {}", engine, out.stderr);
        assert_eq!(
            out.stdout_lines(),
            vec!["main-output", "task-output"],
            "[{}] task output lost at exit",
            engine
        );
    }
}

/// Tasks that are blocked forever must not hang the process: exit waits for the
/// configured deadline, then reports what is still running and exits cleanly.
#[test]
fn exit_reports_tasks_that_never_finish() {
    let src = r#"
fn main() {
    let ch = channel();
    spawn fn() {
        ch.recv();
    };
    sleep_ms(50);
    println("done");
    return 0;
}
"#;
    for engine in ["tree", "vm"] {
        let started = Instant::now();
        let out = run_with(
            src,
            engine,
            DEFAULT_TIMEOUT,
            &[("SPID3R_TASK_DRAIN_MS", "300")],
        );
        let elapsed = started.elapsed();
        assert!(!out.timed_out, "[{}] stuck task hung the process", engine);
        assert_eq!(out.code, Some(0), "[{}] stderr: {}", engine, out.stderr);
        assert_eq!(out.stdout_lines(), vec!["done"], "[{}]", engine);
        assert!(
            out.stderr.contains("still running"),
            "[{}] expected a warning about stuck tasks, got {:?}",
            engine,
            out.stderr
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "[{}] exit took {:?}, the drain deadline was not honored",
            engine,
            elapsed
        );
    }
}

// --------------------------------------------------------------------- A-4

/// The GC gate is *precise*, not a blanket "no tasks" rule: a collection is
/// refused only while another thread is actually executing Spid3r code, because
/// that is when it could move a value between the strong-count read and the mark
/// phase. A task parked in `recv`/`sleep`/`join`/`mutex` cannot mutate anything,
/// so it must not block the collector (it holds its values on its own stack, and
/// those are external references the sweep treats as roots).
#[test]
fn gc_collect_runs_while_tasks_are_parked_and_refuses_while_they_run() {
    let src = r#"
async fn wait(t) {
    return await t;
}
fn main() {
    // 1. A task parked in recv: collecting is safe.
    let ch = channel();
    let blocked = spawn fn() {
        ch.recv();
        return 1;
    };
    sleep_ms(150);
    try {
        println(gc_collect() >= 0);
    } catch(e) {
        println("refused-while-parked");
    }
    ch.send(1);
    let v = wait(blocked);

    // 2. A task actively running: collecting would race it. The task stays
    //    busy on the clock, not on an iteration count, so this holds in debug
    //    and release builds alike.
    let busy = spawn fn() {
        let t0 = now();
        while (now() - t0 < 1500) {
        }
        return 1;
    };
    sleep_ms(100);
    try {
        gc_collect();
        println("swept-while-running");
    } catch(e) {
        println("refused-while-running");
        println(e.contains("gc_collect"));
    }
    let w = wait(busy);

    // 3. Nothing running: collecting works again.
    println(gc_collect() >= 0);
    return 0;
}
"#;
    assert_both_engines(
        "gc gate",
        src,
        &["true", "refused-while-running", "true", "true"],
    );
}

/// A task must be able to collect while every other thread is parked: the old
/// rule (`live_tasks() == 0`) made `gc_collect()` fail inside every task, even
/// when it was the only thread running.
#[test]
fn a_task_can_collect_when_no_other_thread_is_running() {
    let src = r#"
fn main() {
    let t = spawn fn() {
        // Collect succeeds only when no other thread is executing Spid3r code
        // (every other thread is parked in a blocking wait like `join`). Under
        // heavy multi-threaded test load, retry until the main thread has
        // entered `join`.
        let collected = -1;
        let attempts = 0;
        while (collected < 0 and attempts < 100) {
            try {
                collected = gc_collect();
            } catch(e) {
                attempts = attempts + 1;
                sleep_ms(20);
            }
        }
        return collected >= 0;
    };
    println(t.join());
    return 0;
}
"#;
    assert_both_engines("gc from task", src, &["true"]);
}

// --------------------------------------------------------------------- A-9

/// Thrown values must cross every task/callback seam as values, not display
/// strings: `catch` rebinds the original map/object so its fields are usable.
#[test]
fn thrown_values_cross_seams_structurally() {
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "await",
            r#"
async fn joiner(t) {
    return await t;
}
fn main() {
    let t = spawn fn() {
        let o = map();
        o.set("code", 7);
        o.set("why", "bad");
        throw o;
    };
    try {
        let v = joiner(t);
        println("no-throw");
    } catch(e) {
        println(type_of(e));
        println(e["code"]);
        println(e["why"]);
    }
    return 0;
}
"#,
            &["map", "7", "bad"],
        ),
        (
            "task.join",
            r#"
fn main() {
    let t = spawn fn() {
        let o = map();
        o.set("code", 9);
        throw o;
    };
    sleep_ms(50);
    try {
        let v = t.join();
        println("no-throw");
    } catch(e) {
        println(type_of(e));
        println(e["code"]);
    }
    return 0;
}
"#,
            &["map", "9"],
        ),
        (
            "callback",
            r#"
fn main() {
    let l = [1, 2, 3];
    try {
        let m = l.map(fn(x) {
            let o = map();
            o.set("code", 11);
            throw o;
        });
        println("no-throw");
    } catch(e) {
        println(type_of(e));
        println(e["code"]);
    }
    return 0;
}
"#,
            &["map", "11"],
        ),
        (
            "nested callback",
            r#"
fn main() {
    let outer = [1];
    try {
        let m = outer.map(fn(x) {
            let inner = [2];
            return inner.map(fn(y) {
                let o = map();
                o.set("code", 13);
                throw o;
            });
        });
        println("no-throw");
    } catch(e) {
        println(type_of(e));
        println(e["code"]);
    }
    return 0;
}
"#,
            &["map", "13"],
        ),
        (
            "comparator",
            r#"
fn main() {
    let l = [3, 1, 2];
    try {
        l.sort_by(fn(a, b) {
            let o = map();
            o.set("code", 17);
            throw o;
        });
        println("no-throw");
    } catch(e) {
        println(type_of(e));
        println(e["code"]);
    }
    return 0;
}
"#,
            &["map", "17"],
        ),
        (
            "string throw keeps its text",
            r#"
async fn joiner(t) {
    return await t;
}
fn main() {
    let t = spawn fn() {
        throw "plain";
    };
    try {
        let v = joiner(t);
        println("no-throw");
    } catch(e) {
        println(type_of(e));
        println(e);
    }
    return 0;
}
"#,
            &["string", "plain"],
        ),
    ];

    for (name, src, expected) in cases {
        assert_both_engines(name, src, expected);
    }
}

// ------------------------------------------------ scheduler limits (A-3/A-7)

/// The ready queue is bounded: a spawn storm is paced by backpressure instead of
/// growing memory without limit, and every task still runs exactly once. The cap
/// is lowered through the environment so the pressure is real, not theoretical.
#[test]
fn bounded_queue_paces_a_spawn_storm_without_losing_work() {
    let src = r#"
async fn joiner(t) {
    return await t;
}
fn main() {
    let ts = list();
    let i = 0;
    while (i < 400) {
        ts.push(spawn fn() {
            return 1;
        });
        i = i + 1;
    }
    let sum = 0;
    let j = 0;
    while (j < 400) {
        sum = sum + joiner(ts[j]);
        j = j + 1;
    }
    println(sum);
    return 0;
}
"#;
    for engine in ["tree", "vm"] {
        let out = run_with(src, engine, DEFAULT_TIMEOUT, &[("SPID3R_QUEUE_CAP", "4")]);
        assert!(!out.timed_out, "[{}] bounded queue deadlocked", engine);
        assert_eq!(out.code, Some(0), "[{}] stderr: {}", engine, out.stderr);
        assert_eq!(out.stdout_lines(), vec!["400"], "[{}]", engine);
    }
}

/// Concurrent `set.add` of the same value must leave one element: the
/// snapshot-then-commit is a verified compare-and-swap, so two tasks cannot both
/// observe "absent" and push duplicates.
#[test]
fn concurrent_set_adds_do_not_duplicate() {
    let src = r#"
fn main() {
    let s = set();
    let i = 0;
    while (i < 8) {
        spawn fn() {
            let j = 0;
            while (j < 200) {
                s.add("x");
                j = j + 1;
            }
        };
        i = i + 1;
    }
    sleep_ms(800);
    println(s.len());
    return 0;
}
"#;
    assert_both_engines("set add race", src, &["1"]);
}

/// Concurrent `bag.add` of the same value must count every add exactly once:
/// the per-entry commit is verified against the snapshot, so no add is lost and
/// no duplicate entry is created.
#[test]
fn concurrent_bag_adds_keep_an_exact_count() {
    let src = r#"
fn main() {
    let b = bag();
    let i = 0;
    while (i < 4) {
        spawn fn() {
            let j = 0;
            while (j < 50) {
                b.add("x");
                j = j + 1;
            }
        };
        i = i + 1;
    }
    sleep_ms(800);
    println(b.count_of("x"));
    println(b.len());
    return 0;
}
"#;
    assert_both_engines("bag add race", src, &["200", "200"]);
}

/// At the worker ceiling, queued work whose workers are all parked can never
/// run. Instead of hanging silently the watchdog reports it on stderr with an
/// actionable message; the program still completes once the tasks are released.
#[test]
fn a_stalled_pool_is_reported_instead_of_hanging_silently() {
    let src = r#"
fn main() {
    let ch = channel();
    let i = 0;
    while (i < 3) {
        spawn fn() {
            ch.recv();
        };
        i = i + 1;
    }
    sleep_ms(4200);
    let j = 0;
    while (j < 3) {
        ch.send(0);
        j = j + 1;
    }
    println("released");
    return 0;
}
"#;
    for engine in ["tree", "vm"] {
        let out = run_with(src, engine, DEFAULT_TIMEOUT, &[("SPID3R_MAX_WORKERS", "2")]);
        assert!(!out.timed_out, "[{}] stall test timed out", engine);
        assert_eq!(out.code, Some(0), "[{}] stderr: {}", engine, out.stderr);
        assert_eq!(out.stdout_lines(), vec!["released"], "[{}]", engine);
        assert!(
            out.stderr.contains("queued but all"),
            "[{}] expected a stall warning, got {:?}",
            engine,
            out.stderr
        );
        assert!(
            out.stderr.contains("SPID3R_MAX_WORKERS"),
            "[{}] the stall warning must say how to fix it, got {:?}",
            engine,
            out.stderr
        );
    }
}
