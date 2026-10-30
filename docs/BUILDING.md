# Building Spid3r from source

Spid3r is a single Rust crate whose **core has no third-party
dependencies**. Two features add heavyweight dependencies, and both are on by
default:

| Feature | Adds | Requires |
| --- | --- | --- |
| *(core, `--no-default-features`)* | — | Rust toolchain only |
| `gui` *(default)* | embedded webview GUI (`spid3r gui`) via tao + wry | system webview runtime (see below) |
| `native` *(default)* | LLVM AOT backend (`spid3r build`) via inkwell 0.10 | LLVM 18.1+ |
| `tls` *(opt-in)* | `https://` in `http_fetch` via native-tls | OS certificate store (schannel / OpenSSL / Security.framework) |

Without LLVM installed, build with `--no-default-features --features gui`.

Everything below assumes the repository root as the working directory.

## Prerequisites

- **Rust** — **1.99.1**, pinned in `rust-toolchain.toml` (rustup installs it
  automatically on the first `cargo` invocation in this directory). The version
  is pinned because CI lints with `-D warnings`: a new stable can deprecate an
  API and turn the whole matrix red overnight (Rust 1.99 renamed
  `AtomicUsize::fetch_update` to `try_update`, which is exactly what happened).
  To upgrade: bump `rust-toolchain.toml` **and** the `toolchain:` inputs in
  `.github/workflows/ci.yml`, then run the gates. Edition 2021; `rust-version`
  in `Cargo.toml` records the MSRV.
- **A C toolchain / linker** — only needed for the `native` feature:
  - Windows: MSVC (`link.exe`) from Visual Studio Build Tools **and** the
    Windows 10/11 SDK.
  - macOS: Xcode command line tools (`clang`).
  - Linux: `cc` / `clang` plus `libc` dev files.
- **LLVM 18.1+** — only needed for the `native` feature.
- **A system webview runtime** — only needed for the `gui` feature:
  - Windows: the evergreen **WebView2 Runtime** (preinstalled on Windows 11 and
    on patched Windows 10; otherwise install from Microsoft).
  - macOS: the system **WKWebView** — always present.
  - Linux: **WebKit2GTK** (`libwebkit2gtk-4.1-dev` on Debian/Ubuntu) plus
    `libgtk-3-dev`, `libxdo-dev` (for `xdotool`-style synthetic input) and
    `libssl-dev`.

## Installing LLVM 18.1+

**Windows** — run the official `LLVM-18.1.x-win64.exe` installer from the
LLVM GitHub releases. The default location `C:\Program Files\LLVM` is probed
automatically; for a custom location set `SPID3R_LLVM_LIB_DIR` (see below).

**macOS** — `brew install llvm@18`.

**Linux (Debian/Ubuntu)** — `sudo apt install llvm-18 libllvm18 llvm-18-dev`
(distros shipping `llvm-18` expose `llvm-config-18`, which `build.rs` probes
alongside plain `llvm-config`).

If LLVM is missing, the build still **succeeds** with a
`cargo:warning=LLVM 18 not found ...` and `spid3r build` later fails with a
usage error pointing at this document — the rest of the toolchain
(`run`, `check`, `compile`, `exec`, `repl`, `test`) works fine.

## Build

```
# default: core toolchain + embedded webview GUI + LLVM native backend
# (on Windows this also writes Spid3r-Setup-<version>.exe; see "Windows installer")
cargo build --release

# without LLVM: core toolchain + GUI
cargo build --release --no-default-features --features gui

# zero-dependency core toolchain only (tree interpreter + bytecode VM)
cargo build --release --no-default-features

# run without installing
cargo run --release -- build examples/hello.matrix -o hello
./hello
```

The `spid3r` binary is at `target/release/spid3r` (`.exe` on Windows).

> **Build all targets, not just `--bin spid3r`.** `spid3r build` links apps
> against `target/release/spid3r_lang.lib`, the crate's `staticlib` artifact.
> `cargo build --release --bin spid3r` only produces the rlib the binary needs,
> so the `.lib` can lag behind the source and an app built right after picks up
> stale runtime code. `cargo build --release` (what the release instructions and
> CI use) refreshes both.

### Windows: the binaries carry no runtime prerequisites

`.cargo/config.toml` builds the MSVC target with `+crt-static`, and `build.rs`
delay-loads `LLVM-C.dll`. The result:

- `spid3r.exe` imports only system DLLs (`kernel32`, `user32`, `ws2_32`, …) —
  no `VCRUNTIME140.dll`, so no Visual C++ Redistributable is required, and no
  `LLVM-C.dll` at startup (it is resolved on the first AOT call, i.e. inside
  `spid3r build`).
- An executable produced by `spid3r build` has the same property. The link step
  picks the CRT flavour matching the toolchain (`libcmt`/`libvcruntime` when the
  toolchain is static, `msvcrt`/`vcruntime` otherwise) and passes
  `/DELAYLOAD:LLVM-C.dll` too, so a non-`--release` build no longer imports
  `LLVM-C.dll` and dies with `0xC0000135` on a machine without LLVM.

If you override `RUSTFLAGS` for a release build, keep `-C target-feature=+crt-static`
on Windows or the toolchain and its apps regain the redistributable dependency.
`tests/single_file_app.rs` asserts the import table of a built app, so a
regression here fails CI rather than a user's first launch.

## `compile` vs `build`

Two different outputs — do not confuse them:

- `spid3r compile <file.matrix> -o out` serializes **bytecode** (header
  `SPID3R…`, run with `spid3r exec out`). The output is always a `.matrixc`
  artifact: an `-o` path **without an extension** gains the canonical
  `.matrixc` extension (`-o out` writes `out.matrixc`), and the resolved
  path is what gets printed. An explicit extension is used verbatim.
- `spid3r build <file.matrix> -o out.exe` compiles to a **native
  machine-code executable** via the LLVM backend (requires LLVM — above).
- `spid3r exec <file.matrixc>` runs bytecode on the VM.

### Desktop executables: icon, no terminal

```bash
spid3r build app.matrix -o app.exe --icon app.ico --subsystem windows
```

- `--icon <path>` embeds an `.ico` (compiled with the Windows SDK `rc.exe`). A
  missing or empty icon file is a build error; if `rc.exe` rejects the resource,
  the build fails instead of quietly producing an icon-less executable.
- `--subsystem windows` marks the PE as a GUI binary, so Windows starts it
  **without** a terminal window. The default stays `console`. The entry point
  remains `main`: the linker is told `-entry:mainCRTStartup`, so the CRT startup
  still calls `main` and exits with its return value.
- `--subsystem` is Windows-only; on other targets the flag is accepted and has
  no effect.
- `-o app` (no extension) writes `app.exe` on Windows, matching
  `spid3r compile -o`.
- `--target` accepts the **host** triple only. The link step uses the host's
  linker and runtime library, so a foreign triple used to compile successfully
  and then fail in the linker with a message that named neither cause.

A GUI program still cannot be *compiled* by the AOT backend, because the
WebView bridge lives in the toolchain, not in the runtime staticlib. The
supported shape is a **launcher**: a program that hands off to `spid3r gui`.
Write one call and let the runtime do the rest:

```matrix
fn main() {
    return run_payload();
}
```

```bash
spid3r build launcher.matrix -o app.exe --icon app.ico --subsystem windows
```

`run_payload()` is a native, so this launcher needs no `--allow-gui` and no
GUI imports: it finds a runtime (payload first, then `%SPID3R_EXE%`, the normal
install, `PATH`), extracts the embedded copy when there is one, anchors the
working directory to the executable's folder, and starts
`spid3r gui --payload <self>` hidden. Use `--bytecode` to have `build` generate
exactly this stub for you.

If you write the handoff by hand instead, `--allow-gui` is required because the
launcher then mentions GUI natives. Without it, `build` refuses
(`error[build] … does not link the WebView bridge`) rather than producing a
binary whose GUI calls are silently null. With it, the build warns and the
natives resolve to `null` at runtime — which is exactly what
`type_of(gui_title) == "function"` is for.
- Hand-rolled handoffs must start the runtime with
  `spawn_process("spid3r", ["gui", "--payload", self_path], map("hidden", true))`
  so the child console never appears. Without `hidden`, a GUI-subsystem parent
  still causes Windows to allocate a console window for the console-subsystem
  child. `run_payload()` already does this, along with the `chdir` that relative
  data paths depend on.

To ship as **one file**, add `--embed`, `--bundle-runtime`, and — if you would
rather not ship source — `--bytecode`:

```bash
spid3r compile app.matrix -o app.matrixc

spid3r build app.matrix -o app.exe --icon app.ico --subsystem windows \
    --bytecode app.matrixc --embed ui --bundle-runtime
```

The program and assets are appended after the PE image (obfuscated, not
encrypted) and read back into memory at startup, so no folder travels with the
exe. `--bundle-runtime` adds the running `spid3r.exe` to the same payload, so the
target machine needs no Spid3r install — `run_payload()` extracts it to
`%LOCALAPPDATA%\Spid3r\runtime\<content-hash>\` on first launch (atomic write,
re-verified by content hash on every launch, old copies pruned). See
`docs/GUI.md` → "Shipping the runtime too".

`build` validates the payload **before** compiling and linking, so a mistake does
not leave a half-finished executable behind:

| Rejected | Why |
| --- | --- |
| `--embed` specs that expand to the same entry name | the payload is a flat map; which copy wins would be arbitrary |
| `--bundle-runtime` with no `.matrix`/`.matrixc` entry | the extracted runtime would report "no program" at launch, where a GUI-subsystem exe cannot show it |
| `--bytecode` that is not a valid `.matrixc` | the failure would otherwise happen at first launch on the user's machine |
| `--icon` that is missing/empty, or rejected by `rc.exe` | an icon-less binary was previously produced without a word |

Without `--bytecode`, the payload carries the `.matrix` **source**, which anyone
can read back out (the obfuscation is not encryption). `--bytecode` ships the
compiled program instead and compiles the launcher from a three-line stub, so no
application code, symbol names or string literals reach the native image. A
source payload still resolves `import "semver"` on a machine with no `stdlib/`
directory: the stdlib sources are compiled into the toolchain and used as the
last-resort resolution step.

Failures in a packaged app are invisible on a GUI-subsystem exe (no console), so
the launcher and the `spid3r gui` runtime it starts append them to
`%LOCALAPPDATA%\Spid3r\logs\spid3r.log` (1 MiB, rotated to `.log.1`;
`SPID3R_LOG=0` disables, `SPID3R_LOG_FILE` overrides the path).

### Native backend limits

`spid3r build` flattens `import` (including `import "x" as alias;`) before codegen, so a
program the checker accepts compiles unless it hits a construct the backend cannot
lower — and then it fails with a clean `error[usage]`/`error[build]` naming the
construct (exit code 2) rather than mis-compiling.

Two limits were previously listed here and are no longer accurate: **aliased
imports** now work (covered by `tests/f2_cli_outputs.rs`), and a **class declared
inside a function** is rejected by the parser for every engine, so it never reaches
the backend. If you find a construct the tree/VM run but `build` refuses, that is a
backend gap worth reporting — the front end is shared, so it should compile.

The default `gui` feature pulls in `tao` (window) and `wry` (webview), so
first-time compilation takes noticeably longer than the core toolchain; use
`--no-default-features` to skip those crates.

### Windows installer

On Windows, `cargo build --release` also writes `Spid3r-Setup-<version>.exe`
(version from `Cargo.toml`) to the repository root. A build script runs before
its own crate is compiled, so `build.rs` first builds release binaries
(`spid3r.exe` and, when `gui` is enabled, `calculator.exe`) into `target/installer/`,
then runs NSIS on `packaging/nsis/spid3r.nsi`.

> **The installer is a build-script side effect, not a Cargo output.** `build.rs`
> runs once and Cargo then caches it, so a later `cargo build --release` finishes
> instantly (`Finished in 0.2s`) and does **not** recreate an installer that was
> skipped or since deleted. Cargo may even replay the old `installer written:`
> warning from its cache, which makes it look like the file was produced. To force
> the step to run again:
>
> ```bash
> # bash / Git Bash
> SPID3R_PACKAGE=1 cargo build --release
> # PowerShell
> $env:SPID3R_PACKAGE=1; cargo build --release
> # or invalidate the script directly
> touch build.rs && cargo build --release
> ```
>
> Setting `SPID3R_PACKAGE=1` works because `build.rs` declares
> `cargo:rerun-if-env-changed` for it, so the changed value invalidates the cache.
> Watch the `cargo:warning=` lines — every skip path reports there, including
> missing NSIS, missing `LLVM-C.dll`/`LLVM-C.lib` under `native`, and a failed
> nested build.

- NSIS 3.x is found in `packaging/nsis/nsis/nsis-3.10/` (the portable zip) or
  on `PATH`; when it is missing, the step is skipped with a `cargo:warning`.
- The installer packs `spid3r.exe` plus (with `gui`) the standalone native
  `calculator.exe` desktop app, `stdlib/`, `README.md`, `docs/` (developer guide,
  WASM playground, benchmarks dashboard), `examples/` (including `demo_apps/`), and
  `benchmarks/` (the 9 canonical benchmark scripts and dataset).
- Creates a dedicated **Spid3r** Start Menu group with shortcuts for the Command
  Shell, Developer Guide, WebAssembly Playground, Benchmarks Dashboard, Desktop Calculator,
  and Uninstaller, plus an optional Desktop Shortcuts component.
- Registers `.matrix` file associations with Windows Explorer context menu actions
  ("Run with Spid3r", "Check Syntax", and "Run with GUI").
- With `native`, it also ships `LLVM-C.dll` (loaded by `spid3r.exe`),
  `LLVM-C.lib`, and the `spid3r_lang.lib` runtime staticlib, which `spid3r build`
  links into every program. Users still need MSVC Build Tools with the
  Windows SDK to link; the programs they build have no LLVM dependency.
- Set `SPID3R_NO_INSTALLER=1` to skip the installer step.

#### WebView2 bootstrapper (opt-in, verified)

The installer can bundle Microsoft's evergreen WebView2 bootstrapper so a machine
without the runtime can install it. That download is **opt-in**: a plain
`cargo build --release` performs no network access.

```bash
# 1. Fetch and bundle it (needs the hash of the file you verified).
SPID3R_FETCH_WEBVIEW2=1 cargo build --release

# 2. If Microsoft rotated the bootstrapper, pin the new hash explicitly.
SPID3R_FETCH_WEBVIEW2=1 SPID3R_WEBVIEW2_SHA256=<64-hex> cargo build --release
```

The file is downloaded to `packaging/nsis/redist/*.part`, verified against the
pinned SHA-256 (`build_support/webview2.rs`) **and** a minimum size, and only then
renamed into place. A partial or tampered download is deleted and the installer is
built without the bootstrapper (`!ifdef WEBVIEW2_SETUP` in `spid3r.nsi`) — it is
never bundled unverified.

GUI programs can be run headlessly with `spid3r gui --dry-run <file>`, which
replays the whole native↔JS bridge (script flushes and event dispatch) without
opening a window — useful for CI and for verifying handler wiring. See
[GUI.md](GUI.md).

### Environment variables

| Variable | Meaning |
| --- | --- |
| `SPID3R_NO_INSTALLER` | Any value skips the Windows installer step of `cargo build --release`. |
| `SPID3R_LLVM_LIB_DIR` | Directory containing `LLVM-C.lib` (Windows) or `libLLVM-18.so` / `libLLVM.dylib` (Unix). Checked first by both `build.rs` and the linker stage. |
| `SPID3R_LLVM_BIN_DIR` | Windows: directory containing `LLVM-C.dll` (default: `C:\Program Files\LLVM\bin`). `build.rs` copies it next to the built binaries (and into `deps/` for test executables) so `cargo run` / `cargo test` work without modifying PATH. |
| `LLVM_SYS_181_PREFIX` | The llvm-sys convention prefix; `<prefix>/lib` is searched as a fallback when the variables above are unset. |

Set them when LLVM lives somewhere non-standard, e.g.:

```
SPID3R_LLVM_LIB_DIR=/opt/llvm-18/lib SPID3R_LLVM_BIN_DIR=/opt/llvm-18/bin \
  cargo build --release --features native
```

## How the LLVM link works

`inkwell`/`llvm-sys` are built with `no-llvm-linking` because llvm-sys has no
`llvm-config` story on the stock Windows installer and no dynamic-linking
support there. Instead, `build.rs` links the LLVM C API itself:

- **Windows**: locates `LLVM-C.lib` (import library for `LLVM-C.dll`) via
  `SPID3R_LLVM_LIB_DIR` → `LLVM_SYS_181_PREFIX\lib` →
  `C:\Program Files\LLVM\lib`.
- **Linux/macOS**: runs `llvm-config --libdir` (Debian emits `-lLLVM-18`,
  Homebrew `-lLLVM`), with Homebrew prefix fallbacks.

When *linking a compiled program*, `link.rs` additionally needs a system
linker — MSVC `link.exe` plus the CRT/SDK import libraries on Windows,
`clang`/`cc` elsewhere — and the runtime staticlib produced by the crate's
`[lib] crate-type = ["rlib", "staticlib", "cdylib"]` (`libspid3r_lang.a` /
`spid3r_lang.lib`, found next to the `spid3r` binary or under
`target/{release,debug}/`). See
[NATIVE.md](NATIVE.md) for what the linker passes and why (notably
`-opt:ref`, which dead-strips the inkwell bindings so the shipped executable
has **no** `LLVM-C.dll` dependency — verified by running built binaries from
an isolated directory).

## Building the VS Code extension

The language server is part of the compiler, so building the compiler is all the
server needs:

```
cargo build --no-default-features
```

The editor client lives in `packaging/vscode/` and has **no dependencies and no
build step** — `package.json` plus one `extension.js`. Package it into a `.vsix`
with the official packager:

```
cd packaging/vscode
npx --yes @vscode/vsce package      # writes spid3r-0.1.0.vsix
code --install-extension spid3r-0.1.0.vsix
```

`npx` fetches `@vscode/vsce` on demand, so nothing has to be installed
permanently and no `npm install` is required in that directory. To develop
against it, open `packaging/vscode` in VS Code and press `F5` to launch an
Extension Development Host.

If `spid3r` is not on your `PATH`, set `spid3r.serverPath` to its absolute path
in VS Code settings. Set `spid3r.trace.server` to `messages` or `verbose` to
log the JSON-RPC traffic to the *Spid3r Language Server* output channel — that
channel is also where any server startup failure is reported. See
[LSP.md](LSP.md) for the protocol surface.

## Tests and verification

```
cargo test                       # full suite: core, GUI bridge + dry-run, native runtime + differential
cargo test --no-default-features # core engines only
cargo run -- test                # tree == vm == exec over every examples/*.matrix
bash tools/native_diff.sh examples/*.matrix   # tree vs native differential
```

Feature configurations are tested **sequentially**, never concurrently: they all
write to the same `target/debug/spid3r`, and a concurrent build with a different
feature set silently overwrites the binary another test run is using.

`tools/native_diff.sh` wraps every step in a timeout and prints `HANG <name>`
if one ever stalls, so a wedged build or binary can never silently eat the
whole run; tune with `VM_TIMEOUT` / `BUILD_TIMEOUT` / `RUN_TIMEOUT`.

Code quality gate (project convention):
`cargo clippy --all-targets -- -D warnings` — keep `src/` at **0** warnings
(warnings as well as errors; plain clippy lets lints like `map_identity` slip
through as warnings). `bash tools/gate.sh` runs this for every feature
configuration plus a release build.

## Troubleshooting

| Symptom | Cause / fix |
| --- | --- |
| `cargo:warning=LLVM 18 not found for the native backend` | Install LLVM 18.1+ or set `SPID3R_LLVM_LIB_DIR`. Harmless if you don't need `spid3r build`. |
| `no MSVC linker (link.exe) found` (on `spid3r build`) | Install Visual Studio Build Tools, or run from a Developer prompt (`VCINSTALLDIR` is honored). |
| `no Windows SDK found` | Install the Windows 10/11 SDK component. |
| `cannot find the Spid3r runtime library` | Build the toolchain first: `cargo build --features native` produces the staticlib that program links use. |
| `linking failed` with `LNK2019` on `LLVM*` symbols | The final link needs `LLVM-C.lib` on the libpath; a non-default LLVM location needs `SPID3R_LLVM_LIB_DIR` set for the linker stage too. |
| `error deleting lock file ... (os error 5)` during builds | A OneDrive-over-Windows incremental-build quirk, not a code issue; vanishes after `cargo clean`. |
| `spid3r build` says the backend is not enabled | The binary was built with `--no-default-features` (and without `native`); rebuild with the defaults. |
| `spid3r gui` says the GUI is not enabled | The binary was built with `--no-default-features`; rebuild without it (or add `--features gui`). |
| `spid3r gui` builds but no window appears / a create-error is printed | The system webview runtime is missing or too old: WebView2 on Windows, WebKit2GTK on Linux (`libwebkit2gtk-4.1-dev`). `--dry-run` still works without it. |

## Updating the developer guide (`docs/html/doc.html`)

`doc.html` is hand-written single-page HTML. There is no generator and no build
step — you edit the file, then verify it:

```bash
node tools/verify_dev_snippets.js
```

That script evaluates every `<div class="w3-code-body">` snippet on the wasm
runtime (`check`, then `eval` on both the tree and VM engines), checks every
internal `#anchor` against the ids present in the page, and checks the
playground's base64 URL round-trip. It exits non-zero on any drift, and CI runs
it — a stale sample fails the build instead of shipping.

Two rules when editing:

- **Snippets must be valid on both engines.** GUI and other host-dependent
  natives cannot run under wasm, so keep them out of `w3-code-body`; a plain
  styled `<div>` is fine for shell commands and illustrative fragments.
- **Keep statements on their own lines.** Hand-wrapped code can swallow a `//`
  marker and split an expression across lines. Three samples in the guide were
  broken exactly that way (a wrapped comment turned `let account = null;` into
  prose, and a wrapped `let result = ...` lost its `let`) until the verifier
  started reporting which snippet failed.

## Rebuilding the playground runtime (`web/spid3r.wasm`)

`docs/html/playground.html` does not compile anything itself — it loads the
**committed** `web/spid3r.wasm` (and the base64 copy in
`web/spid3r_wasm_data.js` for `file://` use). Those are build artifacts checked
into the repository, so **any change to the language, the `stdlib/*.matrix`
modules, or the wasm entry point must be followed by a rebuild**:

```bash
cargo build --target wasm32-unknown-unknown --release --lib --no-default-features
cp target/wasm32-unknown-unknown/release/spid3r_lang.wasm web/spid3r.wasm
node tools/gen_wasm_data.js            # regenerates web/spid3r_wasm_data.js

node tools/test_wasm_embedded.js       # committed set is self-consistent
node tools/test_playground_presets.js  # all presets, both engines
```

CI guards this in two ways — it replays every playground preset against the
committed module *and* against a fresh build, and it asserts that regenerating
the base64 file changes nothing. What it cannot see is a deployment set that is
merely **one commit old but still behaviourally correct**: it runs fine, so both
checks pass, and the playground silently serves an older compiler. That is why
the rebuild is a manual step in this list rather than something CI can catch —
when in doubt, rebuild and re-run the two scripts above.
