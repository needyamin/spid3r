//! Stage-3 artifact hardening regressions (assessment/AUDIT_2026-10-20.md §6):
//!
//! * **A-8** — a crafted or corrupt `.matrixc` must fail with a clean
//!   `Diagnostic`, never a panic: trailing bytes, instruction-table
//!   mismatches, a fall-through chunk end, unbounded nesting, and the
//!   quadratic jump validation.
//! * **A-11** — the WebView2 bootstrapper is opt-in, checksum-verified, and
//!   fails closed on a partial/tampered download. The policy module is the one
//!   `build.rs` runs (`build_support/webview2.rs`), included here directly.
//!
//! A-5 (git URL whitelist, locked commits, raw-byte hashes) is covered in
//! `tests/pkg_tests.rs`, which owns the project harness.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use spid3r_lang::value::VmFunc;
use spid3r_lang::vm::bytecode::{
    deserialize_vmfunc, serialize_vmfunc, validate_chunk, Chunk, OpCode, VmFuncProto,
};

#[path = "../build_support/webview2.rs"]
mod webview2;

fn func_with(chunk: Chunk) -> VmFunc {
    VmFunc {
        name: None,
        arity: 0,
        min_arity: 0,
        defaults: Vec::new(),
        is_async: false,
        chunk: Arc::new(chunk),
        upvalues: Vec::new(),
        own_upvalues: 0,
    }
}

/// A minimal valid chunk: `Return`.
fn simple_chunk() -> Chunk {
    let mut c = Chunk::new();
    c.emit(OpCode::Return);
    c
}

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(bin())
        .args(args)
        .output()
        .expect("spawn spid3r");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

// --------------------------------------------------------------------- A-8

/// Appending bytes after the last structure must be rejected: a file whose
/// declared structures do not consume the buffer is malformed.
#[test]
fn bytecode_with_trailing_bytes_is_rejected() {
    let mut bytes = serialize_vmfunc(&func_with(simple_chunk()));
    let clean = deserialize_vmfunc(&bytes).expect("valid bytecode round-trips");
    validate_chunk(&clean.chunk).expect("valid chunk validates");

    bytes.push(0xAA);
    let err = match deserialize_vmfunc(&bytes) {
        Ok(_) => panic!("trailing byte must be rejected"),
        Err(e) => e,
    };
    assert!(
        err.message.contains("trailing"),
        "error should mention trailing bytes: {}",
        err.message
    );
}

/// `inst_offsets` and `inst_spans` are read with independent counts, so a
/// crafted file can make them disagree. `span_at` used to index `inst_spans`
/// blindly and panicked; validation now rejects the file before execution.
#[test]
fn mismatched_instruction_tables_are_rejected() {
    let mut chunk = Chunk::new();
    chunk.emit(OpCode::Return);
    // Two offsets, one span — the shape a hand-edited file can produce.
    chunk.inst_offsets.push(1);
    assert_eq!(chunk.inst_offsets.len(), 2);
    assert_eq!(chunk.inst_spans.len(), 1);
    let err = validate_chunk(&chunk).expect_err("mismatched tables must be rejected");
    assert!(
        err.message.contains("instruction tables disagree"),
        "unexpected error: {}",
        err.message
    );

    // And the accessor itself is total: no panic, falls back to pending_span.
    let span = chunk.span_at(0);
    assert_eq!(span.line, chunk.pending_span.line);
}

/// A chunk that can fall through the end would make the VM fetch `code[ip]`
/// past the buffer; the validator must require a terminator.
#[test]
fn a_fall_through_chunk_end_is_rejected() {
    let mut chunk = Chunk::new();
    chunk.emit(OpCode::Pop);
    let err = validate_chunk(&chunk).expect_err("fall-through end must be rejected");
    assert!(
        err.message.contains("fall-through"),
        "unexpected error: {}",
        err.message
    );

    // An empty code section has no terminator at all.
    let empty = Chunk::new();
    let err = validate_chunk(&empty).expect_err("empty code must be rejected");
    assert!(
        err.message.contains("empty code"),
        "unexpected: {}",
        err.message
    );
}

/// Deeply nested function chunks must hit the deserializer's depth cap instead
/// of recursing until the host stack overflows (an uncatchable abort).
#[test]
fn deeply_nested_bytecode_hits_the_depth_limit() {
    let mut inner = simple_chunk();
    for _ in 0..200 {
        let mut outer = Chunk::new();
        outer.emit(OpCode::Return);
        outer.functions.push(Arc::new(VmFuncProto {
            name: None,
            arity: 0,
            min_arity: 0,
            defaults: Vec::new(),
            is_async: false,
            upvalues: Vec::new(),
            own_upvalues: 0,
            chunk: Arc::new(inner),
        }));
        inner = outer;
    }
    let bytes = serialize_vmfunc(&func_with(inner));
    let err = match deserialize_vmfunc(&bytes) {
        Ok(_) => panic!("200 levels of nesting must be rejected"),
        Err(e) => e,
    };
    assert!(
        err.message.contains("depth limit"),
        "error should name the depth limit: {}",
        err.message
    );
}

/// Jump-target validation must stay linear-ish: the old `Vec::contains` scan
/// made a file with many jumps cost O(jumps × instructions), which is a CPU
/// DoS at every `exec` entry. 50k jumps used to take tens of seconds in a debug
/// build; the binary search takes milliseconds.
#[test]
fn jump_validation_is_not_quadratic() {
    let mut chunk = Chunk::new();
    for _ in 0..50_000 {
        chunk.emit_i16(OpCode::Jump, 0); // falls through to the next instruction
    }
    chunk.emit(OpCode::Return);
    let started = Instant::now();
    validate_chunk(&chunk).expect("all jumps land on instruction boundaries");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "validation took {:?}; jump checking is quadratic again",
        elapsed
    );
}

/// End-to-end: a corrupted `.matrixc` must exit 1 with a diagnostic on stderr,
/// never abort the process (a Rust panic would surface as exit code 101 or a
/// STATUS_STACK_OVERFLOW/0xC0000409 on Windows).
#[test]
fn exec_rejects_a_corrupted_matrixc_without_crashing() {
    let dir = std::env::temp_dir().join(format!("spid3r_stage3_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let src = dir.join("hello.matrix");
    std::fs::write(
        &src,
        "fn main() {\n    println(\"hi\");\n    return 0;\n}\n",
    )
    .expect("write");
    let good = dir.join("hello.matrixc");
    let (code, out, err) = run(&[
        "compile",
        src.to_str().unwrap(),
        "-o",
        good.to_str().unwrap(),
    ]);
    assert_eq!(code, Some(0), "compile failed: {}{}", out, err);
    let bytes = std::fs::read(&good).expect("read bytecode");
    assert!(deserialize_vmfunc(&bytes).is_ok());

    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("trailing", {
            let mut b = bytes.clone();
            b.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
            b
        }),
        ("truncated", bytes[..bytes.len() - 5].to_vec()),
        ("empty", Vec::new()),
        ("magic", b"NOTSPID3R".to_vec()),
    ];
    for (name, data) in cases {
        let path = dir.join(format!("bad_{}.matrixc", name));
        std::fs::write(&path, &data).expect("write");
        let (code, _out, err) = run(&["exec", path.to_str().unwrap()]);
        assert_eq!(
            code,
            Some(1),
            "{}: expected a clean user error (exit 1), got {:?} ({})",
            name,
            code,
            err
        );
        assert!(
            !err.contains("panicked"),
            "{}: the host must not panic: {}",
            name,
            err
        );
        assert!(
            err.contains("error["),
            "{}: expected a Spid3r diagnostic, got: {}",
            name,
            err
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// -------------------------------------------------------------------- A-11

/// The policy seam: only affirmative values enable the download.
#[test]
fn webview2_fetch_is_opt_in() {
    for v in [None, Some(""), Some("0"), Some("no"), Some("off")] {
        assert!(
            !webview2::enabled_from(v),
            "{:?} must not enable the fetch",
            v
        );
    }
    for v in [Some("1"), Some("true"), Some("YES"), Some(" on ")] {
        assert!(webview2::enabled_from(v), "{:?} must enable the fetch", v);
    }
}

/// A checksum override must be a full 64-hex digest; anything else falls back to
/// the pinned value instead of silently disabling verification.
#[test]
fn webview2_checksum_override_must_be_a_full_digest() {
    let pinned = webview2::expected_from(None).expect("a pinned hash must exist");
    assert_eq!(pinned.len(), 64);
    assert_eq!(
        webview2::expected_from(Some("deadbeef")),
        Some(pinned.clone())
    );
    assert_eq!(
        webview2::expected_from(Some("zz")).as_deref(),
        Some(pinned.as_str())
    );
    let custom = "a".repeat(64);
    assert_eq!(webview2::expected_from(Some(&custom)), Some(custom));
}

/// Verification rejects a tampered file and a partial download, and accepts the
/// exact bytes it was given the hash of.
#[test]
fn webview2_verification_fails_closed() {
    let dir = std::env::temp_dir().join(format!("spid3r_wv2_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let file = dir.join("setup.exe");
    let payload = b"not-a-real-bootstrapper-but-stable";
    std::fs::write(&file, payload).expect("write");
    let good = spid3r_lang::pkg::sha256_hex(payload);

    let actual = webview2::verify_file(&file, &good, payload.len() as u64).expect("hash matches");
    assert_eq!(actual, good);

    // Tampered: same length, different bytes.
    std::fs::write(&file, b"not-a-real-bootstrapper-but-stablE").expect("write");
    let err = webview2::verify_file(&file, &good, payload.len() as u64)
        .expect_err("a changed byte must be rejected");
    assert!(err.contains("checksum mismatch"), "unexpected: {}", err);

    // Partial download: shorter than the minimum.
    let err =
        webview2::verify_file(&file, &good, 10_000).expect_err("a short file must be rejected");
    assert!(err.contains("partial download"), "unexpected: {}", err);

    // No file at all is an error, not a panic.
    assert!(webview2::verify_file(&dir.join("missing.exe"), &good, 0).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// With the fetch disabled, `ensure_with` never touches the network and never
/// fabricates a cached file; a verified cache is reused; an unverified cache is
/// refused (fail closed).
#[test]
fn webview2_ensure_never_accepts_an_unverified_file() {
    let dir = std::env::temp_dir().join(format!("spid3r_wv2e_{}", std::process::id()));
    let redist = dir.join("redist");
    std::fs::create_dir_all(&redist).expect("mkdir");
    let dest = redist.join("MicrosoftEdgeWebview2Setup.exe");

    // 1. Nothing cached, fetch disabled: no file, no download.
    assert!(webview2::ensure_with(&dir, false, Some("00"), 0).is_none());
    assert!(!dest.exists());

    // 2. A cached file that matches: reused without fetching.
    let payload = b"cached-bootstrapper";
    std::fs::write(&dest, payload).expect("write");
    let hash = spid3r_lang::pkg::sha256_hex(payload);
    assert_eq!(
        webview2::ensure_with(&dir, false, Some(&hash), payload.len() as u64),
        Some(dest.clone())
    );

    // 3. A cached file that does not match, fetch disabled: refused, kept.
    assert!(webview2::ensure_with(&dir, false, Some("11"), 0).is_none());
    assert!(dest.exists(), "a disabled fetch must not delete the cache");

    // 4. No hash at all: nothing is accepted (fail closed).
    assert!(webview2::ensure_with(&dir, true, None, 0).is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

/// Sanity: the pinned digest is a well-formed SHA-256 and the pinned size is
/// plausible for the bootstrapper (~1.8 MB), so a typo cannot silently disable
/// verification.
#[test]
fn webview2_pins_a_plausible_artifact() {
    let pinned = webview2::PINNED_SHA256;
    assert_eq!(pinned.len(), 64);
    assert!(pinned.chars().all(|c| c.is_ascii_hexdigit()));
    // The size is the `min_bytes` floor `ensure` passes to `verify_file`; a
    // zero/absurd value would disable the partial-download check.
    let floor = webview2::PINNED_BYTES;
    assert!(floor != 0 && floor < u64::MAX / 2, "implausible size floor");
    assert!(webview2::URL.starts_with("https://"));
}
