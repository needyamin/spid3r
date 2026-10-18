#![no_main]

use libfuzzer_sys::fuzz_target;
use spid3r::vm::bytecode::{deserialize_vmfunc, validate_chunk};

fuzz_target!(|data: &[u8]| {
    if let Ok(func) = deserialize_vmfunc(data) {
        let _ = validate_chunk(&func.chunk);
    }
});
