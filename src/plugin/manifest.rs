//! The `plugin.toml` manifest.
//!
//! Parsed with the same hand-written TOML subset the package manager uses
//! (`crate::pkg`), so a plugin manifest and a `spider.toml` cannot disagree
//! about what a quoted string or an array looks like. No third-party crate.
//!
//! ```toml
//! [plugin]
//! name = "acme.httpkit"
//! version = "1.2.0"
//! abi = "spid3r-plugin-abi-1"
//! kind = "host"
//! capabilities = ["network"]
//!
//! [[function]]
//! name = "acme_fetch"
//! arity = 2
//! permission = "network"
//! ```
//!
//! `[[function]]` is a table array, which the manifest subset deliberately does
//! not support — that is why this parser is separate rather than reusing
//! `pkg::parse_manifest`.

use crate::embed::Capability;

/// The ABI string every v1 plugin must declare, verbatim.
pub const ABI_V1: &str = "spid3r-plugin-abi-1";

/// How a plugin is hosted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginKind {
    /// Compiled into the host binary. **Trusted code** — same address space,
    /// same authority, and no filter can revoke it.
    Host,
    /// A WebAssembly module. The only kind that can be sandboxed.
    Wasm,
    /// A `.matrix` file: a plugin written in Spid3r itself. Script authority.
    Matrix,
}

impl PluginKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PluginKind::Host => "host",
            PluginKind::Wasm => "wasm",
            PluginKind::Matrix => "matrix",
        }
    }
}

/// One function a plugin provides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionDecl {
    /// The name the script calls. Must be unique across builtins and plugins.
    pub name: String,
    pub arity: usize,
    /// The capability this function needs, if any. `None` means it touches
    /// nothing the host can revoke (pure computation).
    pub permission: Option<Capability>,
}

/// A parsed `plugin.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    /// Must equal [`ABI_V1`] exactly. Anything else is rejected before any
    /// plugin code runs — a plugin built against a different contract is not
    /// "probably fine".
    pub abi: String,
    pub kind: PluginKind,
    /// Every capability the plugin declares up front.
    pub capabilities: Vec<Capability>,
    /// The module file, for `kind = "wasm"`. Empty for a host plugin.
    pub entry: String,
    /// Functions declared in `[[function]]` blocks.
    pub functions: Vec<FunctionDecl>,
}

impl PluginManifest {
    /// Parse a `plugin.toml` document.
    pub fn parse(text: &str) -> Result<PluginManifest, String> {
        let mut name: Option<String> = None;
        let mut version: Option<String> = None;
        let mut abi: Option<String> = None;
        let mut kind: Option<PluginKind> = None;
        let mut capabilities: Vec<Capability> = Vec::new();
        let mut entry = String::new();
        let mut functions: Vec<FunctionDecl> = Vec::new();

        // `None` = inside `[plugin]`; `Some(partial)` = inside a `[[function]]`
        // block being accumulated.
        let mut section = "";
        let mut cur_fn: Option<FunctionDecl> = None;

        for (lineno, raw) in text.lines().enumerate() {
            let line = crate::pkg::strip_comment(raw).trim().to_string();
            if line.is_empty() {
                continue;
            }
            let where_ = format!("line {}", lineno + 1);

            if line.starts_with("[[") {
                let sect = line
                    .strip_prefix("[[")
                    .and_then(|s| s.strip_suffix("]]"))
                    .ok_or_else(|| format!("{}: unterminated table-array header", where_))?
                    .trim();
                if sect != "function" {
                    return Err(format!("{}: unknown table array '[[{}]]'", where_, sect));
                }
                if let Some(f) = cur_fn.take() {
                    functions.push(f);
                }
                cur_fn = Some(FunctionDecl {
                    name: String::new(),
                    arity: 0,
                    permission: None,
                });
                section = "function";
                continue;
            }
            if let Some(rest) = line.strip_prefix('[') {
                let sect = rest
                    .strip_suffix(']')
                    .ok_or_else(|| format!("{}: unterminated section header", where_))?
                    .trim();
                if sect != "plugin" {
                    return Err(format!("{}: unknown section '[{}]'", where_, sect));
                }
                if let Some(f) = cur_fn.take() {
                    functions.push(f);
                }
                section = "plugin";
                continue;
            }

            let Some((key, value)) = line.split_once('=') else {
                return Err(format!("{}: expected `key = value`", where_));
            };
            let key = key.trim();
            let value = value.trim();

            match section {
                "plugin" => match key {
                    "name" => {
                        name = Some(quoted(value, &where_, key)?);
                    }
                    "version" => {
                        version = Some(quoted(value, &where_, key)?);
                    }
                    "abi" => {
                        abi = Some(quoted(value, &where_, key)?);
                    }
                    "kind" => {
                        let k = quoted(value, &where_, key)?;
                        kind = Some(match k.as_str() {
                            "host" => PluginKind::Host,
                            "wasm" => PluginKind::Wasm,
                            "matrix" => PluginKind::Matrix,
                            other => {
                                return Err(format!(
                                    "{}: plugin.kind must be \"matrix\", \"host\" or \"wasm\", got \"{}\"",
                                    where_, other
                                ))
                            }
                        });
                    }
                    "entry" => {
                        entry = quoted(value, &where_, key)?;
                    }
                    "capabilities" => {
                        let names = crate::pkg::parse_string_array(value).ok_or_else(|| {
                            format!("{}: capabilities expects [\"network\"]", where_)
                        })?;
                        capabilities = names
                            .iter()
                            .map(|n| capability(n))
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(|e| format!("{}: {}", where_, e))?;
                    }
                    other => {
                        return Err(format!("{}: unknown plugin key '{}'", where_, other));
                    }
                },
                "function" => {
                    let f = cur_fn
                        .as_mut()
                        .ok_or_else(|| format!("{}: function key outside [[function]]", where_))?;
                    match key {
                        "name" => f.name = quoted(value, &where_, key)?,
                        "arity" => {
                            f.arity = value.parse().map_err(|_| {
                                format!("{}: function.arity must be a number", where_)
                            })?;
                        }
                        "permission" => {
                            let p = quoted(value, &where_, key)?;
                            f.permission =
                                Some(capability(&p).map_err(|e| format!("{}: {}", where_, e))?);
                        }
                        other => {
                            return Err(format!("{}: unknown function key '{}'", where_, other));
                        }
                    }
                }
                _ => return Err(format!("{}: value outside of a section", where_)),
            }
        }
        if let Some(f) = cur_fn.take() {
            functions.push(f);
        }

        let name = name.ok_or("[plugin] name is required")?;
        if name.is_empty() {
            return Err("[plugin] name must not be empty".to_string());
        }
        for f in &functions {
            if f.name.is_empty() {
                return Err("every [[function]] needs a name".to_string());
            }
        }
        let mut seen: Vec<&str> = Vec::new();
        for f in &functions {
            if seen.contains(&f.name.as_str()) {
                return Err(format!("duplicate function '{}'", f.name));
            }
            seen.push(&f.name);
        }

        Ok(PluginManifest {
            name,
            version: version.unwrap_or_else(|| "0.0.0".to_string()),
            abi: abi.unwrap_or_default(),
            kind: kind.unwrap_or(PluginKind::Host),
            capabilities,
            entry,
            functions,
        })
    }

    /// Load `plugin.toml` from a plugin directory.
    pub fn load(dir: &std::path::Path) -> Result<PluginManifest, String> {
        let path = dir.join("plugin.toml");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
        PluginManifest::parse(&text).map_err(|e| format!("{}: {}", path.display(), e))
    }

    /// Whether this manifest declares the ABI this build implements.
    pub fn abi_matches(&self) -> bool {
        self.abi == ABI_V1
    }
}

fn quoted(value: &str, where_: &str, key: &str) -> Result<String, String> {
    crate::pkg::parse_string(value)
        .ok_or_else(|| format!("{}: {} expects a quoted string", where_, key))
}

/// The capability vocabulary, shared with [`crate::embed::Capability`] so a
/// plugin's declared permission and `Engine::deny` speak the same language.
pub fn capability(name: &str) -> Result<Capability, String> {
    match name {
        "filesystem" => Ok(Capability::FileSystem),
        "process" => Ok(Capability::Process),
        "network" => Ok(Capability::Network),
        "environment" => Ok(Capability::Environment),
        other => Err(format!(
            "unknown capability '{}' (filesystem, process, network, environment)",
            other
        )),
    }
}

/// The manifest name for a capability, for diagnostics and `plugin list`.
pub fn capability_name(cap: Capability) -> &'static str {
    match cap {
        Capability::FileSystem => "filesystem",
        Capability::Process => "process",
        Capability::Network => "network",
        Capability::Environment => "environment",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# a comment
[plugin]
name = \"acme.httpkit\"
version = \"1.2.0\"
abi = \"spid3r-plugin-abi-1\"
kind = \"host\"
capabilities = [\"network\"]

[[function]]
name = \"acme_fetch\"
arity = 2
permission = \"network\"

[[function]]
name = \"acme_pure\"
arity = 1
";

    #[test]
    fn parses_the_documented_manifest() {
        let m = PluginManifest::parse(SAMPLE).expect("parse");
        assert_eq!(m.name, "acme.httpkit");
        assert_eq!(m.version, "1.2.0");
        assert!(m.abi_matches());
        assert_eq!(m.kind, PluginKind::Host);
        assert_eq!(m.capabilities, vec![Capability::Network]);
        assert_eq!(m.functions.len(), 2);
        assert_eq!(m.functions[0].name, "acme_fetch");
        assert_eq!(m.functions[0].arity, 2);
        assert_eq!(m.functions[0].permission, Some(Capability::Network));
        // A function with no `permission` is pure.
        assert_eq!(m.functions[1].permission, None);
    }

    #[test]
    fn an_unknown_abi_does_not_match() {
        let text = SAMPLE.replace(ABI_V1, "spid3r-plugin-abi-2");
        let m = PluginManifest::parse(&text).expect("parse");
        assert!(!m.abi_matches(), "abi 2 must not match abi 1");
    }

    #[test]
    fn rejects_unknown_capabilities_and_bad_kinds() {
        let bad_cap = SAMPLE.replace("\"network\"]", "\"telepathy\"]");
        assert!(PluginManifest::parse(&bad_cap)
            .unwrap_err()
            .contains("telepathy"));

        let bad_kind = SAMPLE.replace("kind = \"host\"", "kind = \"elf\"");
        assert!(PluginManifest::parse(&bad_kind)
            .unwrap_err()
            .contains("host"));
    }

    #[test]
    fn rejects_duplicate_function_names() {
        let dup = format!("{}\n[[function]]\nname = \"acme_fetch\"\n", SAMPLE);
        assert!(PluginManifest::parse(&dup)
            .unwrap_err()
            .contains("duplicate function"));
    }
}
