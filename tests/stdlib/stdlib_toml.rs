//! `stdlib/toml.matrix` — the pure-Spid3r TOML-subset parser.
//!
//! The module is plain Spid3r, so it is tested on BOTH engines.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run(engine: &str, name: &str, src: &str) -> (i32, String, String) {
    let dir = std::env::temp_dir().join("spid3r_toml");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{}.matrix", name));
    std::fs::write(&path, src).expect("write case");
    let o = Command::new(bin())
        .args(["run", "--engine", engine, path.to_str().unwrap()])
        .output()
        .expect("spawn spid3r");
    let _ = std::fs::remove_file(&path);
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

const FULL: &str = "\
import \"toml\" as t;

fn main() {
    let cfg = t.toml_parse(\"# a config\\ntitle = \\\"my app\\\"\\nversion = 2\\nratio = 0.5\\nenabled = true\\n\\n[server]\\nport = 8080\\nhosts = [\\\"a.io\\\", \\\"b.io\\\"]\\n\\n[server.limits]\\nmax = 100\\n\\n[[workers]]\\nname = \\\"w1\\\"\\n\\n[[workers]]\\nname = \\\"w2\\\"\\nmemory = 512\\n\");
    println(cfg[\"title\"]);
    println(cfg[\"version\"]);
    println(cfg[\"ratio\"]);
    println(cfg[\"enabled\"]);
    println(cfg[\"server\"][\"port\"]);
    println(cfg[\"server\"][\"hosts\"][1]);
    println(t.toml_get(cfg, \"server.limits.max\"));
    println(len(cfg[\"workers\"]));
    println(cfg[\"workers\"][1][\"name\"]);
    println(cfg[\"workers\"][1][\"memory\"]);
    let dotted = t.toml_parse(\"a.b.c = 7\\n\");
    println(t.toml_get(dotted, \"a.b.c\"));
    let inline = t.toml_parse(\"pt = { x = 1, y = 2 }\\n\");
    println(inline[\"pt\"][\"y\"]);
    let multi = t.toml_parse(\"arr = [\\n  1,  # one\\n  2,\\n  3,\\n]\\n\");
    println(multi[\"arr\"][2]);
    let literal = t.toml_parse(\"path = 'C:/Users'\\n\");
    println(literal[\"path\"]);
    let nested = t.toml_parse(\"m = [[1, 2], [3]]\\n\");
    println(nested[\"m\"][0][1]);
    let neg = t.toml_parse(\"n = -17\\n\");
    println(neg[\"n\"]);
    let us = t.toml_parse(\"big = 1_000_000\\n\");
    println(us[\"big\"]);
    return 0;
}
";

const EXPECTED: &str =
    "my app\n2\n0.5\ntrue\n8080\nb.io\n100\n2\nw2\n512\n7\n2\n3\nC:/Users\n2\n-17\n1000000\n";

#[test]
fn toml_subset_parses_on_both_engines() {
    for engine in ["vm", "tree"] {
        let (code, stdout, stderr) = run(engine, "toml_full", FULL);
        assert_eq!(code, 0, "[{}] stderr: {}", engine, stderr);
        assert_eq!(stdout, EXPECTED, "[{}]", engine);
    }
}

#[test]
fn malformed_toml_throws_with_a_line_number() {
    let src = "\
import \"toml\" as t;

fn main() {
    let cfg = t.toml_parse(\"ok = 1\\nbroken line here\\n\");
    println(cfg[\"ok\"]);
    return 0;
}
";
    for engine in ["vm", "tree"] {
        let (code, _, stderr) = run(engine, "toml_bad", src);
        assert_eq!(code, 1, "[{}]", engine);
        assert!(
            stderr.contains("toml: line 2:"),
            "[{}] stderr: {}",
            engine,
            stderr
        );
    }
}

#[test]
fn unterminated_string_throws() {
    let src = "\
import \"toml\" as t;

fn main() {
    let cfg = t.toml_parse(\"k = \\\"oops\");
    return 0;
}
";
    for engine in ["vm", "tree"] {
        let (code, _, stderr) = run(engine, "toml_unterminated", src);
        assert_eq!(code, 1, "[{}]", engine);
        assert!(
            stderr.contains("toml: line 1: unterminated string"),
            "[{}] stderr: {}",
            engine,
            stderr
        );
    }
}
