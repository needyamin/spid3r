//! `multipart/form-data` parsing and building (RFC 7578).
//!
//! This is a native rather than a script module for one reason: an uploaded
//! file is **bytes**, and every script-level operation available today
//! (`tcp_read`, `http_read_request`'s `body`) decodes as UTF-8 and replaces
//! invalid sequences with U+FFFD. A PNG would arrive corrupted and the bug
//! would only show up in production. `multipart_parse` therefore takes the
//! request's `body_bytes` and returns each part's content as `Bytes`, with a
//! lossy `text` view alongside it for the common case of form fields.
//!
//! The scanner is deliberately strict about framing (CRLF, `--boundary`,
//! closing `--`) and lenient about everything else, which is what every real
//! browser and HTTP client produces:
//!
//! ```text
//! --boundary CRLF
//! Content-Disposition: form-data; name="file"; filename="a.png" CRLF
//! Content-Type: image/png CRLF
//! CRLF
//! <bytes> CRLF
//! --boundary-- CRLF
//! ```

use super::NativeDef;
use crate::value::collections;
use crate::value::{new_ref, type_name, NativeCtx, Value};

/// More parts than this in one body is a client bug or an attack, not a form.
const MAX_PARTS: usize = 4096;

pub fn native_defs() -> Vec<NativeDef> {
    vec![
        ("multipart_parse", n_parse),
        ("__multipart_parse", n_parse),
        ("multipart_build", n_build),
        ("__multipart_build", n_build),
    ]
}

// ------------------------------------------------------------ helpers

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    let last = hay.len() - needle.len();
    let mut i = from;
    while i <= last {
        if &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn str_val(s: impl Into<String>) -> Value {
    Value::Str(s.into().into())
}

fn map_of(pairs: Vec<(&str, Value)>) -> Value {
    let mut flat = Vec::with_capacity(pairs.len() * 2);
    for (k, v) in pairs {
        flat.push(str_val(k));
        flat.push(v);
    }
    collections::construct("map", flat).expect("map construction cannot fail")
}

fn ok(v: Value) -> Result<Value, String> {
    collections::construct("ok", vec![v])
}

fn err(msg: impl Into<String>) -> Result<Value, String> {
    collections::construct("err", vec![str_val(msg.into())])
}

/// Bytes out of a `Bytes` or a `Str` (the latter already went through UTF-8
/// on the way in, which is why callers should pass `body_bytes`).
fn as_bytes(v: &Value, who: &str) -> Result<Vec<u8>, String> {
    match v {
        Value::Bytes(b) => Ok(b.lock().unwrap().clone()),
        Value::Str(s) => Ok(s.as_bytes().to_vec()),
        other => Err(format!(
            "{} expects bytes or a string, found {}",
            who,
            type_name(other)
        )),
    }
}

/// The boundary parameter of a `multipart/*` content type.
fn boundary_of(content_type: &str) -> Result<String, String> {
    let lower = content_type.to_ascii_lowercase();
    if !lower.starts_with("multipart/") {
        return Err(format!(
            "not a multipart content type: '{}'",
            content_type.trim()
        ));
    }
    for (key, value) in params(content_type) {
        if key == "boundary" {
            if value.is_empty() || value.len() > 70 {
                return Err("multipart boundary is empty or too long".to_string());
            }
            return Ok(value);
        }
    }
    Err("multipart content type has no boundary parameter".to_string())
}

/// `key=value` parameters of a header value, honouring quoted strings and
/// backslash escapes: `form-data; name="a \"b\""; filename=x.png`.
fn params(value: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes: Vec<char> = value.chars().collect();
    let mut i = 0;
    // Skip the leading token (the media type / disposition type).
    while i < bytes.len() && bytes[i] != ';' {
        i += 1;
    }
    while i < bytes.len() {
        i += 1; // skip ';'
        while i < bytes.len() && bytes[i].is_whitespace() {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len() && bytes[i] != '=' && bytes[i] != ';' {
            i += 1;
        }
        let key: String = bytes[key_start..i]
            .iter()
            .collect::<String>()
            .trim()
            .to_ascii_lowercase();
        if i >= bytes.len() || bytes[i] == ';' {
            continue;
        }
        i += 1; // skip '='
        let mut val = String::new();
        if i < bytes.len() && bytes[i] == '"' {
            i += 1;
            while i < bytes.len() && bytes[i] != '"' {
                if bytes[i] == '\\' && i + 1 < bytes.len() {
                    i += 1;
                }
                val.push(bytes[i]);
                i += 1;
            }
            i += 1; // closing quote
        } else {
            while i < bytes.len() && bytes[i] != ';' {
                val.push(bytes[i]);
                i += 1;
            }
            val = val.trim().to_string();
        }
        out.push((key, val));
    }
    out
}

/// RFC 5987 `filename*=UTF-8''na%C3%AFve.png`.
fn decode_ext_value(v: &str) -> String {
    let rest = match v.find("''") {
        Some(i) => &v[i + 2..],
        None => v,
    };
    let raw = rest.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' && i + 2 < raw.len() {
            let hex = std::str::from_utf8(&raw[i + 1..i + 3]).ok();
            if let Some(b) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Split a part into its header block and content.
fn split_part(raw: &[u8]) -> (&[u8], &[u8]) {
    match find(raw, b"\r\n\r\n", 0) {
        Some(i) => (&raw[..i], &raw[i + 4..]),
        // A part with no headers at all: all content.
        None => (&[], raw),
    }
}

fn part_value(headers: &[(String, String)], content: &[u8]) -> Value {
    let mut name = String::new();
    let mut filename: Option<String> = None;
    let mut ctype: Option<String> = None;
    for (k, v) in headers {
        if k == "content-disposition" {
            let mut ext: Option<String> = None;
            for (pk, pv) in params(v) {
                match pk.as_str() {
                    "name" => name = pv,
                    "filename" => filename = Some(pv),
                    // RFC 5987 wins over the plain parameter when both exist.
                    "filename*" => ext = Some(decode_ext_value(&pv)),
                    _ => {}
                }
            }
            if let Some(e) = ext {
                filename = Some(e);
            }
        } else if k == "content-type" {
            ctype = Some(v.clone());
        }
    }
    let mut header_map = Vec::with_capacity(headers.len() * 2);
    for (k, v) in headers {
        header_map.push(str_val(k.clone()));
        header_map.push(str_val(v.clone()));
    }
    map_of(vec![
        ("name", str_val(name)),
        (
            "filename",
            match &filename {
                Some(f) => str_val(f.clone()),
                None => Value::Null,
            },
        ),
        (
            "content_type",
            match &ctype {
                Some(c) => str_val(c.clone()),
                None => Value::Null,
            },
        ),
        (
            "headers",
            collections::construct("map", header_map).expect("map construction cannot fail"),
        ),
        ("data", Value::Bytes(new_ref(content.to_vec()))),
        ("size", Value::Int(content.len() as i64)),
        (
            "text",
            str_val(String::from_utf8_lossy(content).into_owned()),
        ),
    ])
}

// ------------------------------------------------------------ parse

fn parse(body: &[u8], content_type: &str) -> Result<Value, String> {
    let boundary = boundary_of(content_type)?;
    let delim = format!("--{}", boundary).into_bytes();
    let sep = format!("\r\n--{}", boundary).into_bytes();

    let mut pos = match find(body, &delim, 0) {
        Some(i) => i + delim.len(),
        None => return Err(format!("boundary '{}' not found in the body", boundary)),
    };

    let mut parts: Vec<Value> = Vec::new();
    loop {
        if body[pos..].starts_with(b"--") {
            break; // closing delimiter
        }
        // Tolerate transport padding and a missing CRLF after the delimiter.
        while pos < body.len() && (body[pos] == b' ' || body[pos] == b'\t') {
            pos += 1;
        }
        if body[pos..].starts_with(b"\r\n") {
            pos += 2;
        }
        let end = match find(body, &sep, pos) {
            Some(i) => i,
            None => return Err("unterminated multipart part (no closing boundary)".to_string()),
        };
        let (head, content) = split_part(&body[pos..end]);
        let mut headers: Vec<(String, String)> = Vec::new();
        for line in head.split(|&b| b == b'\n') {
            let line = if line.ends_with(b"\r") {
                &line[..line.len() - 1]
            } else {
                line
            };
            if line.is_empty() {
                continue;
            }
            let text = String::from_utf8_lossy(line);
            if let Some(colon) = text.find(':') {
                headers.push((
                    text[..colon].trim().to_ascii_lowercase(),
                    text[colon + 1..].trim().to_string(),
                ));
            }
        }
        parts.push(part_value(&headers, content));
        if parts.len() > MAX_PARTS {
            return Err(format!("more than {} parts", MAX_PARTS));
        }
        pos = end + sep.len();
    }

    let items = collections::construct("list", parts)?;
    Ok(map_of(vec![
        ("boundary", str_val(boundary)),
        ("parts", items),
    ]))
}

/// `multipart_parse(body_bytes, content_type)` -> `ok({boundary, parts})` /
/// `err(msg)`. Each part: `{name, filename, content_type, headers, data,
/// size, text}`.
fn n_parse(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "multipart_parse";
    if args.len() != 2 {
        return Err(format!("{} expects 2 arguments, got {}", WHO, args.len()));
    }
    let body = as_bytes(&args[0], WHO)?;
    let ctype = match &args[1] {
        Value::Str(s) => s.to_string(),
        other => {
            return Err(format!(
                "{} expects the content-type header as a string, found {}",
                WHO,
                type_name(other)
            ))
        }
    };
    match parse(&body, &ctype) {
        Ok(v) => ok(v),
        Err(e) => err(format!("{}: {}", WHO, e)),
    }
}

// ------------------------------------------------------------ build

/// `multipart_build(boundary, parts)` -> `ok(bytes)`.
///
/// The encoder half, used by clients and by the tests. `parts` is a list of
/// `{name, data|text, filename?, content_type?}`.
fn n_build(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    const WHO: &str = "multipart_build";
    if args.len() != 2 {
        return Err(format!("{} expects 2 arguments, got {}", WHO, args.len()));
    }
    let (boundary, items) = match (&args[0], &args[1]) {
        (Value::Str(s), Value::List(l)) if !s.is_empty() => {
            (s.to_string(), l.lock().unwrap().items.clone())
        }
        (Value::List(l), Value::Str(s)) if !s.is_empty() => {
            (s.to_string(), l.lock().unwrap().items.clone())
        }
        (Value::Str(s), _) if s.is_empty() => {
            return Err(format!("{}: boundary must be a non-empty string", WHO));
        }
        (_, Value::Str(s)) if s.is_empty() => {
            return Err(format!("{}: boundary must be a non-empty string", WHO));
        }
        _ => {
            return Err(format!(
                "{} expects boundary string and parts list, found ({}, {})",
                WHO,
                type_name(&args[0]),
                type_name(&args[1])
            ));
        }
    };

    let mut out: Vec<u8> = Vec::new();
    for part in items {
        // `index_get` is the same lookup `part["name"]` performs, so the
        // builder accepts exactly the maps a script would build by hand.
        let field = |k: &str| collections::index_get(&part, &str_val(k)).ok();
        let name = match field("name") {
            Some(Value::Str(s)) => s.to_string(),
            _ => return Err(format!("{}: every part needs a 'name'", WHO)),
        };
        let filename = match field("filename") {
            Some(Value::Str(s)) => Some(s.to_string()),
            _ => None,
        };
        let ctype = match field("content_type") {
            Some(Value::Str(s)) => Some(s.to_string()),
            _ => None,
        };
        let data = match field("data") {
            Some(Value::Bytes(b)) => b.lock().unwrap().clone(),
            Some(Value::Str(s)) => s.as_bytes().to_vec(),
            _ => match field("text") {
                Some(Value::Str(s)) => s.as_bytes().to_vec(),
                _ => Vec::new(),
            },
        };
        out.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        let mut disp = format!("Content-Disposition: form-data; name=\"{}\"", escape(&name));
        if let Some(f) = &filename {
            disp.push_str(&format!("; filename=\"{}\"", escape(f)));
        }
        out.extend_from_slice(disp.as_bytes());
        out.extend_from_slice(b"\r\n");
        if let Some(c) = &ctype {
            out.extend_from_slice(format!("Content-Type: {}\r\n", c).as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&data);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
    ok(Value::Bytes(new_ref(out)))
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
