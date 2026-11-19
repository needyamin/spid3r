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

/// Fast FxHash of a string key, matching `fx_hash_map_key(&MapKey::Str(..))`.
#[inline]
pub fn fx_hash_str(s: &str) -> u64 {
    let mut h = FxHasher::default();
    h.write_u8(3);
    h.write(s.as_bytes());
    h.finish()
}

/// Fast FxHash of a `MapKey` without going through `std::hash::Hash`.
#[inline]
pub fn fx_hash_map_key(k: &crate::value::MapKey) -> u64 {
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
        crate::value::MapKey::Str(s) => {
            h.write_u8(3);
            h.write(s.as_bytes());
        }
    }
    h.finish()
}
