use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_spid3r")
}

fn scratch(tag: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "spid3r_phase5_{}_{}_{}",
        tag,
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create scratch directory");
    dir
}

fn run_in(dir: &Path, ini: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(dir)
        .env("SPID3R_INI", ini)
        .env("SPID3R_CONFIG_DIR", dir.join("config"))
        .env("SPID3R_STATE_DIR", dir.join("state"))
        .output()
        .expect("spawn spid3r")
}

fn write_ini(dir: &Path, text: &str) -> PathBuf {
    let path = dir.join("spid3r.ini");
    std::fs::write(&path, text).expect("write ini");
    path
}

#[test]
fn config_validate_reports_broken_pin_and_rejects_options() {
    let dir = scratch("validate-pin");
    let missing = dir.join("absent.ini");
    let out = run_in(&dir, &missing, &["config", "validate"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("SPID3R_INI"), "{err}");
    assert!(err.contains("absent.ini"), "{err}");

    let out = run_in(&dir, &missing, &["config", "validate", "--strict"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("usage"), "{err}");

    let valid = write_ini(&dir, "; valid isolated config\n");
    let out = run_in(&dir, &valid, &["config", "validate"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("ok"));
}

#[test]
fn broken_pin_blocks_program_commands_but_keeps_diagnostics_available() {
    let dir = scratch("execution-pin");
    let other_dir = dir.join("config");
    std::fs::create_dir_all(&other_dir).expect("create alternate config directory");
    write_ini(&other_dir, "[agent]\nname = alternate-config\n");
    let missing = dir.join("missing-pin.ini");

    let out = run_in(&dir, &missing, &["run", "does-not-matter.matrix"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("SPID3R_INI"));

    let out = run_in(&dir, &missing, &["exec", "does-not-matter.matrixc"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("SPID3R_INI"));

    let out = run_in(&dir, &missing, &["--version"]);
    assert_eq!(out.status.code(), Some(0));

    let out = run_in(&dir, &missing, &["config", "validate"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("missing-pin.ini"));

    let out = run_in(&dir, &missing, &["config", "get", "agent.name"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("alternate-config"));

    let out = run_in(&dir, &missing, &["control", "config", "validate"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("missing-pin.ini"));

    let out = run_in(&dir, &missing, &["info", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("missing-pin.ini"));

    let out = run_in(&dir, &missing, &["control", "status"]);
    assert_eq!(out.status.code(), Some(0));
    let status = String::from_utf8_lossy(&out.stdout);
    assert!(status.contains("no spid3r.ini yet"), "{status}");
    assert!(status.contains("INVALID: SPID3R_INI"), "{status}");
    assert!(status.contains("missing-pin.ini"), "{status}");

    let out = run_in(&dir, &missing, &["config", "init"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(missing.is_file());
    assert!(String::from_utf8_lossy(&out.stderr).contains("warning"));
}

#[test]
fn control_config_validate_returns_nonzero_for_invalid_config() {
    let dir = scratch("control-validate");
    let invalid = write_ini(&dir, "[agent]\nport = banana\n");
    let out = run_in(&dir, &invalid, &["control", "config", "validate"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("agent.port"), "{err}");

    let valid = write_ini(&dir, "; valid isolated config\n");
    let out = run_in(&dir, &valid, &["control", "config", "validate"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("configuration valid"));
}

#[test]
fn embedded_engine_refuses_execution_under_a_broken_pin() {
    let dir = scratch("embed-pin");
    let missing = dir.join("missing.ini");
    let previous = std::env::var_os("SPID3R_INI");
    std::env::set_var("SPID3R_INI", &missing);

    let result =
        spid3r_lang::embed::Engine::new().run("embedded.matrix", "fn main() { return 0; }");

    if let Some(value) = previous {
        std::env::set_var("SPID3R_INI", value);
    } else {
        std::env::remove_var("SPID3R_INI");
    }
    let error = result.expect_err("broken pin must block embedded execution");
    assert!(error.to_string().contains("SPID3R_INI"), "{error}");
}

#[test]
fn config_init_help_is_help_and_init_requires_force_to_overwrite() {
    let dir = scratch("config-init");
    let valid = write_ini(&dir, "; initial ini\n");
    let out = run_in(&dir, &valid, &["config", "init", "--help"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("USAGE:"));
    assert!(!dir.join("--help").exists());

    let target = dir.join("existing.ini");
    std::fs::write(&target, "; preserve this\n").expect("write existing ini");
    let target_text = std::fs::read_to_string(&target).unwrap();
    let out = run_in(&dir, &valid, &["config", "init", target.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), target_text);

    let out = run_in(
        &dir,
        &valid,
        &["config", "init", target.to_str().unwrap(), "--force"],
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(std::fs::read_to_string(&target)
        .unwrap()
        .contains("[spid3r]"));
}

/// The `spid3r.engine` config key must actually select the engine.
///
/// The key is documented as "Default engine for `spid3r run` when --engine is
/// omitted" and the schema's own default is `vm`, but `cmd_run` never read it:
/// the CLI used its built-in default regardless, so an INI `engine = tree`
/// was silently ignored. The observable used here is `--prof`, which is a
/// usage error on the tree engine (exit 2) and a working run on the VM
/// (exit 0) — deterministic, no timing involved.
#[test]
fn config_engine_key_selects_the_run_engine() {
    let dir = scratch("config-engine");
    let prog = dir.join("loop.matrix");
    std::fs::write(&prog, "fn main() {\n    println(7);\n    return 0;\n}\n").expect("write prog");

    // No INI: the built-in default (VM) runs --prof fine.
    let none = dir.join("none.ini");
    std::fs::write(&none, "").expect("write ini");
    let out = run_in(&dir, &none, &["run", "--prof", prog.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "unconfigured --prof must run on the VM"
    );

    // INI `engine = tree`: --prof becomes the tree-engine usage error, which
    // proves the config key is consumed.
    let tree = dir.join("tree.ini");
    std::fs::write(&tree, "[spid3r]\nengine = tree\n").expect("write ini");
    let out = run_in(&dir, &tree, &["run", "--prof", prog.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "config engine = tree must be honored: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--prof is a VM-engine feature"), "{err}");

    // An explicit flag overrides the INI: --engine vm with engine = tree.
    let out = run_in(
        &dir,
        &tree,
        &["run", "--engine", "vm", "--prof", prog.to_str().unwrap()],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "explicit --engine must override the INI"
    );

    // And INI `engine = vm` runs --prof fine, matching the schema default.
    let vm = dir.join("vm.ini");
    std::fs::write(&vm, "[spid3r]\nengine = vm\n").expect("write ini");
    let out = run_in(&dir, &vm, &["run", "--prof", prog.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
}
