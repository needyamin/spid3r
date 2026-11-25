//! Pure safe Rust SWAR (SIMD Within A Register) byte scanners.
//!
//! Scans 8 bytes at a time using portable `u64::from_le_bytes` arithmetic
//! without `unsafe` or external dependencies.

const LO_ONES: u64 = 0x0101_0101_0101_0101;
const HI_BITS: u64 = 0x8080_8080_8080_8080;
const TWENTIES: u64 = 0x2020_2020_2020_2020;

const QUOTE_MASK: u64 = 0x2222_2222_2222_2222;
const SLASH_MASK: u64 = 0x5C5C_5C5C_5C5C_5C5C;
const CR_MASK: u64 = 0x0D0D_0D0D_0D0D_0D0D;
const LF_MASK: u64 = 0x0A0A_0A0A_0A0A_0A0A;

#[inline(always)]
fn has_zero_byte(w: u64) -> u64 {
    w.wrapping_sub(LO_ONES) & !w & HI_BITS
}

#[inline(always)]
fn splat(b: u8) -> u64 {
    (b as u64).wrapping_mul(LO_ONES)
}

/// Find the first index of `needle` in `bytes`.
#[inline]
pub(crate) fn find_byte(bytes: &[u8], needle: u8) -> Option<usize> {
    let needle_mask = splat(needle);
    let len = bytes.len();
    let mut i = 0usize;
    while i + 8 <= len {
        let w = u64::from_le_bytes([
            bytes[i],
            bytes[i + 1],
            bytes[i + 2],
            bytes[i + 3],
            bytes[i + 4],
            bytes[i + 5],
            bytes[i + 6],
            bytes[i + 7],
        ]);
        let m = has_zero_byte(w ^ needle_mask);
        if m != 0 {
            return Some(i + (m.trailing_zeros() >> 3) as usize);
        }
        i += 8;
    }
    while i < len {
        if bytes[i] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Find the first `"` (`0x22`) or `\` (`0x5C`) in JSON string content.
#[cfg(test)]
#[inline]
pub(crate) fn find_json_string_end(bytes: &[u8]) -> Option<usize> {
    let len = bytes.len();
    let mut i = 0usize;
    while i + 8 <= len {
        let w = u64::from_le_bytes([
            bytes[i],
            bytes[i + 1],
            bytes[i + 2],
            bytes[i + 3],
            bytes[i + 4],
            bytes[i + 5],
            bytes[i + 6],
            bytes[i + 7],
        ]);
        let m = has_zero_byte(w ^ QUOTE_MASK) | has_zero_byte(w ^ SLASH_MASK);
        if m != 0 {
            return Some(i + (m.trailing_zeros() >> 3) as usize);
        }
        i += 8;
    }
    while i < len {
        let b = bytes[i];
        if b == b'"' || b == b'\\' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Find the first byte requiring JSON escaping (`b < 0x20 || b == '"' || b == '\\'`).
#[inline]
pub(crate) fn find_json_escape_needed(bytes: &[u8]) -> Option<usize> {
    let len = bytes.len();
    let mut i = 0usize;
    while i + 8 <= len {
        let w = u64::from_le_bytes([
            bytes[i],
            bytes[i + 1],
            bytes[i + 2],
            bytes[i + 3],
            bytes[i + 4],
            bytes[i + 5],
            bytes[i + 6],
            bytes[i + 7],
        ]);
        let ctrl = w.wrapping_sub(TWENTIES) & !w & HI_BITS;
        let m = ctrl | has_zero_byte(w ^ QUOTE_MASK) | has_zero_byte(w ^ SLASH_MASK);
        if m != 0 {
            return Some(i + (m.trailing_zeros() >> 3) as usize);
        }
        i += 8;
    }
    while i < len {
        let b = bytes[i];
        if b < 0x20 || b == b'"' || b == b'\\' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Find the first unquoted CSV special byte (`delim`, `"`, `\r`, or `\n`).
#[inline]
pub(crate) fn find_csv_delim(bytes: &[u8], delim: u8) -> Option<usize> {
    let d_mask = splat(delim);
    let len = bytes.len();
    let mut i = 0usize;
    while i + 8 <= len {
        let w = u64::from_le_bytes([
            bytes[i],
            bytes[i + 1],
            bytes[i + 2],
            bytes[i + 3],
            bytes[i + 4],
            bytes[i + 5],
            bytes[i + 6],
            bytes[i + 7],
        ]);
        let m = has_zero_byte(w ^ d_mask)
            | has_zero_byte(w ^ QUOTE_MASK)
            | has_zero_byte(w ^ CR_MASK)
            | has_zero_byte(w ^ LF_MASK);
        if m != 0 {
            return Some(i + (m.trailing_zeros() >> 3) as usize);
        }
        i += 8;
    }
    while i < len {
        let b = bytes[i];
        if b == delim || b == b'"' || b == b'\r' || b == b'\n' {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swar_matches_naive_across_lengths_and_positions() {
        for len in 0..=40 {
            let mut base = vec![b'a'; len];
            assert_eq!(find_byte(&base, b','), None);
            assert_eq!(find_json_string_end(&base), None);
            assert_eq!(find_json_escape_needed(&base), None);
            assert_eq!(find_csv_delim(&base, b','), None);

            for pos in 0..len {
                for &special in &[
                    b',', b'"', b'\\', b'\n', b'\r', 0x00, 0x1F, 0x7F, 0x80, 0xFF,
                ] {
                    base.fill(b'a');
                    base[pos] = special;
                    assert_eq!(
                        find_byte(&base, b','),
                        base.iter().position(|&b| b == b','),
                        "find_byte len={len} pos={pos} special={special:#x}"
                    );
                    assert_eq!(
                        find_json_string_end(&base),
                        base.iter().position(|&b| b == b'"' || b == b'\\'),
                        "find_json_string_end len={len} pos={pos} special={special:#x}"
                    );
                    assert_eq!(
                        find_json_escape_needed(&base),
                        base.iter()
                            .position(|&b| b < 0x20 || b == b'"' || b == b'\\'),
                        "find_json_escape_needed len={len} pos={pos} special={special:#x}"
                    );
                    assert_eq!(
                        find_csv_delim(&base, b','),
                        base.iter()
                            .position(|&b| b == b',' || b == b'"' || b == b'\r' || b == b'\n'),
                        "find_csv_delim len={len} pos={pos} special={special:#x}"
                    );
                }
            }
        }
    }

    #[test]
    fn swar_ctrl_interactions_with_neighbors() {
        // Test combinations around control chars, 0x20, 0x7F, and high UTF-8 bytes
        // across word boundaries so carry/borrow never triggers false hits.
        let sample = [
            0x20, 0x7F, 0x80, 0xC3, 0xA9, 0x21, 0x20, 0x7F, // word 0: no escape
            0xFF, 0x80, 0x20, 0x7F, b'z', 0x1F, b'q', 0x00, // word 1: first at idx 13
        ];
        assert_eq!(find_json_escape_needed(&sample), Some(13));
        assert_eq!(find_json_string_end(&sample), None);

        let mixed = [
            b'a', 0x1F, b'"', 0x80, 0x20, b'\\', 0x01, b'x', // word 0: ctrl 0x1F at idx 1
        ];
        assert_eq!(find_json_escape_needed(&mixed), Some(1));
        assert_eq!(find_json_string_end(&mixed), Some(2));
    }
}
