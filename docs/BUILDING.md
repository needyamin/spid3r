# Building Spid3r from source

Spid3r is a single Rust crate with **no third-party dependencies in its
default configuration**. Two optional features add heavyweight dependencies
only when you ask for them:

| Feature | Adds | Requires |
| --- | --- | --- |
| *(default)* | — | Rust toolchain only |
| `native` | LLVM AOT backend (`spid3r build`) via inkwell 0.10 | LLVM 18.1+ |
| `gui` | embedded webview GUI (`spid3r gui`) via tao + wry | system webview runtime (see below) |

Everything below assumes the repository root as the working directory.

## Prerequisites

- **Rust** — any recent stable (edition 2021).
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
# zero-dependency core toolchain (tree interpreter + bytecode VM)
cargo build --release

# with the LLVM native backend
cargo build --release --features native

# with the embedded webview GUI
cargo build --release --features gui

# both optional features
cargo build --release --features "native gui"

# run without installing
cargo run --release -- build examples/hello.matrix -o hello
./hello
```

The `spid3r` binary is at `target/release/spid3r` (`.exe` on Windows).

The `gui` feature pulls in `tao` (window) and `wry` (webview), so first-time
compilation of that configuration takes noticeably longer than the core
toolchain — those crates are only compiled when you pass the feature.

GUI programs can be run headlessly with `spid3r gui --dry-run <file>`, which
replays the whole native↔JS bridge (script flushes and event dispatch) without
opening a window — useful for CI and for verifying handler wiring. See
[GUI.md](GUI.md).

### Environment variables

| Variable | Meaning |
| --- | --- |
| `SPID3R_LLVM_LIB_DIR` | Directory containing `LLVM-C.lib` (Windows) or `libLLVM-18.so` / `libLLVM.dylib` (Unix). Checked first by both `build.rs` and the linker stage. |
| `SPID3R_LLVM_BIN_DIR` | Windows: directory containing `LLVM-C.dll`. `build.rs` copies it next to the built binaries so `cargo run` / `cargo test` work without modifying PATH. |
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
`[lib] crate-type = ["rlib", "staticlib"]` (`libspid3r.a` / `spid3r.lib`,
found next to the `spid3r` binary or under `target/{release,debug}/`). See
[NATIVE.md](NATIVE.md) for what the linker passes and why (notably
`-opt:ref`, which dead-strips the inkwell bindings so the shipped executable
has **no** `LLVM-C.dll` dependency — verified by running built binaries from
an isolated directory).

## Tests and verification

```
cargo test                       # full suite on the core engines
cargo test --features native     # + runtime unit tests + native differential tests
cargo test --features gui        # + bridge unit tests + GUI dry-run replay tests
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
| `spid3r build` says the backend is not enabled | The binary was built without `--features native`; rebuild the toolchain with it. |
| `spid3r gui` says the GUI is not enabled | The binary was built without `--features gui`; rebuild the toolchain with it. |
| `spid3r gui` builds but no window appears / a create-error is printed | The system webview runtime is missing or too old: WebView2 on Windows, WebKit2GTK on Linux (`libwebkit2gtk-4.1-dev`). `--dry-run` still works without it. |
