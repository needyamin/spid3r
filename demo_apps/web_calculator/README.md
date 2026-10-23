# Spid3r Calculator

A desktop calculator written in Spid3r, using the `spid3r gui` webview backend. It has a
keypad and display on the left and a history panel on the right.

## Features

- Basic arithmetic (`+ − × ÷`), chained left to right like a desk calculator (`2 + 3 × 4 = 20`).
- `%`, `±`, `1/x`, `x²`, `√x`, backspace, `CE` (clear entry), and `C` (clear all).
- Repeated `=` repeats the last step (`2 + 3 = = =` gives `11`), and `5 + =` gives `10`.
- Thousands separators, up to 10 decimal places, and scientific notation for very large or very small results.
- Clear error messages: `Cannot divide by zero`, `Invalid input`, and `Overflow`.
- A history panel with the last 8 calculations. Click an entry to reuse its result.
- Full keyboard support.

| Key | Action |
|---|---|
| `0`–`9`, `.` or `,` | Type a number |
| `+` `-` `*` (or `x`) `/` | Operators |
| `Enter` or `=` | Evaluate |
| `%` | Percent |
| `Backspace` | Delete the last digit |
| `Delete` | Clear entry (`CE`) |
| `Esc` | Clear all (`C`) |

## Run it

You need a `spid3r` built with the GUI backend: the installer's binary, or a default build
from the repository root (the `gui` feature is on by default):

```powershell
cargo build --release
.\target\release\spid3r.exe gui demo_apps\web_calculator\calculator.matrix
```

With the installer, run `spid3r gui demo_apps\web_calculator\calculator.matrix` from the
repository root (or from the install directory, adjusting the script path).

On Windows, the window needs the WebView2 runtime, which ships with Windows 11 and Microsoft Edge.

To replay the UI without opening a window (this prints the JavaScript sent to the page):

```powershell
.\target\release\spid3r.exe gui demo_apps\web_calculator\calculator.matrix --dry-run
```

## Files

| File | Purpose |
|---|---|
| `calculator.matrix` | GUI layer: builds the page, forwards clicks and keys, and renders state |
| `engine.matrix` | Calculator state machine and number formatting (pure Spid3r) |
| `index.html` | The page the GUI backend loads (markup, styles, and the key-forwarding script) |

## How it works

`spid3r gui` runs `calculator.matrix` first and then opens a window. When the page loads, the
`ready` handler injects the markup, styles, and a small script. Every button click or key press
calls `window.spid3r.emit("key", …)`. The `on_key` handler passes the key to `press()` in the
engine, then `render()` sends the new display, expression, active operator, and history back to
the page as one JSON object.
