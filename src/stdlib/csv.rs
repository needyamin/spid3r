//! RFC-4180 compliant CSV parser and writer with automatic type inference.
//!
//! Handles quoted fields, embedded newlines, escaped quotes, custom delimiters,
//! header extraction, and type inference for int, float, bool, null, and string.

use crate::value::collections::{make_list, MapData};
use crate::value::{new_ref, MapKey, Value};

pub fn parse_csv_text(
    text: &str,
    delimiter: char,
    has_header: bool,
) -> Result<Value, String> {
    let mut raw_rows: Vec<Vec<String>> = Vec::new();
    let mut cur_row: Vec<String> = Vec::new();
    let mut cur_field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    cur_field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                cur_field.push(ch);
            }
        } else if ch == '"' {
            in_quotes = true;
        } else if ch == delimiter {
            cur_row.push(cur_field);
            cur_field = String::new();
        } else if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            if cur_row.is_empty() && cur_field.is_empty() {
                continue;
            }
            cur_row.push(cur_field);
            raw_rows.push(cur_row);
            cur_row = Vec::new();
            cur_field = String::new();
        } else if ch == '\n' {
            if cur_row.is_empty() && cur_field.is_empty() {
                continue;
            }
            cur_row.push(cur_field);
            raw_rows.push(cur_row);
            cur_row = Vec::new();
            cur_field = String::new();
        } else {
            cur_field.push(ch);
        }
    }

    if in_quotes {
        return Err("unclosed quote in CSV input".to_string());
    }

    if !cur_field.is_empty() || !cur_row.is_empty() {
        cur_row.push(cur_field);
        raw_rows.push(cur_row);
    }

    let (headers, data_rows) = if raw_rows.is_empty() {
        (Vec::new(), Vec::new())
    } else if has_header {
        let first = raw_rows.remove(0);
        let headers: Vec<Value> = first.into_iter().map(|s| Value::Str(s.into())).collect();
        (headers, raw_rows)
    } else {
        let max_cols = raw_rows.iter().map(|r| r.len()).max().unwrap_or(0);
        let headers: Vec<Value> = (0..max_cols)
            .map(|i| Value::Str(format!("col_{}", i).into()))
            .collect();
        (headers, raw_rows)
    };

    let rows_val: Vec<Value> = data_rows
        .into_iter()
        .map(|row| {
            let row_vals: Vec<Value> = row.into_iter().map(|f| infer_csv_value(&f)).collect();
            make_list(row_vals)
        })
        .collect();

    let entries = vec![
        (MapKey::Str("headers".into()), make_list(headers)),
        (MapKey::Str("rows".into()), make_list(rows_val)),
    ];

    Ok(Value::Result(Ok(Box::new(Value::Map(new_ref(
        MapData::from_entries(entries),
    ))))))
}

pub fn infer_csv_value(field: &str) -> Value {
    let trimmed = field.trim();
    if trimmed.is_empty() || trimmed == "NA" || trimmed == "null" || trimmed == "NULL" {
        Value::Null
    } else if let Ok(i) = trimmed.parse::<i64>() {
        Value::Int(i)
    } else if let Ok(f) = trimmed.parse::<f64>() {
        if f.is_finite() {
            Value::Float(f)
        } else {
            Value::Str(field.to_string().into())
        }
    } else if trimmed.eq_ignore_ascii_case("true") {
        Value::Bool(true)
    } else if trimmed.eq_ignore_ascii_case("false") {
        Value::Bool(false)
    } else {
        Value::Str(field.to_string().into())
    }
}

pub fn format_csv_field(val: &Value, delimiter: char) -> String {
    let s = match val {
        Value::Null => String::new(),
        Value::Str(s) => s.to_string(),
        other => crate::value::display_value(other),
    };
    if s.contains(delimiter) || s.contains('"') || s.contains('\n') || s.contains('\r') {
        let escaped = s.replace('"', "\"\"");
        format!("\"{}\"", escaped)
    } else {
        s
    }
}

pub fn write_csv_file(path: &str, headers: &[Value], rows: &[Value]) -> Result<Value, String> {
    let mut out = String::new();

    // Headers
    let header_strs: Vec<String> = headers
        .iter()
        .map(|h| format_csv_field(h, ','))
        .collect();
    out.push_str(&header_strs.join(","));
    out.push('\n');

    // Rows
    for row_val in rows {
        let row_items = match row_val {
            Value::List(l) | Value::VecDyn(l) => l.lock().unwrap().items.clone(),
            Value::Tuple(t) => t.lock().unwrap().clone(),
            other => {
                return Err(format!(
                    "write_csv row expects a list or tuple, found {}",
                    crate::value::type_name(other)
                ));
            }
        };
        let line: Vec<String> = row_items
            .iter()
            .map(|cell| format_csv_field(cell, ','))
            .collect();
        out.push_str(&line.join(","));
        out.push('\n');
    }

    match std::fs::write(path, out) {
        Ok(()) => Ok(Value::Result(Ok(Box::new(Value::Null)))),
        Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(
            format!("cannot write csv to '{}': {}", path, e).into(),
        ))))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_csv_basic() {
        let csv = "name,age,score,active\nAlice,30,95.5,true\nBob,25,null,false\n\"Charlie, Jr.\",40,NA,true\n";
        let res = parse_csv_text(csv, ',', true).unwrap();
        if let Value::Result(Ok(m)) = res {
            if let Value::Map(map_ref) = *m {
                let map = map_ref.lock().unwrap();
                assert_eq!(map.entries.len(), 2);
            } else {
                panic!("expected map");
            }
        } else {
            panic!("expected ok result");
        }
    }
}
