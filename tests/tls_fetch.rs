//! HTTPS support for `http_fetch` (the `tls` feature).
//!
//! The HTTPS test performs a REAL fetch of https://example.com and is
//! skipped (not failed) when the machine has no outbound network access —
//! CI runners and offline environments must stay green. The http:// path is
//! unchanged by this feature and is covered by the differential suite.

#![cfg(feature = "tls")]

use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn network_available() -> bool {
    std::net::ToSocketAddrs::to_socket_addrs(&("example.com", 443))
        .ok()
        .and_then(|mut addrs| addrs.next())
        .map(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(4)).is_ok())
        .unwrap_or(false)
}

fn run(src: &str) -> (i32, String, String) {
    let dir = std::env::temp_dir().join("spid3r_tls_fetch");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("tls_fetch.matrix");
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

#[test]
fn https_fetch_gets_example_com() {
    if !network_available() {
        eprintln!("skipping: no outbound network access");
        return;
    }
    let src = "\
fn main() {
    let r = http_fetch(\"GET\", \"https://example.com\").unwrap();
    println(r[\"status\"]);
    println(r[\"body\"].contains(\"Example Domain\"));
    println(r[\"body\"].contains(\"example.com\") or r[\"body\"].contains(\"example.org\"));
    return 0;
}
";
    let (code, out, err) = run(src);
    assert_eq!(code, 0, "script failed: {}", err);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.first().copied(), Some("200"), "stdout: {}", out);
    assert_eq!(lines.get(1).copied(), Some("true"), "body content mismatch");
}

#[test]
fn malformed_scheme_is_a_result_error() {
    // http_fetch reports failures as a catchable `err(...)` result, not a
    // thrown error — the script exits 0 and the result must be a failure.
    let (code, out, err) = run(
        "fn main() {\n    let r = http_fetch(\"GET\", \"ftp://example.com/x\");\n    println(is_failure(r));\n    return 0;\n}\n",
    );
    assert_eq!(code, 0, "script failed: {}", err);
    assert!(
        out.lines().any(|l| l == "true"),
        "ftp:// must produce a failure result, stdout: {}",
        out
    );
}
