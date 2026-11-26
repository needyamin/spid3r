//! Review verification tests for 2026-10-27 audit findings.

use spid3r_lang::fmt;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::modules;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::vm::Vm;
use std::sync::Arc;

fn check_err(src: &str, needle: &str) {
    let program = match parser::parse_file(src, Arc::from("test.matrix")) {
        Ok(p) => p,
        Err(e) => panic!("parse error: {:?}", e),
    };
    match semantic::check_file(&program, true, "test.matrix") {
        Err(d) => assert!(
            d.message.contains(needle),
            "error message {:?} does not contain expected substring {:?}",
            d.message,
            needle
        ),
        Ok(()) => panic!(
            "expected check error containing {:?}, but check passed",
            needle
        ),
    }
}

fn run_both_engines(src: &str) {
    let program = parser::parse_file(src, Arc::from("test.matrix")).expect("parse");
    semantic::check_file(&program, true, "test.matrix").expect("check");

    let interp = Interp::new(Arc::from("test.matrix"), modules::default_module_dirs());
    let tree_res = interp.run_program(&program);
    assert_eq!(tree_res.expect("tree run"), 0, "tree engine non-zero exit");

    let mut vm = Vm::new(Arc::from("test.matrix"), modules::default_module_dirs());
    let vm_res = vm.run_program(&program);
    assert_eq!(vm_res.expect("vm run"), 0, "vm engine non-zero exit");
}

#[test]
fn subclass_cannot_read_parent_private_field() {
    let src = r#"
class Base {
    private let secret;
    public fn new() {
        self.secret = 7;
    }
}
class Sub extends Base {
    public fn read(self) {
        return self.secret;
    }
}
fn main() {
    let s = new Sub();
    println(s.read());
    return 0;
}
"#;
    check_err(src, "field 'secret' of class 'Base' is private");
}

#[test]
fn subclass_cannot_write_parent_private_field() {
    let src = r#"
class Base {
    private let secret;
    public fn new() {
        self.secret = 7;
    }
}
class Sub extends Base {
    public fn mutate(self) {
        self.secret = 99;
    }
}
fn main() {
    let s = new Sub();
    s.mutate();
    return 0;
}
"#;
    check_err(src, "field 'secret' of class 'Base' is private");
}

#[test]
fn subclass_cannot_call_parent_private_method() {
    let src = r#"
class Base {
    private fn hidden(self) {
        return 42;
    }
}
class Sub extends Base {
    public fn call_hidden(self) {
        return self.hidden();
    }
}
fn main() {
    let s = new Sub();
    s.call_hidden();
    return 0;
}
"#;
    check_err(src, "method 'hidden' of class 'Base' is private");
}

#[test]
fn subclass_cannot_call_parent_private_method_via_super() {
    let src = r#"
class Base {
    private fn hidden(self) {
        return 42;
    }
}
class Sub extends Base {
    public fn call_super_hidden(self) {
        return super.hidden();
    }
}
fn main() {
    let s = new Sub();
    s.call_super_hidden();
    return 0;
}
"#;
    check_err(src, "method 'hidden' of class 'Base' is private");
}

#[test]
fn min_int_literal_with_and_without_underscores() {
    let src = r#"
fn main() {
    let a = -9223372036854775808;
    let b = -9_223_372_036_854_775_808;
    assert_eq(a, b);
    assert_eq(string(a), "-9223372036854775808");
    return 0;
}
"#;
    run_both_engines(src);
}

#[test]
fn yield_in_expression_position_and_in_main() {
    let src = r#"
fn main() {
    let res = yield();
    assert_eq(res, null);
    println("after yield");
    return 0;
}
"#;
    run_both_engines(src);
}

#[test]
fn graph_dijkstra_rejects_negative_edge_weights() {
    let src = r#"
fn main() {
    let g = graph("directed");
    g.add_node("a");
    g.add_node("b");
    g.add_edge("a", "b", -2.5);
    try {
        g.dijkstra("a", "b");
        assert(false);
    } catch (e) {
        assert(string(e).contains("negative edge weights"));
    }
    return 0;
}
"#;
    run_both_engines(src);
}

#[test]
fn graph_cycle_and_has_cycle_aliases() {
    let src = r#"
fn main() {
    let g = graph("directed");
    g.add_node("x");
    g.add_node("y");
    g.add_edge("x", "y", 1.0);
    g.add_edge("y", "x", 1.0);
    assert(g.cycle());
    assert(g.has_cycle());
    return 0;
}
"#;
    run_both_engines(src);
}

#[test]
fn multipart_build_and_parse_via_module_import() {
    let src = r#"
import "multipart" as mp;

fn main() {
    let parts = list(
        map("name", "title", "data", bytes("Audit Report")),
        map("name", "file", "filename", "notes.txt", "content_type", "text/plain", "data", bytes("All tests passed"))
    );
    let built = mp.multipart_build("testboundary", parts).unwrap();
    let parsed_res = mp.multipart_parse(built, "multipart/form-data; boundary=testboundary").unwrap();
    let parsed = parsed_res["parts"];
    assert_eq(len(parsed), 2);
    assert_eq(parsed[0]["name"], "title");
    assert_eq(parsed[0]["text"], "Audit Report");
    assert_eq(parsed[1]["filename"], "notes.txt");
    assert_eq(parsed[1]["text"], "All tests passed");
    return 0;
}
"#;
    run_both_engines(src);
}

#[test]
fn fmt_preserves_ampamp_and_pipepipe() {
    let src = "fn main() {\n    let ok = true && not false || false;\n    return 0;\n}\n";
    let formatted = fmt::format_source(src, &Arc::from("f.matrix")).expect("fmt");
    assert!(formatted.contains("&&"));
    assert!(formatted.contains("||"));
}
