# Spid3r Native GUI Examples

Spid3r features a lightweight, native desktop GUI subsystem built directly into the language toolchain. It pairs Spid3r's tree interpreter with an OS-native webview ([Tao](https://github.com/tauri-apps/tao) + [Wry](https://github.com/tauri-apps/wry) / WebView2 on Windows) via a zero-latency, asynchronous bidirectional IPC bridge.

---

## How It Works

```
┌───────────────────────────────────────────────┐
│              Spid3r GUI Runtime               │
│                                               │
│  1. Program runs on tree engine               │
│     - Registers handlers: gui_on("event", fn) │
│     - Sets window title:  gui_title("App")    │
│     - Queues UI commands: gui_eval("...")     │
│                                               │
│  2. Native OS Window opens (Tao + Wry)        │
│     - Injects bridge: window.spid3r.emit(...) │
│     - Renders UI (built-in or custom --html)  │
│                                               │
│  3. "ready" event fires                       │
│     - Replays all queued DOM operations       │
│                                               │
│  4. Bidirectional Event Loop                  │
│     - Frontend JS: window.spid3r.emit(ev, pl) │
│     - Backend Spid3r: calls handler(ev, pl)   │
│     - Pushes updates back: gui_eval(...)      │
└───────────────────────────────────────────────┘
```

1. **Setup Phase**: The `.matrix` script executes first. It registers event listeners with `gui_on()`, sets window attributes with `gui_title()`, and queues initial DOM manipulations.
2. **Window Creation**: A native OS desktop window opens containing an embedded webview with modern hardware acceleration.
3. **Bridge Injection**: Before any webpage script runs, Spid3r injects the native IPC bridge (`window.spid3r.emit(event, payload)`).
4. **Interactive Loop**: Clicking or interacting with elements in HTML dispatches events to your Spid3r functions, which execute business logic (math, file I/O, ML, concurrency) and push updates back into the UI with `gui_eval()`.

---

## Included Examples

### 1. Minimal Reactive Counter (`counter.matrix`)
A self-contained demonstration of dynamic UI generation without any external HTML file:
- Injects markup dynamically into `#spid3r-root` using `gui_set_html()`.
- Captures button clicks via `onclick="window.spid3r.emit('click', 'go')"`.
- Bumps the counter in Spid3r and updates the DOM label with `gui_eval()`.

**Run GUI Window:**
```bash
spid3r gui examples/gui/counter.matrix
```

**Run Headless / CI Dry-Run (No Window Needed):**
```bash
spid3r gui examples/gui/counter.matrix --dry-run
```

---

### 2. DataLab & Analytics Studio (`data_studio/`)
A flagship desktop analytics application demonstrating how to build rich, professional GUI software in pure Spid3r:
- **Custom UI**: Uses `--html examples/gui/data_studio/index.html` featuring a glassmorphism dark theme, live canvas charts, and responsive layouts.
- **Data Analytics (`import "stats"`)**: Computes mean, median, sample variance, standard deviation, percentiles (P25, P75), and IQR on arbitrary datasets.
- **Machine Learning (`import "ml"`)**: Fits multi-point linear regression models, computing slope, intercept, predictions, and $R^2$ goodness-of-fit metrics.
- **Linear Algebra**: Computes $2 \times 2$ matrix determinants, traces, full matrix inverses, and real eigenvalues.

**Run GUI Window:**
```bash
spid3r gui examples/gui/data_studio/app.matrix --html examples/gui/data_studio/index.html
```

*Or on Windows, simply double-click [run_gui.bat](run_gui.bat).*

**Run Headless / CI Dry-Run:**
```bash
spid3r gui examples/gui/data_studio/app.matrix --dry-run
```

---

## GUI Language Natives Reference

| Function | Signature | Description |
| :--- | :--- | :--- |
| `gui_title` | `gui_title(title: string) -> null` | Sets the desktop window title bar text. |
| `gui_on` | `gui_on(event: string, handler: fn(event, payload)) -> null` | Registers a Spid3r callback function triggered when the UI emits `event`. |
| `gui_emit` | `gui_emit(event: string, payload: any) -> null` | Synthetically emits an event into the handler queue (useful for testing or initial state pushes). |
| `gui_eval` | `gui_eval(javascript: string) -> null` | Executes arbitrary JavaScript code inside the active webview context. |
| `gui_set_html` | `gui_set_html(html: string) -> null` | Safely replaces the inner HTML of the default `<div id="spid3r-root">` container. |

### Frontend JavaScript Bridge API
In any HTML document loaded by Spid3r (`--html`), the global object `window.spid3r` is automatically available:
```javascript
// Send an event with an optional payload (string, number, or JSON string) to Spid3r
window.spid3r.emit("compute_stats", JSON.stringify([12.5, 45.2, 88.0]));
```

---

## Flagship Standalone Executables
For distributing standalone desktop apps without requiring users to install the Spid3r CLI or terminal:
- See [examples/demo_apps/native_calculator/](../demo_apps/native_calculator/README.md) for a complete native calculator bundled into `calculator.exe` with no terminal window (`#![windows_subsystem = "windows"]`).
- See [docs/GUI.md](../../docs/GUI.md) for comprehensive deep-dive documentation on the GUI subsystem.
