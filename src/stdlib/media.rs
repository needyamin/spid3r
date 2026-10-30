//! Media: PNG encode/decode, WAV (PCM) encode/decode, and a small PDF writer.
//!
//! PNG rides on the DEFLATE implementation in `compress`, which is what makes
//! a from-scratch codec practical. Supported: colour types 0/2/3/4/6 at 8 bits
//! per sample (16-bit is reduced to 8 by taking the high byte), all five row
//! filters, and Adam7 interlacing. Not supported: animated PNG, ICC colour
//! management, and gamma correction — pixels are returned as stored.
//!
//! JPEG is deliberately absent: a baseline decoder is a project of its own and
//! a half-working one would be worse than none.

use crate::stdlib::compress::{crc32_bytes, unzlib_bytes, zlib_compress_bytes};
use crate::value::collections::seq::make_list;
use crate::value::{new_ref, MapKey, NativeCtx, Value};

const PNG_SIG: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn put_be32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    put_be32(out, data.len() as u32);
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32_bytes(&out[start..]);
    put_be32(out, crc);
}

fn paeth(a: i32, b: i32, c: i32) -> i32 {
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Decoded image: `{width, height, channels, data}` with 8-bit samples.
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub data: Vec<u8>,
}

pub fn png_decode(bytes: &[u8]) -> Result<Image, String> {
    if bytes.len() < 8 || bytes[..8] != PNG_SIG {
        return Err("png: not a PNG file (bad signature)".to_string());
    }
    let mut pos = 8usize;
    let mut width = 0usize;
    let mut height = 0usize;
    let mut bit_depth = 8u8;
    let mut color_type = 0u8;
    let mut interlace = 0u8;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    let mut idat: Vec<u8> = Vec::new();
    let mut seen_ihdr = false;
    while pos + 8 <= bytes.len() {
        let len = be32(bytes, pos) as usize;
        let kind = &bytes[pos + 4..pos + 8];
        let data_start = pos + 8;
        if data_start + len + 4 > bytes.len() {
            return Err("png: chunk runs past the end of the file".to_string());
        }
        let data = &bytes[data_start..data_start + len];
        match kind {
            b"IHDR" => {
                if len != 13 {
                    return Err("png: IHDR has the wrong length".to_string());
                }
                width = be32(data, 0) as usize;
                height = be32(data, 4) as usize;
                bit_depth = data[8];
                color_type = data[9];
                if data[10] != 0 || data[11] != 0 {
                    return Err("png: unknown compression or filter method".to_string());
                }
                interlace = data[12];
                seen_ihdr = true;
            }
            b"PLTE" => {
                for c in data.chunks(3) {
                    if c.len() == 3 {
                        palette.push([c[0], c[1], c[2]]);
                    }
                }
            }
            b"tRNS" => trns = data.to_vec(),
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        pos = data_start + len + 4;
    }
    if !seen_ihdr {
        return Err("png: missing IHDR".to_string());
    }
    if width == 0 || height == 0 {
        return Err("png: zero-sized image".to_string());
    }
    if width > 100_000 || height > 100_000 {
        return Err("png: image dimensions are implausible".to_string());
    }
    let channels = match color_type {
        0 => 1,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        other => return Err(format!("png: unsupported colour type {}", other)),
    };
    if bit_depth != 8 {
        // 16-bit samples are reduced to 8 by keeping the high byte; lower bit
        // depths (1/2/4) are only legal for greyscale and palette, and are not
        // implemented.
        if bit_depth != 16 {
            return Err(format!("png: unsupported bit depth {}", bit_depth));
        }
    }
    let raw = unzlib_bytes(&idat)?;
    let bytes_per_sample = if bit_depth == 16 { 2 } else { 1 };
    let bpp = channels * bytes_per_sample;

    // Bound the pixel buffer with checked arithmetic BEFORE allocating: a
    // 69-byte PNG can claim 100000x100000 16-bit RGBA (80 GB) and the old
    // code allocated the buffer before ever comparing it against the
    // (tiny) decompressed data, aborting the process on the failed alloc.
    // A real image's pixel buffer is never larger than its decompressed
    // IDAT stream plus one filter byte per row, so 256 MiB (the same cap
    // `unzlib_bytes` enforces on `raw`) is a safe ceiling.
    const MAX_PIXEL_BYTES: usize = 256 * 1024 * 1024;
    let total = width
        .checked_mul(height)
        .and_then(|v| v.checked_mul(channels))
        .and_then(|v| v.checked_mul(bytes_per_sample))
        .ok_or("png: image dimensions are implausible")?;
    if total > MAX_PIXEL_BYTES {
        return Err("png: image is too large to decode".to_string());
    }

    // 16-bit images keep two bytes per sample, so the unfiltered buffer is
    // twice the 8-bit size (this used to be sized for 8 bits and panicked).
    let mut pixels = vec![0u8; total];
    if interlace == 0 {
        let need = height * (1 + width * bpp);
        if raw.len() < need {
            return Err("png: image data is shorter than the header says".to_string());
        }
        unfilter_pass(
            &raw,
            width,
            height,
            channels,
            bytes_per_sample,
            &mut pixels,
            width,
            0,
            0,
            1,
            1,
        )?;
    } else if interlace == 1 {
        const PASSES: [(usize, usize, usize, usize); 7] = [
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ];
        let mut off = 0usize;
        for &(xs, ys, xstep, ystep) in PASSES.iter() {
            let pw = if width > xs {
                (width - xs).div_ceil(xstep)
            } else {
                0
            };
            let ph = if height > ys {
                (height - ys).div_ceil(ystep)
            } else {
                0
            };
            if pw == 0 || ph == 0 {
                continue;
            }
            let need = ph * (1 + pw * bpp);
            if off + need > raw.len() {
                return Err("png: interlaced data is truncated".to_string());
            }
            unfilter_pass(
                &raw[off..off + need],
                pw,
                ph,
                channels,
                bytes_per_sample,
                &mut pixels,
                width,
                xs,
                ys,
                xstep,
                ystep,
            )?;
            off += need;
        }
    } else {
        return Err(format!("png: unknown interlace method {}", interlace));
    }

    // Expand palette and drop 16-bit precision.
    let mut out = Vec::new();
    if color_type == 3 {
        if palette.is_empty() {
            return Err("png: palette image without PLTE".to_string());
        }
        let has_alpha = !trns.is_empty();
        let out_channels = if has_alpha { 4 } else { 3 };
        out.reserve(width * height * out_channels);
        for &idx in pixels.iter() {
            let entry = palette
                .get(idx as usize)
                .ok_or_else(|| format!("png: palette index {} out of range", idx))?;
            out.extend_from_slice(entry);
            if has_alpha {
                out.push(*trns.get(idx as usize).unwrap_or(&255));
            }
        }
        return Ok(Image {
            width,
            height,
            channels: out_channels,
            data: out,
        });
    }
    if bit_depth == 16 {
        for row in 0..height {
            for col in 0..width {
                for ch in 0..channels {
                    let at = (row * width + col) * channels * 2 + ch * 2;
                    out.push(pixels[at]);
                }
            }
        }
        return Ok(Image {
            width,
            height,
            channels,
            data: out,
        });
    }
    Ok(Image {
        width,
        height,
        channels,
        data: pixels,
    })
}

#[allow(clippy::too_many_arguments)]
fn unfilter_pass(
    raw: &[u8],
    pass_w: usize,
    pass_h: usize,
    channels: usize,
    sample_bytes: usize,
    out: &mut [u8],
    full_w: usize,
    x0: usize,
    y0: usize,
    xstep: usize,
    ystep: usize,
) -> Result<(), String> {
    let bpp = channels * sample_bytes;
    let stride = pass_w * bpp;
    let mut prev = vec![0u8; stride];
    let mut cur = vec![0u8; stride];
    for row in 0..pass_h {
        let at = row * (1 + stride);
        let filter = raw[at];
        cur.copy_from_slice(&raw[at + 1..at + 1 + stride]);
        for i in 0..stride {
            let a = if i >= bpp { cur[i - bpp] as i32 } else { 0 };
            let b = prev[i] as i32;
            let c = if i >= bpp { prev[i - bpp] as i32 } else { 0 };
            let v = cur[i] as i32;
            let predicted = match filter {
                0 => 0,
                1 => a,
                2 => b,
                3 => (a + b) / 2,
                4 => paeth(a, b, c),
                other => return Err(format!("png: unknown row filter {}", other)),
            };
            cur[i] = ((v + predicted) & 0xff) as u8;
        }
        // Scatter this pass row into the full image.
        let y = y0 + row * ystep;
        for col in 0..pass_w {
            let x = x0 + col * xstep;
            for ch in 0..channels {
                let src = col * bpp + ch * sample_bytes;
                let dst = (y * full_w + x) * channels * sample_bytes + ch * sample_bytes;
                out[dst..dst + sample_bytes].copy_from_slice(&cur[src..src + sample_bytes]);
            }
        }
        prev.copy_from_slice(&cur);
    }
    Ok(())
}

/// Encode 8-bit samples. `channels` must be 1 (grey), 2 (grey+alpha), 3 (RGB)
/// or 4 (RGBA); rows use filter 0 and the whole stream is one DEFLATE block.
pub fn png_encode(
    width: usize,
    height: usize,
    channels: usize,
    data: &[u8],
) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("png_encode: image must have a positive size".to_string());
    }
    let color_type = match channels {
        1 => 0u8,
        2 => 4,
        3 => 2,
        4 => 6,
        other => return Err(format!("png_encode: unsupported channel count {}", other)),
    };
    let expected = width * height * channels;
    if data.len() < expected {
        return Err(format!(
            "png_encode: expected {} bytes for {}x{}x{}, got {}",
            expected,
            width,
            height,
            channels,
            data.len()
        ));
    }
    let stride = width * channels;
    let mut raw = Vec::with_capacity(height * (stride + 1));
    for row in 0..height {
        raw.push(0); // filter: none
        raw.extend_from_slice(&data[row * stride..(row + 1) * stride]);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&PNG_SIG);
    let mut ihdr = Vec::with_capacity(13);
    put_be32(&mut ihdr, width as u32);
    put_be32(&mut ihdr, height as u32);
    ihdr.push(8); // bit depth
    ihdr.push(color_type);
    ihdr.push(0); // compression
    ihdr.push(0); // filter
    ihdr.push(0); // interlace
    chunk(&mut out, b"IHDR", &ihdr);
    // PNG requires a zlib stream (RFC 1950) inside IDAT, not raw DEFLATE.
    chunk(&mut out, b"IDAT", &zlib_compress_bytes(&raw));
    chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

// ----------------------------------------------------------------------- wav

pub struct Wav {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits: u16,
    pub data: Vec<u8>,
}

pub fn wav_decode(bytes: &[u8]) -> Result<Wav, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("wav: not a RIFF/WAVE file".to_string());
    }
    let mut pos = 12usize;
    let mut fmt: Option<(u16, u16, u32, u16)> = None; // channels, bits, rate, format
    let mut data: Option<Vec<u8>> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let body = pos + 8;
        if body + size > bytes.len() {
            return Err("wav: chunk runs past the end of the file".to_string());
        }
        if id == b"fmt " {
            if size < 16 {
                return Err("wav: fmt chunk is too short".to_string());
            }
            let format = u16::from_le_bytes([bytes[body], bytes[body + 1]]);
            let channels = u16::from_le_bytes([bytes[body + 2], bytes[body + 3]]);
            let rate = u32::from_le_bytes([
                bytes[body + 4],
                bytes[body + 5],
                bytes[body + 6],
                bytes[body + 7],
            ]);
            let bits = u16::from_le_bytes([bytes[body + 14], bytes[body + 15]]);
            fmt = Some((channels, bits, rate, format));
        } else if id == b"data" {
            data = Some(bytes[body..body + size].to_vec());
        }
        pos = body + size + (size & 1); // chunks are word-aligned
    }
    let (channels, bits, sample_rate, format) =
        fmt.ok_or_else(|| "wav: missing fmt chunk".to_string())?;
    if format != 1 && format != 0xFFFE {
        return Err(format!(
            "wav: only PCM is supported (format {} — compressed audio is not)",
            format
        ));
    }
    if bits != 8 && bits != 16 && bits != 24 && bits != 32 {
        return Err(format!("wav: unsupported sample size {} bits", bits));
    }
    Ok(Wav {
        sample_rate,
        channels,
        bits,
        data: data.ok_or_else(|| "wav: missing data chunk".to_string())?,
    })
}

pub fn wav_encode(
    sample_rate: u32,
    channels: u16,
    bits: u16,
    data: &[u8],
) -> Result<Vec<u8>, String> {
    if channels == 0 {
        return Err("wav_encode: channels must be at least 1".to_string());
    }
    if bits != 8 && bits != 16 && bits != 24 && bits != 32 {
        return Err(format!("wav_encode: unsupported sample size {} bits", bits));
    }
    let block_align = channels * (bits / 8);
    let byte_rate = sample_rate * block_align as u32;
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend_from_slice(b"RIFF");
    put_le32(&mut out, (36 + data.len()) as u32);
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    put_le32(&mut out, 16);
    put_le16(&mut out, 1); // PCM
    put_le16(&mut out, channels);
    put_le32(&mut out, sample_rate);
    put_le32(&mut out, byte_rate);
    put_le16(&mut out, block_align);
    put_le16(&mut out, bits);
    out.extend_from_slice(b"data");
    put_le32(&mut out, data.len() as u32);
    out.extend_from_slice(data);
    Ok(out)
}

fn put_le32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_le16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

// ----------------------------------------------------------------------- pdf

/// A minimal single/multi-page PDF with Helvetica text.
///
/// `pages` is a list of strings; each string is one page's content stream in
/// PDF operators (`BT /F1 12 Tf 72 720 Td (Hello) Tj ET`). Text is escaped.
pub fn pdf_write(pages: &[String], width: f64, height: f64) -> Vec<u8> {
    let mut objects: Vec<String> = Vec::new();
    let n_pages = pages.len();
    // 1: catalog, 2: pages, then per page: page object + content object,
    // then the font.
    let font_id = 3 + n_pages * 2;
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".to_string());
    let kids: Vec<String> = (0..n_pages).map(|i| format!("{} 0 R", 3 + i * 2)).collect();
    objects.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        n_pages
    ));
    for (i, content) in pages.iter().enumerate() {
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Contents {} 0 R /Resources << /Font << /F1 {} 0 R >> >> >>",
            width,
            height,
            4 + i * 2,
            font_id
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            content.len(),
            content
        ));
    }
    objects.push(
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_string(),
    );

    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n");
    let mut offsets: Vec<usize> = Vec::with_capacity(objects.len());
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", i + 1, body).as_bytes());
    }
    let xref_at = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in offsets.iter() {
        out.extend_from_slice(format!("{:010} 00000 n \n", off).as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            xref_at
        )
        .as_bytes(),
    );
    out
}

/// Escape a string for a PDF literal `(...)`.
pub fn pdf_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '(' | ')' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 32 => out.push(' '),
            c if (c as u32) > 126 => out.push('?'),
            c => out.push(c),
        }
    }
    out
}

// ------------------------------------------------------------------- natives

fn bytes_arg(v: &Value, who: &str) -> Result<Vec<u8>, String> {
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

fn image_value(img: Image) -> Value {
    let entries: Vec<(MapKey, Value)> = vec![
        (MapKey::Str("width".into()), Value::Int(img.width as i64)),
        (MapKey::Str("height".into()), Value::Int(img.height as i64)),
        (
            MapKey::Str("channels".into()),
            Value::Int(img.channels as i64),
        ),
        (MapKey::Str("data".into()), Value::Bytes(new_ref(img.data))),
    ];
    Value::Map(new_ref(
        crate::value::collections::seq::MapData::from_entries(entries),
    ))
}

fn n_png_decode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("png_decode expects 1 argument, got {}", args.len()));
    }
    let data = bytes_arg(&args[0], "png_decode")?;
    match png_decode(&data) {
        Ok(img) => Ok(Value::Result(Ok(Box::new(image_value(img))))),
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(e.into()))))),
    }
}

fn n_png_encode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 4 {
        return Err(format!(
            "png_encode expects 4 arguments (width, height, channels, data), got {}",
            args.len()
        ));
    }
    let dim = |i: usize, name: &str| -> Result<usize, String> {
        match &args[i] {
            Value::Int(v) if *v > 0 => Ok(*v as usize),
            other => Err(format!(
                "png_encode: {} must be a positive int, found {}",
                name,
                crate::value::type_name(other)
            )),
        }
    };
    let width = dim(0, "width")?;
    let height = dim(1, "height")?;
    let channels = dim(2, "channels")?;
    let data = bytes_arg(&args[3], "png_encode")?;
    Ok(Value::Bytes(new_ref(png_encode(
        width, height, channels, &data,
    )?)))
}

fn n_wav_decode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("wav_decode expects 1 argument, got {}", args.len()));
    }
    let data = bytes_arg(&args[0], "wav_decode")?;
    match wav_decode(&data) {
        Ok(w) => {
            let entries: Vec<(MapKey, Value)> = vec![
                (
                    MapKey::Str("sample_rate".into()),
                    Value::Int(w.sample_rate as i64),
                ),
                (
                    MapKey::Str("channels".into()),
                    Value::Int(w.channels as i64),
                ),
                (MapKey::Str("bits".into()), Value::Int(w.bits as i64)),
                (MapKey::Str("data".into()), Value::Bytes(new_ref(w.data))),
            ];
            Ok(Value::Result(Ok(Box::new(Value::Map(new_ref(
                crate::value::collections::seq::MapData::from_entries(entries),
            ))))))
        }
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(e.into()))))),
    }
}

fn n_wav_encode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 4 {
        return Err(format!(
            "wav_encode expects 4 arguments (sample_rate, channels, bits, data), got {}",
            args.len()
        ));
    }
    let rate = match &args[0] {
        Value::Int(v) if *v > 0 => *v as u32,
        other => {
            return Err(format!(
                "wav_encode: sample_rate must be a positive int, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    let channels = match &args[1] {
        Value::Int(v) if *v > 0 => *v as u16,
        other => {
            return Err(format!(
                "wav_encode: channels must be a positive int, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    let bits = match &args[2] {
        Value::Int(v) => *v as u16,
        other => {
            return Err(format!(
                "wav_encode: bits must be an int, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    let data = bytes_arg(&args[3], "wav_encode")?;
    Ok(Value::Bytes(new_ref(wav_encode(
        rate, channels, bits, &data,
    )?)))
}

/// `pdf_write(pages, width = 595, height = 842)` -> bytes.
fn n_pdf_write(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.is_empty() || args.len() > 3 {
        return Err(format!(
            "pdf_write expects 1 to 3 arguments (pages, width=595, height=842), got {}",
            args.len()
        ));
    }
    let pages = match &args[0] {
        Value::List(l) => l.lock().unwrap().items.clone(),
        Value::Str(s) => vec![Value::Str(s.clone())],
        other => {
            return Err(format!(
                "pdf_write: pages must be a list of strings, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    let mut streams: Vec<String> = Vec::with_capacity(pages.len());
    for p in pages.iter() {
        match p {
            Value::Str(s) => streams.push(s.to_string()),
            other => {
                return Err(format!(
                    "pdf_write: each page must be a string, found {}",
                    crate::value::type_name(other)
                ))
            }
        }
    }
    let num = |i: usize, default: f64| -> Result<f64, String> {
        if args.len() > i {
            match &args[i] {
                Value::Int(v) => Ok(*v as f64),
                Value::Float(v) => Ok(*v),
                other => Err(format!(
                    "pdf_write: page size must be a number, found {}",
                    crate::value::type_name(other)
                )),
            }
        } else {
            Ok(default)
        }
    };
    let w = num(1, 595.0)?;
    let h = num(2, 842.0)?;
    Ok(Value::Bytes(new_ref(pdf_write(&streams, w, h))))
}

/// `pdf_escape(text)` — escape for a PDF literal string.
fn n_pdf_escape(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("pdf_escape expects 1 argument, got {}", args.len()));
    }
    let s = match &args[0] {
        Value::Str(s) => s.to_string(),
        other => {
            return Err(format!(
                "pdf_escape expects a string, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    Ok(Value::Str(pdf_escape(&s).into()))
}

/// `image_channels` is not needed — but keep a helper for lists of samples.
#[allow(dead_code)]
fn samples_to_list(data: &[u8]) -> Value {
    make_list(data.iter().map(|b| Value::Int(*b as i64)).collect())
}

pub fn native_defs() -> Vec<crate::stdlib::NativeDef> {
    vec![
        ("png_decode", n_png_decode),
        ("png_encode", n_png_encode),
        ("wav_decode", n_wav_decode),
        ("wav_encode", n_wav_encode),
        ("pdf_write", n_pdf_write),
        ("pdf_escape", n_pdf_escape),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_roundtrip_rgb() {
        let (w, h) = (4usize, 3usize);
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                data.push((x * 40) as u8);
                data.push((y * 80) as u8);
                data.push(128);
            }
        }
        let png = png_encode(w, h, 3, &data).unwrap();
        assert_eq!(&png[..8], &PNG_SIG);
        let back = png_decode(&png).unwrap();
        assert_eq!(back.width, w);
        assert_eq!(back.height, h);
        assert_eq!(back.channels, 3);
        assert_eq!(back.data, data);
    }

    #[test]
    fn png_roundtrip_rgba_and_gray() {
        for channels in [1usize, 2, 4] {
            let (w, h) = (5usize, 4usize);
            let data: Vec<u8> = (0..w * h * channels).map(|i| (i * 7) as u8).collect();
            let png = png_encode(w, h, channels, &data).unwrap();
            let back = png_decode(&png).unwrap();
            assert_eq!(back.channels, channels);
            assert_eq!(back.data, data, "channels {}", channels);
        }
    }

    #[test]
    fn png_rejects_garbage() {
        assert!(png_decode(b"not a png").is_err());
        let mut png = png_encode(1, 1, 3, &[1, 2, 3]).unwrap();
        png[20] ^= 0xff; // corrupt the IHDR width
        assert!(png_decode(&png).is_err());
    }

    #[test]
    fn wav_roundtrip() {
        let samples: Vec<u8> = (0..400).map(|i| (i % 256) as u8).collect();
        let wav = wav_encode(44100, 2, 16, &samples).unwrap();
        let back = wav_decode(&wav).unwrap();
        assert_eq!(back.sample_rate, 44100);
        assert_eq!(back.channels, 2);
        assert_eq!(back.bits, 16);
        assert_eq!(back.data, samples);
    }

    #[test]
    fn pdf_has_a_valid_structure() {
        let pdf = pdf_write(
            &["BT /F1 12 Tf 72 720 Td (Hello) Tj ET".to_string()],
            595.0,
            842.0,
        );
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.starts_with("%PDF-1.4"));
        assert!(text.contains("/Type /Catalog"));
        assert!(text.contains("xref"));
        assert!(text.ends_with("%%EOF\n"));
    }
}
