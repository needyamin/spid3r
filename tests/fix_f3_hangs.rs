//! F3 regression: self-referential containers must not hang the runtime.
//!
//! Before the fix, four plain Spid3r constructs deadlocked BOTH engines with
//! no diagnostic (audit probes p01/p03/p05/p06): the container's mutex was
//! held across a recursion into values that re-lock it. The fix snapshots
//! contents under the lock and recurses outside it (the established
//! json_write pattern) in the shared `value::collections` layer:
//! - `iter_values` (for-in over a self-containing list) — all arms
//! - `call_method_host` special cases: linked_list.to_list, map.values,
//!   set union/intersection/difference (snapshot both sides)
//! - seq.rs helpers `set_op` / `map_values_out`, nodes.rs `to_vec`
//!
//! The suite must never hang: every run is spawned and polled with
//! `try_wait` against a deadline and killed on timeout (pattern from
//! tests/fix_f4_gc.rs / tests/audit_regressions.rs).

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run_with_deadline(engine: &str, src: &str, name: &str) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!("spid3r_f3_{}_{}.matrix", name, std::process::id()));
    std::fs::write(&path, src).map_err(|e| format!("write {}: {}", path.display(), e))?;
    let mut child = Command::new(bin())
        .args(["run", "--engine", engine])
        .arg(&path)
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
                    break Err(format!(
                        "[{}] still running after 15s — the F3 deadlock is back",
                        name
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => break Err(format!("wait failed: {}", e)),
        }
    };
    let _ = std::fs::remove_file(&path);
    let (status, out, err) = outcome?;
    if !status.success() {
        return Err(format!("[{}] exit {:?}: {}", name, status.code(), err));
    }
    Ok(out)
}

const P01_FOR_IN_SELF: &str = r#"fn main() {
    let l = list();
    l.push(l);
    println("before for");
    for x in l {
        println("iterating");
        break;
    }
    println("after for");
    return 0;
}
"#;

const P03_MUTUAL_SET_UNION: &str = r#"fn main() {
    let a = set();
    let b = set();
    a.add(b);
    b.add(a);
    println("before union");
    a.union(b);
    println("after union");
    return 0;
}
"#;

const P05_MAP_SELF_VALUES: &str = r#"fn main() {
    let m = map();
    m.set("self", m);
    println("before values");
    let vs = m.values();
    println("after values");
    return 0;
}
"#;

const P06_LINKED_LIST_SELF_TO_LIST: &str = r#"fn main() {
    let ll = linked_list();
    ll.push_back(ll);
    println("before to_list");
    let l = ll.to_list();
    println("after to_list");
    return 0;
}
"#;

#[test]
fn for_in_over_self_containing_list_terminates() {
    for engine in ["tree", "vm"] {
        let out = run_with_deadline(engine, P01_FOR_IN_SELF, "p01")
            .unwrap_or_else(|e| panic!("{}", e));
        assert!(out.contains("before for"), "[{}] {}", engine, out);
        assert!(out.contains("iterating"), "[{}] {}", engine, out);
        assert!(out.contains("after for"), "[{}] {}", engine, out);
    }
}

#[test]
fn set_union_with_mutually_self_referencing_sets_terminates() {
    for engine in ["tree", "vm"] {
        let out = run_with_deadline(engine, P03_MUTUAL_SET_UNION, "p03")
            .unwrap_or_else(|e| panic!("{}", e));
        assert!(out.contains("before union"), "[{}] {}", engine, out);
        assert!(out.contains("after union"), "[{}] {}", engine, out);
    }
}

#[test]
fn map_values_on_self_valued_map_terminates() {
    for engine in ["tree", "vm"] {
        let out = run_with_deadline(engine, P05_MAP_SELF_VALUES, "p05")
            .unwrap_or_else(|e| panic!("{}", e));
        assert!(out.contains("before values"), "[{}] {}", engine, out);
        assert!(out.contains("after values"), "[{}] {}", engine, out);
    }
}

#[test]
fn linked_list_to_list_on_self_referencing_list_terminates() {
    for engine in ["tree", "vm"] {
        let out = run_with_deadline(engine, P06_LINKED_LIST_SELF_TO_LIST, "p06")
            .unwrap_or_else(|e| panic!("{}", e));
        assert!(out.contains("before to_list"), "[{}] {}", engine, out);
        assert!(out.contains("after to_list"), "[{}] {}", engine, out);
    }
}

#[test]
fn normal_iteration_is_unaffected() {
    // Non-regression: ordinary (non-cyclic) containers iterate in order with
    // identical output on both engines.
    let src = r#"fn main() {
    let xs = list(1);
    xs.push(2);
    xs.push(3);
    for x in xs {
        print(string(x));
    }
    println("");
    let s = set();
    s.add("a");
    s.add("b");
    println(len(s.to_list()));
    return 0;
}
"#;
    for engine in ["tree", "vm"] {
        let out = run_with_deadline(engine, src, "normal")
            .unwrap_or_else(|e| panic!("{}", e));
        assert!(out.contains("123"), "[{}] {}", engine, out);
        assert!(out.contains("2"), "[{}] {}", engine, out);
    }
}
