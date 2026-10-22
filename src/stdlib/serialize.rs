//! Binary serialization: MessagePack, CBOR and the protocol-buffers wire
//! format.
//!
//! All three map the same value model (null/bool/int/float/string/bytes/list/
//! map) onto bytes, so one set of helpers does the traversal. There is no
//! `.proto` compiler — `pb_encode`/`pb_decode` speak the wire format directly
//! (field number -> value), which is what a schema-less client needs and what
//! a generated stub would emit anyway.
//!
//! Nesting is capped at `MAX_DEPTH` so a cyclic list is an error, not a hang.

use crate::value::collections::seq::{make_list, MapData};
use crate::value::{new_ref, MapKey, NativeCtx, Value};

const MAX_DEPTH: usize = 128;

fn entries_of(m: &std::sync::Arc<std::sync::Mutex<MapData>>) -> Vec<(MapKey, Value)> {
    m.lock().unwrap().entries.clone()
}

fn build_map(pairs: Vec<(MapKey, Value)>) -> Value {
    Value::Map(new_ref(MapData::from_entries(pairs)))
}

fn too_deep() -> String {
    format!("nesting deeper than {} levels (cyclic data?)", MAX_DEPTH)
}

// ------------------------------------------------------------------ msgpack

fn mp_put_str(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    let n = b.len();
    if n < 32 {
        out.push(0xa0 | n as u8);
    } else if n < 256 {
        out.push(0xd9);
        out.push(n as u8);
    } else if n < 65536 {
        out.push(0xda);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(0xdb);
        out.extend_from_slice(&(n as u32).to_be_bytes());
    }
    out.extend_from_slice(b);
}

fn mp_put_int(out: &mut Vec<u8>, v: i64) {
    if (0..128).contains(&v) {
        out.push(v as u8);
    } else if (-32..0).contains(&v) {
        out.push((v as i8) as u8);
    } else if v >= 0 {
        if v <= u8::MAX as i64 {
            out.push(0xcc);
            out.push(v as u8);
        } else if v <= u16::MAX as i64 {
            out.push(0xcd);
            out.extend_from_slice(&(v as u16).to_be_bytes());
        } else if v <= u32::MAX as i64 {
            out.push(0xce);
            out.extend_from_slice(&(v as u32).to_be_bytes());
        } else {
            out.push(0xcf);
            out.extend_from_slice(&(v as u64).to_be_bytes());
        }
    } else if v >= i8::MIN as i64 {
        out.push(0xd0);
        out.push((v as i8) as u8);
    } else if v >= i16::MIN as i64 {
        out.push(0xd1);
        out.extend_from_slice(&(v as i16).to_be_bytes());
    } else if v >= i32::MIN as i64 {
        out.push(0xd2);
        out.extend_from_slice(&(v as i32).to_be_bytes());
    } else {
        out.push(0xd3);
        out.extend_from_slice(&v.to_be_bytes());
    }
}

fn msgpack_encode_into(v: &Value, out: &mut Vec<u8>, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(too_deep());
    }
    match v {
        Value::Null => out.push(0xc0),
        Value::Bool(b) => out.push(if *b { 0xc3 } else { 0xc2 }),
        Value::Int(i) => mp_put_int(out, *i),
        Value::Float(f) => {
            out.push(0xcb);
            out.extend_from_slice(&f.to_be_bytes());
        }
        Value::Str(s) => mp_put_str(out, s),
        Value::Bytes(b) => {
            let b = b.lock().unwrap();
            let n = b.len();
            if n < 256 {
                out.push(0xc4);
                out.push(n as u8);
            } else if n < 65536 {
                out.push(0xc5);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            } else {
                out.push(0xc6);
                out.extend_from_slice(&(n as u32).to_be_bytes());
            }
            out.extend_from_slice(&b);
        }
        Value::List(l) => {
            let items = l.lock().unwrap().items.clone();
            let n = items.len();
            if n < 16 {
                out.push(0x90 | n as u8);
            } else if n < 65536 {
                out.push(0xdc);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            } else {
                out.push(0xdd);
                out.extend_from_slice(&(n as u32).to_be_bytes());
            }
            for it in items.iter() {
                msgpack_encode_into(it, out, depth + 1)?;
            }
        }
        Value::Map(m) => {
            let pairs = entries_of(m);
            let n = pairs.len();
            if n < 16 {
                out.push(0x80 | n as u8);
            } else if n < 65536 {
                out.push(0xde);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            } else {
                out.push(0xdf);
                out.extend_from_slice(&(n as u32).to_be_bytes());
            }
            for (k, val) in pairs.iter() {
                match k {
                    MapKey::Str(s) => mp_put_str(out, s),
                    MapKey::Int(i) => mp_put_int(out, *i),
                    MapKey::Bool(b) => out.push(if *b { 0xc3 } else { 0xc2 }),
                    MapKey::Null => out.push(0xc0),
                }
                msgpack_encode_into(val, out, depth + 1)?;
            }
        }
        other => {
            return Err(format!(
                "msgpack_encode: cannot encode a {} value",
                crate::value::type_name(other)
            ))
        }
    }
    Ok(())
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }
    fn byte(&mut self) -> Result<u8, String> {
        if self.pos >= self.data.len() {
            return Err("unexpected end of input".to_string());
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Ok(b)
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.pos + n > self.data.len() {
            return Err("unexpected end of input".to_string());
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u16be(&mut self) -> Result<u16, String> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32be(&mut self) -> Result<u32, String> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn u64be(&mut self) -> Result<u64, String> {
        let s = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Ok(u64::from_be_bytes(a))
    }
}

fn mp_key(v: Value) -> MapKey {
    match v {
        Value::Str(s) => MapKey::Str(s),
        Value::Int(i) => MapKey::Int(i),
        Value::Bool(b) => MapKey::Bool(b),
        _ => MapKey::Null,
    }
}

fn msgpack_decode_from(r: &mut Reader, depth: usize) -> Result<Value, String> {
    if depth > MAX_DEPTH {
        return Err(too_deep());
    }
    let b = r.byte()?;
    Ok(match b {
        0xc0 => Value::Null,
        0xc2 => Value::Bool(false),
        0xc3 => Value::Bool(true),
        0xc4 => {
            let n = r.byte()? as usize;
            Value::Bytes(new_ref(r.take(n)?.to_vec()))
        }
        0xc5 => {
            let n = r.u16be()? as usize;
            Value::Bytes(new_ref(r.take(n)?.to_vec()))
        }
        0xc6 => {
            let n = r.u32be()? as usize;
            Value::Bytes(new_ref(r.take(n)?.to_vec()))
        }
        0xca => {
            let bits = r.u32be()?;
            Value::Float(f32::from_bits(bits) as f64)
        }
        0xcb => {
            let bits = r.u64be()?;
            Value::Float(f64::from_bits(bits))
        }
        0xcc => Value::Int(r.byte()? as i64),
        0xcd => Value::Int(r.u16be()? as i64),
        0xce => Value::Int(r.u32be()? as i64),
        0xcf => Value::Int(r.u64be()? as i64),
        0xd0 => Value::Int(r.byte()? as i8 as i64),
        0xd1 => Value::Int(r.u16be()? as i16 as i64),
        0xd2 => Value::Int(r.u32be()? as i32 as i64),
        0xd3 => Value::Int(r.u64be()? as i64),
        0xd9 => {
            let n = r.byte()? as usize;
            let s = r.take(n)?;
            Value::Str(String::from_utf8_lossy(s).into())
        }
        0xda => {
            let n = r.u16be()? as usize;
            let s = r.take(n)?;
            Value::Str(String::from_utf8_lossy(s).into())
        }
        0xdb => {
            let n = r.u32be()? as usize;
            let s = r.take(n)?;
            Value::Str(String::from_utf8_lossy(s).into())
        }
        0xdc => {
            let n = r.u16be()? as usize;
            let mut items = Vec::with_capacity(n);
            for _ in 0..n {
                items.push(msgpack_decode_from(r, depth + 1)?);
            }
            make_list(items)
        }
        0xdd => {
            let n = r.u32be()? as usize;
            let mut items = Vec::with_capacity(n);
            for _ in 0..n {
                items.push(msgpack_decode_from(r, depth + 1)?);
            }
            make_list(items)
        }
        0xde => {
            let n = r.u16be()? as usize;
            let mut pairs = Vec::with_capacity(n);
            for _ in 0..n {
                let k = mp_key(msgpack_decode_from(r, depth + 1)?);
                let v = msgpack_decode_from(r, depth + 1)?;
                pairs.push((k, v));
            }
            build_map(pairs)
        }
        0xdf => {
            let n = r.u32be()? as usize;
            let mut pairs = Vec::with_capacity(n);
            for _ in 0..n {
                let k = mp_key(msgpack_decode_from(r, depth + 1)?);
                let v = msgpack_decode_from(r, depth + 1)?;
                pairs.push((k, v));
            }
            build_map(pairs)
        }
        _ => {
            if b < 0x80 {
                Value::Int(b as i64) // positive fixint
            } else if b >= 0xe0 {
                Value::Int(b as i8 as i64) // negative fixint
            } else if (0x80..0x90).contains(&b) {
                let n = (b & 0x0f) as usize;
                let mut pairs = Vec::with_capacity(n);
                for _ in 0..n {
                    let k = mp_key(msgpack_decode_from(r, depth + 1)?);
                    let v = msgpack_decode_from(r, depth + 1)?;
                    pairs.push((k, v));
                }
                build_map(pairs)
            } else if (0x90..0xa0).contains(&b) {
                let n = (b & 0x0f) as usize;
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    items.push(msgpack_decode_from(r, depth + 1)?);
                }
                make_list(items)
            } else if (0xa0..0xc0).contains(&b) {
                let n = (b & 0x1f) as usize;
                let s = r.take(n)?;
                Value::Str(String::from_utf8_lossy(s).into())
            } else {
                return Err(format!("msgpack: unsupported type byte 0x{:02x}", b));
            }
        }
    })
}

// ---------------------------------------------------------------------- cbor

fn cbor_head(out: &mut Vec<u8>, major: u8, value: u64) {
    let m = major << 5;
    if value < 24 {
        out.push(m | value as u8);
    } else if value < 256 {
        out.push(m | 24);
        out.push(value as u8);
    } else if value < 65536 {
        out.push(m | 25);
        out.extend_from_slice(&(value as u16).to_be_bytes());
    } else if value <= u32::MAX as u64 {
        out.push(m | 26);
        out.extend_from_slice(&(value as u32).to_be_bytes());
    } else {
        out.push(m | 27);
        out.extend_from_slice(&value.to_be_bytes());
    }
}

fn cbor_encode_into(v: &Value, out: &mut Vec<u8>, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(too_deep());
    }
    match v {
        Value::Null => out.push(0xf6),
        Value::Bool(b) => out.push(if *b { 0xf5 } else { 0xf4 }),
        Value::Int(i) => {
            if *i >= 0 {
                cbor_head(out, 0, *i as u64);
            } else {
                // Major type 1 encodes -(n+1).
                cbor_head(out, 1, (-1 - *i) as u64);
            }
        }
        Value::Float(f) => {
            out.push(0xfb);
            out.extend_from_slice(&f.to_be_bytes());
        }
        Value::Str(s) => {
            cbor_head(out, 3, s.len() as u64);
            out.extend_from_slice(s.as_bytes());
        }
        Value::Bytes(b) => {
            let b = b.lock().unwrap();
            cbor_head(out, 2, b.len() as u64);
            out.extend_from_slice(&b);
        }
        Value::List(l) => {
            let items = l.lock().unwrap().items.clone();
            cbor_head(out, 4, items.len() as u64);
            for it in items.iter() {
                cbor_encode_into(it, out, depth + 1)?;
            }
        }
        Value::Map(m) => {
            let pairs = entries_of(m);
            cbor_head(out, 5, pairs.len() as u64);
            for (k, val) in pairs.iter() {
                match k {
                    MapKey::Str(s) => {
                        cbor_head(out, 3, s.len() as u64);
                        out.extend_from_slice(s.as_bytes());
                    }
                    MapKey::Int(i) => {
                        if *i >= 0 {
                            cbor_head(out, 0, *i as u64);
                        } else {
                            cbor_head(out, 1, (-1 - *i) as u64);
                        }
                    }
                    MapKey::Bool(b) => out.push(if *b { 0xf5 } else { 0xf4 }),
                    MapKey::Null => out.push(0xf6),
                }
                cbor_encode_into(val, out, depth + 1)?;
            }
        }
        other => {
            return Err(format!(
                "cbor_encode: cannot encode a {} value",
                crate::value::type_name(other)
            ))
        }
    }
    Ok(())
}

fn cbor_read_head(r: &mut Reader) -> Result<(u8, u64), String> {
    let b = r.byte()?;
    let major = b >> 5;
    let info = b & 0x1f;
    let value = match info {
        0..=23 => info as u64,
        24 => r.byte()? as u64,
        25 => r.u16be()? as u64,
        26 => r.u32be()? as u64,
        27 => r.u64be()?,
        31 => {
            return Err("cbor: indefinite-length items are not supported".to_string());
        }
        _ => return Err(format!("cbor: reserved additional info {}", info)),
    };
    Ok((major, value))
}

fn cbor_decode_from(r: &mut Reader, depth: usize) -> Result<Value, String> {
    if depth > MAX_DEPTH {
        return Err(too_deep());
    }
    let b = r
        .data
        .get(r.pos)
        .copied()
        .ok_or("unexpected end of input")?;
    // Simple values are not head-encoded.
    if b == 0xf4 {
        r.pos += 1;
        return Ok(Value::Bool(false));
    }
    if b == 0xf5 {
        r.pos += 1;
        return Ok(Value::Bool(true));
    }
    if b == 0xf6 || b == 0xf7 {
        r.pos += 1;
        return Ok(Value::Null);
    }
    if b == 0xfb {
        r.pos += 1;
        return Ok(Value::Float(f64::from_bits(r.u64be()?)));
    }
    if b == 0xfa {
        r.pos += 1;
        return Ok(Value::Float(f32::from_bits(r.u32be()?) as f64));
    }
    let (major, value) = cbor_read_head(r)?;
    Ok(match major {
        0 => Value::Int(value as i64),
        1 => Value::Int(-1 - value as i64),
        2 => {
            let n = value as usize;
            Value::Bytes(new_ref(r.take(n)?.to_vec()))
        }
        3 => {
            let n = value as usize;
            Value::Str(String::from_utf8_lossy(r.take(n)?).into())
        }
        4 => {
            let n = value as usize;
            let mut items = Vec::with_capacity(n);
            for _ in 0..n {
                items.push(cbor_decode_from(r, depth + 1)?);
            }
            make_list(items)
        }
        5 => {
            let n = value as usize;
            let mut pairs = Vec::with_capacity(n);
            for _ in 0..n {
                let k = mp_key(cbor_decode_from(r, depth + 1)?);
                let v = cbor_decode_from(r, depth + 1)?;
                pairs.push((k, v));
            }
            build_map(pairs)
        }
        7 => {
            return Err("cbor: unsupported simple/float value".to_string());
        }
        _ => return Err(format!("cbor: unsupported major type {}", major)),
    })
}

// ------------------------------------------------------------------ protobuf

fn pb_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn pb_encode_field(out: &mut Vec<u8>, field: u64, v: &Value, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(too_deep());
    }
    match v {
        Value::Int(i) => {
            pb_varint(out, field << 3);
            pb_varint(out, *i as u64);
        }
        Value::Bool(b) => {
            pb_varint(out, field << 3);
            pb_varint(out, if *b { 1 } else { 0 });
        }
        Value::Float(f) => {
            pb_varint(out, (field << 3) | 1);
            out.extend_from_slice(&f.to_le_bytes());
        }
        Value::Str(s) => {
            pb_varint(out, (field << 3) | 2);
            pb_varint(out, s.len() as u64);
            out.extend_from_slice(s.as_bytes());
        }
        Value::Bytes(b) => {
            let b = b.lock().unwrap();
            pb_varint(out, (field << 3) | 2);
            pb_varint(out, b.len() as u64);
            out.extend_from_slice(&b);
        }
        Value::List(l) => {
            // A list becomes repeated occurrences of the field.
            let items = l.lock().unwrap().items.clone();
            for it in items.iter() {
                pb_encode_field(out, field, it, depth + 1)?;
            }
        }
        Value::Map(m) => {
            // A nested map becomes a length-delimited submessage.
            let mut inner = Vec::new();
            for (k, val) in entries_of(m).iter() {
                let f = match k {
                    MapKey::Int(i) if *i > 0 => *i as u64,
                    MapKey::Str(s) => s
                        .parse::<u64>()
                        .map_err(|_| format!("pb_encode: field name '{}' is not a number", s))?,
                    _ => return Err("pb_encode: field numbers must be positive ints".to_string()),
                };
                pb_encode_field(&mut inner, f, val, depth + 1)?;
            }
            pb_varint(out, (field << 3) | 2);
            pb_varint(out, inner.len() as u64);
            out.extend_from_slice(&inner);
        }
        Value::Null => {}
        other => {
            return Err(format!(
                "pb_encode: cannot encode a {} value",
                crate::value::type_name(other)
            ))
        }
    }
    Ok(())
}

fn pb_read_varint(r: &mut Reader) -> Result<u64, String> {
    let mut shift = 0u32;
    let mut out = 0u64;
    loop {
        let b = r.byte()?;
        out |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok(out);
        }
        shift += 7;
        if shift >= 64 {
            return Err("pb_decode: varint is too long".to_string());
        }
    }
}

/// Decode into `{field_number: [values...]}` — repeated fields accumulate,
/// and a nested message comes back as `bytes` (call `pb_decode` again).
fn pb_decode_into(
    r: &mut Reader,
    out: &mut Vec<(MapKey, Value)>,
    depth: usize,
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(too_deep());
    }
    while r.pos < r.data.len() {
        let key = pb_read_varint(r)?;
        let field = key >> 3;
        let wire = (key & 7) as u8;
        if field == 0 {
            return Err("pb_decode: field number 0 is invalid".to_string());
        }
        let value = match wire {
            0 => Value::Int(pb_read_varint(r)? as i64),
            1 => {
                let s = r.take(8)?;
                let mut a = [0u8; 8];
                a.copy_from_slice(s);
                Value::Float(f64::from_le_bytes(a))
            }
            2 => {
                let n = pb_read_varint(r)? as usize;
                Value::Bytes(new_ref(r.take(n)?.to_vec()))
            }
            5 => {
                let s = r.take(4)?;
                Value::Float(f32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f64)
            }
            other => {
                return Err(format!("pb_decode: unsupported wire type {}", other));
            }
        };
        out.push((MapKey::Int(field as i64), value));
    }
    Ok(())
}

// ------------------------------------------------------------------- natives

fn n_msgpack_encode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let v = args
        .into_iter()
        .next()
        .ok_or_else(|| "msgpack_encode expects 1 argument".to_string())?;
    let mut out = Vec::new();
    msgpack_encode_into(&v, &mut out, 0)?;
    Ok(Value::Bytes(new_ref(out)))
}

fn n_msgpack_decode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!(
            "msgpack_decode expects 1 argument, got {}",
            args.len()
        ));
    }
    let data = bytes_arg(&args[0], "msgpack_decode")?;
    let mut r = Reader::new(&data);
    match msgpack_decode_from(&mut r, 0) {
        Ok(v) => Ok(Value::Result(Ok(Box::new(v)))),
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(e.into()))))),
    }
}

fn n_cbor_encode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    let v = args
        .into_iter()
        .next()
        .ok_or_else(|| "cbor_encode expects 1 argument".to_string())?;
    let mut out = Vec::new();
    cbor_encode_into(&v, &mut out, 0)?;
    Ok(Value::Bytes(new_ref(out)))
}

fn n_cbor_decode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!(
            "cbor_decode expects 1 argument, got {}",
            args.len()
        ));
    }
    let data = bytes_arg(&args[0], "cbor_decode")?;
    let mut r = Reader::new(&data);
    match cbor_decode_from(&mut r, 0) {
        Ok(v) => Ok(Value::Result(Ok(Box::new(v)))),
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(e.into()))))),
    }
}

fn n_pb_encode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("pb_encode expects 1 argument, got {}", args.len()));
    }
    let pairs = match &args[0] {
        Value::Map(m) => entries_of(m),
        other => {
            return Err(format!(
                "pb_encode expects a map of field numbers, found {}",
                crate::value::type_name(other)
            ))
        }
    };
    let mut out = Vec::new();
    for (k, v) in pairs.iter() {
        let field = match k {
            MapKey::Int(i) if *i > 0 => *i as u64,
            MapKey::Str(s) => s
                .parse::<u64>()
                .map_err(|_| format!("pb_encode: field '{}' is not a number", s))?,
            _ => return Err("pb_encode: field numbers must be positive ints".to_string()),
        };
        pb_encode_field(&mut out, field, v, 0)?;
    }
    Ok(Value::Bytes(new_ref(out)))
}

fn n_pb_decode(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    if args.len() != 1 {
        return Err(format!("pb_decode expects 1 argument, got {}", args.len()));
    }
    let data = bytes_arg(&args[0], "pb_decode")?;
    let mut r = Reader::new(&data);
    let mut pairs: Vec<(MapKey, Value)> = Vec::new();
    match pb_decode_into(&mut r, &mut pairs, 0) {
        Ok(()) => {
            // Group repeated fields into lists, keeping field order.
            let mut grouped: Vec<(MapKey, Value)> = Vec::new();
            let mut order: Vec<i64> = Vec::new();
            for (k, v) in pairs {
                let field = match k {
                    MapKey::Int(i) => i,
                    _ => 0,
                };
                if let Some(pos) = order.iter().position(|f| *f == field) {
                    if let Value::List(l) = &grouped[pos].1 {
                        l.lock().unwrap().items.push(v);
                    }
                } else {
                    order.push(field);
                    grouped.push((MapKey::Int(field), make_list(vec![v])));
                }
            }
            Ok(Value::Result(Ok(Box::new(build_map(grouped)))))
        }
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(e.into()))))),
    }
}

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

pub fn native_defs() -> Vec<crate::stdlib::NativeDef> {
    vec![
        ("msgpack_encode", n_msgpack_encode),
        ("msgpack_decode", n_msgpack_decode),
        ("cbor_encode", n_cbor_encode),
        ("cbor_decode", n_cbor_decode),
        ("pb_encode", n_pb_encode),
        ("pb_decode", n_pb_decode),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msgpack_fixed_encodings() {
        // Spec examples: 0x00 -> 0, 0xcc 0x80 -> 128, 0xa3 'a' 'b' 'c' -> "abc".
        let mut out = Vec::new();
        msgpack_encode_into(&Value::Int(0), &mut out, 0).unwrap();
        assert_eq!(out, vec![0x00]);

        let mut out = Vec::new();
        msgpack_encode_into(&Value::Int(128), &mut out, 0).unwrap();
        assert_eq!(out, vec![0xcc, 0x80]);

        let mut out = Vec::new();
        msgpack_encode_into(&Value::Str("abc".into()), &mut out, 0).unwrap();
        assert_eq!(out, vec![0xa3, b'a', b'b', b'c']);
    }

    #[test]
    fn cbor_fixed_encodings() {
        // RFC 8949 examples: 0 -> 0x00, 23 -> 0x17, 24 -> 0x18 0x18.
        let mut out = Vec::new();
        cbor_encode_into(&Value::Int(0), &mut out, 0).unwrap();
        assert_eq!(out, vec![0x00]);
        let mut out = Vec::new();
        cbor_encode_into(&Value::Int(23), &mut out, 0).unwrap();
        assert_eq!(out, vec![0x17]);
        let mut out = Vec::new();
        cbor_encode_into(&Value::Int(24), &mut out, 0).unwrap();
        assert_eq!(out, vec![0x18, 0x18]);
        // -1 -> 0x20
        let mut out = Vec::new();
        cbor_encode_into(&Value::Int(-1), &mut out, 0).unwrap();
        assert_eq!(out, vec![0x20]);
    }

    #[test]
    fn pb_varint_roundtrip() {
        for v in [0u64, 1, 127, 128, 300, 16384, u32::MAX as u64, u64::MAX] {
            let mut out = Vec::new();
            pb_varint(&mut out, v);
            let mut r = Reader::new(&out);
            assert_eq!(pb_read_varint(&mut r).unwrap(), v);
        }
    }
}
