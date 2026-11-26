//! Phase-2 `spid3r.ini` [net] keys, end-to-end through the real binary:
//!
//! * `net.timeout_ms` — the HTTP framing deadline (slowloris guard), settable
//!   from the INI and from its generic env alias `SPID3R_NET_TIMEOUT_MS`;
//! * `net.max_body` — oversized request bodies are rejected with 413;
//! * `net.max_headers` — too many request headers are rejected with 431;
//! * `net.keepalive_ms` — the idle window between keep-alive requests; 0
//!   closes after each response;
//! * defaults keep today's documented behavior;
//! * the server loop is engine code, so tree and VM agree byte-for-byte;
//! * the config surfaces (`info`, `config validate`, `config init`) carry
//!   the new keys.
//!
//! Every case spawns `spid3r run` of a server script (so that process's
//! config loader picks up the SPID3R_INI file / env overrides) and speaks
//! raw HTTP over 127.0.0.1 with `std::net::TcpStream`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_spid3r")
}

/// A unique scratch directory per test: the tests run in parallel and must
/// not race on shared files.
fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r_phase2_{}", tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn write_in(dir: &Path, name: &str, src: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, src).expect("write");
    p.to_str().unwrap().to_string()
}

/// Pre-pick a port so the raw client knows where to connect; the script then
/// binds the same port (the classic test bind/drop race — acceptable for
/// loopback CI, and the same convention net_tests.rs uses).
fn pick_port() -> u16 {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    port
}

/// The shared probe server: two routes and `max_conns` accepted connections,
/// then exit. `extra_opts` adds listen options that must beat the ini.
fn server_script(port: u16, max_conns: u32, extra_opts: &str) -> String {
    format!(
        r#"import "http";

fn main() {{
    let app = new HttpServer();
    app.logging = false;
    app.opts = map("access_log", false, "watch_signals", false{extra});
    app.get("/ping", fn(req) {{ return http_text(200, "pong"); }});
    app.post("/echo", fn(req) {{ return http_text(200, "echo:" + req["body"]); }});
    app.bind("127.0.0.1:{port}");
    app.serve({max_conns});
    return 0;
}}
"#,
        port = port,
        max_conns = max_conns,
        extra = extra_opts,
    )
}

/// Spawn `spid3r run` of a server script with the given ini text (None = no
/// SPID3R_INI) and extra environment. Returns the child; the caller probes it
/// over TCP and then reaps it with [`reap`].
fn spawn_server(
    dir: &Path,
    tag: &str,
    engine: Option<&str>,
    ini_text: Option<&str>,
    envs: &[(&str, &str)],
    script: &str,
) -> Child {
    let path = write_in(dir, &format!("server_{}.matrix", tag), script);
    let log = write_in(dir, &format!("server_{}.log", tag), "");
    let ini = ini_text.map(|text| write_in(dir, &format!("{}.ini", tag), text));
    spawn_cmd(engine, &path, ini, envs, &log)
}

fn spawn_cmd(
    engine: Option<&str>,
    path: &str,
    ini: Option<String>,
    envs: &[(&str, &str)],
    log: &str,
) -> Child {
    let mut cmd = Command::new(bin());
    cmd.arg("run").arg(path);
    if let Some(e) = engine {
        cmd.arg("--engine").arg(e);
    }
    if let Some(i) = ini {
        cmd.env("SPID3R_INI", i);
    }
    cmd.env("SPID3R_LOG_FILE", log);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    cmd.spawn().expect("spawn spid3r server")
}

/// Reap the server regardless of assertion outcome.
fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Connect to the freshly spawned server, retrying until it accepts (the
/// process needs a moment to boot the engine and bind).
fn connect_when_ready(port: u16) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => {
                s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                s.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
                return s;
            }
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => panic!("server on port {} never came up: {}", port, e),
        }
    }
}

/// A parsed HTTP response: status line, headers (lower-case names), body.
fn read_response(stream: &mut TcpStream) -> (String, Vec<(String, String)>, String) {
    let mut buf: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => buf.push(byte[0]),
            Err(e) => panic!("read_response: {}", e),
        }
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or("").to_string();
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let len: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    if len > 0 {
        stream.read_exact(&mut body).expect("read body");
    }
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// Assert the server process exited on its own within `limit` seconds.
fn assert_exits(child: &mut Child, limit: Duration, what: &str) {
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => return,
            None if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            None => panic!("server did not exit within {:?}: {}", limit, what),
        }
    }
}

// ------------------------------------------------- net.timeout_ms (slowloris)

/// A client that sends half a request head and then stalls must be cut off by
/// the framing deadline. The deadline comes from the INI `[net] timeout_ms`
/// here; a second run proves the env alias `SPID3R_NET_TIMEOUT_MS` produces
/// the same behavior (same effective deadline via env or ini).
#[test]
fn slowloris_is_bounded_by_ini_timeout_and_env_alias_matches() {
    for (tag, ini, env) in [
        ("ini", Some("[net]\ntimeout_ms = 1000\n"), None),
        ("env", None, Some("1000")),
    ] {
        let dir = scratch_dir(&format!("slowloris_{}", tag));
        let port = pick_port();
        let script = server_script(port, 1, "");
        let mut child = spawn_server(
            &dir,
            tag,
            None,
            ini,
            &match env {
                Some(v) => vec![("SPID3R_NET_TIMEOUT_MS", v)],
                None => vec![],
            },
            &script,
        );

        let mut stream = connect_when_ready(port);
        // Half a request head, then silence: the framing deadline (1 s) must
        // close the connection well before the test's 8 s bound.
        let started = Instant::now();
        stream
            .write_all(b"GET / HTTP/1.1\r\nhost: slowloris\r\n")
            .expect("write partial head");
        let mut saw_eof = false;
        let mut buf = [0u8; 256];
        while started.elapsed() < Duration::from_secs(8) {
            match stream.read(&mut buf) {
                Ok(0) => {
                    saw_eof = true;
                    break;
                }
                Ok(_) => {}
                Err(e) => panic!("[{tag}] unexpected read error: {}", e),
            }
        }
        assert!(
            saw_eof,
            "[{tag}] the server must close the stalled connection at the deadline"
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(500),
            "[{tag}] closed too early ({:?}) — something else cut the connection",
            elapsed
        );
        assert!(
            elapsed < Duration::from_secs(8),
            "[{tag}] the framing deadline did not apply ({:?})",
            elapsed
        );
        assert_exits(
            &mut child,
            Duration::from_secs(10),
            "serve(1) returns after the connection closes",
        );
        reap(&mut child);
    }
}

/// The default deadline is 30 s: a stalled client must NOT be cut off within
/// a few seconds (which would mean some other timeout leaked in), and the
/// request that arrives late but complete still succeeds.
#[test]
fn default_deadline_leaves_a_stalled_client_connected() {
    let dir = scratch_dir("slowloris_default");
    let port = pick_port();
    let script = server_script(port, 1, "");
    let mut child = spawn_server(&dir, "default", None, None, &[], &script);

    let mut stream = connect_when_ready(port);
    stream
        .write_all(b"GET /ping HTTP/1.1\r\nhost: x\r\n")
        .expect("write partial head");
    let started = Instant::now();
    // Wait 1.5 s (well under the 30 s default), then finish the request.
    std::thread::sleep(Duration::from_millis(1500));
    stream.write_all(b"\r\n").expect("finish head");
    let (status, _, body) = read_response(&mut stream);
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "a complete late request must still be served: {}",
        status
    );
    assert!(body.contains("pong"), "body: {}", body);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the default 30 s deadline must not have fired: {:?}",
        started.elapsed()
    );
    reap(&mut child);
}

// --------------------------------------------------------- net.max_body (413)

/// A POST whose Content-Length exceeds `net.max_body` gets a 413 diagnostic,
/// on both engines (the limit check is engine code in http.matrix).
#[test]
fn oversized_body_gets_413_on_both_engines() {
    for engine in [None, Some("vm")] {
        let tag = engine.unwrap_or("tree");
        let dir = scratch_dir(&format!("body413_{}", tag));
        let port = pick_port();
        let script = server_script(port, 1, "");
        let mut child = spawn_server(
            &dir,
            tag,
            engine,
            Some("[net]\nmax_body = 1024\n"),
            &[],
            &script,
        );

        let mut stream = connect_when_ready(port);
        let body = "y".repeat(2000);
        let req = format!(
            "POST /echo HTTP/1.1\r\nhost: x\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(req.as_bytes())
            .expect("write oversized post");
        let (status, headers, body_out) = read_response(&mut stream);
        assert!(
            status.starts_with("HTTP/1.1 413"),
            "[{tag}] expected 413, got: {}",
            status
        );
        assert_eq!(
            header(&headers, "connection"),
            Some("close"),
            "[{tag}] a 413 rejects the connection"
        );
        assert!(
            body_out.contains("payload too large") && body_out.contains("1024"),
            "[{tag}] 413 body should name the error and the limit: {}",
            body_out
        );
        assert_exits(&mut child, Duration::from_secs(10), "serve(1) drains");
        reap(&mut child);
    }
}

/// A body within the limit still goes through, and the listen option
/// `max_body_bytes` beats the ini default.
#[test]
fn body_within_limit_and_listen_option_override() {
    let dir = scratch_dir("body413_ok");
    let port = pick_port();
    // ini says 1024, but the listen option raises it to 4096.
    let script = server_script(port, 2, ", \"max_body_bytes\", 4096");
    let mut child = spawn_server(
        &dir,
        "ok",
        None,
        Some("[net]\nmax_body = 1024\n"),
        &[],
        &script,
    );

    let mut stream = connect_when_ready(port);
    let body = "z".repeat(2000);
    let req = format!(
        "POST /echo HTTP/1.1\r\nhost: x\r\ncontent-length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(req.as_bytes()).expect("write post");
    let (status, _, body_out) = read_response(&mut stream);
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "the listen option beats the ini: {}",
        status
    );
    assert!(
        body_out.contains("echo:zz"),
        "echoed body: {}",
        &body_out[..40]
    );
    reap(&mut child);
}

// ------------------------------------------------------ net.max_headers (431)

/// More request headers than `net.max_headers` gets a 431 diagnostic; the
/// boundary (exactly the limit) still succeeds. Both engines agree.
#[test]
fn too_many_headers_get_431_on_both_engines() {
    for engine in [None, Some("vm")] {
        let tag = engine.unwrap_or("tree");
        let dir = scratch_dir(&format!("hdr431_{}", tag));
        let port = pick_port();
        let script = server_script(port, 2, "");
        let mut child = spawn_server(
            &dir,
            tag,
            engine,
            Some("[net]\nmax_headers = 8\n"),
            &[],
            &script,
        );

        // 10 headers > 8: rejected with 431.
        let mut stream = connect_when_ready(port);
        let extra: String = (0..9).map(|i| format!("x{}: v\r\n", i)).collect();
        let req = format!("GET /ping HTTP/1.1\r\nhost: x\r\n{}\r\n", extra);
        stream
            .write_all(req.as_bytes())
            .expect("write too many headers");
        let (status, headers, body_out) = read_response(&mut stream);
        assert!(
            status.starts_with("HTTP/1.1 431"),
            "[{tag}] expected 431, got: {}",
            status
        );
        assert_eq!(
            header(&headers, "connection"),
            Some("close"),
            "[{tag}] a 431 rejects the connection"
        );
        assert!(
            body_out.contains("request header fields too large") && body_out.contains("8"),
            "[{tag}] 431 body should name the error and the limit: {}",
            body_out
        );
        // Connection 2: exactly 8 headers is fine.
        let mut stream = connect_when_ready(port);
        let extra: String = (0..7).map(|i| format!("x{}: v\r\n", i)).collect();
        let req = format!("GET /ping HTTP/1.1\r\nhost: x\r\n{}\r\n", extra);
        stream
            .write_all(req.as_bytes())
            .expect("write boundary headers");
        let (status, _, body) = read_response(&mut stream);
        assert!(
            status.starts_with("HTTP/1.1 200"),
            "[{tag}] exactly the limit must pass: {}",
            status
        );
        assert!(body.contains("pong"), "[{tag}] body: {}", body);
        assert_exits(&mut child, Duration::from_secs(10), "serve(2) drains");
        reap(&mut child);
    }
}

/// The listen option `max_headers` beats the ini value.
#[test]
fn max_headers_listen_option_beats_ini() {
    let dir = scratch_dir("hdr431_opts");
    let port = pick_port();
    // ini says 8; the listen option raises it to 16.
    let script = server_script(port, 1, ", \"max_headers\", 16");
    let mut child = spawn_server(
        &dir,
        "opts",
        None,
        Some("[net]\nmax_headers = 8\n"),
        &[],
        &script,
    );

    let mut stream = connect_when_ready(port);
    let extra: String = (0..11).map(|i| format!("x{}: v\r\n", i)).collect();
    let req = format!("GET /ping HTTP/1.1\r\nhost: x\r\n{}\r\n", extra);
    stream.write_all(req.as_bytes()).expect("write 12 headers");
    let (status, _, body) = read_response(&mut stream);
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "the listen option beats the ini: {}",
        status
    );
    assert!(body.contains("pong"), "body: {}", body);
    reap(&mut child);
}

// ------------------------------------------------------ net.keepalive_ms

/// Two sequential requests on ONE connection inside the keep-alive window
/// both succeed; the first response advertises keep-alive.
#[test]
fn keepalive_window_serves_two_requests_on_one_connection() {
    let dir = scratch_dir("keepalive_on");
    let port = pick_port();
    let script = server_script(port, 1, "");
    let mut child = spawn_server(
        &dir,
        "on",
        None,
        Some("[net]\nkeepalive_ms = 5000\n"),
        &[],
        &script,
    );

    let mut stream = connect_when_ready(port);
    stream
        .write_all(b"GET /ping HTTP/1.1\r\nhost: x\r\n\r\n")
        .expect("request 1");
    let (status1, headers1, body1) = read_response(&mut stream);
    assert!(status1.starts_with("HTTP/1.1 200"), "{}", status1);
    assert_eq!(
        header(&headers1, "connection"),
        Some("keep-alive"),
        "inside the window the connection stays open"
    );
    assert!(body1.contains("pong"), "body 1: {}", body1);
    stream
        .write_all(b"GET /ping HTTP/1.1\r\nhost: x\r\n\r\n")
        .expect("request 2");
    let (status2, _, body2) = read_response(&mut stream);
    assert!(
        status2.starts_with("HTTP/1.1 200"),
        "the second request on the same connection must succeed: {}",
        status2
    );
    assert!(body2.contains("pong"), "body 2: {}", body2);
    reap(&mut child);
}

/// `net.keepalive_ms = 0` closes after each response: the first response
/// says `connection: close` and the socket reaches EOF instead of serving a
/// second request.
#[test]
fn keepalive_zero_closes_after_each_response() {
    let dir = scratch_dir("keepalive_zero");
    let port = pick_port();
    let script = server_script(port, 1, "");
    let mut child = spawn_server(
        &dir,
        "zero",
        None,
        Some("[net]\nkeepalive_ms = 0\n"),
        &[],
        &script,
    );

    let mut stream = connect_when_ready(port);
    stream
        .write_all(b"GET /ping HTTP/1.1\r\nhost: x\r\n\r\n")
        .expect("request 1");
    let (status1, headers1, _) = read_response(&mut stream);
    assert!(status1.starts_with("HTTP/1.1 200"), "{}", status1);
    assert_eq!(
        header(&headers1, "connection"),
        Some("close"),
        "keepalive 0 means Connection: close semantics"
    );
    // The server must close the socket rather than wait for another request.
    let started = Instant::now();
    let mut buf = [0u8; 64];
    let eof = matches!(stream.read(&mut buf), Ok(0));
    assert!(
        eof && started.elapsed() < Duration::from_secs(5),
        "the connection must close right after the response (elapsed {:?})",
        started.elapsed()
    );
    assert_exits(&mut child, Duration::from_secs(10), "serve(1) drains");
    reap(&mut child);
}

// ------------------------------------------------------------- defaults

/// With no ini and no env, behavior matches today's documented defaults:
/// requests work, keep-alive stays on, 64 headers pass (the default cap),
/// and a multi-KB body is served.
#[test]
fn defaults_keep_documented_behavior() {
    let dir = scratch_dir("defaults");
    let port = pick_port();
    let script = server_script(port, 3, "");
    let mut child = spawn_server(&dir, "defaults", None, None, &[], &script);

    // 1. A plain request works.
    let mut stream = connect_when_ready(port);
    stream
        .write_all(b"GET /ping HTTP/1.1\r\nhost: x\r\n\r\n")
        .expect("plain request");
    let (status, _, body) = read_response(&mut stream);
    assert!(status.starts_with("HTTP/1.1 200"), "{}", status);
    assert!(body.contains("pong"), "body: {}", body);

    // 2. Two requests on one connection (keep-alive default 5000 ms).
    stream
        .write_all(b"GET /ping HTTP/1.1\r\nhost: x\r\n\r\n")
        .expect("request 2");
    let (status, _, _) = read_response(&mut stream);
    assert!(status.starts_with("HTTP/1.1 200"), "{}", status);

    // 3. Exactly 64 headers (the default max_headers) is accepted.
    let mut stream2 = connect_when_ready(port);
    let extra: String = (0..63).map(|i| format!("x{}: v\r\n", i)).collect();
    let req = format!("GET /ping HTTP/1.1\r\nhost: x\r\n{}\r\n", extra);
    stream2.write_all(req.as_bytes()).expect("64 headers");
    let (status, _, _) = read_response(&mut stream2);
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "64 headers (the default cap) must pass: {}",
        status
    );

    // 4. A 4096-byte body is served by the default 8 MiB limit.
    let mut stream3 = connect_when_ready(port);
    let body = "w".repeat(4096);
    let req = format!(
        "POST /echo HTTP/1.1\r\nhost: x\r\ncontent-length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    stream3.write_all(req.as_bytes()).expect("4 KiB post");
    let (status, _, out) = read_response(&mut stream3);
    assert!(status.starts_with("HTTP/1.1 200"), "{}", status);
    assert!(out.contains("echo:ww"), "echoed: {}", &out[..40]);
    reap(&mut child);
}

// -------------------------------------------------- differential contract

/// The server loop is engine code: tree and VM must answer the same bytes.
#[test]
fn tree_and_vm_answer_identically() {
    for (label, ini, probe) in [
        (
            "413",
            "[net]\nmax_body = 1024\n",
            b"POST /echo HTTP/1.1\r\nhost: x\r\ncontent-length: 2000\r\n\r\n" as &[u8],
        ),
        (
            "431",
            "[net]\nmax_headers = 8\n",
            b"GET /ping HTTP/1.1\r\nhost: x\r\nx0: v\r\nx1: v\r\nx2: v\r\nx3: v\r\nx4: v\r\nx5: v\r\nx6: v\r\nx7: v\r\nx8: v\r\n\r\n" as &[u8],
        ),
    ] {
        let mut raw = Vec::new();
        for engine in [None, Some("vm")] {
            let tag = format!("diff_{}_{}", label, engine.unwrap_or("tree"));
            let dir = scratch_dir(&tag);
            let port = pick_port();
            let script = server_script(port, 1, "");
            let mut child = spawn_server(&dir, &tag, engine, Some(ini), &[], &script);
            let mut stream = connect_when_ready(port);
            // The 413 probe declares 2000 bytes; send the body too so the
            // framing read completes before the policy check answers.
            stream.write_all(probe).expect("write probe");
            if label == "413" {
                stream
                    .write_all(&vec![b'y'; 2000])
                    .expect("write probe body");
            }
            let (status, headers, body) = read_response(&mut stream);
            assert!(!body.is_empty(), "[{tag}] empty response");
            let _ = status;
            let _ = headers;
            raw.push((status, headers, body));
            assert_exits(&mut child, Duration::from_secs(10), "serve(1) drains");
            reap(&mut child);
        }
        assert_eq!(raw[0], raw[1], "[{}] tree and vm must answer identically", label);
    }
}

// ------------------------------------------------------ config surfaces

/// `spid3r info` lists the four [net] keys with their defaults.
#[test]
fn info_lists_the_net_keys() {
    let out = Command::new(bin())
        .arg("info")
        .output()
        .expect("spawn spid3r info");
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "info failed: {}", text);
    for (key, default) in [
        ("net.timeout_ms", "30000"),
        ("net.max_body", "8388608"),
        ("net.max_headers", "64"),
        ("net.keepalive_ms", "5000"),
    ] {
        assert!(text.contains(key), "info must list {}: {}", key, text);
        assert!(
            text.contains(default),
            "info must show the default {} of {}: {}",
            default,
            key,
            text
        );
    }
}

/// `config validate` accepts a good [net] section and rejects non-integer
/// and out-of-range values.
#[test]
fn config_validate_checks_the_net_keys() {
    let dir = scratch_dir("validate");

    // Good values: ok.
    let good = write_in(
        &dir,
        "good.ini",
        "[net]\ntimeout_ms = 15000\nmax_body = 8388608\nmax_headers = 64\nkeepalive_ms = 5000\n",
    );
    let out = Command::new(bin())
        .arg("config")
        .arg("validate")
        .env("SPID3R_INI", &good)
        .output()
        .expect("spawn spid3r config validate");
    assert!(out.status.success(), "good ini must validate");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("ok"),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // Bad values: rejected with a note per key.
    let bad = write_in(
        &dir,
        "bad.ini",
        "[net]\ntimeout_ms = banana\nmax_headers = 7\nmax_body = 999999999\nkeepalive_ms = -1\n",
    );
    let out = Command::new(bin())
        .arg("config")
        .arg("validate")
        .env("SPID3R_INI", &bad)
        .output()
        .expect("spawn spid3r config validate");
    assert!(!out.status.success(), "bad ini must not validate");
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    for key in [
        "net.timeout_ms",
        "net.max_headers",
        "net.max_body",
        "net.keepalive_ms",
    ] {
        assert!(err.contains(key), "validate must mention {}: {}", key, err);
    }
}

/// `config init` writes the commented default, which includes [net].
#[test]
fn config_init_template_includes_net_section() {
    let dir = scratch_dir("init");
    let target = dir.join("spid3r.ini");
    let out = Command::new(bin())
        .arg("config")
        .arg("init")
        .arg(target.to_str().unwrap())
        .output()
        .expect("spawn spid3r config init");
    assert!(out.status.success(), "init failed");
    let text = std::fs::read_to_string(&target).expect("read written ini");
    assert!(text.contains("[net]"), "template must have a [net] section");
    for line in [
        "timeout_ms = 30000",
        "max_body = 8388608",
        "max_headers = 64",
        "keepalive_ms = 5000",
    ] {
        assert!(
            text.contains(line),
            "template must contain {:?}: {}",
            line,
            text
        );
    }
}
