//! Stage 7: lint warnings.
//!
//! Before this the checker could only fail a build — there was no way to say
//! "this is suspicious" without rejecting the program. Four lints now report
//! through the same `Diagnostic` machinery (snippet, `--json`, `--allow`):
//!
//! * `unused-variable`, `unused-parameter`, `unused-import`
//! * `unreachable-code`
//!
//! A warning never changes the exit code unless `--deny-warnings` is passed.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn write(tag: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("spid3r_stage7");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{}.matrix", tag));
    std::fs::write(&path, body).expect("write case");
    path
}

fn check(path: &PathBuf, extra: &[&str]) -> (i32, String, String) {
    let mut args = vec!["check"];
    args.extend_from_slice(extra);
    args.push(path.to_str().unwrap());
    let o = Command::new(bin()).args(&args).output().expect("spawn");
    let _ = std::fs::remove_file(path);
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

#[test]
fn an_unused_local_is_reported_but_does_not_fail_the_check() {
    let path = write(
        "unused_var",
        "fn main() {\n    let spare = 5;\n    return 0;\n}\n",
    );
    let (code, _, err) = check(&path, &[]);
    assert_eq!(code, 0, "a warning must not fail the check: {}", err);
    assert!(
        err.contains("warning[semantic]: unused variable 'spare'"),
        "expected an unused-variable warning:\n{}",
        err
    );
    // It still shows the source line and a caret.
    assert!(
        err.contains("let spare = 5;") && err.contains('^'),
        "{}",
        err
    );
}

/// A name starting with `_` opts out — the convention for callback parameters
/// that must match a framework signature.
#[test]
fn an_underscore_name_is_not_reported() {
    let path = write(
        "underscore",
        "fn main() {\n    let _ignored = 5;\n    return 0;\n}\n",
    );
    let (code, _, err) = check(&path, &[]);
    assert_eq!(code, 0);
    assert!(
        !err.contains("warning"),
        "`_`-prefixed names must stay silent:\n{}",
        err
    );
}

/// A variable that is *called* counts as used. This was a false positive: the
/// checker looked a callee up without visiting it, so every closure held in a
/// variable and then invoked was reported as unused.
#[test]
fn calling_a_closure_counts_as_using_it() {
    let path = write(
        "called",
        "fn main() {\n    let adder = fn(x) {\n        return x + 1;\n    };\n    println(adder(5));\n    return 0;\n}\n",
    );
    let (code, _, err) = check(&path, &[]);
    assert_eq!(code, 0);
    assert!(
        !err.contains("unused variable"),
        "a called closure is used:\n{}",
        err
    );
}

#[test]
fn an_unused_parameter_is_reported() {
    let path = write(
        "unused_param",
        "fn f(used, ignored) {\n    return used;\n}\nfn main() {\n    println(f(1, 2));\n    return 0;\n}\n",
    );
    let (code, _, err) = check(&path, &[]);
    assert_eq!(code, 0);
    assert!(
        err.contains("unused parameter 'ignored'") && !err.contains("'used'"),
        "only the unused parameter should be named:\n{}",
        err
    );
}

#[test]
fn unreachable_code_is_reported_once() {
    let path = write(
        "unreachable",
        "fn main() {\n    return 0;\n    println(\"dead\");\n    println(\"also dead\");\n}\n",
    );
    let (code, _, err) = check(&path, &[]);
    assert_eq!(code, 0);
    assert!(
        err.contains("unreachable code"),
        "expected an unreachable-code warning:\n{}",
        err
    );
    assert_eq!(
        err.matches("unreachable code").count(),
        1,
        "only the first dead statement should be reported:\n{}",
        err
    );
}

#[test]
fn deny_warnings_turns_them_into_a_failure() {
    let path = write(
        "deny",
        "fn main() {\n    let spare = 5;\n    return 0;\n}\n",
    );
    let (code, _, err) = check(&path, &["--deny-warnings"]);
    assert_eq!(code, 1, "deny-warnings must fail the build");
    assert!(err.contains("warning[semantic]"), "{}", err);
    assert!(err.contains("treated as errors"), "{}", err);
}

#[test]
fn allow_silences_one_lint_without_silencing_the_rest() {
    let path = write(
        "allow",
        "fn f(ignored) {\n    return 0;\n}\nfn main() {\n    let spare = 5;\n    println(f(1));\n    return 0;\n}\n",
    );
    let (_, _, all) = check(&path, &[]);
    assert!(all.contains("unused parameter") && all.contains("unused variable"));

    // Re-create the file: `check` deletes it.
    let path = write(
        "allow",
        "fn f(ignored) {\n    return 0;\n}\nfn main() {\n    let spare = 5;\n    println(f(1));\n    return 0;\n}\n",
    );
    let (_, _, filtered) = check(&path, &["--allow", "unused-parameter"]);
    assert!(
        !filtered.contains("unused parameter") && filtered.contains("unused variable"),
        "--allow should drop only the named lint:\n{}",
        filtered
    );
}

/// Warnings are part of the JSON stream, tagged with severity and lint name.
#[test]
fn warnings_appear_in_json_with_their_lint() {
    let path = write(
        "json",
        "fn main() {\n    let spare = 5;\n    return 0;\n}\n",
    );
    let (code, out, err) = check(&path, &["--json"]);
    assert_eq!(code, 0);
    assert!(err.is_empty(), "json mode should not print the human form");
    let line = out
        .lines()
        .find(|l| l.contains("unused variable"))
        .expect("warning in json");
    assert!(line.contains("\"severity\":\"warning\""), "{}", line);
    assert!(line.contains("\"lint\":\"unused-variable\""), "{}", line);
}

/// A file that does not check produces no warnings: the error short-circuits.
#[test]
fn a_failing_file_reports_only_the_error() {
    let path = write(
        "error_only",
        "fn main() {\n    let spare = 5;\n    let bad: int = \"s\";\n    return 0;\n}\n",
    );
    let (code, _, err) = check(&path, &[]);
    assert_eq!(code, 1);
    assert!(err.contains("type mismatch"), "{}", err);
    assert!(
        !err.contains("warning"),
        "warnings must not be mixed into a failed check:\n{}",
        err
    );
}

/// An `abstract fn m(self);` declaration has no body, so binding `self` (and
/// the parameters) only produced an "unused variable 'self'" warning for a
/// method that could not possibly use it. Every abstract method in a program
/// warned, which trained people to ignore the lint.
#[test]
fn an_abstract_declaration_does_not_warn_about_self() {
    let path = write(
        "abstract_decl",
        "abstract class Shape {\n\
         public let id;\n\
         fn new(id) {\n\
         self.id = id;\n\
         }\n\
         abstract fn area(self);\n\
         public fn label(self) {\n\
         return \"shape:\" + self.id;\n\
         }\n\
         }\n\
         class Square extends Shape {\n\
         public let side;\n\
         fn new(id, side) {\n\
         super(id);\n\
         self.side = side;\n\
         }\n\
         public fn area(self) {\n\
         return self.side * self.side;\n\
         }\n\
         }\n\
         fn main() {\n\
         let s = new Square(\"a\", 3);\n\
         return s.label() == \"shape:a\" and s.area() == 9;\n\
         }\n",
    );
    let (code, _, err) = check(&path, &[]);
    assert_eq!(code, 0, "{}", err);
    assert!(
        !err.contains("unused"),
        "an abstract declaration must not be linted:\n{}",
        err
    );
}

/// The lint still fires for a *concrete* method that ignores its receiver -
/// exempting the declaration must not exempt real code.
#[test]
fn a_concrete_method_that_ignores_self_still_warns() {
    let path = write(
        "concrete_self",
        "class A {\n\
         public fn hi(self) {\n\
         return 1;\n\
         }\n\
         }\n\
         fn main() {\n\
         return new A().hi();\n\
         }\n",
    );
    let (code, _, err) = check(&path, &[]);
    assert_eq!(code, 0, "{}", err);
    assert!(
        err.contains("unused variable 'self'"),
        "expected the lint to survive:\n{}",
        err
    );
}
