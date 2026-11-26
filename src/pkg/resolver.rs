//! Two-phase deterministic dependency resolver for both single packages and
//! `[workspace]` roots:
//!
//! 1. **Resolve Phase:** Unify SemVer constraints across direct, workspace, and
//!    transitive dependencies, honoring `spider.lock` pins unless `update` is
//!    set (fixes B2), lazily fetching only needed remote versions (fixes B1),
//!    and checking host/compiler compatibility before touching `vendor/`.
//! 2. **Commit Phase:** Atomically vendor every reachable dependency, prune
//!    orphaned `vendor/<pkg>` directories (fixes B7), and write escaped `spider.lock`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use super::git;
use super::lockfile::{
    locked_registry_version, locked_source_map, read_lock, write_lock_string, LockEntry,
};
use super::manifest::{valid_pkg_name, DepSource, Dependency, Manifest};
use super::registry;
use super::semver::{Constraint, Version};
use super::vendor;

/// Summary returned to `cmd_add` / `cmd_install`.
#[derive(Debug, Default)]
pub struct InstallReport {
    pub deps: Vec<String>,
    pub vendored: usize,
    pub unchanged: usize,
}

/// Options controlling `spkg install` resolution, lockfile strictness, and
/// optional/development dependency groups.
#[derive(Debug, Default, Clone)]
pub struct InstallOptions {
    pub update: bool,
    pub frozen: bool,
    pub offline: bool,
    pub include_dev: bool,
    pub extras: Vec<String>,
}

struct ResolvedPkg {
    name: String,
    dep_dir: PathBuf,
    origin: String,
    version: String,
    is_registry: bool,
    registry_name: Option<String>,
    source: DepSource,
    constraints: Vec<(String, Constraint, String)>,
    used_by: String,
    child_deps: Vec<Dependency>,
}

/// Resolve the highest published `Version` of `name` across configured local
/// directories or remote mirrors.
pub fn latest_registry_version(name: &str, registry: Option<&str>) -> Result<Version, String> {
    valid_pkg_name(name)?;
    let endpoints = registry::registry_endpoints();
    if endpoints.is_empty() {
        return Err(format!(
            "dependency '{}' is a registry dependency, but SPID3R_REGISTRY is not set \
             (point it at a package registry directory or URL; see docs/REGISTRY.md)",
            name
        ));
    }

    let mut best: Option<Version> = None;
    let mut last_err: Option<String> = None;
    for ep in &endpoints {
        if registry::is_remote_url(ep) {
            match registry::fetch_versions_across_mirrors(std::slice::from_ref(ep), registry, name)
            {
                Ok((_, vers)) => {
                    if let Some(top) = vers.into_iter().max() {
                        if best.as_ref().is_none_or(|b| top > *b) {
                            best = Some(top);
                        }
                        break;
                    }
                }
                Err(e) => last_err = Some(e),
            }
        } else {
            let reg_root = match registry {
                Some(sub) => {
                    valid_pkg_name(sub)?;
                    PathBuf::from(ep).join(sub)
                }
                None => PathBuf::from(ep),
            };
            let pkg_dir = reg_root.join(name);
            if !pkg_dir.is_dir() {
                last_err = Some(format!(
                    "registry: no package '{}' (looked in {})",
                    name,
                    pkg_dir.display()
                ));
                continue;
            }
            if let Ok(rd) = std::fs::read_dir(&pkg_dir) {
                for entry in rd.filter_map(|e| e.ok()) {
                    if !entry.path().is_dir() {
                        continue;
                    }
                    let fname = entry.file_name();
                    if let Some(dirname) = fname.to_str() {
                        if let Ok(v) = Version::parse(dirname) {
                            if best.as_ref().is_none_or(|b| v > *b) {
                                best = Some(v);
                            }
                        }
                    }
                }
            }
            if best.is_some() {
                break;
            }
        }
    }

    best.ok_or_else(|| {
        last_err.unwrap_or_else(|| format!("registry: no versions found for '{}'", name))
    })
}

/// Resolve and select the winning registry version dir for `name` matching all
/// `constraints`, honoring `locked_ver` when compatible and `!update`, skipping
/// yanked versions for fresh resolutions, and lazily fetching ONLY the single
/// chosen version (fixes B1 & B2).
fn resolve_registry_version_dir(
    project_root: &Path,
    name: &str,
    constraints: &[(String, Constraint, String)],
    registry: Option<&str>,
    locked_ver: Option<&Version>,
    update: bool,
    offline: bool,
) -> Result<(PathBuf, Version), String> {
    let endpoints = registry::registry_endpoints();
    if endpoints.is_empty() {
        return Err(format!(
            "dependency '{}' is a registry dependency, but SPID3R_REGISTRY is not set \
             (point it at a package registry directory or URL; see docs/REGISTRY.md)",
            name
        ));
    }

    if offline {
        let (cache_pkg_dir, available) =
            registry::fetch_cached_versions(project_root, &endpoints, registry, name)?;
        let chosen = pick_matching_version(
            name,
            &available,
            constraints,
            locked_ver,
            update,
            &cache_pkg_dir.display().to_string(),
        )?;
        let dir = cache_pkg_dir.join(chosen.to_string());
        return Ok((dir, chosen));
    }

    let mut last_err: Option<String> = None;
    for ep in &endpoints {
        if registry::is_remote_url(ep) {
            let (effective_url, available) = match registry::fetch_versions_across_mirrors(
                std::slice::from_ref(ep),
                registry,
                name,
            ) {
                Ok(pair) => pair,
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            };
            let yanked =
                registry::fetch_remote_yanked(&effective_url, registry, name).unwrap_or_default();
            let chosen = pick_matching_version_with_yank(
                name,
                &available,
                &yanked,
                constraints,
                locked_ver,
                update,
                &effective_url,
            )?;
            let dir = registry::fetch_remote_version(
                project_root,
                &effective_url,
                registry,
                name,
                &chosen.to_string(),
                update,
            )?;
            return Ok((dir, chosen));
        }

        let reg_root = match registry {
            Some(sub) => {
                valid_pkg_name(sub)?;
                PathBuf::from(ep).join(sub)
            }
            None => PathBuf::from(ep),
        };
        let pkg_dir = reg_root.join(name);
        if !pkg_dir.is_dir() {
            last_err = Some(format!(
                "registry: no package '{}' (looked in {})",
                name,
                pkg_dir.display()
            ));
            continue;
        }
        let mut available: Vec<Version> = Vec::new();
        let entries = std::fs::read_dir(&pkg_dir)
            .map_err(|e| format!("registry: cannot read {}: {}", pkg_dir.display(), e))?;
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if let Some(dirname) = path.file_name().and_then(|s| s.to_str()) {
                if let Ok(v) = Version::parse(dirname) {
                    available.push(v);
                }
            }
        }
        available.sort();
        let mut yanked = BTreeSet::new();
        let yanked_file = pkg_dir.join("yanked.txt");
        if let Ok(ytext) = std::fs::read_to_string(&yanked_file) {
            for line in ytext.lines() {
                let t = line.trim();
                if !t.is_empty() && !t.starts_with('#') {
                    if let Ok(v) = Version::parse(t) {
                        yanked.insert(v);
                    }
                }
            }
        }
        let chosen = pick_matching_version_with_yank(
            name,
            &available,
            &yanked,
            constraints,
            locked_ver,
            update,
            &pkg_dir.display().to_string(),
        )?;
        let dir = pkg_dir.join(chosen.to_string());
        return Ok((dir, chosen));
    }

    Err(last_err.unwrap_or_else(|| format!("registry: no package '{}' found", name)))
}

fn pick_matching_version(
    name: &str,
    available: &[Version],
    constraints: &[(String, Constraint, String)],
    locked_ver: Option<&Version>,
    update: bool,
    location: &str,
) -> Result<Version, String> {
    pick_matching_version_with_yank(
        name,
        available,
        &BTreeSet::new(),
        constraints,
        locked_ver,
        update,
        location,
    )
}

fn pick_matching_version_with_yank(
    name: &str,
    available: &[Version],
    yanked: &BTreeSet<Version>,
    constraints: &[(String, Constraint, String)],
    locked_ver: Option<&Version>,
    update: bool,
    location: &str,
) -> Result<Version, String> {
    if !update {
        if let Some(locked) = locked_ver {
            if available.contains(locked) && constraints.iter().all(|(_, c, _)| c.matches(locked)) {
                if yanked.contains(locked) {
                    eprintln!(
                        "warning: locked dependency '{}@{}' has been yanked upstream",
                        name, locked
                    );
                }
                return Ok(locked.clone());
            }
        }
    }
    let best = available
        .iter()
        .filter(|v| !yanked.contains(v) && constraints.iter().all(|(_, c, _)| c.matches(v)))
        .max()
        .cloned();
    match best {
        Some(v) => Ok(v),
        None if constraints.len() == 1 => Err(format!(
            "registry: no version of '{}' matches {} (looked in {})",
            name, constraints[0].2, location
        )),
        None => {
            let desc: Vec<String> = constraints
                .iter()
                .map(|(req, _, raw)| {
                    if req.is_empty() {
                        raw.clone()
                    } else {
                        format!("{} ({})", raw, req)
                    }
                })
                .collect();
            Err(format!(
                "registry: no version of '{}' satisfies all conflicting constraints [{}] (looked in {})",
                name,
                desc.join(", "),
                location
            ))
        }
    }
}

fn resolve_source(
    root: &Path,
    base_dir: &Path,
    name: &str,
    source: &DepSource,
    lock_entries: &[LockEntry],
    locked_sources: &BTreeMap<String, String>,
    opts: &InstallOptions,
) -> Result<(PathBuf, String), String> {
    match source {
        DepSource::Path(rel) => {
            let dir = base_dir
                .join(rel)
                .canonicalize()
                .map_err(|e| format!("dependency '{}': cannot resolve '{}': {}", name, rel, e))?;
            Ok((dir, format!("path+{}", rel)))
        }
        DepSource::Git { url, rev } => {
            let pin = if opts.update {
                None
            } else {
                git::locked_commit(locked_sources, name, url)
            };
            if opts.offline {
                let cached = root.join(".spid3r").join("git").join(name);
                if !cached.is_dir() {
                    return Err(format!(
                        "offline mode: git dependency '{}' ({}) has no cached checkout at {}",
                        name,
                        url,
                        cached.display()
                    ));
                }
                let origin = match pin {
                    Some(c) => format!("git+{}#{}", url, c),
                    None => locked_sources
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| format!("git+{}", url)),
                };
                return Ok((cached, origin));
            }
            let (dir, commit) = git::fetch_git(root, name, url, rev.as_deref(), pin.as_deref())?;
            Ok((dir, format!("git+{}#{}", url, commit)))
        }
        DepSource::Registry { version, registry } => {
            let constraint =
                Constraint::parse(version).map_err(|e| format!("dependency '{}': {}", name, e))?;
            let constraints = vec![(String::new(), constraint, version.clone())];
            let locked_v = locked_registry_version(lock_entries, name);
            let (dir, chosen) = resolve_registry_version_dir(
                root,
                name,
                &constraints,
                registry.as_deref(),
                locked_v.as_ref(),
                opts.update,
                opts.offline,
            )?;
            Ok((dir, format!("registry+{}", chosen)))
        }
    }
}

fn check_constraints_match_version(
    name: &str,
    constraints: &[(String, Constraint, String)],
    declared_version: &str,
    declared_pkg_name: &str,
) -> Result<(), String> {
    let actual =
        Version::parse(declared_version).map_err(|e| format!("dependency '{}': {}", name, e))?;
    for (req, constraint, raw) in constraints {
        if !constraint.matches(&actual) {
            if req.is_empty() {
                return Err(format!(
                    "dependency '{}' requires version {}, but {} provides {}",
                    name, raw, declared_pkg_name, declared_version
                ));
            }
            return Err(format!(
                "dependency '{}' (required by {}) requires version {}, but {} provides {}",
                name, req, raw, declared_pkg_name, declared_version
            ));
        }
    }
    Ok(())
}

/// Remove any directories in `vendor/` that are no longer reachable from the
/// resolved dependency graph (fixes B7) and clean leftover temp/backup folders.
fn prune_orphaned_vendor_dirs(root: &Path, keep: &BTreeSet<String>) -> Result<(), String> {
    let vendor_dir = root.join("vendor");
    if !vendor_dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&vendor_dir)
        .map_err(|e| format!("cannot read {}: {}", vendor_dir.display(), e))?
        .filter_map(|e| e.ok())
    {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(".tmp_") || name.starts_with(".old_") {
            let _ = std::fs::remove_dir_all(&p);
            continue;
        }
        if !keep.contains(&name) {
            let meta = std::fs::symlink_metadata(&p)
                .map_err(|e| format!("cannot inspect {}: {}", p.display(), e))?;
            if meta.file_type().is_symlink() || meta.is_file() {
                let _ = std::fs::remove_file(&p);
            } else if meta.is_dir() {
                std::fs::remove_dir_all(&p)
                    .map_err(|e| format!("cannot prune {}: {}", p.display(), e))?;
            }
        }
    }
    Ok(())
}

/// Resolve and vendor every dependency, writing `spider.lock`.
pub fn install(root: &Path) -> Result<InstallReport, String> {
    install_opts(root, &InstallOptions::default())
}

/// Backward-compatible alias taking `update` only.
pub fn install_with(root: &Path, update: bool) -> Result<InstallReport, String> {
    install_opts(
        root,
        &InstallOptions {
            update,
            ..InstallOptions::default()
        },
    )
}

fn collect_root_deps(
    manifest: &Manifest,
    opts: &InstallOptions,
) -> Result<Vec<Dependency>, String> {
    let mut deps = manifest.deps.clone();
    let mut push_unique_dep = |d: &Dependency| -> Result<(), String> {
        if deps.iter().any(|existing| existing.name == d.name) {
            return Err(format!(
                "dependency '{}' appears in multiple dependency groups",
                d.name
            ));
        }
        deps.push(d.clone());
        Ok(())
    };
    if opts.include_dev {
        for d in &manifest.dev_deps {
            push_unique_dep(d)?;
        }
    }
    for extra in &opts.extras {
        let group = manifest.optional_deps.get(extra).ok_or_else(|| {
            format!(
                "unknown optional dependency group '{}' (not declared under [optional-dependencies.{}])",
                extra, extra
            )
        })?;
        for d in group {
            push_unique_dep(d)?;
        }
    }
    Ok(deps)
}

fn verify_frozen_lock_matches(locked: &[LockEntry], planned: &[LockEntry]) -> Result<(), String> {
    if locked.len() != planned.len() {
        return Err(format!(
            "frozen lockfile: spider.lock has {} package(s), but spider.toml resolves {} — run `spkg install` without --frozen",
            locked.len(),
            planned.len()
        ));
    }
    for p in planned {
        let Some(l) = locked
            .iter()
            .find(|e| e.name == p.name && e.used_by == p.used_by)
        else {
            return Err(format!(
                "frozen lockfile: package '{}' is missing from spider.lock — run `spkg install` without --frozen",
                p.name
            ));
        };
        if l.version != p.version || l.source != p.source {
            return Err(format!(
                "frozen lockfile: package '{}' changed ({}@{} vs {}@{}) — run `spkg install` without --frozen",
                p.name, l.version, l.source, p.version, p.source
            ));
        }
    }
    Ok(())
}

/// Two-phase `spkg install` supporting `--frozen`, `--offline`, `--dev`, `--extra`,
/// workspaces, diamond constraint unification, lazy remote version fetch,
/// lockfile registry pinning, per-target wheel verification, and atomic vendoring.
pub fn install_opts(root: &Path, opts: &InstallOptions) -> Result<InstallReport, String> {
    if opts.frozen && opts.update {
        return Err("install: --frozen and --update cannot be used together".to_string());
    }
    let Some(manifest) = Manifest::load(root)? else {
        return Err(format!(
            "no spider.toml in {} — run `spid3r init` first",
            root.display()
        ));
    };
    if !manifest.members.is_empty() {
        return install_workspace(root, &manifest, opts);
    }

    let lock_path = root.join("spider.lock");
    if opts.frozen && !lock_path.is_file() {
        return Err(
            "frozen lockfile: spider.lock does not exist — run `spkg install` first".to_string(),
        );
    }
    let lock_entries = read_lock(root)?;
    let locked_sources = locked_source_map(&lock_entries);
    let mut resolutions: BTreeMap<String, ResolvedPkg> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let root_pkg_name = manifest.name().to_string();
    let root_deps = collect_root_deps(&manifest, opts)?;
    if opts.frozen {
        for dep in &root_deps {
            if !lock_entries.iter().any(|e| e.name == dep.name) {
                return Err(format!(
                    "frozen lockfile: dependency '{}' in spider.toml is missing from spider.lock",
                    dep.name
                ));
            }
        }
    }

    struct QueueItem {
        dep: Dependency,
        base_dir: PathBuf,
        active_chain: Vec<String>,
        requirer: String,
    }

    let mut queue: VecDeque<QueueItem> = VecDeque::new();
    for dep in &root_deps {
        queue.push_back(QueueItem {
            dep: dep.clone(),
            base_dir: root.to_path_buf(),
            active_chain: vec![root_pkg_name.clone()],
            requirer: root_pkg_name.clone(),
        });
    }

    // Phase 1: Graph resolution without mutating vendor/ or spider.lock.
    while let Some(item) = queue.pop_front() {
        let name = item.dep.name.clone();
        if item.active_chain.contains(&name) {
            return Err(format!(
                "circular dependency detected: {} -> {}",
                item.active_chain.join(" -> "),
                name
            ));
        }

        if resolutions.contains_key(&name) {
            let mut updated_node: Option<(PathBuf, Manifest, String)> = None;
            {
                let existing = resolutions.get_mut(&name).unwrap();
                match &item.dep.source {
                    DepSource::Registry { version, registry } if existing.is_registry => {
                        if existing.registry_name != *registry {
                            return Err(format!(
                                "dependency '{}' is declared twice with different sources ({} and {})",
                                name,
                                existing.origin,
                                item.dep.source.describe()
                            ));
                        }
                        let c = Constraint::parse(version)
                            .map_err(|e| format!("dependency '{}': {}", name, e))?;
                        existing.constraints.push((
                            item.requirer.clone(),
                            c.clone(),
                            version.clone(),
                        ));
                        let cur_ver = Version::parse(&existing.version)
                            .map_err(|e| format!("dependency '{}': {}", name, e))?;
                        if !c.matches(&cur_ver) {
                            let locked_v = locked_registry_version(&lock_entries, &name);
                            let (new_dir, chosen_v) = resolve_registry_version_dir(
                                root,
                                &name,
                                &existing.constraints,
                                registry.as_deref(),
                                locked_v.as_ref(),
                                opts.update,
                                opts.offline,
                            )?;
                            let dep_manifest = Manifest::load(&new_dir)?.ok_or_else(|| {
                                format!(
                                    "dependency '{}' ({}) has no spider.toml",
                                    name,
                                    new_dir.display()
                                )
                            })?;
                            dep_manifest.check_host_compat(&name)?;
                            check_constraints_match_version(
                                &name,
                                &existing.constraints,
                                dep_manifest.version(),
                                dep_manifest.name(),
                            )?;
                            updated_node =
                                Some((new_dir, dep_manifest, format!("registry+{}", chosen_v)));
                        }
                    }
                    _ => {
                        let (_dep_dir, origin) = resolve_source(
                            root,
                            &item.base_dir,
                            &name,
                            &item.dep.source,
                            &lock_entries,
                            &locked_sources,
                            opts,
                        )?;
                        if existing.origin != origin {
                            return Err(format!(
                                "dependency '{}' is declared twice with different sources ({} and {})",
                                name, existing.origin, origin
                            ));
                        }
                        if let Some(constraint_text) = item.dep.version.as_deref() {
                            let constraint = Constraint::parse(constraint_text)
                                .map_err(|e| format!("dependency '{}': {}", name, e))?;
                            let actual = Version::parse(&existing.version)
                                .map_err(|e| format!("dependency '{}': {}", name, e))?;
                            if !constraint.matches(&actual) {
                                return Err(format!(
                                    "dependency '{}' requires version {}, but {} provides {}",
                                    name, constraint_text, name, existing.version
                                ));
                            }
                        }
                    }
                }
            }
            if let Some((new_dir, dep_manifest, new_origin)) = updated_node {
                let existing = resolutions.get_mut(&name).unwrap();
                existing.dep_dir = new_dir.clone();
                existing.origin = new_origin;
                existing.version = dep_manifest.version().to_string();
                existing.child_deps = dep_manifest.deps.clone();
                let mut next_chain = item.active_chain.clone();
                next_chain.push(name.clone());
                for child_dep in dep_manifest.deps {
                    queue.push_back(QueueItem {
                        dep: child_dep,
                        base_dir: new_dir.clone(),
                        active_chain: next_chain.clone(),
                        requirer: name.clone(),
                    });
                }
            }
            continue;
        }

        let (dep_dir, origin) = resolve_source(
            root,
            &item.base_dir,
            &name,
            &item.dep.source,
            &lock_entries,
            &locked_sources,
            opts,
        )?;
        let dep_manifest = Manifest::load(&dep_dir)?.ok_or_else(|| {
            format!(
                "dependency '{}' ({}) has no spider.toml",
                name,
                dep_dir.display()
            )
        })?;
        dep_manifest.check_host_compat(&name)?;

        let mut constraints = Vec::new();
        if let Some(constraint_text) = item.dep.version.as_deref() {
            let constraint = Constraint::parse(constraint_text)
                .map_err(|e| format!("dependency '{}': {}", name, e))?;
            constraints.push((
                item.requirer.clone(),
                constraint,
                constraint_text.to_string(),
            ));
            check_constraints_match_version(
                &name,
                &constraints,
                dep_manifest.version(),
                dep_manifest.name(),
            )?;
        }

        let (is_registry, registry_name) = match &item.dep.source {
            DepSource::Registry { registry, .. } => (true, registry.clone()),
            _ => (false, None),
        };
        let child_deps = dep_manifest.deps.clone();
        resolutions.insert(
            name.clone(),
            ResolvedPkg {
                name: name.clone(),
                dep_dir: dep_dir.clone(),
                origin,
                version: dep_manifest.version().to_string(),
                is_registry,
                registry_name,
                source: item.dep.source.clone(),
                constraints,
                used_by: String::new(),
                child_deps: child_deps.clone(),
            },
        );
        order.push(name.clone());

        let mut next_chain = item.active_chain.clone();
        next_chain.push(name.clone());
        for child_dep in child_deps {
            queue.push_back(QueueItem {
                dep: child_dep,
                base_dir: dep_dir.clone(),
                active_chain: next_chain.clone(),
                requirer: name.clone(),
            });
        }
    }

    // Recompute reachable packages from root manifest so diamond re-resolutions
    // never leave orphaned transitive packages behind (fixes B7).
    let mut reachable: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut walk_q: VecDeque<String> = root_deps.iter().map(|d| d.name.clone()).collect();
    while let Some(pkg) = walk_q.pop_front() {
        if !seen.insert(pkg.clone()) {
            continue;
        }
        reachable.push(pkg.clone());
        if let Some(node) = resolutions.get(&pkg) {
            for child in &node.child_deps {
                walk_q.push_back(child.name.clone());
            }
        }
    }

    if opts.frozen {
        let planned_entries: Vec<LockEntry> = order
            .iter()
            .filter(|pkg| seen.contains(*pkg))
            .filter_map(|pkg| resolutions.get(pkg))
            .map(|res| LockEntry {
                name: res.name.clone(),
                version: res.version.clone(),
                source: res.origin.clone(),
                files: None,
                sha256: None,
                manifest_sha256: None,
                target: None,
                native_sha256: None,
                ffi_libs: Vec::new(),
                used_by: None,
            })
            .collect();
        verify_frozen_lock_matches(&lock_entries, &planned_entries)?;
    }

    // Phase 2: Atomic vendoring + orphan pruning + escaped lockfile write.
    let mut report = InstallReport::default();
    let mut final_entries: Vec<LockEntry> = Vec::new();
    for pkg in &order {
        if !seen.contains(pkg) {
            continue;
        }
        if let Some(res) = resolutions.get(pkg) {
            let vendored = vendor::vendor_dep_checked(root, &res.name, &res.dep_dir, &res.source)?;
            report.deps.push(res.name.clone());
            report.vendored += vendored.files;
            final_entries.push(LockEntry {
                name: res.name.clone(),
                version: res.version.clone(),
                source: res.origin.clone(),
                files: Some(vendored.files),
                sha256: Some(vendored.sha256),
                manifest_sha256: vendored.manifest_sha256,
                target: vendored.target,
                native_sha256: vendored.native_sha256,
                ffi_libs: vendored.ffi_libs,
                used_by: None,
            });
        }
    }

    prune_orphaned_vendor_dirs(root, &seen)?;

    if opts.frozen {
        report.unchanged += 1;
        return Ok(report);
    }

    let header = format!(
        "# generated by spid3r install; do not edit\n# package {} {}",
        manifest.name(),
        manifest.version()
    );
    let lock = write_lock_string(&header, &final_entries);
    let old = std::fs::read_to_string(&lock_path).unwrap_or_default();
    if old == lock {
        report.unchanged += 1;
    } else {
        std::fs::write(&lock_path, lock)
            .map_err(|e| format!("cannot write {}: {}", lock_path.display(), e))?;
    }
    Ok(report)
}

fn install_workspace(
    root: &Path,
    manifest: &Manifest,
    opts: &InstallOptions,
) -> Result<InstallReport, String> {
    let lock_path = root.join("spider.lock");
    if opts.frozen && !lock_path.is_file() {
        return Err(
            "frozen lockfile: spider.lock does not exist — run `spkg install` first".to_string(),
        );
    }
    let lock_entries = read_lock(root)?;
    let locked_sources = locked_source_map(&lock_entries);
    let mut resolutions: BTreeMap<String, ResolvedPkg> = BTreeMap::new();
    let mut member_blocks: Vec<(String, String, Vec<String>)> = Vec::new();
    let mut member_roots: Vec<(String, Vec<Dependency>)> = Vec::new();

    struct WsQueueItem {
        dep: Dependency,
        base_dir: PathBuf,
        active_chain: Vec<String>,
        member: String,
        requirer: String,
    }

    for member in &manifest.members {
        let member_dir = root
            .join(member)
            .canonicalize()
            .map_err(|e| format!("workspace member '{}': cannot resolve: {}", member, e))?;
        let member_manifest = Manifest::load(&member_dir)?.ok_or_else(|| {
            format!(
                "workspace member '{}' ({}) has no spider.toml",
                member,
                member_dir.display()
            )
        })?;
        member_roots.push((member.clone(), member_manifest.deps.clone()));

        let mut member_pkgs: Vec<String> = Vec::new();
        let mut queue: VecDeque<WsQueueItem> = VecDeque::new();
        for dep in &member_manifest.deps {
            queue.push_back(WsQueueItem {
                dep: dep.clone(),
                base_dir: member_dir.clone(),
                active_chain: vec![member.clone()],
                member: member.clone(),
                requirer: member.clone(),
            });
        }

        while let Some(item) = queue.pop_front() {
            let name = item.dep.name.clone();
            if item.active_chain.contains(&name) {
                return Err(format!(
                    "circular dependency detected: {} -> {}",
                    item.active_chain.join(" -> "),
                    name
                ));
            }

            if resolutions.contains_key(&name) {
                let mut updated_node: Option<(PathBuf, Manifest, String)> = None;
                {
                    let existing = resolutions.get_mut(&name).unwrap();
                    match &item.dep.source {
                        DepSource::Registry { version, registry } if existing.is_registry => {
                            if existing.registry_name != *registry {
                                return Err(format!(
                                    "workspace: dependency '{}' is declared twice with different sources ({} and {})",
                                    name,
                                    existing.origin,
                                    item.dep.source.describe()
                                ));
                            }
                            let c = Constraint::parse(version)
                                .map_err(|e| format!("dependency '{}': {}", name, e))?;
                            existing.constraints.push((
                                item.requirer.clone(),
                                c.clone(),
                                version.clone(),
                            ));
                            let cur_ver = Version::parse(&existing.version)
                                .map_err(|e| format!("dependency '{}': {}", name, e))?;
                            if !c.matches(&cur_ver) {
                                let locked_v = locked_registry_version(&lock_entries, &name);
                                let (new_dir, chosen_v) = resolve_registry_version_dir(
                                    root,
                                    &name,
                                    &existing.constraints,
                                    registry.as_deref(),
                                    locked_v.as_ref(),
                                    opts.update,
                                    opts.offline,
                                )?;
                                let dep_manifest = Manifest::load(&new_dir)?.ok_or_else(|| {
                                    format!(
                                        "dependency '{}' ({}) has no spider.toml",
                                        name,
                                        new_dir.display()
                                    )
                                })?;
                                dep_manifest.check_host_compat(&name)?;
                                check_constraints_match_version(
                                    &name,
                                    &existing.constraints,
                                    dep_manifest.version(),
                                    dep_manifest.name(),
                                )?;
                                updated_node =
                                    Some((new_dir, dep_manifest, format!("registry+{}", chosen_v)));
                            }
                        }
                        _ => {
                            let (_dep_dir, origin) = resolve_source(
                                root,
                                &item.base_dir,
                                &name,
                                &item.dep.source,
                                &lock_entries,
                                &locked_sources,
                                opts,
                            )?;
                            if existing.origin != origin {
                                return Err(format!(
                                    "workspace: dependency '{}' is declared twice with different sources ({} and {})",
                                    name, existing.origin, origin
                                ));
                            }
                            if let Some(constraint_text) = item.dep.version.as_deref() {
                                let constraint = Constraint::parse(constraint_text)
                                    .map_err(|e| format!("dependency '{}': {}", name, e))?;
                                let actual = Version::parse(&existing.version)
                                    .map_err(|e| format!("dependency '{}': {}", name, e))?;
                                if !constraint.matches(&actual) {
                                    return Err(format!(
                                        "dependency '{}' requires version {}, but {} provides {}",
                                        name, constraint_text, name, existing.version
                                    ));
                                }
                            }
                        }
                    }
                }
                if let Some((new_dir, dep_manifest, new_origin)) = updated_node {
                    let existing = resolutions.get_mut(&name).unwrap();
                    existing.dep_dir = new_dir.clone();
                    existing.origin = new_origin;
                    existing.version = dep_manifest.version().to_string();
                    existing.child_deps = dep_manifest.deps.clone();
                    let mut next_chain = item.active_chain.clone();
                    next_chain.push(name.clone());
                    for child_dep in dep_manifest.deps {
                        queue.push_back(WsQueueItem {
                            dep: child_dep,
                            base_dir: new_dir.clone(),
                            active_chain: next_chain.clone(),
                            member: item.member.clone(),
                            requirer: name.clone(),
                        });
                    }
                }
                continue;
            }

            let (dep_dir, origin) = resolve_source(
                root,
                &item.base_dir,
                &name,
                &item.dep.source,
                &lock_entries,
                &locked_sources,
                opts,
            )?;
            let dep_manifest = Manifest::load(&dep_dir)?.ok_or_else(|| {
                format!(
                    "dependency '{}' ({}) has no spider.toml",
                    name,
                    dep_dir.display()
                )
            })?;
            dep_manifest.check_host_compat(&name)?;

            let mut constraints = Vec::new();
            if let Some(constraint_text) = item.dep.version.as_deref() {
                let constraint = Constraint::parse(constraint_text)
                    .map_err(|e| format!("dependency '{}': {}", name, e))?;
                constraints.push((
                    item.requirer.clone(),
                    constraint,
                    constraint_text.to_string(),
                ));
                check_constraints_match_version(
                    &name,
                    &constraints,
                    dep_manifest.version(),
                    dep_manifest.name(),
                )?;
            }

            let (is_registry, registry_name) = match &item.dep.source {
                DepSource::Registry { registry, .. } => (true, registry.clone()),
                _ => (false, None),
            };
            let child_deps = dep_manifest.deps.clone();
            resolutions.insert(
                name.clone(),
                ResolvedPkg {
                    name: name.clone(),
                    dep_dir: dep_dir.clone(),
                    origin,
                    version: dep_manifest.version().to_string(),
                    is_registry,
                    registry_name,
                    source: item.dep.source.clone(),
                    constraints,
                    used_by: item.member.clone(),
                    child_deps: child_deps.clone(),
                },
            );
            member_pkgs.push(name.clone());

            let mut next_chain = item.active_chain.clone();
            next_chain.push(name.clone());
            for child_dep in child_deps {
                queue.push_back(WsQueueItem {
                    dep: child_dep,
                    base_dir: dep_dir.clone(),
                    active_chain: next_chain.clone(),
                    member: item.member.clone(),
                    requirer: name.clone(),
                });
            }
        }
        member_blocks.push((
            member.clone(),
            member_manifest.version().to_string(),
            member_pkgs,
        ));
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut walk_q: VecDeque<String> = member_roots
        .iter()
        .flat_map(|(_, deps)| deps.iter().map(|d| d.name.clone()))
        .collect();
    while let Some(pkg) = walk_q.pop_front() {
        if !seen.insert(pkg.clone()) {
            continue;
        }
        if let Some(node) = resolutions.get(&pkg) {
            for child in &node.child_deps {
                walk_q.push_back(child.name.clone());
            }
        }
    }

    if opts.frozen {
        let planned_entries: Vec<LockEntry> = member_blocks
            .iter()
            .flat_map(|(_, _, pkgs)| pkgs.iter())
            .filter(|pkg| seen.contains(*pkg))
            .filter_map(|pkg| resolutions.get(pkg))
            .map(|res| LockEntry {
                name: res.name.clone(),
                version: res.version.clone(),
                source: res.origin.clone(),
                files: None,
                sha256: None,
                manifest_sha256: None,
                target: None,
                native_sha256: None,
                ffi_libs: Vec::new(),
                used_by: Some(res.used_by.clone()),
            })
            .collect();
        verify_frozen_lock_matches(&lock_entries, &planned_entries)?;
    }

    let mut report = InstallReport::default();
    let mut lock_header = String::from("# generated by spid3r install; do not edit\n# workspace\n");
    let mut final_entries: Vec<LockEntry> = Vec::new();
    for (member, mver, pkgs) in member_blocks {
        lock_header.push_str(&format!("# member {} ({})\n", member, mver));
        for pkg in pkgs {
            if !seen.contains(&pkg) {
                continue;
            }
            if let Some(res) = resolutions.get(&pkg) {
                let vendored =
                    vendor::vendor_dep_checked(root, &res.name, &res.dep_dir, &res.source)?;
                report.deps.push(res.name.clone());
                report.vendored += vendored.files;
                final_entries.push(LockEntry {
                    name: res.name.clone(),
                    version: res.version.clone(),
                    source: res.origin.clone(),
                    files: Some(vendored.files),
                    sha256: Some(vendored.sha256),
                    manifest_sha256: vendored.manifest_sha256,
                    target: vendored.target,
                    native_sha256: vendored.native_sha256,
                    ffi_libs: vendored.ffi_libs,
                    used_by: Some(res.used_by.clone()),
                });
            }
        }
    }

    prune_orphaned_vendor_dirs(root, &seen)?;

    if opts.frozen {
        report.unchanged += 1;
        return Ok(report);
    }

    let lock = write_lock_string(&lock_header, &final_entries);
    let old = std::fs::read_to_string(&lock_path).unwrap_or_default();
    if old == lock {
        report.unchanged += 1;
    } else {
        std::fs::write(&lock_path, lock)
            .map_err(|e| format!("cannot write {}: {}", lock_path.display(), e))?;
    }
    Ok(report)
}
