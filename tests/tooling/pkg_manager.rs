//! SPM: manifest/lock compatibility, the add → verify → publish cycle, and
//! integrity.
//!
//! The plan's P3 exit criteria are the shape of this file: a full
//! add/remove/verify cycle in a temp dir with **no network**, a v1 manifest
//! that still installs, and a tampered vendored file that makes `spidr verify`
//! exit 1.

use std::path::{Path, PathBuf};
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

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r_pkg_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).expect("mkdir parent");
    }
    std::fs::write(&p, body).expect("write");
}

fn run_in(dir: &Path, args: &[&str]) -> (i32, String, String) {
    run_env(dir, args, &[])
}

fn run_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(bin());
    cmd.args(args).current_dir(dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let o = cmd.output().expect("spawn spidr");
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

/// A package with a manifest and a `src/lib.matrix`.
fn make_package(root: &Path, name: &str, version: &str, body: &str) {
    write(
        root,
        "spider.toml",
        &format!(
            "[package]\nname = \"{}\"\nversion = \"{}\"\n",
            name, version
        ),
    );
    write(root, "src/lib.matrix", body);
}

// ----------------------------------------------------------------- manifest

/// A v1 manifest — no `description`, no `license`, nothing but the three
/// original keys — must keep parsing. This is the R4 backward-compatibility
/// gate: existing projects must not need an edit to keep building.
#[test]
fn a_v1_manifest_still_parses_and_installs() {
    let dir = scratch("v1");
    make_package(
        &dir.join("mathlib"),
        "mathlib",
        "0.1.0",
        "public fn add(a, b) {\n    return a + b;\n}\n",
    );
    write(
        &dir,
        "app/spider.toml",
        "[package]\nname = \"app\"\nversion = \"1.0.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nmathlib = { path = \"../mathlib\" }\n",
    );
    write(
        &dir,
        "app/src/main.matrix",
        "import \"mathlib/lib\";\nfn main() {\n    println(add(2, 3));\n    return 0;\n}\n",
    );

    let app = dir.join("app");
    let (code, out, err) = run_in(&app, &["install"]);
    assert_eq!(code, 0, "v1 manifest failed to install: {err}");
    assert!(!out.is_empty(), "install printed nothing");

    // And the program still runs against the vendored copy.
    let (code, out, err) = run_in(&app, &["run", "src/main.matrix"]);
    assert_eq!(code, 0, "run failed: {err}");
    assert_eq!(out.trim_end(), "5");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `spidr verify` on a clean tree succeeds and counts what it checked.
#[test]
fn verify_reports_a_clean_tree() {
    let dir = scratch("verify_ok");
    make_package(
        &dir.join("mathlib"),
        "mathlib",
        "0.1.0",
        "public fn f() {\n    return 1;\n}\n",
    );
    write(
        &dir,
        "app/spider.toml",
        "[package]\nname = \"app\"\nversion = \"1.0.0\"\n\n[dependencies]\nmathlib = { path = \"../mathlib\" }\n",
    );
    write(
        &dir,
        "app/src/main.matrix",
        "fn main() {\n    return 0;\n}\n",
    );
    let app = dir.join("app");
    assert_eq!(run_in(&app, &["install"]).0, 0);

    let (code, out, err) = run_in(&app, &["verify"]);
    assert_eq!(code, 0, "verify failed on a clean tree: {err}");
    assert!(out.contains("1 vendored package"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// **The integrity gate**: editing one vendored byte must make `verify` exit 1
/// and name the package and both hashes.
#[test]
fn verify_fails_with_exit_1_when_a_vendored_file_is_tampered_with() {
    let dir = scratch("verify_bad");
    make_package(
        &dir.join("mathlib"),
        "mathlib",
        "0.1.0",
        "public fn f() {\n    return 1;\n}\n",
    );
    write(
        &dir,
        "app/spider.toml",
        "[package]\nname = \"app\"\nversion = \"1.0.0\"\n\n[dependencies]\nmathlib = { path = \"../mathlib\" }\n",
    );
    write(
        &dir,
        "app/src/main.matrix",
        "fn main() {\n    return 0;\n}\n",
    );
    let app = dir.join("app");
    assert_eq!(run_in(&app, &["install"]).0, 0);

    // The vendored copy mirrors the dependency's src/.
    write(
        &app,
        "vendor/mathlib/lib.matrix",
        "public fn f() {\n    return 999;\n}\n",
    );

    let (code, _, err) = run_in(&app, &["verify"]);
    assert_eq!(code, 1, "tampering must fail verify: {err}");
    assert!(err.contains("mathlib"), "the package must be named: {err}");
    assert!(err.contains("expected"), "both hashes must be shown: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

// -------------------------------------------------------------- add/remove

/// `add` declares the dependency, vendors it, and is idempotent.
#[test]
fn add_declares_vendors_and_is_idempotent() {
    let dir = scratch("add");
    make_package(
        &dir.join("mathlib"),
        "mathlib",
        "0.1.0",
        "public fn f() {\n    return 1;\n}\n",
    );
    write(
        &dir,
        "app/spider.toml",
        "[package]\nname = \"app\"\nversion = \"1.0.0\"\n",
    );
    write(
        &dir,
        "app/src/main.matrix",
        "fn main() {\n    return 0;\n}\n",
    );
    let app = dir.join("app");

    let (code, out, err) = run_in(&app, &["add", "mathlib", "--path", "../mathlib"]);
    assert_eq!(code, 0, "add failed: {err}");
    assert!(out.contains("added mathlib"), "{out}");

    let manifest = std::fs::read_to_string(app.join("spider.toml")).unwrap();
    assert!(manifest.contains("[dependencies]"), "{manifest}");
    assert!(
        manifest.contains("mathlib = { path = \"../mathlib\" }"),
        "{manifest}"
    );
    assert!(
        app.join("vendor/mathlib/lib.matrix").is_file(),
        "not vendored"
    );

    // Second add: no duplicate line, and it says so.
    let (code, out, _) = run_in(&app, &["add", "mathlib", "--path", "../mathlib"]);
    assert_eq!(code, 0);
    assert!(out.contains("already declared"), "{out}");
    let again = std::fs::read_to_string(app.join("spider.toml")).unwrap();
    assert_eq!(manifest, again, "the manifest changed on a repeat add");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A dependency that does not resolve must be rolled back out of the manifest,
/// not left behind to break every later command.
#[test]
fn a_failed_add_leaves_the_manifest_unchanged() {
    let dir = scratch("add_fail");
    write(
        &dir,
        "app/spider.toml",
        "[package]\nname = \"app\"\nversion = \"1.0.0\"\n",
    );
    write(
        &dir,
        "app/src/main.matrix",
        "fn main() {\n    return 0;\n}\n",
    );
    let app = dir.join("app");
    let before = std::fs::read_to_string(app.join("spider.toml")).unwrap();

    let (code, _, err) = run_in(&app, &["add", "ghost", "--path", "../nowhere"]);
    assert_ne!(code, 0, "adding a missing path must fail");
    assert!(
        err.contains("ghost"),
        "the error must name the package: {err}"
    );
    let after = std::fs::read_to_string(app.join("spider.toml")).unwrap();
    assert_eq!(before, after, "the manifest was not rolled back:\n{after}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `remove` drops the declaration and the vendored copy, so a later `verify`
/// does not report an unlocked package.
#[test]
fn remove_drops_the_declaration_and_the_vendored_copy() {
    let dir = scratch("remove");
    make_package(
        &dir.join("mathlib"),
        "mathlib",
        "0.1.0",
        "public fn f() {\n    return 1;\n}\n",
    );
    write(
        &dir,
        "app/spider.toml",
        "[package]\nname = \"app\"\nversion = \"1.0.0\"\n",
    );
    write(
        &dir,
        "app/src/main.matrix",
        "fn main() {\n    return 0;\n}\n",
    );
    let app = dir.join("app");
    assert_eq!(
        run_in(&app, &["add", "mathlib", "--path", "../mathlib"]).0,
        0
    );

    let (code, out, err) = run_in(&app, &["remove", "mathlib"]);
    assert_eq!(code, 0, "remove failed: {err}");
    assert!(out.contains("removed mathlib"), "{out}");
    let manifest = std::fs::read_to_string(app.join("spider.toml")).unwrap();
    assert!(!manifest.contains("mathlib"), "{manifest}");
    assert!(!app.join("vendor/mathlib").exists(), "vendor copy survived");

    let (code, _, _) = run_in(&app, &["verify"]);
    assert_eq!(code, 0, "verify must be clean after a remove");
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------------- tree

/// `spidr tree` shows the declared dependency with its locked version and
/// origin.
#[test]
fn tree_prints_the_declared_dependencies() {
    let dir = scratch("tree");
    make_package(
        &dir.join("mathlib"),
        "mathlib",
        "0.1.0",
        "public fn f() {\n    return 1;\n}\n",
    );
    write(
        &dir,
        "app/spider.toml",
        "[package]\nname = \"app\"\nversion = \"1.0.0\"\n",
    );
    write(
        &dir,
        "app/src/main.matrix",
        "fn main() {\n    return 0;\n}\n",
    );
    let app = dir.join("app");
    assert_eq!(
        run_in(&app, &["add", "mathlib", "--path", "../mathlib"]).0,
        0
    );

    let (code, out, err) = run_in(&app, &["tree"]);
    assert_eq!(code, 0, "tree failed: {err}");
    assert!(out.starts_with("app 1.0.0"), "{out}");
    assert!(out.contains("mathlib 0.1.0"), "{out}");
    assert!(out.contains("../mathlib"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

// -------------------------------------------------------- publish → install

/// The plan's round trip: publish a package into a local registry directory,
/// then install it **by version** from that registry in a second project.
#[test]
fn publish_then_install_by_version_from_a_local_registry() {
    let dir = scratch("publish");
    make_package(
        &dir.join("mathlib"),
        "mathlib",
        "0.1.0",
        "public fn add(a, b) {\n    return a + b;\n}\n",
    );
    let lib = dir.join("mathlib");
    let registry = dir.join("registry");

    let (code, _, err) = run_in(&lib, &["publish", "--to-dir", registry.to_str().unwrap()]);
    assert_eq!(code, 0, "publish failed: {err}");
    assert!(
        registry.join("mathlib/0.1.0/spider.toml").is_file(),
        "registry layout is <dir>/<name>/<version>/"
    );
    assert!(
        registry.join("mathlib/0.1.0/sha256.txt").is_file(),
        "publish must write sha256.txt"
    );

    // A consumer resolves it by version, with no network.
    write(
        &dir,
        "app/spider.toml",
        "[package]\nname = \"app\"\nversion = \"1.0.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nmathlib = { version = \"^0.1.0\" }\n",
    );
    write(
        &dir,
        "app/src/main.matrix",
        "import \"mathlib/lib\";\nfn main() {\n    println(add(4, 5));\n    return 0;\n}\n",
    );
    let app = dir.join("app");
    let (code, _, err) = run_env(
        &app,
        &["install"],
        &[("SPID3R_REGISTRY", registry.to_str().unwrap())],
    );
    assert_eq!(code, 0, "install from the local registry failed: {err}");

    let (code, out, err) = run_env(
        &app,
        &["run", "src/main.matrix"],
        &[("SPID3R_REGISTRY", registry.to_str().unwrap())],
    );
    assert_eq!(code, 0, "run failed: {err}");
    assert_eq!(out.trim_end(), "9");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Publishing the same version twice is refused: a registry entry is
/// immutable, and silently overwriting one would make every existing
/// `spider.lock` a lie.
#[test]
fn publishing_an_existing_version_is_refused() {
    let dir = scratch("publish_twice");
    make_package(
        &dir.join("mathlib"),
        "mathlib",
        "0.1.0",
        "public fn f() {\n    return 1;\n}\n",
    );
    let lib = dir.join("mathlib");
    let registry = dir.join("registry");
    assert_eq!(
        run_in(&lib, &["publish", "--to-dir", registry.to_str().unwrap()]).0,
        0
    );
    let (code, _, err) = run_in(&lib, &["publish", "--to-dir", registry.to_str().unwrap()]);
    assert_ne!(code, 0, "the second publish must be refused");
    assert!(err.contains("already exists"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

mod r8_tests {
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread::JoinHandle;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn spid3r_bin() -> &'static str {
        env!("CARGO_BIN_EXE_spid3r")
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "spid3r_r8_{}_{}_{}",
            label,
            std::process::id(),
            nanos
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn run_spid3r(cwd: &Path, args: &[&str], envs: &[(&str, Option<&str>)]) -> Output {
        let mut cmd = Command::new(spid3r_bin());
        cmd.current_dir(cwd).args(args);
        cmd.env_remove("SPID3R_REGISTRY")
            .env_remove("SPID3R_PACKAGES_REGISTRY")
            .env_remove("SPID3R_TRUST_PK")
            .env_remove("SPID3R_PACKAGES_TRUST_PK")
            .env_remove("SPID3R_SIGN_KEY")
            .env_remove("SPID3R_PACKAGES_SIGN_KEY");
        for (k, v) in envs {
            match v {
                Some(val) => {
                    cmd.env(k, val);
                }
                None => {
                    cmd.env_remove(k);
                }
            }
        }
        cmd.output().expect("spid3r command runs")
    }

    struct LoopbackServer {
        port: u16,
        stop: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    impl Drop for LoopbackServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = TcpStream::connect(("127.0.0.1", self.port));
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    fn start_file_server(root: PathBuf) -> LoopbackServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = stop.clone();
        let handle = std::thread::spawn(move || {
            while !stop_bg.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if stop_bg.load(Ordering::SeqCst) {
                            break;
                        }
                        let mut buf = [0u8; 4096];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]);
                        let raw_path = req
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("/");
                        let rel = raw_path
                            .split('?')
                            .next()
                            .unwrap_or("/")
                            .trim_start_matches('/');
                        let mut candidate = root.clone();
                        let mut safe = true;
                        for comp in Path::new(rel).components() {
                            match comp {
                                std::path::Component::Normal(c) => candidate.push(c),
                                _ => {
                                    safe = false;
                                    break;
                                }
                            }
                        }
                        if safe && candidate.is_file() {
                            let body = std::fs::read(&candidate).unwrap_or_default();
                            let hdr = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(hdr.as_bytes());
                            let _ = stream.write_all(&body);
                        } else {
                            let body = b"not found";
                            let hdr = format!(
                                "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(hdr.as_bytes());
                            let _ = stream.write_all(body);
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        LoopbackServer {
            port,
            stop,
            handle: Some(handle),
        }
    }

    fn collect_tree_bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(base: &Path, cur: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            let mut entries: Vec<_> = std::fs::read_dir(cur)
                .unwrap()
                .map(|e| e.unwrap())
                .collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let p = entry.path();
                if p.is_dir() {
                    walk(base, &p, out);
                } else if p.is_file() {
                    let rel = p
                        .strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.insert(rel, std::fs::read(&p).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    fn raw_tar_with_entry(name: &str, data: &[u8]) -> Vec<u8> {
        let mut hdr = [0u8; 512];
        let name_bytes = name.as_bytes();
        hdr[..name_bytes.len()].copy_from_slice(name_bytes);
        hdr[100..108].copy_from_slice(b"0000644\0");
        hdr[108..116].copy_from_slice(b"0000000\0");
        hdr[116..124].copy_from_slice(b"0000000\0");
        let size_oct = format!("{:011o}\0", data.len());
        hdr[124..136].copy_from_slice(size_oct.as_bytes());
        hdr[136..148].copy_from_slice(b"00000000000\0");
        hdr[148..156].copy_from_slice(b"        ");
        hdr[156] = b'0';
        hdr[257..263].copy_from_slice(b"ustar\0");
        hdr[263..265].copy_from_slice(b"00");
        let sum: u32 = hdr.iter().map(|&b| b as u32).sum();
        let cksum = format!("{:06o}\0 ", sum);
        hdr[148..156].copy_from_slice(cksum.as_bytes());

        let mut tar = Vec::new();
        tar.extend_from_slice(&hdr);
        let pad = (512 - (data.len() % 512)) % 512;
        tar.extend_from_slice(data);
        tar.resize(tar.len() + pad, 0);
        tar.resize(tar.len() + 1024, 0);
        tar
    }

    #[test]
    fn r8_2_http_registry_install_matches_local_dir_and_rejects_traversal_and_offline() {
        let root = temp_dir("http_reg");
        let reg_dir = root.join("registry");
        let lib_dir = root.join("lib_src");
        write_file(
            &lib_dir.join("spider.toml"),
            "[package]\nname = \"greet\"\nversion = \"1.0.0\"\nentry = \"src/lib.matrix\"\n",
        );
        write_file(
            &lib_dir.join("src/lib.matrix"),
            "public fn hello() -> str { return \"hi from remote\"; }\n",
        );

        let pub_out = run_spid3r(
            &lib_dir,
            &["publish", "--to-dir", reg_dir.to_str().unwrap()],
            &[],
        );
        assert!(
            pub_out.status.success(),
            "publish failed: {}",
            String::from_utf8_lossy(&pub_out.stderr)
        );
        assert!(reg_dir.join("greet/1.0.0/package.tar").is_file());
        assert!(reg_dir.join("greet/index.txt").is_file());

        let proj_local = root.join("proj_local");
        write_file(
            &proj_local.join("spider.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\ngreet = { version = \"^1.0\" }\n",
        );
        write_file(
            &proj_local.join("src/main.matrix"),
            "import \"greet/lib\";\nprint(hello());\n",
        );
        let reg_str = reg_dir.to_str().unwrap();
        let loc_out = run_spid3r(
            &proj_local,
            &["install"],
            &[("SPID3R_REGISTRY", Some(reg_str))],
        );
        assert!(
            loc_out.status.success(),
            "local install failed: {}",
            String::from_utf8_lossy(&loc_out.stderr)
        );

        let srv = start_file_server(reg_dir.clone());
        let url = format!("http://127.0.0.1:{}", srv.port);

        let proj_remote = root.join("proj_remote");
        write_file(
            &proj_remote.join("spider.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\ngreet = { version = \"^1.0\" }\n",
        );
        write_file(
            &proj_remote.join("src/main.matrix"),
            "import \"greet/lib\";\nprint(hello());\n",
        );
        let rem_out = run_spid3r(
            &proj_remote,
            &["install"],
            &[("SPID3R_REGISTRY", Some(&url))],
        );
        assert!(
            rem_out.status.success(),
            "remote install failed: {}\nstdout: {}",
            String::from_utf8_lossy(&rem_out.stderr),
            String::from_utf8_lossy(&rem_out.stdout)
        );

        let local_tree = collect_tree_bytes(&proj_local.join("vendor/greet"));
        let remote_tree = collect_tree_bytes(&proj_remote.join("vendor/greet"));
        assert!(
            !local_tree.is_empty(),
            "vendor/greet must contain vendored files"
        );
        assert_eq!(
            local_tree, remote_tree,
            "remote HTTP install must match local directory install byte-for-byte"
        );

        let proj_offline = root.join("proj_offline");
        write_file(
            &proj_offline.join("spider.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\ngreet = { version = \"^1.0\" }\n",
        );
        write_file(&proj_offline.join("src/main.matrix"), "print(1);\n");
        let off_out = run_spid3r(
            &proj_offline,
            &["install"],
            &[("SPID3R_REGISTRY", Some("http://127.0.0.1:1"))],
        );
        assert!(
            !off_out.status.success(),
            "offline remote registry install must fail cleanly"
        );
        let off_err = String::from_utf8_lossy(&off_out.stderr);
        assert!(
            off_err.contains("error") && !off_err.contains("panicked"),
            "unexpected offline error: {}",
            off_err
        );

        // Path-traversal tarball served over HTTP must be refused cleanly.
        let evil_reg = root.join("evil_reg");
        write_file(&evil_reg.join("evilpkg/index.txt"), "1.0.0\n");
        write_file(&evil_reg.join("evilpkg/1.0.0/sha256.txt"), &"0".repeat(64));
        let evil_tar = raw_tar_with_entry("../escaped.txt", b"pwn");
        std::fs::create_dir_all(evil_reg.join("evilpkg/1.0.0")).unwrap();
        std::fs::write(evil_reg.join("evilpkg/1.0.0/package.tar"), evil_tar).unwrap();

        let evil_srv = start_file_server(evil_reg);
        let evil_url = format!("http://127.0.0.1:{}", evil_srv.port);
        let proj_evil = root.join("proj_evil/nested");
        write_file(
            &proj_evil.join("spider.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nevilpkg = { version = \"^1.0\" }\n",
        );
        write_file(&proj_evil.join("src/main.matrix"), "print(1);\n");
        let evil_out = run_spid3r(
            &proj_evil,
            &["install"],
            &[("SPID3R_REGISTRY", Some(&evil_url))],
        );
        assert!(!evil_out.status.success(), "traversal tar must be refused");
        assert!(
            !root.join("proj_evil/escaped.txt").exists() && !proj_evil.join("escaped.txt").exists(),
            "path traversal wrote a file outside extraction root"
        );

        drop(evil_srv);
        drop(srv);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn r8_3_ed25519_signed_registry_verify_tamper_and_trust_pk() {
        let root = temp_dir("ed25519_reg");
        let reg_dir = root.join("registry");
        let lib_dir = root.join("signed_src");
        write_file(
            &lib_dir.join("spider.toml"),
            "[package]\nname = \"signedlib\"\nversion = \"1.0.0\"\nentry = \"src/lib.matrix\"\n",
        );
        write_file(
            &lib_dir.join("src/lib.matrix"),
            "public fn val() -> int { return 7; }\n",
        );

        let pub_out = run_spid3r(
            &lib_dir,
            &["publish", "--to-dir", reg_dir.to_str().unwrap()],
            &[],
        );
        assert!(
            pub_out.status.success(),
            "publish failed: {}",
            String::from_utf8_lossy(&pub_out.stderr)
        );
        let sig_path = reg_dir.join("signedlib/1.0.0/sha256.txt.sig");
        let pubkey_path = reg_dir.join("registry.pub");
        assert!(sig_path.is_file(), "sha256.txt.sig missing");
        assert!(pubkey_path.is_file(), "registry.pub missing");
        assert!(
            reg_dir.join("registry.key").is_file(),
            "registry.key missing"
        );
        let pk_hex = std::fs::read_to_string(&pubkey_path)
            .unwrap()
            .trim()
            .to_string();
        assert_eq!(pk_hex.len(), 64);

        let reg_str = reg_dir.to_str().unwrap();

        // 1. Valid signature + matching trust_pk installs cleanly.
        let proj_ok = root.join("proj_ok");
        write_file(
            &proj_ok.join("spider.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nsignedlib = { version = \"^1.0\" }\n",
        );
        write_file(&proj_ok.join("src/main.matrix"), "print(1);\n");
        let ok_out = run_spid3r(
            &proj_ok,
            &["install"],
            &[
                ("SPID3R_REGISTRY", Some(reg_str)),
                ("SPID3R_PACKAGES_TRUST_PK", Some(&pk_hex)),
            ],
        );
        assert!(
            ok_out.status.success(),
            "valid signed install failed: {}",
            String::from_utf8_lossy(&ok_out.stderr)
        );
        assert!(proj_ok.join("vendor/signedlib/lib.matrix").is_file());

        // 2. Wrong trust_pk refuses install and leaves vendor/ empty.
        let proj_wrong_pk = root.join("proj_wrong_pk");
        write_file(
            &proj_wrong_pk.join("spider.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nsignedlib = { version = \"^1.0\" }\n",
        );
        write_file(&proj_wrong_pk.join("src/main.matrix"), "print(1);\n");
        let wrong_pk = "01".repeat(32);
        let wrong_out = run_spid3r(
            &proj_wrong_pk,
            &["install"],
            &[
                ("SPID3R_REGISTRY", Some(reg_str)),
                ("SPID3R_PACKAGES_TRUST_PK", Some(&wrong_pk)),
            ],
        );
        assert!(
            !wrong_out.status.success(),
            "wrong trust_pk must be refused"
        );
        assert!(!proj_wrong_pk.join("vendor/signedlib").exists());

        // 3. 1-byte tamper in sha256.txt.sig refuses install and leaves vendor/ empty.
        let orig_sig = std::fs::read(&sig_path).unwrap();
        let mut tampered_sig = orig_sig.clone();
        tampered_sig[0] ^= 0xFF;
        std::fs::write(&sig_path, &tampered_sig).unwrap();
        let proj_bad_sig = root.join("proj_bad_sig");
        write_file(
            &proj_bad_sig.join("spider.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nsignedlib = { version = \"^1.0\" }\n",
        );
        write_file(&proj_bad_sig.join("src/main.matrix"), "print(1);\n");
        let bad_sig_out = run_spid3r(
            &proj_bad_sig,
            &["install"],
            &[
                ("SPID3R_REGISTRY", Some(reg_str)),
                ("SPID3R_PACKAGES_TRUST_PK", Some(&pk_hex)),
            ],
        );
        assert!(!bad_sig_out.status.success(), "tampered sig must fail");
        assert!(!proj_bad_sig.join("vendor/signedlib").exists());

        // 4. Unsigned package: refused when trust_pk set; warns and installs when trust_pk unset.
        std::fs::remove_file(&sig_path).unwrap();
        let proj_unsigned_pinned = root.join("proj_unsigned_pinned");
        write_file(
            &proj_unsigned_pinned.join("spider.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nsignedlib = { version = \"^1.0\" }\n",
        );
        write_file(&proj_unsigned_pinned.join("src/main.matrix"), "print(1);\n");
        let up_out = run_spid3r(
            &proj_unsigned_pinned,
            &["install"],
            &[
                ("SPID3R_REGISTRY", Some(reg_str)),
                ("SPID3R_PACKAGES_TRUST_PK", Some(&pk_hex)),
            ],
        );
        assert!(
            !up_out.status.success(),
            "unsigned package must fail when trust_pk is configured"
        );
        assert!(!proj_unsigned_pinned.join("vendor/signedlib").exists());

        let proj_unsigned_warn = root.join("proj_unsigned_warn");
        write_file(
            &proj_unsigned_warn.join("spider.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nsignedlib = { version = \"^1.0\" }\n",
        );
        write_file(&proj_unsigned_warn.join("src/main.matrix"), "print(1);\n");
        let uw_out = run_spid3r(
            &proj_unsigned_warn,
            &["install"],
            &[("SPID3R_REGISTRY", Some(reg_str))],
        );
        assert!(
            uw_out.status.success(),
            "unsigned without trust_pk should still install: {}",
            String::from_utf8_lossy(&uw_out.stderr)
        );
        let uw_err = String::from_utf8_lossy(&uw_out.stderr);
        assert!(
            uw_err.contains("unsigned"),
            "expected unsigned warning on stderr, got: {}",
            uw_err
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn r8_4_diamond_dependency_unification_and_conflict() {
        let root = temp_dir("diamond");
        let reg_dir = root.join("registry");
        let reg_str = reg_dir.to_str().unwrap();

        for ver in ["1.2.0", "1.5.0", "2.0.0"] {
            let dir = root.join(format!("common_{}", ver));
            write_file(
                &dir.join("spider.toml"),
                &format!(
                    "[package]\nname = \"common\"\nversion = \"{}\"\nentry = \"src/lib.matrix\"\n",
                    ver
                ),
            );
            write_file(
                &dir.join("src/lib.matrix"),
                &format!("public fn ver() -> str {{ return \"{}\"; }}\n", ver),
            );
            let out = run_spid3r(&dir, &["publish", "--to-dir", reg_str], &[]);
            assert!(
                out.status.success(),
                "publish common {} failed: {}",
                ver,
                String::from_utf8_lossy(&out.stderr)
            );
        }

        let mid_dir = root.join("mid_1_0_0");
        write_file(
            &mid_dir.join("spider.toml"),
            "[package]\nname = \"mid\"\nversion = \"1.0.0\"\nentry = \"src/lib.matrix\"\n\n[dependencies]\ncommon = { version = \"=1.2.0\" }\n",
        );
        write_file(
            &mid_dir.join("src/lib.matrix"),
            "import \"common/lib\";\npublic fn mid_ver() -> str { return ver(); }\n",
        );
        let out = run_spid3r(&mid_dir, &["publish", "--to-dir", reg_str], &[]);
        assert!(
            out.status.success(),
            "publish mid failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        // 1. Compatible single-project diamond: ^1.0 and =1.2.0 must unify to 1.2.0 (not 1.5.0).
        let proj = root.join("app_ok");
        write_file(
            &proj.join("spider.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\ncommon = { version = \"^1.0\" }\nmid = { version = \"^1.0\" }\n",
        );
        write_file(
            &proj.join("src/main.matrix"),
            "import \"common/lib\";\nimport \"mid/lib\";\nprint(ver() + \":\" + mid_ver());\n",
        );
        let ok_out = run_spid3r(&proj, &["install"], &[("SPID3R_REGISTRY", Some(reg_str))]);
        assert!(
            ok_out.status.success(),
            "diamond install failed: {}",
            String::from_utf8_lossy(&ok_out.stderr)
        );
        let vendored_lib = std::fs::read_to_string(proj.join("vendor/common/lib.matrix")).unwrap();
        let lock_text = std::fs::read_to_string(proj.join("spider.lock")).unwrap();
        assert!(
            vendored_lib.contains("\"1.2.0\"") && lock_text.contains("version = \"1.2.0\""),
            "expected unified common 1.2.0, got:\n{}\nlock:\n{}",
            vendored_lib,
            lock_text
        );

        // 2. Compatible workspace diamond: member_a ^1.0 + member_b mid (=1.2.0) -> 1.2.0.
        let ws = root.join("ws_ok");
        write_file(
            &ws.join("spider.toml"),
            "[workspace]\nmembers = [\"a\", \"b\"]\n",
        );
        write_file(
            &ws.join("a/spider.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\ncommon = { version = \"^1.0\" }\n",
        );
        write_file(&ws.join("a/src/main.matrix"), "print(1);\n");
        write_file(
            &ws.join("b/spider.toml"),
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nmid = { version = \"^1.0\" }\n",
        );
        write_file(&ws.join("b/src/main.matrix"), "print(2);\n");
        let ws_out = run_spid3r(&ws, &["install"], &[("SPID3R_REGISTRY", Some(reg_str))]);
        assert!(
            ws_out.status.success(),
            "workspace diamond install failed: {}",
            String::from_utf8_lossy(&ws_out.stderr)
        );
        let ws_lib = std::fs::read_to_string(ws.join("vendor/common/lib.matrix")).unwrap();
        let ws_lock = std::fs::read_to_string(ws.join("spider.lock")).unwrap();
        assert!(
            ws_lib.contains("\"1.2.0\"") && ws_lock.contains("version = \"1.2.0\""),
            "expected workspace unified common 1.2.0, got:\n{}\nlock:\n{}",
            ws_lib,
            ws_lock
        );

        // 3. Conflicting diamond: ^2.0 vs =1.2.0 must fail cleanly.
        let proj_bad = root.join("app_conflict");
        write_file(
            &proj_bad.join("spider.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\ncommon = { version = \"^2.0\" }\nmid = { version = \"^1.0\" }\n",
        );
        write_file(&proj_bad.join("src/main.matrix"), "print(1);\n");
        let bad_out = run_spid3r(
            &proj_bad,
            &["install"],
            &[("SPID3R_REGISTRY", Some(reg_str))],
        );
        assert!(
            !bad_out.status.success(),
            "conflicting diamond must fail cleanly"
        );
        let bad_err = String::from_utf8_lossy(&bad_out.stderr);
        assert!(
            bad_err.contains("common") && !bad_err.contains("panicked"),
            "unexpected conflict error: {}",
            bad_err
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pip_style_install_uninstall_search_and_redirect_work() {
        let root = temp_dir("pip_style");
        let reg_dir = root.join("registry");

        let pkg_dir = root.join("hello_src");
        write_file(
            &pkg_dir.join("spider.toml"),
            "[package]\nname = \"hello\"\nversion = \"0.1.0\"\nentry = \"src/lib.matrix\"\ndescription = \"Starter greeting package for Spid3r\"\n",
        );
        write_file(
            &pkg_dir.join("src/lib.matrix"),
            "public fn greet(name: string) -> string {\n    return \"Hello, \" + name + \"!\";\n}\n",
        );
        let pub_out = run_spid3r(
            &pkg_dir,
            &["publish", "--to-dir", reg_dir.to_str().unwrap()],
            &[],
        );
        assert!(
            pub_out.status.success(),
            "publish failed: {}",
            String::from_utf8_lossy(&pub_out.stderr)
        );
        assert!(
            reg_dir.join("catalog.txt").is_file(),
            "catalog.txt must be written"
        );

        let srv = start_file_server(reg_dir.clone());
        let base_url = format!("http://127.0.0.1:{}", srv.port);

        // Redirect proxy server: sends HTTP 302 with relative/absolute Location to `srv`.
        let redir_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let redir_port = redir_listener.local_addr().unwrap().port();
        let redir_stop = Arc::new(AtomicBool::new(false));
        let redir_stop_bg = redir_stop.clone();
        let target_base = base_url.clone();
        let redir_handle = std::thread::spawn(move || {
            while !redir_stop_bg.load(Ordering::SeqCst) {
                match redir_listener.accept() {
                    Ok((mut stream, _)) => {
                        if redir_stop_bg.load(Ordering::SeqCst) {
                            break;
                        }
                        let mut buf = [0u8; 4096];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]);
                        let raw_path = req
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("/");
                        let dest = format!("{}{}", target_base, raw_path);
                        let hdr = format!(
                            "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            dest
                        );
                        let _ = stream.write_all(hdr.as_bytes());
                    }
                    Err(_) => break,
                }
            }
        });
        let redir_url = format!("http://127.0.0.1:{}", redir_port);

        // 1. Remote `spkg search` against catalog.txt via HTTP 302 redirect.
        let search_dir = root.join("search_cwd");
        std::fs::create_dir_all(&search_dir).unwrap();
        let search_out = run_spid3r(
            &search_dir,
            &["search", "hello"],
            &[("SPID3R_REGISTRY", Some(&redir_url))],
        );
        assert!(
            search_out.status.success(),
            "remote search failed: {}",
            String::from_utf8_lossy(&search_out.stderr)
        );
        let search_txt = String::from_utf8_lossy(&search_out.stdout);
        assert!(
            search_txt.contains("hello") && search_txt.contains("0.1.0"),
            "unexpected search output: {}",
            search_txt
        );

        // 2. Fresh directory: `spkg install hello` auto-initializes spider.toml & fetches latest.
        let fresh = root.join("fresh_app");
        std::fs::create_dir_all(&fresh).unwrap();
        let inst_out = run_spid3r(
            &fresh,
            &["install", "hello"],
            &[("SPID3R_REGISTRY", Some(&redir_url))],
        );
        assert!(
            inst_out.status.success(),
            "pip-style install failed: {}\nstdout: {}",
            String::from_utf8_lossy(&inst_out.stderr),
            String::from_utf8_lossy(&inst_out.stdout)
        );
        assert!(fresh.join("spider.toml").is_file());
        assert!(fresh.join("vendor/hello/lib.matrix").is_file());
        write_file(
            &fresh.join("src/main.matrix"),
            "import \"hello/lib\" as h;\nfn main() {\n    println(h.greet(\"Spid3r\"));\n}\n",
        );
        let run_out = run_spid3r(&fresh, &["run", "src/main.matrix"], &[]);
        assert!(
            run_out.status.success(),
            "run importing hello/lib failed: {}",
            String::from_utf8_lossy(&run_out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&run_out.stdout).trim(),
            "Hello, Spid3r!"
        );

        // 3. Exact version spec (`hello==0.1.0`) + `uninstall hello` + clean `verify`.
        let pinned = root.join("pinned_app");
        std::fs::create_dir_all(&pinned).unwrap();
        let pin_out = run_spid3r(
            &pinned,
            &["install", "hello==0.1.0"],
            &[("SPID3R_REGISTRY", Some(&base_url))],
        );
        assert!(
            pin_out.status.success(),
            "hello==0.1.0 install failed: {}",
            String::from_utf8_lossy(&pin_out.stderr)
        );
        let manifest_pinned = std::fs::read_to_string(pinned.join("spider.toml")).unwrap();
        assert!(
            manifest_pinned.contains("\"=0.1.0\""),
            "expected exact pin =0.1.0 in:\n{}",
            manifest_pinned
        );

        let un_out = run_spid3r(&pinned, &["uninstall", "hello"], &[]);
        assert!(
            un_out.status.success(),
            "uninstall failed: {}",
            String::from_utf8_lossy(&un_out.stderr)
        );
        assert!(!pinned.join("vendor/hello").exists());
        let ver_out = run_spid3r(&pinned, &["verify"], &[]);
        assert!(ver_out.status.success());

        redir_stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", redir_port));
        let _ = redir_handle.join();
        drop(srv);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn lazy_fetch_preextract_sig_pin_orphan_prune_and_mirrors_work() {
        let root = temp_dir("advanced_pkg");
        let reg_dir = root.join("registry");
        let cache_dir = root.join("shared_cache");
        let cache_str = cache_dir.to_str().unwrap();

        // Publish leaf 1.0.0, 1.1.0, and 1.2.0 using explicit signing key #1.
        let key1 = "11".repeat(32);
        let key2 = "22".repeat(32);
        for (ver, ret_val) in [("1.0.0", "v100"), ("1.1.0", "v110"), ("1.2.0", "v120")] {
            let ldir = root.join(format!("leaf_{}", ver));
            write_file(
                &ldir.join("spider.toml"),
                &format!(
                    "[package]\nname = \"leaf\"\nversion = \"{}\"\nentry = \"src/lib.matrix\"\ndescription = \"Array math\"\ncategories = [\"data-science\"]\ntargets = [\"any\"]\n",
                    ver
                ),
            );
            write_file(
                &ldir.join("src/lib.matrix"),
                &format!(
                    "public fn leaf_ver() -> string {{ return \"{}\"; }}\n",
                    ret_val
                ),
            );
            let out = run_spid3r(
                &ldir,
                &["publish", "--to-dir", reg_dir.to_str().unwrap()],
                &[("SPID3R_SIGN_KEY", Some(&key1))],
            );
            assert!(
                out.status.success(),
                "publish leaf {} failed: {}",
                ver,
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(reg_dir
                .join(format!("leaf/{}/manifest.sha256", ver))
                .is_file());
            assert!(reg_dir
                .join(format!("leaf/{}/manifest.sha256.sig", ver))
                .is_file());
        }
        let pk1 = std::fs::read_to_string(reg_dir.join("registry.pub"))
            .unwrap()
            .trim()
            .to_string();

        // Publish `mid` 0.1.0 signed with key #2 (simulating key rotation).
        let mid_dir = root.join("mid_010");
        write_file(
            &mid_dir.join("spider.toml"),
            "[package]\nname = \"mid\"\nversion = \"0.1.0\"\nentry = \"src/lib.matrix\"\ncategories = [\"media\"]\n\n[dependencies]\nleaf = { version = \"^1.0.0\" }\n",
        );
        write_file(
            &mid_dir.join("src/lib.matrix"),
            "import \"leaf/lib\" as lf;\npublic fn mid_info() -> string { return \"mid+\" + lf.leaf_ver(); }\n",
        );
        let pub_mid = run_spid3r(
            &mid_dir,
            &["publish", "--to-dir", reg_dir.to_str().unwrap()],
            &[("SPID3R_SIGN_KEY", Some(&key2))],
        );
        assert!(pub_mid.status.success());
        let pk2 = std::fs::read_to_string(reg_dir.join("registry.pub"))
            .unwrap()
            .trim()
            .to_string();
        let rotated_trust = format!("{},{}", pk1, pk2);

        // Request-recording HTTP server serving `reg_dir`.
        let req_log: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let req_log_bg = req_log.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = stop.clone();
        let reg_clone = reg_dir.clone();
        let srv_handle = std::thread::spawn(move || {
            while !stop_bg.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if stop_bg.load(Ordering::SeqCst) {
                            break;
                        }
                        let mut buf = [0u8; 4096];
                        let n = stream.read(&mut buf).unwrap_or(0);
                        let req = String::from_utf8_lossy(&buf[..n]);
                        let raw_path = req
                            .lines()
                            .next()
                            .and_then(|l| l.split_whitespace().nth(1))
                            .unwrap_or("/");
                        let rel = raw_path
                            .split('?')
                            .next()
                            .unwrap_or("/")
                            .trim_start_matches('/');
                        req_log_bg.lock().unwrap().push(rel.to_string());
                        let mut candidate = reg_clone.clone();
                        for comp in Path::new(rel).components() {
                            if let std::path::Component::Normal(c) = comp {
                                candidate.push(c);
                            }
                        }
                        if candidate.is_file() {
                            let body = std::fs::read(&candidate).unwrap_or_default();
                            let hdr = format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(hdr.as_bytes());
                            let _ = stream.write_all(&body);
                        } else {
                            let hdr = "HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found";
                            let _ = stream.write_all(hdr.as_bytes());
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let mirror_list = format!("http://127.0.0.1:1,http://127.0.0.1:{}", port);

        // 1. Lazy single-version fetch + mirror failover + category search.
        let search_out = run_spid3r(
            &root,
            &["search", "--category", "data-science"],
            &[
                ("SPID3R_REGISTRY", Some(&mirror_list)),
                ("SPID3R_CACHE_DIR", Some(cache_str)),
            ],
        );
        assert!(search_out.status.success());
        let stxt = String::from_utf8_lossy(&search_out.stdout);
        assert!(
            stxt.contains("leaf") && !stxt.contains("mid"),
            "cat search: {stxt}"
        );

        let proj_lazy = root.join("proj_lazy");
        std::fs::create_dir_all(&proj_lazy).unwrap();
        req_log.lock().unwrap().clear();
        let inst_lazy = run_spid3r(
            &proj_lazy,
            &["install", "leaf==1.1.0"],
            &[
                ("SPID3R_REGISTRY", Some(&mirror_list)),
                ("SPID3R_TRUST_PK", Some(&rotated_trust)),
                ("SPID3R_CACHE_DIR", Some(cache_str)),
            ],
        );
        assert!(
            inst_lazy.status.success(),
            "lazy install failed: {}",
            String::from_utf8_lossy(&inst_lazy.stderr)
        );
        {
            let logged = req_log.lock().unwrap();
            assert!(
                logged.iter().any(|p| p == "leaf/1.1.0/package.tar"),
                "must fetch 1.1.0: {:?}",
                *logged
            );
            assert!(
                !logged
                    .iter()
                    .any(|p| p.contains("leaf/1.0.0/") || p.contains("leaf/1.2.0/")),
                "must NOT fetch unselected versions: {:?}",
                *logged
            );
        }

        // 2. Pre-extract Ed25519 refusal: bad sha256.txt.sig fails BEFORE package.tar is ever requested!
        let bad_sig_file = reg_dir.join("leaf/1.0.0/sha256.txt.sig");
        let orig_sig = std::fs::read(&bad_sig_file).unwrap();
        std::fs::write(&bad_sig_file, format!("{}\n", "00".repeat(64))).unwrap();
        let proj_bad_sig = root.join("proj_bad_sig");
        std::fs::create_dir_all(&proj_bad_sig).unwrap();
        req_log.lock().unwrap().clear();
        let bad_sig_out = run_spid3r(
            &proj_bad_sig,
            &["install", "leaf==1.0.0"],
            &[
                (
                    "SPID3R_REGISTRY",
                    Some(&format!("http://127.0.0.1:{}", port)),
                ),
                ("SPID3R_TRUST_PK", Some(&rotated_trust)),
                ("SPID3R_CACHE_DIR", Some(cache_str)),
            ],
        );
        std::fs::write(&bad_sig_file, orig_sig).unwrap();
        assert!(!bad_sig_out.status.success());
        {
            let logged = req_log.lock().unwrap();
            assert!(
                !logged.iter().any(|p| p == "leaf/1.0.0/package.tar"),
                "tampered signature must reject BEFORE downloading package.tar: {:?}",
                *logged
            );
        }

        // 3. Multi-key rotation + transitive tree + orphan vendor pruning + Windows backslash path escape.
        let proj_tree = root.join("proj_tree");
        std::fs::create_dir_all(&proj_tree).unwrap();
        let inst_mid = run_spid3r(
            &proj_tree,
            &["install", "mid"],
            &[
                (
                    "SPID3R_REGISTRY",
                    Some(&format!("http://127.0.0.1:{}", port)),
                ),
                ("SPID3R_TRUST_PK", Some(&rotated_trust)),
                ("SPID3R_CACHE_DIR", Some(cache_str)),
            ],
        );
        assert!(
            inst_mid.status.success(),
            "mid install with key rotation failed: {}",
            String::from_utf8_lossy(&inst_mid.stderr)
        );
        assert!(proj_tree.join("vendor/leaf/lib.matrix").is_file());
        let tree_out = run_spid3r(
            &proj_tree,
            &["tree"],
            &[
                (
                    "SPID3R_REGISTRY",
                    Some(&format!("http://127.0.0.1:{}", port)),
                ),
                ("SPID3R_CACHE_DIR", Some(cache_str)),
            ],
        );
        let tree_txt = String::from_utf8_lossy(&tree_out.stdout);
        assert!(
            tree_txt.contains("mid 0.1.0") && tree_txt.contains("leaf 1.2.0"),
            "transitive tree missing leaf:\n{}",
            tree_txt
        );

        // Removing `mid` must prune orphaned transitive `vendor/leaf` cleanly!
        let rem_mid = run_spid3r(&proj_tree, &["remove", "mid"], &[]);
        assert!(rem_mid.status.success());
        assert!(!proj_tree.join("vendor/mid").exists());
        assert!(
            !proj_tree.join("vendor/leaf").exists(),
            "orphaned transitive vendor/leaf was not pruned"
        );
        assert!(run_spid3r(&proj_tree, &["verify"], &[]).status.success());

        // 4. Lockfile pins registry version until --update.
        let reg_pin = root.join("reg_pin");
        let p_src = root.join("p_src");
        write_file(
            &p_src.join("spider.toml"),
            "[package]\nname = \"p\"\nversion = \"1.0.0\"\nentry = \"src/lib.matrix\"\n",
        );
        write_file(
            &p_src.join("src/lib.matrix"),
            "public fn pv() { return 1; }\n",
        );
        assert!(run_spid3r(
            &p_src,
            &["publish", "--to-dir", reg_pin.to_str().unwrap()],
            &[]
        )
        .status
        .success());

        let app_pin = root.join("app_pin");
        write_file(
            &app_pin.join("spider.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\np = { version = \"^1.0.0\" }\n",
        );
        write_file(
            &app_pin.join("src/main.matrix"),
            "fn main() { return 0; }\n",
        );
        assert!(run_spid3r(
            &app_pin,
            &["install"],
            &[("SPID3R_REGISTRY", Some(reg_pin.to_str().unwrap()))]
        )
        .status
        .success());

        write_file(
            &p_src.join("spider.toml"),
            "[package]\nname = \"p\"\nversion = \"1.1.0\"\nentry = \"src/lib.matrix\"\n",
        );
        assert!(run_spid3r(
            &p_src,
            &["publish", "--to-dir", reg_pin.to_str().unwrap()],
            &[]
        )
        .status
        .success());

        // Plain `install` stays at locked 1.0.0; `--update` upgrades to 1.1.0.
        assert!(run_spid3r(
            &app_pin,
            &["install"],
            &[("SPID3R_REGISTRY", Some(reg_pin.to_str().unwrap()))]
        )
        .status
        .success());
        let lock1 = std::fs::read_to_string(app_pin.join("spider.lock")).unwrap();
        assert!(
            lock1.contains("version = \"1.0.0\""),
            "must stay pinned:\n{lock1}"
        );

        assert!(run_spid3r(
            &app_pin,
            &["install", "--update"],
            &[("SPID3R_REGISTRY", Some(reg_pin.to_str().unwrap()))]
        )
        .status
        .success());
        let lock2 = std::fs::read_to_string(app_pin.join("spider.lock")).unwrap();
        assert!(
            lock2.contains("version = \"1.1.0\""),
            "--update must upgrade:\n{lock2}"
        );

        // 5. Escaped backslash path + incompatible target gate.
        let win_lib = root.join("bs_lib");
        write_file(
            &win_lib.join("spider.toml"),
            "[package]\nname = \"bs_lib\"\nversion = \"0.1.0\"\nentry = \"src/lib.matrix\"\n",
        );
        write_file(
            &win_lib.join("src/lib.matrix"),
            "public fn ok() { return 7; }\n",
        );
        let consumer = root.join("bs_consumer");
        write_file(
            &consumer.join("spider.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        );
        write_file(
            &consumer.join("src/main.matrix"),
            "fn main() { return 0; }\n",
        );
        let rel_path = if cfg!(windows) {
            "..\\bs_lib"
        } else {
            "../bs_lib"
        };
        let bs_add = run_spid3r(&consumer, &["add", "bs_lib", "--path", rel_path], &[]);
        assert!(
            bs_add.status.success(),
            "backslash path failed: {}",
            String::from_utf8_lossy(&bs_add.stderr)
        );

        let incompat = root.join("incompat_pkg");
        write_file(
            &incompat.join("spider.toml"),
            "[package]\nname = \"incompat\"\nversion = \"0.1.0\"\ntargets = [\"plan9-vax\"]\n",
        );
        write_file(&incompat.join("src/lib.matrix"), "public fn x() {}\n");
        let bad_target = run_spid3r(
            &consumer,
            &["add", "incompat", "--path", "../incompat_pkg"],
            &[],
        );
        assert!(!bad_target.status.success());
        assert!(String::from_utf8_lossy(&bad_target.stderr).contains("plan9-vax"));

        stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", port));
        let _ = srv_handle.join();
        let _ = std::fs::remove_dir_all(&root);
    }
}
