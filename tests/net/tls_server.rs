//! TLS server natives (`tls_listen`, `tls_accept`) across Tree and VM engines.

#![cfg(feature = "tls")]

#[path = "../common/tls_fixture.rs"]
mod tls_fixture;

use native_tls::{Certificate, TlsConnector};
use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::modules;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::vm::Vm;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tls_fixture::{escape_matrix_str, TempTlsFixture, LOCALHOST_CERT_DER};

const CASE_TIMEOUT: Duration = Duration::from_secs(60);

type Runner = fn(&str, &str) -> Result<i32, Diagnostic>;

fn run_tree(src: &str, name: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from(name))?;
    semantic::check_file(&program, true, name)?;
    let interp = Interp::new(Arc::from(name), modules::default_module_dirs());
    interp.run_program(&program)
}

fn run_vm(src: &str, name: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from(name))?;
    semantic::check_file(&program, true, name)?;
    let dirs = modules::default_module_dirs();
    let mut vm = Vm::new(Arc::from(name), dirs);
    vm.run_program(&program)
}

fn run_both(name: &str, src: &str) {
    let cases: [(&str, Runner); 2] = [("tree", run_tree), ("vm", run_vm)];
    for (engine, run) in cases {
        let (tx, rx) = mpsc::channel();
        let src = src.to_string();
        let label = name.to_string();
        std::thread::Builder::new()
            .name(format!("{name}-{engine}"))
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                let _ = tx.send(run(&src, &label));
            })
            .expect("spawn case thread");
        match rx.recv_timeout(CASE_TIMEOUT) {
            Ok(Ok(0)) => {}
            Ok(Ok(code)) => panic!("[{name}/{engine}] exited {code}"),
            Ok(Err(e)) => panic!("[{name}/{engine}] failed: {e:?}"),
            Err(_) => panic!(
                "[{name}/{engine}] did not finish within {:?} — case is hung",
                CASE_TIMEOUT
            ),
        }
    }
}

fn run_both_with_client<F>(name: &str, make_src: impl Fn(&Path) -> String, client_fn: F)
where
    F: Fn(u16) + Send + Sync + Clone + 'static,
{
    let cases: [(&str, Runner); 2] = [("tree", run_tree), ("vm", run_vm)];
    for (engine, run) in cases {
        let fix = TempTlsFixture::new();
        let port_file = fix.dir.join(format!("port_{engine}.txt"));
        let src = make_src(&port_file);

        let port_file_client = port_file.clone();
        let cfn = client_fn.clone();
        let client_handle = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(15);
            let port: u16 = loop {
                if let Ok(text) = std::fs::read_to_string(&port_file_client) {
                    if let Ok(p) = text.trim().parse::<u16>() {
                        break p;
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for server port file"
                );
                std::thread::sleep(Duration::from_millis(10));
            };
            cfn(port);
        });

        let (tx, rx) = mpsc::channel();
        let label = name.to_string();
        std::thread::Builder::new()
            .name(format!("{name}-{engine}"))
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                let _ = tx.send(run(&src, &label));
            })
            .expect("spawn case thread");

        match rx.recv_timeout(CASE_TIMEOUT) {
            Ok(Ok(0)) => {}
            Ok(Ok(code)) => panic!("[{name}/{engine}] exited {code}"),
            Ok(Err(e)) => panic!("[{name}/{engine}] failed: {e:?}"),
            Err(_) => panic!(
                "[{name}/{engine}] did not finish within {:?} — case is hung",
                CASE_TIMEOUT
            ),
        }
        client_handle.join().expect("client thread panicked");
    }
}

fn rust_tls_connector() -> TlsConnector {
    let cert = Certificate::from_der(LOCALHOST_CERT_DER).expect("parse der cert");
    TlsConnector::builder()
        .add_root_certificate(cert)
        .build()
        .expect("build TlsConnector")
}

#[test]
fn tls_echo_and_binary_roundtrip_on_tree_and_vm() {
    let fix = TempTlsFixture::new();
    let p12 = escape_matrix_str(&fix.p12_path);

    run_both_with_client(
        "tls_echo_and_binary_roundtrip",
        move |port_file| {
            let pf = escape_matrix_str(port_file);
            format!(
                r#"
fn main() {{
    let l = tls_listen("{p12}", "", 0).unwrap();
    let addr = tcp_local_addr(l).unwrap();
    let parts = addr.split(":");
    let port = parts[len(parts) - 1];
    write_file_atomic("{pf}", port);

    let conn = tls_accept(l, 10000).unwrap();
    assert(conn != null);
    let msg = tcp_read(conn, 1024).unwrap();
    assert_eq(msg, "hello tls");
    tcp_write(conn, "ECHO:" + msg).unwrap();

    let raw = tcp_read_exact(conn, 5).unwrap();
    assert_eq(raw, bytes([0, 1, 254, 255, 128]));
    tcp_write_bytes(conn, raw).unwrap();

    tcp_close(conn);
    tcp_close(l);
    return 0;
}}
"#
            )
        },
        |port| {
            let connector = rust_tls_connector();
            let tcp = TcpStream::connect(("127.0.0.1", port)).expect("tcp connect");
            let mut tls = connector.connect("localhost", tcp).expect("tls handshake");
            tls.write_all(b"hello tls").unwrap();
            tls.flush().unwrap();

            let mut buf = [0u8; 14];
            tls.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"ECHO:hello tls");

            let payload = [0u8, 1, 254, 255, 128];
            tls.write_all(&payload).unwrap();
            tls.flush().unwrap();

            let mut reply = [0u8; 5];
            tls.read_exact(&mut reply).unwrap();
            assert_eq!(reply, payload);
        },
    );
}

#[test]
fn tls_accept_timeout_and_non_tls_peer_do_not_wedge() {
    let fix = TempTlsFixture::new();
    let p12 = escape_matrix_str(&fix.p12_path);

    run_both_with_client(
        "tls_accept_timeout_and_non_tls_peer",
        move |port_file| {
            let pf = escape_matrix_str(port_file);
            format!(
                r#"
fn main() {{
    let l = tls_listen("{p12}", null, "127.0.0.1:0").unwrap();
    let timed_out = tls_accept(l, 50);
    assert_eq(timed_out, null);

    let addr = tcp_local_addr(l).unwrap();
    let parts = addr.split(":");
    let port = parts[len(parts) - 1];
    write_file_atomic("{pf}", port);

    // First peer speaks plaintext HTTP instead of TLS ClientHello.
    let bad = tls_accept(l, 10000);
    assert(is_err(bad));

    // Listener must remain healthy for the subsequent valid TLS client.
    let conn = tls_accept(l, 10000).unwrap();
    assert(conn != null);
    let msg = tcp_read(conn, 1024).unwrap();
    assert_eq(msg, "after-bad");
    tcp_write(conn, "OK:" + msg).unwrap();

    tcp_close(conn);
    tcp_close(l);
    return 0;
}}
"#
            )
        },
        |port| {
            // Peer 1: plain HTTP without TLS handshake.
            {
                let mut plain = TcpStream::connect(("127.0.0.1", port)).expect("plain connect");
                let _ = plain.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n");
                let _ = plain.flush();
            }

            // Peer 2: real TLS client.
            let connector = rust_tls_connector();
            let tcp = TcpStream::connect(("127.0.0.1", port)).expect("tls tcp connect");
            let mut tls = connector.connect("localhost", tcp).expect("tls handshake");
            tls.write_all(b"after-bad").unwrap();
            tls.flush().unwrap();

            let mut buf = [0u8; 12];
            tls.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"OK:after-bad");
        },
    );
}

#[test]
fn tls_http_read_request_and_write_response_on_tree_and_vm() {
    let fix = TempTlsFixture::new();
    let p12 = escape_matrix_str(&fix.p12_path);

    run_both_with_client(
        "tls_http_read_request_and_write_response",
        move |port_file| {
            let pf = escape_matrix_str(port_file);
            format!(
                r#"
fn main() {{
    let l = tls_listen("{p12}", "", "127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(l).unwrap();
    let parts = addr.split(":");
    let port = parts[len(parts) - 1];
    write_file_atomic("{pf}", port);

    let conn = tls_accept(l, 10000).unwrap();
    assert(conn != null);

    // Poll before reading to exercise kernel_poll / conn.pending interaction.
    let ready = tcp_poll([conn], 5000).unwrap();
    assert(len(ready["readable"]) >= 0);

    let req = http_read_request(conn).unwrap();
    assert_eq(req["method"], "POST");
    assert_eq(req["path"], "/tls/echo");
    assert_eq(req["query"]["a"], "1");
    assert_eq(req["headers"]["x-client"], "spid3r");
    assert_eq(req["body"], "secret");

    http_write_response(conn, 200, map("x-tls", "ok"), "{{\"ok\":true}}").unwrap();
    tcp_close(conn);
    tcp_close(l);
    return 0;
}}
"#
            )
        },
        |port| {
            let connector = rust_tls_connector();
            let tcp = TcpStream::connect(("127.0.0.1", port)).expect("tcp connect");
            let mut tls = connector.connect("localhost", tcp).expect("tls handshake");
            let req = b"POST /tls/echo?a=1 HTTP/1.1\r\nhost: localhost\r\nx-client: spid3r\r\ncontent-length: 6\r\n\r\nsecret";
            tls.write_all(req).unwrap();
            tls.flush().unwrap();

            let mut resp = Vec::new();
            tls.read_to_end(&mut resp).unwrap();
            let resp_str = String::from_utf8(resp).expect("utf8 response");
            assert!(
                resp_str.starts_with("HTTP/1.1 200 OK\r\n"),
                "unexpected status: {resp_str}"
            );
            assert!(
                resp_str.to_ascii_lowercase().contains("x-tls: ok\r\n"),
                "missing x-tls header: {resp_str}"
            );
            assert!(
                resp_str.ends_with("\r\n\r\n{\"ok\":true}"),
                "unexpected body: {resp_str}"
            );
        },
    );
}

#[test]
fn tls_wss_handshake_via_ws_accept_on_tree_and_vm() {
    let fix = TempTlsFixture::new();
    let p12 = escape_matrix_str(&fix.p12_path);
    let cert_pem = escape_matrix_str(&fix.cert_pem_path);

    let src = format!(
        r#"
fn main() {{
    let l = tls_listen("{p12}", "", "127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(l).unwrap();
    let parts = addr.split(":");
    let port = parts[len(parts) - 1];

    let done = channel();
    spawn fn() {{
        // Connection 1: untrusted wss client rejects the self-signed cert.
        // Depending on TLS 1.2 vs 1.3 timing, server tls_accept may fail the
        // handshake directly or return a stream whose client immediately closes.
        let r1 = tls_accept(l, 5000);
        if is_ok(r1) {{
            let c1 = r1.unwrap();
            try {{
                let _ = ws_accept(c1);
            }} catch _e {{}}
            tcp_close(c1);
        }}

        // Connection 2: trusted wss client upgrades and exchanges text + binary frames.
        let conn = tls_accept(l, 5000).unwrap();
        let ws = ws_accept(conn).unwrap();
        let msg = ws_recv(ws).unwrap();
        assert_eq(msg, "ping");
        ws_send(ws, "wss:" + msg).unwrap();

        let bin = ws_recv(ws).unwrap();
        assert_eq(bin, bytes([0, 1, 254, 255]));
        ws_send_bytes(ws, bin).unwrap();

        sleep_ms(100);
        ws_close(ws);
        done.send("ok");
    }};

    // Without ca_cert, default OS trust store must reject the self-signed cert.
    let untrusted_threw = false;
    try {{
        let _bad = ws_connect("wss://localhost:" + port);
    }} catch e {{
        untrusted_threw = true;
    }}
    assert(untrusted_threw);

    // With explicit ca_cert, wss handshake and text/binary frames succeed.
    let ws = ws_connect("wss://localhost:" + port, map("ca_cert", "{cert_pem}")).unwrap();
    ws_send(ws, "ping").unwrap();
    assert_eq(ws_recv(ws).unwrap(), "wss:ping");

    let payload = bytes([0, 1, 254, 255]);
    ws_send_bytes(ws, payload).unwrap();
    assert_eq(ws_recv(ws).unwrap(), payload);

    ws_close(ws);
    assert_eq(done.recv(), "ok");
    tcp_close(l);
    return 0;
}}
"#
    );

    run_both("tls_wss_handshake_via_ws_accept", &src);
}

#[test]
fn tls_listen_rejects_pem_and_invalid_inputs_on_tree_and_vm() {
    let fix = TempTlsFixture::new();
    let p12 = escape_matrix_str(&fix.p12_path);
    let cert_pem = escape_matrix_str(&fix.cert_pem_path);
    let dummy_key = escape_matrix_str(&fix.dummy_key_path);
    let missing = escape_matrix_str(&fix.dir.join("does_not_exist.p12"));

    let src = format!(
        r#"
fn main() {{
    // PEM certificate + key rejected cleanly with PKCS#12 guidance.
    let pem_res = tls_listen("{cert_pem}", "{dummy_key}", "127.0.0.1:0");
    assert(is_err(pem_res));

    // Wrong passphrase for valid PKCS#12 rejected cleanly.
    let bad_pass = tls_listen("{p12}", "wrong-passphrase", "127.0.0.1:0");
    assert(is_err(bad_pass));

    // Missing file rejected cleanly.
    let missing_res = tls_listen("{missing}", "", "127.0.0.1:0");
    assert(is_err(missing_res));

    // Plain TCP listener cannot be passed to tls_accept.
    let plain = tcp_listen("127.0.0.1:0").unwrap();
    let threw_plain = false;
    try {{
        let _ = tls_accept(plain, 50);
    }} catch e {{
        threw_plain = true;
    }}
    assert(threw_plain);
    tcp_close(plain);

    // TLS listener cannot be passed to tcp_accept_timeout.
    let tl = tls_listen("{p12}", "", "127.0.0.1:0").unwrap();
    let threw_tls = false;
    try {{
        let _ = tcp_accept_timeout(tl, 50);
    }} catch e {{
        threw_tls = true;
    }}
    assert(threw_tls);
    tcp_close(tl);

    return 0;
}}
"#
    );

    run_both("tls_listen_rejects_pem_and_invalid_inputs", &src);
}

#[test]
fn tls_listen_and_ca_cert_enforce_security_policy() {
    let fix = TempTlsFixture::new();
    let allowed_dir = fix.dir.join("allowed");
    let outside_dir = fix.dir.join("outside");
    std::fs::create_dir_all(&allowed_dir).expect("mkdir allowed");
    std::fs::create_dir_all(&outside_dir).expect("mkdir outside");

    let inside_p12 = allowed_dir.join("inside.p12");
    let outside_p12 = outside_dir.join("outside.p12");
    let outside_cert = outside_dir.join("outside_cert.pem");
    std::fs::copy(&fix.p12_path, &inside_p12).expect("copy inside p12");
    std::fs::copy(&fix.p12_path, &outside_p12).expect("copy outside p12");
    std::fs::copy(&fix.cert_pem_path, &outside_cert).expect("copy outside cert");

    let ini_path = fix.dir.join("spid3r.ini");
    std::fs::write(
        &ini_path,
        format!(
            "[security]\nbasedir = {}\n",
            escape_matrix_str(&allowed_dir)
        ),
    )
    .expect("write ini");

    let script_path = allowed_dir.join("policy_check.matrix");
    let inside_p12_s = escape_matrix_str(&inside_p12);
    let outside_p12_s = escape_matrix_str(&outside_p12);
    let outside_cert_s = escape_matrix_str(&outside_cert);
    std::fs::write(
        &script_path,
        format!(
            r#"
fn main() {{
    // Inside basedir succeeds.
    let l = tls_listen("{inside_p12_s}", "", "127.0.0.1:0").unwrap();
    tcp_close(l);

    // Outside basedir is rejected by fs_policy guard on tls_listen.
    let denied_listen = false;
    try {{
        let _ = tls_listen("{outside_p12_s}", "", "127.0.0.1:0");
    }} catch e {{
        assert(string(e).contains("path outside basedir"));
        denied_listen = true;
    }}
    assert(denied_listen);

    // Outside basedir is rejected by check_path on http_fetch ca_cert.
    let fetch_res = http_fetch("GET", "https://localhost:1/", null, null, map("ca_cert", "{outside_cert_s}"));
    assert(is_err(fetch_res));
    println("POLICY_OK");
    return 0;
}}
"#
        ),
    )
    .expect("write policy script");

    let bin = env!("CARGO_BIN_EXE_spid3r");
    for engine in ["tree", "vm"] {
        let out = std::process::Command::new(bin)
            .env("SPID3R_INI", &ini_path)
            .args(["run", "--engine", engine, script_path.to_str().unwrap()])
            .output()
            .expect("run spid3r basedir policy test");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "[{engine}] basedir test failed: {stderr}\nstdout: {stdout}"
        );
        assert!(stdout.contains("POLICY_OK"), "[{engine}] stdout: {stdout}");
    }

    // --sandbox denies tls_listen as a Network capability.
    let sandbox_script = fix.dir.join("sandbox_check.matrix");
    let p12_s = escape_matrix_str(&fix.p12_path);
    std::fs::write(
        &sandbox_script,
        format!(
            r#"
fn main() {{
    let denied = false;
    try {{
        let _ = tls_listen("{p12_s}", "", "127.0.0.1:0");
    }} catch e {{
        assert(string(e).contains("sandboxed: network access denied"));
        denied = true;
    }}
    assert(denied);
    println("SANDBOX_OK");
    return 0;
}}
"#
        ),
    )
    .expect("write sandbox script");

    for engine in ["tree", "vm"] {
        let out = std::process::Command::new(bin)
            .args([
                "run",
                "--sandbox",
                "--engine",
                engine,
                sandbox_script.to_str().unwrap(),
            ])
            .output()
            .expect("run spid3r sandbox test");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "[{engine}] sandbox test failed: {stderr}\nstdout: {stdout}"
        );
        assert!(stdout.contains("SANDBOX_OK"), "[{engine}] stdout: {stdout}");
    }
}
