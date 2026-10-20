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
    assert_eq!(out_tree, out_vm, "web module outputs differ between tree and vm");
    assert!(out_tree.contains("jwt_valid:true"));
    assert!(out_tree.contains("get_body:{\"item_id\":\"99\"}"));
    let _ = std::fs::remove_file(path);
}

#[test]
fn test_stdlib_dataframe_module() {
    let src = r#"
import "dataframe" as df_mod;

fn main() {
    let records = list(
        map("name", "Alice", "dept", "Engineering", "salary", 120.0),
        map("name", "Bob", "dept", "Engineering", "salary", 110.0),
        map("name", "Charlie", "dept", "Marketing", "salary", 90.0),
        map("name", "Dave", "dept", "Marketing", "salary", 95.0),
        map("name", "Eve", "dept", "HR", "salary", 80.0)
    );

    let df = df_mod.from_records(records);
    println("df_rows:" + string(df.num_rows));
    println("df_cols:" + string(df.col_names.len()));

    // select
    let sel = df.select(list("name", "salary"));
    println("sel_cols:" + string(sel.col_names));

    // filter
    let high_earners = df.filter(fn(r) { return r["salary"] >= 100.0; });
    println("high_earners_rows:" + string(high_earners.num_rows));

    // sort_by
    let sorted = df.sort_by("salary", true);
    println("top_earner:" + sorted.get_row(0)["name"]);

    // group_by
    let grp = df.group_by("dept", map("salary", "mean"));
    println("dept_groups_count:" + string(grp.num_rows));

    // head
    let top2 = df.head(2);
    println("head_len:" + string(top2.num_rows));

    // drop_na & fill_na
    let with_nulls = df_mod.from_records(list(
        map("a", 1.0, "b", null),
        map("a", null, "b", 2.0),
        map("a", 3.0, "b", 4.0)
    ));
    let dropped = with_nulls.drop_na("b");
    println("dropped_rows:" + string(dropped.num_rows));
    let filled = with_nulls.fill_na("b", 0.0);
    println("filled_rows:" + string(filled.num_rows));
    println("filled_b0:" + string(filled.get_row(0)["b"]));

    // describe
    let stats = df.describe();
    println("describe_rows:" + string(stats.num_rows));
}
"#;
    let path = write_tmp("test_stdlib_dataframe.matrix", src);
    let out_tree = run_engine("tree", &path).expect("tree run dataframe");
    let out_vm = run_engine("vm", &path).expect("vm run dataframe");
    assert_eq!(out_tree, out_vm, "dataframe module outputs differ between tree and vm");
    assert!(out_tree.contains("df_rows:5"));
    assert!(out_tree.contains("high_earners_rows:2"));
    assert!(out_tree.contains("top_earner:Alice"));
    assert!(out_tree.contains("dept_groups_count:3"));
    assert!(out_tree.contains("dropped_rows:2"));
    assert!(out_tree.contains("filled_b0:0.0"));
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
    assert_eq!(out_tree, out_vm, "stats module outputs differ between tree and vm");
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
fn test_stdlib_ml_module() {
    let src = r#"
import "ml" as ml;

fn main() {
    // 1. Linear regression
    let X = matrix(4, 1);
    X.set(0, 0, 1.0);
    X.set(1, 0, 2.0);
    X.set(2, 0, 3.0);
    X.set(3, 0, 4.0);
    let y = vector(list(3.0, 5.0, 7.0, 9.0)); // y = 2x + 1

    let lin = ml.linear_regression_fit(X, y);
    let lin_preds = ml.linear_regression_predict(lin, X);
    println("lin_r2_high:" + string(ml.r2_score(y, lin_preds) > 0.999));
    println("lin_pred_0:" + string(round(lin_preds[0])));

    // 2. Logistic regression
    let X_log = matrix(6, 1);
    X_log.set(0, 0, 0.5);
    X_log.set(1, 0, 1.0);
    X_log.set(2, 0, 1.5);
    X_log.set(3, 0, 7.0);
    X_log.set(4, 0, 8.0);
    X_log.set(5, 0, 9.0);
    let y_log = vector(list(0.0, 0.0, 0.0, 1.0, 1.0, 1.0));

    let log_mod = ml.logistic_regression_fit(X_log, y_log, 0.2, 200);
    let log_preds = ml.logistic_regression_predict(log_mod, X_log);
    let acc = ml.accuracy(y_log, log_preds);
    println("log_acc_high:" + string(acc >= 0.8));

    // 3. KNN
    let knn_preds = ml.knn_predict(X_log, y_log, X_log, 3);
    println("knn_acc_1:" + string(ml.accuracy(y_log, knn_preds) == 1.0));

    // 4. KMeans
    let km = ml.kmeans_fit(X_log, 2, 50);
    println("km_k:" + string(km["k"]));
    println("km_inertia_nonneg:" + string(km["inertia"] >= 0.0));

    // 5. PCA
    let X_pca = matrix(3, 3);
    X_pca.set(0, 0, 1.0); X_pca.set(0, 1, 2.0); X_pca.set(0, 2, 3.0);
    X_pca.set(1, 0, 2.0); X_pca.set(1, 1, 4.0); X_pca.set(1, 2, 6.0);
    X_pca.set(2, 0, 3.0); X_pca.set(2, 1, 6.0); X_pca.set(2, 2, 9.0);
    let pca = ml.pca_fit_transform(X_pca, 2);
    let tr = pca["transformed"];
    println("pca_shape:" + string(tr.rows()) + "x" + string(tr.cols()));

    // 6. Metrics
    let yt = vector(list(1.0, 0.0, 1.0, 1.0));
    let yp = vector(list(1.0, 0.0, 1.0, 0.0));
    println("cm_tp:" + string(ml.confusion_matrix(yt, yp)["tp"]));
    println("prec:" + string(ml.precision(yt, yp)));
    println("rec:" + string(round(ml.recall(yt, yp) * 100.0) / 100.0));
    println("f1:" + string(round(ml.f1_score(yt, yp) * 10.0) / 10.0));
    println("mse:" + string(ml.mse(yt, yp)));
    println("rmse:" + string(ml.rmse(yt, yp)));
}
"#;
    let path = write_tmp("test_stdlib_ml.matrix", src);
    let out_tree = run_engine("tree", &path).expect("tree run ml");
    let out_vm = run_engine("vm", &path).expect("vm run ml");
    assert_eq!(out_tree, out_vm, "ml module outputs differ between tree and vm");
    assert!(out_tree.contains("lin_r2_high:true"));
    assert!(out_tree.contains("lin_pred_0:3.0"));
    assert!(out_tree.contains("log_acc_high:true"));
    assert!(out_tree.contains("knn_acc_1:true"));
    assert!(out_tree.contains("pca_shape:3x2"));
    assert!(out_tree.contains("cm_tp:2"));
    let _ = std::fs::remove_file(path);
}
