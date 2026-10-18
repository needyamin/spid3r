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

#[test]
fn closure_upvalue_order_and_nested_captures() {
    // Regression for upvalue index collision when an enclosing function
    // captures an outer variable `a` before declaring its own captured local `b`,
    // plus branch-skipped first closure (`if false`) and 3-level nested closures.
    assert_both_engines(
        "upvalue_order_nested",
        r#"fn make_outer(a) {
    return fn(x) {
        let seen_a = a;
        let b = x + 10;
        if false {
            let dead = fn() { return b; };
        }
        let inner = fn(y) {
            b = b + y;
            return a + b + seen_a;
        };
        return inner(5) + b;
    };
}
fn main() {
    let f = make_outer(100);
    println(f(3));
    return 0;
}
"#,
        "236\n",
    );
}

#[test]
fn local_named_function_declarations_and_recursion() {
    assert_both_engines(
        "local_named_fn",
        r#"fn main() {
    fn fact(n) {
        if n <= 1 { return 1; }
        return n * fact(n - 1);
    }
    println(fact(5));
    return 0;
}
"#,
        "120\n",
    );
}

#[test]
fn cyclic_containers_display_equal_and_clone_without_deadlock() {
    assert_both_engines(
        "cyclic_containers",
        r#"fn main() {
    let a = [];
    a.push(a);
    println(a);
    println(a == a);
    let b = a.clone();
    println(b);
    return 0;
}
"#,
        "[[...]]\ntrue\n[[[...]]]\n",
    );
}

#[test]
fn self_argument_collection_methods_do_not_deadlock() {
    assert_both_engines(
        "self_collection_ops",
        r#"fn main() {
    let xs = [1, 2];
    xs.append(xs);
    println(xs);

    let s = set(1, 2);
    println(s.union(s).len());
    println(s.intersection(s).len());
    println(s.difference(s).len());

    let m = matrix(2, 2, 0);
    m.set(0, 0, 1);
    m.set(0, 1, 2);
    m.set(1, 0, 3);
    m.set(1, 1, 4);
    println(m.add(m).get(0, 1));
    println(m.mul(m).get(0, 0));
    return 0;
}
"#,
        "[1, 2, 1, 2]\n2\n2\n0\n4.0\n7.0\n",
    );
}

#[test]
fn linked_list_pop_back_preserves_tail() {
    assert_both_engines(
        "linked_list_tail",
        r#"fn main() {
    let l = linked_list(10, 20, 30);
    println(l.pop_back());
    println(l.peek_back());
    l.push_back(40);
    println(l.to_list());
    return 0;
}
"#,
        "30\n20\n[10, 20, 40]\n",
    );
}

#[test]
fn bst_and_heap_validate_comparability_when_empty() {
    assert_both_engines(
        "bst_heap_comp",
        r#"fn main() {
    let t = bst();
    try {
        t.insert(map("a", 1));
        println("bst accepted map");
    } catch (e) {
        println("bst rejected map");
    }
    println(t.len());

    let h = heap("min");
    try {
        h.push(map("a", 1));
        println("heap accepted map");
    } catch (e) {
        println("heap rejected map");
    }
    println(h.len());
    return 0;
}
"#,
        "bst rejected map\n0\nheap rejected map\n0\n",
    );
}

#[test]
fn string_pad_negative_width_throws_error() {
    assert_both_engines(
        "pad_neg_width",
        r#"fn main() {
    try {
        println("abc".pad_left(-1, " "));
    } catch (e) {
        println("caught left");
    }
    try {
        println("abc".pad_right(-5, " "));
    } catch (e) {
        println("caught right");
    }
    return 0;
}
"#,
        "caught left\ncaught right\n",
    );
}

#[test]
fn instance_clone_deep_copies_fields_on_both_engines() {
    assert_both_engines(
        "instance_deep_clone",
        r#"class Bag {
    public let items;
    public fn new(items) {
        self.items = items;
    }
}
fn main() {
    let xs = [new Bag([1, 2])];
    let ys = xs.clone();
    ys[0].items.push(3);
    println(xs[0].items);
    println(ys[0].items);
    return 0;
}
"#,
        "[1, 2]\n[1, 2, 3]\n",
    );
}

#[test]
fn json_parse_deduplicates_keys_and_rejects_malformed_numbers() {
    assert_both_engines(
        "json_hardening",
        r#"fn main() {
    let m = json_parse("{\"a\": 1, \"a\": 2}");
    println(m.len());
    println(m["a"]);
    for bad in ["1e999", "1.", "1e", "1e+"] {
        try {
            json_parse(bad);
            println("accepted " + bad);
        } catch (e) {
            println("rejected " + bad);
        }
    }
    return 0;
}
"#,
        "1\n2\nrejected 1e999\nrejected 1.\nrejected 1e\nrejected 1e+\n",
    );
}

#[test]
fn constructor_arity_checked_when_no_new_or_inherited_new() {
    let bad_empty = r#"class Empty {}
fn main() {
    let e = new Empty(1);
    return 0;
}
"#;
    assert!(run_src("tree", "bad_empty_ctor", bad_empty).is_err());
    assert!(run_src("vm", "bad_empty_ctor", bad_empty).is_err());

    let bad_inh = r#"class Base {
    public let x;
    public fn new(x) { self.x = x; }
}
class Sub : Base {}
fn main() {
    let s = new Sub(1, 2);
    return 0;
}
"#;
    assert!(run_src("tree", "bad_inh_ctor", bad_inh).is_err());
    assert!(run_src("vm", "bad_inh_ctor", bad_inh).is_err());
}

#[test]
fn lexer_bom_crlf_comments_float_overflow_and_multiline_escape_span() {
    // UTF-8 BOM + CRLF comments work on both engines.
    let src = "\u{FEFF}// header comment\r\nfn main() {\r\n    println(\"ok\");\r\n    return 0;\r\n}\r\n";
    assert_both_engines("bom_crlf", src, "ok\n");

    // Overflowing float literal is rejected at lex time.
    let overflow_float = format!("fn main() {{ let x = {}.0; return 0; }}", "9".repeat(310));
    assert!(spid3r::lexer::Lexer::new(&overflow_float, std::sync::Arc::from("<test>")).tokenize().is_err());

    // Backslash followed by newline in string literal does not underflow column.
    let bs_newline = "fn main() { let s = \"\\\n\"; return 0; }";
    assert!(spid3r::lexer::Lexer::new(bs_newline, std::sync::Arc::from("<test>")).tokenize().is_err());

    // Unexpected multi-byte UTF-8 character reports the full character.
    let err = spid3r::lexer::Lexer::new("€", std::sync::Arc::from("<test>")).tokenize().unwrap_err();
    assert!(err.message.contains("'€'"), "got: {}", err.message);
}

#[test]
fn parser_deep_nesting_returns_diagnostic_instead_of_stack_overflow() {
    let deep_parens = format!("fn main() {{ let x = {}1{}; return 0; }}", "(".repeat(300), ")".repeat(300));
    let err = spid3r::parser::parse_file(&deep_parens, std::sync::Arc::from("<test>")).unwrap_err();
    assert!(
        err.message.contains("nesting deeper than"),
        "unexpected error: {}",
        err.message
    );
}

#[test]
fn bytecode_deserializer_rejects_truncated_constants_huge_counts_and_negative_jumps() {
    use spid3r::vm::bytecode::{deserialize_vmfunc, serialize_vmfunc, validate_chunk, Chunk, OpCode};
    use spid3r::value::{Value, VmFunc};
    use std::sync::Arc;

    // 1. Huge constant count in header must fail immediately without OOM.
    let mut huge = Vec::new();
    huge.extend_from_slice(b"SPDR");
    huge.extend_from_slice(&3u32.to_le_bytes()); // version
    huge.push(0); // name = None
    huge.extend_from_slice(&0usize.to_le_bytes()); // arity = 0
    huge.push(0); // is_async = false
    huge.push(0); // own_upvalues = 0
    huge.extend_from_slice(&0usize.to_le_bytes()); // code_len = 0
    huge.extend_from_slice(&(u32::MAX as usize).to_le_bytes()); // huge constants count
    assert!(deserialize_vmfunc(&huge).is_err());

    // 2. Truncated Int/Float constant payload must return Err, not panic.
    let mut chunk = Chunk::new();
    let ci = chunk.emit_const(Value::Int(42));
    let cf = chunk.emit_const(Value::Float(3.5));
    chunk.emit_u16(OpCode::Constant, ci);
    chunk.emit(OpCode::Pop);
    chunk.emit_u16(OpCode::Constant, cf);
    chunk.emit(OpCode::Return);
    let func = VmFunc {
        name: None,
        arity: 0,
        is_async: false,
        chunk: Arc::new(chunk),
        upvalues: Vec::new(),
        own_upvalues: 0,
    };
    let valid_bytes = serialize_vmfunc(&func);
    for cut in 0..valid_bytes.len() {
        assert!(deserialize_vmfunc(&valid_bytes[..cut]).is_err());
    }

    // 3. Negative jump target before IP 0 must be rejected by validate_chunk.
    let mut bad_jump = Chunk::new();
    bad_jump.emit_i16(OpCode::Jump, -10);
    bad_jump.emit(OpCode::Return);
    assert!(validate_chunk(&bad_jump).is_err());
}

#[test]
fn finally_runs_on_return_break_and_continue_and_now_is_typed_int() {
    assert_both_engines(
        "finally_early_exit",
        r#"fn test_ret() -> int {
    try {
        try {
            let local_in_try = 42;
            return local_in_try;
        } finally {
            println("inner finally");
        }
    } finally {
        println("outer finally");
    }
    return 0;
}

fn main() {
    let t: int = now();
    println(t >= 0);
    println(test_ret());

    for x in [1, 2, 3] {
        try {
            let tmp = x * 10;
            if x == 2 {
                continue;
            }
            if x == 3 {
                break;
            }
            println(tmp);
        } finally {
            println("loop finally " + string(x));
        }
    }
    return 0;
}
"#,
        "true\ninner finally\nouter finally\n42\n10\nloop finally 1\nloop finally 2\nloop finally 3\n",
    );
}


