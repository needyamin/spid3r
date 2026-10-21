//! Audit A-15: the regex engine must scan the input once, not once per start.
//!
//! `regex_find("a+b", "a" * n)` used to re-run the NFA from every candidate
//! start index, so it was O(n²): 2 500 chars took 634 ms, 5 000 took 2 553 ms,
//! and 20 000 never finished inside 30 s. The engine now carries a
//! "start anywhere" thread through a single scan.
//!
//! Timing is asserted as a *deadline in a child process*, not as a ratio: a
//! ratio between two runs is noise-prone on CI, while "20 000 characters in
//! under 10 s" separates the old behaviour (>30 s) from the new one (~5 ms) by
//! three orders of magnitude.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

/// Run `src` in a child process with a wall-clock deadline.
fn run_within(src: &str, tag: &str, limit: Duration) -> (String, Duration) {
    let path = std::env::temp_dir().join(format!("spid3r_regex_{}.matrix", tag));
    std::fs::write(&path, src).expect("write probe");
    let started = Instant::now();
    let out = Command::new(bin())
        .args(["run", path.to_str().unwrap()])
        .output()
        .expect("spawn spid3r");
    let elapsed = started.elapsed();
    let _ = std::fs::remove_file(&path);
    assert!(
        elapsed < limit,
        "`{}` took {:?}, over the {:?} deadline (the engine is scanning per start again)",
        tag,
        elapsed,
        limit
    );
    assert!(
        out.status.success(),
        "`{}` failed: {}",
        tag,
        String::from_utf8_lossy(&out.stderr)
    );
    (String::from_utf8_lossy(&out.stdout).to_string(), elapsed)
}

/// A long run of `a`s with no match for `a+b`: the worst case for the old
/// per-start rescan, because every start position walks to the end.
#[test]
fn a_failing_scan_over_a_long_input_finishes_quickly() {
    let src = r#"fn main() {
    let s = "";
    let i = 0;
    while (i < 20000) { s = s + "a"; i = i + 1; }
    let r = regex_find("a+b", s);
    println(string(r));
    return 0;
}
"#;
    let (out, elapsed) = run_within(src, "longfail", Duration::from_secs(10));
    assert_eq!(out.trim(), "none", "expected no match for a+b in a-run");
    eprintln!("20k-char failing scan: {:?}", elapsed);
}

/// The same shape, but matching: the scan must still report the whole run.
#[test]
fn a_long_match_reports_the_full_span() {
    let src = r#"fn main() {
    let s = "";
    let i = 0;
    while (i < 20000) { s = s + "a"; i = i + 1; }
    s = s + "b";
    let r = regex_find("a+b", s);
    println(r.unwrap().len());
    return 0;
}
"#;
    let (out, elapsed) = run_within(src, "longmatch", Duration::from_secs(10));
    assert_eq!(
        out.trim(),
        "20001",
        "the match must cover every `a` plus the b"
    );
    eprintln!("20k-char matching scan: {:?}", elapsed);
}

/// `find_all` and `replace` walk the input once overall, so a long input with
/// many matches must also stay fast.
#[test]
fn many_matches_on_a_long_input_stay_fast() {
    let src = r#"fn main() {
    let s = "";
    let i = 0;
    while (i < 20000) { s = s + "ab"; i = i + 1; }
    let all = regex_find_all("ab", s);
    println(all.len());
    let r = regex_replace("ab", s, "-");
    println(r.len());
    return 0;
}
"#;
    let (out, _) = run_within(src, "manymatch", Duration::from_secs(10));
    assert_eq!(
        out.trim(),
        "20000\n20000",
        "unexpected match/replace counts"
    );
}

/// Anchors are the case the single-pass rewrite has to be careful about: `$`
/// matches the empty string at end-of-input, `^` only at position 0, and `^$`
/// only on an empty input.
#[test]
fn anchors_keep_their_meaning() {
    let src = r#"fn main() {
    println(regex_match("$", "abc"));
    println(regex_find("$", "abc").unwrap().len());
    println(regex_match("^", "abc"));
    println(regex_find("^", "abc").unwrap().len());
    println(regex_match("^$", ""));
    println(regex_match("^$", "abc"));
    println(regex_match("^ab$", "ab"));
    println(regex_match("^ab$", "abc"));
    println(regex_replace("$", "abc", "<>"));
    println(regex_replace("^", "abc", "<>"));
    return 0;
}
"#;
    let (out, _) = run_within(src, "anchors", Duration::from_secs(10));
    assert_eq!(
        out.trim(),
        "true\n0\ntrue\n0\ntrue\nfalse\ntrue\nfalse\nabc\n<>abc",
        "anchor semantics drifted"
    );
}
