//! SHA-512 (FIPS 180-4). Companion to [`crate::pkg::sha256`]: same Merkle-
//! Damgard structure, but 64-bit words, 80 rounds, 128-byte blocks, a
//! different IV and K table, and a 128-bit length field. Exposed to Spid3r
//! programs as the `sha512` builtin and used by Ed25519 (RFC 8032), whose
//! every hash is a SHA-512.

/// Round constants for SHA-512: the first 64 bits of the fractional parts of
/// the cube roots of the first 80 primes.
const K: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

/// SHA-512 of `data` as the raw 64-byte digest (FIPS 180-4).
pub fn sha512_bytes(data: &[u8]) -> [u8; 64] {
    let mut h: [u64; 8] = [
        0x6a09e667f3bcc908,
        0xbb67ae8584caa73b,
        0x3c6ef372fe94f82b,
        0xa54ff53a5f1d36f1,
        0x510e527fade682d1,
        0x9b05688c2b3e6c1f,
        0x1f83d9abfb41bd6b,
        0x5be0cd19137e2179,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u128).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 128 != 112 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.as_chunks::<128>().0 {
        let mut w = [0u64; 80];
        for i in 0..16 {
            let mut word = [0u8; 8];
            word.copy_from_slice(&chunk[i * 8..i * 8 + 8]);
            w[i] = u64::from_be_bytes(word);
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 64];
    for (i, word) in h.iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// SHA-512 of `data`, lowercase hex. Exposed to Spid3r programs as `sha512`.
pub fn sha512_hex(data: &[u8]) -> String {
    crate::pkg::hex_encode(&sha512_bytes(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- first-principles derivation of the K table (guard test) ----
    //
    // FIPS 180-4: K[i] is the first 64 bits of the fractional part of the
    // cube root of the i-th prime. We recompute every entry from scratch
    // with exact 256-bit integer arithmetic, so a single mistyped constant
    // anywhere in the table fails here instead of silently producing wrong
    // digests.

    /// Full 4x4 -> 8 limb multiply (256-bit inputs).
    fn mul_wide(a: &[u64; 4], b: &[u64; 4]) -> [u64; 8] {
        let mut out = [0u64; 8];
        for i in 0..4 {
            let mut carry = 0u128;
            for j in 0..4 {
                let t = a[i] as u128 * b[j] as u128 + out[i + j] as u128 + carry;
                out[i + j] = t as u64;
                carry = t >> 64;
            }
            let mut k = i + 4;
            while carry > 0 {
                let t = out[k] as u128 + carry;
                out[k] = t as u64;
                carry = t >> 64;
                k += 1;
            }
        }
        out
    }

    /// Multiply two values that are known to fit in 256 bits.
    fn mul256(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
        let wide = mul_wide(a, b);
        assert!(
            wide[4..].iter().all(|w| *w == 0),
            "product overflows 256 bits: a={a:?} b={b:?} wide={wide:?}"
        );
        let mut out = [0u64; 4];
        out.copy_from_slice(&wide[..4]);
        out
    }

    fn dec(r: &mut [u64; 4]) {
        for limb in r.iter_mut() {
            let (next, overflow) = limb.overflowing_sub(1);
            *limb = next;
            if !overflow {
                return;
            }
        }
        panic!("underflow decrementing 256-bit value");
    }

    fn cmp256(a: &[u64; 4], b: &[u64; 4]) -> std::cmp::Ordering {
        for i in (0..4).rev() {
            if a[i] != b[i] {
                return a[i].cmp(&b[i]);
            }
        }
        std::cmp::Ordering::Equal
    }

    fn first_80_primes() -> Vec<u64> {
        let mut primes = Vec::new();
        let mut n = 2u64;
        while primes.len() < 80 {
            if primes.iter().all(|p| n % p != 0) {
                primes.push(n);
            }
            n += 1;
        }
        primes
    }

    #[test]
    fn k_table_matches_cube_roots_of_primes() {
        let mut bad = Vec::new();
        for (i, p) in first_80_primes().into_iter().enumerate() {
            // r = floor(p^(1/3) * 2^64): the largest r with r^3 <= p * 2^192.
            // p * 2^192 = p shifted into limb 3 (p < 2^10, primes <= 409).
            let target = [0u64, 0, 0, p];
            // 1 <= cbrt(p) < 8 for p <= 409, so r is in [2^64, 2^67).
            let mut lo = [0u64, 1, 0, 0]; // pred(2^64) = (1 <= cbrt) true
            let mut hi = [0u64, 8, 0, 0]; // pred(2^67) = (8 <= cbrt) false
            while cmp256(&lo, &hi) == std::cmp::Ordering::Less {
                // mid = (lo + hi + 1) / 2, no overflow: both < 2^67.
                let mut sum = [0u64; 4];
                let mut carry = 0u128;
                for k in 0..4 {
                    let plus = if k == 0 { 1u128 } else { 0 };
                    let t = lo[k] as u128 + hi[k] as u128 + plus + carry;
                    sum[k] = t as u64;
                    carry = t >> 64;
                }
                let mut mid = [0u64; 4];
                for k in 0..4 {
                    mid[k] = (sum[k] >> 1) | (if k + 1 < 4 { sum[k + 1] & 1 } else { 0 }) << 63;
                }
                let cube = mul256(&mul256(&mid, &mid), &mid);
                if cmp256(&cube, &target) != std::cmp::Ordering::Greater {
                    lo = mid;
                } else {
                    dec(&mut mid);
                    hi = mid;
                }
            }
            // K = floor(frac(cbrt(p)) * 2^64) = r mod 2^64 (the fraction's
            // bits; r's limb 0 is exactly those 64 bits).
            if K[i] != lo[0] {
                bad.push(format!(
                    "K[{}] = {:#018x}, table has {:#018x}",
                    i, lo[0], K[i]
                ));
            }
        }
        assert!(bad.is_empty(), "K table mismatches:\n{}", bad.join("\n"));
    }

    #[test]
    fn sha512_matches_known_vectors() {
        assert_eq!(
            sha512_hex(b""),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce\
             47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
        assert_eq!(
            sha512_hex(b"abc"),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }

    #[test]
    fn sha512_handles_two_block_message() {
        // The FIPS 180-4 two-block vector (112 bytes). Verified against
        // Windows certutil -hashfile (independent implementation).
        let m = b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmno\
                  ijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";
        assert_eq!(m.len(), 112);
        assert_eq!(
            sha512_hex(m),
            "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018\
             501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909"
        );
    }

    #[test]
    fn sha512_handles_multi_block_length_padding() {
        // 200 bytes: forces three blocks (200 > 112), so the 128-bit length
        // field and the padding loop's boundary conditions are all exercised.
        // Verified against Windows certutil -hashfile.
        let m = [b'a'; 200];
        assert_eq!(
            sha512_hex(&m),
            "4b11459c33f52a22ee8236782714c150a3b2c60994e9acee17fe68947a3e6789f\
             31e7668394592da7bef827cddca88c4e6f86e4df7ed1ae6cba71f3e98faee9f"
        );
    }

    #[test]
    fn sha512_padding_boundary_lengths_match_reference() {
        // Lengths around the padding edge cases: 111 (0x80 fits in the last
        // block), 112 (exactly fills the message word, forcing an extra
        // padding block), 127, 128. All verified against Windows
        // `certutil -hashfile` (an independent implementation).
        for (len, expected) in [
            (111usize, "63fa5a401595a853303fb88e6033941169e71974d10dd0c733a0e7df452de15d2b27b693de09a51027690187bc8e042cf1d78698a483f7c21c5c788c9e3f1d8a"),
            (112, "8854587e6396740f2d09535f38bdc315b7c48e3c7d844d97e3fb17f9b95fce9a4ce6d1a38a37142fe6fba7bf2e7947c232137fb4b9968cac1fd253b123d2729f"),
            (127, "96b296a81dc1897a871460905924b885eb683ea19b5eaa379adc92043cae31343c3b11f6a06e66d1bb5fd1078ecaeb07ee35a89d8e75f63cdb1f2c12756b1325"),
            (128, "9ec48a5196300954208efd9435e508a6a23beb0c3dcf3bac052c615f705a40c4e3c0ce3e7ec875382fc47f06b447dad3944b6750c1b0edc51ab18311f4ca6b13"),
        ] {
            let m: Vec<u8> = (0..len).map(|i| b'A' + (i % 26) as u8).collect();
            assert_eq!(sha512_hex(&m), expected, "length {}", len);
        }
    }
}
