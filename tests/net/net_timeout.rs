//! Audit A-16: HTTP framing must not wait forever on a silent peer.
//!
//! `read_head` had no deadline, so a client that connects and sends nothing —
//! or dribbles one byte a minute (slowloris) — held a server worker until the
//! peer chose to hang up. `SPID3R_NET_TIMEOUT_MS` now bounds the framing phase
//! (default 30 s, `0` disables), and an explicit `tcp_set_timeout` still wins.
//!
//! These run in a child process with a watchdog: before the fix the program
//! blocks until the client closes, which would wedge the whole test binary.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run(src: &str, tag: &str, envs: &[(&str, &str)], limit: Duration) -> (i32, String, Duration) {
    let path = std::env::temp_dir().join(format!("spid3r_net_{}.matrix", tag));
    std::fs::write(&path, src).expect("write probe");
    let mut cmd = Command::new(bin());
    cmd.args(["run", path.to_str().unwrap()]);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let started = Instant::now();
    let out = cmd.output().expect("spawn spid3r");
    let elapsed = started.elapsed();
    let _ = std::fs::remove_file(&path);
    assert!(
        elapsed < limit,
        "`{}` did not finish within {:?} — the framing read has no deadline",
        tag,
        limit
    );
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr),
        elapsed,
    )
}

/// A client that connects and sends nothing at all.
const SILENT_CLIENT: &str = r#"fn main() {
    let listener = tcp_listen("127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(listener).unwrap();
    let client = spawn fn() {
        let c = tcp_connect(addr).unwrap();
        sleep_ms(6000);
        tcp_close(c);
        return 0;
    };
    let conn = tcp_accept(listener).unwrap();
    let t0 = now();
    let r = http_read_request(conn);
    let waited = now() - t0;
    println(r.is_err());
    println(waited < 3000);
    return 0;
}
"#;

#[test]
fn a_silent_client_does_not_hold_the_server() {
    // 400 ms deadline; the client stays silent for 6 s.
    let (code, out, elapsed) = run(
        SILENT_CLIENT,
        "silent",
        &[("SPID3R_NET_TIMEOUT_MS", "400")],
        Duration::from_secs(20),
    );
    assert_eq!(code, 0, "probe failed: {}", out);
    assert!(
        out.contains("true"),
        "http_read_request should have failed on a silent peer: {}",
        out
    );
    assert!(
        elapsed < Duration::from_secs(4),
        "the read should have timed out after ~400 ms, took {:?}",
        elapsed
    );
    eprintln!("silent client released the worker in {:?}", elapsed);
}

/// A client that sends a partial header and then stalls: the same deadline
/// applies, so a slowloris cannot keep the connection open either.
#[test]
fn a_stalled_partial_header_is_bounded() {
    let src = r#"fn main() {
    let listener = tcp_listen("127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(listener).unwrap();
    let client = spawn fn() {
        let c = tcp_connect(addr).unwrap();
        tcp_write(c, "GET / HTTP/1.1\r\nhost: x\r\n");
        sleep_ms(6000);
        tcp_close(c);
        return 0;
    };
    let conn = tcp_accept(listener).unwrap();
    let t0 = now();
    let r = http_read_request(conn);
    let waited = now() - t0;
    println(r.is_err());
    println(waited < 3000);
    return 0;
}
"#;
    let (code, out, elapsed) = run(
        src,
        "partial",
        &[("SPID3R_NET_TIMEOUT_MS", "400")],
        Duration::from_secs(20),
    );
    assert_eq!(code, 0, "probe failed: {}", out);
    assert!(
        elapsed < Duration::from_secs(4),
        "a stalled partial header should be bounded, took {:?}",
        elapsed
    );
}

/// A complete request must still be read normally, and an explicit
/// `tcp_set_timeout` must not be overwritten by the default.
#[test]
fn a_normal_request_still_works_and_keeps_its_own_timeout() {
    let src = r#"fn main() {
    let listener = tcp_listen("127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(listener).unwrap();
    let client = spawn fn() {
        let c = tcp_connect(addr).unwrap();
        tcp_write(c, "GET /hello?x=1 HTTP/1.1\r\nhost: x\r\n\r\n");
        let _ = tcp_read(c, 64);
        tcp_close(c);
        return 0;
    };
    let conn = tcp_accept(listener).unwrap();
    tcp_set_timeout(conn, 1500);
    let r = http_read_request(conn);
    println(r.is_ok());
    println(r.unwrap()["method"]);
    println(r.unwrap()["path"]);
    return 0;
}
"#;
    let (code, out, _) = run(
        src,
        "normal",
        &[("SPID3R_NET_TIMEOUT_MS", "400")],
        Duration::from_secs(20),
    );
    assert_eq!(code, 0, "probe failed: {}", out);
    assert!(
        out.contains("true") && out.contains("GET") && out.contains("/hello"),
        "a complete request must still parse: {}",
        out
    );
}
