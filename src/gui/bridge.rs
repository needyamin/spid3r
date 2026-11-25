//! The pure half of the GUI layer: the bridge between the language runtime
//! and the webview.
//!
//! Exactly one GUI program runs per `spid3r` process, so the bridge is
//! process-global state behind a `Mutex`. Every `Value` in the runtime is
//! `Arc`-based (`Ref<T> = Arc<Mutex<T>>`), so handler closures can safely
//! cross the thread boundary between the interpreter and the window's event
//! loop — no `unsafe`, no `Rc`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::diagnostics::Span;
use crate::value::Value;

/// Whether a registered `gui_on` callback expects 1 arg `(payload)` or 2 args
/// `(event, payload)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandlerArity {
    OneArg,
    TwoArgs,
}

/// Registered `gui_on` callback together with its expected arity and declaration
/// span (used for accurate runtime error attribution).
#[derive(Clone)]
pub struct HandlerEntry {
    pub handler: Value,
    pub arity: HandlerArity,
    pub span: Option<Span>,
}

/// Native window theme override for `gui_theme(...)` / `--theme`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowTheme {
    Light,
    Dark,
    System,
}

/// Window/webview state commands queued by `gui_*` builtins.
///
/// Commands pushed during Phase 1 (before the window opens) are applied to
/// `WindowBuilder` / `WebViewBuilder` (eliminating dark-mode startup flash and
/// initial resize flicker); commands pushed in Phase 2 apply live via the
/// event loop.
#[derive(Clone, Debug, PartialEq)]
pub enum WindowCommand {
    Icon(String),
    Size(f64, f64),
    MinSize(f64, f64),
    Maximized(bool),
    Fullscreen(bool),
    Resizable(bool),
    Decorations(bool),
    Theme(WindowTheme),
    BgColor(u8, u8, u8, u8),
    DevTools(bool),
    Close,
}

/// Parse a `#RGB`, `#RGBA`, `#RRGGBB`, or `#RRGGBBAA` hex color string.
pub fn parse_hex_color(s: &str) -> Result<(u8, u8, u8, u8), String> {
    let hex = s.trim().strip_prefix('#').unwrap_or(s.trim());
    let dup = |b: u8| -> Result<u8, String> {
        let v = (b as char)
            .to_digit(16)
            .ok_or_else(|| format!("invalid hex color '{}'", s))? as u8;
        Ok((v << 4) | v)
    };
    let pair = |slice: &str| -> Result<u8, String> {
        u8::from_str_radix(slice, 16).map_err(|_| format!("invalid hex color '{}'", s))
    };
    let b = hex.as_bytes();
    match b.len() {
        3 => Ok((dup(b[0])?, dup(b[1])?, dup(b[2])?, 255)),
        4 => Ok((dup(b[0])?, dup(b[1])?, dup(b[2])?, dup(b[3])?)),
        6 => Ok((pair(&hex[0..2])?, pair(&hex[2..4])?, pair(&hex[4..6])?, 255)),
        8 => Ok((
            pair(&hex[0..2])?,
            pair(&hex[2..4])?,
            pair(&hex[4..6])?,
            pair(&hex[6..8])?,
        )),
        _ => Err(format!(
            "invalid hex color '{}' (expected #RGB, #RRGGBB, or #RRGGBBAA)",
            s
        )),
    }
}

/// The process-global bridge. Guarded by a mutex because the webview IPC
/// callback, background `spawn` threads, and the window's event loop all touch
/// it (potentially from different OS threads).
struct Bridge {
    /// JavaScript commands queued by `gui_eval` / `gui_set_html`, in order.
    out: VecDeque<String>,
    /// Events pushed by the page (`window.spid3r.emit`) or by `gui_emit`,
    /// in order. Unmatched events (no handler) are dropped on dispatch.
    inbound: VecDeque<(String, String)>,
    /// Handlers registered by `gui_on`: (event, entry). Re-registering an
    /// event replaces its handler.
    handlers: Vec<(String, HandlerEntry)>,
    /// Window title requested by `gui_title`.
    title: Option<String>,
    /// Window/webview configuration & runtime commands.
    window_cmds: VecDeque<WindowCommand>,
    /// Set by `gui_cancel_close()` during a `before_close` / `close` handler.
    close_cancelled: bool,
    /// Callback that nudges the active window event loop when worker threads
    /// queue JS, events, or title updates.
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Set once the window closes so post-close worker pushes are discarded.
    closed: bool,
}

impl Bridge {
    const fn new() -> Self {
        Bridge {
            out: VecDeque::new(),
            inbound: VecDeque::new(),
            handlers: Vec::new(),
            title: None,
            window_cmds: VecDeque::new(),
            close_cancelled: false,
            waker: None,
            closed: false,
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
    b.window_cmds.clear();
    b.close_cancelled = false;
    b.waker = None;
    b.closed = false;
}

/// Install or clear the event-loop wake callback invoked whenever `push_js`,
/// `push_event`, or `set_title` updates the bridge while the window is open.
pub fn set_waker(waker: Option<Arc<dyn Fn() + Send + Sync>>) {
    let mut b = lock();
    b.waker = waker;
}

/// Mark the bridge closed and drop the event-loop waker.
pub fn mark_closed() {
    let mut b = lock();
    b.closed = true;
    b.waker = None;
}

/// Queue a JavaScript command for the page (evaluated in order).
pub fn push_js(js: &str) {
    let waker = {
        let mut b = lock();
        if b.closed {
            return;
        }
        b.out.push_back(js.to_string());
        b.waker.clone()
    };
    if let Some(w) = waker {
        w();
    }
}

/// Take every queued JavaScript command, leaving the queue empty.
pub fn take_out() -> Vec<String> {
    std::mem::take(&mut lock().out).into_iter().collect()
}

/// Queue an event (payload is always a string; the page side JSON-encodes).
pub fn push_event(event: &str, payload: &str) {
    let waker = {
        let mut b = lock();
        if b.closed {
            return;
        }
        b.inbound
            .push_back((event.to_string(), payload.to_string()));
        b.waker.clone()
    };
    if let Some(w) = waker {
        w();
    }
}

/// Take every queued event paired with its handler entry. Events without a
/// registered handler are dropped.
pub fn take_event_entries() -> Vec<(String, String, HandlerEntry)> {
    let mut b = lock();
    let events = std::mem::take(&mut b.inbound);
    let handlers = &b.handlers;
    events
        .into_iter()
        .filter_map(|(event, payload)| {
            handlers
                .iter()
                .find(|(name, _)| *name == event)
                .map(|(_, entry)| (event.clone(), payload, entry.clone()))
        })
        .collect()
}

/// Take every queued event paired with its handler value.
pub fn take_events() -> Vec<(String, String, Value)> {
    take_event_entries()
        .into_iter()
        .map(|(event, payload, entry)| (event, payload, entry.handler))
        .collect()
}

/// Register (or replace) the handler entry for `event`.
pub fn set_handler_entry(event: &str, entry: HandlerEntry) {
    let mut b = lock();
    b.handlers.retain(|(name, _)| name != event);
    b.handlers.push((event.to_string(), entry));
}

/// Register (or replace) a 2-arg handler for `event`.
pub fn set_handler(event: &str, handler: Value) {
    set_handler_entry(
        event,
        HandlerEntry {
            handler,
            arity: HandlerArity::TwoArgs,
            span: None,
        },
    );
}

/// Request a window title (read on open and whenever an event wakes the loop).
pub fn set_title(title: &str) {
    let waker = {
        let mut b = lock();
        if b.closed {
            return;
        }
        b.title = Some(title.to_string());
        b.waker.clone()
    };
    if let Some(w) = waker {
        w();
    }
}

/// Take the requested window title, if any.
pub fn take_title() -> Option<String> {
    lock().title.take()
}

/// Queue a window/webview command (size, min-size, icon, theme, bg-color, etc.).
pub fn push_window_cmd(cmd: WindowCommand) {
    let waker = {
        let mut b = lock();
        if b.closed {
            return;
        }
        b.window_cmds.push_back(cmd);
        b.waker.clone()
    };
    if let Some(w) = waker {
        w();
    }
}

/// Take all queued window/webview commands, in FIFO order.
pub fn take_window_cmds() -> Vec<WindowCommand> {
    std::mem::take(&mut lock().window_cmds)
        .into_iter()
        .collect()
}

/// Return true if a `gui_on` handler is registered for `event`.
pub fn has_handler(event: &str) -> bool {
    lock().handlers.iter().any(|(name, _)| name == event)
}

/// Look up the registered handler entry for `event`, if any.
pub fn get_handler_entry(event: &str) -> Option<HandlerEntry> {
    lock()
        .handlers
        .iter()
        .find(|(name, _)| name == event)
        .map(|(_, entry)| entry.clone())
}

/// Mark the current window close request as cancelled (`gui_cancel_close()`).
pub fn cancel_close() {
    lock().close_cancelled = true;
}

/// Reset and return whether a `before_close` / `close` handler called `gui_cancel_close()`.
pub fn take_close_cancelled() -> bool {
    let mut b = lock();
    let v = b.close_cancelled;
    b.close_cancelled = false;
    v
}

/// Clear any stale close-cancellation flag before dispatching `before_close`.
pub fn reset_close_cancelled() {
    lock().close_cancelled = false;
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
    read_json_string_with_end(json, start).map(|(s, _)| s)
}

/// Read a JSON string literal starting just after its opening quote, returning
/// the decoded string and the byte offset immediately after the closing quote.
fn read_json_string_with_end(json: &str, start: usize) -> Option<(String, usize)> {
    let bytes = json.as_bytes();
    let mut out = String::new();
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some((out, i + 1)),
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
                        if (0xD800..=0xDBFF).contains(&code) {
                            // RFC 8259 §7: combine UTF-16 high+low surrogate pair.
                            let combined = (|| -> Option<char> {
                                if json.get(i + 5..i + 7)? != "\\u" {
                                    return None;
                                }
                                let lo = u32::from_str_radix(json.get(i + 7..i + 11)?, 16).ok()?;
                                if !(0xDC00..=0xDFFF).contains(&lo) {
                                    return None;
                                }
                                char::from_u32(0x10000 + ((code - 0xD800) << 10) + (lo - 0xDC00))
                            })();
                            if let Some(ch) = combined {
                                out.push(ch);
                                i += 10;
                            } else {
                                out.push('\u{fffd}');
                                i += 4;
                            }
                        } else {
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                            i += 4;
                        }
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

fn scan_json_number_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    if bytes.get(i) == Some(&b'-') {
        i += 1;
    }
    match bytes.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => {
            i += 1;
            while matches!(bytes.get(i), Some(b'0'..=b'9')) {
                i += 1;
            }
        }
        _ => return None,
    }
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        if !matches!(bytes.get(i), Some(b'0'..=b'9')) {
            return None;
        }
        while matches!(bytes.get(i), Some(b'0'..=b'9')) {
            i += 1;
        }
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(bytes.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if !matches!(bytes.get(i), Some(b'0'..=b'9')) {
            return None;
        }
        while matches!(bytes.get(i), Some(b'0'..=b'9')) {
            i += 1;
        }
    }
    Some(i)
}

fn scan_json_value_end(json: &str, start: usize) -> Option<usize> {
    let bytes = json.as_bytes();
    match *bytes.get(start)? {
        b'"' => read_json_string_with_end(json, start + 1).map(|(_, end)| end),
        b'{' | b'[' => {
            let mut stack = vec![if bytes[start] == b'{' { b'}' } else { b']' }];
            let mut i = start + 1;
            while i < bytes.len() {
                match bytes[i] {
                    b'"' => {
                        let (_, end) = read_json_string_with_end(json, i + 1)?;
                        i = end;
                    }
                    b'{' => {
                        stack.push(b'}');
                        i += 1;
                    }
                    b'[' => {
                        stack.push(b']');
                        i += 1;
                    }
                    b'}' | b']' => {
                        let expected = stack.pop()?;
                        if bytes[i] != expected {
                            return None;
                        }
                        i += 1;
                        if stack.is_empty() {
                            return Some(i);
                        }
                    }
                    _ => i += 1,
                }
            }
            None
        }
        b't' => json
            .get(start..start + 4)
            .filter(|s| *s == "true")
            .map(|_| start + 4),
        b'f' => json
            .get(start..start + 5)
            .filter(|s| *s == "false")
            .map(|_| start + 5),
        b'n' => json
            .get(start..start + 4)
            .filter(|s| *s == "null")
            .map(|_| start + 4),
        b'-' | b'0'..=b'9' => scan_json_number_end(bytes, start),
        _ => None,
    }
}

enum TopJsonValue {
    Str(String),
    Raw(String),
}

fn parse_top_level_object(json: &str) -> Result<Vec<(String, TopJsonValue)>, String> {
    let trimmed = json.trim();
    let bytes = trimmed.as_bytes();
    if bytes.first() != Some(&b'{') {
        return Err("expected a JSON object".to_string());
    }
    let mut i = skip_ws(bytes, 1);
    let mut members = Vec::new();
    if bytes.get(i) == Some(&b'}') {
        let rest = skip_ws(bytes, i + 1);
        if rest != bytes.len() {
            return Err("unexpected trailing data after JSON object".to_string());
        }
        return Ok(members);
    }
    loop {
        if bytes.get(i) != Some(&b'"') {
            return Err("expected string key in JSON object".to_string());
        }
        let (key, key_end) = read_json_string_with_end(trimmed, i + 1)
            .ok_or_else(|| "malformed JSON key string".to_string())?;
        i = skip_ws(bytes, key_end);
        if bytes.get(i) != Some(&b':') {
            return Err("expected ':' after JSON key".to_string());
        }
        i = skip_ws(bytes, i + 1);
        let val_start = i;
        let val_end = scan_json_value_end(trimmed, val_start)
            .ok_or_else(|| format!("malformed JSON value for '{}'", key))?;
        let val = if bytes.get(val_start) == Some(&b'"') {
            let (s, _) = read_json_string_with_end(trimmed, val_start + 1)
                .ok_or_else(|| format!("malformed JSON string for '{}'", key))?;
            TopJsonValue::Str(s)
        } else {
            TopJsonValue::Raw(trimmed[val_start..val_end].to_string())
        };
        members.push((key, val));
        i = skip_ws(bytes, val_end);
        match bytes.get(i) {
            Some(b',') => i = skip_ws(bytes, i + 1),
            Some(b'}') => {
                let rest = skip_ws(bytes, i + 1);
                if rest != bytes.len() {
                    return Err("unexpected trailing data after JSON object".to_string());
                }
                return Ok(members);
            }
            _ => return Err("expected ',' or '}' in JSON object".to_string()),
        }
    }
}

/// Read the top-level member stored under `key` in a JSON object: decoded text
/// when the value is a string, or its JSON text representation when the value
/// is an object, array, number, boolean, or null.
pub fn json_payload(json: &str, key: &str) -> Option<String> {
    let members = parse_top_level_object(json).ok()?;
    let mut found = None;
    for (k, v) in members {
        if k == key {
            found = Some(match v {
                TopJsonValue::Str(s) => s,
                TopJsonValue::Raw(raw) => raw,
            });
        }
    }
    found
}

/// Decode a `window.spid3r.emit(event, payload)` JSON envelope.
///
/// String payloads are unquoted cleanly; structured payloads (objects, arrays,
/// numbers, booleans, null) are preserved as their JSON text; an omitted
/// `"payload"` key decodes as `""`. Malformed JSON or a missing/non-string
/// `"event"` returns an explicit error instead of silently dispatching an empty event.
pub fn decode_ipc_message(body: &str) -> Result<(String, String), String> {
    let members = parse_top_level_object(body)?;
    let mut event: Option<String> = None;
    let mut payload = String::new();
    for (k, v) in members {
        if k == "event" {
            match v {
                TopJsonValue::Str(s) => event = Some(s),
                TopJsonValue::Raw(_) => {
                    return Err("GUI IPC message 'event' must be a string".to_string());
                }
            }
        } else if k == "payload" {
            payload = match v {
                TopJsonValue::Str(s) => s,
                TopJsonValue::Raw(raw) => raw,
            };
        }
    }
    let event = event.ok_or_else(|| "GUI IPC message missing 'event' field".to_string())?;
    Ok((event, payload))
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

    #[test]
    fn ipc_message_preserves_structured_and_scalar_payloads() {
        let obj = r#"{"event":"save","data":{"payload":"nested"},"payload":{"id":42,"ok":true}}"#;
        let (ev, pl) = decode_ipc_message(obj).expect("object payload");
        assert_eq!(ev, "save");
        assert_eq!(pl, r#"{"id":42,"ok":true}"#);
        assert_eq!(
            json_payload(obj, "payload").as_deref(),
            Some(r#"{"id":42,"ok":true}"#)
        );

        let arr = r#"{"event":"items","payload":[1,"two",false,null]}"#;
        assert_eq!(
            decode_ipc_message(arr).unwrap(),
            ("items".to_string(), r#"[1,"two",false,null]"#.to_string())
        );

        let num = r#"{"event":"count","payload":42}"#;
        assert_eq!(
            decode_ipc_message(num).unwrap(),
            ("count".to_string(), "42".to_string())
        );

        let boolean = r#"{"event":"flag","payload":false}"#;
        assert_eq!(
            decode_ipc_message(boolean).unwrap(),
            ("flag".to_string(), "false".to_string())
        );

        let null_pl = r#"{"event":"empty","payload":null}"#;
        assert_eq!(
            decode_ipc_message(null_pl).unwrap(),
            ("empty".to_string(), "null".to_string())
        );

        let omitted = r#"{"event":"ping"}"#;
        assert_eq!(
            decode_ipc_message(omitted).unwrap(),
            ("ping".to_string(), String::new())
        );

        assert!(decode_ipc_message(r#"{"event":"bad","payload":"#).is_err());
        assert!(decode_ipc_message(r#"{"payload":1}"#).is_err());
        assert!(decode_ipc_message("not json").is_err());
    }

    #[test]
    fn concurrent_worker_pushes_wake_loop_in_fifo_order() {
        let _serial = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let wakes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wakes2 = wakes.clone();
        set_waker(Some(std::sync::Arc::new(move || {
            wakes2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })));

        let mut handles = Vec::new();
        for tid in 0..4 {
            handles.push(std::thread::spawn(move || {
                for seq in 0..100 {
                    push_js(&format!("t{}:{}", tid, seq));
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let out = take_out();
        assert_eq!(out.len(), 400);
        assert_eq!(wakes.load(std::sync::atomic::Ordering::SeqCst), 400);

        let mut next_seq = [0usize; 4];
        for item in out {
            let (tid_s, seq_s) = item.strip_prefix('t').unwrap().split_once(':').unwrap();
            let tid: usize = tid_s.parse().unwrap();
            let seq: usize = seq_s.parse().unwrap();
            assert_eq!(seq, next_seq[tid], "FIFO broken for thread {}", tid);
            next_seq[tid] += 1;
        }
        assert_eq!(next_seq, [100, 100, 100, 100]);
        reset();
    }
}
