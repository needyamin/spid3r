//! REPL: reads lines, evaluates them, keeps successful bindings, survives a
//! bad line, supports multiline input until braces balance, and provides
//! interactive cursor editing + command history when attached to a TTY.

use std::io::{IsTerminal, Write};
use std::sync::Arc;

use crate::interpreter::{Flow, Interp};
use crate::lexer::Lexer;
use crate::modules;
use crate::parser;
use crate::semantic;
use crate::value::display_value;

const MAX_HISTORY: usize = 500;

/// Single-line editor keys supported by [`LineEditor`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    KillToStart,
    KillToEnd,
    ClearScreen,
    Enter,
    Eof,
    Interrupt,
}

/// Action emitted after [`LineEditor::handle_key`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Continue,
    Redraw,
    Submit(String),
    ClearScreen,
    Eof,
    Interrupt,
}

/// Pure, terminal-independent line buffer and history state machine.
pub struct LineEditor {
    chars: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    hist_idx: Option<usize>,
    draft: String,
}

impl Default for LineEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl LineEditor {
    pub fn new() -> Self {
        Self {
            chars: Vec::new(),
            cursor: 0,
            history: Vec::new(),
            hist_idx: None,
            draft: String::new(),
        }
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn cursor_pos(&self) -> usize {
        self.cursor
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    pub fn reset_line(&mut self) {
        self.chars.clear();
        self.cursor = 0;
        self.hist_idx = None;
        self.draft.clear();
    }

    pub fn push_history(&mut self, entry: &str) {
        let cleaned = entry.trim_end_matches(['\r', '\n']);
        if cleaned.trim().is_empty() {
            return;
        }
        if self.history.last().is_some_and(|last| last == cleaned) {
            return;
        }
        if self.history.len() >= MAX_HISTORY {
            self.history.remove(0);
        }
        self.history.push(cleaned.to_string());
    }

    fn set_buffer(&mut self, text: &str) {
        self.chars = text.chars().collect();
        self.cursor = self.chars.len();
    }

    pub fn handle_key(&mut self, key: Key) -> KeyAction {
        match key {
            Key::Char(c) => {
                self.chars.insert(self.cursor, c);
                self.cursor += 1;
                self.hist_idx = None;
                KeyAction::Redraw
            }
            Key::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.chars.remove(self.cursor);
                    self.hist_idx = None;
                    KeyAction::Redraw
                } else {
                    KeyAction::Continue
                }
            }
            Key::Delete => {
                if self.cursor < self.chars.len() {
                    self.chars.remove(self.cursor);
                    self.hist_idx = None;
                    KeyAction::Redraw
                } else {
                    KeyAction::Continue
                }
            }
            Key::Left => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    KeyAction::Redraw
                } else {
                    KeyAction::Continue
                }
            }
            Key::Right => {
                if self.cursor < self.chars.len() {
                    self.cursor += 1;
                    KeyAction::Redraw
                } else {
                    KeyAction::Continue
                }
            }
            Key::Home => {
                if self.cursor != 0 {
                    self.cursor = 0;
                    KeyAction::Redraw
                } else {
                    KeyAction::Continue
                }
            }
            Key::End => {
                if self.cursor != self.chars.len() {
                    self.cursor = self.chars.len();
                    KeyAction::Redraw
                } else {
                    KeyAction::Continue
                }
            }
            Key::KillToStart => {
                if self.cursor > 0 {
                    self.chars.drain(..self.cursor);
                    self.cursor = 0;
                    self.hist_idx = None;
                    KeyAction::Redraw
                } else {
                    KeyAction::Continue
                }
            }
            Key::KillToEnd => {
                if self.cursor < self.chars.len() {
                    self.chars.truncate(self.cursor);
                    self.hist_idx = None;
                    KeyAction::Redraw
                } else {
                    KeyAction::Continue
                }
            }
            Key::Up => {
                if self.history.is_empty() {
                    return KeyAction::Continue;
                }
                let idx = match self.hist_idx {
                    None => {
                        self.draft = self.text();
                        self.history.len() - 1
                    }
                    Some(i) => i.saturating_sub(1),
                };
                self.hist_idx = Some(idx);
                let line = self.history[idx].clone();
                self.set_buffer(&line);
                KeyAction::Redraw
            }
            Key::Down => {
                let Some(idx) = self.hist_idx else {
                    return KeyAction::Continue;
                };
                if idx + 1 < self.history.len() {
                    let next = idx + 1;
                    self.hist_idx = Some(next);
                    let line = self.history[next].clone();
                    self.set_buffer(&line);
                } else {
                    self.hist_idx = None;
                    let draft = std::mem::take(&mut self.draft);
                    self.set_buffer(&draft);
                }
                KeyAction::Redraw
            }
            Key::ClearScreen => KeyAction::ClearScreen,
            Key::Enter => {
                let submitted = self.text();
                self.reset_line();
                KeyAction::Submit(submitted)
            }
            Key::Interrupt => {
                self.reset_line();
                KeyAction::Interrupt
            }
            Key::Eof => {
                if self.chars.is_empty() {
                    KeyAction::Eof
                } else if self.cursor < self.chars.len() {
                    self.chars.remove(self.cursor);
                    KeyAction::Redraw
                } else {
                    KeyAction::Continue
                }
            }
        }
    }
}

#[cfg(windows)]
mod tty {
    use core::ffi::c_void;
    use std::io::Write;

    use super::{Key, KeyAction, LineEditor};

    const STD_INPUT_HANDLE: u32 = (-10i32) as u32;
    const INVALID_HANDLE: *mut c_void = (-1isize) as *mut c_void;
    const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
    const ENABLE_LINE_INPUT: u32 = 0x0002;
    const ENABLE_ECHO_INPUT: u32 = 0x0004;
    const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(nStdHandle: u32) -> *mut c_void;
        fn GetConsoleMode(hConsoleHandle: *mut c_void, lpMode: *mut u32) -> i32;
        fn SetConsoleMode(hConsoleHandle: *mut c_void, dwMode: u32) -> i32;
        fn ReadConsoleW(
            hConsoleInput: *mut c_void,
            lpBuffer: *mut u16,
            nNumberOfCharsToRead: u32,
            lpNumberOfCharsRead: *mut u32,
            pInputControl: *mut c_void,
        ) -> i32;
    }

    struct ConsoleModeGuard {
        handle: *mut c_void,
        orig_mode: u32,
    }

    impl Drop for ConsoleModeGuard {
        fn drop(&mut self) {
            // SAFETY: `self.handle` was validated non-null/non-invalid and
            // `self.orig_mode` came from a successful `GetConsoleMode` call.
            unsafe {
                let _ = SetConsoleMode(self.handle, self.orig_mode);
            }
        }
    }

    fn read_u16(handle: *mut c_void) -> Option<u16> {
        let mut unit: u16 = 0;
        let mut read: u32 = 0;
        // SAFETY: `handle` is the validated Win32 console stdin handle and
        // `unit`/`read` point to initialized stack storage for 1 UTF-16 unit.
        let ok = unsafe { ReadConsoleW(handle, &mut unit, 1, &mut read, core::ptr::null_mut()) };
        if ok == 0 || read == 0 {
            None
        } else {
            Some(unit)
        }
    }

    fn read_char(handle: *mut c_void) -> Option<char> {
        let first = read_u16(handle)?;
        if (0xD800..=0xDBFF).contains(&first) {
            let second = read_u16(handle)?;
            char::decode_utf16([first, second]).next()?.ok()
        } else {
            char::from_u32(u32::from(first))
        }
    }

    fn read_key(handle: *mut c_void) -> Option<Key> {
        let ch = read_char(handle)?;
        match ch {
            '\r' | '\n' => Some(Key::Enter),
            '\x03' => Some(Key::Interrupt),
            '\x04' => Some(Key::Eof),
            '\x01' => Some(Key::Home),
            '\x05' => Some(Key::End),
            '\x08' | '\x7f' => Some(Key::Backspace),
            '\x0b' => Some(Key::KillToEnd),
            '\x0c' => Some(Key::ClearScreen),
            '\x15' => Some(Key::KillToStart),
            '\x1b' => {
                let next = read_char(handle)?;
                if next != '[' {
                    return None;
                }
                let code = read_char(handle)?;
                match code {
                    'A' => Some(Key::Up),
                    'B' => Some(Key::Down),
                    'C' => Some(Key::Right),
                    'D' => Some(Key::Left),
                    'H' => Some(Key::Home),
                    'F' => Some(Key::End),
                    c if c.is_ascii_digit() => {
                        let mut num = String::from(c);
                        while let Some(d) = read_char(handle) {
                            if d.is_ascii_digit() {
                                num.push(d);
                            } else if d == '~' {
                                return match num.as_str() {
                                    "1" | "7" => Some(Key::Home),
                                    "3" => Some(Key::Delete),
                                    "4" | "8" => Some(Key::End),
                                    _ => None,
                                };
                            } else {
                                break;
                            }
                        }
                        None
                    }
                    _ => None,
                }
            }
            c if !c.is_control() => Some(Key::Char(c)),
            _ => None,
        }
    }

    fn redraw(prompt: &str, editor: &LineEditor) {
        let text = editor.text();
        let len = text.chars().count();
        let cur = editor.cursor_pos();
        let mut out = std::io::stdout();
        let _ = write!(out, "\r\x1b[2K{}{}", prompt, text);
        if cur < len {
            let _ = write!(out, "\x1b[{}D", len - cur);
        }
        let _ = out.flush();
    }

    pub fn read_interactive_line(
        prompt: &str,
        editor: &mut LineEditor,
    ) -> Result<Option<String>, ()> {
        // SAFETY: `STD_INPUT_HANDLE` is a constant Win32 selector; null or
        // `INVALID_HANDLE` returns `Err(())` cleanly without dereferencing.
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        if handle.is_null() || handle == INVALID_HANDLE {
            return Err(());
        }
        let mut orig_mode: u32 = 0;
        // SAFETY: `handle` was checked non-null and `orig_mode` is valid stack memory.
        if unsafe { GetConsoleMode(handle, &mut orig_mode) } == 0 {
            return Err(());
        }
        let raw_mode = (orig_mode
            & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT))
            | ENABLE_VIRTUAL_TERMINAL_INPUT;
        // SAFETY: setting raw mode on validated console `handle`; guard restores `orig_mode`.
        if unsafe { SetConsoleMode(handle, raw_mode) } == 0 {
            return Err(());
        }
        let _guard = ConsoleModeGuard { handle, orig_mode };

        redraw(prompt, editor);
        loop {
            let Some(key) = read_key(handle) else {
                continue;
            };
            match editor.handle_key(key) {
                KeyAction::Continue => {}
                KeyAction::Redraw => redraw(prompt, editor),
                KeyAction::ClearScreen => {
                    let _ = write!(std::io::stdout(), "\x1b[2J\x1b[H");
                    redraw(prompt, editor);
                }
                KeyAction::Submit(line) => {
                    let _ = write!(std::io::stdout(), "\r\n");
                    let _ = std::io::stdout().flush();
                    return Ok(Some(format!("{}\n", line)));
                }
                KeyAction::Interrupt => {
                    let _ = write!(std::io::stdout(), "^C\r\n");
                    let _ = std::io::stdout().flush();
                    return Ok(Some(String::new()));
                }
                KeyAction::Eof => {
                    let _ = write!(std::io::stdout(), "\r\n");
                    let _ = std::io::stdout().flush();
                    return Ok(None);
                }
            }
        }
    }
}

#[cfg(all(unix, any(target_os = "linux", target_os = "macos")))]
mod tty {
    use core::ffi::c_int;
    use std::io::{Read, Write};

    use super::{Key, KeyAction, LineEditor};

    #[cfg(target_os = "linux")]
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Termios {
        c_iflag: u32,
        c_oflag: u32,
        c_cflag: u32,
        c_lflag: u32,
        c_line: u8,
        c_cc: [u8; 32],
        c_ispeed: u32,
        c_ospeed: u32,
        _pad: [u8; 32],
    }

    #[cfg(target_os = "linux")]
    const ICANON: u32 = 0x0002;
    #[cfg(target_os = "linux")]
    const ECHO: u32 = 0x0008;
    #[cfg(target_os = "linux")]
    const ISIG: u32 = 0x0001;
    #[cfg(target_os = "linux")]
    const IXON: u32 = 0x0400;
    #[cfg(target_os = "linux")]
    const VMIN: usize = 6;
    #[cfg(target_os = "linux")]
    const VTIME: usize = 5;

    #[cfg(target_os = "macos")]
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Termios {
        c_iflag: u64,
        c_oflag: u64,
        c_cflag: u64,
        c_lflag: u64,
        c_cc: [u8; 20],
        _pad_align: [u8; 4],
        c_ispeed: u64,
        c_ospeed: u64,
        _pad: [u8; 32],
    }

    #[cfg(target_os = "macos")]
    const ICANON: u64 = 0x0100;
    #[cfg(target_os = "macos")]
    const ECHO: u64 = 0x0008;
    #[cfg(target_os = "macos")]
    const ISIG: u64 = 0x0080;
    #[cfg(target_os = "macos")]
    const IXON: u64 = 0x0200;
    #[cfg(target_os = "macos")]
    const VMIN: usize = 16;
    #[cfg(target_os = "macos")]
    const VTIME: usize = 17;

    const STDIN_FILENO: c_int = 0;
    const TCSAFLUSH: c_int = 2;

    unsafe extern "C" {
        fn tcgetattr(fd: c_int, termios_p: *mut Termios) -> c_int;
        fn tcsetattr(fd: c_int, optional_actions: c_int, termios_p: *const Termios) -> c_int;
    }

    struct TermiosGuard {
        orig: Termios,
    }

    impl Drop for TermiosGuard {
        fn drop(&mut self) {
            // SAFETY: `self.orig` holds the validated prior `Termios` state
            // captured from `tcgetattr(STDIN_FILENO, ...)`.
            unsafe {
                let _ = tcsetattr(STDIN_FILENO, TCSAFLUSH, &self.orig);
            }
        }
    }

    fn read_byte(stdin: &mut std::io::StdinLock<'_>) -> Option<u8> {
        let mut b = [0u8; 1];
        match stdin.read(&mut b) {
            Ok(1) => Some(b[0]),
            _ => None,
        }
    }

    fn read_utf8_char(first: u8, stdin: &mut std::io::StdinLock<'_>) -> Option<char> {
        let width = if first & 0x80 == 0 {
            1
        } else if first & 0xE0 == 0xC0 {
            2
        } else if first & 0xF0 == 0xE0 {
            3
        } else if first & 0xF8 == 0xF0 {
            4
        } else {
            return None;
        };
        let mut buf = [0u8; 4];
        buf[0] = first;
        for slot in buf.iter_mut().take(width).skip(1) {
            *slot = read_byte(stdin)?;
        }
        core::str::from_utf8(&buf[..width]).ok()?.chars().next()
    }

    fn read_key(stdin: &mut std::io::StdinLock<'_>) -> Option<Key> {
        let b = read_byte(stdin)?;
        match b {
            b'\r' | b'\n' => Some(Key::Enter),
            0x03 => Some(Key::Interrupt),
            0x04 => Some(Key::Eof),
            0x01 => Some(Key::Home),
            0x05 => Some(Key::End),
            0x08 | 0x7f => Some(Key::Backspace),
            0x0b => Some(Key::KillToEnd),
            0x0c => Some(Key::ClearScreen),
            0x15 => Some(Key::KillToStart),
            0x1b => {
                if read_byte(stdin)? != b'[' {
                    return None;
                }
                match read_byte(stdin)? {
                    b'A' => Some(Key::Up),
                    b'B' => Some(Key::Down),
                    b'C' => Some(Key::Right),
                    b'D' => Some(Key::Left),
                    b'H' => Some(Key::Home),
                    b'F' => Some(Key::End),
                    c if c.is_ascii_digit() => {
                        let mut num = String::from(c as char);
                        while let Some(d) = read_byte(stdin) {
                            if d.is_ascii_digit() {
                                num.push(d as char);
                            } else if d == b'~' {
                                return match num.as_str() {
                                    "1" | "7" => Some(Key::Home),
                                    "3" => Some(Key::Delete),
                                    "4" | "8" => Some(Key::End),
                                    _ => None,
                                };
                            } else {
                                break;
                            }
                        }
                        None
                    }
                    _ => None,
                }
            }
            other => {
                let c = read_utf8_char(other, stdin)?;
                if c.is_control() {
                    None
                } else {
                    Some(Key::Char(c))
                }
            }
        }
    }

    fn redraw(prompt: &str, editor: &LineEditor) {
        let text = editor.text();
        let len = text.chars().count();
        let cur = editor.cursor_pos();
        let mut out = std::io::stdout();
        let _ = write!(out, "\r\x1b[2K{}{}", prompt, text);
        if cur < len {
            let _ = write!(out, "\x1b[{}D", len - cur);
        }
        let _ = out.flush();
    }

    pub fn read_interactive_line(
        prompt: &str,
        editor: &mut LineEditor,
    ) -> Result<Option<String>, ()> {
        let mut orig = core::mem::MaybeUninit::<Termios>::zeroed();
        // SAFETY: `orig` is properly aligned and sized for `Termios`, and if
        // `tcgetattr` succeeds the entire struct is initialized.
        if unsafe { tcgetattr(STDIN_FILENO, orig.as_mut_ptr()) } != 0 {
            return Err(());
        }
        // SAFETY: `tcgetattr` returned 0 above, so `orig` is initialized.
        let orig = unsafe { orig.assume_init() };
        let mut raw = orig;
        raw.c_lflag &= !(ICANON | ECHO | ISIG);
        raw.c_iflag &= !IXON;
        raw.c_cc[VMIN] = 1;
        raw.c_cc[VTIME] = 0;
        // SAFETY: setting `raw` derived from `orig`; `TermiosGuard` restores `orig`.
        if unsafe { tcsetattr(STDIN_FILENO, TCSAFLUSH, &raw) } != 0 {
            return Err(());
        }
        let _guard = TermiosGuard { orig };

        let stdin_handle = std::io::stdin();
        let mut stdin = stdin_handle.lock();
        redraw(prompt, editor);
        loop {
            let Some(key) = read_key(&mut stdin) else {
                continue;
            };
            match editor.handle_key(key) {
                KeyAction::Continue => {}
                KeyAction::Redraw => redraw(prompt, editor),
                KeyAction::ClearScreen => {
                    let _ = write!(std::io::stdout(), "\x1b[2J\x1b[H");
                    redraw(prompt, editor);
                }
                KeyAction::Submit(line) => {
                    let _ = write!(std::io::stdout(), "\r\n");
                    let _ = std::io::stdout().flush();
                    return Ok(Some(format!("{}\n", line)));
                }
                KeyAction::Interrupt => {
                    let _ = write!(std::io::stdout(), "^C\r\n");
                    let _ = std::io::stdout().flush();
                    return Ok(Some(String::new()));
                }
                KeyAction::Eof => {
                    let _ = write!(std::io::stdout(), "\r\n");
                    let _ = std::io::stdout().flush();
                    return Ok(None);
                }
            }
        }
    }
}

#[cfg(not(any(windows, all(unix, any(target_os = "linux", target_os = "macos")))))]
mod tty {
    use super::LineEditor;

    pub fn read_interactive_line(
        _prompt: &str,
        _editor: &mut LineEditor,
    ) -> Result<Option<String>, ()> {
        Err(())
    }
}

pub fn run() {
    println!("spid3r repl - :quit to leave, :help for commands");
    let (mut interp, mut env) = fresh_session();
    let mut buffer = String::new();
    // Names bound by earlier lines, so a later line can use them.
    let mut known: Vec<String> = Vec::new();
    let mut editor = LineEditor::new();
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();

    loop {
        let prompt = if buffer.is_empty() { "> " } else { ". " };
        let line = if interactive {
            match tty::read_interactive_line(prompt, &mut editor) {
                Ok(Some(l)) => l,
                Ok(None) => break,
                Err(()) => {
                    print!("{}", prompt);
                    let _ = std::io::stdout().flush();
                    let mut fallback = String::new();
                    match std::io::stdin().read_line(&mut fallback) {
                        Ok(0) => {
                            println!();
                            break;
                        }
                        Ok(_) => fallback,
                        Err(_) => break,
                    }
                }
            }
        } else {
            print!("{}", prompt);
            let _ = std::io::stdout().flush();
            let mut plain = String::new();
            match std::io::stdin().read_line(&mut plain) {
                Ok(0) => {
                    println!();
                    break;
                }
                Ok(_) => plain,
                Err(_) => break,
            }
        };

        let trimmed = line.trim();
        if buffer.is_empty() {
            match trimmed {
                ":quit" | ":q" => break,
                ":help" | ":h" => {
                    editor.push_history(trimmed);
                    println!(":quit leave   :help this text   :reset clear bindings");
                    continue;
                }
                ":reset" => {
                    editor.push_history(trimmed);
                    // A reset must rebuild the whole per-session state, not
                    // just the name list: the const set lives in the global
                    // environment, so consts from earlier lines would
                    // otherwise survive a reset and reject `let`/assignment
                    // of the same name. fresh_session() is the same
                    // constructor startup uses, so the two cannot drift.
                    (interp, env) = fresh_session();
                    known.clear();
                    println!("bindings cleared");
                    continue;
                }
                "" => continue,
                _ => {}
            }
        }
        buffer.push_str(&line);
        if !balanced(&buffer) {
            continue;
        }
        let source = std::mem::take(&mut buffer);
        editor.push_history(source.trim_end_matches(['\r', '\n']));
        let is_expr = is_bare_expr(&source);
        match eval_line(&interp, &env, &source, &mut known) {
            Ok(Some(v)) => {
                // C-02: a statement whose value is `null` (a println, a let,
                // an assignment) echoes nothing — the stray `null` line after
                // every side-effecting expression was noise, not information.
                if is_expr && !matches!(v, crate::value::Value::Null) {
                    println!("{}", display_value(&v));
                }
            }
            Ok(None) => {}
            Err(d) => {
                eprintln!("{}", d);
                if let Some(h) = d.help() {
                    eprintln!("  = help: {}", h);
                }
            }
        }
    }
}

/// The per-session state a fresh REPL starts with: a new interpreter (which
/// owns a fresh global environment, including its empty const set) and a clone
/// of that environment handle. Both REPL startup and `:reset` build from here,
/// so a reset produces exactly the state a fresh session has.
fn fresh_session() -> (Interp, crate::environment::Env) {
    let interp = Interp::new(Arc::from("<repl>"), modules::default_module_dirs());
    let env = interp.globals.clone();
    (interp, env)
}

/// True when braces, brackets, and parens balance outside strings and comments.
fn balanced(src: &str) -> bool {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if in_line_comment {
            if c == '\n' {
                in_line_comment = false;
            }
        } else if in_block_comment {
            if c == '*' && next == Some('/') {
                in_block_comment = false;
                i += 1;
            }
        } else if in_string {
            if c == '\\' {
                i += 1;
            } else if c == '"' {
                in_string = false;
            }
        } else {
            match c {
                '/' if next == Some('/') => {
                    in_line_comment = true;
                    i += 1;
                }
                '/' if next == Some('*') => {
                    in_block_comment = true;
                    i += 1;
                }
                '"' => in_string = true,
                '{' | '[' | '(' => depth += 1,
                '}' | ']' | ')' => depth -= 1,
                _ => {}
            }
        }
        i += 1;
    }
    depth <= 0 && !in_string && !in_block_comment
}

/// A REPL line is a "bare expression" when it is a single expression statement,
/// so its value gets printed.
fn is_bare_expr(src: &str) -> bool {
    let trimmed = src.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Starts with a statement keyword: not a bare expression.
    const STARTERS: &[&str] = &[
        "let ",
        "const ",
        "if ",
        "while ",
        "for ",
        "fn ",
        "class ",
        "interface ",
        "return ",
        "break",
        "continue",
        "import ",
        "try ",
        "throw ",
        "spawn ",
        "public ",
        "private ",
        "async ",
    ];
    if STARTERS.iter().any(|s| trimmed.starts_with(s)) {
        return false;
    }
    // A bare expression is echoed whether or not it ends in `;`; the trailing
    // semicolon is optional noise in the REPL.
    true
}

fn eval_line(
    interp: &Interp,
    env: &crate::environment::Env,
    src: &str,
    known: &mut Vec<String>,
) -> Result<Option<crate::value::Value>, crate::diagnostics::Diagnostic> {
    let file: Arc<str> = Arc::from("<repl>");
    let toks = Lexer::new(src, file.clone()).tokenize()?;
    let program = match parser::parse(toks) {
        Ok(p) => p,
        Err(d) => {
            // A bare expression may omit its trailing `;` in the REPL (its
            // value is echoed either way). When the line failed to parse, is
            // a bare expression, and does not already end in `;`, retry the
            // parse once with `;` appended. If that also fails, report the
            // original diagnostic so genuinely broken input keeps its
            // accurate error. The retry happens at parse time only, before
            // any semantic check or execution, so nothing can run twice.
            if is_bare_expr(src) && !src.trim_end().ends_with(';') {
                let retried = format!("{};", src.trim_end());
                match Lexer::new(&retried, file.clone())
                    .tokenize()
                    .and_then(parser::parse)
                {
                    Ok(p) => p,
                    Err(_) => return Err(d),
                }
            } else {
                // C-02: a statement missing its terminator used to fail with
                // "expected ';' after let, found end of file" pointing at
                // <repl>:2 — a line that does not exist. The terminator is
                // still required (the language requires it everywhere), but
                // the diagnostic now names the real problem against the line
                // that was typed.
                if d.message.contains("found end of file") && !src.trim_end().ends_with(';') {
                    let mut friendly = d.clone().with_help(
                        "statements (let, const, import, return, throw, ...) require a trailing ';'; bare expressions do not",
                    );
                    friendly.message = "missing ';' at end of statement".to_string();
                    let len = src.trim_end().chars().count() as u32;
                    friendly.span =
                        Some(crate::diagnostics::Span::new(file.clone(), 1, len + 1, 1));
                    return Err(friendly);
                }
                return Err(d);
            }
        }
    };
    semantic::check_repl(&program, known)?;
    // Evaluate on a large-stack thread, same as `run`: deep recursion must
    // hit the interpreter's depth guard, not the OS stack.
    let result = std::thread::scope(|s| {
        let handle = std::thread::Builder::new()
            .stack_size(crate::interpreter::REPL_STACK_BYTES)
            .spawn_scoped(s, || {
                let mut last = None;
                for stmt in &program.stmts {
                    match interp.exec_stmt(stmt, env) {
                        Flow::Value(v) => last = Some(v),
                        Flow::Return(v) => last = Some(v),
                        Flow::Error(d) => return Err(*d),
                        _ => {}
                    }
                }
                Ok(last)
            })
            .expect("failed to start the repl evaluation thread");
        handle.join().unwrap_or_else(|_| {
            Err(crate::diagnostics::Diagnostic::new(
                crate::diagnostics::Phase::Runtime,
                "the repl thread panicked; this is a bug in spid3r",
            ))
        })
    });
    if result.is_ok() {
        // remember what this line bound, so the next line can use it
        for n in semantic::declared_names(&program) {
            if !known.contains(&n) {
                known.push(n);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_editor_cursor_editing_and_kill_keys() {
        let mut ed = LineEditor::new();
        for c in "let x = 42;".chars() {
            assert_eq!(ed.handle_key(Key::Char(c)), KeyAction::Redraw);
        }
        assert_eq!(ed.text(), "let x = 42;");
        assert_eq!(ed.cursor_pos(), 11);

        assert_eq!(ed.handle_key(Key::Home), KeyAction::Redraw);
        assert_eq!(ed.cursor_pos(), 0);
        assert_eq!(ed.handle_key(Key::Right), KeyAction::Redraw);
        assert_eq!(ed.handle_key(Key::Delete), KeyAction::Redraw);
        assert_eq!(ed.text(), "lt x = 42;");
        assert_eq!(ed.handle_key(Key::Char('e')), KeyAction::Redraw);
        assert_eq!(ed.text(), "let x = 42;");

        assert_eq!(ed.handle_key(Key::End), KeyAction::Redraw);
        assert_eq!(ed.handle_key(Key::Left), KeyAction::Redraw);
        assert_eq!(ed.handle_key(Key::KillToEnd), KeyAction::Redraw);
        assert_eq!(ed.text(), "let x = 42");
        assert_eq!(ed.handle_key(Key::KillToStart), KeyAction::Redraw);
        assert_eq!(ed.text(), "");
        assert_eq!(ed.handle_key(Key::Eof), KeyAction::Eof);
    }

    #[test]
    fn line_editor_history_navigation_and_draft_restore() {
        let mut ed = LineEditor::new();
        ed.push_history("let a = 1;");
        ed.push_history("let a = 1;");
        ed.push_history("   ");
        ed.push_history("let b = 2;");
        assert_eq!(ed.history(), &["let a = 1;", "let b = 2;"]);

        assert_eq!(ed.handle_key(Key::Char('x')), KeyAction::Redraw);
        assert_eq!(ed.handle_key(Key::Up), KeyAction::Redraw);
        assert_eq!(ed.text(), "let b = 2;");
        assert_eq!(ed.handle_key(Key::Up), KeyAction::Redraw);
        assert_eq!(ed.text(), "let a = 1;");
        assert_eq!(ed.handle_key(Key::Down), KeyAction::Redraw);
        assert_eq!(ed.text(), "let b = 2;");
        assert_eq!(ed.handle_key(Key::Down), KeyAction::Redraw);
        assert_eq!(ed.text(), "x");

        assert_eq!(
            ed.handle_key(Key::Enter),
            KeyAction::Submit("x".to_string())
        );
        assert_eq!(ed.text(), "");
    }
}
