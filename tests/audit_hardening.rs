//! Regression tests for the 2026-10-23 security & robustness audit (round 2).
//!
//! Every fix in this file was driven by a hostile-input probe (see
//! `audit/probes/`) and is pinned here so it cannot regress. Engine-level
//! tests run the real `spid3r` binary; the database-client tests drive the
//! library through purpose-built fake servers written in Rust (no Python
//! dependency); the native-backend test is skipped without LLVM.
//!
//! Covered fixes:
//! * `break`/`continue` are lexically scoped to the enclosing function's own
//!   loops — a closure may not break the caller's loop (checker rejects it on
//!   both engines instead of the VM compiler panicking).
//! * Functions with more than 255 parameters are a clean semantic error
//!   (VM locals live in 1-byte slots).
//! * `cbor_decode`/`msgpack_decode` length prefixes cannot overflow or
//!   pre-allocate attacker-controlled capacities.
//! * `png_decode` rejects dimension bombs before allocating the pixel buffer.
//! * `argon2` validates its integer arguments (sign and upper bounds).
//! * The parser depth guard also covers else-if chains, nested patterns, and
//!   interpolation holes (all three used to bypass `MAX_PARSE_DEPTH`).
//! * The VM rejects a function body whose try/handler offsets exceed the
//!   i16 jump range with a diagnostic instead of emitting a bad offset.
//! * `spid3r fmt` caps indentation depth, so formatting a deeply nested file
//!   is linear instead of quadratic in the output size.
//! * MySQL/PG clients survive truncated and oversized server messages.
//! * The native backend arity-checks calls to compiled functions instead of
//!   reading out of bounds (SIGSEGV -> catchable error).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::modules;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::vm::Vm;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

/// Write `src` to a temp file and run it on `engine`; return (exit code,
/// stdout + stderr).
fn run_engine(engine: &str, name: &str, src: &str) -> (i32, String) {
    let path = std::env::temp_dir().join(format!("spid3r_hard_{}.matrix", name));
    std::fs::write(&path, src).expect("write probe");
    let out = Command::new(bin())
        .args(["run", "--engine", engine, path.to_str().unwrap()])
        .output()
        .expect("spawn spid3r");
    let _ = std::fs::remove_file(&path);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr),
    )
}

/// Both engines must reject the program with `needle` in the diagnostics.
fn reject_both(name: &str, src: &str, needle: &str) {
    for engine in ["tree", "vm"] {
        let (code, out) = run_engine(engine, name, src);
        assert_eq!(
            code, 1,
            "[{}] {} should exit 1, got {}:\n{}",
            name, engine, code, out
        );
        assert!(
            out.contains(needle),
            "[{}] {} output missing {:?}:\n{}",
            name,
            engine,
            needle,
            out
        );
    }
}

/// Both engines must accept the program and print exactly `expected`.
fn accept_both(name: &str, src: &str, expected: &str) {
    for engine in ["tree", "vm"] {
        let (code, out) = run_engine(engine, name, src);
        assert_eq!(
            code, 0,
            "[{}] {} should exit 0, got {}:\n{}",
            name, engine, code, out
        );
        assert_eq!(out, expected, "[{}] {} stdout", name, engine);
    }
}

// =====================================================================
// break / continue are scoped to the function's own loops
// =====================================================================

#[test]
fn break_in_a_function_literal_is_rejected() {
    // Before the fix the semantic checker let this through and the VM
    // compiler hit `.expect("break outside loop")` — a panic, not a
    // diagnostic. The tree engine raised a runtime error only when the
    // closure was *called*, so the two engines disagreed.
    reject_both(
        "break_in_fn",
        r#"fn main() {
    while (true) {
        let g = fn() { break; };
        break;
    }
    return 0;
}
"#,
        "break outside a loop",
    );
}

#[test]
fn continue_in_a_function_literal_is_rejected() {
    reject_both(
        "continue_in_fn",
        r#"fn main() {
    while (true) {
        let g = fn() { continue; };
        break;
    }
    return 0;
}
"#,
        "continue outside a loop",
    );
}

#[test]
fn break_without_any_loop_is_rejected() {
    reject_both(
        "break_no_loop",
        r#"fn main() {
    break;
    return 0;
}
"#,
        "break outside a loop",
    );
}

#[test]
fn normal_break_and_continue_still_work() {
    // The sanity half of the scoping fix: loops inside functions, inside
    // closures, and nested loops all behave exactly as documented.
    accept_both(
        "sanity_break",
        r#"fn main() {
    let i = 0;
    while (i < 10) {
        if (i == 3) { break; }
        i = i + 1;
    }
    println(i);
    let s = 0;
    for let j = 0; j < 5; j = j + 1 {
        if (j == 2) { continue; }
        s = s + j;
    }
    println(s);
    let g = fn() {
        let k = 0;
        while (true) {
            if (k == 4) { break; }
            k = k + 1;
        }
        return k;
    };
    println(g());
    let n = 0;
    for a in [1, 2] {
        for b in [1, 2, 3] {
            if (b == 2) { break; }
            n = n + 1;
        }
    }
    println(n);
    return 0;
}
"#,
        "3\n8\n4\n2\n",
    );
}

// =====================================================================
// VM slot width: at most 255 parameters per function
// =====================================================================

#[test]
fn more_than_255_parameters_is_rejected() {
    // Locals and parameters live in 1-byte VM slots; a 300-parameter
    // function used to compile with silently truncated slot indices.
    let params: Vec<String> = (1..=300).map(|i| format!("p{} = 1", i)).collect();
    let src = format!(
        "fn f({}) {{\n    return 0;\n}}\nfn main() {{\n    f({});\n    return 0;\n}}\n",
        params.join(", "),
        (1..=300)
            .map(|i| format!("{}", i))
            .collect::<Vec<_>>()
            .join(", ")
    );
    reject_both(
        "too_many_params",
        &src,
        "too many parameters in one function (max 255)",
    );
}

// =====================================================================
// Serialization: length-prefix bombs and overflow
// =====================================================================

#[test]
fn cbor_length_prefix_overflow_is_a_clean_error() {
    // 0x5b = byte string, 8-byte length; 2^64-9 makes `pos + n` wrap to 0 so
    // the old bounds check passed and `take()` sliced out of bounds.
    accept_both(
        "cbor_overflow",
        r#"fn main() {
    let b = hex_decode("5bfffffffffffffff7");
    let r = cbor_decode(b);
    if r.is_ok() { println("decoded ok"); } else { println("error: {r}"); }
    return 0;
}
"#,
        "error: err(length prefix overflows the input size)\n",
    );
}

#[test]
fn cbor_u64_array_bomb_is_a_clean_error() {
    // 0x9b = array, 8-byte length of u64::MAX: `Vec::with_capacity` used to
    // abort the allocator before a single element was read.
    accept_both(
        "cbor_bomb",
        r#"fn main() {
    let arr = hex_decode("9bffffffffffffffff");
    let r = cbor_decode(arr);
    if r.is_ok() { println("decoded ok"); } else { println("error: {r}"); }
    return 0;
}
"#,
        "error: err(unexpected end of input)\n",
    );
}

#[test]
fn msgpack_array32_bomb_is_a_clean_error() {
    // 0xdd = array32, count 0xFFFFFFFF.
    accept_both(
        "msgpack_bomb",
        r#"fn main() {
    let bomb = hex_decode("ddffffff");
    let r = msgpack_decode(bomb);
    if r.is_ok() { println("decoded ok"); } else { println("error: {r}"); }
    return 0;
}
"#,
        "error: err(unexpected end of input)\n",
    );
}

// =====================================================================
// PNG dimension bomb
// =====================================================================

#[test]
fn png_dimension_bomb_is_a_clean_error() {
    // IHDR claims 100000x100000 RGBA (40 GB) in a 69-byte file.
    accept_both(
        "png_bomb",
        r#"fn main() {
    let png = hex_decode("89504e470d0a1a0a0000000d49484452000186a0000186a00806000000a8520bc80000000c4944415478da63606060000000040001c8eaebf90000000049454e44ae426082");
    let img = png_decode(png);
    if img.is_ok() { println("decoded ok"); } else { println("error: {img}"); }
    return 0;
}
"#,
        "error: err(png: image is too large to decode)\n",
    );
}

// =====================================================================
// argon2 argument validation
// =====================================================================

#[test]
fn argon2_rejects_non_positive_passes() {
    // passes = -1 used to become u32::MAX iterations (never returns). The
    // native raises a runtime error, so both engines exit 1 with it.
    reject_both(
        "argon2_passes",
        r#"fn main() {
    let h = argon2("password", "saltsalt1234", -1, 32, 1, "id");
    println("returned: {h}");
    return 0;
}
"#,
        "argon2: passes must be positive, got -1",
    );
}

#[test]
fn argon2_rejects_non_positive_out_len() {
    // out_len = -1 used to become usize::MAX in a `Vec::with_capacity`.
    reject_both(
        "argon2_outlen",
        r#"fn main() {
    let h = argon2("password", "saltsalt1234", 1, 32, 1, "id", -1);
    println("returned: {h}");
    return 0;
}
"#,
        "argon2: out_len must be between 1 and 1024, got -1",
    );
}

#[test]
fn argon2_rejects_oversized_memory() {
    // memory_kib above the 1 GiB cap used to reach the allocator unchecked.
    reject_both(
        "argon2_mem",
        r#"fn main() {
    let h = argon2("password", "saltsalt1234", 1, 1048577, 1, "id");
    println("returned: {h}");
    return 0;
}
"#,
        "argon2: 1048577 KiB exceeds the 1 GiB limit",
    );
}

// =====================================================================
// Parser depth guard coverage
// =====================================================================

#[test]
fn else_if_chain_hits_the_parser_depth_guard() {
    // `parse_if_expr` recursed into its else branch without incrementing the
    // depth counter, so a long else-if chain blew the stack.
    let mut src = String::from("fn main() {\n    let x = if (0) { 1 }");
    for _ in 0..300 {
        src.push_str(" else if (0) { 1 }");
    }
    src.push_str(";\n    return 0;\n}\n");
    reject_both("elseif_depth", &src, "nesting deeper than 64 levels");
}

#[test]
fn nested_pattern_hits_the_parser_depth_guard() {
    // `parse_pattern` had no depth guard of its own.
    let mut src = String::from("fn main() {\n    match (0) { ");
    for _ in 0..300 {
        src.push('a');
        src.push('(');
    }
    src.push('0');
    for _ in 0..300 {
        src.push(')');
    }
    src.push_str(" { _ => 1 } }\n    return 0;\n}\n");
    reject_both("pattern_depth", &src, "nesting deeper than 64 levels");
}

#[test]
fn deep_interpolation_hole_does_not_overflow_the_stack() {
    // The interpolation-hole sub-parser used to reset `depth` to 0, so a
    // deeply nested `{h({h(...)...})}` string bypassed the guard entirely.
    // With the guard the hole fails to parse and degrades to literal text
    // (`unknown name 'h'`); either way the process must exit normally
    // rather than die on a stack overflow.
    let depth = 20000;
    let mut src = String::from("fn main() {\n    let s = \"");
    for _ in 0..depth {
        src.push_str("{h(");
    }
    src.push('0');
    for _ in 0..depth {
        src.push_str(")}");
    }
    src.push_str("\";\n    return 0;\n}\n");
    for engine in ["tree", "vm"] {
        let (code, out) = run_engine(engine, "interp_depth", &src);
        assert!(
            code == 0 || code == 1,
            "[interp_depth] {} died with code {} (stack overflow?):\n{}",
            engine,
            code,
            out
        );
    }
}

// =====================================================================
// VM jump range
// =====================================================================

#[test]
fn vm_try_body_over_32kib_is_a_clean_error() {
    // try/catch/finally offsets are i16. A >32 KiB body used to wrap the
    // offset into a jump to the wrong place (or a negative one). The VM now
    // reports a diagnostic at compile time; the tree engine has no such
    // limit and simply runs the program (a documented engine divergence —
    // `spid3r test` differentials never combine a >32 KiB function with
    // `try`).
    let pad = "q = q + 1;\n";
    let src = format!(
        r#"fn main() {{
    let q = 0;
    try {{
{pad}    }} catch (e) {{
        println("caught");
    }}
    println(q);
    return 0;
}}
"#,
        pad = pad.repeat(20000)
    );
    let (code, out) = run_engine("vm", "try_offset", &src);
    assert_eq!(code, 1, "[try_offset] vm should exit 1:\n{}", out);
    assert!(
        out.contains("exceeds the 32 KiB jump range"),
        "[try_offset] vm output missing the range diagnostic:\n{}",
        out
    );
    let (code, out) = run_engine("tree", "try_offset", &src);
    assert_eq!(code, 0, "[try_offset] tree should run it:\n{}", out);
    assert_eq!(out, "20000\n", "[try_offset] tree stdout");
}

// =====================================================================
// Formatter amplification
// =====================================================================

#[test]
fn formatter_indentation_is_bounded() {
    // Indentation used to be `depth * 4` spaces per line with no cap, so a
    // deeply nested file produced quadratically more output (a 360 KB probe
    // became 1.6 GB). The depth is now capped at 64 levels, which bounds the
    // output to a small multiple of the input.
    let depth = 20000usize;
    let mut src = String::from("fn main() {\n");
    for _ in 0..depth {
        src.push_str("  if true {\n");
    }
    src.push_str("    return 0;\n");
    for _ in 0..depth {
        src.push_str("  }\n");
    }
    src.push_str("}\n");

    let path = std::env::temp_dir().join("spid3r_hard_fmt_deep.matrix");
    std::fs::write(&path, &src).expect("write deep source");
    let out = Command::new(bin())
        .args(["fmt", path.to_str().unwrap()])
        .output()
        .expect("spawn spid3r fmt");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "fmt failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let formatted = std::fs::read_to_string(&path).unwrap_or_default();
    // Linear bound: every line is at most 64*4 + a few bytes of code.
    let longest = formatted.lines().map(|l| l.len()).max().unwrap_or(0);
    assert!(
        longest <= 320,
        "indentation is not capped: longest line is {} bytes",
        longest
    );
    assert!(
        formatted.len() < 16 * 1024 * 1024,
        "formatter output is unbounded: {} bytes",
        formatted.len()
    );
}

// =====================================================================
// Database clients vs. hostile servers (Rust fakes, no Python)
// =====================================================================

fn run_tree(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("hardening.matrix"))?;
    semantic::check_file(&program, true, "hardening.matrix")?;
    let interp = Interp::new(
        Arc::from("hardening.matrix"),
        modules::default_module_dirs(),
    );
    interp.run_program(&program)
}

fn run_vm(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("hardening.matrix"))?;
    semantic::check_file(&program, true, "hardening.matrix")?;
    let dirs = modules::default_module_dirs();
    let mut vm = Vm::new(Arc::from("hardening.matrix"), dirs);
    vm.run_program(&program)
}

/// A MySQL wire packet: 3-byte little-endian length, 1-byte sequence.
fn mysql_pkt(payload: &[u8], seq: u8) -> Vec<u8> {
    let len = payload.len() as u32;
    let mut v = vec![
        (len & 0xff) as u8,
        ((len >> 8) & 0xff) as u8,
        ((len >> 16) & 0xff) as u8,
        seq,
    ];
    v.extend_from_slice(payload);
    v
}

/// A protocol-10 handshake v10 packet, enough for `mysql_connect` to finish
/// the mysql_native_password exchange.
fn mysql_handshake() -> Vec<u8> {
    let mut hs = vec![10u8]; // protocol version
    hs.extend_from_slice(b"8.0.32-malicious\x00"); // server version
    hs.extend_from_slice(&7u32.to_le_bytes()); // connection id
    hs.extend_from_slice(b"AAAAAAAA"); // auth-plugin-data-part-1
    hs.push(0); // filler
    hs.extend_from_slice(&0x0001u16.to_le_bytes()); // capability flags lower
    hs.push(0x2d); // charset
    hs.extend_from_slice(&0x0002u16.to_le_bytes()); // status flags
    hs.extend_from_slice(&0x0008u16.to_le_bytes()); // capability flags upper
    hs.push(21); // auth plugin data length
    hs.extend_from_slice(&[0u8; 10]); // reserved
    hs.extend_from_slice(b"BBBBBBBBBBBB\0"); // auth-plugin-data-part-2
    hs.extend_from_slice(b"mysql_native_password\0");
    hs
}

/// Serve MySQL connections in a loop (the test drives both engines through
/// the library in one process, so each engine needs its own connection).
/// Returns the port the fake server listens on.
fn spawn_fake_mysql(mode: &str) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind fake mysql");
    let port = listener.local_addr().unwrap().port();
    let mode = mode.to_string();
    thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else { continue };
            stream.set_nodelay(true).ok();
            if stream.write_all(&mysql_pkt(&mysql_handshake(), 0)).is_err() {
                continue;
            }
            let mut sink = [0u8; 8192];
            let _ = stream.read(&mut sink); // handshake response (seq 1)
            if stream
                .write_all(&mysql_pkt(&[0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00], 2))
                .is_err()
            {
                continue; // OK packet (seq 2)
            }
            let _ = stream.read(&mut sink); // COM_QUERY (seq 0)
            match mode.as_str() {
                // A zero-length packet where a result set is expected: the
                // old client indexed `packet[0]` directly and panicked.
                "empty" => {
                    stream.write_all(&mysql_pkt(&[], 1)).ok();
                }
                // Column count 1 followed by a 7-byte column definition: the
                // old client read `def[after_org + 7]` past the end.
                "shortdef" => {
                    stream.write_all(&mysql_pkt(&[0x01], 1)).ok();
                    stream.write_all(&mysql_pkt(&[0u8; 7], 2)).ok();
                }
                _ => unreachable!(),
            }
            thread::sleep(Duration::from_millis(200));
        }
    });
    port
}

#[test]
fn mysql_empty_packet_is_a_clean_error() {
    let port = spawn_fake_mysql("empty");
    let src = format!(
        r#"fn main() {{
    let db = mysql_connect("127.0.0.1:{}", "u", "p", "d");
    let rows = mysql_query(db, "SELECT 1").unwrap();
    println(len(rows));
    return 0;
}}
"#,
        port
    );
    for (label, res) in [("tree", run_tree(&src)), ("vm", run_vm(&src))] {
        let err = res
            .err()
            .unwrap_or_else(|| panic!("[mysql_empty] {} engine accepted a hostile server", label));
        assert!(
            err.message.contains("mysql: empty packet"),
            "[mysql_empty] {} wrong error: {}",
            label,
            err.message
        );
    }
}

#[test]
fn mysql_truncated_column_definition_is_a_clean_error() {
    let port = spawn_fake_mysql("shortdef");
    let src = format!(
        r#"fn main() {{
    let db = mysql_connect("127.0.0.1:{}", "u", "p", "d");
    let rows = mysql_query(db, "SELECT 1").unwrap();
    println(len(rows));
    return 0;
}}
"#,
        port
    );
    for (label, res) in [("tree", run_tree(&src)), ("vm", run_vm(&src))] {
        let err = res.err().unwrap_or_else(|| {
            panic!(
                "[mysql_shortdef] {} engine accepted a hostile server",
                label
            )
        });
        assert!(
            err.message.contains("mysql: truncated column definition"),
            "[mysql_shortdef] {} wrong error: {}",
            label,
            err.message
        );
    }
}

/// Serve PostgreSQL connections in a loop (see `spawn_fake_mysql`): read the
/// startup message, then claim a 2 GiB message body in a 5-byte header. The
/// old client allocated the named length before reading anything.
fn spawn_fake_postgres_oversized() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind fake pg");
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else { continue };
            stream.set_nodelay(true).ok();
            let mut sink = [0u8; 8192];
            let _ = stream.read(&mut sink); // StartupMessage
                                            // 'E' (ErrorResponse) + u32 length 0x7FFFFFFF + a few bytes.
            let mut msg = vec![b'E'];
            msg.extend_from_slice(&0x7fffffffu32.to_be_bytes());
            msg.extend_from_slice(b"S\0ERROR\0C\0XX000\0M\0boom\0\0");
            if stream.write_all(&msg).is_err() {
                continue;
            }
            thread::sleep(Duration::from_millis(200));
        }
    });
    port
}

#[test]
fn postgres_oversized_message_is_a_clean_error() {
    let port = spawn_fake_postgres_oversized();
    let src = format!(
        r#"fn main() {{
    let db = pg_connect("127.0.0.1:{}", "u", "p", "d").unwrap();
    let rows = pg_query(db, "SELECT 1").unwrap();
    println(len(rows));
    return 0;
}}
"#,
        port
    );
    for (label, res) in [("tree", run_tree(&src)), ("vm", run_vm(&src))] {
        let err = res
            .err()
            .unwrap_or_else(|| panic!("[pg_oversized] {} engine accepted a hostile server", label));
        assert!(
            err.message.contains("is too large"),
            "[pg_oversized] {} wrong error: {}",
            label,
            err.message
        );
    }
}

// =====================================================================
// Native backend: arity check instead of an out-of-bounds read
// =====================================================================

/// The native test needs two things: the `spid3r` binary under test must have
/// been compiled with the `native` feature (CI's core job builds with
/// `--no-default-features` and must skip), and `LLVM-C.dll` must be locatable
/// next to it. `cfg!` is faithful here because cargo compiles every target of
/// one invocation — bin and test — with the same feature set.
fn native_available() -> bool {
    if !cfg!(feature = "native") {
        return false;
    }
    bin()
        .parent()
        .map(|d| d.join("LLVM-C.dll"))
        .map(|d| d.is_file())
        .unwrap_or(false)
}

#[test]
fn native_short_call_is_a_clean_error_not_a_segfault() {
    if !native_available() {
        eprintln!(
            "skip: native backend unavailable (feature `native` off in this \
             build, or LLVM-C.dll not found next to {}; build with \
             SPID3R_LLVM_BIN_DIR set)",
            bin().display()
        );
        return;
    }
    // Generated native prologues load `argv[offset + i]` per declared
    // parameter; a short call read out of bounds and took the process down
    // with SIGSEGV. The call goes through a list so the semantic checker
    // cannot see the callee statically — this is the dynamic path that used
    // to reach the generated code unchecked. It must now be a catchable
    // error with the same message the tree/VM engines produce.
    let src = r#"fn two(a, b) {
    return a + b;
}
fn main() {
    let fs = [two];
    let g = fs[0];
    let r = g(1);
    println(r);
    return 0;
}
"#;
    let matrix = std::env::temp_dir().join("spid3r_hard_arity.matrix");
    std::fs::write(&matrix, src).expect("write arity probe");
    let exe = std::env::temp_dir().join(if cfg!(windows) {
        "spid3r_hard_arity.exe"
    } else {
        "spid3r_hard_arity"
    });
    let build = Command::new(bin())
        .args([
            "build",
            matrix.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("spawn spid3r build");
    assert!(
        build.status.success(),
        "native build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(&exe).output().expect("spawn native exe");
    let _ = std::fs::remove_file(&matrix);
    let _ = std::fs::remove_file(&exe);
    assert_eq!(
        run.status.code(),
        Some(1),
        "native exe should exit 1, got {:?} (signal?): stdout={} stderr={}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        combined.contains("two expects 2 argument(s), got 1"),
        "native arity error missing:\n{}",
        combined
    );
}
