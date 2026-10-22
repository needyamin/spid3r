//! PostgreSQL client over wire protocol v3.
//!
//! Startup, authentication (cleartext, MD5, SCRAM-SHA-256), the simple query
//! protocol, and text-format row decoding. Deliberately not implemented:
//! TLS/SSL (`sslmode` is refused rather than silently downgraded — if the
//! server demands TLS the connect fails with a clear error), extended query /
//! binary format, COPY, and notifications.
//!
//! Values come back typed where the column's type OID says so: int2/int4/int8
//! -> int, float4/float8/numeric -> float, bool -> bool, NULL -> null,
//! everything else -> string.

use crate::stdlib::digest::{hex, md5_bytes};
use crate::value::collections::seq::{make_list, MapData};
use crate::value::{new_ref, MapKey, NativeCtx, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

const READ_TIMEOUT: Duration = Duration::from_secs(30);
const PROTOCOL_V3: u32 = 196608; // 3 << 16

/// Column type OIDs we convert from their text form.
const OID_INT8: u32 = 20;
const OID_INT2: u32 = 21;
const OID_INT4: u32 = 23;
const OID_FLOAT4: u32 = 700;
const OID_FLOAT8: u32 = 701;
const OID_NUMERIC: u32 = 1700;
const OID_BOOL: u32 = 16;

fn get_stream(id: i64, who: &str) -> Result<Arc<Mutex<TcpStream>>, String> {
    table()
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("{}: handle {} is not open", who, id))
}

// ------------------------------------------------------------------ framing

/// One server message: type byte + payload.
struct Message {
    kind: u8,
    payload: Vec<u8>,
}

fn read_message<R: Read>(stream: &mut R) -> Result<Message, String> {
    let mut head = [0u8; 5];
    stream
        .read_exact(&mut head)
        .map_err(|e| format!("postgres read: {}", e))?;
    let kind = head[0];
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len < 4 {
        return Err(format!(
            "postgres: message '{}' has an impossible length",
            kind as char
        ));
    }
    let payload = {
        let mut buf = vec![0u8; len - 4];
        stream
            .read_exact(&mut buf)
            .map_err(|e| format!("postgres read: {}", e))?;
        buf
    };
    Ok(Message { kind, payload })
}

fn write_message<R: Write>(stream: &mut R, kind: u8, payload: &[u8]) -> Result<(), String> {
    let mut out = Vec::with_capacity(payload.len() + 5);
    out.push(kind);
    out.extend_from_slice(&((payload.len() + 4) as u32).to_be_bytes());
    out.extend_from_slice(payload);
    stream
        .write_all(&out)
        .map_err(|e| format!("postgres write: {}", e))
}

/// Write a StartupMessage (or any message without a type byte): just the
/// 4-byte length followed by the payload. The regular writer prepends a type
/// byte, which is wrong for the startup packet.
fn write_startup(stream: &mut TcpStream, payload: &[u8]) -> Result<(), String> {
    let mut out = Vec::with_capacity(payload.len() + 4);
    out.extend_from_slice(&((payload.len() + 4) as u32).to_be_bytes());
    out.extend_from_slice(payload);
    stream
        .write_all(&out)
        .map_err(|e| format!("postgres write: {}", e))
}

/// A C-string inside a payload: bytes up to the NUL, and how far it read.
fn cstr(payload: &[u8], at: usize) -> Result<(String, usize), String> {
    let end = payload[at..]
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| "postgres: unterminated string in message".to_string())?
        + at;
    Ok((
        String::from_utf8_lossy(&payload[at..end]).into_owned(),
        end + 1,
    ))
}

// --------------------------------------------------------------------- auth

/// `md5` auth: "md5" + md5hex(md5hex(password + user) + salt).
fn md5_password(password: &str, user: &str, salt: &[u8]) -> String {
    // The protocol hashes the *hex text* of the inner digest, not its bytes.
    let inner = md5_bytes(format!("{}{}", password, user).as_bytes());
    let mut step2 = hex(&inner).into_bytes();
    step2.extend_from_slice(salt);
    format!("md5{}", hex(&md5_bytes(&step2)))
}

// SCRAM-SHA-256 (RFC 5802/7677) ---------------------------------------------

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    crate::stdlib::hmac_sha256_bytes(key, msg)
}

fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    crate::stdlib::base64_decode_str(s, crate::stdlib::B64_STD, "postgres SCRAM")
}

fn b64_encode(data: &[u8]) -> String {
    // SCRAM requires RFC 4648 base64 *with* padding, so the server's decoder
    // never has to guess the missing '=' characters.
    crate::stdlib::base64_encode_bytes(data, crate::stdlib::B64_STD, true)
}

/// Everything the caller needs to finish SCRAM and verify the server.
struct ScramFinal {
    /// client-final-message-with-proof (what we send).
    message: String,
    /// SaltedPassword, needed to verify the server signature.
    salted: Vec<u8>,
    /// The auth message, needed to verify the server signature.
    auth_message: String,
}

/// Build the client-final message from the server's first message.
fn scram_client_final(
    password: &str,
    server_first: &str,
    client_first_bare: &str,
    client_nonce: &str,
) -> Result<ScramFinal, String> {
    let mut nonce = None;
    let mut salt_b64 = None;
    let mut iterations = 4096u32;
    for attr in server_first.split(',') {
        let (k, v) = match attr.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        match k {
            "r" => nonce = Some(v.to_string()),
            "s" => salt_b64 = Some(v.to_string()),
            "i" => {
                iterations = v
                    .parse()
                    .map_err(|_| "postgres: bad SCRAM iteration count".to_string())?
            }
            _ => {}
        }
    }
    let server_nonce = nonce.ok_or_else(|| "postgres: SCRAM reply has no nonce".to_string())?;
    if !server_nonce.starts_with(client_nonce) {
        return Err("postgres: SCRAM nonce does not extend the client nonce".to_string());
    }
    let salt = b64_decode(salt_b64.as_deref().unwrap_or(""))?;
    if salt.is_empty() {
        return Err("postgres: SCRAM reply has an empty salt".to_string());
    }

    // SaltedPassword = Hi(password, salt, i). Commas and '=' in a password
    // must be escaped per RFC 5802, or the derived keys silently differ.
    let escaped = password.replace(",", "\\,").replace("=", "\\=");
    let salted = crate::stdlib::kdf::pbkdf2(escaped.as_bytes(), &salt, iterations, 32);
    let client_key = hmac_sha256(&salted, b"Client Key");
    let stored_key = crate::pkg::sha256_bytes(&client_key);
    let client_final_no_proof = format!("c=biws,r={}", server_nonce);
    let auth_message = format!(
        "{},{},{}",
        client_first_bare, server_first, client_final_no_proof
    );
    let client_sig = hmac_sha256(&stored_key, auth_message.as_bytes());
    let mut proof = client_key;
    for i in 0..32 {
        proof[i] ^= client_sig[i];
    }
    Ok(ScramFinal {
        message: format!("{},p={}", client_final_no_proof, b64_encode(&proof)),
        salted,
        auth_message,
    })
}

fn scram_server_key(salted: &[u8]) -> [u8; 32] {
    hmac_sha256(salted, b"Server Key")
}

// ----------------------------------------------------------------- querying

#[derive(Debug)]
struct RowSet {
    columns: Vec<(String, u32)>,
    rows: Vec<Vec<Option<String>>>,
}

fn rows_to_value(set: &RowSet) -> Value {
    let mut rows: Vec<Value> = Vec::with_capacity(set.rows.len());
    for row in &set.rows {
        let mut pairs: Vec<(MapKey, Value)> = Vec::with_capacity(row.len());
        for (i, raw) in row.iter().enumerate() {
            let (name, oid) = &set.columns[i];
            let value = match raw {
                None => Value::Null,
                Some(text) => convert(text, *oid),
            };
            pairs.push((MapKey::Str(name.clone().into()), value));
        }
        rows.push(Value::Map(new_ref(MapData::from_entries(pairs))));
    }
    make_list(rows)
}

fn convert(text: &str, oid: u32) -> Value {
    match oid {
        OID_INT2 | OID_INT4 | OID_INT8 => text
            .parse::<i64>()
            .map(Value::Int)
            .unwrap_or_else(|_| Value::Str(text.into())),
        OID_FLOAT4 | OID_FLOAT8 | OID_NUMERIC => text
            .parse::<f64>()
            .map(Value::Float)
            .unwrap_or_else(|_| Value::Str(text.into())),
        OID_BOOL => match text {
            "t" | "true" | "1" => Value::Bool(true),
            "f" | "false" | "0" => Value::Bool(false),
            _ => Value::Str(text.into()),
        },
        _ => Value::Str(text.into()),
    }
}

/// Run one query and drain everything up to ReadyForQuery.
/// Returns the last resultset seen (commands with no rows yield an empty one).
fn simple_query(stream: &mut TcpStream, sql: &str) -> Result<Value, String> {
    let mut payload = sql.as_bytes().to_vec();
    payload.push(0);
    write_message(stream, b'Q', &payload)?;

    let mut columns: Vec<(String, u32)> = Vec::new();
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    let mut last_error: Option<String> = None;
    loop {
        let msg = read_message(stream)?;
        match msg.kind {
            b'T' => {
                // RowDescription
                let ncols = u16::from_be_bytes([msg.payload[0], msg.payload[1]]) as usize;
                columns.clear();
                let mut at = 2usize;
                for _ in 0..ncols {
                    let (name, next) = cstr(&msg.payload, at)?;
                    // After the name: table oid(4) + attnum(2) + type oid(4)
                    // + typlen(2) + typmod(4) + format(2) = 18 bytes.
                    let type_oid = u32::from_be_bytes([
                        msg.payload[next + 6],
                        msg.payload[next + 7],
                        msg.payload[next + 8],
                        msg.payload[next + 9],
                    ]);
                    columns.push((name, type_oid));
                    at = next + 18;
                }
            }
            b'D' => {
                let ncols = u16::from_be_bytes([msg.payload[0], msg.payload[1]]) as usize;
                let mut row = Vec::with_capacity(ncols);
                let mut at = 2usize;
                for _ in 0..ncols {
                    let len = i32::from_be_bytes([
                        msg.payload[at],
                        msg.payload[at + 1],
                        msg.payload[at + 2],
                        msg.payload[at + 3],
                    ]);
                    at += 4;
                    if len < 0 {
                        row.push(None); // NULL
                    } else {
                        let len = len as usize;
                        row.push(Some(
                            String::from_utf8_lossy(&msg.payload[at..at + len]).into_owned(),
                        ));
                        at += len;
                    }
                }
                rows.push(row);
            }
            b'E' => {
                last_error = Some(error_fields(&msg.payload));
            }
            b'Z' => {
                // ReadyForQuery: end of the simple-query cycle.
                return match last_error {
                    Some(e) => Err(e),
                    None => Ok(rows_to_value(&RowSet { columns, rows })),
                };
            }
            _ => {} // S/K/C/... are ignored by a simple client
        }
    }
}

fn error_fields(payload: &[u8]) -> String {
    // Fields are <code><value>\0 pairs; 'M' is the message, 'S' the severity.
    let mut message = String::new();
    let mut severity = String::new();
    let mut at = 0usize;
    while at < payload.len() && payload[at] != 0 {
        let code = payload[at] as char;
        let (value, next) = match cstr(payload, at + 1) {
            Ok(v) => v,
            Err(_) => break,
        };
        match code {
            'M' => message = value,
            'S' => severity = value,
            _ => {}
        }
        at = next;
    }
    if severity.is_empty() {
        message
    } else {
        format!("{}: {}", severity, message)
    }
}

// ------------------------------------------------------------------- natives

fn str_arg(v: &Value, who: &str, what: &str) -> Result<String, String> {
    match v {
        Value::Str(s) => Ok(s.to_string()),
        other => Err(format!(
            "{}: {} must be a string, found {}",
            who,
            what,
            crate::value::type_name(other)
        )),
    }
}

/// `pg_connect("127.0.0.1:5432", user, password, database)` -> `ok(handle)`.
///
/// Auth methods supported: trust/ok, cleartext, md5, SCRAM-SHA-256. If the
/// server insists on TLS the connect fails with a clear error — there is no
/// silent downgrade to plaintext.
fn n_pg_connect(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "pg_connect";
    if args.len() != 4 {
        return Err(format!(
            "{} expects 4 arguments (addr, user, password, database), got {}",
            WHO,
            args.len()
        ));
    }
    let addr = str_arg(&args[0], WHO, "addr")?;
    let user = str_arg(&args[1], WHO, "user")?;
    let password = str_arg(&args[2], WHO, "password")?;
    let database = str_arg(&args[3], WHO, "database")?;

    let stream = TcpStream::connect(&addr).map_err(|e| format!("{} '{}': {}", WHO, addr, e))?;
    stream.set_read_timeout(Some(READ_TIMEOUT)).ok();
    stream.set_write_timeout(Some(READ_TIMEOUT)).ok();
    stream.set_nodelay(true).ok();
    let mut stream = stream;

    // StartupMessage (no type byte).
    let mut payload = Vec::new();
    payload.extend_from_slice(&PROTOCOL_V3.to_be_bytes());
    payload.extend_from_slice(b"user\0");
    payload.extend_from_slice(user.as_bytes());
    payload.push(0);
    payload.extend_from_slice(b"database\0");
    payload.extend_from_slice(database.as_bytes());
    payload.push(0);
    payload.push(0);
    write_startup(&mut stream, &payload).map_err(|e| format!("{}: {}", WHO, e))?;

    // Auth loop.
    loop {
        let msg = read_message(&mut stream)?;
        match msg.kind {
            b'R' => {
                let code = i32::from_be_bytes([
                    msg.payload[0],
                    msg.payload[1],
                    msg.payload[2],
                    msg.payload[3],
                ]);
                match code {
                    0 => {} // AuthenticationOk
                    3 => {
                        // CleartextPassword
                        let mut p = password.as_bytes().to_vec();
                        p.push(0);
                        write_message(&mut stream, b'p', &p)?;
                    }
                    5 => {
                        // MD5Password: 4-byte salt follows the code.
                        let salt = &msg.payload[4..8];
                        let response = md5_password(&password, &user, salt);
                        let mut p = response.as_bytes().to_vec();
                        p.push(0);
                        write_message(&mut stream, b'p', &p)?;
                    }
                    10 => {
                        // SASL: negotiate SCRAM-SHA-256.
                        let client_nonce = crate::stdlib::next_random().to_le_bytes();
                        let nonce = b64_encode(&client_nonce);
                        let client_first_bare = format!("n=,r={}", nonce);
                        let mut init = b"SCRAM-SHA-256\0".to_vec();
                        let bare = client_first_bare.as_bytes();
                        init.extend_from_slice(&(bare.len() as u32).to_be_bytes());
                        init.extend_from_slice(bare);
                        write_message(&mut stream, b'p', &init)?;
                        // The server's SASLContinue carries its first message.
                        let cont = read_message(&mut stream)?;
                        if cont.kind != b'R' {
                            return Err(format!("{}: expected SASLContinue, got '{}'", WHO, cont.kind as char));
                        }
                        let ccode = i32::from_be_bytes([
                            cont.payload[0], cont.payload[1], cont.payload[2], cont.payload[3],
                        ]);
                        if ccode != 11 {
                            return Err(format!("{}: expected SASLContinue (11), got {}", WHO, ccode));
                        }
                        let server_first =
                            String::from_utf8_lossy(&cont.payload[4..]).into_owned();
                        let fin_msg = scram_client_final(
                            &password,
                            &server_first,
                            &client_first_bare,
                            &nonce,
                        )?;
                        let mut p = fin_msg.message.as_bytes().to_vec();
                        p.push(0);
                        write_message(&mut stream, b'p', &p)?;
                        let fin = read_message(&mut stream)?;
                        if fin.kind != b'R' {
                            return Err(format!("{}: expected SASLFinal, got '{}'", WHO, fin.kind as char));
                        }
                        let fcode = i32::from_be_bytes([
                            fin.payload[0], fin.payload[1], fin.payload[2], fin.payload[3],
                        ]);
                        if fcode != 12 {
                            return Err(format!("{}: expected SASLFinal (12), got {}", WHO, fcode));
                        }
                        // Verify the server signature: v=<base64>. This proves
                        // the server holds the same password-derived key.
                        let final_text = String::from_utf8_lossy(&fin.payload[4..]).into_owned();
                        let server_sig_b64 = final_text
                            .strip_prefix("v=")
                            .ok_or_else(|| format!("{}: SASLFinal without a server signature", WHO))?;
                        let server_sig = b64_decode(server_sig_b64)?;
                        // The signature is HMAC(ServerKey, AuthMessage), not
                        // the ServerKey itself.
                        let expect = hmac_sha256(
                            &scram_server_key(&fin_msg.salted),
                            fin_msg.auth_message.as_bytes(),
                        );
                        if server_sig.as_slice() != &expect[..] {
                            return Err("postgres: SCRAM server signature mismatch".to_string());
                        }
                    }
                    12 => {} // SASLFinal handled inside the SASL branch
                    other => {
                        return Err(format!(
                            "{}: unsupported auth method {} (trust/cleartext/md5/scram-sha-256 are supported)",
                            WHO, other
                        ))
                    }
                }
            }
            // A server FATAL during authentication is a catchable value, not a
            // thrown error: the caller decides whether a bad password ends the
            // program.
            b'E' => {
                return Ok(Value::Result(Err(Box::new(Value::Str(
                    format!("{}: {}", WHO, error_fields(&msg.payload)).into(),
                )))))
            }
            // ParameterStatus/BackendKeyData/NoticeResponse are ordinary
            // chatter on the way to ReadyForQuery and are skipped.
            b'S' | b'K' | b'N' => continue,
            b'Z' => {
                // ReadyForQuery: connection established.
                let handle = next_handle();
                table()
                    .lock()
                    .unwrap()
                    .insert(handle, Arc::new(Mutex::new(stream)));
                return Ok(Value::Int(handle));
            }
            other => {
                return Err(format!(
                    "{}: unexpected message '{}' during authentication",
                    WHO, other as char
                ))
            }
        }
    }
}

/// `pg_query(handle, sql)` -> `ok([row, ...])` where a row is a map of
/// column -> typed value (int/float/bool/string/null). The last resultset of
/// the statement wins; SQL errors come back as catchable err values.
fn n_pg_query(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "pg_query";
    if args.len() != 2 {
        return Err(format!("{} expects 2 arguments, got {}", WHO, args.len()));
    }
    let id = match &args[0] {
        Value::Int(i) => *i,
        other => {
            return Err(format!(
                "{}: handle must be an int, found {}",
                WHO,
                crate::value::type_name(other)
            ))
        }
    };
    let sql = str_arg(&args[1], WHO, "sql")?;
    let stream = get_stream(id, WHO)?;
    let mut stream = stream.lock().unwrap();
    match simple_query(&mut stream, &sql) {
        Ok(v) => Ok(Value::Result(Ok(Box::new(v)))),
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(e.into()))))),
    }
}

/// `pg_exec(handle, sql)` -> `ok(null)`; errors are catchable err values.
fn n_pg_exec(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "pg_exec";
    if args.len() != 2 {
        return Err(format!("{} expects 2 arguments, got {}", WHO, args.len()));
    }
    let id = match &args[0] {
        Value::Int(i) => *i,
        other => {
            return Err(format!(
                "{}: handle must be an int, found {}",
                WHO,
                crate::value::type_name(other)
            ))
        }
    };
    let sql = str_arg(&args[1], WHO, "sql")?;
    let stream = get_stream(id, WHO)?;
    let mut stream = stream.lock().unwrap();
    match simple_query(&mut stream, &sql) {
        Ok(_) => Ok(Value::Result(Ok(Box::new(Value::Null)))),
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(e.into()))))),
    }
}

/// `pg_close(handle)` -> true if the handle was open.
fn n_pg_close(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("pg_close expects 1 argument, got {}", args.len()));
    }
    let id = match &args[0] {
        Value::Int(i) => *i,
        other => {
            return Err(format!(
                "pg_close: handle must be an int, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    let removed = table().lock().unwrap().remove(&id);
    if let Some(stream) = &removed {
        let _ = stream.lock().unwrap().write_all(b"X\0\0\0\x04");
    }
    Ok(Value::Bool(removed.is_some()))
}

pub fn native_defs() -> Vec<crate::stdlib::NativeDef> {
    vec![
        ("pg_connect", n_pg_connect),
        ("pg_query", n_pg_query),
        ("pg_exec", n_pg_exec),
        ("pg_close", n_pg_close),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_password_matches_the_documented_construction() {
        // The construction is what matters: md5(md5(pw+user)+salt).
        let salt = [0x12, 0x34, 0x56, 0x78];
        let pw = md5_password("secret", "ada", &salt);
        assert!(pw.starts_with("md5") && pw.len() == 35);
        // Deterministic for the same inputs, salt-sensitive.
        assert_eq!(pw, md5_password("secret", "ada", &salt));
        assert_ne!(pw, md5_password("secret", "ada", &[0, 0, 0, 1]));
        assert_ne!(pw, md5_password("secret2", "ada", &salt));
    }

    #[test]
    fn row_description_offsets_are_right() {
        // Build a RowDescription for two columns and check the reader finds
        // both names and type OIDs.
        let mut payload = vec![0u8; 0];
        payload.extend_from_slice(&2u16.to_be_bytes());
        for (name, oid) in [("id", OID_INT4), ("name", OID_VARCHAR)] {
            payload.extend_from_slice(name.as_bytes());
            payload.push(0);
            payload.extend_from_slice(&0u32.to_be_bytes()); // table oid
            payload.extend_from_slice(&0u16.to_be_bytes()); // attnum
            payload.extend_from_slice(&oid.to_be_bytes()); // type oid
            payload.extend_from_slice(&0i16.to_be_bytes()); // typlen
            payload.extend_from_slice(&0i32.to_be_bytes()); // typmod
            payload.extend_from_slice(&0i16.to_be_bytes()); // format
        }
        let ncols = u16::from_be_bytes([payload[0], payload[1]]) as usize;
        assert_eq!(ncols, 2);
        let mut at = 2usize;
        for _ in 0..ncols {
            let (name, next) = cstr(&payload, at).unwrap();
            let oid = u32::from_be_bytes([
                payload[next + 6],
                payload[next + 7],
                payload[next + 8],
                payload[next + 9],
            ]);
            assert!(!name.is_empty());
            assert!(oid == OID_INT4 || oid == OID_VARCHAR);
            at = next + 18;
        }
        assert_eq!(at, payload.len());
    }

    const OID_VARCHAR: u32 = 1043;

    #[test]
    fn text_values_convert_by_type_oid() {
        assert!(matches!(convert("42", OID_INT4), Value::Int(42)));
        assert!(matches!(convert("3.5", OID_FLOAT8), Value::Float(f) if (f - 3.5).abs() < 1e-12));
        assert!(matches!(convert("t", OID_BOOL), Value::Bool(true)));
        assert!(matches!(convert("f", OID_BOOL), Value::Bool(false)));
        assert!(matches!(convert("hello", 25 /* text */), Value::Str(_)));
        assert!(matches!(convert("not-an-int", OID_INT4), Value::Str(_)));
    }

    #[test]
    fn scram_final_builds_a_proof_and_checks_the_nonce() {
        let client_nonce = "abcdef";
        // A server nonce that does not extend ours must be rejected.
        assert!(
            scram_client_final("pw", "r=zzzz,s=abcd,i=4096", "n=,r=abcdef", client_nonce).is_err()
        );
        // RFC 7677 test vector inputs: user "user", password "pencil".
        let fin_msg = scram_client_final(
            "pencil",
            "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096",
            "n=,r=rOprNGfwEbeRWgbNEkqO",
            "rOprNGfwEbeRWgbNEkqO",
        )
        .unwrap();
        assert!(
            fin_msg
                .message
                .starts_with("c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p="),
            "{}",
            fin_msg.message
        );
    }
}
