//! Integration tests for Phase R7.2 (`spawn_process` `env` & `cwd` options),
//! Phase R7.3 (`signal_requested`), and Phase R7.4 (`uds_listen`, `uds_connect`).

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r_r7_proc_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

#[test]
fn spawn_process_passes_env_and_cwd_options() {
    let dir = scratch_dir("env_cwd");
    let sub = dir.join("child_work_dir");
    std::fs::create_dir_all(&sub).expect("mkdir sub");

    // Child script checks its env("R7_CHILD_VAR") and writes cwd().unwrap() to a marker file
    let child_script = sub.join("child.matrix");
    std::fs::write(
        &child_script,
        r#"fn main() {
    let v = env("R7_CHILD_VAR").unwrap();
    let d = cwd().unwrap();
    write_file("child_out.txt", v + "|" + d).unwrap();
    return 0;
}
"#,
    )
    .expect("write child");

    let parent_script = dir.join("parent.matrix");
    let parent_src = format!(
        r#"fn main() {{
    let exe = exe_path().unwrap();
    let opts = map(
        "cwd", {:?},
        "env", map("R7_CHILD_VAR", "secret_value_42")
    );
    let rc = spawn_process(exe, ["run", "child.matrix"], opts).unwrap();
    println("rc=" + string(rc));
    return 0;
}}
"#,
        sub.to_str().unwrap()
    );
    std::fs::write(&parent_script, parent_src).expect("write parent");

    let out = Command::new(bin())
        .args(["run", parent_script.to_str().unwrap()])
        .current_dir(&dir)
        .output()
        .expect("run parent");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "rc=0");

    let child_out = std::fs::read_to_string(sub.join("child_out.txt")).expect("read child_out.txt");
    let (val, child_cwd) = child_out.split_once('|').expect("split output");
    assert_eq!(val, "secret_value_42");
    assert_eq!(
        std::fs::canonicalize(child_cwd).unwrap(),
        std::fs::canonicalize(&sub).unwrap()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spawn_process_cwd_honors_security_basedir() {
    let dir = scratch_dir("cwd_basedir");
    let allowed = dir.join("allowed");
    let outside = dir.join("outside");
    std::fs::create_dir_all(&allowed).expect("mkdir allowed");
    std::fs::create_dir_all(&outside).expect("mkdir outside");

    let ini = format!(
        "[security]\nbasedir = {}\n",
        allowed.to_str().unwrap().replace('\\', "/")
    );
    let ini_path = dir.join("spid3r.ini");
    std::fs::write(&ini_path, ini).expect("write ini");

    let script = allowed.join("escape_cwd.matrix");
    let src = format!(
        r#"fn main() {{
    let exe = exe_path().unwrap();
    let r = spawn_process(exe, ["--version"], map("cwd", {:?}));
    println(r);
    return 0;
}}
"#,
        outside.to_str().unwrap()
    );
    std::fs::write(&script, src).expect("write script");

    let out = Command::new(bin())
        .args(["run", script.to_str().unwrap()])
        .env("SPID3R_INI", ini_path.to_str().unwrap())
        .current_dir(&allowed)
        .output()
        .expect("run escape_cwd");
    assert_ne!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("outside basedir"),
        "expected basedir denial on spawn_process cwd, got: {}",
        stderr
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn signal_requested_registers_and_validates_names() {
    let dir = scratch_dir("signals");
    let script = dir.join("sig.matrix");
    std::fs::write(
        &script,
        r#"fn main() {
    reset_shutdown();
    println(signal_requested("SIGINT"));
    println(signal_requested("SIGTERM"));
    println(signal_requested("SIGHUP"));
    println(signal_requested("SIGUSR1"));
    println(signal_requested("SIGUSR2"));
    println(signal_requested("hup", false));
    return 0;
}
"#,
    )
    .expect("write sig");

    let out = Command::new(bin())
        .args(["run", script.to_str().unwrap()])
        .current_dir(&dir)
        .output()
        .expect("run sig");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        "false\nfalse\nfalse\nfalse\nfalse\nfalse\n"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn unix_sighup_and_sigusr1_set_signal_requested() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let dir = scratch_dir("unix_sig");
    let script = dir.join("wait_sig.matrix");
    std::fs::write(
        &script,
        r#"fn main() {
    // Install handlers by polling once
    let _ = signal_requested("SIGHUP");
    let _ = signal_requested("SIGUSR1");
    println("READY");
    let got_hup = false;
    let got_usr1 = false;
    let i = 0;
    while i < 200 and (not got_hup or not got_usr1) {
        if signal_requested("SIGHUP") {
            got_hup = true;
        }
        if signal_requested("SIGUSR1") {
            got_usr1 = true;
        }
        sleep_ms(20);
        i = i + 1;
    }
    // Second poll after swap(false) must be false
    let after_hup = signal_requested("SIGHUP");
    println("HUP=" + string(got_hup) + " USR1=" + string(got_usr1) + " AFTER=" + string(after_hup));
    return 0;
}
"#,
    )
    .expect("write wait_sig");

    let mut child = Command::new(bin())
        .args(["run", script.to_str().unwrap()])
        .current_dir(&dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn child");

    let pid = child.id() as i32;
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read READY");
    assert_eq!(line.trim(), "READY");

    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    #[cfg(target_os = "macos")]
    const SIGUSR1_NUM: i32 = 30;
    #[cfg(not(target_os = "macos"))]
    const SIGUSR1_NUM: i32 = 10;

    unsafe {
        kill(pid, 1); // SIGHUP
        kill(pid, SIGUSR1_NUM); // SIGUSR1
    }

    line.clear();
    reader.read_line(&mut line).expect("read result");
    let status = child.wait().expect("wait child");
    assert!(status.success());
    assert_eq!(line.trim(), "HUP=true USR1=true AFTER=false");

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn unix_uds_listen_and_connect_round_trip() {
    let dir = scratch_dir("uds");
    let sock_path = dir.join("test.sock");
    let script = dir.join("uds.matrix");
    let src = format!(
        r#"fn main() {{
    let p = {:?};
    let l = uds_listen(p).unwrap();
    let srv = spawn fn() {{
        let c = tcp_accept(l).unwrap();
        let msg = tcp_read(c, 64).unwrap();
        tcp_write(c, "echo:" + msg).unwrap();
        tcp_close(c);
        tcp_close(l);
        return 0;
    }};
    let cli = uds_connect(p).unwrap();
    tcp_write(cli, "hello-uds").unwrap();
    let reply = tcp_read(cli, 64).unwrap();
    tcp_close(cli);
    srv.join();
    println(reply);
    println(file_exists(p));
    return 0;
}}
"#,
        sock_path.to_str().unwrap()
    );
    std::fs::write(&script, src).expect("write uds.matrix");

    let out = Command::new(bin())
        .args(["run", script.to_str().unwrap()])
        .current_dir(&dir)
        .output()
        .expect("run uds");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        "echo:hello-uds\nfalse\n"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(not(unix))]
#[test]
fn non_unix_uds_returns_clean_error_result() {
    let dir = scratch_dir("uds_non_unix");
    let script = dir.join("uds.matrix");
    std::fs::write(
        &script,
        r#"fn main() {
    let r1 = uds_listen("test.sock");
    let r2 = uds_connect("test.sock");
    assert(r1.is_err());
    assert(r2.is_err());
    println("UDS_CLEAN_ERR");
    return 0;
}
"#,
    )
    .expect("write uds");

    let out = Command::new(bin())
        .args(["run", script.to_str().unwrap()])
        .current_dir(&dir)
        .output()
        .expect("run uds");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "UDS_CLEAN_ERR");

    let _ = std::fs::remove_dir_all(&dir);
}
