//! Embedding demo: a Rust host running Spid3r as a scripting layer.
//!
//! Run with: `cargo run --example embed_hello`
//! Full API reference: docs/EMBEDDING.md

use spid3r::embed::{conv, Engine};

fn main() {
    // 1. Configure the engine: host functions + host globals.
    let engine = Engine::new()
        .register("shout", |args| {
            let s = conv::as_str(args.first().unwrap_or(&spid3r::Value::Null)).unwrap_or("");
            Ok(conv::from_string(s.to_uppercase()))
        })
        .register("add", |args| {
            let a = conv::as_i64(args.first().unwrap_or(&spid3r::Value::Null)).unwrap_or(0);
            let b = conv::as_i64(args.get(1).unwrap_or(&spid3r::Value::Null)).unwrap_or(0);
            Ok(conv::from_i64(a + b))
        })
        .register("host_time_ms", |_args| {
            use std::time::{SystemTime, UNIX_EPOCH};
            let ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            Ok(conv::from_i64(ms))
        })
        .set_global("app_name", conv::from_string("embed-demo"))
        .set_global("version_major", conv::from_i64(1));

    // 2. Run a script. `main`'s return value is the exit code; stdout is captured.
    let script = r#"
fn main() {
    println(shout("hello from spid3r"));
    println("2 + 40 = " + string(add(2, 40)));
    println("host time: " + string(host_time_ms()));
    println(app_name + " v" + string(version_major));
    return 0;
}
"#;

    match engine.run("demo", script) {
        Ok(out) => {
            print!("{}", out.output);
            std::process::exit(if out.exit_code == 0 { 0 } else { out.exit_code });
        }
        Err(e) => {
            eprintln!("script failed: {}", e);
            if !e.output.is_empty() {
                eprintln!("--- output before failure ---");
                eprint!("{}", e.output);
            }
            std::process::exit(1);
        }
    }
}
