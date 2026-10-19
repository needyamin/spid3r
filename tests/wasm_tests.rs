use spid3r::wasm::{spid3r_check_json, spid3r_eval_json, spid3r_format_json, spid3r_version_json};

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
