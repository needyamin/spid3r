//! Differential test driver (spec requirement): every example is executed three
//! ways — the tree-walk interpreter (`run --engine tree`), the bytecode VM
//! (`run --engine vm`), and the compiled+executed form (`compile` then `exec`) —
//! and their stdout must be byte-for-byte identical. Library modules (no `fn
//! main`) are only required to pass the semantic `check`.
//!
//! This is the `cargo test` entry point for differential testing; the same logic
//! runs from the CLI via `spid3r test`.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

/// Run the spid3r binary and return stdout, or an error string (stderr) on
/// non-zero exit / IO failure.
fn out(args: &[&str]) -> Result<String, String> {
    let o = Command::new(bin())
        .args(args)
        .output()
        .map_err(|e| format!("spawn failed: {}", e))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).to_string())
    }
}

#[test]
fn all_examples_match_across_engines() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let root = PathBuf::from(manifest).join("examples");
    assert!(root.is_dir(), "examples/ must exist at the crate root");

    let mut entries: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("read examples/")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "matrix").unwrap_or(false))
        .collect();
    entries.sort();

    let mut programs: Vec<PathBuf> = Vec::new();
    let mut libraries: Vec<PathBuf> = Vec::new();
    for p in &entries {
        let src = std::fs::read_to_string(p).expect("read example");
        // Classification by the literal `fn main` is adequate here: this test only
        // needs to decide run-vs-check, and the real CLI (`spid3r test`) uses the
        // parsed AST. All shipped examples are well-formed.
        if src.contains("fn main") {
            programs.push(p.clone());
        } else {
            libraries.push(p.clone());
        }
    }

    for lib in &libraries {
        let path = lib.to_str().unwrap();
        out(&["check", path]).unwrap_or_else(|e| panic!("library {} must pass check:\n{}", path, e));
    }

    for prog in &programs {
        let path = prog.to_str().unwrap();
        let tree = out(&["run", "--engine", "tree", path])
            .unwrap_or_else(|e| panic!("tree run of {} failed:\n{}", path, e));
        let vm = out(&["run", "--engine", "vm", path])
            .unwrap_or_else(|e| panic!("vm run of {} failed:\n{}", path, e));

        let tc = std::env::temp_dir().join(format!(
            "spid3r_dt_{}.matrixc",
            prog.file_name().unwrap().to_string_lossy().replace('.', "_")
        ));
        let tc = tc.to_str().unwrap();
        out(&["compile", path, "-o", tc])
            .unwrap_or_else(|e| panic!("compile of {} failed:\n{}", path, e));
        let exec = out(&["exec", tc])
            .unwrap_or_else(|e| panic!("exec of {} failed:\n{}", path, e));

        assert_eq!(tree, vm, "tree vs vm stdout differ for {}", path);
        assert_eq!(vm, exec, "vm vs exec stdout differ for {}", path);
    }
}

#[test]
fn global_assignment_matches_across_engines() {
    // Regression for the VM `SetGlobal` double-pop: the opcode used to pop its
    // value while the statement-level `OpPop` consumed it too, so *every*
    // assignment to a global ("g = x" at top level, inside main, inside a
    // nested function, or a compound "g += x") died with "stack underflow".
    // Local assignment worked, which is why no shipped example caught it.
    let cases: &[(&str, &str)] = &[
        (
            "top_level",
            "let g = 0;\ng = 5;\nprintln(g);\nfn main() { return 0; }\n",
        ),
        (
            "in_main",
            "let g = 0;\nfn main() {\n    g = 7;\n    println(g);\n    return 0;\n}\n",
        ),
        (
            "nested_function",
            "let g = 0;\nfn bump() { g = g + 1; }\nfn main() {\n    bump();\n    bump();\n    println(g);\n    return 0;\n}\n",
        ),
        (
            "compound",
            "let g = 0;\nfn bump() { g += 5; }\nfn main() {\n    bump();\n    bump();\n    println(g);\n    return 0;\n}\n",
        ),
        (
            "assignment_is_an_expression",
            "let g = 1;\nfn main() {\n    let y = g = 99;\n    println(y);\n    println(g);\n    return 0;\n}\n",
        ),
    ];
    for (name, src) in cases {
        let path = std::env::temp_dir().join(format!("spid3r_global_assign_{}.matrix", name));
        std::fs::write(&path, src).expect("write case");
        let p = path.to_str().unwrap();
        let tree = out(&["run", "--engine", "tree", p])
            .unwrap_or_else(|e| panic!("{}: tree failed:\n{}", name, e));
        let vm = out(&["run", "--engine", "vm", p])
            .unwrap_or_else(|e| panic!("{}: vm failed:\n{}", name, e));
        assert_eq!(tree, vm, "{}: tree vs vm differ", name);
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn field_index_assignment_matches_across_engines() {
    // Regression for the VM `FieldSet`/`IndexSet` stack bug: both opcodes
    // popped their operands and pushed nothing, while `SetLocal`/`SetGlobal`
    // leave the assigned value for the enclosing statement's `OpPop`. The
    // statement `Pop` therefore ate the value *below* the assignment - for a
    // method that is the `self` local itself, so any field assignment in a
    // method followed by another stack-using statement died with
    // "null has no fields". Compound targets (`+=`) additionally consumed the
    // object via FieldGet/IndexGet; both now duplicate it (OpDup/OpDup2).
    let cases: &[(&str, &str)] = &[
        (
            "field_assign_then_read",
            "class C {\n    public let n;\n    fn new() { self.n = 0; }\n    public fn bump(self) {\n        self.n = self.n + 1;\n        return self.n;\n    }\n}\nfn main() {\n    let c = new C();\n    println(c.bump());\n    println(c.bump());\n    return 0;\n}\n",
        ),
        (
            "field_compound",
            "class C {\n    public let n;\n    fn new() { self.n = 10; }\n    public fn bump(self) {\n        self.n += 5;\n        self.n -= 2;\n        return self.n;\n    }\n}\nfn main() {\n    let c = new C();\n    println(c.bump());\n    return 0;\n}\n",
        ),
        (
            "index_compound",
            "fn main() {\n    let xs = list(10, 20, 30);\n    xs[1] += 5;\n    xs[0] *= 2;\n    println(xs[0]);\n    println(xs[1]);\n    let m = map();\n    m.set(\"k\", 1);\n    println(m.get(\"k\"));\n    return 0;\n}\n",
        ),
        (
            "assign_in_call_args",
            "class C {\n    public let n;\n    fn new() { self.n = 1; }\n    public fn bump(self) -> int {\n        self.n = self.n * 3;\n        return self.n;\n    }\n}\nfn id(x) { return x; }\nfn main() {\n    let c = new C();\n    println(id(c.bump()) + c.bump());\n    return 0;\n}\n",
        ),
    ];
    for (name, src) in cases {
        let path = std::env::temp_dir().join(format!("spid3r_field_assign_{}.matrix", name));
        std::fs::write(&path, src).expect("write case");
        let p = path.to_str().unwrap();
        let tree = out(&["run", "--engine", "tree", p])
            .unwrap_or_else(|e| panic!("{}: tree failed:/n{}", name, e));
        let vm = out(&["run", "--engine", "vm", p])
            .unwrap_or_else(|e| panic!("{}: vm failed:/n{}", name, e));
        assert_eq!(tree, vm, "{}: tree vs vm differ", name);
        let _ = std::fs::remove_file(&path);
    }
}
