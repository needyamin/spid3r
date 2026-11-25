//! Shared control operations, JSON-in / JSON-out.
//!
//! Both surfaces — the `spid3r control` CLI and the `spid3r-control` desktop
//! panel — go through [`dispatch`], so a control action behaves identically
//! no matter which surface invokes it. Every op returns a FLAT JSON object:
//! one level, all values strings (arrays are `"\n"`-joined, rows are
//! `"|"`-separated). That keeps the dependency-free JSON handling clean —
//! the crate builds with zero external deps by default.
//!
//! Security posture: config writes validate against `config::schema`, and
//! values returned to UI callers pass through `runtime_info::redact_value` /
//! `redact_diagnostic`.

use crate::config;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::Once;

static INSTALL_TZ_VALIDATOR: Once = Once::new();

/// Dispatch one control operation. `params` is a flat JSON object string.
pub fn dispatch(op: &str, params: &str) -> Result<String, String> {
    INSTALL_TZ_VALIDATOR.call_once(|| {
        crate::config::schema::install_tz_validator(crate::stdlib::tz::check_timezone_setting);
    });
    match op {
        "status" => op_status(params),
        "safety_overview" => op_safety_overview(),
        "ini_info" => op_ini_info(),
        "ini_init" => op_ini_init(params),
        "config_list" => op_config_list(),
        "config_get" => op_config_get(params),
        "config_set" => op_config_set(params),
        "config_reset" => op_config_reset(params),
        "config_reset_all" => op_config_reset_all(params),
        "config_reload" => op_config_reload(),
        "config_validate" => op_config_validate(),
        "logs_tail" => op_logs_tail(params),
        "lang_info" => super::lang::op_lang_info(),
        "lang_exec" => super::lang::op_lang_exec(params),
        "lang_terminal" => super::lang::op_lang_terminal(params),
        "lang_files" => super::lang::op_lang_files(params),
        "lang_read" => super::lang::op_lang_read(params),
        "lang_write" => super::lang::op_lang_write(params),
        other => Err(format!("unknown control op '{}'", other)),
    }
}

// ----------------------------------------------------------- safety keys --
//
// The switches that decide what a Spid3r PROGRAM may do on this machine.
// For `security.deny_*`, "open" means the deny flag is false (programs MAY
// touch the capability); `danger_when_open` stays configurable if any
// future opt-in switch is added.

struct SafetyKey {
    dotted: &'static str,
    /// True when the dangerous state is the value `true`.
    danger_when_open: bool,
    label: &'static str,
    /// One-line consequence of leaving the capability open.
    consequence: &'static str,
}

static SAFETY_KEYS: &[SafetyKey] = &[
    SafetyKey {
        dotted: "security.deny_filesystem",
        danger_when_open: false,
        label: "Filesystem access",
        consequence: "programs can read and write files as this user",
    },
    SafetyKey {
        dotted: "security.deny_process",
        danger_when_open: false,
        label: "Process spawning",
        consequence: "programs can launch other executables",
    },
    SafetyKey {
        dotted: "security.deny_network",
        danger_when_open: false,
        label: "Network access",
        consequence: "programs can open network connections",
    },
    SafetyKey {
        dotted: "security.deny_environment",
        danger_when_open: false,
        label: "Environment variables",
        consequence: "programs can read the machine's environment variables",
    },
];

/// True when the key is currently in its DANGEROUS state (capability open).
fn safety_is_open(key: &SafetyKey) -> bool {
    let value = config::get(key.dotted).unwrap_or_default();
    let truthy = crate::config::schema::parse_bool(&value);
    if key.danger_when_open {
        truthy
    } else {
        // deny_*: the capability is open when the deny flag is FALSE.
        !truthy
    }
}

/// One row per safety key, `|`-separated like the other list ops:
/// `dotted|open|label|consequence|value`
fn safety_rows() -> Vec<String> {
    SAFETY_KEYS
        .iter()
        .map(|k| {
            // Defensive redaction: these are boolean switches today, but if a
            // sensitive key ever joins the list it must not leak via this row.
            let value = crate::runtime_info::redact_value(
                k.dotted,
                &config::get(k.dotted).unwrap_or_default(),
            );
            format!(
                "{}|{}|{}|{}|{}",
                k.dotted,
                if safety_is_open(k) {
                    "open"
                } else {
                    "restricted"
                },
                k.label.replace('|', "/"),
                k.consequence.replace('|', "/"),
                value,
            )
        })
        .collect()
}

fn op_safety_overview() -> Result<String, String> {
    let rows = safety_rows();
    let open = rows
        .iter()
        .filter(|r| r.split('|').nth(1) == Some("open"))
        .count();
    Ok(format!(
        "{{\"open\":\"{}\",\"total\":\"{}\",\"items\":{}}}",
        open,
        rows.len(),
        jstr(&rows.join("\n")),
    ))
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

/// Decode one JSON string starting right at its opening `"` quote, returning
/// `(unescaped_text, byte_index_after_closing_quote)`.
fn parse_json_str(s: &str) -> Option<(String, usize)> {
    let rest = s.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.char_indices();
    while let Some((idx, c)) = chars.next() {
        match c {
            '"' => return Some((out, 1 + idx + 1)),
            '\\' => {
                let (_, esc) = chars.next()?;
                match esc {
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'b' => out.push('\u{0008}'),
                    'f' => out.push('\u{000c}'),
                    'u' => {
                        let mut hex = String::with_capacity(4);
                        for _ in 0..4 {
                            hex.push(chars.next()?.1);
                        }
                        let code = u32::from_str_radix(&hex, 16).ok()?;
                        if (0xD800..=0xDBFF).contains(&code) {
                            // RFC 8259 §7: combine UTF-16 high+low surrogate pair.
                            let mut look = chars.clone();
                            let pair = (|| -> Option<(char, std::str::CharIndices<'_>)> {
                                if look.next()?.1 != '\\' || look.next()?.1 != 'u' {
                                    return None;
                                }
                                let mut lo_hex = String::with_capacity(4);
                                for _ in 0..4 {
                                    lo_hex.push(look.next()?.1);
                                }
                                let lo = u32::from_str_radix(&lo_hex, 16).ok()?;
                                if !(0xDC00..=0xDFFF).contains(&lo) {
                                    return None;
                                }
                                let scalar = 0x10000 + ((code - 0xD800) << 10) + (lo - 0xDC00);
                                Some((char::from_u32(scalar)?, look))
                            })();
                            if let Some((ch, next_iter)) = pair {
                                out.push(ch);
                                chars = next_iter;
                            } else {
                                out.push('\u{fffd}');
                            }
                        } else {
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                    }
                    other => out.push(other),
                }
            }
            c => out.push(c),
        }
    }
    None
}

/// Skip over one JSON value (string, nested object/array, or scalar) inside a
/// top-level JSON object, returning the byte offset immediately after it.
fn skip_json_value(s: &str) -> Option<usize> {
    let trimmed_len = s.len() - s.trim_start().len();
    let tail = &s[trimmed_len..];
    if tail.starts_with('"') {
        let (_, end) = parse_json_str(tail)?;
        return Some(trimmed_len + end);
    }
    if tail.starts_with('{') || tail.starts_with('[') {
        let bytes = tail.as_bytes();
        let mut depth = 0usize;
        let mut in_str = false;
        let mut escaped = false;
        for (i, &b) in bytes.iter().enumerate() {
            if in_str {
                if escaped {
                    escaped = false;
                } else if b == b'\\' {
                    escaped = true;
                } else if b == b'"' {
                    in_str = false;
                }
                continue;
            }
            match b {
                b'"' => in_str = true,
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(trimmed_len + i + 1);
                    }
                }
                _ => {}
            }
        }
        return None;
    }
    let end = tail
        .find([',', '}', ']', ' ', '\t', '\r', '\n'])
        .unwrap_or(tail.len());
    (end > 0).then_some(trimmed_len + end)
}

/// Locate a top-level field in a JSON object and return the slice beginning at
/// its raw JSON value (string- and nesting-aware, so keys inside string values
/// or child objects never false-match).
pub(crate) fn raw_field_slice<'a>(json: &'a str, name: &str) -> Option<&'a str> {
    let mut pos = json.len() - json.trim_start().len();
    if !json[pos..].starts_with('{') {
        return None;
    }
    pos += 1;
    loop {
        let rem = &json[pos..];
        pos += rem.len() - rem.trim_start().len();
        let cur = &json[pos..];
        if cur.starts_with('}') || cur.is_empty() {
            return None;
        }
        let (key, key_end) = parse_json_str(cur)?;
        pos += key_end;
        let after_key = &json[pos..];
        pos += after_key.len() - after_key.trim_start().len();
        if !json[pos..].starts_with(':') {
            return None;
        }
        pos += 1;
        let after_colon = &json[pos..];
        pos += after_colon.len() - after_colon.trim_start().len();
        if key == name {
            return Some(&json[pos..]);
        }
        let val_len = skip_json_value(&json[pos..])?;
        pos += val_len;
        let after_val = &json[pos..];
        pos += after_val.len() - after_val.trim_start().len();
        if json[pos..].starts_with(',') {
            pos += 1;
        } else {
            return None;
        }
    }
}

/// Extract a top-level scalar field from a flat JSON object (`"string"`,
/// unquoted `true`/`false`, or unquoted integer/float number) as an unescaped
/// `String`. JSON `null`, objects, and arrays return `None`.
pub fn field(json: &str, name: &str) -> Option<String> {
    let val = raw_field_slice(json, name)?;
    if val.starts_with('"') {
        return parse_json_str(val).map(|(s, _)| s);
    }
    let token_end = val
        .find([',', '}', ']', ' ', '\t', '\r', '\n'])
        .unwrap_or(val.len());
    let token = &val[..token_end];
    if token == "true" || token == "false" {
        return Some(token.to_string());
    }
    if token == "null" || token.is_empty() || token.starts_with('{') || token.starts_with('[') {
        return None;
    }
    if token.parse::<i64>().is_ok() || token.parse::<f64>().is_ok() {
        return Some(token.to_string());
    }
    None
}

// -------------------------------------------------------------------- ops --

fn op_status(_params: &str) -> Result<String, String> {
    let cfg = config::load();
    let ini_active = cfg.path.clone();
    let mut errors = config::validate(&cfg);
    errors.extend(config::pin_problems(&cfg));
    let errors = errors
        .iter()
        .map(|error| crate::runtime_info::redact_diagnostic(error))
        .collect::<Vec<_>>();
    let ini_display = ini_active
        .as_deref()
        .or_else(|| config::pin_target(&cfg))
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let ini_exists = ini_active.as_ref().is_some_and(|p| p.is_file());
    Ok(format!(
        "{{\"version\":{},\"language\":{},\
         \"env\":{},\
         \"ini_path\":{},\"ini_exists\":{},\"ini_valid\":{},\"ini_errors\":{},\
         \"state_dir\":{}}}",
        jstr(crate::VERSION),
        jstr(crate::LANGUAGE_VERSION),
        jstr(&config::get("spid3r.env").unwrap_or_else(|| "default".into())),
        jstr(&ini_display),
        jstr(if ini_exists { "true" } else { "false" }),
        jstr(if errors.is_empty() { "true" } else { "false" }),
        jstr(&errors.join("\n")),
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
    let current = config::snapshot();
    if config::pin_failed(&current) {
        eprintln!(
            "warning: repairing broken SPID3R_INI pin ({})",
            config::pin_problems(&current).join("; ")
        );
    }
    let target = config::active_path();
    if let Some(existing) = target.as_ref().filter(|p| p.is_file()) {
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
    let path = config::write_default_ini(target.as_deref())?;
    config::reload();
    Ok(format!(
        "{{\"written\":\"true\",\"path\":{},\"message\":{}}}",
        jstr(&path.display().to_string()),
        jstr(&format!("default ini written: {}", path.display())),
    ))
}

fn canonical_kind_name(kind: crate::config::schema::ConfigValue) -> &'static str {
    match kind {
        crate::config::schema::ConfigValue::Bool => "Bool",
        crate::config::schema::ConfigValue::Int => "Int",
        crate::config::schema::ConfigValue::Str => "Str",
        crate::config::schema::ConfigValue::List => "List",
        crate::config::schema::ConfigValue::Enum(_) => "Enum",
    }
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
        // Row: key|kind|value|default|description|enum-options|reload|frozen
        // Enum options drive the panel's dropdown; empty for other kinds.
        let options = match k.kind {
            crate::config::schema::ConfigValue::Enum(opts) => opts.join(","),
            _ => String::new(),
        };
        let reload = match k.reload {
            crate::config::schema::ReloadSafety::Live => "live",
            crate::config::schema::ReloadSafety::StartupOnly => "startup",
        };
        rows.push(format!(
            "{}|{}|{}|{}|{}|{}|{}|{}",
            dotted,
            canonical_kind_name(k.kind),
            redacted.replace('|', "/").replace('\n', " "),
            k.default.replace('|', "/").replace('\n', " "),
            k.description.replace('|', "/").replace('\n', " "),
            options,
            reload,
            if k.frozen { "true" } else { "false" },
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

/// Make sure a writable `spid3r.ini` exists BEFORE the first save.
///
/// `config::save` falls back to the FIRST candidate path — the executable's
/// directory — when no ini was loaded. On an installed machine that directory
/// is `C:\Program Files\Spid3r\`, which a per-user process cannot write (and
/// should not: the install directory is not user configuration). So the
/// control surface bootstraps the default ini into the platform config dir
/// (`%APPDATA%\Spid3r` / XDG config) instead, and reloads so subsequent saves
/// target that file.
fn ensure_writable_ini() -> Result<PathBuf, String> {
    if let Some(pin) = config::pinned_path() {
        let current = config::snapshot();
        if config::pin_failed(&current) {
            eprintln!(
                "warning: repairing broken SPID3R_INI pin ({})",
                config::pin_problems(&current).join("; ")
            );
        }
        if pin.is_file() {
            return Ok(pin);
        }
        let created = config::write_default_ini(Some(&pin))?;
        config::reload();
        return Ok(created);
    }
    if let Some(active) = config::candidate_paths().into_iter().find(|p| p.is_file()) {
        return Ok(active);
    }
    let target = config::config_dir()
        .ok_or("no config directory available for spid3r.ini")?
        .join("spid3r.ini");
    let created = config::write_default_ini(Some(&target))?;
    config::reload();
    Ok(created)
}

fn op_config_set(params: &str) -> Result<String, String> {
    let Some(key) = field(params, "key") else {
        return Err("config_set needs \"key\"".into());
    };
    let Some(value) = field(params, "value") else {
        return Err("config_set needs \"value\"".into());
    };
    let spec = crate::config::schema::known_key(&key)
        .ok_or_else(|| format!("unknown config key '{}'", key))?;
    // Coerce and validate extra rules against the schema BEFORE touching disk
    // or bootstrapping spid3r.ini, so rejected writes never leave a fresh INI
    // behind and always name the key's exact rule.
    spec.coerce(&value)?;
    crate::config::schema::validate_extra(&key, &value)?;
    // Normalize the stored value so the panel's `=== "true"` checks and the
    // INI stay canonical: "on"/"yes" become "true", "007" becomes "7".
    let value = match spec.kind {
        crate::config::schema::ConfigValue::Bool => {
            crate::config::schema::parse_bool(&value).to_string()
        }
        crate::config::schema::ConfigValue::Int => value
            .trim()
            .parse::<i64>()
            .map(|n| n.to_string())
            .unwrap_or(value),
        _ => value,
    };
    // A machine with no spid3r.ini yet must not silently target the install
    // directory (config::save's fallback): bootstrap the default ini into the
    // per-user config dir first.
    let bootstrapped = ensure_writable_ini()?;
    config::set_live(&key, &value)?;
    let cfg = config::snapshot();
    let errors = config::validate(&cfg);
    if !errors.is_empty() {
        // The live snapshot is now invalid: discard it by re-reading the file.
        config::reload();
        return Err(errors
            .iter()
            .map(|error| crate::runtime_info::redact_diagnostic(error))
            .collect::<Vec<_>>()
            .join("; "));
    }
    let path = config::save(&config::snapshot())?;
    Ok(format!(
        "{{\"message\":{},\"path\":{},\"bootstrapped\":{}}}",
        jstr(&format!("{} set; saved to {}", key, path.display())),
        jstr(&path.display().to_string()),
        jstr(&bootstrapped.display().to_string()),
    ))
}

fn op_config_reset(params: &str) -> Result<String, String> {
    let Some(key) = field(params, "key") else {
        return Err("config_reset needs \"key\"".into());
    };
    let spec = crate::config::schema::known_key(&key)
        .ok_or_else(|| format!("unknown config key '{}'", key))?;
    let default = spec.default.to_string();
    let _ = ensure_writable_ini()?;
    config::set_live(&key, &default)?;
    let errors = config::validate(&config::snapshot());
    if !errors.is_empty() {
        config::reload();
        return Err(errors
            .iter()
            .map(|error| crate::runtime_info::redact_diagnostic(error))
            .collect::<Vec<_>>()
            .join("; "));
    }
    let path = config::save(&config::snapshot())?;
    Ok(format!(
        "{{\"message\":{}}}",
        jstr(&format!(
            "{} reset to default ({}) — saved to {}",
            key,
            crate::runtime_info::redact_value(&key, &default),
            path.display()
        )),
    ))
}

fn op_config_validate() -> Result<String, String> {
    let cfg = config::snapshot();
    let mut errors = config::validate(&cfg);
    errors.extend(config::pin_problems(&cfg));
    let errors = errors
        .iter()
        .map(|error| crate::runtime_info::redact_diagnostic(error))
        .collect::<Vec<_>>();
    Ok(format!(
        "{{\"valid\":\"{}\",\"errors\":{}}}",
        if errors.is_empty() { "true" } else { "false" },
        jstr(&errors.join("\n")),
    ))
}

fn op_config_reload() -> Result<String, String> {
    let cfg = config::reload();
    let path = cfg
        .path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(defaults)".into());
    Ok(format!(
        "{{\"path\":{},\"notes\":{},\"message\":{}}}",
        jstr(&path),
        jstr(&cfg.notes.join("\n")),
        jstr(&format!("reloaded {}", path)),
    ))
}

fn op_config_reset_all(params: &str) -> Result<String, String> {
    let section = field(params, "section").unwrap_or_default();
    let section = section.trim().to_string();
    let _ = ensure_writable_ini()?;
    let mut count = 0usize;
    for k in crate::config::schema::KEYS {
        if !section.is_empty() && k.section != section {
            continue;
        }
        config::set_live(&k.dotted(), k.default)?;
        count += 1;
    }
    if count == 0 {
        return Err(format!("unknown config section '{}'", section));
    }
    let path = config::save(&config::snapshot())?;
    let scope = if section.is_empty() {
        "all".to_string()
    } else {
        format!("[{}]", section)
    };
    Ok(format!(
        "{{\"count\":\"{}\",\"path\":{},\"message\":{}}}",
        count,
        jstr(&path.display().to_string()),
        jstr(&format!(
            "reset {} key(s) in {} to defaults — saved to {}",
            count,
            scope,
            path.display()
        )),
    ))
}

fn op_logs_tail(params: &str) -> Result<String, String> {
    let n: usize = field(params, "n")
        .and_then(|v| v.parse().ok())
        .unwrap_or(50)
        .clamp(1, 1000);
    let path = crate::logging::path();
    let Some(path) = path else {
        return Ok("{\"path\":\"\",\"text\":\"\"}".to_string());
    };
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
            format!(
                " (no spid3r.ini yet — run: spid3r control ini --init){}",
                if get("ini_errors").is_empty() {
                    String::new()
                } else {
                    format!(" (INVALID: {})", get("ini_errors"))
                }
            )
        },
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
        // it because it is inside a string literal.
        let json = "{\"a\":\"has \\\"a\\\" inside\",\"b\":\"v\"}";
        assert_eq!(field(json, "a").unwrap(), "has \"a\" inside");
        assert_eq!(field(json, "b").unwrap(), "v");
    }

    #[test]
    fn field_reads_unquoted_booleans_and_numbers_and_ignores_nested() {
        let json = r#"{"force":true,"n":100,"nested":{"n":999,"flag":false},"flag":false}"#;
        assert_eq!(field(json, "force").as_deref(), Some("true"));
        assert_eq!(field(json, "n").as_deref(), Some("100"));
        assert_eq!(field(json, "flag").as_deref(), Some("false"));
        assert!(field(json, "nested").is_none());
    }

    #[test]
    fn config_list_emits_canonical_enum_kind() {
        let json = dispatch("config_list", "{}").unwrap();
        let keys = field(&json, "keys").unwrap();
        let row = keys
            .lines()
            .find(|l| l.starts_with("spid3r.engine|"))
            .expect("spid3r.engine row present");
        let cols: Vec<&str> = row.split('|').collect();
        assert_eq!(cols.len(), 8);
        assert_eq!(cols[1], "Enum");
        assert_eq!(cols[5], "tree,vm");
    }
}
