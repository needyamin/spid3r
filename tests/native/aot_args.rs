//! `args()` in a natively built executable: the process argv after the
//! program name reaches the script exactly like `spid3r run app.matrix one
//! two` delivers it. Regression test for the Phase-1 fix in
//! `spid3r_rt_start` (built executables never installed argv).

#![cfg(feature = "native")]

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

#[test]
fn aot_built_executable_receives_argv() {
    let dir = std::env::temp_dir().join(format!("spid3r_aot_args_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let src = dir.join("app.matrix");
    std::fs::write(
        &src,
        "fn main() {\n    let a = args();\n    println(string(a.len()));\n    println(a[0] + \":\" + a[1]);\n    return 0;\n}\n",
    )
    .expect("write source");

    let exe = dir.join("app.exe");
    let b = bin();
    let build = Command::new(&b)
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .current_dir(&dir)
        .output()
        .expect("spawn spid3r build");
    assert_eq!(
        build.status.code(),
        Some(0),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let run = Command::new(&exe)
        .args(["one", "two"])
        .current_dir(&dir)
        .output()
        .expect("run the built binary");
    assert_eq!(run.status.code(), Some(0));
    let out = String::from_utf8_lossy(&run.stdout);
    assert_eq!(
        out.trim_end(),
        "2\none:two",
        "argv must reach args() in a built executable; stdout was {out:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
