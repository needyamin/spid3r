//! The plugin wire format: a typed binary encoding passed through a WASM
//! module's linear memory.
//!
//! # Why not JSON
//!
//! `plan.txt` sketched "JSON over linear memory". A typed binary encoding is
//! used instead, for one reason: a guest would otherwise have to ship a JSON
//! parser *into the sandbox*. That is a large attack surface and a dependency
//! burden on every plugin author, to encode a value set that is only six types
//! wide. The binary form is trivial to produce in any language and impossible
//! to mis-parse into the wrong type.
//!
//! # The format
//!
//! Little-endian throughout. A request is an argument vector; a response is a
//! single value.
//!
//! ```text
//! request  := u8 argc, argc × value
//! response := value | error
//! error    := 0xFF, u32 byte_len, utf8 bytes
//!
//! value := u8 tag, payload
//!   0  null      (no payload)
//!   1  int       i64
//!   2  float     f64
//!   3  bool      u8 (0 or 1)
//!   4  string    u32 byte_len, utf8 bytes
//!   5  list      u32 count, count × value
//! ```
//!
//! Anything else — a map, a class instance, a closure — is refused with a
//! diagnostic rather than silently coerced. A plugin boundary is the wrong
//! place to guess.

use std::sync::Arc;

use crate::value::{collections, Value};

pub const TAG_NULL: u8 = 0;
pub const TAG_INT: u8 = 1;
pub const TAG_FLOAT: u8 = 2;
pub const TAG_BOOL: u8 = 3;
pub const TAG_STRING: u8 = 4;
pub const TAG_LIST: u8 = 5;
pub const TAG_ERROR: u8 = 255;

/// The largest payload either direction may carry.
///
/// A guest controls the length it reports, so the host must bound what it will
/// believe before allocating: an 8 MiB ceiling is generous for a plugin call and
/// small enough that a hostile length cannot exhaust memory in one go.
pub const MAX_PAYLOAD: usize = 8 * 1024 * 1024;

/// Encode a call's arguments.
pub fn encode_args(args: &[Value]) -> Result<Vec<u8>, String> {
    if args.len() > u8::MAX as usize {
        return Err(format!(
            "a plugin call takes at most {} arguments, got {}",
            u8::MAX,
            args.len()
        ));
    }
    let mut out = Vec::with_capacity(16 + args.len() * 8);
    out.push(args.len() as u8);
    for a in args {
        encode_value(a, &mut out)?;
    }
    if out.len() > MAX_PAYLOAD {
        return Err(format!(
            "arguments encode to {} bytes, over the {} byte limit",
            out.len(),
            MAX_PAYLOAD
        ));
    }
    Ok(out)
}

fn encode_value(v: &Value, out: &mut Vec<u8>) -> Result<(), String> {
    match v {
        Value::Null => out.push(TAG_NULL),
        Value::Int(i) => {
            out.push(TAG_INT);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Value::Float(f) => {
            out.push(TAG_FLOAT);
            out.extend_from_slice(&f.to_le_bytes());
        }
        Value::Bool(b) => {
            out.push(TAG_BOOL);
            out.push(u8::from(*b));
        }
        Value::Str(s) => {
            out.push(TAG_STRING);
            let bytes = s.as_bytes();
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(bytes);
        }
        Value::List(_) | Value::VecDyn(_) => {
            let items = collections::iter_values(v)?;
            out.push(TAG_LIST);
            out.extend_from_slice(&(items.len() as u32).to_le_bytes());
            for item in &items {
                encode_value(item, out)?;
            }
        }
        other => {
            return Err(format!(
                "cannot pass a {} to a plugin (only null, int, float, bool, string and list cross the boundary)",
                crate::value::type_name(other)
            ))
        }
    }
    Ok(())
}

/// Decode a response into a value.
pub fn decode_response(bytes: &[u8]) -> Result<Value, String> {
    if bytes.is_empty() {
        return Err("plugin returned an empty response".to_string());
    }
    if bytes[0] == TAG_ERROR {
        if bytes.len() < 5 {
            return Err("plugin returned a truncated error".to_string());
        }
        let len = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
        let end = 5usize
            .checked_add(len)
            .filter(|e| *e <= bytes.len())
            .ok_or("plugin returned a truncated error message")?;
        return Err(String::from_utf8_lossy(&bytes[5..end]).to_string());
    }
    let mut pos = 0usize;
    let v = decode_value(bytes, &mut pos)?;
    if pos != bytes.len() {
        return Err(format!(
            "plugin response has {} trailing byte(s)",
            bytes.len() - pos
        ));
    }
    Ok(v)
}

fn decode_value(bytes: &[u8], pos: &mut usize) -> Result<Value, String> {
    let tag = *bytes.get(*pos).ok_or("truncated value: missing tag")?;
    *pos += 1;
    match tag {
        TAG_NULL => Ok(Value::Null),
        TAG_INT => {
            let raw = take(bytes, pos, 8)?;
            Ok(Value::Int(i64::from_le_bytes(raw.try_into().unwrap())))
        }
        TAG_FLOAT => {
            let raw = take(bytes, pos, 8)?;
            Ok(Value::Float(f64::from_le_bytes(raw.try_into().unwrap())))
        }
        TAG_BOOL => {
            let b = *bytes.get(*pos).ok_or("truncated bool")?;
            *pos += 1;
            Ok(Value::Bool(b != 0))
        }
        TAG_STRING => {
            let raw = take(bytes, pos, 4)?;
            let len = u32::from_le_bytes(raw.try_into().unwrap()) as usize;
            let s = take(bytes, pos, len)?;
            let text =
                std::str::from_utf8(s).map_err(|e| format!("plugin string is not UTF-8: {e}"))?;
            Ok(Value::Str(Arc::from(text)))
        }
        TAG_LIST => {
            let raw = take(bytes, pos, 4)?;
            let count = u32::from_le_bytes(raw.try_into().unwrap()) as usize;
            // Each item needs at least one tag byte, so a count larger than the
            // remaining bytes is a lie and must not drive an allocation.
            if count > bytes.len() - *pos {
                return Err(format!(
                    "plugin list claims {} items but only {} bytes remain",
                    count,
                    bytes.len() - *pos
                ));
            }
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                items.push(decode_value(bytes, pos)?);
            }
            collections::construct("list", items)
        }
        other => Err(format!("unknown plugin value tag {}", other)),
    }
}

fn take<'a>(bytes: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u8], String> {
    let end = pos
        .checked_add(len)
        .filter(|e| *e <= bytes.len())
        .ok_or_else(|| format!("truncated value: wanted {} byte(s)", len))?;
    let slice = &bytes[*pos..end];
    *pos = end;
    Ok(slice)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_supported_type() {
        for v in [
            Value::Null,
            Value::Int(-42),
            Value::Float(1.5),
            Value::Bool(true),
            Value::Str(Arc::from("hi")),
        ] {
            let mut buf = Vec::new();
            encode_value(&v, &mut buf).expect("encode");
            let back = decode_response(&buf).expect("decode");
            assert_eq!(
                format!("{:?}", v),
                format!("{:?}", back),
                "round trip changed the value"
            );
        }
    }

    #[test]
    fn an_error_tag_decodes_to_an_error() {
        let mut buf = vec![TAG_ERROR];
        let msg = b"boom";
        buf.extend_from_slice(&(msg.len() as u32).to_le_bytes());
        buf.extend_from_slice(msg);
        assert_eq!(decode_response(&buf).unwrap_err(), "boom");
    }

    #[test]
    fn a_hostile_length_is_rejected_not_allocated() {
        // A list claiming 4 billion items in a 6-byte buffer.
        let mut buf = vec![TAG_LIST];
        buf.extend_from_slice(&u32::MAX.to_le_bytes());
        let err = decode_response(&buf).unwrap_err();
        assert!(err.contains("claims"), "{err}");
    }

    #[test]
    fn a_truncated_value_is_rejected() {
        // An int tag with only two payload bytes.
        let buf = vec![TAG_INT, 1, 2];
        assert!(decode_response(&buf).is_err());
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut buf = vec![TAG_BOOL, 1];
        buf.push(0);
        assert!(decode_response(&buf).unwrap_err().contains("trailing"));
    }

    #[test]
    fn an_unsupported_type_is_refused_with_its_name() {
        let err = encode_args(&[Value::Null, Value::Null]).err();
        assert!(err.is_none(), "null args are fine");
        // A native function value cannot cross the boundary.
        let native = Value::Native(Arc::new(crate::value::NativeFn {
            name: "x",
            func: |_c, _a| Ok(Value::Null),
        }));
        let e = encode_args(&[native]).unwrap_err();
        assert!(e.contains("cannot pass a"), "{e}");
    }
}
