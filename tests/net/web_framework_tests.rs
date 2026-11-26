//! WebApp framework upgrades (2026-10-21): static files, cookies, and
//! signed-cookie sessions — exercised through `app.handle(req)` with
//! synthesized requests (no sockets), on both engines.
//!
//! The response-cookie flow is the full round trip: `/login` sets a
//! signed `spid3r_session` cookie, and the value it set authenticates
//! `/profile` (JWT-verified server-side data, stateless).

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run_src(engine: &str, name: &str, src: &str) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!("spid3r_webfx_{}.matrix", name));
    std::fs::write(&path, src).map_err(|e| format!("write {}: {}", path.display(), e))?;
    let o = Command::new(bin())
        .args(["run", "--engine", engine, path.to_str().unwrap()])
        .output()
        .map_err(|e| format!("spawn failed: {}", e))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).to_string())
    }
}

/// The probe exercises: plain routing, the cookie-setting login response,
/// the session round trip (cookie -> verified data), static file serving
/// (content type + binary body), traversal rejection, 404, and 401.
#[test]
fn cookies_sessions_and_static_files() {
    let src = r#"import "web";

fn main() {
    let app = web_app();
    let store = new SessionStore("s3cret", null);

    app.get("/", fn(req) { return "hello"; });
    app.get("/login", fn(req) {
        let setc = store.start(map("user", "ada", "role", "admin"));
        return map("__http_response", true, "status", 200, "headers", map(), "body", "ok", "cookies", map("spid3r_session", setc));
    });
    app.get("/profile", fn(req) {
        let sess = store.read(req);
        if sess == null { return json_response(401, map("error", "no session")); }
        return json_response(200, map("user", sess["user"]));
    });
    app.static_files(PROBE_PUBLIC_DIR, "/static");

    let r1 = app.handle(map("method", "GET", "path", "/", "raw_query", "", "headers", map()));
    println(r1["body"]);

    let r2 = app.handle(map("method", "GET", "path", "/login", "raw_query", "", "headers", map()));
    let token = "";
    for h in r2["headers"] {
        if h["name"] == "set-cookie" { token = h["value"]; }
    }
    println(token.contains("HttpOnly"));

    let cookie_header = "spid3r_session=" + token.split(";")[0].split("=")[1];
    let r3 = app.handle(map("method", "GET", "path", "/profile", "raw_query", "", "headers", map("cookie", cookie_header)));
    println(r3["body"]);

    let r4 = app.handle(map("method", "GET", "path", "/static/style.css", "raw_query", "", "headers", map()));
    println(r4["headers"]["content-type"]);
    println(r4["body"]);

    let r5 = app.handle(map("method", "GET", "path", "/static/../secrets.txt", "raw_query", "", "headers", map()));
    println(r5["status"]);

    let r6 = app.handle(map("method", "GET", "path", "/static/nope.css", "raw_query", "", "headers", map()));
    println(r6["status"]);

    let r7 = app.handle(map("method", "GET", "path", "/profile", "raw_query", "", "headers", map()));
    println(r7["status"]);
    return 0;
}
"#;
    // The static dir must exist relative to the process CWD (the repo root
    // when tests run), so the test writes it there.
    let pub_dir = PathBuf::from("target/spid3r_webfx_public");
    let _ = std::fs::create_dir_all(&pub_dir);
    std::fs::write(pub_dir.join("style.css"), "body { color: red; }").expect("write css");
    let src = src.replace(
        "PROBE_PUBLIC_DIR",
        &format!("{:?}", pub_dir.display().to_string()),
    );

    for engine in ["tree", "vm"] {
        let out = run_src(engine, "features", &src)
            .unwrap_or_else(|e| panic!("[webfx] {} run failed:\n{}", engine, e));
        assert_eq!(
            out,
            "hello\ntrue\n{\"user\":\"ada\"}\ntext/css; charset=utf-8\n<bytes len=20>\n403\n404\n401\n",
            "[webfx] {} output",
            engine
        );
    }
    let _ = std::fs::remove_dir_all(&pub_dir);
}
