# Spid3r Native Calculator

A flagship native calculator application for Windows and cross-platform desktop, written in pure Spid3r.

## Running the Native GUI Window

To launch the real desktop GUI window with interactive mouse buttons, keypad, display, and history tape:

```bash
spid3r gui demo_apps/native_calculator/calculator.matrix
```

### GUI Features
- **Native OS Window**: Rendered using native desktop webview with Windows 11 aesthetics and titlebar.
- **Full Keypad**: Digits `0-9`, operators `+`, `-`, `×`, `÷`, `%`, `±`, `√`, `x²`, `1/x`, `CE`, `C`, `⌫`, `=`.
- **Keyboard Shortcuts**: Type numbers and operations directly on your physical keyboard.
- **History Tape**: Click any past calculation result on the side panel to recall it back into the display.



---

## Standalone Native GUI Application (`calculator.exe`)

`main.rs` in this directory is a small Rust launcher wired into the build as the
`calculator` binary target (`required-features = ["gui"]`), so the calculator can be
shipped as a standalone Windows executable:

```bash
cargo build --bin calculator
.\target\debug\calculator.exe        # run from the repository root
```

The launcher is `#![windows_subsystem = "windows"]`, so double-clicking the binary in
File Explorer opens the calculator window directly — no terminal window and no
command-line prompt. It locates `demo_apps/native_calculator/calculator.matrix` next
to the executable (for an installed layout) or relative to the current directory, so
run it from the repository root. On Windows the window needs the WebView2 runtime,
which ships with Windows 11 and Microsoft Edge.

