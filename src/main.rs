fn main() {
    let code = spid3r_lang::cli::run(std::env::args().collect());
    std::process::exit(code);
}
