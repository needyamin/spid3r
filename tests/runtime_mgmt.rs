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
    assert!(cand.is_file(), "spid3r binary not found at {}", cand.display());
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
    assert!(stdout.contains("1.0.0"), "no version in {}", stdout);
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
    assert!(stdout.contains("== Spid3r =="));
}

#[test]
fn config_list_and_validate() {
    let (code, stdout, _) = run_cli(&["config", "list"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("agent.port = 9527"));
    assert!(stdout.contains("spid3r.engine = vm"));
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
        assert!(stdout.contains("1.0.0"), "{}: {}", engine, stdout);
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
        .args(["admin", "pair", &format!("127.0.0.1:{}", port), &code, "--name", "ci"])
        .envs(envs.iter().map(|(a, b)| (a, b)))
        .output()
        .expect("pair");
    let pstdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let pstderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "pair failed: {} {}", pstdout, pstderr);
    assert!(pstdout.contains("paired"), "unexpected pair output {}", pstdout);

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

    let (ok, stdout, stderr) = call(&["admin", "--host", "ci", "ops.run", "--op-name", "self_test"]);
    assert!(ok, "self_test: {} {}", stdout, stderr);
    assert!(stdout.contains("passed"), "self_test body {}", stdout);

    let (ok, stdout, stderr) = call(&["admin", "--host", "ci", "info"]);
    assert!(ok, "info: {} {}", stdout, stderr);
    assert!(stdout.contains("Spid3r"), "info body {}", stdout);

    let _ = child.kill();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn admin_unknown_host_fails() {
    let (code, _, stderr) = run_cli(&["admin", "--host", "no-such-host-xyz", "ping"]);
    assert_ne!(code, 0);
    assert!(stderr.contains("unknown host") || stderr.contains("no paired"));
}
