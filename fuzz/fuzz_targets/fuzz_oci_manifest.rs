#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = spid3r_lang::oci::manifest::parse_manifest_or_index(data);
    let _ = spid3r_lang::oci::manifest::parse_image_config(data);
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = spid3r_lang::oci::layers::sanitize_tar_rel_path(s);
    }
});
