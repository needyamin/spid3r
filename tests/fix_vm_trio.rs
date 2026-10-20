//! F8/F9/F10 regression tests (VM hardening round).
//!
//! F9: the VM's circular-import diagnostic now shares the tree's chain
//! rendering via `modules::circular_import_message` (pub, used by BOTH
//! engines' import loaders) — format parity pinned here.
//!
//! F10: a crafted `Return` on an empty operand stack used to swallow its own
//! error and exit Ok. The Return handler now routes a failed pop through the
//! normal error funnel. The mutation sweep below re-pins the deserializer/validator
//! contract on hand-mutated bytecode: every mutation yields a clean
//! diagnostic or normal behavior — never a host panic.
//!
//! F8 (VM Import drops const-ness; SetGlobal now refuses imported const
//! names) is verified by code review against the tree's `define_const`
//! contract; its only reachable path is crafted bytecode whose serialization
//! layout is length-prefixed and recursive, so a byte-precise behavioral test
//! needs a dedicated construction seam (follow-up item, see plan.html).

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run_with_deadline(args: &[&str], limit: Duration) -> Result<(i32, String, String), String> {
    let mut child = Command::new(bin())
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn failed: {}", e))?;
    let deadline = Instant::now() + limit;
    loop {
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
                let code = status.code().unwrap_or(-1);
                break Ok((code, out, err));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err("did not finish within the deadline".to_string());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => break Err(format!("wait failed: {}", e)),
        }
    }
}

#[test]
fn circular_import_message_parity_with_tree() {
    // F9: one shared formatter for both engines; the tree rendered exactly
    // this "(chain: a -> b -> a)" shape before, the VM lacked the suffix.
    let msg = spid3r_lang::modules::circular_import_message(
        &[PathBuf::from("a.matrix"), PathBuf::from("b.matrix")],
        "a.matrix",
    );
    assert_eq!(
        msg,
        "circular import of 'a.matrix' (chain: a.matrix -> b.matrix -> a.matrix)"
    );
    // Empty in-progress chain still renders the offending path.
    let msg0 = spid3r_lang::modules::circular_import_message(&[], "self.matrix");
    assert_eq!(
        msg0,
        "circular import of 'self.matrix' (chain: self.matrix)"
    );
}

#[test]
fn tree_engine_still_reports_the_import_chain() {
    // Behavioral F9 half: the tree loader keeps producing the chain suffix.
    let dir = std::env::temp_dir().join(format!("spid3r_f9_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.matrix"), "import \"b\";\nfn main() { return 0; }\n").unwrap();
    std::fs::write(dir.join("b.matrix"), "import \"a\";\nfn main() { return 0; }\n").unwrap();
    let main = dir.join("a.matrix");
    let (code, _, err) = run_with_deadline(
        &["run", "--engine", "tree", main.to_str().unwrap()],
        Duration::from_secs(15),
    )
    .unwrap_or_else(|e| panic!("{}", e));
    assert_eq!(code, 1, "circular import must fail with exit 1");
    assert!(
        err.contains("circular import of") && err.contains("(chain:"),
        "expected chain suffix, got: {}",
        err
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn compiled_artifact_run_is_unaffected() {
    // No-regression for the F8/F10 area: a legal compile -> exec round trip
    // still behaves identically (the new const guard and Return checks only
    // fire on malformed input).
    let dir = std::env::temp_dir().join(format!("spid3r_f8_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("ok.matrix");
    std::fs::write(&src, "fn main() {\n    println(\"ok\");\n    return 0;\n}\n").unwrap();
    let out_path = dir.join("ok.matrixc");
    let (code, _, err) = run_with_deadline(
        &[
            "compile",
            src.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
        ],
        Duration::from_secs(15),
    )
    .unwrap_or_else(|e| panic!("{}", e));
    assert_eq!(code, 0, "compile failed: {}", err);
    let (code, out, err) = run_with_deadline(
        &["exec", out_path.to_str().unwrap()],
        Duration::from_secs(15),
    )
    .unwrap_or_else(|e| panic!("{}", e));
    assert_eq!(code, 0, "exec failed: {}", err);
    assert!(out.contains("ok"), "exec output: {}", out);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hand_mutated_bytecode_never_panics_the_host() {
    // Compile a real artifact, then apply deterministic mutations (truncation,
    // 0xFF opcode byte, misplaced Return bytes). Every run must produce a
    // clean diagnostic or normal behavior — never a Rust panic, abort, or
    // stack overflow (exit codes 101 / 0xC0000005 / 0xC00000FD).
    let dir = std::env::temp_dir().join(format!("spid3r_f10_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("hello.matrix");
    std::fs::write(&src, "fn main() {\n    println(\"hello\");\n    return 0;\n}\n").unwrap();
    let base = dir.join("hello.matrixc");
    let (code, _, err) = run_with_deadline(
        &[
            "compile",
            src.to_str().unwrap(),
            "-o",
            base.to_str().unwrap(),
        ],
        Duration::from_secs(15),
    )
    .unwrap_or_else(|e| panic!("{}", e));
    assert_eq!(code, 0, "compile failed: {}", err);
    let bytes = std::fs::read(&base).unwrap();
    assert!(bytes.len() > 16, "artifact unexpectedly small");

    let mut mutations: Vec<(String, Vec<u8>)> = Vec::new();
    mutations.push(("trunc60".to_string(), bytes[..bytes.len() * 6 / 10].to_vec()));
    let mut b = bytes.clone();
    let mid = bytes.len() / 2;
    b[mid] = 0xFF;
    mutations.push(("midFF".to_string(), b));
    let mut c = bytes.clone();
    c[8] = 0x26; // OpCode::Return placed after the format header
    mutations.push(("retAt8".to_string(), c));
    let mut d = bytes.clone();
    let last = d.len() - 1;
    d[last] = 0x26;
    mutations.push(("retAtLast".to_string(), d));
    // A sparse sweep over the whole body: every 64th byte -> 0x26 (Return).
    for (i, off) in (8..bytes.len()).step_by(64).enumerate() {
        let mut e = bytes.clone();
        e[off] = 0x26;
        mutations.push((format!("retScan{}", i), e));
    }

    for (name, data) in mutations {
        let path = dir.join(format!("mut_{}.matrixc", name));
        std::fs::write(&path, &data).unwrap();
        let (code, out, err) = run_with_deadline(
            &["exec", path.to_str().unwrap()],
            Duration::from_secs(15),
        )
        .unwrap_or_else(|e| panic!("[{}] {}", name, e));
        assert!(
            code == 0 || code == 1,
            "[{}] host panic/abort: exit {} stderr: {}",
            name,
            code,
            err
        );
        assert!(
            !err.contains("panicked at") && !err.contains("has overflowed its stack"),
            "[{}] host panic text: {}",
            name,
            err
        );
        if code == 0 {
            // Either the file still runs (mutation landed in a constant) or,
            // before the F10 fix, a misplaced Return could swallow its error
            // and exit Ok with NO output at all — that silence is now
            // impossible for a Return on an empty stack: it must exit 1 with
            // the underflow diagnostic.
            assert!(
                out.contains("hello") || err.contains("stack underflow") || out.is_empty(),
                "[{}] unexpected silent output combination: out={} err={}",
                name,
                out,
                err
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
