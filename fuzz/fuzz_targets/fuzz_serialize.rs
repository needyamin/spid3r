#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    spid3r_lang::stdlib::serialize::fuzz_decode_all(data);
    let _ = spid3r_lang::pkg::read_tar(data);
});
