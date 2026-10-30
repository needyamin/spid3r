//! Append-only diagnostic log for failures that have nowhere else to go.
//!
//! A launcher built with `--subsystem windows` owns no console, and the GUI
//! runtime it starts is spawned with `CREATE_NO_WINDOW`, so `eprintln!` from
//! either of them goes to a handle that does not exist. When a single-file app
//! fails on a user's machine — no runtime to extract, a payload that does not
//! parse, a webview that cannot start — the only useful artefact is a log file
//! that says what happened and where.
//!
//! Behaviour:
//!
//! * Default path: `%LOCALAPPDATA%\Spid3r\logs\spid3r.log` on Windows,
//!   `$XDG_STATE_HOME/spid3r/spid3r.log` (else `~/.local/state/...`) elsewhere.
//! * `SPID3R_LOG_FILE` overrides the path; `SPID3R_LOG=0` (or `off`/`false`)
//!   disables logging entirely.
//! * One line per event: `<UTC timestamp> <scope>: <message>`, truncated to
//!   8 KiB so a diagnostic containing a whole file cannot balloon the log.
//! * The file is rotated to `<name>.1` once it passes 1 MiB.
//!
//! Logging never panics and never fails a caller: every I/O error is dropped.
//! It is a diagnostic aid, not a correctness dependency.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Rotate once the log passes this size.
const MAX_BYTES: u64 = 1024 * 1024;

/// A single line is capped here (a diagnostic can embed a source excerpt).
const MAX_LINE: usize = 8 * 1024;

/// Serialises writers: the launcher, the GUI thread and the CLI all log.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Whether logging is enabled for this process.
pub fn enabled() -> bool {
    match std::env::var("SPID3R_LOG") {
        Ok(v) => {
            let v = v.trim();
            !(v == "0"
                || v.eq_ignore_ascii_case("false")
                || v.eq_ignore_ascii_case("off")
                || v.eq_ignore_ascii_case("no"))
        }
        Err(_) => true,
    }
}

/// The log file for this process, if a location could be determined.
pub fn path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SPID3R_LOG_FILE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
            .map(|base| base.join("Spid3r").join("logs").join("spid3r.log"))
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state"))
            })
            .map(|base| base.join("spid3r").join("spid3r.log"))
            .or_else(|| Some(std::env::temp_dir().join("spid3r.log")))
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` for the current UTC time.
fn timestamp() -> String {
    let secs = crate::stdlib::datetime::unix_time();
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = crate::stdlib::datetime::civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Record one line. `scope` names the component (`build`, `run_payload`, …).
pub fn record(scope: &str, message: &str) {
    if !enabled() {
        return;
    }
    let Some(file) = path() else { return };
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    if let Some(parent) = file.parent() {
        if !parent.is_dir() && std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    rotate_if_needed(&file);

    // Callers pass the same string they return to the user, which is usually
    // already prefixed with the component name ("run_payload: ..."). Strip it
    // so the line does not read `run_payload: run_payload: ...`.
    let message = message
        .strip_prefix(scope)
        .and_then(|rest| rest.strip_prefix(": "))
        .unwrap_or(message);

    let mut line = format!("{} {}: {}\n", timestamp(), scope, message);
    if line.len() > MAX_LINE {
        // Truncate on a character boundary: `String::truncate` panics otherwise.
        let mut cut = MAX_LINE;
        while cut > 0 && !line.is_char_boundary(cut) {
            cut -= 1;
        }
        line.truncate(cut);
        line.push_str("…\n");
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
    {
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
    }
}

/// Keep the log bounded: rename to `<name>.1` once it is too big. Best effort;
/// a failure here only means the file keeps growing.
fn rotate_if_needed(file: &std::path::Path) {
    let too_big = std::fs::metadata(file)
        .map(|m| m.len() > MAX_BYTES)
        .unwrap_or(false);
    if !too_big {
        return;
    }
    let rotated = file.with_extension("log.1");
    let _ = std::fs::remove_file(&rotated);
    let _ = std::fs::rename(file, rotated);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests below run in one process, so they serialise on the same lock
    /// the writer uses for the file it is told to use.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn timestamp_has_the_expected_shape() {
        let ts = timestamp();
        assert_eq!(ts.len(), 20, "{}", ts);
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
    }

    #[test]
    fn record_appends_one_line_per_call() {
        let _serial = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("spid3r_log_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("spid3r.log");

        // SAFETY-adjacent: set_var is process-global, hence the test lock.
        std::env::set_var("SPID3R_LOG_FILE", &file);
        std::env::remove_var("SPID3R_LOG");
        record("build", "first failure");
        record("run_payload", "second failure");
        std::env::remove_var("SPID3R_LOG_FILE");

        let text = std::fs::read_to_string(&file).unwrap();
        // Other tests run in parallel and may log to the same file while the
        // override is set, so assert on this test's own lines rather than on
        // the total count.
        let ours: Vec<&str> = text
            .lines()
            .filter(|l| l.contains("first failure") || l.contains("second failure"))
            .collect();
        assert_eq!(ours.len(), 2, "{}", text);
        assert!(ours[0].ends_with("build: first failure"), "{}", text);
        assert!(ours[1].ends_with("run_payload: second failure"), "{}", text);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn long_messages_are_truncated_without_panicking_on_utf8() {
        let _serial = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("spid3r_log_long_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("spid3r.log");
        std::env::set_var("SPID3R_LOG_FILE", &file);
        // Multi-byte characters straddling the cut point.
        record("test", &"é".repeat(MAX_LINE));
        std::env::remove_var("SPID3R_LOG_FILE");

        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.len() < MAX_LINE + 200, "{}", text.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opt_out_stops_writing() {
        let _serial = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("spid3r_log_off_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("spid3r.log");
        std::env::set_var("SPID3R_LOG_FILE", &file);
        std::env::set_var("SPID3R_LOG", "0");
        record("test", "should not appear");
        std::env::remove_var("SPID3R_LOG");
        std::env::remove_var("SPID3R_LOG_FILE");
        assert!(!file.exists(), "SPID3R_LOG=0 must disable logging");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
