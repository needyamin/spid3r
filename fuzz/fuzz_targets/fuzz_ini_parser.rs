#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Arbitrary bytes through the INI parser. Parsing never fails hard (it
    // returns problems instead), so the target just requires "no panic".
    let text = String::from_utf8_lossy(data);
    let _ = spid3r_lang::config::parse_ini(&text);
});
