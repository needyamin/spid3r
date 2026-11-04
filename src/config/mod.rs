//! `spid3r.ini` — the centralized runtime configuration.
//!
//! Spid3r's centralized runtime configuration: `spid3r.ini`. One file, one
//! schema, one loader. Values resolve in a fixed order so an operator can always say
//! which layer won:
//!
//! 1. compiled defaults (`schema::DEFAULTS`)
//! 2. the INI file (`spid3r.ini`)
//! 3. the `[env.<name>]` section matching `SPID3R_ENV` (or `[spid3r] env`)
//! 4. process environment variables (`SPID3R_<KEY>`)
//!
//! The parser is a self-contained INI reader, not a TOML reader: comments
//! (`;` and `#`), `key = value`, `[section]`, quoted strings, and inline
//! `key.subkey` dotted names inside a section. No third-party crates.
//!
//! Live reload re-reads the file when its mtime changes (polled on access)
//! or when an operator asks for it (`spid3r config reload`, `SIGHUP` on Unix,
//! or `POST /api/v1/config/reload` on the management agent). Unknown keys are
//! kept and reported by `validate`; they never abort a run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::time::SystemTime;

pub mod schema;

use schema::{known_key, DEFAULTS};

/// A parsed INI document: `section -> key -> raw string`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IniDoc {
    /// `""` is the implicit top section (keys before any `[section]`).
    pub sections: BTreeMap<String, BTreeMap<String, String>>,
}

impl IniDoc {
    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.sections.get(section)?.get(key).map(String::as_str)
    }

    pub fn set(&mut self, section: &str, key: &str, value: &str) {
        self.sections
            .entry(section.to_string())
            .or_default()
            .insert(key.to_string(), value.to_string());
    }

    /// Dotted `section.key` lookup (`agent.bind`).
    pub fn get_dotted(&self, dotted: &str) -> Option<&str> {
        let (sec, key) = split_dotted(dotted)?;
        self.get(sec, key)
    }

    /// Every known key as `section.key -> raw`, plus unknown keys.
    pub fn flatten(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for (sec, keys) in &self.sections {
            for (k, v) in keys {
                if sec.is_empty() {
                    out.insert(k.clone(), v.clone());
                } else {
                    out.insert(format!("{}.{}", sec, k), v.clone());
                }
            }
        }
        out
    }
}

fn split_dotted(dotted: &str) -> Option<(&str, &str)> {
    let i = dotted.find('.')?;
    Some((&dotted[..i], &dotted[i + 1..]))
}

/// Parse INI text. Returns the document and a list of parse problems
/// (duplicate-ish leftovers, missing `=`, unclosed quotes). Parsing never
/// fails hard: a malformed line is skipped and reported.
pub fn parse_ini(text: &str) -> (IniDoc, Vec<String>) {
    let mut doc = IniDoc::default();
    let mut problems = Vec::new();
    let mut section = String::new();
    for (lineno, raw) in text.lines().enumerate() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            let Some(end) = line.find(']') else {
                problems.push(format!("line {}: unclosed section header", lineno + 1));
                continue;
            };
            let name = line[1..end].trim();
            if name.is_empty() {
                problems.push(format!("line {}: empty section name", lineno + 1));
                continue;
            }
            section = name.to_string();
            doc.sections.entry(section.clone()).or_default();
            continue;
        }
        let Some(eq) = line.find('=') else {
            problems.push(format!("line {}: expected key = value", lineno + 1));
            continue;
        };
        let key = line[..eq].trim();
        let mut val = line[eq + 1..].trim();
        if key.is_empty() {
            problems.push(format!("line {}: empty key", lineno + 1));
            continue;
        }
        if val.len() >= 2
            && ((val.starts_with('"') && val.ends_with('"'))
                || (val.starts_with('\'') && val.ends_with('\'')))
        {
            val = &val[1..val.len() - 1];
        }
        doc.sections
            .entry(section.clone())
            .or_default()
            .insert(key.to_string(), unescape_ini(val));
    }
    (doc, problems)
}

fn strip_comment(line: &str) -> &str {
    // A `;` or `#` that is not inside quotes starts a comment.
    let bytes = line.as_bytes();
    let mut in_quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate() {
        match in_quote {
            Some(q) if b == q => in_quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => in_quote = Some(b),
            None if b == b';' || b == b'#' => return &line[..i],
            None => {}
        }
    }
    line
}

fn unescape_ini(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn escape_ini(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            ';' | '#' | '"' if s.starts_with(';') || s.starts_with('#') => {
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    if s.contains(';') || s.contains('#') || s.trim() != s || s.is_empty() {
        format!("\"{}\"", out)
    } else {
        out
    }
}

/// Serialize an INI document back to text, preserving section order.
pub fn write_ini(doc: &IniDoc) -> String {
    let mut out = String::new();
    for (sec, keys) in &doc.sections {
        if !sec.is_empty() {
            out.push_str(&format!("[{}]\n", sec));
        }
        for (k, v) in keys {
            out.push_str(&format!("{} = {}\n", k, escape_ini(v)));
        }
        out.push('\n');
    }
    out
}

/// Where we look for `spid3r.ini`, in order. The first hit wins; the rest
/// are reported by `search_path()` for `spid3r config path`.
pub fn candidate_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = std::env::var_os("SPID3R_INI") {
        if !p.is_empty() {
            out.push(PathBuf::from(p));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("spid3r.ini"));
        }
    }
    if let Some(dir) = config_dir() {
        out.push(dir.join("spid3r.ini"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join("spid3r.ini"));
    }
    out
}

/// Platform configuration directory (`%APPDATA%\Spid3r`, XDG config, `~/.spid3r`).
pub fn config_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SPID3R_CONFIG_DIR") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    if cfg!(windows) {
        return std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
            .map(|b| b.join("Spid3r"));
    }
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return Some(PathBuf::from(x).join("spid3r"));
        }
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join(".config").join("spid3r"))
}

/// Mutable state directory (agent devices, tokens, audit log).
pub fn state_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("SPID3R_STATE_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
            .map(|b| b.join("Spid3r").join("state"))
            .unwrap_or_else(|| PathBuf::from("spid3r-state"));
    }
    if let Some(x) = std::env::var_os("XDG_STATE_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("spid3r");
        }
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join(".local").join("state").join("spid3r"))
        .unwrap_or_else(|| PathBuf::from("spid3r-state"))
}

/// Resolved configuration: typed values plus the raw document they came from.
#[derive(Debug, Clone)]
pub struct Config {
    /// Flattened `section.key -> value` after the full override chain.
    pub values: BTreeMap<String, String>,
    /// The INI file that was loaded, if any.
    pub path: Option<PathBuf>,
    /// Parse/validation notes (unknown keys, type errors that fell back).
    pub notes: Vec<String>,
    /// mtime of the file at load, for live-reload polling.
    pub mtime: Option<SystemTime>,
}

impl Config {
    pub fn get(&self, dotted: &str) -> Option<&str> {
        self.values.get(dotted).map(String::as_str)
    }

    pub fn get_or(&self, dotted: &str, default: &str) -> String {
        self.get(dotted).unwrap_or(default).to_string()
    }

    pub fn bool_of(&self, dotted: &str) -> bool {
        self.get(dotted).map(schema::parse_bool).unwrap_or(false)
    }

    pub fn int_of(&self, dotted: &str) -> Option<i64> {
        self.get(dotted).and_then(|v| v.trim().parse::<i64>().ok())
    }

    pub fn str_of(&self, dotted: &str) -> String {
        self.get(dotted).unwrap_or("").to_string()
    }

    /// Set a known key in memory (does not write the file).
    pub fn set(&mut self, dotted: &str, value: &str) -> Result<(), String> {
        let spec = known_key(dotted).ok_or_else(|| format!("unknown key '{}'", dotted))?;
        spec.coerce(value)?;
        self.values.insert(dotted.to_string(), value.to_string());
        Ok(())
    }
}

/// Load configuration: defaults, then file, then environment section, then env.
pub fn load() -> Config {
    let path = candidate_paths().into_iter().find(|p| p.is_file());
    let mut notes: Vec<String> = Vec::new();
    let (doc, mtime) = match &path {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(text) => {
                let (d, probs) = parse_ini(&text);
                notes = probs;
                (d, std::fs::metadata(p).and_then(|m| m.modified()).ok())
            }
            Err(e) => {
                notes.push(format!("cannot read {}: {}", p.display(), e));
                (IniDoc::default(), None)
            }
        },
        None => (IniDoc::default(), None),
    };

    let env_name = std::env::var("SPID3R_ENV")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| doc.get("spid3r", "env").map(String::from))
        .unwrap_or_else(|| "default".to_string());

    let mut values: BTreeMap<String, String> = DEFAULTS
        .iter()
        .map(|(k, _, v)| (k.to_string(), v.to_string()))
        .collect();

    // File values (excluding [env.*] which apply later).
    for (k, v) in doc.flatten() {
        if k.starts_with("env.") {
            continue;
        }
        match known_key(&k) {
            Some(spec) => {
                if let Err(e) = spec.coerce(&v) {
                    notes.push(format!("{}: {} (using default)", k, e));
                    continue;
                }
                values.insert(k, v);
            }
            None => {
                notes.push(format!("unknown key '{}' in ini", k));
                values.insert(k, v);
            }
        }
    }

    // Environment-specific section overrides.
    let env_prefix = format!("env.{}.", env_name);
    for (k, v) in doc.flatten() {
        if let Some(rest) = k.strip_prefix(&env_prefix) {
            if known_key(rest).is_some() {
                values.insert(rest.to_string(), v);
            } else {
                notes.push(format!("unknown key '{}' in [env.{}]", rest, env_name));
            }
        }
    }

    // Process environment: SPID3R_AGENT_BIND / SPID3R_AGENT_BIND.
    // Existing documented names (SPID3R_MAX_WORKERS, ...) also apply.
    apply_process_env(&mut values, &mut notes);

    Config {
        values,
        path,
        notes,
        mtime,
    }
}

fn apply_process_env(values: &mut BTreeMap<String, String>, notes: &mut Vec<String>) {
    // Documented legacy names first.
    let legacy = [
        ("SPID3R_MAX_WORKERS", "spid3r.max_workers"),
        ("SPID3R_QUEUE_CAP", "spid3r.queue_cap"),
        ("SPID3R_GC", "spid3r.gc"),
        ("SPID3R_LOG", "logging.enabled"),
        ("SPID3R_LOG_FILE", "logging.file"),
        ("SPID3R_ENV", "spid3r.env"),
        ("SPID3R_PLUGINS", "modules.plugins"),
        ("SPID3R_PATH", "modules.path"),
    ];
    for (env, key) in legacy {
        if let Ok(v) = std::env::var(env) {
            if !v.is_empty() {
                values.insert(key.to_string(), v);
            }
        }
    }
    // SPID3R_<SECTION>_<KEY> for every known key.
    for spec in schema::KEYS {
        let env_name = format!(
            "SPID3R_{}_{}",
            spec.section.to_ascii_uppercase().replace('-', "_"),
            spec.name.to_ascii_uppercase().replace('-', "_")
        );
        if let Ok(v) = std::env::var(&env_name) {
            if !v.is_empty() {
                if let Err(e) = spec.coerce(&v) {
                    notes.push(format!("{}: {}", env_name, e));
                    continue;
                }
                values.insert(spec.dotted().to_string(), v);
            }
        }
    }
}

// ------------------------------------------------------------- global handle

static GLOBAL: OnceLock<RwLock<Config>> = OnceLock::new();

fn global() -> &'static RwLock<Config> {
    GLOBAL.get_or_init(|| RwLock::new(load()))
}

/// Read a snapshot of the current configuration.
pub fn snapshot() -> Config {
    let g = global().read().unwrap_or_else(|e| e.into_inner());
    g.clone()
}

/// Read one key.
pub fn get(dotted: &str) -> Option<String> {
    snapshot().get(dotted).map(String::from)
}

/// Update a key in the live snapshot (memory only; `save` persists).
pub fn set_live(dotted: &str, value: &str) -> Result<(), String> {
    let mut g = global().write().unwrap_or_else(|e| e.into_inner());
    g.set(dotted, value)
}

/// Re-read the INI file and swap it in. Returns the new snapshot.
pub fn reload() -> Config {
    let mut g = global().write().unwrap_or_else(|e| e.into_inner());
    *g = load();
    g.clone()
}

/// Write the current values (known keys only) back to the primary INI path.
pub fn save(cfg: &Config) -> Result<PathBuf, String> {
    let path = cfg
        .path
        .clone()
        .or_else(|| candidate_paths().into_iter().next())
        .unwrap_or_else(|| {
            config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("spid3r.ini")
        });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
    }
    let mut doc = IniDoc::default();
    for (k, v) in &cfg.values {
        if let Some(spec) = known_key(k) {
            doc.set(spec.section, spec.name, v);
        }
    }
    std::fs::write(&path, write_ini(&doc))
        .map_err(|e| format!("write {}: {}", path.display(), e))?;
    Ok(path)
}

/// Validate a config: unknown keys, type errors, out-of-range numbers.
pub fn validate(cfg: &Config) -> Vec<String> {
    let mut problems = cfg.notes.clone();
    for (k, v) in &cfg.values {
        match known_key(k) {
            Some(spec) => {
                if let Err(e) = spec.coerce(v) {
                    problems.push(format!("{}: {}", k, e));
                }
            }
            None => problems.push(format!("unknown key '{}'", k)),
        }
    }
    problems
}

/// Poll mtime and reload if the file changed. Cheap enough to call per CLI run.
pub fn maybe_reload_from_disk() {
    let need = {
        let g = global().read().unwrap_or_else(|e| e.into_inner());
        match (&g.path, g.mtime) {
            (Some(p), Some(old)) => match std::fs::metadata(p).and_then(|m| m.modified()) {
                Ok(new) => new != old,
                Err(_) => false,
            },
            _ => false,
        }
    };
    if need {
        reload();
    }
}

/// Write a commented default `spid3r.ini` next to the first candidate path.
pub fn write_default_ini(path: Option<&Path>) -> Result<PathBuf, String> {
    let path = path
        .map(Path::to_path_buf)
        .or_else(|| candidate_paths().into_iter().next())
        .unwrap_or_else(|| config_dir().unwrap_or_default().join("spid3r.ini"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {}: {}", parent.display(), e))?;
    }
    std::fs::write(&path, schema::DEFAULT_INI_TEXT)
        .map_err(|e| format!("write {}: {}", path.display(), e))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections_quotes_and_comments() {
        let (doc, probs) = parse_ini(
            "; comment\n[spid3r]\nenv = \"prod\" ; trailing\nname = spid3r # hash\n[agent]\nport = 9527\n",
        );
        assert!(probs.is_empty());
        assert_eq!(doc.get("spid3r", "env"), Some("prod"));
        assert_eq!(doc.get("spid3r", "name"), Some("spid3r"));
        assert_eq!(doc.get("agent", "port"), Some("9527"));
    }

    #[test]
    fn roundtrip_ini() {
        let mut doc = IniDoc::default();
        doc.set("agent", "bind", "127.0.0.1");
        doc.set("agent", "port", "9527");
        let text = write_ini(&doc);
        let (back, _) = parse_ini(&text);
        assert_eq!(back.get("agent", "bind"), Some("127.0.0.1"));
        assert_eq!(back.get("agent", "port"), Some("9527"));
    }

    #[test]
    fn bool_and_int_helpers() {
        let mut cfg = Config {
            values: BTreeMap::new(),
            path: None,
            notes: Vec::new(),
            mtime: None,
        };
        cfg.values.insert("agent.enabled".into(), "true".into());
        cfg.values.insert("agent.port".into(), "9527".into());
        assert!(cfg.bool_of("agent.enabled"));
        assert_eq!(cfg.int_of("agent.port"), Some(9527));
        assert_eq!(cfg.int_of("agent.nope"), None);
    }
}
