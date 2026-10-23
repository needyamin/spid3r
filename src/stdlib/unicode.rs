//! Unicode normalization (UAX #15) and full case folding.
//!
//! The tables live in `unicode_tables.rs`, generated from the UCD by
//! `tools/gen_unicode_tables.py`. This module is the algorithm:
//!
//!   1. **Decompose** every character recursively (canonical, or compatibility
//!      for the K forms). Hangul syllables are decomposed arithmetically -
//!      they have no table entries (UAX #15 section 3.12).
//!   2. **Canonical order**: stable-sort each run of non-starters by combining
//!      class.
//!   3. **Compose** (NFC/NFKC only): walk the buffer, trying to combine a
//!      starter with the following character, honouring the blocking rule
//!      (a character is blocked from the starter if a character of equal or
//!      higher combining class intervenes).
//!
//! The composition table only contains pairs that the reference
//! implementation actually composes, so Full_Composition_Exclusion is honoured
//! by construction rather than by a second list.
//!
//! Everything here is a pure function of the tables; no global state, no
//! locks, and it runs identically on every engine because it is a native.

use super::unicode_tables as t;
use super::NativeDef;
use crate::value::{type_name, NativeCtx, Value};

// ------------------------------------------------------------ UAX #15 constants

const S_BASE: u32 = 0xAC00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT; // 588
const S_COUNT: u32 = L_COUNT * N_COUNT; // 11172

pub fn native_defs() -> Vec<NativeDef> {
    vec![
        ("unicode_normalize", n_normalize),
        ("unicode_is_normalized", n_is_normalized),
        ("unicode_casefold", n_casefold),
        ("unicode_strip_marks", n_strip_marks),
        ("unicode_combining_class", n_combining_class),
        ("unicode_version", n_version),
    ]
}

// ------------------------------------------------------------ table lookups

fn lookup_decomp(cp: u32) -> Option<(bool, &'static [u32])> {
    match t::DECOMP_CP.binary_search(&cp) {
        Ok(i) => {
            let off = t::DECOMP_OFF[i] as usize;
            let len = t::DECOMP_LEN[i] as usize;
            Some((t::DECOMP_FLAGS[i] != 0, &t::DECOMP_SEQ[off..off + len]))
        }
        Err(_) => None,
    }
}

/// Canonical combining class; 0 for starters (and for unassigned characters).
fn combining_class(cp: u32) -> u8 {
    match t::CCC_CP.binary_search(&cp) {
        Ok(i) => t::CCC_VAL[i],
        Err(_) => 0,
    }
}

fn lookup_fold(cp: u32) -> Option<&'static [u32]> {
    match t::FOLD_CP.binary_search(&cp) {
        Ok(i) => {
            let off = t::FOLD_OFF[i] as usize;
            let len = t::FOLD_LEN[i] as usize;
            Some(&t::FOLD_SEQ[off..off + len])
        }
        Err(_) => None,
    }
}

/// Primary composite of `a` + `b`, if one exists.
fn compose_pair(a: u32, b: u32) -> Option<u32> {
    // Hangul LV and LVT are composed arithmetically; they are not in the table.
    if (L_BASE..L_BASE + L_COUNT).contains(&a) && (V_BASE..V_BASE + V_COUNT).contains(&b) {
        return Some(S_BASE + (a - L_BASE) * N_COUNT + (b - V_BASE) * T_COUNT);
    }
    if (S_BASE..S_BASE + S_COUNT).contains(&a)
        && (a - S_BASE).is_multiple_of(T_COUNT)
        && (T_BASE + 1..T_BASE + T_COUNT).contains(&b)
    {
        return Some(a + (b - T_BASE));
    }
    let key = ((a as u64) << 32) | b as u64;
    let mut lo = 0usize;
    let mut hi = t::COMP_A.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        let k = ((t::COMP_A[mid] as u64) << 32) | t::COMP_B[mid] as u64;
        if k < key {
            lo = mid + 1;
        } else if k > key {
            hi = mid;
        } else {
            return Some(t::COMP_C[mid]);
        }
    }
    None
}

// ------------------------------------------------------------ steps

fn decompose_into(cp: u32, compat: bool, out: &mut Vec<u32>) {
    if let Some((is_compat, seq)) = lookup_decomp(cp) {
        if compat || !is_compat {
            for &p in seq {
                decompose_into(p, compat, out);
            }
            return;
        }
    }
    if (S_BASE..S_BASE + S_COUNT).contains(&cp) {
        let s = cp - S_BASE;
        out.push(L_BASE + s / N_COUNT);
        out.push(V_BASE + (s % N_COUNT) / T_COUNT);
        let t_index = s % T_COUNT;
        if t_index != 0 {
            out.push(T_BASE + t_index);
        }
        return;
    }
    out.push(cp);
}

/// Stable sort of every non-starter run by combining class.
///
/// Insertion sort is the right tool: the runs are short (a few marks), and it
/// is stable, which the algorithm requires - two marks with the same class
/// must keep their relative order.
fn canonical_order(buf: &mut [u32]) {
    for i in 1..buf.len() {
        let cc = combining_class(buf[i]);
        if cc == 0 {
            continue;
        }
        let mut j = i;
        while j > 0 {
            let prev = combining_class(buf[j - 1]);
            if prev == 0 || prev <= cc {
                break;
            }
            buf.swap(j - 1, j);
            j -= 1;
        }
    }
}

fn compose(buf: &[u32]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::with_capacity(buf.len());
    let mut starter: Option<usize> = None;
    let mut last_cc: u8 = 0;
    for &cp in buf {
        let cc = combining_class(cp);
        if let Some(si) = starter {
            // Blocking rule: a character may combine with the starter only if
            // nothing of equal-or-higher class intervenes. `last_cc == 0` with
            // the starter immediately before covers starter+starter pairs
            // (Hangul LV + T, and any two starters that form a composite).
            let adjacent = si + 1 == out.len();
            if last_cc < cc || (last_cc == 0 && adjacent) {
                if let Some(composed) = compose_pair(out[si], cp) {
                    out[si] = composed;
                    continue;
                }
            }
        }
        if cc == 0 {
            starter = Some(out.len());
        }
        last_cc = cc;
        out.push(cp);
    }
    out
}

// ------------------------------------------------------------ forms

#[derive(Clone, Copy, PartialEq, Eq)]
enum Form {
    Nfc,
    Nfd,
    Nfkc,
    Nfkd,
}

impl Form {
    fn parse(name: &str) -> Result<Form, String> {
        match name.trim().to_ascii_uppercase().as_str() {
            "NFC" => Ok(Form::Nfc),
            "NFD" => Ok(Form::Nfd),
            "NFKC" => Ok(Form::Nfkc),
            "NFKD" => Ok(Form::Nfkd),
            other => Err(format!(
                "unknown normalization form '{}' (expected NFC, NFD, NFKC or NFKD)",
                other
            )),
        }
    }

    fn compat(self) -> bool {
        matches!(self, Form::Nfkc | Form::Nfkd)
    }

    fn composes(self) -> bool {
        matches!(self, Form::Nfc | Form::Nfkc)
    }
}

fn normalize(text: &str, form: Form) -> String {
    let mut buf: Vec<u32> = Vec::with_capacity(text.chars().count());
    for ch in text.chars() {
        decompose_into(ch as u32, form.compat(), &mut buf);
    }
    canonical_order(&mut buf);
    let result = if form.composes() { compose(&buf) } else { buf };
    result
        .into_iter()
        .filter_map(char::from_u32)
        .collect::<String>()
}

fn casefold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match lookup_fold(ch as u32) {
            Some(seq) => {
                for &p in seq {
                    if let Some(c) = char::from_u32(p) {
                        out.push(c);
                    }
                }
            }
            None => out.push(ch),
        }
    }
    out
}

// ------------------------------------------------------------ natives

fn arity(who: &str, args: &[Value], n: usize) -> Result<(), String> {
    if args.len() == n {
        Ok(())
    } else {
        Err(format!(
            "{} expects {} argument{}, got {}",
            who,
            n,
            if n == 1 { "" } else { "s" },
            args.len()
        ))
    }
}

fn want_str(v: &Value, who: &str) -> Result<String, String> {
    match v {
        Value::Str(s) => Ok(s.to_string()),
        other => Err(format!(
            "{} expects a string, found {}",
            who,
            type_name(other)
        )),
    }
}

fn str_val(s: impl Into<String>) -> Value {
    Value::Str(s.into().into())
}

/// `unicode_normalize(text, form)` -> normalized text.
fn n_normalize(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("unicode_normalize", &args, 2)?;
    let text = want_str(&args[0], "unicode_normalize")?;
    let form = Form::parse(&want_str(&args[1], "unicode_normalize")?)?;
    Ok(str_val(normalize(&text, form)))
}

/// `unicode_is_normalized(text, form)` -> bool.
fn n_is_normalized(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("unicode_is_normalized", &args, 2)?;
    let text = want_str(&args[0], "unicode_is_normalized")?;
    let form = Form::parse(&want_str(&args[1], "unicode_is_normalized")?)?;
    Ok(Value::Bool(normalize(&text, form) == text))
}

/// `unicode_casefold(text)` -> full case folding (UCD `CaseFolding.txt`, C+F).
fn n_casefold(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("unicode_casefold", &args, 1)?;
    Ok(str_val(casefold(&want_str(&args[0], "unicode_casefold")?)))
}

/// `unicode_strip_marks(text)` -> NFD with every combining mark removed.
///
/// The "search folding" primitive: it is what makes a query for "cafe" match
/// "café" without throwing away the distinction between different base
/// letters (which is what a naive ASCII transliteration does).
fn n_strip_marks(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("unicode_strip_marks", &args, 1)?;
    let text = want_str(&args[0], "unicode_strip_marks")?;
    let mut buf: Vec<u32> = Vec::with_capacity(text.chars().count());
    for ch in text.chars() {
        decompose_into(ch as u32, false, &mut buf);
    }
    let out: String = buf
        .into_iter()
        .filter(|&cp| combining_class(cp) == 0)
        .filter_map(char::from_u32)
        .collect();
    Ok(str_val(out))
}

/// `unicode_combining_class(ch)` -> canonical combining class of one character.
fn n_combining_class(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("unicode_combining_class", &args, 1)?;
    let text = want_str(&args[0], "unicode_combining_class")?;
    let mut chars = text.chars();
    let ch = match (chars.next(), chars.next()) {
        (Some(c), None) => c,
        _ => {
            return Err(format!(
                "unicode_combining_class expects exactly one character, got {}",
                text.chars().count()
            ))
        }
    };
    Ok(Value::Int(combining_class(ch as u32) as i64))
}

/// `unicode_version()` -> the UCD version the tables were generated from.
fn n_version(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    arity("unicode_version", &args, 0)?;
    Ok(str_val(t::UNICODE_VERSION))
}
