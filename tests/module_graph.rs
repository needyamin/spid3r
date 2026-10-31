//! `spidr modules` — the module graph, and the cycle diagnostic it shares with
//! the engines.
//!
//! Two things are pinned here:
//!
//! 1. The graph reports the **origin** the resolver chose, so the search
//!    precedence stops being guesswork. A module imported by two others is one
//!    node with two edges, not two nodes.
//! 2. A circular import produces the **same diagnostic** from `spidr modules`
//!    and from `spidr run`, because `docs/ENGINE_CONTRACT.md` makes the phase
//!    and message part of the contract. A second wording would be a contract
//!    violation, not a cosmetic difference.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Path to the `spidr` binary under test (see `tests/cli_dispatch.rs` for why
/// this is resolved at run time rather than through `env!`).
fn bin() -> PathBuf {
    for key in ["CARGO_BIN_EXE_spid3r", "CARGO_BIN_EXE_spidr"] {
        if let Ok(p) = std::env::var(key) {
            let p = PathBuf::from(p);
            if p.is_file() {
                return p;
            }
        }
    }
    let exe = std::env::current_exe().expect("test executable path");
    let profile_dir = exe
        .parent()
        .and_then(|deps| deps.parent())
        .expect("profile directory");
    for stem in ["spid3r", "spidr"] {
        let cand = profile_dir.join(format!("{}{}", stem, std::env::consts::EXE_SUFFIX));
        if cand.is_file() {
            return cand;
        }
    }
    panic!("spid3r binary not found next to {}", profile_dir.display());
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r_graph_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).expect("mkdir parent");
    }
    std::fs::write(&p, body).expect("write");
}

fn run(args: &[&str], dir: &Path) -> (i32, String, String) {
    let o = Command::new(bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn spidr");
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

/// The flat listing reports one line per module as `origin<TAB>path`, and the
/// origin distinguishes a sibling file from a vendored one.
#[test]
fn the_listing_reports_each_modules_origin() {
    let dir = scratch("flat");
    write(
        &dir,
        "helper.matrix",
        "public fn twice(n) {\n    return n * 2;\n}\n",
    );
    write(
        &dir,
        "vendor/packed.matrix",
        "public fn tag() {\n    return \"v\";\n}\n",
    );
    write(
        &dir,
        "main.matrix",
        "import \"helper\";\nimport \"packed\";\nfn main() {\n    return 0;\n}\n",
    );

    let (code, out, err) = run(&["modules", "main.matrix"], &dir);
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 4, "3 modules + a summary line: {out}");

    assert!(
        lines[0].starts_with("disk\t") && lines[0].ends_with("main.matrix"),
        "entry line: {}",
        lines[0]
    );
    assert!(
        lines[1].starts_with("disk\t") && lines[1].ends_with("helper.matrix"),
        "sibling line: {}",
        lines[1]
    );
    assert!(
        lines[2].starts_with("vendor\t") && lines[2].ends_with("packed.matrix"),
        "vendored line must be tagged `vendor`: {}",
        lines[2]
    );
    assert!(
        lines[3].contains("3 module(s)") && lines[3].contains("1 vendor"),
        "summary: {}",
        lines[3]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A module imported by two others is **one** node. The tree prints it twice
/// (once per edge, marked `(*)`) but does not expand its subtree twice.
#[test]
fn a_diamond_prints_the_shared_module_once_per_edge_and_marks_the_repeat() {
    let dir = scratch("diamond");
    write(&dir, "shared.matrix", "public fn s() {\n    return 1;\n}\n");
    write(
        &dir,
        "a.matrix",
        "import \"shared\";\npublic fn a() {\n    return s();\n}\n",
    );
    write(
        &dir,
        "b.matrix",
        "import \"shared\";\npublic fn b() {\n    return s();\n}\n",
    );
    write(
        &dir,
        "main.matrix",
        "import \"a\";\nimport \"b\";\nfn main() {\n    return 0;\n}\n",
    );

    let (code, out, err) = run(&["modules", "main.matrix", "--tree"], &dir);
    assert_eq!(code, 0, "stderr: {err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "main.matrix (disk)", "root: {:?}", lines[0]);
    // shared appears under a and under b; the second is marked.
    let shared_lines: Vec<&&str> = lines
        .iter()
        .filter(|l| l.contains("shared.matrix"))
        .collect();
    assert_eq!(shared_lines.len(), 2, "tree:\n{out}");
    assert!(
        shared_lines[0].contains("shared.matrix (disk)") && !shared_lines[0].contains("(*)"),
        "first occurrence must not be marked: {}",
        shared_lines[0]
    );
    assert!(
        shared_lines[1].contains("(*)"),
        "the repeat must be marked: {}",
        shared_lines[1]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `spidr modules` and `spidr run` must report a cycle with the same phase and
/// message. The graph builder has its own traversal, so this is a real parity
/// risk rather than a tautology.
#[test]
fn a_cycle_reports_the_same_diagnostic_as_the_engines() {
    let dir = scratch("cycle");
    write(
        &dir,
        "x.matrix",
        "import \"y\";\npublic fn x() {\n    return 1;\n}\n",
    );
    write(
        &dir,
        "y.matrix",
        "import \"x\";\npublic fn y() {\n    return 2;\n}\n",
    );
    write(
        &dir,
        "main.matrix",
        "import \"x\";\nfn main() {\n    return 0;\n}\n",
    );

    let (mcode, _, merr) = run(&["modules", "main.matrix"], &dir);
    assert_eq!(mcode, 1, "modules must fail on a cycle: {merr}");
    let mfirst = merr.lines().next().unwrap_or("").trim().to_string();
    assert!(
        mfirst.contains("circular import"),
        "modules diagnostic: {mfirst}"
    );

    for engine in ["tree", "vm"] {
        let (code, _, err) = run(&["run", "--engine", engine, "main.matrix"], &dir);
        assert_eq!(code, 1, "[{engine}] must fail: {err}");
        let first = err.lines().next().unwrap_or("").trim().to_string();
        assert_eq!(
            first, mfirst,
            "[{engine}] cycle diagnostic differs from `spidr modules`"
        );
    }

    // The AOT flattener has its own cycle detection; it must agree too.
    #[cfg(feature = "native")]
    {
        let exe = dir.join(format!("built{}", std::env::consts::EXE_SUFFIX));
        let (code, _, err) = run(&["build", "main.matrix", "-o", exe.to_str().unwrap()], &dir);
        assert_eq!(code, 1, "[aot] build must fail: {err}");
        let first = err.lines().next().unwrap_or("").trim().to_string();
        assert_eq!(first, mfirst, "[aot] cycle diagnostic differs");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The plan's P2 exit criterion: `spidr modules --tree` is stable across the
/// 25-example suite. The graph is engine-independent by construction (one
/// resolver), so the meaningful assertion is that every shipped example
/// resolves and that the output is deterministic.
#[test]
fn every_shipped_example_produces_a_stable_module_tree() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let examples = root.join("examples");
    let mut checked = 0usize;
    let mut files: Vec<PathBuf> = std::fs::read_dir(&examples)
        .expect("read examples/")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("matrix"))
        .collect();
    files.sort();

    for file in files {
        let rel = file.strip_prefix(&root).expect("under repo");
        let (code, out1, err) = run(&["modules", rel.to_str().unwrap(), "--tree"], &root);
        assert_eq!(
            code,
            0,
            "{}: modules failed: {}",
            rel.display(),
            err.lines().next().unwrap_or("")
        );
        let (_, out2, _) = run(&["modules", rel.to_str().unwrap(), "--tree"], &root);
        assert_eq!(out1, out2, "{}: --tree is not deterministic", rel.display());
        assert!(
            out1.lines().next().is_some_and(|l| l.contains("(disk)")),
            "{}: tree has no root line: {out1}",
            rel.display()
        );
        checked += 1;
    }
    assert!(
        checked >= 20,
        "expected the full example suite, saw {checked}"
    );
}

/// A file that cannot resolve an import must fail with exit 1 and name the
/// module — the same contract `spidr run` gives.
#[test]
fn an_unresolvable_import_is_reported_not_silently_skipped() {
    let dir = scratch("missing");
    write(
        &dir,
        "main.matrix",
        "import \"nope_not_here\";\nfn main() {\n    return 0;\n}\n",
    );
    let (code, _, err) = run(&["modules", "main.matrix"], &dir);
    assert_eq!(code, 1, "stderr: {err}");
    assert!(
        err.contains("nope_not_here") && err.contains("not found"),
        "diagnostic: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
