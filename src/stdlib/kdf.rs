//! `scrypt` (RFC 7914) — the memory-hard password KDF, alongside the existing
//! `pbkdf2_sha256`.
//!
//! Implemented from the RFC: PBKDF2-HMAC-SHA256 → ROMix (Salsa20/8 BlockMix)
//! → PBKDF2-HMAC-SHA256. Verified against CPython's `hashlib.scrypt`, which is
//! backed by OpenSSL. Deliberately not here: bcrypt and argon2 (see §3.5).

use crate::pkg::sha256_bytes;
use crate::value::{NativeCtx, Value};

/// Refuse parameters whose working set would exceed this, so a bad call
/// cannot take the process down (128 * N * r bytes per lane, times p).
const MAX_MEMORY: u64 = 1024 * 1024 * 1024;

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&sha256_bytes(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Vec::with_capacity(BLOCK + msg.len());
    let mut outer = Vec::with_capacity(BLOCK + 32);
    for &b in k.iter() {
        inner.push(b ^ 0x36);
        outer.push(b ^ 0x5c);
    }
    inner.extend_from_slice(msg);
    let ih = sha256_bytes(&inner);
    outer.extend_from_slice(&ih);
    sha256_bytes(&outer)
}

/// PBKDF2-HMAC-SHA256 with an arbitrary output length (the existing native
/// only produces one 32-byte block).
pub(crate) fn pbkdf2(pw: &[u8], salt: &[u8], iters: u32, dklen: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(dklen);
    let mut block: u32 = 1;
    while out.len() < dklen {
        let mut seed = Vec::with_capacity(salt.len() + 4);
        seed.extend_from_slice(salt);
        seed.extend_from_slice(&block.to_be_bytes());
        let mut u = hmac_sha256(pw, &seed);
        let mut acc = u;
        for _ in 1..iters {
            u = hmac_sha256(pw, &u);
            for i in 0..32 {
                acc[i] ^= u[i];
            }
        }
        out.extend_from_slice(&acc);
        block += 1;
    }
    out.truncate(dklen);
    out
}

fn salsa20_8(input: &[u8; 64]) -> [u8; 64] {
    let mut x = [0u32; 16];
    for i in 0..16 {
        x[i] = u32::from_le_bytes([
            input[i * 4],
            input[i * 4 + 1],
            input[i * 4 + 2],
            input[i * 4 + 3],
        ]);
    }
    let orig = x;
    for _ in 0..4 {
        // Column round.
        x[4] ^= x[0].wrapping_add(x[12]).rotate_left(7);
        x[8] ^= x[4].wrapping_add(x[0]).rotate_left(9);
        x[12] ^= x[8].wrapping_add(x[4]).rotate_left(13);
        x[0] ^= x[12].wrapping_add(x[8]).rotate_left(18);
        x[9] ^= x[5].wrapping_add(x[1]).rotate_left(7);
        x[13] ^= x[9].wrapping_add(x[5]).rotate_left(9);
        x[1] ^= x[13].wrapping_add(x[9]).rotate_left(13);
        x[5] ^= x[1].wrapping_add(x[13]).rotate_left(18);
        x[14] ^= x[10].wrapping_add(x[6]).rotate_left(7);
        x[2] ^= x[14].wrapping_add(x[10]).rotate_left(9);
        x[6] ^= x[2].wrapping_add(x[14]).rotate_left(13);
        x[10] ^= x[6].wrapping_add(x[2]).rotate_left(18);
        x[3] ^= x[15].wrapping_add(x[11]).rotate_left(7);
        x[7] ^= x[3].wrapping_add(x[15]).rotate_left(9);
        x[11] ^= x[7].wrapping_add(x[3]).rotate_left(13);
        x[15] ^= x[11].wrapping_add(x[7]).rotate_left(18);
        // Row round.
        x[1] ^= x[0].wrapping_add(x[3]).rotate_left(7);
        x[2] ^= x[1].wrapping_add(x[0]).rotate_left(9);
        x[3] ^= x[2].wrapping_add(x[1]).rotate_left(13);
        x[0] ^= x[3].wrapping_add(x[2]).rotate_left(18);
        x[6] ^= x[5].wrapping_add(x[4]).rotate_left(7);
        x[7] ^= x[6].wrapping_add(x[5]).rotate_left(9);
        x[4] ^= x[7].wrapping_add(x[6]).rotate_left(13);
        x[5] ^= x[4].wrapping_add(x[7]).rotate_left(18);
        x[11] ^= x[10].wrapping_add(x[9]).rotate_left(7);
        x[8] ^= x[11].wrapping_add(x[10]).rotate_left(9);
        x[9] ^= x[8].wrapping_add(x[11]).rotate_left(13);
        x[10] ^= x[9].wrapping_add(x[8]).rotate_left(18);
        x[12] ^= x[15].wrapping_add(x[14]).rotate_left(7);
        x[13] ^= x[12].wrapping_add(x[15]).rotate_left(9);
        x[14] ^= x[13].wrapping_add(x[12]).rotate_left(13);
        x[15] ^= x[14].wrapping_add(x[13]).rotate_left(18);
    }
    let mut out = [0u8; 64];
    for i in 0..16 {
        out[i * 4..i * 4 + 4].copy_from_slice(&x[i].wrapping_add(orig[i]).to_le_bytes());
    }
    out
}

fn xor_into(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst.iter_mut().zip(src.iter()) {
        *d ^= *s;
    }
}

/// BlockMix: 2r 64-byte blocks in, 2r out, in the even-then-odd order the
/// RFC specifies.
fn block_mix(input: &[u8], r: usize) -> Vec<u8> {
    let mut x = [0u8; 64];
    x.copy_from_slice(&input[(2 * r - 1) * 64..2 * r * 64]);
    let mut y = vec![0u8; 2 * r * 64];
    for i in 0..2 * r {
        let mut t = [0u8; 64];
        t.copy_from_slice(&input[i * 64..i * 64 + 64]);
        xor_into(&mut t, &x);
        x = salsa20_8(&t);
        y[i * 64..i * 64 + 64].copy_from_slice(&x);
    }
    let mut out = vec![0u8; 2 * r * 64];
    for i in 0..r {
        out[i * 64..i * 64 + 64].copy_from_slice(&y[2 * i * 64..2 * i * 64 + 64]);
    }
    for i in 0..r {
        out[(r + i) * 64..(r + i) * 64 + 64]
            .copy_from_slice(&y[(2 * i + 1) * 64..(2 * i + 1) * 64 + 64]);
    }
    out
}

fn romix(block: &mut Vec<u8>, n: usize, r: usize) {
    let block_len = 128 * r;
    let mut v = vec![0u8; block_len * n];
    let mut x = block.clone();
    for i in 0..n {
        v[i * block_len..(i + 1) * block_len].copy_from_slice(&x);
        x = block_mix(&x, r);
    }
    for _ in 0..n {
        let last = (2 * r - 1) * 64;
        let mut j = 0usize;
        for k in 0..8 {
            j |= (x[last + k] as usize) << (8 * k);
        }
        j &= n - 1;
        let base = j * block_len;
        for k in 0..block_len {
            x[k] ^= v[base + k];
        }
        x = block_mix(&x, r);
    }
    *block = x;
}

/// `scrypt(password, salt, n, r, p, dklen)` — hex digest, like `pbkdf2_sha256`.
pub fn scrypt_hex(
    password: &[u8],
    salt: &[u8],
    n: u64,
    r: u64,
    p: u64,
    dklen: usize,
) -> Result<String, String> {
    if n < 2 || !n.is_power_of_two() {
        return Err(format!(
            "scrypt: n must be a power of two greater than 1, got {}",
            n
        ));
    }
    if r == 0 || p == 0 {
        return Err("scrypt: r and p must be at least 1".to_string());
    }
    if dklen == 0 {
        return Err("scrypt: dklen must be at least 1".to_string());
    }
    let per_lane = 128u64
        .checked_mul(n)
        .and_then(|x| x.checked_mul(r))
        .ok_or_else(|| "scrypt: parameters overflow".to_string())?;
    let total = per_lane
        .checked_mul(p)
        .ok_or_else(|| "scrypt: parameters overflow".to_string())?;
    if total > MAX_MEMORY {
        return Err(format!(
            "scrypt: parameters need {} MiB, above the 1024 MiB limit",
            total / (1024 * 1024)
        ));
    }
    let n = n as usize;
    let r = r as usize;
    let p = p as usize;
    let mut b = pbkdf2(password, salt, 1, p * 128 * r);
    for lane in 0..p {
        let start = lane * 128 * r;
        let mut block = b[start..start + 128 * r].to_vec();
        romix(&mut block, n, r);
        b[start..start + 128 * r].copy_from_slice(&block);
    }
    let dk = pbkdf2(password, &b, 1, dklen);
    Ok(crate::pkg::hex_encode(&dk))
}

fn n_scrypt(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() < 5 || args.len() > 6 {
        return Err(format!(
            "scrypt expects 5 or 6 arguments (password, salt, n, r, p, dklen=32), got {}",
            args.len()
        ));
    }
    let password = match &args[0] {
        Value::Str(s) => s.as_bytes().to_vec(),
        Value::Bytes(b) => b.lock().unwrap().clone(),
        other => {
            return Err(format!(
                "scrypt: password must be a string or bytes, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    let salt = match &args[1] {
        Value::Str(s) => s.as_bytes().to_vec(),
        Value::Bytes(b) => b.lock().unwrap().clone(),
        other => {
            return Err(format!(
                "scrypt: salt must be a string or bytes, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    let int_arg = |i: usize, name: &str| -> Result<u64, String> {
        match &args[i] {
            Value::Int(v) if *v > 0 => Ok(*v as u64),
            Value::Int(v) => Err(format!("scrypt: {} must be positive, got {}", name, v)),
            other => Err(format!(
                "scrypt: {} must be an int, found {}",
                name,
                crate::value::type_name(other)
            )),
        }
    };
    let n = int_arg(2, "n")?;
    let r = int_arg(3, "r")?;
    let p = int_arg(4, "p")?;
    let dklen = if args.len() == 6 {
        int_arg(5, "dklen")? as usize
    } else {
        32
    };
    let hex = scrypt_hex(&password, &salt, n, r, p, dklen)?;
    Ok(Value::Str(hex.into()))
}

pub fn native_defs() -> Vec<crate::stdlib::NativeDef> {
    vec![("scrypt", n_scrypt)]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7914 §12: scrypt("", "", N=16, r=1, p=1, dkLen=64).
    #[test]
    fn rfc7914_vector_1() {
        let got = scrypt_hex(b"", b"", 16, 1, 1, 64).unwrap();
        assert_eq!(
            got,
            "77d6576238657b203b19ca42c18a0497f16b4844e3074ae8dfdffa3fede21442\
             fcd0069ded0948f8326a753a0fc81f17e8d3e0fb2e0d3628cf35e20c38d18906"
        );
    }

    /// RFC 7914 §12: scrypt("password", "NaCl", N=1024, r=8, p=16, dkLen=64).
    #[test]
    fn rfc7914_vector_2() {
        let got = scrypt_hex(b"password", b"NaCl", 1024, 8, 16, 64).unwrap();
        assert_eq!(
            got,
            "fdbabe1c9d3472007856e7190d01e9fe7c6ad7cbc8237830e77376634b373162\
             2eaf30d92e22a3886ff109279d9830dac727afb94a83ee6d8360cbdfa2cc0640"
        );
    }

    #[test]
    fn rejects_bad_parameters() {
        assert!(scrypt_hex(b"p", b"s", 15, 1, 1, 32).is_err()); // not a power of two
        assert!(scrypt_hex(b"p", b"s", 1, 1, 1, 32).is_err()); // n < 2
        assert!(scrypt_hex(b"p", b"s", 1024, 0, 1, 32).is_err()); // r = 0
        assert!(scrypt_hex(b"p", b"s", 1 << 40, 8, 1, 32).is_err()); // too much memory
    }
}
