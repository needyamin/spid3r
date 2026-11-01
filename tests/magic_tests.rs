use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    for key in ["CARGO_BIN_EXE_spid3r", "CARGO_BIN_EXE_spidr"] {
        if let Ok(p) = std::env::var(key) {
            let p = PathBuf::from(p);
            if p.is_file() {
                return p;
            }
        }
    }
    let exe = std::env::current_exe().expect("test executable path");
    let profile_dir = exe
        .parent()
        .and_then(|deps| deps.parent())
        .expect("profile directory");
    for stem in ["spid3r", "spidr"] {
        let cand = profile_dir.join(format!("{}{}", stem, std::env::consts::EXE_SUFFIX));
        if cand.is_file() {
            return cand;
        }
    }
    panic!("spid3r binary not found next to {}", profile_dir.display());
}

#[test]
fn magic_outside_project_reports_usage_error() {
    let tmp = std::env::temp_dir();
    let o = Command::new(bin())
        .arg("magic")
        .current_dir(&tmp)
        .output()
        .expect("spawn spid3r");
    assert_ne!(o.status.code(), Some(0));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(
        err.contains("no 'magic' file found"),
        "expected error about missing magic file, got: {}",
        err
    );
}

#[test]
fn magic_help_and_subcommand_dispatch() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let web_dir = manifest_dir.join("examples").join("spid3r_web");

    // 1. spid3r magic help
    let o = Command::new(bin())
        .args(["magic", "help"])
        .current_dir(&web_dir)
        .output()
        .expect("spawn spid3r");
    assert_eq!(o.status.code(), Some(0));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("Spid3rWeb Magic CLI"));

    // 2. spid3r magic route:list
    let o = Command::new(bin())
        .args(["magic", "route:list"])
        .current_dir(&web_dir)
        .output()
        .expect("spawn spid3r");
    assert_eq!(o.status.code(), Some(0));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("/api/v1/posts"));

    // 3. spid3r magic --engine=vm help
    let o = Command::new(bin())
        .args(["magic", "--engine=vm", "help"])
        .current_dir(&web_dir)
        .output()
        .expect("spawn spid3r");
    assert_eq!(o.status.code(), Some(0));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("Spid3rWeb Magic CLI"));

    // 4. Invocation from subdirectory (app/) resolves project root and dispatches routes
    let app_dir = web_dir.join("app");
    let o = Command::new(bin())
        .args(["magic", "route:list"])
        .current_dir(&app_dir)
        .output()
        .expect("spawn spid3r");
    assert_eq!(o.status.code(), Some(0));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("/api/v1/posts"));
}
