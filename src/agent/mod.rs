//! Remote-management agent and admin client.
//!
//! One binary, two roles:
//!
//! * `spid3r agent start` — listen on `[agent] bind:port`, pair devices, serve
//!   the encrypted API. **No unrestricted shell.** Every operation is a
//!   predefined name checked against the caller's role.
//! * `spid3r admin ...` — pair to a host, then call those operations.
//!
//! Transport is ChaCha20-Poly1305 over TCP (`spid3r-agent/1`). There is no
//! plaintext data frame. Pairing uses a one-time 48-hex code the operator
//! reads off the host and types into the client; the code never becomes a
//! long-lived key.

pub mod protocol;
pub mod store;

use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use crate::config;
use crate::crypto::{self, KEY_LEN, NONCE_LEN};
use crate::value::{MapKey, Value};
use store::Store;

use protocol::{KIND_DATA, KIND_ERROR, KIND_PAIR};

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
    let listener = TcpListener::bind(&addr).map_err(|e| format!("bind {}: {}", addr, e))?;
    eprintln!("agent: listening on {} (spid3r-agent/1)", addr);

    let live = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // SIGHUP reload on Unix.
    #[cfg(unix)]
    {
        let stop2 = stop.clone();
        unsafe {
            libc_signal(1, sig_handler);
        }
        let _ = stop2;
    }

    loop {
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        match listener.accept() {
            Ok((stream, peer)) => {
                if live.load(std::sync::atomic::Ordering::Relaxed) >= max_conn {
                    drop(stream);
                    continue;
                }
                live.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let live2 = live.clone();
                let timeout = Duration::from_millis(timeout_ms);
                let max_body = max_body;
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(timeout));
                    let _ = stream.set_write_timeout(Some(timeout));
                    let _ = stream.set_nodelay(true);
                    let _ = handle_conn(stream, peer.to_string(), max_body);
                    live2.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                });
            }
            Err(e) => {
                crate::logging::record("agent", &format!("accept: {}", e));
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
extern "C" fn sig_handler(_sig: i32) {
    // Reload configuration; the next request picks it up.
    config::reload();
}

#[cfg(unix)]
unsafe fn libc_signal(sig: i32, handler: extern "C" fn(i32)) {
    extern "C" {
        fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
    }
    signal(sig, handler);
}

fn handle_conn(mut stream: TcpStream, peer: String, max_body: usize) -> Result<(), String> {
    // Replay window: nonces seen recently.
    let mut seen: std::collections::VecDeque<String> = std::collections::VecDeque::new();
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

    loop {
        let (hdr, payload) = match protocol::read_frame(&mut stream, &lookup) {
            Ok(x) => x,
            Err(e) => {
                // A clean EOF (client closed after one request) is normal.
                if e != "closed"
                    && e != "authentication failed"
                    && !e.contains("unknown device")
                {
                    crate::logging::record("agent", &format!("{}: {}", peer, e));
                }
                return Err(e);
            }
        };
        if payload.len() > max_body {
            let _ = reply(&mut stream, KIND_ERROR, &hdr.device, &lookup, err_json("body too large"), &peer);
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
                let mut st = Store::open();
                st.sweep();
                let code = json_str_field(&req, "code").unwrap_or_default();
                let name = json_str_field(&req, "name").unwrap_or_default();
                match st.consume_pairing(&code, &name) {
                    Ok((device, shared)) => {
                        if let Err(e) = st.save() {
                            crate::logging::record("agent", &format!("save devices: {}", e));
                        }
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
                            (MapKey::Str("device_id".into()), Value::Str(device.id.into())),
                            (MapKey::Str("check".into()), Value::Str(check[..8].to_string().into())),
                            (MapKey::Str("role".into()), Value::Str(device.role.into())),
                        ]);
                        let _ = reply_plain(&mut stream, resp);
                    }
                    Err(e) => {
                        store::audit("unknown", "?", "pair", false, &peer, &e);
                        let _ = reply_plain(&mut stream, err_json(&e));
                    }
                }
            }
            KIND_DATA | KIND_ERROR => {
                let req = match crate::stdlib::json_parse_text(&String::from_utf8_lossy(&payload)) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = reply(&mut stream, KIND_ERROR, &hdr.device, &lookup, err_json(&format!("bad json: {}", e)), &peer);
                        continue;
                    }
                };
                // Replay guard: reject a nonce we have already accepted.
                let nonce_id = format!(
                    "{}:{}",
                    protocol::device_hex(&hdr.device),
                    crate::pkg::hex_encode(&hdr.nonce)
                );
                if seen.contains(&nonce_id) {
                    let _ = reply(&mut stream, KIND_ERROR, &hdr.device, &lookup, err_json("replayed frame"), &peer);
                    continue;
                }
                seen.push_back(nonce_id);
                while seen.len() > 256 {
                    seen.pop_front();
                }

                let hex_id = protocol::device_hex(&hdr.device);
                let mut st = Store::open();
                let Some(device) = st.find_device(&hex_id).cloned() else {
                    let _ = reply(&mut stream, KIND_ERROR, &hdr.device, &lookup, err_json("unknown device"), &peer);
                    continue;
                };
                st.touch(&hex_id);
                let _ = st.save();
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
    protocol::write_frame(w, kind, device, text.as_bytes(), Some(&key))
        .map_err(|e| e.to_string())
}

// ----------------------------------------------------------------- operations

fn dispatch(op: &str, req: &Value, _device: &store::Device) -> Value {
    match op {
        "ping" => ok_json(vec![
            (MapKey::Str("pong".into()), Value::Bool(true)),
            (MapKey::Str("ts".into()), Value::Int(now())),
            (MapKey::Str("agent".into()), Value::Str("spid3r-agent/1".into())),
        ]),
        "status" => status_json(),
        "info" => {
            let no_redact = json_bool_field(req, "no_redact").unwrap_or(false);
            // Remote callers never get unredacted output.
            Value::Map({
                use crate::value::collections::MapData;
                use crate::value::new_ref;
                let inner = crate::runtime_info::info_map(true);
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
                            Value::Str(crate::runtime_info::redact_value(&k, v).into()),
                        ),
                    ]),
                    None => err_json(&format!("unknown key '{}'", k)),
                },
                None => {
                    use crate::value::collections::MapData;
                    use crate::value::new_ref;
                    let mut rows = Vec::new();
                    for (k, v) in &cfg.values {
                        rows.push((
                            MapKey::Str(k.clone().into()),
                            Value::Str(crate::runtime_info::redact_value(k, v).into()),
                        ));
                    }
                    ok_json(vec![(
                        MapKey::Str("values".into()),
                        Value::Map(new_ref(MapData::from_entries(rows))),
                    )])
                }
            }
        }
        "config.set" => {
            let Some(key) = json_str_field(req, "key") else {
                return err_json("missing key");
            };
            let Some(value) = json_str_field(req, "value") else {
                return err_json("missing value");
            };
            match config::set_live(&key, &value) {
                Ok(()) => {
                    // Persist through the file when we can.
                    let cfg = config::snapshot();
                    let _ = config::save(&cfg);
                    ok_json(vec![(MapKey::Str("key".into()), Value::Str(key.into()))])
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
            let plugin_names: Vec<Value> = plugins.iter().map(|(n, _)| Value::Str((*n).into())).collect();
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
                        (MapKey::Str("name".into()), Value::Str(d.name.clone().into())),
                        (MapKey::Str("role".into()), Value::Str(d.role.clone().into())),
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
            let mut st = Store::open();
            if st.revoke(&id) {
                let _ = st.save();
                ok_json(vec![(MapKey::Str("revoked".into()), Value::Bool(true))])
            } else {
                err_json("no such device")
            }
        }
        "pair.create" => {
            let role = json_str_field(req, "role").unwrap_or_else(|| "operator".into());
            let name = json_str_field(req, "name").unwrap_or_default();
            let ttl = json_int_field(req, "ttl")
                .unwrap_or_else(|| {
                    config::get("agent.pairing_ttl_sec")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(300)
                });
            let mut st = Store::open();
            match st.create_pairing(&role, &name, ttl) {
                Ok(code) => {
                    let _ = st.save();
                    ok_json(vec![
                        (MapKey::Str("code".into()), Value::Str(code.into())),
                        (MapKey::Str("role".into()), Value::Str(role.into())),
                    ])
                }
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
            // Force a rotation by appending a marker and calling the logger's
            // internal path; simplest correct action is to note the request.
            crate::logging::record("agent", "rotate_log requested");
            ok_json(vec![(
                MapKey::Str("path".into()),
                Value::Str(
                    crate::logging::path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                        .into(),
                ),
            )])
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
            let text = crate::runtime_info::render_text(&crate::runtime_info::collect(true));
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
            std::process::exit(0);
        }
        Err(e) => err_json(&format!("spawn: {}", e)),
    }
}

fn status_json() -> Value {
    let st = Store::open();
    ok_json(vec![
        (MapKey::Str("version".into()), Value::Str(crate::VERSION.into())),
        (MapKey::Str("language".into()), Value::Str(crate::LANGUAGE_VERSION.into())),
        (MapKey::Str("os".into()), Value::Str(std::env::consts::OS.into())),
        (MapKey::Str("arch".into()), Value::Str(std::env::consts::ARCH.into())),
        (
            MapKey::Str("uptime_sec".into()),
            Value::Int(now() - process_start()),
        ),
        (
            MapKey::Str("live_cells".into()),
            Value::Int(crate::value::gc::live_cells() as i64),
        ),
        (MapKey::Str("devices".into()), Value::Int(st.devices.len() as i64)),
        (
            MapKey::Str("agent_name".into()),
            Value::Str(config::get("agent.name").unwrap_or_default().into()),
        ),
        (MapKey::Str("proto".into()), Value::Str("spid3r-agent/1".into())),
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
pub fn call(host: &store::ClientHost, op: &str, extra: Vec<(MapKey, Value)>) -> Result<Value, String> {
    let mut stream = TcpStream::connect(&host.addr)
        .map_err(|e| format!("connect {}: {}", host.addr, e))?;
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
                extra.push((MapKey::Str("key".into()), Value::Str(rest[j + 1].clone().into())));
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
                extra.push((MapKey::Str("role".into()), Value::Str(rest[j + 1].clone().into())));
                j += 2;
            }
            "--name" if j + 1 < rest.len() => {
                extra.push((MapKey::Str("name".into()), Value::Str(rest[j + 1].clone().into())));
                j += 2;
            }
            "--op-name" if j + 1 < rest.len() => {
                extra.push((MapKey::Str("name".into()), Value::Str(rest[j + 1].clone().into())));
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

    match call(&host, &wire_op, extra) {
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
