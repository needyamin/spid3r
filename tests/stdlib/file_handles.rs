//! Integration tests for Phase R7.1 (`src/stdlib/file.rs`) opaque integer
//! file-handle natives (`file_open`, `file_read`, `file_read_line`,
//! `file_write`, `file_seek`, `file_tell`, `file_flush`, `file_fsync`,
//! `file_lock`, `file_unlock`, `file_close`) and `security.basedir` policy
//! enforcement.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r_r7_file_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn run_script(dir: &PathBuf, name: &str, src: &str) -> (i32, String, String) {
    let path = dir.join(format!("{}.matrix", name));
    std::fs::write(&path, src).expect("write script");
    let out = Command::new(bin())
        .args(["run", path.to_str().unwrap()])
        .current_dir(dir)
        .output()
        .expect("spawn spid3r");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
    )
}

#[test]
fn file_open_modes_and_read_write_seek_tell() {
    let dir = scratch_dir("modes");
    let src = r#"fn main() {
    // 1. Write mode creates and truncates
    let h1 = file_open("data.txt", "w").unwrap();
    assert_eq(file_write(h1, "hello\r\nworld\nlast"), ok(17));
    assert_eq(file_flush(h1), ok(true));
    assert_eq(file_fsync(h1), ok(true));
    assert_eq(file_close(h1), ok(true));

    // 2. Append mode adds at the end
    let h2 = file_open("data.txt", "a").unwrap();
    assert_eq(file_write(h2, "!"), ok(1));
    assert_eq(file_close(h2), ok(true));

    // 3. Read mode + file_read_line + file_tell + file_seek
    let h3 = file_open("data.txt", "r").unwrap();
    assert_eq(file_read_line(h3), ok("hello"));
    assert_eq(file_tell(h3), ok(7));
    assert_eq(file_read_line(h3), ok("world"));
    assert_eq(file_tell(h3), ok(13));
    assert_eq(file_read_line(h3), ok("last!"));
    assert_eq(file_read_line(h3), ok(null));

    // Seek back to start, cur, end
    assert_eq(file_seek(h3, 0, "start"), ok(0));
    let b = file_read(h3, 5).unwrap();
    assert_eq(len(b), 5);
    assert_eq(file_seek(h3, 2, "cur"), ok(7));
    assert_eq(file_read_line(h3), ok("world"));
    assert_eq(file_seek(h3, -5, "end"), ok(13));
    assert_eq(file_read_line(h3), ok("last!"));

    // Negative seek before start returns err
    let bad_seek = file_seek(h3, -1, "start");
    assert(bad_seek.is_err());
    assert_eq(file_close(h3), ok(true));

    // 4. r+ mode updates in-place
    let h4 = file_open("data.txt", "r+").unwrap();
    assert_eq(file_seek(h4, 0, 0), ok(0));
    assert_eq(file_write(h4, "HELLO"), ok(5));
    assert_eq(file_seek(h4, 0, "start"), ok(0));
    assert_eq(file_read_line(h4), ok("HELLO"));
    assert_eq(file_close(h4), ok(true));

    println("MODES_OK");
    return 0;
}
"#;
    let (code, stdout, stderr) = run_script(&dir, "modes", src);
    assert_eq!(code, 0, "stderr: {}", stderr);
    assert_eq!(stdout.trim(), "MODES_OK");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_lock_unlock_and_double_close_error() {
    let dir = scratch_dir("lock_close");
    let src = r#"fn main() {
    let h = file_open("lockme.bin", "w+").unwrap();
    let b = bytes(4);
    b[0] = 65;
    b[1] = 66;
    b[2] = 67;
    b[3] = 68;
    assert_eq(file_write(h, b), ok(4));
    assert_eq(file_lock(h, true), ok(true));
    assert_eq(file_unlock(h), ok(true));
    assert_eq(file_lock(h, false), ok(true));
    assert_eq(file_unlock(h), ok(true));
    assert_eq(file_close(h), ok(true));

    // Double close returns a clean error Result, never panics or reuses handle
    let c2 = file_close(h);
    assert(c2.is_err());
    println(c2.unwrap_err());

    let r2 = file_read(h, 4);
    assert(r2.is_err());

    // Next handle is strictly greater than h (no handle reuse)
    let h_next = file_open("lockme.bin", "r").unwrap();
    assert(h_next > h);
    assert_eq(file_close(h_next), ok(true));
    return 0;
}
"#;
    let (code, stdout, stderr) = run_script(&dir, "lock_close", src);
    assert_eq!(code, 0, "stderr: {}", stderr);
    assert!(
        stdout.contains("is not open (already closed or invalid)"),
        "unexpected double-close message: {}",
        stdout
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_open_honors_security_basedir() {
    let dir = scratch_dir("basedir");
    let allowed = dir.join("allowed");
    let outside = dir.join("outside");
    std::fs::create_dir_all(&allowed).expect("mkdir allowed");
    std::fs::create_dir_all(&outside).expect("mkdir outside");
    let secret = outside.join("secret.txt");
    std::fs::write(&secret, "top-secret").expect("write secret");
    let inside_file = allowed.join("ok.txt");
    std::fs::write(&inside_file, "inside-ok").expect("write ok");

    let ini = format!(
        "[security]\nbasedir = {}\n",
        allowed.to_str().unwrap().replace('\\', "/")
    );
    let ini_path = dir.join("spid3r.ini");
    std::fs::write(&ini_path, ini).expect("write ini");

    let ok_src = format!(
        "fn main() {{\n    let h = file_open({:?}, \"r\").unwrap();\n    println(file_read_line(h).unwrap());\n    file_close(h);\n    return 0;\n}}\n",
        inside_file.to_str().unwrap()
    );
    let ok_script = allowed.join("ok_prog.matrix");
    std::fs::write(&ok_script, ok_src).expect("write ok_prog");
    let out_ok = Command::new(bin())
        .args(["run", ok_script.to_str().unwrap()])
        .env("SPID3R_INI", ini_path.to_str().unwrap())
        .current_dir(&allowed)
        .output()
        .expect("spawn ok");
    assert_eq!(
        out_ok.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out_ok.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out_ok.stdout).trim(), "inside-ok");

    let deny_src = format!(
        "fn main() {{\n    let h = file_open({:?}, \"r\");\n    println(h);\n    return 0;\n}}\n",
        secret.to_str().unwrap()
    );
    let deny_script = allowed.join("deny_prog.matrix");
    std::fs::write(&deny_script, deny_src).expect("write deny_prog");
    let out_deny = Command::new(bin())
        .args(["run", deny_script.to_str().unwrap()])
        .env("SPID3R_INI", ini_path.to_str().unwrap())
        .current_dir(&allowed)
        .output()
        .expect("spawn deny");
    assert_ne!(out_deny.status.code(), Some(0));
    let err_text = String::from_utf8_lossy(&out_deny.stderr);
    assert!(
        err_text.contains("outside basedir"),
        "expected basedir denial, got: {}",
        err_text
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn http_streaming_accepts_file_open_handle() {
    let dir = scratch_dir("http_stream");
    let payload_path = dir.join("stream_payload.txt");
    std::fs::write(&payload_path, "chunk-one\nchunk-two\n").expect("write payload");
    let spid3r_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = format!(
        r#"import "http";

fn main() {{
    let f = file_open({:?}, "r").unwrap();
    let l = tcp_listen("127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(l).unwrap();
    let srv = spawn fn() {{
        let c = tcp_accept(l).unwrap();
        let _req = http_read_request(c).unwrap();
        let res = http_stream(200, map("content-type", "text/plain"), f);
        let hdrs = map("content-type", "text/plain", "transfer-encoding", "chunked");
        http_write_response(c, 200, hdrs, null, false);
        while true {{
            let r = file_read(res["stream"], 8);
            if r.is_err() {{ break; }}
            let chunk = r.unwrap();
            if len(chunk) == 0 {{ break; }}
            http_write_response_chunk(c, chunk, false);
        }}
        http_write_response_chunk(c, "", true);
        file_close(f);
        tcp_close(c);
        tcp_close(l);
        return 0;
    }};
    let resp = http_fetch("GET", "http://" + addr + "/file").unwrap();
    srv.join();
    println(resp["status"]);
    print(resp["body"]);
    return 0;
}}
"#,
        payload_path.to_str().unwrap()
    );
    let script = dir.join("stream.matrix");
    std::fs::write(&script, src).expect("write stream.matrix");
    let out = Command::new(bin())
        .args(["run", script.to_str().unwrap()])
        .env("SPID3R_PATH", spid3r_root.join("stdlib").to_str().unwrap())
        .current_dir(&dir)
        .output()
        .expect("spawn stream");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        "200\nchunk-one\nchunk-two\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
