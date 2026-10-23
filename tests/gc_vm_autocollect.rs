//! Regression: the VM engine must auto-collect cyclic garbage.
//!
//! `docs/GC.md` specifies two auto-collection triggers: the interpreter's
//! statement loop **and the VM's opcode dispatch boundary**. Only the tree
//! engine actually had one (`interpreter::exec_block` -> `gc::maybe_collect`),
//! so a VM program that allocates cyclic garbage in a loop leaked every cell
//! until process exit — the same program on the tree engine stayed flat.
//! Measured before the fix (audit probe gc_grow.matrix): 40 rounds x 50k
//! mutual Node cycles grew the VM's live-cell count linearly to 4,000,000
//! while the tree engine stayed at ~100-200.
//!
//! The fix adds `gc::maybe_collect()` to the VM's existing 2048-instruction
//! periodic poll in `vm::run_loop` (an instruction boundary holds every live
//! value as a strong reference, so it is a safe point).
//!
//! This test pins reclamation on BOTH engines WITHOUT any explicit
//! `gc_collect()` call — only the automatic path can keep the count bounded —
//! and pins the safety half: a cycle kept live across the whole churn is still
//! intact afterwards (a premature sweep would corrupt it).

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

/// Spawn `spid3r run --engine <engine> <path>` with auto-GC explicitly ON
/// (an ambient `SPID3R_GC=0` must not mask the regression) and poll with
/// `try_wait` until exit or the deadline; killed at the deadline so the
/// suite can never hang.
fn run_gc_on(engine: &str, path: &std::path::Path, limit: Duration) -> Result<String, String> {
    let mut child = Command::new(bin())
        .args(["run", "--engine", engine])
        .arg(path)
        .env("SPID3R_GC", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn failed: {}", e))?;
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("wait failed: {}", e))?
        {
            let mut out = String::new();
            if let Some(mut s) = child.stdout.take() {
                let _ = s.read_to_string(&mut out);
            }
            if !status.success() {
                let mut err = String::new();
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut err);
                }
                return Err(format!("exit {:?}: {}", status.code(), err));
            }
            return Ok(out);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("did not finish within {:?}", limit));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 400k mutual two-node cycles are allocated and dropped in a loop with NO
/// explicit `gc_collect()` anywhere. One live `head <-> tail` cycle is held
/// across the entire churn; at the end it must still read back 42/43 (safety)
/// and the live-cell count must be bounded (reclamation).
const VM_CYCLE_CHURN: &str = r#"
class Node {
    public let next;
    public let val;
    fn new(v) { self.next = null; self.val = v; }
}

fn make_cycle() {
    let a = Node(1);
    let b = Node(2);
    a.next = b;
    b.next = a;
}

fn main() {
    let head = Node(42);
    let tail = Node(43);
    head.next = tail;
    tail.next = head;
    for let round = 0; round < 8; round = round + 1 {
        for let i = 0; i < 50000; i = i + 1 {
            make_cycle();
        }
    }
    let a = head.next.val;
    let b = head.next.next.val;
    if (a != 43) { println("SWEPT-A"); return 1; }
    if (b != 42) { println("SWEPT-B"); return 1; }
    println(gc_live_cells());
    return 0;
}
"#;

/// Without the VM auto-collect hook, 400k x ~3 tracked cells (two instances +
/// one call environment) leak linearly: the VM reported >1.2M live cells for
/// this exact program before the fix. With it, both engines stay near 1k.
/// The bound is deliberately loose (100k) so it cannot flake on collector
/// timing, yet is >100x below the pre-fix leak.
#[test]
fn vm_engine_auto_collects_cyclic_garbage() {
    let path = std::env::temp_dir().join(format!("spid3r_vm_gc_autocollect_{}.matrix", std::process::id()));
    std::fs::write(&path, VM_CYCLE_CHURN).expect("write probe");
    for engine in ["tree", "vm"] {
        let out = run_gc_on(engine, &path, Duration::from_secs(60))
            .unwrap_or_else(|e| panic!("[{}] cycle churn failed: {}", engine, e));
        assert!(
            !out.contains("SWEPT"),
            "[{}] the kept live cycle was corrupted — the collector swept a reachable cell: {}",
            engine,
            out
        );
        let live: usize = out
            .trim()
            .lines()
            .last()
            .expect("a live-cell count line")
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("[{}] expected a live-cell count, got: {}", engine, out));
        assert!(
            live < 100_000,
            "[{}] cyclic garbage was not reclaimed: {} live cells after the churn \
             (pre-fix VM leaked >1.2M here)",
            engine,
            live
        );
    }
    let _ = std::fs::remove_file(&path);
}
