//! Regression tests for audit findings fixed from `bugs_mixed.html`.

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use spid3r_lang::parser;
use spid3r_lang::runtime_info;
use spid3r_lang::semantic;

fn bin() -> String {
    env!("CARGO_BIN_EXE_spid3r").to_string()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("spid3r_audit_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn run_spid3r(args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(bin());
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn spid3r");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
    )
}

fn run_both(name: &str, src: &str) -> (String, String) {
    let dir = scratch(name);
    let path = dir.join(format!("{name}.matrix"));
    std::fs::write(&path, src).expect("write");
    let p = path.to_string_lossy().to_string();
    let (c_tree, o_tree, e_tree) = run_spid3r(&["run", "--engine", "tree", &p], &[]);
    assert_eq!(
        c_tree, 0,
        "[tree {name}] stderr:\n{e_tree}\nstdout:\n{o_tree}"
    );
    let (c_vm, o_vm, e_vm) = run_spid3r(&["run", "--engine", "vm", &p], &[]);
    assert_eq!(c_vm, 0, "[vm {name}] stderr:\n{e_vm}\nstdout:\n{o_vm}");
    assert_eq!(o_tree, o_vm, "[{name}] tree/vm stdout mismatch");
    let _ = std::fs::remove_dir_all(&dir);
    (o_tree, e_vm)
}

// ------------------------------------------------------------ AUD-SEC-01

#[test]
fn aud_sec_01_spid3r_info_redacts_bearer_and_jwt_under_unrecognized_env_name() {
    let fake_jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJ1c2VyXzQyIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
    let fake_hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let fake_bearer = "Bearer ghp_AbCdEfGhIjKlMnOpQrStUvWxYz123456";

    assert!(runtime_info::looks_like_secret(fake_jwt));
    assert!(runtime_info::looks_like_secret(fake_hex));
    assert!(runtime_info::looks_like_secret(fake_bearer));
    assert!(runtime_info::looks_like_secret("Bearer 12345678"));
    assert!(runtime_info::looks_like_secret("Basic dXNlcjpw"));
    assert!(runtime_info::looks_like_secret(
        "postgres://admin:s3cr3tpass@db.internal:5432/app"
    ));
    assert!(runtime_info::looks_like_secret(
        "http://user:mysecret@proxy.corp:8080"
    ));
    assert!(!runtime_info::looks_like_secret("C:/Users/demo/project"));
    assert!(!runtime_info::looks_like_secret("x86_64-pc-windows-msvc"));
    assert!(!runtime_info::looks_like_secret("service.example.internal"));

    let redacted = runtime_info::redact_value("WORKBUDDY_VLM_CONTEXT", fake_jwt);
    assert!(
        redacted.starts_with("[redacted sha256:"),
        "expected redaction marker, got: {redacted}"
    );

    let (code, out, err) = run_spid3r(&["info", "--json"], &[("WORKBUDDY_VLM_CONTEXT", fake_jwt)]);
    assert_eq!(code, 0, "spid3r info --json failed: {err}");
    assert!(
        !out.contains(fake_jwt),
        "raw secret leaked in default spid3r info --json!"
    );
    assert!(
        out.contains("[redacted sha256:"),
        "expected redaction marker in spid3r info --json"
    );

    let (code2, out2, err2) = run_spid3r(
        &["info", "--json", "--no-redact"],
        &[("WORKBUDDY_VLM_CONTEXT", fake_jwt)],
    );
    assert_eq!(code2, 0, "spid3r info --json --no-redact failed: {err2}");
    assert!(
        out2.contains(fake_jwt),
        "--no-redact must still expose raw value"
    );
}

// ------------------------------------------------------------ AUD-TYPE-01

fn sem_check(src: &str) -> Result<(), String> {
    let prog = parser::parse_file(src, Arc::from("<test>")).map_err(|d| d.message)?;
    semantic::check_file(&prog, true, "<test>").map_err(|d| d.message)
}

#[test]
fn aud_type_01_nullable_and_union_rejected_in_operators_unless_narrowed() {
    // Nullable in arithmetic, unary neg, ordering, and indexing must fail.
    for bad in [
        "fn f(x: int?) -> int { return x + 1; }\nfn main() { return f(1); }\n",
        "fn f(x: int?) -> int { return -x; }\nfn main() { return f(1); }\n",
        "fn f(x: int?) -> bool { return x < 5; }\nfn main() { return 0; }\n",
        "fn f(x: int | string) -> int { return x + 1; }\nfn main() { return 0; }\n",
        "fn f(xs: list?) { return xs[0]; }\nfn main() { return 0; }\n",
        "fn f(xs: list, i: int?) { return xs[i]; }\nfn main() { return 0; }\n",
        "fn main() { let x: int? = null; let y = x + 1; println(y); }\n",
        "fn main() { let x: int? = 1; x = null; let y = x + 1; println(y); }\n",
        "fn f(x: int?) -> int { if (x == null) { return x + 1; } return 0; }\nfn main() { return 0; }\n",
        "fn f(x: int?) -> int { if (x != null) { return 0; } else { return x + 1; } }\nfn main() { return 0; }\n",
        "fn f(x: int?) { x += 1; }\nfn main() { f(1); }\n",
        "fn f(c: bool) { let x: int? = 1; if (c) { x = null; } let y = x + 1; println(y); }\nfn main() { f(true); }\n",
        "fn f(xs: list?) { xs[0] = 1; }\nfn main() { return 0; }\n",
        "fn f(xs: list, i: int?) { xs[i] = 1; }\nfn main() { return 0; }\n",
    ] {
        assert!(
            sem_check(bad).is_err(),
            "expected semantic error for:\n{bad}"
        );
    }

    // Narrowed, null-coalesced, equality-compared, numeric union, and `any` pass.
    for good in [
        "fn f(x: int?) -> int { if (x != null) { return x + 1; } return 0; }\nfn main() { return f(1); }\n",
        "fn f(x: int?) -> int { if (x == null) { return 0; } return x + 1; }\nfn main() { return f(1); }\n",
        "fn f(x: int?) -> int { return (x ?? 0) + 1; }\nfn main() { return f(null); }\n",
        "fn f(x: int?) -> bool { return x == null; }\nfn main() { return 0; }\n",
        "fn f(x: int | float) { return x + 1; }\nfn main() { return 0; }\n",
        "fn f(x: any) { return x + 1; }\nfn main() { return 0; }\n",
        "fn f(c: bool) { let x: int? = 1; if (c) { x = 2; } let y = x + 1; println(y); }\nfn main() { f(true); }\n",
    ] {
        if let Err(e) = sem_check(good) {
            panic!("expected check_ok, got error: {e}\nsrc:\n{good}");
        }
    }
}

// ------------------------------------------------------------ AUD-OBS-08

#[test]
fn aud_obs_08_semver_caret_tilde_wildcards_and_unknown_operator() {
    let src = r#"
import { satisfies } from "semver";

fn main() {
    assert_eq(satisfies("1.2.3", "^1.0.0"), true);
    assert_eq(satisfies("2.0.0", "^1.0.0"), false);
    assert_eq(satisfies("2.0.0-alpha.1", "^1.2.3"), false);
    assert_eq(satisfies("0.2.5", "^0.2.3"), true);
    assert_eq(satisfies("0.3.0", "^0.2.3"), false);
    assert_eq(satisfies("0.0.3", "^0.0.3"), true);
    assert_eq(satisfies("0.0.4", "^0.0.3"), false);
    assert_eq(satisfies("1.2.9", "~1.2.0"), true);
    assert_eq(satisfies("1.3.0", "~1.2.0"), false);
    assert_eq(satisfies("1.3.0-alpha.1", "~1.2.0"), false);
    assert_eq(satisfies("1.5.2", "1.x"), true);
    assert_eq(satisfies("2.0.0", "1.x"), false);
    assert_eq(satisfies("9.9.9", "*"), true);

    let caught = "";
    try {
        satisfies("1.2.3", "!=1.0.0");
    } catch (e) {
        caught = string(e);
    }
    assert(caught.contains("!="), "error must name unknown operator: " + caught);
    println("ok");
    return 0;
}
"#;
    let (out, _) = run_both("semver_ranges", src);
    assert_eq!(out, "ok\n");
}

// ------------------------------------------------------------ AUD-OBS-11

#[test]
fn aud_obs_11_jwt_rejects_empty_secret_and_alg_confusion() {
    let src = r#"
import { jwt_sign, jwt_verify } from "web";

fn b64url(s) {
    let b = bytes(s);
    return b.to_base64().replace("+", "-").replace("/", "_").replace("=", "");
}

fn main() {
    let threw = false;
    try {
        jwt_sign(map("sub", "u1"), "");
    } catch (e) {
        threw = true;
    }
    assert(threw, "jwt_sign with empty secret must throw");

    let tok = jwt_sign(map("sub", "u1"), "real-secret");
    assert(jwt_verify(tok, "").is_err(), "jwt_verify with empty secret must return err");

    // Alg-confusion forgeries: alg=none, alg=RS256, unsigned, tampered sig.
    let h_none = b64url(json_write(map("alg", "none", "typ", "JWT")));
    let h_rs256 = b64url(json_write(map("alg", "RS256", "typ", "JWT")));
    let p_adm = b64url(json_write(map("sub", "admin")));

    let f1 = h_none + "." + p_adm + ".";
    let f2 = h_none + "." + p_adm + ".aaaa";
    // Even if signed with the HMAC secret, non-HS256 header must be rejected.
    let sig_rs = bytes().from_hex(hmac_sha256("real-secret", h_rs256 + "." + p_adm)).to_base64().replace("+", "-").replace("/", "_").replace("=", "");
    let f3 = h_rs256 + "." + p_adm + "." + sig_rs;
    let f4 = tok + "x";

    assert(jwt_verify(f1, "real-secret").is_err(), "f1 alg=none empty sig");
    assert(jwt_verify(f2, "real-secret").is_err(), "f2 alg=none fake sig");
    assert(jwt_verify(f3, "real-secret").is_err(), "f3 alg=RS256 hmac-signed");
    assert(jwt_verify(f4, "real-secret").is_err(), "f4 tampered sig");
    println("ok");
    return 0;
}
"#;
    let (out, _) = run_both("jwt_sec", src);
    assert_eq!(out, "ok\n");
}

// ------------------------------------------------------------ AUD-OBS-10

#[test]
fn aud_obs_10_take_and_composed_gen_pipelines_exit_cleanly_without_drain_warning() {
    let src = r#"
import "gen" as g;

fn main() {
    let a = g.take(g.range_gen(0, 100), 1);
    assert_eq(a, [0]);
    let pipe = g.map(g.filter(g.range_gen(0, 100), fn(x) { return x % 2 == 0; }), fn(x) { return x * 10; });
    let b = g.take(pipe, 3);
    assert_eq(b, [0, 20, 40]);
    println("ok");
    return 0;
}
"#;
    let start = Instant::now();
    let (out, err) = run_both("gen_clean_exit", src);
    assert_eq!(out, "ok\n");
    assert!(
        !err.contains("task(s) still running"),
        "unexpected drain warning:\n{err}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "took {:?} — expected immediate exit without 2000ms drain",
        start.elapsed()
    );
}

// ------------------------------------------------------------ AUD-DX-04

#[test]
fn aud_dx_04_fixed_arity_natives_reject_extra_arguments() {
    // Check every native reported in AUD-DX-04 plus 0-arg / 1-arg / bounded natives.
    let src = r#"
fn expect_err(name, thunk) {
    try {
        thunk();
    } catch (e) {
        return;
    }
    throw "expected arity error for " + name;
}

fn main() {
    expect_err("now(0)", fn() { now(0); });
    expect_err("now_ms(0)", fn() { now_ms(0); });
    expect_err("task_count(0)", fn() { task_count(0); });
    expect_err("gc_live_cells(0)", fn() { gc_live_cells(0); });
    expect_err("gc_collect(0)", fn() { gc_collect(0); });
    expect_err("read_line(0)", fn() { read_line(0); });
    expect_err("config_reload(0)", fn() { config_reload(0); });
    expect_err("args(0)", fn() { args(0); });
    expect_err("cbor_encode(0, 0, 0)", fn() { cbor_encode(0, 0, 0); });
    expect_err("msgpack_encode(0, 0, 0)", fn() { msgpack_encode(0, 0, 0); });
    expect_err("spid3rinfo(1, 2, 3)", fn() { spid3rinfo("map", false, "extra"); });
    println("ok");
    return 0;
}
"#;
    let (out, _) = run_both("arity_sweep", src);
    assert_eq!(out, "ok\n");
}

// ------------------------------------------------------------ AUD-DX-06 / DX-07 / OBS-02 / OBS-04 / COMPAT-01 / COMPAT-02

#[test]
fn aud_dx_06_07_obs_02_04_compat_01_02() {
    let src = r#"
fn main() {
    // AUD-DX-06: priority_queue.push error names (value, priority)
    let pq = priority_queue("min");
    let pq_err = "";
    try {
        pq.push(3);
    } catch (e) {
        pq_err = string(e);
    }
    assert(pq_err.contains("value") and pq_err.contains("priority"), "pq err: " + pq_err);
    pq.push("a", 2);
    pq.push("b", 1);
    assert_eq(pq.pop(), "b");

    // AUD-DX-07: len(ch) == ch.len()
    let ch = channel();
    assert_eq(len(ch), 0);
    ch.send(10);
    ch.send(20);
    assert_eq(len(ch), ch.len());
    assert_eq(len(ch), 2);

    // AUD-OBS-02: set([1,2,2,3]) raises a diagnostic instead of silently building 1-element set
    let set_err = "";
    try {
        let _bad = set([1, 2, 2, 3]);
    } catch (e) {
        set_err = string(e);
    }
    assert(set_err.contains("set(1, 2, 3)"), "set err: " + set_err);
    let good_s = set(1, 2, 2, 3);
    assert_eq(len(good_s), 3);
    assert_eq(good_s.contains(2), true);

    // AUD-OBS-04: sort() places NaN deterministically last
    let nums = [nan(), 2.0, 1.0, inf(), -5.0];
    nums.sort();
    assert_eq(nums[0], -5.0);
    assert_eq(nums[1], 1.0);
    assert_eq(nums[2], 2.0);
    assert_eq(is_inf(nums[3]), true);
    assert_eq(is_nan(nums[4]), true);

    // AUD-COMPAT-01: POSIX leftmost-longest alternation
    assert_eq(regex_find("a|ab", "ab").unwrap(), "ab");

    // AUD-COMPAT-02: Numeric edge cases
    let div0_f = false;
    try { let _x = 1.0 / 0.0; } catch (e) { div0_f = true; }
    assert(div0_f, "float div by zero must throw");
    let div0_i = false;
    try { let _x = 1 / 0; } catch (e) { div0_i = true; }
    assert(div0_i, "int div by zero must throw");
    let ovf = false;
    try { let _x = 9223372036854775807 + 1; } catch (e) { ovf = true; }
    assert(ovf, "i64 overflow must throw");
    let sh_err = false;
    try { let _x = 1 << 64; } catch (e) { sh_err = true; }
    assert(sh_err, "shift >= 64 must throw");
    assert_eq(nan() == nan(), false);
    assert_eq(nan() != nan(), true);
    let nan_ord = false;
    try { let _x = nan() < 1.0; } catch (e) { nan_ord = true; }
    assert(nan_ord, "ordered comparison with NaN must throw");

    println("ok");
    return 0;
}
"#;
    let (out, _) = run_both("collections_and_compat", src);
    assert_eq!(out, "ok\n");
}

// ------------------------------------------------------------ AUD-OBS-03

#[test]
fn aud_obs_03_repl_missing_semicolon_includes_helpful_hint() {
    let mut child = Command::new(bin())
        .arg("repl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn repl");
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"let x = 5\n:quit\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("missing ';' at end of statement"),
        "stderr: {stderr}"
    );
    assert!(
        stderr.contains("require a trailing ';'"),
        "expected helpful hint in REPL stderr: {stderr}"
    );
}

// ------------------------------------------------------------ AUD-DX-02

#[test]
fn aud_dx_02_doc_json_includes_failure_mode() {
    let dir = scratch("doc_fm");
    let src = dir.join("sample.matrix");
    std::fs::write(
        &src,
        r#"
public fn load(p) {
    if p == "" { return err("empty"); }
    return ok(p);
}

public fn find_one(x) {
    if x == 0 { return none; }
    return some(x);
}

public fn must_pos(x) {
    if x < 0 { throw "negative"; }
    return x;
}

public fn double(x) {
    return x * 2;
}

public fn via_if(c) {
    return if (c) { ok(1) } else { err("no") };
}

public fn via_match(x) {
    return match (x) {
        0 => { throw "zero"; },
        _ => { x }
    };
}
"#,
    )
    .expect("write");

    let (code, out, err) = run_spid3r(&["doc", "--json", src.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "doc --json failed: {err}");
    assert!(out.contains("\"failure_mode\": \"result\""), "{out}");
    assert!(out.contains("\"failure_mode\": \"option\""), "{out}");
    assert!(out.contains("\"failure_mode\": \"throws\""), "{out}");
    assert!(out.contains("\"failure_mode\": \"infallible\""), "{out}");
    // Verify match/if expressions are classified too (count >= 2 result, >= 2 throws).
    assert!(
        out.matches("\"failure_mode\": \"result\"").count() >= 2,
        "via_if should report result: {out}"
    );
    assert!(
        out.matches("\"failure_mode\": \"throws\"").count() >= 2,
        "via_match should report throws: {out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------ AUD-BUG-03 (budget parity)

#[test]
fn aud_bug_03_event_loop_run_budget_exhaustion_is_fatal_on_both_engines() {
    let dir = scratch("el_budget");
    let path = dir.join("el_budget.matrix");
    std::fs::write(
        &path,
        r#"
fn main() {
    let t = after_ms(60000);
    on_timer(t, fn(id) { return null; });
    try {
        event_loop_run();
    } catch (e) {
        println("caught:" + string(e));
    }
    println("escaped");
    return 0;
}
"#,
    )
    .expect("write");
    let p = path.to_string_lossy().to_string();
    for eng in ["tree", "vm"] {
        let (code, out, err) = run_spid3r(&["run", "--engine", eng, "--max-steps", "40", &p], &[]);
        assert_eq!(
            code, 1,
            "[{eng}] expected exit 1 on budget exhaustion, stdout={out:?} stderr={err:?}"
        );
        assert!(
            err.contains("execution budget exhausted"),
            "[{eng}] expected 'execution budget exhausted', got stderr={err:?}"
        );
        assert!(
            !out.contains("caught:") && !out.contains("escaped"),
            "[{eng}] fatal budget exhaustion must not be caught or continue; stdout={out:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------ AUD-DX-03

#[test]
fn aud_dx_03_native_doc_states_line_table_limitation() {
    let doc = std::fs::read_to_string("docs/NATIVE.md").expect("read docs/NATIVE.md");
    assert!(
        doc.contains("do not embed a source line table") && doc.contains("rt_throw_v2"),
        "docs/NATIVE.md must document AOT diagnostic limitation"
    );
}

// ------------------------------------------------------------ AUD-BUG-02 (VM fast-path push)

#[test]
fn aud_bug_02_vm_fast_path_list_push_respects_memory_limit_mb() {
    let dir = scratch("mem_list_push");
    let ini = dir.join("limit.ini");
    std::fs::write(&ini, "[runtime]\nmemory_limit_mb = 16\n").expect("write ini");
    let script = dir.join("push_oom.matrix");
    std::fs::write(
        &script,
        r#"
fn main() {
    // Exercises both CallMethod("push") fast path and GetLocal+GetLocal+push+Pop superinstruction.
    try {
        let xs = [];
        let v = 1;
        while true {
            xs.push(v);
            xs.push(2);
        }
    } catch (e) {
        println("CAUGHT:" + string(e));
        return 0;
    }
    println("ESCAPED");
    return 1;
}
"#,
    )
    .expect("write script");
    let sp = script.to_string_lossy().to_string();
    let ip = ini.to_string_lossy().to_string();
    for eng in ["tree", "vm"] {
        let start = Instant::now();
        let (code, out, err) = run_spid3r(&["run", "--engine", eng, &sp], &[("SPID3R_INI", &ip)]);
        assert_eq!(code, 0, "[{eng}] failed: stdout={out:?} stderr={err:?}");
        assert!(
            out.contains("CAUGHT:out of memory"),
            "[{eng}] expected catchable OOM, got stdout={out:?} stderr={err:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "[{eng}] took {:?}, expected fast catchable OOM",
            start.elapsed()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn aud_bug_02_all_growable_containers_and_string_ops_respect_memory_limit_mb() {
    let dir = scratch("mem_containers");
    let ini = dir.join("limit.ini");
    std::fs::write(&ini, "[runtime]\nmemory_limit_mb = 2\n").expect("write ini");
    let ip = ini.to_string_lossy().to_string();

    let cases: &[(&str, &str)] = &[
        (
            "map_index_grow",
            "fn main() { try { let m = {}; let i = 0; while true { m[string(i)] = i; i = i + 1; } } catch (e) { println(\"CAUGHT:\" + string(e)); return 0; } return 1; }",
        ),
        (
            "map_set_prefix_grow",
            "fn main() { try { let m = {}; let i = 0; while true { m.set(\"k_\" + string(i), i); i = i + 1; } } catch (e) { println(\"CAUGHT:\" + string(e)); return 0; } return 1; }",
        ),
        (
            "set_add_grow",
            "fn main() { try { let s = set(); let i = 0; while true { s.add(i); i = i + 1; } } catch (e) { println(\"CAUGHT:\" + string(e)); return 0; } return 1; }",
        ),
        (
            "deque_push_grow",
            "fn main() { try { let d = deque(); while true { d.push_back(1); } } catch (e) { println(\"CAUGHT:\" + string(e)); return 0; } return 1; }",
        ),
        (
            "stack_push_grow",
            "fn main() { try { let s = stack(); while true { s.push(1); } } catch (e) { println(\"CAUGHT:\" + string(e)); return 0; } return 1; }",
        ),
        (
            "queue_push_grow",
            "fn main() { try { let q = queue(); while true { q.push(1); } } catch (e) { println(\"CAUGHT:\" + string(e)); return 0; } return 1; }",
        ),
        (
            "bytes_push_grow",
            "fn main() { try { let b = bytes(); while true { b.push(65); } } catch (e) { println(\"CAUGHT:\" + string(e)); return 0; } return 1; }",
        ),
        (
            "bytes_fill_alloc",
            "fn main() { try { let b = bytes(3000000, 65); println(len(b)); } catch (e) { println(\"CAUGHT:\" + string(e)); return 0; } return 1; }",
        ),
        (
            "linked_list_grow",
            "fn main() { try { let l = linked_list(); while true { l.push_back(1); } } catch (e) { println(\"CAUGHT:\" + string(e)); return 0; } return 1; }",
        ),
        (
            "pq_push_grow",
            "fn main() { try { let p = priority_queue(\"min\"); while true { p.push(1, 1); } } catch (e) { println(\"CAUGHT:\" + string(e)); return 0; } return 1; }",
        ),
        (
            "heap_push_grow",
            "fn main() { try { let h = heap(\"min\"); while true { h.push(1); } } catch (e) { println(\"CAUGHT:\" + string(e)); return 0; } return 1; }",
        ),
        (
            "list_join_blowup",
            "fn main() { try { let xs = []; let i = 0; while i < 1000 { xs.push(\"a\"); i = i + 1; } let sep = \"b\".repeat(3000); let s = xs.join(sep); println(len(s)); } catch (e) { println(\"CAUGHT:\" + string(e)); return 0; } return 1; }",
        ),
        (
            "str_replace_blowup",
            "fn main() { try { let s = \"a\".repeat(1000).replace(\"a\", \"b\".repeat(3000)); println(len(s)); } catch (e) { println(\"CAUGHT:\" + string(e)); return 0; } return 1; }",
        ),
    ];

    for (name, src) in cases {
        let script = dir.join(format!("{name}.matrix"));
        std::fs::write(&script, src).expect("write script");
        let sp = script.to_string_lossy().to_string();
        for eng in ["tree", "vm"] {
            let start = Instant::now();
            let (code, out, err) =
                run_spid3r(&["run", "--engine", eng, &sp], &[("SPID3R_INI", &ip)]);
            assert_eq!(
                code, 0,
                "[{name}/{eng}] expected clean caught exit 0, got {code}: stdout={out:?} stderr={err:?}"
            );
            assert!(
                out.contains("CAUGHT:out of memory"),
                "[{name}/{eng}] expected catchable OOM, got stdout={out:?} stderr={err:?}"
            );
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "[{name}/{eng}] took {:?}, expected fast catchable OOM",
                start.elapsed()
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------ AUD-T-07 (254/254 native sweep)

#[test]
fn aud_t_07_all_254_builtins_panic_free_under_sandbox_and_bad_args() {
    use spid3r_lang::sched::Scheduler;
    use spid3r_lang::stdlib::builtin_defs;
    use spid3r_lang::value::{NativeCtx, Value};
    use std::panic::{catch_unwind, AssertUnwindSafe};

    let defs = builtin_defs();
    assert_eq!(defs.len(), 254, "expected 254 builtin natives");
    let ctx = NativeCtx {
        sched: Scheduler::shared(),
    };

    // Natives whose 0-arg invocation legitimately exits, blocks on stdin, or mutates global shutdown state.
    let skip_zero_arg = ["exit", "read_line", "input", "prompt", "request_shutdown"];
    // Natives whose wrong-type / 6-arg call could mutate process cwd or exit if accepted.
    let bad_six: Vec<Value> = (0..6).map(|_| Value::Bool(false)).collect();
    let bad_one = vec![Value::Null];

    for (name, func) in defs {
        if !skip_zero_arg.contains(&name) {
            let r = catch_unwind(AssertUnwindSafe(|| func(&ctx, vec![])));
            assert!(r.is_ok(), "builtin '{name}' panicked on 0 args");
        }
        let r6 = catch_unwind(AssertUnwindSafe(|| func(&ctx, bad_six.clone())));
        assert!(r6.is_ok(), "builtin '{name}' panicked on 6 bool args");
        let r1 = catch_unwind(AssertUnwindSafe(|| func(&ctx, bad_one.clone())));
        assert!(r1.is_ok(), "builtin '{name}' panicked on 1 null arg");
    }
}
