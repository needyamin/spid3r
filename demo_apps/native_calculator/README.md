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

The standalone binary [calculator.exe](file:///c:/Users/needyamin/OneDrive/Desktop/GaliLang/calculator.exe) in the project root is a genuine Windows GUI executable (`windows_subsystem`).

- Double-click **`calculator.exe`** in Windows Explorer or run `.\calculator.exe` to immediately launch the Native Desktop GUI Calculator window.
- No terminal window, no command-line prompt, and no external dependencies required.

