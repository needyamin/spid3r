//! CLI integration tests for `spid3r neofetch`.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

#[test]
fn neofetch_prints_all_sections_and_no_color_flag_works() {
    let out = Command::new(bin())
        .args(["neofetch", "--no-color"])
        .output()
        .expect("spawn spid3r neofetch");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("S P I D 3 R"), "stdout:\n{stdout}");
    assert!(stdout.contains("TOOLCHAIN"), "stdout:\n{stdout}");
    assert!(stdout.contains("SYSTEM"), "stdout:\n{stdout}");
    assert!(stdout.contains("HARDWARE"), "stdout:\n{stdout}");
    assert!(stdout.contains("POWER & BATTERY"), "stdout:\n{stdout}");
    assert!(stdout.contains("NETWORK & ENGINE"), "stdout:\n{stdout}");
    assert!(stdout.contains("Native Core Fetch"), "stdout:\n{stdout}");
    assert!(stdout.contains("0 Deps"), "stdout:\n{stdout}");
    assert!(stdout.contains("spvm"), "stdout:\n{stdout}");
    assert!(stdout.contains("YamGC"), "stdout:\n{stdout}");
    assert!(stdout.contains("ANSNEW TECH"), "stdout:\n{stdout}");
    assert!(stdout.contains("Spid3r Engine"), "stdout:\n{stdout}");
    if cfg!(feature = "native") {
        assert!(stdout.contains("YamLLVM"), "stdout:\n{stdout}");
    } else {
        assert!(stdout.contains("yamcvm"), "stdout:\n{stdout}");
    }
    assert!(!stdout.contains("Rust Stg0"), "stdout:\n{stdout}");
    assert!(!stdout.contains("Self-Hosted AOT"), "stdout:\n{stdout}");
    assert!(
        !stdout.contains("\x1b["),
        "--no-color emitted ANSI escapes:\n{stdout}"
    );
}

#[test]
fn neofetch_help_and_unknown_flag_exit_codes() {
    let help = Command::new(bin())
        .args(["neofetch", "--help"])
        .output()
        .expect("spawn spid3r neofetch --help");
    assert_eq!(help.status.code(), Some(0));
    let help_out = String::from_utf8_lossy(&help.stdout);
    assert!(help_out.contains("spid3r neofetch"), "help:\n{help_out}");

    let bad = Command::new(bin())
        .args(["neofetch", "--definitely-invalid-flag"])
        .output()
        .expect("spawn spid3r neofetch --definitely-invalid-flag");
    assert_eq!(bad.status.code(), Some(2));
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(err.contains("error[usage]"), "stderr:\n{err}");
}
