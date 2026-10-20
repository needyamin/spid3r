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

/// Assert a program produces `expected` identically on tree, vm, and compile+exec.
fn assert_three_engines(name: &str, src: &str, expected: &str) {
    let tree = run_src("tree", name, src)
        .unwrap_or_else(|e| panic!("[{}] tree run failed:\n{}", name, e));
    let vm = run_src("vm", name, src)
        .unwrap_or_else(|e| panic!("[{}] vm run failed:\n{}", name, e));
    assert_eq!(tree, expected, "[{}] tree output", name);
    assert_eq!(vm, expected, "[{}] vm output (diverged from tree)", name);

    let path = std::env::temp_dir().join(format!("spid3r_audit_{}.matrix", name));
    let tc = std::env::temp_dir().join(format!("spid3r_audit_{}.matrixc", name));
    let comp = Command::new(bin())
        .args(["compile", path.to_str().unwrap(), "-o", tc.to_str().unwrap()])
        .output()
        .expect("compile");
    assert!(
        comp.status.success(),
        "[{}] compile failed: {}",
        name,
        String::from_utf8_lossy(&comp.stderr)
    );
    let exec = Command::new(bin())
        .args(["exec", tc.to_str().unwrap()])
        .output()
        .expect("exec");
    assert!(
        exec.status.success(),
        "[{}] exec failed: {}",
        name,
        String::from_utf8_lossy(&exec.stderr)
    );
    let exec_out = String::from_utf8_lossy(&exec.stdout).to_string();
    assert_eq!(exec_out, expected, "[{}] exec output (diverged from tree/vm)", name);
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

#[test]
fn vm_stack_trace_preserves_exact_line_numbers_across_calls_and_throws() {
    use spid3r::parser;
    use spid3r::semantic;
    use spid3r::vm::bytecode::{deserialize_vmfunc, serialize_vmfunc};
    use spid3r::vm::{compile_program, Vm};
    use std::sync::Arc;

    // Line 1: fn leaf(a, b) {
    // Line 2:     throw "leaf_err";
    // Line 3: }
    // Line 4: fn middle(x) {
    // Line 5:     return leaf(
    // Line 6:         x,
    // Line 7:         x + 1
    // Line 8:     );
    // Line 9: }
    // Line 10: fn main() {
    // Line 11:     middle(10);
    // Line 12:     return 0;
    // Line 13: }
    let src = "fn leaf(a, b) {\n    throw \"leaf_err\";\n}\nfn middle(x) {\n    return leaf(\n        x,\n        x + 1\n    );\n}\nfn main() {\n    middle(10);\n    return 0;\n}\n";

    let tree_err = run_src("tree", "trace_lines", src).unwrap_err();
    let vm_err = run_src("vm", "trace_lines", src).unwrap_err();

    for (engine, err) in [("tree", &tree_err), ("vm", &vm_err)] {
        assert!(
            err.contains(":2:"),
            "{} error should point to line 2 (throw statement):\n{}",
            engine,
            err
        );
        assert!(
            err.contains("at leaf (") && err.contains("at middle (") && err.contains("at main ("),
            "{} error should include leaf, middle, and main frames:\n{}",
            engine,
            err
        );
    }

    // Also verify after .matrixc round-trip serialization + deserialization.
    let prog = parser::parse_file(src, Arc::from("trace_lines.matrix")).expect("parse");
    semantic::check_file(&prog, true, "trace_lines.matrix").expect("check");
    let (func, _) = compile_program(&prog, "trace_lines.matrix", &[]).expect("compile");
    let bytes = serialize_vmfunc(&func);
    let roundtrip = deserialize_vmfunc(&bytes).expect("deserialize");
    let mut vm = Vm::new(Arc::from("trace_lines.matrix"), Vec::new());
    let diag = vm.run_compiled(Arc::new(roundtrip)).unwrap_err();
    assert_eq!(diag.span.as_ref().map(|s| s.line), Some(2));
    let frame_lines: Vec<(&str, u32)> = diag
        .trace
        .iter()
        .map(|f| (f.func.as_deref().unwrap_or("<script>"), f.span.line))
        .collect();
    assert_eq!(
        frame_lines,
        vec![("leaf", 2), ("middle", 5), ("main", 11)],
        "unexpected VM trace frame lines: {:?}",
        frame_lines
    );
}

#[test]
fn native_staticlib_discovery_supports_env_overrides_and_sysroot_layouts() {
    use spid3r::native::{runtime_lib_name, staticlib_candidates};
    use std::path::Path;

    let name = runtime_lib_name();
    let rt_file = Path::new("/opt/spid3r/custom_rt.a");
    let lib_dir = Path::new("/opt/spid3r/lib");
    let matrix_dir = Path::new("/opt/matrix/rlib");
    let exe_path = Path::new("/usr/local/bin/spid3r");
    let cwd_path = Path::new("/workspace/project");

    let cands = staticlib_candidates(
        Some(rt_file),
        Some(lib_dir),
        Some(matrix_dir),
        Some(exe_path),
        Some(cwd_path),
    );

    assert_eq!(cands[0], rt_file);
    assert!(cands.contains(&lib_dir.join(name)));
    assert!(cands.contains(&matrix_dir.join(name)));
    assert!(cands.contains(&Path::new("/usr/local/lib").join(name)));
    assert!(cands.contains(&Path::new("/usr/local/lib/spid3r").join(name)));
    assert!(cands.contains(&cwd_path.join("target").join("release").join(name)));
    assert!(cands.contains(&cwd_path.join("target").join("debug").join(name)));
}



// ================================================================ 2026-10-19
// Regression tests for the second full audit: mutex cross-task deadlock,
// uncapped matrix allocation, VM compiler size limits, crafted `.matrixc`
// reader overflow, module path escapes, `http_fetch` URL control characters,
// and the debugger's BOM handling.

#[test]
fn mutex_contention_across_tasks_does_not_deadlock() {
    // Was: `lock()` parked on the internal condvar while holding the outer
    // `Arc<Mutex<MutexData>>` guard, so a second task could never reach
    // `unlock()` — the first contention hung both tasks (and the process).
    let src = r#"fn main() {
    let m = mutex(0);
    let t1 = spawn fn() {
        m.lock();
        sleep_ms(50);
        m.unlock();
        return 1;
    };
    let t2 = spawn fn() {
        m.lock();
        m.unlock();
        return 2;
    };
    println(t1.join());
    println(t2.join());
    return 0;
}
"#;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = run_src("tree", "mutex_contention", src);
        let _ = tx.send(out);
    });
    match rx.recv_timeout(std::time::Duration::from_secs(30)) {
        Ok(out) => assert_eq!(
            out.expect("mutex contention run failed"),
            "1\n2\n",
            "mutex contention result"
        ),
        Err(_) => panic!("mutex contention deadlocked again (no completion in 30s)"),
    }
}

#[test]
fn matrix_constructor_rejects_oversized_allocation() {
    // Was: `matrix(2000000000, 2000000000, 0)` aborted the host (capacity
    // overflow) and `matrix(9000000000, 9000000000, 0)` overflowed rows*cols.
    let src = r#"fn main() {
    let m = matrix(2000000000, 2000000000, 0);
    println(len(m));
    return 0;
}
"#;
    let tree = run_src("tree", "matrix_big_tree", src).unwrap_err();
    let vm = run_src("vm", "matrix_big_vm", src).unwrap_err();
    for (engine, err) in [("tree", tree), ("vm", vm)] {
        assert!(
            err.contains("max") && err.contains("cells"),
            "[{}] expected a clean matrix size error, got: {}",
            engine,
            err
        );
    }
    // A small matrix still works on both engines.
    assert_both_engines(
        "matrix_small",
        "fn main() {\n    let m = matrix(2, 3, 1.5);\n    println(m.rows());\n    println(m.cols());\n    return 0;\n}\n",
        "2\n3\n",
    );
}

#[test]
fn vm_rejects_more_than_255_call_arguments() {
    // Was: argc was emitted with `as u8`, silently truncating 300 -> 44 and
    // corrupting the VM stack (wrong results / bogus errors).
    let args = (1..=300).map(|i| i.to_string()).collect::<Vec<_>>().join(", ");
    let src = format!(
        "fn main() {{\n    let xs = list({});\n    println(len(xs));\n    return 0;\n}}\n",
        args
    );
    let tree = run_src("tree", "args300_tree", &src).expect("tree handles 300 args");
    assert_eq!(tree, "300\n");
    let vm = run_src("vm", "args300_vm", &src).unwrap_err();
    assert!(
        vm.contains("too many arguments"),
        "expected a clean argc diagnostic, got: {}",
        vm
    );
}

#[test]
fn vm_rejects_more_than_255_locals() {
    // Was: slot indices were `locals.len() as u8`, so local 299 silently
    // aliased slot 43 and read the wrong value.
    let mut body = String::from("fn main() {\n");
    for i in 0..300 {
        body.push_str(&format!("    let v{} = {};\n", i, i));
    }
    body.push_str("    println(v299);\n    return 0;\n}\n");
    let tree = run_src("tree", "locals300_tree", &body).expect("tree handles 300 locals");
    assert_eq!(tree, "299\n");
    let vm = run_src("vm", "locals300_vm", &body).unwrap_err();
    assert!(
        vm.contains("too many locals"),
        "expected a clean locals diagnostic, got: {}",
        vm
    );
}

#[test]
fn vm_rejects_oversized_function_bodies_instead_of_wrapping_jumps() {
    // Was: jump offsets were cast to i16, so a >32 KiB function produced a
    // mis-jumping chunk (and `compile` wrote the broken .matrixc).
    let mut body = String::from("fn main() {\n    let x = 0;\n    while x < 6000 {\n");
    for _ in 0..6000 {
        body.push_str("        x = x + 1;\n");
    }
    body.push_str("    }\n    println(x);\n    return 0;\n}\n");
    let tree = run_src("tree", "bigloop_tree", &body).expect("tree runs the big loop");
    assert_eq!(tree, "6000\n");
    let vm = run_src("vm", "bigloop_vm", &body).unwrap_err();
    assert!(
        vm.contains("function body too large"),
        "expected a clean size diagnostic, got: {}",
        vm
    );
    // `compile` must refuse to write the broken artifact.
    let src_path = std::env::temp_dir().join("spid3r_audit_bigloop_compile.matrix");
    std::fs::write(&src_path, &body).unwrap();
    let out = Command::new(bin())
        .args(["compile", src_path.to_str().unwrap(), "-o"])
        .arg(std::env::temp_dir().join("spid3r_audit_bigloop.matrixc"))
        .output()
        .expect("spawn compile");
    assert!(!out.status.success(), "compile should fail on oversized functions");
    assert!(String::from_utf8_lossy(&out.stderr).contains("function body too large"));
}

#[test]
fn crafted_matrixc_overflowing_string_length_is_rejected() {
    // Was: `self.p + n` wrapped for a crafted length near usize::MAX, past the
    // bounds check, and the host panicked on the slice (exit 101).
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"SPID3R");
    bytes.extend_from_slice(&1u16.to_le_bytes()); // format version
    bytes.push(1); // opt_str tag: Some
    bytes.extend_from_slice(&u64::MAX.to_le_bytes()); // string length
    let crafted_err = match spid3r::vm::bytecode::deserialize_vmfunc(&bytes) {
        Err(e) => e,
        Ok(_) => panic!("crafted header must be rejected"),
    };
    assert!(
        format!("{}", crafted_err).contains("string length exceeds file"),
        "expected a clean format error, got: {}",
        crafted_err
    );
}

#[test]
fn import_rejects_escaping_paths() {
    // Imports must stay inside the documented search roots: no absolute
    // paths, drive letters, or `..` components.
    for (name, path) in [
        ("import_abs", "/etc/passwd"),
        ("import_parent", "../outside"),
        ("import_drive", "C:/Windows/win.ini"),
    ] {
        let src = format!("import \"{}\";\nfn main() {{ return 0; }}\n", path);
        for engine in ["tree", "vm"] {
            let err = run_src(engine, name, &src)
                .expect_err(&format!("[{}] import '{}' must be rejected", engine, path));
            assert!(
                err.contains("invalid module path"),
                "[{}] expected invalid-module-path, got: {}",
                engine,
                err
            );
        }
    }
}

#[test]
fn http_fetch_rejects_control_characters_in_url() {
    // A URL built from untrusted text could inject CRLF into the request line
    // or host header; the fetch must fail before any connection is attempted.
    let src = r#"fn main() {
    let r = http_fetch("GET", "http://example.invalid/\r\nx-injected: 1");
    if r.is_err() {
        println(r.unwrap_err());
    }
    return 0;
}
"#;
    let tree = run_src("tree", "fetch_ctl_tree", src).expect("tree runs");
    let vm = run_src("vm", "fetch_ctl_vm", src).expect("vm runs");
    assert!(tree.contains("control characters"), "tree: {}", tree);
    assert_eq!(tree, vm, "engines must agree on the fetch error");
}

#[test]
fn debugger_commands_file_strips_utf8_bom() {
    // Was: a BOM-prefixed commands file made the first command
    // "unknown command '<BOM>bt'".
    let dir = std::env::temp_dir();
    let prog = dir.join("spid3r_audit_dbg_bom.matrix");
    std::fs::write(
        &prog,
        "fn add(a, b) {\n    return a + b;\n}\nfn main() {\n    let x = add(2, 3);\n    println(x);\n    return 0;\n}\n",
    )
    .unwrap();
    let cmds = dir.join("spid3r_audit_dbg_bom_cmds.txt");
    std::fs::write(&cmds, "\u{feff}bt\nquit\n").unwrap();
    let out = Command::new(bin())
        .arg("debug")
        .arg(&prog)
        .args(["--break", "6", "--commands"])
        .arg(&cmds)
        .output()
        .expect("spawn debug");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !stdout.contains("unknown command"),
        "BOM must be stripped from the first command; got:\n{}",
        stdout
    );
}

#[test]
fn vm_rejects_more_than_128_captured_variables() {
    // Was: `capture_local` let `own_upvalues` grow past the 7-bit descriptor
    // encoding, so capture #129 silently aliased capture #1's cell and the VM
    // computed wrong values (tree: 9385, vm: 10257 on the 129-capture probe).
    let mut body = String::from("fn main() {\n");
    for i in 1..=130 {
        body.push_str(&format!("    let v{} = {};\n", i, i));
    }
    let refs: Vec<String> = (1..=130).map(|i| format!("v{}", i)).collect();
    body.push_str(&format!(
        "    let f = fn() {{ return {}; }};\n",
        refs.join(" + ")
    ));
    body.push_str("    v1 = 1001;\n    println(f());\n    return 0;\n}\n");
    let tree = run_src("tree", "caps130_tree", &body).expect("tree handles 130 captures");
    assert_eq!(tree, "9515\n");
    let vm = run_src("vm", "caps130_vm", &body).unwrap_err();
    assert!(
        vm.contains("too many captured variables"),
        "expected a clean capture-limit diagnostic, got: {}",
        vm
    );
    // Boundary: exactly 128 captured locals must still work on both engines.
    let mut ok = String::from("fn main() {\n");
    for i in 1..=128 {
        ok.push_str(&format!("    let v{} = {};\n", i, i));
    }
    let ok_refs: Vec<String> = (1..=128).map(|i| format!("v{}", i)).collect();
    ok.push_str(&format!(
        "    let f = fn() {{ return {}; }};\n",
        ok_refs.join(" + ")
    ));
    ok.push_str("    v1 = 1001;\n    println(f());\n    return 0;\n}\n");
    let tree_ok = run_src("tree", "caps128_tree", &ok).expect("tree handles 128 captures");
    let vm_ok = run_src("vm", "caps128_vm", &ok).expect("vm handles 128 captures");
    assert_eq!(tree_ok, "9256\n");
    assert_eq!(vm_ok, "9256\n");
}

#[test]
fn vm_rejects_more_than_255_element_list_literals() {
    // Was: `Expr::List` emitted `items.len() as u8` into Construct with no
    // argc guard, so a 256-element literal truncated to argc=0 and corrupted
    // the value stack ("int has no length"); `compile` still wrote the broken
    // .matrixc with exit 0.
    let items: Vec<String> = (1..=300).map(|i| i.to_string()).collect();
    let src = format!(
        "fn main() {{\n    let xs = [{}];\n    println(len(xs));\n    return 0;\n}}\n",
        items.join(", ")
    );
    let tree = run_src("tree", "list300_tree", &src).expect("tree handles 300 elements");
    assert_eq!(tree, "300\n");
    let vm = run_src("vm", "list300_vm", &src).unwrap_err();
    assert!(
        vm.contains("too many elements in list literal"),
        "expected a clean list-literal diagnostic, got: {}",
        vm
    );
}

// ===========================================================================
// 2026-10-20: standard-library expansion (math, crypto, exec_capture) and
// scientific-notation float literals. Every program below must produce the
// same stdout on the tree engine and on the bytecode VM.
// ===========================================================================

#[test]
fn scalar_math_builtins_match_on_both_engines() {
    // Rounding modes, sign, the exponential/log family, trigonometry and the
    // inverse trig family. Values are printed with the same float formatter
    // both engines use, so an engine that folded a constant differently (or
    // routed a builtin to a different implementation) would diverge here.
    assert_both_engines(
        "math_rounding",
        r#"fn main() {
    println(floor(3.7) + floor(-3.2) + floor(0.0));
    println(ceil(3.2) + ceil(-3.7));
    println(round(2.5) + round(-2.5) + round(0.4));
    println(trunc(-3.9) + trunc(3.9));
    println(sign(-4.0) + sign(0.0) + sign(9.0));
    println(sign(-0.0) < 0.0);
    return 0;
}
"#,
        "-1.0\n1.0\n0.0\n0.0\n0.0\nfalse\n",
    );
}
#[test]
fn math_log_and_trig_builtins_match_on_both_engines() {
    assert_both_engines(
        "math_log_trig",
        r#"fn main() {
    println(exp(0.0));
    println(ln(1.0));
    println(log10(1000.0));
    println(log2(8.0));
    println(sin(0.0) + cos(0.0) + tan(0.0));
    println(asin(1.0));
    println(acos(1.0));
    println(atan(1.0));
    println(atan2(1.0, 1.0));
    println(atan2(0.0, -1.0));
    println(hypot(3.0, 4.0));
    return 0;
}
"#,
        "1.0\n0.0\n3.0\n3.0\n1.0\n1.5707963267948966\n0.0\n0.7853981633974483\n0.7853981633974483\n3.141592653589793\n5.0\n",
    );
}

#[test]
fn math_predicate_and_extremes_match_on_both_engines() {
    // `nan()`, `inf()`, and the predicates: a builtin that turned a NaN into
    // an error (or silently normalized it) would break these lines.
    assert_both_engines(
        "math_pred",
        r#"fn main() {
    println(is_nan(nan()));
    println(is_nan(0.0));
    println(is_inf(inf()));
    println(is_inf(-1.0));
    println(is_nan(sqrt(-1.0)));
    println(is_inf(exp(1000.0)));
    println(sign(nan()) == sign(nan()));
    return 0;
}
"#,
        "true\nfalse\ntrue\nfalse\ntrue\ntrue\nfalse\n",
    );
}

#[test]
fn clamp_and_float_or_match_on_both_engines() {
    assert_both_engines(
        "math_clamp_float_or",
        r#"fn main() {
    println(clamp(15.0, 0.0, 10.0));
    println(clamp(-15.0, 0.0, 10.0));
    println(clamp(5.0, 0.0, 10.0));
    println(clamp(-15, 0, 10));
    println(float_or("abc", 1.5));
    println(float_or("2.25", 1.5));
    println(float_or("", 0.5));
    println(float_or(" 7 ", 1.0));
    println(float_or(3, 1.0));
    return 0;
}
"#,
        "10.0\n0.0\n5.0\n0.0\n1.5\n2.25\n0.5\n7.0\n3.0\n",
    );
}

#[test]
fn nan_and_inf_constants_are_finite_safe() {
    // A bare `nan()`/`inf()` literal must print and compare without the host
    // panicking on a non-finite float.
    assert_both_engines(
        "math_consts",
        r#"fn main() {
    println(is_nan(nan()));
    println(is_inf(inf()));
    println(inf() > 1e308);
    println(nan() == nan());
    return 0;
}
"#,
        "true\ntrue\ntrue\nfalse\n",
    );
}

#[test]
fn scalar_math_arity_and_type_errors_are_clean() {
    // Wrong arity / wrong type must be a catchable Spid3r error, never a host
    // panic (the VM decodes native calls through the same table).
    let bad_arity = "fn main() { floor(1.0, 2.0); return 0; }";
    let bad_type = "fn main() { floor(\"x\"); return 0; }";
    let clamp_range = "fn main() { clamp(1.0, 10.0, 0.0); return 0; }";
    for (name, src, needle) in [
        ("math_arity", bad_arity, "floor expects 1 argument"),
        ("math_type", bad_type, "floor expects a number"),
        ("clamp_range", clamp_range, "not below or equal"),
    ] {
        for engine in ["tree", "vm"] {
            // run_src reports a non-zero exit as Err, so the diagnostic is
            // expected here: misuse must fail cleanly, never panic the host.
            let err = run_src(engine, name, src).expect_err("argument misuse must fail");
            assert!(
                err.contains(needle),
                "[{} {}] expected {:?}, got: {}",
                engine,
                name,
                needle,
                err
            );
        }
    }
}

// ---------------------------------------------------------------- crypto

#[test]
fn sha256_and_hmac_sha256_match_reference_vectors() {
    // FIPS 180-4 / RFC 2104 test vectors, checked against an independent
    // implementation. `sha256`/`hmac_sha256` share the package layer's
    // FIPS 180-4 code, so a drift in either one shows up here.
    assert_both_engines(
        "sha256_vectors",
        r#"fn main() {
    println(sha256(""));
    println(sha256("abc"));
    println(sha256(string("a").repeat(1000)));
    println(hmac_sha256("key", "The quick brown fox jumps over the lazy dog"));
    // A key longer than the 64-byte block must be pre-hashed, not rejected.
    println(hmac_sha256(string("K").repeat(200), "msg"));
    println(hmac_sha256("", ""));
    return 0;
}
"#,
        concat!(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\n",
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3\n",
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8\n",
            "9b10789f99ca41174b211d7ef1b98d78053cfda1ba9cd12c6e4e459ed03bbc13\n",
            "b613679a0814d9ec772f95d778c35fc5ff1697c493715653c6c712144292c5ad\n",
        ),
    );
}

#[test]
fn sha256_hex_digest_is_lowercase_and_fixed_width() {
    // The digest must be exactly 64 lowercase hex characters: uppercase or a
    // padded/short form would break a manifest hash.
    assert_both_engines(
        "sha256_shape",
        r#"fn main() {
    let d = sha256("hello");
    println(len(d));
    println(d == d.lower());
    println(d == string("hello"));
    println(len(hmac_sha256("k", "m")));
    return 0;
}
"#,
        "64\ntrue\nfalse\n64\n",
    );
}

// ---------------------------------------------------------- exec_capture

/// Escape a Rust string into a Spid3r string literal body. Only `\`, `"` and
/// the three named escapes are meaningful in Spid3r, so everything else is
/// passed through verbatim.
fn spid3r_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

#[test]
fn exec_capture_pipes_stdin_and_collects_stdout() {
    // The child is the interpreter under test running a script that echoes
    // its stdin, so the test needs no OS-specific helper binary: stdin is
    // written, stdout is captured, and the exit code is reported. Brackets
    // mark the exact captured text so a stray trailing newline is visible.
    let child_script = std::env::temp_dir().join("spid3r_audit_echo_stdin.matrix");
    std::fs::write(
        &child_script,
        "fn main() {\n    let s = read_line();\n    print(\"got:\" + string(s));\n    return 0;\n}\n",
    )
    .unwrap();
    let bin = spid3r_string(bin().to_str().unwrap());
    let script = spid3r_string(child_script.to_str().unwrap());
    let src = format!(
        r#"fn main() {{
    let child = "{bin}";
    let r = exec_capture(child, ["run", "{script}"], "hello from the parent");
    if not r.is_ok() {{ println("SPAWN " + r.unwrap_err()); return 1; }}
    let m = r.unwrap();
    println(m["code"]);
    println("[" + m["stdout"] + "]");
    println("(" + m["stderr"] + ")");
    let empty = exec_capture(child, ["run", "{script}"], null);
    if not empty.is_ok() {{ println("SPAWN2 " + empty.unwrap_err()); return 1; }}
    let m2 = empty.unwrap();
    println(m2["code"]);
    println("[" + m2["stdout"] + "]");
    return 0;
}}
"#
    );
    assert_both_engines(
        "exec_capture_stdin",
        &src,
        "0\n[got:hello from the parent]\n()\n0\n[got:null]\n",
    );
}

#[test]
fn exec_capture_reports_child_exit_code_and_stderr() {
    // A missing script is a usage error, not a spawn failure: the child runs,
    // exits 2, and writes its diagnostic to stderr (never stdout). Both fields
    // of the result map have to be wired correctly for this to hold.
    let missing = std::env::temp_dir().join("spid3r_audit_missing_script.matrix");
    let _ = std::fs::remove_file(&missing);
    let bin = spid3r_string(bin().to_str().unwrap());
    let target = spid3r_string(missing.to_str().unwrap());
    let src = format!(
        r#"fn main() {{
    let r = exec_capture("{bin}", ["run", "{target}"], null);
    if not r.is_ok() {{ println("SPAWN " + r.unwrap_err()); return 1; }}
    let m = r.unwrap();
    println(m["code"]);
    println("[" + m["stdout"] + "]");
    println(m["stderr"].contains("is not a file"));
    return 0;
}}
"#
    );
    assert_both_engines("exec_capture_stderr", &src, "2\n[]\ntrue\n");
}

#[test]
fn exec_capture_reports_a_failed_spawn_as_an_error() {
    // Documented contract: a process that cannot be spawned yields
    // `err(msg)`, not a thrown runtime error.
    assert_both_engines(
        "exec_capture_spawn_err",
        r#"fn main() {
    let r = exec_capture("definitely-not-a-real-spid3r-binary-xyz", [], null);
    if not r.is_err() { return 1; }
    let msg = r.unwrap_err();
    if not msg.contains("definitely-not-a-real-spid3r-binary-xyz") { return 1; }
    println("spawn failure yields err");
    return 0;
}
"#,
        "spawn failure yields err\n",
    );
}

#[test]
fn exec_capture_arity_and_type_errors_are_clean() {
    for (name, src, needle) in [
        ("exec_arity", "fn main() { exec_capture(\"x\", list()); return 0; }", "exec_capture expects 3 arguments"),
        ("exec_cmd_type", "fn main() { exec_capture(1, list(), null); return 0; }", "exec_capture expects a string"),
        ("exec_argv_type", "fn main() { exec_capture(\"x\", \"not-a-list\", null); return 0; }", "expects a list of arguments"),
        ("exec_stdin_type", "fn main() { exec_capture(\"x\", list(), 5); return 0; }", "expects a string or null for stdin"),
    ] {
        for engine in ["tree", "vm"] {
            // run_src reports a non-zero exit as Err, so the diagnostic is
            // expected here: misuse must fail cleanly, never panic the host.
            let err = run_src(engine, name, src).expect_err("argument misuse must fail");
            assert!(
                err.contains(needle),
                "[{} {}] expected {:?}, got: {}",
                engine,
                name,
                needle,
                err
            );
        }
    }
}

// ------------------------------------------------ scientific float literals

#[test]
fn scientific_notation_float_literals_lex_as_single_floats() {
    // `1.5e3`, `1e-5` and `2E+4` must each be ONE float token: if the lexer
    // split `1.5e3` into `1.5` followed by the ident `e3`, this program would
    // fail to parse instead of printing 1501.0.
    assert_both_engines(
        "sci_notation",
        r#"fn main() {
    println(1.5e3 + 1.0);
    println(1e-5 * 100000.0);
    println(2E+4 / 4.0);
    println(0.5e1 + 5e0 + 1.0E2);
    let a = 1.5e3;
    let b = 2E-1;
    println(a * b);
    return 0;
}
"#,
        "1501.0\n1.0\n5000.0\n110.0\n300.0\n",
    );
}

#[test]
fn scientific_notation_does_not_swallow_identifiers() {
    // The exponent marker must be followed by a digit, so `1emit` stays the
    // int `1` plus the identifier `emit`. Without that lookahead the lexer
    // would consume `1emit` and fail to make a float of it.
    let src = "fn emit() { return 7; }\nfn main() { println(1emit()); return 0; }";
    for engine in ["tree", "vm"] {
        let err = run_src(engine, "sci_ident", src).expect_err("no operator before the call");
        assert!(
            err.contains("expected"),
            "[{}] expected a parse error, got: {}",
            engine,
            err
        );
    }
}

#[test]
fn overflowing_scientific_float_literal_is_a_clean_lexer_error() {
    // `1e400` overflows f64. It must be a lexer diagnostic naming the literal,
    // never an infinity, a host panic, or a silently truncated value.
    for engine in ["tree", "vm"] {
        let err = run_src(engine, "sci_overflow", "fn main() { println(1e400); return 0; }")
            .expect_err("an overflowing float literal must be rejected");
        assert!(
            err.contains("out of f64 range"),
            "[{}] expected an out-of-range diagnostic, got: {}",
            engine,
            err
        );
        assert!(
            err.contains("1e400"),
            "[{}] the diagnostic should name the offending literal, got: {}",
            engine,
            err
        );
    }
}

// -------------------------------------------------- matrix indexing docs

#[test]
fn matrix_indexing_is_flat_row_major_and_2d_uses_get_set() {
    // Pins the documented contract stated in docs/DATA_STRUCTURES.md and
    // README.md: `m[i]` is a FLAT row-major cell index answering a float, and
    // 2-D addressing goes through `get`/`set`.
    assert_both_engines(
        "matrix_indexing",
        r#"fn main() {
    let mx = matrix(2, 3, 0.0);
    mx[0] = 1.0;
    mx[1] = 2.0;
    mx[2] = 3.0;
    mx[3] = 4.0;
    println(mx[3]);
    println(mx.get(1, 0));
    println(mx.get(0, 0));
    mx.set(1, 2, 7.5);
    println(mx[5]);
    println(len(mx));
    println(mx.rows());
    println(mx.cols());
    println(mx.row(0));
    println(mx.col(0));
    println(mx.get(1, 0) == mx[1 * mx.cols() + 0]);
    return 0;
}
"#,
        "4.0\n4.0\n1.0\n7.5\n6\n2\n3\n[1.0, 2.0, 3.0]\n[1.0, 4.0]\ntrue\n",
    );
}

#[test]
fn matrix_flat_index_out_of_bounds_throws() {
    // The flat buffer has rows*cols cells, so index 6 on a 2x3 matrix is out
    // of range even though 6 is twice its column count.
    let src = r#"fn main() {
    let mx = matrix(2, 3, 0.0);
    println(mx[6]);
    return 0;
}
"#;
    for engine in ["tree", "vm"] {
        let err = run_src(engine, "matrix_oob", src).expect_err("index 6 is out of bounds");
        assert!(
            err.contains("out of bounds"),
            "[{}] expected an out-of-bounds error, got: {}",
            engine,
            err
        );
    }
}


// ===========================================================================
// 2026-10-20: comparator sorting & float-precision formatting round.
// Every program below must produce the same stdout on tree and on the VM.
// ===========================================================================

#[test]
fn sort_by_accepts_bool_and_numeric_comparators() {
    // `sort_by` sorts in place; `sorted_by` returns a new list and leaves the
    // receiver alone. Both accept a bool comparator (`true` means `a < b`)
    // and a 3-way numeric one.
    assert_both_engines(
        "sort_by_kinds",
        r#"fn main() {
    let xs = list(3, 1, 2, 1);
    xs.sort_by(fn(a, b) { return a < b; });
    println(xs);
    let ys = list(5, 1, 4, 2);
    ys.sorted_by(fn(a, b) { return b - a; });
    println(ys);
    let zs = ys.sorted_by(fn(a, b) { return a < b; });
    println(zs);
    let ws = list(3, 1, 2);
    println(ws.sorted_by(fn(a, b) { return float(b) - float(a); }));
    println(ws);
    let vs = list();
    vs.sort_by(fn(a, b) { return a < b; });
    println(vs);
    let one = list(7);
    one.sort_by(fn(a, b) { return a < b; });
    println(one);
    return 0;
}
"#,
        "[1, 1, 2, 3]\n[5, 1, 4, 2]\n[1, 2, 4, 5]\n[3, 2, 1]\n[3, 1, 2]\n[]\n[7]\n",
    );
}

#[test]
fn sort_by_on_lists_of_maps_sorts_by_field() {
    // The comparator sees whole elements, so a list of maps sorts by any
    // expression over them — including a compound (length, then value) key.
    assert_both_engines(
        "sort_by_maps",
        r#"fn main() {
    let people = list(
        map("name", "carol", "age", 30),
        map("name", "alice", "age", 25),
        map("name", "bob", "age", 35)
    );
    people.sort_by(fn(a, b) { return a["age"] < b["age"]; });
    println(people[0]["name"] + " " + people[1]["name"] + " " + people[2]["name"]);
    let words = list("pear", "fig", "banana", "fig");
    println(words.sorted_by(fn(a, b) { return len(a) < len(b) or (len(a) == len(b) and a < b); }));
    return 0;
}
"#,
        "alice carol bob\n[fig, fig, pear, banana]\n",
    );
}

#[test]
fn sort_by_is_stable_for_equal_elements() {
    // A bool comparator can only say "a is less than b", so a `false` alone
    // cannot distinguish "greater" from "equal". Was: reading `false` as
    // "greater" reversed every equal group (this printed dbca instead of
    // bdac, and reversed the whole list for an always-false comparator). The
    // implementation asks the mirrored question to recover equality, so the
    // input order survives inside an equal group.
    assert_both_engines(
        "sort_by_stable",
        r#"fn main() {
    let pairs = list(list(1, "a"), list(0, "b"), list(1, "c"), list(0, "d"));
    pairs.sort_by(fn(x, y) { return x[0] < y[0]; });
    let out = "";
    for p in pairs { out = out + p[1]; }
    println(out);

    let big = list();
    for i in list(1, 0, 1, 0, 1, 0, 0, 1) {
        big.push(list(i, string(i)));
    }
    big.sort_by(fn(x, y) { return x[0] < y[0]; });
    let seq = "";
    for p in big { seq = seq + p[1]; }
    println(seq);

    // Comparators that describe no order at all must not scramble the list.
    let same = list(1, 2, 3, 4, 5);
    same.sort_by(fn(a, b) { return false; });
    println(same);
    let same2 = list(1, 2, 3, 4, 5);
    same2.sort_by(fn(a, b) { return true; });
    println(same2);
    return 0;
}
"#,
        "bdac\n00001111\n[1, 2, 3, 4, 5]\n[1, 2, 3, 4, 5]\n",
    );
}

#[test]
fn argsort_returns_the_ascending_index_permutation() {
    // `argsort()[i]` is the ORIGINAL position of the element that belongs at
    // position i — a stable argsort of numbers and of strings, plus the natural
    // order `list.sort()` uses (so `xs.argsort()` agrees with `xs.sort()`).
    assert_both_engines(
        "argsort_basic",
        r#"fn main() {
    println(list(3, 1, 2).argsort());
    println(list(1, 0, 1, 0).argsort());
    println(list("pear", "fig", "banana").argsort());
    println(list().argsort());
    println(list(42).argsort());
    println(list(2.5, -1.0, 2.5, 0.0).argsort());
    // Cross-check against sort(): applying the permutation must equal sort().
    let xs = list(5, 2, 9, 2, 1);
    let perm = xs.argsort();
    let by_perm = list();
    for i in perm { by_perm.push(xs[i]); }
    let sorted = xs.clone();
    sorted.sort();
    println(by_perm == sorted);
    return 0;
}
"#,
        "[1, 2, 0]\n[1, 3, 0, 2]\n[2, 1, 0]\n[]\n[0]\n[1, 3, 0, 2]\ntrue\n",
    );
}

#[test]
fn sort_and_argsort_agree_with_natural_order() {
    // `sort`/`sorted` already existed; `argsort` must order the same way.
    assert_both_engines(
        "argsort_natural",
        r#"fn main() {
    let a = list(4, -2, 7, 0, 4);
    let b = a.clone();
    a.sort();
    let c = list();
    for i in b.argsort() { c.push(b[i]); }
    println(a == c);
    println(c);
    return 0;
}
"#,
        "true\n[-2, 0, 4, 4, 7]\n",
    );
}

#[test]
fn sort_by_propagates_a_comparator_throw() {
    // A comparator that throws must surface as a catchable error carrying the
    // THROWN value, and the list must be left untouched. This also covers the
    // VM bug where a throw inside any callback unwound past the `call_value_sync`
    // frame boundary and panicked the host on the next `Return` (Invariant #3:
    // user code must never panic the host).
    assert_both_engines(
        "sort_by_throw",
        r#"fn main() {
    let xs = list(3, 1, 2);
    try {
        xs.sort_by(fn(a, b) { if a == 1 { throw "comparator refused"; } return a < b; });
        println("NO ERROR");
    } catch (e) {
        println("caught: " + e);
    }
    println(xs);

    // The same path through the pre-existing higher-order methods.
    try {
        let r = list(1, 2).map(fn(x) { if x == 1 { throw "map boom"; } return x; });
        println(r);
    } catch (e) {
        println("caught: " + e);
    }
    return 0;
}
"#,
        "caught: comparator refused\n[3, 1, 2]\ncaught: map boom\n",
    );
}

#[test]
fn sort_by_rejects_a_comparator_with_a_bad_return_type() {
    // Only bool / int / float answers mean anything; a comparator returning a
    // string is a clean, catchable error (not a panic, and not silently
    // treated as "equal").
    assert_both_engines(
        "sort_by_badtype",
        r#"fn main() {
    let xs = list(3, 1, 2);
    try {
        xs.sort_by(fn(a, b) { return string(a); });
        println("NO ERROR");
    } catch (e) {
        println(e);
    }
    println(xs);
    // Arity: exactly one comparator argument.
    let ys = list(1, 2);
    try {
        ys.sort_by(fn(a, b) { return a < b; }, 1);
    } catch (e) {
        println(e);
    }
    try {
        ys.sorted_by();
    } catch (e) {
        println(e);
    }
    // Not a function at all.
    try {
        ys.sort_by(5);
    } catch (e) {
        println(e);
    }
    return 0;
}
"#,
        concat!(
            "sort_by: comparator must return a bool, int, or float, found string\n",
            "[3, 1, 2]\n",
            "sort_by expects 1 argument(s), got 2\n",
            "sorted_by expects 1 argument(s), got 0\n",
            "sort_by expects a function, found int\n",
        ),
    );
}

#[test]
fn sort_by_does_not_deadlock_on_a_self_referential_list() {
    // The list contains itself, and the comparator reaches back into it
    // (len / index / push all re-lock the same container). Invariant #3: no
    // collection mutex may be held across a callback, or this deadlocks.
    // Sorting a permutation of a snapshot keeps the lock out of the way.
    assert_both_engines(
        "sort_by_selfref",
        r#"fn main() {
    let xs = list(3, 1, 2);
    xs.push(xs);
    xs.sort_by(fn(a, b) {
        let av = a;
        let bv = b;
        if type_of(a) == "list" and len(a) > 0 { av = a[0]; }
        if type_of(b) == "list" and len(b) > 0 { bv = b[0]; }
        return av < bv;
    });
    println(len(xs));
    println(xs[0]);
    // The self-reference sorts last (its key is its own first cell, 3), and
    // the engines agree on the cycle-safe recursive display.
    println(xs);
    return 0;
}
"#,
        "4\n1\n[1, 2, 3, [...]]\n",
    );
}

#[test]
fn sort_by_survives_a_comparator_that_mutates_the_receiver() {
    // The comparator pops from the list it is sorting. No lock is held while
    // it runs, so the mutation completes without deadlocking; the write-back
    // happens only after the last comparator call, so the outcome is the
    // sorted snapshot either way — deterministic, never a panic.
    assert_both_engines(
        "sort_by_mutating",
        r#"fn main() {
    let ys = list(5, 4, 3, 2, 1);
    ys.sort_by(fn(a, b) { if len(ys) > 0 { ys.pop(); } return a < b; });
    println(len(ys));
    // `clear()` during the comparator is overwritten by the write-back: the
    // sorted snapshot is what the receiver ends up holding, so a comparator
    // that mutates the list cannot change the outcome.
    let zs = list(3, 1, 2);
    zs.sort_by(fn(a, b) { zs.clear(); return a < b; });
    println(len(zs));
    return 0;
}
"#,
        "5\n3\n",
    );
}

#[test]
fn string_format_precision_renders_fixed_decimals() {
    // `{:.N}` pins N fractional digits; `{}` behavior is unchanged.
    assert_both_engines(
        "fmt_precision",
        r#"fn main() {
    println("{:.2}".format(88.6875));
    println("{:.0}".format(2.5));
    println("{:.0}".format(3.5));
    println("{:.3}".format(1.0 / 3.0));
    println("{:.15}".format(1.0 / 3.0));
    println("{:.0}".format(5));
    println("{:.2}".format(-1.0));
    println("[{:.2}]".format(1.0));
    println("[{:.2}]".format(99.5));
    println("{:.1}".format(0.0));
    println("{:.6}".format(pow(2.0, 0.5)));
    // {} and {{ }} escapes keep their old meaning
    println("a={} b={}".format("x", 1));
    println("{} {} {}".format(1, 2.5, "s"));
    println("{{{}}}".format(7));
    println("no args".format());
    println("{}".format(list(1, 2)));
    println("{}{}{}".format(1, 2, 3));
    return 0;
}
"#,
        concat!(
            "88.69\n2\n4\n0.333\n0.333333333333333\n5\n-1.00\n[1.00]\n[99.50]\n0.0\n1.414214\n",
            "a=x b=1\n1 2.5 s\n{7}\nno args\n[1, 2]\n123\n",
        ),
    );
}

#[test]
fn string_format_rejects_bad_precision_specs() {
    for (name, src, needle) in [
        ("fmt_toobig", "fn main() { println(\"{:.16}\".format(1.0)); return 0; }", "above the maximum"),
        ("fmt_nodigit", "fn main() { println(\"{:.}\".format(1.0)); return 0; }", "not a precision"),
        ("fmt_alpha", "fn main() { println(\"{:.2a}\".format(1.0)); return 0; }", "not a precision"),
        ("fmt_colon", "fn main() { println(\"{:}\".format(1.0)); return 0; }", "not a valid placeholder"),
        ("fmt_baretype", "fn main() { println(\"{:.2}\".format(\"s\")); return 0; }", "expects a number, found string"),
        ("fmt_listtype", "fn main() { println(\"{:.2}\".format(list())); return 0; }", "expects a number, found list"),
        ("fmt_opttype", "fn main() { println(\"{:.2}\".format(none)); return 0; }", "expects a number, found option"),
        ("fmt_fewargs", "fn main() { println(\"{:.2}\".format()); return 0; }", "more placeholders than arguments"),
        ("fmt_manyargs", "fn main() { println(\"{}\".format(1, 2)); return 0; }", "more arguments than placeholders"),
    ] {
        for engine in ["tree", "vm"] {
            let err = run_src(engine, name, src).expect_err("bad precision must be rejected");
            assert!(
                err.contains(needle),
                "[{} {}] expected {:?}, got: {}",
                engine,
                name,
                needle,
                err
            );
        }
    }
}

#[test]
fn throw_inside_any_callback_is_caught_not_a_panic() {
    // 2026-10-20: a `throw` inside a callback unwound past the
    // `call_value_sync` frame boundary on the VM, which popped the frame stack
    // below `stop` and then PANICKED the host on the next `Return`
    // (Invariant #3: user code must never panic the host). The naive case is
    // worth pinning separately from sort_by: it reproduces with `map` alone.
    for engine in ["tree", "vm"] {
        let src = r#"fn main() {
    try {
        let r = list(1, 2, 3).map(fn(x) {
            if x == 2 { throw "boom in map"; }
            return x * 2;
        });
        println(r);
    } catch (e) {
        println("caught: " + e);
    }
    // Nested: a second try after the failed callback still works.
    try {
        throw "outer";
    } catch (e2) {
        println("outer caught: " + e2);
    }
    return 0;
}
"#;
        let out = run_src(engine, "throw_in_map", src)
            .unwrap_or_else(|e| panic!("[{}] throw_in_map run failed:/n{}", engine, e));
        assert_eq!(
            out, "caught: boom in map\nouter caught: outer\n",
            "[{}] engine output",
            engine
        );
    }
}

// ------------------------------------------------------------- vector & matrix

#[test]
fn vector_differential_all_three_engines() {
    assert_three_engines(
        "vector_diff",
        r#"fn main() {
    let v = vector([1.0, 2.0, 3.0, 4.0]);
    println(v.len());
    println(v[0]);
    println(v[-1]);
    v[-1] = 10.0;
    println(v[3]);
    let s = 0.0;
    for x in v {
        s = s + x;
    }
    println(s);
    let sl = v.slice(1, 3);
    println(sl);
    let v2 = vector([2.0, 2.0, 2.0, 2.0]);
    println(v.add(v2));
    println(v.sub(v2));
    println(v.mul(2.0));
    println(v.div(2.0));
    println(v.sum());
    println(v.mean());
    println(v.min());
    println(v.max());
    println(v.argmin());
    println(v.argmax());
    println(v.dot(v2));
    let v3 = vector([3.0, 4.0]);
    println(v3.norm());
    println(v3.cumsum());
    println(v.diff());
    let u = vector([9.0, 3.0, 7.0, 1.0]);
    println(u.argsort());
    u.sort();
    println(u);
    return 0;
}
"#,
        "4\n1.0\n4.0\n10.0\n16.0\nvector[2.0, 3.0]\nvector[3.0, 4.0, 5.0, 12.0]\nvector[-1.0, 0.0, 1.0, 8.0]\nvector[2.0, 4.0, 6.0, 20.0]\nvector[0.5, 1.0, 1.5, 5.0]\n16.0\n4.0\n1.0\n10.0\n0\n3\n32.0\n5.0\nvector[3.0, 7.0]\nvector[1.0, 1.0, 7.0]\n[3, 1, 2, 0]\nvector[1.0, 3.0, 7.0, 9.0]\n",
    );
}

#[test]
fn matrix_algebra_differential_all_three_engines() {
    assert_three_engines(
        "matrix_diff",
        r#"fn main() {
    let m = matrix_from_list([[4.0, 7.0], [2.0, 6.0]]);
    println(m.rows());
    println(m.cols());
    println(m.transpose());
    println(m.scale(2.0));
    let m2 = matrix([[1.0, 1.0], [1.0, 1.0]]);
    println(m.sub(m2));
    println(m.hadamard(m2));
    println(m.trace());
    println(m.sum(null));
    println(m.sum(0));
    println(m.sum(1));
    println(m.mean(null));
    println(m.mean(0));
    println(m.mean(1));
    println(m.det());

    // Inverse check: m * inv(m) == I
    let inv = m.inverse();
    let ident = m.mul(inv);
    let ok_inv = (abs(ident.get(0, 0) - 1.0) < 1e-9) and
                 (abs(ident.get(0, 1)) < 1e-9) and
                 (abs(ident.get(1, 0)) < 1e-9) and
                 (abs(ident.get(1, 1) - 1.0) < 1e-9);
    println(ok_inv);

    // Solve check: m * x = b => x
    let b = vector([18.0, 14.0]);
    let x = m.solve(b);
    let ok_solve = (abs(x[0] - 1.0) < 1e-9) and (abs(x[1] - 2.0) < 1e-9);
    println(ok_solve);

    // Solve with matrix B:
    let bm = matrix([[18.0], [14.0]]);
    let xm = m.solve(bm);
    let ok_solve_m = (abs(xm.get(0, 0) - 1.0) < 1e-9) and (abs(xm.get(1, 0) - 2.0) < 1e-9);
    println(ok_solve_m);

    // Symmetric Eigenvalues:
    let s = matrix([[2.0, 1.0], [1.0, 2.0]]);
    let res = s.eigen_sym();
    let vals = res["values"];
    let vecs = res["vectors"];
    let ok_eig = (abs(vals[0] - 1.0) < 1e-9) and (abs(vals[1] - 3.0) < 1e-9);
    println(ok_eig);
    let vt = vecs.transpose();
    let v_ident = vt.mul(vecs);
    let ok_ortho = (abs(v_ident.get(0, 0) - 1.0) < 1e-9) and
                   (abs(v_ident.get(1, 1) - 1.0) < 1e-9) and
                   (abs(v_ident.get(0, 1)) < 1e-9) and
                   (abs(v_ident.get(1, 0)) < 1e-9);
    println(ok_ortho);

    return 0;
}
"#,
        "2\n2\nmatrix[[4.0, 2.0], [7.0, 6.0]]\nmatrix[[8.0, 14.0], [4.0, 12.0]]\nmatrix[[3.0, 6.0], [1.0, 5.0]]\nmatrix[[4.0, 7.0], [2.0, 6.0]]\n10.0\n19.0\nvector[6.0, 13.0]\nvector[11.0, 8.0]\n4.75\nvector[3.0, 6.5]\nvector[5.5, 4.0]\n10.0\ntrue\ntrue\ntrue\ntrue\ntrue\n",
    );
}

#[test]
fn matrix_singular_differential_catch() {
    assert_both_engines(
        "singular_catch",
        r#"fn main() {
    let m = matrix([[1.0, 2.0], [2.0, 4.0]]);
    try {
        let inv = m.inverse();
        println("fail: did not throw");
    } catch (e) {
        println("caught inv: " + e);
    }
    try {
        let x = m.solve(vector([1.0, 2.0]));
        println("fail: did not throw");
    } catch (e2) {
        println("caught solve: " + e2);
    }
    return 0;
}
"#,
        "caught inv: matrix is singular\ncaught solve: matrix is singular\n",
    );
}

#[test]
fn bytes_differential_all_three_engines() {
    assert_three_engines(
        "bytes_diff",
        r#"fn main() {
    let b = bytes("hello");
    println(b.len());
    println(b[0]);
    println(b[-1]);
    b[0] = 72;
    println(b.to_string().unwrap());
    let hex = b.to_hex();
    println(hex);
    let from_h = bytes().from_hex(hex);
    println(from_h == b);
    let b64 = b.to_base64();
    println(b64);
    let from_b = bytes().from_base64(b64);
    println(from_b == b);
    let sl = b.slice(1, 4);
    println(sl.to_string().unwrap());
    let list_b = b.to_list();
    println(list_b.len());
    return 0;
}
"#,
        "5\n104\n111\nHello\n48656c6c6f\ntrue\nSGVsbG8=\ntrue\nell\n5\n",
    );
}

#[test]
fn npy_roundtrip_differential_all_three_engines() {
    assert_three_engines(
        "npy_diff",
        r#"fn main() {
    let v = vector([10.5, 20.25, 30.125]);
    v.save_npy("target/difftest_vec.npy").unwrap();
    let v2 = load_npy("target/difftest_vec.npy").unwrap();
    println(v == v2);

    let m = matrix([[1.0, 2.5], [3.25, 4.125]]);
    m.save_npy("target/difftest_mat.npy").unwrap();
    let m2 = load_npy("target/difftest_mat.npy").unwrap();
    println(m == m2);

    return 0;
}
"#,
        "true\ntrue\n",
    );
}

#[test]
fn csv_differential_all_three_engines() {
    assert_three_engines(
        "csv_diff",
        r#"fn main() {
    let text = "name,age,active,score\n\"Doe, John\",35,true,98.5\nJane,28,false,NA\n";
    let res = parse_csv(text, null).unwrap();
    let h = res["headers"];
    let rows = res["rows"];
    println(h.len());
    println(rows.len());
    println(rows[0][0]);
    println(rows[0][1]);
    println(rows[0][2]);
    println(rows[0][3]);
    println(rows[1][2]);
    println(rows[1][3] == null);
    return 0;
}
"#,
        "4\n2\nDoe, John\n35\ntrue\n98.5\nfalse\ntrue\n",
    );
}

#[test]
fn regex_differential_all_three_engines() {
    assert_three_engines(
        "regex_diff",
        r#"fn main() {
    println(regex_match("\\d+", "abc123xyz"));
    println(regex_match("^\\d+$", "abc123xyz"));
    println(regex_match("^\\d+$", "987654"));

    let f = regex_find("[a-z]+", "123 orange 456");
    println(f.unwrap());

    let all = regex_find_all("\\d+", "a10 b20 c30");
    println(all.len());
    println(all[0]);
    println(all[1]);
    println(all[2]);

    let rep = regex_replace("\\d+", "num1_num2_num3", "X");
    println(rep);
    return 0;
}
"#,
        "true\nfalse\ntrue\norange\n3\n10\n20\n30\nnumX_numX_numX\n",
    );
}

#[test]
fn datetime_differential_all_three_engines() {
    assert_three_engines(
        "datetime_diff",
        r#"fn main() {
    let p = datetime_parts(0);
    println(p["year"]);
    println(p["month"]);
    println(p["day"]);
    println(p["hour"]);
    println(p["minute"]);
    println(p["second"]);
    println(p["weekday"]);

    let ts = datetime_from_parts(2026, 10, 20, 16, 45, 30);
    let p2 = datetime_parts(ts);
    println(p2["year"] == 2026 and p2["month"] == 10 and p2["day"] == 20);
    println(p2["hour"] == 16 and p2["minute"] == 45 and p2["second"] == 30);

    let fmt = format_datetime(0, "%Y-%m-%d %H:%M:%S");
    println(fmt);
    return 0;
}
"#,
        "1970\n1\n1\n0\n0\n0\n4\ntrue\ntrue\n1970-01-01 00:00:00\n",
    );
}

#[test]
fn callable_map_fields_differential_both_engines() {
    assert_both_engines(
        "callable_map_fields",
        r#"fn add(a, b) {
    return a + b;
}

fn main() {
    let m = map(
        "compute", add,
        "mul", fn(x, y) { return x * y; },
        "val", 42
    );
    println(m.compute(3, 4));
    println(m.mul(5, 6));
    println(m.val);
    try {
        m.val(1);
    } catch (e) {
        println("not callable caught");
    }
    try {
        m.nonexistent(1);
    } catch (e) {
        println("missing method caught");
    }
    return 0;
}
"#,
        "7\n30\n42\nnot callable caught\nmissing method caught\n",
    );
}


