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
    let expected = concat!(
        "js: document.getElementById('count').textContent = \"clicks: 1\";\n",
        "js: document.getElementById('count').textContent = \"clicks: 2\";\n",
        "js: document.getElementById('spid3r-root').innerHTML = \"<button id=\\\"go\\\" onclick=\\\"window.spid3r.emit('click','go')\\\">click me</button> <span id=\\\"count\\\"></span>\";\n",
        "js: document.getElementById('count').textContent = \"clicks: 2\";\n",
    );
    let out = gui(&["gui", "examples/gui/counter.matrix", "--dry-run"])
        .expect("dry-run must succeed");
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
    let err = gui(&["gui", "examples/gui/counter.matrix", "--nope"]).expect_err("must fail");
    assert!(err.contains("unknown option '--nope'"), "got: {}", err);

    let err = gui(&["gui", "examples/gui/does_not_exist.matrix"]).expect_err("must fail");
    assert!(err.contains("is not a file"), "got: {}", err);

    let err = gui(&["gui"]).expect_err("must fail");
    assert!(
        err.contains("gui needs a source file"),
        "got: {}",
        err
    );
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
    assert!(html_at < render_at, "set_html must precede handler js:/n{}", out);
    assert!(
        out.contains("\"<div id=\\\"x\\\"></div>\";"),
        "html must be JSON-quoted in the emitted JS:/n{}",
        out
    );
}
