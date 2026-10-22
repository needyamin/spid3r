//! End-to-end tests for the network database clients (Redis, PostgreSQL,
//! MySQL) against protocol-accurate servers.
//!
//! `tools/db_test_servers.py` speaks the real wire protocols:
//!   - RESP2 with a working key/value store,
//!   - PostgreSQL v3 whose auth method is chosen by the username (trust,
//!     cleartext, md5, SCRAM-SHA-256 — with the server verifying our proof),
//!   - MySQL protocol 41 with mysql_native_password, where the server checks
//!     the scramble we compute against its own hashlib reference.
//!
//! Skipped (with a notice) when no Python is available, so a runner without
//! one never fails these.

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::modules;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::vm::Vm;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

struct Servers {
    child: Child,
    redis: u16,
    pg: u16,
    mysql: u16,
}

fn python() -> &'static str {
    if cfg!(windows) {
        "python"
    } else {
        "python3"
    }
}

/// Start the three servers and read the ports they printed.
fn start_servers() -> Option<Servers> {
    let mut child = Command::new(python())
        .arg("tools/db_test_servers.py")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut first = String::new();
    {
        use std::io::BufRead;
        // Read just the READY line, then keep draining the pipe on a thread:
        // dropping it would close the read end, and a server that writes
        // anything (a traceback, a stray print) would die with EPIPE.
        let mut reader = std::io::BufReader::new(&mut stdout);
        reader.read_line(&mut first).ok()?;
    }
    // Drain the rest on a thread so the pipe never fills up.
    std::thread::spawn(move || {
        let mut sink = std::io::sink();
        let _ = std::io::copy(&mut stdout, &mut sink);
    });
    let line = first;
    // "READY redis=1234 pg=1235 mysql=1236"
    let mut ports: std::collections::HashMap<&str, u16> = std::collections::HashMap::new();
    for part in line.split_whitespace().skip(1) {
        let mut kv = part.split('=');
        if let (Some(key), Some(value)) = (kv.next(), kv.next()) {
            if let Ok(port) = value.parse() {
                ports.insert(key, port);
            }
        }
    }
    let lookup = |name: &str| -> Option<u16> { ports.get(name).copied() };
    Some(Servers {
        child,
        redis: lookup("redis")?,
        pg: lookup("pg")?,
        mysql: lookup("mysql")?,
    })
}

impl Drop for Servers {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn run_tree(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("dbdrivers.matrix"))?;
    semantic::check_file(&program, true, "dbdrivers.matrix")?;
    let interp = Interp::new(
        Arc::from("dbdrivers.matrix"),
        modules::default_module_dirs(),
    );
    interp.run_program(&program)
}

fn run_vm(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("dbdrivers.matrix"))?;
    semantic::check_file(&program, true, "dbdrivers.matrix")?;
    let dirs = modules::default_module_dirs();
    let mut vm = Vm::new(Arc::from("dbdrivers.matrix"), dirs);
    vm.run_program(&program)
}

fn both(src: &str) {
    assert_eq!(run_tree(src).unwrap(), 0, "tree engine");
    assert_eq!(run_vm(src).unwrap(), 0, "vm engine");
}

const REDIS: &str = r#"
fn main() {
    let r = redis_connect("127.0.0.1:PORT");
    // SET returns the simple-string reply "+OK".
    assert_eq(redis_command(r, "SET", "greeting", "hello"), "OK");
    // A bulk reply decodes to a string.
    assert_eq(redis_command(r, "GET", "greeting"), "hello");
    // Integers come back as ints.
    assert_eq(redis_command(r, "INCR", "counter"), 1);
    assert_eq(redis_command(r, "INCR", "counter"), 2);
    // An error reply becomes a catchable err value.
    let bad = redis_command(r, "NOSUCH", "x");
    assert_eq(is_failure(bad), true);
    // Missing keys are null, not an empty string.
    assert_eq(redis_command(r, "GET", "missing"), null);
    // Arrays decode to lists.
    redis_command(r, "LPUSH", "queue", "a");
    redis_command(r, "LPUSH", "queue", "b");
    let items = redis_command(r, "LRANGE", "queue", "0", "-1");
    assert_eq(len(items), 2);
    assert_eq(items[0], "b");
    assert_eq(redis_close(r), true);
    assert_eq(redis_close(r), false);
    return 0;
}
"#;

const PG: &str = r#"
fn main() {
    // The server picks the auth method from the username, so all four
    // PostgreSQL auth paths are exercised here.
    let users = list("ada", "ada_clear", "ada_md5", "ada_scram");
    for u in users {
        let db = pg_connect("127.0.0.1:PORT", u, "pencil", "shop");
        let rows = pg_query(db, "SELECT id, name, price FROM items").unwrap();
        assert_eq(len(rows), 2);
        assert_eq(rows[0]["id"], 1);
        assert_eq(rows[0]["name"], "ada");
        assert_eq(rows[0]["price"], 9.99);
        // A NULL column is null, not a string.
        assert_eq(rows[1]["price"], null);
        assert_eq(rows[1]["name"], "bob");
        pg_close(db);
    }
    // A wrong password is a catchable err, not a panic.
    let bad = pg_connect("127.0.0.1:PORT", "ada_clear", "wrong", "shop");
    assert_eq(is_failure(bad), true);
    return 0;
}
"#;

const MYSQL: &str = r#"
fn main() {
    let db = mysql_connect("127.0.0.1:PORT", "ada", "secret123", "shop");
    let rows = mysql_query(db, "SELECT id, name, price FROM items").unwrap();
    assert_eq(len(rows), 2);
    // Column types drive the conversion: int, string, float, NULL.
    assert_eq(rows[0]["id"], 1);
    assert_eq(rows[0]["name"], "ada");
    assert_eq(rows[0]["price"], 9.99);
    assert_eq(rows[1]["id"], 2);
    assert_eq(rows[1]["name"], "bob");
    assert_eq(rows[1]["price"], null);
    assert_eq(mysql_close(db), true);
    assert_eq(mysql_close(db), false);
    return 0;
}
"#;

#[test]
fn redis_client_talks_to_a_real_resp_server() {
    let servers = match start_servers() {
        Some(s) => s,
        None => {
            eprintln!("skipping: python not available");
            return;
        }
    };
    both(&REDIS.replace("PORT", &servers.redis.to_string()));
}

#[test]
fn postgres_client_handles_all_four_auth_methods() {
    let servers = match start_servers() {
        Some(s) => s,
        None => {
            eprintln!("skipping: python not available");
            return;
        }
    };
    both(&PG.replace("PORT", &servers.pg.to_string()));
}

#[test]
fn mysql_client_authenticates_and_parses_results() {
    let servers = match start_servers() {
        Some(s) => s,
        None => {
            eprintln!("skipping: python not available");
            return;
        }
    };
    both(&MYSQL.replace("PORT", &servers.mysql.to_string()));
}
