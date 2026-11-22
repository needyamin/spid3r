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

/// String key stored in `MapKey::Str`: either a permanently interned `'static`
/// key (zero atomic refcount traffic on clone/drop) or a compact 8-byte `ThinArcStr`.
pub enum KeyStr {
    Static(&'static str),
    Thin(ThinArcStr),
}

impl KeyStr {
    #[inline(always)]
    pub fn as_str(&self) -> &str {
        match self {
            KeyStr::Static(s) => s,
            KeyStr::Thin(t) => t.as_str(),
        }
    }

    #[inline]
    pub fn to_thin_arc_str(&self) -> ThinArcStr {
        match self {
            KeyStr::Static(s) => ThinArcStr::from(*s),
            KeyStr::Thin(t) => t.clone(),
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
            (KeyStr::Thin(a), KeyStr::Thin(b)) => a.ptr_eq(b) || a.as_str() == b.as_str(),
            _ => self.as_str() == other.as_str(),
        }
    }
}

impl Eq for KeyStr {}

impl PartialEq<str> for KeyStr {
    #[inline]
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for KeyStr {
    #[inline]
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
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
        KeyStr::Thin(ThinArcStr::from(s))
    }
}

impl From<String> for KeyStr {
    #[inline]
    fn from(s: String) -> Self {
        KeyStr::Thin(ThinArcStr::from(s.as_str()))
    }
}

impl From<Arc<str>> for KeyStr {
    #[inline]
    fn from(a: Arc<str>) -> Self {
        KeyStr::Thin(ThinArcStr::from(a.as_ref()))
    }
}

impl From<ThinArcStr> for KeyStr {
    #[inline]
    fn from(t: ThinArcStr) -> Self {
        KeyStr::Thin(t)
    }
}

impl From<&String> for KeyStr {
    #[inline]
    fn from(s: &String) -> Self {
        KeyStr::Thin(ThinArcStr::from(s.as_str()))
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

/// Look up or lock-free intern a short string key (`<= 64` bytes) into `GLOBAL_KEY_TABLE`
/// using precomputed `fx_hash_str(s)`. Returns `KeyStr::Arc` if longer than `INTERN_MAX_LEN`
/// or if the probe chain is saturated.
#[inline]
pub fn intern_key_with_hash(s: &str, h: u64) -> KeyStr {
    debug_assert_eq!(h, super::fx_hash_str(s));
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
}
