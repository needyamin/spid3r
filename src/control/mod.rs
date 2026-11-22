//! `spid3r control` — the local runtime control surface.
//!
//! One subcommand that ties local runtime administration together:
//!
//! - `spid3r control [status]`  dashboard: version, resolved `spid3r.ini`,
//!   state dir, and INI validation
//! - `spid3r control safety`  capability switches (`security.deny_*`)
//! - `spid3r control config`  read/write/reset/reload/validate `spid3r.ini`
//! - `spid3r control ini [--init] [--force]`  show or write the default `spid3r.ini`
//! - `spid3r control logs [N]`  tail the diagnostic log
//!
//! The CLI and the `spid3r-control` desktop panel (see [`ops`]) call the same
//! operation layer, so the two surfaces always agree on every control action.

pub mod lang;
pub mod ops;

// The desktop panel shares this module so it can reach `ops` via `super`.
// Compiled only when wry/tao are available (the `gui` feature).
#[cfg(feature = "gui")]
pub mod panel;

/// `spid3r control [status|safety|ini|config|logs]`
pub fn cmd_control(args: &[String]) -> i32 {
    // Raw op passthrough: `spid3r control --op NAME --params '{...}'`. Same
    // operations the panel's IPC surface uses — this is what the UI test
    // harness drives, so the browser page and the CLI are provably exercising
    // identical code. Not documented in --help: it is a test/dev hook.
    if args.first().map(String::as_str) == Some("--op") {
        let op = args.get(1).cloned().unwrap_or_default();
        // `--params` is a flag; the JSON is the argument AFTER it. Position
        // lookup (not a fixed index) so flag order does not matter — getting
        // this wrong silently fed "--params" itself as the params object.
        let params = args
            .iter()
            .position(|a| a == "--params")
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_else(|| "{}".to_string());
        return match ops::dispatch(&op, &params) {
            Ok(json) => {
                println!("{}", json);
                0
            }
            Err(e) => {
                eprintln!("error[control]: {}", e);
                1
            }
        };
    }
    let sub = args.first().map(String::as_str).unwrap_or("status");
    match sub {
        "status" | "dashboard" => print_status(),
        "safety" => print_safety(),
        "ini" => cmd_ini(&args[1.min(args.len())..]),
        "config" => cmd_config(&args[1.min(args.len())..]),
        "logs" => cmd_logs(&args[1.min(args.len())..]),
        "--help" | "help" | "-h" => {
            print!(
                "USAGE:\n    \
                 spid3r control [status]        Machine-wide runtime dashboard\n    \
                 spid3r control safety          Capability switches (deny_*)\n    \
                 spid3r control config set KEY VALUE | get KEY | reset KEY | reset-all [SEC] | reload | validate | list\n    \
                 spid3r control ini [--init] [--force]\n    \
                 spid3r control logs [N]        Tail the diagnostic log (default 50)\n"
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

fn print_safety() -> i32 {
    match ops::dispatch("safety_overview", "{}") {
        Ok(json) => {
            let open = ops::field(&json, "open").unwrap_or_default();
            let total = ops::field(&json, "total").unwrap_or_default();
            println!(
                "capabilities open: {} of {} (restricted is the safe state)\n",
                open, total
            );
            if let Some(items) = ops::field(&json, "items") {
                for row in items.lines() {
                    let parts: Vec<&str> = row.splitn(5, '|').collect();
                    if parts.len() != 5 {
                        continue;
                    }
                    let (key, state, label, consequence, value) =
                        (parts[0], parts[1], parts[2], parts[3], parts[4]);
                    println!(
                        "[{}] {} ({})\n    {}: {} = {}\n",
                        if state == "open" { "OPEN " } else { "SAFE " },
                        label,
                        consequence,
                        key,
                        value,
                        if state == "open" {
                            "open"
                        } else {
                            "restricted"
                        },
                    );
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

/// `spid3r control config set|get|reset|validate|list` — the panel's config
/// editing surface on the CLI, including the ini bootstrap (`set`/`reset`
/// create a per-user spid3r.ini when none exists, instead of silently
/// targeting the install directory).
fn cmd_config(args: &[String]) -> i32 {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    let rest = &args[1.min(args.len())..];
    let result = match sub {
        "set" => match (rest.first(), rest.get(1)) {
            (Some(key), Some(value)) => ops::dispatch(
                "config_set",
                &format!(
                    "{{\"key\":{},\"value\":{}}}",
                    ops::jstr(key),
                    ops::jstr(value)
                ),
            ),
            _ => return usage_err("control config set needs KEY and VALUE"),
        },
        "get" => match rest.first() {
            Some(key) => ops::dispatch("config_get", &format!("{{\"key\":{}}}", ops::jstr(key))),
            None => return usage_err("control config get needs KEY"),
        },
        "reset" => match rest.first() {
            Some(key) => ops::dispatch("config_reset", &format!("{{\"key\":{}}}", ops::jstr(key))),
            None => return usage_err("control config reset needs KEY"),
        },
        "reset-all" => {
            let sec = rest.first().map(String::as_str).unwrap_or("");
            ops::dispatch(
                "config_reset_all",
                &format!("{{\"section\":{}}}", ops::jstr(sec)),
            )
        }
        "reload" => ops::dispatch("config_reload", "{}"),
        "validate" => ops::dispatch("config_validate", "{}"),
        "list" => ops::dispatch("config_list", "{}"),
        "--help" | "help" | "-h" => {
            print!(
                "USAGE:\n    \
                 spid3r control config set KEY VALUE\n    \
                 spid3r control config get KEY\n    \
                 spid3r control config reset KEY        Restore the schema default\n    \
                 spid3r control config reset-all [SEC]  Restore all defaults (or one section)\n    \
                 spid3r control config reload           Reload spid3r.ini live\n    \
                 spid3r control config validate\n    \
                 spid3r control config list\n"
            );
            return 0;
        }
        other => return usage_err(&format!("unknown config subcommand '{}'", other)),
    };
    match result {
        Ok(json) => match sub {
            "get" => {
                println!(
                    "{} = {}",
                    ops::field(&json, "key").unwrap_or_default(),
                    ops::field(&json, "value").unwrap_or_default()
                );
                0
            }
            "list" => {
                if let Some(keys) = ops::field(&json, "keys") {
                    for row in keys.lines() {
                        let parts: Vec<&str> = row.splitn(8, '|').collect();
                        if parts.len() >= 6 {
                            println!("{:<28} {:<6} {}", parts[0], parts[1], parts[2]);
                        }
                    }
                }
                0
            }
            "validate" => {
                let errors = ops::field(&json, "errors").unwrap_or_default();
                if ops::field(&json, "valid").as_deref() == Some("false") {
                    eprintln!("{}", errors);
                    1
                } else {
                    println!("configuration valid");
                    0
                }
            }
            _ => {
                println!("{}", ops::field(&json, "message").unwrap_or_default());
                0
            }
        },
        Err(e) => {
            eprintln!("error[control]: {}", e);
            1
        }
    }
}

fn usage_err(msg: &str) -> i32 {
    eprintln!("error[control]: {}", msg);
    2
}

fn cmd_logs(args: &[String]) -> i32 {
    let n: usize = args.first().and_then(|a| a.parse().ok()).unwrap_or(50);
    let params = format!("{{\"n\":\"{}\"}}", n);
    match ops::dispatch("logs_tail", &params) {
        Ok(json) => {
            if let Some(p) = ops::field(&json, "path") {
                if !p.is_empty() {
                    println!("# {}", p);
                }
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
