//! Phase 5 performance-edit differential regression.
//!
//! Three additive edits shipped in Phase 5 must not change observable
//! behaviour on any of the four VM-family execution paths
//! (tree / vm / vm `--no-opt` / `exec` of a compiled container):
//!
//!   1. `src/stdlib/mod.rs` — scalar-only JSON writer fast path. The original
//!      snapshot-clone recursion is retained as the fallback; removing it
//!      outright would reintroduce the documented non-reentrant `Mutex`
//!      deadlock on self-referential containers.
//!   2. `src/value/collections/seq.rs` + `src/vm/mod.rs` — a
//!      `MapData::generation` counter and a bounded, per-VM, monomorphic
//!      `FieldGet` / `FieldGetSoft` inline cache. A cached index is
//!      re-validated under the map lock on every use and the value is read
//!      fresh, so mutation must never yield a stale result.
//!   3. `src/vm/mod.rs` + `src/cli.rs` — `opt_enabled`, so `--no-opt` disables
//!      the Phase 1 runtime superinstruction peeks as well as the compile-time
//!      pass. Peeks are optimizations only, so parity must still hold.
//!
//! Every case runs in a fresh child process with an explicit `SPID3R_INI`, so
//! the host's `%APPDATA%` config cannot change the verdict.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_spid3r")
}

fn scratch(tag: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "spid3r_phase5_{}_{}_{}",
        tag,
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create scratch directory");
    dir
}

fn run_in(dir: &Path, ini: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(dir)
        .env("SPID3R_INI", ini)
        .env("SPID3R_CONFIG_DIR", dir.join("config"))
        .env("SPID3R_STATE_DIR", dir.join("state"))
        .output()
        .expect("spawn spid3r")
}

/// Run tree / vm / vm --no-opt / exec on `src`, returning labelled outputs.
fn all_engines(dir: &Path, ini: &Path, src: &Path) -> Vec<(&'static str, Output)> {
    let mut out = Vec::new();
    for (label, args) in [
        ("tree", vec!["run", "--engine", "tree"]),
        ("vm", vec!["run", "--engine", "vm"]),
        ("vm --no-opt", vec!["run", "--engine", "vm", "--no-opt"]),
    ] {
        let mut a = args;
        a.push(src.to_str().unwrap());
        out.push((label, run_in(dir, ini, &a)));
    }
    let mc = dir.join("probe.matrixc");
    let compiled = run_in(
        dir,
        ini,
        &["compile", src.to_str().unwrap(), "-o", mc.to_str().unwrap()],
    );
    assert_eq!(
        compiled.status.code(),
        Some(0),
        "compile failed: {:?}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    out.push(("exec", run_in(dir, ini, &["exec", mc.to_str().unwrap()])));
    out
}

fn assert_uniform(out: &[(&str, Output)], want_code: i32, want_stdout: &str, want_err: &str) {
    for (label, o) in out {
        assert_eq!(
            o.status.code(),
            Some(want_code),
            "{label}: rc differs; stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&o.stdout).trim(),
            want_stdout,
            "{label}: stdout differs; stderr={:?}",
            String::from_utf8_lossy(&o.stderr)
        );
        let stderr = String::from_utf8_lossy(&o.stderr);
        assert!(
            stderr.contains(want_err),
            "{label}: missing '{want_err}' in stderr: {stderr}"
        );
    }
}

fn case(tag: &str, source: &str) -> Vec<(&'static str, Output)> {
    let dir = scratch(tag);
    let ini = dir.join("spid3r.ini");
    let src = dir.join("probe.matrix");
    std::fs::write(&ini, "# no policy overrides\n").expect("write ini");
    std::fs::write(&src, source).expect("write source");
    all_engines(&dir, &ini, &src)
}

/// Scalar-only maps inside a list: the top-level list takes the snapshot
/// fallback while every child map takes the new scalar fast path. Mixed
/// nesting, round-tripping through `json_parse`, and a second map site must
/// all stay byte-identical across engines.
#[test]
fn scalar_only_json_fast_path_is_byte_identical() {
    let out = case(
        "json-scalar",
        concat!(
            "fn main() {\n",
            "    let rows = list();\n",
            "    let i = 0;\n",
            "    while i < 3 {\n",
            "        rows.push(map(\"id\", i, \"sku\", \"SKU-\" + string(i), \"qty\", (i % 7) + 1));\n",
            "        i += 1;\n",
            "    }\n",
            "    let nested = list();\n",
            "    nested.push(rows);\n",
            "    nested.push(map(\"ok\", true, \"n\", -12));\n",
            "    let s = json_write(nested);\n",
            "    println(s);\n",
            "    println(s.len());\n",
            "    let back = json_parse(s);\n",
            "    println(back[0][1][\"sku\"]);\n",
            "    return 0;\n",
            "}\n",
        ),
    );
    // The expected JSON is written by the same writer on every engine; pin the
    // full spelling so the fast path cannot silently change escaping/order.
    let want = concat!(
        "[[{\"id\":0,\"sku\":\"SKU-0\",\"qty\":1},",
        "{\"id\":1,\"sku\":\"SKU-1\",\"qty\":2},",
        "{\"id\":2,\"sku\":\"SKU-2\",\"qty\":3}],",
        "{\"ok\":true,\"n\":-12}]\n",
        "116\n",
        "SKU-1",
    );
    for (label, o) in &out {
        assert_eq!(
            o.status.code(),
            Some(0),
            "{label}: rc differs; stderr={:?}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&o.stdout).trim(),
            want.trim(),
            "{label}: JSON output differs"
        );
    }
}

/// A self-referential container must still take the snapshot-clone fallback
/// and hit the depth guard rather than deadlocking or emitting garbage.
#[test]
fn self_referential_json_still_uses_the_safe_fallback() {
    let out = case(
        "json-cycle",
        concat!(
            "fn main() {\n",
            "    let l = list();\n",
            "    l.push(l);\n",
            "    println(json_write(l));\n",
            "    return 0;\n",
            "}\n",
        ),
    );
    assert_uniform(&out, 1, "", "nesting deeper than");
}

/// Repeated constant-name map reads at one site must be invalidated by every
/// mutation path (`IndexSet`, `map.set`, `remove`, `clear`). A stale inline
/// cache would print the first value for all 30 iterations.
#[test]
fn map_field_inline_cache_invalidates_on_every_mutation() {
    let out = case(
        "map-generation",
        concat!(
            "fn main() {\n",
            "    let m = map(\"a\", 1, \"b\", 2);\n",
            "    let acc = 0;\n",
            "    let r = 0;\n",
            "    while r < 30 {\n",
            "        acc = acc + m.a;\n",
            "        m[\"a\"] = (r % 5) + 1;\n",
            "        r += 1;\n",
            "    }\n",
            "    println(acc);\n",
            "    m.set(\"a\", 42);\n",
            "    println(m.a);\n",
            "    m.remove(\"a\");\n",
            "    m.set(\"a\", 7);\n",
            "    println(m.a);\n",
            "    println(m.b);\n",
            "    m.clear();\n",
            "    println(m?.a);\n",
            "    m.set(\"a\", 11);\n",
            "    println(m.a);\n",
            "    return 0;\n",
            "}\n",
        ),
    );
    // Reads are 1, then (k % 5)+1 for k=0..28: five full cycles (75) plus
    // 1+2+3+4 (10) = 85, so acc = 86.
    let want = "86\n42\n7\n2\nnull\n11";
    for (label, o) in &out {
        assert_eq!(
            o.status.code(),
            Some(0),
            "{label}: rc differs; stderr={:?}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&o.stdout).trim(),
            want,
            "{label}: stale map field value"
        );
    }
}

/// `--no-opt` now disables the Phase 1 runtime peeks (fused A loop condition
/// and fused C string prefix) as well as the compile-time pass. Disabling an
/// optimization must change only speed, never stdout or the exit code.
#[test]
fn no_opt_disables_runtime_peeks_without_changing_semantics() {
    let out = case(
        "opt-enabled",
        concat!(
            "fn main() {\n",
            "    let n = 5000;\n",
            "    let d = 2;\n",
            "    let count = 0;\n",
            "    while d * d <= n {\n",
            "        count += 1;\n",
            "        d += 1;\n",
            "    }\n",
            "    println(count);\n",
            "    let r = 0;\n",
            "    let acc = 0;\n",
            "    while r < 100 {\n",
            "        let key = \"k\" + string(r % 64);\n",
            "        acc = acc + key.len();\n",
            "        r += 1;\n",
            "    }\n",
            "    println(acc);\n",
            "    return 0;\n",
            "}\n",
        ),
    );
    // "k0".."k9" / "k10".."k63" / "k0".."k9" / "k10".."k35"
    // => 10*2 + 54*3 + 10*2 + 26*3 = 280.
    let want = "69\n280";
    for (label, o) in &out {
        assert_eq!(
            o.status.code(),
            Some(0),
            "{label}: rc differs; stderr={:?}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&o.stdout).trim(),
            want,
            "{label}: stdout differs"
        );
    }
}
