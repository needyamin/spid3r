//! Language-toolchain control ops: the panel's (and `spid3r control --op`'s)
//! way to drive the `spid3r` compiler/VM itself — run, check, compile, build,
//! test, fmt, and project/package management — plus a small source editor.
//!
//! Every toolchain action runs the real `spid3r` binary as a child process
//! (never in-process), so the panel gets exactly the CLI's behaviour, exit
//! codes and sandbox flags, and a crashing program cannot take the panel down.
//! The subcommand is checked against a closed allow-list and arguments are
//! passed as an argv vector, never through a shell.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::ops::{field, jstr};

/// One-shot toolchain commands the panel may run through `lang_exec`. This is
/// the FULL CLI surface minus documented exclusions:
///
///   - `repl`, `debug`, `gui`  → interactive; run through `op_lang_terminal`
///     (own console / window) instead of the captured-output exec path.
///   - `lsp`                   → a long-running stdio server; it would occupy
///     the exec path until the wall-clock timeout with nothing to show.
///
/// `control` and `config` are not here because the panel drives their exact
/// operations directly through `ops::dispatch` (status / safety_overview /
/// config_* / logs_tail) — the same ops the `spid3r control` CLI uses, so
/// both surfaces stay provably identical. `tests/control_tests.rs` enforces
/// this whitelist against `spid3r --help` so a new CLI command cannot ship
/// without a panel decision.
const ALLOWED: &[&str] = &[
    "run", "check", "compile", "build", "test", "fmt", "doc", "disasm", "exec", "modules", "init",
    "install", "pack", "unpack", "dist", "bench", "info", "neofetch", "version", "magic", "bundle",
    // Package / registry one-shots and module tooling (parity with the CLI).
    "add", "publish", "remove", "search", "verify", "tree", "help",
    // Stage-0 oracle dumps (SH-5): deterministic captured-output one-shots,
    // same shape as `disasm`.
    "tokens", "parse",
];

/// Commands that run interactively in their own console / window instead of
/// the captured-output exec path.
const TERMINAL: &[&str] = &["repl", "debug", "gui"];

/// Commands deliberately NOT panel-runnable, with the reason surfaced in the
/// parity test: `control` and `config` are driven through their structured
/// `ops::dispatch` operations (never by re-execing the CLI), and `lsp` is a
/// long-running stdio server that would occupy the exec path until the
/// wall-clock timeout.
const NON_RUNNABLE: &[&str] = &["control", "config", "lsp"];

/// Single authority for "can the panel run this command with captured
/// output?" — used by `op_lang_exec`, reported by `op_lang_info` and enforced
/// against `spid3r --help` by `tests/control_tests.rs`.
pub fn lang_exec_allowed(cmd: &str) -> bool {
    ALLOWED.contains(&cmd)
}

/// Single authority for "does this command open in its own terminal /
/// window?" — used by `op_lang_terminal`.
pub fn lang_terminal_allowed(cmd: &str) -> bool {
    TERMINAL.contains(&cmd)
}

/// Single authority for the documented non-runnable set.
pub fn lang_non_runnable(cmd: &str) -> bool {
    NON_RUNNABLE.contains(&cmd)
}

/// Output cap per stream, so a chatty program cannot balloon the IPC reply.
const MAX_OUTPUT: usize = 256 * 1024;
/// Wall-clock limit for one toolchain action.
const TIMEOUT: Duration = Duration::from_secs(120);
/// Editable source types (the editor is for project files, not arbitrary I/O).
const EDITABLE_EXT: &[&str] = &["matrix", "toml", "html", "md", "txt", "json"];

/// Locate the `spid3r` toolchain binary. Inside `spid3r-control` the current
/// exe is the PANEL, so spawning it would open another panel — look for the
/// sibling `spid3r` first and fall back to PATH.
pub fn toolchain_exe() -> PathBuf {
    let name = if cfg!(windows) {
        "spid3r.exe"
    } else {
        "spid3r"
    };
    if let Ok(me) = std::env::current_exe() {
        let stem = me
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if stem == "spid3r" {
            return me;
        }
        if let Some(dir) = me.parent() {
            let sib = dir.join(name);
            if sib.is_file() {
                return sib;
            }
        }
    }
    PathBuf::from(name)
}

/// Split an argument string on whitespace, honouring `"double quotes"`.
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut has = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_q = !in_q;
                has = true;
            }
            c if c.is_whitespace() && !in_q => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

fn drain<R: std::io::Read + Send + 'static>(mut r: R) -> std::thread::JoinHandle<Vec<u8>> {
    crate::value::mark_multithreaded();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match r.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    // Keep draining past the cap so the child never blocks
                    // on a full pipe; just stop storing.
                    if buf.len() < MAX_OUTPUT {
                        let take = n.min(MAX_OUTPUT - buf.len());
                        buf.extend_from_slice(&chunk[..take]);
                    }
                }
            }
        }
        buf
    })
}

fn valid_dir(cwd: &str) -> Result<Option<PathBuf>, String> {
    if cwd.trim().is_empty() {
        return Ok(None);
    }
    let p = PathBuf::from(cwd.trim());
    if p.is_dir() {
        Ok(Some(p))
    } else {
        Err(format!("working directory '{}' does not exist", cwd))
    }
}

/// `lang_exec` — params: `cmd` (allow-listed subcommand), `file` (optional,
/// first positional), `args` (extra flags/args, whitespace-split), `cwd`.
pub fn op_lang_exec(params: &str) -> Result<String, String> {
    let cmd = field(params, "cmd").unwrap_or_default();
    if !lang_exec_allowed(&cmd) {
        return Err(format!(
            "'{}' is not a panel-runnable command (allowed: {})",
            cmd,
            ALLOWED.join(", ")
        ));
    }
    let file = field(params, "file").unwrap_or_default();
    let extra = field(params, "args").unwrap_or_default();
    let cwd = valid_dir(&field(params, "cwd").unwrap_or_default())?;

    let mut argv: Vec<String> = vec![cmd.clone()];
    // Flags first, then the file: `run --sandbox app.matrix`. The CLI treats
    // trailing args after the file as program args for `run`, so a user who
    // wants those puts them after `--` in the args box (handled below).
    let (flags, tail) = match extra.find(" -- ") {
        Some(i) => (extra[..i].to_string(), extra[i + 4..].to_string()),
        None => (extra.clone(), String::new()),
    };
    argv.extend(split_args(&flags));
    if !file.trim().is_empty() {
        argv.push(file.trim().to_string());
    }
    argv.extend(split_args(&tail));

    let exe = toolchain_exe();
    let mut c = std::process::Command::new(&exe);
    c.args(&argv)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(d) = &cwd {
        c.current_dir(d);
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let started = Instant::now();
    let mut child = c
        .spawn()
        .map_err(|e| format!("cannot start {}: {}", exe.display(), e))?;
    let out_h = child.stdout.take().map(drain);
    let err_h = child.stderr.take().map(drain);

    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if started.elapsed() > TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(format!("wait failed: {}", e)),
        }
    };
    let stdout = out_h
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    let stderr = err_h
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    let code = match status {
        Some(s) => s
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".into()),
        None => "timeout".into(),
    };
    let ms = started.elapsed().as_millis();
    let _ = timed_out;
    Ok(format!(
        "{{\"code\":{},\"command\":{},\"stdout\":{},\"stderr\":{},\"ms\":\"{}\"}}",
        jstr(&code),
        jstr(&format!("spid3r {}", argv.join(" "))),
        jstr(&String::from_utf8_lossy(&stdout)),
        jstr(&String::from_utf8_lossy(&stderr)),
        ms
    ))
}

/// `lang_terminal` — open an interactive tool (`repl`, `debug`) or a GUI
/// program (`gui`) in its own window, detached from the panel. Windows only
/// for console tools (a new console is the only portable-free way to give
/// them a terminal without a dependency).
pub fn op_lang_terminal(params: &str) -> Result<String, String> {
    let cmd = field(params, "cmd").unwrap_or_default();
    if !lang_terminal_allowed(&cmd) {
        return Err(format!("'{}' cannot be opened in a terminal window", cmd));
    }
    let file = field(params, "file").unwrap_or_default();
    let extra = field(params, "args").unwrap_or_default();
    let cwd = valid_dir(&field(params, "cwd").unwrap_or_default())?;
    if cmd != "repl" && file.trim().is_empty() {
        return Err(format!("{} needs a source file", cmd));
    }
    let mut argv = vec![cmd.clone()];
    argv.extend(split_args(&extra));
    if !file.trim().is_empty() {
        argv.push(file.trim().to_string());
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new(toolchain_exe());
        c.args(&argv);
        if let Some(d) = &cwd {
            c.current_dir(d);
        }
        // CREATE_NEW_CONSOLE for console tools; gui programs bring their own window.
        if cmd != "gui" {
            c.creation_flags(0x0000_0010);
        } else {
            c.creation_flags(0x0800_0000);
        }
        c.spawn()
            .map_err(|e| format!("cannot start spid3r: {}", e))?;
        Ok(format!(
            "{{\"message\":{}}}",
            jstr(&format!("opened: spid3r {}", argv.join(" ")))
        ))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (cwd, argv);
        Err("terminal launch is Windows-only; run it from a shell: spid3r repl|debug".into())
    }
}

/// `lang_info` — toolchain location and version string.
pub fn op_lang_info() -> Result<String, String> {
    let exe = toolchain_exe();
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("version").stdin(std::process::Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let ver = cmd.output().ok().filter(|o| o.status.success()).map(|o| {
        let raw = String::from_utf8_lossy(&o.stdout);
        if let Some(v_line) = raw.lines().find(|l| l.trim_start().starts_with("Version")) {
            let v = v_line.split_whitespace().nth(1).unwrap_or("");
            if !v.is_empty() {
                return format!("spid3r {} (language {})", v, crate::LANGUAGE_VERSION);
            }
        }
        raw.lines().next().unwrap_or("").trim().to_string()
    });
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    Ok(format!(
        "{{\"exe\":{},\"found\":\"{}\",\"version\":{},\"cwd\":{},\"commands\":{}}}",
        jstr(&exe.display().to_string()),
        ver.is_some(),
        jstr(&ver.unwrap_or_default()),
        jstr(&cwd),
        jstr(&ALLOWED.join(",")),
    ))
}

const SKIP_DIRS: &[&str] = &[
    "target",
    "vendor",
    ".git",
    "node_modules",
    "dist",
    ".workbuddy-ai",
];

fn walk(dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 4 || out.len() >= 500 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                walk(&p, depth + 1, out);
            }
        } else if matches!(
            p.extension().and_then(|x| x.to_str()),
            Some("matrix") | Some("matrixc")
        ) {
            out.push(p.display().to_string());
        }
    }
}

/// `lang_files` — `.matrix` / `.matrixc` files under `dir` (default: cwd).
pub fn op_lang_files(params: &str) -> Result<String, String> {
    let dir = field(params, "dir").unwrap_or_default();
    let root = match valid_dir(&dir)? {
        Some(p) => p,
        None => std::env::current_dir().map_err(|e| e.to_string())?,
    };
    let mut files = Vec::new();
    walk(&root, 0, &mut files);
    Ok(format!(
        "{{\"dir\":{},\"files\":{}}}",
        jstr(&root.display().to_string()),
        jstr(&files.join("\n"))
    ))
}

fn editable(path: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(path.trim());
    let ext = p
        .extension()
        .and_then(|x| x.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    if !EDITABLE_EXT.contains(&ext.as_str()) {
        return Err(format!(
            "the editor only opens {} files",
            EDITABLE_EXT.join("/")
        ));
    }
    Ok(p)
}

fn resolve_editor_path(raw: &str, cwd: &str) -> Result<PathBuf, String> {
    let p = editable(raw)?;
    if p.is_relative() {
        if let Some(dir) = valid_dir(cwd)? {
            return Ok(dir.join(p));
        }
    }
    Ok(p)
}

/// `lang_read` — read a source file (max 2 MiB), resolving relative paths
/// against `cwd` when supplied.
pub fn op_lang_read(params: &str) -> Result<String, String> {
    let raw_path = field(params, "path").unwrap_or_default();
    let cwd = field(params, "cwd").unwrap_or_default();
    let path = resolve_editor_path(&raw_path, &cwd)?;
    let mut f = std::fs::File::open(&path).map_err(|e| format!("{}: {}", path.display(), e))?;
    let mut buf = Vec::new();
    f.by_ref()
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    if buf.len() > 2 * 1024 * 1024 {
        return Err("file is larger than 2 MiB".into());
    }
    let text = String::from_utf8(buf).map_err(|_| "file is not valid UTF-8".to_string())?;
    Ok(format!(
        "{{\"path\":{},\"text\":{}}}",
        jstr(&path.display().to_string()),
        jstr(&text)
    ))
}

/// `lang_write` — save a source file (creating parent dirs is NOT done: the
/// directory must exist, so a typo cannot scatter folders around the disk).
pub fn op_lang_write(params: &str) -> Result<String, String> {
    let raw_path = field(params, "path").unwrap_or_default();
    let cwd = field(params, "cwd").unwrap_or_default();
    let path = resolve_editor_path(&raw_path, &cwd)?;
    let text = field(params, "text").unwrap_or_default();
    if text.len() > 2 * 1024 * 1024 {
        return Err("refusing to write more than 2 MiB".into());
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.is_dir() {
            return Err(format!("directory {} does not exist", parent.display()));
        }
    }
    std::fs::write(&path, text.as_bytes()).map_err(|e| format!("{}: {}", path.display(), e))?;
    Ok(format!(
        "{{\"message\":{}}}",
        jstr(&format!("saved {} ({} bytes)", path.display(), text.len()))
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_quoted_args() {
        assert_eq!(
            split_args(r#"--budget 100 "my file.matrix" -x"#),
            vec!["--budget", "100", "my file.matrix", "-x"]
        );
        assert!(split_args("   ").is_empty());
    }

    #[test]
    fn rejects_unlisted_commands() {
        assert!(op_lang_exec(r#"{"cmd":"repl"}"#).is_err());
        assert!(op_lang_exec(r#"{"cmd":"rm"}"#).is_err());
    }

    #[test]
    fn editor_extension_allowlist() {
        assert!(editable("a.matrix").is_ok());
        assert!(editable("a.exe").is_err());
        assert!(editable("noext").is_err());
    }

    #[test]
    fn lang_read_write_resolve_relative_path_against_cwd() {
        let dir =
            std::env::temp_dir().join(format!("spid3r-control-lang-rel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let params_w = format!(
            "{{\"path\":\"src/main.matrix\",\"cwd\":{},\"text\":\"fn main() {{ return 0; }}\"}}",
            jstr(&dir.display().to_string())
        );
        assert!(op_lang_write(&params_w).is_ok());
        let params_r = format!(
            "{{\"path\":\"src/main.matrix\",\"cwd\":{}}}",
            jstr(&dir.display().to_string())
        );
        let read = op_lang_read(&params_r).unwrap();
        assert_eq!(
            field(&read, "text").as_deref(),
            Some("fn main() { return 0; }")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
