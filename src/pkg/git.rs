//! Hardened Git dependency transport and commit pinning (audit A-5 & S6).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::lockfile;
use super::manifest::valid_pkg_name;

/// Validate a Git remote URL against policy (`packages.allow_insecure_git`,
/// `packages.allow_local_git`), blocking `ext::` and option-smuggling paths.
pub fn validate_git_url(name: &str, url: &str) -> Result<(), String> {
    let reject = |why: &str| {
        Err(format!(
            "git dependency '{}': {} ('{}'). Allowed: https:// and ssh:// (scp-like \
             user@host:path); local paths need SPID3R_ALLOW_LOCAL_GIT_DEPS=1, plain \
             http/git need SPID3R_ALLOW_INSECURE_GIT=1",
            name, why, url
        ))
    };
    if url.is_empty() {
        return reject("empty url");
    }
    if url.starts_with('-') {
        return reject("url may not start with '-'");
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return reject("url contains whitespace or control characters");
    }
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("ext::") {
        return Err(format!(
            "git dependency '{}': refusing the ext:: transport, which runs an arbitrary \
             command during install ('{}')",
            name, url
        ));
    }
    if lower.starts_with("https://") {
        return Ok(());
    }
    if lower.starts_with("ssh://") {
        let authority = url.get(6..).unwrap_or("").split('/').next().unwrap_or("");
        let host = authority.rsplit('@').next().unwrap_or(authority);
        if host.starts_with('-') || host.is_empty() {
            return reject("ssh host is missing or starts with '-'");
        }
        return Ok(());
    }
    if lower.starts_with("http://") || lower.starts_with("git://") {
        return if crate::config::bool_of("packages.allow_insecure_git") {
            Ok(())
        } else {
            reject("plaintext transport has no integrity protection")
        };
    }
    if lower.starts_with("file://") {
        return if crate::config::bool_of("packages.allow_local_git") {
            Ok(())
        } else {
            reject("file:// reads the local filesystem")
        };
    }
    if lower.contains("://") {
        return reject("unsupported url scheme");
    }
    let bytes = url.as_bytes();
    let is_drive = bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic();
    let is_scp_like = !is_drive
        && url
            .split_once(':')
            .map(|(host, path)| {
                !host.is_empty()
                    && !host.contains('/')
                    && !host.contains('\\')
                    && !path.is_empty()
                    && !host.rsplit('@').next().unwrap_or(host).starts_with('-')
            })
            .unwrap_or(false);
    if is_scp_like {
        return Ok(());
    }
    if crate::config::bool_of("packages.allow_local_git") {
        Ok(())
    } else {
        reject("local path")
    }
}

/// Return true when `s` is a valid lowercase/uppercase 40-char hex Git commit hash.
pub fn is_valid_git_commit(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Clone (or fetch) a Git dependency into `<root>/.spid3r/git/<name>` with
/// symlinks, fsmonitor, hooks, and submodules disabled (audit S6).
pub fn fetch_git(
    root: &Path,
    name: &str,
    url: &str,
    rev: Option<&str>,
    locked: Option<&str>,
) -> Result<(PathBuf, String), String> {
    valid_pkg_name(name)?;
    validate_git_url(name, url)?;
    if let Some(commit) = locked {
        if !is_valid_git_commit(commit) {
            return Err(format!(
                "git dependency '{}': invalid locked commit hash '{}'",
                name, commit
            ));
        }
    }
    if let Some(r) = rev {
        let safe = !r.is_empty()
            && !r.starts_with('-')
            && r.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
        if !safe {
            return Err(format!(
                "git dependency '{}': unsafe revision '{}'",
                name, r
            ));
        }
    }
    let dir = root.join(".spid3r").join("git").join(name);
    if dir.join(".git").is_dir() {
        run_git(&["-C", &path_str(&dir), "fetch", "--all", "--tags", "--quiet"])?;
    } else {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {}", parent.display(), e))?;
        }
        run_git(&[
            "clone",
            "--no-recurse-submodules",
            "--quiet",
            "--",
            url,
            &path_str(&dir),
        ])?;
    }
    if let Some(commit) = locked {
        run_git(&[
            "-C",
            &path_str(&dir),
            "checkout",
            "--detach",
            "--force",
            "--quiet",
            commit,
        ])
        .map_err(|e| {
            format!(
                "git dependency '{}': the commit {} recorded in spider.lock is not \
                 available ({}). If upstream rewrote its history, run \
                 `spkg install --update` to re-resolve it.",
                name, commit, e
            )
        })?;
    } else if let Some(rev) = rev {
        run_git(&[
            "-C",
            &path_str(&dir),
            "checkout",
            "--detach",
            "--force",
            "--quiet",
            rev,
        ])?;
    } else {
        let target = remote_default_branch(&dir)
            .map(|b| format!("origin/{}", b))
            .unwrap_or_else(|| "origin/HEAD".to_string());
        if run_git(&[
            "-C",
            &path_str(&dir),
            "checkout",
            "--quiet",
            "--force",
            "--detach",
            &target,
        ])
        .is_err()
        {
            // Audit A-4: reset tracked modifications before attempting a
            // fast-forward pull so dirty tracked files neither block the pull
            // nor survive if pull is a no-op / fails.
            run_git(&["-C", &path_str(&dir), "reset", "--hard", "--quiet", "HEAD"])?;
            let _ = run_git(&["-C", &path_str(&dir), "pull", "--quiet", "--ff-only"]);
        }
    }
    // Audit A-4: reset tracked files and wipe any leftover untracked/ignored
    // files in the cached git working tree across every branch so local
    // modifications can never leak into `vendor/`.
    run_git(&["-C", &path_str(&dir), "reset", "--hard", "--quiet", "HEAD"])?;
    run_git(&["-C", &path_str(&dir), "clean", "-fdxq"])?;
    let out = run_git(&["-C", &path_str(&dir), "rev-parse", "HEAD"])?;
    let head = out.trim().to_ascii_lowercase();
    if let Some(commit) = locked {
        if !head.eq_ignore_ascii_case(commit) {
            return Err(format!(
                "git dependency '{}': checked out {} but spider.lock pins {}; refusing to \
                 vendor a different tree (run `spkg install --update` to re-resolve)",
                name, head, commit
            ));
        }
    }
    Ok((dir, head))
}

/// Verify an existing cached Git checkout in `--offline` mode matches the locked
/// commit pin (when present) and return its normalized 40-hex `HEAD` hash.
pub fn verify_offline_checkout(
    name: &str,
    dir: &Path,
    locked: Option<&str>,
) -> Result<String, String> {
    if !dir.join(".git").exists() {
        return Err(format!(
            "offline mode: cached git dependency '{}' at {} is not a git repository",
            name,
            dir.display()
        ));
    }
    let out = run_git(&["-C", &path_str(dir), "rev-parse", "HEAD"])?;
    let head = out.trim().to_ascii_lowercase();
    if !is_valid_git_commit(&head) {
        return Err(format!(
            "offline mode: git dependency '{}' has invalid HEAD '{}'",
            name, head
        ));
    }
    if let Some(commit) = locked {
        if !is_valid_git_commit(commit) {
            return Err(format!(
                "git dependency '{}': invalid locked commit hash '{}'",
                name, commit
            ));
        }
        if !head.eq_ignore_ascii_case(commit) {
            return Err(format!(
                "offline mode: cached git dependency '{}' is at {}, but spider.lock pins {} — run `spkg install` online",
                name, head, commit
            ));
        }
    }
    // Audit A-4: refuse to vendor a dirty offline git cache (including ignored
    // untracked files, matching online `git clean -fdxq`).
    let status = run_git(&[
        "-C",
        &path_str(dir),
        "status",
        "--porcelain",
        "--untracked-files=all",
        "--ignored=matching",
    ])?;
    if !status.trim().is_empty() {
        return Err(format!(
            "offline mode: cached git dependency '{}' at {} has uncommitted or untracked modifications; refusing to vendor a dirty tree",
            name,
            dir.display()
        ));
    }
    Ok(head)
}

/// Resolve locked Git commit from `locked` map when the URL matches.
pub fn locked_commit(locked: &BTreeMap<String, String>, name: &str, url: &str) -> Option<String> {
    lockfile::locked_git_commit(locked, name, url)
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

fn remote_default_branch(dir: &Path) -> Option<String> {
    let out = run_git(&[
        "-C",
        &path_str(dir),
        "symbolic-ref",
        "--short",
        "refs/remotes/origin/HEAD",
    ])
    .ok()?;
    let name = out.trim().strip_prefix("origin/")?.trim().to_string();
    (!name.is_empty()).then_some(name)
}

fn run_git(args: &[&str]) -> Result<String, String> {
    let null_dev = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let hooks_opt = format!("core.hooksPath={}", null_dev);
    let mut cmd = std::process::Command::new("git");
    cmd.args([
        "-c",
        "core.symlinks=false",
        "-c",
        "core.fsmonitor=false",
        "-c",
        &hooks_opt,
    ])
    .args(args);
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run git ({}): is git installed?", e))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!(
            "git {} failed: {}",
            args.iter()
                .filter(|a| !a.starts_with('-'))
                .cloned()
                .collect::<Vec<_>>()
                .join(" "),
            if err.is_empty() {
                "unknown error".to_string()
            } else {
                err
            }
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_rejects_invalid_locked_commit_and_offline_mismatch() {
        let dir = std::env::temp_dir().join(format!("spid3r_git_sec_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        assert!(is_valid_git_commit(&"a".repeat(40)));
        assert!(!is_valid_git_commit("--upload-pack=evil"));
        assert!(!is_valid_git_commit("abc"));

        let err = fetch_git(
            &dir,
            "dep",
            "https://example.com/repo.git",
            None,
            Some("--upload-pack=evil"),
        )
        .unwrap_err();
        assert!(err.contains("invalid locked commit hash"), "{err}");

        let off_err = verify_offline_checkout("dep", &dir, Some(&"a".repeat(40))).unwrap_err();
        assert!(off_err.contains("offline mode"), "{off_err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
