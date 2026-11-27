//! Regression tests for `.mt` extension, `sp` CLI alias, and BUG-11..BUG-31.

use std::path::PathBuf;
use std::process::Command;

fn spid3r_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn sp_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_sp"))
}

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spid3r_batch_{}_{}_{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_both_engines(tag: &str, src: &str, ext: &str) -> String {
    let dir = scratch_dir(tag);
    let path = dir.join(format!("prog.{}", ext));
    std::fs::write(&path, src).unwrap();
    let tree = Command::new(spid3r_bin())
        .args(["run", "--engine", "tree"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        tree.status.success(),
        "tree failed ({}): stdout={} stderr={}",
        tag,
        String::from_utf8_lossy(&tree.stdout),
        String::from_utf8_lossy(&tree.stderr)
    );
    let vm = Command::new(spid3r_bin())
        .args(["run", "--engine", "vm"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        vm.status.success(),
        "vm failed ({}): stdout={} stderr={}",
        tag,
        String::from_utf8_lossy(&vm.stdout),
        String::from_utf8_lossy(&vm.stderr)
    );
    let t_out = String::from_utf8_lossy(&tree.stdout).to_string();
    let v_out = String::from_utf8_lossy(&vm.stdout).to_string();
    assert_eq!(t_out, v_out, "tree vs vm mismatch ({})", tag);
    let _ = std::fs::remove_dir_all(&dir);
    t_out
}

#[test]
fn mt_extension_and_sp_cli_alias_work_end_to_end() {
    let dir = scratch_dir("mt_sp");
    let lib = dir.join("helper.mt");
    let main_mt = dir.join("app.mt");
    let bc = dir.join("app.matrixc");
    std::fs::write(
        &lib,
        "public fn double(n: int) -> int {\n    return n * 2;\n}\n",
    )
    .unwrap();
    std::fs::write(
        &main_mt,
        "import { double } from \"helper\";\nimport { parse } from \"semver.mt\";\nfn main() -> int {\n    let v = parse(\"1.2.3\");\n    println(string(double(21)) + \":\" + string(v[\"major\"]));\n    return 0;\n}\n",
    )
    .unwrap();

    // `sp check` on directory containing `.mt` files
    let chk = Command::new(sp_bin())
        .args(["check"])
        .arg(&dir)
        .output()
        .unwrap();
    assert!(
        chk.status.success(),
        "sp check dir failed: {}",
        String::from_utf8_lossy(&chk.stderr)
    );

    // `sp run` on `.mt` file (tree & vm)
    for eng in ["tree", "vm"] {
        let out = Command::new(sp_bin())
            .args(["run", "--engine", eng])
            .arg(&main_mt)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "sp run --engine {eng} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "42:1");
    }

    // `sp compile` strips `.mt` -> `.matrixc`, then `sp exec` runs it
    let comp = Command::new(sp_bin())
        .args(["compile"])
        .arg(&main_mt)
        .output()
        .unwrap();
    assert!(
        comp.status.success(),
        "sp compile failed: {}",
        String::from_utf8_lossy(&comp.stderr)
    );
    assert!(bc.is_file(), "expected {} to be produced", bc.display());
    let exec = Command::new(sp_bin())
        .args(["exec"])
        .arg(&bc)
        .output()
        .unwrap();
    assert!(exec.status.success());
    assert_eq!(String::from_utf8_lossy(&exec.stdout).trim(), "42:1");

    // `sp fmt` recognizes `.mt` files inside directories
    let fmt = Command::new(sp_bin())
        .args(["fmt", "--check"])
        .arg(&lib)
        .output()
        .unwrap();
    assert!(
        fmt.status.success(),
        "sp fmt --check .mt failed: {}",
        String::from_utf8_lossy(&fmt.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bug_11_qualified_enum_variants_match_and_imports() {
    let dir = scratch_dir("bug11");
    std::fs::write(
        dir.join("colors.mt"),
        r#"public enum Palette {
    Red,
    Green,
    Rgb(r, g, b),
}
"#,
    )
    .unwrap();
    let main_p = dir.join("main.matrix");
    std::fs::write(
        &main_p,
        r#"import { Palette } from "colors";

enum Shape {
    Empty,
    Circle(radius),
}

fn describe(p) -> string {
    return match (p) {
        Palette.Red => "red",
        Palette.Green => "green",
        Palette.Rgb(r, g, b) => string(r) + "," + string(g) + "," + string(b),
    };
}

fn main() -> int {
    let c = Palette.Red;
    let rgb = Palette.Rgb(10, 20, 30);
    let s1 = Shape.Empty;
    let s2 = Shape.Circle(5);
    println(describe(c));
    println(describe(rgb));
    println(string(s1 == Empty()));
    let area = match (s2) {
        Shape.Circle(r) => r * r,
        Shape.Empty => 0,
    };
    println(string(area));
    return 0;
}
"#,
    )
    .unwrap();
    for eng in ["tree", "vm"] {
        let out = Command::new(spid3r_bin())
            .args(["run", "--engine", eng])
            .arg(&main_p)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "bug11 ({eng}) failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "red\n10,20,30\ntrue\n25\n"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bug_13_csv_preserves_identifiers_and_supports_infer_types_false() {
    let dir = scratch_dir("bug13");
    let csv_path = dir.join("ids.csv");
    let csv_escaped = csv_path.to_string_lossy().replace('\\', "/");
    let src = format!(
        r#"fn main() -> int {{
    write_csv("{csv}", ["zip", "id", "val"], [["007", "1e5", "1.50"], ["0012", "0x10", "-0"]]);
    let back = read_csv("{csv}").unwrap()["rows"];
    println(string(back));
    println(type_of(back[0][0]) + "," + type_of(back[0][1]) + "," + type_of(back[0][2]));
    println(type_of(back[1][0]) + "," + type_of(back[1][1]) + "," + type_of(back[1][2]));
    let quoted = parse_csv("a,b\n\"42\",\"true\"\n", null).unwrap()["rows"];
    println(type_of(quoted[0][0]) + "," + type_of(quoted[0][1]));
    let raw = parse_csv("a,b\n42,true\n", {{"infer_types": false}}).unwrap()["rows"];
    println(type_of(raw[0][0]) + "," + type_of(raw[0][1]));
    return 0;
}}
"#,
        csv = csv_escaped
    );
    let out = run_both_engines("bug13_run", &src, "matrix");
    assert_eq!(
        out,
        "[[007, 1e5, 1.50], [0012, 0x10, -0]]\nstring,string,string\nstring,string,string\nstring,string\nstring,string\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bug_14_binary_tree_level_order_constructor() {
    let out = run_both_engines(
        "bug14",
        r#"fn main() -> int {
    let t = binary_tree(1, 2, 3);
    println(string(t.len()) + ":" + string(t.level_order()) + ":" + string(t.inorder()));
    let empty = binary_tree();
    println(string(empty.len()));
    return 0;
}
"#,
        "matrix",
    );
    assert_eq!(out, "3:[1, 2, 3]:[2, 1, 3]\n0\n");
}

#[test]
fn bug_15_gen_collect_take_composition() {
    let out = run_both_engines(
        "bug15",
        r#"import { range_gen, collect, take } from "gen";
fn main() -> int {
    println(string(collect(take(range_gen(0, 10), 3))));
    let err_msg = "";
    try {
        collect(42);
    } catch (e) {
        err_msg = string(e);
    }
    println(string(err_msg.contains("gen.collect()")));
    return 0;
}
"#,
        "matrix",
    );
    assert_eq!(out, "[0, 1, 2]\ntrue\n");
}

#[test]
fn bug_16_stage0_verify_runtime_cli() {
    let dir = scratch_dir("bug16");
    let lib_path = dir.join("spid3r_lang.lib");
    let payload = b"synthetic-runtime-staticlib-bytes";
    std::fs::write(&lib_path, payload).unwrap();
    let hex = spid3r_lang::pkg::sha256_hex(payload);
    let manifest = format!(
        r#"{{"version":"1.5.0","staticlib_name":"spid3r_lang.lib","sha256":"{}","provenance":{{"git_commit":"test","llvm_version":"23.1.0"}}}}"#,
        hex
    );
    std::fs::write(dir.join("runtime-manifest.json"), manifest).unwrap();

    // Valid directory exits 0
    let ok = Command::new(spid3r_bin())
        .args(["verify-runtime"])
        .arg(&dir)
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "verify-runtime valid failed: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    assert!(String::from_utf8_lossy(&ok.stdout).contains("verified runtime staticlib:"));

    // Tampered staticlib exits 4
    std::fs::write(&lib_path, b"tampered-bytes").unwrap();
    let bad = Command::new(spid3r_bin())
        .args(["verify-runtime"])
        .arg(&dir)
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(4));

    // Missing path exits 2
    let miss = Command::new(spid3r_bin())
        .args(["verify-runtime", "definitely-does-not-exist-runtime"])
        .output()
        .unwrap();
    assert_eq!(miss.status.code(), Some(2));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bug_17_mutex_set_store_and_with_scalar_mutation() {
    let out = run_both_engines(
        "bug17",
        r#"fn main() -> int {
    let m = mutex(0);
    m.set(5);
    println(string(m.lock()));
    m.set(9);
    m.unlock();
    println(string(m.lock()));
    m.unlock();
    m.store(10);
    let next = m.with(fn(v) { return v + 32; });
    println(string(next));
    println(string(m.lock()));
    m.unlock();
    return 0;
}
"#,
        "matrix",
    );
    assert_eq!(out, "5\n9\n42\n42\n");
}

#[test]
fn bug_18_map_remove_and_delete_return_removed_value() {
    let out = run_both_engines(
        "bug18",
        r#"fn main() -> int {
    let m = {"a": 1, "b": 2};
    println("remove=" + string(m.remove("a")) + " delete=" + string(m.delete("b")));
    return 0;
}
"#,
        "matrix",
    );
    assert_eq!(out, "remove=1 delete=2\n");
}

#[test]
fn bug_20_json_parse_rejects_raw_control_characters() {
    let out = run_both_engines(
        "bug20",
        r#"fn main() -> int {
    for code in [0, 1, 10, 31] {
        let raw = "\"a" + chr(code) + "b\"";
        let failed = false;
        try {
            json_parse(raw);
        } catch (_e) {
            failed = true;
        }
        println(string(code) + ":" + string(failed));
    }
    let ok = json_parse("\"a\\u0001b\"");
    println(json_write(ok));
    return 0;
}
"#,
        "matrix",
    );
    assert_eq!(out, "0:true\n1:true\n10:true\n31:true\n\"a\\u0001b\"\n");
}

#[cfg(feature = "gui")]
#[test]
fn bug_22_to_31_gui_bridge_options_surrogates_and_bare_payload() {
    use spid3r_lang::gui::bridge::{self, WindowCommand, WindowTheme};
    use spid3r_lang::gui::WindowOptions;

    // BUG-27: UTF-16 surrogate pair decoding in GUI IPC & control JSON decoder
    let (ev, payload) =
        bridge::decode_ipc_message(r#"{"event":"surrogate","payload":"\uD83D\uDE00"}"#)
            .expect("decode surrogate pair");
    assert_eq!(ev, "surrogate");
    assert_eq!(payload, "😀");

    let (_, lone) =
        bridge::decode_ipc_message(r#"{"event":"lone","payload":"\uD83Dabc"}"#).expect("decode");
    assert_eq!(lone, "\u{fffd}abc");

    assert_eq!(
        spid3r_lang::control::ops::field(r#"{"msg":"\uD83D\uDE00"}"#, "msg").as_deref(),
        Some("😀")
    );

    // BUG-22/23/26: Phase-1 window commands fold cleanly into WindowOptions
    let mut opts = WindowOptions::default();
    assert_eq!(opts.min_size, Some((320.0, 200.0)));
    let closed = opts.apply_phase1_cmds(vec![
        WindowCommand::Size(1280.0, 800.0),
        WindowCommand::MinSize(800.0, 600.0),
        WindowCommand::Maximized(true),
        WindowCommand::Theme(WindowTheme::Dark),
        WindowCommand::BgColor(0x1e, 0x1e, 0x1e, 0xff),
        WindowCommand::Icon("app.ico".to_string()),
        WindowCommand::DevTools(true),
    ]);
    assert!(!closed);
    assert_eq!((opts.width, opts.height), (1280.0, 800.0));
    assert_eq!(opts.min_size, Some((800.0, 600.0)));
    assert!(opts.maximized);
    assert_eq!(opts.theme, Some(WindowTheme::Dark));
    assert_eq!(opts.bg_color, Some((0x1e, 0x1e, 0x1e, 0xff)));
    assert_eq!(opts.icon.as_deref(), Some("app.ico"));
    assert!(opts.devtools);

    // BUG-28: `spid3r gui --help` accurately describes `--html` with `--payload`
    let help = Command::new(spid3r_bin())
        .args(["gui", "--help"])
        .output()
        .unwrap();
    let help_txt = String::from_utf8_lossy(&help.stdout);
    assert!(help_txt.contains("--icon"));
    assert!(help_txt.contains("--width"));
    assert!(help_txt.contains("--devtools"));
    assert!(help_txt.contains("payload entry name only"));

    // BUG-31: bare `--payload dummy.exe` finds loose `.matrix` / `.mt` beside it in cwd
    let dir = scratch_dir("bug31");
    std::fs::write(dir.join("dummy.exe"), b"MZ-not-embedded").unwrap();
    std::fs::write(
        dir.join("main.mt"),
        "fn main() -> int {\n    gui_size(1024, 768);\n    gui_min_size(640, 480);\n    gui_theme(\"dark\");\n    gui_bg_color(\"#1e1e1e\");\n    gui_set_html(\"<p>ok</p>\");\n    return 0;\n}\n",
    )
    .unwrap();
    let bare = Command::new(spid3r_bin())
        .current_dir(&dir)
        .args([
            "gui",
            "--payload",
            "dummy.exe",
            "--width",
            "1280",
            "--height",
            "800",
            "--devtools",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(
        bare.status.success(),
        "bare --payload failed: stdout={} stderr={}",
        String::from_utf8_lossy(&bare.stdout),
        String::from_utf8_lossy(&bare.stderr)
    );
    assert!(String::from_utf8_lossy(&bare.stdout).contains("<p>ok</p>"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bug_11_enum_namespace_is_frozen_and_handles_method_names_and_nested_match() {
    let dir = scratch_dir("bug11_frozen");
    std::fs::write(
        dir.join("colors.mt"),
        "public enum Color {\n    Red,\n    Rgb(r, g, b),\n}\n",
    )
    .unwrap();
    let main_p = dir.join("main.mt");
    std::fs::write(
        &main_p,
        r#"import "./colors" as C;

enum Act {
    set(k, v),
    get(k),
    keys,
}

fn main() -> int {
    let c = C.Color.Rgb(1, 2, 3);
    let label = match (c) {
        C.Color.Red => "red",
        C.Color.Rgb(r, g, b) => string(r + g + b),
    };
    println("label=" + label);

    let a = Act.set("x", 9);
    let av = match (a) {
        Act.set(k, v) => k + "=" + string(v),
        Act.get(k) => k,
        Act.keys => "keys",
    };
    println("act=" + av);

    let frozen = false;
    try {
        C.Color["Red"] = 999;
    } catch (_e) {
        frozen = true;
    }
    println("frozen=" + string(frozen));
    return 0;
}
"#,
    )
    .unwrap();

    for eng in ["tree", "vm"] {
        let out = Command::new(sp_bin())
            .args(["run", "--engine", eng])
            .arg(&main_p)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "bug11 frozen ({eng}) failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "label=6\nact=x=9\nfrozen=true\n"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
