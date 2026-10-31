//! The web-serving stack: multipart uploads, keep-alive, sessions, the
//! middleware library, graceful shutdown and the FastCGI responder.
//!
//! Everything here talks over a real socket to a server running in the same
//! program (`serve_listener` in a spawned task), or speaks the wire protocol
//! directly - the FastCGI test is a hand-built client, so the framing is
//! checked byte by byte rather than against our own idea of it.

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::modules;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::vm::Vm;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

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

/// How long one case may take on one engine before the harness gives up.
///
/// These cases talk over real sockets, so a blocked `accept` or `recv` can
/// stall forever. Without this bound a single hung case consumes the whole
/// CI step's 30-minute budget and reports only "the action timed out", which
/// says nothing about *which* case hung — it cost exactly that on
/// windows-latest once (2026-11-01, `graceful_shutdown_stops_the_accept_loop`).
/// A named failure in a minute is worth far more than a silent 30.
const CASE_TIMEOUT: Duration = Duration::from_secs(60);

/// One engine entry point, as a value so the loop below can name it.
type Runner = fn(&str, &str) -> Result<i32, Diagnostic>;

fn run_both(name: &str, src: &str) {
    let cases: [(&str, Runner); 2] = [("tree", run_tree), ("vm", run_vm)];
    for (engine, run) in cases {
        let (tx, rx) = mpsc::channel();
        let src = src.to_string();
        let label = name.to_string();
        // A big stack for the same reason the engines use one: parsing and
        // checking recurse per nesting level.
        std::thread::Builder::new()
            .name(format!("{name}-{engine}"))
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                let _ = tx.send(run(&src, &label));
            })
            .expect("failed to spawn the case thread");
        match rx.recv_timeout(CASE_TIMEOUT) {
            Ok(Ok(0)) => {}
            Ok(Ok(code)) => panic!("[{name}/{engine}] exited {code}"),
            Ok(Err(e)) => panic!("[{name}/{engine}] failed: {e:?}"),
            Err(_) => panic!(
                "[{name}/{engine}] did not finish within {:?} — the case is hung. A blocked \
                 accept/recv is the usual cause; check that the server notices request_shutdown().",
                CASE_TIMEOUT
            ),
        }
    }
}

/// A minimal HTTP/1.1 client: sends a request and reads exactly one response
/// using Content-Length framing, so several requests can share a connection.
const HTTP_CLIENT: &str = r#"
fn header_value(head, name) {
    let lower = head.lower();
    let want = name.lower();
    let at = lower.index_of("\r\n" + want + ":");
    if at < 0 {
        if lower.starts_with(want + ":") {
            return head.slice(len(want) + 1, len(head)).trim();
        }
        return null;
    }
    let rest = head.slice(at + 2 + len(want) + 1, len(head));
    let eol = rest.index_of("\r\n");
    if eol < 0 {
        return rest.trim();
    }
    return rest.slice(0, eol).trim();
}

fn status_of(head) {
    let sp = head.index_of(" ");
    if sp < 0 {
        return 0;
    }
    let rest = head.slice(sp + 1, len(head));
    let sp2 = rest.index_of(" ");
    if sp2 < 0 {
        return int(rest);
    }
    return int(rest.slice(0, sp2));
}

class Client {
    let conn;

    fn new(addr) {
        self.conn = tcp_connect(addr).unwrap();
        tcp_set_timeout(self.conn, 5000);
    }

    public fn send(self, head, body) {
        let packet = bytes_concat([head, body]);
        tcp_write_bytes(self.conn, packet);
        return true;
    }

    /// Read exactly one response.
    ///
    /// The head is read a byte at a time and the body with `tcp_read_exact`,
    /// because a gzipped body is not text: `tcp_read` would decode it as UTF-8
    /// and the Content-Length arithmetic would no longer describe bytes.
    public fn read(self) {
        let head = "";
        while true {
            let b = tcp_read_exact(self.conn, 1);
            if b.is_err() {
                return map("head", "", "body", "", "body_bytes", bytes(), "status", 0);
            }
            let chunk = b.unwrap();
            if len(chunk) == 0 {
                return map("head", "", "body", "", "body_bytes", bytes(), "status", 0);
            }
            head = head + chunk.to_string().unwrap();
            if head.ends_with("\r\n\r\n") {
                break;
            }
        }
        let text = head.slice(0, len(head) - 4);
        let cl = header_value(text, "content-length");
        let clen = 0;
        if cl != null {
            clen = int(cl);
        }
        let body = bytes();
        if clen > 0 {
            let r = tcp_read_exact(self.conn, clen);
            if r.is_ok() {
                body = r.unwrap();
            }
        }
        // A gzipped or binary body is not UTF-8; the text view is best-effort
        // and `body_bytes` is what assertions should use for binary content.
        let text_body = "";
        let converted = body.to_string();
        if converted.is_ok() {
            text_body = converted.unwrap();
        }
        return map(
            "head",
            text,
            "body",
            text_body,
            "body_bytes",
            body,
            "status",
            status_of(text)
        );
    }

    public fn close(self) {
        tcp_close(self.conn);
        return true;
    }
}

fn request(method, path, headers, body) {
    let lines = list();
    lines.push(method + " " + path + " HTTP/1.1");
    lines.push("host: test");
    for k in headers.keys() {
        lines.push(k + ": " + string(headers[k]));
    }
    if body != null {
        lines.push("content-length: " + string(len(body)));
    }
    return lines.join("\r\n") + "\r\n\r\n";
}
"#;

// ---------------------------------------------------------------- multipart

#[test]
fn multipart_upload_is_byte_exact() {
    let src = format!(
        r#"import "web";
import "http";
import "multipart";
{client}
fn main() {{
    reset_shutdown();
    let app = web_app();
    app.post("/upload", fn(req) {{
        let f = multipart_form(req);
        if f.is_err() {{
            return json_response(400, map("error", f.unwrap_err()));
        }}
        let form = f.unwrap();
        let file = multipart_file(form, "avatar");
        if file == null {{
            return json_response(400, map("error", "no file"));
        }}
        return json_response(200, map(
            "note", multipart_field(form, "note", "?"),
            "filename", file["filename"],
            "size", file["size"],
            "b64", base64_encode(file["data"])
        ));
    }});
    let l = tcp_listen("127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(l).unwrap();
    spawn fn() {{
        serve_listener(l, fn(req) {{ return app.handle(req); }}, map("access_log", false, "watch_signals", false), -1);
        return 0;
    }};
    sleep_ms(50);

    // 0x00, 0xFE, 0xFF and 0x80 are all invalid UTF-8 or changed by a lossy
    // round trip: if the parser took req["body"] instead of body_bytes, this
    // comparison fails.
    let payload = bytes([0, 1, 254, 255, 128, 10, 13]);
    let boundary = "----probe123";
    let body = multipart_build(boundary, [
        map("name", "note", "text", "hi there"),
        map("name", "avatar", "filename", "pixel.bin",
            "content_type", "application/octet-stream", "data", payload)
    ]).unwrap();
    let c = new Client(addr);
    c.send(
        request("POST", "/upload", map("content-type", "multipart/form-data; boundary=" + boundary), body),
        body
    );
    let res = c.read();
    assert_eq(res["status"], 200);
    assert(res["body"].contains("\"note\":\"hi there\""));
    assert(res["body"].contains("\"size\":7"));
    assert(res["body"].contains("\"filename\":\"pixel.bin\""));
    assert(res["body"].contains(base64_encode(payload)));
    c.close();

    // A body that is not multipart is a 400, not a crash.
    let c2 = new Client(addr);
    let junk = "not multipart at all";
    c2.send(request("POST", "/upload", map("content-type", "text/plain"), junk), junk);
    let res2 = c2.read();
    assert_eq(res2["status"], 400);
    c2.close();
    return 0;
}}
"#,
        client = HTTP_CLIENT
    );
    run_both("multipart_upload_is_byte_exact", &src);
}

// ---------------------------------------------------------------- keep-alive

#[test]
fn keep_alive_serves_several_requests_per_connection() {
    let src = format!(
        r#"import "web";
import "http";
{client}
fn main() {{
    reset_shutdown();
    let app = web_app();
    let hits = map("n", 0);
    app.get("/ping", fn(req) {{
        hits["n"] = hits["n"] + 1;
        return json_response(200, map("hit", hits["n"]));
    }});
    let l = tcp_listen("127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(l).unwrap();
    spawn fn() {{
        serve_listener(l, fn(req) {{ return app.handle(req); }}, map("access_log", false, "watch_signals", false, "max_requests_per_conn", 5), -1);
        return 0;
    }};
    sleep_ms(50);

    let c = new Client(addr);
    let i = 0;
    while i < 3 {{
        c.send(request("GET", "/ping", map(), null), "");
        let res = c.read();
        assert_eq(res["status"], 200);
        assert_eq(header_value(res["head"], "connection"), "keep-alive");
        assert(res["body"].contains("\"hit\":" + string(i + 1)));
        i += 1;
    }}
    c.close();

    // `connection: close` is honoured: the server answers and hangs up.
    let c2 = new Client(addr);
    c2.send(request("GET", "/ping", map("connection", "close"), null), "");
    let res2 = c2.read();
    assert_eq(res2["status"], 200);
    assert_eq(header_value(res2["head"], "connection"), "close");
    c2.close();
    return 0;
}}
"#,
        client = HTTP_CLIENT
    );
    run_both("keep_alive_serves_several_requests_per_connection", &src);
}

// ---------------------------------------------------------------- sessions

#[test]
fn server_side_sessions_rotate_and_revoke() {
    let src = format!(
        r#"import "web";
import "http";
import "session";
{client}
fn main() {{
    reset_shutdown();
    let store = session_store("a-secret-long-enough-to-be-real", map("idle", 3600)).unwrap();
    let app = web_app();
    app.use_next(store.middleware());
    app.post("/login", fn(req) {{
        let s = req["session"];
        s["data"]["user"] = "ada";
        return json_response(200, map("ok", true));
    }});
    app.get("/me", fn(req) {{
        let s = req["session"];
        if not s["data"].has("user") {{
            return json_response(401, map("error", "login first"));
        }}
        return json_response(200, map("user", s["data"]["user"]));
    }});
    app.post("/logout", fn(req) {{
        let h = store.destroy(req);
        let res = json_response(200, map("ok", true));
        return response_set_header(res, "set-cookie", h);
    }});
    let l = tcp_listen("127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(l).unwrap();
    spawn fn() {{
        serve_listener(l, fn(req) {{ return app.handle(req); }}, map("access_log", false, "watch_signals", false), -1);
        return 0;
    }};
    sleep_ms(50);

    // 1. Not logged in.
    let c = new Client(addr);
    c.send(request("GET", "/me", map(), null), "");
    assert_eq(c.read()["status"], 401);

    // 2. Login sets a session cookie.
    c.send(request("POST", "/login", map(), ""), "");
    let login = c.read();
    assert_eq(login["status"], 200);
    let set_cookie = header_value(login["head"], "set-cookie");
    assert(set_cookie != null);
    assert(set_cookie.contains("HttpOnly"));
    assert(set_cookie.contains("SameSite=Lax"));
    let cookie = set_cookie.slice(0, set_cookie.index_of(";"));

    // 3. The cookie authenticates the next request.
    c.send(request("GET", "/me", map("cookie", cookie), null), "");
    let me = c.read();
    assert_eq(me["status"], 200);
    assert(me["body"].contains("\"user\":\"ada\""));

    // 4. A tampered cookie is rejected: flip one character of the signature.
    let dot = cookie.index_of(".");
    let sig = cookie.slice(dot + 1, len(cookie));
    let first = sig.slice(0, 1);
    let swapped = "0";
    if first == "0" {{
        swapped = "1";
    }}
    let forged = cookie.slice(0, dot + 1) + swapped + sig.slice(1, len(sig));
    c.send(request("GET", "/me", map("cookie", forged), null), "");
    assert_eq(c.read()["status"], 401);

    // 5. Logout revokes server-side: the old cookie stops working.
    c.send(request("POST", "/logout", map("cookie", cookie), ""), "");
    let out = c.read();
    assert_eq(out["status"], 200);
    assert(header_value(out["head"], "set-cookie").contains("Max-Age=0"));
    c.send(request("GET", "/me", map("cookie", cookie), null), "");
    assert_eq(c.read()["status"], 401);

    // 6. Session fixation: logging in twice must produce different IDs.
    c.send(request("POST", "/login", map(), ""), "");
    let a = header_value(c.read()["head"], "set-cookie");
    c.send(request("POST", "/login", map(), ""), "");
    let b = header_value(c.read()["head"], "set-cookie");
    assert(a != b);
    c.close();

    // 7. Every visitor gets a row (anonymous ones included), so the store
    //    grows; prune() is the sweeper. A zero idle timeout expires everything
    //    immediately, which is what a periodic prune call would do later.
    assert(store.count() >= 1);
    let sweeper = session_store("another-secret-long-enough", map("idle", 0)).unwrap();
    sweeper.start(map(), map("a", 1));
    sweeper.start(map(), map("b", 2));
    assert_eq(sweeper.count(), 2);
    assert_eq(sweeper.prune(), 2);
    assert_eq(sweeper.count(), 0);
    return 0;
}}
"#,
        client = HTTP_CLIENT
    );
    run_both("server_side_sessions_rotate_and_revoke", &src);
}

// ---------------------------------------------------------------- middleware

#[test]
fn middleware_chain_behaves() {
    let src = format!(
        r#"import "web";
import "http";
import "middleware" as mw;
{client}
fn main() {{
    reset_shutdown();
    let app = web_app();
    // Recover first (wraps everything), then the rest.
    app.use_next(mw.mw_recover(map("log", false)));
    app.use_next(mw.mw_request_id(map()));
    app.use_next(mw.mw_security_headers(map()));
    app.use_next(mw.mw_cors(map("origin", "https://app.example.com")));
    app.use_next(mw.mw_gzip(map("min_bytes", 32)));
    app.use_next(mw.mw_etag(map()));
    app.get("/big", fn(req) {{
        return html_response(200, "x".repeat(500));
    }});
    app.get("/boom", fn(req) {{
        throw "kaboom: secret details";
    }});
    app.get("/json", fn(req) {{
        return json_response(200, map("ok", true));
    }});
    let l = tcp_listen("127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(l).unwrap();
    spawn fn() {{
        serve_listener(l, fn(req) {{ return app.handle(req); }}, map("access_log", false, "watch_signals", false), -1);
        return 0;
    }};
    sleep_ms(50);

    let c = new Client(addr);

    // Security headers + request id on every response.
    c.send(request("GET", "/json", map(), null), "");
    let r1 = c.read();
    assert_eq(r1["status"], 200);
    assert_eq(header_value(r1["head"], "x-content-type-options"), "nosniff");
    assert_eq(header_value(r1["head"], "x-frame-options"), "DENY");
    assert(header_value(r1["head"], "x-request-id") != null);
    assert_eq(header_value(r1["head"], "access-control-allow-origin"), "https://app.example.com");

    // A throwing handler becomes a 500 that does not leak the message.
    c.send(request("GET", "/boom", map(), null), "");
    let r2 = c.read();
    assert_eq(r2["status"], 500);
    assert(not r2["body"].contains("kaboom"));

    // gzip: the client asked, the body is big enough.
    c.send(request("GET", "/big", map("accept-encoding", "gzip, deflate"), null), "");
    let r3 = c.read();
    assert_eq(header_value(r3["head"], "content-encoding"), "gzip");
    assert(header_value(r3["head"], "vary").contains("accept-encoding"));
    // 500 'x' compress to far less than 500 bytes.
    assert(len(r3["body_bytes"]) < 100);

    // The same request without Accept-Encoding is not compressed.
    c.send(request("GET", "/big", map(), null), "");
    let r4 = c.read();
    assert(header_value(r4["head"], "content-encoding") == null);
    assert_eq(len(r4["body"]), 500);

    // ETag + conditional GET -> 304 with no body.
    let tag = header_value(r4["head"], "etag");
    assert(tag != null);
    c.send(request("GET", "/big", map("if-none-match", tag), null), "");
    let r5 = c.read();
    assert_eq(r5["status"], 304);
    assert_eq(len(r5["body"]), 0);

    // CORS preflight is answered without reaching the router.
    c.send(request("OPTIONS", "/json", map("access-control-request-method", "GET"), null), "");
    let r6 = c.read();
    assert_eq(r6["status"], 204);
    assert_eq(header_value(r6["head"], "access-control-allow-methods"), "GET, POST, PUT, PATCH, DELETE, OPTIONS");
    c.close();
    return 0;
}}
"#,
        client = HTTP_CLIENT
    );
    run_both("middleware_chain_behaves", &src);
}

#[test]
fn middleware_rate_limit_and_basic_auth() {
    let src = format!(
        r#"import "web";
import "http";
import "middleware" as mw;
{client}
fn main() {{
    reset_shutdown();
    let app = web_app();
    app.use_next(mw.mw_rate_limit(map("limit", 2, "window", 60)));
    app.get("/limited", fn(req) {{ return json_response(200, map("ok", true)); }});

    let guarded = web_app();
    guarded.use_next(mw.mw_basic_auth("ada", "lovelace"));
    guarded.get("/secret", fn(req) {{ return json_response(200, map("ok", true)); }});

    let l = tcp_listen("127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(l).unwrap();
    spawn fn() {{
        serve_listener(l, fn(req) {{ return app.handle(req); }}, map("access_log", false, "watch_signals", false), -1);
        return 0;
    }};
    let l2 = tcp_listen("127.0.0.1:0").unwrap();
    let addr2 = tcp_local_addr(l2).unwrap();
    spawn fn() {{
        serve_listener(l2, fn(req) {{ return guarded.handle(req); }}, map("access_log", false, "watch_signals", false), -1);
        return 0;
    }};
    sleep_ms(50);

    let c = new Client(addr);
    c.send(request("GET", "/limited", map(), null), "");
    assert_eq(c.read()["status"], 200);
    c.send(request("GET", "/limited", map(), null), "");
    assert_eq(c.read()["status"], 200);
    c.send(request("GET", "/limited", map(), null), "");
    let third = c.read();
    assert_eq(third["status"], 429);
    assert(header_value(third["head"], "retry-after") != null);
    c.close();

    let g = new Client(addr2);
    g.send(request("GET", "/secret", map(), null), "");
    let unauth = g.read();
    assert_eq(unauth["status"], 401);
    assert(header_value(unauth["head"], "www-authenticate").starts_with("Basic"));

    let good = base64_encode("ada:lovelace");
    g.send(request("GET", "/secret", map("authorization", "Basic " + good), null), "");
    assert_eq(g.read()["status"], 200);

    let bad = base64_encode("ada:wrong");
    g.send(request("GET", "/secret", map("authorization", "Basic " + bad), null), "");
    assert_eq(g.read()["status"], 401);
    g.close();
    return 0;
}}
"#,
        client = HTTP_CLIENT
    );
    run_both("middleware_rate_limit_and_basic_auth", &src);
}

// ---------------------------------------------------------------- shutdown

#[test]
fn graceful_shutdown_stops_the_accept_loop() {
    let src = format!(
        r#"import "web";
import "http";
{client}
fn main() {{
    reset_shutdown();
    let app = web_app();
    app.get("/", fn(req) {{ return json_response(200, map("ok", true)); }});
    let l = tcp_listen("127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(l).unwrap();
    let done = channel();
    spawn fn() {{
        // max_connections < 0: serve until shutdown is requested.
        serve_listener(l, fn(req) {{ return app.handle(req); }}, map("access_log", false, "drain_ms", 500), -1);
        done.send("stopped");
        return 0;
    }};
    sleep_ms(50);

    let c = new Client(addr);
    c.send(request("GET", "/", map(), null), "");
    assert_eq(c.read()["status"], 200);
    c.close();

    // What SIGTERM/Ctrl+C does, minus the signal.
    request_shutdown();
    let outcome = done.recv();
    assert_eq(outcome, "stopped");

    // The port is closed now: a new connection is refused.
    let late = tcp_connect(addr);
    assert(late.is_err());
    reset_shutdown();
    return 0;
}}
"#,
        client = HTTP_CLIENT
    );
    run_both("graceful_shutdown_stops_the_accept_loop", &src);
}

// ---------------------------------------------------------------- fastcgi

/// A hand-built FastCGI client: the test asserts on the raw records, so a
/// framing mistake in the responder cannot pass.
const FCGI_CLIENT: &str = r#"
fn rec(kind, id, content) {
    let head = bytes([
        1, kind, (id / 256) | 0, id % 256,
        (len(content) / 256) | 0, len(content) % 256, 0, 0
    ]);
    return bytes_concat([head, content]);
}

fn pair(name, value) {
    let n = len(name);
    let v = len(value);
    return bytes_concat([bytes([n, v]), name, value]);
}

fn read_rec(conn) {
    let h = tcp_read_exact(conn, 8);
    if h.is_err() {
        return null;
    }
    let b = h.unwrap();
    if len(b) < 8 {
        return null;
    }
    let clen = b[4] * 256 + b[5];
    let content = bytes();
    if clen > 0 {
        content = tcp_read_exact(conn, clen).unwrap();
    }
    if b[6] > 0 {
        tcp_read_exact(conn, b[6]);
    }
    return map("type", b[1], "id", b[2] * 256 + b[3], "content", content);
}
"#;

#[test]
fn fastcgi_responder_speaks_the_protocol() {
    let src = format!(
        r#"import "web";
import "fastcgi";
{fcgi}
fn main() {{
    reset_shutdown();
    let app = web_app();
    app.post("/echo", fn(req) {{
        return json_response(200, map(
            "method", req["method"],
            "path", req["path"],
            "query", req["raw_query"],
            "body", req["body"],
            "ua", req["headers"]["user-agent"]
        ));
    }});
    let l = tcp_listen("127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(l).unwrap();
    spawn fn() {{
        fastcgi_serve_on(l, fn(req) {{ return app.handle(req); }}, map("log", false, "watch_signals", false));
        return 0;
    }};
    sleep_ms(50);

    let conn = tcp_connect(addr).unwrap();
    tcp_set_timeout(conn, 5000);

    // BEGIN_REQUEST (responder, no keep-alive).
    tcp_write_bytes(conn, rec(1, 1, bytes([0, 1, 0, 0, 0, 0, 0, 0])));
    // PARAMS: one record, then the empty terminator.
    let params = bytes_concat([
        pair("REQUEST_METHOD", "POST"),
        pair("SCRIPT_NAME", "/echo"),
        pair("PATH_INFO", ""),
        pair("QUERY_STRING", "a=1&b=2"),
        pair("SERVER_PROTOCOL", "HTTP/1.1"),
        pair("REMOTE_ADDR", "127.0.0.1"),
        pair("HTTP_USER_AGENT", "fcgi-probe"),
        pair("CONTENT_TYPE", "text/plain"),
        pair("CONTENT_LENGTH", "5")
    ]);
    tcp_write_bytes(conn, rec(4, 1, params));
    tcp_write_bytes(conn, rec(4, 1, bytes()));
    // STDIN: the body, then the terminator.
    tcp_write_bytes(conn, rec(5, 1, bytes_concat(["hello"])));
    tcp_write_bytes(conn, rec(5, 1, bytes()));

    // Read STDOUT until the empty record, then END_REQUEST.
    let out = "";
    while true {{
        let r = read_rec(conn);
        assert(r != null);
        if r["type"] == 6 {{
            if len(r["content"]) == 0 {{
                break;
            }}
            let piece = r["content"].to_string();
            if piece.is_ok() {{
                out = out + piece.unwrap();
            }}
        }}
    }}
    let end = read_rec(conn);
    assert_eq(end["type"], 3);
    assert_eq(len(end["content"]), 8);
    assert_eq(end["content"][4], 0);

    assert(out.contains("Status: 200"));
    assert(out.contains("\"method\":\"POST\""));
    assert(out.contains("\"path\":\"/echo\""));
    assert(out.contains("\"query\":\"a=1&b=2\""));
    assert(out.contains("\"body\":\"hello\""));
    assert(out.contains("\"ua\":\"fcgi-probe\""));
    tcp_close(conn);
    return 0;
}}
"#,
        fcgi = FCGI_CLIENT
    );
    run_both("fastcgi_responder_speaks_the_protocol", &src);
}
