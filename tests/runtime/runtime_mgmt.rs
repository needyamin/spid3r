//! End-to-end tests for spid3r.ini and spid3rinfo().

use std::process::Command;

fn bin() -> std::path::PathBuf {
    for key in ["CARGO_BIN_EXE_spid3r", "CARGO_BIN_EXE_spidr"] {
        if let Ok(p) = std::env::var(key) {
            let p = std::path::PathBuf::from(p);
            if p.is_file() {
                return p;
            }
        }
    }
    let exe = std::env::current_exe().unwrap();
    let profile = exe.parent().and_then(|d| d.parent()).unwrap();
    let cand = profile.join(format!("spid3r{}", std::env::consts::EXE_SUFFIX));
    assert!(
        cand.is_file(),
        "spid3r binary not found at {}",
        cand.display()
    );
    cand
}

fn run_cli(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(bin()).args(args).output().expect("spawn");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn info_json_has_version_and_sections() {
    let (code, stdout, stderr) = run_cli(&["info", "--json"]);
    assert_eq!(code, 0, "stderr: {}", stderr);
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "no version in {}",
        stdout
    );
    assert!(stdout.contains("Spid3r version"), "missing version row");
}

#[test]
fn info_html_and_text() {
    let (code, stdout, _) = run_cli(&["info", "--html"]);
    assert_eq!(code, 0);
    assert!(stdout.starts_with("<!DOCTYPE html>"));
    assert!(stdout.contains("spid3rinfo"));
    let (code, stdout, _) = run_cli(&["info"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("Spid3r ") && stdout.contains("runtime information"));
}

#[test]
fn config_list_and_validate() {
    let dir = std::env::temp_dir().join(format!("spid3r_cfg_list_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ini = dir.join("spid3r.ini");
    std::fs::write(&ini, "").unwrap();
    let out = Command::new(bin())
        .args(["config", "list"])
        .env("SPID3R_INI", &ini)
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Grouped layout: one [section] header per dotted prefix.
    assert!(
        stdout.contains("[net]"),
        "missing [net] group in {}",
        stdout
    );
    assert!(stdout.contains("timeout_ms") && stdout.contains("30000"));
    assert!(stdout.contains("[spid3r]"), "missing [spid3r] group");
    assert!(stdout.contains("engine") && stdout.contains("vm"));
    let out = Command::new(bin())
        .args(["config", "validate"])
        .env("SPID3R_INI", &ini)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{}", stdout);
    assert!(stdout.contains("ok"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn config_get_roundtrip() {
    let dir = std::env::temp_dir().join(format!("spid3r_cfg_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ini = dir.join("spid3r.ini");
    std::fs::write(&ini, "[net]\ntimeout_ms = 9527\n").unwrap();
    let out = Command::new(bin())
        .args(["config", "get", "net.timeout_ms"])
        .env("SPID3R_INI", &ini)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "9527");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spid3rinfo_and_config_natives() {
    let dir = std::env::temp_dir().join(format!("spid3r_native_info_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("main.matrix");
    std::fs::write(
        &src,
        r#"fn main() {
    let m = spid3rinfo();
    println(m["Spid3r"]["Spid3r version"]);
    println(type_of(spid3rinfo("json")));
    let p = config_get("net.timeout_ms");
    println(if p == null { "null" } else { "set" });
    println(config_set("spid3r.env", "test"));
    println(config_get("spid3r.env"));
    return 0;
}
"#,
    )
    .unwrap();
    for engine in ["tree", "vm"] {
        let out = Command::new(bin())
            .args(["run", "--engine", engine, src.to_str().unwrap()])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "engine {} stderr {}", engine, stderr);
        assert!(
            stdout.contains(env!("CARGO_PKG_VERSION")),
            "{}: {}",
            engine,
            stdout
        );
        assert!(stdout.contains("string"), "{}: {}", engine, stdout);
        assert!(stdout.contains("set"), "{}: {}", engine, stdout);
        assert!(stdout.contains("test"), "{}: {}", engine, stdout);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn run_with_ini(
    tag: &str,
    ini_body: &str,
    script_body: &str,
    extra_args: &[&str],
) -> (i32, String, String) {
    let dir = std::env::temp_dir().join(format!("spid3r-ini-teeth-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ini = dir.join("spid3r.ini");
    let script = dir.join("prog.matrix");
    std::fs::write(&ini, ini_body).unwrap();
    std::fs::write(&script, script_body).unwrap();

    let mut cmd = Command::new(bin());
    cmd.args(extra_args)
        .arg(&script)
        .env("SPID3R_INI", &ini)
        .env("SPID3R_STATE_DIR", dir.join("state"));
    let out = cmd.output().expect("run spid3r with ini");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    (code, stdout, stderr)
}

#[test]
fn ini_security_deny_filesystem_blocks_vm_tree_and_exec() {
    let ini = "[security]\ndeny_filesystem = true\n";
    let prog = "fn main() { try { read_file(\"Cargo.toml\"); println(\"ALLOWED\"); } catch (e) { println(\"DENIED:\" + string(e)); } }\n";

    // VM engine
    let (code, out, err) = run_with_ini("deny-fs-vm", ini, prog, &["run", "--engine", "vm"]);
    assert_eq!(code, 0, "vm stderr: {err}");
    assert!(
        out.contains("DENIED:sandboxed: filesystem access denied"),
        "vm out: {out}"
    );

    // Tree engine
    let (code, out, err) = run_with_ini("deny-fs-tree", ini, prog, &["run", "--engine", "tree"]);
    assert_eq!(code, 0, "tree stderr: {err}");
    assert!(
        out.contains("DENIED:sandboxed: filesystem access denied"),
        "tree out: {out}"
    );

    // Compiled .matrixc via `spid3r exec`
    let dir = std::env::temp_dir().join(format!("spid3r-ini-exec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ini_path = dir.join("spid3r.ini");
    let src_path = dir.join("p.matrix");
    let bc_path = dir.join("p.matrixc");
    std::fs::write(&ini_path, ini).unwrap();
    std::fs::write(&src_path, prog).unwrap();
    let c = Command::new(bin())
        .args([
            "compile",
            src_path.to_str().unwrap(),
            "-o",
            bc_path.to_str().unwrap(),
        ])
        .env("SPID3R_INI", &ini_path)
        .output()
        .unwrap();
    assert!(
        c.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&c.stderr)
    );
    let ex = Command::new(bin())
        .args(["exec", bc_path.to_str().unwrap()])
        .env("SPID3R_INI", &ini_path)
        .output()
        .unwrap();
    let ex_out = String::from_utf8_lossy(&ex.stdout);
    assert!(
        ex.status.success(),
        "exec failed: {}",
        String::from_utf8_lossy(&ex.stderr)
    );
    assert!(
        ex_out.contains("DENIED:sandboxed: filesystem access denied"),
        "exec out: {ex_out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ini_security_deny_process_network_and_env_block_matching_natives() {
    let ini = "[security]\ndeny_process = true\ndeny_network = true\ndeny_environment = true\n";
    let prog = r#"
fn main() {
    try { exec_capture("echo", ["hi"]); println("PROC:OK"); } catch (e) { println("PROC:" + string(e)); }
    try { tcp_connect("127.0.0.1:1"); println("NET:OK"); } catch (e) { println("NET:" + string(e)); }
    try { env("PATH"); println("ENV:OK"); } catch (e) { println("ENV:" + string(e)); }
}
"#;
    let (code, out, err) = run_with_ini("deny-pne", ini, prog, &["run"]);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("PROC:sandboxed: process access denied"),
        "out: {out}"
    );
    assert!(
        out.contains("NET:sandboxed: network access denied"),
        "out: {out}"
    );
    assert!(
        out.contains("ENV:sandboxed: environment access denied"),
        "out: {out}"
    );
}

#[test]
fn ini_memory_limits_enforce_steps_depth_and_allocation_cap() {
    // memory.max_steps aborts infinite loop
    let (code, _, err) = run_with_ini(
        "max-steps",
        "[memory]\nmax_steps = 500\n",
        "fn main() { let i = 0; while (true) { i = i + 1; } }\n",
        &["run"],
    );
    assert_ne!(code, 0, "expected budget error");
    assert!(
        err.to_lowercase().contains("budget") || err.to_lowercase().contains("step"),
        "unexpected stderr: {err}"
    );

    // memory.max_call_depth aborts deep recursion
    let (code, _, err) = run_with_ini(
        "max-depth",
        "[memory]\nmax_call_depth = 20\n",
        "fn f(n) { if (n <= 0) { return 0; } return f(n - 1); }\nfn main() { f(50); }\n",
        &["run"],
    );
    assert_ne!(code, 0, "expected recursion limit error");
    assert!(
        err.to_lowercase().contains("depth") || err.to_lowercase().contains("recursion"),
        "unexpected stderr: {err}"
    );

    // memory.allocation_cap_mb rejects a >1 MiB single allocation as a catchable error
    let (code, out, err) = run_with_ini(
        "alloc-cap",
        "[memory]\nallocation_cap_mb = 1\n",
        "fn main() { try { let b = bytes(2 * 1024 * 1024); println(\"ALLOC:OK\"); } catch (e) { println(\"ALLOC:\" + string(e)); } }\n",
        &["run"],
    );
    assert_eq!(code, 0, "stderr: {err}");
    assert!(
        out.contains("ALLOC:allocation limit exceeded"),
        "out: {out}"
    );
}

#[test]
fn ini_modules_path_resolves_custom_directory() {
    let dir = std::env::temp_dir().join(format!("spid3r-ini-modpath-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mod_dir = dir.join("custom_mods");
    std::fs::create_dir_all(&mod_dir).unwrap();
    std::fs::write(
        mod_dir.join("greet_cfg.matrix"),
        "public fn hi() { return \"from_ini_path\"; }\n",
    )
    .unwrap();
    let ini = dir.join("spid3r.ini");
    std::fs::write(
        &ini,
        format!(
            "[modules]\npath = \"{}\"\n",
            mod_dir.display().to_string().replace('\\', "\\\\")
        ),
    )
    .unwrap();
    let script = dir.join("main.matrix");
    std::fs::write(
        &script,
        "import \"greet_cfg\";\nfn main() { println(hi()); }\n",
    )
    .unwrap();

    let out = Command::new(bin())
        .args(["run", script.to_str().unwrap()])
        .env("SPID3R_INI", &ini)
        .env_remove("SPID3R_PATH")
        .env_remove("SPID3R_MODULES_PATH")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    assert!(stdout.contains("from_ini_path"), "stdout: {stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}
