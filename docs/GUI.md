# Spid3r GUI layer

`spid3r gui` runs a Spid3r program inside a desktop window with an embedded
webview — the program is the "app", the page is the UI. It lives behind the
`gui` Cargo feature (`wry` + `tao`), which is **on by default**; build with
`--no-default-features` for the zero-dependency core. See
[BUILDING.md](BUILDING.md).

```
cargo run -- gui examples/gui/counter.matrix
```

## The model

1. **The program runs first.** `spid3r gui prog.matrix` parses and
   semantically checks the program, then runs it on the tree interpreter
   (the most feature-complete engine). `spid3r gui prog.matrix --engine vm`,
   or any `.matrixc` argument, runs it on the bytecode VM instead — same
   natives, same bridge, and the engine the shipped `.matrixc` payload uses.
   The program *builds the UI*: it
   registers handlers with `gui_on` and queues initial JavaScript with
   `gui_eval` / `gui_set_html`. Nothing touches the window yet.
2. **A window opens.** A `tao` window with a `wry` webview shows a page: the
   built-in default page, or your own with `--html page.html` (served over
   `spid3r://app/`, see below). Before any page script runs, the bridge script
   below is injected.
3. **`ready` fires.** When the page finishes loading, every queued JavaScript
   command is evaluated in order.
4. **The page talks back.** JS calls `window.spid3r.emit(event, payload)`;
   the window's event loop dispatches each event to the registered handler as
   `handler(event, payload)` on the interpreter's thread and evaluates any
   JavaScript the handler queues.

Because the interpreter runs before the window exists, `gui_*` calls made
during setup are queued and replayed at step 3.

## `--html` and the served root

`--html <path>` may be absolute or relative to the working directory. The page
and its assets are served over `spid3r://app/`, not `file://`, and the served
root depends on where the page is:

- a page **under** the working directory keeps the working directory as the
  root, so `../data.json` from `ui/index.html` resolves exactly as it did under
  `file://`;
- a page **outside** it — the usual case for an absolute path, or
  `--html ../site/page.html` — is served with its own directory as the root, so
  its sibling assets stay reachable.

Both roots are canonicalised before use, and the entry is *derived* from the
resolved path rather than copied from the command line. A relative `--html`
that climbed out of the working directory used to reach the server as a `..`
path segment, which the traversal check refused: the window came up blank with
no diagnostic, even though the file existed.

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
| `gui_on(event, handler)` | Call `handler(event, payload)` for every `event` the page emits. Re-registering replaces. Parameters the handler does not read never trigger the unused-parameter lint — the (event, payload) signature is imposed by the runtime; prefixing unused parameters with `_` also works. |
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

## Packaging a desktop app

`spid3r gui` opens the window, but the `spid3r` executable itself is a console
program: on Windows it brings up a terminal window and carries the default
toolchain icon. `spid3r build` closes that gap:

```bash
spid3r build app.matrix -o App.exe --release --icon app.ico --subsystem windows
```

| Flag | Effect |
| --- | --- |
| `--icon <path.ico>` | Embeds the icon as `RT_ICON` + `RT_GROUP_ICON`; every size in the file is kept. |
| `--subsystem windows` | PE subsystem `2` (GUI) instead of `3`, plus `-entry:mainCRTStartup`. Windows allocates no console. Windows-only; ignored by the ELF/Mach-O linkers. |
| `--allow-gui` | Build a program that uses GUI natives even though the AOT runtime does not link the bridge. |
| `--embed <path>` | Appends a file or directory to the executable so `gui --payload` can run it in place. Repeatable; relative paths resolve beside the source. |
| `--bundle-runtime` | Also appends the running `spid3r` executable, so the app starts its own runtime copy on a machine with no Spid3r installed. Implies a payload. |
| `--product-name` / `--version` | `RT_VERSION` metadata (Properties → Details). |

Because the AOT runtime registers the standard library but not the GUI bridge,
`gui_on` and friends resolve to `null` in the native binary. The supported shape
is therefore a **launcher**: the executable does its startup work, then hands
the program to the installed `spid3r gui` runtime.

```matrix
fn launch() {
    // The working directory is NOT this program's folder when Windows starts
    // it from a shortcut, the Run dialog, a scheduled task, or Start-Process.
    let self_path = exe_path().unwrap_or("");
    if self_path != "" { chdir(dirname(self_path)); }

    spawn_process("spid3r", ["gui", "--payload", self_path],
                  map("hidden", true)).unwrap();
}
```

Two details decide whether the window ever appears:

- **Anchor the working directory.** A GUI-subsystem process that resolves
  `ui/index.html` against an inherited directory finds nothing and exits with no
  message — it has no console to print to. `exe_path()` + `chdir(dirname(...))`
  fixes it.
- **Hide the handoff.** Without `map("hidden", true)` the child inherits a
  console and flashes a terminal (`CREATE_NO_WINDOW` on Windows; ignored
  elsewhere).

### Putting the app inside the executable

The launcher needs `spid3r` installed (or on `PATH`), and the GUI engine is an
interpreter — it reads the program source at startup. Both are why a naive
`spid3r build` output cannot travel alone: move the exe and the source it was
compiled from is left behind.

`--embed` closes that gap by appending the source and assets to the executable:

```bash
spid3r build app.matrix -o App.exe --allow-gui --subsystem windows \
    --embed app.matrix --embed ui
```

Relative `--embed` paths resolve beside the source file, and the embedded names
keep that layout (`ui/index.html`, `ui/app.js`). `spid3r gui --payload App.exe`
then reads them out of the binary and serves `ui/` from memory over the
`spid3r://app/` protocol — no temp directory, no sidecar files.

The same flag also accepts a loose layout: with no payload present it falls back
to a `.matrix` and `ui/` beside the executable, so a build made without `--embed`
still runs while it is developed.

`--html <name>` selects a page by its **payload entry name** (`ui/index.html`).
A file on disk is *not* accepted here: the payload is the app, and a page picked
up from whatever directory the process happens to run in is not its UI. A name
that is not an entry is an error that lists the `.html` entries the payload does
have — it used to be used as the page anyway and answered 404 for every request.

Embedded contents are XOR-obfuscated, not encrypted — the key is in the binary,
so this defeats `strings` and a text editor, not a determined reader. Note that
string *literals* in the program are also compiled into the AOT image, so they
remain visible regardless of `--embed` (use `--bytecode`, below, to keep them
out).

A source payload keeps working on a machine with **no `stdlib/` directory**: the
`stdlib/*.matrix` sources are compiled into the toolchain, and module resolution
falls back to that table when the filesystem search fails (a project-local
`stdlib/` still wins). Without it, `import "semver"` in a packaged app failed
with `module 'semver' not found` on exactly the clean machine the single-file
build exists for.

`--payload` also runs the webview on an **in-memory profile**: no HTTP cache, no
code cache, no history. Without that, Chromium copies the served page and its
scripts into the user profile, putting the UI source back on disk and undoing
the point of packaging. Pass `--persist` to keep the profile on disk and restore
`localStorage` across restarts; programs that need durable state should use the
bridge instead.

The on-disk profile lives under `%LOCALAPPDATA%\Spid3r\WebView2` and is chosen
per app, not once for the whole machine:

| Run | Folder |
| --- | --- |
| packaged app, no `--persist` | `ephemeral\<pid>` — one folder per process |
| `--persist` | `persist\<app-id>` — one folder per app |

`<app-id>` is the first 16 hex characters of the SHA-256 of the app's canonical
path (the payload executable for `--payload`, else the program file), so the
same app keeps its storage across restarts while two apps never share one.
Sharing a single folder meant sharing an origin, which let app A read app B's
`localStorage` under `--persist`. Packaged apps also get a per-process folder
because WebView2 does not support several processes on one user-data folder:
launching the same app three to five times at once failed intermittently with
`HRESULT(0x80070057)`. Folders under `ephemeral\` are pruned at startup once
they are a day old. An explicitly set `WEBVIEW2_USER_DATA_FOLDER` is respected
and overrides all of this.

**Known limitation:** two *concurrent* instances of a `--persist` app still
share one folder, so they can still collide the same way. Durable state and
several simultaneous copies are contradictory demands on WebView2 — pick one.

A payload is data appended after the PE image, which the Windows loader ignores.
It is written after linking, so `--icon` and `--version` still work, but any
external tool that rewrites the executable (signing, for example) will drop it.

### Shipping the runtime too

`--embed` still assumes the target machine has Spid3r: the launcher has to start
`spid3r gui`. `--bundle-runtime` appends the running `spid3r` executable to the
payload as `__spid3r_runtime/spid3r.exe`, and `run_payload()` starts that copy
instead of an installed one. The whole app is one file:

```bash
spid3r build app.matrix -o App.exe --release --icon app.ico --subsystem windows --allow-gui --embed app.matrix --embed ui --bundle-runtime
```

The launcher then shrinks to one call, which also anchors the working directory
and hides the handoff:

```matrix
fn main() {
    // `run_payload_strict()` is what `--bytecode` generates: a missing payload
    // is fatal instead of falling back to an installed toolchain. Use
    // `run_payload()` when the app is allowed to rely on a global install.
    return run_payload_strict();
}
```

The exe grows by the size of `spid3r.exe` (~11 MB). The first launch extracts the
runtime to `%LOCALAPPDATA%\Spid3r\runtime\<content-hash>`; later launches reuse
it. The extraction is written to a temporary file and renamed into place, so two
launches at once cannot read or execute a half-written copy, and a cached file is
re-verified against its content hash before it is run (a same-size impostor is
replaced, not executed). Runtimes from earlier builds are pruned to the current
one plus the two most recent, so rebuilding does not accumulate a full copy per
build in the user profile. Set `SPID3R_CACHE_DIR` to move the cache (used by the
tests, and by locked-down machines where `%LOCALAPPDATA%` is unavailable).

The target machine still needs the **WebView2 runtime** — present by default on
Windows 11, and on most Windows 10 machines through Edge. That is the only
external prerequisite; nothing from a Spid3r install and no Visual C++
Redistributable is required:

- **`LLVM-C.dll` is delay-loaded, not imported at startup** — in the toolchain
  (`build.rs`) *and* in every executable `spid3r build` produces
  (`src/native/link.rs`). Only `spid3r build` calls into LLVM; `gui` and `exec`
  never do. A built app used to import `LLVM-C.dll` when it was linked without
  `--release` (dead-stripping was what removed the import), so a debug build died
  with `0xC0000135 STATUS_DLL_NOT_FOUND` on any machine without LLVM.
- **The C runtime is static** (`.cargo/config.toml`, `+crt-static` on MSVC), so
  neither the toolchain nor a built app imports `VCRUNTIME140.dll` /
  `VCRUNTIME140_1.dll`. Both used to depend on the Visual C++ Redistributable,
  which is not part of a bare Windows install. `spid3r build` links the matching
  CRT flavour for the toolchain it was built from.

### Shipping bytecode instead of source

`--embed app.matrix` puts the **source text** in the payload. Anyone can read it
back out — the payload is obfuscated, not encrypted, and the key is in the
toolchain. If you would rather ship a compiled program, compile first and hand
the bytecode to `build`:

```bash
spid3r compile app.matrix -o app.matrixc
spid3r build app.matrix -o App.exe --release --icon app.ico --subsystem windows \
    --bytecode app.matrixc --embed ui --bundle-runtime
```

`--bytecode` does two things beyond embedding the file:

1. **The launcher is compiled from a three-line stub** (`fn main() { return
   run_payload_strict(); }`) instead of your program. Without this the whole
   application is AOT-compiled into the exe — every function body, every string
   literal, and a symbol name per function — even though a packaged run only
   ever executes `main` → `run_payload_strict`. The strict form is deliberate:
   `run_payload()` falls back to an installed `spid3r` when the executable
   carries no payload, so cutting the 24-byte trailer off a packaged app would
   redirect the launch to whatever `spid3r.exe` is on `PATH`. The strict form
   treats a missing payload as fatal ("payload is missing or truncated; rebuild
   it"), which is what a self-contained build should do.
2. **The payload carries `app.matrixc` and no `.matrix`.** `gui --payload`
   prefers a `.matrixc` entry and runs it on the VM.

`spid3r gui` grew a matching flag for development: `--engine vm` runs a
`.matrix` source through the VM, and a `.matrixc` argument runs on the VM
automatically. `--dry-run` works for both engines.

What this buys, honestly: `strings` on the exe shows no application logic, no
function names and no source structure; a reader would have to write a
decompiler for Spid3r bytecode, which does not exist. It is **not** secrecy —
the VM is open source, so a determined person can build one. It is the same
ceiling Java has: shipping bytecode beats shipping text, and nothing client-side
beats a determined reverse-engineer.

What a `.matrixc` payload does not change: the GUI still runs on the VM rather
than the tree interpreter, so anything relying on tree-only behaviour should be
checked with `--engine vm` before shipping.

## Limitations (v1)

- One window per process; the window is created only by `spid3r gui`.
- Windows needs the WebView2 runtime (ships with Windows 11 and Edge); macOS
  uses WKWebView, Linux WebKitGTK — provided by `wry`.
- GUI natives are not available to `spid3r run`, `spid3r exec`, or a plain
  `spid3r build` (the AOT backend does not link the GUI bridge). Running a GUI
  program with those commands fails with a clean diagnostic. `spid3r gui` is the
  only entry point that installs them, on either engine (`--engine tree` or
  `--engine vm`). `--allow-gui` overrides the check for `build`, but the natives
  then resolve to `null`, so that binary only works as the launcher described
  above.
- The window itself is not covered by automated tests — only the bridge
  (unit tests in `src/gui/bridge.rs`) and the end-to-end replay
  (`--dry-run`) are.
