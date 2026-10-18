//! AOT module flattening: resolve `import "name";` and inline the module's
//! top-level declarations into one self-contained `Program` before codegen.
//!
//! The bytecode compiler embeds imported modules into `.matrixc` (see
//! [`crate::vm::compiler`]); the AOT backend instead merges them at the AST
//! level, so [`crate::native::ir::build_module`] sees a single module with
//! every dependency present (transitively) and needs no runtime import
//! window. Visibility is already enforced by the semantic pass, which runs
//! before this flattening in the `build` flow, so a program that reaches here
//! only imports public names.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::ast::{Program, Stmt};
use crate::diagnostics::{Diagnostic, Phase};

/// Recursively inline the declarations of every imported module into a single
/// program. `Stmt::Import` statements are expanded; all other statements pass
/// through unchanged. Each module is inlined at most once (dedup by canonical
/// path); a cycle is reported like the bytecode path does.
pub fn flatten_imports(program: &Program, importer_file: &str) -> Result<Program, Diagnostic> {
    let mut out: Vec<Stmt> = Vec::new();
    let mut done: HashSet<PathBuf> = HashSet::new();
    let mut in_progress: HashSet<PathBuf> = HashSet::new();
    if let Ok(c) = std::fs::canonicalize(importer_file) {
        in_progress.insert(c);
    }
    flatten_stmts(&program.stmts, importer_file, &mut out, &mut done, &mut in_progress)?;
    Ok(Program { stmts: out })
}

fn flatten_stmts(
    stmts: &[Stmt],
    importer_file: &str,
    out: &mut Vec<Stmt>,
    done: &mut HashSet<PathBuf>,
    in_progress: &mut HashSet<PathBuf>,
) -> Result<(), Diagnostic> {
    for s in stmts {
        if let Stmt::Import { path, .. } = s {
            let resolved = resolve_module(importer_file, path)?;
            let canon = std::fs::canonicalize(&resolved).unwrap_or_else(|_| resolved.clone());
            if done.contains(&canon) {
                continue; // already inlined by another importer
            }
            if in_progress.contains(&canon) {
                return Err(Diagnostic::new(
                    Phase::Semantic,
                    format!("circular import of '{}'", path),
                ));
            }
            let src = std::fs::read_to_string(&resolved).map_err(|e| {
                Diagnostic::new(Phase::Semantic, format!("cannot import '{}': {}", path, e))
            })?;
            let file_id: Arc<str> = Arc::from(resolved.to_string_lossy().as_ref());
            let module: Program = crate::parser::parse_file(&src, file_id)?;
            let resolved_str = resolved.to_string_lossy().to_string();
            in_progress.insert(canon.clone());
            flatten_stmts(&module.stmts, &resolved_str, out, done, in_progress)?;
            in_progress.remove(&canon);
            done.insert(canon);
        } else {
            out.push(s.clone());
        }
    }
    Ok(())
}

/// Resolve an import path relative to the importing file's directory first,
/// then `SPID3R_PATH` — mirroring [`crate::vm::compiler`]'s resolver so both
/// the bytecode and AOT paths agree on module lookup.
fn resolve_module(importer_file: &str, path: &str) -> Result<PathBuf, Diagnostic> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let base = PathBuf::from(importer_file);
    if let Some(dir) = base.parent() {
        candidates.push(dir.join(path));
        candidates.push(dir.join(format!("{}.matrix", path)));
    }
    if let Some(paths) = std::env::var_os("SPID3R_PATH") {
        for d in std::env::split_paths(&paths) {
            candidates.push(d.join(path));
            candidates.push(d.join(format!("{}.matrix", path)));
        }
    }
    for c in candidates {
        if c.is_file() {
            return Ok(c);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_imports_passthrough() {
        let program = crate::parser::parse_file("fn main() { return 0; }", std::sync::Arc::from("m"))
            .expect("parse");
        let flat = flatten_imports(&program, "m").expect("flatten");
        assert_eq!(flat.stmts.len(), program.stmts.len());
    }

    #[test]
    fn missing_module_errors() {
        let program =
            crate::parser::parse_file("import \"nope\";\nfn main() { return 0; }", std::sync::Arc::from("m"))
                .expect("parse");
        let e = flatten_imports(&program, "m").unwrap_err();
        assert!(e.message.contains("not found"), "got: {}", e.message);
    }
}
