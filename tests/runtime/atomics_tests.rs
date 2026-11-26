//! R9.3: `atomic_int` value type — ordering, CAS, exchange, and multi-task contention
//! across both script engines (tree + VM).

use std::path::PathBuf;
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

fn assert_both(name: &str, src: &str, expected: &str) {
    let dir = std::env::temp_dir().join(format!("spid3r_atomics_{}_{}", name, std::process::id()));
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
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn atomic_int_basic_load_store_exchange_cas() {
    let src = r#"
fn main() {
    let a = atomic_int(10);
    println(type_of(a));
    println(a.load());
    a.store(25);
    println(a.load());
    println(a.exchange(40));
    println(a.load());
    println(a.cas(99, 100));
    println(a.load());
    println(a.cas(40, 100));
    println(a.load());
    println(a.fetch_add(5));
    println(a.fetch_sub(3));
    println(a.load());
    let b = atomic_int();
    println(b.load());
    return 0;
}
"#;
    assert_both(
        "basic",
        src,
        "atomic_int\n10\n25\n25\n40\nfalse\n40\ntrue\n100\n100\n105\n102\n0\n",
    );
}

#[test]
fn atomic_int_spawn_contention_and_cas_loop() {
    let src = r#"
fn worker(counter, iters) {
    let i = 0;
    while i < iters {
        let cur = counter.load();
        if counter.cas(cur, cur + 1) {
            i = i + 1;
        }
    }
    return 0;
}

async fn main() {
    let counter = atomic_int(0);
    let fast = atomic_int(0);
    let t1 = spawn (() => { worker(counter, 150); fast.fetch_add(150); });
    let t2 = spawn (() => { worker(counter, 150); fast.fetch_add(150); });
    let t3 = spawn (() => { worker(counter, 150); fast.fetch_add(150); });
    let t4 = spawn (() => { worker(counter, 150); fast.fetch_add(150); });
    await t1;
    await t2;
    await t3;
    await t4;
    println(counter.load());
    println(fast.load());
    return 0;
}
"#;
    assert_both("contention", src, "600\n600\n");
}
