//! Global CLI tool installer (`spkg tool install / list / uninstall`, `pipx`-style).
//!
//! Installs packages declaring `[bin]` into isolated directories under
//! `<state_dir>/tools/<pkg>/`, compiles their `.matrix` entrypoints to
//! SHA-256-verified `.matrixc` bytecode, and places cross-platform launcher
//! shims in `<state_dir>/bin/`.

use std::path::{Path, PathBuf};

use super::lockfile::read_lock;
use super::manifest::{parse_manifest, toml_escape_string, valid_pkg_name, validate_bin_entry};
use super::resolver::{install_opts, InstallOptions};

#[derive(Debug, Clone)]
pub struct ToolInstallReport {
    pub package: String,
    pub version: String,
    pub bins: Vec<String>,
    pub bin_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct InstalledTool {
    pub package: String,
    pub version: String,
    pub bins: Vec<String>,
}

pub fn tools_root() -> PathBuf {
    crate::config::state_dir().join("tools")
}

pub fn tool_bin_dir() -> PathBuf {
    crate::config::state_dir().join("bin")
}

fn compile_tool_entry(
    entry_src: &Path,
    out_matrixc: &Path,
    module_dirs: &[PathBuf],
) -> Result<(), String> {
    let text = std::fs::read_to_string(entry_src)
        .map_err(|e| format!("cannot read {}: {}", entry_src.display(), e))?;
    let file_str = entry_src.to_string_lossy().to_string();
    let file_arc: std::sync::Arc<str> = std::sync::Arc::from(file_str.as_str());
    let program = crate::parser::parse_file(&text, file_arc).map_err(|d| d.render())?;
    let externals = crate::semantic::Externals {
        names: Default::default(),
        module_dirs: module_dirs.to_vec(),
    };
    crate::semantic::check_file_with(&program, true, &file_str, &externals)
        .map_err(|d| d.render())?;
    let (func, _) =
        crate::vm::compile_program(&program, &file_str, module_dirs).map_err(|d| d.render())?;
    let bytes = crate::vm::bytecode::serialize_vmfunc(&func);
    if let Some(parent) = out_matrixc.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {}", parent.display(), e))?;
    }
    std::fs::write(out_matrixc, &bytes)
        .map_err(|e| format!("cannot write {}: {}", out_matrixc.display(), e))?;
    Ok(())
}

fn write_shim(bin_dir: &Path, bin_name: &str, matrixc_path: &Path) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().unwrap_or_else(|_| {
        PathBuf::from(if cfg!(windows) {
            "spid3r.exe"
        } else {
            "spid3r"
        })
    });
    if cfg!(windows) {
        let shim = bin_dir.join(format!("{}.cmd", bin_name));
        let body = format!(
            "@echo off\r\n\"{}\" exec \"{}\" -- %*\r\n",
            exe.display(),
            matrixc_path.display()
        );
        std::fs::write(&shim, body)
            .map_err(|e| format!("cannot write {}: {}", shim.display(), e))?;
        Ok(shim)
    } else {
        let shim = bin_dir.join(bin_name);
        let body = format!(
            "#!/bin/sh\nexec \"{}\" exec \"{}\" -- \"$@\"\n",
            exe.display(),
            matrixc_path.display()
        );
        std::fs::write(&shim, body)
            .map_err(|e| format!("cannot write {}: {}", shim.display(), e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&shim)
                .map_err(|e| format!("cannot stat {}: {}", shim.display(), e))?
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&shim, perms)
                .map_err(|e| format!("cannot chmod {}: {}", shim.display(), e))?;
        }
        Ok(shim)
    }
}

fn read_tool_meta(tool_dir: &Path) -> Option<InstalledTool> {
    let raw = std::fs::read_to_string(tool_dir.join("tool.toml")).ok()?;
    let mut package = String::new();
    let mut version = String::new();
    let mut bins = Vec::new();
    for line in raw.lines() {
        let Some((k, v)) = line.trim().split_once('=') else {
            continue;
        };
        let parsed = super::manifest::parse_string(v.trim()).unwrap_or_default();
        match k.trim() {
            "name" => package = parsed,
            "version" => version = parsed,
            "bins" => {
                bins = parsed
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string)
                    .collect();
            }
            _ => {}
        }
    }
    if package.is_empty() {
        None
    } else {
        Some(InstalledTool {
            package,
            version,
            bins,
        })
    }
}

/// Install a package declaring `[bin]` into `<state_dir>/tools/<name>/` and create
/// executable shims inside `<state_dir>/bin/`.
pub fn tool_install(
    name: &str,
    version_constraint: Option<&str>,
    path: Option<&str>,
    git: Option<&str>,
    rev: Option<&str>,
    force: bool,
) -> Result<ToolInstallReport, String> {
    valid_pkg_name(name)?;
    let tools = tools_root();
    let bin_dir = tool_bin_dir();
    std::fs::create_dir_all(&tools)
        .map_err(|e| format!("cannot create {}: {}", tools.display(), e))?;
    std::fs::create_dir_all(&bin_dir)
        .map_err(|e| format!("cannot create {}: {}", bin_dir.display(), e))?;

    let final_dir = tools.join(name);
    if final_dir.exists() && !force {
        return Err(format!(
            "tool '{}' is already installed (pass --force to reinstall)",
            name
        ));
    }

    let pid = std::process::id();
    let stage_dir = tools.join(format!(".tmp_{}_{}", name, pid));
    let backup_dir = tools.join(format!(".old_{}_{}", name, pid));
    if stage_dir.exists() {
        let _ = std::fs::remove_dir_all(&stage_dir);
    }
    if backup_dir.exists() {
        let _ = std::fs::remove_dir_all(&backup_dir);
    }

    let res = (|| -> Result<ToolInstallReport, String> {
        std::fs::create_dir_all(stage_dir.join("src"))
            .map_err(|e| format!("cannot create {}: {}", stage_dir.display(), e))?;
        let dep_line = match (path, git, version_constraint) {
            (Some(p), None, _) => {
                let abs_p = Path::new(p)
                    .canonicalize()
                    .map_err(|e| format!("cannot resolve '{}': {}", p, e))?;
                format!(
                    "{} = {{ path = \"{}\" }}",
                    name,
                    toml_escape_string(&abs_p.to_string_lossy())
                )
            }
            (None, Some(u), _) => match rev {
                Some(r) => format!(
                    "{} = {{ git = \"{}\", rev = \"{}\" }}",
                    name,
                    toml_escape_string(u),
                    toml_escape_string(r)
                ),
                None => format!("{} = {{ git = \"{}\" }}", name, toml_escape_string(u)),
            },
            (None, None, Some(v)) => {
                format!("{} = {{ version = \"{}\" }}", name, toml_escape_string(v))
            }
            (None, None, None) => {
                let top = super::latest_registry_version(name, None)?;
                format!("{} = {{ version = \"^{}\" }}", name, top)
            }
            _ => return Err("--path and --git are mutually exclusive".to_string()),
        };
        std::fs::write(
            stage_dir.join("spider.toml"),
            format!(
                "[package]\nname = \"__tool_{}\"\nversion = \"0.1.0\"\n\n[dependencies]\n{}\n",
                name, dep_line
            ),
        )
        .map_err(|e| format!("cannot write staging spider.toml: {}", e))?;
        std::fs::write(
            stage_dir.join("src").join("main.matrix"),
            "fn main() { return 0; }\n",
        )
        .map_err(|e| format!("cannot write staging entry: {}", e))?;

        install_opts(&stage_dir, &InstallOptions::default())?;

        let lock = read_lock(&stage_dir)?;
        let installed_ver = lock
            .iter()
            .find(|e| e.name == name)
            .map(|e| e.version.clone())
            .unwrap_or_else(|| "0.0.0".to_string());

        let vendor_pkg = stage_dir.join("vendor").join(name);
        let snap_manifest = std::fs::read_to_string(vendor_pkg.join(".spid3r-manifest.toml"))
            .map_err(|_| format!("installed package '{}' has no manifest snapshot", name))?;
        let pkg_manifest = parse_manifest(&snap_manifest)?;
        if pkg_manifest.bins.is_empty() {
            return Err(format!(
                "package '{}' declares no [bin] executables in spider.toml",
                name
            ));
        }

        let mut mod_dirs = vec![stage_dir.join("vendor"), vendor_pkg.clone()];
        mod_dirs.extend(crate::modules::default_module_dirs());

        let mut installed_bins = Vec::new();
        for (bin_name, entry_rel) in &pkg_manifest.bins {
            validate_bin_entry(bin_name, entry_rel)?;
            let stripped = entry_rel
                .strip_prefix("src/")
                .or_else(|| entry_rel.strip_prefix("src\\"))
                .unwrap_or(entry_rel);
            let entry_src = vendor_pkg.join(stripped);
            if !entry_src.is_file() {
                return Err(format!(
                    "tool '{}': [bin] '{}' target '{}' not found in vendored package",
                    name, bin_name, entry_rel
                ));
            }
            let out_bc_final = final_dir.join("bin").join(format!("{}.matrixc", bin_name));
            let out_bc_stage = stage_dir.join("bin").join(format!("{}.matrixc", bin_name));
            compile_tool_entry(&entry_src, &out_bc_stage, &mod_dirs)?;

            let shim_path = if cfg!(windows) {
                bin_dir.join(format!("{}.cmd", bin_name))
            } else {
                bin_dir.join(bin_name)
            };
            if shim_path.exists() && !force {
                let owned_by_self = final_dir.exists()
                    && read_tool_meta(&final_dir).is_some_and(|m| m.bins.contains(bin_name));
                if !owned_by_self {
                    return Err(format!(
                        "binary '{}' already exists at {} (pass --force to overwrite)",
                        bin_name,
                        shim_path.display()
                    ));
                }
            }
            let _ = out_bc_final;
            installed_bins.push(bin_name.clone());
        }

        std::fs::write(
            stage_dir.join("tool.toml"),
            format!(
                "name = \"{}\"\nversion = \"{}\"\nbins = \"{}\"\n",
                toml_escape_string(name),
                toml_escape_string(&installed_ver),
                toml_escape_string(&installed_bins.join(","))
            ),
        )
        .map_err(|e| format!("cannot write tool.toml: {}", e))?;

        let had_final = final_dir.exists();
        if had_final {
            std::fs::rename(&final_dir, &backup_dir)
                .map_err(|e| format!("cannot stage backup of {}: {}", final_dir.display(), e))?;
        }
        if let Err(e) = std::fs::rename(&stage_dir, &final_dir) {
            if had_final && backup_dir.exists() {
                let _ = std::fs::rename(&backup_dir, &final_dir);
            }
            return Err(format!("cannot finalize {}: {}", final_dir.display(), e));
        }
        if backup_dir.exists() {
            let _ = std::fs::remove_dir_all(&backup_dir);
        }

        for bin_name in &installed_bins {
            let matrixc = final_dir.join("bin").join(format!("{}.matrixc", bin_name));
            write_shim(&bin_dir, bin_name, &matrixc)?;
        }

        Ok(ToolInstallReport {
            package: name.to_string(),
            version: installed_ver,
            bins: installed_bins,
            bin_dir,
        })
    })();

    if res.is_err() {
        let _ = std::fs::remove_dir_all(&stage_dir);
    }
    res
}

/// List all globally installed Spid3r tools (`spkg tool list`).
pub fn tool_list() -> Result<Vec<InstalledTool>, String> {
    let tools = tools_root();
    let mut out = Vec::new();
    if !tools.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(&tools)
        .map_err(|e| format!("cannot read {}: {}", tools.display(), e))?
        .filter_map(|e| e.ok())
    {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let dname = entry.file_name().to_string_lossy().to_string();
        if dname.starts_with(".tmp_") || dname.starts_with(".old_") {
            continue;
        }
        if let Some(meta) = read_tool_meta(&p) {
            out.push(meta);
        }
    }
    out.sort_by(|a, b| a.package.cmp(&b.package));
    Ok(out)
}

/// Remove a globally installed tool and its launcher shims (`spkg tool uninstall <name>`).
pub fn tool_uninstall(name: &str) -> Result<Vec<String>, String> {
    valid_pkg_name(name)?;
    let tool_dir = tools_root().join(name);
    if !tool_dir.is_dir() {
        return Err(format!("tool '{}' is not installed", name));
    }
    let meta = read_tool_meta(&tool_dir);
    let bin_dir = tool_bin_dir();
    let mut removed_bins = Vec::new();
    if let Some(m) = meta {
        for b in m.bins {
            let shim = if cfg!(windows) {
                bin_dir.join(format!("{}.cmd", b))
            } else {
                bin_dir.join(&b)
            };
            if shim.is_file() {
                let _ = std::fs::remove_file(&shim);
            }
            removed_bins.push(b);
        }
    }
    std::fs::remove_dir_all(&tool_dir)
        .map_err(|e| format!("cannot remove {}: {}", tool_dir.display(), e))?;
    Ok(removed_bins)
}
