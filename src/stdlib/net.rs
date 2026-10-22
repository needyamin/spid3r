//! Networking natives: TCP sockets and HTTP/1.1 framing, built on `std::net`
//! only (the core toolchain stays zero-dependency).
//!
//! Sockets are exposed to Spid3r as integer **handles** into a process-global
//! table, so they work identically on every engine (tree, VM, native AOT)
//! without a new `Value` variant. The table only ever holds `Arc`s: a native
//! clones the socket out and releases the table lock *before* blocking I/O, so
//! one task blocked in `accept`/`read` never stalls another task's socket.
//!
//! Error contract (matches the rest of the stdlib): wrong argument types or
//! arities throw; operating-system / network failures return `err(msg)`.
//!
//! HTTP framing lives here rather than in Spid3r because the language's
//! string literals have no `\r` escape, and HTTP requires CRLF line endings.
//! The routing framework on top is pure Spid3r (`stdlib/http.matrix`).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use super::NativeDef;
use crate::value::collections;
use crate::value::{display_value, type_name, NativeCtx, Value};

/// Largest accepted request/response head (request line + headers).
const MAX_HEAD_BYTES: usize = 64 * 1024;
/// Largest accepted message body.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Largest single `tcp_read`.
const MAX_READ_BYTES: i64 = 16 * 1024 * 1024;
/// Connect timeout for `tcp_connect` / `http_fetch`.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Read/write timeout for `http_fetch`.
const FETCH_IO_TIMEOUT: Duration = Duration::from_secs(30);

pub fn native_defs() -> Vec<NativeDef> {
    vec![
        ("tcp_listen", n_tcp_listen),
        ("tcp_accept", n_tcp_accept),
        ("tcp_connect", n_tcp_connect),
        ("tcp_read", n_tcp_read),
        ("tcp_write", n_tcp_write),
        ("tcp_close", n_tcp_close),
        ("tcp_local_addr", n_tcp_local_addr),
        ("tcp_peer_addr", n_tcp_peer_addr),
        ("tcp_set_timeout", n_tcp_set_timeout),
        ("tcp_poll", n_tcp_poll),
        ("udp_bind", n_udp_bind),
        ("udp_local_addr", n_udp_local_addr),
        ("udp_send_to", n_udp_send_to),
        ("udp_recv_from", n_udp_recv_from),
        ("udp_close", n_udp_close),
        ("http_read_request", n_http_read_request),
        ("http_write_response", n_http_write_response),
        ("http_fetch", n_http_fetch),
        ("url_decode", n_url_decode),
        ("url_encode", n_url_encode),
    ]
}

// ------------------------------------------------------------ handle table

#[derive(Clone)]
enum Sock {
    Listener(Arc<TcpListener>),
    Stream(Arc<Conn>),
    Udp(Arc<UdpSocket>),
}

/// A TCP connection with the state a readiness probe needs.
///
/// The stream lives behind its own mutex: a readiness probe flips the socket
/// to non-blocking, reads what is available, and flips it back. `pending`
/// holds bytes the probe read ahead, so polling never loses data - the next
/// `tcp_read` hands them over first.
struct Conn {
    stream: Mutex<TcpStream>,
    pending: Mutex<Vec<u8>>,
}

impl Conn {
    fn new(stream: TcpStream) -> Conn {
        Conn {
            stream: Mutex::new(stream),
            pending: Mutex::new(Vec::new()),
        }
    }
}

fn table() -> &'static Mutex<HashMap<i64, Sock>> {
    static TABLE: OnceLock<Mutex<HashMap<i64, Sock>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);

fn register(sock: Sock) -> i64 {
    let id = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    table().lock().unwrap().insert(id, sock);
    id
}

fn lookup(id: i64) -> Option<Sock> {
    table().lock().unwrap().get(&id).cloned()
}

fn get_listener(id: i64, who: &str) -> Result<Arc<TcpListener>, String> {
    match lookup(id) {
        Some(Sock::Listener(l)) => Ok(l),
        Some(Sock::Stream(_)) => Err(format!(
            "{}: handle {} is a connection, not a listener",
            who, id
        )),
        Some(Sock::Udp(_)) => Err(format!(
            "{}: handle {} is a UDP socket, not a listener",
            who, id
        )),
        None => Err(format!("{}: handle {} is not open", who, id)),
    }
}

fn get_stream(id: i64, who: &str) -> Result<Arc<Conn>, String> {
    match lookup(id) {
        Some(Sock::Stream(s)) => Ok(s),
        Some(Sock::Listener(_)) => Err(format!(
            "{}: handle {} is a listener, not a connection",
            who, id
        )),
        Some(Sock::Udp(_)) => Err(format!(
            "{}: handle {} is a UDP socket, not a connection",
            who, id
        )),
        None => Err(format!("{}: handle {} is not open", who, id)),
    }
}

fn get_udp(id: i64, who: &str) -> Result<Arc<UdpSocket>, String> {
    match lookup(id) {
        Some(Sock::Udp(s)) => Ok(s),
        Some(Sock::Stream(_)) => Err(format!(
            "{}: handle {} is a TCP connection, not a UDP socket",
            who, id
        )),
        Some(Sock::Listener(_)) => Err(format!(
            "{}: handle {} is a TCP listener, not a UDP socket",
            who, id
        )),
        None => Err(format!("{}: handle {} is not open", who, id)),
    }
}

// ----------------------------------------------------------------- helpers

fn ok(v: Value) -> Result<Value, String> {
    collections::construct("ok", vec![v])
}

fn err(msg: impl Into<String>) -> Result<Value, String> {
    collections::construct("err", vec![Value::Str(msg.into().into())])
}

fn arity(who: &str, args: &[Value], n: usize) -> Result<(), String> {
    if args.len() == n {
        Ok(())
    } else {
        Err(format!(
            "{} expects {} argument{}, got {}",
            who,
            n,
            if n == 1 { "" } else { "s" },
            args.len()
        ))
    }
}

fn want_int(v: &Value, who: &str) -> Result<i64, String> {
    match v {
        Value::Int(i) => Ok(*i),
        other => Err(format!(
            "{} expects an int, found {}",
            who,
            type_name(other)
        )),
    }
}

fn want_str(v: &Value, who: &str) -> Result<String, String> {
    match v {
        Value::Str(s) => Ok(s.to_string()),
        other => Err(format!(
            "{} expects a string, found {}",
            who,
            type_name(other)
        )),
    }
}

fn str_val(s: impl Into<String>) -> Value {
    Value::Str(s.into().into())
}

/// Build a Spid3r `map` from string pairs.
fn make_map(pairs: Vec<(String, Value)>) -> Result<Value, String> {
    let mut flat = Vec::with_capacity(pairs.len() * 2);
    for (k, v) in pairs {
        flat.push(str_val(k));
        flat.push(v);
    }
    collections::construct("map", flat)
}

/// Read `(name, value)` pairs out of a header map (`null` = no headers).
fn header_pairs(v: &Value, who: &str) -> Result<Vec<(String, String)>, String> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::Map(m) => {
            let m = m.lock().unwrap();
            let mut out = Vec::with_capacity(m.entries.len());
            for (k, val) in &m.entries {
                let name = k.display();
                let value = match val {
                    Value::Str(s) => s.to_string(),
                    other => display_value(other),
                };
                // Reject CR/LF so user data can never inject extra headers
                // or split the response.
                if name.contains(['\r', '\n']) || value.contains(['\r', '\n']) {
                    return Err(format!("{}: header '{}' contains a line break", who, name));
                }
                if name.trim().is_empty() {
                    return Err(format!("{}: empty header name", who));
                }
                out.push((name, value));
            }
            Ok(out)
        }
        // A list of `{name, value}` maps: needed when the same header name
        // must appear more than once (multiple Set-Cookie headers).
        Value::List(l) => {
            let items = l.lock().unwrap().items.clone();
            let mut out = Vec::with_capacity(items.len());
            for item in &items {
                let m = match item {
                    Value::Map(m) => m.lock().unwrap(),
                    other => {
                        return Err(format!(
                            "{} expects a list of {{name, value}} maps, found {}",
                            who,
                            crate::value::type_name(other)
                        ))
                    }
                };
                let name = m
                    .entries
                    .iter()
                    .find(|(k, _)| k.display() == "name")
                    .map(|(_, v)| display_value(v))
                    .unwrap_or_default();
                let value = m
                    .entries
                    .iter()
                    .find(|(k, _)| k.display() == "value")
                    .map(|(_, v)| display_value(v))
                    .unwrap_or_default();
                if name.contains(['\r', '\n']) || value.contains(['\r', '\n']) {
                    return Err(format!("{}: header '{}' contains a line break", who, name));
                }
                if name.trim().is_empty() {
                    return Err(format!("{}: empty header name", who));
                }
                out.push((name, value));
            }
            Ok(out)
        }
        other => Err(format!(
            "{} expects a map of headers, found {}",
            who,
            type_name(other)
        )),
    }
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn io_msg(who: &str, e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => {
            format!(
                "{}: timed out (SPID3R_NET_TIMEOUT_MS sets the limit, \
                 tcp_set_timeout overrides it per connection)",
                who
            )
        }
        _ => format!("{}: {}", who, e),
    }
}

/// Default read/write timeout for HTTP framing: `SPID3R_NET_TIMEOUT_MS`
/// (default 30 000; `0` disables).
///
/// Audit A-16: `read_head` had no deadline, so a client that dribbles one byte
/// per minute (slowloris) held a server worker forever. Raw `tcp_*` connections
/// keep whatever timeout the program sets; the default is applied when a
/// connection enters HTTP framing, which is the unbounded wait a hostile peer
/// controls.
fn default_io_timeout() -> Option<Duration> {
    let ms = std::env::var("SPID3R_NET_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(30_000);
    if ms == 0 {
        None
    } else {
        Some(Duration::from_millis(ms))
    }
}

/// Give `stream` the default timeout unless the program already set one.
///
/// A timeout the program chose with `tcp_set_timeout` always wins: silently
/// shortening it would break a deliberately patient connection.
fn ensure_io_timeout(stream: &TcpStream) -> Result<(), String> {
    match stream.read_timeout() {
        Ok(Some(_)) => Ok(()),
        Ok(None) => match default_io_timeout() {
            Some(t) => stream
                .set_read_timeout(Some(t))
                .and_then(|_| stream.set_write_timeout(Some(t)))
                .map_err(|e| io_msg("net", &e)),
            None => Ok(()),
        },
        Err(e) => Err(io_msg("net", &e)),
    }
}

fn connect_with_timeout(addr: &str) -> std::io::Result<TcpStream> {
    let mut last = None;
    for sa in addr.to_socket_addrs()? {
        match TcpStream::connect_timeout(&sa, CONNECT_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "address resolved to nothing")
    }))
}

// -------------------------------------------------------------------- TCP

/// `tcp_listen("127.0.0.1:8080")` -> `ok(listener)` / `err(msg)`.
/// Port `0` picks a free port; read it back with `tcp_local_addr`.
fn n_tcp_listen(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("tcp_listen", &args, 1)?;
    let addr = want_str(&args[0], "tcp_listen")?;
    match TcpListener::bind(&addr) {
        Ok(l) => ok(Value::Int(register(Sock::Listener(Arc::new(l))))),
        Err(e) => err(format!("tcp_listen '{}': {}", addr, e)),
    }
}

/// `tcp_accept(listener)` -> `ok(conn)`. Blocks the calling task's worker
/// until a client connects.
fn n_tcp_accept(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("tcp_accept", &args, 1)?;
    let id = want_int(&args[0], "tcp_accept")?;
    let listener = get_listener(id, "tcp_accept")?;
    match listener.accept() {
        Ok((stream, _)) => {
            // `tcp_close` on the listener wakes a blocked accept with a
            // throwaway connection; drop it and report the closure.
            if lookup(id).is_none() {
                let _ = stream.shutdown(Shutdown::Both);
                return err("tcp_accept: listener closed");
            }
            let _ = stream.set_nodelay(true);
            ok(Value::Int(register(Sock::Stream(Arc::new(Conn::new(
                stream,
            ))))))
        }
        Err(e) => err(io_msg("tcp_accept", &e)),
    }
}

/// `tcp_connect("example.com:80")` -> `ok(conn)` / `err(msg)`.
fn n_tcp_connect(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("tcp_connect", &args, 1)?;
    let addr = want_str(&args[0], "tcp_connect")?;
    match connect_with_timeout(&addr) {
        Ok(s) => {
            let _ = s.set_nodelay(true);
            ok(Value::Int(register(Sock::Stream(Arc::new(Conn::new(s))))))
        }
        Err(e) => err(format!("tcp_connect '{}': {}", addr, e)),
    }
}

/// `tcp_read(conn, max_bytes)` -> `ok(text)`; `ok("")` means the peer closed
/// the connection. Invalid UTF-8 is replaced with U+FFFD.
fn n_tcp_read(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("tcp_read", &args, 2)?;
    let id = want_int(&args[0], "tcp_read")?;
    let max = want_int(&args[1], "tcp_read")?;
    if max <= 0 || max > MAX_READ_BYTES {
        return Err(format!(
            "tcp_read: max_bytes must be between 1 and {}",
            MAX_READ_BYTES
        ));
    }
    let conn = get_stream(id, "tcp_read")?;
    // Bytes a readiness probe read ahead come first.
    let ahead = {
        let mut p = conn.pending.lock().unwrap();
        if p.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *p)
        }
    };
    if !ahead.is_empty() {
        let take = ahead.len().min(max as usize);
        let chunk = ahead[..take].to_vec();
        let rest = ahead[take..].to_vec();
        *conn.pending.lock().unwrap() = rest;
        return ok(str_val(String::from_utf8_lossy(&chunk).into_owned()));
    }
    let mut buf = vec![0u8; max as usize];
    let mut stream = conn.stream.lock().unwrap();
    match stream.read(&mut buf) {
        Ok(n) => ok(str_val(String::from_utf8_lossy(&buf[..n]).into_owned())),
        Err(e) => err(io_msg("tcp_read", &e)),
    }
}

/// `tcp_write(conn, text)` -> `ok(bytes_written)` / `err(msg)`.
fn n_tcp_write(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("tcp_write", &args, 2)?;
    let id = want_int(&args[0], "tcp_write")?;
    let text = want_str(&args[1], "tcp_write")?;
    let conn = get_stream(id, "tcp_write")?;
    let mut stream = conn.stream.lock().unwrap();
    match stream
        .write_all(text.as_bytes())
        .and_then(|_| stream.flush())
    {
        Ok(()) => ok(Value::Int(text.len() as i64)),
        Err(e) => err(io_msg("tcp_write", &e)),
    }
}

/// `tcp_close(handle)` -> `true` if the handle was open. Closing a listener
/// also wakes any task blocked in `tcp_accept` on it.
fn n_tcp_close(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("tcp_close", &args, 1)?;
    let id = want_int(&args[0], "tcp_close")?;
    let removed = table().lock().unwrap().remove(&id);
    match removed {
        Some(Sock::Stream(s)) => {
            let _ = s.stream.lock().unwrap().shutdown(Shutdown::Both);
            Ok(Value::Bool(true))
        }
        Some(Sock::Udp(s)) => {
            // Dropping the last Arc closes the socket; no shutdown needed.
            drop(s);
            Ok(Value::Bool(true))
        }
        Some(Sock::Listener(l)) => {
            // std cannot interrupt a blocking accept(); poke it with a
            // short-lived connection so the acceptor notices the closure.
            if let Ok(mut addr) = l.local_addr() {
                if addr.ip().is_unspecified() {
                    addr = match addr {
                        SocketAddr::V4(a) => SocketAddr::from(([127, 0, 0, 1], a.port())),
                        SocketAddr::V6(a) => {
                            SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, a.port()))
                        }
                    };
                }
                let _ = TcpStream::connect_timeout(&addr, Duration::from_millis(250));
            }
            Ok(Value::Bool(true))
        }
        None => Ok(Value::Bool(false)),
    }
}

/// `tcp_local_addr(handle)` -> `ok("127.0.0.1:54321")`.
fn n_tcp_local_addr(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("tcp_local_addr", &args, 1)?;
    let id = want_int(&args[0], "tcp_local_addr")?;
    let addr = match lookup(id) {
        Some(Sock::Listener(l)) => l.local_addr(),
        Some(Sock::Stream(s)) => s.stream.lock().unwrap().local_addr(),
        Some(Sock::Udp(s)) => s.local_addr(),
        None => return Err(format!("tcp_local_addr: handle {} is not open", id)),
    };
    match addr {
        Ok(a) => ok(str_val(a.to_string())),
        Err(e) => err(io_msg("tcp_local_addr", &e)),
    }
}

/// `tcp_peer_addr(conn)` -> `ok("203.0.113.7:51000")`.
fn n_tcp_peer_addr(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("tcp_peer_addr", &args, 1)?;
    let id = want_int(&args[0], "tcp_peer_addr")?;
    let conn = get_stream(id, "tcp_peer_addr")?;
    let addr = conn.stream.lock().unwrap().peer_addr();
    match addr {
        Ok(a) => ok(str_val(a.to_string())),
        Err(e) => err(io_msg("tcp_peer_addr", &e)),
    }
}

/// `tcp_set_timeout(conn, ms)` — read/write timeout; `0` disables it.
fn n_tcp_set_timeout(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("tcp_set_timeout", &args, 2)?;
    let id = want_int(&args[0], "tcp_set_timeout")?;
    let ms = want_int(&args[1], "tcp_set_timeout")?;
    if ms < 0 {
        return Err("tcp_set_timeout: ms must be >= 0".to_string());
    }
    let stream = get_stream(id, "tcp_set_timeout")?;
    let t = if ms == 0 {
        None
    } else {
        Some(Duration::from_millis(ms as u64))
    };
    let s = stream.stream.lock().unwrap();
    match s.set_read_timeout(t).and_then(|_| s.set_write_timeout(t)) {
        Ok(()) => ok(Value::Null),
        Err(e) => err(io_msg("tcp_set_timeout", &e)),
    }
}

/// `tcp_poll(conn_ids, timeout_ms)` -> `ok({readable: [...], writable: [...]})`
///
/// Waits until at least one of the given connections has data to read, or
/// the timeout expires (0 = do not wait). This is the readiness primitive
/// behind an evented server: one task can watch many connections instead of
/// blocking on each in turn.
///
/// The probe reads ahead into the connection's pending buffer, so polling
/// never loses data - the next `tcp_read` hands the probed bytes over first.
/// `writable` lists the connections that are open for writing; without a
/// platform readiness syscall the runtime reports them optimistically (a
/// connected socket accepts writes until the peer stops reading).
fn n_tcp_poll(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "tcp_poll";
    arity(WHO, &args, 2)?;
    let ids = match collections::iter_values(&args[0]) {
        Ok(items) => {
            let mut out = Vec::with_capacity(items.len());
            for v in &items {
                out.push(want_int(v, WHO)?);
            }
            out
        }
        Err(_) => {
            return Err(format!(
                "{} expects a list of connection ids, found {}",
                WHO,
                type_name(&args[0])
            ))
        }
    };
    let timeout_ms = want_int(&args[1], WHO)?;
    if timeout_ms < 0 {
        return Err(format!("{}: timeout_ms must not be negative", WHO));
    }
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms as u64);
    let mut readable: Vec<i64> = Vec::new();
    loop {
        readable.clear();
        for id in &ids {
            if probe_readable(*id, WHO)? {
                readable.push(*id);
            }
        }
        if !readable.is_empty() || std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let mut writable: Vec<i64> = Vec::new();
    for id in &ids {
        if lookup(*id).is_some() {
            writable.push(*id);
        }
    }
    let readable_values: Vec<Value> = readable.into_iter().map(Value::Int).collect();
    let writable_values: Vec<Value> = writable.into_iter().map(Value::Int).collect();
    let readable_list = collections::construct("list", readable_values)?;
    let writable_list = collections::construct("list", writable_values)?;
    collections::construct(
        "map",
        vec![
            Value::Str("readable".into()),
            readable_list,
            Value::Str("writable".into()),
            writable_list,
        ],
    )
    .and_then(ok)
}

/// Is there data to read on this connection? Reads ahead into the pending
/// buffer so the bytes are not lost.
fn probe_readable(id: i64, who: &str) -> Result<bool, String> {
    let conn = match lookup(id) {
        Some(Sock::Stream(c)) => c,
        Some(Sock::Listener(_)) => {
            return Err(format!("{}: handle {} is a listener", who, id));
        }
        Some(Sock::Udp(_)) => {
            return Err(format!("{}: handle {} is a UDP socket", who, id));
        }
        None => return Err(format!("{}: handle {} is not open", who, id)),
    };
    let mut stream = conn.stream.lock().unwrap();
    let _ = stream.set_nonblocking(true);
    let mut buf = vec![0u8; 64 * 1024];
    let result = match stream.read(&mut buf) {
        Ok(n) if n > 0 => {
            let mut pending = conn.pending.lock().unwrap();
            pending.extend_from_slice(&buf[..n]);
            Ok(true)
        }
        Ok(0) => Ok(true), // the peer closed: readable (EOF)
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(false),
        Err(e) => Err(io_msg(who, &e)),
    };
    // Restore blocking mode for the next ordinary read.
    let _ = stream.set_nonblocking(false);
    result
}

// ------------------------------------------------------------------- UDP

/// `udp_bind("127.0.0.1:0")` -> `ok(handle)`. Port 0 picks an ephemeral port;
/// discover it with `udp_local_addr`. UdpSocket::bind does not block.
fn n_udp_bind(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("udp_bind", &args, 1)?;
    let addr = want_str(&args[0], "udp_bind")?;
    match UdpSocket::bind(&addr) {
        Ok(s) => ok(Value::Int(register(Sock::Udp(Arc::new(s))))),
        Err(e) => err(format!("udp_bind '{}': {}", addr, e)),
    }
}

/// `udp_local_addr(handle)` -> `ok("127.0.0.1:54321")`.
fn n_udp_local_addr(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("udp_local_addr", &args, 1)?;
    let id = want_int(&args[0], "udp_local_addr")?;
    let sock = get_udp(id, "udp_local_addr")?;
    match sock.local_addr() {
        Ok(a) => ok(str_val(a.to_string())),
        Err(e) => err(io_msg("udp_local_addr", &e)),
    }
}

/// `udp_send_to(handle, "127.0.0.1:9000", text)` -> `ok(bytes_written)`.
/// The datagram carries the UTF-8 bytes of `text`.
fn n_udp_send_to(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("udp_send_to", &args, 3)?;
    let id = want_int(&args[0], "udp_send_to")?;
    let addr = want_str(&args[1], "udp_send_to")?;
    let text = want_str(&args[2], "udp_send_to")?;
    let sock = get_udp(id, "udp_send_to")?;
    match sock.send_to(text.as_bytes(), &addr) {
        Ok(n) => ok(Value::Int(n as i64)),
        Err(e) => err(format!("udp_send_to '{}': {}", addr, e)),
    }
}

/// `udp_recv_from(handle, max_bytes)` ->
/// `ok([text, "sender-ip:port"])`. Invalid UTF-8 is replaced with U+FFFD
/// (like `tcp_read`); blocks until a datagram arrives.
fn n_udp_recv_from(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("udp_recv_from", &args, 2)?;
    let id = want_int(&args[0], "udp_recv_from")?;
    let max = want_int(&args[1], "udp_recv_from")?;
    if max <= 0 || max > MAX_READ_BYTES {
        return Err(format!(
            "udp_recv_from: max_bytes must be between 1 and {}",
            MAX_READ_BYTES
        ));
    }
    let sock = get_udp(id, "udp_recv_from")?;
    let mut buf = vec![0u8; max as usize];
    match sock.recv_from(&mut buf) {
        Ok((n, sender)) => {
            let text = String::from_utf8_lossy(&buf[..n]).into_owned();
            ok(collections::construct(
                "list",
                vec![str_val(text), str_val(sender.to_string())],
            )?)
        }
        Err(e) => err(io_msg("udp_recv_from", &e)),
    }
}

/// `udp_close(handle)` -> `true` if the handle was open.
fn n_udp_close(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("udp_close", &args, 1)?;
    let id = want_int(&args[0], "udp_close")?;
    let removed = table().lock().unwrap().remove(&id);
    Ok(Value::Bool(removed.is_some()))
}

// ------------------------------------------------------------ HTTP framing

/// A parsed message head plus any body bytes already read past it.
struct Head {
    start_line: String,
    headers: Vec<(String, String)>,
    leftover: Vec<u8>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Read up to and including the blank line that ends an HTTP head.
/// Generic over the transport: a plain `TcpStream` or a TLS stream (the
/// `tls` feature), which share the `Read` surface the framing needs.
fn read_head<S: Read>(mut stream: S, who: &str) -> Result<Head, String> {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 8192];
    let end = loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Err(format!(
                "{}: header section larger than {} bytes",
                who, MAX_HEAD_BYTES
            ));
        }
        // Never read past the cap: a peer that streams headers forever would
        // otherwise overshoot by a whole chunk on every iteration (A-16).
        let room = (MAX_HEAD_BYTES + 1 - buf.len()).min(chunk.len());
        let n = stream
            .read(&mut chunk[..room])
            .map_err(|e| io_msg(who, &e))?;
        if n == 0 {
            return Err(if buf.is_empty() {
                format!("{}: connection closed before any data", who)
            } else {
                format!("{}: connection closed mid-header", who)
            });
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head_text = String::from_utf8_lossy(&buf[..end]).into_owned();
    let mut lines = head_text.split("\r\n");
    let start_line = lines.next().unwrap_or("").to_string();
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(format!("{}: malformed header line '{}'", who, line));
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_string();
        // Repeated headers fold into one comma-separated value (RFC 9110).
        if let Some(slot) = headers.iter_mut().find(|(k, _)| *k == name) {
            slot.1.push_str(", ");
            slot.1.push_str(&value);
        } else {
            headers.push((name, value));
        }
    }
    Ok(Head {
        start_line,
        headers,
        leftover: buf[end + 4..].to_vec(),
    })
}

fn read_exact_body<S: Read>(
    mut stream: S,
    mut body: Vec<u8>,
    len: usize,
    who: &str,
) -> Result<Vec<u8>, String> {
    if len > MAX_BODY_BYTES {
        return Err(format!(
            "{}: body of {} bytes exceeds the {} byte limit",
            who, len, MAX_BODY_BYTES
        ));
    }
    let mut chunk = [0u8; 8192];
    while body.len() < len {
        let n = stream.read(&mut chunk).map_err(|e| io_msg(who, &e))?;
        if n == 0 {
            return Err(format!(
                "{}: connection closed mid-body ({} of {} bytes)",
                who,
                body.len(),
                len
            ));
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(len);
    Ok(body)
}

fn read_to_eof<S: Read>(mut stream: S, mut body: Vec<u8>, who: &str) -> Result<Vec<u8>, String> {
    let mut chunk = [0u8; 8192];
    loop {
        if body.len() > MAX_BODY_BYTES {
            return Err(format!(
                "{}: body exceeds the {} byte limit",
                who, MAX_BODY_BYTES
            ));
        }
        let n = stream.read(&mut chunk).map_err(|e| io_msg(who, &e))?;
        if n == 0 {
            return Ok(body);
        }
        body.extend_from_slice(&chunk[..n]);
    }
}

/// Decode a `Transfer-Encoding: chunked` body.
fn read_chunked<S: Read>(stream: S, leftover: Vec<u8>, who: &str) -> Result<Vec<u8>, String> {
    let mut reader = ChunkReader {
        stream,
        buf: leftover,
        pos: 0,
        who,
    };
    let mut body = Vec::new();
    loop {
        let size_line = reader.line()?;
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| format!("{}: bad chunk size '{}'", who, size_hex))?;
        if size == 0 {
            // Skip optional trailers up to the final blank line.
            while !reader.line()?.is_empty() {}
            return Ok(body);
        }
        // checked_add: a hostile chunk size near usize::MAX would otherwise
        // wrap the sum past the cap and stream unbounded memory.
        match body.len().checked_add(size) {
            Some(total) if total <= MAX_BODY_BYTES => {}
            _ => {
                return Err(format!(
                    "{}: body exceeds the {} byte limit",
                    who, MAX_BODY_BYTES
                ))
            }
        }
        body.extend_from_slice(&reader.take(size)?);
        reader.line()?; // CRLF after each chunk
    }
}

struct ChunkReader<'a, S: Read> {
    stream: S,
    buf: Vec<u8>,
    pos: usize,
    who: &'a str,
}

impl<S: Read> ChunkReader<'_, S> {
    fn fill(&mut self) -> Result<(), String> {
        let mut chunk = [0u8; 8192];
        let n = self
            .stream
            .read(&mut chunk)
            .map_err(|e| io_msg(self.who, &e))?;
        if n == 0 {
            return Err(format!("{}: connection closed mid-chunk", self.who));
        }
        self.buf.extend_from_slice(&chunk[..n]);
        Ok(())
    }

    /// Drop the consumed prefix once it dominates the buffer.
    ///
    /// A-16: `pos` advanced but `buf` was never compacted, so a long chunked
    /// body kept every byte it had ever read (roughly doubling peak memory for
    /// an 8 MB body, and making every `take` copy from a steadily growing
    /// vector).
    fn compact(&mut self) {
        if self.pos >= 8192 && self.pos * 2 >= self.buf.len() {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
    }

    fn line(&mut self) -> Result<String, String> {
        loop {
            if let Some(i) = find_subslice(&self.buf[self.pos..], b"\r\n") {
                let s = String::from_utf8_lossy(&self.buf[self.pos..self.pos + i]).into_owned();
                self.pos += i + 2;
                self.compact();
                return Ok(s);
            }
            if self.buf.len() - self.pos > MAX_HEAD_BYTES {
                return Err(format!("{}: chunk header too long", self.who));
            }
            self.fill()?;
        }
    }

    fn take(&mut self, n: usize) -> Result<Vec<u8>, String> {
        while self.buf.len() - self.pos < n {
            self.fill()?;
        }
        let out = self.buf[self.pos..self.pos + n].to_vec();
        self.pos += n;
        self.compact();
        Ok(out)
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Percent-decode; `plus_is_space` applies form-encoding rules (`+` = space).
fn percent_decode(s: &str, plus_is_space: bool) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
                _ => out.push(b'%'),
            },
            b'+' if plus_is_space => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Parse `a=1&b=two` into a Spid3r map (later duplicates win).
fn parse_query(q: &str) -> Result<Value, String> {
    let mut pairs = Vec::new();
    for part in q.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        pairs.push((percent_decode(k, true), str_val(percent_decode(v, true))));
    }
    make_map(pairs)
}

fn headers_value(headers: &[(String, String)]) -> Result<Value, String> {
    make_map(
        headers
            .iter()
            .map(|(k, v)| (k.clone(), str_val(v.clone())))
            .collect(),
    )
}

/// `http_read_request(conn)` -> `ok(request)` where request is a map:
/// `method`, `path` (decoded), `query` (map), `raw_query`, `version`,
/// `headers` (map, lower-case names), `body`, `remote`.
fn n_http_read_request(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "http_read_request";
    arity(WHO, &args, 1)?;
    let id = want_int(&args[0], WHO)?;
    let conn = get_stream(id, WHO)?;
    let stream = conn.stream.lock().unwrap();
    match read_request(&stream) {
        Ok(v) => ok(v),
        Err(e) => err(e),
    }
}

fn read_request(stream: &TcpStream) -> Result<Value, String> {
    const WHO: &str = "http_read_request";
    // A-16: bound the header phase unless the program set its own timeout.
    ensure_io_timeout(stream)?;
    let head = read_head(stream, WHO)?;
    let parts: Vec<&str> = head.start_line.split_whitespace().collect();
    if parts.len() != 3 || !parts[2].starts_with("HTTP/") {
        return Err(format!(
            "{}: malformed request line '{}'",
            WHO, head.start_line
        ));
    }
    let method = parts[0].to_ascii_uppercase();
    let target = parts[1];
    let version = parts[2].to_string();

    if head
        .header("transfer-encoding")
        .is_some_and(|te| te.to_ascii_lowercase().contains("chunked"))
    {
        return Err(format!("{}: chunked request bodies are not supported", WHO));
    }
    let len = match head.header("content-length") {
        Some(v) => v
            .parse::<usize>()
            .map_err(|_| format!("{}: bad content-length '{}'", WHO, v))?,
        None => 0,
    };
    let leftover = head.leftover.clone();
    let body = read_exact_body(stream, leftover, len, WHO)?;

    let (raw_path, raw_query) = target.split_once('?').unwrap_or((target, ""));
    let remote = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    make_map(vec![
        ("method".to_string(), str_val(method)),
        ("path".to_string(), str_val(percent_decode(raw_path, false))),
        ("query".to_string(), parse_query(raw_query)?),
        ("raw_query".to_string(), str_val(raw_query)),
        ("version".to_string(), str_val(version)),
        ("headers".to_string(), headers_value(&head.headers)?),
        (
            "body".to_string(),
            str_val(String::from_utf8_lossy(&body).into_owned()),
        ),
        ("remote".to_string(), str_val(remote)),
    ])
}

fn reason_phrase(status: i64) -> &'static str {
    match status {
        100 => "Continue",
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Unknown",
    }
}

/// `http_write_response(conn, status, headers, body)` -> `ok(bytes)`.
/// Always sends `content-length` and `connection: close`; a missing
/// `content-type` defaults to `text/plain; charset=utf-8`.
fn n_http_write_response(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "http_write_response";
    arity(WHO, &args, 4)?;
    let id = want_int(&args[0], WHO)?;
    let status = want_int(&args[1], WHO)?;
    if !(100..=999).contains(&status) {
        return Err(format!("{}: status {} is out of range", WHO, status));
    }
    let headers = header_pairs(&args[2], WHO)?;
    // A `bytes` body is written verbatim (binary responses: images, wasm,
    // any pre-encoded payload); everything else renders as text.
    let (body_text, body_raw): (String, Option<Vec<u8>>) = match &args[3] {
        Value::Str(s) => (s.to_string(), None),
        Value::Null => (String::new(), None),
        Value::Bytes(b) => (String::new(), Some(b.lock().unwrap().clone())),
        other => (display_value(other), None),
    };
    let body_len = body_raw.as_ref().map_or(body_text.len(), |b| b.len());
    let stream = get_stream(id, WHO)?;

    let mut out = format!("HTTP/1.1 {} {}\r\n", status, reason_phrase(status));
    let mut has_type = false;
    for (name, value) in &headers {
        let lower = name.to_ascii_lowercase();
        if lower == "content-length" || lower == "connection" || lower == "transfer-encoding" {
            continue; // framing is owned by the runtime
        }
        has_type |= lower == "content-type";
        out.push_str(&format!("{}: {}\r\n", name, value));
    }
    if !has_type {
        out.push_str("content-type: text/plain; charset=utf-8\r\n");
    }
    out.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\nserver: spid3r\r\n\r\n",
        body_len
    ));
    let mut bytes = out.into_bytes();
    match body_raw {
        Some(raw) => bytes.extend_from_slice(&raw),
        None => bytes.extend_from_slice(body_text.as_bytes()),
    }
    let mut s = stream.stream.lock().unwrap();
    match s.write_all(&bytes).and_then(|_| s.flush()) {
        Ok(()) => ok(Value::Int(bytes.len() as i64)),
        Err(e) => err(io_msg(WHO, &e)),
    }
}

/// `http_fetch(method, url, headers?, body?)` -> `ok(response)` where the
/// response map has `status` (int), `headers` (map), and `body`.
/// Plain `http://` only: TLS would need a third-party crate.
fn n_http_fetch(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "http_fetch";
    if !(2..=4).contains(&args.len()) {
        return Err(format!(
            "{} expects 2 to 4 arguments, got {}",
            WHO,
            args.len()
        ));
    }
    let method = want_str(&args[0], WHO)?.to_ascii_uppercase();
    let url = want_str(&args[1], WHO)?;
    let headers = match args.get(2) {
        Some(v) => header_pairs(v, WHO)?,
        None => Vec::new(),
    };
    let body = match args.get(3) {
        Some(Value::Null) | None => String::new(),
        Some(v) => want_str(v, WHO)?,
    };
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Err(format!("{}: invalid method '{}'", WHO, method));
    }
    match fetch(&method, &url, &headers, &body) {
        Ok(v) => ok(v),
        Err(e) => err(e),
    }
}

fn fetch(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: &str,
) -> Result<Value, String> {
    const WHO: &str = "http_fetch";
    let (secure, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(format!(
            "{}: URL must start with http:// or https:// ('{}')",
            WHO, url
        ));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err(format!("{}: URL has no host ('{}')", WHO, url));
    }
    // CR/LF and other control characters in the authority or path would let a
    // crafted URL inject or overwrite request headers (request smuggling).
    let has_ctl = |s: &str| s.chars().any(|c| (c as u32) < 0x20 || c == '\u{7f}');
    if has_ctl(authority) || has_ctl(path) {
        return Err(format!(
            "{}: URL contains control characters ('{}')",
            WHO, url
        ));
    }
    let default_port = if secure { 443 } else { 80 };
    let addr = if authority.contains(':') && !authority.ends_with(']') {
        authority.to_string()
    } else {
        format!("{}:{}", authority, default_port)
    };

    // The TLS handshake needs the bare host name (SNI), not host:port.
    #[allow(unused_variables)]
    let host = match authority.rsplit_once(':') {
        Some((h, _)) if !h.contains(']') => h,
        _ => authority,
    };

    // Both transports are boxed behind one Read+Write surface so the request
    // framing below is written once. I/O timeouts are set on the underlying
    // TcpStream and survive the TLS handshake.
    type Conn = Box<dyn ReadWrite>;

    let mut stream: Conn = if secure {
        #[cfg(feature = "tls")]
        {
            let tcp =
                connect_with_timeout(&addr).map_err(|e| format!("{} '{}': {}", WHO, url, e))?;
            let _ = tcp.set_read_timeout(Some(FETCH_IO_TIMEOUT));
            let _ = tcp.set_write_timeout(Some(FETCH_IO_TIMEOUT));
            let connector = native_tls::TlsConnector::new()
                .map_err(|e| format!("{}: TLS setup failed: {}", WHO, e))?;
            let tls = connector
                .connect(host, tcp)
                .map_err(|e| format!("{}: TLS handshake with '{}' failed: {}", WHO, host, e))?;
            Box::new(tls)
        }
        #[cfg(not(feature = "tls"))]
        {
            let _ = host;
            return Err(format!(
                "{}: https:// needs the `tls` feature (rebuild the toolchain with \
                 --features tls); the zero-dependency core ships without a TLS stack",
                WHO
            ));
        }
    } else {
        let tcp = connect_with_timeout(&addr).map_err(|e| format!("{} '{}': {}", WHO, url, e))?;
        let _ = tcp.set_read_timeout(Some(FETCH_IO_TIMEOUT));
        let _ = tcp.set_write_timeout(Some(FETCH_IO_TIMEOUT));
        Box::new(tcp)
    };

    let mut req = format!(
        "{} {} HTTP/1.1\r\nhost: {}\r\nuser-agent: spid3r\r\naccept: */*\r\nconnection: close\r\n",
        method, path, authority
    );
    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "host" | "connection" | "content-length" | "transfer-encoding"
        ) {
            continue;
        }
        req.push_str(&format!("{}: {}\r\n", name, value));
    }
    if !body.is_empty() || matches!(method, "POST" | "PUT" | "PATCH") {
        req.push_str(&format!("content-length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    req.push_str(body);
    stream
        .write_all(req.as_bytes())
        .map_err(|e| io_msg(WHO, &e))?;

    let head = read_head(&mut *stream, WHO)?;
    let mut parts = head.start_line.splitn(3, ' ');
    let version = parts.next().unwrap_or("");
    let status: i64 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .filter(|_| version.starts_with("HTTP/"))
        .ok_or_else(|| format!("{}: malformed status line '{}'", WHO, head.start_line))?;

    let no_body =
        method == "HEAD" || status == 204 || status == 304 || (100..200).contains(&status);
    let leftover = head.leftover.clone();
    let body_bytes = if no_body {
        Vec::new()
    } else if head
        .header("transfer-encoding")
        .is_some_and(|te| te.to_ascii_lowercase().contains("chunked"))
    {
        read_chunked(&mut *stream, leftover, WHO)?
    } else if let Some(len) = head.header("content-length") {
        let len = len
            .parse::<usize>()
            .map_err(|_| format!("{}: bad content-length '{}'", WHO, len))?;
        read_exact_body(&mut *stream, leftover, len, WHO)?
    } else {
        read_to_eof(&mut *stream, leftover, WHO)?
    };

    make_map(vec![
        ("status".to_string(), Value::Int(status)),
        ("headers".to_string(), headers_value(&head.headers)?),
        (
            "body".to_string(),
            str_val(String::from_utf8_lossy(&body_bytes).into_owned()),
        ),
    ])
}

/// One connection surface for HTTP framing: a plain TCP stream or (with the
/// `tls` feature) a TLS stream over one.
trait ReadWrite: Read + Write {}
impl<T: Read + Write + ?Sized> ReadWrite for T {}

/// `url_decode("a%20b+c")` -> `"a b c"` (form rules: `+` is a space).
fn n_url_decode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("url_decode", &args, 1)?;
    Ok(str_val(percent_decode(
        &want_str(&args[0], "url_decode")?,
        true,
    )))
}

/// `url_encode("a b&c")` -> `"a%20b%26c"` (RFC 3986 unreserved kept).
fn n_url_encode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("url_encode", &args, 1)?;
    Ok(str_val(percent_encode(&want_str(&args[0], "url_encode")?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b", false), "a b");
        assert_eq!(percent_decode("a+b", false), "a+b");
        assert_eq!(percent_decode("a+b", true), "a b");
        assert_eq!(percent_decode("100%", true), "100%");
        assert_eq!(percent_decode("%zz%4", true), "%zz%4");
        assert_eq!(percent_decode("caf%C3%A9", true), "café");
    }

    #[test]
    fn percent_encoding_round_trips() {
        let s = "a b&c=d/é~";
        assert_eq!(percent_decode(&percent_encode(s), false), s);
        assert_eq!(percent_encode("a b"), "a%20b");
    }

    /// A hostile chunked response with a chunk size near usize::MAX must be
    /// rejected by the cap, not wrap `body.len() + size` past it (the cap
    /// check previously used unchecked addition).
    #[test]
    fn chunked_body_overflow_size_is_rejected() {
        use std::io::Write as _;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            // One huge chunk size, then close.
            let _ = s.write_all(b"ffffffffffffffff\r\n");
        });
        let stream = std::net::TcpStream::connect(addr).unwrap();
        let err = read_chunked(&stream, Vec::new(), "test").unwrap_err();
        server.join().unwrap();
        assert!(
            err.contains("body exceeds"),
            "expected the body cap to reject the hostile size, got: {}",
            err
        );
    }
}
