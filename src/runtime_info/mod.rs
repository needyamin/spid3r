//! `spid3rinfo()` — Spid3r's own runtime introspection.
//!
//! One collector, three renderers (HTML, JSON, text), one redaction pass.
//! Scripts call `spid3rinfo()`, `spid3rinfo("html")`, `spid3rinfo("json")`;
//! the CLI calls `spid3r info [--json|--html]`. Sensitive values (tokens,
//! passwords, agent shared keys) are redacted unless the caller is local
//! *and* asked for `--no-redact`.

use std::collections::BTreeMap;

pub mod neofetch;

use crate::value::collections::{make_list, MapData};
use crate::value::{new_ref, MapKey, NativeCtx, Value};

use crate::config;

/// A flat section of `key -> string` rows, in insertion order.
#[derive(Debug, Clone, Default)]
pub struct Section {
    pub name: String,
    pub rows: Vec<(String, String)>,
}

impl Section {
    pub fn new(name: &str) -> Self {
        Section {
            name: name.to_string(),
            rows: Vec::new(),
        }
    }
    pub fn push(&mut self, k: &str, v: impl Into<String>) {
        self.rows.push((k.to_string(), v.into()));
    }
}

/// Keys whose values are redacted even in a local dump.
const SENSITIVE_SUBSTR: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "passphrase",
    "token",
    "secret",
    "shared",
    "credential",
    "authorization",
    "cookie",
    "api_key",
    "apikey",
    "private",
];

/// True when a key looks like it carries a credential.
///
/// Substring matches above, plus a suffix rule for `_key`/`-key`/`key`
/// endings (signing_key, access_key, encryption_key, ...). Redacting a bit
/// too much is the safe side; a bare value under a key literally named
/// "monkey" is an acceptable false positive.
pub fn is_sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    SENSITIVE_SUBSTR.iter().any(|s| k.contains(s))
        || k.ends_with("key")
        || k.ends_with("_key")
        || k.ends_with("-key")
        || k.ends_with("secret")
        || k.ends_with("token")
}

/// The phase-3 policy keys: `security.basedir` maps the host's directory
/// layout for a remote peer, and `security.disable_natives` names exactly
/// which natives are denied — reconnaissance either way. They are NOT
/// credentials: the script that is the subject of the policy can probe both
/// lists by observation anyway, so local and script surfaces show them raw;
/// every agent (remote) surface redacts them via [`redact_value_remote`].
///
/// Phase 4 adds the path keys: `paths.cache_dir`, `paths.state_dir`,
/// `paths.sqlite_path` and `cache.bytecode_dir` map the host's write
/// targets and library locations, so a remote peer learning them gains a
/// map of where data lands (and, for the SQLite library, what could be
/// planted). Same policy treatment: redacted remotely, raw locally.
pub fn is_policy_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    matches!(k.as_str(), "security.basedir" | "security.disable_natives")
        || k == "paths.cache_dir"
        || k == "paths.state_dir"
        || k == "paths.sqlite_path"
        || k == "cache.bytecode_dir"
}

/// True when a value has the shape of a bearer token, JWT / signed session
/// context, long hex key, or high-entropy opaque credential — even when stored
/// under an environment variable name the key-name heuristic does not know.
pub fn looks_like_secret(value: &str) -> bool {
    let v = value.trim();
    if v.len() < 10 {
        return false;
    }
    let lower = v.to_ascii_lowercase();
    if lower.starts_with("bearer ") || lower.starts_with("basic ") {
        let rest = v.split_once(' ').map(|(_, r)| r.trim()).unwrap_or("");
        if rest.len() >= 4 {
            return true;
        }
    }
    // URLs carrying embedded user:password@ credentials (e.g. proxy or DB URLs).
    if let Some((_, after_scheme)) = v.split_once("://") {
        let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
        if let Some((userinfo, host)) = authority.rsplit_once('@') {
            if !host.is_empty() {
                if let Some((user, pass)) = userinfo.split_once(':') {
                    if !user.is_empty() && !pass.is_empty() {
                        return true;
                    }
                }
            }
        }
    }
    if v.len() < 16 {
        return false;
    }
    const SECRET_PREFIXES: &[&str] = &[
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "glpat-",
        "sk-",
        "sk_live_",
        "sk_test_",
        "xoxb-",
        "xoxp-",
        "akia",
        "asia",
        "aiza",
    ];
    if SECRET_PREFIXES.iter().any(|p| lower.starts_with(p)) && !v.contains(char::is_whitespace) {
        return true;
    }
    // JWT or 3-part dot-separated base64url signed token (e.g. header.payload.sig).
    // Avoid false positives on plain lowercase DNS names (e.g. `service.example.internal`)
    // by requiring either the standard JWT `eyJ` JSON-header prefix or a long
    // mixed-class segment (>= 16 chars with upper/digit/`_`/`-` variety).
    if !v.contains(char::is_whitespace) {
        let parts: Vec<&str> = v.split('.').collect();
        if parts.len() == 3
            && v.len() >= 24
            && parts.iter().all(|p| {
                !p.is_empty()
                    && p.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'=')
            })
            && parts.iter().filter(|p| p.len() >= 6).count() >= 2
        {
            let is_jwt_header = parts[0].starts_with("eyJ");
            let has_token_shape = parts.iter().any(|p| p.len() >= 16)
                && (v.bytes().any(|b| b.is_ascii_uppercase())
                    || v.bytes().any(|b| b.is_ascii_digit())
                    || v.bytes().any(|b| b == b'_' || b == b'-'));
            if is_jwt_header || has_token_shape {
                return true;
            }
        }
    }
    // >= 32-char pure hexadecimal secret / digest token.
    if v.len() >= 32 && v.bytes().all(|b| b.is_ascii_hexdigit()) {
        return true;
    }
    // High-entropy opaque token (no spaces, no path/list separators).
    if v.len() >= 24
        && !v.contains(char::is_whitespace)
        && !v.contains('/')
        && !v.contains('\\')
        && !v.contains(';')
        && !v.contains(',')
        && v.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+' | b'=' | b'.' | b':')
        })
    {
        let mut has_lower = false;
        let mut has_upper = false;
        let mut has_digit = false;
        let mut has_sym = false;
        let mut counts = [0u32; 128];
        for &b in v.as_bytes() {
            if b.is_ascii_lowercase() {
                has_lower = true;
            } else if b.is_ascii_uppercase() {
                has_upper = true;
            } else if b.is_ascii_digit() {
                has_digit = true;
            } else {
                has_sym = true;
            }
            if (b as usize) < 128 {
                counts[b as usize] += 1;
            }
        }
        let classes = (has_lower as u8) + (has_upper as u8) + (has_digit as u8) + (has_sym as u8);
        if classes >= 3 {
            let n = v.len() as f64;
            let entropy: f64 = counts
                .iter()
                .filter(|&&c| c > 0)
                .map(|&c| {
                    let p = (c as f64) / n;
                    -p * p.log2()
                })
                .sum();
            if entropy >= 3.8 {
                return true;
            }
        }
    }
    false
}

/// Remote-side redaction: credentials plus the policy keys (F-003). Used by
/// EVERY agent remote branch that ships configuration to a paired device —
/// the keyed `config.get` path, the dump-all `config.get` path (no `key`
/// field), `info` and `dump_diagnostics`. The dump-all branch originally
/// called the local [`redact_value`] and leaked the policy keys raw to a
/// paired peer (QA-caught); both branches now share this remote redactor,
/// and local/script surfaces keep [`redact_value`].
pub fn redact_value_remote(key: &str, value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if !is_sensitive_key(key) && !is_policy_key(key) && !looks_like_secret(value) {
        return value.to_string();
    }
    hash_redact(value)
}

/// Replace a sensitive value with a stable prefix + hash, so an operator can
/// still see that a value is set and whether two values match.
pub fn redact_value(key: &str, value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if !is_sensitive_key(key) && !looks_like_secret(value) {
        return value.to_string();
    }
    hash_redact(value)
}

/// Redact credential-like values embedded in a validation diagnostic while
/// keeping local policy paths visible for operator troubleshooting.
pub fn redact_diagnostic(line: &str) -> String {
    let Some((prefix, _)) = line.split_once(':') else {
        return line.to_string();
    };
    let key = prefix
        .rsplit_once(']')
        .map(|(_, tail)| tail.trim())
        .unwrap_or(prefix.trim());
    if is_sensitive_key(key) {
        format!("{}: [redacted diagnostic]", prefix)
    } else {
        line.to_string()
    }
}

/// The hash form shared by both redaction levels.
fn hash_redact(value: &str) -> String {
    let h = crate::pkg::sha256_hex(value.as_bytes());
    format!("[redacted sha256:{}]", &h[..12])
}

/// Collect every section. `redact` applies to environment and agent state;
/// `remote` additionally redacts the policy keys (`security.basedir`,
/// `security.disable_natives`) — set by the agent's remote ops only, so
/// local dumps keep them readable.
pub fn collect(redact: bool) -> Vec<Section> {
    collect_with(redact, false)
}

/// The agent's variant: same sections, policy keys redacted.
pub fn collect_remote(redact: bool) -> Vec<Section> {
    collect_with(redact, true)
}

fn collect_with(redact: bool, remote: bool) -> Vec<Section> {
    let mut out = Vec::new();
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());

    let mut s = Section::new("Spid3r");
    s.push("Spid3r version", crate::VERSION);
    s.push("Language version", crate::LANGUAGE_VERSION);
    s.push("Host language", "Rust");
    s.push("Compiler", "yamc (YamLLVM)");
    s.push("Execution engine", "Spid3r Engine (spvm / spvm_rt)");
    s.push("Memory manager", "YamGC");
    s.push("Runtime library", "spid3r_lang");
    s.push("Package manager", "spkg");
    s.push("Rust compiler", rustc_version());
    s.push(
        "Build profile",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
    );
    s.push("Target triple", target_triple());
    s.push("License", "GPL-3.0-or-later");
    s.push("Homepage", "https://github.com/needyamin/spid3r");
    out.push(s);

    let mut s = Section::new("System");
    s.push("OS", os_name());
    s.push("OS family", std::env::consts::FAMILY);
    s.push("Architecture", std::env::consts::ARCH);
    s.push(
        "Pointer width",
        format!("{}-bit", std::mem::size_of::<usize>() * 8),
    );
    s.push("Hostname", hostname());
    s.push("Username", whoami());
    // std::process::id() panics (abort) on wasm32-unknown-unknown: there is
    // no OS process there. The playground calls spid3rinfo(), so this row is
    // native-only.
    #[cfg(not(target_arch = "wasm32"))]
    s.push("Process id", std::process::id().to_string());
    s.push(
        "Endian",
        if cfg!(target_endian = "big") {
            "big"
        } else {
            "little"
        },
    );
    let (total, free) = memory_totals();
    s.push("Memory total", total.unwrap_or_else(|| "?".into()));
    s.push("Memory free", free.unwrap_or_else(|| "?".into()));
    s.push("Process RSS", process_rss().unwrap_or_else(|| "?".into()));
    s.push(
        "Process RSS bytes",
        process_rss_bytes()
            .map(|b| b.to_string())
            .unwrap_or_else(|| "?".into()),
    );
    s.push(
        "Logical CPUs",
        std::thread::available_parallelism()
            .map(|n| n.get().to_string())
            .unwrap_or_else(|_| "?".into()),
    );
    out.push(s);

    let cfg = config::snapshot();
    let local_path = |value: String| {
        if remote {
            "[redacted]".to_string()
        } else {
            value
        }
    };
    let mut s = Section::new("Installation");
    s.push("Executable", local_path(exe));
    s.push(
        "Working directory",
        local_path(
            std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "?".into()),
        ),
    );
    s.push(
        "Config file",
        local_path(
            cfg.path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(none; defaults)".into()),
        ),
    );
    s.push(
        "Config directory",
        local_path(
            config::config_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "?".into()),
        ),
    );
    s.push(
        "State directory",
        local_path(config::state_dir().display().to_string()),
    );
    s.push(
        "Runtime cache",
        local_path(crate::stdlib::runtime_cache_root().display().to_string()),
    );
    s.push(
        "SQLite library",
        local_path(match config::get("paths.sqlite_path") {
            Some(v) if !v.trim().is_empty() => v,
            _ => "(default search)".to_string(),
        }),
    );
    s.push(
        "Log file",
        local_path(
            crate::logging::path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "?".into()),
        ),
    );
    s.push(
        "INI search",
        local_path(
            config::candidate_paths()
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("; "),
        ),
    );
    out.push(s);

    let mut s = Section::new("Features");
    s.push("gui", feature(cfg!(feature = "gui")));
    s.push("native (YamLLVM)", feature(cfg!(feature = "native")));
    s.push("tls", feature(cfg!(feature = "tls")));
    s.push("spid3rinfo", "on");
    out.push(s);

    {
        let st = crate::native::rt::slab_alloc_stats();
        let mut s = Section::new("AOT Memory");
        s.push(
            "Slab capacity",
            crate::native::rt::ARENA_SLAB_LEN.to_string(),
        );
        s.push("Slabs allocated", st.slabs_allocated.to_string());
        s.push("Current call allocs", st.current_call_allocs.to_string());
        s.push("Last call allocs", st.last_call_allocs.to_string());
        s.push("Max call allocs", st.max_call_allocs.to_string());
        s.push(
            "Completed call allocs",
            st.completed_call_allocs.to_string(),
        );
        out.push(s);
    }

    let mut s = Section::new("Modules");
    s.push(
        "Embedded stdlib",
        format!("{} modules", crate::stdlib_embed::names().len()),
    );
    out.push(s);

    let mut s = Section::new("Configuration");
    for (k, v) in &cfg.values {
        let shown = if redact && remote {
            redact_value_remote(k, v)
        } else if redact {
            redact_value(k, v)
        } else {
            v.clone()
        };
        s.push(k.as_str(), shown);
    }
    if !remote {
        let mut notes = cfg.notes.clone();
        notes.extend(config::pin_problems(&cfg));
        if !notes.is_empty() {
            s.push(
                "notes",
                notes
                    .iter()
                    .map(|note| redact_diagnostic(note))
                    .collect::<Vec<_>>()
                    .join(" | "),
            );
        }
    }
    out.push(s);

    // std::env::vars() panics on wasm32-unknown-unknown (no process
    // environment exists there), and an env dump would be empty anyway.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut s = Section::new("Environment");
        let mut env: Vec<(String, String)> = std::env::vars().collect();
        env.sort();
        for (k, v) in env {
            // Skip huge values; a full dump should not include the entire environment either.
            if v.len() > 512 {
                continue;
            }
            let shown = if redact && remote {
                redact_value_remote(&k, &v)
            } else if redact {
                redact_value(&k, &v)
            } else {
                v
            };
            s.push(&k, shown);
        }
        out.push(s);
    }

    let mut s = Section::new("Memory (interpreter)");
    s.push("auto GC", config::get("spid3r.gc").unwrap_or_default());
    s.push("live cells", crate::value::gc::live_cells().to_string());
    s.push(
        "max steps",
        config::get("memory.max_steps").unwrap_or_default(),
    );
    s.push(
        "max call depth",
        config::get("memory.max_call_depth").unwrap_or_default(),
    );
    out.push(s);

    out
}

fn feature(on: bool) -> &'static str {
    if on {
        "enabled"
    } else {
        "disabled"
    }
}

fn os_name() -> String {
    if cfg!(target_os = "windows") {
        "Windows".to_string()
    } else if cfg!(target_os = "macos") {
        "macOS".to_string()
    } else if cfg!(target_os = "linux") {
        "Linux".to_string()
    } else {
        std::env::consts::OS.to_string()
    }
}

fn hostname() -> String {
    if let Ok(h) = std::env::var("COMPUTERNAME") {
        return h;
    }
    if let Ok(h) = std::env::var("HOSTNAME") {
        return h;
    }
    if let Ok(text) = std::fs::read_to_string("/etc/hostname") {
        let t = text.trim().to_string();
        if !t.is_empty() {
            return t;
        }
    }
    "?".to_string()
}

fn whoami() -> String {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "?".into())
}

fn rustc_version() -> String {
    option_env!("RUSTC_VERSION")
        .unwrap_or("unknown")
        .to_string()
}

/// Full target triple when the build recorded it (build.rs passes TARGET
/// through), otherwise the bare architecture.
fn target_triple() -> String {
    match option_env!("SPID3R_TARGET") {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => std::env::consts::ARCH.to_string(),
    }
}

/// (total, free) human strings, best effort per OS.
fn memory_totals() -> (Option<String>, Option<String>) {
    #[cfg(target_os = "linux")]
    {
        if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
            let get = |name: &str| -> Option<u64> {
                for line in text.lines() {
                    if let Some(rest) = line.strip_prefix(name) {
                        let rest = rest.trim_start_matches(':').trim();
                        let kb = rest.split_whitespace().next()?;
                        return kb.parse::<u64>().ok().map(|n| n * 1024);
                    }
                }
                None
            };
            let total = get("MemTotal").map(human_bytes);
            let free = get("MemAvailable")
                .or_else(|| get("MemFree"))
                .map(human_bytes);
            return (total, free);
        }
        (None, None)
    }
    #[cfg(target_os = "windows")]
    {
        // GlobalMemoryStatusEx
        #[repr(C)]
        struct MemoryStatusEx {
            dw_length: u32,
            dw_memory_load: u32,
            ull_total_phys: u64,
            ull_avail_phys: u64,
            ull_total_page: u64,
            ull_avail_page: u64,
            ull_total_virtual: u64,
            ull_avail_virtual: u64,
            ull_avail_extended_virtual: u64,
        }
        extern "system" {
            fn GlobalMemoryStatusEx(lpBuffer: *mut MemoryStatusEx) -> i32;
        }
        let mut ms = MemoryStatusEx {
            dw_length: std::mem::size_of::<MemoryStatusEx>() as u32,
            dw_memory_load: 0,
            ull_total_phys: 0,
            ull_avail_phys: 0,
            ull_total_page: 0,
            ull_avail_page: 0,
            ull_total_virtual: 0,
            ull_avail_virtual: 0,
            ull_avail_extended_virtual: 0,
        };
        // SAFETY: ms is a valid, correctly-sized struct.
        let ok = unsafe { GlobalMemoryStatusEx(&mut ms) };
        if ok != 0 {
            return (
                Some(human_bytes(ms.ull_total_phys)),
                Some(human_bytes(ms.ull_avail_phys)),
            );
        }
        (None, None)
    }
    #[cfg(target_os = "macos")]
    {
        // sysctl hw.memsize for total; skip free (needs mach).
        extern "C" {
            fn sysctl(
                name: *const i32,
                namelen: u32,
                oldp: *mut u8,
                oldlenp: *mut usize,
                newp: *mut u8,
                newlen: usize,
            ) -> i32;
        }
        // CTL_HW = 6, HW_MEMSIZE = 24 on Darwin
        let name: [i32; 2] = [6, 24];
        let mut val: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        // SAFETY: sysctl with a valid MIB and out buffer.
        let rc = unsafe {
            sysctl(
                name.as_ptr(),
                2,
                &mut val as *mut u64 as *mut u8,
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc == 0 {
            return (Some(human_bytes(val)), None);
        }
        (None, None)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        (None, None)
    }
}

fn process_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(statm) = std::fs::read_to_string("/proc/self/statm") {
            if let Some(pages_str) = statm.split_whitespace().nth(1) {
                if let Ok(pages) = pages_str.parse::<u64>() {
                    return Some(pages * 4096);
                }
            }
        }
        let text = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                let kb = rest.split_whitespace().next()?;
                return kb.parse::<u64>().ok().map(|n| n * 1024);
            }
        }
        None
    }
    #[cfg(target_os = "windows")]
    {
        #[repr(C)]
        struct ProcessMemoryCounters {
            cb: u32,
            page_fault_count: u32,
            peak_working_set_size: usize,
            working_set_size: usize,
            quota_peak_paged_pool_usage: usize,
            quota_paged_pool_usage: usize,
            quota_peak_non_paged_pool_usage: usize,
            quota_non_paged_pool_usage: usize,
            pagefile_usage: usize,
            peak_pagefile_usage: usize,
        }
        extern "system" {
            fn GetCurrentProcess() -> isize;
            fn K32GetProcessMemoryInfo(
                process: isize,
                ppsmem_counters: *mut ProcessMemoryCounters,
                cb: u32,
            ) -> i32;
        }
        let mut pmc = ProcessMemoryCounters {
            cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
            page_fault_count: 0,
            peak_working_set_size: 0,
            working_set_size: 0,
            quota_peak_paged_pool_usage: 0,
            quota_paged_pool_usage: 0,
            quota_peak_non_paged_pool_usage: 0,
            quota_non_paged_pool_usage: 0,
            pagefile_usage: 0,
            peak_pagefile_usage: 0,
        };
        // SAFETY: `GetCurrentProcess()` returns a valid pseudo-handle and `pmc`
        // is a valid `ProcessMemoryCounters` buffer sized by `pmc.cb`.
        // SAFETY: Win32/POSIX FFI pointers and output buffers are valid and properly sized.
        let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb) };
        if ok != 0 {
            return Some(pmc.working_set_size as u64);
        }
        None
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

fn process_rss() -> Option<String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        process_rss_bytes().map(human_bytes)
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

// Only the OS-specific memory collectors below use this; they are all
// cfg-gated to native targets, so the helper does not exist on wasm.
#[cfg(not(target_arch = "wasm32"))]
fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{} B", n)
    } else {
        format!("{:.1} {}", v, UNITS[u])
    }
}

// ----------------------------------------------------------------- renderers

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Long sections (Environment can carry 150+ rows) are split across cards so
/// the grid stays balanced instead of producing one towering card.
const ROWS_PER_CARD: usize = 18;

/// HTML fragment: one card per section. Shared by the standalone page and
/// any embedder (GUI webview, docs tooling).
pub fn render_html(sections: &[Section]) -> String {
    let mut out = String::new();
    out.push_str("<div class=\"spid3rinfo\">\n");
    let rows_total: usize = sections.iter().map(|s| s.rows.len()).sum();
    out.push_str("<header class=\"si-hero\"><div class=\"si-brand\">Spid<span class=\"si-accent\">3</span>r</div>");
    out.push_str(&format!(
        "<span class=\"si-chip\">v{}</span><span class=\"si-chip si-dim\">runtime information</span><span class=\"si-chip si-dim\">{} sections · {} keys</span></header>\n",
        html_escape(crate::VERSION),
        sections.len(),
        rows_total,
    ));
    out.push_str("<div class=\"si-grid\">\n");
    for sec in sections {
        // At least one card even for an empty section, so nothing vanishes.
        let total_cards = sec.rows.len().div_ceil(ROWS_PER_CARD);
        let chunks: Vec<&[(String, String)]> = if sec.rows.is_empty() {
            vec![&[]]
        } else {
            sec.rows.chunks(ROWS_PER_CARD).collect()
        };
        for (i, chunk) in chunks.iter().enumerate() {
            out.push_str("<section class=\"si-card\">\n");
            if total_cards > 1 {
                out.push_str(&format!(
                    "<h2>{} <span class=\"si-part\">{}/{}</span></h2>\n",
                    html_escape(&sec.name),
                    i + 1,
                    total_cards
                ));
            } else {
                out.push_str(&format!("<h2>{}</h2>\n", html_escape(&sec.name)));
            }
            out.push_str("<dl>\n");
            for (k, v) in chunk.iter() {
                out.push_str(&format!(
                    "<div class=\"si-row\"><dt>{}</dt><dd>{}</dd></div>\n",
                    html_escape(k),
                    html_escape(v)
                ));
            }
            out.push_str("</dl>\n</section>\n");
        }
    }
    out.push_str("</div>\n");
    out.push_str("<footer class=\"si-foot\"><span>generated by <code>spid3rinfo()</code></span><span>zero external assets · dark &amp; light</span></footer>\n");
    out.push_str("</div>\n");
    out
}

/// Full standalone HTML page (`spid3r info --html`, `spid3rinfo("html")`).
/// Self-contained: system fonts only, dark/light follows the OS setting.
pub fn render_html_page(sections: &[Section]) -> String {
    const CSS: &str = concat!(
        ":root{color-scheme:dark light;--bg:#0b0f17;--card:#161e2e;--card-alt:#1b2436;",
        "--line:#24304a;--fg:#f1f5f9;--dim:#94a3b8;--accent:#38bdf8;--accent2:#818cf8;--green:#34d399}",
        "@media (prefers-color-scheme:light){:root{--bg:#eef2f7;--card:#ffffff;--card-alt:#f1f5f9;",
        "--line:#e2e8f0;--fg:#0f172a;--dim:#52627a;--accent:#0284c7;--accent2:#6366f1;--green:#059862}}",
        "*{box-sizing:border-box}",
        "html{-webkit-text-size-adjust:100%}",
        "body{margin:0;background:var(--bg);color:var(--fg);line-height:1.55;",
        "font-family:system-ui,-apple-system,'Segoe UI',Roboto,sans-serif}",
        "::selection{background:var(--accent);color:var(--bg)}",
        ".spid3rinfo{max-width:1180px;margin:0 auto;",
        "padding:clamp(1.1rem,4vw,2.4rem) clamp(.8rem,3vw,1.4rem) 2.6rem}",
        ".si-hero{display:flex;align-items:center;gap:.55rem;flex-wrap:wrap;padding:1.3rem 0 1.2rem;",
        "border-bottom:1px solid var(--line);margin-bottom:1.4rem}",
        ".si-brand{font-size:clamp(1.5rem,4.5vw,2rem);font-weight:800;letter-spacing:-.02em;margin-right:.45rem}",
        ".si-accent{color:var(--green)}",
        ".si-chip{background:var(--card);border:1px solid var(--line);border-radius:999px;",
        "padding:.24rem .8rem;font-size:.78rem;font-family:ui-monospace,SFMono-Regular,Consolas,monospace}",
        ".si-chip.si-dim{color:var(--dim)}",
        ".si-grid{display:grid;grid-template-columns:1fr;gap:1rem;align-items:start}",
        ".si-card{background:var(--card);border:1px solid var(--line);border-radius:14px;",
        "padding:1rem 1.1rem 1.05rem;box-shadow:0 1px 2px rgba(0,0,0,.10);",
        "transition:transform .15s ease,border-color .15s ease,box-shadow .15s ease}",
        "@media (hover:hover){.si-card:hover{transform:translateY(-2px);",
        "border-color:color-mix(in srgb,var(--accent) 45%,var(--line));",
        "box-shadow:0 10px 26px rgba(0,0,0,.20)}}",
        ".si-card h2{display:flex;align-items:center;gap:.5rem;margin:0 0 .65rem;font-size:.75rem;",
        "font-weight:700;text-transform:uppercase;letter-spacing:.12em;color:var(--accent);",
        "font-family:ui-monospace,SFMono-Regular,Consolas,monospace;flex-wrap:wrap}",
        ".si-card h2::before{content:\"\";width:.55rem;height:.55rem;border-radius:3px;flex:none;",
        "background:linear-gradient(135deg,var(--accent),var(--accent2))}",
        ".si-part{color:var(--dim);letter-spacing:.04em}",
        ".si-card dl{margin:0}",
        ".si-row{display:grid;grid-template-columns:minmax(7.5rem,40%) 1fr;gap:.7rem;",
        "align-items:baseline;padding:.34rem 0;border-bottom:1px solid var(--line)}",
        ".si-row:last-child{border-bottom:none;padding-bottom:0}",
        ".si-row dt{color:var(--dim);font-size:.84rem;min-width:0;overflow-wrap:anywhere}",
        ".si-row dd{margin:0;min-width:0;width:fit-content;max-width:100%;",
        "font-family:ui-monospace,SFMono-Regular,Consolas,monospace;font-size:.8rem;",
        "overflow-wrap:anywhere;background:var(--card-alt);border-radius:6px;padding:.1rem .5rem}",
        "@media (max-width:520px){.si-row{grid-template-columns:1fr;gap:.15rem;padding:.45rem 0}}",
        ".si-foot{margin-top:1.6rem;padding-top:1rem;border-top:1px solid var(--line);",
        "display:flex;justify-content:space-between;gap:.5rem;flex-wrap:wrap;",
        "color:var(--dim);font-size:.75rem}",
        ".si-foot code{font-family:ui-monospace,SFMono-Regular,Consolas,monospace}",
        "@media (prefers-reduced-motion:reduce){.si-card{transition:none}}",
    );
    let mut s = String::new();
    s.push_str("<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">");
    s.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    s.push_str("<meta name=\"color-scheme\" content=\"dark light\">");
    s.push_str("<title>spid3rinfo()</title>");
    s.push_str("<style>");
    s.push_str(CSS);
    s.push_str("</style></head><body>\n");
    s.push_str(&render_html(sections));
    s.push_str("</body></html>\n");
    s
}

/// Aligned plain-text dump (CLI default). Organized for both surfaces that
/// show it: a color terminal (ANSI, TTY only) and a plain console (the
/// playground/browser, pipes) where the same layout reads cleanly without
/// escapes.
pub fn render_text(sections: &[Section]) -> String {
    #[cfg(target_arch = "wasm32")]
    let tty = false;
    #[cfg(not(target_arch = "wasm32"))]
    let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());

    const BOLD: &str = "\x1b[1m";
    const CYAN: &str = "\x1b[36m";
    const DIM: &str = "\x1b[2m";
    const RESET: &str = "\x1b[0m";

    let mut out = String::new();
    if tty {
        out.push_str(&format!(
            "{BOLD}{CYAN}Spid3r {}{RESET} · runtime information\n",
            crate::VERSION
        ));
    } else {
        out.push_str(&format!(
            "Spid3r {} - runtime information\n",
            crate::VERSION
        ));
    }
    for sec in sections {
        let width = sec.rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
        let sec_name = &sec.name;
        if tty {
            out.push_str(&format!(
                "\n{BOLD}{CYAN} {sec_name}{RESET} {DIM}{}\n{RESET}",
                "─".repeat(38)
            ));
        } else {
            out.push_str(&format!("\n[{}]\n", sec.name));
        }
        for (k, v) in &sec.rows {
            if tty {
                out.push_str(&format!("{DIM}  {k:<width$}{RESET}  {v}\n"));
            } else {
                out.push_str(&format!("  {k:<width$}  {v}\n"));
            }
        }
    }
    out
}

/// JSON object of `section -> {key: value}`.
pub fn render_json(sections: &[Section]) -> String {
    let mut obj = BTreeMap::new();
    for sec in sections {
        let mut inner = BTreeMap::new();
        for (k, v) in &sec.rows {
            inner.insert(k.clone(), v.clone());
        }
        obj.insert(sec.name.clone(), inner);
    }
    json_object_of_maps(&obj)
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
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

fn json_object_of_maps(m: &BTreeMap<String, BTreeMap<String, String>>) -> String {
    let mut parts = Vec::new();
    for (sec, inner) in m {
        let mut kv = Vec::new();
        for (k, v) in inner {
            kv.push(format!("\"{}\":\"{}\"", json_escape(k), json_escape(v)));
        }
        parts.push(format!("\"{}\":{{{}}}", json_escape(sec), kv.join(",")));
    }
    format!("{{{}}}", parts.join(","))
}

// ------------------------------------------------------------------- native

/// Convert sections into a Spid3r map of maps.
pub fn sections_to_value(sections: &[Section]) -> Value {
    let mut outer = Vec::new();
    for sec in sections {
        let mut rows = Vec::new();
        for (k, v) in &sec.rows {
            rows.push((
                MapKey::Str(k.as_str().into()),
                Value::Str(v.as_str().into()),
            ));
        }
        outer.push((
            MapKey::Str(sec.name.clone().into()),
            Value::Map(new_ref(MapData::from_entries(rows))),
        ));
    }
    Value::Map(new_ref(MapData::from_entries(outer)))
}

/// `spid3rinfo()` / `spid3rinfo("html"|"json"|"text"|"map" [, no_redact])`.
///
/// Default (no argument) returns the structured map, redacted.
/// `"html"` / `"json"` / `"text"` return the rendered string.
/// A second truthy argument disables redaction (local operator choice).
pub fn n_spid3rinfo(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() > 2 {
        return Err(format!(
            "spid3rinfo expects at most 2 argument(s), got {}",
            args.len()
        ));
    }
    let mode = match args.first() {
        None => "map",
        Some(Value::Str(s)) => match s.as_ref() {
            "html" | "json" | "text" | "map" => s.as_ref(),
            other => {
                return Err(format!(
                    "spid3rinfo: unknown mode {:?} (html, json, text, map)",
                    other
                ))
            }
        },
        Some(other) => {
            return Err(format!(
                "spid3rinfo: expected a string mode, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    let no_redact = match args.get(1) {
        None => false,
        Some(v) => crate::value::truthy(v),
    };
    let sections = collect(!no_redact);
    Ok(match mode {
        "map" => sections_to_value(&sections),
        "html" => Value::Str(render_html_page(&sections).into()),
        "json" => Value::Str(render_json(&sections).into()),
        "text" => Value::Str(render_text(&sections).into()),
        _ => unreachable!(),
    })
}

/// Render the info payload as a string in the requested format.
///
/// Used by `spid3r info --out <path>` so the output can be redirected to a
/// file without relying on shell piping (e.g. from a GUI or script runner).
pub fn render_for_cli(json: bool, html: bool, no_redact: bool) -> String {
    let sections = collect(!no_redact);
    if json {
        render_json(&sections)
    } else if html {
        render_html_page(&sections)
    } else {
        render_text(&sections)
    }
}

/// CLI convenience: print the selected rendering to stdout.
pub fn print_for_cli(json: bool, html: bool, no_redact: bool) {
    let sections = collect(!no_redact);
    if json {
        println!("{}", render_json(&sections));
    } else if html {
        println!("{}", render_html_page(&sections));
    } else {
        print!("{}", render_text(&sections));
    }
}

/// Used by the agent `/api/v1/info` endpoint.
pub fn info_map(redact: bool) -> Value {
    info_map_remote(redact)
}

/// The agent's variant: policy keys (`security.basedir`,
/// `security.disable_natives`) are redacted for remote callers (F-003).
pub fn info_map_remote(redact: bool) -> Value {
    sections_to_value(&collect_remote(redact))
}

/// Keep `make_list` in the API surface for future list-shaped sections.
#[allow(dead_code)]
fn unused_make_list(items: Vec<Value>) -> Value {
    make_list(items)
}

pub(crate) mod host_probes {
    //! Audited host OS/hardware probes for `spid3r neofetch`.
    //!
    //! All `unsafe` FFI calls for host introspection live strictly in this
    //! module (`src/runtime_info/mod.rs` is part of the audited host-shim
    //! boundary in `AGENTS.md` §3). Every `unsafe` block carries a `// SAFETY:`
    //! comment and guards buffer lengths and null pointers.

    #[derive(Debug, Clone, Default)]
    pub(crate) struct HostSnapshot {
        pub username: String,
        pub hostname: String,
        pub elevated: bool,
        pub os_display: String,
        pub host: Option<String>,
        pub kernel: String,
        pub firmware_boot: Option<String>,
        pub uptime_secs: Option<u64>,
        pub procs: Option<u32>,
        pub threads: Option<u32>,
        pub shell: String,
        pub terminal: String,
        pub term_cols: Option<u16>,
        pub term_rows: Option<u16>,
        pub display: Option<String>,
        pub cpu_brand: String,
        pub cpu_phys_cores: Option<u32>,
        pub cpu_log_cores: Option<u32>,
        pub cpu_mhz: Option<u32>,
        pub cpu_load_pct: Option<u8>,
        pub cpu_temp_c: Option<i32>,
        pub cpu_l3_bytes: Option<u64>,
        pub gpu: Option<String>,
        pub mem_used: Option<u64>,
        pub mem_total: Option<u64>,
        pub mem_type: Option<String>,
        pub swap_used: Option<u64>,
        pub swap_total: Option<u64>,
        pub disk_label: String,
        pub disk_used: Option<u64>,
        pub disk_total: Option<u64>,
        pub disk_media: Option<String>,
        pub has_battery: bool,
        pub ac_online: Option<bool>,
        pub battery_pct: Option<u8>,
        pub battery_charging: bool,
        pub battery_secs_left: Option<u32>,
        pub battery_full_mwh: Option<u32>,
        pub battery_design_mwh: Option<u32>,
        pub battery_cycles: Option<u32>,
        pub battery_voltage_mv: Option<u32>,
        pub battery_chemistry: Option<String>,
        pub battery_rate_mw: Option<i32>,
        pub power_plan: Option<String>,
        pub local_ip: Option<String>,
        pub link_info: Option<String>,
        pub timezone_iana: Option<String>,
        pub utc_offset_mins: Option<i32>,
        pub locale: String,
    }

    pub(crate) fn enable_vt_console() {
        #[cfg(target_os = "windows")]
        windows_impl::enable_vt_console();
    }

    pub(crate) fn collect_host_snapshot() -> HostSnapshot {
        #[cfg(target_os = "windows")]
        {
            windows_impl::collect()
        }
        #[cfg(all(not(target_os = "windows"), not(target_arch = "wasm32")))]
        {
            unix_impl::collect()
        }
        #[cfg(target_arch = "wasm32")]
        {
            wasm_impl::collect()
        }
    }

    /// Outbound route local IPv4 address via UDP socket connect (zero wire packets).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn udp_route_local_ipv4() -> Option<String> {
        let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        // RFC 5737 TEST-NET-1: connect() on a UDP socket only consults the kernel
        // routing table and picks the source interface address without sending packets.
        sock.connect("192.0.2.1:80").ok()?;
        match sock.local_addr().ok()?.ip() {
            std::net::IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => {
                Some(v4.to_string())
            }
            _ => None,
        }
    }

    #[cfg(target_arch = "wasm32")]
    mod wasm_impl {
        use super::HostSnapshot;

        pub(super) fn collect() -> HostSnapshot {
            HostSnapshot {
                username: super::super::whoami(),
                hostname: super::super::hostname(),
                os_display: "WebAssembly (wasm32)".to_string(),
                kernel: "wasm32-unknown-unknown".to_string(),
                shell: "sandbox".to_string(),
                terminal: "Web".to_string(),
                cpu_brand: "wasm32 virtual CPU".to_string(),
                disk_label: "/".to_string(),
                locale: "C [UTF-8]".to_string(),
                ..HostSnapshot::default()
            }
        }
    }

    #[cfg(target_os = "windows")]
    mod windows_impl {
        use super::HostSnapshot;
        use std::collections::HashMap;
        use std::ffi::c_void;
        use std::io::IsTerminal;
        use std::thread;
        use std::time::Duration;

        // `HKEY_LOCAL_MACHINE` is `(HKEY)(ULONG_PTR)(LONG)0x80000002`, sign-extended on 64-bit.
        const HKEY_LOCAL_MACHINE: usize = (-2_147_483_646_i32 as isize) as usize;
        const KEY_READ: u32 = 0x0002_0019;
        const REG_SZ: u32 = 1;
        const REG_EXPAND_SZ: u32 = 2;
        const REG_BINARY: u32 = 3;
        const REG_DWORD: u32 = 4;
        const REG_QWORD: u32 = 11;

        const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5;
        const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
        const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
        const TOKEN_QUERY: u32 = 0x0008;
        const TOKEN_ELEVATION_CLASS: u32 = 20;
        const ENUM_CURRENT_SETTINGS: u32 = u32::MAX;
        const RELATION_PROCESSOR_CORE: u32 = 0;
        const RELATION_CACHE: u32 = 2;
        const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
        const AF_INET_U32: u32 = 2;
        const GAA_FLAG_SKIP_ANYCAST: u32 = 0x0002;
        const GAA_FLAG_SKIP_MULTICAST: u32 = 0x0004;
        const GAA_FLAG_SKIP_DNS_SERVER: u32 = 0x0008;
        const IF_OPER_STATUS_UP: u32 = 1;
        const IF_TYPE_ETHERNET: u32 = 6;
        const IF_TYPE_PPP: u32 = 23;
        const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
        const IF_TYPE_IEEE80211: u32 = 71;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        const OPEN_EXISTING: u32 = 3;
        const GENERIC_READ: u32 = 0x8000_0000;
        const GENERIC_WRITE: u32 = 0x4000_0000;
        const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
        const IOCTL_BATTERY_QUERY_TAG: u32 = 0x0029_4040;
        const IOCTL_BATTERY_QUERY_INFORMATION: u32 = 0x0029_4044;
        const IOCTL_BATTERY_QUERY_STATUS: u32 = 0x0029_404C;
        const DIGCF_PRESENT: u32 = 0x0000_0002;
        const DIGCF_DEVICEINTERFACE: u32 = 0x0000_0010;

        #[repr(C)]
        struct FileTime {
            lo: u32,
            hi: u32,
        }

        impl FileTime {
            fn as_u64(&self) -> u64 {
                ((self.hi as u64) << 32) | (self.lo as u64)
            }
        }

        #[repr(C)]
        struct Coord {
            x: i16,
            y: i16,
        }

        #[repr(C)]
        struct SmallRect {
            left: i16,
            top: i16,
            right: i16,
            bottom: i16,
        }

        #[repr(C)]
        struct ConsoleScreenBufferInfo {
            dw_size: Coord,
            dw_cursor_position: Coord,
            w_attributes: u16,
            sr_window: SmallRect,
            dw_maximum_window_size: Coord,
        }

        #[repr(C)]
        struct MemoryStatusEx {
            dw_length: u32,
            dw_memory_load: u32,
            ull_total_phys: u64,
            ull_avail_phys: u64,
            ull_total_page: u64,
            ull_avail_page: u64,
            ull_total_virtual: u64,
            ull_avail_virtual: u64,
            ull_avail_extended_virtual: u64,
        }

        #[repr(C)]
        struct PerformanceInformation {
            cb: u32,
            commit_total: usize,
            commit_limit: usize,
            commit_peak: usize,
            physical_total: usize,
            physical_available: usize,
            system_cache: usize,
            kernel_total: usize,
            kernel_paged: usize,
            kernel_nonpaged: usize,
            page_size: usize,
            handle_count: u32,
            process_count: u32,
            thread_count: u32,
        }

        #[repr(C)]
        struct ProcessEntry32W {
            dw_size: u32,
            cnt_usage: u32,
            th32_process_id: u32,
            th32_default_heap_id: usize,
            th32_module_id: u32,
            cnt_threads: u32,
            th32_parent_process_id: u32,
            pc_pri_class_base: i32,
            dw_flags: u32,
            sz_exe_file: [u16; 260],
        }

        #[repr(C)]
        struct SystemPowerStatus {
            ac_line_status: u8,
            battery_flag: u8,
            battery_life_percent: u8,
            system_status_flag: u8,
            battery_life_time: u32,
            battery_full_life_time: u32,
        }

        #[repr(C)]
        struct SystemBatteryState {
            ac_on_line: u8,
            battery_present: u8,
            charging: u8,
            discharging: u8,
            spare1: [u8; 3],
            tag: u8,
            max_capacity: u32,
            remaining_capacity: u32,
            rate: i32,
            estimated_time: u32,
            default_alert1: u32,
            default_alert2: u32,
        }

        #[repr(C)]
        struct SystemTime {
            year: u16,
            month: u16,
            day_of_week: u16,
            day: u16,
            hour: u16,
            minute: u16,
            second: u16,
            milliseconds: u16,
        }

        #[repr(C)]
        struct DynamicTimeZoneInformation {
            bias: i32,
            standard_name: [u16; 32],
            standard_date: SystemTime,
            standard_bias: i32,
            daylight_name: [u16; 32],
            daylight_date: SystemTime,
            daylight_bias: i32,
            time_zone_key_name: [u16; 128],
            dynamic_daylight_time_disabled: u8,
        }

        #[repr(C)]
        struct TokenElevation {
            token_is_elevated: u32,
        }

        #[repr(C)]
        struct Guid {
            data1: u32,
            data2: u16,
            data3: u16,
            data4: [u8; 8],
        }

        #[repr(C)]
        struct SpDeviceInterfaceData {
            cb_size: u32,
            interface_class_guid: Guid,
            flags: u32,
            reserved: usize,
        }

        #[repr(C)]
        struct BatteryQueryInformation {
            battery_tag: u32,
            information_level: u32,
            at_rate: i32,
        }

        #[repr(C)]
        struct BatteryInformation {
            capabilities: u32,
            technology: u8,
            reserved: [u8; 3],
            chemistry: [u8; 4],
            designed_capacity: u32,
            full_charged_capacity: u32,
            default_alert1: u32,
            default_alert2: u32,
            critical_bias: u32,
            cycle_count: u32,
        }

        #[repr(C)]
        struct BatteryWaitStatus {
            battery_tag: u32,
            timeout: u32,
            power_state: u32,
            low_capacity: u32,
            high_capacity: u32,
        }

        #[repr(C)]
        struct BatteryStatus {
            power_state: u32,
            capacity: u32,
            voltage: u32,
            rate: i32,
        }

        #[repr(C)]
        struct SockAddrIn {
            sin_family: u16,
            sin_port: u16,
            sin_addr: [u8; 4],
            sin_zero: [u8; 8],
        }

        #[repr(C)]
        struct SocketAddress {
            lp_sockaddr: *const SockAddrIn,
            i_sockaddr_length: i32,
        }

        #[repr(C)]
        struct IpAdapterUnicastAddress {
            alignment: u64,
            next: *mut IpAdapterUnicastAddress,
            address: SocketAddress,
        }

        #[repr(C)]
        struct IpAdapterAddresses {
            alignment: u64,
            next: *mut IpAdapterAddresses,
            adapter_name: *const u8,
            first_unicast_address: *mut IpAdapterUnicastAddress,
            first_anycast_address: *mut c_void,
            first_multicast_address: *mut c_void,
            first_dns_server_address: *mut c_void,
            dns_suffix: *const u16,
            description: *const u16,
            friendly_name: *const u16,
            physical_address: [u8; 8],
            physical_address_length: u32,
            flags: u32,
            mtu: u32,
            if_type: u32,
            oper_status: u32,
            ipv6_if_index: u32,
            zone_indices: [u32; 16],
            first_prefix: *mut c_void,
            transmit_link_speed: u64,
            receive_link_speed: u64,
        }

        extern "system" {
            fn GetModuleHandleA(lpModuleName: *const u8) -> *mut c_void;
            fn LoadLibraryA(lpLibFileName: *const std::ffi::c_char) -> *mut c_void;
            fn FreeLibrary(hLibModule: *mut c_void) -> i32;
            fn GetProcAddress(hModule: *mut c_void, lpProcName: *const u8) -> *mut c_void;
            fn GetLastError() -> u32;
            fn CloseHandle(hObject: isize) -> i32;
            fn LocalFree(hMem: *mut c_void) -> *mut c_void;
            fn GetCurrentProcess() -> isize;
            fn GetCurrentProcessId() -> u32;
            fn GetTickCount64() -> u64;
            fn GetStdHandle(nStdHandle: u32) -> *mut c_void;
            fn GetConsoleMode(hConsoleHandle: *mut c_void, lpMode: *mut u32) -> i32;
            fn SetConsoleMode(hConsoleHandle: *mut c_void, dwMode: u32) -> i32;
            fn GetConsoleScreenBufferInfo(
                hConsoleOutput: *mut c_void,
                lpConsoleScreenBufferInfo: *mut ConsoleScreenBufferInfo,
            ) -> i32;
            fn GetSystemTimes(
                lpIdleTime: *mut FileTime,
                lpKernelTime: *mut FileTime,
                lpUserTime: *mut FileTime,
            ) -> i32;
            fn GlobalMemoryStatusEx(lpBuffer: *mut MemoryStatusEx) -> i32;
            fn GetDiskFreeSpaceExW(
                lpDirectoryName: *const u16,
                lpFreeBytesAvailableToCaller: *mut u64,
                lpTotalNumberOfBytes: *mut u64,
                lpTotalNumberOfFreeBytes: *mut u64,
            ) -> i32;
            fn GetSystemPowerStatus(lpSystemPowerStatus: *mut SystemPowerStatus) -> i32;
            fn CreateToolhelp32Snapshot(dwFlags: u32, th32ProcessID: u32) -> isize;
            fn Process32FirstW(hSnapshot: isize, lppe: *mut ProcessEntry32W) -> i32;
            fn Process32NextW(hSnapshot: isize, lppe: *mut ProcessEntry32W) -> i32;
            fn CreateFileW(
                lpFileName: *const u16,
                dwDesiredAccess: u32,
                dwShareMode: u32,
                lpSecurityAttributes: *const c_void,
                dwCreationDisposition: u32,
                dwFlagsAndAttributes: u32,
                hTemplateFile: isize,
            ) -> isize;
            fn DeviceIoControl(
                hDevice: isize,
                dwIoControlCode: u32,
                lpInBuffer: *const c_void,
                nInBufferSize: u32,
                lpOutBuffer: *mut c_void,
                nOutBufferSize: u32,
                lpBytesReturned: *mut u32,
                lpOverlapped: *mut c_void,
            ) -> i32;
        }

        struct HandleGuard(isize);
        impl Drop for HandleGuard {
            fn drop(&mut self) {
                if self.0 != 0 && self.0 != -1 {
                    // SAFETY: `self.0` is an owned Win32 HANDLE that is neither null nor INVALID_HANDLE_VALUE.
                    unsafe {
                        let _ = CloseHandle(self.0);
                    }
                }
            }
        }

        struct DllGuard {
            module: *mut c_void,
            owned: bool,
        }
        impl DllGuard {
            fn open(name_nul: &[u8]) -> Option<Self> {
                // SAFETY: `name_nul` is a static NUL-terminated ASCII DLL name.
                let existing = unsafe { GetModuleHandleA(name_nul.as_ptr()) };
                if !existing.is_null() {
                    return Some(Self {
                        module: existing,
                        owned: false,
                    });
                }
                // SAFETY: `name_nul` is a static NUL-terminated ASCII DLL name.
                let loaded = unsafe { LoadLibraryA(name_nul.as_ptr() as *const std::ffi::c_char) };
                if loaded.is_null() {
                    None
                } else {
                    Some(Self {
                        module: loaded,
                        owned: true,
                    })
                }
            }

            fn sym(&self, proc_nul: &[u8]) -> Option<*mut c_void> {
                // SAFETY: `self.module` is a live HMODULE and `proc_nul` is NUL-terminated ASCII.
                let ptr = unsafe { GetProcAddress(self.module, proc_nul.as_ptr()) };
                if ptr.is_null() {
                    None
                } else {
                    Some(ptr)
                }
            }
        }
        impl Drop for DllGuard {
            fn drop(&mut self) {
                if self.owned && !self.module.is_null() {
                    // SAFETY: `self.module` was obtained via `LoadLibraryA` and is freed once here.
                    unsafe {
                        let _ = FreeLibrary(self.module);
                    }
                }
            }
        }

        fn to_wide_nul(s: &str) -> Vec<u16> {
            let mut out: Vec<u16> = s.encode_utf16().collect();
            out.push(0);
            out
        }

        fn from_wide_nul(buf: &[u16]) -> String {
            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..end]).trim().to_string()
        }

        pub(super) fn enable_vt_console() {
            if !std::io::stdout().is_terminal() {
                return;
            }
            // SAFETY: `GetStdHandle`, `GetConsoleMode`, and `SetConsoleMode` operate on the
            // process standard output handle with valid stack storage for `mode`.
            // SAFETY: Win32/POSIX FFI pointers and output buffers are valid and properly sized.
            unsafe {
                let h = GetStdHandle(STD_OUTPUT_HANDLE);
                if h.is_null() || h as isize == -1 {
                    return;
                }
                let mut mode: u32 = 0;
                if GetConsoleMode(h, &mut mode) != 0 {
                    let _ = SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
                }
            }
        }

        fn reg_query_raw(adv: &DllGuard, subkey: &str, value: &str) -> Option<(u32, Vec<u8>)> {
            type RegOpenKeyExWFn =
                unsafe extern "system" fn(usize, *const u16, u32, u32, *mut usize) -> i32;
            type RegQueryValueExWFn = unsafe extern "system" fn(
                usize,
                *const u16,
                *const u32,
                *mut u32,
                *mut u8,
                *mut u32,
            ) -> i32;
            type RegCloseKeyFn = unsafe extern "system" fn(usize) -> i32;

            // SAFETY: Function pointers come from `advapi32.dll` with exact Win32 ABI signatures.
            let (open_fn, query_fn, close_fn) = unsafe {
                let open_p = adv.sym(b"RegOpenKeyExW\0")?;
                let query_p = adv.sym(b"RegQueryValueExW\0")?;
                let close_p = adv.sym(b"RegCloseKey\0")?;
                (
                    std::mem::transmute::<*mut c_void, RegOpenKeyExWFn>(open_p),
                    std::mem::transmute::<*mut c_void, RegQueryValueExWFn>(query_p),
                    std::mem::transmute::<*mut c_void, RegCloseKeyFn>(close_p),
                )
            };

            let subkey_w = to_wide_nul(subkey);
            let value_w = to_wide_nul(value);
            let mut hkey: usize = 0;
            // SAFETY: `subkey_w` is NUL-terminated UTF-16 and `hkey` receives the opened handle.
            let rc = unsafe {
                open_fn(
                    HKEY_LOCAL_MACHINE,
                    subkey_w.as_ptr(),
                    0,
                    KEY_READ,
                    &mut hkey,
                )
            };
            if rc != 0 {
                return None;
            }
            struct RegKeyGuard(usize, RegCloseKeyFn);
            impl Drop for RegKeyGuard {
                fn drop(&mut self) {
                    // SAFETY: `self.0` was opened by `RegOpenKeyExW`.
                    unsafe {
                        let _ = (self.1)(self.0);
                    }
                }
            }
            let _guard = RegKeyGuard(hkey, close_fn);

            let mut kind: u32 = 0;
            let mut size: u32 = 0;
            // SAFETY: Passing null data buffer queries required byte count into `size`.
            let rc = unsafe {
                query_fn(
                    hkey,
                    value_w.as_ptr(),
                    std::ptr::null(),
                    &mut kind,
                    std::ptr::null_mut(),
                    &mut size,
                )
            };
            if rc != 0 || size == 0 || size > 64 * 1024 {
                return None;
            }
            let mut buf = vec![0u8; size as usize];
            // SAFETY: `buf` has length `size`, matching the queried capacity.
            let rc = unsafe {
                query_fn(
                    hkey,
                    value_w.as_ptr(),
                    std::ptr::null(),
                    &mut kind,
                    buf.as_mut_ptr(),
                    &mut size,
                )
            };
            if rc != 0 {
                return None;
            }
            buf.truncate(size as usize);
            Some((kind, buf))
        }

        fn reg_str(adv: &DllGuard, subkey: &str, value: &str) -> Option<String> {
            let (kind, bytes) = reg_query_raw(adv, subkey, value)?;
            if kind != REG_SZ && kind != REG_EXPAND_SZ {
                return None;
            }
            let mut units = Vec::with_capacity(bytes.len() / 2);
            for &[b0, b1] in bytes.as_chunks::<2>().0 {
                units.push(u16::from_le_bytes([b0, b1]));
            }
            let s = from_wide_nul(&units);
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }

        fn reg_u32(adv: &DllGuard, subkey: &str, value: &str) -> Option<u32> {
            let (kind, bytes) = reg_query_raw(adv, subkey, value)?;
            if (kind == REG_DWORD || kind == REG_BINARY) && bytes.len() >= 4 {
                Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            } else {
                None
            }
        }

        fn reg_u64(adv: &DllGuard, subkey: &str, value: &str) -> Option<u64> {
            let (kind, bytes) = reg_query_raw(adv, subkey, value)?;
            if (kind == REG_QWORD || kind == REG_BINARY) && bytes.len() >= 8 {
                Some(u64::from_le_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ]))
            } else if (kind == REG_DWORD || kind == REG_BINARY) && bytes.len() >= 4 {
                Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64)
            } else {
                None
            }
        }

        fn is_elevated(adv: Option<&DllGuard>) -> bool {
            let Some(adv) = adv else {
                return false;
            };
            type OpenProcessTokenFn = unsafe extern "system" fn(isize, u32, *mut isize) -> i32;
            type GetTokenInformationFn =
                unsafe extern "system" fn(isize, u32, *mut c_void, u32, *mut u32) -> i32;
            // SAFETY: Exact Win32 `advapi32.dll` function signatures.
            let (open_tok, get_tok) = unsafe {
                let Some(p1) = adv.sym(b"OpenProcessToken\0") else {
                    return false;
                };
                let Some(p2) = adv.sym(b"GetTokenInformation\0") else {
                    return false;
                };
                (
                    std::mem::transmute::<*mut c_void, OpenProcessTokenFn>(p1),
                    std::mem::transmute::<*mut c_void, GetTokenInformationFn>(p2),
                )
            };
            let mut token: isize = 0;
            // SAFETY: `GetCurrentProcess()` returns the pseudo-handle; `token` receives the token handle.
            if unsafe { open_tok(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
                return false;
            }
            let _guard = HandleGuard(token);
            let mut elev = TokenElevation {
                token_is_elevated: 0,
            };
            let mut ret_len: u32 = 0;
            // SAFETY: `elev` is sized for `TokenElevation` and `ret_len` receives written byte length.
            let ok = unsafe {
                get_tok(
                    token,
                    TOKEN_ELEVATION_CLASS,
                    &mut elev as *mut TokenElevation as *mut c_void,
                    std::mem::size_of::<TokenElevation>() as u32,
                    &mut ret_len,
                )
            };
            ok != 0 && elev.token_is_elevated != 0
        }

        fn firmware_boot(k32: Option<&DllGuard>, adv: Option<&DllGuard>) -> Option<String> {
            type GetFirmwareTypeFn = unsafe extern "system" fn(*mut u32) -> i32;
            let fw_type = if let Some(k32) = k32 {
                if let Some(p) = k32.sym(b"GetFirmwareType\0") {
                    // SAFETY: `GetFirmwareType` takes `*mut u32` and returns BOOL.
                    let func = unsafe { std::mem::transmute::<*mut c_void, GetFirmwareTypeFn>(p) };
                    let mut ft: u32 = 0;
                    // SAFETY: `ft` is valid stack storage.
                    if unsafe { func(&mut ft) } != 0 {
                        Some(ft)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            let secure = adv.and_then(|a| {
                reg_u32(
                    a,
                    r"SYSTEM\CurrentControlSet\Control\SecureBoot\State",
                    "UEFISecureBootEnabled",
                )
            });
            match (fw_type, secure) {
                (Some(2), Some(1)) => Some("UEFI SecureBoot".to_string()),
                (Some(2), _) => Some("UEFI".to_string()),
                (Some(1), _) => Some("BIOS".to_string()),
                (None, Some(1)) => Some("UEFI SecureBoot".to_string()),
                _ => None,
            }
        }

        fn perf_counts(k32: Option<&DllGuard>) -> (Option<u32>, Option<u32>, Option<(u64, u64)>) {
            type K32GetPerfFn = unsafe extern "system" fn(*mut PerformanceInformation, u32) -> i32;
            let psapi = DllGuard::open(b"psapi.dll\0");
            let fn_ptr = k32
                .and_then(|d| d.sym(b"K32GetPerformanceInfo\0"))
                .or_else(|| psapi.as_ref().and_then(|d| d.sym(b"GetPerformanceInfo\0")));
            let Some(p) = fn_ptr else {
                return (None, None, None);
            };
            // SAFETY: Matches `K32GetPerformanceInfo` / `GetPerformanceInfo` signature.
            let func = unsafe { std::mem::transmute::<*mut c_void, K32GetPerfFn>(p) };
            let mut pi = PerformanceInformation {
                cb: std::mem::size_of::<PerformanceInformation>() as u32,
                commit_total: 0,
                commit_limit: 0,
                commit_peak: 0,
                physical_total: 0,
                physical_available: 0,
                system_cache: 0,
                kernel_total: 0,
                kernel_paged: 0,
                kernel_nonpaged: 0,
                page_size: 0,
                handle_count: 0,
                process_count: 0,
                thread_count: 0,
            };
            // SAFETY: `pi` is properly initialized with `cb = size_of::<PerformanceInformation>()`.
            if unsafe { func(&mut pi, pi.cb) } == 0 {
                return (None, None, None);
            }
            let page_sz = pi.page_size as u64;
            let commit_used = (pi.commit_total as u64).saturating_mul(page_sz);
            let commit_limit = (pi.commit_limit as u64).saturating_mul(page_sz);
            (
                Some(pi.process_count),
                Some(pi.thread_count),
                Some((commit_used, commit_limit)),
            )
        }

        fn detect_parent_shell() -> String {
            // SAFETY: `CreateToolhelp32Snapshot` returns a valid snapshot or `INVALID_HANDLE_VALUE` (-1).
            let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
            if snap != 0 && snap != -1 {
                let _guard = HandleGuard(snap);
                let mut entry = ProcessEntry32W {
                    dw_size: std::mem::size_of::<ProcessEntry32W>() as u32,
                    cnt_usage: 0,
                    th32_process_id: 0,
                    th32_default_heap_id: 0,
                    th32_module_id: 0,
                    cnt_threads: 0,
                    th32_parent_process_id: 0,
                    pc_pri_class_base: 0,
                    dw_flags: 0,
                    sz_exe_file: [0; 260],
                };
                let mut map: HashMap<u32, (u32, String)> = HashMap::new();
                // SAFETY: `snap` is valid and `entry.dw_size` is initialized.
                let mut ok = unsafe { Process32FirstW(snap, &mut entry) };
                while ok != 0 {
                    let exe = from_wide_nul(&entry.sz_exe_file);
                    map.insert(entry.th32_process_id, (entry.th32_parent_process_id, exe));
                    // SAFETY: `snap` and `entry` remain valid for enumeration.
                    ok = unsafe { Process32NextW(snap, &mut entry) };
                }
                // SAFETY: OS/FFI pointer and length arguments are validated before the call.
                let mut cur = unsafe { GetCurrentProcessId() };
                for _ in 0..8 {
                    let Some((ppid, _)) = map.get(&cur) else {
                        break;
                    };
                    if *ppid == 0 || *ppid == cur {
                        break;
                    }
                    let Some((_, pexe)) = map.get(ppid) else {
                        break;
                    };
                    let lower = pexe.to_ascii_lowercase();
                    if matches!(
                        lower.as_str(),
                        "powershell.exe"
                            | "pwsh.exe"
                            | "cmd.exe"
                            | "bash.exe"
                            | "zsh.exe"
                            | "sh.exe"
                            | "fish.exe"
                            | "nu.exe"
                            | "wsl.exe"
                    ) {
                        return pexe.clone();
                    }
                    cur = *ppid;
                }
            }
            if let Ok(sh) = std::env::var("SHELL") {
                if let Some(name) = std::path::Path::new(&sh)
                    .file_name()
                    .and_then(|s| s.to_str())
                {
                    if !name.is_empty() {
                        return name.to_string();
                    }
                }
            }
            std::env::var("COMSPEC")
                .ok()
                .and_then(|p| {
                    std::path::Path::new(&p)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| "cmd.exe".to_string())
        }

        fn console_dimensions() -> Option<(u16, u16)> {
            // SAFETY: `GetStdHandle` and `GetConsoleScreenBufferInfo` validate handle before use.
            unsafe {
                let h = GetStdHandle(STD_OUTPUT_HANDLE);
                if h.is_null() || h as isize == -1 {
                    return None;
                }
                let mut info = ConsoleScreenBufferInfo {
                    dw_size: Coord { x: 0, y: 0 },
                    dw_cursor_position: Coord { x: 0, y: 0 },
                    w_attributes: 0,
                    sr_window: SmallRect {
                        left: 0,
                        top: 0,
                        right: 0,
                        bottom: 0,
                    },
                    dw_maximum_window_size: Coord { x: 0, y: 0 },
                };
                if GetConsoleScreenBufferInfo(h, &mut info) != 0 {
                    let cols = (info.sr_window.right - info.sr_window.left + 1).max(1) as u16;
                    let rows = (info.sr_window.bottom - info.sr_window.top + 1).max(1) as u16;
                    return Some((cols, rows));
                }
                None
            }
        }

        fn detect_terminal() -> String {
            if std::env::var_os("WT_SESSION").is_some() {
                return "Windows Terminal".to_string();
            }
            if let Ok(tp) = std::env::var("TERM_PROGRAM") {
                let t = tp.trim();
                if !t.is_empty() {
                    if t.eq_ignore_ascii_case("vscode") {
                        return "VS Code".to_string();
                    }
                    return t.to_string();
                }
            }
            if std::env::var_os("ConEmuPID").is_some() {
                return "ConEmu".to_string();
            }
            if let Ok(t) = std::env::var("TERM") {
                let trimmed = t.trim();
                if !trimmed.is_empty() && trimmed != "dumb" {
                    return trimmed.to_string();
                }
            }
            "Windows Console".to_string()
        }

        fn display_info() -> Option<String> {
            let user32 = DllGuard::open(b"user32.dll\0")?;
            type EnumDisplaySettingsWFn =
                unsafe extern "system" fn(*const u16, u32, *mut u8) -> i32;
            type GetDpiForSystemFn = unsafe extern "system" fn() -> u32;
            // SAFETY: Exact Win32 `user32.dll` signatures.
            let enum_fn = unsafe {
                std::mem::transmute::<*mut c_void, EnumDisplaySettingsWFn>(
                    user32.sym(b"EnumDisplaySettingsW\0")?,
                )
            };
            // `DEVMODEW` is 220 bytes; `dmSize` at offset 68, `dmPelsWidth` at 172,
            // `dmPelsHeight` at 176, `dmDisplayFrequency` at 184.
            #[repr(C, align(4))]
            struct DevModeBuf([u8; 220]);
            let mut buf = DevModeBuf([0u8; 220]);
            buf.0[68..70].copy_from_slice(&(220u16).to_le_bytes());
            // SAFETY: `buf` is 220 bytes, 4-byte aligned, with `dmSize = 220`.
            let ok =
                // SAFETY: Win32/POSIX FFI pointers and output buffers are valid and properly sized.
                unsafe { enum_fn(std::ptr::null(), ENUM_CURRENT_SETTINGS, buf.0.as_mut_ptr()) };
            if ok == 0 {
                return None;
            }
            let width = u32::from_le_bytes([buf.0[172], buf.0[173], buf.0[174], buf.0[175]]);
            let height = u32::from_le_bytes([buf.0[176], buf.0[177], buf.0[178], buf.0[179]]);
            let hz = u32::from_le_bytes([buf.0[184], buf.0[185], buf.0[186], buf.0[187]]);
            if width == 0 || height == 0 {
                return None;
            }
            let dpi_pct = user32
                .sym(b"GetDpiForSystem\0")
                .map(|p| {
                    // SAFETY: `GetDpiForSystem` takes no arguments and returns DPI.
                    let f = unsafe { std::mem::transmute::<*mut c_void, GetDpiForSystemFn>(p) };
                    // SAFETY: Win32/POSIX FFI pointers and output buffers are valid and properly sized.
                    unsafe { f() }
                })
                .filter(|&d| d >= 96)
                .map(|d| (d * 100 + 48) / 96);

            let mut s = if hz > 0 {
                format!("{}x{} @ {}Hz", width, height, hz)
            } else {
                format!("{}x{}", width, height)
            };
            if let Some(pct) = dpi_pct {
                s.push_str(&format!("  \u{2022}  {}% DPI", pct));
            }
            Some(s)
        }

        fn sample_cpu_times() -> Option<(u64, u64)> {
            let mut idle = FileTime { lo: 0, hi: 0 };
            let mut kernel = FileTime { lo: 0, hi: 0 };
            let mut user = FileTime { lo: 0, hi: 0 };
            // SAFETY: Stack-allocated `FileTime` pointers are valid and aligned.
            if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
                return None;
            }
            let idle_u = idle.as_u64();
            let total_u = kernel.as_u64().saturating_add(user.as_u64());
            Some((idle_u, total_u))
        }

        fn cpu_topology(k32: Option<&DllGuard>) -> (Option<u32>, Option<u64>) {
            let Some(k32) = k32 else {
                return (None, None);
            };
            let Some(p) = k32.sym(b"GetLogicalProcessorInformationEx\0") else {
                return (None, None);
            };
            type GetLpiExFn = unsafe extern "system" fn(u32, *mut u8, *mut u32) -> i32;
            // SAFETY: Exact Win32 `GetLogicalProcessorInformationEx` signature.
            let func = unsafe { std::mem::transmute::<*mut c_void, GetLpiExFn>(p) };

            let query_rel = |rel: u32| -> Option<Vec<u8>> {
                let mut len: u32 = 0;
                // SAFETY: First call with null buffer retrieves needed size into `len`.
                let ok = unsafe { func(rel, std::ptr::null_mut(), &mut len) };
                // SAFETY: Win32/POSIX FFI pointers and output buffers are valid and properly sized.
                if ok != 0 || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER || len == 0 {
                    return None;
                }
                let mut buf = vec![0u8; len as usize];
                // SAFETY: `buf` has exact length `len` returned by the OS.
                if unsafe { func(rel, buf.as_mut_ptr(), &mut len) } == 0 {
                    return None;
                }
                buf.truncate(len as usize);
                Some(buf)
            };

            let phys_cores = query_rel(RELATION_PROCESSOR_CORE).map(|buf| {
                let mut count: u32 = 0;
                let mut off = 0usize;
                while off + 8 <= buf.len() {
                    let rel =
                        u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
                    let sz = u32::from_le_bytes([
                        buf[off + 4],
                        buf[off + 5],
                        buf[off + 6],
                        buf[off + 7],
                    ]) as usize;
                    if sz < 8 || off + sz > buf.len() {
                        break;
                    }
                    if rel == RELATION_PROCESSOR_CORE {
                        count += 1;
                    }
                    off += sz;
                }
                count
            });

            let l3_bytes = query_rel(RELATION_CACHE).and_then(|buf| {
                let mut total_l3: u64 = 0;
                let mut off = 0usize;
                while off + 20 <= buf.len() {
                    let rel =
                        u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
                    let sz = u32::from_le_bytes([
                        buf[off + 4],
                        buf[off + 5],
                        buf[off + 6],
                        buf[off + 7],
                    ]) as usize;
                    if sz < 20 || off + sz > buf.len() {
                        break;
                    }
                    if rel == RELATION_CACHE {
                        let level = buf[off + 8];
                        let cache_size = u32::from_le_bytes([
                            buf[off + 12],
                            buf[off + 13],
                            buf[off + 14],
                            buf[off + 15],
                        ]);
                        if level == 3 {
                            total_l3 = total_l3.saturating_add(cache_size as u64);
                        }
                    }
                    off += sz;
                }
                if total_l3 > 0 {
                    Some(total_l3)
                } else {
                    None
                }
            });

            (phys_cores.filter(|&c| c > 0), l3_bytes)
        }

        fn gpu_probe(adv: Option<&DllGuard>) -> Option<String> {
            let adv = adv?;
            let base =
                r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
            let mut candidates: Vec<(bool, u64, String)> = Vec::new();
            for idx in 0..8 {
                let sub = format!("{base}\\{idx:04}");
                let Some(desc) = reg_str(adv, &sub, "DriverDesc") else {
                    continue;
                };
                if desc.is_empty() {
                    continue;
                }
                let is_basic = desc.eq_ignore_ascii_case("Microsoft Basic Display Adapter");
                // Read 64-bit qwMemorySize first; note 32-bit `MemorySize` overflows at 2 GiB
                // (e.g. Intel iGPU reports `0x7ffff000` sentinel ~2 GiB) so ignore negative/sentinel
                // 32-bit `MemorySize` when `qwMemorySize` is absent unless cleanly <= 16 GiB.
                let vram_bytes = reg_u64(adv, &sub, "HardwareInformation.qwMemorySize")
                    .filter(|&v| v > 0 && v < (1u64 << 40))
                    .or_else(|| {
                        reg_u64(adv, &sub, "HardwareInformation.MemorySize")
                            .filter(|&v| v > 0 && v < 0x7FFF_0000)
                    })
                    .unwrap_or(0);
                candidates.push((!is_basic, vram_bytes, desc));
            }
            candidates.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
            let (_, vram, name) = candidates.into_iter().next()?;
            if vram >= 128 * 1024 * 1024 {
                let gib = (vram as f64) / (1024.0 * 1024.0 * 1024.0);
                if (gib - gib.round()).abs() < 0.08 {
                    Some(format!("{} ({:.0} GiB)", name, gib.round()))
                } else {
                    Some(format!("{} ({:.1} GiB)", name, gib))
                }
            } else {
                Some(name)
            }
        }

        fn smbios_mem_type(k32: Option<&DllGuard>) -> Option<String> {
            let k32 = k32?;
            let p = k32.sym(b"GetSystemFirmwareTable\0")?;
            type GetSysFwTableFn = unsafe extern "system" fn(u32, u32, *mut u8, u32) -> u32;
            // SAFETY: Exact Win32 `GetSystemFirmwareTable` signature.
            let func = unsafe { std::mem::transmute::<*mut c_void, GetSysFwTableFn>(p) };
            let rsmb = u32::from_be_bytes(*b"RSMB");
            // SAFETY: Query buffer size with null buffer.
            let needed = unsafe { func(rsmb, 0, std::ptr::null_mut(), 0) };
            if !(16..=1024 * 1024).contains(&needed) {
                return None;
            }
            let mut buf = vec![0u8; needed as usize];
            // SAFETY: `buf` is allocated with `needed` bytes.
            let got = unsafe { func(rsmb, 0, buf.as_mut_ptr(), needed) };
            if got < 8 || (got as usize) > buf.len() {
                return None;
            }
            let data = &buf[8..got as usize];
            let mut pos = 0usize;
            while pos + 4 <= data.len() {
                let rec_type = data[pos];
                let hdr_len = data[pos + 1] as usize;
                if hdr_len < 4 || pos + hdr_len > data.len() {
                    break;
                }
                if rec_type == 17 && hdr_len >= 0x13 {
                    let size_word = u16::from_le_bytes([data[pos + 0x0C], data[pos + 0x0D]]);
                    if size_word != 0 {
                        let mem_type = data[pos + 0x12];
                        let label = match mem_type {
                            0x12 => Some("DDR"),
                            0x13 => Some("DDR2"),
                            0x18 => Some("DDR3"),
                            0x1A => Some("DDR4"),
                            0x1B => Some("LPDDR"),
                            0x1C => Some("LPDDR2"),
                            0x1D => Some("LPDDR3"),
                            0x1E => Some("LPDDR4"),
                            0x22 => Some("DDR5"),
                            0x23 => Some("LPDDR5"),
                            _ => None,
                        };
                        if let Some(l) = label {
                            return Some(l.to_string());
                        }
                    }
                }
                let mut end = pos + hdr_len;
                while end + 1 < data.len() && !(data[end] == 0 && data[end + 1] == 0) {
                    end += 1;
                }
                if end + 2 > data.len() {
                    break;
                }
                pos = end + 2;
            }
            None
        }

        fn disk_probe() -> (String, Option<u64>, Option<u64>, Option<String>) {
            let sys_drive = std::env::var("SYSTEMDRIVE")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "C:".to_string());
            let label = sys_drive.trim_end_matches('\\').to_string();
            let root = format!("{}\\", label);
            let root_w = to_wide_nul(&root);
            let mut avail: u64 = 0;
            let mut total: u64 = 0;
            let mut free: u64 = 0;
            // SAFETY: `root_w` is NUL-terminated UTF-16 and output pointers are valid.
            let ok =
                // SAFETY: Win32/POSIX FFI pointers and output buffers are valid and properly sized.
                unsafe { GetDiskFreeSpaceExW(root_w.as_ptr(), &mut avail, &mut total, &mut free) };
            let (used_opt, total_opt) = if ok != 0 && total > 0 {
                (Some(total.saturating_sub(free)), Some(total))
            } else {
                (None, None)
            };

            let dev_path = to_wide_nul(&format!(r"\\.\{}", label));
            // SAFETY: Opens volume handle with 0 desired access (metadata query only).
            let h = unsafe {
                CreateFileW(
                    dev_path.as_ptr(),
                    0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    0,
                )
            };
            let media = if h != 0 && h != -1 {
                let _guard = HandleGuard(h);
                let query: [u32; 3] = [0, 0, 0]; // StorageDeviceProperty, PropertyStandardQuery
                let mut out = [0u8; 256];
                let mut returned: u32 = 0;
                // SAFETY: `query` (12B) and `out` (256B) are valid buffers for `IOCTL_STORAGE_QUERY_PROPERTY`.
                let ok_io = unsafe {
                    DeviceIoControl(
                        h,
                        IOCTL_STORAGE_QUERY_PROPERTY,
                        query.as_ptr() as *const c_void,
                        std::mem::size_of_val(&query) as u32,
                        out.as_mut_ptr() as *mut c_void,
                        out.len() as u32,
                        &mut returned,
                        std::ptr::null_mut(),
                    )
                };
                if ok_io != 0 && returned >= 32 {
                    let bus_type = u32::from_le_bytes([out[28], out[29], out[30], out[31]]);
                    match bus_type {
                        17 => Some("NVMe".to_string()),
                        11 => Some("SATA".to_string()),
                        10 => Some("SAS".to_string()),
                        7 => Some("USB".to_string()),
                        _ => None,
                    }
                } else {
                    None
                }
            } else {
                None
            };

            (label, used_opt, total_opt, media)
        }

        fn query_power_plan() -> Option<String> {
            let powr = DllGuard::open(b"powrprof.dll\0")?;
            type PowerGetActiveSchemeFn =
                unsafe extern "system" fn(*const c_void, *mut *mut Guid) -> u32;
            type PowerReadFriendlyNameFn = unsafe extern "system" fn(
                *const c_void,
                *const Guid,
                *const Guid,
                *const Guid,
                *mut u8,
                *mut u32,
            ) -> u32;
            // SAFETY: Exact Win32 `powrprof.dll` signatures.
            let (get_scheme, read_name) = unsafe {
                (
                    std::mem::transmute::<*mut c_void, PowerGetActiveSchemeFn>(
                        powr.sym(b"PowerGetActiveScheme\0")?,
                    ),
                    std::mem::transmute::<*mut c_void, PowerReadFriendlyNameFn>(
                        powr.sym(b"PowerReadFriendlyName\0")?,
                    ),
                )
            };
            let mut guid_ptr: *mut Guid = std::ptr::null_mut();
            // SAFETY: `guid_ptr` receives a `LocalAlloc`'d GUID pointer freed via `LocalFree`.
            if unsafe { get_scheme(std::ptr::null(), &mut guid_ptr) } != 0 || guid_ptr.is_null() {
                return None;
            }
            struct GuidGuard(*mut Guid);
            impl Drop for GuidGuard {
                fn drop(&mut self) {
                    if !self.0.is_null() {
                        // SAFETY: `self.0` was allocated by `PowerGetActiveScheme`.
                        unsafe {
                            let _ = LocalFree(self.0 as *mut c_void);
                        }
                    }
                }
            }
            let _guard = GuidGuard(guid_ptr);

            let mut size: u32 = 0;
            // SAFETY: Query UTF-16 friendly-name buffer size.
            let rc = unsafe {
                read_name(
                    std::ptr::null(),
                    guid_ptr,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    &mut size,
                )
            };
            if (rc != 0 && rc != 234) || !(2..=4096).contains(&size) {
                return None;
            }
            let mut buf = vec![0u8; size as usize];
            // SAFETY: `buf` is allocated to exact queried byte size.
            if unsafe {
                read_name(
                    std::ptr::null(),
                    guid_ptr,
                    std::ptr::null(),
                    std::ptr::null(),
                    buf.as_mut_ptr(),
                    &mut size,
                )
            } != 0
            {
                return None;
            }
            buf.truncate(size as usize);
            let mut units = Vec::with_capacity(buf.len() / 2);
            for &[b0, b1] in buf.as_chunks::<2>().0 {
                units.push(u16::from_le_bytes([b0, b1]));
            }
            let s = from_wide_nul(&units);
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }

        fn query_battery_details(snap: &mut HostSnapshot) {
            // First fill baseline from CallNtPowerInformation(SystemBatteryState).
            if let Some(powr) = DllGuard::open(b"powrprof.dll\0") {
                if let Some(p) = powr.sym(b"CallNtPowerInformation\0") {
                    type CallNtPowerFn = unsafe extern "system" fn(
                        i32,
                        *const c_void,
                        u32,
                        *mut SystemBatteryState,
                        u32,
                    ) -> i32;
                    // SAFETY: `SystemBatteryState` matches information level 5.
                    let func = unsafe { std::mem::transmute::<*mut c_void, CallNtPowerFn>(p) };
                    let mut st = SystemBatteryState {
                        ac_on_line: 0,
                        battery_present: 0,
                        charging: 0,
                        discharging: 0,
                        spare1: [0; 3],
                        tag: 0,
                        max_capacity: 0,
                        remaining_capacity: 0,
                        rate: 0,
                        estimated_time: 0,
                        default_alert1: 0,
                        default_alert2: 0,
                    };
                    // SAFETY: Output pointer and size match `SystemBatteryState`.
                    let status = unsafe {
                        func(
                            5,
                            std::ptr::null(),
                            0,
                            &mut st,
                            std::mem::size_of::<SystemBatteryState>() as u32,
                        )
                    };
                    if status == 0 && st.battery_present != 0 {
                        if st.max_capacity > 0 && st.max_capacity != u32::MAX {
                            snap.battery_full_mwh = Some(st.max_capacity);
                        }
                        if st.rate != i32::MIN {
                            snap.battery_rate_mw = Some(st.rate);
                        }
                    }
                }
            }

            // Query SetupAPI battery interface for design capacity, cycle count, voltage, chemistry.
            let Some(setup) = DllGuard::open(b"setupapi.dll\0") else {
                return;
            };
            type SetupDiGetClassDevsWFn =
                unsafe extern "system" fn(*const Guid, *const u16, *mut c_void, u32) -> isize;
            type SetupDiEnumDeviceInterfacesFn = unsafe extern "system" fn(
                isize,
                *const c_void,
                *const Guid,
                u32,
                *mut SpDeviceInterfaceData,
            ) -> i32;
            type SetupDiGetDeviceInterfaceDetailWFn = unsafe extern "system" fn(
                isize,
                *const SpDeviceInterfaceData,
                *mut u8,
                u32,
                *mut u32,
                *mut c_void,
            ) -> i32;
            type SetupDiDestroyDeviceInfoListFn = unsafe extern "system" fn(isize) -> i32;

            // SAFETY: Exact Win32 `setupapi.dll` function signatures.
            let (get_devs, enum_if, get_detail, destroy_list) = unsafe {
                let Some(p1) = setup.sym(b"SetupDiGetClassDevsW\0") else {
                    return;
                };
                let Some(p2) = setup.sym(b"SetupDiEnumDeviceInterfaces\0") else {
                    return;
                };
                let Some(p3) = setup.sym(b"SetupDiGetDeviceInterfaceDetailW\0") else {
                    return;
                };
                let Some(p4) = setup.sym(b"SetupDiDestroyDeviceInfoList\0") else {
                    return;
                };
                (
                    std::mem::transmute::<*mut c_void, SetupDiGetClassDevsWFn>(p1),
                    std::mem::transmute::<*mut c_void, SetupDiEnumDeviceInterfacesFn>(p2),
                    std::mem::transmute::<*mut c_void, SetupDiGetDeviceInterfaceDetailWFn>(p3),
                    std::mem::transmute::<*mut c_void, SetupDiDestroyDeviceInfoListFn>(p4),
                )
            };

            let guid_battery = Guid {
                data1: 0x7263_1E54,
                data2: 0x78A4,
                data3: 0x11D0,
                data4: [0xBC, 0xF7, 0x00, 0xAA, 0x00, 0xB7, 0xB3, 0x2A],
            };
            // SAFETY: `guid_battery` is valid GUID_DEVCLASS_BATTERY.
            let hdev = unsafe {
                get_devs(
                    &guid_battery,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
                )
            };
            if hdev == 0 || hdev == -1 {
                return;
            }
            struct DevInfoGuard(isize, SetupDiDestroyDeviceInfoListFn);
            impl Drop for DevInfoGuard {
                fn drop(&mut self) {
                    // SAFETY: `self.0` was returned by `SetupDiGetClassDevsW`.
                    unsafe {
                        let _ = (self.1)(self.0);
                    }
                }
            }
            let _dev_guard = DevInfoGuard(hdev, destroy_list);

            let mut did = SpDeviceInterfaceData {
                cb_size: std::mem::size_of::<SpDeviceInterfaceData>() as u32,
                interface_class_guid: Guid {
                    data1: 0,
                    data2: 0,
                    data3: 0,
                    data4: [0; 8],
                },
                flags: 0,
                reserved: 0,
            };
            // SAFETY: `did.cb_size` is initialized to exact struct size.
            if unsafe { enum_if(hdev, std::ptr::null(), &guid_battery, 0, &mut did) } == 0 {
                return;
            }
            let mut req_size: u32 = 0;
            // SAFETY: Queries required buffer size for device interface detail.
            let _ = unsafe {
                get_detail(
                    hdev,
                    &did,
                    std::ptr::null_mut(),
                    0,
                    &mut req_size,
                    std::ptr::null_mut(),
                )
            };
            let header_size: u32 = if std::mem::size_of::<usize>() == 8 {
                8
            } else {
                6
            };
            if req_size <= header_size || req_size > 4096 {
                return;
            }
            #[repr(C, align(8))]
            struct AlignedBuf([u8; 4096]);
            let mut detail_buf = AlignedBuf([0u8; 4096]);
            detail_buf.0[..4].copy_from_slice(&header_size.to_le_bytes());
            // SAFETY: `detail_buf` is 8-byte aligned and sized >= `req_size`.
            if unsafe {
                get_detail(
                    hdev,
                    &did,
                    detail_buf.0.as_mut_ptr(),
                    req_size,
                    &mut req_size,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return;
            }
            // DevicePath starts at byte offset 4 (`SP_DEVICE_INTERFACE_DETAIL_DATA_W.DevicePath`).
            let path_bytes = &detail_buf.0[4..(req_size as usize).min(4096)];
            let mut path_w = Vec::with_capacity(path_bytes.len() / 2 + 1);
            for &[b0, b1] in path_bytes.as_chunks::<2>().0 {
                let u = u16::from_le_bytes([b0, b1]);
                path_w.push(u);
                if u == 0 {
                    break;
                }
            }
            if *path_w.last().unwrap_or(&1) != 0 {
                path_w.push(0);
            }
            // SAFETY: `path_w` is a NUL-terminated UTF-16 device interface path.
            let hbat = unsafe {
                CreateFileW(
                    path_w.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    0,
                )
            };
            if hbat == 0 || hbat == -1 {
                return;
            }
            let _bat_guard = HandleGuard(hbat);

            let wait: u32 = 0;
            let mut tag: u32 = 0;
            let mut ret: u32 = 0;
            // SAFETY: `wait` and `tag` are stack-allocated 4-byte integers.
            if unsafe {
                DeviceIoControl(
                    hbat,
                    IOCTL_BATTERY_QUERY_TAG,
                    &wait as *const u32 as *const c_void,
                    4,
                    &mut tag as *mut u32 as *mut c_void,
                    4,
                    &mut ret,
                    std::ptr::null_mut(),
                )
            } == 0
                || tag == 0
            {
                return;
            }

            let bqi = BatteryQueryInformation {
                battery_tag: tag,
                information_level: 0, // BatteryInformation
                at_rate: 0,
            };
            let mut info = BatteryInformation {
                capabilities: 0,
                technology: 0,
                reserved: [0; 3],
                chemistry: [0; 4],
                designed_capacity: 0,
                full_charged_capacity: 0,
                default_alert1: 0,
                default_alert2: 0,
                critical_bias: 0,
                cycle_count: 0,
            };
            // SAFETY: Input/output structs match `IOCTL_BATTERY_QUERY_INFORMATION`.
            if unsafe {
                DeviceIoControl(
                    hbat,
                    IOCTL_BATTERY_QUERY_INFORMATION,
                    &bqi as *const BatteryQueryInformation as *const c_void,
                    std::mem::size_of::<BatteryQueryInformation>() as u32,
                    &mut info as *mut BatteryInformation as *mut c_void,
                    std::mem::size_of::<BatteryInformation>() as u32,
                    &mut ret,
                    std::ptr::null_mut(),
                )
            } != 0
            {
                if info.designed_capacity > 0 && info.designed_capacity != u32::MAX {
                    snap.battery_design_mwh = Some(info.designed_capacity);
                }
                if info.full_charged_capacity > 0 && info.full_charged_capacity != u32::MAX {
                    snap.battery_full_mwh = Some(info.full_charged_capacity);
                }
                if info.cycle_count > 0 && info.cycle_count != u32::MAX {
                    snap.battery_cycles = Some(info.cycle_count);
                }
                let chem = String::from_utf8_lossy(&info.chemistry)
                    .trim_matches('\0')
                    .trim()
                    .to_string();
                let mapped_chem = match chem.to_ascii_uppercase().as_str() {
                    "LION" => Some("Li-Ion".to_string()),
                    "LIP" => Some("Li-Poly".to_string()),
                    "NIMH" => Some("NiMH".to_string()),
                    _ if !chem.is_empty() => Some(chem),
                    _ => None,
                };
                if mapped_chem.is_some() {
                    snap.battery_chemistry = mapped_chem;
                }
            }

            let bws = BatteryWaitStatus {
                battery_tag: tag,
                timeout: 0,
                power_state: 0,
                low_capacity: 0,
                high_capacity: 0,
            };
            let mut bst = BatteryStatus {
                power_state: 0,
                capacity: 0,
                voltage: 0,
                rate: 0,
            };
            // SAFETY: Input/output structs match `IOCTL_BATTERY_QUERY_STATUS`.
            if unsafe {
                DeviceIoControl(
                    hbat,
                    IOCTL_BATTERY_QUERY_STATUS,
                    &bws as *const BatteryWaitStatus as *const c_void,
                    std::mem::size_of::<BatteryWaitStatus>() as u32,
                    &mut bst as *mut BatteryStatus as *mut c_void,
                    std::mem::size_of::<BatteryStatus>() as u32,
                    &mut ret,
                    std::ptr::null_mut(),
                )
            } != 0
            {
                if bst.voltage > 0 && bst.voltage != u32::MAX {
                    snap.battery_voltage_mv = Some(bst.voltage);
                }
                if bst.rate != i32::MIN {
                    snap.battery_rate_mw = Some(bst.rate);
                }
            }
        }

        fn network_probe() -> (Option<String>, Option<String>) {
            let route_ip = super::udp_route_local_ipv4();
            let Some(iphlp) = DllGuard::open(b"iphlpapi.dll\0") else {
                return (route_ip, None);
            };
            let Some(p) = iphlp.sym(b"GetAdaptersAddresses\0") else {
                return (route_ip, None);
            };
            type GetAdaptersAddressesFn =
                unsafe extern "system" fn(u32, u32, *mut c_void, *mut u8, *mut u32) -> u32;
            // SAFETY: Exact Win32 `iphlpapi.dll` signature.
            let func = unsafe { std::mem::transmute::<*mut c_void, GetAdaptersAddressesFn>(p) };
            let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
            let mut size: u32 = 16 * 1024;
            let mut buf = vec![0u8; size as usize];
            // SAFETY: `buf` holds `size` bytes; `GetAdaptersAddresses` writes required size on overflow.
            let mut rc = unsafe {
                func(
                    AF_INET_U32,
                    flags,
                    std::ptr::null_mut(),
                    buf.as_mut_ptr(),
                    &mut size,
                )
            };
            if rc == 111 && size <= 512 * 1024 {
                // ERROR_BUFFER_OVERFLOW
                buf.resize(size as usize, 0);
                // SAFETY: `buf` has exact `size` returned above.
                rc = unsafe {
                    func(
                        AF_INET_U32,
                        flags,
                        std::ptr::null_mut(),
                        buf.as_mut_ptr(),
                        &mut size,
                    )
                };
            }
            if rc != 0 {
                return (route_ip, None);
            }

            let mut best_ip: Option<String> = None;
            let mut best_link: Option<String> = None;
            let mut best_score: i32 = -1;

            let mut cur = buf.as_ptr() as *const IpAdapterAddresses;
            let mut steps = 0;
            while !cur.is_null() && steps < 128 {
                steps += 1;
                // SAFETY: `cur` points within `buf` populated by `GetAdaptersAddresses`.
                let ad = unsafe { &*cur };
                if ad.oper_status == IF_OPER_STATUS_UP && ad.if_type != IF_TYPE_SOFTWARE_LOOPBACK {
                    let mut ua = ad.first_unicast_address as *const IpAdapterUnicastAddress;
                    let mut usteps = 0;
                    while !ua.is_null() && usteps < 16 {
                        usteps += 1;
                        // SAFETY: `ua` points to a valid `IpAdapterUnicastAddress` node in `buf`.
                        let ua_ref = unsafe { &*ua };
                        let sa = ua_ref.address.lp_sockaddr;
                        if !sa.is_null() && ua_ref.address.i_sockaddr_length >= 8 {
                            // SAFETY: `sa` points to at least `sizeof(SockAddrIn)` bytes inside `buf`.
                            let sin = unsafe { &*sa };
                            if sin.sin_family as u32 == AF_INET_U32 {
                                let [a, b, c, d] = sin.sin_addr;
                                let ip_str = format!("{a}.{b}.{c}.{d}");
                                if a != 127 && !(a == 169 && b == 254) {
                                    let mut score = if Some(&ip_str) == route_ip.as_ref() {
                                        100
                                    } else {
                                        10
                                    };
                                    let kind = match ad.if_type {
                                        IF_TYPE_IEEE80211 => {
                                            score += 5;
                                            Some("Wi-Fi")
                                        }
                                        IF_TYPE_ETHERNET => {
                                            score += 4;
                                            Some("Ethernet")
                                        }
                                        IF_TYPE_PPP => Some("PPP"),
                                        _ => None,
                                    };
                                    if score > best_score {
                                        best_score = score;
                                        best_ip = Some(ip_str);
                                        best_link = match (kind, ad.transmit_link_speed) {
                                            (Some(k), spd) if spd > 0 && spd != u64::MAX => {
                                                let mbps = spd / 1_000_000;
                                                if mbps >= 1000 {
                                                    Some(format!(
                                                        "{k} ({:.1} Gbps)",
                                                        spd as f64 / 1_000_000_000.0
                                                    ))
                                                } else if mbps > 0 {
                                                    Some(format!("{k} ({mbps} Mbps)"))
                                                } else {
                                                    Some(k.to_string())
                                                }
                                            }
                                            (Some(k), _) => Some(k.to_string()),
                                            (None, _) => None,
                                        };
                                    }
                                }
                            }
                        }
                        ua = ua_ref.next;
                    }
                }
                cur = ad.next;
            }
            (best_ip.or(route_ip), best_link)
        }

        fn timezone_and_locale(k32: Option<&DllGuard>) -> (Option<String>, Option<i32>, String) {
            let mut iana: Option<String> = None;
            let mut offset_mins: Option<i32> = None;
            let mut locale = String::from("en-US");

            if let Some(k32) = k32 {
                if let Some(p) = k32.sym(b"GetDynamicTimeZoneInformation\0") {
                    type GetDtzFn =
                        unsafe extern "system" fn(*mut DynamicTimeZoneInformation) -> u32;
                    // SAFETY: Exact Win32 `GetDynamicTimeZoneInformation` signature.
                    let func = unsafe { std::mem::transmute::<*mut c_void, GetDtzFn>(p) };
                    let mut dtzi = DynamicTimeZoneInformation {
                        bias: 0,
                        standard_name: [0; 32],
                        standard_date: SystemTime {
                            year: 0,
                            month: 0,
                            day_of_week: 0,
                            day: 0,
                            hour: 0,
                            minute: 0,
                            second: 0,
                            milliseconds: 0,
                        },
                        standard_bias: 0,
                        daylight_name: [0; 32],
                        daylight_date: SystemTime {
                            year: 0,
                            month: 0,
                            day_of_week: 0,
                            day: 0,
                            hour: 0,
                            minute: 0,
                            second: 0,
                            milliseconds: 0,
                        },
                        daylight_bias: 0,
                        time_zone_key_name: [0; 128],
                        dynamic_daylight_time_disabled: 0,
                    };
                    // SAFETY: `dtzi` is zero-initialized and sized properly.
                    let tz_id = unsafe { func(&mut dtzi) };
                    if tz_id != u32::MAX {
                        let extra = match tz_id {
                            2 => dtzi.daylight_bias,
                            1 => dtzi.standard_bias,
                            _ => 0,
                        };
                        offset_mins = Some(-(dtzi.bias + extra));
                        let key = from_wide_nul(&dtzi.time_zone_key_name);
                        if !key.is_empty() {
                            iana = Some(
                                super::super::neofetch::windows_tz_to_iana(&key)
                                    .map(|s| s.to_string())
                                    .unwrap_or(key),
                            );
                        }
                    }
                }
                if let Some(p) = k32.sym(b"GetUserDefaultLocaleName\0") {
                    type GetLocaleFn = unsafe extern "system" fn(*mut u16, i32) -> i32;
                    // SAFETY: Exact Win32 `GetUserDefaultLocaleName` signature.
                    let func = unsafe { std::mem::transmute::<*mut c_void, GetLocaleFn>(p) };
                    let mut loc_buf = [0u16; 85];
                    // SAFETY: `loc_buf` holds up to 85 UTF-16 code units (LOCALE_NAME_MAX_LENGTH).
                    let len = unsafe { func(loc_buf.as_mut_ptr(), loc_buf.len() as i32) };
                    if len > 0 {
                        let s = from_wide_nul(&loc_buf);
                        if !s.is_empty() {
                            locale = s;
                        }
                    }
                }
            }
            (iana, offset_mins, locale)
        }

        pub(super) fn collect() -> HostSnapshot {
            let k32 = DllGuard::open(b"kernel32.dll\0");
            let adv = DllGuard::open(b"advapi32.dll\0");

            // Take an initial CPU sample before inspecting other subsystems so we avoid
            // adding an extra artificial wait unless elapsed probe time < 20ms.
            let t0 = sample_cpu_times();

            let mut snap = HostSnapshot {
                username: super::super::whoami(),
                hostname: super::super::hostname(),
                elevated: is_elevated(adv.as_ref()),
                ..HostSnapshot::default()
            };

            let nt_key = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
            let prod = adv
                .as_ref()
                .and_then(|a| reg_str(a, nt_key, "ProductName"))
                .unwrap_or_else(|| "Windows".to_string());
            let disp_ver = adv
                .as_ref()
                .and_then(|a| reg_str(a, nt_key, "DisplayVersion"));
            let build_str = adv
                .as_ref()
                .and_then(|a| reg_str(a, nt_key, "CurrentBuildNumber"));
            let major = adv
                .as_ref()
                .and_then(|a| reg_u32(a, nt_key, "CurrentMajorVersionNumber"))
                .unwrap_or(10);
            let minor = adv
                .as_ref()
                .and_then(|a| reg_u32(a, nt_key, "CurrentMinorVersionNumber"))
                .unwrap_or(0);
            let build_num = build_str
                .as_deref()
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            let norm_prod =
                super::super::neofetch::normalize_windows_product_name(&prod, build_num);
            snap.os_display = match (disp_ver.as_deref(), build_str.as_deref()) {
                (Some(dv), Some(b)) => format!("{norm_prod} {dv} (Build {b})"),
                (None, Some(b)) => format!("{norm_prod} (Build {b})"),
                _ => norm_prod,
            };

            let bios_key = r"HARDWARE\DESCRIPTION\System\BIOS";
            if let Some(a) = adv.as_ref() {
                let mfg = reg_str(a, bios_key, "SystemManufacturer")
                    .or_else(|| reg_str(a, bios_key, "BaseBoardManufacturer"));
                let model = reg_str(a, bios_key, "SystemProductName")
                    .or_else(|| reg_str(a, bios_key, "BaseBoardProduct"));
                snap.host =
                    super::super::neofetch::clean_host_name(mfg.as_deref(), model.as_deref());
            }

            let build_display = build_str.as_deref().unwrap_or("0");
            snap.kernel = format!(
                "NT {major}.{minor}.{build_display} ({})",
                std::env::consts::ARCH
            );
            snap.firmware_boot = firmware_boot(k32.as_ref(), adv.as_ref());

            // SAFETY: `GetTickCount64` takes no parameters and cannot fail.
            snap.uptime_secs = Some(unsafe { GetTickCount64() } / 1000);
            let (procs, threads, commit_info) = perf_counts(k32.as_ref());
            snap.procs = procs;
            snap.threads = threads;

            snap.shell = detect_parent_shell();
            snap.terminal = detect_terminal();
            if let Some((c, r)) = console_dimensions() {
                snap.term_cols = Some(c);
                snap.term_rows = Some(r);
            }
            snap.display = display_info();

            let cpu_key = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0";
            let raw_cpu = adv
                .as_ref()
                .and_then(|a| reg_str(a, cpu_key, "ProcessorNameString"))
                .unwrap_or_else(|| std::env::consts::ARCH.to_string());
            snap.cpu_brand = super::super::neofetch::clean_cpu_name(&raw_cpu);
            snap.cpu_mhz = adv.as_ref().and_then(|a| reg_u32(a, cpu_key, "~MHz"));
            let (phys, l3) = cpu_topology(k32.as_ref());
            snap.cpu_phys_cores = phys;
            snap.cpu_log_cores = std::thread::available_parallelism()
                .ok()
                .map(|n| n.get() as u32);
            snap.cpu_l3_bytes = l3;

            snap.gpu = gpu_probe(adv.as_ref());

            let mut ms = MemoryStatusEx {
                dw_length: std::mem::size_of::<MemoryStatusEx>() as u32,
                dw_memory_load: 0,
                ull_total_phys: 0,
                ull_avail_phys: 0,
                ull_total_page: 0,
                ull_avail_page: 0,
                ull_total_virtual: 0,
                ull_avail_virtual: 0,
                ull_avail_extended_virtual: 0,
            };
            // SAFETY: `ms` is valid and `dw_length` equals `size_of::<MemoryStatusEx>()`.
            if unsafe { GlobalMemoryStatusEx(&mut ms) } != 0 {
                let mem_used = ms.ull_total_phys.saturating_sub(ms.ull_avail_phys);
                snap.mem_total = Some(ms.ull_total_phys);
                snap.mem_used = Some(mem_used);
                let page_only_total = ms.ull_total_page.saturating_sub(ms.ull_total_phys);
                if page_only_total > 0 {
                    let commit_used = ms.ull_total_page.saturating_sub(ms.ull_avail_page);
                    let page_used = commit_used.saturating_sub(mem_used).min(page_only_total);
                    snap.swap_total = Some(page_only_total);
                    snap.swap_used = Some(page_used);
                } else if let Some((c_used, c_limit)) = commit_info {
                    snap.swap_total = Some(c_limit);
                    snap.swap_used = Some(c_used.min(c_limit));
                }
            }
            snap.mem_type = smbios_mem_type(k32.as_ref());

            let (d_lbl, d_used, d_total, d_media) = disk_probe();
            snap.disk_label = d_lbl;
            snap.disk_used = d_used;
            snap.disk_total = d_total;
            snap.disk_media = d_media;

            let mut sps = SystemPowerStatus {
                ac_line_status: 255,
                battery_flag: 255,
                battery_life_percent: 255,
                system_status_flag: 0,
                battery_life_time: u32::MAX,
                battery_full_life_time: u32::MAX,
            };
            // SAFETY: `sps` is valid stack storage.
            if unsafe { GetSystemPowerStatus(&mut sps) } != 0 {
                snap.ac_online = match sps.ac_line_status {
                    1 => Some(true),
                    0 => Some(false),
                    _ => None,
                };
                let no_bat = (sps.battery_flag & 128) != 0 || sps.battery_flag == 255;
                snap.has_battery = !no_bat;
                if snap.has_battery {
                    if sps.battery_life_percent <= 100 {
                        snap.battery_pct = Some(sps.battery_life_percent);
                    }
                    snap.battery_charging = (sps.battery_flag & 8) != 0;
                    if sps.battery_life_time != u32::MAX {
                        snap.battery_secs_left = Some(sps.battery_life_time);
                    }
                    query_battery_details(&mut snap);
                }
            }
            snap.power_plan = query_power_plan();

            let (ip, link) = network_probe();
            snap.local_ip = ip;
            snap.link_info = link;

            let (iana, utc_mins, loc) = timezone_and_locale(k32.as_ref());
            snap.timezone_iana = iana;
            snap.utc_offset_mins = utc_mins;
            snap.locale = format!("{loc} [UTF-8]");

            // Ensure a small ~18ms sampling window so `GetSystemTimes` reports real CPU load.
            thread::sleep(Duration::from_millis(18));
            if let (Some((i0, t0)), Some((i1, t1))) = (t0, sample_cpu_times()) {
                let dt = t1.saturating_sub(t0);
                let di = i1.saturating_sub(i0);
                if dt > 0 {
                    let busy = dt.saturating_sub(di);
                    let pct = ((busy * 100) + dt / 2) / dt;
                    snap.cpu_load_pct = Some(pct.min(100) as u8);
                }
            }
            snap
        }
    }

    #[cfg(all(not(target_os = "windows"), not(target_arch = "wasm32")))]
    mod unix_impl {
        use super::HostSnapshot;
        #[cfg(target_os = "linux")]
        use std::collections::HashSet;
        use std::ffi::{c_char, c_ulong};
        #[cfg(target_os = "linux")]
        use std::thread;
        #[cfg(target_os = "linux")]
        use std::time::Duration;

        #[repr(C)]
        struct Winsize {
            ws_row: u16,
            ws_col: u16,
            ws_xpixel: u16,
            ws_ypixel: u16,
        }

        #[cfg(any(target_os = "linux", target_os = "android"))]
        const TIOCGWINSZ_REQ: c_ulong = 0x5413;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        const TIOCGWINSZ_REQ: c_ulong = 0x4008_7468;

        #[cfg(target_os = "macos")]
        #[repr(C)]
        struct StatVfs {
            f_bsize: c_ulong,
            f_frsize: c_ulong,
            f_blocks: u32,
            f_bfree: u32,
            f_bavail: u32,
            f_files: u32,
            f_ffree: u32,
            f_favail: u32,
            f_fsid: c_ulong,
            f_flag: c_ulong,
            f_namemax: c_ulong,
        }

        #[cfg(not(target_os = "macos"))]
        #[repr(C)]
        struct StatVfs {
            f_bsize: c_ulong,
            f_frsize: c_ulong,
            f_blocks: u64,
            f_bfree: u64,
            f_bavail: u64,
            f_files: u64,
            f_ffree: u64,
            f_favail: u64,
            f_fsid: c_ulong,
            f_flag: c_ulong,
            f_namemax: c_ulong,
            #[cfg(target_pointer_width = "32")]
            _pad: [u32; 12],
            #[cfg(target_pointer_width = "64")]
            _pad: [i32; 6],
        }

        extern "C" {
            fn geteuid() -> u32;
            fn ioctl(fd: i32, request: c_ulong, ...) -> i32;
            fn statvfs(path: *const c_char, buf: *mut StatVfs) -> i32;
        }

        #[cfg(target_os = "linux")]
        fn read_proc_cpu_sample() -> Option<(u64, u64)> {
            let text = std::fs::read_to_string("/proc/stat").ok()?;
            let line = text.lines().next()?;
            let mut nums = line
                .split_whitespace()
                .skip(1)
                .filter_map(|s| s.parse::<u64>().ok());
            let user = nums.next()?;
            let nice = nums.next()?;
            let system = nums.next()?;
            let idle = nums.next()?;
            let iowait = nums.next().unwrap_or(0);
            let irq = nums.next().unwrap_or(0);
            let softirq = nums.next().unwrap_or(0);
            let steal = nums.next().unwrap_or(0);
            let total = user + nice + system + idle + iowait + irq + softirq + steal;
            Some((idle + iowait, total))
        }

        fn tty_size() -> Option<(u16, u16)> {
            let mut ws = Winsize {
                ws_row: 0,
                ws_col: 0,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            // SAFETY: `ioctl` writes a valid `Winsize` struct on stdout fd 1.
            if unsafe { ioctl(1, TIOCGWINSZ_REQ, &mut ws) } == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
                Some((ws.ws_col, ws.ws_row))
            } else {
                None
            }
        }

        fn root_disk() -> (Option<u64>, Option<u64>) {
            let cpath = c"/";
            // SAFETY: `StatVfs` is a plain C POD struct where zero initialization is valid.
            let mut st: StatVfs = unsafe { std::mem::zeroed() };
            // SAFETY: `cpath` is valid NUL-terminated `"/"` and `st` is properly sized.
            if unsafe { statvfs(cpath.as_ptr(), &mut st) } == 0 && st.f_blocks > 0 {
                #[allow(clippy::unnecessary_cast)]
                let unit = if st.f_frsize > 0 {
                    st.f_frsize as u64
                } else {
                    st.f_bsize as u64
                };
                #[allow(clippy::unnecessary_cast)]
                let total = (st.f_blocks as u64).saturating_mul(unit);
                #[allow(clippy::unnecessary_cast)]
                let avail = (st.f_bfree as u64).saturating_mul(unit);
                (Some(total.saturating_sub(avail)), Some(total))
            } else {
                (None, None)
            }
        }

        pub(super) fn collect() -> HostSnapshot {
            #[cfg(target_os = "linux")]
            let cpu0 = read_proc_cpu_sample();

            // SAFETY: `geteuid()` is always safe to call.
            let elevated = unsafe { geteuid() == 0 };
            let shell = std::env::var("SHELL")
                .ok()
                .and_then(|s| {
                    std::path::Path::new(&s)
                        .file_name()
                        .and_then(|f| f.to_str())
                        .map(|n| n.to_string())
                })
                .unwrap_or_else(|| "sh".to_string());
            let terminal = std::env::var("TERM_PROGRAM")
                .ok()
                .filter(|s| !s.is_empty())
                .or_else(|| std::env::var("TERM").ok().filter(|s| !s.is_empty()))
                .unwrap_or_else(|| "tty".to_string());
            let (term_cols, term_rows) = match tty_size() {
                Some((c, r)) => (Some(c), Some(r)),
                None => (None, None),
            };

            let mut snap = HostSnapshot {
                username: super::super::whoami(),
                hostname: super::super::hostname(),
                elevated,
                shell,
                terminal,
                term_cols,
                term_rows,
                cpu_log_cores: std::thread::available_parallelism()
                    .ok()
                    .map(|n| n.get() as u32),
                disk_label: "/".to_string(),
                local_ip: super::udp_route_local_ipv4(),
                ..HostSnapshot::default()
            };

            let (du, dt) = root_disk();
            snap.disk_used = du;
            snap.disk_total = dt;

            let tz_iana = std::env::var("TZ")
                .ok()
                .filter(|s| s.contains('/') && !s.starts_with(':'))
                .or_else(|| {
                    std::fs::read_to_string("/etc/timezone")
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                })
                .or_else(|| {
                    std::fs::read_link("/etc/localtime").ok().and_then(|p| {
                        let s = p.to_string_lossy();
                        s.split_once("zoneinfo/").map(|(_, rest)| rest.to_string())
                    })
                });
            snap.timezone_iana = tz_iana;
            let raw_loc = std::env::var("LC_ALL")
                .or_else(|_| std::env::var("LANG"))
                .unwrap_or_else(|_| "en-US.UTF-8".to_string());
            snap.locale = raw_loc
                .split('.')
                .next()
                .unwrap_or("en-US")
                .replace('_', "-")
                + " [UTF-8]";

            #[cfg(target_os = "linux")]
            {
                snap.os_display = std::fs::read_to_string("/etc/os-release")
                    .ok()
                    .and_then(|txt| {
                        txt.lines()
                            .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                            .map(|v| v.trim_matches('"').to_string())
                    })
                    .unwrap_or_else(|| "Linux".to_string());
                let rel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| "Linux".to_string());
                snap.kernel = format!("Linux {rel} ({})", std::env::consts::ARCH);
                if std::path::Path::new("/sys/firmware/efi").exists() {
                    snap.firmware_boot = Some("UEFI".to_string());
                }
                let vendor = std::fs::read_to_string("/sys/devices/virtual/dmi/id/sys_vendor").ok();
                let product =
                    std::fs::read_to_string("/sys/devices/virtual/dmi/id/product_name").ok();
                snap.host =
                    super::super::neofetch::clean_host_name(vendor.as_deref(), product.as_deref());
                if let Ok(up) = std::fs::read_to_string("/proc/uptime") {
                    if let Some(secs) = up
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse::<f64>().ok())
                    {
                        snap.uptime_secs = Some(secs as u64);
                    }
                }
                if let Ok(rd) = std::fs::read_dir("/proc") {
                    let count = rd
                        .flatten()
                        .filter(|e| {
                            e.file_name().to_str().is_some_and(|n| {
                                !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
                            })
                        })
                        .count();
                    if count > 0 {
                        snap.procs = Some(count as u32);
                    }
                }
                if let Ok(la) = std::fs::read_to_string("/proc/loadavg") {
                    if let Some((_, tot)) =
                        la.split_whitespace().nth(3).and_then(|s| s.split_once('/'))
                    {
                        if let Ok(n) = tot.parse::<u32>() {
                            snap.threads = Some(n);
                        }
                    }
                }
                if let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo") {
                    let mut phys_cores: HashSet<(String, String)> = HashSet::new();
                    let mut cur_phys = String::new();
                    for line in cpuinfo.lines() {
                        if let Some((k, v)) = line.split_once(':') {
                            let k = k.trim();
                            let v = v.trim();
                            if k == "model name" && snap.cpu_brand.is_empty() {
                                snap.cpu_brand = super::super::neofetch::clean_cpu_name(v);
                            } else if k == "cpu MHz" && snap.cpu_mhz.is_none() {
                                if let Ok(mhz) = v.parse::<f64>() {
                                    snap.cpu_mhz = Some(mhz as u32);
                                }
                            } else if k == "physical id" {
                                cur_phys = v.to_string();
                            } else if k == "core id" {
                                phys_cores.insert((cur_phys.clone(), v.to_string()));
                            }
                        }
                    }
                    if !phys_cores.is_empty() {
                        snap.cpu_phys_cores = Some(phys_cores.len() as u32);
                    }
                }
                if snap.cpu_brand.is_empty() {
                    snap.cpu_brand = std::env::consts::ARCH.to_string();
                }
                if let Ok(rd) = std::fs::read_dir("/sys/class/thermal") {
                    for e in rd.flatten() {
                        let p = e.path().join("temp");
                        if let Ok(raw) = std::fs::read_to_string(p) {
                            if let Ok(mc) = raw.trim().parse::<i32>() {
                                if (10_000..=115_000).contains(&mc) {
                                    snap.cpu_temp_c = Some(mc / 1000);
                                    break;
                                }
                            }
                        }
                    }
                }
                if let Ok(mem) = std::fs::read_to_string("/proc/meminfo") {
                    let get_kb = |key: &str| -> Option<u64> {
                        mem.lines().find_map(|l| {
                            let rest = l.strip_prefix(key)?.trim_start_matches(':').trim();
                            rest.split_whitespace()
                                .next()?
                                .parse::<u64>()
                                .ok()
                                .map(|n| n * 1024)
                        })
                    };
                    if let Some(tot) = get_kb("MemTotal") {
                        let avail = get_kb("MemAvailable")
                            .or_else(|| get_kb("MemFree"))
                            .unwrap_or(0);
                        snap.mem_total = Some(tot);
                        snap.mem_used = Some(tot.saturating_sub(avail));
                    }
                    if let (Some(st), Some(sf)) = (get_kb("SwapTotal"), get_kb("SwapFree")) {
                        if st > 0 {
                            snap.swap_total = Some(st);
                            snap.swap_used = Some(st.saturating_sub(sf));
                        }
                    }
                }
                if let Ok(rd) = std::fs::read_dir("/sys/class/power_supply") {
                    for e in rd.flatten() {
                        let name = e.file_name().to_string_lossy().to_string();
                        let dir = e.path();
                        if name.starts_with("BAT") {
                            snap.has_battery = true;
                            snap.battery_pct = std::fs::read_to_string(dir.join("capacity"))
                                .ok()
                                .and_then(|s| s.trim().parse::<u8>().ok());
                            snap.battery_charging = std::fs::read_to_string(dir.join("status"))
                                .ok()
                                .is_some_and(|s| s.trim().eq_ignore_ascii_case("Charging"));
                            snap.battery_full_mwh =
                                std::fs::read_to_string(dir.join("energy_full"))
                                    .ok()
                                    .and_then(|s| s.trim().parse::<u64>().ok())
                                    .map(|u| (u / 1000) as u32);
                            snap.battery_design_mwh =
                                std::fs::read_to_string(dir.join("energy_full_design"))
                                    .ok()
                                    .and_then(|s| s.trim().parse::<u64>().ok())
                                    .map(|u| (u / 1000) as u32);
                            snap.battery_cycles = std::fs::read_to_string(dir.join("cycle_count"))
                                .ok()
                                .and_then(|s| s.trim().parse::<u32>().ok())
                                .filter(|&c| c > 0);
                            snap.battery_voltage_mv =
                                std::fs::read_to_string(dir.join("voltage_now"))
                                    .ok()
                                    .and_then(|s| s.trim().parse::<u64>().ok())
                                    .map(|u| (u / 1000) as u32);
                            snap.battery_chemistry =
                                std::fs::read_to_string(dir.join("technology"))
                                    .ok()
                                    .map(|s| s.trim().to_string())
                                    .filter(|s| !s.is_empty());
                            break;
                        } else if name.starts_with("AC") || name.starts_with("ADP") {
                            if let Ok(on) = std::fs::read_to_string(dir.join("online")) {
                                snap.ac_online = Some(on.trim() == "1");
                            }
                        }
                    }
                }
                thread::sleep(Duration::from_millis(18));
                if let (Some((i0, t0)), Some((i1, t1))) = (cpu0, read_proc_cpu_sample()) {
                    let dt = t1.saturating_sub(t0);
                    let di = i1.saturating_sub(i0);
                    if dt > 0 {
                        let busy = dt.saturating_sub(di);
                        snap.cpu_load_pct = Some((((busy * 100) + dt / 2) / dt).min(100) as u8);
                    }
                }
            }

            #[cfg(target_os = "macos")]
            {
                snap.os_display = "macOS".to_string();
                snap.kernel = format!("Darwin ({})", std::env::consts::ARCH);
                if snap.cpu_brand.is_empty() {
                    snap.cpu_brand = std::env::consts::ARCH.to_string();
                }
            }

            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                snap.os_display = super::super::os_name();
                snap.kernel = format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH);
                if snap.cpu_brand.is_empty() {
                    snap.cpu_brand = std::env::consts::ARCH.to_string();
                }
            }

            snap
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The phase-3 policy keys reveal host directory layout (basedir) and
    /// the deny list itself, so every agent (remote) surface redacts them
    /// (F-003) — while local/script surfaces keep them readable, because a
    /// script is the subject of the policy and can probe both lists by
    /// observation anyway.
    #[test]
    fn policy_keys_are_redacted_remotely_and_visible_locally() {
        for key in ["security.basedir", "security.disable_natives"] {
            assert!(is_policy_key(key), "{} is a policy key", key);
            // Not credentials: the local matcher does not fire.
            assert!(!is_sensitive_key(key), "{} is not a credential key", key);
            assert_eq!(redact_value(key, "C:/data;C:/tmp"), "C:/data;C:/tmp");
            // Remote surfaces get the stable hash form.
            assert!(
                redact_value_remote(key, "C:/data;C:/tmp").starts_with("[redacted sha256:"),
                "{} must come back hashed remotely, got: {}",
                key,
                redact_value_remote(key, "C:/data;C:/tmp")
            );
        }
        // Case-insensitive key matching, matching the lowercase comparison
        // the matcher uses.
        assert!(is_policy_key("SECURITY.BASEDIR"));

        // Credentials keep working through both levels.
        assert!(is_sensitive_key("agent.shared"));
        assert!(redact_value_remote("agent.shared", "abc").starts_with("[redacted sha256:"));

        // Benign keys stay readable everywhere (regression guard against
        // over-redaction sneaking past the substring list).
        for key in ["packages.registry", "modules.assert_active"] {
            assert!(!is_policy_key(key) && !is_sensitive_key(key));
            assert_eq!(redact_value_remote(key, "D:/reg"), "D:/reg");
        }
    }

    #[test]
    fn diagnostic_redaction_keeps_local_policy_paths_visible() {
        assert_eq!(
            redact_diagnostic("security.basedir: entry '/secret/path' does not exist"),
            "security.basedir: entry '/secret/path' does not exist"
        );
        assert_eq!(
            redact_diagnostic("agent.shared: expected a boolean, found \"secret-token\""),
            "agent.shared: [redacted diagnostic]"
        );
    }

    #[test]
    fn remote_info_omits_notes_and_redacts_installation_paths() {
        let sections = collect_remote(true);
        let configuration = sections
            .iter()
            .find(|section| section.name == "Configuration")
            .expect("configuration section");
        assert!(configuration.rows.iter().all(|(key, _)| key != "notes"));

        let installation = sections
            .iter()
            .find(|section| section.name == "Installation")
            .expect("installation section");
        for (key, value) in &installation.rows {
            assert_eq!(value, "[redacted]", "unexpected remote path row: {key}");
        }
    }
}
