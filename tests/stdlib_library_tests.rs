//! Differential integration tests for first-party standard library modules:
//! `web.matrix`, `dataframe.matrix`, `stats.matrix`, `ml.matrix`.
//!
//! Every module and function must execute with identical behavior on both
//! `tree` and `vm` engines.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run_engine(engine: &str, file: &str) -> Result<String, String> {
    let o = Command::new(bin())
        .args(["run", "--engine", engine, file])
        .output()
        .map_err(|e| format!("failed to spawn spid3r: {}", e))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).to_string())
    }
}

fn write_tmp(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, src).expect("write tmp matrix file");
    path.to_str().unwrap().to_string()
}

#[test]
fn test_stdlib_web_module() {
    let src = r#"
import "web" as w;

fn main() {
    // 1. Query & form parsing
    let q = w.parse_query("user=Alice+Smith&role=admin&active");
    println("q_user:" + q["user"]);
    println("q_role:" + q["role"]);
    println("q_active:" + q["active"]);

    let f = w.parse_form("title=Data+Science&count=10");
    println("f_title:" + f["title"]);
    println("f_count:" + f["count"]);

    // 2. HTML escaping & templates
    let escaped = w.escape_html("<b>Hello & 'World'</b>");
    println("escaped:" + escaped);

    let tpl = "Hello, {{ name }}! Count is {{ count }}.";
    let rendered = w.render_template(tpl, map("name", "Spid3r User", "count", 42));
    println("rendered:" + rendered);

    // 3. Cookies
    let res0 = w.json_response(200, map("status", "ok"));
    let res1 = w.set_cookie(res0, "auth", "tok123", map("path", "/", "http_only", true));
    println("cookie_hdr:" + res1["headers"]["set-cookie"]);

    let cookies = w.parse_cookies("auth=tok123; session=abc; theme=dark");
    println("parsed_auth:" + cookies["auth"]);
    println("parsed_theme:" + cookies["theme"]);

    // 4. JWT Sign & Verify
    let secret = "test-secret-key-42";
    let token = w.jwt_sign(map("sub", "usr_100", "admin", true), secret);
    let v_ok = w.jwt_verify(token, secret);
    println("jwt_valid:" + string(v_ok.is_ok()));
    let claims = v_ok.unwrap();
    println("jwt_sub:" + claims["sub"]);
    println("jwt_admin:" + string(claims["admin"]));

    let v_bad = w.jwt_verify(token, "wrong-key");
    println("jwt_bad_fails:" + string(v_bad.is_err()));

    // 5. Router & Dispatch
    let app = w.web_app();
    app.get("/items/:id", fn(req) {
        return w.json_response(200, map("item_id", req.params["id"]));
    });
    app.post("/items", fn(req) {
        return w.json_response(201, map("created", true));
    });

    let resp_get = app.handle(map("method", "GET", "path", "/items/99", "headers", map()));
    println("get_status:" + string(resp_get["status"]));
    println("get_body:" + resp_get["body"]);

    let resp_post = app.handle(map("method", "POST", "path", "/items", "headers", map()));
    println("post_status:" + string(resp_post["status"]));
    println("post_body:" + resp_post["body"]);

    let resp_404 = app.handle(map("method", "GET", "path", "/notfound", "headers", map()));
    println("404_status:" + string(resp_404["status"]));
}
"#;
    let path = write_tmp("test_stdlib_web.matrix", src);
    let out_tree = run_engine("tree", &path).expect("tree run web");
    let out_vm = run_engine("vm", &path).expect("vm run web");
    assert_eq!(
        out_tree, out_vm,
        "web module outputs differ between tree and vm"
    );
    assert!(out_tree.contains("jwt_valid:true"));
    assert!(out_tree.contains("get_body:{\"item_id\":\"99\"}"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn test_stdlib_stats_module() {
    let src = r#"
import "stats" as st;

fn main() {
    let data = list(2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0);
    println("mean:" + string(st.mean(data)));
    println("median:" + string(st.median(data)));
    println("mode:" + string(st.mode(data)));
    println("var_sample:" + string(round(st.variance(data, 1) * 100.0) / 100.0));
    println("p25:" + string(st.percentile(data, 25.0)));
    println("p75:" + string(st.percentile(data, 75.0)));
    println("iqr:" + string(st.iqr(data)));

    let x = list(1.0, 2.0, 3.0, 4.0, 5.0);
    let y = list(2.0, 4.0, 6.0, 8.0, 10.0);
    println("covar:" + string(st.covariance(x, y)));
    println("pearson:" + string(st.pearson(x, y)));
    println("spearman:" + string(st.spearman(x, y)));

    let z = st.zscore(x);
    println("z0:" + string(round(z[0] * 1000.0) / 1000.0));

    // Distribution functions
    let pdf0 = st.normal_pdf(0.0, 0.0, 1.0);
    println("pdf0_gt_039:" + string(pdf0 > 0.39));
    let cdf0 = st.normal_cdf(0.0, 0.0, 1.0);
    println("cdf0_eq_05:" + string(round(cdf0 * 10.0) == 5.0));

    // Hypothesis tests
    let a = list(1.0, 2.0, 3.0, 2.0, 1.0);
    let b = list(10.0, 11.0, 12.0, 11.0, 10.0);
    let t_res = st.t_test_ind(a, b);
    println("t_stat_neg:" + string(t_res["statistic"] < 0.0));
    println("t_p_sig:" + string(t_res["pvalue"] < 0.01));

    let chi_res = st.chi_square(list(10.0, 10.0, 10.0), null);
    println("chi_zero:" + string(chi_res["statistic"] == 0.0));

    // Train-test split
    let split = st.train_test_split(x, y, 0.4, 42);
    println("train_n:" + string(split["X_train"].len()));
    println("test_n:" + string(split["X_test"].len()));
}
"#;
    let path = write_tmp("test_stdlib_stats.matrix", src);
    let out_tree = run_engine("tree", &path).expect("tree run stats");
    let out_vm = run_engine("vm", &path).expect("vm run stats");
    assert_eq!(
        out_tree, out_vm,
        "stats module outputs differ between tree and vm"
    );
    assert!(out_tree.contains("mean:5.0"));
    assert!(out_tree.contains("mode:4.0"));
    assert!(out_tree.contains("pearson:1.0"));
    assert!(out_tree.contains("spearman:1.0"));
    assert!(out_tree.contains("t_stat_neg:true"));
    assert!(out_tree.contains("train_n:3"));
    assert!(out_tree.contains("test_n:2"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn test_stdlib_modules_differential_three_engines() {
    let src = r#"
import "web" as web;
import "stats" as stats;

fn main() {
    let q = web.parse_query("page=2&sort=desc");
    println("web:" + q["page"] + ":" + q["sort"]);
    let esc = web.escape_html("<data>");
    println("web_esc:" + esc);

    let nums = list(10.0, 20.0, 30.0, 40.0, 50.0);
    println("stats_mean:" + string(stats.mean(nums)));
    println("stats_median:" + string(stats.median(nums)));

    return 0;
}
"#;
    let path = write_tmp("test_stdlib_all_four.matrix", src);
    let out_tree = run_engine("tree", &path).expect("tree run all four");
    let out_vm = run_engine("vm", &path).expect("vm run all four");
    assert_eq!(out_tree, out_vm, "tree and vm outputs differ");

    let comp_path = std::env::temp_dir().join("test_stdlib_all_four.matrixc");
    let comp = Command::new(bin())
        .args(["compile", &path, "-o", comp_path.to_str().unwrap()])
        .output()
        .expect("compile all four");
    assert!(
        comp.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&comp.stderr)
    );

    let exec = Command::new(bin())
        .args(["exec", comp_path.to_str().unwrap()])
        .output()
        .expect("exec all four");
    assert!(
        exec.status.success(),
        "exec failed: {}",
        String::from_utf8_lossy(&exec.stderr)
    );
    let out_exec = String::from_utf8_lossy(&exec.stdout).to_string();
    assert_eq!(out_tree, out_exec, "compile+exec differed from tree/vm");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&comp_path);
}

/// `render_template` must not re-scan what it substitutes. The pre-fix version
/// replaced one context key at a time into the *mutating* output, so a user
/// value containing `{{ api_key }}` was expanded into the server's secret
/// (audit F-03). Values are now appended once and never re-parsed.
const TEMPLATE_NO_REEXPANSION: &str = r#"
import "web" as w;

// "{{" + inner + "}}" - the no-space form cannot be written in source, because
// `{ident}` inside a string literal is a language interpolation hole.
fn dbl(inner) { return "{" + "{" + inner + "}" + "}"; }

fn main() {
    // A value that looks like template syntax stays literal text.
    println(w.render_template("<p>{{ name }}</p>", map("name", "{{ api_key }}", "api_key", "SECRET")));
    println(w.render_template("<p>{{ name }}</p>", map("name", "{{{ api_key }}}", "api_key", "SECRET")));
    println(w.render_template("<p>" + dbl("name") + "</p>", map("name", dbl("api_key"), "api_key", "SECRET")));
    // Normal substitution, escaping, the raw form and unknown keys are unchanged.
    println(w.render_template("Hi {{ name }} & {{ html }}", map("name", "Ada", "html", "<b>")));
    println(w.render_template("{{ html }} / {{{ html }}}", map("html", "<b>")));
    println(w.render_template("x {{ nope }} y", map("k", "v")));
    return 0;
}
"#;

#[test]
fn render_template_never_re_expands_substituted_values() {
    let path = write_tmp("template_no_reexpansion.matrix", TEMPLATE_NO_REEXPANSION);
    let expected = "<p>{{ api_key }}</p>\n\
                    <p>{{{ api_key }}}</p>\n\
                    <p>{{api_key}}</p>\n\
                    Hi Ada & &lt;b&gt;\n\
                    &lt;b&gt; / <b>\n\
                    x {{ nope }} y\n";
    for engine in ["tree", "vm"] {
        let out = run_engine(engine, &path).unwrap_or_else(|e| panic!("[{}] {}", engine, e));
        assert_eq!(out, expected, "[{}] render_template output", engine);
    }
    let _ = std::fs::remove_file(&path);
}

/// F-06: `jwt_verify` must compare signatures in constant time and enforce
/// `exp`/`nbf` with a bounded skew allowance. Before the fix an expired token
/// verified, and the byte-wise `!=` leaked the length of a matching prefix.
const JWT_CLAIMS: &str = r#"
import "web";

fn main() {
    let s = "k";
    let now = unix_time();
    println(jwt_verify(jwt_sign(map("exp", now + 3600), s), s).is_ok());
    println(jwt_verify(jwt_sign(map("exp", now - 3600), s), s).unwrap_err());
    // 30 s past `exp` is inside the 60 s allowance, and outside it when strict.
    println(jwt_verify(jwt_sign(map("exp", now - 30), s), s).is_ok());
    println(jwt_verify(jwt_sign(map("exp", now - 30), s), s, 0).unwrap_err());
    println(jwt_verify(jwt_sign(map("nbf", now + 3600), s), s).unwrap_err());
    println(jwt_verify(jwt_sign(map("exp", now + 3600), s), "other").unwrap_err());
    // A signed payload that is not a JSON object is refused.
    println(jwt_verify(jwt_sign(list(1, 2), s), s).unwrap_err());
    return 0;
}
"#;

#[test]
fn jwt_verify_enforces_exp_and_nbf_with_skew() {
    let path = write_tmp("jwt_claims.matrix", JWT_CLAIMS);
    let expected = "true\n\
                    expired JWT\n\
                    true\n\
                    expired JWT\n\
                    JWT is not valid yet (nbf)\n\
                    invalid JWT signature\n\
                    invalid JWT: payload is not a JSON object\n";
    for engine in ["tree", "vm"] {
        let out = run_engine(engine, &path).unwrap_or_else(|e| panic!("[{}] {}", engine, e));
        assert_eq!(out, expected, "[{}] jwt_verify output", engine);
    }
    let _ = std::fs::remove_file(&path);
}

/// F-07: one cookie builder that always emits `SameSite`, `Secure` on request,
/// and a `set_cookie` that appends instead of overwriting the previous cookie.
const COOKIES: &str = r#"
import "web";

fn main() {
    println(cookie("a", "1", null));
    println(cookie("a", "1", map("secure", true)));
    println(cookie("a", "1", map("https", true)));
    println(cookie("a", "1", map("same_site", "Strict")));
    let res = json_response(200, map("ok", true));
    set_cookie(res, "a", "1", null);
    set_cookie(res, "b", "2", null);
    set_cookie(res, "c", "3", null);
    let n = 0;
    let names = "";
    for item in res["headers"] {
        if item["name"] == "set-cookie" {
            n = n + 1;
            names = names + string(item["value"]).slice(0, 1);
        }
    }
    println("cookies: " + string(n) + " " + names);
    return 0;
}
"#;

#[test]
fn cookies_always_carry_samesite_and_multiple_survive() {
    let path = write_tmp("cookies_attrs.matrix", COOKIES);
    let expected = "a=1; Path=/; SameSite=Lax\n\
                    a=1; Path=/; SameSite=Lax; Secure\n\
                    a=1; Path=/; SameSite=Lax; Secure\n\
                    a=1; Path=/; SameSite=Strict\n\
                    cookies: 3 abc\n";
    for engine in ["tree", "vm"] {
        let out = run_engine(engine, &path).unwrap_or_else(|e| panic!("[{}] {}", engine, e));
        assert_eq!(out, expected, "[{}] cookie output", engine);
    }
    let _ = std::fs::remove_file(&path);
}

/// F-14: the CSRF middleware issues a token on a safe method and refuses a
/// state-changing method that does not repeat it. Called directly (no server) so
/// the assertions are deterministic.
const CSRF: &str = r#"
import "http";
import "web";
import "middleware" as mw;

fn ok(req) { return http_text(200, "ok"); }

fn main() {
    let m = mw.mw_csrf(map());
    let r1 = m(map("method", "GET", "headers", map()), ok);
    let set = string(r1["headers"]["set-cookie"]);
    println("issued=" + string(set.starts_with("spid3r_csrf=")) + " lax=" + string(set.contains("SameSite=Lax")));
    let tok = set.slice(12, set.index_of(";"));
    let r2 = m(map("method", "POST", "headers", map("cookie", "spid3r_csrf=" + tok)), ok);
    println("no_header=" + string(r2["status"]));
    let r3 = m(map("method", "POST", "headers", map("cookie", "spid3r_csrf=" + tok, "x-csrf-token", tok)), ok);
    println("with_header=" + string(r3["status"]));
    let r4 = m(map("method", "POST", "headers", map("cookie", "spid3r_csrf=" + tok, "x-csrf-token", "wrong")), ok);
    println("wrong_header=" + string(r4["status"]));
    return 0;
}
"#;

#[test]
fn csrf_middleware_requires_a_matching_token() {
    let path = write_tmp("csrf_mw.matrix", CSRF);
    let expected = "issued=true lax=true\nno_header=403\nwith_header=200\nwrong_header=403\n";
    for engine in ["tree", "vm"] {
        let out = run_engine(engine, &path).unwrap_or_else(|e| panic!("[{}] {}", engine, e));
        assert_eq!(out, expected, "[{}] mw_csrf output", engine);
    }
    let _ = std::fs::remove_file(&path);
}
