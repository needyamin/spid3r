fn main() -> std::process::ExitCode {
    // `env::args()` panics on an argument that is not valid UTF-8 (on Unix a
    // filename may be arbitrary bytes). `args_os` lets us replace the invalid
    // ones lossily and still run, instead of aborting before any code runs.
    let args: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let code = spid3r_lang::cli::run(args);
    // Return ExitCode instead of calling std::process::exit: on Windows with
    // +crt-static, process::exit invokes ExitProcess directly without running
    // CRT atexit hooks, which leaves LLVM PGO .profraw dumps at 0 bytes.
    if code == 0 {
        std::process::ExitCode::SUCCESS
    } else if (1..=255).contains(&code) {
        std::process::ExitCode::from(code as u8)
    } else {
        std::process::ExitCode::from(1)
    }
}
