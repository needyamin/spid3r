//! Redis client over RESP2 (the protocol every Redis version speaks).
//!
//! One command = one array of bulk strings; replies are decoded into Spid3r
//! values (`+str`, `:int`, `$str|null`, `*array`; a `-ERR` reply becomes a
//! catchable `err`). No pipelining, no RESP3 push frames, no cluster
//! redirection — a single connection, command by command, which covers what a
//! cache or queue needs and stays easy to audit.
//!
//! Decoding is generic over `Read`, which is what makes it unit-testable
//! without a socket: the tests feed canned bytes through `Cursor`.

use crate::value::collections::seq::make_list;
use crate::value::{new_ref, NativeCtx, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Handles are process-global, exactly like the `tcp_*` natives.
fn table() -> &'static Mutex<HashMap<i64, Arc<Mutex<TcpStream>>>> {
    use std::sync::OnceLock;
    static TABLE: OnceLock<Mutex<HashMap<i64, Arc<Mutex<TcpStream>>>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_handle() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static NEXT: AtomicI64 = AtomicI64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

const READ_TIMEOUT: Duration = Duration::from_secs(10);

fn get_stream(id: i64, who: &str) -> Result<Arc<Mutex<TcpStream>>, String> {
    table()
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("{}: handle {} is not open", who, id))
}

// ------------------------------------------------------------------ encoding

/// A command is an array of bulk strings.
fn encode_command(args: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("*{}\r\n", args.len()).as_bytes());
    for a in args {
        out.extend_from_slice(format!("${}\r\n", a.len()).as_bytes());
        out.extend_from_slice(a.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out
}

// ------------------------------------------------------------------ decoding

enum Reply {
    Simple(String),
    Error(String),
    Int(i64),
    Bulk(Option<Vec<u8>>),
    Array(Vec<Reply>),
}

fn read_line<R: Read>(stream: &mut R) -> Result<Vec<u8>, String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = stream
            .read(&mut byte)
            .map_err(|e| format!("redis read: {}", e))?;
        if n == 0 {
            return Err("redis: connection closed mid-reply".to_string());
        }
        if byte[0] == b'\n' {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(line);
        }
        line.push(byte[0]);
        if line.len() > 512 * 1024 {
            return Err("redis: reply line exceeds 512 KiB".to_string());
        }
    }
}

fn read_n<R: Read>(stream: &mut R, n: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; n];
    stream
        .read_exact(&mut buf)
        .map_err(|e| format!("redis read: {}", e))?;
    Ok(buf)
}

fn decode_reply<R: Read>(stream: &mut R, depth: usize) -> Result<Reply, String> {
    if depth > 64 {
        return Err("redis: reply nesting deeper than 64".to_string());
    }
    let line = read_line(stream)?;
    if line.is_empty() {
        return Err("redis: empty reply line".to_string());
    }
    let (kind, rest) = (line[0], &line[1..]);
    let text = String::from_utf8_lossy(rest).into_owned();
    Ok(match kind {
        b'+' => Reply::Simple(text),
        b'-' => Reply::Error(text),
        b':' => Reply::Int(
            text.parse::<i64>()
                .map_err(|_| format!("redis: bad integer reply '{}'", text))?,
        ),
        b'$' => {
            let len: i64 = text
                .parse()
                .map_err(|_| format!("redis: bad bulk length '{}'", text))?;
            if len < 0 {
                Reply::Bulk(None)
            } else {
                let mut data = read_n(stream, len as usize)?;
                if &read_n(stream, 2)? != b"\r\n" {
                    return Err("redis: bulk reply is not CRLF-terminated".to_string());
                }
                data.shrink_to_fit();
                Reply::Bulk(Some(data))
            }
        }
        b'*' => {
            let len: i64 = text
                .parse()
                .map_err(|_| format!("redis: bad array length '{}'", text))?;
            if len < 0 {
                Reply::Array(Vec::new())
            } else {
                let mut items = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    items.push(decode_reply(stream, depth + 1)?);
                }
                Reply::Array(items)
            }
        }
        other => return Err(format!("redis: unknown reply type '{}'", other as char)),
    })
}

fn reply_to_value(r: Reply) -> Value {
    match r {
        Reply::Simple(s) => Value::Str(s.into()),
        Reply::Error(e) => Value::Result(Err(Box::new(Value::Str(e.into())))),
        Reply::Int(i) => Value::Int(i),
        Reply::Bulk(None) => Value::Null,
        Reply::Bulk(Some(b)) => match String::from_utf8(b) {
            Ok(s) => Value::Str(s.into()),
            Err(e) => Value::Bytes(new_ref(e.into_bytes())),
        },
        Reply::Array(items) => make_list(items.into_iter().map(reply_to_value).collect()),
    }
}

// ------------------------------------------------------------------- natives

fn want_conn(v: &Value, who: &str) -> Result<i64, String> {
    match v {
        Value::Int(i) => Ok(*i),
        other => Err(format!(
            "{}: handle must be an int, found {}",
            who,
            crate::value::type_name(other)
        )),
    }
}

fn run_command(handle: i64, args: Vec<String>) -> Result<Value, String> {
    let stream = get_stream(handle, "redis_command")?;
    let mut stream = stream.lock().unwrap();
    stream
        .write_all(&encode_command(&args))
        .map_err(|e| format!("redis write: {}", e))?;
    let reply = decode_reply(&mut *stream, 0)?;
    Ok(reply_to_value(reply))
}

/// `redis_connect("127.0.0.1:6379", password = null)` -> `ok(handle)`.
/// A non-empty password sends `AUTH` first.
fn n_redis_connect(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err(format!(
            "redis_connect expects 1 or 2 arguments (addr, password?), got {}",
            args.len()
        ));
    }
    let addr = match &args[0] {
        Value::Str(s) => s.to_string(),
        other => {
            return Err(format!(
                "redis_connect: addr must be a string, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    let stream =
        TcpStream::connect(&addr).map_err(|e| format!("redis_connect '{}': {}", addr, e))?;
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok();
    stream.set_write_timeout(Some(READ_TIMEOUT)).ok();
    stream.set_nodelay(true).ok();
    let handle = next_handle();
    table()
        .lock()
        .unwrap()
        .insert(handle, Arc::new(Mutex::new(stream)));

    if args.len() == 2 {
        if let Value::Str(s) = &args[1] {
            if !s.is_empty() {
                return run_command(handle, vec!["AUTH".to_string(), s.to_string()]);
            }
        }
    }
    Ok(Value::Int(handle))
}

/// `redis_command(handle, arg, ...)` — variadic; every argument is sent as a
/// bulk string. The reply is decoded: `+OK` -> `"OK"`, `:5` -> `5`, a bulk ->
/// string or null, an array -> list, and `-ERR ...` -> a catchable err.
fn n_redis_command(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() < 2 {
        return Err(format!(
            "redis_command expects a handle and at least one argument, got {}",
            args.len()
        ));
    }
    let handle = want_conn(&args[0], "redis_command")?;
    let mut argv = Vec::with_capacity(args.len() - 1);
    for a in &args[1..] {
        match a {
            Value::Str(s) => argv.push(s.to_string()),
            Value::Int(i) => argv.push(i.to_string()),
            Value::Float(f) => argv.push(f.to_string()),
            Value::Bool(b) => argv.push(if *b { "1" } else { "0" }.to_string()),
            Value::Null => argv.push(String::new()),
            other => {
                return Err(format!(
                    "redis_command: arguments must be strings/ints, found {}",
                    crate::value::type_name(other)
                ))
            }
        }
    }
    run_command(handle, argv)
}

/// `redis_close(handle)` -> true if the handle was open.
fn n_redis_close(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!(
            "redis_close expects 1 argument, got {}",
            args.len()
        ));
    }
    let id = want_conn(&args[0], "redis_close")?;
    let removed = table().lock().unwrap().remove(&id);
    if let Some(stream) = &removed {
        // Best effort QUIT; the socket drops regardless.
        let _ = stream.lock().unwrap().write_all(b"*1\r\n$4\r\nQUIT\r\n");
    }
    Ok(Value::Bool(removed.is_some()))
}

pub fn native_defs() -> Vec<crate::stdlib::NativeDef> {
    vec![
        ("redis_connect", n_redis_connect),
        ("redis_command", n_redis_command),
        ("redis_close", n_redis_close),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn command_encoding_is_resp_arrays_of_bulk_strings() {
        assert_eq!(
            encode_command(&["PING".to_string()]),
            b"*1\r\n$4\r\nPING\r\n"
        );
        assert_eq!(
            encode_command(&["SET".to_string(), "k".to_string(), "v x".to_string()]),
            b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$3\r\nv x\r\n"
        );
    }

    #[test]
    fn decode_reply_parses_every_type() {
        let mut r = Cursor::new(b"+OK\r\n".to_vec());
        assert!(matches!(decode_reply(&mut r, 0), Ok(Reply::Simple(s)) if s == "OK"));

        let mut r = Cursor::new(b"-WRONGTYPE Operation\r\n".to_vec());
        assert!(
            matches!(decode_reply(&mut r, 0), Ok(Reply::Error(e)) if e.starts_with("WRONGTYPE"))
        );

        let mut r = Cursor::new(b":1000\r\n".to_vec());
        assert!(matches!(decode_reply(&mut r, 0), Ok(Reply::Int(1000))));

        let mut r = Cursor::new(b"$5\r\nhello\r\n".to_vec());
        assert!(matches!(decode_reply(&mut r, 0), Ok(Reply::Bulk(Some(b))) if b == b"hello"));

        let mut r = Cursor::new(b"$-1\r\n".to_vec());
        assert!(matches!(decode_reply(&mut r, 0), Ok(Reply::Bulk(None))));

        // Nested array with mixed element types.
        let mut r = Cursor::new(b"*3\r\n:1\r\n$3\r\nfoo\r\n*2\r\n+a\r\n+b\r\n".to_vec());
        match decode_reply(&mut r, 0) {
            Ok(Reply::Array(items)) => {
                assert_eq!(items.len(), 3);
                assert!(matches!(items[0], Reply::Int(1)));
                assert!(matches!(items[1], Reply::Bulk(Some(ref b)) if b == b"foo"));
                assert!(matches!(items[2], Reply::Array(ref inner) if inner.len() == 2));
            }
            other => panic!("bad array reply: {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn reply_to_value_maps_errors_to_err_results() {
        let v = reply_to_value(Reply::Error("NOAUTH required".to_string()));
        assert!(matches!(v, Value::Result(Err(_))));
        assert!(matches!(reply_to_value(Reply::Bulk(None)), Value::Null));
        assert!(matches!(reply_to_value(Reply::Int(-3)), Value::Int(-3)));
    }
}
