//! End-to-end tests for `spid3r lsp`: a real `spid3r` child process speaking
//! JSON-RPC over stdio with `Content-Length` framing.
//!
//! The binary is launched via `CARGO_BIN_EXE_spid3r`, so these tests exercise
//! the shipped command line path (argv dispatch, the big-stack worker thread,
//! stdin/stdout framing) rather than a re-entrant library call.

#![allow(clippy::too_many_lines)]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_spid3r");

/// How long any single protocol read may block before the test fails.
///
/// Reading a child's stdout is unbounded: if the server stops answering (or a
/// response never matches what `request` waits for), the test blocks forever and
/// the *whole CI job* dies on its timeout with no failing test named. A bounded
/// read turns that into "no message within 20 s" on the test that caused it.
const READ_TIMEOUT: Duration = Duration::from_secs(20);

// ---------------------------------------------------------- minimal JSON read

/// Enough JSON to navigate an LSP response. Deliberately hand-written so the
/// test does not depend on the implementation's own parser.
#[derive(Debug, Clone, PartialEq)]
enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Obj(o) => o.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn as_str(&self) -> Option<&str> {
        match self {
            J::Str(s) => Some(s),
            _ => None,
        }
    }
    fn as_arr(&self) -> Option<&[J]> {
        match self {
            J::Arr(a) => Some(a),
            _ => None,
        }
    }
    fn as_i(&self) -> Option<i64> {
        match self {
            J::Num(n) => Some(*n as i64),
            _ => None,
        }
    }
    fn as_bool(&self) -> Option<bool> {
        match self {
            J::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.s.len() && (self.s[self.i] as char).is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn value(&mut self) -> J {
        self.ws();
        match self.s[self.i] {
            b'{' => self.object(),
            b'[' => self.array(),
            b'"' => J::Str(self.string()),
            b't' => {
                self.i += 4;
                J::Bool(true)
            }
            b'f' => {
                self.i += 5;
                J::Bool(false)
            }
            b'n' => {
                self.i += 4;
                J::Null
            }
            _ => self.number(),
        }
    }
    fn object(&mut self) -> J {
        let mut o = Vec::new();
        self.i += 1; // {
        loop {
            self.ws();
            if self.i < self.s.len() && self.s[self.i] == b'}' {
                self.i += 1;
                break;
            }
            let k = self.string();
            self.ws();
            if self.i < self.s.len() && self.s[self.i] == b':' {
                self.i += 1;
            }
            o.push((k, self.value()));
            self.ws();
            if self.i < self.s.len() && self.s[self.i] == b',' {
                self.i += 1;
            }
        }
        J::Obj(o)
    }
    fn array(&mut self) -> J {
        let mut a = Vec::new();
        self.i += 1; // [
        loop {
            self.ws();
            if self.i < self.s.len() && self.s[self.i] == b']' {
                self.i += 1;
                break;
            }
            a.push(self.value());
            self.ws();
            if self.i < self.s.len() && self.s[self.i] == b',' {
                self.i += 1;
            }
        }
        J::Arr(a)
    }
    fn string(&mut self) -> String {
        let mut out = String::new();
        self.i += 1; // opening quote
        while self.i < self.s.len() {
            let c = self.s[self.i] as char;
            if c == '"' {
                self.i += 1;
                break;
            }
            if c == '\\' {
                self.i += 1;
                let e = self.s[self.i] as char;
                match e {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'u' => {
                        let hex = &self.s[self.i + 1..self.i + 5];
                        let n = u32::from_str_radix(std::str::from_utf8(hex).unwrap(), 16).unwrap();
                        out.push(char::from_u32(n).unwrap_or('\u{fffd}'));
                        self.i += 4;
                    }
                    other => out.push(other),
                }
                self.i += 1;
                continue;
            }
            out.push(c);
            self.i += 1;
        }
        out
    }
    fn number(&mut self) -> J {
        let start = self.i;
        while self.i < self.s.len()
            && matches!(
                self.s[self.i],
                b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'
            )
        {
            self.i += 1;
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).unwrap();
        J::Num(text.parse().unwrap_or(0.0))
    }
}

fn parse(s: &str) -> J {
    let mut p = P {
        s: s.as_bytes(),
        i: 0,
    };
    p.value()
}

// -------------------------------------------------------------------- session

/// One `spid3r lsp` child process, already past `initialize`/`initialized`.
struct Session {
    child: Child,
    stdin: ChildStdin,
    /// Parsed messages, pushed by the reader thread.
    /// `Ok(Some(_))` message, `Ok(None)` end of stream, `Err(_)` bad framing.
    rx: Receiver<Result<Option<J>, String>>,
    next_id: i64,
    /// The `initialize` result, kept so a test can inspect the capabilities.
    init: J,
}

/// Read one `Content-Length` framed message. `Ok(None)` is end of stream.
fn read_frame(r: &mut BufReader<ChildStdout>) -> Result<Option<J>, String> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        match r.read_line(&mut line) {
            Ok(0) => return Ok(None),
            Ok(_) => {}
            Err(e) => return Err(format!("read error while reading headers: {e}")),
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse().ok();
        }
    }
    let len = match content_length {
        Some(len) if len <= 8 * 1024 * 1024 => len,
        Some(len) => return Err(format!("implausible Content-Length: {len}")),
        None => return Err("server sent no Content-Length header".to_string()),
    };
    let mut buf = vec![0u8; len];
    if let Err(e) = r.read_exact(&mut buf) {
        return Err(format!("short body ({len} bytes expected): {e}"));
    }
    let text = std::str::from_utf8(&buf).map_err(|e| format!("body is not UTF-8: {e}"))?;
    Ok(Some(parse(text)))
}

impl Session {
    fn start() -> Session {
        let mut child = Command::new(BIN)
            .arg("lsp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spid3r lsp should start");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());

        // Drain stdout on its own thread so every wait below can be bounded by
        // a timeout instead of blocking forever.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut stdout = stdout;
            loop {
                let frame = read_frame(&mut stdout);
                let last = !matches!(frame, Ok(Some(_)));
                if tx.send(frame).is_err() || last {
                    return; // test finished, or end of stream / bad framing
                }
            }
        });

        let mut s = Session {
            child,
            stdin,
            rx,
            next_id: 1,
            init: J::Null,
        };
        s.init = s.request("initialize", "{\"capabilities\":{}}");
        s.notify("initialized", "{}");
        s
    }

    /// Write one framed JSON-RPC message.
    fn send(&mut self, body: &str) {
        let _ = self
            .stdin
            .write_all(format!("Content-Length: {}\r\n\r\n{}", body.len(), body).as_bytes());
        let _ = self.stdin.flush();
    }

    /// A notification: no `id`, no reply expected.
    fn notify(&mut self, method: &str, params: &str) {
        self.send(&format!(
            "{{\"jsonrpc\":\"2.0\",\"method\":\"{}\",\"params\":{}}}",
            method, params
        ));
    }

    /// A request: returns the `result` object. Server-initiated notifications
    /// that arrive while we wait are skipped.
    fn request(&mut self, method: &str, params: &str) -> J {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"{}\",\"params\":{}}}",
            id, method, params
        ));
        loop {
            let msg = self.read();
            match msg.get("id").and_then(|i| i.as_i()) {
                Some(got) if got == id => {
                    if let Some(err) = msg.get("error") {
                        panic!("request {} failed: {:?}", method, err);
                    }
                    return msg.get("result").cloned().unwrap_or(J::Null);
                }
                _ => continue, // a notification or another request's reply
            }
        }
    }

    /// Read the next message whose `method` matches, skipping others.
    fn read_notification(&mut self, method: &str) -> J {
        loop {
            let msg = self.read();
            if msg.get("method").and_then(|m| m.as_str()) == Some(method) {
                return msg;
            }
        }
    }

    fn read(&mut self) -> J {
        match self.rx.recv_timeout(READ_TIMEOUT) {
            Ok(Ok(Some(msg))) => msg,
            Ok(Ok(None)) => {
                panic!("spid3r lsp closed stdout while the test was waiting for a message")
            }
            Ok(Err(e)) => panic!("spid3r lsp sent a malformed frame: {e}"),
            Err(RecvTimeoutError::Timeout) => panic!(
                "spid3r lsp sent no message within {}s (possible hang)",
                READ_TIMEOUT.as_secs()
            ),
            Err(RecvTimeoutError::Disconnected) => {
                panic!("spid3r lsp reader stopped unexpectedly")
            }
        }
    }

    /// Open a document and return its first `publishDiagnostics` payload.
    fn open(&mut self, uri: &str, text: &str) -> J {
        let params = format!(
            "{{\"textDocument\":{{\"uri\":\"{}\",\"languageId\":\"spid3r\",\"version\":1,\"text\":{}}}}}",
            uri,
            json_string(text)
        );
        self.notify("textDocument/didOpen", &params);
        let msg = self.read_notification("textDocument/publishDiagnostics");
        assert_eq!(
            msg.get("params").unwrap().get("uri").unwrap().as_str(),
            Some(uri)
        );
        msg.get("params")
            .unwrap()
            .get("diagnostics")
            .cloned()
            .unwrap()
    }

    fn finish(mut self) -> std::process::ExitStatus {
        self.request("shutdown", "null");
        self.notify("exit", "null");
        drop(self.stdin); // EOF is the belt to `exit`'s braces
        self.child.wait().expect("child should exit")
    }
}

/// Quote a Rust string as a JSON string literal (handles the escapes the test
/// sources need).
fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// --------------------------------------------------------------------- tests

const URI: &str = "file:///project/main.matrix";

const TWO_FUNCS: &str = "fn helper(a) {\n  return a;\n}\nfn main() {\n  helper(1);\n}\n";

/// The same two functions in canonical formatting (`spid3r fmt` indents with
/// four spaces), so a formatting request has nothing to change.
const CANONICAL_FUNCS: &str = "fn helper(a) {\n    return a;\n}\nfn main() {\n    helper(1);\n}\n";

#[test]
fn initialize_reports_the_supported_features() {
    let s = Session::start();
    let caps = s.init.get("capabilities").expect("a capabilities object");
    assert_eq!(caps.get("textDocumentSync").and_then(|v| v.as_i()), Some(1));
    assert_eq!(
        caps.get("hoverProvider").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        caps.get("definitionProvider").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        caps.get("documentSymbolProvider").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        caps.get("documentFormattingProvider")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    assert!(caps.get("completionProvider").is_some());
    assert_eq!(
        s.init
            .get("serverInfo")
            .and_then(|i| i.get("name"))
            .and_then(|n| n.as_str()),
        Some("spid3r")
    );
    s.finish();
}

#[test]
fn opening_a_file_publishes_its_semantic_diagnostics() {
    let mut s = Session::start();
    // `"hello"` is not an `int`, so the checker reports a type mismatch on the
    // line that holds the string literal (line 1, 0-based).
    let src = "fn main() {\n  let x: int = \"hello\";\n}\n";
    let diags = s.open(URI, src);
    let first = diags
        .as_arr()
        .expect("a diagnostics array")
        .first()
        .expect("one diagnostic");
    assert_eq!(first.get("severity").and_then(|v| v.as_i()), Some(1));
    assert_eq!(first.get("source").and_then(|v| v.as_str()), Some("spid3r"));
    let line = first
        .get("range")
        .and_then(|r| r.get("start"))
        .and_then(|p| p.get("line"))
        .and_then(|l| l.as_i());
    assert_eq!(line, Some(1));
    assert!(first
        .get("message")
        .and_then(|m| m.as_str())
        .map(|m| !m.is_empty())
        .unwrap_or(false));
    s.finish();
}

#[test]
fn a_clean_file_publishes_no_diagnostics() {
    let mut s = Session::start();
    let diags = s.open(URI, TWO_FUNCS);
    assert!(diags.as_arr().expect("array").is_empty());
    s.finish();
}

#[test]
fn positions_are_utf16_code_units_not_unicode_scalars() {
    // LSP counts characters in UTF-16 code units; Spid3r internally counts
    // Unicode scalars. The three emoji between `let a = "` and the rest of
    // the line are three scalars but SIX UTF-16 units, so every column after
    // them used to be reported three units too small — an editor would then
    // place the cursor, the hover, and the rename on the wrong character.
    let mut s = Session::start();
    let src = "let a = \"\u{1f600}\u{1f600}\u{1f600}\"; let x: int = \"bad\";\n";
    let diags = s.open(URI, src);
    let first = diags
        .as_arr()
        .expect("a diagnostics array")
        .first()
        .expect("one diagnostic");
    // The diagnostic covers the string literal `"bad"` (5 chars, 31..36).
    let start = first
        .get("range")
        .and_then(|r| r.get("start"))
        .expect("a start position");
    assert_eq!(start.get("line").and_then(|l| l.as_i()), Some(0));
    assert_eq!(start.get("character").and_then(|c| c.as_i()), Some(31));
    let end = first
        .get("range")
        .and_then(|r| r.get("end"))
        .expect("an end position");
    assert_eq!(end.get("character").and_then(|c| c.as_i()), Some(36));

    // The outline must use the same convention: `x` starts 18 units in
    // (9 + 6 + 3), not 15 scalars in.
    let result = s.request(
        "textDocument/documentSymbol",
        &format!("{{\"textDocument\":{{\"uri\":\"{}\"}}}}", URI),
    );
    let x = result
        .as_arr()
        .expect("a symbol array")
        .iter()
        .find(|sym| sym.get("name").and_then(|n| n.as_str()) == Some("x"))
        .expect("an x symbol");
    assert_eq!(
        x.get("range")
            .and_then(|r| r.get("start"))
            .and_then(|p| p.get("character"))
            .and_then(|c| c.as_i()),
        Some(18)
    );
    s.finish();
}

#[test]
fn editing_a_file_republishes_diagnostics() {
    let mut s = Session::start();
    s.open(URI, "fn main() {\n  let x: int = \"hello\"\n}\n");
    let params = format!(
        "{{\"textDocument\":{{\"uri\":\"{}\",\"version\":2}},\"contentChanges\":[{{\"text\":{}}}]}}",
        URI,
        json_string("fn main() {\n  let x: int = 7;\n  print(x);\n}\n")
    );
    s.notify("textDocument/didChange", &params);
    let msg = s.read_notification("textDocument/publishDiagnostics");
    let diags = msg.get("params").unwrap().get("diagnostics").unwrap();
    assert!(diags.as_arr().expect("array").is_empty());
    s.finish();
}

#[test]
fn hover_reports_the_declaration_under_the_cursor() {
    let mut s = Session::start();
    s.open(URI, TWO_FUNCS);
    // `helper` inside `helper(1)` is line 4, character 2.
    let result = s.request(
        "textDocument/hover",
        &format!(
            "{{\"textDocument\":{{\"uri\":\"{}\"}},\"position\":{{\"line\":4,\"character\":2}}}}",
            URI
        ),
    );
    let value = result
        .get("contents")
        .and_then(|c| c.get("value"))
        .and_then(|v| v.as_str())
        .expect("a markdown value");
    assert!(
        value.contains("function"),
        "hover should name the kind: {}",
        value
    );
    assert!(
        value.contains("helper(a)"),
        "hover should show the signature: {}",
        value
    );
    s.finish();
}

#[test]
fn hover_shows_a_doc_comment_when_there_is_one() {
    let mut s = Session::start();
    let src = "/// Doubles its argument.\nfn double(x) {\n  return x * 2;\n}\nfn main() {\n  double(2);\n}\n";
    s.open(URI, src);
    let result = s.request(
        "textDocument/hover",
        &format!(
            "{{\"textDocument\":{{\"uri\":\"{}\"}},\"position\":{{\"line\":5,\"character\":2}}}}",
            URI
        ),
    );
    let value = result
        .get("contents")
        .and_then(|c| c.get("value"))
        .and_then(|v| v.as_str())
        .expect("a markdown value");
    assert!(value.contains("Doubles its argument."), "{}", value);
    s.finish();
}

#[test]
fn hover_on_a_local_resolves_it_to_the_parameter() {
    let mut s = Session::start();
    // `a` is the parameter of `helper`; hovering it inside the body must not
    // resolve to something else.
    s.open(URI, TWO_FUNCS);
    let result = s.request(
        "textDocument/hover",
        &format!(
            "{{\"textDocument\":{{\"uri\":\"{}\"}},\"position\":{{\"line\":1,\"character\":9}}}}",
            URI
        ),
    );
    let value = result
        .get("contents")
        .and_then(|c| c.get("value"))
        .and_then(|v| v.as_str())
        .expect("a markdown value");
    assert!(value.contains("parameter"), "{}", value);
    s.finish();
}

#[test]
fn hover_on_a_keyword_yields_nothing() {
    let mut s = Session::start();
    s.open(URI, TWO_FUNCS);
    // `fn` at line 0, character 0.
    let result = s.request(
        "textDocument/hover",
        &format!(
            "{{\"textDocument\":{{\"uri\":\"{}\"}},\"position\":{{\"line\":0,\"character\":0}}}}",
            URI
        ),
    );
    assert_eq!(result, J::Null);
    s.finish();
}

#[test]
fn definition_jumps_to_the_declaration() {
    let mut s = Session::start();
    s.open(URI, TWO_FUNCS);
    let result = s.request(
        "textDocument/definition",
        &format!(
            "{{\"textDocument\":{{\"uri\":\"{}\"}},\"position\":{{\"line\":4,\"character\":2}}}}",
            URI
        ),
    );
    let locs = result.as_arr().expect("a location array");
    assert_eq!(locs.len(), 1);
    assert_eq!(locs[0].get("uri").and_then(|u| u.as_str()), Some(URI));
    let line = locs[0]
        .get("range")
        .and_then(|r| r.get("start"))
        .and_then(|p| p.get("line"))
        .and_then(|l| l.as_i());
    assert_eq!(line, Some(0), "helper is declared on line 0");
    s.finish();
}

#[test]
fn document_symbol_lists_the_outline() {
    let mut s = Session::start();
    s.open(URI, TWO_FUNCS);
    let result = s.request(
        "textDocument/documentSymbol",
        &format!("{{\"textDocument\":{{\"uri\":\"{}\"}}}}", URI),
    );
    let syms = result.as_arr().expect("a symbol array");
    let names: Vec<&str> = syms
        .iter()
        .filter_map(|s| s.get("name").and_then(|n| n.as_str()))
        .collect();
    assert!(names.contains(&"helper"), "{:?}", names);
    assert!(names.contains(&"main"), "{:?}", names);
    s.finish();
}

#[test]
fn document_symbol_nests_methods_inside_a_class() {
    let mut s = Session::start();
    let src = "class Point {\n  let x: int;\n  fn x2() {\n    return 2;\n  }\n}\n";
    s.open(URI, src);
    let result = s.request(
        "textDocument/documentSymbol",
        &format!("{{\"textDocument\":{{\"uri\":\"{}\"}}}}", URI),
    );
    let point = result
        .as_arr()
        .expect("array")
        .iter()
        .find(|s| s.get("name").and_then(|n| n.as_str()) == Some("Point"))
        .expect("a Point symbol");
    let kids: Vec<&str> = point
        .get("children")
        .and_then(|c| c.as_arr())
        .expect("children")
        .iter()
        .filter_map(|s| s.get("name").and_then(|n| n.as_str()))
        .collect();
    assert!(kids.contains(&"x2"), "{:?}", kids);
    assert!(kids.contains(&"x"), "{:?}", kids);
    s.finish();
}

#[test]
fn completion_offers_keywords_and_file_symbols() {
    let mut s = Session::start();
    s.open(URI, TWO_FUNCS);
    let result = s.request(
        "textDocument/completion",
        &format!(
            "{{\"textDocument\":{{\"uri\":\"{}\"}},\"position\":{{\"line\":5,\"character\":0}}}}",
            URI
        ),
    );
    let labels: Vec<&str> = result
        .as_arr()
        .expect("array")
        .iter()
        .filter_map(|i| i.get("label").and_then(|l| l.as_str()))
        .collect();
    assert!(labels.contains(&"helper"), "{:?}", labels);
    assert!(labels.contains(&"fn"), "{:?}", labels);
    assert!(labels.contains(&"while"), "{:?}", labels);
    s.finish();
}

#[test]
fn formatting_returns_a_full_document_edit() {
    let mut s = Session::start();
    s.open(URI, CANONICAL_FUNCS);
    let result = s.request(
        "textDocument/formatting",
        &format!(
            "{{\"textDocument\":{{\"uri\":\"{}\"}},\"options\":{{\"tabSize\":2,\"insertSpaces\":true}}}}",
            URI
        ),
    );
    // Already formatted: no edits, and the server says so rather than
    // rewriting the buffer.
    assert!(result.as_arr().expect("array").is_empty());
    s.finish();
}

#[test]
fn formatting_rewrites_unformatted_source() {
    let mut s = Session::start();
    let ugly = "fn   main   (   ){  return 1 ;}";
    s.open(URI, ugly);
    let result = s.request(
        "textDocument/formatting",
        &format!(
            "{{\"textDocument\":{{\"uri\":\"{}\"}},\"options\":{{}}}}",
            URI
        ),
    );
    let edits = result.as_arr().expect("array");
    assert_eq!(edits.len(), 1, "one whole-document edit");
    let new_text = edits[0]
        .get("newText")
        .and_then(|t| t.as_str())
        .expect("a newText");
    assert!(new_text.contains("fn main()"), "{}", new_text);
    assert_ne!(new_text, ugly);
    s.finish();
}

#[test]
fn an_unknown_method_is_answered_with_an_error() {
    let mut s = Session::start();
    let id = s.next_id;
    s.send(&format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"textDocument/teleport\",\"params\":{{}}}}",
        id
    ));
    s.next_id += 1;
    loop {
        let msg = s.read();
        if msg.get("id").and_then(|i| i.as_i()) == Some(id) {
            let code = msg
                .get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_i());
            assert_eq!(code, Some(-32601));
            break;
        }
    }
    s.finish();
}

#[test]
fn exiting_without_shutdown_signals_a_failed_session() {
    let mut child = Command::new(BIN)
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spid3r lsp should start");
    // `exit` without `shutdown` first: the server must report failure (LSP §exit).
    let body = "{\"jsonrpc\":\"2.0\",\"method\":\"exit\",\"params\":null}";
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("Content-Length: {}\r\n\r\n{}", body.len(), body).as_bytes())
        .unwrap();
    let status = child.wait().expect("child should exit");
    assert_eq!(status.code(), Some(1));
}

#[test]
fn exiting_after_shutdown_signals_success() {
    let s = Session::start();
    let status = s.finish();
    assert!(status.success(), "{:?}", status);
}

#[test]
fn closing_stdin_ends_the_session_cleanly() {
    let mut s = Session::start();
    s.open(URI, TWO_FUNCS);
    drop(s.stdin);
    let status = s.child.wait().expect("child should exit");
    assert_eq!(status.code(), Some(0));
}

// ------------------------------------------------------------------ rename

/// Shadowing fixture: an outer `value` and an inner `value` must rename
/// independently.
const SHADOW: &str = "fn main() {\n  let value = 1;\n  if (true) {\n    let value = 2;\n    println(value);\n  }\n  println(value);\n  return 0;\n}\n";

#[test]
fn references_find_all_occurrences_including_the_declaration() {
    let mut s = Session::start();
    s.open(URI, TWO_FUNCS);
    // `helper` inside `helper(1)` — line 4, character 2.
    let result = s.request(
        "textDocument/references",
        &format!(
            "{{\"textDocument\":{{\"uri\":\"{}\"}},\"position\":{{\"line\":4,\"character\":2}},\"context\":{{\"includeDeclaration\":true}}}}",
            URI
        ),
    );
    let arr = result.as_arr().expect("an array of locations");
    assert_eq!(arr.len(), 2, "declaration + one call site: {:?}", arr);
    for loc in arr {
        assert_eq!(
            loc.get("uri").and_then(|u| u.as_str()),
            Some(URI),
            "file-scoped references"
        );
    }
    s.finish();
}

#[test]
fn references_respect_include_declaration_false() {
    let mut s = Session::start();
    s.open(URI, TWO_FUNCS);
    let result = s.request(
        "textDocument/references",
        &format!(
            "{{\"textDocument\":{{\"uri\":\"{}\"}},\"position\":{{\"line\":4,\"character\":2}},\"context\":{{\"includeDeclaration\":false}}}}",
            URI
        ),
    );
    let arr = result.as_arr().expect("an array of locations");
    assert_eq!(arr.len(), 1, "call site only: {:?}", arr);
    s.finish();
}

#[test]
fn references_do_not_cross_shadowing_boundaries() {
    let mut s = Session::start();
    s.open(URI, SHADOW);
    // The OUTER `value` (line 1, char 6): declaration + the final println.
    let result = s.request(
        "textDocument/references",
        &format!(
            "{{\"textDocument\":{{\"uri\":\"{}\"}},\"position\":{{\"line\":1,\"character\":6}},\"context\":{{\"includeDeclaration\":true}}}}",
            URI
        ),
    );
    let arr = result.as_arr().expect("an array of locations");
    assert_eq!(arr.len(), 2, "outer value: decl + println: {:?}", arr);
    // The INNER `value` (line 3, char 8): declaration + its println.
    let result = s.request(
        "textDocument/references",
        &format!(
            "{{\"textDocument\":{{\"uri\":\"{}\"}},\"position\":{{\"line\":3,\"character\":8}},\"context\":{{\"includeDeclaration\":true}}}}",
            URI
        ),
    );
    let arr = result.as_arr().expect("an array of locations");
    assert_eq!(arr.len(), 2, "inner value: decl + println: {:?}", arr);
    s.finish();
}

#[test]
fn rename_rewrites_every_occurrence_of_the_target() {
    let mut s = Session::start();
    s.open(URI, TWO_FUNCS);
    let result = s.request(
        "textDocument/rename",
        &format!(
            "{{\"textDocument\":{{\"uri\":\"{}\"}},\"position\":{{\"line\":4,\"character\":2}},\"newName\":\"greet\"}}",
            URI
        ),
    );
    let changes = result.get("changes").expect("a workspace edit");
    let edits = changes
        .get(URI)
        .and_then(|e| e.as_arr())
        .expect("edits for the uri");
    assert_eq!(edits.len(), 2, "declaration + call site renamed");
    for edit in edits {
        assert_eq!(edit.get("newText").and_then(|t| t.as_str()), Some("greet"));
    }
    // Applying the edits to the source text yields the renamed program.
    // (The fixture is ASCII, so character offsets are byte offsets.)
    fn offset_of(text: &str, line: i64, ch: i64) -> usize {
        let mut off = 0usize;
        for (i, l) in text.lines().enumerate() {
            if i as i64 == line {
                return off + ch as usize;
            }
            off += l.len() + 1;
        }
        off
    }
    let mut text = TWO_FUNCS.to_string();
    let mut spans: Vec<(usize, usize)> = edits
        .iter()
        .map(|e| {
            let r = e.get("range").expect("range");
            let st = r.get("start").expect("start");
            let en = r.get("end").expect("end");
            (
                offset_of(
                    &text,
                    st.get("line").and_then(|v| v.as_i()).unwrap_or(0),
                    st.get("character").and_then(|v| v.as_i()).unwrap_or(0),
                ),
                offset_of(
                    &text,
                    en.get("line").and_then(|v| v.as_i()).unwrap_or(0),
                    en.get("character").and_then(|v| v.as_i()).unwrap_or(0),
                ),
            )
        })
        .collect();
    spans.sort();
    for (s, e) in spans.iter().rev() {
        text.replace_range(s..e, "greet");
    }
    assert!(
        text.contains("fn greet(a)") && text.contains("greet(1);") && !text.contains("helper"),
        "renamed program should have no `helper` left: {}",
        text
    );
    s.finish();
}

#[test]
fn rename_rejects_an_invalid_identifier() {
    let mut s = Session::start();
    s.open(URI, TWO_FUNCS);
    let result = s.request(
        "textDocument/rename",
        &format!(
            "{{\"textDocument\":{{\"uri\":\"{}\"}},\"position\":{{\"line\":4,\"character\":2}},\"newName\":\"2bad name\"}}",
            URI
        ),
    );
    assert!(
        result.get("failureReason").is_some(),
        "an invalid identifier must report a failure reason: {:?}",
        result
    );
    s.finish();
}

// ------------------------------------------------------- semantic tokens

/// Decode the LSP's delta encoding back into absolute tokens:
/// `(line, char, length, type, modifiers)`, all 0-based.
fn decode_tokens(data: &[J]) -> Vec<(i64, i64, i64, i64, i64)> {
    let nums: Vec<i64> = data.iter().filter_map(|v| v.as_i()).collect();
    assert_eq!(nums.len() % 5, 0, "semantic token data must be 5-tuples");
    let mut out = Vec::new();
    let (mut line, mut ch) = (0i64, 0i64);
    for chunk in nums.chunks(5) {
        line += chunk[0];
        ch = if chunk[0] == 0 {
            ch + chunk[1]
        } else {
            chunk[1]
        };
        out.push((line, ch, chunk[2], chunk[3], chunk[4]));
    }
    out
}

/// The legend index of each type, mirroring the server's advertised order.
fn type_index(name: &str) -> i64 {
    match name {
        "type" => 0,
        "class" => 1,
        "enum" => 2,
        "interface" => 3,
        "parameter" => 4,
        "variable" => 5,
        "property" => 6,
        "enumMember" => 7,
        "function" => 8,
        "method" => 9,
        "keyword" => 10,
        "comment" => 11,
        "string" => 12,
        "number" => 13,
        "operator" => 14,
        other => panic!("unknown token type in test: {}", other),
    }
}

#[test]
fn semantic_tokens_classify_keywords_functions_and_literals() {
    let mut s = Session::start();
    let src = "// counts things\nfn double(n) {\n  return n * 2;\n}\n";
    s.open(URI, src);
    let result = s.request(
        "textDocument/semanticTokens/full",
        &format!("{{\"textDocument\":{{\"uri\":\"{}\"}}}}", URI),
    );
    let tokens = decode_tokens(
        result
            .get("data")
            .and_then(|d| d.as_arr())
            .expect("data array"),
    );

    let at = |line: i64, ch: i64| {
        tokens
            .iter()
            .find(|(l, c, _, _, _)| *l == line && *c == ch)
            .copied()
            .unwrap_or_else(|| panic!("no token at {}:{} in {:?}", line, ch, tokens))
    };

    // line 0: the comment, in full.
    assert_eq!(at(0, 0).3, type_index("comment"));
    assert_eq!(at(0, 0).2, "// counts things".len() as i64);
    // line 1: `fn` keyword, `double` a function declaration, `n` a parameter.
    assert_eq!(at(1, 0).3, type_index("keyword"));
    assert_eq!(at(1, 3).3, type_index("function"));
    assert_eq!(
        at(1, 3).4 & 1,
        1,
        "a declaration carries the declaration bit"
    );
    assert_eq!(at(1, 10).3, type_index("parameter"));
    // line 2: `return` keyword, `n` a variable use, `*` an operator, `2` a number.
    assert_eq!(at(2, 2).3, type_index("keyword"));
    assert_eq!(at(2, 9).3, type_index("parameter"));
    assert_eq!(at(2, 11).3, type_index("operator"));
    assert_eq!(at(2, 13).3, type_index("number"));
    s.finish();
}

#[test]
fn semantic_tokens_mark_a_class_and_its_methods() {
    let mut s = Session::start();
    let src = "class Box {\n  public fn new() {\n    return 1;\n  }\n}\n";
    s.open(URI, src);
    let result = s.request(
        "textDocument/semanticTokens/full",
        &format!("{{\"textDocument\":{{\"uri\":\"{}\"}}}}", URI),
    );
    let tokens = decode_tokens(
        result
            .get("data")
            .and_then(|d| d.as_arr())
            .expect("data array"),
    );
    let find = |needle: &str| {
        tokens
            .iter()
            .find(|(l, c, len, _, _)| {
                let line = src.lines().nth(*l as usize).unwrap_or("");
                let start = *c as usize;
                line.chars()
                    .skip(start)
                    .take(*len as usize)
                    .collect::<String>()
                    == needle
            })
            .copied()
            .unwrap_or_else(|| panic!("no token spelled '{}' in {:?}", needle, tokens))
    };
    assert_eq!(find("Box").3, type_index("class"));
    assert_eq!(find("new").3, type_index("method"));
    s.finish();
}

// ---------------------------------------------------------- code actions

#[test]
fn code_action_renames_an_unused_variable() {
    let mut s = Session::start();
    // `unused` is reported on line 1; the fix is to prefix it with `_`.
    let src = "fn main() {\n  let unused = 1;\n  return 0;\n}\n";
    s.open(URI, src);
    let actions = s.request(
        "textDocument/codeAction",
        &format!(
            "{{\"textDocument\":{{\"uri\":\"{}\"}},\"range\":{{\"start\":{{\"line\":1,\"character\":0}},\"end\":{{\"line\":1,\"character\":6}}}},\"context\":{{\"diagnostics\":[]}}}}",
            URI
        ),
    );
    let list = actions.as_arr().expect("an array of actions");
    assert!(
        !list.is_empty(),
        "an unused variable must offer a quick fix"
    );
    let first = &list[0];
    assert_eq!(first.get("kind").and_then(|k| k.as_str()), Some("quickfix"));
    let text = first
        .get("edit")
        .and_then(|e| e.get("changes"))
        .and_then(|c| c.get(URI))
        .and_then(|a| a.as_arr())
        .and_then(|a| a.first())
        .and_then(|e| e.get("newText"))
        .and_then(|t| t.as_str())
        .expect("an edit with newText");
    assert_eq!(text, "_unused");
    s.finish();
}

#[test]
fn code_action_adds_a_missing_stdlib_import() {
    let mut s = Session::start();
    // `yaml` is a stdlib module, so `yaml.parse(...)` without an import is a
    // mechanical fix: add the import.
    let src = "fn main() {\n  let cfg = yaml.parse(\"a: 1\");\n  return 0;\n}\n";
    s.open(URI, src);
    let actions = s.request(
        "textDocument/codeAction",
        &format!(
            "{{\"textDocument\":{{\"uri\":\"{}\"}},\"range\":{{\"start\":{{\"line\":1,\"character\":0}},\"end\":{{\"line\":1,\"character\":10}}}},\"context\":{{\"diagnostics\":[]}}}}",
            URI
        ),
    );
    let list = actions.as_arr().expect("an array of actions");
    let import_action = list
        .iter()
        .find(|a| {
            a.get("title")
                .and_then(|t| t.as_str())
                .map(|t| t.contains("import"))
                .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("expected an import quick fix, got {:?}", list));
    let text = import_action
        .get("edit")
        .and_then(|e| e.get("changes"))
        .and_then(|c| c.get(URI))
        .and_then(|a| a.as_arr())
        .and_then(|a| a.first())
        .and_then(|e| e.get("newText"))
        .and_then(|t| t.as_str())
        .expect("an edit with newText");
    assert_eq!(text, "import \"yaml\";\n");
    s.finish();
}

#[test]
fn code_action_is_empty_away_from_a_diagnostic() {
    let mut s = Session::start();
    let src = "fn main() {\n  let unused = 1;\n  return 0;\n}\n";
    s.open(URI, src);
    let actions = s.request(
        "textDocument/codeAction",
        &format!(
            "{{\"textDocument\":{{\"uri\":\"{}\"}},\"range\":{{\"start\":{{\"line\":3,\"character\":0}},\"end\":{{\"line\":3,\"character\":0}}}},\"context\":{{\"diagnostics\":[]}}}}",
            URI
        ),
    );
    assert_eq!(
        actions.as_arr().map(|a| a.len()),
        Some(0),
        "no diagnostic on that line means no quick fix"
    );
    s.finish();
}
