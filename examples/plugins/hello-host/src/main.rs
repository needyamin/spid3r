//! A host binary that loads the `hello` plugin and runs a `.matrix` script.
//!
//! ```console
//! $ cargo run -- demo.matrix
//! [hello] started
//! Hello, Ada!
//! demo-var
//! [hello] stopped
//! ```
//!
//! `--deny-environment` shows the permission gate: the same script now fails on
//! `hello_env` with a catchable `capability denied` error, while `hello_greet`
//! keeps working.

use std::process::ExitCode;

use spid3r_lang::embed::{Capability, Engine};
use spid3r_plugin_hello::HelloPlugin;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let deny = args.iter().any(|a| a == "--deny-environment");
    let script = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .cloned()
        .unwrap_or_else(|| "demo.matrix".to_string());

    // Registration happens once, before any engine exists: `native_defs()`
    // merges the plugin's functions into the same table all three engines read.
    if let Err(e) = spid3r_lang::plugin::register(Box::new(HelloPlugin::new())) {
        eprintln!("error: {e}");
        return ExitCode::from(2);
    }

    let source = match std::fs::read_to_string(&script) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {script}: {e}");
            return ExitCode::from(2);
        }
    };

    let mut engine = Engine::new();
    if deny {
        // A policy filter over names: the script cannot call `hello_env`, and
        // gets a catchable error if it tries. It does not constrain the plugin.
        engine = engine.deny(Capability::Environment);
    }

    let result = engine.run(&script, &source);
    // Stop the plugin before the process ends, so its teardown runs in a
    // defined order rather than being skipped at exit.
    let stop_errors = spid3r_lang::plugin::shutdown_all();
    for e in stop_errors {
        eprintln!("warning: {e}");
    }

    match result {
        Ok(out) => {
            print!("{}", out.output);
            ExitCode::from(out.exit_code.clamp(0, 255) as u8)
        }
        Err(e) => {
            eprintln!("{}", e.diagnostic.message);
            ExitCode::from(1)
        }
    }
}
