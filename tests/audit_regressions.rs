//! Regression tests added by the 2026-10 audit.
//!
//! Each test pins behavior that was broken and is now fixed; every Spid3r
//! program runs on BOTH engines (tree reference + bytecode VM) and both must
//! produce identical stdout.
//!
//! Covered here:
//! * VM closure/upvalue fixes (per-activation cells, per-iteration loop-var
//!   capture, captured-slot cleanup, enclosing-upvalue index offset)
//! * `map[k] = v` index assignment (documented, previously unsupported)
//! * `read_file`/`write_file` returning `ok(...)`/`err(...)` (documented)
//! * `task.status()` / repeatable `task.join()` (documented) and thrown-value
//!   fidelity across the task boundary

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

/// Write `src` to a temp `.matrix` file and run it on `engine`.
fn run_src(engine: &str, name: &str, src: &str) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!("spid3r_audit_{}.matrix", name));
    std::fs::write(&path, src).map_err(|e| format!("write {}: {}", path.display(), e))?;
    let o = Command::new(bin())
        .args(["run", "--engine", engine, path.to_str().unwrap()])
        .output()
        .map_err(|e| format!("spawn failed: {}", e))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(format!(
            "{} exit {}: {}",
            engine,
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stderr)
        ))
    }
}

/// Assert a program produces `expected` identically on tree and vm.
fn assert_both_engines(name: &str, src: &str, expected: &str) {
    let tree = run_src("tree", name, src)
        .unwrap_or_else(|e| panic!("[{}] tree run failed:\n{}", name, e));
    let vm = run_src("vm", name, src)
        .unwrap_or_else(|e| panic!("[{}] vm run failed:\n{}", name, e));
    assert_eq!(tree, expected, "[{}] tree output", name);
    assert_eq!(vm, expected, "[{}] vm output (diverged from tree)", name);
}

// ------------------------------------------------------------- VM closures

#[test]
fn closure_cells_are_per_activation_not_per_function() {
    // Was: the second make_counter() call shared the first call's captured
    // cell, so `b()` continued `a`'s counter (3 then 4 instead of 3 then 1).
    assert_both_engines(
        "activation",
        r#"fn make_counter() {
    let c = 0;
    let inc = fn() { c = c + 1; return c; };
    return inc;
}
fn main() {
    let a = make_counter();
    let b = make_counter();
    a(); a();
    println(a());
    println(b());
    return 0;
}
"#,
        "3\n1\n",
    );
}

#[test]
fn forin_loop_var_is_captured_fresh_per_iteration() {
    // Was: all closures created in a for-in body shared one cell and saw the
    // final value (2, 2, 2) instead of each iteration's value.
    assert_both_engines(
        "forin_capture",
        r#"fn main() {
    let fns = list();
    for x in[0, 1, 2] {
        fns.push(fn() { return x; });
    }
    println(fns[0]());
    println(fns[1]());
    println(fns[2]());
    return 0;
}
"#,
        "0\n1\n2\n",
    );
}

#[test]
fn while_loop_captures_the_single_outer_binding() {
    // `i` lives in the enclosing scope: every closure observes the final
    // value. (Per-iteration capture would be wrong here.)
    assert_both_engines(
        "while_capture",
        r#"fn main() {
    let ws = list();
    let i = 0;
    while i < 3 {
        ws.push(fn() { return i; });
        i = i + 1;
    }
    println(ws[0]());
    println(ws[2]());
    return 0;
}
"#,
        "3\n3\n",
    );
}

#[test]
fn captured_slot_is_released_when_scope_ends() {
    // Was: captured-slot routing survived the loop, so later locals that
    // reused the slot index silently read/wrote the stale cell (up to
    // "int has no length" crashes in mixed programs).
    assert_both_engines(
        "slot_reuse",
        r#"fn main() {
    let acc = list();
    for i in[1, 2, 3] {
        acc.push(fn() { return i * i; });
    }
    let t = 0;
    acc.for_each(fn(g) { t = t + g(); });
    println(t);
    let fresh = 7;
    println(fresh);
    return 0;
}
"#,
        "14\n7\n",
    );
}

#[test]
fn enclosing_upvalue_indices_do_not_collide_with_own_captured_locals() {
    // Was: `resolve_upvalue` ignored `own_upvalues`, so a function that both
    // captured an enclosing variable and had a local captured by a nested
    // function aliased the two (r + g read r's cell twice: 2 instead of 6).
    assert_both_engines(
        "offset",
        r#"fn outer_fn() {
    let g = 5;
    let F = fn() {
        let r = 1;
        let child = fn() { return r; };
        return r + g;
    };
    return F();
}
fn main() {
    println(outer_fn());
    return 0;
}
"#,
        "6\n",
    );
}

#[test]
fn two_closures_over_one_binding_share_mutation() {
    assert_both_engines(
        "share",
        r#"fn main() {
    let x = 0;
    let inc = fn() { x = x + 1; return x; };
    let get = fn() { return x; };
    inc();
    println(get());
    println(x);
    return 0;
}
"#,
        "1\n1\n",
    );
}

// ------------------------------------------------- documented stdlib surface

#[test]
fn map_supports_index_assignment() {
    // Documented in docs/DATA_STRUCTURES.md; previously threw
    // "map does not support index assignment".
    assert_both_engines(
        "map_index_set",
        r#"fn main() {
    let m = map("a", 1, "b", 2);
    m["c"] = 3;
    println(m.len());
    println(m.get("c"));
    m["a"] = 10;
    println(m.get("a"));
    println(m.keys());
    return 0;
}
"#,
        "3\n3\n10\n[a, b, c]\n",
    );
}

#[test]
fn file_ops_return_results_not_thrown_errors() {
    // Documented contract: read_file/write_file return ok(...)/err(...).
    let dir = std::env::temp_dir().join("spid3r_audit_files");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let file = dir.join("probe_result.txt");
    let path = file.to_str().unwrap().replace('\\', "/");
    let src = format!(
        r#"fn main() {{
    let w = write_file("{}", "hello");
    println(w.is_ok());
    let r = read_file("{}");
    println(r.unwrap());
    let bad = read_file("{}missing_zz.txt");
    println(bad.is_err());
    return 0;
}}
"#,
        path, path, path
    );
    assert_both_engines("file_results", &src, "true\nhello\ntrue\n");
}

#[test]
fn task_status_and_repeatable_join() {
    let src = r#"fn main() {
    let t = spawn fn() { return 41; };
    sleep_ms(50);
    println(t.status());
    println(t.join());
    println(t.join());
    return 0;
}
"#;
    assert_both_engines("task_join", src, "done\n41\n41\n");
}

#[test]
fn failed_task_join_surfaces_the_thrown_value() {
    let src = r#"fn main() {
    let tf = spawn fn() { throw "boom"; };
    sleep_ms(50);
    println(tf.status());
    try {
        tf.join();
        println("no error");
    } catch (e) {
        println("caught " + e);
    }
    return 0;
}
"#;
    assert_both_engines("task_throw", src, "failed\ncaught boom\n");
}

// ------------------------------------------------------ json / rng coverage

#[test]
fn json_roundtrips_floats_escapes_and_map_keys() {
    assert_both_engines(
        "json_roundtrip",
        r#"fn main() {
    println(json_write(json_parse("1e30")));
    println(json_write(json_parse("2.0")));
    println(json_parse("\"\\u00e9\"").len());
    println(json_parse("\"\\ud83d\\ude00\"").len());
    println(json_write(map("a\"b", 1)));
    try { json_parse("[\"a\", 100000"); } catch (e) { println("caught1"); }
    return 0;
}
"#,
        "1e30\n2.0\n1\n1\n{\"a\\\"b\":1}\ncaught1\n",
    );
}

#[test]
fn json_depth_is_capped_not_a_stack_overflow_or_hang() {
    let src = r#"fn main() {
    let s = "[".repeat(100000) + "]".repeat(100000);
    try {
        json_parse(s);
        println("no error");
    } catch (e) {
        println("caught");
    }
    return 0;
}
"#;
    assert_both_engines("json_depth", src, "caught\n");
}

#[test]
fn random_int_stays_in_range() {
    let src = r#"fn main() {
    let a = random_int(0, 1000000);
    let b = random_int(0, 1000000);
    println(a >= 0 and a <= 1000000 and b >= 0 and b <= 1000000);
    return 0;
}
"#;
    // Deterministic assertions only: bounds hold on both engines. (Cross-run
    // non-repetition is asserted by the shell-level audit, not here.)
    assert_both_engines("rng_bounds", src, "true\n");
}

#[test]
fn wide_random_range_does_not_overflow() {
    // (hi - lo + 1) used to overflow u64 and panic on wide ranges.
    assert_both_engines(
        "rng_wide",
        r#"fn main() {
    let v = random_int(-9223372036854775807, 9223372036854775807);
    println(v >= -9223372036854775807 and v <= 9223372036854775807);
    return 0;
}
"#,
        "true\n",
    );
}
