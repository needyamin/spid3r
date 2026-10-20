//! Bytecode optimizer tests: the optimizer must never change observable
//! behaviour. Verified by running every shipped example with and without
//! `--no-opt` (both on the VM and through a compiled `.matrixc`), and by
//! checking that folding actually removes instructions.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn out(args: &[&str]) -> Result<(i32, String), String> {
    let o = Command::new(bin())
        .args(args)
        .output()
        .map_err(|e| format!("spawn failed: {}", e))?;
    Ok((
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
    ))
}

fn examples() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut v: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("examples/")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "matrix").unwrap_or(false))
        .collect();
    v.sort();
    v
}

#[test]
fn optimizer_preserves_behaviour_on_every_example() {
    let files = examples();
    assert!(files.len() >= 20, "expected the shipped examples");
    for p in files {
        let path = p.to_str().unwrap();
        let src = std::fs::read_to_string(&p).expect("read");
        if !src.contains("fn main") {
            continue; // library module: nothing to run
        }
        let (c1, opt) = out(&["run", "--engine", "vm", path]).expect("optimized run");
        let (c2, plain) = out(&["run", "--engine", "vm", "--no-opt", path]).expect("plain run");
        assert_eq!(c1, c2, "exit code differs for {} (opt vs --no-opt)", path);
        assert_eq!(opt, plain, "stdout differs for {} (opt vs --no-opt)", path);
    }
}

#[test]
fn compiled_bytecode_behaves_identically_with_and_without_opt() {
    let dir = std::env::temp_dir().join(format!("spid3r_opt_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let prog = dir.join("prog.matrix");
    std::fs::write(
        &prog,
        "fn calc(n) {\n    let a = 2 + 3 * 4;\n    let b = \"x\" + \"y\";\n    let c = -7;\n    let d = 10 / 4;\n    let e = 1.5 * 2.0;\n    let f = 3 > 2;\n    println(a);\n    println(b);\n    println(c);\n    println(d);\n    println(e);\n    println(f);\n    println(a + n);\n    return a;\n}\nfn main() {\n    println(calc(1));\n    return 0;\n}\n",
    )
    .expect("write");

    let mut outputs = Vec::new();
    for (tag, extra) in [("opt", None), ("plain", Some("--no-opt"))] {
        let mc = dir.join(format!("{}.matrixc", tag));
        let mut args = vec![
            "compile",
            prog.to_str().unwrap(),
            "-o",
            mc.to_str().unwrap(),
        ];
        if let Some(e) = extra {
            args.push(e);
        }
        let (code, _) = out(&args).expect("compile");
        assert_eq!(code, 0, "compile failed for {}", tag);
        let (rc, stdout) = out(&["exec", mc.to_str().unwrap()]).expect("exec");
        assert_eq!(rc, 0, "exec failed for {}", tag);
        outputs.push(stdout);
    }
    assert_eq!(
        outputs[0], outputs[1],
        "optimized bytecode changed behaviour"
    );
    assert!(
        outputs[0].contains("14"),
        "expected folded arithmetic: {}",
        outputs[0]
    );
    assert!(
        outputs[0].contains("xy"),
        "expected folded concat: {}",
        outputs[0]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn folding_shrinks_compiled_bytecode() {
    let dir = std::env::temp_dir().join(format!("spid3r_optsize_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let prog = dir.join("prog.matrix");
    std::fs::write(
        &prog,
        "fn main() {\n    let a = 1 + 2 + 3 + 4 + 5 + 6;\n    let b = \"a\" + \"b\" + \"c\";\n    println(a);\n    println(b);\n    return 0;\n}\n",
    )
    .expect("write");
    let mut sizes = Vec::new();
    for (tag, extra) in [("opt", None), ("plain", Some("--no-opt"))] {
        let mc = dir.join(format!("{}.matrixc", tag));
        let mut args = vec![
            "compile",
            prog.to_str().unwrap(),
            "-o",
            mc.to_str().unwrap(),
        ];
        if let Some(e) = extra {
            args.push(e);
        }
        out(&args).expect("compile");
        sizes.push(std::fs::metadata(&mc).expect("stat").len());
    }
    assert!(
        sizes[0] < sizes[1],
        "optimized bytecode should be smaller: {} vs {}",
        sizes[0],
        sizes[1]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn runtime_errors_still_fire_with_the_same_message_under_opt() {
    let dir = std::env::temp_dir().join(format!("spid3r_opterr_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let prog = dir.join("div.matrix");
    std::fs::write(
        &prog,
        "fn main() {\n    let x = 1 / 0;\n    println(x);\n    return 0;\n}\n",
    )
    .expect("write");
    let err = |args: &[&str]| {
        let o = Command::new(bin()).args(args).output().expect("run");
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stderr).to_string(),
        )
    };
    let (c1, e1) = err(&["run", "--engine", "vm", prog.to_str().unwrap()]);
    let (c2, e2) = err(&["run", "--engine", "vm", "--no-opt", prog.to_str().unwrap()]);
    assert_ne!(c1, 0, "division by zero must fail");
    assert_eq!(c1, c2);
    assert_eq!(e1, e2, "error text must be identical with and without opt");
    assert!(
        e1.contains("zero"),
        "expected a division-by-zero error: {}",
        e1
    );
    let _ = std::fs::remove_dir_all(&dir);
}
