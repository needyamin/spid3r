//! Embedding API integration tests (spid3r::embed).
//!
//! These complement the unit tests in src\embed.rs: module resolution from
//! host-provided directories, engine reuse, exit-code propagation, stdlib
//! override semantics, eprint capture, deep-data runs on the big-stack worker,
//! capture isolation between concurrent engines, and the value conversion
//! helpers.

use spid3r::embed::{conv, Engine};

#[test]
fn run_returns_exit_code_and_captured_output() {
    let engine = Engine::new();
    let out = engine
        .run(
            "t",
            "fn main() { println(\"hello\"); eprint(\"to-err\"); return 7; }",
        )
        .expect("run");
    assert_eq!(out.exit_code, 7);
    assert!(out.output.contains("hello"), "{}", out.output);
    // eprint goes through the same capture channel (stdout + eprint stream).
    assert!(out.output.contains("to-err"), "{}", out.output);
}

#[test]
fn engine_is_reusable_across_runs() {
    let engine = Engine::new().set_global("base", conv::from_i64(10));
    let a = engine
        .run("a", "fn main() { return base + 1; }")
        .expect("run a");
    let b = engine
        .run("b", "fn main() { return base + 2; }")
        .expect("run b");
    assert_eq!(a.exit_code, 11);
    assert_eq!(b.exit_code, 12);
}

#[test]
fn host_functions_override_stdlib_of_the_same_name() {
    let engine = Engine::new().register("len", |_args| Ok(conv::from_i64(99)));
    let out = engine
        .run("t", "fn main() { println(len(list(1, 2))); return 0; }")
        .expect("run");
    assert!(out.output.contains("99"), "{}", out.output);
}

#[test]
fn modules_resolve_from_host_directories() {
    let dir = std::env::temp_dir().join(format!("spid3r_embed_mod_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("embed_lib.matrix"),
        "public fn twice(x) {\n    return x * 2;\n}\n",
    )
    .unwrap();
    let engine = Engine::new().add_module_dir(dir.clone());
    let out = engine
        .run(
            "main",
            "import \"embed_lib\";\nfn main() { println(twice(21)); return 0; }",
        )
        .expect("run");
    assert!(out.output.contains("42"), "{}", out.output);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deep_data_builds_run_on_the_embed_worker() {
    // 200k-deep nested list: exercises the F4 amortized GC + iterative drop
    // inside the embedding worker (the suite must never hang — the run itself
    // is the deadline, bounded well under a second at this depth).
    let engine = Engine::new();
    let out = engine
        .run(
            "t",
            "fn main() {\n    let l = list(1);\n    let i = 0;\n    while i < 200000 {\n        let n = list();\n        n.push(l);\n        l = n;\n        i = i + 1;\n    }\n    println(gc_live_cells() > 200000);\n    return 0;\n}\n",
        )
        .expect("run");
    assert!(out.output.contains("true"), "{}", out.output);
}

#[test]
fn concurrent_engines_do_not_cross_captured_output() {
    // Capture is thread-local: two engines running concurrently in different
    // host threads must not see each other's output.
    let e1 = Engine::new();
    let e2 = Engine::new();
    let h1 = std::thread::spawn(move || {
        e1.run("t1", "fn main() { println(\"engine-one\"); return 0; }")
            .unwrap()
    });
    let h2 = std::thread::spawn(move || {
        e2.run("t2", "fn main() { println(\"engine-two\"); return 0; }")
            .unwrap()
    });
    let (r1, r2) = (h1.join().unwrap(), h2.join().unwrap());
    assert!(r1.output.contains("engine-one") && !r1.output.contains("engine-two"), "{}", r1.output);
    assert!(r2.output.contains("engine-two") && !r2.output.contains("engine-one"), "{}", r2.output);
}

#[test]
fn runtime_failure_carries_diagnostic_and_partial_output() {
    let engine = Engine::new();
    let err = engine
        .run(
            "t",
            "fn main() { println(\"step1\"); assert(false, \"nope\"); return 0; }",
        )
        .unwrap_err();
    assert_eq!(err.diagnostic.phase, spid3r::diagnostics::Phase::Runtime);
    assert!(err.output.contains("step1"), "{}", err.output);
    assert!(err.diagnostic.message.contains("nope"), "{}", err.diagnostic.message);
}

#[test]
fn check_failures_name_the_phase() {
    let engine = Engine::new();
    for (src, phase) in [
        ("fn main( { return 0; }", spid3r::diagnostics::Phase::Parser),
        (
            "fn main() { return unknown_thing(); }",
            spid3r::diagnostics::Phase::Semantic,
        ),
    ] {
        let err = engine.run("t", src).unwrap_err();
        assert_eq!(err.diagnostic.phase, phase, "{}", err.diagnostic.message);
        assert!(err.output.is_empty());
    }
}

#[test]
fn value_conversions_round_trip() {
    // Host side: build values, run a script over them, read results back out
    // of the arguments of a host callback.
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<spid3r::Value>::new()));
    let s = seen.clone();
    let engine = Engine::new()
        .set_global(
            "payload",
            conv::from_list(vec![
                conv::from_i64(3),
                conv::from_f64(1.5),
                conv::from_bool(true),
                conv::from_string("tag"),
            ]),
        )
        .register("observe", move |args| {
            s.lock().unwrap().extend(args.iter().cloned());
            Ok(spid3r::Value::Null)
        });
    let out = engine
        .run(
            "t",
            "fn main() { for x in payload { observe(x); } return 0; }",
        )
        .expect("run");
    assert_eq!(out.exit_code, 0);
    let got = seen.lock().unwrap();
    assert_eq!(conv::as_i64(&got[0]), Some(3));
    assert_eq!(conv::as_f64(&got[1]), Some(1.5));
    assert_eq!(conv::as_bool(&got[2]), Some(true));
    assert_eq!(conv::as_str(&got[3]), Some("tag"));
    // The host-built list round-trips through the language.
    let host_list = conv::from_list(vec![conv::from_i64(1), conv::from_i64(2)]);
    assert_eq!(conv::as_list(&host_list).unwrap().len(), 2);
}

#[test]
fn host_globals_are_dynamic_and_shadowable() {
    // Host globals are `Any`: a script may pass them anywhere and may shadow
    // them with `let` (the documented contract).
    let engine = Engine::new().set_global("thing", conv::from_i64(1));
    let out = engine
        .run(
            "t",
            "fn main() { let thing = \"shadowed\"; println(thing); return 0; }",
        )
        .expect("run");
    assert!(out.output.contains("shadowed"), "{}", out.output);
}
