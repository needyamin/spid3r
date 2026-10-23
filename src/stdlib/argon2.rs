//! Argon2 (RFC 9106) — the memory-hard password hash, all three variants
//! (`argon2d`, `argon2i`, `argon2id`).
//!
//! Written from the RFC: Blake2b for hashing, `H'` for variable-length
//! output, the `G` compression function over 1 KiB blocks with BlaMka
//! multiplication, and the data-independent address generator for the `i`
//! and `id` variants. Verified against OpenSSL 3.5's `ARGON2ID`/`ARGON2I`/
//! `ARGON2D` implementations.
//!
//! Not here: the encoded `$argon2id$v=19$m=...` string format (that is a
//! packaging concern) — `argon2_hex` returns the raw tag.

use crate::value::{NativeCtx, Value};

/// Refuse parameter sets that would need more than this (RFC 9106 allows up
/// to 2^32 KiB; that is a denial-of-service waiting to happen).
const MAX_MEMORY_KIB: u64 = 1024 * 1024; // 1 GiB

// -------------------------------------------------------------------- blake2b

const BLAKE2B_IV: [u64; 8] = [
    0x6a09_e667_f3bc_c908,
    0xbb67_ae85_84ca_a73b,
    0x3c6e_f372_fe94_f82b,
    0xa54f_f53a_5f1d_36f1,
    0x510e_527f_ade6_82d1,
    0x9b05_688c_2b3e_6c1f,
    0x1f83_d9ab_fb41_bd6b,
    0x5be0_cd19_137e_2179,
];

const SIGMA: [[usize; 16]; 12] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
];

#[inline]
fn blake_g(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize, x: u64, y: u64) {
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
    v[d] = (v[d] ^ v[a]).rotate_right(32);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(24);
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(63);
}

fn blake2b_compress(h: &mut [u64; 8], block: &[u8], t: u128, last: bool) {
    let mut m = [0u64; 16];
    for i in 0..16 {
        let mut a = [0u8; 8];
        a.copy_from_slice(&block[i * 8..i * 8 + 8]);
        m[i] = u64::from_le_bytes(a);
    }
    let mut v = [0u64; 16];
    v[..8].copy_from_slice(h);
    v[8..].copy_from_slice(&BLAKE2B_IV);
    v[12] ^= t as u64;
    v[13] ^= (t >> 64) as u64;
    if last {
        v[14] = !v[14];
    }
    for s in SIGMA.iter() {
        blake_g(&mut v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
        blake_g(&mut v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
        blake_g(&mut v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
        blake_g(&mut v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
        blake_g(&mut v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
        blake_g(&mut v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
        blake_g(&mut v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
        blake_g(&mut v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
    }
    for i in 0..8 {
        h[i] ^= v[i] ^ v[i + 8];
    }
}

/// Unkeyed Blake2b with a caller-chosen output length (1..=64).
pub fn blake2b(data: &[u8], out_len: usize) -> Vec<u8> {
    assert!((1..=64).contains(&out_len), "blake2b output length");
    let mut h = BLAKE2B_IV;
    h[0] ^= 0x0101_0000 ^ out_len as u64;
    let mut t: u128 = 0;
    let mut offset = 0usize;
    while data.len() - offset > 128 {
        t += 128;
        blake2b_compress(&mut h, &data[offset..offset + 128], t, false);
        offset += 128;
    }
    let mut block = [0u8; 128];
    let rest = &data[offset..];
    block[..rest.len()].copy_from_slice(rest);
    t += rest.len() as u128;
    blake2b_compress(&mut h, &block, t, true);
    let mut out = Vec::with_capacity(out_len);
    for word in h.iter() {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out.truncate(out_len);
    out
}

/// `H'(T, X)` from RFC 9106 §3.3: variable-length hash built on Blake2b.
fn blake2b_long(out_len: usize, input: &[u8]) -> Vec<u8> {
    let mut prefixed = Vec::with_capacity(input.len() + 4);
    prefixed.extend_from_slice(&(out_len as u32).to_le_bytes());
    prefixed.extend_from_slice(input);
    if out_len <= 64 {
        return blake2b(&prefixed, out_len);
    }
    let mut out = Vec::with_capacity(out_len);
    let mut v = blake2b(&prefixed, 64);
    out.extend_from_slice(&v[..32]);
    let mut remaining = out_len - 32;
    while remaining > 64 {
        v = blake2b(&v, 64);
        out.extend_from_slice(&v[..32]);
        remaining -= 32;
    }
    v = blake2b(&v, remaining);
    out.extend_from_slice(&v);
    out.truncate(out_len);
    out
}

// --------------------------------------------------------------------- argon2

type Block = [u64; 128];

#[inline]
fn blamka(x: u64, y: u64) -> u64 {
    let xl = x & 0xFFFF_FFFF;
    let yl = y & 0xFFFF_FFFF;
    x.wrapping_add(y)
        .wrapping_add(2u64.wrapping_mul(xl).wrapping_mul(yl))
}

/// The permutation `P` over 16 words.
fn permute(v: &mut [u64; 16]) {
    gb16(v, 0, 4, 8, 12);
    gb16(v, 1, 5, 9, 13);
    gb16(v, 2, 6, 10, 14);
    gb16(v, 3, 7, 11, 15);
    gb16(v, 0, 5, 10, 15);
    gb16(v, 1, 6, 11, 12);
    gb16(v, 2, 7, 8, 13);
    gb16(v, 3, 4, 9, 14);
}

#[inline]
fn gb16(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize) {
    v[a] = blamka(v[a], v[b]);
    v[d] = (v[d] ^ v[a]).rotate_right(32);
    v[c] = blamka(v[c], v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(24);
    v[a] = blamka(v[a], v[b]);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = blamka(v[c], v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(63);
}

/// `G(X, Y)`: the compression function from RFC 9106 §3.5.
fn compress(x: &Block, y: &Block) -> Block {
    let mut r: Block = [0u64; 128];
    for i in 0..128 {
        r[i] = x[i] ^ y[i];
    }
    let mut q = r;
    // Rows: 8 rows of 16 words.
    for row in 0..8 {
        let mut v = [0u64; 16];
        v.copy_from_slice(&q[row * 16..row * 16 + 16]);
        permute(&mut v);
        q[row * 16..row * 16 + 16].copy_from_slice(&v);
    }
    // Columns: 8 groups of 16 words, two from each row.
    for col in 0..8 {
        let mut v = [0u64; 16];
        for k in 0..8 {
            v[2 * k] = q[16 * k + 2 * col];
            v[2 * k + 1] = q[16 * k + 2 * col + 1];
        }
        permute(&mut v);
        for k in 0..8 {
            q[16 * k + 2 * col] = v[2 * k];
            q[16 * k + 2 * col + 1] = v[2 * k + 1];
        }
    }
    let mut out: Block = [0u64; 128];
    for i in 0..128 {
        out[i] = q[i] ^ r[i];
    }
    out
}

fn block_from_bytes(b: &[u8]) -> Block {
    let mut out = [0u64; 128];
    for i in 0..128 {
        let mut a = [0u8; 8];
        a.copy_from_slice(&b[i * 8..i * 8 + 8]);
        out[i] = u64::from_le_bytes(a);
    }
    out
}

fn block_to_bytes(b: &Block) -> Vec<u8> {
    let mut out = Vec::with_capacity(1024);
    for w in b.iter() {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
pub enum Variant {
    D,
    I,
    Id,
}

/// Argon2 core. Returns the raw tag of `out_len` bytes.
#[allow(clippy::too_many_arguments)] // the RFC's parameter list, verbatim
pub fn argon2(
    password: &[u8],
    salt: &[u8],
    secret: &[u8],
    ad: &[u8],
    passes: u32,
    memory_kib: u32,
    lanes: u32,
    out_len: usize,
    variant: Variant,
) -> Result<Vec<u8>, String> {
    if salt.len() < 8 {
        return Err(format!(
            "argon2: salt must be at least 8 bytes, got {}",
            salt.len()
        ));
    }
    if out_len < 4 {
        return Err("argon2: output must be at least 4 bytes".to_string());
    }
    if lanes == 0 || lanes > 0xFFFF {
        return Err(format!("argon2: lanes must be 1..65535, got {}", lanes));
    }
    if passes == 0 {
        return Err("argon2: passes must be at least 1".to_string());
    }
    let min_mem = 8 * lanes as u64;
    if (memory_kib as u64) < min_mem {
        return Err(format!(
            "argon2: memory must be at least 8*lanes = {} KiB",
            min_mem
        ));
    }
    if memory_kib as u64 > MAX_MEMORY_KIB {
        return Err(format!(
            "argon2: {} KiB exceeds the 1 GiB limit",
            memory_kib
        ));
    }
    let type_code: u32 = match variant {
        Variant::D => 0,
        Variant::I => 1,
        Variant::Id => 2,
    };

    // H0
    let mut h0_input = Vec::new();
    for v in [
        lanes,
        out_len as u32,
        memory_kib,
        passes,
        0x13u32,
        type_code,
    ] {
        h0_input.extend_from_slice(&v.to_le_bytes());
    }
    for part in [password, salt, secret, ad] {
        h0_input.extend_from_slice(&(part.len() as u32).to_le_bytes());
        h0_input.extend_from_slice(part);
    }
    let h0 = blake2b(&h0_input, 64);

    let m_prime = {
        let m = memory_kib as u64;
        let p = lanes as u64;
        4 * p * (m / (4 * p))
    } as usize;
    let lane_len = m_prime / lanes as usize;
    let segment_len = lane_len / 4;

    // First two blocks of each lane.
    let mut blocks: Vec<Block> = vec![[0u64; 128]; m_prime];
    for lane in 0..lanes as usize {
        for idx in 0..2usize {
            let mut input = Vec::with_capacity(72);
            input.extend_from_slice(&h0);
            input.extend_from_slice(&(idx as u32).to_le_bytes());
            input.extend_from_slice(&(lane as u32).to_le_bytes());
            let bytes = blake2b_long(1024, &input);
            blocks[lane * lane_len + idx] = block_from_bytes(&bytes);
        }
    }

    let zero_block: Block = [0u64; 128];
    for pass in 0..passes {
        for slice in 0..4usize {
            for lane in 0..lanes as usize {
                let data_independent = match variant {
                    Variant::I => true,
                    Variant::Id => pass == 0 && slice < 2,
                    Variant::D => false,
                };
                let mut address_block: Block = [0u64; 128];
                let mut input_block: Block = [0u64; 128];
                let mut counter: u64 = 0;
                if data_independent {
                    input_block[0] = pass as u64;
                    input_block[1] = lane as u64;
                    input_block[2] = slice as u64;
                    input_block[3] = m_prime as u64;
                    input_block[4] = passes as u64;
                    input_block[5] = type_code as u64;
                    // counter starts at 1 for the first address block
                    input_block[6] = 1;
                    address_block = compress(&zero_block, &compress(&zero_block, &input_block));
                    counter = 1;
                }
                let start = if pass == 0 && slice == 0 { 2 } else { 0 };
                for index in start..segment_len {
                    let cur = lane * lane_len + slice * segment_len + index;
                    let prev = if slice == 0 && index == 0 {
                        lane * lane_len + lane_len - 1
                    } else {
                        cur - 1
                    };
                    let (j1, j2) = if data_independent {
                        if index % 128 == 0 && index != 0 {
                            counter += 1;
                            input_block[6] = counter;
                            address_block =
                                compress(&zero_block, &compress(&zero_block, &input_block));
                        }
                        let w = address_block[index % 128];
                        (w & 0xFFFF_FFFF, w >> 32)
                    } else {
                        let w = blocks[prev][0];
                        (w & 0xFFFF_FFFF, w >> 32)
                    };
                    // Reference lane.
                    let mut ref_lane = (j2 % lanes as u64) as usize;
                    if pass == 0 && slice == 0 {
                        ref_lane = lane;
                    }
                    // Reference area size.
                    let same_lane = ref_lane == lane;
                    let area: u64 = if pass == 0 {
                        if slice == 0 {
                            index as u64 - 1
                        } else if same_lane {
                            (slice * segment_len + index - 1) as u64
                        } else {
                            (slice * segment_len) as u64 - if index == 0 { 1 } else { 0 }
                        }
                    } else if same_lane {
                        (lane_len - segment_len + index - 1) as u64
                    } else {
                        (lane_len - segment_len) as u64 - if index == 0 { 1 } else { 0 }
                    };
                    let area = if area == 0 { 1 } else { area };
                    let rel = (j1 * j1) >> 32;
                    let relative = area - 1 - ((area * rel) >> 32);
                    let start_pos = if pass == 0 || slice == 3 {
                        0
                    } else {
                        ((slice + 1) * segment_len) as u64
                    };
                    let ref_index = ((start_pos + relative) % lane_len as u64) as usize;
                    let ref_block = blocks[ref_lane * lane_len + ref_index];
                    let prev_block = blocks[prev];
                    let mut new_block = compress(&prev_block, &ref_block);
                    if pass > 0 {
                        for i in 0..128 {
                            new_block[i] ^= blocks[cur][i];
                        }
                    }
                    blocks[cur] = new_block;
                }
            }
        }
    }

    // XOR the last block of every lane, then H'.
    let mut final_block = blocks[lane_len - 1];
    for lane in 1..lanes as usize {
        let b = blocks[lane * lane_len + lane_len - 1];
        for i in 0..128 {
            final_block[i] ^= b[i];
        }
    }
    Ok(blake2b_long(out_len, &block_to_bytes(&final_block)))
}

fn n_argon2(ctx_args: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let _ = ctx_args;
    if args.len() < 6 || args.len() > 8 {
        return Err(format!(
            "argon2 expects 6 to 8 arguments (password, salt, passes, memory_kib, lanes, variant, out_len=32, mode=id), got {}",
            args.len()
        ));
    }
    let password = match &args[0] {
        Value::Str(s) => s.as_bytes().to_vec(),
        Value::Bytes(b) => b.lock().unwrap().clone(),
        other => {
            return Err(format!(
                "argon2: password must be a string or bytes, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    let salt = match &args[1] {
        Value::Str(s) => s.as_bytes().to_vec(),
        Value::Bytes(b) => b.lock().unwrap().clone(),
        other => {
            return Err(format!(
                "argon2: salt must be a string or bytes, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    let int_arg = |i: usize, name: &str| -> Result<i64, String> {
        match &args[i] {
            Value::Int(v) => Ok(*v),
            other => Err(format!(
                "argon2: {} must be an int, found {}",
                name,
                crate::value::type_name(other)
            )),
        }
    };
    // The raw `as` casts below silently wrap negatives (-1 -> u32::MAX /
    // usize::MAX), which either never terminates (passes = 2^32-1) or
    // aborts the allocator (out_len = usize::MAX). Validate like scrypt
    // does: strictly positive, with sane upper bounds.
    let positive = |v: i64, name: &str| -> Result<u32, String> {
        if v <= 0 {
            return Err(format!("argon2: {} must be positive, got {}", name, v));
        }
        if v > u32::MAX as i64 {
            return Err(format!("argon2: {} is implausibly large: {}", name, v));
        }
        Ok(v as u32)
    };
    let passes = positive(int_arg(2, "passes")?, "passes")?;
    let memory = positive(int_arg(3, "memory_kib")?, "memory_kib")?;
    let lanes = positive(int_arg(4, "lanes")?, "lanes")?;
    let variant_name = match &args[5] {
        Value::Str(s) => s.to_string(),
        other => {
            return Err(format!(
                "argon2: variant must be a string (\"d\"/\"i\"/\"id\"), found {}",
                crate::value::type_name(other)
            ))
        }
    };
    let variant = match variant_name.as_str() {
        "d" | "argon2d" => Variant::D,
        "i" | "argon2i" => Variant::I,
        "id" | "argon2id" => Variant::Id,
        other => {
            return Err(format!(
                "argon2: variant must be \"d\", \"i\" or \"id\", got {:?}",
                other
            ))
        }
    };
    let out_len = if args.len() >= 7 {
        let v = int_arg(6, "out_len")?;
        if v <= 0 || v > 1024 {
            return Err(format!(
                "argon2: out_len must be between 1 and 1024, got {}",
                v
            ));
        }
        v as usize
    } else {
        32
    };
    let tag = argon2(
        &password,
        &salt,
        &[],
        &[],
        passes,
        memory,
        lanes,
        out_len,
        variant,
    )?;
    Ok(Value::Str(crate::pkg::hex_encode(&tag).into()))
}

pub fn native_defs() -> Vec<crate::stdlib::NativeDef> {
    vec![("argon2", n_argon2)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blake2b_empty() {
        // RFC 7693 Appendix A: Blake2b-512("abc").
        assert_eq!(
            crate::pkg::hex_encode(&blake2b(b"abc", 64)),
            "ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d1\
             7d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923"
        );
    }

    #[test]
    fn argon2id_matches_openssl() {
        // openssl kdf -keylen 32 -kdfopt pass:password -kdfopt salt:somesalt12345678
        //   -kdfopt iter:3 -kdfopt memcost:32 -kdfopt lanes:4 ARGON2ID
        let tag = argon2(
            b"password",
            b"somesalt12345678",
            b"",
            b"",
            3,
            32,
            4,
            32,
            Variant::Id,
        )
        .unwrap();
        assert_eq!(
            crate::pkg::hex_encode(&tag),
            "cd342db86d5658c8e3267ba1c32c14f66bd95c49ca2c842dbff0e298d37817c9"
        );
    }

    #[test]
    fn argon2i_matches_openssl() {
        let tag = argon2(
            b"password",
            b"somesalt12345678",
            b"",
            b"",
            3,
            32,
            4,
            32,
            Variant::I,
        )
        .unwrap();
        assert_eq!(
            crate::pkg::hex_encode(&tag),
            "ae04861538d7375fa00ec326ee8c7f679cd7020e24d634a041946dafa3bc9568"
        );
    }

    #[test]
    fn argon2d_matches_openssl() {
        let tag = argon2(
            b"password",
            b"somesalt12345678",
            b"",
            b"",
            3,
            32,
            4,
            32,
            Variant::D,
        )
        .unwrap();
        assert_eq!(
            crate::pkg::hex_encode(&tag),
            "fd4bedc4e13fbd88c0ddbfa94be889f28a1b438e0bae4c390864cd656d52ade0"
        );
    }

    #[test]
    fn argon2id_single_lane_matches_openssl() {
        let tag = argon2(
            b"hunter2",
            b"abcdefgh12345678",
            b"",
            b"",
            2,
            64,
            1,
            32,
            Variant::Id,
        )
        .unwrap();
        assert_eq!(
            crate::pkg::hex_encode(&tag),
            "7d09462c5493d1d2d8bddf6b8a18ee7002d1628ea26ad14c4d8e1e14f3e3b84b"
        );
    }
}
