//! Lock-free global key interning table and `KeyStr` for pointer-equality map lookups.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use crate::value::ThinArcStr;

const INTERN_SLOTS: usize = 2048;
const INTERN_MAX_LEN: usize = 64;
const INTERN_MAX_PROBES: usize = 8;

const INLINE_KEY_MAX: usize = 15;

/// String key stored in `MapKey::Str`: either a small inline byte array
/// (`<= 15` bytes, zero allocation and zero atomic refcounting), a permanently
/// interned `'static` key (`16..=64` bytes), or a compact 8-byte `ThinArcStr`.
pub enum KeyStr {
    Static(&'static str),
    Thin(ThinArcStr),
    Inline { len: u8, buf: [u8; INLINE_KEY_MAX] },
}

/// Compare short slices inline without calling `memcmp` for `<= 16`-byte keys.
#[cfg_attr(not(debug_assertions), inline(always))]
#[cfg_attr(debug_assertions, inline(never))]
pub fn bytes_eq_short(a: &[u8], b: &[u8]) -> bool {
    let n = a.len();
    if n != b.len() {
        return false;
    }
    if n <= 8 {
        if n >= 4 {
            return a[..4] == b[..4] && a[n - 4..] == b[n - 4..];
        }
        if n >= 2 {
            return a[0] == b[0] && a[1] == b[1] && a[n - 1] == b[n - 1];
        }
        return n == 0 || a[0] == b[0];
    }
    if n <= 16 {
        return a[..8] == b[..8] && a[n - 8..] == b[n - 8..];
    }
    a == b
}

impl KeyStr {
    #[inline]
    fn try_inline(s: &str) -> Option<Self> {
        let len = s.len();
        if len <= INLINE_KEY_MAX {
            let mut buf = [0u8; INLINE_KEY_MAX];
            buf[..len].copy_from_slice(s.as_bytes());
            return Some(KeyStr::Inline {
                len: len as u8,
                buf,
            });
        }
        None
    }

    #[inline(always)]
    pub fn from_thin_str_ref(t: &ThinArcStr) -> Self {
        if let Some(inl) = Self::try_inline(t.as_str()) {
            return inl;
        }
        KeyStr::Thin(t.clone())
    }

    #[inline(always)]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            KeyStr::Static(s) => s.as_bytes(),
            KeyStr::Thin(t) => t.as_bytes(),
            KeyStr::Inline { len, buf } => &buf[..(*len as usize).min(INLINE_KEY_MAX)],
        }
    }

    #[inline(always)]
    pub fn eq_bytes(&self, s: &[u8]) -> bool {
        match self {
            KeyStr::Inline { len, buf } => {
                let n = *len as usize;
                n <= INLINE_KEY_MAX && bytes_eq_short(&buf[..n], s)
            }
            KeyStr::Static(k) => bytes_eq_short(k.as_bytes(), s),
            KeyStr::Thin(t) => bytes_eq_short(t.as_bytes(), s),
        }
    }

    #[inline(always)]
    pub fn as_str(&self) -> &str {
        match self {
            KeyStr::Static(s) => s,
            KeyStr::Thin(t) => t.as_str(),
            KeyStr::Inline { len, buf } => {
                let n = (*len as usize).min(INLINE_KEY_MAX);
                std::str::from_utf8(&buf[..n]).unwrap_or("")
            }
        }
    }

    #[inline]
    pub fn to_thin_arc_str(&self) -> ThinArcStr {
        match self {
            KeyStr::Static(s) => ThinArcStr::from(*s),
            KeyStr::Thin(t) => t.clone(),
            KeyStr::Inline { .. } => ThinArcStr::from(self.as_str()),
        }
    }
}

impl Deref for KeyStr {
    type Target = str;

    #[inline(always)]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for KeyStr {
    #[inline(always)]
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Clone for KeyStr {
    #[inline(always)]
    fn clone(&self) -> Self {
        match self {
            KeyStr::Static(s) => KeyStr::Static(s),
            KeyStr::Thin(t) => KeyStr::Thin(t.clone()),
            KeyStr::Inline { len, buf } => KeyStr::Inline {
                len: *len,
                buf: *buf,
            },
        }
    }
}

impl PartialEq for KeyStr {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (KeyStr::Static(a), KeyStr::Static(b)) => {
                (std::ptr::eq(a.as_ptr(), b.as_ptr()) && a.len() == b.len()) || *a == *b
            }
            (KeyStr::Thin(a), KeyStr::Thin(b)) => a.ptr_eq(b) || a.as_bytes() == b.as_bytes(),
            _ => self.as_bytes() == other.as_bytes(),
        }
    }
}

impl Eq for KeyStr {}

impl PartialEq<str> for KeyStr {
    #[inline(always)]
    fn eq(&self, other: &str) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl PartialEq<&str> for KeyStr {
    #[inline(always)]
    fn eq(&self, other: &&str) -> bool {
        self.as_bytes() == other.as_bytes()
    }
}

impl Hash for KeyStr {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl fmt::Debug for KeyStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for KeyStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

impl From<&str> for KeyStr {
    #[inline]
    fn from(s: &str) -> Self {
        Self::try_inline(s).unwrap_or_else(|| KeyStr::Thin(ThinArcStr::from(s)))
    }
}

impl From<String> for KeyStr {
    #[inline]
    fn from(s: String) -> Self {
        Self::try_inline(s.as_str()).unwrap_or_else(|| KeyStr::Thin(ThinArcStr::from(s.as_str())))
    }
}

impl From<Arc<str>> for KeyStr {
    #[inline]
    fn from(a: Arc<str>) -> Self {
        Self::try_inline(a.as_ref()).unwrap_or_else(|| KeyStr::Thin(ThinArcStr::from(a.as_ref())))
    }
}

impl From<ThinArcStr> for KeyStr {
    #[inline]
    fn from(t: ThinArcStr) -> Self {
        Self::try_inline(t.as_str()).unwrap_or(KeyStr::Thin(t))
    }
}

impl From<&String> for KeyStr {
    #[inline]
    fn from(s: &String) -> Self {
        Self::try_inline(s.as_str()).unwrap_or_else(|| KeyStr::Thin(ThinArcStr::from(s.as_str())))
    }
}

struct InternSlot {
    hash: AtomicU64,
    key: OnceLock<Box<str>>,
}

impl InternSlot {
    const fn new() -> Self {
        Self {
            hash: AtomicU64::new(0),
            key: OnceLock::new(),
        }
    }
}

static GLOBAL_KEY_TABLE: [InternSlot; INTERN_SLOTS] = [const { InternSlot::new() }; INTERN_SLOTS];

#[inline(always)]
fn nz_hash(h: u64) -> u64 {
    if h == 0 {
        1
    } else {
        h
    }
}

/// Look up or lock-free intern a short key: `<= 15` bytes return `KeyStr::Inline`
/// without heap allocation or atomics; `16..=64` bytes intern into `GLOBAL_KEY_TABLE`
/// using precomputed `fx_hash_str(s)`; longer or saturated keys return `KeyStr::Thin`.
#[inline]
pub fn intern_key_with_hash(s: &str, h: u64) -> KeyStr {
    debug_assert_eq!(h, super::fx_hash_str(s));
    if let Some(inl) = KeyStr::try_inline(s) {
        return inl;
    }
    if s.len() > INTERN_MAX_LEN {
        return KeyStr::Thin(ThinArcStr::from(s));
    }
    let nz = nz_hash(h);
    let mask = INTERN_SLOTS - 1;
    let start = (h as usize) & mask;
    for probe in 0..INTERN_MAX_PROBES {
        let slot = &GLOBAL_KEY_TABLE[(start + probe) & mask];
        let sh = slot.hash.load(Ordering::Acquire);
        if sh == nz {
            if let Some(actual) = slot.key.get() {
                if actual.as_ref() == s {
                    return KeyStr::Static(actual.as_ref());
                }
            }
            continue;
        }
        if sh == 0 {
            let actual: &'static str = slot.key.get_or_init(|| Box::<str>::from(s)).as_ref();
            let actual_nz = nz_hash(super::fx_hash_str(actual));
            slot.hash.store(actual_nz, Ordering::Release);
            if actual == s {
                return KeyStr::Static(actual);
            }
        }
    }
    KeyStr::Thin(ThinArcStr::from(s))
}

/// Intern a key string using `fx_hash_str`.
#[inline]
pub fn intern_key(s: &str) -> KeyStr {
    intern_key_with_hash(s, super::fx_hash_str(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;

    #[test]
    fn intern_returns_identical_static_pointers_and_matches_arc() {
        let k1 = intern_key("intern_test_field_alpha");
        let k2 = intern_key("intern_test_field_alpha");
        let k3 = KeyStr::from("intern_test_field_alpha");
        assert!(matches!(k1, KeyStr::Static(_)));
        assert!(matches!(k2, KeyStr::Static(_)));
        assert_eq!(k1, k2);
        assert_eq!(k1, k3);
        assert_eq!(k3, k1);
        if let (KeyStr::Static(s1), KeyStr::Static(s2)) = (&k1, &k2) {
            assert!(std::ptr::eq(s1.as_ptr(), s2.as_ptr()));
        }

        let mut h1 = DefaultHasher::new();
        let mut h3 = DefaultHasher::new();
        k1.hash(&mut h1);
        k3.hash(&mut h3);
        assert_eq!(h1.finish(), h3.finish());
    }

    #[test]
    fn long_key_bypasses_global_table() {
        let long = "x".repeat(INTERN_MAX_LEN + 1);
        let k = intern_key(&long);
        assert!(matches!(k, KeyStr::Thin(_)));
        assert_eq!(k.as_str(), long);
    }

    #[test]
    fn multithreaded_intern_is_consistent() {
        let handles: Vec<_> = (0..6)
            .map(|tid| {
                std::thread::spawn(move || {
                    for i in 0..50 {
                        let name = format!("mt_field_{}", (tid * 7 + i) % 24);
                        let a = intern_key(&name);
                        let b = intern_key(&name);
                        assert_eq!(a, b);
                        assert_eq!(a.as_str(), name);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn inline_key_str_matches_static_and_thin_without_size_growth() {
        assert!(std::mem::size_of::<KeyStr>() <= 24);
        let k_empty = intern_key("");
        let k_short = intern_key("price");
        let k_15 = intern_key("exactly_15bytes");
        let k_16 = intern_key("exactly_16_bytes");
        assert!(matches!(k_empty, KeyStr::Inline { .. }));
        assert!(matches!(k_short, KeyStr::Inline { .. }));
        assert!(matches!(k_15, KeyStr::Inline { .. }));
        assert!(matches!(k_16, KeyStr::Static(_)));

        let t_short = ThinArcStr::from("price");
        let k_thin = KeyStr::Thin(t_short.clone());
        let k_from_ref = KeyStr::from_thin_str_ref(&t_short);
        assert!(matches!(k_from_ref, KeyStr::Inline { .. }));
        assert_eq!(k_short, k_thin);
        assert_eq!(k_thin, k_short);
        assert_eq!(k_short.as_bytes(), b"price");
        assert_eq!(k_short, "price");

        let mut h1 = DefaultHasher::new();
        let mut h2 = DefaultHasher::new();
        k_short.hash(&mut h1);
        k_thin.hash(&mut h2);
        assert_eq!(h1.finish(), h2.finish());
    }

    #[test]
    fn bytes_eq_short_covers_lengths_and_single_byte_flips() {
        for len in 0..=20 {
            let a: Vec<u8> = (0..len).map(|i| b'a' + (i as u8 % 23)).collect();
            let b = a.clone();
            assert!(bytes_eq_short(&a, &b));
            if len > 0 {
                let mut shorter = a.clone();
                shorter.pop();
                assert!(!bytes_eq_short(&a, &shorter));
            }
            for pos in 0..len {
                let mut flipped = a.clone();
                flipped[pos] ^= 1;
                assert!(!bytes_eq_short(&a, &flipped));
            }
        }
    }
}
