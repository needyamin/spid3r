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
pub fn is_policy_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "security.basedir" | "security.disable_natives"
    )
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
    if !is_sensitive_key(key) && !is_policy_key(key) {
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
    if !is_sensitive_key(key) {
        return value.to_string();
    }
    hash_redact(value)
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
        let shown = if redact && remote {
            redact_value_remote(k, v)
        } else if redact {
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
}
