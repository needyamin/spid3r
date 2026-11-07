//! `spid3r control` — the runtime control surface.
//!
//! One subcommand that ties the management surfaces together:
//!
//! - `spid3r control [status]`  dashboard: version, resolved `spid3r.ini`,
//!   agent liveness, paired-device count, audit-log tail
//! - `spid3r control ini [--init] [--force]`  show (or write) the default
//!   `spid3r.ini`
//! - `spid3r control agent start|stop|status`  detached agent lifecycle
//! - `spid3r control devices [list] | revoke <id>`  paired-device management
//! - `spid3r control audit [N]`  tail the audit log
//!
//! The CLI and the `spid3r-control` desktop panel (see [`ops`]) call the same
//! operation layer, so the two surfaces can never disagree about what a
//! control action does. Everything here is local-machine scope: managing
//! OTHER hosts stays with `spid3r agent pair` / `spid3r admin` by design.

pub mod ops;

// The desktop panel shares this module so it can reach `ops` via `super`.
// Compiled only when wry/tao are available (the `gui` feature).
#[cfg(feature = "gui")]
pub mod panel;

/// `spid3r control [status|ini|agent|devices|audit]`
pub fn cmd_control(args: &[String]) -> i32 {
    let sub = args.first().map(String::as_str).unwrap_or("status");
    match sub {
        "status" | "dashboard" => print_status(),
        "ini" => cmd_ini(&args[1.min(args.len())..]),
        "agent" => cmd_agent(&args[1.min(args.len())..]),
        "devices" => cmd_devices(&args[1.min(args.len())..]),
        "audit" => cmd_audit(&args[1.min(args.len())..]),
        "--help" | "help" | "-h" => {
            print!(
                "USAGE:\n    \
                 spid3r control [status]        Machine-wide runtime dashboard\n    \
                 spid3r control ini [--init] [--force]\n    \
                 spid3r control agent start|stop|status\n    \
                 spid3r control devices [list] | revoke <id>\n    \
                 spid3r control audit [N]       Tail the audit log (default 20)\n"
            );
            0
        }
        other => {
            eprintln!(
                "error[control]: unknown subcommand '{}' (try: spid3r control help)",
                other
            );
            2
        }
    }
}

fn print_status() -> i32 {
    match ops::dispatch("status", "{}") {
        Ok(json) => {
            println!("{}", ops::render_status_text(&json));
            0
        }
        Err(e) => {
            eprintln!("error[control]: {}", e);
            1
        }
    }
}

fn cmd_ini(args: &[String]) -> i32 {
    let init = args.iter().any(|a| a == "--init");
    let force = args.iter().any(|a| a == "--force");
    if !init {
        match ops::dispatch("ini_info", "{}") {
            Ok(json) => {
                println!("{}", ops::render_ini_info_text(&json));
                0
            }
            Err(e) => {
                eprintln!("error[control]: {}", e);
                1
            }
        }
    } else {
        let params = format!("{{\"force\":{}}}", force);
        match ops::dispatch("ini_init", &params) {
            Ok(json) => {
                println!("{}", ops::field(&json, "message").unwrap_or_default());
                0
            }
            Err(e) => {
                eprintln!("error[control]: {}", e);
                1
            }
        }
    }
}

fn cmd_agent(args: &[String]) -> i32 {
    let sub = args.first().map(String::as_str).unwrap_or("status");
    let op = match sub {
        "start" => "agent_start",
        "stop" => "agent_stop",
        "status" => "agent_status",
        "--help" | "help" | "-h" => {
            print!(
                "USAGE:\n    \
                 spid3r control agent start   Spawn a detached background agent\n    \
                 spid3r control agent stop    Stop the agent this tool started\n    \
                 spid3r control agent status  Liveness (TCP probe) + PID file\n"
            );
            return 0;
        }
        other => {
            eprintln!("error[control]: unknown agent subcommand '{}'", other);
            return 2;
        }
    };
    match ops::dispatch(op, "{}") {
        Ok(json) => {
            println!("{}", ops::field(&json, "message").unwrap_or_default());
            0
        }
        Err(e) => {
            eprintln!("error[control]: {}", e);
            1
        }
    }
}

fn cmd_devices(args: &[String]) -> i32 {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    match sub {
        "list" | "" => match ops::dispatch("devices_list", "{}") {
            Ok(json) => {
                println!("{}", ops::render_devices_text(&json));
                0
            }
            Err(e) => {
                eprintln!("error[control]: {}", e);
                1
            }
        },
        "revoke" => {
            let Some(id) = args.get(1) else {
                eprintln!("error[control]: devices revoke needs a device id");
                return 2;
            };
            let params = format!("{{\"id\":{}}}", ops::jstr(id));
            match ops::dispatch("devices_revoke", &params) {
                Ok(json) => {
                    println!("{}", ops::field(&json, "message").unwrap_or_default());
                    0
                }
                Err(e) => {
                    eprintln!("error[control]: {}", e);
                    1
                }
            }
        }
        "--help" | "help" | "-h" => {
            print!(
                "USAGE:\n    \
                 spid3r control devices [list]     List paired devices\n    \
                 spid3r control devices revoke <id>\n"
            );
            0
        }
        other => {
            eprintln!("error[control]: unknown devices subcommand '{}'", other);
            2
        }
    }
}

fn cmd_audit(args: &[String]) -> i32 {
    let n: usize = args.first().and_then(|a| a.parse().ok()).unwrap_or(20);
    let params = format!("{{\"n\":{}}}", n);
    match ops::dispatch("audit_tail", &params) {
        Ok(json) => {
            if let Some(p) = ops::field(&json, "path") {
                println!("# {}", p);
            }
            if let Some(text) = ops::field(&json, "text") {
                for line in text.lines() {
                    println!("{}", line);
                }
            }
            0
        }
        Err(e) => {
            eprintln!("error[control]: {}", e);
            1
        }
    }
}
