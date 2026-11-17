//! HTTPS support for `http_fetch` (the `tls` feature).
//!
//! The HTTPS test performs a REAL fetch of https://example.com and is
//! skipped (not failed) when the machine has no outbound network access —
//! CI runners and offline environments must stay green. The http:// path is
//! unchanged by this feature and is covered by the differential suite.

#![cfg(feature = "tls")]

#[path = "common/tls_fixture.rs"]
mod tls_fixture;

use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tls_fixture::{escape_matrix_str, TempTlsFixture, LOCALHOST_CERT_PEM};

/// Per-invocation counter: the two tests in this file run in parallel
/// threads, and they used to share ONE fixed temp path, so whichever test
/// finished first deleted the file the other one was still running
/// ("... is not a file"). Every call now gets its own file name.
static SEQ: AtomicU32 = AtomicU32::new(0);

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

fn run_with_engine(engine: &str, src: &str) -> (i32, String, String) {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join("spid3r_tls_fetch");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("tls_fetch_{}_{}.matrix", std::process::id(), seq));
    std::fs::write(&path, src).expect("write case");
    let o = Command::new(bin())
        .args(["run", "--engine", engine, path.to_str().unwrap()])
        .output()
        .expect("spawn spid3r");
    let _ = std::fs::remove_file(&path);
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

fn run(src: &str) -> (i32, String, String) {
    run_with_engine("vm", src)
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

#[test]
fn local_https_fetch_succeeds_with_explicit_ca_cert() {
    let fix = TempTlsFixture::new();
    let p12 = escape_matrix_str(&fix.p12_path);
    let cert_pem = escape_matrix_str(&fix.cert_pem_path);

    let src = format!(
        r#"
import "http";

fn main() {{
    let l = tls_listen("{p12}", "", "127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(l).unwrap();
    let parts = addr.split(":");
    let port = parts[len(parts) - 1];

    let done = channel();
    spawn fn() {{
        // Request 1: builtin __http_fetch GET
        let c1 = tls_accept(l, 5000).unwrap();
        let r1 = http_read_request(c1).unwrap();
        assert_eq(r1["method"], "GET");
        assert_eq(r1["path"], "/hello");
        http_write_response(c1, 200, map("x-srv", "tls1"), "hi-get").unwrap();
        tcp_close(c1);

        // Request 2: stdlib/http.matrix http_fetch POST
        let c2 = tls_accept(l, 5000).unwrap();
        let r2 = http_read_request(c2).unwrap();
        assert_eq(r2["method"], "POST");
        assert_eq(r2["path"], "/post");
        assert_eq(r2["headers"]["x-tok"], "abc");
        assert_eq(r2["body"], "payload");
        http_write_response(c2, 200, map("x-srv", "tls2"), "hi-post").unwrap();
        tcp_close(c2);

        done.send("ok");
    }};

    let opts = map("ca_cert", "{cert_pem}");
    let res1 = __http_fetch("GET", "https://localhost:" + port + "/hello", null, null, opts).unwrap();
    assert_eq(res1["status"], 200);
    assert_eq(res1["headers"]["x-srv"], "tls1");
    assert_eq(res1["body"], "hi-get");

    let res2 = http_fetch(
        "POST",
        "https://localhost:" + port + "/post",
        map("x-tok", "abc"),
        "payload",
        opts
    ).unwrap();
    assert_eq(res2["status"], 200);
    assert_eq(res2["headers"]["x-srv"], "tls2");
    assert_eq(res2["body"], "hi-post");

    assert_eq(done.recv(), "ok");
    tcp_close(l);
    println("PASS");
    return 0;
}}
"#
    );

    for engine in ["tree", "vm"] {
        let (code, out, err) = run_with_engine(engine, &src);
        assert_eq!(code, 0, "[{engine}] script failed: {err}\nstdout: {out}");
        assert!(out.lines().any(|l| l == "PASS"), "[{engine}] stdout: {out}");
    }
}

#[test]
fn local_https_fetch_succeeds_with_ca_pem_and_rejects_bad_pem() {
    let fix = TempTlsFixture::new();
    let p12 = escape_matrix_str(&fix.p12_path);
    let pem_str = std::str::from_utf8(LOCALHOST_CERT_PEM)
        .expect("utf8 pem")
        .replace('\r', "");
    let escaped_pem = pem_str
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");

    let src = format!(
        r#"
fn main() {{
    let l = tls_listen("{p12}", "", "127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(l).unwrap();
    let parts = addr.split(":");
    let port = parts[len(parts) - 1];

    let done = channel();
    spawn fn() {{
        let c1 = tls_accept(l, 5000).unwrap();
        let req = http_read_request(c1).unwrap();
        assert_eq(req["path"], "/pem");
        http_write_response(c1, 200, map(), "pem-ok").unwrap();
        tcp_close(c1);

        // Drain the bad-pem client attempt if it didn't even connect, just close listener later.
        done.send("ok");
    }};

    let pem = "{escaped_pem}";
    let ok_res = http_fetch("GET", "https://localhost:" + port + "/pem", null, null, map("ca_pem", pem)).unwrap();
    assert_eq(ok_res["status"], 200);
    assert_eq(ok_res["body"], "pem-ok");

    let bad_res = http_fetch("GET", "https://localhost:" + port + "/pem", null, null, map("ca_pem", "not-a-cert"));
    assert(is_failure(bad_res));

    assert_eq(done.recv(), "ok");
    tcp_close(l);
    println("PASS");
    return 0;
}}
"#
    );

    for engine in ["tree", "vm"] {
        let (code, out, err) = run_with_engine(engine, &src);
        assert_eq!(code, 0, "[{engine}] script failed: {err}\nstdout: {out}");
        assert!(out.lines().any(|l| l == "PASS"), "[{engine}] stdout: {out}");
    }
}

#[test]
fn local_https_fetch_rejects_untrusted_self_signed_cert_by_default() {
    let fix = TempTlsFixture::new();
    let p12 = escape_matrix_str(&fix.p12_path);

    let src = format!(
        r#"
fn main() {{
    let l = tls_listen("{p12}", "", "127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(l).unwrap();
    let parts = addr.split(":");
    let port = parts[len(parts) - 1];

    let done = channel();
    spawn fn() {{
        let r = tls_accept(l, 5000);
        if is_ok(r) {{
            let c = r.unwrap();
            let _ = http_read_request(c);
            tcp_close(c);
        }}
        done.send("ok");
    }};

    let res = http_fetch("GET", "https://localhost:" + port + "/hello");
    assert(is_failure(res));

    assert_eq(done.recv(), "ok");
    tcp_close(l);
    println("PASS");
    return 0;
}}
"#
    );

    for engine in ["tree", "vm"] {
        let (code, out, err) = run_with_engine(engine, &src);
        assert_eq!(code, 0, "[{engine}] script failed: {err}\nstdout: {out}");
        assert!(out.lines().any(|l| l == "PASS"), "[{engine}] stdout: {out}");
    }
}
