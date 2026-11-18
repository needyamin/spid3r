//! R12.2: Smoke/property suite exercising all pure I/O, serialization, tar, and
//! OCI manifest fuzz entry points on adversarial, truncated, and malformed byte
//! inputs (verifies zero panics and bounded failure).

use spid3r_lang::oci::layers::sanitize_tar_rel_path;
use spid3r_lang::oci::manifest::{parse_image_config, parse_manifest_or_index};
use spid3r_lang::pkg::read_tar;
use spid3r_lang::stdlib::net::fuzz_parse_http_and_chunked;
use spid3r_lang::stdlib::serialize::fuzz_decode_all;

#[test]
fn r12_2_pure_fuzz_surfaces_never_panic_on_malformed_inputs() {
    let corpus: &[&[u8]] = &[
        b"",
        b"\x00",
        b"\xff\xff\xff\xff\xff\xff\xff\xff",
        b"5\r\nhello\r\n0\r\n\r\n",
        b"ffffffffffffffff\r\nbad",
        b"\x81\x85\x11\x22\x33\x44\x55\x66\x77\x88\x99",
        b"\x81\xfe\xff\xff",
        b"\x81\xff\x7f\xff\xff\xff\xff\xff\xff\xff",
        b"\x9f\xff\xdf\xff\xc1",
        b"{invalid json",
        b"ustar\x00../etc/passwd",
    ];
    for &sample in corpus {
        fuzz_parse_http_and_chunked(sample);
        fuzz_decode_all(sample);
        let _ = read_tar(sample);
        let _ = parse_manifest_or_index(sample);
        let _ = parse_image_config(sample);
        if let Ok(s) = std::str::from_utf8(sample) {
            let _ = sanitize_tar_rel_path(s);
        }
    }

    // Deterministic pseudo-random byte sweep (256 cases, 0..128 bytes each)
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    for i in 0..256usize {
        let len = i % 128;
        let mut buf = Vec::with_capacity(len);
        for _ in 0..len {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            buf.push((seed >> 56) as u8);
        }
        fuzz_parse_http_and_chunked(&buf);
        fuzz_decode_all(&buf);
        let _ = read_tar(&buf);
        let _ = parse_manifest_or_index(&buf);
        let _ = parse_image_config(&buf);
    }
}
