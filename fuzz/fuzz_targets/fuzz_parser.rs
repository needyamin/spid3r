#![no_main]

use std::sync::Arc;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let src = String::from_utf8_lossy(data);
    if let Ok(prog) = spid3r_lang::parser::parse_file(&src, Arc::from("<fuzz>")) {
        let _ = spid3r_lang::semantic::check_file(&prog, false, "<fuzz>");
    }
});
