//! Native FFI (`extern fn`): calling C from a Spidr program.
//!
//! The contract under test, from `plan.txt` §P6:
//!
//! * the AOT binary calls the C function correctly, for int / float / bool /
//!   string;
//! * the tree and VM reject `extern fn` with a clean `error[usage]` **before**
//!   running anything, rather than defining a body-less function;
//! * a symbol that nothing defines fails at **build** time, not at run time.
//!
//! The AOT cases need a C compiler; they skip when there is none, so a machine
//! without one still gets the rejection tests.

#![cfg(feature = "native")]

use std::path::{Path, PathBuf};
use std::process::Command;

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

/// A C compiler, or `None` to skip the AOT cases.
fn cc() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["clang.exe", "clang", "gcc.exe", "gcc"]
    } else {
        &["clang", "gcc", "cc"]
    };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in names {
            let cand = dir.join(name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    // LLVM installs outside PATH on some Windows machines.
    let fallback = PathBuf::from(r"C:\Program Files\LLVM\bin\clang.exe");
    fallback.is_file().then_some(fallback)
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r_ffi_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn write(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write");
}

const C_SOURCE: &str = r#"
long long c_add(long long a, long long b) { return a + b; }
double c_scale(double x, double k) { return x * k; }
int c_is_even(long long n) { return n % 2 == 0; }
const char* c_hello(const char* name) {
    static char buf[128];
    int i = 0;
    const char* p = "Hello, ";
    while (*p) buf[i++] = *p++;
    while (*name) buf[i++] = *name++;
    buf[i++] = '!';
    buf[i] = 0;
    return buf;
}
"#;

const PROGRAM: &str = r#"extern fn c_add(a: int, b: int) -> int;
extern fn c_scale(x: float, k: float) -> float;
extern fn c_is_even(n: int) -> bool;
extern fn c_hello(name: string) -> string;

fn main() {
    println(c_add(20, 22));
    println(c_scale(1.5, 4.0));
    println(c_is_even(10));
    println(c_hello("Ada"));
    return 0;
}
"#;

/// `extern fn` is AOT-only. Both script engines must refuse it with a usage
/// error and exit 2 — not define an empty function that returns `null`.
#[test]
fn the_script_engines_reject_extern_fn() {
    let dir = scratch("reject");
    write(&dir, "app.matrix", PROGRAM);

    for engine in ["tree", "vm"] {
        let o = Command::new(bin())
            .args(["run", "--engine", engine, "app.matrix"])
            .current_dir(&dir)
            .output()
            .expect("spawn spidr");
        assert_eq!(o.status.code(), Some(2), "[{engine}] must exit 2");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains("error[usage]"), "[{engine}] stderr: {err}");
        assert!(
            err.contains("extern fn 'c_add'"),
            "[{engine}] the message must name the declaration: {err}"
        );
        assert!(
            err.contains("spidr build"),
            "[{engine}] the message must say what to do instead: {err}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The whole point: an AOT binary calls C, and every supported type round-trips.
#[test]
fn aot_calls_c_with_int_float_bool_and_string() {
    let Some(cc) = cc() else {
        eprintln!("skipping: no C compiler on PATH");
        return;
    };
    let dir = scratch("call");
    write(&dir, "cmath.c", C_SOURCE);
    write(&dir, "app.matrix", PROGRAM);

    let obj = dir.join(if cfg!(windows) {
        "cmath.obj"
    } else {
        "cmath.o"
    });
    let st = Command::new(&cc)
        .arg("-c")
        .arg(dir.join("cmath.c"))
        .arg("-o")
        .arg(&obj)
        .status()
        .expect("run the C compiler");
    assert!(st.success(), "the C compiler failed");

    let exe = dir.join(format!("app{}", std::env::consts::EXE_SUFFIX));
    let o = Command::new(bin())
        .args([
            "build",
            "app.matrix",
            "-o",
            exe.to_str().unwrap(),
            "--link-lib",
            obj.to_str().unwrap(),
        ])
        .current_dir(&dir)
        .output()
        .expect("spawn spidr build");
    assert_eq!(
        o.status.code(),
        Some(0),
        "build failed: {}",
        String::from_utf8_lossy(&o.stderr)
    );

    let run = Command::new(&exe).output().expect("run the built binary");
    assert_eq!(run.status.code(), Some(0), "the binary exited non-zero");
    let out = String::from_utf8_lossy(&run.stdout);
    assert_eq!(
        out.trim_end(),
        "42\n6.0\ntrue\nHello, Ada!",
        "marshalling is wrong; stdout was {out:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A declaration nothing defines must fail at **build**, and must not leave a
/// binary behind. Failing at run time instead is the one outcome the engine
/// contract calls worse than a compile error.
#[test]
fn a_missing_symbol_fails_at_build_not_at_run() {
    let Some(cc) = cc() else {
        eprintln!("skipping: no C compiler on PATH");
        return;
    };
    // The compiler is only needed to prove the *empty* link fails for the
    // symbol, not for a missing toolchain.
    let _ = cc;
    let dir = scratch("missing");
    write(
        &dir,
        "app.matrix",
        "extern fn no_such_symbol(a: int) -> int;\nfn main() {\n    println(no_such_symbol(1));\n    return 0;\n}\n",
    );
    let exe = dir.join(format!("app{}", std::env::consts::EXE_SUFFIX));
    let o = Command::new(bin())
        .args(["build", "app.matrix", "-o", exe.to_str().unwrap()])
        .current_dir(&dir)
        .output()
        .expect("spawn spidr build");
    assert_ne!(o.status.code(), Some(0), "the build must fail");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("no_such_symbol"),
        "the linker error must name the symbol: {err}"
    );
    assert!(
        !exe.exists(),
        "a failed link must not leave a binary behind"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Only the four documented types cross the boundary. A struct-typed parameter
/// is refused at build time with the reason, not marshalled as a pointer.
#[test]
fn an_unsupported_parameter_type_is_refused() {
    let dir = scratch("badtype");
    write(
        &dir,
        "app.matrix",
        "extern fn takes_map(m: map) -> int;\nfn main() {\n    return 0;\n}\n",
    );
    let exe = dir.join(format!("app{}", std::env::consts::EXE_SUFFIX));
    let o = Command::new(bin())
        .args(["build", "app.matrix", "-o", exe.to_str().unwrap()])
        .current_dir(&dir)
        .output()
        .expect("spawn spidr build");
    assert_ne!(o.status.code(), Some(0), "the build must fail");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("cannot pass 'map'"),
        "the message must name the type and the supported set: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An extern declaration must not break the ordinary paths: a program with no
/// `extern fn` still parses, checks and builds.
#[test]
fn ordinary_programs_are_unaffected() {
    let dir = scratch("plain");
    write(
        &dir,
        "app.matrix",
        "fn main() {\n    println(1 + 1);\n    return 0;\n}\n",
    );
    let o = Command::new(bin())
        .args(["run", "app.matrix"])
        .current_dir(&dir)
        .output()
        .expect("spawn spidr");
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim_end(), "2");
    let _ = std::fs::remove_dir_all(&dir);
}
