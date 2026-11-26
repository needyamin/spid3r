//! Native AOT compiler CLI binary (`yamc`) for the Spid3r language toolchain.
//!
//! Invokes `spid3r_lang::cli::run_tool(CliTool::Yamc, args)` directly in-process
//! with zero wrapper indirection or runtime overhead.
fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let code = spid3r_lang::cli::run_tool(spid3r_lang::cli::CliTool::Yamc, args);
    if code == 0 {
        std::process::ExitCode::SUCCESS
    } else if (1..=255).contains(&code) {
        std::process::ExitCode::from(code as u8)
    } else {
        std::process::ExitCode::from(1)
    }
}
