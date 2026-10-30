//! The pure half of the GUI layer: the bridge between the language runtime
//! and the webview.
//!
//! Exactly one GUI program runs per `spid3r` process, so the bridge is
//! process-global state behind a `Mutex`. Every `Value` in the runtime is
//! `Arc`-based (`Ref<T> = Arc<Mutex<T>>`), so handler closures can safely
//! cross the thread boundary between the interpreter and the window's event
//! loop — no `unsafe`, no `Rc`.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::value::Value;

/// The process-global bridge. Guarded by a mutex because the webview IPC
/// callback, the interpreter thread, and the window's event loop all touch
/// it (potentially from different OS threads).
struct Bridge {
    /// JavaScript commands queued by `gui_eval` / `gui_set_html`, in order.
    out: VecDeque<String>,
    /// Events pushed by the page (`window.spid3r.emit`) or by `gui_emit`,
    /// in order. Unmatched events (no handler) are dropped on dispatch.
    inbound: VecDeque<(String, String)>,
    /// Handlers registered by `gui_on`: (event, callable). Re-registering an
    /// event replaces its handler.
    handlers: Vec<(String, Value)>,
    /// Window title requested by `gui_title`.
    title: Option<String>,
}

impl Bridge {
    const fn new() -> Self {
        Bridge {
            out: VecDeque::new(),
            inbound: VecDeque::new(),
            handlers: Vec::new(),
            title: None,
        }
    }
}

static BRIDGE: Mutex<Bridge> = Mutex::new(Bridge::new());

/// Lock the bridge, recovering a poisoned mutex instead of panicking: a
/// panicking GUI handler must not make the whole runtime unusable.
fn lock() -> std::sync::MutexGuard<'static, Bridge> {
    BRIDGE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Clear every queue and handler. Called once per `spid3r gui` run and by the
/// unit tests so each starts from a clean bridge.
pub fn reset() {
    let mut b = lock();
    b.out.clear();
    b.inbound.clear();
    b.handlers.clear();
    b.title = None;
}

/// Queue a JavaScript command for the page (evaluated in order).
pub fn push_js(js: &str) {
    lock().out.push_back(js.to_string());
}

/// Take every queued JavaScript command, leaving the queue empty.
pub fn take_out() -> Vec<String> {
    std::mem::take(&mut lock().out).into_iter().collect()
}

/// Queue an event (payload is always a string; the page side JSON-encodes).
pub fn push_event(event: &str, payload: &str) {
    lock()
        .inbound
        .push_back((event.to_string(), payload.to_string()));
}

/// Take every queued event paired with its handler. Events without a
/// registered handler are dropped.
pub fn take_events() -> Vec<(String, String, Value)> {
    let mut b = lock();
    let events = std::mem::take(&mut b.inbound);
    let handlers = &b.handlers;
    events
        .into_iter()
        .filter_map(|(event, payload)| {
            handlers
                .iter()
                .find(|(name, _)| *name == event)
                .map(|(_, handler)| (event.clone(), payload, handler.clone()))
        })
        .collect()
}

/// Register (or replace) the handler for `event`.
pub fn set_handler(event: &str, handler: Value) {
    let mut b = lock();
    b.handlers.retain(|(name, _)| name != event);
    b.handlers.push((event.to_string(), handler));
}

/// Request a window title (read once, when the window is created).
pub fn set_title(title: &str) {
    lock().title = Some(title.to_string());
}

/// Take the requested window title, if any.
pub fn take_title() -> Option<String> {
    lock().title.take()
}

// ---------------------------------------------------------------------------
// minimal JSON string helpers
//
// The only data crossing the bridge are strings, so instead of a JSON
// dependency we encode/decode exactly string literals. Payloads produced by
// `JSON.stringify({ event, payload })` are flat objects whose values are
// strings, which is what `json_field` reads back.
// ---------------------------------------------------------------------------

/// Escape `s` as a JSON string literal (quotes included).
pub fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Read the string value stored under `key` in a flat JSON object literal.
/// Returns `None` when the key is absent, when its value is not a string, or
/// when the "key" match is inside another string's value.
pub fn json_field(json: &str, key: &str) -> Option<String> {
    let needle = json_quote(key);
    let bytes = json.as_bytes();
    let mut from = 0;
    while let Some(rel) = json[from..].find(&needle) {
        let start = from + rel;
        // A real member position: the key is preceded (ignoring whitespace)
        // by '{' or ','. Anything else means the match is inside a string.
        let before = json[..start].trim_end().as_bytes().last().copied();
        if matches!(before, Some(b'{') | Some(b',')) {
            let mut i = skip_ws(bytes, start + needle.len());
            if bytes.get(i) != Some(&b':') {
                return None;
            }
            i = skip_ws(bytes, i + 1);
            if bytes.get(i) != Some(&b'"') {
                return None; // present, but not a string value
            }
            return read_json_string(json, i + 1);
        }
        from = start + needle.len();
    }
    None
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while matches!(bytes.get(i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        i += 1;
    }
    i
}

/// Read a JSON string literal starting just after its opening quote.
fn read_json_string(json: &str, start: usize) -> Option<String> {
    let bytes = json.as_bytes();
    let mut out = String::new();
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some(out),
            b'\\' => {
                i += 1;
                match bytes.get(i)? {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{08}'),
                    b'f' => out.push('\u{0c}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let hex = json.get(i + 1..i + 5)?;
                        let code = u32::from_str_radix(hex, 16).ok()?;
                        // surrogates: decode as a lone replacement char; the
                        // bridge only ever carries BMP-escaped text from
                        // JSON.stringify of ordinary strings
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        i += 4;
                    }
                    _ => return None,
                }
                i += 1;
            }
            _ => {
                // copy one UTF-8 code point
                let ch = json[i..].chars().next()?;
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    None // unterminated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_quote_escapes_specials() {
        assert_eq!(json_quote("hi"), "\"hi\"");
        assert_eq!(json_quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_quote("a\\b"), "\"a\\\\b\"");
        assert_eq!(json_quote("a\nb\tc"), "\"a\\nb\\tc\"");
        assert_eq!(json_quote("\u{1}"), "\"\\u0001\"");
    }

    #[test]
    fn json_field_reads_string_members() {
        let json = format!(
            "{{{},{}}}",
            json_quote("event") + ":" + &json_quote("click"),
            json_quote("payload") + ":" + &json_quote("row 7 \"x\"")
        );
        assert_eq!(json_field(&json, "event").as_deref(), Some("click"));
        assert_eq!(json_field(&json, "payload").as_deref(), Some("row 7 \"x\""));
        assert_eq!(json_field(&json, "missing"), None);
    }

    #[test]
    fn json_field_ignores_keys_inside_values() {
        // "payload" appears inside the value of "note" — it must not match.
        let json = format!(
            "{{{}}}",
            json_quote("note") + ":" + &json_quote("the payload is wrong")
        );
        assert_eq!(json_field(&json, "payload"), None);
    }

    #[test]
    fn json_field_rejects_non_string_values() {
        let json = format!("{{{}}}", json_quote("n") + ":42");
        assert_eq!(json_field(&json, "n"), None);
    }

    #[test]
    fn json_field_decodes_escapes() {
        let json = "{\"event\":\"a\\nb\",\"payload\":\"\\u00e9\\t\\\\\"}";
        assert_eq!(json_field(json, "event").as_deref(), Some("a\nb"));
        assert_eq!(json_field(json, "payload").as_deref(), Some("é\t\\"));
    }

    /// The bridge keeps handlers and queues in process-wide statics, so the
    /// tests below must not interleave: each takes this lock before `reset()`.
    /// (Without it, `cargo test`'s parallel harness races the shared state.)
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn out_queue_keeps_order_and_drains() {
        let _serial = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        push_js("one");
        push_js("two");
        push_js("three");
        assert_eq!(take_out(), vec!["one", "two", "three"]);
        assert!(take_out().is_empty());
    }

    #[test]
    fn events_pair_with_handlers() {
        let _serial = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let handler = Value::Int(1); // stand-in: any Value is paired through
        set_handler("click", handler);
        push_event("click", "row");
        push_event("unheard", "ignored");
        let events = take_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "click");
        assert_eq!(events[0].1, "row");
        assert!(matches!(events[0].2, Value::Int(1)));
        // drained
        assert!(take_events().is_empty());
    }

    #[test]
    fn set_handler_replaces_previous() {
        let _serial = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        set_handler("tap", Value::Int(1));
        set_handler("tap", Value::Int(2));
        push_event("tap", "");
        let events = take_events();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].2, Value::Int(2)));
    }

    #[test]
    fn title_roundtrip() {
        let _serial = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        assert_eq!(take_title(), None);
        set_title("my app");
        assert_eq!(take_title().as_deref(), Some("my app"));
        assert_eq!(take_title(), None);
    }

    #[test]
    fn json_quote_roundtrips_unicode_and_controls() {
        for s in [
            "é",
            "🦀",
            "a\u{0}b",
            "tab\there",
            "back\\slash\"q",
            "nul\u{0}and\r",
        ] {
            let q = json_quote(s);
            let body = format!("{{{}}}", json_quote("k") + ":" + &q);
            assert_eq!(
                json_field(&body, "k").as_deref(),
                Some(s),
                "roundtrip failed for {:?}",
                s
            );
        }
    }

    #[test]
    fn json_field_tolerates_whitespace_and_extra_members() {
        // A number member, spaces around `:` and `,`, and a value with braces.
        let json = "{ \"n\" : 42 , \"event\" : \"e\" , \"payload\" : \"a{b},c\" }";
        assert_eq!(json_field(json, "event").as_deref(), Some("e"));
        assert_eq!(json_field(json, "payload").as_deref(), Some("a{b},c"));
        assert_eq!(json_field(json, "n"), None); // present but not a string
    }

    #[test]
    fn json_field_truncated_or_malformed_is_none() {
        assert_eq!(json_field("{\"event\":\"unterminated", "event"), None);
        assert_eq!(json_field("{\"event\":", "event"), None);
        assert_eq!(json_field("{\"event\":\"a\\u00", "event"), None); // truncated \u
    }

    /// The real page -> Rust path, minus the webview transport: the injected
    /// `window.spid3r.emit` posts `JSON.stringify({ event, payload })`, which the
    /// IPC handler decodes with `json_field` and dispatches via `push_event`.
    #[test]
    fn page_body_decodes_and_dispatches_to_the_handler() {
        let _serial = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        set_handler("click", Value::Int(7));
        let payload = "row 7 \"x\"\nline2 é 🦀";
        let body = format!(
            "{{{},{}}}",
            json_quote("event") + ":" + &json_quote("click"),
            json_quote("payload") + ":" + &json_quote(payload),
        );
        // Exactly what `with_ipc_handler` does with the request body:
        let event = json_field(&body, "event").unwrap_or_default();
        let pl = json_field(&body, "payload").unwrap_or_default();
        push_event(&event, &pl);
        let events = take_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0, "click");
        assert_eq!(events[0].1, payload);
        assert!(matches!(events[0].2, Value::Int(7)));
    }
}
