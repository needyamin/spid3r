//! Known `spid3r.ini` keys: names, types, defaults, validation, and the
//! commented default file `spid3r config init` writes.

/// Value type of a known key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigValue {
    Bool,
    Int,
    Str,
    /// Comma- or semicolon-separated list.
    List,
    /// Enumerated strings (`tree|vm`).
    Enum(&'static [&'static str]),
}

impl ConfigValue {
    pub fn as_default(&self) -> &'static str {
        match self {
            ConfigValue::Bool => "false",
            ConfigValue::Int => "0",
            ConfigValue::Str | ConfigValue::List => "",
            ConfigValue::Enum(_) => "",
        }
    }
}

/// One known configuration key.
#[derive(Debug, Clone, Copy)]
pub struct KeySpec {
    pub section: &'static str,
    pub name: &'static str,
    pub kind: ConfigValue,
    pub default: &'static str,
    pub description: &'static str,
    /// Inclusive integer range when `kind` is `Int`.
    pub min: Option<i64>,
    pub max: Option<i64>,
}

impl KeySpec {
    pub fn dotted(&self) -> String {
        format!("{}.{}", self.section, self.name)
    }

    pub fn coerce(&self, raw: &str) -> Result<(), String> {
        match self.kind {
            ConfigValue::Bool => {
                parse_bool(raw);
                Ok(())
            }
            ConfigValue::Int => {
                let n: i64 = raw
                    .trim()
                    .parse()
                    .map_err(|_| format!("expected an integer, found {:?}", raw))?;
                if let Some(min) = self.min {
                    if n < min {
                        return Err(format!("must be >= {}", min));
                    }
                }
                if let Some(max) = self.max {
                    if n > max {
                        return Err(format!("must be <= {}", max));
                    }
                }
                Ok(())
            }
            ConfigValue::Str | ConfigValue::List => Ok(()),
            ConfigValue::Enum(allowed) => {
                if allowed.contains(&raw.trim()) {
                    Ok(())
                } else {
                    Err(format!("expected one of {:?}", allowed))
                }
            }
        }
    }
}

/// `true` for `1`/`true`/`on`/`yes`; `false` for `0`/`false`/`off`/`no`/empty.
pub fn parse_bool(raw: &str) -> bool {
    let v = raw.trim().to_ascii_lowercase();
    !(v == "0" || v == "false" || v == "off" || v == "no" || v.is_empty())
}

macro_rules! key {
    ($section:literal, $name:literal, $kind:expr, $default:literal, $min:expr, $max:expr, $desc:literal) => {
        KeySpec {
            section: $section,
            name: $name,
            kind: $kind,
            default: $default,
            description: $desc,
            min: $min,
            max: $max,
        }
    };
}

/// Every known key. Order is documentation order (the default INI).
pub static KEYS: &[KeySpec] = &[
    // [spid3r]
    key!("spid3r", "env", ConfigValue::Str, "default", None, None,
        "Configuration environment name; selects [env.<name>] overrides."),
    key!("spid3r", "engine", ConfigValue::Enum(&["tree", "vm"]), "vm", None, None,
        "Default engine for `spid3r run` when --engine is omitted."),
    key!("spid3r", "max_workers", ConfigValue::Int, "256", Some(1), Some(4096),
        "Ceiling for the grow-on-demand task pool."),
    key!("spid3r", "queue_cap", ConfigValue::Int, "65536", Some(16), Some(1_048_576),
        "Bound on the shared ready queue."),
    key!("spid3r", "deadlock_abort", ConfigValue::Bool, "true", None, None,
        "Abort when no task can ever wake a wait."),
    key!("spid3r", "task_drain_ms", ConfigValue::Int, "2000", Some(0), Some(60_000),
        "How long exit waits for still-running tasks."),
    key!("spid3r", "gc", ConfigValue::Bool, "true", None, None,
        "Automatic cycle collection at statement boundaries."),
    // [memory]
    key!("memory", "max_steps", ConfigValue::Int, "0", Some(0), Some(i64::MAX),
        "Execution budget in engine steps; 0 disables the limit."),
    key!("memory", "max_call_depth", ConfigValue::Int, "1000", Some(16), Some(100_000),
        "Interpreter call-depth limit."),
    key!("memory", "gc_threshold", ConfigValue::Int, "0", Some(0), Some(1_000_000_000),
        "Live-cell threshold that triggers an extra collection; 0 uses the default."),
    key!("memory", "allocation_cap_mb", ConfigValue::Int, "0", Some(0), Some(1_048_576),
        "Soft cap on a single allocation, in MiB; 0 disables."),
    // [debug]
    key!("debug", "enabled", ConfigValue::Bool, "false", None, None,
        "Master debug switch (verbose diagnostics, extra checks)."),
    key!("debug", "log_level", ConfigValue::Enum(&["error", "warn", "info", "debug", "trace"]),
        "info", None, None, "Minimum level written to the diagnostic log."),
    key!("debug", "log_stdout", ConfigValue::Bool, "false", None, None,
        "Mirror diagnostic log lines to stderr."),
    // [logging]
    key!("logging", "enabled", ConfigValue::Bool, "true", None, None,
        "Write the append-only diagnostic log."),
    key!("logging", "file", ConfigValue::Str, "", None, None,
        "Log path override (empty = platform default)."),
    key!("logging", "rotate_mb", ConfigValue::Int, "1", Some(1), Some(1024),
        "Rotate the log once it passes this size."),
    key!("logging", "audit", ConfigValue::Bool, "true", None, None,
        "Append remote-management actions to the audit log."),
    // [modules]
    key!("modules", "path", ConfigValue::Str, "", None, None,
        "Extra module search directories, semicolon-separated."),
    key!("modules", "plugins", ConfigValue::Str, "", None, None,
        "Plugin search path (defaults to SPID3R_PLUGINS)."),
    key!("modules", "allow_wasm", ConfigValue::Bool, "true", None, None,
        "Allow loading sandboxed .wasm plugins."),
    key!("modules", "allow_host", ConfigValue::Bool, "true", None, None,
        "Allow host-registered plugins (compiled into the embedding host)."),
    // [security]
    key!("security", "deny_filesystem", ConfigValue::Bool, "false", None, None,
        "Deny filesystem natives to scripts (embed-style policy)."),
    key!("security", "deny_process", ConfigValue::Bool, "false", None, None,
        "Deny process natives to scripts."),
    key!("security", "deny_network", ConfigValue::Bool, "false", None, None,
        "Deny network natives to scripts."),
    key!("security", "deny_environment", ConfigValue::Bool, "false", None, None,
        "Deny environment natives to scripts."),
    key!("security", "allow_remote", ConfigValue::Bool, "false", None, None,
        "Master switch for the management agent; agent.enabled also must be true."),
    // [agent]
    key!("agent", "enabled", ConfigValue::Bool, "false", None, None,
        "Start the management agent when `spid3r agent start` runs."),
    key!("agent", "bind", ConfigValue::Str, "127.0.0.1", None, None,
        "Listen address. 127.0.0.1 is local-only; 0.0.0.0 is required for other hosts."),
    key!("agent", "port", ConfigValue::Int, "9527", Some(1), Some(65535),
        "TCP port of the encrypted management API."),
    key!("agent", "max_connections", ConfigValue::Int, "16", Some(1), Some(256),
        "Concurrent management connections."),
    key!("agent", "request_timeout_ms", ConfigValue::Int, "10000", Some(1000), Some(300_000),
        "Per-request deadline."),
    key!("agent", "max_body", ConfigValue::Int, "262144", Some(1024), Some(8_388_608),
        "Largest accepted (decrypted) request body, in bytes."),
    key!("agent", "pairing_ttl_sec", ConfigValue::Int, "300", Some(30), Some(3600),
        "How long a one-time pairing code remains valid."),
    key!("agent", "name", ConfigValue::Str, "", None, None,
        "Friendly name advertised to administrators (empty = hostname)."),
    // Roles are stored as name=ops pairs; the schema keeps the three built-ins.
    key!("agent", "role_viewer", ConfigValue::Str,
        "status,info,config.get,logs.tail,modules.list,devices.list",
        None, None, "Operations granted to the viewer role."),
    key!("agent", "role_operator", ConfigValue::Str,
        "status,info,config.get,config.set,logs.tail,modules.list,devices.list,ops.run,reload",
        None, None, "Operations granted to the operator role."),
    key!("agent", "role_admin", ConfigValue::Str, "*", None, None,
        "Operations granted to the admin role (* = all)."),
];

/// Dotted default map (`agent.bind` -> `"127.0.0.1"`).
pub static DEFAULTS: &[(&str, ConfigValue, &str)] = {
    // Built from KEYS at compile time is not possible in a const fn over a
    // slice of structs with methods; list the defaults explicitly so the
    // loader never has to call a non-const dotted().
    &[
        ("spid3r.env", ConfigValue::Str, "default"),
        ("spid3r.engine", ConfigValue::Enum(&["tree", "vm"]), "vm"),
        ("spid3r.max_workers", ConfigValue::Int, "256"),
        ("spid3r.queue_cap", ConfigValue::Int, "65536"),
        ("spid3r.deadlock_abort", ConfigValue::Bool, "true"),
        ("spid3r.task_drain_ms", ConfigValue::Int, "2000"),
        ("spid3r.gc", ConfigValue::Bool, "true"),
        ("memory.max_steps", ConfigValue::Int, "0"),
        ("memory.max_call_depth", ConfigValue::Int, "1000"),
        ("memory.gc_threshold", ConfigValue::Int, "0"),
        ("memory.allocation_cap_mb", ConfigValue::Int, "0"),
        ("debug.enabled", ConfigValue::Bool, "false"),
        ("debug.log_level", ConfigValue::Enum(&["error", "warn", "info", "debug", "trace"]), "info"),
        ("debug.log_stdout", ConfigValue::Bool, "false"),
        ("logging.enabled", ConfigValue::Bool, "true"),
        ("logging.file", ConfigValue::Str, ""),
        ("logging.rotate_mb", ConfigValue::Int, "1"),
        ("logging.audit", ConfigValue::Bool, "true"),
        ("modules.path", ConfigValue::Str, ""),
        ("modules.plugins", ConfigValue::Str, ""),
        ("modules.allow_wasm", ConfigValue::Bool, "true"),
        ("modules.allow_host", ConfigValue::Bool, "true"),
        ("security.deny_filesystem", ConfigValue::Bool, "false"),
        ("security.deny_process", ConfigValue::Bool, "false"),
        ("security.deny_network", ConfigValue::Bool, "false"),
        ("security.deny_environment", ConfigValue::Bool, "false"),
        ("security.allow_remote", ConfigValue::Bool, "false"),
        ("agent.enabled", ConfigValue::Bool, "false"),
        ("agent.bind", ConfigValue::Str, "127.0.0.1"),
        ("agent.port", ConfigValue::Int, "9527"),
        ("agent.max_connections", ConfigValue::Int, "16"),
        ("agent.request_timeout_ms", ConfigValue::Int, "10000"),
        ("agent.max_body", ConfigValue::Int, "262144"),
        ("agent.pairing_ttl_sec", ConfigValue::Int, "300"),
        ("agent.name", ConfigValue::Str, ""),
        (
            "agent.role_viewer",
            ConfigValue::Str,
            "status,info,config.get,logs.tail,modules.list,devices.list",
        ),
        (
            "agent.role_operator",
            ConfigValue::Str,
            "status,info,config.get,config.set,logs.tail,modules.list,devices.list,ops.run,reload",
        ),
        ("agent.role_admin", ConfigValue::Str, "*"),
    ]
};

/// Look up a known key by dotted name.
pub fn known_key(dotted: &str) -> Option<&'static KeySpec> {
    let (sec, name) = dotted.split_once('.')?;
    KEYS.iter().find(|k| k.section == sec && k.name == name)
}

/// Commented default `spid3r.ini`.
pub const DEFAULT_INI_TEXT: &str = r#"# spid3r.ini — Spid3r runtime configuration
# Precedence: process env (SPID3R_<SECTION>_<KEY>) > [env.<name>] > this file > defaults.
# Reload: `spid3r config reload`, SIGHUP, or POST /api/v1/config/reload.

[spid3r]
env = default
engine = vm
max_workers = 256
queue_cap = 65536
deadlock_abort = true
task_drain_ms = 2000
gc = true

[memory]
max_steps = 0              ; 0 = unlimited
max_call_depth = 1000
gc_threshold = 0
allocation_cap_mb = 0

[debug]
enabled = false
log_level = info           ; error|warn|info|debug|trace
log_stdout = false

[logging]
enabled = true
file =
rotate_mb = 1
audit = true

[modules]
path =
plugins =
allow_wasm = true
allow_host = true

[security]
deny_filesystem = false
deny_process = false
deny_network = false
deny_environment = false
allow_remote = false

[agent]
enabled = false
bind = 127.0.0.1           ; 0.0.0.0 to accept other hosts
port = 9527
max_connections = 16
request_timeout_ms = 10000
max_body = 262144
pairing_ttl_sec = 300
name =
role_viewer = status,info,config.get,logs.tail,modules.list,devices.list
role_operator = status,info,config.get,config.set,logs.tail,modules.list,devices.list,ops.run,reload
role_admin = *

# [env.production]
# max_workers = 128
# agent.enabled = true
# agent.bind = 0.0.0.0

# [env.development]
# debug.enabled = true
# log_level = trace
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_keys_resolve() {
        assert!(known_key("agent.bind").is_some());
        assert!(known_key("agent.bind").unwrap().section == "agent");
        assert!(known_key("nope.nope").is_none());
    }

    #[test]
    fn coerce_int_range() {
        let k = known_key("agent.port").unwrap();
        assert!(k.coerce("80").is_ok());
        assert!(k.coerce("0").is_err());
        assert!(k.coerce("abc").is_err());
    }

    #[test]
    fn coerce_enum() {
        let k = known_key("spid3r.engine").unwrap();
        assert!(k.coerce("vm").is_ok());
        assert!(k.coerce("wasm").is_err());
    }

    #[test]
    fn defaults_cover_every_key() {
        for k in KEYS {
            let dotted = k.dotted();
            assert!(
                DEFAULTS.iter().any(|(d, _, _)| *d == dotted),
                "missing default for {}",
                dotted
            );
        }
    }
}
