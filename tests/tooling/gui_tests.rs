//! GUI layer tests (Phase 6).
//!
//! A real window cannot be driven headlessly, so the integration tests use
//! `spid3r gui --dry-run`, which replays the whole bridge — handlers
//! dispatched in order, JavaScript printed instead of evaluated. The pure
//! bridge logic (queues, JSON, handler pairing) is unit-tested in
//! `src/gui/bridge.rs`; the window itself is exercised manually.

#![cfg(feature = "gui")]

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn gui(args: &[&str]) -> Result<String, String> {
    let o = Command::new(bin())
        .args(args)
        .output()
        .map_err(|e| format!("spawn failed: {}", e))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).to_string())
    }
}

fn write_case(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("spid3r_gui_{}.matrix", name));
    std::fs::write(&path, src).expect("write case");
    path.to_string_lossy().to_string()
}

#[test]
fn dry_run_replays_the_counter_demo() {
    // The demo: two synthetic clicks (from main), then `ready` (the page load
    // event) which installs the HTML and re-renders the count.
    let src = r#"
        let clicks = 0;
        fn set_count() {
            let label = "clicks: " + string(clicks);
            gui_eval("document.getElementById('count').textContent = " + json_write(label) + ";");
        }
        fn on_ready(event, payload) {
            gui_set_html("<button id=\"go\" onclick=\"window.spid3r.emit('click','go')\">click me</button> <span id=\"count\"></span>");
            set_count();
            return null;
        }
        fn on_click(event, payload) {
            clicks = clicks + 1;
            set_count();
            return null;
        }
        fn main() {
            gui_title("spid3r gui - counter");
            gui_on("ready", on_ready);
            gui_on("click", on_click);
            gui_emit("click", "go");
            gui_emit("click", "go");
            return 0;
        }
    "#;
    let path = write_case("counter", src);
    let expected = concat!(
        "js: document.getElementById('count').textContent = \"clicks: 1\";\n",
        "js: document.getElementById('count').textContent = \"clicks: 2\";\n",
        "js: document.getElementById('spid3r-root').innerHTML = \"<button id=\\\"go\\\" onclick=\\\"window.spid3r.emit('click','go')\\\">click me</button> <span id=\\\"count\\\"></span>\";\n",
        "js: document.getElementById('count').textContent = \"clicks: 2\";\n",
    );
    let out = gui(&["gui", &path, "--dry-run"]).expect("dry-run must succeed");
    assert_eq!(out, expected);
}

#[test]
fn dry_run_uses_program_events_and_handlers() {
    let src = r#"
        fn on_ping(event, payload) {
            println("ping: " + event + "/" + payload);
            gui_eval("1");
            return null;
        }
        fn main() {
            gui_on("ping", on_ping);
            gui_emit("ping", "hello");
            gui_emit("nobody-cares", "dropped");
            return 0;
        }
    "#;
    let path = write_case("events", src);
    let out = gui(&["gui", &path, "--dry-run"]).expect("dry-run must succeed");
    assert_eq!(out, "ping: ping/hello\njs: 1\n");
}

#[test]
fn gui_on_rejects_non_callable_handlers() {
    let src = r#"
        fn main() {
            gui_on("click", 5);
            return 0;
        }
    "#;
    let path = write_case("bad_handler", src);
    let err = gui(&["gui", &path, "--dry-run"]).expect_err("must fail");
    assert!(
        err.contains("gui_on expects a function as the handler"),
        "unexpected error: {}",
        err
    );
}

#[test]
fn gui_eval_checks_its_argument_count() {
    let src = r#"
        fn main() {
            gui_eval("a", "b");
            return 0;
        }
    "#;
    let path = write_case("bad_arity", src);
    let err = gui(&["gui", &path, "--dry-run"]).expect_err("must fail");
    assert!(
        err.contains("gui_eval expects 1 argument(s), got 2"),
        "unexpected error: {}",
        err
    );
}

#[test]
fn gui_rejects_unknown_options_and_missing_files() {
    let dummy = write_case("dummy", "fn main() { return 0; }");
    let err = gui(&["gui", &dummy, "--nope"]).expect_err("must fail");
    assert!(err.contains("unknown option '--nope'"), "got: {}", err);

    let err = gui(&["gui", "examples/does_not_exist.matrix"]).expect_err("must fail");
    assert!(err.contains("is not a file"), "got: {}", err);

    let err = gui(&["gui"]).expect_err("must fail");
    assert!(err.contains("gui needs a source file"), "got: {}", err);
}

#[test]
fn gui_requires_main() {
    let src = "fn helper() { return 0; }\n";
    let path = write_case("no_main", src);
    let err = gui(&["gui", &path, "--dry-run"]).expect_err("must fail");
    assert!(
        err.contains("fn main") || err.contains("main"),
        "unexpected error: {}",
        err
    );
}

#[test]
fn gui_names_are_unknown_without_the_natives() {
    // A program that calls a GUI native under `run` (no GUI engine): the
    // semantic pass knows the name, so the failure is a clean runtime error
    // rather than a crash or a panic.
    let src = r#"
        fn main() {
            gui_eval("1");
            return 0;
        }
    "#;
    let path = write_case("no_engine", src);
    let err = Command::new(bin())
        .args(["run", &path])
        .output()
        .map_err(|e| format!("spawn failed: {}", e))
        .expect("spawn");
    assert!(!err.status.success());
    let stderr = String::from_utf8_lossy(&err.stderr);
    assert!(
        stderr.contains("gui_eval"),
        "the error should mention the native: {}",
        stderr
    );
}

#[test]
fn dry_run_dispatches_events_in_order_and_drops_unknown() {
    let src = r#"
        fn on_a(event, payload) { println("a:" + payload); return null; }
        fn on_b(event, payload) { println("b:" + payload); return null; }
        fn main() {
            gui_on("a", on_a);
            gui_on("b", on_b);
            gui_emit("a", "1");
            gui_emit("zzz", "dropped");   // no handler -> dropped
            gui_emit("b", "2");
            gui_emit("a", "3");
            return 0;
        }
    "#;
    let path = write_case("order", src);
    let out = gui(&["gui", &path, "--dry-run"]).expect("dry-run must succeed");
    assert_eq!(out, "a:1\nb:2\na:3\n");
}

#[test]
fn dry_run_passes_the_payload_through_verbatim() {
    let src = r#"
        fn on_x(event, payload) { println("[" + payload + "]"); return null; }
        fn main() {
            gui_on("x", on_x);
            gui_emit("x", "q\"uote\nnewline");
            return 0;
        }
    "#;
    let path = write_case("payload", src);
    let out = gui(&["gui", &path, "--dry-run"]).expect("dry-run must succeed");
    assert_eq!(out, "[q\"uote\nnewline]\n");
}

#[test]
fn dry_run_prints_set_html_before_handler_queued_js() {
    let src = r#"
        fn on_go(event, payload) { gui_eval("render()"); return null; }
        fn main() {
            gui_set_html("<div id=\"x\"></div>");
            gui_on("go", on_go);
            gui_emit("go", "");
            return 0;
        }
    "#;
    let path = write_case("sethtml", src);
    let out = gui(&["gui", &path, "--dry-run"]).expect("dry-run must succeed");
    let html_at = out.find("innerHTML").expect("set_html js missing");
    let render_at = out.find("js: render()").expect("handler js missing");
    assert!(
        html_at < render_at,
        "set_html must precede handler js:/n{}",
        out
    );
    assert!(
        out.contains("\"<div id=\\\"x\\\"></div>\";"),
        "html must be JSON-quoted in the emitted JS:/n{}",
        out
    );
}

// `--payload` runs a program embedded by `build --embed`. Building an exe
// needs LLVM, so these cover the resolution rules instead: a missing path, a
// file with no payload, and the loose-folder fallback beside the executable.

#[test]
fn payload_rejects_a_missing_executable() {
    let missing = std::env::temp_dir().join("spid3r_gui_does_not_exist.exe");
    let err = gui(&["gui", "--payload", &missing.to_string_lossy(), "--dry-run"])
        .expect_err("a missing --payload target must fail");
    assert!(
        err.contains("no such file"),
        "expected a 'no such file' diagnostic, got:/n{}",
        err
    );
}

#[test]
fn payload_without_embedding_or_loose_files_explains_itself() {
    let dir = std::env::temp_dir().join(format!("spid3r_gui_payload_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    // A file that carries no payload: the magic is absent, so the loader must
    // fall through to the folder layout and, finding nothing, say so.
    let plain = dir.join("App.exe");
    std::fs::write(&plain, b"MZ not really a payload").expect("write");

    let err = gui(&["gui", "--payload", &plain.to_string_lossy(), "--dry-run"])
        .expect_err("a payload-less exe with nothing beside it must fail");
    assert!(
        err.contains("no embedded payload"),
        "expected the fallback diagnostic, got:/n{}",
        err
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn payload_falls_back_to_a_matrix_beside_the_executable() {
    let dir = std::env::temp_dir().join(format!("spid3r_gui_loose_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let app = dir.join("App.exe");
    std::fs::write(&app, b"MZ no payload here").expect("write exe");
    std::fs::write(
        dir.join("loose.matrix"),
        "fn main() {\n    gui_eval(\"window.loose = 1;\");\n    return 0;\n}\n",
    )
    .expect("write program");

    let out = gui(&["gui", "--payload", &app.to_string_lossy(), "--dry-run"])
        .expect("the loose .matrix beside the exe should run");
    assert!(
        out.contains("window.loose = 1;"),
        "expected the fallback program to run, got:/n{}",
        out
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn gh2_gui_on_supports_one_and_two_arg_handlers_and_rejects_bad_arity() {
    let ok_src = r#"
        fn one_arg(payload) {
            println("one:" + payload);
            return null;
        }
        fn two_arg(ev, payload) {
            println("two:" + ev + ":" + payload);
            return null;
        }
        fn defaulted(ev, payload = "def") {
            println("opt:" + ev + ":" + payload);
            return null;
        }
        fn main() {
            gui_on("a", one_arg);
            gui_on("b", two_arg);
            gui_on("c", defaulted);
            gui_on("d", p => { println("arrow:" + p); return null; });
            gui_emit("a", "alpha");
            gui_emit("b", "beta");
            gui_emit("c", "gamma");
            gui_emit("d", "delta");
            return 0;
        }
    "#;
    let ok_path = write_case("gh2_arity_ok", ok_src);
    for extra in [&[][..], &["--engine", "vm"][..]] {
        let mut args = vec!["gui", ok_path.as_str(), "--dry-run"];
        args.extend_from_slice(extra);
        let out = gui(&args).expect("1-arg and 2-arg handlers must succeed");
        assert_eq!(out, "one:alpha\ntwo:b:beta\nopt:c:gamma\narrow:delta\n");
    }

    let zero_src = r#"
        fn bad0() { return null; }
        fn main() { gui_on("x", bad0); return 0; }
    "#;
    let zero_path = write_case("gh2_arity_zero", zero_src);
    let err0 = gui(&["gui", &zero_path, "--dry-run"]).expect_err("0-arg handler must fail");
    assert!(err0.contains("1 argument"), "got: {}", err0);

    let three_src = r#"
        fn bad3(a, b, c) { return null; }
        fn main() { gui_on("x", bad3); return 0; }
    "#;
    let three_path = write_case("gh2_arity_three", three_src);
    let err3 = gui(&["gui", &three_path, "--dry-run"]).expect_err("3-arg handler must fail");
    assert!(err3.contains("2 arguments"), "got: {}", err3);
}

#[test]
fn gh2_handler_runtime_error_reports_real_source_span() {
    let src = r#"
        fn boom(payload) {
            let x = 1 / 0;
            return null;
        }
        fn main() {
            gui_on("ready", boom);
            return 0;
        }
    "#;
    let path = write_case("gh2_span", src);
    for extra in [&[][..], &["--engine", "vm"][..]] {
        let mut args = vec!["gui", path.as_str(), "--dry-run"];
        args.extend_from_slice(extra);
        let out = Command::new(bin())
            .args(&args)
            .output()
            .expect("spawn gui handler span");
        assert!(out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("<gui>:0"),
            "expected real file:line span, got:\n{}",
            stderr
        );
        assert!(
            stderr.contains("spid3r_gui_gh2_span.matrix"),
            "expected source file name in stderr:\n{}",
            stderr
        );
    }
}

#[test]
fn gh3_spawned_worker_can_push_gui_updates() {
    let src = r#"
        fn worker() {
            gui_title("worker-title");
            gui_eval("window.fromWorker = true;");
            return null;
        }
        fn main() {
            spawn worker;
            return 0;
        }
    "#;
    let path = write_case("gh3_worker", src);
    for extra in [&[][..], &["--engine", "vm"][..]] {
        let mut args = vec!["gui", path.as_str(), "--dry-run"];
        args.extend_from_slice(extra);
        let out = gui(&args).expect("worker gui calls must succeed");
        assert!(
            out.contains("js: window.fromWorker = true;"),
            "got:\n{}",
            out
        );
    }
}

#[test]
fn gh4_cooperative_shutdown_stops_spawned_loop_quickly() {
    let src = r#"
        fn poller() {
            while not shutdown_requested() {
                sleep_ms(10);
            }
            gui_eval("window.pollerDone = true;");
            return null;
        }
        fn on_ready(ev, payload) {
            request_shutdown();
            return null;
        }
        fn main() {
            gui_on("ready", on_ready);
            spawn poller;
            return 0;
        }
    "#;
    let path = write_case("gh4_shutdown", src);
    for extra in [&[][..], &["--engine", "vm"][..]] {
        let mut args = vec!["gui", path.as_str(), "--dry-run"];
        args.extend_from_slice(extra);
        let start = std::time::Instant::now();
        let out = gui(&args).expect("cooperative shutdown must succeed");
        let elapsed = start.elapsed();
        assert!(
            out.contains("js: window.pollerDone = true;"),
            "worker should finish and flush after request_shutdown, got:\n{}",
            out
        );
        assert!(
            elapsed < std::time::Duration::from_millis(1000),
            "took {:?}; should not wait for 2000ms drain timeout",
            elapsed
        );
    }
}

#[test]
fn gh5_imported_module_can_call_gui_natives_in_gui_mode_only() {
    let dir = std::env::temp_dir().join(format!("spid3r_gui_mod_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let ui_mod = dir.join("ui_helper.matrix");
    std::fs::write(
        &ui_mod,
        r#"
public fn setup_ui(msg: string) {
    gui_title("from module");
    gui_eval("window.modMsg = " + json_write(msg) + ";");
    return null;
}
"#,
    )
    .expect("write ui_helper");
    let main_file = dir.join("main.matrix");
    std::fs::write(
        &main_file,
        r#"
import "ui_helper" as ui;
fn main() {
    ui.setup_ui("hello-mod");
    return 0;
}
"#,
    )
    .expect("write main");
    let main_str = main_file.to_string_lossy().to_string();
    for extra in [&[][..], &["--engine", "vm"][..]] {
        let mut args = vec!["gui", main_str.as_str(), "--dry-run"];
        args.extend_from_slice(extra);
        let out = gui(&args).expect("module calling gui_* under gui mode must succeed");
        assert!(
            out.contains("js: window.modMsg = \"hello-mod\";"),
            "got:\n{}",
            out
        );
    }

    // `spid3r run` must still fail cleanly with gui_* undefined.
    let run_out = Command::new(bin())
        .args(["run", &main_str])
        .output()
        .expect("spawn run");
    assert!(!run_out.status.success());
    let stderr = String::from_utf8_lossy(&run_out.stderr);
    assert!(
        stderr.contains("gui_title") || stderr.contains("gui_eval"),
        "expected gui native error under run, got:\n{}",
        stderr
    );
    let _ = std::fs::remove_dir_all(&dir);
}
