//! The module dependency graph, for `spidr modules`.
//!
//! This is a **read-only** traversal: it resolves and parses every reachable
//! `import`, but never executes a module body, never type-checks one, and never
//! writes anything. Its only job is to make the resolver's precedence
//! *inspectable* — which file an import actually named, and where that file
//! came from — instead of something a user has to infer from behaviour.
//!
//! Resolution goes through [`crate::modules::resolve::resolve_module`], the one
//! resolver every backend uses, so what this prints is what the engines will
//! load. If they ever disagreed, `spidr modules` would be lying.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast::{Program, Stmt};
use crate::diagnostics::{Diagnostic, Phase};
use crate::parser;

use super::resolve::{resolve_module, ModuleOrigin};

/// One module in the graph.
#[derive(Clone, Debug)]
pub struct ModuleNode {
    /// Canonical path, or the synthetic `<stdlib>/name.matrix` marker for an
    /// embedded module.
    pub path: PathBuf,
    pub origin: ModuleOrigin,
    /// Indices of the modules this one imports, in declaration order.
    pub imports: Vec<usize>,
}

impl ModuleNode {
    /// What to show a user: the marker for an embedded module, the path
    /// otherwise. The path is already canonical and prefix-clean, so it can be
    /// printed as-is.
    pub fn display(&self) -> String {
        self.path.to_string_lossy().to_string()
    }

    /// The file name, for the tree view (a full path per level is unreadable).
    pub fn short(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.display())
    }
}

/// The resolved module graph for one entry file.
#[derive(Clone, Debug)]
pub struct ModuleGraph {
    /// Index 0 is always the entry file.
    pub nodes: Vec<ModuleNode>,
}

impl ModuleGraph {
    /// The entry file's node.
    pub fn root(&self) -> &ModuleNode {
        &self.nodes[0]
    }

    /// How many modules came from each origin, for the summary line.
    pub fn count_by_origin(&self) -> Vec<(ModuleOrigin, usize)> {
        let mut counts: Vec<(ModuleOrigin, usize)> = Vec::new();
        for node in &self.nodes {
            match counts.iter_mut().find(|(o, _)| *o == node.origin) {
                Some((_, n)) => *n += 1,
                None => counts.push((node.origin, 1)),
            }
        }
        counts
    }
}

/// Build the graph rooted at `entry`.
///
/// `extra_dirs` are appended to the search path exactly as the embedding API's
/// `add_module_dir` does, so a host can inspect what its own scripts would
/// resolve.
///
/// A cycle is reported with the same message the engines produce
/// ([`super::circular_import_message`]), because the diagnostic is part of the
/// engine contract and `spidr modules` must not invent a second wording.
pub fn build(entry: &Path, extra_dirs: &[PathBuf]) -> Result<ModuleGraph, Diagnostic> {
    let entry_key = crate::modules::canonical_key(entry);
    let mut nodes: Vec<ModuleNode> = Vec::new();
    let mut index: HashMap<PathBuf, usize> = HashMap::new();
    let mut chain: Vec<PathBuf> = Vec::new();

    index.insert(entry_key.clone(), 0);
    nodes.push(ModuleNode {
        path: entry_key.clone(),
        origin: ModuleOrigin::Disk,
        imports: Vec::new(),
    });
    // The entry file is deliberately **not** on the chain: the engines never
    // claim it (it is the program, not an imported module), so their cycle
    // chain starts at the first imported module. Seeding it here produced
    // `main.matrix -> x.matrix -> y.matrix -> x` against the engines'
    // `x.matrix -> y.matrix -> x`, which is a contract mismatch.

    let imports = imports_of(&entry_key)?;
    let mut edges: Vec<(usize, Vec<usize>)> = Vec::new();
    for (spec, span) in imports {
        let child = walk(
            &entry_key, &spec, &span, extra_dirs, &mut nodes, &mut index, &mut chain,
        )?;
        edges.push((0, vec![child]));
    }
    attach(&mut nodes, edges);

    Ok(ModuleGraph { nodes })
}

/// Add `child` as an edge of `parent`, creating the edge list if needed.
fn attach(nodes: &mut [ModuleNode], edges: Vec<(usize, Vec<usize>)>) {
    for (parent, children) in edges {
        for child in children {
            if !nodes[parent].imports.contains(&child) {
                nodes[parent].imports.push(child);
            }
        }
    }
}

/// Resolve one import, then recurse into it. Returns the child's node index.
fn walk(
    importer: &Path,
    spec: &str,
    span: &crate::diagnostics::Span,
    extra_dirs: &[PathBuf],
    nodes: &mut Vec<ModuleNode>,
    index: &mut HashMap<PathBuf, usize>,
    chain: &mut Vec<PathBuf>,
) -> Result<usize, Diagnostic> {
    let importer_str = importer.to_string_lossy().to_string();
    let resolved = resolve_module(&importer_str, spec, extra_dirs).map_err(|mut e| {
        e.span = Some(span.clone());
        e
    })?;

    // A module already in the graph is reused: one node per file, which is the
    // same identity rule the module cache uses.
    if let Some(&existing) = index.get(&resolved.path) {
        // Re-entering a path that is still on the active chain is a cycle, not
        // a diamond. The engines report it, so `modules` must too.
        if chain.contains(&resolved.path) {
            return Err(Diagnostic::at(
                Phase::Semantic,
                span.clone(),
                super::circular_import_message(chain, spec),
            ));
        }
        return Ok(existing);
    }

    let idx = nodes.len();
    index.insert(resolved.path.clone(), idx);
    nodes.push(ModuleNode {
        path: resolved.path.clone(),
        origin: resolved.origin,
        imports: Vec::new(),
    });

    chain.push(resolved.path.clone());
    let children = imports_of(&resolved.path)?;
    let mut edges: Vec<(usize, Vec<usize>)> = Vec::new();
    for (child_spec, child_span) in children {
        let child = walk(
            &resolved.path,
            &child_spec,
            &child_span,
            extra_dirs,
            nodes,
            index,
            chain,
        )?;
        edges.push((idx, vec![child]));
    }
    chain.pop();
    attach(nodes, edges);
    Ok(idx)
}

/// Parse `path` and return its `import` statements as `(spec, span)`.
///
/// Parsing only — no semantic pass. `spidr modules` must work on a file whose
/// *body* has errors, because the module graph is exactly what a user needs
/// when a build fails.
fn imports_of(path: &Path) -> Result<Vec<(String, crate::diagnostics::Span)>, Diagnostic> {
    let src = crate::modules::read_module_source(path).map_err(|e| {
        Diagnostic::new(
            Phase::Semantic,
            format!("cannot read '{}': {}", path.display(), e),
        )
    })?;
    let file: std::sync::Arc<str> = std::sync::Arc::from(path.to_string_lossy().to_string());
    let program: Program = parser::parse_file(&src, file)?;
    Ok(program
        .stmts
        .iter()
        .filter_map(|s| match s {
            Stmt::Import { path, span, .. } => Some((path.clone(), span.clone())),
            _ => None,
        })
        .collect())
}

/// Render the flat listing: one line per module, `origin<TAB>path`.
///
/// Tab-separated so it is both readable and parseable; the origin column is a
/// fixed vocabulary (`disk`, `vendor`, `embedded`, `plugin`).
pub fn render_flat(graph: &ModuleGraph) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(graph.nodes.len());
    for node in &graph.nodes {
        lines.push(format!("{}\t{}", node.origin.as_str(), node.display()));
    }
    lines.join("\n")
}

/// Render the dependency tree, one module per line, children indented.
///
/// A module that appears more than once is printed once per edge but marked
/// `(*)` from its second appearance on, so a diamond is visible without
/// duplicating the whole subtree.
pub fn render_tree(graph: &ModuleGraph) -> String {
    let mut out = String::new();
    let mut seen: Vec<usize> = Vec::new();
    out.push_str(&format!(
        "{} ({})\n",
        graph.root().short(),
        graph.root().origin.as_str()
    ));
    seen.push(0);
    render_children(graph, 0, "", &mut seen, &mut out);
    out
}

fn render_children(
    graph: &ModuleGraph,
    idx: usize,
    prefix: &str,
    seen: &mut Vec<usize>,
    out: &mut String,
) {
    let kids = &graph.nodes[idx].imports;
    for (i, &child) in kids.iter().enumerate() {
        let last = i + 1 == kids.len();
        let branch = if last { "└─ " } else { "├─ " };
        let node = &graph.nodes[child];
        let repeat = if seen.contains(&child) { " (*)" } else { "" };
        out.push_str(&format!(
            "{}{}{} ({}){}\n",
            prefix,
            branch,
            node.short(),
            node.origin.as_str(),
            repeat
        ));
        if !seen.contains(&child) {
            seen.push(child);
            let next = format!("{}{}", prefix, if last { "   " } else { "│  " });
            render_children(graph, child, &next, seen, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spid3r_graph_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn a_diamond_is_one_node_with_two_edges() {
        let dir = scratch("diamond");
        std::fs::write(dir.join("shared.matrix"), "public fn s() { return 1; }\n").unwrap();
        std::fs::write(
            dir.join("a.matrix"),
            "import \"shared\";\npublic fn a() { return s(); }\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.matrix"),
            "import \"shared\";\npublic fn b() { return s(); }\n",
        )
        .unwrap();
        let main = dir.join("main.matrix");
        std::fs::write(
            &main,
            "import \"a\";\nimport \"b\";\nfn main() { return 0; }\n",
        )
        .unwrap();

        let g = build(&main, &[]).expect("graph");
        // main + a + b + shared, and shared appears exactly once.
        assert_eq!(g.nodes.len(), 4, "one node per module: {:?}", g.nodes);
        let shared = g
            .nodes
            .iter()
            .filter(|n| n.short() == "shared.matrix")
            .count();
        assert_eq!(shared, 1, "the diamond must not duplicate the shared node");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cycle_is_reported_with_the_engine_wording() {
        let dir = scratch("cycle");
        std::fs::write(
            dir.join("x.matrix"),
            "import \"y\";\npublic fn x() { return 1; }\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("y.matrix"),
            "import \"x\";\npublic fn y() { return 2; }\n",
        )
        .unwrap();
        let main = dir.join("main.matrix");
        std::fs::write(&main, "import \"x\";\nfn main() { return 0; }\n").unwrap();

        let e = build(&main, &[]).unwrap_err();
        assert_eq!(e.phase, Phase::Semantic);
        assert!(e.message.contains("circular import"), "got: {}", e.message);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
