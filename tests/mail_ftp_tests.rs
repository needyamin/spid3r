//! SMTP, FTP and SSH clients: protocol-level tests.
//!
//! The SMTP and FTP tests run against a protocol-accurate fake server written
//! in Spid3r itself (the same trick `tests/net_tests.rs` uses for TCP): a
//! listener on 127.0.0.1:0, a `spawn`ed task speaking the server side, and the
//! client under test on the other end. That exercises the real wire dialogue -
//! CRLF framing, EHLO capability parsing, dot-stuffing, PASV/EPSV, the 226
//! that closes a transfer - not a mock of our own client.
//!
//! The FTP test moves a payload with the bytes 0x00, 0x80, 0xC8 and 0xFF
//! through `RETR` and `STOR` and compares it byte for byte, which is the only
//! way to catch the UTF-8-lossy `tcp_read` path silently corrupting a binary
//! download.
//!
//! The SSH tests are argv-level on purpose: argument construction is the
//! security boundary of `stdlib/ssh.matrix` (it is the thing that keeps a host
//! string from becoming an option), and it can be verified without a server.

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::modules;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::vm::Vm;
use std::sync::Arc;

fn run_tree(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("mail_ftp_test.matrix"))?;
    semantic::check_file(&program, true, "mail_ftp_test.matrix")?;
    let interp = Interp::new(
        Arc::from("mail_ftp_test.matrix"),
        modules::default_module_dirs(),
    );
    interp.run_program(&program)
}

fn run_vm(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("mail_ftp_test.matrix"))?;
    semantic::check_file(&program, true, "mail_ftp_test.matrix")?;
    let dirs = modules::default_module_dirs();
    let mut vm = Vm::new(Arc::from("mail_ftp_test.matrix"), dirs);
    vm.run_program(&program)
}

/// Run on both engines and require exit 0 on each.
fn run_both(name: &str, src: &str) {
    match run_tree(src) {
        Ok(0) => {}
        Ok(code) => panic!("[{}] tree engine exited {}", name, code),
        Err(e) => panic!("[{}] tree engine failed: {:?}", name, e),
    }
    match run_vm(src) {
        Ok(0) => {}
        Ok(code) => panic!("[{}] vm engine exited {}", name, code),
        Err(e) => panic!("[{}] vm engine failed: {:?}", name, e),
    }
}

// ---------------------------------------------------------------- SMTP

/// A fake ESMTP server: greeting, EHLO capabilities, AUTH PLAIN with a real
/// credential check, MAIL/RCPT/DATA, and QUIT. It reports the number of
/// accepted recipients and the raw (still dot-stuffed) DATA payload back
/// through a channel so the test can assert on what actually crossed the wire.
const SMTP_SERVER: &str = r#"
fn crlf() { return chr(13) + chr(10); }

fn serve_smtp(listener, ch, starttls) {
    let conn = tcp_accept(listener).unwrap();
    tcp_write(conn, "220 fake.local ESMTP" + crlf());
    let buf = "";
    let rcpts = 0;
    while true {
        let idx = buf.index_of(crlf());
        if idx < 0 {
            let r = tcp_read(conn, 4096);
            if r.is_err() { return 0; }
            let chunk = r.unwrap();
            if chunk == "" { return 0; }
            buf = buf + chunk;
            continue;
        }
        let line = buf.slice(0, idx);
        buf = buf.slice(idx + 2, len(buf));
        if line.starts_with("EHLO") {
            let caps = "250-fake.local" + crlf() + "250-SIZE 35882577" + crlf() + "250-8BITMIME" + crlf();
            if starttls {
                caps = caps + "250-STARTTLS" + crlf() + "250 AUTH PLAIN LOGIN" + crlf();
            } else {
                caps = caps + "250 AUTH PLAIN LOGIN" + crlf();
            }
            tcp_write(conn, caps);
        } elif line.starts_with("AUTH PLAIN ") {
            let token = line.slice(11, len(line));
            if token == base64_encode(chr(0) + "user" + chr(0) + "pass") {
                tcp_write(conn, "235 2.7.0 accepted" + crlf());
            } else {
                tcp_write(conn, "535 5.7.8 bad credentials" + crlf());
            }
        } elif line.starts_with("MAIL FROM:") {
            tcp_write(conn, "250 2.1.0 ok" + crlf());
        } elif line.starts_with("RCPT TO:") {
            rcpts = rcpts + 1;
            tcp_write(conn, "250 2.1.5 ok" + crlf());
        } elif line == "DATA" {
            tcp_write(conn, "354 end with <CRLF>.<CRLF>" + crlf());
            let text = buf;
            buf = "";
            let end = text.index_of(crlf() + "." + crlf());
            while end < 0 {
                let r = tcp_read(conn, 4096);
                if r.is_err() { return 0; }
                let chunk = r.unwrap();
                if chunk == "" { return 0; }
                text = text + chunk;
                end = text.index_of(crlf() + "." + crlf());
            }
            let data = text.slice(0, end + 2);
            buf = text.slice(end + 5, len(text));
            ch.send(map("rcpts", rcpts, "data", data));
            tcp_write(conn, "250 2.0.0 queued as 1" + crlf());
        } elif line == "QUIT" {
            tcp_write(conn, "221 2.0.0 bye" + crlf());
            tcp_close(conn);
            return 1;
        } elif line == "RSET" or line == "NOOP" {
            tcp_write(conn, "250 2.0.0 ok" + crlf());
        } else {
            tcp_write(conn, "502 5.5.1 not implemented" + crlf());
        }
    }
    return 0;
}
"#;

#[test]
fn smtp_full_transaction() {
    let src = format!(
        r#"import "smtp";
{server}

fn main() {{
    let listener = tcp_listen("127.0.0.1:0").unwrap();
    let port = int(tcp_local_addr(listener).unwrap().split(":")[1]);
    let ch = channel();
    let srv = spawn fn() {{ return serve_smtp(listener, ch, false); }};

    let c = smtp_connect("127.0.0.1", port, map("allow_plaintext_auth", true));
    assert(c.is_ok());
    let client = c.unwrap();
    assert_eq(client.supports("SIZE"), true);
    assert_eq(client.supports("AUTH"), true);
    assert_eq(client.supports("8BITMIME"), true);
    assert_eq(client.supports("STARTTLS"), false);
    assert_eq(client.auth_plain("user", "pass"), true);

    let msg = smtp_message(map(
        "from", "Build Bot <build@example.com>",
        "to", ["ops@example.com"],
        "bcc", ["audit@example.com"],
        "subject", "Deploy 42",
        "text", "alpha\n.dot leading\nomega\n"
    ));
    let r = client.send(msg);
    assert(r.is_ok());
    let receipt = r.unwrap();
    assert_eq(receipt["code"], 250);
    assert_eq(receipt["recipients"], 2);
    client.quit();

    let got = ch.recv();
    assert_eq(got["rcpts"], 2);
    let data = got["data"];
    assert(data.starts_with("From: Build Bot <build@example.com>" + chr(13) + chr(10)));
    assert(data.contains("Subject: Deploy 42"));
    // Dot-stuffing is what the client must do on the wire: the body line
    // ".dot leading" arrives as "..dot leading".
    assert(data.contains(chr(13) + chr(10) + "..dot leading" + chr(13) + chr(10)));
    // Bcc recipients are in the envelope, never in a header.
    assert(not data.contains("Bcc:"));
    // Every newline is CRLF: strip them all and no bare CR/LF may remain.
    let stripped = data.replace(chr(13) + chr(10), "");
    assert(not stripped.contains(chr(10)));
    assert(not stripped.contains(chr(13)));
    tcp_close(listener);

    // The one-shot entry point: connect, send, quit, no handle to manage.
    let l2 = tcp_listen("127.0.0.1:0").unwrap();
    let p2 = int(tcp_local_addr(l2).unwrap().split(":")[1]);
    let ch2 = channel();
    let srv2 = spawn fn() {{ return serve_smtp(l2, ch2, false); }};
    let msg2 = smtp_message(map(
        "from", "a@example.com",
        "to", ["b@example.com"],
        "subject", "one shot",
        "text", "hi"
    ));
    let r2 = smtp_send(map("host", "127.0.0.1", "port", p2), msg2);
    assert(r2.is_ok());
    assert_eq(r2.unwrap()["code"], 250);
    assert(ch2.recv()["data"].contains("Subject: one shot"));
    tcp_close(l2);
    return 0;
}}
"#,
        server = SMTP_SERVER
    );
    run_both("smtp_full_transaction", &src);
}

#[test]
fn smtp_refuses_cleartext_credentials() {
    let src = format!(
        r#"import "smtp";
{server}

fn main() {{
    // 1. No opt-in: AUTH must be refused even though the server offers it.
    let listener = tcp_listen("127.0.0.1:0").unwrap();
    let port = int(tcp_local_addr(listener).unwrap().split(":")[1]);
    let ch = channel();
    let srv = spawn fn() {{ return serve_smtp(listener, ch, false); }};
    let client = smtp_connect("127.0.0.1", port, map()).unwrap();
    let caught = "";
    try {{
        client.auth_plain("user", "pass");
    }} catch(e) {{
        caught = string(e);
    }}
    assert(caught.contains("refusing to send credentials"));
    client.close();
    tcp_close(listener);

    // 2. A server that offers STARTTLS gets a different, equally explicit
    //    refusal: the upgrade exists and this build cannot take it.
    let l2 = tcp_listen("127.0.0.1:0").unwrap();
    let p2 = int(tcp_local_addr(l2).unwrap().split(":")[1]);
    let ch2 = channel();
    let srv2 = spawn fn() {{ return serve_smtp(l2, ch2, true); }};
    let c2 = smtp_connect("127.0.0.1", p2, map("allow_plaintext_auth", true)).unwrap();
    assert_eq(c2.supports("STARTTLS"), true);
    let caught2 = "";
    try {{
        c2.auth_plain("user", "pass");
    }} catch(e) {{
        caught2 = string(e);
    }}
    assert(caught2.contains("STARTTLS"));
    c2.close();
    tcp_close(l2);

    // 3. Asking for TLS is a hard error at connect time, never a silent
    //    downgrade. No server needed: the check happens before connecting.
    let r = smtp_connect("127.0.0.1", 1, map("tls", true));
    assert(r.is_err());
    assert(r.unwrap_err().contains("TLS"));
    return 0;
}}
"#,
        server = SMTP_SERVER
    );
    run_both("smtp_refuses_cleartext_credentials", &src);
}

// ---------------------------------------------------------------- FTP

/// A fake FTP server: USER/PASS, FEAT, TYPE, EPSV, RETR, STOR, MLSD, SIZE,
/// PWD, QUIT. Binary payloads are served from and stored to real files with
/// `tcp_send_file` / `tcp_recv_to_file`, so the test never routes binary data
/// through a Spid3r string.
const FTP_SERVER: &str = r#"
fn crlf() { return chr(13) + chr(10); }

fn serve_ftp(listener, src_path, store_path) {
    let conn = tcp_accept(listener).unwrap();
    tcp_write(conn, "220 fake FTP ready" + crlf());
    let buf = "";
    let data_listener = -1;
    while true {
        let idx = buf.index_of(crlf());
        if idx < 0 {
            let r = tcp_read(conn, 4096);
            if r.is_err() { return 0; }
            let chunk = r.unwrap();
            if chunk == "" { return 0; }
            buf = buf + chunk;
            continue;
        }
        let line = buf.slice(0, idx);
        buf = buf.slice(idx + 2, len(buf));
        if line.starts_with("USER ") {
            tcp_write(conn, "331 password required" + crlf());
        } elif line.starts_with("PASS ") {
            tcp_write(conn, "230 logged in" + crlf());
        } elif line == "FEAT" {
            tcp_write(conn, "211-Features:" + crlf() + " EPSV" + crlf() + " MLSD" + crlf() + "211 End" + crlf());
        } elif line.starts_with("TYPE ") {
            tcp_write(conn, "200 type set" + crlf());
        } elif line == "EPSV" {
            data_listener = tcp_listen("127.0.0.1:0").unwrap();
            let dp = int(tcp_local_addr(data_listener).unwrap().split(":")[1]);
            tcp_write(conn, "229 Entering Extended Passive Mode (|||" + string(dp) + "|)" + crlf());
        } elif line.starts_with("RETR ") {
            tcp_write(conn, "150 opening data connection" + crlf());
            let d = tcp_accept(data_listener).unwrap();
            tcp_send_file(d, src_path);
            tcp_close(d);
            tcp_close(data_listener);
            tcp_write(conn, "226 transfer complete" + crlf());
        } elif line.starts_with("STOR ") {
            tcp_write(conn, "150 ready for data" + crlf());
            let d = tcp_accept(data_listener).unwrap();
            tcp_recv_to_file(d, store_path);
            tcp_close(d);
            tcp_close(data_listener);
            tcp_write(conn, "226 stored" + crlf());
        } elif line == "MLSD" {
            tcp_write(conn, "150 listing" + crlf());
            let d = tcp_accept(data_listener).unwrap();
            tcp_write(d, "type=file;size=7;modify=20260101000000; report.bin" + crlf());
            tcp_write(d, "type=dir;modify=20260101000000; archive" + crlf());
            tcp_close(d);
            tcp_close(data_listener);
            tcp_write(conn, "226 done" + crlf());
        } elif line == "SIZE report.bin" {
            tcp_write(conn, "213 7" + crlf());
        } elif line == "PWD" {
            tcp_write(conn, "257 \"/pub\" is the current directory" + crlf());
        } elif line == "QUIT" {
            tcp_write(conn, "221 bye" + crlf());
            tcp_close(conn);
            return 1;
        } else {
            tcp_write(conn, "502 not implemented" + crlf());
        }
    }
    return 0;
}
"#;

#[test]
fn ftp_binary_roundtrip_and_listing() {
    let src = format!(
        r#"import "ftp";
{server}

fn main() {{
    let src_path = "target/ftp_probe_src.bin";
    let dst_path = "target/ftp_probe_dst.bin";
    let up_path = "target/ftp_probe_up.bin";
    // 0x00, 0x80, 0xC8 and 0xFF: every one of them is either invalid UTF-8 or
    // changed by a UTF-8 round trip, so a lossy path cannot pass this test.
    let payload = bytes([0, 1, 2, 65, 128, 200, 255]);
    assert(write_bytes(src_path, payload).is_ok());

    let listener = tcp_listen("127.0.0.1:0").unwrap();
    let port = int(tcp_local_addr(listener).unwrap().split(":")[1]);
    let srv = spawn fn() {{ return serve_ftp(listener, src_path, up_path); }};

    let f = ftp_connect("127.0.0.1", port, map());
    assert(f.is_ok());
    let client = f.unwrap();
    assert_eq(client.supports("EPSV"), true);
    assert_eq(client.supports("MLSD"), true);

    let p = client.pwd();
    assert(p.is_ok());
    assert_eq(p.unwrap(), "/pub");

    let got = client.retr("report.bin", dst_path);
    assert(got.is_ok());
    assert_eq(got.unwrap(), 7);
    assert_eq(base64_encode(read_bytes(src_path).unwrap()), base64_encode(read_bytes(dst_path).unwrap()));

    let sent = client.stor(src_path, "up.bin");
    assert(sent.is_ok());
    assert_eq(sent.unwrap(), 7);
    assert_eq(base64_encode(read_bytes(up_path).unwrap()), base64_encode(payload));

    let entries = client.list(null).unwrap();
    assert_eq(len(entries), 2);
    assert_eq(entries[0]["name"], "report.bin");
    assert_eq(entries[0]["type"], "file");
    assert_eq(entries[0]["size"], 7);
    assert_eq(entries[1]["name"], "archive");
    assert_eq(entries[1]["type"], "dir");

    let sz = client.size("report.bin");
    assert(sz.is_ok());
    assert_eq(sz.unwrap(), 7);

    client.quit();
    tcp_close(listener);

    // ftp_with: connect, run the callback, quit - even the callback form must
    // see a usable client.
    let l2 = tcp_listen("127.0.0.1:0").unwrap();
    let p2 = int(tcp_local_addr(l2).unwrap().split(":")[1]);
    let srv2 = spawn fn() {{ return serve_ftp(l2, src_path, up_path); }};
    let r = ftp_with("127.0.0.1", p2, map(), fn(f) {{ return f.pwd(); }});
    assert(r.is_ok());
    assert_eq(r.unwrap(), "/pub");
    tcp_close(l2);
    return 0;
}}
"#,
        server = FTP_SERVER
    );
    run_both("ftp_binary_roundtrip_and_listing", &src);
}

#[test]
fn ftp_refuses_plaintext_password() {
    let src = format!(
        r#"import "ftp";
{server}

fn main() {{
    let listener = tcp_listen("127.0.0.1:0").unwrap();
    let port = int(tcp_local_addr(listener).unwrap().split(":")[1]);
    let srv = spawn fn() {{ return serve_ftp(listener, "target/nope_a.bin", "target/nope_b.bin"); }};
    let r = ftp_connect("127.0.0.1", port, map("user", "bob", "password", "secret"));
    assert(r.is_err());
    assert(r.unwrap_err().contains("refusing to send credentials"));
    tcp_close(listener);
    return 0;
}}
"#,
        server = FTP_SERVER
    );
    run_both("ftp_refuses_plaintext_password", &src);
}

// ---------------------------------------------------------------- SSH

#[test]
fn ssh_argv_is_hardened() {
    let src = r#"
import "ssh";

fn rejected(host, opts) {
    try {
        ssh_argv(host, "id", opts);
        return false;
    } catch(e) {
        return true;
    }
}

fn main() {
    // The exact argv, so a future edit cannot quietly drop BatchMode or the
    // "--" that ends option parsing.
    let a = ssh_argv("example.com", "uptime", map("user", "deploy", "port", 2222, "connect_timeout", 5));
    assert_eq(
        a.join(" "),
        "-o BatchMode=yes -o StrictHostKeyChecking=yes -o ConnectTimeout=5 -p 2222 -l deploy -- example.com uptime"
    );

    // Host-key verification is on by default and only downgradable by an
    // explicit, validated value.
    let b = ssh_argv("host", null, map());
    assert(b.join(" ").contains("StrictHostKeyChecking=yes"));
    assert(not b.join(" ").contains("ProxyCommand"));

    // Anything that could be parsed as an option, or that smuggles a user in,
    // is rejected before it reaches argv.
    assert(rejected("-oProxyCommand=calc.exe", map()));
    assert(rejected("user@host", map()));
    assert(rejected("host name", map()));
    assert(rejected("host;rm -rf /", map()));
    assert(rejected("host", map("user", "-root")));
    assert(rejected("host", map("strict_host_key", "nope")));
    assert(rejected("host", map("port", 0)));

    // An unknown option key is ignored, never forwarded as `-o key=value`:
    // that is what keeps ProxyCommand (local command execution) out of reach.
    let c = ssh_argv("host", "id", map("proxy_command", "calc.exe", "o", "ProxyCommand=calc.exe"));
    for arg in c {
        assert(not arg.contains("ProxyCommand"));
        assert(not arg.contains("proxy_command"));
    }

    // sftp uses the same hardened prefix plus batch mode on stdin.
    let s = sftp_argv("host", map("user", "deploy"));
    assert(s.join(" ").ends_with("-b - -- host"));
    assert(s.join(" ").contains("BatchMode=yes"));
    return 0;
}
"#;
    run_both("ssh_argv_is_hardened", src);
}
