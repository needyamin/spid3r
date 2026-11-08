//! Ed25519 signatures (RFC 8032), hand-written in the style of TweetNaCl.
//!
//! Zero dependencies: field arithmetic mod p = 2^255 - 19 uses five 51-bit
//! limbs (`u64` products widened to `u128`), group arithmetic uses extended
//! twisted-Edwards coordinates (X, Y, Z, T) with the strongly unified
//! addition formulas from RFC 8032 section 5.1.4, and scalars are reduced
//! mod the group order L with a bit-by-bit conditional subtract (simple and
//! obviously correct, fast enough for a scripting-language builtin).
//!
//! Verification follows TweetNaCl's `crypto_sign_open`: reject a
//! non-canonical S (S >= L), decode the public key, then check
//! [S]B == R + [H(R||A||M)]A (equivalently [S]B - [h]A == R) — the
//! non-cofactored check the RFC specifies. Every failure path returns
//! `false`; nothing panics on malformed input.

use crate::pkg::sha512::sha512_bytes;

// ---------------------------------------------------------------- field

/// Field element mod p = 2^255 - 19: five limbs, each holding 51 bits.
/// Invariant after every operation: limbs < 2^52.
#[derive(Clone, Copy, Debug)]
struct Fe([u64; 5]);

const MASK51: u64 = (1 << 51) - 1;

/// p in canonical 51-bit limbs (limb 0 is the low limb).
const P_LIMBS: [u64; 5] = [(1 << 51) - 19, MASK51, MASK51, MASK51, MASK51];

impl Fe {
    const ZERO: Fe = Fe([0, 0, 0, 0, 0]);
    const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    fn from_u64(v: u64) -> Fe {
        Fe([v, 0, 0, 0, 0])
    }

    /// Single-pass carry: folds every limb's bits above 51 into the neighbor,
    /// wrapping limb 4's overflow back into limb 0 as *19. Keeps limbs < 2^52.
    fn weak_reduce(&mut self) {
        let c0 = self.0[0] >> 51;
        let c1 = self.0[1] >> 51;
        let c2 = self.0[2] >> 51;
        let c3 = self.0[3] >> 51;
        let c4 = self.0[4] >> 51;
        self.0[0] &= MASK51;
        self.0[1] &= MASK51;
        self.0[2] &= MASK51;
        self.0[3] &= MASK51;
        self.0[4] &= MASK51;
        self.0[0] += c4 * 19;
        self.0[1] += c0;
        self.0[2] += c1;
        self.0[3] += c2;
        self.0[4] += c3;
    }

    /// Fully reduce into [0, p). Adds 19*q where q = 1 iff value >= p, then
    /// drops the carry out of the top limb: x + 19 - 2^255 = x - p.
    fn canonical(mut self) -> Fe {
        self.weak_reduce();
        let mut q = (self.0[0] + 19) >> 51;
        q = (self.0[1] + q) >> 51;
        q = (self.0[2] + q) >> 51;
        q = (self.0[3] + q) >> 51;
        q = (self.0[4] + q) >> 51;
        self.0[0] += 19 * q;
        self.0[1] += self.0[0] >> 51;
        self.0[0] &= MASK51;
        self.0[2] += self.0[1] >> 51;
        self.0[1] &= MASK51;
        self.0[3] += self.0[2] >> 51;
        self.0[2] &= MASK51;
        self.0[4] += self.0[3] >> 51;
        self.0[3] &= MASK51;
        self.0[4] &= MASK51; // drop the 2^255 bit
        self
    }

    fn from_bytes(b: &[u8; 32]) -> Fe {
        let load = |i: usize| -> u64 {
            let mut w = [0u8; 8];
            w.copy_from_slice(&b[i..i + 8]);
            u64::from_le_bytes(w)
        };
        Fe([
            load(0) & MASK51,
            (load(6) >> 3) & MASK51,
            (load(12) >> 6) & MASK51,
            (load(19) >> 1) & MASK51,
            (load(24) >> 12) & MASK51,
        ])
    }

    /// Canonical little-endian encoding (32 bytes).
    fn to_bytes(self) -> [u8; 32] {
        let l = self.canonical().0;
        let w0 = l[0] | ((l[1] & 0x1fff) << 51);
        let w1 = (l[1] >> 13) | ((l[2] & 0x3ffffff) << 38);
        let w2 = (l[2] >> 26) | ((l[3] & 0x7fffffffff) << 25);
        let w3 = (l[3] >> 39) | (l[4] << 12);
        let mut out = [0u8; 32];
        out[0..8].copy_from_slice(&w0.to_le_bytes());
        out[8..16].copy_from_slice(&w1.to_le_bytes());
        out[16..24].copy_from_slice(&w2.to_le_bytes());
        out[24..32].copy_from_slice(&w3.to_le_bytes());
        out
    }

    fn add(&self, o: &Fe) -> Fe {
        let mut r = Fe([
            self.0[0] + o.0[0],
            self.0[1] + o.0[1],
            self.0[2] + o.0[2],
            self.0[3] + o.0[3],
            self.0[4] + o.0[4],
        ]);
        r.weak_reduce();
        r
    }

    fn sub(&self, o: &Fe) -> Fe {
        // p - o with borrow propagation (o canonical, so no final borrow),
        // then add: self + (p - o) == self - o (mod p).
        let o = o.canonical();
        let mut nb = [0i64; 5];
        let mut borrow = 0i64;
        for i in 0..5 {
            let v = P_LIMBS[i] as i64 - o.0[i] as i64 - borrow;
            if v < 0 {
                nb[i] = v + (1 << 51);
                borrow = 1;
            } else {
                nb[i] = v;
                borrow = 0;
            }
        }
        debug_assert_eq!(borrow, 0);
        let mut r = Fe([
            self.0[0] + nb[0] as u64,
            self.0[1] + nb[1] as u64,
            self.0[2] + nb[2] as u64,
            self.0[3] + nb[3] as u64,
            self.0[4] + nb[4] as u64,
        ]);
        r.weak_reduce();
        r
    }

    fn neg(&self) -> Fe {
        Fe::ZERO.sub(self)
    }

    /// Schoolbook multiply in u128, carry down from limb 0, fold the top
    /// carry (times 19) back into limb 0.
    fn mul(&self, o: &Fe) -> Fe {
        let a = &self.0;
        let b = &o.0;
        let b1_19 = b[1] * 19;
        let b2_19 = b[2] * 19;
        let b3_19 = b[3] * 19;
        let b4_19 = b[4] * 19;
        let c: [u128; 5] = [
            a[0] as u128 * b[0] as u128
                + a[1] as u128 * b4_19 as u128
                + a[2] as u128 * b3_19 as u128
                + a[3] as u128 * b2_19 as u128
                + a[4] as u128 * b1_19 as u128,
            a[0] as u128 * b[1] as u128
                + a[1] as u128 * b[0] as u128
                + a[2] as u128 * b4_19 as u128
                + a[3] as u128 * b3_19 as u128
                + a[4] as u128 * b2_19 as u128,
            a[0] as u128 * b[2] as u128
                + a[1] as u128 * b[1] as u128
                + a[2] as u128 * b[0] as u128
                + a[3] as u128 * b4_19 as u128
                + a[4] as u128 * b3_19 as u128,
            a[0] as u128 * b[3] as u128
                + a[1] as u128 * b[2] as u128
                + a[2] as u128 * b[1] as u128
                + a[3] as u128 * b[0] as u128
                + a[4] as u128 * b4_19 as u128,
            a[0] as u128 * b[4] as u128
                + a[1] as u128 * b[3] as u128
                + a[2] as u128 * b[2] as u128
                + a[3] as u128 * b[1] as u128
                + a[4] as u128 * b[0] as u128,
        ];
        const M: u128 = MASK51 as u128;
        let mut out = [0u64; 5];
        out[0] = (c[0] & M) as u64;
        let c1 = c[1] + (c[0] >> 51);
        out[1] = (c1 & M) as u64;
        let c2 = c[2] + (c1 >> 51);
        out[2] = (c2 & M) as u64;
        let c3 = c[3] + (c2 >> 51);
        out[3] = (c3 & M) as u64;
        let c4 = c[4] + (c3 >> 51);
        out[4] = (c4 & M) as u64;
        let carry = (c4 >> 51) as u64;
        out[0] += carry * 19;
        out[1] += out[0] >> 51;
        out[0] &= MASK51;
        out[2] += out[1] >> 51;
        out[1] &= MASK51;
        out[3] += out[2] >> 51;
        out[2] &= MASK51;
        out[4] += out[3] >> 51;
        out[3] &= MASK51;
        Fe(out)
    }

    fn square(&self) -> Fe {
        self.mul(self)
    }

    /// Square-and-multiply over a little-endian 256-bit exponent.
    fn pow(&self, exp: &[u8; 32]) -> Fe {
        let mut result = Fe::ONE;
        let mut base = *self;
        for i in 0..256 {
            if (exp[i / 8] >> (i % 8)) & 1 == 1 {
                result = result.mul(&base);
            }
            base = base.square();
        }
        result
    }

    /// Modular inverse: x^(p-2) mod p.
    fn invert(&self) -> Fe {
        self.pow(&exponent_bytes(255, 21)) // p - 2 = 2^255 - 21
    }

    fn is_zero(&self) -> bool {
        self.canonical().0 == [0, 0, 0, 0, 0]
    }

    fn eq(&self, o: &Fe) -> bool {
        self.to_bytes() == o.to_bytes()
    }
}

/// Build the little-endian 32-byte encoding of `2^shift - minus`, used for
/// the (p-5)/8, (p-1)/4 and (p-2) exponents. Computing these from two
/// integers beats transcribing 32-byte constants by hand.
fn exponent_bytes(shift: u32, minus: u64) -> [u8; 32] {
    let mut b = [0u8; 32];
    b[(shift / 8) as usize] = 1u8 << (shift % 8);
    let mut borrow = 0i64;
    for (i, byte) in b.iter_mut().enumerate() {
        let sub = if i < 8 { (minus >> (8 * i)) & 0xff } else { 0 } as i64;
        let cur = *byte as i64 - sub - borrow;
        if cur < 0 {
            *byte = (cur + 256) as u8;
            borrow = 1;
        } else {
            *byte = cur as u8;
            borrow = 0;
        }
    }
    b
}

/// d = -121665/121666 (the curve constant), computed once.
fn d_value() -> Fe {
    static D: std::sync::OnceLock<Fe> = std::sync::OnceLock::new();
    *D.get_or_init(|| {
        Fe::from_u64(121665)
            .neg()
            .mul(&Fe::from_u64(121666).invert())
    })
}

/// 2*d, used by the unified addition formula.
fn d2_value() -> Fe {
    static D2: std::sync::OnceLock<Fe> = std::sync::OnceLock::new();
    *D2.get_or_init(|| {
        let d = d_value();
        d.add(&d)
    })
}

/// sqrt(-1) = 2^((p-1)/4) mod p, used to fix up decompressed x coordinates.
fn sqrt_m1() -> Fe {
    static M1: std::sync::OnceLock<Fe> = std::sync::OnceLock::new();
    *M1.get_or_init(|| Fe::from_u64(2).pow(&exponent_bytes(253, 5)))
}

// ---------------------------------------------------------------- points

/// A point on edwards25519 (-x^2 + y^2 = 1 + d x^2 y^2) in extended
/// coordinates: x = X/Z, y = Y/Z, T = x*y.
#[derive(Clone, Copy)]
struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

impl Point {
    fn identity() -> Point {
        Point {
            x: Fe::ZERO,
            y: Fe::ONE,
            z: Fe::ONE,
            t: Fe::ZERO,
        }
    }

    /// RFC 8032 section 5.1.4 addition (strongly unified: also doubles).
    fn add(&self, o: &Point) -> Point {
        let a = self.y.sub(&self.x).mul(&o.y.sub(&o.x));
        let b = self.y.add(&self.x).mul(&o.y.add(&o.x));
        let c = self.t.mul(&d2_value()).mul(&o.t);
        let dd = self.z.mul(&o.z).add(&self.z.mul(&o.z)); // 2*Z1*Z2
        let e = b.sub(&a);
        let f = dd.sub(&c);
        let g = dd.add(&c);
        let h = b.add(&a);
        Point {
            x: e.mul(&f),
            y: g.mul(&h),
            z: f.mul(&g),
            t: e.mul(&h),
        }
    }

    fn negate(&self) -> Point {
        Point {
            x: self.x.neg(),
            y: self.y,
            z: self.z,
            t: self.t.neg(),
        }
    }

    /// Compress: y with the parity of x in the top bit.
    fn pack(&self) -> [u8; 32] {
        let zinv = self.z.invert();
        let x = self.x.mul(&zinv);
        let y = self.y.mul(&zinv);
        let mut out = y.to_bytes();
        out[31] |= (x.to_bytes()[0] & 1) << 7;
        out
    }
}

fn cswap_fe(swap: u64, a: &mut Fe, b: &mut Fe) {
    let mask = 0u64.wrapping_sub(swap & 1);
    for i in 0..5 {
        let t = mask & (a.0[i] ^ b.0[i]);
        a.0[i] ^= t;
        b.0[i] ^= t;
    }
}

/// Constant-time conditional swap of two points.
fn cswap(swap: u64, a: &mut Point, b: &mut Point) {
    cswap_fe(swap, &mut a.x, &mut b.x);
    cswap_fe(swap, &mut a.y, &mut b.y);
    cswap_fe(swap, &mut a.z, &mut b.z);
    cswap_fe(swap, &mut a.t, &mut b.t);
}

/// Double-and-add-always Montgomery-style ladder over extended coordinates:
/// maintains r0 = k*P and r1 = (k+1)*P through all 256 bit steps.
fn scalarmult(p: &Point, s: &[u8; 32]) -> Point {
    let mut r0 = Point::identity();
    let mut r1 = *p;
    for i in (0..256).rev() {
        let bit = ((s[i / 8] >> (i % 8)) & 1) as u64;
        cswap(bit, &mut r0, &mut r1);
        let t = r0.add(&r1);
        r0 = r0.add(&r0);
        r1 = t;
        cswap(bit, &mut r0, &mut r1);
    }
    r0
}

/// The standard base point, in compressed form (y = 4/5 mod p, x even).
/// Decoded with our own decompression code, so a bad constant there would
/// fail every test vector rather than silently corrupting signatures.
const BASE_COMPRESSED: [u8; 32] = [
    0x58, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
    0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
];

fn base_point() -> Point {
    static BASE: std::sync::OnceLock<Point> = std::sync::OnceLock::new();
    *BASE.get_or_init(|| match unpack(&BASE_COMPRESSED) {
        Some(p) => p,
        None => unreachable!("the hard-coded base point is on the curve"),
    })
}

fn scalarbase(s: &[u8; 32]) -> Point {
    scalarmult(&base_point(), s)
}

/// Decompress a point per RFC 8032 section 5.1.3. `None` when the encoding
/// is non-canonical or the point is not on the curve.
fn unpack(w: &[u8; 32]) -> Option<Point> {
    let mut ybytes = *w;
    let sign = ybytes[31] >> 7;
    ybytes[31] &= 0x7f;
    let y = Fe::from_bytes(&ybytes).canonical();
    // Reject non-canonical y encodings (y >= p re-encodes differently).
    if y.to_bytes() != ybytes {
        return None;
    }
    let y2 = y.square();
    let u = y2.sub(&Fe::ONE); // y^2 - 1
    let v = d_value().mul(&y2).add(&Fe::ONE); // d*y^2 + 1
    let v3 = v.square().mul(&v);
    let v7 = v3.square().mul(&v);
    let uv7 = u.mul(&v7);
    let mut x = u.mul(&v3).mul(&uv7.pow(&exponent_bytes(252, 3))); // (p-5)/8
    let check = v.mul(&x.square());
    if check.eq(&u) {
        // x is a square root of u/v.
    } else if check.eq(&u.neg()) {
        x = x.mul(&sqrt_m1());
    } else {
        return None; // not on the curve
    }
    if x.is_zero() && sign == 1 {
        return None;
    }
    if x.to_bytes()[0] & 1 != sign {
        x = x.neg();
    }
    Some(Point {
        x,
        y,
        z: Fe::ONE,
        t: x.mul(&y),
    })
}

// ---------------------------------------------------------------- scalars

/// The group order L = 2^252 + 27742317777372353535851937790883648493,
/// little-endian.
const L_LE: [u8; 32] = [
    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10,
];

fn le_to_limbs(b: &[u8]) -> [u64; 4] {
    let mut out = [0u64; 4];
    for (i, limb) in out.iter_mut().enumerate() {
        let lo = i * 8;
        if lo < b.len() {
            let hi = (lo + 8).min(b.len());
            let mut w = [0u8; 8];
            w[..hi - lo].copy_from_slice(&b[lo..hi]);
            *limb = u64::from_le_bytes(w);
        }
    }
    out
}

fn limbs_to_le32(l: &[u64; 4]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, limb) in l.iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&limb.to_le_bytes());
    }
    out
}

fn sub_borrow(a: &[u64; 4], b: &[u64; 4]) -> ([u64; 4], u64) {
    let mut out = [0u64; 4];
    let mut borrow = 0u64;
    for i in 0..4 {
        let (d1, b1) = a[i].overflowing_sub(b[i]);
        let (d2, b2) = d1.overflowing_sub(borrow);
        out[i] = d2;
        borrow = (b1 as u64) | (b2 as u64);
    }
    (out, borrow)
}

/// Reduce a little-endian integer of any length mod L, bit by bit from the
/// top: r = 2r + bit, with a masked conditional subtract when r >= L.
fn sc_reduce(bytes: &[u8]) -> [u8; 32] {
    let l = le_to_limbs(&L_LE);
    let mut r = [0u64; 4];
    for j in (0..bytes.len()).rev() {
        let byte = bytes[j];
        for k in (0..8).rev() {
            let bit = ((byte >> k) & 1) as u64;
            r[3] = (r[3] << 1) | (r[2] >> 63);
            r[2] = (r[2] << 1) | (r[1] >> 63);
            r[1] = (r[1] << 1) | (r[0] >> 63);
            r[0] = (r[0] << 1) | bit;
            let (d, borrow) = sub_borrow(&r, &l);
            let mask = 0u64.wrapping_sub(1 - borrow);
            for i in 0..4 {
                r[i] ^= mask & (r[i] ^ d[i]);
            }
        }
    }
    limbs_to_le32(&r)
}

/// Schoolbook 256x256-bit multiply, result as 64 little-endian bytes.
fn sc_mul_wide(a: &[u8; 32], b: &[u8; 32]) -> [u8; 64] {
    let a4 = le_to_limbs(a);
    let b4 = le_to_limbs(b);
    let mut out = [0u64; 8];
    for i in 0..4 {
        let mut carry = 0u128;
        for j in 0..4 {
            let t = a4[i] as u128 * b4[j] as u128 + out[i + j] as u128 + carry;
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
    let mut bytes = [0u8; 64];
    for (i, limb) in out.iter().enumerate() {
        bytes[i * 8..i * 8 + 8].copy_from_slice(&limb.to_le_bytes());
    }
    bytes
}

/// Add two reduced scalars mod L (both < L, so the sum < 2L fits).
fn sc_add(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut sum = [0u8; 32];
    let mut carry = 0u16;
    for i in 0..32 {
        let t = a[i] as u16 + b[i] as u16 + carry;
        sum[i] = t as u8;
        carry = t >> 8;
    }
    debug_assert_eq!(carry, 0); // 2L < 2^256
    sc_reduce(&sum)
}

fn clamp_scalar(h32: &[u8]) -> [u8; 32] {
    let mut a = [0u8; 32];
    a.copy_from_slice(&h32[..32]);
    a[0] &= 248;
    a[31] &= 127;
    a[31] |= 64;
    a
}

// ---------------------------------------------------------------- public API

/// Derive (public, secret) from a 32-byte seed per RFC 8032. The secret key
/// is `seed || public` (64 bytes).
pub fn keypair_from_seed(seed: &[u8; 32]) -> ([u8; 32], [u8; 64]) {
    let h = sha512_bytes(seed);
    let a = clamp_scalar(&h[..32]);
    let pk = scalarbase(&a).pack();
    let mut sk = [0u8; 64];
    sk[..32].copy_from_slice(seed);
    sk[32..].copy_from_slice(&pk);
    (pk, sk)
}

/// Sign `msg` with a 64-byte secret key (`seed || public`). The public key
/// half is re-derived from the seed, so a stale/corrupted pk half cannot
/// produce a signature that verifies against a different key.
pub fn sign(secret: &[u8; 64], msg: &[u8]) -> Result<[u8; 64], String> {
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&secret[..32]);
    let (pk, _) = keypair_from_seed(&seed);
    let h = sha512_bytes(&seed);
    let a = clamp_scalar(&h[..32]);
    let prefix = &h[32..64];
    let mut r_input = Vec::with_capacity(32 + msg.len());
    r_input.extend_from_slice(prefix);
    r_input.extend_from_slice(msg);
    // RFC 8032: r = SHA-512(prefix || PH(M)) mod L.
    let r = sc_reduce(&sha512_bytes(&r_input));
    let rb = scalarbase(&r).pack();
    let mut k_input = Vec::with_capacity(64 + msg.len());
    k_input.extend_from_slice(&rb);
    k_input.extend_from_slice(&pk);
    k_input.extend_from_slice(msg);
    // RFC 8032: k = SHA-512(R || A || PH(M)) mod L.
    let k = sc_reduce(&sha512_bytes(&k_input));
    let ka = sc_reduce(&sc_mul_wide(&k, &a));
    let s = sc_add(&r, &ka);
    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(&rb);
    sig[32..].copy_from_slice(&s);
    Ok(sig)
}

/// Verify a 64-byte signature over `msg` under `public`. Never panics:
/// malformed or non-canonical encodings and off-curve keys return `false`.
///
/// Rejected by construction (TweetNaCl `crypto_sign_open` structure plus
/// verify-strict hardening):
/// - a public key that does not decompress (non-canonical y, point not on
///   the curve, x = 0 with the sign bit set);
/// - a public key of small order: 8*A == identity rejects the order-1/2/4/8
///   torsion points, which enable small-subgroup forgery against the
///   non-cofactored check. This is stricter than TweetNaCl and RFC 8032;
/// - a non-canonical S: any S >= L fails the reduced-scalar identity check,
///   closing the (S, S+L) malleability class;
/// - any (R, S) with [S]B - [H(R||A||M)]A != R, so a modified R, a modified
///   S within range, or a modified message cannot verify.
///
/// R is never decompressed: it is only compared against the packed result,
/// so a non-canonical R encoding cannot smuggle a second valid point.
pub fn verify(public: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> bool {
    let a_point = match unpack(public) {
        Some(p) => p,
        None => return false,
    };
    // Small-subgroup hardening: reject public keys of order 1, 2, 4 or 8.
    let a2 = a_point.add(&a_point);
    let a4 = a2.add(&a2);
    let a8 = a4.add(&a4);
    if a8.x.is_zero() && a8.y.eq(&a8.z) {
        return false; // 8*A == identity (projective (0 : 1 : 1))
    }
    let mut rb = [0u8; 32];
    rb.copy_from_slice(&sig[..32]);
    let mut s = [0u8; 32];
    s.copy_from_slice(&sig[32..]);
    // S must be canonical: rejecting S >= L is the RFC 8032 requirement, and
    // stricter than TweetNaCl's top-bit check.
    if sc_reduce(&s) != s {
        return false;
    }
    let mut k_input = Vec::with_capacity(64 + msg.len());
    k_input.extend_from_slice(&rb);
    k_input.extend_from_slice(public);
    k_input.extend_from_slice(msg);
    let k = sc_reduce(&sha512_bytes(&k_input));
    // Check [S]B - [k]A == R (TweetNaCl's crypto_sign_open check).
    let p1 = scalarbase(&s);
    let p2 = scalarmult(&a_point.negate(), &k);
    let p = p1.add(&p2);
    // Constant-time-ish comparison of the packed point with R.
    let packed = p.pack();
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= packed[i] ^ rb[i];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::hex_decode;

    fn h(s: &str) -> Vec<u8> {
        hex_decode(s).expect("valid hex in tests")
    }

    #[test]
    fn field_roundtrips_bytes() {
        let mut b = [0u8; 32];
        for (i, byte) in b.iter_mut().enumerate() {
            *byte = (i * 37 + 11) as u8;
        }
        assert_eq!(Fe::from_bytes(&b).to_bytes()[..31], b[..31]);
        // Top bit is ignored by from_bytes (it is the sign bit of a point).
        let mut c = b;
        c[31] &= 0x7f;
        assert_eq!(Fe::from_bytes(&b).to_bytes()[..], c[..]);
    }

    #[test]
    fn field_arithmetic_is_consistent() {
        // (a + b) * c == a*c + b*c, and a - a == 0, over pseudo-random values.
        let mut vals = Vec::new();
        for i in 0..8 {
            let mut b = [0u8; 32];
            b[0] = i;
            b[7] = 0x9e;
            b[15] = 0x37;
            b[23] = 0x79;
            b[31] = 0x21 + i; // keep < p
            vals.push(Fe::from_bytes(&b).canonical());
        }
        for i in 0..4 {
            let (a, b, c) = (vals[2 * i], vals[2 * i + 1], vals[(i + 3) % 8]);
            let left = a.add(&b).mul(&c);
            let right = a.mul(&c).add(&b.mul(&c));
            assert!(left.eq(&right), "distributivity case {}", i);
            assert!(a.sub(&a).is_zero(), "a - a == 0 case {}", i);
            assert!(a.mul(&a.invert()).eq(&Fe::ONE), "inverse case {}", i);
        }
    }

    #[test]
    fn d_is_minus_121665_over_121666() {
        // 121666 * d == -121665 (mod p).
        let prod = d_value().mul(&Fe::from_u64(121666));
        assert!(prod.eq(&Fe::from_u64(121665).neg()));
    }

    #[test]
    fn sqrt_m1_squares_to_minus_one() {
        assert!(sqrt_m1().square().eq(&Fe::ONE.neg()));
    }

    #[test]
    fn base_point_roundtrips_and_is_on_curve() {
        let b = base_point();
        assert_eq!(b.pack(), BASE_COMPRESSED);
    }

    #[test]
    fn l_times_base_point_is_identity() {
        let p = scalarmult(&base_point(), &L_LE);
        let packed = p.pack();
        let mut identity = [0u8; 32];
        identity[0] = 1;
        assert_eq!(packed, identity, "[L]B must be the identity point");
    }

    #[test]
    fn unpack_rejects_non_canonical_and_off_curve() {
        // y = p (non-canonical: re-encodes to 0).
        let mut b = [0xffu8; 32];
        b[0] = 0xed;
        b[31] &= 0x7f;
        assert!(unpack(&b).is_none(), "y = p must be rejected");
        // y = 1 with sign bit set: x = 0, so a positive sign is invalid.
        let mut one = [0u8; 32];
        one[0] = 1;
        one[31] |= 0x80;
        assert!(
            unpack(&one).is_none(),
            "x = 0 with sign bit 1 must be rejected"
        );
    }

    /// RFC 8032 section 7.1 TEST 1: SHA(abc)-broken-key... standard vector.
    const TEST1_SEED: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
    const TEST1_PK: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    const TEST1_SIG: &str = "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b";

    /// RFC 8032 section 7.1 TEST 2 (message is the single byte 0x72).
    const TEST2_SEED: &str = "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb";
    const TEST2_PK: &str = "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c";
    const TEST2_SIG: &str = "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00";

    #[test]
    fn sc_reduce_basic_cases() {
        let mut three = [0u8; 32];
        three[0] = 3;
        let mut nine = [0u8; 32];
        nine[0] = 9;
        let mut one = [0u8; 32];
        one[0] = 1;
        // Zero and small values pass through.
        assert_eq!(sc_reduce(&[0u8; 32]), [0u8; 32]);
        assert_eq!(sc_reduce(&[0u8; 64]), [0u8; 32]);
        assert_eq!(sc_reduce(&three), three);
        // L reduces to zero; L + 3 reduces to 3.
        assert_eq!(sc_reduce(&L_LE), [0u8; 32]);
        let mut l_plus_3 = [0u8; 64];
        l_plus_3[..32].copy_from_slice(&L_LE);
        l_plus_3[0] += 3; // no carry: L's low byte 0xed + 3 = 0xf0
        assert_eq!(sc_reduce(&l_plus_3), three);
        // Wide products reduce correctly: (L-1)^2 == 1 (mod L), 3*3 == 9.
        let mut l_minus_1 = [0u8; 32];
        l_minus_1.copy_from_slice(&L_LE);
        l_minus_1[0] -= 1;
        assert_eq!(sc_reduce(&sc_mul_wide(&l_minus_1, &l_minus_1)), one);
        assert_eq!(sc_reduce(&sc_mul_wide(&three, &three)), nine);
    }

    #[test]
    fn rfc8032_test1_keypair_sign_verify() {
        let seed = h(TEST1_SEED);
        let mut seed_arr = [0u8; 32];
        seed_arr.copy_from_slice(&seed);
        let (pk, sk) = keypair_from_seed(&seed_arr);
        assert_eq!(crate::pkg::hex_encode(&pk), TEST1_PK);
        let sig = sign(&sk, b"").expect("sign");
        assert_eq!(crate::pkg::hex_encode(&sig), TEST1_SIG);
        assert!(verify(&pk, b"", &sig));
    }

    #[test]
    fn rfc8032_test2_sign_verify() {
        let seed = h(TEST2_SEED);
        let mut seed_arr = [0u8; 32];
        seed_arr.copy_from_slice(&seed);
        let (pk, sk) = keypair_from_seed(&seed_arr);
        assert_eq!(crate::pkg::hex_encode(&pk), TEST2_PK);
        let msg = h("72"); // one byte, 0x72
        let sig = sign(&sk, &msg).expect("sign");
        assert_eq!(crate::pkg::hex_encode(&sig), TEST2_SIG);
        assert!(verify(&pk, &msg, &sig));
        // The same signature must NOT verify under the TEST 1 public key.
        let pk1 = h(TEST1_PK);
        let mut pk1_arr = [0u8; 32];
        pk1_arr.copy_from_slice(&pk1);
        assert!(!verify(&pk1_arr, &msg, &sig));
    }

    #[test]
    fn verify_rejects_tampered_and_malformed_input() {
        let seed = h(TEST1_SEED);
        let mut seed_arr = [0u8; 32];
        seed_arr.copy_from_slice(&seed);
        let (pk, sk) = keypair_from_seed(&seed_arr);
        let sig = sign(&sk, b"hello spid3r").expect("sign");

        // Flipped bit in R half.
        let mut bad = sig;
        bad[0] ^= 1;
        assert!(!verify(&pk, b"hello spid3r", &bad));
        // Flipped bit in S half.
        let mut bad = sig;
        bad[63] ^= 1;
        assert!(!verify(&pk, b"hello spid3r", &bad));
        // Wrong message.
        assert!(!verify(&pk, b"hello spid3R", &sig));
        // Non-canonical S (S >= L): setting the top byte to 0xff.
        let mut bad = sig;
        bad[63] = 0xff;
        assert!(!verify(&pk, b"hello spid3r", &bad));
        // Off-curve public key.
        let mut bad_pk = pk;
        bad_pk[0] ^= 1;
        assert!(!verify(&bad_pk, b"hello spid3r", &sig));
        // All-zero public key is not on the curve.
        assert!(!verify(&[0u8; 32], b"hello spid3r", &sig));
    }

    #[test]
    fn matches_independent_reference_for_non_vector_message() {
        // Signature of b"x" under the TEST 1 seed, produced by an
        // independent implementation (Python port of the RFC 8032 reference
        // code, cross-checked against the RFC vectors above).
        const SIG_X: &str = "18d0b7f5338df220d1ec7c8e820a9f59afcb5dbd480d04842e5d3a8f139efd9b5\
                             ab4fe6ff283ab7667f4736530bf8ea25f189083283280c5acc05ac4a5fdc609";
        let seed = h(TEST1_SEED);
        let mut seed_arr = [0u8; 32];
        seed_arr.copy_from_slice(&seed);
        let (pk, sk) = keypair_from_seed(&seed_arr);
        let sig = sign(&sk, b"x").expect("sign");
        assert_eq!(crate::pkg::hex_encode(&sig), SIG_X);
        assert!(verify(&pk, b"x", &sig));
    }

    #[test]
    fn verify_rejects_malleable_s_values() {
        let seed = h(TEST1_SEED);
        let mut seed_arr = [0u8; 32];
        seed_arr.copy_from_slice(&seed);
        let (pk, sk) = keypair_from_seed(&seed_arr);
        let sig = sign(&sk, b"malleability").expect("sign");
        assert!(verify(&pk, b"malleability", &sig));

        // S + L: group-equivalent to S (L*B = identity) but non-canonical.
        // The strict S < L check must reject it.
        let mut s_plus_l = sig;
        let mut carry = 0u16;
        for i in 0..32 {
            let t = s_plus_l[32 + i] as u16 + L_LE[i] as u16 + carry;
            s_plus_l[32 + i] = t as u8;
            carry = t >> 8;
        }
        assert_eq!(carry, 0); // S + L < 2L < 2^256 fits in 32 bytes
        assert!(!verify(&pk, b"malleability", &s_plus_l));

        // S with the high bit set (>= 2^255 > L).
        let mut s_high = sig;
        s_high[63] |= 0x80;
        assert!(!verify(&pk, b"malleability", &s_high));

        // S tweaked within range changes the group element -> R mismatch.
        let mut s_tweak = sig;
        s_tweak[32] ^= 1;
        assert!(!verify(&pk, b"malleability", &s_tweak));
    }

    #[test]
    fn verify_rejects_small_order_public_keys() {
        // All four are valid curve points (they decode fine), so this
        // exercises the 8*A == identity rejection specifically. Encodings
        // derived with the independent Python reference implementation.
        const ORDER1: &str = "0100000000000000000000000000000000000000000000000000000000000000";
        const ORDER2: &str = "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f";
        const ORDER4: &str = "0000000000000000000000000000000000000000000000000000000000000080";
        const ORDER8: &str = "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a";

        let seed = h(TEST1_SEED);
        let mut seed_arr = [0u8; 32];
        seed_arr.copy_from_slice(&seed);
        let (pk, sk) = keypair_from_seed(&seed_arr);
        let sig = sign(&sk, b"small subgroup").expect("sign");
        assert!(verify(&pk, b"small subgroup", &sig));

        for enc in [ORDER1, ORDER2, ORDER4, ORDER8] {
            let bad_pk = h(enc);
            let mut pk_arr = [0u8; 32];
            pk_arr.copy_from_slice(&bad_pk);
            assert!(
                !verify(&pk_arr, b"small subgroup", &sig),
                "order-<8 public key {} must be rejected",
                enc
            );
        }
    }

    #[test]
    fn roundtrip_with_unicode_message() {
        let seed = h(TEST2_SEED);
        let mut seed_arr = [0u8; 32];
        seed_arr.copy_from_slice(&seed);
        let (pk, sk) = keypair_from_seed(&seed_arr);
        let msg = "签名 over UTF-8 bytes — é, 中, \u{1F40D}".as_bytes();
        let sig = sign(&sk, msg).expect("sign");
        assert!(verify(&pk, msg, &sig));
    }
}
