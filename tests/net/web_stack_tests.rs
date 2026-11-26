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

/// `tcp_accept_timeout` is the primitive that makes shutdown deterministic:
/// a bounded accept returns `null` instead of blocking forever. Without it the
/// server could only be stopped by poking a blocked `accept`, which is
/// best-effort and failed on windows-latest.
#[test]
fn bounded_accept_times_out_then_takes_a_real_connection() {
    let src = r#"import "http";
fn main() {
    let l = tcp_listen("127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(l).unwrap();

    // Nothing pending: returns null after the timeout rather than blocking.
    let first = tcp_accept_timeout(l, 100);
    assert_eq(first, null);

    // A real client: returns a connection handle.
    let c = tcp_connect(addr).unwrap();
    let second = tcp_accept_timeout(l, 2000);
    assert(second.is_ok());
    tcp_close(second.unwrap());
    tcp_close(c);
    tcp_close(l);
    return 0;
}
"#;
    run_both("bounded_accept_times_out_then_takes_a_real_connection", src);
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

// ------------------------------------------------ R5.2 keep-alive edge cases

#[test]
fn keep_alive_idle_eviction_pipelining_off_and_tls_reuse() {
    let p12_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("localhost.p12")
        .to_string_lossy()
        .replace('\\', "/");
    let cert_pem_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("localhost_cert.pem")
        .to_string_lossy()
        .replace('\\', "/");
    let tls_enabled = if cfg!(feature = "tls") {
        "true"
    } else {
        "false"
    };

    let src = format!(
        r#"import "web";
import "http";
{client}
fn main() {{
    let app = web_app();
    let hits = map("n", 0);
    app.get("/ping", fn(req) {{
        hits["n"] = hits["n"] + 1;
        return json_response(200, map("hit", hits["n"]));
    }});
    let l = tcp_listen("127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(l).unwrap();
    spawn fn() {{
        serve_listener(
            l,
            fn(req) {{ return app.handle(req); }},
            map(
                "access_log", false,
                "watch_signals", false,
                "idle_timeout_ms", 150,
                "max_requests_per_conn", 5
            ),
            -1
        );
        return 0;
    }};
    sleep_ms(40);

    // 1. Idle-timeout eviction: first request keeps alive; sleeping past
    //    idle_timeout_ms causes the server to evict/close the idle socket.
    let c1 = new Client(addr);
    c1.send(request("GET", "/ping", map(), null), "");
    let r1 = c1.read();
    assert_eq(r1["status"], 200);
    assert_eq(header_value(r1["head"], "connection"), "keep-alive");
    sleep_ms(300);
    // Socket was closed by server after idle timeout; a fresh connection works.
    c1.close();

    // 2. Pipelining off: sending two back-to-back requests in one write
    //    answers the first with `connection: close` and drops the second.
    let before_hits = hits["n"];
    let c2 = new Client(addr);
    let req_a = request("GET", "/ping", map(), null);
    let req_b = request("GET", "/ping", map(), null);
    c2.send(req_a + req_b, "");
    let r2 = c2.read();
    assert_eq(r2["status"], 200);
    assert_eq(header_value(r2["head"], "connection"), "close");
    sleep_ms(50);
    assert_eq(hits["n"], before_hits + 1);
    c2.close();

    tcp_close(l);

    // 3. TLS keep-alive reuse via serve_listener + http_fetch when TLS is compiled in.
    if {tls_enabled} {{
        let tl = tls_listen("{p12}", "", "127.0.0.1:0").unwrap();
        let taddr = tcp_local_addr(tl).unwrap();
        spawn fn() {{
            serve_listener(
                tl,
                fn(req) {{ return app.handle(req); }},
                map("access_log", false, "watch_signals", false, "tls", true),
                -1
            );
            return 0;
        }};
        sleep_ms(50);
        let url = "https://localhost:" + taddr.split(":")[1] + "/ping";
        let f1 = http_fetch("GET", url, map(), null, map("ca_cert", "{ca_pem}")).unwrap();
        assert_eq(f1["status"], 200);
        let f2 = http_fetch("GET", url, map(), null, map("ca_cert", "{ca_pem}")).unwrap();
        assert_eq(f2["status"], 200);
        tcp_close(tl);
    }}
    return 0;
}}
"#,
        client = HTTP_CLIENT,
        tls_enabled = tls_enabled,
        p12 = p12_path,
        ca_pem = cert_pem_path
    );
    run_both(
        "keep_alive_idle_eviction_pipelining_off_and_tls_reuse",
        &src,
    );
}

// ------------------------------------------------ R5.3 chunked request bodies

#[test]
fn chunked_request_bodies_and_adversarial_framing() {
    let src = format!(
        r#"import "web";
import "http";
{client}
fn main() {{
    let app = web_app();
    app.post("/upload", fn(req) {{
        return json_response(200, map("body", req["body"], "len", len(req["body_bytes"])));
    }});
    let l = tcp_listen("127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(l).unwrap();
    spawn fn() {{
        serve_listener(
            l,
            fn(req) {{ return app.handle(req); }},
            map("access_log", false, "watch_signals", false, "max_body_bytes", 32),
            -1
        );
        return 0;
    }};
    sleep_ms(40);

    // 1. Valid multi-chunk request with chunk extension and trailer.
    let c1 = new Client(addr);
    let raw_ok = "POST /upload HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n"
        + "5;ext=1\r\nhello\r\n"
        + "7\r\n, world\r\n"
        + "0\r\nX-Checksum: abc\r\n\r\n";
    c1.send(raw_ok, "");
    let r1 = c1.read();
    assert_eq(r1["status"], 200);
    assert(r1["body"].contains("\"body\":\"hello, world\""));
    assert(r1["body"].contains("\"len\":12"));
    c1.close();

    // 2. Oversized chunked body (> max_body_bytes = 32) -> 413 Payload Too Large.
    let c2 = new Client(addr);
    let raw_big = "POST /upload HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n"
        + "25\r\n0123456789abcdefghijklmnopqrstuvwxyz!\r\n0\r\n\r\n";
    c2.send(raw_big, "");
    let r2 = c2.read();
    assert_eq(r2["status"], 413);
    c2.close();

    // 3. Negative chunk size -> 400 Bad Request.
    let c3 = new Client(addr);
    c3.send("POST /upload HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n-4\r\ntest\r\n0\r\n\r\n", "");
    let r3 = c3.read();
    assert_eq(r3["status"], 400);
    c3.close();

    // 4. Hex-with-junk chunk size -> 400 Bad Request.
    let c4 = new Client(addr);
    c4.send("POST /upload HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n4zz\r\ntest\r\n0\r\n\r\n", "");
    let r4 = c4.read();
    assert_eq(r4["status"], 400);
    c4.close();

    // 5. Conflicting Transfer-Encoding + Content-Length -> 400 Bad Request.
    let c5 = new Client(addr);
    c5.send("POST /upload HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\nContent-Length: 4\r\n\r\n4\r\ntest\r\n0\r\n\r\n", "");
    let r5 = c5.read();
    assert_eq(r5["status"], 400);
    c5.close();

    tcp_close(l);
    return 0;
}}
"#,
        client = HTTP_CLIENT
    );
    run_both("chunked_request_bodies_and_adversarial_framing", &src);
}

// ------------------------------------------------ R5.4 streaming responses

#[test]
fn streaming_responses_across_sources_and_curl_interop() {
    let src = r#"import "web";
import "http";
fn main() {
    // Prepare an upstream socket pair for the socket-streaming route.
    let up_l = tcp_listen("127.0.0.1:0").unwrap();
    let up_addr = tcp_local_addr(up_l).unwrap();

    let app = web_app();
    app.get("/stream-list", fn(req) {
        return stream_response(
            200,
            map("content-type", "text/plain; charset=utf-8"),
            ["alpha-", "beta-", "gamma"]
        );
    });
    app.get("/stream-fn", fn(req) {
        let state = map("i", 0);
        return stream_response(
            200,
            map("content-type", "text/plain; charset=utf-8"),
            fn() {
                let idx = state["i"];
                if idx >= 3 {
                    return null;
                }
                state["i"] = idx + 1;
                return "part" + string(idx) + ";";
            }
        );
    });
    app.get("/stream-sock", fn(req) {
        let c = tcp_connect(up_addr).unwrap();
        return stream_response(
            200,
            map("content-type", "text/plain; charset=utf-8"),
            c
        );
    });

    spawn fn() {
        let s = tcp_accept(up_l).unwrap();
        tcp_write(s, "from-");
        tcp_write(s, "upstream-socket");
        tcp_close(s);
        return 0;
    };

    let l = tcp_listen("127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(l).unwrap();
    spawn fn() {
        serve_listener(
            l,
            fn(req) { return app.handle(req); },
            map("access_log", false, "watch_signals", false),
            -1
        );
        return 0;
    };
    sleep_ms(40);

    let base = "http://" + addr;
    let r1 = http_fetch("GET", base + "/stream-list").unwrap();
    assert_eq(r1["status"], 200);
    assert_eq(r1["headers"]["transfer-encoding"], "chunked");
    assert_eq(r1["body"], "alpha-beta-gamma");

    let r2 = http_fetch("GET", base + "/stream-fn").unwrap();
    assert_eq(r2["status"], 200);
    assert_eq(r2["headers"]["transfer-encoding"], "chunked");
    assert_eq(r2["body"], "part0;part1;part2;");

    let r3 = http_fetch("GET", base + "/stream-sock").unwrap();
    assert_eq(r3["status"], 200);
    assert_eq(r3["headers"]["transfer-encoding"], "chunked");
    assert_eq(r3["body"], "from-upstream-socket");

    tcp_close(up_l);
    tcp_close(l);
    return 0;
}
"#;
    run_both("streaming_responses_across_sources", src);

    // Real curl interop check against a live Spid3r chunked streaming endpoint.
    let curl_bin = if cfg!(windows) { "curl.exe" } else { "curl" };
    if std::process::Command::new(curl_bin)
        .arg("--version")
        .output()
        .is_ok()
    {
        let (addr_tx, addr_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let server_thread = std::thread::Builder::new()
            .name("curl-interop-server".to_string())
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                let port = listener.local_addr().unwrap().port();
                drop(listener);
                let addr = format!("127.0.0.1:{}", port);
                let _ = addr_tx.send(addr.clone());
                let srv_src = format!(
                    r#"import "http";
fn main() {{
    let srv = new HttpServer();
    srv.logging = false;
    srv.opts["watch_signals"] = false;
    srv.get("/curl-stream", fn(req) {{
        return http_stream(200, map("content-type", "text/plain"), ["curl-", "chunked-", "ok"]);
    }});
    srv.bind("{addr}");
    srv.serve(1);
    srv.close();
    return 0;
}}
"#,
                    addr = addr
                );
                let res = run_vm(&srv_src, "curl_interop_server");
                let _ = done_tx.send(res);
            })
            .unwrap();

        let addr = addr_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        std::thread::sleep(Duration::from_millis(80));
        let out = std::process::Command::new(curl_bin)
            .args([
                "--http1.1",
                "-sS",
                "-D",
                "-",
                &format!("http://{}/curl-stream", addr),
            ])
            .output()
            .expect("failed to execute curl");
        assert!(
            out.status.success(),
            "curl failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.to_ascii_lowercase()
                .contains("transfer-encoding: chunked"),
            "missing chunked header in curl output: {}",
            text
        );
        assert!(
            text.ends_with("curl-chunked-ok"),
            "unexpected curl body: {}",
            text
        );
        let srv_res = done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(srv_res.unwrap(), 0);
        server_thread.join().unwrap();
    }
}

#[test]
fn http_fetch_caller_accept_and_user_agent_do_not_duplicate_defaults() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (hdr_tx, hdr_rx) = mpsc::channel();

    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let mut raw = Vec::new();
            loop {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
                if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let _ = hdr_tx.send(String::from_utf8_lossy(&raw).to_string());
            let body = br#"{"Status":0}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(resp.as_bytes()).unwrap();
            stream.write_all(body).unwrap();
        }
    });

    let src = format!(
        r#"import "http";
fn main() {{
    let res = http_fetch(
        "GET",
        "http://{addr}/dns-query?name=example.com&type=A",
        map("Accept", "application/dns-json", "User-Agent", "custom-ua/1.0"),
        null,
        null
    );
    assert(res.is_ok());
    let r = res.unwrap();
    assert_eq(r["status"], 200);
    return 0;
}}
"#,
        addr = addr
    );
    run_both("http_fetch_override_headers", &src);

    for _ in 0..2 {
        let raw = hdr_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let lower = raw.to_ascii_lowercase();
        assert!(lower.contains("accept: application/dns-json\r\n"), "{raw}");
        assert!(lower.contains("user-agent: custom-ua/1.0\r\n"), "{raw}");
        assert!(!lower.contains("accept: */*"), "{raw}");
        assert!(!lower.contains("user-agent: spid3r"), "{raw}");
    }
    server.join().unwrap();
}
