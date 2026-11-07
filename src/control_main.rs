//! `spid3r-control` — the Spid3r Control Panel desktop application.
//!
//! A separate binary (bundled beside spid3r.exe by the NSIS installer) that
//! opens the webview control panel over the same control ops the
//! `spid3r control` CLI uses. Built only when the `gui` feature is enabled:
//! core-only builds (the Linux/macOS release tarballs) ship the CLI surface.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    let code = match spid3r_lang::control::panel::run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error[control]: {}", e);
            1
        }
    };
    std::process::exit(code);
}
