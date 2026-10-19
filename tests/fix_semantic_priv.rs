//! F6 + F12 + F5 regression tests (semantic checker — shared by both engines).
//!
//! F6: class privacy used to be enforced only on bare-Ident receivers, so
//! `new C().hide()` on a private method ran and `new C().hidden_field` silently
//! yielded null; field WRITES had no privacy check at all. The checker now
//! resolves the receiver's static class for any expression (via the same
//! best-effort type computer the gradual typing uses) and applies the
//! identical rule. Receivers whose class is not statically known stay
//! dynamically unchecked (documented gradual behavior).
//!
//! F12: imported bindings were inserted with a raw scope insert, so a
//! same-named local declaration silently shadowed them. Both directions now
//! produce a diagnostic; ordinary nested-scope shadowing stays legal.
//!
//! F5: gradual typing was bypassable through rebound function values and
//! nested fn declarations (`let g = f; g("hello")` ran with `fn f(x: int)`).
//! Function values now carry their signature, so the checker validates calls
//! through any statically-typed callee. Unannotated functions stay fully
//! dynamic.
//!
//! The checker is shared by both engines, so every rejection below holds for
//! `run --engine tree` and `run --engine vm` alike; allowed programs are
//! additionally smoke-run through the VM CLI to pin that.

use spid3r::diagnostics::Diagnostic;
use spid3r::interpreter::Interp;
use spid3r::parser;
use spid3r::semantic;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ------------------------------------------------------------- in-process

/// Check only (no execution) — the checker gates both engines.
fn check(src: &str) -> Result<(), Diagnostic> {
    let program = parser::parse_file(src, Arc::from("t.matrix"))?;
    semantic::check_file(&program, true, "t.matrix")
}

fn expect_check_err(src: &str, needle: &str) {
    match check(src) {
        Err(d) => assert!(
            d.message.contains(needle),
            "error {:?} does not mention {}",
            d.message,
            needle
        ),
        Ok(()) => panic!("expected check failure mentioning {}", needle),
    }
}

/// Check + run on the tree engine in-process (allowed programs must run).
fn run(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("t.matrix"))?;
    semantic::check_file(&program, true, "t.matrix")?;
    let interp = Interp::new(Arc::from("t.matrix"), vec![]);
    interp.run_program(&program)
}

fn expect_ok(src: &str) {
    run(src).unwrap_or_else(|d| panic!("expected success, got: {}", d));
}

// ------------------------------------------------------------- CLI helper

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run_vm_with_deadline(src: &str, name: &str) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!("spid3r_f6_{}.matrix", name));
    std::fs::write(&path, src).map_err(|e| format!("write: {}", e))?;
    run_vm_file_with_deadline(&path, name)
}

fn run_vm_file_with_deadline(path: &Path, _name: &str) -> Result<String, String> {
    let mut child = Command::new(bin())
        .args(["run", "--engine", "vm"])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn failed: {}", e))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut s) = child.stdout.take() {
                    let _ = s.read_to_string(&mut out);
                }
                let mut err = String::new();
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut err);
                }
                break Ok((status, out, err));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err("vm run did not finish within 15s".to_string());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => break Err(format!("wait failed: {}", e)),
        }
    };
    let _ = std::fs::remove_file(path);
    let (status, out, err) = outcome?;
    if !status.success() {
        return Err(format!("exit {:?}: {}", status.code(), err));
    }
    Ok(out)
}

// ------------------------------------------------------------------- F6

const CLASS: &str = r#"
class C {
    let hidden_field;
    public let pub_field;
    fn new() {
        self.hidden_field = 7;
        self.pub_field = 8;
        return null;
    }
    private fn hide(self) {
        return 1;
    }
    public fn show(self) {
        return self.hide() + 1;
    }
}
"#;

#[test]
fn private_method_via_new_expr_is_rejected() {
    // Was the F6 hole: ran and printed 1 on both engines.
    expect_check_err(
        &format!(
            "{} fn main() {{ println(new C().hide()); return 0; }}",
            CLASS
        ),
        "method 'hide' of class 'C' is private",
    );
}

#[test]
fn private_field_read_via_new_expr_is_rejected() {
    // Was the F6 hole: silently printed null.
    expect_check_err(
        &format!(
            "{} fn main() {{ println(new C().hidden_field); return 0; }}",
            CLASS
        ),
        "field 'hidden_field' of class 'C' is private",
    );
}

#[test]
fn private_field_write_via_ident_receiver_is_rejected() {
    // Was entirely unchecked: field writes had no privacy check at all.
    expect_check_err(
        &format!(
            "{} fn main() {{ let c = new C(); c.hidden_field = 9; return 0; }}",
            CLASS
        ),
        "field 'hidden_field' of class 'C' is private",
    );
}

#[test]
fn private_field_write_via_new_expr_is_rejected() {
    expect_check_err(
        &format!(
            "{} fn main() {{ new C().hidden_field = 9; return 0; }}",
            CLASS
        ),
        "field 'hidden_field' of class 'C' is private",
    );
}

#[test]
fn private_members_via_ident_receiver_still_rejected() {
    // No regression on the pre-existing Ident-receiver checks.
    expect_check_err(
        &format!(
            "{} fn main() {{ let c = new C(); println(c.hide()); return 0; }}",
            CLASS
        ),
        "method 'hide' of class 'C' is private",
    );
    expect_check_err(
        &format!(
            "{} fn main() {{ let c = new C(); println(c.hidden_field); return 0; }}",
            CLASS
        ),
        "field 'hidden_field' of class 'C' is private",
    );
}

#[test]
fn public_members_and_in_class_use_still_work() {
    expect_ok(&format!(
        "{} fn main() {{ let c = new C(); println(c.show()); println(c.pub_field); return 0; }}",
        CLASS
    ));
    // show() calls the private hide() through self inside the class.
    let out = run_vm_with_deadline(
        &format!(
            "{} fn main() {{ let c = new C(); println(c.show()); return 0; }}",
            CLASS
        ),
        "pub_ok",
    )
    .unwrap_or_else(|e| panic!("{}", e));
    assert!(out.contains('2'), "vm: {}", out);
}

#[test]
fn dynamic_receivers_stay_unchecked() {
    // A factory without a return annotation has no static type: the receiver
    // class is unknown, so the access is unchecked (documented gradual
    // behavior, matching the pre-fix Ident-receiver coverage boundary).
    expect_ok(&format!(
        "{} fn make() {{ return new C(); }} fn main() {{ println(make().hide()); return 0; }}",
        CLASS
    ));
}

// ------------------------------------------------------------------- F12

#[test]
fn local_fn_shadowing_an_import_is_rejected() {
    let dir = std::env::temp_dir().join(format!("spid3r_f12_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mod_path = dir.join("shadow_mod.matrix");
    std::fs::write(&mod_path, "public fn helper() {\n    return 1;\n}\n").unwrap();
    let main_path = dir.join("main.matrix");
    let src = "import \"shadow_mod\";\nfn helper() {\n    return 2;\n}\nfn main() {\n    return 0;\n}\n";
    std::fs::write(&main_path, src).unwrap();
    let program = parser::parse_file(src, Arc::from(main_path.to_str().unwrap())).unwrap();
    let err = semantic::check_file(&program, true, main_path.to_str().unwrap()).unwrap_err();
    assert!(
        err.message.contains("local 'helper' shadows an imported binding"),
        "got: {}",
        err.message
    );
    // Same for a plain let.
    let src2 = "import \"shadow_mod\";\nlet helper = 5;\nfn main() {\n    return 0;\n}\n";
    std::fs::write(&main_path, src2).unwrap();
    let program2 = parser::parse_file(src2, Arc::from(main_path.to_str().unwrap())).unwrap();
    let err2 = semantic::check_file(&program2, true, main_path.to_str().unwrap()).unwrap_err();
    assert!(
        err2.message.contains("local 'helper' shadows an imported binding"),
        "got: {}",
        err2.message
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn import_shadowing_a_known_local_is_rejected_in_repl_mode() {
    // REPL direction: a name bound by an earlier chunk (seeded via `known`),
    // then imported by a later chunk — diagnosed at the import statement.
    let dir = std::env::temp_dir().join(format!("spid3r_f12b_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mod_path = dir.join("shadow_mod.matrix");
    std::fs::write(&mod_path, "public fn helper() {\n    return 1;\n}\n").unwrap();
    let main_path = dir.join("main.matrix");
    let src = "import \"shadow_mod\";\nfn main() {\n    return 0;\n}\n";
    std::fs::write(&main_path, src).unwrap();
    let program = parser::parse_file(src, Arc::from(main_path.to_str().unwrap())).unwrap();
    let err = semantic::check_repl_in(
        &program,
        &["helper".to_string()],
        main_path.to_str().unwrap(),
    )
    .unwrap_err();
    assert!(
        err.message.contains("import 'helper' shadows a local binding"),
        "got: {}",
        err.message
    );
    // Without the earlier local, the same import must stay clean.
    let program2 = parser::parse_file(src, Arc::from(main_path.to_str().unwrap())).unwrap();
    semantic::check_repl_in(&program2, &[], main_path.to_str().unwrap())
        .unwrap_or_else(|d| panic!("clean import must pass, got: {}", d));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unrelated_names_and_nested_shadowing_still_work() {
    let dir = std::env::temp_dir().join(format!("spid3r_f12c_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mod_path = dir.join("shadow_mod.matrix");
    std::fs::write(&mod_path, "public fn helper() {\n    return 1;\n}\n").unwrap();
    let main_path = dir.join("main.matrix");
    // An import whose name is shadowed only in a NESTED scope stays legal,
    // exactly like ordinary local shadowing; importing module code (helper()
    // from the module, called through a local wrapper) still resolves.
    let src = "import \"shadow_mod\";\nfn main() {\n    fn helper() {\n        return 3;\n    }\n    println(helper());\n    println(module_helper());\n    return 0;\n}\nfn module_helper() {\n    return helper();\n}\n";
    std::fs::write(&main_path, src).unwrap();
    let program = parser::parse_file(src, Arc::from(main_path.to_str().unwrap())).unwrap();
    semantic::check_file(&program, true, main_path.to_str().unwrap())
        .unwrap_or_else(|d| panic!("nested shadowing must stay legal, got: {}", d));
    // And it runs: inside main the nested local helper shadows the import (3);
    // module_helper calls the IMPORTED helper (1).
    let out = run_vm_file_with_deadline(&main_path, "f12_ok").unwrap_or_else(|e| panic!("{}", e));
    assert!(out.contains('3') && out.contains('1'), "vm: {}", out);
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------- F5

#[test]
fn direct_call_type_checking_still_works() {
    // Pre-existing behavior, pinned so the F5 work cannot weaken it.
    expect_check_err(
        r#"fn f(x: int) -> int { return 42; } fn main() { println(f("s")); return 0; }"#,
        "argument 1 of 'f' expects int, got string",
    );
}

#[test]
fn rebound_function_value_is_type_checked() {
    // Was the F5 bypass: ran and printed 42 on both engines.
    expect_check_err(
        r#"fn f(x: int) -> int { return 42; } fn main() { let g = f; println(g("s")); return 0; }"#,
        "argument 1 of 'g' expects int, got string",
    );
}

#[test]
fn nested_fn_call_is_type_checked() {
    // Was the F5 bypass: nested declarations were never checked.
    expect_check_err(
        r#"fn main() { fn h(a: int) -> int { return a; } println(h("s")); return 0; }"#,
        "argument 1 of 'h' expects int, got string",
    );
}

#[test]
fn rebound_function_arity_is_checked() {
    expect_check_err(
        r#"fn f(x: int) -> int { return x; } fn main() { let g = f; println(g(1, 2)); return 0; }"#,
        "expects 1 argument(s), got 2",
    );
}

#[test]
fn annotated_lambda_via_let_is_type_checked() {
    expect_check_err(
        r#"fn main() { let g = fn(a: int) -> int { return a; }; println(g("s")); return 0; }"#,
        "argument 1 of 'g' expects int, got string",
    );
}

#[test]
fn rebound_return_type_propagates() {
    // The signature also flows into let annotations downstream.
    expect_check_err(
        r#"fn f() -> int { return 1; } fn main() { let g = f; let x: string = g(); return 0; }"#,
        "type mismatch in 'x'",
    );
}

#[test]
fn correct_rebound_calls_still_run() {
    expect_ok(r#"fn f(x: int) -> int { return x + 1; } fn main() { let g = f; println(g(41)); return 0; }"#);
}

#[test]
fn unannotated_functions_stay_dynamic() {
    // The F5 contract: no annotations -> no checking (fully dynamic).
    expect_ok(r#"fn f(x) { return x; } fn main() { let g = f; println(g("s")); return 0; }"#);
    expect_ok(r#"fn main() { let g = fn(a) { return a; }; println(g(5)); return 0; }"#);
}

#[test]
fn vm_engine_accepts_the_same_allowed_programs() {
    // The checker gates both engines: what passes above must also pass `vm`.
    let out = run_vm_with_deadline(
        r#"fn f(x: int) -> int { return x + 1; } fn main() { let g = f; println(g(41)); return 0; }"#,
        "f5_ok",
    )
    .unwrap_or_else(|e| panic!("{}", e));
    assert!(out.contains("42"), "vm: {}", out);
}
