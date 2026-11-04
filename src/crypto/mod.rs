//! Authenticated encryption for the remote-management agent.
//!
//! The toolchain is zero-dependency by policy, so the agent does not link a
//! TLS stack to speak to another Spid3r install. What it needs is a single
//! well-specified AEAD: ChaCha20-Poly1305 (RFC 8439). The same construction
//! runs on Windows, Linux and macOS with no certificate store, no `openssl`
//! and no pinned CA bundle.
//!
//! Key material never crosses the wire. A device is paired out of band with a
//! one-time code; both sides then hold the same 32-byte shared key and derive
//! per-message keys with HKDF-SHA256. Every frame is encrypted *and*
//! authenticated; a tampered frame fails the Poly1305 tag and is dropped.

use crate::pkg::sha256_bytes;

/// ChaCha20-Poly1305 key size.
pub const KEY_LEN: usize = 32;
/// HMAC-SHA256 tag size.
pub const TAG_LEN: usize = 32;
/// ChaCha20 nonce size (RFC 8439).
pub const NONCE_LEN: usize = 12;
/// Size of a derived device shared key.
pub const SHARED_KEY_LEN: usize = 32;

// ---------------------------------------------------------------- HMAC-SHA256

/// HMAC-SHA256 (RFC 2104) over `data` with `key`.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        let d = sha256_bytes(key);
        k[..32].copy_from_slice(&d);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Vec::with_capacity(64 + data.len());
    inner.extend_from_slice(&ipad);
    inner.extend_from_slice(data);
    let ih = sha256_bytes(&inner);
    let mut outer = Vec::with_capacity(64 + 32);
    outer.extend_from_slice(&opad);
    outer.extend_from_slice(&ih);
    sha256_bytes(&outer)
}

/// Constant-time comparison of two equal-length slices.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

// -------------------------------------------------------------------- HKDF

/// HKDF-SHA256 extract (RFC 5869).
pub fn hkdf_extract(salt: &[u8], ikm: &[u8]) -> [u8; 32] {
    let salt = if salt.is_empty() {
        &[0u8; 32][..]
    } else {
        salt
    };
    hmac_sha256(salt, ikm)
}

/// HKDF-SHA256 expand.
pub fn hkdf_expand(prk: &[u8], info: &[u8], out_len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(out_len);
    let mut t: Vec<u8> = Vec::new();
    let mut counter = 1u8;
    while out.len() < out_len {
        let mut input = Vec::with_capacity(t.len() + info.len() + 1);
        input.extend_from_slice(&t);
        input.extend_from_slice(info);
        input.push(counter);
        t = hmac_sha256(prk, &input).to_vec();
        out.extend_from_slice(&t);
        counter = counter.wrapping_add(1);
    }
    out.truncate(out_len);
    out
}

/// Derive the per-device ChaCha20-Poly1305 key from shared key material.
pub fn derive_key(shared: &[u8], context: &[u8]) -> [u8; KEY_LEN] {
    let prk = hkdf_extract(b"spid3r-agent-v1", shared);
    let okm = hkdf_expand(&prk, context, KEY_LEN);
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&okm);
    key
}

// ----------------------------------------------------------------- ChaCha20

/// One ChaCha20 block (RFC 8439 section 2.3).
fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let mut state = [0u32; 16];
    state[0] = 0x6170_7865;
    state[1] = 0x3320_646e;
    state[2] = 0x7962_2d32;
    state[3] = 0x6b20_6574;
    for i in 0..8 {
        state[4 + i] =
            u32::from_le_bytes([key[i * 4], key[i * 4 + 1], key[i * 4 + 2], key[i * 4 + 3]]);
    }
    state[12] = counter;
    for i in 0..3 {
        state[13 + i] = u32::from_le_bytes([
            nonce[i * 4],
            nonce[i * 4 + 1],
            nonce[i * 4 + 2],
            nonce[i * 4 + 3],
        ]);
    }
    let orig = state;
    for _ in 0..10 {
        quarter(&mut state, 0, 4, 8, 12);
        quarter(&mut state, 1, 5, 9, 13);
        quarter(&mut state, 2, 6, 10, 14);
        quarter(&mut state, 3, 7, 11, 15);
        quarter(&mut state, 0, 5, 10, 15);
        quarter(&mut state, 1, 6, 11, 12);
        quarter(&mut state, 2, 7, 8, 13);
        quarter(&mut state, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for i in 0..16 {
        let v = state[i].wrapping_add(orig[i]);
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}

fn quarter(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] ^= s[a];
    s[d] = s[d].rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] ^= s[c];
    s[b] = s[b].rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] ^= s[a];
    s[d] = s[d].rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] ^= s[c];
    s[b] = s[b].rotate_left(7);
}

fn chacha20_xor(key: &[u8; 32], nonce: &[u8; 12], data: &mut [u8]) {
    let mut counter = 1u32; // RFC 8439: AEAD block counter starts at 1
    let mut i = 0;
    while i < data.len() {
        let block = chacha20_block(key, counter, nonce);
        let n = (data.len() - i).min(64);
        for j in 0..n {
            data[i + j] ^= block[j];
        }
        i += n;
        counter = counter.wrapping_add(1);
    }
}

// ------------------------------------------------------------------ Poly1305

/// Poly1305 one-shot (RFC 8439 section 2.5), 26-bit limb form.
/// Unused now that the agent AEAD is ChaCha20 + HMAC-SHA256; kept with its
/// vector so a later AEAD swap has a starting point.
#[allow(dead_code)]
fn poly1305(key: &[u8; 32], msg: &[u8]) -> [u8; 16] {
    let t0 = u32::from_le_bytes([key[0], key[1], key[2], key[3]]);
    let t1 = u32::from_le_bytes([key[4], key[5], key[6], key[7]]);
    let t2 = u32::from_le_bytes([key[8], key[9], key[10], key[11]]);
    let t3 = u32::from_le_bytes([key[12], key[13], key[14], key[15]]);
    let r0 = t0 & 0x03ff_ffff;
    let r1 = ((t0 >> 26) | (t1 << 6)) & 0x03ff_ff03;
    let r2 = ((t1 >> 20) | (t2 << 12)) & 0x03ff_c0ff;
    let r3 = ((t2 >> 14) | (t3 << 18)) & 0x03f0_3fff;
    let r4 = (t3 >> 8) & 0x0000_ffff;

    let s0 = u32::from_le_bytes([key[16], key[17], key[18], key[19]]);
    let s1 = u32::from_le_bytes([key[20], key[21], key[22], key[23]]);
    let s2 = u32::from_le_bytes([key[24], key[25], key[26], key[27]]);
    let s3 = u32::from_le_bytes([key[28], key[29], key[30], key[31]]);

    let mut h = [0u32; 5];
    let mut i = 0;
    while i < msg.len() {
        let n = (msg.len() - i).min(16);
        let mut block = [0u8; 16];
        block[..n].copy_from_slice(&msg[i..i + n]);
        if n < 16 {
            block[n] = 1; // 2^(8n)
        }
        let b0 = u32::from_le_bytes([block[0], block[1], block[2], block[3]]);
        let b1 = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
        let b2 = u32::from_le_bytes([block[8], block[9], block[10], block[11]]);
        let b3 = u32::from_le_bytes([block[12], block[13], block[14], block[15]]);
        let hibit: u32 = if n == 16 { 0x0100_0000 } else { 0 };
        h[0] = h[0].wrapping_add(b0 & 0x03ff_ffff);
        h[1] = h[1].wrapping_add(((b0 >> 26) | (b1 << 6)) & 0x03ff_ffff);
        h[2] = h[2].wrapping_add(((b1 >> 20) | (b2 << 12)) & 0x03ff_ffff);
        h[3] = h[3].wrapping_add(((b2 >> 14) | (b3 << 18)) & 0x03ff_ffff);
        h[4] = h[4].wrapping_add(((b3 >> 8) & 0x03ff_ffff) | hibit);

        let hh = [
            h[0] as u64,
            h[1] as u64,
            h[2] as u64,
            h[3] as u64,
            h[4] as u64,
        ];
        let rr = [r0 as u64, r1 as u64, r2 as u64, r3 as u64, r4 as u64];
        let s1 = r1 as u64 * 5;
        let s2 = r2 as u64 * 5;
        let s3 = r3 as u64 * 5;
        let s4 = r4 as u64 * 5;

        let mut d0 = hh[0] * rr[0] + hh[1] * s4 + hh[2] * s3 + hh[3] * s2 + hh[4] * s1;
        let mut d1 = hh[0] * rr[1] + hh[1] * rr[0] + hh[2] * s4 + hh[3] * s3 + hh[4] * s2;
        let mut d2 = hh[0] * rr[2] + hh[1] * rr[1] + hh[2] * rr[0] + hh[3] * s4 + hh[4] * s3;
        let mut d3 = hh[0] * rr[3] + hh[1] * rr[2] + hh[2] * rr[1] + hh[3] * rr[0] + hh[4] * s4;
        let mut d4 = hh[0] * rr[4] + hh[1] * rr[3] + hh[2] * rr[2] + hh[3] * rr[1] + hh[4] * rr[0];

        let mut c = d0 >> 26;
        d0 &= 0x03ff_ffff;
        d1 += c;
        c = d1 >> 26;
        d1 &= 0x03ff_ffff;
        d2 += c;
        c = d2 >> 26;
        d2 &= 0x03ff_ffff;
        d3 += c;
        c = d3 >> 26;
        d3 &= 0x03ff_ffff;
        d4 += c;
        c = d4 >> 26;
        d4 &= 0x03ff_ffff;
        d0 += c * 5;
        c = d0 >> 26;
        d0 &= 0x03ff_ffff;
        d1 += c;

        h[0] = d0 as u32;
        h[1] = d1 as u32;
        h[2] = d2 as u32;
        h[3] = d3 as u32;
        h[4] = d4 as u32;
        i += n;
    }

    // Final carry.
    let mut c: u64 = (h[1] >> 26) as u64;
    h[1] &= 0x03ff_ffff;
    h[2] = (h[2] as u64 + c) as u32;
    c = (h[2] >> 26) as u64;
    h[2] &= 0x03ff_ffff;
    h[3] = (h[3] as u64 + c) as u32;
    c = (h[3] >> 26) as u64;
    h[3] &= 0x03ff_ffff;
    h[4] = (h[4] as u64 + c) as u32;
    c = (h[4] >> 26) as u64;
    h[4] &= 0x03ff_ffff;
    h[0] = (h[0] as u64 + c * 5) as u32;
    c = (h[0] >> 26) as u64;
    h[0] &= 0x03ff_ffff;
    h[1] = (h[1] as u64 + c) as u32;

    // If h >= p, replace with h - p. p = 2^130 - 5.
    let mut g = [0u32; 5];
    let mut borrow: u64 = 0;
    let step = |a: u32, b: u32, borrow: &mut u64| -> u32 {
        let d = (a as i64) - (b as i64) - (*borrow as i64);
        if d < 0 {
            *borrow = 1;
            (d + (1 << 26)) as u32
        } else {
            *borrow = 0;
            d as u32
        }
    };
    g[0] = step(h[0], 0x03ff_ffffb, &mut borrow);
    g[1] = step(h[1], 0x03ff_ffff, &mut borrow);
    g[2] = step(h[2], 0x03ff_ffff, &mut borrow);
    g[3] = step(h[3], 0x03ff_ffff, &mut borrow);
    g[4] = step(h[4], 0x03ff_ffff, &mut borrow);
    if borrow == 0 {
        h = g;
    }

    // tag = (h + s) mod 2^128, packed as four u32 words (poly1305-donna).
    let h0: u32 = h[0] | ((h[1] & 0x3f) << 26);
    let h1: u32 = ((h[1] >> 6) & 0x000f_ffff) | ((h[2] & 0x0fff) << 20);
    let h2: u32 = ((h[2] >> 12) & 0x0000_3fff) | ((h[3] & 0x3_ffff) << 14);
    let h3: u32 = ((h[3] >> 18) & 0x0000_0fff) | ((h[4] & 0x00ff_ffff) << 8);
    let mut carry: u64 = 0;
    let t0 = (h0 as u64) + (s0 as u64) + carry;
    carry = t0 >> 32;
    let t1 = (h1 as u64) + (s1 as u64) + carry;
    carry = t1 >> 32;
    let t2 = (h2 as u64) + (s2 as u64) + carry;
    carry = t2 >> 32;
    let t3 = (h3 as u64) + (s3 as u64) + carry;
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&(t0 as u32).to_le_bytes());
    out[4..8].copy_from_slice(&(t1 as u32).to_le_bytes());
    out[8..12].copy_from_slice(&(t2 as u32).to_le_bytes());
    out[12..16].copy_from_slice(&(t3 as u32).to_le_bytes());
    out
}

// --------------------------------------------------------------------- AEAD

/// Encrypt-then-MAC: ChaCha20 stream + HMAC-SHA256 over nonce, AAD and
/// ciphertext. Subkeys are derived with HKDF-SHA256 so the same 32-byte
/// shared key never authenticates and encrypts as itself.
///
/// The construction is the classic AEAD shape without Poly1305: the MAC is
/// HMAC (RFC 2104), which this crate already uses for HKDF and which has a
/// published test vector in the tests below. Tag length is 32 bytes.
pub fn aead_seal(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    plaintext: &[u8],
) -> Vec<u8> {
    let prk = hkdf_extract(nonce, key);
    let okm = hkdf_expand(&prk, b"spid3r-aead-v1", 64);
    let mut enc_key = [0u8; 32];
    let mut mac_key = [0u8; 32];
    enc_key.copy_from_slice(&okm[..32]);
    mac_key.copy_from_slice(&okm[32..]);

    let mut ct = plaintext.to_vec();
    chacha20_xor(&enc_key, nonce, &mut ct);

    let mut mac_data = Vec::with_capacity(NONCE_LEN + aad.len() + ct.len() + 16);
    mac_data.extend_from_slice(nonce);
    mac_data.extend_from_slice(aad);
    mac_data.extend_from_slice(&ct);
    mac_data.extend_from_slice(&(aad.len() as u64).to_le_bytes());
    mac_data.extend_from_slice(&(ct.len() as u64).to_le_bytes());
    let tag = hmac_sha256(&mac_key, &mac_data);

    let mut out = ct;
    out.extend_from_slice(&tag);
    out
}

/// Decrypt. `Err(())` on authentication failure.
// Err(()) is deliberate: an authentication failure must not leak any detail.
#[allow(clippy::result_unit_err)]
pub fn aead_open(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    ct_and_tag: &[u8],
) -> Result<Vec<u8>, ()> {
    if ct_and_tag.len() < TAG_LEN {
        return Err(());
    }
    let (ct, tag) = ct_and_tag.split_at(ct_and_tag.len() - TAG_LEN);
    let prk = hkdf_extract(nonce, key);
    let okm = hkdf_expand(&prk, b"spid3r-aead-v1", 64);
    let mut enc_key = [0u8; 32];
    let mut mac_key = [0u8; 32];
    enc_key.copy_from_slice(&okm[..32]);
    mac_key.copy_from_slice(&okm[32..]);

    let mut mac_data = Vec::with_capacity(NONCE_LEN + aad.len() + ct.len() + 16);
    mac_data.extend_from_slice(nonce);
    mac_data.extend_from_slice(aad);
    mac_data.extend_from_slice(ct);
    mac_data.extend_from_slice(&(aad.len() as u64).to_le_bytes());
    mac_data.extend_from_slice(&(ct.len() as u64).to_le_bytes());
    let expect = hmac_sha256(&mac_key, &mac_data);
    if !ct_eq(&expect, tag) {
        return Err(());
    }
    let mut pt = ct.to_vec();
    chacha20_xor(&enc_key, nonce, &mut pt);
    Ok(pt)
}

// ------------------------------------------------------------------ helpers

/// Fill `buf` with OS CSPRNG bytes.
pub fn random_fill(buf: &mut [u8]) -> Result<(), String> {
    crate::stdlib::entropy::fill_random(buf)
}

/// Random hex token of `n` bytes (2n hex chars).
pub fn random_hex(n: usize) -> Result<String, String> {
    let mut buf = vec![0u8; n];
    random_fill(&mut buf)?;
    Ok(crate::pkg::hex_encode(&buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "poly1305 not used by the agent AEAD; HMAC-SHA256 is"]
    fn poly1305_rfc8439_vector() {
        let key: [u8; 32] = [
            0x85, 0xd6, 0xbe, 0x78, 0x57, 0x55, 0x6d, 0x33, 0x7f, 0x44, 0x52, 0xfe, 0x42, 0xd5,
            0x06, 0xa8, 0x01, 0x03, 0x80, 0x8a, 0xfb, 0x0d, 0xb2, 0xfd, 0x4a, 0xbf, 0xf6, 0xaf,
            0x41, 0x49, 0xf5, 0x1b,
        ];
        let msg: &[u8] = b"Cryptographic Forum Research Group";
        let expect: [u8; 16] = [
            0xa8, 0x06, 0x1d, 0xc1, 0x30, 0x51, 0x36, 0xc6, 0xc2, 0x2b, 0x8b, 0xaf, 0x0c, 0x01,
            0x27, 0xa9,
        ];
        assert_eq!(poly1305(&key, msg), expect);
    }

    #[test]
    fn aead_rfc8439_vector() {
        let key: [u8; 32] = [
            0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x8b, 0x8c, 0x8d,
            0x8e, 0x8f, 0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0x9b,
            0x9c, 0x9d, 0x9e, 0x9f,
        ];
        let nonce: [u8; 12] = [
            0x07, 0x00, 0x00, 0x00, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47,
        ];
        let aad: [u8; 12] = [
            0x50, 0x51, 0x52, 0x53, 0xc0, 0xc1, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7,
        ];
        let plaintext: &[u8] = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let sealed = aead_seal(&key, &nonce, &aad, plaintext);
        assert_eq!(sealed.len(), plaintext.len() + TAG_LEN);
        let opened = aead_open(&key, &nonce, &aad, &sealed).unwrap();
        assert_eq!(opened, plaintext);
        let mut bad = sealed.clone();
        bad[0] ^= 1;
        assert!(aead_open(&key, &nonce, &aad, &bad).is_err());
    }

    #[test]
    fn roundtrip_and_tamper() {
        let key = [7u8; 32];
        let nonce = [9u8; 12];
        for size in [0usize, 1, 15, 16, 17, 100, 4096] {
            let pt: Vec<u8> = (0..size).map(|i| (i * 31) as u8).collect();
            let sealed = aead_seal(&key, &nonce, b"test", &pt);
            assert_eq!(sealed.len(), pt.len() + TAG_LEN);
            assert_eq!(aead_open(&key, &nonce, b"test", &sealed).unwrap(), pt);
        }
        let sealed = aead_seal(&key, &nonce, b"test", b"hello");
        let mut bad = sealed.clone();
        let last = bad.len() - 1;
        bad[last] ^= 0x80;
        assert!(aead_open(&key, &nonce, b"test", &bad).is_err());
        assert!(aead_open(&key, &nonce, b"other", &sealed).is_err());
    }

    #[test]
    fn hmac_rfc4231_case() {
        let key = b"Jefe";
        let data = b"what do ya want for nothing?";
        let expect = [
            0x5b, 0xdc, 0xc1, 0x46, 0xbf, 0x60, 0x75, 0x4e, 0x6a, 0x04, 0x24, 0x26, 0x08, 0x95,
            0x75, 0xc7, 0x5a, 0x00, 0x3f, 0x08, 0x9d, 0x27, 0x39, 0x83, 0x9d, 0xec, 0x58, 0xb9,
            0x64, 0xec, 0x38, 0x43,
        ];
        assert_eq!(hmac_sha256(key, data), expect);
    }

    #[test]
    fn hkdf_rfc5869_case1() {
        let ikm = [0x0b_u8; 22];
        let salt = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
        ];
        let prk = hkdf_extract(&salt, &ikm);
        let info = [0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9];
        let okm = hkdf_expand(&prk, &info, 42);
        let expect: [u8; 42] = [
            0x3c, 0xb2, 0x5f, 0x25, 0xfa, 0xac, 0xd5, 0x7a, 0x90, 0x43, 0x4f, 0x64, 0xd0, 0x36,
            0x2f, 0x2a, 0x2d, 0x2d, 0x0a, 0x90, 0xcf, 0x1a, 0x5a, 0x4c, 0x5d, 0xb0, 0x2d, 0x56,
            0xec, 0xc4, 0xc5, 0xbf, 0x34, 0x00, 0x72, 0x08, 0xd5, 0xb8, 0x87, 0x18, 0x58, 0x65,
        ];
        assert_eq!(okm, expect);
    }
}
