//! Persistent agent state: paired devices, one-time pairing codes, and the
//! JSON file they live in.
//!
//! The file is `devices.json` under the state directory. Shared keys are
//! stored as hex; the file is `0600` on Unix. A missing or corrupt file is
//! an empty store, not a crash — the operator re-pairs.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::config;
use crate::crypto;

#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub role: String,
    /// Hex-encoded 32-byte shared key.
    pub shared: String,
    pub created: i64,
    pub last_seen: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pairing {
    pub code: String,
    pub role: String,
    pub name: String,
    pub expires: i64,
    pub used: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Store {
    pub devices: Vec<Device>,
    pub pairings: Vec<Pairing>,
    pub path: PathBuf,
}

fn now() -> i64 {
    crate::stdlib::datetime::unix_time()
}

impl Store {
    pub fn open() -> Store {
        let dir = config::state_dir();
        let path = dir.join("devices.json");
        let mut st = Store {
            devices: Vec::new(),
            pairings: Vec::new(),
            path,
        };
        if let Ok(text) = std::fs::read_to_string(&st.path) {
            if let Ok(v) = crate::stdlib::json_parse_text(&text) {
                st.from_json(&v);
            }
        }
        st
    }

    pub fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
        }
        let text = crate::stdlib::json_write_text(&self.to_json())?;
        std::fs::write(&self.path, text)
            .map_err(|e| format!("write {}: {}", self.path.display(), e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    fn to_json(&self) -> crate::value::Value {
        use crate::value::{new_ref, MapKey, Value};
        use crate::value::collections::MapData;
        let devices: Vec<_> = self
            .devices
            .iter()
            .map(|d| {
                Value::Map(new_ref(MapData::from_entries(vec![
                    (MapKey::Str("id".into()), Value::Str(d.id.clone().into())),
                    (MapKey::Str("name".into()), Value::Str(d.name.clone().into())),
                    (MapKey::Str("role".into()), Value::Str(d.role.clone().into())),
                    (MapKey::Str("shared".into()), Value::Str(d.shared.clone().into())),
                    (MapKey::Str("created".into()), Value::Int(d.created)),
                    (MapKey::Str("last_seen".into()), Value::Int(d.last_seen)),
                ])))
            })
            .collect();
        let pairings: Vec<_> = self
            .pairings
            .iter()
            .map(|p| {
                Value::Map(new_ref(MapData::from_entries(vec![
                    (MapKey::Str("code".into()), Value::Str(p.code.clone().into())),
                    (MapKey::Str("role".into()), Value::Str(p.role.clone().into())),
                    (MapKey::Str("name".into()), Value::Str(p.name.clone().into())),
                    (MapKey::Str("expires".into()), Value::Int(p.expires)),
                    (MapKey::Str("used".into()), Value::Bool(p.used)),
                ])))
            })
            .collect();
        use crate::value::collections::seq::make_list;
        Value::Map(new_ref(MapData::from_entries(vec![
            (MapKey::Str("devices".into()), make_list(devices)),
            (MapKey::Str("pairings".into()), make_list(pairings)),
        ])))
    }

    fn from_json(&mut self, v: &crate::value::Value) {
        use crate::value::Value;
        let Value::Map(m) = v else { return };
        let m = m.lock().unwrap();
        if let Some(crate::value::Value::List(list)) = m
            .find(&crate::value::MapKey::Str("devices".into()))
            .map(|i| &m.entries[i].1)
        {
            for item in &list.lock().unwrap().items {
                if let Some(d) = device_from_json(item) {
                    self.devices.push(d);
                }
            }
        }
        if let Some(crate::value::Value::List(list)) = m
            .find(&crate::value::MapKey::Str("pairings".into()))
            .map(|i| &m.entries[i].1)
        {
            for item in &list.lock().unwrap().items {
                if let Some(p) = pairing_from_json(item) {
                    self.pairings.push(p);
                }
            }
        }
    }

    pub fn find_device(&self, id: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == id)
    }

    pub fn find_device_mut(&mut self, id: &str) -> Option<&mut Device> {
        self.devices.iter_mut().find(|d| d.id == id)
    }

    pub fn role_of(&self, id: &str) -> Option<String> {
        self.find_device(id).map(|d| d.role.clone())
    }

    /// Create a one-time pairing code. Returns the code (hex).
    pub fn create_pairing(&mut self, role: &str, name: &str, ttl: i64) -> Result<String, String> {
        if !matches!(role, "viewer" | "operator" | "admin") {
            return Err(format!("unknown role '{}' (viewer|operator|admin)", role));
        }
        let code = crypto::random_hex(24)?; // 48 hex chars
        self.pairings.push(Pairing {
            code: code.clone(),
            role: role.to_string(),
            name: name.to_string(),
            expires: now() + ttl,
            used: false,
        });
        self.pairings
            .retain(|p| !p.used && p.expires > now());
        Ok(code)
    }

    /// Consume a pairing code and register the device. Returns (device, shared).
    pub fn consume_pairing(
        &mut self,
        code: &str,
        client_name: &str,
    ) -> Result<(Device, [u8; crypto::SHARED_KEY_LEN]), String> {
        let t = now();
        let idx = self
            .pairings
            .iter()
            .position(|p| !p.used && p.expires > t && crypto::ct_eq(p.code.as_bytes(), code.as_bytes()));
        let Some(idx) = idx else {
            return Err("invalid or expired pairing code".to_string());
        };
        let p = self.pairings[idx].clone();
        self.pairings[idx].used = true;

        let id = crypto::random_hex(16)?;
        let shared_bytes = derive_shared_from_code(&code)?;
        let device = Device {
            id: id.clone(),
            name: if client_name.is_empty() {
                if p.name.is_empty() {
                    "unnamed-device".to_string()
                } else {
                    p.name.clone()
                }
            } else {
                client_name.to_string()
            },
            role: p.role,
            shared: crate::pkg::hex_encode(&shared_bytes),
            created: t,
            last_seen: t,
        };
        self.devices.push(device.clone());
        Ok((device, shared_bytes))
    }

    pub fn revoke(&mut self, id: &str) -> bool {
        let before = self.devices.len();
        self.devices.retain(|d| d.id != id);
        self.devices.len() != before
    }

    pub fn touch(&mut self, id: &str) {
        if let Some(d) = self.find_device_mut(id) {
            d.last_seen = now();
        }
    }

    /// Drop expired pairings. Call on load and after use.
    pub fn sweep(&mut self) {
        let t = now();
        self.pairings.retain(|p| !p.used && p.expires > t);
    }
}

/// Shared key derived from the pairing code. Both sides run this after they
/// have mixed in the two ephemeral nonces (see `protocol::pair_shared`).
pub fn derive_shared_from_code(code: &str) -> Result<[u8; crypto::SHARED_KEY_LEN], String> {
    let mut buf = [0u8; crypto::SHARED_KEY_LEN];
    // The code itself is already 24 random bytes as hex; hash it to 32.
    let h = crate::pkg::sha256_bytes(code.as_bytes());
    buf.copy_from_slice(&h);
    Ok(buf)
}

fn device_from_json(v: &crate::value::Value) -> Option<Device> {
    use crate::value::{MapKey, Value};
    let Value::Map(m) = v else { return None };
    let m = m.lock().unwrap();
    let get = |k: &str| -> Option<String> {
        match m.find(&MapKey::Str(k.into())) {
            Some(i) => match &m.entries[i].1 {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            },
            None => None,
        }
    };
    let geti = |k: &str| -> Option<i64> {
        match m.find(&MapKey::Str(k.into())) {
            Some(i) => match &m.entries[i].1 {
                Value::Int(n) => Some(*n),
                Value::Float(f) => Some(*f as i64),
                _ => None,
            },
            None => None,
        }
    };
    Some(Device {
        id: get("id")?,
        name: get("name").unwrap_or_default(),
        role: get("role").unwrap_or_else(|| "viewer".into()),
        shared: get("shared")?,
        created: geti("created").unwrap_or(0),
        last_seen: geti("last_seen").unwrap_or(0),
    })
}

fn pairing_from_json(v: &crate::value::Value) -> Option<Pairing> {
    use crate::value::{MapKey, Value};
    let Value::Map(m) = v else { return None };
    let m = m.lock().unwrap();
    let get = |k: &str| -> Option<String> {
        match m.find(&MapKey::Str(k.into())) {
            Some(i) => match &m.entries[i].1 {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            },
            None => None,
        }
    };
    let geti = |k: &str| -> Option<i64> {
        match m.find(&MapKey::Str(k.into())) {
            Some(i) => match &m.entries[i].1 {
                Value::Int(n) => Some(*n),
                _ => None,
            },
            None => None,
        }
    };
    let getb = |k: &str| -> Option<bool> {
        match m.find(&MapKey::Str(k.into())) {
            Some(i) => match &m.entries[i].1 {
                Value::Bool(b) => Some(*b),
                _ => None,
            },
            None => None,
        }
    };
    Some(Pairing {
        code: get("code")?,
        role: get("role").unwrap_or_else(|| "viewer".into()),
        name: get("name").unwrap_or_default(),
        expires: geti("expires").unwrap_or(0),
        used: getb("used").unwrap_or(false),
    })
}

/// Role → allowed op list from configuration.
pub fn allowed_ops(role: &str) -> Option<Vec<String>> {
    let key = match role {
        "viewer" => "agent.role_viewer",
        "operator" => "agent.role_operator",
        "admin" => "agent.role_admin",
        _ => return None,
    };
    let raw = config::get(key).unwrap_or_default();
    if raw.trim() == "*" {
        return Some(vec!["*".to_string()]);
    }
    Some(
        raw.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    )
}

pub fn op_allowed(role: &str, op: &str) -> bool {
    let Some(ops) = allowed_ops(role) else {
        return false;
    };
    if ops.iter().any(|o| o == "*") {
        return true;
    }
    ops.iter().any(|o| o == op)
}

/// Append one audit record. Never fails the caller.
pub fn audit(device: &str, role: &str, op: &str, ok: bool, peer: &str, detail: &str) {
    if !config::get("logging.audit")
        .map(|v| crate::config::schema::parse_bool(&v))
        .unwrap_or(true)
    {
        return;
    }
    let dir = config::state_dir();
    let path = dir.join("audit.log");
    let line = format!(
        "{{\"ts\":{},\"device\":\"{}\",\"role\":\"{}\",\"op\":\"{}\",\"ok\":{},\"peer\":\"{}\",\"detail\":\"{}\"}}\n",
        now(),
        json_escape(device),
        json_escape(role),
        json_escape(op),
        ok,
        json_escape(peer),
        json_escape(detail),
    );
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Client-side known hosts (`clients.json`).
#[derive(Debug, Clone, Default)]
pub struct ClientBook {
    pub hosts: BTreeMap<String, ClientHost>,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ClientHost {
    pub name: String,
    pub addr: String,
    pub device_id: String,
    pub shared: String,
}

impl ClientBook {
    pub fn open() -> ClientBook {
        let path = config::state_dir().join("clients.json");
        let mut book = ClientBook {
            hosts: BTreeMap::new(),
            path,
        };
        if let Ok(text) = std::fs::read_to_string(&book.path) {
            if let Ok(crate::value::Value::Map(m)) = crate::stdlib::json_parse_text(&text) {
                let m = m.lock().unwrap();
                if let Some(crate::value::Value::List(list)) = m
                    .find(&crate::value::MapKey::Str("hosts".into()))
                    .map(|i| &m.entries[i].1)
                {
                    for item in &list.lock().unwrap().items {
                        if let Some(h) = host_from_json(item) {
                            book.hosts.insert(h.name.clone(), h);
                        }
                    }
                }
            }
        }
        book
    }

    pub fn save(&self) -> Result<(), String> {
        use crate::value::collections::seq::make_list;
        use crate::value::{new_ref, MapKey, Value};
        use crate::value::collections::MapData;
        let items: Vec<_> = self
            .hosts
            .values()
            .map(|h| {
                Value::Map(new_ref(MapData::from_entries(vec![
                    (MapKey::Str("name".into()), Value::Str(h.name.clone().into())),
                    (MapKey::Str("addr".into()), Value::Str(h.addr.clone().into())),
                    (MapKey::Str("device_id".into()), Value::Str(h.device_id.clone().into())),
                    (MapKey::Str("shared".into()), Value::Str(h.shared.clone().into())),
                ])))
            })
            .collect();
        let v = Value::Map(new_ref(MapData::from_entries(vec![(
            MapKey::Str("hosts".into()),
            make_list(items),
        )])));
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
        }
        let text = crate::stdlib::json_write_text(&v)?;
        std::fs::write(&self.path, text)
            .map_err(|e| format!("write {}: {}", self.path.display(), e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

fn host_from_json(v: &crate::value::Value) -> Option<ClientHost> {
    use crate::value::{MapKey, Value};
    let Value::Map(m) = v else { return None };
    let m = m.lock().unwrap();
    let get = |k: &str| -> Option<String> {
        match m.find(&MapKey::Str(k.into())) {
            Some(i) => match &m.entries[i].1 {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            },
            None => None,
        }
    };
    Some(ClientHost {
        name: get("name")?,
        addr: get("addr")?,
        device_id: get("device_id")?,
        shared: get("shared")?,
    })
}
