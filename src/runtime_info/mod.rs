//! `spid3rinfo()` — Spid3r's own runtime introspection.
//!
//! One collector, three renderers (HTML, JSON, text), one redaction pass.
//! Scripts call `spid3rinfo()`, `spid3rinfo("html")`, `spid3rinfo("json")`;
//! the CLI calls `spid3r info [--json|--html]`. Sensitive values (tokens,
//! passwords, agent shared keys) are redacted unless the caller is local
//! *and* asked for `--no-redact`.

use std::collections::BTreeMap;

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
    if SENSITIVE_SUBSTR.iter().any(|s| k.contains(s)) {
        return true;
    }
    k.ends_with("key")
        || k.ends_with("_key")
        || k.ends_with("-key")
        || k.ends_with("secret")
        || k.ends_with("token")
}

/// Replace a sensitive value with a stable prefix + hash, so an operator can
/// still see that a value is set and whether two values match.
pub fn redact_value(key: &str, value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if !is_sensitive_key(key) {
        return value.to_string();
    }
    let h = crate::pkg::sha256_hex(value.as_bytes());
    format!("[redacted sha256:{}]", &h[..12])
}

/// Collect every section. `redact` applies to environment and agent state.
pub fn collect(redact: bool) -> Vec<Section> {
    let mut out = Vec::new();
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into());

    let mut s = Section::new("Spid3r");
    s.push("Spid3r version", crate::VERSION);
    s.push("Language version", crate::LANGUAGE_VERSION);
    s.push("Host language", "Rust");
    s.push("Rust compiler", rustc_version());
    s.push(
        "Build profile",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
    );
    s.push("Target triple", std::env::consts::ARCH);
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
        "Logical CPUs",
        std::thread::available_parallelism()
            .map(|n| n.get().to_string())
            .unwrap_or_else(|_| "?".into()),
    );
    out.push(s);

    let mut s = Section::new("Installation");
    s.push("Executable", exe);
    s.push(
        "Working directory",
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "?".into()),
    );
    s.push(
        "Config file",
        config::snapshot()
            .path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none; defaults)".into()),
    );
    s.push(
        "Config directory",
        config::config_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "?".into()),
    );
    s.push("State directory", config::state_dir().display().to_string());
    s.push(
        "Log file",
        crate::logging::path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "?".into()),
    );
    s.push(
        "INI search",
        config::candidate_paths()
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("; "),
    );
    out.push(s);

    let mut s = Section::new("Features");
    s.push("gui", feature(cfg!(feature = "gui")));
    s.push("native (LLVM AOT)", feature(cfg!(feature = "native")));
    s.push("tls", feature(cfg!(feature = "tls")));
    s.push("plugins-wasm", feature(cfg!(feature = "plugins-wasm")));
    s.push("agent", "on");
    s.push("spid3rinfo", "on");
    out.push(s);

    let mut s = Section::new("Modules");
    s.push(
        "Embedded stdlib",
        format!("{} modules", crate::stdlib_embed::names().len()),
    );
    s.push("Plugin ABI", crate::plugin::ABI_V1);
    let plugins = crate::plugin::registered();
    s.push("Host plugin functions", plugins.len().to_string());
    if !plugins.is_empty() {
        s.push(
            "Host plugin names",
            plugins
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    out.push(s);

    let cfg = config::snapshot();
    let mut s = Section::new("Configuration");
    for (k, v) in &cfg.values {
        let shown = if redact {
            redact_value(k, v)
        } else {
            v.clone()
        };
        s.push(k.as_str(), shown);
    }
    if !cfg.notes.is_empty() {
        s.push("notes", cfg.notes.join(" | "));
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
            let shown = if redact { redact_value(&k, &v) } else { v };
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

    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut s = Section::new("Agent");
        s.push("enabled", config::get("agent.enabled").unwrap_or_default());
        s.push("bind", config::get("agent.bind").unwrap_or_default());
        s.push("port", config::get("agent.port").unwrap_or_default());
        s.push("name", config::get("agent.name").unwrap_or_default());
        s.push(
            "allow_remote",
            config::get("security.allow_remote").unwrap_or_default(),
        );
        s.push(
            "transport",
            "ChaCha20 + HMAC-SHA256 over TCP (spid3r-agent/1)",
        );
        let store = crate::agent::store::Store::open();
        s.push("registered devices", store.devices.len().to_string());
        if redact {
            s.push("pairing codes", "[redacted]");
        } else {
            s.push("pairing codes", store.pairings.len().to_string());
        }
        out.push(s);
    }

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

fn process_rss() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                let kb = rest.split_whitespace().next()?;
                return kb.parse::<u64>().ok().map(|n| human_bytes(n * 1024));
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
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

/// HTML table for the info dump. Caller is responsible for wrapping
/// in a full document; `render_html_page` does that.
pub fn render_html(sections: &[Section]) -> String {
    let mut out = String::new();
    out.push_str("<div class=\"spid3rinfo\">\n");
    out.push_str(&format!(
        "<h1>Spid3r {}</h1>\n",
        html_escape(crate::VERSION)
    ));
    for sec in sections {
        out.push_str(&format!("<h2>{}</h2>\n", html_escape(&sec.name)));
        out.push_str("<table>\n");
        for (k, v) in &sec.rows {
            out.push_str("<tr><th>");
            out.push_str(&html_escape(k));
            out.push_str("</th><td>");
            out.push_str(&html_escape(v));
            out.push_str("</td></tr>\n");
        }
        out.push_str("</table>\n");
    }
    out.push_str("</div>\n");
    out
}

/// Full standalone HTML page.
pub fn render_html_page(sections: &[Section]) -> String {
    let mut s = String::new();
    s.push_str("<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">");
    s.push_str("<title>spid3rinfo()</title>");
    s.push_str("<style>");
    s.push_str("body{font-family:system-ui,sans-serif;margin:2rem;background:#111;color:#eee}");
    s.push_str("h1{font-size:1.4rem}h2{font-size:1.1rem;margin-top:1.5rem;color:#8cf}");
    s.push_str("table{border-collapse:collapse;width:100%;max-width:64rem;margin-bottom:0.5rem}");
    s.push_str(
        "th,td{border:1px solid #333;padding:0.25rem 0.6rem;text-align:left;vertical-align:top}",
    );
    s.push_str("th{background:#1a1a1a;font-weight:600;width:18rem;word-break:break-all}");
    s.push_str("td{word-break:break-all;font-family:ui-monospace,monospace;font-size:0.85rem}");
    s.push_str("</style></head><body>\n");
    s.push_str(&render_html(sections));
    s.push_str("</body></html>\n");
    s
}

/// Aligned plain-text dump (CLI default).
pub fn render_text(sections: &[Section]) -> String {
    let mut out = String::new();
    for sec in sections {
        out.push_str(&format!("== {} ==\n", sec.name));
        let width = sec.rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
        for (k, v) in &sec.rows {
            out.push_str(&format!("{:width$}  {}\n", k, v, width = width));
        }
        out.push('\n');
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
    sections_to_value(&collect(redact))
}

/// Keep `make_list` in the API surface for future list-shaped sections.
#[allow(dead_code)]
fn unused_make_list(items: Vec<Value>) -> Value {
    make_list(items)
}
