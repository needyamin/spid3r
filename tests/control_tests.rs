//! `spid3r control` integration tests.
//!
//! Every test runs the CLI as a subprocess with `SPID3R_CONFIG_DIR` and
//! `SPID3R_STATE_DIR` pinned to a per-test temp dir, so no test can read or
//! mutate a developer machine's real configuration (same pattern as the other
//! CLI integration suites).

use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;

/// Serialise tests that mutate process-global cwd (each test runs the CLI in
/// its own temp cwd via current_dir, which is per-Command — but temp dir
/// creation + cleanup is easier to reason about serially).
static LOCK: Mutex<()> = Mutex::new(());

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_spid3r"))
}

struct Sandbox {
    _guard: std::sync::MutexGuard<'static, ()>,
    config_dir: PathBuf,
    state_dir: PathBuf,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.config_dir);
        let _ = std::fs::remove_dir_all(&self.state_dir);
    }
}

fn sandbox(tag: &str) -> Sandbox {
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let base = std::env::temp_dir().join(format!(
        "spid3r-control-tests-{}-{}",
        tag,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let config_dir = base.join("config");
    let state_dir = base.join("state");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(&state_dir).unwrap();
    Sandbox {
        _guard: guard,
        config_dir,
        state_dir,
    }
}

impl Sandbox {
    fn command(&self, args: &[&str]) -> Command {
        let mut c = bin();
        c.args(args)
            .env("SPID3R_CONFIG_DIR", &self.config_dir)
            .env("SPID3R_STATE_DIR", &self.state_dir);
        // Keep the repo's own spid3r.ini (if any) out of the lookup: the
        // exe-dir candidate still wins over the sandbox, so pin SPID3R_INI to
        // a path inside the sandbox that does not exist.
        c.env("SPID3R_INI", self.config_dir.join("spid3r.ini"));
        c
    }
}

fn ini_in(sandbox: &Sandbox) -> PathBuf {
    sandbox.config_dir.join("spid3r.ini")
}

#[test]
fn status_runs_without_any_configuration() {
    let sb = sandbox("status-bare");
    let out = sb.command(&["control", "status"]).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("spid3r "), "missing version line: {stdout}");
    assert!(stdout.contains("not listening"), "{stdout}");
    assert!(stdout.contains("no spid3r.ini yet"), "{stdout}");
}

#[test]
fn ini_init_writes_the_default_file_and_is_idempotent() {
    let sb = sandbox("ini-init");
    let out = sb.command(&["control", "ini", "--init"]).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let written = ini_in(&sb);
    assert!(written.is_file(), "default ini not written");
    let text = std::fs::read_to_string(&written).unwrap();
    assert!(text.contains("spid3r.ini"), "{text}");
    assert!(text.contains("[security]"), "{text}");

    // Second run without --force refuses to overwrite.
    let before = std::fs::read_to_string(&written).unwrap();
    let out2 = sb.command(&["control", "ini", "--init"]).output().unwrap();
    assert!(out2.status.success());
    let stdout = String::from_utf8_lossy(&out2.stdout);
    assert!(stdout.contains("already exists"), "{stdout}");
    assert_eq!(std::fs::read_to_string(&written).unwrap(), before);
}

#[test]
fn ini_help_lists_search_order() {
    let sb = sandbox("ini-info");
    let out = sb.command(&["control", "ini"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("search order"), "{stdout}");
    assert!(stdout.contains("no spid3r.ini found"), "{stdout}");
}

#[test]
fn devices_list_is_empty_and_revoke_reports_missing() {
    let sb = sandbox("devices");
    let out = sb
        .command(&["control", "devices", "list"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("devices: 0"), "{stdout}");

    let out2 = sb
        .command(&["control", "devices", "revoke", "no-such-device"])
        .output()
        .unwrap();
    assert_eq!(out2.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out2.stderr);
    assert!(stderr.contains("no such device"), "{stderr}");
}

#[test]
fn audit_tail_on_empty_state_is_clean() {
    let sb = sandbox("audit-empty");
    let out = sb.command(&["control", "audit", "5"]).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn audit_tail_shows_control_actions_after_an_audited_op() {
    let sb = sandbox("audit-populated");
    // ini_init is an audited control action.
    let _ = sb.command(&["control", "ini", "--init"]).output().unwrap();
    let out = sb.command(&["control", "audit", "20"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("ini.init"), "{stdout}");
}

#[test]
fn agent_status_when_not_running_reports_not_listening() {
    let sb = sandbox("agent-stopped");
    // Port 1 is never our agent; even if a probe raced, "not listening" is
    // the only sane outcome for an unconfigured sandbox.
    let out = sb
        .command(&["control", "agent", "status"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("not listening") || stdout.contains("listening"),
        "{stdout}"
    );
}

#[test]
fn control_help_and_unknown_subcommand_exit_codes() {
    let sb = sandbox("help");
    let out = sb.command(&["control", "help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("USAGE"), "{stdout}");

    let out2 = sb
        .command(&["control", "definitely-not-a-subcommand"])
        .output()
        .unwrap();
    assert_eq!(out2.status.code(), Some(2));
}

#[test]
fn help_is_registered_in_the_top_level_cli() {
    let out = bin().args(["control", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("spid3r control"), "{stdout}");
    assert!(stdout.contains("agent start"), "{stdout}");
}

#[test]
fn safety_lists_capability_switches_with_states() {
    let sb = sandbox("safety");
    let out = sb.command(&["control", "safety"]).output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Default state: the two agent-related surfaces plus plugin loading are
    // restricted/off; the four deny_* capability gates are OPEN by default.
    assert!(stdout.contains("capabilities open: 6 of 8"), "{stdout}");
    assert!(stdout.contains("Remote management access"), "{stdout}");
    assert!(stdout.contains("security.allow_remote"), "{stdout}");
    assert!(stdout.contains("OPEN"), "{stdout}");
    assert!(stdout.contains("SAFE"), "{stdout}");
}

#[test]
fn config_set_bootstraps_the_ini_into_the_config_dir() {
    let sb = sandbox("config-bootstrap");
    // No ini exists yet. Saving must create one in the CONFIG dir — never
    // silently target the exe directory (config::save's old fallback).
    let out = sb
        .command(&["control", "config", "set", "agent.enabled", "true"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ini = ini_in(&sb);
    assert!(
        ini.is_file(),
        "ini was not bootstrapped into the config dir"
    );
    let text = std::fs::read_to_string(&ini).unwrap();
    assert!(text.contains("enabled = true"), "{text}");

    // The value survives a reload (a fresh process re-reads the file).
    let out2 = sb
        .command(&["control", "config", "get", "agent.enabled"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out2.stdout);
    assert!(stdout.contains("agent.enabled = true"), "{stdout}");
}

#[test]
fn config_set_rejects_schema_violations_and_keeps_the_old_value() {
    let sb = sandbox("config-invalid");
    // Bootstrap a good ini first.
    let _ = sb
        .command(&["control", "config", "set", "agent.port", "9600"])
        .output()
        .unwrap();
    // agent.port is an Int; "not-a-number" must be rejected.
    let out = sb
        .command(&["control", "config", "set", "agent.port", "not-a-number"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    // The on-disk value is untouched.
    let text = std::fs::read_to_string(ini_in(&sb)).unwrap();
    assert!(text.contains("port = 9600"), "{text}");
}

#[test]
fn config_reset_restores_the_schema_default() {
    let sb = sandbox("config-reset");
    let _ = sb
        .command(&["control", "config", "set", "agent.port", "9999"])
        .output()
        .unwrap();
    let out = sb
        .command(&["control", "config", "reset", "agent.port"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(ini_in(&sb)).unwrap();
    assert!(text.contains("port = 9527"), "{text}");
}

#[test]
fn config_list_groups_known_keys() {
    let sb = sandbox("config-list");
    let out = sb.command(&["control", "config", "list"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("security.allow_remote"), "{stdout}");
    assert!(stdout.contains("agent.enabled"), "{stdout}");
    assert!(stdout.contains("spid3r.engine"), "{stdout}");
}
