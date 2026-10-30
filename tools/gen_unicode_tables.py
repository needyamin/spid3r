#!/usr/bin/env python3
"""Generate src/stdlib/unicode_tables.rs from CPython's unicodedata (the UCD).

    python tools/gen_unicode_tables.py

Why CPython and not the UCD text files: `unicodedata` *is* the UCD, already
parsed, and the same interpreter is the oracle the differential tests compare
against (tests/unicode_tz_tests.rs). Generating from the same source we verify
against would be circular if the tables were derived from the oracle's
*behaviour*; they are not - they are derived from its data accessors
(`decomposition`, `combining`) and then checked against its algorithm
(`normalize`), which is exactly the old-vs-new split the project uses
elsewhere.

Tables emitted (all sorted by code point, surrogates excluded because Spid3r
strings cannot contain them):

  DECOMP_*   single-step decompositions, canonical and compatibility
  CCC_*      canonical combining classes (only non-zero entries)
  COMP_*     primary composites: (a, b) -> c, only pairs that CPython's NFC
             actually composes (which is how composition exclusions are
             honoured without a separate exclusion list)
  FOLD_*     full case folding (can be one-to-many: U+00DF -> "ss")

Hangul syllables are deliberately absent: their decomposition and composition
are algorithmic (UAX #15 section 3.12) and live in src/stdlib/unicode.rs.
"""

import sys
import unicodedata

MAX_CP = 0x110000
SURROGATES = range(0xD800, 0xE000)


def is_surrogate(cp):
    return cp in SURROGATES


def collect_decompositions():
    """(cp, compatibility?, [parts]) for every character with a decomposition."""
    out = []
    for cp in range(MAX_CP):
        if is_surrogate(cp):
            continue
        d = unicodedata.decomposition(chr(cp))
        if not d:
            continue
        parts = d.split()
        compat = parts[0].startswith("<")
        if compat:
            parts = parts[1:]
        out.append((cp, compat, [int(p, 16) for p in parts]))
    return out


def collect_ccc():
    out = []
    for cp in range(MAX_CP):
        if is_surrogate(cp):
            continue
        cc = unicodedata.combining(chr(cp))
        if cc:
            out.append((cp, cc))
    return out


def collect_compositions(decomps):
    """Primary composites, verified against the reference implementation.

    A character is a primary composite when its *canonical* decomposition is
    exactly two characters and it is not a full composition exclusion. Rather
    than re-deriving Full_Composition_Exclusion, each candidate is checked with
    unicodedata.normalize: if NFC(a + b) is not c, the pair is excluded. That
    is the definition, applied by the implementation that defines it.
    """
    out = []
    seen = set()
    for cp, compat, parts in decomps:
        if compat or len(parts) != 2:
            continue
        a, b = parts
        if unicodedata.normalize("NFC", chr(a) + chr(b)) != chr(cp):
            continue
        if (a, b) in seen:
            raise SystemExit(f"duplicate composition pair {a:x}+{b:x}")
        seen.add((a, b))
        out.append((a, b, cp))
    out.sort()
    return out


def collect_casefold():
    out = []
    for cp in range(MAX_CP):
        if is_surrogate(cp):
            continue
        folded = chr(cp).casefold()
        if folded != chr(cp):
            out.append((cp, [ord(c) for c in folded]))
    return out


def emit_array(f, name, ty, values, per_line=16, fmt=str):
    # rustfmt must not reflow generated data: the array is one value per
    # source token, and reformatting it on every regeneration makes noisy
    # diffs for no benefit.
    f.write(f"#[rustfmt::skip]\npub static {name}: &[{ty}] = &[\n")
    for i in range(0, len(values), per_line):
        chunk = values[i : i + per_line]
        f.write("    " + ", ".join(fmt(v) for v in chunk) + ",\n")
    f.write("];\n\n")


def main():
    dest = sys.argv[1] if len(sys.argv) > 1 else "src/stdlib/unicode_tables.rs"

    decomps = collect_decompositions()
    ccc = collect_ccc()
    comps = collect_compositions(decomps)
    folds = collect_casefold()

    # Flatten the decomposition sequences into one pool with offsets.
    decomp_cp, decomp_flags, decomp_off, decomp_len, decomp_seq = [], [], [], [], []
    for cp, compat, parts in decomps:
        decomp_cp.append(cp)
        decomp_flags.append(1 if compat else 0)
        decomp_off.append(len(decomp_seq))
        decomp_len.append(len(parts))
        decomp_seq.extend(parts)

    fold_cp, fold_off, fold_len, fold_seq = [], [], [], []
    for cp, parts in folds:
        fold_cp.append(cp)
        fold_off.append(len(fold_seq))
        fold_len.append(len(parts))
        fold_seq.extend(parts)

    with open(dest, "w", encoding="utf-8", newline="\n") as f:
        f.write(
            "//! GENERATED FILE - do not edit.\n"
            "//!\n"
            "//! Unicode tables for normalization and case folding, produced by\n"
            "//! `tools/gen_unicode_tables.py` from CPython's `unicodedata`.\n"
            "//!\n"
            f"//! Unicode version: {unicodedata.unidata_version}\n"
            "//!\n"
            "//! Regenerate after a UCD bump; the differential tests in\n"
            "//! tests/unicode_tz_tests.rs compare these tables against CPython, so a\n"
            "//! stale table fails the build rather than silently mis-normalizing.\n"
            "\n"
            "#![allow(clippy::unreadable_literal)]\n\n"
        )
        f.write(
            f'pub const UNICODE_VERSION: &str = "{unicodedata.unidata_version}";\n\n'
        )
        emit_array(f, "DECOMP_CP", "u32", decomp_cp, fmt=lambda v: f"0x{v:X}")
        emit_array(f, "DECOMP_FLAGS", "u8", decomp_flags)
        emit_array(f, "DECOMP_OFF", "u32", decomp_off)
        emit_array(f, "DECOMP_LEN", "u8", decomp_len)
        emit_array(f, "DECOMP_SEQ", "u32", decomp_seq, fmt=lambda v: f"0x{v:X}")
        emit_array(f, "CCC_CP", "u32", [c for c, _ in ccc], fmt=lambda v: f"0x{v:X}")
        emit_array(f, "CCC_VAL", "u8", [v for _, v in ccc])
        emit_array(f, "COMP_A", "u32", [a for a, _, _ in comps], fmt=lambda v: f"0x{v:X}")
        emit_array(f, "COMP_B", "u32", [b for _, b, _ in comps], fmt=lambda v: f"0x{v:X}")
        emit_array(f, "COMP_C", "u32", [c for _, _, c in comps], fmt=lambda v: f"0x{v:X}")
        emit_array(f, "FOLD_CP", "u32", fold_cp, fmt=lambda v: f"0x{v:X}")
        emit_array(f, "FOLD_OFF", "u32", fold_off)
        emit_array(f, "FOLD_LEN", "u8", fold_len)
        emit_array(f, "FOLD_SEQ", "u32", fold_seq, fmt=lambda v: f"0x{v:X}")

    with open(dest, "r", encoding="utf-8") as f:
        text = f.read()
    with open(dest, "w", encoding="utf-8", newline="\n") as f:
        f.write(text.rstrip("\n") + "\n")

    print(
        f"{dest}: UCD {unicodedata.unidata_version}, "
        f"{len(decomps)} decompositions ({len(decomp_seq)} parts), "
        f"{len(ccc)} combining classes, {len(comps)} composites, "
        f"{len(folds)} case folds"
    )


if __name__ == "__main__":
    main()
