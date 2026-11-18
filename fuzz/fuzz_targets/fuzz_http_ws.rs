#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    spid3r_lang::stdlib::net::fuzz_parse_http_and_chunked(data);
});
