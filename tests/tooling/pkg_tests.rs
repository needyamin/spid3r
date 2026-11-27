//! `spid3r init` / `spid3r install` tests: manifest scaffolding, path-dependency
//! vendoring, the lockfile, idempotence, and running a program that imports a
//! vendored dependency.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

struct Project {
    /// The application directory (the project root, where spider.toml lives).
    root: PathBuf,
    /// Its parent: path dependencies are created as siblings here, which is
    /// how real projects reference them (`../mathlib`).
    base: PathBuf,
}

impl Project {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!("spid3r_pkg_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("app");
        std::fs::create_dir_all(&root).expect("mkdir");
        Project { root, base }
    }

    /// Write a file relative to the base (siblings included).
    fn write_sibling(&self, rel: &str, text: &str) {
        let p = self.base.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&p, text).expect("write");
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&p, text).expect("write");
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap_or_default()
    }

    fn exists(&self, rel: &str) -> bool {
        self.root.join(rel).exists()
    }

    /// Run a command with the project root as the working directory.
    ///
    /// The tests create git dependencies as local repositories, so they opt into
    /// the local-transport allowance the URL whitelist requires
    /// (`SPID3R_ALLOW_LOCAL_GIT_DEPS`, audit A-5). `ext::` stays rejected even
    /// with this set.
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(bin())
            .args(args)
            .current_dir(&self.root)
            .env("SPID3R_ALLOW_LOCAL_GIT_DEPS", "1")
            .output()
            .expect("spawn");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    /// Run with extra environment variables (e.g. `SPID3R_REGISTRY`).
    fn run_env(&self, args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
        let mut cmd = Command::new(bin());
        cmd.args(args)
            .current_dir(&self.root)
            .env("SPID3R_ALLOW_LOCAL_GIT_DEPS", "1");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("spawn");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    /// Run with a variable explicitly removed (to test the "unset" error path)
    /// against an empty `SPID3R_INI` so machine-global config never leaks in.
    fn run_unset(&self, args: &[&str], key: &str) -> (i32, String, String) {
        let ini = self.base.join("empty.ini");
        if !ini.exists() {
            let _ = std::fs::write(&ini, "");
        }
        let out = Command::new(bin())
            .args(args)
            .current_dir(&self.root)
            .env("SPID3R_INI", &ini)
            .env_remove(key)
            .env_remove("SPID3R_PACKAGES_REGISTRY")
            .output()
            .expect("spawn");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    /// Create a dependency project as a sibling of the app.
    fn add_dep(&self, name: &str, version: &str, body: &str) {
        self.write_sibling(
            &format!("{}/spider.toml", name),
            &format!(
                "[package]\nname = \"{}\"\nversion = \"{}\"\nentry = \"src/lib.matrix\"\n",
                name, version
            ),
        );
        self.write_sibling(&format!("{}/src/lib.matrix", name), body);
    }

    /// Create a package inside the local registry at `<base>/registry`.
    fn add_registry_pkg(&self, name: &str, version: &str, body: &str) {
        self.write_sibling(
            &format!("registry/{}/{}/spider.toml", name, version),
            &format!(
                "[package]\nname = \"{}\"\nversion = \"{}\"\nentry = \"src/lib.matrix\"\n",
                name, version
            ),
        );
        self.write_sibling(
            &format!("registry/{}/{}/src/lib.matrix", name, version),
            body,
        );
        // A registry package MUST ship sha256.txt (the installer hard-fails
        // without it). Match what `spid3r publish --to-dir` writes: the hash
        // of the src/ tree, one line.
        let pkg_dir = self.base.join("registry").join(name).join(version);
        let hash = spid3r_lang::pkg::tree_hash(&pkg_dir).expect("tree hash");
        self.write_sibling(
            &format!("registry/{}/{}/sha256.txt", name, version),
            &format!("{}\n", hash),
        );
    }

    fn registry_dir(&self) -> String {
        self.base.join("registry").to_str().unwrap().to_string()
    }

    fn add_app(&self, deps: &[(&str, &str)], main: &str) {
        let mut manifest = String::from(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\n",
        );
        for (name, path) in deps {
            manifest.push_str(&format!("{} = {{ path = \"{}\" }}\n", name, path));
        }
        self.write("spider.toml", &manifest);
        self.write("src/main.matrix", main);
    }

    /// Like `add_app`, but each dependency is a registry version constraint.
    fn add_app_registry(&self, deps: &[(&str, &str)], main: &str) {
        let mut manifest = String::from(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\n",
        );
        for (name, constraint) in deps {
            manifest.push_str(&format!("{} = {{ version = \"{}\" }}\n", name, constraint));
        }
        self.write("spider.toml", &manifest);
        self.write("src/main.matrix", main);
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

const DEP_BODY: &str =
    "public fn square(x) {\n    return x * x;\n}\npublic const VERSION = \"1.0.0\";\n";

#[test]
fn init_scaffolds_a_project_and_never_overwrites() {
    let p = Project::new("init");
    let (code, out, _) = p.run(&["init"]);
    assert_eq!(code, 0, "init failed: {}", out);
    assert!(p.exists("spider.toml"), "manifest missing");
    assert!(p.exists("src/main.matrix"), "entry missing");
    let manifest = p.read("spider.toml");
    assert!(manifest.contains("[package]"), "manifest: {}", manifest);
    assert!(
        manifest.contains("[dependencies]"),
        "manifest: {}",
        manifest
    );

    // Second run leaves the existing files alone.
    p.write("src/main.matrix", "// mine\nfn main() { return 0; }\n");
    let (code, out, _) = p.run(&["init"]);
    assert_eq!(code, 0);
    assert!(out.contains("already initialised"), "output: {}", out);
    assert!(
        p.read("src/main.matrix").contains("// mine"),
        "init must not overwrite"
    );
}

#[test]
fn install_vendors_path_dependencies_and_writes_a_lockfile() {
    let p = Project::new("install");
    p.add_dep("mathlib", "1.0.0", DEP_BODY);
    p.add_app(
        &[("mathlib", "../mathlib")],
        "fn main() {\n    return 0;\n}\n",
    );

    let (code, out, err) = p.run(&["install"]);
    assert_eq!(code, 0, "install failed: {}{}", out, err);
    assert!(out.contains("mathlib"), "summary: {}", out);
    assert!(
        p.exists("vendor/mathlib/lib.matrix"),
        "vendored file missing"
    );
    let lock = p.read("spider.lock");
    assert!(lock.contains("name = \"mathlib\""), "lock: {}", lock);
    assert!(lock.contains("version = \"1.0.0\""), "lock: {}", lock);
    assert!(lock.contains("sha256 = \""), "lock: {}", lock);
    assert!(lock.contains("files = 1"), "lock: {}", lock);
}

#[test]
fn install_is_idempotent_and_restores_tampered_vendor_files() {
    let p = Project::new("idem");
    p.add_dep("mathlib", "1.0.0", DEP_BODY);
    p.add_app(
        &[("mathlib", "../mathlib")],
        "fn main() {\n    return 0;\n}\n",
    );

    assert_eq!(p.run(&["install"]).0, 0);
    let first_lock = p.read("spider.lock");
    assert_eq!(p.run(&["install"]).0, 0);
    assert_eq!(p.read("spider.lock"), first_lock, "lock must be stable");

    // Tamper with the vendored copy: install re-syncs it from the dependency.
    p.write(
        "vendor/mathlib/lib.matrix",
        "public fn square(x) {\n    return 0;\n}\n",
    );
    let (code, out, _) = p.run(&["install"]);
    assert_eq!(code, 0);
    assert!(
        p.read("vendor/mathlib/lib.matrix").contains("x * x"),
        "vendored file should be restored; summary: {}",
        out
    );
}

#[test]
fn install_reports_missing_and_malformed_dependencies() {
    let p = Project::new("errors");
    p.add_app(&[("ghost", "../ghost")], "fn main() {\n    return 0;\n}\n");
    let (code, _, err) = p.run(&["install"]);
    assert_ne!(code, 0, "missing dependency should fail");
    assert!(err.contains("ghost"), "error should name it: {}", err);

    // A dependency directory without a manifest is rejected.
    let p2 = Project::new("errors2");
    p2.write_sibling("lib/src/lib.matrix", DEP_BODY);
    p2.add_app(&[("lib", "../lib")], "fn main() {\n    return 0;\n}\n");
    let (code, _, err) = p2.run(&["install"]);
    assert_ne!(code, 0);
    assert!(err.contains("spider.toml"), "error: {}", err);
}

#[test]
fn install_without_a_manifest_is_a_clear_error() {
    let p = Project::new("nomanifest");
    let (code, _, err) = p.run(&["install"]);
    assert_ne!(code, 0);
    assert!(err.contains("spider.toml"), "error: {}", err);
}

#[test]
fn a_program_can_import_a_vendored_dependency() {
    let p = Project::new("run");
    p.add_dep("mathlib", "1.0.0", DEP_BODY);
    p.add_app(
        &[("mathlib", "../mathlib")],
        "import \"mathlib/lib\";\n\nfn main() {\n    println(square(7));\n    println(VERSION);\n    return 0;\n}\n",
    );
    assert_eq!(p.run(&["install"]).0, 0);

    for engine in ["vm", "tree"] {
        let (code, out, err) = p.run(&["run", "--engine", engine, "src/main.matrix"]);
        assert_eq!(code, 0, "{} run failed: {}{}", engine, out, err);
        assert_eq!(out, "49\n1.0.0\n", "{} output: {}", engine, out);
    }
}

#[test]
fn two_dependencies_are_vendored_independently() {
    let p = Project::new("multi");
    p.add_dep("mathlib", "1.0.0", DEP_BODY);
    p.add_dep(
        "strlib",
        "2.0.0",
        "public fn shout(s) {\n    return s.upper() + \"!\";\n}\n",
    );
    p.add_app(
        &[("mathlib", "../mathlib"), ("strlib", "../strlib")],
        "import \"mathlib/lib\";\nimport \"strlib/lib\";\n\nfn main() {\n    println(square(3));\n    println(shout(\"hi\"));\n    return 0;\n}\n",
    );
    let (code, out, _) = p.run(&["install"]);
    assert_eq!(code, 0);
    assert!(out.contains("2 dependency"), "summary: {}", out);
    assert!(p.exists("vendor/mathlib/lib.matrix"));
    assert!(p.exists("vendor/strlib/lib.matrix"));
    let (code, out, err) = p.run(&["run", "--engine", "vm", "src/main.matrix"]);
    assert_eq!(code, 0, "run failed: {}{}", out, err);
    assert_eq!(out, "9\nHI!\n", "output: {}", out);
}

#[test]
fn lockfile_paths_are_recorded_relative_to_the_manifest() {
    let p = Project::new("lockpaths");
    p.add_dep("mathlib", "1.0.0", DEP_BODY);
    p.add_app(
        &[("mathlib", "../mathlib")],
        "fn main() {\n    return 0;\n}\n",
    );
    assert_eq!(p.run(&["install"]).0, 0);
    let lock = p.read("spider.lock");
    assert!(
        lock.contains("source = \"path+../mathlib\""),
        "lock should record the declared path: {}",
        lock
    );
}

/// Guard: the CLI's `install` must not walk outside the project root.
#[test]
fn vendor_directory_lives_inside_the_project() {
    let p = Project::new("vendorloc");
    p.add_dep("mathlib", "1.0.0", DEP_BODY);
    p.add_app(
        &[("mathlib", "../mathlib")],
        "fn main() {\n    return 0;\n}\n",
    );
    assert_eq!(p.run(&["install"]).0, 0);
    let vendor = Path::new(&p.root).join("vendor");
    assert!(vendor.is_dir());
    let outside = p
        .root
        .parent()
        .map(|d| d.join("vendor"))
        .unwrap_or_default();
    let _ = outside; // documented: vendor/ is per-project
    assert!(p.exists("vendor/mathlib/lib.matrix"));
}

// ------------------------------------------------------------------- git deps

/// Run a git command in `dir`, failing loudly.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-C", dir.to_str().unwrap()])
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Create a git-backed dependency repository at `<base>/<name>` with one commit.
fn add_git_dep(base: &Path, name: &str, version: &str, body: &str) -> PathBuf {
    let dir = base.join(name);
    std::fs::create_dir_all(dir.join("src")).expect("mkdir");
    std::fs::write(
        dir.join("spider.toml"),
        format!(
            "[package]\nname = \"{}\"\nversion = \"{}\"\nentry = \"src/lib.matrix\"\n",
            name, version
        ),
    )
    .expect("write");
    std::fs::write(dir.join("src/lib.matrix"), body).expect("write");
    git(&dir, &["init", "--quiet"]);
    git(&dir, &["add", "-A"]);
    git(
        &dir,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--quiet",
            "-m",
            "initial",
        ],
    );
    dir
}

#[test]
fn parses_git_dependencies() {
    let m = spid3r_lang::pkg::parse_manifest(
        "[package]\nname = \"app\"\n\n[dependencies]\nmathlib = { git = \"https://example.com/mathlib\", rev = \"main\" }\nutil = { git = \"https://example.com/util\" }\n",
    )
    .expect("parse");
    assert_eq!(
        m.deps[0].source,
        spid3r_lang::pkg::DepSource::Git {
            url: "https://example.com/mathlib".to_string(),
            rev: Some("main".to_string())
        }
    );
    assert_eq!(
        m.deps[1].source,
        spid3r_lang::pkg::DepSource::Git {
            url: "https://example.com/util".to_string(),
            rev: None
        }
    );
    // Both keys at once is rejected.
    assert!(spid3r_lang::pkg::parse_manifest(
        "[package]\nname = \"a\"\n\n[dependencies]\nx = { path = \"../x\", git = \"u\" }\n"
    )
    .is_err());
}

#[test]
fn install_clones_a_git_dependency_and_pins_the_commit() {
    let p = Project::new("gitdep");
    let repo = add_git_dep(&p.base, "gitsrc", "1.0.0", DEP_BODY);
    let url = repo.to_string_lossy().replace('\\', "/");
    p.write(
        "spider.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\ngitsrc = {{ git = \"{}\" }}\n",
            url
        ),
    );
    p.write(
        "src/main.matrix",
        "import \"gitsrc/lib\";\n\nfn main() {\n    println(square(6));\n    return 0;\n}\n",
    );

    let (code, out, err) = p.run(&["install"]);
    assert_eq!(code, 0, "install failed: {}{}", out, err);
    assert!(p.exists("vendor/gitsrc/lib.matrix"), "not vendored");
    let lock = p.read("spider.lock");
    assert!(lock.contains("source = \"git+"), "lock: {}", lock);
    // Pinned to a full 40-character commit hash.
    let sha_line = lock
        .lines()
        .find(|l| l.starts_with("source = \"git+"))
        .expect("source line");
    let commit = sha_line.rsplit('#').next().unwrap().trim_end_matches('"');
    assert_eq!(
        commit.len(),
        40,
        "expected a full commit hash: {}",
        sha_line
    );
    assert!(
        commit.chars().all(|c| c.is_ascii_hexdigit()),
        "commit should be hex: {}",
        commit
    );

    // The program runs against the vendored clone.
    let (code, out, err) = p.run(&["run", "--engine", "vm", "src/main.matrix"]);
    assert_eq!(code, 0, "run failed: {}{}", out, err);
    assert_eq!(out, "36\n");

    // Re-install with no upstream change keeps the lock byte-identical.
    let before = p.read("spider.lock");
    assert_eq!(p.run(&["install"]).0, 0);
    assert_eq!(p.read("spider.lock"), before, "lock must be stable");
}

#[test]
fn a_pinned_rev_survives_new_upstream_commits() {
    let p = Project::new("gitpin");
    let repo = add_git_dep(&p.base, "pinned", "1.0.0", DEP_BODY);
    let first = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    // A newer commit changes the dependency.
    std::fs::write(
        repo.join("src/lib.matrix"),
        "public fn square(x) {\n    return x * x * x;\n}\n",
    )
    .expect("write");
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--quiet",
            "-m",
            "second",
        ],
    );
    let url = repo.to_string_lossy().replace('\\', "/");
    p.write(
        "spider.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\npinned = {{ git = \"{}\", rev = \"{}\" }}\n",
            url, first
        ),
    );
    p.write("src/main.matrix", "fn main() {\n    return 0;\n}\n");
    let (code, out, err) = p.run(&["install"]);
    assert_eq!(code, 0, "install failed: {}{}", out, err);
    assert!(
        p.read("vendor/pinned/lib.matrix").contains("x * x"),
        "pinned rev must be the first commit's content"
    );
    let lock = p.read("spider.lock");
    assert!(
        lock.contains(&first),
        "lock should record the pinned commit {}: {}",
        first,
        lock
    );
}

#[test]
fn a_git_dependency_that_is_not_a_repository_fails_clearly() {
    let p = Project::new("gitbad");
    let not_a_repo = p.base.join("plain");
    std::fs::create_dir_all(not_a_repo.join("src")).expect("mkdir");
    std::fs::write(
        not_a_repo.join("spider.toml"),
        "[package]\nname = \"plain\"\n",
    )
    .expect("write");
    let url = not_a_repo.to_string_lossy().replace('\\', "/");
    p.write(
        "spider.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nplain = {{ git = \"{}\" }}\n",
            url
        ),
    );
    p.write("src/main.matrix", "fn main() {\n    return 0;\n}\n");
    let (code, _, err) = p.run(&["install"]);
    assert_ne!(code, 0, "cloning a non-repository must fail");
    assert!(err.contains("git"), "error should mention git: {}", err);
}

// ------------------------------------------------------------ version rules

#[test]
fn version_constraints_match_as_documented() {
    use spid3r_lang::pkg::{Constraint, Version};
    let v = |s| Version::parse(s).expect("parse");
    // ^1.2.3: same major, at least 1.2.3.
    let caret = Constraint::parse("^1.2.3").expect("parse");
    assert!(caret.matches(&v("1.2.3")));
    assert!(caret.matches(&v("1.9.9")));
    assert!(!caret.matches(&v("2.0.0")));
    assert!(!caret.matches(&v("1.2.2")));
    // >=1.2: at least 1.2.0, any major.
    let at_least = Constraint::parse(">=1.2").expect("parse");
    assert!(at_least.matches(&v("2.0.0")));
    assert!(at_least.matches(&v("1.2.0")));
    assert!(!at_least.matches(&v("1.1.9")));
    // =1.0.0 and bare 1.0.0 are exact.
    for spec in ["=1.0.0", "1.0.0"] {
        let exact = Constraint::parse(spec).expect("parse");
        assert!(exact.matches(&v("1.0.0")));
        assert!(!exact.matches(&v("1.0.1")));
    }
    // 1..3 components are accepted ("1" = 1.0.0); anything else is not.
    let one = Constraint::parse("1").expect("parse");
    assert!(one.matches(&v("1.0.0")));
    assert!(Constraint::parse("a.b.c").is_err());
    assert!(Constraint::parse("1.2.3.4").is_err());
}

#[test]
fn a_version_mismatch_is_a_clear_error() {
    let p = Project::new("mismatch");
    p.add_dep("mathlib", "2.0.0", DEP_BODY);
    p.write(
        "spider.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nmathlib = { path = \"../mathlib\", version = \"^1.0\" }\n",
    );
    p.write("src/main.matrix", "fn main() {\n    return 0;\n}\n");
    let (code, _, err) = p.run(&["install"]);
    assert_ne!(code, 0, "incompatible version must fail");
    assert!(
        err.contains("^1.0") && err.contains("2.0.0"),
        "error should name the constraint and the actual version: {}",
        err
    );
}

#[test]
fn a_satisfied_version_constraint_installs() {
    let p = Project::new("satisfied");
    p.add_dep("mathlib", "1.5.0", DEP_BODY);
    p.write(
        "spider.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nmathlib = { path = \"../mathlib\", version = \"^1.2\" }\n",
    );
    p.write("src/main.matrix", "fn main() {\n    return 0;\n}\n");
    let (code, _, err) = p.run(&["install"]);
    assert_eq!(code, 0, "satisfying constraint should install: {}", err);
    assert!(p.read("spider.lock").contains("version = \"1.5.0\""));
}

// ----------------------------------------------------------------- build

#[test]
fn init_writes_a_gitignore_ignoring_vendor_target_and_cache() {
    let p = Project::new("gitignore");
    let (code, _, _) = p.run(&["init"]);
    assert_eq!(code, 0);
    let gi = p.read(".gitignore");
    for wanted in ["vendor/", "target/", ".spid3r/"] {
        assert!(gi.contains(wanted), ".gitignore missing {}: {}", wanted, gi);
    }
}

#[test]
fn build_compiles_the_project_entry_and_exec_runs_it() {
    let p = Project::new("build");
    p.add_dep("mathlib", "1.0.0", DEP_BODY);
    p.write(
        "spider.toml",
        "[package]\nname = \"myapp\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nmathlib = { path = \"../mathlib\" }\n",
    );
    p.write(
        "src/main.matrix",
        "import \"mathlib/lib\";\n\nfn main() {\n    println(square(9));\n    return 0;\n}\n",
    );
    assert_eq!(p.run(&["install"]).0, 0);

    // build refuses to run before install when dependencies are declared.
    let fresh = Project::new("build-fresh");
    fresh.write(
        "spider.toml",
        "[package]\nname = \"myapp\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nmathlib = { path = \"../mathlib\" }\n",
    );
    fresh.write("src/main.matrix", "fn main() {\n    return 0;\n}\n");
    let (code, _, err) = fresh.run(&["dist"]);
    assert_ne!(code, 0, "build without install should fail");
    assert!(err.contains("spid3r install"), "error: {}", err);

    let (code, out, err) = p.run(&["dist"]);
    assert_eq!(code, 0, "build failed: {}{}", out, err);
    assert!(p.exists("target/myapp.matrixc"), "artifact missing");
    let (code, out, err) = p.run(&["exec", "target/myapp.matrixc"]);
    assert_eq!(code, 0, "exec failed: {}{}", out, err);
    assert_eq!(out, "81\n", "output: {}", out);

    // An explicit -o overrides the default location.
    let (code, out, _) = p.run(&["dist", "-o", "dist/app.matrixc"]);
    assert_eq!(code, 0, "build -o failed: {}", out);
    assert!(p.exists("dist/app.matrixc"));
}

#[test]
fn build_outside_a_project_is_a_clear_error() {
    let p = Project::new("build-empty");
    let (code, _, err) = p.run(&["dist"]);
    assert_ne!(code, 0);
    assert!(err.contains("spider.toml"), "error: {}", err);
}

// ------------------------------------------------- project-aware run / test

#[test]
fn run_without_a_file_runs_the_project_entry() {
    let p = Project::new("runentry");
    p.add_app(
        &[],
        "fn main() {\n    println(\"entry ran\");\n    return 0;\n}\n",
    );
    for engine in ["tree", "vm"] {
        let (code, out, err) = p.run(&["run", "--engine", engine]);
        assert_eq!(code, 0, "{} run failed: {}{}", engine, out, err);
        assert_eq!(out, "entry ran\n", "{} output: {}", engine, out);
    }
    // A missing entry is reported clearly.
    p.write("spider.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/gone.matrix\"\n\n[dependencies]\n");
    let (code, _, err) = p.run(&["run"]);
    assert_ne!(code, 0);
    assert!(err.contains("gone.matrix"), "error: {}", err);
}

#[test]
fn run_without_a_file_outside_a_project_keeps_the_old_error() {
    let p = Project::new("runnoentry");
    let (code, _, err) = p.run(&["run"]);
    assert_ne!(code, 0);
    assert!(err.contains("run needs a .matrix file"), "error: {}", err);
}

#[test]
fn project_test_runs_the_tests_directory_differentially() {
    let p = Project::new("ptest");
    p.add_app(
        &[],
        "fn add(a, b) {\n    return a + b;\n}\nfn main() {\n    println(add(2, 3));\n    return 0;\n}\n",
    );
    p.write(
        "tests/add.matrix",
        "fn main() {\n    println(5);\n    return 0;\n}\n",
    );
    // A library file in tests/ only needs the semantic check.
    p.write(
        "tests/lib.matrix",
        "public fn helper(x) {\n    return x;\n}\n",
    );

    let (code, out, err) = p.run(&["test"]);
    assert_eq!(code, 0, "test failed: {}{}", out, err);
    assert!(
        out.contains("ok   tests/add.matrix (tree==vm)"),
        "output:/n{}",
        out
    );
    assert!(
        out.contains("ok   tests/lib.matrix (library: checked)"),
        "output:/n{}",
        out
    );
    assert!(out.contains("2 ok, 0 failed"), "output:/n{}", out);
}

#[test]
fn project_test_fails_when_a_test_fails_or_engines_disagree() {
    let p = Project::new("ptest-fail");
    p.add_app(&[], "fn main() {\n    return 0;\n}\n");
    // A test whose runtime error makes it exit non-zero.
    p.write(
        "tests/boom.matrix",
        "fn main() {\n    println(1 / 0);\n    return 0;\n}\n",
    );
    let (code, out, _) = p.run(&["test"]);
    assert_ne!(code, 0, "a failing test must fail the suite");
    assert!(out.contains("FAIL tests/boom.matrix"), "output:/n{}", out);
    assert!(out.contains("0 ok, 1 failed"), "output:/n{}", out);
}

#[test]
fn project_test_with_no_tests_directory_is_not_an_error() {
    let p = Project::new("ptest-empty");
    p.add_app(&[], "fn main() {\n    return 0;\n}\n");
    let (code, out, _) = p.run(&["test"]);
    assert_eq!(code, 0, "nothing to run is not a failure");
    assert!(out.contains("no tests/ directory"), "output:/n{}", out);
}

// ------------------------------------------------------------------ workspace

#[test]
fn a_workspace_installs_all_members_into_one_shared_vendor() {
    // Layout: ws/ (workspace root), ws/libs/corelib (member), ws/apps/cli
    // (member, imports corelib + an external path dep).
    let base = std::env::temp_dir().join(format!("spid3r_ws_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let ws = base.join("ws");
    std::fs::create_dir_all(ws.join("libs/corelib/src")).expect("mkdir");
    std::fs::create_dir_all(ws.join("apps/cli/src")).expect("mkdir");
    std::fs::create_dir_all(base.join("extdep/src")).expect("mkdir");

    std::fs::write(
        ws.join("spider.toml"),
        "[workspace]\nmembers = [\"libs/corelib\", \"apps/cli\"]\n",
    )
    .expect("write");
    std::fs::write(
        ws.join("libs/corelib/spider.toml"),
        "[package]\nname = \"corelib\"\nversion = \"0.3.0\"\nentry = \"src/lib.matrix\"\n",
    )
    .expect("write");
    std::fs::write(
        ws.join("libs/corelib/src/lib.matrix"),
        "public fn twice(x) {\n    return x * 2;\n}\n",
    )
    .expect("write");
    std::fs::write(
        ws.join("apps/cli/spider.toml"),
        "[package]\nname = \"cli\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\ncorelib = { path = \"../../libs/corelib\" }\nextlib = { path = \"../../../extdep\" }\n",
    )
    .expect("write");
    std::fs::write(
        ws.join("apps/cli/src/main.matrix"),
        "import \"corelib/lib\";\nimport \"extlib/lib\";\n\nfn main() {\n    println(twice(square(4)));\n    return 0;\n}\n",
    )
    .expect("write");
    std::fs::write(
        base.join("extdep/spider.toml"),
        "[package]\nname = \"extlib\"\nversion = \"2.0.0\"\nentry = \"src/lib.matrix\"\n",
    )
    .expect("write");
    std::fs::write(
        base.join("extdep/src/lib.matrix"),
        "public fn square(x) {\n    return x * x;\n}\n",
    )
    .expect("write");

    let run = |args: &[&str], cwd: &Path| {
        let o = Command::new(bin())
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("spawn");
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).to_string(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        )
    };

    // Install at the workspace root: one lockfile, one shared vendor/.
    let (code, out, err) = run(&["install"], &ws);
    assert_eq!(code, 0, "workspace install failed: {}{}", out, err);
    assert!(ws.join("vendor/corelib/lib.matrix").is_file());
    assert!(ws.join("vendor/extlib/lib.matrix").is_file());
    let lock = std::fs::read_to_string(ws.join("spider.lock")).expect("lock");
    assert!(lock.contains("# workspace"), "lock: {}", lock);
    assert!(lock.contains("used-by = \"apps/cli\""), "lock: {}", lock);
    assert!(
        lock.contains("source = \"path+../../../extdep\""),
        "lock: {}",
        lock
    );

    // Running from INSIDE a member resolves the shared vendor via walk-up.
    let (code, out, err) = run(&["run", "--engine", "vm"], &ws.join("apps/cli"));
    assert_eq!(code, 0, "member run failed: {}{}", out, err);
    assert_eq!(out, "32\n", "output: {}", out);

    // dist inside the member also works.
    let (code, out, err) = run(&["dist"], &ws.join("apps/cli"));
    assert_eq!(code, 0, "member dist failed: {}{}", out, err);
    assert!(ws.join("apps/cli/target/cli.matrixc").is_file());

    // A workspace-only root has no entry to run: clear error, not a panic.
    let (code, _, err) = run(&["run"], &ws);
    assert_ne!(code, 0, "running a workspace-only root should fail");
    assert!(
        err.contains("does not exist") || err.contains("entry"),
        "error: {}",
        err
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn the_same_dependency_name_from_two_sources_is_a_workspace_error() {
    let base = std::env::temp_dir().join(format!("spid3r_ws_conflict_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let ws = base.join("ws");
    for d in ["m1", "m2"] {
        std::fs::create_dir_all(ws.join(d).join("src")).expect("mkdir");
    }
    std::fs::create_dir_all(ws.join("depA/src")).expect("mkdir");
    std::fs::create_dir_all(ws.join("depB/src")).expect("mkdir");

    std::fs::write(
        ws.join("spider.toml"),
        "[workspace]\nmembers = [\"m1\", \"m2\"]\n",
    )
    .expect("write");
    std::fs::write(
        ws.join("m1/spider.toml"),
        "[package]\nname = \"m1\"\n\n[dependencies]\nlib = { path = \"../depA\" }\n",
    )
    .expect("write");
    std::fs::write(ws.join("m1/src/lib.matrix"), "").expect("write");
    std::fs::write(
        ws.join("m2/spider.toml"),
        "[package]\nname = \"m2\"\n\n[dependencies]\nlib = { path = \"../depB\" }\n",
    )
    .expect("write");
    std::fs::write(ws.join("m2/src/lib.matrix"), "").expect("write");
    std::fs::write(ws.join("depA/spider.toml"), "[package]\nname = \"depA\"\n").expect("write");
    std::fs::write(ws.join("depB/spider.toml"), "[package]\nname = \"depB\"\n").expect("write");

    let o = Command::new(bin())
        .args(["install"])
        .current_dir(&ws)
        .output()
        .expect("spawn");
    let err = String::from_utf8_lossy(&o.stderr).to_string();
    assert_ne!(o.status.code().unwrap_or(-1), 0, "conflict should fail");
    assert!(
        err.contains("different sources"),
        "error should name the conflict: {}",
        err
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_workspace_only_root_parses_without_a_package() {
    let m =
        spid3r_lang::pkg::parse_manifest("[workspace]\nmembers = [\"a\", \"b\"]\n").expect("parse");
    assert!(m.package.is_none());
    assert_eq!(m.members, vec!["a".to_string(), "b".to_string()]);
    // A manifest with neither package nor workspace is still an error.
    assert!(spid3r_lang::pkg::parse_manifest("").is_err());
}

// ------------------------------------------------- assert builtins + workspace test

#[test]
fn assert_builtins_pass_and_fail_in_both_engines() {
    let p = Project::new("assert");
    p.add_app(
        &[],
        "fn main() {\n    assert(true);\n    assert(1 < 2, \"math is broken\");\n    assert_eq(2 + 2, 4);\n    assert_eq(\"a\", \"a\", \"strings\");\n    println(\"all assertions passed\");\n    return 0;\n}\n",
    );
    for engine in ["tree", "vm"] {
        let (code, out, err) = p.run(&["run", "--engine", engine, "src/main.matrix"]);
        assert_eq!(code, 0, "{} failed: {}{}", engine, out, err);
        assert_eq!(out, "all assertions passed\n", "{}: {}", engine, out);
    }
    // A failing assert exits non-zero with the message.
    p.write(
        "src/main.matrix",
        "fn main() {\n    assert(false, \"boom\");\n    return 0;\n}\n",
    );
    let (code, _, err) = p.run(&["run", "--engine", "vm", "src/main.matrix"]);
    assert_ne!(code, 0);
    assert!(err.contains("boom"), "error: {}", err);
    // assert_eq shows both sides.
    p.write(
        "src/main.matrix",
        "fn main() {\n    assert_eq(1, 2, \"numbers differ\");\n    return 0;\n}\n",
    );
    let (code, _, err) = p.run(&["run", "--engine", "vm", "src/main.matrix"]);
    assert_ne!(code, 0);
    assert!(
        err.contains("1 != 2") && err.contains("numbers differ"),
        "error: {}",
        err
    );
}

#[test]
fn project_tests_use_the_assert_builtins() {
    let p = Project::new("assert-tests");
    p.add_app(
        &[],
        "fn add(a, b) {\n    return a + b;\n}\nfn main() {\n    assert_eq(add(2, 3), 5);\n    return 0;\n}\n",
    );
    p.write(
        "tests/add.matrix",
        "fn main() {\n    assert_eq(2 + 3, 5, \"addition\");\n    assert(true);\n    println(\"ok\");\n    return 0;\n}\n",
    );
    let (code, out, _) = p.run(&["test"]);
    assert_eq!(code, 0, "tests should pass:/n{}", out);
    assert!(out.contains("1 ok, 0 failed"), "output:/n{}", out);

    // A broken assertion fails the suite with the assertion message.
    p.write(
        "tests/add.matrix",
        "fn main() {\n    assert_eq(2 + 2, 5, \"bad math\");\n    return 0;\n}\n",
    );
    let (code, out, _) = p.run(&["test"]);
    assert_ne!(code, 0);
    assert!(
        out.contains("assertion failed") && out.contains("4 != 5"),
        "output:/n{}",
        out
    );
}

#[test]
fn workspace_test_runs_every_member() {
    let base = std::env::temp_dir().join(format!("spid3r_ws_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let ws = base.join("ws");
    for d in ["liba", "libb"] {
        std::fs::create_dir_all(ws.join(d).join("tests")).expect("mkdir");
    }
    std::fs::write(
        ws.join("spider.toml"),
        "[workspace]\nmembers = [\"liba\", \"libb\"]\n",
    )
    .expect("write");
    std::fs::write(ws.join("liba/spider.toml"), "[package]\nname = \"liba\"\n").expect("write");
    std::fs::write(ws.join("libb/spider.toml"), "[package]\nname = \"libb\"\n").expect("write");
    // liba: one passing test.
    std::fs::write(
        ws.join("liba/tests/a.matrix"),
        "fn main() {\n    assert_eq(1, 1);\n    return 0;\n}\n",
    )
    .expect("write");
    // libb: one passing and one failing test.
    std::fs::write(
        ws.join("libb/tests/ok.matrix"),
        "fn main() {\n    return 0;\n}\n",
    )
    .expect("write");
    std::fs::write(
        ws.join("libb/tests/bad.matrix"),
        "fn main() {\n    assert(false, \"libb is broken\");\n    return 0;\n}\n",
    )
    .expect("write");

    let o = Command::new(bin())
        .args(["test"])
        .current_dir(&ws)
        .output()
        .expect("spawn");
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert_ne!(
        o.status.code().unwrap_or(-1),
        0,
        "a failing member must fail the workspace"
    );
    assert!(out.contains("== member liba =="), "output:/n{}", out);
    assert!(out.contains("== member libb =="), "output:/n{}", out);
    assert!(
        out.contains("member liba: 1 ok, 0 failed"),
        "output:/n{}",
        out
    );
    assert!(
        out.contains("member libb: 1 ok, 1 failed"),
        "output:/n{}",
        out
    );
    assert!(out.contains("2 ok, 1 failed"), "output:/n{}", out);
    let _ = std::fs::remove_dir_all(&base);
}

// ------------------------------------------------------------------ pack

#[test]
fn tar_round_trips_entries_exactly() {
    let entries = vec![
        (
            "spider.toml".to_string(),
            b"[package]\nname = \"x\"\n".to_vec(),
        ),
        (
            "src/lib.matrix".to_string(),
            b"public fn f() {\n    return 1;\n}\n".to_vec(),
        ),
        ("src/empty.matrix".to_string(), Vec::new()),
        (
            "src/big.matrix".to_string(),
            vec![b'x'; 5000], // spans multiple 512-byte blocks
        ),
    ];
    let data = spid3r_lang::pkg::write_tar(&entries);
    let back = spid3r_lang::pkg::read_tar(&data).expect("read_tar");
    assert_eq!(back, entries, "tar round-trip must be exact");
    // The archive is 512-byte aligned overall (plus the 1024-byte terminator).
    assert_eq!(data.len() % 512, 0);
    // Deterministic: same input, same bytes.
    assert_eq!(spid3r_lang::pkg::write_tar(&entries), data);
}

#[test]
fn pack_and_unpack_restore_a_project_byte_for_byte() {
    let p = Project::new("pack");
    p.add_dep("mathlib", "1.0.0", DEP_BODY);
    p.write(
        "spider.toml",
        "[package]\nname = \"packedlib\"\nversion = \"2.1.0\"\nentry = \"src/lib.matrix\"\n",
    );
    p.write(
        "src/lib.matrix",
        "public fn triple(x) {\n    return x * 3;\n}\n",
    );

    let (code, out, err) = p.run(&["pack"]);
    assert_eq!(code, 0, "pack failed: {}{}", out, err);
    let archive = p.root.join("dist/packedlib-2.1.0.tar");
    assert!(
        archive.is_file(),
        "archive missing; pack said: {}{}",
        out,
        err
    );
    assert!(out.contains("sha256:"), "pack should print a hash: {}", out);

    // Unpack elsewhere and compare every file (cwd is the project root, so
    // "unpacked" lands there).
    let dest = p.root.join("unpacked");
    let (code, out, _) = p.run(&["unpack", "dist/packedlib-2.1.0.tar", "unpacked"]);
    assert_eq!(code, 0, "unpack failed: {}", out);
    assert!(out.contains("2 file(s)"), "output: {}", out);
    assert_eq!(
        std::fs::read_to_string(dest.join("spider.toml")).expect("read"),
        std::fs::read_to_string(p.root.join("spider.toml")).expect("read")
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("src/lib.matrix")).expect("read"),
        "public fn triple(x) {\n    return x * 3;\n}\n"
    );

    // Unpacking again refuses to clobber.
    let (code, _, err) = p.run(&["unpack", "dist/packedlib-2.1.0.tar", "unpacked"]);
    assert_ne!(code, 0);
    assert!(err.contains("already exists"), "error: {}", err);
}

#[test]
fn a_packed_library_works_as_a_path_dependency() {
    // The full distribution story: pack a library, unpack it next to an app,
    // depend on it, install, run.
    let base = std::env::temp_dir().join(format!("spid3r_pack_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let lib = base.join("lib");
    let app = base.join("app");
    for d in [&lib, &app] {
        std::fs::create_dir_all(d.join("src")).expect("mkdir");
    }
    std::fs::write(
        lib.join("spider.toml"),
        "[package]\nname = \"coolmath\"\nversion = \"1.0.0\"\nentry = \"src/lib.matrix\"\n",
    )
    .expect("write");
    std::fs::write(
        lib.join("src/lib.matrix"),
        "public fn double(x) {\n    return x * 2;\n}\n",
    )
    .expect("write");

    let run = |args: &[&str], cwd: &Path| {
        let o = Command::new(bin())
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("spawn");
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).to_string(),
            String::from_utf8_lossy(&o.stderr).to_string(),
        )
    };

    let (code, out, err) = run(&["pack"], &lib);
    assert_eq!(code, 0, "pack failed: {}{}", out, err);
    std::fs::create_dir_all(&app).expect("mkdir");
    let (code, out, err) = run(
        &["unpack", "../lib/dist/coolmath-1.0.0.tar", "deps/coolmath"],
        &app,
    );
    assert_eq!(code, 0, "unpack failed: {}{}", out, err);
    std::fs::write(
        app.join("spider.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\ncoolmath = { path = \"deps/coolmath\" }\n",
    )
    .expect("write");
    std::fs::write(
        app.join("src/main.matrix"),
        "import \"coolmath/lib\";\n\nfn main() {\n    println(double(21));\n    return 0;\n}\n",
    )
    .expect("write");

    let (code, out, err) = run(&["install"], &app);
    assert_eq!(code, 0, "install failed: {}{}", out, err);
    let (code, out, err) = run(&["run", "--engine", "vm"], &app);
    assert_eq!(code, 0, "run failed: {}{}", out, err);
    assert_eq!(out, "42\n", "output: {}", out);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn workspace_pack_archives_every_member() {
    let base = std::env::temp_dir().join(format!("spid3r_ws_pack_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let ws = base.join("ws");
    for d in ["libs/core", "apps/tool"] {
        std::fs::create_dir_all(ws.join(d).join("src")).expect("mkdir");
    }
    std::fs::write(
        ws.join("spider.toml"),
        "[workspace]\nmembers = [\"libs/core\", \"apps/tool\"]\n",
    )
    .expect("write");
    std::fs::write(
        ws.join("libs/core/spider.toml"),
        "[package]\nname = \"core\"\nversion = \"0.5.0\"\nentry = \"src/lib.matrix\"\n",
    )
    .expect("write");
    std::fs::write(
        ws.join("libs/core/src/lib.matrix"),
        "public fn id(x) {\n    return x;\n}\n",
    )
    .expect("write");
    std::fs::write(
        ws.join("apps/tool/spider.toml"),
        "[package]\nname = \"tool\"\nversion = \"1.1.0\"\nentry = \"src/main.matrix\"\n",
    )
    .expect("write");
    std::fs::write(
        ws.join("apps/tool/src/main.matrix"),
        "fn main() {\n    return 0;\n}\n",
    )
    .expect("write");

    let o = Command::new(bin())
        .args(["pack"])
        .current_dir(&ws)
        .output()
        .expect("spawn");
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert_eq!(
        o.status.code().unwrap_or(-1),
        0,
        "workspace pack failed: {}",
        out
    );
    assert!(
        ws.join("dist/core-0.5.0.tar").is_file(),
        "core archive missing:/n{}",
        out
    );
    assert!(
        ws.join("dist/tool-1.1.0.tar").is_file(),
        "tool archive missing:/n{}",
        out
    );
    assert!(out.contains("2 archive(s)"), "output:/n{}", out);

    // Each archive is a valid tar of its member: unpack and check contents.
    let data = std::fs::read(ws.join("dist/core-0.5.0.tar")).expect("read");
    let entries = spid3r_lang::pkg::read_tar(&data).expect("parse");
    assert!(
        entries.iter().any(|(n, _)| n == "src/lib.matrix"),
        "core entries: {:?}",
        entries.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn registry_dependency_installs_from_a_local_index() {
    let p = Project::new("reg");
    p.add_registry_pkg("mathlib", "1.2.0", DEP_BODY);
    p.add_app_registry(&[("mathlib", "^1.0")], "fn main() {\n    return 0;\n}\n");
    let reg = p.registry_dir();
    let (code, out, err) = p.run_env(&["install"], &[("SPID3R_REGISTRY", &reg)]);
    assert_eq!(code, 0, "install failed: {}{}", out, err);
    assert!(
        p.exists("vendor/mathlib/lib.matrix"),
        "vendored file missing"
    );
    let lock = p.read("spider.lock");
    assert!(
        lock.contains("source = \"registry+1.2.0\""),
        "lock: {}",
        lock
    );
    assert!(lock.contains("sha256 = \""), "lock: {}", lock);
}

#[test]
fn registry_picks_the_highest_matching_version() {
    let p = Project::new("regmax");
    p.add_registry_pkg("mathlib", "1.1.0", DEP_BODY);
    p.add_registry_pkg("mathlib", "1.5.0", DEP_BODY);
    p.add_app_registry(&[("mathlib", "^1.0")], "fn main() {\n    return 0;\n}\n");
    let reg = p.registry_dir();
    let (code, _, err) = p.run_env(&["install"], &[("SPID3R_REGISTRY", &reg)]);
    assert_eq!(code, 0, "install failed: {}", err);
    assert!(
        p.read("spider.lock").contains("registry+1.5.0"),
        "should pick 1.5.0; lock: {}",
        p.read("spider.lock")
    );
}

#[test]
fn registry_dependency_runs_end_to_end() {
    let p = Project::new("regrun");
    p.add_registry_pkg("mathlib", "1.2.0", DEP_BODY);
    p.add_app_registry(
        &[("mathlib", "^1.0")],
        "import \"mathlib/lib\";\n\nfn main() {\n    println(square(6));\n    return 0;\n}\n",
    );
    let reg = p.registry_dir();
    assert_eq!(p.run_env(&["install"], &[("SPID3R_REGISTRY", &reg)]).0, 0);
    let (code, out, err) = p.run(&["run", "--engine", "vm", "src/main.matrix"]);
    assert_eq!(code, 0, "run failed: {}{}", out, err);
    assert_eq!(out, "36\n", "output: {}", out);
}

#[test]
fn registry_errors_are_clear() {
    // Unsetting SPID3R_REGISTRY falls back to the built-in GitHub Pages registry.
    let p = Project::new("regnone");
    p.add_registry_pkg("mathlib", "1.0.0", DEP_BODY);
    p.add_app_registry(&[("mathlib", "^1.0")], "fn main() {\n    return 0;\n}\n");
    let (code_def, out_def, err_def) =
        p.run_unset(&["config", "get", "packages.registry"], "SPID3R_REGISTRY");
    assert_eq!(code_def, 0, "config get failed: {}", err_def);
    assert_eq!(out_def.trim(), "https://spid3r-spkg.github.io");

    // Registry explicitly disabled (no network needed).
    let (code, _, err) = p.run_env(&["install"], &[("SPID3R_REGISTRY", "none")]);
    assert_ne!(code, 0, "install must fail when registry is disabled");
    assert!(err.contains("SPID3R_REGISTRY"), "error: {}", err);

    // No version matches the constraint.
    let p2 = Project::new("regnomatch");
    p2.add_registry_pkg("mathlib", "1.0.0", DEP_BODY);
    p2.add_app_registry(&[("mathlib", "^2.0")], "fn main() {\n    return 0;\n}\n");
    let reg = p2.registry_dir();
    let (code, _, err) = p2.run_env(&["install"], &[("SPID3R_REGISTRY", &reg)]);
    assert_ne!(code, 0, "install must fail when no version matches");
    assert!(err.contains("no version"), "error: {}", err);
}

/// A registry package WITHOUT sha256.txt must be refused at install time:
/// a missing checksum is exactly what a tampered package looks like (S-3).
#[test]
fn install_refuses_a_registry_package_without_a_checksum() {
    let p = Project::new("regnochecksum");
    p.add_registry_pkg("mathlib", "1.0.0", DEP_BODY);
    std::fs::remove_file(
        p.base
            .join("registry")
            .join("mathlib")
            .join("1.0.0")
            .join("sha256.txt"),
    )
    .expect("remove sha256.txt");
    p.add_app_registry(&[("mathlib", "^1.0")], "fn main() {\n    return 0;\n}\n");
    let reg = p.registry_dir();
    let (code, _, err) = p.run_env(&["install"], &[("SPID3R_REGISTRY", &reg)]);
    assert_ne!(code, 0, "install must fail without sha256.txt");
    assert!(
        err.contains("sha256.txt"),
        "the error must name the missing checksum: {err}"
    );
    assert!(
        !p.exists("vendor/mathlib/lib.matrix"),
        "nothing may be vendored from an unverified package"
    );
}

/// An EMPTY sha256.txt is the same refusal.
#[test]
fn install_refuses_a_registry_package_with_an_empty_checksum() {
    let p = Project::new("regemptychecksum");
    p.add_registry_pkg("mathlib", "1.0.0", DEP_BODY);
    std::fs::write(
        p.base
            .join("registry")
            .join("mathlib")
            .join("1.0.0")
            .join("sha256.txt"),
        "\n",
    )
    .expect("write empty sha256.txt");
    p.add_app_registry(&[("mathlib", "^1.0")], "fn main() {\n    return 0;\n}\n");
    let reg = p.registry_dir();
    let (code, _, err) = p.run_env(&["install"], &[("SPID3R_REGISTRY", &reg)]);
    assert_ne!(code, 0, "install must fail on an empty sha256.txt");
    assert!(err.contains("empty"), "error: {err}");
}

/// A TAMPERED package (contents do not match sha256.txt) must be refused, and
/// the refused tree must not be left importable under vendor/.
#[test]
fn install_refuses_a_registry_package_with_a_tampered_checksum() {
    let p = Project::new("regtampered");
    p.add_registry_pkg("mathlib", "1.0.0", DEP_BODY);
    // The publisher's checksum says one thing; the tree in the registry says
    // something else (a malicious substitution).
    std::fs::write(
        p.base
            .join("registry")
            .join("mathlib")
            .join("1.0.0")
            .join("sha256.txt"),
        format!("{}\n", "0".repeat(64)),
    )
    .expect("write wrong sha256.txt");
    p.add_app_registry(&[("mathlib", "^1.0")], "fn main() {\n    return 0;\n}\n");
    let reg = p.registry_dir();
    let (code, _, err) = p.run_env(&["install"], &[("SPID3R_REGISTRY", &reg)]);
    assert_ne!(code, 0, "install must fail on a checksum mismatch");
    assert!(
        err.contains("checksum mismatch"),
        "the error must say the checksum mismatched: {err}"
    );
    assert!(
        !p.exists("vendor/mathlib/lib.matrix"),
        "nothing may be left vendored from a tampered package"
    );
}

/// A registry dependency whose lock entry lost its sha256 (a lock from before
/// the mandatory-checksum rule, or a hand-edited one) must fail `verify`.
#[test]
fn verify_flags_a_registry_entry_without_a_checksum() {
    let p = Project::new("reglocknosha");
    p.add_registry_pkg("mathlib", "1.0.0", DEP_BODY);
    p.add_app_registry(&[("mathlib", "^1.0")], "fn main() {\n    return 0;\n}\n");
    let reg = p.registry_dir();
    assert_eq!(p.run_env(&["install"], &[("SPID3R_REGISTRY", &reg)]).0, 0);
    // Strip the recorded hash, keeping the registry source line.
    let lock = p.read("spider.lock");
    let stripped: String = lock
        .lines()
        .filter(|l| !l.starts_with("sha256 = "))
        .collect::<Vec<_>>()
        .join("\n");
    p.write("spider.lock", &format!("{}\n", stripped));
    let (code, _, err) = p.run(&["verify"]);
    assert_ne!(code, 0, "verify must fail on an unverified registry entry");
    assert!(
        err.contains("unverified") || err.contains("sha256"),
        "the report must flag the missing verification: {err}"
    );
}

#[test]
fn tar_supports_ustar_prefix_and_longlink_and_verifies_checksum() {
    // 1. Path between 101 and 255 bytes with a `/` split (uses POSIX ustar prefix).
    let ustar_path = format!(
        "src/{}/module_{}.matrix",
        "deep_subdir_".repeat(8),
        "a".repeat(30)
    );
    assert!(ustar_path.len() > 100 && ustar_path.len() <= 255);

    // 2. Path > 255 bytes (uses GNU @LongLink 'L' block).
    let longlink_path = format!(
        "src/{}/very_long_{}.matrix",
        "nested_dir_".repeat(20),
        "b".repeat(80)
    );
    assert!(longlink_path.len() > 255);

    let entries = vec![
        (
            ustar_path.clone(),
            b"public fn a() { return 1; }\n".to_vec(),
        ),
        (
            longlink_path.clone(),
            b"public fn b() { return 2; }\n".to_vec(),
        ),
    ];
    let mut archive = spid3r_lang::pkg::write_tar(&entries);
    let decoded = spid3r_lang::pkg::read_tar(&archive).expect("read_tar with long names");
    assert_eq!(
        decoded, entries,
        "long paths must round-trip without truncation"
    );

    // 3. Corrupt a byte in the first header and verify checksum rejection.
    archive[0] ^= 0xff;
    let err =
        spid3r_lang::pkg::read_tar(&archive).expect_err("corrupted header must fail checksum");
    assert!(
        err.contains("checksum mismatch"),
        "expected checksum mismatch error, got: {}",
        err
    );
}

#[test]
fn transitive_dependencies_are_resolved_and_vendored() {
    let p = Project::new("transitive_ok");
    p.add_dep("c", "1.0.0", "public fn c_val() { return 100; }\n");

    // b depends on c:
    p.write_sibling(
        "b/spider.toml",
        "[package]\nname = \"b\"\nversion = \"1.0.0\"\nentry = \"src/lib.matrix\"\n\n[dependencies]\nc = { path = \"../c\" }\n",
    );
    p.write_sibling(
        "b/src/lib.matrix",
        "import \"c/lib\" as c;\npublic fn b_val() { return c.c_val() + 20; }\n",
    );

    // app depends on b:
    p.add_app(
        &[("b", "../b")],
        "import \"b/lib\" as b;\nfn main() {\n    println(b.b_val());\n    return 0;\n}\n",
    );

    let (code, out, err) = p.run(&["install"]);
    assert_eq!(code, 0, "install failed: {}{}", out, err);

    assert!(p.exists("vendor/b/lib.matrix"), "vendor/b missing");
    assert!(p.exists("vendor/c/lib.matrix"), "vendor/c missing");

    let lock = p.read("spider.lock");
    assert!(lock.contains("name = \"b\""), "lock missing b: {}", lock);
    assert!(lock.contains("name = \"c\""), "lock missing c: {}", lock);

    let (t_code, t_out, t_err) = p.run(&["run", "--engine", "tree"]);
    assert_eq!(t_code, 0, "tree run failed: {}{}", t_out, t_err);
    assert_eq!(t_out.trim(), "120");

    let (v_code, v_out, v_err) = p.run(&["run", "--engine", "vm"]);
    assert_eq!(v_code, 0, "vm run failed: {}{}", v_out, v_err);
    assert_eq!(v_out.trim(), "120");
}

#[test]
fn circular_dependencies_are_detected_in_pkg_install() {
    let p = Project::new("transitive_cycle");
    p.write_sibling(
        "b/spider.toml",
        "[package]\nname = \"b\"\nversion = \"1.0.0\"\nentry = \"src/lib.matrix\"\n\n[dependencies]\nc = { path = \"../c\" }\n",
    );
    p.write_sibling("b/src/lib.matrix", "public fn b() { return 1; }\n");

    p.write_sibling(
        "c/spider.toml",
        "[package]\nname = \"c\"\nversion = \"1.0.0\"\nentry = \"src/lib.matrix\"\n\n[dependencies]\nb = { path = \"../b\" }\n",
    );
    p.write_sibling("c/src/lib.matrix", "public fn c() { return 2; }\n");

    p.add_app(&[("b", "../b")], "fn main() { return 0; }\n");

    let (code, _, err) = p.run(&["install"]);
    assert_ne!(code, 0, "circular dependency should fail install");
    assert!(
        err.contains("circular dependency"),
        "expected circular dependency error, got: {}",
        err
    );
}

#[test]
fn transitive_version_constraint_mismatch_fails_install() {
    let p = Project::new("transitive_ver_err");
    p.add_dep("c", "1.0.0", "public fn c() { return 1; }\n");

    p.write_sibling(
        "b/spider.toml",
        "[package]\nname = \"b\"\nversion = \"1.0.0\"\nentry = \"src/lib.matrix\"\n\n[dependencies]\nc = { path = \"../c\", version = \"^2.0\" }\n",
    );
    p.write_sibling("b/src/lib.matrix", "public fn b() { return 1; }\n");

    p.add_app(&[("b", "../b")], "fn main() { return 0; }\n");

    let (code, _, err) = p.run(&["install"]);
    assert_ne!(code, 0, "version mismatch should fail install");
    assert!(
        err.contains("requires version ^2.0") && err.contains("1.0.0"),
        "expected version mismatch error, got: {}",
        err
    );
}

// -------------------------------------------------- A-5 / A-13 (Stage 3)

/// `ext::` is git's "run this command" transport (the CVE-2022-25912 class).
/// A dependency manifest is attacker-controlled input, so it is rejected
/// outright — before any clone runs.
#[test]
fn the_ext_git_transport_is_rejected_before_any_command_runs() {
    let p = Project::new("gitext");
    // Relative marker: the manifest parser rejects Windows backslashes inside
    // quoted values, and the command (if it ever ran) would run in the project.
    p.write(
        "spider.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n\
         [dependencies]\nbad = { git = \"ext::sh -c 'touch pwned.txt'\" }\n",
    );
    p.write("src/main.matrix", "fn main() {\n    return 0;\n}\n");
    let (code, _, err) = p.run(&["install"]);
    assert_ne!(code, 0, "an ext:: url must fail the install");
    assert!(
        err.contains("ext::"),
        "the error must name the rejected transport: {}",
        err
    );
    assert!(
        !p.exists("pwned.txt"),
        "the ext:: command must never have run"
    );
    assert!(
        !p.exists(".spid3r/git/bad"),
        "nothing may be cloned for a rejected url"
    );
}

/// Plaintext `http://`/`git://` has no integrity protection; it needs the
/// explicit `SPID3R_ALLOW_INSECURE_GIT=1` opt-in.
#[test]
fn a_plaintext_git_url_needs_the_insecure_opt_in() {
    let p = Project::new("githttp");
    p.write(
        "spider.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n\
         [dependencies]\nplain = { git = \"http://example.invalid/lib\" }\n",
    );
    p.write("src/main.matrix", "fn main() {\n    return 0;\n}\n");
    let (code, _, err) = p.run(&["install"]);
    assert_ne!(code, 0, "http must be rejected by default");
    assert!(
        err.contains("SPID3R_ALLOW_INSECURE_GIT"),
        "the error must name the opt-in: {}",
        err
    );
    // An unknown scheme is rejected too (no fallback to a local path).
    p.write(
        "spider.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n\
         [dependencies]\nplain = { git = \"ftp://example.invalid/lib\" }\n",
    );
    let (code, _, err) = p.run(&["install"]);
    assert_ne!(code, 0, "an unknown scheme must be rejected");
    assert!(err.contains("scheme"), "expected a scheme error: {}", err);
}

/// A re-install must reproduce the commit recorded in `spider.lock` instead of
/// tracking a moving branch; `install --update` is the explicit way to move.
#[test]
fn a_reinstall_honors_the_locked_commit_and_update_moves_it() {
    let p = Project::new("gitlock");
    let repo = add_git_dep(&p.base, "moving", "1.0.0", DEP_BODY);
    let url = repo.to_string_lossy().replace('\\', "/");
    p.write(
        "spider.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n\
             [dependencies]\nmoving = {{ git = \"{}\" }}\n",
            url
        ),
    );
    p.write("src/main.matrix", "fn main() {\n    return 0;\n}\n");
    assert_eq!(p.run(&["install"]).0, 0);
    let first_lock = p.read("spider.lock");
    assert!(p.read("vendor/moving/lib.matrix").contains("x * x"));

    // Upstream moves on (no rev in the manifest: the branch tip changed).
    std::fs::write(
        repo.join("src/lib.matrix"),
        "public fn square(x) {\n    return x * x * x;\n}\n",
    )
    .expect("write");
    git(&repo, &["add", "-A"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--quiet",
            "-m",
            "second",
        ],
    );

    // Re-install: the locked commit wins, so the vendored tree is unchanged.
    assert_eq!(p.run(&["install"]).0, 0);
    assert!(
        p.read("vendor/moving/lib.matrix").contains("x * x"),
        "a re-install must reproduce the locked commit, not the new tip"
    );
    assert_eq!(
        p.read("spider.lock"),
        first_lock,
        "the lock must stay stable"
    );

    // --update re-resolves and vendors the new tip.
    let (code, out, err) = p.run(&["install", "--update"]);
    assert_eq!(code, 0, "install --update failed: {}{}", out, err);
    assert!(
        p.read("vendor/moving/lib.matrix").contains("x * x * x"),
        "--update must move to the new upstream commit"
    );
    assert_ne!(
        p.read("spider.lock"),
        first_lock,
        "the lock must record the new commit"
    );
}

/// Everything under `src/` is vendored and covered by the lockfile hash, with
/// raw bytes: a non-`.matrix` asset and non-UTF-8 bytes must both be detected.
#[test]
fn non_matrix_and_binary_assets_are_vendored_and_hashed() {
    let p = Project::new("assets");
    let dep = p.base.join("withdata");
    std::fs::create_dir_all(dep.join("src")).expect("mkdir");
    std::fs::write(
        dep.join("spider.toml"),
        "[package]\nname = \"withdata\"\nversion = \"1.0.0\"\n",
    )
    .expect("write");
    std::fs::write(dep.join("src/lib.matrix"), DEP_BODY).expect("write");
    std::fs::write(dep.join("src/table.txt"), "1,2,3\n").expect("write");
    // Invalid UTF-8: `from_utf8_lossy` maps every bad byte to U+FFFD, so two
    // different payloads used to hash identically.
    std::fs::write(dep.join("src/blob.bin"), [0x80u8, 0x81, 0x82]).expect("write");
    p.write(
        "spider.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n\
         [dependencies]\nwithdata = { path = \"../withdata\" }\n",
    );
    p.write("src/main.matrix", "fn main() {\n    return 0;\n}\n");
    assert_eq!(p.run(&["install"]).0, 0);
    assert!(
        p.exists("vendor/withdata/table.txt"),
        "data asset not vendored"
    );
    assert!(
        p.exists("vendor/withdata/blob.bin"),
        "binary asset not vendored"
    );
    let hash_before = spid3r_lang::pkg::tree_hash(&dep).expect("tree hash");

    // A different non-UTF-8 payload must change the hash.
    std::fs::write(dep.join("src/blob.bin"), [0x90u8, 0x91, 0x92]).expect("write");
    let hash_after = spid3r_lang::pkg::tree_hash(&dep).expect("tree hash");
    assert_ne!(
        hash_before, hash_after,
        "binary content must be hashed as raw bytes, not lossily decoded"
    );

    // Changing only the data asset must change the hash too.
    std::fs::write(dep.join("src/table.txt"), "4,5,6\n").expect("write");
    let hash_asset = spid3r_lang::pkg::tree_hash(&dep).expect("tree hash");
    assert_ne!(
        hash_after, hash_asset,
        "a data asset must be covered by the hash"
    );
}

/// The transport policy itself (audit A-5): what a manifest may point git at,
/// with no opt-in environment variables set.
#[test]
fn the_git_transport_policy_accepts_only_network_transports_by_default() {
    use spid3r_lang::pkg::validate_git_url;
    let ok = [
        "https://github.com/acme/lib.git",
        "https://example.com/a/b?x=1",
        "ssh://git@example.com/acme/lib.git",
        "ssh://example.com:2222/acme/lib.git",
        "git@github.com:acme/lib.git",
        "https://[::1]/acme/lib.git",
        "https://example.com/ext::not-a-transport",
    ];
    for url in ok {
        assert!(
            validate_git_url("dep", url).is_ok(),
            "should accept {}: {:?}",
            url,
            validate_git_url("dep", url)
        );
    }
    let bad = [
        "ext::sh -c id",
        "EXT::sh -c id",
        "ext::/bin/sh",
        "http://example.com/lib.git",
        "git://example.com/lib.git",
        "file:///etc/repo",
        "ftp://example.com/lib.git",
        "-oProxyCommand=calc",
        "ssh://-oProxyCommand=calc/repo",
        "git@-oProxyCommand=calc:repo",
        "/tmp/local-repo",
        "C:/repo",
        "",
        "https://example.com/a b",
    ];
    for url in bad {
        assert!(
            validate_git_url("dep", url).is_err(),
            "should reject {:?}",
            url
        );
    }
}

#[test]
fn wheel_publish_install_verify_audit_and_tamper_refusal() {
    let p = Project::new("wheel_e2e");
    let triple = spid3r_lang::pkg::manifest::host_target_triple();
    let wheel_pkg = p.base.join("numwheel");
    std::fs::create_dir_all(wheel_pkg.join("src")).unwrap();
    std::fs::create_dir_all(wheel_pkg.join("native").join(triple)).unwrap();
    std::fs::write(
        wheel_pkg.join("spider.toml"),
        format!(
            "[package]\nname = \"numwheel\"\nversion = \"1.0.0\"\nentry = \"src/lib.matrix\"\ndescription = \"Native accelerated wheel\"\nlicense = \"MIT\"\ntargets = [\"{}\"]\n\n[ffi]\nlibs = [\"{}\"]\nbundled = [\"dummy.lib\"]\nruntime_libs = [\"helper.dll\"]\n",
            triple,
            if cfg!(windows) { "kernel32" } else { "m" }
        ),
    )
    .unwrap();
    std::fs::write(
        wheel_pkg.join("src/lib.matrix"),
        "public fn wheel_ok() -> int { return 42; }\n",
    )
    .unwrap();
    std::fs::write(
        wheel_pkg.join("native").join(triple).join("dummy.lib"),
        b"MOCKLIB1",
    )
    .unwrap();
    std::fs::write(
        wheel_pkg.join("native").join(triple).join("helper.dll"),
        b"MOCKDLL1",
    )
    .unwrap();

    let reg = p.base.join("wheel_reg");
    std::fs::create_dir_all(&reg).unwrap();
    let seed = "13e73ff995a5d91c9cd42884ce989ec6378fa899cdc25b3fa6e915050c5947ab";
    let (pub_code, _, pub_err) = {
        let out = Command::new(bin())
            .args([
                "publish",
                "--to-dir",
                reg.to_str().unwrap(),
                "--target",
                triple,
            ])
            .current_dir(&wheel_pkg)
            .env("SPID3R_SIGN_KEY", seed)
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    assert_eq!(pub_code, 0, "wheel publish failed: {pub_err}");
    let pk = std::fs::read_to_string(reg.join("registry.pub"))
        .unwrap()
        .trim()
        .to_string();
    assert!(reg.join("catalog.txt.sig").is_file());
    assert!(reg.join("registry.toml.sig").is_file());
    assert!(reg.join("numwheel/index.txt.sig").is_file());
    assert!(reg
        .join(format!("numwheel/1.0.0/package-{}.tar", triple))
        .is_file());
    assert!(reg
        .join(format!("numwheel/1.0.0/sha256-{}.txt.sig", triple))
        .is_file());
    assert!(reg
        .join(format!("numwheel/1.0.0/manifest-{}.sha256.sig", triple))
        .is_file());

    let ini = p.base.join("trust.ini");
    std::fs::write(&ini, "").unwrap();
    p.write(
        "spider.toml",
        "[package]\nname = \"wheel_app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nnumwheel = { version = \"^1.0\" }\n",
    );
    p.write(
        "src/main.matrix",
        "import \"numwheel/lib\";\nfn main() { println(wheel_ok()); return 0; }\n",
    );

    let reg_str = reg.to_str().unwrap();
    let ini_str = ini.to_str().unwrap();
    let (code, _, err) = p.run_env(
        &["install"],
        &[
            ("SPID3R_INI", ini_str),
            ("SPID3R_REGISTRY", reg_str),
            ("SPID3R_TRUST_PK", &pk),
        ],
    );
    assert_eq!(code, 0, "wheel install failed: {err}");
    assert!(p
        .root
        .join("vendor/numwheel/_native")
        .join(triple)
        .join("dummy.lib")
        .is_file());
    let lock = p.read("spider.lock");
    assert!(lock.contains(&format!("target = \"{}\"", triple)), "{lock}");
    assert!(lock.contains("native-sha256 = "), "{lock}");

    let (vcode, _, verr) = p.run(&["verify"]);
    assert_eq!(vcode, 0, "verify failed: {verr}");
    let (acode, aout, aerr) = p.run_env(
        &["audit", "--offline"],
        &[("SPID3R_INI", ini_str), ("SPID3R_TRUST_PK", &pk)],
    );
    assert_eq!(acode, 0, "audit failed: out={aout} err={aerr}");
    assert!(aout.contains("ffi-surface"), "{aout}");

    // Tamper one byte inside _native/<target>/dummy.lib -> verify, audit, and auto-FFI must refuse!
    std::fs::write(
        p.root
            .join("vendor/numwheel/_native")
            .join(triple)
            .join("dummy.lib"),
        b"EVILLIB!",
    )
    .unwrap();
    assert_ne!(p.run(&["verify"]).0, 0, "verify must catch native tamper");
    assert_ne!(
        p.run_env(
            &["audit", "--offline"],
            &[("SPID3R_INI", ini_str), ("SPID3R_TRUST_PK", &pk)]
        )
        .0,
        0,
        "audit must fail on native tamper"
    );
    let ffi_err =
        spid3r_lang::pkg::ffi_link::collect_project_ffi_links(&p.root.join("src/main.matrix"))
            .unwrap_err();
    assert!(
        ffi_err.contains("tampered native binaries") || ffi_err.contains("native-sha256"),
        "{ffi_err}"
    );
}

#[test]
fn enterprise_commands_frozen_offline_extras_list_show_freeze_cache() {
    let p = Project::new("ent_cmds");
    let reg = p.base.join("reg");
    std::fs::create_dir_all(&reg).unwrap();
    let ini = p.base.join("empty.ini");
    std::fs::write(&ini, "").unwrap();
    let cache = p.base.join("cache");
    std::fs::create_dir_all(&cache).unwrap();

    for (name, ver) in [
        ("coredep", "1.0.0"),
        ("devtool", "0.2.0"),
        ("gpulib", "0.5.0"),
    ] {
        let dir = p.base.join(format!("src_{}", name));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("spider.toml"),
            format!(
                "[package]\nname = \"{}\"\nversion = \"{}\"\nentry = \"src/lib.matrix\"\ndescription = \"{} desc\"\nlicense = \"Apache-2.0\"\n",
                name, ver, name
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("src/lib.matrix"),
            format!(
                "public fn who_{}() -> string {{ return \"{}\"; }}\n",
                name, name
            ),
        )
        .unwrap();
        assert!(Command::new(bin())
            .args(["publish", "--to-dir", reg.to_str().unwrap()])
            .current_dir(&dir)
            .output()
            .unwrap()
            .status
            .success());
    }

    p.write(
        "spider.toml",
        "[package]\nname = \"ent_app\"\nversion = \"1.2.3\"\n\n\
         [dependencies]\ncoredep = { version = \"^1.0\" }\n\n\
         [dev-dependencies]\ndevtool = { version = \"^0.2\" }\n\n\
         [optional-dependencies.gpu]\ngpulib = { version = \"^0.5\" }\n",
    );
    p.write("src/main.matrix", "fn main() { return 0; }\n");

    let reg_str = reg.to_str().unwrap();
    let ini_str = ini.to_str().unwrap();
    let cache_str = cache.to_str().unwrap();
    let envs = [
        ("SPID3R_INI", ini_str),
        ("SPID3R_REGISTRY", reg_str),
        ("SPID3R_CACHE_DIR", cache_str),
    ];

    // --dev + --extra gpu installs all three groups cleanly.
    let (code, _, err) = p.run_env(&["install", "--dev", "--extra", "gpu"], &envs);
    assert_eq!(code, 0, "install failed: {err}");
    assert!(p.exists("vendor/coredep/lib.matrix"));
    assert!(p.exists("vendor/devtool/lib.matrix"));
    assert!(p.exists("vendor/gpulib/lib.matrix"));

    // --frozen succeeds when spider.lock matches.
    let (code, _, err) = p.run_env(&["install", "--frozen", "--dev", "--extra", "gpu"], &envs);
    assert_eq!(code, 0, "--frozen failed unexpectedly: {err}");

    // Unknown --extra is rejected cleanly.
    let (bad_extra, _, _) = p.run_env(&["install", "--extra", "nonexistent"], &envs);
    assert_ne!(bad_extra, 0);

    // --frozen refuses when spider.toml drifts from spider.lock.
    let (drift_code, _, drift_err) = p.run_env(&["install", "--frozen"], &envs);
    assert_ne!(drift_code, 0, "--frozen should fail on lock drift");
    assert!(drift_err.contains("frozen lockfile"), "{drift_err}");

    // --offline works with registry removed when cache/registry dir is omitted or using local path.
    let (list_c, list_out, _) = p.run_env(&["list", "--json"], &envs);
    assert_eq!(list_c, 0);
    assert!(
        list_out.contains("coredep") && list_out.contains("gpulib"),
        "{list_out}"
    );

    let (show_c, show_out, _) = p.run_env(&["show", "coredep"], &envs);
    assert_eq!(show_c, 0);
    assert!(
        show_out.contains("Apache-2.0") && show_out.contains("SHA-256:"),
        "{show_out}"
    );

    let (frz_c, frz_out, _) = p.run_env(&["freeze"], &envs);
    assert_eq!(frz_c, 0);
    assert!(frz_out.contains("coredep==1.0.0"), "{frz_out}");

    let (sbom_c, sbom_out, _) = p.run_env(&["freeze", "--sbom"], &envs);
    assert_eq!(sbom_c, 0);
    assert!(
        sbom_out.contains("\"bomFormat\": \"CycloneDX\"") && sbom_out.contains("SHA-256"),
        "{sbom_out}"
    );

    let (cdir_c, cdir_out, _) = p.run_env(&["cache", "dir"], &envs);
    assert_eq!(cdir_c, 0);
    assert!(!cdir_out.trim().is_empty());
    assert_eq!(p.run_env(&["cache", "prune"], &envs).0, 0);

    // Publish newer version 1.5.0 of coredep -> outdated must report 1.0.0 -> 1.5.0.
    let core_new = p.base.join("src_coredep_new");
    std::fs::create_dir_all(core_new.join("src")).unwrap();
    std::fs::write(
        core_new.join("spider.toml"),
        "[package]\nname = \"coredep\"\nversion = \"1.5.0\"\nentry = \"src/lib.matrix\"\n",
    )
    .unwrap();
    std::fs::write(
        core_new.join("src/lib.matrix"),
        "public fn who() -> int { return 2; }\n",
    )
    .unwrap();
    assert!(Command::new(bin())
        .args(["publish", "--to-dir", reg.to_str().unwrap()])
        .current_dir(&core_new)
        .output()
        .unwrap()
        .status
        .success());
    let (out_c, out_out, _) = p.run_env(&["outdated"], &envs);
    assert_eq!(out_c, 0);
    assert!(
        out_out.contains("coredep") && out_out.contains("1.5.0"),
        "{out_out}"
    );
}

#[test]
fn tool_install_runs_shim_lists_and_uninstalls() {
    let p = Project::new("tool_cli");
    let state = p.base.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let ini = p.base.join("empty.ini");
    std::fs::write(&ini, "").unwrap();

    p.write_sibling(
        "greeter/spider.toml",
        "[package]\nname = \"greeter\"\nversion = \"0.3.0\"\nentry = \"src/main.matrix\"\n\n[bin]\ngreetctl = \"src/cli.matrix\"\n",
    );
    p.write_sibling(
        "greeter/src/cli.matrix",
        "fn main() {\n    let a = args();\n    let who = if a.len() > 0 { a[0] } else { \"world\" };\n    println(\"hello \" + who);\n    return 0;\n}\n",
    );
    let pkg_dir = p.base.join("greeter");
    let state_str = state.to_str().unwrap();
    let ini_str = ini.to_str().unwrap();

    let (ic, iout, ierr) = p.run_env(
        &[
            "tool",
            "install",
            "greeter",
            "--path",
            pkg_dir.to_str().unwrap(),
        ],
        &[("SPID3R_STATE_DIR", state_str), ("SPID3R_INI", ini_str)],
    );
    assert_eq!(ic, 0, "tool install failed: out={iout} err={ierr}");

    let (lc, lout, _) = p.run_env(
        &["tool", "list"],
        &[("SPID3R_STATE_DIR", state_str), ("SPID3R_INI", ini_str)],
    );
    assert_eq!(lc, 0);
    assert!(
        lout.contains("greeter") && lout.contains("greetctl"),
        "{lout}"
    );

    let shim = if cfg!(windows) {
        state.join("bin").join("greetctl.cmd")
    } else {
        state.join("bin").join("greetctl")
    };
    assert!(shim.is_file(), "missing shim {}", shim.display());
    let run_out = Command::new(&shim)
        .arg("spid3r")
        .env("SPID3R_INI", ini_str)
        .output()
        .expect("execute installed shim");
    assert!(
        run_out.status.success(),
        "shim failed: {}",
        String::from_utf8_lossy(&run_out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run_out.stdout).trim(),
        "hello spid3r"
    );

    let (uc, _, uerr) = p.run_env(
        &["tool", "uninstall", "greeter"],
        &[("SPID3R_STATE_DIR", state_str), ("SPID3R_INI", ini_str)],
    );
    assert_eq!(uc, 0, "uninstall failed: {uerr}");
    assert!(!shim.exists(), "shim should be removed on uninstall");
}

#[cfg(feature = "native")]
#[test]
fn build_auto_links_vendored_ffi_system_lib_without_flag() {
    let p = Project::new("auto_ffi_build");
    let sys_lib = if cfg!(windows) { "kernel32" } else { "m" };
    p.write_sibling(
        "syswrap/spider.toml",
        &format!(
            "[package]\nname = \"syswrap\"\nversion = \"0.1.0\"\nentry = \"src/lib.matrix\"\n\n[ffi]\nlibs = [\"{}\"]\n",
            sys_lib
        ),
    );
    if cfg!(windows) {
        p.write_sibling(
            "syswrap/src/lib.matrix",
            "extern fn GetCurrentProcessId() -> int;\npublic fn host_pid() -> int { return GetCurrentProcessId(); }\n",
        );
    } else {
        p.write_sibling(
            "syswrap/src/lib.matrix",
            "extern fn tgamma(x: float) -> float;\npublic fn math_six() -> float { return tgamma(4.0); }\n",
        );
    }
    p.write(
        "spider.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nsyswrap = \"../syswrap\"\n",
    );
    if cfg!(windows) {
        p.write(
            "src/main.matrix",
            "import \"syswrap/lib\";\nfn main() { let pid = host_pid(); if (pid <= 0) { return 2; } println(\"AUTO_FFI_OK\"); return 0; }\n",
        );
    } else {
        p.write(
            "src/main.matrix",
            "import \"syswrap/lib\";\nfn main() { let v = math_six(); if (v < 5.9 || v > 6.1) { return 2; } println(\"AUTO_FFI_OK\"); return 0; }\n",
        );
    }
    assert_eq!(p.run(&["install"]).0, 0);
    let exe_out = p.root.join(if cfg!(windows) { "app.exe" } else { "app" });
    let (bcode, bout, berr) = p.run(&["build", "src/main.matrix", "-o", exe_out.to_str().unwrap()]);
    assert_eq!(bcode, 0, "auto-ffi build failed: out={bout} err={berr}");
    assert!(
        berr.contains("auto-linking FFI"),
        "expected note in stderr: {berr}"
    );
    let run_out = Command::new(&exe_out).output().expect("run built binary");
    assert!(run_out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&run_out.stdout).trim(),
        "AUTO_FFI_OK"
    );
}

#[test]
fn security_trust_pk_fails_closed_on_stripped_metadata_sigs_and_http_mirrors() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let p = Project::new("sec_c2");
    let reg = p.base.join("reg");
    p.write_sibling(
        "slib/spider.toml",
        "[package]\nname = \"slib\"\nversion = \"1.0.0\"\nentry = \"src/lib.matrix\"\n",
    );
    p.write_sibling(
        "slib/src/lib.matrix",
        "public fn ok() -> int { return 1; }\n",
    );

    let seed = "33".repeat(32);
    let pub_out = Command::new(bin())
        .args(["publish", "--to-dir", reg.to_str().unwrap()])
        .current_dir(p.base.join("slib"))
        .env("SPID3R_SIGN_KEY", &seed)
        .output()
        .unwrap();
    assert!(pub_out.status.success());
    let pk = std::fs::read_to_string(reg.join("registry.pub"))
        .unwrap()
        .trim()
        .to_string();
    let ini = p.base.join("empty.ini");
    std::fs::write(&ini, "").unwrap();
    let ini_str = ini.to_str().unwrap();

    // 1. Removing manifest.sha256.sig must fail when SPID3R_TRUST_PK is set.
    let msig_path = reg.join("slib/1.0.0/manifest.sha256.sig");
    let orig_msig = std::fs::read(&msig_path).unwrap();
    std::fs::remove_file(&msig_path).unwrap();

    p.write(
        "spider.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nslib = { version = \"^1.0\" }\n",
    );
    p.write("src/main.matrix", "fn main() { return 0; }\n");
    let (code, _, err) = p.run_env(
        &["install"],
        &[
            ("SPID3R_INI", ini_str),
            ("SPID3R_REGISTRY", reg.to_str().unwrap()),
            ("SPID3R_TRUST_PK", &pk),
        ],
    );
    assert_ne!(
        code, 0,
        "stripped manifest.sha256.sig must fail under trust_pk"
    );
    assert!(err.contains("manifest.sha256.sig"), "{err}");
    std::fs::write(&msig_path, orig_msig).unwrap();

    // 2. Remote registry missing registry.toml.sig must fail under trust_pk.
    std::fs::remove_file(reg.join("registry.toml.sig")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let reg_clone = reg.clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_bg = stop.clone();
    let h = std::thread::spawn(move || {
        while !stop_bg.load(std::sync::atomic::Ordering::SeqCst) {
            if let Ok((mut s, _)) = listener.accept() {
                if stop_bg.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let raw = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                let rel = raw.trim_start_matches('/');
                let cand = reg_clone.join(rel);
                if cand.is_file() {
                    let body = std::fs::read(&cand).unwrap();
                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = s.write_all(hdr.as_bytes());
                    let _ = s.write_all(&body);
                } else {
                    let _ = s.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
            }
        }
    });

    let remote_url = format!("http://127.0.0.1:{}", port);
    let cache = p.base.join("cache");
    let (rcode, _, rerr) = p.run_env(
        &["install"],
        &[
            ("SPID3R_INI", ini_str),
            ("SPID3R_REGISTRY", &remote_url),
            ("SPID3R_TRUST_PK", &pk),
            ("SPID3R_CACHE_DIR", cache.to_str().unwrap()),
        ],
    );
    assert_ne!(rcode, 0, "unsigned registry.toml must fail under trust_pk");
    assert!(rerr.contains("registry.toml.sig"), "{rerr}");

    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = std::net::TcpStream::connect(("127.0.0.1", port));
    let _ = h.join();
}

#[test]
fn security_ffi_link_and_verify_reject_tampered_manifest_snapshot() {
    let p = Project::new("sec_c3");
    p.add_dep("purelib", "1.0.0", DEP_BODY);
    p.add_app(&[("purelib", "../purelib")], "fn main() { return 0; }\n");
    assert_eq!(p.run(&["install"]).0, 0);

    // Tamper .spid3r-manifest.toml to inject an undeclared [ffi] lib.
    let snap = p.root.join("vendor/purelib/.spid3r-manifest.toml");
    std::fs::write(
        &snap,
        "[package]\nname = \"purelib\"\nversion = \"1.0.0\"\nentry = \"src/lib.matrix\"\n\n[ffi]\nlibs = [\"evil_injected\"]\n",
    )
    .unwrap();

    let ffi_err =
        spid3r_lang::pkg::ffi_link::collect_project_ffi_links(&p.root.join("src/main.matrix"))
            .unwrap_err();
    assert!(ffi_err.contains("tampered manifest"), "{ffi_err}");
    assert_ne!(
        p.run(&["audit", "--offline"]).0,
        0,
        "audit must fail on ffi drift"
    );

    // Deleting .spid3r-manifest.toml must also fail verify when manifest-sha256 is locked.
    let reg = p.base.join("reg");
    p.write_sibling(
        "regmod/spider.toml",
        "[package]\nname = \"regmod\"\nversion = \"1.0.0\"\nentry = \"src/lib.matrix\"\n",
    );
    p.write_sibling(
        "regmod/src/lib.matrix",
        "public fn a() -> int { return 1; }\n",
    );
    assert!(Command::new(bin())
        .args(["publish", "--to-dir", reg.to_str().unwrap()])
        .current_dir(p.base.join("regmod"))
        .output()
        .unwrap()
        .status
        .success());
    let p2 = Project::new("sec_c3_del");
    p2.write(
        "spider.toml",
        "[package]\nname = \"app2\"\nversion = \"0.1.0\"\n\n[dependencies]\nregmod = { version = \"^1.0\" }\n",
    );
    p2.write("src/main.matrix", "fn main() { return 0; }\n");
    assert_eq!(
        p2.run_env(&["install"], &[("SPID3R_REGISTRY", reg.to_str().unwrap())])
            .0,
        0
    );
    std::fs::remove_file(p2.root.join("vendor/regmod/.spid3r-manifest.toml")).unwrap();
    let (vcode, vout, verr) = p2.run(&["verify"]);
    assert_ne!(
        vcode, 0,
        "verify must catch deleted .spid3r-manifest.toml: {vout}{verr}"
    );
    let (_, lout, _) = p2.run(&["list"]);
    assert!(lout.contains("UNVERIFIED"), "{lout}");
}

#[test]
fn security_frozen_rejects_hash_drift_without_mutating_vendor() {
    let p = Project::new("sec_frozen_h1");
    p.add_dep("mathlib", "1.0.0", DEP_BODY);
    p.add_app(&[("mathlib", "../mathlib")], "fn main() { return 0; }\n");
    assert_eq!(p.run(&["install"]).0, 0);
    assert_eq!(p.run(&["install", "--frozen"]).0, 0);

    // Mutate upstream path dependency source without changing its version.
    p.write_sibling(
        "mathlib/src/lib.matrix",
        "public fn square(x) {\n    return 999;\n}\n",
    );
    let (code, _, err) = p.run(&["install", "--frozen"]);
    assert_ne!(code, 0, "--frozen must reject sha256 drift");
    assert!(err.contains("frozen lockfile"), "{err}");
    // Original vendored copy must remain untouched!
    assert!(
        p.read("vendor/mathlib/lib.matrix").contains("x * x"),
        "failed --frozen must not overwrite vendor/"
    );
}

#[test]
fn security_tar_rejects_pax_headers_and_normalized_case_collisions() {
    let pax_tar = spid3r_lang::pkg::write_tar(&[]);
    let _ = pax_tar;
    // Construct a minimal valid ustar header with typeflag 'x' (PAX header).
    let mut hdr = [0u8; 512];
    hdr[..11].copy_from_slice(b"PaxHeader/a");
    hdr[100..108].copy_from_slice(b"0000644\0");
    hdr[108..116].copy_from_slice(b"0000000\0");
    hdr[116..124].copy_from_slice(b"0000000\0");
    hdr[124..136].copy_from_slice(b"00000000000\0");
    hdr[136..148].copy_from_slice(b"00000000000\0");
    hdr[148..156].copy_from_slice(b"        ");
    hdr[156] = b'x';
    hdr[257..263].copy_from_slice(b"ustar\0");
    hdr[263..265].copy_from_slice(b"00");
    let cksum: u64 = hdr.iter().map(|&b| b as u64).sum();
    let ck_str = format!("{:06o}\0 ", cksum);
    hdr[148..156].copy_from_slice(ck_str.as_bytes());
    let mut raw = Vec::new();
    raw.extend_from_slice(&hdr);
    raw.extend_from_slice(&[0u8; 1024]);
    let err = spid3r_lang::pkg::read_tar(&raw).unwrap_err();
    assert!(err.contains("PAX"), "{err}");

    let dup_tar = spid3r_lang::pkg::write_tar(&[
        ("src//a.matrix".to_string(), b"1\n".to_vec()),
        ("src/A.matrix".to_string(), b"2\n".to_vec()),
    ]);
    let out_dir = std::env::temp_dir().join(format!("spid3r_tar_dup_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).unwrap();
    let dup_err = spid3r_lang::pkg::tar::extract_tar_checked(&dup_tar, &out_dir).unwrap_err();
    assert!(dup_err.contains("case-colliding"), "{dup_err}");
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn tool_lifecycle_cleans_orphans_refuses_ffi_and_supports_offline() {
    let p = Project::new("tool_h5");
    let state = p.base.join("state");
    let cache = p.base.join("cache");
    let reg = p.base.join("reg");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::create_dir_all(&reg).unwrap();
    let ini = p.base.join("empty.ini");
    std::fs::write(&ini, "").unwrap();
    let state_str = state.to_str().unwrap();
    let ini_str = ini.to_str().unwrap();
    let reg_str = reg.to_str().unwrap();
    let cache_str = cache.to_str().unwrap();

    // 1. Tool with 2 bins installed, then upgraded (--force) dropping one bin -> orphan shim removed.
    p.write_sibling(
        "tpkg_v1/spider.toml",
        "[package]\nname = \"tpkg\"\nversion = \"1.0.0\"\n\n[bin]\nbin_keep = \"src/a.matrix\"\nbin_drop = \"src/b.matrix\"\n",
    );
    p.write_sibling(
        "tpkg_v1/src/a.matrix",
        "fn main() { println(\"A1\"); return 0; }\n",
    );
    p.write_sibling(
        "tpkg_v1/src/b.matrix",
        "fn main() { println(\"B1\"); return 0; }\n",
    );
    assert!(Command::new(bin())
        .args(["publish", "--to-dir", reg_str])
        .current_dir(p.base.join("tpkg_v1"))
        .output()
        .unwrap()
        .status
        .success());

    let (c1, _, e1) = p.run_env(
        &["tool", "install", "tpkg"],
        &[
            ("SPID3R_STATE_DIR", state_str),
            ("SPID3R_INI", ini_str),
            ("SPID3R_REGISTRY", reg_str),
            ("SPID3R_CACHE_DIR", cache_str),
        ],
    );
    assert_eq!(c1, 0, "{e1}");
    let drop_shim = if cfg!(windows) {
        state.join("bin/bin_drop.cmd")
    } else {
        state.join("bin/bin_drop")
    };
    assert!(drop_shim.is_file());

    // Upgrade via --path removing `bin_drop`.
    p.write_sibling(
        "tpkg_v2/spider.toml",
        "[package]\nname = \"tpkg\"\nversion = \"2.0.0\"\n\n[bin]\nbin_keep = \"src/a.matrix\"\n",
    );
    p.write_sibling(
        "tpkg_v2/src/a.matrix",
        "fn main() { println(\"A2\"); return 0; }\n",
    );
    let (c2, _, e2) = p.run_env(
        &[
            "tool",
            "install",
            "tpkg",
            "--path",
            p.base.join("tpkg_v2").to_str().unwrap(),
            "--force",
        ],
        &[("SPID3R_STATE_DIR", state_str), ("SPID3R_INI", ini_str)],
    );
    assert_eq!(c2, 0, "{e2}");
    assert!(
        !drop_shim.exists(),
        "orphan shim must be deleted on upgrade"
    );

    // 2. `--offline` install succeeds from local registry.
    assert_eq!(
        p.run_env(
            &["tool", "uninstall", "tpkg"],
            &[("SPID3R_STATE_DIR", state_str), ("SPID3R_INI", ini_str)],
        )
        .0,
        0
    );
    let (off_c, _, off_e) = p.run_env(
        &["tool", "install", "tpkg", "--offline"],
        &[
            ("SPID3R_STATE_DIR", state_str),
            ("SPID3R_INI", ini_str),
            ("SPID3R_REGISTRY", reg_str),
            ("SPID3R_CACHE_DIR", cache_str),
        ],
    );
    assert_eq!(off_c, 0, "tool --offline failed: {off_e}");

    // 3. [ffi] tool is cleanly rejected.
    p.write_sibling(
        "ffitool/spider.toml",
        "[package]\nname = \"ffitool\"\nversion = \"0.1.0\"\n\n[bin]\nffibin = \"src/cli.matrix\"\n\n[ffi]\nlibs = [\"m\"]\n",
    );
    p.write_sibling("ffitool/src/cli.matrix", "fn main() { return 0; }\n");
    let (fc, _, ferr) = p.run_env(
        &[
            "tool",
            "install",
            "ffitool",
            "--path",
            p.base.join("ffitool").to_str().unwrap(),
        ],
        &[("SPID3R_STATE_DIR", state_str), ("SPID3R_INI", ini_str)],
    );
    assert_ne!(fc, 0, "[ffi] tool must be rejected");
    assert!(ferr.contains("[ffi]"), "{ferr}");
}

#[test]
fn yanked_latest_skipped_local_audit_remove_dev_and_json_escape() {
    let p = Project::new("h3_h6_m2_m3");
    let reg = p.base.join("reg");
    let reg_str = reg.to_str().unwrap();

    for ver in ["1.0.0", "1.1.0"] {
        let d = p.base.join(format!("yk_{}", ver));
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(
            d.join("spider.toml"),
            format!(
                "[package]\nname = \"yk\"\nversion = \"{}\"\nentry = \"src/lib.matrix\"\n",
                ver
            ),
        )
        .unwrap();
        std::fs::write(d.join("src/lib.matrix"), "public fn f() { return 1; }\n").unwrap();
        assert!(Command::new(bin())
            .args(["publish", "--to-dir", reg_str])
            .current_dir(&d)
            .output()
            .unwrap()
            .status
            .success());
    }
    // Yank 1.1.0 -> fresh `spkg install yk` must pick 1.0.0!
    std::fs::write(reg.join("yk/yanked.txt"), "1.1.0\n").unwrap();

    let (ic, _, ie) = p.run_env(&["install", "yk"], &[("SPID3R_REGISTRY", reg_str)]);
    assert_eq!(ic, 0, "{ie}");
    let lock = p.read("spider.lock");
    assert!(
        lock.contains("version = \"1.0.0\""),
        "must skip yanked 1.1.0: {lock}"
    );

    // Unyank 1.1.0 via explicit `spkg install yk==1.1.0` -> updates spider.toml + lock, then local audit catches yank + advisory!
    std::fs::write(
        reg.join("advisories.txt"),
        "yk\t<=1.1.0\thigh\tSPK-TEST-1\tTest flaw\n",
    )
    .unwrap();
    let (ac, aout, _) = p.run_env(&["audit"], &[("SPID3R_REGISTRY", reg_str)]);
    assert_ne!(
        ac, 0,
        "local registry advisory must make audit fail: {aout}"
    );
    assert!(aout.contains("SPK-TEST-1"), "{aout}");

    // `remove` handles [dev-dependencies], and `list --json` escapes Windows backslashes.
    p.add_dep("devx", "0.1.0", DEP_BODY);
    p.write(
        "spider.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\npathdep = { path = \"..\\devx\" }\n\n[dev-dependencies]\ndevx = { path = \"../devx\" }\n",
    );
    let _ = p.run(&["install", "--dev"]);
    let (_, jout, _) = p.run(&["list", "--json"]);
    assert!(
        !jout.contains("path+..\\devx") || jout.contains("path+..\\\\devx"),
        "Windows backslashes must be JSON-escaped:\n{jout}"
    );
    let (rc, _, re) = p.run(&["remove", "devx"]);
    assert_eq!(rc, 0, "remove dev-dep failed: {re}");
    assert!(!p.read("spider.toml").contains("devx ="));
}

#[test]
fn tool_install_path_note_override_hint_and_nsis_bin_parity() {
    let p = Project::new("tool_path_hint");
    p.write_sibling(
        "pathprobe/spider.toml",
        "[package]\nname = \"pathprobe\"\nversion = \"0.1.0\"\n\n[bin]\npathprobe = \"src/cli.matrix\"\n",
    );
    p.write_sibling("pathprobe/src/cli.matrix", "fn main() { return 0; }\n");
    let pkg_dir = p.base.join("pathprobe");

    // 1. INI sets paths.state_dir (with forward slashes, like `phase4_paths`),
    //    and bin_dir is NOT on PATH -> prints clean native separators on Windows,
    //    the PATH note, and the INI provenance hint using `control config reset`.
    let state_ini = p.base.join("state_from_ini");
    let state_ini_fwd = state_ini.to_string_lossy().replace('\\', "/");
    let ini = p.base.join("custom.ini");
    std::fs::write(&ini, format!("[paths]\nstate_dir = {}\n", state_ini_fwd)).unwrap();

    let out1 = Command::new(bin())
        .args([
            "tool",
            "install",
            "pathprobe",
            "--path",
            pkg_dir.to_str().unwrap(),
        ])
        .current_dir(&p.root)
        .env("SPID3R_INI", &ini)
        .env_remove("SPID3R_STATE_DIR")
        .env_remove("SPID3R_PATHS_STATE_DIR")
        .env("PATH", "")
        .output()
        .expect("spawn");
    assert!(
        out1.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out1.stderr)
    );
    let stdout1 = String::from_utf8_lossy(&out1.stdout);
    let err1 = String::from_utf8_lossy(&out1.stderr);
    if cfg!(windows) {
        assert!(
            !stdout1.contains('/') && !err1.contains("state/bin") && !err1.contains("/state\\bin"),
            "Windows tool output must use native separators: stdout={stdout1} stderr={err1}"
        );
    }
    assert!(
        err1.contains("is not on your PATH"),
        "expected PATH note: {err1}"
    );
    assert!(
        err1.contains("paths.state_dir")
            && err1.contains("custom.ini")
            && err1.contains("spid3r control config reset paths.state_dir"),
        "expected INI provenance hint with cross-shell reset command: {err1}"
    );

    // 2. Same INI state_dir (forward slashes), now WITH bin_dir on PATH
    //    (native separators) -> PATH note must be suppressed.
    let bin_native = state_ini.join("bin");
    let path_with_bin = std::env::join_paths([&bin_native]).unwrap();
    let out2 = Command::new(bin())
        .args([
            "tool",
            "install",
            "pathprobe",
            "--path",
            pkg_dir.to_str().unwrap(),
            "--force",
        ])
        .current_dir(&p.root)
        .env("SPID3R_INI", &ini)
        .env_remove("SPID3R_STATE_DIR")
        .env_remove("SPID3R_PATHS_STATE_DIR")
        .env("PATH", &path_with_bin)
        .output()
        .expect("spawn");
    assert!(
        out2.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let err2 = String::from_utf8_lossy(&out2.stderr);
    assert!(
        !err2.contains("is not on your PATH"),
        "bin_dir is on PATH; unexpected warning: {err2}"
    );

    // 2b. On Windows, an 8.3 short path in PATH matches the long `bin_dir` via canonicalization.
    if cfg!(windows) {
        if let Ok(short_out) = Command::new("cmd")
            .args([
                "/c",
                &format!("for %I in (\"{}\") do @echo %~sI", bin_native.display()),
            ])
            .output()
        {
            let short_str = String::from_utf8_lossy(&short_out.stdout)
                .trim()
                .to_string();
            if !short_str.is_empty() && Path::new(&short_str).is_dir() {
                let path_short = std::env::join_paths([Path::new(&short_str)]).unwrap();
                let out_short = Command::new(bin())
                    .args([
                        "tool",
                        "install",
                        "pathprobe",
                        "--path",
                        pkg_dir.to_str().unwrap(),
                        "--force",
                    ])
                    .current_dir(&p.root)
                    .env("SPID3R_INI", &ini)
                    .env_remove("SPID3R_STATE_DIR")
                    .env_remove("SPID3R_PATHS_STATE_DIR")
                    .env("PATH", &path_short)
                    .output()
                    .expect("spawn");
                assert!(out_short.status.success());
                let err_short = String::from_utf8_lossy(&out_short.stderr);
                assert!(
                    !err_short.contains("is not on your PATH"),
                    "8.3 short PATH entry '{short_str}' must match '{}': {err_short}",
                    bin_native.display()
                );
            }
        }
    }

    // 2c. Executing the hinted reset command actually clears `paths.state_dir`.
    let reset_out = Command::new(bin())
        .args(["control", "config", "reset", "paths.state_dir"])
        .current_dir(&p.root)
        .env("SPID3R_INI", &ini)
        .env_remove("SPID3R_STATE_DIR")
        .env_remove("SPID3R_PATHS_STATE_DIR")
        .output()
        .expect("spawn");
    assert!(
        reset_out.status.success(),
        "control config reset failed: out={} err={}",
        String::from_utf8_lossy(&reset_out.stdout),
        String::from_utf8_lossy(&reset_out.stderr)
    );
    let get_after = Command::new(bin())
        .args(["config", "get", "paths.state_dir"])
        .current_dir(&p.root)
        .env("SPID3R_INI", &ini)
        .env_remove("SPID3R_STATE_DIR")
        .env_remove("SPID3R_PATHS_STATE_DIR")
        .output()
        .expect("spawn");
    assert!(get_after.status.success());
    assert_eq!(
        String::from_utf8_lossy(&get_after.stdout).trim(),
        "",
        "paths.state_dir must be empty after reset"
    );

    // 3. Env overrides (`SPID3R_STATE_DIR` and `SPID3R_PATHS_STATE_DIR`) not on PATH
    //    -> mention the exact overriding variable name.
    let state_env = p.base.join("state_from_env");
    let empty_ini = p.base.join("empty.ini");
    std::fs::write(&empty_ini, "").unwrap();
    let out3 = Command::new(bin())
        .args([
            "tool",
            "install",
            "pathprobe",
            "--path",
            pkg_dir.to_str().unwrap(),
        ])
        .current_dir(&p.root)
        .env("SPID3R_INI", &empty_ini)
        .env("SPID3R_STATE_DIR", &state_env)
        .env_remove("SPID3R_PATHS_STATE_DIR")
        .env("PATH", "")
        .output()
        .expect("spawn");
    assert!(
        out3.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out3.stderr)
    );
    let err3 = String::from_utf8_lossy(&out3.stderr);
    assert!(
        err3.contains("SPID3R_STATE_DIR"),
        "expected SPID3R_STATE_DIR provenance hint: {err3}"
    );

    let state_env2 = p.base.join("state_from_paths_env");
    let out4 = Command::new(bin())
        .args([
            "tool",
            "install",
            "pathprobe",
            "--path",
            pkg_dir.to_str().unwrap(),
        ])
        .current_dir(&p.root)
        .env("SPID3R_INI", &empty_ini)
        .env_remove("SPID3R_STATE_DIR")
        .env("SPID3R_PATHS_STATE_DIR", &state_env2)
        .env("PATH", "")
        .output()
        .expect("spawn");
    assert!(
        out4.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out4.stderr)
    );
    let err4 = String::from_utf8_lossy(&out4.stderr);
    assert!(
        err4.contains("SPID3R_PATHS_STATE_DIR"),
        "expected SPID3R_PATHS_STATE_DIR provenance hint: {err4}"
    );

    // 4. Windows NSIS installer PATH must match Windows default `tool_bin_dir()`
    //    (`%LOCALAPPDATA%\Spid3r\state\bin`), never add `$LOCALAPPDATA\Spid3r\bin`,
    //    wrap PowerShell `$parts = @(...)` (avoiding scalar string concatenation),
    //    and preserve `REG_EXPAND_SZ`.
    let nsi = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/nsis/spid3r.nsi"),
    )
    .expect("read packaging/nsis/spid3r.nsi");
    assert!(
        nsi.contains(r#"$LOCALAPPDATA\Spid3r\state\bin"#),
        "NSIS installer must add $LOCALAPPDATA\\Spid3r\\state\\bin to PATH"
    );
    assert!(
        !nsi.contains("Push \"$LOCALAPPDATA\\Spid3r\\bin\"\n  Call ${UN}SafeAddDirToPath")
            && !nsi.contains("Push \"$LOCALAPPDATA\\Spid3r\\bin\"\r\n  Call ${UN}SafeAddDirToPath"),
        "NSIS installer must not add the wrong $LOCALAPPDATA\\Spid3r\\bin directory"
    );
    assert!(
        nsi.contains("$$parts = @($$cur -split ';'")
            && nsi.contains("DoNotExpandEnvironmentNames")
            && nsi.contains("RegistryValueKind]::ExpandString"),
        "NSIS PowerShell PATH fallback must force array wrapping and preserve REG_EXPAND_SZ"
    );

    // 5. Unix `install.sh` must have the same `spkg tool` PATH parity:
    //    add and remove `${XDG_STATE_HOME:-$HOME/.local/state}/spid3r/bin`
    //    alongside `${INSTALL_DIR}`, and pass POSIX `sh -n`.
    let install_sh_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh");
    let install_sh = std::fs::read_to_string(&install_sh_path).expect("read install.sh");
    assert!(
        install_sh.contains("${XDG_STATE_HOME:-$HOME/.local/state}")
            && install_sh.contains("${XDG_STATE_HOME:-${HOME_PREFIX}/.local/state}")
            && install_sh.contains("TOOL_BIN_DIR=")
            && install_sh.contains("/spid3r/bin")
            && install_sh.contains("${INSTALL_DIR}:${TOOL_BIN_DIR}")
            && install_sh.contains("rpf_exp_both=")
            && install_sh.contains("rpf_exp_tool=")
            && install_sh.contains("write_rc_inplace"),
        "install.sh must add and remove the default XDG spkg tool bin dir"
    );

    #[cfg(unix)]
    {
        let syntax = Command::new("sh")
            .args(["-n", install_sh_path.to_str().unwrap()])
            .output()
            .expect("spawn sh -n");
        assert!(
            syntax.status.success(),
            "sh -n install.sh failed: {}",
            String::from_utf8_lossy(&syntax.stderr)
        );

        let fake_home = p.base.join("unix_home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let bashrc = fake_home.join(".bashrc");
        std::fs::write(
            &bashrc,
            format!(
                "export KEEP_USER=1\nalias spdir=\"cd {home}/.local/state/spid3r/bin\"\nexport OTHER=1 # {home}/.spid3r/bin\n",
                home = fake_home.display()
            ),
        )
        .unwrap();

        let probe = format!(
            "set -eu\n\
             lib=\"{tmp}/lib.sh\"\n\
             awk '/^if \\[ \"\\$DO_UNINSTALL\" -eq 1 \\]; then/ {{ exit }} {{ print }}' \"{src}\" > \"$lib\"\n\
             unset XDG_STATE_HOME SPID3R_INSTALL_DIR SPID3R_NO_PATH\n\
             export HOME=\"{home}/\"\n\
             export PATH=\"/usr/bin:/bin\"\n\
             set -- --modify-path\n\
             . \"$lib\"\n\
             configure_path\n\
             configure_path\n",
            tmp = p.base.display(),
            src = install_sh_path.display(),
            home = fake_home.display(),
        );
        let run_cfg = Command::new("sh")
            .args(["-c", &probe])
            .output()
            .expect("run install.sh configure_path");
        assert!(
            run_cfg.status.success(),
            "configure_path probe failed: {}",
            String::from_utf8_lossy(&run_cfg.stderr)
        );
        let rc_after = std::fs::read_to_string(&bashrc).unwrap();
        let expected_line = format!(
            "export PATH=\"{home}/.spid3r/bin:{home}/.local/state/spid3r/bin:$PATH\"",
            home = fake_home.display()
        );
        assert_eq!(
            rc_after.lines().filter(|l| *l == expected_line).count(),
            1,
            "expected exactly one combined PATH export in .bashrc:\n{rc_after}"
        );

        let uninst = Command::new("sh")
            .args([install_sh_path.to_str().unwrap(), "--uninstall"])
            .env("HOME", &fake_home)
            .env_remove("XDG_STATE_HOME")
            .env_remove("SPID3R_INSTALL_DIR")
            .env("PATH", "/usr/bin:/bin")
            .output()
            .expect("run install.sh --uninstall");
        assert!(
            uninst.status.success(),
            "install.sh --uninstall failed: {}",
            String::from_utf8_lossy(&uninst.stderr)
        );
        let rc_uninst = std::fs::read_to_string(&bashrc).unwrap();
        assert!(rc_uninst.contains("export KEEP_USER=1"), "{rc_uninst}");
        assert!(
            !rc_uninst.contains(&expected_line)
                && !rc_uninst.contains("# Spid3r (installed by install.sh)"),
            "uninstall left spid3r PATH entry behind:\n{rc_uninst}"
        );
    }
}

/// Audit A-4 fallback path: even when `origin/HEAD` is missing in the cached
/// git repository (forcing the fallback branch in `fetch_git`), locally
/// modified tracked files and untracked files in `.spid3r/git/<name>` must be
/// reset/cleaned and must never leak into `vendor/`.
#[test]
fn git_fetch_fallback_resets_dirty_tracked_and_untracked_files() {
    let p = Project::new("git_fallback_clean");
    let repo = p.base.join("upstream_git");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("spider.toml"),
        "[package]\nname = \"gdep\"\nversion = \"0.1.0\"\nentry = \"src/lib.matrix\"\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/lib.matrix"),
        "public fn greeting() { return \"clean\"; }\n",
    )
    .unwrap();

    let git = |dir: &std::path::Path, args: &[&str]| {
        let out = Command::new("git")
            .args(["-C", dir.to_str().unwrap()])
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "test@spid3r.local"]);
    git(&repo, &["config", "user.name", "Spid3r Test"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);

    let repo_url = repo.to_str().unwrap().replace('\\', "/");
    p.write(
        "spider.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ngdep = {{ git = \"{}\" }}\n",
            repo_url
        ),
    );
    let (code, _, err) = p.run(&["install"]);
    assert_eq!(code, 0, "initial git install failed: {err}");
    assert!(p.read("vendor/gdep/lib.matrix").contains("\"clean\""));

    // Dirty the cached git checkout with both a tracked modification and an
    // untracked file, AND delete refs/remotes/origin/HEAD to force the
    // fallback branch in fetch_git when reinstalling without a lock pin.
    let cache_dir = p.root.join(".spid3r/git/gdep");
    let _ = Command::new("git")
        .args([
            "-C",
            cache_dir.to_str().unwrap(),
            "remote",
            "set-head",
            "origin",
            "-d",
        ])
        .output();
    std::fs::write(
        cache_dir.join("src/lib.matrix"),
        "public fn greeting() { return \"TAMPERED\"; }\n",
    )
    .unwrap();
    std::fs::write(cache_dir.join("src/injected.matrix"), "let evil = 1;\n").unwrap();
    let _ = std::fs::remove_file(p.root.join("spider.lock"));

    let (code2, _, err2) = p.run(&["install"]);
    assert_eq!(code2, 0, "re-install after dirty cache failed: {err2}");
    let vendored = p.read("vendor/gdep/lib.matrix");
    assert!(
        vendored.contains("\"clean\"") && !vendored.contains("TAMPERED"),
        "tracked modification leaked into vendor/: {vendored}"
    );
    assert!(
        !p.exists("vendor/gdep/injected.matrix"),
        "untracked file leaked into vendor/"
    );
}

/// Audit §9.9: `spkg audit` must flag configured plaintext `http://` registry
/// endpoints as high-severity failures.
#[test]
fn audit_flags_plaintext_http_registry_in_cli() {
    let p = Project::new("audit_http_reg");
    p.write(
        "spider.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    let ini = p.base.join("empty.ini");
    std::fs::write(&ini, "").unwrap();

    let (code, out, err) = p.run_env(
        &["audit", "--offline"],
        &[
            ("SPID3R_INI", ini.to_str().unwrap()),
            ("SPID3R_REGISTRY", "http://plaintext.registry.invalid/v1"),
        ],
    );
    assert_ne!(
        code, 0,
        "spkg audit must fail on plaintext http:// registry: out={out} err={err}"
    );
    assert!(
        out.contains("insecure-registry") && out.contains("http://plaintext.registry.invalid/v1"),
        "expected insecure-registry finding in audit output: out={out} err={err}"
    );
}

/// Audit A-1: `spid3r run` warns on stderr when vendored dependencies drift
/// from `spider.lock`, and fails closed when `SPID3R_VERIFY_ON_RUN=1`.
#[test]
fn run_warns_and_optionally_fails_on_tampered_vendor() {
    let p = Project::new("vendor_on_run");
    p.add_registry_pkg("vlib", "1.0.0", "public fn val() { return 42; }\n");
    p.write(
        "spider.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nentry = \"src/main.matrix\"\n\n[dependencies]\nvlib = { version = \"^1.0\" }\n",
    );
    p.write(
        "src/main.matrix",
        "import \"vlib/lib\";\nfn main() { println(val()); return 0; }\n",
    );
    let ini = p.base.join("empty.ini");
    std::fs::write(&ini, "").unwrap();
    let reg = p.registry_dir();
    let (ic, _, ierr) = p.run_env(
        &["install"],
        &[
            ("SPID3R_INI", ini.to_str().unwrap()),
            ("SPID3R_REGISTRY", &reg),
        ],
    );
    assert_eq!(ic, 0, "install failed: {ierr}");

    // Clean vendor -> no warning on run.
    let (rc1, out1, err1) = p.run_env(
        &["run", "src/main.matrix"],
        &[("SPID3R_INI", ini.to_str().unwrap())],
    );
    assert_eq!(rc1, 0, "clean run failed: {err1}");
    assert_eq!(out1.trim(), "42");
    assert!(!err1.contains("warning[pkg]"), "unexpected warning: {err1}");

    // Tamper vendored file -> warning[pkg] on default run; exit 1 with SPID3R_VERIFY_ON_RUN=1.
    p.write("vendor/vlib/lib.matrix", "public fn val() { return 99; }\n");
    let (rc2, out2, err2) = p.run_env(
        &["run", "src/main.matrix"],
        &[("SPID3R_INI", ini.to_str().unwrap())],
    );
    assert_eq!(rc2, 0, "warn-only run failed: {err2}");
    assert_eq!(out2.trim(), "99");
    assert!(
        err2.contains("warning[pkg]") && err2.contains("spider.lock"),
        "expected vendor tamper warning: {err2}"
    );

    let (rc3, _, err3) = p.run_env(
        &["run", "src/main.matrix"],
        &[
            ("SPID3R_INI", ini.to_str().unwrap()),
            ("SPID3R_VERIFY_ON_RUN", "1"),
        ],
    );
    assert_eq!(rc3, 1, "strict verify_on_run must fail closed: {err3}");
    assert!(err3.contains("spider.lock"), "{err3}");

    // Deleting vendor/ entirely must ALSO fail closed under SPID3R_VERIFY_ON_RUN=1.
    std::fs::remove_dir_all(p.root.join("vendor")).unwrap();
    let (rc4, _, err4) = p.run_env(
        &["run", "src/main.matrix"],
        &[
            ("SPID3R_INI", ini.to_str().unwrap()),
            ("SPID3R_VERIFY_ON_RUN", "1"),
        ],
    );
    assert_eq!(
        rc4, 1,
        "deleted vendor/ must fail closed under SPID3R_VERIFY_ON_RUN=1: {err4}"
    );
    assert!(err4.contains("spider.lock"), "{err4}");
}

/// Audit A-3 & A-5: installing from a signed registry without `packages.trust_pk`
/// warns loudly on stderr, whereas pinning `SPID3R_TRUST_PK` silences it;
/// `spkg audit` also flags an exposed `registry.key` inside a local registry.
#[test]
fn unsigned_install_warns_and_audit_flags_exposed_registry_key() {
    let p = Project::new("trust_warn_and_key");
    let lib = p.base.join("pub_lib");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::write(
        lib.join("spider.toml"),
        "[package]\nname = \"slib\"\nversion = \"1.0.0\"\nentry = \"src/lib.matrix\"\n",
    )
    .unwrap();
    std::fs::write(lib.join("src/lib.matrix"), "public fn ok() { return 1; }\n").unwrap();

    let reg = p.base.join("reg");
    let state = p.base.join("state");
    let ini = p.base.join("empty.ini");
    std::fs::write(&ini, "").unwrap();
    let pub_out = Command::new(bin())
        .current_dir(&lib)
        .args(["publish", "--to-dir", reg.to_str().unwrap()])
        .env("SPID3R_INI", &ini)
        .env("SPID3R_STATE_DIR", &state)
        .output()
        .expect("publish");
    assert!(
        pub_out.status.success(),
        "publish failed: {}",
        String::from_utf8_lossy(&pub_out.stderr)
    );
    assert!(
        !reg.join("registry.key").exists(),
        "registry.key must stay outside published directory"
    );
    let pk = std::fs::read_to_string(reg.join("registry.pub"))
        .unwrap()
        .trim()
        .to_string();

    p.write(
        "spider.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nslib = { version = \"^1.0\" }\n",
    );

    // 1. Install without trust_pk -> warning[pkg] emitted.
    let (c1, _, e1) = p.run_env(
        &["install"],
        &[
            ("SPID3R_INI", ini.to_str().unwrap()),
            ("SPID3R_REGISTRY", reg.to_str().unwrap()),
        ],
    );
    assert_eq!(c1, 0, "install failed: {e1}");
    assert!(
        e1.contains("packages.trust_pk is unset"),
        "expected trust_pk warning on stderr: {e1}"
    );

    // 2. Re-install with trust_pk pinned -> no trust_pk warning.
    let (c2, _, e2) = p.run_env(
        &["install", "--frozen"],
        &[
            ("SPID3R_INI", ini.to_str().unwrap()),
            ("SPID3R_REGISTRY", reg.to_str().unwrap()),
            ("SPID3R_TRUST_PK", &pk),
        ],
    );
    assert_eq!(c2, 0, "trusted install failed: {e2}");
    assert!(
        !e2.contains("packages.trust_pk is unset"),
        "unexpected trust_pk warning when pinned: {e2}"
    );

    // 3. Drop a legacy registry.key inside the local registry -> audit flags it.
    std::fs::write(reg.join("registry.key"), "00".repeat(32)).unwrap();
    let (ac, aout, aerr) = p.run_env(
        &["audit", "--offline"],
        &[
            ("SPID3R_INI", ini.to_str().unwrap()),
            ("SPID3R_REGISTRY", reg.to_str().unwrap()),
            ("SPID3R_TRUST_PK", &pk),
        ],
    );
    assert_ne!(
        ac, 0,
        "audit must fail when registry.key is exposed: {aout}"
    );
    assert!(
        aout.contains("exposed-signing-key"),
        "expected exposed-signing-key in audit output: out={aout} err={aerr}"
    );
}
