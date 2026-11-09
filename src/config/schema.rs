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

/// Whether a key survives `spid3r config reload` without a restart.
///
/// `Live` keys are re-read at their point of use (or pushed into shared
/// handles by the reload path), so a reload applies them to the running
/// process. `StartupOnly` keys are read once — at engine start, agent bind,
/// module-dir setup — or are not yet consumed by the reload path at all;
/// classifying them honestly beats pretending a reload did something.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReloadSafety {
    /// Applied live by `spid3r config reload`.
    #[default]
    Live,
    /// Takes effect on the next process start, not on reload.
    StartupOnly,
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
    /// How `spid3r config reload` treats this key.
    pub reload: ReloadSafety,
    /// Whether the management agent's remote `config.set` may change this
    /// key. Frozen keys widen or hide capability (deny lists, bind address,
    /// audit toggle), so a remote peer must edit `spid3r.ini` on the host
    /// instead. Derived from the schema, not a hand-written list, so a new
    /// key cannot silently forget the classification.
    pub frozen: bool,
}

impl KeySpec {
    pub fn dotted(&self) -> String {
        format!("{}.{}", self.section, self.name)
    }

    pub fn coerce(&self, raw: &str) -> Result<(), String> {
        match self.kind {
            ConfigValue::Bool => {
                // Strict: a typo like `ture` used to pass validation and then
                // silently mean "true" at the use site.
                let v = raw.trim().to_ascii_lowercase();
                if !matches!(
                    v.as_str(),
                    "" | "0" | "1" | "true" | "false" | "on" | "off" | "yes" | "no"
                ) {
                    return Err(format!("expected a boolean, found {:?}", raw));
                }
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
            reload: ReloadSafety::Live,
            frozen: false,
        }
    };
}

/// A key that only takes effect at process start (or that the reload path
/// does not yet apply): the honest classification for engine/agent-binding
/// knobs a running process cannot re-read.
macro_rules! key_startup {
    ($section:literal, $name:literal, $kind:expr, $default:literal, $min:expr, $max:expr, $desc:literal) => {
        KeySpec {
            section: $section,
            name: $name,
            kind: $kind,
            default: $default,
            description: $desc,
            min: $min,
            max: $max,
            reload: ReloadSafety::StartupOnly,
            frozen: false,
        }
    };
}

/// A key the management agent's remote `config.set` may not change: it
/// widens or hides capability, so only the host's `spid3r.ini` may set it.
macro_rules! key_frozen {
    ($section:literal, $name:literal, $kind:expr, $default:literal, $min:expr, $max:expr, $desc:literal) => {
        KeySpec {
            section: $section,
            name: $name,
            kind: $kind,
            default: $default,
            description: $desc,
            min: $min,
            max: $max,
            reload: ReloadSafety::Live,
            frozen: true,
        }
    };
}

/// Like [`key_frozen!`], for keys that only take effect at process start.
macro_rules! key_frozen_startup {
    ($section:literal, $name:literal, $kind:expr, $default:literal, $min:expr, $max:expr, $desc:literal) => {
        KeySpec {
            section: $section,
            name: $name,
            kind: $kind,
            default: $default,
            description: $desc,
            min: $min,
            max: $max,
            reload: ReloadSafety::StartupOnly,
            frozen: true,
        }
    };
}

/// Every known key. Order is documentation order (the default INI).
pub static KEYS: &[KeySpec] = &[
    // [spid3r]
    key_startup!(
        "spid3r",
        "env",
        ConfigValue::Str,
        "default",
        None,
        None,
        "Configuration environment name; selects [env.<name>] overrides."
    ),
    key_startup!(
        "spid3r",
        "engine",
        ConfigValue::Enum(&["tree", "vm"]),
        "vm",
        None,
        None,
        "Default engine for `spid3r run` when --engine is omitted."
    ),
    key_startup!(
        "spid3r",
        "max_workers",
        ConfigValue::Int,
        "256",
        Some(1),
        Some(4096),
        "Ceiling for the grow-on-demand task pool."
    ),
    key_startup!(
        "spid3r",
        "queue_cap",
        ConfigValue::Int,
        "65536",
        Some(16),
        Some(1_048_576),
        "Bound on the shared ready queue."
    ),
    key_startup!(
        "spid3r",
        "deadlock_abort",
        ConfigValue::Bool,
        "true",
        None,
        None,
        "Abort when no task can ever wake a wait."
    ),
    key_startup!(
        "spid3r",
        "task_drain_ms",
        ConfigValue::Int,
        "2000",
        Some(0),
        Some(60_000),
        "How long exit waits for still-running tasks."
    ),
    key_startup!(
        "spid3r",
        "gc",
        ConfigValue::Bool,
        "true",
        None,
        None,
        "Automatic cycle collection at statement boundaries."
    ),
    // [runtime]
    key!(
        "runtime",
        "wall_clock_limit_ms",
        ConfigValue::Int,
        "0",
        Some(0),
        None,
        "Fatal wall-clock deadline for program execution in ms; 0 = unlimited."
    ),
    key!(
        "runtime",
        "memory_limit_mb",
        ConfigValue::Int,
        "0",
        Some(0),
        Some(1_048_576),
        "Soft cap on total script-value heap in MiB (catchable); 0 = unlimited."
    ),
    key!(
        "runtime",
        "timezone",
        ConfigValue::Str,
        "UTC",
        None,
        None,
        "Default timezone for datetime natives: UTC, a fixed offset like +06:00, or an IANA name (tz_names())."
    ),
    // [net]
    key!(
        "net",
        "timeout_ms",
        ConfigValue::Int,
        "30000",
        Some(0),
        None,
        "HTTP framing deadline in ms (slowloris guard); 0 disables it."
    ),
    key!(
        "net",
        "max_body",
        ConfigValue::Int,
        "8388608",
        Some(1024),
        Some(67_108_864),
        "Largest accepted HTTP request body, in bytes (413 above it)."
    ),
    key!(
        "net",
        "max_headers",
        ConfigValue::Int,
        "64",
        Some(8),
        Some(512),
        "Most request headers accepted per HTTP request (431 above it)."
    ),
    key!(
        "net",
        "keepalive_ms",
        ConfigValue::Int,
        "5000",
        Some(0),
        None,
        "Idle window between keep-alive requests; 0 closes after each response."
    ),
    // [memory]
    key_startup!(
        "memory",
        "max_steps",
        ConfigValue::Int,
        "0",
        Some(0),
        Some(i64::MAX),
        "Execution budget in engine steps; 0 disables the limit."
    ),
    key_startup!(
        "memory",
        "max_call_depth",
        ConfigValue::Int,
        "1000",
        Some(16),
        Some(100_000),
        "Interpreter call-depth limit."
    ),
    key_startup!(
        "memory",
        "gc_threshold",
        ConfigValue::Int,
        "0",
        Some(0),
        Some(1_000_000_000),
        "Live-cell threshold that triggers an extra collection; 0 uses the default."
    ),
    key_startup!(
        "memory",
        "allocation_cap_mb",
        ConfigValue::Int,
        "0",
        Some(0),
        Some(1_048_576),
        "Soft cap on a single allocation, in MiB; 0 disables."
    ),
    // [debug]
    key_startup!(
        "debug",
        "enabled",
        ConfigValue::Bool,
        "false",
        None,
        None,
        "Master debug switch (verbose diagnostics, extra checks)."
    ),
    key_startup!(
        "debug",
        "log_level",
        ConfigValue::Enum(&["error", "warn", "info", "debug", "trace"]),
        "info",
        None,
        None,
        "Minimum level written to the diagnostic log."
    ),
    key!(
        "debug",
        "log_stdout",
        ConfigValue::Bool,
        "false",
        None,
        None,
        "Mirror diagnostic log lines to stderr (alias of errors.display)."
    ),
    // [errors]
    key!(
        "errors",
        "level",
        ConfigValue::Enum(&["error", "warn", "info"]),
        "info",
        None,
        None,
        "Minimum level for script-visible diagnostics (error|warn|info)."
    ),
    key!(
        "errors",
        "display",
        ConfigValue::Bool,
        "false",
        None,
        None,
        "Mirror diagnostic log lines to stderr; canonical spelling of debug.log_stdout."
    ),
    key!(
        "errors",
        "log_file",
        ConfigValue::Str,
        "",
        None,
        None,
        "Dedicated error-log path; empty = the shared diagnostic log only."
    ),
    // [logging]
    key_frozen!(
        "logging",
        "enabled",
        ConfigValue::Bool,
        "true",
        None,
        None,
        "Write the append-only diagnostic log."
    ),
    key_frozen!(
        "logging",
        "file",
        ConfigValue::Str,
        "",
        None,
        None,
        "Log path override (empty = platform default)."
    ),
    key_frozen_startup!(
        "logging",
        "rotate_mb",
        ConfigValue::Int,
        "1",
        Some(1),
        Some(1024),
        "Rotate the log once it passes this size."
    ),
    key_frozen!(
        "logging",
        "audit",
        ConfigValue::Bool,
        "true",
        None,
        None,
        "Append remote-management actions to the audit log."
    ),
    // [modules]
    key_startup!(
        "modules",
        "path",
        ConfigValue::Str,
        "",
        None,
        None,
        "Extra module search directories, semicolon-separated."
    ),
    key_startup!(
        "modules",
        "plugins",
        ConfigValue::Str,
        "",
        None,
        None,
        "Plugin search path (defaults to SPID3R_PLUGINS)."
    ),
    key_startup!(
        "modules",
        "allow_wasm",
        ConfigValue::Bool,
        "true",
        None,
        None,
        "Allow loading sandboxed .wasm plugins."
    ),
    key_startup!(
        "modules",
        "allow_host",
        ConfigValue::Bool,
        "true",
        None,
        None,
        "Allow host-registered plugins (compiled into the embedding host)."
    ),
    key_frozen_startup!(
        "modules",
        "disable",
        ConfigValue::List,
        "",
        None,
        None,
        "Module names that fail to import with a catchable 'module disabled by configuration' error (';' or ',' separated)."
    ),
    key!(
        "modules",
        "assert_active",
        ConfigValue::Bool,
        "true",
        None,
        None,
        "When false, assert()/assert_eq() become no-ops returning true (zend.assertions analog)."
    ),
    // [security]
    key_frozen_startup!(
        "security",
        "deny_filesystem",
        ConfigValue::Bool,
        "false",
        None,
        None,
        "Deny filesystem natives to scripts (embed-style policy)."
    ),
    key_frozen_startup!(
        "security",
        "deny_process",
        ConfigValue::Bool,
        "false",
        None,
        None,
        "Deny process natives to scripts."
    ),
    key_frozen_startup!(
        "security",
        "deny_network",
        ConfigValue::Bool,
        "false",
        None,
        None,
        "Deny network natives to scripts."
    ),
    key_frozen_startup!(
        "security",
        "deny_environment",
        ConfigValue::Bool,
        "false",
        None,
        None,
        "Deny environment natives to scripts."
    ),
    key_frozen_startup!(
        "security",
        "allow_remote",
        ConfigValue::Bool,
        "false",
        None,
        None,
        "Serve the management agent when true; the agent also serves when [agent] enabled is true (either switch starts it, bound to agent.bind)."
    ),
    key_frozen_startup!(
        "security",
        "basedir",
        ConfigValue::List,
        "",
        None,
        None,
        "open_basedir analog: when non-empty, scripts may only touch paths inside these directories (';' or ',' separated); empty = no restriction."
    ),
    key_frozen_startup!(
        "security",
        "disable_natives",
        ConfigValue::List,
        "",
        None,
        None,
        "Named natives replaced by a catchable 'disabled by configuration' denial (';' or ',' separated); unknown names are noted and ignored."
    ),
    // [packages]
    key!(
        "packages",
        "registry",
        ConfigValue::Str,
        "",
        None,
        None,
        "Canonical package registry directory (alias of SPID3R_REGISTRY)."
    ),
    key_frozen!(
        "packages",
        "allow_insecure_git",
        ConfigValue::Bool,
        "false",
        None,
        None,
        "Allow http:// and git:// dependencies (alias of SPID3R_ALLOW_INSECURE_GIT)."
    ),
    key_frozen!(
        "packages",
        "allow_local_git",
        ConfigValue::Bool,
        "false",
        None,
        None,
        "Allow bare-path and file:// git dependencies (alias of SPID3R_ALLOW_LOCAL_GIT_DEPS)."
    ),
    // [paths]
    // All three are frozen: a remote peer that could relocate the host's
    // write targets could point the runtime cache, the agent state home or
    // the SQLite library at attacker-chosen locations (data poisoning or a
    // planted library). Only the host's spid3r.ini may set them.
    key_frozen!(
        "paths",
        "cache_dir",
        ConfigValue::Str,
        "",
        None,
        None,
        "Cache home for the extracted bundle runtime (alias of SPID3R_CACHE_DIR); empty = platform default."
    ),
    key_frozen!(
        "paths",
        "state_dir",
        ConfigValue::Str,
        "",
        None,
        None,
        "Per-user mutable state home: agent store, audit log, pid file (alias of SPID3R_STATE_DIR); empty = platform default."
    ),
    key_frozen_startup!(
        "paths",
        "sqlite_path",
        ConfigValue::Str,
        "",
        None,
        None,
        "Full path of the SQLite shared library tried first (alias of SPID3R_SQLITE_PATH); empty = default search. Read once per process."
    ),
    // [cache]
    key_frozen!(
        "cache",
        "bytecode_dir",
        ConfigValue::Str,
        "",
        None,
        None,
        "Bytecode cache for `spid3r run`: .matrixc containers keyed by the source's SHA-256; empty = disabled."
    ),
    // [agent]
    key_frozen_startup!(
        "agent",
        "enabled",
        ConfigValue::Bool,
        "false",
        None,
        None,
        "Start the management agent when `spid3r agent start` runs."
    ),
    key_frozen_startup!(
        "agent",
        "bind",
        ConfigValue::Str,
        "127.0.0.1",
        None,
        None,
        "Listen address. 127.0.0.1 is local-only; 0.0.0.0 is required for other hosts."
    ),
    key_frozen_startup!(
        "agent",
        "port",
        ConfigValue::Int,
        "9527",
        Some(1),
        Some(65535),
        "TCP port of the encrypted management API."
    ),
    key_frozen_startup!(
        "agent",
        "max_connections",
        ConfigValue::Int,
        "16",
        Some(1),
        Some(256),
        "Concurrent management connections."
    ),
    key_frozen_startup!(
        "agent",
        "request_timeout_ms",
        ConfigValue::Int,
        "10000",
        Some(1000),
        Some(300_000),
        "Per-request deadline."
    ),
    key_frozen_startup!(
        "agent",
        "max_body",
        ConfigValue::Int,
        "262144",
        Some(1024),
        Some(8_388_608),
        "Largest accepted (decrypted) request body, in bytes."
    ),
    key!(
        "agent",
        "pairing_ttl_sec",
        ConfigValue::Int,
        "300",
        Some(30),
        Some(3600),
        "How long a one-time pairing code remains valid."
    ),
    key!(
        "agent",
        "name",
        ConfigValue::Str,
        "",
        None,
        None,
        "Friendly name advertised to administrators (empty = hostname)."
    ),
    // Roles are stored as name=ops pairs; the schema keeps the three built-ins.
    key_frozen!(
        "agent",
        "role_viewer",
        ConfigValue::Str,
        "status,ping,info,config.get,logs.tail,modules.list,devices.list",
        None,
        None,
        "Operations granted to the viewer role."
    ),
    key_frozen!(
        "agent",
        "role_operator",
        ConfigValue::Str,
        "status,ping,info,config.get,config.set,logs.tail,modules.list,devices.list,ops.run,config.reload",
        None,
        None,
        "Operations granted to the operator role."
    ),
    key_frozen!(
        "agent",
        "role_admin",
        ConfigValue::Str,
        "*",
        None,
        None,
        "Operations granted to the admin role (* = all)."
    ),
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
        ("runtime.wall_clock_limit_ms", ConfigValue::Int, "0"),
        ("runtime.memory_limit_mb", ConfigValue::Int, "0"),
        ("runtime.timezone", ConfigValue::Str, "UTC"),
        ("net.timeout_ms", ConfigValue::Int, "30000"),
        ("net.max_body", ConfigValue::Int, "8388608"),
        ("net.max_headers", ConfigValue::Int, "64"),
        ("net.keepalive_ms", ConfigValue::Int, "5000"),
        ("memory.max_steps", ConfigValue::Int, "0"),
        ("memory.max_call_depth", ConfigValue::Int, "1000"),
        ("memory.gc_threshold", ConfigValue::Int, "0"),
        ("memory.allocation_cap_mb", ConfigValue::Int, "0"),
        ("debug.enabled", ConfigValue::Bool, "false"),
        (
            "debug.log_level",
            ConfigValue::Enum(&["error", "warn", "info", "debug", "trace"]),
            "info",
        ),
        ("debug.log_stdout", ConfigValue::Bool, "false"),
        ("errors.level", ConfigValue::Enum(&["error", "warn", "info"]), "info"),
        ("errors.display", ConfigValue::Bool, "false"),
        ("errors.log_file", ConfigValue::Str, ""),
        ("logging.enabled", ConfigValue::Bool, "true"),
        ("logging.file", ConfigValue::Str, ""),
        ("logging.rotate_mb", ConfigValue::Int, "1"),
        ("logging.audit", ConfigValue::Bool, "true"),
        ("modules.path", ConfigValue::Str, ""),
        ("modules.plugins", ConfigValue::Str, ""),
        ("modules.allow_wasm", ConfigValue::Bool, "true"),
        ("modules.allow_host", ConfigValue::Bool, "true"),
        ("modules.disable", ConfigValue::Str, ""),
        ("modules.assert_active", ConfigValue::Bool, "true"),
        ("security.basedir", ConfigValue::Str, ""),
        ("security.disable_natives", ConfigValue::Str, ""),
        ("packages.registry", ConfigValue::Str, ""),
        ("packages.allow_insecure_git", ConfigValue::Bool, "false"),
        ("packages.allow_local_git", ConfigValue::Bool, "false"),
        ("paths.cache_dir", ConfigValue::Str, ""),
        ("paths.state_dir", ConfigValue::Str, ""),
        ("paths.sqlite_path", ConfigValue::Str, ""),
        ("cache.bytecode_dir", ConfigValue::Str, ""),
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
            "status,ping,info,config.get,logs.tail,modules.list,devices.list",
        ),
        (
            "agent.role_operator",
            ConfigValue::Str,
            "status,ping,info,config.get,config.set,logs.tail,modules.list,devices.list,ops.run,config.reload",
        ),
        ("agent.role_admin", ConfigValue::Str, "*"),
    ]
};

/// Look up a known key by dotted name.
pub fn known_key(dotted: &str) -> Option<&'static KeySpec> {
    let (sec, name) = dotted.split_once('.')?;
    KEYS.iter().find(|k| k.section == sec && k.name == name)
}

/// Validation beyond the type/range check in [`KeySpec::coerce`], applied by
/// every config layer. Kept per-key here so the loader never needs to know
/// which keys carry extra rules.
pub fn validate_extra(dotted: &str, raw: &str) -> Result<(), String> {
    match dotted {
        // A mistyped timezone would otherwise only fail at the first datetime
        // call, far from the config line that caused it. Accepts exactly what
        // the tz machinery supports (see `stdlib::tz`): UTC, fixed offsets
        // like `+06:00`, and IANA names from `tz_names()`.
        "runtime.timezone" => crate::stdlib::tz::check_timezone_setting(raw),
        _ => Ok(()),
    }
}

/// Post-load normalization for list keys whose entries are checked against
/// runtime state.
///
/// `validate_extra` can only accept or reject a whole value, but a typo in
/// one entry of `security.disable_natives` must not discard the other names
/// (that would silently re-enable a native the operator meant to deny). So
/// the final value is normalized here, after every layer has been applied:
/// entries are split, trimmed, de-duplicated, and unknown native names are
/// dropped with a note instead of poisoning the whole list.
pub fn normalize_lists(
    values: &mut std::collections::BTreeMap<String, String>,
    notes: &mut Vec<String>,
) {
    let raw = match values.get("security.disable_natives") {
        Some(v) if !v.trim().is_empty() => v.clone(),
        _ => return,
    };
    let known: std::collections::HashSet<&str> = crate::stdlib::builtin_defs()
        .iter()
        .map(|(n, _)| *n)
        .collect();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut kept: Vec<String> = Vec::new();
    for entry in raw.split([',', ';']) {
        let name = entry.trim();
        if name.is_empty() || !seen.insert(name.to_string()) {
            continue;
        }
        if known.contains(name) {
            kept.push(name.to_string());
        } else {
            notes.push(format!(
                "security.disable_natives: unknown native '{}' (ignored)",
                name
            ));
        }
    }
    values.insert("security.disable_natives".to_string(), kept.join(","));
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

[runtime]
wall_clock_limit_ms = 0    ; fatal execution deadline in ms; 0 = unlimited
memory_limit_mb = 0        ; soft cap on script-value heap (catchable); 0 = unlimited
timezone = UTC             ; UTC, a fixed offset like +06:00, or an IANA name

[net]
timeout_ms = 30000         ; HTTP framing deadline in ms (slowloris guard); 0 disables
max_body = 8388608         ; largest accepted request body in bytes (413 above it)
max_headers = 64           ; most request headers accepted (431 above it)
keepalive_ms = 5000        ; idle window between keep-alive requests; 0 = close after each

[memory]
max_steps = 0              ; 0 = unlimited
max_call_depth = 1000
gc_threshold = 0
allocation_cap_mb = 0

[debug]
enabled = false
log_level = info           ; error|warn|info|debug|trace
log_stdout = false         ; alias of errors.display

[errors]
level = info               ; error|warn|info — minimum surfaced diagnostic level
display = false            ; mirror diagnostic log lines to stderr
log_file =                 ; dedicated error-log path; empty = shared log only

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
# disable =                  ; module names that fail to import (';' or ',' separated)
assert_active = true         ; false makes assert()/assert_eq() no-ops returning true

[security]
deny_filesystem = false
deny_process = false
deny_network = false
deny_environment = false
allow_remote = false
# basedir =                  ; open_basedir analog: scripts may only touch paths
                             ; inside these directories (';' or ',' separated)
# disable_natives =          ; natives replaced by a catchable
                             ; 'disabled by configuration' denial

[packages]
# registry =                 ; canonical registry directory (alias: SPID3R_REGISTRY)
allow_insecure_git = false   ; allow http:// and git:// dependencies
allow_local_git = false      ; allow bare-path and file:// git dependencies

[paths]
# cache_dir =                ; cache home for the extracted bundle runtime
                             ; (alias: SPID3R_CACHE_DIR); empty = platform default
# state_dir =                ; per-user state home: agent store, audit log, pid
                             ; file (alias: SPID3R_STATE_DIR); empty = default
# sqlite_path =              ; full path of the SQLite shared library tried first
                             ; (alias: SPID3R_SQLITE_PATH); empty = default search

[cache]
# bytecode_dir =             ; bytecode cache for `spid3r run`: .matrixc containers
                             ; keyed by the source's SHA-256; empty = disabled

[agent]
enabled = false
bind = 127.0.0.1           ; 0.0.0.0 to accept other hosts
port = 9527
max_connections = 16
request_timeout_ms = 10000
max_body = 262144
pairing_ttl_sec = 300
name =
role_viewer = status,ping,info,config.get,logs.tail,modules.list,devices.list
role_operator = status,ping,info,config.get,config.set,logs.tail,modules.list,devices.list,ops.run,config.reload
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
    fn coerce_bool_is_strict() {
        let k = known_key("agent.enabled").unwrap();
        for good in ["true", "false", "1", "0", "on", "off", "yes", "no", ""] {
            assert!(k.coerce(good).is_ok(), "{good:?} should coerce");
        }
        // A typo used to pass validation and silently mean true.
        assert!(k.coerce("banana").is_err());
        assert!(k.coerce("ture").is_err());
    }

    #[test]
    fn defaults_cover_every_key() {
        assert_eq!(DEFAULTS.len(), KEYS.len(), "DEFAULTS/KEYS count drift");
        for k in KEYS {
            let dotted = k.dotted();
            assert!(
                DEFAULTS.iter().any(|(d, _, _)| *d == dotted),
                "missing default for {}",
                dotted
            );
        }
        for (dotted, _, _) in DEFAULTS {
            assert!(known_key(dotted).is_some(), "orphan default for {dotted}");
        }
    }

    #[test]
    fn every_schema_default_coerces_and_default_ini_mentions_every_key() {
        assert_eq!(
            KEYS.len(),
            59,
            "update the phase-5 schema key baseline deliberately"
        );
        for key in KEYS {
            let dotted = key.dotted();
            assert!(
                key.coerce(key.default).is_ok(),
                "invalid default for {dotted}"
            );
            let assignment = format!("{} =", key.name);
            assert!(
                DEFAULT_INI_TEXT.lines().any(|line| {
                    let line = line
                        .trim_start()
                        .strip_prefix('#')
                        .unwrap_or(line)
                        .trim_start();
                    line.starts_with(&assignment)
                }),
                "DEFAULT_INI_TEXT omits {dotted}"
            );
        }
    }

    #[test]
    fn coerce_phase1_keys() {
        let wall = known_key("runtime.wall_clock_limit_ms").unwrap();
        assert!(wall.coerce("0").is_ok());
        assert!(wall.coerce("500").is_ok());
        assert!(wall.coerce("-1").is_err());
        assert!(wall.coerce("banana").is_err());

        let mem = known_key("runtime.memory_limit_mb").unwrap();
        assert!(mem.coerce("0").is_ok());
        assert!(mem.coerce("64").is_ok());
        assert!(mem.coerce("-1").is_err());
        assert!(mem.coerce("1048577").is_err());

        let level = known_key("errors.level").unwrap();
        assert!(level.coerce("error").is_ok());
        assert!(level.coerce("warn").is_ok());
        assert!(level.coerce("info").is_ok());
        assert!(level.coerce("trace").is_err());

        let display = known_key("errors.display").unwrap();
        assert!(display.coerce("true").is_ok());
        assert!(display.coerce("banana").is_err());

        let tz = known_key("runtime.timezone").unwrap();
        assert!(tz.coerce("UTC").is_ok()); // Str: plain coerce passes
        assert!(tz.coerce("Mars/Olympus").is_ok()); // extra rules live in validate_extra
    }

    #[test]
    fn validate_extra_rejects_unknown_timezones() {
        for good in [
            "UTC",
            "utc",
            "+06:00",
            "-05:30",
            "+0630",
            "Europe/Berlin",
            "",
        ] {
            assert!(
                validate_extra("runtime.timezone", good).is_ok(),
                "{good:?} should be accepted"
            );
        }
        for bad in ["Mars/Olympus", "+25:00", "06:00", "+06:0", "Bogus/Zone"] {
            assert!(
                validate_extra("runtime.timezone", bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        // Any other key has no extra rules.
        assert!(validate_extra("agent.bind", "whatever").is_ok());
    }

    #[test]
    fn coerce_phase2_net_keys() {
        let timeout = known_key("net.timeout_ms").unwrap();
        assert!(timeout.coerce("30000").is_ok());
        assert!(timeout.coerce("0").is_ok());
        assert!(timeout.coerce("-1").is_err());
        assert!(timeout.coerce("banana").is_err());

        let body = known_key("net.max_body").unwrap();
        assert!(body.coerce("8388608").is_ok());
        assert!(body.coerce("1024").is_ok());
        assert!(body.coerce("67108864").is_ok());
        assert!(body.coerce("1023").is_err());
        assert!(body.coerce("67108865").is_err());

        let headers = known_key("net.max_headers").unwrap();
        assert!(headers.coerce("64").is_ok());
        assert!(headers.coerce("8").is_ok());
        assert!(headers.coerce("512").is_ok());
        assert!(headers.coerce("7").is_err());
        assert!(headers.coerce("513").is_err());

        let keepalive = known_key("net.keepalive_ms").unwrap();
        assert!(keepalive.coerce("5000").is_ok());
        assert!(keepalive.coerce("0").is_ok());
        assert!(keepalive.coerce("-1").is_err());
    }

    #[test]
    fn reload_classification_is_consistent() {
        // The documented classification: these keys are known to be re-read
        // at their point of use, so a reload applies them.
        for dotted in [
            "runtime.wall_clock_limit_ms",
            "runtime.memory_limit_mb",
            "runtime.timezone",
            "net.timeout_ms",
            "net.max_body",
            "net.max_headers",
            "net.keepalive_ms",
            "errors.level",
            "errors.display",
            "errors.log_file",
            "debug.log_stdout",
            "logging.enabled",
            "logging.file",
            "logging.audit",
        ] {
            let k = known_key(dotted).unwrap();
            assert_eq!(
                k.reload,
                ReloadSafety::Live,
                "{} is documented Live",
                dotted
            );
        }
        // These are read once at startup (or not applied by the reload path
        // at all): claiming Live for them would pretend a reload did work.
        for dotted in [
            "spid3r.engine",
            "spid3r.max_workers",
            "spid3r.queue_cap",
            "memory.max_steps",
            "memory.max_call_depth",
            "agent.bind",
            "agent.port",
            "agent.enabled",
            "agent.max_connections",
            "security.basedir",
            "security.disable_natives",
            "modules.disable",
        ] {
            let k = known_key(dotted).unwrap();
            assert_eq!(
                k.reload,
                ReloadSafety::StartupOnly,
                "{} is documented StartupOnly",
                dotted
            );
        }
    }

    #[test]
    fn coerce_phase3_keys() {
        let basedir = known_key("security.basedir").unwrap();
        assert!(basedir.coerce("C:\\data;C:\\tmp").is_ok());
        assert!(basedir.coerce("").is_ok());

        let disable = known_key("security.disable_natives").unwrap();
        assert!(disable.coerce("read_file,write_file").is_ok());
        assert!(disable.coerce("").is_ok());

        let moddisable = known_key("modules.disable").unwrap();
        assert!(moddisable.coerce("json").is_ok());

        let assert_active = known_key("modules.assert_active").unwrap();
        assert!(assert_active.coerce("false").is_ok());
        assert!(assert_active.coerce("banana").is_err());

        let registry = known_key("packages.registry").unwrap();
        assert!(registry.coerce("D:\\registry").is_ok());

        for dotted in [
            "packages.allow_insecure_git",
            "packages.allow_local_git",
            "modules.assert_active",
        ] {
            let k = known_key(dotted).unwrap();
            assert!(k.coerce("true").is_ok(), "{dotted}");
            assert!(k.coerce("yes").is_ok(), "{dotted}");
            assert!(k.coerce("maybe").is_err(), "{dotted}");
        }
    }

    /// The agent's remote `config.set` refusal is derived from `frozen` in
    /// this schema. The set is pinned exactly: it must cover everything the
    /// previous hand-written check covered (every `[security]` key, every
    /// `[logging]` key, the agent role lists, and the agent
    /// enabled/bind/port/max_connections switches) plus the phase-3 keys
    /// (`security.basedir`, `security.disable_natives`, `modules.disable`,
    /// `packages.allow_insecure_git`, `packages.allow_local_git`,
    /// `agent.request_timeout_ms`, `agent.max_body`). A new key added
    /// without a deliberate frozen decision will fail this test and force
    /// the classification to be made explicitly.
    #[test]
    fn frozen_set_is_pinned() {
        let mut frozen: Vec<String> = KEYS
            .iter()
            .filter(|k| k.frozen)
            .map(|k| k.dotted())
            .collect();
        frozen.sort();
        let mut expected: Vec<&str> = [
            // Everything the hand-written agent check blocked: all
            // [security] and [logging] keys, the role lists, and the
            // agent enable/bind/port/max_connections switches.
            "security.deny_filesystem",
            "security.deny_process",
            "security.deny_network",
            "security.deny_environment",
            "security.allow_remote",
            "security.basedir",
            "security.disable_natives",
            "logging.enabled",
            "logging.file",
            "logging.rotate_mb",
            "logging.audit",
            "agent.enabled",
            "agent.bind",
            "agent.port",
            "agent.max_connections",
            "agent.request_timeout_ms",
            "agent.max_body",
            "agent.role_viewer",
            "agent.role_operator",
            "agent.role_admin",
            // New in phase 3.
            "modules.disable",
            "packages.allow_insecure_git",
            "packages.allow_local_git",
            // New in phase 4: the host's write targets and the SQLite
            // library location. A remote peer that could relocate them
            // could poison data or plant a library.
            "paths.cache_dir",
            "paths.state_dir",
            "paths.sqlite_path",
            "cache.bytecode_dir",
        ]
        .to_vec();
        expected.sort();
        assert_eq!(
            frozen, expected,
            "the frozen set drifted from the pinned list"
        );

        // Deliberately NOT frozen: benign remote management targets.
        for dotted in [
            "agent.name",
            "agent.pairing_ttl_sec",
            "packages.registry",
            "modules.assert_active",
        ] {
            let k = known_key(dotted).unwrap();
            assert!(!k.frozen, "{} must stay remotely settable", dotted);
        }
    }

    #[test]
    fn coerce_phase4_path_keys() {
        for dotted in [
            "paths.cache_dir",
            "paths.state_dir",
            "paths.sqlite_path",
            "cache.bytecode_dir",
        ] {
            let k = known_key(dotted).unwrap_or_else(|| panic!("missing key {}", dotted));
            // Str keys: empty = platform default, any path text is accepted
            // (directories are created on demand; there is no existence
            // requirement to validate cheaply).
            assert!(k.coerce("").is_ok(), "{dotted}");
            assert!(k.coerce("C:\\data\\cache").is_ok(), "{dotted}");
            assert!(k.coerce("/var/cache/spid3r").is_ok(), "{dotted}");
            // All four relocate host write targets: frozen to remote set.
            assert!(k.frozen, "{dotted} must be frozen");
        }
        // Reload classification mirrors how each consumer reads the key:
        // state_dir()/runtime_cache_root()/the bytecode cache consult the
        // snapshot at every call (Live); the SQLite library is loaded once
        // into a process-lifetime handle (StartupOnly).
        assert_eq!(
            known_key("paths.cache_dir").unwrap().reload,
            ReloadSafety::Live
        );
        assert_eq!(
            known_key("paths.state_dir").unwrap().reload,
            ReloadSafety::Live
        );
        assert_eq!(
            known_key("cache.bytecode_dir").unwrap().reload,
            ReloadSafety::Live
        );
        assert_eq!(
            known_key("paths.sqlite_path").unwrap().reload,
            ReloadSafety::StartupOnly
        );
    }

    #[test]
    fn normalize_lists_dedupes_and_notes_unknown_natives() {
        let mut values = std::collections::BTreeMap::new();
        let mut notes: Vec<String> = Vec::new();
        values.insert(
            "security.disable_natives".to_string(),
            "read_file; read_file , write_file, bogus_native ;".to_string(),
        );
        normalize_lists(&mut values, &mut notes);
        assert_eq!(
            values.get("security.disable_natives").unwrap(),
            "read_file,write_file"
        );
        assert_eq!(notes.len(), 1, "{}", notes.join("; "));
        assert!(notes[0].contains("bogus_native"), "{}", notes[0]);

        // Empty or unset lists are left untouched.
        let mut values = std::collections::BTreeMap::new();
        let mut notes: Vec<String> = Vec::new();
        normalize_lists(&mut values, &mut notes);
        assert!(notes.is_empty());
    }
}
