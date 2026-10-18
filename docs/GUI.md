# Spid3r GUI layer

`spid3r gui` runs a Spid3r program inside a desktop window with an embedded
webview — the program is the "app", the page is the UI. It is gated behind the
optional `gui` Cargo feature (`wry` + `tao`); the core toolchain stays
zero-dependency. See [BUILDING.md](BUILDING.md) for building with the feature.

```
cargo run --features gui -- gui examples/gui/counter.matrix
```

## The model

1. **The program runs first.** `spid3r gui prog.matrix` parses and
   semantically checks the program, then runs it on the tree interpreter
   (the most feature-complete engine). The program *builds the UI*: it
   registers handlers with `gui_on` and queues initial JavaScript with
   `gui_eval` / `gui_set_html`. Nothing touches the window yet.
2. **A window opens.** A `tao` window with a `wry` webview shows a page: the
   built-in default page, or your own with `--html page.html`
   (`file://` URL). Before any page script runs, the bridge script below is
   injected.
3. **`ready` fires.** When the page finishes loading, every queued JavaScript
   command is evaluated in order.
4. **The page talks back.** JS calls `window.spid3r.emit(event, payload)`;
   the window's event loop dispatches each event to the registered handler as
   `handler(event, payload)` on the interpreter's thread and evaluates any
   JavaScript the handler queues.

Because the interpreter runs before the window exists, `gui_*` calls made
during setup are queued and replayed at step 3.

## The bridge script

Injected before any page script:

```js
window.spid3r = {
  emit: function (event, payload) {
    window.ipc.postMessage(JSON.stringify({ event: event, payload: payload }));
  }
};
```

The built-in page contains `<div id="spid3r-root">`, the element
`gui_set_html` replaces. A custom `--html` page should contain the same id.

## Language natives

| Native | Effect |
| --- | --- |
| `gui_eval(js)` | Evaluate `js` in the page, in order. |
| `gui_set_html(html)` | Set `#spid3r-root`'s content (JSON-quoted into the JS command). |
| `gui_on(event, handler)` | Call `handler(event, payload)` for every `event` the page emits. Re-registering replaces. |
| `gui_emit(event, payload)` | Queue an event as if the page emitted it (program-driven UI updates; also how tests drive handlers). |
| `gui_title(text)` | Window title (read when the window opens; default `spid3r gui`). |

All five take strings except `gui_on`'s handler, which must be a function
(wrong types and arities are clean errors, not panics). There is no DOM
binding layer: the program speaks JavaScript, exactly like a web app would.

## Dry-run mode

```
spid3r gui prog.matrix --dry-run
```

Replays the whole bridge **without opening a window**: handlers are
dispatched, a `ready` event is injected, and every queued JavaScript command
is printed as `js: …` instead of evaluated. This is what
`tests/gui_tests.rs` asserts against — a real window cannot be driven
headlessly — and it doubles as a debugging aid.

```
$ spid3r gui examples/gui/counter.matrix --dry-run
js: document.getElementById('count').textContent = "clicks: 1";
js: document.getElementById('count').textContent = "clicks: 2";
js: document.getElementById('spid3r-root').innerHTML = "<button id=\"go\" onclick=\"window.spid3r.emit('click','go')\">click me</button> <span id=\"count\"></span>";
js: document.getElementById('count').textContent = "clicks: 2";
```

## Where GUI examples live

`examples/gui/`. GUI programs are deliberately **outside** `examples/`:
`spid3r test` and the differential suite run every flat `examples/*.matrix`
on all three engines (tree, VM, `exec`), and a GUI program cannot run without
a window.

## Threading and safety

The webview IPC callback may run on a different OS thread than the
interpreter, so the bridge is a `Mutex`-guarded process-global. Every runtime
value is `Arc`-based, so handler closures cross that boundary safely. The
event loop wakes through a user event (`Msg::Event`), and all handler
invocation happens on the window's thread — no `unsafe`, no data races. A
handler that fails prints its diagnostic to stderr; the window stays up.

## Limitations (v1)

- One window per process; the window is created only by `spid3r gui`.
- Windows needs the WebView2 runtime (ships with Windows 11 and Edge); macOS
  uses WKWebView, Linux WebKitGTK — provided by `wry`.
- GUI natives are not available to `spid3r run`, `run --engine vm`, `exec`,
  or `spid3r build` (the AOT backend does not link the GUI bridge). Running a
  GUI program with those commands fails with a clean diagnostic.
- The window itself is not covered by automated tests — only the bridge
  (unit tests in `src/gui/bridge.rs`) and the end-to-end replay
  (`--dry-run`) are.
