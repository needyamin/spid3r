#![no_main]

use std::sync::Arc;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(src) = std::str::from_utf8(data) {
        let _ = spid3r_lang::lexer::Lexer::new(src, Arc::from("<fuzz>")).tokenize();
    } else {
        let lossy = String::from_utf8_lossy(data);
        let _ = spid3r_lang::lexer::Lexer::new(&lossy, Arc::from("<fuzz>")).tokenize();
    }
});
