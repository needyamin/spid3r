//! Module loading: `import "name";` resolves relative to the importing file
//! first, then the documented search path. Modules are cached by canonical
//! path. Circular imports fail. No package manager in v1.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::ast::Program;
use crate::diagnostics::{Diagnostic, Phase, Span};
use crate::interpreter::{Flow, Interp};
use crate::parser;
use crate::sched::Scheduler;
use crate::semantic;
use crate::value::Value;

#[derive(Default)]
pub struct ModuleCache {
    /// Exports per resolved module path: `(name, value, is_const)`.
    pub loaded: HashMap<PathBuf, Vec<(String, Value, bool)>>,
    /// Modules currently being loaded, with the thread that claimed them.
    /// Claiming and consulting happen under the same lock as `loaded`, so two
    /// tasks importing the same new module cannot both load it (the
    /// check-then-act race the audit found): the second task *waits*.
    pub in_progress: Vec<(PathBuf, std::thread::ThreadId)>,
}

/// The result of trying to claim a module path for loading.
pub enum Claim {
    /// Served from the cache; the exports are the load's result.
    AlreadyLoaded(Vec<(String, Value, bool)>),
    /// The *current thread* already has this path in flight — a genuine
    /// import cycle.
    Cyclic(String),
    /// This thread may load the module; the [`LoadClaim`] guard releases the
    /// path on drop (including a panic), waking any waiter.
    Acquired(LoadClaim),
}

/// Atomically claim `resolved` for loading: consult the cache, treat a
/// same-thread in-flight claim as a cycle, and **wait** while another task is
/// loading the same path (then re-check the cache; if that load failed, retry
/// the claim here so the error surfaces with this task's span).
pub fn claim_load(modules: &Arc<Mutex<ModuleCache>>, resolved: &Path, path: &str) -> Claim {
    let mut guard = modules.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if let Some(exports) = guard.loaded.get(resolved) {
            return Claim::AlreadyLoaded(exports.clone());
        }
        let owner = guard
            .in_progress
            .iter()
            .find(|(p, _)| p == resolved)
            .map(|(_, t)| *t);
        match owner {
            Some(t) if t == std::thread::current().id() => {
                // Our own chain is loading this path: a genuine cycle.
                let chain: Vec<PathBuf> =
                    guard.in_progress.iter().map(|(p, _)| p.clone()).collect();
                return Claim::Cyclic(circular_import_message(&chain, path));
            }
            Some(_) => {
                // Another task is loading it: wait for it to finish, then
                // re-check (the loop re-serves from the cache or retries).
                guard = LOAD_DONE.wait(guard).unwrap_or_else(|e| e.into_inner());
            }
            None => {
                guard
                    .in_progress
                    .push((resolved.to_path_buf(), std::thread::current().id()));
                return Claim::Acquired(LoadClaim {
                    modules: modules.clone(),
                    resolved: resolved.to_path_buf(),
                    done: false,
                });
            }
        }
    }
}

/// Signalled whenever a path leaves `in_progress`. A module-level condvar
/// (rather than a field) so `claim_load` can move its `MutexGuard` into
/// `wait` — the pairing rule is that every user of this condvar holds the
/// module cache's mutex.
static LOAD_DONE: std::sync::Condvar = std::sync::Condvar::new();

/// A held load claim: releases the path and wakes waiters on drop, so a
/// panicked loader cannot leave other tasks waiting forever.
pub struct LoadClaim {
    modules: Arc<Mutex<ModuleCache>>,
    resolved: PathBuf,
    done: bool,
}

impl LoadClaim {
    /// Record successful exports and release the claim.
    pub fn complete(mut self, exports: Vec<(String, Value, bool)>) {
        self.done = true;
        let mut cache = self.modules.lock().unwrap_or_else(|e| e.into_inner());
        cache.finish(&self.resolved, Some(exports));
    }
}

impl Drop for LoadClaim {
    fn drop(&mut self) {
        if !self.done {
            let mut cache = self.modules.lock().unwrap_or_else(|e| e.into_inner());
            cache.finish(&self.resolved, None);
        }
    }
}

impl ModuleCache {
    pub fn new() -> Self {
        ModuleCache {
            loaded: HashMap::new(),
            in_progress: Vec::new(),
        }
    }

    /// Complete a claim: record the exports (or just release the path when
    /// the load failed) and wake every waiter.
    pub fn finish(&mut self, resolved: &Path, exports: Option<Vec<(String, Value, bool)>>) {
        self.in_progress.retain(|(p, _)| p != resolved);
        if let Some(exports) = exports {
            self.loaded.insert(resolved.to_path_buf(), exports);
        }
        LOAD_DONE.notify_all();
    }
}

/// Build the diagnostic message for a circular import: the in-progress load
/// chain (`a.matrix -> b.matrix`) followed by the offending path, in the
/// wording shared by the tree interpreter and the VM. Mirrors the tree's
/// chain rendering exactly: file names when available, full paths otherwise.
pub fn circular_import_message(in_progress: &[PathBuf], path: &str) -> String {
    let mut chain: Vec<String> = in_progress
        .iter()
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| p.to_string_lossy().to_string())
        })
        .collect();
    chain.push(path.to_string());
    format!(
        "circular import of '{}' (chain: {})",
        path,
        chain.join(" -> ")
    )
}

/// Import paths are search-relative by contract (importing file's directory,
/// then SPID3R_PATH / vendor walk-up / ./stdlib). Absolute paths, drive/UNC
/// prefixes, root-anchored forms, and `..` components would escape those
/// search roots entirely, so both resolution paths (semantic checking and
/// loading) reject them with a clear diagnostic.
pub fn check_import_path(path: &str) -> Result<(), Diagnostic> {
    let p = std::path::Path::new(path);
    if p.is_absolute()
        || p.has_root()
        || path.contains(':')
        || p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(Diagnostic::new(
            Phase::Semantic,
            format!(
                "invalid module path '{}': imports are relative to the importing file and the module search path (no absolute paths, drive letters, or '..')",
                path
            ),
        ));
    }
    Ok(())
}

fn resolve(interp: &Interp, path: &str) -> Result<PathBuf, Diagnostic> {
    check_import_path(path)?;
    let candidates: Vec<PathBuf> = {
        let mut v = Vec::new();
        let base = PathBuf::from(interp.file.as_ref());
        if let Some(dir) = base.parent() {
            v.push(dir.join(path));
            v.push(dir.join(format!("{}.matrix", path)));
        }
        for d in &interp.module_dirs {
            v.push(d.join(path));
            v.push(d.join(format!("{}.matrix", path)));
        }
        v
    };
    for c in candidates {
        if c.is_file() {
            // Canonicalize so the module cache and cycle detection key on the
            // real file, not the import spelling: `import "lib/util"` and
            // `import "./lib/util"` (or a Windows case variant) must resolve
            // to ONE module instance. Strip the `\\?\` device prefix Windows
            // canonicalize adds for local drives so diagnostics keep showing
            // ordinary paths (UNC paths keep their prefix).
            return Ok(clean_canonical(&c));
        }
    }
    if let Some(p) = embedded_path(path) {
        return Ok(p);
    }
    Err(Diagnostic::new(
        Phase::Semantic,
        format!(
            "module '{}' not found (looked next to the importing file and on SPID3R_PATH)",
            path
        ),
    ))
}

/// `fs::canonicalize`, with Windows' `\\?\C:\...` device prefix reduced back
/// to `C:\...` for local drives (UNC `\\?\UNC\...` paths are kept as-is).
/// Shared by the interpreter's module cache, the VM compiler (import keys),
/// and the VM's module cache so all three key modules identically.
pub fn canonical_key(p: &std::path::Path) -> PathBuf {
    let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = canon.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if rest.len() >= 2 && rest.as_bytes()[1] == b':' {
            return PathBuf::from(rest);
        }
    }
    canon
}

fn clean_canonical(p: &std::path::Path) -> PathBuf {
    canonical_key(p)
}

/// Load a module and return its exported names with their mutability
/// (`true` = `const`). Imported names are defined by the caller in the
/// importing scope.
/// Prefix of the synthetic path a module gets when it is served from the
/// embedded stdlib table instead of the filesystem.
pub const EMBEDDED_PREFIX: &str = "<stdlib>/";

/// Embedded stdlib source for `name`.
///
/// The table is compiled into every build, not just wasm32. On native targets
/// it is only consulted after the filesystem search fails (see [`resolve`]),
/// so a project-local `stdlib/` still wins. Without it, a single-file app that
/// ships `.matrix` source (`--embed app.matrix`) could not resolve a single
/// stdlib import on a machine with no Spid3r install: the payload's synthetic
/// `<exe>/app.matrix` path has no `stdlib/` beside it and the extracted runtime
/// lives alone in the user cache.
fn embedded_module(name: &str) -> Option<&'static str> {
    crate::stdlib_embed::source(name)
}

/// The synthetic path for an embedded module, if this build has one.
///
/// Every resolver (`resolve` here, the checker's, and the VM compiler's) falls
/// back to this, so `import "json"` works in a browser with no filesystem.
pub fn embedded_path(path: &str) -> Option<PathBuf> {
    embedded_module(path).map(|_| PathBuf::from(format!("{}{}.matrix", EMBEDDED_PREFIX, path)))
}

/// Read a module's source. Marker paths from [`embedded_path`] resolve to the
/// embedded table; everything else is a normal file read.
pub fn read_module_source(path: &std::path::Path) -> std::io::Result<String> {
    if let Some(name) = path.to_str().and_then(|s| s.strip_prefix(EMBEDDED_PREFIX)) {
        if let Some(src) = embedded_module(name.trim_end_matches(".matrix")) {
            return Ok(src.to_string());
        }
    }
    std::fs::read_to_string(path)
}

pub fn load(
    interp: &Interp,
    path: &str,
    span: &Span,
) -> Result<Vec<(String, Value, bool)>, Diagnostic> {
    let resolved = resolve(interp, path)?;
    // Claim the path atomically with the cache check: two tasks importing the
    // same new module must load it exactly once (the second waits), and a
    // same-thread re-entry is the genuine circular-import error.
    let _claim = match crate::modules::claim_load(&interp.modules, &resolved, path) {
        Claim::AlreadyLoaded(exports) => return Ok(exports),
        Claim::Cyclic(message) => {
            return Err(Diagnostic::at(Phase::Semantic, span.clone(), message))
        }
        Claim::Acquired(claim) => claim,
    };
    let src = read_module_source(&resolved).map_err(|e| {
        Diagnostic::at(
            Phase::Semantic,
            span.clone(),
            format!("cannot import '{}': {}", path, e),
        )
    })?;
    let file: Arc<str> = Arc::from(resolved.to_string_lossy().to_string());
    let program: Program = parser::parse_file(&src, file.clone())?;
    // Check the module with its REAL path: the semantic pass resolves the
    // module's own `import`s relative to the importing file, and "<memory>"
    // (the default) would break that for any module that imports another
    // module from a different working directory.
    semantic::check_file(&program, false, &resolved.to_string_lossy())?;
    // A module is a program too: it goes through the same lowering seam as the
    // entry file, so a desugaring rule applies inside imported code as well.
    let lowered = crate::lower::lower(&program);
    let program = lowered.as_ref();

    let mut module_dirs = interp.module_dirs.clone();
    if let Some(dir) = resolved.parent() {
        module_dirs.push(dir.to_path_buf());
    }
    let minterp = Interp::module_interp(
        file,
        module_dirs,
        interp.sched.clone(),
        interp.modules.clone(),
        interp.globals.clone(),
        interp.budget_handle(),
    );
    let mut result: Result<Vec<(String, Value)>, Diagnostic> = Ok(Vec::new());
    // A panic while the module body runs releases the module's mutexes (see
    // the interpreter's `GuardSweep`).
    let _guard_sweep = crate::interpreter::GuardSweepPublic::new(&minterp);
    for stmt in &program.stmts {
        // Fatal (budget exhaustion) propagates like an error here; the
        // importer re-raises it as fatal (see `Stmt::Import`).
        if let Flow::Error(d) | Flow::Fatal(d) = minterp.exec_stmt(stmt, &minterp.globals) {
            result = Err(d);
            break;
        }
    }
    result?;

    let public = minterp.public_names.lock().unwrap().clone();
    // Const-ness of public bindings, straight from the module AST: a `public
    // const x` export must stay immutable in the importer too.
    let const_names: std::collections::HashSet<String> = program
        .stmts
        .iter()
        .filter_map(|s| match s {
            crate::ast::Stmt::Const { name, vis, .. } if *vis == crate::ast::Vis::Public => {
                Some(name.clone())
            }
            _ => None,
        })
        .collect();
    let mut exports = Vec::new();
    for name in public {
        if let Some(v) = crate::environment::get(&minterp.globals, &name) {
            exports.push((name.clone(), v, const_names.contains(&name)));
        }
    }
    // Record the exports and release the claim (waking any task that waited).
    _claim.complete(exports.clone());
    Ok(exports)
}

/// Search path from the SPID3R_PATH environment variable, plus ./stdlib.
pub fn default_module_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(path) = std::env::var("SPID3R_PATH") {
        for part in path.split(';') {
            for d in std::env::split_paths(part.trim()) {
                let s = d.to_string_lossy();
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    dirs.push(PathBuf::from(trimmed));
                }
            }
        }
    }
    dirs.push(PathBuf::from("stdlib"));
    // The stdlib shipped beside the executable (installer / release tarball
    // layout) or in an ancestor directory (development layout e.g. target/release/../../stdlib),
    // so `import "http"` (and implicit "bigint" for 123n) works from any working directory.
    if let Ok(exe) = std::env::current_exe() {
        let mut cur = exe.parent();
        while let Some(dir) = cur {
            let shipped = dir.join("stdlib");
            if shipped.is_dir() {
                dirs.push(shipped);
                break;
            }
            cur = dir.parent();
        }
    }
    // Project dependencies vendored by `spid3r install`. Inside a workspace
    // member the shared `vendor/` lives at the workspace root, so walk up.
    if let Some(vendor) = find_vendor_dir() {
        dirs.push(vendor);
    }
    dirs
}

/// Nearest `vendor/` directory at or above the current working directory.
pub fn find_vendor_dir() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join("vendor");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Public names of an imported module with their mutability, for the semantic
/// pass. Resolves and parses the module; full checking happens when the module
/// is loaded at run time. `true` = the name is a `const` binding, so an
/// assignment to it in the importer is rejected at check time.
pub fn public_names_of(
    path: &str,
    importer_file: &str,
    span: &Span,
) -> Result<Vec<(String, bool)>, Diagnostic> {
    public_names_of_in(path, importer_file, span, &[])
}

/// Like [`public_names_of`], with extra module search roots appended after
/// the defaults (embedding API: the host's `add_module_dir` directories).
pub fn public_names_of_in(
    path: &str,
    importer_file: &str,
    span: &Span,
    extra_dirs: &[std::path::PathBuf],
) -> Result<Vec<(String, bool)>, Diagnostic> {
    module_info_of_in(path, importer_file, span, extra_dirs).map(|(names, _)| names)
}

/// What a module scan returns: the exported names (name, is_const) and the
/// unexported declarations found while scanning ((module display name, local
/// name, declaration kind)) — the latter feeds the unexported-symbol
/// diagnostic. Factored out so the signature stays under clippy's
/// type-complexity limit (CI runs clippy with `-D warnings`).
pub type ModuleScan = (Vec<(String, bool)>, Vec<(String, String, &'static str)>);

pub fn module_info_of_in(
    path: &str,
    importer_file: &str,
    span: &Span,
    extra_dirs: &[std::path::PathBuf],
) -> Result<ModuleScan, Diagnostic> {
    check_import_path(path)?;
    let base = PathBuf::from(importer_file);
    let mut dirs = default_module_dirs();
    dirs.extend(extra_dirs.iter().cloned());
    if let Some(dir) = base.parent() {
        dirs.insert(0, dir.to_path_buf());
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    for d in &dirs {
        candidates.push(d.join(path));
        candidates.push(d.join(format!("{}.matrix", path)));
    }
    let resolved = candidates
        .into_iter()
        .find(|c| c.is_file())
        .or_else(|| embedded_path(path))
        .ok_or_else(|| {
            Diagnostic::at(
                Phase::Semantic,
                span.clone(),
                format!("module '{}' not found", path),
            )
        })?;
    let src = read_module_source(&resolved).map_err(|e| {
        Diagnostic::at(
            Phase::Semantic,
            span.clone(),
            format!("cannot import '{}': {}", path, e),
        )
    })?;
    let file: Arc<str> = Arc::from(resolved.to_string_lossy().to_string());
    let program = parser::parse_file(&src, file)?;
    let mod_display = resolved
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string();
    let mut names = Vec::new();
    let mut unexported = Vec::new();
    for stmt in &program.stmts {
        match stmt {
            crate::ast::Stmt::Fn(f) => {
                if let Some(n) = &f.name {
                    if f.vis == crate::ast::Vis::Public {
                        names.push((n.clone(), false));
                    } else {
                        unexported.push((n.clone(), mod_display.clone(), "fn"));
                    }
                }
            }
            crate::ast::Stmt::Class(c) => {
                if c.vis == crate::ast::Vis::Public {
                    names.push((c.name.clone(), false));
                } else {
                    unexported.push((c.name.clone(), mod_display.clone(), "class"));
                }
            }
            crate::ast::Stmt::Interface(i) => {
                if i.vis == crate::ast::Vis::Public {
                    names.push((i.name.clone(), false));
                } else {
                    unexported.push((i.name.clone(), mod_display.clone(), "interface"));
                }
            }
            crate::ast::Stmt::Enum(e) => {
                if e.vis == crate::ast::Vis::Public {
                    for v in &e.variants {
                        names.push((v.name.clone(), false));
                    }
                } else {
                    for v in &e.variants {
                        unexported.push((v.name.clone(), mod_display.clone(), "enum"));
                    }
                }
            }
            crate::ast::Stmt::Let { name, vis, .. } => {
                if *vis == crate::ast::Vis::Public {
                    names.push((name.clone(), false));
                } else {
                    unexported.push((name.clone(), mod_display.clone(), "let"));
                }
            }
            crate::ast::Stmt::Const { name, vis, .. } => {
                if *vis == crate::ast::Vis::Public {
                    names.push((name.clone(), true));
                } else {
                    unexported.push((name.clone(), mod_display.clone(), "const"));
                }
            }
            _ => {}
        }
    }
    Ok((names, unexported))
}

/// Convenience for tests: a fresh interpreter with the default module dirs.
pub fn fresh_interp(file: &str) -> (Interp, Arc<Scheduler>) {
    let interp = Interp::new(Arc::from(file), default_module_dirs());
    let sched = interp.sched.clone();
    (interp, sched)
}
