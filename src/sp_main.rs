//! Short CLI alias binary (`sp`) for the Spid3r language toolchain.
//!
//! Invokes the exact same `spid3r_lang::cli::run` entrypoint as `spid3r` with
//! zero wrapper indirection or runtime overhead.
fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let code = spid3r_lang::cli::run(args);
    if code == 0 {
        std::process::ExitCode::SUCCESS
    } else if (1..=255).contains(&code) {
        std::process::ExitCode::from(code as u8)
    } else {
        std::process::ExitCode::from(1)
    }
}
