//! End-to-end integration tests for the additive component CLI tools:
//! `yamc`, `yamcvm`, `spvm`, and `spkg` (both dedicated Cargo binaries and
//! multicall `argv[0]` dispatch via `spid3r` copies/symlinks).

use std::path::PathBuf;
use std::process::Command;

fn spid3r_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn yamc_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_yamc"))
}

fn yamcvm_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_yamcvm"))
}

fn spvm_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spvm"))
}

fn spkg_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spkg"))
}

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "spid3r_comp_cli_{}_{}_{}",
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

#[test]
fn component_clis_version_help_no_args_and_unknown_command() {
    let cases = [
        (yamc_bin(), "yamc", "YamLLVM"),
        (yamcvm_bin(), "yamcvm", "bytecode"),
        (spvm_bin(), "spvm", "spvm_rt"),
        (spkg_bin(), "spkg", "package manager"),
    ];
    for (bin, name, marker) in cases {
        let ver = Command::new(&bin).arg("--version").output().unwrap();
        assert!(ver.status.success(), "{name} --version failed");
        let ver_out = String::from_utf8_lossy(&ver.stdout);
        assert!(
            ver_out.contains(name) && ver_out.contains(marker),
            "{name} --version unexpected output: {ver_out}"
        );

        let help = Command::new(&bin).arg("--help").output().unwrap();
        assert!(help.status.success(), "{name} --help failed");
        let help_out = String::from_utf8_lossy(&help.stdout);
        assert!(
            help_out.contains(name) && help_out.contains("USAGE:"),
            "{name} --help unexpected output: {help_out}"
        );

        let no_args = Command::new(&bin).output().unwrap();
        assert_eq!(
            no_args.status.code(),
            Some(2),
            "{name} with no args must exit 2"
        );
        assert!(
            String::from_utf8_lossy(&no_args.stderr).contains("USAGE:"),
            "{name} no-args stderr missing USAGE"
        );

        let bogus = Command::new(&bin).arg("not-a-command").output().unwrap();
        assert_eq!(
            bogus.status.code(),
            Some(2),
            "{name} unknown command must exit 2"
        );
        let bogus_err = String::from_utf8_lossy(&bogus.stderr);
        assert!(
            bogus_err.contains("error[usage]:"),
            "{name} unknown command stderr missing error[usage]: {bogus_err}"
        );
    }

    // `help <subcommand>` prints subcommand help for valid commands and exits 2 for disallowed ones.
    for (bin, sub, expected) in [
        (yamc_bin(), "build", "spid3r build"),
        (yamcvm_bin(), "compile", "spid3r compile"),
        (spvm_bin(), "exec", "spid3r exec"),
        (spkg_bin(), "init", "spid3r init"),
    ] {
        let out = Command::new(&bin).args(["help", sub]).output().unwrap();
        assert!(
            out.status.success(),
            "help {sub} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let txt = String::from_utf8_lossy(&out.stdout);
        assert!(
            txt.contains(expected),
            "help {sub} missing '{expected}': {txt}"
        );
    }

    let bad_help = Command::new(spkg_bin())
        .args(["help", "build"])
        .output()
        .unwrap();
    assert_eq!(
        bad_help.status.code(),
        Some(2),
        "spkg help build must exit 2"
    );
    assert!(
        String::from_utf8_lossy(&bad_help.stderr).contains("error[usage]:"),
        "spkg help build missing error[usage]"
    );

    let extra_help = Command::new(yamc_bin())
        .args(["--help", "extra"])
        .output()
        .unwrap();
    assert!(extra_help.status.success(), "yamc --help extra must exit 0");
    assert!(String::from_utf8_lossy(&extra_help.stdout).contains("USAGE:"));
}

#[test]
fn yamcvm_and_spvm_bytecode_and_source_workflows() {
    let dir = scratch_dir("yamcvm_spvm");
    let src = dir.join("hello.mt");
    let bc = dir.join("out.matrixc");
    std::fs::write(
        &src,
        "fn main() -> int {\n    let a = args();\n    println(\"sum=\" + string(19 + 23) + \":\" + string(a.len()));\n    return 0;\n}\n",
    )
    .unwrap();

    // Shorthand bytecode compilation via `yamcvm <file.mt> -o <out.matrixc>`
    let comp = Command::new(yamcvm_bin())
        .arg(&src)
        .args(["-o"])
        .arg(&bc)
        .output()
        .unwrap();
    assert!(
        comp.status.success(),
        "yamcvm shorthand compile failed: {}",
        String::from_utf8_lossy(&comp.stderr)
    );
    assert!(bc.is_file(), "expected bytecode at {}", bc.display());

    // Shorthand execution via `spvm <file.matrixc>` and explicit `spvm exec`
    for args_list in [
        vec![bc.to_string_lossy().into_owned()],
        vec!["exec".to_string(), bc.to_string_lossy().into_owned()],
        vec!["--sandbox".to_string(), bc.to_string_lossy().into_owned()],
        vec![bc.to_string_lossy().into_owned(), "--sandbox".to_string()],
    ] {
        let out = Command::new(spvm_bin()).args(&args_list).output().unwrap();
        assert!(
            out.status.success(),
            "spvm {:?} failed: {}",
            args_list,
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "sum=42:0");
    }

    // Disassembly via both `yamcvm disasm` and `spvm disasm`
    for bin in [yamcvm_bin(), spvm_bin()] {
        let dis = Command::new(&bin)
            .args(["disasm"])
            .arg(&bc)
            .output()
            .unwrap();
        assert!(
            dis.status.success(),
            "disasm failed: {}",
            String::from_utf8_lossy(&dis.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&dis.stdout).trim().is_empty(),
            "disasm stdout should be non-empty"
        );
    }

    // Shorthand source run via `spvm <file.mt> arg1 arg2` and `--engine tree`
    let run_args = Command::new(spvm_bin())
        .arg(&src)
        .args(["alpha", "beta"])
        .output()
        .unwrap();
    assert!(
        run_args.status.success(),
        "spvm source run failed: {}",
        String::from_utf8_lossy(&run_args.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run_args.stdout).trim(), "sum=42:2");

    let run_tree = Command::new(spvm_bin())
        .args(["--engine", "tree"])
        .arg(&src)
        .output()
        .unwrap();
    assert!(
        run_tree.status.success(),
        "spvm --engine tree failed: {}",
        String::from_utf8_lossy(&run_tree.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run_tree.stdout).trim(), "sum=42:0");

    let run_tree_eq = Command::new(spvm_bin())
        .arg("--engine=tree")
        .arg(&src)
        .output()
        .unwrap();
    assert!(
        run_tree_eq.status.success(),
        "spvm --engine=tree failed: {}",
        String::from_utf8_lossy(&run_tree_eq.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run_tree_eq.stdout).trim(),
        "sum=42:0"
    );

    // Corrupt `.matrixc` must exit 4 (SHA-256 tamper protection preserved)
    let mut tampered = std::fs::read(&bc).unwrap();
    let last = tampered.len() - 1;
    tampered[last] ^= 0xFF;
    let bad_bc = dir.join("tampered.matrixc");
    std::fs::write(&bad_bc, tampered).unwrap();
    let bad_exec = Command::new(spvm_bin()).arg(&bad_bc).output().unwrap();
    assert_eq!(
        bad_exec.status.code(),
        Some(4),
        "corrupt .matrixc must exit 4 via spvm"
    );

    // `yamcvm --json` on invalid source emits JSON diagnostic on stdout and exits 1
    let bad_src = dir.join("broken.mt");
    std::fs::write(&bad_src, "fn main() { let x = ; }\n").unwrap();
    let j = Command::new(yamcvm_bin())
        .args(["--json"])
        .arg(&bad_src)
        .output()
        .unwrap();
    assert_eq!(j.status.code(), Some(1));
    let j_out = String::from_utf8_lossy(&j.stdout);
    assert!(
        j_out.trim_start().starts_with('{') && j_out.contains("\"severity\""),
        "expected JSON diagnostic from yamcvm --json on stdout, got: {j_out}"
    );

    // `spvm --sandbox` denies filesystem write
    let io_src = dir.join("io_write.mt");
    let target_file = dir.join("blocked.txt");
    std::fs::write(
        &io_src,
        format!(
            "import \"fs\";\nfn main() -> int {{\n    let _ = fs.write_file({:?}, \"nope\");\n    return 0;\n}}\n",
            target_file.to_string_lossy()
        ),
    )
    .unwrap();
    let sb = Command::new(spvm_bin())
        .args(["--sandbox"])
        .arg(&io_src)
        .output()
        .unwrap();
    assert!(
        !target_file.exists(),
        "spvm --sandbox must not allow creating {}",
        target_file.display()
    );
    assert!(
        !sb.status.success() || !target_file.exists(),
        "spvm --sandbox unexpectedly wrote file"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spkg_project_lifecycle_works_end_to_end() {
    let dir = scratch_dir("spkg_proj");
    let proj = dir.join("demo_pkg");

    let init = Command::new(spkg_bin())
        .args(["init"])
        .arg(&proj)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "spkg init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    assert!(proj.join("spider.toml").is_file());
    assert!(proj.join("src").join("main.matrix").is_file());

    let tree = Command::new(spkg_bin())
        .arg("tree")
        .current_dir(&proj)
        .output()
        .unwrap();
    assert!(
        tree.status.success(),
        "spkg tree failed: {}",
        String::from_utf8_lossy(&tree.stderr)
    );

    let dist = Command::new(spkg_bin())
        .arg("dist")
        .current_dir(&proj)
        .output()
        .unwrap();
    assert!(
        dist.status.success(),
        "spkg dist failed: {}",
        String::from_utf8_lossy(&dist.stderr)
    );

    let tar_out = dir.join("demo_pkg.tar");
    let pack = Command::new(spkg_bin())
        .args(["pack", "-o"])
        .arg(&tar_out)
        .current_dir(&proj)
        .output()
        .unwrap();
    assert!(
        pack.status.success(),
        "spkg pack failed: {}",
        String::from_utf8_lossy(&pack.stderr)
    );
    assert!(tar_out.is_file());

    let unpacked = dir.join("unpacked");
    let unpack = Command::new(spkg_bin())
        .args(["unpack"])
        .arg(&tar_out)
        .arg(&unpacked)
        .output()
        .unwrap();
    assert!(
        unpack.status.success(),
        "spkg unpack failed: {}",
        String::from_utf8_lossy(&unpack.stderr)
    );
    assert!(unpacked.join("spider.toml").is_file());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn yamc_check_compile_and_tokens_work() {
    let dir = scratch_dir("yamc_front");
    let src = dir.join("ok.mt");
    let bc = dir.join("ok.matrixc");
    std::fs::write(
        &src,
        "fn main() -> int {\n    println(\"yamc ok\");\n    return 0;\n}\n",
    )
    .unwrap();

    let chk = Command::new(yamc_bin())
        .args(["check"])
        .arg(&src)
        .output()
        .unwrap();
    assert!(
        chk.status.success(),
        "yamc check failed: {}",
        String::from_utf8_lossy(&chk.stderr)
    );

    let comp = Command::new(yamc_bin())
        .args(["compile"])
        .arg(&src)
        .args(["-o"])
        .arg(&bc)
        .output()
        .unwrap();
    assert!(
        comp.status.success(),
        "yamc compile failed: {}",
        String::from_utf8_lossy(&comp.stderr)
    );
    assert!(bc.is_file());

    let toks = Command::new(yamc_bin())
        .args(["tokens"])
        .arg(&src)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        toks.status.success(),
        "yamc tokens failed: {}",
        String::from_utf8_lossy(&toks.stderr)
    );
    let toks_out = String::from_utf8_lossy(&toks.stdout);
    assert!(
        toks_out.trim_start().starts_with('{') && toks_out.contains("\"tokens\""),
        "expected tokens JSON object, got: {toks_out}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "native")]
#[test]
fn yamc_shorthand_native_aot_build_runs_executable() {
    let dir = scratch_dir("yamc_aot");
    let src = dir.join("aot_hello.mt");
    let exe_out = dir.join(if cfg!(windows) {
        "aot_hello.exe"
    } else {
        "aot_hello"
    });
    std::fs::write(
        &src,
        "fn main() -> int {\n    println(\"hello from yamc\");\n    return 0;\n}\n",
    )
    .unwrap();

    // Shorthand: `yamc <file.mt> -o <exe_out>` invokes native AOT build directly.
    let built = Command::new(yamc_bin())
        .arg(&src)
        .args(["-o"])
        .arg(&exe_out)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "yamc shorthand build failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    assert!(
        exe_out.is_file(),
        "expected native binary at {}",
        exe_out.display()
    );

    let run = Command::new(&exe_out).output().unwrap();
    assert!(
        run.status.success(),
        "native binary failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "hello from yamc"
    );

    // `yamc --version 2.0.0 <file.mt> -o <exe>` must invoke `build --version`,
    // not print yamc's own version banner.
    let exe_v2 = dir.join(if cfg!(windows) {
        "aot_v2.exe"
    } else {
        "aot_v2"
    });
    let built_v2 = Command::new(yamc_bin())
        .args(["--version", "2.0.0"])
        .arg(&src)
        .args(["-o"])
        .arg(&exe_v2)
        .output()
        .unwrap();
    assert!(
        built_v2.status.success(),
        "yamc --version 2.0.0 build failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&built_v2.stdout),
        String::from_utf8_lossy(&built_v2.stderr)
    );
    assert!(
        exe_v2.is_file(),
        "expected native binary at {}",
        exe_v2.display()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn multicall_argv0_dispatch_matches_dedicated_binaries() {
    let dir = scratch_dir("multicall");
    let spid3r = spid3r_bin();
    let suffix = if cfg!(windows) { ".exe" } else { "" };

    // Copy spid3r.exe (simulating NSIS File "/oname=..." on Windows) or symlink on Unix.
    let make_alias = |name: &str| -> PathBuf {
        let dst = dir.join(format!("{name}{suffix}"));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&spid3r, &dst).unwrap();
        }
        #[cfg(not(unix))]
        {
            std::fs::copy(&spid3r, &dst).unwrap();
        }
        dst
    };

    // On Windows, delay-loaded LLVM-C.dll is not needed for non-build commands,
    // so running copied spid3r.exe from a scratch dir works with zero extra DLLs.
    let yamc = make_alias("yamc");
    let yamcvm = make_alias("yamcvm");
    let spvm = make_alias("spvm");
    let spkg = make_alias("spkg");

    for (exe, expected) in [
        (&yamc, "yamc"),
        (&yamcvm, "yamcvm"),
        (&spvm, "spvm"),
        (&spkg, "spkg"),
    ] {
        let out = Command::new(exe).arg("--version").output().unwrap();
        assert!(
            out.status.success(),
            "multicall {expected} --version failed"
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains(expected),
            "multicall {expected} --version mismatch: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    let src = dir.join("mc.mt");
    let bc = dir.join("mc.matrixc");
    std::fs::write(
        &src,
        "fn main() -> int { println(\"multicall\"); return 0; }\n",
    )
    .unwrap();

    assert!(Command::new(&yamc)
        .args(["check"])
        .arg(&src)
        .status()
        .unwrap()
        .success());
    assert!(Command::new(&yamcvm)
        .arg(&src)
        .args(["-o"])
        .arg(&bc)
        .status()
        .unwrap()
        .success());
    let run = Command::new(&spvm).arg(&bc).output().unwrap();
    assert!(run.status.success());
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "multicall");

    let proj = dir.join("mc_pkg");
    assert!(Command::new(&spkg)
        .args(["init"])
        .arg(&proj)
        .status()
        .unwrap()
        .success());
    assert!(proj.join("spider.toml").is_file());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn broken_spid3r_ini_refuses_execution_across_component_clis() {
    let dir = scratch_dir("broken_ini");
    let missing_ini = dir.join("does_not_exist.ini");
    let src = dir.join("ok.mt");
    std::fs::write(&src, "fn main() -> int { return 0; }\n").unwrap();

    for (bin, args) in [
        (yamc_bin(), vec!["check", src.to_str().unwrap()]),
        (yamcvm_bin(), vec![src.to_str().unwrap()]),
        (spvm_bin(), vec![src.to_str().unwrap()]),
        (spkg_bin(), vec!["verify"]),
    ] {
        let out = Command::new(&bin)
            .args(&args)
            .env("SPID3R_INI", &missing_ini)
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(1),
            "expected exit 1 on broken SPID3R_INI for {:?} {:?}",
            bin,
            args
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("error[config]:"),
            "expected error[config] on stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn spkg_tool_install_shim_executes_via_spid3r_runner() {
    let dir = scratch_dir("spkg_tool_shim");
    let pkg_dir = dir.join("sayhi");
    let state = dir.join("state");
    let ini = dir.join("empty.ini");
    std::fs::create_dir_all(pkg_dir.join("src")).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(&ini, "").unwrap();
    std::fs::write(
        pkg_dir.join("spider.toml"),
        "[package]\nname = \"sayhi\"\nversion = \"0.1.0\"\nentry = \"src/cli.matrix\"\n\n[bin]\nsayhi = \"src/cli.matrix\"\n",
    )
    .unwrap();
    std::fs::write(
        pkg_dir.join("src/cli.matrix"),
        "fn main() {\n    let a = args();\n    let who = if a.len() > 0 { a[0] } else { \"none\" };\n    println(\"hi \" + who);\n    return 0;\n}\n",
    )
    .unwrap();

    // Ensure sibling spid3r binary is materialized for Cargo test runner too.
    let _spid3r = spid3r_bin();
    let spkg = spkg_bin();
    let inst = Command::new(&spkg)
        .args([
            "tool",
            "install",
            "sayhi",
            "--path",
            pkg_dir.to_str().unwrap(),
        ])
        .env("SPID3R_STATE_DIR", &state)
        .env("SPID3R_INI", &ini)
        .output()
        .unwrap();
    assert!(
        inst.status.success(),
        "spkg tool install failed: out={} err={}",
        String::from_utf8_lossy(&inst.stdout),
        String::from_utf8_lossy(&inst.stderr)
    );

    let shim = if cfg!(windows) {
        state.join("bin").join("sayhi.cmd")
    } else {
        state.join("bin").join("sayhi")
    };
    assert!(shim.is_file(), "missing shim {}", shim.display());
    let run = Command::new(&shim)
        .arg("world")
        .env("SPID3R_INI", &ini)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "shim failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "hi world");

    let run_help = Command::new(&shim)
        .arg("--help")
        .env("SPID3R_INI", &ini)
        .output()
        .unwrap();
    assert!(run_help.status.success());
    assert_eq!(
        String::from_utf8_lossy(&run_help.stdout).trim(),
        "hi --help"
    );

    let un = Command::new(&spkg)
        .args(["tool", "uninstall", "sayhi"])
        .env("SPID3R_STATE_DIR", &state)
        .env("SPID3R_INI", &ini)
        .output()
        .unwrap();
    assert!(un.status.success());
    assert!(!shim.exists());

    let _ = std::fs::remove_dir_all(&dir);
}
