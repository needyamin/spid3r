//! Compression: DEFLATE (RFC 1951) with the gzip (RFC 1952) and zlib
//! (RFC 1950) wrappers, written from scratch — the zero-dependency rule means
//! no `flate2`/`miniz` here.
//!
//! The decoder handles every block type (stored, fixed Huffman, dynamic
//! Huffman), because that is what real `.gz` files use. The encoder emits
//! fixed-Huffman blocks over greedy LZ77 matches: valid, interoperable output
//! that any gzip/zlib implementation reads; it does not chase ratio (dynamic
//! Huffman would buy another ~10-20% and is not implemented).
//!
//! Natives: `deflate`/`inflate` (raw), `gzip_compress`/`gunzip`,
//! `zlib_compress`/`unzlib`, plus the checksums `crc32`/`adler32`.

pub use super::deflate::{deflate_raw, inflate_raw};
use crate::value::{new_ref, NativeCtx, Value};

// ------------------------------------------------------------------ checksums

fn crc_table() -> &'static [u32; 256] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        let mut n = 0;
        while n < 256 {
            let mut c = n as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
                k += 1;
            }
            t[n] = c;
            n += 1;
        }
        t
    })
}

pub fn crc32_bytes(data: &[u8]) -> u32 {
    let t = crc_table();
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = t[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

pub fn adler32_bytes(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

// ------------------------------------------------------------- gzip / zlib

pub fn gzip_compress_bytes(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0xff];
    out.extend_from_slice(&deflate_raw(data));
    out.extend_from_slice(&crc32_bytes(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

pub fn gunzip_bytes(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 18 {
        return Err("gunzip: input is too short to be a gzip stream".to_string());
    }
    if data[0] != 0x1f || data[1] != 0x8b {
        return Err("gunzip: not a gzip stream (bad magic)".to_string());
    }
    if data[2] != 0x08 {
        return Err(format!(
            "gunzip: unsupported compression method {}",
            data[2]
        ));
    }
    let flags = data[3];
    let mut pos = 10usize;
    if flags & 0x04 != 0 {
        // FEXTRA
        if pos + 2 > data.len() {
            return Err("gunzip: truncated FEXTRA".to_string());
        }
        let xlen = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2 + xlen;
    }
    for mask in [0x08u8, 0x10u8] {
        // FNAME, FCOMMENT: zero-terminated
        if flags & mask != 0 {
            while pos < data.len() && data[pos] != 0 {
                pos += 1;
            }
            pos += 1;
        }
    }
    if flags & 0x02 != 0 {
        pos += 2; // FHCRC
    }
    if pos > data.len() {
        return Err("gunzip: truncated header".to_string());
    }
    let body = &data[pos..];
    let out = inflate_raw(body)?;
    // Trailer: CRC32 + ISIZE, little endian, last 8 bytes.
    if data.len() >= 8 {
        let tail = &data[data.len() - 8..];
        let want_crc = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
        let want_len = u32::from_le_bytes([tail[4], tail[5], tail[6], tail[7]]);
        let got_crc = crc32_bytes(&out);
        if got_crc != want_crc {
            return Err(format!(
                "gunzip: CRC mismatch (stored {:08x}, computed {:08x})",
                want_crc, got_crc
            ));
        }
        if (out.len() as u32) != want_len {
            return Err(format!(
                "gunzip: length mismatch (stored {}, got {})",
                want_len,
                out.len()
            ));
        }
    }
    Ok(out)
}

pub fn zlib_compress_bytes(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x9c];
    out.extend_from_slice(&deflate_raw(data));
    out.extend_from_slice(&adler32_bytes(data).to_be_bytes());
    out
}

pub fn unzlib_bytes(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 6 {
        return Err("unzlib: input is too short to be a zlib stream".to_string());
    }
    let cmf = data[0];
    let flg = data[1];
    if cmf & 0x0f != 8 {
        return Err(format!(
            "unzlib: unsupported compression method {}",
            cmf & 0x0f
        ));
    }
    if !(((cmf as u16) << 8) | flg as u16).is_multiple_of(31) {
        return Err("unzlib: header checksum failed".to_string());
    }
    if flg & 0x20 != 0 {
        return Err("unzlib: preset dictionaries are not supported".to_string());
    }
    let out = inflate_raw(&data[2..])?;
    if data.len() >= 4 {
        let tail = &data[data.len() - 4..];
        let want = u32::from_be_bytes([tail[0], tail[1], tail[2], tail[3]]);
        let got = adler32_bytes(&out);
        if got != want {
            return Err(format!(
                "unzlib: Adler-32 mismatch (stored {:08x}, computed {:08x})",
                want, got
            ));
        }
    }
    Ok(out)
}

// ----------------------------------------------------------------------- zip

/// One archive member. `zip_create` takes a list of maps
/// `{name: string, data: string|bytes}` and returns the archive bytes;
/// `zip_read` reverses it into a name -> bytes map.
pub struct ZipEntry {
    pub name: String,
    pub data: Vec<u8>,
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn rd_u16(data: &[u8], at: usize) -> Result<u16, String> {
    if at + 2 > data.len() {
        return Err("zip: truncated archive".to_string());
    }
    Ok(u16::from_le_bytes([data[at], data[at + 1]]))
}

fn rd_u32(data: &[u8], at: usize) -> Result<u32, String> {
    if at + 4 > data.len() {
        return Err("zip: truncated archive".to_string());
    }
    Ok(u32::from_le_bytes([
        data[at],
        data[at + 1],
        data[at + 2],
        data[at + 3],
    ]))
}

pub fn zip_create(entries: &[ZipEntry]) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    for e in entries {
        if e.name.is_empty() {
            return Err("zip: entry name must not be empty".to_string());
        }
        if e.name.len() > 0xFFFF {
            return Err(format!("zip: entry name is too long: {}", e.name));
        }
        let crc = crc32_bytes(&e.data);
        let packed = deflate_raw(&e.data);
        // Store when deflating does not help (tiny or incompressible entries).
        let (method, body): (u16, Vec<u8>) = if packed.len() < e.data.len() {
            (8, packed)
        } else {
            (0, e.data.clone())
        };
        let offset = out.len() as u32;
        put_u32(&mut out, 0x0403_4b50);
        put_u16(&mut out, 20); // version needed
        put_u16(&mut out, 0); // flags
        put_u16(&mut out, method);
        put_u16(&mut out, 0); // mod time
        put_u16(&mut out, 0x21); // mod date: 1980-01-01
        put_u32(&mut out, crc);
        put_u32(&mut out, body.len() as u32);
        put_u32(&mut out, e.data.len() as u32);
        put_u16(&mut out, e.name.len() as u16);
        put_u16(&mut out, 0); // extra length
        out.extend_from_slice(e.name.as_bytes());
        out.extend_from_slice(&body);

        put_u32(&mut central, 0x0201_4b50);
        put_u16(&mut central, 20); // version made by
        put_u16(&mut central, 20); // version needed
        put_u16(&mut central, 0); // flags
        put_u16(&mut central, method);
        put_u16(&mut central, 0);
        put_u16(&mut central, 0x21);
        put_u32(&mut central, crc);
        put_u32(&mut central, body.len() as u32);
        put_u32(&mut central, e.data.len() as u32);
        put_u16(&mut central, e.name.len() as u16);
        put_u16(&mut central, 0); // extra
        put_u16(&mut central, 0); // comment
        put_u16(&mut central, 0); // disk
        put_u16(&mut central, 0); // internal attrs
        put_u32(&mut central, 0); // external attrs
        put_u32(&mut central, offset);
        central.extend_from_slice(e.name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    let count = entries.len() as u16;
    out.extend_from_slice(&central);
    put_u32(&mut out, 0x0605_4b50);
    put_u16(&mut out, 0);
    put_u16(&mut out, 0);
    put_u16(&mut out, count);
    put_u16(&mut out, count);
    put_u32(&mut out, cd_size);
    put_u32(&mut out, cd_offset);
    put_u16(&mut out, 0); // comment length
    Ok(out)
}

pub fn zip_read(data: &[u8]) -> Result<Vec<ZipEntry>, String> {
    if data.len() < 22 {
        return Err("zip: input is too short to be an archive".to_string());
    }
    // The EOCD is at the end, possibly followed by a comment.
    let mut eocd = None;
    let mut i = data.len() - 22;
    loop {
        if data[i..i + 4] == [0x50, 0x4b, 0x05, 0x06] {
            eocd = Some(i);
            break;
        }
        if i == 0 {
            break;
        }
        i -= 1;
    }
    let eocd = eocd.ok_or_else(|| "zip: no end-of-central-directory record".to_string())?;
    let count = rd_u16(data, eocd + 10)? as usize;
    let cd_size = rd_u32(data, eocd + 12)? as usize;
    let cd_off = rd_u32(data, eocd + 16)? as usize;
    if cd_off + cd_size > data.len() {
        return Err("zip: central directory runs past the end of the file".to_string());
    }
    let mut pos = cd_off;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if rd_u32(data, pos)? != 0x0201_4b50 {
            return Err("zip: bad central-directory signature".to_string());
        }
        let method = rd_u16(data, pos + 10)?;
        let crc = rd_u32(data, pos + 16)?;
        let csize = rd_u32(data, pos + 20)? as usize;
        let usize_ = rd_u32(data, pos + 24)? as usize;
        let nlen = rd_u16(data, pos + 28)? as usize;
        let elen = rd_u16(data, pos + 30)? as usize;
        let clen = rd_u16(data, pos + 32)? as usize;
        let loff = rd_u32(data, pos + 42)? as usize;
        let name_start = pos + 46;
        if name_start + nlen > data.len() {
            return Err("zip: truncated entry name".to_string());
        }
        let name = String::from_utf8_lossy(&data[name_start..name_start + nlen]).to_string();

        // The local header repeats the name and may add an extra field; the
        // payload starts after both.
        let l_nlen = rd_u16(data, loff + 26)? as usize;
        let l_elen = rd_u16(data, loff + 28)? as usize;
        let start = loff + 30 + l_nlen + l_elen;
        if start + csize > data.len() {
            return Err(format!(
                "zip: entry '{}' runs past the end of the file",
                name
            ));
        }
        let body = &data[start..start + csize];
        let content = match method {
            0 => body.to_vec(),
            8 => inflate_raw(body)?,
            other => {
                return Err(format!(
                    "zip: entry '{}' uses unsupported method {}",
                    name, other
                ))
            }
        };
        if content.len() != usize_ {
            return Err(format!(
                "zip: entry '{}' size mismatch (stored {}, got {})",
                name,
                usize_,
                content.len()
            ));
        }
        if crc32_bytes(&content) != crc {
            return Err(format!("zip: entry '{}' failed its CRC check", name));
        }
        out.push(ZipEntry {
            name,
            data: content,
        });
        pos = name_start + nlen + elen + clen;
    }
    Ok(out)
}

// ------------------------------------------------------------------- natives

fn input_bytes(v: &Value, who: &str) -> Result<Vec<u8>, String> {
    match v {
        Value::Str(s) => Ok(s.as_bytes().to_vec()),
        Value::Bytes(b) => Ok(b.lock().unwrap().clone()),
        other => Err(format!(
            "{} expects a string or bytes, found {}",
            who,
            crate::value::type_name(other)
        )),
    }
}

fn ok_bytes(data: Vec<u8>) -> Value {
    Value::Bytes(new_ref(data))
}

fn ok_result(r: Result<Vec<u8>, String>) -> Value {
    match r {
        Ok(d) => Value::ok(Value::Bytes(new_ref(d))),
        Err(e) => Value::err(Value::Str(e.into())),
    }
}

fn n_crc32(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("crc32 expects 1 argument, got {}", args.len()));
    }
    let data = input_bytes(&args[0], "crc32")?;
    Ok(Value::Int(crc32_bytes(&data) as i64))
}

fn n_adler32(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("adler32 expects 1 argument, got {}", args.len()));
    }
    let data = input_bytes(&args[0], "adler32")?;
    Ok(Value::Int(adler32_bytes(&data) as i64))
}

fn n_deflate(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("deflate expects 1 argument, got {}", args.len()));
    }
    let data = input_bytes(&args[0], "deflate")?;
    Ok(ok_bytes(deflate_raw(&data)))
}

fn n_inflate(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("inflate expects 1 argument, got {}", args.len()));
    }
    let data = input_bytes(&args[0], "inflate")?;
    Ok(ok_result(inflate_raw(&data)))
}

fn n_gzip_compress(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!(
            "gzip_compress expects 1 argument, got {}",
            args.len()
        ));
    }
    let data = input_bytes(&args[0], "gzip_compress")?;
    Ok(ok_bytes(gzip_compress_bytes(&data)))
}

fn n_gunzip(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("gunzip expects 1 argument, got {}", args.len()));
    }
    let data = input_bytes(&args[0], "gunzip")?;
    Ok(ok_result(gunzip_bytes(&data)))
}

fn n_zlib_compress(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!(
            "zlib_compress expects 1 argument, got {}",
            args.len()
        ));
    }
    let data = input_bytes(&args[0], "zlib_compress")?;
    Ok(ok_bytes(zlib_compress_bytes(&data)))
}

fn n_unzlib(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("unzlib expects 1 argument, got {}", args.len()));
    }
    let data = input_bytes(&args[0], "unzlib")?;
    Ok(ok_result(unzlib_bytes(&data)))
}

fn map_field(
    m: &crate::value::Ref<crate::value::collections::seq::MapData>,
    key: &str,
) -> Option<Value> {
    let guard = m.lock().unwrap();
    for (k, v) in guard.entries.iter() {
        if let crate::value::MapKey::Str(s) = k {
            if &**s == key {
                return Some(v.clone());
            }
        }
    }
    None
}

/// `zip_create([{name, data}, ...])` -> archive bytes.
fn n_zip_create(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("zip_create expects 1 argument, got {}", args.len()));
    }
    let items = match &args[0] {
        Value::List(l) => l.lock().unwrap().clone_items(),
        other => {
            return Err(format!(
                "zip_create expects a list of {{name, data}} maps, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    let mut entries = Vec::with_capacity(items.len());
    for item in items.iter() {
        let m = match item {
            Value::Map(m) => m.clone(),
            other => {
                return Err(format!(
                    "zip_create: each entry must be a map, found {}",
                    crate::value::type_name(other)
                ))
            }
        };
        let name = match map_field(&m, "name") {
            Some(Value::Str(s)) => s.to_string(),
            _ => return Err("zip_create: each entry needs a string 'name'".to_string()),
        };
        let data = match map_field(&m, "data") {
            Some(v) => input_bytes(&v, "zip_create")?,
            None => return Err("zip_create: each entry needs a 'data' value".to_string()),
        };
        entries.push(ZipEntry { name, data });
    }
    Ok(ok_bytes(zip_create(&entries)?))
}

/// `zip_read(archive)` -> ok(map name -> bytes) | err(message).
fn n_zip_read(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("zip_read expects 1 argument, got {}", args.len()));
    }
    let data = input_bytes(&args[0], "zip_read")?;
    match zip_read(&data) {
        Ok(entries) => {
            let pairs: Vec<(crate::value::MapKey, Value)> = entries
                .into_iter()
                .map(|e| {
                    (
                        crate::value::MapKey::Str(e.name.into()),
                        Value::Bytes(new_ref(e.data)),
                    )
                })
                .collect();
            Ok(Value::ok(Value::Map(new_ref(
                crate::value::collections::seq::MapData::from_entries(pairs),
            ))))
        }
        Err(e) => Ok(Value::err(Value::Str(e.into()))),
    }
}

/// `zip_list(archive)` -> ok([names]) | err(message).
fn n_zip_list(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("zip_list expects 1 argument, got {}", args.len()));
    }
    let data = input_bytes(&args[0], "zip_list")?;
    match zip_read(&data) {
        Ok(entries) => {
            let items: Vec<Value> = entries
                .into_iter()
                .map(|e| Value::Str(e.name.into()))
                .collect();
            Ok(Value::ok(crate::value::collections::seq::make_list(items)))
        }
        Err(e) => Ok(Value::err(Value::Str(e.into()))),
    }
}

pub fn native_defs() -> Vec<crate::stdlib::NativeDef> {
    vec![
        ("crc32", n_crc32),
        ("adler32", n_adler32),
        ("deflate", n_deflate),
        ("inflate", n_inflate),
        ("gzip_compress", n_gzip_compress),
        ("gunzip", n_gunzip),
        ("zlib_compress", n_zlib_compress),
        ("unzlib", n_unzlib),
        ("zip_create", n_zip_create),
        ("zip_read", n_zip_read),
        ("zip_list", n_zip_list),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_known_value() {
        // The canonical CRC-32 of "123456789".
        assert_eq!(crc32_bytes(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn adler32_matches_the_known_value() {
        assert_eq!(adler32_bytes(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn roundtrip_short() {
        for input in [
            &b""[..],
            b"a",
            b"hello hello hello hello",
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            let packed = deflate_raw(input);
            assert_eq!(inflate_raw(&packed).unwrap(), input);
        }
    }

    #[test]
    fn roundtrip_repetitive_is_smaller() {
        let input: Vec<u8> = b"the quick brown fox jumps over the lazy dog. "
            .iter()
            .cycle()
            .take(4000)
            .copied()
            .collect();
        let packed = deflate_raw(&input);
        assert!(packed.len() < input.len() / 2, "packed {}", packed.len());
        assert_eq!(inflate_raw(&packed).unwrap(), input);
    }

    #[test]
    fn gzip_roundtrip_and_checksum() {
        let input = b"spid3r compression test payload";
        let packed = gzip_compress_bytes(input);
        assert_eq!(&packed[..2], &[0x1f, 0x8b]);
        assert_eq!(gunzip_bytes(&packed).unwrap(), input);
    }

    #[test]
    fn zlib_roundtrip_and_checksum() {
        let input = b"spid3r zlib payload";
        let packed = zlib_compress_bytes(input);
        assert_eq!(&packed[..2], &[0x78, 0x9c]);
        assert_eq!(unzlib_bytes(&packed).unwrap(), input);
    }

    /// Deterministic pseudo-random payloads across sizes and alphabets: the
    /// dynamic-block path has many edge cases (single symbol, all 256 symbols,
    /// skewed frequencies) that a fixed-Huffman test would never reach.
    #[test]
    fn roundtrip_pseudo_random() {
        let mut state: u64 = 0x2545F4914F6CDD1D;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for size in [0usize, 1, 2, 3, 17, 64, 255, 256, 1000, 5000, 40000] {
            // Alphabet of 2: long runs -> dynamic codes with few symbols.
            let two: Vec<u8> = (0..size)
                .map(|_| if next() % 2 == 0 { b'a' } else { b'b' })
                .collect();
            let packed = deflate_raw(&two);
            assert_eq!(
                inflate_raw(&packed).unwrap(),
                two,
                "binary alphabet, size {}",
                size
            );

            // Full alphabet: every literal and distance code gets exercised.
            let all: Vec<u8> = (0..size).map(|_| (next() & 0xFF) as u8).collect();
            let packed = deflate_raw(&all);
            assert_eq!(
                inflate_raw(&packed).unwrap(),
                all,
                "full alphabet, size {}",
                size
            );
        }
    }

    /// The encoder must pick dynamic blocks when they win (BTYPE = 10) and
    /// still produce a stream the decoder accepts.
    #[test]
    fn skewed_text_uses_a_dynamic_block() {
        let mut text = String::new();
        for i in 0..2000 {
            text.push_str(["alpha", "beta", "gamma", "delta"][i % 4]);
            text.push(' ');
        }
        let packed = deflate_raw(text.as_bytes());
        assert_eq!(packed[0] & 1, 1, "BFINAL");
        assert_eq!((packed[0] >> 1) & 3, 2, "BTYPE should be dynamic");
        assert_eq!(inflate_raw(&packed).unwrap(), text.as_bytes());
    }

    #[test]
    fn corrupt_input_is_rejected_not_panicked() {
        let mut packed = gzip_compress_bytes(b"hello world hello world");
        let last = packed.len() - 5;
        packed[last] ^= 0xFF;
        assert!(gunzip_bytes(&packed).is_err());
    }
}
