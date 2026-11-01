//! Integration tests for the Spid3r Core Language Evolution upgrades:
//! 1. Cross-module OOP (`extends`, `super(...)`, `super.method(...)`, `implements`)
//! 2. Default methods on interfaces (local and cross-module)
//! 3. Selective imports (`import { A, B as C } from "mod";`)
//! 4. Map literals (`{ "k": v }`, `{}`, `#{ "k": v }`)
//! 5. Arrow closures (`x => expr`, `(a, b) => a + b`, `() => expr`, `x => { ... }`)
//! 6. Raw string literals (`r"..."`, `r#"..."#`) & escaped interpolation braces (`\{`, `\}`)
//! 7. Fresh mutable collection default parameters (`fn f(xs = [], cfg = {})`)
//! 8. Static class methods (`static fn` and `ClassName.method(...)`)
//! 9. Nullable (`T?`) and Union (`A | B`) gradual types

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use spid3r_lang::parser;
use spid3r_lang::semantic;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

#[cfg(feature = "native")]
fn native_available() -> bool {
    if cfg!(windows) {
        bin()
            .parent()
            .map(|d| d.join("LLVM-C.dll"))
            .unwrap_or_default()
            .is_file()
    } else {
        Command::new("cc").arg("--version").output().is_ok()
            || Command::new("clang").arg("--version").output().is_ok()
    }
}

#[cfg(not(feature = "native"))]
fn native_available() -> bool {
    false
}

fn run_on_engine(dir: &Path, entry: &Path, engine: &str) -> String {
    if engine == "native" {
        let exe = if cfg!(windows) {
            dir.join("out_bin.exe")
        } else {
            dir.join("out_bin")
        };
        let build = Command::new(bin())
            .current_dir(dir)
            .arg("build")
            .arg(entry)
            .arg("-o")
            .arg(&exe)
            .output()
            .expect("failed to invoke spid3r build");
        assert!(
            build.status.success(),
            "[native build] failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        let run = Command::new(&exe)
            .current_dir(dir)
            .output()
            .expect("failed to run native binary");
        assert!(
            run.status.success(),
            "[native run] failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        String::from_utf8_lossy(&run.stdout)
            .replace("\r\n", "\n")
            .trim()
            .to_string()
    } else {
        let out = Command::new(bin())
            .current_dir(dir)
            .arg("run")
            .arg("--engine")
            .arg(engine)
            .arg(entry)
            .output()
            .expect("failed to invoke spid3r run");
        assert!(
            out.status.success(),
            "[engine={}] failed:\nstdout: {}\nstderr: {}",
            engine,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .replace("\r\n", "\n")
            .trim()
            .to_string()
    }
}

fn assert_all_engines(test_name: &str, files: &[(&str, &str)], expected_stdout: &str) {
    let dir = std::env::temp_dir().join(format!(
        "spid3r_core_evo_{}_{}",
        test_name,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (rel, content) in files {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, content).unwrap();
    }
    let entry = dir.join(files[0].0);
    for engine in ["tree", "vm"] {
        let actual = run_on_engine(&dir, &entry, engine);
        assert_eq!(
            actual, expected_stdout,
            "[{} / {}] stdout mismatch",
            test_name, engine
        );
    }
    if native_available() {
        let actual = run_on_engine(&dir, &entry, "native");
        assert_eq!(
            actual, expected_stdout,
            "[{} / native] stdout mismatch",
            test_name
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_module_inheritance_super_and_default_interface_methods() {
    let main_src = r#"
        import "lib/base";

        class AdminUser extends BaseEntity implements Describable {
            public let role: string;

            fn new(id: int, role: string) {
                super(id);
                self.role = role;
            }

            public fn summary(self) -> string {
                return super.summary() + ":" + self.role;
            }

            public fn title(self) -> string {
                return "admin#" + string(self.id);
            }
        }

        fn main() {
            let u = new AdminUser(7, "root");
            println(u.summary());
            println(u.badge());
            return 0;
        }
    "#;

    let base_src = r#"
        public interface Describable {
            fn title(self) -> string;
            fn badge(self) -> string {
                return "[" + self.title() + "]";
            }
        }

        public class BaseEntity {
            public let id: int;

            fn new(id: int) {
                self.id = id;
            }

            public fn summary(self) -> string {
                return "entity#" + string(self.id);
            }
        }
    "#;

    assert_all_engines(
        "cross_mod_oop",
        &[("main.matrix", main_src), ("lib/base.matrix", base_src)],
        "entity#7:root\n[admin#7]",
    );
}

#[test]
fn selective_imports_with_aliases_and_inheritance() {
    let main_src = r#"
        import { Counter as BaseCounter, STEP, triple as mul3 } from "lib/math_mod";

        class FastCounter extends BaseCounter {
            fn new(start: int) {
                super(start);
            }
            public fn jump(self) -> int {
                return self.add(STEP);
            }
        }

        fn main() {
            let c = new FastCounter(5);
            println(c.jump());
            println(mul3(4));
            return 0;
        }
    "#;

    let mod_src = r#"
        public const STEP = 10;

        public fn triple(x: int) -> int {
            return x * 3;
        }

        fn private_helper() -> int {
            return 99;
        }

        public class Counter {
            public let val: int;
            fn new(v: int) {
                self.val = v;
            }
            public fn add(self, d: int) -> int {
                self.val += d;
                return self.val;
            }
        }
    "#;

    assert_all_engines(
        "selective_imports",
        &[("main.matrix", main_src), ("lib/math_mod.matrix", mod_src)],
        "15\n12",
    );
}

#[test]
fn map_literals_arrow_closures_strings_and_mutable_defaults() {
    let main_src = r##"
        fn push_item(x: int, acc = []) {
            acc.push(x);
            return acc;
        }

        fn set_flag(k: string, cfg = {}) {
            cfg[k] = true;
            return cfg;
        }

        fn main() {
            // 1. Map literals: {}, {"k": v}, #{"k": v}
            let empty = {};
            let user = { "name": "Spider", "level": 5 };
            let extra = #{ "active": true };
            println( len(empty) );
            println( user["name"] + ":" + string(user["level"]) + ":" + string(extra["active"]) );

            // 2. Arrow closures: single param, multi-param, zero-param, block body
            let nums = [1, 2, 3, 4];
            let doubled = nums.map(x => x * 2);
            let sum = doubled.reduce(0, (acc, x) => acc + x);
            let get_ten = () => 10;
            let compute = (a: int, b: int) -> int => {
                let tmp = a + b;
                return tmp * 2;
            };
            println(string(sum) + ":" + string(get_ten()) + ":" + string(compute(3, 4)));

            // 3. Match guard compatibility with arrow closures
            let v = 2;
            let matched = match (v) {
                1 => "one",
                n if n > 1 => "many",
                _ => "other",
            };
            println(matched);

            // 4. Escaped braces and raw strings
            let escaped = "\{hello\} {1 + 2}";
            let raw1 = r"C:\dir\{no_interp}\n";
            let raw2 = r#"quote "inside" raw"#;
            println(escaped);
            println(raw1);
            println(raw2);

            // 5. Fresh mutable default parameters on every call
            let a1 = push_item(10);
            let a2 = push_item(20);
            let c1 = set_flag("a");
            let c2 = set_flag("b");
            println(string(len(a1)) + ":" + string(len(a2)) + ":" + string(a2[0]));
            println(string(len(c1)) + ":" + string(len(c2)));
            return 0;
        }
    "##;

    let expected = [
        "0",
        "Spider:5:true",
        "20:10:14",
        "many",
        "{hello} 3",
        r"C:\dir\{no_interp}\n",
        r#"quote "inside" raw"#,
        "1:1:20",
        "1:1",
    ]
    .join("\n");

    assert_all_engines("ergonomics_suite", &[("main.matrix", main_src)], &expected);
}

#[test]
fn static_class_methods_across_all_engines() {
    let main_src = r#"
        class MathBox {
            public let base: int;

            fn new(b: int) {
                self.base = b;
            }

            public static fn double(x: int) -> int {
                return x * 2;
            }

            public static fn quadruple(x: int) -> int {
                return self.double(self.double(x));
            }
        }

        fn main() {
            println(MathBox.double(7));
            println(MathBox.quadruple(5));
            return 0;
        }
    "#;

    assert_all_engines("static_methods", &[("main.matrix", main_src)], "14\n20");
}

#[test]
fn nullable_and_union_types_checked_and_executed() {
    let valid_src = r#"
        fn format_id(id: int | string, prefix: string? = null) -> string {
            let p = prefix ?? "ID";
            return p + ":" + string(id);
        }

        fn find_name(ok_flag: bool) -> string? {
            if ok_flag {
                return "alice";
            }
            return null;
        }

        fn main() {
            let a: int? = null;
            a = 42;
            let b: int | string = "abc";
            b = 99;
            println(format_id(101));
            println(format_id("xyz", "USR"));
            println(find_name(true) ?? "none");
            println(find_name(false) ?? "none");
            return 0;
        }
    "#;

    assert_all_engines(
        "nullable_union_types",
        &[("main.matrix", valid_src)],
        "ID:101\nUSR:xyz\nalice\nnone",
    );

    // Negative semantic check: assigning string to `int?` must fail
    let bad_nullable = r#"
        fn main() {
            let x: int? = "not an int";
            return 0;
        }
    "#;
    let prog = parser::parse_file(bad_nullable, Arc::from("bad1.matrix")).unwrap();
    let err = semantic::check_file(&prog, true, "bad1.matrix").unwrap_err();
    assert!(
        err.message.contains("type mismatch"),
        "expected type mismatch for int?, got: {}",
        err.message
    );

    // Negative semantic check: passing bool to `int | string` parameter must fail
    let bad_union = r#"
        fn take_id(v: int | string) -> string {
            return string(v);
        }
        fn main() {
            println(take_id(true));
            return 0;
        }
    "#;
    let prog2 = parser::parse_file(bad_union, Arc::from("bad2.matrix")).unwrap();
    let err2 = semantic::check_file(&prog2, true, "bad2.matrix").unwrap_err();
    assert!(
        err2.message.contains("expects int | string, got bool"),
        "expected union type error for int | string, got: {}",
        err2.message
    );
}
