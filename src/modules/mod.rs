//! Module loading: `import "name";` resolves relative to the importing file
//! first, then the documented search path. Modules are cached by canonical
//! path. Circular imports fail. No package manager in v1.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

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
    pub in_progress: Vec<PathBuf>,
}

impl ModuleCache {
    pub fn new() -> Self {
        ModuleCache {
            loaded: HashMap::new(),
            in_progress: Vec::new(),
        }
    }
}

fn resolve(interp: &Interp, path: &str) -> Result<PathBuf, Diagnostic> {
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
pub fn load(
    interp: &Interp,
    path: &str,
    span: &Span,
) -> Result<Vec<(String, Value, bool)>, Diagnostic> {
    let resolved = resolve(interp, path)?;
    if let Some(exports) = interp.modules.lock().unwrap().loaded.get(&resolved) {
        return Ok(exports.clone());
    }
    {
        let cache = interp.modules.lock().unwrap();
        if cache.in_progress.contains(&resolved) {
            // Show the full chain so the cycle is obvious:
            //   circular import of 'a' (chain: main -> a -> b -> a)
            let mut chain: Vec<String> = cache
                .in_progress
                .iter()
                .map(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| p.to_string_lossy().to_string())
                })
                .collect();
            chain.push(path.to_string());
            return Err(Diagnostic::at(
                Phase::Semantic,
                span.clone(),
                format!("circular import of '{}' (chain: {})", path, chain.join(" -> ")),
            ));
        }
    }
    let src = std::fs::read_to_string(&resolved).map_err(|e| {
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

    interp
        .modules
        .lock()
        .unwrap()
        .in_progress
        .push(resolved.clone());
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
    );
    let mut result: Result<Vec<(String, Value)>, Diagnostic> = Ok(Vec::new());
    for stmt in &program.stmts {
        if let Flow::Error(d) = minterp.exec_stmt(stmt, &minterp.globals) {
            result = Err(d);
            break;
        }
    }
    interp.modules.lock().unwrap().in_progress.pop();
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
    interp
        .modules
        .lock()
        .unwrap()
        .loaded
        .insert(resolved, exports.clone());
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
    let base = PathBuf::from(importer_file);
    let mut dirs = default_module_dirs();
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
        .ok_or_else(|| {
            Diagnostic::at(
                Phase::Semantic,
                span.clone(),
                format!("module '{}' not found", path),
            )
        })?;
    let src = std::fs::read_to_string(&resolved).map_err(|e| {
        Diagnostic::at(
            Phase::Semantic,
            span.clone(),
            format!("cannot import '{}': {}", path, e),
        )
    })?;
    let file: Arc<str> = Arc::from(resolved.to_string_lossy().to_string());
    let program = parser::parse_file(&src, file)?;
    let mut names = Vec::new();
    for stmt in &program.stmts {
        match stmt {
            crate::ast::Stmt::Fn(f) if f.vis == crate::ast::Vis::Public => {
                if let Some(n) = &f.name {
                    names.push((n.clone(), false));
                }
            }
            crate::ast::Stmt::Class(c) if c.vis == crate::ast::Vis::Public => {
                names.push((c.name.clone(), false));
            }
            crate::ast::Stmt::Interface(i) if i.vis == crate::ast::Vis::Public => {
                names.push((i.name.clone(), false));
            }
            crate::ast::Stmt::Let { name, vis, .. }
                if *vis == crate::ast::Vis::Public =>
            {
                names.push((name.clone(), false));
            }
            crate::ast::Stmt::Const { name, vis, .. }
                if *vis == crate::ast::Vis::Public =>
            {
                names.push((name.clone(), true));
            }
            _ => {}
        }
    }
    Ok(names)
}

/// Convenience for tests: a fresh interpreter with the default module dirs.
pub fn fresh_interp(file: &str) -> (Interp, Arc<Scheduler>) {
    let interp = Interp::new(Arc::from(file), default_module_dirs());
    let sched = interp.sched.clone();
    (interp, sched)
}
