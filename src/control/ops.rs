//! Shared control operations, JSON-in / JSON-out.
//!
//! Both surfaces — the `spid3r control` CLI and the `spid3r-control` desktop
//! panel — go through [`dispatch`], so a control action behaves identically
//! no matter which surface invokes it. Every op returns a FLAT JSON object:
//! one level, all values strings (arrays are `"\n"`-joined, rows are
//! `"|"`-separated). That keeps the dependency-free JSON handling honest —
//! the crate builds without serde, so the panel must not smuggle one in.
//!
//! Security posture: ops that mutate state (config_set, devices_revoke,
//! agent_start/stop) append to the agent audit log, so a panel action is
//! auditable exactly like a remote agent action. Config values are redacted
//! through `runtime_info::redact_value` before they ever reach the UI.

use crate::agent::store::{self, Store};
use crate::config;
use std::io::Read as _;
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

/// Dispatch one control operation. `params` is a flat JSON object string.
pub fn dispatch(op: &str, params: &str) -> Result<String, String> {
    match op {
        "status" => op_status(params),
        "ini_info" => op_ini_info(),
        "ini_init" => op_ini_init(params),
        "config_list" => op_config_list(),
        "config_get" => op_config_get(params),
        "config_set" => op_config_set(params),
        "config_validate" => op_config_validate(),
        "agent_status" => op_agent_status(),
        "agent_start" => op_agent_start(),
        "agent_stop" => op_agent_stop(),
        "devices_list" => op_devices_list(),
        "devices_revoke" => op_devices_revoke(params),
        "audit_tail" => op_audit_tail(params),
        other => Err(format!("unknown control op '{}'", other)),
    }
}

// ------------------------------------------------------------ json helpers --

/// Escape a Rust string into a JSON string literal (with quotes).
pub fn jstr(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
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
    out.push('"');
    out
}

/// Extract a top-level string field from a flat JSON object (the format
/// `dispatch` produces). Returns the unescaped value.
pub fn field(json: &str, name: &str) -> Option<String> {
    let needle = format!("\"{}\"", name);
    let mut from = 0;
    loop {
        let pos = json[from..].find(&needle)? + from;
        // The match must be a key: the preceding non-space char is '{' or ','.
        let before = json[..pos].trim_end();
        if !before.ends_with('{') && !before.ends_with(',') {
            from = pos + needle.len();
            continue;
        }
        let after = &json[pos + needle.len()..];
        let after = after.trim_start();
        let after = after.strip_prefix(':')?.trim_start();
        let rest = after.strip_prefix('"')?;
        let mut out = String::new();
        let mut chars = rest.chars();
        loop {
            let c = chars.next()?;
            match c {
                '"' => return Some(out),
                '\\' => match chars.next()? {
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let hex: String = chars.by_ref().take(4).collect();
                        let code = u32::from_str_radix(&hex, 16).ok()?;
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                    other => out.push(other),
                },
                c => out.push(c),
            }
        }
    }
}

// ------------------------------------------------------------ agent probe --

fn agent_addr() -> String {
    let bind = config::get("agent.bind").unwrap_or_else(|| "127.0.0.1".into());
    let port = config::get("agent.port").unwrap_or_else(|| "9527".into());
    format!("{}:{}", bind, port)
}

/// Best-effort liveness probe: can we open a TCP connection to the agent?
fn probe(addr: &str) -> bool {
    TcpStream::connect_timeout(
        &addr
            .parse()
            .unwrap_or(std::net::SocketAddr::from(([127, 0, 0, 1], 0))),
        Duration::from_millis(300),
    )
    .is_ok()
}

fn pid_file() -> PathBuf {
    config::state_dir().join("agent.pid")
}

fn read_pid_file() -> Option<(u32, String)> {
    let text = std::fs::read_to_string(pid_file()).ok()?;
    let mut pid = None;
    let mut port = String::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("pid=") {
            pid = v.parse().ok();
        }
        if let Some(v) = line.strip_prefix("port=") {
            port = v.to_string();
        }
    }
    pid.map(|p| (p, port))
}

fn write_pid_file(pid: u32) -> Result<(), String> {
    let addr = agent_addr();
    let port = addr.rsplit(':').next().unwrap_or("?");
    std::fs::write(
        pid_file(),
        format!(
            "pid={}\nport={}\nstarted={}\n",
            pid,
            port,
            crate::stdlib::datetime::unix_time()
        ),
    )
    .map_err(|e| format!("write agent.pid: {}", e))
}

/// Is `pid` plausibly one of our own agent processes? Verified with the
/// platform process list BEFORE any kill: the PID file is a hint, not
/// authority, and a recycled PID must never be signalled blindly. The match
/// is exact on the image name ("spid3r" / "spid3r-control" / "spid3r.exe"),
/// not a substring — "notspid3r.exe" must not pass.
fn pid_is_spid3r(pid: u32) -> bool {
    let name = process_image_name(pid).to_lowercase();
    name == "spid3r"
        || name == "spid3r.exe"
        || name == "spid3r-control"
        || name == "spid3r-control.exe"
}

fn process_image_name(pid: u32) -> String {
    #[cfg(target_os = "windows")]
    {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/FO", "CSV", "/NH"])
            .output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .split(',')
                .next()
                .unwrap_or("")
                .trim_matches('"')
                .to_string(),
            Err(_) => String::new(),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let out = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            Err(_) => String::new(),
        }
    }
}

fn kill_pid(pid: u32) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let status = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .status();
    #[cfg(not(target_os = "windows"))]
    let status = std::process::Command::new("kill")
        .arg(pid.to_string())
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("kill exited with {}", s)),
        Err(e) => Err(format!("kill: {}", e)),
    }
}

/// Spawn `agent start` detached from this process so it outlives the CLI /
/// panel that launched it. On Windows the child gets no console (a panel
/// launch must not flash a terminal); on Unix the child is orphaned on
/// purpose — the parent exits and init reaps it.
fn spawn_agent_detached() -> Result<u32, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {}", e))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["agent", "start"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
        cmd.creation_flags(0x0000_0008 | 0x0000_0200 | 0x0800_0000);
    }
    let child = cmd.spawn().map_err(|e| format!("spawn agent: {}", e))?;
    Ok(child.id())
}

// -------------------------------------------------------------------- ops --

fn op_status(_params: &str) -> Result<String, String> {
    let ini_active = config::candidate_paths().into_iter().find(|p| p.is_file());
    let cfg = config::load();
    let errors = config::validate(&cfg);
    let addr = agent_addr();
    let listening = probe(&addr);
    let pid_info = read_pid_file();
    let (devices, pairings) = {
        let st = Store::open();
        (st.devices.len(), st.pairings.len())
    };
    let audit_path = config::state_dir().join("audit.log");
    let audit_lines = std::fs::read_to_string(&audit_path)
        .map(|t| t.lines().count())
        .unwrap_or(0);
    Ok(format!(
        "{{\"version\":{},\"language\":{},\
         \"ini_path\":{},\"ini_exists\":{},\"ini_valid\":{},\"ini_errors\":{},\
         \"agent_addr\":{},\"agent_enabled\":{},\"agent_listening\":{},\"agent_pid\":{},\
         \"devices\":\"{}\",\"pairings\":\"{}\",\
         \"audit_path\":{},\"audit_lines\":\"{}\",\"state_dir\":{}}}",
        jstr(crate::VERSION),
        jstr(crate::LANGUAGE_VERSION),
        jstr(
            &ini_active
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        ),
        jstr(if ini_active.is_some() {
            "true"
        } else {
            "false"
        }),
        jstr(if errors.is_empty() { "true" } else { "false" }),
        jstr(&errors.join("\n")),
        jstr(&addr),
        jstr(&config::get("agent.enabled").unwrap_or_default()),
        jstr(if listening { "true" } else { "false" }),
        jstr(&pid_info.map(|(p, _)| p.to_string()).unwrap_or_default()),
        devices,
        pairings,
        jstr(&audit_path.display().to_string()),
        audit_lines,
        jstr(&config::state_dir().display().to_string()),
    ))
}

fn op_ini_info() -> Result<String, String> {
    let candidates: Vec<String> = config::candidate_paths()
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    let active = config::load().path;
    Ok(format!(
        "{{\"active\":{},\"candidates\":{}}}",
        jstr(
            &active
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        ),
        jstr(&candidates.join("\n")),
    ))
}

fn op_ini_init(params: &str) -> Result<String, String> {
    let force = field(params, "force").as_deref() == Some("true");
    if let Some(existing) = config::candidate_paths().into_iter().find(|p| p.is_file()) {
        if !force {
            return Ok(format!(
                "{{\"written\":\"false\",\"path\":{},\"message\":{}}}",
                jstr(&existing.display().to_string()),
                jstr(&format!(
                    "ini already exists: {} (use --force to overwrite)",
                    existing.display()
                )),
            ));
        }
    }
    let path = config::write_default_ini(None)?;
    store::audit(
        "control",
        "admin",
        "ini.init",
        true,
        "local",
        &path.display().to_string(),
    );
    Ok(format!(
        "{{\"written\":\"true\",\"path\":{},\"message\":{}}}",
        jstr(&path.display().to_string()),
        jstr(&format!("default ini written: {}", path.display())),
    ))
}

fn op_config_list() -> Result<String, String> {
    let cfg = config::snapshot();
    let mut rows = Vec::new();
    for k in crate::config::schema::KEYS {
        let dotted = k.dotted();
        let value = cfg
            .get(&dotted)
            .map(str::to_string)
            .unwrap_or_else(|| k.default.to_string());
        let redacted = crate::runtime_info::redact_value(&dotted, &value);
        rows.push(format!(
            "{}|{:?}|{}|{}",
            dotted,
            k.kind,
            redacted.replace('|', "/").replace('\n', " "),
            k.default.replace('|', "/").replace('\n', " "),
        ));
    }
    Ok(format!(
        "{{\"count\":\"{}\",\"keys\":{}}}",
        rows.len(),
        jstr(&rows.join("\n")),
    ))
}

fn op_config_get(params: &str) -> Result<String, String> {
    let Some(key) = field(params, "key") else {
        return Err("config_get needs \"key\"".into());
    };
    match config::get(&key) {
        Some(v) => Ok(format!(
            "{{\"key\":{},\"value\":{}}}",
            jstr(&key),
            jstr(&crate::runtime_info::redact_value(&key, &v)),
        )),
        None => Err(format!("unknown config key '{}'", key)),
    }
}

fn op_config_set(params: &str) -> Result<String, String> {
    let Some(key) = field(params, "key") else {
        return Err("config_set needs \"key\"".into());
    };
    let Some(value) = field(params, "value") else {
        return Err("config_set needs \"value\"".into());
    };
    if crate::config::schema::known_key(&key).is_none() {
        return Err(format!("unknown config key '{}'", key));
    }
    config::set_live(&key, &value)?;
    let cfg = config::snapshot();
    let errors = config::validate(&cfg);
    if !errors.is_empty() {
        // The live snapshot is now invalid: discard it by re-reading the file.
        config::reload();
        return Err(errors.join("; "));
    }
    let path = config::save(&config::snapshot())?;
    store::audit(
        "control",
        "admin",
        "config.set",
        true,
        "local",
        &format!(
            "{}={}",
            key,
            crate::runtime_info::redact_value(&key, &value)
        ),
    );
    Ok(format!(
        "{{\"message\":{},\"path\":{}}}",
        jstr(&format!("{} set; saved to {}", key, path.display())),
        jstr(&path.display().to_string()),
    ))
}

fn op_config_validate() -> Result<String, String> {
    let errors = config::validate(&config::snapshot());
    Ok(format!(
        "{{\"valid\":\"{}\",\"errors\":{}}}",
        if errors.is_empty() { "true" } else { "false" },
        jstr(&errors.join("\n")),
    ))
}

fn op_agent_status() -> Result<String, String> {
    let addr = agent_addr();
    let listening = probe(&addr);
    let pid = read_pid_file()
        .map(|(p, _)| p.to_string())
        .unwrap_or_default();
    let msg = if listening {
        format!(
            "agent listening on {}{}",
            addr,
            if pid.is_empty() {
                String::new()
            } else {
                format!(" (pid {})", pid)
            }
        )
    } else {
        format!("agent not listening on {}", addr)
    };
    Ok(format!(
        "{{\"message\":{},\"listening\":\"{}\",\"pid\":{},\"addr\":{}}}",
        jstr(&msg),
        if listening { "true" } else { "false" },
        jstr(&pid),
        jstr(&addr),
    ))
}

fn op_agent_start() -> Result<String, String> {
    let addr = agent_addr();
    if probe(&addr) {
        return Ok(format!(
            "{{\"message\":{},\"listening\":\"true\",\"pid\":\"\"}}",
            jstr(&format!("agent already listening on {}", addr)),
        ));
    }
    let pid = spawn_agent_detached()?;
    write_pid_file(pid)?;
    // Give the listener a moment to bind, then verify — a spawn that died
    // instantly must not be reported as success.
    std::thread::sleep(Duration::from_millis(600));
    let listening = probe(&addr);
    store::audit(
        "control",
        "admin",
        "agent.start",
        listening,
        "local",
        &format!("pid={} addr={}", pid, addr),
    );
    if listening {
        Ok(format!(
            "{{\"message\":{},\"listening\":\"true\",\"pid\":\"{}\"}}",
            jstr(&format!("agent started (pid {}) on {}", pid, addr)),
            pid,
        ))
    } else {
        Err(format!(
            "agent process {} did not bind {} within 600 ms — check `spid3r control agent status` \
             and [agent] in spid3r.ini (agent.enabled must be true)",
            pid, addr
        ))
    }
}

fn op_agent_stop() -> Result<String, String> {
    let Some((pid, _port)) = read_pid_file() else {
        return Err(
            "no agent.pid — this tool can only stop an agent it started \
             (a foreground `spid3r agent start` is stopped with Ctrl+C)"
                .into(),
        );
    };
    if !pid_is_spid3r(pid) {
        let _ = std::fs::remove_file(pid_file());
        return Err(format!(
            "pid {} is not a spid3r process (stale agent.pid removed; refusing to signal it)",
            pid
        ));
    }
    kill_pid(pid)?;
    let addr = agent_addr();
    std::thread::sleep(Duration::from_millis(400));
    let listening = probe(&addr);
    let _ = std::fs::remove_file(pid_file());
    store::audit(
        "control",
        "admin",
        "agent.stop",
        !listening,
        "local",
        &format!("pid={}", pid),
    );
    if listening {
        Err(format!("signalled pid {} but {} still answers", pid, addr))
    } else {
        Ok(format!(
            "{{\"message\":{},\"listening\":\"false\",\"pid\":\"\"}}",
            jstr(&format!("agent stopped (pid {})", pid)),
        ))
    }
}

fn op_devices_list() -> Result<String, String> {
    let mut st = Store::open();
    st.sweep();
    let mut rows = Vec::new();
    for d in &st.devices {
        rows.push(format!(
            "{}|{}|{}|{}",
            d.id,
            d.name.replace('|', "/"),
            d.role,
            d.last_seen,
        ));
    }
    let pairings = st.pairings.iter().filter(|p| !p.used).count();
    Ok(format!(
        "{{\"count\":\"{}\",\"pending_pairings\":\"{}\",\"devices\":{}}}",
        rows.len(),
        pairings,
        jstr(&rows.join("\n")),
    ))
}

fn op_devices_revoke(params: &str) -> Result<String, String> {
    let Some(id) = field(params, "id") else {
        return Err("devices_revoke needs \"id\"".into());
    };
    let mut st = Store::open();
    if st.revoke(&id) {
        st.save()?;
        store::audit("control", "admin", "devices.revoke", true, "local", &id);
        Ok(format!(
            "{{\"message\":{}}}",
            jstr(&format!("device {} revoked", id)),
        ))
    } else {
        Err(format!("no such device '{}'", id))
    }
}

fn op_audit_tail(params: &str) -> Result<String, String> {
    let n: usize = field(params, "n")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let path = config::state_dir().join("audit.log");
    let mut text = String::new();
    if let Ok(mut f) = std::fs::File::open(&path) {
        let _ = f.read_to_string(&mut text);
    }
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    Ok(format!(
        "{{\"path\":{},\"text\":{}}}",
        jstr(&path.display().to_string()),
        jstr(&lines[start..].join("\n")),
    ))
}

// --------------------------------------------------------------- renderers --

/// Human-readable dashboard for the CLI (the panel renders the same JSON).
pub fn render_status_text(json: &str) -> String {
    let get = |k: &str| field(json, k).unwrap_or_default();
    format!(
        "spid3r {} (language {})\n\
         config : {}{}\n\
         agent  : {} — {}{}\n\
         devices: {} paired, {} pending pairing code(s)\n\
         audit  : {} ({} line(s))\n\
         state  : {}",
        get("version"),
        get("language"),
        get("ini_path"),
        if get("ini_exists") == "true" {
            if get("ini_valid") == "true" {
                String::new()
            } else {
                format!(" (INVALID: {})", get("ini_errors"))
            }
        } else {
            " (no spid3r.ini yet — run: spid3r control ini --init)".to_string()
        },
        get("agent_addr"),
        if get("agent_listening") == "true" {
            "listening".to_string()
        } else {
            "not listening".to_string()
        },
        {
            let pid = get("agent_pid");
            if pid.is_empty() {
                String::new()
            } else {
                format!(", pid {}", pid)
            }
        },
        get("devices"),
        get("pairings"),
        get("audit_path"),
        get("audit_lines"),
        get("state_dir"),
    )
}

pub fn render_ini_info_text(json: &str) -> String {
    let mut out = String::from("spid3r.ini search order (first hit wins):\n");
    for (i, p) in field(json, "candidates")
        .unwrap_or_default()
        .lines()
        .enumerate()
    {
        out.push_str(&format!("  {}. {}\n", i + 1, p));
    }
    let active = field(json, "active").unwrap_or_default();
    if active.is_empty() {
        out.push_str("no spid3r.ini found yet (defaults apply)\n");
    } else {
        out.push_str(&format!("active: {}\n", active));
    }
    out
}

pub fn render_devices_text(json: &str) -> String {
    let count = field(json, "count").unwrap_or_default();
    let mut out = format!("devices: {}\n", count);
    if let Some(devices) = field(json, "devices") {
        for row in devices.lines() {
            if row.is_empty() {
                continue;
            }
            let parts: Vec<&str> = row.splitn(4, '|').collect();
            if parts.len() == 4 {
                out.push_str(&format!(
                    "  {:<20}  {:<16}  {:<8}  last_seen={}\n",
                    parts[0], parts[1], parts[2], parts[3]
                ));
            } else {
                out.push_str(&format!("  {}\n", row));
            }
        }
    }
    let pending = field(json, "pending_pairings").unwrap_or_default();
    out.push_str(&format!("pending pairings: {}\n", pending));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jstr_escapes_json_specials() {
        assert_eq!(jstr("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
        assert_eq!(jstr(""), "\"\"");
    }

    #[test]
    fn field_reads_flat_objects_with_escapes() {
        let json = "{\"a\":\"x\\\"y\",\"b\":\"line1\\nline2\",\"c\":\"z\"}";
        assert_eq!(field(json, "a").unwrap(), "x\"y");
        assert_eq!(field(json, "b").unwrap(), "line1\nline2");
        assert_eq!(field(json, "c").unwrap(), "z");
        assert!(field(json, "missing").is_none());
    }

    #[test]
    fn field_ignores_matching_text_inside_values() {
        // The key name appears inside the first value; the scanner must skip
        // it because it is not preceded by '{' or ','.
        let json = "{\"a\":\"has \\\"a\\\" inside\",\"b\":\"v\"}";
        assert_eq!(field(json, "a").unwrap(), "has \"a\" inside");
        assert_eq!(field(json, "b").unwrap(), "v");
    }
}
