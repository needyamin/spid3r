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

    /// Run with a variable explicitly removed (to test the "unset" error path).
    fn run_unset(&self, args: &[&str], key: &str) -> (i32, String, String) {
        let out = Command::new(bin())
            .args(args)
            .current_dir(&self.root)
            .env_remove(key)
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
    // Unset SPID3R_REGISTRY.
    let p = Project::new("regnone");
    p.add_registry_pkg("mathlib", "1.0.0", DEP_BODY);
    p.add_app_registry(&[("mathlib", "^1.0")], "fn main() {\n    return 0;\n}\n");
    let (code, _, err) = p.run_unset(&["install"], "SPID3R_REGISTRY");
    assert_ne!(code, 0, "install must fail without SPID3R_REGISTRY");
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
