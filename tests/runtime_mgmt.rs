//! End-to-end tests for spid3r.ini, spid3rinfo(), and the management agent.
//!
//! The agent tests bind 127.0.0.1 on an ephemeral port, pair a client, and
//! exercise the predefined operations. No unrestricted shell is ever granted.

use std::net::{TcpListener, TcpStream};
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
    let (code, stdout, _) = run_cli(&["config", "list"]);
    assert_eq!(code, 0);
    // Grouped layout: one [section] header per dotted prefix.
    assert!(
        stdout.contains("[agent]"),
        "missing [agent] group in {}",
        stdout
    );
    assert!(stdout.contains("port") && stdout.contains("9527"));
    assert!(stdout.contains("[spid3r]"), "missing [spid3r] group");
    assert!(stdout.contains("engine") && stdout.contains("vm"));
    let (code, stdout, _) = run_cli(&["config", "validate"]);
    assert_eq!(code, 0, "{}", stdout);
    assert!(stdout.contains("ok"));
}

#[test]
fn config_get_roundtrip() {
    let dir = std::env::temp_dir().join(format!("spid3r_cfg_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ini = dir.join("spid3r.ini");
    std::fs::write(&ini, "[agent]\nport = 9527\n").unwrap();
    let out = Command::new(bin())
        .args(["config", "get", "agent.port"])
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
    let p = config_get("agent.port");
    println(if p == null { "null" } else { "set" });
    println(config_set("agent.name", "test"));
    println(config_get("agent.name"));
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

/// Bind an ephemeral port, start the agent on it, pair, and call ping.
#[test]
fn agent_pair_and_ping() {
    let probe = TcpListener::bind("127.0.0.1:0").expect("probe bind");
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    let dir = std::env::temp_dir().join(format!("spid3r_agent_{}", port));
    std::fs::create_dir_all(&dir).unwrap();
    let ini = dir.join("spid3r.ini");
    std::fs::write(
        &ini,
        format!(
            "[agent]\nenabled = true\nbind = 127.0.0.1\nport = {}\n[security]\nallow_remote = true\n",
            port
        ),
    )
    .unwrap();
    let state = dir.join("state");
    std::fs::create_dir_all(&state).unwrap();

    let exe = bin();
    let envs = [
        ("SPID3R_INI", ini.to_string_lossy().into_owned()),
        ("SPID3R_STATE_DIR", state.to_string_lossy().into_owned()),
        ("SPID3R_CONFIG_DIR", dir.to_string_lossy().into_owned()),
    ];
    let mut child = Command::new(&exe)
        .args(["agent", "start"])
        .envs(envs.iter().map(|(a, b)| (a, b)))
        .spawn()
        .expect("start agent");

    // Kill the agent on EVERY exit path: a panic between spawn and the
    // success-path kill used to leak a running agent into later tests, which
    // hung a macOS CI run for the whole 30-minute step timeout.
    struct KillOnDrop<'a>(&'a mut std::process::Child);
    impl Drop for KillOnDrop<'_> {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _guard = KillOnDrop(&mut child);

    let mut connected = false;
    for _ in 0..50 {
        if TcpStream::connect(format!("127.0.0.1:{}", port)).is_ok() {
            connected = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(connected, "agent did not listen on {}", port);

    let out = Command::new(&exe)
        .args(["agent", "pair", "--role", "admin", "--name", "tester"])
        .envs(envs.iter().map(|(a, b)| (a, b)))
        .output()
        .expect("pair code");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "pair-code failed: {}", stdout);
    let code = stdout
        .lines()
        .find_map(|l| l.split_whitespace().last().map(String::from))
        .filter(|s| s.len() >= 16)
        .unwrap_or_else(|| panic!("no code in {}", stdout));

    let out = Command::new(&exe)
        .args([
            "admin",
            "pair",
            &format!("127.0.0.1:{}", port),
            &code,
            "--name",
            "ci",
        ])
        .envs(envs.iter().map(|(a, b)| (a, b)))
        .output()
        .expect("pair");
    let pstdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let pstderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "pair failed: {} {}", pstdout, pstderr);
    assert!(
        pstdout.contains("paired"),
        "unexpected pair output {}",
        pstdout
    );

    let call = |args: &[&str]| -> (bool, String, String) {
        let out = Command::new(&exe)
            .args(args)
            .envs(envs.iter().map(|(a, b)| (a, b)))
            .output()
            .expect("admin call");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    let (ok, stdout, stderr) = call(&["admin", "--host", "ci", "ping"]);
    assert!(ok, "ping: {} {}", stdout, stderr);
    assert!(stdout.contains("pong"), "ping body {}", stdout);

    let (ok, stdout, stderr) = call(&["admin", "--host", "ci", "status"]);
    assert!(ok, "status: {} {}", stdout, stderr);
    assert!(stdout.contains("version"), "status body {}", stdout);

    let (ok, stdout, stderr) =
        call(&["admin", "--host", "ci", "ops.run", "--op-name", "self_test"]);
    assert!(ok, "self_test: {} {}", stdout, stderr);
    assert!(stdout.contains("passed"), "self_test body {}", stdout);

    let (ok, stdout, stderr) = call(&["admin", "--host", "ci", "info"]);
    assert!(ok, "info: {} {}", stdout, stderr);
    assert!(stdout.contains("Spid3r"), "info body {}", stdout);

    // The KillOnDrop guard kills and reaps the agent here (and on any
    // earlier panic path).
    drop(_guard);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn admin_unknown_host_fails() {
    let (code, _, stderr) = run_cli(&["admin", "--host", "no-such-host-xyz", "ping"]);
    assert_ne!(code, 0);
    assert!(stderr.contains("unknown host") || stderr.contains("no paired"));
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
