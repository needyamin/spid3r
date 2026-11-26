use spid3r_lang::wasm::{
    spid3r_check_json, spid3r_clear_modules_json, spid3r_eval_json, spid3r_format_json,
    spid3r_register_module_json, spid3r_session_eval_json, spid3r_session_reset_json,
    spid3r_version_json,
};

#[test]
fn test_wasm_version() {
    let json = spid3r_version_json();
    assert!(json.contains("\"version\""));
    assert!(json.contains("\"language_version\""));
}

#[test]
fn test_wasm_check_valid() {
    let code = "let x = 42; println(x);";
    let res = spid3r_check_json(code);
    assert_eq!(res, "{\"ok\":true}");
}

#[test]
fn test_wasm_check_syntax_error() {
    let code = "let x = ;";
    let res = spid3r_check_json(code);
    assert!(res.contains("\"ok\":false"));
    assert!(res.contains("\"phase\":\"Parser\""));
}

#[test]
fn test_wasm_eval_tree_and_vm() {
    let code = r#"
        let greeting = "Hello from WebAssembly!";
        println(greeting);
        let sum = 10 + 20;
        println(sum);
    "#;

    let res_tree = spid3r_eval_json(code, "tree");
    assert!(res_tree.contains("\"ok\":true"));
    assert!(res_tree.contains("Hello from WebAssembly!"));
    assert!(res_tree.contains("30"));

    let res_vm = spid3r_eval_json(code, "vm");
    assert!(res_vm.contains("\"ok\":true"));
    assert!(res_vm.contains("Hello from WebAssembly!"));
    assert!(res_vm.contains("30"));
}

#[test]
fn test_wasm_eval_runtime_error() {
    let code = "throw \"Browser explosion\";";
    let res = spid3r_eval_json(code, "tree");
    assert!(res.contains("\"ok\":false"));
    assert!(res.contains("Browser explosion"));
}

#[test]
fn test_wasm_format() {
    let unformatted = "let   a=1+2;\nprintln(  a  );";
    let res = spid3r_format_json(unformatted);
    assert!(res.contains("\"ok\":true"));
    assert!(res.contains("let a = 1 + 2"));
}

#[test]
fn test_wasm_eval_fn_main_in_string_or_comment() {
    // SPID3R-2026-011: A top-level script mentioning "fn main" inside a string
    // literal or comment must still synthesize an empty `fn main() {}` and run.
    let code = r#"
        // Example showing how fn main() works:
        println("You can define fn main() or write top-level code!");
    "#;
    for engine in ["tree", "vm"] {
        let res = spid3r_eval_json(code, engine);
        assert!(res.contains("\"ok\":true"), "[{}] got: {}", engine, res);
        assert!(
            res.contains("You can define fn main() or write top-level code!"),
            "[{}] got: {}",
            engine,
            res
        );
    }
}

#[test]
fn test_wasm_virtual_modules_and_persistent_session() {
    assert_eq!(spid3r_clear_modules_json(), "{\"ok\":true}");
    let reg = spid3r_register_module_json(
        "kit/greeter",
        "public fn hello(who) { return \"hi \" + who; }\n",
    );
    assert_eq!(reg, "{\"ok\":true}");

    let prog = "import \"kit/greeter\" as g;\nprintln(g.hello(\"wasm\"));\n";
    for engine in ["tree", "vm"] {
        let res = spid3r_eval_json(prog, engine);
        assert!(res.contains("\"ok\":true"), "[{engine}] {res}");
        assert!(res.contains("hi wasm"), "[{engine}] {res}");
    }

    for engine in ["tree", "vm"] {
        assert_eq!(spid3r_session_reset_json(), "{\"ok\":true}");
        let r1 = spid3r_session_eval_json(
            "let counter = 10;\nfn bump(n) { counter += n; return counter; }\n",
            engine,
        );
        assert!(r1.contains("\"ok\":true"), "[{engine}] r1: {r1}");
        let r2 = spid3r_session_eval_json(
            "import \"kit/greeter\" as g;\nprintln(g.hello(\"sess\") + \":\" + string(bump(5)));\n",
            engine,
        );
        assert!(r2.contains("\"ok\":true"), "[{engine}] r2: {r2}");
        assert!(r2.contains("hi sess:15"), "[{engine}] r2: {r2}");
        let r3 = spid3r_session_eval_json("println(counter);\n", engine);
        assert!(r3.contains("15"), "[{engine}] r3: {r3}");
    }
    assert_eq!(spid3r_session_reset_json(), "{\"ok\":true}");
    assert_eq!(spid3r_clear_modules_json(), "{\"ok\":true}");
}
