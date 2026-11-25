//! Pure-Rust, zero-dependency ICO and PNG icon decoder for `tao::window::Icon`.
//!
//! Avoids adding image-decoding crates while supporting both `.ico` (PNG-encoded
//! and 24/32-bit BMP DIB entries) and `.png` (8-bit RGB, RGBA, Grayscale,
//! Grayscale+Alpha, and indexed palette) icons from disk or embedded payloads.

use crate::stdlib::compress::unzlib_bytes;

const PNG_SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const MAX_ICON_DIM: u32 = 2048;

/// Decode a `.png` or `.ico` byte buffer into `(rgba_pixels, width, height)`.
pub fn decode_icon_bytes(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    if bytes.starts_with(PNG_SIG) {
        decode_png(bytes)
    } else if bytes.len() >= 6
        && u16::from_le_bytes([bytes[0], bytes[1]]) == 0
        && u16::from_le_bytes([bytes[2], bytes[3]]) == 1
    {
        decode_ico(bytes)
    } else {
        Err("unsupported icon format (expected .ico or .png)".to_string())
    }
}

/// Decode bytes into a `tao::window::Icon`.
pub fn load_tao_icon(bytes: &[u8]) -> Result<tao::window::Icon, String> {
    let (rgba, w, h) = decode_icon_bytes(bytes)?;
    tao::window::Icon::from_rgba(rgba, w, h).map_err(|e| format!("invalid icon pixels: {e}"))
}

fn decode_ico(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    if bytes.len() < 6 {
        return Err("truncated .ico header".to_string());
    }
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    if count == 0 {
        return Err(".ico contains no images".to_string());
    }
    let dir_end = 6usize
        .checked_add(count.checked_mul(16).ok_or("ico entry count overflow")?)
        .ok_or("ico header overflow")?;
    if bytes.len() < dir_end {
        return Err("truncated .ico directory".to_string());
    }

    // Pick the best entry: prefer largest dimension up to 256px and highest bpp.
    let mut best: Option<(u32, u16, usize, usize)> = None;
    for idx in 0..count {
        let off = 6 + idx * 16;
        let w = match bytes[off] {
            0 => 256u32,
            n => n as u32,
        };
        let h = match bytes[off + 1] {
            0 => 256u32,
            n => n as u32,
        };
        let bpp = u16::from_le_bytes([bytes[off + 6], bytes[off + 7]]);
        let size = u32::from_le_bytes([
            bytes[off + 8],
            bytes[off + 9],
            bytes[off + 10],
            bytes[off + 11],
        ]) as usize;
        let data_off = u32::from_le_bytes([
            bytes[off + 12],
            bytes[off + 13],
            bytes[off + 14],
            bytes[off + 15],
        ]) as usize;
        let Some(end) = data_off.checked_add(size) else {
            continue;
        };
        if end > bytes.len() || size == 0 {
            continue;
        }
        let area = w.saturating_mul(h);
        // PNG entries in ICO directories frequently record bpp == 0 (color depth
        // lives inside the PNG IHDR), so treat 0 as 32-bit when ranking.
        let eff_bpp = if bpp == 0 { 32 } else { bpp };
        let better = match best {
            None => true,
            Some((best_area, best_bpp, _, _)) => {
                area > best_area || (area == best_area && eff_bpp > best_bpp)
            }
        };
        if better {
            best = Some((area, eff_bpp, data_off, end));
        }
    }

    let (_, _, start, end) = best.ok_or_else(|| "no valid image entry in .ico".to_string())?;
    let entry = &bytes[start..end];
    if entry.starts_with(PNG_SIG) {
        return decode_png(entry);
    }
    decode_ico_bmp_dib(entry)
}

fn decode_ico_bmp_dib(dib: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    if dib.len() < 40 {
        return Err("truncated BMP DIB in .ico".to_string());
    }
    let header_size = u32::from_le_bytes([dib[0], dib[1], dib[2], dib[3]]) as usize;
    if header_size < 40 || header_size > dib.len() {
        return Err("invalid BMP DIB header size in .ico".to_string());
    }
    let width = i32::from_le_bytes([dib[4], dib[5], dib[6], dib[7]]);
    let height_raw = i32::from_le_bytes([dib[8], dib[9], dib[10], dib[11]]);
    let bpp = u16::from_le_bytes([dib[14], dib[15]]);
    let compression = u32::from_le_bytes([dib[16], dib[17], dib[18], dib[19]]);
    if compression != 0 {
        return Err(format!(
            "unsupported compressed BMP DIB ({compression}) in .ico"
        ));
    }
    let top_down = height_raw < 0;
    let height = (height_raw.unsigned_abs()) / 2;
    if width <= 0 || height == 0 || (width as u32) > MAX_ICON_DIM || height > MAX_ICON_DIM {
        return Err("invalid BMP DIB dimensions in .ico".to_string());
    }
    let w = width as u32;
    let h = height;
    let pixels = &dib[header_size..];

    match bpp {
        32 => {
            let row_bytes = (w as usize) * 4;
            let needed = row_bytes
                .checked_mul(h as usize)
                .ok_or("ico pixel size overflow")?;
            if pixels.len() < needed {
                return Err("truncated 32-bit BMP DIB pixels in .ico".to_string());
            }
            let mut rgba = vec![0u8; needed];
            for y in 0..h as usize {
                let src_y = if top_down { y } else { (h as usize - 1) - y };
                let src_row = &pixels[src_y * row_bytes..(src_y + 1) * row_bytes];
                let dst_row = &mut rgba[y * row_bytes..(y + 1) * row_bytes];
                for x in 0..w as usize {
                    let s = x * 4;
                    dst_row[s] = src_row[s + 2];
                    dst_row[s + 1] = src_row[s + 1];
                    dst_row[s + 2] = src_row[s];
                    dst_row[s + 3] = src_row[s + 3];
                }
            }
            Ok((rgba, w, h))
        }
        24 => {
            let xor_stride = ((w as usize * 3) + 3) & !3;
            let xor_size = xor_stride
                .checked_mul(h as usize)
                .ok_or("ico 24-bit size overflow")?;
            if pixels.len() < xor_size {
                return Err("truncated 24-bit BMP DIB pixels in .ico".to_string());
            }
            let and_stride = (w as usize).div_ceil(32) * 4;
            let and_mask = pixels.get(xor_size..xor_size + and_stride * h as usize);
            let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
            for y in 0..h as usize {
                let src_y = if top_down { y } else { (h as usize - 1) - y };
                let src_row = &pixels[src_y * xor_stride..(src_y + 1) * xor_stride];
                for x in 0..w as usize {
                    let d = (y * w as usize + x) * 4;
                    let s = x * 3;
                    rgba[d] = src_row[s + 2];
                    rgba[d + 1] = src_row[s + 1];
                    rgba[d + 2] = src_row[s];
                    let alpha = if let Some(mask) = and_mask {
                        let byte = mask[src_y * and_stride + (x / 8)];
                        if (byte & (0x80 >> (x % 8))) != 0 {
                            0
                        } else {
                            255
                        }
                    } else {
                        255
                    };
                    rgba[d + 3] = alpha;
                }
            }
            Ok((rgba, w, h))
        }
        other => Err(format!("unsupported {other}-bit BMP DIB in .ico")),
    }
}

fn decode_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    if bytes.len() < 8 || !bytes.starts_with(PNG_SIG) {
        return Err("invalid PNG signature".to_string());
    }
    let mut pos = 8usize;
    let mut width = 0u32;
    let mut height = 0u32;
    let mut color_type = 0u8;
    let mut have_ihdr = false;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    let mut idat: Vec<u8> = Vec::new();

    while pos + 12 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        let kind = &bytes[pos + 4..pos + 8];
        let data_start = pos + 8;
        let Some(data_end) = data_start.checked_add(len) else {
            return Err("PNG chunk length overflow".to_string());
        };
        let Some(next_pos) = data_end.checked_add(4) else {
            return Err("PNG chunk CRC overflow".to_string());
        };
        if next_pos > bytes.len() {
            return Err("truncated PNG chunk".to_string());
        }
        let chunk = &bytes[data_start..data_end];
        match kind {
            b"IHDR" => {
                if chunk.len() != 13 {
                    return Err("invalid PNG IHDR length".to_string());
                }
                width = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                height = u32::from_be_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
                let bit_depth = chunk[8];
                color_type = chunk[9];
                let compression = chunk[10];
                let filter = chunk[11];
                let interlace = chunk[12];
                if width == 0 || height == 0 || width > MAX_ICON_DIM || height > MAX_ICON_DIM {
                    return Err("invalid PNG dimensions".to_string());
                }
                if compression != 0 || filter != 0 || interlace != 0 {
                    return Err("unsupported PNG compression/filter/interlace mode".to_string());
                }
                if bit_depth != 8 {
                    return Err(format!(
                        "unsupported PNG bit depth {bit_depth} (expected 8)"
                    ));
                }
                have_ihdr = true;
            }
            b"PLTE" => {
                palette.clear();
                for rgb in chunk.as_chunks::<3>().0 {
                    palette.push(*rgb);
                }
            }
            b"tRNS" => {
                trns = chunk.to_vec();
            }
            b"IDAT" => {
                idat.extend_from_slice(chunk);
            }
            b"IEND" => break,
            _ => {}
        }
        pos = next_pos;
    }

    if !have_ihdr {
        return Err("PNG missing IHDR".to_string());
    }
    let bpp: usize = match color_type {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        other => return Err(format!("unsupported PNG color type {other}")),
    };
    let raw = unzlib_bytes(&idat).map_err(|e| format!("PNG inflate failed: {e}"))?;
    let stride = (width as usize) * bpp;
    let expected = (stride + 1)
        .checked_mul(height as usize)
        .ok_or("PNG image size overflow")?;
    if raw.len() < expected {
        return Err("truncated PNG pixel data".to_string());
    }

    let mut prev = vec![0u8; stride];
    let mut cur = vec![0u8; stride];
    let mut rgba = Vec::with_capacity((width as usize) * (height as usize) * 4);

    for y in 0..height as usize {
        let row_in = &raw[y * (stride + 1)..(y + 1) * (stride + 1)];
        let filter = row_in[0];
        let scan = &row_in[1..];
        for i in 0..stride {
            let a = if i >= bpp { cur[i - bpp] } else { 0 };
            let b = prev[i];
            let c = if i >= bpp { prev[i - bpp] } else { 0 };
            cur[i] = match filter {
                0 => scan[i],
                1 => scan[i].wrapping_add(a),
                2 => scan[i].wrapping_add(b),
                3 => scan[i].wrapping_add(((a as u16 + b as u16) >> 1) as u8),
                4 => scan[i].wrapping_add(paeth(a, b, c)),
                other => return Err(format!("unsupported PNG filter {other}")),
            };
        }
        match color_type {
            0 => {
                for &g in &cur {
                    rgba.extend_from_slice(&[g, g, g, 255]);
                }
            }
            2 => {
                for px in cur.as_chunks::<3>().0 {
                    rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
                }
            }
            3 => {
                for &idx in &cur {
                    let rgb = palette.get(idx as usize).copied().unwrap_or([0, 0, 0]);
                    let a = trns.get(idx as usize).copied().unwrap_or(255);
                    rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], a]);
                }
            }
            4 => {
                for px in cur.as_chunks::<2>().0 {
                    rgba.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
                }
            }
            6 => {
                rgba.extend_from_slice(&cur);
            }
            _ => unreachable!(),
        }
        std::mem::swap(&mut prev, &mut cur);
    }

    Ok((rgba, width, height))
}

#[inline]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let a = a as i32;
    let b = b as i32;
    let c = c as i32;
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_shipped_spid3r_ico() {
        let path = std::path::Path::new("packaging/nsis/assets/spid3r.ico");
        if path.is_file() {
            let bytes = std::fs::read(path).expect("read spid3r.ico");
            let (rgba, w, h) = decode_icon_bytes(&bytes).expect("decode spid3r.ico");
            assert!(w > 0 && h > 0);
            assert_eq!(rgba.len(), (w * h * 4) as usize);
        }
    }

    #[test]
    fn rejects_invalid_icon_bytes() {
        assert!(decode_icon_bytes(b"not an icon").is_err());
    }
}
