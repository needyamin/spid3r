//! Stdlib additions round (2026-10-22): pbkdf2_sha256, base64_* and the
//! udp_* natives.
//!
//! PBKDF2-HMAC-SHA256 is pinned against the three published test vectors for
//! P="password", S="salt" (dkLen 32, c = 1 / 2 / 4096 — widely reproduced,
//! e.g. in RFC drafts and cross-library test suites). Base64 is pinned
//! against the RFC 4648 §10 test alphabet vectors. UDP is exercised over
//! real loopback sockets in both directions.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run(name: &str, src: &str) -> (i32, String, String) {
    let dir = std::env::temp_dir().join("spid3r_stdlib_additions");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{}.matrix", name));
    std::fs::write(&path, src).expect("write case");
    let o = Command::new(bin())
        .args(["run", "--engine", "vm", path.to_str().unwrap()])
        .output()
        .expect("spawn spid3r");
    let _ = std::fs::remove_file(&path);
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

/// PBKDF2-HMAC-SHA256, P="password", S="salt", dkLen=32, c = 1 / 2 / 4096.
const PBKDF2_VECTORS: &[(&str, &str)] = &[
    (
        "1",
        "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b",
    ),
    (
        "2",
        "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43",
    ),
    (
        "4096",
        "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a",
    ),
];

#[test]
fn pbkdf2_matches_published_vectors() {
    let mut src = String::from("fn main() {\n");
    for (c, _) in PBKDF2_VECTORS {
        src.push_str(&format!(
            "    println(pbkdf2_sha256(\"password\", \"salt\", {}));\n",
            c
        ));
    }
    // A bytes salt must hash identically to the same string salt.
    src.push_str(
        "    let sb = bytes();\n    sb.push(115); sb.push(97); sb.push(108); sb.push(116);\n    println(pbkdf2_sha256(\"password\", sb, 2));\n",
    );
    src.push_str("    return 0;\n}\n");

    let (code, out, err) = run("pbkdf2_vectors", &src);
    assert_eq!(code, 0, "script failed: {}", err);
    let mut lines = out.lines();
    for (_, expected) in PBKDF2_VECTORS {
        assert_eq!(lines.next().unwrap(), *expected, "pbkdf2 vector mismatch");
    }
    // The bytes-salt variant equals the string-salt vector for c = 2.
    assert_eq!(
        lines.next().unwrap(),
        PBKDF2_VECTORS[1].1,
        "bytes salt must match string salt"
    );
}

#[test]
fn pbkdf2_rejects_bad_iteration_counts() {
    let (code, _, err) = run(
        "pbkdf2_bad_iters",
        "fn main() {\n    pbkdf2_sha256(\"p\", \"s\", 0);\n    return 0;\n}\n",
    );
    assert_ne!(code, 0, "iterations = 0 must be rejected");
    assert!(
        err.contains("iterations must be between"),
        "stderr: {}",
        err
    );
}

/// RFC 4648 §10: the canonical alphabet vectors.
const B64_VECTORS: &[(&str, &str)] = &[
    ("", ""),
    ("f", "Zg=="),
    ("fo", "Zm8="),
    ("foo", "Zm9v"),
    ("foob", "Zm9vYg=="),
    ("fooba", "Zm9vYmE="),
    ("foobar", "Zm9vYmFy"),
];

#[test]
fn base64_encode_matches_rfc4648_vectors() {
    let mut src = String::from("fn main() {\n");
    for (plain, encoded) in B64_VECTORS {
        src.push_str(&format!(
            "    if (base64_encode(\"{}\") != \"{}\") {{ println(\"FAIL enc {}\"); }}\n",
            plain, encoded, plain
        ));
    }
    src.push_str("    println(\"done\");\n    return 0;\n}\n");

    let (code, out, err) = run("base64_vectors", &src);
    assert_eq!(code, 0, "script failed: {}", err);
    assert!(
        !out.contains("FAIL"),
        "RFC 4648 encode vectors failed:\n{}",
        out
    );
    assert!(out.contains("done"));
}

#[test]
fn base64_encode_and_decode_round_trip() {
    // Encode all RFC vectors, then decode each encoded form and compare to
    // the original. Done as one script with explicit expected outputs so a
    // failure names the case.
    let mut src = String::from("fn main() {\n");
    for (plain, encoded) in B64_VECTORS {
        src.push_str(&format!(
            "    if (base64_encode(\"{}\") != \"{}\") {{ println(\"FAIL enc {}\"); }}\n",
            plain, encoded, plain
        ));
        src.push_str(&format!(
            "    if (base64_encode(base64_decode(\"{}\")) != \"{}\") {{ println(\"FAIL dec {}\"); }}\n",
            encoded, encoded, plain
        ));
    }
    // URL-safe alphabet: the standard encoding of these bytes contains +/ and
    // padding; the URL encoding must not.
    src.push_str("    let raw = bytes();\n");
    src.push_str("    raw.push(251); raw.push(239); raw.push(191);\n");
    src.push_str(
        "    let std_form = base64_encode(raw);\n    let url_form = base64_url_encode(raw);\n",
    );
    src.push_str("    println(std_form);\n    println(url_form);\n");
    src.push_str(
        "    println(string(base64_url_decode(url_form)) == string(base64_decode(std_form)));\n",
    );
    src.push_str("    println(\"done\");\n    return 0;\n}\n");

    let (code, out, err) = run("base64_roundtrip", &src);
    assert_eq!(code, 0, "script failed: {}", err);
    for (plain, _) in B64_VECTORS {
        assert!(
            !out.contains(&format!("FAIL enc {}", plain)),
            "encode mismatch for {:?}",
            plain
        );
        assert!(
            !out.contains(&format!("FAIL dec {}", plain)),
            "decode mismatch for {:?}",
            plain
        );
    }
    // 0xfb 0xef 0xbf in standard base64 is "++++" style output containing '+'
    // and '/', while the URL form uses '-_' and no padding.
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines.iter().any(|l| l.contains('+') && l.contains('/')));
    assert!(lines
        .iter()
        .any(|l| l.contains('-') && l.contains('_') && !l.contains('=')));
    assert_eq!(lines.last().copied(), Some("done"));
}

#[test]
fn udp_loopback_send_and_receive() {
    // Bind two ephemeral sockets, send A -> B, receive on B, check the
    // datagram text and the sender address; then close both.
    let src = "\
fn main() {
    let a = udp_bind(\"127.0.0.1:0\").unwrap();
    let b = udp_bind(\"127.0.0.1:0\").unwrap();
    let a_addr = udp_local_addr(a).unwrap();
    let written = udp_send_to(b, a_addr, \"ping\").unwrap();
    println(written);
    let r = udp_recv_from(a, 1024).unwrap();
    println(r[0]);
    println(r[1]);
    println(udp_close(a));
    println(udp_close(b));
    return 0;
}
";
    let (code, out, err) = run("udp_loopback", src);
    assert_eq!(code, 0, "script failed: {}", err);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "4", "send_to should report 4 bytes written");
    assert_eq!(lines[1], "ping");
    assert!(
        lines[2].starts_with("127.0.0.1:"),
        "sender address should be loopback, got {}",
        lines[2]
    );
    assert_eq!(lines[3], "true");
    assert_eq!(lines[4], "true");
}

#[test]
fn udp_closed_handle_is_an_error() {
    let (code, _, err) = run(
        "udp_closed",
        "fn main() {\n    let a = udp_bind(\"127.0.0.1:0\").unwrap();\n    udp_close(a);\n    udp_send_to(a, \"127.0.0.1:1\", \"x\");\n    return 0;\n}\n",
    );
    assert_ne!(code, 0, "using a closed handle must fail");
    assert!(err.contains("is not open"), "stderr: {}", err);
}
