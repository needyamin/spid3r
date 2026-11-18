//! R10.2 & R10.3: Real generic syntax, constraints, explicit instantiation,
//! AOT monomorphization, and unboxed `list<int>` / `list<float>` parity across
//! tree, VM, and native AOT backends.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use spid3r_lang::{parser, semantic};

fn bin() -> PathBuf {
    for key in ["CARGO_BIN_EXE_spid3r", "CARGO_BIN_EXE_spidr"] {
        if let Ok(p) = std::env::var(key) {
            let p = PathBuf::from(p);
            if p.is_file() {
                return p;
            }
        }
    }
    let exe = std::env::current_exe().expect("test executable path");
    let profile_dir = exe
        .parent()
        .and_then(|deps| deps.parent())
        .expect("profile directory");
    for stem in ["spid3r", "spidr"] {
        let cand = profile_dir.join(format!("{}{}", stem, std::env::consts::EXE_SUFFIX));
        if cand.is_file() {
            return cand;
        }
    }
    panic!("spid3r binary not found next to {}", profile_dir.display());
}

fn assert_all_engines(name: &str, src: &str, expected: &str) {
    let dir = std::env::temp_dir().join(format!("spid3r_gen_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let file = dir.join("app.matrix");
    std::fs::write(&file, src).expect("write");

    for engine in ["tree", "vm"] {
        let o = Command::new(bin())
            .args(["run", "--engine", engine, file.to_str().unwrap()])
            .output()
            .expect("spawn spid3r");
        assert_eq!(
            o.status.code(),
            Some(0),
            "[{engine}] stderr: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
            expected,
            "[{engine}] stdout mismatch"
        );
    }

    #[cfg(feature = "native")]
    {
        let exe_out = dir.join(format!("app{}", std::env::consts::EXE_SUFFIX));
        let b = Command::new(bin())
            .args([
                "build",
                file.to_str().unwrap(),
                "-o",
                exe_out.to_str().unwrap(),
            ])
            .output()
            .expect("build native");
        assert_eq!(
            b.status.code(),
            Some(0),
            "[native build] stderr: {}",
            String::from_utf8_lossy(&b.stderr)
        );
        let r = Command::new(&exe_out).output().expect("run native");
        assert_eq!(
            r.status.code(),
            Some(0),
            "[native run] stderr: {}",
            String::from_utf8_lossy(&r.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&r.stdout).replace("\r\n", "\n"),
            expected,
            "[native] stdout mismatch"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn generic_instantiation_multi_args_and_monomorphization_parity() {
    let src = r#"
interface Named {
    fn tag(self) -> string;
}

class Item implements Named {
    public let label: string;
    new(label: string) { self.label = label; }
    public fn tag(self) -> string { return self.label; }
}

class Box<T> {
    public let value;
    new(value) { self.value = value; }
    public fn get(self) { return self.value; }
}

fn identity<T>(x: T) -> T {
    return x;
}

fn add_num<T: Numeric>(a: T, b: T) -> T {
    return a + b;
}

fn cmp_eq<T: Comparable>(a: T, b: T) -> bool {
    return a == b;
}

fn show_tag<T: Named>(x: T) -> string {
    return x.tag();
}

fn nested_types(m: map<string, list<int>>, r: result<int, string>) -> int {
    let _ = r;
    return m["k"][0];
}

fn main() {
    let a: int = identity<int>(42);
    let b = add_num<int>(10, 32);
    let f = add_num<float>(1.5, 2.5);
    let eq = cmp_eq<string>("hi", "hi");
    let it = new Item("ok");
    let tg = show_tag<Item>(it);
    let bx = new Box<int>(99);
    let m: map<string, list<int>> = {"k": [7, 8]};
    let n = nested_types(m, ok(1));
    println(a);
    println(b);
    println(f);
    println(eq);
    println(tg);
    println(bx.get());
    println(n);
    return 0;
}
"#;
    assert_all_engines("mono", src, "42\n42\n4.0\ntrue\nok\n99\n7\n");
}

#[test]
fn unboxed_typed_lists_parity_and_bounds() {
    let src = r#"
fn sum_unboxed() -> int {
    let xs: list<int> = [1, 2, 3];
    xs.push(4);
    xs.push(5);
    xs[1] = xs[1] + 10;
    let last = xs.pop();
    let total = 0;
    let i = 0;
    while i < xs.len() {
        total = total + xs[i];
        i = i + 1;
    }
    return total + last;
}

fn sum_floats() -> float {
    let fs: list<float> = [];
    fs.push(1.5);
    fs.push(2.5);
    fs[0] = 3.5;
    return fs[0] + fs[1];
}

fn main() {
    println(sum_unboxed());
    println(sum_floats());
    return 0;
}
"#;
    assert_all_engines("unboxed_lists", src, "25\n6.0\n");
}

#[test]
fn generic_checker_rejects_mismatched_args_and_violated_bounds() {
    let bad_arg = r#"
fn identity<T>(x: T) -> T { return x; }
fn main() {
    let _ = identity<int>("not-an-int");
    return 0;
}
"#;
    let p1 = parser::parse_file(bad_arg, Arc::from("bad_arg.matrix")).expect("parse");
    let e1 = semantic::check_file(&p1, true, "bad_arg.matrix").expect_err("must reject");
    assert!(
        e1.message.contains("expects int, got string"),
        "{}",
        e1.message
    );

    let bad_bound = r#"
fn add_num<T: Numeric>(a: T, b: T) -> T { return a + b; }
fn main() {
    let _ = add_num<string>("a", "b");
    return 0;
}
"#;
    let p2 = parser::parse_file(bad_bound, Arc::from("bad_bound.matrix")).expect("parse");
    let e2 = semantic::check_file(&p2, true, "bad_bound.matrix").expect_err("must reject");
    assert!(
        e2.message.contains("does not satisfy constraint 'Numeric'"),
        "{}",
        e2.message
    );

    let bad_arity = r#"
fn identity<T>(x: T) -> T { return x; }
fn main() {
    let _ = identity<int, string>(1);
    return 0;
}
"#;
    let p3 = parser::parse_file(bad_arity, Arc::from("bad_arity.matrix")).expect("parse");
    let e3 = semantic::check_file(&p3, true, "bad_arity.matrix").expect_err("must reject");
    assert!(
        e3.message.contains("expects 1 type argument(s), got 2"),
        "{}",
        e3.message
    );
}
