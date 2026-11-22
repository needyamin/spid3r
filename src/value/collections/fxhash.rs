//! Fast non-cryptographic FxHash-style hasher for internal collection indices.
//!
//! Only used for unobservable internal lookup tables (`MapData.index`,
//! `MapData.frozen`, `SetData.index`). User-observable `hash_table()` keeps
//! `DefaultHasher` so bucket order is unchanged.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

const K: u64 = 0x517c_c1b7_2722_0a95;

/// Fast non-cryptographic 64-bit hasher.
#[derive(Default)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn mix(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(K);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }

    #[inline]
    fn write(&mut self, mut bytes: &[u8]) {
        while bytes.len() >= 8 {
            let w = u64::from_ne_bytes(bytes[..8].try_into().unwrap());
            self.mix(w);
            bytes = &bytes[8..];
        }
        if bytes.len() >= 4 {
            let w = u32::from_ne_bytes(bytes[..4].try_into().unwrap()) as u64;
            self.mix(w);
            bytes = &bytes[4..];
        }
        if bytes.len() >= 2 {
            let w = u16::from_ne_bytes(bytes[..2].try_into().unwrap()) as u64;
            self.mix(w);
            bytes = &bytes[2..];
        }
        if !bytes.is_empty() {
            self.mix(bytes[0] as u64);
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.mix(i as u64);
    }

    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.mix(i as u64);
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.mix(i as u64);
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.mix(i);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.mix(i as u64);
    }
}

pub type FxHashMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;
pub type FxHashSet<T> = HashSet<T, BuildHasherDefault<FxHasher>>;

/// Fast FxHash of raw key bytes, matching `fx_hash_map_key(&MapKey::Str(..))`.
/// Uses a single-branch unaligned word fold for `<= 8`-byte keys (`"k0"`..`"price"`).
#[inline]
pub fn fx_hash_bytes(bytes: &[u8]) -> u64 {
    let len = bytes.len();
    let mut hash = 3u64.wrapping_mul(K);
    if len <= 8 {
        let word = match len {
            0 => 0u64,
            1 => (1u64 << 56) | (bytes[0] as u64),
            2 => (2u64 << 56) | (u16::from_le_bytes([bytes[0], bytes[1]]) as u64),
            3 => {
                (3u64 << 56)
                    | (u16::from_le_bytes([bytes[0], bytes[1]]) as u64)
                    | ((bytes[2] as u64) << 16)
            }
            _ => {
                ((len as u64) << 56)
                    | (u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64)
                    | ((u32::from_le_bytes([
                        bytes[len - 4],
                        bytes[len - 3],
                        bytes[len - 2],
                        bytes[len - 1],
                    ]) as u64)
                        << 24)
            }
        };
        return (hash.rotate_left(5) ^ word).wrapping_mul(K);
    }
    let mut rest = bytes;
    while rest.len() >= 8 {
        let w = u64::from_ne_bytes(rest[..8].try_into().unwrap());
        hash = (hash.rotate_left(5) ^ w).wrapping_mul(K);
        rest = &rest[8..];
    }
    if rest.len() >= 4 {
        let w = u32::from_ne_bytes(rest[..4].try_into().unwrap()) as u64;
        hash = (hash.rotate_left(5) ^ w).wrapping_mul(K);
        rest = &rest[4..];
    }
    if rest.len() >= 2 {
        let w = u16::from_ne_bytes(rest[..2].try_into().unwrap()) as u64;
        hash = (hash.rotate_left(5) ^ w).wrapping_mul(K);
        rest = &rest[2..];
    }
    if !rest.is_empty() {
        hash = (hash.rotate_left(5) ^ (rest[0] as u64)).wrapping_mul(K);
    }
    hash
}

/// Fast FxHash of a string key, matching `fx_hash_map_key(&MapKey::Str(..))`.
#[inline]
pub fn fx_hash_str(s: &str) -> u64 {
    fx_hash_bytes(s.as_bytes())
}

/// Fast FxHash of a `MapKey` without going through `std::hash::Hash`.
#[inline]
pub fn fx_hash_map_key(k: &crate::value::MapKey) -> u64 {
    if let crate::value::MapKey::Str(s) = k {
        return fx_hash_str(s.as_str());
    }
    let mut h = FxHasher::default();
    match k {
        crate::value::MapKey::Null => h.write_u8(0),
        crate::value::MapKey::Bool(b) => {
            h.write_u8(1);
            h.write_u8(*b as u8);
        }
        crate::value::MapKey::Int(i) => {
            h.write_u8(2);
            h.write_u64(*i as u64);
        }
        crate::value::MapKey::Str(_) => unreachable!(),
    }
    h.finish()
}
