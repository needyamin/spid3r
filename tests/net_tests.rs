//! Networking & HTTP tests: TCP sockets, HTTP framing, URL helpers,
//! and the pure-Spid3r HttpServer framework on both tree and VM engines.

use std::sync::Arc;
use spid3r::diagnostics::Diagnostic;
use spid3r::interpreter::Interp;
use spid3r::modules;
use spid3r::parser;
use spid3r::semantic;
use spid3r::vm::Vm;

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
