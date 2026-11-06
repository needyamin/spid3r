//! Networking & HTTP tests: TCP sockets, HTTP framing, URL helpers,
//! and the pure-Spid3r HttpServer framework on both tree and VM engines.

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::modules;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::vm::Vm;
use std::sync::Arc;

fn run_tree(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("net_test.matrix"))?;
    semantic::check_file(&program, true, "net_test.matrix")?;
    let interp = Interp::new(Arc::from("net_test.matrix"), modules::default_module_dirs());
    interp.run_program(&program)
}

fn run_vm(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("net_test.matrix"))?;
    semantic::check_file(&program, true, "net_test.matrix")?;
    let dirs = modules::default_module_dirs();
    let mut vm = Vm::new(Arc::from("net_test.matrix"), dirs);
    vm.run_program(&program)
}

#[test]
fn url_encode_and_decode_roundtrip() {
    let code = r#"
    fn main() {
        assert_eq(url_decode("a%20b+c"), "a b c");
        assert_eq(url_encode("hello world & 123"), "hello%20world%20%26%20123");
        assert_eq(url_decode(url_encode("foo/bar?key=val&test=1")), "foo/bar?key=val&test=1");
        return 0;
    }
    "#;
    assert_eq!(run_tree(code).unwrap(), 0);
    assert_eq!(run_vm(code).unwrap(), 0);
}

#[test]
fn tcp_echo_server_and_client() {
    let code = r#"
    fn main() {
        let listener = tcp_listen("127.0.0.1:0").unwrap();
        let addr = tcp_local_addr(listener).unwrap();
        let ch = channel();

        // Server task: accept 1 connection, echo back, close
        let server_task = spawn fn() {
            let conn = tcp_accept(listener).unwrap();
            let msg = tcp_read(conn, 1024).unwrap();
            tcp_write(conn, "ECHO: " + msg);
            tcp_close(conn);
            return 0;
        };

        // Client task: connect, send, read response
        let client_task = spawn fn() {
            let conn = tcp_connect(addr).unwrap();
            tcp_write(conn, "Spid3r Net");
            let resp = tcp_read(conn, 1024).unwrap();
            ch.send(resp);
            tcp_close(conn);
            return 0;
        };

        let result = ch.recv();
        assert_eq(result, "ECHO: Spid3r Net");
        tcp_close(listener);
        return 0;
    }
    "#;
    assert_eq!(run_tree(code).unwrap(), 0);
    assert_eq!(run_vm(code).unwrap(), 0);
}

#[test]
fn http_read_and_write_primitives() {
    let code = r#"
    fn main() {
        let listener = tcp_listen("127.0.0.1:0").unwrap();
        let addr = tcp_local_addr(listener).unwrap();

        // Server accepts raw HTTP request and replies with http_write_response
        spawn fn() {
            let conn = tcp_accept(listener).unwrap();
            let req = http_read_request(conn).unwrap();
            assert_eq(req["method"], "POST");
            assert_eq(req["path"], "/api/test");
            assert_eq(req["query"]["key"], "value");
            assert_eq(req["body"], "payload");
            
            http_write_response(conn, 200, map("x-custom", "ok"), "{\"success\":true}");
            tcp_close(conn);
            return null;
        };

        // Client connects and sends HTTP request
        let conn = tcp_connect(addr).unwrap();
        let raw = "POST /api/test?key=value HTTP/1.1\r\nHost: localhost\r\nContent-Length: 7\r\n\r\npayload";
        tcp_write(conn, raw);

        let resp = tcp_read(conn, 2048).unwrap();
        assert(resp.contains("200 OK"));
        assert(resp.contains("x-custom: ok"));
        assert(resp.contains("{\"success\":true}"));
        tcp_close(conn);
        tcp_close(listener);
        return 0;
    }
    "#;
    assert_eq!(run_tree(code).unwrap(), 0);
    assert_eq!(run_vm(code).unwrap(), 0);
}

#[test]
fn http_server_module_routing_and_fetch() {
    let code = r#"
    import "http";

    fn main() {
        let app = new HttpServer();
        app.logging = false;

        app.get("/ping", fn(req) {
            return http_text(200, "pong");
        });

        app.get("/user/:id", fn(req) {
            return map("id", req["params"]["id"], "role", "admin");
        });

        app.post("/submit", fn(req) {
            let body = http_body_json(req).unwrap();
            return map("received", body["msg"]);
        });

        let real_addr = app.bind("127.0.0.1:0");

        // Serve 3 requests in the background
        let srv = spawn fn() {
            app.serve(3);
            return 0;
        };

        // 1. GET /ping
        let r1 = http_fetch("GET", "http://" + real_addr + "/ping").unwrap();
        assert_eq(r1["status"], 200);
        assert_eq(r1["body"], "pong");

        // 2. GET /user/42 (path param & JSON auto-serialization)
        let r2 = http_fetch("GET", "http://" + real_addr + "/user/42").unwrap();
        assert_eq(r2["status"], 200);
        let user = json_parse(r2["body"]);
        assert_eq(user["id"], "42");
        assert_eq(user["role"], "admin");

        // 3. POST /submit
        let r3 = http_fetch("POST", "http://" + real_addr + "/submit", map("content-type", "application/json"), "{\"msg\":\"hello\"}").unwrap();
        assert_eq(r3["status"], 200);
        let sub = json_parse(r3["body"]);
        assert_eq(sub["received"], "hello");

        app.close();
        return 0;
    }
    "#;
    assert_eq!(run_tree(code).unwrap(), 0);
    assert_eq!(run_vm(code).unwrap(), 0);
}

#[test]
fn websocket_server_frames_are_unmasked_rfc6455() {
    // RFC 6455 §5.1: a server MUST NOT mask the frames it sends, and a
    // compliant client (every browser) closes the connection when it sees
    // a masked server frame. This test speaks raw bytes to a ws_accept()
    // server: the 101 handshake must be followed by an UNMASKED text frame.
    use std::io::{Read, Write};
    use std::net::TcpStream;

    // Pre-pick a port so the raw Rust client knows where to connect; the
    // script then binds the same port (the classic test bind/drop race —
    // acceptable for loopback CI).
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    let code = format!(
        r#"
    fn main() {{
        let listener = tcp_listen("127.0.0.1:{}").unwrap();
        let t = spawn fn() {{
            let conn = tcp_accept(listener).unwrap();
            let up = ws_accept(conn).unwrap();
            let _ = ws_send(up, "hi");
            let _msg = ws_recv(up);
            ws_close(up);
            return 0;
        }};
        let done = channel();
        let w = spawn fn() {{
            sleep_ms(1500);
            done.send("over");
            return 0;
        }};
        let _ = done.recv();
        return 0;
    }}
    "#,
        port
    );

    // Run the script in a background thread while the raw client talks.
    let handle = std::thread::spawn(move || run_tree(&code).unwrap());
    std::thread::sleep(std::time::Duration::from_millis(200));

    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .write_all(
            b"GET / HTTP/1.1\r\nHost: t\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
        )
        .unwrap();

    // Read the 101 response head.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    assert!(
        head.starts_with("HTTP/1.1 101"),
        "handshake must switch protocols, got: {}",
        head
    );

    // First server frame: FIN+text (0x81), and the length byte must NOT
    // carry the 0x80 MASK bit.
    let mut hdr = [0u8; 2];
    stream.read_exact(&mut hdr).unwrap();
    assert_eq!(hdr[0], 0x81, "expected FIN+text frame, got {:#x}", hdr[0]);
    assert_eq!(
        hdr[1] & 0x80,
        0,
        "server frames MUST NOT be masked (RFC 6455 §5.1), got len byte {:#x}",
        hdr[1]
    );
    let len = (hdr[1] & 0x7f) as usize;
    assert_eq!(len, 2, "payload length should be 2 for \"hi\"");
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).unwrap();
    assert_eq!(payload, b"hi");

    // Client→server frames ARE masked: send "ok" with mask [1,2,3,4].
    let mask = [1u8, 2, 3, 4];
    let payload = b"ok";
    let mut frame = vec![0x81u8, 0x80 | payload.len() as u8];
    frame.extend_from_slice(&mask);
    frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    stream.write_all(&frame).unwrap();
    drop(stream);

    handle.join().unwrap();
}

#[test]
fn websocket_binary_frames_roundtrip() {
    // ws_send_bytes sends an opcode-0x2 binary frame; ws_recv delivers it
    // as a bytes value. Server echoes whatever the client sends.
    let code = r#"
    fn main() {
        let listener = tcp_listen("127.0.0.1:0").unwrap();
        let addr = tcp_local_addr(listener).unwrap();
        let ch = channel();
        let t = spawn fn() {
            let conn = tcp_accept(listener).unwrap();
            let up = ws_accept(conn).unwrap();
            let msg = ws_recv(up).unwrap();
            let n = ws_send_bytes(up, msg).unwrap();
            assert(n > 0, "binary write reported bytes");
            let _ = ws_recv(up);
            return 0;
        };
        let c = ws_connect("ws://" + addr).unwrap();
        let payload = bytes([0, 1, 2, 254, 255]);
        let w = ws_send_bytes(c, payload).unwrap();
        assert(w > 0, "client binary write reported bytes");
        let echoed = ws_recv(c).unwrap();
        assert_eq(echoed, bytes([0, 1, 2, 254, 255]));
        ws_close(c);
        return 0;
    }
    "#;
    assert_eq!(run_tree(code).unwrap(), 0);
    assert_eq!(run_vm(code).unwrap(), 0);
}
