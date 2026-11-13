//! Remote-management agent and admin client.
//!
//! One binary, two roles:
//!
//! * `spid3r agent start` — listen on `[agent] bind:port`, pair devices, serve
//!   the encrypted API. **No unrestricted shell.** Every operation is a
//!   predefined name checked against the caller's role.
//! * `spid3r admin ...` — pair to a host, then call those operations.
//!
//! Transport is ChaCha20 + HMAC-SHA256 (encrypt-then-MAC) over TCP
//! (`spid3r-agent/1`). There is no
//! plaintext data frame. Pairing uses a one-time 48-hex code the operator
//! reads off the host and types into the client; the code never becomes a
//! long-lived key.

pub mod protocol;
pub mod store;

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::config;
use crate::crypto::{self, KEY_LEN, NONCE_LEN};
use crate::value::{MapKey, Value};
use store::Store;

use protocol::{KIND_DATA, KIND_ERROR, KIND_PAIR};

/// Set by the SIGHUP handler (async-signal-safe: only this flag store); the
/// accept loop and every connection loop perform the actual reload.
static RELOAD_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Set when `ops.run restart_runtime` has acked the client and the process
/// should exit so the freshly spawned child can take over the port.
static EXIT_AFTER_REPLY: AtomicBool = AtomicBool::new(false);

/// Perform a pending SIGHUP reload, if one was requested.
fn maybe_reload() {
    if RELOAD_REQUESTED.swap(false, Ordering::Relaxed) {
        config::reload();
    }
}

// ------------------------------------------------------------------- helpers

fn now() -> i64 {
    crate::stdlib::datetime::unix_time()
}

fn json_str_field(v: &Value, key: &str) -> Option<String> {
    let Value::Map(m) = v else { return None };
    let m = m.lock().unwrap();
    let i = m.find(&MapKey::Str(key.into()))?;
    match &m.entries[i].1 {
        Value::Str(s) => Some(s.to_string()),
        _ => None,
    }
}

fn json_int_field(v: &Value, key: &str) -> Option<i64> {
    let Value::Map(m) = v else { return None };
    let m = m.lock().unwrap();
    let i = m.find(&MapKey::Str(key.into()))?;
    match &m.entries[i].1 {
        Value::Int(n) => Some(*n),
        Value::Float(f) => Some(*f as i64),
        _ => None,
    }
}

fn json_bool_field(v: &Value, key: &str) -> Option<bool> {
    let Value::Map(m) = v else { return None };
    let m = m.lock().unwrap();
    let i = m.find(&MapKey::Str(key.into()))?;
    match &m.entries[i].1 {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

fn ok_json(extra: Vec<(MapKey, Value)>) -> Value {
    use crate::value::collections::MapData;
    use crate::value::new_ref;
    let mut entries = vec![(MapKey::Str("ok".into()), Value::Bool(true))];
    entries.extend(extra);
    Value::Map(new_ref(MapData::from_entries(entries)))
}

fn err_json(msg: &str) -> Value {
    use crate::value::collections::MapData;
    use crate::value::new_ref;
    Value::Map(new_ref(MapData::from_entries(vec![
        (MapKey::Str("ok".into()), Value::Bool(false)),
        (MapKey::Str("error".into()), Value::Str(msg.into())),
    ])))
}

// -------------------------------------------------------------------- server

/// Serve until the process is asked to stop. Blocks the calling thread.
pub fn serve() -> Result<(), String> {
    if !config::get("security.allow_remote")
        .map(|v| config::schema::parse_bool(&v))
        .unwrap_or(false)
        && !config::get("agent.enabled")
            .map(|v| config::schema::parse_bool(&v))
            .unwrap_or(false)
    {
        eprintln!(
            "agent: disabled by configuration (set [agent] enabled = true and/or [security] allow_remote = true)"
        );
        return Err("agent disabled".to_string());
    }
    let bind = config::get("agent.bind").unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = config::get("agent.port")
        .and_then(|v| v.parse().ok())
        .unwrap_or(9527);
    let max_conn: usize = config::get("agent.max_connections")
        .and_then(|v| v.parse().ok())
        .unwrap_or(16);
    let timeout_ms: u64 = config::get("agent.request_timeout_ms")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000);
    let max_body: usize = config::get("agent.max_body")
        .and_then(|v| v.parse().ok())
        .unwrap_or(262_144);

    let addr = format!("{}:{}", bind, port);
    // Retry the bind for a few seconds: `ops.run restart_runtime` spawns the
    // child before the parent's listener socket is released, so without a
    // retry the child can lose the race, exit on EADDRINUSE and leave the
    // host with no agent at all.
    let mut listener = None;
    for attempt in 0..50 {
        match TcpListener::bind(&addr) {
            Ok(l) => {
                listener = Some(l);
                break;
            }
            Err(e) if attempt < 49 => {
                if attempt == 0 {
                    eprintln!("agent: bind {} busy ({}), retrying...", addr, e);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("bind {}: {}", addr, e)),
        }
    }
    let listener = listener.ok_or_else(|| format!("bind {}: unavailable", addr))?;
    // Non-blocking accept: the loop below polls every 100ms, which lets a
    // pending SIGHUP reload flag be consumed even when no client ever
    // connects. With the blocking accept, an idle agent only reloaded when
    // the next connection arrived.
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("nonblocking: {}", e))?;
    eprintln!("agent: listening on {} (spid3r-agent/1)", addr);

    let live = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Server-wide replay window, shared by every connection: nonces seen
    // recently, with their arrival time. (A per-connection window let a
    // captured frame be replayed on a fresh TCP connection.)
    let seen = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::<(
        String,
        i64,
    )>::new()));

    // SIGHUP reload on Unix: the handler only raises a flag (allocations,
    // locks and file I/O are not async-signal-safe); the loops reload.
    #[cfg(unix)]
    unsafe {
        libc_signal(1, sig_handler);
    }

    loop {
        maybe_reload();
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        match listener.accept() {
            Ok((stream, peer)) => {
                if live.load(std::sync::atomic::Ordering::Relaxed) >= max_conn {
                    // Refusal must be visible: otherwise an operator watching
                    // only the log sees a silent connection black hole.
                    crate::logging::record("agent", "connection refused: max_connections reached");
                    drop(stream);
                    continue;
                }
                live.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let live2 = live.clone();
                let seen2 = seen.clone();
                let timeout = Duration::from_millis(timeout_ms);
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(timeout));
                    let _ = stream.set_write_timeout(Some(timeout));
                    let _ = stream.set_nodelay(true);
                    // catch_unwind: a panic inside handle_conn must not skip
                    // the connection-counter decrement, or after max_conn
                    // panics the agent silently refuses every new client.
                    let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let _ = handle_conn(stream, peer.to_string(), max_body, &seen2, timeout);
                    }));
                    if done.is_err() {
                        crate::logging::record("agent", "connection handler panicked");
                    }
                    live2.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                });
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::WouldBlock {
                    // Nothing to accept: consume a pending reload and yield.
                    maybe_reload();
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                crate::logging::record("agent", &format!("accept: {}", e));
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
extern "C" fn sig_handler(_sig: i32) {
    // Async-signal-safe by construction: a single lock-free atomic store.
    // The real reload (locks + file I/O) happens in maybe_reload().
    RELOAD_REQUESTED.store(true, Ordering::SeqCst);
}

#[cfg(unix)]
unsafe fn libc_signal(sig: i32, handler: extern "C" fn(i32)) {
    extern "C" {
        // Same signature as the declaration in stdlib/shutdown.rs: a
        // mismatched redeclaration of the same libc symbol is a hard clippy
        // error. The handler is passed as usize and cast inside.
        fn signal(sig: i32, handler: usize) -> usize;
    }
    signal(sig, handler as usize);
}

/// Half-open window for replayed nonces: an entry older than this is evicted
/// even under the cap (frames older than the window are stale anyway —
/// clients rotate nonces per frame).
const REPLAY_TTL_SEC: i64 = 3600;
const REPLAY_CAP: usize = 4096;

fn handle_conn(
    stream: TcpStream,
    peer: String,
    max_body: usize,
    seen: &std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<(String, i64)>>>,
    frame_timeout: Duration,
) -> Result<(), String> {
    // Reads go through a deadline wrapper (a per-read socket timeout is
    // re-armed by `read_exact` on every byte, so a dribbling peer could hold
    // a slot forever); writes keep the original stream handle.
    let mut rdr = match stream.try_clone() {
        Ok(s) => protocol::DeadlineReader::new(s, frame_timeout),
        Err(e) => return Err(format!("clone stream: {}", e)),
    };
    let mut stream = stream;
    let lookup = |device: &[u8; 16]| -> Option<[u8; KEY_LEN]> {
        let st = Store::open();
        let hex = protocol::device_hex(device);
        let d = st.find_device(&hex)?;
        let mut key = [0u8; KEY_LEN];
        let raw = crate::pkg::hex_decode(&d.shared)?;
        if raw.len() != KEY_LEN {
            return None;
        }
        key.copy_from_slice(&raw);
        Some(key)
    };

    let mut pair_failures = 0usize;
    loop {
        maybe_reload();
        // Reject an oversized frame right after the header, BEFORE reading
        // and buffering the body: agent.max_body is far smaller than the
        // 8 MiB wire cap, and an unauthenticated peer could otherwise park
        // 8 MiB per connection in server memory.
        // The deadline is per frame: a new budget for every request.
        rdr.set_deadline(frame_timeout);
        let (hdr, payload) =
            match protocol::read_frame_bounded(&mut rdr, &lookup, max_body + crypto::TAG_LEN) {
                Ok(x) => x,
                Err(e) => {
                    // A clean EOF (client closed after one request) is normal,
                    // and so is a dribbling client killed by the deadline —
                    // logging those only enables a log-flood DoS.
                    if e != "closed"
                        && e != "authentication failed"
                        && !e.contains("unknown device")
                        && !e.contains("deadline")
                    {
                        crate::logging::record("agent", &format!("{}: {}", peer, e));
                    }
                    return Err(e);
                }
            };
        if payload.len() > max_body {
            let _ = reply(
                &mut stream,
                KIND_ERROR,
                &hdr.device,
                &lookup,
                err_json("body too large"),
                &peer,
            );
            return Err("body too large".to_string());
        }

        match hdr.kind {
            KIND_PAIR => {
                let req = match crate::stdlib::json_parse_text(&String::from_utf8_lossy(&payload)) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = reply_plain(&mut stream, err_json(&format!("bad json: {}", e)));
                        continue;
                    }
                };
                // The pairing consume is a read-modify-write on devices.json;
                // hold the process-wide store lock so two concurrent pair
                // requests cannot consume one code twice.
                let pair_result = {
                    let _guard = store::lock();
                    let mut st = Store::open();
                    st.sweep();
                    let code = json_str_field(&req, "code").unwrap_or_default();
                    let name = json_str_field(&req, "name").unwrap_or_default();
                    match st.consume_pairing(&code, &name) {
                        Ok((device, shared)) => {
                            if let Err(e) = st.save() {
                                crate::logging::record("agent", &format!("save devices: {}", e));
                            }
                            Ok((device, shared))
                        }
                        Err(e) => Err(e),
                    }
                };
                match pair_result {
                    Ok((device, shared)) => {
                        pair_failures = 0;
                        store::audit(
                            &device.name,
                            &device.role,
                            "pair",
                            true,
                            &peer,
                            &format!("device {}", device.id),
                        );
                        // Never send the long-term shared key. The client
                        // derives the same value from the pairing code it
                        // already holds (SHA-256 of the code).
                        let check = crate::pkg::sha256_hex(&shared);
                        let resp = ok_json(vec![
                            (
                                MapKey::Str("device_id".into()),
                                Value::Str(device.id.into()),
                            ),
                            (
                                MapKey::Str("check".into()),
                                Value::Str(check[..8].to_string().into()),
                            ),
                            (MapKey::Str("role".into()), Value::Str(device.role.into())),
                        ]);
                        let _ = reply_plain(&mut stream, resp);
                    }
                    Err(e) => {
                        pair_failures += 1;
                        store::audit("unknown", "?", "pair", false, &peer, &e);
                        let _ = reply_plain(&mut stream, err_json(&e));
                        // A code-guessing client gets five tries per
                        // connection, then the connection goes away: the
                        // next attempt pays a fresh TCP connect.
                        if pair_failures >= 5 {
                            return Err("too many pairing failures".to_string());
                        }
                    }
                }
            }
            KIND_ERROR => {
                // Only the agent sends error frames; a client-sent one is
                // protocol abuse (an attempt to inject fake failures into
                // the stream or probe the parser), not a request.
                let _ = reply(
                    &mut stream,
                    KIND_ERROR,
                    &hdr.device,
                    &lookup,
                    err_json("clients may not send error frames"),
                    &peer,
                );
                return Err("client sent an error frame".to_string());
            }
            KIND_DATA => {
                let req = match crate::stdlib::json_parse_text(&String::from_utf8_lossy(&payload)) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = reply(
                            &mut stream,
                            KIND_ERROR,
                            &hdr.device,
                            &lookup,
                            err_json(&format!("bad json: {}", e)),
                            &peer,
                        );
                        continue;
                    }
                };
                // Replay guard: reject a nonce we have already accepted.
                // Server-wide and time-bounded, so a captured frame cannot be
                // replayed on a fresh TCP connection or after eviction.
                let nonce_id = format!(
                    "{}:{}",
                    protocol::device_hex(&hdr.device),
                    crate::pkg::hex_encode(&hdr.nonce)
                );
                let t = now();
                let mut seen_w = seen.lock().unwrap_or_else(|e| e.into_inner());
                while let Some((_, ts)) = seen_w.front() {
                    if t - ts > REPLAY_TTL_SEC {
                        seen_w.pop_front();
                    } else {
                        break;
                    }
                }
                if seen_w.iter().any(|(id, _)| *id == nonce_id) {
                    drop(seen_w);
                    let _ = reply(
                        &mut stream,
                        KIND_ERROR,
                        &hdr.device,
                        &lookup,
                        err_json("replayed frame"),
                        &peer,
                    );
                    continue;
                }
                seen_w.push_back((nonce_id, t));
                while seen_w.len() > REPLAY_CAP {
                    seen_w.pop_front();
                }
                drop(seen_w);

                let hex_id = protocol::device_hex(&hdr.device);
                // touch+save is a read-modify-write; hold the store lock so a
                // concurrent connection's stale snapshot cannot erase newer
                // devices from devices.json.
                {
                    let _guard = store::lock();
                    let mut st = Store::open();
                    if st.find_device(&hex_id).is_none() {
                        drop(_guard);
                        let _ = reply(
                            &mut stream,
                            KIND_ERROR,
                            &hdr.device,
                            &lookup,
                            err_json("unknown device"),
                            &peer,
                        );
                        continue;
                    }
                    st.touch(&hex_id);
                    let _ = st.save();
                }
                let device = {
                    let st = Store::open();
                    st.find_device(&hex_id).cloned()
                };
                let Some(device) = device else {
                    let _ = reply(
                        &mut stream,
                        KIND_ERROR,
                        &hdr.device,
                        &lookup,
                        err_json("unknown device"),
                        &peer,
                    );
                    continue;
                };
                let op = json_str_field(&req, "op").unwrap_or_default();
                let allowed = store::op_allowed(&device.role, &op);
                store::audit(&device.name, &device.role, &op, allowed, &peer, "");
                if !allowed {
                    let _ = reply(
                        &mut stream,
                        KIND_ERROR,
                        &hdr.device,
                        &lookup,
                        err_json(&format!("role '{}' may not call '{}'", device.role, op)),
                        &peer,
                    );
                    continue;
                }
                let resp = dispatch(&op, &req, &device);
                let _ = reply(&mut stream, KIND_DATA, &hdr.device, &lookup, resp, &peer);
                // restart_runtime acks first and exits only after the reply
                // frame is on the wire; previously the process died inside
                // dispatch and every client saw a dropped connection.
                if EXIT_AFTER_REPLY.load(Ordering::SeqCst) {
                    std::process::exit(0);
                }
            }
            other => {
                let _ = reply_plain(&mut stream, err_json(&format!("unknown kind {}", other)));
            }
        }
    }
}

fn reply_plain(w: &mut TcpStream, v: Value) -> Result<(), String> {
    let text = crate::stdlib::json_write_text(&v)?;
    protocol::write_frame(w, KIND_PAIR, &[0u8; 16], text.as_bytes(), None)
        .map_err(|e| e.to_string())
}

fn reply(
    w: &mut TcpStream,
    kind: u8,
    device: &[u8; 16],
    lookup: &dyn Fn(&[u8; 16]) -> Option<[u8; KEY_LEN]>,
    v: Value,
    _peer: &str,
) -> Result<(), String> {
    let Some(key) = lookup(device) else {
        return Err("no key".to_string());
    };
    let text = crate::stdlib::json_write_text(&v)?;
    protocol::write_frame(w, kind, device, text.as_bytes(), Some(&key)).map_err(|e| e.to_string())
}

// ----------------------------------------------------------------- operations

/// The `values` map returned by the remote dump-all `config.get` branch
/// (no `key` field). Factored out of `dispatch` so the redaction contract
/// is testable without a live socket: a remote dump applies
/// `redact_value_remote` — credentials AND the policy keys — exactly like
/// the keyed branch. This branch originally called the local
/// `redact_value` and shipped `security.basedir` /
/// `security.disable_natives` raw to a paired peer (QA-caught: only the
/// keyed path had been pinned).
fn config_values_map_remote(values: &std::collections::BTreeMap<String, String>) -> Value {
    use crate::value::collections::MapData;
    use crate::value::new_ref;
    let mut rows = Vec::new();
    for (k, v) in values {
        rows.push((
            MapKey::Str(k.clone().into()),
            Value::Str(crate::runtime_info::redact_value_remote(k, v).into()),
        ));
    }
    Value::Map(new_ref(MapData::from_entries(rows)))
}

fn dispatch(op: &str, req: &Value, _device: &store::Device) -> Value {
    match op {
        "ping" => ok_json(vec![
            (MapKey::Str("pong".into()), Value::Bool(true)),
            (MapKey::Str("ts".into()), Value::Int(now())),
            (
                MapKey::Str("agent".into()),
                Value::Str("spid3r-agent/1".into()),
            ),
        ]),
        "status" => status_json(),
        "info" => {
            let no_redact = json_bool_field(req, "no_redact").unwrap_or(false);
            // Remote callers never get unredacted output, and the policy
            // keys (security.basedir, security.disable_natives) redact too
            // (F-003): they map the host layout and name the denials.
            Value::Map({
                use crate::value::collections::MapData;
                use crate::value::new_ref;
                let inner = crate::runtime_info::info_map_remote(true);
                let _ = no_redact;
                new_ref(MapData::from_entries(vec![
                    (MapKey::Str("ok".into()), Value::Bool(true)),
                    (MapKey::Str("info".into()), inner),
                ]))
            })
        }
        "config.get" => {
            let key = json_str_field(req, "key");
            let cfg = config::snapshot();
            match key {
                Some(k) => match cfg.get(&k) {
                    Some(v) => ok_json(vec![
                        (MapKey::Str("key".into()), Value::Str(k.as_str().into())),
                        (
                            MapKey::Str("value".into()),
                            Value::Str(
                                // Remote surface: credentials AND the policy
                                // keys (basedir, disable_natives) are
                                // redacted (F-003). The local CLI and script
                                // `config_get` keep showing policy values —
                                // the script that is the subject of the
                                // policy can probe both lists by calling the
                                // natives anyway.
                                crate::runtime_info::redact_value_remote(&k, v).into(),
                            ),
                        ),
                    ]),
                    None => err_json(&format!("unknown key '{}'", k)),
                },
                None => ok_json(vec![(
                    MapKey::Str("values".into()),
                    config_values_map_remote(&cfg.values),
                )]),
            }
        }
        "config.set" => {
            let Some(key) = json_str_field(req, "key") else {
                return err_json("missing key");
            };
            let Some(value) = json_str_field(req, "value") else {
                return err_json("missing value");
            };
            // Remote config.set must not be able to grant itself more power
            // or cover its tracks. Which keys are blocked is derived from
            // the schema (`frozen` on each KeySpec — see
            // schema::frozen_set_is_pinned), not a hand-written list, so a
            // new capability-widening key cannot silently forget the
            // classification. Benign keys (e.g. agent.name,
            // packages.registry, modules.assert_active) stay settable;
            // everything else uses the local CLI.
            if crate::config::schema::known_key(&key).is_some_and(|spec| spec.frozen) {
                return err_json(&format!(
                    "key '{}' may not be changed remotely (edit spid3r.ini on the host)",
                    key
                ));
            }
            let before = config::snapshot();
            if config::pin_failed(&before) {
                return err_json(
                    "SPID3R_INI pin is broken; remote config.set is disabled until the host repairs it",
                );
            }
            match config::set_live(&key, &value) {
                Ok(()) => {
                    // Persist through the file when we can.
                    let cfg = config::snapshot();
                    match config::save(&cfg) {
                        Ok(_) => ok_json(vec![(MapKey::Str("key".into()), Value::Str(key.into()))]),
                        Err(e) => err_json(&e),
                    }
                }
                Err(e) => err_json(&e),
            }
        }
        "config.reload" => {
            let cfg = config::reload();
            ok_json(vec![(
                MapKey::Str("path".into()),
                Value::Str(
                    cfg.path
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "(defaults)".into())
                        .into(),
                ),
            )])
        }
        "logs.tail" => {
            let n = json_int_field(req, "lines").unwrap_or(50).clamp(1, 1000) as usize;
            let path = crate::logging::path();
            let Some(path) = path else {
                return ok_json(vec![(MapKey::Str("lines".into()), Value::Str("".into()))]);
            };
            let Ok(text) = std::fs::read_to_string(&path) else {
                return ok_json(vec![(MapKey::Str("lines".into()), Value::Str("".into()))]);
            };
            let lines: Vec<&str> = text.lines().collect();
            let start = lines.len().saturating_sub(n);
            let tail = lines[start..].join("\n");
            ok_json(vec![(MapKey::Str("lines".into()), Value::Str(tail.into()))])
        }
        "modules.list" => {
            use crate::value::collections::seq::make_list;
            let names = crate::stdlib_embed::names();
            let items: Vec<Value> = names.into_iter().map(|s| Value::Str(s.into())).collect();
            let plugins = crate::plugin::registered();
            let plugin_names: Vec<Value> = plugins
                .iter()
                .map(|(n, _)| Value::Str((*n).into()))
                .collect();
            ok_json(vec![
                (MapKey::Str("stdlib".into()), make_list(items)),
                (MapKey::Str("plugins".into()), make_list(plugin_names)),
            ])
        }
        "devices.list" => {
            use crate::value::collections::seq::make_list;
            use crate::value::collections::MapData;
            use crate::value::new_ref;
            let st = Store::open();
            let items: Vec<Value> = st
                .devices
                .iter()
                .map(|d| {
                    Value::Map(new_ref(MapData::from_entries(vec![
                        (MapKey::Str("id".into()), Value::Str(d.id.clone().into())),
                        (
                            MapKey::Str("name".into()),
                            Value::Str(d.name.clone().into()),
                        ),
                        (
                            MapKey::Str("role".into()),
                            Value::Str(d.role.clone().into()),
                        ),
                        (MapKey::Str("created".into()), Value::Int(d.created)),
                        (MapKey::Str("last_seen".into()), Value::Int(d.last_seen)),
                    ])))
                })
                .collect();
            ok_json(vec![(MapKey::Str("devices".into()), make_list(items))])
        }
        "devices.revoke" => {
            let Some(id) = json_str_field(req, "device_id") else {
                return err_json("missing device_id");
            };
            let revoked = {
                let _guard = store::lock();
                let mut st = Store::open();
                let hit = st.revoke(&id);
                if hit {
                    let _ = st.save();
                }
                hit
            };
            if revoked {
                ok_json(vec![(MapKey::Str("revoked".into()), Value::Bool(true))])
            } else {
                err_json("no such device")
            }
        }
        "pair.create" => {
            let role = json_str_field(req, "role").unwrap_or_else(|| "operator".into());
            let name = json_str_field(req, "name").unwrap_or_default();
            let ttl = json_int_field(req, "ttl").unwrap_or_else(|| {
                config::get("agent.pairing_ttl_sec")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(300)
            });
            let code = {
                let _guard = store::lock();
                let mut st = Store::open();
                match st.create_pairing(&role, &name, ttl) {
                    Ok(code) => {
                        let _ = st.save();
                        Ok(code)
                    }
                    Err(e) => Err(e),
                }
            };
            match code {
                Ok(code) => ok_json(vec![
                    (MapKey::Str("code".into()), Value::Str(code.into())),
                    (MapKey::Str("role".into()), Value::Str(role.into())),
                ]),
                Err(e) => err_json(&e),
            }
        }
        "ops.run" => {
            let Some(name) = json_str_field(req, "name") else {
                return err_json("missing name");
            };
            run_op(&name)
        }
        "ops.list" => {
            use crate::value::collections::seq::make_list;
            let names = OP_NAMES.iter().map(|s| Value::Str((*s).into())).collect();
            ok_json(vec![(MapKey::Str("ops".into()), make_list(names))])
        }
        other => err_json(&format!("unknown op '{}'", other)),
    }
}

pub const OP_NAMES: &[&str] = &[
    "ping",
    "reload_config",
    "rotate_log",
    "gc_collect",
    "self_test",
    "dump_diagnostics",
    "restart_runtime",
];

fn run_op(name: &str) -> Value {
    match name {
        "ping" => ok_json(vec![(MapKey::Str("pong".into()), Value::Bool(true))]),
        "reload_config" => {
            let cfg = config::reload();
            ok_json(vec![(
                MapKey::Str("path".into()),
                Value::Str(
                    cfg.path
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "(defaults)".into())
                        .into(),
                ),
            )])
        }
        "rotate_log" => {
            // Actually rotate both logs: rename the current files to `.1`.
            // The old code only wrote a log line saying rotation was
            // requested, so the operator's call did nothing.
            let log_path = crate::logging::rotate_now();
            let audit_path = store::rotate_audit_now();
            crate::logging::record("agent", "rotate_log: rotated spid3r.log and audit.log");
            ok_json(vec![
                (
                    MapKey::Str("log".into()),
                    Value::Str(
                        log_path
                            .map(|p| p.display().to_string())
                            .unwrap_or_default()
                            .into(),
                    ),
                ),
                (
                    MapKey::Str("audit".into()),
                    Value::Str(
                        audit_path
                            .map(|p| p.display().to_string())
                            .unwrap_or_default()
                            .into(),
                    ),
                ),
            ])
        }
        "gc_collect" => {
            crate::value::gc::collect();
            ok_json(vec![(
                MapKey::Str("live_cells".into()),
                Value::Int(crate::value::gc::live_cells() as i64),
            )])
        }
        "self_test" => {
            // A tiny in-process check: crypto roundtrip + ini parse + sha256.
            let key = [1u8; KEY_LEN];
            let nonce = [2u8; NONCE_LEN];
            let sealed = crypto::aead_seal(&key, &nonce, b"self", b"ok");
            let opened = crypto::aead_open(&key, &nonce, b"self", &sealed);
            let (doc, probs) = config::parse_ini("[a]\nk = 1\n");
            let sha = crate::pkg::sha256_hex(b"spid3r");
            let ok = opened.map(|p| p == b"ok").unwrap_or(false)
                && probs.is_empty()
                && doc.get("a", "k") == Some("1")
                && sha.len() == 64;
            if ok {
                ok_json(vec![(MapKey::Str("passed".into()), Value::Bool(true))])
            } else {
                err_json("self_test failed")
            }
        }
        "dump_diagnostics" => {
            let dir = config::state_dir();
            let path = dir.join(format!("diagnostics-{}.txt", now()));
            let text = crate::runtime_info::render_text(&crate::runtime_info::collect_remote(true));
            match std::fs::write(&path, text) {
                Ok(()) => ok_json(vec![(
                    MapKey::Str("path".into()),
                    Value::Str(path.display().to_string().into()),
                )]),
                Err(e) => err_json(&e.to_string()),
            }
        }
        "restart_runtime" => restart_runtime(),
        other => err_json(&format!("unknown operation '{}'", other)),
    }
}

/// Spawn this executable as a new agent and exit. A service manager should
/// supervise the process in production; this is the in-place restart.
///
/// The reply is written by the caller BEFORE the process exits (via
/// EXIT_AFTER_REPLY); the child retries its bind because the parent's
/// listener socket is still held at spawn time.
fn restart_runtime() -> Value {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return err_json(&format!("current_exe: {}", e)),
    };
    match std::process::Command::new(&exe)
        .args(["agent", "start"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => {
            EXIT_AFTER_REPLY.store(true, Ordering::SeqCst);
            ok_json(vec![(MapKey::Str("restarting".into()), Value::Bool(true))])
        }
        Err(e) => err_json(&format!("spawn: {}", e)),
    }
}

fn status_json() -> Value {
    let st = Store::open();
    ok_json(vec![
        (
            MapKey::Str("version".into()),
            Value::Str(crate::VERSION.into()),
        ),
        (
            MapKey::Str("language".into()),
            Value::Str(crate::LANGUAGE_VERSION.into()),
        ),
        (
            MapKey::Str("os".into()),
            Value::Str(std::env::consts::OS.into()),
        ),
        (
            MapKey::Str("arch".into()),
            Value::Str(std::env::consts::ARCH.into()),
        ),
        (
            MapKey::Str("uptime_sec".into()),
            Value::Int(now() - process_start()),
        ),
        (
            MapKey::Str("live_cells".into()),
            Value::Int(crate::value::gc::live_cells() as i64),
        ),
        (
            MapKey::Str("devices".into()),
            Value::Int(st.devices.len() as i64),
        ),
        (
            MapKey::Str("agent_name".into()),
            Value::Str(config::get("agent.name").unwrap_or_default().into()),
        ),
        (
            MapKey::Str("proto".into()),
            Value::Str("spid3r-agent/1".into()),
        ),
    ])
}

fn process_start() -> i64 {
    static START: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *START.get_or_init(now)
}

// -------------------------------------------------------------------- client

/// Pair with a running agent. Returns the stored ClientHost name.
pub fn pair(addr: &str, code: &str, name: &str, role: &str) -> Result<store::ClientHost, String> {
    let _ = role; // role is chosen by the pairing code, not the client
                  // A fresh agent's listener sits in the TCP backlog before its accept
                  // loop is serving, so the very first pairing exchange can fail with a
                  // transport error (connection refused / reset) even though a probe
                  // connect succeeded moments earlier. Retry the whole exchange on
                  // transport errors only — a protocol-level "invalid or expired pairing
                  // code" answer is deterministic and must not be retried.
    let mut transport_failures = 0;
    loop {
        let attempt = pair_once(addr, code, name);
        match attempt {
            Ok(v) => return Ok(v),
            Err(e) => {
                let transient = e.contains("connect ")
                    || e.contains("os error 10061")
                    || e.contains("os error 10054")
                    || e.contains("connection refused")
                    || e.contains("connection reset");
                transport_failures += 1;
                if !transient || transport_failures >= 10 {
                    return Err(e);
                }
                std::thread::sleep(Duration::from_millis(150));
            }
        }
    }
}

fn pair_once(addr: &str, code: &str, name: &str) -> Result<store::ClientHost, String> {
    let mut stream = TcpStream::connect(addr).map_err(|e| format!("connect {}: {}", addr, e))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let req = {
        use crate::value::collections::MapData;
        use crate::value::new_ref;
        Value::Map(new_ref(MapData::from_entries(vec![
            (MapKey::Str("op".into()), Value::Str("pair".into())),
            (MapKey::Str("code".into()), Value::Str(code.into())),
            (MapKey::Str("name".into()), Value::Str(name.into())),
        ])))
    };
    let text = crate::stdlib::json_write_text(&req)?;
    protocol::write_frame(&mut stream, KIND_PAIR, &[0u8; 16], text.as_bytes(), None)
        .map_err(|e| e.to_string())?;
    let zero_lookup = |_: &[u8; 16]| -> Option<[u8; KEY_LEN]> { None };
    let (_, payload) = protocol::read_frame(&mut stream, &zero_lookup)?;
    let resp = crate::stdlib::json_parse_text(&String::from_utf8_lossy(&payload))?;
    if json_bool_field(&resp, "ok") != Some(true) {
        let msg = json_str_field(&resp, "error").unwrap_or_else(|| "pairing failed".into());
        return Err(msg);
    }
    let device_id = json_str_field(&resp, "device_id").ok_or("missing device_id")?;
    let check = json_str_field(&resp, "check").unwrap_or_default();
    let shared_bytes = store::derive_shared_from_code(code)?;
    let shared = crate::pkg::hex_encode(&shared_bytes);
    let expect = crate::pkg::sha256_hex(&shared_bytes)[..8].to_string();
    if !check.is_empty() && check != expect {
        return Err("pairing check mismatch".to_string());
    }
    let host = store::ClientHost {
        name: if name.is_empty() {
            addr.to_string()
        } else {
            name.to_string()
        },
        addr: addr.to_string(),
        device_id,
        shared,
    };
    let mut book = store::ClientBook::open();
    book.hosts.insert(host.name.clone(), host.clone());
    book.save()?;
    Ok(host)
}

/// Call one operation on a paired host.
pub fn call(
    host: &store::ClientHost,
    op: &str,
    extra: Vec<(MapKey, Value)>,
) -> Result<Value, String> {
    let mut stream =
        TcpStream::connect(&host.addr).map_err(|e| format!("connect {}: {}", host.addr, e))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));
    let dev = protocol::device_bytes(&host.device_id).ok_or("bad device id")?;
    let mut key = [0u8; KEY_LEN];
    let raw = crate::pkg::hex_decode(&host.shared).ok_or("bad shared key")?;
    if raw.len() != KEY_LEN {
        return Err("shared key must be 32 bytes".to_string());
    }
    key.copy_from_slice(&raw);

    use crate::value::collections::MapData;
    use crate::value::new_ref;
    let mut entries = vec![(MapKey::Str("op".into()), Value::Str(op.into()))];
    entries.extend(extra);
    let req = Value::Map(new_ref(MapData::from_entries(entries)));
    let text = crate::stdlib::json_write_text(&req)?;
    protocol::write_frame(&mut stream, KIND_DATA, &dev, text.as_bytes(), Some(&key))
        .map_err(|e| e.to_string())?;
    let lookup = |d: &[u8; 16]| -> Option<[u8; KEY_LEN]> {
        if d == &dev {
            Some(key)
        } else {
            None
        }
    };
    let (_, payload) = protocol::read_frame(&mut stream, &lookup)?;
    crate::stdlib::json_parse_text(&String::from_utf8_lossy(&payload))
}

/// Resolve `--host name` or `host:port` from the client book.
pub fn resolve_host(spec: &str) -> Result<store::ClientHost, String> {
    let book = store::ClientBook::open();
    if let Some(h) = book.hosts.get(spec) {
        return Ok(h.clone());
    }
    // Treat as addr if it looks like host:port.
    if spec.contains(':') {
        return Err(format!(
            "no paired device named '{}' (pair first with `spid3r admin pair`)",
            spec
        ));
    }
    Err(format!("unknown host '{}'", spec))
}

// ----------------------------------------------------------------------- CLI

pub fn cmd_agent(args: &[String]) -> i32 {
    let sub = args.first().map(String::as_str).unwrap_or("start");
    match sub {
        "start" => match serve() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error[agent]: {}", e);
                1
            }
        },
        "pair" => {
            let mut role = "operator";
            let mut name = "";
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--role" if i + 1 < args.len() => {
                        role = &args[i + 1];
                        i += 2;
                    }
                    "--name" if i + 1 < args.len() => {
                        name = &args[i + 1];
                        i += 2;
                    }
                    _ => i += 1,
                }
            }
            let mut st = Store::open();
            st.sweep();
            let ttl: i64 = config::get("agent.pairing_ttl_sec")
                .and_then(|v| v.parse().ok())
                .unwrap_or(300);
            match st.create_pairing(role, name, ttl) {
                Ok(code) => {
                    if st.save().is_err() {
                        eprintln!("warning: could not persist pairing code");
                    }
                    println!("pairing code ({}): {}", role, code);
                    println!("expires in {}s", ttl);
                    println!("on the other host:");
                    println!("  spid3r admin pair <host:port> {} --name <name>", code);
                    0
                }
                Err(e) => {
                    eprintln!("error[agent]: {}", e);
                    1
                }
            }
        }
        "status" => {
            let st = Store::open();
            println!("devices: {}", st.devices.len());
            for d in &st.devices {
                println!(
                    "  {}  {:<16}  {:<8}  last_seen={}",
                    d.id, d.name, d.role, d.last_seen
                );
            }
            println!("pending pairings: {}", st.pairings.len());
            0
        }
        "ops" => {
            for n in OP_NAMES {
                println!("{}", n);
            }
            0
        }
        "--help" | "help" => {
            print!(
                "USAGE:\n    spid3r agent start              Listen on [agent] bind:port\n    spid3r agent pair [--role viewer|operator|admin] [--name N]\n    spid3r agent status             List devices and pending pairings\n    spid3r agent ops                List predefined operations\n"
            );
            0
        }
        other => {
            eprintln!("error[agent]: unknown subcommand '{}'", other);
            2
        }
    }
}

pub fn cmd_admin(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("USAGE: spid3r admin <command> [args]");
        eprintln!("  pair <host:port> <code> [--name N]");
        eprintln!("  hosts");
        eprintln!("  --host NAME <op> [options]");
        eprintln!("commands: status info config.get config.set config.reload logs.tail modules.list devices.list devices.revoke pair.create ops.run ops.list ping");
        return 2;
    }
    // `spid3r admin pair ...` has no --host
    if args[0] == "pair" {
        let mut it = args[1..].iter();
        let Some(addr) = it.next() else {
            eprintln!("error: pair needs <host:port> <code>");
            return 2;
        };
        let Some(code) = it.next() else {
            eprintln!("error: pair needs a pairing code");
            return 2;
        };
        let mut name = addr.clone();
        let mut i = 0;
        let rest: Vec<&String> = it.collect();
        while i < rest.len() {
            if rest[i] == "--name" && i + 1 < rest.len() {
                name = rest[i + 1].clone();
                i += 2;
            } else {
                i += 1;
            }
        }
        return match pair(addr, code, &name, "operator") {
            Ok(h) => {
                println!("paired as {} ({})", h.name, h.device_id);
                0
            }
            Err(e) => {
                eprintln!("error[admin]: {}", e);
                1
            }
        };
    }
    if args[0] == "hosts" {
        let book = store::ClientBook::open();
        for h in book.hosts.values() {
            println!("{}  {}  device={}", h.name, h.addr, h.device_id);
        }
        return 0;
    }

    // Remaining commands take --host NAME
    let mut host_spec: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--host" && i + 1 < args.len() {
            host_spec = Some(args[i + 1].clone());
            i += 2;
        } else {
            rest.push(args[i].clone());
            i += 1;
        }
    }
    let Some(spec) = host_spec else {
        eprintln!("error: --host NAME is required (or `spid3r admin pair` first)");
        return 2;
    };
    let host = match resolve_host(&spec) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error[admin]: {}", e);
            return 1;
        }
    };
    let Some(op) = rest.first() else {
        eprintln!("error: missing operation");
        return 2;
    };
    let mut extra: Vec<(MapKey, Value)> = Vec::new();
    let mut j = 1;
    while j < rest.len() {
        match rest[j].as_str() {
            "--key" if j + 1 < rest.len() => {
                extra.push((
                    MapKey::Str("key".into()),
                    Value::Str(rest[j + 1].clone().into()),
                ));
                j += 2;
            }
            "--value" if j + 1 < rest.len() => {
                extra.push((
                    MapKey::Str("value".into()),
                    Value::Str(rest[j + 1].clone().into()),
                ));
                j += 2;
            }
            "--lines" if j + 1 < rest.len() => {
                extra.push((
                    MapKey::Str("lines".into()),
                    Value::Int(rest[j + 1].parse().unwrap_or(50)),
                ));
                j += 2;
            }
            "--device" if j + 1 < rest.len() => {
                extra.push((
                    MapKey::Str("device_id".into()),
                    Value::Str(rest[j + 1].clone().into()),
                ));
                j += 2;
            }
            "--role" if j + 1 < rest.len() => {
                extra.push((
                    MapKey::Str("role".into()),
                    Value::Str(rest[j + 1].clone().into()),
                ));
                j += 2;
            }
            "--name" if j + 1 < rest.len() => {
                extra.push((
                    MapKey::Str("name".into()),
                    Value::Str(rest[j + 1].clone().into()),
                ));
                j += 2;
            }
            "--op-name" if j + 1 < rest.len() => {
                extra.push((
                    MapKey::Str("name".into()),
                    Value::Str(rest[j + 1].clone().into()),
                ));
                j += 2;
            }
            other => {
                eprintln!("error: unknown flag '{}'", other);
                return 2;
            }
        }
    }

    // Map CLI verbs onto wire ops.
    let (wire_op, extra) = match op.as_str() {
        "status" => ("status", extra),
        "info" => ("info", extra),
        "ping" => ("ping", extra),
        "config" | "config.get" => ("config.get", extra),
        "config.set" => ("config.set", extra),
        "reload" | "config.reload" => ("config.reload", extra),
        "logs" | "logs.tail" => ("logs.tail", extra),
        "modules" | "modules.list" => ("modules.list", extra),
        "devices" | "devices.list" => ("devices.list", extra),
        "revoke" | "devices.revoke" => ("devices.revoke", extra),
        "pair.create" => ("pair.create", extra),
        "ops" | "ops.list" => ("ops.list", extra),
        "run" | "ops.run" => ("ops.run", extra),
        other => (other, extra),
    };

    match call(&host, wire_op, extra) {
        Ok(v) => {
            match crate::stdlib::json_write_text(&v) {
                Ok(text) => println!("{}", pretty_json(&text)),
                Err(_) => println!("{:?}", v),
            }
            if json_bool_field(&v, "ok") == Some(false) {
                1
            } else {
                0
            }
        }
        Err(e) => {
            eprintln!("error[admin]: {}", e);
            1
        }
    }
}

/// Light pretty-print of the flat JSON the agent returns.
fn pretty_json(text: &str) -> String {
    // One level of indentation for readability; the agent JSON is shallow.
    let mut out = String::with_capacity(text.len() + 32);
    let mut depth = 0;
    let mut in_str = false;
    let mut esc = false;
    for c in text.chars() {
        if in_str {
            out.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '{' | '[' => {
                out.push(c);
                depth += 1;
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
                out.push(c);
            }
            ',' => {
                out.push(c);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            ':' => {
                out.push_str(": ");
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req_json(body: &str) -> Value {
        crate::stdlib::json_parse_text(body).expect("test json")
    }

    #[test]
    fn remote_config_set_blocks_security_keys() {
        let device = store::Device {
            id: "0".repeat(32),
            name: "t".into(),
            role: "operator".into(),
            shared: String::new(),
            created: 0,
            last_seen: 0,
        };
        // Role escalation: an operator must not be able to rewrite its own
        // role list.
        let resp = dispatch(
            "config.set",
            &req_json(r#"{"op":"config.set","key":"agent.role_operator","value":"*"}"#),
            &device,
        );
        assert_eq!(
            json_str_field(&resp, "error").unwrap(),
            "key 'agent.role_operator' may not be changed remotely (edit spid3r.ini on the host)"
        );

        // Audit suppression.
        let resp = dispatch(
            "config.set",
            &req_json(r#"{"op":"config.set","key":"logging.audit","value":"false"}"#),
            &device,
        );
        assert!(json_str_field(&resp, "error").is_some());

        // Network self-exposure on restart.
        for key in [
            "agent.bind",
            "security.allow_remote",
            "agent.enabled",
            "agent.max_connections",
            // Phase-3 capability-widening keys: the deny lists and the git
            // transport opt-ins are frozen with the rest of [security].
            "security.basedir",
            "security.disable_natives",
            "modules.disable",
            "packages.allow_insecure_git",
            "packages.allow_local_git",
        ] {
            let body = format!(r#"{{"op":"config.set","key":"{}","value":"0.0.0.0"}}"#, key);
            let resp = dispatch("config.set", &req_json(&body), &device);
            assert!(
                json_str_field(&resp, "error").is_some(),
                "{} must be blocked",
                key
            );
        }

        // Benign keys still work is NOT exercised here: dispatch would
        // persist to the developer's real spid3r.ini. The deny-list is
        // prefix-based; agent.name passes the gate by construction.
    }

    fn json_map_field(v: &Value, key: &str) -> Option<Value> {
        let Value::Map(m) = v else { return None };
        let m = m.lock().unwrap();
        let i = m.find(&MapKey::Str(key.into()))?;
        Some(m.entries[i].1.clone())
    }

    #[test]
    fn remote_config_get_dump_all_redacts_policy_keys() {
        let device = store::Device {
            id: "0".repeat(32),
            name: "t".into(),
            role: "operator".into(),
            shared: String::new(),
            created: 0,
            last_seen: 0,
        };
        // Inject non-empty policy values into the live snapshot so the
        // redaction is observable: defaults are empty strings, and
        // redact_value_remote passes empty values through by design. The
        // keyed branch was pinned by the runtime_info unit tests; this test
        // pins the DUMP-ALL branch, which a QA pass caught leaking the raw
        // basedir to a paired remote peer.
        config::set_live("security.basedir", "C:/host/secret/dir").expect("set basedir");
        config::set_live("security.disable_natives", "read_file").expect("set disable_natives");

        let resp = dispatch("config.get", &req_json(r#"{"op":"config.get"}"#), &device);
        let values = json_map_field(&resp, "values").expect("values map");
        let basedir = json_str_field(&values, "security.basedir").expect("basedir entry");
        assert!(
            basedir.starts_with("[redacted sha256:"),
            "dump-all must redact security.basedir, got {basedir:?}"
        );
        let disabled =
            json_str_field(&values, "security.disable_natives").expect("disable_natives entry");
        assert!(
            disabled.starts_with("[redacted sha256:"),
            "dump-all must redact security.disable_natives, got {disabled:?}"
        );
        let registry = json_str_field(&values, "packages.registry").expect("registry entry");
        assert!(
            !registry.starts_with("[redacted"),
            "benign key stays raw on the dump-all branch, got {registry:?}"
        );

        // Restore: the injected policy values must not leak into other tests.
        config::set_live("security.basedir", "").expect("reset basedir");
        config::set_live("security.disable_natives", "").expect("reset disable_natives");
    }

    /// Socket-free pin of the same contract, through the factored helper:
    /// the dump-all map redacts credentials and the policy keys while benign
    /// keys stay raw. Unlike the dispatch test above, this one does not
    /// mutate the process-global config — `fs_policy` bakes its basedir from
    /// the snapshot on first native install, so injecting values here could
    /// leak into other suites' guards.
    #[test]
    fn config_values_map_remote_redacts_policy_keys() {
        let mut values = std::collections::BTreeMap::new();
        values.insert(
            "security.basedir".to_string(),
            "C:/Users/me/data".to_string(),
        );
        values.insert(
            "security.disable_natives".to_string(),
            "read_file,write_file".to_string(),
        );
        values.insert("agent.name".to_string(), "web-1".to_string());
        values.insert("agent.shared".to_string(), "s3cr3t".to_string());

        let dumped = config_values_map_remote(&values);
        let Value::Map(m) = &dumped else {
            panic!(
                "dump-all must return a map, got {:?}",
                crate::value::type_name(&dumped)
            );
        };
        let m = m.lock().unwrap();
        let get = |k: &str| -> String {
            match m.find(&MapKey::Str(k.into())) {
                Some(i) => match &m.entries[i].1 {
                    Value::Str(s) => s.to_string(),
                    other => panic!("entry '{}' is not a string: {:?}", k, other),
                },
                None => panic!("entry '{}' missing from the dump", k),
            }
        };

        // The policy keys: hashed, never the raw host path or deny list.
        for key in ["security.basedir", "security.disable_natives"] {
            assert!(
                get(key).starts_with("[redacted sha256:"),
                "dump-all must redact {}: {}",
                key,
                get(key)
            );
        }
        // Credentials redact through both levels.
        assert!(
            get("agent.shared").starts_with("[redacted sha256:"),
            "{}",
            get("agent.shared")
        );
        // Benign keys stay raw in the dump.
        assert_eq!(get("agent.name"), "web-1");
    }
}
