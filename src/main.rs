fn main() {
    // `env::args()` panics on an argument that is not valid UTF-8 (on Unix a
    // filename may be arbitrary bytes). `args_os` lets us replace the invalid
    // ones lossily and still run, instead of aborting before any code runs.
    let args: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let code = spid3r_lang::cli::run(args);
    std::process::exit(code);
}
